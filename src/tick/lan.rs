//! Pin PRs and the LAN leg (`tick/release-lan`; PLAN.md §Shared surfaces "rpi-homelab";
//! 11-D16 to 11-D20, 11-D26).
//!
//! A pin opens with `gh.pin.update` and merges, at least one tick later, with `gh.pin.merge` built
//! from the recorded update receipt's `number` and `headSha` (11-D18). Before planning the merge:
//! `homelab_deploy` in `[authority]`; no merge mismatch holding pins (`merge::pins_held`); no other
//! non-draft homelab PR ready; the homelab gate's four checks green at that head; and a readiness
//! receipt no older than one heartbeat. Readiness is always a ledger receipt of
//! `kubectl.get.readyz`: on the RPi the runner applies it, and in the interim the local applier
//! hands it to the coordinator's LAN-leg runbook, which records it with `cm effects record`.
//! "One heartbeat" is the previous tick: a receipt recorded after the previous tick's first push
//! (`Cursor::last_tick`) is fresh; missing, failed and stale each hold the pin.
//!
//! After a pin merges, the LAN leg is one ordered run of effects: Argo refresh, application health,
//! the deployment's rollout and `/readyz`. Their receipts are the probe: they mark the step
//! deployed or hold it. Only a probe step that needs a live turn or LAN-only access files a
//! MAC-LANE item.

#![allow(
    unused_variables,
    reason = "skeleton: tick/release-lan fills these bodies"
)]

use std::path::Path;

use crate::forge::{ForgeError, Gh};
use crate::tick::ledger::{EffectLedger, IntentId, IntentRow, Observation};
use crate::tick::run::{Cycle, Deps, Planned};
use crate::tick::wire::{EffectInput, PinValue, Receipt};

pub const HOMELAB_REPO: &str = "xrl/rpi-homelab";

/// The homelab reads `Forge` does not have.
pub trait LanReads {
    /// Open PRs on rpi-homelab with their draft flag and head (SKELETON.md, Named gaps: 11-D10's
    /// route table opens only `pulls/{number}` there).
    fn homelab_pulls(&self) -> Result<Vec<HomelabPull>, ForgeError>;
}

pub struct GhLanReads<'a> {
    pub gh: &'a Gh,
}

impl LanReads for GhLanReads<'_> {
    fn homelab_pulls(&self) -> Result<Vec<HomelabPull>, ForgeError> {
        todo!("tick/release-lan")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomelabPull {
    pub number: u32,
    pub draft: bool,
    pub head: String,
}

/// The newest `kubectl.get.readyz` receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub intent: IntentId,
    /// `cm` stamp of its receipt row.
    pub at: String,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    ReadinessMissing,
    ReadinessFailed,
    ReadinessStale { at: String },
    AnotherHomelabPrReady { number: u32 },
    HomelabGateRed { why: Vec<String> },
    HomelabDeployNotAuthorized,
    MergeMismatch { intent: IntentId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    Deployed,
    Held {
        why: String,
    },
    /// A receipt of the run is still missing.
    Pending,
}

pub fn readiness(ledger: &EffectLedger) -> Option<Readiness> {
    todo!("tick/release-lan")
}

/// Missing, failed and stale each hold; `last_tick` is `Cursor::last_tick`.
pub fn readiness_hold(readiness: Option<&Readiness>, last_tick: Option<&str>) -> Option<Hold> {
    todo!("tick/release-lan")
}

/// Non-draft homelab PRs only.
pub fn ready_homelab_prs(pulls: &[HomelabPull]) -> usize {
    todo!("tick/release-lan")
}

pub fn pin_update(target: &str, pins: Vec<PinValue>, title: &str) -> Planned {
    todo!("tick/release-lan")
}

/// Built from the recorded `gh.pin.update` receipt of `update`.
pub fn pin_merge(ledger: &EffectLedger, update: &IntentId) -> Option<Planned> {
    todo!("tick/release-lan")
}

/// Argo refresh, application read, deployment read and `/readyz`, in that order.
pub fn lan_leg(application: &str, deployment: &str, namespace: &str) -> Vec<EffectInput> {
    todo!("tick/release-lan")
}

/// The LAN leg's receipts read as the step's probe.
pub fn probe(receipts: &[&Receipt]) -> ProbeResult {
    todo!("tick/release-lan")
}

/// A pin or kubectl intent's lookup. A kubectl read is not done (reads are emitted again); a pin
/// merge is done when the PR merged at its head; a pin update whose receipt was lost is
/// undecidable, since the branch name alone does not find its PR without a list read.
pub fn observe(root: &Path, deps: &Deps, row: &IntentRow) -> Observation {
    todo!("tick/release-lan")
}

/// Stage 7 of `run`, second half: pin updates for releases whose rollout step is ready, pin merges
/// from recorded updates, the LAN leg after a merged pin, and probes from its receipts.
pub fn stage(cycle: &mut Cycle) -> crate::tick::Result<()> {
    todo!("tick/release-lan")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_pin_killed_between_update_and_merge_merges_from_the_recorded_receipt() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_missing_failed_or_stale_readiness_receipt_holds_the_pin() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn at_most_one_non_draft_homelab_pr_is_ready_at_a_time() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_restart_after_a_failed_probe_keeps_the_step_held() {
        todo!()
    }
}
