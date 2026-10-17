use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Result;

use crate::host::is_stamp;
use crate::markdown::first_difference;
use crate::outcome::Violation;
use crate::state::CampaignState;
use crate::store::{JOURNAL, write_atomic};
use crate::templates;

pub fn heading(stamp: &str, title: &str) -> String {
    format!("## {stamp} — {title}")
}

pub fn is_stamped(line: &str) -> bool {
    line.strip_prefix("## ")
        .and_then(|rest| rest.split_once(" — "))
        .is_some_and(|(stamp, title)| is_stamp(stamp) && !title.trim().is_empty())
}

pub fn committed(root: &Path, name: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &format!("HEAD:./{name}")])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn check(root: &Path, state: &CampaignState) -> Vec<Violation> {
    let Ok(text) = fs::read_to_string(root.join(JOURNAL)) else {
        return vec![Violation::new(
            JOURNAL,
            "journal-missing",
            "JOURNAL.md is gone; restore it with `git checkout HEAD -- JOURNAL.md`",
        )];
    };
    let mut out = templates::journal_title(&text);
    let Some(mark) = &state.journal else {
        out.push(Violation::new(
            JOURNAL,
            "journal-unrecorded",
            "cm has not recorded this journal; fix its conformance and run `cm init`",
        ));
        return out;
    };
    if let Some(recorded) = committed(root, JOURNAL) {
        match text.strip_prefix(recorded.as_str()) {
            Some(tail) => {
                let first = recorded.lines().count() + usize::from(recorded.ends_with('\n'));
                let added = tail
                    .lines()
                    .enumerate()
                    .find(|(_, line)| !line.trim().is_empty());
                if let Some((index, line)) = added
                    && !is_stamped(line)
                {
                    out.push(
                        Violation::new(
                            JOURNAL,
                            "journal-hand-append",
                            "lines were added to the last committed entry; move them into a new entry with `cm journal`",
                        )
                        .at((first + index).max(1)),
                    );
                }
            }
            None => {
                let line = first_difference(&text, &recorded);
                out.push(
                    Violation::new(
                        JOURNAL,
                        "journal-rewritten",
                        "the committed journal (git HEAD) was changed from this line on; restore it with `git checkout HEAD -- JOURNAL.md` and re-add uncommitted entries with `cm journal`",
                    )
                    .at(line),
                );
            }
        }
    }
    for (index, line) in text.lines().enumerate().skip(mark.baseline_lines) {
        if line.starts_with("## ") && !is_stamped(line) {
            out.push(
                Violation::new(
                    JOURNAL,
                    "journal-unstamped-heading",
                    "an entry heading is `## YYYY-MM-DDTHH:MMZ — <title>`, stamped by cm",
                )
                .at(index + 1),
            );
        }
    }
    out
}

pub fn validate(title: &str, body: Option<&str>) -> Result<(), String> {
    if title.trim().is_empty() || title.contains('\n') {
        return Err("a journal title is one non-empty line".into());
    }
    if let Some(body) = body
        && let Some(line) = body.lines().find(|line| {
            let level = line.bytes().take_while(|byte| *byte == b'#').count();
            (level == 1 || level == 2) && (line.len() == level || line[level..].starts_with(' '))
        })
    {
        return Err(format!(
            "the body may not hold a `#` or `##` heading, which only cm writes: `{line}`"
        ));
    }
    Ok(())
}

pub fn entry(stamp: &str, title: &str, body: Option<&str>) -> String {
    let mut out = heading(stamp, title.trim());
    out.push('\n');
    if let Some(body) = body
        .map(str::trim_end)
        .filter(|body| !body.trim().is_empty())
    {
        out.push('\n');
        out.push_str(body.trim_start_matches('\n'));
        out.push('\n');
    }
    out
}

pub fn append(root: &Path, entry: &str) -> Result<()> {
    let current = fs::read_to_string(root.join(JOURNAL))?;
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() && !next.ends_with("\n\n") {
        next.push('\n');
    }
    next.push_str(entry);
    write_atomic(&root.join(JOURNAL), next.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamped_headings() {
        assert!(is_stamped("## 2026-10-08T07:04Z — launch 01-a/u/s"));
        assert!(!is_stamped("## 2026-10-08T07:0xZ — typed by a model"));
        assert!(!is_stamped("## 2026-10-08: planning"));
        assert!(!is_stamped("## 2026-10-08T07:04Z — "));
    }

    #[test]
    fn bodies_cannot_forge_headings() {
        assert!(validate("t", Some("text\n## 2026-10-08T07:04Z — x")).is_err());
        assert!(validate("t", Some("#\n")).is_err());
        assert!(validate("t", Some("### fine\n#hashtag\n")).is_ok());
        assert!(validate("two\nlines", None).is_err());
    }
}
