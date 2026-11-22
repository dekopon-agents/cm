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

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::host::Host;
use crate::state::Lane;
use crate::tick::dispatch::LaunchRoute;
use crate::tick::run::{Cycle, Deps};
use crate::tick::wire::{Capability, EffectInput, Receipt, ReceiptOutcome};

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
        match lane {
            Lane::Codex => Some(Family::Gpt),
            Lane::ClaudeCloud | Lane::Fable => Some(Family::Claude),
            Lane::Owner | Lane::Coordinator | Lane::Pi => None,
        }
    }

    pub fn other(self) -> Family {
        match self {
            Family::Claude => Family::Gpt,
            Family::Gpt => Family::Claude,
        }
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
pub struct EffectLedger {
    dir: PathBuf,
    rows: Vec<LedgerRow>,
    /// `true` only for [`EffectLedger::open`]: the handle that may append.
    writable: bool,
    /// How many rows were on disk when this handle loaded: rows past it are this handle's own,
    /// which [`reconcile`] does not observe.
    loaded: usize,
}

impl EffectLedger {
    /// The read-only view: every segment, a torn last line ignored in memory, nothing written.
    pub fn snapshot(root: &Path) -> crate::tick::Result<Self> {
        let dir = root.join(LEDGER_DIR);
        let rows = load(&dir, false)?;
        Ok(EffectLedger {
            dir,
            loaded: rows.len(),
            rows,
            writable: false,
        })
    }

    /// The exclusive handle: a torn last line is truncated; only this handle appends. Any other
    /// unreadable line is [`crate::tick::TickError::Row`].
    pub fn open(root: &Path, lock: &TickLock) -> crate::tick::Result<Self> {
        let _ = lock;
        let dir = root.join(LEDGER_DIR);
        let rows = load(&dir, true)?;
        Ok(EffectLedger {
            dir,
            loaded: rows.len(),
            rows,
            writable: true,
        })
    }

    pub fn rows(&self) -> &[LedgerRow] {
        &self.rows
    }

    /// The segment appends go to, relative to the campaign folder: the tick force-adds it.
    pub fn active_segment(&self) -> PathBuf {
        Path::new(LEDGER_DIR).join(segment_name(active_number(&self.dir)))
    }

    pub fn state(&self, id: &IntentId) -> Option<IntentState> {
        let intent = self.intent(id)?;
        let mut state = IntentState::Open;
        for row in &self.rows {
            state = match row {
                LedgerRow::Bound { intent, bind, .. } if intent == id && !closed(state) => {
                    if matches!(bind, Bind::Action { .. }) {
                        IntentState::Handed
                    } else {
                        IntentState::Bound
                    }
                }
                LedgerRow::Parked { intent, .. } if intent == id && !closed(state) => {
                    IntentState::Parked
                }
                LedgerRow::Receipt { receipt, .. }
                    if receipt.intent == *id
                        && !closed(state)
                        && Some(receipt.capability) == intent.capability =>
                {
                    match receipt.outcome {
                        ReceiptOutcome::Ok | ReceiptOutcome::Refused => IntentState::Received,
                        ReceiptOutcome::Failed => IntentState::Open,
                    }
                }
                LedgerRow::Ruled { intent, ruling, .. } if intent == id && !closed(state) => {
                    match ruling {
                        Ruling::Retry if state == IntentState::Parked => IntentState::Open,
                        Ruling::Retry => state,
                        Ruling::Done { .. } | Ruling::Drop { .. } => IntentState::Closed,
                    }
                }
                LedgerRow::Verdict { verdict, .. } if verdict.intent == *id => IntentState::Closed,
                _ => state,
            };
        }
        Some(state)
    }

    /// Intents in [`IntentState::Open`] or [`IntentState::Handed`], oldest first.
    pub fn open_intents(&self) -> Vec<&IntentRow> {
        self.intents_in(&[IntentState::Open, IntentState::Handed])
    }

    /// Intents in [`IntentState::Parked`], oldest first.
    pub fn parked(&self) -> Vec<&IntentRow> {
        self.intents_in(&[IntentState::Parked])
    }

    pub fn intent(&self, id: &IntentId) -> Option<&IntentRow> {
        self.intents().find(|row| row.id == *id)
    }

    /// The cycle of an intent's latest emission.
    pub fn last_emitted(&self, id: &IntentId) -> Option<u64> {
        self.rows.iter().rev().find_map(|row| match row {
            LedgerRow::Emitted { intent, cycle, .. } if intent == id => Some(*cycle),
            _ => None,
        })
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
        let live = self.intents_in(&[IntentState::Open, IntentState::Handed, IntentState::Parked]);
        if let Some(same) = live.iter().find(|row| {
            row.target == draft.target
                && row.kind == draft.kind
                && row.capability == draft.capability
                && row.head == draft.head
        }) {
            return Ok(Intended::AlreadyOpen(same.id.clone()));
        }
        let full = crate::tick::TickError::LedgerFull {
            open: live.len(),
            max: MAX_OPEN_INTENTS,
        };
        if live.len() >= MAX_OPEN_INTENTS {
            return Err(full);
        }
        let at = crate::host::stamp(host.now());
        let minute: String = at.chars().filter(char::is_ascii_digit).collect();
        let prefix = format!("i-{}T{}-", &minute[..8], &minute[8..]);
        let next = self
            .intents()
            .filter_map(|row| row.id.0.strip_prefix(&prefix)?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        // Ten thousand intents in one minute cannot come from bounded ticks; refuse rather than
        // reuse an id.
        if next > 9999 {
            return Err(full);
        }
        let id = IntentId(format!("{prefix}{next:04}"));
        // A review's nonce is derived from its id, so a review draft that carries none gets it
        // here, on the row the launch's prompt is built from.
        let minted = (draft.kind == IntentKind::ReviewLaunch && draft.nonce.is_none())
            .then(|| self.mint_nonce(&id));
        let Draft {
            target,
            kind,
            capability,
            input,
            repo,
            pr,
            head,
            nonce,
            model,
            effort,
            family,
            route,
            attempt,
            requested_by,
            follows,
        } = draft;
        self.append(LedgerRow::Intent(IntentRow {
            id: id.clone(),
            at,
            cycle,
            target,
            kind,
            capability,
            input,
            repo,
            pr,
            head,
            nonce: nonce.or(minted),
            model,
            effort,
            family,
            route,
            attempt,
            requested_by,
            follows,
        }))?;
        Ok(Intended::New(id))
    }

    pub fn emitted(
        &mut self,
        host: &dyn Host,
        id: &IntentId,
        cycle: u64,
    ) -> crate::tick::Result<()> {
        self.known(id)?;
        self.append(LedgerRow::Emitted {
            intent: id.clone(),
            at: crate::host::stamp(host.now()),
            cycle,
        })
    }

    pub fn bind(&mut self, host: &dyn Host, id: &IntentId, bind: Bind) -> crate::tick::Result<()> {
        self.known(id)?;
        self.append(LedgerRow::Bound {
            intent: id.clone(),
            at: crate::host::stamp(host.now()),
            bind,
        })
    }

    pub fn park(
        &mut self,
        host: &dyn Host,
        id: &IntentId,
        reason: &str,
    ) -> crate::tick::Result<()> {
        self.known(id)?;
        self.append(LedgerRow::Parked {
            intent: id.clone(),
            at: crate::host::stamp(host.now()),
            reason: reason.to_owned(),
        })
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
        match self.check_receipt(&receipt)? {
            Recorded::Duplicate => Ok(Recorded::Duplicate),
            Recorded::New => {
                self.append(LedgerRow::Receipt {
                    at: crate::host::stamp(host.now()),
                    cycle,
                    receipt,
                })?;
                Ok(Recorded::New)
            }
        }
    }

    /// Refuses a ruling on an intent that is not parked, and a `retry` past
    /// [`MAX_RULINGS_PER_INTENT`].
    pub fn rule(
        &mut self,
        host: &dyn Host,
        id: &IntentId,
        ruling: Ruling,
    ) -> crate::tick::Result<()> {
        if let Some(reason) = self.ruling_refusal(id, &ruling)? {
            return Err(crate::tick::TickError::Io {
                path: self.dir.join(segment_name(active_number(&self.dir))),
                source: io::Error::other(reason),
            });
        }
        self.append(LedgerRow::Ruled {
            intent: id.clone(),
            at: crate::host::stamp(host.now()),
            ruling,
        })
    }

    /// Appends a bound verdict. The gate has already refused a consumed nonce (S-tick-51).
    pub fn verdict(&mut self, host: &dyn Host, verdict: VerdictRow) -> crate::tick::Result<()> {
        self.known(&verdict.intent)?;
        self.append(LedgerRow::Verdict {
            at: crate::host::stamp(host.now()),
            verdict,
        })
    }

    /// The nonce a review intent with this id carries: 32 hex digits of SHA-256 over the ledger's
    /// path inside the checkout and the id.
    pub fn mint_nonce(&self, id: &IntentId) -> Nonce {
        let inside = self
            .dir
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .and_then(|top| self.dir.strip_prefix(top).ok())
            .unwrap_or(&self.dir);
        let path = inside
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let digest = Sha256::digest(format!("{path}\n{id}").as_bytes());
        Nonce(
            digest[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// The review intent that carries `nonce`, if any: the verdict gate's lookup.
    pub fn by_nonce(&self, nonce: &Nonce) -> Option<&IntentRow> {
        self.intents()
            .find(|row| row.kind == IntentKind::ReviewLaunch && row.nonce.as_ref() == Some(nonce))
    }

    /// True once a Verdict row cites the intent that carries `nonce`: a second verdict with it is
    /// a replay.
    pub fn nonce_consumed(&self, nonce: &Nonce) -> bool {
        let Some(intent) = self.by_nonce(nonce) else {
            return false;
        };
        self.rows.iter().any(
            |row| matches!(row, LedgerRow::Verdict { verdict, .. } if verdict.intent == intent.id),
        )
    }

    /// The latest receipt for an intent.
    pub fn receipt(&self, id: &IntentId) -> Option<&Receipt> {
        self.rows.iter().rev().find_map(|row| match row {
            LedgerRow::Receipt { receipt, .. } if receipt.intent == *id => Some(receipt),
            _ => None,
        })
    }

    /// Receipt rows from position `from` on (counted over receipt rows only), with their cycle:
    /// the `effect-refused` items' source (R17).
    pub fn receipts_from(&self, from: usize) -> Vec<(u64, &Receipt)> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                LedgerRow::Receipt { cycle, receipt, .. } => Some((*cycle, receipt)),
                _ => None,
            })
            .skip(from)
            .collect()
    }

    /// Bound verdicts for a PR at a head, oldest first.
    pub fn verdicts_at(&self, repo: &str, pr: u32, head: &str) -> Vec<&VerdictRow> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                LedgerRow::Verdict { verdict, .. } if verdict.head == head => Some(verdict),
                _ => None,
            })
            .filter(|verdict| {
                self.intent(&verdict.intent)
                    .is_some_and(|row| row.repo.as_deref() == Some(repo) && row.pr == Some(pr))
            })
            .collect()
    }

    fn intents(&self) -> impl Iterator<Item = &IntentRow> {
        self.rows.iter().filter_map(|row| match row {
            LedgerRow::Intent(intent) => Some(intent),
            _ => None,
        })
    }

    fn intents_in(&self, states: &[IntentState]) -> Vec<&IntentRow> {
        self.intents()
            .filter(|row| {
                self.state(&row.id)
                    .is_some_and(|state| states.contains(&state))
            })
            .collect()
    }

    fn known(&self, id: &IntentId) -> crate::tick::Result<&IntentRow> {
        self.intent(id)
            .ok_or_else(|| crate::tick::TickError::UnknownIntent(id.clone()))
    }

    /// The fold's receipt rule without writing: `New` when the receipt would be appended.
    pub(crate) fn check_receipt(&self, receipt: &Receipt) -> crate::tick::Result<Recorded> {
        let intent = self.known(&receipt.intent)?;
        let conflict = || crate::tick::TickError::ReceiptConflict(receipt.intent.clone());
        if intent.capability != Some(receipt.capability) {
            return Err(conflict());
        }
        let latest = self.rows.iter().rposition(
            |row| matches!(row, LedgerRow::Receipt { receipt: seen, .. } if seen.intent == receipt.intent),
        );
        let same = latest.filter(|at| {
            matches!(&self.rows[*at], LedgerRow::Receipt { receipt: seen, .. } if seen == receipt)
        });
        // The same receipt again with no emission since it: the runner resent a batch.
        let reemitted = |at: usize| {
            self.rows[at..].iter().any(
                |row| matches!(row, LedgerRow::Emitted { intent, .. } if *intent == receipt.intent),
            )
        };
        if let Some(at) = same
            && !reemitted(at)
        {
            return Ok(Recorded::Duplicate);
        }
        let state = self.state(&receipt.intent).unwrap_or(IntentState::Open);
        if closed(state) {
            return if same.is_some() {
                Ok(Recorded::Duplicate)
            } else {
                Err(conflict())
            };
        }
        Ok(Recorded::New)
    }

    /// Why a ruling would be refused, or `None` when it may be written.
    pub(crate) fn ruling_refusal(
        &self,
        id: &IntentId,
        ruling: &Ruling,
    ) -> crate::tick::Result<Option<String>> {
        self.known(id)?;
        if self.state(id) != Some(IntentState::Parked) {
            return Ok(Some(format!("intent {id} is not parked")));
        }
        let rulings = self
            .rows
            .iter()
            .filter(|row| matches!(row, LedgerRow::Ruled { intent, .. } if intent == id))
            .count();
        if *ruling == Ruling::Retry && rulings >= MAX_RULINGS_PER_INTENT {
            return Ok(Some(format!(
                "intent {id} has had {rulings} rulings; only done or drop is accepted now"
            )));
        }
        Ok(None)
    }

    /// One row as one `write` of one whole line, then fsync.
    fn append(&mut self, row: LedgerRow) -> crate::tick::Result<()> {
        let number = active_number(&self.dir);
        let path = self.dir.join(segment_name(number));
        let io = |source| crate::tick::TickError::Io {
            path: path.clone(),
            source,
        };
        if !self.writable {
            return Err(io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a ledger snapshot never writes",
            )));
        }
        let mut line = serde_json::to_vec(&row).map_err(|source| crate::tick::TickError::Row {
            path: path.clone(),
            line: 0,
            source,
        })?;
        line.push(b'\n');
        fs::create_dir_all(&self.dir).map_err(io)?;
        let created = !path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io)?;
        let written = file.write(&line).map_err(io)?;
        if written != line.len() {
            return Err(io(io::Error::new(
                io::ErrorKind::WriteZero,
                "short write of a ledger row",
            )));
        }
        file.sync_all().map_err(io)?;
        if created {
            File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(io)?;
        }
        self.rows.push(row);
        Ok(())
    }
}

/// `Bound`, `Received` and `Closed` take no further receipt, bind, park or ruling.
fn closed(state: IntentState) -> bool {
    matches!(
        state,
        IntentState::Bound | IntentState::Received | IntentState::Closed
    )
}

fn segment_name(number: u32) -> String {
    format!("{SEGMENT_PREFIX}{number:04}.jsonl")
}

/// The segments in `dir`, by number.
fn segments(dir: &Path) -> crate::tick::Result<Vec<(u32, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(crate::tick::TickError::Io {
                path: dir.to_owned(),
                source,
            });
        }
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| crate::tick::TickError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let name = entry.file_name();
        let Some(number) = name
            .to_str()
            .and_then(|name| name.strip_prefix(SEGMENT_PREFIX))
            .and_then(|rest| rest.strip_suffix(".jsonl"))
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u32>().ok())
        else {
            continue;
        };
        found.push((number, entry.path()));
    }
    found.sort();
    Ok(found)
}

/// The highest segment while it is under [`SEGMENT_MAX_BYTES`], else the next one.
fn active_number(dir: &Path) -> u32 {
    let last = segments(dir).ok().and_then(|found| found.last().cloned());
    match last {
        None => 1,
        Some((number, path)) => {
            let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            if size >= SEGMENT_MAX_BYTES {
                number + 1
            } else {
                number
            }
        }
    }
}

/// Every segment's rows in order. A segment that does not end in a newline ends in a torn append:
/// dropped in memory, and with `truncate` cut from the file.
fn load(dir: &Path, truncate: bool) -> crate::tick::Result<Vec<LedgerRow>> {
    let mut rows = Vec::new();
    for (_, path) in segments(dir)? {
        let io = |source| crate::tick::TickError::Io {
            path: path.clone(),
            source,
        };
        let bytes = fs::read(&path).map_err(io)?;
        let whole = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
        if whole < bytes.len() && truncate {
            let file = OpenOptions::new().write(true).open(&path).map_err(io)?;
            file.set_len(whole as u64).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        for (index, line) in bytes[..whole].split(|byte| *byte == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let row =
                serde_json::from_slice(line).map_err(|source| crate::tick::TickError::Row {
                    path: path.clone(),
                    line: index + 1,
                    source,
                })?;
            rows.push(row);
        }
    }
    Ok(rows)
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
        let path = root.join(TICK_LOCK);
        let io = |source| crate::tick::TickError::Io {
            path: path.clone(),
            source,
        };
        let cm = root.join(crate::store::CM_DIR);
        fs::create_dir_all(root.join(LEDGER_DIR)).map_err(io)?;
        // The same ignore file `store::lock` writes, so the lock never reaches a commit.
        crate::store::create_new(
            &cm.join(".gitignore"),
            format!(
                "{}\n{}\n",
                crate::store::LOCK,
                crate::store::LEGACY_JOURNAL_COPY
            )
            .as_bytes(),
        )
        .map_err(io)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(TickLock { _file: file })),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(errno) => Err(io(errno.into())),
        }
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
    let mine: std::collections::BTreeSet<IntentId> = ledger.rows[ledger.loaded..]
        .iter()
        .filter_map(|row| match row {
            LedgerRow::Intent(intent) => Some(intent.id.clone()),
            LedgerRow::Emitted { intent, .. } => Some(intent.clone()),
            _ => None,
        })
        .collect();
    let candidates: Vec<(IntentRow, IntentState)> = ledger
        .open_intents()
        .into_iter()
        .filter(|row| !mine.contains(&row.id))
        .map(|row| {
            (
                row.clone(),
                ledger.state(&row.id).unwrap_or(IntentState::Open),
            )
        })
        .collect();
    let mut out = Reconciled::default();
    for (row, state) in candidates {
        let mut seen = observer.observe(&row);
        // R06: absence never reads as not done for a write that cannot be repeated safely once
        // an earlier cycle emitted it.
        if seen == Observation::NotDone
            && unrepeatable(&row)
            && ledger.last_emitted(&row.id).is_some()
        {
            seen = Observation::Undecidable {
                reason: "a non-idempotent effect was emitted in an earlier cycle with no \
                         receipt and no evidence that it happened"
                    .to_owned(),
            };
        }
        match seen {
            Observation::Done(bind) => {
                ledger.bind(host, &row.id, bind)?;
                out.bound.push(row.id);
            }
            Observation::Unread { .. } => out.unread.push(row.id),
            // A handed intent is the coordinator's to perform: looked up for done, never
            // re-emitted or parked here; its action deadline parks it (R18).
            _ if state == IntentState::Handed => {}
            Observation::NotDone => out.retry.push(row.id),
            Observation::Undecidable { reason } => {
                ledger.park(host, &row.id, &reason)?;
                out.parked.push(row.id);
            }
        }
    }
    Ok(out)
}

/// The non-idempotent effects: a second emission is a second write (R06, R10).
fn unrepeatable(row: &IntentRow) -> bool {
    matches!(
        row.capability,
        Some(Capability::GhWorkflowRun | Capability::GhPinUpdate)
    )
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

/// The scratch git the `tick/ledger` tests share with `tests/tick_ledger.rs`.
#[cfg(test)]
#[path = "../../tests/fixtures/tick-ledger/scratch.rs"]
pub(crate) mod scratch;

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::tick::TickError;
    use crate::tick::wire::ReceiptOutput;

    pub(crate) use super::scratch;

    pub(crate) struct Clock(pub jiff::Timestamp);

    impl Host for Clock {
        fn now(&self) -> jiff::Timestamp {
            self.0
        }

        fn free_bytes(&self, _: &Path) -> std::io::Result<u64> {
            Ok(100 << 30)
        }

        fn heavy_builds(&self) -> std::io::Result<u32> {
            Ok(0)
        }
    }

    /// The tick lock, retried briefly: in a multi-threaded test binary a sibling test's forked
    /// git child can hold a just-closed lock descriptor until it execs.
    pub(crate) fn tick_lock(root: &Path) -> TickLock {
        for _ in 0..200 {
            if let Some(lock) = TickLock::try_acquire(root).unwrap() {
                return lock;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the tick lock stayed held");
    }

    pub(crate) fn clock() -> Clock {
        Clock("2026-11-22T18:00:00Z".parse().unwrap())
    }

    pub(crate) fn draft(target: &str, kind: IntentKind, capability: Option<Capability>) -> Draft {
        Draft {
            target: target.to_owned(),
            kind,
            capability,
            input: None,
            repo: Some("dekopon-agents/cm".to_owned()),
            pr: Some(7),
            head: Some("a".repeat(40)),
            nonce: None,
            model: None,
            effort: None,
            family: None,
            route: None,
            attempt: None,
            requested_by: None,
            follows: None,
        }
    }

    pub(crate) fn effect(target: &str, capability: Capability) -> Draft {
        draft(target, IntentKind::Effect, Some(capability))
    }

    pub(crate) fn receipt(
        id: &IntentId,
        capability: Capability,
        outcome: ReceiptOutcome,
    ) -> Receipt {
        Receipt {
            intent: id.clone(),
            capability,
            outcome,
            output: ReceiptOutput::Inline(format!("{outcome:?}")),
        }
    }

    pub(crate) fn new_id(intended: Intended) -> IntentId {
        match intended {
            Intended::New(id) => id,
            Intended::AlreadyOpen(id) => panic!("{id} was already open"),
        }
    }

    fn segment(root: &Path) -> std::path::PathBuf {
        root.join(LEDGER_DIR).join("effects-0001.jsonl")
    }

    /// Answers by intent id; counts every lookup.
    struct Fixed {
        answers: BTreeMap<IntentId, Observation>,
        asked: RefCell<Vec<IntentId>>,
    }

    impl Fixed {
        fn new(answers: &[(&IntentId, Observation)]) -> Fixed {
            Fixed {
                answers: answers
                    .iter()
                    .map(|(id, seen)| ((*id).clone(), seen.clone()))
                    .collect(),
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl Observer for Fixed {
        fn observe(&self, row: &IntentRow) -> Observation {
            self.asked.borrow_mut().push(row.id.clone());
            self.answers
                .get(&row.id)
                .cloned()
                .unwrap_or(Observation::Unread {
                    reason: "no answer".to_owned(),
                })
        }
    }

    fn states(ledger: &EffectLedger) -> Vec<(IntentId, IntentState)> {
        ledger
            .intents()
            .map(|row| (row.id.clone(), ledger.state(&row.id).unwrap()))
            .collect()
    }

    #[test]
    fn a_killed_append_leaves_no_partial_row_and_the_torn_line_is_dropped_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let first = new_id(
            ledger
                .intend(&host, 1, effect("01-a/u/s", Capability::GhPinUpdate))
                .unwrap(),
        );
        let whole = fs::read(segment(dir.path())).unwrap();
        assert!(whole.ends_with(b"\n"), "one row is one whole line");
        // A kill mid-append leaves part of a line and no newline.
        let torn = br#"{"row":"emitted","intent":"i-2026"#;
        scratch::append(&segment(dir.path()), std::str::from_utf8(torn).unwrap());
        drop(ledger);

        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        assert_eq!(ledger.rows().len(), 1);
        assert_eq!(
            fs::read(segment(dir.path())).unwrap(),
            whole,
            "the torn line is cut"
        );
        ledger.emitted(&host, &first, 1).unwrap();
        let reopened = EffectLedger::open(dir.path(), &lock).unwrap();
        assert_eq!(reopened.rows(), ledger.rows());
        assert_eq!(reopened.last_emitted(&first), Some(1));

        // A whole line that does not parse is not a torn append.
        scratch::append(&segment(dir.path()), "{\"row\":\"nonsense\"}\n");
        let error = EffectLedger::open(dir.path(), &lock).unwrap_err();
        assert!(matches!(error, TickError::Row { line: 3, .. }), "{error}");
    }

    #[test]
    fn a_reopened_ledger_reads_every_row_it_wrote_in_order_with_the_same_states() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let merge = new_id(
            ledger
                .intend(&host, 3, effect("01-a/u/s", Capability::GhPullRequestMerge))
                .unwrap(),
        );
        let pin = new_id(
            ledger
                .intend(&host, 3, effect("01-a/u/t", Capability::GhPinUpdate))
                .unwrap(),
        );
        let run = new_id(
            ledger
                .intend(&host, 3, effect("01-a/u/v", Capability::GhWorkflowRun))
                .unwrap(),
        );
        let handed = new_id(
            ledger
                .intend(
                    &host,
                    3,
                    effect("01-a/u/w", Capability::KubectlAnnotateApplication),
                )
                .unwrap(),
        );
        let review = new_id(
            ledger
                .intend(&host, 3, draft("01-a/u/x", IntentKind::ReviewLaunch, None))
                .unwrap(),
        );
        for id in [&merge, &pin, &run, &handed] {
            ledger.emitted(&host, id, 3).unwrap();
        }
        ledger
            .record(
                &host,
                3,
                receipt(&merge, Capability::GhPullRequestMerge, ReceiptOutcome::Ok),
            )
            .unwrap();
        ledger.park(&host, &pin, "undecidable").unwrap();
        ledger
            .record(
                &host,
                3,
                receipt(&run, Capability::GhWorkflowRun, ReceiptOutcome::Failed),
            )
            .unwrap();
        ledger
            .bind(
                &host,
                &handed,
                Bind::Action {
                    key: "annotate".to_owned(),
                },
            )
            .unwrap();
        ledger
            .verdict(
                &host,
                VerdictRow {
                    intent: review.clone(),
                    head: "a".repeat(40),
                    word: crate::verdict::Verdict::Accept,
                    proof: VerdictProof::Comment {
                        id: 9,
                        app: "astra".to_owned(),
                    },
                },
            )
            .unwrap();
        let written = ledger.rows().to_vec();
        let before = states(&ledger);
        assert_eq!(
            before.iter().map(|(_, state)| *state).collect::<Vec<_>>(),
            [
                IntentState::Received,
                IntentState::Parked,
                IntentState::Open,
                IntentState::Handed,
                IntentState::Closed
            ]
        );
        drop(ledger);

        for reopened in [
            EffectLedger::open(dir.path(), &lock).unwrap(),
            EffectLedger::snapshot(dir.path()).unwrap(),
        ] {
            assert_eq!(reopened.rows(), written.as_slice());
            assert_eq!(states(&reopened), before);
            assert_eq!(
                reopened
                    .open_intents()
                    .iter()
                    .map(|row| &row.id)
                    .collect::<Vec<_>>(),
                [&run, &handed]
            );
            assert_eq!(
                reopened
                    .parked()
                    .iter()
                    .map(|row| &row.id)
                    .collect::<Vec<_>>(),
                [&pin]
            );
        }
    }

    #[test]
    fn open_intents_never_exceed_sixty_four_and_a_target_holds_one_open_intent_per_purpose() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let first = new_id(
            ledger
                .intend(
                    &host,
                    1,
                    effect("01-a/u/s0", Capability::GhPullRequestMerge),
                )
                .unwrap(),
        );
        assert_eq!(
            ledger
                .intend(
                    &host,
                    1,
                    effect("01-a/u/s0", Capability::GhPullRequestMerge)
                )
                .unwrap(),
            Intended::AlreadyOpen(first.clone()),
            "one open intent per target and purpose"
        );
        // Another purpose on the same target, or the same purpose at another head, is new.
        let enqueue = new_id(
            ledger
                .intend(
                    &host,
                    1,
                    effect("01-a/u/s0", Capability::GhPullRequestEnqueue),
                )
                .unwrap(),
        );
        let mut moved = effect("01-a/u/s0", Capability::GhPullRequestMerge);
        moved.head = Some("b".repeat(40));
        new_id(ledger.intend(&host, 1, moved).unwrap());
        for n in 3..MAX_OPEN_INTENTS {
            new_id(
                ledger
                    .intend(
                        &host,
                        1,
                        effect(&format!("01-a/u/s{n}"), Capability::GhPinMerge),
                    )
                    .unwrap(),
            );
        }
        assert_eq!(ledger.open_intents().len(), MAX_OPEN_INTENTS);
        let full = ledger
            .intend(&host, 1, effect("01-a/u/over", Capability::GhPinMerge))
            .unwrap_err();
        assert!(
            matches!(full, TickError::LedgerFull { open: 64, max: 64 }),
            "{full}"
        );
        // A parked intent still counts and still answers AlreadyOpen.
        ledger.park(&host, &enqueue, "undecidable").unwrap();
        assert!(
            ledger
                .intend(&host, 1, effect("01-a/u/over", Capability::GhPinMerge))
                .is_err()
        );
        assert_eq!(
            ledger
                .intend(
                    &host,
                    1,
                    effect("01-a/u/s0", Capability::GhPullRequestEnqueue)
                )
                .unwrap(),
            Intended::AlreadyOpen(enqueue)
        );
        // The cap gates new intents only: completion rows are written while full.
        ledger.emitted(&host, &first, 1).unwrap();
        ledger
            .record(
                &host,
                2,
                receipt(&first, Capability::GhPullRequestMerge, ReceiptOutcome::Ok),
            )
            .unwrap();
        let next = new_id(
            ledger
                .intend(&host, 2, effect("01-a/u/over", Capability::GhPinMerge))
                .unwrap(),
        );
        assert_ne!(next, first);
        let ids: std::collections::BTreeSet<_> =
            ledger.intents().map(|row| row.id.clone()).collect();
        assert_eq!(ids.len(), ledger.intents().count(), "ids are unique");
        assert!(ids.iter().all(|id| id.0.len() == INTENT_ID_MAX_LEN));
    }

    #[test]
    fn a_restart_between_intent_and_effect_binds_retries_or_parks_and_never_launches_twice() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let launches = RefCell::new(Vec::<IntentId>::new());
        // The launch path every tick takes: an intent row, then the effect only for a new one.
        let tick = |ledger: &mut EffectLedger, cycle: u64, target: &str, kill: bool| {
            let intended = ledger
                .intend(&host, cycle, draft(target, IntentKind::StepLaunch, None))
                .unwrap();
            if let Intended::New(id) = intended {
                ledger.emitted(&host, &id, cycle).unwrap();
                if !kill {
                    launches.borrow_mut().push(id);
                }
            }
        };
        {
            let lock = tick_lock(dir.path());
            let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
            // Killed between each intent row and its launch.
            for target in ["01-a/u/done", "01-a/u/gone", "01-a/u/murky"] {
                tick(&mut ledger, 1, target, true);
            }
        }
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let ids: Vec<IntentId> = ledger.intents().map(|row| row.id.clone()).collect();
        let observer = Fixed::new(&[
            (
                &ids[0],
                Observation::Done(Bind::Session {
                    id: "session_1".to_owned(),
                }),
            ),
            (&ids[1], Observation::NotDone),
            (
                &ids[2],
                Observation::Undecidable {
                    reason: "attempt unknown".to_owned(),
                },
            ),
        ]);
        let first = reconcile(&host, &mut ledger, &observer).unwrap();
        assert_eq!(first.bound, [ids[0].clone()]);
        assert_eq!(first.retry, [ids[1].clone()]);
        assert_eq!(first.parked, [ids[2].clone()]);
        assert!(first.unread.is_empty());
        assert_eq!(ledger.state(&ids[0]), Some(IntentState::Bound));
        assert_eq!(
            ledger.state(&ids[1]),
            Some(IntentState::Open),
            "a retry is readmitted, not closed"
        );
        assert_eq!(ledger.state(&ids[2]), Some(IntentState::Parked));

        // The next planning pass writes no second intent and launches nothing for any of them.
        for target in ["01-a/u/done", "01-a/u/gone", "01-a/u/murky"] {
            tick(&mut ledger, 2, target, false);
        }
        assert_eq!(
            ledger.intents().count(),
            4,
            "only the bound target got a new intent"
        );
        let again = reconcile(&host, &mut ledger, &observer).unwrap();
        assert!(again.bound.is_empty() && again.parked.is_empty());
        assert_eq!(again.retry, [ids[1].clone()]);
        assert_eq!(launches.borrow().len(), 1);
        assert!(
            !launches.borrow().iter().any(|id| ids.contains(id)),
            "no intent launched twice"
        );
        assert!(
            !observer.asked.borrow().contains(&launches.borrow()[0]),
            "an intent this handle emitted is not observed"
        );
    }

    #[test]
    fn a_minted_nonce_is_unique_in_the_ledger_and_consumed_once_its_verdict_row_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let mut nonces = std::collections::BTreeSet::new();
        let mut reviews = Vec::new();
        for n in 0..32 {
            let target = format!("01-a/u/s{n}");
            let id = new_id(
                ledger
                    .intend(&host, 1, draft(&target, IntentKind::ReviewLaunch, None))
                    .unwrap(),
            );
            let nonce = ledger.mint_nonce(&id);
            assert_eq!(
                ledger.intent(&id).unwrap().nonce.as_ref(),
                Some(&nonce),
                "minted on the row"
            );
            assert_eq!(ledger.by_nonce(&nonce).map(|row| &row.id), Some(&id));
            assert_eq!(nonce.0.len(), 32);
            assert!(
                nonce
                    .0
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
            assert_eq!(ledger.mint_nonce(&id), nonce, "minting is deterministic");
            assert!(nonces.insert(nonce.0.clone()), "nonce {nonce:?} repeated");
            reviews.push((id, nonce));
        }
        // The nonce travels on the intent row the launch writes.
        let target = "01-a/u/review";
        let mut row = draft(target, IntentKind::ReviewLaunch, None);
        row.nonce = Some(Nonce("0123456789abcdef0123456789abcdef".to_owned()));
        let carried = new_id(ledger.intend(&host, 1, row).unwrap());
        let nonce = Nonce("0123456789abcdef0123456789abcdef".to_owned());
        assert_eq!(ledger.by_nonce(&nonce).map(|row| &row.id), Some(&carried));
        assert!(!ledger.nonce_consumed(&nonce));
        ledger
            .verdict(
                &host,
                VerdictRow {
                    intent: carried.clone(),
                    head: "a".repeat(40),
                    word: crate::verdict::Verdict::FixRequired,
                    proof: VerdictProof::Astra {
                        report_sha256: "0".repeat(64),
                    },
                },
            )
            .unwrap();
        assert!(ledger.nonce_consumed(&nonce));
        assert_eq!(ledger.state(&carried), Some(IntentState::Closed));
        assert_eq!(
            ledger
                .verdicts_at("dekopon-agents/cm", 7, &"a".repeat(40))
                .len(),
            1
        );
        assert!(
            ledger
                .verdicts_at("dekopon-agents/cm", 8, &"a".repeat(40))
                .is_empty()
        );
        assert!(
            !ledger.nonce_consumed(&reviews[0].1),
            "another review's nonce is still free"
        );
        // Another epoch's ledger mints other nonces for the same id.
        let other = tempfile::tempdir().unwrap();
        fs::create_dir_all(other.path().join(".git")).unwrap();
        let elsewhere = EffectLedger::snapshot(&other.path().join("store/epochs/other")).unwrap();
        let here = EffectLedger::snapshot(&other.path().join("store/epochs/here")).unwrap();
        assert_ne!(
            elsewhere.mint_nonce(&reviews[0].0),
            here.mint_nonce(&reviews[0].0)
        );
    }

    #[test]
    fn an_unread_lookup_leaves_the_intent_open_and_parks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        {
            let lock = tick_lock(dir.path());
            let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
            for target in ["01-a/u/s", "01-a/u/t"] {
                let id = new_id(
                    ledger
                        .intend(&host, 1, effect(target, Capability::GhWorkflowRun))
                        .unwrap(),
                );
                ledger.emitted(&host, &id, 1).unwrap();
            }
        }
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let before = ledger.rows().to_vec();
        let ids: Vec<IntentId> = ledger.intents().map(|row| row.id.clone()).collect();
        let down = Observation::Unread {
            reason: "gh: HTTP 502".to_owned(),
        };
        let observer = Fixed::new(&[(&ids[0], down.clone()), (&ids[1], down)]);
        for _ in 0..3 {
            let seen = reconcile(&host, &mut ledger, &observer).unwrap();
            assert_eq!(seen.unread, ids);
            assert!(seen.parked.is_empty() && seen.bound.is_empty() && seen.retry.is_empty());
        }
        assert_eq!(ledger.rows(), before.as_slice(), "nothing is written");
        assert!(ledger.parked().is_empty());
        assert_eq!(ledger.open_intents().len(), 2);
    }

    #[test]
    fn two_clones_appending_rows_merge_by_union_and_every_row_survives() {
        use crate::tick::push::{self, SystemGit};
        let scratch = scratch::Scratch::new();
        let host = clock();
        let tick_clone = scratch.clone("tick");
        let agent_clone = scratch.clone("agent");
        let git = SystemGit::default();
        let mut rows = Vec::new();
        // The tick parks an intent and pushes it; both clones start from that.
        let parked = {
            let campaign = scratch::campaign(&tick_clone);
            let lock = tick_lock(&campaign);
            let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
            let id = new_id(
                ledger
                    .intend(&host, 1, effect("01-a/u/s", Capability::GhPinUpdate))
                    .unwrap(),
            );
            ledger.emitted(&host, &id, 1).unwrap();
            ledger.park(&host, &id, "undecidable").unwrap();
            let seg = std::path::Path::new(scratch::EPOCH).join(ledger.active_segment());
            push::commit_and_push(&git, &tick_clone, &[seg], &[], "tick 1").unwrap();
            id
        };
        scratch::git(
            &agent_clone,
            &["pull", "-q", "--no-rebase", "origin", "main"],
        );
        // Then each appends without seeing the other: the agent a ruling, the tick a new intent.
        for (checkout, cycle) in [(&agent_clone, 0), (&tick_clone, 2)] {
            let campaign = scratch::campaign(checkout);
            let lock = tick_lock(&campaign);
            let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
            if cycle == 0 {
                ledger.rule(&host, &parked, Ruling::Retry).unwrap();
            } else {
                let id = new_id(
                    ledger
                        .intend(&host, cycle, effect("01-a/u/t", Capability::GhPinMerge))
                        .unwrap(),
                );
                ledger.emitted(&host, &id, cycle).unwrap();
            }
            rows.extend(ledger.rows().iter().cloned());
            let seg = std::path::Path::new(scratch::EPOCH).join(ledger.active_segment());
            let pushed = push::commit_and_push(&git, checkout, &[seg], &[], "append").unwrap();
            assert!(pushed.commit.is_some());
        }
        scratch::git(
            &agent_clone,
            &["pull", "-q", "--no-rebase", "origin", "main"],
        );
        for checkout in [&tick_clone, &agent_clone] {
            let ledger = EffectLedger::snapshot(&scratch::campaign(checkout)).unwrap();
            for row in &rows {
                assert!(ledger.rows().contains(row), "{row:?} lost in {checkout:?}");
            }
            assert_eq!(
                ledger.rows().len(),
                6,
                "three shared rows, one ruling, two new rows"
            );
            assert_eq!(
                ledger.state(&parked),
                Some(IntentState::Open),
                "the ruling folds in"
            );
        }
    }

    #[test]
    fn a_snapshot_ignores_a_torn_line_and_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        {
            let lock = tick_lock(dir.path());
            let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
            new_id(
                ledger
                    .intend(&host, 1, effect("01-a/u/s", Capability::GhPinUpdate))
                    .unwrap(),
            );
        }
        scratch::append(&segment(dir.path()), "{\"row\":\"parked\",\"int");
        let bytes = fs::read(segment(dir.path())).unwrap();
        // Taken while another process holds the lock.
        let held = tick_lock(dir.path());
        assert!(
            TickLock::try_acquire(dir.path()).unwrap().is_none(),
            "a second holder is refused"
        );
        let mut snapshot = EffectLedger::snapshot(dir.path()).unwrap();
        assert_eq!(snapshot.rows().len(), 1);
        let error = snapshot
            .intend(&host, 1, effect("01-a/u/t", Capability::GhPinUpdate))
            .unwrap_err();
        assert!(matches!(error, TickError::Io { .. }), "{error}");
        let id = snapshot.intents().next().unwrap().id.clone();
        assert!(snapshot.park(&host, &id, "x").is_err());
        assert_eq!(snapshot.rows().len(), 1);
        assert_eq!(
            fs::read(segment(dir.path())).unwrap(),
            bytes,
            "the torn line stays on disk"
        );
        let names: Vec<_> = fs::read_dir(dir.path().join(LEDGER_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "the segment and the lock: {names:?}");
        drop(held);
    }

    #[test]
    fn an_intent_stays_within_its_row_budget_and_no_segment_reaches_one_mib() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let cap = Capability::GhPinUpdate;
        let id = new_id(ledger.intend(&host, 1, effect("01-a/u/s", cap)).unwrap());
        let tries = crate::tick::timers::MAX_TRIES as u64;
        for round in 0..=MAX_RULINGS_PER_INTENT as u64 {
            for n in 0..tries {
                ledger.emitted(&host, &id, round * 10 + n).unwrap();
                let failed = receipt(&id, cap, ReceiptOutcome::Failed);
                assert_eq!(
                    ledger.record(&host, round * 10 + n, failed).unwrap(),
                    Recorded::New
                );
            }
            ledger.park(&host, &id, "five tries failed").unwrap();
            if round < MAX_RULINGS_PER_INTENT as u64 {
                ledger.rule(&host, &id, Ruling::Retry).unwrap();
            }
        }
        assert!(
            ledger.rule(&host, &id, Ruling::Retry).is_err(),
            "only done or drop now"
        );
        ledger
            .rule(
                &host,
                &id,
                Ruling::Drop {
                    reason: "owner".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(ledger.state(&id), Some(IntentState::Closed));
        let mine = ledger
            .rows()
            .iter()
            .filter(|row| match row {
                LedgerRow::Intent(row) => row.id == id,
                LedgerRow::Emitted { intent, .. }
                | LedgerRow::Bound { intent, .. }
                | LedgerRow::Parked { intent, .. }
                | LedgerRow::Ruled { intent, .. } => *intent == id,
                LedgerRow::Receipt { receipt, .. } => receipt.intent == id,
                LedgerRow::Verdict { verdict, .. } => verdict.intent == id,
            })
            .count();
        assert!(mine <= MAX_ROWS_PER_INTENT, "{mine} rows");
        assert_eq!(MAX_ROWS_PER_INTENT, 38);

        // Rows near the 9 KiB ceiling roll segments at 512 KiB, and none reaches 1 MiB.
        let other = new_id(ledger.intend(&host, 1, effect("01-a/u/t", cap)).unwrap());
        let reason = "r".repeat(9 * 1024 - 200);
        for _ in 0..200 {
            ledger.park(&host, &other, &reason).unwrap();
        }
        let sizes: Vec<u64> = fs::read_dir(dir.path().join(LEDGER_DIR))
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(SEGMENT_PREFIX)
            })
            .map(|entry| entry.metadata().unwrap().len())
            .collect();
        assert!(sizes.len() >= 3, "{sizes:?}");
        assert!(sizes.iter().all(|size| *size < 1024 * 1024), "{sizes:?}");
        let reread = EffectLedger::snapshot(dir.path()).unwrap();
        assert_eq!(reread.rows(), ledger.rows(), "segments read back in order");
        assert_eq!(reread.active_segment(), ledger.active_segment());
    }

    #[test]
    fn a_late_receipt_after_park_closes_the_intent_and_a_conflicting_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        let cap = Capability::GhPinUpdate;
        let parked = new_id(ledger.intend(&host, 1, effect("01-a/u/s", cap)).unwrap());
        let handed = new_id(ledger.intend(&host, 1, effect("01-a/u/t", cap)).unwrap());
        for id in [&parked, &handed] {
            ledger.emitted(&host, id, 1).unwrap();
        }
        ledger.park(&host, &parked, "no receipt").unwrap();
        ledger
            .bind(
                &host,
                &handed,
                Bind::Action {
                    key: "pin".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(ledger.state(&handed), Some(IntentState::Handed));

        let ok = receipt(&parked, cap, ReceiptOutcome::Ok);
        assert_eq!(ledger.record(&host, 2, ok.clone()).unwrap(), Recorded::New);
        assert_eq!(ledger.state(&parked), Some(IntentState::Received));
        assert_eq!(
            ledger.record(&host, 3, ok.clone()).unwrap(),
            Recorded::Duplicate
        );
        let rows = ledger.rows().len();
        let different = receipt(&parked, cap, ReceiptOutcome::Refused);
        assert!(matches!(
            ledger.record(&host, 3, different),
            Err(TickError::ReceiptConflict(id)) if id == parked
        ));
        let mut wrong = receipt(&handed, Capability::GhPinMerge, ReceiptOutcome::Ok);
        assert!(matches!(
            ledger.record(&host, 3, wrong.clone()),
            Err(TickError::ReceiptConflict(_))
        ));
        wrong.intent = IntentId("i-20261122T1800-9999".to_owned());
        wrong.capability = cap;
        assert!(matches!(
            ledger.record(&host, 3, wrong),
            Err(TickError::UnknownIntent(_))
        ));
        assert_eq!(ledger.rows().len(), rows, "refused receipts write nothing");

        let refused = receipt(&handed, cap, ReceiptOutcome::Refused);
        assert_eq!(
            ledger.record(&host, 3, refused.clone()).unwrap(),
            Recorded::New
        );
        assert_eq!(ledger.state(&handed), Some(IntentState::Received));
        assert_eq!(ledger.receipt(&handed), Some(&refused));
        assert_eq!(
            ledger
                .receipts_from(1)
                .iter()
                .map(|(cycle, r)| (*cycle, r.intent.clone()))
                .collect::<Vec<_>>(),
            [(3, handed.clone())]
        );
        assert!(ledger.open_intents().is_empty() && ledger.parked().is_empty());
    }

    #[test]
    fn an_earlier_cycles_unreceipted_dispatch_parks_and_is_never_reemitted_by_absence() {
        let dir = tempfile::tempdir().unwrap();
        let host = clock();
        let (dispatched, pin, unsent, handed) = {
            let lock = tick_lock(dir.path());
            let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
            let dispatched = new_id(
                ledger
                    .intend(&host, 1, effect("01-a/u/r", Capability::GhWorkflowRun))
                    .unwrap(),
            );
            let pin = new_id(
                ledger
                    .intend(&host, 1, effect("01-a/u/p", Capability::GhPinUpdate))
                    .unwrap(),
            );
            let unsent = new_id(
                ledger
                    .intend(&host, 1, effect("01-a/u/u", Capability::GhWorkflowRun))
                    .unwrap(),
            );
            let handed = new_id(
                ledger
                    .intend(&host, 1, effect("01-a/u/h", Capability::GhPinMerge))
                    .unwrap(),
            );
            for id in [&dispatched, &pin, &handed] {
                ledger.emitted(&host, id, 1).unwrap();
            }
            ledger
                .bind(
                    &host,
                    &handed,
                    Bind::Action {
                        key: "pin-merge".to_owned(),
                    },
                )
                .unwrap();
            (dispatched, pin, unsent, handed)
        };
        let lock = tick_lock(dir.path());
        let mut ledger = EffectLedger::open(dir.path(), &lock).unwrap();
        // Every lookup finds nothing: absence.
        let observer = Fixed::new(&[
            (&dispatched, Observation::NotDone),
            (&pin, Observation::NotDone),
            (&unsent, Observation::NotDone),
            (&handed, Observation::NotDone),
        ]);
        let seen = reconcile(&host, &mut ledger, &observer).unwrap();
        assert_eq!(seen.parked, [dispatched.clone(), pin.clone()]);
        assert_eq!(
            seen.retry,
            std::slice::from_ref(&unsent),
            "never emitted, so absence is not done"
        );
        assert!(
            !seen.retry.contains(&handed),
            "a handed intent is never re-emitted"
        );
        assert_eq!(ledger.state(&handed), Some(IntentState::Handed));
        // Positive evidence still binds one that was never parked.
        let late = Fixed::new(&[(
            &unsent,
            Observation::Done(Bind::Pull {
                number: 3,
                head: "c".repeat(40),
            }),
        )]);
        assert_eq!(
            reconcile(&host, &mut ledger, &late).unwrap().bound,
            [unsent]
        );
        let again = reconcile(&host, &mut ledger, &observer).unwrap();
        assert!(
            again.retry.is_empty() && again.parked.is_empty(),
            "parked stays parked"
        );
    }
}
