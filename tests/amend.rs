use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm::commands::{self, Amendment, Declaration};
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::plan;
use cm::state::{Evidence, Lane, Phase, Work};
use cm::store::{self, parse_target};
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

struct Campaign {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

fn campaign(host: &Fake) -> Campaign {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    assert!(commands::init(host, &root).unwrap().ok());
    let git = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&root)
        .status();
    assert!(
        git.unwrap().success(),
        "a campaign lives in a git repository"
    );
    Campaign { _temp: temp, root }
}

fn declare(host: &Fake, c: &Campaign, target: &str, lane: Lane, after: &[&str]) -> Outcome {
    let declaration = Declaration {
        lane: Some(lane),
        after: after.iter().map(|text| (*text).to_owned()).collect(),
        ..Declaration::default()
    };
    let outcome = commands::declare(host, &c.root.join(target), &declaration).unwrap();
    fs::write(c.root.join(target).join("kickoff.md"), "Go.\n").unwrap();
    outcome
}

fn target(text: &str) -> store::Target {
    parse_target(text, false).unwrap()
}

fn note(c: &Campaign, name: &str) -> PathBuf {
    let path = c.root.join(name);
    fs::write(&path, "The owner answered.\n").unwrap();
    path
}

fn advance(host: &Fake, c: &Campaign, step: &str, to: &str, file: Option<&Path>) -> Outcome {
    commands::advance(
        host,
        &c.root,
        &target(step),
        &to.parse().unwrap(),
        &Evidence::default(),
        file,
        None,
    )
    .unwrap()
}

fn state(c: &Campaign, step: &str) -> Phase {
    let units = plan::load_units(&c.root).unwrap();
    plan::step(&units, step).unwrap().track.state.clone()
}

fn ready(c: &Campaign) -> Vec<String> {
    let outcome = plan::next(&c.root, None, None).unwrap();
    outcome.fields["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["target"].as_str().unwrap().to_owned())
        .collect()
}

fn launch(host: &Fake, c: &Campaign, step: &str) -> Outcome {
    commands::launch(host, &c.root, &target(step), None).unwrap()
}

#[test]
fn an_owner_answer_unblocks_parked_work_with_its_evidence() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::ClaudeCloud, &[]).ok());
    let answer = note(&c, "answer.md");
    let early = commands::unblock(&host, &c.root, &target("01-a/u/S1"), &answer).unwrap();
    assert_eq!(rules(&early), vec!["unblock-state"]);
    assert!(advance(&host, &c, "01-a/u/S1", "blocked:owner", None).ok());
    let missing = c.root.join("nope.md");
    let refused = commands::unblock(&host, &c.root, &target("01-a/u/S1"), &missing).unwrap();
    assert_eq!(rules(&refused), vec!["evidence-file"]);
    let done = commands::unblock(&host, &c.root, &target("01-a/u/S1"), &answer).unwrap();
    assert!(done.ok(), "{:?}", done.violations);
    assert_eq!(state(&c, "01-a/u/S1"), Phase::Drafted);
    assert_eq!(ready(&c), vec!["01-a/u/S1"]);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("unblock 01-a/u/S1: blocked:owner → drafted"));
    assert!(journal.contains("- evidence: file answer.md"));
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn blocked_draft_still_requires_its_after_gate() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/gate", Lane::Owner, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/producer", Lane::Owner, &[]).ok());
    assert!(
        declare(
            &host,
            &c,
            "01-a/u/consumer",
            Lane::ClaudeCloud,
            &["gate", "producer"]
        )
        .ok()
    );
    assert!(advance(&host, &c, "01-a/u/consumer", "blocked:owner", None).ok());
    let gate = note(&c, "gate.md");
    assert!(advance(&host, &c, "01-a/u/gate", "done", Some(&gate)).ok());
    let direct = launch(&host, &c, "01-a/u/consumer");
    assert_eq!(rules(&direct), vec!["after-unmet"]);
    assert!(direct.violations[0].message.contains("producer"));
    let unblocked = commands::unblock(&host, &c.root, &target("01-a/u/consumer"), &gate).unwrap();
    assert!(unblocked.ok());
    assert_eq!(unblocked.fields["waits_on"].as_array().unwrap().len(), 1);
    assert!(!ready(&c).contains(&"01-a/u/consumer".to_owned()));
    assert_eq!(
        rules(&launch(&host, &c, "01-a/u/consumer")),
        vec!["after-unmet"]
    );
    assert!(advance(&host, &c, "01-a/u/producer", "done", Some(&gate)).ok());
    assert_eq!(ready(&c), vec!["01-a/u/consumer"]);
}

#[test]
fn amend_marks_a_study_moves_a_lane_and_adds_an_acyclic_gate() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/plan", Lane::Fable, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/study", Lane::ClaudeCloud, &["plan"]).ok());
    let amend = |step: &str, amendment: Amendment| {
        commands::amend(&host, &c.root, &target(step), &amendment).unwrap()
    };
    let changed = amend(
        "01-a/u/study",
        Amendment {
            study: true,
            lane: Some(Lane::Coordinator),
            after: Vec::new(),
            ..Amendment::default()
        },
    );
    assert!(changed.ok(), "{:?}", changed.violations);
    let units = plan::load_units(&c.root).unwrap();
    let step = plan::step(&units, "01-a/u/study").unwrap();
    assert_eq!(
        (step.work, step.lane),
        (Work::Study, Some(Lane::Coordinator))
    );
    let cycle = amend(
        "01-a/u/plan",
        Amendment {
            after: vec!["study".into()],
            ..Amendment::default()
        },
    );
    assert_eq!(rules(&cycle), vec!["after-cycle"]);
    let same = amend(
        "01-a/u/study",
        Amendment {
            study: true,
            ..Amendment::default()
        },
    );
    assert_eq!(rules(&same), vec!["amend-empty"]);
    let evidence = note(&c, "found.md");
    assert!(advance(&host, &c, "01-a/u/plan", "blocked:wait", None).ok());
    assert_eq!(
        rules(&advance(&host, &c, "01-a/u/study", "done", Some(&evidence))),
        vec!["after-unmet"]
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("amend 01-a/u/study"));
    assert!(journal.contains("- lane: claude-cloud → coordinator"));
    let late = amend(
        "01-a/u/study",
        Amendment {
            lane: Some(Lane::Pi),
            ..Amendment::default()
        },
    );
    assert!(late.ok());
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn identical_init_repairs_a_partial_declaration() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Codex, &[]).ok());
    let state_md = c.root.join("01-a/u/STATE.md");
    let rendered = fs::read_to_string(&state_md).unwrap();
    fs::remove_file(&state_md).unwrap();
    assert_eq!(rules(&lint(&c.root).unwrap()), vec!["unit-state-missing"]);
    let again = declare(&host, &c, "01-a/u/S1", Lane::Codex, &[]);
    assert!(again.ok(), "{:?}", again.violations);
    assert_eq!(
        again.fields["repaired"],
        serde_json::json!(["01-a/u/STATE.md"])
    );
    assert_eq!(fs::read_to_string(&state_md).unwrap(), rendered);
    fs::write(&state_md, "stale\n").unwrap();
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Codex, &[]).ok());
    assert_eq!(fs::read_to_string(&state_md).unwrap(), rendered);
    let idle = declare(&host, &c, "01-a/u/S1", Lane::Codex, &[]);
    assert_eq!(idle.fields["repaired"], serde_json::json!([]));
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn a_tag_only_release_and_a_superseded_step_are_recorded() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let repo = c.root.parent().unwrap().join("vm-runner");
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=cm", "-c", "user.email=cm@example.com"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "{args:?}");
    };
    fs::create_dir_all(&repo).unwrap();
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "guest"]);
    git(&["tag", "guest-v0.7.0"]);
    let declaration = Declaration {
        lane: Some(Lane::Coordinator),
        ships: commands::Ships {
            github: Some("xrl/vm-runner".parse().unwrap()),
            tag: Some("guest-v{version}".into()),
        },
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/guest/0.7.0"), &declaration)
            .unwrap()
            .ok()
    );
    let released = commands::advance(
        &host,
        &c.root,
        &target("00-release/guest/0.7.0"),
        &Phase::Released,
        &Evidence {
            tag: Some("guest-v0.7.0".into()),
            ..Evidence::default()
        },
        None,
        Some(&repo),
    )
    .unwrap();
    assert!(released.ok(), "{:?}", released.violations);
    assert_eq!(state(&c, "00-release/guest/0.7.0"), Phase::Released);
    assert!(declare(&host, &c, "01-a/u/gh-review-pin", Lane::Codex, &[]).ok());
    assert!(
        declare(
            &host,
            &c,
            "01-a/u/after-pin",
            Lane::Codex,
            &["gh-review-pin"]
        )
        .ok()
    );
    assert_eq!(
        rules(&advance(
            &host,
            &c,
            "01-a/u/gh-review-pin",
            "superseded",
            None
        )),
        vec!["evidence-missing"]
    );
    let why = note(&c, "superseded.md");
    assert!(advance(&host, &c, "01-a/u/gh-review-pin", "superseded", Some(&why)).ok());
    assert_eq!(state(&c, "01-a/u/gh-review-pin"), Phase::Superseded);
    assert_eq!(
        rules(&advance(
            &host,
            &c,
            "01-a/u/gh-review-pin",
            "blocked:x",
            None
        )),
        vec!["advance-refused"]
    );
    assert!(!ready(&c).contains(&"01-a/u/after-pin".to_owned()));
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn a_consumer_launches_after_its_superseded_gate_is_replaced() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/old", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/replacement", Lane::Coordinator, &[]).ok());
    let study = Amendment {
        study: true,
        ..Amendment::default()
    };
    assert!(
        commands::amend(&host, &c.root, &target("01-a/u/replacement"), &study)
            .unwrap()
            .ok()
    );
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &["old"]).ok());
    let why = note(&c, "superseded.md");
    assert!(advance(&host, &c, "01-a/u/old", "superseded", Some(&why)).ok());
    let done = note(&c, "done.md");
    assert!(advance(&host, &c, "01-a/u/replacement", "done", Some(&done)).ok());
    let regate = Amendment {
        drop_after: vec!["old".into()],
        after: vec!["replacement".into()],
        ..Amendment::default()
    };
    let amended = commands::amend(&host, &c.root, &target("01-a/u/consumer"), &regate).unwrap();
    assert!(amended.ok(), "{:?}", amended.violations);
    let units = plan::load_units(&c.root).unwrap();
    let step = plan::step(&units, "01-a/u/consumer").unwrap();
    assert_eq!(
        step.after
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["01-a/u/replacement"]
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("- after: - 01-a/u/old\n- after: + 01-a/u/replacement\n"));
    let rendered = fs::read_to_string(c.root.join("01-a/u/STATE.md")).unwrap();
    assert!(rendered.contains("| consumer | codex | 01-a/u/replacement |"));
    assert!(lint(&c.root).unwrap().ok());
    let launched = launch(&host, &c, "01-a/u/consumer");
    assert!(launched.ok(), "{:?}", launched.violations);
}

#[test]
fn replacing_a_gate_refuses_an_unknown_replacement() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/old", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &["old"]).ok());
    let amendment = Amendment {
        drop_after: vec!["old".into()],
        after: vec!["unknown".into()],
        ..Amendment::default()
    };
    let refused = commands::amend(&host, &c.root, &target("01-a/u/consumer"), &amendment).unwrap();
    assert_eq!(rules(&refused), vec!["after-unknown"]);
    let units = plan::load_units(&c.root).unwrap();
    assert_eq!(
        plan::step(&units, "01-a/u/consumer").unwrap().after[0].to_string(),
        "01-a/u/old"
    );
}

#[test]
fn removing_a_gate_the_step_lacks_is_refused() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/other", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &[]).ok());
    let amendment = Amendment {
        drop_after: vec!["other".into()],
        ..Amendment::default()
    };
    let refused = commands::amend(&host, &c.root, &target("01-a/u/consumer"), &amendment).unwrap();
    assert_eq!(rules(&refused), vec!["after-absent"]);
}

#[test]
fn replacing_a_gate_cannot_create_a_cycle() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/old", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &["old"]).ok());
    assert!(declare(&host, &c, "01-a/u/replacement", Lane::Codex, &["consumer"]).ok());
    let amendment = Amendment {
        drop_after: vec!["old".into()],
        after: vec!["replacement".into()],
        ..Amendment::default()
    };
    let refused = commands::amend(&host, &c.root, &target("01-a/u/consumer"), &amendment).unwrap();
    assert_eq!(rules(&refused), vec!["after-cycle"]);
}

#[test]
fn a_superseded_prerequisite_keeps_its_consumer_blocked() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/old", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &["old"]).ok());
    let why = note(&c, "superseded.md");
    assert!(advance(&host, &c, "01-a/u/old", "superseded", Some(&why)).ok());
    assert_eq!(
        rules(&launch(&host, &c, "01-a/u/consumer")),
        vec!["after-unmet"]
    );
}

#[test]
fn malformed_gate_removal_is_refused() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &[]).ok());
    let amendment = Amendment {
        drop_after: vec!["tag:invalid".into()],
        ..Amendment::default()
    };
    let refused = commands::amend(&host, &c.root, &target("01-a/u/consumer"), &amendment).unwrap();
    assert_eq!(rules(&refused), vec!["after-parse"]);
}

#[test]
fn the_cli_removes_multiple_gates_from_a_launched_step() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let done = note(&c, "done.md");
    for gate in ["first", "second"] {
        let step = format!("01-a/u/{gate}");
        assert!(declare(&host, &c, &step, Lane::Owner, &[]).ok());
        assert!(advance(&host, &c, &step, "done", Some(&done)).ok());
    }
    assert!(
        declare(
            &host,
            &c,
            "01-a/u/consumer",
            Lane::Codex,
            &["first", "second"]
        )
        .ok()
    );
    assert!(launch(&host, &c, "01-a/u/consumer").ok());
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(&c.root)
        .args([
            "amend",
            "01-a/u/consumer",
            "--drop-after",
            "first",
            "--drop-after",
            "second",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let units = plan::load_units(&c.root).unwrap();
    let step = plan::step(&units, "01-a/u/consumer").unwrap();
    assert_eq!(step.track.state, Phase::Launched);
    assert!(step.after.is_empty());
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn replacing_a_duplicated_superseded_gate_permits_launch() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/old", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/replacement", Lane::Owner, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &["old", "old"]).ok());
    let why = note(&c, "superseded.md");
    assert!(advance(&host, &c, "01-a/u/old", "superseded", Some(&why)).ok());
    let done = note(&c, "done.md");
    assert!(advance(&host, &c, "01-a/u/replacement", "done", Some(&done)).ok());
    let amendment = Amendment {
        drop_after: vec!["old".into()],
        after: vec!["replacement".into()],
        ..Amendment::default()
    };
    let amended = commands::amend(&host, &c.root, &target("01-a/u/consumer"), &amendment).unwrap();
    assert!(amended.ok(), "{:?}", amended.violations);
    let launched = launch(&host, &c, "01-a/u/consumer");
    assert!(launched.ok(), "{:?}", launched.violations);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert_eq!(journal.matches("- after: - 01-a/u/old\n").count(), 1);
}

#[test]
fn dropping_and_readding_the_same_gate_refuses_an_unchanged_amendment() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/old", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "01-a/u/consumer", Lane::Codex, &["old"]).ok());
    let before = fs::read(c.root.join("JOURNAL.md")).unwrap();
    let amendment = Amendment {
        drop_after: vec!["old".into()],
        after: vec!["old".into()],
        ..Amendment::default()
    };
    let refused = commands::amend(&host, &c.root, &target("01-a/u/consumer"), &amendment).unwrap();
    assert_eq!(rules(&refused), vec!["amend-empty"]);
    assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), before);
}
