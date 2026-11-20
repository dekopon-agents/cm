//! The effect ledger (`tick/ledger`; IDEA-0063 §Recovery; PLAN.md `07-tick/ledger`).
//!
//! Before each external effect cm appends an intent row to [`LEDGER_FILE`] in the epoch folder
//! and pushes it to campaign-trail main (11-D8): a runner effect, a review launch or a step
//! launch. On restart [`reconcile`] asks the [`Observer`] whether each open intent happened (the
//! reviewer session or comment, the exact-head merge guard, the PR and tag identity): a match is
//! bound, a lookup that cannot decide parks the intent with a packet item, and a known-not-done
//! intent is emitted again under the same id. Nothing ever launches twice.
//!
//! The file is JSON Lines, one [`LedgerRow`] per line, append-only: a row is never rewritten, and
//! a later row binds, parks, records, rules or consumes an earlier intent. Only a holder of the
//! tick lock (S-tick-05) appends. A last line without its newline is a torn append from a killed
//! process; [`EffectLedger::open`] drops it, which is safe because no effect is emitted before its
//! row is pushed. The file grows by at most five rows per intent and lives and closes with its
//! epoch (S-tick-06).
//!
//! A step launch also writes the step's existing attempt record (`cm attempt begin`/`record`,
//! `src/attempt.rs`). The intent row names that attempt, and reconcile reads the attempt's own
//! plan (`attempt::plan`), so the two records never disagree about whether a launch happened.

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::Host;
use crate::state::Lane;
use crate::tick::dispatch::LaunchRoute;
use crate::tick::run::Deps;
use crate::tick::wire::{Capability, Receipt};

/// Relative to the campaign (epoch) folder. The top-level `.gitignore` of campaign-trail ignores
/// `*.jsonl`, so the tick adds its files with `git add --force` (S-tick-20).
pub const LEDGER_FILE: &str = ".cm/tick/effects.jsonl";
/// Growth bound: open intents across the epoch. Past it the planning stages write no intent and
/// put one `ledger-full` packet item instead.
pub const MAX_OPEN_INTENTS: usize = 64;
/// `i-YYYYMMDDTHHMM-NNNN`; [`crate::tick::wire::admit`] sizes effects with this length.
pub const INTENT_ID_MAX_LEN: usize = 20;

/// Unique within the ledger, assigned in append order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntentId(pub String);

impl std::fmt::Display for IntentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One per review, minted before launch and put in the reviewer's prompt (EXECUTION.md §Verdict
/// grammar: `[A-Za-z0-9_-]{16,64}`). A correlation value, not a secret (IDEA-0063 §Trust
/// boundary): 32 lower-case hex digits derived from the intent id, so it is unique in the ledger
/// without randomness the [`Host`] does not offer.
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

impl Family {
    /// The writer family of a step's lane: codex writes GPT, claude-cloud and fable write Claude;
    /// owner, coordinator and pi steps have no reviewed family.
    pub fn writer_of(lane: Lane) -> Option<Family> {
        todo!("tick/ledger")
    }

    pub fn other(self) -> Family {
        todo!("tick/ledger")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntentKind {
    /// `cm launch` of a ready step and its dispatch; `attempt` names the attempt.
    StepLaunch,
    /// A review start with a fresh nonce; `route` says where to look for it.
    ReviewLaunch,
    /// An 11-D3 effect; `capability` and `input` are set.
    Effect,
}

/// IDEA-0063 and the done check: "PR, head SHA, nonce, intended model and family", plus what a
/// retry needs to emit the same effect again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentRow {
    pub id: IntentId,
    /// `cm` stamp, `YYYY-MM-DDTHH:MMZ`.
    pub at: String,
    /// `<NN-name>/<unit>/<step>`.
    pub target: String,
    pub kind: IntentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    /// The effect's input exactly as emitted, so a retry re-emits the same bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u32>,
    /// The head the effect or review is bound to, 40 hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<Nonce>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// For a review, the reviewer's family; for an effect on a PR, the writer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<Family>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<LaunchRoute>,
    /// A step launch's attempt id, `<step>#<launch>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    /// The intent whose recorded receipt this one was built from (a pin merge after its update,
    /// the LAN leg after a pin merge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<IntentId>,
}

/// The fields a caller supplies; the ledger assigns `id` and `at`.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub target: String,
    pub kind: IntentKind,
    pub capability: Option<Capability>,
    pub input: Option<Value>,
    pub repo: Option<String>,
    pub pr: Option<u32>,
    pub head: Option<String>,
    pub nonce: Option<Nonce>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub family: Option<Family>,
    pub route: Option<LaunchRoute>,
    pub attempt: Option<String>,
    pub follows: Option<IntentId>,
}

/// What showed an intent happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "kebab-case")]
pub enum Bind {
    /// A Claude session or Codex Cloud task id.
    Session {
        id: String,
    },
    /// An Astra thread, with its out-dir under the step's evidence.
    Thread {
        id: String,
        out_dir: String,
    },
    /// A MAC-LANE.md item that carries the nonce.
    MacLane {
        number: u32,
    },
    /// A packet action the coordinator performs; its receipt comes by `cm effects record`.
    Action {
        key: String,
    },
    Merged {
        sha: String,
    },
    Enqueued {
        entry: String,
    },
    Tag {
        tag: String,
    },
    Pull {
        number: u32,
        head: String,
    },
}

/// A ruling on a parked intent: `cm effects resolve` (S-tick-22). In the interim the coordinator
/// writes it; on the RPi the coordinator agent pushes it (11-D22). The RPi coordinator acts on it
/// only within `[authority]` at the approved SHA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "kebab-case")]
pub enum Ruling {
    /// It happened; `evidence` is a campaign file that shows it.
    Done { evidence: String },
    /// It did not happen: emit it again under the same id.
    Retry,
    /// Close it without doing it.
    Drop { reason: String },
}

/// A verdict the gate bound (S-tick-50). Writing it consumes the review intent's nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRow {
    /// The review launch intent whose nonce the verdict carried.
    pub intent: IntentId,
    pub head: String,
    pub word: crate::verdict::Verdict,
    pub proof: VerdictProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "proof", rename_all = "kebab-case")]
pub enum VerdictProof {
    /// A PR issue comment with its `performed_via_github_app` slug.
    Comment { id: u64, app: String },
    /// An Astra `receipt.json` and the recorded report whose SHA-256 it names.
    Astra { report_sha256: String },
}

/// One JSON line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "kebab-case")]
pub enum LedgerRow {
    Intent(IntentRow),
    Bound {
        intent: IntentId,
        at: String,
        bind: Bind,
    },
    /// Undecidable; the intent waits for a ruling through the decision packet.
    Parked {
        intent: IntentId,
        at: String,
        reason: String,
    },
    /// The runner's receipt (11-D3 (3)), or the local applier's in the interim.
    Receipt {
        at: String,
        receipt: Receipt,
    },
    Ruled {
        intent: IntentId,
        at: String,
        ruling: Ruling,
    },
    Verdict {
        at: String,
        #[serde(flatten)]
        verdict: VerdictRow,
    },
}

/// An intent's state, read from the rows after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    /// No closing row yet, or a receipt with outcome `error`, or a `retry` ruling.
    Open,
    /// Bound `via: action`: handed to the coordinator as a packet action. Reconcile looks for it
    /// done but never re-emits it; its receipt or a ruling closes it.
    Handed,
    Bound,
    Parked,
    /// A receipt with outcome `applied` or `refused`.
    Received,
    /// Ruled done or dropped.
    Closed,
}

/// [`EffectLedger::intend`]'s answer: a second intent for the same target and purpose while one
/// is open is never written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intended {
    New(IntentId),
    AlreadyOpen(IntentId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    New,
    /// The same receipt for the same intent again: a no-op.
    Duplicate,
}

/// The loaded ledger. `cm` loads it once per command, under the tick lock.
#[derive(Debug)]
#[expect(dead_code, reason = "skeleton: tick/ledger reads these fields")]
pub struct EffectLedger {
    path: PathBuf,
    rows: Vec<LedgerRow>,
}

impl EffectLedger {
    /// Reads `root/`[`LEDGER_FILE`]. Absent is empty; a torn last line is dropped and truncated;
    /// any other unreadable line is [`crate::tick::TickError::Row`].
    pub fn open(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/ledger")
    }

    pub fn path(&self) -> &Path {
        todo!("tick/ledger")
    }

    pub fn rows(&self) -> &[LedgerRow] {
        todo!("tick/ledger")
    }

    pub fn state(&self, id: &IntentId) -> Option<IntentState> {
        todo!("tick/ledger")
    }

    /// Intents in [`IntentState::Open`], oldest first.
    pub fn open_intents(&self) -> Vec<&IntentRow> {
        todo!("tick/ledger")
    }

    /// Intents in [`IntentState::Parked`], oldest first.
    pub fn parked(&self) -> Vec<&IntentRow> {
        todo!("tick/ledger")
    }

    pub fn intent(&self, id: &IntentId) -> Option<&IntentRow> {
        todo!("tick/ledger")
    }

    /// Appends one intent row as one `write` of one whole line, then fsyncs. Refuses past
    /// [`MAX_OPEN_INTENTS`] with [`crate::tick::TickError::LedgerFull`]; answers
    /// [`Intended::AlreadyOpen`] when the same target already holds an open or parked intent of
    /// the same kind, capability and head.
    pub fn intend(&mut self, host: &dyn Host, draft: Draft) -> crate::tick::Result<Intended> {
        todo!("tick/ledger")
    }

    pub fn bind(&mut self, host: &dyn Host, id: &IntentId, bind: Bind) -> crate::tick::Result<()> {
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

    /// Idempotent by intent: the same receipt twice is [`Recorded::Duplicate`], a different one
    /// for an intent already received is [`crate::tick::TickError::ReceiptConflict`], and a
    /// receipt for an intent not in the ledger is [`crate::tick::TickError::UnknownIntent`].
    pub fn record(&mut self, host: &dyn Host, receipt: Receipt) -> crate::tick::Result<Recorded> {
        todo!("tick/ledger")
    }

    pub fn rule(
        &mut self,
        host: &dyn Host,
        id: &IntentId,
        ruling: Ruling,
    ) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }

    /// Appends a bound verdict. The gate has already refused a consumed nonce (S-tick-51).
    pub fn verdict(&mut self, host: &dyn Host, verdict: VerdictRow) -> crate::tick::Result<()> {
        todo!("tick/ledger")
    }

    /// The nonce a review intent with this id carries: 32 hex digits of SHA-256 over the ledger's
    /// path inside the checkout and the id.
    pub fn mint_nonce(&self, id: &IntentId) -> Nonce {
        todo!("tick/ledger")
    }

    /// The review intent that carries `nonce`, if any: the verdict gate's lookup.
    pub fn by_nonce(&self, nonce: &Nonce) -> Option<&IntentRow> {
        todo!("tick/ledger")
    }

    /// True once a Verdict row cites the intent that carries `nonce`: a second verdict with it is
    /// a replay.
    pub fn nonce_consumed(&self, nonce: &Nonce) -> bool {
        todo!("tick/ledger")
    }

    /// The latest receipt for an intent.
    pub fn receipt(&self, id: &IntentId) -> Option<&Receipt> {
        todo!("tick/ledger")
    }

    /// Bound verdicts for a PR at a head, oldest first.
    pub fn verdicts_at(&self, repo: &str, pr: u32, head: &str) -> Vec<&VerdictRow> {
        todo!("tick/ledger")
    }
}

/// Relative to the campaign folder. `.cm/.gitignore`'s `lock` line keeps it out of git.
pub const TICK_LOCK: &str = ".cm/tick/lock";

/// S-tick-05: the one lock every writer of `.cm/tick/` holds: `cm tick`, `cm effects record` and
/// `cm effects resolve`. Taken without waiting (`flock` `LOCK_EX | LOCK_NB`): a second holder is
/// refused at once, never queued. It is separate from `store::lock`, which the existing cm
/// commands the tick calls take and release themselves; the tick never holds `store::lock` across
/// such a call or a child process, since `astra-review.sh collect` runs a second `cm`.
#[derive(Debug)]
pub struct TickLock {
    _file: std::fs::File,
}

impl TickLock {
    /// `None` when another process holds it.
    pub fn try_acquire(root: &Path) -> crate::tick::Result<Option<TickLock>> {
        todo!("tick/ledger")
    }
}

/// What an open intent's lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Done(Bind),
    /// Known not to have happened: the same intent is emitted again, never a new one.
    NotDone,
    /// The lookups answered and still cannot tell: park with a packet item.
    Undecidable {
        reason: String,
    },
    /// A lookup failed (forge down, timeout): leave the intent open and look again next tick.
    Unread {
        reason: String,
    },
}

/// The lookups [`reconcile`] uses. Read-only.
pub trait Observer {
    fn observe(&self, row: &IntentRow) -> Observation;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub bound: Vec<IntentId>,
    pub parked: Vec<IntentId>,
    /// Known not done; the caller re-emits each under its id, counting tries (S-tick-34).
    pub retry: Vec<IntentId>,
    pub unread: Vec<IntentId>,
}

/// Binds, parks or marks for retry every open intent, appending one row per decided intent.
/// Intents with a pending receipt from this tick's own effects are not observed.
pub fn reconcile(
    host: &dyn Host,
    ledger: &mut EffectLedger,
    observer: &dyn Observer,
) -> crate::tick::Result<Reconciled> {
    todo!("tick/ledger")
}

/// S-tick-17: the system observer. Skeleton glue: it routes each intent to the module whose unit
/// owns that kind of lookup, so no unit edits this match.
pub struct SystemObserver<'a> {
    pub root: &'a Path,
    pub deps: &'a Deps<'a>,
}

impl Observer for SystemObserver<'_> {
    fn observe(&self, row: &IntentRow) -> Observation {
        use crate::tick::{dispatch, lan, merge, release};
        match (row.kind, row.capability) {
            (IntentKind::StepLaunch, _) => dispatch::observe_step(self.root, row),
            (IntentKind::ReviewLaunch, _) => dispatch::observe_review(self.root, self.deps, row),
            (
                IntentKind::Effect,
                Some(Capability::GhPullRequestMerge | Capability::GhPullRequestEnqueue),
            ) => merge::observe(self.deps, row),
            (IntentKind::Effect, Some(Capability::GhWorkflowRun)) => {
                release::observe(self.deps, row)
            }
            (IntentKind::Effect, Some(_)) => lan::observe(self.root, self.deps, row),
            (IntentKind::Effect, None) => Observation::Undecidable {
                reason: "an effect intent names no capability".to_owned(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_killed_append_leaves_no_partial_row_and_the_torn_line_is_dropped_on_open() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_reopened_ledger_reads_every_row_it_wrote_in_order_with_the_same_states() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn open_intents_never_exceed_sixty_four_and_a_target_holds_one_open_intent_per_purpose() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_restart_between_intent_and_effect_binds_retries_or_parks_and_never_launches_twice() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_minted_nonce_is_unique_in_the_ledger_and_consumed_once_its_verdict_row_is_written() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn an_unread_lookup_leaves_the_intent_open_and_parks_nothing() {
        todo!()
    }
}
