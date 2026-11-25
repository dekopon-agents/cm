//! `cm tick`: heartbeat.sh's routine coordinator moves as typed Rust (the 07-tick MVP).
//!
//! [`beat`] is the frame and the act loop; [`act`] is the contract executors and phases are
//! written against; [`proc`] runs every child by argv in its own process group; [`exec`] holds
//! the executors and the housekeeping phases are [`gitsync`], [`pin`], [`directive`],
//! [`collect`] and [`claude_md`]. Module names stay clear of the 07-tick skeleton's (cm#114);
//! [`TickError`] and [`Result`] are its names, so 07-tick/core extends them.

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

/// The tick tree's one result type.
pub type Result<T> = std::result::Result<T, TickError>;
