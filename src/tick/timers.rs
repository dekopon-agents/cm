//! Timers and retries (`tick/core`): due work that is not an event. Durable in [`TIMERS_FILE`],
//! keyed so arming is idempotent, saved into the tick's first commit (S-tick-41, stage 9), so a
//! killed tick leaves the pushed file as it was.

#![allow(unused_variables, reason = "skeleton: tick/core fills these bodies")]

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::tick::ledger::IntentId;

pub const TIMERS_FILE: &str = ".cm/tick/timers.json";
/// Growth bound: armed timers. Arming past it is [`crate::tick::TickError::TimersFull`], which the
/// arming stage turns into a parked intent and one packet item.
pub const MAX_TIMERS: usize = 256;
/// A known-not-done intent is emitted at most this many times, then parked.
pub const MAX_TRIES: u32 = 5;
/// How often a parked intent comes back into the packet while nobody rules on it.
pub const REMINDER_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// How long a review may run before the packet says it is late.
pub const REVIEW_DEADLINE: Duration = Duration::from_secs(90 * 60);
/// How long a handed packet action may stay undone before its intent is parked (R18).
pub const ACTION_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);
/// The first retry delay (U05): the RPi runner's 5-minute schedule (EXECUTION.md §Runner).
pub const BACKOFF_BASE: Duration = Duration::from_secs(5 * 60);
/// The longest retry delay.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TimerKind {
    /// Emit a known-not-done intent again after [`backoff`].
    Retry { intent: IntentId },
    /// Poll an Astra review with `astra-review.sh collect` (S-tick-38).
    Collect { intent: IntentId },
    /// A review launched with this intent's nonce has no verdict by the deadline.
    ReviewDeadline { intent: IntentId },
    /// A parked intent nobody has ruled on.
    Reminder { intent: IntentId },
    /// A handed packet action: if its intent is still handed when this fires, reconcile parks it
    /// and arms its reminder (R18).
    ActionDeadline { intent: IntentId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timer {
    /// Unique; arming an existing key moves its due time and keeps its tries.
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
    /// Absent is empty.
    pub fn load(root: &Path) -> crate::tick::Result<Self> {
        todo!("tick/core")
    }

    /// Written atomically; the caller adds the path to the tick's commit.
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

    /// The timers due at `now` (a `cm` stamp), oldest first, removed from this copy only.
    pub fn take_due(&mut self, now: &str) -> Vec<Timer> {
        todo!("tick/core")
    }
}

/// [`BACKOFF_BASE`] doubled per try, capped at [`BACKOFF_MAX`]: the delay before try `tries + 1`.
pub fn backoff(tries: u32) -> Duration {
    todo!("tick/core")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "skeleton: tick/core"]
    fn a_tick_killed_after_take_due_leaves_the_pushed_timers_file_unchanged() {
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
    fn a_restart_fires_each_overdue_timer_once() {
        todo!()
    }
}
