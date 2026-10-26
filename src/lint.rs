use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::journal;
use crate::markdown::{code_spans, first_difference, named_path};
use crate::outcome::{Outcome, Violation};
use crate::render;
use crate::state::{CampaignState, Phase, Track, UnitState};
use crate::store::{
    self, JOURNAL, UNIT_STATE, find_root, is_name, is_subcampaign_name, relative, state_path,
};
use crate::templates::{self, Banner, QUEUE_FILE, ROOT_FILES};

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn packet_files(step: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(step) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name == "kickoff.md"
                            || (name.ends_with(".md")
                                && (name.starts_with("step-") || name.starts_with("resume")))
                    })
        })
        .collect();
    files.sort();
    files
}

pub fn packet_paths(
    root: &Path,
    files: &[PathBuf],
    home: Option<&Path>,
) -> (usize, Vec<Violation>) {
    let repo = root.ancestors().find(|dir| dir.join(".git").exists());
    let mut checked = 0;
    let mut out = Vec::new();
    for file in files {
        let Ok(text) = fs::read_to_string(file) else {
            out.push(Violation::new(
                relative(root, file),
                "packet-unreadable",
                "cannot read this packet file",
            ));
            continue;
        };
        let base = file.parent().unwrap_or(root);
        for (line, span) in code_spans(&text) {
            let Some(candidates) = packet_targets(span, home, repo, root, base) else {
                continue;
            };
            checked += 1;
            if !candidates.iter().any(|path| path.exists()) {
                out.push(
                    Violation::new(
                        relative(root, file),
                        "packet-path",
                        format!("`{span}` does not exist"),
                    )
                    .at(line),
                );
            }
        }
    }
    (checked, out)
}

fn has_extension(span: &str) -> bool {
    span.rsplit('/')
        .next()
        .and_then(|leaf| leaf.rsplit_once('.'))
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.bytes().any(|b| b.is_ascii_alphabetic()))
}

/// `01-a/u/S1` or `00-release/cm/0.5.0` names a unit or step, not a file; `01-a/u/S1.md` does.
fn is_target_id(span: &str) -> bool {
    !has_extension(span) && store::parse_target(span, false).is_ok()
}

/// A route such as `/v1/sessions/abc` names no file: no extension, and its first directory is
/// not on this machine. `/var/lib/x` and `/definitely/not/here.md` are still checked.
fn is_route(path: &Path, span: &str) -> bool {
    !has_extension(span)
        && path
            .components()
            .nth(1)
            .is_some_and(|first| !Path::new("/").join(first).is_dir())
}

fn packet_targets(
    span: &str,
    home: Option<&Path>,
    repo: Option<&Path>,
    campaign: &Path,
    base: &Path,
) -> Option<Vec<PathBuf>> {
    if let Some(path) = named_path(span, home) {
        if span.starts_with('/') && is_route(&path, span) {
            return None;
        }
        let mut targets = vec![path.clone()];
        if let Some(repo) = repo.filter(|_| span.starts_with('/'))
            && let Some(name) = repo.file_name()
        {
            let parts: Vec<_> = path.components().collect();
            if let Some(at) = parts.iter().rposition(|part| part.as_os_str() == name) {
                targets.push(
                    parts[at + 1..]
                        .iter()
                        .fold(repo.to_path_buf(), |acc, part| acc.join(part)),
                );
            }
        }
        return Some(targets);
    }
    if span.starts_with("./") || span.starts_with("../") {
        let path = named_path(&format!("/{span}"), None)?;
        let rel = path.strip_prefix("/").ok()?;
        return Some(vec![store::absolute(&base.join(rel))]);
    }
    if span.contains("://") || is_target_id(span) {
        return None;
    }
    let rooted = named_path(&format!("/{span}"), None)?;
    let rel = rooted.strip_prefix("/").ok()?;
    let first = rel.components().next()?.as_os_str().to_str()?;
    if first.starts_with('.') {
        return None;
    }
    let targets: Vec<PathBuf> = repo
        .into_iter()
        .chain([campaign])
        .filter(|dir| dir.join(first).is_dir())
        .map(|dir| dir.join(rel))
        .collect();
    (!targets.is_empty()).then_some(targets)
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn name_of(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
}

pub fn unit_dirs(root: &Path) -> Vec<PathBuf> {
    subdirs(root)
        .into_iter()
        .filter(|dir| is_subcampaign_name(name_of(dir)))
        .flat_map(|dir| subdirs(&dir))
        .filter(|dir| is_name(name_of(dir)) && state_path(dir).is_file())
        .collect()
}

/// A unit folder holding STATE.md but no `.cm/state.json` lost its state; lint never counts it as
/// a smaller campaign.
pub fn orphan_units(root: &Path) -> Vec<Violation> {
    subdirs(root)
        .into_iter()
        .filter(|dir| is_subcampaign_name(name_of(dir)))
        .flat_map(|dir| subdirs(&dir))
        .filter(|dir| dir.join(UNIT_STATE).is_file() && !state_path(dir).is_file())
        .map(|dir| {
            Violation::new(
                relative(root, &state_path(&dir)),
                "unit-state-json-missing",
                format!(
                    "{} holds STATE.md but no .cm/state.json; restore it from git (`git checkout HEAD -- {}`)",
                    relative(root, &dir),
                    relative(root, &state_path(&dir))
                ),
            )
        })
        .collect()
}

/// Unit and step folders git ignores never reach the campaign's repository.
pub fn ignored(root: &Path, units: &[PathBuf]) -> Vec<Violation> {
    let dirs: Vec<String> = units
        .iter()
        .flat_map(|unit| std::iter::once(unit.clone()).chain(subdirs(unit)))
        .filter(|dir| name_of(dir) != store::CM_DIR)
        .map(|dir| format!("{}/", relative(root, &dir)))
        .collect();
    if dirs.is_empty() {
        return Vec::new();
    }
    let unknown = |why: String| {
        vec![Violation::new(
            ".",
            "git-ignored-unknown",
            format!(
                "cannot tell which unit or step folders git ignores: {why}; the campaign belongs in a git repository"
            ),
        )]
    };
    let output = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--no-index", "--"])
        .args(&dirs)
        .output()
    {
        Ok(output) => output,
        Err(error) => return unknown(format!("git did not run ({error})")),
    };
    match output.status.code() {
        Some(0 | 1) => {}
        Some(code) => return unknown(format!("`git check-ignore` exited {code}")),
        None => return unknown("`git check-ignore` was killed".to_owned()),
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|path| {
            Violation::new(
                path.trim_end_matches('/'),
                "git-ignored",
                "git ignores this folder, so its files never reach the campaign's repository; narrow the .gitignore pattern",
            )
        })
        .collect()
}

pub fn unit_state_file(root: &Path, dir: &Path, unit: &UnitState) -> Vec<Violation> {
    let path = dir.join(UNIT_STATE);
    let rendered = render::unit(unit);
    match fs::read_to_string(&path) {
        Err(_) => vec![Violation::new(
            relative(root, &path),
            "unit-state-missing",
            format!("STATE.md is gone; re-render it with `cm init {}`", unit.unit),
        )],
        Ok(text) if text == rendered => Vec::new(),
        Ok(text) => vec![
            Violation::new(
                relative(root, &path),
                "unit-state-hand-edit",
                format!(
                    "STATE.md differs from the render of .cm/state.json; reset it with `rm {} && cm init {}`, then change state with `cm advance`",
                    relative(root, &path),
                    unit.unit
                ),
            )
            .at(first_difference(&text, &rendered)),
        ],
    }
}

struct Seen<'a> {
    home: Option<&'a Path>,
    sealed: bool,
    packets: usize,
    warnings: Vec<String>,
    now: jiff::Timestamp,
}

fn lint_holds(root: &Path, unit: &UnitState, seen: &mut Seen, out: &mut Vec<Violation>) {
    for (name, step) in &unit.steps {
        let target = format!("{}/{name}", unit.unit);
        for pin in &step.pins {
            if let Err(error) = pin.file.check(root) {
                out.push(Violation::new(
                    &target,
                    "evidence-file",
                    format!("its pinned evidence {} no longer holds: {error}", pin.file),
                ));
            }
        }
        for pin in step.stale_pins() {
            out.push(Violation::new(
                &target,
                "evidence-stale",
                format!(
                    "{} was pinned to {}{} and is stale: {}; write fresh evidence pinned to the new head",
                    pin.file,
                    pin.candidate,
                    pin.harness
                        .as_ref()
                        .map(|harness| format!(" under {harness}"))
                        .unwrap_or_default(),
                    pin.stale.as_deref().unwrap_or_default()
                ),
            ));
        }
        for watch in step.unresolved_watches() {
            match watch.status(seen.now) {
                crate::state::WatchStatus::Expired => seen.warnings.push(format!(
                    "{target}#{}: expired: unresolved ({}, deadline {}); it holds the step below deployed",
                    watch.id, watch.signal, watch.deadline
                )),
                crate::state::WatchStatus::Open | crate::state::WatchStatus::Resolved => {}
            }
        }
        for waived in &step.waived {
            let named = step.known_red.iter().any(|exception| {
                crate::state::same_commit(&exception.commit, &waived.commit)
                    && exception.check == waived.check
            });
            if !named {
                seen.warnings.push(format!(
                    "{target}: --override {} waived red check `{}` at {} with no known-red exception; record one with `cm known-red`",
                    waived.file, waived.check, waived.commit
                ));
            }
        }
    }
}

fn lint_unit(root: &Path, dir: &Path, seen: &mut Seen) -> Vec<Violation> {
    let (home, sealed) = (seen.home, seen.sealed);
    let state_rel = relative(root, &state_path(dir));
    let unit = match store::load_unit(dir) {
        Ok(unit) => unit,
        Err(error) => {
            return vec![Violation::new(
                state_rel,
                "state-parse",
                format!("{error:#}"),
            )];
        }
    };
    let mut out = Vec::new();
    let expected = relative(root, dir);
    if unit.unit != expected {
        out.push(Violation::new(
            state_rel,
            "unit-name",
            format!(
                "the state names unit `{}` but lives in `{expected}`",
                unit.unit
            ),
        ));
    }
    out.extend(unit_state_file(root, dir, &unit));
    lint_holds(root, &unit, seen, &mut out);
    let tracks = std::iter::once((expected.clone(), &unit.track)).chain(
        unit.steps
            .iter()
            .map(|(name, step)| (format!("{expected}/{name}"), &step.track)),
    );
    for (target, track) in tracks {
        if let Some(file) = &track.evidence.file
            && let Err(error) = file.check(root)
        {
            out.push(Violation::new(
                target,
                "evidence-file",
                format!("its recorded evidence {file} no longer holds: {error}"),
            ));
        }
    }
    for step_dir in subdirs(dir) {
        let name = name_of(&step_dir);
        let files = packet_files(&step_dir);
        if files.is_empty() {
            continue;
        }
        let live = match unit.steps.get(name) {
            _ if sealed => false,
            None => true,
            Some(step) => matches!(step.track.resting(), Phase::Drafted | Phase::Launched),
        };
        if live {
            seen.packets += 1;
            out.extend(packet_paths(root, &files, home).1);
        }
    }
    for (name, step) in &unit.steps {
        if *step.track.resting() == Phase::Launched && packet_files(&dir.join(name)).is_empty() {
            out.push(Violation::new(
                relative(root, &dir.join(name)),
                "packet-missing",
                format!("step `{name}` is launched but its folder holds no kickoff.md, step-*.md or resume*.md"),
            ));
        }
    }
    out
}

pub fn tracked_runtime(root: &Path) -> Vec<Violation> {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--", ".cm/JOURNAL.md", ".cm/lock"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|path| {
            Violation::new(
                path,
                "cm-tracked",
                format!(
                    "cm keeps no journal copy and `.cm/lock` is a runtime lock; untrack it with `git rm --cached {path}`"
                ),
            )
        })
        .collect()
}

pub fn root_files(root: &Path, state: Option<&CampaignState>) -> Vec<Violation> {
    let mut out = tracked_runtime(root);
    let closed = state.is_some_and(|state| state.closed.is_some());
    let banner = Banner::of(root, closed);
    for (name, _) in ROOT_FILES {
        if name == JOURNAL {
            continue;
        }
        match fs::read_to_string(root.join(name)) {
            Ok(text) => out.extend(templates::conform(name, &text, banner)),
            Err(_) => out.push(Violation::new(
                name,
                "missing-file",
                "run `cm init` to write it",
            )),
        }
    }
    if closed {
        out.extend(unsealed(root));
    }
    match state {
        Some(state) => out.extend(journal::check(root, state)),
        None => {
            if let Ok(text) = fs::read_to_string(root.join(JOURNAL)) {
                out.extend(templates::journal_title(&text));
            }
        }
    }
    out
}

fn open_track(out: &mut Vec<Violation>, target: &str, track: &Track) {
    if !matches!(
        track.state,
        Phase::Done | Phase::Superseded | Phase::Blocked(_)
    ) {
        out.push(Violation::new(
            target,
            "closed-track",
            format!(
                "{target} is {}; a closed campaign holds only done or blocked tracks: `cm advance {target} done` or `blocked:<reason>`",
                track.state
            ),
        ));
    }
}

pub fn unsealed(root: &Path) -> Vec<Violation> {
    let mut out = Vec::new();
    if templates::queue_open_in(root) {
        out.push(Violation::new(
            QUEUE_FILE,
            "closed-queue",
            "OWNER-QUEUE.md has an open item; answer it or move it to the successor before the campaign closes",
        ));
    }
    for dir in unit_dirs(root) {
        let Ok(unit) = store::load_unit(&dir) else {
            continue;
        };
        let target = relative(root, &dir);
        open_track(&mut out, &target, &unit.track);
        for (name, step) in &unit.steps {
            open_track(&mut out, &format!("{target}/{name}"), &step.track);
        }
    }
    out
}

pub fn lint(start: &Path) -> Result<Outcome> {
    lint_at(start, jiff::Timestamp::now())
}

pub fn lint_at(start: &Path, now: jiff::Timestamp) -> Result<Outcome> {
    let Some(root) = find_root(start) else {
        return Ok(Outcome::refused(vec![Violation::new(
            store::absolute(start).to_string_lossy(),
            "no-campaign",
            "no .cm/state.json at or above this folder; run `cm init`",
        )]));
    };
    let home = home();
    let mut violations = Vec::new();
    let state = match store::load_campaign(&root) {
        Ok(state) => Some(state),
        Err(error) => {
            violations.push(Violation::new(
                relative(&root, &state_path(&root)),
                "state-parse",
                format!("{error:#}"),
            ));
            None
        }
    };
    violations.extend(root_files(&root, state.as_ref()));
    let units = unit_dirs(&root);
    violations.extend(crate::ledger::lint_feedback(&root));
    violations.extend(crate::review::lint_reviews(&root));
    violations.extend(orphan_units(&root));
    violations.extend(ignored(&root, &units));
    let mut seen = Seen {
        home: home.as_deref(),
        sealed: state.as_ref().is_some_and(|state| state.closed.is_some()),
        packets: 0,
        warnings: Vec::new(),
        now,
    };
    for dir in &units {
        violations.extend(lint_unit(&root, dir, &mut seen));
    }
    let packets = seen.packets;
    let mut outcome = Outcome::refused(violations);
    outcome.field("root", root.to_string_lossy().into_owned());
    outcome.field("units", units.len());
    outcome.field("packets", packets);
    for warning in &seen.warnings {
        outcome.line(format!("warning: {warning}"));
    }
    outcome.field("warnings", seen.warnings);
    let summary = if outcome.ok() {
        format!(
            "clean: {} ({} unit(s), {} live packet(s))",
            root.display(),
            units.len(),
            packets
        )
    } else {
        format!("{} violation(s)", outcome.violations.len())
    };
    outcome.line(summary);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_paths_resolve_against_the_repository() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("trail");
        let root = repo.join("store/epochs/e");
        let step = root.join("01-a/u/S1");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&step).unwrap();
        fs::write(step.join("brief.md"), "x").unwrap();
        let packet = step.join("kickoff.md");
        let check = |span: &str| {
            fs::write(&packet, format!("see `{span}`\n")).unwrap();
            let (checked, missing) = packet_paths(&root, std::slice::from_ref(&packet), None);
            assert_eq!(checked, 1, "{span}");
            missing.is_empty()
        };
        assert!(check("store/epochs/e/01-a/u/S1/brief.md"));
        assert!(!check("store/epochs/e/01-a/u/S1/NOPE.md"));
        assert!(check(
            "/home/other/code/trail/store/epochs/e/01-a/u/S1/brief.md:3"
        ));
        assert!(check(&step.join("brief.md").to_string_lossy()));
        assert!(!check("/home/other/code/trail/store/epochs/e/NOPE.md"));
        assert!(!check("/home/other/elsewhere/x.md"));
        assert!(check("../S1/brief.md"));
        assert!(check("./brief.md:4"));
        assert!(!check("../S1/NOPE.md"));
        assert!(check("01-a/u/S1/brief.md"));
        assert!(!check("01-a/u/NOPE.md"));
        fs::write(&packet, "`other/x.rs` `.git/x` `crate::a/b`\n").unwrap();
        assert_eq!(
            packet_paths(&root, std::slice::from_ref(&packet), None).0,
            0
        );
    }
}
