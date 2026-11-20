//! What the tick may do, read at the approved SHA and never from the working tree (`tick/core`;
//! 11-D4; EXECUTION.md §Authority, §Gates, §Lanes "Review proof per phase", §Runner).
//!
//! One `git show <sha>:<epoch>/LIMITS.toml` yields all three tables: `[authority]`, `[gates]` and
//! `[review]`. A revision where the file or any table is missing or malformed is
//! [`crate::tick::TickError::AuthorityMissing`], and the tick applies nothing and parks with a
//! packet item. An unreachable SHA gets one `git fetch origin <sha>` first. Only `paused` is read
//! live, from the working tree; a working-tree LIMITS.toml cm cannot read counts as paused.
//!
//! Repository names are read through the rename rule: a name that is the `new` side of a pair in
//! `[authority.repository_renames]` is compared as its `old` side, on both sides of every lookup.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::collections::BTreeMap;
use std::path::Path;

use crate::forge::CheckState;
use crate::plan::Units;
use crate::tick::ledger::Family;
use crate::tick::push::Git;

/// A full 40-hex, lower-case commit SHA from the runner entry (`--approved`); never EPOCH.md's
/// `approved_plan_revision`, which sits on the main the agent can push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedSha(String);

impl ApprovedSha {
    /// Refuses anything but 40 lower-case hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        todo!("tick/core")
    }

    pub fn as_str(&self) -> &str {
        todo!("tick/core")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    Patch,
    Minor,
    /// Never in `release_bumps` (a major or 1.0 release is not authorized), so always refused.
    Major,
}

/// `[authority]` as the stamp froze it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    pub merge: bool,
    pub merge_excluded_repos: Vec<String>,
    pub release_repos: Vec<String>,
    pub release_bumps: Vec<Bump>,
    pub homelab_deploy: bool,
    pub live_defect_fixes: bool,
    /// `old = new`.
    pub repository_renames: BTreeMap<String, String>,
}

impl Authority {
    /// The name a repository is compared under: a pair's `new` reads as its `old`.
    pub fn canonical<'a>(&'a self, repo: &'a str) -> &'a str {
        todo!("tick/core")
    }

    pub fn may_merge(&self, repo: &str) -> bool {
        todo!("tick/core")
    }

    pub fn may_release(&self, repo: &str, bump: Bump) -> bool {
        todo!("tick/core")
    }
}

/// `[gates]`: required check names per repository, and `[gates.provider]`, the default for a
/// `release_repos` member with no entry of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gates {
    pub entries: BTreeMap<String, Vec<String>>,
    pub provider: Vec<String>,
}

impl Gates {
    /// §Gates "Which entry applies": the repository's own entry, else the provider default when
    /// it is in `release_repos`, else `None`, which is red for every head.
    pub fn required(&self, authority: &Authority, repo: &str) -> Option<&[String]> {
        todo!("tick/core")
    }
}

/// dekopon's classifier: it never counts as the one `success` the merge rule needs.
pub const CLASSIFIER_CHECK: &str = "classify CI changes";

/// The §Gates merge rule at one head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecksRead {
    Green,
    /// Every reason: a named check absent, zero checks, a check not `success`, `skipped` or
    /// `neutral`, or no `success` besides [`CLASSIFIER_CHECK`].
    Red {
        why: Vec<String>,
    },
}

/// Every named check present; every check at the head `success`, `skipped` or `neutral`; at least
/// one `success` besides the classifier. Zero checks is red. `forge::ci_green` stays the
/// `cm sync` rule; the tick and `07-tick/gate-merge`'s gate use this one.
pub fn checks_green(required: &[String], checks: &[CheckState]) -> ChecksRead {
    todo!("tick/core")
}

/// A `[review.<phase>]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Bootstrap,
    Interim,
    GptRoute,
}

/// §Lanes `proof`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofRule {
    /// `astra-receipt+review-record`: the Astra receipt and the dekopon-review record.
    AstraReceiptPlusRecord,
    /// `astra-receipt`: the Astra half alone, ledger-bound.
    AstraReceipt,
    /// `app-comment`: a nonce-bound PR issue comment from this App (its slug, or the slug the
    /// named owner step's evidence records).
    AppComment { app: String },
}

/// One writer family's row in a phase table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterProof {
    pub reviewer: Option<Family>,
    pub proof: ProofRule,
    /// `escalate = "opus"`: a contract or auth surface, or an unsure report, goes to Opus.
    pub escalate: Option<String>,
}

/// `[review]`, every phase frozen at the stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewTable {
    pub phases: BTreeMap<Phase, PhaseTable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseTable {
    /// Steps that must be `done` or `deployed` in cm before the phase starts.
    pub starts_after: Vec<String>,
    /// Steps whose evidence lists the repositories where the phase's App is installed.
    pub installed: Vec<String>,
    pub codex: WriterProof,
    pub claude: WriterProof,
}

impl ReviewTable {
    /// §Lanes selection rule for a PR in `repo`: bootstrap until the interim's steps are done;
    /// then gpt-route where its steps are done and `repo` is in a recorded installation list;
    /// otherwise interim. Reads cm state, never LIMITS live.
    pub fn phase_for(
        &self,
        root: &Path,
        units: &Units,
        authority: &Authority,
        repo: &str,
    ) -> Phase {
        todo!("tick/core")
    }

    /// The writer family's row in that phase.
    pub fn proof(&self, phase: Phase, writer: Family) -> &WriterProof {
        todo!("tick/core")
    }
}

/// The three tables at one SHA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approved {
    pub sha: ApprovedSha,
    pub authority: Authority,
    pub gates: Gates,
    pub review: ReviewTable,
}

impl Approved {
    /// `git show <sha>:<campaign>/LIMITS.toml`, `campaign` relative to the checkout; one
    /// `git fetch origin <sha>` when the SHA is not local.
    pub fn read(
        git: &dyn Git,
        checkout: &Path,
        campaign: &Path,
        sha: &ApprovedSha,
    ) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }
}

/// `paused` from the working tree's LIMITS.toml, read on every tick; unreadable reads as paused.
pub fn paused_live(root: &Path) -> bool {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn authority_is_read_at_the_approved_sha_and_never_from_the_working_tree() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn an_unreachable_sha_is_fetched_once_then_the_tick_applies_nothing_and_parks() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn either_name_of_a_rename_selects_the_same_release_gate_and_phase_entries() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn paused_read_live_stops_every_launch_and_effect_and_an_unreadable_limits_reads_paused() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_missing_named_check_zero_checks_or_a_lone_classifier_success_is_red() {
        todo!()
    }
}
