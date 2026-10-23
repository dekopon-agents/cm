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
        coordinator: Some("session_a".into()),
        cap_min,
    };
    commands::launch_remote(host, &c.root, &target(step), None, &remote).unwrap()
}

fn act(host: &Fake, c: &Campaign, generation: u32, act: Act) -> Outcome {
    attempt::attempt(host, &c.root, &target("01-a/u/S1"), Some(generation), &act).unwrap()
}

fn status(host: &Fake, c: &Campaign) -> Outcome {
    attempt::attempt(host, &c.root, &target("01-a/u/S1"), None, &Act::Status).unwrap()
}

/// The remote runtime: each creation is counted, so a redispatch shows.
#[derive(Default)]
struct Runtime {
    threads: u32,
    turns: u32,
}

/// A coordinator that runs the attempt from committed state, stopping at `kill` boundaries.
fn coordinate(host: &Fake, c: &Campaign, runtime: &mut Runtime, generation: u32, kill: usize) {
    let mut boundary = 0;
    loop {
        let plan = status(host, c);
        let (code, last) = (
            plan.fields["plan"].as_str().unwrap().to_owned(),
            plan.fields["attempt"]["actions"]
                .as_array()
                .and_then(|actions| actions.last().cloned()),
        );
        let kind = last
            .as_ref()
            .map(|last| last["kind"].as_str().unwrap().to_owned());
        let begin = match (code.as_str(), kind.as_deref()) {
            ("dispatch", _) => "thread",
            ("resume", Some("thread")) => "turn",
            ("resume", Some("turn")) | ("unknown" | "ended" | "expired", _) => return,
            other => panic!("{other:?}"),
        };
        if boundary == kill {
            return;
        }
        boundary += 1;
        assert!(act(host, c, generation, Act::Begin { kind: begin.into() }).ok());
        let id = if begin == "thread" {
            runtime.threads += 1;
            format!("thread-{}", runtime.threads)
        } else {
            runtime.turns += 1;
            format!("turn-{}", runtime.turns)
        };
        let recorded = act(
            host,
            c,
            generation,
            Act::Record {
                kind: begin.into(),
                id,
            },
        );
        assert!(recorded.ok(), "{:?}", recorded.violations);
    }
}

#[test]
fn a_replacement_coordinator_resumes_one_execution_from_committed_state() {
    for kill in 0..3 {
        let host = at("2026-10-19T09:00:00Z");
        let c = campaign(&host);
        assert!(declare(&host, &c, "01-a/u/S1", Lane::ClaudeCloud, &[]).ok());
        let launched = launch(&host, &c, "01-a/u/S1", None);
        assert!(launched.ok(), "{:?}", launched.violations);
        assert_eq!(launched.fields["attempt"]["id"], "01-a/u/S1#1");
        let mut runtime = Runtime::default();
        coordinate(&host, &c, &mut runtime, 1, kill);
        let claimed = act(
            &host,
            &c,
            0,
            Act::Claim {
                coordinator: "session_b".into(),
                expect: 1,
            },
        );
        assert!(claimed.ok(), "{:?}", claimed.violations);
        coordinate(&host, &c, &mut runtime, 2, usize::MAX);
        assert_eq!((runtime.threads, runtime.turns), (1, 1), "killed at {kill}");
        assert_eq!(status(&host, &c).fields["plan"], "resume");
    }
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::ClaudeCloud, &[]).ok());
    assert!(launch(&host, &c, "01-a/u/S1", None).ok());
    assert!(
        act(
            &host,
            &c,
            1,
            Act::Begin {
                kind: "thread".into()
            }
        )
        .ok()
    );
    assert_eq!(status(&host, &c).fields["plan"], "unknown");
    assert_eq!(
        rules(&act(
            &host,
            &c,
            1,
            Act::Begin {
                kind: "thread".into()
            }
        )),
        vec!["attempt-unknown"]
    );
    assert!(
        act(
            &host,
            &c,
            1,
            Act::End {
                outcome: AttemptOutcome::Unknown,
                file: None
            }
        )
        .ok()
    );
    assert!(advance(&host, &c, "01-a/u/S1", "blocked:lost", None).ok());
    assert!(launch(&host, &c, "01-a/u/S1", None).ok());
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn two_coordinators_racing_one_claim_fence_the_loser() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::ClaudeCloud, &[]).ok());
    assert!(launch(&host, &c, "01-a/u/S1", None).ok());
    let won: Vec<bool> = std::thread::scope(|scope| {
        let racers: Vec<_> = ["session_b", "session_c"]
            .into_iter()
            .map(|name| {
                let (host, c) = (&host, &c);
                scope.spawn(move || {
                    let claim = Act::Claim {
                        coordinator: name.into(),
                        expect: 1,
                    };
                    act(host, c, 0, claim).ok()
                })
            })
            .collect();
        racers
            .into_iter()
            .map(|racer| racer.join().unwrap())
            .collect()
    });
    assert_eq!(won.iter().filter(|won| **won).count(), 1, "{won:?}");
    for stale in [1, 3] {
        assert_eq!(
            rules(&act(
                &host,
                &c,
                stale,
                Act::Begin {
                    kind: "thread".into()
                }
            )),
            vec!["attempt-fenced"]
        );
    }
    assert!(
        act(
            &host,
            &c,
            2,
            Act::Begin {
                kind: "thread".into()
            }
        )
        .ok()
    );
}

#[test]
fn a_capped_attempt_keeps_its_deadline_across_a_replacement() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::ClaudeCloud, &[]).ok());
    let launched = launch(&host, &c, "01-a/u/S1", Some(30));
    assert_eq!(launched.fields["attempt"]["deadline"], "2026-10-19T09:30Z");
    for (kind, id) in [("thread", "t1"), ("turn", "u1")] {
        assert!(act(&host, &c, 1, Act::Begin { kind: kind.into() }).ok());
        assert!(
            act(
                &host,
                &c,
                1,
                Act::Record {
                    kind: kind.into(),
                    id: id.into()
                }
            )
            .ok()
        );
    }
    let later = at("2026-10-19T09:20:00Z");
    let claim = Act::Claim {
        coordinator: "session_b".into(),
        expect: 1,
    };
    assert!(act(&later, &c, 0, claim).ok());
    assert_eq!(status(&later, &c).fields["plan"], "resume");
    let overrun = at("2026-10-19T09:31:00Z");
    let expired = status(&overrun, &c);
    assert_eq!(expired.fields["plan"], "expired");
    assert!(
        expired.fields["next"]
            .as_str()
            .unwrap()
            .contains("cancel turn u1")
    );
    assert_eq!(
        rules(&act(
            &overrun,
            &c,
            2,
            Act::Begin {
                kind: "turn".into()
            }
        )),
        vec!["attempt-deadline"]
    );
    assert!(
        act(
            &overrun,
            &c,
            2,
            Act::Begin {
                kind: "cancel".into()
            }
        )
        .ok()
    );
    assert!(
        act(
            &overrun,
            &c,
            2,
            Act::Record {
                kind: "cancel".into(),
                id: "u1".into()
            }
        )
        .ok()
    );
    let end = |file| Act::End {
        outcome: AttemptOutcome::Partial,
        file,
    };
    assert_eq!(
        rules(&act(&overrun, &c, 2, end(None))),
        vec!["evidence-missing"]
    );
    let partial = note(&c, "partial.md");
    assert!(act(&overrun, &c, 2, end(Some(partial))).ok());
    let ended = status(&overrun, &c);
    assert_eq!(ended.fields["plan"], "ended");
    assert_eq!(ended.fields["attempt"]["ended"]["outcome"], "partial");
    assert!(lint(&c.root).unwrap().ok());
}
