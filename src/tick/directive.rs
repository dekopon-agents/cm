//! heartbeat.sh step 3: list the owner directives nobody picked up.
//!
//! Stub until `07-tick/mvp/housekeeping` fills [`run`]. What it ports: the `stamp_entry=` of
//! every `Handoff: ` line in `<epoch>/handoff/coordinator-*.md` (`none` ignored); more than one
//! distinct stamp prints `heartbeat: coordinator handoffs name more than one stamp_entry
//! (<stamps>); owner directives not listed`; a stamp with no `## <stamp> ` heading in
//! JOURNAL.md prints `heartbeat: no JOURNAL.md heading for stamp_entry <stamp>; owner directives
//! not listed`; otherwise each `## <time> — Owner directive:` heading below the stamp's, with
//! no `— Directive picked up: <heading>` line in the journal, prints
//! `owner-directive: <heading>`.

use std::time::Duration;

use crate::tick::act::{Ctx, Report};

/// The phase's budget.
pub const BUDGET: Duration = Duration::from_secs(10);

/// Prints `owner-directive: <heading>` for each unpicked owner directive.
pub fn run(ctx: &Ctx) -> Report {
    let _ = ctx;
    Report::default()
}
