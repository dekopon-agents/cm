use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::evidence::CampaignFile;
use crate::forge::GithubRepo;
use crate::host::{Host, stamp};
use crate::ledger::{
    QueueItem, add_queue_item, declared, evidence_dir, guard, journal_line, one_line, read_existing,
};
use crate::outcome::{Outcome, Violation};
use crate::state::Evidence;
use crate::state::same_commit;
use crate::store::{
    self, CM_DIR, Target, create_new, read_json, relative, write_atomic, write_json,
};

pub const VERDICTS: &str = "verdicts.json";
pub const BLOCK: &str = "cm-review";
pub const SCOPE: &str = "coordinator-scope.md";
pub const SCOPE_HEADING: &str = "## Coordinator scope and rulings";
pub const ASTRA_MODEL: &str = "gpt-6-astra";
pub const ASTRA_EFFORT: &str = "xhigh";
pub const ASTRA_REVIEWER: &str = "astra";
const RECEIPT: &str = "receipt.json";
const NON_CONVERGENCE: &str = "non-convergence";
const RULING: &str = "Fix passes: when a review round closes fewer blocking findings than it opens, or a finding marked fixed returns, one fresh independent reader (Opus, high effort) reviews the head; the PR or design escalates to the owner only if that round also fails to converge. A third FIX REQUIRED by count alone escalates nothing. The session cap of 10 passes stands.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingType {
    Contract,
    Guideline,
    Taste,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Introduced,
    Exposed,
    Inherited,
}

impl FindingType {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "contract" => Some(FindingType::Contract),
            "guideline" => Some(FindingType::Guideline),
            "taste" => Some(FindingType::Taste),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            FindingType::Contract => "contract",
            FindingType::Guideline => "guideline",
            FindingType::Taste => "taste",
        }
    }
}

impl Origin {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "introduced" => Some(Origin::Introduced),
            "exposed" => Some(Origin::Exposed),
            "inherited" => Some(Origin::Inherited),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Origin::Introduced => "introduced",
            Origin::Exposed => "exposed",
            Origin::Inherited => "inherited",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Accept,
    FixRequired,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Verdict::Accept => "ACCEPT",
            Verdict::FixRequired => "FIX REQUIRED",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FindingType,
    pub origin: Origin,
    /// `<path>:<line>`, `<path>:<start>-<end>`, or `-` for a finding with no place in a file.
    pub at: String,
    pub summary: String,
}

impl Finding {
    pub fn blocks(&self) -> bool {
        match (self.kind, self.origin) {
            (FindingType::Contract, Origin::Introduced | Origin::Exposed) => true,
            (FindingType::Contract, Origin::Inherited)
            | (FindingType::Guideline | FindingType::Taste, _) => false,
        }
    }
}

/// A parsed `cm-review` block and the verdict line after it, if the report has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub head: String,
    pub findings: Vec<Finding>,
    pub stated: Option<Verdict>,
}

impl Block {
    pub fn verdict(&self) -> Verdict {
        if self.findings.iter().any(Finding::blocks) {
            Verdict::FixRequired
        } else {
            Verdict::Accept
        }
    }
}

fn is_sha(text: &str) -> bool {
    (7..=40).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_location(text: &str) -> bool {
    if text == "-" {
        return true;
    }
    let Some((path, lines)) = text.rsplit_once(':') else {
        return false;
    };
    let number = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    let lines_fit = match lines.split_once('-') {
        Some((start, end)) => number(start) && number(end),
        None => number(lines),
    };
    !path.is_empty() && !path.contains(char::is_whitespace) && lines_fit
}

fn verdict_line(line: &str) -> Option<Verdict> {
    let bare = line
        .trim()
        .trim_matches(|c: char| c == '*' || c == '`' || c.is_whitespace());
    match bare {
        "ACCEPT" => Some(Verdict::Accept),
        "FIX REQUIRED" => Some(Verdict::FixRequired),
        _ => None,
    }
}

/// A fence opener's marker (three or more backticks or tildes) and its info string.
fn fence(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let count = trimmed.chars().take_while(|c| *c == marker).count();
    (count >= 3).then(|| (marker.to_string().repeat(count), trimmed[count..].trim()))
}

/// One problem with a report's `cm-review` block, at its 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    Missing {
        line: usize,
    },
    /// The report holds `count` `cm-review` blocks; `line` is where the second opens.
    Several {
        line: usize,
        count: usize,
    },
    Unclosed {
        line: usize,
    },
    /// Another fenced block opens after the `cm-review` block.
    FenceAfter {
        line: usize,
    },
    /// Text other than one verdict line follows the block.
    TextAfter {
        line: usize,
    },
    SecondHead {
        line: usize,
    },
    NoHead {
        line: usize,
    },
    Sha {
        line: usize,
        sha: String,
    },
    /// A line that is neither `head:` nor a five-cell finding.
    Shape {
        line: usize,
        text: String,
    },
    Id {
        line: usize,
        id: String,
    },
    DuplicateId {
        line: usize,
        id: String,
    },
    Type {
        line: usize,
        kind: String,
    },
    Origin {
        line: usize,
        origin: String,
    },
    Location {
        line: usize,
        at: String,
    },
    EmptySummary {
        line: usize,
    },
}

impl BlockError {
    pub fn line(&self) -> usize {
        match self {
            BlockError::Missing { line }
            | BlockError::Several { line, .. }
            | BlockError::Unclosed { line }
            | BlockError::FenceAfter { line }
            | BlockError::TextAfter { line }
            | BlockError::SecondHead { line }
            | BlockError::NoHead { line }
            | BlockError::Sha { line, .. }
            | BlockError::Shape { line, .. }
            | BlockError::Id { line, .. }
            | BlockError::DuplicateId { line, .. }
            | BlockError::Type { line, .. }
            | BlockError::Origin { line, .. }
            | BlockError::Location { line, .. }
            | BlockError::EmptySummary { line } => *line,
        }
    }
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockError::Missing { .. } => write!(
                f,
                "the report has no ```{BLOCK} block; a review report ends with one"
            ),
            BlockError::Several { count, .. } => write!(
                f,
                "the report holds {count} ```{BLOCK} blocks; exactly one ends the report"
            ),
            BlockError::Unclosed { .. } => write!(f, "the ```{BLOCK} block is never closed"),
            BlockError::FenceAfter { .. } => write!(
                f,
                "a fenced block follows the ```{BLOCK} block, which ends the report"
            ),
            BlockError::TextAfter { .. } => write!(
                f,
                "text follows the ```{BLOCK} block; only the final ACCEPT or FIX REQUIRED line may"
            ),
            BlockError::SecondHead { .. } => f.write_str("a second `head:` line"),
            BlockError::NoHead { .. } => {
                write!(f, "the ```{BLOCK} block names no `head: <sha>`")
            }
            BlockError::Sha { sha, .. } => write!(f, "`{sha}` is not a commit SHA"),
            BlockError::Shape { text, .. } => write!(
                f,
                "`{text}` is not `head: <sha>` or `<id> | <type> | <origin> | <file:line> | <summary>`"
            ),
            BlockError::Id { id, .. } => {
                write!(f, "`{id}` is not an id of letters, digits, `-` and `_`")
            }
            BlockError::DuplicateId { id, .. } => write!(f, "a second finding `{id}`"),
            BlockError::Type { kind, .. } => {
                write!(f, "the type `{kind}` is not contract, guideline or taste")
            }
            BlockError::Origin { origin, .. } => write!(
                f,
                "the origin `{origin}` is not introduced, exposed or inherited"
            ),
            BlockError::Location { at, .. } => write!(
                f,
                "`{at}` is not <file>:<line>, <file>:<start>-<end> or `-`"
            ),
            BlockError::EmptySummary { .. } => f.write_str("the summary is empty"),
        }
    }
}

/// Parses the `cm-review` block that ends a report. The block is the report's last fenced block;
/// only blank lines and the final `ACCEPT` or `FIX REQUIRED` line may follow it. Every problem is
/// listed with its 1-based line.
pub fn parse_report(text: &str) -> Result<Block, Vec<BlockError>> {
    let lines: Vec<&str> = text.lines().collect();
    // (info, first body line, closing fence line) for every fenced block.
    let mut blocks: Vec<(String, usize, Option<usize>)> = Vec::new();
    let mut open: Option<(String, String, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        match &open {
            None => {
                if let Some((marker, info)) = fence(line) {
                    open = Some((marker, info.to_owned(), index));
                }
            }
            Some((marker, info, start)) => {
                let trimmed = line.trim();
                if trimmed.starts_with(marker.as_str())
                    && trimmed
                        .trim_start_matches(marker.chars().next().unwrap_or('`'))
                        .is_empty()
                {
                    blocks.push((info.clone(), start + 1, Some(index)));
                    open = None;
                }
            }
        }
    }
    if let Some((_, info, start)) = open {
        blocks.push((info, start + 1, None));
    }
    let ours: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, (info, _, _))| info.split_whitespace().next() == Some(BLOCK))
        .map(|(index, _)| index)
        .collect();
    let index = match ours[..] {
        [] => {
            return Err(vec![BlockError::Missing {
                line: lines.len().max(1),
            }]);
        }
        [one] => one,
        _ => {
            return Err(vec![BlockError::Several {
                line: blocks[ours[1]].1,
                count: ours.len(),
            }]);
        }
    };
    let (_, first, close) = &blocks[index];
    let mut errors = Vec::new();
    let Some(close) = *close else {
        return Err(vec![BlockError::Unclosed { line: *first }]);
    };
    if index + 1 != blocks.len() {
        errors.push(BlockError::FenceAfter {
            line: blocks[index + 1].1,
        });
    }
    let after: Vec<(usize, &str)> = lines[close + 1..]
        .iter()
        .enumerate()
        .map(|(offset, line)| (close + 2 + offset, *line))
        .filter(|(_, line)| !line.trim().is_empty())
        .collect();
    let stated = match after[..] {
        [] => None,
        [(_, line)] if verdict_line(line).is_some() => verdict_line(line),
        [(number, _), ..] => {
            errors.push(BlockError::TextAfter { line: number });
            None
        }
    };
    let mut head = None;
    let mut head_line = false;
    let mut findings: Vec<Finding> = Vec::new();
    for (offset, line) in lines[*first..close].iter().enumerate() {
        let number = first + offset + 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(sha) = line.strip_prefix("head:") {
            let sha = sha.trim();
            if head_line {
                errors.push(BlockError::SecondHead { line: number });
            } else if !is_sha(sha) {
                errors.push(BlockError::Sha {
                    line: number,
                    sha: sha.to_owned(),
                });
            } else {
                head = Some(sha.to_lowercase());
            }
            head_line = true;
            continue;
        }
        let cells: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
        let [id, kind, origin, at, summary] = cells[..] else {
            errors.push(BlockError::Shape {
                line: number,
                text: line.to_owned(),
            });
            continue;
        };
        let mut fits = true;
        let mut bad = |error: BlockError| {
            errors.push(error);
            fits = false;
        };
        if !is_id(id) {
            bad(BlockError::Id {
                line: number,
                id: id.to_owned(),
            });
        } else if findings.iter().any(|finding| finding.id == id) {
            bad(BlockError::DuplicateId {
                line: number,
                id: id.to_owned(),
            });
        }
        let kind = FindingType::parse(kind);
        if kind.is_none() {
            bad(BlockError::Type {
                line: number,
                kind: cells[1].to_owned(),
            });
        }
        let origin = Origin::parse(origin);
        if origin.is_none() {
            bad(BlockError::Origin {
                line: number,
                origin: cells[2].to_owned(),
            });
        }
        if !is_location(at) {
            bad(BlockError::Location {
                line: number,
                at: at.to_owned(),
            });
        }
        if summary.is_empty() {
            bad(BlockError::EmptySummary { line: number });
        }
        if let (true, Some(kind), Some(origin)) = (fits, kind, origin) {
            findings.push(Finding {
                id: id.to_owned(),
                kind,
                origin,
                at: at.to_owned(),
                summary: summary.to_owned(),
            });
        }
    }
    if !head_line {
        errors.push(BlockError::NoHead { line: *first });
    }
    match (head, errors.is_empty()) {
        (Some(head), true) => Ok(Block {
            head,
            findings,
            stated,
        }),
        _ => Err(errors),
    }
}

// The ledger: .cm/verdicts.json

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdicts {
    #[serde(default)]
    pub reviews: Vec<Recorded>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewKind {
    #[default]
    Head,
    Resolution,
}

impl ReviewKind {
    fn is_head(&self) -> bool {
        *self == ReviewKind::Head
    }

    fn name(self) -> &'static str {
        match self {
            ReviewKind::Head => "head",
            ReviewKind::Resolution => "resolution",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub number: u32,
    pub at: String,
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    pub head: String,
    #[serde(default, skip_serializing_if = "ReviewKind::is_head")]
    pub kind: ReviewKind,
    pub verdict: Verdict,
    /// The verdict line the report ends with, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stated: Option<Verdict>,
    /// The report as recorded, copied under the step's `evidence/reviews/`.
    pub report: CampaignFile,
    #[serde(default)]
    pub findings: Vec<Logged>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logged {
    #[serde(flatten)]
    pub finding: Finding,
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub at: String,
    pub by: By,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Commit(String),
    Ruling(CampaignFile),
}

impl fmt::Display for By {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            By::Commit(sha) => write!(f, "commit {sha}"),
            By::Ruling(file) => write!(f, "ruling {file}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    pub at: String,
    pub by: String,
}

impl Recorded {
    pub fn id(&self) -> String {
        format!("R{}", self.number)
    }

    pub fn unresolved(&self) -> Vec<&Logged> {
        self.findings
            .iter()
            .filter(|logged| logged.blocking && logged.resolution.is_none())
            .collect()
    }

    /// The review was of `head`; a report may name the head by a prefix of at least 7.
    pub fn at_head(&self, head: &str) -> bool {
        same_commit(&self.head, head)
    }

    fn binds(&self, at: &str) -> bool {
        match self.kind {
            ReviewKind::Head => self.at_head(at),
            ReviewKind::Resolution => self.head.eq_ignore_ascii_case(at),
        }
    }
}

fn verdicts_path(root: &Path) -> PathBuf {
    root.join(CM_DIR).join(VERDICTS)
}

pub fn load(root: &Path) -> Result<Verdicts> {
    let path = verdicts_path(root);
    if path.exists() {
        read_json(&path)
    } else {
        Ok(Verdicts::default())
    }
}

fn save(root: &Path, verdicts: &Verdicts) -> Result<()> {
    fs::create_dir_all(root.join(CM_DIR))?;
    write_json(&verdicts_path(root), verdicts)
}

fn full_name(target: &Target) -> String {
    format!(
        "{}/{}",
        target.unit,
        target.step.as_deref().unwrap_or_default()
    )
}

fn blocking_count(review: &Recorded) -> usize {
    review
        .findings
        .iter()
        .filter(|finding| finding.blocking)
        .count()
}

fn previous_review<'a>(
    previous: &'a [Recorded],
    step: &str,
    pr: Option<&str>,
    part: Option<&str>,
    reviewer: Option<&str>,
    round: impl Fn(&Recorded) -> bool,
) -> Option<&'a Recorded> {
    previous.iter().rev().find(|before| {
        before.step == step
            && before.pr.as_deref() == pr
            && before.part.as_deref() == part
            && before.reviewer.as_deref() == reviewer
            && round(before)
    })
}

fn same_round(review: &Recorded, kind: ReviewKind, head: &str) -> bool {
    review.kind == kind && (kind.is_head() || review.binds(head))
}

pub(crate) fn fresh_readers(verdicts: &Verdicts) -> Vec<&Recorded> {
    let mut previous = HashMap::new();
    let mut pending = HashMap::new();
    for review in &verdicts.reviews {
        let scope = (
            review.step.as_str(),
            review.pr.as_deref(),
            review.part.as_deref(),
            (!review.kind.is_head()).then(|| review.head.to_ascii_lowercase()),
        );
        let count = blocking_count(review);
        let before = previous.insert((scope.clone(), review.reviewer.as_deref()), count);
        if review.reviewer.as_deref() == Some("fresh-opus") {
            pending.remove(&scope);
        } else if before.is_some_and(|before| count > before) {
            pending.insert(scope, review);
        }
    }
    let mut pending: Vec<_> = pending.into_values().collect();
    pending.sort_by_key(|review| review.number);
    pending
}

fn fix_heads<'a>(reviews: impl Iterator<Item = &'a Recorded>) -> Vec<&'a str> {
    let mut heads: Vec<&str> = Vec::new();
    for review in reviews.filter(|review| review.verdict == Verdict::FixRequired) {
        if !heads.iter().any(|head| same_commit(head, &review.head)) {
            heads.push(review.head.as_str());
        }
    }
    heads
}

fn escalation_pending(root: &Path, step: &str) -> Result<bool> {
    let title = format!("{step}: non-converging fresh reader");
    Ok(
        read_existing(&root.join(crate::templates::QUEUE_FILE))?.is_some_and(|queue| {
            crate::markdown::headings(&queue).iter().any(|heading| {
                heading.level == 2
                    && heading
                        .text
                        .split_once(". ")
                        .is_some_and(|(_, text)| text == title)
            })
        }),
    )
}

fn step_pr(root: &Path, target: &Target, part: Option<&str>) -> Result<Option<String>> {
    let dir = root.join(&target.unit);
    if !store::is_unit(&dir) {
        return Ok(None);
    }
    Ok(store::load_unit(&dir)?
        .steps
        .get(target.step.as_deref().unwrap_or_default())
        .and_then(|step| match part {
            Some(part) => step.parts.get(part),
            None => Some(&step.track),
        })
        .and_then(|track| track.evidence.pr.clone()))
}

pub fn record(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    report: &Path,
    reviewer: Option<&str>,
    part: Option<&str>,
) -> Result<Outcome> {
    record_as(host, root, target, report, reviewer, part, None)
}

pub fn record_resolution(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    report: &Path,
    reviewer: Option<&str>,
    part: Option<&str>,
    repo: Option<&Path>,
) -> Result<Outcome> {
    record_as(
        host,
        root,
        target,
        report,
        reviewer,
        part,
        Some(Resolving { repo }),
    )
}

struct Resolving<'a> {
    repo: Option<&'a Path>,
}

fn step_worktree(root: &Path, target: &Target) -> Option<PathBuf> {
    let unit = store::load_unit(&root.join(&target.unit)).ok()?;
    let step = unit.steps.get(target.step.as_deref()?)?;
    let worktree = PathBuf::from(&step.worktree);
    (!step.worktree.is_empty() && worktree.exists()).then_some(worktree)
}

fn resolution_parent(
    root: &Path,
    target: &Target,
    part: Option<&str>,
    pr: Option<&str>,
    merge: &str,
    reviewer: Option<&str>,
    repo: Option<&Path>,
) -> Result<(String, String), String> {
    if Family::reviewer(reviewer).is_none() {
        return Err(
            "a resolution review counts only for its reviewer's family: name a Codex reviewer (astra, sol, terra, gpt-…) or a Claude one (claude, opus, fresh-opus, fable, sonnet, haiku) with --reviewer"
                .to_owned(),
        );
    }
    let Some(repo) = repo
        .map(Path::to_path_buf)
        .or_else(|| step_worktree(root, target))
    else {
        return Err(
            "a resolution review is checked against the merge in a checkout; pass --repo <a checkout of the repo>"
                .to_owned(),
        );
    };
    let base = ORIGIN_HEAD;
    if git_out(
        &repo,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )
    .is_none()
    {
        return Err(format!(
            "cannot read {}, the base branch's tip, in {}; fetch the base branch there",
            shown(base),
            repo.display()
        ));
    }
    let named = merge;
    let Some(merge) = git_out(
        &repo,
        &["rev-parse", "--verify", &format!("{named}^{{commit}}")],
    ) else {
        return Err(CleanUpdateError::Unreadable {
            commit: named.to_owned(),
            repo,
        }
        .to_string());
    };
    if !merge
        .to_ascii_lowercase()
        .starts_with(&named.to_ascii_lowercase())
    {
        return Err(format!(
            "{named} resolves to {merge} in {}, a commit it does not name: a ref of that name shadows it",
            repo.display()
        ));
    }
    let unreadable = || {
        CleanUpdateError::Unreadable {
            commit: merge.clone(),
            repo: repo.clone(),
        }
        .to_string()
    };
    let isolated =
        Isolated::over(&repo).map_err(|why| why.map_or_else(unreadable, |why| why.to_string()))?;
    let merged = merge_from_base(&repo, &isolated, &merge, base).map_err(|why| why.to_string())?;
    if merged.clean {
        return Err(format!(
            "{} is a clean update from the base branch: its remerge diff is empty, so it needs no review",
            short(&merge)
        ));
    }
    if let Some(gap) = beyond_resolution(&repo, &isolated, &merge, &merged) {
        return Err(format!(
            "{} is no conflict resolution a resolution review can cover: {gap}; it needs a review of its own",
            short(&merge)
        ));
    }
    let first = merged.first;
    let lineage = Lineage {
        repo: &repo,
        base: Some(base),
    };
    let cleared = gate(root, target, part, pr, &first, Some(&lineage), true)
        .map_err(|error| format!("{error:#}"))?;
    if cleared.gaps.is_empty() {
        Ok((merge, first))
    } else {
        Err(format!(
            "{}'s first parent {} does not clear the gate, so no resolution review can stand on it: {}",
            short(&merge),
            short(&first),
            cleared.gaps.join("; ")
        ))
    }
}

fn record_as(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    report: &Path,
    reviewer: Option<&str>,
    part: Option<&str>,
    resolving: Option<Resolving>,
) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    let full = full_name(target);
    let mut non_convergence_block = false;
    if !declared(root, target)? {
        violations.push(Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        ));
    } else {
        let unit = store::load_unit(&root.join(&target.unit))?;
        let step = &unit.steps[target.step.as_deref().unwrap_or_default()];
        non_convergence_block = matches!(&step.track.state,
            crate::state::Phase::Blocked(reason) if reason == NON_CONVERGENCE);
        if let Some(part) = part {
            if !step.parts.contains_key(part) {
                violations.push(Violation::new(
                    &full,
                    "part-unknown",
                    format!("the step declares no part `{part}`"),
                ));
            }
        } else if !step.parts.is_empty() {
            violations.push(Violation::new(
                &full,
                "review-part",
                "the step has parts; select the reviewed part with --part NAME",
            ));
        }
    }
    if let Some(kind) = reviewer.filter(|kind| !store::is_name(kind)) {
        violations.push(Violation::new(
            &full,
            "review-reviewer",
            format!("`{kind}` is not a name of letters, digits, `.`, `_` and `-`"),
        ));
    }
    let shown = report.display().to_string();
    let block = match fs::read_to_string(report) {
        Err(error) => {
            violations.push(Violation::new(
                &shown,
                "review-report",
                format!("cannot read the report: {error}"),
            ));
            None
        }
        Ok(text) => match parse_report(&text) {
            Ok(block) => Some((block, text)),
            Err(errors) => {
                violations.extend(errors.into_iter().map(|error| {
                    Violation::new(&shown, "review-block", error.to_string()).at(error.line())
                }));
                None
            }
        },
    };
    let mut verdicts = load(root)?;
    let pr = step_pr(root, target, part)?;
    let mut first_parent = None;
    let mut canonical = None;
    if let Some(resolving) = &resolving
        && let Some((block, _)) = &block
    {
        match resolution_parent(
            root,
            target,
            part,
            pr.as_deref(),
            &block.head,
            reviewer,
            resolving.repo,
        ) {
            Ok((merge, parent)) => {
                canonical = Some(merge);
                first_parent = Some(parent);
            }
            Err(why) => violations.push(Violation::new(&full, "review-resolution", why)),
        }
    }
    let heads = fix_heads(
        verdicts
            .reviews
            .iter()
            .filter(|review| review.step == full && review.pr == pr),
    );
    let new_pass = block.as_ref().is_some_and(|(block, _)| {
        block.verdict() == Verdict::FixRequired
            && !heads
                .iter()
                .any(|head| same_commit(head, canonical.as_deref().unwrap_or(&block.head)))
    });
    let passes = heads.len() + usize::from(new_pass);
    if new_pass && passes > 10 {
        violations.push(Violation::new(
            &full,
            "review-cap",
            "session cap of 10 passes; an 11th fix pass on the current PR is refused",
        ));
    }
    let number = verdicts
        .reviews
        .iter()
        .map(|review| review.number)
        .max()
        .unwrap_or(0)
        + 1;
    let name = match reviewer {
        Some(kind) => format!("R{number}-{kind}.md"),
        None => format!("R{number}.md"),
    };
    let copy = evidence_dir(root, target).join("reviews").join(name);
    let copy_rel = relative(root, &copy);
    if copy.exists() {
        violations.push(Violation::new(
            &copy_rel,
            "review-exists",
            "a report is already recorded at this path; .cm/verdicts.json is behind the step's evidence",
        ));
    }
    if let Some((_, text)) = &block
        // A report is recorded once per step, except that one recorded before the step had parts
        // may be recorded again for a part, so its receipt can bind there; a report recorded for
        // one part never serves another.
        && let Some(again) = verdicts.reviews.iter().find(|review| {
            review.step == full
                && (review.part.is_some()
                    || (review.part.as_deref() == part
                        // A review recorded before pr-open names no PR: it stands for this one.
                        && (review.pr.is_none() || pr.is_none() || review.pr == pr)))
                && fs::read_to_string(root.join(review.report.as_str()))
                    .ok()
                    .as_ref()
                    == Some(text)
        })
    {
        violations.push(Violation::new(
            &full,
            "review-duplicate",
            format!(
                "this report is already recorded as {} ({})",
                again.id(),
                again.report
            ),
        ));
    }
    let Some((block, text)) = block.filter(|_| violations.is_empty()) else {
        return Ok(Outcome::refused(violations));
    };
    let head = canonical.unwrap_or_else(|| block.head.clone());
    let kind = if resolving.is_some() {
        ReviewKind::Resolution
    } else {
        ReviewKind::Head
    };
    let round = |review: &Recorded| same_round(review, kind, &head);
    let verdict = block.verdict();
    let at = stamp(host.now());
    let part_heads =
        fix_heads(verdicts.reviews.iter().filter(|review| {
            review.step == full && review.pr == pr && review.part.as_deref() == part
        }));
    let rounds = part_heads.len()
        + usize::from(
            verdict == Verdict::FixRequired && !part_heads.iter().any(|at| same_commit(at, &head)),
        );
    let findings: Vec<Logged> = block
        .findings
        .iter()
        .map(|finding| Logged {
            blocking: finding.blocks(),
            finding: finding.clone(),
            resolution: None,
            closed: None,
        })
        .collect();
    let previous = if reviewer == Some("fresh-opus") {
        fresh_readers(&verdicts)
            .into_iter()
            .find(|review| {
                review.step == full
                    && review.pr == pr
                    && review.part.as_deref() == part
                    && round(review)
            })
            .or_else(|| {
                verdicts.reviews.iter().rev().find(|review| {
                    review.step == full
                        && review.pr == pr
                        && review.part.as_deref() == part
                        && round(review)
                })
            })
            .map(|baseline| {
                verdicts
                    .reviews
                    .iter()
                    .filter(|review| {
                        review.step == full
                            && review.pr == pr
                            && review.part.as_deref() == part
                            && review.at_head(&baseline.head)
                            && round(review)
                    })
                    .max_by_key(|review| blocking_count(review))
                    .unwrap_or(baseline)
            })
    } else {
        previous_review(
            &verdicts.reviews,
            &full,
            pr.as_deref(),
            part,
            reviewer,
            round,
        )
    };
    let converging = match previous {
        Some(before) => Some(
            findings.iter().filter(|finding| finding.blocking).count() <= blocking_count(before),
        ),
        None if reviewer == Some("fresh-opus") => None,
        None => Some(true),
    };
    let escalate = converging == Some(false) && reviewer == Some("fresh-opus");
    let mut queued = None;
    if escalate && !non_convergence_block && !escalation_pending(root, &full)? {
        let earlier = previous.expect("a non-converging review has a previous round");
        let title = format!("{full}: non-converging fresh reader");
        let context = format!(
            "{full}{} still fails to converge after an independent Opus read; the reports are `{}` and `{copy_rel}`. Ruling: {RULING}",
            part.map(|part| format!(" part {part}")).unwrap_or_default(),
            earlier.report,
        );
        let question = format!("Should {full} take another fix round, be re-scoped, or stop?");
        let item = QueueItem {
            title: &title,
            context: &context,
            question: &question,
            recommendation: "Read the reports; rule in the step's evidence/coordinator-scope.md, then `cm unblock` it or re-scope it",
        };
        let outcome = add_queue_item(host, root, &item, Vec::new())?;
        if !outcome.ok() {
            return Ok(outcome);
        }
        queued = outcome.fields.get("item").cloned();
    }
    fs::create_dir_all(copy.parent().unwrap_or(root))?;
    if !create_new(&copy, text.as_bytes())? {
        return Ok(Outcome::refused(vec![Violation::new(
            &copy_rel,
            "review-exists",
            "written meanwhile",
        )]));
    }
    let report_file = CampaignFile::resolve(root, &copy).map_err(anyhow::Error::from)?;
    let recorded = Recorded {
        number,
        at: at.clone(),
        step: full.clone(),
        reviewer: reviewer.map(str::to_owned),
        pr: pr.clone(),
        part: part.map(str::to_owned),
        head: head.clone(),
        kind,
        verdict,
        stated: block.stated,
        report: report_file.clone(),
        findings,
    };
    let id = recorded.id();
    let blocking: Vec<String> = recorded
        .findings
        .iter()
        .filter(|logged| logged.blocking)
        .map(|logged| format!("{id}.{}", logged.finding.id))
        .collect();
    let followups: Vec<String> = recorded
        .findings
        .iter()
        .filter(|logged| !logged.blocking)
        .map(|logged| format!("{id}.{}", logged.finding.id))
        .collect();
    // The campaign is stamped before the ledger holds a resolution review, so a cm that would read
    // one as an ACCEPT of the head refuses the campaign instead.
    let mut stamped = false;
    if recorded.kind == ReviewKind::Resolution {
        let mut campaign = store::load_campaign(root)?;
        if campaign.schema < crate::state::RESOLUTION_SCHEMA {
            campaign.schema = crate::state::RESOLUTION_SCHEMA;
            write_json(&store::state_path(root), &campaign)?;
            stamped = true;
        }
    }
    verdicts.reviews.push(recorded);
    save(root, &verdicts)?;
    let mut body = format!(
        "- report: `{copy_rel}` @{}{}\n",
        head,
        reviewer
            .map(|kind| format!(" by {kind}"))
            .unwrap_or_default()
    );
    if !blocking.is_empty() {
        body.push_str(&format!("- blocking: {}\n", blocking.join(", ")));
    }
    if !followups.is_empty() {
        body.push_str(&format!("- follow-ups: {}\n", followups.join(", ")));
    }
    let mismatch = block.stated.filter(|stated| *stated != verdict);
    if let Some(stated) = mismatch {
        body.push_str(&format!(
            "- the report says {stated}; cm computes {verdict} from its findings\n"
        ));
    }
    if let Some(part) = part {
        body.push_str(&format!("- part: {part}\n"));
    }
    if let Some(parent) = &first_parent {
        body.push_str(&format!(
            "- resolution: the conflict resolutions of a merge from the base branch; its first parent {parent} clears\n"
        ));
    }
    if stamped {
        body.push_str(&format!(
            "- schema: the campaign is at schema {} now; an older cm refuses it\n",
            crate::state::RESOLUTION_SCHEMA
        ));
    }
    if converging == Some(false) {
        body.push_str(&format!("- converging: false; ruling: “{RULING}”\n"));
    }
    let journal = journal_line(
        root,
        &at,
        &format!("review record {full} {id}: {verdict}"),
        &body,
    )?;
    let mut blocked = None;
    if escalate {
        let evidence = Evidence {
            file: Some(report_file),
            ..Evidence::default()
        };
        blocked = crate::commands::block_step(root, target, NON_CONVERGENCE, &at, &evidence)?;
    }
    let mut outcome = Outcome::default();
    outcome.line(format!(
        "{id} {full} @{}: {verdict} ({} blocking, {} follow-up(s)); report {copy_rel}",
        head,
        blocking.len(),
        followups.len()
    ));
    if let Some(stated) = mismatch {
        outcome.line(format!(
            "the report says {stated}; cm computes {verdict}: only an introduced or exposed contract finding blocks"
        ));
    }
    if let Some(parent) = &first_parent {
        outcome.line(format!(
            "resolution review of the merge; its first parent {parent} clears"
        ));
        outcome.field("kind", "resolution");
        outcome.field("first_parent", parent.clone());
    }
    if stamped {
        outcome.line(format!(
            "the campaign is at schema {} now; an older cm refuses it",
            crate::state::RESOLUTION_SCHEMA
        ));
    }
    if let Some(converging) = converging {
        outcome.line(format!("converging: {converging}"));
    }
    if escalate {
        outcome.line(format!(
            "non-converging fresh reader: {full}{}; owner-queue item {}",
            if blocked.is_some() {
                format!(" → blocked:{NON_CONVERGENCE}")
            } else {
                String::new()
            },
            queued
                .as_ref()
                .map_or_else(|| "—".to_owned(), Value::to_string)
        ));
    }
    outcome.field("id", id);
    outcome.field("verdict", verdict.to_string());
    outcome.field("head", head);
    outcome.field("report", copy_rel);
    outcome.field("blocking", blocking);
    outcome.field("followups", followups);
    outcome.field("part", part.map(str::to_owned));
    outcome.field("converging", converging);
    outcome.field("rounds", rounds);
    outcome.field("passes", passes);
    outcome.field("owner_queue", queued.unwrap_or(Value::Null));
    outcome.field("blocked", blocked.is_some());
    outcome.field("journal", journal);
    Ok(outcome)
}

/// What `--by` names: a ruling file inside the campaign, else a commit SHA.
fn by_of(root: &Path, text: &str, path: &Path) -> Result<By, String> {
    if path.exists() {
        return CampaignFile::resolve(root, path)
            .map(By::Ruling)
            .map_err(|error| error.to_string());
    }
    if is_sha(text) {
        return Ok(By::Commit(text.to_lowercase()));
    }
    Err(format!(
        "`{text}` is neither a commit SHA nor a ruling file in the campaign"
    ))
}

/// `R<n>.<id>` names one review's finding; a bare `<id>` the step's latest review holding it.
fn find<'a>(
    verdicts: &'a mut Verdicts,
    step: Option<&str>,
    id: &str,
) -> Option<(String, &'a mut Logged)> {
    let (number, bare) = match id.split_once('.') {
        Some((review, bare)) => (Some(review.strip_prefix('R')?.parse::<u32>().ok()?), bare),
        None => (None, id),
    };
    let review = verdicts.reviews.iter_mut().rev().find(|review| {
        step.is_none_or(|step| review.step == step)
            && number.is_none_or(|number| review.number == number)
            && review
                .findings
                .iter()
                .any(|logged| logged.finding.id == bare)
    })?;
    let key = format!("{}.{bare}", review.id());
    let logged = review
        .findings
        .iter_mut()
        .find(|logged| logged.finding.id == bare)?;
    Some((key, logged))
}

pub fn resolve(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    id: &str,
    by: &str,
    by_path: &Path,
) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    let full = full_name(target);
    let mut verdicts = load(root)?;
    let by = match by_of(root, by, by_path) {
        Ok(by) => Some(by),
        Err(message) => {
            violations.push(Violation::new(&full, "resolve-by", message));
            None
        }
    };
    let at = stamp(host.now());
    let Some((key, logged)) = find(&mut verdicts, Some(&full), id) else {
        violations.push(Violation::new(
            &full,
            "finding-unknown",
            format!("no recorded review of {full} holds a finding `{id}`"),
        ));
        return Ok(Outcome::refused(violations));
    };
    if !logged.blocking {
        violations.push(Violation::new(
            &full,
            "resolve-not-blocking",
            format!("{key} is a follow-up, not a blocking finding; close it with `cm followups close {key} --by …`"),
        ));
    }
    if let Some(done) = &logged.resolution {
        violations.push(Violation::new(
            &full,
            "resolve-done",
            format!("{key} was resolved at {} by {}", done.at, done.by),
        ));
    }
    let Some(by) = by.filter(|_| violations.is_empty()) else {
        return Ok(Outcome::refused(violations));
    };
    logged.resolution = Some(Resolution {
        at: at.clone(),
        by: by.clone(),
    });
    save(root, &verdicts)?;
    let journal = journal_line(
        root,
        &at,
        &format!("review resolve {full} {key}"),
        &format!("- by: {by}\n"),
    )?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{key} resolved by {by}"));
    outcome.field("id", key);
    outcome.field("by", serde_json::to_value(&by)?);
    outcome.field("journal", journal);
    Ok(outcome)
}

// Follow-ups

fn followup_json(review: &Recorded, logged: &Logged) -> Value {
    json!({
        "id": format!("{}.{}", review.id(), logged.finding.id),
        "step": review.step,
        "pr": review.pr,
        "head": review.head,
        "type": logged.finding.kind.name(),
        "origin": logged.finding.origin.name(),
        "at": logged.finding.at,
        "summary": logged.finding.summary,
        "status": if logged.closed.is_some() { "closed" } else { "open" },
        "closed": logged.closed.as_ref().map(|closed| json!({"at": closed.at, "by": closed.by})),
    })
}

fn pr_short(pr: Option<&str>) -> String {
    pr.and_then(|url| {
        let repo = GithubRepo::of_pr(url)?;
        let number = url.rsplit('/').next()?;
        Some(format!("{repo}#{number}"))
    })
    .unwrap_or_else(|| "—".to_owned())
}

/// Every non-blocking finding recorded in the campaign; `epoch` lists only the open ones, grouped
/// by repository as Markdown checklist lines for the epoch's cleanup PR.
pub fn followups(root: &Path, epoch: bool) -> Result<Outcome> {
    let verdicts = load(root)?;
    let all: Vec<(&Recorded, &Logged)> = verdicts
        .reviews
        .iter()
        .flat_map(|review| {
            review
                .findings
                .iter()
                .filter(|logged| !logged.blocking)
                .map(move |logged| (review, logged))
        })
        .filter(|(_, logged)| !epoch || logged.closed.is_none())
        .collect();
    let mut outcome = Outcome::default();
    if epoch {
        let mut repos: Vec<String> = all
            .iter()
            .map(|(review, _)| {
                review
                    .pr
                    .as_deref()
                    .and_then(GithubRepo::of_pr)
                    .map_or_else(|| "—".to_owned(), |repo| repo.to_string())
            })
            .collect();
        repos.sort();
        repos.dedup();
        for repo in &repos {
            outcome.line(format!("### {repo}"));
            outcome.line(String::new());
            for (review, logged) in all.iter().filter(|(review, _)| {
                review
                    .pr
                    .as_deref()
                    .and_then(GithubRepo::of_pr)
                    .map_or_else(|| "—".to_owned(), |repo| repo.to_string())
                    == *repo
            }) {
                outcome.line(format!(
                    "- [ ] `{}` ({}, {}): {} — {}.{}, {} {}",
                    logged.finding.at,
                    logged.finding.kind.name(),
                    logged.finding.origin.name(),
                    logged.finding.summary,
                    review.id(),
                    logged.finding.id,
                    review.step,
                    pr_short(review.pr.as_deref()),
                ));
            }
            outcome.line(String::new());
        }
    } else {
        for (review, logged) in &all {
            let status = logged.closed.as_ref().map_or_else(
                || "open".to_owned(),
                |closed| format!("closed ({})", closed.by),
            );
            outcome.line(format!(
                "{}.{} {status} {} {} {}/{} {} — {}",
                review.id(),
                logged.finding.id,
                review.step,
                pr_short(review.pr.as_deref()),
                logged.finding.kind.name(),
                logged.finding.origin.name(),
                logged.finding.at,
                logged.finding.summary,
            ));
        }
    }
    if all.is_empty() {
        outcome.line(if epoch {
            "no open follow-ups"
        } else {
            "no follow-ups recorded"
        });
    }
    outcome.field(
        "followups",
        all.iter()
            .map(|(review, logged)| followup_json(review, logged))
            .collect::<Vec<_>>(),
    );
    Ok(outcome)
}

pub fn close_followup(host: &dyn Host, root: &Path, id: &str, by: &str) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    one_line("--by", by, &mut violations, VERDICTS);
    let mut verdicts = load(root)?;
    let at = stamp(host.now());
    let Some((key, logged)) = id
        .contains('.')
        .then(|| find(&mut verdicts, None, id))
        .flatten()
    else {
        violations.push(Violation::new(
            VERDICTS,
            "finding-unknown",
            format!("`{id}` names no recorded follow-up; ids are R<n>.<finding>, as `cm followups` lists them"),
        ));
        return Ok(Outcome::refused(violations));
    };
    if logged.blocking {
        violations.push(Violation::new(
            VERDICTS,
            "followup-blocking",
            format!("{key} is a blocking finding; resolve it with `cm review resolve <step> {key} --by …`"),
        ));
    }
    if let Some(closed) = &logged.closed {
        violations.push(Violation::new(
            VERDICTS,
            "followup-closed",
            format!("{key} was closed at {} by {}", closed.at, closed.by),
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    logged.closed = Some(Closed {
        at: at.clone(),
        by: by.trim().to_owned(),
    });
    save(root, &verdicts)?;
    let journal = journal_line(
        root,
        &at,
        &format!("followups close {key}"),
        &format!("- by: {}\n", by.trim()),
    )?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{key} closed by {}", by.trim()));
    outcome.field("id", key);
    outcome.field("journal", journal);
    Ok(outcome)
}

// Briefs: <step>/evidence/coordinator-scope.md

pub fn scope_path(root: &Path, target: &Target) -> PathBuf {
    evidence_dir(root, target).join(SCOPE)
}

pub fn ruling(host: &dyn Host, root: &Path, target: &Target, text: &str) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    let full = full_name(target);
    if !declared(root, target)? {
        violations.push(Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        ));
    }
    one_line("the ruling", text, &mut violations, &full);
    let path = scope_path(root, target);
    let rel = relative(root, &path);
    let current = read_existing(&path)?;
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let now = host.now();
    let at = stamp(now);
    let date = now.strftime("%Y-%m-%d").to_string();
    let mut next = current
        .clone()
        .unwrap_or_else(|| format!("# Scope and coordinator rulings: {full}\n\n"));
    if !next.ends_with('\n') {
        next.push('\n');
    }
    let line = format!("- {date}: {}", text.trim());
    next.push_str(&line);
    next.push('\n');
    fs::create_dir_all(evidence_dir(root, target))?;
    write_atomic(&path, next.as_bytes())?;
    let mut body = format!("- {rel}: `{line}`\n");
    if current.is_none() {
        body.push_str("- created the scope file\n");
    }
    let journal = journal_line(root, &at, &format!("ruling {full}"), &body)?;
    let mut outcome = Outcome::default();
    outcome.line(format!("{rel}: {line}"));
    outcome.field("path", rel);
    outcome.field("line", line);
    outcome.field("journal", journal);
    Ok(outcome)
}

/// The step's kickoff, then its scope file under [`SCOPE_HEADING`]: the brief the coordinator
/// hands every reviewer.
pub fn brief(root: &Path, target: &Target) -> Result<Outcome> {
    let full = full_name(target);
    let kickoff = root.join(&full).join("kickoff.md");
    let Some(kickoff_text) = read_existing(&kickoff)? else {
        return Ok(Outcome::refused(vec![Violation::new(
            &full,
            "packet-kickoff",
            "the step folder has no kickoff.md",
        )]));
    };
    let path = scope_path(root, target);
    let rel = relative(root, &path);
    let scope = read_existing(&path)?;
    let mut text = kickoff_text.trim_end().to_owned();
    text.push_str(&format!("\n\n{SCOPE_HEADING}\n\n"));
    match &scope {
        Some(scope) => text.push_str(scope.trim_end()),
        None => text.push_str(&format!(
            "No rulings are recorded for {full} (`{rel}` does not exist)."
        )),
    }
    let mut outcome = Outcome::default();
    outcome.line(text.clone());
    outcome.field("brief", text);
    outcome.field("kickoff", relative(root, &kickoff));
    outcome.field("scope", scope.is_some().then_some(rel));
    Ok(outcome)
}

// The codex merge gate's review half: (a) an Astra receipt and (b) a resolved review at the head.

fn receipts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            receipts(&path, out);
        } else if path.file_name().is_some_and(|name| name == RECEIPT) {
            out.push(path);
        }
    }
}

/// Why a receipt does not show a review that happened, as `astra-review.sh collect` judges it
/// before printing REVIEW_BLOCKED: the turn completed in a fresh, unforked one-turn thread.
fn unfinished(receipt: &Value) -> Option<&'static str> {
    let null = |pointer: &str| receipt.pointer(pointer) == Some(&Value::Null);
    if receipt.pointer("/turn/status").and_then(Value::as_str) != Some("completed") {
        Some("turn not completed")
    } else if !null("/thread_fresh/forkedFromId") || !null("/thread_fresh/parentThreadId") {
        Some("thread forked or has a parent")
    } else if receipt
        .pointer("/thread_fresh/turns_in_thread")
        .and_then(Value::as_u64)
        != Some(1)
    {
        Some("thread holds other than one turn")
    } else {
        None
    }
}

/// The SHA-256 of a recorded review's report copy, lower-case hex, as `sha256sum` prints it.
fn report_sha256(root: &Path, review: &Recorded) -> Option<String> {
    #[cfg(test)]
    tests::HASHES.with(|hashes| hashes.set(hashes.get() + 1));
    let mut report = fs::File::open(root.join(review.report.as_str())).ok()?;
    let mut hasher = Sha256::new();
    io::copy(&mut report, &mut hasher).ok()?;
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn finished_receipt_at(root: &Path, target: &Target, verdicts: &Verdicts, head: &str) -> bool {
    let mut found = Vec::new();
    receipts(&evidence_dir(root, target), &mut found);
    let step = full_name(target);
    let mut resolutions: Option<Vec<String>> = None;
    found.iter().any(|path| {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|receipt| {
                receipt
                    .pointer("/head")
                    .and_then(Value::as_str)
                    .is_some_and(|at| same_commit(at, head))
                    && unfinished(&receipt).is_none()
                    && !receipt
                        .pointer("/report_sha256")
                        .and_then(Value::as_str)
                        .is_some_and(|want| {
                            resolutions
                                .get_or_insert_with(|| {
                                    verdicts
                                        .reviews
                                        .iter()
                                        .filter(|review| {
                                            review.step == step
                                                && review.binds(head)
                                                && review.kind == ReviewKind::Resolution
                                        })
                                        .filter_map(|review| report_sha256(root, review))
                                        .collect()
                                })
                                .iter()
                                .any(|sha256| sha256.eq_ignore_ascii_case(want))
                        })
            })
    })
}

/// Why the step does not hold an Astra review of `head` of `pr`: a receipt for a review that
/// happened at that head with effective gpt-6-astra/xhigh, and that review recorded with
/// `--reviewer astra` (whose verdict (b) then holds to ACCEPT or resolved). Every such receipt at
/// the head binds, each unbound one a gap; empty when all are recorded.
fn astra_gap(
    root: &Path,
    target: &Target,
    verdicts: &Verdicts,
    pr: Option<&str>,
    head: &str,
    part: Option<&str>,
    kind: ReviewKind,
) -> Vec<String> {
    let scope = part;
    let part = format!(
        "{}{}",
        part.map(|part| format!(" --part={}", crate::commands::shell_word(part)))
            .unwrap_or_default(),
        if kind == ReviewKind::Resolution {
            " --kind resolution"
        } else {
            ""
        }
    );
    let mut found = Vec::new();
    receipts(&evidence_dir(root, target), &mut found);
    let repo = pr.and_then(GithubRepo::of_pr).map(|repo| repo.to_string());
    let number = pr.and_then(|url| url.rsplit('/').next()?.parse::<u64>().ok());
    let step = full_name(target);
    let mut astra: Option<Vec<(&Recorded, Option<String>)>> = None;
    let mut seen = Vec::new();
    let mut finished = 0;
    let mut unbound = Vec::new();
    for path in &found {
        let rel = relative(root, path);
        let Ok(text) = fs::read_to_string(path) else {
            seen.push(format!("{rel} (unreadable)"));
            continue;
        };
        let Ok(receipt) = serde_json::from_str::<Value>(&text) else {
            seen.push(format!("{rel} (not JSON)"));
            continue;
        };
        let text_at = |pointer: &str| receipt.pointer(pointer).and_then(Value::as_str);
        let model = text_at("/effective/model");
        let effort = text_at("/effective/effort");
        let at = text_at("/head");
        let same_repo = repo
            .as_deref()
            .is_none_or(|repo| text_at("/repo").is_none_or(|found| found == repo));
        let same_pr = number.is_none_or(|number| {
            receipt
                .get("pr")
                .is_none_or(|found| found.as_u64() == Some(number))
        });
        let matches = model == Some(ASTRA_MODEL)
            && effort == Some(ASTRA_EFFORT)
            && at.is_some_and(|at| same_commit(at, head))
            && same_repo
            && same_pr;
        match (matches, unfinished(&receipt)) {
            (true, None) => {
                let Some(want) = text_at("/report_sha256").map(str::to_lowercase) else {
                    finished += 1;
                    unbound.push(format!(
                        "{rel} names no report_sha256, so no recorded report is bound to it"
                    ));
                    continue;
                };
                let astra = astra.get_or_insert_with(|| {
                    verdicts
                        .reviews
                        .iter()
                        // Bound to this PR and part, as every review the gate reads.
                        .filter(|review| {
                            review.step == step
                                && review.binds(head)
                                && review.reviewer.as_deref() == Some(ASTRA_REVIEWER)
                                && review.part.as_deref() == scope
                                && review
                                    .pr
                                    .as_deref()
                                    .is_none_or(|recorded| pr.is_none_or(|pr| recorded == pr))
                        })
                        .map(|review| (review, report_sha256(root, review)))
                        .collect()
                });
                match astra
                    .iter()
                    .find(|(_, sha256)| sha256.as_deref() == Some(want.as_str()))
                {
                    Some((review, _)) if review.kind != kind => {
                        seen.push(format!(
                            "{rel} (the receipt of {}, a {} review)",
                            review.id(),
                            review.kind.name()
                        ));
                        continue;
                    }
                    Some(_) => {
                        finished += 1;
                        continue;
                    }
                    None => finished += 1,
                }
                let astra: Vec<&(&Recorded, Option<String>)> = astra
                    .iter()
                    .filter(|(review, _)| review.kind == kind)
                    .collect();
                let report = relative(root, &path.with_file_name("report.md"));
                unbound.push(if astra.is_empty() {
                    format!(
                        "{rel} records the Astra review at {head}, but it is not recorded; `cm review record {step} {report} --reviewer {ASTRA_REVIEWER}{part}`"
                    )
                } else {
                    format!(
                        "{rel} receipts a report with sha256 {want}, but no Astra review recorded at {head} ({}) is that report; `cm review record {step} {report} --reviewer {ASTRA_REVIEWER}{part}`",
                        astra
                            .iter()
                            .map(|(review, _)| review.id())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                });
            }
            (true, Some(why)) => seen.push(format!("{rel} (the review did not happen: {why})")),
            (false, _) => seen.push(format!(
                "{rel} (effective {}/{}, head {})",
                model.unwrap_or("—"),
                effort.unwrap_or("—"),
                at.unwrap_or("—")
            )),
        }
    }
    if finished > 0 {
        return unbound;
    }
    vec![if seen.is_empty() {
        format!(
            "no Astra receipt.json under {}",
            relative(root, &evidence_dir(root, target))
        )
    } else {
        format!(
            "no Astra receipt at {head} with effective {ASTRA_MODEL}/{ASTRA_EFFORT}; found {}",
            seen.join(", ")
        )
    }]
}

/// Why the recorded reviews do not clear `head`; `None` when at least one review was recorded
/// there and each is ACCEPT or has every blocking finding resolved.
/// The model family a `--reviewer` name belongs to: Codex models (`astra`, `sol`, `terra`,
/// `gpt-*`) or Claude models (`claude`, `opus`, `fresh-opus`, `fable`, `sonnet`, `haiku`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Gpt,
    Claude,
}

impl Family {
    pub fn reviewer(name: Option<&str>) -> Option<Family> {
        let name = name?.to_ascii_lowercase();
        if ["astra", "sol", "terra"].contains(&name.as_str()) || name.starts_with("gpt") {
            Some(Family::Gpt)
        } else if ["claude", "opus", "fresh-opus", "fable", "sonnet", "haiku"]
            .contains(&name.as_str())
        {
            Some(Family::Claude)
        } else {
            None
        }
    }
}

/// A lane a model builds in: its PRs need both families' verdicts, Astra's and a Claude
/// reviewer's, whichever family wrote them.
pub fn model_lane(lane: Option<crate::state::Lane>) -> bool {
    use crate::state::Lane;
    matches!(
        lane,
        Some(Lane::Codex | Lane::ClaudeCloud | Lane::Fable | Lane::Coordinator)
    )
}

/// The reviews recorded for this PR of this step or part at `head`: a review binds to the part
/// it was recorded for and, when it names one, its PR, so two parts never share a verdict.
fn reviews_at<'a>(
    verdicts: &'a Verdicts,
    step: &str,
    head: &str,
    pr: Option<&str>,
    part: Option<&str>,
    kind: ReviewKind,
) -> Vec<&'a Recorded> {
    verdicts
        .reviews
        .iter()
        .filter(|review| {
            review.step == step
                && review.binds(head)
                && review.kind == kind
                && review.part.as_deref() == part
                && review
                    .pr
                    .as_deref()
                    .is_none_or(|recorded| pr.is_none_or(|pr| recorded == pr))
        })
        .collect()
}

/// The verdicts a model lane's head needs (owner, 2026-11-25): an explicit ACCEPT from Astra
/// and one from a Claude reviewer. A FIX REQUIRED whose findings were resolved is no ACCEPT.
fn missing_accepts(at_head: &[&Recorded]) -> Vec<&'static str> {
    let accepted = |family: &dyn Fn(&Recorded) -> bool| {
        at_head.iter().any(|review| {
            review.verdict == Verdict::Accept
                && review.stated == Some(Verdict::Accept)
                && family(review)
        })
    };
    let mut missing = Vec::new();
    if !accepted(&|review| review.reviewer.as_deref() == Some(ASTRA_REVIEWER)) {
        missing.push(ASTRA_REVIEWER);
    }
    if !accepted(&|review| Family::reviewer(review.reviewer.as_deref()) == Some(Family::Claude)) {
        missing.push("claude");
    }
    missing
}

/// The verdicts a head still needs: on a model lane, Astra's and a Claude reviewer's ACCEPT; on
/// any other lane, any one review.
pub fn missing_reviews(
    verdicts: &Verdicts,
    step: &str,
    head: &str,
    pr: Option<&str>,
    part: Option<&str>,
    model: bool,
) -> Vec<&'static str> {
    let at_head = reviews_at(verdicts, step, head, pr, part, ReviewKind::Head);
    if model {
        missing_accepts(&at_head)
    } else if at_head.is_empty() {
        vec!["any"]
    } else {
        Vec::new()
    }
}

fn review_gap(
    verdicts: &Verdicts,
    step: &str,
    head: &str,
    pr: Option<&str>,
    part: Option<&str>,
    model: bool,
) -> Option<String> {
    let at_head = reviews_at(verdicts, step, head, pr, part, ReviewKind::Head);
    let resolutions = reviews_at(verdicts, step, head, pr, part, ReviewKind::Resolution);
    let part = part
        .map(|part| format!(" --part={}", crate::commands::shell_word(part)))
        .unwrap_or_default();
    if at_head.is_empty() && !resolutions.is_empty() {
        return Some(format!(
            "only resolution reviews of {head} are recorded ({}); they count only on the walk from a checkout (`--repo` or the step's worktree) to a first parent that clears; `cm review record {step} <report>{part}`",
            resolutions
                .iter()
                .map(|review| review.id())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if at_head.is_empty() {
        return Some(format!(
            "no review of {head} is recorded; `cm review record {step} <report>{part}`"
        ));
    }
    let open: Vec<String> = at_head
        .iter()
        .flat_map(|review| {
            review
                .unresolved()
                .into_iter()
                .map(move |logged| format!("{}.{}", review.id(), logged.finding.id))
        })
        .collect();
    if !open.is_empty() {
        return Some(format!(
            "the review at {head} is FIX REQUIRED and {} unresolved; `cm review resolve {step} <id> --by <commit|ruling-file>`",
            open.join(", ")
        ));
    }
    // On a model lane both families accept the head explicitly: Astra and a Claude reviewer,
    // each with a computed and a stated ACCEPT.
    let missing: Vec<&str> = if model {
        missing_accepts(&at_head)
    } else {
        Vec::new()
    };
    (!missing.is_empty()).then(|| {
        format!(
            "no {} ACCEPT of {head} is recorded; a model lane's head needs an ACCEPT from Astra and from a Claude reviewer, and a resolved FIX REQUIRED is not one; `cm review record {step} <report> --reviewer <{}>{part}`",
            missing.join(" or "),
            missing.join("|")
        )
    })
}

/// Where the gate may walk a PR's history: a checkout holding its commits, and the base branch's
/// tip (GitHub's, else `origin/HEAD` in the checkout).
pub struct Lineage<'a> {
    pub repo: &'a Path,
    pub base: Option<&'a str>,
}

const MAX_UPDATES: usize = 100;

// Named in full: a local tag or branch called origin/HEAD would stand in for the short name.
const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";

fn shown(base: &str) -> &str {
    base.strip_prefix("refs/remotes/").unwrap_or(base)
}

// Every git command the gate runs must be built here: replace refs, an info/grafts file, a
// commit-graph and a multi-pack-index can each change what git reads for a commit id, and the gate
// reads the history as committed.
mod gate_git {
    use std::path::Path;
    use std::process::{Command, Output};

    // /dev/null is no directory, so no graft file can exist at this path.
    const NO_GRAFTS: &str = "/dev/null/grafts";

    // Variables that move the repository a checkout read reads from; the GIT_TEST_ ones go too,
    // since some bring back the caches the flags below turn off.
    const REDIRECTS: [&str; 8] = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_SHALLOW_FILE",
        "GIT_REPLACE_REF_BASE",
        "GIT_INDEX_FILE",
    ];

    fn git(cleared: bool) -> Command {
        let mut command = Command::new("git");
        if cleared {
            command.env_clear();
            if let Some(path) = std::env::var_os("PATH") {
                command.env("PATH", path);
            }
        } else {
            for (name, _) in std::env::vars_os() {
                let bytes = name.as_encoded_bytes();
                if bytes.starts_with(b"GIT_TEST_")
                    || REDIRECTS
                        .iter()
                        .any(|redirect| bytes == redirect.as_bytes())
                {
                    command.env_remove(&name);
                }
            }
        }
        command
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_GRAFT_FILE", NO_GRAFTS)
            .args([
                "-c",
                "core.commitGraph=false",
                "-c",
                "core.multiPackIndex=false",
            ]);
        command
    }

    pub(super) fn output(repo: &Path, args: &[&str]) -> Option<Output> {
        git(false).arg("-C").arg(repo).args(args).output().ok()
    }

    pub(super) fn isolated(dir: &Path, args: &[&str]) -> Option<Output> {
        let home = dir.join("home");
        git(true)
            .env("GIT_DIR", dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .current_dir(dir)
            .args(args)
            .output()
            .ok()
    }
}

// Which files a merge's parents conflict in, and which files a merge changes, are read where only
// the checkout's objects reach, so that no merge attribute or setting, the checkout's or the
// caller's, decides them.
struct Isolated {
    dir: PathBuf,
}

impl Isolated {
    // Err(None) when the checkout cannot be read.
    fn over(repo: &Path) -> Result<Isolated, Option<CleanUpdateError>> {
        let mut objects = git_bytes(
            repo,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
            ],
        )
        .ok_or(None)?;
        if objects.pop() != Some(b'\n') {
            return Err(None);
        }
        let format = git_out(repo, &["rev-parse", "--show-object-format"]).ok_or(None)?;
        let isolated = Isolated {
            dir: scratch_dir().map_err(Some)?,
        };
        let made = isolated
            .output(&[
                "init",
                "--bare",
                "--quiet",
                "--template=",
                &format!("--object-format={format}"),
            ])
            .ok_or(None)?;
        let alternates = isolated.dir.join("objects").join("info").join("alternates");
        (made.status.success() && fs::write(alternates, quoted(&objects)).is_ok())
            .then_some(isolated)
            .ok_or(None)
    }

    fn output(&self, args: &[&str]) -> Option<std::process::Output> {
        gate_git::isolated(&self.dir, args)
    }

    fn bytes(&self, args: &[&str]) -> Option<Vec<u8>> {
        let output = self.output(args)?;
        output.status.success().then_some(output.stdout)
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// git gets the scratch repository's path as GIT_DIR from inside it, so the path must be absolute.
fn scratch_dir() -> Result<PathBuf, CleanUpdateError> {
    let temp = std::env::temp_dir();
    let temp = if temp.is_absolute() {
        Some(temp)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(temp))
    };
    scratch_dir_in(temp.into_iter().chain([PathBuf::from("/tmp")]).collect())
}

fn scratch_dir_in(mut parents: Vec<PathBuf>) -> Result<PathBuf, CleanUpdateError> {
    parents.dedup();
    parents
        .iter()
        .find_map(|parent| scratch_dir_under(parent))
        .ok_or(CleanUpdateError::NoScratch { tried: parents })
}

fn scratch_dir_under(temp: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..100 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let dir = temp.join(format!(
            "cm-gate-{}-{}-{stamp}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Some(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

// git reads a quoted alternates line C-style, so a path holding a newline or a quote survives.
fn quoted(path: &[u8]) -> Vec<u8> {
    let mut line = vec![b'"'];
    for &byte in path {
        match byte {
            b'"' | b'\\' => line.extend([b'\\', byte]),
            byte if byte < b' ' || byte == 0x7f => line.extend(format!("\\{byte:03o}").bytes()),
            byte => line.push(byte),
        }
    }
    line.extend(b"\"\n");
    line
}

#[derive(Clone, Copy)]
enum Reader<'a> {
    Checkout(&'a Path),
    Isolated(&'a Isolated),
}

impl Reader<'_> {
    fn bytes(self, args: &[&str]) -> Option<Vec<u8>> {
        match self {
            Reader::Checkout(repo) => git_bytes(repo, args),
            Reader::Isolated(isolated) => isolated.bytes(args),
        }
    }
}

// Only for commands that print object names, which are ASCII hex; paths go through git_bytes.
fn git_out(repo: &Path, args: &[&str]) -> Option<String> {
    let output = gate_git::output(repo, args)?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn is_ancestor(repo: &Path, ancestor: &str, of: &str) -> Option<bool> {
    let output = gate_git::output(repo, &["merge-base", "--is-ancestor", ancestor, of])?;
    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Why a commit on the walk back from the head is not a clean update from the base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanUpdateError {
    /// git could not read the commit, or its remerge diff, in the checkout.
    Unreadable {
        commit: String,
        repo: PathBuf,
    },
    NotMerge {
        commit: String,
    },
    /// The merge's second parent is not on the base branch.
    OffBase {
        commit: String,
        parent: String,
        base: String,
    },
    /// The merge's `git show --remerge-diff` is not empty: it resolved conflicts.
    Conflicts {
        commit: String,
    },
    /// The walk passed this many clean updates without reaching a reviewed commit.
    TooLong {
        limit: usize,
    },
    /// The base tip already contains the head (the PR merged into it, or it is the base), so
    /// "on the base branch" says nothing about the head's own commits; the walk does not run.
    BaseContainsHead {
        head: String,
        base: String,
    },
    ResolutionGaps {
        commit: String,
        gaps: Vec<String>,
    },
    NoScratch {
        tried: Vec<PathBuf>,
    },
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

impl fmt::Display for CleanUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CleanUpdateError::Unreadable { commit, repo } => {
                write!(f, "cannot read {} in {}", short(commit), repo.display())
            }
            CleanUpdateError::NotMerge { commit } => {
                write!(f, "{} is not a merge from the base branch", short(commit))
            }
            CleanUpdateError::OffBase {
                commit,
                parent,
                base,
            } => write!(
                f,
                "{} merges {} which is not on the base branch {base}",
                short(commit),
                short(parent)
            ),
            CleanUpdateError::Conflicts { commit } => write!(
                f,
                "{} resolved merge conflicts; it needs its own review",
                short(commit)
            ),
            CleanUpdateError::TooLong { limit } => write!(f, "more than {limit} updates"),
            CleanUpdateError::BaseContainsHead { head, base } => write!(
                f,
                "the base {base} already contains {}, so no merge can be told apart from the PR's own commits",
                short(head)
            ),
            CleanUpdateError::ResolutionGaps { commit, gaps } => write!(
                f,
                "{}'s resolution reviews do not clear it: {}",
                short(commit),
                gaps.join("; ")
            ),
            CleanUpdateError::NoScratch { tried } => write!(
                f,
                "no scratch repository for reading the merges can be made in {}; set TMPDIR to a writable directory",
                tried
                    .iter()
                    .map(|dir| dir.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        }
    }
}

/// `commit`'s first parent when it is a clean update from the base branch: a merge whose second
/// parent is on `base` and whose `git show --remerge-diff` is empty (no conflict resolved).
pub fn clean_update(repo: &Path, commit: &str, base: &str) -> Result<String, CleanUpdateError> {
    let isolated = Isolated::over(repo).map_err(|why| {
        why.unwrap_or_else(|| CleanUpdateError::Unreadable {
            commit: commit.to_owned(),
            repo: repo.to_path_buf(),
        })
    })?;
    let merge = merge_from_base(repo, &isolated, commit, base)?;
    if merge.clean {
        Ok(merge.first)
    } else {
        Err(CleanUpdateError::Conflicts {
            commit: commit.to_owned(),
        })
    }
}

struct Merge {
    first: String,
    second: String,
    clean: bool,
}

fn merge_from_base(
    repo: &Path,
    isolated: &Isolated,
    commit: &str,
    base: &str,
) -> Result<Merge, CleanUpdateError> {
    let unreadable = || CleanUpdateError::Unreadable {
        commit: commit.to_owned(),
        repo: repo.to_path_buf(),
    };
    let line =
        git_out(repo, &["rev-list", "--parents", "-n", "1", commit]).ok_or_else(unreadable)?;
    let parents: Vec<&str> = line.split_whitespace().skip(1).collect();
    let [first, second] = parents[..] else {
        return Err(CleanUpdateError::NotMerge {
            commit: commit.to_owned(),
        });
    };
    if !is_ancestor(repo, second, base).unwrap_or(false) {
        return Err(CleanUpdateError::OffBase {
            commit: commit.to_owned(),
            parent: second.to_owned(),
            base: shown(base).to_owned(),
        });
    }
    let empty = |reader| {
        remerge_diff(reader, commit, Listing::Waiver).map(|diff| diff.trim_ascii().is_empty())
    };
    // Empty in the checkout, as the waiver always asked, and where no local merge setting reaches.
    let clean = match empty(Reader::Checkout(repo)) {
        Some(true) => empty(Reader::Isolated(isolated)).ok_or_else(unreadable)?,
        Some(false) => false,
        None => return Err(unreadable()),
    };
    Ok(Merge {
        first: first.to_owned(),
        second: second.to_owned(),
        clean,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Listing {
    Waiver,
    Paths,
}

// Every listing the gate reads as a complete account of a merge comes from here, with each flag
// that would let configuration or attributes leave a change out: a relative or submodule-blind
// diff, a textconv or external driver, colour, renames folding two paths into one. Only the paths
// leave out a signature check's output: in the waiver's patch it still makes a signed merge no
// clean update under `log.showSignature`, as it always has, because leaving it out there would
// let the waiver accept more.
fn remerge_diff(reader: Reader<'_>, commit: &str, listing: Listing) -> Option<Vec<u8>> {
    let mut args = vec![
        "show",
        "--remerge-diff",
        "--format=",
        "--no-renames",
        "--ignore-submodules=none",
        "--no-ext-diff",
        "--no-textconv",
        "--no-relative",
        "--no-color",
    ];
    if listing == Listing::Paths {
        args.extend(["--no-show-signature", "--name-only", "-z"]);
    }
    args.push(commit);
    reader.bytes(&args)
}

// A hex name must be a prefix of the commit it resolves to, so that no ref named like it stands in;
// any other name must mean one commit wherever git would look for it.
fn named_commit(repo: &Path, named: &str) -> Option<String> {
    let commit = |name: &str| {
        git_out(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ],
        )
    };
    let full = commit(named)?;
    if named.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return full
            .to_ascii_lowercase()
            .starts_with(&named.to_ascii_lowercase())
            .then_some(full);
    }
    ["refs/", "refs/tags/", "refs/heads/", "refs/remotes/"]
        .into_iter()
        .map(|space| format!("{space}{named}"))
        .chain([format!("refs/remotes/{named}/HEAD")])
        .filter_map(|name| commit(&name))
        .all(|other| other == full)
        .then_some(full)
}

fn git_bytes(repo: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = gate_git::output(repo, args)?;
    output.status.success().then_some(output.stdout)
}

fn nul_paths(listed: &[u8]) -> Vec<&[u8]> {
    listed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect()
}

fn beyond_resolution(
    repo: &Path,
    isolated: &Isolated,
    commit: &str,
    merge: &Merge,
) -> Option<String> {
    let unreadable = || {
        CleanUpdateError::Unreadable {
            commit: commit.to_owned(),
            repo: repo.to_path_buf(),
        }
        .to_string()
    };
    let merged = isolated.output(&[
        "merge-tree",
        "--write-tree",
        "--no-messages",
        "-z",
        &merge.first,
        &merge.second,
    ]);
    let listed = match merged {
        Some(output) if output.status.code() == Some(1) => output.stdout,
        Some(output) if output.status.code() == Some(0) => {
            return Some(
                "its parents merge without a conflict, so its remerge diff resolves none"
                    .to_owned(),
            );
        }
        _ => return Some(unreadable()),
    };
    // After the tree id, one `<mode> <object> <stage>\t<path>` entry per stage of each conflict.
    let mut conflicted: Vec<&[u8]> = Vec::new();
    let mut submodules: Vec<String> = Vec::new();
    for entry in nul_paths(&listed).into_iter().skip(1) {
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            return Some(unreadable());
        };
        let path = &entry[tab + 1..];
        let shown = format!("\"{}\"", path.escape_ascii());
        if entry.starts_with(b"160000 ") && !submodules.contains(&shown) {
            submodules.push(shown);
        }
        if !conflicted.contains(&path) {
            conflicted.push(path);
        }
    }
    // The scratch repository holds no submodule's objects, so it cannot tell a submodule's
    // fast-forward from a conflict; no resolution review covers either.
    if !submodules.is_empty() {
        return Some(format!(
            "its parents conflict in the submodule {}",
            submodules.join(", ")
        ));
    }
    let Some(changed) = remerge_diff(Reader::Isolated(isolated), commit, Listing::Paths) else {
        return Some(unreadable());
    };
    let beyond: Vec<String> = nul_paths(&changed)
        .into_iter()
        .filter(|path| !conflicted.contains(path))
        .map(|path| format!("\"{}\"", path.escape_ascii()))
        .collect();
    (!beyond.is_empty()).then(|| {
        format!(
            "it changes {} beyond the files its parents conflict in",
            beyond.join(", ")
        )
    })
}

/// The gate's (a) and (b) for a codex step about to merge at `head`.
pub struct Cleared {
    /// Each unmet condition; empty when the gate's review half holds.
    pub gaps: Vec<String>,
    pub reviewed: Option<String>,
    /// Why the walk back from the head stopped short of a reviewed commit, when it did.
    pub stop: Option<CleanUpdateError>,
    pub resolved: Vec<String>,
}

fn walked_updates(resolved: &[String]) -> String {
    if resolved.is_empty() {
        "clean updates".to_owned()
    } else {
        format!(
            "clean updates and merges whose conflict resolutions both families accepted ({})",
            resolved.join(", ")
        )
    }
}

pub fn merge_gaps(
    root: &Path,
    target: &Target,
    part: Option<&str>,
    pr: Option<&str>,
    head: &str,
    lineage: Option<&Lineage>,
) -> Result<Cleared> {
    gate(root, target, part, pr, head, lineage, false)
}

// A resolution review stands on both families' ACCEPTs on every lane: admitting one, and landing
// past a merge it covers, read the first parent as a model lane's head.
fn gate(
    root: &Path,
    target: &Target,
    part: Option<&str>,
    pr: Option<&str>,
    head: &str,
    lineage: Option<&Lineage>,
    both_families: bool,
) -> Result<Cleared> {
    let verdicts = load(root)?;
    let model = both_families
        || model_lane(
            store::load_unit(&root.join(&target.unit))
                .ok()
                .and_then(|unit| unit.steps.get(target.step.as_deref()?)?.lane),
        );
    let step = full_name(target);
    // (a)'s Astra receipt binds model lanes only; any other lane needs any one cleared review.
    let gaps_at = |at: &str, model: bool| -> Vec<String> {
        let astra = if model {
            astra_gap(root, target, &verdicts, pr, at, part, ReviewKind::Head)
        } else {
            Vec::new()
        };
        astra
            .into_iter()
            .chain(review_gap(&verdicts, &step, at, pr, part, model))
            .collect()
    };
    // A step without a resolution review matches reviews to the head as given, as before
    // resolution reviews existed.
    let resolving = verdicts
        .reviews
        .iter()
        .any(|review| review.step == step && review.kind == ReviewKind::Resolution);
    let resolved = lineage
        .filter(|_| resolving)
        .and_then(|lineage| named_commit(lineage.repo, head));
    let head_at = resolved.as_deref().unwrap_or(head);
    let gaps = gaps_at(head_at, model);
    // The clean-update rule only skips a review nobody did: a review recorded at a commit, or a
    // finished receipt for it, binds there and is never outweighed by an earlier ACCEPT. A
    // resolution review and its receipt bind only as the cover for their merge on the walk.
    let reviewed_at = |at: &str| {
        verdicts.reviews.iter().any(|review| {
            review.step == step && review.at_head(at) && review.kind == ReviewKind::Head
        }) || finished_receipt_at(root, target, &verdicts, at)
    };
    let resolution_at = |at: &str| {
        verdicts.reviews.iter().any(|review| {
            review.step == step && review.binds(at) && review.kind == ReviewKind::Resolution
        })
    };
    let resolution_gaps = |at: &str| -> Vec<String> {
        let reviews = reviews_at(&verdicts, &step, at, pr, part, ReviewKind::Resolution);
        let open: Vec<String> = reviews
            .iter()
            .flat_map(|review| {
                review
                    .unresolved()
                    .into_iter()
                    .map(move |logged| format!("{}.{}", review.id(), logged.finding.id))
            })
            .collect();
        let missing = missing_accepts(&reviews);
        let mut gaps = Vec::new();
        if !open.is_empty() {
            gaps.push(format!("{} unresolved", open.join(", ")));
        }
        if !missing.is_empty() {
            gaps.push(format!("no {} resolution ACCEPT", missing.join(" or ")));
        }
        gaps.extend(astra_gap(
            root,
            target,
            &verdicts,
            pr,
            at,
            part,
            ReviewKind::Resolution,
        ));
        gaps
    };
    let Some(lineage) = lineage.filter(|_| !gaps.is_empty() && !reviewed_at(head_at)) else {
        return Ok(Cleared {
            gaps,
            reviewed: None,
            stop: None,
            resolved: Vec::new(),
        });
    };
    let repo = lineage.repo;
    let base = lineage.base.unwrap_or(ORIGIN_HEAD);
    let start = if resolving {
        resolved
    } else {
        named_commit(repo, head)
    };
    let Some(mut at) = start else {
        return Ok(Cleared {
            gaps,
            reviewed: None,
            stop: None,
            resolved: Vec::new(),
        });
    };
    let mut gaps = gaps;
    let mut resolved = Vec::new();
    let mut why = CleanUpdateError::TooLong { limit: MAX_UPDATES };
    let isolated = if is_ancestor(repo, &at, base).unwrap_or(false) {
        why = CleanUpdateError::BaseContainsHead {
            head: at.clone(),
            base: shown(base).to_owned(),
        };
        None
    } else {
        match Isolated::over(repo) {
            Ok(isolated) => Some(isolated),
            Err(no) => {
                why = no.unwrap_or_else(|| CleanUpdateError::Unreadable {
                    commit: at.clone(),
                    repo: repo.to_path_buf(),
                });
                None
            }
        }
    };
    if let Some(isolated) = isolated {
        for _ in 0..MAX_UPDATES {
            match merge_from_base(repo, &isolated, &at, base) {
                Ok(merge) if merge.clean => at = merge.first,
                Ok(merge) if resolution_at(&at) => {
                    let open = match beyond_resolution(repo, &isolated, &at, &merge) {
                        Some(gap) => vec![gap],
                        None => resolution_gaps(&at),
                    };
                    if !open.is_empty() {
                        why = CleanUpdateError::ResolutionGaps {
                            commit: at,
                            gaps: open,
                        };
                        break;
                    }
                    resolved.push(std::mem::replace(&mut at, merge.first));
                }
                Ok(_) => {
                    why = CleanUpdateError::Conflicts { commit: at };
                    break;
                }
                Err(reason) => {
                    why = reason;
                    break;
                }
            }
            if reviewed_at(&at) {
                let earlier = gaps_at(&at, model || !resolved.is_empty());
                if earlier.is_empty() {
                    return Ok(Cleared {
                        gaps: Vec::new(),
                        reviewed: Some(at),
                        stop: None,
                        resolved,
                    });
                }
                gaps.push(format!(
                    "the nearest reviewed commit before {head}, {at}, joined to it by {}, does not clear: {}",
                    walked_updates(&resolved),
                    earlier.join("; ")
                ));
                return Ok(Cleared {
                    gaps,
                    reviewed: None,
                    stop: None,
                    resolved,
                });
            }
        }
    }
    let earlier = verdicts
        .reviews
        .iter()
        .any(|review| review.step == step && !review.at_head(head));
    if earlier {
        gaps.push(format!(
            "no reviewed head before {head} is joined to it by {} from the base branch: {why}",
            walked_updates(&resolved)
        ));
    }
    Ok(Cleared {
        gaps,
        reviewed: None,
        stop: Some(why),
        resolved,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    thread_local! {
        pub(super) static HASHES: Cell<usize> = const { Cell::new(0) };
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn report(block: &str, tail: &str) -> String {
        format!("# Review\n\nFindings follow.\n\n```{BLOCK}\n{block}```\n{tail}")
    }

    #[test]
    fn a_scratch_repository_is_private_and_without_a_writable_parent_names_tmpdir() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("missing");
        let dir = scratch_dir_in(vec![missing.clone(), parent.path().to_path_buf()]).unwrap();
        assert!(dir.starts_with(parent.path()), "{dir:?}");
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let refused = scratch_dir_in(vec![missing.clone(), missing.join("deeper")]).unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "no scratch repository for reading the merges can be made in {} or {}; set TMPDIR to a writable directory",
                missing.display(),
                missing.join("deeper").display()
            )
        );
    }

    #[test]
    fn only_an_introduced_or_exposed_contract_finding_blocks() {
        let text = report(
            &format!(
                "head: {SHA}\nF1 | contract | inherited | src/a.rs:3 | old bug\nF2 | guideline | introduced | src/b.rs:4-9 | naming | with a pipe\nF3 | taste | exposed | - | style\n"
            ),
            "\nACCEPT\n",
        );
        let block = parse_report(&text).unwrap();
        assert_eq!(block.verdict(), Verdict::Accept);
        assert_eq!(block.stated, Some(Verdict::Accept));
        assert_eq!(block.findings[1].summary, "naming | with a pipe");
        let exposed = report(
            &format!("head: {SHA}\nF1 | contract | exposed | src/a.rs:3 | now reachable\n"),
            "\n**FIX REQUIRED**\n",
        );
        let block = parse_report(&exposed).unwrap();
        assert_eq!(block.verdict(), Verdict::FixRequired);
        assert_eq!(block.stated, Some(Verdict::FixRequired));
        assert_eq!(
            parse_report(&report(&format!("head: {SHA}\n"), ""))
                .unwrap()
                .verdict(),
            Verdict::Accept
        );
    }

    #[test]
    fn a_malformed_block_lists_every_problem() {
        let errors = |text: &str| parse_report(text).unwrap_err();
        assert_eq!(
            errors("# Review\n\nACCEPT\n"),
            [BlockError::Missing { line: 3 }]
        );
        assert_eq!(
            errors(&report(
                "F1 | contract | new | src/a.rs | x\nF1 | flaw | introduced | src/a.rs:1 |\nhead: nope\nhead: abcdef1\n",
                "",
            )),
            [
                BlockError::Origin {
                    line: 6,
                    origin: "new".into()
                },
                BlockError::Location {
                    line: 6,
                    at: "src/a.rs".into()
                },
                BlockError::Type {
                    line: 7,
                    kind: "flaw".into()
                },
                BlockError::EmptySummary { line: 7 },
                BlockError::Sha {
                    line: 8,
                    sha: "nope".into()
                },
                BlockError::SecondHead { line: 9 },
            ]
        );
        assert_eq!(
            errors(&report(
                &format!(
                    "head: {SHA}\nF1 | taste | introduced | - | x\nF1 | taste | introduced | - | y\nF 2 | taste | introduced | - | z\n"
                ),
                "",
            )),
            [
                BlockError::DuplicateId {
                    line: 8,
                    id: "F1".into()
                },
                BlockError::Id {
                    line: 9,
                    id: "F 2".into()
                },
            ]
        );
        assert_eq!(
            errors(&report("F1 | taste | introduced | - | x\n", "")),
            [BlockError::NoHead { line: 5 }]
        );
        assert_eq!(
            errors(&report(
                &format!("head: {SHA}\n"),
                "\nMore prose.\nACCEPT\n"
            )),
            [BlockError::TextAfter { line: 9 }]
        );
        assert_eq!(
            errors(&format!(
                "{}\n```sh\nls\n```\n",
                report(&format!("head: {SHA}\n"), "")
            )),
            [
                BlockError::FenceAfter { line: 9 },
                BlockError::TextAfter { line: 9 }
            ]
        );
        assert_eq!(
            errors(&format!("```{BLOCK}\nhead: {SHA}\n")),
            [BlockError::Unclosed { line: 1 }]
        );
        let two = format!(
            "{}{}",
            report(&format!("head: {SHA}\n"), ""),
            report(&format!("head: {SHA}\n"), "")
        );
        assert_eq!(errors(&two), [BlockError::Several { line: 12, count: 2 }]);
        assert_eq!(
            errors(&report(
                &format!("head: {SHA}\nF1 contract introduced\n"),
                ""
            )),
            [BlockError::Shape {
                line: 7,
                text: "F1 contract introduced".into()
            }]
        );
    }

    #[test]
    fn other_fenced_blocks_may_come_before_it() {
        let text = format!(
            "Run:\n\n```console\n$ cargo test\n```\n\n````{BLOCK}\nhead: {}\n````\n",
            &SHA[..8]
        );
        let block = parse_report(&text).unwrap();
        assert_eq!(block.head, &SHA[..8]);
        assert_eq!(block.stated, None);
    }

    #[test]
    fn astra_gap_hashes_each_eligible_report_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let target = Target {
            unit: "01-a/u".to_owned(),
            step: Some("S1".to_owned()),
        };
        let evidence = evidence_dir(root, &target);
        let mut reviews = Vec::new();
        for (number, name) in [(1, "a"), (2, "b")] {
            let copy = evidence.join("reviews").join(format!("R{number}-astra.md"));
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            fs::write(&copy, format!("report {name}\n")).unwrap();
            let sha256: String = Sha256::digest(format!("report {name}\n"))
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let receipt = evidence.join(name).join(RECEIPT);
            fs::create_dir_all(receipt.parent().unwrap()).unwrap();
            fs::write(
                &receipt,
                json!({
                    "head": SHA,
                    "effective": {"model": ASTRA_MODEL, "effort": ASTRA_EFFORT},
                    "turn": {"status": "completed"},
                    "thread_fresh": {"forkedFromId": null, "parentThreadId": null, "turns_in_thread": 1},
                    "report_sha256": sha256,
                })
                .to_string(),
            )
            .unwrap();
            reviews.push(Recorded {
                number,
                at: "2026-10-25T00:00:00Z".to_owned(),
                step: full_name(&target),
                reviewer: Some(ASTRA_REVIEWER.to_owned()),
                pr: None,
                part: None,
                head: SHA.to_owned(),
                kind: ReviewKind::Head,
                verdict: Verdict::Accept,
                stated: None,
                report: CampaignFile::resolve(root, &copy).unwrap(),
                findings: Vec::new(),
            });
        }
        let verdicts = Verdicts { reviews };
        HASHES.with(|hashes| hashes.set(0));
        assert!(astra_gap(root, &target, &verdicts, None, SHA, None, ReviewKind::Head).is_empty());
        assert_eq!(HASHES.with(Cell::get), 2);
    }
}
