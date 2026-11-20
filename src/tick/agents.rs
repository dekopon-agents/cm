//! The agent ledger (`tick/agent-ledger`): every agent invocation as one row of the epoch's
//! `AGENTS.jsonl`, in the schema of `tools/agent-ledger/LEDGER.md`, which becomes cm's. `cm launch`
//! writes the requested model and effort; `cm attempt record` and `cm review record` fill the
//! echoed values; a harvest fills tokens at close; an unechoed value is `unknown`.

#![allow(
    unused_variables,
    reason = "skeleton: tick/agent-ledger fills these bodies"
)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::host::Host;
use crate::outcome::Outcome;
use crate::store::Target;

pub const AGENTS_FILE: &str = "AGENTS.jsonl";
pub const UNKNOWN: &str = "unknown";

/// The echoed values, as flags on `cm attempt record` and `cm review record`. All absent is the
/// existing behaviour; the CLI calls [`echoed`] only when one is given.
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct Echo {
    /// The model the runtime reports.
    #[arg(long = "echo-model", value_name = "MODEL")]
    pub model: Option<String>,
    /// The effort the runtime reports.
    #[arg(long = "echo-effort", value_name = "EFFORT")]
    pub effort: Option<String>,
    #[arg(long, value_name = "NAME")]
    pub harness: Option<String>,
    #[arg(long, value_name = "VERSION")]
    pub harness_version: Option<String>,
    #[arg(long, value_name = "N")]
    pub tokens_in: Option<u64>,
    #[arg(long, value_name = "N")]
    pub tokens_out: Option<u64>,
    #[arg(long, value_name = "N")]
    pub tokens_cache_read: Option<u64>,
    #[arg(long, value_name = "N")]
    pub tokens_cache_write: Option<u64>,
}

/// One value: known, or the literal `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Field<T> {
    Known(T),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: Field<u64>,
    pub output: Field<u64>,
    pub cache_read: Field<u64>,
    pub cache_write: Field<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Launch,
    Echo,
    Harvest,
}

/// One `AGENTS.jsonl` row; field names are LEDGER.md's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRow {
    pub at: String,
    pub target: String,
    pub source: Source,
    pub requested_model: Field<String>,
    pub requested_effort: Field<String>,
    pub model: Field<String>,
    pub effort: Field<String>,
    pub harness: Field<String>,
    pub harness_version: Field<String>,
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
}

/// `cm launch`: the requested model and effort, everything echoed still `unknown`.
pub fn record_launch(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    model: &str,
    effort: &str,
) -> crate::tick::Result<()> {
    todo!("tick/agent-ledger")
}

/// After `cm attempt record` or `cm review record` succeeded: one echo row. Refused outcomes pass
/// through untouched.
pub fn echoed(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    outcome: Outcome,
    echo: &Echo,
) -> anyhow::Result<Outcome> {
    todo!("tick/agent-ledger")
}

/// At `cm attempt end`: fills tokens from the attempt's receipts; what none carries stays `unknown`.
pub fn harvest(host: &dyn Host, root: &Path, target: &Target) -> crate::tick::Result<()> {
    todo!("tick/agent-ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn an_aborted_append_leaves_no_partial_agents_row() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn a_reloaded_ledger_reads_hand_written_record_py_rows_and_cm_rows_alike() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn an_invocation_holds_one_launch_row_and_at_most_one_harvest_row() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/agent-ledger"]
    fn restart_before_harvest_leaves_every_unechoed_value_unknown() {
        todo!()
    }
}
