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
    ("OWNER-QUEUE.md", OWNER_QUEUE),
];

pub const RESUME_HEADINGS: [&str; 4] = [
    "# Resume",
    "## State today",
    "## How to work",
    "## Read, in this order",
];

pub fn conform(name: &str, text: &str) -> Vec<Violation> {
    match name {
        "RESUME.md" => resume(text),
        "JOURNAL.md" => journal_title(text),
        "LIMITS.toml" => limits::check(text).err().unwrap_or_default(),
        "OWNER-QUEUE.md" => owner_queue(text),
        _ => Vec::new(),
    }
}

pub fn resume(text: &str) -> Vec<Violation> {
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
        if !has_loop {
            out.push(
                Violation::new(
                    "RESUME.md",
                    "resume-loop",
                    "the banner holds a fenced `/loop …` command that resumes the campaign",
                )
                .at(first.map_or(1, |start| start + 1)),
            );
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
            assert_eq!(conform(name, text), Vec::new(), "{name}");
        }
    }

    #[test]
    fn resume_lists_every_gap() {
        let violations = resume("# Resume\n\n## How to work\n\n## State today\n");
        let rules: Vec<_> = violations.iter().map(|v| v.rule).collect();
        assert_eq!(
            rules,
            vec!["resume-banner", "resume-heading", "resume-heading"],
            "{violations:?}"
        );
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
