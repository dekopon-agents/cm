//! `cm export --json` (`tick/agent-ledger`; R20): the unit and step graph cm holds, under the
//! outcome's `graph` field. cm holds state, lanes and `--after` edges, not plan facts (title, wave,
//! size, ideas, depends_on, calls); those stay in UNITS.json, the plan-fact source, and are not
//! copied into cm. Each node carries its UNITS.json id, so the dashboard joins the two. Retiring
//! UNITS.json needs a plan-fact source the owner names (G8); this unit does not retire it.

#![allow(
    unused_variables,
    reason = "skeleton: tick/agent-ledger fills these bodies"
)]

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::outcome::Outcome;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Node {
    /// `<NN-name>/<unit>` or `<NN-name>/<unit>/<step>`.
    pub id: String,
    /// The UNITS.json id a unit node maps to: the unit path with its `NN-` prefix dropped
    /// (`07-tick/ledger` → `tick/ledger`); `None` for a step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    /// `unit` or `step`.
    pub kind: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edge {
    pub from: String,
    /// A step id, or for a `tag` or `deployed` gate the release target `<line>@<version>`, which
    /// may name a line no unit in this campaign declares.
    pub to: String,
    /// The `--after` form: `merged`, `pr-open`, `tag` or `deployed`.
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

pub fn graph(root: &Path) -> Result<Graph> {
    todo!("tick/agent-ledger")
}

/// The outcome carries the graph under `graph`.
pub fn export(root: &Path) -> Result<Outcome> {
    todo!("tick/agent-ledger")
}
