use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm::commands;
use cm::commands::{Declaration, Ships};
use cm::epoch::{self, Status};
use cm::forge::{CommitSha, Forge, ForgeError, Gh, GithubRepo, Pull, PullState, TagName};
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::plan;
use cm::state::{Evidence, Lane, Phase, Work};
use cm::store::{self, parse_target};
use cm::sync::sync;
use jiff::Timestamp;

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

struct Campaign {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

fn campaign(host: &Fake) -> Campaign {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    assert!(commands::init(host, &root).unwrap().ok());
    Campaign { _temp: temp, root }
}

fn declare_work(
    host: &Fake,
    c: &Campaign,
    target: &str,
    lane: Lane,
    after: &[&str],
    work: Work,
) -> Outcome {
    let declaration = Declaration {
        lane: Some(lane),
        after: after.iter().map(|text| (*text).to_owned()).collect(),
        ships: Ships::default(),
        work,
    };
    commands::declare(host, &c.root.join(target), &declaration).unwrap()
}

fn declare(host: &Fake, c: &Campaign, target: &str, lane: Lane, after: &[&str]) -> Outcome {
    declare_work(host, c, target, lane, after, Work::Code)
}

fn declare_release(host: &Fake, c: &Campaign, target: &str, github: &str, tag: &str) -> Outcome {
    let declaration = Declaration {
        lane: Some(Lane::Pi),
        ships: Ships {
            github: Some(github.parse().unwrap()),
            tag: Some(tag.to_owned()),
        },
        ..Declaration::default()
    };
    commands::declare(host, &c.root.join(target), &declaration).unwrap()
}

fn kickoff(c: &Campaign, target: &str) {
    fs::write(c.root.join(target).join("kickoff.md"), "Go.\n").unwrap();
}

fn ready(c: &Campaign, lane: Option<Lane>) -> Vec<String> {
    let outcome = plan::next(&c.root, None, lane).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    outcome.fields["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["target"].as_str().unwrap().to_owned())
        .collect()
}

fn next_one(c: &Campaign, target: &str) -> Outcome {
    plan::next(&c.root, Some(&parse_target(target, true).unwrap()), None).unwrap()
}

fn advance(host: &Fake, c: &Campaign, target: &str, to: &str, evidence: Evidence) -> Outcome {
    commands::advance(
        host,
        &c.root,
        &parse_target(target, false).unwrap(),
        &to.parse().unwrap(),
        &evidence,
        None,
    )
    .unwrap()
}

fn state(c: &Campaign, target: &str) -> Phase {
    let (unit, step) = target.rsplit_once('/').unwrap();
    store::load_unit(&c.root.join(unit)).unwrap().steps[step]
        .track
        .state
        .clone()
}

fn open_pr(host: &Fake, c: &Campaign, target: &str, pr: &str) {
    let worktree = c.root.parent().unwrap().join("wt");
    if !worktree.exists() {
        fs::create_dir_all(&worktree).unwrap();
        git(&worktree, &["init", "-q"]);
    }
    let launched = commands::launch(
        host,
        &c.root,
        &parse_target(target, true).unwrap(),
        Some(&worktree),
    )
    .unwrap();
    assert!(launched.ok(), "{:?}", launched.violations);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", target]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    let evidence = Evidence {
        pr: Some(pr.into()),
        sha: Some(head),
        ..Evidence::default()
    };
    let moved = advance(host, c, target, "pr-open", evidence);
    assert!(moved.ok(), "{:?}", moved.violations);
}

#[test]
fn next_hands_out_only_steps_whose_after_is_met() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Pi, &[]).ok());
    let refused = declare(
        &host,
        &c,
        "01-a/u/S3",
        Lane::Codex,
        &["S2", "tag:dekopon@1.0.0"],
    );
    assert_eq!(rules(&refused), vec!["after-unknown", "after-unknown"]);
    assert!(declare(&host, &c, "01-a/u/S2", Lane::Codex, &["S1"]).ok());
    assert!(declare(&host, &c, "01-a/u/S2b", Lane::Pi, &["pr-open:S1"]).ok());
    let bad = declare(&host, &c, "01-a/u/S4", Lane::Pi, &["S1 S2"]);
    assert_eq!(rules(&bad), vec!["after-parse"]);
    for step in ["S1", "S2", "S2b"] {
        kickoff(&c, &format!("01-a/u/{step}"));
    }
    assert_eq!(ready(&c, None), vec!["01-a/u/S1"]);

    let out_of_order = next_one(&c, "01-a/u/S2");
    assert_eq!(rules(&out_of_order), vec!["after-unmet"]);
    assert!(
        out_of_order.violations[0]
            .message
            .contains("01-a/u/S1 (needs merged, is drafted)"),
        "{:?}",
        out_of_order.violations
    );
    let wt = c.root.parent().unwrap().join("wt");
    fs::create_dir_all(&wt).unwrap();
    git(&wt, &["init", "-q"]);
    let launch = commands::launch(
        &host,
        &c.root,
        &parse_target("01-a/u/S2", true).unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(rules(&launch), vec!["after-unmet"]);
    assert_eq!(state(&c, "01-a/u/S2"), Phase::Drafted);

    assert!(next_one(&c, "01-a/u/S1").ok());
    open_pr(&host, &c, "01-a/u/S1", "https://github.com/o/r/pull/1");
    assert_eq!(ready(&c, None), vec!["01-a/u/S2b"]);
    assert_eq!(ready(&c, Some(Lane::Codex)), Vec::<String>::new());
    assert_eq!(rules(&next_one(&c, "01-a/u/S1")), vec!["next-state"]);

    let sha = git(&wt, &["rev-parse", "HEAD"]);
    let merged = advance(
        &host,
        &c,
        "01-a/u/S1",
        "merged",
        Evidence {
            sha: Some(sha),
            ..Evidence::default()
        },
    );
    assert!(merged.ok(), "{:?}", merged.violations);
    assert_eq!(ready(&c, Some(Lane::Codex)), vec!["01-a/u/S2"]);
    let codex = commands::launch(
        &host,
        &c.root,
        &parse_target("01-a/u/S2", true).unwrap(),
        None,
    )
    .unwrap();
    assert!(codex.ok(), "{:?}", codex.violations);
    assert_eq!(codex.fields["lane"], "codex");
    assert!(codex.fields["launch"].is_null());
    let pi = commands::launch(
        &host,
        &c.root,
        &parse_target("01-a/u/S2b", true).unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(rules(&pi), vec!["worktree-missing"]);

    let state_md = fs::read_to_string(c.root.join("01-a/u/STATE.md")).unwrap();
    assert!(state_md.contains("## Order\n"), "{state_md}");
    assert!(
        state_md.contains("| S2b | pi | pr-open:01-a/u/S1 |"),
        "{state_md}"
    );
    let again = declare(&host, &c, "01-a/u/S2", Lane::Codex, &["S1"]);
    assert!(again.ok());
    let changed = declare(&host, &c, "01-a/u/S2", Lane::Pi, &["S1"]);
    assert_eq!(rules(&changed), vec!["step-declared"]);
    let lint = lint(&c.root).unwrap();
    assert!(lint.ok(), "{:#?}", lint.violations);
}

#[test]
fn a_kickoff_less_or_paused_step_is_not_next() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Owner, &[]).ok());
    assert_eq!(ready(&c, None), Vec::<String>::new());
    assert_eq!(rules(&next_one(&c, "01-a/u/S1")), vec!["packet-kickoff"]);
    kickoff(&c, "01-a/u/S1");
    assert_eq!(ready(&c, None), vec!["01-a/u/S1"]);
    let limits = fs::read_to_string(c.root.join("LIMITS.toml"))
        .unwrap()
        .replace("paused = false", "paused = true");
    fs::write(c.root.join("LIMITS.toml"), limits).unwrap();
    assert_eq!(ready(&c, None), Vec::<String>::new());
    assert_eq!(rules(&next_one(&c, "01-a/u/S1")), vec!["limits-paused"]);
}

#[test]
fn release_steps_name_increasing_versions_and_their_tags() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert_eq!(
        rules(&declare(
            &host,
            &c,
            "00-release/dekopon/v1.0.0",
            Lane::Pi,
            &[]
        )),
        vec!["release-version"]
    );
    assert!(declare(&host, &c, "00-release/dekopon/0.49.0", Lane::Pi, &[]).ok());
    assert_eq!(
        rules(&declare(
            &host,
            &c,
            "00-release/dekopon/0.48.9",
            Lane::Pi,
            &[]
        )),
        vec!["release-version"]
    );
    assert!(declare(&host, &c, "00-release/dekopon/0.50.0", Lane::Pi, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Codex, &["tag:dekopon@0.49.0"]).ok());
    assert!(
        declare(
            &host,
            &c,
            "01-a/u/S2",
            Lane::Codex,
            &["deployed:dekopon@0.49.0"]
        )
        .ok()
    );
    let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    assert_eq!(
        unit.steps["S2"].after[0].target(),
        "00-release/dekopon/0.49.0"
    );
    let lint = lint(&c.root).unwrap();
    assert!(lint.ok(), "{:#?}", lint.violations);
}

#[derive(Default)]
struct FakeForge {
    pulls: BTreeMap<String, Pull>,
    tags: BTreeMap<(String, String), BTreeSet<String>>,
    broken: BTreeSet<String>,
    calls: RefCell<usize>,
}

impl Forge for FakeForge {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError> {
        *self.calls.borrow_mut() += 1;
        if self.broken.contains(url) {
            return Err(ForgeError::Exit(Some(1)));
        }
        self.pulls.get(url).cloned().ok_or(ForgeError::Json)
    }

    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError> {
        Ok(self.tags.contains_key(&(repo.to_string(), tag.to_string())))
    }

    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError> {
        Ok(self
            .tags
            .get(&(repo.to_string(), tag.to_string()))
            .is_some_and(|shas| shas.contains(sha.as_str())))
    }
}

fn merged(sha: &str) -> Pull {
    Pull {
        state: PullState::Merged,
        merge_commit: Some(CommitSha::new(sha)),
    }
}

#[test]
fn sync_moves_steps_from_github_state_and_renders_the_card() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    let feature = "https://github.com/dekopon-agents/dekopon/pull/600";
    let prep = "https://github.com/dekopon-agents/dekopon/pull/601";
    let closed = "https://github.com/dekopon-agents/dekopon/pull/602";
    let flaky = "https://github.com/dekopon-agents/other/pull/1";
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Pi, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/S9", Lane::Pi, &[]).ok());
    assert!(declare(&host, &c, "02-b/v/S1", Lane::Pi, &[]).ok());
    assert!(
        declare(
            &host,
            &c,
            "00-release/dekopon/0.49.0",
            Lane::Pi,
            &["01-a/u/S1"]
        )
        .ok()
    );
    assert!(
        declare(
            &host,
            &c,
            "03-c/w/S1",
            Lane::Codex,
            &["deployed:dekopon@0.49.0"]
        )
        .ok()
    );
    for target in [
        "01-a/u/S1",
        "01-a/u/S9",
        "02-b/v/S1",
        "00-release/dekopon/0.49.0",
        "03-c/w/S1",
    ] {
        kickoff(&c, target);
    }
    open_pr(&host, &c, "01-a/u/S1", feature);
    open_pr(&host, &c, "01-a/u/S9", closed);
    open_pr(&host, &c, "02-b/v/S1", flaky);

    let mut forge = FakeForge::default();
    forge.pulls.insert(feature.into(), merged("aaa1"));
    forge.pulls.insert(
        closed.into(),
        Pull {
            state: PullState::Closed,
            merge_commit: None,
        },
    );
    forge.broken.insert(flaky.into());
    let later = at("2026-10-17T09:05:00Z");
    let first = sync(&later, &forge, &c.root).unwrap();
    assert_eq!(rules(&first), vec!["pr-closed", "sync-unknown"]);
    assert_eq!(state(&c, "01-a/u/S1"), Phase::Merged);
    assert_eq!(state(&c, "01-a/u/S9"), Phase::PrOpen);
    let card = fs::read_to_string(c.root.join("CARD.md")).unwrap();
    assert_eq!(card.lines().next(), Some("Last run: 2026-10-17T09:05Z"));
    assert!(
        card.contains("- `00-release/dekopon/0.49.0` (pi)"),
        "{card}"
    );
    assert!(
        card.contains("- `01-a/u/S1` pr-open → merged (sha aaa1)"),
        "{card}"
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("## 2026-10-17T09:05Z — cm sync: 1 move(s) from GitHub\n"));

    let next = plan::next(&c.root, None, None).unwrap();
    assert_eq!(
        next.fields["steps"][0]["target"],
        "00-release/dekopon/0.49.0"
    );
    open_pr(&host, &c, "00-release/dekopon/0.49.0", prep);
    forge.pulls.insert(prep.into(), merged("bbb2"));
    let quiet = sync(&later, &FakeForge::default(), &c.root).unwrap();
    assert_eq!(
        rules(&quiet),
        vec!["sync-unknown", "sync-unknown", "sync-unknown"]
    );
    let tagless = sync(&later, &forge, &c.root).unwrap();
    assert_eq!(rules(&tagless), vec!["pr-closed", "sync-unknown"]);
    assert_eq!(state(&c, "00-release/dekopon/0.49.0"), Phase::Merged);
    let next = plan::next(&c.root, None, None).unwrap();
    assert_eq!(next.fields["tags"][0]["version"], "0.49.0");
    assert_eq!(next.fields["tags"][0]["sha"], "bbb2");
    assert_eq!(next.fields["tags"][0]["repo"], "dekopon-agents/dekopon");

    forge.tags.insert(
        ("dekopon-agents/dekopon".into(), "v0.49.0".into()),
        ["aaa1".to_owned(), "bbb2".to_owned()].into(),
    );
    let tagged = sync(&later, &forge, &c.root).unwrap();
    assert_eq!(rules(&tagged), vec!["pr-closed", "sync-unknown"]);
    assert_eq!(state(&c, "00-release/dekopon/0.49.0"), Phase::Released);
    assert_eq!(state(&c, "01-a/u/S1"), Phase::Released);
    let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    assert_eq!(
        unit.steps["S1"].track.evidence.tag.as_deref(),
        Some("v0.49.0")
    );
    assert_eq!(rules(&next_one(&c, "03-c/w/S1")), vec!["after-unmet"]);

    let digest = format!("sha256:{}", "c".repeat(64));
    let deployed = advance(
        &host,
        &c,
        "00-release/dekopon/0.49.0",
        "deployed",
        Evidence {
            digest: Some(digest.clone()),
            ..Evidence::default()
        },
    );
    assert!(deployed.ok(), "{:?}", deployed.violations);
    let calls = *forge.calls.borrow();
    sync(&later, &forge, &c.root).unwrap();
    assert_eq!(*forge.calls.borrow(), calls + 2, "one read per open PR");
    assert_eq!(state(&c, "01-a/u/S1"), Phase::Deployed);
    let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    assert_eq!(
        unit.steps["S1"].track.evidence.digest.as_deref(),
        Some(digest.as_str())
    );
    assert!(next_one(&c, "03-c/w/S1").ok());
    let lint = lint(&c.root).unwrap();
    assert!(lint.ok(), "{:#?}", lint.violations);
}

#[test]
fn a_release_tag_must_name_its_version() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(
        declare_release(
            &host,
            &c,
            "00-release/cm/0.3.0",
            "dekopon-agents/cm",
            "cm-{version}"
        )
        .ok()
    );
    kickoff(&c, "00-release/cm/0.3.0");
    open_pr(
        &host,
        &c,
        "00-release/cm/0.3.0",
        "https://github.com/dekopon-agents/cm/pull/19",
    );
    let wt = c.root.parent().unwrap().join("wt");
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    assert!(
        advance(
            &host,
            &c,
            "00-release/cm/0.3.0",
            "merged",
            Evidence {
                sha: Some(sha),
                ..Evidence::default()
            }
        )
        .ok()
    );
    git(&wt, &["tag", "cm-0.2.9"]);
    git(&wt, &["tag", "cm-0.3.0"]);
    git(&wt, &["tag", "v0.3.0"]);
    for wrong in ["cm-0.2.9", "v0.3.0"] {
        let wrong = advance(
            &host,
            &c,
            "00-release/cm/0.3.0",
            "released",
            Evidence {
                tag: Some(wrong.into()),
                ..Evidence::default()
            },
        );
        assert_eq!(rules(&wrong), vec!["release-tag"]);
    }
    let right = advance(
        &host,
        &c,
        "00-release/cm/0.3.0",
        "released",
        Evidence {
            tag: Some("cm-0.3.0".into()),
            ..Evidence::default()
        },
    );
    assert!(right.ok(), "{:?}", right.violations);
}

const EPOCH_MD: &str = "---\nepoch: d\nstatus: OPEN\nplanning_started_at: null\nworking_started_at: null\nclosed_at: null\nskill_revision: a461b854\napproved_plan_revision: null\nideas: [IDEA-0001]\n---\n\n# D\n";

#[test]
fn epoch_moves_one_status_at_a_time_and_refuses_null_pins() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    let path = c.root.join("EPOCH.md");
    assert_eq!(
        rules(&epoch::epoch(&host, &c.root, Status::Planning, None, None).unwrap()),
        vec!["epoch-missing"]
    );
    fs::write(
        &path,
        EPOCH_MD.replace("skill_revision: a461b854", "skill_revision: null"),
    )
    .unwrap();
    assert_eq!(
        rules(&epoch::epoch(&host, &c.root, Status::Planning, None, None).unwrap()),
        vec!["epoch-pin"]
    );
    assert_eq!(
        rules(&epoch::epoch(&host, &c.root, Status::Working, Some("abc"), Some("def")).unwrap()),
        vec!["epoch-status"]
    );
    assert_eq!(
        rules(&epoch::epoch(&host, &c.root, Status::Planning, Some("x: y"), None).unwrap()),
        vec!["epoch-pin"]
    );
    let planning = epoch::epoch(&host, &c.root, Status::Planning, Some("a461b854"), None).unwrap();
    assert!(planning.ok(), "{:?}", planning.violations);
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("status: PLANNING\nplanning_started_at: 2026-10-17\n"),
        "{text}"
    );
    assert!(text.contains("skill_revision: a461b854\n"), "{text}");
    assert!(text.ends_with("\n# D\n"));

    let refused = epoch::epoch(&host, &c.root, Status::Working, None, None).unwrap();
    assert_eq!(rules(&refused), vec!["epoch-pin"]);
    assert!(
        refused.violations[0]
            .message
            .contains("approved_plan_revision")
    );
    let working =
        epoch::epoch(&host, &c.root, Status::Working, None, Some("2175090728ea")).unwrap();
    assert!(working.ok(), "{:?}", working.violations);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("working_started_at: 2026-10-17\n"), "{text}");
    assert!(
        text.contains("approved_plan_revision: 2175090728ea\n"),
        "{text}"
    );

    assert_eq!(
        rules(&epoch::epoch(&host, &c.root, Status::Closed, None, None).unwrap()),
        vec!["epoch-open-campaign"]
    );
    assert!(commands::close(&host, &c.root).unwrap().ok());
    assert!(
        epoch::epoch(&host, &c.root, Status::Closed, None, None)
            .unwrap()
            .ok()
    );
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("status: CLOSED\n") && text.contains("closed_at: 2026-10-17\n"));
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains("## 2026-10-17T08:00Z — epoch working\n\n- status: PLANNING → WORKING\n")
    );
}

#[test]
fn the_journal_is_checked_against_git_and_runtime_files_stay_out_of_it() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(!c.root.join(".cm/JOURNAL.md").exists());
    assert_eq!(
        fs::read_to_string(c.root.join(".cm/.gitignore")).unwrap(),
        "lock\nJOURNAL.md\n"
    );
    git(&c.root, &["init", "-q"]);
    git(&c.root, &["add", "-A"]);
    git(&c.root, &["commit", "-q", "-m", "ledger"]);
    assert_eq!(
        git(&c.root, &["ls-files", ".cm"]),
        ".cm/.gitignore\n.cm/state.json"
    );
    assert!(
        commands::journal(&host, &c.root, "after the commit", None)
            .unwrap()
            .ok()
    );
    assert!(lint(&c.root).unwrap().ok());

    fs::write(c.root.join(".cm/JOURNAL.md"), "stale copy\n").unwrap();
    git(&c.root, &["add", "-f", ".cm/JOURNAL.md", ".cm/lock"]);
    let tracked = lint(&c.root).unwrap();
    assert_eq!(rules(&tracked), vec!["cm-tracked", "cm-tracked"]);
    assert!(
        tracked.violations[0]
            .message
            .contains("git rm --cached .cm/JOURNAL.md")
    );
    git(
        &c.root,
        &["rm", "-q", "--cached", ".cm/JOURNAL.md", ".cm/lock"],
    );
    assert!(lint(&c.root).unwrap().ok());

    let path = c.root.join("JOURNAL.md");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\n## 2026-10-17T08:00Z — stamped by hand\n");
    fs::write(&path, &text).unwrap();
    assert!(
        lint(&c.root).unwrap().ok(),
        "an appended stamped entry is an append"
    );
    git(&c.root, &["commit", "-q", "-am", "entries"]);
    text.push_str("tacked on\n");
    fs::write(&path, &text).unwrap();
    let appended = lint(&c.root).unwrap();
    assert_eq!(rules(&appended), vec!["journal-hand-append"]);
    assert_eq!(appended.violations[0].line, Some(text.lines().count()));
}

fn merge(host: &Fake, c: &Campaign, target: &str, pr: &str) -> String {
    open_pr(host, c, target, pr);
    let sha = git(&c.root.parent().unwrap().join("wt"), &["rev-parse", "HEAD"]);
    let merged = advance(
        host,
        c,
        target,
        "merged",
        Evidence {
            sha: Some(sha.clone()),
            ..Evidence::default()
        },
    );
    assert!(merged.ok(), "{:?}", merged.violations);
    sha
}

#[test]
fn a_release_moves_only_on_its_declared_tag_in_its_declared_repo() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    let guest = "00-release/guest/0.7.0";
    assert_eq!(
        rules(&declare_release(
            &host,
            &c,
            guest,
            "xrl/vm-runner",
            "guest-v0.7.0"
        )),
        vec!["release-tag"]
    );
    assert!(declare_release(&host, &c, guest, "xrl/vm-runner", "guest-v{version}").ok());
    let declaration = Declaration {
        lane: Some(Lane::Pi),
        ships: Ships {
            github: Some("xrl/vm-runner".parse().unwrap()),
            tag: None,
        },
        ..Declaration::default()
    };
    let feature = commands::declare(&host, &c.root.join("01-a/u/S1"), &declaration).unwrap();
    assert_eq!(rules(&feature), vec!["init-target"]);
    kickoff(&c, guest);
    let prep = "https://github.com/xrl/vm-runner/pull/5";
    open_pr(&host, &c, guest, prep);
    let mut forge = FakeForge::default();
    forge.pulls.insert(prep.into(), merged("m1"));
    forge.tags.insert(
        ("xrl/vm-runner".into(), "v0.7.0".into()),
        ["m1".to_owned()].into(),
    );
    sync(&host, &forge, &c.root).unwrap();
    assert_eq!(
        state(&c, guest),
        Phase::Merged,
        "the runner's v0.7.0 is not guest's tag"
    );
    let next = plan::next(&c.root, None, None).unwrap();
    assert_eq!(
        next.fields["tags"][0]["tags"],
        serde_json::json!(["guest-v0.7.0"])
    );
    forge.tags.insert(
        ("xrl/vm-runner".into(), "guest-v0.7.0".into()),
        ["m1".to_owned()].into(),
    );
    sync(&host, &forge, &c.root).unwrap();
    assert_eq!(state(&c, guest), Phase::Released);

    let runner = "00-release/runner/0.4.0";
    assert!(declare(&host, &c, runner, Lane::Pi, &[]).ok());
    kickoff(&c, runner);
    let worktree = c.root.parent().unwrap().join("wt");
    assert!(
        commands::launch(
            &host,
            &c.root,
            &parse_target(runner, true).unwrap(),
            Some(&worktree)
        )
        .unwrap()
        .ok()
    );
    let elsewhere = advance(
        &host,
        &c,
        runner,
        "pr-open",
        Evidence {
            pr: Some("https://github.com/xrl/vm-runner/pull/6".into()),
            sha: Some(git(&worktree, &["rev-parse", "HEAD"])),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&elsewhere), vec!["release-repo"]);
}

#[test]
fn a_member_is_deployed_by_any_deployed_release_of_its_line_that_contains_it() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    for target in ["01-a/u/S1", "00-release/cm/0.3.0", "00-release/cm/0.3.1"] {
        assert!(declare(&host, &c, target, Lane::Pi, &[]).ok());
        kickoff(&c, target);
    }
    let prs = [
        ("01-a/u/S1", "https://github.com/o/cm/pull/1", "s1"),
        (
            "00-release/cm/0.3.0",
            "https://github.com/o/cm/pull/2",
            "p0",
        ),
        (
            "00-release/cm/0.3.1",
            "https://github.com/o/cm/pull/3",
            "p1",
        ),
    ];
    let mut forge = FakeForge::default();
    for (target, pr, sha) in prs {
        open_pr(&host, &c, target, pr);
        forge.pulls.insert(pr.into(), merged(sha));
    }
    forge.tags.insert(
        ("o/cm".into(), "v0.3.0".into()),
        ["s1".to_owned(), "p0".to_owned()].into(),
    );
    forge.tags.insert(
        ("o/cm".into(), "v0.3.1".into()),
        ["s1".to_owned(), "p0".to_owned(), "p1".to_owned()].into(),
    );
    sync(&host, &forge, &c.root).unwrap();
    assert_eq!(state(&c, "01-a/u/S1"), Phase::Released);
    let digest = format!("sha256:{}", "d".repeat(64));
    let deployed = advance(
        &host,
        &c,
        "00-release/cm/0.3.1",
        "deployed",
        Evidence {
            digest: Some(digest.clone()),
            ..Evidence::default()
        },
    );
    assert!(deployed.ok(), "{:?}", deployed.violations);
    sync(&host, &forge, &c.root).unwrap();
    assert_eq!(state(&c, "01-a/u/S1"), Phase::Deployed);
    assert!(advance(&host, &c, "01-a/u/S1", "done", Evidence::default()).ok());
}

#[test]
fn a_met_gate_stays_met_while_its_dependency_is_blocked() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Pi, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/S2", Lane::Pi, &["pr-open:S1"]).ok());
    kickoff(&c, "01-a/u/S1");
    kickoff(&c, "01-a/u/S2");
    open_pr(&host, &c, "01-a/u/S1", "https://github.com/o/r/pull/1");
    assert!(advance(&host, &c, "01-a/u/S1", "blocked:ci", Evidence::default()).ok());
    let next = next_one(&c, "01-a/u/S2");
    assert!(next.ok(), "{:?}", next.violations);
}

#[test]
fn next_lists_tags_in_version_order() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    for (target, pr) in [
        ("00-release/cm/0.9.0", "https://github.com/o/cm/pull/1"),
        ("00-release/cm/0.10.0", "https://github.com/o/cm/pull/2"),
    ] {
        assert!(declare(&host, &c, target, Lane::Pi, &[]).ok());
        kickoff(&c, target);
        merge(&host, &c, target, pr);
    }
    let next = plan::next(&c.root, None, None).unwrap();
    let versions: Vec<&str> = next.fields["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tag| tag["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["0.9.0", "0.10.0"]);
}

#[test]
fn a_refused_step_is_listed_as_waiting_and_never_as_next() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Pi, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/S2", Lane::Pi, &["S1"]).ok());
    kickoff(&c, "01-a/u/S2");
    let refused = next_one(&c, "01-a/u/S2");
    assert_eq!(rules(&refused), vec!["after-unmet"]);
    assert_eq!(refused.fields["steps"], serde_json::json!([]));
    assert_eq!(refused.fields["waiting"][0]["target"], "01-a/u/S2");
    assert!(
        refused.fields["waiting"][0]["waits_on"][0]
            .as_str()
            .unwrap()
            .contains("01-a/u/S1")
    );
    kickoff(&c, "01-a/u/S1");
    let ready = next_one(&c, "01-a/u/S1");
    assert!(ready.ok());
    assert_eq!(ready.fields["steps"][0]["target"], "01-a/u/S1");
}

#[test]
fn a_paused_campaign_lists_nothing_next_on_the_card() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Pi, &[]).ok());
    kickoff(&c, "01-a/u/S1");
    let limits = fs::read_to_string(c.root.join("LIMITS.toml"))
        .unwrap()
        .replace("paused = false", "paused = true");
    fs::write(c.root.join("LIMITS.toml"), limits).unwrap();
    sync(&host, &FakeForge::default(), &c.root).unwrap();
    let card = fs::read_to_string(c.root.join("CARD.md")).unwrap();
    assert!(
        card.contains("## Next\n\nNothing is launchable.\n"),
        "{card}"
    );
    assert!(card.contains("- `01-a/u/S1` waits on"), "{card}");
}

#[cfg(unix)]
mod support;

#[test]
#[cfg(unix)]
fn sync_reads_github_without_holding_the_lock() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Pi, &[]).ok());
    kickoff(&c, "01-a/u/S1");
    let pr = "https://github.com/o/r/pull/1";
    open_pr(&host, &c, "01-a/u/S1", pr);
    let gh = support::FakeGh::new(
        r#"touch "$FAKE_GH/started"
while [ ! -f "$FAKE_GH/release" ]; do sleep 0.05; done
echo '{"state":"closed","merged":true,"merge_commit_sha":"abc123"}'
"#,
    );
    let cm = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        command
            .arg("-C")
            .arg(&c.root)
            .args(args)
            .env("PATH", gh.path())
            .stdout(Stdio::null());
        command
    };
    let mut syncing = cm(&["sync"]).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gh.fixture().join("started").exists() {
        assert!(Instant::now() < deadline, "gh never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut journal = cm(&["journal", "while gh is out"]).spawn().unwrap();
    let journaled = loop {
        if let Some(status) = journal.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            journal.kill().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    fs::write(gh.fixture().join("release"), "").unwrap();
    assert!(syncing.wait().unwrap().success());
    assert!(
        journaled.is_some_and(|status| status.success()),
        "cm journal waited on sync's lock while gh ran"
    );
    assert_eq!(state(&c, "01-a/u/S1"), Phase::Merged);
    let text = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(text.contains("while gh is out"));
    assert!(text.contains("cm sync: 1 move(s) from GitHub"));
}

#[test]
#[cfg(unix)]
fn a_stalled_gh_is_killed_at_its_timeout() {
    use std::time::{Duration, Instant};

    let gh = support::FakeGh::new("exec sleep 30\n");
    let forge = Gh {
        program: gh.program(),
        timeout: Duration::from_millis(300),
    };
    let start = Instant::now();
    let read = forge.pull("https://github.com/o/r/pull/1");
    assert_eq!(read, Err(ForgeError::Timeout(Duration::from_millis(300))));
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_release_declared_before_its_tag_was_recorded_takes_it_once() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    let chart = "00-release/dekopon-chart/1.2.0";
    assert!(declare(&host, &c, chart, Lane::Pi, &[]).ok());
    let path = c.root.join("00-release/dekopon-chart/.cm/state.json");
    let mut stored: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    stored["steps"]["1.2.0"]
        .as_object_mut()
        .unwrap()
        .remove("tag");
    fs::write(&path, serde_json::to_string_pretty(&stored).unwrap()).unwrap();
    let recorded = declare_release(
        &host,
        &c,
        chart,
        "dekopon-agents/dekopon",
        "dekopon-chart-{version}",
    );
    assert!(recorded.ok(), "{:?}", recorded.violations);
    assert_eq!(recorded.fields["tag"], "dekopon-chart-1.2.0");
    let again = declare_release(
        &host,
        &c,
        chart,
        "dekopon-agents/dekopon",
        "dekopon-chart-{version}",
    );
    assert!(again.ok() && again.lines[0].starts_with("nothing to do"));
    let changed = declare_release(&host, &c, chart, "dekopon-agents/dekopon", "v{version}");
    assert_eq!(rules(&changed), vec!["step-declared"]);
    assert!(lint(&c.root).unwrap().ok());
}

fn finish(c: &Campaign, target: &str, file: Option<&Path>) -> Outcome {
    commands::advance(
        &at("2026-10-18T09:00:00Z"),
        &c.root,
        &parse_target(target, false).unwrap(),
        &Phase::Done,
        &Evidence {
            file: file.map(|file| file.to_string_lossy().into_owned()),
            ..Evidence::default()
        },
        None,
    )
    .unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
#[cfg(unix)]
fn a_study_done_with_evidence_frees_its_consumer() {
    let host = at("2026-10-18T08:00:00Z");
    let c = campaign(&host);
    let study = declare_work(
        &host,
        &c,
        "01-a/u/LOOK",
        Lane::Coordinator,
        &[],
        Work::Study,
    );
    assert!(study.ok(), "{:?}", study.violations);
    assert_eq!(study.fields["study"], true);
    assert!(declare(&host, &c, "01-a/u/BUILD", Lane::Pi, &["LOOK"]).ok());
    kickoff(&c, "01-a/u/LOOK");
    kickoff(&c, "01-a/u/BUILD");
    assert_eq!(ready(&c, None), ["01-a/u/LOOK"]);
    let waiting = next_one(&c, "01-a/u/BUILD");
    assert_eq!(rules(&waiting), vec!["after-unmet"]);
    assert!(
        waiting.violations[0]
            .message
            .contains("01-a/u/LOOK (needs done, is drafted)"),
        "{:?}",
        waiting.violations
    );

    assert_eq!(
        rules(&finish(&c, "01-a/u/LOOK", None)),
        vec!["evidence-missing"]
    );
    let missing = c.root.join("01-a/u/LOOK/findings.md");
    assert_eq!(
        rules(&finish(&c, "01-a/u/LOOK", Some(&missing))),
        vec!["evidence-file"]
    );
    let outside = c.root.parent().unwrap().join("elsewhere.md");
    fs::write(&outside, "x").unwrap();
    assert_eq!(
        rules(&finish(&c, "01-a/u/LOOK", Some(&outside))),
        vec!["evidence-file"]
    );
    assert_eq!(state(&c, "01-a/u/LOOK"), Phase::Drafted);

    fs::write(&missing, "Findings.\n").unwrap();
    let gh = support::FakeGh::new("echo unexpected gh call >&2\nexit 1\n");
    let cm = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&c.root)
            .args(args)
            .env("PATH", gh.path())
            .output()
            .unwrap()
    };
    let done = cm(&[
        "advance",
        "01-a/u/LOOK",
        "done",
        "--evidence",
        "01-a/u/LOOK/findings.md",
    ]);
    assert!(done.status.success(), "{done:?}");
    assert_eq!(state(&c, "01-a/u/LOOK"), Phase::Done);
    let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    let track = &unit.steps["LOOK"].track;
    assert_eq!(
        track.evidence.file.as_deref(),
        Some("01-a/u/LOOK/findings.md")
    );
    let moved = unit.history.last().unwrap();
    assert_eq!((&moved.from, &moved.to), (&Phase::Drafted, &Phase::Done));
    assert_eq!(moved.at, track.since);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains("- evidence: file 01-a/u/LOOK/findings.md\n"),
        "{journal}"
    );

    let rendered = fs::read_to_string(c.root.join("01-a/u/STATE.md")).unwrap();
    assert!(
        rendered.contains("| LOOK | coordinator, study | — |\n"),
        "{rendered}"
    );
    assert_eq!(ready(&c, None), ["01-a/u/BUILD"]);
    assert!(cm(&["sync"]).status.success());
    let card = fs::read_to_string(c.root.join("CARD.md")).unwrap();
    assert!(
        card.contains("## Next\n\n- `01-a/u/BUILD` (pi)\n"),
        "{card}"
    );
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn done_refuses_a_code_step_and_an_owner_step_short_of_evidence_or_gates() {
    let host = at("2026-10-18T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/CODE", Lane::Pi, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/ASK", Lane::Owner, &["CODE"]).ok());
    let note = c.root.join("01-a/u/note.md");
    fs::write(&note, "Answered.\n").unwrap();
    kickoff(&c, "01-a/u/CODE");
    open_pr(&host, &c, "01-a/u/CODE", "https://github.com/o/r/pull/1");
    assert_eq!(
        rules(&finish(&c, "01-a/u/CODE", Some(&note))),
        vec!["done-code-step"]
    );
    assert_eq!(
        rules(&finish(&c, "01-a/u/CODE", None)),
        vec!["done-code-step"]
    );
    assert_eq!(state(&c, "01-a/u/CODE"), Phase::PrOpen);

    assert_eq!(
        rules(&finish(&c, "01-a/u/ASK", None)),
        vec!["after-unmet", "evidence-missing"]
    );
    assert_eq!(
        rules(&finish(&c, "01-a/u/ASK", Some(&note))),
        vec!["after-unmet"]
    );
    let sha = git(&c.root.parent().unwrap().join("wt"), &["rev-parse", "HEAD"]);
    let merged = advance(
        &host,
        &c,
        "01-a/u/CODE",
        "merged",
        Evidence {
            sha: Some(sha),
            ..Evidence::default()
        },
    );
    assert!(merged.ok(), "{:?}", merged.violations);
    let asked = finish(&c, "01-a/u/ASK", Some(&note));
    assert!(asked.ok(), "{:?}", asked.violations);
    assert_eq!(
        rules(&finish(&c, "01-a/u/ASK", Some(&note))),
        vec!["advance-refused"]
    );
}

#[test]
fn a_coordinator_lane_launch_prints_no_spawn_line() {
    let host = at("2026-10-18T08:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/PIN", Lane::Coordinator, &[]).ok());
    kickoff(&c, "01-a/u/PIN");
    assert_eq!(ready(&c, Some(Lane::Coordinator)), ["01-a/u/PIN"]);
    let launched = commands::launch(
        &host,
        &c.root,
        &parse_target("01-a/u/PIN", true).unwrap(),
        None,
    )
    .unwrap();
    assert!(launched.ok(), "{:?}", launched.violations);
    assert_eq!(launched.fields["launch"], serde_json::Value::Null);
    assert_eq!(launched.fields["lane"], "coordinator");
    assert_eq!(
        launched.lines.last().unwrap(),
        "launched 01-a/u/PIN in lane coordinator; its prompt is 01-a/u/PIN/kickoff.md"
    );
    assert!(
        launched
            .lines
            .iter()
            .all(|line| !line.contains("pi-turn.sh"))
    );
    assert_eq!(state(&c, "01-a/u/PIN"), Phase::Launched);
}

#[test]
fn a_campaign_from_cm_0_3_1_loads_unchanged_and_its_owner_step_finishes() {
    let host = at("2026-10-18T08:00:00Z");
    let temp = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/campaign-0.3.1");
    let root = temp.path().join("campaign");
    copy_tree(&fixture, &root);
    let c = Campaign { _temp: temp, root };
    let clean = lint(&c.root).unwrap();
    assert!(clean.ok(), "{:#?}", clean.violations);
    for unit in ["01-a/u", "00-release/cm"] {
        let path = c.root.join(unit).join(".cm/state.json");
        let stored = fs::read_to_string(&path).unwrap();
        let loaded = store::load_unit(&c.root.join(unit)).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&loaded).unwrap().trim_end(),
            stored.trim_end(),
            "{unit}"
        );
    }
    assert_eq!(
        state(&c, "01-a/u/PLAN"),
        Phase::Blocked("waiting-owner".into())
    );
    let release = store::load_unit(&c.root.join("00-release/cm")).unwrap();
    assert_eq!(
        release.steps["0.3.1"].tag.as_ref().map(ToString::to_string),
        Some("cm-0.3.1".to_owned())
    );
    kickoff(&c, "01-a/u/BUILD");
    assert_eq!(rules(&next_one(&c, "01-a/u/BUILD")), vec!["after-unmet"]);

    let decision = c.root.join("01-a/u/PLAN/decision.md");
    fs::write(&decision, "The owner chose the first.\n").unwrap();
    let done = finish(&c, "01-a/u/PLAN", Some(&decision));
    assert!(done.ok(), "{:?}", done.violations);
    assert_eq!(done.fields["from"], "blocked:waiting-owner");
    assert_eq!(ready(&c, None), ["01-a/u/BUILD"]);
    assert!(declare(&host, &c, "01-a/u/LATER", Lane::Codex, &["SPIKE"]).ok());
    let after = lint(&c.root).unwrap();
    assert!(after.ok(), "{:#?}", after.violations);
}
