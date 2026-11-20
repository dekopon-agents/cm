//! Git for the tick (`tick/ledger`): the clone 11-D7 hands in, the pull each tick starts with,
//! the ledger's one direct push (11-D8), and the `git show` reads the authority uses (11-D4).
//!
//! S-tick-19, task. **Owner:** the tick's thread; every call is one child process run through
//! `forge::run` with [`GIT_TIMEOUT`] and killed on expiry. **Bound:** one child at a time.
//! **Queue:** none. **Shutdown:** a commit that did not push stays local, and the next tick's
//! [`begin`] pushes it before anything else, so no effect is emitted whose intent row is not on
//! main.
//!
//! **What a commit holds (R02).** The tick's own `.cm/tick/` files, added with `git add --force`
//! because campaign-trail's top-level `.gitignore` ignores `*.jsonl`; every dirty cm-owned path
//! under the campaign folder ([`cm_owned`]: the state, render and queue files cm writes); and the
//! other paths the tick named in `Cycle::wrote` (an Astra out-dir), added without `--force`, so an
//! ignored transcript stays out. A path no cm command writes is never committed by the tick.
//!
//! **Begin (R02, R03).** Before any read of campaign state:
//! 1. The ledger segment's `merge` attribute must be `union` (`ledger` module doc); otherwise
//!    [`crate::tick::TickError::LedgerNotUnion`].
//! 2. Recovery: dirty cm-owned paths, a killed tick's or a `cm` command's, are committed.
//! 3. Any unpushed commit is pushed with [`commit_and_push`]'s tries.
//! 4. `git pull --no-rebase origin main`. A dirty path that is not cm-owned and blocks the pull is
//!    [`crate::tick::TickError::PullBlocked`]. A conflict is aborted; then a checkout cm cloned
//!    itself is rebuilt ([`rebuild`]), and any other checkout (the coordinator's own, in the
//!    interim) is [`crate::tick::TickError::PullConflict`] for a human to resolve.
//!
//! Never `--force` on a push, never another ref.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::forge::CommitSha;

/// 11-D8: at most three tries, then [`crate::tick::TickError::PushExhausted`].
pub const PUSH_TRIES: u8 = 3;
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Written in a checkout cm cloned itself (`--clone`); its presence allows [`rebuild`].
pub const OWNED_MARK: &str = ".git/cm-tick-owned";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullResult {
    /// Up to date, or a clean merge commit.
    Merged,
    /// Dirty paths that would be overwritten; nothing was merged.
    Blocked { paths: Vec<String> },
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
    /// `git clone <url> <dir>` and the [`OWNED_MARK`].
    fn clone_into(&self, url: &str, dir: &Path) -> crate::tick::Result<()>;
    /// `git rev-parse --show-toplevel` of the campaign folder: the checkout.
    fn toplevel(&self, dir: &Path) -> crate::tick::Result<PathBuf>;
    /// `git check-attr merge -- <path>`'s value.
    fn merge_attr(&self, repo: &Path, path: &Path) -> crate::tick::Result<Option<String>>;
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
    /// `git fetch origin main` and `git reset --hard origin/main`; only [`rebuild`] calls it.
    fn reset_to_origin(&self, repo: &Path) -> crate::tick::Result<()>;
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
    fn clone_into(&self, url: &str, dir: &Path) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }

    fn toplevel(&self, dir: &Path) -> crate::tick::Result<PathBuf> {
        todo!("tick/ledger")
    }

    fn merge_attr(&self, repo: &Path, path: &Path) -> crate::tick::Result<Option<String>> {
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

    fn reset_to_origin(&self, repo: &Path) -> crate::tick::Result<()> {
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

/// Whether a path relative to the campaign folder is one cm writes: `.cm/**` (not its locks),
/// `STATE.md` at any depth, `JOURNAL.md`, `CARD.md`, `RESUME.md`, `MAC-LANE.md`, `OWNER-QUEUE.md`,
/// `REVIEW-OFF.md`, `FEEDBACK.md`, `AGENTS.jsonl`, and a step's `evidence/reviews/` copies.
pub fn cm_owned(rel: &Path) -> bool {
    todo!("tick/ledger")
}

/// 11-D7's hand-in, for `--clone`: clones `url` into the checkout that holds `campaign` when that
/// checkout is absent. `true` when it cloned.
pub fn ensure_checkout(git: &dyn Git, url: &str, campaign: &Path) -> crate::tick::Result<bool> {
    todo!("tick/ledger")
}

/// The tick's first git act, in the order the module doc gives.
pub fn begin(git: &dyn Git, checkout: &Path, campaign: &Path) -> crate::tick::Result<()> {
    todo!("tick/ledger")
}

/// R03's wedge recovery, only in a checkout carrying [`OWNED_MARK`]: keep the ledger rows main
/// lacks, reset to `origin/main`, append them again (union order), commit and push. The state
/// writes dropped with the reset belong to intents whose effects never ran (their rows were never
/// pushed) or that reconcile parks (an attempt id lost after its launch).
pub fn rebuild(git: &dyn Git, checkout: &Path, campaign: &Path) -> crate::tick::Result<()> {
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
    fn a_gitignored_tick_file_is_committed_and_a_path_no_cm_command_writes_is_not() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_killed_ticks_cm_writes_are_committed_before_the_next_pull() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_wedged_owned_checkout_rebuilds_from_origin_keeping_every_ledger_row() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn an_absent_checkout_is_cloned_and_a_reaped_one_recloned() {
        todo!()
    }
}
