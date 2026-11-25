//! The one `thiserror` enum for the tick tree.
//!
//! An error here is only what stops one child process from being run at all; everything about
//! one act line, executor or phase is data (a printed line), never an error, so `cm tick` exits
//! 0 whenever its loop ran. 07-tick/core extends this enum with the skeleton's variants
//! (cm#114 `src/tick/error.rs`); the names here do not collide with them.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum TickError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A child process could not be started, or its pipes or exit status could not be read.
    #[error("{program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}
