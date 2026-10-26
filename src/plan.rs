use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::forge::{GithubRepo, MergeableState, TagName};
use crate::limits;
use crate::lint::unit_dirs;
use crate::outcome::{Outcome, Violation};
use crate::state::{After, Finish, Lane, Phase, Release, Step, Track, UnitState};
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
    pub finish: Finish,
    pub stale: Vec<String>,
}

impl Gate {
    pub fn needs(&self) -> Phase {
        self.after.needs(self.finish)
    }

    pub fn met(&self) -> bool {
        self.stale.is_empty()
            && self
                .track
                .as_ref()
                .is_some_and(|track| self.after.met_by(track, self.finish))
    }

    pub fn describe(&self) -> String {
        let state = self.track.as_ref().map_or_else(
            || "not declared".to_owned(),
            |track| track.state.to_string(),
        );
        let stale = if self.stale.is_empty() {
            String::new()
        } else {
            format!("; its pinned evidence is stale: {}", self.stale.join(", "))
        };
        format!("{} (needs {}, is {state}{stale})", self.after, self.needs())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "after": self.after.to_string(),
            "needs": self.needs().to_string(),
            "state": self.track.as_ref().map(|track| track.state.to_string()),
            "met": self.met(),
            "evidence": self.track.as_ref().map(|track| serde_json::to_value(&track.evidence).unwrap_or(Value::Null)),
        })
    }
}

pub fn gate(units: &Units, after: &After) -> Gate {
    let target = step(units, &after.target());
    Gate {
        after: after.clone(),
        track: target.map(|step| step.track.clone()),
        finish: target.map_or(Finish::Shipped, Step::finish),
        stale: target
            .map(|step| {
                step.stale_pins()
                    .iter()
                    .map(|pin| pin.file.to_string())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn gates(units: &Units, step: &Step) -> Vec<Gate> {
    step.after.iter().map(|after| gate(units, after)).collect()
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

    fn waiting_json(&self, root: &Path) -> Value {
        let mut value = self.to_json(root);
        value["waits_on"] = json!(self.waits_on());
        value
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
    pub tag: TagName,
    pub repo: Option<GithubRepo>,
    pub sha: Option<String>,
}

pub fn untagged(units: &Units) -> Vec<Tag> {
    let mut out: Vec<Tag> = units
        .iter()
        .flat_map(|(unit, state)| {
            state
                .steps
                .iter()
                .map(move |(name, step)| (format!("{unit}/{name}"), step))
        })
        .filter(|(_, step)| step.track.state == Phase::Merged)
        .filter_map(|(target, step)| {
            let release = Release::of_target(&target)?;
            let repo = step
                .track
                .evidence
                .pr
                .as_deref()
                .and_then(GithubRepo::of_pr);
            if repo
                .as_ref()
                .is_some_and(|repo| !step.ships_from(&release, repo))
            {
                return None;
            }
            Some(Tag {
                tag: step.release_tag(&release),
                release,
                repo,
                sha: step.track.evidence.sha.clone(),
                target,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (a.release.version, &a.release.line).cmp(&(b.release.version, &b.release.line))
    });
    out
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

/// A codex-lane step launched with no PR whose latest attempt is older than `codex_stall_hours`.
pub struct Stall {
    pub target: String,
    /// The attempt's claim stamp, or the launch stamp for a step launched without one.
    pub since: String,
    pub hours: f64,
}

/// Reads a cm stamp (`2026-10-08T07:04Z`) or any RFC 3339 instant.
pub fn instant(text: &str) -> Option<Timestamp> {
    text.parse().ok().or_else(|| {
        text.strip_suffix('Z')
            .and_then(|minute| format!("{minute}:00Z").parse().ok())
    })
}

pub fn stall_hours(root: &Path) -> f64 {
    fs::read_to_string(root.join("LIMITS.toml"))
        .ok()
        .and_then(|text| limits::check(&text).ok())
        .map_or(limits::CODEX_STALL_HOURS, |limits| limits.codex_stall_hours)
}

pub fn stalls(units: &Units, now: Timestamp, limit: f64) -> Vec<Stall> {
    units
        .iter()
        .flat_map(|(unit, state)| {
            state
                .steps
                .iter()
                .map(move |(name, step)| (format!("{unit}/{name}"), step))
        })
        .filter(|(_, step)| {
            step.lane == Some(Lane::Codex)
                && step.track.state == Phase::Launched
                && step.track.evidence.pr.is_none()
        })
        .filter_map(|(target, step)| {
            let since = step
                .attempt
                .as_ref()
                .map_or(&step.track.since, |attempt| &attempt.claimed_at)
                .clone();
            let age = now.duration_since(instant(&since)?).as_secs_f64() / 3600.0;
            (age > limit).then_some(Stall {
                target,
                since,
                hours: (age * 10.0).round() / 10.0,
            })
        })
        .collect()
}

pub fn stall_json(stall: &Stall, limit: f64) -> Value {
    json!({
        "target": stall.target,
        "since": stall.since,
        "hours": stall.hours,
        "limit_hours": limit,
    })
}

pub fn stall_line(stall: &Stall, limit: f64) -> String {
    format!(
        "stalled {}: codex launch with no PR, attempt since {} ({}h > {limit}h)",
        stall.target, stall.since, stall.hours
    )
}

/// What `cm sync` last saw of an open PR, in `.cm/pulls.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Observed {
    pub target: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeable_state: Option<MergeableState>,
}

pub const PULLS: &str = "pulls.json";

pub fn pulls_path(root: &Path) -> PathBuf {
    root.join(store::CM_DIR).join(PULLS)
}

/// The open PRs whose last sync saw `mergeable_state: dirty`, for steps still at that PR.
pub fn dirty(root: &Path, units: &Units) -> Vec<(String, Observed)> {
    let Ok(seen) = store::read_json::<BTreeMap<String, Observed>>(&pulls_path(root)) else {
        return Vec::new();
    };
    seen.into_iter()
        .filter(|(url, observed)| {
            let dirty = match observed.mergeable_state {
                Some(MergeableState::Dirty) => true,
                Some(MergeableState::Other(_)) | None => false,
            };
            dirty
                && step(units, &observed.target).is_some_and(|step| {
                    step.track.state == Phase::PrOpen
                        && step.track.evidence.pr.as_deref() == Some(url.as_str())
                })
        })
        .collect()
}

pub fn dirty_json(url: &str, observed: &Observed) -> Value {
    json!({
        "target": observed.target,
        "pr": url,
        "head": observed.head,
        "mergeable_state": observed.mergeable_state,
        "observed_at": observed.at,
    })
}

pub fn dirty_line(url: &str, observed: &Observed) -> String {
    format!(
        "dirty {}: {url} has merge conflicts (mergeable_state dirty, seen {})",
        observed.target, observed.at
    )
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
        if !paused && candidate.ready() {
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
    next_at(root, target, lane, Timestamp::now())
}

pub fn next_at(
    root: &Path,
    target: Option<&Target>,
    lane: Option<Lane>,
    now: Timestamp,
) -> Result<Outcome> {
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
            outcome.field("steps", vec![candidate.to_json(root)]);
            outcome.field("waiting", Vec::<Value>::new());
        } else {
            outcome.field("steps", Vec::<Value>::new());
            outcome.field("waiting", vec![candidate.waiting_json(root)]);
        }
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
            tag.repo
                .as_ref()
                .map_or_else(|| tag.release.line.clone(), ToString::to_string),
            tag.tag,
            tag.sha.as_deref().unwrap_or("—")
        ));
    }
    if ready.is_empty() && waiting.is_empty() && tags.is_empty() {
        outcome.line("nothing is declared and drafted");
    }
    let limit = stall_hours(root);
    let stalled = stalls(&units, now, limit);
    let dirty = dirty(root, &units);
    for stall in &stalled {
        outcome.line(stall_line(stall, limit));
    }
    for (url, observed) in &dirty {
        outcome.line(dirty_line(url, observed));
    }
    let held = crate::holds::held(&units, now);
    outcome.lines.extend(held.lines);
    outcome.field("watches", held.watches);
    outcome.field("stale", held.stale);
    outcome.field(
        "stalled",
        stalled
            .iter()
            .map(|stall| stall_json(stall, limit))
            .collect::<Vec<_>>(),
    );
    outcome.field(
        "dirty",
        dirty
            .iter()
            .map(|(url, observed)| dirty_json(url, observed))
            .collect::<Vec<_>>(),
    );
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
            .map(|candidate| candidate.waiting_json(root))
            .collect::<Vec<_>>(),
    );
    outcome.field(
        "tags",
        tags.iter()
            .map(|tag| {
                json!({
                    "target": tag.target,
                    "repo": tag.repo.as_ref().map(ToString::to_string),
                    "version": tag.release.version.to_string(),
                    "tags": [tag.tag.to_string()],
                    "sha": tag.sha,
                })
            })
            .collect::<Vec<_>>(),
    );
    Ok(outcome)
}
