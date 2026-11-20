//! The decision packet (`tick/core`): the only thing a model reads, bounded and built from
//! deltas (IDEA-0063 §Boundary). An empty packet means nothing needs judgment, and the
//! coordinator's turn ends (PLAN.md `07-tick/interim-cutover`).
//!
//! Delivery is at most once per delta: the keys of a packet's items go into [`Seen`] in the
//! tick's first commit, before the packet is printed or returned. A state that persists (a parked
//! intent, a held pin) comes back through its reminder timer, so nothing is lost when a delivery
//! is. The rendered `text` is what the runner posts (11-D3 (4)) and what `cm tick` prints.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tick::ledger::IntentId;
use crate::tick::wire::PacketOut;

pub const PACKET_MAX_ITEMS: usize = 20;
/// Leaves room under the 64 KiB tick stdout for the effects.
pub const PACKET_MAX_BYTES: usize = 16 * 1024;
pub const SEEN_FILE: &str = ".cm/tick/seen.json";
/// Growth bound on the seen set; the oldest keys drop first.
pub const MAX_SEEN: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PacketKind {
    /// The tick started and failed; the item names the error. Exit is still 0 (S-tick-10).
    TickAborted,
    /// `paused` changed; only the change is an item.
    Paused,
    /// 11-D4: the approved revision or its tables could not be read; nothing was applied.
    AuthorityUnavailable,
    /// An intent reconcile could not decide; answered by `cm effects resolve`.
    Parked,
    /// A `cm sync` violation: a merge-gate gap, a PR closed unmerged, an unknown read.
    Sync,
    /// The forge could not be read for some PRs this tick; one summary item.
    Unread,
    /// The open-intent or timer bound was reached.
    LedgerFull,
    /// A verdict the gate refused, with the refusal.
    VerdictRefused,
    /// A reviewer's third FIX REQUIRED on one PR: the lane goes to the owner (EXECUTION.md
    /// §Lanes).
    ReviewBudget,
    /// A merged head that is not the gated head: revert PR, every pin held, journaled.
    HeadMovedAfterMerge,
    ReleaseRefused,
    PinHeld,
    ProbeFailed,
    /// A PR or release `[authority]` does not cover waits.
    OutOfAuthority,
    /// An effect the local applier cannot run, for the coordinator (S-tick-24).
    ApplyAction,
    /// A launch the coordinator performs (S-tick-37).
    LaunchAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketItem {
    /// Stable across ticks for one delta: what [`Seen`] records.
    pub key: String,
    pub kind: PacketKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<IntentId>,
    /// One line.
    pub summary: String,
    /// The exact commands that answer or perform it, when there are any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionPacket {
    pub items: Vec<PacketItem>,
    /// Items past either bound; unseen, so they come back next tick.
    pub omitted: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// Oldest first.
    pub keys: Vec<String>,
}

impl DecisionPacket {
    /// Adds an item unless its key is seen or already present; counts it `omitted` past either
    /// bound.
    pub fn push(&mut self, seen: &Seen, item: PacketItem) {
        todo!("tick/core")
    }

    pub fn is_empty(&self) -> bool {
        todo!("tick/core")
    }

    /// Markdown, one line per item plus its action, within [`PACKET_MAX_BYTES`].
    pub fn render(&self) -> String {
        todo!("tick/core")
    }

    /// `None` when empty.
    pub fn to_wire(&self) -> Option<PacketOut> {
        todo!("tick/core")
    }
}

impl Seen {
    /// Absent is empty.
    pub fn load(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }

    /// Adds the packet's keys, dropping the oldest past [`MAX_SEEN`].
    pub fn deliver(&mut self, packet: &DecisionPacket) {
        todo!("tick/core")
    }

    /// Written atomically; the caller adds the path to the tick's first commit.
    pub fn save(&self, root: &Path) -> crate::tick::Result<()> {
        todo!("tick/core")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_tick_killed_before_its_first_push_marks_no_packet_item_seen() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_reloaded_seen_set_keeps_a_delivered_delta_out_of_the_next_packet() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_packet_holds_at_most_twenty_items_or_sixteen_kib_and_the_seen_set_at_most_1024_keys() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_restart_brings_a_parked_intent_back_only_through_its_reminder() {
        todo!()
    }
}
