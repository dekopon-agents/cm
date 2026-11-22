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

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::forge::CommitSha;
use crate::tick::TickError;

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
        let path = checkout.join(PENDING_MARK);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(TickError::Io { path, source }),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| TickError::Row {
                path,
                line: 1,
                source,
            })
    }

    /// Written whole (a temporary file renamed over it), then fsynced.
    pub fn save(&self, checkout: &Path) -> crate::tick::Result<()> {
        let path = checkout.join(PENDING_MARK);
        let io = |source| TickError::Io {
            path: path.clone(),
            source,
        };
        let dir = path.parent().unwrap_or(checkout);
        let mut bytes = serde_json::to_vec(self).map_err(|source| TickError::Row {
            path: path.clone(),
            line: 1,
            source,
        })?;
        bytes.push(b'\n');
        let mut temporary = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
        temporary.write_all(&bytes).map_err(io)?;
        temporary.as_file().sync_all().map_err(io)?;
        temporary.persist(&path).map_err(|error| io(error.error))?;
        fs::File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(io)
    }

    pub fn clear(checkout: &Path) -> crate::tick::Result<()> {
        let path = checkout.join(PENDING_MARK);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(TickError::Io { path, source }),
        }
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

impl SystemGit {
    /// One `git -C <repo>` child; its exit status is the caller's to read.
    fn raw(&self, repo: &Path, op: &'static str, args: &[&str]) -> crate::tick::Result<Output> {
        let repo = repo.to_string_lossy();
        let mut full = vec!["-C", repo.as_ref()];
        full.extend_from_slice(args);
        self.spawn(op, &full)
    }

    fn spawn(&self, op: &'static str, args: &[&str]) -> crate::tick::Result<Output> {
        crate::forge::run(&self.program, args, self.timeout, None).map_err(|error| {
            TickError::Child {
                program: "git",
                op,
                error,
            }
        })
    }

    /// One child that must exit zero; its stdout.
    fn ok(&self, repo: &Path, op: &'static str, args: &[&str]) -> crate::tick::Result<String> {
        let output = self.raw(repo, op, args)?;
        if !output.status.success() {
            return Err(exit(op, &output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// One child whose exit status is the answer.
    fn test(&self, repo: &Path, op: &'static str, args: &[&str]) -> crate::tick::Result<bool> {
        Ok(self.raw(repo, op, args)?.status.success())
    }

    /// `-z` output split into paths.
    fn paths(
        &self,
        repo: &Path,
        op: &'static str,
        args: &[&str],
    ) -> crate::tick::Result<Vec<String>> {
        let out = self.ok(repo, op, args)?;
        Ok(out
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn unmerged(&self, repo: &Path) -> crate::tick::Result<Vec<String>> {
        self.paths(
            repo,
            "diff",
            &["diff", "--name-only", "-z", "--diff-filter=U"],
        )
    }
}

fn exit(op: &'static str, output: &Output) -> TickError {
    TickError::Child {
        program: "git",
        op,
        error: crate::forge::ForgeError::Exit(output.status.code()),
    }
}

fn args_with<'a>(head: &[&'a str], paths: &'a [String]) -> Vec<&'a str> {
    let mut args = head.to_vec();
    args.push("--");
    args.extend(paths.iter().map(String::as_str));
    args
}

fn strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

impl Git for SystemGit {
    fn clone_into(&self, url: &str, dir: &Path) -> crate::tick::Result<()> {
        let target = dir.to_string_lossy();
        let output = self.spawn("clone", &["clone", "-q", "--", url, target.as_ref()])?;
        if !output.status.success() {
            return Err(exit("clone", &output));
        }
        let mark = dir.join(OWNED_MARK);
        fs::write(&mark, b"cloned by cm tick --clone\n")
            .map_err(|source| TickError::Io { path: mark, source })
    }

    fn toplevel(&self, dir: &Path) -> crate::tick::Result<PathBuf> {
        let out = self.ok(dir, "rev-parse", &["rev-parse", "--show-toplevel"])?;
        Ok(PathBuf::from(out.trim_end_matches('\n')))
    }

    fn merge_attr(&self, repo: &Path, path: &Path) -> crate::tick::Result<Option<String>> {
        let path = path.to_string_lossy();
        let out = self.ok(
            repo,
            "check-attr",
            &["check-attr", "-z", "merge", "--", path.as_ref()],
        )?;
        // `-z`: path NUL attribute NUL value NUL.
        let value = out.split('\0').nth(2).unwrap_or_default();
        Ok(match value {
            "" | "unspecified" => None,
            value => Some(value.to_owned()),
        })
    }

    fn pull_no_rebase(&self, repo: &Path) -> crate::tick::Result<PullResult> {
        self.ok(repo, "fetch", &["fetch", "-q", "origin", "main"])?;
        let pulled = self.raw(
            repo,
            "pull",
            &["pull", "-q", "--no-rebase", "--no-edit", "origin", "main"],
        )?;
        if pulled.status.success() {
            return Ok(PullResult::Merged);
        }
        if self.test(
            repo,
            "rev-parse",
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )? {
            let paths = self.unmerged(repo)?;
            self.ok(repo, "merge", &["merge", "--abort"])?;
            return Ok(PullResult::Conflict { paths });
        }
        // Nothing merged: the dirty paths the incoming side touches are what blocked it.
        let incoming: BTreeSet<String> = self
            .paths(
                repo,
                "diff",
                &["diff", "--name-only", "-z", "HEAD...origin/main"],
            )?
            .into_iter()
            .collect();
        let blocking: Vec<String> = self
            .dirty(repo, Path::new("."))?
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .filter(|path| incoming.contains(path))
            .collect();
        if blocking.is_empty() {
            return Err(exit("pull", &pulled));
        }
        Ok(PullResult::Blocked { paths: blocking })
    }

    fn abort_merge(&self, repo: &Path) -> crate::tick::Result<bool> {
        if !self.test(
            repo,
            "rev-parse",
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )? {
            return Ok(false);
        }
        self.ok(repo, "merge", &["merge", "--abort"])?;
        Ok(true)
    }

    fn head(&self, repo: &Path) -> crate::tick::Result<String> {
        Ok(self
            .ok(repo, "rev-parse", &["rev-parse", "HEAD"])?
            .trim()
            .to_owned())
    }

    fn dirty(&self, repo: &Path, under: &Path) -> crate::tick::Result<BTreeSet<PathBuf>> {
        let under = under.to_string_lossy();
        let out = self.ok(
            repo,
            "status",
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--no-renames",
                "--",
                under.as_ref(),
            ],
        )?;
        Ok(out
            .split('\0')
            .filter(|entry| entry.len() > 3)
            .map(|entry| PathBuf::from(&entry[3..]))
            .collect())
    }

    fn commit(
        &self,
        repo: &Path,
        forced: &[PathBuf],
        paths: &[PathBuf],
        message: &str,
    ) -> crate::tick::Result<Option<CommitSha>> {
        let present = |path: &Path| repo.join(path).exists();
        let forced: Vec<String> = strings(
            &forced
                .iter()
                .filter(|path| present(path))
                .cloned()
                .collect::<Vec<_>>(),
        );
        if !forced.is_empty() {
            self.ok(repo, "add", &args_with(&["add", "--force"], &forced))?;
        }
        let paths = strings(paths);
        let mut plain = Vec::new();
        if !paths.is_empty() {
            // A path is added when it exists and is not ignored, or when git tracks it (a
            // deletion). An ignored path stays out: it is never `--force`d here.
            let ignored: BTreeSet<String> = {
                let output = self.raw(
                    repo,
                    "check-ignore",
                    &args_with(&["check-ignore", "-z"], &paths),
                )?;
                String::from_utf8_lossy(&output.stdout)
                    .split('\0')
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect()
            };
            let tracked: BTreeSet<String> = self
                .paths(repo, "ls-files", &args_with(&["ls-files", "-z"], &paths))?
                .into_iter()
                .collect();
            plain = paths
                .into_iter()
                .filter(|path| {
                    let under = |file: &String| {
                        file == path || Path::new(file).starts_with(Path::new(path))
                    };
                    (present(Path::new(path)) && !ignored.contains(path))
                        || tracked.iter().any(under)
                })
                .collect();
            if !plain.is_empty() {
                self.ok(repo, "add", &args_with(&["add", "--all"], &plain))?;
            }
        }
        let mut all = forced;
        all.extend(plain);
        if all.is_empty() {
            return Ok(None);
        }
        let staged = self.paths(
            repo,
            "diff",
            &args_with(
                &["diff", "--cached", "--name-only", "-z", "--no-renames"],
                &all,
            ),
        )?;
        if staged.is_empty() {
            return Ok(None);
        }
        self.ok(
            repo,
            "commit",
            &args_with(&["commit", "-q", "--no-verify", "-m", message], &staged),
        )?;
        Ok(Some(CommitSha::new(self.head(repo)?)))
    }

    fn push_main(&self, repo: &Path) -> crate::tick::Result<PushResult> {
        let pushed = self.raw(repo, "push", &["push", "-q", "origin", "HEAD:main"])?;
        if pushed.status.success() {
            // Keep `origin/main` at what was pushed, so `unpushed` reads it.
            self.ok(repo, "fetch", &["fetch", "-q", "origin", "main"])?;
            return Ok(PushResult::Pushed);
        }
        self.ok(repo, "fetch", &["fetch", "-q", "origin", "main"])?;
        if self.test(
            repo,
            "merge-base",
            &["merge-base", "--is-ancestor", "origin/main", "HEAD"],
        )? {
            return Err(exit("push", &pushed));
        }
        Ok(PushResult::NonFastForward)
    }

    fn unpushed(&self, repo: &Path) -> crate::tick::Result<bool> {
        let out = self.ok(
            repo,
            "rev-list",
            &["rev-list", "--count", "origin/main..HEAD"],
        )?;
        Ok(out.trim() != "0")
    }

    fn reset_to_origin(&self, repo: &Path) -> crate::tick::Result<()> {
        self.ok(repo, "fetch", &["fetch", "-q", "origin", "main"])?;
        self.ok(repo, "reset", &["reset", "-q", "--hard", "origin/main"])?;
        Ok(())
    }

    fn restore(
        &self,
        repo: &Path,
        base: &str,
        paths: &[PathBuf],
        clean: &Path,
    ) -> crate::tick::Result<()> {
        self.ok(repo, "reset", &["reset", "-q", "--mixed", base])?;
        for path in paths {
            let text = path.to_string_lossy();
            let spec = format!("{base}:{text}");
            if self.test(repo, "cat-file", &["cat-file", "-e", &spec])? {
                self.ok(repo, "checkout", &["checkout", base, "--", text.as_ref()])?;
            } else {
                let full = repo.join(path);
                let removed = if full.is_dir() {
                    fs::remove_dir_all(&full)
                } else {
                    fs::remove_file(&full)
                };
                match removed {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => return Err(TickError::Io { path: full, source }),
                }
            }
        }
        let clean = clean.to_string_lossy();
        // `-e lock` keeps the held tick lock: removing it would let a second holder in.
        self.ok(
            repo,
            "clean",
            &[
                "clean",
                "-q",
                "-f",
                "-d",
                "-x",
                "-e",
                "lock",
                "--",
                clean.as_ref(),
            ],
        )?;
        Ok(())
    }

    fn show(&self, repo: &Path, sha: &str, path: &str) -> crate::tick::Result<Option<String>> {
        let spec = format!("{sha}:{path}");
        if !self.test(repo, "cat-file", &["cat-file", "-e", &spec])? {
            if !self.has_commit(repo, sha)? {
                return Err(TickError::Child {
                    program: "git",
                    op: "show",
                    error: crate::forge::ForgeError::Exit(Some(128)),
                });
            }
            return Ok(None);
        }
        Ok(Some(self.ok(repo, "show", &["show", &spec])?))
    }

    fn has_commit(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool> {
        let spec = format!("{sha}^{{commit}}");
        self.test(repo, "cat-file", &["cat-file", "-e", &spec])
    }

    fn fetch_sha(&self, repo: &Path, sha: &str) -> crate::tick::Result<bool> {
        self.test(repo, "fetch", &["fetch", "-q", "origin", sha])
    }
}

/// Whether a path relative to the campaign folder is one cm writes: `.cm/**` (not its locks),
/// `STATE.md` at any depth, `JOURNAL.md`, `CARD.md`, `RESUME.md`, `MAC-LANE.md`, `OWNER-QUEUE.md`,
/// `REVIEW-OFF.md`, `FEEDBACK.md`, `AGENTS.jsonl`, and a step's `evidence/reviews/` copies.
pub fn cm_owned(rel: &Path) -> bool {
    let parts: Vec<&std::ffi::OsStr> = rel
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let Some(name) = parts.last().and_then(|name| name.to_str()) else {
        return false;
    };
    if parts
        .first()
        .is_some_and(|first| *first == crate::store::CM_DIR)
    {
        return name != crate::store::LOCK;
    }
    if name == "STATE.md" {
        return true;
    }
    if parts.len() == 1 && TOP_LEVEL_OWNED.contains(&name) {
        return true;
    }
    parts
        .windows(2)
        .any(|pair| pair[0] == "evidence" && pair[1] == "reviews")
        && parts.len() > 3
}

/// 11-D7's hand-in, for `--clone`: clones `url` into the checkout that holds `campaign` when that
/// checkout is absent. `true` when it cloned.
pub fn ensure_checkout(git: &dyn Git, url: &str, campaign: &Path) -> crate::tick::Result<bool> {
    if campaign.is_dir() && git.toplevel(campaign).is_ok() {
        return Ok(false);
    }
    // The frozen hand-in (`wire` module doc): `-C <checkout>/store/epochs/<slug>`.
    let parts: Vec<_> = campaign.components().collect();
    let shaped = parts.len() > 3
        && parts[parts.len() - 3].as_os_str() == "store"
        && parts[parts.len() - 2].as_os_str() == "epochs";
    let checkout = campaign
        .ancestors()
        .nth(3)
        .filter(|_| shaped)
        .ok_or_else(|| TickError::Io {
            path: campaign.to_owned(),
            source: io::Error::other(
                "--clone needs a campaign folder at <checkout>/store/epochs/<slug>",
            ),
        })?;
    // A reaped jail can leave the directory without its repository: clone into it afresh.
    if checkout.exists() && !checkout.join(".git").exists() {
        let empty = fs::read_dir(checkout)
            .map_err(|source| TickError::Io {
                path: checkout.to_owned(),
                source,
            })?
            .next()
            .is_none();
        if !empty {
            return Err(TickError::Io {
                path: checkout.to_owned(),
                source: io::Error::other(
                    "the checkout exists, is not a repository and is not empty",
                ),
            });
        }
    }
    git.clone_into(url, checkout)?;
    Ok(true)
}

/// The tick's first git act, in the order the module doc gives.
pub fn begin(git: &dyn Git, checkout: &Path, campaign: &Path) -> crate::tick::Result<()> {
    // 0. A merge a killed git child left.
    git.abort_merge(checkout)?;
    // 1. The union precondition (R03).
    let segment = inside(checkout, &campaign.join(crate::tick::ledger::LEDGER_DIR))
        .join(format!("{}0001.jsonl", crate::tick::ledger::SEGMENT_PREFIX));
    if git.merge_attr(checkout, &segment)?.as_deref() != Some("union") {
        return Err(TickError::LedgerNotUnion { path: segment });
    }
    // 2. Recovery by the pending mark.
    let mark = PendingMark::load(checkout)?;
    match &mark {
        Some(mark)
            if mark.phase == TickPhase::Decide
                && (git.head(checkout)? == mark.base || git.unpushed(checkout)?) =>
        {
            discard(git, checkout, campaign, mark)?;
        }
        _ => {
            let marked = mark
                .as_ref()
                .map(|mark| mark.paths.clone())
                .unwrap_or_default();
            let mut paths = owned_dirty(git, checkout, campaign, false)?;
            paths.extend(marked);
            let message = match &mark {
                Some(mark) => format!("cm tick: recorded facts of tick {}", mark.tick),
                None => "cm tick: commit cm writes before the pull".to_owned(),
            };
            git.commit(checkout, &tick_files(checkout, campaign)?, &paths, &message)?;
            PendingMark::clear(checkout)?;
        }
    }
    // Steps 3 and 4: a conflict or block rebuilds a checkout cm cloned; any other is a human's.
    let owned = checkout.join(OWNED_MARK).exists();
    let wedged = |error: TickError| match error {
        TickError::PullConflict { .. } | TickError::PullBlocked { .. } if owned => {
            rebuild(git, checkout, campaign)
        }
        error => Err(error),
    };
    // 3. An unpushed commit goes out before the pull.
    if git.unpushed(checkout)?
        && let Err(error) = commit_and_push(git, checkout, &[], &[], "cm tick: push")
    {
        return wedged(error);
    }
    // 4. The pull.
    match git.pull_no_rebase(checkout)? {
        PullResult::Merged => Ok(()),
        PullResult::Conflict { paths } => wedged(TickError::PullConflict { paths }),
        PullResult::Blocked { paths } => wedged(TickError::PullBlocked { paths }),
    }
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
    let _lock =
        crate::store::lock(campaign).map_err(|error| TickError::Cm { op: "lock", error })?;
    let clean = inside(checkout, &campaign.join(crate::tick::ledger::LEDGER_DIR));
    // Back to `base` first, so the paths the unpushed first commit held read as dirty too.
    git.restore(checkout, &mark.base, &[], &clean)?;
    let mut paths = owned_dirty(git, checkout, campaign, true)?;
    paths.extend(mark.paths.iter().cloned());
    paths.sort();
    paths.dedup();
    git.restore(checkout, &mark.base, &paths, &clean)?;
    PendingMark::clear(checkout)
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
    if !checkout.join(OWNED_MARK).exists() {
        return Err(TickError::Io {
            path: checkout.join(OWNED_MARK),
            source: io::Error::other("only a checkout cm cloned is rebuilt"),
        });
    }
    git.abort_merge(checkout)?;
    let ledger = campaign.join(crate::tick::ledger::LEDGER_DIR);
    let agents = campaign.join(crate::tick::agents::AGENTS_FILE);
    let kept_ledger = ledger_lines(&ledger)?;
    let kept_agents = whole_lines(&read_or_empty(&agents)?);
    git.reset_to_origin(checkout)?;
    // What main lacks, as lines, in local order (F3).
    let main_ledger: BTreeSet<String> = ledger_lines(&ledger)?.into_iter().collect();
    let lost_ledger: Vec<String> = kept_ledger
        .into_iter()
        .filter(|line| !main_ledger.contains(line))
        .collect();
    let main_agents: BTreeSet<String> = whole_lines(&read_or_empty(&agents)?).into_iter().collect();
    let lost_agents: Vec<String> = kept_agents
        .into_iter()
        .filter(|line| !main_agents.contains(line))
        .collect();
    let mut forced = Vec::new();
    let mut paths = Vec::new();
    if !lost_ledger.is_empty() {
        let handle = crate::tick::ledger::EffectLedger::snapshot(campaign)?;
        let segment = campaign.join(handle.active_segment());
        append_lines(&segment, &lost_ledger)?;
        forced.push(inside(checkout, &segment));
    }
    if !lost_agents.is_empty() {
        append_lines(&agents, &lost_agents)?;
        paths.push(inside(checkout, &agents));
    }
    commit_and_push(
        git,
        checkout,
        &forced,
        &paths,
        "cm tick: rebuild from origin/main keeping ledger and agent rows",
    )?;
    Ok(())
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
    let commit = git.commit(checkout, forced, paths, message)?;
    if commit.is_none() && !git.unpushed(checkout)? {
        return Ok(Pushed {
            commit: None,
            tries: 0,
        });
    }
    let mut tries = 0;
    loop {
        tries += 1;
        match git.push_main(checkout)? {
            PushResult::Pushed => {
                return Ok(Pushed {
                    commit: Some(CommitSha::new(git.head(checkout)?)),
                    tries,
                });
            }
            PushResult::NonFastForward if tries >= PUSH_TRIES => {
                return Err(TickError::PushExhausted { tries });
            }
            PushResult::NonFastForward => match git.pull_no_rebase(checkout)? {
                PullResult::Merged => {}
                PullResult::Conflict { paths } => return Err(TickError::PullConflict { paths }),
                PullResult::Blocked { paths } => return Err(TickError::PullBlocked { paths }),
            },
        }
    }
}

/// `cm_owned`'s names that live at the campaign folder's top.
const TOP_LEVEL_OWNED: [&str; 8] = [
    "JOURNAL.md",
    "CARD.md",
    "RESUME.md",
    "MAC-LANE.md",
    "OWNER-QUEUE.md",
    "REVIEW-OFF.md",
    "FEEDBACK.md",
    "AGENTS.jsonl",
];

/// `path` relative to the checkout, when it lies inside it.
fn inside(checkout: &Path, path: &Path) -> PathBuf {
    if let Ok(rel) = path.strip_prefix(checkout) {
        return rel.to_owned();
    }
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    // The path may not exist yet: canonicalize its nearest existing ancestor.
    let mut tail = Vec::new();
    let mut base = path;
    while !base.exists() {
        let Some(parent) = base.parent() else { break };
        if let Some(name) = base.file_name() {
            tail.push(name.to_owned());
        }
        base = parent;
    }
    let mut full = canonical(base);
    full.extend(tail.iter().rev());
    full.strip_prefix(canonical(checkout))
        .map(Path::to_owned)
        .unwrap_or_else(|_| path.to_owned())
}

/// Dirty cm-owned paths under the campaign folder, relative to the checkout; `AGENTS.jsonl`
/// left out when `keep_agents`.
fn owned_dirty(
    git: &dyn Git,
    checkout: &Path,
    campaign: &Path,
    keep_agents: bool,
) -> crate::tick::Result<Vec<PathBuf>> {
    let under = inside(checkout, campaign);
    let agents = under.join(crate::tick::agents::AGENTS_FILE);
    Ok(git
        .dirty(checkout, &under)?
        .into_iter()
        .filter(|path| path.strip_prefix(&under).is_ok_and(cm_owned))
        .filter(|path| !(keep_agents && *path == agents))
        .collect())
}

/// Every file under the campaign's `.cm/tick/` but the lock, relative to the checkout.
fn tick_files(checkout: &Path, campaign: &Path) -> crate::tick::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(&path, out)?;
            } else if entry.file_name() != crate::store::LOCK {
                out.push(path);
            }
        }
        Ok(())
    }
    let dir = campaign.join(crate::tick::ledger::LEDGER_DIR);
    let mut found = Vec::new();
    walk(&dir, &mut found).map_err(|source| TickError::Io { path: dir, source })?;
    found.sort();
    Ok(found.iter().map(|path| inside(checkout, path)).collect())
}

fn read_or_empty(path: &Path) -> crate::tick::Result<String> {
    match fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(TickError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// The newline-terminated lines of `text`; a torn tail is not a line.
fn whole_lines(text: &str) -> Vec<String> {
    let whole = text.rfind('\n').map_or("", |at| &text[..=at]);
    whole
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// Every segment's whole lines, in segment order.
fn ledger_lines(dir: &Path) -> crate::tick::Result<Vec<String>> {
    let mut names: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(crate::tick::ledger::SEGMENT_PREFIX)
                            && name.ends_with(".jsonl")
                    })
            })
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(TickError::Io {
                path: dir.to_owned(),
                source,
            });
        }
    };
    names.sort();
    let mut lines = Vec::new();
    for path in names {
        lines.extend(whole_lines(&read_or_empty(&path)?));
    }
    Ok(lines)
}

/// Appends `lines` after any whole lines already there, one `write`, then fsync.
fn append_lines(path: &Path, lines: &[String]) -> crate::tick::Result<()> {
    let io = |source| TickError::Io {
        path: path.to_owned(),
        source,
    };
    let existing = read_or_empty(path)?;
    let mut text = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        text.push('\n');
    }
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io)?;
    file.write_all(text.as_bytes()).map_err(io)?;
    file.sync_all().map_err(io)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::tick::ledger::tests::{clock, effect, new_id, scratch, tick_lock};
    use crate::tick::ledger::{EffectLedger, IntentState};
    use crate::tick::wire::Capability;
    use scratch::{EPOCH, Scratch, git};

    fn rel(path: &str) -> PathBuf {
        Path::new(EPOCH).join(path)
    }

    /// One intent row in the clone's ledger; the segment path relative to the checkout.
    fn intend(checkout: &Path, target: &str) -> (crate::tick::IntentId, PathBuf) {
        let campaign = scratch::campaign(checkout);
        let lock = tick_lock(&campaign);
        let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
        let id = new_id(
            ledger
                .intend(&clock(), 1, effect(target, Capability::GhPinUpdate))
                .unwrap(),
        );
        (id, Path::new(EPOCH).join(ledger.active_segment()))
    }

    fn rows_on_main(scratch: &Scratch) -> String {
        scratch
            .on_main(&format!("{EPOCH}/.cm/tick/effects-0001.jsonl"))
            .unwrap_or_default()
    }

    fn merge_head(checkout: &Path) -> bool {
        scratch::git_ok(checkout, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
    }

    fn logged(scratch: &Scratch) -> SystemGit {
        SystemGit {
            program: scratch.logging_git(),
            timeout: GIT_TIMEOUT,
        }
    }

    /// The position of the first logged call starting with `prefix`.
    fn first(log: &[String], prefix: &str) -> usize {
        log.iter()
            .position(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` in {log:#?}"))
    }

    /// Two clones that both changed `STATE.md`; the rival's change is on main.
    fn conflicting(scratch: &Scratch, name: &str) -> PathBuf {
        let checkout = scratch.clone(name);
        let rival = scratch.clone(&format!("{name}-rival"));
        fs::write(
            scratch::campaign(&rival).join("STATE.md"),
            "# State\n\nrival\n",
        )
        .unwrap();
        scratch::commit_all(&rival, "rival state");
        git(&rival, &["push", "-q", "origin", "HEAD:main"]);
        fs::write(
            scratch::campaign(&checkout).join("STATE.md"),
            "# State\n\nmine\n",
        )
        .unwrap();
        checkout
    }

    #[test]
    fn an_interrupted_push_leaves_the_commit_local_and_its_effects_unemitted() {
        let scratch = Scratch::new();
        let checkout = scratch.clone("tick");
        let system = SystemGit::default();
        let (id, segment) = intend(&checkout, "01-a/u/s");
        // The origin goes away mid-tick.
        let away = scratch.dir.path().join("away.git");
        fs::rename(&scratch.origin, &away).unwrap();
        let mut emitted = false;
        let pushed = commit_and_push(
            &system,
            &checkout,
            std::slice::from_ref(&segment),
            &[],
            "tick 1",
        );
        if pushed.is_ok() {
            emitted = true;
        }
        assert!(pushed.is_err());
        assert!(!emitted, "no effect follows an unpushed intent row");
        assert!(
            system.unpushed(&checkout).unwrap(),
            "the commit stays local"
        );
        let ledger = EffectLedger::snapshot(&scratch::campaign(&checkout)).unwrap();
        assert_eq!(ledger.last_emitted(&id), None);
        fs::rename(&away, &scratch.origin).unwrap();
        assert!(!rows_on_main(&scratch).contains(&id.0));
        // The next tick's begin pushes it before anything else.
        begin(&system, &checkout, &scratch::campaign(&checkout)).unwrap();
        assert!(rows_on_main(&scratch).contains(&id.0));
        assert!(!system.unpushed(&checkout).unwrap());
    }

    #[test]
    fn a_rejected_push_pulls_without_rebase_and_pushes_the_merge_from_the_new_tip() {
        let scratch = Scratch::new();
        let mine = scratch.clone("tick");
        let other = scratch.clone("other");
        let system = SystemGit::default();
        let (id, segment) = intend(&mine, "01-a/u/s");
        let (theirs, their_segment) = intend(&other, "01-a/u/t");
        commit_and_push(&system, &other, &[their_segment], &[], "theirs").unwrap();
        let their_tip = scratch.main_sha();
        let pushed = commit_and_push(&system, &mine, &[segment], &[], "mine").unwrap();
        assert_eq!(pushed.tries, 2);
        let local = pushed.commit.unwrap();
        assert_eq!(local.as_str(), scratch.main_sha());
        let parents = git(&mine, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        let parents: Vec<&str> = parents.split(' ').skip(1).collect();
        assert_eq!(parents.len(), 2, "a merge, not a rebase");
        assert!(
            parents.contains(&their_tip.as_str()),
            "merged from the new tip"
        );
        let rows = rows_on_main(&scratch);
        assert!(
            rows.contains(&id.0) && rows.contains(&theirs.0),
            "union keeps both rows"
        );
    }

    #[test]
    fn a_push_is_tried_at_most_three_times_then_reports_exhausted() {
        let scratch = Scratch::new();
        let mine = scratch.clone("tick");
        let rival = scratch.clone("rival");
        let system = logged(&scratch);
        let (_, segment) = intend(&mine, "01-a/u/s");
        // Each push loses a real race to the rival's push.
        scratch.set_rival(Some(&rival));
        let error = commit_and_push(&system, &mine, &[segment], &[], "tick").unwrap_err();
        scratch.set_rival(None);
        assert!(
            matches!(error, TickError::PushExhausted { tries: 3 }),
            "{error}"
        );
        let pushes = scratch
            .log()
            .iter()
            .filter(|line| line.starts_with("push "))
            .count();
        assert_eq!(pushes, 3);
        assert!(system.unpushed(&mine).unwrap(), "the commit stays local");
    }

    #[test]
    fn a_restart_with_an_unpushed_commit_pushes_it_before_the_pull_and_before_any_effect() {
        let scratch = Scratch::new();
        let checkout = scratch.clone("tick");
        let system = logged(&scratch);
        let (id, segment) = intend(&checkout, "01-a/u/s");
        system
            .commit(&checkout, &[segment], &[], "tick 1, killed before its push")
            .unwrap()
            .unwrap();
        let other = scratch.clone("other");
        fs::write(other.join("README.md"), "other\n").unwrap();
        scratch::commit_all(&other, "unrelated");
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        fs::remove_file(scratch.dir.path().join("git.log")).ok();

        begin(&system, &checkout, &scratch::campaign(&checkout)).unwrap();
        let log = scratch.log();
        assert!(first(&log, "push ") < first(&log, "pull -q --no-rebase"));
        assert!(rows_on_main(&scratch).contains(&id.0));
        assert!(checkout.join("README.md").exists(), "the pull ran after");
        assert!(!system.unpushed(&checkout).unwrap());
    }

    #[test]
    fn a_conflicting_pull_is_aborted_and_the_tick_writes_nothing() {
        let scratch = Scratch::new();
        let checkout = conflicting(&scratch, "tick");
        let system = SystemGit::default();
        scratch::commit_all(&checkout, "my state");
        let head = system.head(&checkout).unwrap();
        let campaign = scratch::campaign(&checkout);
        let error = begin(&system, &checkout, &campaign).unwrap_err();
        assert!(
            matches!(&error, TickError::PullConflict { paths } if paths == &[format!("{EPOCH}/STATE.md")]),
            "{error}"
        );
        assert!(!merge_head(&checkout), "the merge was aborted");
        assert_eq!(system.head(&checkout).unwrap(), head);
        assert!(system.dirty(&checkout, Path::new(".")).unwrap().is_empty());
        assert!(
            !campaign.join(".cm/tick/effects-0001.jsonl").exists(),
            "no ledger row"
        );
        // A direct pull says the same and leaves the tree as it was.
        let pulled = system.pull_no_rebase(&checkout).unwrap();
        assert!(matches!(pulled, PullResult::Conflict { .. }));
        assert_eq!(
            fs::read_to_string(campaign.join("STATE.md")).unwrap(),
            "# State\n\nmine\n"
        );
    }

    #[test]
    fn a_gitignored_tick_file_is_committed_and_a_path_no_cm_command_writes_is_not() {
        let scratch = Scratch::new();
        let checkout = scratch.clone("tick");
        let campaign = scratch::campaign(&checkout);
        let system = SystemGit::default();
        let (id, _) = intend(&checkout, "01-a/u/s");
        fs::write(campaign.join(".cm/tick/cursor.json"), "{}\n").unwrap();
        fs::write(campaign.join("notes.txt"), "mine\n").unwrap();
        fs::write(campaign.join("STATE.md"), "# State\n\ncm wrote this\n").unwrap();
        // An Astra out-dir the tick named: its report goes in, its ignored transcript stays out.
        let out = campaign.join("01-a/u/s/evidence/astra-review-7");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("report.md"), "ACCEPT\n").unwrap();
        fs::write(out.join("transcript.jsonl"), "{}\n").unwrap();
        assert!(
            !system
                .dirty(&checkout, Path::new("."))
                .unwrap()
                .contains(&rel(".cm/tick/effects-0001.jsonl")),
            "an ignored segment is invisible to dirty"
        );
        let pushed = commit_and_push(
            &system,
            &checkout,
            &tick_files(&checkout, &campaign).unwrap(),
            &[
                owned_dirty(&system, &checkout, &campaign, false).unwrap(),
                vec![rel("01-a/u/s/evidence/astra-review-7")],
            ]
            .concat(),
            "tick",
        )
        .unwrap();
        assert!(pushed.commit.is_some());
        let committed = git(&checkout, &["show", "--name-only", "--format=", "HEAD"]);
        let committed: Vec<&str> = committed.lines().collect();
        assert!(committed.contains(&format!("{EPOCH}/.cm/tick/effects-0001.jsonl").as_str()));
        assert!(committed.contains(&format!("{EPOCH}/.cm/tick/cursor.json").as_str()));
        assert!(committed.contains(&format!("{EPOCH}/STATE.md").as_str()));
        assert!(
            committed
                .contains(&format!("{EPOCH}/01-a/u/s/evidence/astra-review-7/report.md").as_str())
        );
        assert!(
            !committed.iter().any(|path| path.ends_with("notes.txt")),
            "{committed:?}"
        );
        assert!(
            !committed
                .iter()
                .any(|path| path.ends_with("transcript.jsonl"))
        );
        assert!(!committed.iter().any(|path| path.ends_with("/lock")));
        assert!(rows_on_main(&scratch).contains(&id.0));
        assert!(
            system
                .dirty(&checkout, Path::new("."))
                .unwrap()
                .contains(&rel("notes.txt"))
        );

        for owned in [
            ".cm/state.json",
            ".cm/tick/effects-0003.jsonl",
            "STATE.md",
            "01-a/u/STATE.md",
            "JOURNAL.md",
            "CARD.md",
            "RESUME.md",
            "MAC-LANE.md",
            "OWNER-QUEUE.md",
            "REVIEW-OFF.md",
            "FEEDBACK.md",
            "AGENTS.jsonl",
            "01-a/u/s/evidence/reviews/R1-astra.md",
        ] {
            assert!(cm_owned(Path::new(owned)), "{owned}");
        }
        for not in [
            ".cm/lock",
            ".cm/tick/lock",
            "notes.txt",
            "01-a/u/JOURNAL.md",
            "01-a/u/s/evidence/astra-review-7/report.md",
            "RELEASE.md",
            "kickoff.md",
        ] {
            assert!(!cm_owned(Path::new(not)), "{not}");
        }
    }

    #[test]
    fn a_cm_commands_writes_and_a_killed_ticks_recorded_facts_are_committed_before_the_next_pull() {
        let scratch = Scratch::new();
        let checkout = scratch.clone("tick");
        let campaign = scratch::campaign(&checkout);
        let system = logged(&scratch);
        // A tick pushed its first commit, recorded a bind and a tally row, and was killed.
        let (id, segment) = intend(&checkout, "01-a/u/s");
        commit_and_push(&system, &checkout, &[segment], &[], "tick 4").unwrap();
        let base = system.head(&checkout).unwrap();
        PendingMark {
            tick: 4,
            base: base.clone(),
            phase: TickPhase::Record,
            paths: vec![rel("RELEASE.md")],
        }
        .save(&checkout)
        .unwrap();
        {
            let lock = tick_lock(&campaign);
            let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
            ledger
                .bind(
                    &clock(),
                    &id,
                    crate::tick::ledger::Bind::Session {
                        id: "s1".to_owned(),
                    },
                )
                .unwrap();
        }
        fs::write(campaign.join("RELEASE.md"), "| drop | 1 |\n").unwrap();
        // And a cm command wrote state before it.
        fs::write(campaign.join("STATE.md"), "# State\n\nsynced\n").unwrap();
        fs::write(campaign.join("notes.txt"), "not cm's\n").unwrap();
        fs::remove_file(scratch.dir.path().join("git.log")).ok();

        begin(&system, &checkout, &campaign).unwrap();
        let log = scratch.log();
        assert!(first(&log, "commit ") < first(&log, "pull -q --no-rebase"));
        assert_eq!(
            PendingMark::load(&checkout).unwrap(),
            None,
            "the mark is cleared"
        );
        assert!(rows_on_main(&scratch).contains("\"bound\""));
        assert_eq!(
            scratch.on_main(&format!("{EPOCH}/RELEASE.md")).unwrap(),
            "| drop | 1 |\n"
        );
        assert!(
            scratch
                .on_main(&format!("{EPOCH}/STATE.md"))
                .unwrap()
                .contains("synced")
        );
        assert_eq!(scratch.on_main(&format!("{EPOCH}/notes.txt")), None);
        let ledger = EffectLedger::snapshot(&campaign).unwrap();
        assert_eq!(ledger.state(&id), Some(IntentState::Bound));

        // With no mark at all, a killed `cm effects record`'s rows and a cm write still go first.
        let (late, _) = intend(&checkout, "01-a/u/t");
        fs::write(campaign.join("CARD.md"), "card\n").unwrap();
        begin(&system, &checkout, &campaign).unwrap();
        assert!(rows_on_main(&scratch).contains(&late.0));
        assert_eq!(
            scratch.on_main(&format!("{EPOCH}/CARD.md")).unwrap(),
            "card\n"
        );
    }

    #[test]
    fn a_wedged_owned_checkout_rebuilds_from_origin_keeping_every_ledger_and_agent_row() {
        let scratch = Scratch::new();
        let system = SystemGit::default();
        let campaign = scratch.dir.path().join("jail").join(EPOCH);
        assert!(ensure_checkout(&system, scratch.origin.to_str().unwrap(), &campaign).unwrap());
        let checkout = scratch.dir.path().join("jail");
        scratch::configure(&checkout);
        // The owned clone's tick wrote rows, an agent echo and a state change it cannot push.
        let (mine, segment) = intend(&checkout, "01-a/u/s");
        scratch::append(
            &campaign.join("AGENTS.jsonl"),
            "{\"invocation\":\"mine\"}\n",
        );
        fs::write(campaign.join("STATE.md"), "# State\n\nmine\n").unwrap();
        system
            .commit(
                &checkout,
                &[segment],
                &[rel("STATE.md"), rel("AGENTS.jsonl")],
                "tick 1",
            )
            .unwrap()
            .unwrap();
        // Meanwhile main moved with a conflicting state change and rows of its own.
        let other = scratch.clone("other");
        let (theirs, their_segment) = intend(&other, "01-a/u/t");
        fs::write(
            scratch::campaign(&other).join("STATE.md"),
            "# State\n\ntheirs\n",
        )
        .unwrap();
        commit_and_push(
            &system,
            &other,
            &[their_segment],
            &[rel("STATE.md")],
            "theirs",
        )
        .unwrap();

        begin(&system, &checkout, &campaign).unwrap();
        assert!(!merge_head(&checkout));
        assert!(!system.unpushed(&checkout).unwrap());
        let rows = rows_on_main(&scratch);
        assert!(rows.contains(&mine.0) && rows.contains(&theirs.0), "{rows}");
        let agents = scratch.on_main(&format!("{EPOCH}/AGENTS.jsonl")).unwrap();
        assert!(
            agents.contains("\"mine\"") && agents.contains("\"seed\""),
            "{agents}"
        );
        assert!(
            scratch
                .on_main(&format!("{EPOCH}/STATE.md"))
                .unwrap()
                .contains("theirs")
        );
        assert!(
            EffectLedger::snapshot(&campaign)
                .unwrap()
                .intent(&mine)
                .is_some()
        );

        // A checkout cm did not clone gets the error instead.
        let human = conflicting(&scratch, "human");
        scratch::commit_all(&human, "mine");
        assert!(rebuild(&system, &human, &scratch::campaign(&human)).is_err());
    }

    #[test]
    fn an_absent_checkout_is_cloned_and_a_reaped_one_recloned() {
        let scratch = Scratch::new();
        let system = SystemGit::default();
        let url = scratch.origin.to_str().unwrap();
        let checkout = scratch.dir.path().join("jail");
        let campaign = checkout.join(EPOCH);
        assert!(
            ensure_checkout(&system, url, &campaign).unwrap(),
            "absent: cloned"
        );
        assert!(campaign.join("STATE.md").exists());
        assert!(checkout.join(OWNED_MARK).exists());
        assert!(
            !ensure_checkout(&system, url, &campaign).unwrap(),
            "present: used"
        );
        // Reaped whole, then reaped to an empty directory.
        fs::remove_dir_all(&checkout).unwrap();
        assert!(ensure_checkout(&system, url, &campaign).unwrap());
        assert!(campaign.join("STATE.md").exists());
        fs::remove_dir_all(&checkout).unwrap();
        fs::create_dir_all(&checkout).unwrap();
        assert!(ensure_checkout(&system, url, &campaign).unwrap());
        assert!(checkout.join(OWNED_MARK).exists());
        assert_eq!(
            system.toplevel(&campaign).unwrap().canonicalize().unwrap(),
            checkout.canonicalize().unwrap()
        );
        // A folder that is not `<checkout>/store/epochs/<slug>` is not guessed at.
        assert!(ensure_checkout(&system, url, &scratch.dir.path().join("x/y")).is_err());
    }

    #[test]
    fn a_tick_killed_before_its_first_push_is_discarded_whole_and_its_tally_row_and_new_segment_go_with_it()
     {
        let scratch = Scratch::new();
        let checkout = scratch.clone("tick");
        let campaign = scratch::campaign(&checkout);
        let system = SystemGit::default();
        fs::write(campaign.join("RELEASE.md"), "| tally |\n").unwrap();
        fs::create_dir_all(campaign.join(".cm/tick")).unwrap();
        fs::write(campaign.join(".cm/tick/cursor.json"), "{\"tick\":3}\n").unwrap();
        scratch::commit_all(&checkout, "before");
        git(&checkout, &["push", "-q", "origin", "HEAD:main"]);
        let base = system.head(&checkout).unwrap();
        PendingMark {
            tick: 4,
            base: base.clone(),
            phase: TickPhase::Decide,
            paths: vec![rel("RELEASE.md")],
        }
        .save(&checkout)
        .unwrap();
        // The tick decides: a tally row, intents in a new segment, a cursor, a state write, and a
        // harvester's agent row lands meanwhile. Its first commit is made but never pushed.
        scratch::append(&campaign.join("RELEASE.md"), "| reserved | 7 |\n");
        let (_, segment) = intend(&checkout, "01-a/u/s");
        fs::write(campaign.join(".cm/tick/cursor.json"), "{\"tick\":4}\n").unwrap();
        fs::write(campaign.join("STATE.md"), "# State\n\ndecided\n").unwrap();
        system
            .commit(
                &checkout,
                &[segment, rel(".cm/tick/cursor.json")],
                &[rel("RELEASE.md"), rel("STATE.md")],
                "tick 4",
            )
            .unwrap()
            .unwrap();
        scratch::append(&campaign.join("AGENTS.jsonl"), "{\"harvested\":1}\n");
        // The next tick holds the tick lock while it begins.
        let lock = tick_lock(&campaign);

        begin(&system, &checkout, &campaign).unwrap();
        assert_eq!(system.head(&checkout).unwrap(), base);
        assert_eq!(
            fs::read_to_string(campaign.join("RELEASE.md")).unwrap(),
            "| tally |\n"
        );
        assert_eq!(
            fs::read_to_string(campaign.join(".cm/tick/cursor.json")).unwrap(),
            "{\"tick\":3}\n"
        );
        assert_eq!(
            fs::read_to_string(campaign.join("STATE.md")).unwrap(),
            "# State\n\nseed\n"
        );
        assert!(
            !campaign.join(".cm/tick/effects-0001.jsonl").exists(),
            "the new segment goes"
        );
        assert!(EffectLedger::snapshot(&campaign).unwrap().rows().is_empty());
        assert!(
            fs::read_to_string(campaign.join("AGENTS.jsonl"))
                .unwrap()
                .contains("harvested"),
            "AGENTS.jsonl is kept"
        );
        assert!(
            campaign.join(".cm/tick/lock").exists(),
            "the lock file is never cleaned"
        );
        assert_eq!(PendingMark::load(&checkout).unwrap(), None);
        assert_eq!(scratch.main_sha(), base, "nothing reached main");
        drop(lock);
    }

    #[test]
    fn a_conflict_inside_commit_and_push_keeps_the_commit_local_and_only_begin_rebuilds() {
        let scratch = Scratch::new();
        let system = SystemGit::default();
        let checkout = scratch.dir.path().join("jail");
        let campaign = checkout.join(EPOCH);
        ensure_checkout(&system, scratch.origin.to_str().unwrap(), &campaign).unwrap();
        scratch::configure(&checkout);
        let other = scratch.clone("other");
        fs::write(
            scratch::campaign(&other).join("STATE.md"),
            "# State\n\ntheirs\n",
        )
        .unwrap();
        scratch::commit_all(&other, "theirs");
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        let main = scratch.main_sha();

        let (mine, segment) = intend(&checkout, "01-a/u/s");
        fs::write(campaign.join("STATE.md"), "# State\n\nmine\n").unwrap();
        let error = commit_and_push(&system, &checkout, &[segment], &[rel("STATE.md")], "tick 9")
            .unwrap_err();
        assert!(matches!(error, TickError::PullConflict { .. }), "{error}");
        assert!(!merge_head(&checkout));
        assert!(
            system.unpushed(&checkout).unwrap(),
            "the commit stays local"
        );
        assert_eq!(scratch.main_sha(), main, "and no rebuild pushed anything");
        assert!(
            fs::read_to_string(campaign.join("STATE.md"))
                .unwrap()
                .contains("mine")
        );

        begin(&system, &checkout, &campaign).unwrap();
        assert!(
            rows_on_main(&scratch).contains(&mine.0),
            "begin rebuilt, keeping the row"
        );
        assert!(
            fs::read_to_string(campaign.join("STATE.md"))
                .unwrap()
                .contains("theirs")
        );
    }

    #[test]
    fn a_merge_left_by_a_killed_git_child_is_aborted_before_anything_else() {
        // No union attribute, so begin stops at step 1: the abort at step 0 happened first.
        let scratch = Scratch::with_attributes("");
        let checkout = conflicting(&scratch, "tick");
        scratch::commit_all(&checkout, "mine");
        assert!(!scratch::git_ok(
            &checkout,
            &["pull", "-q", "--no-rebase", "--no-edit", "origin", "main"]
        ));
        assert!(merge_head(&checkout), "a merge in progress");
        let system = logged(&scratch);
        let error = begin(&system, &checkout, &scratch::campaign(&checkout)).unwrap_err();
        assert!(matches!(error, TickError::LedgerNotUnion { .. }), "{error}");
        assert!(!merge_head(&checkout));
        let log = scratch.log();
        assert_eq!(log[0], "rev-parse -q --verify MERGE_HEAD");
        assert_eq!(log[1], "merge --abort");
        assert!(
            !system.abort_merge(&checkout).unwrap(),
            "nothing left to abort"
        );
    }
}
