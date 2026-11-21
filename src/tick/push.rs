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
//! because campaign-trail's top-level `.gitignore` ignores `*.jsonl` (a new segment is invisible to
//! [`Git::dirty`] until then); every dirty cm-owned path under the campaign folder ([`cm_owned`]:
//! the state, render and queue files cm writes); and the other paths the tick named in
//! `Cycle::wrote` (the drop tally's `RELEASE.md`, an Astra out-dir), added without `--force`, so
//! an ignored transcript stays out. A path no cm command writes is never committed by the tick.
//!
//! **The pending mark (F1).** A tick's decisions are one unit with its first commit: pushed whole,
//! or discarded whole. Right after [`begin`] the tick saves [`PendingMark`] in [`PENDING_MARK`]
//! (inside `.git`, never committed): its tick number, the commit `begin` left (`base`) and phase
//! `decide`. `Cycle::wrote` adds each path outside the cm-owned set to the mark before the stage
//! writes it. After the first push the phase becomes `record`; after the second push the mark is
//! cleared. An error before the first push discards at once ([`discard`]); a kill leaves the mark
//! for the next `begin`. Nothing external happens before the first push, so a discarded tick's
//! intents, `emitted` rows, reconcile rows, sync and launch writes, cursor, timers, seen set and
//! tally reservation are all re-derived by the next tick.
//!
//! **Begin (R02, R03, F1).** Before any read of campaign state:
//! 0. A merge a killed git child left in progress is aborted ([`Git::abort_merge`]).
//! 1. The ledger segment's `merge` attribute must be `union` (`ledger` module doc); otherwise
//!    [`crate::tick::TickError::LedgerNotUnion`].
//! 2. Recovery, by the mark, which is then cleared:
//!    - `decide`, with its first commit not on main (HEAD is `base`, or holds commits
//!      `origin/main` lacks): [`discard`].
//!    - `record`, or `decide` whose first commit is already on main: the killed tick's facts
//!      (binds, receipts, attempt ids, Astra out-dirs, agent rows) are committed: the dirty
//!      cm-owned paths, the mark's paths, and `.cm/tick/` with `--force`.
//!    - No mark: the dirty cm-owned paths (a `cm` command's) and `.cm/tick/` with `--force` (a
//!      `cm effects record` or `resolve` killed before its push).
//! 3. Any unpushed commit is pushed with [`commit_and_push`]'s tries.
//! 4. `git pull --no-rebase origin main`.
//!
//! At steps 3 and 4 a conflict is aborted and a blocking dirty path merges nothing. Then a checkout
//! cm cloned itself is rebuilt ([`rebuild`]): nothing but cm writes there. Any other checkout (the
//! coordinator's own, in the interim) gets [`crate::tick::TickError::PullConflict`] or
//! [`crate::tick::TickError::PullBlocked`] for a human to resolve.
//!
//! **Conflicts at every push (F2).** [`commit_and_push`] pulls inside its retry loop. A conflict
//! there is aborted, a block merges nothing, and either way the local commit is kept, unpushed, and
//! the error is returned. What each call site does with it:
//! - `begin` step 3: rebuild in an owned checkout, else the error (above).
//! - Stage 9, the tick's first commit (`run.rs`): no effect has happened. The tick discards its
//!   decisions with the mark and stops, and the next tick re-derives them on the new main.
//! - Stage 11, the second commit: the facts stay in the local commit and the mark stays `record`.
//!   The tick reports the error, and the next `begin` pushes them at step 3 or rebuilds.
//! - `cm effects record` and `cm effects resolve`: the command exits non-zero with its rows in the
//!   local commit. The runner sends the batch again (recorded idempotently by intent), and the next
//!   `begin` pushes or rebuilds.
//!
//! So a rebuild happens only inside `begin`, between ticks, never mid-tick. A conflict on
//! [`rebuild`]'s own push is `PullConflict`, for a human.
//!
//! Never `--force` on a push, never another ref.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::forge::CommitSha;

/// 11-D8: at most three tries, then [`crate::tick::TickError::PushExhausted`].
pub const PUSH_TRIES: u8 = 3;
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Written in a checkout cm cloned itself (`--clone`); its presence allows [`rebuild`].
pub const OWNED_MARK: &str = ".git/cm-tick-owned";
/// The running tick's [`PendingMark`], relative to the checkout (F1).
pub const PENDING_MARK: &str = ".git/cm-tick-pending";

/// Where a tick is relative to its first push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TickPhase {
    /// Before the first push: everything written since `base` is a decision, discarded on death.
    Decide,
    /// After it: what is written now records effects that happened, and is kept.
    Record,
}

/// F1: what a killed or failed tick leaves for the next `begin` to discard or commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMark {
    pub tick: u64,
    /// HEAD when `begin` returned: every later commit and write is this tick's.
    pub base: String,
    pub phase: TickPhase,
    /// Paths outside the cm-owned set the tick writes, relative to the checkout, each added and
    /// saved before its write.
    pub paths: Vec<PathBuf>,
}

impl PendingMark {
    /// `None` when no tick is pending.
    pub fn load(checkout: &Path) -> crate::tick::Result<Option<Self>> {
        todo!("tick/ledger")
    }

    /// Written whole (a temporary file renamed over it), then fsynced.
    pub fn save(&self, checkout: &Path) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }

    pub fn clear(checkout: &Path) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }
}

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
    /// `git merge --abort` when `MERGE_HEAD` exists; `true` when it aborted one.
    fn abort_merge(&self, repo: &Path) -> crate::tick::Result<bool>;
    /// `git rev-parse HEAD`.
    fn head(&self, repo: &Path) -> crate::tick::Result<String>;
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
    /// `git reset --mixed <base>` (the branch moves back, the tree is kept); then each of `paths`
    /// is checked out from `base`, or deleted when absent there; then `git clean -fdx -- <clean>`.
    /// Only [`discard`] calls it.
    fn restore(
        &self,
        repo: &Path,
        base: &str,
        paths: &[PathBuf],
        clean: &Path,
    ) -> crate::tick::Result<()>;
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

    fn abort_merge(&self, repo: &Path) -> crate::tick::Result<bool> {
        todo!("tick/ledger")
    }

    fn head(&self, repo: &Path) -> crate::tick::Result<String> {
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

    fn restore(
        &self,
        repo: &Path,
        base: &str,
        paths: &[PathBuf],
        clean: &Path,
    ) -> crate::tick::Result<()> {
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

/// F1: a tick that died or failed before its first push. Holding the campaign lock
/// (`<epoch>/.cm/lock`, R16) throughout, it moves the branch back to the mark's `base`, restores to
/// `base` every dirty cm-owned path under `campaign` except `AGENTS.jsonl` and every path the mark
/// lists, removes every untracked or ignored file under `.cm/tick/` (a new segment), and clears the
/// mark. `AGENTS.jsonl` is never restored: its rows and already-recorded echoes stay exactly as the
/// last lock-holding writer left them, so an independent writer's row (a harvester's) is never
/// deleted, and an echo of a run that happened (a collected Astra receipt) is kept. Nothing else
/// of that tick remains.
pub fn discard(
    git: &dyn Git,
    checkout: &Path,
    campaign: &Path,
    mark: &PendingMark,
) -> crate::tick::Result<()> {
    todo!("tick/ledger")
}

/// R03's wedge recovery, only in a checkout carrying [`OWNED_MARK`] and only from [`begin`] (F2).
/// Before the reset it keeps, as lines, what main lacks in the ledger segments and in
/// `AGENTS.jsonl` (F3): a line-set difference against `origin/main`'s copy, with no parsing. Then
/// it resets to `origin/main`, appends those lines again (union order), commits and pushes. Two
/// lines for one agent invocation (main's and the filled one kept here) fold in `agents::fold`.
/// The state writes dropped with the reset are a recovery commit's: an attempt id lost after its
/// launch, which reconcile parks. The tick number is not rewound: `run` takes the larger of
/// `Cursor::tick` and the highest row `cycle`.
pub fn rebuild(git: &dyn Git, checkout: &Path, campaign: &Path) -> crate::tick::Result<()> {
    todo!("tick/ledger")
}

/// 11-D8: commit `forced` and `paths` ([`Git::commit`]), push, and on a non-fast-forward reject
/// pull `--no-rebase` and push again, at most [`PUSH_TRIES`] pushes in all, then
/// [`crate::tick::TickError::PushExhausted`]. Also pushes an earlier unpushed commit when nothing
/// changed. A pull that conflicts (aborted) or is blocked keeps the local commit unpushed and
/// returns [`crate::tick::TickError::PullConflict`] or [`crate::tick::TickError::PullBlocked`];
/// each call site's handling is in the module doc (F2). It never rebuilds.
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
    fn a_cm_commands_writes_and_a_killed_ticks_recorded_facts_are_committed_before_the_next_pull() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_wedged_owned_checkout_rebuilds_from_origin_keeping_every_ledger_and_agent_row() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn an_absent_checkout_is_cloned_and_a_reaped_one_recloned() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_tick_killed_before_its_first_push_is_discarded_whole_and_its_tally_row_and_new_segment_go_with_it()
     {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_conflict_inside_commit_and_push_keeps_the_commit_local_and_only_begin_rebuilds() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_merge_left_by_a_killed_git_child_is_aborted_before_anything_else() {
        todo!()
    }
}
