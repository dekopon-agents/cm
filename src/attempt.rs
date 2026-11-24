use std::path::{Path, PathBuf};

use anyhow::Result;
use jiff::Timestamp;

use crate::commands::{closed, write_unit};
use crate::evidence::CampaignFile;
use crate::host::{Host, stamp};
use crate::journal;
use crate::outcome::{Outcome, Violation};
use crate::state::{Attempt, AttemptOutcome, Ended, External, Lane, ObservedTurn};
use crate::store::{self, Target, is_name, lock};

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
            Some(match value.strip_prefix("cse_") {
                Some(id) => format!("session_{id}"),
                None => value.to_owned(),
            })
        }
    })
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
fn expired(attempt: &Attempt, now: Timestamp) -> bool {
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
