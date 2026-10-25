use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::commands::{closed, write_unit};
use crate::forge::{
    CheckState, CommitSha, Forge, ForgeError, GithubRepo, Pull, PullState, TagName, ci_green,
};
use crate::host::{Host, stamp};
use crate::journal;
use crate::outcome::{Outcome, Violation};
use crate::plan::{self, Units};
use crate::render;
use crate::state::{Evidence, Lane, Move, Phase, Release, Step};
use crate::store::{self, lock, parse_target, write_atomic};

pub const CARD: &str = "CARD.md";

pub struct Moved {
    pub target: String,
    pub from: Phase,
    pub to: Phase,
    pub evidence: Evidence,
}

type Read<T> = Result<T, ForgeError>;

#[derive(Default)]
struct Reads {
    pulls: BTreeMap<String, Read<Pull>>,
    tags: BTreeMap<(GithubRepo, TagName), Read<bool>>,
    contains: BTreeMap<(GithubRepo, TagName, CommitSha), Read<bool>>,
    checks: BTreeMap<(GithubRepo, CommitSha), Read<Vec<CheckState>>>,
}

fn cached<K: Ord, T: Clone>(
    cache: &mut BTreeMap<K, Read<T>>,
    key: K,
    forge: Option<&dyn Forge>,
    read: impl FnOnce(&dyn Forge) -> Read<T>,
) -> Option<Read<T>> {
    if let Some(hit) = cache.get(&key) {
        return Some(hit.clone());
    }
    let result = read(forge?);
    cache.insert(key, result.clone());
    Some(result)
}

struct Shipped {
    release: Release,
    repo: GithubRepo,
    tag: TagName,
    digest: Option<String>,
}

struct Run<'a> {
    root: &'a Path,
    forge: Option<&'a dyn Forge>,
    reads: Reads,
    at: String,
    units: Units,
    dirty: BTreeSet<String>,
    moves: Vec<Moved>,
    violations: Vec<Violation>,
    open: BTreeMap<String, plan::Observed>,
}

fn merge_of(evidence: &Evidence) -> Option<(GithubRepo, CommitSha)> {
    let repo = evidence.pr.as_deref().and_then(GithubRepo::of_pr)?;
    let sha = CommitSha::new(evidence.sha.clone()?);
    Some((repo, sha))
}

impl<'a> Run<'a> {
    fn new(
        root: &'a Path,
        forge: Option<&'a dyn Forge>,
        reads: Reads,
        at: String,
        units: Units,
    ) -> Self {
        Run {
            root,
            forge,
            reads,
            at,
            units,
            dirty: BTreeSet::new(),
            moves: Vec::new(),
            violations: Vec::new(),
            open: BTreeMap::new(),
        }
    }

    fn walk(&mut self) {
        self.merges();
        self.tags();
        self.released();
        self.deployed();
    }

    fn steps_at(&self, phase: &Phase) -> Vec<(String, Step)> {
        self.units
            .iter()
            .flat_map(|(unit, state)| {
                state
                    .steps
                    .iter()
                    .filter(|(_, step)| step.track.state == *phase)
                    .map(move |(name, step)| (format!("{unit}/{name}"), step.clone()))
            })
            .collect()
    }

    fn unknown(&mut self, target: &str, message: String) {
        self.violations
            .push(Violation::new(target, "sync-unknown", message));
    }

    fn advance(&mut self, target: &str, to: Phase, evidence: Evidence) {
        let Some((unit_name, step_name)) = plan::split(target) else {
            return;
        };
        let Some(unit) = self.units.get_mut(unit_name) else {
            return;
        };
        let Some(step) = unit.steps.get_mut(step_name) else {
            return;
        };
        if step.track.check(&to, step.finish()).is_err() {
            return;
        }
        let from = step.track.state.clone();
        step.track.apply(to.clone(), &self.at, &evidence);
        unit.history.push(Move {
            at: self.at.clone(),
            target: target.to_owned(),
            from: from.clone(),
            to: to.clone(),
            evidence: evidence.clone(),
        });
        self.dirty.insert(unit_name.to_owned());
        self.moves.push(Moved {
            target: target.to_owned(),
            from,
            to,
            evidence,
        });
    }

    fn pull(&mut self, url: &str) -> Option<Read<Pull>> {
        cached(&mut self.reads.pulls, url.to_owned(), self.forge, |forge| {
            forge.pull(url)
        })
    }

    fn tag_exists(&mut self, repo: &GithubRepo, tag: &TagName) -> Option<Read<bool>> {
        let key = (repo.clone(), tag.clone());
        cached(&mut self.reads.tags, key, self.forge, |forge| {
            forge.tag_exists(repo, tag)
        })
    }

    fn contains(
        &mut self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Option<Read<bool>> {
        let key = (repo.clone(), tag.clone(), sha.clone());
        cached(&mut self.reads.contains, key, self.forge, |forge| {
            forge.tag_contains(repo, tag, sha)
        })
    }

    fn checks(&mut self, repo: &GithubRepo, sha: &CommitSha) -> Option<Read<Vec<CheckState>>> {
        let key = (repo.clone(), sha.clone());
        cached(&mut self.reads.checks, key, self.forge, |forge| {
            forge.checks(repo, sha)
        })
    }

    /// A codex step merged on GitHub moves to merged only when its merge gate holds at the PR
    /// head, as `cm advance` requires; otherwise it stays at pr-open with a `merge-gate`.
    fn gate_holds(
        &mut self,
        target: &str,
        step: &Step,
        url: &str,
        head: Option<&CommitSha>,
    ) -> bool {
        if step.lane != Some(Lane::Codex) {
            return true;
        }
        let Some(head) = head else {
            self.unknown(
                target,
                format!("{url} is merged but GitHub reported no head"),
            );
            return false;
        };
        let Some(parsed) = parse_target(target, true).ok() else {
            return false;
        };
        let mut gaps =
            match crate::verdict::merge_gaps(self.root, &parsed, Some(url), head.as_str()) {
                Ok(gaps) => gaps,
                Err(error) => {
                    self.unknown(target, format!("{error:#}"));
                    return false;
                }
            };
        let Some(repo) = GithubRepo::of_pr(url) else {
            return false;
        };
        match self.checks(&repo, head) {
            None => return false,
            Some(Ok(checks)) if ci_green(&checks) => {}
            Some(Ok(_)) => gaps.push(format!("CI is not green at {head}")),
            Some(Err(error)) => gaps.push(format!("cannot read CI at {head}: {error}")),
        }
        for gap in &gaps {
            self.violations.push(Violation::new(
                target,
                "merge-gate",
                format!(
                    "{url} merged on GitHub, but {gap}; it stays pr-open until the gate holds or `cm advance {target} merged --override <evidence>`"
                ),
            ));
        }
        gaps.is_empty()
    }

    fn merges(&mut self) {
        for (target, step) in self.steps_at(&Phase::PrOpen) {
            let Some(url) = step.track.evidence.pr.clone() else {
                continue;
            };
            let Some(read) = self.pull(&url) else {
                continue;
            };
            match read {
                Ok(Pull {
                    state: PullState::Merged,
                    merge_commit: Some(sha),
                    head,
                    ..
                }) => {
                    if self.gate_holds(&target, &step, &url, head.as_ref()) {
                        self.advance(
                            &target,
                            Phase::Merged,
                            Evidence {
                                sha: Some(sha.to_string()),
                                ..Evidence::default()
                            },
                        );
                    }
                }
                Ok(Pull {
                    state: PullState::Merged,
                    merge_commit: None,
                    ..
                }) => self.unknown(&target, format!("{url} is merged but has no merge commit")),
                Ok(Pull {
                    state: PullState::Closed,
                    ..
                }) => self.violations.push(Violation::new(
                    &target,
                    "pr-closed",
                    format!(
                        "{url} closed unmerged; reopen it, or `cm advance {target} blocked:<reason>`"
                    ),
                )),
                Ok(Pull {
                    state: PullState::Open,
                    head,
                    mergeable_state,
                    ..
                }) => {
                    self.open.insert(
                        url.clone(),
                        plan::Observed {
                            target: target.clone(),
                            at: self.at.clone(),
                            head: head.map(|head| head.to_string()),
                            mergeable_state,
                        },
                    );
                }
                Err(error) => self.unknown(&target, format!("{url}: {error}")),
            }
        }
    }

    fn tags(&mut self) {
        for (target, step) in self.steps_at(&Phase::Merged) {
            let Some(release) = Release::of_target(&target) else {
                continue;
            };
            let Some((repo, sha)) = merge_of(&step.track.evidence) else {
                self.unknown(
                    &target,
                    "a merged release step needs its prep PR and merge commit to find its tag"
                        .to_owned(),
                );
                continue;
            };
            if !step.ships_from(&release, &repo) {
                self.violations.push(Violation::new(
                    &target,
                    "release-repo",
                    format!(
                        "the prep PR is in {repo}, but {} ships from {}",
                        release,
                        step.release_repo(&release)
                    ),
                ));
                continue;
            }
            let tag = step.release_tag(&release);
            match self.tag_exists(&repo, &tag) {
                None | Some(Ok(false)) => {}
                Some(Ok(true)) => match self.contains(&repo, &tag, &sha) {
                    None => {}
                    Some(Ok(true)) => self.advance(
                        &target,
                        Phase::Released,
                        Evidence {
                            tag: Some(tag.to_string()),
                            ..Evidence::default()
                        },
                    ),
                    Some(Ok(false)) => self.violations.push(Violation::new(
                        &target,
                        "release-tag",
                        format!("{repo} tag {tag} does not contain the prep merge {sha}"),
                    )),
                    Some(Err(error)) => self.unknown(&target, format!("{repo} {tag}: {error}")),
                },
                Some(Err(error)) => self.unknown(&target, format!("{repo} {tag}: {error}")),
            }
        }
    }

    fn releases(&self, at_least: &Phase) -> Vec<Shipped> {
        let mut out: Vec<Shipped> = self
            .units
            .iter()
            .flat_map(|(unit, state)| {
                state
                    .steps
                    .iter()
                    .map(move |(name, step)| (format!("{unit}/{name}"), step))
            })
            .filter(|(_, step)| {
                matches!((step.track.state.rank(), at_least.rank()), (Some(at), Some(needed)) if at >= needed)
            })
            .filter_map(|(target, step)| {
                let release = Release::of_target(&target)?;
                let repo = step.track.evidence.pr.as_deref().and_then(GithubRepo::of_pr)?;
                let tag = step.track.evidence.tag.as_deref()?.parse().ok()?;
                step.ships_from(&release, &repo).then(|| Shipped {
                    release,
                    repo,
                    tag,
                    digest: step.track.evidence.digest.clone(),
                })
            })
            .collect();
        out.sort_by_key(|shipped| shipped.release.version);
        out
    }

    fn released(&mut self) {
        let releases = self.releases(&Phase::Released);
        for (target, step) in self.steps_at(&Phase::Merged) {
            if Release::of_target(&target).is_some() {
                continue;
            }
            let Some((repo, sha)) = merge_of(&step.track.evidence) else {
                continue;
            };
            for shipped in releases.iter().filter(|shipped| shipped.repo == repo) {
                match self.contains(&repo, &shipped.tag, &sha) {
                    None => break,
                    Some(Ok(true)) => {
                        self.advance(
                            &target,
                            Phase::Released,
                            Evidence {
                                tag: Some(shipped.tag.to_string()),
                                ..Evidence::default()
                            },
                        );
                        break;
                    }
                    Some(Ok(false)) => {}
                    Some(Err(error)) => {
                        self.unknown(&target, format!("{repo} {}: {error}", shipped.tag));
                        break;
                    }
                }
            }
        }
    }

    fn deployed(&mut self) {
        let released = self.releases(&Phase::Released);
        let deployed = self.releases(&Phase::Deployed);
        for (target, step) in self.steps_at(&Phase::Released) {
            if Release::of_target(&target).is_some() {
                continue;
            }
            let Some((repo, sha)) = merge_of(&step.track.evidence) else {
                continue;
            };
            let Some(tag) = step
                .track
                .evidence
                .tag
                .as_deref()
                .and_then(|tag| tag.parse::<TagName>().ok())
            else {
                continue;
            };
            let line = released
                .iter()
                .find(|shipped| shipped.repo == repo && shipped.tag == tag)
                .map(|shipped| shipped.release.line.clone());
            let mut found = None;
            for shipped in deployed.iter().filter(|shipped| shipped.repo == repo) {
                if shipped.tag == tag {
                    found = Some(shipped);
                    break;
                }
                if line.as_ref() != Some(&shipped.release.line) {
                    continue;
                }
                match self.contains(&repo, &shipped.tag, &sha) {
                    None => break,
                    Some(Ok(true)) => {
                        found = Some(shipped);
                        break;
                    }
                    Some(Ok(false)) => {}
                    Some(Err(error)) => {
                        self.unknown(&target, format!("{repo} {}: {error}", shipped.tag));
                        break;
                    }
                }
            }
            if let Some(digest) = found.and_then(|shipped| shipped.digest.clone()) {
                self.advance(
                    &target,
                    Phase::Deployed,
                    Evidence {
                        digest: Some(digest),
                        ..Evidence::default()
                    },
                );
            }
        }
    }
}

fn guard(root: &Path) -> Result<Vec<Violation>> {
    let campaign = store::load_campaign(root)?;
    let mut guard = journal::check(root, &campaign);
    guard.extend(closed(root, &campaign));
    Ok(guard)
}

pub fn sync(host: &dyn Host, forge: &dyn Forge, root: &Path) -> Result<Outcome> {
    let found = guard(root)?;
    if !found.is_empty() {
        return Ok(Outcome::refused(found));
    }
    let at = stamp(host.now());
    let mut read = Run::new(
        root,
        Some(forge),
        Reads::default(),
        at.clone(),
        plan::load_units(root)?,
    );
    read.walk();
    let _lock = lock(root)?;
    let found = guard(root)?;
    if !found.is_empty() {
        return Ok(Outcome::refused(found));
    }
    let mut run = Run::new(root, None, read.reads, at, plan::load_units(root)?);
    run.walk();
    for name in &run.dirty {
        write_unit(&root.join(name), &run.units[name])?;
    }
    let mut open = std::mem::take(&mut run.open);
    if let Ok(seen) = store::read_json::<BTreeMap<String, plan::Observed>>(&plan::pulls_path(root))
    {
        // A PR GitHub could not answer for keeps what the last sync saw, stamp and all, while
        // its step still rests at pr-open on it.
        for (url, observed) in seen {
            let failed = !matches!(run.reads.pulls.get(&url), Some(Ok(_)));
            let current = plan::step(&run.units, &observed.target).is_some_and(|step| {
                step.track.state == Phase::PrOpen
                    && step.track.evidence.pr.as_deref() == Some(url.as_str())
            });
            if failed && current {
                open.entry(url).or_insert(observed);
            }
        }
    }
    store::write_json(&plan::pulls_path(root), &open)?;
    let mut outcome = Outcome::refused(std::mem::take(&mut run.violations));
    if !run.moves.is_empty() {
        let body: String = run
            .moves
            .iter()
            .map(|moved| {
                format!(
                    "- {}: {} → {} ({})\n",
                    moved.target,
                    moved.from,
                    moved.to,
                    moved.evidence.summary()
                )
            })
            .collect();
        let title = format!("cm sync: {} move(s) from GitHub", run.moves.len());
        journal::append(root, &journal::entry(&run.at, &title, Some(&body)))?;
        outcome.field("journal", journal::heading(&run.at, &title));
    }
    let next = plan::summary(root, &run.units);
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let card = render::card(&run.at, &name, &run.units, &next, &run.moves);
    write_atomic(&root.join(CARD), card.as_bytes())?;
    for moved in &run.moves {
        outcome.line(format!(
            "{}: {} → {} ({})",
            moved.target,
            moved.from,
            moved.to,
            moved.evidence.summary()
        ));
    }
    let limit = plan::stall_hours(root);
    let stalled = plan::stalls(&run.units, host.now(), limit);
    let dirty = plan::dirty(root, &run.units);
    for stall in &stalled {
        outcome.line(plan::stall_line(stall, limit));
    }
    for (url, observed) in &dirty {
        outcome.line(plan::dirty_line(url, observed));
    }
    outcome.field(
        "stalled",
        stalled
            .iter()
            .map(|stall| plan::stall_json(stall, limit))
            .collect::<Vec<_>>(),
    );
    outcome.field(
        "dirty",
        dirty
            .iter()
            .map(|(url, observed)| plan::dirty_json(url, observed))
            .collect::<Vec<_>>(),
    );
    outcome.line(format!(
        "synced at {}: {} move(s); wrote {CARD}",
        run.at,
        run.moves.len()
    ));
    outcome.field("at", run.at.clone());
    outcome.field("card", CARD);
    let moves = run
        .moves
        .iter()
        .map(|moved| {
            Ok(json!({
                "target": moved.target,
                "from": moved.from.to_string(),
                "to": moved.to.to_string(),
                "evidence": serde_json::to_value(&moved.evidence)?,
            }))
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    outcome.field("moves", moves);
    Ok(outcome)
}
