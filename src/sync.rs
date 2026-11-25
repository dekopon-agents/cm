use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use crate::attempt::{recorded_id, remote_turn};
use crate::commands::{check_release_pr, closed, is_pr_url, write_unit};
use crate::forge::{
    CheckState, CommitSha, Forge, ForgeError, GithubRepo, MergeableState, Pull, PullState,
    TURN_READER_TIMEOUT, TagName, TurnRead, ci_green, read_remote_turn,
};
use crate::host::{Host, stamp};
use crate::journal;
use crate::limits::TurnCommands;
use crate::outcome::{Outcome, Violation};
use crate::plan::{self, Units};
use crate::render;
use crate::state::{
    Evidence, Lane, Move, ObservedTurn, Phase, Release, Step, Track, Version, Work,
};
use crate::store::{self, lock, parse_target, write_atomic};

pub const CARD: &str = "CARD.md";

pub const BUDGET: Duration = Duration::from_secs(30);

const READERS: usize = 8;

const PLAN_ROUNDS: usize = 5;

pub struct Moved {
    pub target: String,
    pub from: Phase,
    pub to: Phase,
    pub evidence: Evidence,
}

type Read<T> = Result<T, ForgeError>;
type TurnQuery = (String, String, String, Vec<String>);

const PULL_MAX_AGE: jiff::SignedDuration = jiff::SignedDuration::from_mins(30);

// Ignored by git, so a quiet sync's reads change no tracked file.
pub const PULLS_READ: &str = "pulls-read.json";

const UNREADABLE_NOTIFIED: &str = "unreadable-notified.json";

// A PR's 304 restores the stored observation, so its ETag goes out only while `.cm/pulls.json`
// holds the one it validated; a check page's ETag is keyed by its own head's URL.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct LastRead {
    at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mergeable_state: Option<MergeableState>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    etags: BTreeMap<String, String>,
}

impl LastRead {
    fn validates(&self, seen: &plan::Observed) -> bool {
        self.head == seen.head
            && self.base == seen.base
            && self.mergeable_state == seen.mergeable_state
    }
}

#[derive(Clone)]
struct PullRead {
    pull: Pull,
    etags: BTreeMap<String, String>,
    cached_at: Option<String>,
}

#[derive(Clone, Default)]
struct OpenChecks {
    /// GitHub answered 304 for every page: nothing changed since the stored ETags.
    unchanged: bool,
    completed: bool,
    green: bool,
    etags: BTreeMap<String, String>,
}

#[derive(Default)]
struct Reads {
    branch_pulls: BTreeMap<(String, GithubRepo, String), Read<Option<String>>>,
    turns: BTreeMap<TurnQuery, Result<TurnRead, String>>,
    pulls: BTreeMap<String, Read<PullRead>>,
    open_checks: BTreeMap<(GithubRepo, CommitSha), Read<OpenChecks>>,
    tags: BTreeMap<(GithubRepo, TagName), Read<bool>>,
    contains: BTreeMap<(GithubRepo, TagName, CommitSha), Read<bool>>,
    checks: BTreeMap<(GithubRepo, CommitSha), Read<Vec<CheckState>>>,
    publications:
        BTreeMap<(Vec<String>, GithubRepo, TagName, Version), Read<crate::forge::Publication>>,
    completed_checks: BTreeMap<(GithubRepo, CommitSha, Vec<String>), Read<Vec<CheckState>>>,
    made: BTreeMap<&'static str, usize>,
}

impl Reads {
    fn summary(&self) -> String {
        if self.made.is_empty() {
            return "no forge reads".to_owned();
        }
        let kinds: Vec<String> = self
            .made
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        kinds.join(", ")
    }
}

type Store = Box<dyn FnOnce(&mut Reads) + Send>;
type Job = Box<dyn FnOnce(&dyn Forge) -> Store + Send>;

#[derive(Clone, Copy)]
enum Access<'a> {
    Read(&'a dyn Forge),
    Plan,
    Cached,
}

#[derive(Clone, Copy)]
pub enum Reader<'a> {
    Serial(&'a dyn Forge),
    Concurrent(&'a (dyn Forge + Sync)),
}

struct Phases {
    start: Instant,
    current: Option<(&'static str, Instant, String)>,
    spent: Vec<(&'static str, Duration, String)>,
}

impl Phases {
    fn new() -> Self {
        Phases {
            start: Instant::now(),
            current: None,
            spent: Vec::new(),
        }
    }

    fn enter(&mut self, phase: &'static str) {
        self.close();
        self.current = Some((phase, Instant::now(), String::new()));
    }

    fn note(&mut self, note: String) {
        if let Some((_, _, current)) = &mut self.current {
            *current = note;
        }
    }

    fn fail(&mut self) {
        if let Some((_, _, note)) = &mut self.current {
            if !note.is_empty() {
                note.push_str("; ");
            }
            note.push_str("failed");
        }
        self.close();
    }

    fn close(&mut self) {
        if let Some((phase, began, note)) = self.current.take() {
            self.spent.push((phase, began.elapsed(), note));
        }
    }

    fn lines(&self, total: Duration, budget: Duration) -> Vec<String> {
        let mut lines = vec![format!(
            "sync took {:.2} s (budget {} s); phases:",
            total.as_secs_f64(),
            budget.as_secs()
        )];
        lines.extend(self.spent.iter().map(|(phase, spent, note)| {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!(" ({note})")
            };
            format!("phase {phase}: {:.2} s{note}", spent.as_secs_f64())
        }));
        lines
    }

    fn json(&self, total: Duration, budget: Duration) -> Value {
        json!({
            "seconds": total.as_secs_f64(),
            "budget_seconds": budget.as_secs(),
            "phases": self.spent.iter().map(|(phase, spent, note)| json!({
                "phase": phase,
                "seconds": spent.as_secs_f64(),
                "note": note,
            })).collect::<Vec<_>>(),
        })
    }
}

struct Shipped {
    release: Release,
    repo: GithubRepo,
    tag: TagName,
    digest: Option<String>,
}

struct Run<'a> {
    root: &'a Path,
    access: Access<'a>,
    reads: Reads,
    wanted: Vec<Job>,
    planned: BTreeSet<String>,
    at: String,
    units: Units,
    dirty: BTreeSet<String>,
    moves: Vec<Moved>,
    violations: Vec<Violation>,
    open: BTreeMap<String, plan::Observed>,
    seen: BTreeMap<String, plan::Observed>,
    last_read: BTreeMap<String, LastRead>,
    read_now: BTreeMap<String, LastRead>,
    now: jiff::Timestamp,
    heads: BTreeMap<String, String>,
    notes: Vec<(String, String)>,
    owner_unread: Vec<String>,
    unreadable: BTreeSet<String>,
    unread_publications: BTreeSet<String>,
    discovery_warnings: Vec<String>,
    turn_commands: TurnCommands,
    turn_warnings: Vec<String>,
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

// Every field but the stamp is a fact, including any a later change adds.
fn same_facts(a: &plan::Observed, b: &plan::Observed) -> bool {
    plan::Observed {
        at: b.at.clone(),
        ..a.clone()
    } == *b
}

fn merge_of(evidence: &Evidence) -> Option<(GithubRepo, CommitSha)> {
    let repo = evidence.pr.as_deref().and_then(GithubRepo::of_pr)?;
    let sha = CommitSha::new(evidence.sha.clone()?);
    Some((repo, sha))
}

impl<'a> Run<'a> {
    fn new(
        root: &'a Path,
        access: Access<'a>,
        reads: Reads,
        now: jiff::Timestamp,
        units: Units,
        owner_unread: Vec<String>,
    ) -> Self {
        let turn_commands = TurnCommands::read(root);
        let turn_warnings = turn_commands.errors().cloned().collect();
        Run {
            root,
            access,
            reads,
            wanted: Vec::new(),
            planned: BTreeSet::new(),
            at: stamp(now),
            units,
            dirty: BTreeSet::new(),
            moves: Vec::new(),
            violations: Vec::new(),
            open: BTreeMap::new(),
            seen: store::read_json(&plan::pulls_path(root)).unwrap_or_default(),
            last_read: store::read_json(&root.join(store::CM_DIR).join(PULLS_READ))
                .unwrap_or_default(),
            read_now: BTreeMap::new(),
            now,
            heads: BTreeMap::new(),
            notes: Vec::new(),
            owner_unread,
            unreadable: BTreeSet::new(),
            unread_publications: BTreeSet::new(),
            discovery_warnings: Vec::new(),
            turn_commands,
            turn_warnings,
        }
    }

    fn planning(&self) -> bool {
        matches!(self.access, Access::Plan)
    }

    fn read<K, T>(
        &mut self,
        kind: &'static str,
        cache: fn(&mut Reads) -> &mut BTreeMap<K, T>,
        key: K,
        read: impl FnOnce(&dyn Forge) -> T + Send + 'static,
    ) -> Option<T>
    where
        K: Ord + fmt::Debug + Send + 'static,
        T: Clone + Send + 'static,
    {
        if let Some(hit) = cache(&mut self.reads).get(&key) {
            return Some(hit.clone());
        }
        match self.access {
            Access::Read(forge) => {
                let value = read(forge);
                *self.reads.made.entry(kind).or_default() += 1;
                cache(&mut self.reads).insert(key, value.clone());
                Some(value)
            }
            Access::Plan => {
                if self.planned.insert(format!("{kind} {key:?}")) {
                    self.wanted
                        .push(Box::new(move |forge: &dyn Forge| -> Store {
                            let value = read(forge);
                            Box::new(move |reads: &mut Reads| {
                                *reads.made.entry(kind).or_default() += 1;
                                cache(reads).entry(key).or_insert(value);
                            })
                        }));
                }
                None
            }
            Access::Cached => None,
        }
    }

    fn walk(&mut self) {
        self.observe_turns();
        self.discover_pulls();
        self.merges();
        self.tags();
        self.released();
        self.deployed();
        self.settle();
        self.remember_heads();
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

    fn remember_heads(&mut self) {
        let seen = &self.seen;
        for (unit_name, unit) in &mut self.units {
            for (name, step) in &mut unit.steps {
                let target = format!("{unit_name}/{name}");
                let tracks = std::iter::once((target.clone(), &mut step.track)).chain(
                    step.parts
                        .iter_mut()
                        .map(|(part, track)| (format!("{target}#{part}"), track)),
                );
                for (target, track) in tracks {
                    if track.resting() != &Phase::PrOpen {
                        continue;
                    }
                    let Some(pr) = &track.evidence.pr else {
                        continue;
                    };
                    let head = self.heads.get(&target).cloned().or_else(|| {
                        self.open
                            .get(pr)
                            .or_else(|| seen.get(pr))
                            .filter(|observed| observed.target == target)
                            .and_then(|observed| observed.head.clone())
                    });
                    if let Some(head) = head
                        && track.evidence.sha.as_deref() != Some(head.as_str())
                    {
                        let before = track
                            .evidence
                            .sha
                            .replace(head.clone())
                            .unwrap_or_else(|| "not recorded".into());
                        self.dirty.insert(unit_name.clone());
                        self.notes.push((
                            unit_name.clone(),
                            format!("- head {target}: {before} → {head}"),
                        ));
                    }
                }
            }
        }
    }

    fn settle(&mut self) {
        for (unit_name, unit) in self.units.iter_mut() {
            for (name, step) in unit.steps.iter_mut() {
                let target = format!("{unit_name}/{name}");
                if !self.owner_unread.is_empty()
                    && step.track.state == Phase::PrOpen
                    && step.parts_reach() == Some(Phase::Merged)
                {
                    self.violations.push(Violation::new(
                        crate::templates::QUEUE_FILE,
                        "owner-queue-unread",
                        format!(
                            "{target} waits for `cm owner-queue read`: {}",
                            self.owner_unread.join(", ")
                        ),
                    ));
                    continue;
                }
                for (from, to) in step.roll_up_all(
                    Release::of_target(&target).is_some(),
                    &self.at,
                    &Evidence::default(),
                    self.root,
                ) {
                    let evidence = Evidence {
                        merged_at: if to == Phase::Merged {
                            step.track.evidence.merged_at.clone()
                        } else {
                            None
                        },
                        ..Evidence::default()
                    };
                    unit.history.push(Move {
                        at: self.at.clone(),
                        target: target.clone(),
                        from: from.clone(),
                        to: to.clone(),
                        evidence: evidence.clone(),
                    });
                    self.dirty.insert(unit_name.clone());
                    self.moves.push(Moved {
                        target: target.clone(),
                        from,
                        to,
                        evidence,
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
        if !self.owner_unread.is_empty()
            && (to == Phase::Merged || Release::of_target(target).is_some())
        {
            self.violations.push(Violation::new(
                crate::templates::QUEUE_FILE,
                "owner-queue-unread",
                format!(
                    "{target} waits for `cm owner-queue read`: {}",
                    self.owner_unread.join(", ")
                ),
            ));
            return;
        }
        let Some((unit_name, step_name)) = plan::split(target) else {
            return;
        };
        let Some(unit) = self.units.get_mut(unit_name) else {
            return;
        };
        let Some(step) = unit.steps.get_mut(step_name) else {
            return;
        };
        let finish = step.finish(Release::of_target(target).is_some());
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
            for (from, to) in step.roll_up_all(
                Release::of_target(target).is_some(),
                &self.at,
                &evidence,
                self.root,
            ) {
                let mut rolled_evidence = evidence.clone();
                if to == Phase::Merged {
                    rolled_evidence.merged_at = step.track.evidence.merged_at.clone();
                }
                moved.push(Moved {
                    target: target.to_owned(),
                    from,
                    to,
                    evidence: rolled_evidence,
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

    fn pull(&mut self, url: &str, target: Option<&str>) -> Option<Read<PullRead>> {
        let fresh = self.last_read.get(url).filter(|read| {
            read.at.parse::<jiff::Timestamp>().is_ok_and(|at| {
                (jiff::SignedDuration::ZERO..PULL_MAX_AGE).contains(&self.now.duration_since(at))
            })
        });
        let previous = self
            .seen
            .get(url)
            .filter(|seen| {
                Some(seen.target.as_str()) == target
                    && fresh.is_some_and(|read| read.validates(seen))
            })
            .cloned();
        let validators = fresh
            .filter(|_| previous.is_some())
            .map(|read| read.etags.clone())
            .unwrap_or_default();
        let read = url.to_owned();
        self.read(
            "pull",
            |reads| &mut reads.pulls,
            url.to_owned(),
            move |forge| {
                let mut etags = BTreeMap::new();
                if let Some(pull) = forge.pull_conditional(&read, &validators, &mut etags)? {
                    return Ok(PullRead {
                        pull,
                        etags,
                        cached_at: None,
                    });
                }
                let seen = previous.ok_or(ForgeError::Json)?;
                let pull = Pull {
                    state: PullState::Open,
                    merge_commit: None,
                    merged_at: None,
                    head: seen.head.as_ref().map(CommitSha::new),
                    head_ref: None,
                    base: seen.base.as_ref().map(CommitSha::new),
                    mergeable_state: seen.mergeable_state.clone(),
                };
                Ok(PullRead {
                    pull,
                    etags,
                    cached_at: Some(seen.at.clone()),
                })
            },
        )
    }

    fn open_checks(
        &mut self,
        repo: &GithubRepo,
        head: &CommitSha,
        previous: &BTreeMap<String, String>,
    ) -> Option<Read<OpenChecks>> {
        let key = (repo.clone(), head.clone());
        let (repo, head, previous) = (repo.clone(), head.clone(), previous.clone());
        self.read(
            "pending-ci",
            |reads| &mut reads.open_checks,
            key,
            move |forge| {
                let mut etags = BTreeMap::new();
                let checks = forge.checks_conditional(&repo, &head, &previous, &mut etags)?;
                let completed = checks.as_ref().is_some_and(|checks| {
                    !checks.is_empty() && checks.iter().all(CheckState::completed)
                });
                let green = checks.as_deref().is_some_and(ci_green);
                Ok(OpenChecks {
                    unchanged: checks.is_none(),
                    completed,
                    green,
                    etags,
                })
            },
        )
    }

    fn tag_exists(&mut self, repo: &GithubRepo, tag: &TagName) -> Option<Read<bool>> {
        let key = (repo.clone(), tag.clone());
        let (repo, tag) = key.clone();
        self.read(
            "tag",
            |reads| &mut reads.tags,
            key,
            move |forge| forge.tag_exists(&repo, &tag),
        )
    }

    fn contains(
        &mut self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Option<Read<bool>> {
        let key = (repo.clone(), tag.clone(), sha.clone());
        let (repo, tag, sha) = key.clone();
        self.read(
            "compare",
            |reads| &mut reads.contains,
            key,
            move |forge| forge.tag_contains(&repo, &tag, &sha),
        )
    }

    fn checks(&mut self, repo: &GithubRepo, sha: &CommitSha) -> Option<Read<Vec<CheckState>>> {
        let key = (repo.clone(), sha.clone());
        let (repo, sha) = key.clone();
        self.read(
            "ci",
            |reads| &mut reads.checks,
            key,
            move |forge| forge.checks(&repo, &sha),
        )
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
        let (repo, base) = (repo.clone(), base.clone());
        self.read(
            "completed-ci",
            |reads| &mut reads.completed_checks,
            key,
            move |forge| forge.latest_completed_checks(&repo, &base, &names),
        )
    }

    /// A codex step merged on GitHub moves to merged only when its merge gate holds at the PR
    /// head, as `cm advance` requires; otherwise it stays at pr-open with a `merge-gate`.
    fn gate_holds(
        &mut self,
        target: &str,
        part: Option<&str>,
        step: &Step,
        url: &str,
        head: Option<&CommitSha>,
        base: Option<&CommitSha>,
    ) -> Option<Vec<String>> {
        if !crate::commands::merge_gated(target.split('#').next().unwrap_or(target), step) {
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
            part,
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

    fn observe_turns(&mut self) {
        // A turn reader is an operator's command cm cannot vouch for as read-only: it runs in
        // the read walk, one at a time, never beside other reads.
        if self.planning() {
            return;
        }
        for (target, step) in self.steps_at(&Phase::Launched) {
            let Some(attempt) = step
                .attempt
                .as_ref()
                .filter(|attempt| attempt.ended.is_none())
            else {
                continue;
            };
            let Some(lane) = attempt.lane.or(step.lane) else {
                continue;
            };
            let Some(programs) = self.turn_commands.for_lane(lane) else {
                continue;
            };
            let Some(turn) = remote_turn(attempt, lane) else {
                continue;
            };
            let ended = turn
                .observation(attempt)
                .filter(|read| read.completed_at.is_some());
            let argv = programs.reader.argv(turn.thread, turn.turn);
            let key = (
                attempt.id.clone(),
                turn.thread.to_owned(),
                turn.turn.to_owned(),
                argv.clone(),
            );
            let (thread, turn_id) = (key.1.clone(), key.2.clone());
            let observed = match self.read(
                "turn",
                |reads| &mut reads.turns,
                key,
                move |_| read_remote_turn(&argv, &thread, &turn_id, TURN_READER_TIMEOUT),
            ) {
                Some(Ok(read)) if read.done => {
                    let completed_at = read
                        .completed_at
                        .filter(|at| plan::instant(at).is_some())
                        .or_else(|| ended.and_then(|read| read.completed_at.clone()))
                        .or_else(|| Some(self.at.clone()));
                    Some(ObservedTurn {
                        thread: turn.thread.to_owned(),
                        turn: turn.turn.to_owned(),
                        completed_at,
                        pr_url: read
                            .pr_url
                            .or_else(|| ended.and_then(|read| read.pr_url.clone())),
                    })
                }
                Some(Ok(_)) => ended.cloned(),
                Some(Err(error)) => {
                    self.turn_warnings.push(format!("{target}: {error}"));
                    ended.cloned()
                }
                // A new record or configuration observed under lock has no read to apply.
                None => continue,
            };
            if observed != attempt.observed_turn {
                let (unit, name) = plan::split(&target).expect("step target has a unit and name");
                let attempt = self
                    .units
                    .get_mut(unit)
                    .expect("scanned unit")
                    .steps
                    .get_mut(name)
                    .expect("scanned step")
                    .attempt
                    .as_mut()
                    .expect("scanned attempt");
                attempt.observed_turn = observed;
                self.dirty.insert(unit.to_owned());
            }
        }
    }

    fn discover_pulls(&mut self) {
        for (target, step) in self.steps_at(&Phase::Launched) {
            let release = Release::of_target(&target);
            // A recorded branch or returned URL belongs to the attempt, with no part identity.
            if !step.parts.is_empty() || (release.is_some() && !self.owner_unread.is_empty()) {
                continue;
            }
            let Some(attempt) = &step.attempt else {
                continue;
            };
            let Some(lane @ (Lane::Codex | Lane::ClaudeCloud)) = attempt.lane.or(step.lane) else {
                continue;
            };
            let kickoff = std::fs::read_to_string(self.root.join(&target).join("kickoff.md")).ok();
            let kickoff_repo = kickoff.as_deref().and_then(|text| {
                text.lines()
                    .take_while(|line| !line.starts_with('#'))
                    .find_map(|line| line.strip_prefix("repo:"))
                    .and_then(|repo| repo.trim().parse::<GithubRepo>().ok())
            });
            let declared = step.github.clone().or(kickoff_repo);
            let repo = declared
                .clone()
                .or_else(|| recorded_id(attempt, "repo")?.parse().ok());
            let Some(repo) = repo else {
                continue;
            };
            if step
                .track
                .check(&Phase::PrOpen, step.finish(release.is_some()))
                .is_err()
                || release.as_ref().is_some_and(|release| {
                    check_release_pr(&target, &step, release, &repo).is_some()
                })
            {
                continue;
            }
            let recorded_branch = recorded_id(attempt, "branch");
            if let Some(branch) = recorded_branch.filter(|branch| !branch.starts_with('-')) {
                // Replay requires the same attempt and launch metadata that the read used.
                let key = (attempt.id.clone(), repo.clone(), branch.to_owned());
                let (lookup, wanted) = (repo.clone(), branch.to_owned());
                match self.read(
                    "branch",
                    |reads| &mut reads.branch_pulls,
                    key,
                    move |forge| forge.open_pull_for_branch(&lookup, &wanted),
                ) {
                    Some(Ok(Some(url))) if self.discover_pr(&target, &repo, &url, None) => continue,
                    Some(Err(error)) => self.discovery_warnings.push(format!(
                        "{target}: could not look up {repo} branch {branch}: {error}"
                    )),
                    Some(Ok(_)) | None => {}
                }
            }
            if let Some(declared) = declared
                && self.turn_commands.for_lane(lane).is_some()
                && let Some(url) = remote_turn(attempt, lane)
                    .and_then(|turn| turn.observation(attempt))
                    .filter(|read| read.completed_at.is_some())
                    .and_then(|read| read.pr_url.as_deref())
            {
                self.discover_pr(&target, &declared, url, recorded_branch);
            }
        }
    }

    fn discover_pr(
        &mut self,
        target: &str,
        repo: &GithubRepo,
        url: &str,
        recorded_branch: Option<&str>,
    ) -> bool {
        if !is_pr_url(url)
            || !GithubRepo::of_pr(url)
                .is_some_and(|pr_repo| pr_repo.to_string().eq_ignore_ascii_case(&repo.to_string()))
        {
            self.discovery_warnings
                .push(format!("{target}: invalid PR URL for {repo}: {url}"));
            return false;
        }
        // Fetch a full observation for a newly discovered PR, including its head branch.
        match self.pull(url, None) {
            Some(Ok(read)) if read.pull.state == PullState::Open => {
                if let Some(branch) = recorded_branch
                    && read.pull.head_ref.as_deref() != Some(branch)
                {
                    let actual = read.pull.head_ref.as_deref().unwrap_or("unreported");
                    self.discovery_warnings.push(format!(
                        "{target}: {url} head branch {actual} does not match recorded branch {branch}"
                    ));
                    return false;
                }
                let head = read.pull.head.filter(|head| {
                    head.as_str().len() == 40 && crate::state::is_commit(head.as_str())
                });
                if let Some(sha) = head {
                    self.advance(
                        target,
                        None,
                        Phase::PrOpen,
                        Evidence {
                            pr: Some(url.to_owned()),
                            sha: Some(sha.to_string()),
                            ..Evidence::default()
                        },
                    );
                    return true;
                }
                self.discovery_warnings
                    .push(format!("{target}: {url} has no valid head commit"));
            }
            Some(Err(error)) => self
                .discovery_warnings
                .push(format!("{target}: {url}: {error}")),
            Some(Ok(_)) | None => {}
        }
        false
    }

    fn merges(&mut self) {
        for tracked in self.tracks_at(&Phase::PrOpen) {
            let target = tracked.name();
            let Some(url) = tracked.track.evidence.pr.clone() else {
                continue;
            };
            let Some(read) = self.pull(&url, Some(&target)) else {
                continue;
            };
            let PullRead {
                pull,
                mut etags,
                cached_at,
            } = match read {
                Ok(read) => read,
                Err(error) => {
                    self.unknown(&target, format!("{url}: {error}"));
                    continue;
                }
            };
            if let Some(at) = &cached_at {
                let Some(seen) = self.seen.get(&url).filter(|seen| seen.target == target) else {
                    continue;
                };
                if seen.at != *at
                    || seen.head.as_deref() != pull.head.as_ref().map(CommitSha::as_str)
                    || seen.base.as_deref() != pull.base.as_ref().map(CommitSha::as_str)
                    || seen.mergeable_state != pull.mergeable_state
                {
                    // Another sync published a full read while this read was in flight.
                    self.open.insert(url, seen.clone());
                    continue;
                }
            }
            if let Some(head) = &pull.head {
                self.heads.insert(target.clone(), head.to_string());
            }
            match pull {
                Pull {
                    state: PullState::Merged,
                    merge_commit: Some(sha),
                    merged_at,
                    head,
                    base,
                    ..
                } => {
                    if let Some(used) = self.gate_holds(
                        &tracked.target,
                        tracked.part.as_deref(),
                        &tracked.step,
                        &url,
                        head.as_ref(),
                        base.as_ref(),
                    ) {
                        let moves = self.moves.len();
                        self.advance(
                            &tracked.target,
                            tracked.part.as_deref(),
                            Phase::Merged,
                            Evidence {
                                sha: Some(sha.to_string()),
                                merged_at,
                                ..Evidence::default()
                            },
                        );
                        // An exception explains a merge; a refused advance has none to explain.
                        if self.moves.len() > moves {
                            self.notes.extend(
                                used.into_iter()
                                    .map(|line| (String::new(), format!("{line} ({target})"))),
                            );
                        }
                    }
                }
                Pull {
                    state: PullState::Merged,
                    merge_commit: None,
                    ..
                } => self.unknown(&target, format!("{url} is merged but has no merge commit")),
                Pull {
                    state: PullState::Closed,
                    ..
                } => self.violations.push(Violation::new(
                    &target,
                    "pr-closed",
                    format!(
                        "{url} closed unmerged; reopen it, or `cm advance {target} blocked:<reason>`"
                    ),
                )),
                Pull {
                    state: PullState::Open,
                    head,
                    mergeable_state,
                    base,
                    ..
                } => {
                    let previous = self.seen.get(&url).filter(|seen| seen.target == target);
                    let mut ci_head = previous.and_then(|seen| seen.ci_head.clone());
                    let mut ci_base = previous.and_then(|seen| seen.ci_base.clone());
                    let mut ci_green = previous.and_then(|seen| seen.ci_green);
                    // GitHub answers `unknown` while it computes mergeability: keep what was
                    // stored for the same head and base rather than write it twice.
                    let mergeable_state = match (&mergeable_state, previous) {
                        (Some(MergeableState::Other(state)), Some(seen))
                            if state == "unknown"
                                && seen.head.as_deref() == head.as_ref().map(CommitSha::as_str)
                                && seen.base.as_deref() == base.as_ref().map(CommitSha::as_str) =>
                        {
                            seen.mergeable_state.clone()
                        }
                        _ => mergeable_state,
                    };
                    // Checks are read on every sync, conditionally: a check registered later or a
                    // rerun at the same head changes what `ci_green` says about it. A check
                    // page's ETag belongs to its own head's URL: a 304 says that page is
                    // unchanged whatever `.cm/pulls.json` holds.
                    // Finished CI at the same head is re-read with each full PR read (at least
                    // every 30 minutes), so a quiet sync that GitHub answered 304 reads nothing;
                    // CI not known green (pending, or a failed read) is read on every sync.
                    if let (Some(head), Some(base), Some(repo)) = (&head, &base, GithubRepo::of_pr(&url))
                        && (ci_head.as_deref() != Some(head.as_str())
                            || cached_at.is_none()
                            || ci_green.is_none())
                    {
                        let validators = previous
                            .filter(|seen| seen.head.as_deref() == Some(head.as_str()))
                            .and_then(|_| self.last_read.get(&url))
                            .map(|read| read.etags.clone())
                            .unwrap_or_default();
                        let read = self.open_checks(&repo, head, &validators);
                        if matches!(read, Some(Err(_))) {
                            // An unread CI is unknown, never the last green.
                            ci_green = None;
                        }
                        if let Some(Ok(checks)) = read {
                            if checks.unchanged {
                                for (page, tag) in validators {
                                    etags.entry(page).or_insert(tag);
                                }
                            } else {
                                etags.extend(checks.etags.clone());
                                if checks.completed {
                                    // CI ran against the base it saw then: a re-read at the same
                                    // head keeps that base, so a moved base stays visible.
                                    if ci_head.as_deref() != Some(head.as_str()) {
                                        ci_head = Some(head.to_string());
                                        ci_base = Some(base.to_string());
                                    }
                                    ci_green = Some(checks.green);
                                } else {
                                    ci_green = None;
                                }
                            }
                        }
                    }
                    // A 304 keeps the time of the full read it validated.
                    let read_at = match &cached_at {
                        None => Some(self.at.clone()),
                        Some(_) => self.last_read.get(&url).map(|read| read.at.clone()),
                    };
                    let observed = plan::Observed {
                        base: base.map(|base| base.to_string()),
                        ci_head,
                        ci_base,
                        ci_green,
                        target: target.clone(),
                        at: cached_at.unwrap_or_else(|| self.at.clone()),
                        head: head.map(|head| head.to_string()),
                        mergeable_state,
                    };
                    // A read that saw no new fact keeps the stored observation and its stamp, so
                    // `.cm/pulls.json` changes only with a fact.
                    let observed = self
                        .seen
                        .get(&url)
                        .filter(|seen| same_facts(seen, &observed))
                        .cloned()
                        .unwrap_or(observed);
                    if let Some(at) = read_at {
                        let read = LastRead {
                            at,
                            head: observed.head.clone(),
                            base: observed.base.clone(),
                            mergeable_state: observed.mergeable_state.clone(),
                            etags,
                        };
                        self.read_now.insert(url.clone(), read);
                    }
                    self.open.insert(url.clone(), observed);
                }
            }
        }
    }

    fn tags(&mut self) {
        for (target, step) in self.steps_at(&Phase::Merged) {
            let Some(release) = Release::of_target(&target) else {
                continue;
            };
            self.unread_publications.insert(target.clone());
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
                    Some(Ok(true)) => {
                        let key = (
                            step.artifacts.iter().map(ToString::to_string).collect(),
                            repo.clone(),
                            tag.clone(),
                            release.version,
                        );
                        let (artifacts, shipped, tagged, version) = (
                            step.artifacts.clone(),
                            repo.clone(),
                            tag.clone(),
                            release.version,
                        );
                        let Some(Ok(read)) = self.read(
                            "publication",
                            |reads| &mut reads.publications,
                            key,
                            move |forge| {
                                Ok(crate::forge::publication(
                                    Some(forge),
                                    &artifacts,
                                    Some(&shipped),
                                    &version,
                                    &tagged,
                                ))
                            },
                        ) else {
                            continue;
                        };
                        self.unread_publications.remove(&target);
                        let present = read.missing.is_empty() && read.unknown.is_empty();
                        for message in read.missing {
                            self.violations.push(Violation::new(
                                &target,
                                "release-unpublished",
                                message,
                            ));
                        }
                        for message in read.unknown {
                            let note = format!("- unknown {target}: {message}");
                            self.unreadable.insert(note.clone());
                            self.notes.push((String::new(), note));
                            self.unknown(&target, message);
                        }
                        if present {
                            self.advance(
                                &target,
                                None,
                                Phase::Released,
                                Evidence {
                                    tag: Some(tag.to_string()),
                                    published: read.published,
                                    ..Evidence::default()
                                },
                            );
                        }
                    }
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

    fn releases(units: &Units, at_least: &Phase) -> Vec<Shipped> {
        let mut out: Vec<Shipped> = units
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
        let releases = Self::releases(&self.units, &Phase::Released);
        'steps: for tracked in self.tracks_at(&Phase::Merged) {
            let target = tracked.name();
            if tracked.step.work == Work::Rollout || Release::of_target(&tracked.target).is_some() {
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
                    // A planning walk asks for every tag at once rather than one per round.
                    None if self.planning() => {}
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
                "its release deployed, but it stays released until watch {} resolves with evidence or is withdrawn with a reason",
                held.join(", ")
            ),
        ));
        true
    }

    fn deployed(&mut self) {
        let released = Self::releases(&self.units, &Phase::Released);
        let deployed = Self::releases(&self.units, &Phase::Deployed);
        for tracked in self.tracks_at(&Phase::Released) {
            let target = tracked.name();
            if tracked.step.work == Work::Rollout || Release::of_target(&tracked.target).is_some() {
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
                    None if self.planning() => {}
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

// EPOCH.md's `epoch` slug, else a fixed name: a checkout under another folder name renders the
// same card.
fn card_name(root: &Path) -> String {
    let declared = std::fs::read_to_string(root.join(crate::epoch::EPOCH))
        .ok()
        .and_then(|text| {
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            let front = crate::epoch::front(&lines)?;
            crate::epoch::value(&lines, &front, "epoch")
                .filter(|slug| !slug.is_empty())
                .map(str::to_owned)
        });
    declared.unwrap_or_else(|| "campaign".to_owned())
}

fn guard(root: &Path) -> Result<Vec<Violation>> {
    let campaign = store::load_campaign(root)?;
    let mut guard = journal::check(root, &campaign);
    guard.extend(closed(root, &campaign));
    Ok(guard)
}

// The caller holds the campaign lock. This renders state without advancing any steps.
pub(crate) fn render_resume(root: &Path, units: &Units, closed: bool) -> Result<()> {
    let resume_path = root.join("RESUME.md");
    match std::fs::read_to_string(&resume_path) {
        Ok(mut resume) => {
            if let Ok(Some(span)) = crate::templates::status_span(&resume) {
                let epoch = if closed {
                    Some(crate::epoch::Status::Closed)
                } else {
                    match std::fs::read_to_string(root.join(crate::epoch::EPOCH)) {
                        Ok(text) => crate::epoch::status(&text),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                        Err(error) => return Err(error.into()),
                    }
                };
                let releases = |phase| {
                    Run::releases(units, phase)
                        .into_iter()
                        .map(|shipped| shipped.release)
                        .collect::<Vec<_>>()
                };
                let status = render::status_lines(
                    units,
                    &releases(&Phase::Released),
                    &releases(&Phase::Deployed),
                    epoch,
                );
                if resume[span.clone()] != status {
                    resume.replace_range(span, &status);
                    write_atomic(&resume_path, resume.as_bytes())?;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

// Planning walks change nothing on disk; a read they miss, the read walk makes itself.
fn prefetch(
    root: &Path,
    forge: &(dyn Forge + Sync),
    now: jiff::Timestamp,
    units: &Units,
    owner_unread: &[String],
) -> Reads {
    let mut reads = Reads::default();
    for _ in 0..PLAN_ROUNDS {
        let mut plan = Run::new(
            root,
            Access::Plan,
            reads,
            now,
            units.clone(),
            owner_unread.to_vec(),
        );
        plan.walk();
        reads = std::mem::take(&mut plan.reads);
        let jobs = std::mem::take(&mut plan.wanted);
        if jobs.is_empty() {
            break;
        }
        for store in perform(jobs, forge) {
            store(&mut reads);
        }
    }
    reads
}

fn perform(jobs: Vec<Job>, forge: &(dyn Forge + Sync)) -> Vec<Store> {
    let readers = READERS.min(jobs.len());
    let queue = Mutex::new(jobs.into_iter());
    let done = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..readers {
            scope.spawn(|| {
                loop {
                    let Some(job) = queue.lock().expect("read queue").next() else {
                        break;
                    };
                    let store = job(forge);
                    done.lock().expect("read results").push(store);
                }
            });
        }
    });
    done.into_inner().expect("read results")
}

pub fn sync(host: &dyn Host, forge: &dyn Forge, root: &Path) -> Result<Outcome> {
    sync_with(host, Reader::Serial(forge), root, BUDGET)
}

pub fn sync_with(
    host: &dyn Host,
    reader: Reader<'_>,
    root: &Path,
    budget: Duration,
) -> Result<Outcome> {
    let mut phases = Phases::new();
    let result = run_sync(host, reader, root, &mut phases);
    if result.is_err() {
        phases.fail();
    }
    phases.close();
    let total = phases.start.elapsed();
    match result {
        Ok(mut outcome) => {
            if !outcome.ok() || total > budget {
                for line in phases.lines(total, budget) {
                    outcome.line(line);
                }
                outcome.field("timing", phases.json(total, budget));
            }
            Ok(outcome)
        }
        Err(error) => {
            for line in phases.lines(total, budget) {
                eprintln!("{line}");
            }
            Err(error)
        }
    }
}

fn run_sync(
    host: &dyn Host,
    reader: Reader<'_>,
    root: &Path,
    phases: &mut Phases,
) -> Result<Outcome> {
    phases.enter("guard");
    let found = guard(root)?;
    if !found.is_empty() {
        return Ok(Outcome::refused(found));
    }
    phases.enter("load");
    let now = host.now();
    let units = plan::load_units(root)?;
    let owner_unread = crate::ledger::unread_owner_queue(root)?;
    phases.enter("read");
    let (forge, reads, at_once): (&dyn Forge, _, _) = match reader {
        Reader::Serial(forge) => (forge, Reads::default(), 1),
        Reader::Concurrent(forge) => (
            forge,
            prefetch(root, forge, now, &units, &owner_unread),
            READERS,
        ),
    };
    let mut read = Run::new(root, Access::Read(forge), reads, now, units, owner_unread);
    read.walk();
    // Sampled after the reads, so `used` counts this sync's own.
    let rate_limit = forge.rate_limit();
    let summary = read.reads.summary();
    phases.note(format!("{summary}; up to {at_once} at once"));
    phases.enter("lock");
    let _lock = lock(root)?;
    phases.enter("apply");
    let found = guard(root)?;
    if !found.is_empty() {
        return Ok(Outcome::refused(found));
    }
    let mut run = Run::new(
        root,
        Access::Cached,
        read.reads,
        now,
        plan::load_units(root)?,
        crate::ledger::unread_owner_queue(root)?,
    );
    run.walk();
    phases.enter("write");
    for name in &run.dirty {
        write_unit(&root.join(name), &run.units[name])?;
    }
    let mut open = std::mem::take(&mut run.open);
    // Keep the previous observation when GitHub could not read a still-open PR.
    for (url, observed) in std::mem::take(&mut run.seen) {
        let failed = !matches!(run.reads.pulls.get(&url), Some(Ok(_)));
        let current = plan::track(&run.units, &observed.target).is_some_and(|track| {
            track.state == Phase::PrOpen && track.evidence.pr.as_deref() == Some(url.as_str())
        });
        if failed && current {
            // The PR went unread, so its CI is unknown now: keep the rest, never the green.
            open.entry(url).or_insert(plan::Observed {
                ci_green: None,
                ..observed
            });
        }
    }
    let mut last_read = std::mem::take(&mut run.read_now);
    for url in open.keys() {
        if let Some(kept) = run
            .last_read
            .get(url)
            .filter(|_| !last_read.contains_key(url))
        {
            last_read.insert(url.clone(), kept.clone());
        }
    }
    last_read.retain(|url, _| open.contains_key(url));
    let mut campaign = store::load_campaign(root)?;
    campaign.schema = campaign.schema.max(crate::state::SCHEMA);
    store::write_json(&store::state_path(root), &campaign)?;
    // The observations before the memo of their validators: a sync stopped between the two
    // leaves PR validators that no longer match, which `LastRead::validates` refuses.
    store::write_json(&plan::pulls_path(root), &open)?;
    let last_read_path = root.join(store::CM_DIR).join(PULLS_READ);
    if !last_read.is_empty() || last_read_path.exists() {
        store::ignore(root, PULLS_READ)?;
        store::write_json(&last_read_path, &last_read)?;
    }
    phases.enter("journal");
    let mut outcome = Outcome::refused(std::mem::take(&mut run.violations));
    crate::ledger::owner_queue_banner(root, &mut outcome)?;
    if !run.turn_warnings.is_empty() {
        for warning in &run.turn_warnings {
            outcome.line(format!("remote turn: {warning}"));
        }
        outcome.field("turn_reader_warnings", json!(run.turn_warnings));
    }
    if !run.discovery_warnings.is_empty() {
        for warning in &run.discovery_warnings {
            outcome.line(format!("PR discovery: {warning}"));
        }
        outcome.field("pr_discovery_warnings", json!(run.discovery_warnings));
    }
    let marker_path = root.join(store::CM_DIR).join("unblock-notified.json");
    let mut notified: BTreeSet<String> = if marker_path.exists() {
        store::read_json(&marker_path)?
    } else {
        BTreeSet::new()
    };
    let mut changed = false;
    for item in plan::unblock_now(root, &run.units, None) {
        let key = serde_json::to_string(&json!({
            "target": item["target"],
            "condition": item["condition"],
            "since": item["since"],
        }))?;
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
    // A publication already journaled as unreadable, and unreadable since, is not news.
    let unreadable_path = root.join(store::CM_DIR).join(UNREADABLE_NOTIFIED);
    let published: BTreeSet<String> = store::read_json(&unreadable_path).unwrap_or_default();
    let news = run
        .notes
        .iter()
        .any(|(_, note)| !run.unreadable.contains(note) || !published.contains(note));
    for (_, note) in &run.notes {
        outcome.line(note.trim_start_matches("- ").to_owned());
    }
    if !run.moves.is_empty() || news {
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
        }
        let title = format!("cm sync: {} move(s) from GitHub", run.moves.len());
        journal::append(root, &journal::entry(&run.at, &title, Some(&body)))?;
        outcome.field("journal", journal::heading(&run.at, &title));
    }
    if changed {
        store::write_json(&marker_path, &notified)?;
    }
    // A merged release whose publication went unread this sync, whatever stopped the read, keeps
    // the notes last published for it; one no longer merged keeps none.
    let standing: BTreeSet<String> = published
        .iter()
        .filter(|note| {
            run.unread_publications
                .iter()
                .any(|target| note.starts_with(&format!("- unknown {target}: ")))
        })
        .chain(&run.unreadable)
        .cloned()
        .collect();
    if standing != published && (!standing.is_empty() || unreadable_path.exists()) {
        store::write_json(&unreadable_path, &standing)?;
    }
    phases.enter("render");
    let next = plan::summary(root, &run.units);
    let card = render::card(root, run.now, &card_name(root), &run.units, &next);
    let wrote_card = store::write_changed(&root.join(CARD), card.as_bytes())?;
    render_resume(root, &run.units, false)?;
    phases.enter("report");
    for moved in &run.moves {
        outcome.line(format!(
            "{}: {} → {} ({})",
            moved.target,
            moved.from,
            moved.to,
            moved.evidence.summary()
        ));
    }
    let inputs = plan::ActionInputs::read(root, now);
    let stalled = plan::stalls(&run.units, &inputs);
    let dirty = plan::dirty(root, &run.units);
    for stall in &stalled {
        outcome.line(plan::stall_line(stall));
    }
    for (url, observed) in &dirty {
        outcome.line(plan::dirty_line(url, observed));
    }
    outcome.field(
        "stalled",
        stalled.iter().map(plan::stall_json).collect::<Vec<_>>(),
    );
    outcome.field(
        "dirty",
        dirty
            .iter()
            .map(|(url, observed)| plan::dirty_json(url, observed))
            .collect::<Vec<_>>(),
    );
    if let Some(sample) = rate_limit {
        outcome.line(sample.clone());
        outcome.field("rate_limit", sample);
    }
    outcome.line(format!(
        "synced at {}: {} move(s); {}",
        run.at,
        run.moves.len(),
        if wrote_card {
            format!("wrote {CARD}")
        } else {
            format!("{CARD} unchanged")
        }
    ));
    outcome.field("at", run.at.clone());
    outcome.field("card", CARD);
    outcome.field("card_written", wrote_card);
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
