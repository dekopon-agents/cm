//! The executors `cm tick` runs, one per heartbeat.sh `act.d/` script.

pub mod codex_dispatch;
pub mod merge_ready;
pub mod released_undeployed;
pub mod review_owed;
pub mod unblock_now;

use crate::tick::act::Registry;

/// Every executor, under its `act.d/` name.
pub fn registry() -> Registry {
    Registry::new()
        .with(codex_dispatch::KEY, codex_dispatch::CodexDispatch)
        .with(merge_ready::KEY, merge_ready::MergeReady)
        .with(
            released_undeployed::KEY,
            released_undeployed::ReleasedUndeployed,
        )
        .with(review_owed::KEY, review_owed::ReviewOwed)
        .with(unblock_now::KEY, unblock_now::UnblockNow)
}
