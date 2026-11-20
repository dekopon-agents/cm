//! Event intake (`tick/core`): the deltas since the durable cursor, from read-only GitHub reads
//! (11-D10). A PR opening and an `@dekopon review` comment each request a review.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Relative to the campaign folder.
pub const CURSOR_FILE: &str = ".cm/tick-cursor.json";
/// Growth bound: events one tick takes in; the rest stay behind the cursor for the next tick.
pub const MAX_EVENTS_PER_TICK: usize = 200;
pub const REVIEW_COMMAND: &str = "@dekopon review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PrOpened {
        repo: String,
        number: u64,
        head: String,
    },
    ReviewRequested {
        repo: String,
        number: u64,
        comment_id: u64,
    },
    HeadMoved {
        repo: String,
        number: u64,
        from: String,
        to: String,
    },
    ChecksCompleted {
        repo: String,
        sha: String,
    },
    /// An issue comment that may be a verdict; the gate decides.
    Comment {
        repo: String,
        number: u64,
        comment_id: u64,
        app: Option<String>,
        body: String,
    },
    Merged {
        repo: String,
        number: u64,
        merge_sha: String,
    },
}

/// Per watched repository, the newest event already taken in. Advanced only after the tick's
/// ledger push succeeds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub repos: BTreeMap<String, String>,
}

/// The reads intake needs, so tests replay fixtures.
pub trait Events {
    fn since(
        &self,
        repo: &str,
        after: Option<&str>,
        limit: usize,
    ) -> crate::tick::Result<(Vec<Event>, Option<String>)>;
}

/// `gh api` GETs with the forge's timeout.
pub struct GhEvents {
    pub gh: crate::forge::Gh,
}

impl Events for GhEvents {
    fn since(
        &self,
        repo: &str,
        after: Option<&str>,
        limit: usize,
    ) -> crate::tick::Result<(Vec<Event>, Option<String>)> {
        todo!("tick/core")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Intake {
    pub events: Vec<Event>,
    /// The cursor to save once the tick's push lands.
    pub next: Cursor,
    /// Events left for the next tick by [`MAX_EVENTS_PER_TICK`].
    pub deferred: usize,
}

impl Cursor {
    pub fn load(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }

    pub fn save(&self, root: &Path) -> crate::tick::Result<()> {
        todo!("tick/core")
    }
}

/// Reads every watched repository since the cursor, deduplicated by event identity.
pub fn intake(
    events: &dyn Events,
    repos: &[String],
    cursor: &Cursor,
) -> crate::tick::Result<Intake> {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn an_aborted_tick_leaves_the_cursor_at_the_last_pushed_tick() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_reloaded_cursor_takes_in_each_event_exactly_once() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn intake_takes_at_most_two_hundred_events_and_counts_the_rest_deferred() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn restart_rereads_since_the_durable_cursor_and_requests_one_review_per_event() {
        todo!()
    }
}
