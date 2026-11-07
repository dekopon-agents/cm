use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use cm::commands;
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::state::{Evidence, Phase, UnitState};
use cm::store::{self, Target, parse_target};
use jiff::Timestamp;

#[path = "support/campaign.rs"]
mod campaign_fixture;
#[path = "support/kickoff.rs"]
mod kickoff_fixture;

use campaign_fixture::{Campaign, campaign, git, repo};

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

fn unit_with_step(c: &Campaign, host: &Fake, kickoff: &str) {
    let outcome = commands::init(host, &c.root.join("01-first/alpha")).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    fs::create_dir_all(c.root.join("01-first/alpha/BUILD")).unwrap();
    fs::write(c.root.join("01-first/alpha/BUILD/kickoff.md"), kickoff).unwrap();
}

fn commit_campaign(root: &Path) {
    if !root.join(".git").exists() {
        git(root, &["init", "-q"]);
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "ledger"]);
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
    let template = cm::templates::RESUME;
    let fence = template.find("> ```").unwrap();
    let end = template.find("\n\n# Resume").unwrap();
    let resume = format!(
        "{}> {}{}",
        &template[..fence],
        cm::templates::LOOP_WITHHELD,
        &template[end..]
    )
    .replace("Nothing has shipped yet.", "Our own words.");
    fs::write(root.join("RESUME.md"), &resume).unwrap();
    fs::write(
        root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nAnswer:\n",
    )
    .unwrap();
    let journal = "# Journal\n\n## 2026-09-28 and 29: planning\n\n- old prose\n";
    fs::write(root.join("JOURNAL.md"), journal).unwrap();

    let outcome = commands::init(&host, root).unwrap();
    assert_eq!(
        rules(&outcome),
        vec!["owner-queue-field", "owner-queue-field"]
    );
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
        "# Owner queue\n\n## Pick one\n\nContext: two paths.\n\nRecommendation: the first\n\nAnswer:\n",
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
        Some(&c.root.join("no-such-worktree")),
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
    let outcome = commands::launch(
        &host,
        &c.root,
        &parse_target(step, true).unwrap(),
        Some(&repo),
    )
    .unwrap();
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
        commands::launch(
            &host,
            &c.root,
            &parse_target(step, true).unwrap(),
            Some(&repo)
        )
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
        commands::launch(&host, &c.root, &launch_target, Some(&repo))
            .unwrap()
            .ok()
    );
    let again = commands::launch(&host, &c.root, &launch_target, Some(&repo)).unwrap();
    assert_eq!(rules(&again), vec!["launch-state"]);
    assert!(advance(&host, &c, step, "blocked:driver", Evidence::default()).ok());
    for name in ["resume-2.md", "resume-10.md"] {
        fs::write(c.root.join("01-first/alpha/BUILD").join(name), "Resume.\n").unwrap();
    }
    let outcome = commands::launch(&host, &c.root, &launch_target, Some(&repo)).unwrap();
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
    commit_campaign(&c.root);
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
        "# Owner queue\n\n## Pick one\n\nContext: two paths.\n\nRecommendation: yes\n",
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
            "resume-loop",
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
    commit_campaign(&c.root);
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

#[test]
fn a_removed_worktree_points_advance_at_repo() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, "Go.\n");
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::launch(
            &host,
            &c.root,
            &parse_target(step, true).unwrap(),
            Some(&repo)
        )
        .unwrap()
        .ok()
    );
    let moved = c.root.parent().unwrap().join("moved");
    fs::rename(&repo, &moved).unwrap();
    let pr = Evidence {
        pr: Some("https://github.com/o/r/pull/7".into()),
        sha: Some(sha.clone()),
        ..Evidence::default()
    };
    let refused = advance(&host, &c, step, "pr-open", pr.clone());
    assert_eq!(rules(&refused), vec!["evidence-repo"]);
    assert!(
        refused.violations[0]
            .message
            .contains("is gone; pass --repo")
    );
    let outcome = commands::advance(
        &host,
        &c.root,
        &target(step),
        &"pr-open".parse().unwrap(),
        &pr,
        None,
        Some(&moved),
    )
    .unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
}

#[test]
fn hand_edit_messages_name_the_reset() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    unit_with_step(&c, &host, "Go.\n");
    commit_campaign(&c.root);
    let state_md = c.root.join("01-first/alpha/STATE.md");
    fs::write(&state_md, "edited\n").unwrap();
    let mut journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    journal.push_str("extra\n");
    fs::write(c.root.join("JOURNAL.md"), journal).unwrap();
    let outcome = lint(&c.root).unwrap();
    let messages: Vec<&str> = outcome
        .violations
        .iter()
        .map(|v| v.message.as_str())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("into a new entry with `cm journal`")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`rm 01-first/alpha/STATE.md && cm init 01-first/alpha`")),
        "{messages:?}"
    );
    fs::remove_file(&state_md).unwrap();
    git(&c.root, &["checkout", "HEAD", "--", "JOURNAL.md"]);
    assert!(
        commands::init(&host, &c.root.join("01-first/alpha"))
            .unwrap()
            .ok()
    );
    assert_clean(&c.root);
}

#[test]
fn close_seals_the_campaign() {
    let host = Fake::at("2026-10-15T18:00:00Z");
    let c = campaign(&host);
    let (repo, _) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, &format!("Work in `{}`.\n", repo.display()));
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::launch(
            &host,
            &c.root,
            &parse_target(step, true).unwrap(),
            Some(&repo)
        )
        .unwrap()
        .ok()
    );
    fs::write(
        c.root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nContext: two paths.\n\nRecommendation: yes\n\nAnswer:\n",
    )
    .unwrap();

    let refused = commands::close(&host, &c.root).unwrap();
    assert_eq!(
        rules(&refused),
        vec!["closed-queue", "closed-track", "closed-track"],
        "{:#?}",
        refused.violations
    );
    assert!(store::load_campaign(&c.root).unwrap().closed.is_none());

    fs::write(c.root.join("OWNER-QUEUE.md"), "# Owner queue\n").unwrap();
    assert!(advance(&host, &c, step, "blocked:superseded", Evidence::default()).ok());
    assert!(
        advance(
            &host,
            &c,
            "01-first/alpha",
            "blocked:superseded",
            Evidence::default()
        )
        .ok()
    );
    let closed = commands::close(&host, &c.root).unwrap();
    assert!(closed.ok(), "{:#?}", closed.violations);
    assert_eq!(
        store::load_campaign(&c.root).unwrap().closed.as_deref(),
        Some("2026-10-15T18:00Z")
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("## 2026-10-15T18:00Z — cm close\n"));

    assert_eq!(rules(&lint(&c.root).unwrap()), vec!["resume-loop"]);
    let resume = fs::read_to_string(c.root.join("RESUME.md")).unwrap();
    let start = resume.find("> ```").unwrap();
    let end = resume.find("\n\n# Resume").unwrap();
    fs::write(
        c.root.join("RESUME.md"),
        format!(
            "{}> Closed; the successor governs.{}",
            &resume[..start],
            &resume[end..]
        ),
    )
    .unwrap();
    assert_clean(&c.root);

    assert_eq!(
        rules(&advance(&host, &c, step, "launched", Evidence::default())),
        vec!["advance-refused", "campaign-closed"]
    );
    assert_eq!(
        rules(&commands::init(&host, &c.root.join("01-first/beta")).unwrap()),
        vec!["campaign-closed"]
    );
    assert_eq!(
        rules(&commands::close(&host, &c.root).unwrap()),
        vec!["campaign-closed"]
    );
    assert!(
        commands::journal(&host, &c.root, "follow-up", None)
            .unwrap()
            .ok()
    );
    assert_clean(&c.root);
}

#[test]
fn init_writes_a_kickoff_once_and_repairs_a_missing_kickoff() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let step = "01-first/alpha/BUILD";
    let file = kickoff_fixture::file("Build the feature.\n");
    let init = || {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args(["--json", "init", step, "--lane", "pi", "--kickoff-file"])
            .arg(file.path())
            .output()
            .unwrap()
    };
    let first = init();
    assert!(first.status.success(), "{first:?}");
    let fields: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(fields["kickoff"], format!("{step}/kickoff.md"));
    assert!(fields["journal"].is_string());
    let kickoff = c.root.join(step).join("kickoff.md");
    assert_eq!(
        fs::read_to_string(&kickoff).unwrap(),
        "Build the feature.\n"
    );
    fs::write(file.path(), "A different file.\n").unwrap();
    let before = fs::read(c.root.join("JOURNAL.md")).unwrap();
    let kept = init();
    assert!(kept.status.success(), "{kept:?}");
    let fields: serde_json::Value = serde_json::from_slice(&kept.stdout).unwrap();
    assert_eq!(fields["kickoff_kept"], true);
    assert_eq!(fields["kickoff"], format!("{step}/kickoff.md"));
    assert_eq!(
        fs::read_to_string(&kickoff).unwrap(),
        "Build the feature.\n"
    );
    assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), before);
    fs::remove_dir_all(c.root.join(step)).unwrap();
    fs::remove_file(c.root.join("01-first/alpha/STATE.md")).unwrap();
    let repaired = init();
    assert!(repaired.status.success(), "{repaired:?}");
    let fields: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(
        fields["repaired"],
        serde_json::json!([
            format!("{step}/"),
            format!("{step}/kickoff.md"),
            "01-first/alpha/STATE.md"
        ])
    );
    assert_eq!(fs::read_to_string(&kickoff).unwrap(), "A different file.\n");
    assert_clean(&c.root);
}

#[test]
fn init_accepts_a_kickoff_on_stdin_and_rejects_empty_input() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    for text in ["", " \n\t", "Do the work.\n"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args([
                "--json",
                "init",
                "01-first/alpha/BUILD",
                "--lane",
                "pi",
                "--kickoff-file",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.success(), !text.trim().is_empty());
        if text.trim().is_empty() {
            assert!(!c.root.join("01-first/alpha").exists());
            let fields: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(fields["violations"][0]["rule"], "kickoff-file");
        }
    }
    assert_eq!(
        fs::read_to_string(c.root.join("01-first/alpha/BUILD/kickoff.md")).unwrap(),
        "Do the work.\n"
    );
}

#[test]
fn release_init_inherits_the_newest_declared_repository_and_tag_pattern() {
    use cm::commands::{Declaration, Ships};
    use cm::state::Lane;

    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let mut declaration = Declaration {
        lane: Some(Lane::Pi),
        ships: Ships {
            github: Some("example/old".parse().unwrap()),
            tag: Some("v{version}".into()),
        },
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/widget/1.9.0"), &declaration)
            .unwrap()
            .ok()
    );
    declaration.ships = Ships {
        github: Some("example/project".parse().unwrap()),
        tag: Some("cm-{version}".into()),
    };
    assert!(
        commands::declare(
            &host,
            &c.root.join("00-release/widget/1.10.0"),
            &declaration
        )
        .unwrap()
        .ok()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(&c.root)
        .args(["init", "00-release/widget/1.11.0", "--lane", "pi"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let unit = store::load_unit(&c.root.join("00-release/widget")).unwrap();
    let inherited = &unit.steps["1.11.0"];
    assert_eq!(
        inherited.github.as_ref().unwrap().to_string(),
        "example/project"
    );
    assert_eq!(inherited.tag.as_ref().unwrap().as_str(), "cm-1.11.0");
    let retry = Declaration {
        lane: Some(Lane::Pi),
        kickoff: Some("Release prep.\n".into()),
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/widget/1.10.0"), &retry)
            .unwrap()
            .ok()
    );
    assert_eq!(
        fs::read_to_string(c.root.join("00-release/widget/1.10.0/kickoff.md")).unwrap(),
        "Release prep.\n"
    );
    let explicit = Declaration {
        lane: Some(Lane::Pi),
        ships: Ships {
            github: Some("example/override".parse().unwrap()),
            tag: None,
        },
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/widget/1.12.0"), &explicit)
            .unwrap()
            .ok()
    );
    let unit = store::load_unit(&c.root.join("00-release/widget")).unwrap();
    assert_eq!(
        unit.steps["1.12.0"].tag.as_ref().unwrap().as_str(),
        "v1.12.0"
    );
    let defaults = Declaration {
        lane: Some(Lane::Pi),
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/other/1.0.0"), &defaults)
            .unwrap()
            .ok()
    );
    let other = store::load_unit(&c.root.join("00-release/other")).unwrap();
    assert_eq!(other.steps["1.0.0"].github, None);
    assert_eq!(
        other.steps["1.0.0"].tag.as_ref().unwrap().as_str(),
        "v1.0.0"
    );
}

#[test]
fn init_reports_a_preexisting_kickoff_as_kept() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let step = "01-first/alpha/BUILD";
    c.kickoff(step, "Existing work.\n");
    let declaration = commands::Declaration {
        lane: Some(cm::state::Lane::Pi),
        kickoff: Some("Different work.\n".into()),
        ..commands::Declaration::default()
    };
    let outcome = commands::declare(&host, &c.root.join(step), &declaration).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    assert_eq!(outcome.fields["kickoff_kept"], true);
    assert_eq!(outcome.fields["kickoff"], format!("{step}/kickoff.md"));
    assert_eq!(
        fs::read_to_string(c.root.join(step).join("kickoff.md")).unwrap(),
        "Existing work.\n"
    );
}

#[test]
fn inherited_defaults_refuse_git_invalid_tags_saved_by_older_versions() {
    use cm::commands::Declaration;
    use cm::state::Lane;

    let host = Fake::at("2026-10-08T07:04:00Z");
    for tag in [
        "release/.hidden/1.2.3",
        "release/part.lock/1.2.3",
        "release/1.2.3.lock",
    ] {
        let c = campaign(&host);
        let declaration = Declaration {
            lane: Some(Lane::Pi),
            ..Declaration::default()
        };
        assert!(
            commands::declare(&host, &c.root.join("00-release/widget/1.2.3"), &declaration)
                .unwrap()
                .ok()
        );
        c.legacy_release_tag("00-release/widget", "1.2.3", tag);
        let state_path = store::state_path(&c.root.join("00-release/widget"));
        let before = fs::read(&state_path).unwrap();
        let refused =
            commands::declare(&host, &c.root.join("00-release/widget/1.2.4"), &declaration)
                .unwrap();
        assert_eq!(rules(&refused), vec!["release-tag"]);
        assert!(!c.root.join("00-release/widget/1.2.4").exists());
        assert_eq!(fs::read(&state_path).unwrap(), before);
    }
}

#[test]
fn legacy_release_tags_load_lint_and_can_be_repaired_by_amend() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let step = "00-release/widget/1.2.3";
    let declaration = commands::Declaration {
        lane: Some(cm::state::Lane::Pi),
        ..commands::Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join(step), &declaration)
            .unwrap()
            .ok()
    );
    let baseline = lint(&c.root).unwrap();
    c.legacy_release_tag("00-release/widget", "1.2.3", "release/1.2.3.lock");
    let unit_dir = c.root.join("00-release/widget");
    let unit = store::load_unit(&unit_dir).unwrap();
    assert_eq!(
        unit.steps["1.2.3"].tag.as_ref().unwrap().as_str(),
        "release/1.2.3.lock"
    );
    let legacy_lint = lint(&c.root).unwrap();
    assert_eq!(legacy_lint.violations, baseline.violations);
    assert_eq!(legacy_lint.fields["warnings"], baseline.fields["warnings"]);
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(&c.root)
        .args(["--json", "amend", step, "--tag", "v{version}"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let unit = store::load_unit(&unit_dir).unwrap();
    assert_eq!(unit.steps["1.2.3"].tag.as_ref().unwrap().as_str(), "v1.2.3");
    assert_clean(&c.root);
}

#[test]
fn kickoff_inputs_accept_the_byte_limit_and_refuse_larger_files_and_stdin() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let limit = 1024 * 1024;
    for command in ["init", "amend"] {
        for source in ["file", "stdin"] {
            for size in [limit, limit + 1] {
                let step = format!("01-first/alpha/{command}-{source}-{size}");
                if command == "amend" {
                    let declaration = commands::Declaration {
                        lane: Some(cm::state::Lane::Pi),
                        kickoff: Some("Existing work.\n".into()),
                        ..commands::Declaration::default()
                    };
                    assert!(
                        commands::declare(&host, &c.root.join(&step), &declaration)
                            .unwrap()
                            .ok()
                    );
                }
                let input = kickoff_fixture::file(&"x".repeat(size));
                let before = fs::read(c.root.join("JOURNAL.md")).unwrap();
                let state_path = store::state_path(&c.root.join("01-first/alpha"));
                let state_before = fs::read(&state_path).ok();
                let mut cli = Command::new(env!("CARGO_BIN_EXE_cm"));
                cli.current_dir(&c.root).args(["--json", command, &step]);
                if command == "init" {
                    cli.args(["--lane", "pi"]);
                }
                cli.arg("--kickoff-file");
                if source == "stdin" {
                    cli.arg("-").stdin(Stdio::from(input.reopen().unwrap()));
                } else {
                    cli.arg(input.path());
                }
                let output = cli.output().unwrap();
                let kickoff = c.root.join(&step).join("kickoff.md");
                if size == limit {
                    assert!(output.status.success(), "{output:?}");
                    assert_eq!(fs::read(&kickoff).unwrap(), vec![b'x'; limit]);
                } else {
                    assert_eq!(output.status.code(), Some(1));
                    let fields: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(fields["violations"][0]["rule"], "kickoff-size");
                    assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), before);
                    assert_eq!(fs::read(&state_path).ok(), state_before);
                    if command == "amend" {
                        assert_eq!(fs::read(&kickoff).unwrap(), b"Existing work.\n");
                    } else {
                        assert!(!c.root.join(&step).exists());
                    }
                }
            }
        }
    }
}
