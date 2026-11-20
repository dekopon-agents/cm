//! The verdict gate (`tick/gate-merge`; IDEA-0063 §What counts as a verdict). A verdict is a PR
//! issue comment whose App origin matches trusted configuration, whose nonce matches an intent in
//! the ledger, for the current head, from the family that did not write the PR. While LIMITS
//! names Astra, a GPT verdict is the ledger-bound Astra receipt recorded with `cm review record`.
//! A refusal is data that reaches the packet, never an error.

#![allow(
    unused_variables,
    reason = "skeleton: tick/gate-merge fills these bodies"
)]

use crate::tick::intake::Event;
use crate::tick::ledger::{EffectLedger, Family, IntentId, Nonce};

/// App slugs pinned in trusted configuration, never in writer-controlled PR content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedApps {
    /// `claude`.
    pub claude: String,
    /// The dedicated Dekopon reviewer App.
    pub gpt: String,
}

/// Which proof LIMITS names for the phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofRule {
    /// Bootstrap: Astra receipt plus the dekopon-review record.
    Bootstrap,
    /// After the cutover: the App-origin, ledger-bound comment.
    AppComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Accept,
    FixRequired,
}

/// The frozen verdict grammar (EXECUTION.md): nonce, printed head, verdict word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printed {
    pub nonce: Nonce,
    pub head: String,
    pub word: Word,
}

/// `cm review record`'s receipt facts the gate binds (receipt `head`, `report_sha256`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstraReceipt {
    pub target: String,
    pub pr: u64,
    pub head: String,
    pub report_sha256: String,
    pub word: Word,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proof {
    Comment { comment_id: u64, app: String },
    Astra { report_sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundVerdict {
    pub intent: IntentId,
    pub repo: String,
    pub pr: u64,
    pub head: String,
    pub reviewer: Family,
    pub word: Word,
    pub proof: Proof,
}

/// Every must-not-merge case `tick/gate-merge`'s done check names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No App origin: a hand-written comment.
    HandWritten,
    OtherApp {
        app: String,
    },
    UnmatchedNonce,
    ReplayedNonce,
    SameFamily,
    OldHead {
        bound: String,
        current: String,
    },
    PrintedHeadDiffers {
        printed: String,
        bound: String,
    },
    /// A receipt with no ledger row, or a row for another PR or family.
    NoLedgerRow,
    ReceiptHeadDiffers {
        receipt: String,
        bound: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gated {
    Bound(BoundVerdict),
    Refused(Refusal),
    /// Not a verdict comment at all; ignored.
    NotAVerdict,
}

/// Parses the frozen grammar; `None` for any other comment.
pub fn parse(body: &str) -> Option<Printed> {
    todo!("tick/gate-merge")
}

/// Binds a [`Event::Comment`] against the ledger and the PR's current head.
pub fn bind_comment(
    trusted: &TrustedApps,
    ledger: &EffectLedger,
    comment: &Event,
    current_head: &str,
    writer: Family,
) -> Gated {
    todo!("tick/gate-merge")
}

/// Binds a recorded Astra receipt to its ledger row's PR, head and family.
pub fn bind_receipt(
    ledger: &EffectLedger,
    receipt: &AstraReceipt,
    current_head: &str,
    writer: Family,
) -> Gated {
    todo!("tick/gate-merge")
}

/// The proof the phase takes, from the LIMITS reviewer table at the approved SHA.
pub fn proof_rule(limits_toml: &str) -> ProofRule {
    todo!("tick/gate-merge")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_verdict_read_mid_bind_binds_nothing_until_its_bound_row_is_written() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn a_reloaded_ledger_still_refuses_a_replayed_nonce() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn one_nonce_binds_at_most_one_verdict() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/gate-merge"]
    fn restart_refuses_hand_written_other_app_unmatched_same_family_old_and_misprinted_heads() {
        todo!()
    }
}
