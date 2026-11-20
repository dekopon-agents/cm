//! Timers and retries (`tick/core`): due work that is not an event. Durable in
//! `.cm/timers.json`, keyed so re-arming is idempotent.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::tick::ledger::IntentId;

pub const TIMERS_FILE: &str = ".cm/timers.json";
/// Growth bound: armed timers; arming past it parks the arming unit.
pub const MAX_TIMERS: usize = 256;
/// A retried intent parks after this many tries.
pub const MAX_TRIES: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TimerKind {
    /// Re-emit a known-not-done intent after backoff.
    Retry { intent: IntentId },
    /// A review launched with this nonce has not posted by the deadline.
    ReviewDeadline { target: String, intent: IntentId },
    /// A launched step has had no attempt activity for the LIMITS stall hours.
    Stall { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timer {
    /// Unique; arming an existing key moves its due time.
    pub key: String,
    /// `cm` stamp.
    pub due: String,
    pub tries: u32,
    #[serde(flatten)]
    pub kind: TimerKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timers {
    pub timers: Vec<Timer>,
}

impl Timers {
    pub fn load(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }

    pub fn save(&self, root: &Path) -> crate::tick::Result<()> {
        todo!("tick/core")
    }

    /// Refuses past [`MAX_TIMERS`].
    pub fn arm(&mut self, timer: Timer) -> crate::tick::Result<()> {
        todo!("tick/core")
    }

    pub fn disarm(&mut self, key: &str) -> Option<Timer> {
        todo!("tick/core")
    }

    /// Removes and returns the timers due at `now` (a `cm` stamp), oldest first.
    pub fn take_due(&mut self, now: &str) -> Vec<Timer> {
        todo!("tick/core")
    }
}

/// Exponential, capped: the delay before try `tries + 1`.
pub fn backoff(tries: u32) -> Duration {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_tick_aborted_after_take_due_leaves_the_timers_file_unchanged() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn rearming_a_key_moves_its_due_time_and_never_duplicates_it() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn timers_never_exceed_the_cap_and_a_retry_parks_after_five_tries() {
        todo!()
    }

    #[test]
    #[ignore = "skeleton: tick/core"]
    fn restart_fires_each_overdue_timer_once() {
        todo!()
    }
}
