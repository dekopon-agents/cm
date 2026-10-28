use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use crate::commands::write_unit;
use crate::evidence::CampaignFile;
use crate::forge::{CheckState, CommitSha, Conclusion, Forge, GithubRepo};
use crate::host::{Host, stamp};
use crate::ledger::{guard, journal_line, one_line};
use crate::outcome::{Outcome, Violation};
use crate::plan::{self, Units};
use crate::state::{
    After, Evidence, KnownRed, Move, Phase, Pin, Resolved, Revision, Signal, Step, Watch,
    WatchStatus, is_commit, same_commit,
};
use crate::store::{self, Target};

fn full(target: &Target) -> String {
    format!(
        "{}/{}",
        target.unit,
        target.step.as_deref().unwrap_or_default()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Uncovered {
    NoException,
    NotAFailure(Conclusion),
    GreenAtBase,
    BaseUnknown,
    Closed(After),
}

impl fmt::Display for Uncovered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Uncovered::NoException => f.write_str("no known-red exception names it"),
            Uncovered::NotAFailure(state) => write!(
                f,
                "`{state}` is not a test failure; an exception never covers a cancelled, timed-out or unrun check"
            ),
            Uncovered::GreenAtBase => f.write_str(
                "it passes at the PR's base, so this change broke it; an exception never covers a regression",
            ),
            Uncovered::BaseUnknown => f.write_str(
                "it did not fail at the PR's base (absent, unread or not a completed failure), so it may be a regression",
            ),
            Uncovered::Closed(until) => write!(f, "its exception closed when {until} held"),
        }
    }
}

pub fn verify(check: &CheckState, base: Option<&[CheckState]>) -> Result<(), Uncovered> {
    match check.state {
        Conclusion::Failure => {}
        Conclusion::Success | Conclusion::Neutral | Conclusion::Skipped | Conclusion::Other(_) => {
            return Err(Uncovered::NotAFailure(check.state.clone()));
        }
    }
    let before: Vec<&CheckState> = base
        .ok_or(Uncovered::BaseUnknown)?
        .iter()
        .filter(|before| before.name == check.name)
        .collect();
    if before.is_empty() {
        return Err(Uncovered::BaseUnknown);
    }
    let mut verdict = Ok(());
    for before in before {
        match before.state {
            Conclusion::Failure => {}
            Conclusion::Success | Conclusion::Neutral | Conclusion::Skipped => {
                return Err(Uncovered::GreenAtBase);
            }
            Conclusion::Other(_) => verdict = Err(Uncovered::BaseUnknown),
        }
    }
    verdict
}

/// Closed once its condition held; a gate the condition's step later reopens, such as through
/// stale pinned evidence, does not reopen the exception.
pub fn is_open(units: &Units, exception: &KnownRed) -> bool {
    let gate = plan::gate(units, &exception.until);
    exception.closed.is_none()
        && !gate
            .track
            .as_ref()
            .is_some_and(|track| exception.until.met_by(track, gate.finish))
}

pub fn mark_closed(units: &mut Units, at: &str) -> Vec<(String, String)> {
    let snapshot = units.clone();
    let mut out = Vec::new();
    for (unit_name, unit) in units.iter_mut() {
        for (name, step) in unit.steps.iter_mut() {
            for exception in step.known_red.iter_mut() {
                if exception.closed.is_none() && !is_open(&snapshot, exception) {
                    exception.closed = Some(at.to_owned());
                    out.push((
                        unit_name.clone(),
                        format!(
                            "- known-red {unit_name}/{name}: `{}` at {} closed, {} holds; the check blocks again",
                            exception.check, if exception.standing { "any head" } else { &exception.commit }, exception.until
                        ),
                    ));
                }
            }
        }
    }
    out
}

pub fn cover<'a>(
    exceptions: &'a [KnownRed],
    units: &Units,
    head: &str,
    check: &CheckState,
    base: Option<&[CheckState]>,
    standing_base: Option<&[CheckState]>,
) -> Result<&'a KnownRed, Uncovered> {
    let named: Vec<&KnownRed> = exceptions
        .iter()
        .filter(|exception| {
            (exception.standing || exception.commit.eq_ignore_ascii_case(head))
                && exception.check == check.name
        })
        .collect();
    let mut why = Uncovered::NoException;
    for exception in named {
        match verify(
            check,
            if exception.standing {
                standing_base
            } else {
                base
            },
        ) {
            Err(error) => why = error,
            Ok(()) if is_open(units, exception) => return Ok(exception),
            Ok(()) => why = Uncovered::Closed(exception.until.clone()),
        }
    }
    Err(why)
}

pub(crate) fn standing_names(units: &Units, exceptions: &[KnownRed]) -> Vec<String> {
    exceptions
        .iter()
        .chain(
            units
                .values()
                .flat_map(|unit| unit.steps.values())
                .flat_map(|step| &step.known_red)
                .filter(|exception| exception.campaign),
        )
        .filter(|exception| exception.standing && is_open(units, exception))
        .map(|exception| exception.check.clone())
        .collect()
}

pub fn used_line(head: &str, exception: &KnownRed) -> String {
    format!(
        "- known-red: {} failed at {head}, accepted by the exception owned by {} until {} ({})",
        exception.check, exception.owner, exception.until, exception.reason
    )
}

pub struct KnownRedArgs<'a> {
    pub commit: &'a str,
    pub standing: bool,
    pub campaign: bool,
    pub check: &'a str,
    pub reason: &'a str,
    pub owner: &'a str,
    pub until: &'a str,
}

fn pull_requests(step: &Step) -> Vec<String> {
    step.track
        .evidence
        .pr
        .iter()
        .chain(
            step.parts
                .values()
                .filter_map(|part| part.evidence.pr.as_ref()),
        )
        .cloned()
        .collect()
}

fn red_on_github(
    forge: &dyn Forge,
    full: &str,
    step: &Step,
    commit: &str,
    check: &str,
) -> Vec<Violation> {
    let prs = pull_requests(step);
    let mut unread = Vec::new();
    let mut refused = Vec::new();
    for url in &prs {
        let pull = match forge.pull(url) {
            Ok(pull) => pull,
            Err(error) => {
                unread.push(format!("{url}: {error}"));
                continue;
            }
        };
        let Some(head) = pull
            .head
            .filter(|head| commit.is_empty() || same_commit(head.as_str(), commit))
        else {
            continue;
        };
        let Some(repo) = GithubRepo::of_pr(url) else {
            continue;
        };
        let violations = red_at_head(
            forge,
            full,
            &repo,
            &head,
            pull.base.as_ref(),
            check,
            commit.is_empty(),
        );
        if violations.is_empty() || !commit.is_empty() {
            return violations;
        }
        refused.extend(violations);
    }
    if !refused.is_empty() {
        return refused;
    }
    let message = if prs.is_empty() {
        format!("{full} records no PR; the exception names a PR head")
    } else if unread.is_empty() {
        format!("`{commit}` is not the head of {}", prs.join(", "))
    } else {
        format!("cannot verify `{commit}` on GitHub: {}", unread.join("; "))
    };
    vec![Violation::new(full, "known-red-commit", message)]
}

fn red_at_head(
    forge: &dyn Forge,
    full: &str,
    repo: &GithubRepo,
    head: &CommitSha,
    base: Option<&CommitSha>,
    check: &str,
    standing: bool,
) -> Vec<Violation> {
    let checks = match forge.checks(repo, head) {
        Ok(checks) => checks,
        Err(error) => {
            return vec![Violation::new(
                full,
                "known-red-unverified",
                format!("cannot read CI at {head}: {error}"),
            )];
        }
    };
    let Some(named) = checks.iter().find(|found| found.name == check) else {
        return vec![Violation::new(
            full,
            "known-red-check",
            format!("GitHub reports no check `{check}` at {head}"),
        )];
    };
    let before = base.and_then(|base| {
        if standing {
            forge
                .latest_completed_checks(repo, base, &[check.to_owned()])
                .ok()
        } else {
            forge.checks(repo, base).ok()
        }
    });
    match verify(named, before.as_deref()) {
        Ok(()) => Vec::new(),
        Err(why) => vec![Violation::new(
            full,
            "known-red-refused",
            format!("`{check}` at {head}: {why}"),
        )],
    }
}

pub fn known_red(
    host: &dyn Host,
    forge: &dyn Forge,
    root: &Path,
    target: &Target,
    args: &KnownRedArgs,
) -> Result<Outcome> {
    let full = full(target);
    let units = plan::load_units(root)?;
    let mut violations = Vec::new();
    for (field, text) in [
        ("--check", args.check),
        ("--reason", args.reason),
        ("--owner", args.owner),
    ] {
        one_line(field, text, &mut violations, &full);
    }
    if (args.standing && !args.commit.is_empty())
        || (!args.standing && !(args.commit.len() == 40 && is_commit(args.commit)))
        || (args.campaign && !args.standing)
    {
        violations.push(Violation::new(
            &full,
            "known-red-commit",
            format!(
                "`{}`: use --commit with all 40 hex digits, or --standing without --commit; campaign scope requires --standing",
                args.commit
            ),
        ));
    }
    let until = match After::parse(args.until, &target.unit) {
        Ok(until) => Some(until),
        Err(message) => {
            violations.push(Violation::new(&full, "after-parse", message));
            None
        }
    };
    if let Some(until) = &until {
        if plan::step(&units, &until.target()).is_none() {
            violations.push(Violation::new(
                &full,
                "after-unknown",
                format!(
                    "`{until}` names {}, which is not declared; the closing condition is a cm-checkable event",
                    until.target()
                ),
            ));
        } else if plan::gate(&units, until).met() {
            violations.push(Violation::new(
                &full,
                "known-red-closed",
                format!("the closing condition {until} already holds"),
            ));
        }
    }
    match plan::step(&units, &full) {
        None => violations.push(Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        )),
        Some(step) if step.track.state.is_final() => violations.push(Violation::new(
            &full,
            "known-red-state",
            format!("{full} is {}", step.track.state),
        )),
        Some(step) if violations.is_empty() => violations.extend(red_on_github(
            forge,
            &full,
            step,
            &args.commit.to_ascii_lowercase(),
            args.check,
        )),
        Some(_) => {}
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let Some(until) = until else {
        return Ok(Outcome::refused(violations));
    };
    let (_lock, violations) = guard(root)?;
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let dir = root.join(&target.unit);
    let mut unit = store::load_unit(&dir)?;
    let at = stamp(host.now());
    let exception = KnownRed {
        commit: args.commit.to_ascii_lowercase(),
        standing: args.standing,
        campaign: args.campaign,
        check: args.check.trim().to_owned(),
        reason: args.reason.trim().to_owned(),
        owner: args.owner.trim().to_owned(),
        until,
        at: at.clone(),
        closed: None,
    };
    let Some(step) = unit
        .steps
        .get_mut(target.step.as_deref().unwrap_or_default())
    else {
        return Ok(Outcome::refused(vec![Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        )]));
    };
    step.known_red.push(exception.clone());
    write_unit(&dir, &unit)?;
    let mut body = format!(
        "- check: {}\n- commit: {}\n- reason: {}\n- owner: {}\n- closes when: {}\n",
        exception.check,
        if exception.standing {
            "any head"
        } else {
            &exception.commit
        },
        exception.reason,
        exception.owner,
        exception.until
    );
    if exception.standing {
        body.push_str(&format!(
            "- standing: true\n- scope: {}\n",
            if exception.campaign {
                "campaign"
            } else {
                "step"
            }
        ));
    }
    let journal = journal_line(root, &at, &format!("known-red {full}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!(
        "{full}: known-red {} at {} until {}",
        exception.check,
        if exception.standing {
            "any head"
        } else {
            &exception.commit
        },
        exception.until
    ));
    outcome.field("target", full);
    outcome.field("exception", serde_json::to_value(&exception)?);
    outcome.field("journal", journal);
    Ok(outcome)
}

pub fn watch_add(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    signal: &str,
    deadline: &str,
) -> Result<Outcome> {
    let full = full(target);
    let (_lock, mut violations) = guard(root)?;
    let signal = signal
        .parse::<Signal>()
        .map_err(|message| violations.push(Violation::new(&full, "watch-signal", message)))
        .ok();
    let now = host.now();
    let deadline = match plan::instant(deadline) {
        Some(instant) if instant > now => Some(instant),
        Some(_) => {
            violations.push(Violation::new(
                &full,
                "watch-deadline",
                format!("the deadline {deadline} has already passed"),
            ));
            None
        }
        None => {
            violations.push(Violation::new(
                &full,
                "watch-deadline",
                format!("`{deadline}` is not an RFC 3339 instant such as 2026-10-27T06:00Z"),
            ));
            None
        }
    };
    let dir = root.join(&target.unit);
    let mut unit = store::load_unit(&dir)?;
    let step = unit
        .steps
        .get_mut(target.step.as_deref().unwrap_or_default());
    match &step {
        None => violations.push(Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        )),
        Some(step) if step.track.state.is_final() => violations.push(Violation::new(
            &full,
            "watch-state",
            format!("{full} is {}", step.track.state),
        )),
        Some(_) => {}
    }
    let (Some(step), Some(signal), Some(deadline), true) =
        (step, signal, deadline, violations.is_empty())
    else {
        return Ok(Outcome::refused(violations));
    };
    let at = stamp(now);
    let id = step.watches.iter().map(|watch| watch.id).max().unwrap_or(0) + 1;
    let watch = Watch {
        id,
        signal,
        deadline: stamp(deadline),
        added: at.clone(),
        resolved: None,
        expired: None,
    };
    step.watches.push(watch.clone());
    write_unit(&dir, &unit)?;
    let body = format!(
        "- signal: {}\n- deadline: {}\n- holds {full} below deployed until `cm watch {full} resolve {id} --evidence <path>`\n",
        watch.signal, watch.deadline
    );
    let journal = journal_line(root, &at, &format!("watch {full}#{id}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!(
        "{full}#{id}: watching {} until {}",
        watch.signal, watch.deadline
    ));
    outcome.field("target", full);
    outcome.field("id", id);
    outcome.field("deadline", watch.deadline);
    outcome.field("journal", journal);
    Ok(outcome)
}

pub fn watch_resolve(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    id: u32,
    file: &Path,
) -> Result<Outcome> {
    let full = full(target);
    let (_lock, mut violations) = guard(root)?;
    let evidence = CampaignFile::resolve(root, file)
        .map_err(|error| violations.push(Violation::new(&full, "evidence-file", error.to_string())))
        .ok();
    let dir = root.join(&target.unit);
    let mut unit = store::load_unit(&dir)?;
    let now = host.now();
    let watch = unit
        .steps
        .get_mut(target.step.as_deref().unwrap_or_default())
        .and_then(|step| step.watches.iter_mut().find(|watch| watch.id == id));
    let status = watch.as_ref().map(|watch| watch.status(now, root));
    match status {
        None => violations.push(Violation::new(
            &full,
            "watch-unknown",
            format!("{full} has no watch {id}"),
        )),
        Some(WatchStatus::Resolved) => violations.push(Violation::new(
            &full,
            "watch-resolved",
            format!("{full}#{id} is already resolved"),
        )),
        Some(WatchStatus::Open | WatchStatus::Expired) => {}
    }
    let (Some(watch), Some(evidence), true) = (watch, evidence, violations.is_empty()) else {
        return Ok(Outcome::refused(violations));
    };
    let at = stamp(now);
    watch.resolved = Some(Resolved {
        at: at.clone(),
        evidence: evidence.clone(),
    });
    let late = match status {
        Some(WatchStatus::Expired) => format!(
            "- landed after its deadline {}; it was expired: unresolved until now\n",
            watch.deadline
        ),
        Some(WatchStatus::Open | WatchStatus::Resolved) | None => String::new(),
    };
    let mut body = format!("- signal: {}\n- evidence: {evidence}\n{late}", watch.signal);
    let mut rolled = Vec::new();
    if let Some(step) = unit
        .steps
        .get_mut(target.step.as_deref().unwrap_or_default())
    {
        rolled = step.roll_up_all(&at, &Evidence::default(), root);
    }
    for (from, to) in &rolled {
        body.push_str(&format!(
            "- {full}: {from} → {to}, every part has reached {to}\n"
        ));
        unit.history.push(Move {
            at: at.clone(),
            target: full.clone(),
            from: from.clone(),
            to: to.clone(),
            evidence: Evidence::default(),
        });
    }
    write_unit(&dir, &unit)?;
    let journal = journal_line(root, &at, &format!("watch {full}#{id} resolved"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{full}#{id}: resolved by {evidence}"));
    outcome.field("target", full);
    outcome.field("id", id);
    outcome.field("evidence", evidence.to_string());
    outcome.field("journal", journal);
    Ok(outcome)
}

pub fn head(root: &Path, units: &Units, target: &str) -> Option<String> {
    let (name, part) = match target.split_once('#') {
        Some((name, part)) => (name, Some(part)),
        None => (target, None),
    };
    let step = plan::step(units, name)?;
    let track = match part {
        Some(part) => step.parts.get(part)?,
        None if step.parts.is_empty() => &step.track,
        None => return None,
    };
    match track.resting() {
        Phase::PrOpen => {}
        Phase::Drafted
        | Phase::Launched
        | Phase::Merged
        | Phase::Released
        | Phase::Deployed
        | Phase::Done
        | Phase::Superseded
        | Phase::Blocked(_) => return None,
    }
    let observed = track.evidence.pr.as_deref().and_then(|pr| {
        store::read_json::<BTreeMap<String, plan::Observed>>(&plan::pulls_path(root))
            .ok()?
            .remove(pr)?
            .head
    });
    observed.or_else(|| track.evidence.sha.clone())
}

pub fn revision(
    root: &Path,
    units: &Units,
    unit: &str,
    flag: &str,
    text: &str,
) -> Result<Revision, Violation> {
    let (name, sha) = match text.rsplit_once('@') {
        Some((name, sha)) => (name, Some(sha)),
        None => (text, None),
    };
    let (name, part) = match name.split_once('#') {
        Some((name, part)) => (name, Some(part)),
        None => (name, None),
    };
    let refuse = |message: String| Violation::new(flag, "pin-revision", message);
    let step = match After::parse(name, unit) {
        Ok(After::Merged(step)) => step,
        Ok(After::PrOpen(_) | After::Tag(_) | After::Deployed(_)) | Err(_) => {
            return Err(refuse(format!(
                "`{text}` is not <step>[#<part>][@<sha>] or <NN-name>/<unit>/<step>[#<part>][@<sha>]"
            )));
        }
    };
    let Some(declared) = plan::step(units, &step) else {
        return Err(refuse(format!("{step} is not declared")));
    };
    let step = match part {
        Some(part) if declared.parts.contains_key(part) => format!("{step}#{part}"),
        Some(part) => return Err(refuse(format!("{step} declares no part `{part}`"))),
        None if declared.parts.is_empty() => step,
        None => {
            return Err(refuse(format!(
                "{step} is a multipart step; pin evidence to one part's head as {step}#<part>"
            )));
        }
    };
    let current = head(root, units, &step);
    match (sha, current) {
        (Some(sha), _) if !is_commit(sha) => Err(refuse(format!("`{sha}` is not a commit"))),
        (Some(sha), Some(current)) if !same_commit(sha, &current) => Err(refuse(format!(
            "`{sha}` is not {step}'s head {current}; pin evidence to the head it was produced at"
        ))),
        (Some(sha), _) => Ok(Revision {
            step,
            sha: sha.to_ascii_lowercase(),
        }),
        (None, Some(current)) => Ok(Revision { step, sha: current }),
        (None, None) => Err(refuse(format!(
            "{step} has no open PR head; name the commit as {step}@<sha>"
        ))),
    }
}

pub fn pin(file: CampaignFile, candidate: Revision, harness: Option<Revision>, at: &str) -> Pin {
    Pin {
        file,
        candidate,
        harness,
        at: at.to_owned(),
        stale: None,
    }
}

pub fn mark_stale(
    units: &mut Units,
    heads: &BTreeMap<String, String>,
    at: &str,
) -> Vec<(String, String)> {
    let multipart: Vec<String> = units
        .iter()
        .flat_map(|(unit_name, unit)| {
            unit.steps
                .iter()
                .filter(|(_, step)| !step.parts.is_empty())
                .map(move |(name, _)| format!("{unit_name}/{name}"))
        })
        .collect();
    let mut out = Vec::new();
    for (unit_name, unit) in units.iter_mut() {
        for (name, step) in unit.steps.iter_mut() {
            for pin in step.pins.iter_mut().filter(|pin| pin.stale.is_none()) {
                let moved = std::iter::once(&pin.candidate)
                    .chain(pin.harness.as_ref())
                    .find_map(|revision| {
                        if multipart.contains(&revision.step) {
                            return Some(format!(
                                "{} became multipart; pin to one part as {}#<part>",
                                revision.step, revision.step
                            ));
                        }
                        let now = heads.get(&revision.step)?;
                        (!same_commit(now, &revision.sha)).then(|| {
                            format!("{} moved from {} to {now}", revision.step, revision.sha)
                        })
                    });
                if let Some(moved) = moved {
                    out.push((
                        unit_name.clone(),
                        format!(
                            "- evidence {} of {unit_name}/{name} is stale: {moved}",
                            pin.file
                        ),
                    ));
                    pin.stale = Some(format!("{moved} at {at}"));
                }
            }
        }
    }
    out
}

pub fn mark_expired(
    units: &mut Units,
    now: jiff::Timestamp,
    at: &str,
    root: &Path,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (unit_name, unit) in units.iter_mut() {
        for (name, step) in unit.steps.iter_mut() {
            for watch in step.watches.iter_mut() {
                let first =
                    watch.expired.is_none() && watch.status(now, root) == WatchStatus::Expired;
                if first {
                    watch.expired = Some(at.to_owned());
                    out.push((
                        unit_name.clone(),
                        format!(
                            "- watch {unit_name}/{name}#{} expired: unresolved ({}, deadline {})",
                            watch.id, watch.signal, watch.deadline
                        ),
                    ));
                }
            }
        }
    }
    out
}

pub struct Held {
    pub watches: Vec<Value>,
    pub stale: Vec<Value>,
    pub lines: Vec<String>,
}

pub fn held(units: &Units, now: jiff::Timestamp, root: &Path) -> Held {
    let mut held = Held {
        watches: Vec::new(),
        stale: Vec::new(),
        lines: Vec::new(),
    };
    for (unit_name, unit) in units {
        for (name, step) in &unit.steps {
            let target = format!("{unit_name}/{name}");
            for watch in step.unresolved_watches(root) {
                let status = watch.status(now, root);
                held.lines.push(format!(
                    "watch {target}#{} {}: {} (deadline {})",
                    watch.id,
                    status.name(),
                    watch.signal,
                    watch.deadline
                ));
                held.watches.push(json!({
                    "target": target,
                    "id": watch.id,
                    "signal": watch.signal.to_string(),
                    "deadline": watch.deadline,
                    "status": status.name(),
                }));
            }
            for pin in plan::stale_evidence(units, step) {
                let why = pin.stale.clone().unwrap_or_default();
                held.lines
                    .push(format!("stale {target}: {} ({why})", pin.file));
                held.stale.push(json!({
                    "target": target,
                    "file": pin.file.to_string(),
                    "candidate": pin.candidate.to_string(),
                    "harness": pin.harness.as_ref().map(ToString::to_string),
                    "stale": why,
                }));
            }
        }
    }
    held
}
