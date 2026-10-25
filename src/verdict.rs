use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

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
use crate::state::{Evidence, Phase};
use crate::store::{
    self, CM_DIR, Target, create_new, read_json, relative, write_atomic, write_json,
};

pub const VERDICTS: &str = "verdicts.json";
pub const BLOCK: &str = "cm-review";
pub const SCOPE: &str = "coordinator-scope.md";
pub const SCOPE_HEADING: &str = "## Coordinator scope and rulings";
pub const THIRD_FIX: &str = "third-fix";
pub const ASTRA_MODEL: &str = "gpt-6-astra";
pub const ASTRA_EFFORT: &str = "xhigh";
pub const ASTRA_REVIEWER: &str = "astra";
const RECEIPT: &str = "receipt.json";

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
        self.kind == FindingType::Contract
            && match self.origin {
                Origin::Introduced | Origin::Exposed => true,
                Origin::Inherited => false,
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
    /// The report has no `cm-review` block.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub number: u32,
    pub at: String,
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    pub head: String,
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

    /// The blocking findings no resolution has cleared.
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

/// The heads of the step's computed FIX REQUIRED reviews, each one round.
fn fix_rounds<'a>(verdicts: &'a Verdicts, step: &str) -> Vec<&'a str> {
    let mut heads: Vec<&str> = Vec::new();
    for review in verdicts
        .reviews
        .iter()
        .filter(|review| review.step == step && review.verdict == Verdict::FixRequired)
    {
        if !heads.iter().any(|head| same_commit(head, &review.head)) {
            heads.push(&review.head);
        }
    }
    heads
}

fn step_pr(root: &Path, target: &Target) -> Result<Option<String>> {
    let dir = root.join(&target.unit);
    if !store::is_unit(&dir) {
        return Ok(None);
    }
    Ok(store::load_unit(&dir)?
        .steps
        .get(target.step.as_deref().unwrap_or_default())
        .and_then(|step| step.track.evidence.pr.clone()))
}

pub fn record(
    host: &dyn Host,
    root: &Path,
    target: &Target,
    report: &Path,
    reviewer: Option<&str>,
) -> Result<Outcome> {
    let (_lock, mut violations) = guard(root)?;
    let full = full_name(target);
    if !declared(root, target)? {
        violations.push(Violation::new(
            &full,
            "step-unknown",
            "the step is not declared",
        ));
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
        && let Some(again) = verdicts.reviews.iter().find(|review| {
            review.step == full
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
    let verdict = block.verdict();
    let at = stamp(host.now());
    let heads = fix_rounds(&verdicts, &full);
    let before = heads.len();
    let new_round =
        verdict == Verdict::FixRequired && !heads.iter().any(|head| same_commit(head, &block.head));
    let rounds = before + usize::from(new_round);
    let third = new_round && rounds == 3;
    let blockable = third && {
        let unit = store::load_unit(&root.join(&target.unit))?;
        unit.steps
            .get(target.step.as_deref().unwrap_or_default())
            .is_some_and(|step| {
                let blocked = match step.track.state {
                    Phase::Blocked(_) => true,
                    Phase::Drafted
                    | Phase::Launched
                    | Phase::PrOpen
                    | Phase::Merged
                    | Phase::Released
                    | Phase::Deployed
                    | Phase::Done
                    | Phase::Superseded => false,
                };
                !blocked && !step.track.state.is_final()
            })
    };
    let mut queued = None;
    if third {
        let earlier = verdicts
            .reviews
            .iter()
            .rev()
            .find(|review| review.step == full && review.verdict == Verdict::FixRequired)
            .map(|review| review.report.to_string());
        let reports = earlier
            .iter()
            .map(|path| format!("`{path}`"))
            .chain([format!("`{copy_rel}`")])
            .collect::<Vec<_>>()
            .join(" and ");
        let title = format!("{full}: third FIX REQUIRED");
        let context = format!(
            "{full} has a computed FIX REQUIRED verdict on {rounds} reviewed heads{}; the latest reports are {reports}",
            if blockable {
                format!("; cm blocked it `blocked:{THIRD_FIX}`")
            } else {
                String::new()
            }
        );
        let question = format!("Should {full} take another fix round, be re-scoped, or stop?");
        let recommendation = "Read the two reports; rule in the step's evidence/coordinator-scope.md, then `cm unblock` it or re-scope it";
        let item = QueueItem {
            title: &title,
            context: &context,
            question: &question,
            recommendation,
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
    let pr = step_pr(root, target)?;
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
    let recorded = Recorded {
        number,
        at: at.clone(),
        step: full.clone(),
        reviewer: reviewer.map(str::to_owned),
        pr: pr.clone(),
        head: block.head.clone(),
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
    verdicts.reviews.push(recorded);
    save(root, &verdicts)?;
    let mut body = format!(
        "- report: `{copy_rel}` @{}{}\n",
        block.head,
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
    let journal = journal_line(
        root,
        &at,
        &format!("review record {full} {id}: {verdict}"),
        &body,
    )?;
    let mut blocked = None;
    if blockable {
        let evidence = Evidence {
            file: Some(report_file),
            ..Evidence::default()
        };
        blocked = crate::commands::block_step(root, target, THIRD_FIX, &at, &evidence)?;
    }
    let mut outcome = Outcome::default();
    outcome.line(format!(
        "{id} {full} @{}: {verdict} ({} blocking, {} follow-up(s)); report {copy_rel}",
        block.head,
        blocking.len(),
        followups.len()
    ));
    if let Some(stated) = mismatch {
        outcome.line(format!(
            "the report says {stated}; cm computes {verdict}: only an introduced or exposed contract finding blocks"
        ));
    }
    if third {
        outcome.line(format!(
            "third FIX REQUIRED round: {full}{}; owner-queue item {}",
            if blocked.is_some() {
                format!(" → blocked:{THIRD_FIX}")
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
    outcome.field("head", block.head);
    outcome.field("report", copy_rel);
    outcome.field("blocking", blocking);
    outcome.field("followups", followups);
    outcome.field("rounds", rounds);
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

fn same_commit(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    a.len().min(b.len()) >= 7 && (a.starts_with(&b) || b.starts_with(&a))
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

/// The step's evidence holds a receipt for a review of `head` that happened, whatever its model.
fn finished_receipt_at(root: &Path, target: &Target, head: &str) -> bool {
    let mut found = Vec::new();
    receipts(&evidence_dir(root, target), &mut found);
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
) -> Vec<String> {
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
                finished += 1;
                let Some(want) = text_at("/report_sha256").map(str::to_lowercase) else {
                    unbound.push(format!(
                        "{rel} names no report_sha256, so no recorded report is bound to it"
                    ));
                    continue;
                };
                let astra = astra.get_or_insert_with(|| {
                    verdicts
                        .reviews
                        .iter()
                        .filter(|review| {
                            review.step == step
                                && review.at_head(head)
                                && review.reviewer.as_deref() == Some(ASTRA_REVIEWER)
                        })
                        .map(|review| (review, report_sha256(root, review)))
                        .collect()
                });
                if astra
                    .iter()
                    .any(|(_, sha256)| sha256.as_deref() == Some(want.as_str()))
                {
                    continue;
                }
                let report = relative(root, &path.with_file_name("report.md"));
                unbound.push(if astra.is_empty() {
                    format!(
                        "{rel} records the Astra review at {head}, but it is not recorded; `cm review record {step} {report} --reviewer {ASTRA_REVIEWER}`"
                    )
                } else {
                    format!(
                        "{rel} receipts a report with sha256 {want}, but no Astra review recorded at {head} ({}) is that report; `cm review record {step} {report} --reviewer {ASTRA_REVIEWER}`",
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
fn review_gap(verdicts: &Verdicts, step: &str, head: &str) -> Option<String> {
    let at_head: Vec<&Recorded> = verdicts
        .reviews
        .iter()
        .filter(|review| review.step == step && review.at_head(head))
        .collect();
    if at_head.is_empty() {
        return Some(format!(
            "no review of {head} is recorded; `cm review record {step} <report>`"
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
    (!open.is_empty()).then(|| {
        format!(
            "the review at {head} is FIX REQUIRED and {} unresolved; `cm review resolve {step} <id> --by <commit|ruling-file>`",
            open.join(", ")
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

fn git_out(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn is_ancestor(repo: &Path, ancestor: &str, of: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, of])
        .status()
        .is_ok_and(|status| status.success())
}

/// Why a commit on the walk back from the head is not a clean update from the base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanUpdateError {
    /// git could not read the commit, or its remerge diff, in the checkout.
    Unreadable { commit: String, repo: PathBuf },
    /// The commit is not a merge.
    NotMerge { commit: String },
    /// The merge's second parent is not on the base branch.
    OffBase {
        commit: String,
        parent: String,
        base: String,
    },
    /// The merge's `git show --remerge-diff` is not empty: it resolved conflicts.
    Conflicts { commit: String },
    /// The walk passed this many clean updates without reaching a reviewed commit.
    TooLong { limit: usize },
    /// The base tip already contains the head (the PR merged into it, or it is the base), so
    /// "on the base branch" says nothing about the head's own commits; the walk does not run.
    BaseContainsHead { head: String, base: String },
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
        }
    }
}

/// `commit`'s first parent when it is a clean update from the base branch: a merge whose second
/// parent is on `base` and whose `git show --remerge-diff` is empty (no conflict resolved).
pub fn clean_update(repo: &Path, commit: &str, base: &str) -> Result<String, CleanUpdateError> {
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
    if !is_ancestor(repo, second, base) {
        return Err(CleanUpdateError::OffBase {
            commit: commit.to_owned(),
            parent: second.to_owned(),
            base: base.to_owned(),
        });
    }
    match git_out(repo, &["show", "--remerge-diff", "--format=", commit]) {
        Some(diff) if diff.is_empty() => Ok(first.to_owned()),
        Some(_) => Err(CleanUpdateError::Conflicts {
            commit: commit.to_owned(),
        }),
        None => Err(unreadable()),
    }
}

/// The gate's (a) and (b) for a codex step about to merge at `head`.
pub struct Cleared {
    /// Each unmet condition; empty when the gate's review half holds.
    pub gaps: Vec<String>,
    /// The reviewed head the gate took, when it is an earlier commit than `head` and every
    /// commit since is a clean update from the base branch.
    pub reviewed: Option<String>,
    /// Why the walk back from the head stopped short of a reviewed commit, when it did.
    pub stop: Option<CleanUpdateError>,
}

/// The gate's (a) and (b) at `head`, or, with `lineage`, at the nearest earlier head on its
/// first-parent chain where both hold and every first-parent commit after it is a clean update
/// from the base branch: a clean update needs no review beyond green CI on the new head.
pub fn merge_gaps(
    root: &Path,
    target: &Target,
    pr: Option<&str>,
    head: &str,
    lineage: Option<&Lineage>,
) -> Result<Cleared> {
    let verdicts = load(root)?;
    let gaps_at = |at: &str| -> Vec<String> {
        astra_gap(root, target, &verdicts, pr, at)
            .into_iter()
            .chain(review_gap(&verdicts, &full_name(target), at))
            .collect()
    };
    let gaps = gaps_at(head);
    // The clean-update rule only skips a review nobody did: a review recorded at a commit, or a
    // finished receipt for it, binds there and is never outweighed by an earlier ACCEPT.
    let reviewed_at = |at: &str| {
        let step = full_name(target);
        verdicts
            .reviews
            .iter()
            .any(|review| review.step == step && review.at_head(at))
            || finished_receipt_at(root, target, at)
    };
    let Some(lineage) = lineage.filter(|_| !gaps.is_empty() && !reviewed_at(head)) else {
        return Ok(Cleared {
            gaps,
            reviewed: None,
            stop: None,
        });
    };
    let repo = lineage.repo;
    let base = lineage.base.unwrap_or("origin/HEAD");
    let Some(mut at) = git_out(
        repo,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    ) else {
        return Ok(Cleared {
            gaps,
            reviewed: None,
            stop: None,
        });
    };
    let mut gaps = gaps;
    let mut why = CleanUpdateError::TooLong { limit: MAX_UPDATES };
    if is_ancestor(repo, &at, base) {
        why = CleanUpdateError::BaseContainsHead {
            head: at.clone(),
            base: base.to_owned(),
        };
    } else {
        for _ in 0..MAX_UPDATES {
            match clean_update(repo, &at, base) {
                Ok(parent) => at = parent,
                Err(reason) => {
                    why = reason;
                    break;
                }
            }
            if reviewed_at(&at) {
                let earlier = gaps_at(&at);
                if earlier.is_empty() {
                    return Ok(Cleared {
                        gaps: Vec::new(),
                        reviewed: Some(at),
                        stop: None,
                    });
                }
                gaps.push(format!(
                    "the nearest reviewed commit before {head}, {at}, joined to it by clean updates, does not clear: {}",
                    earlier.join("; ")
                ));
                return Ok(Cleared {
                    gaps,
                    reviewed: None,
                    stop: None,
                });
            }
        }
    }
    let step = full_name(target);
    let earlier = verdicts
        .reviews
        .iter()
        .any(|review| review.step == step && !review.at_head(head));
    if earlier {
        gaps.push(format!(
            "no reviewed head before {head} is joined to it by clean updates from the base branch: {why}"
        ));
    }
    Ok(Cleared {
        gaps,
        reviewed: None,
        stop: Some(why),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn report(block: &str, tail: &str) -> String {
        format!("# Review\n\nFindings follow.\n\n```{BLOCK}\n{block}```\n{tail}")
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
}
