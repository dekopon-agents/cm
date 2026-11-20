//! The effect ledger (`tick/ledger`; IDEA-0063 §Recovery). Before each external effect cm appends
//! an intent row to `.cm/effects.jsonl` and pushes it to campaign-trail main (11-D8). On restart
//! [`reconcile`] looks for the matching reviewer session or comment, the exact-head merge guard and
//! the PR and tag identity: a match is bound, an undecidable lookup parks the unit with a packet
//! item, and no launch is duplicated.
//!
//! Append-only JSON lines. A row is never rewritten; a later row binds, parks or records it.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::host::Host;
use crate::tick::wire::{Capability, Receipt};

/// Relative to the campaign (epoch) folder.
pub const LEDGER_FILE: &str = ".cm/effects.jsonl";
/// Growth bound: open intents across all units. The tick emits nothing new past it and parks.
pub const MAX_OPEN_INTENTS: usize = 64;

/// `i-<stamp>-<seq>`: unique within the ledger, assigned in append order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntentId(pub String);

impl std::fmt::Display for IntentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One per review, minted before launch and put in the reviewer's prompt. A correlation value,
/// not a secret (IDEA-0063 §Trust boundary).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nonce(pub String);

/// The model family that wrote or reviewed a PR; a verdict must come from the other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Claude,
    Gpt,
}

/// What an intent is for. `capability` is set when the runner applies it; launches and Mac-lane
/// items are cm's own actions and carry none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntentKind {
    StepLaunch,
    ReviewLaunch,
    MacLaneItem,
    Merge,
    Enqueue,
    ReleaseCut,
    PinUpdate,
    PinMerge,
    ArgoRefresh,
    Probe,
}

/// IDEA-0063: "PR, head SHA, nonce, intended model and family", plus the session id when the
/// launch returns one (that arrives as a later [`LedgerRow::Bound`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentRow {
    pub id: IntentId,
    /// `cm` stamp, `YYYY-MM-DDTHH:MMZ`.
    pub at: String,
    /// `<NN-name>/<unit>/<step>`.
    pub target: String,
    pub kind: IntentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<Nonce>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<Family>,
}

/// One JSON line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "kebab-case")]
pub enum LedgerRow {
    Intent(IntentRow),
    /// The effect is known done: a session id, comment id, merge SHA or tag.
    Bound {
        intent: IntentId,
        at: String,
        evidence: String,
    },
    /// Undecidable; the unit waits for a ruling through the decision packet.
    Parked {
        intent: IntentId,
        at: String,
        reason: String,
    },
    /// The runner's receipt (11-D3 step 3).
    Receipt {
        at: String,
        receipt: Receipt,
    },
}

/// What an open intent's lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Done {
        evidence: String,
    },
    /// Known not to have happened: the same intent may be emitted again, never a new one.
    NotDone,
    Undecidable {
        reason: String,
    },
}

/// The lookups [`reconcile`] uses: sessions and comments, the exact-head merge guard, PR and tag
/// identity. Read-only.
pub trait Observer {
    fn observe(&self, row: &IntentRow) -> crate::tick::Result<Observation>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub bound: Vec<IntentId>,
    pub parked: Vec<IntentId>,
    /// Known not done; re-emitted under the same id.
    pub retry: Vec<IntentId>,
}

/// The loaded ledger. Holds every row of the file; `cm` loads it once per command under the
/// campaign lock.
#[derive(Debug)]
#[expect(dead_code, reason = "skeleton: tick/ledger reads these fields")]
pub struct EffectLedger {
    path: PathBuf,
    rows: Vec<LedgerRow>,
}

/// The fields a caller supplies; the ledger assigns `id` and `at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub target: String,
    pub kind: IntentKind,
    pub capability: Option<Capability>,
    pub repo: Option<String>,
    pub pr: Option<u64>,
    pub head: Option<String>,
    pub nonce: Option<Nonce>,
    pub model: Option<String>,
    pub family: Option<Family>,
}

impl EffectLedger {
    /// Reads `root/.cm/effects.jsonl`; absent is empty, an unreadable line is an error.
    pub fn open(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/ledger")
    }

    pub fn path(&self) -> &Path {
        todo!("tick/ledger")
    }

    pub fn rows(&self) -> &[LedgerRow] {
        todo!("tick/ledger")
    }

    /// Intents with no Bound, Parked or Receipt row after them.
    pub fn open_intents(&self) -> Vec<&IntentRow> {
        todo!("tick/ledger")
    }

    pub fn intent(&self, id: &IntentId) -> Option<&IntentRow> {
        todo!("tick/ledger")
    }

    /// Appends and fsyncs one intent row. Refuses past [`MAX_OPEN_INTENTS`] and refuses a second
    /// open launch intent for the same target.
    pub fn intend(&mut self, host: &dyn Host, draft: Draft) -> crate::tick::Result<IntentId> {
        todo!("tick/ledger")
    }

    pub fn bind(
        &mut self,
        host: &dyn Host,
        id: &IntentId,
        evidence: &str,
    ) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }

    pub fn park(
        &mut self,
        host: &dyn Host,
        id: &IntentId,
        reason: &str,
    ) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }

    /// Idempotent by intent id: the same receipt twice is a no-op, a different one a conflict.
    pub fn record(&mut self, host: &dyn Host, receipt: Receipt) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }

    /// A fresh nonce no row in the ledger carries.
    pub fn mint_nonce(&self, host: &dyn Host) -> Nonce {
        todo!("tick/ledger")
    }

    /// The intent that carries `nonce`, if any; the verdict gate's lookup.
    pub fn by_nonce(&self, nonce: &Nonce) -> Option<&IntentRow> {
        todo!("tick/ledger")
    }

    /// True once a Bound row cites `nonce`: a second verdict with it is a replay.
    pub fn nonce_consumed(&self, nonce: &Nonce) -> bool {
        todo!("tick/ledger")
    }

    /// The latest receipt for an intent.
    pub fn receipt(&self, id: &IntentId) -> Option<&Receipt> {
        todo!("tick/ledger")
    }
}

/// Binds, parks or marks for retry every open intent, appending one row per decided intent.
pub fn reconcile(
    host: &dyn Host,
    ledger: &mut EffectLedger,
    observer: &dyn Observer,
) -> crate::tick::Result<Reconciled> {
    todo!("tick/ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn aborted_append_leaves_no_partial_intent_row() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn reopened_ledger_reads_every_row_it_wrote_in_order() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn open_intents_never_exceed_the_cap_and_a_target_holds_one_open_launch() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn restart_between_intent_and_effect_reconciles_or_parks_with_no_second_launch() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_minted_nonce_is_unique_in_the_ledger_and_consumed_once_bound() {
        todo!()
    }
}
