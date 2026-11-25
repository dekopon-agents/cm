use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum TickError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}
