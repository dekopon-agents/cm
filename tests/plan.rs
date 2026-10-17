use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm::commands;
use cm::epoch::{self, Status};
use cm::forge::{Forge, ForgeError, Pull, PullState};
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::plan;
use cm::state::{Evidence, Lane, Phase};
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

fn declare(host: &Fake, c: &Campaign, target: &str, lane: Lane, after: &[&str]) -> Outcome {
    let after: Vec<String> = after.iter().map(|text| (*text).to_owned()).collect();
    commands::declare(host, &c.root.join(target), Some(lane), &after).unwrap()
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

    fn tag_exists(&self, repo: &str, tag: &str) -> Result<bool, ForgeError> {
        Ok(self.tags.contains_key(&(repo.to_owned(), tag.to_owned())))
    }

    fn tag_contains(&self, repo: &str, tag: &str, sha: &str) -> Result<bool, ForgeError> {
        Ok(self
            .tags
            .get(&(repo.to_owned(), tag.to_owned()))
            .is_some_and(|shas| shas.contains(sha)))
    }
}

fn merged(sha: &str) -> Pull {
    Pull {
        state: PullState::Merged,
        merge_commit: Some(sha.into()),
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
    assert!(declare(&host, &c, "00-release/cm/0.3.0", Lane::Pi, &[]).ok());
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
    let wrong = advance(
        &host,
        &c,
        "00-release/cm/0.3.0",
        "released",
        Evidence {
            tag: Some("cm-0.2.9".into()),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&wrong), vec!["release-tag"]);
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
