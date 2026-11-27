use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
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
use crate::state::{
    After, Attempt, AttemptOutcome, Finish, Lane, Phase, Priority, RELEASES, Release, Step, Track,
    UnitState, Work,
};
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

/// How much review a card needs, derived by rule from its kickoff's checklist line
/// `Weight: simple|complex — repos: <list>; files: <n or list>; surface: none|<what>`: simple only
/// for one repository, at most three footprint files and no contract, auth or secret surface.
/// A missing line is complex. kickoff-lint's grammar: an optional `- ` bullet, any case, `—`, `-`
/// or `:` before the checklist, a backticked value, and `files: 12` or `12 files` as twelve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Weight {
    Simple,
    Complex,
}

/// Why a kickoff's `Weight:` line is refused, with its fixed violation code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightRefusal {
    pub rule: &'static str,
    pub message: String,
}

const SIMPLE_MAX_FILES: usize = 3;

/// The weight a kickoff states and derives, or why its `Weight:` line is refused: the line does
/// not parse (`weight-malformed`), or it says simple while its checklist derives complex
/// (`weight-contradiction`). No line is complex.
pub fn weight_check(kickoff: &str) -> Result<Weight, WeightRefusal> {
    let Some(line) = kickoff.lines().find_map(|line| {
        let line = line.trim_start();
        let line = line.strip_prefix("- ").unwrap_or(line);
        line.get(..7)
            .filter(|head| head.eq_ignore_ascii_case("weight:"))
            .map(|_| &line[7..])
    }) else {
        return Ok(Weight::Complex);
    };
    let malformed = || WeightRefusal {
        rule: "weight-malformed",
        message: format!(
            "the kickoff's `Weight:{line}` is not `Weight: simple|complex — repos: <list>; files: <n or list>; surface: none|<what>`"
        ),
    };
    let rest = line.trim_start();
    let word_end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let stated = match rest[..word_end].to_ascii_lowercase().as_str() {
        "simple" => Weight::Simple,
        "complex" => Weight::Complex,
        _ => return Err(malformed()),
    };
    let after = rest[word_end..].trim_start();
    let checklist = after.trim_start_matches(['—', ':', '-']);
    if checklist.len() == after.len() {
        return Err(malformed());
    }
    let fields: Vec<&str> = checklist.split(';').collect();
    let [repos, files, surface] = fields.as_slice() else {
        return Err(malformed());
    };
    fn value<'a>(field: &'a str, key: &str) -> Option<&'a str> {
        let (name, value) = field.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case(key)
            .then(|| value.trim().trim_matches('`').trim())
    }
    let (Some(repos), Some(files), Some(surface)) = (
        value(repos, "repos"),
        value(files, "files"),
        value(surface, "surface"),
    ) else {
        return Err(malformed());
    };
    let items = |text: &str| {
        text.split(',')
            .map(|item| item.trim().trim_matches('`').trim())
            .filter(|item| !item.is_empty())
            .count()
    };
    let lower = files.to_ascii_lowercase();
    let counted = lower
        .strip_suffix("files")
        .or_else(|| lower.strip_suffix("file"))
        .unwrap_or(&lower)
        .trim()
        .parse::<usize>()
        .ok()
        .or_else(|| (items(files) > 0).then(|| items(files)));
    let derived = if items(repos) == 1
        && counted.is_some_and(|files| files <= SIMPLE_MAX_FILES)
        && surface.eq_ignore_ascii_case("none")
    {
        Weight::Simple
    } else {
        Weight::Complex
    };
    if stated == Weight::Simple && derived == Weight::Complex {
        return Err(WeightRefusal {
            rule: "weight-contradiction",
            message: format!(
                "the kickoff says simple, but its checklist derives complex ({} repo(s), {} file(s), surface {surface}); simple is one repository, at most {SIMPLE_MAX_FILES} files and surface none",
                items(repos),
                counted.map_or_else(|| "no".to_owned(), |files| files.to_string())
            ),
        });
    }
    Ok(stated)
}

/// The weight a kickoff derives; a refused line is complex.
pub fn weight_of(kickoff: &str) -> Weight {
    weight_check(kickoff).unwrap_or(Weight::Complex)
}

/// The weight refusal of a kickoff, as a violation on its step.
pub(crate) fn weight_violation(target: &str, kickoff: &str) -> Option<Violation> {
    weight_check(kickoff)
        .err()
        .map(|refusal| Violation::new(target, refusal.rule, refusal.message))
}

pub(crate) fn weight(root: &Path, target: &str) -> Weight {
    kickoff(root, target)
        .and_then(|path| fs::read_to_string(path).ok())
        .map_or(Weight::Complex, |text| weight_of(&text))
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
            "weight": weight(root, &self.target),
            "dir": self.target,
            "kickoff": self.kickoff.as_ref().map(|path| relative(root, path)),
            "after": self.gates.iter().map(Gate::to_json).collect::<Vec<_>>(),
        })
    }

    fn dispatch_hold<'a>(&self, quota: Option<&'a str>) -> Option<&'a str> {
        quota.filter(|_| self.lane() == Some(Lane::Codex) && self.ready())
    }

    /// The one predicate for `cm next`'s launchable list and the `launchable-not-launched` action:
    /// ready, the campaign not paused and no Codex quota hold on its dispatch.
    fn launchable(&self, paused: bool, quota: Option<&str>) -> bool {
        !paused && self.ready() && self.dispatch_hold(quota).is_none()
    }

    /// The newest of the step's own stamp and its gate targets' stamps: when it became launchable.
    fn launchable_since(&self) -> &str {
        std::iter::once(self.step.track.since.as_str())
            .chain(
                self.gates
                    .iter()
                    .filter_map(|gate| gate.track.as_ref().map(|track| track.since.as_str())),
            )
            .max_by_key(|since| instant(since))
            .unwrap_or_default()
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
    let mut out = units
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
        .collect::<Vec<_>>();
    out.sort_by_key(|candidate| priority_of(units, &candidate.target));
    out
}

/// The priority of the unit a target (a step, or a unit itself) belongs to.
fn priority_of(units: &Units, target: &str) -> Priority {
    units
        .get(target)
        .or_else(|| units.get(split(target)?.0))
        .map_or(Priority::Whenever, |unit| unit.priority)
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
    paused: bool,
    codex_quota: Option<String>,
    release_after_hours: Option<f64>,
    deploy_after_hours: Option<f64>,
    drivers: Drivers,
}

impl ActionInputs {
    // What any reader sees, for CARD.md and sync's stall lines: which session runs the sync is not
    // a campaign fact, so neither its coordinator nor the lane-driver registry enters.
    pub(crate) fn read(root: &Path, now: Timestamp) -> Self {
        Self {
            coordinator: None,
            ..Self::with_drivers(root, now, Drivers::default())
        }
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
            paused: paused(root).unwrap_or(true),
            codex_quota: limits::codex_pause(),
            release_after_hours: limits::read(root).and_then(|limits| limits.release_after_hours),
            deploy_after_hours: limits::read(root).and_then(|limits| limits.deploy_after_hours),
            drivers,
        }
    }

    /// The Codex quota hold as `cm next` applies it: none while paused, which already holds all.
    fn quota(&self) -> Option<&str> {
        (!self.paused)
            .then_some(self.codex_quota.as_deref())
            .flatten()
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

pub(crate) fn active_attempt(step: &Step) -> Option<&Attempt> {
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
    ReviewOwed,
    MissingKickoff,
    StalledAttempt,
    EarlierCoordinator,
    LaunchableNotLaunched,
    AttemptEnded,
    MergedUnreleased,
    UnblockNow,
    ExpiredWatch,
    MergeReady,
    StaleBlock,
    ReleasedUndeployed,
    LanePaused,
    ContextOver,
}

impl From<ActionKind> for &'static str {
    fn from(kind: ActionKind) -> Self {
        match kind {
            ActionKind::TurnEnded => "turn-ended",
            ActionKind::ReviewOwed => "review-owed",
            ActionKind::MissingKickoff => "missing-kickoff",
            ActionKind::StalledAttempt => "stalled-attempt",
            ActionKind::EarlierCoordinator => "earlier-coordinator",
            ActionKind::LaunchableNotLaunched => "launchable-not-launched",
            ActionKind::AttemptEnded => "attempt-ended",
            ActionKind::MergedUnreleased => "merged-unreleased",
            ActionKind::UnblockNow => "unblock-now",
            ActionKind::ExpiredWatch => "expired-watch",
            ActionKind::MergeReady => "merge-ready",
            ActionKind::StaleBlock => "stale-block",
            ActionKind::ReleasedUndeployed => "released-undeployed",
            ActionKind::LanePaused => "lane-paused",
            ActionKind::ContextOver => "context-over",
        }
    }
}

impl ActionKind {
    fn act_priority(self) -> u8 {
        match self {
            Self::TurnEnded => 0,
            Self::ReviewOwed => 1,
            Self::MissingKickoff
            | Self::StalledAttempt
            | Self::EarlierCoordinator
            | Self::LaunchableNotLaunched
            | Self::AttemptEnded
            | Self::MergedUnreleased
            | Self::UnblockNow
            | Self::ExpiredWatch
            | Self::MergeReady
            | Self::StaleBlock
            | Self::ReleasedUndeployed
            | Self::LanePaused
            | Self::ContextOver => 2,
        }
    }

    /// Kinds whose line `cm next` already prints as its own item (next, unblock now, watch); their
    /// actions stay in the JSON `actionable` list without a second numbered line.
    fn printed_by_next(self) -> bool {
        matches!(
            self,
            Self::LaunchableNotLaunched | Self::UnblockNow | Self::ExpiredWatch
        )
    }
}

#[derive(serde::Serialize)]
struct Action {
    kind: ActionKind,
    target: String,
    priority: Priority,
    part: Option<String>,
    since: String,
    waiting_count: usize,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lane: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    weight: Option<Weight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ci_green: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handoff: Option<String>,
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
            priority: Priority::Whenever,
            part: None,
            since: since.to_owned(),
            waiting_count: 0,
            command,
            lane: None,
            weight: None,
            pr: None,
            head: None,
            base: None,
            ci_green: None,
            handoff: None,
            description,
        }
    }

    fn summary(&self) -> String {
        let kind = <&str>::from(self.kind);
        let detail = self.description.replace(char::is_control, " ");
        let prefix = format!("{kind} {}: {detail}", self.target);
        if matches!(self.kind, ActionKind::TurnEnded | ActionKind::ContextOver) {
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

fn context_over(context: &crate::state::ObservedContext, remedy: &str) -> String {
    let stale = context
        .unread_since
        .as_deref()
        .map(|since| format!("; stale: unread since {since}"))
        .unwrap_or_default();
    format!(
        "{} at {}k > {}k cap (read {}{stale}); {remedy}",
        context.session,
        context.kilotokens(),
        limits::CONTEXT_CAP_TOKENS / 1000,
        context.read_at
    )
}

// The registry is caller-relative, so CARD.md, which reads no registry, lists none of these.
fn drivers_over_cap(root: &Path, inputs: &ActionInputs) -> Vec<Action> {
    if inputs.turn_commands.for_lane(Lane::ClaudeCloud).is_none() {
        return Vec::new();
    }
    let contexts: BTreeMap<String, crate::state::ObservedContext> =
        store::read_json(&crate::sync::lane_context_path(root)).unwrap_or_default();
    inputs
        .drivers
        .drivers
        .iter()
        .filter_map(|(lane, driver)| {
            let context = contexts.get(lane).filter(|context| {
                context.session == crate::attempt::session_label(&driver.session)
                    && context.over_cap()
            })?;
            let units: String = driver
                .units
                .iter()
                .map(|unit| format!(" --unit {}", shell_word(unit)))
                .collect();
            Some(Action::new(
                ActionKind::ContextOver,
                &format!("lane:{lane}"),
                &context.read_at,
                format!(
                    "cm lane driver add {} --session '<fresh-session>'{units}",
                    shell_word(lane)
                ),
                context_over(
                    context,
                    "hand off to a fresh driver session from the lane's handoff",
                ),
            ))
        })
        .collect()
}

/// A launched step with no PR whose last attempt has ended: the coordinator owes the next move.
/// It cannot overlap `turn-ended` or `stalled-attempt`, which need a live attempt.
fn attempt_ended(target: &str, step: &Step, now: Timestamp) -> Option<Action> {
    if step.track.state != Phase::Launched
        || step.lane == Some(Lane::Mac)
        || step.track.evidence.pr.is_some()
        || step.parts.values().any(|part| part.evidence.pr.is_some())
    {
        return None;
    }
    let attempt = step.attempt.as_ref()?;
    let ended = attempt.ended.as_ref()?;
    let word = shell_word(target);
    // Restart is refused past the attempt's deadline (`attempt-deadline`): inspect it instead.
    let command =
        if ended.outcome == AttemptOutcome::Unknown && crate::attempt::expired(attempt, now) {
            format!("cm attempt {word}")
        } else if ended.outcome == AttemptOutcome::Unknown {
            let lane = attempt.lane.or(step.lane).map_or("'<lane>'", Lane::name);
            format!("cm attempt {word} restart --lane {lane}")
        } else {
            let part = if step.parts.is_empty() {
                ""
            } else {
                " --part '<part>'"
            };
            match step.finish(Release::of_target(target).is_some()) {
                Finish::Evidence => format!("cm advance {word} done --evidence '<evidence-file>'"),
                Finish::Shipped | Finish::Rollout => {
                    format!("cm advance {word} pr-open --pr '<pr-url>' --sha '<head-sha>'{part}")
                }
            }
        };
    Some(Action::new(
        ActionKind::AttemptEnded,
        target,
        &ended.at,
        command,
        format!(
            "attempt {} ended {} at {}, no PR",
            attempt.id, ended.outcome, ended.at
        ),
    ))
}

/// A PR whose observed head has green CI and a merge gate with no gaps: it can merge now.
fn merge_ready(
    root: &Path,
    target: &str,
    part: Option<&str>,
    pr: &str,
    observed: Option<&Observed>,
    since: &str,
    gated: bool,
) -> Option<Action> {
    let observed = observed?;
    let head = observed.head.as_deref()?;
    let clean =
        matches!(&observed.mergeable_state, Some(MergeableState::Other(state)) if state == "clean");
    if observed.ci_head.as_deref() != Some(head) || observed.ci_green != Some(true) || !clean {
        return None;
    }
    if gated {
        let parsed = store::parse_target(target, true).ok()?;
        let cleared = crate::verdict::merge_gaps(root, &parsed, part, Some(pr), head, None).ok()?;
        if !cleared.gaps.is_empty() {
            return None;
        }
    }
    let part_flag = part
        .map(|part| format!(" --part {}", shell_word(part)))
        .unwrap_or_default();
    let mut action = Action::new(
        ActionKind::MergeReady,
        target,
        since,
        format!(
            "cm advance {} merged --pr {} --sha '<merge-sha>'{part_flag}",
            shell_word(target),
            shell_word(pr)
        ),
        format!(
            "merge {pr} at {}: the gate holds and CI is green, then record the merge",
            head.chars().take(12).collect::<String>()
        ),
    );
    action.part = part.map(str::to_owned);
    Some(action)
}

/// A free-text block older than 24 hours: lint's `block-stale`, without its journal-mention
/// silencer. An `until:` block names a machine condition and is `unblock-now` once it holds.
pub(crate) fn stale_block(step: &Step, now: Timestamp) -> Option<&str> {
    let Phase::Blocked(reason) = &step.track.state else {
        return None;
    };
    (!reason.starts_with("until:")
        && instant(&step.track.since)
            .is_some_and(|since| now.duration_since(since).as_secs() > 24 * 3600))
    .then_some(reason.as_str())
}

fn reached(step: &Step, phase: &Phase) -> bool {
    step.track.resting() != &Phase::Superseded
        && step
            .track
            .resting()
            .rank()
            .zip(phase.rank())
            .is_some_and(|(at, needed)| at >= needed)
}

/// One `released-undeployed` action per deploy that a line's newest release waits on, once it
/// has waited past `deploy_after_hours`. A release is deployed when a deploy step (a rollout, or a
/// PR in a repository no release line ships) gated on its tag, or a later one of its line, has
/// merged: a pin ends at its merge. Lines that no step deploys stay
/// quiet, as do lines whose deploy is in flight or already launchable. Off without the dial and
/// while paused.
struct Undeployed<'a> {
    line: String,
    release: Release,
    target: String,
    step: &'a Step,
    /// The deploy steps gated on this release's tag or a later one of its line.
    covering: Vec<(Release, String, &'a Step)>,
    /// The released versions of the line no merged deploy covers.
    waiting: usize,
}

/// Each line's newest release that no step gated on its tag, or a later one, has merged.
/// A deploy is a rollout, or a PR in a repository no release line ships (a homelab pin). A code
/// PR in a released repository that waits on a tag consumes the release; it deploys nothing.
fn deploy_predicate(units: &Units) -> impl Fn(&str, &Step) -> bool + '_ {
    let releases: Vec<(Release, &Step)> = steps(units)
        .filter_map(|(target, step)| Some((Release::of_target(&target)?, step)))
        .collect();
    // A pin or deploy step deploys by its name, as a rollout does, even when declared as code.
    move |target: &str, step: &Step| {
        step.work == Work::Rollout
            || target.ends_with("/pin")
            || target.ends_with("/deploy")
            || step
                .track
                .evidence
                .pr
                .as_deref()
                .and_then(GithubRepo::of_pr)
                .is_some_and(|repo| {
                    !releases
                        .iter()
                        .any(|(release, step)| step.ships_from(release, &repo))
                })
    }
}

fn undeployed_releases(units: &Units) -> Vec<Undeployed<'_>> {
    let deploys = deploy_predicate(units);
    let mut deployers: BTreeMap<String, Vec<(Release, String, &Step)>> = BTreeMap::new();
    let mut newest: BTreeMap<String, (Release, String, &Step)> = BTreeMap::new();
    for (target, step) in steps(units) {
        if let Some(release) = Release::of_target(&target) {
            if reached(step, &Phase::Released)
                && newest
                    .get(&release.line)
                    .is_none_or(|(known, _, _)| known.version < release.version)
            {
                newest.insert(release.line.clone(), (release, target, step));
            }
            continue;
        }
        if !deploys(&target, step) {
            continue;
        }
        for after in &step.after {
            if let After::Tag(release) = after {
                deployers.entry(release.line.clone()).or_default().push((
                    release.clone(),
                    target.clone(),
                    step,
                ));
            }
        }
    }
    let mut out = Vec::new();
    for (line, (release, target, step)) in newest {
        let Some(deploys) = deployers.get(&line) else {
            continue;
        };
        let covering: Vec<(Release, String, &Step)> = deploys
            .iter()
            .filter(|(gate, _, _)| gate.version >= release.version)
            .cloned()
            .collect();
        if reached(step, &Phase::Deployed)
            || covering
                .iter()
                .any(|(_, _, step)| reached(step, &Phase::Merged))
        {
            continue;
        }
        let waiting = steps(units)
            .filter(|(target, step)| {
                Release::of_target(target).is_some_and(|other| {
                    other.line == line
                        && reached(step, &Phase::Released)
                        && !deploys.iter().any(|(gate, _, deployer)| {
                            gate.version >= other.version && reached(deployer, &Phase::Merged)
                        })
                })
            })
            .count();
        out.push(Undeployed {
            line,
            release,
            target,
            step,
            covering,
            waiting,
        });
    }
    out
}

fn deploy_holds(root: &Path, units: &Units, inputs: &ActionInputs) -> Vec<Action> {
    let Some(limit) = inputs.deploy_after_hours else {
        return Vec::new();
    };
    if inputs.paused {
        return Vec::new();
    }
    let quota = inputs.quota();
    let mut held: BTreeMap<String, (String, Vec<String>, String, usize)> = BTreeMap::new();
    for Undeployed {
        line,
        release,
        target: release_target,
        step: release_step,
        covering,
        waiting,
    } in undeployed_releases(units)
    {
        if covering
            .iter()
            .any(|(_, _, step)| matches!(step.track.resting(), Phase::Launched | Phase::PrOpen))
        {
            continue;
        }
        let Some(since) = instant(&release_step.track.since) else {
            continue;
        };
        let hours = inputs.now.duration_since(since).as_secs_f64() / 3600.0;
        if hours <= limit {
            continue;
        }
        let drafted = covering
            .iter()
            .filter(|(_, _, step)| step.track.resting() == &Phase::Drafted)
            .min_by_key(|(gate, target, _)| (gate.version, target.clone()));
        let (key, command) = match drafted {
            Some((_, target, step)) => {
                let candidate = Candidate {
                    target: target.clone(),
                    step,
                    gates: gates(root, units, step),
                    kickoff: kickoff(root, target),
                };
                if candidate.step.track.state == Phase::Drafted
                    && candidate.lane() != Some(Lane::Mac)
                    && candidate.launchable(inputs.paused, quota)
                {
                    continue;
                }
                (target.clone(), waiting_command(root, target))
            }
            None => (
                release_target.clone(),
                format!("tools/cloud-env/deploy.sh {release_target}"),
            ),
        };
        let entry = held
            .entry(key)
            .or_insert_with(|| (release_step.track.since.clone(), Vec::new(), command, 0));
        if instant(&release_step.track.since) < instant(&entry.0) {
            entry.0.clone_from(&release_step.track.since);
        }
        entry.1.push(format!("{line}@{}", release.version));
        entry.3 += waiting;
    }
    held.into_iter()
        .map(|(target, (since, releases, command, waiting))| {
            let mut action = Action::new(
                ActionKind::ReleasedUndeployed,
                &target,
                &since,
                command,
                format!(
                    "{} released, not deployed (> {limit}h)",
                    releases.join(", ")
                ),
            );
            action.waiting_count = waiting;
            action
        })
        .collect()
}

/// One `merged-unreleased` action per repository with a release line whose oldest merge has
/// waited past `release_after_hours` for a release. Off without the dial and while paused; quiet
/// while a release of that line is in flight or a drafted one is launchable, which already has
/// its own line.
type ReleaseLines<'a> = BTreeMap<String, Vec<(String, &'a Step, Release)>>;

/// Each release line's merged, unreleased steps with their merge stamps: the merged tracks of
/// steps that release with their repository, as sync's `released` reads them.
fn unreleased_merges(
    units: &Units,
) -> (ReleaseLines<'_>, BTreeMap<String, BTreeMap<String, &str>>) {
    let mut lines: ReleaseLines = BTreeMap::new();
    for (target, step) in steps(units) {
        if let Some(release) = Release::of_target(&target) {
            lines
                .entry(release.line.clone())
                .or_default()
                .push((target, step, release));
        }
    }
    let mut held: BTreeMap<String, BTreeMap<String, &str>> = BTreeMap::new();
    for (target, step) in steps(units) {
        if step.work == Work::Rollout || Release::of_target(&target).is_some() {
            continue;
        }
        let tracks: Vec<&Track> = if step.parts.is_empty() {
            vec![&step.track]
        } else {
            step.parts.values().collect()
        };
        for track in tracks
            .into_iter()
            .filter(|track| track.state == Phase::Merged)
        {
            let Some(repo) = track.evidence.pr.as_deref().and_then(GithubRepo::of_pr) else {
                continue;
            };
            let Some(line) = lines
                .iter()
                .filter(|(_, releases)| {
                    releases
                        .iter()
                        .any(|(_, step, release)| step.ships_from(release, &repo))
                })
                .map(|(line, _)| line.as_str())
                .min_by_key(|line| (*line != repo.name(), *line))
            else {
                continue;
            };
            let merged = track.evidence.merged_at.as_deref().unwrap_or(&track.since);
            let oldest = held
                .entry(line.to_owned())
                .or_default()
                .entry(target.clone())
                .or_insert(merged);
            if instant(merged) < instant(oldest) {
                *oldest = merged;
            }
        }
    }
    (lines, held)
}

fn release_holds(root: &Path, units: &Units, inputs: &ActionInputs) -> Vec<Action> {
    let Some(limit) = inputs.release_after_hours else {
        return Vec::new();
    };
    if inputs.paused {
        return Vec::new();
    }
    let (lines, held) = unreleased_merges(units);
    let quota = inputs.quota();
    let mut out = Vec::new();
    for (line, merges) in held {
        let line = line.as_str();
        let releases = &lines[line];
        if releases.iter().any(|(_, step, _)| {
            matches!(
                step.track.resting(),
                Phase::Launched | Phase::PrOpen | Phase::Merged
            )
        }) {
            continue;
        }
        let Some((since, oldest)) = merges
            .values()
            .filter_map(|merged| Some((*merged, instant(merged)?)))
            .min_by_key(|(_, at)| *at)
        else {
            continue;
        };
        let hours = inputs.now.duration_since(oldest).as_secs_f64() / 3600.0;
        if hours <= limit {
            continue;
        }
        let drafted = releases
            .iter()
            .filter(|(_, step, _)| step.track.resting() == &Phase::Drafted)
            .min_by_key(|(_, _, release)| release.version);
        let (target, command) = match drafted {
            Some((target, step, _)) => {
                let candidate = Candidate {
                    target: target.clone(),
                    step,
                    gates: gates(root, units, step),
                    kickoff: kickoff(root, target),
                };
                if candidate.step.track.state == Phase::Drafted
                    && candidate.lane() != Some(Lane::Mac)
                    && candidate.launchable(inputs.paused, quota)
                {
                    continue;
                }
                (target.clone(), waiting_command(root, target))
            }
            None => {
                let newest = releases
                    .iter()
                    .max_by_key(|(_, _, release)| release.version);
                let mut command = format!(
                    "cm init {RELEASES}/{line}/'<version>' --lane {}",
                    newest
                        .and_then(|(_, step, _)| step.lane)
                        .unwrap_or(Lane::Coordinator)
                        .name()
                );
                for after in newest
                    .map(|(target, _, _)| target)
                    .into_iter()
                    .chain(merges.keys())
                {
                    command.push_str(&format!(" --after {}", shell_word(after)));
                }
                command.push_str(" --kickoff-file '<kickoff-file>'");
                (format!("{RELEASES}/{line}"), command)
            }
        };
        let count = merges.len();
        let mut action = Action::new(
            ActionKind::MergedUnreleased,
            &target,
            since,
            command,
            format!(
                "{count} merged {} unreleased on line {line}, oldest {:.1}h > {limit}h",
                if count == 1 { "step" } else { "steps" },
                (hours * 10.0).round() / 10.0
            ),
        );
        action.waiting_count = count;
        out.push(action);
    }
    out
}

fn actionable(
    root: &Path,
    units: &Units,
    inputs: &ActionInputs,
    stalled: &[Stall],
    verdicts: &crate::verdict::Verdicts,
) -> Vec<Action> {
    let verdict_prs: BTreeSet<_> = verdicts
        .reviews
        .iter()
        .filter_map(|row| row.pr.as_deref())
        .collect();
    let seen: BTreeMap<String, Observed> = store::read_json(&pulls_path(root)).unwrap_or_default();
    let mut actions = Vec::new();
    let releasing = release_holds(root, units, inputs);
    let deploying = deploy_holds(root, units, inputs);
    for candidate in candidates(root, units) {
        if candidate.launchable(inputs.paused, inputs.quota()) {
            actions.push(Action::new(
                ActionKind::LaunchableNotLaunched,
                &candidate.target,
                candidate.launchable_since(),
                launch_command(&candidate.target, candidate.lane(), candidate.finish()),
                format!(
                    "launchable in lane {}, not launched",
                    candidate.lane().map_or("—", Lane::name)
                ),
            ));
        }
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
        if let Some(action) = attempt_ended(&target, step, inputs.now) {
            actions.push(action);
        }
        // A reviewer wait is a timer `review_waits` shows, not a block the coordinator owes.
        if let Some(reason) = stale_block(step, inputs.now)
            .filter(|reason| !["EVIDENCE-BLOCKED", "REVIEW_BLOCKED"].contains(reason))
        {
            actions.push(Action::new(
                ActionKind::StaleBlock,
                &target,
                &step.track.since,
                unblock_command(&target, false),
                format!("blocked:{reason} for more than 24 h with no machine condition"),
            ));
        }
        for watch in step.unresolved_watches(root) {
            if watch.status(inputs.now, root) != crate::state::WatchStatus::Expired {
                continue;
            }
            actions.push(Action::new(
                ActionKind::ExpiredWatch,
                &target,
                &watch.deadline,
                format!(
                    "cm watch {} resolve {} --evidence '<watch-evidence-file>'",
                    shell_word(&target),
                    watch.id
                ),
                format!(
                    "watch #{} expired: unresolved ({}, deadline {}); or `cm watch {} withdraw {} --reason '<reason>'`",
                    watch.id,
                    watch.signal,
                    watch.deadline,
                    shell_word(&target),
                    watch.id
                ),
            ));
        }
        if let Some(attempt) = active_attempt(step)
            && inputs
                .coordinator
                .as_deref()
                .is_some_and(|label| label != crate::attempt::session_label(&attempt.coordinator))
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
        if let Some(attempt) = active_attempt(step)
            && attempt.lane.or(step.lane) == Some(Lane::ClaudeCloud)
            && inputs.turn_commands.for_lane(Lane::ClaudeCloud).is_some()
            && let Some(context) = crate::attempt::current_context(attempt)
            && context.over_cap()
        {
            actions.push(Action::new(
                ActionKind::ContextOver,
                &target,
                &context.read_at,
                format!("cm attempt {} begin session", shell_word(&target)),
                context_over(context, crate::attempt::FRESH_SESSION),
            ));
        }
        let mut review = |part: Option<&str>, track: &Track| {
            let Some(pr) = track.evidence.pr.as_deref() else {
                return;
            };
            if track.state != Phase::PrOpen {
                return;
            }
            // Only a script's pin owes no review and merges on clean, green CI alone; every
            // other step, a lane no model writes in included, needs its verdicts.
            let gated = !crate::commands::script_pin(&target, step);
            let name = part.map_or_else(|| target.clone(), |part| format!("{target}#{part}"));
            let observed = seen.get(pr).filter(|observed| observed.target == name);
            let head = observed
                .and_then(|observed| observed.head.as_deref())
                .or(track.evidence.sha.as_deref());
            let on_step: Vec<&crate::verdict::Recorded> = verdicts
                .reviews
                .iter()
                .filter(|review| {
                    review.step == target
                        && review.part.as_deref() == part
                        && review.pr.as_deref().is_none_or(|recorded| recorded == pr)
                })
                .collect();
            let at_head: Vec<&&crate::verdict::Recorded> = on_step
                .iter()
                .filter(|review| head.is_some_and(|head| review.at_head(head)))
                .collect();
            let reviewed_since = at_head
                .iter()
                .map(|review| review.at.as_str())
                .chain([track.since.as_str()])
                .max_by_key(|at| instant(at))
                .unwrap_or_default();
            // `paused = true` is the owner's stop, merges included; the Codex quota hold is not.
            // A blocked step merges none of its parts until it is unblocked.
            if !inputs.paused
                && !matches!(step.track.state, Phase::Blocked(_))
                && let Some(action) =
                    merge_ready(root, &target, part, pr, observed, reviewed_since, gated)
            {
                actions.push(action);
            }
            if !gated {
                return;
            }
            let detail = part.map_or_else(
                || format!("PR {pr}"),
                |part| format!("part {part}, PR {pr}"),
            );
            let head_word = head.map_or_else(
                || "its head".to_owned(),
                |head| head.chars().take(12).collect(),
            );
            let (since, command, detail) = if let Some(fix) = at_head
                .iter()
                .filter(|review| !review.unresolved().is_empty())
                .max_by_key(|review| review.number)
            {
                (
                    fix.at.clone(),
                    format!(
                        "cm attention --pr {} --worker '<worker-session>' --coordinator '<coordinator-session>'",
                        shell_word(pr)
                    ),
                    format!(
                        "{detail}: {} is FIX REQUIRED at {head_word} and no newer head",
                        fix.id()
                    ),
                )
            } else {
                // Every review the merge gate needs is owed at the head, not just the first.
                let missing = match head {
                    Some(head) => crate::verdict::missing_reviews(
                        verdicts,
                        &target,
                        head,
                        Some(pr),
                        part,
                        crate::verdict::model_lane(step.lane),
                    ),
                    None if verdict_prs.contains(pr) => Vec::new(),
                    None => vec!["any"],
                };
                // A review ledger row names no head, so it cannot stand for a verdict here.
                if missing.is_empty()
                    || crate::review::wait_marker(root, pr).is_some_and(|mark| mark.is_file())
                {
                    return;
                }
                // A head after a verdict is owed its review from that verdict on, not from pr-open.
                let since = on_step
                    .iter()
                    .filter(|review| review.pr.as_deref() == Some(pr))
                    .map(|review| review.at.as_str())
                    .chain([track.since.as_str()])
                    .max_by_key(|at| instant(at))
                    .unwrap_or_default()
                    .to_owned();
                let owed = if missing == ["any"] {
                    "no verdict".to_owned()
                } else {
                    format!("no {} verdict", missing.join(" and "))
                };
                (
                    since,
                    format!("cm brief {}", shell_word(&target)),
                    format!("{detail}: {owed} at {head_word}"),
                )
            };
            let mut action = Action::new(ActionKind::ReviewOwed, &target, &since, command, detail);
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
    for item in unblock_now(root, units, None) {
        let condition = item["condition"].as_str().unwrap_or_default();
        actions.push(Action::new(
            ActionKind::UnblockNow,
            item["target"].as_str().unwrap_or_default(),
            item["since"].as_str().unwrap_or_default(),
            item["command"].as_str().unwrap_or_default().to_owned(),
            format!("{condition} holds"),
        ));
    }
    actions.extend(drivers_over_cap(root, inputs));
    waiting_counts(root, units, &mut actions);
    actions.extend(releasing);
    actions.extend(deploying);
    for action in &mut actions {
        action.priority = priority_of(units, &action.target);
        let Some(step) = step(units, &action.target) else {
            continue;
        };
        action.lane = step.lane.map(Lane::name);
        action.weight = Some(weight(root, &action.target));
        let track = match action.part.as_deref() {
            Some(part) => step.parts.get(part),
            None => Some(&step.track),
        };
        let Some(pr) = track.and_then(|track| track.evidence.pr.clone()) else {
            continue;
        };
        let name = action.part.as_deref().map_or_else(
            || action.target.clone(),
            |part| format!("{}#{part}", action.target),
        );
        let observed = seen.get(&pr).filter(|observed| observed.target == name);
        action.head = observed
            .and_then(|observed| observed.head.clone())
            .or_else(|| track.and_then(|track| track.evidence.sha.clone()));
        action.base = observed.and_then(|observed| observed.base.clone());
        action.ci_green = observed
            .filter(|observed| observed.ci_head.is_some() && observed.ci_head == action.head)
            .and_then(|observed| observed.ci_green);
        action.pr = Some(pr);
    }
    actions.sort_by(|a, b| {
        (
            a.priority,
            Reverse(a.waiting_count),
            &a.target,
            &a.part,
            a.kind,
        )
            .cmp(&(
                b.priority,
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
    for action in actions
        .iter()
        .filter(|action| !action.kind.printed_by_next())
    {
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

/// The actions CARD counts. The Codex quota is an environment reading and CARD changes only
/// with a fact, so CARD leaves it out.
fn card_actions(root: &Path, units: &Units, now: Timestamp) -> Vec<Action> {
    let mut inputs = ActionInputs::read(root, now);
    inputs.codex_quota = None;
    let stalled = stalls(units, &inputs);
    // Unlike next, sync has not historically refused an unreadable review ledger.
    let verdicts = crate::verdict::load(root).unwrap_or_default();
    actionable(root, units, &inputs, &stalled, &verdicts)
}

fn oldest<'a>(items: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    items
        .into_iter()
        .filter_map(|(what, since)| Some((instant(since)?, what, since)))
        .min()
        .map_or_else(
            || "—".to_owned(),
            |(_, what, since)| format!("{what} since {since}"),
        )
}

/// CARD's `Actionable` line and its guarantee lines, in a fixed `Label: value` form the silence
/// watchdog parses (README, CARD guarantees).
pub(crate) fn card_lines(
    root: &Path,
    units: &Units,
    now: Timestamp,
    launchable: usize,
) -> Vec<String> {
    let actions = card_actions(root, units, now);
    let first = actions
        .iter()
        .filter_map(|action| Some((instant(&action.since)?, &action.since)))
        .min()
        .map_or("—", |(_, since)| since.as_str());
    let day_ago = now - jiff::SignedDuration::from_hours(24);
    // A deploy is a deploy step (a rollout, a pin, or a tag-gated PR in a repository no release
    // line ships) reaching merged: the pin ends at its merge. A multipart step counts when the step
    // itself moves, never on one part's move.
    let deploys_step = deploy_predicate(units);
    let deploy_steps: BTreeSet<String> = steps(units)
        .filter(|(target, step)| {
            deploys_step(target, step)
                && (step.work == Work::Rollout
                    || step
                        .after
                        .iter()
                        .any(|after| matches!(after, After::Tag(_))))
        })
        .map(|(target, _)| target)
        .collect();
    let deploys: Vec<(&str, Timestamp, &str)> = units
        .values()
        .flat_map(|unit| &unit.history)
        .filter(|moved| matches!(moved.to, Phase::Merged | Phase::Deployed))
        .filter_map(|moved| {
            // Only the step's own move counts: one part merging is not the step's deploy.
            let target = moved.target.as_str();
            deploy_steps.contains(target).then_some((
                target,
                instant(&moved.at)?,
                moved.at.as_str(),
            ))
        })
        .collect();
    let deployed: BTreeSet<&str> = deploys
        .iter()
        .filter(|(_, at, _)| *at >= day_ago)
        .map(|(target, _, _)| *target)
        .collect();
    let last_deploy = deploys
        .iter()
        .map(|(_, at, stamp)| (*at, *stamp))
        .max()
        .map_or("—", |(_, stamp)| stamp);
    let first_launchable = actions
        .iter()
        .filter(|action| action.kind == ActionKind::LaunchableNotLaunched)
        .filter_map(|action| Some((instant(&action.since)?, action.since.as_str())))
        .min()
        .map_or("—", |(_, since)| since);
    let first_launch = |target: &str| {
        let unit = split(target).and_then(|(unit, _)| units.get(unit))?;
        unit.history
            .iter()
            .find(|moved| moved.target == target && moved.to == Phase::Launched)
            .map(|moved| moved.at.clone())
    };
    let open: Vec<(String, String)> = steps(units)
        .filter(|(target, step)| {
            Release::of_target(target).is_none()
                && step.finish(false) == Finish::Shipped
                && matches!(step.track.resting(), Phase::Launched | Phase::PrOpen)
        })
        .map(|(target, step)| {
            let since = first_launch(&target).unwrap_or_else(|| step.track.since.clone());
            (target, since)
        })
        .collect();
    let (_, merges) = unreleased_merges(units);
    let undeployed: Vec<(String, String)> = undeployed_releases(units)
        .into_iter()
        .map(|held| {
            (
                format!("{}@{}", held.line, held.release.version),
                held.step.track.since.clone(),
            )
        })
        .collect();
    vec![
        format!("Actionable: {} (oldest: {first})", actions.len()),
        format!("Deployed in 24 h: {} (last: {last_deploy})", deployed.len()),
        format!(
            "Oldest open code card: {}",
            oldest(
                open.iter()
                    .map(|(target, since)| (target.as_str(), since.as_str()))
            )
        ),
        format!(
            "Oldest merge-ready: {}",
            oldest(
                actions
                    .iter()
                    .filter(|action| action.kind == ActionKind::MergeReady)
                    .map(|action| (action.target.as_str(), action.since.as_str()))
            )
        ),
        format!(
            "Oldest unreleased merge: {}",
            oldest(
                merges
                    .values()
                    .flatten()
                    .map(|(target, since)| (target.as_str(), *since))
            )
        ),
        format!(
            "Oldest undeployed tag: {}",
            oldest(
                undeployed
                    .iter()
                    .map(|(tag, since)| (tag.as_str(), since.as_str()))
            )
        ),
        format!("Launchable at sync: {launchable} (oldest: {first_launchable})"),
    ]
}

fn campaign_lanes(root: &Path, drivers: &Drivers) -> BTreeSet<String> {
    let briefs = fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let lane = name.strip_prefix("LANE-")?.strip_suffix(".md")?;
            (!lane.is_empty()).then(|| lane.to_owned())
        });
    briefs.chain(drivers.drivers.keys().cloned()).collect()
}

fn entry_lane<'a>(title: &str, lanes: &'a BTreeSet<String>) -> Option<&'a str> {
    let title = title.to_lowercase();
    lanes
        .iter()
        .find(|lane| {
            let lane = lane.to_lowercase();
            title.starts_with(&format!("lane {lane}:"))
                || title.starts_with(&format!("{lane} lane:"))
        })
        .map(String::as_str)
}

fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|token| token.eq_ignore_ascii_case(word))
}

// The resume record the owner's resume directive (2026-11-25T23:36Z) prescribes: an entry with this
// exact title whose body states the assignment as a clause of its own. Any other coordinator
// restart, and a sentence that recalls or plans the unpause, is none; a line break inside a
// paragraph ends no clause, so a wrapped recollection stays one sentence. A question states nothing
// and code quotes (a fence, or a line indented four columns), so neither is the record. The record
// is read by its form: a clause of that form that plans or restates the unpause still counts, since
// without history cm cannot tell it from the transition.
const RESUME_TITLE: &str = "Coordinator: resumed after quota reset";
const ASSIGNMENT: [&str; 5] = ["paused", "=", "false", "in", "limits.toml"];

fn list_item(line: &str) -> Option<&str> {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let rest = match digits {
        0 => line.strip_prefix(['-', '*', '+']),
        1..=9 => line[digits..].strip_prefix(['.', ')']),
        _ => None,
    }?;
    rest.starts_with([' ', '\t']).then(|| rest.trim_start())
}

fn fence_run(line: &str) -> Option<(char, usize)> {
    let mark = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = line.len() - line.trim_start_matches(mark).len();
    (len >= 3).then_some((mark, len))
}

fn indent(line: &str) -> usize {
    line.chars()
        .map_while(|c| match c {
            ' ' => Some(1),
            '\t' => Some(4),
            _ => None,
        })
        .sum()
}

// A fence closes only on a run of its own character at least as long as the opener, with nothing
// after it and indented at most three columns past its container (a list item's content column
// when the opener is on the item's line, else the margin): anything else is fenced content, as
// CommonMark reads it. A non-blank line dedented out of the item ends the item and can open another
// fence that later lines would have to be matched against, so from that line on nothing in the body
// is read: an unclosed fence in a list item hides the rest of the body.
fn states_unpause(body: &str) -> bool {
    let mut fence: Option<(char, usize, usize)> = None;
    let mut hidden = false;
    let text = body
        .lines()
        .map(|raw| {
            let depth = indent(raw);
            let line = raw.trim();
            if hidden {
                return ";".to_owned();
            }
            if let Some((mark, open, at)) = fence {
                if !line.is_empty() && depth < at {
                    hidden = true;
                } else if (at..=at + 3).contains(&depth)
                    && line.len() >= open
                    && line.chars().all(|c| c == mark)
                {
                    fence = None;
                }
                return ";".to_owned();
            }
            if depth >= 4 {
                return ";".to_owned();
            }
            let item = list_item(line);
            if let Some((mark, open)) = fence_run(item.unwrap_or(line)) {
                let column = item.map_or(0, |rest| depth + line.len() - rest.len());
                fence = Some((mark, open, column));
                return ";".to_owned();
            }
            match item {
                Some(item) => format!("; {item}"),
                None if line.is_empty() => ";".to_owned(),
                None => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .replace('`', "")
        .replace('=', " = ")
        .to_lowercase();
    let mut clause = Vec::new();
    for word in text.split_whitespace() {
        let bare = word.trim_end_matches(['.', ';', '!', '?']);
        if !bare.is_empty() {
            clause.push(bare);
        }
        if bare.len() < word.len() {
            if clause == ASSIGNMENT && !word[bare.len()..].contains('?') {
                return true;
            }
            clause.clear();
        }
    }
    clause == ASSIGNMENT
}

fn handoff_paths(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| c.is_whitespace() || "`()[],;\"'".contains(c))
        .map(|word| word.trim_end_matches(['.', ':']))
        .filter(|word| {
            word.starts_with("handoff/") && word.ends_with(".md") && !word.contains("..")
        })
}

struct Pause {
    at: Option<Timestamp>,
    stamp: String,
    title: String,
    handoffs: Vec<String>,
}

#[derive(Default)]
struct LaneRecord {
    pause: Option<Pause>,
    latest: Option<Timestamp>,
}

struct ResumeRecord {
    at: Timestamp,
    stamp: String,
}

enum Reading {
    Other,
    Pause(String),
    Resume(ResumeRecord, String),
}

#[derive(Default)]
struct LaneJournal {
    lanes: BTreeMap<String, LaneRecord>,
    resume: Option<ResumeRecord>,
}

impl LaneJournal {
    // One pass that keeps per lane only its open pause and newest stamp: the journal only grows.
    fn read(root: &Path, lanes: &BTreeSet<String>) -> Option<Self> {
        let file = fs::File::open(root.join(store::JOURNAL)).ok()?;
        let mut journal = Self::default();
        let mut reading = Reading::Other;
        for line in BufReader::new(file).lines() {
            let line = line.ok()?;
            let Some((stamp, title)) = crate::journal::split_heading(&line) else {
                match &mut reading {
                    Reading::Pause(lane) => {
                        if let Some(pause) = journal
                            .lanes
                            .get_mut(lane)
                            .and_then(|record| record.pause.as_mut())
                        {
                            pause
                                .handoffs
                                .extend(handoff_paths(&line).map(str::to_owned));
                        }
                    }
                    Reading::Resume(_, body) => {
                        body.push_str(&line);
                        body.push('\n');
                    }
                    Reading::Other => {}
                }
                continue;
            };
            journal.close(std::mem::replace(&mut reading, Reading::Other));
            let at = instant(stamp);
            if let Some(lane) = entry_lane(title, lanes) {
                let record = journal.lanes.entry(lane.to_owned()).or_default();
                record.latest = record.latest.max(at);
                // `resumed` wins: "resumed after the paused window" is no pause.
                if has_word(title, "resumed") {
                    record.pause = None;
                } else if has_word(title, "paused") {
                    record.pause = Some(Pause {
                        at,
                        stamp: stamp.to_owned(),
                        title: title.to_owned(),
                        handoffs: Vec::new(),
                    });
                    reading = Reading::Pause(lane.to_owned());
                }
            } else if title == RESUME_TITLE
                && let Some(at) = at
            {
                let record = ResumeRecord {
                    at,
                    stamp: stamp.to_owned(),
                };
                reading = Reading::Resume(record, String::new());
            }
        }
        journal.close(reading);
        Some(journal)
    }

    fn close(&mut self, reading: Reading) {
        if let Reading::Resume(record, body) = reading
            && states_unpause(&body)
            && self
                .resume
                .as_ref()
                .is_none_or(|newest| record.at >= newest.at)
        {
            self.resume = Some(record);
        }
    }
}

fn lane_handoff(root: &Path, paths: &[String], lane: &str) -> Option<(String, Option<String>)> {
    let role = format!("role=lane-{lane}");
    paths.iter().find_map(|path| {
        let text = fs::read_to_string(root.join(path)).ok()?;
        let line = text.lines().find(|line| line.starts_with("Handoff:"))?;
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.contains(&role.as_str()).then(|| {
            let lineage = fields
                .iter()
                .find_map(|field| field.strip_prefix("lineage="))
                .map(str::to_owned);
            (path.clone(), lineage)
        })
    })
}

fn lane_paused(root: &Path, inputs: &ActionInputs) -> Vec<Action> {
    if inputs.paused {
        return Vec::new();
    }
    let lanes = campaign_lanes(root, &inputs.drivers);
    if lanes.is_empty() {
        return Vec::new();
    }
    let Some(LaneJournal { lanes, resume }) = LaneJournal::read(root, &lanes) else {
        return Vec::new();
    };
    if lanes.values().all(|record| record.pause.is_none()) {
        return Vec::new();
    }
    let (unpaused, source) = match limits::unpause(root) {
        Ok(unpause) => (
            unpause.at,
            format!(
                "LIMITS.toml commit {}",
                unpause.sha.chars().take(12).collect::<String>()
            ),
        ),
        Err(why) => {
            let Some(resume) = resume else {
                return Vec::new();
            };
            (
                resume.at,
                format!("journal {} `{RESUME_TITLE}`; {why}", resume.stamp),
            )
        }
    };
    let mut out = Vec::new();
    for (lane, record) in &lanes {
        let Some(pause) = &record.pause else {
            continue;
        };
        if pause.at.is_none_or(|at| at >= unpaused) || record.latest.is_some_and(|at| at > unpaused)
        {
            continue;
        }
        let handoff = lane_handoff(root, &pause.handoffs, lane);
        let session = inputs
            .drivers
            .drivers
            .get(lane)
            .map(|driver| driver.session.clone())
            .or_else(|| handoff.as_ref().and_then(|(_, lineage)| lineage.clone()));
        let from = handoff.as_ref().map_or_else(
            || format!("its journal entry {}", pause.stamp),
            |(path, _)| path.clone(),
        );
        let title = pause.title.split_once(':').map_or("", |(prefix, _)| prefix);
        let command = format!(
            "send_message {} {}",
            session
                .as_deref()
                .map_or_else(|| "'<lane-session>'".to_owned(), shell_word),
            shell_word(&format!(
                "LIMITS.toml is paused = false: resume lane {lane} from {from}, then journal `{title}: resumed`"
            ))
        );
        let mut action = Action::new(
            ActionKind::LanePaused,
            lane,
            &unpaused.to_string(),
            command,
            format!(
                "paused at {} (`{}`), no lane entry after the unpause ({source})",
                pause.stamp,
                pause.title.replace('`', "'")
            ),
        );
        action.handoff = handoff.map(|(path, _)| path);
        out.push(action);
    }
    out
}

pub(crate) fn next_act(
    root: &Path,
    target: Option<&Target>,
    lane: Option<Lane>,
    now: Timestamp,
    forge: Option<&dyn crate::forge::Forge>,
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
        let mut actions = confirm_merge_ready(
            action_scope(
                actionable(root, &units, &inputs, &stalled, &verdicts),
                &units,
                target,
                lane,
            ),
            forge,
        );
        // A campaign lane is no step, so a `--target` or `--lane` view leaves the line out.
        if target.is_none() && lane.is_none() {
            actions.extend(lane_paused(root, &inputs));
        }
        actions
    };
    actions.sort_by_key(|action| (action.priority, action.kind.act_priority()));
    for action in &actions {
        outcome.line(format!("{} — `{}`", action.summary(), action.command));
    }
    outcome.field("actionable", json!(actions));
    // Executors hold `merge-ready` while the owner has unread queue items, as merges refuse then.
    outcome.field(
        "owner_queue_unread",
        json!(crate::ledger::unread_owner_queue(root)?),
    );
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
    /// Whether CI was green when the last read completed at `ci_head`; cleared by a later read
    /// that found checks still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_green: Option<bool>,
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
    pub launchable: Vec<(String, Option<Lane>, Finish)>,
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
    // CARD's Next holds no Codex quota: that hold is an environment reading, and CARD changes
    // only with a fact. `cm next` applies it.
    let mut launchable = Vec::new();
    let mut waiting = Vec::new();
    for candidate in candidates(root, units) {
        if candidate.launchable(paused, None) {
            launchable.push((
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
        launchable,
        waiting,
    }
}

pub fn next(root: &Path, target: Option<&Target>, lane: Option<Lane>) -> Result<Outcome> {
    next_at(root, target, lane, Timestamp::now())
}

/// `cm next` without GitHub: every `merge-ready` fails closed, since none can be confirmed.
pub fn next_at(
    root: &Path,
    target: Option<&Target>,
    lane: Option<Lane>,
    now: Timestamp,
) -> Result<Outcome> {
    next_with(root, target, lane, now, None)
}

/// A `merge-ready` action survives only a fresh read of its PR: still open at the head it was
/// reviewed at, `mergeable_state` clean, and CI green there now. Sync's cached observation lets
/// the line be found; this read is what lets an executor merge on it. A failed read, or no
/// forge, drops the line.
fn confirm_merge_ready(
    actions: Vec<Action>,
    forge: Option<&dyn crate::forge::Forge>,
) -> Vec<Action> {
    actions
        .into_iter()
        .filter(|action| {
            if action.kind != ActionKind::MergeReady {
                return true;
            }
            let (Some(forge), Some(pr), Some(head)) = (forge, &action.pr, &action.head) else {
                return false;
            };
            let Ok(pull) = forge.pull(pr) else {
                return false;
            };
            let open = pull.state == crate::forge::PullState::Open
                && pull.head.as_ref().map(crate::forge::CommitSha::as_str) == Some(head.as_str())
                && matches!(&pull.mergeable_state, Some(MergeableState::Other(state)) if state == "clean");
            open && GithubRepo::of_pr(pr).is_some_and(|repo| {
                forge
                    .checks(&repo, &crate::forge::CommitSha::new(head.clone()))
                    .is_ok_and(|checks| crate::forge::ci_green(&checks))
            })
        })
        .collect()
}

pub fn next_with(
    root: &Path,
    target: Option<&Target>,
    lane: Option<Lane>,
    now: Timestamp,
    forge: Option<&dyn crate::forge::Forge>,
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
    let mut actions = confirm_merge_ready(
        action_scope(
            actionable(root, &units, &inputs, &stalled, &verdicts),
            &units,
            target,
            lane,
        ),
        forge,
    );
    // As in `--act`, so both list the same actions; CARD.md, rendered by sync, leaves it out.
    if target.is_none() && lane.is_none() {
        actions.extend(lane_paused(root, &inputs));
    }
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
        .filter(|candidate| candidate.launchable(paused, quota))
        .collect();
    let waiting: Vec<&Candidate> = all
        .iter()
        .filter(in_lane)
        .filter(|candidate| !candidate.launchable(paused, quota))
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
        let n = crate::ledger::queue_item_number(number)
            .ok_or("owner-queue condition needs a positive item number")?;
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
        // No item number names nothing to wait for, never a met condition.
        return crate::ledger::queue_item_number(number).is_some_and(|number| {
            root.join("OWNER-QUEUE.md").is_file()
                && !crate::ledger::open_items(root).contains(&number)
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
