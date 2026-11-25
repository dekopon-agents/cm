//! heartbeat.sh steps 1 and 5: commit what the tree holds, then `git pull --rebase` and push to
//! main, retried.
//!
//! Stub until `07-tick/mvp/housekeeping` fills [`sync`]. What it ports: off `main` it prints
//! `heartbeat: the checkout is on <branch|no branch>, not main; nothing committed, pulled or
//! pushed`; a dirty tree is committed (`git add -A`, `git commit -qm "chore(<slug after its last
//! dash>): heartbeat sync"`, else `heartbeat: git commit of the tree's changes failed; commit
//! them by hand`); [`Mode::IfDirty`] with a clean tree stops there; then up to 3 tries of
//! `git pull -q --rebase --autostash origin main` and `git push -q origin main`, each failure
//! followed by `git rebase --abort` and, before another try, a 2 s pause; after 3 failures
//! `heartbeat: git pull --rebase and push to main failed 3 times; main does not have this
//! checkout's commits`.

use std::time::Duration;

use crate::tick::act::{Ctx, Report};

/// The phase's budget: every git child shares it.
pub const BUDGET: Duration = Duration::from_secs(60);

/// When the phase pulls and pushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// heartbeat.sh step 1: always, after committing a dirty tree.
    Always,
    /// heartbeat.sh step 5, after the executors ran: only when the tree was dirty.
    IfDirty,
}

/// Commits, pulls and pushes the checkout; returns only lines that need a decision.
pub fn sync(ctx: &Ctx, mode: Mode) -> Report {
    let _ = (ctx, mode);
    Report::default()
}
