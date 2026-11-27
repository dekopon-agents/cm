#![cfg(unix)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm::attempt::Act;
use cm::commands::{self, Declaration};
use cm::forge::Gh;
use cm::host::Host;
use cm::outcome::Outcome;
use cm::state::{Evidence, Lane, Phase};
use cm::store::parse_target;
use jiff::Timestamp;
use serde_json::Value;

#[path = "support/campaign.rs"]
mod campaign_fixture;
use campaign_fixture::{Campaign, campaign};

#[path = "support/readers.rs"]
mod readers;

#[allow(dead_code)]
mod support;

impl readers::Twin for Gh {
    fn twin(&self, _: &Path) -> Self {
        Gh {
            program: self.program.clone(),
            timeout: self.timeout,
        }
    }
}

const STEP: &str = "01-a/u/cloud";
const WORKER: &str = "session_worker";
const DRIVER: &str = "session_driver";

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

// Far ahead of the wall clock, so `cm next` run as a process sees no stall.
fn at(minute: u32) -> Fake {
    Fake {
        now: format!("2030-01-01T10:{minute:02}:00Z").parse().unwrap(),
    }
}

// A campaign with one launched claude-cloud step whose worker is on its first turn, and a turn
// reader that answers from files the test rewrites between syncs.
struct Scratch {
    c: Campaign,
    claim: String,
    reader: PathBuf,
    gh: support::FakeGh,
}

impl Scratch {
    fn new() -> Self {
        Self::with_gh("exit 1")
    }

    fn with_gh(gh: &str) -> Self {
        let host = at(0);
        let c = campaign(&host);
        let declaration = Declaration {
            lane: Some(Lane::ClaudeCloud),
            ..Declaration::default()
        };
        assert!(
            commands::declare(&host, &c.root.join(STEP), &declaration)
                .unwrap()
                .ok()
        );
        c.kickoff(STEP, "Go.\n");
        let launched = commands::launch_remote(
            &host,
            &c.root,
            &parse_target(STEP, true).unwrap(),
            None,
            &commands::Remote {
                coordinator: Some("session_current".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(launched.ok(), "{:?}", launched.violations);
        let claim = launched.fields["attempt"]["claim"]
            .as_str()
            .unwrap()
            .to_owned();
        let reader = c._temp.path().join("turn-reader");
        support::write_executable(
            &reader,
            &format!(
                r#"#!/bin/sh
[ "$#" = 2 ] || exit 2
dir='{dir}'
[ -f "$dir/fail" ] && exit 9
tokens=$(cat "$dir/tokens")
case "$1/$2" in
    {WORKER}/turn-1)
        if [ -f "$dir/ended" ]; then
            printf '{{"done":true,"successful":false,"turn_status":"failed","completed_at":"2030-01-01T10:04:00Z","context_used_tokens":%s}}\n' "$tokens"
        else
            printf '{{"done":false,"successful":false,"turn_status":"inProgress","context_used_tokens":%s}}\n' "$tokens"
        fi ;;
    {DRIVER}/)
        printf '{{"thread_id":"{DRIVER}","context_used_tokens":%s}}\n' "$tokens" ;;
    *) exit 2 ;;
esac
"#,
                dir = c._temp.path().display()
            ),
        );
        let scratch = Scratch {
            c,
            claim,
            reader,
            gh: support::FakeGh::new(gh),
        };
        scratch.configure_reader();
        scratch.act(
            &host,
            Act::Begin {
                kind: "session".into(),
            },
        );
        scratch.act(
            &host,
            Act::Record {
                kind: "session".into(),
                id: WORKER.into(),
            },
        );
        scratch.act(
            &host,
            Act::Begin {
                kind: "turn".into(),
            },
        );
        scratch.act(
            &host,
            Act::Record {
                kind: "turn".into(),
                id: "turn-1".into(),
            },
        );
        scratch
    }

    fn limits(&self) -> PathBuf {
        self.c.root.join("LIMITS.toml")
    }

    fn configure_reader(&self) {
        let original = fs::read_to_string(self.limits()).unwrap();
        let argv = serde_json::json!([self.reader, "{thread}", "{turn}"]);
        fs::write(
            self.limits(),
            format!(
                "{original}\nclaude_cloud_turn_reader = {argv}\nclaude_cloud_turn_logs = [\"turn-logs\", \"{{thread}}\", \"{{turn}}\"]\n"
            ),
        )
        .unwrap();
    }

    fn remove_reader(&self) {
        let kept: String = fs::read_to_string(self.limits())
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("claude_cloud_turn_"))
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(self.limits(), kept).unwrap();
    }

    fn reports(&self, tokens: &str) {
        fs::write(self.c._temp.path().join("tokens"), tokens).unwrap();
        let _ = fs::remove_file(self.c._temp.path().join("fail"));
    }

    fn fails(&self) {
        fs::write(self.c._temp.path().join("fail"), "").unwrap();
    }

    fn turn_ends(&self) {
        fs::write(self.c._temp.path().join("ended"), "").unwrap();
    }

    fn try_act(&self, host: &Fake, act: Act) -> Outcome {
        cm::attempt::attempt(
            host,
            &self.c.root,
            &parse_target(STEP, true).unwrap(),
            Some(&self.claim),
            &act,
        )
        .unwrap()
    }

    fn act(&self, host: &Fake, act: Act) {
        let outcome = self.try_act(host, act);
        assert!(outcome.ok(), "{:?}", outcome.violations);
    }

    fn sync(&self, host: &Fake) -> Outcome {
        let forge = Gh {
            program: self.gh.program(),
            timeout: std::time::Duration::from_secs(5),
        };
        let synced = readers::both(host, &forge, &self.c.root).unwrap();
        assert!(synced.ok(), "{:?}", synced.violations);
        synced
    }

    fn cm(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&self.c.root)
            .args(args)
            .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_current")
            .env_remove("CM_CODEX_WEEKLY_PCT")
            .env("PATH", self.gh.path())
            .output()
            .unwrap()
    }

    fn act_lines(&self) -> Vec<String> {
        let out = self.cm(&["next", "--act"]);
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn context_lines(&self) -> Vec<String> {
        self.act_lines()
            .into_iter()
            .filter(|line| line.starts_with("context-over "))
            .collect()
    }

    fn attempt(&self) -> Value {
        let state: Value =
            cm::store::read_json(&cm::store::state_path(&self.c.root.join("01-a/u"))).unwrap();
        state["steps"]["cloud"]["attempt"].clone()
    }

    fn unit_state(&self) -> Vec<u8> {
        fs::read(cm::store::state_path(&self.c.root.join("01-a/u"))).unwrap()
    }
}

fn rules(outcome: &Outcome) -> Vec<&'static str> {
    outcome.violations.iter().map(|v| v.rule).collect()
}

const REMEDY: &str = "start a fresh session from the step's handoff, open findings, head and the diff since the last reviewed head";

#[test]
fn a_worker_session_over_the_cap_is_listed_after_turn_ended_and_its_resume_turn_is_refused() {
    let s = Scratch::new();
    s.reports("250000");
    s.turn_ends();
    s.sync(&at(5));
    assert_eq!(
        s.attempt()["observed_context"],
        serde_json::json!({"session": WORKER, "used_tokens": 250000, "read_at": "2030-01-01T10:05Z"})
    );
    assert_eq!(
        s.attempt()["observed_turn"]["completed_at"],
        "2030-01-01T10:04:00Z"
    );
    let lines = s.act_lines();
    assert_eq!(
        lines,
        [
            "turn-ended 01-a/u/cloud: thread session_worker completed 2030-01-01T10:04:00Z, no PR — `turn-logs session_worker turn-1`".to_owned(),
            format!(
                "context-over 01-a/u/cloud: session_worker at 250k > 200k cap (read 2030-01-01T10:05Z); {REMEDY} — `cm attempt 01-a/u/cloud begin session`"
            ),
        ]
    );
    let json: Value = serde_json::from_slice(&s.cm(&["next", "--act", "--json"]).stdout).unwrap();
    let context = &json["actionable"][1];
    assert_eq!(context["kind"], "context-over");
    assert_eq!(context["target"], STEP);
    assert_eq!(context["since"], "2030-01-01T10:05Z");
    assert_eq!(context["command"], "cm attempt 01-a/u/cloud begin session");

    let recorded = s.attempt()["actions"].clone();
    let resumed = s.try_act(
        &at(6),
        Act::Begin {
            kind: "turn".into(),
        },
    );
    assert_eq!(rules(&resumed), ["attempt-context-over"]);
    assert_eq!(
        resumed.violations[0].message,
        format!(
            "session session_worker is at 250k, over the 200k context cap (read 2030-01-01T10:05Z): never resume it; {REMEDY}: `cm attempt 01-a/u/cloud begin session`, create the session, then `cm attempt 01-a/u/cloud record session '<fresh-session>'`"
        )
    );
    assert_eq!(
        s.attempt()["actions"],
        recorded,
        "a refused begin records nothing"
    );

    s.act(
        &at(7),
        Act::Begin {
            kind: "session".into(),
        },
    );
    let same = s.try_act(
        &at(8),
        Act::Record {
            kind: "session".into(),
            id: WORKER.into(),
        },
    );
    assert_eq!(rules(&same), ["attempt-context-over"]);
    assert!(same.violations[0].message.contains(&format!(
        "never resume it; {REMEDY}, then `cm attempt 01-a/u/cloud record session '<fresh-session>'`"
    )));
    s.act(
        &at(9),
        Act::Record {
            kind: "session".into(),
            id: "session_fresh".into(),
        },
    );
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    s.act(
        &at(10),
        Act::Begin {
            kind: "turn".into(),
        },
    );
}

#[test]
fn a_session_at_or_under_the_cap_is_neither_listed_nor_refused() {
    for tokens in ["150000", "200000"] {
        let s = Scratch::new();
        s.reports(tokens);
        s.sync(&at(5));
        assert_eq!(
            s.attempt()["observed_context"]["used_tokens"],
            tokens.parse::<u64>().unwrap()
        );
        assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
        s.act(
            &at(6),
            Act::Begin {
                kind: "turn".into(),
            },
        );
    }
    let s = Scratch::new();
    s.reports("200001");
    s.sync(&at(5));
    assert_eq!(
        s.context_lines(),
        [format!(
            "context-over 01-a/u/cloud: session_worker at 201k > 200k cap (read 2030-01-01T10:05Z); {REMEDY} — `cm attempt 01-a/u/cloud begin session`"
        )]
    );
}

#[test]
fn a_failed_read_keeps_the_value_stale_so_it_is_listed_but_never_refuses() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    s.fails();
    let failed = s.sync(&at(10));
    assert_eq!(
        failed.fields["turn_reader_warnings"],
        serde_json::json!(["01-a/u/cloud: turn reader exited 9; check access with turn reader"])
    );
    assert_eq!(
        s.attempt()["observed_context"],
        serde_json::json!({"session": WORKER, "used_tokens": 250000, "read_at": "2030-01-01T10:05Z", "unread_since": "2030-01-01T10:10Z"})
    );
    s.sync(&at(15));
    assert_eq!(
        s.attempt()["observed_context"]["unread_since"],
        "2030-01-01T10:10Z",
        "staleness dates from the first failed read"
    );
    assert_eq!(
        s.context_lines(),
        [format!(
            "context-over 01-a/u/cloud: session_worker at 250k > 200k cap (read 2030-01-01T10:05Z; stale: unread since 2030-01-01T10:10Z); {REMEDY} — `cm attempt 01-a/u/cloud begin session`"
        )]
    );
    let resumed = s.try_act(
        &at(16),
        Act::Begin {
            kind: "turn".into(),
        },
    );
    assert!(resumed.ok(), "{:?}", resumed.violations);
    s.act(
        &at(17),
        Act::Record {
            kind: "turn".into(),
            id: "turn-2".into(),
        },
    );

    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    s.fails();
    s.sync(&at(10));
    s.reports("250000");
    s.sync(&at(15));
    assert_eq!(
        s.attempt()["observed_context"],
        serde_json::json!({"session": WORKER, "used_tokens": 250000, "read_at": "2030-01-01T10:15Z"}),
        "a read that delivers again makes the value fresh"
    );
    let refused = s.try_act(
        &at(16),
        Act::Begin {
            kind: "turn".into(),
        },
    );
    assert_eq!(rules(&refused), ["attempt-context-over"]);
}

#[test]
fn a_missing_or_non_integer_value_records_nothing_and_warns_once_per_sync() {
    for tokens in ["null", "\"250000\"", "250000.5", "-1"] {
        let s = Scratch::new();
        s.reports(tokens);
        s.turn_ends();
        let synced = s.sync(&at(5));
        assert_eq!(
            synced.fields["turn_reader_warnings"],
            serde_json::json!([
                "01-a/u/cloud: turn reader reported no integer context_used_tokens for session_worker; its 200k context cap is unchecked"
            ]),
            "{tokens}"
        );
        assert_eq!(s.attempt()["observed_context"], Value::Null, "{tokens}");
        assert_eq!(
            s.attempt()["observed_turn"]["completed_at"],
            "2030-01-01T10:04:00Z",
            "the turn read is unaffected by {tokens}"
        );
        assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    }
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    s.reports("null");
    s.sync(&at(10));
    assert_eq!(
        s.attempt()["observed_context"]["unread_since"],
        "2030-01-01T10:10Z",
        "a missing value keeps the earlier one, stale"
    );
}

#[test]
fn a_sync_that_reads_an_unchanged_value_rewrites_no_state() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    let before = s.unit_state();
    s.sync(&at(10));
    assert_eq!(s.unit_state(), before);
    s.reports("260000");
    s.sync(&at(15));
    assert_eq!(
        s.attempt()["observed_context"]["read_at"],
        "2030-01-01T10:15Z"
    );
}

#[test]
fn with_no_reader_configured_a_recorded_value_is_neither_listed_nor_refused() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    s.remove_reader();
    s.sync(&at(10));
    assert_eq!(s.attempt()["observed_context"]["used_tokens"], 250000);
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    s.act(
        &at(11),
        Act::Begin {
            kind: "turn".into(),
        },
    );
}

#[test]
fn a_fix_round_at_pr_open_is_read_and_its_resume_refused() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, sha) = campaign_fixture::repo(checkout.path());
    let s = Scratch::with_gh(&format!(
        r#"case "$*" in
    'api repos/o/r/pulls/7 --include')
        echo '{{"state":"open","merged":false,"merge_commit_sha":null,"mergeable_state":"blocked","head":{{"sha":"{sha}","ref":"work"}},"base":{{"sha":"{sha}"}}}}' ;;
    *) exit 1 ;;
esac"#
    ));
    s.reports("150000");
    s.sync(&at(5));
    let opened = commands::advance(
        &at(6),
        &s.c.root,
        &parse_target(STEP, true).unwrap(),
        &Phase::PrOpen,
        &Evidence {
            pr: Some("https://github.com/o/r/pull/7".to_owned()),
            sha: Some(sha),
            ..Evidence::default()
        },
        None,
        Some(&repo),
    )
    .unwrap();
    assert!(opened.ok(), "{:?}", opened.violations);
    s.reports("250000");
    s.sync(&at(10));
    assert_eq!(s.attempt()["observed_context"]["used_tokens"], 250000);
    let refused = s.try_act(
        &at(11),
        Act::Begin {
            kind: "turn".into(),
        },
    );
    assert_eq!(rules(&refused), ["attempt-context-over"]);
}

#[test]
fn a_lane_driver_over_the_cap_is_listed_with_its_reregistration_and_kept_off_the_card() {
    let s = Scratch::new();
    s.reports("150000");
    let add = |session: &str| {
        let out = s.cm(&[
            "lane",
            "driver",
            "add",
            "tooling",
            "--session",
            session,
            "--unit",
            "01-a/u",
        ]);
        assert!(out.status.success(), "{out:?}");
    };
    add(DRIVER);
    s.sync(&at(5));
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    s.reports("260000");
    s.sync(&at(10));
    let recorded: Value = cm::store::read_json(&s.c.root.join(".cm/lane-context.json")).unwrap();
    assert_eq!(
        recorded,
        serde_json::json!({"tooling": {"session": DRIVER, "used_tokens": 260000, "read_at": "2030-01-01T10:10Z"}})
    );
    // The worker's attempt is at 260k as well.
    assert_eq!(
        s.context_lines(),
        [
            format!(
                "context-over 01-a/u/cloud: session_worker at 260k > 200k cap (read 2030-01-01T10:10Z); {REMEDY} — `cm attempt 01-a/u/cloud begin session`"
            ),
            "context-over lane:tooling: session_driver at 260k > 200k cap (read 2030-01-01T10:10Z); hand off to a fresh driver session from the lane's handoff — `cm lane driver add tooling --session '<fresh-session>' --unit 01-a/u`".to_owned(),
        ]
    );
    let card = fs::read_to_string(s.c.root.join("CARD.md")).unwrap();
    assert!(
        card.contains("Actionable: 1 (oldest: 2030-01-01T10:10Z)"),
        "the card counts the worker's line, not the driver's: {card}"
    );

    add("session_fresh_driver");
    assert_eq!(s.context_lines().len(), 1, "{:?}", s.act_lines());
    let out = s.cm(&["lane", "driver", "remove", "tooling"]);
    assert!(out.status.success(), "{out:?}");
    s.sync(&at(15));
    let recorded: Value = cm::store::read_json(&s.c.root.join(".cm/lane-context.json")).unwrap();
    assert_eq!(
        recorded,
        serde_json::json!({}),
        "a removed driver's value goes"
    );
}
