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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::proc::{FakeProc, Ran};

    /// The stub's own test; the card that fills [`MergeReady`] replaces it.
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
            json: json!({"kind": "merge-ready", "target": "01-a/u/b", "command": "cm x 01-a/u/b"}),
            lane: None,
        };
        assert_eq!(MergeReady.run(&line, &ctx), Outcome::Left(line.text()));
        assert!(proc.calls().is_empty());
    }
}
