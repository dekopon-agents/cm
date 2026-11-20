//! `[authority]` and `[gates]` read at the approved SHA, never the working tree (11-D4; EXECUTION.md
//! §Runner). Only `paused` is read live. `tick/core` owns it.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::collections::BTreeMap;
use std::path::Path;

use crate::tick::push::Git;

/// A full 40-hex commit SHA from the runner entry (`--approved`); never EPOCH.md's pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedSha(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    Patch,
    Minor,
    Major,
}

/// The table as the stamp froze it. Repository keys are `owner/name`; renames map old to new and
/// either name selects the same entries (EXECUTION.md §Authority, Rename rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    pub sha: ApprovedSha,
    pub merge: bool,
    pub merge_excluded_repos: Vec<String>,
    pub release_repos: Vec<String>,
    pub release_bumps: Vec<Bump>,
    pub homelab_deploy: bool,
    pub live_defect_fixes: bool,
    pub repository_renames: BTreeMap<String, String>,
    /// `[gates."owner/name"]`: the required check names; zero present at a head is red.
    pub gates: BTreeMap<String, Vec<String>>,
}

impl ApprovedSha {
    /// Refuses anything but 40 lower-case hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        todo!("tick/core")
    }

    pub fn as_str(&self) -> &str {
        todo!("tick/core")
    }
}

impl Authority {
    /// `git show <sha>:<epoch>/LIMITS.toml`; an unreachable SHA gets one `git fetch origin <sha>`.
    /// Unreachable after it, or a revision without the file or the table, is an error the tick
    /// turns into "apply nothing, park with a packet".
    pub fn read(
        git: &dyn Git,
        checkout: &Path,
        epoch: &Path,
        sha: &ApprovedSha,
    ) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }

    /// The name a repository is compared under: a rename's `new` side.
    pub fn canonical<'a>(&'a self, repo: &'a str) -> &'a str {
        todo!("tick/core")
    }

    pub fn may_merge(&self, repo: &str) -> bool {
        todo!("tick/core")
    }

    pub fn may_release(&self, repo: &str, bump: Bump) -> bool {
        todo!("tick/core")
    }

    /// `None` when the repository has no `[gates]` entry, which holds every merge there.
    pub fn required_checks(&self, repo: &str) -> Option<&[String]> {
        todo!("tick/core")
    }
}

/// `paused` from the working tree's LIMITS.toml: the kill switch, read live on every tick.
pub fn paused_live(root: &Path) -> crate::tick::Result<bool> {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn authority_is_read_at_the_approved_sha_and_never_from_the_worktree() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn an_unreachable_sha_is_fetched_once_then_the_tick_parks_with_nothing_applied() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn either_name_of_a_rename_selects_the_same_gate_and_release_entries() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn paused_read_live_stops_every_effect_even_when_the_sha_allows_it() {
        todo!()
    }
}
