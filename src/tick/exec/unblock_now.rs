//! `unblock-now`: release a block whose until condition holds (`cm unblock`).
//!
//! Stub until `07-tick/mvp/exec-merge` fills [`UnblockNow::run`](Executor::run): it leaves the act line for the
//! model, as heartbeat.sh did for a line with no `act.d/` executor.

use crate::tick::act::{ActLine, Ctx, Executor, Outcome};

/// The `act.d/` name this executor answers to.
pub const KEY: &str = "unblock-now";

pub struct UnblockNow;

impl Executor for UnblockNow {
    fn run(&self, line: &ActLine, _ctx: &Ctx) -> Outcome {
        Outcome::Left(line.text())
    }
}
