use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::host::{Host, stamp};
use crate::outcome::{Outcome, Violation};
use crate::{journal, store};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub url: String,
    pub head_ref_oid: String,
    pub base_ref_oid: String,
    pub state: String,
    pub is_draft: bool,
    pub merge_state_status: String,
    pub updated_at: String,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub name: String,
    pub state: String,
    pub link: String,
    pub started_at: String,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub pr: Option<PullRequest>,
    pub required_checks: Vec<Check>,
    pub error: Option<String>,
}

pub trait Github {
    fn observe(&self, url: &str) -> Evidence;
}

pub struct Gh;

fn gh_json<T: serde::de::DeserializeOwned>(args: &[&str]) -> Result<T, String> {
    let output = Command::new("gh")
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .map_err(|_| "could not execute gh; check installation".to_owned())?;
    if !output.status.success() {
        return Err(format!(
            "gh exited {}; check access and JSON support",
            output
                .status
                .code()
                .map_or("by signal".into(), |c| c.to_string())
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "gh returned invalid JSON evidence".into())
}

fn read_pr(url: &str) -> Result<PullRequest, String> {
    gh_json(&[
        "pr",
        "view",
        url,
        "--json",
        "url,headRefOid,baseRefOid,state,isDraft,mergeStateStatus,updatedAt,mergedAt,closedAt",
    ])
}

impl Github for Gh {
    fn observe(&self, url: &str) -> Evidence {
        let mut evidence = Evidence {
            pr: None,
            required_checks: Vec::new(),
            error: None,
        };
        let first = match read_pr(url) {
            Ok(pr) => pr,
            Err(error) => {
                evidence.error = Some(format!("PR read: {error}"));
                return evidence;
            }
        };
        if first.url != url || first.head_ref_oid.len() != 40 || first.base_ref_oid.len() != 40 {
            evidence.error = Some("PR identity or commit evidence is invalid".into());
            return evidence;
        }
        evidence.pr = Some(first.clone());
        if first.state == "MERGED" || first.state == "CLOSED" {
            return evidence;
        }
        match gh_json::<Vec<Check>>(&[
            "pr",
            "checks",
            url,
            "--required",
            "--json",
            "name,state,link,startedAt,completedAt",
        ]) {
            Ok(mut checks) => {
                checks.sort();
                evidence.required_checks = checks;
            }
            Err(error) => evidence.error = Some(format!("required checks: {error}")),
        }
        match read_pr(url) {
            Ok(last) if last == first => {}
            Ok(last) => {
                evidence.pr = Some(last);
                evidence.required_checks.clear();
                evidence.error = Some("PR changed during observation; run attention again".into());
            }
            Err(error) => evidence.error = Some(format!("PR recheck: {error}")),
        }
        evidence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    InvestigateEvidence,
    ReconcileMerged,
    ReconcileClosed,
    FixCi,
    ReviewChangedHead,
    FinishDraft,
    WaitForCi,
    VerifyChecks,
    ReviewHead,
}

impl Action {
    fn text(&self) -> &'static str {
        match self {
            Self::InvestigateEvidence => "resolve unknown evidence; no readiness claim",
            Self::ReconcileMerged => {
                "reconcile merged PR and obsolete work; verify release separately"
            }
            Self::ReconcileClosed => "reconcile closed PR and obsolete work; do not keep fixing it",
            Self::FixCi => "inspect failing required checks and route an authorized fix",
            Self::ReviewChangedHead => "head changed; discard prior readiness and review this head",
            Self::FinishDraft => "finish draft before requesting review",
            Self::WaitForCi => "wait for required checks; no readiness claim",
            Self::VerifyChecks => "verify missing, skipped, unknown or blocked check evidence",
            Self::ReviewHead => {
                "reported required checks passed; review exact head and authority, not approval to merge"
            }
        }
    }

    fn worker(&self) -> bool {
        matches!(self, Self::FixCi | Self::FinishDraft | Self::WaitForCi)
    }
}

fn action(evidence: &Evidence, previous_head: Option<&str>) -> Action {
    if evidence.error.is_some() {
        return Action::InvestigateEvidence;
    }
    let Some(pr) = &evidence.pr else {
        return Action::InvestigateEvidence;
    };
    match pr.state.as_str() {
        "MERGED" => return Action::ReconcileMerged,
        "CLOSED" => return Action::ReconcileClosed,
        "OPEN" => {}
        _ => return Action::InvestigateEvidence,
    }
    if evidence.required_checks.iter().any(|c| {
        matches!(
            c.state.as_str(),
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
        )
    }) {
        return Action::FixCi;
    }
    if previous_head.is_some_and(|head| head != pr.head_ref_oid) {
        return Action::ReviewChangedHead;
    }
    if pr.is_draft {
        return Action::FinishDraft;
    }
    if evidence.required_checks.is_empty() {
        return Action::VerifyChecks;
    }
    if evidence.required_checks.iter().any(|c| {
        !matches!(
            c.state.as_str(),
            "SUCCESS" | "PENDING" | "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED"
        )
    }) {
        return Action::VerifyChecks;
    }
    if evidence
        .required_checks
        .iter()
        .any(|c| c.state != "SUCCESS")
    {
        return Action::WaitForCi;
    }
    if pr.merge_state_status != "CLEAN" {
        return Action::VerifyChecks;
    }
    Action::ReviewHead
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub observed_at: String,
    pub worker: String,
    pub coordinator: String,
    pub last_head: Option<String>,
    pub evidence: Evidence,
    pub action: Action,
}

pub fn run(
    host: &dyn Host,
    github: &dyn Github,
    root: &Path,
    urls: &[String],
    worker: &str,
    coordinator: &str,
    record: bool,
) -> Result<Outcome> {
    if urls.is_empty() || urls.len() > 20 {
        bail!("attention takes 1–20 explicit --pr URLs");
    }
    for label in [worker, coordinator] {
        if label.trim().is_empty() || label.len() > 200 || label.chars().any(char::is_control) {
            bail!("owners must be nonempty single-line labels of at most 200 bytes");
        }
    }
    for url in urls {
        let parts: Vec<_> = url
            .strip_prefix("https://github.com/")
            .unwrap_or("")
            .split('/')
            .collect();
        if parts.len() != 4
            || parts[2] != "pull"
            || parts[3].parse::<u64>().ok().is_none_or(|n| n == 0)
            || parts[..2].iter().any(|p| {
                p.is_empty()
                    || !p
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            })
        {
            bail!("invalid GitHub PR URL: {url}");
        }
    }
    let root = store::find_root(root).ok_or_else(|| anyhow::anyhow!("not inside a cm campaign"))?;
    let _lock = store::lock(&root)?;
    let state = store::load_campaign(&root)?;
    let violations = journal::check(&root, &state);
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let path = root.join(".cm/attention.json");
    let mut saved: BTreeMap<String, Observation> = if path.exists() {
        store::read_json(&path)?
    } else {
        BTreeMap::new()
    };
    let mut output = Outcome::default();
    let mut rows = BTreeMap::new();
    let mut changed = 0;
    for url in urls {
        if rows.contains_key(url) {
            continue;
        }
        let evidence = github.observe(url);
        let old = saved.get(url);
        let different = old.is_none_or(|o| {
            o.evidence != evidence || o.worker != worker || o.coordinator != coordinator
        });
        let last_head = evidence
            .pr
            .as_ref()
            .map(|p| p.head_ref_oid.clone())
            .or_else(|| old.and_then(|o| o.last_head.clone()));
        let next = if different {
            action(&evidence, old.and_then(|o| o.last_head.as_deref()))
        } else {
            old.unwrap().action.clone()
        };
        let row = Observation {
            observed_at: stamp(host.now()),
            worker: worker.into(),
            coordinator: coordinator.into(),
            last_head,
            evidence,
            action: next,
        };
        let owner = if row.action.worker() {
            worker
        } else {
            coordinator
        };
        let head = row
            .evidence
            .pr
            .as_ref()
            .map_or("unknown", |p| p.head_ref_oid.as_str());
        output.line(format!("{url} @ {head} → {owner}: {}", row.action.text()));
        if matches!(
            row.action,
            Action::InvestigateEvidence | Action::VerifyChecks
        ) {
            output.violations.push(Violation::new(
                url,
                "attention-evidence",
                row.evidence.error.as_deref().unwrap_or(row.action.text()),
            ));
        }
        if different {
            changed += 1;
            if record {
                let body = serde_json::to_string(&row)?;
                journal::append(
                    &root,
                    &journal::entry(&row.observed_at, &format!("attention {url}"), Some(&body)),
                )?;
                saved.insert(url.clone(), row.clone());
                store::write_json(&path, &saved)?;
            }
        }
        rows.insert(url.clone(), row);
    }
    output.field("observations", serde_json::to_value(rows)?);
    output.field("changed", changed);
    output.field("recorded", record);
    Ok(output)
}
