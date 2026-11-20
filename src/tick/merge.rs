//! The exact-head merge (`tick/gate-merge`). A queued branch (dekopon main) enqueues with
//! `expectedHeadSha` = the reviewed head, never REST; elsewhere the REST merge with `sha`. In the
//! cloud interim (GraphQL 403) the enqueue is a packet action the coordinator runs with MCP
//! `enable_pr_auto_merge`, bracketed by a REST head read before and a merged-head read after. A
//! merged head that differs from the reviewed head parks with a packet: revert PR, every pin held,
//! incident journaled. `gh pr` is never used.

#![allow(
    unused_variables,
    reason = "skeleton: tick/gate-merge fills these bodies"
)]

use crate::forge::Forge;
use crate::tick::gate::BoundVerdict;
use crate::tick::ledger::IntentId;
use crate::tick::packet::PacketItem;
use crate::tick::run::Cycle;
use crate::tick::wire::Effect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchKind {
    Queued,
    Rest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applier {
    /// The runner applies effects (RPi, and `--effects` anywhere).
    Runner,
    /// The cloud session: an enqueue becomes a packet action.
    CloudInterim,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Effect(Effect),
    Packet(PacketItem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostMerge {
    Open,
    Merged {
        merge_sha: String,
    },
    /// The merged PR's head is not the reviewed head.
    Mismatch {
        merged_head: String,
        reviewed: String,
    },
}

/// The branch kind from the repository's merge-queue rule, read over REST (11-D10).
pub fn branch_kind(forge: &dyn Forge, repo: &str) -> crate::tick::Result<BranchKind> {
    todo!("tick/gate-merge")
}

/// The merge effect or packet action for a bound ACCEPT whose checks are green at its head.
pub fn route(
    intent: &IntentId,
    verdict: &BoundVerdict,
    kind: BranchKind,
    applier: Applier,
) -> Route {
    todo!("tick/gate-merge")
}

/// After a merge intent: reads the merged PR over REST and compares its head.
pub fn confirm(forge: &dyn Forge, pr_url: &str, reviewed: &str) -> crate::tick::Result<PostMerge> {
    todo!("tick/gate-merge")
}

/// Stage 5 of [`crate::tick::run`]: bind verdicts, emit merges, confirm earlier merges.
pub fn stage(cycle: &mut Cycle) -> crate::tick::Result<()> {
    todo!("tick/gate-merge")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_merge_interrupted_after_its_effect_is_confirmed_from_the_merged_pr_and_never_resent() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_reloaded_ledger_confirms_a_pending_merge_by_its_reviewed_head() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_pr_holds_at_most_one_open_merge_intent() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn restart_after_a_mismatched_merge_parks_with_a_revert_and_holds_every_pin() {
        todo!()
    }
}
