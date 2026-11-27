use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cm::commands::{self, Declaration, MergeGate, Remote};
use cm::forge::{
    CheckState, CommitSha, Forge, ForgeError, Gh, GithubRepo, Pull, PullState, TagName, ci_green,
};
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::plan;
use cm::state::{Evidence, Lane, Phase};
use cm::store::{self, parse_target};
use cm::verdict::{self, CleanUpdateError, Lineage};
use jiff::Timestamp;
use sha2::{Digest, Sha256};

#[allow(dead_code)]
mod support;

#[path = "support/verdict.rs"]
mod review_fixture;

#[path = "support/readers.rs"]
mod readers;
use readers::both as sync;

impl readers::Twin for FakeForge {
    fn twin(&self, root: &Path) -> Self {
        FakeForge {
            campaign: self.campaign.as_ref().map(|_| root.to_path_buf()),
            ..self.clone()
        }
    }
}

impl readers::Twin for forge_fixture::FakeForge {
    fn twin(&self, root: &Path) -> Self {
        forge_fixture::FakeForge::twin(self, root)
    }
}

// For a `gh` script that keeps no state between calls.
impl readers::Twin for Gh {
    fn twin(&self, _: &Path) -> Self {
        Gh {
            program: self.program.clone(),
            timeout: self.timeout,
        }
    }
}

struct Fake {
    now: Timestamp,
}

impl Host for Fake {
    fn now(&self) -> Timestamp {
        self.now
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(100 << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(0)
    }
}

fn at(now: &str) -> Fake {
    Fake {
        now: now.parse().unwrap(),
    }
}

/// GitHub as one PR at `head` with `mergeable_state` and these checks on it; `down` fails every read.
#[derive(Clone)]
struct FakeForge {
    head: String,
    state: PullState,
    merged_at: Option<String>,
    mergeable: &'static str,
    checks: Vec<(&'static str, &'static str)>,
    down: bool,
    base: Option<String>,
    base_checks: Vec<(&'static str, &'static str)>,
    parts: std::collections::BTreeMap<String, FakeForge>,
    tags: std::collections::BTreeMap<(String, String), std::collections::BTreeSet<String>>,
    branches: std::collections::BTreeMap<(String, String), std::collections::BTreeSet<String>>,
    campaign: Option<PathBuf>,
}

impl FakeForge {
    fn assert_unlocked(&self) {
        if let Some(root) = &self.campaign {
            assert!(
                forge_fixture::lock_free(root),
                "forge read held the campaign lock"
            );
        }
    }

    fn open(head: &str, checks: &[(&'static str, &'static str)]) -> Self {
        FakeForge {
            head: head.to_owned(),
            state: PullState::Open,
            merged_at: None,
            mergeable: "clean",
            checks: checks.to_vec(),
            down: false,
            base: None,
            base_checks: Vec::new(),
            parts: Default::default(),
            tags: Default::default(),
            branches: Default::default(),
            campaign: None,
        }
    }
}

impl Forge for FakeForge {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError> {
        if let Some(part) = self.parts.get(url) {
            return part.pull(url);
        }
        if self.down {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(Pull {
            state: self.state,
            merged_at: self.merged_at.clone(),
            merge_commit: (self.state == PullState::Merged)
                .then(|| CommitSha::new(self.head.clone())),
            head: Some(CommitSha::new(self.head.clone())),
            head_ref: None,
            base: self.base.clone().map(CommitSha::new),
            mergeable_state: Some(self.mergeable.to_owned().into()),
        })
    }

    fn checks(&self, _repo: &GithubRepo, sha: &CommitSha) -> Result<Vec<CheckState>, ForgeError> {
        self.assert_unlocked();
        if let Some(part) = self.parts.values().find(|part| part.head == sha.as_str()) {
            return part.checks(_repo, sha);
        }
        if self.down {
            return Err(ForgeError::Exit(Some(1)));
        }
        let checks = if self.base.as_deref() == Some(sha.as_str()) {
            &self.base_checks
        } else {
            assert_eq!(
                sha.as_str(),
                self.head,
                "CI is read at the PR head or its base"
            );
            &self.checks
        };
        Ok(checks
            .iter()
            .map(|(name, state)| CheckState {
                name: (*name).to_owned(),
                state: (*state).to_owned().into(),
            })
            .collect())
    }

    fn latest_completed_checks(
        &self,
        _: &GithubRepo,
        _: &CommitSha,
        names: &[String],
    ) -> Result<Vec<CheckState>, ForgeError> {
        if self.down {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(self
            .base_checks
            .iter()
            .filter(|(name, state)| {
                names.iter().any(|wanted| wanted == name)
                    && !["pending", "in_progress", "queued"].contains(state)
            })
            .map(|(name, state)| CheckState {
                name: (*name).into(),
                state: (*state).to_owned().into(),
            })
            .collect())
    }

    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError> {
        self.assert_unlocked();
        if self.down {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(self.tags.contains_key(&(repo.to_string(), tag.to_string())))
    }

    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError> {
        self.assert_unlocked();
        if self.down {
            return Err(ForgeError::Exit(Some(1)));
        }
        let key = (repo.to_string(), tag.to_string());
        Ok(self
            .tags
            .get(&key)
            .or_else(|| self.branches.get(&key))
            .is_some_and(|shas| shas.contains(sha.as_str())))
    }
}

/// `sha256sum`'s hex digest of `path`, or of nothing when it does not exist.
fn sha256(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap_or_default())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn rules(outcome: &Outcome) -> Vec<&'static str> {
    let mut rules: Vec<_> = outcome.violations.iter().map(|v| v.rule).collect();
    rules.sort_unstable();
    rules
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=cm", "-c", "user.email=cm@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

const STEP: &str = "01-a/u/S1";
const PR: &str = "https://github.com/o/r/pull/7";

struct Campaign {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

/// A campaign with the codex step S1 launched remotely and, with `pr`, at pr-open on PR 7.
fn campaign(host: &Fake, pr: bool) -> Campaign {
    let setup = Setup {
        pr,
        worktree: false,
        lane: Lane::Codex,
        home: None,
    };
    campaign_in(host, tempfile::tempdir().unwrap(), setup)
}

#[derive(Clone, Copy)]
struct Setup<'a> {
    pr: bool,
    worktree: bool,
    lane: Lane,
    home: Option<&'a Path>,
}

fn campaign_in(host: &Fake, temp: tempfile::TempDir, setup: Setup) -> Campaign {
    let run = |repo: &Path, args: &[&str]| match setup.home {
        Some(home) => {
            let output = scratch_git(home, repo, args);
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }
        None => git(repo, args),
    };
    let root = temp.path().join("campaign");
    assert!(commands::init(host, &root).unwrap().ok());
    run(&root, &["init", "-q"]);
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    run(&repo, &["init", "-q"]);
    run(&repo, &["commit", "-q", "--allow-empty", "-m", "head"]);
    let declaration = Declaration {
        lane: Some(setup.lane),
        ..Declaration::default()
    };
    assert!(
        commands::declare(host, &root.join(STEP), &declaration)
            .unwrap()
            .ok()
    );
    fs::write(root.join(STEP).join("kickoff.md"), "Build the thing.\n").unwrap();
    let remote = Remote {
        no_parts: None,
        coordinator: Some("session_a".into()),
        cap_min: None,
    };
    let launched = commands::launch_remote(
        host,
        &root,
        &target(),
        setup.worktree.then_some(repo.as_path()),
        &remote,
    )
    .unwrap();
    assert!(launched.ok(), "{:?}", launched.violations);
    let c = Campaign {
        _temp: temp,
        root,
        repo,
    };
    if setup.pr {
        let opened = commands::advance(
            host,
            &c.root,
            &target(),
            &Phase::PrOpen,
            &Evidence {
                pr: Some(PR.into()),
                sha: Some(run(&c.repo, &["rev-parse", "HEAD"])),
                ..Evidence::default()
            },
            None,
            Some(&c.repo),
        )
        .unwrap();
        assert!(opened.ok(), "{:?}", opened.violations);
    }
    c
}

impl Campaign {
    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn report(&self, name: &str, head: &str, findings: &[&str], verdict: &str) -> PathBuf {
        let mut block = format!("head: {head}\n");
        for finding in findings {
            block.push_str(finding);
            block.push('\n');
        }
        // A report named for an Astra review dir is that review's report.md, beside its receipt.
        let rel = if name.starts_with("astra-review") {
            format!("{STEP}/evidence/{name}/report.md")
        } else {
            format!("drafts/{name}.md")
        };
        self.write(
            &rel,
            &format!("# Review\n\nProse.\n\n```cm-review\n{block}```\n\n{verdict}\n"),
        )
    }

    fn receipt(&self, dir: &str, head: &str, model: &str, effort: &str) {
        self.receipt_with(dir, head, model, effort, |_| {});
    }

    /// An `astra-review.sh collect` receipt for a review that happened, changed by `edit`; its
    /// report_sha256 is that of `<dir>/report.md` when one is written.
    fn receipt_with(
        &self,
        dir: &str,
        head: &str,
        model: &str,
        effort: &str,
        edit: impl FnOnce(&mut serde_json::Value),
    ) {
        let mut receipt = serde_json::json!({
            "repo": "o/r",
            "pr": 7,
            "head": head,
            "requested": {"model": "gpt-6-astra", "effort": "xhigh"},
            "effective": {"model": model, "effort": effort, "source": "harness thread/read"},
            "thread_fresh": {"forkedFromId": null, "parentThreadId": null, "turns_in_thread": 1},
            "turn": {"status": "completed", "error": null},
            "report_sha256": sha256(&self.root.join(format!("{STEP}/evidence/{dir}/report.md"))),
            "verdict": "ACCEPT",
        });
        edit(&mut receipt);
        self.write(
            &format!("{STEP}/evidence/{dir}/receipt.json"),
            &serde_json::to_string_pretty(&receipt).unwrap(),
        );
    }

    fn state(&self) -> Phase {
        store::load_unit(&self.root.join("01-a/u")).unwrap().steps["S1"]
            .track
            .state
            .clone()
    }

    fn merge(&self, host: &Fake, gate: &MergeGate) -> Outcome {
        commands::advance_gated(
            host,
            &self.root,
            &target(),
            commands::Advance {
                to: &Phase::Merged,
                given: &Evidence {
                    sha: Some(self.head()),
                    ..Evidence::default()
                },
                file: None,
                repo: Some(&self.repo),
                part: None,
                by: &[],
            },
            gate,
        )
        .unwrap()
    }
}

fn target() -> store::Target {
    parse_target(STEP, true).unwrap()
}

fn record(host: &Fake, c: &Campaign, report: &Path, reviewer: &str) -> Outcome {
    verdict::record(host, &c.root, &target(), report, Some(reviewer), None).unwrap()
}

#[test]
fn released_without_a_checkout_reads_tag_containment_from_the_forge() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let mut forge = FakeForge::open(&head, &[]);
    assert!(
        c.merge(
            &host,
            &MergeGate {
                forge: Some(&forge),
                waiver: Some(c.write("waiver.md", "Proceed with the merge.\n")),
                ..MergeGate::default()
            }
        )
        .ok()
    );
    let release = |forge: &FakeForge| {
        commands::advance_gated(
            &host,
            &c.root,
            &target(),
            commands::Advance {
                to: &Phase::Released,
                given: &Evidence {
                    tag: Some("v1.0.0".into()),
                    ..Evidence::default()
                },
                file: None,
                repo: None,
                part: None,
                by: &[],
            },
            &MergeGate {
                forge: Some(forge),
                ..MergeGate::default()
            },
        )
        .unwrap()
    };
    forge.campaign = Some(c.root.clone());
    forge
        .tags
        .insert(("o/r".into(), "v1.0.0".into()), Default::default());
    let before = sha256(&store::state_path(&c.root.join("01-a/u")));
    let journal = sha256(&c.root.join("JOURNAL.md"));
    assert_eq!(rules(&release(&forge)), ["tag-lineage"]);
    forge.down = true;
    assert_eq!(rules(&release(&forge)), ["evidence-tag"]);
    assert_eq!(sha256(&store::state_path(&c.root.join("01-a/u"))), before);
    assert_eq!(sha256(&c.root.join("JOURNAL.md")), journal);
    forge.down = false;
    forge
        .tags
        .insert(("o/r".into(), "v1.0.0".into()), [head].into());
    let released = release(&forge);
    assert!(released.ok(), "{:?}", released.violations);
    assert_eq!(c.state(), Phase::Released);
}

#[test]
fn released_without_a_checkout_refuses_a_containing_branch() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let mut forge = FakeForge::open(&head, &[]);
    assert!(
        c.merge(
            &host,
            &MergeGate {
                forge: Some(&forge),
                waiver: Some(c.write("waiver.md", "Proceed with the merge.\n")),
                ..MergeGate::default()
            }
        )
        .ok()
    );
    forge
        .branches
        .insert(("o/r".into(), "main".into()), [head].into());
    let before = sha256(&store::state_path(&c.root.join("01-a/u")));
    let journal = sha256(&c.root.join("JOURNAL.md"));
    let refused = commands::advance_gated(
        &host,
        &c.root,
        &target(),
        commands::Advance {
            to: &Phase::Released,
            given: &Evidence {
                tag: Some("main".into()),
                ..Evidence::default()
            },
            file: None,
            repo: None,
            part: None,
            by: &[],
        },
        &MergeGate {
            forge: Some(&forge),
            ..MergeGate::default()
        },
    )
    .unwrap();
    assert_eq!(rules(&refused), ["evidence-tag"]);
    assert_eq!(sha256(&store::state_path(&c.root.join("01-a/u"))), before);
    assert_eq!(sha256(&c.root.join("JOURNAL.md")), journal);
}

#[test]
fn a_tag_only_release_without_a_checkout_uses_its_declared_repository() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, false);
    let head = c.head();
    let step = parse_target("00-release/widget/1.0.0", false).unwrap();
    assert!(
        commands::declare(
            &host,
            &c.root.join("00-release/widget/1.0.0"),
            &Declaration {
                lane: Some(Lane::Coordinator),
                ships: commands::Ships {
                    github: Some("o/widget".parse().unwrap()),
                    ..commands::Ships::default()
                },
                ..Declaration::default()
            }
        )
        .unwrap()
        .ok()
    );
    let mut forge = FakeForge::open(&head, &[]);
    let release = |sha, forge: &FakeForge| {
        commands::advance_gated(
            &host,
            &c.root,
            &step,
            commands::Advance {
                to: &Phase::Released,
                given: &Evidence {
                    sha,
                    tag: Some("v1.0.0".into()),
                    ..Evidence::default()
                },
                file: None,
                repo: None,
                part: None,
                by: &[],
            },
            &MergeGate {
                forge: Some(forge),
                ..MergeGate::default()
            },
        )
        .unwrap()
    };
    forge
        .tags
        .insert(("o/widget".into(), "v1.0.0".into()), Default::default());
    assert_eq!(rules(&release(None, &forge)), ["evidence-missing"]);
    assert_eq!(rules(&release(Some(head.clone()), &forge)), ["tag-lineage"]);
    forge
        .tags
        .insert(("o/widget".into(), "v1.0.0".into()), [head.clone()].into());
    let released = release(Some(head.clone()), &forge);
    assert!(released.ok(), "{:?}", released.violations);
    assert_eq!(released.fields["evidence"]["sha"], head);
}

#[test]
fn record_computes_the_verdict_from_the_block() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let report = c.report(
        "r1",
        &head,
        &[
            "F1 | contract | introduced | src/lib.rs:40 | the refusal drops its reason",
            "F2 | contract | inherited | src/old.rs:3 | an old leak",
            "F3 | guideline | introduced | src/lib.rs:9-12 | a name the guidelines forbid",
        ],
        "FIX REQUIRED",
    );
    let recorded = record(&host, &c, &report, "astra");
    assert!(recorded.ok(), "{:?}", recorded.violations);
    assert_eq!(recorded.fields["verdict"], "FIX REQUIRED");
    assert_eq!(recorded.fields["blocking"], serde_json::json!(["R1.F1"]));
    assert_eq!(
        recorded.fields["followups"],
        serde_json::json!(["R1.F2", "R1.F3"])
    );
    let copy = c.root.join(recorded.fields["report"].as_str().unwrap());
    assert_eq!(copy, c.root.join(STEP).join("evidence/reviews/R1-astra.md"));
    assert_eq!(
        fs::read_to_string(&copy).unwrap(),
        fs::read_to_string(&report).unwrap()
    );
    let ledger = verdict::load(&c.root).unwrap();
    assert_eq!(ledger.reviews[0].pr.as_deref(), Some(PR));
    assert_eq!(
        rules(&record(&host, &c, &report, "claude")),
        vec!["review-duplicate"]
    );

    let inherited_only = c.report(
        "r2",
        &head,
        &["F1 | contract | inherited | src/old.rs:3 | an old leak"],
        "FIX REQUIRED",
    );
    let second = record(&host, &c, &inherited_only, "claude");
    assert_eq!(second.fields["verdict"], "ACCEPT");
    assert!(
        second
            .lines
            .iter()
            .any(|line| line.contains("the report says FIX REQUIRED")),
        "{:?}",
        second.lines
    );
    assert!(
        lint(&c.root).unwrap().ok(),
        "{:?}",
        lint(&c.root).unwrap().violations
    );
}

#[test]
fn record_rejects_a_report_without_a_well_formed_block() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let bare = c.write("drafts/bare.md", "# Review\n\nLooks fine.\n\nACCEPT\n");
    assert_eq!(
        rules(&record(&host, &c, &bare, "astra")),
        vec!["review-block"]
    );
    let bad = c.report(
        "bad",
        "nope",
        &["F1 | contract | new | src/a.rs | x"],
        "ACCEPT",
    );
    let refused = record(&host, &c, &bad, "astra");
    assert_eq!(
        rules(&refused),
        vec!["review-block", "review-block", "review-block"],
        "{:?}",
        refused.violations
    );
    let missing = verdict::record(
        &host,
        &c.root,
        &target(),
        &c.root.join("nope.md"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(rules(&missing), vec!["review-report"]);
    assert!(verdict::load(&c.root).unwrap().reviews.is_empty());
    assert!(!c.root.join(STEP).join("evidence/reviews").exists());
}

#[test]
fn merge_gate_needs_astra_a_cleared_review_and_green_ci_at_the_head() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let green = FakeForge::open(
        &head,
        &[("fmt, clippy, test", "success"), ("lint", "skipped")],
    );
    let gate = MergeGate {
        forge: Some(&green),
        ..MergeGate::default()
    };
    let bare = c.merge(&host, &gate);
    assert_eq!(
        rules(&bare),
        vec!["merge-gate", "merge-gate"],
        "{:?}",
        bare.violations
    );

    c.receipt(
        "astra-review-7-old",
        "0000000000000000000000000000000000000000",
        "gpt-6-astra",
        "xhigh",
    );
    c.receipt("astra-review-7-sol", &head, "gpt-6-sol", "xhigh");
    let fix = c.report(
        "astra-review-7",
        &head,
        &["F1 | contract | exposed | src/lib.rs:40 | now reachable"],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &fix, "astra").ok());
    let refused = c.merge(&host, &gate);
    let messages: Vec<&str> = refused
        .violations
        .iter()
        .map(|v| v.message.as_str())
        .collect();
    assert_eq!(
        rules(&refused),
        vec!["merge-gate", "merge-gate"],
        "{messages:?}"
    );
    assert!(
        messages[0].contains("effective gpt-6-sol/xhigh"),
        "{messages:?}"
    );
    assert!(messages[1].contains("R1.F1 unresolved"), "{messages:?}");

    c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
    let ruling = c.write(
        &format!("{STEP}/evidence/coordinator-scope.md"),
        "- the edge is in scope\n",
    );
    let resolved = verdict::resolve(
        &host,
        &c.root,
        &target(),
        "F1",
        &ruling.to_string_lossy(),
        &ruling,
    )
    .unwrap();
    assert!(resolved.ok(), "{:?}", resolved.violations);
    assert_eq!(resolved.fields["id"], "R1.F1");
    let again = verdict::resolve(
        &host,
        &c.root,
        &target(),
        "R1.F1",
        "abcdef1",
        Path::new("abcdef1"),
    )
    .unwrap();
    assert_eq!(rules(&again), vec!["resolve-done"]);
    // Resolved findings are no ACCEPT, and a model lane's head needs Astra's and a Claude
    // reviewer's.
    assert!(
        c.merge(&host, &gate).violations[0]
            .message
            .contains("no astra or claude ACCEPT")
    );
    claude_accept(&host, &c, &head);
    astra_accept(&host, &c, &head);

    let red = FakeForge::open(&head, &[("fmt, clippy, test", "failure")]);
    let refused = c.merge(
        &host,
        &MergeGate {
            forge: Some(&red),
            ..MergeGate::default()
        },
    );
    assert_eq!(rules(&refused), vec!["merge-gate"]);
    assert!(
        refused.violations[0]
            .message
            .contains("fmt, clippy, test failure")
    );

    let merged = c.merge(&host, &gate);
    assert!(merged.ok(), "{:?}", merged.violations);
    assert_eq!(c.state(), Phase::Merged);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains(&format!("- CI green at {head} (2 check(s))")),
        "{journal}"
    );
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn merge_gate_refuses_unknown_ci_unless_verified_and_overrides_only_with_evidence() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let accept = c.report(
        "astra-review-7",
        &head,
        &["F1 | taste | introduced | - | a nit"],
        "ACCEPT",
    );
    c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
    assert!(record(&host, &c, &accept, "astra").ok());
    claude_accept(&host, &c, &head);
    let down = FakeForge {
        down: true,
        ..FakeForge::open(&head, &[])
    };
    let offline = MergeGate {
        forge: Some(&down),
        ..MergeGate::default()
    };
    let refused = c.merge(&host, &offline);
    assert_eq!(rules(&refused), vec!["merge-gate"]);
    assert!(
        refused.violations[0].message.contains("--ci-verified"),
        "{:?}",
        refused.violations
    );
    let empty = c.write("ci.md", "");
    let empty_override = c.merge(
        &host,
        &MergeGate {
            waiver: Some(empty.clone()),
            ..MergeGate::default()
        },
    );
    assert_eq!(rules(&empty_override), vec!["evidence-file"]);
    let verified = c.write("ci.md", "CI run 41 green at the head.\n");
    let merged = c.merge(
        &host,
        &MergeGate {
            forge: Some(&down),
            ci_verified: Some(verified),
            ..MergeGate::default()
        },
    );
    assert!(merged.ok(), "{:?}", merged.violations);
    assert!(
        fs::read_to_string(c.root.join("JOURNAL.md"))
            .unwrap()
            .contains("- CI verified by ci.md (cannot read")
    );

    let other = campaign(&host, true);
    let why = other.write("waiver.md", "Owner: merge without the review; outage.\n");
    let waived = other.merge(
        &host,
        &MergeGate {
            waiver: Some(why),
            ..MergeGate::default()
        },
    );
    assert!(waived.ok(), "{:?}", waived.violations);
    let journal = fs::read_to_string(other.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains("- merge gate overridden by waiver.md"),
        "{journal}"
    );
    assert!(
        journal.contains("- waived: no Astra receipt.json"),
        "{journal}"
    );
}

#[test]
fn a_pi_step_merges_without_the_gate_and_refuses_its_flags() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let unit = c.root.join("01-a/u");
    let mut state = store::load_unit(&unit).unwrap();
    state.steps.get_mut("S1").unwrap().lane = Some(Lane::Pi);
    commands::write_unit(&unit, &state).unwrap();
    let why = c.write("waiver.md", "x\n");
    let flagged = c.merge(
        &host,
        &MergeGate {
            waiver: Some(why),
            ..MergeGate::default()
        },
    );
    assert_eq!(rules(&flagged), vec!["evidence-unexpected"]);
    assert!(c.merge(&host, &MergeGate::default()).ok());
}

#[test]
fn three_fix_required_rounds_with_falling_counts_do_not_block_or_queue() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let queue = sha256(&c.root.join("OWNER-QUEUE.md"));
    for (round, count) in [3, 2, 1].into_iter().enumerate() {
        let report = c.report(
            &format!("r{round}"),
            &format!("{round:08x}"),
            &review_fixture::BLOCKING[..count],
            "FIX REQUIRED",
        );
        let recorded = record(&host, &c, &report, "astra");
        assert!(recorded.ok(), "{:?}", recorded.violations);
        assert_eq!(recorded.fields["rounds"], round + 1);
        assert_eq!(recorded.fields["converging"], true);
        assert_eq!(recorded.fields["blocked"], false);
        assert_eq!(recorded.fields["owner_queue"], serde_json::Value::Null);
    }
    assert_eq!(c.state(), Phase::PrOpen);
    assert_eq!(sha256(&c.root.join("OWNER-QUEUE.md")), queue);
}

#[test]
fn rising_counts_request_a_fresh_reader_until_one_is_recorded() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let first = c.report(
        "first",
        "aaaaaaa1",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &first, "astra").ok());
    let rise = c.report(
        "rise",
        "bbbbbbb2",
        &review_fixture::BLOCKING[..2],
        "FIX REQUIRED",
    );
    let recorded = record(&host, &c, &rise, "astra");
    assert_eq!(recorded.fields["converging"], false);
    assert_eq!(recorded.fields["blocked"], false);
    assert_eq!(recorded.fields["owner_queue"], serde_json::Value::Null);
    assert!(
        recorded
            .lines
            .iter()
            .any(|line| line == "converging: false")
    );
    let command = format!("cm review record {STEP} '<fresh-review-report>' --reviewer fresh-opus");
    for target in [None, Some(target())] {
        let next = plan::next_at(&c.root, target.as_ref(), None, host.now).unwrap();
        assert!(next.ok(), "{:?}", next.violations);
        assert_eq!(next.fields["fresh_readers"][0]["command"], command);
        assert_eq!(next.fields["fresh_readers"][0]["review"], "R2");
        assert!(next.lines.iter().any(|line| line
            == &format!("1. fresh reader {STEP}: independent Opus, high effort — `{command}`")));
    }
    // Another pass by the original reviewer does not substitute for an independent read.
    let falling = c.report(
        "falling",
        "ccccccc3",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &falling, "astra").ok());
    assert_eq!(
        plan::next(&c.root, None, None).unwrap().fields["fresh_readers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let fresh = c.report("fresh", "ddddddd4", &[], "ACCEPT");
    assert!(record(&host, &c, &fresh, "fresh-opus").ok());
    assert_eq!(
        plan::next(&c.root, None, None).unwrap().fields["fresh_readers"],
        serde_json::json!([])
    );
}

#[test]
fn a_pr_open_stale_pin_stays_visible_with_a_pending_fresh_review() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let stale = review_fixture::stale_pin(&c.root, STEP);
    for count in 1..=2 {
        let report = c.report(
            &format!("r{count}"),
            &format!("{count:08x}"),
            &review_fixture::BLOCKING[..count],
            "FIX REQUIRED",
        );
        assert!(record(&host, &c, &report, "astra").ok());
    }
    for target in [None, Some(target())] {
        let next = plan::next_at(&c.root, target.as_ref(), None, host.now).unwrap();
        assert_eq!(next.fields["steps"], serde_json::json!([]));
        assert_eq!(next.fields["fresh_readers"].as_array().unwrap().len(), 1);
        assert_eq!(next.fields["fresh_readers"][0]["target"], STEP);
        assert_eq!(
            next.fields["fresh_readers"][0]["command"],
            format!("cm review record {STEP} '<fresh-review-report>' --reviewer fresh-opus")
        );
        assert_eq!(next.fields["waiting"][0]["target"], STEP);
        assert_eq!(
            next.fields["waiting"][0]["waits_on"],
            serde_json::json!([stale])
        );
        assert_eq!(
            next.fields["waiting"][0]["command"],
            format!("cm next {STEP}")
        );
    }
    let units = plan::load_units(&c.root).unwrap();
    let card = cm::render::card(
        &c.root,
        host.now,
        "campaign",
        &units,
        &plan::summary(&c.root, &units),
    );
    assert!(
        card.lines()
            .any(|line| line == format!("1. `{STEP}` waits on {stale} — `cm next {STEP}`"))
    );
    let linted = lint(&c.root).unwrap();
    assert!(
        linted.fields["warnings"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(format!(
                "pin-unmet: {STEP}: {stale}; recheck with `cm next {STEP}`"
            )))
    );
}

#[test]
fn a_step_without_a_pr_still_requests_a_fresh_reader() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, false);
    for count in 1..=2 {
        let report = c.report(
            &format!("r{count}"),
            &format!("{count:08x}"),
            &review_fixture::BLOCKING[..count],
            "FIX REQUIRED",
        );
        assert!(record(&host, &c, &report, "astra").ok());
    }
    for target in [None, Some(target())] {
        let next = plan::next(&c.root, target.as_ref(), None).unwrap();
        assert!(next.ok(), "{:?}", next.violations);
        let requests = next.fields["fresh_readers"].as_array().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["target"], STEP);
        assert_eq!(requests[0]["pr"], serde_json::Value::Null);
        assert_eq!(
            requests[0]["command"],
            format!("cm review record {STEP} '<fresh-review-report>' --reviewer fresh-opus")
        );
    }
}

#[test]
fn a_failing_fresh_read_queues_the_owner_without_replacing_an_unrelated_block() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    for cycle in 0..2 {
        for count in 1..=2 {
            let report = c.report(
                &format!("normal{cycle}-{count}"),
                &format!("{cycle:07x}{count}"),
                &review_fixture::BLOCKING[..count],
                "FIX REQUIRED",
            );
            assert!(record(&host, &c, &report, "astra").ok());
        }
        if cycle == 0 {
            assert!(
                commands::advance(
                    &host,
                    &c.root,
                    &target(),
                    &Phase::Blocked("ci".into()),
                    &Evidence::default(),
                    None,
                    None
                )
                .unwrap()
                .ok()
            );
        }
        let before = store::load_unit(&c.root.join("01-a/u")).unwrap().steps["S1"]
            .track
            .clone();
        let report = c.report(
            &format!("fresh{cycle}"),
            &format!("{cycle:07x}2"),
            &review_fixture::BLOCKING,
            "FIX REQUIRED",
        );
        let recorded = record(&host, &c, &report, "fresh-opus");
        assert!(recorded.ok(), "{:?}", recorded.violations);
        assert_eq!(recorded.fields["converging"], false);
        assert_eq!(recorded.fields["blocked"], false);
        assert_eq!(
            recorded.fields["owner_queue"],
            if cycle == 0 {
                serde_json::json!(1)
            } else {
                serde_json::Value::Null
            }
        );
        let after = store::load_unit(&c.root.join("01-a/u")).unwrap().steps["S1"]
            .track
            .clone();
        assert_eq!(after, before);
        assert_eq!(after.state, Phase::Blocked("ci".into()));
        assert_eq!(after.blocked_from, Some(Phase::PrOpen));
    }
    let queue = fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap();
    let titles: Vec<_> = cm::markdown::headings(&queue)
        .into_iter()
        .filter(|heading| heading.level == 2)
        .map(|heading| heading.text)
        .collect();
    assert_eq!(titles, ["1. 01-a/u/S1: non-converging fresh reader"]);
    let evidence = c.write("ci-fixed.md", "CI is green.\n");
    assert!(
        commands::unblock(&host, &c.root, &target(), Some(&evidence))
            .unwrap()
            .ok()
    );
    assert_eq!(c.state(), Phase::PrOpen);
    assert_eq!(
        fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap(),
        queue
    );
    assert!(cm::templates::queue_open_in(&c.root).unwrap());
}

#[test]
fn a_fresh_read_without_a_pending_request_uses_the_previous_report() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let report = c.report(
        "normal",
        "aaaaaaa1",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &report, "astra").ok());
    assert_eq!(
        plan::next(&c.root, None, None).unwrap().fields["fresh_readers"],
        serde_json::json!([])
    );
    let fresh = c.report(
        "fresh",
        "bbbbbbb2",
        &review_fixture::BLOCKING[..2],
        "FIX REQUIRED",
    );
    let recorded = record(&host, &c, &fresh, "fresh-opus");
    assert!(recorded.ok(), "{:?}", recorded.violations);
    assert_eq!(recorded.fields["converging"], false);
    assert_eq!(recorded.fields["blocked"], true);
    assert_eq!(recorded.fields["owner_queue"], 1);
    let again = c.report(
        "fresh-again",
        "ccccccc3",
        &review_fixture::BLOCKING,
        "FIX REQUIRED",
    );
    let duplicate_escalation = record(&host, &c, &again, "fresh-opus");
    assert!(
        duplicate_escalation.ok(),
        "{:?}",
        duplicate_escalation.violations
    );
    assert_eq!(duplicate_escalation.fields["converging"], false);
    assert_eq!(
        duplicate_escalation.fields["owner_queue"],
        serde_json::Value::Null
    );
}

#[test]
fn fresh_read_baselines_use_the_head_maximum_in_either_recording_order() {
    let host = at("2026-10-25T06:00:00Z");
    for pending in [false, true] {
        for reverse in [false, true] {
            for fresh_count in [2, 3] {
                let c = campaign(&host, true);
                let older = c.report(
                    "older",
                    "eeeeeee0",
                    &review_fixture::BLOCKING,
                    "FIX REQUIRED",
                );
                assert!(record(&host, &c, &older, "other").ok());
                if pending {
                    for (reviewer, head) in [("astra", "aaaaaaa0"), ("claude", "bbbbbbb0")] {
                        let report = c.report(&format!("initial-{reviewer}"), head, &[], "ACCEPT");
                        assert!(record(&host, &c, &report, reviewer).ok());
                    }
                }
                let mut reports = [("astra", 1), ("claude", 2)];
                if reverse {
                    reports.reverse();
                }
                for (reviewer, count) in reports {
                    let report = c.report(
                        reviewer,
                        "ccccccc1",
                        &review_fixture::BLOCKING[..count],
                        "FIX REQUIRED",
                    );
                    assert!(record(&host, &c, &report, reviewer).ok());
                }
                assert_eq!(
                    plan::next(&c.root, None, None).unwrap().fields["fresh_readers"]
                        .as_array()
                        .unwrap()
                        .len(),
                    usize::from(pending)
                );
                let fresh = c.report(
                    "fresh",
                    "ddddddd2",
                    &review_fixture::BLOCKING[..fresh_count],
                    "FIX REQUIRED",
                );
                let recorded = record(&host, &c, &fresh, "fresh-opus");
                assert!(recorded.ok(), "{:?}", recorded.violations);
                let escalating = fresh_count > 2;
                assert_eq!(
                    recorded.fields["converging"], !escalating,
                    "pending={pending}, reverse={reverse}, fresh_count={fresh_count}"
                );
                assert_eq!(recorded.fields["blocked"], escalating);
                assert_eq!(
                    recorded.fields["owner_queue"],
                    if escalating {
                        serde_json::json!(1)
                    } else {
                        serde_json::Value::Null
                    }
                );
                assert_eq!(
                    c.state(),
                    if escalating {
                        Phase::Blocked("non-convergence".into())
                    } else {
                        Phase::PrOpen
                    }
                );
                assert_eq!(cm::templates::queue_open_in(&c.root).unwrap(), escalating);
            }
        }
    }
}

#[test]
fn a_fresh_read_without_a_previous_report_has_no_convergence_verdict() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let report = c.report(
        "fresh",
        "aaaaaaa1",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    let recorded = record(&host, &c, &report, "fresh-opus");
    assert!(recorded.ok(), "{:?}", recorded.violations);
    assert_eq!(recorded.fields["converging"], serde_json::Value::Null);
    assert_eq!(recorded.fields["blocked"], false);
    assert_eq!(recorded.fields["owner_queue"], serde_json::Value::Null);
}

#[test]
fn a_fresh_round_compares_with_its_trigger_and_each_block_queues_the_owner() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    for cycle in 0..2 {
        for count in 1..=2 {
            let report = c.report(
                &format!("normal{cycle}-{count}"),
                &format!("{cycle:07x}{count}"),
                &review_fixture::BLOCKING[..count],
                "FIX REQUIRED",
            );
            assert!(record(&host, &c, &report, "astra").ok());
        }
        let report = c.report(
            &format!("fresh{cycle}"),
            &format!("{cycle:07x}2"),
            &review_fixture::BLOCKING,
            "FIX REQUIRED",
        );
        let recorded = record(&host, &c, &report, "fresh-opus");
        assert!(recorded.ok(), "{:?}", recorded.violations);
        assert_eq!(recorded.fields["converging"], false);
        assert_eq!(recorded.fields["blocked"], true);
        assert_eq!(recorded.fields["owner_queue"], cycle + 1);
        assert_eq!(c.state(), Phase::Blocked("non-convergence".into()));
        if cycle == 0 {
            assert!(
                cm::ledger::owner_queue_answer(&host, &c.root, 1, "Continue with the fix.")
                    .unwrap()
                    .ok()
            );
            let evidence = c.write("decision.md", "Continue with the fix.\n");
            assert!(
                commands::unblock(&host, &c.root, &target(), Some(&evidence))
                    .unwrap()
                    .ok()
            );
        }
    }
    let queue = fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap();
    let titles: Vec<_> = cm::markdown::headings(&queue)
        .into_iter()
        .filter(|heading| heading.level == 2)
        .map(|heading| heading.text)
        .collect();
    assert_eq!(titles, ["2. 01-a/u/S1: non-converging fresh reader"]);
}

#[test]
fn a_refused_owner_queue_escalation_leaves_no_orphan_report() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let first = c.report(
        "first",
        "aaaaaaa1",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &first, "astra").ok());
    let rise = c.report(
        "rise",
        "bbbbbbb2",
        &review_fixture::BLOCKING[..2],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &rise, "astra").ok());
    let resume = fs::read_to_string(c.root.join("RESUME.md")).unwrap();
    fs::write(c.root.join("RESUME.md"), "Missing banner.\n").unwrap();
    let report = c.report(
        "fresh",
        "bbbbbbb2",
        &review_fixture::BLOCKING,
        "FIX REQUIRED",
    );
    let refused = record(&host, &c, &report, "fresh-opus");
    assert_eq!(rules(&refused), vec!["resume-banner"]);
    assert!(
        !c.root
            .join(STEP)
            .join("evidence/reviews/R3-fresh-opus.md")
            .exists()
    );
    fs::write(c.root.join("RESUME.md"), resume).unwrap();
    let retried = record(&host, &c, &report, "fresh-opus");
    assert!(retried.ok(), "{:?}", retried.violations);
    assert_eq!(retried.fields["owner_queue"], 1);
}

#[test]
fn counts_compare_the_same_reviewer_and_pr_and_include_accepted_rounds() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let first = c.report(
        "first",
        "aaaaaaa1",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &first, "astra").ok());
    let other = c.report(
        "other",
        "bbbbbbb2",
        &review_fixture::BLOCKING,
        "FIX REQUIRED",
    );
    assert_eq!(
        record(&host, &c, &other, "claude").fields["converging"],
        true
    );
    let equal = c.report(
        "equal",
        "ccccccc3",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert_eq!(
        record(&host, &c, &equal, "astra").fields["converging"],
        true
    );
    let accepted = c.report("accepted", "ddddddd4", &[], "ACCEPT");
    assert!(record(&host, &c, &accepted, "astra").ok());
    let returned = c.report(
        "returned",
        "eeeeeee5",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert_eq!(
        record(&host, &c, &returned, "astra").fields["converging"],
        false
    );
    let mut unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    unit.steps.get_mut("S1").unwrap().track.evidence.pr =
        Some("https://github.com/o/r/pull/8".into());
    commands::write_unit(&c.root.join("01-a/u"), &unit).unwrap();
    let changed_pr = c.report(
        "changed-pr",
        "fffffff6",
        &review_fixture::BLOCKING,
        "FIX REQUIRED",
    );
    assert_eq!(
        record(&host, &c, &changed_pr, "astra").fields["converging"],
        true
    );
    assert_eq!(
        plan::next(&c.root, None, None).unwrap().fields["fresh_readers"],
        serde_json::json!([])
    );
}

#[test]
fn part_rounds_use_the_parts_pr_and_do_not_mix_counts() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let amendment = commands::Amendment {
        parts: vec!["-one".into(), "two".into()],
        ..Default::default()
    };
    assert!(
        commands::amend(&host, &c.root, &target(), &amendment)
            .unwrap()
            .ok()
    );
    let second_pr = "https://github.com/o/r/pull/8";
    assert!(
        commands::advance_gated(
            &host,
            &c.root,
            &target(),
            commands::Advance {
                to: &Phase::PrOpen,
                given: &Evidence {
                    pr: Some(second_pr.into()),
                    sha: Some(c.head()),
                    ..Default::default()
                },
                file: None,
                repo: Some(&c.repo),
                part: Some("two"),
                by: &[],
            },
            &MergeGate::default()
        )
        .unwrap()
        .ok()
    );
    let report = c.report(
        "first",
        "aaaaaaa1",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    let before = sha256(&c.root.join(".cm/verdicts.json"));
    assert_eq!(
        rules(&record(&host, &c, &report, "astra")),
        vec!["review-part"]
    );
    let unknown = verdict::record(
        &host,
        &c.root,
        &target(),
        &report,
        Some("astra"),
        Some("missing"),
    )
    .unwrap();
    assert_eq!(rules(&unknown), vec!["part-unknown"]);
    assert_eq!(sha256(&c.root.join(".cm/verdicts.json")), before);
    for (part, count, round, converging) in [
        ("-one", 1, 1, true),
        ("two", 3, 1, true),
        ("-one", 2, 2, false),
        ("two", 2, 2, true),
    ] {
        let report = c.report(
            &format!("{part}{round}"),
            &format!("{round:07x}{}", if part == "-one" { "a" } else { "b" }),
            &review_fixture::BLOCKING[..count],
            "FIX REQUIRED",
        );
        let recorded = verdict::record(
            &host,
            &c.root,
            &target(),
            &report,
            Some("astra"),
            Some(part),
        )
        .unwrap();
        assert!(recorded.ok(), "{:?}", recorded.violations);
        assert_eq!(recorded.fields["rounds"], round);
        assert_eq!(recorded.fields["converging"], converging);
    }
    let reviews = verdict::load(&c.root).unwrap().reviews;
    assert_eq!(reviews[0].part.as_deref(), Some("-one"));
    assert_eq!(reviews[0].pr.as_deref(), Some(PR));
    assert_eq!(reviews[1].part.as_deref(), Some("two"));
    assert_eq!(reviews[1].pr.as_deref(), Some(second_pr));
    let next = plan::next(&c.root, None, None).unwrap();
    assert_eq!(next.fields["fresh_readers"][0]["part"], "-one");
    assert_eq!(
        next.fields["fresh_readers"][0]["command"],
        format!(
            "cm review record {STEP} '<fresh-review-report>' --reviewer fresh-opus --part=-one"
        )
    );
    let duplicate = verdict::record(
        &host,
        &c.root,
        &target(),
        &c.root.join(reviews[0].report.as_str()),
        Some("astra"),
        Some("two"),
    )
    .unwrap();
    assert_eq!(rules(&duplicate), vec!["review-duplicate"]);
    let report = c.report("cli-part", "ccccccc3", &[], "ACCEPT");
    let command = next.fields["fresh_readers"][0]["command"]
        .as_str()
        .unwrap()
        .replacen(
            "cm ",
            &format!(
                "'{}' -C '{}' --json ",
                env!("CARGO_BIN_EXE_cm"),
                c.root.display()
            ),
            1,
        )
        .replace(
            "'<fresh-review-report>'",
            &format!("'{}'", report.display()),
        );
    let output = Command::new("sh").args(["-c", &command]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let saved: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(saved["part"], "-one");
    assert_eq!(
        verdict::load(&c.root)
            .unwrap()
            .reviews
            .last()
            .unwrap()
            .pr
            .as_deref(),
        Some(PR)
    );
}

#[test]
fn the_fix_pass_cap_ignores_accepts_second_reviewers_and_earlier_prs() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let old = c.report(
        "old",
        "aaaaaaaa",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &old, "astra").ok());
    let mut unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    unit.steps.get_mut("S1").unwrap().track.evidence.pr =
        Some("https://github.com/o/r/pull/8".into());
    commands::write_unit(&c.root.join("01-a/u"), &unit).unwrap();
    for round in 1..=10 {
        for (kind, count) in [("astra", 1), ("claude", 2)] {
            let report = c.report(
                &format!("{kind}{round}"),
                &format!("{round:040x}"),
                &review_fixture::BLOCKING[..count],
                "FIX REQUIRED",
            );
            let recorded = record(&host, &c, &report, kind);
            assert!(recorded.ok(), "{:?}", recorded.violations);
            assert_eq!(recorded.fields["passes"], round);
        }
    }
    for round in 1..=11 {
        let report = c.report(
            &format!("accept{round}"),
            &format!("{round:039x}a"),
            &[],
            "ACCEPT",
        );
        let accepted = record(&host, &c, &report, "astra");
        assert!(accepted.ok(), "{:?}", accepted.violations);
        assert_eq!(accepted.fields["passes"], 10);
    }
    let report = c.report(
        "r11",
        "bbbbbbb1",
        &review_fixture::BLOCKING[..1],
        "FIX REQUIRED",
    );
    let paths = [
        ".cm/verdicts.json",
        "JOURNAL.md",
        "OWNER-QUEUE.md",
        "RESUME.md",
    ];
    let before = paths.map(|path| sha256(&c.root.join(path)));
    let number = verdict::load(&c.root).unwrap().reviews.len() + 1;
    let refused = record(&host, &c, &report, "astra");
    assert_eq!(rules(&refused), vec!["review-cap"]);
    assert_eq!(
        refused.violations[0].message,
        "session cap of 10 passes; an 11th fix pass on the current PR is refused"
    );
    assert_eq!(paths.map(|path| sha256(&c.root.join(path))), before);
    assert!(
        !c.root
            .join(STEP)
            .join(format!("evidence/reviews/R{number}-astra.md"))
            .exists()
    );
}

#[test]
fn fresh_reader_items_stop_when_the_step_is_merged() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    for count in 1..=2 {
        let report = c.report(
            &format!("r{count}"),
            &format!("{count:08x}"),
            &review_fixture::BLOCKING[..count],
            "FIX REQUIRED",
        );
        assert!(record(&host, &c, &report, "astra").ok());
    }
    assert_eq!(
        plan::next(&c.root, None, None).unwrap().fields["fresh_readers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for phase in [Phase::Merged, Phase::Released, Phase::Deployed] {
        let mut unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
        unit.steps.get_mut("S1").unwrap().track.state = phase;
        commands::write_unit(&c.root.join("01-a/u"), &unit).unwrap();
        assert_eq!(
            plan::next(&c.root, None, None).unwrap().fields["fresh_readers"],
            serde_json::json!([])
        );
    }
}

#[test]
fn merge_gate_repair_commands_select_the_part_in_advance_and_sync() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let amendment = commands::Amendment {
        parts: vec!["-one".into(), "two".into()],
        ..Default::default()
    };
    assert!(
        commands::amend(&host, &c.root, &target(), &amendment)
            .unwrap()
            .ok()
    );
    let head = c.head();
    let forge = FakeForge::open(&head, &[("test", "success")]);
    let gate = MergeGate {
        forge: Some(&forge),
        ..Default::default()
    };
    let advance = || {
        commands::advance_gated(
            &host,
            &c.root,
            &target(),
            commands::Advance {
                to: &Phase::Merged,
                given: &Evidence {
                    sha: Some(head.clone()),
                    ..Default::default()
                },
                file: None,
                repo: Some(&c.repo),
                part: Some("-one"),
                by: &[],
            },
            &gate,
        )
        .unwrap()
    };
    let missing = advance();
    assert!(missing.violations.iter().any(|violation| violation.message
        == format!(
            "no review of {head} is recorded; `cm review record {STEP} <report> --part=-one`"
        )));
    let report = c.report("astra-review-7-part", &head, &[], "ACCEPT");
    c.receipt("astra-review-7-part", &head, "gpt-6-astra", "xhigh");
    let unrecorded = advance();
    let expected = format!(
        "cm review record {STEP} {STEP}/evidence/astra-review-7-part/report.md --reviewer astra --part=-one"
    );
    assert!(
        unrecorded
            .violations
            .iter()
            .any(|violation| violation.message.split('`').any(|text| text == expected))
    );
    let merged = FakeForge {
        state: PullState::Merged,
        ..forge
    };
    let synced = sync(&host, &merged, &c.root).unwrap();
    assert!(
        synced
            .violations
            .iter()
            .any(|violation| violation.message.split('`').any(|text| text == expected))
    );
    let saved = verdict::record(
        &host,
        &c.root,
        &target(),
        &report,
        Some("astra"),
        Some("-one"),
    )
    .unwrap();
    assert!(saved.ok(), "{:?}", saved.violations);
}

#[test]
fn a_legacy_third_fix_block_and_review_without_a_part_still_load_and_unblock() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let report = c.report("legacy", "aaaaaaa1", &[], "ACCEPT");
    assert!(record(&host, &c, &report, "astra").ok());
    let saved: serde_json::Value = store::read_json(&c.root.join(".cm/verdicts.json")).unwrap();
    assert!(saved["reviews"][0].get("part").is_none());
    assert_eq!(verdict::load(&c.root).unwrap().reviews[0].part, None);
    assert!(
        commands::advance(
            &host,
            &c.root,
            &target(),
            &Phase::Blocked("third-fix".into()),
            &Evidence::default(),
            None,
            None
        )
        .unwrap()
        .ok()
    );
    assert_eq!(c.state(), Phase::Blocked("third-fix".into()));
    let evidence = c.write("ruling.md", "Continue the review.\n");
    assert!(
        commands::unblock(&host, &c.root, &target(), Some(&evidence))
            .unwrap()
            .ok()
    );
    assert_eq!(c.state(), Phase::PrOpen);
}

#[test]
fn rulings_append_to_the_scope_file_and_the_brief_carries_it() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, false);
    let without = verdict::brief(&c.root, &target()).unwrap();
    assert!(without.fields["scope"].is_null());
    assert!(
        without.fields["brief"]
            .as_str()
            .unwrap()
            .contains("No rulings are recorded")
    );
    let first = verdict::ruling(&host, &c.root, &target(), "the sha2 edge is in scope").unwrap();
    assert!(first.ok(), "{:?}", first.violations);
    let later = at("2026-10-26T01:00:00Z");
    assert!(
        verdict::ruling(&later, &c.root, &target(), "--attach stays refused")
            .unwrap()
            .ok()
    );
    assert_eq!(
        rules(&verdict::ruling(&later, &c.root, &target(), "two\nlines").unwrap()),
        vec!["ledger-field"]
    );
    let scope =
        fs::read_to_string(c.root.join(STEP).join("evidence/coordinator-scope.md")).unwrap();
    assert_eq!(
        scope,
        "# Scope and coordinator rulings: 01-a/u/S1\n\n- 2026-10-25: the sha2 edge is in scope\n- 2026-10-26: --attach stays refused\n"
    );
    let brief = verdict::brief(&c.root, &target()).unwrap();
    assert_eq!(
        brief.fields["brief"],
        format!(
            "Build the thing.\n\n{}\n\n{}",
            verdict::SCOPE_HEADING,
            scope.trim_end()
        )
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("— ruling 01-a/u/S1"));
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn next_and_sync_flag_a_stalled_codex_launch_and_a_dirty_pr() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, false);
    let soon = plan::next_at(&c.root, None, None, at("2026-10-25T07:59:00Z").now).unwrap();
    assert_eq!(soon.fields["stalled"], serde_json::json!([]));
    let late = plan::next_at(&c.root, None, None, at("2026-10-25T08:30:00Z").now).unwrap();
    assert_eq!(late.fields["stalled"][0]["target"], STEP);
    assert_eq!(late.fields["stalled"][0]["since"], "2026-10-25T06:00Z");
    assert_eq!(late.fields["stalled"][0]["hours"], 2.5);
    assert!(
        late.lines
            .iter()
            .any(|line| line.starts_with("1. stalled 01-a/u/S1: codex launch with no PR"))
    );
    let limits = fs::read_to_string(c.root.join("LIMITS.toml"))
        .unwrap()
        .replace("codex_stall_hours = 2", "codex_stall_hours = 3");
    fs::write(c.root.join("LIMITS.toml"), limits).unwrap();
    let raised = plan::next_at(&c.root, None, None, at("2026-10-25T08:30:00Z").now).unwrap();
    assert_eq!(raised.fields["stalled"], serde_json::json!([]));

    let late_host = at("2026-10-25T10:00:00Z");
    let opened = commands::advance(
        &late_host,
        &c.root,
        &target(),
        &Phase::PrOpen,
        &Evidence {
            pr: Some(PR.into()),
            sha: Some(c.head()),
            ..Evidence::default()
        },
        None,
        Some(&c.repo),
    )
    .unwrap();
    assert!(opened.ok());
    let dirty = FakeForge {
        mergeable: "dirty",
        ..FakeForge::open(&c.head(), &[])
    };
    let synced = sync(&late_host, &dirty, &c.root).unwrap();
    assert!(synced.ok(), "{:?}", synced.violations);
    assert_eq!(synced.fields["stalled"], serde_json::json!([]));
    assert_eq!(synced.fields["dirty"][0]["target"], STEP);
    assert_eq!(synced.fields["dirty"][0]["pr"], PR);
    assert!(
        synced
            .lines
            .iter()
            .any(|line| line.starts_with("dirty 01-a/u/S1: https://github.com/o/r/pull/7"))
    );
    let next = plan::next_at(&c.root, None, None, late_host.now).unwrap();
    assert_eq!(next.fields["dirty"][0]["observed_at"], "2026-10-25T10:00Z");
    let clean = FakeForge::open(&c.head(), &[]);
    assert_eq!(
        sync(&late_host, &clean, &c.root).unwrap().fields["dirty"],
        serde_json::json!([])
    );
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn followups_list_every_non_blocking_finding_and_close_one() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let report = c.report(
        "r1",
        &head,
        &[
            "F1 | contract | introduced | src/lib.rs:40 | blocking",
            "F2 | guideline | introduced | src/lib.rs:9 | rename it",
            "F3 | contract | inherited | src/old.rs:3 | an old leak",
        ],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &report, "astra").ok());
    let all = verdict::followups(&c.root, false).unwrap();
    let ids: Vec<&str> = all.fields["followups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|followup| followup["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["R1.F2", "R1.F3"]);
    assert_eq!(all.fields["followups"][1]["pr"], PR);
    assert_eq!(all.fields["followups"][1]["origin"], "inherited");
    assert_eq!(all.fields["followups"][1]["status"], "open");
    assert_eq!(
        all.lines[0],
        "R1.F2 open 01-a/u/S1 o/r#7 guideline/introduced src/lib.rs:9 — rename it"
    );
    assert_eq!(
        rules(&verdict::close_followup(&host, &c.root, "R1.F1", "abc1234").unwrap()),
        vec!["followup-blocking"]
    );
    assert_eq!(
        rules(&verdict::close_followup(&host, &c.root, "R9.F2", "abc1234").unwrap()),
        vec!["finding-unknown"]
    );
    let closed = verdict::close_followup(&host, &c.root, "R1.F2", "fixed in abc1234").unwrap();
    assert!(closed.ok(), "{:?}", closed.violations);
    assert_eq!(
        rules(&verdict::close_followup(&host, &c.root, "R1.F2", "again").unwrap()),
        vec!["followup-closed"]
    );
    let listed = verdict::followups(&c.root, false).unwrap();
    assert_eq!(listed.fields["followups"][0]["status"], "closed");
    let epoch = verdict::followups(&c.root, true).unwrap();
    assert_eq!(epoch.fields["followups"].as_array().unwrap().len(), 1);
    assert_eq!(epoch.lines[0], "### o/r");
    assert_eq!(
        epoch.lines[2],
        "- [ ] `src/old.rs:3` (contract, inherited): an old leak — R1.F3, 01-a/u/S1 o/r#7"
    );
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn sync_holds_a_codex_merge_at_pr_open_until_its_gate_holds() {
    for merged_at in [Some(review_fixture::MERGED_AT), None] {
        let host = at("2026-10-25T06:00:00Z");
        let c = campaign(&host, true);
        let head = c.head();
        let merged = FakeForge {
            state: PullState::Merged,
            merged_at: merged_at.map(str::to_owned),
            ..FakeForge::open(&head, &[("fmt, clippy, test", "success")])
        };
        let held = sync(&host, &merged, &c.root).unwrap();
        assert_eq!(
            rules(&held),
            vec!["merge-gate", "merge-gate"],
            "{:?}",
            held.violations
        );
        assert_eq!(c.state(), Phase::PrOpen);
        let accept = c.report("astra-review-7", &head, &[], "ACCEPT");
        c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
        assert!(record(&host, &c, &accept, "astra").ok());
        claude_accept(&host, &c, &head);
        let later = at("2026-10-25T07:00:00Z");
        let moved = sync(&later, &merged, &c.root).unwrap();
        assert!(moved.ok(), "{:?}", moved.violations);
        assert_eq!(c.state(), Phase::Merged);
        let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
        let track = &unit.steps["S1"].track;
        assert_eq!(track.evidence.merged_at.as_deref(), merged_at);
        assert_eq!(track.since, cm::host::stamp(later.now()));
        assert_eq!(
            unit.history.last().unwrap().evidence.merged_at.as_deref(),
            merged_at
        );
        let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        let suffix = merged_at
            .map(|at| format!("; merged_at {at}"))
            .unwrap_or_default();
        assert_eq!(
            journal
                .lines()
                .find(|line| line.starts_with("- 01-a/u/S1: pr-open → merged ")),
            Some(format!("- {STEP}: pr-open → merged (sha {head}{suffix})").as_str()),
        );
        assert!(sync(&later, &merged, &c.root).unwrap().ok());
        let card = fs::read_to_string(c.root.join("CARD.md")).unwrap();
        assert_eq!(
            card.split("## In flight\n\n")
                .nth(1)
                .unwrap()
                .split("\n## ")
                .next()
                .unwrap()
                .trim(),
            "Nothing is in flight."
        );
    }
}

#[test]
fn advance_merged_with_pr_records_merge_time_and_refuses_a_different_pr() {
    for (state, reported, expected) in [
        (
            PullState::Merged,
            Some(review_fixture::MERGED_AT),
            Some(review_fixture::MERGED_AT),
        ),
        (PullState::Merged, None, None),
        (PullState::Open, Some(review_fixture::MERGED_AT), None),
    ] {
        let host = at("2026-10-25T06:00:00Z");
        let c = campaign(&host, true);
        let head = c.head();
        let report = c.report("astra-review-7", &head, &[], "ACCEPT");
        c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
        assert!(record(&host, &c, &report, "astra").ok());
        claude_accept(&host, &c, &head);
        let forge = FakeForge {
            state,
            merged_at: reported.map(str::to_owned),
            ..FakeForge::open(&head, &[("test", "success")])
        };
        let later = at("2026-10-25T07:00:00Z");
        let advance = |pr: &str| {
            commands::advance_gated(
                &later,
                &c.root,
                &target(),
                commands::Advance {
                    to: &Phase::Merged,
                    given: &Evidence {
                        sha: Some(head.clone()),
                        pr: Some(pr.into()),
                        ..Evidence::default()
                    },
                    file: None,
                    repo: Some(&c.repo),
                    part: None,
                    by: &[],
                },
                &MergeGate {
                    forge: Some(&forge),
                    ..MergeGate::default()
                },
            )
            .unwrap()
        };
        let before = store::load_unit(&c.root.join("01-a/u")).unwrap();
        let refused = advance(review_fixture::OTHER_PR);
        assert_eq!(rules(&refused), ["evidence-pr"]);
        assert_eq!(store::load_unit(&c.root.join("01-a/u")).unwrap(), before);
        let moved = advance(PR);
        assert!(moved.ok(), "{:?}", moved.violations);
        let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
        assert_eq!(
            unit.steps["S1"].track.evidence.merged_at.as_deref(),
            expected
        );
        assert_eq!(unit.steps["S1"].track.since, cm::host::stamp(later.now()));
    }
}

#[test]
fn multipart_merge_time_uses_latest_part_and_requires_every_time() {
    for mode in ["sync", "advance", "settle", "blocked-pr", "blocked-no-pr"] {
        for (first, second, expected) in [
            (
                Some(review_fixture::MERGED_AT),
                Some(review_fixture::EARLIER_MERGED_AT),
                Some(review_fixture::MERGED_AT),
            ),
            (Some(review_fixture::MERGED_AT), None, None),
            (None, Some(review_fixture::EARLIER_MERGED_AT), None),
        ] {
            let host = at("2026-10-25T06:00:00Z");
            let c = review_fixture::multipart_campaign(&host);
            let blocked = mode.starts_with("blocked-");
            if blocked {
                review_fixture::block_multipart_parent(&c, &host);
            }
            let head = c.head();
            let mut forge = FakeForge {
                state: PullState::Merged,
                merged_at: first.map(str::to_owned),
                ..FakeForge::open(&head, &[])
            };
            forge
                .parts
                .insert(review_fixture::OTHER_PR.into(), FakeForge::open(&head, &[]));
            let advance_part = |host: &Fake, part, pr: &str, forge: &FakeForge| {
                let moved = commands::advance_gated(
                    host,
                    &c.root,
                    &target(),
                    commands::Advance {
                        to: &Phase::Merged,
                        given: &Evidence {
                            sha: Some(head.clone()),
                            pr: Some(pr.into()),
                            ..Default::default()
                        },
                        file: None,
                        repo: Some(&c.repo),
                        part: Some(part),
                        by: &[],
                    },
                    &MergeGate {
                        forge: Some(forge),
                        ..Default::default()
                    },
                )
                .unwrap();
                assert!(moved.ok(), "{:?}", moved.violations);
            };
            match mode {
                "sync" | "blocked-pr" | "blocked-no-pr" => {
                    assert!(sync(&host, &forge, &c.root).unwrap().ok())
                }
                "advance" => advance_part(&host, "one", PR, &forge),
                _ => review_fixture::parts_at(&c, &host, Phase::Merged, [first, second]),
            }
            let later = at("2026-10-25T07:00:00Z");
            let last = forge.parts.get_mut(review_fixture::OTHER_PR).unwrap();
            last.state = PullState::Merged;
            last.merged_at = second.map(str::to_owned);
            if mode == "advance" {
                advance_part(&later, "two", review_fixture::OTHER_PR, &forge);
            } else {
                assert!(sync(&later, &forge, &c.root).unwrap().ok());
            }
            let parent_at = if blocked {
                at("2026-10-25T08:00:00Z")
            } else {
                later
            };
            if blocked {
                assert_eq!(c.state(), Phase::Blocked("parts".into()));
                let moved = commands::advance_gated(
                    &parent_at,
                    &c.root,
                    &target(),
                    commands::Advance {
                        to: &Phase::Merged,
                        given: &Evidence {
                            sha: Some(head.clone()),
                            pr: (mode == "blocked-pr").then(|| review_fixture::OTHER_PR.into()),
                            ..Default::default()
                        },
                        file: None,
                        repo: Some(&c.repo),
                        part: None,
                        by: &[],
                    },
                    &MergeGate {
                        forge: Some(&forge),
                        ..Default::default()
                    },
                )
                .unwrap();
                assert!(moved.ok(), "{:?}", moved.violations);
                assert_eq!(
                    moved.fields["evidence"]["merged_at"],
                    serde_json::json!(expected)
                );
            }
            let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
            let track = &unit.steps["S1"].track;
            assert_eq!(track.state, Phase::Merged);
            assert_eq!(track.evidence.merged_at.as_deref(), expected, "{mode}");
            assert_eq!(track.since, cm::host::stamp(parent_at.now()));
            let moved = unit
                .history
                .iter()
                .find(|moved| moved.target == STEP && moved.to == Phase::Merged)
                .unwrap();
            assert_eq!(moved.evidence.merged_at.as_deref(), expected, "{mode}");
            assert_eq!(moved.at, cm::host::stamp(parent_at.now()));
            review_fixture::parts_at(&c, &parent_at, Phase::Released, [first, second]);
            assert!(sync(&parent_at, &forge, &c.root).unwrap().ok());
            let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
            assert_eq!(unit.steps["S1"].track.state, Phase::Released);
            assert_eq!(
                unit.steps["S1"].track.evidence.merged_at.as_deref(),
                expected,
                "{mode}"
            );
        }
    }
}

#[test]
fn a_receipt_counts_only_for_a_review_that_happened_and_was_recorded() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let green = FakeForge::open(&head, &[("fmt, clippy, test", "success")]);
    let gate = MergeGate {
        forge: Some(&green),
        ..MergeGate::default()
    };
    let claude = c.report("claude", &head, &[], "ACCEPT");
    assert!(record(&host, &c, &claude, "claude").ok());
    for (dir, edit) in [
        ("failed", "/turn/status"),
        ("forked", "/thread_fresh/forkedFromId"),
        ("child", "/thread_fresh/parentThreadId"),
        ("long", "/thread_fresh/turns_in_thread"),
    ] {
        c.receipt_with(dir, &head, "gpt-6-astra", "xhigh", |receipt| {
            *receipt.pointer_mut(edit).unwrap() = match edit {
                "/turn/status" => "failed".into(),
                "/thread_fresh/turns_in_thread" => 2.into(),
                _ => "thread-0".into(),
            };
        });
    }
    // No receipt holds, and so no Astra ACCEPT is recorded either: both gaps are named.
    let refused = c.merge(&host, &gate);
    assert_eq!(
        rules(&refused),
        vec!["merge-gate", "merge-gate"],
        "{:?}",
        refused.violations
    );
    assert!(refused.violations[1].message.contains("no astra ACCEPT"));
    let message = &refused.violations[0].message;
    for why in [
        "turn not completed",
        "thread forked or has a parent",
        "thread holds other than one turn",
    ] {
        assert!(message.contains(why), "{why}: {message}");
    }

    // Astra's FIX REQUIRED receipt with only Claude's ACCEPT recorded does not merge.
    let astra = c.report(
        "astra-review-7",
        &head,
        &["F1 | contract | introduced | src/lib.rs:1 | broken"],
        "FIX REQUIRED",
    );
    c.receipt_with("astra-review-7", &head, "gpt-6-astra", "xhigh", |receipt| {
        receipt["verdict"] = "FIX REQUIRED".into();
    });
    let unrecorded = c.merge(&host, &gate);
    // The receipt binds no recorded review, and so no Astra ACCEPT is recorded either.
    assert_eq!(rules(&unrecorded), vec!["merge-gate", "merge-gate"]);
    assert!(
        unrecorded.violations[0].message.contains(
            "it is not recorded; `cm review record 01-a/u/S1 01-a/u/S1/evidence/astra-review-7/report.md --reviewer astra`"
        ),
        "{:?}",
        unrecorded.violations
    );
    assert!(record(&host, &c, &astra, "astra").ok());
    let blocked = c.merge(&host, &gate);
    assert_eq!(rules(&blocked), vec!["merge-gate"]);
    assert!(blocked.violations[0].message.contains("R2.F1 unresolved"));
    let ruling = c.write("ruling.md", "Coordinator: F1 is out of scope.\n");
    assert!(
        verdict::resolve(&host, &c.root, &target(), "R2.F1", "ruling.md", &ruling)
            .unwrap()
            .ok()
    );
    // A resolved FIX REQUIRED is no ACCEPT: the model lane's head still needs Astra's.
    let unaccepted = c.merge(&host, &gate);
    assert!(
        unaccepted.violations[0].message.contains("no astra ACCEPT"),
        "{:?}",
        unaccepted.violations
    );
    astra_accept(&host, &c, &head);
    assert!(c.merge(&host, &gate).ok());
}

#[test]
fn a_malformed_review_number_names_no_finding() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let report = c.report(
        "r1",
        &c.head(),
        &[
            "F1 | contract | introduced | src/lib.rs:1 | broken",
            "F2 | taste | introduced | - | a nit",
        ],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &report, "astra").ok());
    for id in ["Rbogus.F2", "R.F2", "X1.F2"] {
        assert_eq!(
            rules(&verdict::close_followup(&host, &c.root, id, "x").unwrap()),
            vec!["finding-unknown"],
            "{id}"
        );
    }
    for id in ["Rbogus.F1", "R1x.F1"] {
        assert_eq!(
            rules(
                &verdict::resolve(
                    &host,
                    &c.root,
                    &target(),
                    id,
                    "abcdef1",
                    Path::new("abcdef1")
                )
                .unwrap()
            ),
            vec!["finding-unknown"],
            "{id}"
        );
    }
    let ledger = verdict::load(&c.root).unwrap();
    assert!(
        ledger.reviews[0]
            .findings
            .iter()
            .all(|logged| logged.resolution.is_none() && logged.closed.is_none())
    );
}

#[test]
fn ci_reads_every_page_of_checks_and_refuses_a_short_one() {
    let runs = |range: std::ops::RangeInclusive<u32>, conclusion: &str| {
        range
            .map(|n| {
                format!(r#"{{"name":"c{n}","status":"completed","conclusion":"{conclusion}"}}"#)
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let gh = support::FakeGh::new(&format!(
        r#"case "$2" in
  */check-runs\?per_page=100\&page=1) echo '{{"total_count":101,"check_runs":[{}]}}' ;;
  */check-runs\?per_page=100\&page=2) echo '{{"total_count":101,"check_runs":[{}]}}' ;;
  */status\?per_page=100\&page=1) echo '{{"total_count":0,"statuses":[]}}' ;;
  *) exit 1 ;;
esac
"#,
        runs(1..=100, "success"),
        runs(101..=101, "failure"),
    ));
    let forge = Gh {
        program: gh.program(),
        timeout: Duration::from_secs(10),
    };
    let repo: GithubRepo = "o/r".parse().unwrap();
    let checks = forge.checks(&repo, &CommitSha::new("abc")).unwrap();
    assert_eq!(checks.len(), 101);
    assert!(!ci_green(&checks));
    assert_eq!(checks[100].state, "failure");

    let short = support::FakeGh::new(
        r#"case "$2" in
  */check-runs\?per_page=100\&page=1) echo '{"total_count":101,"check_runs":[]}' ;;
  *) echo '{"total_count":0,"statuses":[]}' ;;
esac
"#,
    );
    let forge = Gh {
        program: short.program(),
        timeout: Duration::from_secs(10),
    };
    assert_eq!(
        forge.checks(&repo, &CommitSha::new("abc")),
        Err(ForgeError::Json)
    );
    let broken = support::FakeGh::new(
        r#"case "$2" in
  */check-runs\?per_page=100\&page=1) echo '{"total_count":101,"check_runs":[{"name":"a","status":"completed","conclusion":"success"}]}' ;;
  *) exit 1 ;;
esac
"#,
    );
    let forge = Gh {
        program: broken.program(),
        timeout: Duration::from_secs(10),
    };
    assert_eq!(
        forge.checks(&repo, &CommitSha::new("abc")),
        Err(ForgeError::Exit(Some(1)))
    );
}

#[test]
fn a_failed_sync_keeps_the_last_dirty_observation() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let dirty = FakeForge {
        mergeable: "dirty",
        ..FakeForge::open(&c.head(), &[])
    };
    assert!(sync(&host, &dirty, &c.root).unwrap().ok());
    let later = at("2026-10-25T07:00:00Z");
    let down = FakeForge {
        down: true,
        ..FakeForge::open(&c.head(), &[])
    };
    let failed = sync(&later, &down, &c.root).unwrap();
    assert_eq!(rules(&failed), vec!["sync-unknown"]);
    assert_eq!(
        failed.fields["dirty"][0]["observed_at"],
        "2026-10-25T06:00Z"
    );
    let next = plan::next_at(&c.root, None, None, later.now).unwrap();
    assert_eq!(next.fields["dirty"][0]["target"], STEP);
    let clean = FakeForge::open(&c.head(), &[]);
    assert!(sync(&later, &clean, &c.root).unwrap().ok());
    assert_eq!(
        plan::next_at(&c.root, None, None, later.now)
            .unwrap()
            .fields["dirty"],
        serde_json::json!([])
    );
}

/// Commits `file` with `text` on the current branch of `repo` and returns the new head.
fn commit(repo: &Path, file: &str, text: &str) -> String {
    fs::write(repo.join(file), text).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-q", "-m", file]);
    git(repo, &["rev-parse", "HEAD"])
}

/// The repo with a feature branch whose tip H1 is reviewed and accepted, and the base branch's
/// name, checked out on the feature branch.
fn reviewed_feature(host: &Fake, c: &Campaign) -> (String, String) {
    let base = git(&c.repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(&c.repo, &["checkout", "-q", "-b", "feature"]);
    let h1 = commit(&c.repo, "shared.txt", "feature\n");
    let report = c.report("astra-review-7-h1", &h1, &[], "ACCEPT");
    c.receipt("astra-review-7-h1", &h1, "gpt-6-astra", "xhigh");
    assert!(record(host, c, &report, "astra").ok());
    claude_accept(host, c, &h1);
    (h1, base)
}

/// Why the gate's walk from the head stopped, read straight from `merge_gaps`.
fn walk_stop(c: &Campaign, base: &str) -> Option<CleanUpdateError> {
    let tip = git(&c.repo, &["rev-parse", base]);
    let lineage = Lineage {
        repo: &c.repo,
        base: Some(&tip),
    };
    verdict::merge_gaps(
        &c.root,
        &target(),
        None,
        Some(PR),
        &c.head(),
        Some(&lineage),
    )
    .unwrap()
    .stop
}

fn merge_with(host: &Fake, c: &Campaign, base: &str) -> Outcome {
    let head = c.head();
    let tip = git(&c.repo, &["rev-parse", base]);
    let forge = FakeForge {
        base: Some(tip),
        ..FakeForge::open(&head, &[("fmt, clippy, test", "success")])
    };
    c.merge(
        host,
        &MergeGate {
            forge: Some(&forge),
            ..MergeGate::default()
        },
    )
}

#[test]
fn a_clean_update_from_the_base_branch_needs_no_new_review() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let (h1, base) = reviewed_feature(&host, &c);
    for round in 0..2 {
        git(&c.repo, &["checkout", "-q", &base]);
        commit(&c.repo, &format!("main-{round}.txt"), "main\n");
        git(&c.repo, &["checkout", "-q", "feature"]);
        git(
            &c.repo,
            &[
                "-c",
                "user.name=cm",
                "-c",
                "user.email=cm@example.com",
                "merge",
                "-q",
                "--no-edit",
                &base,
            ],
        );
    }
    let merged = merge_with(&host, &c, &base);
    assert!(merged.ok(), "{:?}", merged.violations);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains(&format!(
            "- reviewed at {h1}; every commit since is a clean update from the base branch"
        )),
        "{journal}"
    );
}

#[test]
fn a_merge_that_resolved_conflicts_needs_its_own_review() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let (_, base) = reviewed_feature(&host, &c);
    git(&c.repo, &["checkout", "-q", &base]);
    commit(&c.repo, "shared.txt", "main\n");
    git(&c.repo, &["checkout", "-q", "feature"]);
    let conflicted = Command::new("git")
        .arg("-C")
        .arg(&c.repo)
        .args([
            "-c",
            "user.name=cm",
            "-c",
            "user.email=cm@example.com",
            "merge",
            "-q",
            "--no-edit",
            &base,
        ])
        .output()
        .unwrap();
    assert!(!conflicted.status.success(), "the merge conflicts");
    commit(&c.repo, "shared.txt", "feature and main\n");
    let refused = merge_with(&host, &c, &base);
    assert!(
        rules(&refused).iter().all(|rule| *rule == "merge-gate"),
        "{:?}",
        refused.violations
    );
    let merge = c.head();
    assert_eq!(
        walk_stop(&c, &base),
        Some(CleanUpdateError::Conflicts {
            commit: merge.clone()
        })
    );
    assert!(rules(&refused).contains(&"merge-gate"));
    let merge = c.head();
    let report = c.report("astra-review-7-merge", &merge, &[], "ACCEPT");
    c.receipt("astra-review-7-merge", &merge, "gpt-6-astra", "xhigh");
    assert!(record(&host, &c, &report, "astra").ok());
    claude_accept(&host, &c, &merge);
    let merged = merge_with(&host, &c, &base);
    assert!(merged.ok(), "{:?}", merged.violations);
}

#[test]
fn a_new_commit_after_the_reviewed_head_needs_a_review() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let (_, base) = reviewed_feature(&host, &c);
    commit(&c.repo, "more.txt", "a fix\n");
    let refused = merge_with(&host, &c, &base);
    assert_eq!(
        walk_stop(&c, &base),
        Some(CleanUpdateError::NotMerge { commit: c.head() })
    );
    assert!(rules(&refused).contains(&"merge-gate"));
    // A merge from a branch other than the base is no clean update either.
    let c = campaign(&host, true);
    let (_, base) = reviewed_feature(&host, &c);
    git(&c.repo, &["checkout", "-q", "-b", "side", &base]);
    commit(&c.repo, "side.txt", "side\n");
    git(&c.repo, &["checkout", "-q", "feature"]);
    git(
        &c.repo,
        &[
            "-c",
            "user.name=cm",
            "-c",
            "user.email=cm@example.com",
            "merge",
            "-q",
            "--no-edit",
            "side",
        ],
    );
    let refused = merge_with(&host, &c, &base);
    let side = git(&c.repo, &["rev-parse", "side"]);
    let tip = git(&c.repo, &["rev-parse", &base]);
    assert_eq!(
        walk_stop(&c, &base),
        Some(CleanUpdateError::OffBase {
            commit: c.head(),
            parent: side,
            base: tip,
        })
    );
    assert!(rules(&refused).contains(&"merge-gate"));
}

/// Merges a new base-branch commit into `feature` without conflicts; returns the merge.
fn clean_merge(c: &Campaign, base: &str, file: &str) -> String {
    git(&c.repo, &["checkout", "-q", base]);
    commit(&c.repo, file, "main\n");
    git(&c.repo, &["checkout", "-q", "feature"]);
    git(
        &c.repo,
        &[
            "-c",
            "user.name=cm",
            "-c",
            "user.email=cm@example.com",
            "merge",
            "-q",
            "--no-edit",
            base,
        ],
    );
    c.head()
}

#[test]
fn a_clean_update_never_outweighs_a_review_at_the_head_or_a_walked_commit() {
    let blocking = ["F1 | contract | introduced | src/lib.rs:1 | broken"];
    let host = at("2026-10-25T06:00:00Z");

    // A FIX REQUIRED review recorded at the head binds there.
    let c = campaign(&host, true);
    let (_, base) = reviewed_feature(&host, &c);
    let head = clean_merge(&c, &base, "main-1.txt");
    let fix = c.report("head", &head, &blocking, "FIX REQUIRED");
    assert!(record(&host, &c, &fix, "claude").ok());
    let refused = merge_with(&host, &c, &base);
    assert_eq!(
        rules(&refused),
        vec!["merge-gate", "merge-gate"],
        "{:?}",
        refused.violations
    );
    assert!(
        refused
            .violations
            .iter()
            .any(|v| v.message.contains("R3.F1 unresolved"))
    );

    // So does a finished Astra receipt at the head whose review is not recorded.
    let c = campaign(&host, true);
    let (_, base) = reviewed_feature(&host, &c);
    let head = clean_merge(&c, &base, "main-1.txt");
    c.receipt_with(
        "astra-review-7-head",
        &head,
        "gpt-6-astra",
        "xhigh",
        |receipt| {
            receipt["verdict"] = "FIX REQUIRED".into();
        },
    );
    let refused = merge_with(&host, &c, &base);
    assert!(
        refused
            .violations
            .iter()
            .any(|v| v.message.contains("it is not recorded")),
        "{:?}",
        refused.violations
    );

    // And a FIX REQUIRED review at a walked commit stops the walk before the ACCEPT under it.
    let c = campaign(&host, true);
    let (_, base) = reviewed_feature(&host, &c);
    let walked = clean_merge(&c, &base, "main-1.txt");
    let fix = c.report("walked", &walked, &blocking, "FIX REQUIRED");
    assert!(record(&host, &c, &fix, "claude").ok());
    clean_merge(&c, &base, "main-2.txt");
    let refused = merge_with(&host, &c, &base);
    assert!(
        refused.violations.iter().any(|v| v.message.contains(&format!(
            "the nearest reviewed commit before {}, {walked}, joined to it by clean updates, does not clear",
            c.head()
        )) && v.message.contains("R3.F1 unresolved")),
        "{:?}",
        refused.violations
    );
    assert_eq!(c.state(), Phase::PrOpen);
}

#[test]
fn the_recorded_astra_review_must_be_the_receipted_report() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let green = FakeForge::open(&head, &[("fmt, clippy, test", "success")]);
    let gate = MergeGate {
        forge: Some(&green),
        ..MergeGate::default()
    };
    let receipted = c.report(
        "astra-review-7",
        &head,
        &["F1 | contract | introduced | src/lib.rs:1 | broken"],
        "FIX REQUIRED",
    );
    c.receipt_with("astra-review-7", &head, "gpt-6-astra", "xhigh", |receipt| {
        receipt["verdict"] = "FIX REQUIRED".into();
    });
    let other = c.report("other", &head, &[], "ACCEPT");
    assert!(record(&host, &c, &other, "astra").ok());
    claude_accept(&host, &c, &head);
    let refused = c.merge(&host, &gate);
    assert_eq!(
        rules(&refused),
        vec!["merge-gate"],
        "{:?}",
        refused.violations
    );
    assert!(
        refused.violations[0]
            .message
            .contains("but no Astra review recorded at")
            && refused.violations[0]
                .message
                .contains("(R1) is that report"),
        "{:?}",
        refused.violations
    );
    let unhashed = campaign(&host, true);
    unhashed.receipt_with(
        "astra-review-7",
        &unhashed.head(),
        "gpt-6-astra",
        "xhigh",
        |receipt| {
            receipt.as_object_mut().unwrap().remove("report_sha256");
        },
    );
    let accept = unhashed.report("astra-review-7", &unhashed.head(), &[], "ACCEPT");
    assert!(record(&host, &unhashed, &accept, "astra").ok());
    let green_too = FakeForge::open(&unhashed.head(), &[("fmt, clippy, test", "success")]);
    let refused = unhashed.merge(
        &host,
        &MergeGate {
            forge: Some(&green_too),
            ..MergeGate::default()
        },
    );
    assert!(
        refused.violations[0]
            .message
            .contains("names no report_sha256")
    );

    assert!(record(&host, &c, &receipted, "astra").ok());
    let blocked = c.merge(&host, &gate);
    assert_eq!(rules(&blocked), vec!["merge-gate"]);
    assert!(blocked.violations[0].message.contains("R3.F1 unresolved"));
}

#[test]
fn every_finished_astra_receipt_at_the_head_binds() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let green = FakeForge::open(&head, &[("fmt, clippy, test", "success")]);
    let gate = MergeGate {
        forge: Some(&green),
        ..MergeGate::default()
    };
    let accept = c.report("astra-review-7-a", &head, &[], "ACCEPT");
    c.receipt("astra-review-7-a", &head, "gpt-6-astra", "xhigh");
    assert!(record(&host, &c, &accept, "astra").ok());
    claude_accept(&host, &c, &head);
    let rerun = c.report(
        "astra-review-7-b",
        &head,
        &["F1 | contract | introduced | src/lib.rs:1 | broken"],
        "FIX REQUIRED",
    );
    c.receipt_with(
        "astra-review-7-b",
        &head,
        "gpt-6-astra",
        "xhigh",
        |receipt| {
            receipt["verdict"] = "FIX REQUIRED".into();
        },
    );
    let refused = c.merge(&host, &gate);
    assert_eq!(
        rules(&refused),
        vec!["merge-gate"],
        "{:?}",
        refused.violations
    );
    assert!(
        refused.violations[0]
            .message
            .starts_with("01-a/u/S1/evidence/astra-review-7-b/receipt.json receipts a report"),
        "{:?}",
        refused.violations
    );
    assert!(record(&host, &c, &rerun, "astra").ok());
    let blocked = c.merge(&host, &gate);
    assert_eq!(rules(&blocked), vec!["merge-gate"]);
    assert!(blocked.violations[0].message.contains("R3.F1 unresolved"));
    let ruling = c.write("ruling.md", "Coordinator: F1 is out of scope.\n");
    assert!(
        verdict::resolve(&host, &c.root, &target(), "R3.F1", "ruling.md", &ruling)
            .unwrap()
            .ok()
    );
    assert!(c.merge(&host, &gate).ok());
}

#[test]
fn a_base_that_already_contains_the_head_takes_no_clean_update() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let (_, base) = reviewed_feature(&host, &c);
    git(&c.repo, &["checkout", "-q", "-b", "side", &base]);
    commit(&c.repo, "side.txt", "unreviewed\n");
    git(&c.repo, &["checkout", "-q", "feature"]);
    let merge = [
        "-c",
        "user.name=cm",
        "-c",
        "user.email=cm@example.com",
        "merge",
        "-q",
        "--no-ff",
        "--no-edit",
    ];
    git(&c.repo, &[&merge[..], &["side"]].concat());
    let head = c.head();
    assert!(matches!(
        walk_stop(&c, &base),
        Some(CleanUpdateError::OffBase { .. })
    ));
    git(&c.repo, &["checkout", "-q", &base]);
    git(&c.repo, &[&merge[..], &["feature"]].concat());
    git(&c.repo, &["checkout", "-q", "feature"]);
    let tip = git(&c.repo, &["rev-parse", &base]);
    assert_eq!(
        walk_stop(&c, &base),
        Some(CleanUpdateError::BaseContainsHead {
            head: head.clone(),
            base: tip,
        })
    );
    let refused = merge_with(&host, &c, &base);
    assert!(
        rules(&refused).contains(&"merge-gate"),
        "{:?}",
        refused.violations
    );
    assert_eq!(c.state(), Phase::PrOpen);
}

fn red_at_head(head: &str, lab: &'static str, base_lab: &'static str) -> FakeForge {
    FakeForge {
        base: Some("b".repeat(40)),
        base_checks: vec![("quality", "success"), ("lab", base_lab)],
        ..FakeForge::open(head, &[("quality", "success"), ("lab", lab)])
    }
}

fn known_red(
    host: &Fake,
    c: &Campaign,
    forge: &dyn Forge,
    commit: &str,
    reason: &str,
    until: &str,
) -> Outcome {
    cm::holds::known_red(
        host,
        forge,
        &c.root,
        &target(),
        &cm::holds::KnownRedArgs {
            commit,
            standing: commit.is_empty(),
            campaign: false,
            check: "lab",
            reason,
            owner: "coordinator",
            until,
        },
    )
    .unwrap()
}

/// A Claude reviewer's ACCEPT at `head`: the family that did not write a Codex-built PR.
/// A second Astra report at `head`, an explicit ACCEPT after a FIX REQUIRED was resolved.
fn astra_accept(host: &Fake, c: &Campaign, head: &str) {
    let astra = c.write(
        &format!("drafts/astra-accept-{head}.md"),
        &format!("# Astra re-review\n\nFixed.\n\n```cm-review\nhead: {head}\n```\n\nACCEPT\n"),
    );
    let recorded = record(host, c, &astra, "astra");
    assert!(recorded.ok(), "{:?}", recorded.violations);
}

fn claude_accept(host: &Fake, c: &Campaign, head: &str) {
    let claude = c.write(
        &format!("drafts/claude-{head}.md"),
        &format!("# Claude review\n\nRead in full.\n\n```cm-review\nhead: {head}\n```\n\nACCEPT\n"),
    );
    let recorded = record(host, c, &claude, "claude");
    assert!(recorded.ok(), "{:?}", recorded.violations);
}

/// The stamp's gate at the head of a Codex-built PR: Astra's receipted ACCEPT and a Claude
/// reviewer's, the family that did not write it.
fn accepted(host: &Fake, c: &Campaign) {
    let head = c.head();
    let accept = c.report("astra-review-7", &head, &[], "ACCEPT");
    c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
    assert!(record(host, c, &accept, "astra").ok());
    claude_accept(host, c, &head);
    let fix = Declaration {
        lane: Some(Lane::Coordinator),
        work: cm::state::Work::Study,
        ..Declaration::default()
    };
    assert!(
        commands::declare(host, &c.root.join("01-a/u/build"), &fix)
            .unwrap()
            .ok()
    );
}

#[test]
fn a_known_red_exception_covers_only_its_failing_check_at_its_head_until_it_closes() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    accepted(&host, &c);
    let head = c.head();
    let red = red_at_head(&head, "failure", "failure");
    let why = "red by design until build lands";
    assert_eq!(
        rules(&known_red(&host, &c, &red, &head, why, "pin:build@abcdef0")),
        vec!["after-parse"]
    );
    assert_eq!(
        rules(&known_red(&host, &c, &red, &head, "", "build")),
        vec!["ledger-field"]
    );
    assert_eq!(
        rules(&known_red(&host, &c, &red, &head[..12], why, "build")),
        vec!["known-red-commit"]
    );
    assert_eq!(
        rules(&known_red(&host, &c, &red, &"e".repeat(40), why, "build")),
        vec!["known-red-commit"]
    );
    assert_eq!(
        rules(&known_red(&host, &c, &red, &head, why, "nowhere")),
        vec!["after-unknown"]
    );
    let cancelled = red_at_head(&head, "cancelled", "failure");
    assert_eq!(
        rules(&known_red(&host, &c, &cancelled, &head, why, "build")),
        vec!["known-red-refused"]
    );
    let regression = red_at_head(&head, "failure", "success");
    let refused = known_red(&host, &c, &regression, &head, why, "build");
    assert_eq!(rules(&refused), vec!["known-red-refused"]);
    assert!(
        refused.violations[0].message.contains("regression"),
        "{:?}",
        refused.violations
    );
    let unseen = FakeForge {
        base_checks: vec![("quality", "success")],
        ..red_at_head(&head, "failure", "failure")
    };
    assert_eq!(
        rules(&known_red(&host, &c, &unseen, &head, why, "build")),
        vec!["known-red-refused"]
    );
    let namesakes = FakeForge {
        base_checks: vec![("lab", "failure"), ("lab", "success")],
        ..red_at_head(&head, "failure", "failure")
    };
    assert_eq!(
        rules(&known_red(&host, &c, &namesakes, &head, why, "build")),
        vec!["known-red-refused"]
    );
    let pending = red_at_head(&head, "failure", "in_progress");
    assert_eq!(
        rules(&known_red(&host, &c, &pending, &head, why, "build")),
        vec!["known-red-refused"]
    );
    let added = known_red(&host, &c, &red, &head, why, "build");
    assert!(added.ok(), "{:?}", added.violations);

    let gate = |forge: &FakeForge| {
        c.merge(
            &host,
            &MergeGate {
                forge: Some(forge),
                ..MergeGate::default()
            },
        )
    };
    let other = FakeForge {
        base: Some("b".repeat(40)),
        base_checks: vec![("lab", "failure")],
        ..FakeForge::open(&head, &[("quality", "failure"), ("lab", "failure")])
    };
    let refused = gate(&other);
    assert_eq!(rules(&refused), vec!["merge-gate"]);
    assert!(
        refused.violations[0].message.contains("quality failure"),
        "{:?}",
        refused.violations
    );
    assert!(
        !refused.violations[0].message.contains("lab"),
        "{:?}",
        refused.violations
    );
    let timed_out = gate(&red_at_head(&head, "timed_out", "failure"));
    assert_eq!(rules(&timed_out), vec!["merge-gate"]);
    assert!(
        timed_out.violations[0].message.contains("lab timed_out"),
        "{:?}",
        timed_out.violations
    );
    assert_eq!(
        rules(&gate(&red_at_head(&head, "failure", "success"))),
        vec!["merge-gate"]
    );
    let unseen = FakeForge {
        base_checks: vec![("quality", "success")],
        ..red_at_head(&head, "failure", "failure")
    };
    assert_eq!(rules(&gate(&unseen)), vec!["merge-gate"]);

    let merged = gate(&red);
    assert!(merged.ok(), "{:?}", merged.violations);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains(&format!("- known-red: lab failed at {head}")),
        "{journal}"
    );
    assert!(lint(&c.root).unwrap().ok());

    let synced = campaign(&host, true);
    accepted(&host, &synced);
    let head = synced.head();
    let red = FakeForge {
        state: PullState::Merged,
        ..red_at_head(&head, "failure", "failure")
    };
    assert!(known_red(&host, &synced, &red, &head, why, "build").ok());
    let moved = sync(&host, &red, &synced.root).unwrap();
    assert!(moved.ok(), "{:?}", moved.violations);
    assert_eq!(synced.state(), Phase::Merged);
    let journal = fs::read_to_string(synced.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains(&format!("- known-red: lab failed at {head}")),
        "{journal}"
    );

    let closing = campaign(&host, true);
    accepted(&host, &closing);
    let head = closing.head();
    let red = red_at_head(&head, "failure", "failure");
    assert!(known_red(&host, &closing, &red, &head, why, "build").ok());
    let built = closing.write("01-a/u/build/evidence/built.md", "Build merged.\n");
    let done = commands::advance(
        &host,
        &closing.root,
        &parse_target("01-a/u/build", true).unwrap(),
        &Phase::Done,
        &Evidence::default(),
        Some(&built),
        None,
    )
    .unwrap();
    assert!(done.ok(), "{:?}", done.violations);
    let closed = closing.merge(
        &host,
        &MergeGate {
            forge: Some(&red),
            ..MergeGate::default()
        },
    );
    assert_eq!(rules(&closed), vec!["merge-gate"]);
    assert!(
        closed.violations[0].message.contains("exception closed"),
        "{:?}",
        closed.violations
    );
    sync(&host, &red, &closing.root).unwrap();
    sync(&host, &red, &closing.root).unwrap();
    let journal = fs::read_to_string(closing.root.join("JOURNAL.md")).unwrap();
    assert_eq!(
        journal.matches("- known-red 01-a/u/S1: `lab` at").count(),
        1,
        "{journal}"
    );
    assert!(journal.contains("closed, 01-a/u/build holds"), "{journal}");

    let waiver = closing.write("waiver.md", "Owner: merge anyway.\n");
    let waived = closing.merge(
        &host,
        &MergeGate {
            forge: Some(&red_at_head(&head, "failure", "success")),
            waiver: Some(waiver),
            ..MergeGate::default()
        },
    );
    assert!(waived.ok(), "{:?}", waived.violations);
    let linted = lint(&closing.root).unwrap();
    assert!(linted.ok(), "{:?}", linted.violations);
    assert_eq!(
        linted.fields["warnings"].as_array().unwrap().len(),
        0,
        "an exception names lab at this head"
    );
}

#[test]
fn lint_warns_on_an_override_of_a_red_check_no_exception_names() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    accepted(&host, &c);
    let head = c.head();
    let waiver = c.write("waiver.md", "Owner: merge anyway.\n");
    let waived = c.merge(
        &host,
        &MergeGate {
            forge: Some(&red_at_head(&head, "failure", "failure")),
            waiver: Some(waiver),
            ..MergeGate::default()
        },
    );
    assert!(waived.ok(), "{:?}", waived.violations);
    let linted = lint(&c.root).unwrap();
    assert!(linted.ok(), "{:?}", linted.violations);
    let warnings = linted.fields["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .contains("waived red check `lab`"),
        "{warnings:?}"
    );
}

#[test]
fn standing_known_red_accepts_a_new_head_only_while_the_base_check_fails() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    accepted(&host, &c);
    let head = c.head();
    let red = red_at_head(&head, "failure", "failure");
    assert!(known_red(&host, &c, &red, "", "red by design", "build").ok());
    let units = cm::plan::load_units(&c.root).unwrap();
    let exceptions = &units["01-a/u"].steps["S1"].known_red;
    let changed = "e".repeat(40);
    let check = cm::forge::CheckState {
        name: "lab".into(),
        state: cm::forge::Conclusion::Failure,
    };
    assert!(
        cm::holds::cover(
            exceptions,
            &units,
            &changed,
            &check,
            Some(std::slice::from_ref(&check)),
            Some(std::slice::from_ref(&check))
        )
        .is_ok()
    );
    let green = cm::forge::CheckState {
        name: "lab".into(),
        state: cm::forge::Conclusion::Success,
    };
    assert_eq!(
        cm::holds::cover(exceptions, &units, &changed, &check, None, Some(&[green])),
        Err(cm::holds::Uncovered::GreenAtBase)
    );
    let refused = c.merge(
        &host,
        &MergeGate {
            forge: Some(&red_at_head(&head, "failure", "success")),
            ..MergeGate::default()
        },
    );
    assert_eq!(rules(&refused), vec!["merge-gate"]);
    assert!(
        c.merge(
            &host,
            &MergeGate {
                forge: Some(&red),
                ..MergeGate::default()
            }
        )
        .ok()
    );
}

#[test]
fn campaign_standing_exception_covers_another_step_in_advance_sync_and_lint() {
    let host = at("2026-10-26T08:00:00Z");
    for syncing in [false, true] {
        let c = campaign(&host, true);
        accepted(&host, &c);
        let head = c.head();
        let red = red_at_head(&head, "failure", "failure");
        assert!(known_red(&host, &c, &red, "", "red by design", "build").ok());
        let dir = c.root.join("01-a/u");
        let mut unit = store::load_unit(&dir).unwrap();
        let exception = unit.steps.get_mut("S1").unwrap().known_red.remove(0);
        unit.steps
            .get_mut("build")
            .unwrap()
            .known_red
            .push(exception);
        commands::write_unit(&dir, &unit).unwrap();
        assert_eq!(
            rules(&c.merge(
                &host,
                &MergeGate {
                    forge: Some(&red),
                    ..MergeGate::default()
                }
            )),
            vec!["merge-gate"]
        );
        unit.steps.get_mut("build").unwrap().known_red[0].campaign = true;
        commands::write_unit(&dir, &unit).unwrap();
        let waiver = c.write("waiver.md", "Owner: red by design.\n");
        unit.steps
            .get_mut("S1")
            .unwrap()
            .waived
            .push(cm::state::Waived {
                commit: "e".repeat(40),
                check: "lab".into(),
                file: cm::evidence::CampaignFile::resolve(&c.root, &waiver).unwrap(),
                at: "2026-10-26T08:00:00Z".into(),
            });
        commands::write_unit(&dir, &unit).unwrap();
        let linted = lint(&c.root).unwrap();
        assert_eq!(linted.fields["warnings"].as_array().unwrap().len(), 0);
        if syncing {
            let merged = FakeForge {
                state: PullState::Merged,
                ..red
            };
            let result = sync(&host, &merged, &c.root).unwrap();
            assert!(result.ok(), "{:?}", result.violations);
        } else {
            let result = c.merge(
                &host,
                &MergeGate {
                    forge: Some(&red),
                    ..MergeGate::default()
                },
            );
            assert!(result.ok(), "{:?}", result.violations);
        }
        assert_eq!(c.state(), Phase::Merged);
    }
}

#[test]
fn standing_registration_and_both_merge_gates_use_the_latest_completed_base_check() {
    let host = at("2026-10-26T08:00:00Z");
    for syncing in [false, true] {
        let c = campaign(&host, true);
        accepted(&host, &c);
        let head = c.head();
        let base = "b".repeat(40);
        let earlier = "c".repeat(40);
        let gh = support::FakeGh::new(&format!(
            r#"case "$2" in
  repos/o/r/pulls/7) cat "$FAKE_GH/pull.json" ;;
  repos/o/r/commits/{head}/check-runs\?*) cat "$FAKE_GH/head.json" ;;
  repos/o/r/commits/{base}/check-runs\?status=*) echo '{{"total_count":0,"check_runs":[]}}' ;;
  repos/o/r/commits/{base}/check-runs\?*) echo '{{"total_count":1,"check_runs":[{{"name":"lab","status":"in_progress","conclusion":null}}]}}' ;;
  repos/o/r/commits/{base}) echo '{{"parents":[{{"sha":"{earlier}"}},{{"sha":"side"}}]}}' ;;
  repos/o/r/commits/{earlier}/check-runs\?status=*) cat "$FAKE_GH/earlier.json" ;;
  */statuses\?*) echo '[]' ;;
  */status\?*) echo '{{"total_count":0,"statuses":[]}}' ;;
  *) exit 1 ;;
esac
"#
        ));
        let pull = serde_json::json!({ "state": "open", "merged": false, "head": {"sha": head}, "base": {"sha": base} });
        fs::write(gh.fixture().join("pull.json"), pull.to_string()).unwrap();
        fs::write(gh.fixture().join("head.json"), r#"{"total_count":1,"check_runs":[{"name":"lab","status":"completed","conclusion":"failure"}]}"#).unwrap();
        let completed = |state| {
            serde_json::json!({"total_count": 2, "check_runs": [
                {"id": 1, "name": "lab", "status": "completed", "conclusion": "failure"},
                {"id": 2, "name": "lab", "status": "completed", "conclusion": state}
            ]})
            .to_string()
        };
        let forge = Gh {
            program: gh.program(),
            timeout: Duration::from_secs(10),
        };
        let previous = gh.fixture().join("earlier.json");
        fs::write(&previous, completed("success")).unwrap();
        assert_eq!(
            rules(&known_red(&host, &c, &forge, "", "red by design", "build")),
            vec!["known-red-refused"]
        );
        fs::write(&previous, completed("failure")).unwrap();
        assert_eq!(
            rules(&known_red(
                &host,
                &c,
                &forge,
                &head,
                "red by design",
                "build"
            )),
            vec!["known-red-refused"]
        );
        let registered = known_red(&host, &c, &forge, "", "red by design", "build");
        assert!(registered.ok(), "{:?}", registered.violations);
        if syncing {
            let mut merged = pull.clone();
            merged["merged"] = true.into();
            merged["merge_commit_sha"] = head.clone().into();
            fs::write(gh.fixture().join("pull.json"), merged.to_string()).unwrap();
        }
        let gate = || {
            if syncing {
                sync(&host, &forge, &c.root).unwrap()
            } else {
                c.merge(
                    &host,
                    &MergeGate {
                        forge: Some(&forge),
                        ..MergeGate::default()
                    },
                )
            }
        };
        for state in ["success", "cancelled", "timed_out"] {
            fs::write(&previous, completed(state)).unwrap();
            assert_eq!(rules(&gate()), vec!["merge-gate"], "{state}");
            assert_eq!(c.state(), Phase::PrOpen);
        }
        fs::write(&previous, "unavailable").unwrap();
        assert_eq!(rules(&gate()), vec!["merge-gate"]);
        fs::write(&previous, completed("failure")).unwrap();
        let merged = gate();
        assert!(merged.ok(), "{:?}", merged.violations);
        assert_eq!(c.state(), Phase::Merged);
    }
}

#[test]
fn standing_registration_tries_each_parts_pr_without_changing_commit_selection() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    accepted(&host, &c);
    let amendment = commands::Amendment {
        parts: vec!["one".into(), "two".into()],
        ..Default::default()
    };
    assert!(
        commands::amend(&host, &c.root, &target(), &amendment)
            .unwrap()
            .ok()
    );
    let second_pr = "https://github.com/o/r/pull/8";
    let head = c.head();
    let second_head = commit(&c.repo, "second.txt", "second part\n");
    let opened = commands::advance_gated(
        &host,
        &c.root,
        &target(),
        commands::Advance {
            to: &Phase::PrOpen,
            given: &Evidence {
                pr: Some(second_pr.into()),
                sha: Some(second_head.clone()),
                ..Default::default()
            },
            file: None,
            repo: Some(&c.repo),
            part: Some("two"),
            by: &[],
        },
        &MergeGate::default(),
    )
    .unwrap();
    assert!(opened.ok(), "{:?}", opened.violations);
    let mut forge = red_at_head(&head, "success", "failure");
    forge.parts.insert(
        second_pr.into(),
        red_at_head(&second_head, "failure", "failure"),
    );
    assert_eq!(
        rules(&known_red(
            &host,
            &c,
            &forge,
            &head,
            "red by design",
            "build"
        )),
        vec!["known-red-refused"]
    );
    assert!(known_red(&host, &c, &forge, &second_head, "red by design", "build").ok());
    let registered = known_red(&host, &c, &forge, "", "red by design", "build");
    assert!(registered.ok(), "{:?}", registered.violations);
    assert_eq!(registered.fields["exception"]["standing"], true);
    let mut green = red_at_head(&head, "success", "failure");
    green.parts.insert(
        second_pr.into(),
        red_at_head(&second_head, "success", "failure"),
    );
    assert!(!known_red(&host, &c, &green, "", "red by design", "build").ok());
}

#[test]
fn cli_step_review_wait_on_multipart_work_has_no_part_identity() {
    let host = at("2026-10-25T00:00Z");
    let c = review_fixture::multipart_campaign(&host);
    let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    let step = &unit.steps["S1"];
    assert_eq!(step.parts.len(), 2);
    assert!(step.track.evidence.pr.is_some());
    assert!(step.track.evidence.sha.is_some());
    assert_eq!(step.track.evidence.pr, step.parts["two"].evidence.pr);
    let blocked = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["--json", "advance", STEP, "blocked:REVIEW_BLOCKED"])
        .output()
        .unwrap();
    assert!(blocked.status.success(), "{blocked:?}");
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["--json", "next"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let waits = json["review_waits"].as_array().unwrap();
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0]["target"], STEP);
    assert_eq!(waits[0]["reason"], "REVIEW_BLOCKED");
    assert!(waits[0]["part"].is_null());
    assert!(waits[0]["pr"].is_null());
    assert!(waits[0]["head"].is_null());
}

#[allow(dead_code)]
#[path = "support/publication.rs"]
mod publication_fixture;

#[path = "support/campaign.rs"]
mod campaign_fixture;
#[allow(dead_code)]
#[path = "support/forge.rs"]
mod forge_fixture;

#[test]
fn hand_release_checks_publication_for_merged_tag_only_and_resumed_steps() {
    use cm::forge::Read;
    use publication_fixture as p;
    let host = at("2026-10-25T06:00:00Z");
    for phase in [
        Phase::Merged,
        Phase::Drafted,
        Phase::Launched,
        Phase::Blocked("retry".into()),
    ] {
        let tag_only = matches!(phase, Phase::Drafted | Phase::Launched);
        let resuming = matches!(phase, Phase::Blocked(_));
        let c = p::campaign(
            &host,
            if resuming { Phase::Released } else { phase },
            &p::ARTIFACTS,
        );
        if resuming {
            let mut unit = p::unit(&c);
            let track = &mut unit.steps.get_mut("1.4.0").unwrap().track;
            track.evidence.tag = Some(p::TAG.into());
            track.apply(
                Phase::Blocked("retry".into()),
                "2026-10-25T06:00Z",
                &Evidence::default(),
            );
            commands::write_unit(&c.root.join("00-release/widget"), &unit).unwrap();
        }
        let before = c.files();
        for read in [Read::Missing, Read::Yanked, Read::Stale("1.3.0".into())] {
            let mut forge = p::present();
            forge
                .crates
                .insert(("acme-sdk".into(), "1.4.0".into()), read);
            let out = p::advance(&host, &c, &forge, tag_only, resuming);
            assert_eq!(rules(&out), ["release-unpublished"]);
            assert_eq!(c.files(), before);
        }
        let mut forge = p::present();
        forge.root = Some(c.root.clone());
        forge.broken.insert("image:ghcr.io/acme/widget".into());
        let out = p::advance(&host, &c, &forge, tag_only, resuming);
        assert_eq!(rules(&out), ["publication-unread"]);
        assert_eq!(c.files(), before);
        let out = p::advance(&host, &c, &p::present(), tag_only, resuming);
        assert!(out.ok(), "{:?}", out.violations);
        assert_eq!(p::step(&c).track.state, Phase::Released);
        assert_eq!(p::step(&c).track.evidence.published.len(), 3);
    }
}

#[test]
fn a_forge_without_publication_support_refuses_the_hand_release_as_unread() {
    use publication_fixture as p;
    let host = at("2026-10-25T06:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    let before = c.files();
    let mut forge = FakeForge::open(p::SHA, &[]);
    forge
        .tags
        .insert(("o/widget".into(), p::TAG.into()), [p::SHA.into()].into());
    let out = p::advance(&host, &c, &forge, false, false);
    assert_eq!(rules(&out), ["publication-unread"]);
    assert_eq!(c.files(), before);
}

#[test]
fn unsupported_tap_content_has_a_formula_error() {
    use publication_fixture as p;
    for body in [
        p::formula(p::TAG).replace(r#"version "1.4.0""#, r#"version("1.4.0".sub("4", "3"))"#),
        p::formula(p::TAG).replace(p::TAG, "v#{version}"),
    ] {
        let response = p::formula_response(&body);
        let gh = Gh {
            program: response.program(),
            timeout: Duration::from_secs(10),
        };
        let result = gh.tap_formula(
            &"o/homebrew-tap".parse().unwrap(),
            "widget",
            &"o/widget".parse().unwrap(),
            &p::TAG.parse().unwrap(),
            &"1.4.0".parse().unwrap(),
        );
        assert!(
            matches!(result, Err(ForgeError::UnsupportedFormula)),
            "{result:?}"
        );
    }
}

#[test]
fn review_wait_observes_a_concurrent_record_and_distinguishes_deadline_and_unreadable() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let report = c.report("wait", &head, &[], "ACCEPT");
    let spawn = |reviewer: &str, deadline: &str| {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&c.root)
            .args([
                "--json",
                "review",
                "wait",
                STEP,
                "--head",
                &head,
                "--reviewer",
                reviewer,
                "--deadline",
                deadline,
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let deadline = (jiff::Timestamp::now() + jiff::SignedDuration::from_secs(10)).to_string();
    let mut child = spawn("astra", &deadline);
    std::thread::sleep(Duration::from_millis(100));
    assert!(child.try_wait().unwrap().is_none());
    assert!(
        verdict::record(&host, &c.root, &target(), &report, Some("astra"), None)
            .unwrap()
            .ok()
    );
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["wait_status"], "recorded");
    assert_eq!(json["review"]["reviewer"], "astra");
    let output = spawn("other", &jiff::Timestamp::now().to_string())
        .wait_with_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["wait_status"], "deadline");
    c.write(".cm/verdicts.json", "broken JSON");
    let output = spawn("astra", &deadline).wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(4));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["wait_status"], "unreadable");
}

#[test]
fn review_wait_validation_exits_two_and_state_decode_failures_exit_four() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let deadline = jiff::Timestamp::now().to_string();
    let run = |step: &str, head: &str, reviewer: &str, part: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        command.arg("-C").arg(&c.root).args([
            "--json",
            "review",
            "wait",
            step,
            "--head",
            head,
            "--reviewer",
            reviewer,
            "--deadline",
            &deadline,
        ]);
        if let Some(part) = part {
            command.args(["--part", part]);
        }
        command.output().unwrap()
    };
    assert_eq!(
        run(STEP, review_fixture::INVALID_WAIT_HEAD, "astra", None)
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        run(STEP, &head, review_fixture::INVALID_WAIT_REVIEWER, None)
            .status
            .code(),
        Some(2)
    );
    for step in review_fixture::UNKNOWN_WAIT_STEPS {
        let output = run(step, &head, "astra", None);
        assert_eq!(output.status.code(), Some(2));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["wait_status"], "refused");
        assert_eq!(json["violations"][0]["rule"], "step-unknown");
    }
    let output = run(STEP, &head, "astra", Some("missing"));
    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["wait_status"], "refused");
    assert_eq!(json["violations"][0]["rule"], "part-unknown");
    c.write("01-a/u/.cm/state.json", "not JSON");
    let output = run(STEP, &head, "astra", None);
    assert_eq!(output.status.code(), Some(4));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["wait_status"], "unreadable");
}

#[test]
fn sync_surfaces_a_moved_base_since_ci_and_refreshes_only_at_a_tested_new_head() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let mut forge = FakeForge::open(&head, &[("test", "success")]);
    forge.campaign = Some(c.root.clone());
    forge.base = Some("b".repeat(40));
    assert!(sync(&host, &forge, &c.root).unwrap().ok());
    assert_eq!(
        plan::next_at(&c.root, None, None, host.now())
            .unwrap()
            .fields["stale_base"],
        serde_json::json!([])
    );
    forge.base = Some("c".repeat(40));
    assert!(sync(&host, &forge, &c.root).unwrap().ok());
    let next = plan::next_at(&c.root, None, None, host.now()).unwrap();
    assert!(next.ok());
    assert_eq!(next.fields["stale_base"][0]["ci_head"], head);
    assert_eq!(next.fields["stale_base"][0]["ci_base"], "b".repeat(40));
    assert_eq!(next.fields["stale_base"][0]["base"], "c".repeat(40));
    let linted = cm::lint::lint_at(&c.root, host.now()).unwrap();
    assert!(linted.ok(), "{:?}", linted.violations);
    assert!(
        linted.fields["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().starts_with("stale-base "))
    );
    let mut before: serde_json::Value = store::read_json(&plan::pulls_path(&c.root)).unwrap();
    forge.down = true;
    assert!(!sync(&host, &forge, &c.root).unwrap().ok());
    let after: serde_json::Value = store::read_json(&plan::pulls_path(&c.root)).unwrap();
    // An unread PR keeps its observation, but its CI is unknown now, never the last green.
    assert_eq!(before[PR]["ci_green"], true);
    before[PR].as_object_mut().unwrap().remove("ci_green");
    assert_eq!(before, after);
    forge.down = false;
    forge.head = "d".repeat(40);
    forge.checks = vec![("test", "in_progress")];
    assert!(sync(&host, &forge, &c.root).unwrap().ok());
    assert_eq!(
        plan::next_at(&c.root, None, None, host.now())
            .unwrap()
            .fields["stale_base"][0]["ci_head"],
        head
    );
    forge.checks = vec![("test", "success")];
    assert!(sync(&host, &forge, &c.root).unwrap().ok());
    assert_eq!(
        plan::next_at(&c.root, None, None, host.now())
            .unwrap()
            .fields["stale_base"],
        serde_json::json!([])
    );
}

#[test]
fn owner_queue_banner_persists_until_explicit_journaled_read_and_reappears_for_new_items() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    let forge = FakeForge::open(&c.head(), &[("test", "success")]);
    review_fixture::new_owner_item(&host, &c.root, "Choose a setting");
    let next = plan::next_at(&c.root, None, None, host.now()).unwrap();
    assert_eq!(
        next.lines[0],
        "NEW OWNER-QUEUE since last read: 1. Choose a setting"
    );
    let synced = sync(&host, &forge, &c.root).unwrap();
    assert_eq!(
        synced.fields["owner_queue_unread"],
        serde_json::json!(["1. Choose a setting"])
    );
    assert!(
        commands::journal(&host, &c.root, "inspect queue", None)
            .unwrap()
            .ok()
    );
    let state: serde_json::Value = store::read_json(&store::state_path(&c.root)).unwrap();
    assert!(state["owner_queue_read"].is_null());
    let output = review_fixture::owner_read(&c.root);
    assert!(output.status.success(), "{output:?}");
    let read: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        read["acknowledged"],
        serde_json::json!(["1. Choose a setting"])
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let headings = cm::markdown::headings(&journal);
    assert_eq!(
        format!("## {}", headings.last().unwrap().text),
        read["journal"]
    );
    assert_eq!(
        plan::next_at(&c.root, None, None, host.now())
            .unwrap()
            .fields["owner_queue_unread"],
        serde_json::json!([])
    );
    assert_eq!(
        sync(&host, &forge, &c.root).unwrap().fields["owner_queue_unread"],
        serde_json::json!([])
    );
    review_fixture::new_owner_item(&host, &c.root, "Second decision");
    assert_eq!(
        plan::next_at(&c.root, None, None, host.now())
            .unwrap()
            .fields["owner_queue_unread"],
        serde_json::json!(["2. Second decision"])
    );
}

#[test]
fn unread_owner_queue_refuses_merge_even_with_override_then_allows_it_after_read() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let report = c.report("astra-review-owner", &head, &[], "ACCEPT");
    c.receipt("astra-review-owner", &head, "gpt-6-astra", "xhigh");
    assert!(
        verdict::record(&host, &c.root, &target(), &report, Some("astra"), None)
            .unwrap()
            .ok()
    );
    claude_accept(&host, &c, &head);
    let forge = FakeForge::open(&head, &[("test", "success")]);
    let waiver = c.write("override.md", "Independent gate evidence.\n");
    let advance = || {
        commands::advance_gated(
            &host,
            &c.root,
            &target(),
            commands::Advance {
                to: &Phase::Merged,
                given: &Evidence {
                    sha: Some(head.clone()),
                    ..Default::default()
                },
                file: None,
                repo: Some(&c.repo),
                part: None,
                by: &[],
            },
            &MergeGate {
                forge: Some(&forge),
                waiver: Some(waiver.clone()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    review_fixture::new_owner_item(&host, &c.root, "Choose a setting");
    assert_eq!(rules(&advance()), vec!["owner-queue-unread"]);
    assert_eq!(c.state(), Phase::PrOpen);
    assert!(review_fixture::owner_read(&c.root).status.success());
    assert!(advance().ok());
    assert_eq!(c.state(), Phase::Merged);
}

#[test]
fn sync_refuses_an_unread_owner_merge_then_moves_it_after_read() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let report = c.report("astra-review-owner", &head, &[], "ACCEPT");
    c.receipt("astra-review-owner", &head, "gpt-6-astra", "xhigh");
    assert!(
        verdict::record(&host, &c.root, &target(), &report, Some("astra"), None)
            .unwrap()
            .ok()
    );
    claude_accept(&host, &c, &head);
    let mut forge = FakeForge::open(&head, &[("test", "success")]);
    forge.state = PullState::Merged;
    review_fixture::new_owner_item(&host, &c.root, "Choose a setting");
    let refused = sync(&host, &forge, &c.root).unwrap();
    assert_eq!(rules(&refused), vec!["owner-queue-unread"]);
    assert_eq!(refused.fields["moves"], serde_json::json!([]));
    assert_eq!(c.state(), Phase::PrOpen);
    assert!(review_fixture::owner_read(&c.root).status.success());
    assert!(sync(&host, &forge, &c.root).unwrap().ok());
    assert_eq!(c.state(), Phase::Merged);
}

#[test]
fn owner_queue_read_failure_does_not_acknowledge_and_legacy_headings_are_readable() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    c.write("OWNER-QUEUE.md", review_fixture::LEGACY_QUEUE);
    let next = plan::next_at(&c.root, None, None, host.now()).unwrap();
    assert_eq!(
        next.fields["owner_queue_unread"],
        serde_json::json!(["Choose a setting"])
    );
    fs::create_dir(c.root.join(".JOURNAL.md.cm-tmp")).unwrap();
    assert_eq!(review_fixture::owner_read(&c.root).status.code(), Some(1));
    let state: serde_json::Value = store::read_json(&store::state_path(&c.root)).unwrap();
    assert!(state["owner_queue_read"].is_null());
    fs::remove_dir(c.root.join(".JOURNAL.md.cm-tmp")).unwrap();
    assert!(review_fixture::owner_read(&c.root).status.success());
    c.write(
        "OWNER-QUEUE.md",
        &review_fixture::LEGACY_QUEUE.replace("Context: A decision.", "Context: Revised decision."),
    );
    assert_eq!(
        plan::next_at(&c.root, None, None, host.now())
            .unwrap()
            .fields["owner_queue_unread"],
        serde_json::json!([])
    );
}

#[test]
fn invalid_missing_and_unreadable_queues_do_not_invent_new_merge_refusals() {
    for kind in ["missing", "invalid", "unreadable"] {
        let host = at("2026-10-25T00:00Z");
        let c = campaign(&host, true);
        let dir = c.root.join("01-a/u");
        let mut unit = store::load_unit(&dir).unwrap();
        unit.steps.get_mut("S1").unwrap().lane = Some(Lane::Pi);
        commands::write_unit(&dir, &unit).unwrap();
        let path = c.root.join("OWNER-QUEUE.md");
        match kind {
            "missing" => fs::remove_file(path).unwrap(),
            "invalid" => {
                c.write("OWNER-QUEUE.md", review_fixture::INVALID_QUEUE);
            }
            _ => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(path).unwrap();
            }
        }
        assert_eq!(
            plan::next_at(&c.root, None, None, host.now())
                .unwrap()
                .fields["owner_queue_unread"],
            serde_json::json!([])
        );
        let moved = commands::advance(
            &host,
            &c.root,
            &target(),
            &Phase::Merged,
            &Evidence {
                sha: Some(c.head()),
                ..Default::default()
            },
            None,
            Some(&c.repo),
        )
        .unwrap();
        assert!(moved.ok(), "{kind}: {:?}", moved.violations);
    }
}

#[test]
fn unread_owner_items_refuse_release_launch_hand_release_and_sync_until_read() {
    use publication_fixture as p;
    let host = at("2026-10-25T00:00Z");
    for phase in [Phase::Drafted, Phase::Merged] {
        let c = p::campaign(&host, phase.clone(), &[]);
        let forge = p::present();
        review_fixture::new_owner_item(&host, &c.root, "Choose a setting");
        assert_eq!(
            rules(&p::advance(
                &host,
                &c,
                &forge,
                phase == Phase::Drafted,
                false
            )),
            vec!["owner-queue-unread"]
        );
        if phase == Phase::Drafted {
            let launch = commands::launch_remote(
                &host,
                &c.root,
                &parse_target(p::STEP, true).unwrap(),
                None,
                &Remote::default(),
            )
            .unwrap();
            assert_eq!(rules(&launch), vec!["owner-queue-unread"]);
        } else {
            assert_eq!(
                rules(&sync(&host, &forge, &c.root).unwrap()),
                vec!["owner-queue-unread"]
            );
        }
        assert!(review_fixture::owner_read(&c.root).status.success());
        if phase == Phase::Merged {
            assert!(sync(&host, &forge, &c.root).unwrap().ok());
        } else {
            let launch = commands::launch_remote(
                &host,
                &c.root,
                &parse_target(p::STEP, true).unwrap(),
                None,
                &Remote::default(),
            )
            .unwrap();
            assert!(launch.ok(), "{:?}", launch.violations);
            assert!(p::advance(&host, &c, &forge, true, false).ok());
        }
        assert_eq!(p::step(&c).track.state, Phase::Released);
    }
}

#[test]
fn unread_owner_items_hold_parts_and_automatic_parent_merge_rollups() {
    let host = at("2026-10-25T00:00Z");
    let c = review_fixture::multipart_campaign(&host);
    review_fixture::new_owner_item(&host, &c.root, "Choose a setting");
    let moved = commands::advance_gated(
        &host,
        &c.root,
        &target(),
        commands::Advance {
            to: &Phase::Merged,
            given: &Evidence {
                sha: Some(c.head()),
                ..Default::default()
            },
            file: None,
            repo: Some(&c.repo),
            part: Some("one"),
            by: &[],
        },
        &MergeGate::default(),
    )
    .unwrap();
    assert_eq!(rules(&moved), vec!["owner-queue-unread"]);
    let dir = c.root.join("01-a/u");
    let mut unit = store::load_unit(&dir).unwrap();
    let step = unit.steps.get_mut("S1").unwrap();
    for part in step.parts.values_mut() {
        part.state = Phase::Merged;
    }
    assert!(
        step.roll_up_all(false, "2026-10-25T00:00Z", &Evidence::default(), &c.root)
            .is_empty()
    );
    commands::write_unit(&dir, &unit).unwrap();
    let forge = FakeForge::open(&c.head(), &[]);
    assert_eq!(
        rules(&sync(&host, &forge, &c.root).unwrap()),
        vec!["owner-queue-unread"]
    );
    assert_eq!(c.state(), Phase::PrOpen);
    assert!(review_fixture::owner_read(&c.root).status.success());
    assert!(sync(&host, &forge, &c.root).unwrap().ok());
    assert_eq!(c.state(), Phase::Merged);
}

#[test]
fn review_wait_follows_a_pr_opened_while_waiting() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, false);
    let head = c.head();
    let report = c.report("wait-for-pr", &head, &[], "ACCEPT");
    let deadline = (jiff::Timestamp::now() + jiff::SignedDuration::from_secs(10)).to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args([
            "--json",
            "review",
            "wait",
            STEP,
            "--head",
            &head,
            "--reviewer",
            "astra",
            "--deadline",
            &deadline,
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    assert!(child.try_wait().unwrap().is_none());
    let opened = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args([
            "--json", "advance", STEP, "pr-open", "--pr", PR, "--sha", &head, "--repo",
        ])
        .arg(&c.repo)
        .output()
        .unwrap();
    assert!(opened.status.success(), "{opened:?}");
    let recorded = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["--json", "review", "record", STEP])
        .arg(&report)
        .args(["--reviewer", "astra"])
        .output()
        .unwrap();
    assert!(recorded.status.success(), "{recorded:?}");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["wait_status"], "recorded");
    assert_eq!(json["review"]["pr"], PR);
    assert_eq!(json["review"]["head"], head);
}

#[test]
fn review_wait_distinguishes_bad_campaign_state_from_a_missing_campaign() {
    let host = at("2026-10-25T00:00Z");
    for kind in 0..4 {
        let c = campaign(&host, true);
        let head = c.head();
        let path = store::state_path(&c.root);
        match kind {
            0 | 1 => fs::write(&path, review_fixture::MALFORMED_CAMPAIGN_STATES[kind]).unwrap(),
            2 => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            _ => fs::remove_file(&path).unwrap(),
        }
        for start in [&c.root, &c.root.join("01-a/u")] {
            let output = Command::new(env!("CARGO_BIN_EXE_cm"))
                .arg("-C")
                .arg(start)
                .args([
                    "--json",
                    "review",
                    "wait",
                    STEP,
                    "--head",
                    &head,
                    "--reviewer",
                    "astra",
                    "--deadline",
                    &jiff::Timestamp::now().to_string(),
                ])
                .output()
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if kind == 3 {
                assert_eq!(output.status.code(), Some(2), "{output:?}");
                assert_eq!(json["wait_status"], "refused");
                assert_eq!(json["violations"][0]["rule"], "no-campaign");
            } else {
                assert_eq!(output.status.code(), Some(4), "{output:?}");
                assert_eq!(json["wait_status"], "unreadable");
                assert_eq!(json["violations"][0]["rule"], "review-wait-unreadable");
            }
        }
    }
}

#[test]
fn review_wait_does_not_match_an_outer_round_through_a_malformed_inner_campaign() {
    let host = at("2026-10-25T00:00Z");
    let outer = campaign(&host, true);
    let head = outer.head();
    let report = outer.report("outer-round", &head, &[], "ACCEPT");
    assert!(
        verdict::record(&host, &outer.root, &target(), &report, Some("astra"), None)
            .unwrap()
            .ok()
    );
    let run = |start: &Path| {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(start)
            .args([
                "--json",
                "review",
                "wait",
                STEP,
                "--head",
                &head,
                "--reviewer",
                "astra",
                "--deadline",
                &jiff::Timestamp::now().to_string(),
            ])
            .output()
            .unwrap()
    };
    assert_eq!(run(&outer.root).status.code(), Some(0));
    for kind in 0..3 {
        let inner = campaign(&host, true);
        let nested = outer.root.join(format!("inner-{kind}"));
        fs::rename(&inner.root, &nested).unwrap();
        let path = store::state_path(&nested);
        match kind {
            0 | 1 => fs::write(&path, review_fixture::MALFORMED_CAMPAIGN_STATES[kind]).unwrap(),
            _ => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
        }
        for start in [&nested, &nested.join("01-a/u")] {
            let output = run(start);
            assert_eq!(output.status.code(), Some(4), "{output:?}");
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(json["wait_status"], "unreadable");
            assert_eq!(json["violations"][0]["rule"], "review-wait-unreadable");
        }
    }
}

#[test]
fn review_owed_and_merge_ready_follow_the_observed_head_and_its_verdicts() {
    let host = at("2026-10-25T00:00Z");
    let c = campaign(&host, true);
    let owed = |c: &Campaign, forge: Option<&dyn Forge>| -> Vec<(String, String)> {
        plan::next_with(&c.root, None, None, host.now(), forge)
            .unwrap()
            .fields["actionable"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|action| {
                ["review-owed", "merge-ready"].contains(&action["kind"].as_str().unwrap())
            })
            .map(|action| {
                (
                    action["kind"].as_str().unwrap().to_owned(),
                    action["command"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let brief = ("review-owed".to_owned(), format!("cm brief {STEP}"));
    assert_eq!(owed(&c, None), std::slice::from_ref(&brief));
    let fix = c.report(
        "r1",
        &c.head(),
        &["F1 | contract | introduced | src/lib.rs:40 | the refusal drops its reason"],
        "FIX REQUIRED",
    );
    assert!(record(&host, &c, &fix, "astra").ok());
    assert_eq!(
        owed(&c, None),
        [(
            "review-owed".to_owned(),
            format!(
                "cm attention --pr {PR} --worker '<worker-session>' --coordinator '<coordinator-session>'"
            )
        )]
    );
    // The worker's new head, observed green by sync, is owed its own review.
    git(&c.repo, &["commit", "-q", "--allow-empty", "-m", "fix"]);
    let mut forge = FakeForge::open(&c.head(), &[("test", "success")]);
    forge.campaign = Some(c.root.clone());
    forge.base = Some("b".repeat(40));
    assert!(sync(&host, &forge, &c.root).unwrap().ok());
    let seen: serde_json::Value = store::read_json(&plan::pulls_path(&c.root)).unwrap();
    assert_eq!(seen[PR]["ci_green"], true);
    assert_eq!(owed(&c, Some(&forge)), std::slice::from_ref(&brief));
    accepted(&host, &c);
    let next = plan::next_with(&c.root, None, None, host.now(), Some(&forge)).unwrap();
    let ready = next.fields["actionable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["kind"] == "merge-ready")
        .unwrap()
        .clone();
    assert_eq!(ready["lane"], "codex");
    assert_eq!(ready["weight"], "complex");
    assert_eq!(ready["pr"], PR);
    assert_eq!(ready["head"], c.head());
    assert_eq!(ready["base"], "b".repeat(40));
    assert_eq!(ready["ci_green"], true);
    assert_eq!(
        owed(&c, Some(&forge)),
        [(
            "merge-ready".to_owned(),
            format!("cm advance {STEP} merged --pr {PR} --sha '<merge-sha>'")
        )]
    );
    // A red head is owed its review and is not merge-ready.
    git(&c.repo, &["commit", "-q", "--allow-empty", "-m", "again"]);
    forge.head = c.head();
    forge.checks = vec![("test", "failure")];
    sync(&host, &forge, &c.root).unwrap();
    let seen: serde_json::Value = store::read_json(&plan::pulls_path(&c.root)).unwrap();
    assert_eq!(seen[PR]["ci_green"], false);
    assert_eq!(owed(&c, Some(&forge)), [brief]);
}

/// The `merge-ready` commands `cm next` gives with `forge` as GitHub for its fresh read.
fn merge_ready(c: &Campaign, now: Timestamp, forge: &dyn Forge) -> Vec<String> {
    plan::next_with(&c.root, None, None, now, Some(forge))
        .unwrap()
        .fields["actionable"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["kind"] == "merge-ready")
        .map(|action| action["command"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn merge_ready_reads_ci_on_every_sync_and_needs_a_clean_mergeable_head() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    accepted(&host, &c);
    let head = c.head();
    let ready = format!("cm advance {STEP} merged --pr {PR} --sha '<merge-sha>'");
    let mut forge = FakeForge::open(&head, &[("test", "success")]);
    forge.campaign = Some(c.root.clone());
    forge.base = Some("b".repeat(40));
    let synced = |forge: &FakeForge| {
        sync(&host, forge, &c.root).unwrap();
        store::read_json::<serde_json::Value>(&plan::pulls_path(&c.root)).unwrap()[PR]["ci_green"]
            .clone()
    };
    assert_eq!(synced(&forge), true);
    assert_eq!(
        merge_ready(&c, host.now(), &forge),
        std::slice::from_ref(&ready)
    );
    // A check registered later at the same head makes the green stale.
    forge.checks = vec![("test", "success"), ("late", "in_progress")];
    assert_eq!(synced(&forge), serde_json::Value::Null);
    assert!(merge_ready(&c, host.now(), &forge).is_empty());
    // Its rerun finishing green at the same head is merge-ready again.
    forge.checks = vec![("test", "success"), ("late", "success")];
    assert_eq!(synced(&forge), true);
    assert_eq!(
        merge_ready(&c, host.now(), &forge),
        std::slice::from_ref(&ready)
    );
    // A pause is the owner's stop and quiets it; the Codex quota hold holds dispatch only.
    let limits = c.root.join("LIMITS.toml");
    let text = fs::read_to_string(&limits).unwrap();
    fs::write(&limits, text.replace("paused = false", "paused = true")).unwrap();
    assert!(merge_ready(&c, host.now(), &forge).is_empty());
    fs::write(&limits, &text).unwrap();
    assert_eq!(
        merge_ready(&c, host.now(), &forge),
        std::slice::from_ref(&ready)
    );
    // The CLI confirms with a fresh read of GitHub, here a `gh` that answers for this PR.
    let gh = support::FakeGh::new(&format!(
        r#"case "$*" in
    'api repos/o/r/pulls/7')
        echo '{{"state":"open","merged":false,"merge_commit_sha":null,"mergeable_state":"clean","head":{{"sha":"{head}"}},"base":{{"sha":"{base}"}}}}' ;;
    'api repos/o/r/commits/{head}/check-runs?per_page=100&page=1')
        printf '{{"total_count":1,"check_runs":[{{"id":1,"name":"test","status":"completed","conclusion":"%s"}}]}}' "$(cat "$FAKE_GH/conclusion")" ;;
    'api repos/o/r/commits/{head}/status?per_page=100&page=1')
        echo '{{"total_count":0,"statuses":[]}}' ;;
    *) echo "$*" >&2; exit 1 ;;
esac"#,
        base = "b".repeat(40),
    ));
    let act = |quota: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&c.root)
            .args(["--json", "next", "--act"])
            .env("CM_CODEX_WEEKLY_PCT", quota)
            .env("PATH", gh.path())
            .output()
            .unwrap();
        let act: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        act["actionable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["kind"] == "merge-ready")
    };
    fs::write(gh.fixture().join("conclusion"), "success").unwrap();
    assert!(act("90"), "the Codex quota hold does not hold a merge");
    // The cached observation is green, but the head is red now: the fresh read fails closed.
    fs::write(gh.fixture().join("conclusion"), "failure").unwrap();
    assert!(!act("0"));
    let mut rerun = forge.clone();
    rerun.checks = vec![("test", "in_progress")];
    assert!(merge_ready(&c, host.now(), &rerun).is_empty());
    let down = FakeForge {
        down: true,
        ..forge.clone()
    };
    assert!(merge_ready(&c, host.now(), &down).is_empty());
    assert!(
        plan::next_at(&c.root, None, None, host.now())
            .unwrap()
            .fields["actionable"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| action["kind"] != "merge-ready"),
        "no GitHub, no merge-ready"
    );
    // A conflicted PR is not merge-ready.
    forge.mergeable = "dirty";
    synced(&forge);
    assert!(merge_ready(&c, host.now(), &forge).is_empty());
    // A red check is never merge-ready, even one a known-red exception covers: that merge
    // stays with `cm advance … merged`.
    let mut red = red_at_head(&head, "failure", "failure");
    red.campaign = Some(c.root.clone());
    let why = "red by design until build lands";
    assert!(known_red(&host, &c, &red, &head, why, "build").ok());
    assert_eq!(synced(&red), false);
    assert!(merge_ready(&c, host.now(), &forge).is_empty());
    forge.mergeable = "clean";
    assert_eq!(synced(&forge), true);
    assert_eq!(merge_ready(&c, host.now(), &forge), [ready]);
}

#[test]
fn each_part_of_a_multipart_step_is_merge_ready_on_its_own_pr() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    let amendment = commands::Amendment {
        parts: vec!["one".into(), "two".into()],
        ..Default::default()
    };
    assert!(
        commands::amend(&host, &c.root, &target(), &amendment)
            .unwrap()
            .ok()
    );
    let head = c.head();
    let second_pr = "https://github.com/o/r/pull/8";
    let second_head = commit(&c.repo, "second.txt", "second part\n");
    let opened = commands::advance_gated(
        &host,
        &c.root,
        &target(),
        commands::Advance {
            to: &Phase::PrOpen,
            given: &Evidence {
                pr: Some(second_pr.into()),
                sha: Some(second_head.clone()),
                ..Default::default()
            },
            file: None,
            repo: Some(&c.repo),
            part: Some("two"),
            by: &[],
        },
        &MergeGate::default(),
    )
    .unwrap();
    assert!(opened.ok(), "{:?}", opened.violations);
    let mut forge = FakeForge::open(&head, &[("test", "success")]);
    forge.campaign = Some(c.root.clone());
    forge.base = Some("b".repeat(40));
    let mut second = FakeForge::open(&second_head, &[("test", "success")]);
    second.base = Some("b".repeat(40));
    forge.parts.insert(second_pr.into(), second);
    sync(&host, &forge, &c.root).unwrap();
    let one = format!("cm advance {STEP} merged --pr {PR} --sha '<merge-sha>' --part one");
    assert!(merge_ready(&c, host.now(), &forge).is_empty());
    // Part one's own reviews: Astra's receipted report and a Claude reviewer's, each recorded
    // for that part. A review recorded for another part, or for none, would clear neither.
    for (reviewer, name) in [("astra", "astra-review-7-one"), ("claude", "claude-one")] {
        let report = if reviewer == "astra" {
            let report = c.report(name, &head, &[], "ACCEPT");
            c.receipt(name, &head, "gpt-6-astra", "xhigh");
            report
        } else {
            c.write(
                &format!("drafts/{name}.md"),
                &format!(
                    "# {name}\n\nRead in full.\n\n```cm-review\nhead: {head}\n```\n\nACCEPT\n"
                ),
            )
        };
        let recorded = verdict::record(
            &host,
            &c.root,
            &target(),
            &report,
            Some(reviewer),
            Some("one"),
        )
        .unwrap();
        assert!(recorded.ok(), "{:?}", recorded.violations);
    }
    assert_eq!(
        merge_ready(&c, host.now(), &forge),
        std::slice::from_ref(&one)
    );
    let report = c.report("astra-review-8", &second_head, &[], "ACCEPT");
    c.receipt_with(
        "astra-review-8",
        &second_head,
        "gpt-6-astra",
        "xhigh",
        |receipt| {
            receipt["pr"] = 8.into();
        },
    );
    let recorded = verdict::record(
        &host,
        &c.root,
        &target(),
        &report,
        Some("astra"),
        Some("two"),
    )
    .unwrap();
    assert!(recorded.ok(), "{:?}", recorded.violations);
    let claude = c.write(
        "drafts/claude-two.md",
        &format!(
            "# Claude review\n\nRead in full.\n\n```cm-review\nhead: {second_head}\n```\n\nACCEPT\n"
        ),
    );
    let recorded = verdict::record(
        &host,
        &c.root,
        &target(),
        &claude,
        Some("claude"),
        Some("two"),
    )
    .unwrap();
    assert!(recorded.ok(), "{:?}", recorded.violations);
    assert_eq!(
        merge_ready(&c, host.now(), &forge),
        [
            one,
            format!("cm advance {STEP} merged --pr {second_pr} --sha '<merge-sha>' --part two")
        ]
    );
    // Each action carries its own part, so the JSON names that part's PR, head and CI.
    let next = plan::next_with(&c.root, None, None, host.now(), Some(&forge)).unwrap();
    let two = next.fields["actionable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["kind"] == "merge-ready" && action["part"] == "two")
        .unwrap()
        .clone();
    assert_eq!(two["pr"], second_pr);
    assert_eq!(two["head"], second_head);
    assert_eq!(two["ci_green"], true);
    // A blocked step merges none of its parts.
    let blocked = commands::advance(
        &host,
        &c.root,
        &target(),
        &Phase::Blocked("waiting-on-the-owner".into()),
        &Evidence::default(),
        None,
        None,
    )
    .unwrap();
    assert!(blocked.ok(), "{:?}", blocked.violations);
    assert!(merge_ready(&c, host.now(), &forge).is_empty());
}

#[test]
fn every_model_lane_needs_astra_and_a_claude_verdict_at_the_head() {
    for lane in [
        Lane::Codex,
        Lane::ClaudeCloud,
        Lane::Fable,
        Lane::Coordinator,
    ] {
        let host = at("2026-10-26T08:00:00Z");
        let c = campaign(&host, true);
        let head = c.head();
        let dir = c.root.join(target().unit);
        let mut unit = store::load_unit(&dir).unwrap();
        unit.steps.get_mut("S1").unwrap().lane = Some(lane);
        commands::write_unit(&dir, &unit).unwrap();
        let gaps = || {
            verdict::merge_gaps(&c.root, &target(), None, Some(PR), &head, None)
                .unwrap()
                .gaps
        };
        let missing = || {
            verdict::missing_reviews(
                &verdict::load(&c.root).unwrap(),
                STEP,
                &head,
                Some(PR),
                None,
                verdict::model_lane(Some(lane)),
            )
        };
        assert_eq!(missing(), ["astra", "claude"], "{lane:?}");
        let accept = c.report("astra-review-7", &head, &[], "ACCEPT");
        c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
        assert!(record(&host, &c, &accept, "astra").ok());
        assert_eq!(missing(), ["claude"], "{lane:?}");
        let gaps_now = gaps();
        assert_eq!(gaps_now.len(), 1, "{lane:?} {gaps_now:?}");
        assert!(gaps_now[0].contains("no claude ACCEPT"), "{gaps_now:?}");
        claude_accept(&host, &c, &head);
        assert!(missing().is_empty(), "{lane:?}");
        assert!(gaps().is_empty(), "{lane:?} {:?}", gaps());
    }
    // A lane no model builds in needs any one verdict.
    assert!(!verdict::model_lane(Some(Lane::Owner)));
    assert!(!verdict::model_lane(Some(Lane::Pi)));
}

#[test]
fn every_model_lane_refuses_a_merge_without_astra_and_claude_at_the_head() {
    for lane in [
        Lane::Codex,
        Lane::ClaudeCloud,
        Lane::Fable,
        Lane::Coordinator,
    ] {
        let host = at("2026-10-26T08:00:00Z");
        let c = campaign(&host, true);
        let head = c.head();
        let dir = c.root.join(target().unit);
        let mut unit = store::load_unit(&dir).unwrap();
        unit.steps.get_mut("S1").unwrap().lane = Some(lane);
        commands::write_unit(&dir, &unit).unwrap();
        let green = FakeForge::open(&head, &[("test", "success")]);
        let gate = MergeGate {
            forge: Some(&green),
            ..MergeGate::default()
        };
        let accept = c.report("astra-review-7", &head, &[], "ACCEPT");
        c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
        assert!(record(&host, &c, &accept, "astra").ok());
        let refused = c.merge(&host, &gate);
        assert_eq!(rules(&refused), ["merge-gate"], "{lane:?}");
        assert!(
            refused.violations[0].message.contains("no claude ACCEPT"),
            "{lane:?}: {:?}",
            refused.violations
        );
        assert_eq!(c.state(), Phase::PrOpen);
        claude_accept(&host, &c, &head);
        let merged = c.merge(&host, &gate);
        assert!(merged.ok(), "{lane:?}: {:?}", merged.violations);
        assert_eq!(c.state(), Phase::Merged);
    }
}

#[test]
fn next_act_json_carries_the_unread_owner_queue_for_the_merge_hold() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    let act = || {
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&c.root)
            .args(["--json", "next", "--act"])
            .env_remove("CM_CODEX_WEEKLY_PCT")
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    assert_eq!(act()["owner_queue_unread"], serde_json::json!([]));
    review_fixture::new_owner_item(&host, &c.root, "Choose a setting");
    let unread = act()["owner_queue_unread"].clone();
    assert_eq!(unread.as_array().unwrap().len(), 1, "{unread}");
    assert!(
        unread[0].as_str().unwrap().contains("Choose a setting"),
        "{unread}"
    );
}

#[test]
fn a_known_red_note_is_journaled_only_with_the_merge_it_explains() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    accepted(&host, &c);
    let head = c.head();
    let red = FakeForge {
        state: PullState::Merged,
        ..red_at_head(&head, "failure", "failure")
    };
    assert!(known_red(&host, &c, &red, &head, "red by design", "build").ok());
    let item = cm::ledger::QueueItem {
        title: "Hold the merges",
        context: "A release is in review.",
        question: "May merged steps move?",
        recommendation: "Wait for the review.",
    };
    let queued = cm::ledger::owner_queue_add(&host, &c.root, &item).unwrap();
    assert!(queued.ok(), "{:?}", queued.violations);
    let journal = || fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let before = journal();
    for now in [
        "2026-10-26T08:05:00Z",
        "2026-10-26T08:10:00Z",
        "2026-10-26T08:15:00Z",
        "2026-10-26T08:20:00Z",
    ] {
        let held = sync(&at(now), &red, &c.root).unwrap();
        assert_eq!(rules(&held), ["owner-queue-unread"], "{now}");
        assert_eq!(c.state(), Phase::PrOpen);
    }
    let after = journal();
    let added = after.strip_prefix(before.as_str()).unwrap();
    assert!(!added.contains("cm sync:"), "{added}");
    assert!(!added.contains("known-red"), "{added}");
}

#[test]
fn an_explicit_accept_is_stated_as_well_as_computed() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let accept = c.report("astra-review-7", &head, &[], "ACCEPT");
    c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
    assert!(record(&host, &c, &accept, "astra").ok());
    let gaps = || {
        verdict::merge_gaps(&c.root, &target(), None, Some(PR), &head, None)
            .unwrap()
            .gaps
    };
    // A Claude report with no findings and no verdict line computes ACCEPT but states none.
    let unstated = c.write(
        "drafts/claude-unstated.md",
        &format!("# Claude review\n\nRead.\n\n```cm-review\nhead: {head}\n```\n"),
    );
    let recorded = record(&host, &c, &unstated, "claude");
    assert!(recorded.ok(), "{:?}", recorded.violations);
    let open = gaps();
    assert_eq!(open.len(), 1, "{open:?}");
    assert!(open[0].contains("no claude ACCEPT"), "{open:?}");
    claude_accept(&host, &c, &head);
    assert!(gaps().is_empty(), "{:?}", gaps());
}

#[test]
fn reviews_recorded_before_parts_can_be_recorded_again_for_a_part() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, true);
    accepted(&host, &c);
    let head = c.head();
    let amendment = commands::Amendment {
        parts: vec!["one".into(), "two".into()],
        ..Default::default()
    };
    assert!(
        commands::amend(&host, &c.root, &target(), &amendment)
            .unwrap()
            .ok()
    );
    let gaps = || {
        verdict::merge_gaps(&c.root, &target(), Some("one"), Some(PR), &head, None)
            .unwrap()
            .gaps
    };
    // The step's reviews from before it had parts bind no part, so its receipt stays unbound.
    assert!(!gaps().is_empty());
    let astra = c.root.join(STEP).join("evidence/astra-review-7/report.md");
    let claude = c.root.join(format!("drafts/claude-{head}.md"));
    for (report, reviewer) in [(&astra, "astra"), (&claude, "claude")] {
        let again = verdict::record(
            &host,
            &c.root,
            &target(),
            report,
            Some(reviewer),
            Some("one"),
        )
        .unwrap();
        assert!(again.ok(), "{:?}", again.violations);
    }
    assert!(gaps().is_empty(), "{:?}", gaps());
    // Within one PR and part a report is still recorded once.
    let twice = verdict::record(
        &host,
        &c.root,
        &target(),
        &astra,
        Some("astra"),
        Some("one"),
    )
    .unwrap();
    assert_eq!(rules(&twice), ["review-duplicate"]);
}

#[test]
fn a_report_recorded_before_pr_open_is_not_recorded_again_as_another_family() {
    let host = at("2026-10-26T08:00:00Z");
    let c = campaign(&host, false);
    let head = c.head();
    let astra = c.report("astra-review-7", &head, &[], "ACCEPT");
    assert!(record(&host, &c, &astra, "astra").ok());
    let opened = commands::advance(
        &host,
        &c.root,
        &target(),
        &Phase::PrOpen,
        &Evidence {
            pr: Some(PR.into()),
            sha: Some(head.clone()),
            ..Evidence::default()
        },
        None,
        Some(&c.repo),
    )
    .unwrap();
    assert!(opened.ok(), "{:?}", opened.violations);
    // The review recorded with no PR stands for this one: the same report is no Claude verdict.
    assert_eq!(
        rules(&record(&host, &c, &astra, "claude")),
        ["review-duplicate"]
    );
}

fn scratch_git(home: &Path, repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=cm", "-c", "user.email=cm@example.com"])
        .args(args)
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

struct Scratch {
    c: Campaign,
    home: PathBuf,
    base: String,
    h1: String,
}

impl Scratch {
    fn new(host: &Fake) -> Scratch {
        Scratch::with(host, Lane::Codex, false)
    }

    fn with(host: &Fake, lane: Lane, worktree: bool) -> Scratch {
        let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let origin = temp.path().join("origin.git");
        let setup = Setup {
            pr: true,
            worktree,
            lane,
            home: Some(&home),
        };
        let c = campaign_in(host, temp, setup);
        let mut s = Scratch {
            c,
            home,
            base: String::new(),
            h1: String::new(),
        };
        s.base = s.git(&["rev-parse", "--abbrev-ref", "HEAD"]);
        let bare = scratch_git(
            &s.home,
            origin.parent().unwrap(),
            &["init", "-q", "--bare", "origin.git"],
        );
        assert!(bare.status.success(), "{bare:?}");
        s.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
        s.git(&["push", "-q", "origin", &s.base]);
        s.git(&["remote", "set-head", "origin", &s.base]);
        s.git(&["checkout", "-q", "-b", "feature"]);
        s.h1 = s.commit("shared.txt", "feature\n");
        s.astra(host, &s.h1, "h1", "ACCEPT");
        claude_accept(host, &s.c, &s.h1);
        s
    }

    fn git(&self, args: &[&str]) -> String {
        let output = scratch_git(&self.home, &self.c.repo, args);
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    fn commit(&self, file: &str, text: &str) -> String {
        fs::write(self.c.repo.join(file), text).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-m", file]);
        self.head()
    }

    fn on_main(&self, file: &str, text: &str) {
        self.git(&["checkout", "-q", &self.base]);
        self.commit(file, text);
        self.git(&["push", "-q", "origin", &self.base]);
        self.git(&["checkout", "-q", "feature"]);
    }

    fn merge_main(&self, resolution: Option<&str>) -> String {
        let main = format!("origin/{}", self.base);
        let merged = scratch_git(
            &self.home,
            &self.c.repo,
            &["merge", "-q", "--no-edit", &main],
        );
        match resolution {
            None => assert!(merged.status.success(), "{merged:?}"),
            Some(text) => {
                assert!(!merged.status.success(), "the merge conflicts: {merged:?}");
                fs::write(self.c.repo.join("shared.txt"), text).unwrap();
                self.git(&["add", "shared.txt"]);
                self.git(&["commit", "-q", "--no-edit"]);
            }
        }
        self.head()
    }

    fn astra(&self, host: &Fake, head: &str, name: &str, verdict: &str) {
        let dir = format!("astra-review-7-{name}");
        let report = self.c.report(&dir, head, &[], verdict);
        self.c.receipt(&dir, head, "gpt-6-astra", "xhigh");
        let recorded = record(host, &self.c, &report, "astra");
        assert!(recorded.ok(), "{:?}", recorded.violations);
    }

    fn resolution(
        &self,
        host: &Fake,
        reviewer: Option<&str>,
        merge: &str,
        name: &str,
        findings: &[&str],
    ) -> Outcome {
        let verdict = if findings.is_empty() {
            "ACCEPT"
        } else {
            "FIX REQUIRED"
        };
        let mut block = format!("head: {merge}\n");
        for finding in findings {
            block.push_str(finding);
            block.push('\n');
        }
        let text = format!(
            "# Resolution review by {}\n\nThe remerge diff only.\n\n```cm-review\n{block}```\n\n{verdict}\n",
            reviewer.unwrap_or("nobody")
        );
        let report = if reviewer == Some("astra") {
            let dir = format!("astra-review-7-{name}");
            let report = self
                .c
                .write(&format!("{STEP}/evidence/{dir}/report.md"), &text);
            self.c.receipt(&dir, merge, "gpt-6-astra", "xhigh");
            report
        } else {
            self.c.write(&format!("drafts/{name}.md"), &text)
        };
        verdict::record_resolution(
            host,
            &self.c.root,
            &target(),
            &report,
            reviewer,
            None,
            Some(&self.c.repo),
        )
        .unwrap()
    }

    fn accepted_resolution(&self, host: &Fake, merge: &str, name: &str) {
        for reviewer in ["astra", "claude"] {
            let recorded = self.resolution(
                host,
                Some(reviewer),
                merge,
                &format!("{name}-{reviewer}"),
                &[],
            );
            assert!(recorded.ok(), "{:?}", recorded.violations);
        }
    }

    fn cleared(&self) -> verdict::Cleared {
        let tip = self.git(&["rev-parse", &self.base]);
        let lineage = Lineage {
            repo: &self.c.repo,
            base: Some(&tip),
        };
        verdict::merge_gaps(
            &self.c.root,
            &target(),
            None,
            Some(PR),
            &self.head(),
            Some(&lineage),
        )
        .unwrap()
    }

    fn owed(&self, host: &Fake, head: &str) -> Vec<String> {
        let forge = FakeForge::open(head, &[("fmt, clippy, test", "success")]);
        let synced = cm::sync::sync(host, &forge, &self.c.root).unwrap();
        assert!(synced.ok(), "{:?}", synced.violations);
        let next = plan::next_at(&self.c.root, None, None, host.now).unwrap();
        next.lines
            .iter()
            .filter_map(|line| line.split_once(". ").map(|(_, line)| line))
            .filter(|line| line.starts_with("review-owed ") || line.starts_with("merge-ready "))
            .map(str::to_owned)
            .collect()
    }

    fn merge(&self, host: &Fake) -> Outcome {
        let head = self.head();
        let forge = FakeForge {
            base: Some(self.git(&["rev-parse", &self.base])),
            ..FakeForge::open(&head, &[("fmt, clippy, test", "success")])
        };
        commands::advance_gated(
            host,
            &self.c.root,
            &target(),
            commands::Advance {
                to: &Phase::Merged,
                given: &Evidence {
                    sha: Some(head),
                    ..Evidence::default()
                },
                file: None,
                repo: Some(&self.c.repo),
                part: None,
                by: &[],
            },
            &MergeGate {
                forge: Some(&forge),
                ..MergeGate::default()
            },
        )
        .unwrap()
    }

    fn edit_ledger(&self, edit: impl FnOnce(&mut verdict::Verdicts)) {
        let mut ledger = verdict::load(&self.c.root).unwrap();
        edit(&mut ledger);
        store::write_json(&self.c.root.join(".cm/verdicts.json"), &ledger).unwrap();
    }

    fn reviews(&self) -> usize {
        verdict::load(&self.c.root).unwrap().reviews.len()
    }

    fn journal(&self) -> String {
        fs::read_to_string(self.c.root.join("JOURNAL.md")).unwrap()
    }
}

fn walk_gaps(cleared: &verdict::Cleared) -> Vec<String> {
    match &cleared.stop {
        Some(CleanUpdateError::ResolutionGaps { gaps, .. }) => gaps.clone(),
        other => panic!("the walk stops at a merge's resolution reviews, not {other:?}"),
    }
}

#[test]
fn a_conflicted_merge_from_main_clears_once_both_families_accept_its_resolution() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::Conflicts {
            commit: merge.clone()
        })
    );

    let claude = s.resolution(&host, Some("claude"), &merge, "claude-resolution", &[]);
    assert!(claude.ok(), "{:?}", claude.violations);
    assert_eq!(claude.fields["kind"], "resolution");
    assert_eq!(claude.fields["first_parent"], s.h1.as_str());
    let one = s.cleared();
    assert!(!one.gaps.is_empty());
    assert!(
        walk_gaps(&one).contains(&"no astra resolution ACCEPT".to_owned()),
        "{:?}",
        one.stop
    );
    let refused = s.merge(&host);
    assert!(
        rules(&refused).contains(&"merge-gate"),
        "{:?}",
        refused.violations
    );
    assert_eq!(s.c.state(), Phase::PrOpen);

    let astra = s.resolution(&host, Some("astra"), &merge, "resolution", &[]);
    assert!(astra.ok(), "{:?}", astra.violations);
    let both = s.cleared();
    assert!(both.gaps.is_empty(), "{:?}", both.gaps);
    assert_eq!(both.reviewed.as_deref(), Some(s.h1.as_str()));
    assert_eq!(both.resolved, vec![merge.clone()]);

    let ledger: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(s.c.root.join(".cm/verdicts.json")).unwrap())
            .unwrap();
    let kinds: Vec<Option<&str>> = ledger["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .map(|review| review.get("kind").map(|kind| kind.as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        vec![None, None, Some("resolution"), Some("resolution")]
    );

    let merged = s.merge(&host);
    assert!(merged.ok(), "{:?}", merged.violations);
    assert!(
        s.journal().contains(&format!(
            "- reviewed at {}; every commit since is a clean update from the base branch or a merge from it whose conflict resolution both families accepted ({merge})",
            s.h1
        )),
        "{}",
        s.journal()
    );
}

#[test]
fn a_resolution_accept_is_never_an_accept_of_the_head() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    assert!(s.cleared().gaps.is_empty());

    let without_checkout =
        verdict::merge_gaps(&s.c.root, &target(), None, Some(PR), &merge, None).unwrap();
    assert!(
        without_checkout
            .gaps
            .iter()
            .any(|gap| gap.starts_with(&format!(
                "only resolution reviews of {merge} are recorded (R3, R4)"
            ))),
        "{:?}",
        without_checkout.gaps
    );
    let verdicts = verdict::load(&s.c.root).unwrap();
    assert_eq!(
        verdict::missing_reviews(&verdicts, STEP, &merge, Some(PR), None, true),
        vec!["astra", "claude"]
    );

    let fix = s.c.report(
        "head-review",
        &merge,
        &["F1 | contract | introduced | src/lib.rs:1 | broken"],
        "FIX REQUIRED",
    );
    assert!(record(&host, &s.c, &fix, "claude").ok());
    let governed = s.cleared();
    assert_eq!(governed.stop, None);
    assert!(
        governed
            .gaps
            .iter()
            .any(|gap| gap.contains("R5.F1 unresolved")),
        "{:?}",
        governed.gaps
    );
}

#[test]
fn an_open_resolution_finding_or_an_unrecorded_receipt_at_the_merge_keeps_the_gate_shut() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let blocking = ["F1 | contract | exposed | shared.txt:1 | main's change breaks it"];
    assert!(
        s.resolution(&host, Some("astra"), &merge, "resolution-1", &blocking)
            .ok()
    );
    assert!(
        s.resolution(&host, Some("claude"), &merge, "claude-resolution", &[])
            .ok()
    );
    assert_eq!(
        walk_gaps(&s.cleared()),
        vec!["R3.F1 unresolved", "no astra resolution ACCEPT"]
    );

    let ruling =
        s.c.write("ruling.md", "Coordinator: F1 is main's to fix.\n");
    assert!(
        verdict::resolve(&host, &s.c.root, &target(), "R3.F1", "ruling.md", &ruling)
            .unwrap()
            .ok()
    );
    assert_eq!(walk_gaps(&s.cleared()), vec!["no astra resolution ACCEPT"]);

    let unstated = s.c.write(
        &format!("{STEP}/evidence/astra-review-7-unstated/report.md"),
        &format!("# Astra resolution\n\nNo verdict line.\n\n```cm-review\nhead: {merge}\n```\n"),
    );
    s.c.receipt("astra-review-7-unstated", &merge, "gpt-6-astra", "xhigh");
    assert!(
        verdict::record_resolution(
            &host,
            &s.c.root,
            &target(),
            &unstated,
            Some("astra"),
            None,
            Some(&s.c.repo),
        )
        .unwrap()
        .ok()
    );
    assert_eq!(walk_gaps(&s.cleared()), vec!["no astra resolution ACCEPT"]);

    assert!(
        s.resolution(&host, Some("astra"), &merge, "resolution-2", &[])
            .ok()
    );
    assert!(s.cleared().gaps.is_empty());

    s.c.receipt_with(
        "astra-review-7-unrecorded",
        &merge,
        "gpt-6-astra",
        "xhigh",
        |receipt| {
            receipt["verdict"] = "FIX REQUIRED".into();
        },
    );
    let bound = s.cleared();
    assert_eq!(bound.stop, None);
    assert!(
        bound
            .gaps
            .iter()
            .any(|gap| gap.contains("astra-review-7-unrecorded/receipt.json")),
        "{:?}",
        bound.gaps
    );
}

#[test]
fn a_commit_after_an_accepted_resolution_needs_a_review_of_its_own() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    let owed_at = |head: &str| {
        vec![format!(
            "review-owed {STEP}: PR {PR}: no astra and claude verdict at {} since 2026-11-25T06:00Z (0 waiting) — `cm brief {STEP}`",
            &head[..12]
        )]
    };
    assert_eq!(s.owed(&host, &merge), owed_at(&merge));
    let fix = s.commit("fix.txt", "a fix after the merge\n");
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::NotMerge {
            commit: fix.clone()
        })
    );
    assert_eq!(s.owed(&host, &fix), owed_at(&fix));
    let refused = s.merge(&host);
    assert!(
        rules(&refused).contains(&"merge-gate"),
        "{:?}",
        refused.violations
    );

    s.astra(&host, &fix, "fix", "ACCEPT");
    claude_accept(&host, &s.c, &fix);
    let merged = s.merge(&host);
    assert!(merged.ok(), "{:?}", merged.violations);
}

#[test]
fn a_second_merge_from_main_walks_back_through_the_first_resolution() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let first = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &first, "resolution-1");

    s.on_main("other.txt", "main\n");
    let clean = s.merge_main(None);
    let cleared = s.cleared();
    assert!(cleared.gaps.is_empty(), "{:?}", cleared.gaps);
    assert_eq!(cleared.reviewed.as_deref(), Some(s.h1.as_str()));
    assert_eq!(cleared.resolved, vec![first.clone()]);

    s.on_main("shared.txt", "main again\n");
    let second = s.merge_main(Some("feature and main again\n"));
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::Conflicts {
            commit: second.clone()
        })
    );
    let astra = s.resolution(&host, Some("astra"), &second, "resolution-2", &[]);
    assert!(astra.ok(), "{:?}", astra.violations);
    assert_eq!(astra.fields["first_parent"], clean.as_str());
    assert_eq!(walk_gaps(&s.cleared()), vec!["no claude resolution ACCEPT"]);
    assert!(
        s.resolution(&host, Some("claude"), &second, "claude-resolution-2", &[])
            .ok()
    );
    let merged = s.merge(&host);
    assert!(merged.ok(), "{:?}", merged.violations);
    assert!(
        s.journal().contains(&format!(
            "\n- reviewed at {}; every commit since is a clean update from the base branch or a merge from it whose conflict resolution both families accepted ({second}, {first})\n",
            s.h1
        )),
        "{}",
        s.journal()
    );
}

#[test]
fn an_amended_merge_loses_the_resolution_accepts_of_the_merge_it_replaced() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    assert!(s.cleared().gaps.is_empty());
    s.git(&["commit", "-q", "--amend", "-m", "Merge main, amended"]);
    let amended = s.head();
    assert_ne!(amended, merge);
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::Conflicts { commit: amended })
    );
    let refused = s.merge(&host);
    assert!(
        rules(&refused).contains(&"merge-gate"),
        "{:?}",
        refused.violations
    );
}

#[test]
fn a_resolution_review_stands_only_on_a_conflicted_merge_from_main_whose_first_parent_clears() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    let refused = |outcome: Outcome| -> String {
        assert_eq!(
            rules(&outcome),
            ["review-resolution"],
            "{:?}",
            outcome.violations
        );
        assert_eq!(
            s.reviews(),
            2,
            "a refused resolution review is not recorded"
        );
        outcome.violations[0].message.clone()
    };
    let family = "a resolution review counts only for its reviewer's family: name a Codex reviewer (astra, sol, terra, gpt-…) or a Claude one (claude, opus, fresh-opus, fable, sonnet, haiku) with --reviewer";
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    assert_eq!(
        refused(s.resolution(&host, None, &merge, "nobody", &[])),
        family
    );
    assert_eq!(
        refused(s.resolution(&host, Some("bot"), &merge, "bot", &[])),
        family
    );
    let report = s.c.report("no-checkout", &merge, &[], "ACCEPT");
    assert_eq!(
        refused(
            verdict::record_resolution(
                &host,
                &s.c.root,
                &target(),
                &report,
                Some("claude"),
                None,
                None,
            )
            .unwrap()
        ),
        "a resolution review is checked against the merge in a checkout; pass --repo <a checkout of the repo>"
    );
    s.git(&["remote", "set-head", "origin", "--delete"]);
    assert_eq!(
        refused(s.resolution(&host, Some("claude"), &merge, "no-origin-head", &[])),
        format!(
            "cannot read origin/HEAD, the base branch's tip, in {}; fetch the base branch there",
            s.c.repo.display()
        )
    );
    s.git(&["remote", "set-head", "origin", &s.base]);
    assert_eq!(
        refused(s.resolution(&host, Some("claude"), &s.h1, "not-a-merge", &[])),
        format!("{} is not a merge from the base branch", &s.h1[..12])
    );

    s.on_main("other.txt", "main\n");
    let clean = s.merge_main(None);
    assert_eq!(
        refused(s.resolution(&host, Some("claude"), &clean, "clean", &[])),
        format!(
            "{} is a clean update from the base branch: its remerge diff is empty, so it needs no review",
            &clean[..12]
        )
    );

    s.git(&["checkout", "-q", "-b", "side", &s.base]);
    let side = s.commit("shared.txt", "side\n");
    s.git(&["checkout", "-q", "feature"]);
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", "side"]);
    assert!(!merged.status.success(), "the side merge conflicts");
    fs::write(s.c.repo.join("shared.txt"), "feature and side\n").unwrap();
    s.git(&["add", "shared.txt"]);
    s.git(&["commit", "-q", "--no-edit"]);
    let off_base = s.head();
    assert_eq!(
        refused(s.resolution(&host, Some("claude"), &off_base, "off-base", &[])),
        format!(
            "{} merges {} which is not on the base branch origin/HEAD",
            &off_base[..12],
            &side[..12]
        )
    );

    s.git(&["reset", "-q", "--hard", &clean]);
    let unreviewed = s.commit("fix.txt", "not reviewed\n");
    s.on_main("shared.txt", "main again\n");
    let over_unreviewed = s.merge_main(Some("feature, side and main\n"));
    let parent = refused(s.resolution(&host, Some("claude"), &over_unreviewed, "parent", &[]));
    assert!(
        parent.starts_with(&format!(
            "{}'s first parent {} does not clear the gate, so no resolution review can stand on it: ",
            &over_unreviewed[..12],
            &unreviewed[..12]
        )),
        "{parent}"
    );
}

#[test]
fn the_cli_records_a_resolution_review_only_under_kind_resolution() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let cm = |report: &Path, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&s.c.root)
            .args(["--json", "review", "record", STEP])
            .arg(report)
            .args(args)
            .env("HOME", &s.home)
            .env("TMPDIR", &s.home)
            .env("GIT_CONFIG_GLOBAL", s.home.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    };
    let repo = s.c.repo.to_str().unwrap();
    let resolution = s.c.report("cli-resolution", &merge, &[], "ACCEPT");
    for usage in [
        vec!["--reviewer", "claude", "--repo", repo],
        vec!["--reviewer", "claude", "--kind", "delta", "--repo", repo],
    ] {
        let output = cm(&resolution, &usage);
        assert_eq!(output.status.code(), Some(2), "{usage:?}: {output:?}");
    }
    assert_eq!(s.reviews(), 2);
    let output = cm(
        &resolution,
        &[
            "--reviewer",
            "claude",
            "--kind",
            "resolution",
            "--repo",
            repo,
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["kind"], "resolution");
    assert_eq!(json["first_parent"], s.h1.as_str());

    let head = s.c.write(
        "drafts/cli-head.md",
        &format!(
            "# Opus review\n\nThe whole head.\n\n```cm-review\nhead: {merge}\n```\n\nACCEPT\n"
        ),
    );
    let output = cm(&head, &["--reviewer", "opus"]);
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json.get("kind").is_none() && json.get("first_parent").is_none(),
        "{json}"
    );
    let kinds: Vec<verdict::ReviewKind> = verdict::load(&s.c.root)
        .unwrap()
        .reviews
        .iter()
        .map(|review| review.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            verdict::ReviewKind::Head,
            verdict::ReviewKind::Head,
            verdict::ReviewKind::Resolution,
            verdict::ReviewKind::Head
        ]
    );
}

#[test]
fn a_resolution_review_reads_the_merge_in_the_steps_worktree_when_no_repo_is_named() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::with(&host, Lane::Codex, true);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let report = s.c.write(
        "drafts/worktree-resolution.md",
        &format!(
            "# Resolution\n\nRead in the worktree.\n\n```cm-review\nhead: {merge}\n```\n\nACCEPT\n"
        ),
    );
    let recorded = verdict::record_resolution(
        &host,
        &s.c.root,
        &target(),
        &report,
        Some("claude"),
        None,
        None,
    )
    .unwrap();
    assert!(recorded.ok(), "{:?}", recorded.violations);
    assert_eq!(recorded.fields["first_parent"], s.h1.as_str());
}

#[test]
fn a_ledger_cm_0_17_1_wrote_round_trips_byte_for_byte_and_keeps_its_records_on_a_new_review() {
    let written = include_str!("fixtures/verdicts-0.17.1.json");
    let host = at("2026-11-25T06:00:00Z");
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let setup = Setup {
        pr: true,
        worktree: false,
        lane: Lane::Codex,
        home: Some(&home),
    };
    let c = campaign_in(&host, temp, setup);
    let ledger = c.root.join(".cm/verdicts.json");
    fs::write(&ledger, written).unwrap();
    let resaved = c.root.join("resaved.json");
    store::write_json(&resaved, &verdict::load(&c.root).unwrap()).unwrap();
    assert_eq!(fs::read_to_string(&resaved).unwrap(), written);

    claude_accept(&host, &c, &c.head());
    let after = fs::read_to_string(&ledger).unwrap();
    let kept = written.strip_suffix("\n  ]\n}\n").unwrap();
    let next = verdict::load(&c.root).unwrap().reviews.len();
    assert!(
        after.starts_with(&format!("{kept},\n    {{\n      \"number\": {next},")),
        "{after}"
    );
    assert!(!after.contains("\"kind\""), "{after}");
}

fn as_resolutions(ledger: &mut verdict::Verdicts, from: &str, to: &str) {
    let copies: Vec<verdict::Recorded> = ledger
        .reviews
        .iter()
        .filter(|review| review.at_head(from))
        .map(|review| verdict::Recorded {
            number: review.number + 100,
            head: to.to_owned(),
            kind: verdict::ReviewKind::Resolution,
            ..review.clone()
        })
        .collect();
    ledger.reviews.extend(copies);
}

fn schema(c: &Campaign) -> u64 {
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(c.root.join(".cm/state.json")).unwrap()).unwrap();
    state["schema"].as_u64().unwrap()
}

fn scratch_cm(s: &Scratch, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&s.c.root)
        .args(args)
        .env("HOME", &s.home)
        .env("TMPDIR", &s.home)
        .env("GIT_CONFIG_GLOBAL", s.home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

#[test]
fn a_resolution_review_binds_the_full_merge_sha_when_its_report_names_a_prefix() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let prefix = &merge[..7];
    let claude = s.resolution(&host, Some("claude"), prefix, "claude-prefix", &[]);
    assert!(claude.ok(), "{:?}", claude.violations);
    assert_eq!(claude.fields["head"], merge.as_str());
    assert!(
        s.resolution(&host, Some("astra"), prefix, "astra-prefix", &[])
            .ok()
    );
    let recorded = verdict::load(&s.c.root).unwrap();
    let heads: Vec<&str> = recorded.reviews[2..]
        .iter()
        .map(|review| review.head.as_str())
        .collect();
    assert_eq!(heads, [merge.as_str(), merge.as_str()]);
    assert!(s.cleared().gaps.is_empty());

    s.git(&["commit", "-q", "--amend", "-m", "Merge main, amended"]);
    let amended = s.head();
    let sharing = format!("{}{}", &amended[..7], &merge[7..]);
    s.edit_ledger(|ledger| {
        for review in &mut ledger.reviews[2..] {
            review.head = sharing.clone();
        }
    });
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::Conflicts { commit: amended })
    );
}

#[test]
fn a_resolution_review_needs_both_families_at_the_first_parent_on_every_lane() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::with(&host, Lane::Pi, true);
    let h2 = s.commit("more.txt", "claude only\n");
    claude_accept(&host, &s.c, &h2);
    let pi = verdict::merge_gaps(&s.c.root, &target(), None, Some(PR), &h2, None).unwrap();
    assert!(pi.gaps.is_empty(), "{:?}", pi.gaps);

    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let refused = s.resolution(&host, Some("claude"), &merge, "claude-resolution", &[]);
    assert_eq!(
        rules(&refused),
        ["review-resolution"],
        "{:?}",
        refused.violations
    );
    let message = &refused.violations[0].message;
    assert!(
        message.starts_with(&format!(
            "{}'s first parent {} does not clear the gate, so no resolution review can stand on it: ",
            &merge[..12],
            &h2[..12]
        )) && message.contains(&format!("no astra ACCEPT of {h2} is recorded")),
        "{message}"
    );

    s.astra(&host, &h2, "h2", "ACCEPT");
    s.accepted_resolution(&host, &merge, "resolution");
    let cleared = s.cleared();
    assert!(cleared.gaps.is_empty(), "{:?}", cleared.gaps);
    assert_eq!(cleared.reviewed.as_deref(), Some(h2.as_str()));

    s.c.receipt_with(
        "astra-review-7-late",
        &h2,
        "gpt-6-astra",
        "xhigh",
        |receipt| {
            receipt["verdict"] = "FIX REQUIRED".into();
        },
    );
    let landed = s.cleared();
    assert!(
        landed.gaps.iter().any(|gap| gap.contains(&format!(
            "the nearest reviewed commit before {merge}, {h2}, joined to it by clean updates and merges whose conflict resolutions both families accepted ({merge}), does not clear"
        )) && gap.contains("astra-review-7-late/receipt.json")),
        "{:?}",
        landed.gaps
    );
}

#[test]
fn a_merge_whose_parents_merge_cleanly_is_no_conflict_resolution() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("other.txt", "main\n");
    s.commit("unreviewed.txt", "never reviewed\n");
    let tree = s.git(&["rev-parse", "HEAD^{tree}"]);
    let main = s.git(&["rev-parse", &format!("origin/{}", s.base)]);
    let synthetic = s.git(&[
        "commit-tree",
        &tree,
        "-p",
        &s.h1,
        "-p",
        &main,
        "-m",
        "synthetic",
    ]);
    s.git(&["reset", "-q", "--hard", &synthetic]);
    let root_commit = s.git(&["rev-list", "--max-parents=0", "HEAD"]);
    let refused = |head: &str| {
        let outcome = s.resolution(&host, Some("claude"), head, &format!("claude-{head}"), &[]);
        assert_eq!(
            rules(&outcome),
            ["review-resolution"],
            "{:?}",
            outcome.violations
        );
        assert_eq!(
            outcome.violations[0].message,
            format!(
                "{} is no conflict resolution a resolution review can cover: its parents merge without a conflict, so its remerge diff resolves none; it needs a review of its own",
                &head[..12]
            )
        );
    };
    refused(&synthetic);
    s.edit_ledger(|ledger| as_resolutions(ledger, &s.h1, &synthetic));
    assert_eq!(
        walk_gaps(&s.cleared()),
        ["its parents merge without a conflict, so its remerge diff resolves none"]
    );
    assert!(rules(&s.merge(&host)).contains(&"merge-gate"));

    let fix_tree = s.git(&["rev-parse", "HEAD^{tree}"]);
    let old_main = s.git(&[
        "commit-tree",
        &fix_tree,
        "-p",
        &s.h1,
        "-p",
        &root_commit,
        "-m",
        "fix with an old main commit as second parent",
    ]);
    s.git(&["reset", "-q", "--hard", &old_main]);
    refused(&old_main);
}

#[test]
fn a_conflict_resolution_that_also_changes_other_files_is_refused() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let main = format!("origin/{}", s.base);
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
    assert!(!merged.status.success(), "the merge conflicts");
    fs::write(s.c.repo.join("shared.txt"), "feature and main\n").unwrap();
    fs::write(s.c.repo.join("other.txt"), "an edit outside the conflict\n").unwrap();
    s.git(&["add", "shared.txt", "other.txt"]);
    s.git(&["commit", "-q", "--no-edit"]);
    let merge = s.head();
    let refused = s.resolution(&host, Some("claude"), &merge, "claude-resolution", &[]);
    assert_eq!(
        rules(&refused),
        ["review-resolution"],
        "{:?}",
        refused.violations
    );
    assert_eq!(
        refused.violations[0].message,
        format!(
            "{} is no conflict resolution a resolution review can cover: it changes \"other.txt\" beyond the files its parents conflict in; it needs a review of its own",
            &merge[..12]
        )
    );
    s.edit_ledger(|ledger| as_resolutions(ledger, &s.h1, &merge));
    assert_eq!(
        walk_gaps(&s.cleared()),
        ["it changes \"other.txt\" beyond the files its parents conflict in"]
    );
}

#[test]
fn a_receipted_astra_resolution_accept_never_meets_the_heads_receipt_condition() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let unreceipted = s.c.write(
        "drafts/astra-head.md",
        &format!("# Astra review\n\nNo receipt.\n\n```cm-review\nhead: {merge}\n```\n\nACCEPT\n"),
    );
    assert!(record(&host, &s.c, &unreceipted, "astra").ok());
    claude_accept(&host, &s.c, &merge);
    let astra = s.resolution(&host, Some("astra"), &merge, "resolution", &[]);
    assert!(astra.ok(), "{:?}", astra.violations);
    let id = astra.fields["id"].as_str().unwrap().to_owned();
    let at_head = verdict::merge_gaps(&s.c.root, &target(), None, Some(PR), &merge, None).unwrap();
    assert!(
        at_head.gaps.iter().any(|gap| gap.starts_with(&format!(
            "no Astra receipt at {merge} with effective gpt-6-astra/xhigh; found "
        )) && gap.contains(&format!(
            "01-a/u/S1/evidence/astra-review-7-resolution/receipt.json (the receipt of {id}, a resolution review)"
        ))),
        "{:?}",
        at_head.gaps
    );
    assert!(rules(&s.merge(&host)).contains(&"merge-gate"));
    assert!(
        s.owed(&host, &merge)
            .iter()
            .all(|line| !line.starts_with("merge-ready ")),
    );
}

#[test]
fn a_resolution_review_stamps_the_campaign_with_a_schema_cm_0_17_1_refuses() {
    const READ_BY_0_17_1: u64 = 7;
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    let state = s.c.root.join(".cm/state.json");
    assert_eq!(schema(&s.c), READ_BY_0_17_1);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let unstamped = fs::read(&state).unwrap();
    let head = s.c.write(
        "drafts/opus-head.md",
        &format!("# Opus review\n\nThe head.\n\n```cm-review\nhead: {merge}\n```\n\nACCEPT\n"),
    );
    assert!(record(&host, &s.c, &head, "opus").ok());
    assert_eq!(fs::read(&state).unwrap(), unstamped);

    let first = s.resolution(&host, Some("claude"), &merge, "claude-resolution", &[]);
    assert!(first.ok(), "{:?}", first.violations);
    assert!(
        first
            .lines
            .contains(&"the campaign is at schema 8 now; an older cm refuses it".to_owned()),
        "{:?}",
        first.lines
    );
    assert!(schema(&s.c) > READ_BY_0_17_1);
    assert!(
        s.journal()
            .contains("- schema: the campaign is at schema 8 now; an older cm refuses it\n")
    );
    let second = s.resolution(&host, Some("astra"), &merge, "resolution", &[]);
    assert!(!second.lines.iter().any(|line| line.contains("schema")));

    let forge = FakeForge::open(&merge, &[("fmt, clippy, test", "success")]);
    assert!(cm::sync::sync(&host, &forge, &s.c.root).unwrap().ok());
    let read = scratch_cm(&s, &["owner-queue", "read"]);
    assert!(read.status.success(), "{read:?}");
    assert_eq!(schema(&s.c), 8);

    let mut newer: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    newer["schema"] = 9.into();
    fs::write(&state, serde_json::to_string_pretty(&newer).unwrap()).unwrap();
    let refused = scratch_cm(&s, &["next"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("has schema 9, newer than the schema 8 this cm"),
        "{refused:?}"
    );
}

#[test]
fn an_astra_resolution_accept_without_its_receipt_leaves_the_merge_uncovered() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let report = s.c.write(
        "drafts/astra-unreceipted.md",
        &format!(
            "# Astra resolution\n\nNo receipt.\n\n```cm-review\nhead: {merge}\n```\n\nACCEPT\n"
        ),
    );
    let astra = verdict::record_resolution(
        &host,
        &s.c.root,
        &target(),
        &report,
        Some("astra"),
        None,
        Some(&s.c.repo),
    )
    .unwrap();
    assert!(astra.ok(), "{:?}", astra.violations);
    assert!(
        s.resolution(&host, Some("claude"), &merge, "claude-resolution", &[])
            .ok()
    );
    let gaps = walk_gaps(&s.cleared());
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        gaps[0].starts_with(&format!(
            "no Astra receipt at {merge} with effective gpt-6-astra/xhigh; found "
        )),
        "{gaps:?}"
    );
}

#[test]
fn a_late_finding_at_the_first_parent_reopens_a_covered_merge() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    assert!(s.cleared().gaps.is_empty());
    let late = s.c.report(
        "late",
        &s.h1,
        &["F1 | contract | introduced | src/lib.rs:1 | broken"],
        "FIX REQUIRED",
    );
    let recorded = record(&host, &s.c, &late, "opus");
    assert!(recorded.ok(), "{:?}", recorded.violations);
    let id = recorded.fields["id"].as_str().unwrap().to_owned();
    let reopened = s.cleared();
    assert!(
        reopened.gaps.iter().any(|gap| gap.starts_with(&format!(
            "the nearest reviewed commit before {merge}, {}, joined to it by clean updates and merges whose conflict resolutions both families accepted ({merge}), does not clear: ",
            s.h1
        )) && gap.contains(&format!("{id}.F1 unresolved"))),
        "{:?}",
        reopened.gaps
    );
    assert!(rules(&s.merge(&host)).contains(&"merge-gate"));
}

#[test]
fn an_octopus_merge_takes_no_resolution_review() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.git(&["checkout", "-q", "-b", "side", &s.base]);
    let side = s.commit("side.txt", "side\n");
    s.git(&["checkout", "-q", "feature"]);
    s.on_main("other.txt", "main\n");
    let main = s.git(&["rev-parse", &format!("origin/{}", s.base)]);
    let tree = s.git(&["rev-parse", "HEAD^{tree}"]);
    let octopus = s.git(&[
        "commit-tree",
        &tree,
        "-p",
        &s.h1,
        "-p",
        &main,
        "-p",
        &side,
        "-m",
        "octopus",
    ]);
    s.git(&["reset", "-q", "--hard", &octopus]);
    let refused = s.resolution(&host, Some("claude"), &octopus, "claude-octopus", &[]);
    assert_eq!(
        rules(&refused),
        ["review-resolution"],
        "{:?}",
        refused.violations
    );
    assert_eq!(
        refused.violations[0].message,
        format!("{} is not a merge from the base branch", &octopus[..12])
    );
    s.edit_ledger(|ledger| as_resolutions(ledger, &s.h1, &octopus));
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::NotMerge { commit: octopus })
    );
}

#[test]
fn resolution_reviews_recorded_for_another_pr_cover_nothing() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    assert!(s.cleared().gaps.is_empty());
    s.edit_ledger(|ledger| {
        for review in &mut ledger.reviews[2..] {
            review.pr = Some("https://github.com/o/r/pull/8".into());
        }
    });
    let gaps = walk_gaps(&s.cleared());
    assert!(
        gaps.contains(&"no astra or claude resolution ACCEPT".to_owned()),
        "{gaps:?}"
    );
}

impl Scratch {
    fn conflicted_with(&self, extra: impl FnOnce(&Scratch)) -> String {
        self.on_main("shared.txt", "main\n");
        let main = format!("origin/{}", self.base);
        let merged = scratch_git(
            &self.home,
            &self.c.repo,
            &["merge", "-q", "--no-edit", &main],
        );
        assert!(!merged.status.success(), "the merge conflicts: {merged:?}");
        fs::write(self.c.repo.join("shared.txt"), "feature and main\n").unwrap();
        extra(self);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--no-edit"]);
        self.head()
    }

    fn reviewed(&self, host: &Fake, name: &str) -> String {
        let head = self.head();
        self.astra(host, &head, name, "ACCEPT");
        claude_accept(host, &self.c, &head);
        head
    }

    fn on_main_with(&self, change: impl FnOnce(&Scratch)) {
        self.git(&["checkout", "-q", &self.base]);
        change(self);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "main"]);
        self.git(&["push", "-q", "origin", &self.base]);
        self.git(&["checkout", "-q", "feature"]);
    }
}

fn beyond_the_conflict(s: &Scratch, host: &Fake, merge: &str, parent: &str, shown: &str) {
    let gap = format!("it changes {shown} beyond the files its parents conflict in");
    let refused = s.resolution(host, Some("claude"), merge, "claude-beyond", &[]);
    assert_eq!(
        rules(&refused),
        ["review-resolution"],
        "{:?}",
        refused.violations
    );
    assert_eq!(
        refused.violations[0].message,
        format!(
            "{} is no conflict resolution a resolution review can cover: {gap}; it needs a review of its own",
            &merge[..12]
        )
    );
    s.edit_ledger(|ledger| as_resolutions(ledger, parent, merge));
    assert_eq!(walk_gaps(&s.cleared()), [gap]);
}

#[test]
fn a_change_beyond_the_conflict_is_seen_by_its_exact_name() {
    let host = at("2026-11-25T06:00:00Z");
    for (name, shown) in [
        (" shared.txt", "\" shared.txt\""),
        ("shared.txt\n", "\"shared.txt\\n\""),
        ("-shared.txt", "\"-shared.txt\""),
        ("shared.tx", "\"shared.tx\""),
        ("Shared.txt", "\"Shared.txt\""),
    ] {
        let s = Scratch::new(&host);
        let merge = s.conflicted_with(|s| {
            fs::write(s.c.repo.join(name), "unreviewed\n").unwrap();
        });
        beyond_the_conflict(&s, &host, &merge, &s.h1, shown);
    }
}

#[cfg(unix)]
#[test]
fn file_names_that_are_not_utf_8_are_compared_byte_for_byte() {
    use std::os::unix::ffi::OsStrExt;
    let named = |bytes: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    let (conflicted, colliding) = (named(b"caf\xe9.txt"), named(b"caf\xe8.txt"));
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main_with(|s| fs::write(s.c.repo.join(&conflicted), "base\n").unwrap());
    s.merge_main(None);
    fs::write(s.c.repo.join(&conflicted), "feature\n").unwrap();
    s.git(&["commit", "-q", "-a", "-m", "feature edit"]);
    let h2 = s.reviewed(&host, "h2");
    s.on_main_with(|s| fs::write(s.c.repo.join(&conflicted), "main\n").unwrap());
    let main = format!("origin/{}", s.base);
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
    assert!(!merged.status.success(), "the merge conflicts: {merged:?}");
    fs::write(s.c.repo.join(&conflicted), "feature and main\n").unwrap();
    fs::write(s.c.repo.join(&colliding), "unreviewed\n").unwrap();
    s.git(&["add", "-A"]);
    s.git(&["commit", "-q", "--no-edit"]);
    let merge = s.head();
    beyond_the_conflict(&s, &host, &merge, &h2, "\"caf\\xe8.txt\"");
}

#[test]
fn a_rename_never_hides_a_change_beyond_the_conflict() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("a.txt", "a line one\na line two\n");
    s.on_main("u.txt", "u content\nmore u\n");
    s.merge_main(None);
    s.git(&["mv", "a.txt", "b.txt"]);
    s.git(&["commit", "-q", "-m", "rename a to b"]);
    let h2 = s.reviewed(&host, "h2");
    s.on_main_with(|s| {
        s.git(&["mv", "a.txt", "c.txt"]);
    });
    let main = format!("origin/{}", s.base);
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
    assert!(!merged.status.success(), "the renames conflict: {merged:?}");
    s.git(&["rm", "-q", "c.txt"]);
    s.git(&["mv", "u.txt", "a.txt"]);
    s.git(&["add", "-A"]);
    s.git(&["commit", "-q", "--no-edit"]);
    let merge = s.head();
    beyond_the_conflict(&s, &host, &merge, &h2, "\"u.txt\"");
}

#[test]
fn add_add_and_modify_delete_conflicts_stay_coverable() {
    let host = at("2026-11-25T06:00:00Z");
    let resolve_and_cover = |s: &Scratch, resolve: &dyn Fn(&Scratch)| {
        let main = format!("origin/{}", s.base);
        let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
        assert!(!merged.status.success(), "the merge conflicts: {merged:?}");
        resolve(s);
        s.git(&["add", "-A"]);
        s.git(&["commit", "-q", "--no-edit"]);
        let merge = s.head();
        s.accepted_resolution(&host, &merge, "resolution");
        let cleared = s.cleared();
        assert!(cleared.gaps.is_empty(), "{:?}", cleared.gaps);
        assert_eq!(cleared.resolved, [merge]);
    };

    let s = Scratch::new(&host);
    fs::write(s.c.repo.join("new.txt"), "feature new\n").unwrap();
    s.git(&["add", "new.txt"]);
    s.git(&["commit", "-q", "-m", "add new"]);
    s.reviewed(&host, "h2");
    s.on_main_with(|s| fs::write(s.c.repo.join("new.txt"), "main new\n").unwrap());
    resolve_and_cover(&s, &|s| {
        fs::write(s.c.repo.join("new.txt"), "feature and main new\n").unwrap();
    });

    let s = Scratch::new(&host);
    fs::write(s.c.repo.join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
    s.git(&["add", "bin.dat"]);
    s.git(&["commit", "-q", "-m", "add binary"]);
    s.reviewed(&host, "h2");
    s.on_main_with(|s| fs::write(s.c.repo.join("bin.dat"), [0u8, 9, 9, 9]).unwrap());
    resolve_and_cover(&s, &|s| {
        fs::write(s.c.repo.join("bin.dat"), [0u8, 5, 5, 5]).unwrap();
    });

    let s = Scratch::new(&host);
    s.on_main("m.txt", "base\n");
    s.merge_main(None);
    fs::write(s.c.repo.join("m.txt"), "feature\n").unwrap();
    s.git(&["commit", "-q", "-a", "-m", "modify m"]);
    s.reviewed(&host, "h2");
    s.on_main_with(|s| {
        s.git(&["rm", "-q", "m.txt"]);
    });
    resolve_and_cover(&s, &|s| {
        s.git(&["rm", "-q", "m.txt"]);
    });
}

#[test]
fn a_ref_named_like_the_reports_head_never_redirects_the_record() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.git(&["commit", "-q", "--amend", "-m", "Merge main, amended"]);
    let amended = s.head();
    s.git(&["tag", &merge[..12], &amended]);
    let refused = s.resolution(&host, Some("claude"), &merge[..12], "claude-tag", &[]);
    assert_eq!(
        rules(&refused),
        ["review-resolution"],
        "{:?}",
        refused.violations
    );
    assert_eq!(
        refused.violations[0].message,
        format!(
            "{} resolves to {amended} in {}, a commit it does not name: a ref of that name shadows it",
            &merge[..12],
            s.c.repo.display()
        )
    );
}

#[test]
fn a_resolution_record_binds_only_its_full_merge_sha() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    assert!(s.cleared().gaps.is_empty());
    s.edit_ledger(|ledger| {
        for review in &mut ledger.reviews[2..] {
            review.head = merge[..8].to_owned();
        }
    });
    let prefixed = s.cleared();
    assert!(prefixed.resolved.is_empty());
    assert!(!prefixed.gaps.is_empty());
    assert!(rules(&s.merge(&host)).contains(&"merge-gate"));
}

fn gitlink(s: &Scratch, path: &str, sha: &str) {
    s.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{sha},{path}"),
    ]);
}

const SUB_A: &str = "1111111111111111111111111111111111111111";
const SUB_B: &str = "2222222222222222222222222222222222222222";

fn with_submodule(host: &Fake, ignore: bool, before: Option<&str>) -> Scratch {
    let s = Scratch::new(host);
    s.on_main_with(|s| {
        let ignore = if ignore { "\tignore = all\n" } else { "" };
        fs::write(
            s.c.repo.join(".gitmodules"),
            format!("[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n{ignore}"),
        )
        .unwrap();
        s.git(&["add", ".gitmodules"]);
        if let Some(sha) = before {
            gitlink(s, "sub", sha);
        }
    });
    s.merge_main(None);
    s
}

fn moves_sub(s: &Scratch, to: &str) -> String {
    s.on_main("shared.txt", "main\n");
    let main = format!("origin/{}", s.base);
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
    assert!(!merged.status.success(), "the merge conflicts: {merged:?}");
    fs::write(s.c.repo.join("shared.txt"), "feature and main\n").unwrap();
    s.git(&["add", "shared.txt"]);
    gitlink(s, "sub", to);
    s.git(&["commit", "-q", "--no-edit"]);
    s.head()
}

#[test]
fn a_tree_id_and_a_long_name_are_paths_like_any_other() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    let merge = s.conflicted_with(|s| {
        let (first, second) = (
            s.git(&["rev-parse", "HEAD"]),
            s.git(&["rev-parse", "MERGE_HEAD"]),
        );
        let listed = scratch_git(
            &s.home,
            &s.c.repo,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                "-z",
                &first,
                &second,
            ],
        );
        let tree = String::from_utf8(listed.stdout).unwrap();
        let tree = tree.split('\0').next().unwrap().to_owned();
        fs::write(s.c.repo.join(&tree), "unreviewed\n").unwrap();
        fs::write(s.c.repo.join("tree-id.txt"), &tree).unwrap();
    });
    let tree = fs::read_to_string(s.c.repo.join("tree-id.txt")).unwrap();
    beyond_the_conflict(
        &s,
        &host,
        &merge,
        &s.h1,
        &format!("\"{tree}\", \"tree-id.txt\""),
    );

    let long = "a-long-conflicted-file-name-one.txt";
    let s = Scratch::new(&host);
    s.on_main(long, "base\n");
    s.merge_main(None);
    fs::write(s.c.repo.join(long), "feature\n").unwrap();
    s.git(&["commit", "-q", "-a", "-m", "feature edit"]);
    let h2 = s.reviewed(&host, "h2");
    s.on_main(long, "main\n");
    let main = format!("origin/{}", s.base);
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
    assert!(!merged.status.success(), "the merge conflicts: {merged:?}");
    fs::write(s.c.repo.join(long), "feature and main\n").unwrap();
    fs::write(
        s.c.repo.join("a-long-conflicted-file-name-two.txt"),
        "unreviewed\n",
    )
    .unwrap();
    s.git(&["add", "-A"]);
    s.git(&["commit", "-q", "--no-edit"]);
    let merge = s.head();
    beyond_the_conflict(
        &s,
        &host,
        &merge,
        &h2,
        "\"a-long-conflicted-file-name-two.txt\"",
    );
}

#[test]
fn an_ignored_submodule_never_hides_a_change_beyond_the_conflict() {
    let host = at("2026-11-25T06:00:00Z");
    for (ignore, before) in [(true, Some(SUB_A)), (true, None), (false, Some(SUB_A))] {
        let s = with_submodule(&host, ignore, before);
        let merge = moves_sub(&s, SUB_B);
        beyond_the_conflict(&s, &host, &merge, &s.h1, "\"sub\"");
    }

    let s = with_submodule(&host, true, Some(SUB_A));
    s.git(&["config", "diff.ignoreSubmodules", "all"]);
    s.git(&["config", "submodule.sub.ignore", "all"]);
    let merge = moves_sub(&s, SUB_B);
    beyond_the_conflict(&s, &host, &merge, &s.h1, "\"sub\"");

    let s = with_submodule(&host, true, Some(SUB_A));
    let merge = moves_sub(&s, SUB_B);
    let report = s.c.report("claude-hostile", &merge, &[], "ACCEPT");
    fs::write(
        s.home.join("gitconfig"),
        "[diff]\n\tignoreSubmodules = all\n\tnoprefix = true\n\trenames = copies\n[color]\n\tdiff = always\n",
    )
    .unwrap();
    let repo = s.c.repo.to_str().unwrap();
    let report = report.to_str().unwrap();
    let refused = scratch_cm(
        &s,
        &[
            "review",
            "record",
            STEP,
            report,
            "--reviewer",
            "claude",
            "--kind",
            "resolution",
            "--repo",
            repo,
        ],
    );
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stdout)
            .contains("it changes \"sub\" beyond the files its parents conflict in"),
        "{refused:?}"
    );
}

#[test]
fn a_clean_merge_that_moves_an_ignored_submodule_needs_its_own_review() {
    let host = at("2026-11-25T06:00:00Z");
    let s = with_submodule(&host, true, Some(SUB_A));
    s.on_main("other.txt", "main\n");
    s.merge_main(None);
    gitlink(&s, "sub", SUB_B);
    s.git(&["commit", "-q", "--amend", "--no-edit"]);
    let moved = s.head();
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::Conflicts { commit: moved })
    );
    assert!(rules(&s.merge(&host)).contains(&"merge-gate"));
}

fn file_directory_merge(
    host: &Fake,
    resolve: impl FnOnce(&Scratch, &str, &str),
) -> (Scratch, String, String) {
    let s = Scratch::new(host);
    fs::write(s.c.repo.join("x"), "a file\n").unwrap();
    s.git(&["add", "x"]);
    s.git(&["commit", "-q", "-m", "x is a file"]);
    let h2 = s.reviewed(host, "h2");
    s.on_main_with(|s| {
        fs::create_dir_all(s.c.repo.join("x")).unwrap();
        fs::write(s.c.repo.join("x/inner.txt"), "a directory\n").unwrap();
    });
    let main = format!("origin/{}", s.base);
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
    assert!(!merged.status.success(), "x conflicts: {merged:?}");
    let aside = fs::read_dir(&s.c.repo)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .find(|name| name.starts_with("x~"))
        .unwrap();
    resolve(&s, &aside, &h2);
    s.git(&["add", "-A"]);
    s.git(&["commit", "-q", "--no-edit"]);
    let merge = s.head();
    (s, merge, h2)
}

fn record_through_cm(s: &Scratch, merge: &str) -> std::process::Output {
    let report = s.c.report("claude-file-dir", merge, &[], "ACCEPT");
    scratch_cm(
        s,
        &[
            "review",
            "record",
            STEP,
            report.to_str().unwrap(),
            "--reviewer",
            "claude",
            "--kind",
            "resolution",
            "--repo",
            s.c.repo.to_str().unwrap(),
        ],
    )
}

#[test]
fn a_file_directory_conflict_stands_only_where_the_moved_file_keeps_its_remerge_name() {
    let host = at("2026-11-25T06:00:00Z");
    let (s, merge, h2) = file_directory_merge(&host, |s, aside, _| {
        fs::rename(s.c.repo.join(aside), s.c.repo.join("x.file")).unwrap();
    });
    let refused = record_through_cm(&s, &merge);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let out = String::from_utf8_lossy(&refused.stdout);
    assert!(
        out.contains("\"x.file\"") && out.contains(&format!("\"x~{} (x is a file)\"", &h2[..7])),
        "{out}"
    );

    let (s, merge, _) = file_directory_merge(&host, |s, aside, h2| {
        fs::rename(
            s.c.repo.join(aside),
            s.c.repo.join(format!("x~{} (x is a file)", &h2[..7])),
        )
        .unwrap();
    });
    let kept = record_through_cm(&s, &merge);
    assert!(kept.status.success(), "{kept:?}");

    let (s, merge, _) = file_directory_merge(&host, |s, aside, h2| {
        fs::rename(
            s.c.repo.join(aside),
            s.c.repo.join(format!("x~{} (x is a file)", &h2[..7])),
        )
        .unwrap();
        fs::write(s.c.repo.join(format!("x~{h2}")), "added in the merge\n").unwrap();
    });
    let added = record_through_cm(&s, &merge);
    assert!(added.status.success(), "{added:?}");
}

#[test]
fn an_abbreviated_head_reaches_a_covered_merge_while_github_is_unread() {
    let host = at("2026-11-25T06:00:00Z");
    let gate = |s: &Scratch, head: &str| {
        let ci = s.c.write("ci-green.md", "CI green at the head.\n");
        let forge = FakeForge {
            down: true,
            ..FakeForge::open(head, &[])
        };
        commands::advance_gated(
            &host,
            &s.c.root,
            &target(),
            commands::Advance {
                to: &Phase::Merged,
                given: &Evidence {
                    sha: Some(s.head()),
                    ..Evidence::default()
                },
                file: None,
                repo: Some(&s.c.repo),
                part: None,
                by: &[],
            },
            &MergeGate {
                forge: Some(&forge),
                head: Some(head.to_owned()),
                ci_verified: Some(ci),
                ..MergeGate::default()
            },
        )
        .unwrap()
    };

    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    let short = &merge[..12];
    assert!(rules(&gate(&s, "deadbeefdead")).contains(&"merge-gate"));
    s.git(&["tag", short, &s.h1]);
    let shadowed = gate(&s, short);
    assert!(
        rules(&shadowed).contains(&"merge-gate"),
        "{:?}",
        shadowed.violations
    );
    assert_eq!(s.c.state(), Phase::PrOpen);
    s.git(&["tag", "-d", short]);
    let merged = gate(&s, short);
    assert!(merged.ok(), "{:?}", merged.violations);

    let s = Scratch::new(&host);
    let merged = gate(&s, &s.h1[..12]);
    assert!(merged.ok(), "{:?}", merged.violations);
}

fn offline_cm(s: &Scratch, args: &[&str]) -> std::process::Output {
    offline_cm_in(s, None, args)
}

fn offline_cm_in<A: AsRef<std::ffi::OsStr>>(
    s: &Scratch,
    cwd: Option<&Path>,
    args: &[A],
) -> std::process::Output {
    offline_cm_env(s, cwd, &[], args)
}

fn offline_cm_env<A: AsRef<std::ffi::OsStr>>(
    s: &Scratch,
    cwd: Option<&Path>,
    env: &[(&str, &str)],
    args: &[A],
) -> std::process::Output {
    let bin = s.home.join("bin");
    let gh = bin.join("gh");
    if !gh.exists() {
        fs::create_dir_all(&bin).unwrap();
        support::write_executable(&gh, "#!/bin/sh\necho 'gh: offline' >&2\nexit 1\n");
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
        .arg("-C")
        .arg(&s.c.root)
        .args(args)
        .env("HOME", &s.home)
        .env("TMPDIR", &s.home)
        .env("GIT_CONFIG_GLOBAL", s.home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", path)
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn offline_merge(s: &Scratch, head: &str, repo: &Path) -> std::process::Output {
    offline_merge_in(s, None, head, repo.as_os_str())
}

fn offline_merge_in(
    s: &Scratch,
    cwd: Option<&Path>,
    head: &str,
    repo: &std::ffi::OsStr,
) -> std::process::Output {
    offline_merge_env(s, cwd, &[], head, repo)
}

fn offline_merge_env(
    s: &Scratch,
    cwd: Option<&Path>,
    env: &[(&str, &str)],
    head: &str,
    repo: &std::ffi::OsStr,
) -> std::process::Output {
    use std::ffi::OsStr;
    let ci = s.c.write("ci-green.md", "CI green at the head.\n");
    let sha = s.head();
    offline_cm_env(
        s,
        cwd,
        env,
        &[
            OsStr::new("advance"),
            OsStr::new(STEP),
            OsStr::new("merged"),
            OsStr::new("--sha"),
            OsStr::new(&sha),
            OsStr::new("--repo"),
            repo,
            OsStr::new("--head"),
            OsStr::new(head),
            OsStr::new("--ci-verified"),
            ci.as_os_str(),
        ],
    )
}

#[derive(Clone, Copy, Debug)]
enum Knob {
    Relative,
    IgnoreSubmodules,
    SubmoduleIgnore,
    Textconv,
}

#[derive(Clone, Copy, Debug)]
enum Scope {
    Global,
    Repo,
}

fn knob_history(host: &Fake) -> Scratch {
    let s = Scratch::new(host);
    s.on_main_with(|s| {
        fs::create_dir_all(s.c.repo.join("dir")).unwrap();
        fs::write(s.c.repo.join("dir/keep.txt"), "kept\n").unwrap();
        fs::write(s.c.repo.join("data.dat"), "data one\n").unwrap();
        fs::write(
            s.c.repo.join(".gitmodules"),
            "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n",
        )
        .unwrap();
        s.git(&["add", "-A"]);
        gitlink(s, "sub", SUB_A);
    });
    s.merge_main(None);
    s
}

fn hide(s: &Scratch, knob: Knob) -> &'static str {
    match knob {
        Knob::Relative => {
            fs::write(s.c.repo.join("evil.txt"), "outside dir/\n").unwrap();
            s.git(&["add", "evil.txt"]);
            "\"evil.txt\""
        }
        Knob::IgnoreSubmodules | Knob::SubmoduleIgnore => {
            gitlink(s, "sub", SUB_B);
            "\"sub\""
        }
        Knob::Textconv => {
            fs::write(s.c.repo.join("data.dat"), "data two\n").unwrap();
            s.git(&["add", "data.dat"]);
            "\"data.dat\""
        }
    }
}

fn turn_hostile(s: &Scratch, knob: Knob, scope: Scope) {
    let pairs: &[(&str, &str)] = match knob {
        Knob::Relative => &[("diff.relative", "true")],
        Knob::IgnoreSubmodules => &[("diff.ignoreSubmodules", "all")],
        Knob::SubmoduleIgnore => &[("submodule.sub.ignore", "all")],
        Knob::Textconv => &[("diff.lossy.textconv", "sh -c 'echo same'")],
    };
    let attributes = "*.dat diff=lossy\n";
    match scope {
        Scope::Global => {
            let mut config = String::new();
            for (key, value) in pairs {
                let (section, name) = key.rsplit_once('.').unwrap();
                let section = match section.split_once('.') {
                    Some((outer, inner)) => format!("{outer} \"{inner}\""),
                    None => section.to_owned(),
                };
                config.push_str(&format!("[{section}]\n\t{name} = {value}\n"));
            }
            if matches!(knob, Knob::Textconv) {
                let file = s.home.join("attributes");
                fs::write(&file, attributes).unwrap();
                config.push_str(&format!("[core]\n\tattributesFile = {}\n", file.display()));
            }
            fs::write(s.home.join("gitconfig"), config).unwrap();
        }
        Scope::Repo => {
            for (key, value) in pairs {
                s.git(&["config", key, value]);
            }
            if matches!(knob, Knob::Textconv) {
                fs::create_dir_all(s.c.repo.join(".git/info")).unwrap();
                fs::write(s.c.repo.join(".git/info/attributes"), attributes).unwrap();
            }
        }
    }
}

#[test]
fn no_configuration_or_attribute_hides_a_change_from_record_walk_or_waiver() {
    let host = at("2026-11-25T06:00:00Z");
    let knobs = [
        Knob::Relative,
        Knob::IgnoreSubmodules,
        Knob::SubmoduleIgnore,
        Knob::Textconv,
    ];
    for knob in knobs {
        for scope in [Scope::Global, Scope::Repo] {
            for subdir in [false, true] {
                let row = format!("{knob:?} {scope:?} subdir={subdir}");

                let s = knob_history(&host);
                s.on_main("shared.txt", "main\n");
                let main = format!("origin/{}", s.base);
                let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", &main]);
                assert!(!merged.status.success(), "{row}: the merge conflicts");
                fs::write(s.c.repo.join("shared.txt"), "feature and main\n").unwrap();
                s.git(&["add", "shared.txt"]);
                let shown = hide(&s, knob);
                s.git(&["commit", "-q", "--no-edit"]);
                let merge = s.head();
                turn_hostile(&s, knob, scope);
                let repo = if subdir {
                    s.c.repo.join("dir")
                } else {
                    s.c.repo.clone()
                };
                let report = s.c.report("claude-hostile", &merge, &[], "ACCEPT");
                let record = offline_cm(
                    &s,
                    &[
                        "review",
                        "record",
                        STEP,
                        report.to_str().unwrap(),
                        "--reviewer",
                        "claude",
                        "--kind",
                        "resolution",
                        "--repo",
                        repo.to_str().unwrap(),
                    ],
                );
                assert_eq!(record.status.code(), Some(1), "{row}: record {record:?}");
                assert!(
                    String::from_utf8_lossy(&record.stdout).contains(&format!(
                        "it changes {shown} beyond the files its parents conflict in"
                    )),
                    "{row}: record {record:?}"
                );
                s.edit_ledger(|ledger| as_resolutions(ledger, &s.h1, &merge));
                let walk = offline_merge(&s, &merge, &repo);
                assert_eq!(walk.status.code(), Some(1), "{row}: walk {walk:?}");
                assert!(
                    String::from_utf8_lossy(&walk.stdout).contains(&format!(
                        "resolution reviews do not clear it: it changes {shown} beyond the files its parents conflict in"
                    )),
                    "{row}: walk {walk:?}"
                );
                assert_eq!(s.c.state(), Phase::PrOpen, "{row}: walk");

                let s = knob_history(&host);
                s.on_main("other.txt", "main\n");
                s.merge_main(None);
                hide(&s, knob);
                s.git(&["commit", "-q", "--amend", "--no-edit"]);
                let clean = s.head();
                turn_hostile(&s, knob, scope);
                let repo = if subdir {
                    s.c.repo.join("dir")
                } else {
                    s.c.repo.clone()
                };
                let waiver = offline_merge(&s, &clean, &repo);
                assert_eq!(waiver.status.code(), Some(1), "{row}: waiver {waiver:?}");
                assert!(
                    String::from_utf8_lossy(&waiver.stdout).contains("needs its own review"),
                    "{row}: waiver {waiver:?}"
                );
            }
        }
    }
}

fn signed_and_shown(s: &Scratch) {
    let gpg = s.home.join("bin").join("gpg-stub");
    fs::create_dir_all(gpg.parent().unwrap()).unwrap();
    support::write_executable(
        &gpg,
        r#"#!/bin/sh
cat >/dev/null
case " $* " in
*" --verify "*) echo 'gpg: a signature shown' >&2; exit 1 ;;
esac
echo '[GNUPG:] SIG_CREATED ' >&2
printf '%s\n' '-----BEGIN PGP SIGNATURE-----' '' 'stub' '-----END PGP SIGNATURE-----'
"#,
    );
    s.git(&["config", "gpg.program", gpg.to_str().unwrap()]);
    s.git(&["config", "commit.gpgsign", "true"]);
    s.git(&["config", "log.showSignature", "true"]);
}

#[test]
fn a_shown_signature_is_no_change_to_a_resolution_and_still_no_clean_update() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    signed_and_shown(&s);
    s.on_main("other.txt", "main\n");
    let clean = s.merge_main(None);
    let shown = s.git(&["show", "--remerge-diff", "--format=", &clean]);
    assert!(shown.contains("gpg: a signature shown"), "{shown}");
    let refused = offline_merge(&s, &clean, &s.c.repo);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains(&format!(
            "{} resolved merge conflicts; it needs its own review",
            &clean[..12]
        )),
        "{refused:?}"
    );
    assert_eq!(s.c.state(), Phase::PrOpen);

    let s = Scratch::new(&host);
    signed_and_shown(&s);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "signed");
    let merged = offline_merge(&s, &merge, &s.c.repo);
    assert!(merged.status.success(), "{merged:?}");
}

#[derive(Clone, Copy, Debug)]
enum Rewrite {
    Replace,
    Graft,
}

fn rewritten(host: &Fake, rewrite: Rewrite, conflicted: bool) -> (Scratch, String) {
    let s = Scratch::new(host);
    let held = match rewrite {
        Rewrite::Replace => {
            s.on_main(
                if conflicted {
                    "shared.txt"
                } else {
                    "other.txt"
                },
                "main\n",
            );
            None
        }
        Rewrite::Graft => {
            s.on_main_with(|s| {
                if conflicted {
                    fs::write(s.c.repo.join("shared.txt"), "main\n").unwrap();
                }
                fs::write(s.c.repo.join("evil.txt"), "smuggled\n").unwrap();
            });
            let held = s.git(&["rev-parse", &s.base]);
            s.on_main_with(|s| fs::remove_file(s.c.repo.join("evil.txt")).unwrap());
            Some(held)
        }
    };
    let honest = s.merge_main(conflicted.then_some("feature and main\n"));
    fs::write(s.c.repo.join("evil.txt"), "smuggled\n").unwrap();
    s.git(&["add", "evil.txt"]);
    s.git(&["commit", "-q", "--amend", "--no-edit"]);
    let merge = s.head();
    match held {
        None => {
            s.git(&["replace", &merge, &honest]);
        }
        Some(held) => {
            let parents = s.git(&["rev-list", "--parents", "-n", "1", &merge]);
            let first = parents.split_whitespace().nth(1).unwrap();
            let info = s.c.repo.join(".git").join("info");
            fs::create_dir_all(&info).unwrap();
            fs::write(info.join("grafts"), format!("{merge} {first} {held}\n")).unwrap();
        }
    }
    let seen = s.git(&["show", "--remerge-diff", "--format=", "--name-only", &merge]);
    let hidden = if conflicted { "shared.txt" } else { "" };
    assert_eq!(seen, hidden, "{rewrite:?} hides evil.txt from plain git");
    (s, merge)
}

#[test]
fn no_replace_ref_or_graft_rewrites_what_record_walk_or_waiver_read() {
    let host = at("2026-11-25T06:00:00Z");
    for rewrite in [Rewrite::Replace, Rewrite::Graft] {
        let (s, merge) = rewritten(&host, rewrite, true);
        let report = s.c.report("claude-rewritten", &merge, &[], "ACCEPT");
        let record = offline_cm(
            &s,
            &[
                "review",
                "record",
                STEP,
                report.to_str().unwrap(),
                "--reviewer",
                "claude",
                "--kind",
                "resolution",
                "--repo",
                s.c.repo.to_str().unwrap(),
            ],
        );
        assert_eq!(
            record.status.code(),
            Some(1),
            "{rewrite:?}: record {record:?}"
        );
        assert!(
            String::from_utf8_lossy(&record.stdout)
                .contains("it changes \"evil.txt\" beyond the files its parents conflict in"),
            "{rewrite:?}: record {record:?}"
        );
        s.edit_ledger(|ledger| as_resolutions(ledger, &s.h1, &merge));
        let walk = offline_merge(&s, &merge, &s.c.repo);
        assert_eq!(walk.status.code(), Some(1), "{rewrite:?}: walk {walk:?}");
        assert!(
            String::from_utf8_lossy(&walk.stdout).contains(
                "resolution reviews do not clear it: it changes \"evil.txt\" beyond the files its parents conflict in"
            ),
            "{rewrite:?}: walk {walk:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{rewrite:?}: walk");

        let (s, clean) = rewritten(&host, rewrite, false);
        let waiver = offline_merge(&s, &clean, &s.c.repo);
        assert_eq!(
            waiver.status.code(),
            Some(1),
            "{rewrite:?}: waiver {waiver:?}"
        );
        assert!(
            String::from_utf8_lossy(&waiver.stdout).contains(&format!(
                "{} resolved merge conflicts; it needs its own review",
                &clean[..12]
            )),
            "{rewrite:?}: waiver {waiver:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{rewrite:?}: waiver");
    }
}

#[test]
fn a_clean_update_that_a_replace_ref_or_graft_disguised_is_waived() {
    let host = at("2026-11-25T06:00:00Z");
    for rewrite in [Rewrite::Replace, Rewrite::Graft] {
        let s = Scratch::new(&host);
        s.on_main("other.txt", "main\n");
        let clean = s.merge_main(None);
        match rewrite {
            Rewrite::Replace => {
                fs::write(s.c.repo.join("evil.txt"), "seen\n").unwrap();
                s.git(&["add", "evil.txt"]);
                s.git(&["commit", "-q", "--amend", "--no-edit"]);
                let disguise = s.head();
                s.git(&["reset", "-q", "--hard", &clean]);
                s.git(&["replace", &clean, &disguise]);
                let seen = s.git(&["show", "--remerge-diff", "--format=", "--name-only", &clean]);
                assert_eq!(seen, "evil.txt", "{rewrite:?}");
            }
            Rewrite::Graft => {
                let info = s.c.repo.join(".git").join("info");
                fs::create_dir_all(&info).unwrap();
                fs::write(info.join("grafts"), format!("{clean} {}\n", s.h1)).unwrap();
                let seen = s.git(&["rev-list", "--parents", "-n", "1", &clean]);
                assert_eq!(seen, format!("{clean} {}", s.h1), "{rewrite:?}");
            }
        }
        let merged = offline_merge(&s, &clean, &s.c.repo);
        assert!(merged.status.success(), "{rewrite:?}: {merged:?}");
    }
}

fn logging_git(s: &Scratch) -> PathBuf {
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', r"'\''"));
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .unwrap();
    let log = s.home.join("git.log");
    let bin = s.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    support::write_executable(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\nprintf '%s\\t%s\\t%s\\t%s\\t%s\\n' \"${{GIT_NO_REPLACE_OBJECTS-}}\" \"${{GIT_GRAFT_FILE-}}\" \"${{GIT_DIR-}}\" \"${{GIT_CONFIG_GLOBAL-}}\" \"$*\" >> {}\nexec {} \"$@\"\n",
            quote(&log),
            quote(&git)
        ),
    );
    log
}

#[derive(Default)]
struct Logged {
    checkout: Vec<String>,
    isolated: Vec<(PathBuf, String)>,
}

// `cm advance` reads `--sha` itself, outside the gate.
fn logged(s: &Scratch, log: &Path, into: &mut Logged) {
    let canonical = fs::canonicalize(&s.c.repo).unwrap();
    let checkouts = [
        format!("-C {} ", s.c.repo.display()),
        format!("-C {} ", canonical.display()),
    ];
    let sha = format!("rev-parse --verify --quiet {}^{{commit}}", s.head());
    for line in fs::read_to_string(log).unwrap().lines() {
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        let [replace, graft, dir, global, args] = fields[..] else {
            panic!("{line}");
        };
        let uncached = args.strip_prefix("-c core.commitGraph=false -c core.multiPackIndex=false ");
        let ignores = replace == "1" && graft == "/dev/null/grafts" && uncached.is_some();
        let args = uncached.unwrap_or(args);
        if !dir.is_empty() {
            assert!(ignores && global == "/dev/null", "{line}");
            into.isolated.push((PathBuf::from(dir), args.to_owned()));
        } else if let Some(call) = checkouts
            .iter()
            .find_map(|checkout| args.strip_prefix(checkout.as_str()))
        {
            if ignores {
                into.checkout.push(call.to_owned());
            } else {
                assert_eq!(
                    call, sha,
                    "a git command in the checkout reads rewritten history"
                );
            }
        }
    }
}

#[test]
fn every_gate_git_command_ignores_rewritten_history_and_merge_listings_run_isolated() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let log = logging_git(&s);
    let report = s.c.report("claude-logged", &merge, &[], "ACCEPT");
    let record = offline_cm(
        &s,
        &[
            "review",
            "record",
            STEP,
            report.to_str().unwrap(),
            "--reviewer",
            "claude",
            "--kind",
            "resolution",
            "--repo",
            s.c.repo.to_str().unwrap(),
        ],
    );
    assert!(record.status.success(), "{record:?}");
    let short = &merge[..12];
    let walk = offline_merge(&s, short, &s.c.repo);
    assert_eq!(walk.status.code(), Some(1), "{walk:?}");
    assert!(
        String::from_utf8_lossy(&walk.stdout).contains("no astra resolution ACCEPT"),
        "{walk:?}"
    );
    let mut calls = Logged::default();
    logged(&s, &log, &mut calls);

    let s = Scratch::new(&host);
    s.on_main("other.txt", "main\n");
    let clean = s.merge_main(None);
    let log = logging_git(&s);
    let waiver = offline_merge(&s, &clean, &s.c.repo);
    assert!(waiver.status.success(), "{waiver:?}");
    logged(&s, &log, &mut calls);

    let listing = "show --remerge-diff --format= --no-renames --ignore-submodules=none --no-ext-diff --no-textconv --no-relative --no-color";
    for site in [
        "rev-parse --verify refs/remotes/origin/HEAD^{commit}".to_owned(),
        format!("rev-parse --verify {merge}^{{commit}}"),
        format!("rev-parse --verify --quiet {short}^{{commit}}"),
        format!("rev-parse --verify --quiet {clean}^{{commit}}"),
        "rev-parse --path-format=absolute --git-path objects".to_owned(),
        "rev-parse --show-object-format".to_owned(),
        "rev-list --parents -n 1 ".to_owned(),
        "merge-base --is-ancestor ".to_owned(),
        format!("{listing} {clean}"),
    ] {
        assert!(
            calls.checkout.iter().any(|call| call.starts_with(&site)),
            "{site}: {:#?}",
            calls.checkout
        );
    }
    for site in [
        "init --bare --quiet --template= --object-format=sha1".to_owned(),
        "merge-tree --write-tree ".to_owned(),
        format!("{listing} --no-show-signature --name-only -z {merge}"),
        format!("{listing} {clean}"),
    ] {
        assert!(
            calls
                .isolated
                .iter()
                .any(|(_, call)| call.starts_with(&site)),
            "{site}: {:#?}",
            calls.isolated
        );
    }
    assert!(
        !calls
            .checkout
            .iter()
            .any(|call| call.starts_with("merge-tree") || call.contains("--name-only")),
        "{:#?}",
        calls.checkout
    );
    for (dir, _) in &calls.isolated {
        assert!(!dir.starts_with(&s.c.repo) && !dir.exists(), "{dir:?}");
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MergeKnob {
    Committed,
    InfoAttributes,
    WorktreeAttributes,
    GlobalAttributes,
    AttrSource,
    RepoConfig,
    GlobalConfig,
    ConfigCount,
    ConfigParameters,
}

type Knobs<'a> = &'a [MergeKnob];
type Setting<'a> = (&'a str, &'a str);

fn plain_git(s: &Scratch, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(&s.c.repo)
        .args(args)
        .env("HOME", &s.home)
        .env("TMPDIR", &s.home)
        .env("GIT_CONFIG_GLOBAL", s.home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn turn_merge_knobs(
    s: &Scratch,
    knobs: Knobs,
    attribute: &str,
    (key, value): Setting,
) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    for knob in knobs {
        match knob {
            MergeKnob::Committed => {}
            MergeKnob::InfoAttributes => {
                let info = s.c.repo.join(".git").join("info");
                fs::create_dir_all(&info).unwrap();
                fs::write(info.join("attributes"), attribute).unwrap();
            }
            MergeKnob::WorktreeAttributes => {
                fs::write(s.c.repo.join(".gitattributes"), attribute).unwrap();
            }
            MergeKnob::GlobalAttributes => {
                let file = s.home.join("attributes");
                fs::write(&file, attribute).unwrap();
                fs::write(
                    s.home.join("gitconfig"),
                    format!("[core]\n\tattributesFile = {}\n", file.display()),
                )
                .unwrap();
            }
            MergeKnob::AttrSource => {
                s.git(&["checkout", "-q", "--orphan", "attributes"]);
                s.git(&["rm", "-r", "-f", "-q", "."]);
                fs::write(s.c.repo.join(".gitattributes"), attribute).unwrap();
                s.git(&["add", ".gitattributes"]);
                s.git(&["commit", "-q", "-m", "attributes"]);
                env.push(("GIT_ATTR_SOURCE", s.head()));
                s.git(&["checkout", "-q", "-f", "feature"]);
                s.git(&["branch", "-q", "-D", "attributes"]);
            }
            MergeKnob::RepoConfig => {
                s.git(&["config", key, value]);
            }
            MergeKnob::GlobalConfig => {
                let (section, name) = key.rsplit_once('.').unwrap();
                let section = match section.split_once('.') {
                    Some((outer, inner)) => format!("{outer} \"{inner}\""),
                    None => section.to_owned(),
                };
                fs::write(
                    s.home.join("gitconfig"),
                    format!("[{section}]\n\t{name} = {value}\n"),
                )
                .unwrap();
            }
            MergeKnob::ConfigCount => {
                env.extend([
                    ("GIT_CONFIG_COUNT", "1".to_owned()),
                    ("GIT_CONFIG_KEY_0", key.to_owned()),
                    ("GIT_CONFIG_VALUE_0", value.to_owned()),
                ]);
            }
            MergeKnob::ConfigParameters => {
                env.push(("GIT_CONFIG_PARAMETERS", format!("'{key}'='{value}'")));
            }
        }
    }
    env
}

fn merge_by_hand(s: &Scratch, files: &[(&str, &str)]) -> String {
    let main = format!("origin/{}", s.base);
    scratch_git(
        &s.home,
        &s.c.repo,
        &["merge", "-q", "--no-commit", "--no-ff", &main],
    );
    for (file, text) in files {
        fs::write(s.c.repo.join(file), text).unwrap();
        s.git(&["add", file]);
    }
    s.git(&["commit", "-q", "--no-edit"]);
    s.head()
}

fn widened_merge(host: &Fake, committed: Option<&str>) -> (Scratch, String, String) {
    let s = Scratch::new(host);
    let lines: String = (1..=10).map(|n| format!("line {n}\n")).collect();
    s.on_main_with(|s| fs::write(s.c.repo.join("both.txt"), &lines).unwrap());
    s.merge_main(None);
    let feature = lines.replace("line 1\n", "feature 1\n");
    fs::write(s.c.repo.join("both.txt"), &feature).unwrap();
    s.git(&["commit", "-q", "-am", "feature 1"]);
    let h2 = s.reviewed(host, "h2");
    s.on_main_with(|s| {
        fs::write(
            s.c.repo.join("both.txt"),
            lines.replace("line 9\n", "main 9\n"),
        )
        .unwrap();
        fs::write(s.c.repo.join("shared.txt"), "main\n").unwrap();
        if let Some(attribute) = committed {
            fs::write(s.c.repo.join(".gitattributes"), attribute).unwrap();
        }
    });
    let both = feature
        .replace("line 9\n", "main 9\n")
        .replace("line 5\n", "rewritten 5\n");
    let merge = merge_by_hand(
        &s,
        &[("shared.txt", "feature and main\n"), ("both.txt", &both)],
    );
    (s, h2, merge)
}

#[test]
fn no_local_or_committed_merge_attribute_or_setting_widens_a_conflict_or_hides_a_resolution() {
    let host = at("2026-11-25T06:00:00Z");
    let none = ("", "");
    let binary = ("merge.default", "binary");
    let fail = ("merge.fail.driver", "false");
    let minus = "both.txt -merge\n";
    let widening: [(Knobs, &str, Setting); 10] = [
        (&[MergeKnob::Committed], minus, none),
        (&[MergeKnob::InfoAttributes], minus, none),
        (&[MergeKnob::WorktreeAttributes], minus, none),
        (&[MergeKnob::GlobalAttributes], minus, none),
        (&[MergeKnob::AttrSource], minus, none),
        (
            &[MergeKnob::InfoAttributes, MergeKnob::RepoConfig],
            "both.txt merge=fail\n",
            fail,
        ),
        (&[MergeKnob::RepoConfig], "", binary),
        (&[MergeKnob::GlobalConfig], "", binary),
        (&[MergeKnob::ConfigCount], "", binary),
        (&[MergeKnob::ConfigParameters], "", binary),
    ];
    for (knobs, attribute, config) in widening {
        let row = format!("{knobs:?} {attribute:?} {config:?}");
        let committed = knobs.contains(&MergeKnob::Committed).then_some(attribute);
        let (s, h2, merge) = widened_merge(&host, committed);
        let parents = s.git(&["rev-list", "--parents", "-n", "1", &merge]);
        let parents: Vec<&str> = parents.split_whitespace().skip(1).collect();
        let env = turn_merge_knobs(&s, knobs, attribute, config);
        let env: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let conflicted = plain_git(
            &s,
            &env,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                parents[0],
                parents[1],
            ],
        );
        assert!(
            String::from_utf8_lossy(&conflicted.stdout).contains("both.txt"),
            "{row}: the checkout's conflict is widened {conflicted:?}"
        );
        let report = s.c.report("claude-widened", &merge, &[], "ACCEPT");
        let record = offline_cm_env(
            &s,
            None,
            &env,
            &[
                "review",
                "record",
                STEP,
                report.to_str().unwrap(),
                "--reviewer",
                "claude",
                "--kind",
                "resolution",
                "--repo",
                s.c.repo.to_str().unwrap(),
            ],
        );
        let beyond = "it changes \"both.txt\" beyond the files its parents conflict in";
        assert_eq!(record.status.code(), Some(1), "{row}: record {record:?}");
        assert!(
            String::from_utf8_lossy(&record.stdout).contains(beyond),
            "{row}: record {record:?}"
        );
        s.edit_ledger(|ledger| as_resolutions(ledger, &h2, &merge));
        let walk = offline_merge_env(&s, None, &env, &merge, s.c.repo.as_os_str());
        assert_eq!(walk.status.code(), Some(1), "{row}: walk {walk:?}");
        assert!(
            String::from_utf8_lossy(&walk.stdout)
                .contains(&format!("resolution reviews do not clear it: {beyond}")),
            "{row}: walk {walk:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{row}: walk");
    }

    let keep = ("merge.keep.driver", "true");
    let union = "shared.txt merge=union\n";
    let hiding: [(Knobs, &str, Setting, &str); 7] = [
        (&[MergeKnob::Committed], union, none, "feature\nmain\n"),
        (&[MergeKnob::InfoAttributes], union, none, "feature\nmain\n"),
        (
            &[MergeKnob::WorktreeAttributes],
            union,
            none,
            "feature\nmain\n",
        ),
        (
            &[MergeKnob::GlobalAttributes],
            union,
            none,
            "feature\nmain\n",
        ),
        (&[MergeKnob::AttrSource], union, none, "feature\nmain\n"),
        (
            &[MergeKnob::InfoAttributes, MergeKnob::RepoConfig],
            "shared.txt merge=keep\n",
            keep,
            "feature\n",
        ),
        (
            &[MergeKnob::InfoAttributes, MergeKnob::ConfigCount],
            "shared.txt merge=keep\n",
            keep,
            "feature\n",
        ),
    ];
    for (knobs, attribute, config, resolution) in hiding {
        let row = format!("{knobs:?} {attribute:?} {config:?}");
        let s = Scratch::new(&host);
        s.on_main_with(|s| {
            fs::write(s.c.repo.join("shared.txt"), "main\n").unwrap();
            if knobs.contains(&MergeKnob::Committed) {
                fs::write(s.c.repo.join(".gitattributes"), attribute).unwrap();
            }
        });
        let merge = merge_by_hand(&s, &[("shared.txt", resolution)]);
        let env = turn_merge_knobs(&s, knobs, attribute, config);
        let env: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let shown = plain_git(&s, &env, &["show", "--remerge-diff", "--format=", &merge]);
        assert!(
            shown.status.success() && shown.stdout.is_empty(),
            "{row}: the resolution is hidden from the checkout {shown:?}"
        );
        let waiver = offline_merge_env(&s, None, &env, &merge, s.c.repo.as_os_str());
        assert_eq!(waiver.status.code(), Some(1), "{row}: waiver {waiver:?}");
        assert!(
            String::from_utf8_lossy(&waiver.stdout).contains(&format!(
                "{} resolved merge conflicts; it needs its own review",
                &merge[..12]
            )),
            "{row}: waiver {waiver:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{row}: waiver");
    }
}

fn sha1(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let (tool, args): (&str, &[&str]) = if Command::new("sha1sum").arg("--version").output().is_ok()
    {
        ("sha1sum", &[])
    } else {
        ("shasum", &["-a", "1"])
    };
    let mut child = Command::new(tool)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let sum = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    raw_oid(&sum[..40])
}

fn raw_oid(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|at| u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).unwrap())
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Cache {
    CommitGraph,
    MultiPackIndex,
}

fn forge(s: &Scratch, cache: Cache, commit: &str, honest: &str) {
    let objects = s.c.repo.join(".git").join("objects");
    let (path, table, width, copied) = match cache {
        Cache::CommitGraph => {
            s.git(&["commit-graph", "write", "--reachable"]);
            (objects.join("info").join("commit-graph"), 8, 36, 28)
        }
        Cache::MultiPackIndex => {
            s.git(&["repack", "-a", "-d", "-q"]);
            s.git(&["multi-pack-index", "write"]);
            (objects.join("pack").join("multi-pack-index"), 12, 8, 8)
        }
    };
    let mut file = fs::read(&path).unwrap();
    let chunk = |id: &[u8]| {
        (0..usize::from(file[6]))
            .map(|at| table + 12 * at)
            .find(|&at| &file[at..at + 4] == id)
            .map(|at| u64::from_be_bytes(file[at + 4..at + 12].try_into().unwrap()) as usize)
            .unwrap()
    };
    let (oids, fanout) = (chunk(b"OIDL"), chunk(b"OIDF"));
    let entries = match cache {
        Cache::CommitGraph => chunk(b"CDAT"),
        Cache::MultiPackIndex => chunk(b"OOFF"),
    };
    let count = u32::from_be_bytes(file[fanout + 4 * 255..fanout + 4 * 256].try_into().unwrap());
    let index = |sha: &str| {
        let oid = raw_oid(sha);
        (0..count as usize)
            .find(|at| file[oids + 20 * at..oids + 20 * at + 20] == oid[..])
            .unwrap()
    };
    let (from, to) = (
        entries + width * index(honest),
        entries + width * index(commit),
    );
    file.copy_within(from..from + copied, to);
    let end = file.len() - 20;
    let sum = sha1(&file[..end]);
    file[end..].copy_from_slice(&sum);
    fs::remove_file(&path).unwrap();
    fs::write(&path, file).unwrap();
    let honest_parents = s.git(&["rev-list", "--parents", "-n", "1", honest]);
    let parents = s.git(&["rev-list", "--parents", "-n", "1", commit]);
    assert_eq!(
        parents.split_once(' ').unwrap().1,
        honest_parents.split_once(' ').unwrap().1,
        "{cache:?}"
    );
    let listed = |sha: &str| s.git(&["show", "--remerge-diff", "--format=", "--name-only", sha]);
    assert_eq!(listed(commit), listed(honest), "{cache:?}");
}

fn forged_history(
    host: &Fake,
    beyond: bool,
    resolution: Option<&str>,
) -> (Scratch, String, String) {
    let s = Scratch::new(host);
    s.on_main(
        if resolution.is_some() {
            "shared.txt"
        } else {
            "other.txt"
        },
        "main\n",
    );
    s.git(&["checkout", "-q", "-b", "honest"]);
    let honest = s.merge_main(resolution);
    s.git(&["checkout", "-q", "feature"]);
    if !beyond {
        s.commit("evil.txt", "unreviewed\n");
    }
    let mut merge = s.merge_main(resolution);
    if beyond {
        fs::write(s.c.repo.join("evil.txt"), "beyond\n").unwrap();
        s.git(&["add", "evil.txt"]);
        s.git(&["commit", "-q", "--amend", "--no-edit"]);
        merge = s.head();
    }
    (s, merge, honest)
}

fn hand_written_resolutions(s: &Scratch, merge: &str) {
    let dir = "astra-review-7-hand";
    let report = s.c.report(dir, merge, &[], "ACCEPT");
    s.c.receipt(dir, merge, "gpt-6-astra", "xhigh");
    let report = cm::evidence::CampaignFile::resolve(&s.c.root, &report).unwrap();
    s.edit_ledger(|ledger| {
        as_resolutions(ledger, &s.h1, merge);
        for review in ledger.reviews.iter_mut().filter(|review| {
            review.kind == verdict::ReviewKind::Resolution
                && review.reviewer.as_deref() == Some("astra")
        }) {
            review.report = report.clone();
        }
    });
}

fn record_claude_resolution(s: &Scratch, merge: &str) -> std::process::Output {
    record_claude_resolution_env(s, &[], merge)
}

fn record_claude_resolution_env(
    s: &Scratch,
    env: &[(&str, &str)],
    merge: &str,
) -> std::process::Output {
    let report = s.c.report("claude-forged", merge, &[], "ACCEPT");
    offline_cm_env(
        s,
        None,
        env,
        &[
            "review",
            "record",
            STEP,
            report.to_str().unwrap(),
            "--reviewer",
            "claude",
            "--kind",
            "resolution",
            "--repo",
            s.c.repo.to_str().unwrap(),
        ],
    )
}

#[test]
fn no_cache_of_the_checkout_changes_what_the_gate_reads_for_a_commit() {
    let host = at("2026-11-25T06:00:00Z");
    let resolved = Some("feature and main\n");
    for cache in [Cache::CommitGraph, Cache::MultiPackIndex] {
        let (s, merge, honest) = forged_history(&host, false, resolved);
        forge(&s, cache, &merge, &honest);
        let record = record_claude_resolution(&s, &merge);
        assert_eq!(
            record.status.code(),
            Some(1),
            "{cache:?}: record {record:?}"
        );
        assert!(
            String::from_utf8_lossy(&record.stdout).contains("does not clear the gate"),
            "{cache:?}: record {record:?}"
        );
        hand_written_resolutions(&s, &merge);
        let walk = offline_merge(&s, &merge, &s.c.repo);
        assert_eq!(walk.status.code(), Some(1), "{cache:?}: walk {walk:?}");
        assert!(
            String::from_utf8_lossy(&walk.stdout).contains("is not a merge from the base branch"),
            "{cache:?}: walk {walk:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{cache:?}: walk");

        let (s, merge, honest) = forged_history(&host, true, resolved);
        forge(&s, cache, &merge, &honest);
        let beyond = "it changes \"evil.txt\" beyond the files its parents conflict in";
        let record = record_claude_resolution(&s, &merge);
        assert_eq!(
            record.status.code(),
            Some(1),
            "{cache:?}: record {record:?}"
        );
        assert!(
            String::from_utf8_lossy(&record.stdout).contains(beyond),
            "{cache:?}: record {record:?}"
        );
        hand_written_resolutions(&s, &merge);
        let walk = offline_merge(&s, &merge, &s.c.repo);
        assert_eq!(walk.status.code(), Some(1), "{cache:?}: walk {walk:?}");
        assert!(
            String::from_utf8_lossy(&walk.stdout).contains(beyond),
            "{cache:?}: walk {walk:?}"
        );

        let (s, clean, honest) = forged_history(&host, false, None);
        forge(&s, cache, &clean, &honest);
        let waiver = offline_merge(&s, &clean, &s.c.repo);
        assert_eq!(
            waiver.status.code(),
            Some(1),
            "{cache:?}: waiver {waiver:?}"
        );
        assert!(
            String::from_utf8_lossy(&waiver.stdout).contains("is not a merge from the base branch"),
            "{cache:?}: waiver {waiver:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{cache:?}: waiver");
    }
}

fn round_report(s: &Scratch, name: &str, head: &str, findings: &[&str], verdict: &str) -> String {
    let mut block = format!("head: {head}\n");
    for finding in findings {
        block.push_str(finding);
        block.push('\n');
    }
    let report = s.c.write(
        &format!("drafts/{name}.md"),
        &format!("# {name}\n\n```cm-review\n{block}```\n\n{verdict}\n"),
    );
    report.to_str().unwrap().to_owned()
}

#[test]
fn convergence_compares_a_resolution_round_only_with_resolution_rounds_of_its_merge() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    let accept = round_report(&s, "fresh-h1", &s.h1, &[], "ACCEPT");
    let recorded = offline_cm(
        &s,
        &[
            "review",
            "record",
            STEP,
            &accept,
            "--reviewer",
            "fresh-opus",
        ],
    );
    assert!(recorded.status.success(), "{recorded:?}");
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let repo = s.c.repo.to_str().unwrap();
    let one = "F1 | contract | introduced | shared.txt:1 | the resolution drops main's line";
    let two = "F2 | contract | introduced | shared.txt:1 | the resolution keeps a marker";
    let resolution = |name: &str, reviewer: &str, findings: &[&str]| {
        let report = round_report(&s, name, &merge, findings, "FIX REQUIRED");
        offline_cm(
            &s,
            &[
                "review",
                "record",
                STEP,
                &report,
                "--reviewer",
                reviewer,
                "--kind",
                "resolution",
                "--repo",
                repo,
            ],
        )
    };
    let fresh = resolution("fresh-resolution", "fresh-opus", &[one]);
    assert!(fresh.status.success(), "{fresh:?}");
    let out = String::from_utf8_lossy(&fresh.stdout);
    assert!(
        !out.contains("converging: false") && !out.contains("non-converging"),
        "{out}"
    );
    assert_eq!(s.c.state(), Phase::PrOpen);
    assert!(
        !fs::read_to_string(s.c.root.join("OWNER-QUEUE.md"))
            .unwrap_or_default()
            .contains("non-converging fresh reader")
    );

    let first = resolution("claude-resolution-1", "claude", &[one]);
    assert!(first.status.success(), "{first:?}");
    let second = resolution("claude-resolution-2", "claude", &[one, two]);
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("converging: false"),
        "{second:?}"
    );

    s.git(&[
        "commit",
        "-q",
        "--amend",
        "-m",
        "the same resolution, amended",
    ]);
    let amended = s.head();
    let three =
        "F3 | contract | introduced | shared.txt:1 | the resolution drops the feature's line";
    let report = round_report(
        &s,
        "claude-amended",
        &amended,
        &[one, two, three],
        "FIX REQUIRED",
    );
    let other = offline_cm(
        &s,
        &[
            "review",
            "record",
            STEP,
            &report,
            "--reviewer",
            "claude",
            "--kind",
            "resolution",
            "--repo",
            repo,
        ],
    );
    assert!(other.status.success(), "{other:?}");
    assert!(
        String::from_utf8_lossy(&other.stdout).contains("converging: true"),
        "{other:?}"
    );
}

#[test]
fn a_fresh_reader_is_due_only_when_a_round_rises_against_rounds_of_its_kind() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let repo = s.c.repo.to_str().unwrap();
    let one = "F1 | contract | introduced | shared.txt:1 | the resolution drops main's line";
    let two = "F2 | contract | introduced | shared.txt:1 | the resolution keeps a marker";
    for (name, findings, due) in [
        ("claude-first", &[one][..], false),
        ("claude-second", &[one, two][..], true),
    ] {
        let report = round_report(&s, name, &merge, findings, "FIX REQUIRED");
        let recorded = offline_cm(
            &s,
            &[
                "review",
                "record",
                STEP,
                &report,
                "--reviewer",
                "claude",
                "--kind",
                "resolution",
                "--repo",
                repo,
            ],
        );
        assert!(recorded.status.success(), "{name}: {recorded:?}");
        let next = offline_cm(&s, &["next"]);
        assert_eq!(
            String::from_utf8_lossy(&next.stdout).contains("fresh reader"),
            due,
            "{name}: {next:?}"
        );
    }
}

#[test]
fn the_fresh_reader_cm_next_asks_for_takes_the_round_that_clears_it() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::with(&host, Lane::Codex, true);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    let one = "F1 | contract | introduced | shared.txt:1 | the resolution drops main's line";
    let two = "F2 | contract | introduced | shared.txt:1 | the resolution keeps a marker";
    for (name, findings) in [
        ("claude-first", &[one][..]),
        ("claude-second", &[one, two][..]),
    ] {
        let report = round_report(&s, name, &merge, findings, "FIX REQUIRED");
        let recorded = offline_cm(
            &s,
            &[
                "review",
                "record",
                STEP,
                &report,
                "--reviewer",
                "claude",
                "--kind",
                "resolution",
            ],
        );
        assert!(recorded.status.success(), "{name}: {recorded:?}");
    }
    let next = offline_cm(&s, &["next"]);
    let out = String::from_utf8_lossy(&next.stdout).into_owned();
    let line = out
        .lines()
        .find(|line| line.contains("fresh reader"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        line.contains(&format!("on the conflict resolution of {merge}")),
        "{line}"
    );
    let command = line.rsplit_once(" — `").unwrap().1.trim_end_matches('`');
    let report = round_report(&s, "fresh-resolution", &merge, &[], "ACCEPT");
    let args: Vec<String> = command
        .split_whitespace()
        .skip(1)
        .map(|word| {
            if word == "'<fresh-review-report>'" {
                report.clone()
            } else {
                word.to_owned()
            }
        })
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let followed = offline_cm(&s, &args);
    assert!(followed.status.success(), "{command}: {followed:?}");
    let next = offline_cm(&s, &["next"]);
    assert!(
        !String::from_utf8_lossy(&next.stdout).contains("fresh reader"),
        "{next:?}"
    );
}

#[test]
fn a_fresh_resolution_round_is_measured_only_against_resolution_rounds_of_its_merge() {
    let host = at("2026-11-25T06:00:00Z");
    let one = "F1 | contract | introduced | shared.txt:1 | the resolution drops main's line";
    let two = "F2 | contract | introduced | shared.txt:1 | the resolution keeps a marker";
    let three =
        "F3 | contract | introduced | shared.txt:1 | the resolution drops the feature's line";
    let four = "F4 | contract | introduced | shared.txt:1 | the resolution reorders the lines";
    let record =
        |s: &Scratch, name: &str, head: &str, reviewer: &str, findings: &[&str], kind: bool| {
            let report = round_report(s, name, head, findings, "FIX REQUIRED");
            let repo = s.c.repo.to_str().unwrap().to_owned();
            let mut args = vec!["review", "record", STEP, &report, "--reviewer", reviewer];
            if kind {
                args.extend(["--kind", "resolution", "--repo", &repo]);
            }
            let recorded = offline_cm(s, &args);
            assert!(recorded.status.success(), "{name}: {recorded:?}");
            String::from_utf8_lossy(&recorded.stdout).into_owned()
        };

    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let main = s.git(&["rev-parse", &format!("origin/{}", s.base)]);
    record(&s, "claude-head-1", &main, "claude", &[one], false);
    record(&s, "claude-head-2", &main, "claude", &[one, two], false);
    let merge = s.merge_main(Some("feature and main\n"));
    let fresh = record(
        &s,
        "fresh-resolution",
        &merge,
        "fresh-opus",
        &[one, two, three],
        true,
    );
    assert!(!fresh.contains("converging: false"), "{fresh}");
    assert_eq!(s.c.state(), Phase::PrOpen);

    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    record(&s, "claude-resolution-1", &merge, "claude", &[one], true);
    record(
        &s,
        "claude-resolution-2",
        &merge,
        "claude",
        &[one, two],
        true,
    );
    record(
        &s,
        "opus-head",
        &merge,
        "opus",
        &[one, two, three, four],
        false,
    );
    let fresh = record(
        &s,
        "fresh-resolution",
        &merge,
        "fresh-opus",
        &[one, two, three],
        true,
    );
    assert!(fresh.contains("converging: false"), "{fresh}");
}

#[test]
fn a_git_dir_in_the_environment_never_moves_the_repository_the_gate_reads() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    let side = side_branch(&s, "shared.txt", "side\n");
    let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", "side"]);
    assert!(!merged.status.success(), "{merged:?}");
    fs::write(s.c.repo.join("shared.txt"), "feature and side\n").unwrap();
    s.git(&["add", "shared.txt"]);
    s.git(&["commit", "-q", "--no-edit"]);
    let merge = s.head();
    let mirror = s.home.join("mirror.git");
    let cloned = scratch_git(
        &s.home,
        &s.home,
        &[
            "clone",
            "-q",
            "--mirror",
            s.c.repo.to_str().unwrap(),
            mirror.to_str().unwrap(),
        ],
    );
    assert!(cloned.status.success(), "{cloned:?}");
    let moved = scratch_git(
        &s.home,
        &mirror,
        &["update-ref", "refs/remotes/origin/HEAD", &side],
    );
    assert!(moved.status.success(), "{moved:?}");
    let report = s.c.report("claude-redirected", &merge, &[], "ACCEPT");
    let record = offline_cm_env(
        &s,
        None,
        &[("GIT_DIR", mirror.to_str().unwrap())],
        &[
            "review",
            "record",
            STEP,
            report.to_str().unwrap(),
            "--reviewer",
            "claude",
            "--kind",
            "resolution",
            "--repo",
            s.c.repo.to_str().unwrap(),
        ],
    );
    assert_eq!(record.status.code(), Some(1), "{record:?}");
    assert!(
        String::from_utf8_lossy(&record.stdout)
            .contains("which is not on the base branch origin/HEAD"),
        "{record:?}"
    );

    let s = Scratch::new(&host);
    s.on_main("other.txt", "main\n");
    let clean = s.merge_main(None);
    let stale = s.home.join("stale.git");
    let cloned = scratch_git(
        &s.home,
        &s.home,
        &[
            "clone",
            "-q",
            "--mirror",
            s.c.repo.to_str().unwrap(),
            stale.to_str().unwrap(),
        ],
    );
    assert!(cloned.status.success(), "{cloned:?}");
    let moved = scratch_git(
        &s.home,
        &stale,
        &["update-ref", "refs/remotes/origin/HEAD", &s.h1],
    );
    assert!(moved.status.success(), "{moved:?}");
    let waived = offline_merge_env(
        &s,
        None,
        &[("GIT_DIR", stale.to_str().unwrap())],
        &clean,
        s.c.repo.as_os_str(),
    );
    assert!(waived.status.success(), "{waived:?}");
}

#[test]
fn a_resolution_review_reads_a_worktree_its_git_directory_or_a_bare_repository() {
    let host = at("2026-11-25T06:00:00Z");
    for shape in ["worktree", "git directory", "bare"] {
        let place = |s: &Scratch| -> PathBuf {
            match shape {
                "worktree" => s.c.repo.clone(),
                "git directory" => s.c.repo.join(".git"),
                _ => {
                    let bare = s.home.join("bare.git");
                    let cloned = scratch_git(
                        &s.home,
                        &s.home,
                        &[
                            "clone",
                            "-q",
                            "--mirror",
                            s.c.repo.to_str().unwrap(),
                            bare.to_str().unwrap(),
                        ],
                    );
                    assert!(cloned.status.success(), "{cloned:?}");
                    bare
                }
            }
        };
        let s = Scratch::new(&host);
        s.on_main("shared.txt", "main\n");
        let merge = s.merge_main(Some("feature and main\n"));
        let repo = place(&s);
        for reviewer in ["claude", "astra"] {
            let recorded = record_in(&s, None, repo.as_os_str(), reviewer, &merge);
            assert!(
                recorded.status.success(),
                "{shape}: {reviewer} {recorded:?}"
            );
        }
        let merged = offline_merge(&s, &merge, &repo);
        assert!(merged.status.success(), "{shape}: merged {merged:?}");

        let s = Scratch::new(&host);
        s.on_main("other.txt", "main\n");
        let clean = s.merge_main(None);
        let repo = place(&s);
        let waived = offline_merge(&s, &clean, &repo);
        assert!(waived.status.success(), "{shape}: waiver {waived:?}");
    }
}

#[test]
fn a_named_head_at_a_resolving_step_meets_the_head_reviews_of_the_commit_it_names() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    s.astra(&host, &merge, "head", "ACCEPT");
    claude_accept(&host, &s.c, &merge);
    let merged = offline_merge(&s, "HEAD", &s.c.repo);
    assert!(merged.status.success(), "{merged:?}");
}

#[test]
fn no_git_test_variable_brings_back_a_cache_the_gate_turns_off() {
    let host = at("2026-11-25T06:00:00Z");
    let env = [
        ("GIT_TEST_COMMIT_GRAPH", "1"),
        ("GIT_TEST_MULTI_PACK_INDEX", "1"),
        ("GIT_TEST_SPLIT_INDEX", "1"),
    ];
    for cache in [Cache::CommitGraph, Cache::MultiPackIndex] {
        let (s, merge, honest) = forged_history(&host, false, Some("feature and main\n"));
        forge(&s, cache, &merge, &honest);
        let record = record_claude_resolution_env(&s, &env, &merge);
        assert_eq!(
            record.status.code(),
            Some(1),
            "{cache:?}: record {record:?}"
        );
        assert!(
            String::from_utf8_lossy(&record.stdout).contains("does not clear the gate"),
            "{cache:?}: record {record:?}"
        );
        hand_written_resolutions(&s, &merge);
        let walk = offline_merge_env(&s, None, &env, &merge, s.c.repo.as_os_str());
        assert_eq!(walk.status.code(), Some(1), "{cache:?}: walk {walk:?}");
        assert_eq!(s.c.state(), Phase::PrOpen, "{cache:?}: walk");

        let (s, clean, honest) = forged_history(&host, false, None);
        forge(&s, cache, &clean, &honest);
        let waiver = offline_merge_env(&s, None, &env, &clean, s.c.repo.as_os_str());
        assert_eq!(
            waiver.status.code(),
            Some(1),
            "{cache:?}: waiver {waiver:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{cache:?}: waiver");
    }
}

#[test]
fn the_scratch_repository_finds_a_writable_absolute_home() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    fs::create_dir_all(s.home.join("relative-tmp")).unwrap();
    let relative = [("TMPDIR", "relative-tmp")];
    for reviewer in ["claude", "astra"] {
        let recorded = record_env(
            &s,
            Some(&s.home),
            &relative,
            s.c.repo.as_os_str(),
            reviewer,
            &merge,
        );
        assert!(
            recorded.status.success(),
            "relative TMPDIR: {reviewer} {recorded:?}"
        );
    }
    let merged = offline_merge_env(&s, Some(&s.home), &relative, &merge, s.c.repo.as_os_str());
    assert!(merged.status.success(), "relative TMPDIR: {merged:?}");

    let s = Scratch::new(&host);
    s.on_main("other.txt", "main\n");
    let clean = s.merge_main(None);
    let missing = s.home.join("missing");
    let missing = [("TMPDIR", missing.to_str().unwrap())];
    let waived = offline_merge_env(&s, None, &missing, &clean, s.c.repo.as_os_str());
    assert!(waived.status.success(), "missing TMPDIR: {waived:?}");
}

fn submodule_history(host: &Fake) -> (Scratch, [String; 3]) {
    let s = Scratch::new(host);
    let sub = s.home.join("subrepo");
    let made = scratch_git(&s.home, &s.home, &["init", "-q", sub.to_str().unwrap()]);
    assert!(made.status.success(), "{made:?}");
    let commits: Vec<String> = ["A", "B", "C"]
        .iter()
        .map(|name| {
            fs::write(sub.join("s.txt"), name).unwrap();
            scratch_git(&s.home, &sub, &["add", "s.txt"]);
            scratch_git(&s.home, &sub, &["commit", "-q", "-m", name]);
            let head = scratch_git(&s.home, &sub, &["rev-parse", "HEAD"]);
            String::from_utf8(head.stdout).unwrap().trim().to_owned()
        })
        .collect();
    let at = |s: &Scratch, commit: &str| {
        let moved = scratch_git(&s.home, &s.c.repo.join("sub"), &["checkout", "-q", commit]);
        assert!(moved.status.success(), "{moved:?}");
    };
    s.on_main_with(|s| {
        let added = scratch_git(
            &s.home,
            &s.c.repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "sub",
            ],
        );
        assert!(added.status.success(), "{added:?}");
        at(s, &commits[0]);
    });
    s.merge_main(None);
    at(&s, &commits[1]);
    s.git(&["add", "sub"]);
    s.git(&["commit", "-q", "-m", "sub to B"]);
    s.reviewed(host, "h2");
    s.on_main_with(|s| at(s, &commits[2]));
    at(&s, &commits[1]);
    (
        s,
        [commits[0].clone(), commits[1].clone(), commits[2].clone()],
    )
}

#[test]
fn a_submodule_moved_on_both_sides_is_read_without_its_objects() {
    let host = at("2026-11-25T06:00:00Z");
    let (s, _) = submodule_history(&host);
    let clean = s.merge_main(None);
    let shown = s.git(&["show", "--remerge-diff", "--format=", &clean]);
    assert!(
        shown.is_empty(),
        "the checkout fast-forwards the submodule: {shown}"
    );
    let refused = offline_merge(&s, &clean, &s.c.repo);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains(&format!(
            "{} resolved merge conflicts; it needs its own review",
            &clean[..12]
        )),
        "{refused:?}"
    );

    let (s, [first, _, _]) = submodule_history(&host);
    let main = format!("origin/{}", s.base);
    scratch_git(
        &s.home,
        &s.c.repo,
        &["merge", "-q", "--no-commit", "--no-ff", &main],
    );
    s.git(&[
        "update-index",
        "--cacheinfo",
        &format!("160000,{first},sub"),
    ]);
    s.git(&["commit", "-q", "--no-edit"]);
    let merge = s.head();
    let record = record_claude_resolution(&s, &merge);
    assert_eq!(record.status.code(), Some(1), "{record:?}");
    assert!(
        String::from_utf8_lossy(&record.stdout)
            .contains("its parents conflict in the submodule \"sub\""),
        "{record:?}"
    );
}

#[derive(Clone, Copy, Debug)]
enum Shadow {
    Tag,
    Branch,
}

fn shadow(s: &Scratch, shadow: Shadow, name: &str, at: &str) {
    match shadow {
        Shadow::Tag => s.git(&["tag", name, at]),
        Shadow::Branch => s.git(&["branch", name, at]),
    };
}

fn side_branch(s: &Scratch, file: &str, text: &str) -> String {
    s.git(&["checkout", "-q", "-b", "side", &s.base]);
    let side = s.commit(file, text);
    s.git(&["checkout", "-q", "feature"]);
    side
}

#[test]
fn no_local_ref_named_origin_head_stands_in_for_the_base() {
    let host = at("2026-11-25T06:00:00Z");
    let off_base = "which is not on the base branch origin/HEAD";
    for kind in [Shadow::Tag, Shadow::Branch] {
        let s = Scratch::new(&host);
        let side = side_branch(&s, "shared.txt", "side\n");
        let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", "side"]);
        assert!(!merged.status.success(), "{kind:?}: {merged:?}");
        fs::write(s.c.repo.join("shared.txt"), "feature and side\n").unwrap();
        s.git(&["add", "shared.txt"]);
        s.git(&["commit", "-q", "--no-edit"]);
        let merge = s.head();
        shadow(&s, kind, "origin/HEAD", &side);
        assert_eq!(s.git(&["rev-parse", "origin/HEAD"]), side, "{kind:?}");
        let report = s.c.report("claude-shadowed", &merge, &[], "ACCEPT");
        let record = offline_cm(
            &s,
            &[
                "review",
                "record",
                STEP,
                report.to_str().unwrap(),
                "--reviewer",
                "claude",
                "--kind",
                "resolution",
                "--repo",
                s.c.repo.to_str().unwrap(),
            ],
        );
        assert_eq!(record.status.code(), Some(1), "{kind:?}: record {record:?}");
        assert!(
            String::from_utf8_lossy(&record.stdout).contains(off_base),
            "{kind:?}: record {record:?}"
        );
        s.edit_ledger(|ledger| as_resolutions(ledger, &s.h1, &merge));
        let walk = offline_merge(&s, &merge, &s.c.repo);
        assert_eq!(walk.status.code(), Some(1), "{kind:?}: walk {walk:?}");
        assert!(
            String::from_utf8_lossy(&walk.stdout).contains(off_base),
            "{kind:?}: walk {walk:?}"
        );

        let s = Scratch::new(&host);
        let side = side_branch(&s, "other.txt", "side\n");
        let merged = scratch_git(&s.home, &s.c.repo, &["merge", "-q", "--no-edit", "side"]);
        assert!(merged.status.success(), "{kind:?}: {merged:?}");
        let clean = s.head();
        shadow(&s, kind, "origin/HEAD", &side);
        let waiver = offline_merge(&s, &clean, &s.c.repo);
        assert_eq!(waiver.status.code(), Some(1), "{kind:?}: waiver {waiver:?}");
        assert!(
            String::from_utf8_lossy(&waiver.stdout).contains(off_base),
            "{kind:?}: waiver {waiver:?}"
        );
        assert_eq!(s.c.state(), Phase::PrOpen, "{kind:?}: waiver");

        let s = Scratch::new(&host);
        let side = side_branch(&s, "side.txt", "side\n");
        s.on_main("other.txt", "main\n");
        let clean = s.merge_main(None);
        shadow(&s, kind, "origin/HEAD", &side);
        let merged = offline_merge(&s, &clean, &s.c.repo);
        assert!(merged.status.success(), "{kind:?}: off the base {merged:?}");
    }
}

#[test]
fn a_head_named_by_a_ref_means_one_commit_or_takes_no_walk() {
    let host = at("2026-11-25T06:00:00Z");
    let covered = |host: &Fake| {
        let s = Scratch::new(host);
        s.on_main("shared.txt", "main\n");
        let merge = s.merge_main(Some("feature and main\n"));
        s.accepted_resolution(host, &merge, "resolution");
        s.git(&["tag", "covered", &merge]);
        (s, merge)
    };
    for head in ["HEAD", "feature", "covered"] {
        let (s, _) = covered(&host);
        let merged = offline_merge(&s, head, &s.c.repo);
        assert!(merged.status.success(), "{head}: {merged:?}");
        assert_eq!(s.c.state(), Phase::Merged, "{head}");
    }
    for kind in [Shadow::Tag, Shadow::Branch] {
        let (s, merge) = covered(&host);
        let name = match kind {
            Shadow::Tag => "feature",
            Shadow::Branch => "covered",
        };
        shadow(&s, kind, name, &s.h1);
        let shadowed = offline_merge(&s, name, &s.c.repo);
        assert_eq!(shadowed.status.code(), Some(1), "{kind:?}: {shadowed:?}");
        assert_eq!(s.c.state(), Phase::PrOpen, "{kind:?}");
        assert_eq!(s.head(), merge, "{kind:?}");
    }

    let clean_update = |host: &Fake| {
        let s = Scratch::new(host);
        s.on_main("other.txt", "main\n");
        let clean = s.merge_main(None);
        let tree = format!("{clean}^{{tree}}");
        let main = s.git(&["rev-parse", &format!("origin/{}", s.base)]);
        let twin = s.git(&["commit-tree", &tree, "-p", &s.h1, "-p", &main, "-m", "twin"]);
        (s, clean, twin)
    };
    let (s, clean, twin) = clean_update(&host);
    s.git(&["tag", "feature", &twin]);
    let shadowed = offline_merge(&s, "feature", &s.c.repo);
    assert_eq!(shadowed.status.code(), Some(1), "{shadowed:?}");
    s.git(&["tag", "-d", "feature"]);
    let merged = offline_merge(&s, "feature", &s.c.repo);
    assert!(merged.status.success(), "{merged:?}");
    assert_eq!(s.head(), clean);

    let (s, _, twin) = clean_update(&host);
    s.git(&["tag", "origin", &twin]);
    let shadowed = offline_merge(&s, "origin", &s.c.repo);
    assert_eq!(shadowed.status.code(), Some(1), "{shadowed:?}");
    let merged = offline_merge(&s, &twin, &s.c.repo);
    assert!(merged.status.success(), "{merged:?}");

    let (s, clean, twin) = clean_update(&host);
    let short = &clean[..12];
    s.git(&["tag", short, &twin]);
    let shadowed = offline_merge(&s, short, &s.c.repo);
    assert_eq!(shadowed.status.code(), Some(1), "{shadowed:?}");
    s.git(&["tag", "-d", short]);
    let merged = offline_merge(&s, short, &s.c.repo);
    assert!(merged.status.success(), "{merged:?}");
}

#[derive(Clone, Copy, Debug)]
enum Reach {
    Argument,
    Link,
    Cwd,
}

fn not_utf_8(s: &mut Scratch, reach: Reach) -> (Option<PathBuf>, std::ffi::OsString) {
    use std::os::unix::ffi::OsStrExt;
    let temp = s.c.repo.parent().unwrap().to_path_buf();
    let dir = temp.join(std::ffi::OsStr::from_bytes(b"d\xff\"\nr"));
    fs::create_dir_all(&dir).unwrap();
    let moved = dir.join("repo");
    fs::rename(&s.c.repo, &moved).unwrap();
    s.c.repo = moved.clone();
    match reach {
        Reach::Argument => (None, moved.into_os_string()),
        Reach::Link => {
            let link = temp.join("link");
            std::os::unix::fs::symlink(&moved, &link).unwrap();
            (None, link.into_os_string())
        }
        Reach::Cwd => (Some(dir), "repo".into()),
    }
}

fn record_in(
    s: &Scratch,
    cwd: Option<&Path>,
    repo: &std::ffi::OsStr,
    reviewer: &str,
    merge: &str,
) -> std::process::Output {
    record_env(s, cwd, &[], repo, reviewer, merge)
}

fn record_env(
    s: &Scratch,
    cwd: Option<&Path>,
    env: &[(&str, &str)],
    repo: &std::ffi::OsStr,
    reviewer: &str,
    merge: &str,
) -> std::process::Output {
    use std::ffi::OsStr;
    let text = format!(
        "# Resolution review by {reviewer}\n\nThe remerge diff only.\n\n```cm-review\nhead: {merge}\n```\n\nACCEPT\n"
    );
    let report = if reviewer == "astra" {
        let dir = "astra-review-7-bytes";
        let report =
            s.c.write(&format!("{STEP}/evidence/{dir}/report.md"), &text);
        s.c.receipt(dir, merge, "gpt-6-astra", "xhigh");
        report
    } else {
        s.c.write(&format!("drafts/{reviewer}-bytes.md"), &text)
    };
    offline_cm_env(
        s,
        cwd,
        env,
        &[
            OsStr::new("review"),
            OsStr::new("record"),
            OsStr::new(STEP),
            report.as_os_str(),
            OsStr::new("--reviewer"),
            OsStr::new(reviewer),
            OsStr::new("--kind"),
            OsStr::new("resolution"),
            OsStr::new("--repo"),
            repo,
        ],
    )
}

#[test]
fn a_checkout_whose_path_is_not_utf_8_is_read_like_any_other() {
    let host = at("2026-11-25T06:00:00Z");
    for reach in [Reach::Argument, Reach::Link, Reach::Cwd] {
        let mut s = Scratch::new(&host);
        s.on_main("shared.txt", "main\n");
        let merge = s.merge_main(Some("feature and main\n"));
        let (cwd, repo) = not_utf_8(&mut s, reach);
        for reviewer in ["claude", "astra"] {
            let recorded = record_in(&s, cwd.as_deref(), &repo, reviewer, &merge);
            assert!(
                recorded.status.success(),
                "{reach:?}: {reviewer} {recorded:?}"
            );
        }
        let merged = offline_merge_in(&s, cwd.as_deref(), &merge, &repo);
        assert!(merged.status.success(), "{reach:?}: merged {merged:?}");
        assert_eq!(s.c.state(), Phase::Merged, "{reach:?}: merged");

        let mut s = Scratch::new(&host);
        s.on_main("shared.txt", "main\n");
        let merge = s.merge_main(Some("feature and main\n"));
        s.accepted_resolution(&host, &merge, "earlier");
        let (cwd, repo) = not_utf_8(&mut s, reach);
        let walked = offline_merge_in(&s, cwd.as_deref(), &merge[..12], &repo);
        assert!(walked.status.success(), "{reach:?}: walk {walked:?}");
        assert_eq!(s.c.state(), Phase::Merged, "{reach:?}: walk");

        let mut s = Scratch::new(&host);
        s.on_main("other.txt", "main\n");
        let clean = s.merge_main(None);
        let (cwd, repo) = not_utf_8(&mut s, reach);
        let waived = offline_merge_in(&s, cwd.as_deref(), &clean, &repo);
        assert!(waived.status.success(), "{reach:?}: waiver {waived:?}");
        assert_eq!(s.c.state(), Phase::Merged, "{reach:?}: waiver");

        let mut s = Scratch::new(&host);
        s.on_main("shared.txt", "main\n");
        s.merge_main(Some("feature and main\n"));
        fs::write(s.c.repo.join("evil.txt"), "beyond\n").unwrap();
        s.git(&["add", "evil.txt"]);
        s.git(&["commit", "-q", "--amend", "--no-edit"]);
        let merge = s.head();
        let (cwd, repo) = not_utf_8(&mut s, reach);
        let refused = record_in(&s, cwd.as_deref(), &repo, "claude", &merge);
        let beyond = "it changes \"evil.txt\" beyond the files its parents conflict in";
        assert_eq!(refused.status.code(), Some(1), "{reach:?}: {refused:?}");
        assert!(
            String::from_utf8_lossy(&refused.stdout).contains(beyond),
            "{reach:?}: {refused:?}"
        );
        s.edit_ledger(|ledger| as_resolutions(ledger, &s.h1, &merge));
        let walk = offline_merge_in(&s, cwd.as_deref(), &merge, &repo);
        assert_eq!(walk.status.code(), Some(1), "{reach:?}: {walk:?}");
        assert!(
            String::from_utf8_lossy(&walk.stdout).contains(beyond),
            "{reach:?}: {walk:?}"
        );
    }
}

#[test]
fn a_step_without_a_resolution_record_takes_the_requested_head_as_given() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("other.txt", "main\n");
    s.merge_main(None);
    let merged = offline_merge(&s, "HEAD", &s.c.repo);
    assert!(merged.status.success(), "{merged:?}");

    let s = Scratch::new(&host);
    let fix = s.commit("fix.txt", "unreviewed\n");
    let short = &fix[..6];
    let refused = offline_merge(&s, short, &s.c.repo);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let gates: Vec<String> = String::from_utf8_lossy(&refused.stdout)
        .lines()
        .filter(|line| line.contains("merge-gate"))
        .map(str::to_owned)
        .collect();
    assert_eq!(gates.len(), 3, "{gates:?}");
    assert!(gates[0].contains(&format!("no Astra receipt at {short} with effective")));
    assert!(gates[1].contains(&format!("no review of {short} is recorded")));
    assert!(gates[2].contains(&format!(
        "no reviewed head before {short} is joined to it by clean updates from the base branch: {} is not a merge from the base branch",
        &fix[..12]
    )));
}

#[test]
fn case_never_decides_which_head_or_record_binds() {
    let host = at("2026-11-25T06:00:00Z");
    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    s.accepted_resolution(&host, &merge, "resolution");
    s.edit_ledger(|ledger| {
        for review in &mut ledger.reviews[2..] {
            review.head = review.head.to_uppercase();
        }
    });
    assert!(s.cleared().gaps.is_empty(), "{:?}", s.cleared().gaps);
    let merged = offline_merge(&s, &merge[..12].to_uppercase(), &s.c.repo);
    assert!(merged.status.success(), "{merged:?}");
}

#[test]
fn each_resolution_binding_site_reads_only_full_merge_shas() {
    let host = at("2026-11-25T06:00:00Z");
    let covered = |s: &Scratch| -> String {
        s.on_main("shared.txt", "main\n");
        let merge = s.merge_main(Some("feature and main\n"));
        s.accepted_resolution(&host, &merge, "resolution");
        merge
    };
    let shorten = |s: &Scratch, reviewer: &str, merge: &str| {
        s.edit_ledger(|ledger| {
            for review in &mut ledger.reviews[2..] {
                if review.reviewer.as_deref() == Some(reviewer) {
                    review.head = merge[..8].to_owned();
                }
            }
        });
    };

    let s = Scratch::new(&host);
    let merge = covered(&s);
    shorten(&s, "claude", &merge);
    assert_eq!(walk_gaps(&s.cleared()), ["no claude resolution ACCEPT"]);

    let s = Scratch::new(&host);
    let merge = covered(&s);
    shorten(&s, "astra", &merge);
    let unexcluded = s.cleared();
    assert_eq!(unexcluded.stop, None);
    assert!(
        unexcluded.gaps.iter().any(|gap| gap.contains(&format!(
            "astra-review-7-resolution-astra/receipt.json records the Astra review at {merge}, but it is not recorded"
        ))),
        "{:?}",
        unexcluded.gaps
    );

    let s = Scratch::new(&host);
    s.on_main("shared.txt", "main\n");
    let merge = s.merge_main(Some("feature and main\n"));
    for reviewer in ["astra", "claude"] {
        let report = s.c.write(
            &format!("drafts/{reviewer}-unreceipted.md"),
            &format!("# {reviewer}\n\nNo receipt.\n\n```cm-review\nhead: {merge}\n```\n\nACCEPT\n"),
        );
        assert!(
            verdict::record_resolution(
                &host,
                &s.c.root,
                &target(),
                &report,
                Some(reviewer),
                None,
                Some(&s.c.repo),
            )
            .unwrap()
            .ok()
        );
    }
    shorten(&s, "astra", &merge);
    shorten(&s, "claude", &merge);
    assert_eq!(
        s.cleared().stop,
        Some(CleanUpdateError::Conflicts { commit: merge })
    );
}
