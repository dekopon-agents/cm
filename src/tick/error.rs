//! S-tick-01: the one `thiserror` enum for the tick tree. Refusals the cycle reports (a refused
//! verdict, a held pin, a refused release) are data in their own modules and reach the decision
//! packet; they are never errors. An error aborts the tick before its next ledger push.

use std::path::PathBuf;
use std::time::Duration;

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
    #[error("{path}:{line}: unreadable row: {source}")]
    Row {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("{program} {op} exited {code:?}")]
    Exit {
        program: String,
        op: &'static str,
        code: Option<i32>,
    },
    #[error("{program} {op} timed out after {after:?}")]
    Timeout {
        program: String,
        op: &'static str,
        after: Duration,
    },
    /// 11-D4: the runner entry's SHA is unreachable after one `git fetch origin <sha>`.
    #[error("approved revision {sha} is unreachable after one fetch")]
    ApprovedUnreachable { sha: String },
    /// 11-D4: LIMITS.toml or its `[authority]` table is missing or malformed at the approved SHA.
    #[error("no usable [authority] at {sha}: {detail}")]
    AuthorityMissing { sha: String, detail: String },
    /// 11-D8: three non-fast-forward rejects in a row.
    #[error("ledger push rejected {tries} times")]
    PushExhausted { tries: u8 },
    /// 11-D3: a `cm effects record` stdin over the batch bound.
    #[error("receipt batch of {bytes} bytes exceeds {max}")]
    BatchTooLarge { bytes: usize, max: usize },
    /// 11-D3: step one's stdout came back marked `truncated`.
    #[error("tick output was truncated; nothing may be applied")]
    Truncated,
    #[error("receipt names intent {0}, which the ledger does not hold open")]
    UnknownIntent(IntentId),
    #[error("intent {0} already has a different receipt")]
    ReceiptConflict(IntentId),
    #[error("forge read failed: {0}")]
    Forge(ForgeError),
}

/// The seam from the forge reads; `ForgeError` has `Display` but no `std::error::Error`.
impl From<ForgeError> for TickError {
    fn from(error: ForgeError) -> Self {
        TickError::Forge(error)
    }
}
