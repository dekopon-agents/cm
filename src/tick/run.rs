//! One tick, in order (`tick/core` owns the body; `tick/gate-merge` and `tick/release-lan` add their
//! stage calls here, so this file is their train's shared hot file):
//!
//! 1. `paused` live: if true, no effect, no launch; the packet says paused only on a change.
//! 2. `[authority]` at the approved SHA; unavailable parks with a packet and applies nothing.
//! 3. Ledger: push any unpushed commit, then [`reconcile`](crate::tick::ledger::reconcile).
//! 4. Intake since the cursor; timers due.
//! 5. Stage `gate-merge`: [`crate::tick::merge::stage`].
//! 6. Stage `release-lan`: [`crate::tick::release::stage`], then [`crate::tick::lan::stage`].
//! 7. Dispatch ready steps and owed reviews.
//! 8. Push the ledger with every new intent row, then save cursor and timers.
//! 9. Print [`EffectsOut`] (with `--effects`) or the outcome; mark the packet seen.
//!
//! An error before step 8 leaves cursor, timers and seen set as they were, so the next tick redoes
//! the work from durable state.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;

use crate::forge::Forge;
use crate::host::Host;
use crate::outcome::Outcome;
use crate::tick::authority::{ApprovedSha, Authority};
use crate::tick::dispatch::Launcher;
use crate::tick::intake::{Events, Intake};
use crate::tick::ledger::{EffectLedger, Observer};
use crate::tick::packet::DecisionPacket;
use crate::tick::push::Git;
use crate::tick::wire::{Effect, EffectsOut};

/// `cm tick`'s flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickArgs {
    /// The runner entry's SHA (11-D2, 11-D4).
    pub approved: String,
    /// 11-D3 step (1): print [`EffectsOut`] instead of the outcome.
    pub effects: bool,
}

/// Everything a tick reaches outside the campaign folder.
pub struct Deps<'a> {
    pub host: &'a dyn Host,
    pub forge: &'a dyn Forge,
    pub git: &'a dyn Git,
    pub events: &'a dyn Events,
    pub launcher: &'a dyn Launcher,
    pub observer: &'a dyn Observer,
}

/// The state one tick threads through its stages. Stages append effects and packet items; only
/// step 8 makes anything durable beyond the ledger rows they write.
pub struct Cycle<'a> {
    pub deps: &'a Deps<'a>,
    pub root: PathBuf,
    pub checkout: PathBuf,
    pub authority: Authority,
    pub ledger: EffectLedger,
    pub intake: Intake,
    pub effects: Vec<Effect>,
    pub packet: DecisionPacket,
    /// `cm` stamp of this tick.
    pub now: String,
}

/// What a tick produced.
#[derive(Debug)]
pub struct TickRun {
    pub out: EffectsOut,
    pub outcome: Outcome,
}

/// Runs one tick against `root`.
pub fn tick(deps: &Deps, root: &Path, sha: &ApprovedSha, args: &TickArgs) -> Result<TickRun> {
    todo!("tick/core")
}

/// The CLI seam: builds the system deps, runs [`tick`], and with `--effects` prints the wire JSON
/// itself and returns an empty outcome.
pub fn cli_tick(host: &dyn Host, root: &Path, args: &TickArgs) -> Result<(Outcome, ExitCode)> {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_tick_that_errors_before_its_push_leaves_cursor_timers_and_seen_set_unchanged() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_second_tick_over_unchanged_state_emits_no_effect_and_an_empty_packet() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_tick_emits_at_most_sixteen_effects_and_its_stdout_stays_under_sixty_four_kib() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn every_emitted_effect_has_its_intent_row_pushed_before_stdout_is_written() {
        todo!()
    }
}
