//! Git for the tick (`tick/ledger`): the pull each tick starts with (11-D7), the ledger's one
//! direct push (11-D8), and the `git show` reads the authority uses (11-D4).
//!
//! S-tick-19, task. **Owner:** the tick's thread; every call is one child process run through
//! `forge::run` with [`GIT_TIMEOUT`] and killed on expiry. **Bound:** one child at a time.
//! **Queue:** none. **Shutdown:** the process exits; a commit that did not push stays local, and
//! the next tick's [`begin`] pushes it before anything else, so no effect is emitted whose intent
//! row is not on main.
//!
//! What a tick commits: the paths it names in `Cycle::written` (its own `.cm/tick/` files), added
//! with `git add --force` because campaign-trail's top-level `.gitignore` ignores `*.jsonl`; plus
//! every path under the campaign folder that [`Git::dirty`] lists now but did not list when the
//! tick started (`Cycle::dirty_at_start`): what `cm sync`, `cm launch`, `cm attempt` and
//! `astra-review.sh start` wrote, added without `--force`, so an ignored transcript stays out. A
//! file another writer left dirty before the tick started is committed only if the tick wrote it
//! too. The tick never uses `--force` on a push and never pushes another ref.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::forge::CommitSha;

/// 11-D8: at most three tries, then [`crate::tick::TickError::PushExhausted`].
pub const PUSH_TRIES: u8 = 3;
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullResult {
    /// Up to date, or a clean merge commit.
    Merged,
    /// The merge conflicted and was aborted (`git merge --abort`); the tree is as before.
    Conflict { paths: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    Pushed,
    NonFastForward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    /// `None` when there was nothing to commit and nothing unpushed.
    pub commit: Option<CommitSha>,
    pub tries: u8,
}

/// The git a tick needs, so tests run against a local bare remote.
pub trait Git {
    /// `git rev-parse --show-toplevel` of the campaign folder: the checkout.
    fn toplevel(&self, dir: &Path) -> crate::tick::Result<PathBuf>;
    /// `git pull --no-rebase origin main`; on a conflict, `git merge --abort` and the paths.
    fn pull_no_rebase(&self, repo: &Path) -> crate::tick::Result<PullResult>;
    /// Modified and untracked paths under `under` (`git status --porcelain -- <under>`), relative
    /// to the checkout; ignored files are not listed.
    fn dirty(&self, repo: &Path, under: &Path) -> crate::tick::Result<BTreeSet<PathBuf>>;
    /// `git add --force -- <forced>`, `git add -- <paths>`, then a commit of exactly those paths;
    /// `None` when none changed.
    fn commit(
        &self,
        repo: &Path,
        forced: &[PathBuf],
        paths: &[PathBuf],
        message: &str,
    ) -> crate::tick::Result<Option<CommitSha>>;
    /// `git push origin HEAD:main`.
    fn push_main(&self, repo: &Path) -> crate::tick::Result<PushResult>;
    /// Whether HEAD holds commits `origin/main` lacks.
    fn unpushed(&self, repo: &Path) -> crate::tick::Result<bool>;
    /// `git show <sha>:<path>`; `None` when the path is absent at that revision.
    fn show(&self, repo: &Path, sha: &str, path: &str) -> crate::tick::Result<Option<String>>;
    /// Whether `sha` is a commit in the local object store.
    fn has_commit(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool>;
    /// `git fetch origin <sha>`; `false` when the remote refuses it.
    fn fetch_sha(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool>;
}

pub struct SystemGit {
    pub program: PathBuf,
    pub timeout: Duration,
}

/// Skeleton glue.
impl Default for SystemGit {
    fn default() -> Self {
        SystemGit {
            program: PathBuf::from("git"),
            timeout: GIT_TIMEOUT,
        }
    }
}

impl Git for SystemGit {
    fn toplevel(&self, dir: &Path) -> crate::tick::Result<PathBuf> {
        todo!("tick/ledger")
    }

    fn pull_no_rebase(&self, repo: &Path) -> crate::tick::Result<PullResult> {
        todo!("tick/ledger")
    }

    fn dirty(&self, repo: &Path, under: &Path) -> crate::tick::Result<BTreeSet<PathBuf>> {
        todo!("tick/ledger")
    }

    fn commit(
        &self,
        repo: &Path,
        forced: &[PathBuf],
        paths: &[PathBuf],
        message: &str,
    ) -> crate::tick::Result<Option<CommitSha>> {
        todo!("tick/ledger")
    }

    fn push_main(&self, repo: &Path) -> crate::tick::Result<PushResult> {
        todo!("tick/ledger")
    }

    fn unpushed(&self, repo: &Path) -> crate::tick::Result<bool> {
        todo!("tick/ledger")
    }

    fn show(&self, repo: &Path, sha: &str, path: &str) -> crate::tick::Result<Option<String>> {
        todo!("tick/ledger")
    }

    fn has_commit(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool> {
        todo!("tick/ledger")
    }

    fn fetch_sha(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool> {
        todo!("tick/ledger")
    }
}

/// A tick's first git act: push any commit an earlier tick left unpushed (with
/// [`commit_and_push`]'s tries), then `pull --no-rebase` (11-D7). A conflict is
/// [`crate::tick::TickError::PullConflict`]: the tick aborts with nothing written.
pub fn begin(git: &dyn Git, checkout: &Path) -> crate::tick::Result<()> {
    todo!("tick/ledger")
}

/// 11-D8: commit `forced` and `paths` ([`Git::commit`]), push, and on a non-fast-forward reject
/// pull `--no-rebase` and push again, at most [`PUSH_TRIES`] pushes in all, then
/// [`crate::tick::TickError::PushExhausted`]. Also pushes an earlier unpushed commit when nothing
/// changed.
pub fn commit_and_push(
    git: &dyn Git,
    checkout: &Path,
    forced: &[PathBuf],
    paths: &[PathBuf],
    message: &str,
) -> crate::tick::Result<Pushed> {
    todo!("tick/ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn an_interrupted_push_leaves_the_commit_local_and_its_effects_unemitted() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_rejected_push_pulls_without_rebase_and_pushes_the_merge_from_the_new_tip() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_push_is_tried_at_most_three_times_then_reports_exhausted() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_restart_with_an_unpushed_commit_pushes_it_before_the_pull_and_before_any_effect() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_conflicting_pull_is_aborted_and_the_tick_writes_nothing() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_gitignored_tick_file_is_committed_and_another_writers_dirty_file_is_not() {
        todo!()
    }
}
