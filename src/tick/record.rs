//! `cm effects record` (11-D3 step 3): one batch of receipts on stdin, at most
//! [`BATCH_MAX_BYTES`](crate::tick::wire::BATCH_MAX_BYTES), written into the ledger in effect order
//! and pushed before the runner sends the next batch. `tick/ledger` owns it.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::io::Read;
use std::path::Path;

use anyhow::Result;

use crate::host::Host;
use crate::outcome::Outcome;
use crate::tick::push::Git;
use crate::tick::wire::Receipt;

/// Reads stdin whole (refusing past the batch bound), parses it, records every receipt under the
/// campaign lock and pushes. A refused batch records none of its receipts.
pub fn record(
    host: &dyn Host,
    git: &dyn Git,
    root: &Path,
    stdin: &mut dyn Read,
) -> Result<Outcome> {
    todo!("tick/ledger")
}

/// The batch, checked: a JSON array, each receipt for an open intent, each inline output under
/// [`RECEIPT_MAX_BYTES`](crate::tick::wire::RECEIPT_MAX_BYTES) (a larger one is re-recorded as
/// oversize with its hash, never truncated).
pub fn parse_batch(bytes: &[u8]) -> crate::tick::Result<Vec<Receipt>> {
    todo!("tick/ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_failed_batch_records_none_of_its_receipts() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn rerecording_a_batch_is_a_no_op_by_intent_id() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_batch_over_twenty_kib_is_refused_whole_and_an_eight_kib_output_is_recorded_oversize() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn restart_after_a_pushed_batch_takes_the_next_batch_without_rerecording() {
        todo!()
    }
}
