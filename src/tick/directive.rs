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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::proc::{FakeProc, Ran};

    /// The stub's own test; the card that fills this phase replaces it.
    #[test]
    fn the_stub_prints_nothing_and_runs_nothing() {
        let proc = FakeProc::new(|_| Ok(Ran::code(0, "")));
        let env = Env::default();
        let ctx = Ctx::new(
            Path::new("."),
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::from_secs(1),
        );
        assert_eq!(run(&ctx), Report::default());
        assert!(proc.calls().is_empty());
    }
}
