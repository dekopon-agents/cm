//! The agent ledger (`tick/agent-ledger`; PLAN.md `07-tick/agent-ledger`): every agent invocation
//! as one row of the epoch's `AGENTS.jsonl`, in the schema of campaign-trail
//! `tools/agent-ledger/LEDGER.md`, which becomes cm's.
//!
//! One row per invocation. `cm launch --model --effort` writes it with the requested values and
//! every echoed value `"unknown"`; `cm attempt record`, `cm attempt end` and `cm review record`
//! with echo flags fill what the harness echoed, tokens included (the harvest at close). cm finds
//! its own row by the added `invocation` key (the attempt id, `<step>#<launch>`, or for a review
//! `review:<step>@<head>`) and rewrites that one line in place under `store::lock`. Every other
//! line, a `record.py` row or a shadow receipt's row, is kept byte for byte. A value nobody echoed
//! stays `"unknown"`, never copied from the request (LEDGER.md, Rules).
//!
//! The CLI calls these only when a `Requested` or `Echo` flag is given (the glue in `cli.rs`), so
//! the existing commands behave as before; this unit touches no existing source file.

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
    pub parent_session_id: String,
    pub outcome: Option<String>,
    pub source: String,
    /// cm's key for its own rows; absent on every row cm did not write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    /// LEDGER.md's optional fields (`requests`, `finished`, `description`, `mix`, …), kept as read.
    #[serde(flatten)]
    pub optional: Map<String, Value>,
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
/// fills the invocation's row. A refused outcome passes through untouched.
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
}
