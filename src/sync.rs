use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::commands::{closed, write_unit};
use crate::forge::{Forge, ForgeError, Pull, PullState};
use crate::host::{Host, stamp};
use crate::journal;
use crate::outcome::{Outcome, Violation};
use crate::plan::{self, Units, repo_of_pr};
use crate::render;
use crate::state::{Evidence, Move, Phase, Release};
use crate::store::{self, lock, write_atomic};

pub const CARD: &str = "CARD.md";

pub struct Moved {
    pub target: String,
    pub from: Phase,
    pub to: Phase,
    pub evidence: Evidence,
}

struct Run<'a> {
    forge: &'a dyn Forge,
    at: String,
    units: Units,
    dirty: BTreeSet<String>,
    moves: Vec<Moved>,
    violations: Vec<Violation>,
    pulls: BTreeMap<String, Result<Pull, ForgeError>>,
    contains: BTreeMap<(String, String, String), Result<bool, ForgeError>>,
}

impl Run<'_> {
    fn steps_at(&self, phase: &Phase) -> Vec<(String, Evidence)> {
        self.units
            .iter()
            .flat_map(|(unit, state)| {
                state
                    .steps
                    .iter()
                    .filter(|(_, step)| step.track.state == *phase)
                    .map(move |(name, step)| {
                        (format!("{unit}/{name}"), step.track.evidence.clone())
                    })
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
        if step.track.check(&to).is_err() {
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

    fn pull(&mut self, url: &str) -> Result<Pull, ForgeError> {
        let forge = self.forge;
        self.pulls
            .entry(url.to_owned())
            .or_insert_with(|| forge.pull(url))
            .clone()
    }

    fn contains(&mut self, repo: &str, tag: &str, sha: &str) -> Result<bool, ForgeError> {
        let forge = self.forge;
        self.contains
            .entry((repo.to_owned(), tag.to_owned(), sha.to_owned()))
            .or_insert_with(|| forge.tag_contains(repo, tag, sha))
            .clone()
    }

    fn merges(&mut self) {
        for (target, evidence) in self.steps_at(&Phase::PrOpen) {
            let Some(url) = evidence.pr else {
                continue;
            };
            match self.pull(&url) {
                Ok(Pull {
                    state: PullState::Merged,
                    merge_commit: Some(sha),
                }) => self.advance(
                    &target,
                    Phase::Merged,
                    Evidence {
                        sha: Some(sha),
                        ..Evidence::default()
                    },
                ),
                Ok(Pull {
                    state: PullState::Merged,
                    merge_commit: None,
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
                    ..
                }) => {}
                Err(error) => self.unknown(&target, format!("{url}: {error}")),
            }
        }
    }

    fn tags(&mut self) {
        for (target, evidence) in self.steps_at(&Phase::Merged) {
            let Some(release) = Release::of_target(&target) else {
                continue;
            };
            let (Some(repo), Some(sha)) =
                (evidence.pr.as_deref().and_then(repo_of_pr), evidence.sha)
            else {
                self.unknown(
                    &target,
                    "a merged release step needs its prep PR and merge commit to find its tag"
                        .to_owned(),
                );
                continue;
            };
            for tag in release.tag_names() {
                match self.forge.tag_exists(&repo, &tag) {
                    Ok(false) => continue,
                    Ok(true) => match self.contains(&repo, &tag, &sha) {
                        Ok(true) => self.advance(
                            &target,
                            Phase::Released,
                            Evidence {
                                tag: Some(tag),
                                ..Evidence::default()
                            },
                        ),
                        Ok(false) => self.violations.push(Violation::new(
                            &target,
                            "release-tag",
                            format!("{repo} tag {tag} does not contain the prep merge {sha}"),
                        )),
                        Err(error) => self.unknown(&target, format!("{repo} {tag}: {error}")),
                    },
                    Err(error) => self.unknown(&target, format!("{repo} {tag}: {error}")),
                }
                break;
            }
        }
    }

    fn releases(&self, at_least: &Phase) -> Vec<(Release, String, Evidence)> {
        let mut out: Vec<(Release, String, Evidence)> = self
            .units
            .iter()
            .flat_map(|(unit, state)| {
                state
                    .steps
                    .iter()
                    .map(move |(name, step)| (format!("{unit}/{name}"), &step.track))
            })
            .filter(|(_, track)| {
                matches!((track.state.rank(), at_least.rank()), (Some(at), Some(needed)) if at >= needed)
            })
            .filter_map(|(target, track)| {
                let release = Release::of_target(&target)?;
                let repo = track.evidence.pr.as_deref().and_then(repo_of_pr)?;
                track.evidence.tag.as_ref()?;
                Some((release, repo, track.evidence.clone()))
            })
            .collect();
        out.sort_by_key(|(release, _, _)| release.version);
        out
    }

    fn released(&mut self) {
        let releases = self.releases(&Phase::Released);
        for (target, evidence) in self.steps_at(&Phase::Merged) {
            if Release::of_target(&target).is_some() {
                continue;
            }
            let (Some(repo), Some(sha)) =
                (evidence.pr.as_deref().and_then(repo_of_pr), evidence.sha)
            else {
                continue;
            };
            for (_, release_repo, release) in releases.iter().filter(|(_, r, _)| *r == repo) {
                let Some(tag) = &release.tag else {
                    continue;
                };
                match self.contains(release_repo, tag, &sha) {
                    Ok(true) => {
                        self.advance(
                            &target,
                            Phase::Released,
                            Evidence {
                                tag: Some(tag.clone()),
                                ..Evidence::default()
                            },
                        );
                        break;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        self.unknown(&target, format!("{repo} {tag}: {error}"));
                        break;
                    }
                }
            }
        }
    }

    fn deployed(&mut self) {
        let deployed = self.releases(&Phase::Deployed);
        for (target, evidence) in self.steps_at(&Phase::Released) {
            if Release::of_target(&target).is_some() {
                continue;
            }
            let repo = evidence.pr.as_deref().and_then(repo_of_pr);
            let found = deployed.iter().find(|(_, release_repo, release)| {
                Some(release_repo) == repo.as_ref() && release.tag == evidence.tag
            });
            if let Some((_, _, release)) = found
                && let Some(digest) = &release.digest
            {
                self.advance(
                    &target,
                    Phase::Deployed,
                    Evidence {
                        digest: Some(digest.clone()),
                        ..Evidence::default()
                    },
                );
            }
        }
    }
}

pub fn sync(host: &dyn Host, forge: &dyn Forge, root: &Path) -> Result<Outcome> {
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut guard = journal::check(root, &campaign);
    guard.extend(closed(root, &campaign));
    if !guard.is_empty() {
        return Ok(Outcome::refused(guard));
    }
    let mut run = Run {
        forge,
        at: stamp(host.now()),
        units: plan::load_units(root)?,
        dirty: BTreeSet::new(),
        moves: Vec::new(),
        violations: Vec::new(),
        pulls: BTreeMap::new(),
        contains: BTreeMap::new(),
    };
    run.merges();
    run.tags();
    run.released();
    run.deployed();
    for name in &run.dirty {
        write_unit(&root.join(name), &run.units[name])?;
    }
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
