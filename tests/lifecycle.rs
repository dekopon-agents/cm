use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm::commands;
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::state::{Evidence, Phase, UnitState};
use cm::store::{self, Target, parse_target};
use jiff::Timestamp;

struct Fake {
    now: Timestamp,
    free_gib: u64,
    builds: u32,
}

impl Fake {
    fn at(now: &str) -> Self {
        Fake {
            now: now.parse().unwrap(),
            free_gib: 100,
            builds: 0,
        }
    }
}

impl Host for Fake {
    fn now(&self) -> Timestamp {
        self.now
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(self.free_gib << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(self.builds)
    }
}

fn rules(outcome: &Outcome) -> Vec<&'static str> {
    let mut rules: Vec<_> = outcome.violations.iter().map(|v| v.rule).collect();
    rules.sort_unstable();
    rules
}

fn target(text: &str) -> Target {
    parse_target(text, false).unwrap()
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

fn repo(dir: &Path) -> (PathBuf, String) {
    let repo = dir.join("wt");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    (repo, sha)
}

struct Campaign {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

fn campaign(host: &Fake) -> Campaign {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    let outcome = commands::init(host, &root).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    Campaign { _temp: temp, root }
}

fn unit_with_step(c: &Campaign, host: &Fake, kickoff: &str) {
    let outcome = commands::init(host, &c.root.join("01-first/alpha")).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    fs::create_dir_all(c.root.join("01-first/alpha/BUILD")).unwrap();
    fs::write(c.root.join("01-first/alpha/BUILD/kickoff.md"), kickoff).unwrap();
}

fn assert_clean(root: &Path) {
    let outcome = lint(root).unwrap();
    assert!(outcome.ok(), "{:#?}", outcome.violations);
}

#[test]
fn init_writes_templates_and_lints_clean() {
    let host = Fake::at("2026-10-08T07:04:31Z");
    let c = campaign(&host);
    for name in [
        "RESUME.md",
        "JOURNAL.md",
        "LIMITS.toml",
        "OWNER-QUEUE.md",
        ".cm/state.json",
    ] {
        assert!(c.root.join(name).is_file(), "{name}");
    }
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains("\n## 2026-10-08T07:04Z — cm init\n"),
        "{journal}"
    );
    assert_clean(&c.root);
    let again = commands::init(&host, &c.root).unwrap();
    assert!(again.ok());
    assert_eq!(
        fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
        journal
    );
}

#[test]
fn init_never_overwrites_and_adopts_what_lints_clean() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let resume = cm::templates::RESUME.replace("Nothing has shipped yet.", "Our own words.");
    fs::write(root.join("RESUME.md"), &resume).unwrap();
    fs::write(
        root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nAnswer:\n",
    )
    .unwrap();
    let journal = "# Journal\n\n## 2026-09-28 and 29: planning\n\n- old prose\n";
    fs::write(root.join("JOURNAL.md"), journal).unwrap();

    let outcome = commands::init(&host, root).unwrap();
    assert_eq!(rules(&outcome), vec!["owner-queue-field"]);
    assert_eq!(fs::read_to_string(root.join("RESUME.md")).unwrap(), resume);
    assert_eq!(
        fs::read_to_string(root.join("OWNER-QUEUE.md")).unwrap(),
        "# Owner queue\n\n## Pick one\n\nAnswer:\n"
    );
    let state = store::load_campaign(root).unwrap();
    assert_eq!(state.adopted, vec!["RESUME.md", "JOURNAL.md"]);
    assert_eq!(state.written, vec!["LIMITS.toml"]);

    fs::write(
        root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nRecommendation: the first\n\nAnswer:\n",
    )
    .unwrap();
    let outcome = commands::init(&host, root).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    assert_eq!(
        store::load_campaign(root).unwrap().adopted,
        vec!["RESUME.md", "JOURNAL.md", "OWNER-QUEUE.md"]
    );
    let text = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    assert!(text.starts_with(journal));
    assert_clean(root);
}

#[test]
fn journal_stamps_come_from_the_clock() {
    let c = campaign(&Fake::at("2026-10-08T07:00:00Z"));
    let later = Fake::at("2026-10-08T07:09:59Z");
    let outcome = commands::journal(
        &later,
        &c.root,
        "ruled on S3",
        Some("Body line.\n\n### detail\n"),
    )
    .unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    let text = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        text.ends_with("\n## 2026-10-08T07:09Z — ruled on S3\n\nBody line.\n\n### detail\n"),
        "{text}"
    );
    let forged = commands::journal(
        &later,
        &c.root,
        "x",
        Some("## 2026-10-08T07:0xZ — forged\n"),
    )
    .unwrap();
    assert_eq!(rules(&forged), vec!["journal-entry"]);
    assert_clean(&c.root);
}

#[test]
fn launch_reports_every_failed_check_at_once() {
    let mut host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    unit_with_step(
        &c,
        &host,
        "Read `/definitely/not/here.md` and `~/also/not/here`.\n",
    );
    let limits = fs::read_to_string(c.root.join("LIMITS.toml"))
        .unwrap()
        .replace("paused = false", "paused = true");
    fs::write(c.root.join("LIMITS.toml"), limits).unwrap();
    host.free_gib = 27;
    host.builds = 4;
    let outcome = commands::launch(
        &host,
        &c.root,
        &parse_target("01-first/alpha/BUILD", true).unwrap(),
        &c.root.join("no-such-worktree"),
    )
    .unwrap();
    assert_eq!(
        rules(&outcome),
        vec![
            "limits-disk",
            "limits-heavy-builds",
            "limits-paused",
            "packet-path",
            "packet-path",
            "worktree-missing"
        ],
        "{:#?}",
        outcome.violations
    );
    let unit = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    assert!(unit.steps.is_empty());
    assert_eq!(unit.track.state, Phase::Drafted);
}

fn advance(host: &Fake, c: &Campaign, what: &str, to: &str, evidence: Evidence) -> Outcome {
    commands::advance(
        host,
        &c.root,
        &target(what),
        &to.parse().unwrap(),
        &evidence,
        None,
    )
    .unwrap()
}

#[test]
fn a_step_walks_from_launch_to_done() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, &format!("Work in `{}`.\n", repo.display()));
    let step = "01-first/alpha/BUILD";
    let outcome =
        commands::launch(&host, &c.root, &parse_target(step, true).unwrap(), &repo).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    let command = outcome.fields["launch"].as_str().unwrap();
    assert!(command.contains("pi-turn.sh "), "{command}");
    assert!(
        command.contains(" alpha-BUILD-sup openai-codex/gpt-6-sol:high "),
        "{command}"
    );
    assert!(
        command.ends_with("/sup-turn-1.jsonl kickoff.md"),
        "{command}"
    );

    let pr = Evidence {
        pr: Some("https://github.com/o/r/pull/7".into()),
        sha: Some(sha[..10].into()),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "pr-open", pr).ok());
    let merged = Evidence {
        sha: Some(sha.clone()),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "merged", merged).ok());
    git(&repo, &["tag", "v0.1.0"]);
    let released = Evidence {
        tag: Some("v0.1.0".into()),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "released", released).ok());
    let deployed = Evidence {
        digest: Some(format!("sha256:{}", "a".repeat(64))),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "deployed", deployed).ok());
    assert!(advance(&host, &c, step, "done", Evidence::default()).ok());

    let unit: UnitState = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    let track = &unit.steps["BUILD"].track;
    assert_eq!(track.state, Phase::Done);
    assert_eq!(track.evidence.sha.as_deref(), Some(sha.as_str()));
    assert_eq!(track.evidence.tag.as_deref(), Some("v0.1.0"));
    assert_eq!(unit.history.len(), 7);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains("## 2026-10-08T07:00Z — advance 01-first/alpha/BUILD: deployed → done\n")
    );
    assert_clean(&c.root);
}

#[test]
fn advance_refuses_a_skip_and_missing_or_unexpected_evidence() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, _) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, "No paths.\n");
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::launch(&host, &c.root, &parse_target(step, true).unwrap(), &repo)
            .unwrap()
            .ok()
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();

    let skip = advance(
        &host,
        &c,
        step,
        "released",
        Evidence {
            tag: Some("v9".into()),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&skip), vec!["advance-refused", "evidence-tag"]);
    assert!(skip.violations[0].message.contains("skips pr-open, merged"));

    let thin = advance(
        &host,
        &c,
        step,
        "pr-open",
        Evidence {
            pr: Some("https://example.com/pull/1".into()),
            digest: Some("sha256:abc".into()),
            ..Evidence::default()
        },
    );
    assert_eq!(
        rules(&thin),
        vec![
            "evidence-digest",
            "evidence-missing",
            "evidence-pr",
            "evidence-unexpected"
        ]
    );
    let relaunch = advance(&host, &c, step, "launched", Evidence::default());
    assert_eq!(rules(&relaunch), vec!["advance-refused"]);
    let unknown = advance(
        &host,
        &c,
        "01-first/alpha/NOPE",
        "pr-open",
        Evidence::default(),
    );
    assert_eq!(rules(&unknown), vec!["step-unknown"]);

    assert_eq!(
        fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
        journal
    );
    assert_eq!(
        store::load_unit(&c.root.join("01-first/alpha"))
            .unwrap()
            .steps["BUILD"]
            .track
            .state,
        Phase::Launched
    );
}

#[test]
fn a_blocked_step_relaunches_from_its_newest_resume() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, _) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, "Go.\n");
    let step = "01-first/alpha/BUILD";
    let launch_target = parse_target(step, true).unwrap();
    assert!(
        commands::launch(&host, &c.root, &launch_target, &repo)
            .unwrap()
            .ok()
    );
    let again = commands::launch(&host, &c.root, &launch_target, &repo).unwrap();
    assert_eq!(rules(&again), vec!["launch-state"]);
    assert!(advance(&host, &c, step, "blocked:driver", Evidence::default()).ok());
    for name in ["resume-2.md", "resume-10.md"] {
        fs::write(c.root.join("01-first/alpha/BUILD").join(name), "Resume.\n").unwrap();
    }
    let outcome = commands::launch(&host, &c.root, &launch_target, &repo).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    let command = outcome.fields["launch"].as_str().unwrap();
    assert!(
        command.ends_with("/sup-turn-2.jsonl resume-10.md"),
        "{command}"
    );
    assert_clean(&c.root);
}

#[test]
fn lint_lists_every_violation_in_one_run() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    unit_with_step(&c, &host, "Read `/definitely/not/here.md`.\n");
    let state_md = c.root.join("01-first/alpha/STATE.md");
    let edited = fs::read_to_string(&state_md)
        .unwrap()
        .replace("| drafted |", "| merged |");
    fs::write(&state_md, edited).unwrap();
    let mut journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    journal.push_str("\n## 2026-10-08T07:0xZ — typed by a model\n");
    fs::write(c.root.join("JOURNAL.md"), journal).unwrap();
    let resume = fs::read_to_string(c.root.join("RESUME.md"))
        .unwrap()
        .replace("## How to work\n", "");
    fs::write(c.root.join("RESUME.md"), resume).unwrap();
    let limits = fs::read_to_string(c.root.join("LIMITS.toml"))
        .unwrap()
        .replace("max_heavy_builds = 4\n", "");
    fs::write(c.root.join("LIMITS.toml"), limits).unwrap();
    fs::write(
        c.root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nRecommendation: yes\n",
    )
    .unwrap();

    let outcome = lint(&c.root).unwrap();
    assert_eq!(
        rules(&outcome),
        vec![
            "journal-hand-append",
            "journal-unstamped-heading",
            "limits-key",
            "owner-queue-field",
            "packet-path",
            "resume-heading",
            "unit-state-hand-edit",
        ],
        "{:#?}",
        outcome.violations
    );
}

#[test]
fn a_rewritten_journal_prefix_is_found_and_blocks_writes() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let path = c.root.join("JOURNAL.md");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("— cm init", "— cm init, edited");
    fs::write(&path, text).unwrap();
    let outcome = lint(&c.root).unwrap();
    assert_eq!(rules(&outcome), vec!["journal-rewritten"]);
    assert_eq!(outcome.violations[0].line, Some(5));
    let refused = commands::journal(&host, &c.root, "next", None).unwrap();
    assert_eq!(rules(&refused), vec!["journal-rewritten"]);
}
