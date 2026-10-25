use std::path::{Path, PathBuf};

use anyhow::Result;
use jiff::Timestamp;

use crate::commands::{closed, write_unit};
use crate::evidence::CampaignFile;
use crate::host::{Host, stamp};
use crate::journal;
use crate::outcome::{Outcome, Violation};
use crate::state::{Attempt, AttemptOutcome, Ended, External};
use crate::store::{self, Target, is_name, lock};

#[derive(Debug, Clone)]
pub enum Act {
    Status,
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
    let last = attempt.actions.last();
    if let Some(ended) = &attempt.ended {
        return ("ended", format!("ended {} at {}", ended.outcome, ended.at));
    }
    if let Some(action) = last.filter(|action| action.id.is_none()) {
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
    match last {
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
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = Vec::new();
    if !matches!(act, Act::Status) {
        violations.extend(journal::check(root, &campaign));
        violations.extend(closed(root, &campaign));
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
    let Some(mut attempt) = unit
        .steps
        .get(&step_name)
        .and_then(|step| step.attempt.clone())
    else {
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
        Act::Claim {
            coordinator,
            expect,
        } => {
            if *expect != attempt.generation {
                violations.push(fenced(&format!("generation {expect}")));
            }
            if coordinator.trim().is_empty() || coordinator.contains('\n') {
                violations.push(Violation::new(
                    &full,
                    "attempt-claim",
                    "--coordinator is one non-empty line",
                ));
            }
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
            Some(format!("{} begins {kind}", attempt.id))
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
            Some(format!("{} {kind} {id}", attempt.id))
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
    if ended && !matches!(act, Act::Status) {
        violations.push(Violation::new(
            &full,
            "attempt-ended",
            plan(&attempt, now).1,
        ));
    }
    let mut evidence = None;
    if let Act::End { outcome, file } = act {
        match (file, outcome) {
            (Some(file), _) => match CampaignFile::resolve(root, file) {
                Ok(found) => evidence = Some(found),
                Err(error) => {
                    violations.push(Violation::new(&full, "evidence-file", error.to_string()))
                }
            },
            (None, AttemptOutcome::Partial) => violations.push(Violation::new(
                &full,
                "evidence-missing",
                "a partial attempt keeps its partial results: pass --evidence <file>",
            )),
            (None, AttemptOutcome::Complete | AttemptOutcome::Unknown) => {}
        }
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    match act {
        Act::Status => {}
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
        let body = format!(
            "- generation: {}\n- evidence: {}\n",
            attempt.generation,
            evidence
                .as_ref()
                .map_or("—".to_owned(), ToString::to_string)
        );
        let heading = journal::heading(&at, &format!("attempt {full}: {title}"));
        journal::append(
            root,
            &journal::entry(&at, &format!("attempt {full}: {title}"), Some(&body)),
        )?;
        outcome.field("journal", heading);
    }
    let (code, next) = plan(&attempt, now);
    outcome.line(format!("{}: claim {}; {next}", attempt.id, attempt.claim));
    outcome.field("claim", attempt.claim.clone());
    outcome.field("target", full);
    outcome.field("plan", code);
    outcome.field("next", next);
    outcome.field("attempt", serde_json::to_value(&attempt)?);
    Ok(outcome)
}
