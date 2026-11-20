//! One tick (`tick/core`; S-tick-41, S-tick-42). The stage order is fixed here:
//!
//! 0. With `--clone`, `push::ensure_checkout` (11-D7, G5). Then [`TickLock`](crate::tick::ledger::TickLock) without waiting;
//!    held elsewhere: exit 0, no effects, no packet.
//! 1. `push::begin`: the union check, recovery of dirty cm-owned files, the unpushed commit, then
//!    `pull --no-rebase` (R02, R03).
//! 2. `authority::paused_live`, then `Approved::read` at `--approved` (11-D4). Unreadable: one
//!    `authority-unavailable` item, and stage 3's retries and stages 6 to 8 plan nothing.
//! 3. `ledger::reconcile` through `SystemObserver`; then the due timers (retry, collect, review
//!    deadline, reminder, action deadline: a handed intent past it is parked, R18); then each new
//!    `refused` receipt since `Cursor::receipts_done` becomes one `effect-refused` item (R17).
//!    Each retry then goes through `ledger::readmit` to its owner and, when admitted, is planned
//!    again under its id with [`Cycle::replan`] (R05).
//! 4. `sync::sync`, the existing `cm sync` (pr-open → merged → released → deployed, CARD.md);
//!    each of its violations is one `sync` item. `tick/gate-merge` swaps this one line for
//!    `sync::sync_proved` with the cycle's approved context (R07).
//! 5. `intake::intake` over the watched PRs.
//! 6. `merge::stage` (`tick/gate-merge` adds this one line).
//! 7. `release::stage`, then `lan::stage` (`tick/release-lan` adds these two lines).
//! 8. `dispatch::plan_stage`.
//! 9. `wire::admit` the planned effects beside the packet; write their intent and `emitted` rows
//!    under tick number `Cursor::tick + 1`; save the cursor (the loaded one, with this tick's
//!    handled marks and requests, R04), timers and seen set; the first commit, pushed
//!    (`push::commit_and_push`). Nothing external has happened yet.
//! 10. `dispatch::launch_stage`; then with `--effects` the [`EffectsOut`] line is the output, and
//!     without it `apply::apply_in_order` through `LocalApply` and `record::record_receipts`.
//! 11. The second commit: binds, attempt ids, receipts. A failed push leaves it local, and the next
//!     tick's stage 1 pushes it first.
//!
//! While `paused` is true, stage 3's retries and stages 6 to 8 plan nothing and stage 10 launches and applies
//! nothing; reconcile, sync, intake and review collection still run. An error in stages 1 to 9
//! leaves the pushed cursor, timers, seen set and ledger as they were, so the next tick redoes the
//! work.
//!
//! S-tick-42, task: the tick itself. **Owner:** the `cm` process's main thread, which spawns no
//! thread of its own beyond `forge::run`'s two pipe threads per child. **Bound:** one tick per
//! clone at a time ([`TickLock`](crate::tick::ledger::TickLock)); each stage's bound is its module's (S-tick-06). **Queue:** none
//! in memory; what waits is in the ledger, the timers or the cursor. **Shutdown:** a killed tick
//! leaves at most an unpushed commit, dirty cm-owned files and a torn ledger line, which the next
//! tick's stage 1 and the ledger's exclusive open handle.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::forge::Forge;
use crate::host::Host;
use crate::outcome::Outcome;
use crate::plan::Units;
use crate::tick::agents::AgentRecorder;
use crate::tick::apply::{Applier, Apply};
use crate::tick::authority::{Approved, ApprovedSha};
use crate::tick::dispatch::Launcher;
use crate::tick::intake::{Cursor, Event, PullReads};
use crate::tick::lan::LanReads;
use crate::tick::ledger::{Draft, EffectLedger, IntentId};
use crate::tick::merge::MergeReads;
use crate::tick::packet::{DecisionPacket, Seen};
use crate::tick::push::{Git, Pushed};
use crate::tick::release::ReleaseReads;
use crate::tick::timers::Timers;
use crate::tick::wire::{EffectInput, EffectsOut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickArgs {
    /// The runner entry's SHA (11-D2, 11-D4); in the interim, the stamp SHA the coordinator
    /// passes. Never read from EPOCH.md.
    pub approved: ApprovedSha,
    /// 11-D3 step (1): print [`EffectsOut`] for the runner instead of applying locally.
    pub effects: bool,
    /// 11-D7: the repository to clone when the campaign's checkout is absent.
    pub clone: Option<String>,
}

/// Everything a tick reaches outside the campaign folder, so tests run on fixtures.
pub struct Deps<'a> {
    pub host: &'a dyn Host,
    pub forge: &'a dyn Forge,
    pub git: &'a dyn Git,
    pub pulls: &'a dyn PullReads,
    pub merges: &'a dyn MergeReads,
    pub releases: &'a dyn ReleaseReads,
    pub homelab: &'a dyn LanReads,
    pub launcher: &'a dyn Launcher,
    /// R15: launches and collected receipts reach the agent ledger through this.
    pub agents: &'a dyn AgentRecorder,
    /// `None` with `--effects`: the runner applies.
    pub apply: Option<&'a dyn Apply>,
}

/// An effect a stage wants, before [`crate::tick::wire::admit`] lets it through.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub draft: Draft,
    pub input: EffectInput,
    /// A readmitted retry keeps its intent id (R05); a new effect gets one at stage 9.
    pub retry: Option<IntentId>,
}

/// What a tick counted; printed as the outcome and journaled when non-zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    pub bound: usize,
    pub parked: usize,
    pub retried: usize,
    /// Intents left open because a lookup failed.
    pub unread_intents: usize,
    /// Watched PRs whose read failed.
    pub unread_prs: usize,
    pub deferred_reads: usize,
    pub deferred_effects: usize,
    pub deferred_launches: usize,
}

/// The state one tick threads through its stages. Stages add planned effects, launch intents and
/// packet items; only stages 9 and 11 make anything durable beyond the ledger rows they append.
pub struct Cycle<'a> {
    pub deps: &'a Deps<'a>,
    pub root: PathBuf,
    pub checkout: PathBuf,
    /// `cm` stamp of this tick's start.
    pub now: String,
    /// This tick's number: the loaded `Cursor::tick` + 1.
    pub tick: u64,
    pub applier: Applier,
    pub paused: bool,
    /// `None` when stage 2 could not read the approved revision.
    pub approved: Option<Approved>,
    pub units: Units,
    pub ledger: EffectLedger,
    pub timers: Timers,
    /// As loaded; stage 9 saves it with this tick's marks applied.
    pub cursor: Cursor,
    pub seen: Seen,
    pub events: Vec<Event>,
    /// PR URL to the newest comment id a stage handled this tick (R04).
    pub handled: BTreeMap<String, u64>,
    pub planned: Vec<Planned>,
    pub packet: DecisionPacket,
    /// The tick's own `.cm/tick/` files, relative to the checkout: committed with `--force`.
    pub forced: BTreeSet<PathBuf>,
    /// Other paths the tick wrote outside the cm-owned set (an Astra out-dir): committed without
    /// `--force` (S-tick-20).
    pub wrote: BTreeSet<PathBuf>,
    pub counters: Counters,
}

impl Cycle<'_> {
    /// Queues a new effect for stage 9; refused, with a packet item, while paused or without
    /// authority.
    pub fn plan(&mut self, draft: Draft, input: EffectInput) {
        todo!("tick/core")
    }

    /// Queues a readmitted retry under its own id (R05).
    pub fn replan(&mut self, id: IntentId, draft: Draft, input: EffectInput) {
        todo!("tick/core")
    }

    /// Marks a comment handled, so the saved cursor may pass it (R04).
    pub fn handled(&mut self, url: &str, comment: u64) {
        todo!("tick/core")
    }

    /// Records a path, absolute or relative to the campaign folder, for the next commit.
    pub fn wrote(&mut self, path: &Path) {
        todo!("tick/core")
    }
}

#[derive(Debug)]
pub struct TickRun {
    pub out: EffectsOut,
    pub counters: Counters,
    pub first: Option<Pushed>,
    pub second: Option<Pushed>,
}

/// Runs one tick against the campaign folder `root`. `None` when another tick holds the lock.
pub fn tick(deps: &Deps, root: &Path, args: &TickArgs) -> crate::tick::Result<Option<TickRun>> {
    todo!("tick/core")
}

/// The CLI seam: builds the system deps (`forge::Gh`, `SystemGit`, `ScriptLauncher`,
/// `CmAgentRecorder`, and `LocalApply` unless `--effects`), runs [`tick`], and maps the result.
/// With `--effects` it prints the one [`EffectsOut`] line itself, turns a tick error into a
/// `tick-aborted` item, and returns an empty outcome with exit 0. Without it, the outcome carries
/// the counters and the packet's text, and a tick error is the command's error.
pub fn cli_tick(
    host: &dyn Host,
    root: &Path,
    args: &TickArgs,
) -> anyhow::Result<(Outcome, ExitCode)> {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_tick_that_errors_before_its_first_push_leaves_ledger_cursor_timers_and_seen_set_unchanged()
    {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_second_tick_over_unchanged_state_emits_no_effect_launches_nothing_and_has_an_empty_packet()
    {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_tick_emits_at_most_sixteen_effects_and_its_stdout_stays_under_sixty_four_kib() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn every_effect_and_launch_has_its_intent_row_pushed_before_it_happens() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_second_tick_started_while_one_runs_exits_at_once_and_writes_nothing() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn with_effects_a_failing_tick_still_prints_one_line_with_a_tick_aborted_item() {
        todo!()
    }
}
