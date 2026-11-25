use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::attempt::{Drivers, remote_turn, turn_began};
use crate::commands::shell_word;
use crate::forge::{GithubRepo, MergeableState, TagName};
use crate::limits;
use crate::lint::unit_dirs;
use crate::outcome::{Outcome, Violation};
use crate::render::next_item;
use crate::state::{After, Attempt, Finish, Lane, Phase, Release, Step, Track, UnitState};
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

pub fn track<'a>(units: &'a Units, target: &str) -> Option<&'a Track> {
    match target.split_once('#') {
        Some((name, part)) => step(units, name)?.parts.get(part),
        None => step(units, target).map(|step| &step.track),
    }
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

    pub(crate) fn pin_unmet(&self) -> bool {
        matches!(self.after, After::Pin(_))
            && !self
                .track
                .as_ref()
                .is_some_and(|track| self.after.met_by(track, self.finish))
    }

    pub fn describe(&self) -> String {
        let stale = if self.stale.is_empty() {
            String::new()
        } else {
            format!("; its pinned evidence is stale: {}", self.stale.join(", "))
        };
        if let After::Pin(_) = &self.after {
            if self
                .track
                .as_ref()
                .is_some_and(|track| track.resting() == &Phase::Superseded)
            {
                return format!("{} (stale pin; step superseded{stale})", self.after);
            }
            let Some(head) = self
                .track
                .as_ref()
                .and_then(|track| track.evidence.sha.as_deref())
            else {
                return format!("{} (head not recorded{stale})", self.after);
            };
            if self.pin_unmet() {
                return format!("{} (stale pin; recorded head {head}{stale})", self.after);
            }
            return format!("{} (recorded head {head}{stale})", self.after);
        }
        let state = self.track.as_ref().map_or_else(
            || "not declared".to_owned(),
            |track| track.state.to_string(),
        );
        format!("{} (needs {}, is {state}{stale})", self.after, self.needs())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "after": self.after.to_string(),
            "needs": match &self.after {
                After::Pin(revision) => format!("head {}", revision.sha),
                _ => self.needs().to_string(),
            },
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
        finish: target.map_or(Finish::Shipped, |step| {
            step.finish(Release::of_target(&after.target()).is_some())
        }),
        stale: target
            .map(|step| {
                stale_evidence(units, step)
                    .iter()
                    .map(|pin| pin.file.to_string())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The stale pins a step stands on, one per file: for each file it pinned or finished on, a stale
/// pin of that file, anywhere in the campaign, that no fresh pin of the step renews.
pub fn stale_evidence<'a>(units: &'a Units, step: &Step) -> Vec<&'a crate::state::Pin> {
    let mut files: Vec<&crate::evidence::CampaignFile> =
        step.pins.iter().map(|pin| &pin.file).collect();
    files.extend(&step.track.evidence.file);
    files.extend(
        step.parts
            .values()
            .filter_map(|part| part.evidence.file.as_ref()),
    );
    files.dedup();
    let mut out: Vec<&crate::state::Pin> = Vec::new();
    for file in files {
        if out.iter().any(|pin| pin.file == *file) {
            continue;
        }
        if let Some(pin) = units
            .values()
            .flat_map(|unit| unit.steps.values())
            .flat_map(|holder| holder.pins.iter())
            .find(|pin| {
                pin.file == *file
                    && pin.stale.is_some()
                    && !step.pins.iter().any(|fresh| fresh.renews(pin))
            })
        {
            out.push(pin);
        }
    }
    out
}

pub fn gates(root: &Path, units: &Units, step: &Step) -> Vec<Gate> {
    step.after
        .iter()
        .map(|after| {
            let mut gate = gate(units, after);
            if let After::Pin(revision) = after
                && let Some(track) = &mut gate.track
            {
                track.evidence.sha = self::step(units, &revision.step)
                    .filter(|step| step.parts.is_empty())
                    .and_then(|step| {
                        if step.track.resting() == &Phase::PrOpen {
                            crate::holds::head(root, units, &revision.step)
                        } else {
                            step.track.evidence.sha.clone()
                        }
                    });
            }
            gate
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

pub(crate) fn pin_warning_applies(step: &Step) -> bool {
    matches!(step.track.state, Phase::Launched | Phase::PrOpen)
}

struct Candidate<'a> {
    target: String,
    step: &'a Step,
    gates: Vec<Gate>,
    kickoff: Option<PathBuf>,
}

impl Candidate<'_> {
    fn ready(&self) -> bool {
        self.step.track.state == Phase::Drafted
            && self.kickoff.is_some()
            && self.gates.iter().all(Gate::met)
    }

    fn warns_pin(&self) -> bool {
        pin_warning_applies(self.step) && self.gates.iter().any(Gate::pin_unmet)
    }

    fn lane(&self) -> Option<Lane> {
        self.step.lane
    }

    fn finish(&self) -> Finish {
        self.step.finish(Release::of_target(&self.target).is_some())
    }

    fn waits_on(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .gates
            .iter()
            .filter(|gate| {
                if self.step.track.state == Phase::Drafted {
                    !gate.met()
                } else {
                    gate.pin_unmet()
                }
            })
            .map(Gate::describe)
            .collect();
        if self.step.track.state == Phase::Drafted && self.kickoff.is_none() {
            out.push(format!("{}/kickoff.md (not written)", self.target));
        }
        out
    }

    fn to_json(&self, root: &Path) -> Value {
        json!({
            "target": self.target,
            "command": launch_command(&self.target, self.lane(), self.finish()),
            "lane": self.lane().map(Lane::name),
            "dir": self.target,
            "kickoff": self.kickoff.as_ref().map(|path| relative(root, path)),
            "after": self.gates.iter().map(Gate::to_json).collect::<Vec<_>>(),
        })
    }

    fn dispatch_hold<'a>(&self, quota: Option<&'a str>) -> Option<&'a str> {
        quota.filter(|_| self.lane() == Some(Lane::Codex) && self.ready())
    }

    fn waiting_json(&self, root: &Path, quota: Option<&str>) -> Value {
        let mut value = self.to_json(root);
        let mut on = self.waits_on();
        on.extend(self.dispatch_hold(quota).map(str::to_owned));
        value["waits_on"] = json!(on);
        value["command"] = json!(waiting_command(root, &self.target));
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
        .filter(|(_, step)| step.lane.is_some_and(|lane| lane != Lane::Mac))
        .map(|(target, step)| Candidate {
            kickoff: kickoff(root, &target),
            gates: gates(root, units, step),
            target,
            step,
        })
        .filter(|candidate| candidate.step.track.state == Phase::Drafted || candidate.warns_pin())
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

pub(crate) struct ActionInputs {
    now: Timestamp,
    coordinator: Option<String>,
    stall_limits: limits::StallLimits,
    turn_commands: limits::TurnCommands,
    drivers: Drivers,
}

impl ActionInputs {
    // Sync does not refuse an unreadable registry, as it does not refuse an unreadable review
    // ledger: it registers no driver, so every claim reads as it would without one.
    pub(crate) fn read(root: &Path, now: Timestamp) -> Self {
        Self::with_drivers(root, now, Drivers::read(root).unwrap_or_default())
    }

    pub(crate) fn load(root: &Path, now: Timestamp) -> Result<Self, Violation> {
        Ok(Self::with_drivers(root, now, Drivers::read(root)?))
    }

    fn with_drivers(root: &Path, now: Timestamp, drivers: Drivers) -> Self {
        Self {
            now,
            coordinator: crate::attempt::coordinator_label(&None),
            stall_limits: limits::StallLimits::read(root),
            turn_commands: limits::TurnCommands::read(root),
            drivers,
        }
    }
}

pub(crate) struct Stall {
    target: String,
    since: String,
    hours: f64,
    limit: f64,
    lane: Option<Lane>,
    active: bool,
}

/// Reads a cm stamp (`2026-10-08T07:04Z`) or any RFC 3339 instant.
pub fn instant(text: &str) -> Option<Timestamp> {
    text.parse().ok().or_else(|| {
        text.strip_suffix('Z')
            .and_then(|minute| format!("{minute}:00Z").parse().ok())
    })
}

fn steps(units: &Units) -> impl Iterator<Item = (String, &Step)> {
    units.iter().flat_map(|(unit, state)| {
        state
            .steps
            .iter()
            .map(move |(name, step)| (format!("{unit}/{name}"), step))
    })
}

fn active_attempt(step: &Step) -> Option<&Attempt> {
    step.attempt.as_ref().filter(|attempt| {
        attempt.ended.is_none() && matches!(step.track.resting(), Phase::Launched | Phase::PrOpen)
    })
}

pub(crate) fn stalls(units: &Units, inputs: &ActionInputs) -> Vec<Stall> {
    steps(units)
        .filter_map(|(target, step)| {
            if step.track.state != Phase::Launched || step.track.evidence.pr.is_some() {
                return None;
            }
            let active = active_attempt(step);
            // Keep the existing Codex stalled report for launches that predate attempt records
            // or whose coordinator has already ended the attempt.
            if active.is_none() && step.lane != Some(Lane::Codex) {
                return None;
            }
            // A step that finishes by evidence and waits on an open watch has declared its wait and
            // its deadline: the watch holds its next move, `done`, so the watch's expiry, not the
            // stall clock, calls for the coordinator. A code step's next move is `pr-open`, which no
            // watch holds, so its worker still stalls.
            let watching = step.finish(Release::of_target(&target).is_some()) == Finish::Evidence
                && step.watches.iter().any(|watch| {
                    watch.withdrawn.is_none()
                        && watch.resolved.is_none()
                        && instant(&watch.deadline).is_some_and(|deadline| deadline > inputs.now)
                });
            if watching {
                return None;
            }
            let lane = active.and_then(|attempt| attempt.lane).or(step.lane);
            // The step's own clock: launch and unblock reset it, a re-claim by a successor
            // coordinator does not, so an unblocked step never reads stalled from its first claim
            // and a takeover never hides a stall.
            let mut since = step.track.since.as_str();
            if let Some(attempt) = step.attempt.as_ref()
                && let Some(lane) = lane
                && inputs.turn_commands.for_lane(lane).is_some()
            {
                if active.is_some()
                    && remote_turn(attempt, lane)
                        .and_then(|turn| turn.observation(attempt))
                        .is_some_and(|read| read.completed_at.is_some())
                {
                    return None;
                }
                if let Some(began) = turn_began(attempt, lane)
                    && instant(began) > instant(since)
                {
                    since = began;
                }
            }
            let limit = inputs.stall_limits.hours(lane);
            let age = inputs.now.duration_since(instant(since)?).as_secs_f64() / 3600.0;
            (age > limit).then(|| Stall {
                target,
                since: since.to_owned(),
                hours: (age * 10.0).round() / 10.0,
                limit,
                lane,
                active: active.is_some(),
            })
        })
        .collect()
}

pub(crate) fn stall_json(stall: &Stall) -> Value {
    json!({
        "target": stall.target,
        "command": format!("cm attempt {}", stall.target),
        "since": stall.since,
        "hours": stall.hours,
        "limit_hours": stall.limit,
    })
}

pub(crate) fn stall_line(stall: &Stall) -> String {
    let reason = if stall.lane == Some(Lane::Codex) {
        "codex launch with no PR".to_owned()
    } else {
        format!("{} attempt", stall.lane.map_or("remote", Lane::name))
    };
    format!(
        "stalled {}: {reason}, since {} ({}h > {}h)",
        stall.target, stall.since, stall.hours, stall.limit
    )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(into = "&'static str")]
enum ActionKind {
    TurnEnded,
    PrOpenUnreviewed,
    MissingKickoff,
    StalledAttempt,
    EarlierCoordinator,
}

impl From<ActionKind> for &'static str {
    fn from(kind: ActionKind) -> Self {
        match kind {
            ActionKind::TurnEnded => "turn-ended",
            ActionKind::PrOpenUnreviewed => "pr-open-unreviewed",
            ActionKind::MissingKickoff => "missing-kickoff",
            ActionKind::StalledAttempt => "stalled-attempt",
            ActionKind::EarlierCoordinator => "earlier-coordinator",
        }
    }
}

impl ActionKind {
    fn act_priority(self) -> u8 {
        match self {
            Self::TurnEnded => 0,
            Self::PrOpenUnreviewed => 1,
            Self::MissingKickoff | Self::StalledAttempt | Self::EarlierCoordinator => 2,
        }
    }
}

#[derive(serde::Serialize)]
struct Action {
    kind: ActionKind,
    target: String,
    part: Option<String>,
    since: String,
    waiting_count: usize,
    command: String,
    #[serde(skip)]
    description: String,
}

impl Action {
    fn new(
        kind: ActionKind,
        target: &str,
        since: &str,
        command: String,
        description: String,
    ) -> Self {
        Self {
            kind,
            target: target.to_owned(),
            part: None,
            since: since.to_owned(),
            waiting_count: 0,
            command,
            description,
        }
    }

    fn summary(&self) -> String {
        let kind = <&str>::from(self.kind);
        let detail = self.description.replace(char::is_control, " ");
        let prefix = format!("{kind} {}: {detail}", self.target);
        if self.kind == ActionKind::TurnEnded {
            prefix
        } else {
            format!(
                "{prefix} since {} ({} waiting)",
                self.since, self.waiting_count
            )
        }
    }
}

fn waiting_counts(root: &Path, units: &Units, actions: &mut [Action]) {
    if actions.is_empty() {
        return;
    }
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (target, step) in steps(units).filter(|(_, step)| !step.track.state.is_final()) {
        for gate in gates(root, units, step)
            .into_iter()
            .filter(|gate| !gate.met())
        {
            dependents
                .entry(gate.after.target())
                .or_default()
                .push(target.clone());
        }
    }
    let mut counts = BTreeMap::new();
    for action in actions {
        action.waiting_count = *counts.entry(action.target.clone()).or_insert_with(|| {
            let mut seen = BTreeSet::from([&action.target]);
            let mut queue = vec![&action.target];
            while let Some(target) = queue.pop() {
                for dependent in dependents.get(target).into_iter().flatten() {
                    if seen.insert(dependent) {
                        queue.push(dependent);
                    }
                }
            }
            seen.len() - 1
        });
    }
}

fn actionable(
    root: &Path,
    units: &Units,
    inputs: &ActionInputs,
    stalled: &[Stall],
    verdicts: &crate::verdict::Verdicts,
) -> Vec<Action> {
    let reviews = crate::review::reviews(root);
    let verdict_prs: BTreeSet<_> = verdicts
        .reviews
        .iter()
        .filter_map(|row| row.pr.as_deref())
        .collect();
    let mut actions = Vec::new();
    for candidate in candidates(root, units) {
        if candidate.step.track.state == Phase::Drafted
            && candidate.kickoff.is_none()
            && candidate.gates.iter().all(Gate::met)
        {
            actions.push(Action::new(
                ActionKind::MissingKickoff,
                &candidate.target,
                &candidate.step.track.since,
                waiting_command(root, &candidate.target),
                "kickoff.md is missing".to_owned(),
            ));
        }
    }
    for (target, step) in steps(units) {
        if let Some(attempt) = active_attempt(step)
            && inputs
                .coordinator
                .as_deref()
                .is_some_and(|label| label != attempt.coordinator)
            && inputs
                .drivers
                .driving(&attempt.coordinator, &target)
                .is_none()
        {
            actions.push(Action::new(
                ActionKind::EarlierCoordinator,
                &target,
                &attempt.claimed_at,
                format!(
                    "cm attempt {} claim --expect {}",
                    shell_word(&target),
                    attempt.generation
                ),
                format!("claimed by {}", attempt.coordinator),
            ));
        }
        if step.track.state == Phase::Launched
            && let Some(attempt) = active_attempt(step)
            && let Some(lane) = attempt.lane.or(step.lane)
            && let Some(programs) = inputs.turn_commands.for_lane(lane)
            && let Some(turn) = remote_turn(attempt, lane)
            && let Some(completed_at) = turn
                .observation(attempt)
                .and_then(|read| read.completed_at.as_deref())
        {
            let command = programs
                .logs
                .argv(turn.thread, turn.turn)
                .iter()
                .map(|arg| shell_word(arg))
                .collect::<Vec<_>>()
                .join(" ");
            actions.push(Action::new(
                ActionKind::TurnEnded,
                &target,
                completed_at,
                command,
                format!("thread {} completed {completed_at}, no PR", turn.thread),
            ));
        }
        let mut review = |part: Option<&str>, track: &Track| {
            let Some(pr) = track.evidence.pr.as_deref() else {
                return;
            };
            if track.state != Phase::PrOpen
                || verdict_prs.contains(pr)
                || reviews
                    .as_ref()
                    .is_some_and(|rows| crate::review::reviewed(rows, pr, &target))
                || crate::review::wait_marker(root, pr).is_some_and(|mark| mark.is_file())
            {
                return;
            }
            let detail = part.map_or_else(
                || format!("PR {pr}"),
                |part| format!("part {part}, PR {pr}"),
            );
            let mut action = Action::new(
                ActionKind::PrOpenUnreviewed,
                &target,
                &track.since,
                format!("cm brief {}", shell_word(&target)),
                detail,
            );
            action.part = part.map(str::to_owned);
            actions.push(action);
        };
        if step.parts.is_empty() {
            review(None, &step.track);
        } else {
            for (part, track) in &step.parts {
                review(Some(part), track);
            }
        }
    }
    actions.extend(stalled.iter().filter(|stall| stall.active).map(|stall| {
        Action::new(
            ActionKind::StalledAttempt,
            &stall.target,
            &stall.since,
            format!("cm attempt {}", shell_word(&stall.target)),
            format!(
                "stalled attempt {} ({}, {}h > {}h)",
                stall.target,
                stall.lane.map_or("remote", Lane::name),
                stall.hours,
                stall.limit
            ),
        )
    }));
    waiting_counts(root, units, &mut actions);
    actions.sort_by(|a, b| {
        (Reverse(a.waiting_count), &a.target, &a.part, a.kind).cmp(&(
            Reverse(b.waiting_count),
            &b.target,
            &b.part,
            b.kind,
        ))
    });
    actions
}

fn action_scope(
    actions: Vec<Action>,
    units: &Units,
    target: Option<&Target>,
    lane: Option<Lane>,
) -> Vec<Action> {
    actions
        .into_iter()
        .filter(|action| {
            target.is_none_or(|target| {
                split(&action.target).is_some_and(|(unit, name)| {
                    unit == target.unit && Some(name) == target.step.as_deref()
                })
            }) && lane.is_none_or(|lane| {
                step(units, &action.target).is_some_and(|step| step.lane == Some(lane))
            })
        })
        .collect()
}

fn append_actions(outcome: &mut Outcome, number: &mut usize, actions: &[Action]) {
    for action in actions {
        outcome.line(next_item(number, &action.summary(), &action.command));
    }
    outcome.field("actionable", json!(actions));
}

fn driver_held(
    units: &Units,
    inputs: &ActionInputs,
    target: Option<&Target>,
    lane: Option<Lane>,
) -> Vec<Value> {
    steps(units)
        .filter(|(full, step)| {
            target.is_none_or(|target| {
                split(full).is_some_and(|(unit, name)| {
                    unit == target.unit && Some(name) == target.step.as_deref()
                })
            }) && lane.is_none_or(|lane| step.lane == Some(lane))
        })
        .filter_map(|(full, step)| {
            let attempt = active_attempt(step)?;
            if inputs.coordinator.as_deref()
                == Some(crate::attempt::session_label(&attempt.coordinator).as_str())
            {
                return None;
            }
            let driven = inputs.drivers.driving(&attempt.coordinator, &full)?;
            let note = format!(
                "note: {full} is held by {}, driver of lane {driven} (generation {}, claimed {})",
                attempt.coordinator, attempt.generation, attempt.claimed_at
            );
            Some(json!({
                "target": full,
                "driver": attempt.coordinator,
                "driver_lane": driven,
                "generation": attempt.generation,
                "since": attempt.claimed_at,
                "note": note,
            }))
        })
        .collect()
}

// Absent while empty, so `cm next` output is unchanged where no driver holds a step.
fn append_held(outcome: &mut Outcome, held: &[Value]) {
    for item in held {
        outcome.line(item["note"].as_str().unwrap_or_default());
    }
    if !held.is_empty() {
        outcome.field("driver_held", held);
    }
}

pub(crate) fn action_summary(
    root: &Path,
    units: &Units,
    now: Timestamp,
) -> (usize, Option<String>) {
    // CARD.md counts what any reader sees: which session runs the sync is not a campaign fact.
    let inputs = ActionInputs {
        coordinator: None,
        ..ActionInputs::read(root, now)
    };
    let stalled = stalls(units, &inputs);
    // Unlike next, sync has not historically refused an unreadable review ledger.
    let verdicts = crate::verdict::load(root).unwrap_or_default();
    let actions = actionable(root, units, &inputs, &stalled, &verdicts);
    let oldest = actions
        .iter()
        .filter_map(|action| Some((instant(&action.since)?, &action.since)))
        .min()
        .map(|(_, since)| since.clone());
    (actions.len(), oldest)
}

pub(crate) fn next_act(
    root: &Path,
    target: Option<&Target>,
    lane: Option<Lane>,
    now: Timestamp,
) -> Result<(Outcome, bool)> {
    let mut outcome = Outcome::default();
    let mut actions = if store::load_campaign(root)?.closed.is_some() {
        Vec::new()
    } else {
        let units = load_units(root)?;
        let inputs = match ActionInputs::load(root, now) {
            Ok(inputs) => inputs,
            Err(violation) => return Ok((Outcome::refused(vec![violation]), true)),
        };
        let stalled = stalls(&units, &inputs);
        let verdicts = crate::verdict::load(root)?;
        action_scope(
            actionable(root, &units, &inputs, &stalled, &verdicts),
            &units,
            target,
            lane,
        )
    };
    actions.sort_by_key(|action| action.kind.act_priority());
    for action in &actions {
        outcome.line(format!("{} — `{}`", action.summary(), action.command));
    }
    outcome.field("actionable", json!(actions));
    Ok((outcome, !actions.is_empty()))
}

/// What `cm sync` last saw of an open PR, in `.cm/pulls.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Observed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_base: Option<String>,
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
                && track(units, &observed.target).is_some_and(|track| {
                    track.state == Phase::PrOpen
                        && track.evidence.pr.as_deref() == Some(url.as_str())
                })
        })
        .collect()
}

pub fn dirty_json(url: &str, observed: &Observed) -> Value {
    json!({
        "target": observed.target,
        "command": format!("cm attention --pr {} --worker '<worker-session>' --coordinator '<coordinator-session>'", shell_word(url)),
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

pub(crate) fn stale_bases(root: &Path, units: &Units) -> Vec<Value> {
    let Ok(seen) = store::read_json::<BTreeMap<String, Observed>>(&pulls_path(root)) else {
        return Vec::new();
    };
    seen.into_iter()
        .filter_map(|(url, observed)| {
            let track = track(units, &observed.target)?;
            if track.state != Phase::PrOpen || track.evidence.pr.as_deref() != Some(url.as_str()) {
                return None;
            }
            let base = observed.base.as_ref()?;
            let ci_base = observed.ci_base.as_ref()?;
            let ci_head = observed.ci_head.as_ref()?;
            if base == ci_base {
                return None;
            }
            Some(
                json!({ "target": observed.target, "pr": url, "head": observed.head,
            "base": base, "ci_head": ci_head, "ci_base": ci_base, "observed_at": observed.at,
            "command": "cm sync" }),
            )
        })
        .collect()
}

pub(crate) fn stale_base_line(item: &Value) -> String {
    format!(
        "stale-base {}: base moved from {} to {} since CI head {} (seen {})",
        item["target"].as_str().unwrap(),
        item["ci_base"].as_str().unwrap(),
        item["base"].as_str().unwrap(),
        item["ci_head"].as_str().unwrap(),
        item["observed_at"].as_str().unwrap()
    )
}

pub(crate) fn review_waits(
    units: &Units,
    now: Timestamp,
    target: Option<&Target>,
    lane: Option<Lane>,
) -> Vec<Value> {
    let mut waits = Vec::new();
    for (unit_name, unit) in units {
        for (name, step) in &unit.steps {
            let full = format!("{unit_name}/{name}");
            if lane.is_some_and(|lane| step.lane != Some(lane))
                || target.is_some_and(|target| {
                    target.unit != *unit_name || target.step.as_deref() != Some(name.as_str())
                })
            {
                continue;
            }
            let tracks = std::iter::once((None, &step.track))
                .chain(step.parts.iter().map(|(part, track)| (Some(part), track)));
            for (part, track) in tracks {
                let Phase::Blocked(reason) = &track.state else {
                    continue;
                };
                if !["EVIDENCE-BLOCKED", "REVIEW_BLOCKED"].contains(&reason.as_str()) {
                    continue;
                }
                let elapsed =
                    instant(&track.since).map(|since| now.duration_since(since).as_secs().max(0));
                let (pr, head) = if part.is_none() && !step.parts.is_empty() {
                    (None, None)
                } else {
                    (track.evidence.pr.as_deref(), track.evidence.sha.as_deref())
                };
                waits.push(json!({
                    "target": full, "part": part, "pr": pr, "head": head,
                    "reason": reason, "since": track.since, "waited_seconds": elapsed,
                    "command": format!("cm next {}", shell_word(&full)),
                }));
            }
        }
    }
    waits
}

pub(crate) fn review_wait_line(wait: &Value) -> String {
    let part = wait["part"]
        .as_str()
        .map(|part| format!("#{part}"))
        .unwrap_or_default();
    let elapsed = wait["waited_seconds"]
        .as_i64()
        .map(|seconds| format!("{seconds}s"))
        .unwrap_or_else(|| "unknown duration".into());
    let head = wait["head"]
        .as_str()
        .map(|head| format!(" (head {head})"))
        .unwrap_or_default();
    format!(
        "reviewer blocked {}{part}: {}, waited {elapsed} since {}{head}",
        wait["target"].as_str().unwrap(),
        wait["reason"].as_str().unwrap(),
        wait["since"].as_str().unwrap(),
    )
}

pub struct Next {
    pub paused: bool,
    pub ready: Vec<(String, Option<Lane>, Finish)>,
    pub waiting: Vec<(String, Vec<String>)>,
}

pub(crate) fn mac_dispatch_refusal(target: &str) -> Violation {
    Violation::new(
        target,
        "mac-lane-dispatch",
        "Mac lane work goes through the Mac queue. See `cm mac-lane list` for open items",
    )
}

pub(crate) fn launch_command(target: &str, lane: Option<Lane>, finish: Finish) -> String {
    if lane == Some(Lane::Owner) && finish == Finish::Evidence {
        return format!("cm advance {target} done --evidence '<owner-evidence-file>'");
    }
    let worktree = if lane == Some(Lane::Pi) {
        " --worktree '<worktree-path>'"
    } else {
        ""
    };
    format!("cm launch {target}{worktree}")
}

pub(crate) fn waiting_command(root: &Path, target: &str) -> String {
    if kickoff(root, target).is_none() {
        format!("cm amend {target} --kickoff-file '<kickoff-file>'")
    } else {
        format!("cm next {target}")
    }
}

pub(crate) fn unblock_command(target: &str, condition_met: bool) -> String {
    if condition_met {
        format!("cm unblock {target}")
    } else {
        format!("cm unblock {target} --evidence '<unblock-evidence-file>'")
    }
}

// CARD.md, its only caller, shows no Codex hold: CM_CODEX_WEEKLY_PCT is the caller's environment.
pub fn summary(root: &Path, units: &Units) -> Next {
    let paused = paused(root).unwrap_or(true);
    let mut ready = Vec::new();
    let mut waiting = Vec::new();
    for candidate in candidates(root, units) {
        if !paused && candidate.ready() {
            ready.push((
                candidate.target.clone(),
                candidate.lane(),
                candidate.finish(),
            ));
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
    let inputs = match ActionInputs::load(root, now) {
        Ok(inputs) => inputs,
        Err(violation) => return Ok(Outcome::refused(vec![violation])),
    };
    let units = load_units(root)?;
    let all = candidates(root, &units);
    let mut outcome = Outcome::default();
    crate::ledger::owner_queue_banner(root, &mut outcome)?;
    let mut number = 1;
    outcome.field("paused", paused);
    let stale_bases: Vec<_> = stale_bases(root, &units)
        .into_iter()
        .filter(|item| {
            let full = item["target"].as_str().unwrap().split('#').next().unwrap();
            target.is_none_or(|target| {
                full == format!(
                    "{}/{}",
                    target.unit,
                    target.step.as_deref().unwrap_or_default()
                )
            }) && lane
                .is_none_or(|lane| step(&units, full).is_some_and(|step| step.lane == Some(lane)))
        })
        .collect();
    for item in &stale_bases {
        outcome.line(next_item(&mut number, &stale_base_line(item), "cm sync"));
    }
    outcome.field("stale_base", stale_bases);
    let review_waits = review_waits(&units, now, target, lane);
    for wait in &review_waits {
        outcome.line(next_item(
            &mut number,
            &review_wait_line(wait),
            wait["command"].as_str().unwrap(),
        ));
    }
    outcome.field("review_waits", review_waits);
    let verdicts = crate::verdict::load(root)?;
    let stalled = stalls(&units, &inputs);
    let actions = action_scope(
        actionable(root, &units, &inputs, &stalled, &verdicts),
        &units,
        target,
        lane,
    );
    let driven = driver_held(&units, &inputs, target, lane);
    let quota = limits::codex_pause();
    let quota = (!paused).then_some(quota.as_deref()).flatten();
    let fresh_readers: Vec<Value> = crate::verdict::fresh_readers(&verdicts)
        .into_iter()
        .filter(|review| {
            step(&units, &review.step).is_some_and(|step| {
                let track = match review.part.as_deref() {
                    Some(part) => step.parts.get(part),
                    None if step.parts.is_empty() => Some(&step.track),
                    None => None,
                };
                step.track
                    .resting()
                    .rank()
                    .zip(Phase::Merged.rank())
                    .is_some_and(|(state, merged)| state < merged)
                    && lane.is_none_or(|lane| step.lane == Some(lane))
                    && track.is_some_and(|track| {
                        track.evidence.pr == review.pr
                            && track
                                .resting()
                                .rank()
                                .zip(Phase::Merged.rank())
                                .is_some_and(|(state, merged)| state < merged)
                    })
            }) && target.is_none_or(|target| {
                review.step
                    == format!(
                        "{}/{}",
                        target.unit,
                        target.step.as_deref().unwrap_or_default()
                    )
            })
        })
        .map(|review| {
            let part = review
                .part
                .as_ref()
                .map(|part| format!(" --part={}", shell_word(part)))
                .unwrap_or_default();
            let command = format!(
                "cm review record {} '<fresh-review-report>' --reviewer fresh-opus{part}",
                shell_word(&review.step)
            );
            outcome.line(next_item(
                &mut number,
                &format!(
                    "fresh reader {}{}: independent Opus, high effort",
                    review.step,
                    review
                        .part
                        .as_ref()
                        .map(|part| format!(" part {part}"))
                        .unwrap_or_default()
                ),
                &command,
            ));
            json!({"target": review.step, "part": review.part, "pr": review.pr,
                "review": review.id(), "converging": false, "command": command})
        })
        .collect();
    let has_fresh_reader = !fresh_readers.is_empty();
    outcome.field("fresh_readers", fresh_readers);
    if let Some(target) = target {
        let full = format!(
            "{}/{}",
            target.unit,
            target.step.as_deref().unwrap_or_default()
        );
        let candidate = all.iter().find(|candidate| candidate.target == full);
        if has_fresh_reader && !candidate.is_some_and(Candidate::warns_pin) {
            outcome.field("steps", Vec::<Value>::new());
            outcome.field("waiting", Vec::<Value>::new());
            append_actions(&mut outcome, &mut number, &actions);
            append_held(&mut outcome, &driven);
            return Ok(outcome);
        }
        let Some(candidate) = candidate else {
            let step = step(&units, &full);
            if step.is_some_and(|step| step.lane == Some(Lane::Mac)) {
                outcome.violations.push(mac_dispatch_refusal(&full));
                return Ok(outcome);
            }
            let message = match step {
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
            outcome
                .violations
                .push(Violation::new(&full, "next-state", message));
            append_actions(&mut outcome, &mut number, &actions);
            append_held(&mut outcome, &driven);
            return Ok(outcome);
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
        if candidate.step.track.state == Phase::Drafted && candidate.kickoff.is_none() {
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
        let quota_hold = candidate.dispatch_hold(quota);
        if let Some(reason) = quota_hold {
            outcome
                .violations
                .push(limits::codex_pause_violation(&full, reason));
        }
        if outcome.ok() {
            outcome.line(next_item(
                &mut number,
                &format!(
                    "{full} ({}) is next",
                    candidate.lane().map_or("—", Lane::name)
                ),
                &launch_command(&full, candidate.lane(), candidate.finish()),
            ));
            outcome.field("steps", vec![candidate.to_json(root)]);
            outcome.field("waiting", Vec::<Value>::new());
        } else {
            let mut on = candidate.waits_on();
            on.extend(quota_hold.map(str::to_owned));
            if !on.is_empty() {
                outcome.line(next_item(
                    &mut number,
                    &format!("waits {full}: {}", on.join("; ")),
                    &waiting_command(root, &full),
                ));
            }
            outcome.field("steps", Vec::<Value>::new());
            outcome.field("waiting", vec![candidate.waiting_json(root, quota)]);
        }
        append_actions(&mut outcome, &mut number, &actions);
        append_held(&mut outcome, &driven);
        return Ok(outcome);
    }
    let in_lane = |candidate: &&Candidate| lane.is_none_or(|lane| candidate.lane() == Some(lane));
    let ready: Vec<&Candidate> = all
        .iter()
        .filter(in_lane)
        .filter(|candidate| {
            !paused && candidate.ready() && candidate.dispatch_hold(quota).is_none()
        })
        .collect();
    let waiting: Vec<&Candidate> = all
        .iter()
        .filter(in_lane)
        .filter(|candidate| {
            paused || !candidate.ready() || candidate.dispatch_hold(quota).is_some()
        })
        .collect();
    if paused {
        outcome.line("paused: nothing is next while LIMITS.toml has `paused = true`");
    }
    for candidate in &ready {
        outcome.line(next_item(
            &mut number,
            &format!(
                "next {} ({})",
                candidate.target,
                candidate.lane().map_or("—", Lane::name)
            ),
            &launch_command(&candidate.target, candidate.lane(), candidate.finish()),
        ));
    }
    for candidate in &waiting {
        let mut on = candidate.waits_on();
        on.extend(candidate.dispatch_hold(quota).map(str::to_owned));
        if !on.is_empty() {
            outcome.line(next_item(
                &mut number,
                &format!("waits {}: {}", candidate.target, on.join("; ")),
                &waiting_command(root, &candidate.target),
            ));
        }
    }
    let tags = untagged(&units);
    for tag in &tags {
        outcome.line(next_item(
            &mut number,
            &format!(
                "tag {} {} at {}",
                tag.repo
                    .as_ref()
                    .map_or_else(|| tag.release.line.clone(), ToString::to_string),
                tag.tag,
                tag.sha.as_deref().unwrap_or("—")
            ),
            "cm sync",
        ));
    }
    if ready.is_empty() && waiting.is_empty() && tags.is_empty() && !has_fresh_reader {
        outcome.line("nothing is declared and drafted");
    }
    let dirty = dirty(root, &units);
    for stall in &stalled {
        outcome.line(next_item(
            &mut number,
            &stall_line(stall),
            stall_json(stall)["command"].as_str().unwrap(),
        ));
    }
    for (url, observed) in &dirty {
        outcome.line(next_item(
            &mut number,
            &dirty_line(url, observed),
            dirty_json(url, observed)["command"].as_str().unwrap(),
        ));
    }
    let unblock = unblock_now(root, &units, lane);
    if !unblock.is_empty() {
        outcome.line("unblock now");
    }
    for item in &unblock {
        outcome.line(next_item(
            &mut number,
            &format!(
                "{}: {} holds",
                item["target"].as_str().unwrap(),
                item["condition"].as_str().unwrap()
            ),
            item["command"].as_str().unwrap(),
        ));
    }
    outcome.field("unblock_now", unblock);
    let held = crate::holds::held(&units, now, root, &mut number);
    outcome.lines.extend(held.lines);
    outcome.field("watches", held.watches);
    outcome.field("stale", held.stale);
    outcome.field(
        "stalled",
        stalled.iter().map(stall_json).collect::<Vec<_>>(),
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
            .map(|candidate| candidate.waiting_json(root, quota))
            .collect::<Vec<_>>(),
    );
    outcome.field(
        "tags",
        tags.iter()
            .map(|tag| {
                json!({
                    "target": tag.target,
                    "command": "cm sync",
                    "repo": tag.repo.as_ref().map(ToString::to_string),
                    "version": tag.release.version.to_string(),
                    "tags": [tag.tag.to_string()],
                    "sha": tag.sha,
                })
            })
            .collect::<Vec<_>>(),
    );
    append_actions(&mut outcome, &mut number, &actions);
    append_held(&mut outcome, &driven);
    Ok(outcome)
}

pub(crate) fn block_condition(text: &str, unit: &str) -> Result<String, String> {
    if text == "epoch:WORKING" {
        return Ok(text.into());
    }
    if let Some(number) = text.strip_prefix("owner-queue:") {
        let n: u64 = number
            .parse()
            .map_err(|_| "owner-queue condition needs a positive item number")?;
        if n == 0 || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err("owner-queue condition needs a positive item number".into());
        }
        return Ok(format!("owner-queue:{n}"));
    }
    match After::parse(text, unit)? {
        After::Pin(_) => Err("pin gates are not supported in blocked:until:".into()),
        after => Ok(after.to_string()),
    }
}

pub(crate) fn block_met(root: &Path, units: &Units, unit: &str, condition: &str) -> bool {
    if condition == "epoch:WORKING" {
        return fs::read_to_string(root.join(crate::epoch::EPOCH))
            .is_ok_and(|text| crate::epoch::status(&text) == Some(crate::epoch::Status::Working));
    }

    if let Some(number) = condition.strip_prefix("owner-queue:") {
        return fs::read_to_string(root.join("OWNER-QUEUE.md")).is_ok_and(|text| {
            !crate::markdown::headings(&text).iter().any(|h| {
                h.level == 2
                    && h.text
                        .split_once(". ")
                        .is_some_and(|(n, _)| n.parse::<u64>().ok() == number.parse::<u64>().ok())
            })
        });
    }
    After::parse(condition, unit).is_ok_and(|after| gate(units, &after).met())
}

pub(crate) fn unblock_now(root: &Path, units: &Units, lane: Option<Lane>) -> Vec<Value> {
    units.iter().flat_map(|(unit, state)| state.steps.iter().filter_map(move |(name, step)| {
        let Phase::Blocked(reason) = &step.track.state else { return None; };
        let condition = reason.strip_prefix("until:")?;
        (lane.is_none_or(|l| step.lane == Some(l)) && block_met(root, units, unit, condition))
            .then(|| {
                let target = format!("{unit}/{name}");
                json!({"command": unblock_command(&target, true), "target": target, "condition": condition, "since": step.track.since})
            })
    })).collect()
}
