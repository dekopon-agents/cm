//! `launchable-not-launched.codex`: launch a launchable codex step (`$CODEX_CLOUD_PY`).
//!
//! Stub until `07-tick/mvp/exec-codex` fills [`CodexDispatch::run`](Executor::run): it leaves the act line for the
//! model, as heartbeat.sh did for a line with no `act.d/` executor.

use crate::tick::act::{ActLine, Ctx, Executor, Outcome};

/// The `act.d/` name this executor answers to.
pub const KEY: &str = "launchable-not-launched.codex";

pub struct CodexDispatch;

impl Executor for CodexDispatch {
    fn run(&self, line: &ActLine, _ctx: &Ctx) -> Outcome {
        Outcome::Left(line.text())
    }
}
