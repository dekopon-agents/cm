//! `merge-ready`: merge a PR whose gates all hold (`merge-pr.py`).
//!
//! Stub until `07-tick/mvp/exec-merge` fills [`MergeReady::run`](Executor::run): it leaves the act line for the
//! model, as heartbeat.sh did for a line with no `act.d/` executor.

use crate::tick::act::{ActLine, Ctx, Executor, Outcome};

/// The `act.d/` name this executor answers to.
pub const KEY: &str = "merge-ready";

pub struct MergeReady;

impl Executor for MergeReady {
    fn run(&self, line: &ActLine, _ctx: &Ctx) -> Outcome {
        Outcome::Left(line.text())
    }
}
