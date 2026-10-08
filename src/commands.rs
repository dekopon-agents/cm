use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::host::{Host, stamp};
use crate::journal;
use crate::limits::{self, Limits};
use crate::lint::{self, packet_files, packet_paths};
use crate::outcome::{Outcome, Violation};
use crate::render;
use crate::state::{
    CampaignState, Evidence, Flag, JournalMark, Kind, Move, Phase, SCHEMA, Step, Track, UnitState,
    evidence_for,
};
use crate::store::{
    self, JOURNAL, JOURNAL_COPY, Target, UNIT_STATE, absolute, create_new, find_root, lock,
    parse_target, relative, state_path, write_atomic, write_json,
};
use crate::templates::{self, ROOT_FILES};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn record(root: &Path, at: &str, title: &str, body: &str) -> Result<String> {
    let entry = journal::entry(at, title, Some(body));
    journal::append(root, &entry)?;
    Ok(journal::heading(at, title))
}

fn write_unit(dir: &Path, unit: &UnitState) -> Result<()> {
    write_json(&state_path(dir), unit)?;
    write_atomic(&dir.join(UNIT_STATE), render::unit(unit).as_bytes())
}

pub fn init(host: &dyn Host, dir: &Path) -> Result<Outcome> {
    let dir = absolute(dir);
    if let Some(root) = dir.parent().and_then(find_root) {
        let rel = relative(&root, &dir);
        return match parse_target(&rel, false) {
            Ok(Target { unit, step: None }) => init_unit(host, &root, &dir, &unit),
            _ => Ok(Outcome::refused(vec![Violation::new(
                rel,
                "init-target",
                format!(
                    "{} is inside the campaign at {}; `cm init` there takes a unit folder, <NN-name>/<unit>",
                    dir.display(),
                    root.display()
                ),
            )])),
        };
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
                    format!("JOURNAL.md is gone; restore it from {JOURNAL_COPY}"),
                ));
            }
            continue;
        }
        if create_new(&path, template.as_bytes())? {
            written.push(name.to_owned());
            if name == JOURNAL {
                write_atomic(&dir.join(JOURNAL_COPY), template.as_bytes())?;
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
        let found = templates::conform(name, &text);
        if !found.is_empty() {
            violations.extend(found);
            continue;
        }
        adopted.push(name.to_owned());
        if name == JOURNAL {
            write_atomic(&dir.join(JOURNAL_COPY), text.as_bytes())?;
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
    let guard = journal::check(root, &campaign);
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

pub fn launch(host: &dyn Host, root: &Path, target: &Target, worktree: &Path) -> Result<Outcome> {
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
        violations.extend(launch_checks(host, limits, &mut report));
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
        let relaunch = matches!(step.track.state, Phase::Blocked(_))
            && matches!(step.track.resting(), Phase::Drafted | Phase::Launched);
        if !relaunch {
            violations.push(Violation::new(
                &full,
                "launch-state",
                format!(
                    "the step is {}; launch takes a new step or one blocked before its PR opened",
                    step.track.state
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
    let prompt: Option<PathBuf> = match (&existing, resume) {
        (Some(_), Some(resume)) => Some(resume.clone()),
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
    let worktree = absolute(worktree);
    if !worktree.join(".git").exists() {
        violations.push(Violation::new(
            worktree.to_string_lossy(),
            "worktree-missing",
            "the worktree does not exist; create it before the launch",
        ));
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
    let worktree_text = worktree.to_string_lossy().into_owned();
    let mut step = existing.unwrap_or_else(|| Step {
        track: Track::new(&at),
        worktree: worktree_text.clone(),
        launches: 0,
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
    let command = format!(
        "pi() {{ command pi -ne \"$@\"; }}; export -f pi; pi-turn.sh {} {} {} {} {}",
        shell_word(&step_text),
        shell_word(&format!("{leaf}-{step_name}-sup")),
        shell_word(&format!(
            "{}:{}",
            limits.supervisor_model, limits.supervisor_effort
        )),
        shell_word(&format!("{step_text}/sup-turn-{launches}.jsonl")),
        shell_word(&prompt.file_name().unwrap_or_default().to_string_lossy()),
    );
    let mut body: String = report.iter().map(|line| format!("- {line}\n")).collect();
    body.push_str(&format!(
        "- worktree: `{worktree_text}`\n- launch {launches}: `{command}`\n"
    ));
    let heading = record(root, &at, &format!("launch {full}"), &body)?;
    let mut outcome = Outcome {
        lines: report,
        ..Outcome::default()
    };
    outcome.line(command.clone());
    outcome.field("target", full);
    outcome.field("launches", launches);
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
    repo: Option<&Path>,
) -> Result<Outcome> {
    let full = match &target.step {
        Some(step) => format!("{}/{step}", target.unit),
        None => target.unit.clone(),
    };
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
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
    if let Err(message) = track.check(to) {
        violations.push(Violation::new(&full, "advance-refused", message));
    }
    let resuming = matches!(track.state, Phase::Blocked(_)) && track.resting() == to;
    let (required, optional) = if resuming {
        (&[][..], &[][..])
    } else {
        evidence_for(to)
    };
    for flag in required {
        if !flag.given(given) {
            violations.push(Violation::new(
                &full,
                "evidence-missing",
                format!("{to} needs {}", flag.name()),
            ));
        }
    }
    for flag in [Flag::Sha, Flag::Pr, Flag::Tag, Flag::Digest] {
        if flag.given(given) && !required.contains(&flag) && !optional.contains(&flag) {
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
    let needs_repo = given.sha.is_some() || given.tag.is_some();
    if needs_repo && repos.is_empty() {
        violations.push(Violation::new(
            &full,
            "evidence-repo",
            "no repository to resolve --sha or --tag in; pass --repo",
        ));
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
                        "`{sha}` is not a commit in {}; fetch first if it was made elsewhere",
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
                format!("tag `{tag}` does not resolve; `git fetch --tags` first if CI made it"),
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
