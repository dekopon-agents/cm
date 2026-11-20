//! Pin PRs and the LAN leg (`tick/release-lan`). A pin merges only with a readiness receipt (a
//! `/readyz` pass written by the coordinator's LAN-leg runbook) no older than one heartbeat, only
//! when no other non-draft homelab PR is ready, and only where `[authority]` has
//! `homelab_deploy = true`. After a pin merges: Argo refresh, health, rollout and `/readyz`, and
//! the probe receipt marks the step deployed or holds it. A Mac-lane item is filed only for a probe
//! step that carries one.

#![allow(
    unused_variables,
    reason = "skeleton: tick/release-lan fills these bodies"
)]

use std::path::Path;

use crate::tick::ledger::IntentId;
use crate::tick::run::Cycle;
use crate::tick::wire::{Effect, PinValue};

/// Relative to the campaign folder; written by the LAN-leg runbook.
pub const READINESS_FILE: &str = "14-mac/lan-leg/readiness.json";
/// One heartbeat. Open: the coordinator confirms the heartbeat length (SKELETON.md, Choices).
pub const READINESS_MAX_AGE_MINUTES: i64 = 60;
pub const HOMELAB_REPO: &str = "xrl/rpi-homelab";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    /// `cm` stamp.
    pub at: String,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    ReadinessMissing,
    ReadinessFailed,
    ReadinessStale { at: String },
    AnotherHomelabPrReady { number: u64 },
    HomelabGateRed,
    HomelabDeployNotAuthorized,
    ProbeFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomelabPull {
    pub number: u64,
    pub draft: bool,
    pub head: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Deployed,
    Held,
}

impl Readiness {
    pub fn read(root: &Path) -> crate::tick::Result<Option<Self>> {
        todo!("tick/release-lan")
    }
}

/// Missing, failed and stale each hold the pin.
pub fn readiness_hold(receipt: Option<&Readiness>, now: &str) -> Option<Hold> {
    todo!("tick/release-lan")
}

/// Non-draft homelab PRs only.
pub fn ready_homelab_prs(pulls: &[HomelabPull]) -> usize {
    todo!("tick/release-lan")
}

pub fn pin_update_effect(intent: &IntentId, pins: &[PinValue], title: &str) -> Effect {
    todo!("tick/release-lan")
}

/// Built from the recorded `gh.pin.update` receipt's `number` and `headSha`, one cycle later.
pub fn pin_merge_effect(intent: &IntentId, number: u64, head: &str) -> Effect {
    todo!("tick/release-lan")
}

/// Argo refresh, then the health, rollout and `/readyz` reads, as one ordered run of effects.
pub fn lan_leg_effects(
    intents: &[IntentId],
    app: &str,
    namespace: &str,
    deployment: &str,
) -> Vec<Effect> {
    todo!("tick/release-lan")
}

/// The probe receipt that came back: deployed, or the step holds.
pub fn probe(receipt: &serde_json::Value) -> Probe {
    todo!("tick/release-lan")
}

/// Stage 6 of [`crate::tick::run`], after releases: pins, LAN leg and probes.
pub fn stage(cycle: &mut Cycle) -> crate::tick::Result<()> {
    todo!("tick/release-lan")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_pin_interrupted_between_update_and_merge_merges_from_the_recorded_receipt() {
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
    fn restart_after_a_failed_probe_keeps_the_step_held() {
        todo!()
    }
}
