//! `review-owed`: dispatch the review a PR is owed (`astra-review.sh`).
//!
//! Stub until `07-tick/mvp/exec-merge` fills [`ReviewOwed::run`](Executor::run): it leaves the act line for the
//! model, as heartbeat.sh did for a line with no `act.d/` executor.

use crate::tick::act::{ActLine, Ctx, Executor, Outcome};

/// The `act.d/` name this executor answers to.
pub const KEY: &str = "review-owed";

pub struct ReviewOwed;

impl Executor for ReviewOwed {
    fn run(&self, line: &ActLine, _ctx: &Ctx) -> Outcome {
        Outcome::Left(line.text())
    }
}
