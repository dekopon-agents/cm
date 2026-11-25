//! Pin PRs and the LAN leg (`tick/release-lan`; PLAN.md §Shared surfaces "rpi-homelab";
//! 11-D16 to 11-D20, 11-D26).
//!
//! A pin opens with `gh.pin.update` and merges, at least one tick later, with `gh.pin.merge` built
//! from the recorded update receipt's `number` and `headSha` (11-D18). Before planning the merge:
//! `homelab_deploy` in `[authority]`; no merge mismatch holding pins (`merge::pins_held`); no other
//! non-draft homelab PR ready; the homelab gate's four checks green at that head; and a fresh
//! readiness receipt.
//!
//! **Readiness (R11).** Readiness is always a ledger receipt of `kubectl.get.readyz`: on the RPi
//! the runner applies it, and in the interim the local applier hands it to the coordinator's
//! LAN-leg runbook, which records it with `cm effects record`. "One heartbeat" is the previous
//! tick, ordered by tick number, not by minute stamps: a receipt is fresh when its row's `cycle` is
//! at least the loaded `Cursor::tick`, i.e. it was recorded after the previous tick's first push.
//! Missing, failed and stale each hold the pin.
//!
//! **Expected state (R12).** A pin is planned from a [`Rollout`]: the pin names and values, the
//! Argo application, the deployment and namespace, and the expected image or chart revision. The
//! mapping from a cm release line to pin names and LAN resources has no source in cm or the plan
//! yet (G11, an owner stop); [`rollout_for`] reads it, and with none it holds the step with a
//! packet item. After the pin merges, the LAN leg is one ordered run of effects (Argo refresh,
//! application, deployment, `/readyz`), and [`probe`] compares their receipts with the rollout's
//! expected values and merge identity: healthy receipts from the old deployment do not pass. The
//! result moves the step through the existing `cm advance … deployed --evidence` path. Only a
//! probe step that needs a live turn or LAN-only access files a MAC-LANE item.
//!
//! **Exclusivity (R24).** "One ready homelab PR" needs the open-PR list, which 11-D10 does not
//! open on rpi-homelab (G4, an owner stop). [`LanReads::homelab_pulls`] is the one reader; when it
//! cannot be read, the pin holds (`Hold::HomelabListUnread`).

#![allow(
    unused_variables,
    reason = "skeleton: tick/release-lan fills these bodies"
)]

use std::path::Path;

use crate::forge::{ForgeError, Gh};
use crate::tick::ledger::{EffectLedger, IntentId, IntentRow, Observation, Readmit};
use crate::tick::run::{Cycle, Deps, Planned};
use crate::tick::wire::{EffectInput, PinValue, Receipt};

pub const HOMELAB_REPO: &str = "xrl/rpi-homelab";

/// The homelab reads `Forge` does not have.
pub trait LanReads {
    /// Open PRs on rpi-homelab with their draft flag and head (G4: not in 11-D10's route table).
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
    /// The receipt row's `cycle`.
    pub cycle: u64,
    pub pass: bool,
}

/// What one rollout must reach (R12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rollout {
    /// The rollout step, `<NN-name>/<unit>/pin`.
    pub target: String,
    pub pins: Vec<PinValue>,
    pub application: String,
    pub deployment: String,
    pub namespace: String,
    /// The image reference or chart revision the deployment must show.
    pub expected: String,
    /// The merged pin PR's number and merge SHA, once there is one.
    pub merged: Option<(u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    ReadinessMissing,
    ReadinessFailed,
    ReadinessStale {
        cycle: u64,
    },
    AnotherHomelabPrReady {
        number: u32,
    },
    HomelabListUnread {
        why: String,
    },
    HomelabGateRed {
        why: Vec<String>,
    },
    HomelabDeployNotAuthorized,
    MergeMismatch {
        intent: IntentId,
    },
    /// G11: no mapping from the release line to pin names and LAN resources.
    NoRolloutMapping,
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

/// Missing, failed and stale each hold; `previous_tick` is the loaded `Cursor::tick`.
pub fn readiness_hold(readiness: Option<&Readiness>, previous_tick: u64) -> Option<Hold> {
    todo!("tick/release-lan")
}

/// Non-draft homelab PRs only.
pub fn ready_homelab_prs(pulls: &[HomelabPull]) -> usize {
    todo!("tick/release-lan")
}

/// The rollout for a ready rollout step, from G11's mapping once it has a source.
pub fn rollout_for(cycle: &Cycle, target: &str) -> Result<Rollout, Hold> {
    todo!("tick/release-lan")
}

pub fn pin_update(rollout: &Rollout, title: &str) -> Planned {
    todo!("tick/release-lan")
}

/// Built from the recorded `gh.pin.update` receipt of `update`.
pub fn pin_merge(ledger: &EffectLedger, update: &IntentId) -> Option<Planned> {
    todo!("tick/release-lan")
}

/// Argo refresh, application read, deployment read and `/readyz`, in that order.
pub fn lan_leg(rollout: &Rollout) -> Vec<EffectInput> {
    todo!("tick/release-lan")
}

/// The LAN leg's receipts compared with the rollout's expected values and merge identity.
pub fn probe(rollout: &Rollout, receipts: &[&Receipt]) -> ProbeResult {
    todo!("tick/release-lan")
}

/// A pin or kubectl intent's lookup. A kubectl read is not done (reads are emitted again); a pin
/// merge is done when the PR merged at its head; a pin update whose receipt was lost is
/// undecidable, since the branch name alone does not find its PR without a list read.
pub fn observe(root: &Path, deps: &Deps, row: &IntentRow) -> Observation {
    todo!("tick/release-lan")
}

/// R05: a pin or kubectl effect is emitted again only while `homelab_deploy` holds, no pin hold
/// applies, and for a pin merge the update receipt's head is still the PR's head.
pub fn readmit(cycle: &Cycle, row: &IntentRow) -> Readmit {
    todo!("tick/release-lan")
}

/// Stage 7 of `run`, second half: pin updates for ready rollout steps, pin merges from recorded
/// updates, the LAN leg after a merged pin, and probes from its receipts.
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

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn readiness_recorded_in_the_same_minute_as_the_previous_push_is_ordered_by_tick_number() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn healthy_receipts_from_the_old_deployment_do_not_pass_the_probe() {
        todo!()
    }
}
