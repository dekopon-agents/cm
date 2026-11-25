//! `released-undeployed`: deploy a released, undeployed version (`deploy.sh`).
//!
//! Stub until `07-tick/mvp/exec-merge` fills [`ReleasedUndeployed::run`](Executor::run): it leaves the act line for the
//! model, as heartbeat.sh did for a line with no `act.d/` executor.

use crate::tick::act::{ActLine, Ctx, Executor, Outcome};

/// The `act.d/` name this executor answers to.
pub const KEY: &str = "released-undeployed";

pub struct ReleasedUndeployed;

impl Executor for ReleasedUndeployed {
    fn run(&self, line: &ActLine, _ctx: &Ctx) -> Outcome {
        Outcome::Left(line.text())
    }
}
