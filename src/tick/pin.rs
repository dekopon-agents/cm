//! heartbeat.sh step 2: run setup.sh's pinned cm.
//!
//! Stub until `07-tick/mvp/housekeeping` fills [`run`]. What it ports: read the `cm` row of
//! `bash tools/cloud-env/dekopon-coordinator-setup.sh --list-pins` (`cm|<version>|<asset>`);
//! when `cm --version` differs, fetch the asset with the same script's `--fetch-pinned cm`
//! (sha-checked; release, then the cloud-env-bin mirror), untar it and install its `cm` over
//! the one on `PATH`, so the act lines that follow come from the pinned cm; on any failure
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
