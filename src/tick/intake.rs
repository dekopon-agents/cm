//! Event intake (`tick/core`): the deltas since the durable [`Cursor`], read from GitHub with
//! read-only `gh api` GETs that 11-D10's route table already opens (`pulls/{number}`,
//! `issues/{number}/comments`). The runner hands cm no event payload (11-D3 (1)), so intake polls.
//!
//! Watched PRs are the ones cm tracks: every step or part at `pr-open` with its PR URL. A step's
//! first sight at `pr-open`, a moved head, and a `@dekopon review` comment each make a review owed;
//! every other new issue comment is a verdict candidate for the gate. Recording `pr-open` stays
//! where it is today (`cm advance`, or `cm sync` once a URL is known); intake reads no PR list.
//!
//! S-tick-36, task. **Owner:** the tick's thread; each read is one `gh` child through
//! `forge::run` with `forge::GH_TIMEOUT`. **Bound:** at most [`MAX_READS_PER_TICK`] reads and
//! [`MAX_EVENTS_PER_TICK`] events per tick, taken round-robin from `Cursor::rotation`, so one
//! slow or failing PR never starves the rest. **Full queue:** the PRs not read this tick keep
//! their cursor entry and are read first next tick, counted `deferred`; no packet item.
//! **Failure:** a failed read is listed `unread` with its reason, leaves that PR's cursor as it
//! was, and never aborts the tick. **Shutdown:** nothing is in flight between ticks; the cursor
//! moves only in the tick's first commit.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::forge::{Forge, ForgeError, Gh, GithubRepo};
use crate::plan::Units;
use crate::tick::ledger::Family;

pub const CURSOR_FILE: &str = ".cm/tick/cursor.json";
pub const MAX_EVENTS_PER_TICK: usize = 200;
pub const MAX_READS_PER_TICK: usize = 120;
pub const REVIEW_COMMAND: &str = "@dekopon review";

/// One issue comment as the gate needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueComment {
    pub id: u64,
    pub author: String,
    /// `performed_via_github_app.slug`; `None` for a hand-written comment.
    pub app: Option<String>,
    pub body: String,
    pub created_at: String,
}

/// The intake reads `Forge` does not have.
pub trait PullReads {
    /// Issue comments on a PR with an id above `after`, oldest first, at most `limit`.
    fn comments_after(
        &self,
        repo: &GithubRepo,
        number: u32,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<IssueComment>, ForgeError>;
}

pub struct GhPullReads<'a> {
    pub gh: &'a Gh,
}

impl PullReads for GhPullReads<'_> {
    fn comments_after(
        &self,
        repo: &GithubRepo,
        number: u32,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<IssueComment>, ForgeError> {
        todo!("tick/core")
    }
}

/// A PR the tick watches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// `<NN-name>/<unit>/<step>`, with `part` when the step has parts.
    pub target: String,
    pub part: Option<String>,
    pub url: String,
    pub repo: GithubRepo,
    pub number: u32,
    /// From the step's lane; `None` for a lane with no reviewed family.
    pub writer: Option<Family>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// First sight of a watched PR.
    Opened {
        watched: Watched,
        head: String,
        base: String,
    },
    HeadMoved {
        watched: Watched,
        from: String,
        to: String,
        base: String,
    },
    ReviewRequested {
        watched: Watched,
        comment: u64,
        head: String,
        base: String,
    },
    /// Any other new comment: a verdict candidate.
    Comment {
        watched: Watched,
        head: String,
        comment: IssueComment,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrCursor {
    pub head: Option<String>,
    pub last_comment: Option<u64>,
}

/// Per watched PR URL, what intake already took in; plus the tick bookkeeping that must move with
/// it in one commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub prs: BTreeMap<String, PrCursor>,
    /// Where the next tick's round-robin starts.
    pub rotation: usize,
    /// `cm` stamp of the last tick that pushed its first commit: "one heartbeat ago" for the
    /// readiness rule (S-tick-63).
    pub last_tick: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Intake {
    pub events: Vec<Event>,
    /// The cursor to save in the tick's first commit.
    pub next: Cursor,
    /// Watched PRs left for the next tick by the read or event bound.
    pub deferred: usize,
    /// PR URL and why its read failed.
    pub unread: Vec<(String, String)>,
}

impl Cursor {
    /// Absent is empty.
    pub fn load(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }

    /// Written atomically; the caller adds the path to the tick's commit. A watched PR that is no
    /// longer at `pr-open` drops out, which bounds the file by the open PRs.
    pub fn save(&self, root: &Path) -> crate::tick::Result<()> {
        todo!("tick/core")
    }
}

/// Every step or part at `pr-open` with a PR URL.
pub fn watched(units: &Units) -> Vec<Watched> {
    todo!("tick/core")
}

/// Reads each watched PR (head and base through `Forge::pull`, comments through `reads`) since
/// the cursor, within the bounds. Never fails: a read error is an `unread` entry.
pub fn intake(
    forge: &dyn Forge,
    reads: &dyn PullReads,
    watched: &[Watched],
    cursor: &Cursor,
) -> Intake {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_killed_tick_leaves_the_pushed_cursor_at_the_last_tick_that_pushed() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_reloaded_cursor_takes_in_each_comment_and_head_exactly_once() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn intake_reads_at_most_its_bound_round_robin_and_a_closed_pr_drops_out_of_the_cursor() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_restart_rereads_since_the_cursor_and_owes_one_review_per_head() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_failed_read_for_one_pr_is_unread_and_every_other_pr_is_still_taken_in() {
        todo!()
    }
}
