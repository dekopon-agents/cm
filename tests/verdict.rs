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
use cm::sync::sync;
use cm::verdict;
use jiff::Timestamp;

#[allow(dead_code)]
mod support;

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
struct FakeForge {
    head: String,
    state: PullState,
    mergeable: &'static str,
    checks: Vec<(&'static str, &'static str)>,
    down: bool,
}

impl FakeForge {
    fn open(head: &str, checks: &[(&'static str, &'static str)]) -> Self {
        FakeForge {
            head: head.to_owned(),
            state: PullState::Open,
            mergeable: "clean",
            checks: checks.to_vec(),
            down: false,
        }
    }
}

impl Forge for FakeForge {
    fn pull(&self, _: &str) -> Result<Pull, ForgeError> {
        if self.down {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(Pull {
            state: self.state,
            merge_commit: (self.state == PullState::Merged)
                .then(|| CommitSha::new(self.head.clone())),
            head: Some(CommitSha::new(self.head.clone())),
            mergeable_state: Some(self.mergeable.to_owned()),
        })
    }

    fn checks(&self, _: &GithubRepo, sha: &CommitSha) -> Result<Vec<CheckState>, ForgeError> {
        if self.down {
            return Err(ForgeError::Exit(Some(1)));
        }
        assert_eq!(sha.as_str(), self.head, "CI is read at the PR head");
        Ok(self
            .checks
            .iter()
            .map(|(name, state)| CheckState {
                name: (*name).to_owned(),
                state: (*state).to_owned(),
            })
            .collect())
    }

    fn tag_exists(&self, _: &GithubRepo, _: &TagName) -> Result<bool, ForgeError> {
        Ok(false)
    }

    fn tag_contains(&self, _: &GithubRepo, _: &TagName, _: &CommitSha) -> Result<bool, ForgeError> {
        Ok(false)
    }
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
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    assert!(commands::init(host, &root).unwrap().ok());
    git(&root, &["init", "-q"]);
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "head"]);
    let declaration = Declaration {
        lane: Some(Lane::Codex),
        ..Declaration::default()
    };
    assert!(
        commands::declare(host, &root.join(STEP), &declaration)
            .unwrap()
            .ok()
    );
    fs::write(root.join(STEP).join("kickoff.md"), "Build the thing.\n").unwrap();
    let remote = Remote {
        coordinator: Some("session_a".into()),
        cap_min: None,
    };
    let launched = commands::launch_remote(host, &root, &target(), None, &remote).unwrap();
    assert!(launched.ok(), "{:?}", launched.violations);
    let c = Campaign {
        _temp: temp,
        root,
        repo,
    };
    if pr {
        let opened = commands::advance(
            host,
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
        self.write(
            &format!("drafts/{name}.md"),
            &format!("# Review\n\nProse.\n\n```cm-review\n{block}```\n\n{verdict}\n"),
        )
    }

    fn receipt(&self, dir: &str, head: &str, model: &str, effort: &str) {
        self.receipt_with(dir, head, model, effort, |_| {});
    }

    /// An `astra-review.sh collect` receipt for a review that happened, changed by `edit`.
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
            &Phase::Merged,
            &Evidence {
                sha: Some(self.head()),
                ..Evidence::default()
            },
            None,
            Some(&self.repo),
            gate,
        )
        .unwrap()
    }
}

fn target() -> store::Target {
    parse_target(STEP, true).unwrap()
}

fn record(host: &Fake, c: &Campaign, report: &Path, reviewer: &str) -> Outcome {
    verdict::record(host, &c.root, &target(), report, Some(reviewer)).unwrap()
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
    let missing =
        verdict::record(&host, &c.root, &target(), &c.root.join("nope.md"), None).unwrap();
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
        "r1",
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
    c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
    let accept = c.report(
        "r1",
        &head,
        &["F1 | taste | introduced | - | a nit"],
        "ACCEPT",
    );
    assert!(record(&host, &c, &accept, "astra").ok());
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
fn the_third_fix_required_round_blocks_the_step_and_asks_the_owner() {
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let blocking = ["F1 | contract | introduced | src/lib.rs:1 | broken"];
    for (round, head) in ["aaaaaaa1", "bbbbbbb2"].iter().enumerate() {
        let report = c.report(&format!("r{round}"), head, &blocking, "FIX REQUIRED");
        let recorded = record(&host, &c, &report, "astra");
        assert_eq!(recorded.fields["rounds"], round + 1);
        assert_eq!(recorded.fields["blocked"], false);
    }
    let same_head = c.report(
        "claude",
        "bbbbbbb2",
        &["F1 | contract | exposed | src/lib.rs:2 | also broken"],
        "FIX REQUIRED",
    );
    assert_eq!(record(&host, &c, &same_head, "claude").fields["rounds"], 2);
    assert_eq!(c.state(), Phase::PrOpen);
    let third = c.report("r3", "ccccccc3", &blocking, "FIX REQUIRED");
    let recorded = record(&host, &c, &third, "astra");
    assert!(recorded.ok(), "{:?}", recorded.violations);
    assert_eq!(recorded.fields["rounds"], 3);
    assert_eq!(recorded.fields["blocked"], true);
    assert_eq!(recorded.fields["owner_queue"], 1);
    assert_eq!(c.state(), Phase::Blocked("third-fix".into()));
    let queue = fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap();
    assert!(
        queue.contains("## 1. 01-a/u/S1: third FIX REQUIRED"),
        "{queue}"
    );
    assert!(
        queue.contains(
            "`01-a/u/S1/evidence/reviews/R3-claude.md` and `01-a/u/S1/evidence/reviews/R4-astra.md`"
        ),
        "{queue}"
    );
    assert!(
        fs::read_to_string(c.root.join("RESUME.md"))
            .unwrap()
            .contains("OWNER-QUEUE not drained")
    );
    assert!(
        lint(&c.root).unwrap().ok(),
        "{:?}",
        lint(&c.root).unwrap().violations
    );
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
            .any(|line| line.starts_with("stalled 01-a/u/S1: codex launch with no PR"))
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
    let host = at("2026-10-25T06:00:00Z");
    let c = campaign(&host, true);
    let head = c.head();
    let merged = FakeForge {
        state: PullState::Merged,
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
    c.receipt("astra-review-7", &head, "gpt-6-astra", "xhigh");
    let accept = c.report("r1", &head, &[], "ACCEPT");
    assert!(record(&host, &c, &accept, "astra").ok());
    let moved = sync(&host, &merged, &c.root).unwrap();
    assert!(moved.ok(), "{:?}", moved.violations);
    assert_eq!(c.state(), Phase::Merged);
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
    let refused = c.merge(&host, &gate);
    assert_eq!(
        rules(&refused),
        vec!["merge-gate"],
        "{:?}",
        refused.violations
    );
    let message = &refused.violations[0].message;
    for why in [
        "turn not completed",
        "thread forked or has a parent",
        "thread holds other than one turn",
    ] {
        assert!(message.contains(why), "{why}: {message}");
    }

    // Astra's FIX REQUIRED receipt with only Claude's ACCEPT recorded does not merge.
    c.receipt_with("astra-review-7", &head, "gpt-6-astra", "xhigh", |receipt| {
        receipt["verdict"] = "FIX REQUIRED".into();
    });
    let unrecorded = c.merge(&host, &gate);
    assert_eq!(rules(&unrecorded), vec!["merge-gate"]);
    assert!(
        unrecorded.violations[0].message.contains(
            "it is not recorded; `cm review record 01-a/u/S1 01-a/u/S1/evidence/astra-review-7/report.md --reviewer astra`"
        ),
        "{:?}",
        unrecorded.violations
    );
    let astra = c.report(
        "astra",
        &head,
        &["F1 | contract | introduced | src/lib.rs:1 | broken"],
        "FIX REQUIRED",
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
