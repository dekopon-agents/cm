//! The decision packet (`tick/core`): the only thing the model reads, bounded, built from deltas.
//! An empty packet means nothing needs judgment and the turn ends.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tick::ledger::IntentId;

pub const PACKET_MAX_ITEMS: usize = 20;
/// Leaves room under the 64 KiB tick stdout for the effects.
pub const PACKET_MAX_BYTES: usize = 16 * 1024;
/// Item keys already delivered, so a restart does not repeat one.
pub const SEEN_FILE: &str = ".cm/packet-seen.json";
/// Growth bound on the seen set; the oldest keys drop first.
pub const MAX_SEEN: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PacketKind {
    Parked,
    AuthorityUnavailable,
    VerdictRefused,
    HeadMovedAfterMerge,
    ReleaseRefused,
    PinHeld,
    ProbeFailed,
    OutOfAuthority,
    /// The interim enqueue the coordinator runs with MCP `enable_pr_auto_merge`.
    InterimEnqueue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketItem {
    /// Stable across ticks: what the seen set records.
    pub key: String,
    pub kind: PacketKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<IntentId>,
    /// One line.
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionPacket {
    pub items: Vec<PacketItem>,
    /// Items past either bound; they stay unseen and come back next tick.
    pub omitted: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub keys: Vec<String>,
}

impl DecisionPacket {
    /// Adds an item unless its key is seen; counts it `omitted` past either bound.
    pub fn push(&mut self, seen: &Seen, item: PacketItem) {
        todo!("tick/core")
    }

    pub fn is_empty(&self) -> bool {
        todo!("tick/core")
    }
}

impl Seen {
    pub fn load(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }

    /// Records the delivered packet's keys, dropping the oldest past [`MAX_SEEN`].
    pub fn deliver(&mut self, packet: &DecisionPacket, root: &Path) -> crate::tick::Result<()> {
        todo!("tick/core")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn an_aborted_tick_marks_no_packet_item_seen() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_reloaded_seen_set_keeps_a_delivered_item_out_of_the_next_packet() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_packet_holds_at_most_twenty_items_or_sixteen_kib_and_counts_the_rest() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn restart_rebuilds_the_same_packet_from_deltas_without_repeating_delivered_items() {
        todo!()
    }
}
