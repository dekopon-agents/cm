//! The collect phase (review-latency unit U4): run `astra-review.sh collect` on every gating
//! review out-dir that has a receipt request but no `report.md`. No act line exists for this, so
//! it is a phase, not an executor.
//!
//! Stub until `07-tick/mvp/exec-codex` fills [`run`]. `beat` runs it after the owner directives
//! and before `cm --json next --act` is read, so a verdict collected this tick is already
//! recorded when the act list is built (`merge-ready` can fire the same tick), and only in the
//! bound coordinator: it writes cm records, and there is one Codex login per epoch. Other
//! sessions skip it silently.

use std::time::Duration;

use crate::tick::act::{Ctx, Report};

/// The phase's budget: every `astra-review.sh collect` shares it.
pub const BUDGET: Duration = Duration::from_secs(30);

/// Collects the review reports earlier ticks dispatched; returns only lines that need a decision.
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
