//! `cm effects record` (`tick/wire`): 11-D3 step (3). One batch of receipts on stdin, at most
//! [`BATCH_MAX_BYTES`](crate::tick::wire::BATCH_MAX_BYTES), written into the ledger in effect
//! order with the cursor's current tick as their cycle, committed and pushed before it returns, so
//! the runner sends the next batch only after this one is on main. A batch is taken whole or not
//! at all. The interim local applier records through [`record_receipts`], the same path.
//!
//! It takes the tick lock (S-tick-05) without waiting: while a tick computes it refuses. The
//! runner records only after its compute step has exited and serializes its own cycles (R06).

#![allow(unused_variables, reason = "skeleton: tick/wire fills these bodies")]

use std::io::Read;
use std::path::Path;

use anyhow::Result;

use crate::host::Host;
use crate::outcome::Outcome;
use crate::tick::ledger::{EffectLedger, Recorded};
use crate::tick::push::Git;
use crate::tick::wire::Receipt;

/// Reads stdin whole, refusing past the batch bound; parses; records every receipt; commits and
/// pushes. A refused batch records none of its receipts and leaves the ledger unchanged.
pub fn record(
    host: &dyn Host,
    git: &dyn Git,
    root: &Path,
    stdin: &mut dyn Read,
) -> Result<Outcome> {
    todo!("tick/wire")
}

/// The batch, checked: a JSON array of the runner's [`Receipt`]. An inline output that breaks the
/// one sizing rule (`wire` module doc) is re-recorded as oversize from its bytes, never truncated.
pub fn parse_batch(bytes: &[u8]) -> crate::tick::Result<Vec<Receipt>> {
    todo!("tick/wire")
}

/// Appends each receipt in order with `cycle`. Checks every receipt against the ledger's fold
/// before appending any, so an unknown intent or a conflicting receipt refuses the whole batch.
pub fn record_receipts(
    host: &dyn Host,
    ledger: &mut EffectLedger,
    cycle: u64,
    receipts: &[Receipt],
) -> crate::tick::Result<Vec<Recorded>> {
    todo!("tick/wire")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/wire"]
    fn a_refused_batch_records_none_of_its_receipts() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/wire"]
    fn rerecording_a_pushed_batch_is_a_no_op_by_intent_id() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/wire"]
    fn a_batch_over_twenty_kib_is_refused_whole_and_an_eight_kib_output_is_recorded_oversize() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/wire"]
    fn a_restart_after_a_pushed_batch_takes_the_next_batch_without_rerecording() {
        todo!()
    }
}
