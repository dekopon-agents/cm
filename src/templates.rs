use std::fs;
use std::path::Path;

use crate::limits;
use crate::markdown::headings;
use crate::outcome::Violation;

pub const RESUME: &str = include_str!("../templates/RESUME.md");
pub const JOURNAL: &str = include_str!("../templates/JOURNAL.md");
pub const LIMITS: &str = include_str!("../templates/LIMITS.toml");
pub const OWNER_QUEUE: &str = include_str!("../templates/OWNER-QUEUE.md");

pub const ROOT_FILES: [(&str, &str); 4] = [
    ("RESUME.md", RESUME),
    ("JOURNAL.md", JOURNAL),
    ("LIMITS.toml", LIMITS),
    (QUEUE_FILE, OWNER_QUEUE),
];

pub const RESUME_HEADINGS: [&str; 4] = [
    "# Resume",
    "## State today",
    "## How to work",
    "## Read, in this order",
];

pub const QUEUE_FILE: &str = "OWNER-QUEUE.md";

pub const LOOP_WITHHELD: &str = "OWNER-QUEUE not drained: /loop withheld";

pub fn conform(name: &str, text: &str, queue_open: bool) -> Vec<Violation> {
    match name {
        "RESUME.md" => resume(text, queue_open),
        "JOURNAL.md" => journal_title(text),
        "LIMITS.toml" => limits::check(text).err().unwrap_or_default(),
        QUEUE_FILE => owner_queue(text),
        _ => Vec::new(),
    }
}

pub fn queue_open(queue: &str) -> bool {
    headings(queue).iter().any(|heading| heading.level == 2)
}

pub fn queue_open_in(root: &Path) -> bool {
    fs::read_to_string(root.join(QUEUE_FILE)).is_ok_and(|queue| queue_open(&queue))
}

pub fn resume(text: &str, queue_open: bool) -> Vec<Violation> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let banner: Vec<&str> = match first {
        Some(start) if lines[start].starts_with('>') => lines[start..]
            .iter()
            .take_while(|line| line.starts_with('>'))
            .map(|line| line[1..].trim_start())
            .collect(),
        _ => {
            out.push(
                Violation::new(
                    "RESUME.md",
                    "resume-banner",
                    "the file opens with a `>` banner: what is running and what to read next",
                )
                .at(first.map_or(1, |start| start + 1)),
            );
            Vec::new()
        }
    };
    if !banner.is_empty() {
        let fence = banner.iter().position(|line| line.starts_with("```"));
        let has_loop = fence.is_some_and(|fence| {
            banner[fence + 1..]
                .iter()
                .take_while(|line| !line.starts_with("```"))
                .any(|line| line.starts_with("/loop "))
        });
        let withheld = banner.iter().any(|line| line.contains(LOOP_WITHHELD));
        let line = first.map_or(1, |start| start + 1);
        let message = match (queue_open, has_loop, withheld) {
            (true, true, _) => Some(format!(
                "OWNER-QUEUE.md has an open item, so the banner proposes no `/loop` until the queue is drained; replace the fenced `/loop …` with the line `{LOOP_WITHHELD}`"
            )),
            (true, false, false) => Some(format!(
                "OWNER-QUEUE.md has an open item, so the banner carries the line `{LOOP_WITHHELD}` in place of the `/loop`"
            )),
            (false, false, _) => Some(
                "OWNER-QUEUE.md has no open item, so the banner holds a fenced `/loop …` command that resumes the campaign".to_owned(),
            ),
            (false, true, true) => Some(format!(
                "OWNER-QUEUE.md has no open item, so the banner drops the line `{LOOP_WITHHELD}`"
            )),
            (true, false, true) | (false, true, false) => None,
        };
        if let Some(message) = message {
            out.push(Violation::new("RESUME.md", "resume-loop", message).at(line));
        }
    }
    let found = headings(text);
    let mut last_line = 0;
    for wanted in RESUME_HEADINGS {
        let level = wanted.bytes().take_while(|byte| *byte == b'#').count();
        let title = &wanted[level + 1..];
        match found
            .iter()
            .find(|heading| heading.level == level && heading.text == title)
        {
            None => out.push(Violation::new(
                "RESUME.md",
                "resume-heading",
                format!("missing heading `{wanted}`"),
            )),
            Some(heading) if heading.line < last_line => out.push(
                Violation::new(
                    "RESUME.md",
                    "resume-heading",
                    format!(
                        "`{wanted}` is out of order; the order is {}",
                        RESUME_HEADINGS.join(", ")
                    ),
                )
                .at(heading.line),
            ),
            Some(heading) => last_line = heading.line,
        }
    }
    out
}

pub fn journal_title(text: &str) -> Vec<Violation> {
    match headings(text).first() {
        Some(heading) if heading.level == 1 && heading.text == "Journal" => Vec::new(),
        other => vec![
            Violation::new(
                "JOURNAL.md",
                "journal-title",
                "the first heading is `# Journal`",
            )
            .at(other.map_or(1, |heading| heading.line)),
        ],
    }
}

pub fn owner_queue(text: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    let found = headings(text);
    match found.first() {
        Some(heading) if heading.level == 1 && heading.text == "Owner queue" => {}
        other => out.push(
            Violation::new(
                "OWNER-QUEUE.md",
                "owner-queue-title",
                "the first heading is `# Owner queue`",
            )
            .at(other.map_or(1, |heading| heading.line)),
        ),
    }
    let lines: Vec<&str> = text.lines().collect();
    let items: Vec<usize> = found
        .iter()
        .filter(|heading| heading.level <= 2)
        .map(|heading| heading.line)
        .collect();
    for heading in &found {
        if heading.level != 2 {
            continue;
        }
        let end = items
            .iter()
            .find(|line| **line > heading.line)
            .map_or(lines.len(), |line| line - 1);
        let body = &lines[heading.line..end.max(heading.line)];
        for field in ["Recommendation:", "Answer:"] {
            if !body.iter().any(|line| line.trim_start().starts_with(field)) {
                out.push(
                    Violation::new(
                        "OWNER-QUEUE.md",
                        "owner-queue-field",
                        format!("item `{}` has no line starting `{field}`", heading.text),
                    )
                    .at(heading.line),
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_conform() {
        for (name, text) in ROOT_FILES {
            assert_eq!(
                conform(name, text, queue_open(OWNER_QUEUE)),
                Vec::new(),
                "{name}"
            );
        }
    }

    #[test]
    fn resume_lists_every_gap() {
        let violations = resume("# Resume\n\n## How to work\n\n## State today\n", false);
        let rules: Vec<_> = violations.iter().map(|v| v.rule).collect();
        assert_eq!(
            rules,
            vec!["resume-banner", "resume-heading", "resume-heading"],
            "{violations:?}"
        );
    }

    const OPEN_QUEUE: &str =
        "# Owner queue\n\nIntro.\n\n## Pick one\n\nRecommendation: yes\n\nAnswer:\n";

    fn withheld_resume() -> String {
        let start = RESUME.find("> ```").unwrap();
        let end = RESUME.find("\n\n# Resume").unwrap();
        format!(
            "{}> **{LOOP_WITHHELD}** until the owner answers.{}",
            &RESUME[..start],
            &RESUME[end..]
        )
    }

    fn loop_rules(text: &str, queue_open: bool) -> Vec<String> {
        resume(text, queue_open)
            .into_iter()
            .map(|violation| format!("{}: {}", violation.rule, violation.message))
            .collect()
    }

    #[test]
    fn open_queue_items_are_second_level_headings() {
        assert!(!queue_open(OWNER_QUEUE));
        assert!(queue_open(OPEN_QUEUE));
    }

    #[test]
    fn open_queue_with_withheld_line_passes() {
        assert_eq!(loop_rules(&withheld_resume(), true), Vec::<String>::new());
    }

    #[test]
    fn open_queue_with_loop_fails() {
        let found = loop_rules(RESUME, true);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("resume-loop: OWNER-QUEUE.md has an open item"));
        assert!(found[0].contains("replace the fenced `/loop …`"));
    }

    #[test]
    fn empty_queue_with_loop_passes() {
        assert_eq!(loop_rules(RESUME, false), Vec::<String>::new());
    }

    #[test]
    fn empty_queue_with_withheld_line_fails() {
        let found = loop_rules(&withheld_resume(), false);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("resume-loop: OWNER-QUEUE.md has no open item"));
        assert!(found[0].contains("fenced `/loop …` command"));
    }

    #[test]
    fn owner_queue_items_need_both_fields() {
        let text =
            "# Owner queue\n\n## One\n\nRecommendation: yes\n\nAnswer:\n\n## Two\n\nAnswer: no\n";
        let violations = owner_queue(text);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].line, Some(9));
        assert!(violations[0].message.contains("Recommendation:"));
    }
}
