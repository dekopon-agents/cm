//! `cm effects record` and `cm effects resolve` (`tick/ledger`).
//!
//! `record` is 11-D3 step (3): one batch of receipts on stdin, at most
//! [`BATCH_MAX_BYTES`](crate::tick::wire::BATCH_MAX_BYTES), written into the ledger in effect order,
//! committed and pushed before it returns, so the runner sends the next batch only after this one
//! is on main. A batch is taken whole or not at all. The interim local applier records through
//! [`record_receipts`], the same path.
//!
//! `resolve` writes a ruling row on a parked intent (11-D22: "answers by pushing ruling rows").
//!
//! Both take the tick lock (S-tick-05) without waiting: while a tick runs they refuse, and the
//! runner calls `record` only after step (1) has exited.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::io::Read;
use std::path::Path;

use anyhow::Result;

use crate::host::Host;
use crate::outcome::Outcome;
use crate::tick::ledger::{EffectLedger, IntentId, Recorded, Ruling};
use crate::tick::push::Git;
use crate::tick::wire::Receipt;

/// Reads stdin whole, refusing past the batch bound; parses; records every receipt; commits and
/// pushes. A refused batch records none of its receipts and leaves the ledger file unchanged.
pub fn record(
    host: &dyn Host,
    git: &dyn Git,
    root: &Path,
    stdin: &mut dyn Read,
) -> Result<Outcome> {
    todo!("tick/ledger")
}

/// The batch, checked: a JSON array, each element a [`Receipt`] whose serialized form stays under
/// [`RECEIPT_MAX_BYTES`](crate::tick::wire::RECEIPT_MAX_BYTES). An inline output too large for it
/// is re-recorded as oversize with its hash, never truncated.
pub fn parse_batch(bytes: &[u8]) -> crate::tick::Result<Vec<Receipt>> {
    todo!("tick/ledger")
}

/// Appends each receipt in order. Checks every receipt against the ledger before appending any,
/// so an unknown intent or a conflicting receipt refuses the whole batch.
pub fn record_receipts(
    host: &dyn Host,
    ledger: &mut EffectLedger,
    receipts: &[Receipt],
) -> crate::tick::Result<Vec<Recorded>> {
    todo!("tick/ledger")
}

/// `cm effects resolve <intent> --as done|retry|drop`: refuses an intent that is not parked, and
/// a `done` whose evidence is not a campaign file; commits and pushes the ruling row.
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
    fn a_refused_batch_records_none_of_its_receipts() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn rerecording_a_pushed_batch_is_a_no_op_by_intent_id() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_batch_over_twenty_kib_is_refused_whole_and_an_eight_kib_output_is_recorded_oversize() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_restart_after_a_pushed_batch_takes_the_next_batch_without_rerecording() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_ruling_on_an_intent_that_is_not_parked_is_refused() {
        todo!()
    }
}
