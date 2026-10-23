use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::evidence::CampaignFile;
use crate::forge::{GithubRepo, TagName};
use crate::host::{Host, stamp};
use crate::journal;
use crate::limits::{self, Limits};
use crate::lint::{self, packet_files, packet_paths};
use crate::outcome::{Outcome, Violation};
use crate::plan::{self, Units};
use crate::render;
use crate::state::{
    After, CampaignState, DEFAULT_TAG, Evidence, FLAGS, Finish, Flag, JournalMark, Kind, Lane,
    Move, Phase, RELEASES, Release, SCHEMA, Step, Track, UnitState, Work, evidence_for,
};
use crate::store::{
    self, JOURNAL, Target, UNIT_STATE, absolute, create_new, find_root, lock, parse_target,
    relative, state_path, write_atomic, write_json,
};
use crate::templates::{self, ROOT_FILES};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn record(root: &Path, at: &str, title: &str, body: &str) -> Result<String> {
    let entry = journal::entry(at, title, Some(body));
    journal::append(root, &entry)?;
    Ok(journal::heading(at, title))
}

pub fn closed(root: &Path, campaign: &CampaignState) -> Option<Violation> {
    campaign.closed.as_ref().map(|at| {
        Violation::new(
            relative(root, &state_path(root)),
            "campaign-closed",
            format!("the campaign closed at {at}; nothing is initialized, launched or moved here"),
        )
    })
}

pub fn write_unit(dir: &Path, unit: &UnitState) -> Result<()> {
    let stamped = UnitState {
        schema: SCHEMA,
        ..unit.clone()
    };
    write_json(&state_path(dir), &stamped)?;
    write_atomic(&dir.join(UNIT_STATE), render::unit(unit).as_bytes())
}

#[derive(Debug, Clone, Default)]
pub struct Ships {
    pub github: Option<GithubRepo>,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Declaration {
    pub lane: Option<Lane>,
    pub after: Vec<String>,
    pub ships: Ships,
    pub work: Work,
}

impl Declaration {
    fn given(&self) -> bool {
        self.lane.is_some() || !self.after.is_empty() || self.ships.given() || !self.work.is_code()
    }
}

impl Ships {
    fn given(&self) -> bool {
        self.github.is_some() || self.tag.is_some()
    }
}

pub fn init(host: &dyn Host, dir: &Path) -> Result<Outcome> {
    declare(host, dir, &Declaration::default())
}

pub fn declare(host: &dyn Host, dir: &Path, declaration: &Declaration) -> Result<Outcome> {
    let dir = absolute(dir);
    let flags = declaration.given();
    if let Some(root) = dir.parent().and_then(find_root) {
        let rel = relative(&root, &dir);
        return match (parse_target(&rel, false), declaration.lane) {
            (Ok(Target { unit, step: None }), _) if !flags => init_unit(host, &root, &dir, &unit),
            (
                Ok(Target {
                    unit,
                    step: Some(step),
                }),
                Some(lane),
            ) => init_step(host, &root, &unit, &step, lane, declaration),
            (Ok(Target { step: Some(_), .. }), None) => Ok(Outcome::refused(vec![Violation::new(
                rel,
                "init-target",
                "a step is declared with `cm init <NN-name>/<unit>/<step> --lane <lane> [--after …]`",
            )])),
            _ => Ok(Outcome::refused(vec![Violation::new(
                rel,
                "init-target",
                format!(
                    "{} is inside the campaign at {}; `cm init` there takes a unit folder, <NN-name>/<unit>, or a step with --lane",
                    dir.display(),
                    root.display()
                ),
            )])),
        };
    }
    if flags {
        return Ok(Outcome::refused(vec![Violation::new(
            dir.to_string_lossy(),
            "init-target",
            "--lane, --after, --study, --github and --tag declare a step inside a campaign; no campaign holds this folder",
        )]));
    }
    init_campaign(host, &dir)
}

fn init_campaign(host: &dyn Host, dir: &Path) -> Result<Outcome> {
    fs::create_dir_all(dir)?;
    let _lock = lock(dir)?;
    let at = stamp(host.now());
    let mut state = if state_path(dir).exists() {
        store::load_campaign(dir)?
    } else {
        CampaignState {
            schema: SCHEMA,
            kind: Kind::Campaign,
            initialized: at.clone(),
            written: Vec::new(),
            adopted: Vec::new(),
            journal: None,
            closed: None,
        }
    };
    let mut written = Vec::new();
    let mut adopted = Vec::new();
    let mut violations = Vec::new();
    for (name, template) in ROOT_FILES {
        let path = dir.join(name);
        let known = state
            .written
            .iter()
            .chain(&state.adopted)
            .any(|file| file == name);
        if name == JOURNAL && state.journal.is_some() {
            if !path.exists() {
                violations.push(Violation::new(
                    JOURNAL,
                    "journal-missing",
                    "JOURNAL.md is gone; restore it with `git checkout HEAD -- JOURNAL.md`",
                ));
            }
            continue;
        }
        if create_new(&path, template.as_bytes())? {
            written.push(name.to_owned());
            if name == JOURNAL {
                state.journal = Some(JournalMark {
                    baseline_lines: template.lines().count(),
                });
            }
            continue;
        }
        if known {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let found = templates::conform(
            name,
            &text,
            templates::Banner::of(dir, state.closed.is_some()),
        );
        if !found.is_empty() {
            violations.extend(found);
            continue;
        }
        adopted.push(name.to_owned());
        if name == JOURNAL {
            state.journal = Some(JournalMark {
                baseline_lines: text.lines().count(),
            });
        }
    }
    state.written.extend(written.iter().cloned());
    state.adopted.extend(adopted.iter().cloned());
    write_json(&state_path(dir), &state)?;
    let mut outcome = Outcome::default();
    if (!written.is_empty() || !adopted.is_empty()) && state.journal.is_some() {
        let guard = journal::check(dir, &state);
        if guard.is_empty() {
            let mut body = String::new();
            if !written.is_empty() {
                body.push_str(&format!("- written: {}\n", written.join(", ")));
            }
            if !adopted.is_empty() {
                body.push_str(&format!(
                    "- adopted as the baseline: {}\n",
                    adopted.join(", ")
                ));
            }
            let heading = record(dir, &at, "cm init", &body)?;
            outcome.field("journal", heading);
        } else {
            violations.extend(guard);
        }
    }
    outcome.violations = violations;
    outcome.field("root", dir.to_string_lossy().into_owned());
    outcome.field("written", written.clone());
    outcome.field("adopted", adopted.clone());
    for name in &written {
        outcome.line(format!("wrote {name}"));
    }
    for name in &adopted {
        outcome.line(format!("adopted {name}"));
    }
    if written.is_empty() && adopted.is_empty() && outcome.ok() {
        outcome.line(format!("nothing to do: {} is initialized", dir.display()));
    }
    Ok(outcome)
}

fn init_unit(host: &dyn Host, root: &Path, dir: &Path, unit_name: &str) -> Result<Outcome> {
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut guard = journal::check(root, &campaign);
    guard.extend(closed(root, &campaign));
    if !guard.is_empty() {
        return Ok(Outcome::refused(guard));
    }
    let at = stamp(host.now());
    fs::create_dir_all(dir.join(store::CM_DIR))?;
    let mut written = Vec::new();
    let unit = if state_path(dir).exists() {
        store::load_unit(dir)?
    } else {
        let unit = UnitState::new(unit_name, &at);
        write_json(&state_path(dir), &unit)?;
        written.push(relative(root, &state_path(dir)));
        unit
    };
    let mut outcome = Outcome::default();
    if create_new(&dir.join(UNIT_STATE), render::unit(&unit).as_bytes())? {
        written.push(relative(root, &dir.join(UNIT_STATE)));
    } else {
        outcome.violations = lint::unit_state_file(root, dir, &unit);
    }
    if !written.is_empty() {
        let heading = record(
            root,
            &at,
            &format!("init unit {unit_name}"),
            &format!("- written: {}\n", written.join(", ")),
        )?;
        outcome.field("journal", heading);
    }
    outcome.field("unit", unit_name);
    outcome.field("written", written.clone());
    for name in &written {
        outcome.line(format!("wrote {name}"));
    }
    if written.is_empty() && outcome.ok() {
        outcome.line(format!("nothing to do: unit {unit_name} is initialized"));
    }
    Ok(outcome)
}

fn release_checks(full: &str, unit: &UnitState, step_name: &str) -> Vec<Violation> {
    let Some(release) = Release::of_target(full) else {
        return vec![Violation::new(
            full,
            "release-version",
            format!(
                "a release step is {RELEASES}/<repo>/<major>.<minor>.<patch>; `{step_name}` is not a version"
            ),
        )];
    };
    let above: Vec<String> = unit
        .steps
        .keys()
        .filter(|name| *name != step_name)
        .filter_map(|name| name.parse().ok())
        .filter(|version| *version >= release.version)
        .map(|version: crate::state::Version| version.to_string())
        .collect();
    if above.is_empty() {
        Vec::new()
    } else {
        vec![Violation::new(
            full,
            "release-version",
            format!(
                "{} is not above the release(s) {} already declared for {}",
                release.version,
                above.join(", "),
                release.line
            ),
        )]
    }
}

fn record_release(
    host: &dyn Host,
    root: &Path,
    unit: &mut UnitState,
    step_name: &str,
    ships: &Ships,
    tag: &TagName,
) -> Result<Outcome> {
    let at = stamp(host.now());
    let full = format!("{}/{step_name}", unit.unit);
    if let Some(step) = unit.steps.get_mut(step_name) {
        step.tag = Some(tag.clone());
        step.github.clone_from(&ships.github);
    }
    write_unit(&root.join(&unit.unit), unit)?;
    let mut body = format!("- tag: {tag}\n");
    if let Some(github) = &ships.github {
        body.push_str(&format!("- github: {github}\n"));
    }
    let heading = record(root, &at, &format!("record release {full}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!("recorded {full}'s tag {tag}"));
    outcome.field("target", full);
    outcome.field("tag", tag.to_string());
    outcome.field("journal", heading);
    Ok(outcome)
}

/// The same declaration again finishes one an interrupted `cm init` left half written: the step
/// folder and a missing or stale STATE.md. The journal is not replayed.
fn repair(
    host: &dyn Host,
    root: &Path,
    dir: &Path,
    unit: Option<&UnitState>,
    step_name: &str,
    full: &str,
) -> Result<Outcome> {
    let mut outcome = Outcome::default();
    outcome.field("target", full);
    let Some(unit) = unit else {
        return Ok(outcome);
    };
    let mut repaired = Vec::new();
    if !dir.join(step_name).is_dir() {
        fs::create_dir_all(dir.join(step_name))?;
        repaired.push(format!("{full}/"));
    }
    if !lint::unit_state_file(root, dir, unit).is_empty() {
        write_atomic(&dir.join(UNIT_STATE), render::unit(unit).as_bytes())?;
        repaired.push(relative(root, &dir.join(UNIT_STATE)));
    }
    if repaired.is_empty() {
        outcome.line(format!("nothing to do: {full} is declared"));
    } else {
        let at = stamp(host.now());
        let body = format!("- re-rendered: {}\n", repaired.join(", "));
        outcome.field(
            "journal",
            record(root, &at, &format!("repair {full}"), &body)?,
        );
        outcome.line(format!("repaired {full}: {}", repaired.join(", ")));
    }
    outcome.field("repaired", repaired);
    Ok(outcome)
}

fn init_step(
    host: &dyn Host,
    root: &Path,
    unit_name: &str,
    step_name: &str,
    lane: Lane,
    declaration: &Declaration,
) -> Result<Outcome> {
    let ships = &declaration.ships;
    let work = declaration.work;
    let full = format!("{unit_name}/{step_name}");
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    let units: Units = plan::load_units(root)?;
    let mut parsed = Vec::new();
    for text in &declaration.after {
        match After::parse(text, unit_name) {
            Err(message) => violations.push(Violation::new(&full, "after-parse", message)),
            Ok(after) if after.target() == full => violations.push(Violation::new(
                &full,
                "after-unknown",
                format!("`{text}` names the step itself"),
            )),
            Ok(after) if plan::step(&units, &after.target()).is_none() => {
                violations.push(Violation::new(
                    &full,
                    "after-unknown",
                    format!(
                        "`{text}` names {}, which is not declared; declare it first",
                        after.target()
                    ),
                ))
            }
            Ok(after) => parsed.push(after),
        }
    }
    let dir = root.join(unit_name);
    let existing = units.get(unit_name).cloned();
    let mut tag = None;
    if unit_name.starts_with(&format!("{RELEASES}/")) {
        let empty = UnitState::new(unit_name, "");
        violations.extend(release_checks(
            &full,
            existing.as_ref().unwrap_or(&empty),
            step_name,
        ));
        if let Some(release) = Release::of_target(&full) {
            match release.tag(ships.tag.as_deref().unwrap_or(DEFAULT_TAG)) {
                Ok(name) => tag = Some(name),
                Err(message) => violations.push(Violation::new(&full, "release-tag", message)),
            }
        }
        if !work.is_code() {
            violations.push(Violation::new(
                &full,
                "init-target",
                "a release step ships a tag; --study marks a step that ships none",
            ));
        }
    } else if ships.given() {
        violations.push(Violation::new(
            &full,
            "init-target",
            format!("--github and --tag declare a release step, {RELEASES}/<line>/<version>"),
        ));
    }
    if let Some(step) = existing.as_ref().and_then(|unit| unit.steps.get(step_name)) {
        let same_tag = match (Release::of_target(&full), &tag) {
            (Some(release), Some(tag)) => step.release_tag(&release) == *tag,
            _ => step.tag.is_none(),
        };
        if step.lane == Some(lane)
            && step.after == parsed
            && step.github == ships.github
            && step.work == work
            && same_tag
            && violations.is_empty()
        {
            return repair(host, root, &dir, existing.as_ref(), step_name, &full);
        }
        if let (Some(mut unit), Some(tag)) = (existing.clone(), &tag)
            && step.tag.is_none()
            && step.github.is_none()
            && step.lane == Some(lane)
            && step.after == parsed
            && step.work == work
            && violations.is_empty()
        {
            return record_release(host, root, &mut unit, step_name, ships, tag);
        }
        violations.push(Violation::new(
            &full,
            "step-declared",
            format!(
                "{full} is already declared ({}, lane {}); init never overwrites",
                step.track.state,
                step.lane.map_or("—", Lane::name)
            ),
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    fs::create_dir_all(dir.join(store::CM_DIR))?;
    fs::create_dir_all(dir.join(step_name))?;
    let mut unit = existing.unwrap_or_else(|| UnitState::new(unit_name, &at));
    unit.steps.insert(
        step_name.to_owned(),
        Step {
            track: Track::new(&at),
            worktree: String::new(),
            launches: 0,
            lane: Some(lane),
            after: parsed.clone(),
            github: ships.github.clone(),
            tag: tag.clone(),
            work,
        },
    );
    write_unit(&dir, &unit)?;
    let shown: Vec<String> = parsed.iter().map(After::to_string).collect();
    let mut body = format!(
        "- lane: {lane}\n- after: {}\n",
        if shown.is_empty() {
            "—".to_owned()
        } else {
            shown.join(", ")
        }
    );
    if let Some(tag) = &tag {
        body.push_str(&format!("- tag: {tag}\n"));
    }
    if let Some(github) = &ships.github {
        body.push_str(&format!("- github: {github}\n"));
    }
    if !work.is_code() {
        body.push_str("- study: finishes with `cm advance <step> done --evidence <path>`\n");
    }
    let heading = record(root, &at, &format!("init step {full}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(match work {
        Work::Code => format!("declared {full} ({lane})"),
        Work::Study => format!("declared {full} ({lane}, study)"),
    });
    outcome.field("target", full);
    outcome.field("lane", lane.name());
    outcome.field("study", !work.is_code());
    outcome.field("after", shown);
    outcome.field("tag", tag.as_ref().map(ToString::to_string));
    outcome.field("journal", heading);
    Ok(outcome)
}

pub fn close(host: &dyn Host, root: &Path) -> Result<Outcome> {
    let _lock = lock(root)?;
    let mut campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    for dir in lint::unit_dirs(root) {
        store::load_unit(&dir)?;
    }
    violations.extend(lint::unsealed(root));
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    campaign.closed = Some(at.clone());
    write_json(&state_path(root), &campaign)?;
    let heading = record(
        root,
        &at,
        "cm close",
        "- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n",
    )?;
    let mut outcome = Outcome::default();
    outcome.field("closed", at);
    outcome.field("journal", heading.clone());
    outcome.line(heading);
    outcome.line(
        "next: RESUME.md's banner drops its `/loop` and names the successor; `cm lint` holds it there",
    );
    Ok(outcome)
}

pub fn journal(host: &dyn Host, root: &Path, title: &str, body: Option<&str>) -> Result<Outcome> {
    if let Err(message) = journal::validate(title, body) {
        return Ok(Outcome::refused(vec![Violation::new(
            JOURNAL,
            "journal-entry",
            message,
        )]));
    }
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let guard = journal::check(root, &campaign);
    if !guard.is_empty() {
        return Ok(Outcome::refused(guard));
    }
    let at = stamp(host.now());
    journal::append(root, &journal::entry(&at, title, body))?;
    let heading = journal::heading(&at, title.trim());
    let mut outcome = Outcome::default();
    outcome.line(heading.clone());
    outcome.field("heading", heading);
    Ok(outcome)
}

fn shell_word(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:@%+=-".contains(c))
    {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

fn launch_checks(host: &dyn Host, limits: &Limits, report: &mut Vec<String>) -> Vec<Violation> {
    let mut out = Vec::new();
    if limits.paused {
        out.push(Violation::new(
            "LIMITS.toml",
            "limits-paused",
            "`paused = true`: nothing launches",
        ));
    }
    match host.free_bytes(&limits.disk_volume) {
        Err(error) => out.push(Violation::new(
            "LIMITS.toml",
            "limits-disk",
            format!(
                "cannot read free space on {}: {error}",
                limits.disk_volume.display()
            ),
        )),
        Ok(bytes) => {
            let free = bytes as f64 / GIB;
            let usable = free - limits.kache_headroom_gib;
            report.push(format!(
                "free disk: {free:.1} GiB on {}, {usable:.1} GiB after {} GiB kache headroom (floor {} GiB)",
                limits.disk_volume.display(),
                limits.kache_headroom_gib,
                limits.min_free_disk_gib
            ));
            if usable <= limits.min_free_disk_gib {
                out.push(Violation::new(
                    "LIMITS.toml",
                    "limits-disk",
                    format!(
                        "{free:.1} GiB free minus {} GiB kache headroom is not over the {} GiB floor",
                        limits.kache_headroom_gib, limits.min_free_disk_gib
                    ),
                ));
            }
        }
    }
    match host.heavy_builds() {
        Err(error) => out.push(Violation::new(
            "LIMITS.toml",
            "limits-heavy-builds",
            format!("cannot count live builds: {error}"),
        )),
        Ok(count) => {
            report.push(format!(
                "heavy builds: {count} live, cap {}",
                limits.max_heavy_builds
            ));
            if count >= limits.max_heavy_builds {
                out.push(Violation::new(
                    "LIMITS.toml",
                    "limits-heavy-builds",
                    format!(
                        "{count} live cargo build(s) reach the cap of {}",
                        limits.max_heavy_builds
                    ),
                ));
            }
        }
    }
    out
}

pub fn launch(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    worktree: Option<&Path>,
) -> Result<Outcome> {
    let Some(step_name) = target.step.as_deref() else {
        return Ok(Outcome::refused(vec![Violation::new(
            &target.unit,
            "launch-target",
            "`cm launch` takes <NN-name>/<unit>/<step>",
        )]));
    };
    let full = format!("{}/{step_name}", target.unit);
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    let units = plan::load_units(root)?;
    let declared = plan::step(&units, &full).and_then(|step| step.lane);
    let local = declared.is_none_or(Lane::is_local);
    let mut report = Vec::new();
    let limits = match fs::read_to_string(root.join("LIMITS.toml")) {
        Err(_) => {
            violations.push(Violation::new(
                "LIMITS.toml",
                "missing-file",
                "run `cm init` to write it",
            ));
            None
        }
        Ok(text) => match limits::check(&text) {
            Ok(limits) => Some(limits),
            Err(found) => {
                violations.extend(found);
                None
            }
        },
    };
    if let Some(limits) = &limits {
        if local {
            violations.extend(launch_checks(host, limits, &mut report));
        } else if limits.paused {
            violations.push(Violation::new(
                "LIMITS.toml",
                "limits-paused",
                "`paused = true`: nothing launches",
            ));
        }
    }
    let unit_dir = root.join(&target.unit);
    let mut unit = if store::is_unit(&unit_dir) {
        Some(store::load_unit(&unit_dir)?)
    } else {
        violations.push(Violation::new(
            &target.unit,
            "unit-missing",
            format!("no unit state; run `cm init {}`", target.unit),
        ));
        None
    };
    let existing = unit
        .as_ref()
        .and_then(|unit| unit.steps.get(step_name))
        .cloned();
    if let Some(step) = &existing {
        let fresh = step.track.state == Phase::Drafted;
        let relaunch = matches!(step.track.state, Phase::Blocked(_))
            && matches!(step.track.resting(), Phase::Drafted | Phase::Launched);
        if !fresh && !relaunch {
            violations.push(Violation::new(
                &full,
                "launch-state",
                format!(
                    "the step is {}; launch takes a new step or one blocked before its PR opened",
                    step.track.state
                ),
            ));
        }
        violations.extend(plan::unmet(&full, &plan::gates(&units, step)));
    }
    let step_dir = unit_dir.join(step_name);
    let files = packet_files(&step_dir);
    let resume = files
        .iter()
        .filter_map(|file| {
            let name = file.file_name()?.to_str()?;
            let stem = name.strip_prefix("resume")?.strip_suffix(".md")?;
            let number: u32 = stem.trim_start_matches(['-', '_']).parse().unwrap_or(0);
            Some((number, file))
        })
        .max()
        .map(|(_, file)| file);
    let kickoff = step_dir.join("kickoff.md");
    let relaunching = existing
        .as_ref()
        .is_some_and(|step| step.track.state != Phase::Drafted);
    let prompt: Option<PathBuf> = match (relaunching, resume) {
        (true, Some(resume)) => Some(resume.clone()),
        _ if kickoff.is_file() => Some(kickoff),
        _ => None,
    };
    if prompt.is_none() {
        violations.push(Violation::new(
            relative(root, &step_dir),
            "packet-kickoff",
            "the step folder has no kickoff.md",
        ));
    }
    let (checked, missing) = packet_paths(root, &files, lint::home().as_deref());
    violations.extend(missing);
    report.push(format!(
        "packet: {} file(s), {checked} named path(s) checked",
        files.len()
    ));
    let worktree = worktree.map(absolute);
    match &worktree {
        Some(worktree) if !worktree.join(".git").exists() => violations.push(Violation::new(
            worktree.to_string_lossy(),
            "worktree-missing",
            "the worktree does not exist; create it before the launch",
        )),
        None if local => violations.push(Violation::new(
            &full,
            "worktree-missing",
            "a local lane launches into a worktree; pass --worktree <path>",
        )),
        _ => {}
    }
    if !violations.is_empty() {
        let mut outcome = Outcome::refused(violations);
        outcome.lines = report;
        return Ok(outcome);
    }
    let (Some(limits), Some(unit), Some(prompt)) = (limits, unit.as_mut(), prompt) else {
        unreachable!("a launch with no violations has limits, a unit and a prompt");
    };
    let at = stamp(host.now());
    let worktree_text = worktree
        .as_ref()
        .map(|worktree| worktree.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut step = existing.unwrap_or_else(|| Step {
        track: Track::new(&at),
        worktree: worktree_text.clone(),
        launches: 0,
        lane: None,
        after: Vec::new(),
        github: None,
        tag: None,
        work: Work::Code,
    });
    let from = step.track.state.clone();
    step.track.apply(Phase::Launched, &at, &Evidence::default());
    step.worktree = worktree_text.clone();
    step.launches += 1;
    let launches = step.launches;
    unit.steps.insert(step_name.to_owned(), step);
    unit.history.push(Move {
        at: at.clone(),
        target: full.clone(),
        from,
        to: Phase::Launched,
        evidence: Evidence::default(),
    });
    if unit.track.state == Phase::Drafted {
        unit.track.apply(Phase::Launched, &at, &Evidence::default());
        unit.history.push(Move {
            at: at.clone(),
            target: target.unit.clone(),
            from: Phase::Drafted,
            to: Phase::Launched,
            evidence: Evidence::default(),
        });
    }
    write_unit(&unit_dir, unit)?;
    let leaf = target.unit.rsplit('/').next().unwrap_or(&target.unit);
    let step_text = step_dir.to_string_lossy();
    let command = local.then(|| {
        format!(
            "pi() {{ command pi -ne \"$@\"; }}; export -f pi; pi-turn.sh {} {} {} {} {}",
            shell_word(&step_text),
            shell_word(&format!("{leaf}-{step_name}-sup")),
            shell_word(&format!(
                "{}:{}",
                limits.supervisor_model, limits.supervisor_effort
            )),
            shell_word(&format!("{step_text}/sup-turn-{launches}.jsonl")),
            shell_word(&prompt.file_name().unwrap_or_default().to_string_lossy()),
        )
    });
    let mut body: String = report.iter().map(|line| format!("- {line}\n")).collect();
    if let Some(lane) = declared {
        body.push_str(&format!("- lane: {lane}\n"));
    }
    if !worktree_text.is_empty() {
        body.push_str(&format!("- worktree: `{worktree_text}`\n"));
    }
    match &command {
        Some(command) => body.push_str(&format!("- launch {launches}: `{command}`\n")),
        None => body.push_str(&format!(
            "- launch {launches}: `{}`\n",
            relative(root, &prompt)
        )),
    }
    let heading = record(root, &at, &format!("launch {full}"), &body)?;
    let mut outcome = Outcome {
        lines: report,
        ..Outcome::default()
    };
    if let Some(command) = &command {
        outcome.line(command.clone());
    } else {
        outcome.line(format!(
            "launched {full} in lane {}; its prompt is {}",
            declared.map_or("—", Lane::name),
            relative(root, &prompt)
        ));
    }
    outcome.field("target", full);
    outcome.field("launches", launches);
    outcome.field("lane", declared.map(Lane::name));
    outcome.field("prompt", relative(root, &prompt));
    outcome.field("launch", command);
    outcome.field("journal", heading);
    Ok(outcome)
}

fn git_commit(repo: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{rev}^{{commit}}"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn is_pr_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    parts.len() == 4
        && parts[..2].iter().all(|part| store::is_name(part))
        && parts[2] == "pull"
        && !parts[3].is_empty()
        && parts[3].bytes().all(|byte| byte.is_ascii_digit())
}

fn is_digest(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

pub fn advance(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    to: &Phase,
    given: &Evidence,
    file: Option<&Path>,
    repo: Option<&Path>,
) -> Result<Outcome> {
    let full = match &target.step {
        Some(step) => format!("{}/{step}", target.unit),
        None => target.unit.clone(),
    };
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    let unit_dir = root.join(&target.unit);
    if !store::is_unit(&unit_dir) {
        violations.push(Violation::new(
            &target.unit,
            "unit-missing",
            format!("no unit state; run `cm init {}`", target.unit),
        ));
        return Ok(Outcome::refused(violations));
    }
    let mut unit = store::load_unit(&unit_dir)?;
    let declared = target
        .step
        .as_ref()
        .and_then(|step| unit.steps.get(step))
        .cloned();
    let track = match &target.step {
        None => unit.track.clone(),
        Some(step) => match unit.steps.get(step) {
            Some(step) => step.track.clone(),
            None => {
                violations.push(Violation::new(
                    &full,
                    "step-unknown",
                    "the step has not launched; `cm launch` it first",
                ));
                return Ok(Outcome::refused(violations));
            }
        },
    };
    let finish = declared.as_ref().map_or(Finish::Shipped, Step::finish);
    let finishing_code = *to == Phase::Done && finish == Finish::Shipped && target.step.is_some();
    let code_finish = || {
        Violation::new(
            &full,
            "done-code-step",
            format!(
                "{full} is a code step: it finishes at deployed, or merged where it has no release; `done --evidence <path>` finishes an owner-lane step or one declared with --study"
            ),
        )
    };
    let tag_only = *to == Phase::Released
        && Release::of_target(&full).is_some()
        && matches!(track.resting(), Phase::Drafted | Phase::Launched);
    match track.check(to, finish) {
        Err(_) if tag_only && !track.state.is_final() => {}
        Err(_) if finishing_code && track.state != Phase::Done => violations.push(code_finish()),
        Err(message) => violations.push(Violation::new(&full, "advance-refused", message)),
        Ok(()) if finishing_code && file.is_some() => violations.push(code_finish()),
        Ok(()) => {}
    }
    if *to == Phase::Done
        && finish == Finish::Evidence
        && *track.resting() == Phase::Drafted
        && let Some(step) = &declared
    {
        let units = plan::load_units(root)?;
        violations.extend(plan::unmet(&full, &plan::gates(&units, step)));
    }
    let resuming = matches!(track.state, Phase::Blocked(_)) && track.resting() == to;
    let (required, optional) = if resuming {
        (&[][..], &[][..])
    } else {
        evidence_for(to, finish)
    };
    for flag in required {
        if !flag.given(given, file) {
            violations.push(Violation::new(
                &full,
                "evidence-missing",
                format!("{to} needs {}", flag.name()),
            ));
        }
    }
    for flag in FLAGS {
        if flag.given(given, file)
            && !required.contains(&flag)
            && !optional.contains(&flag)
            && !(finishing_code && flag == Flag::File)
        {
            violations.push(Violation::new(
                &full,
                "evidence-unexpected",
                format!("{to} takes no {}", flag.name()),
            ));
        }
    }
    let repos: Vec<PathBuf> = match (repo, &target.step) {
        (Some(repo), _) => vec![absolute(repo)],
        (None, Some(step)) => unit
            .steps
            .get(step)
            .map(|step| PathBuf::from(&step.worktree))
            .into_iter()
            .collect(),
        (None, None) => unit
            .steps
            .values()
            .map(|step| PathBuf::from(&step.worktree))
            .collect(),
    };
    let mut evidence = given.clone();
    evidence.file = None;
    if let Some(file) = file {
        match CampaignFile::resolve(root, file) {
            Ok(found) => evidence.file = Some(found),
            Err(error) => {
                violations.push(Violation::new(&full, "evidence-file", error.to_string()))
            }
        }
    }
    if let Some(pr) = &given.pr
        && !is_pr_url(pr)
    {
        violations.push(Violation::new(
            &full,
            "evidence-pr",
            format!("`{pr}` is not https://github.com/<owner>/<repo>/pull/<number>"),
        ));
    }
    if let Some(digest) = &given.digest
        && !is_digest(digest)
    {
        violations.push(Violation::new(
            &full,
            "evidence-digest",
            format!("`{digest}` is not sha256:<64 lowercase hex>"),
        ));
    }
    if let (Some(release), Some(step)) = (Release::of_target(&full), &declared) {
        let wanted = step.release_tag(&release);
        if let Some(tag) = &given.tag
            && *tag != wanted.as_str()
        {
            violations.push(Violation::new(
                &full,
                "release-tag",
                format!(
                    "`{tag}` is not {release}'s tag; the release step's tag is {wanted}, declared at `cm init`"
                ),
            ));
        }
        if let Some(repo) = given.pr.as_deref().and_then(GithubRepo::of_pr)
            && !step.ships_from(&release, &repo)
        {
            violations.push(Violation::new(
                &full,
                "release-repo",
                format!(
                    "the prep PR is in {repo}, but {release} ships from {}",
                    step.release_repo(&release)
                ),
            ));
        }
    }
    let needs_repo = given.sha.is_some() || given.tag.is_some();
    let gone: Vec<String> = repos
        .iter()
        .filter(|repo| !repo.exists())
        .map(|repo| repo.display().to_string())
        .collect();
    let repos: Vec<PathBuf> = repos.into_iter().filter(|repo| repo.exists()).collect();
    if needs_repo && repos.is_empty() {
        let message = if gone.is_empty() {
            "no repository to resolve --sha or --tag in; pass --repo <a checkout of the repo>"
                .to_owned()
        } else {
            format!(
                "the worktree {} is gone; pass --repo <a checkout of the repo>",
                gone.join(", ")
            )
        };
        violations.push(Violation::new(&full, "evidence-repo", message));
    } else {
        if let Some(sha) = &given.sha {
            let hex = sha.len() >= 7
                && sha.len() <= 40
                && sha.bytes().all(|byte| byte.is_ascii_hexdigit());
            match repos
                .iter()
                .find_map(|repo| hex.then(|| git_commit(repo, sha)).flatten())
            {
                Some(full_sha) => evidence.sha = Some(full_sha),
                None => violations.push(Violation::new(
                    &full,
                    "evidence-sha",
                    format!(
                        "`{sha}` is not a commit in {}; fetch first if it was made elsewhere, or pass --repo",
                        repos
                            .iter()
                            .map(|repo| repo.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )),
            }
        }
        if let Some(tag) = &given.tag
            && !repos
                .iter()
                .any(|repo| git_commit(repo, &format!("refs/tags/{tag}")).is_some())
        {
            violations.push(Violation::new(
                &full,
                "evidence-tag",
                format!("tag `{tag}` does not resolve; `git fetch --tags` first if CI made it, or pass --repo"),
            ));
        }
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    let from = track.state.clone();
    match &target.step {
        None => unit.track.apply(to.clone(), &at, &evidence),
        Some(step) => {
            if let Some(step) = unit.steps.get_mut(step) {
                step.track.apply(to.clone(), &at, &evidence);
            }
        }
    }
    unit.history.push(Move {
        at: at.clone(),
        target: full.clone(),
        from: from.clone(),
        to: to.clone(),
        evidence: evidence.clone(),
    });
    write_unit(&unit_dir, &unit)?;
    let body = format!("- evidence: {}\n", evidence.summary());
    let heading = record(root, &at, &format!("advance {full}: {from} → {to}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{full}: {from} → {to}"));
    outcome.field("target", full);
    outcome.field("from", from.to_string());
    outcome.field("to", to.to_string());
    outcome.field("evidence", serde_json::to_value(&evidence)?);
    outcome.field("journal", heading);
    Ok(outcome)
}

fn unit_and_track(
    root: &Path,
    target: &Target,
    violations: &mut Vec<Violation>,
) -> Result<Option<(UnitState, String)>> {
    let full = match &target.step {
        Some(step) => format!("{}/{step}", target.unit),
        None => target.unit.clone(),
    };
    let unit_dir = root.join(&target.unit);
    if !store::is_unit(&unit_dir) {
        violations.push(Violation::new(
            &target.unit,
            "unit-missing",
            format!("no unit state; run `cm init {}`", target.unit),
        ));
        return Ok(None);
    }
    let unit = store::load_unit(&unit_dir)?;
    if let Some(step) = &target.step
        && !unit.steps.contains_key(step)
    {
        violations.push(Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        ));
        return Ok(None);
    }
    Ok(Some((unit, full)))
}

fn track_mut<'a>(unit: &'a mut UnitState, target: &Target) -> &'a mut Track {
    match &target.step {
        None => &mut unit.track,
        Some(step) => &mut unit.steps.get_mut(step).expect("checked").track,
    }
}

/// Releases a blocked unit or step to the state it was blocked from, with the evidence that
/// cleared the block. A step that rests at drafted or launched still waits on every `--after`.
pub fn unblock(host: &dyn Host, root: &Path, target: &Target, file: &Path) -> Result<Outcome> {
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    let Some((mut unit, full)) = unit_and_track(root, target, &mut violations)? else {
        return Ok(Outcome::refused(violations));
    };
    let track = track_mut(&mut unit, target).clone();
    if !matches!(track.state, Phase::Blocked(_)) {
        violations.push(Violation::new(
            &full,
            "unblock-state",
            format!("{full} is {}, not blocked", track.state),
        ));
    }
    let mut evidence = Evidence::default();
    match CampaignFile::resolve(root, file) {
        Ok(found) => evidence.file = Some(found),
        Err(error) => violations.push(Violation::new(&full, "evidence-file", error.to_string())),
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    let to = track.resting().clone();
    track_mut(&mut unit, target).apply(to.clone(), &at, &evidence);
    unit.history.push(Move {
        at: at.clone(),
        target: full.clone(),
        from: track.state.clone(),
        to: to.clone(),
        evidence: evidence.clone(),
    });
    write_unit(&root.join(&target.unit), &unit)?;
    let units = plan::load_units(root)?;
    let waits: Vec<String> = plan::step(&units, &full)
        .filter(|_| matches!(to, Phase::Drafted | Phase::Launched))
        .map(|step| plan::unmet(&full, &plan::gates(&units, step)))
        .unwrap_or_default()
        .into_iter()
        .map(|violation| violation.message)
        .collect();
    let mut body = format!("- evidence: {}\n", evidence.summary());
    for wait in &waits {
        body.push_str(&format!("- still: {wait}\n"));
    }
    let heading = record(
        root,
        &at,
        &format!("unblock {full}: {} → {to}", track.state),
        &body,
    )?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{full}: {} → {to}", track.state));
    outcome.lines.extend(waits.iter().cloned());
    outcome.field("target", full);
    outcome.field("from", track.state.to_string());
    outcome.field("to", to.to_string());
    outcome.field("waits_on", waits);
    outcome.field("journal", heading);
    Ok(outcome)
}

#[derive(Debug, Clone, Default)]
pub struct Amendment {
    pub study: bool,
    pub lane: Option<Lane>,
    pub after: Vec<String>,
}

fn reaches(units: &Units, from: &str, to: &str) -> bool {
    let mut stack = vec![from.to_owned()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(at) = stack.pop() {
        if at == to {
            return true;
        }
        if seen.insert(at.clone())
            && let Some(step) = plan::step(units, &at)
        {
            stack.extend(step.after.iter().map(After::target));
        }
    }
    false
}

/// Changes a declared step that has not opened its PR: marks it a study, moves its lane or adds a
/// gate. Journaled like any move; a gate that would close a cycle is refused.
pub fn amend(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    amendment: &Amendment,
) -> Result<Outcome> {
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    let Some((mut unit, full)) = unit_and_track(root, target, &mut violations)? else {
        return Ok(Outcome::refused(violations));
    };
    let Some(step_name) = target.step.as_deref() else {
        violations.push(Violation::new(
            &full,
            "amend-target",
            "`cm amend` takes a step",
        ));
        return Ok(Outcome::refused(violations));
    };
    let units = plan::load_units(root)?;
    let step = unit.steps[step_name].clone();
    if step.lane.is_none() {
        violations.push(Violation::new(
            &full,
            "amend-target",
            "the step was launched without a declaration; only a declared step is amended",
        ));
    }
    if !matches!(step.track.resting(), Phase::Drafted | Phase::Launched) {
        violations.push(Violation::new(
            &full,
            "amend-state",
            format!(
                "{full} is {}; only a drafted or launched step is amended",
                step.track.state
            ),
        ));
    }
    let mut changes = Vec::new();
    let mut next = step.clone();
    if amendment.study && step.work.is_code() {
        if Release::of_target(&full).is_some() {
            violations.push(Violation::new(
                &full,
                "amend-target",
                "a release step ships a tag; --study marks a step that ships none",
            ));
        }
        next.work = Work::Study;
        changes
            .push("- study: finishes with `cm advance <step> done --evidence <path>`".to_owned());
    }
    if let Some(lane) = amendment.lane.filter(|lane| step.lane != Some(*lane)) {
        next.lane = Some(lane);
        changes.push(format!(
            "- lane: {} → {lane}",
            step.lane.map_or("—", Lane::name)
        ));
    }
    for text in &amendment.after {
        match After::parse(text, &target.unit) {
            Err(message) => violations.push(Violation::new(&full, "after-parse", message)),
            Ok(after) if next.after.contains(&after) => {}
            Ok(after) if plan::step(&units, &after.target()).is_none() => {
                violations.push(Violation::new(
                    &full,
                    "after-unknown",
                    format!("`{text}` names {}, which is not declared", after.target()),
                ))
            }
            Ok(after) if reaches(&units, &after.target(), &full) => {
                violations.push(Violation::new(
                    &full,
                    "after-cycle",
                    format!(
                        "{} already waits on {full}; the order would have a cycle",
                        after.target()
                    ),
                ))
            }
            Ok(after) => {
                changes.push(format!("- after: + {after}"));
                next.after.push(after);
            }
        }
    }
    if changes.is_empty() && violations.is_empty() {
        violations.push(Violation::new(
            &full,
            "amend-empty",
            "nothing changes; pass --study, a new --lane or a new --after",
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    unit.steps.insert(step_name.to_owned(), next);
    write_unit(&root.join(&target.unit), &unit)?;
    let body: String = changes.iter().map(|line| format!("{line}\n")).collect();
    let heading = record(root, &at, &format!("amend {full}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!("amended {full}"));
    outcome.lines.extend(changes.iter().cloned());
    outcome.field("target", full);
    outcome.field("changes", changes);
    outcome.field("journal", heading);
    Ok(outcome)
}
