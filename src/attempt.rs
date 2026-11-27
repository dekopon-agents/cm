use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::commands::{closed, write_unit};
use crate::evidence::CampaignFile;
use crate::host::{Host, stamp};
use crate::journal;
use crate::outcome::{Outcome, Violation};
use crate::state::{Attempt, AttemptOutcome, Ended, External, Lane, ObservedContext, ObservedTurn};
use crate::store::{self, CM_DIR, Target, is_name, is_subcampaign_name, lock};

#[derive(Debug, Clone)]
pub enum Act {
    Status,
    Restart {
        lane: Lane,
        file: Option<PathBuf>,
    },
    Claim {
        coordinator: String,
        expect: u32,
    },
    Begin {
        kind: String,
    },
    Record {
        kind: String,
        id: String,
    },
    End {
        outcome: AttemptOutcome,
        file: Option<PathBuf>,
    },
}

pub(crate) fn coordinator_label(given: &Option<String>) -> Option<String> {
    given.clone().or_else(|| {
        let value = std::env::var("CLAUDE_CODE_REMOTE_SESSION_ID").ok()?;
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(session_label(value))
        }
    })
}

pub(crate) fn session_label(value: &str) -> String {
    match value.strip_prefix("cse_") {
        Some(id) => format!("session_{id}"),
        None => value.to_owned(),
    }
}

fn launch_metadata(kind: &str) -> bool {
    matches!(kind, "branch" | "repo")
}

/// Read launch metadata from this attempt only. A newer unfinished record must not revive
/// an older value of the same kind.
pub(crate) fn recorded_id<'a>(attempt: &'a Attempt, kind: &str) -> Option<&'a str> {
    attempt
        .actions
        .iter()
        .rev()
        .find(|action| action.kind == kind)?
        .id
        .as_deref()
}

pub(crate) struct RemoteTurn<'a> {
    pub thread: &'a str,
    pub turn: &'a str,
}

impl RemoteTurn<'_> {
    pub(crate) fn observation<'a>(&self, attempt: &'a Attempt) -> Option<&'a ObservedTurn> {
        attempt
            .observed_turn
            .as_ref()
            .filter(|read| read.thread == self.thread && read.turn == self.turn)
    }
}

/// An in-flight registration of a newer thread or turn supersedes an earlier observation.
fn turn_actions(attempt: &Attempt, lane: Lane) -> Option<(&External, Option<&External>)> {
    let mut actions = attempt.actions.iter().rev();
    let is_thread =
        |kind: &str| kind == "thread" || (lane == Lane::ClaudeCloud && kind == "session");
    let turn = actions.find(|action| action.kind == "turn" || is_thread(&action.kind))?;
    if turn.kind != "turn" {
        return None;
    }
    let thread = actions.find(|action| is_thread(&action.kind));
    Some((turn, thread))
}

pub(crate) const FRESH_SESSION: &str = "start a fresh session from the step's handoff, open findings, head and the diff since the last reviewed head";

fn over_cap(context: &ObservedContext) -> String {
    format!(
        "session {} is at {}k, over the {}k context cap (read {})",
        context.session,
        context.kilotokens(),
        crate::limits::CONTEXT_CAP_TOKENS / 1000,
        context.read_at
    )
}

// The context sync recorded for the session the attempt now addresses, if it read that one.
pub(crate) fn current_context(attempt: &Attempt) -> Option<&ObservedContext> {
    let session = attempt
        .actions
        .iter()
        .rev()
        .find(|action| matches!(action.kind.as_str(), "thread" | "session"))?
        .id
        .as_deref()?;
    attempt
        .observed_context
        .as_ref()
        .filter(|context| context.session == session)
}

pub(crate) fn turn_began(attempt: &Attempt, lane: Lane) -> Option<&str> {
    turn_actions(attempt, lane).map(|(turn, _)| turn.began.as_str())
}

pub(crate) fn remote_turn(attempt: &Attempt, lane: Lane) -> Option<RemoteTurn<'_>> {
    let (turn, thread) = turn_actions(attempt, lane)?;
    Some(RemoteTurn {
        thread: thread?.id.as_deref()?,
        turn: turn.id.as_deref()?,
    })
}

pub(crate) fn check_coordinator(full: &str, coordinator: &str) -> Option<Violation> {
    (coordinator.trim().is_empty() || coordinator.contains(['\n', '\r']))
        .then(|| Violation::new(full, "attempt-claim", "--coordinator is one non-empty line"))
}

/// Past the deadline; a deadline cm cannot read counts as past.
pub(crate) fn expired(attempt: &Attempt, now: Timestamp) -> bool {
    attempt.deadline.as_deref().is_some_and(|deadline| {
        deadline
            .parse::<Timestamp>()
            .map_or(true, |deadline| now >= deadline)
    })
}

/// What a coordinator holding only the committed state does next, as `(code, instruction)`.
pub fn plan(attempt: &Attempt, now: Timestamp) -> (&'static str, String) {
    if let Some(ended) = &attempt.ended {
        return ("ended", format!("ended {} at {}", ended.outcome, ended.at));
    }
    if let Some(action) = attempt.actions.last().filter(|action| action.id.is_none()) {
        return (
            "unknown",
            format!(
                "{} began at {} under generation {} and returned no id: the outcome is unknown; never dispatch again, find it in the runtime, then `cm attempt <step> end unknown` or record its id",
                action.kind, action.began, action.generation
            ),
        );
    }
    if expired(attempt, now) {
        let admitted: Vec<_> = attempt
            .actions
            .iter()
            .rev()
            .filter(|action| !launch_metadata(&action.kind))
            .filter_map(|action| {
                action.id.as_ref().map(|id| {
                    if action.kind == "cancel" {
                        format!("inspect recorded cancellation {id}")
                    } else {
                        format!("cancel {} {id}", action.kind)
                    }
                })
            })
            .collect();
        let cancel = if admitted.is_empty() {
            "nothing was admitted to cancel".to_owned()
        } else {
            format!("inspect the recorded work and {}", admitted.join("; "))
        };
        return (
            "expired",
            format!(
                "the cap ended at {}: {cancel}, retrieve its partial results and `cm attempt <step> end partial --evidence <file>`",
                attempt.deadline.as_deref().unwrap_or_default()
            ),
        );
    }
    match attempt
        .actions
        .iter()
        .rev()
        .find(|action| !launch_metadata(&action.kind))
    {
        None => (
            "dispatch",
            "nothing reached the runtime: `cm attempt <step> begin <kind>`, then dispatch"
                .to_owned(),
        ),
        Some(action) => (
            "resume",
            format!(
                "resume {} {}; record each new id before the next external action",
                action.kind,
                action.id.as_deref().unwrap_or_default()
            ),
        ),
    }
}

pub fn attempt(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    claim: Option<&str>,
    act: &Act,
) -> Result<Outcome> {
    let full = format!(
        "{}/{}",
        target.unit,
        target.step.as_deref().unwrap_or_default()
    );
    let rate_limit = match act {
        Act::End { .. } => host.github_rate_limit(),
        _ => None,
    };
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = Vec::new();
    if !matches!(act, Act::Status) {
        violations.extend(journal::check(root, &campaign));
        violations.extend(closed(root, &campaign));
    }
    if matches!(
        act,
        Act::Restart {
            lane: Lane::Mac,
            ..
        }
    ) {
        violations.push(crate::plan::mac_dispatch_refusal(&full));
        return Ok(Outcome::refused(violations));
    }
    let dir = root.join(&target.unit);
    let mut unit = if store::is_unit(&dir) {
        store::load_unit(&dir)?
    } else {
        violations.push(Violation::new(
            &target.unit,
            "unit-missing",
            "no unit state",
        ));
        return Ok(Outcome::refused(violations));
    };
    let step_name = target.step.clone().unwrap_or_default();
    let step = unit.steps.get(&step_name);
    if matches!(act, Act::Restart { .. }) && step.is_some_and(|step| step.lane == Some(Lane::Mac)) {
        violations.push(crate::plan::mac_dispatch_refusal(&full));
        return Ok(Outcome::refused(violations));
    }
    let Some(mut attempt) = step.and_then(|step| step.attempt.clone()) else {
        violations.push(Violation::new(
            &full,
            "attempt-missing",
            "no remote attempt is recorded; `cm launch` a remote-lane step first",
        ));
        return Ok(Outcome::refused(violations));
    };
    let now = host.now();
    let at = stamp(now);
    // Only a value the last sync read refuses: a stale one is printed by `cm next --act` alone.
    let capped = (attempt.lane.or(step.and_then(|step| step.lane)) == Some(Lane::ClaudeCloud)
        && crate::limits::TurnCommands::read(root)
            .for_lane(Lane::ClaudeCloud)
            .is_some())
    .then_some(attempt.observed_context.as_ref())
    .flatten()
    .filter(|context| context.fresh() && context.over_cap());
    let word = crate::commands::shell_word(&full);
    let fenced = |given: &str| {
        Violation::new(
            &full,
            "attempt-fenced",
            format!(
                "`{given}` is fenced: {} holds attempt {} (generation {}, {}, claimed at {})",
                attempt.claim,
                attempt.id,
                attempt.generation,
                attempt.coordinator,
                attempt.claimed_at
            ),
        )
    };
    let pending = attempt.actions.last().filter(|action| action.id.is_none());
    let ended = attempt.ended.is_some();
    let title = match act {
        Act::Status => None,
        Act::Restart { lane, .. } => {
            if attempt
                .ended
                .as_ref()
                .is_none_or(|end| end.outcome != AttemptOutcome::Unknown)
            {
                violations.push(Violation::new(
                    &full,
                    "attempt-restart",
                    "only `end unknown` permits restart",
                ));
            }
            if expired(&attempt, now) {
                violations.push(Violation::new(
                    &full,
                    "attempt-deadline",
                    plan(&attempt, now).1,
                ));
            }
            let launches = unit.steps[&step_name].launches + 1;
            Some(format!(
                "relaned {} → {full}#{launches} ({lane})",
                attempt.id
            ))
        }
        Act::Claim {
            coordinator,
            expect,
        } => {
            if *expect != attempt.generation {
                violations.push(fenced(&format!("generation {expect}")));
            }
            violations.extend(check_coordinator(&full, coordinator));
            Some(format!(
                "claim {} for generation {} ({coordinator})",
                attempt.id,
                attempt.generation + 1
            ))
        }
        Act::Begin { kind } => {
            if pending.is_some() {
                violations.push(Violation::new(
                    &full,
                    "attempt-unknown",
                    plan(&attempt, now).1,
                ));
            }
            if !is_name(kind) {
                violations.push(Violation::new(
                    &full,
                    "attempt-kind",
                    format!("`{kind}` is not a name such as thread, turn, session or cancel"),
                ));
            }
            if kind != "cancel" && expired(&attempt, now) {
                violations.push(Violation::new(
                    &full,
                    "attempt-deadline",
                    plan(&attempt, now).1,
                ));
            }
            if kind == "turn"
                && let Some(context) = capped.filter(|_| current_context(&attempt).is_some())
            {
                violations.push(Violation::new(
                    &full,
                    "attempt-context-over",
                    format!(
                        "{}: never resume it; {FRESH_SESSION}: `cm attempt {word} begin session`, create the session, then `cm attempt {word} record session '<fresh-session>'`",
                        over_cap(context)
                    ),
                ));
            }
            Some(format!(
                "{} begins {kind} (generation {})",
                attempt.id, attempt.generation
            ))
        }
        Act::Record { kind, id } => {
            if pending.is_none_or(|action| action.kind != *kind) {
                violations.push(Violation::new(
                    &full,
                    "attempt-record",
                    format!("no {kind} has begun without an id; `cm attempt <step> begin {kind}` comes first"),
                ));
            }
            if id.is_empty() || id.contains(char::is_whitespace) {
                violations.push(Violation::new(&full, "attempt-record", "an id is one word"));
            }
            if matches!(kind.as_str(), "thread" | "session")
                && let Some(context) = capped.filter(|context| context.session == *id)
            {
                violations.push(Violation::new(
                    &full,
                    "attempt-context-over",
                    format!(
                        "{}: never resume it; {FRESH_SESSION}, then `cm attempt {word} record {kind} '<fresh-session>'`",
                        over_cap(context)
                    ),
                ));
            }
            Some(format!(
                "{} {kind} {id} (generation {})",
                attempt.id, attempt.generation
            ))
        }
        Act::End { outcome, .. } => {
            if let Some(action) = pending.filter(|_| *outcome != AttemptOutcome::Unknown) {
                violations.push(Violation::new(
                    &full,
                    "attempt-unknown",
                    format!(
                        "{} began at {} and returned no id, so nobody knows whether it ran: only `end unknown` closes this attempt",
                        action.kind, action.began
                    ),
                ));
            }
            Some(format!("end {}: {outcome}", attempt.id))
        }
    };
    if !matches!(act, Act::Status | Act::Claim { .. }) {
        match claim {
            Some(given) if given == attempt.claim && !attempt.claim.is_empty() => {}
            Some(given) => violations.push(fenced(given)),
            None => violations.push(Violation::new(
                &full,
                "attempt-fenced",
                "pass --claim, the token `cm launch` or `cm attempt claim` printed",
            )),
        }
    }
    if ended && !matches!(act, Act::Status | Act::Restart { .. }) {
        violations.push(Violation::new(
            &full,
            "attempt-ended",
            plan(&attempt, now).1,
        ));
    }
    let mut evidence = None;
    if let Act::End { file, .. } | Act::Restart { file, .. } = act {
        match (file, act) {
            (Some(file), _) => match CampaignFile::resolve(root, file) {
                Ok(found) => evidence = Some(found),
                Err(error) => {
                    violations.push(Violation::new(&full, "evidence-file", error.to_string()))
                }
            },
            (
                None,
                Act::End {
                    outcome: AttemptOutcome::Partial,
                    ..
                },
            ) => violations.push(Violation::new(
                &full,
                "evidence-missing",
                "a partial attempt keeps its partial results: pass --evidence <file>",
            )),
            (None, _) => {}
        }
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    match act {
        Act::Status => {}
        Act::Restart { lane, .. } => {
            attempt.ended = Some(Ended {
                at: at.clone(),
                outcome: AttemptOutcome::Relaned,
                evidence: evidence
                    .clone()
                    .or_else(|| attempt.ended.as_ref().and_then(|end| end.evidence.clone())),
            });
            let step = unit.steps.get_mut(&step_name).expect("attempt has a step");
            step.attempt_history.push(attempt.clone());
            step.launches += 1;
            if step.track.state == crate::state::Phase::Launched {
                step.track.since.clone_from(&at);
            }
            attempt.id = format!("{full}#{}", step.launches);
            attempt.lane = Some(*lane);
            attempt.actions.clear();
            attempt.observed_turn = None;
            attempt.observed_context = None;
            attempt.ended = None;
        }
        Act::Claim { coordinator, .. } => {
            attempt.generation += 1;
            attempt.coordinator.clone_from(coordinator);
            attempt.claimed_at.clone_from(&at);
            attempt.mint(now);
        }
        Act::Begin { kind } => attempt.actions.push(External {
            kind: kind.clone(),
            began: at.clone(),
            generation: attempt.generation,
            id: None,
        }),
        Act::Record { id, .. } => {
            if let Some(action) = attempt.actions.last_mut() {
                action.id = Some(id.clone());
            }
        }
        Act::End { outcome, .. } => {
            attempt.ended = Some(Ended {
                at: at.clone(),
                outcome: *outcome,
                evidence: evidence.clone(),
            })
        }
    }
    let mut outcome = Outcome::default();
    if let Some(title) = title {
        if let Some(step) = unit.steps.get_mut(&step_name) {
            step.attempt = Some(attempt.clone());
        }
        write_unit(&dir, &unit)?;
        let mut body = format!(
            "- generation: {}\n- evidence: {}\n",
            attempt.generation,
            evidence
                .as_ref()
                .map_or("—".to_owned(), ToString::to_string)
        );
        if let Some(line) = &rate_limit {
            body.push_str(&format!("- {line}\n"));
        }
        let heading = journal::heading(&at, &format!("attempt {full}: {title}"));
        journal::append(
            root,
            &journal::entry(
                &at,
                &format!("attempt {full}: {title}"),
                if matches!(
                    act,
                    Act::Begin { .. } | Act::Record { .. } | Act::Restart { .. }
                ) {
                    None
                } else {
                    Some(&body)
                },
            ),
        )?;
        outcome.field("journal", heading);
    }
    let (code, next) = plan(&attempt, now);
    outcome.line(format!("{}: claim {}; {next}", attempt.id, attempt.claim));
    if let Some(line) = rate_limit {
        outcome.line(line);
    }
    outcome.field("claim", attempt.claim.clone());
    outcome.field("target", full);
    outcome.field("plan", code);
    outcome.field("next", next);
    outcome.field("attempt", serde_json::to_value(&attempt)?);
    Ok(outcome)
}

pub const LANE_DRIVERS: &str = "lane-drivers.json";

// Unknown fields are refused, not dropped on the next write: a newer cm may have written a
// registry this one cannot apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Drivers {
    #[serde(default)]
    pub drivers: BTreeMap<String, Driver>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Driver {
    pub session: String,
    pub units: Vec<String>,
    pub at: String,
    pub by: String,
}

fn drivers_path(root: &Path) -> PathBuf {
    root.join(CM_DIR).join(LANE_DRIVERS)
}

pub fn covers(prefix: &str, target: &str) -> bool {
    target
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Drivers {
    pub fn read(root: &Path) -> std::result::Result<Self, Violation> {
        let path = drivers_path(root);
        if !path.exists() {
            return Ok(Self::default());
        }
        store::read_json(&path).map_err(|error| {
            Violation::new(
                registry_path(),
                "lane-drivers-unreadable",
                format!(
                    "{error:#}; if a newer cm wrote it, upgrade cm; otherwise restore it with `git checkout HEAD -- {}`, or delete it if it was never committed, then `cm lane driver list` and `cm lane driver add` any driver it lacks",
                    registry_path()
                ),
            )
        })
    }

    pub fn driving(&self, session: &str, target: &str) -> Option<&str> {
        // An explicit `--coordinator` is stored as given, and a hand-merged registry entry is
        // read as written, so either side may carry the `cse_` form.
        let session = session_label(session);
        self.drivers
            .iter()
            .find(|(_, driver)| {
                session_label(&driver.session) == session
                    && driver.units.iter().any(|unit| covers(unit, target))
            })
            .map(|(lane, _)| lane.as_str())
    }

    fn lane_of(&self, session: &str) -> Option<&str> {
        self.drivers
            .iter()
            .find(|(_, driver)| session_label(&driver.session) == session)
            .map(|(lane, _)| lane.as_str())
    }
}

fn unit_prefix(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.trim_matches('/').split('/').collect();
    (parts.len() <= 3
        && is_subcampaign_name(parts[0])
        && parts[1..].iter().all(|part| is_name(part)))
    .then(|| parts.join("/"))
}

fn registry_path() -> String {
    format!("{CM_DIR}/{LANE_DRIVERS}")
}

fn check_lane(lane: &str, out: &mut Vec<Violation>) {
    if !is_name(lane) {
        out.push(Violation::new(
            registry_path(),
            "lane-driver-lane",
            format!(
                "`{lane}` is not a lane name: pass the <name> of LANE-<name>.md, in letters, digits, `.`, `_` and `-`"
            ),
        ));
    }
}

fn check_registrar(by: &str, out: &mut Vec<Violation>) {
    if by.trim().is_empty() || by.contains(['\n', '\r']) {
        out.push(Violation::new(
            registry_path(),
            "lane-driver-registrar",
            "--coordinator is one non-empty line: pass the label of the coordinator's session",
        ));
    }
}

// Journal first, so no registry write goes unjournaled.
fn journal_then_write(
    root: &Path,
    at: &str,
    title: &str,
    body: &str,
    drivers: &Drivers,
) -> std::result::Result<String, Violation> {
    let journal = crate::ledger::journal_line(root, at, title, body).map_err(|error| {
        Violation::new(
            store::JOURNAL,
            "lane-driver-journal-write",
            format!(
                "`{title}` changed nothing: its journal entry failed ({error:#}), so the registry is as it was; re-run the same command once JOURNAL.md is writable"
            ),
        )
    })?;
    std::fs::create_dir_all(root.join(CM_DIR))
        .map_err(anyhow::Error::from)
        .and_then(|()| store::write_json(&drivers_path(root), drivers))
        .map_err(|error| {
            Violation::new(
                registry_path(),
                "lane-driver-write",
                format!(
                    "`{title}` is journaled ({journal}), but writing {} failed ({error:#}), so the registry is as it was; re-run the same command, which journals it again",
                    registry_path()
                ),
            )
        })?;
    Ok(journal)
}

fn units_line(units: &[String]) -> String {
    units.join(", ")
}

pub fn driver_add(
    host: &dyn Host,
    root: &Path,
    lane: &str,
    session: &str,
    units: &[String],
    by: &str,
) -> Result<Outcome> {
    let path = registry_path();
    let session = session_label(session);
    let session = session.as_str();
    let by = session_label(by.trim());
    let by = by.as_str();
    let mut violations = Vec::new();
    check_lane(lane, &mut violations);
    if !is_name(session) {
        violations.push(Violation::new(
            &path,
            "lane-driver-session",
            format!(
                "`{session}` is not a session label: pass --session <label>, the driver's session as its claims record it (such as session_01…), in letters, digits, `.`, `_` and `-`"
            ),
        ));
    }
    if units.is_empty() {
        violations.push(Violation::new(
            &path,
            "lane-driver-units",
            "no unit is given: pass --unit <NN-name>[/<unit>[/<step>]] once for each unit the driver claims",
        ));
    }
    let mut prefixes = Vec::new();
    for unit in units {
        match unit_prefix(unit) {
            Some(prefix) => prefixes.push(prefix),
            None => violations.push(Violation::new(
                &path,
                "lane-driver-units",
                format!(
                    "`{unit}` is not <NN-name>[/<unit>[/<step>]]: pass --unit with a subcampaign, unit or step the driver claims"
                ),
            )),
        }
    }
    prefixes.sort();
    prefixes.dedup();
    check_registrar(by, &mut violations);
    if by == session {
        violations.push(Violation::new(
            &path,
            "lane-driver-registrar",
            format!(
                "`{session}` is the session registering it: a driver never registers itself; the coordinator runs `cm lane driver add`"
            ),
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let (_lock, mut violations) = crate::ledger::guard(root)?;
    let mut drivers = match Drivers::read(root) {
        Ok(drivers) => drivers,
        Err(violation) => {
            violations.push(violation);
            return Ok(Outcome::refused(violations));
        }
    };
    if let Some(driven) = drivers.lane_of(by) {
        violations.push(Violation::new(
            &path,
            "lane-driver-registrar",
            format!(
                "`{by}` is registered as the driver of lane `{driven}`, and only the coordinator registers a driver: if this session is the coordinator now, `cm lane driver remove {driven}` first; otherwise run `cm lane driver add` from the coordinator's session"
            ),
        ));
    }
    let replaced = drivers.drivers.get(lane).cloned();
    if replaced
        .as_ref()
        .is_some_and(|driver| driver.session == session && driver.units == prefixes)
    {
        violations.push(Violation::new(
            &path,
            "lane-driver-unchanged",
            format!(
                "lane `{lane}` already has driver `{session}` for {}; `cm lane driver list` shows it",
                units_line(&prefixes)
            ),
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    let driver = Driver {
        session: session.to_owned(),
        units: prefixes,
        at: at.clone(),
        by: by.to_owned(),
    };
    drivers.drivers.insert(lane.to_owned(), driver.clone());
    let units = crate::plan::load_units(root)?;
    let undeclared: Vec<&String> = driver
        .units
        .iter()
        .filter(|prefix| {
            !units.iter().any(|(unit, state)| {
                covers(prefix, unit)
                    || state
                        .steps
                        .keys()
                        .any(|step| covers(prefix, &format!("{unit}/{step}")))
            })
        })
        .collect();
    let mut body = format!(
        "- session: {session}\n- units: {}\n- registered by: {by}\n",
        units_line(&driver.units)
    );
    let mut line = format!(
        "lane {lane}: driver {session} for {}",
        units_line(&driver.units)
    );
    if let Some(old) = &replaced {
        body.push_str(&format!(
            "- replaces: {} for {}\n",
            old.session,
            units_line(&old.units)
        ));
        line.push_str(&format!("; replaces {}", old.session));
    }
    let journal = match journal_then_write(
        root,
        &at,
        &format!("lane driver add {lane}: {session}"),
        &body,
        &drivers,
    ) {
        Ok(journal) => journal,
        Err(violation) => return Ok(Outcome::refused(vec![violation])),
    };
    let mut outcome = Outcome::default();
    outcome.line(line);
    for prefix in &undeclared {
        outcome.line(format!(
            "warning: no declared unit or step is under {prefix} yet; it covers steps declared there later"
        ));
    }
    outcome.field("undeclared", serde_json::to_value(&undeclared)?);
    outcome.field("lane", lane);
    outcome.field("driver", serde_json::to_value(&driver)?);
    outcome.field("replaced", serde_json::to_value(&replaced)?);
    outcome.field("journal", journal);
    Ok(outcome)
}

pub fn driver_remove(host: &dyn Host, root: &Path, lane: &str, by: &str) -> Result<Outcome> {
    let path = registry_path();
    let by = session_label(by.trim());
    let by = by.as_str();
    let mut violations = Vec::new();
    check_lane(lane, &mut violations);
    check_registrar(by, &mut violations);
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let (_lock, mut violations) = crate::ledger::guard(root)?;
    let mut drivers = match Drivers::read(root) {
        Ok(drivers) => drivers,
        Err(violation) => {
            violations.push(violation);
            return Ok(Outcome::refused(violations));
        }
    };
    let Some(driver) = drivers.drivers.remove(lane) else {
        let registered = if drivers.drivers.is_empty() {
            "none is registered".to_owned()
        } else {
            format!(
                "registered: {}",
                drivers
                    .drivers
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        violations.push(Violation::new(
            &path,
            "lane-driver-unknown",
            format!(
                "lane `{lane}` has no registered driver ({registered}); `cm lane driver list` shows them"
            ),
        ));
        return Ok(Outcome::refused(violations));
    };
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = stamp(host.now());
    let journal = match journal_then_write(
        root,
        &at,
        &format!("lane driver remove {lane}: {}", driver.session),
        &format!(
            "- units: {}\n- removed by: {by}\n",
            units_line(&driver.units)
        ),
        &drivers,
    ) {
        Ok(journal) => journal,
        Err(violation) => return Ok(Outcome::refused(vec![violation])),
    };
    let mut outcome = Outcome::default();
    outcome.line(format!(
        "lane {lane}: removed driver {} for {}",
        driver.session,
        units_line(&driver.units)
    ));
    outcome.field("lane", lane);
    outcome.field("driver", serde_json::to_value(&driver)?);
    outcome.field("journal", journal);
    Ok(outcome)
}

pub fn driver_list(root: &Path) -> Result<Outcome> {
    let drivers = match Drivers::read(root) {
        Ok(drivers) => drivers,
        Err(violation) => return Ok(Outcome::refused(vec![violation])),
    };
    let mut outcome = Outcome::default();
    let mut items = Vec::new();
    for (lane, driver) in &drivers.drivers {
        outcome.line(format!(
            "{lane}: {} for {} (registered {} by {})",
            driver.session,
            units_line(&driver.units),
            driver.at,
            driver.by
        ));
        let mut item = serde_json::to_value(driver)?;
        item["lane"] = lane.as_str().into();
        items.push(item);
    }
    if items.is_empty() {
        outcome.line("no lane driver is registered");
    }
    outcome.field("drivers", items);
    Ok(outcome)
}
