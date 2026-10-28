use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cm::attempt::{self, Act};
use cm::commands::{self, Declaration, Remote};
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::state::{AttemptOutcome, Evidence, Lane};
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

fn launch(host: &Fake, c: &Campaign, step: &str, cap_min: Option<u32>) -> Outcome {
    let remote = Remote {
        no_parts: None,
        coordinator: Some("session_a".into()),
        cap_min,
    };
    commands::launch_remote(host, &c.root, &target(step), None, &remote).unwrap()
}

fn act(host: &Fake, root: &Path, claim: &str, act: Act) -> Outcome {
    attempt::attempt(host, root, &target("01-a/u/S1"), Some(claim), &act).unwrap()
}

fn begin(host: &Fake, root: &Path, claim: &str, kind: &str) -> Outcome {
    act(host, root, claim, Act::Begin { kind: kind.into() })
}

fn record(host: &Fake, root: &Path, claim: &str, kind: &str, id: &str) -> Outcome {
    let record = Act::Record {
        kind: kind.into(),
        id: id.into(),
    };
    act(host, root, claim, record)
}

fn end(
    host: &Fake,
    root: &Path,
    claim: &str,
    outcome: AttemptOutcome,
    file: Option<PathBuf>,
) -> Outcome {
    act(host, root, claim, Act::End { outcome, file })
}

/// Claims the attempt for `coordinator`, returning the new claim token on success.
fn claim(host: &Fake, root: &Path, coordinator: &str, expect: u32) -> Option<String> {
    let claim = Act::Claim {
        coordinator: coordinator.into(),
        expect,
    };
    let claimed = attempt::attempt(host, root, &target("01-a/u/S1"), None, &claim).unwrap();
    claimed
        .ok()
        .then(|| claimed.fields["claim"].as_str().unwrap().to_owned())
}

fn status(host: &Fake, root: &Path) -> Outcome {
    attempt::attempt(host, root, &target("01-a/u/S1"), None, &Act::Status).unwrap()
}

fn launched(host: &Fake, c: &Campaign, cap_min: Option<u32>) -> String {
    assert!(declare(host, c, "01-a/u/S1", Lane::ClaudeCloud, &[]).ok());
    let launched = launch(host, c, "01-a/u/S1", cap_min);
    assert!(launched.ok(), "{:?}", launched.violations);
    assert_eq!(launched.fields["attempt"]["id"], "01-a/u/S1#1");
    launched.fields["attempt"]["claim"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The remote runtime: each creation is counted, so a redispatch shows.
#[derive(Default)]
struct Runtime {
    threads: u32,
    turns: u32,
}

/// A coordinator that runs the attempt from committed state, stopping at `kill` boundaries.
fn coordinate(host: &Fake, c: &Campaign, runtime: &mut Runtime, token: &str, kill: usize) {
    let mut boundary = 0;
    loop {
        let plan = status(host, &c.root);
        let code = plan.fields["plan"].as_str().unwrap().to_owned();
        let last = plan.fields["attempt"]["actions"]
            .as_array()
            .and_then(|actions| actions.last().cloned());
        let kind = last.map(|last| last["kind"].as_str().unwrap().to_owned());
        let next = match (code.as_str(), kind.as_deref()) {
            ("dispatch", _) => "thread",
            ("resume", Some("thread")) => "turn",
            ("resume", Some("turn")) | ("unknown" | "ended" | "expired", _) => return,
            other => panic!("{other:?}"),
        };
        if boundary == kill {
            return;
        }
        boundary += 1;
        assert!(begin(host, &c.root, token, next).ok());
        let id = if next == "thread" {
            runtime.threads += 1;
            format!("thread-{}", runtime.threads)
        } else {
            runtime.turns += 1;
            format!("turn-{}", runtime.turns)
        };
        let recorded = record(host, &c.root, token, next, &id);
        assert!(recorded.ok(), "{:?}", recorded.violations);
    }
}

#[test]
fn a_replacement_coordinator_resumes_one_execution_from_committed_state() {
    for kill in 0..3 {
        let host = at("2026-10-19T09:00:00Z");
        let c = campaign(&host);
        let first = launched(&host, &c, None);
        let mut runtime = Runtime::default();
        coordinate(&host, &c, &mut runtime, &first, kill);
        let second = claim(&host, &c.root, "session_b", 1).expect("the claim");
        coordinate(&host, &c, &mut runtime, &second, usize::MAX);
        assert_eq!((runtime.threads, runtime.turns), (1, 1), "killed at {kill}");
        assert_eq!(status(&host, &c.root).fields["plan"], "resume");
    }
}

#[test]
fn an_action_with_no_id_closes_only_as_unknown() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let token = launched(&host, &c, None);
    assert!(begin(&host, &c.root, &token, "thread").ok());
    assert_eq!(status(&host, &c.root).fields["plan"], "unknown");
    assert_eq!(
        rules(&begin(&host, &c.root, &token, "thread")),
        vec!["attempt-unknown"]
    );
    let notes = note(&c, "notes.md");
    for outcome in [AttemptOutcome::Complete, AttemptOutcome::Partial] {
        assert_eq!(
            rules(&end(&host, &c.root, &token, outcome, Some(notes.clone()))),
            vec!["attempt-unknown"]
        );
    }
    assert!(end(&host, &c.root, &token, AttemptOutcome::Unknown, None).ok());
    assert!(advance(&host, &c, "01-a/u/S1", "blocked:lost", None).ok());
    assert!(launch(&host, &c, "01-a/u/S1", None).ok());
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn two_coordinators_racing_one_claim_fence_the_loser() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let first = launched(&host, &c, None);
    let won: Vec<Option<String>> = std::thread::scope(|scope| {
        let racers: Vec<_> = ["session_b", "session_c"]
            .into_iter()
            .map(|name| {
                let (host, c) = (&host, &c);
                scope.spawn(move || claim(host, &c.root, name, 1))
            })
            .collect();
        racers
            .into_iter()
            .map(|racer| racer.join().unwrap())
            .collect()
    });
    let winners: Vec<&String> = won.iter().flatten().collect();
    assert_eq!(winners.len(), 1, "{won:?}");
    for stale in [first.as_str(), "2:session_x:2026-10-19T09:00:00Z"] {
        assert_eq!(
            rules(&begin(&host, &c.root, stale, "thread")),
            vec!["attempt-fenced"]
        );
    }
    assert!(begin(&host, &c.root, winners[0], "thread").ok());
}

/// Two clones each claim generation 2; the push race keeps one, and the loser rebases onto it.
#[test]
fn a_clone_that_lost_the_push_race_is_fenced_after_it_rebases() {
    let host = at("2026-10-19T09:00:00Z");
    let a = campaign(&host);
    launched(&host, &a, None);
    let b = campaign(&host);
    copy(&a.root, &b.root);
    let winner = claim(&host, &a.root, "session_b", 1).expect("b claims in its clone");
    let loser = claim(&host, &b.root, "session_c", 1).expect("c claims in its clone");
    assert_ne!(winner, loser);
    assert!(winner.starts_with("2:session_b:") && loser.starts_with("2:session_c:"));
    copy(&a.root, &b.root);
    assert_eq!(
        rules(&begin(&host, &b.root, &loser, "thread")),
        vec!["attempt-fenced"]
    );
    assert_eq!(
        rules(&end(&host, &b.root, &loser, AttemptOutcome::Unknown, None)),
        vec!["attempt-fenced"]
    );
    assert!(begin(&host, &b.root, &winner, "thread").ok());
}

/// What a rebase onto the winner leaves in the loser's clone: the winner's campaign files.
fn copy(from: &Path, to: &Path) {
    for file in ["JOURNAL.md", "01-a/u/.cm/state.json", "01-a/u/STATE.md"] {
        fs::create_dir_all(to.join(file).parent().unwrap()).unwrap();
        fs::copy(from.join(file), to.join(file)).unwrap();
    }
}

#[test]
fn a_capped_attempt_keeps_its_deadline_across_a_replacement() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let first = launched(&host, &c, Some(30));
    assert_eq!(
        status(&host, &c.root).fields["attempt"]["deadline"],
        "2026-10-19T09:30Z"
    );
    for (kind, id) in [("thread", "t1"), ("turn", "u1")] {
        assert!(begin(&host, &c.root, &first, kind).ok());
        assert!(record(&host, &c.root, &first, kind, id).ok());
    }
    let later = at("2026-10-19T09:20:00Z");
    let second = claim(&later, &c.root, "session_b", 1).expect("the claim");
    assert_eq!(status(&later, &c.root).fields["plan"], "resume");
    let overrun = at("2026-10-19T09:30:30Z");
    let expired = status(&overrun, &c.root);
    assert_eq!(expired.fields["plan"], "expired");
    let next = expired.fields["next"].as_str().unwrap();
    assert!(next.contains("cancel turn u1"), "{next}");
    assert_eq!(
        rules(&begin(&overrun, &c.root, &second, "turn")),
        vec!["attempt-deadline"]
    );
    assert!(begin(&overrun, &c.root, &second, "cancel").ok());
    assert!(record(&overrun, &c.root, &second, "cancel", "u1").ok());
    let after_cancel = status(&overrun, &c.root);
    let next = after_cancel.fields["next"].as_str().unwrap();
    assert!(
        next.contains("inspect recorded cancellation u1; cancel turn u1; cancel thread t1"),
        "{next}"
    );
    assert!(!next.contains("cancel cancel"), "{next}");
    assert_eq!(
        rules(&end(
            &overrun,
            &c.root,
            &second,
            AttemptOutcome::Partial,
            None
        )),
        vec!["evidence-missing"]
    );
    let partial = note(&c, "partial.md");
    assert!(
        end(
            &overrun,
            &c.root,
            &second,
            AttemptOutcome::Partial,
            Some(partial)
        )
        .ok()
    );
    let ended = status(&overrun, &c.root);
    assert_eq!(ended.fields["plan"], "ended");
    assert_eq!(ended.fields["attempt"]["ended"]["outcome"], "partial");
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn an_expired_session_attempt_names_the_session_to_cancel() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let first = launched(&host, &c, Some(30));
    assert!(begin(&host, &c.root, &first, "session").ok());
    assert!(record(&host, &c.root, &first, "session", "s1").ok());
    let overrun = at("2026-10-19T09:30:30Z");
    let expired = status(&overrun, &c.root);
    assert_eq!(expired.fields["plan"], "expired");
    let next = expired.fields["next"].as_str().unwrap();
    assert!(next.contains("cancel session s1"), "{next}");
    assert!(!next.contains("nothing was admitted"), "{next}");
}

#[test]
fn an_expired_thread_only_attempt_names_the_thread_to_cancel() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let first = launched(&host, &c, Some(30));
    assert!(begin(&host, &c.root, &first, "thread").ok());
    assert!(record(&host, &c.root, &first, "thread", "t1").ok());
    let overrun = at("2026-10-19T09:30:30Z");
    let expired = status(&overrun, &c.root);
    assert_eq!(expired.fields["plan"], "expired");
    let next = expired.fields["next"].as_str().unwrap();
    assert!(next.contains("cancel thread t1"), "{next}");
}

#[test]
fn an_expired_attempt_names_every_admitted_handle_newest_first() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let first = launched(&host, &c, Some(30));
    assert!(begin(&host, &c.root, &first, "thread").ok());
    assert!(record(&host, &c.root, &first, "thread", "t1").ok());
    assert!(begin(&host, &c.root, &first, "session").ok());
    assert!(record(&host, &c.root, &first, "session", "s1").ok());
    assert!(begin(&host, &c.root, &first, "turn").ok());
    assert!(record(&host, &c.root, &first, "turn", "u1").ok());
    assert!(begin(&host, &c.root, &first, "turn").ok());
    assert!(record(&host, &c.root, &first, "turn", "u2").ok());
    let overrun = at("2026-10-19T09:30:30Z");
    let expired = status(&overrun, &c.root);
    assert_eq!(expired.fields["plan"], "expired");
    let next = expired.fields["next"].as_str().unwrap();
    assert!(
        next.contains("cancel turn u2; cancel turn u1; cancel session s1; cancel thread t1"),
        "{next}"
    );
}

#[test]
fn an_expired_attempt_without_admitted_work_says_so() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    launched(&host, &c, Some(30));
    let overrun = at("2026-10-19T09:30:30Z");
    let expired = status(&overrun, &c.root);
    assert_eq!(expired.fields["plan"], "expired");
    let next = expired.fields["next"].as_str().unwrap();
    assert!(next.contains("nothing was admitted to cancel"), "{next}");
}

#[test]
fn an_expired_attempt_with_an_in_flight_action_stays_unknown() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let first = launched(&host, &c, Some(30));
    assert!(begin(&host, &c.root, &first, "session").ok());
    let overrun = at("2026-10-19T09:30:30Z");
    assert_eq!(status(&overrun, &c.root).fields["plan"], "unknown");
}

#[test]
fn an_ended_attempt_stays_ended_past_its_deadline() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let first = launched(&host, &c, Some(30));
    assert!(end(&host, &c.root, &first, AttemptOutcome::Complete, None).ok());
    let overrun = at("2026-10-19T09:30:30Z");
    assert_eq!(status(&overrun, &c.root).fields["plan"], "ended");
}

fn restart(host: &Fake, root: &Path, token: &str, file: Option<PathBuf>) -> Outcome {
    act(
        host,
        root,
        token,
        Act::Restart {
            lane: Lane::Codex,
            file,
        },
    )
}

#[test]
fn restart_archives_unknown_and_dispatches_under_the_same_claim_and_cap() {
    for pending in [false, true] {
        let host = at("2026-10-19T09:00:00Z");
        let c = campaign(&host);
        let token = launched(&host, &c, Some(30));
        if pending {
            assert!(begin(&host, &c.root, &token, "thread").ok());
        }
        assert_eq!(
            rules(&restart(&host, &c.root, &token, None)),
            vec!["attempt-restart"]
        );
        assert!(end(&host, &c.root, &token, AttemptOutcome::Unknown, None).ok());
        let before = status(&host, &c.root).fields["attempt"].clone();
        let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        let notes = note(&c, "relane.md");
        let restarted = restart(&host, &c.root, &token, Some(notes));
        assert!(restarted.ok(), "{:?}", restarted.violations);
        let current = status(&host, &c.root);
        assert_eq!(current.fields["plan"], "dispatch");
        assert_eq!(current.fields["attempt"]["id"], "01-a/u/S1#2");
        assert_eq!(current.fields["attempt"]["lane"], "codex");
        for field in [
            "generation",
            "claim",
            "deadline",
            "coordinator",
            "claimed_at",
        ] {
            assert_eq!(current.fields["attempt"][field], before[field], "{field}");
        }
        let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
        let step = &unit.steps["S1"];
        assert_eq!(step.launches, 2);
        assert_eq!(step.lane, Some(Lane::ClaudeCloud));
        let archived = &step.attempt_history[0];
        assert_eq!(archived.id, "01-a/u/S1#1");
        assert_eq!(archived.actions.len(), usize::from(pending));
        assert_eq!(
            archived.ended.as_ref().unwrap().outcome,
            AttemptOutcome::Relaned
        );
        assert!(archived.ended.as_ref().unwrap().evidence.is_some());
        assert_eq!(
            fs::read_to_string(c.root.join("JOURNAL.md"))
                .unwrap()
                .strip_prefix(&journal)
                .unwrap()
                .trim(),
            "## 2026-10-19T09:00Z — attempt 01-a/u/S1: relaned 01-a/u/S1#1 → 01-a/u/S1#2 (codex)"
        );
        assert!(lint(&c.root).unwrap().ok());
    }
}

#[test]
fn restart_fences_a_stale_claim_and_refuses_an_expired_cap() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let stale = launched(&host, &c, Some(30));
    let token = claim(&host, &c.root, "session_b", 1).unwrap();
    assert!(end(&host, &c.root, &token, AttemptOutcome::Unknown, None).ok());
    assert_eq!(
        rules(&restart(&host, &c.root, &stale, None)),
        vec!["attempt-fenced"]
    );
    let late = at("2026-10-19T09:30:00Z");
    assert_eq!(
        rules(&restart(&late, &c.root, &token, None)),
        vec!["attempt-deadline"]
    );
    assert_eq!(
        status(&host, &c.root).fields["attempt"]["id"],
        "01-a/u/S1#1"
    );
}

#[test]
fn begin_and_record_each_journal_one_line_with_every_action_field() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    let token = launched(&host, &c, None);
    for (result, expected) in [
        (
            begin(&host, &c.root, &token, "thread"),
            "## 2026-10-19T09:00Z — attempt 01-a/u/S1: 01-a/u/S1#1 begins thread (generation 1)",
        ),
        (
            record(&host, &c.root, &token, "thread", "t1"),
            "## 2026-10-19T09:00Z — attempt 01-a/u/S1: 01-a/u/S1#1 thread t1 (generation 1)",
        ),
    ] {
        assert!(result.ok());
        assert_eq!(result.fields["journal"], expected);
    }
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let tail: Vec<_> = journal.lines().rev().take(3).collect();
    assert_eq!(
        tail,
        vec![
            "## 2026-10-19T09:00Z — attempt 01-a/u/S1: 01-a/u/S1#1 thread t1 (generation 1)",
            "",
            "## 2026-10-19T09:00Z — attempt 01-a/u/S1: 01-a/u/S1#1 begins thread (generation 1)",
        ]
    );
}
