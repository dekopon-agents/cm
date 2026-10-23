//! The campaign files people used to append by hand, written through cm: FEEDBACK.md,
//! OWNER-QUEUE.md items, step evidence and the REVIEW-OFF.md ledger. Every writer appends only.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::commands::closed;
use crate::epoch::EPOCH;
use crate::host::{Host, stamp};
use crate::journal;
use crate::markdown::headings;
use crate::outcome::{Outcome, Violation};
use crate::store::{self, Target, is_name, lock, write_atomic};
use crate::templates::{LOOP_WITHHELD, QUEUE_FILE};

pub const FEEDBACK: &str = "FEEDBACK.md";

/// Takes the lock and refuses a closed campaign or a journal cm did not write.
pub(crate) fn guard(root: &Path) -> Result<(store::Lock, Vec<Violation>)> {
    let held = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    violations.extend(closed(root, &campaign));
    Ok((held, violations))
}

pub(crate) fn one_line(field: &str, text: &str, out: &mut Vec<Violation>, path: &str) {
    if text.trim().is_empty() || text.contains('\n') {
        out.push(Violation::new(
            path,
            "ledger-field",
            format!("{field} is one non-empty line"),
        ));
    }
}

/// A body may hold no heading of a level the ledger itself writes.
fn plain_body(path: &str, body: &str, deepest: usize, out: &mut Vec<Violation>) {
    if let Some(heading) = headings(body)
        .iter()
        .find(|heading| heading.level <= deepest)
    {
        out.push(Violation::new(
            path,
            "ledger-body",
            format!(
                "the body may not hold a heading of level {deepest} or above: `{}`",
                heading.text
            ),
        ));
    }
}

fn append(path: &Path, text: &str) -> Result<()> {
    let mut current = fs::read_to_string(path).unwrap_or_default();
    while !current.is_empty() && !current.ends_with("\n\n") {
        current.push('\n');
    }
    current.push_str(text);
    write_atomic(path, current.as_bytes())
}

pub(crate) fn journal_line(root: &Path, at: &str, title: &str, body: &str) -> Result<String> {
    journal::append(root, &journal::entry(at, title, Some(body)))?;
    Ok(journal::heading(at, title))
}

// FEEDBACK.md

/// One id-bearing heading: `### FB-d-001 · urgent · …` or `### FB-d-011 follow-up · …`.
pub struct FeedbackHeading {
    pub line: usize,
    pub id: String,
    pub follow_up: bool,
}

fn feedback_id(word: &str) -> Option<(char, u32)> {
    let rest = word.strip_prefix("FB-")?;
    let (letter, number) = rest.split_once('-')?;
    let mut chars = letter.chars();
    let letter = chars
        .next()
        .filter(|c| c.is_ascii_lowercase() && chars.next().is_none())?;
    (number.len() >= 3 && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| number.parse().ok().map(|number| (letter, number)))
        .flatten()
}

pub fn feedback_headings(text: &str) -> Vec<FeedbackHeading> {
    headings(text)
        .into_iter()
        .filter_map(|heading| {
            let mut words = heading.text.split_whitespace();
            let id = words.next().filter(|word| feedback_id(word).is_some())?;
            Some(FeedbackHeading {
                line: heading.line,
                id: id.to_owned(),
                follow_up: words.next() == Some("follow-up"),
            })
        })
        .collect()
}

pub fn lint_feedback(root: &Path) -> Vec<Violation> {
    let Ok(text) = fs::read_to_string(root.join(FEEDBACK)) else {
        return Vec::new();
    };
    let found = feedback_headings(&text);
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for heading in found.iter().filter(|heading| !heading.follow_up) {
        if !seen.insert(heading.id.as_str()) {
            out.push(
                Violation::new(
                    FEEDBACK,
                    "feedback-id-duplicate",
                    format!("{} names a second entry; ids are unique", heading.id),
                )
                .at(heading.line),
            );
        }
    }
    for heading in found.iter().filter(|heading| heading.follow_up) {
        if !seen.contains(heading.id.as_str()) {
            out.push(
                Violation::new(
                    FEEDBACK,
                    "feedback-follow-up-unknown",
                    format!("the follow-up names {}, which no entry has", heading.id),
                )
                .at(heading.line),
            );
        }
    }
    out
}

fn sequence(root: &Path) -> Option<char> {
    let text = fs::read_to_string(root.join(EPOCH)).ok()?;
    let mut lines = text.lines();
    (lines.next()? == "---").then_some(())?;
    lines
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix("sequence:"))
        .map(|value| value.trim().trim_matches(['"', '\'']))
        .and_then(|value| {
            let mut chars = value.chars();
            chars
                .next()
                .filter(|c| c.is_ascii_lowercase() && chars.next().is_none())
        })
}

pub struct Feedback<'a> {
    pub title: &'a str,
    pub urgency: Option<&'a str>,
    pub follow_up: Option<&'a str>,
    pub disposition: Option<&'a str>,
    pub body: &'a str,
}

pub const URGENCIES: [&str; 2] = ["urgent", "queued"];
pub const DISPOSITIONS: [&str; 5] = ["proposed", "accepted", "applied", "deferred", "rejected"];

pub fn feedback(host: &dyn Host, root: &Path, entry: &Feedback) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    one_line("the title", entry.title, &mut violations, FEEDBACK);
    plain_body(FEEDBACK, entry.body, 3, &mut violations);
    if let Some(urgency) = entry.urgency.filter(|urgency| !URGENCIES.contains(urgency)) {
        violations.push(Violation::new(
            FEEDBACK,
            "feedback-urgency",
            format!("`{urgency}` is not urgent or queued"),
        ));
    }
    let text = fs::read_to_string(root.join(FEEDBACK)).unwrap_or_else(|_| "# Feedback\n".into());
    let found = feedback_headings(&text);
    let heading = match (entry.follow_up, entry.disposition, entry.urgency) {
        (Some(id), Some(disposition), urgency) => {
            if !DISPOSITIONS.contains(&disposition) {
                violations.push(Violation::new(
                    FEEDBACK,
                    "feedback-disposition",
                    format!("`{disposition}` is not one of {}", DISPOSITIONS.join(", ")),
                ));
            }
            if !found
                .iter()
                .any(|heading| !heading.follow_up && heading.id == id)
            {
                violations.push(Violation::new(
                    FEEDBACK,
                    "feedback-follow-up-unknown",
                    format!("{id} names no entry in FEEDBACK.md"),
                ));
            }
            let urgency = urgency
                .map(|urgency| format!(" · {urgency}"))
                .unwrap_or_default();
            (
                id.to_owned(),
                format!(
                    "### {id} follow-up{urgency} · {disposition}: {}",
                    entry.title.trim()
                ),
            )
        }
        (None, None, Some(urgency)) => match sequence(root) {
            None => {
                violations.push(Violation::new(
                    EPOCH,
                    "feedback-sequence",
                    "EPOCH.md's front matter has no one-letter `sequence:`; ids are FB-<sequence>-NNN",
                ));
                (String::new(), String::new())
            }
            Some(letter) => {
                let last = found
                    .iter()
                    .filter_map(|heading| feedback_id(&heading.id))
                    .filter(|(found, _)| *found == letter)
                    .map(|(_, number)| number)
                    .max()
                    .unwrap_or(0);
                let id = format!("FB-{letter}-{:03}", last + 1);
                let heading = format!("### {id} · {urgency} · {}", entry.title.trim());
                (id, heading)
            }
        },
        _ => {
            violations.push(Violation::new(
                FEEDBACK,
                "feedback-usage",
                "an entry takes --urgency; a follow-up takes --follow-up <id> and --disposition",
            ));
            (String::new(), String::new())
        }
    };
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let (id, heading) = heading;
    let at = stamp(host.now());
    let mut text_out = format!("{heading}\n\nLogged {at} by `cm feedback`.\n");
    if !entry.body.trim().is_empty() {
        text_out.push('\n');
        text_out.push_str(entry.body.trim());
        text_out.push('\n');
    }
    if !root.join(FEEDBACK).exists() {
        write_atomic(&root.join(FEEDBACK), b"# Feedback\n")?;
    }
    append(&root.join(FEEDBACK), &text_out)?;
    let kind = if entry.follow_up.is_some() {
        "feedback follow-up"
    } else {
        "feedback"
    };
    let journal = journal_line(
        root,
        &at,
        &format!("{kind} {id}: {}", entry.title.trim()),
        &format!("- {FEEDBACK}: `{}`\n", heading.trim_start_matches("### ")),
    )?;
    let mut outcome = Outcome::default();
    outcome.line(heading.trim_start_matches("### ").to_owned());
    outcome.field("id", id);
    outcome.field("journal", journal);
    Ok(outcome)
}

// OWNER-QUEUE.md

fn item_number(text: &str) -> Option<u32> {
    text.split_once(". ")?.0.parse().ok()
}

/// The RESUME.md banner with its fenced `/loop` replaced by the withheld line.
fn withhold(resume: &str) -> Option<String> {
    let lines: Vec<&str> = resume.lines().collect();
    let start = lines.iter().position(|line| !line.trim().is_empty())?;
    let end = start
        + lines[start..]
            .iter()
            .take_while(|line| line.starts_with('>'))
            .count();
    if lines[start..end]
        .iter()
        .any(|line| line.contains(LOOP_WITHHELD))
    {
        return None;
    }
    let fence: Vec<usize> = (start..end)
        .filter(|index| lines[*index][1..].trim_start().starts_with("```"))
        .collect();
    let withheld = format!("> {LOOP_WITHHELD}");
    let mut out: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    match fence[..] {
        [open, close, ..] => {
            out.splice(open..=close, [withheld]);
        }
        _ => {
            out.splice(end..end, [">".to_owned(), withheld]);
        }
    }
    let mut text = out.join("\n");
    if resume.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

pub struct QueueItem<'a> {
    pub title: &'a str,
    pub context: &'a str,
    pub question: &'a str,
    pub recommendation: &'a str,
}

pub fn owner_queue_add(host: &dyn Host, root: &Path, item: &QueueItem) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    for (field, text) in [
        ("the title", item.title),
        ("--context", item.context),
        ("--question", item.question),
        ("--recommendation", item.recommendation),
    ] {
        one_line(field, text, &mut violations, QUEUE_FILE);
    }
    let Ok(queue) = fs::read_to_string(root.join(QUEUE_FILE)) else {
        violations.push(Violation::new(
            QUEUE_FILE,
            "missing-file",
            "run `cm init` to write it",
        ));
        return Ok(Outcome::refused(violations));
    };
    let resume = fs::read_to_string(root.join("RESUME.md")).unwrap_or_default();
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let number = headings(&queue)
        .iter()
        .filter(|heading| heading.level == 2)
        .filter_map(|heading| item_number(heading.text))
        .max()
        .unwrap_or(0)
        + 1;
    let title = format!("{number}. {}", item.title.trim());
    append(
        &root.join(QUEUE_FILE),
        &format!(
            "## {title}\n\nContext: {}\n{}\nRecommendation: {}\nAnswer:\n",
            item.context.trim(),
            item.question.trim(),
            item.recommendation.trim()
        ),
    )?;
    let withheld = withhold(&resume);
    if let Some(text) = &withheld {
        write_atomic(&root.join("RESUME.md"), text.as_bytes())?;
    }
    let at = stamp(host.now());
    let mut body = format!("- {QUEUE_FILE}: `## {title}`\n");
    if withheld.is_some() {
        body.push_str(&format!("- RESUME.md banner: {LOOP_WITHHELD}\n"));
    }
    let journal = journal_line(root, &at, &format!("owner-queue add {title}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!("## {title}"));
    outcome.field("item", number);
    outcome.field("journal", journal);
    Ok(outcome)
}

// <step>/evidence/<NAME>.md

pub fn evidence_dir(root: &Path, target: &Target) -> PathBuf {
    root.join(&target.unit)
        .join(target.step.as_deref().unwrap_or_default())
        .join("evidence")
}

/// The step's one evidence file, when it has exactly one.
pub fn single_evidence(root: &Path, target: &Target) -> Option<PathBuf> {
    let files: Vec<PathBuf> = fs::read_dir(evidence_dir(root, target))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    match &files[..] {
        [one] => Some(one.clone()),
        _ => None,
    }
}

pub fn evidence(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    name: &str,
    body: &str,
) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    let full = format!(
        "{}/{}",
        target.unit,
        target.step.as_deref().unwrap_or_default()
    );
    let declared = store::is_unit(&root.join(&target.unit))
        && store::load_unit(&root.join(&target.unit))?
            .steps
            .contains_key(target.step.as_deref().unwrap_or_default());
    if !declared {
        violations.push(Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        ));
    }
    if !is_name(name) || name.ends_with(".md") {
        violations.push(Violation::new(
            &full,
            "evidence-name",
            format!("`{name}` is not a name of letters, digits, `.`, `_` and `-` (cm adds `.md`)"),
        ));
    }
    if body.trim().is_empty() {
        violations.push(Violation::new(&full, "evidence-empty", "the body is empty"));
    }
    let path = evidence_dir(root, target).join(format!("{name}.md"));
    let rel = store::relative(root, &path);
    if path.exists() {
        violations.push(Violation::new(
            &rel,
            "evidence-exists",
            "evidence is written once; pick a new --name",
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    fs::create_dir_all(evidence_dir(root, target))?;
    let mut text = body.trim_end().to_owned();
    text.push('\n');
    if !store::create_new(&path, text.as_bytes())? {
        return Ok(Outcome::refused(vec![Violation::new(
            &rel,
            "evidence-exists",
            "written meanwhile",
        )]));
    }
    let at = stamp(host.now());
    let journal = journal_line(
        root,
        &at,
        &format!("evidence {full}"),
        &format!("- written: `{rel}`\n"),
    )?;
    let mut outcome = Outcome::default();
    outcome.line(format!("wrote {rel}"));
    outcome.field("path", rel);
    outcome.field("journal", journal);
    Ok(outcome)
}
