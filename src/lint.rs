use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::journal;
use crate::markdown::{code_spans, first_difference, named_path};
use crate::outcome::{Outcome, Violation};
use crate::render;
use crate::state::{CampaignState, Phase, UnitState};
use crate::store::{
    self, JOURNAL, UNIT_STATE, find_root, is_name, is_subcampaign_name, relative, state_path,
};
use crate::templates::{self, ROOT_FILES};

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
        for (line, span) in code_spans(&text) {
            let Some(path) = named_path(span, home) else {
                continue;
            };
            checked += 1;
            if !path.exists() {
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

fn lint_unit(root: &Path, dir: &Path, home: Option<&Path>, packets: &mut usize) -> Vec<Violation> {
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
    for step_dir in subdirs(dir) {
        let name = name_of(&step_dir);
        let files = packet_files(&step_dir);
        if files.is_empty() {
            continue;
        }
        let live = match unit.steps.get(name) {
            None => true,
            Some(step) => matches!(step.track.resting(), Phase::Drafted | Phase::Launched),
        };
        if live {
            *packets += 1;
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

pub fn root_files(root: &Path, state: Option<&CampaignState>) -> Vec<Violation> {
    let mut out = Vec::new();
    for (name, _) in ROOT_FILES {
        if name == JOURNAL {
            continue;
        }
        match fs::read_to_string(root.join(name)) {
            Ok(text) => out.extend(templates::conform(name, &text)),
            Err(_) => out.push(Violation::new(
                name,
                "missing-file",
                "run `cm init` to write it",
            )),
        }
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

pub fn lint(start: &Path) -> Result<Outcome> {
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
    let mut packets = 0;
    for dir in &units {
        violations.extend(lint_unit(&root, dir, home.as_deref(), &mut packets));
    }
    let mut outcome = Outcome::refused(violations);
    outcome.field("root", root.to_string_lossy().into_owned());
    outcome.field("units", units.len());
    outcome.field("packets", packets);
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
