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
