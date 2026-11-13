use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::idea::record_id;
use crate::outcome::Outcome;
use crate::store::write_atomic;

const KINDS: [(&str, &str); 3] = [
    ("Rubric review", "rubric review"),
    ("Astra review", "astra review"),
    ("Regrets sweep", "regrets sweep"),
];

pub const MIN_BYTES: usize = 20_000;

fn read(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

fn first_date(text: &str) -> Option<&str> {
    (0..text.len().saturating_sub(9))
        .filter(|at| text.is_char_boundary(*at) && text.is_char_boundary(at + 10))
        .map(|at| &text[at..at + 10])
        .find(|candidate| is_date(candidate))
}

fn line_at(text: &str, at: usize) -> &str {
    let end = text[at..].find('\n').map_or(text.len(), |n| at + n);
    &text[at..end]
}

fn section_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .filter(|at| text[*at..].starts_with("## "))
        .collect()
}

/// `## <kind> — <YYYY-MM-DD>…`: the kind's lowercase name and the date.
fn review_heading(line: &str) -> Option<(&'static str, &str)> {
    let rest = line.strip_prefix("## ")?;
    KINDS.iter().find_map(|(kind, lower)| {
        let date = rest.strip_prefix(kind)?.strip_prefix(" — ")?.get(..10)?;
        is_date(date).then_some((*lower, date))
    })
}

fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split([
        '\n', '\r', '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ])
}

fn verdict(body: &str) -> String {
    for line in split_lines(body) {
        if line.to_lowercase().contains("verdict") {
            let chars: Vec<char> = line.replace("**", "").trim().chars().collect();
            let cut = (20..chars.len().saturating_sub(1))
                .find(|at| matches!(chars[*at], '.' | ')') && chars[at + 1].is_whitespace());
            let end = cut.map_or(chars.len(), |at| at + 1).min(200);
            return chars[..end].iter().collect();
        }
    }
    "Verdict: see history.".into()
}

fn anchor(heading: &str) -> String {
    heading[3..]
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == ' ' || *c == '-')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

fn links(line: &str) -> impl Iterator<Item = &str> {
    line.match_indices("](").filter_map(|(at, _)| {
        let rest = &line[at + 2..];
        rest.find(')').map(|end| &rest[..end])
    })
}

/// A link into `history/` or a pass's `designs/<pass>/notes/`, where full reviews live.
fn review_link(target: &str) -> bool {
    let path = target.split('#').next().unwrap_or_default();
    let parts: Vec<&str> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    matches!(
        parts.as_slice(),
        ["history", _] | [.., "ideas", "history", _] | [.., "designs", _, "notes", _]
    )
}

/// A review section already in its minimal form: at most two lines under the heading, naming the
/// verdict and linking the full review. It never moves.
fn stub(section: &str) -> bool {
    let body: Vec<&str> = section
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .collect();
    body.len() <= 2
        && body
            .iter()
            .any(|line| line.to_lowercase().contains("verdict"))
        && body.iter().any(|line| links(line).any(review_link))
}

struct Move {
    from: usize,
    to: usize,
    heading: String,
}

fn plan(text: &str, min_bytes: usize) -> Vec<Move> {
    if text.len() < min_bytes {
        return Vec::new();
    }
    let rulings: Vec<(String, Option<&str>)> = section_starts(text)
        .into_iter()
        .map(|at| line_at(text, at))
        .filter(|line| line.starts_with("## Owner rulings"))
        .map(|line| (line.to_lowercase(), first_date(line)))
        .collect();
    let mut starts = section_starts(text);
    starts.push(text.len());
    starts
        .windows(2)
        .filter_map(|pair| {
            let (from, to) = (pair[0], pair[1]);
            let heading = line_at(text, from);
            let (kind, date) = review_heading(heading)?;
            if text[from..to].contains("Moved to [history/") || stub(&text[from..to]) {
                return None;
            }
            rulings
                .iter()
                .any(|(ruling, ruled)| {
                    ruling.contains(kind) && ruled.is_none_or(|ruled| ruled >= date)
                })
                .then(|| Move {
                    from,
                    to,
                    heading: heading.to_owned(),
                })
        })
        .collect()
}

pub struct SplitArgs<'a> {
    pub ideas: &'a Path,
    pub ids: &'a [String],
    pub apply: bool,
    pub min_bytes: usize,
    pub date: &'a str,
}

/// `cm idea split-history`: ruled-on review sections of large records move to `history/<id>.md`,
/// leaving a heading stub that keeps the anchor. Idempotent: a stub never moves again.
pub fn split(args: &SplitArgs<'_>) -> Result<Outcome> {
    let mut names: Vec<String> = fs::read_dir(args.ideas)
        .with_context(|| format!("reading {}", args.ideas.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file() && record_id(&entry.path()).is_some())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| args.ids.is_empty() || args.ids.iter().any(|id| name.starts_with(id)))
        .collect();
    names.sort();
    let mut outcome = Outcome::default();
    let mut moved_records = Vec::new();
    for name in names {
        let path = args.ideas.join(&name);
        let mut text = read(&path)?;
        let moves = plan(&text, args.min_bytes);
        if moves.is_empty() {
            continue;
        }
        let id = record_id(&path).unwrap_or_default();
        let hist_rel = format!("history/{id}.md");
        let chars: usize = moves
            .iter()
            .map(|one| text[one.from..one.to].chars().count())
            .sum();
        outcome.line(format!(
            "{id}: {} B, moving {} section(s), {chars} chars",
            text.len(),
            moves.len()
        ));
        moved_records.push(json!({
            "id": id, "bytes": text.len(), "sections": moves.len(), "chars": chars,
        }));
        if !args.apply {
            continue;
        }
        let hist = args.ideas.join("history").join(format!("{id}.md"));
        fs::create_dir_all(args.ideas.join("history"))?;
        let mut out = if hist.exists() {
            read(&hist)?
        } else {
            format!(
                "# {id}: review history\n\nReview sections moved out of [the record](../{name}) after the owner ruled on them. Text is verbatim.\n"
            )
        };
        for one in &moves {
            out = format!(
                "{}\n\n{}\n",
                out.trim_end_matches('\n'),
                text[one.from..one.to].trim_end_matches('\n')
            );
        }
        for one in moves.iter().rev() {
            let body = &text[one.from..one.to];
            let stub = format!(
                "{}\n\nMoved to [{hist_rel}]({hist_rel}#{}) on {}, after the owner ruled on it. {}\n\n",
                one.heading,
                anchor(&one.heading),
                args.date,
                verdict(body)
            );
            text = format!("{}{stub}{}", &text[..one.from], &text[one.to..]);
        }
        write_atomic(
            &hist,
            format!("{}\n", out.trim_end_matches('\n')).as_bytes(),
        )?;
        write_atomic(&path, text.as_bytes())?;
    }
    outcome.field("applied", args.apply);
    outcome.field("records", Value::Array(moved_records));
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_cuts_after_twenty_characters() {
        let body = "## Astra review — 2026-11-13\n\n**VERDICT: FIX REQUIRED.** Keep it (mostly). Then more.\n";
        assert_eq!(verdict(body), "VERDICT: FIX REQUIRED.");
        assert_eq!(verdict("Verdict: short."), "Verdict: short.");
        assert_eq!(verdict("nothing here"), "Verdict: see history.");
    }

    #[test]
    fn anchors_drop_punctuation() {
        assert_eq!(
            anchor("## Astra review — 2026-11-13 (gpt-6-astra)"),
            "astra-review--2026-11-13-gpt-6-astra"
        );
    }

    #[test]
    fn stubs_never_move() {
        let pad = "x".repeat(100);
        let text = format!(
            "# R\n\n{pad}\n\n## Astra review — 2026-11-13 (gpt-6-astra high)\n\nFull review: [IDEA-0022.md](../designs/astra-review-20261113-design/notes/IDEA-0022.md). Verdict: **FIX REQUIRED** — sound pilot.\n\n## Regrets sweep — 2026-11-13 (Fable)\n\nVerdict: PASS.\nNotes: [IDEA-0022](../../store/ideas/history/IDEA-0022.md#x)\n\n## Owner rulings — 2026-11-13: the Astra review and regrets sweep\n\nRuled.\n"
        );
        assert!(plan(&text, 10).is_empty());
        let full = text.replace(
            "sound pilot.",
            "sound pilot.\n\n| Severity | Finding |\n| major | one |",
        );
        assert_eq!(plan(&full, 10).len(), 1);
        assert!(!review_link("../designs/x/REPORT.md"));
        assert!(review_link("history/IDEA-0022.md#rubric-review"));
    }

    #[test]
    fn plans_only_ruled_reviews() {
        let pad = "x".repeat(100);
        let text = format!(
            "# R\n\n## Rubric review — 2026-11-01\n\nVerdict: ok.\n{pad}\n## Astra review — 2026-11-20\n\nbody\n## Owner rulings on the rubric review — 2026-11-02\n\nyes\n"
        );
        let moves = plan(&text, 10);
        assert_eq!(moves.len(), 1);
        assert!(moves[0].heading.starts_with("## Rubric review"));
        assert!(plan(&text, 100_000).is_empty());
    }
}
