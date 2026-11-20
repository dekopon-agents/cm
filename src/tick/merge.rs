//! The exact-head merge (`tick/gate-merge`; PLAN.md `07-tick/gate-merge`; 11-D12, 11-D13).
//!
//! For each watched PR with a bound ACCEPT from the other family at its current head, the
//! phase's proof whole, `[authority]` allowing the repository, the §Gates checks green at that head
//! (`authority::checks_green`) and, on core dekopon, the drop admitting one more counting merge
//! (`release::drop_admits`): a branch with a merge queue gets `gh.pull-request.enqueue` with
//! `expectedHeadSha` = that head, never REST; any other gets `gh.pull-request.merge` with it. In the
//! cloud interim the enqueue is a packet action (`apply::defer`), bracketed by a REST head read
//! right before the MCP call. After every merge or enqueue intent, [`confirm`] reads the merged PR
//! over REST and binds merged only when its head is the gated head; otherwise the intent parks
//! with a `head-moved-after-merge` item (revert PR, every pin held, journaled), and
//! [`pins_held`] holds every pin until a ruling closes it. `gh pr` is never used.

#![allow(
    unused_variables,
    reason = "skeleton: tick/gate-merge fills these bodies"
)]

use crate::forge::{Forge, ForgeError, Gh, GithubRepo};
use crate::tick::gate::BoundVerdict;
use crate::tick::ledger::{EffectLedger, IntentId, IntentRow, Observation};
use crate::tick::packet::PacketItem;
use crate::tick::run::{Cycle, Deps, Planned};
use crate::tick::wire::{Effect, Receipt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchKind {
    Queued,
    Rest,
}

/// The merge reads `Forge` does not have.
pub trait MergeReads {
    /// Whether `branch` requires a merge queue (`rules/branches/{branch}` holds a `merge_queue`
    /// rule; 11-D10).
    fn queued(&self, repo: &GithubRepo, branch: &str) -> Result<bool, ForgeError>;
}

pub struct GhMergeReads<'a> {
    pub gh: &'a Gh,
}

impl MergeReads for GhMergeReads<'_> {
    fn queued(&self, repo: &GithubRepo, branch: &str) -> Result<bool, ForgeError> {
        todo!("tick/gate-merge")
    }
}

/// What the merge stage does for one bound verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum MergePlan {
    Effect(Box<Planned>),
    Packet(PacketItem),
    /// Checks pending, the other family's verdict missing, or the drop full: look again next tick.
    Wait {
        why: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostMerge {
    Open,
    Merged {
        merge_sha: String,
    },
    /// The merged PR's head is not the gated head.
    Mismatch {
        merged_head: String,
        gated: String,
    },
    Closed,
}

/// The merge or enqueue for one PR whose verdicts are bound at its current head.
pub fn route(cycle: &Cycle, verdict: &BoundVerdict, kind: BranchKind) -> MergePlan {
    todo!("tick/gate-merge")
}

/// After a merge or enqueue intent: the merged PR read over REST, compared with the gated head.
pub fn confirm(forge: &dyn Forge, url: &str, gated: &str) -> Result<PostMerge, ForgeError> {
    todo!("tick/gate-merge")
}

/// A merge or enqueue intent's lookup: [`confirm`] at the intent's head. Merged at the head binds;
/// merged at another head binds and the stage parks it with the mismatch; open is not done for a
/// merge and undecidable for an enqueue that may still sit in the queue; a failed read is unread.
pub fn observe(deps: &Deps, row: &IntentRow) -> Observation {
    todo!("tick/gate-merge")
}

/// The local applier's REST merge (`PUT /repos/{o}/{r}/pulls/{n}/merge` with `sha` and
/// `merge_method`): 405 or 409 is `refused`, a transport failure `error`.
pub fn apply_local(gh: &Gh, effect: &Effect) -> Receipt {
    todo!("tick/gate-merge")
}

/// A parked merge mismatch holds every pin until a ruling closes it.
pub fn pins_held(ledger: &EffectLedger) -> Option<IntentId> {
    todo!("tick/gate-merge")
}

/// Stage 6 of `run`: bind verdicts from this tick's comments and recorded Astra reviews, refuse
/// what must not merge, plan merges and enqueues, and confirm earlier ones.
pub fn stage(cycle: &mut Cycle) -> crate::tick::Result<()> {
    todo!("tick/gate-merge")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_merge_killed_after_its_effect_is_confirmed_from_the_merged_pr_and_never_resent() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_reloaded_ledger_confirms_a_pending_merge_by_its_gated_head() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_pr_holds_at_most_one_open_merge_or_enqueue_intent() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_restart_after_a_mismatched_merge_keeps_every_pin_held_until_a_ruling() {
        todo!()
    }
}
