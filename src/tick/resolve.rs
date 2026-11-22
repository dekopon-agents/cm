//! `cm effects resolve` (`tick/ledger`): a ruling row on a parked intent (11-D22: the coordinator
//! agent "answers by pushing ruling rows to the ledger"). The ruling is data: a `retry` goes back
//! through its owning stage's [`readmit`](crate::tick::ledger::readmit) under current authority
//! on the next tick, never straight to an effect.
//!
//! It takes the tick lock (S-tick-05) without waiting, in the clone it runs in. On the RPi the
//! agent's clone and the tick's clone each hold their own lock; their rows meet on main by the
//! ledger's union merge (R03).

use std::path::Path;

use anyhow::Result;

use crate::host::Host;
use crate::outcome::{Outcome, Violation};
use crate::tick::ledger::{EffectLedger, IntentId, LEDGER_DIR, Ruling, TickLock};
use crate::tick::push::{self, Git};

/// `cm effects resolve <intent> --as done|retry|drop`: refuses an intent that is not parked, a
/// `retry` past the ruling budget, and a `done` whose evidence is not a campaign file; commits and
/// pushes the ruling row.
pub fn resolve(
    host: &dyn Host,
    git: &dyn Git,
    root: &Path,
    intent: &IntentId,
    ruling: Ruling,
) -> Result<Outcome> {
    let Some(lock) = TickLock::try_acquire(root)? else {
        return Ok(refused(
            crate::tick::ledger::TICK_LOCK,
            "tick-locked",
            "another tick, record or resolve holds the tick lock; run it again once that ends",
        ));
    };
    let ruling = match ruling {
        Ruling::Done { evidence } => match campaign_file(root, &evidence) {
            Some(evidence) => Ruling::Done { evidence },
            None => {
                return Ok(refused(
                    &evidence,
                    "ruling-evidence",
                    "a done ruling needs --evidence naming a file inside the campaign folder",
                ));
            }
        },
        ruling => ruling,
    };
    let mut ledger = EffectLedger::open(root, &lock)?;
    if let Some(reason) = ledger
        .ruling_refusal(intent, &ruling)
        .map_err(anyhow::Error::from)?
    {
        return Ok(refused(LEDGER_DIR, "ruling-refused", reason));
    }
    ledger.rule(host, intent, ruling.clone())?;
    let checkout = git.toplevel(root)?;
    let segment = root.join(ledger.active_segment());
    let forced = [segment
        .strip_prefix(&checkout)
        .map(Path::to_owned)
        .unwrap_or(segment.clone())];
    let word = match &ruling {
        Ruling::Done { .. } => "done",
        Ruling::Retry => "retry",
        Ruling::Drop { .. } => "drop",
    };
    let pushed = push::commit_and_push(
        git,
        &checkout,
        &forced,
        &[],
        &format!("cm effects resolve: {intent} as {word}"),
    )?;
    let mut outcome = Outcome::default();
    outcome.field("intent", intent.0.clone());
    outcome.field("as", word);
    if let Some(commit) = pushed.commit {
        outcome.field("commit", commit.as_str());
    }
    outcome.line(format!("{intent} ruled {word}"));
    Ok(outcome)
}

fn refused(path: &str, rule: &'static str, message: impl Into<String>) -> Outcome {
    Outcome::refused(vec![Violation::new(path, rule, message)])
}

/// `evidence` as a path relative to the campaign folder, when it names a file inside it.
fn campaign_file(root: &Path, evidence: &str) -> Option<String> {
    if evidence.is_empty() {
        return None;
    }
    let path = Path::new(evidence);
    let path = if path.is_relative() && !path.exists() {
        root.join(path)
    } else {
        path.to_owned()
    };
    let file = std::fs::canonicalize(&path)
        .ok()
        .filter(|file| file.is_file())?;
    let root = std::fs::canonicalize(root).ok()?;
    let rel = file.strip_prefix(&root).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::tick::ledger::tests::{clock, effect, new_id, scratch, tick_lock};
    use crate::tick::ledger::{IntentState, LedgerRow};
    use crate::tick::push::SystemGit;
    use crate::tick::wire::Capability;

    /// A clone whose ledger holds one parked and one open intent, pushed.
    fn parked() -> (scratch::Scratch, PathBuf, IntentId, IntentId) {
        let scratch = scratch::Scratch::new();
        let checkout = scratch.clone("agent");
        let campaign = scratch::campaign(&checkout);
        let lock = tick_lock(&campaign);
        let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
        let host = clock();
        let parked = new_id(
            ledger
                .intend(&host, 1, effect("01-a/u/s", Capability::GhPinUpdate))
                .unwrap(),
        );
        let open = new_id(
            ledger
                .intend(&host, 1, effect("01-a/u/t", Capability::GhPinUpdate))
                .unwrap(),
        );
        ledger.emitted(&host, &parked, 1).unwrap();
        ledger.park(&host, &parked, "undecidable").unwrap();
        let segment = Path::new(scratch::EPOCH).join(ledger.active_segment());
        push::commit_and_push(&SystemGit::default(), &checkout, &[segment], &[], "tick 1").unwrap();
        (scratch, campaign, parked, open)
    }

    /// `resolve`, retried while the tick lock reads held: a sibling test's forked git child can
    /// keep a just-closed lock descriptor until it execs.
    fn ruled(
        host: &dyn Host,
        git: &dyn Git,
        root: &Path,
        intent: &IntentId,
        ruling: Ruling,
    ) -> Result<Outcome> {
        for _ in 0..200 {
            let outcome = resolve(host, git, root, intent, ruling.clone())?;
            if rules(&outcome) != ["tick-locked"] {
                return Ok(outcome);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the tick lock stayed held");
    }

    fn rules(outcome: &Outcome) -> Vec<&'static str> {
        outcome.violations.iter().map(|v| v.rule).collect()
    }

    fn rulings(campaign: &Path, id: &IntentId) -> usize {
        EffectLedger::snapshot(campaign)
            .unwrap()
            .rows()
            .iter()
            .filter(|row| matches!(row, LedgerRow::Ruled { intent, .. } if intent == id))
            .count()
    }

    #[test]
    fn a_ruling_on_an_intent_that_is_not_parked_is_refused() {
        let (scratch, campaign, parked, open) = parked();
        let git = SystemGit::default();
        let host = clock();
        let main = scratch.main_sha();
        for ruling in [
            Ruling::Retry,
            Ruling::Drop {
                reason: "no".to_owned(),
            },
            Ruling::Done {
                evidence: "STATE.md".to_owned(),
            },
        ] {
            let outcome = ruled(&host, &git, &campaign, &open, ruling).unwrap();
            assert_eq!(rules(&outcome), ["ruling-refused"]);
        }
        assert!(
            resolve(
                &host,
                &git,
                &campaign,
                &IntentId("i-20261122T1800-0042".to_owned()),
                Ruling::Retry
            )
            .is_err()
        );
        // Closed is not parked either.
        let done = resolve(
            &host,
            &git,
            &campaign,
            &parked,
            Ruling::Drop {
                reason: "owner".to_owned(),
            },
        )
        .unwrap();
        assert!(done.ok(), "{:?}", done.violations);
        let again = ruled(&host, &git, &campaign, &parked, Ruling::Retry).unwrap();
        assert_eq!(rules(&again), ["ruling-refused"]);
        assert_eq!(rulings(&campaign, &open), 0);
        assert_eq!(rulings(&campaign, &parked), 1);
        assert_ne!(
            scratch.main_sha(),
            main,
            "the one accepted ruling was pushed"
        );
        // While another writer holds the tick lock, nothing is ruled.
        let lock = tick_lock(&campaign);
        let held = resolve(&host, &git, &campaign, &open, Ruling::Retry).unwrap();
        assert_eq!(rules(&held), ["tick-locked"]);
        drop(lock);
    }

    #[test]
    fn a_retry_past_the_ruling_budget_is_refused_and_done_or_drop_still_close() {
        let (scratch, campaign, parked, _) = parked();
        let git = SystemGit::default();
        let host = clock();
        let repark = || {
            let lock = tick_lock(&campaign);
            let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
            ledger.emitted(&host, &parked, 2).unwrap();
            ledger.park(&host, &parked, "again").unwrap();
        };
        for _ in 0..crate::tick::ledger::MAX_RULINGS_PER_INTENT {
            assert!(
                ruled(&host, &git, &campaign, &parked, Ruling::Retry)
                    .unwrap()
                    .ok()
            );
            let ledger = EffectLedger::snapshot(&campaign).unwrap();
            assert_eq!(ledger.state(&parked), Some(IntentState::Open));
            repark();
        }
        let over = ruled(&host, &git, &campaign, &parked, Ruling::Retry).unwrap();
        assert_eq!(rules(&over), ["ruling-refused"]);
        // A done needs a campaign file.
        let missing = Ruling::Done {
            evidence: "no-such-file.md".to_owned(),
        };
        assert_eq!(
            rules(&ruled(&host, &git, &campaign, &parked, missing).unwrap()),
            ["ruling-evidence"]
        );
        let outside = scratch.dir.path().join("outside.md");
        fs::write(&outside, "x\n").unwrap();
        let outside = Ruling::Done {
            evidence: outside.to_string_lossy().into_owned(),
        };
        assert_eq!(
            rules(&ruled(&host, &git, &campaign, &parked, outside).unwrap()),
            ["ruling-evidence"]
        );
        let evidence = campaign.join("STATE.md").to_string_lossy().into_owned();
        let done = ruled(&host, &git, &campaign, &parked, Ruling::Done { evidence }).unwrap();
        assert!(done.ok(), "{:?}", done.violations);
        let ledger = EffectLedger::snapshot(&campaign).unwrap();
        assert_eq!(ledger.state(&parked), Some(IntentState::Closed));
        assert!(ledger.rows().iter().any(|row| matches!(
            row,
            LedgerRow::Ruled { ruling: Ruling::Done { evidence }, .. } if evidence == "STATE.md"
        )));
        let on_main = scratch
            .on_main(&format!("{}/.cm/tick/effects-0001.jsonl", scratch::EPOCH))
            .unwrap();
        assert_eq!(on_main.matches("\"ruled\"").count(), 3);

        // And a drop closes one at its budget too.
        let (_second, campaign, parked, _) = self::parked();
        for _ in 0..crate::tick::ledger::MAX_RULINGS_PER_INTENT {
            assert!(
                ruled(&host, &git, &campaign, &parked, Ruling::Retry)
                    .unwrap()
                    .ok()
            );
            let lock = tick_lock(&campaign);
            let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
            ledger.park(&host, &parked, "again").unwrap();
        }
        let drop = resolve(
            &host,
            &git,
            &campaign,
            &parked,
            Ruling::Drop {
                reason: "owner".to_owned(),
            },
        )
        .unwrap();
        assert!(drop.ok());
        assert_eq!(
            EffectLedger::snapshot(&campaign).unwrap().state(&parked),
            Some(IntentState::Closed)
        );
    }
}
