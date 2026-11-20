//! S-tick-01: the one `thiserror` enum for the tick tree.
//!
//! Error policy (S-tick-01). An error is only what stops the tick from writing durable state
//! correctly: the campaign files, the ledger, git. It aborts the tick before its next push, so
//! the next tick redoes the work from what was pushed. Everything about one PR, step or effect is
//! data, never an error: a refused verdict, a held pin, a forge read that failed for one PR (that
//! item is deferred and counted, and the tick goes on). Units add no variant without a skeleton
//! change. The enum crosses into `anyhow` only at the CLI seam in `cli.rs`.

use std::path::PathBuf;

use crate::forge::ForgeError;
use crate::tick::ledger::IntentId;

#[derive(Debug, thiserror::Error)]
pub enum TickError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A durable JSON or JSON Lines file cm cannot read back.
    #[error("{path}:{line}: unreadable: {source}")]
    Row {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    /// A child process (`git`, `gh`, `astra-review.sh`) through `forge::run`: spawn failure, a
    /// non-zero exit or the timeout. `ForgeError` has `Display` but is not `std::error::Error`.
    #[error("{program} {op}: {error}")]
    Child {
        program: &'static str,
        op: &'static str,
        error: ForgeError,
    },
    /// An existing cm command the tick calls as a library (`cm launch`, `cm attempt`, `cm sync`,
    /// `cm mac-lane add`) failed rather than refused; a refusal is its `Outcome`, which is data.
    #[error("cm {op}: {error:#}")]
    Cm {
        op: &'static str,
        error: anyhow::Error,
    },
    /// 11-D4: the runner entry's SHA is unreachable after one `git fetch origin <sha>`.
    #[error("approved revision {sha} is unreachable after one fetch")]
    ApprovedUnreachable { sha: String },
    /// 11-D4: LIMITS.toml, `[authority]`, `[gates]` or `[review]` is missing or malformed there.
    #[error("no usable authority at {sha}: {detail}")]
    AuthorityMissing { sha: String, detail: String },
    /// 11-D7: `git pull --no-rebase` conflicted; the merge was aborted and nothing was written.
    #[error("pull conflicted in {paths:?}; merge aborted")]
    PullConflict { paths: Vec<String> },
    /// 11-D8: non-fast-forward rejects on every try.
    #[error("ledger push rejected {tries} times")]
    PushExhausted { tries: u8 },
    /// 11-D3: a `cm effects record` stdin over the batch bound; nothing of it is recorded.
    #[error("receipt batch of {bytes} bytes exceeds {max}")]
    BatchTooLarge { bytes: usize, max: usize },
    /// 11-D3: stdin is not a JSON array of receipts; nothing of it is recorded.
    #[error("receipt batch unreadable: {detail}")]
    BatchInvalid { detail: String },
    #[error("intent {0} is not in the ledger")]
    UnknownIntent(IntentId),
    #[error("intent {0} already holds a different receipt")]
    ReceiptConflict(IntentId),
    /// S-tick-14: the open-intent bound; the planning stage parks instead of writing one more.
    #[error("{open} open intents; the bound is {max}")]
    LedgerFull { open: usize, max: usize },
    /// S-tick-34: the armed-timer bound.
    #[error("{armed} timers armed; the bound is {max}")]
    TimersFull { armed: usize, max: usize },
}
