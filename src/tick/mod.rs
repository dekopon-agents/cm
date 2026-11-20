//! `cm tick`: one bounded, code-owned coordinator cycle (IDEA-0063; PLAN.md `07-tick`).
//!
//! Skeleton (design/skeleton-tick, SKELETON.md in campaign-trail
//! `store/epochs/eleemosynary-mnemosyne/06-designs/skeleton-tick/`). Every body is `todo!()`;
//! units fill bodies and never add a `mod`, `pub use` or dependency here.
//!
//! The cycle is synchronous and runs on the one thread of a one-shot `cm` process: no task is
//! spawned, every subprocess (`git`, `gh`, `astra-review.sh`) is awaited with a timeout, and all
//! state that outlives the process is in the campaign folder (`.cm/effects.jsonl`,
//! `.cm/tick-cursor.json`, `.cm/timers.json`, `.cm/packet-seen.json`), pushed to campaign-trail main.

pub mod agents;
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
pub mod run;
pub mod timers;
pub mod wire;

pub use authority::{ApprovedSha, Authority};
pub use error::TickError;
pub use ledger::{EffectLedger, IntentId, IntentRow, Nonce};
pub use packet::DecisionPacket;
pub use run::{Cycle, TickArgs, cli_tick, tick};
pub use wire::{Capability, Effect, EffectsOut, Receipt, ReceiptOutput};

/// The crate's one error type for the tick tree; it crosses into `anyhow` at the CLI seam.
pub type Result<T> = std::result::Result<T, TickError>;
