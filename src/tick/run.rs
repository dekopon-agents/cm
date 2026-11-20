//! One tick (`tick/core`; S-tick-41, S-tick-42). The stage order is fixed here:
//!
//! 0. [`TickLock`](crate::tick::ledger::TickLock) without waiting. Held elsewhere: exit 0, no effects, no packet.
//! 1. `push::begin`: push a commit an earlier tick left unpushed, then `pull --no-rebase` (11-D7).
//! 2. `authority::paused_live`, then `Approved::read` at `--approved` (11-D4). Unreadable: one
//!    `authority-unavailable` item, and stages 6 to 8 plan nothing.
//! 3. `ledger::reconcile` through `SystemObserver`; then the due timers (retry, collect, review
//!    deadline, reminder).
//! 4. `sync::sync`, the existing `cm sync` (pr-open → merged → released → deployed, CARD.md);
//!    each of its violations (a merge-gate gap, a PR closed unmerged) is one `sync` packet item.
//! 5. `intake::intake` over the watched PRs.
//! 6. `merge::stage` (`tick/gate-merge` adds this one line).
//! 7. `release::stage`, then `lan::stage` (`tick/release-lan` adds these two lines).
//! 8. `dispatch::plan_stage`.
//! 9. `wire::admit` the planned effects beside the packet; write their intent rows; save the
//!    cursor, timers and seen set; the first commit, pushed (`push::commit_and_push`). Nothing
//!    external has happened yet.
//! 10. `dispatch::launch_stage`; then with `--effects` the [`EffectsOut`] line is the output, and
//!     without it `apply::apply_in_order` through `LocalApply` and `record::record_receipts`.
//! 11. The second commit: binds, attempt ids, receipts. A failed push leaves it local, and the next
//!     tick's stage 1 pushes it first.
//!
//! While `paused` is true, stages 6 to 8 plan nothing and stage 10 launches and applies nothing;
//! reconcile, sync, intake and review collection still run. An error in stages 1 to 9 leaves the
//! pushed cursor, timers, seen set and ledger as they were, so the next tick redoes the work.
//!
//! S-tick-42, task: the tick itself. **Owner:** the `cm` process's main thread, which spawns no
//! thread of its own beyond `forge::run`'s two pipe threads per child. **Bound:** one tick per
//! campaign at a time ([`TickLock`](crate::tick::ledger::TickLock)); each stage's bound is its module's (S-tick-06). **Queue:**
//! none in memory; what waits is in the ledger, the timers or the cursor. **Shutdown:** a killed
//! tick leaves at most an unpushed commit and an unterminated ledger line, both handled by the
//! next tick's stages 1 and 3.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::forge::Forge;
use crate::host::Host;
use crate::outcome::Outcome;
use crate::plan::Units;
use crate::tick::apply::{Applier, Apply};
use crate::tick::authority::{Approved, ApprovedSha};
use crate::tick::dispatch::Launcher;
use crate::tick::intake::{Cursor, Event, PullReads};
use crate::tick::lan::LanReads;
use crate::tick::ledger::{Draft, EffectLedger};
use crate::tick::merge::MergeReads;
use crate::tick::packet::{DecisionPacket, Seen};
use crate::tick::push::{Git, Pushed};
use crate::tick::release::ReleaseReads;
use crate::tick::timers::Timers;
use crate::tick::wire::{EffectInput, EffectsOut};

/// `cm tick`'s flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickArgs {
    /// The runner entry's SHA (11-D2, 11-D4); in the interim, the stamp SHA the coordinator
    /// passes. Never read from EPOCH.md.
    pub approved: ApprovedSha,
    /// 11-D3 step (1): print [`EffectsOut`] for the runner instead of applying locally.
    pub effects: bool,
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
    /// `None` with `--effects`: the runner applies.
    pub apply: Option<&'a dyn Apply>,
}

/// An effect a stage wants, before [`crate::tick::wire::admit`] lets it through and the ledger
/// gives it an intent id.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub draft: Draft,
    pub input: EffectInput,
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
    pub applier: Applier,
    pub paused: bool,
    /// `None` when stage 2 could not read the approved revision.
    pub approved: Option<Approved>,
    pub units: Units,
    pub ledger: EffectLedger,
    pub timers: Timers,
    pub cursor: Cursor,
    pub seen: Seen,
    pub events: Vec<Event>,
    pub planned: Vec<Planned>,
    pub packet: DecisionPacket,
    /// The tick's own files it wrote, relative to the checkout: committed with `--force`.
    pub written: BTreeSet<PathBuf>,
    /// `Git::dirty` under the campaign folder at stage 1: what the commits leave out unless the
    /// tick wrote it too (S-tick-20).
    pub dirty_at_start: BTreeSet<PathBuf>,
    pub counters: Counters,
}

impl Cycle<'_> {
    /// Queues an effect for stage 9; refused (with a packet item) while paused or without
    /// authority.
    pub fn plan(&mut self, draft: Draft, input: EffectInput) {
        todo!("tick/core")
    }

    /// Records that this tick wrote `path` (absolute, or relative to the campaign folder).
    pub fn wrote(&mut self, path: &Path) {
        todo!("tick/core")
    }
}

/// What a tick produced.
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

/// The CLI seam: builds the system deps (`forge::Gh`, `SystemGit`, `ScriptLauncher`, and
/// `LocalApply` unless `--effects`), runs [`tick`], and maps the result. With `--effects` it prints
/// the one [`EffectsOut`] line itself, turns a tick error into a `tick-aborted` item, and returns
/// an empty outcome with exit 0. Without it, the outcome carries the counters and the packet's
/// text, and a tick error is the command's error.
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
