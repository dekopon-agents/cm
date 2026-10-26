use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::evidence::CampaignFile;
use crate::forge::{CheckState, CommitSha, Forge, GithubRepo, TagName, ci_green};
use crate::host::{Host, stamp};
use crate::journal;
use crate::limits::{self, Limits};
use crate::lint::{self, packet_files, packet_paths};
use crate::outcome::{Outcome, Violation};
use crate::plan::{self, Units};
use crate::render;
use crate::state::{
    After, Attempt, CampaignState, DEFAULT_TAG, Evidence, FLAGS, Finish, Flag, JournalMark, Kind,
    Lane, Move, Phase, RELEASES, Release, SCHEMA, Step, Track, UnitState, Waived, Work,
    evidence_for,
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
            lane: Some(lane),
            after: parsed.clone(),
            github: ships.github.clone(),
            tag: tag.clone(),
            work,
            ..Step::new(&at)
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

/// What a remote launch records in its attempt.
#[derive(Debug, Clone, Default)]
pub struct Remote {
    pub coordinator: Option<String>,
    pub cap_min: Option<u32>,
}

pub fn launch(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    worktree: Option<&Path>,
) -> Result<Outcome> {
    launch_remote(host, root, target, worktree, &Remote::default())
}

pub fn launch_remote(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    worktree: Option<&Path>,
    remote: &Remote,
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
        if let Some(attempt) = step
            .attempt
            .as_ref()
            .filter(|attempt| attempt.ended.is_none())
        {
            violations.push(Violation::new(
                &full,
                "attempt-open",
                format!(
                    "attempt {} has not ended; resume it, or end it with `cm attempt {full} end <outcome>`, before a new launch",
                    attempt.id
                ),
            ));
        }
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
    let mut step = existing.unwrap_or_else(|| Step::new(&at));
    let from = step.track.state.clone();
    step.track.apply(Phase::Launched, &at, &Evidence::default());
    step.worktree = worktree_text.clone();
    step.launches += 1;
    let launches = step.launches;
    if !local {
        step.attempt = Some(Attempt {
            id: format!("{full}#{launches}"),
            generation: 1,
            coordinator: remote.coordinator.clone().unwrap_or_else(|| "—".to_owned()),
            claimed_at: at.clone(),
            deadline: remote
                .cap_min
                .map(|minutes| stamp(host.now() + jiff::SignedDuration::from_mins(minutes.into()))),
            claim: String::new(),
            actions: Vec::new(),
            ended: None,
        });
        if let Some(attempt) = step.attempt.as_mut() {
            attempt.mint(host.now());
        }
    }
    let attempt = step.attempt.clone().filter(|_| !local);
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
    if let Some(attempt) = &attempt {
        body.push_str(&format!(
            "- attempt: {} (generation 1, coordinator {}, deadline {})\n",
            attempt.id,
            attempt.coordinator,
            attempt.deadline.as_deref().unwrap_or("—")
        ));
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
    outcome.field("attempt", serde_json::to_value(&attempt)?);
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

/// What `cm advance <codex step> merged` reads for its merge gate.
#[derive(Default)]
pub struct MergeGate<'a> {
    /// GitHub, for the PR head and its CI; `None` reads as unreachable.
    pub forge: Option<&'a dyn Forge>,
    /// The PR head when GitHub cannot say; else it must match GitHub's.
    pub head: Option<String>,
    /// A campaign file showing green CI at the head, taken only when GitHub cannot be read.
    pub ci_verified: Option<PathBuf>,
    /// A campaign file that waives the gate; the waiver and what it waived are journaled.
    pub waiver: Option<PathBuf>,
}

impl MergeGate<'_> {
    fn given(&self) -> Vec<&'static str> {
        [
            (self.head.is_some(), "--head"),
            (self.ci_verified.is_some(), "--ci-verified"),
            (self.waiver.is_some(), "--override"),
        ]
        .into_iter()
        .filter_map(|(given, flag)| given.then_some(flag))
        .collect()
    }
}

/// What GitHub says of a PR for its merge gate, read before cm takes the lock.
struct GithubRead {
    pr: String,
    head: String,
    base: Option<String>,
    checks: Result<Vec<CheckState>, String>,
    /// CI at the PR's base, read only when a check failed at the head.
    base_checks: Option<Vec<CheckState>>,
    /// `--head` disagreed with GitHub's head.
    mismatch: Option<String>,
}

fn read_github(gate: &MergeGate, pr: &str, recorded: Option<&str>) -> GithubRead {
    let pull = match gate.forge {
        Some(forge) => forge.pull(pr).map_err(|error| error.to_string()),
        None => Err("no GitHub reader".to_owned()),
    };
    let fallback = || {
        gate.head
            .clone()
            .or_else(|| recorded.map(str::to_owned))
            .unwrap_or_default()
    };
    let base = pull
        .as_ref()
        .ok()
        .and_then(|pull| pull.base.as_ref())
        .map(ToString::to_string);
    let (head, unreachable) = match pull.as_ref().map(|pull| pull.head.as_ref()) {
        Ok(Some(head)) => (head.to_string(), None),
        Ok(None) => (
            fallback(),
            Some(format!("GitHub reported no head for {pr}")),
        ),
        Err(error) => (
            fallback(),
            Some(format!("cannot read {pr} from GitHub: {error}")),
        ),
    };
    let mismatch = gate
        .head
        .as_ref()
        .filter(|given| unreachable.is_none() && !head.starts_with(&given.to_lowercase()))
        .map(|given| format!("--head {given} is not {pr}'s head {head}"));
    let checks = match (&unreachable, gate.forge, GithubRepo::of_pr(pr)) {
        (Some(why), _, _) => Err(why.clone()),
        (None, Some(forge), Some(repo)) => forge
            .checks(&repo, &CommitSha::new(head.clone()))
            .map_err(|error| format!("cannot read CI at {head}: {error}")),
        (None, _, _) => Err(format!("cannot read CI for {pr}")),
    };
    let failed = checks
        .as_ref()
        .is_ok_and(|checks| checks.iter().any(|check| !check.green()));
    let base_checks = match (failed, gate.forge, GithubRepo::of_pr(pr), &base) {
        (true, Some(forge), Some(repo), Some(base)) => {
            forge.checks(&repo, &CommitSha::new(base.clone())).ok()
        }
        _ => None,
    };
    GithubRead {
        pr: pr.to_owned(),
        head,
        base,
        checks,
        base_checks,
        mismatch,
    }
}

struct GateRead {
    gaps: Vec<String>,
    lines: Vec<String>,
    head: String,
    red: Vec<String>,
}

pub(crate) fn red_checks(
    exceptions: &[crate::state::KnownRed],
    units: &Units,
    head: &str,
    checks: &[CheckState],
    base: Option<&[CheckState]>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    for check in checks.iter().filter(|check| !check.green()) {
        match crate::holds::cover(exceptions, units, head, check, base) {
            Ok(exception) => accepted.push(crate::holds::used_line(head, exception)),
            Err(crate::holds::Uncovered::NoException) => {
                refused.push((
                    check.name.clone(),
                    format!("{} {}", check.name, check.state),
                ));
            }
            Err(why) => refused.push((
                check.name.clone(),
                format!("{} {} ({why})", check.name, check.state),
            )),
        }
    }
    (accepted, refused)
}

struct Gated<'a> {
    track: &'a Track,
    exceptions: &'a [crate::state::KnownRed],
}

/// The codex merge gate: (a) an Astra receipt and (b) a cleared review at the PR head from the
/// step's evidence and `.cm/verdicts.json`, (c) green CI at that head from GitHub or
/// `--ci-verified`, where an open known-red exception may accept a failing check.
fn merge_gate(
    root: &Path,
    target: &Target,
    gated: Gated,
    gate: &MergeGate,
    read: Option<GithubRead>,
    repo: Option<&Path>,
    violations: &mut Vec<Violation>,
) -> Result<GateRead> {
    let full = format!(
        "{}/{}",
        target.unit,
        target.step.as_deref().unwrap_or_default()
    );
    let mut gaps = Vec::new();
    let mut lines = Vec::new();
    let Gated { track, exceptions } = gated;
    let mut red = Vec::new();
    let Some(pr) = track.evidence.pr.clone() else {
        gaps.push("no PR is recorded; the gate reads the PR's head".to_owned());
        return Ok(GateRead {
            gaps,
            lines,
            head: String::new(),
            red,
        });
    };
    let read = match read.filter(|read| read.pr == pr) {
        Some(read) => read,
        None => read_github(gate, &pr, track.evidence.sha.as_deref()),
    };
    if let Some(mismatch) = &read.mismatch {
        violations.push(Violation::new(&full, "merge-head", mismatch.clone()));
    }
    let head = read.head.clone();
    if head.is_empty() {
        gaps.push("the PR head is unknown; pass --head <sha>".to_owned());
        return Ok(GateRead {
            gaps,
            lines,
            head,
            red,
        });
    }
    lines.push(format!("- merge gate at {pr} @{head}"));
    let lineage = repo.map(|repo| crate::verdict::Lineage {
        repo,
        base: read.base.as_deref(),
    });
    let cleared = crate::verdict::merge_gaps(root, target, Some(&pr), &head, lineage.as_ref())?;
    if let Some(reviewed) = &cleared.reviewed {
        lines.push(format!(
            "- reviewed at {reviewed}; every commit since is a clean update from the base branch"
        ));
    }
    gaps.extend(cleared.gaps);
    match read.checks {
        Ok(checks) if ci_green(&checks) => {
            lines.push(format!("- CI green at {head} ({} check(s))", checks.len()));
        }
        Ok(checks) if checks.is_empty() => gaps.push(format!("GitHub reports no CI at {head}")),
        Ok(checks) => {
            let units = plan::load_units(root)?;
            let (accepted, refused) =
                red_checks(exceptions, &units, &head, &checks, read.base_checks.as_deref());
            lines.extend(accepted);
            if refused.is_empty() {
                lines.push(format!(
                    "- CI green at {head} but for known-red check(s) ({} check(s))",
                    checks.len()
                ));
            } else {
                let named: Vec<&str> = refused.iter().map(|(_, text)| text.as_str()).collect();
                gaps.push(format!("CI is not green at {head}: {}", named.join(", ")));
                red = refused.into_iter().map(|(name, _)| name).collect();
            }
        }
        Err(why) => match &gate.ci_verified {
            Some(file) => match CampaignFile::resolve(root, file) {
                Ok(found) => lines.push(format!("- CI verified by {found} ({why})")),
                Err(error) => {
                    violations.push(Violation::new(&full, "evidence-file", error.to_string()))
                }
            },
            None => gaps.push(format!(
                "{why}; CI at {head} is unknown: pass --ci-verified <evidence> once you have checked it"
            )),
        },
    }
    Ok(GateRead {
        gaps,
        lines,
        head,
        red,
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
    advance_gated(
        host,
        root,
        target,
        Advance {
            to,
            given,
            file,
            repo,
            part: None,
        },
        &MergeGate::default(),
    )
}

#[derive(Clone, Copy)]
pub struct Advance<'a> {
    pub to: &'a Phase,
    pub given: &'a Evidence,
    pub file: Option<&'a Path>,
    pub repo: Option<&'a Path>,
    pub part: Option<&'a str>,
}

pub fn advance_gated(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    advance: Advance,
    gate: &MergeGate,
) -> Result<Outcome> {
    let Advance {
        to,
        given,
        file,
        repo,
        part,
    } = advance;
    let step_full = match &target.step {
        Some(step) => format!("{}/{step}", target.unit),
        None => target.unit.clone(),
    };
    let full = match part {
        Some(part) => format!("{step_full}#{part}"),
        None => step_full.clone(),
    };
    // GitHub is read before the lock, as `cm sync` reads it; the gate re-reads under the lock
    // only if the step's PR changed meanwhile.
    let read = (*to == Phase::Merged)
        .then(|| store::load_unit(&root.join(&target.unit)).ok())
        .flatten()
        .and_then(|unit| unit.steps.get(target.step.as_deref()?).cloned())
        .filter(|step| step.lane == Some(Lane::Codex))
        .and_then(|step| {
            let track = match part {
                Some(part) => step.parts.get(part)?,
                None => &step.track,
            };
            let pr = track.evidence.pr.as_deref()?;
            Some(read_github(gate, pr, track.evidence.sha.as_deref()))
        });
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
            Some(step) => match part {
                None => step.track.clone(),
                Some(part) => match step.parts.get(part) {
                    Some(track) => track.clone(),
                    None => {
                        violations.push(Violation::new(
                            &full,
                            "part-unknown",
                            format!("{step_full} declares no part `{part}`; `cm amend {step_full} --part {part}` adds one"),
                        ));
                        return Ok(Outcome::refused(violations));
                    }
                },
            },
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
    if let (Some(name), Some(step)) = (part, &declared) {
        match to {
            Phase::PrOpen | Phase::Merged | Phase::Released | Phase::Deployed => {}
            Phase::Drafted
            | Phase::Launched
            | Phase::Done
            | Phase::Superseded
            | Phase::Blocked(_) => violations.push(Violation::new(
                &full,
                "part-state",
                format!("a part moves to pr-open, merged, released or deployed; {to} belongs to {step_full}"),
            )),
        }
        if *step.track.resting() == Phase::Drafted {
            violations.push(Violation::new(
                &full,
                "part-step-drafted",
                format!("{step_full} has not launched; `cm launch` it before part {name} moves"),
            ));
        }
    } else if let Some(step) = &declared {
        let short = step.short_parts(to);
        if !short.is_empty() {
            let named: Vec<String> = short
                .iter()
                .map(|(name, state)| format!("{name} ({state})"))
                .collect();
            violations.push(Violation::new(
                &full,
                "part-short",
                format!(
                    "{full} moves to {to} only when every part has; short: {}. Move each with `--part <name>`",
                    named.join(", ")
                ),
            ));
        }
    }
    let watched = match to {
        Phase::Deployed | Phase::Done => part.is_none(),
        Phase::Drafted
        | Phase::Launched
        | Phase::PrOpen
        | Phase::Merged
        | Phase::Released
        | Phase::Superseded
        | Phase::Blocked(_) => false,
    };
    if watched && let Some(step) = &declared {
        for watch in step.unresolved_watches() {
            violations.push(Violation::new(
                &full,
                "watch-open",
                format!(
                    "{full}#{} ({}, deadline {}) is {}; resolve it with `cm watch {full} resolve {} --evidence <path>`",
                    watch.id,
                    watch.signal,
                    watch.deadline,
                    watch.status(host.now()).name(),
                    watch.id
                ),
            ));
        }
    }
    let single = (*to == Phase::Done && finish == Finish::Evidence && file.is_none())
        .then(|| crate::ledger::single_evidence(root, target))
        .flatten();
    let file = file.or(single.as_deref());
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
            Ok(found) => {
                if let Some(pin) = declared.as_ref().and_then(|step| {
                    step.pins
                        .iter()
                        .find(|pin| pin.file == found && pin.stale.is_some())
                }) {
                    violations.push(Violation::new(
                        &full,
                        "evidence-stale",
                        format!(
                            "{found} was pinned to {} and is stale: {}",
                            pin.candidate,
                            pin.stale.as_deref().unwrap_or_default()
                        ),
                    ));
                }
                evidence.file = Some(found);
            }
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
    let gated = *to == Phase::Merged
        && !resuming
        && declared.as_ref().and_then(|step| step.lane) == Some(Lane::Codex);
    let mut gate_lines = Vec::new();
    let mut waived = Vec::new();
    if gated {
        let exceptions = declared
            .as_ref()
            .map(|step| step.known_red.as_slice())
            .unwrap_or_default();
        let GateRead {
            gaps,
            lines,
            head,
            red,
        } = merge_gate(
            root,
            target,
            Gated {
                track: &track,
                exceptions,
            },
            gate,
            read,
            repos.first().map(PathBuf::as_path),
            &mut violations,
        )?;
        gate_lines = lines;
        match &gate.waiver {
            Some(waiver) => match CampaignFile::resolve(root, waiver) {
                Ok(found) => {
                    gate_lines.push(format!("- merge gate overridden by {found}"));
                    gate_lines.extend(gaps.iter().map(|gap| format!("- waived: {gap}")));
                    waived = red
                        .into_iter()
                        .map(|check| Waived {
                            commit: head.clone(),
                            check,
                            file: found.clone(),
                            at: stamp(host.now()),
                        })
                        .collect();
                }
                Err(error) => {
                    violations.push(Violation::new(&full, "evidence-file", error.to_string()))
                }
            },
            None => violations.extend(
                gaps.into_iter()
                    .map(|gap| Violation::new(&full, "merge-gate", gap)),
            ),
        }
    } else if !gate.given().is_empty() {
        violations.push(Violation::new(
            &full,
            "evidence-unexpected",
            format!(
                "{} apply only to the merge gate of a codex step moving to merged",
                gate.given().join(", ")
            ),
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    let from = track.state.clone();
    let mut rolled = Vec::new();
    match &target.step {
        None => unit.track.apply(to.clone(), &at, &evidence),
        Some(step) => {
            if let Some(step) = unit.steps.get_mut(step) {
                match part.and_then(|part| step.parts.get_mut(part)) {
                    Some(track) => {
                        track.apply(to.clone(), &at, &evidence);
                        rolled = step.roll_up_all(&at, &evidence);
                    }
                    None => step.track.apply(to.clone(), &at, &evidence),
                }
                step.waived.append(&mut waived);
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
    for (from, to) in &rolled {
        unit.history.push(Move {
            at: at.clone(),
            target: step_full.clone(),
            from: from.clone(),
            to: to.clone(),
            evidence: evidence.clone(),
        });
    }
    write_unit(&unit_dir, &unit)?;
    let mut body = format!("- evidence: {}\n", evidence.summary());
    let rolled_lines: Vec<String> = rolled
        .iter()
        .map(|(from, to)| format!("{step_full}: {from} → {to}, every part has reached {to}"))
        .collect();
    for line in &rolled_lines {
        body.push_str(&format!("- {line}\n"));
    }
    for line in &gate_lines {
        body.push_str(line);
        body.push('\n');
    }
    let heading = record(root, &at, &format!("advance {full}: {from} → {to}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{full}: {from} → {to}"));
    outcome.lines.extend(rolled_lines.iter().cloned());
    if !rolled_lines.is_empty() {
        outcome.field("rolled_up", rolled_lines);
    }
    outcome.lines.extend(
        gate_lines
            .iter()
            .map(|line| line.trim_start_matches("- ").to_owned()),
    );
    if gated {
        outcome.field("merge_gate", gate_lines.clone());
    }
    outcome.field("target", full);
    outcome.field("from", from.to_string());
    outcome.field("to", to.to_string());
    outcome.field("evidence", serde_json::to_value(&evidence)?);
    outcome.field("journal", heading);
    Ok(outcome)
}

/// Moves a declared step to `blocked:<reason>` as `cm advance` would, under the lock the caller
/// holds, and journals the move; `None` when the step cannot move there (already blocked, final).
pub(crate) fn block_step(
    root: &Path,
    target: &Target,
    reason: &str,
    at: &str,
    evidence: &Evidence,
) -> Result<Option<String>> {
    let unit_dir = root.join(&target.unit);
    let mut unit = store::load_unit(&unit_dir)?;
    let to = Phase::Blocked(reason.to_owned());
    let Some(step) = target
        .step
        .as_ref()
        .and_then(|step| unit.steps.get_mut(step))
    else {
        return Ok(None);
    };
    let blocked = match step.track.state {
        Phase::Blocked(_) => true,
        Phase::Drafted
        | Phase::Launched
        | Phase::PrOpen
        | Phase::Merged
        | Phase::Released
        | Phase::Deployed
        | Phase::Done
        | Phase::Superseded => false,
    };
    if blocked || step.track.check(&to, step.finish()).is_err() {
        return Ok(None);
    }
    let from = step.track.state.clone();
    step.track.apply(to.clone(), at, evidence);
    let full = format!(
        "{}/{}",
        target.unit,
        target.step.as_deref().unwrap_or_default()
    );
    unit.history.push(Move {
        at: at.to_owned(),
        target: full.clone(),
        from: from.clone(),
        to: to.clone(),
        evidence: evidence.clone(),
    });
    write_unit(&unit_dir, &unit)?;
    let body = format!("- evidence: {}\n", evidence.summary());
    record(root, at, &format!("advance {full}: {from} → {to}"), &body).map(Some)
}

fn unit_of(
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
    Ok(Some((store::load_unit(&unit_dir)?, full)))
}

fn step_unknown(full: &str) -> Violation {
    Violation::new(full, "step-unknown", "the step is not declared")
}

fn track_mut<'a>(unit: &'a mut UnitState, target: &Target) -> Option<&'a mut Track> {
    match &target.step {
        None => Some(&mut unit.track),
        Some(step) => unit.steps.get_mut(step).map(|step| &mut step.track),
    }
}

/// Releases a blocked unit or step to the state it was blocked from, with the evidence that
/// cleared the block. A step that rests at drafted or launched still waits on every `--after`.
pub fn unblock(host: &dyn Host, root: &Path, target: &Target, file: &Path) -> Result<Outcome> {
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    let Some((mut unit, full)) = unit_of(root, target, &mut violations)? else {
        return Ok(Outcome::refused(violations));
    };
    let Some(current) = track_mut(&mut unit, target) else {
        violations.push(step_unknown(&full));
        return Ok(Outcome::refused(violations));
    };
    let track = current.clone();
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
    current.apply(to.clone(), &at, &evidence);
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
    pub drop_after: Vec<String>,
    pub parts: Vec<String>,
}

/// Declares parts on `step`. A step that already moved past launched with no parts hands its track
/// to the first new part; the step then rests at launched until every part reaches a state.
fn add_parts(
    full: &str,
    names: &[String],
    step: &mut Step,
    at: &str,
    changes: &mut Vec<String>,
    violations: &mut Vec<Violation>,
) {
    if Release::of_target(full).is_some() {
        violations.push(Violation::new(
            full,
            "part-target",
            "a release step ships one prep PR and one tag; it has no parts",
        ));
        return;
    }
    let launched = Track {
        state: Phase::Launched,
        since: at.to_owned(),
        blocked_from: None,
        evidence: Evidence::default(),
    };
    let advanced = step.parts.is_empty()
        && step
            .track
            .resting()
            .rank()
            .zip(Phase::Launched.rank())
            .is_some_and(|(at, launched)| at > launched);
    for (index, name) in names.iter().enumerate() {
        if !crate::store::is_name(name) || step.parts.contains_key(name) {
            violations.push(Violation::new(
                full,
                "part-name",
                format!("`{name}` is not a new part name of letters, digits, `.`, `_` and `-`"),
            ));
            continue;
        }
        let part = if advanced && index == 0 {
            Track {
                state: step.track.resting().clone(),
                blocked_from: None,
                ..step.track.clone()
            }
        } else {
            launched.clone()
        };
        changes.push(format!(
            "- part: + {name} ({}; {})",
            part.state,
            part.evidence.summary()
        ));
        step.parts.insert(name.clone(), part);
    }
    let reach = step.parts_reach().and_then(|reach| reach.rank());
    let resting = step.track.resting().rank();
    if let (Some(reach), Some(resting)) = (reach.max(Phase::Launched.rank()), resting)
        && resting > reach
    {
        let to = crate::state::ORDER[reach].clone();
        changes.push(format!(
            "- rests: {} → {to}; the step moves on only when every part has",
            step.track.state
        ));
        if step.track.blocked_from.is_some() {
            step.track.blocked_from = Some(to);
        } else {
            step.track.state = to;
        }
        step.track.since = at.to_owned();
        step.track.evidence = Evidence::default();
    }
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
    let Some((mut unit, full)) = unit_of(root, target, &mut violations)? else {
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
    let Some(step) = unit.steps.get(step_name).cloned() else {
        violations.push(step_unknown(&full));
        return Ok(Outcome::refused(violations));
    };
    if step.lane.is_none() {
        violations.push(Violation::new(
            &full,
            "amend-target",
            "the step was launched without a declaration; only a declared step is amended",
        ));
    }
    let only_parts = !amendment.study
        && amendment.lane.is_none()
        && amendment.after.is_empty()
        && amendment.drop_after.is_empty()
        && !amendment.parts.is_empty();
    let early = match step.track.resting() {
        Phase::Drafted | Phase::Launched => true,
        Phase::PrOpen
        | Phase::Merged
        | Phase::Released
        | Phase::Deployed
        | Phase::Done
        | Phase::Superseded
        | Phase::Blocked(_) => false,
    };
    if step.track.state.is_final() || (!early && !only_parts) {
        violations.push(Violation::new(
            &full,
            "amend-state",
            format!(
                "{full} is {}; only a drafted or launched step is amended, and --part alone amends any step short of done",
                step.track.state
            ),
        ));
    }
    let mut changes = Vec::new();
    let mut next = step.clone();
    if !amendment.parts.is_empty() {
        let at = stamp(host.now());
        add_parts(
            &full,
            &amendment.parts,
            &mut next,
            &at,
            &mut changes,
            &mut violations,
        );
    }
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
    for text in &amendment.drop_after {
        match After::parse(text, &target.unit) {
            Err(message) => violations.push(Violation::new(&full, "after-parse", message)),
            Ok(after) => {
                if next.after.contains(&after) {
                    next.after.retain(|gate| gate != &after);
                    changes.push(format!("- after: - {after}"));
                } else {
                    violations.push(Violation::new(
                        &full,
                        "after-absent",
                        format!("{full} does not have gate `{after}`"),
                    ));
                }
            }
        }
    }
    for text in &amendment.after {
        match After::parse(text, &target.unit) {
            Err(message) => violations.push(Violation::new(&full, "after-parse", message)),
            Ok(after) if next.after.contains(&after) => {}
            Ok(after) => {
                changes.push(format!("- after: + {after}"));
                next.after.push(after);
            }
        }
    }
    for after in &next.after {
        if plan::step(&units, &after.target()).is_none() {
            violations.push(Violation::new(
                &full,
                "after-unknown",
                format!("`{after}` names {}, which is not declared", after.target()),
            ));
        } else if reaches(&units, &after.target(), &full) {
            violations.push(Violation::new(
                &full,
                "after-cycle",
                format!(
                    "{} already waits on {full}; the order would have a cycle",
                    after.target()
                ),
            ));
        }
    }
    if next == step && violations.is_empty() {
        violations.push(Violation::new(
            &full,
            "amend-empty",
            "nothing changes; pass --study, a new --lane, --after, --drop-after or --part",
        ));
    }
    if !next.parts.is_empty() && next.finish() == Finish::Evidence {
        violations.push(Violation::new(
            &full,
            "part-target",
            "parts are PRs: a study or owner-lane step finishes with evidence and has none",
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    if next.track.state != step.track.state {
        unit.history.push(Move {
            at: at.clone(),
            target: full.clone(),
            from: step.track.state.clone(),
            to: next.track.state.clone(),
            evidence: Evidence::default(),
        });
    }
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
