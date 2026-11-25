//! The collect phase: gather what earlier ticks' executors started, before `cm next --act`
//! reads the campaign.
//!
//! Stub until `07-tick/mvp/housekeeping` fills [`run`]. heartbeat.sh has no such step; the
//! phase runs after the owner directives and before the act loop.

use std::time::Duration;

use crate::tick::act::{Ctx, Report};

/// The phase's budget.
pub const BUDGET: Duration = Duration::from_secs(30);

/// Collects the results earlier moves left; returns only lines that need a decision.
pub fn run(ctx: &Ctx) -> Report {
    let _ = ctx;
    Report::default()
}
