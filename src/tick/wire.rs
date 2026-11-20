//! The 11-D3 wire contract with dekopon's RPi coordinator runner (`rpi/tick-runner`). These shapes
//! are frozen by the skeleton: a unit may not rename a field or a capability.
//!
//! Step (1): `cm tick --effects --approved <sha>` prints one [`EffectsOut`] on stdout, every
//! intent row already pushed. Step (3): `cm effects record` reads a JSON array of [`Receipt`] on
//! stdin, at most [`BATCH_MAX_BYTES`].

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tick::ledger::IntentId;
use crate::tick::packet::DecisionPacket;

/// 11-D3: one receipt's serialized size; a larger output is recorded as [`Oversize`].
pub const RECEIPT_MAX_BYTES: usize = 8 * 1024;
/// 11-D3: one `cm effects record` stdin.
pub const BATCH_MAX_BYTES: usize = 20 * 1024;
/// vm-runner caps step (1)'s stdout (README §Named sessions).
pub const TICK_STDOUT_MAX_BYTES: usize = 64 * 1024;
/// Full-queue policy: effects one tick emits; the rest wait for the next tick, oldest first.
pub const MAX_EFFECTS_PER_TICK: usize = 16;

/// Step (1)'s stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectsOut {
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet: Option<DecisionPacket>,
}

/// One effect the runner applies, in order, as principal `rpi-coordinator`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub intent: IntentId,
    pub capability: Capability,
    pub input: Value,
}

/// 11-D19: the rpi-coordinator principal's grants, by their capability ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "gh.pull-request.merge")]
    GhPullRequestMerge,
    #[serde(rename = "gh.pull-request.enqueue")]
    GhPullRequestEnqueue,
    #[serde(rename = "gh.workflow.run")]
    GhWorkflowRun,
    #[serde(rename = "gh.pin.update")]
    GhPinUpdate,
    #[serde(rename = "gh.pin.merge")]
    GhPinMerge,
    #[serde(rename = "kubectl.annotate.application")]
    KubectlAnnotateApplication,
    #[serde(rename = "kubectl.get.application")]
    KubectlGetApplication,
    #[serde(rename = "kubectl.get.deployment")]
    KubectlGetDeployment,
    #[serde(rename = "kubectl.get.readyz")]
    KubectlGetReadyz,
}

/// 11-D12: REST merge with `sha` bound to the reviewed head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MergeInput {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub expected_head_sha: String,
}

/// 11-D15: `additionalProperties: false`; `expectedHeadSha` is 40 hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnqueueInput {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub expected_head_sha: String,
}

/// 11-D14: the Cut release dispatch, fenced to `cut-release.yml`'s numeric id and `ref == "main"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunInput {
    pub owner: String,
    pub repo: String,
    pub workflow_id: u64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub inputs: CutInputs,
}

/// `cut-release.yml`'s dispatch inputs; GitHub takes every input as a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CutInputs {
    pub tags: String,
    pub sha: String,
    pub dry_run: String,
}

/// 11-D17.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinUpdateInput {
    /// 1 to 8 entries.
    pub pins: Vec<PinValue>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinValue {
    pub name: String,
    pub value: String,
}

/// 11-D18: `number` and the head come from the recorded `gh.pin.update` receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PinMergeInput {
    pub repo: String,
    pub number: u64,
    pub expected_head_sha: String,
}

/// 11-D20: the four kubectl forms; the runner maps each to its capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KubectlInput {
    /// `application`, `deployment` or `readyz`.
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// What the runner saw applying one effect. Effects after the first refusal or error are not
/// run and get no receipt; their intents stay open for the next tick's reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectOutcome {
    Applied,
    Refused,
    Error,
}

/// Step (3)'s stdin element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub intent: IntentId,
    pub capability: Capability,
    pub outcome: EffectOutcome,
    pub output: ReceiptOutput,
}

/// An output is inline below [`RECEIPT_MAX_BYTES`], else only its size and hash: never truncated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReceiptOutput {
    Oversize(Oversize),
    Inline(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oversize {
    /// Always `true` on the wire.
    pub oversize: bool,
    pub bytes: u64,
    /// Lower-case hex of the output's SHA-256.
    pub sha256: String,
}

impl Effect {
    /// Serializes a typed input under its capability; refuses an input whose type does not match.
    pub fn new(intent: IntentId, capability: Capability, input: &impl Serialize) -> Self {
        todo!("tick/ledger")
    }
}

impl Capability {
    /// The wire id, `gh.pull-request.merge` and so on.
    pub fn id(self) -> &'static str {
        todo!("tick/ledger")
    }
}

impl ReceiptOutput {
    /// Inline when the serialized receipt stays under [`RECEIPT_MAX_BYTES`], else [`Oversize`].
    pub fn bounded(output: Value) -> Self {
        todo!("tick/ledger")
    }
}

impl EffectsOut {
    /// The JSON line step (1) prints; refuses (never truncates) past [`TICK_STDOUT_MAX_BYTES`].
    pub fn to_stdout(&self) -> crate::tick::Result<String> {
        todo!("tick/ledger")
    }
}
