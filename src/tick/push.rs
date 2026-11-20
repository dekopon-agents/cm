//! Git for the tick: the ledger's one direct push (11-D8) and the `git show` reads the authority
//! uses (11-D4). `tick/ledger` owns it; `tick/core` only calls it.
//!
//! Task: every call is a child process on the tick's own thread, waited for with [`GIT_TIMEOUT`]
//! and killed on expiry. There is no queue; shutdown is the process exiting, with the commit
//! either pushed or still local, which the next tick pushes before it emits anything.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::forge::CommitSha;

/// 11-D8: at most three tries, then park.
pub const PUSH_TRIES: u8 = 3;
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    Pushed,
    NonFastForward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    pub commit: CommitSha,
    pub tries: u8,
}

/// The git a tick needs, so tests run without a remote.
pub trait Git {
    /// Commits exactly `paths`; `None` when nothing changed.
    fn commit(
        &self,
        repo: &Path,
        paths: &[PathBuf],
        message: &str,
    ) -> crate::tick::Result<Option<CommitSha>>;
    /// `git push origin HEAD:main`; never `--force`, never another ref.
    fn push_main(&self, repo: &Path) -> crate::tick::Result<PushResult>;
    /// `git pull --no-rebase origin main`.
    fn pull_no_rebase(&self, repo: &Path) -> crate::tick::Result<()>;
    /// `git show <sha>:<path>`; `None` when the path is absent at that revision.
    fn show(&self, repo: &Path, sha: &str, path: &str) -> crate::tick::Result<Option<String>>;
    /// Whether `sha` is a commit in the local object store.
    fn has_commit(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool>;
    /// `git fetch origin <sha>`; `false` when the remote refuses it.
    fn fetch_sha(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool>;
    /// `git rev-parse HEAD`.
    fn head(&self, repo: &Path) -> crate::tick::Result<CommitSha>;
}

pub struct SystemGit {
    pub program: PathBuf,
    pub timeout: Duration,
}

impl Default for SystemGit {
    fn default() -> Self {
        SystemGit {
            program: PathBuf::from("git"),
            timeout: GIT_TIMEOUT,
        }
    }
}

impl Git for SystemGit {
    fn commit(
        &self,
        repo: &Path,
        paths: &[PathBuf],
        message: &str,
    ) -> crate::tick::Result<Option<CommitSha>> {
        todo!("tick/ledger")
    }

    fn push_main(&self, repo: &Path) -> crate::tick::Result<PushResult> {
        todo!("tick/ledger")
    }

    fn pull_no_rebase(&self, repo: &Path) -> crate::tick::Result<()> {
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

    fn head(&self, repo: &Path) -> crate::tick::Result<CommitSha> {
        todo!("tick/ledger")
    }
}

/// Commits `paths` and pushes main: on a non-fast-forward, pull `--no-rebase` and push again,
/// at most [`PUSH_TRIES`] times, then [`crate::tick::TickError::PushExhausted`].
pub fn push_ledger(
    git: &dyn Git,
    repo: &Path,
    paths: &[PathBuf],
    message: &str,
) -> crate::tick::Result<Option<Pushed>> {
    todo!("tick/ledger")
}

/// The campaign-trail checkout that holds `root`: the repository the ledger pushes.
pub fn checkout_of(git: &dyn Git, root: &Path) -> crate::tick::Result<PathBuf> {
    todo!("tick/ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn interrupted_push_leaves_the_commit_local_and_its_effects_unemitted() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn rejected_push_pulls_without_rebase_and_retries_from_the_new_tip() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn push_tries_at_most_three_times_then_reports_exhausted() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn restart_with_an_unpushed_ledger_commit_pushes_it_before_any_effect() {
        todo!()
    }
}
