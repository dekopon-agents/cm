//! `cm tick`: one bounded, code-owned coordinator cycle (IDEA-0063; PLAN.md `07-tick`).
//!
//! Skeleton (cm branch `design/skeleton-tick`; campaign-trail
//! `store/epochs/eleemosynary-mnemosyne/06-designs/skeleton-tick/SKELETON.md`). Units fill
//! `todo!()` bodies and never add a `mod`, `pub use`, error variant, CLI flag or dependency; that
//! is a skeleton change (standing order B2).
//!
//! - Process model (S-tick-42): synchronous and one-shot on the main thread, no async runtime.
//!   Every outward call is a child process through `forge::run`, waited for with a timeout.
//! - Concurrency (S-tick-05): the tick lock (`ledger::TickLock`) serializes every writer of
//!   [`TICK_DIR`]; existing cm commands keep `store::lock`, taken and released inside each call.
//! - Durability: everything that outlives the process is in [`TICK_DIR`] (the ledger, cursor,
//!   timers, seen set) or in cm's existing state, and is pushed to campaign-trail main before any
//!   effect it allows happens.
//! - Errors (S-tick-01): one enum, [`TickError`]; per-item failures are data, not errors.
//! - Bounds (S-tick-06): every per-tick and durable bound is a constant in the module it limits.

pub mod agents;
pub mod apply;
pub mod authority;
pub mod dispatch;
pub mod error;
pub mod export;
pub mod gate;
pub mod intake;
pub mod lan;
pub mod ledger;
pub mod merge;
pub mod packet;
pub mod push;
pub mod record;
pub mod release;
pub mod resolve;
pub mod run;
pub mod timers;
pub mod wire;

pub use apply::{Applier, Apply, LocalApply};
pub use authority::{Approved, ApprovedSha};
pub use error::TickError;
pub use ledger::{EffectLedger, IntentId, IntentRow, Nonce, TickLock};
pub use packet::DecisionPacket;
pub use run::{Cycle, Deps, TickArgs, cli_tick, tick};
pub use wire::{Capability, Effect, EffectsOut, Receipt, ReceiptOutput};

/// The crate's one error type for the tick tree; it crosses into `anyhow` at the CLI seam.
pub type Result<T> = std::result::Result<T, TickError>;

/// Every tick-owned file lives here, relative to the campaign folder.
pub const TICK_DIR: &str = ".cm/tick";
