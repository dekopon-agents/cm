//! Releases from the RPi coordinator (`tick/release-lan`; PLAN.md `07-tick/release-lan`, §Releases;
//! 11-D14; EXECUTION.md §Train).
//!
//! A release is cut only through cm's own release step (`00-release/<line>/<version>`, ready in
//! `cm next`) and the repository's `Cut release` dispatch, `gh.workflow.run` on `cut-release.yml`'s
//! numeric id with `ref: main` and inputs `tags`, `sha`, `dry_run`. Before planning a cut: the
//! repository is in `release_repos` (rename rule applied), the bump from the last tag to the
//! declared version is one `release_bumps` allows, and main's head has its §Gates checks green
//! (zero required checks is red). `dry_run` comes from `Approved::release_mode` only (R10, G10). A
//! refused dispatch becomes a MAC-LANE item with the dry-run id at once.
//!
//! **Dispatch evidence (R10).** The dispatch response carries no run id, and `cut-release.yml`
//! sets no `run-name` (cm `.github/workflows/cut-release.yml` at `0e38f02`), so a run cannot be tied
//! to its tag. A cut intent binds only by its tag at its SHA. A dispatch run of the cut workflow
//! created after the intent's emission (`ReleaseReads::dispatch_runs`; its route is G3's) shows
//! that some dispatch happened, and goes into the parked item as evidence. A cut with no receipt
//! and no tag parks either way; tag absence never reads as "not dispatched". A `run-name` naming
//! the tags in each `cut-release.yml` would let a run bind; that is outside these units (G12).
//! The workflow's `dry_run` input defaults to `true`, so [`cut_input`] always sends it.
//!
//! **The drop cap (R09; PLAN §Releases).** On core `dekopon` at most [`CORE_DROP_CAP`] counting
//! merges per open drop; the tally is the open drop's `RELEASE.md`, one row per counting merge.
//! The merge stage calls [`drop_admits`] with the PR and whether it is the drop's release prep
//! (the PR of a `00-release/dekopon/…` step), then [`reserve`] writes the counting PR's row before
//! the merge intent, in the tick's first commit, so two candidates in one tick cannot both pass a
//! tally of two. The prep counts nothing; while a prep PR is open, no other core PR is admitted
//! (the prep hold). Forming a drop, declaring its release steps and opening its `RELEASE.md` stay
//! the coordinator's (PLAN §Releases "Forming a drop"); with no open drop, a core merge waits.

#![allow(
    unused_variables,
    reason = "skeleton: tick/release-lan fills these bodies"
)]

use std::path::{Path, PathBuf};

use crate::forge::{ForgeError, Gh, GithubRepo, TagName};
use crate::tick::authority::{Authority, Bump};
use crate::tick::ledger::{IntentRow, Observation, Readmit};
use crate::tick::run::{Cycle, Deps};
use crate::tick::wire::{Effect, Receipt, WorkflowRunInput};

/// PLAN.md §Releases.
pub const CORE_DROP_CAP: u32 = 3;
pub const CORE_REPO: &str = "dekopon-agents/dekopon";
pub const CUT_WORKFLOW_FILE: &str = "cut-release.yml";

/// The release reads `Forge` does not have.
pub trait ReleaseReads {
    /// The newest tag under `prefix` (`git/matching-refs/tags/{prefix}`, every page), by version.
    fn last_tag(&self, repo: &GithubRepo, prefix: &str) -> Result<Option<TagName>, ForgeError>;
    /// The numeric id of `.github/workflows/<file>` (G3: not in 11-D10's route table).
    fn workflow_id(&self, repo: &GithubRepo, file: &str) -> Result<u64, ForgeError>;
    /// `workflow_dispatch` runs of a workflow created at or after an instant, newest first (G3:
    /// `actions/workflows/{id}/runs` is not in 11-D10's route table either).
    fn dispatch_runs(
        &self,
        repo: &GithubRepo,
        workflow: u64,
        since: &str,
    ) -> Result<Vec<DispatchRun>, ForgeError>;
    /// main's head SHA (`branches/{branch}`).
    fn branch_head(&self, repo: &GithubRepo, branch: &str) -> Result<String, ForgeError>;
}

/// One dispatch run, as `dispatch_runs` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRun {
    pub id: u64,
    pub created_at: String,
    /// `queued`, `in_progress` or `completed`.
    pub status: String,
    pub conclusion: Option<String>,
    /// The run's display title: the workflow name today, since `cut-release.yml` has no
    /// `run-name`.
    pub title: String,
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

    fn dispatch_runs(
        &self,
        repo: &GithubRepo,
        workflow: u64,
        since: &str,
    ) -> Result<Vec<DispatchRun>, ForgeError> {
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
    /// From `Approved::release_mode`, never from the step or a ruling.
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
    /// The drop's `RELEASE.md`, relative to the campaign folder.
    pub file: PathBuf,
    pub counting_merges: u32,
    /// The release prep PR, while one is open.
    pub prep_open: Option<u32>,
}

/// What [`drop_admits`] says about one PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropAdmission {
    /// Not core, or the prep itself: merge without a tally row.
    Exempt,
    /// A counting merge with room left: [`reserve`] its row first.
    Count { tally: DropTally },
    /// Three rows, the prep hold, or no open drop: wait, with the reason.
    Hold { why: String },
}

/// One `RELEASE.md` row (PLAN §Releases: PR, unit, head, enqueue time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TallyRow {
    pub pr: u32,
    pub unit: String,
    pub head: String,
    pub at: String,
}

impl DropTally {
    /// The open drop's tally; `None` when no drop is open.
    pub fn read(root: &Path) -> crate::tick::Result<Option<Self>> {
        todo!("tick/release-lan")
    }
}

/// PR-aware admission for core merges; `Exempt` for every other repository.
pub fn drop_admits(
    root: &Path,
    authority: &Authority,
    repo: &str,
    pr: u32,
    prep: bool,
) -> crate::tick::Result<DropAdmission> {
    todo!("tick/release-lan")
}

/// Appends the counting PR's row to the drop's `RELEASE.md` and returns the path for
/// `Cycle::wrote`, so the row reaches main in the same commit as the merge intent.
pub fn reserve(root: &Path, tally: &DropTally, row: &TallyRow) -> crate::tick::Result<PathBuf> {
    todo!("tick/release-lan")
}

/// Plans one ready release step's cut from its declared version and the repository's last tag.
pub fn plan(cycle: &Cycle, target: &str) -> CutPlan {
    todo!("tick/release-lan")
}

/// Always sends `dry_run`, `"true"` or `"false"`: the workflow's default is `true`.
pub fn cut_input(cut: &Cut, workflow: u64) -> WorkflowRunInput {
    todo!("tick/release-lan")
}

/// A cut intent's lookup (module doc, Dispatch evidence): the tag at the intent's SHA binds;
/// otherwise undecidable, naming any dispatch run created after the emission; a failed read is
/// unread. Never `NotDone`.
pub fn observe(deps: &Deps, row: &IntentRow) -> Observation {
    todo!("tick/release-lan")
}

/// R05: a cut is emitted again only on a `retry` ruling, while the release step is still ready,
/// the bump still allowed and main's checks still green.
pub fn readmit(cycle: &Cycle, row: &IntentRow) -> Readmit {
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

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn two_candidates_at_a_tally_of_two_reserve_one_row_and_the_prep_is_exempt_but_holds() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn an_unconfirmed_dispatch_parks_and_is_never_inferred_from_a_missing_tag() {
        todo!()
    }
}
