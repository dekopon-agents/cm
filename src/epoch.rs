use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::commands::closed;
use crate::host::Host;
use crate::journal;
use crate::outcome::{Outcome, Violation};
use crate::store::{self, lock, write_atomic};

pub const EPOCH: &str = "EPOCH.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Open,
    Planning,
    Working,
    Closed,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Open => "OPEN",
            Status::Planning => "PLANNING",
            Status::Working => "WORKING",
            Status::Closed => "CLOSED",
        }
    }

    fn of_label(text: &str) -> Option<Status> {
        [
            Status::Open,
            Status::Planning,
            Status::Working,
            Status::Closed,
        ]
        .into_iter()
        .find(|status| status.label() == text)
    }

    fn stamp_key(self) -> Option<&'static str> {
        match self {
            Status::Open => None,
            Status::Planning => Some("planning_started_at"),
            Status::Working => Some("working_started_at"),
            Status::Closed => Some("closed_at"),
        }
    }

    fn pins(self) -> &'static [Pin] {
        match self {
            Status::Open => &[],
            Status::Planning => &[Pin::Skill],
            Status::Working | Status::Closed => &[Pin::Skill, Pin::Plan],
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label().to_lowercase())
    }
}

impl std::str::FromStr for Status {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "planning" => Ok(Status::Planning),
            "working" => Ok(Status::Working),
            "closed" => Ok(Status::Closed),
            _ => Err(format!(
                "`{text}` is not an epoch move; moves are planning, working, closed"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    Skill,
    Plan,
}

impl Pin {
    fn key(self) -> &'static str {
        match self {
            Pin::Skill => "skill_revision",
            Pin::Plan => "approved_plan_revision",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Pin::Skill => "--skill-revision",
            Pin::Plan => "--plan-revision",
        }
    }
}

struct Front {
    start: usize,
    end: usize,
}

fn front(lines: &[String]) -> Option<Front> {
    if lines.first().map(String::as_str) != Some("---") {
        return None;
    }
    let end = lines.iter().skip(1).position(|line| line == "---")? + 1;
    Some(Front { start: 1, end })
}

fn value<'a>(lines: &'a [String], front: &Front, key: &str) -> Option<&'a str> {
    lines[front.start..front.end].iter().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim)
    })
}

fn set(lines: &mut Vec<String>, front: &mut Front, key: &str, text: &str) {
    let line = format!("{key}: {text}");
    match (front.start..front.end).find(|index| {
        lines[*index]
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(':'))
    }) {
        Some(index) => lines[index] = line,
        None => {
            lines.insert(front.end, line);
            front.end += 1;
        }
    }
}

fn is_null(text: Option<&str>) -> bool {
    matches!(text, None | Some("" | "null" | "~" | "Null" | "NULL"))
}

fn pin_problem(text: &str) -> Option<&'static str> {
    if is_null(Some(text.trim())) {
        Some("is null")
    } else if text != text.trim() || text.contains('\n') {
        Some("is not one trimmed line")
    } else if text.contains(" #")
        || text.contains(": ")
        || text.starts_with([
            '#', '[', '{', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`',
        ])
    {
        Some("would not read back as one plain YAML value")
    } else {
        None
    }
}

pub fn epoch(
    host: &dyn Host,
    root: &Path,
    to: Status,
    skill: Option<&str>,
    plan: Option<&str>,
) -> Result<Outcome> {
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    match to {
        Status::Closed if campaign.closed.is_none() => violations.push(Violation::new(
            EPOCH,
            "epoch-open-campaign",
            "an epoch closes after its campaign: run `cm close` first",
        )),
        Status::Closed => {}
        _ => violations.extend(closed(root, &campaign)),
    }
    let Ok(text) = fs::read_to_string(root.join(EPOCH)) else {
        violations.push(Violation::new(
            EPOCH,
            "epoch-missing",
            "the campaign folder has no EPOCH.md manifest",
        ));
        return Ok(Outcome::refused(violations));
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let Some(mut front) = front(&lines) else {
        violations.push(Violation::new(
            EPOCH,
            "epoch-front-matter",
            "EPOCH.md opens with a `---` front matter block",
        ));
        return Ok(Outcome::refused(violations));
    };
    let current = value(&lines, &front, "status")
        .unwrap_or_default()
        .to_owned();
    match Status::of_label(&current) {
        None => violations.push(Violation::new(
            EPOCH,
            "epoch-status",
            format!("`status: {current}` is not OPEN, PLANNING, WORKING or CLOSED"),
        )),
        Some(from) if from as u8 + 1 != to as u8 => violations.push(Violation::new(
            EPOCH,
            "epoch-status",
            format!(
                "{} → {} is not a move; epochs go OPEN → PLANNING → WORKING → CLOSED",
                from.label(),
                to.label()
            ),
        )),
        Some(_) => {}
    }
    let mut pins = Vec::new();
    for (pin, given) in [(Pin::Skill, skill), (Pin::Plan, plan)] {
        if let Some(given) = given {
            match pin_problem(given) {
                Some(problem) => violations.push(Violation::new(
                    EPOCH,
                    "epoch-pin",
                    format!("{} `{given}` {problem}", pin.flag()),
                )),
                None => pins.push((pin, given.to_owned())),
            }
        }
    }
    for pin in to.pins() {
        let given = match pin {
            Pin::Skill => skill.is_some(),
            Pin::Plan => plan.is_some(),
        };
        if !given && is_null(value(&lines, &front, pin.key())) {
            violations.push(Violation::new(
                EPOCH,
                "epoch-pin",
                format!(
                    "{} needs `{}` pinned and it is null; pass {} <revision>",
                    to.label(),
                    pin.key(),
                    pin.flag()
                ),
            ));
        }
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let at = crate::host::stamp(host.now());
    let date = &at[..10];
    set(&mut lines, &mut front, "status", to.label());
    if let Some(key) = to.stamp_key() {
        set(&mut lines, &mut front, key, date);
    }
    for (pin, given) in &pins {
        set(&mut lines, &mut front, pin.key(), given);
    }
    let mut next = lines.join("\n");
    if text.ends_with('\n') {
        next.push('\n');
    }
    write_atomic(&root.join(EPOCH), next.as_bytes())?;
    let mut body = format!("- status: {current} → {}\n", to.label());
    for pin in to.pins() {
        body.push_str(&format!(
            "- {}: {}\n",
            pin.key(),
            value(&lines, &front, pin.key()).unwrap_or_default()
        ));
    }
    let title = format!("epoch {to}");
    journal::append(root, &journal::entry(&at, &title, Some(&body)))?;
    let heading = journal::heading(&at, &title);
    let mut outcome = Outcome::default();
    outcome.line(format!("EPOCH.md: {current} → {}", to.label()));
    outcome.field("from", current);
    outcome.field("to", to.label());
    outcome.field("journal", heading);
    Ok(outcome)
}
