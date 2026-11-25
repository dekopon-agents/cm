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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::proc::{FakeProc, Ran};

    /// The stub's own test; the card that fills [`ReleasedUndeployed`] replaces it.
    #[test]
    fn the_stub_leaves_its_line_for_the_model() {
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
        let line = ActLine {
            json: json!({"kind": "released-undeployed", "target": "01-a/u/b", "command": "cm x 01-a/u/b"}),
            lane: None,
        };
        assert_eq!(
            ReleasedUndeployed.run(&line, &ctx),
            Outcome::Left(line.text())
        );
        assert!(proc.calls().is_empty());
    }
}
