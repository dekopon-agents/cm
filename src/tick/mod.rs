pub mod act;
pub mod beat;
pub mod claude_md;
pub mod collect;
pub mod directive;
pub mod error;
pub mod exec;
pub mod gitsync;
pub mod pin;
pub mod proc;

pub use error::TickError;

pub type Result<T> = std::result::Result<T, TickError>;
