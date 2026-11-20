//! The agent ledger (`tick/agent-ledger`; PLAN.md `07-tick/agent-ledger`): every agent invocation
//! as one row of the epoch's `AGENTS.jsonl`, in the schema of campaign-trail
//! `tools/agent-ledger/LEDGER.md`, which becomes cm's.
//!
//! One row per invocation, keyed by [`Invocation`] in the added `invocation` field (R14): a step
//! launch's attempt id, a tick review launch's intent id, or, for a review the tick did not launch,
//! the recorded review's number. `cm launch --model --effort` writes the row with the requested
//! values and every echoed value `"unknown"`; `cm attempt record`, `cm attempt end` and
//! `cm review record` with echo flags fill what the harness echoed, tokens included (the harvest
//! at close). A value nobody echoed stays `"unknown"`, never copied from the request (LEDGER.md,
//! Rules). Every other line, a `record.py` row, a shadow receipt's row or a row cm cannot parse, is
//! kept byte for byte.
//!
//! **One writer protocol (R16).** Every writer of `AGENTS.jsonl` holds the campaign lock,
//! `<epoch>/.cm/lock` (`flock` `LOCK_EX`, cm's `store::lock`), for its whole read, change and
//! write: cm's in-place rewrite and every appender. campaign-trail `tools/agent-ledger/record.py`
//! takes that lock before its load and replace; that one-line change is campaign-trail's (owner:
//! the coordinator, before `tick/agent-ledger` lands), and until it lands `record.py` and cm must
//! not write one epoch's file at the same time.
//!
//! **Two callers.** The CLI calls these only when a `Requested` or `Echo` flag is given (the glue
//! in `cli.rs`), so the existing commands behave as before. The tick records every launch it
//! performs and every Astra receipt it collects through [`AgentRecorder`] (R15): `Deps::agents`
//! is [`CmAgentRecorder`] in production and a fake in `tick/core`'s tests. This unit touches no
//! existing source file.

#![allow(
    unused_variables,
    reason = "skeleton: tick/agent-ledger fills these bodies"
)]

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::host::Host;
use crate::outcome::Outcome;
use crate::store::Target;

pub const AGENTS_FILE: &str = "AGENTS.jsonl";
pub const UNKNOWN: &str = "unknown";

/// `cm launch`'s requested values.
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct Requested {
    /// The model the launch asks for; `default` when the launcher passes none.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// The effort the launch asks for.
    #[arg(long, value_name = "EFFORT")]
    pub effort: Option<String>,
    /// LEDGER.md `role`: planner, writer, reviewer, recon, qc, coordinator, triage or other.
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
}

/// What a harness echoed, as flags on `cm attempt record`, `cm attempt end` and
/// `cm review record`. All absent is the existing behaviour.
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct Echo {
    /// The model the runtime reports.
    #[arg(long = "echo-model", value_name = "MODEL")]
    pub model: Option<String>,
    /// The effort the runtime reports.
    #[arg(long = "echo-effort", value_name = "EFFORT")]
    pub effort: Option<String>,
    #[arg(long = "echo-harness", value_name = "NAME")]
    pub harness: Option<String>,
    #[arg(long = "echo-harness-version", value_name = "VERSION")]
    pub harness_version: Option<String>,
    /// The harness session (Claude Code `sessionId`, Codex `session_id`, codex-cloud thread).
    #[arg(long = "echo-session", value_name = "ID")]
    pub session: Option<String>,
    #[arg(long = "tokens-input-uncached", value_name = "N")]
    pub input_uncached: Option<u64>,
    #[arg(long = "tokens-cache-write", value_name = "N")]
    pub cache_write: Option<u64>,
    #[arg(long = "tokens-cache-read", value_name = "N")]
    pub cache_read: Option<u64>,
    #[arg(long = "tokens-output", value_name = "N")]
    pub output: Option<u64>,
    #[arg(long = "tokens-reasoning", value_name = "N")]
    pub reasoning: Option<u64>,
    /// LEDGER.md `tokens_source`: where the numbers were read.
    #[arg(long = "tokens-source", value_name = "TEXT")]
    pub tokens_source: Option<String>,
}

/// An int, or the literal `"unknown"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Count {
    Known(u64),
    Unknown(String),
}

/// LEDGER.md `{requested, echoed}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub requested: String,
    pub echoed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Harness {
    pub name: String,
    pub version: String,
}

/// LEDGER.md `tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input_uncached: Count,
    pub cache_write: Count,
    pub cache_read: Count,
    pub output: Count,
    pub reasoning: Count,
}

/// One `AGENTS.jsonl` row; field names and order are LEDGER.md's §Fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRow {
    pub ts_start: String,
    pub ts_end: String,
    pub epoch: String,
    pub step: String,
    pub role: String,
    pub harness: Harness,
    pub model: Pair,
    pub effort: Pair,
    pub tokens: Tokens,
    pub tokens_source: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    /// LEDGER.md: `null` when nothing records the launcher.
    pub parent_session_id: Option<String>,
    pub outcome: Option<String>,
    pub source: String,
    /// cm's key for its own rows; absent on every row cm did not write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    /// LEDGER.md's optional fields (`requests`, `finished`, `description`, `mix`, …), kept as read.
    #[serde(flatten)]
    pub optional: Map<String, Value>,
}

/// A row's key: one per invocation, so two reviews of one head never share a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// A step launch: its attempt id, `<step>#<launch>`.
    Attempt(String),
    /// A review the tick launched: its intent id.
    Intent(crate::tick::IntentId),
    /// A review recorded with `cm review record` that no intent launched: `R<number>`.
    Review(u32),
}

/// The tick's seam into the agent ledger (R15).
pub trait AgentRecorder {
    /// A launch the tick performed, with what it requested.
    fn launched(
        &self,
        root: &Path,
        target: &str,
        invocation: &Invocation,
        requested: &Requested,
    ) -> crate::tick::Result<()>;
    /// An Astra receipt the tick collected: its `requested` and `effective` model and effort, and
    /// its turn's start and end, become the invocation row's echo.
    fn astra_receipt(
        &self,
        root: &Path,
        invocation: &Invocation,
        receipt: &Path,
    ) -> crate::tick::Result<()>;
}

/// Production [`AgentRecorder`].
pub struct CmAgentRecorder<'a> {
    pub host: &'a dyn Host,
}

impl AgentRecorder for CmAgentRecorder<'_> {
    fn launched(
        &self,
        root: &Path,
        target: &str,
        invocation: &Invocation,
        requested: &Requested,
    ) -> crate::tick::Result<()> {
        todo!("tick/agent-ledger")
    }

    fn astra_receipt(
        &self,
        root: &Path,
        invocation: &Invocation,
        receipt: &Path,
    ) -> crate::tick::Result<()> {
        todo!("tick/agent-ledger")
    }
}

/// After `cm launch` succeeded with `Requested` flags: the invocation's row.
pub fn record_launch(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    requested: &Requested,
) -> crate::tick::Result<()> {
    todo!("tick/agent-ledger")
}

/// After `cm attempt record`, `cm attempt end` or `cm review record` succeeded with `Echo` flags:
/// fills the invocation's row. The key is the step's current attempt for the attempt commands, and
/// for `cm review record` the intent whose nonce the report carries, else the review's number. A
/// refused outcome passes through untouched.
pub fn echoed(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    outcome: Outcome,
    echo: &Echo,
) -> anyhow::Result<Outcome> {
    todo!("tick/agent-ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn a_killed_rewrite_leaves_the_previous_file_whole() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn a_reloaded_file_keeps_hand_written_record_py_rows_byte_for_byte_beside_cm_rows() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn an_invocation_holds_exactly_one_row_however_many_echoes_fill_it() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn a_restart_before_the_echo_leaves_every_unechoed_value_unknown() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn a_null_parent_session_id_row_round_trips_and_an_unparsable_row_is_kept_raw() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn two_reviews_of_one_head_by_two_reviewers_get_two_rows() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn an_append_by_a_lock_holding_writer_during_a_cm_rewrite_is_kept() {
        todo!()
    }
}
