//! The 11-D3 wire contract with dekopon's RPi coordinator runner (`rpi/tick-runner`). These
//! shapes are frozen by the skeleton: no unit renames a field or a capability (S-tick-10..13).
//!
//! The runner's two command lines, frozen:
//! - Step (1): `cm tick --effects --approved <40-hex> -C <checkout>/store/epochs/<slug>`. Exit 0
//!   means stdout is exactly one [`EffectsOut`] JSON line and every effect's intent row is
//!   already pushed. A tick that started and then failed still exits 0, with no effects and a
//!   `tick-aborted` packet item. A non-zero exit (bad arguments, no campaign folder) prints
//!   nothing on stdout, and the runner applies nothing.
//! - Step (3): `cm effects record -C <same>` reads one JSON array of [`Receipt`] on stdin, at most
//!   [`BATCH_MAX_BYTES`], in effect order. Exit 0 means the batch is committed and pushed.
//!
//! The stdout bound holds by construction: [`admit`] sizes the effects and the packet before any
//! intent row is written, and whatever does not fit waits for the next tick. cm never prints a
//! line past [`TICK_STDOUT_MAX_BYTES`], so vm-runner's `truncated` mark cannot come from cm.
//!
//! The inputs are the provider's own schemas, which deny unknown fields: provider-gh
//! `reviews.rs` `MergeInput` and `workflows.rs` `Input` at `5b1616e`, and 11-D15, 11-D17 and
//! 11-D18 for the verbs `11-rpi/gh-verbs` builds. The kubectl inputs are this skeleton's reading of
//! 11-D20's four forms; `11-rpi/k8s-provider` confirms them (SKELETON.md, Choices).

#![allow(unused_variables, reason = "skeleton: tick/ledger fills these bodies")]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tick::ledger::IntentId;
use crate::tick::packet::PacketKind;

/// 11-D3: one receipt's serialized size; a larger output is recorded as [`Oversize`].
pub const RECEIPT_MAX_BYTES: usize = 8 * 1024;
/// 11-D3: one `cm effects record` stdin (provider-vm's 24,576-byte argv plus stdin budget).
pub const BATCH_MAX_BYTES: usize = 20 * 1024;
/// vm-runner's cap on step (1)'s stdout (README §Named sessions).
pub const TICK_STDOUT_MAX_BYTES: usize = 64 * 1024;
/// Full-queue policy: effects one tick emits; the rest wait for the next tick, oldest first.
pub const MAX_EFFECTS_PER_TICK: usize = 16;

/// Step (1)'s stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectsOut {
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet: Option<PacketOut>,
}

/// 11-D3 (4): what the runner delivers. It posts `text` verbatim as one message to the
/// coordinator conversation and reads nothing else; `items` is for tests and the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketOut {
    /// Markdown, at most [`crate::tick::packet::PACKET_MAX_BYTES`].
    pub text: String,
    pub items: Vec<PacketLine>,
    /// Items past the packet's bounds; they stay unseen and come back on a later tick.
    pub omitted: usize,
}

/// One packet item as the wire carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketLine {
    pub key: String,
    pub kind: PacketKind,
}

/// One effect the runner applies, in order, as principal `rpi-coordinator`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub intent: IntentId,
    pub capability: Capability,
    pub input: Value,
}

/// 11-D19: the `rpi-coordinator` principal's grants, by their capability ids. A capability not
/// listed here cannot be emitted; adding one is a skeleton change and a grant change.
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

/// A typed input; its variant fixes the capability, so a mismatched pair cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectInput {
    Merge(MergeInput),
    Enqueue(EnqueueInput),
    WorkflowRun(WorkflowRunInput),
    PinUpdate(PinUpdateInput),
    PinMerge(PinMergeInput),
    AnnotateApplication(ApplicationInput),
    GetApplication(ApplicationInput),
    GetDeployment(DeploymentInput),
    GetReadyz(ReadyzInput),
}

/// provider-gh `reviews.rs` `MergeInput`: 11-D12's REST merge, `sha` = the gated head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MergeInput {
    pub owner: String,
    pub repo: String,
    pub number: u32,
    pub merge_method: MergeMethod,
    /// Required by cm although the provider takes it as optional: every merge is head-bound.
    pub expected_head_sha: String,
}

/// provider-gh `MergeMethod`. cm sends [`MERGE_METHOD`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

/// The method every tick merge uses: `merge-pr.py`'s default, so the interim and the RPi land
/// commits the same way.
pub const MERGE_METHOD: MergeMethod = MergeMethod::Squash;

/// 11-D15: `additionalProperties: false`; `expectedHeadSha` is 40 hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnqueueInput {
    pub owner: String,
    pub repo: String,
    pub number: u32,
    pub expected_head_sha: String,
}

/// provider-gh `workflows.rs` `Input`, fenced by 11-D14 to `cut-release.yml`'s numeric id with
/// `ref == "main"` and inputs `tags`, `sha`, `dry_run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowRunInput {
    pub owner: String,
    pub repo: String,
    /// The numeric workflow id; the file form would miss 11-D14's Cedar fence.
    pub workflow: u64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub inputs: Vec<WorkflowField>,
}

/// provider-gh `InputField`: GitHub takes every dispatch input as a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowField {
    pub name: String,
    pub value: String,
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
    pub number: u32,
    pub expected_head_sha: String,
}

/// 11-D20 `annotate application <app>` and `get application <app>`; Argo's namespace is a
/// broker setting, never an input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationInput {
    pub application: String,
}

/// 11-D20 `get deployment <name> -n <ns>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentInput {
    pub name: String,
    pub namespace: String,
}

/// 11-D20 `get --raw /readyz`: no input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyzInput {}

/// What the runner saw applying one effect. The runner stops at the first `refused` or `error`;
/// the effects after it get no receipt and their intents stay open for the next reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectOutcome {
    /// Closes the intent.
    Applied,
    /// Closes the intent with a packet item: Cedar or the provider said no, which is judgment.
    Refused,
    /// Leaves the intent open: the write may or may not have happened, so reconcile decides.
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

/// Inline when the whole receipt serializes under [`RECEIPT_MAX_BYTES`], else only the output's
/// size and hash: never truncated.
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

/// What [`admit`] let through and what waits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Admitted {
    /// Indices into the proposed list, in order.
    pub admitted: Vec<usize>,
    /// Proposed effects left for the next tick by the count or byte bound.
    pub deferred: usize,
}

impl Capability {
    /// The wire id, `gh.pull-request.merge` and so on.
    pub fn id(self) -> &'static str {
        todo!("tick/ledger")
    }
}

impl EffectInput {
    pub fn capability(&self) -> Capability {
        todo!("tick/ledger")
    }
}

impl Effect {
    /// Serializes a typed input under its capability.
    pub fn new(intent: IntentId, input: &EffectInput) -> Self {
        todo!("tick/ledger")
    }
}

impl ReceiptOutput {
    /// Inline when the receipt carrying it stays under [`RECEIPT_MAX_BYTES`], else [`Oversize`]
    /// with the output's byte length and SHA-256.
    pub fn bounded(output: Value) -> Self {
        todo!("tick/ledger")
    }
}

impl EffectsOut {
    /// The one JSON line step (1) prints, newline included.
    pub fn to_line(&self) -> String {
        todo!("tick/ledger")
    }
}

/// Picks, in order, the proposed effects that fit [`MAX_EFFECTS_PER_TICK`] and the stdout bound
/// beside a packet of `packet_bytes`, sizing each with the longest intent id. Runs before any
/// intent row is written, so every printed effect has a row and no row lacks its effect.
pub fn admit(proposed: &[EffectInput], packet_bytes: usize) -> Admitted {
    todo!("tick/ledger")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn every_input_round_trips_through_the_provider_field_names() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn admitted_effects_and_packet_never_print_past_sixty_four_kib_or_sixteen_effects() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/ledger"]
    fn an_output_whose_receipt_reaches_eight_kib_is_recorded_oversize_with_its_hash() {
        todo!()
    }
}
