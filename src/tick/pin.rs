//! heartbeat.sh step 2: run setup.sh's pinned cm.
//!
//! Stub until `07-tick/mvp/housekeeping` fills [`run`]. What it ports: read the `cm` row of
//! `bash tools/cloud-env/dekopon-coordinator-setup.sh --list-pins` (`cm|<version>|<asset>`);
//! when `ctx.cm`'s `--version` differs, fetch the asset with the same script's `--fetch-pinned
//! cm` (sha-checked; release, then the cloud-env-bin mirror), untar it and install its `cm` at
//! `ctx.cm`, the program the act list is read from (by default this executable's resolved path:
//! with a symlinked `cm` on `PATH`, the symlink's target), so the act lines that follow come from
//! the pinned cm (heartbeat.sh installed over `command -v cm`); on any failure
//! `heartbeat: cm <pinned> (setup.sh's pin) could not be fetched or installed; still running cm
//! <running|none>`.

use std::time::Duration;

use crate::tick::act::{Ctx, Report};

/// The phase's budget: the fetch and install share it.
pub const BUDGET: Duration = Duration::from_secs(60);

/// Installs setup.sh's pinned cm when the running one differs.
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
