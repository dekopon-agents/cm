//! Dispatch (`tick/core`): each ready step launched once, and each owed review started once with
//! a nonce the ledger holds before launch (IDEA-0063 §Mechanism; PLAN.md `07-tick/core`).
//!
//! Two halves around the tick's first push (S-tick-41):
//! - [`plan_stage`], before the push: picks the ready steps (`plan::summary`, `cm next`'s order)
//!   and the owed reviews, and writes one launch intent each; a step launch also runs the existing
//!   `cm launch` (`commands::launch_remote`) and `cm attempt begin`, so the attempt record and the
//!   intent row reach main together.
//! - [`launch_stage`], after the push: performs each launch by its [`LaunchRoute`], binds the
//!   intent and records the attempt id (`cm attempt record`). A launch error parks that intent;
//!   the next one still runs.
//!
//! Routes ([`route`]): a GPT review is `astra-review.sh start` until the phase is gpt-route
//! (EXECUTION.md §Lanes); a Claude session (a Claude review, or a claude-cloud step) is a decision
//! packet action for the coordinator's `create_session` when cm applies locally (the cloud
//! interim, F1), and one MAC-LANE launcher item (`tools/cloud-env/browser/claude-session-new.sh`)
//! when the runner applies (the RPi); never `claude --cloud`. A Codex step, a coordinator or
//! fable step, and a gpt-route review request are packet actions: cm holds no capability for them
//! (11-D19; SKELETON.md, Named gaps). Owner-lane steps are never dispatched.
//!
//! S-tick-38, task. **Owner:** the tick's thread; each `astra-review.sh` call is one child through
//! `forge::run` with [`LAUNCH_TIMEOUT`]. **Bound:** [`MAX_LAUNCHES_PER_TICK`] launch intents per
//! tick, reviews first (they gate merges), then steps in `cm next` order. **Full queue:** the rest
//! are counted `deferred` and are picked again next tick; waiting is not judgment, so no packet
//! item. **Shutdown:** every launch's intent row is on main before the launch, so a launch killed
//! mid-call is reconciled by [`observe_review`] or [`observe_step`] and never repeated.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::plan::Units;
use crate::state::Lane;
use crate::tick::apply::Applier;
use crate::tick::authority::Phase;
use crate::tick::intake::Event;
use crate::tick::ledger::{Bind, EffectLedger, Family, IntentId, IntentRow, Observation};
use crate::tick::run::{Cycle, Deps};

pub const MAX_LAUNCHES_PER_TICK: usize = 8;
/// `astra-review.sh start` sends `thread/start` and `turn/start` and returns; `collect` returns at
/// once.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(300);

/// Where a launch goes; recorded on the intent so reconcile knows where to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchRoute {
    /// `astra-review.sh start` into the step's `evidence/astra-review-<pr>[-r<n>]`.
    AstraScript,
    /// One MAC-LANE.md item that carries the nonce.
    MacLane,
    /// A decision-packet action the coordinator performs and records with `cm attempt record`.
    Packet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Build,
    Review,
}

/// The route for one launch.
pub fn route(
    purpose: Purpose,
    lane: Option<Lane>,
    reviewer: Option<Family>,
    phase: Phase,
    applier: Applier,
) -> Option<LaunchRoute> {
    todo!("tick/core")
}

/// A step `cm next` reports ready, with no open launch intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub target: String,
    pub lane: Option<Lane>,
}

/// A review the tick owes a PR head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewWant {
    pub target: String,
    pub part: Option<String>,
    pub url: String,
    pub repo: String,
    pub pr: u32,
    pub head: String,
    pub base: String,
    pub writer: Family,
    pub reviewer: Family,
    pub model: String,
    pub effort: String,
    /// The `@dekopon review` comment that asked, if one did.
    pub requested_by: Option<u64>,
}

/// One `astra-review.sh start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstraStart {
    pub repo: String,
    pub pr: u32,
    pub base: String,
    pub head: String,
    /// The step's kickoff plus its coordinator-scope.md (`cm brief`), written to a local file.
    pub brief: PathBuf,
    pub out_dir: PathBuf,
    pub round: Option<u32>,
    pub prev_head: Option<String>,
    pub findings: Option<PathBuf>,
}

/// `astra-review.sh collect`'s exit codes 3, 0, 1 and 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Collected {
    Pending,
    /// Recorded with `cm review record` by the script itself.
    Recorded,
    Blocked,
    NotRecorded {
        why: String,
    },
}

/// The outward launch calls, so tests count them.
pub trait Launcher {
    /// Runs `start`, then reads the thread id from the out-dir's `ids.json`.
    fn astra_start(&self, start: &AstraStart) -> crate::tick::Result<Option<String>>;
    fn astra_collect(&self, out_dir: &Path) -> crate::tick::Result<Collected>;
}

pub struct ScriptLauncher<'a> {
    /// campaign-trail `tools/cloud-env/astra-review.sh`, found from the checkout.
    pub script: PathBuf,
    pub timeout: Duration,
    /// `astra-review.sh collect` runs `cm review record -C <campaign>`, a second cm process that
    /// takes the campaign lock: the tick holds no lock across this call (S-tick-05).
    pub campaign: &'a Path,
}

impl Launcher for ScriptLauncher<'_> {
    fn astra_start(&self, start: &AstraStart) -> crate::tick::Result<Option<String>> {
        todo!("tick/core")
    }

    fn astra_collect(&self, out_dir: &Path) -> crate::tick::Result<Collected> {
        todo!("tick/core")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub intent: IntentId,
    pub target: String,
    pub route: LaunchRoute,
    pub bind: Option<Bind>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dispatched {
    pub launched: Vec<Launched>,
    /// Picked again next tick.
    pub deferred: usize,
    pub parked: Vec<IntentId>,
}

/// The ready steps with no open or parked launch intent, in `cm next` order.
pub fn ready(root: &Path, units: &Units, ledger: &EffectLedger) -> Vec<Ready> {
    todo!("tick/core")
}

/// The reviews owed from this tick's events: one per watched PR head that has none in flight or
/// bound, from the family that did not write the PR, with the model and effort its phase names.
pub fn reviews_owed(cycle: &Cycle, events: &[Event]) -> Vec<ReviewWant> {
    todo!("tick/core")
}

/// Before the push: launch intents for owed reviews, then ready steps, up to the bound.
pub fn plan_stage(cycle: &mut Cycle) -> crate::tick::Result<()> {
    todo!("tick/core")
}

/// After the push: perform each planned launch and bind it, and poll in-flight Astra reviews whose
/// `collect` timer is due.
pub fn launch_stage(cycle: &mut Cycle) -> crate::tick::Result<Dispatched> {
    todo!("tick/core")
}

/// A step launch's lookup: `attempt::plan` on the named attempt. `resume` or `ended` binds,
/// `unknown` (began, no id) is undecidable, `dispatch` (nothing reached the runtime) is not done.
pub fn observe_step(root: &Path, row: &IntentRow) -> Observation {
    todo!("tick/core")
}

/// A review launch's lookup by its route: the out-dir's `ids.json` thread id; the MAC-LANE item
/// that carries the nonce; or the packet action's recorded attempt. An out-dir with a prompt but
/// no thread id is undecidable.
pub fn observe_review(root: &Path, deps: &Deps, row: &IntentRow) -> Observation {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_launch_killed_after_its_intent_is_pushed_is_reconciled_and_never_launched_twice() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_reloaded_ledger_makes_ready_skip_every_step_with_an_open_or_parked_launch() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn at_most_eight_launches_per_tick_reviews_first_and_the_rest_deferred() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_restart_starts_one_astra_review_whose_nonce_was_on_main_before_the_start() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_launch_error_parks_that_intent_and_the_next_launch_still_runs() {
        todo!()
    }
}
