//! `cm effects resolve` (`tick/ledger`): a ruling row on a parked intent (11-D22: the coordinator
//! agent "answers by pushing ruling rows to the ledger"). The ruling is data: a `retry` goes back
//! through its owning stage's [`readmit`](crate::tick::ledger::readmit) under current authority
//! on the next tick, never straight to an effect.
//!
//! It takes the tick lock (S-tick-05) without waiting, in the clone it runs in. On the RPi the
//! agent's clone and the tick's clone each hold their own lock; their rows meet on main by the
//! ledger's union merge (R03).

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::path::Path;

use anyhow::Result;

use crate::host::Host;
use crate::outcome::Outcome;
use crate::tick::ledger::{IntentId, Ruling};
use crate::tick::push::Git;

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
    todo!("tick/ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_ruling_on_an_intent_that_is_not_parked_is_refused() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_retry_past_the_ruling_budget_is_refused_and_done_or_drop_still_close() {
        todo!()
    }
}
