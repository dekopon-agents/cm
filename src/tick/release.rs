//! Releases from the RPi coordinator (`tick/release-lan`; PLAN.md `07-tick/release-lan`, §Releases;
//! 11-D14; EXECUTION.md §Train).
//!
//! A release is cut only through cm's own release step (`00-release/<line>/<version>`, ready in
//! `cm next`) and the repository's `Cut release` dispatch, `gh.workflow.run` on `cut-release.yml`'s
//! numeric id with `ref: main` and inputs `tags`, `sha`, `dry_run`. Before planning a cut: the
//! repository is in `release_repos` (rename rule applied), the bump from the last tag to the
//! declared version is one `release_bumps` allows, and main's head has its §Gates checks green
//! (zero required checks is red). A refused dispatch becomes a MAC-LANE item with the dry-run id at
//! once. The tag's identity binds the intent: the next `cm sync` moves the step to `released`.
//!
//! The drop cap: on core `dekopon`, at most [`CORE_DROP_CAP`] counting merges per open drop, read
//! from that drop's `RELEASE.md` tally every time, never kept in memory, so it holds across
//! restarts. `merge::route` asks [`drop_admits`] before every merge on [`CORE_REPO`].

#![allow(
    unused_variables,
    reason = "skeleton: tick/release-lan fills these bodies"
)]

use std::path::Path;

use crate::forge::{ForgeError, Gh, GithubRepo, TagName};
use crate::tick::authority::{Authority, Bump};
use crate::tick::ledger::{IntentRow, Observation};
use crate::tick::run::{Cycle, Deps};
use crate::tick::wire::{Effect, Receipt, WorkflowRunInput};

/// PLAN.md §Releases.
pub const CORE_DROP_CAP: u32 = 3;
pub const CORE_REPO: &str = "dekopon-agents/dekopon";
pub const CUT_WORKFLOW_FILE: &str = "cut-release.yml";

/// The release reads `Forge` does not have.
pub trait ReleaseReads {
    /// The newest tag under `prefix` (`git/matching-refs/tags/{prefix}`), by version.
    fn last_tag(&self, repo: &GithubRepo, prefix: &str) -> Result<Option<TagName>, ForgeError>;
    /// The numeric id of `.github/workflows/<file>` (SKELETON.md, Named gaps: not in 11-D10's route
    /// table).
    fn workflow_id(&self, repo: &GithubRepo, file: &str) -> Result<u64, ForgeError>;
    /// main's head SHA (`branches/{branch}`).
    fn branch_head(&self, repo: &GithubRepo, branch: &str) -> Result<String, ForgeError>;
}

pub struct GhReleaseReads<'a> {
    pub gh: &'a Gh,
}

impl ReleaseReads for GhReleaseReads<'_> {
    fn last_tag(&self, repo: &GithubRepo, prefix: &str) -> Result<Option<TagName>, ForgeError> {
        todo!("tick/release-lan")
    }

    fn workflow_id(&self, repo: &GithubRepo, file: &str) -> Result<u64, ForgeError> {
        todo!("tick/release-lan")
    }

    fn branch_head(&self, repo: &GithubRepo, branch: &str) -> Result<String, ForgeError> {
        todo!("tick/release-lan")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub target: String,
    pub repo: String,
    pub tag: String,
    pub sha: String,
    pub bump: Bump,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseRefusal {
    NotAReleaseRepo,
    BumpNotAuthorized {
        bump: Bump,
    },
    BumpDoesNotFollowLastTag {
        last: String,
        next: String,
    },
    ZeroRequiredChecks,
    ChecksRed {
        why: Vec<String>,
    },
    /// `paused`, or a class `[authority]` does not grant.
    OutOfAuthority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutPlan {
    Cut(Cut),
    Refused(ReleaseRefusal),
    Wait { why: String },
}

/// The open drop's counting merges on core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropTally {
    pub drop: String,
    pub counting_merges: u32,
}

impl DropTally {
    /// The open drop's `RELEASE.md` tally; `None` when no drop is open.
    pub fn read(root: &Path) -> crate::tick::Result<Option<Self>> {
        todo!("tick/release-lan")
    }
}

/// False for a fourth counting merge on [`CORE_REPO`] while the open drop's tally holds three,
/// and for any core merge while no drop is open; true for every other repository.
pub fn drop_admits(root: &Path, authority: &Authority, repo: &str) -> crate::tick::Result<bool> {
    todo!("tick/release-lan")
}

/// Plans one ready release step's cut from its declared version and the repository's last tag.
pub fn plan(cycle: &Cycle, target: &str) -> CutPlan {
    todo!("tick/release-lan")
}

pub fn cut_input(cut: &Cut, workflow: u64) -> WorkflowRunInput {
    todo!("tick/release-lan")
}

/// A cut intent's lookup: the tag exists and points at the intent's SHA binds; absent with no
/// dispatch run for it is not done; a dispatch run with no tag yet is undecidable.
pub fn observe(deps: &Deps, row: &IntentRow) -> Observation {
    todo!("tick/release-lan")
}

/// The local applier's dispatch (`POST …/actions/workflows/{id}/dispatches`): 403 or 422 is
/// `refused`, which files the MAC-LANE item.
pub fn apply_local(gh: &Gh, effect: &Effect) -> Receipt {
    todo!("tick/release-lan")
}

/// Stage 7 of `run`, first half: plan cuts for ready release steps; file the MAC-LANE item for a
/// refused dispatch.
pub fn stage(cycle: &mut Cycle) -> crate::tick::Result<()> {
    todo!("tick/release-lan")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_cut_killed_after_dispatch_is_bound_by_its_tag_and_never_dispatched_twice() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_reloaded_tally_is_the_release_md_tally_not_a_count_in_memory() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_fourth_counting_core_merge_is_refused_while_the_drop_holds_three() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_restart_still_refuses_the_fourth_counting_merge_and_a_cut_at_zero_required_checks() {
        todo!()
    }
}
