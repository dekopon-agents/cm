//! The effect ledger (`tick/ledger`; IDEA-0063 §Recovery; PLAN.md `07-tick/ledger`).
//!
//! Before each external effect cm appends an intent row to the ledger in the epoch folder and
//! pushes it to campaign-trail main (11-D8): a runner effect, a review launch or a step launch. On
//! restart [`reconcile`] asks the [`Observer`] whether each open intent happened (the reviewer
//! session or comment, the exact-head merge guard, the PR and tag identity): a match is bound, a
//! lookup that cannot decide parks the intent with a packet item, and a known-not-done intent goes
//! back to its owning stage, which re-admits it under current authority (R05, [`readmit`]). Nothing
//! ever launches twice.
//!
//! **Files.** JSON Lines in segments, `.cm/tick/effects-NNNN.jsonl`. A writer appends to the
//! highest segment and starts the next one once a segment reaches [`SEGMENT_MAX_BYTES`]; one row
//! is at most about 9 KiB, so no segment reaches campaign-trail's 1 MB file ceiling (R19). Readers
//! read every segment in order. Rows are never rewritten.
//!
//! **Two clones (R03).** The tick's clone is the only intender; the coordinator agent's clone
//! (11-D22) appends `ruled` rows. campaign-trail's `.gitattributes` carries
//! `store/epochs/*/.cm/tick/effects-*.jsonl merge=union` (the coordinator commits it), so two
//! clones' appends merge without conflict; `push::begin` refuses to run without it. Row order
//! across clones does not matter: every row names its intent, and [`IntentState`] is a fold.
//!
//! **Opening (R08).** [`EffectLedger::snapshot`] is the read-only view any process may take, with
//! or without a lock, e.g. the proof readers of `cm advance` and `cm sync`: it ignores a torn last
//! line in memory and never writes. [`EffectLedger::open`] needs the [`TickLock`]: it truncates a
//! torn last line (a killed append; safe, since no effect follows an unpushed row) and is the only
//! handle that appends.
//!
//! **Fold (U06).** Rows fold in file order per intent:
//! - `intent` opens it; `emitted` records each emission with the cycle that emitted it (R06).
//! - A receipt `ok` or `refused`, a `bound` row, a `verdict` row, and a `done` or `drop` ruling
//!   close it. A receipt `failed` and a `retry` ruling reopen it.
//! - A late receipt for a parked or handed intent closes it. A receipt for a closed intent is a
//!   duplicate when identical and refused when different. A receipt whose capability is not the
//!   intent's is refused.
//! - `bound via: action` makes it handed. A handed intent that is still not done when its
//!   `action-deadline` timer fires is parked, and its reminder is armed (R18).
//!
//! **Budget (R19).** An intent is emitted at most [`MAX_TRIES`](crate::tick::timers::MAX_TRIES)
//! times between rulings and takes at most [`MAX_RULINGS_PER_INTENT`] rulings, so it holds at most
//! [`MAX_ROWS_PER_INTENT`] rows. After the last ruling only `done` or `drop` is accepted.
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
use crate::tick::run::{Cycle, Deps};
use crate::tick::wire::{Capability, EffectInput, Receipt};

/// Relative to the campaign (epoch) folder. Segments are `effects-0001.jsonl` and on. The
/// top-level `.gitignore` of campaign-trail ignores `*.jsonl`, so the tick force-adds them
/// (S-tick-20).
pub const LEDGER_DIR: &str = ".cm/tick";
pub const SEGMENT_PREFIX: &str = "effects-";
/// A writer starts the next segment once the current one reaches this size.
pub const SEGMENT_MAX_BYTES: u64 = 512 * 1024;
/// Growth bound: open intents across the epoch. Past it the planning stages write no intent and
/// put one `ledger-full` packet item instead. The cap gates new intents only: `emitted`, `bound`,
/// `parked`, `receipt`, `ruled` and `verdict` rows are never refused by it, so reconciliation and
/// completion always proceed (R19).
pub const MAX_OPEN_INTENTS: usize = 64;
/// After this many rulings an intent accepts only `done` or `drop`.
pub const MAX_RULINGS_PER_INTENT: usize = 2;
/// One intent row, then per ruling round: `MAX_TRIES` emissions with a receipt each, a bind and a
/// park, then the ruling; then one closing row.
pub const MAX_ROWS_PER_INTENT: usize =
    1 + (MAX_RULINGS_PER_INTENT + 1) * (2 * crate::tick::timers::MAX_TRIES as usize + 2) + 1;
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
    /// The tick number that wrote it (`Cursor::tick` + 1).
    pub cycle: u64,
    /// `<NN-name>/<unit>/<step>`.
    pub target: String,
    pub kind: IntentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    /// The effect's input as first emitted; a retry is re-admitted from current state, and the
    /// owner may emit this same input again only when its checks still pass (R05).
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
    /// For a review launch, the `@dekopon review` comment it answers (R04).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<u64>,
    /// The intent whose recorded receipt this one was built from (a pin merge after its update,
    /// the LAN leg after a pin merge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<IntentId>,
}

/// The fields a caller supplies; the ledger assigns `id`, `at` and `cycle`.
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
    pub requested_by: Option<u64>,
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
    /// A packet action the coordinator performs; its receipt comes by `cm effects record`. Makes
    /// the intent handed, not closed.
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
/// only within `[authority]` at the approved SHA: a `retry` goes through [`readmit`] like any other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "kebab-case")]
pub enum Ruling {
    /// It happened; `evidence` is a campaign file that shows it.
    Done { evidence: String },
    /// It did not happen: re-admit it under the same id.
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
    /// One emission: printed for the runner, applied locally, or launched.
    Emitted {
        intent: IntentId,
        at: String,
        cycle: u64,
    },
    Bound {
        intent: IntentId,
        at: String,
        bind: Bind,
    },
    /// Undecidable, or handed and overdue; the intent waits for a ruling through the packet.
    Parked {
        intent: IntentId,
        at: String,
        reason: String,
    },
    /// The runner's receipt (11-D3 (3)), or the local applier's in the interim. `cycle` is the
    /// cursor's tick when it was recorded, the order readiness freshness uses (R11).
    Receipt {
        at: String,
        cycle: u64,
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

/// An intent's state: the fold of its rows (module doc, Fold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    /// No closing row yet, or a `failed` receipt, or a `retry` ruling.
    Open,
    /// Bound `via: action`: handed to the coordinator as a packet action. Reconcile looks for it
    /// done but never re-emits it; past its deadline it is parked.
    Handed,
    Bound,
    Parked,
    /// A receipt `ok` or `refused`.
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

/// The loaded ledger: every segment's rows in order.
#[derive(Debug)]
#[expect(dead_code, reason = "skeleton: tick/ledger reads these fields")]
pub struct EffectLedger {
    dir: PathBuf,
    rows: Vec<LedgerRow>,
    /// `true` only for [`EffectLedger::open`]: the handle that may append.
    writable: bool,
}

impl EffectLedger {
    /// The read-only view: every segment, a torn last line ignored in memory, nothing written.
    pub fn snapshot(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/ledger")
    }

    /// The exclusive handle: a torn last line is truncated; only this handle appends. Any other
    /// unreadable line is [`crate::tick::TickError::Row`].
    pub fn open(root: &Path, lock: &TickLock) -> crate::tick::Result<Self> {
        todo!("tick/ledger")
    }

    pub fn rows(&self) -> &[LedgerRow] {
        todo!("tick/ledger")
    }

    /// The segment appends go to, relative to the campaign folder: the tick force-adds it.
    pub fn active_segment(&self) -> PathBuf {
        todo!("tick/ledger")
    }

    pub fn state(&self, id: &IntentId) -> Option<IntentState> {
        todo!("tick/ledger")
    }

    /// Intents in [`IntentState::Open`] or [`IntentState::Handed`], oldest first.
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

    /// The cycle of an intent's latest emission.
    pub fn last_emitted(&self, id: &IntentId) -> Option<u64> {
        todo!("tick/ledger")
    }

    /// Appends one intent row as one `write` of one whole line, then fsyncs. Refuses past
    /// [`MAX_OPEN_INTENTS`] with [`crate::tick::TickError::LedgerFull`]; answers
    /// [`Intended::AlreadyOpen`] when the same target already holds an open, handed or parked
    /// intent of the same kind, capability and head.
    pub fn intend(
        &mut self,
        host: &dyn Host,
        cycle: u64,
        draft: Draft,
    ) -> crate::tick::Result<Intended> {
        todo!("tick/ledger")
    }

    pub fn emitted(
        &mut self,
        host: &dyn Host,
        id: &IntentId,
        cycle: u64,
    ) -> crate::tick::Result<()> {
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

    /// The fold's receipt rule: [`Recorded::Duplicate`] for the same receipt again;
    /// [`crate::tick::TickError::ReceiptConflict`] for a different receipt on a closed intent or a
    /// capability that is not the intent's; [`crate::tick::TickError::UnknownIntent`] for an
    /// intent not in the ledger.
    pub fn record(
        &mut self,
        host: &dyn Host,
        cycle: u64,
        receipt: Receipt,
    ) -> crate::tick::Result<Recorded> {
        todo!("tick/ledger")
    }

    /// Refuses a ruling on an intent that is not parked, and a `retry` past
    /// [`MAX_RULINGS_PER_INTENT`].
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

    /// Receipt rows from position `from` on (counted over receipt rows only), with their cycle:
    /// the `effect-refused` items' source (R17).
    pub fn receipts_from(&self, from: usize) -> Vec<(u64, &Receipt)> {
        todo!("tick/ledger")
    }

    /// Bound verdicts for a PR at a head, oldest first.
    pub fn verdicts_at(&self, repo: &str, pr: u32, head: &str) -> Vec<&VerdictRow> {
        todo!("tick/ledger")
    }
}

/// Relative to the campaign folder. `.cm/.gitignore`'s `lock` line keeps it out of git.
pub const TICK_LOCK: &str = ".cm/tick/lock";

/// S-tick-05: the one lock every writer of `.cm/tick/` in one clone holds: `cm tick`,
/// `cm effects record` and `cm effects resolve`. Taken without waiting (`flock` `LOCK_EX |
/// LOCK_NB`): a second holder is refused at once, never queued. It is separate from
/// `store::lock`, which the existing cm commands the tick calls take and release themselves; the
/// tick never holds `store::lock` across such a call or a child process, since
/// `astra-review.sh collect` runs a second `cm`. It does not span the runner's apply: whole-cycle
/// serialization is the runner's (R06, `wire` module doc).
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
    /// Known not to have happened: the intent goes to [`readmit`], never a new intent.
    NotDone,
    /// The lookups answered and still cannot tell: park with a packet item. A non-idempotent
    /// effect (`gh.workflow.run`, `gh.pin.update`) emitted in an earlier cycle with no receipt and
    /// no positive evidence is always this, never `NotDone` (R06, R10).
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
    /// Known not done; each goes to [`readmit`] (S-tick-34 counts the tries).
    pub retry: Vec<IntentId>,
    pub unread: Vec<IntentId>,
}

/// Binds, parks or marks for retry every open intent, appending one row per decided intent.
/// Intents emitted by this tick are not observed.
pub fn reconcile(
    host: &dyn Host,
    ledger: &mut EffectLedger,
    observer: &dyn Observer,
) -> crate::tick::Result<Reconciled> {
    todo!("tick/ledger")
}

/// What an intent's owning stage says about emitting it again (R05).
#[derive(Debug, Clone, PartialEq)]
pub enum Readmit {
    /// Emit under the same id with this input, checked now against current authority, gates and
    /// input validity; it still passes `wire::admit`.
    Emit(EffectInput),
    /// Launch again under the same id (a step or review launch).
    Relaunch,
    /// The owner refuses now: park with this reason.
    Refuse { reason: String },
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

/// S-tick-25: retry admission. Skeleton glue: it routes a retry to the stage that owns the
/// intent's kind, which re-checks `paused`, `[authority]` at the approved SHA, its gates and its
/// input before anything is emitted. Stored ledger JSON never authorizes an act on its own.
pub fn readmit(cycle: &Cycle, row: &IntentRow) -> Readmit {
    use crate::tick::{dispatch, lan, merge, release};
    match (row.kind, row.capability) {
        (IntentKind::StepLaunch | IntentKind::ReviewLaunch, _) => dispatch::readmit(cycle, row),
        (
            IntentKind::Effect,
            Some(Capability::GhPullRequestMerge | Capability::GhPullRequestEnqueue),
        ) => merge::readmit(cycle, row),
        (IntentKind::Effect, Some(Capability::GhWorkflowRun)) => release::readmit(cycle, row),
        (IntentKind::Effect, Some(_)) => lan::readmit(cycle, row),
        (IntentKind::Effect, None) => Readmit::Refuse {
            reason: "an effect intent names no capability".to_owned(),
        },
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

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn two_clones_appending_rows_merge_by_union_and_every_row_survives() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_snapshot_ignores_a_torn_line_and_never_writes() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn an_intent_stays_within_its_row_budget_and_no_segment_reaches_one_mib() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn a_late_receipt_after_park_closes_the_intent_and_a_conflicting_one_is_refused() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn an_earlier_cycles_unreceipted_dispatch_parks_and_is_never_reemitted_by_absence() {
        todo!()
    }
}
