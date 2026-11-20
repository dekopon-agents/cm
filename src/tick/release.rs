//! Releases from the RPi coordinator (`tick/release-lan`): only through cm's release step and each
//! repository's Cut release dispatch (11-D14), the bump checked against the last tag, zero
//! required checks red, and at most three counting merges on core `dekopon` per open drop.

#![allow(
    unused_variables,
    reason = "skeleton: tick/release-lan fills these bodies"
)]

use std::path::Path;

use crate::forge::Forge;
use crate::tick::authority::{Authority, Bump};
use crate::tick::ledger::IntentId;
use crate::tick::run::Cycle;
use crate::tick::wire::Effect;

/// PLAN.md §Releases: the open drop's `RELEASE.md` tally cap on core `dekopon`.
pub const CORE_DROP_CAP: u32 = 3;
pub const CORE_REPO: &str = "dekopon-agents/dekopon";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub repo: String,
    pub tag: String,
    pub sha: String,
    pub bump: Bump,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseRefusal {
    NotAReleaseRepo,
    BumpNotAuthorized { bump: Bump },
    BumpDoesNotFollowLastTag { last: String, next: String },
    ZeroRequiredChecks,
    ChecksRed,
    DropTallyFull { tally: u32 },
    OutOfAuthority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    Cut(Cut),
    Refused(ReleaseRefusal),
}

/// The open drop's counting merges, read from its `RELEASE.md` (the bootstrap coordinator keeps
/// it), so the cap holds across restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropTally {
    pub drop: String,
    pub counting_merges: u32,
}

impl DropTally {
    pub fn read(root: &Path) -> crate::tick::Result<Option<Self>> {
        todo!("tick/release-lan")
    }

    /// False for a fourth counting merge on [`CORE_REPO`].
    pub fn admits(&self, repo: &str) -> bool {
        todo!("tick/release-lan")
    }
}

/// Plans one release step's cut from its declared version and the repository's last tag.
pub fn plan(
    authority: &Authority,
    forge: &dyn Forge,
    root: &Path,
    target: &str,
) -> crate::tick::Result<Planned> {
    todo!("tick/release-lan")
}

/// The `gh.workflow.run` effect for a planned cut.
pub fn cut_effect(intent: &IntentId, cut: &Cut, workflow_id: u64) -> Effect {
    todo!("tick/release-lan")
}

/// Stage 6 of [`crate::tick::run`]: plan cuts for release steps whose prep PR merged.
pub fn stage(cycle: &mut Cycle) -> crate::tick::Result<()> {
    todo!("tick/release-lan")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_cut_interrupted_after_dispatch_is_bound_by_its_tag_and_never_dispatched_twice() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_reloaded_tally_is_the_release_md_tally_not_a_count_in_memory() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn a_fourth_counting_dekopon_merge_is_refused_while_the_drop_holds_three() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/release-lan"]
    fn restart_still_refuses_the_fourth_counting_merge_and_zero_required_checks() {
        todo!()
    }
}
