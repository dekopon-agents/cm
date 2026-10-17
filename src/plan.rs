use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use crate::limits;
use crate::lint::unit_dirs;
use crate::outcome::{Outcome, Violation};
use crate::state::{After, Lane, Phase, Release, Step, Track, UnitState};
use crate::store::{self, Target, relative, state_path};

pub type Units = BTreeMap<String, UnitState>;

pub fn load_units(root: &Path) -> Result<Units> {
    unit_dirs(root)
        .into_iter()
        .map(|dir| Ok((relative(root, &dir), store::load_unit(&dir)?)))
        .collect()
}

pub fn split(target: &str) -> Option<(&str, &str)> {
    target.rsplit_once('/')
}

pub fn step<'a>(units: &'a Units, target: &str) -> Option<&'a Step> {
    let (unit, name) = split(target)?;
    units.get(unit)?.steps.get(name)
}

pub struct Gate {
    pub after: After,
    pub track: Option<Track>,
}

impl Gate {
    pub fn met(&self) -> bool {
        self.track
            .as_ref()
            .is_some_and(|track| self.after.met_by(track))
    }

    pub fn describe(&self) -> String {
        let state = self.track.as_ref().map_or_else(
            || "not declared".to_owned(),
            |track| track.state.to_string(),
        );
        format!("{} (needs {}, is {state})", self.after, self.after.needs())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "after": self.after.to_string(),
            "needs": self.after.needs().to_string(),
            "state": self.track.as_ref().map(|track| track.state.to_string()),
            "met": self.met(),
            "evidence": self.track.as_ref().map(|track| serde_json::to_value(&track.evidence).unwrap_or(Value::Null)),
        })
    }
}

pub fn gates(units: &Units, step: &Step) -> Vec<Gate> {
    step.after
        .iter()
        .map(|after| Gate {
            after: after.clone(),
            track: self::step(units, &after.target()).map(|step| step.track.clone()),
        })
        .collect()
}

pub fn unmet(target: &str, gates: &[Gate]) -> Vec<Violation> {
    gates
        .iter()
        .filter(|gate| !gate.met())
        .map(|gate| {
            Violation::new(
                target,
                "after-unmet",
                format!("{target} waits on {}", gate.describe()),
            )
        })
        .collect()
}

fn kickoff(root: &Path, target: &str) -> Option<PathBuf> {
    let path = root.join(target).join("kickoff.md");
    path.is_file().then_some(path)
}

struct Candidate<'a> {
    target: String,
    step: &'a Step,
    gates: Vec<Gate>,
    kickoff: Option<PathBuf>,
}

impl Candidate<'_> {
    fn ready(&self) -> bool {
        self.kickoff.is_some() && self.gates.iter().all(Gate::met)
    }

    fn lane(&self) -> Option<Lane> {
        self.step.lane
    }

    fn waits_on(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .gates
            .iter()
            .filter(|gate| !gate.met())
            .map(Gate::describe)
            .collect();
        if self.kickoff.is_none() {
            out.push(format!("{}/kickoff.md (not written)", self.target));
        }
        out
    }

    fn to_json(&self, root: &Path) -> Value {
        json!({
            "target": self.target,
            "lane": self.lane().map(Lane::name),
            "dir": self.target,
            "kickoff": self.kickoff.as_ref().map(|path| relative(root, path)),
            "after": self.gates.iter().map(Gate::to_json).collect::<Vec<_>>(),
        })
    }
}

fn candidates<'a>(root: &Path, units: &'a Units) -> Vec<Candidate<'a>> {
    units
        .iter()
        .flat_map(|(unit, state)| {
            state
                .steps
                .iter()
                .map(move |(name, step)| (format!("{unit}/{name}"), step))
        })
        .filter(|(_, step)| step.lane.is_some() && step.track.state == Phase::Drafted)
        .map(|(target, step)| Candidate {
            kickoff: kickoff(root, &target),
            gates: gates(units, step),
            target,
            step,
        })
        .collect()
}

pub struct Tag {
    pub target: String,
    pub release: Release,
    pub repo: Option<String>,
    pub sha: Option<String>,
}

pub fn untagged(units: &Units) -> Vec<Tag> {
    units
        .iter()
        .flat_map(|(unit, state)| {
            state
                .steps
                .iter()
                .map(move |(name, step)| (format!("{unit}/{name}"), step))
        })
        .filter(|(_, step)| step.track.state == Phase::Merged)
        .filter_map(|(target, step)| {
            Some(Tag {
                release: Release::of_target(&target)?,
                repo: step.track.evidence.pr.as_deref().and_then(repo_of_pr),
                sha: step.track.evidence.sha.clone(),
                target,
            })
        })
        .collect()
}

pub fn repo_of_pr(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    (parts.next() == Some("pull")).then(|| format!("{owner}/{repo}"))
}

pub fn paused(root: &Path) -> Result<bool, Vec<Violation>> {
    match fs::read_to_string(root.join("LIMITS.toml")) {
        Err(_) => Err(vec![Violation::new(
            "LIMITS.toml",
            "missing-file",
            "run `cm init` to write it",
        )]),
        Ok(text) => limits::check(&text).map(|limits| limits.paused),
    }
}

pub struct Next {
    pub paused: bool,
    pub ready: Vec<(String, Option<Lane>)>,
    pub waiting: Vec<(String, Vec<String>)>,
}

pub fn summary(root: &Path, units: &Units) -> Next {
    let paused = paused(root).unwrap_or(true);
    let mut ready = Vec::new();
    let mut waiting = Vec::new();
    for candidate in candidates(root, units) {
        if candidate.ready() {
            ready.push((candidate.target.clone(), candidate.lane()));
        } else {
            waiting.push((candidate.target.clone(), candidate.waits_on()));
        }
    }
    Next {
        paused,
        ready,
        waiting,
    }
}

pub fn next(root: &Path, target: Option<&Target>, lane: Option<Lane>) -> Result<Outcome> {
    let campaign = store::load_campaign(root)?;
    if let Some(at) = &campaign.closed {
        return Ok(Outcome::refused(vec![Violation::new(
            relative(root, &state_path(root)),
            "campaign-closed",
            format!("the campaign closed at {at}; nothing is next"),
        )]));
    }
    let paused = match paused(root) {
        Ok(paused) => paused,
        Err(violations) => return Ok(Outcome::refused(violations)),
    };
    let units = load_units(root)?;
    let all = candidates(root, &units);
    let mut outcome = Outcome::default();
    outcome.field("paused", paused);
    if let Some(target) = target {
        let full = format!(
            "{}/{}",
            target.unit,
            target.step.as_deref().unwrap_or_default()
        );
        let Some(candidate) = all.iter().find(|candidate| candidate.target == full) else {
            let message = match step(&units, &full) {
                Some(step) if step.lane.is_none() => format!(
                    "{full} was launched without a declaration; only steps declared with `cm init {full} --lane …` are handed out"
                ),
                Some(step) => format!(
                    "{full} is {}; only a drafted step is next",
                    step.track.state
                ),
                None => format!(
                    "{full} is not declared; declare it with `cm init {full} --lane <lane> [--after …]`"
                ),
            };
            return Ok(Outcome::refused(vec![Violation::new(
                &full,
                "next-state",
                message,
            )]));
        };
        if lane.is_some_and(|lane| candidate.lane() != Some(lane)) {
            outcome.violations.push(Violation::new(
                &full,
                "next-lane",
                format!(
                    "{full} runs in lane {}",
                    candidate.lane().map_or("—", Lane::name)
                ),
            ));
        }
        outcome.violations.extend(unmet(&full, &candidate.gates));
        if candidate.kickoff.is_none() {
            outcome.violations.push(Violation::new(
                &full,
                "packet-kickoff",
                "the step folder has no kickoff.md",
            ));
        }
        if paused {
            outcome.violations.push(Violation::new(
                "LIMITS.toml",
                "limits-paused",
                "`paused = true`: nothing launches",
            ));
        }
        if outcome.ok() {
            outcome.line(format!(
                "{full} ({}) is next",
                candidate.lane().map_or("—", Lane::name)
            ));
        }
        outcome.field("steps", vec![candidate.to_json(root)]);
        return Ok(outcome);
    }
    let in_lane = |candidate: &&Candidate| lane.is_none_or(|lane| candidate.lane() == Some(lane));
    let ready: Vec<&Candidate> = all
        .iter()
        .filter(in_lane)
        .filter(|candidate| !paused && candidate.ready())
        .collect();
    let waiting: Vec<&Candidate> = all
        .iter()
        .filter(in_lane)
        .filter(|candidate| paused || !candidate.ready())
        .collect();
    if paused {
        outcome.line("paused: nothing is next while LIMITS.toml has `paused = true`");
    }
    for candidate in &ready {
        outcome.line(format!(
            "next {} ({})",
            candidate.target,
            candidate.lane().map_or("—", Lane::name)
        ));
    }
    for candidate in &waiting {
        let on = candidate.waits_on();
        if !on.is_empty() {
            outcome.line(format!("waits {}: {}", candidate.target, on.join("; ")));
        }
    }
    let tags = untagged(&units);
    for tag in &tags {
        outcome.line(format!(
            "tag {} {} at {}",
            tag.repo.as_deref().unwrap_or(&tag.release.repo),
            tag.release.version,
            tag.sha.as_deref().unwrap_or("—")
        ));
    }
    if ready.is_empty() && waiting.is_empty() && tags.is_empty() {
        outcome.line("nothing is declared and drafted");
    }
    outcome.field(
        "steps",
        ready
            .iter()
            .map(|candidate| candidate.to_json(root))
            .collect::<Vec<_>>(),
    );
    outcome.field(
        "waiting",
        waiting
            .iter()
            .map(|candidate| {
                let mut value = candidate.to_json(root);
                value["waits_on"] = json!(candidate.waits_on());
                value
            })
            .collect::<Vec<_>>(),
    );
    outcome.field(
        "tags",
        tags.iter()
            .map(|tag| {
                json!({
                    "target": tag.target,
                    "repo": tag.repo,
                    "version": tag.release.version.to_string(),
                    "tags": tag.release.tag_names(),
                    "sha": tag.sha,
                })
            })
            .collect::<Vec<_>>(),
    );
    Ok(outcome)
}
