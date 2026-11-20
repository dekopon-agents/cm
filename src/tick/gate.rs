//! The verdict gate (`tick/gate-merge`; IDEA-0063 §What counts as a verdict; EXECUTION.md
//! §Lanes, §Verdict grammar).
//!
//! A comment verdict is a PR issue comment whose whole body is a `cm-review` report with a
//! `nonce:` line and a 40-hex `head:`, whose `performed_via_github_app` slug is the phase's App,
//! whose nonce names a review intent in the ledger for this PR, head and the reviewing family, and
//! whose nonce no earlier Verdict row consumed. While the phase's proof names Astra, a GPT verdict
//! is instead the Astra `receipt.json` (finished, gpt-6-astra/xhigh, this PR and head) bound to the
//! recorded report whose SHA-256 it names and to a review intent in the ledger. Binding writes one
//! Verdict row; that row is what consumes the nonce. A refusal is data that reaches the packet.
//!
//! The tick binds only at the PR's current head: it reads no checkout of the PR's repository, so
//! `verdict::merge_gaps`'s clean-update walk (`verdict::clean_update`) stays a `cm advance` and
//! `cm sync` path, and a head moved after review gets a fresh review (PLAN.md `07-tick/gate-merge`
//! (1); SKELETON.md, Choices).
//!
//! `tick/gate-merge` also changes three existing files, and only it does: `src/verdict.rs` (parse
//! the `nonce:` line into `Block::nonce`; `merge_gaps` takes the phase's [`ProofRule`]; the RULING
//! text for the third FIX REQUIRED), `src/commands.rs` (`cm advance`'s gate passes that rule) and
//! `src/sync.rs` (`gate_holds` applies it to every reviewed lane, not only codex).

#![allow(
    unused_variables,
    reason = "skeleton: tick/gate-merge fills these bodies"
)]

use std::path::Path;

use crate::store::Target;
use crate::tick::authority::{ProofRule, ReviewTable};
use crate::tick::intake::IssueComment;
use crate::tick::ledger::{EffectLedger, Family, IntentId, Nonce, VerdictProof};
use crate::verdict::Verdict;

/// EXECUTION.md §Stops: a reviewer's third FIX REQUIRED on one PR blocks the lane.
pub const REVIEW_BUDGET: usize = 3;

/// App slugs from the review table at the approved SHA, never from PR content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedApps {
    /// The App a Claude reviewer posts as (`claude`).
    pub claude: String,
    /// The reviewer App a GPT reviewer posts as, once `04-owner/reviewer-app` records it.
    pub gpt: Option<String>,
}

impl TrustedApps {
    pub fn from_table(root: &Path, table: &ReviewTable) -> Self {
        todo!("tick/gate-merge")
    }
}

/// The frozen grammar's fields, as `verdict::parse_report` returns them with `Block::nonce`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printed {
    pub nonce: Nonce,
    pub head: String,
    pub stated: Verdict,
    /// `Block::verdict`, from the findings.
    pub computed: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundVerdict {
    /// The review intent whose nonce it carried.
    pub intent: IntentId,
    pub repo: String,
    pub pr: u32,
    pub head: String,
    pub reviewer: Family,
    pub word: Verdict,
    pub proof: VerdictProof,
}

/// Every must-not-merge case in `tick/gate-merge`'s done check, and the grammar's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No App origin: a hand-written comment.
    HandWritten,
    OtherApp {
        app: String,
    },
    NoNonce,
    UnmatchedNonce,
    ReplayedNonce,
    SameFamily,
    /// The verdict is for a head the PR has moved past.
    OldHead {
        bound: String,
        current: String,
    },
    /// The printed head is not the head the nonce's intent was launched for.
    PrintedHeadDiffers {
        printed: String,
        bound: String,
    },
    /// Fewer than 40 hex digits.
    ShortHead {
        printed: String,
    },
    /// No verdict word after the block: "unsure", which goes to Opus, never merges.
    NoWord,
    /// The stated word is not the one the findings compute.
    WordDisagrees {
        stated: Verdict,
        computed: Verdict,
    },
    /// An Astra receipt with no review intent, or one for another PR or family.
    NoLedgerRow,
    ReceiptHeadDiffers {
        receipt: String,
        bound: String,
    },
    ReceiptUnfinished {
        why: String,
    },
    /// No recorded Astra report hashes to the receipt's `report_sha256`.
    ReportUnrecorded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gated {
    Bound(BoundVerdict),
    Refused(Refusal),
    /// Not a `cm-review` report at all; ignored.
    NotAVerdict,
}

/// The grammar check alone; `None` for a comment with no `cm-review` block.
pub fn parse_comment(body: &str) -> Option<Result<Printed, Refusal>> {
    todo!("tick/gate-merge")
}

/// Binds an issue comment on `repo#pr` against the ledger and the PR's current head.
pub fn bind_comment(
    trusted: &TrustedApps,
    ledger: &EffectLedger,
    comment: &IssueComment,
    repo: &str,
    pr: u32,
    current_head: &str,
    writer: Family,
) -> Gated {
    todo!("tick/gate-merge")
}

/// Binds the Astra receipts under the step's evidence for `current_head`.
pub fn bind_astra(
    root: &Path,
    ledger: &EffectLedger,
    target: &Target,
    repo: &str,
    pr: u32,
    current_head: &str,
    writer: Family,
) -> Vec<Gated> {
    todo!("tick/gate-merge")
}

/// FIX REQUIRED verdicts bound on this PR from this reviewer family, every head counted.
pub fn fix_required_count(ledger: &EffectLedger, repo: &str, pr: u32, reviewer: Family) -> usize {
    todo!("tick/gate-merge")
}

/// What the phase's proof still lacks at `head`, for `cm advance` and `cm sync`: the existing
/// Astra-plus-record gaps for the bootstrap rule, the Astra half for `astra-receipt`, and a bound
/// App comment verdict for `app-comment`. Empty when the proof holds.
pub fn proof_gaps(
    root: &Path,
    target: &Target,
    part: Option<&str>,
    pr: Option<&str>,
    head: &str,
    rule: &ProofRule,
) -> anyhow::Result<Vec<String>> {
    todo!("tick/gate-merge")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_tick_killed_mid_bind_binds_nothing_until_its_verdict_row_is_pushed() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_reloaded_ledger_still_refuses_a_replayed_nonce() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn one_nonce_binds_at_most_one_verdict_and_a_third_fix_required_stops_the_lane() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_restart_still_refuses_every_must_not_merge_comment() {
        todo!()
    }
}
