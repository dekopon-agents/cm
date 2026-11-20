//! Who applies the effects a tick plans (S-tick-23, S-tick-24). One cycle, two appliers:
//!
//! - **Runner** (`cm tick --effects`, the RPi): cm prints the effects and the runner applies them
//!   as principal `rpi-coordinator` (11-D3 (2)); receipts come back by `cm effects record`.
//! - **Local** (`cm tick` in the cloud interim, before any runner exists): cm applies what the
//!   cloud session's own `gh` login can, through [`LocalApply`], in order, stopping at the first
//!   refusal or error exactly as the runner does, and records the receipts with
//!   [`record_receipts`](crate::tick::record::record_receipts). What it cannot apply becomes a
//!   decision-packet action for the coordinator, whose receipt comes back by
//!   `cm effects record` like the runner's: the enqueue (GraphQL is 403 in the cloud, so the
//!   coordinator runs MCP `enable_pr_auto_merge`, PLAN.md `07-tick/gate-merge`), the pin verbs
//!   (the coordinator is the homelab writer) and the kubectl reads (the LAN-leg runbook).
//!
//! Either way the intent rows are pushed first and the ledger is the only record, so the interim
//! and the RPi share one reconcile.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use crate::forge::Gh;
use crate::tick::packet::PacketItem;
use crate::tick::wire::{Capability, Effect, Receipt};

/// Who applies this tick's effects; it also picks where a Claude session starts (S-tick-37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applier {
    /// `cm tick --effects`: the RPi runner.
    Runner,
    /// `cm tick`: cm itself, in the cloud session.
    Local,
}

/// What applying one effect produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    Receipt(Receipt),
    /// Handed to the coordinator: the intent is bound `via: action` (`IntentState::Handed`) and
    /// waits for its receipt.
    Deferred(PacketItem),
}

pub trait Apply {
    fn apply(&self, effect: &Effect) -> Applied;
}

/// The local applier: the cloud session's `gh` login.
pub struct LocalApply<'a> {
    pub gh: &'a Gh,
}

/// Skeleton glue: it routes each capability to the unit that owns its REST call, so no unit
/// edits this match.
impl Apply for LocalApply<'_> {
    fn apply(&self, effect: &Effect) -> Applied {
        match effect.capability {
            Capability::GhPullRequestMerge => {
                Applied::Receipt(crate::tick::merge::apply_local(self.gh, effect))
            }
            Capability::GhWorkflowRun => {
                Applied::Receipt(crate::tick::release::apply_local(self.gh, effect))
            }
            Capability::GhPullRequestEnqueue
            | Capability::GhPinUpdate
            | Capability::GhPinMerge
            | Capability::KubectlAnnotateApplication
            | Capability::KubectlGetApplication
            | Capability::KubectlGetDeployment
            | Capability::KubectlGetReadyz => Applied::Deferred(defer(effect)),
        }
    }
}

/// The packet action for an effect the local applier cannot run: the exact steps, including the
/// `cm effects record` line whose receipt closes the intent. An enqueue's action carries 07-tick
/// gate-merge's bracket: read the head over REST right before the call and enqueue only if it is
/// the gated head.
pub fn defer(effect: &Effect) -> PacketItem {
    todo!("tick/ledger")
}

/// The effects applied in order up to and including the first `refused` or `error` receipt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppliedRun {
    pub receipts: Vec<Receipt>,
    pub deferred: Vec<PacketItem>,
    /// The index of the effect that stopped the run; the ones after it were not applied.
    pub stopped_at: Option<usize>,
}

/// 11-D3 (2)'s order and stop rule, for the local applier.
pub fn apply_in_order(apply: &dyn Apply, effects: &[Effect]) -> AppliedRun {
    todo!("tick/ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn the_local_run_stops_at_the_first_refusal_and_leaves_later_intents_open() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_tick_killed_mid_apply_reconciles_the_applied_effect_and_reemits_only_the_rest() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_deferred_effect_stays_open_until_its_receipt_is_recorded() {
        todo!()
    }
}
