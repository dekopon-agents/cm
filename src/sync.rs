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
use crate::state::{Evidence, Lane, Move, Phase, Release, Step, Track};
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
    completed_checks: BTreeMap<(GithubRepo, CommitSha, Vec<String>), Read<Vec<CheckState>>>,
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
    now: jiff::Timestamp,
    heads: BTreeMap<String, String>,
    notes: Vec<(String, String)>,
}

struct Tracked {
    target: String,
    part: Option<String>,
    track: Track,
    step: Step,
}

impl Tracked {
    fn name(&self) -> String {
        match &self.part {
            Some(part) => format!("{}#{part}", self.target),
            None => self.target.clone(),
        }
    }
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
        now: jiff::Timestamp,
        units: Units,
    ) -> Self {
        Run {
            root,
            forge,
            reads,
            at: stamp(now),
            units,
            dirty: BTreeSet::new(),
            moves: Vec::new(),
            violations: Vec::new(),
            open: BTreeMap::new(),
            now,
            heads: BTreeMap::new(),
            notes: Vec::new(),
        }
    }

    fn walk(&mut self) {
        self.merges();
        self.tags();
        self.released();
        self.deployed();
        self.settle();
        let mut notes = crate::holds::mark_stale(&mut self.units, &self.heads, &self.at);
        notes.extend(crate::holds::mark_closed(&mut self.units, &self.at));
        notes.extend(crate::holds::mark_expired(
            &mut self.units,
            self.now,
            &self.at,
            self.root,
        ));
        for (unit, _) in &notes {
            self.dirty.insert(unit.clone());
        }
        self.notes.extend(notes);
    }

    fn settle(&mut self) {
        for (unit_name, unit) in self.units.iter_mut() {
            for (name, step) in unit.steps.iter_mut() {
                let target = format!("{unit_name}/{name}");
                for (from, to) in step.roll_up_all(&self.at, &Evidence::default(), self.root) {
                    unit.history.push(Move {
                        at: self.at.clone(),
                        target: target.clone(),
                        from: from.clone(),
                        to: to.clone(),
                        evidence: Evidence::default(),
                    });
                    self.dirty.insert(unit_name.clone());
                    self.moves.push(Moved {
                        target: target.clone(),
                        from,
                        to,
                        evidence: Evidence::default(),
                    });
                }
            }
        }
    }

    fn tracks_at(&self, phase: &Phase) -> Vec<Tracked> {
        let mut out = Vec::new();
        for (unit, state) in &self.units {
            for (name, step) in &state.steps {
                let target = format!("{unit}/{name}");
                if step.parts.is_empty() {
                    if step.track.state == *phase {
                        out.push(Tracked {
                            target,
                            part: None,
                            track: step.track.clone(),
                            step: step.clone(),
                        });
                    }
                    continue;
                }
                for (part, track) in &step.parts {
                    if track.state == *phase {
                        out.push(Tracked {
                            target: target.clone(),
                            part: Some(part.clone()),
                            track: track.clone(),
                            step: step.clone(),
                        });
                    }
                }
            }
        }
        out
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

    fn advance(&mut self, target: &str, part: Option<&str>, to: Phase, evidence: Evidence) {
        let Some((unit_name, step_name)) = plan::split(target) else {
            return;
        };
        let Some(unit) = self.units.get_mut(unit_name) else {
            return;
        };
        let Some(step) = unit.steps.get_mut(step_name) else {
            return;
        };
        let finish = step.finish();
        let (track, name) = match part {
            Some(part) => match step.parts.get_mut(part) {
                Some(track) => (track, format!("{target}#{part}")),
                None => return,
            },
            None => (&mut step.track, target.to_owned()),
        };
        if track.check(&to, finish).is_err() {
            return;
        }
        let from = track.state.clone();
        track.apply(to.clone(), &self.at, &evidence);
        let mut moved = vec![Moved {
            target: name,
            from,
            to,
            evidence: evidence.clone(),
        }];
        if part.is_some() {
            for (from, to) in step.roll_up_all(&self.at, &evidence, self.root) {
                moved.push(Moved {
                    target: target.to_owned(),
                    from,
                    to,
                    evidence: evidence.clone(),
                });
            }
        }
        for moved in moved {
            unit.history.push(Move {
                at: self.at.clone(),
                target: moved.target.clone(),
                from: moved.from.clone(),
                to: moved.to.clone(),
                evidence: moved.evidence.clone(),
            });
            self.moves.push(moved);
        }
        self.dirty.insert(unit_name.to_owned());
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

    fn completed_checks(
        &mut self,
        repo: &GithubRepo,
        base: &CommitSha,
        checks: &[CheckState],
        step: &Step,
    ) -> Option<Read<Vec<CheckState>>> {
        let standing = crate::holds::standing_names(&self.units, &step.known_red);
        let names: Vec<_> = checks
            .iter()
            .filter(|check| !check.green() && standing.contains(&check.name))
            .map(|check| check.name.clone())
            .collect();
        let key = (repo.clone(), base.clone(), names.clone());
        cached(&mut self.reads.completed_checks, key, self.forge, |forge| {
            forge.latest_completed_checks(repo, base, &names)
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
        base: Option<&CommitSha>,
    ) -> Option<Vec<String>> {
        if step.lane != Some(Lane::Codex) {
            return Some(Vec::new());
        }
        let Some(head) = head else {
            self.unknown(
                target,
                format!("{url} is merged but GitHub reported no head"),
            );
            return None;
        };
        let parsed = parse_target(target, true).ok()?;
        // A local step's worktree lets the gate take a clean update from the base branch;
        // without one it needs a review at the merged head itself.
        let worktree = Path::new(&step.worktree);
        let lineage =
            (!step.worktree.is_empty() && worktree.exists()).then(|| crate::verdict::Lineage {
                repo: worktree,
                base: base.map(CommitSha::as_str),
            });
        let mut gaps = match crate::verdict::merge_gaps(
            self.root,
            &parsed,
            Some(url),
            head.as_str(),
            lineage.as_ref(),
        ) {
            Ok(cleared) => cleared.gaps,
            Err(error) => {
                self.unknown(target, format!("{error:#}"));
                return None;
            }
        };
        let repo = GithubRepo::of_pr(url)?;
        let mut used = Vec::new();
        match self.checks(&repo, head) {
            None => return None,
            Some(Ok(checks)) if ci_green(&checks) => {}
            Some(Ok(checks)) if checks.is_empty() => {
                gaps.push(format!("CI is not green at {head}"));
            }
            Some(Ok(checks)) => {
                let before = match base {
                    Some(base) => match self.checks(&repo, base) {
                        Some(Ok(before)) => Some(before),
                        Some(Err(_)) | None => None,
                    },
                    None => None,
                };
                let completed = base
                    .and_then(|base| self.completed_checks(&repo, base, &checks, step))
                    .and_then(Result::ok);
                let (accepted, refused) = crate::commands::red_checks(
                    &step.known_red,
                    &self.units,
                    head.as_str(),
                    &checks,
                    before.as_deref(),
                    completed.as_deref(),
                );
                if refused.is_empty() {
                    used = accepted;
                } else {
                    let named: Vec<&str> = refused.iter().map(|(_, text)| text.as_str()).collect();
                    gaps.push(format!("CI is not green at {head}: {}", named.join(", ")));
                }
            }
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
        gaps.is_empty().then_some(used)
    }

    fn merges(&mut self) {
        for tracked in self.tracks_at(&Phase::PrOpen) {
            let target = tracked.name();
            let Some(url) = tracked.track.evidence.pr.clone() else {
                continue;
            };
            let head_of = |pull: &Read<Pull>| match pull {
                Ok(pull) => pull.head.as_ref().map(ToString::to_string),
                Err(_) => None,
            };
            let Some(read) = self.pull(&url) else {
                continue;
            };
            if let Some(head) = head_of(&read) {
                self.heads.insert(target.clone(), head);
            }
            match read {
                Ok(Pull {
                    state: PullState::Merged,
                    merge_commit: Some(sha),
                    head,
                    base,
                    ..
                }) => {
                    if let Some(used) = self.gate_holds(
                        &tracked.target,
                        &tracked.step,
                        &url,
                        head.as_ref(),
                        base.as_ref(),
                    ) {
                        self.notes.extend(
                            used.into_iter()
                                .map(|line| (String::new(), format!("{line} ({target})"))),
                        );
                        self.advance(
                            &tracked.target,
                            tracked.part.as_deref(),
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
                        None,
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
                let repo = match step.track.evidence.pr.as_deref() {
                    Some(pr) => GithubRepo::of_pr(pr)?,
                    None => step.github.clone()?,
                };
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
        'steps: for tracked in self.tracks_at(&Phase::Merged) {
            let target = tracked.name();
            if Release::of_target(&tracked.target).is_some() {
                continue;
            }
            let Some((repo, sha)) = merge_of(&tracked.track.evidence) else {
                continue;
            };
            let mut found: Option<&Shipped> = None;
            let order = |shipped: &Shipped| {
                (
                    shipped.release.line != repo.name(),
                    shipped.release.line.clone(),
                    shipped.release.version,
                )
            };
            for shipped in releases.iter().filter(|shipped| shipped.repo == repo) {
                match self.contains(&repo, &shipped.tag, &sha) {
                    None => {
                        self.unknown(
                            &target,
                            format!("{repo} {}: containment not read", shipped.tag),
                        );
                        continue 'steps;
                    }
                    Some(Ok(true)) => {
                        if found.is_none_or(|current| order(shipped) < order(current)) {
                            found = Some(shipped);
                        }
                    }
                    Some(Ok(false)) => {}
                    Some(Err(error)) => {
                        self.unknown(&target, format!("{repo} {}: {error}", shipped.tag));
                        continue 'steps;
                    }
                }
            }
            if let Some(shipped) = found {
                self.advance(
                    &tracked.target,
                    tracked.part.as_deref(),
                    Phase::Released,
                    Evidence {
                        tag: Some(shipped.tag.to_string()),
                        ..Evidence::default()
                    },
                );
            }
        }
    }

    fn watched(&mut self, target: &str, step: &Step) -> bool {
        let held: Vec<String> = step
            .unresolved_watches(self.root)
            .map(|watch| {
                format!(
                    "#{} {} ({}, deadline {})",
                    watch.id,
                    watch.status(self.now, self.root).name(),
                    watch.signal,
                    watch.deadline
                )
            })
            .collect();
        if held.is_empty() {
            return false;
        }
        self.violations.push(Violation::new(
            target,
            "watch-open",
            format!(
                "its release deployed, but it stays released until watch {} resolves with evidence",
                held.join(", ")
            ),
        ));
        true
    }

    fn deployed(&mut self) {
        let released = self.releases(&Phase::Released);
        let deployed = self.releases(&Phase::Deployed);
        for tracked in self.tracks_at(&Phase::Released) {
            let target = tracked.name();
            if Release::of_target(&tracked.target).is_some() {
                continue;
            }
            let Some((repo, sha)) = merge_of(&tracked.track.evidence) else {
                continue;
            };
            let Some(tag) = tracked
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
                if tracked.part.is_none() && self.watched(&target, &tracked.step) {
                    continue;
                }
                self.advance(
                    &tracked.target,
                    tracked.part.as_deref(),
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
    let now = host.now();
    let mut read = Run::new(
        root,
        Some(forge),
        Reads::default(),
        now,
        plan::load_units(root)?,
    );
    read.walk();
    let _lock = lock(root)?;
    let found = guard(root)?;
    if !found.is_empty() {
        return Ok(Outcome::refused(found));
    }
    let mut run = Run::new(root, None, read.reads, now, plan::load_units(root)?);
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
            let current = plan::track(&run.units, &observed.target).is_some_and(|track| {
                track.state == Phase::PrOpen && track.evidence.pr.as_deref() == Some(url.as_str())
            });
            if failed && current {
                open.entry(url).or_insert(observed);
            }
        }
    }
    store::write_json(&plan::pulls_path(root), &open)?;
    let mut outcome = Outcome::refused(std::mem::take(&mut run.violations));
    let marker_path = root.join(store::CM_DIR).join("unblock-notified.json");
    let mut notified: BTreeSet<String> = if marker_path.exists() {
        store::read_json(&marker_path)?
    } else {
        BTreeSet::new()
    };
    let mut changed = false;
    for item in plan::unblock_now(root, &run.units, None) {
        let key = serde_json::to_string(&item)?;
        if notified.insert(key) {
            changed = true;
            run.notes.push((
                String::new(),
                format!(
                    "- unblock now {}: {} holds; still blocked",
                    item["target"].as_str().unwrap(),
                    item["condition"].as_str().unwrap()
                ),
            ));
        }
    }
    if !run.moves.is_empty() || !run.notes.is_empty() {
        let mut body: String = run
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
        for (_, note) in &run.notes {
            body.push_str(note);
            body.push('\n');
            outcome.line(note.trim_start_matches("- ").to_owned());
        }
        let title = format!("cm sync: {} move(s) from GitHub", run.moves.len());
        journal::append(root, &journal::entry(&run.at, &title, Some(&body)))?;
        outcome.field("journal", journal::heading(&run.at, &title));
    }
    if changed {
        store::write_json(&marker_path, &notified)?;
    }
    let next = plan::summary(root, &run.units);
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let card = render::card(root, &run.at, &name, &run.units, &next, &run.moves);
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
