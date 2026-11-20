//! `cm export --json` (`tick/agent-ledger`): the unit and step graph the dashboard reads, so
//! UNITS.json retires.

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
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// The `--after` form: `merged`, `pr-open`, `tag`, `deployed`.
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
