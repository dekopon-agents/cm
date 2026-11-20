//! Dispatch (`tick/core`): each ready step launched once, and reviews started with a nonce the
//! ledger holds before launch. A GPT review starts with `astra-review.sh start` until
//! `egress/gpt-review-route` is live; a Claude review is one MAC-LANE launcher item
//! (`tools/cloud-env/browser/claude-session-new.sh`), never `claude --cloud`.
//!
//! Task: launches run one at a time on the tick's thread, each a child process with
//! [`LAUNCH_TIMEOUT`]. Full-queue policy: at most [`MAX_LAUNCHES_PER_TICK`] per tick, oldest-ready
//! first; the rest are counted `deferred` and wait, with no packet item (waiting is not judgment).

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::host::Host;
use crate::tick::ledger::{EffectLedger, Family, IntentId, Nonce};

pub const MAX_LAUNCHES_PER_TICK: usize = 8;
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(300);

/// A step `cm next` reports ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub target: String,
    pub lane: crate::state::Lane,
    /// `cm` stamp the step became ready; the queue's order.
    pub since: String,
}

/// A review the tick owes a PR head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewWant {
    pub target: String,
    pub repo: String,
    pub pr: u64,
    pub head: String,
    /// The family that wrote the PR; the reviewer is the other one.
    pub writer: Family,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launched {
    Step {
        target: String,
        intent: IntentId,
    },
    Review {
        target: String,
        intent: IntentId,
        nonce: Nonce,
        session: Option<String>,
    },
    MacLaneItem {
        target: String,
        intent: IntentId,
        number: u32,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dispatched {
    pub launched: Vec<Launched>,
    pub deferred: usize,
}

/// The outward launch calls, so tests count them.
pub trait Launcher {
    /// `cm launch`'s checks and state move for the step; returns the launch command it printed.
    fn launch_step(&self, root: &Path, target: &str) -> crate::tick::Result<String>;
    /// `astra-review.sh start` with the nonce in the prompt; returns the thread id.
    fn astra_start(&self, want: &ReviewWant, nonce: &Nonce) -> crate::tick::Result<String>;
    /// Files one MAC-LANE item (a Claude review start, or a probe that needs the Mac).
    fn mac_lane_item(&self, root: &Path, title: &str, body: &str) -> crate::tick::Result<u32>;
}

pub struct ScriptLauncher {
    pub astra_review: PathBuf,
    pub timeout: Duration,
}

impl Launcher for ScriptLauncher {
    fn launch_step(&self, root: &Path, target: &str) -> crate::tick::Result<String> {
        todo!("tick/core")
    }

    fn astra_start(&self, want: &ReviewWant, nonce: &Nonce) -> crate::tick::Result<String> {
        todo!("tick/core")
    }

    fn mac_lane_item(&self, root: &Path, title: &str, body: &str) -> crate::tick::Result<u32> {
        todo!("tick/core")
    }
}

/// The ready steps and owed reviews with no open or bound launch intent in the ledger.
pub fn ready(
    root: &Path,
    ledger: &EffectLedger,
) -> crate::tick::Result<(Vec<Ready>, Vec<ReviewWant>)> {
    todo!("tick/core")
}

/// For each item, oldest first and up to the cap: write and push the intent (with a fresh nonce for
/// a review), then launch, then bind the returned id. A launch error parks that intent.
pub fn dispatch(
    host: &dyn Host,
    launcher: &dyn Launcher,
    root: &Path,
    ledger: &mut EffectLedger,
    steps: &[Ready],
    reviews: &[ReviewWant],
) -> crate::tick::Result<Dispatched> {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_launch_aborted_after_its_intent_is_reconciled_and_never_launched_twice() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_reloaded_ledger_makes_ready_skip_every_step_already_launched() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn at_most_eight_launches_per_tick_oldest_first_and_the_rest_deferred() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn restart_launches_one_astra_review_with_its_nonce_in_the_ledger_before_launch() {
        todo!()
    }
}
