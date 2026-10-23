//! The review ledger: REVIEW-OFF.md rendered from `.cm/reviews.json`, appended by `cm review`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::host::{Host, stamp};
use crate::ledger::{guard, journal_line, one_line};
use crate::markdown::first_difference;
use crate::outcome::{Outcome, Violation};
use crate::store::{CM_DIR, Target, read_json, write_atomic, write_json};

pub const REVIEW_OFF: &str = "REVIEW-OFF.md";
pub const REVIEWS: &str = "reviews.json";

pub const REVIEW_HEADER: &str = "| # | PR | Kind | Model | Verdict | Confirmed | False positive | Confirmed, missed by the other | Worst severity | Cost | Wall time | Effort reported |";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reviews {
    pub preamble: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "source")]
pub enum Row {
    /// A row written by hand before cm kept the ledger, kept cell for cell.
    Imported {
        cells: Vec<String>,
    },
    Review(Box<Review>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub number: u32,
    pub at: String,
    pub step: String,
    pub pr: String,
    pub head: String,
    pub model: String,
    pub effort: String,
    pub verdict: String,
    pub confirmed: u32,
    pub false_positive: u32,
    pub cost: String,
    pub wall: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

impl Row {
    fn cells(&self) -> Vec<String> {
        match self {
            Row::Imported { cells } => cells.clone(),
            Row::Review(review) => {
                let (repo, number) = pr_key(&review.pr);
                let pr = match &review.delta {
                    Some(from) => format!(
                        "{repo}#{number} @{} (delta from {})",
                        short(&review.head),
                        short(from)
                    ),
                    None => format!("{repo}#{number} @{}", short(&review.head)),
                };
                vec![
                    review.number.to_string(),
                    pr,
                    if review.delta.is_some() {
                        format!("{} (delta)", review.step)
                    } else {
                        review.step.clone()
                    },
                    review.model.clone(),
                    review.verdict.to_uppercase(),
                    review.confirmed.to_string(),
                    review.false_positive.to_string(),
                    "—".into(),
                    "—".into(),
                    format!("${}", review.cost.trim_start_matches('$')),
                    format!("{} min", review.wall),
                    review.effort.clone(),
                ]
            }
        }
    }

    /// (repo, PR number, delta): the render order.
    fn key(&self) -> (String, u64, bool) {
        match self {
            Row::Imported { cells } => {
                let pr = cells.get(1).map_or("", String::as_str);
                let id = pr.split_whitespace().next().unwrap_or_default();
                let (repo, number) = id.split_once('#').unwrap_or((id, ""));
                (
                    repo.to_owned(),
                    number.parse().unwrap_or(u64::MAX),
                    pr.contains("delta"),
                )
            }
            Row::Review(review) => {
                let (repo, number) = pr_key(&review.pr);
                (repo, number, review.delta.is_some())
            }
        }
    }

    fn number(&self) -> u32 {
        match self {
            Row::Imported { cells } => cells
                .first()
                .and_then(|cell| cell.parse().ok())
                .unwrap_or(0),
            Row::Review(review) => review.number,
        }
    }
}

fn pr_key(url: &str) -> (String, u64) {
    let rest = url.strip_prefix("https://github.com/").unwrap_or(url);
    let mut parts = rest.split('/');
    let repo = parts.nth(1).unwrap_or_default().to_owned();
    (
        repo,
        parts
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(u64::MAX),
    )
}

pub fn render_reviews(reviews: &Reviews) -> String {
    let mut rows: Vec<&Row> = reviews.rows.iter().collect();
    rows.sort_by_key(|row| row.key());
    let mut out = reviews.preamble.clone();
    out.push_str(REVIEW_HEADER);
    out.push('\n');
    out.push_str(&"|---".repeat(12));
    out.push_str("|\n");
    for row in rows {
        let cells: Vec<String> = row
            .cells()
            .iter()
            .map(|cell| cell.replace('|', "\\|"))
            .collect();
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out
}

fn split_cells(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = vec![String::new()];
    let mut escaped = false;
    for c in inner.chars() {
        match c {
            '|' if !escaped => cells.push(String::new()),
            _ => cells.last_mut().expect("one cell").push(c),
        }
        escaped = c == '\\';
    }
    cells
        .iter()
        .map(|cell| cell.trim().replace("\\|", "|"))
        .collect()
}

/// Parses a hand-written REVIEW-OFF.md as it stands: the text above the table and its rows.
pub fn parse_reviews(text: &str) -> Result<Reviews, String> {
    let Some(start) = text.find(REVIEW_HEADER) else {
        return Err(format!(
            "{REVIEW_OFF} has no table headed `{REVIEW_HEADER}`"
        ));
    };
    let mut rows = Vec::new();
    for line in text[start..].lines().skip(2) {
        if !line.starts_with('|') {
            if line.trim().is_empty() {
                break;
            }
            return Err(format!(
                "`{line}` follows the table; cm keeps only rows there"
            ));
        }
        let cells = split_cells(line);
        if cells.len() != 12 {
            return Err(format!("`{line}` does not have the table's 12 cells"));
        }
        rows.push(Row::Imported { cells });
    }
    Ok(Reviews {
        preamble: text[..start].to_owned(),
        rows,
    })
}

fn reviews_path(root: &Path) -> PathBuf {
    root.join(CM_DIR).join(REVIEWS)
}

pub fn lint_reviews(root: &Path) -> Vec<Violation> {
    let path = reviews_path(root);
    if !path.exists() {
        return Vec::new();
    }
    let reviews: Reviews = match read_json(&path) {
        Ok(reviews) => reviews,
        Err(error) => {
            return vec![Violation::new(
                ".cm/reviews.json",
                "state-parse",
                format!("{error:#}"),
            )];
        }
    };
    let rendered = render_reviews(&reviews);
    match fs::read_to_string(root.join(REVIEW_OFF)) {
        Ok(text) if text == rendered => Vec::new(),
        Ok(text) => vec![
            Violation::new(
                REVIEW_OFF,
                "review-hand-edit",
                "REVIEW-OFF.md differs from the render of .cm/reviews.json; add rows with `cm review`",
            )
            .at(first_difference(&text, &rendered)),
        ],
        Err(_) => vec![Violation::new(REVIEW_OFF, "review-missing", "REVIEW-OFF.md is gone; `git checkout HEAD -- REVIEW-OFF.md`")],
    }
}

pub struct ReviewArgs<'a> {
    pub pr: &'a str,
    pub head: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    pub verdict: &'a str,
    pub findings: &'a str,
    pub cost: &'a str,
    pub wall: &'a str,
    pub delta: Option<&'a str>,
}

fn load_or_import(root: &Path) -> Result<Result<(Reviews, bool), String>> {
    let path = reviews_path(root);
    if path.exists() {
        return Ok(Ok((read_json(&path)?, false)));
    }
    Ok(match fs::read_to_string(root.join(REVIEW_OFF)) {
        Ok(text) => parse_reviews(&text).map(|reviews| (reviews, true)),
        Err(_) => Ok((
            Reviews {
                preamble: "# Review-off\n\nRendered by cm from `.cm/reviews.json`; add a row with `cm review`.\n\n".into(),
                rows: Vec::new(),
            },
            true,
        )),
    })
}

/// Imports a hand-written REVIEW-OFF.md once, then renders it from the ledger.
pub fn import_reviews(host: &dyn Host, root: &Path) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    if reviews_path(root).exists() {
        violations.push(Violation::new(
            ".cm/reviews.json",
            "review-imported",
            "the ledger is already cm's",
        ));
    }
    let imported = match load_or_import(root)? {
        Ok((reviews, _)) => Some(reviews),
        Err(message) => {
            violations.push(Violation::new(REVIEW_OFF, "review-parse", message));
            None
        }
    };
    let (Some(reviews), true) = (imported, violations.is_empty()) else {
        return Ok(Outcome::refused(violations));
    };
    save_reviews(root, &reviews)?;
    let at = stamp(host.now());
    let journal = journal_line(
        root,
        &at,
        "review import",
        &format!("- imported: {} row(s)\n", reviews.rows.len()),
    )?;
    let mut outcome = Outcome::default();
    outcome.line(format!(
        "imported {} row(s) from {REVIEW_OFF}",
        reviews.rows.len()
    ));
    outcome.field("rows", reviews.rows.len());
    outcome.field("journal", journal);
    Ok(outcome)
}

fn save_reviews(root: &Path, reviews: &Reviews) -> Result<()> {
    fs::create_dir_all(root.join(CM_DIR))?;
    write_json(&reviews_path(root), reviews)?;
    write_atomic(&root.join(REVIEW_OFF), render_reviews(reviews).as_bytes())
}

pub fn review(host: &dyn Host, root: &Path, target: &Target, args: &ReviewArgs) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    let full = format!(
        "{}/{}",
        target.unit,
        target.step.as_deref().unwrap_or_default()
    );
    let hex =
        |sha: &str| (7..=40).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit());
    let mut fail =
        |rule: &'static str, message: String| violations.push(Violation::new(&full, rule, message));
    if crate::forge::GithubRepo::of_pr(args.pr).is_none() || pr_key(args.pr).1 == u64::MAX {
        fail(
            "review-pr",
            format!(
                "`{}` is not https://github.com/<owner>/<repo>/pull/<n>",
                args.pr
            ),
        );
    }
    for (flag, sha) in [("--head", Some(args.head)), ("--delta", args.delta)] {
        if let Some(sha) = sha.filter(|sha| !hex(sha)) {
            fail("review-sha", format!("{flag} `{sha}` is not a commit SHA"));
        }
    }
    if !["accept", "fix-required"].contains(&args.verdict) {
        fail(
            "review-verdict",
            format!("`{}` is not accept or fix-required", args.verdict),
        );
    }
    let findings = args
        .findings
        .split_once('/')
        .and_then(|(c, f)| Some((c.parse::<u32>().ok()?, f.parse::<u32>().ok()?)));
    if findings.is_none() {
        fail(
            "review-findings",
            format!("`{}` is not <confirmed>/<false-positive>", args.findings),
        );
    }
    if args.cost.trim_start_matches('$').parse::<f64>().is_err() {
        fail(
            "review-cost",
            format!("`{}` is not a USD amount", args.cost),
        );
    }
    if args.wall.parse::<f64>().is_err() {
        fail("review-wall", format!("`{}` is not minutes", args.wall));
    }
    for (field, text) in [("--model", args.model), ("--effort", args.effort)] {
        one_line(field, text, &mut violations, &full);
    }
    let loaded = match load_or_import(root)? {
        Ok(loaded) => Some(loaded),
        Err(message) => {
            violations.push(Violation::new(REVIEW_OFF, "review-parse", message));
            None
        }
    };
    if !loaded.as_ref().is_some_and(|(_, fresh)| *fresh) {
        violations.extend(lint_reviews(root));
    }
    let (Some((mut reviews, _)), Some((confirmed, false_positive)), true) =
        (loaded, findings, violations.is_empty())
    else {
        return Ok(Outcome::refused(violations));
    };
    let at = stamp(host.now());
    let number = reviews.rows.iter().map(Row::number).max().unwrap_or(0) + 1;
    reviews.rows.push(Row::Review(Box::new(Review {
        number,
        at: at.clone(),
        step: full.clone(),
        pr: args.pr.to_owned(),
        head: args.head.to_owned(),
        model: args.model.trim().to_owned(),
        effort: args.effort.trim().to_owned(),
        verdict: args.verdict.to_owned(),
        confirmed,
        false_positive,
        cost: args.cost.trim_start_matches('$').to_owned(),
        wall: args.wall.to_owned(),
        delta: args.delta.map(str::to_owned),
    })));
    save_reviews(root, &reviews)?;
    let journal = journal_line(
        root,
        &at,
        &format!("review {full} #{number}: {}", args.verdict),
        &format!(
            "- {REVIEW_OFF}: {} @{} by {}, {confirmed}/{false_positive}\n",
            args.pr,
            short(args.head),
            args.model.trim()
        ),
    )?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{REVIEW_OFF} row #{number}"));
    outcome.field("number", number);
    outcome.field("journal", journal);
    Ok(outcome)
}
