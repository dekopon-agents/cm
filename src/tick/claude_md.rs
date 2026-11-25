//! heartbeat.sh step 6: swap the coordinator's boot-time CLAUDE.md for the import line, once.
//!
//! Stub until `07-tick/mvp/housekeeping` fills [`run`]. What it ports: when line 1 of
//! `$HOME/.claude/CLAUDE.md` is `# dekopon-coordinator cloud environment`, replace the file with
//! `@/home/user/campaign-trail/tools/cloud-env/COORDINATOR.md` and a newline, printing
//! `rules: now imported from tools/cloud-env/COORDINATOR.md; read it`, or on a failed write
//! `heartbeat: could not write the COORDINATOR.md import to <path>`. Any other CLAUDE.md is never
//! touched, and a missing one is not created.

use std::time::Duration;

use crate::tick::act::{Ctx, Report};

/// The phase's budget.
pub const BUDGET: Duration = Duration::from_secs(5);

/// Replaces a boot-time CLAUDE.md with the COORDINATOR.md import.
pub fn run(ctx: &Ctx) -> Report {
    let _ = ctx;
    Report::default()
}
