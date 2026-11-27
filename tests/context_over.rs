#![cfg(unix)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use cm::attempt::Act;
use cm::commands::{self, Declaration};
use cm::forge::Gh;
use cm::host::Host;
use cm::outcome::Outcome;
use cm::state::{AttemptOutcome, Evidence, Lane, Phase};
use cm::store::parse_target;
use jiff::Timestamp;
use serde_json::{Value, json};

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
        let scratch = Self::session_only(gh);
        scratch.record_turn(&at(0), "turn-1");
        scratch
    }

    fn session_only(gh: &str) -> Self {
        let host = at(0);
        let c = campaign(&host);
        let claim = launch(&host, &c, STEP);
        let reader = c._temp.path().join("turn-reader");
        support::write_executable(
            &reader,
            &format!(
                r#"#!/bin/sh
[ "$#" = 2 ] || exit 2
dir='{dir}'
echo "$1/$2" >> "$dir/runs"
[ -f "$dir/hang" ] && exec sleep 30
[ -f "$dir/hang-$1" ] && exec sleep 30
[ -f "$dir/fail" ] && exit 9
[ -f "$dir/fail-$1" ] && exit 9
tokens=$(cat "$dir/tokens")
[ -f "$dir/tokens-$1" ] && tokens=$(cat "$dir/tokens-$1")
thread=$1
[ -f "$dir/thread-$1" ] && thread=$(cat "$dir/thread-$1")
case "$2" in
    '')
        printf '{{"thread_id":"%s","context_used_tokens":%s}}
' "$thread" "$tokens" ;;
    turn-*)
        if [ -f "$dir/ended" ] || [ -f "$dir/ended-$1" ]; then
            printf '{{"done":true,"successful":false,"turn_status":"failed","completed_at":"2030-01-01T10:04:00Z","context_used_tokens":%s}}
' "$tokens"
        else
            printf '{{"done":false,"successful":false,"turn_status":"inProgress","context_used_tokens":%s}}
' "$tokens"
        fi ;;
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
        scratch.record_session(&host, WORKER);
        scratch
    }

    fn record_session(&self, host: &Fake, id: &str) {
        self.act(
            host,
            Act::Begin {
                kind: "session".into(),
            },
        );
        self.act(
            host,
            Act::Record {
                kind: "session".into(),
                id: id.into(),
            },
        );
    }

    fn record_turn(&self, host: &Fake, id: &str) {
        self.act(
            host,
            Act::Begin {
                kind: "turn".into(),
            },
        );
        self.act(
            host,
            Act::Record {
                kind: "turn".into(),
                id: id.into(),
            },
        );
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

    fn file(&self, name: &str) -> PathBuf {
        self.c._temp.path().join(name)
    }

    fn reports(&self, tokens: impl AsRef<[u8]>) {
        fs::write(self.file("tokens"), tokens).unwrap();
        let _ = fs::remove_file(self.file("fail"));
    }

    fn reports_for(&self, session: &str, tokens: &str) {
        fs::write(self.file(&format!("tokens-{session}")), tokens).unwrap();
    }

    fn fails(&self) {
        fs::write(self.file("fail"), "").unwrap();
    }

    fn turn_ends(&self) {
        fs::write(self.file("ended"), "").unwrap();
    }

    fn runs(&self) -> Vec<String> {
        let runs = fs::read_to_string(self.file("runs")).unwrap_or_default();
        let _ = fs::remove_file(self.file("runs"));
        runs.lines().map(str::to_owned).collect()
    }

    fn add_driver(&self, session: &str) {
        let out = self.cm(&[
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
    }

    fn lane_context(&self) -> Value {
        cm::store::read_json(&self.c.root.join(".cm/lane-context.json")).unwrap()
    }

    fn serial_sync(&self, host: &Fake) -> Outcome {
        let synced = cm::sync::sync(host, &self.forge(), &self.c.root).unwrap();
        assert!(synced.ok(), "{:?}", synced.violations);
        synced
    }

    fn forge(&self) -> Gh {
        Gh {
            program: self.gh.program(),
            timeout: std::time::Duration::from_secs(5),
        }
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
        let synced = readers::both(host, &self.forge(), &self.c.root).unwrap();
        assert!(synced.ok(), "{:?}", synced.violations);
        synced
    }

    fn cm(&self, args: &[&str]) -> std::process::Output {
        self.cm_as("cse_current", args)
    }

    fn cm_as(&self, viewer: &str, args: &[&str]) -> std::process::Output {
        self.command(viewer, args).output().unwrap()
    }

    fn command(&self, viewer: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        command
            .arg("-C")
            .arg(&self.c.root)
            .args(args)
            .env("CLAUDE_CODE_REMOTE_SESSION_ID", viewer)
            .env_remove("CM_CODEX_WEEKLY_PCT")
            .env("PATH", self.gh.path());
        command
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

fn launch(host: &Fake, c: &Campaign, step: &str) -> String {
    let declaration = Declaration {
        lane: Some(Lane::ClaudeCloud),
        ..Declaration::default()
    };
    assert!(
        commands::declare(host, &c.root.join(step), &declaration)
            .unwrap()
            .ok()
    );
    c.kickoff(step, "Go.\n");
    let launched = commands::launch_remote(
        host,
        &c.root,
        &parse_target(step, true).unwrap(),
        None,
        &commands::Remote {
            coordinator: Some("session_current".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(launched.ok(), "{:?}", launched.violations);
    launched.fields["attempt"]["claim"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn launch_with(s: &Scratch, step: &str, acts: impl IntoIterator<Item = Act>) {
    let claim = launch(&at(0), &s.c, step);
    for act in acts {
        let outcome = cm::attempt::attempt(
            &at(0),
            &s.c.root,
            &parse_target(step, true).unwrap(),
            Some(&claim),
            &act,
        )
        .unwrap();
        assert!(outcome.ok(), "{:?}", outcome.violations);
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
    s.add_driver(DRIVER);
    s.reports("250000");
    s.sync(&at(5));
    campaign_fixture::git(&s.c.root, &["add", "-A"]);
    campaign_fixture::git(&s.c.root, &["commit", "-qm", "synced"]);
    let stamps = || -> Vec<_> {
        s.c.files()
            .into_keys()
            .map(|path| {
                let modified = fs::metadata(s.c.root.join(&path)).unwrap().modified();
                (path, modified.unwrap())
            })
            .collect()
    };
    let (before, written) = (s.unit_state(), stamps());
    s.sync(&at(10));
    assert_eq!(s.unit_state(), before);
    assert_eq!(stamps(), written, "a quiet sync rewrites no file");
    assert_eq!(
        campaign_fixture::git(&s.c.root, &["status", "--porcelain"]),
        ""
    );
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
    assert_eq!(
        s.attempt()["observed_context"]["unread_since"],
        "2030-01-01T10:10Z",
        "a sync that reads nothing stales the value"
    );
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    s.record_turn(&at(11), "turn-2");
    s.configure_reader();
    let resumed = s.try_act(&at(12), begin("turn"));
    assert!(
        resumed.ok(),
        "a reader configured again revives no value: {:?}",
        resumed.violations
    );
}

fn open_pr_gh(sha: &str) -> String {
    format!(
        r#"case "$*" in
    'api repos/o/r/pulls/7 --include')
        echo '{{"state":"open","merged":false,"merge_commit_sha":null,"mergeable_state":"blocked","head":{{"sha":"{sha}","ref":"work"}},"base":{{"sha":"{sha}"}}}}' ;;
    *) exit 1 ;;
esac"#
    )
}

fn open_pr(s: &Scratch, host: &Fake, step: &str, repo: &Path, sha: &str) {
    let opened = commands::advance(
        host,
        &s.c.root,
        &parse_target(step, true).unwrap(),
        &Phase::PrOpen,
        &Evidence {
            pr: Some("https://github.com/o/r/pull/7".to_owned()),
            sha: Some(sha.to_owned()),
            ..Evidence::default()
        },
        None,
        Some(repo),
    )
    .unwrap();
    assert!(opened.ok(), "{:?}", opened.violations);
}

#[test]
fn a_fix_round_at_pr_open_is_read_and_its_resume_refused() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, sha) = campaign_fixture::repo(checkout.path());
    let s = Scratch::with_gh(&open_pr_gh(&sha));
    s.reports("150000");
    s.sync(&at(5));
    open_pr(&s, &at(6), STEP, &repo, &sha);
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
    s.fails();
    let failed = s.sync(&at(15));
    assert_eq!(
        failed.fields["turn_reader_warnings"],
        json!(["01-a/u/cloud: turn reader exited 9; check access with turn reader"]),
        "at pr-open no turn read warns first"
    );
    assert_eq!(
        s.attempt()["observed_context"]["unread_since"],
        "2030-01-01T10:15Z"
    );
}

#[test]
fn a_lane_driver_over_the_cap_is_listed_with_its_reregistration_and_kept_off_the_card() {
    let s = Scratch::new();
    s.reports("150000");
    s.add_driver(DRIVER);
    s.sync(&at(5));
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    s.reports("260000");
    s.sync(&at(10));
    assert_eq!(
        s.lane_context(),
        serde_json::json!({"tooling": {"session": DRIVER, "used_tokens": 260000, "read_at": "2030-01-01T10:10Z"}})
    );
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

    s.add_driver("session_fresh_driver");
    assert_eq!(s.context_lines().len(), 1, "{:?}", s.act_lines());
    let out = s.cm(&["lane", "driver", "remove", "tooling"]);
    assert!(out.status.success(), "{out:?}");
    s.sync(&at(15));
    assert_eq!(
        s.lane_context(),
        serde_json::json!({}),
        "a removed driver's value goes"
    );
}

const MISSING: &str = "01-a/u/cloud: turn reader reported no integer context_used_tokens for session_worker; its 200k context cap is unchecked";

fn begin(kind: &str) -> Act {
    Act::Begin { kind: kind.into() }
}

fn record(kind: &str, id: &str) -> Act {
    Act::Record {
        kind: kind.into(),
        id: id.into(),
    }
}

#[cfg(target_os = "linux")]
mod interleaved {
    use std::os::unix::fs::MetadataExt;
    use std::process::Stdio;

    use super::*;

    // A gh whose rate-limit sample, taken after a sync's reads and before its lock, waits while a
    // `hold` file is there, so another sync can start in between.
    const HELD_GH: &str = r#"if [ "$1 $2" = "api rate_limit" ] && [ -f "$FAKE_GH/hold" ]; then
    rm "$FAKE_GH/hold"
    : > "$FAKE_GH/held"
    while [ ! -f "$FAKE_GH/release" ]; do sleep 0.02; done
fi
exit 1"#;

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn waits_on_context_lock(root: &Path) -> bool {
        let inode = fs::metadata(root.join(".cm")).unwrap().ino();
        fs::read_to_string("/proc/locks")
            .unwrap()
            .lines()
            .any(|line| line.contains("-> FLOCK ") && line.contains(&format!(":{inode} ")))
    }

    fn hold(s: &Scratch) {
        fs::write(s.gh.fixture().join("hold"), "").unwrap();
    }

    fn held(s: &Scratch) {
        until("the older sync never reached its rate-limit sample", || {
            s.gh.fixture().join("held").exists()
        });
    }

    fn release(s: &Scratch, mut newer_done: impl FnMut() -> bool) {
        until(
            "the newer sync neither waited on the context lock nor finished",
            || newer_done() || waits_on_context_lock(&s.c.root),
        );
        fs::write(s.gh.fixture().join("release"), "").unwrap();
    }

    fn interleaved(s: &Scratch, older: &Fake, newer: &Fake, between: impl FnOnce()) {
        hold(s);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| s.serial_sync(older));
            held(s);
            between();
            let second = scope.spawn(|| s.serial_sync(newer));
            release(s, || second.is_finished());
            first.join().unwrap();
            second.join().unwrap();
        });
    }

    #[test]
    fn an_older_sync_never_overwrites_a_newer_syncs_value() {
        let s = Scratch::with_gh(HELD_GH);
        s.add_driver(DRIVER);
        s.reports("150000");
        interleaved(&s, &at(5), &at(6), || s.reports("250000"));
        assert_eq!(
            s.attempt()["observed_context"],
            json!({"session": WORKER, "used_tokens": 250000, "read_at": "2030-01-01T10:06Z"})
        );
        assert_eq!(
            s.lane_context(),
            json!({"tooling": {"session": DRIVER, "used_tokens": 250000, "read_at": "2030-01-01T10:06Z"}})
        );
        assert_eq!(s.context_lines().len(), 2, "{:?}", s.act_lines());
        assert_eq!(
            rules(&s.try_act(&at(7), begin("turn"))),
            ["attempt-context-over"]
        );
    }

    #[test]
    fn an_older_failed_read_never_stales_a_newer_read_that_left_the_value_as_it_was() {
        let s = Scratch::with_gh(HELD_GH);
        s.add_driver(DRIVER);
        s.reports("250000");
        s.sync(&at(5));
        s.fails();
        interleaved(&s, &at(6), &at(7), || s.reports("250000"));
        let fresh = |session: &str| json!({"session": session, "used_tokens": 250000, "read_at": "2030-01-01T10:07Z"});
        assert_eq!(s.attempt()["observed_context"], fresh(WORKER));
        assert_eq!(s.lane_context()["tooling"], fresh(DRIVER));
        assert_eq!(
            rules(&s.try_act(&at(8), begin("turn"))),
            ["attempt-context-over"]
        );
    }

    #[test]
    fn an_older_read_never_clears_a_newer_failed_reads_staleness() {
        let s = Scratch::with_gh(HELD_GH);
        s.add_driver(DRIVER);
        s.reports("250000");
        s.sync(&at(5));
        s.fails();
        s.sync(&at(6));
        s.reports("250000");
        interleaved(&s, &at(7), &at(8), || s.fails());
        let stale = |session: &str| json!({"session": session, "used_tokens": 250000, "read_at": "2030-01-01T10:07Z", "unread_since": "2030-01-01T10:08Z"});
        assert_eq!(s.attempt()["observed_context"], stale(WORKER));
        assert_eq!(s.lane_context()["tooling"], stale(DRIVER));
        let resumed = s.try_act(&at(9), begin("turn"));
        assert!(resumed.ok(), "{:?}", resumed.violations);
    }

    #[test]
    fn two_cm_sync_processes_record_their_reads_in_order() {
        let s = Scratch::with_gh(HELD_GH);
        s.add_driver(DRIVER);
        s.reports("250000");
        s.sync(&at(5));
        s.fails();
        let sync = || {
            s.command("cse_current", &["sync"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        hold(&s);
        let older = sync();
        held(&s);
        s.reports("251000");
        let mut newer = sync();
        release(&s, || newer.try_wait().unwrap().is_some());
        for child in [older, newer] {
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{out:?}");
        }
        for context in [
            s.attempt()["observed_context"].clone(),
            s.lane_context()["tooling"].clone(),
        ] {
            assert_eq!(context["used_tokens"], 251000, "{context}");
            assert_eq!(context["unread_since"], Value::Null, "{context}");
        }
        let lines = s.context_lines();
        assert_eq!(lines.len(), 2, "{:?}", s.act_lines());
        assert!(
            lines
                .iter()
                .all(|line| line.contains(" at 251k > 200k cap (read ") && !line.contains("stale")),
            "{lines:?}"
        );
        assert_eq!(
            rules(&s.try_act(&at(8), begin("turn"))),
            ["attempt-context-over"]
        );
    }
}

#[test]
fn a_value_cm_cannot_parse_costs_the_context_record_never_the_turn_read() {
    let deep = format!("{}{}", "[".repeat(130), "]".repeat(130));
    let values = [
        b"1e999".to_vec(),
        "9".repeat(400).into_bytes(),
        br#"{"bad":1e400}"#.to_vec(),
        deep.into_bytes(),
        br#"1,"context_used_tokens":2"#.to_vec(),
        b"\"\xff\"".to_vec(),
    ];
    for tokens in values {
        let shown: String = String::from_utf8_lossy(&tokens).chars().take(16).collect();
        let s = Scratch::new();
        s.reports(&tokens);
        s.turn_ends();
        let synced = s.sync(&at(5));
        assert_eq!(
            synced.fields["turn_reader_warnings"],
            json!([MISSING]),
            "{shown}"
        );
        assert_eq!(s.attempt()["observed_context"], Value::Null, "{shown}");
        assert_eq!(
            s.attempt()["observed_turn"]["completed_at"],
            "2030-01-01T10:04:00Z",
            "{shown}"
        );
        assert!(
            s.act_lines()[0].starts_with("turn-ended 01-a/u/cloud: "),
            "{shown}: {:?}",
            s.act_lines()
        );
    }
}

#[test]
fn a_session_only_attempt_is_read_with_an_empty_turn() {
    let s = Scratch::session_only("exit 1");
    s.reports("250000");
    s.sync(&at(5));
    assert_eq!(
        s.runs(),
        ["session_worker/", "session_worker/"],
        "one reader run a sync"
    );
    assert_eq!(
        s.attempt()["observed_context"],
        json!({"session": WORKER, "used_tokens": 250000, "read_at": "2030-01-01T10:05Z"})
    );
    assert_eq!(
        s.context_lines(),
        [format!(
            "context-over 01-a/u/cloud: session_worker at 250k > 200k cap (read 2030-01-01T10:05Z); {REMEDY} — `cm attempt 01-a/u/cloud begin session`"
        )]
    );
    assert_eq!(
        rules(&s.try_act(&at(6), begin("turn"))),
        ["attempt-context-over"]
    );
    s.act(&at(7), begin("session"));
    assert_eq!(
        rules(&s.try_act(&at(8), record("session", WORKER))),
        ["attempt-context-over"]
    );
    s.act(&at(9), record("session", "session_fresh"));
    s.reports_for("session_fresh", "20000");
    s.sync(&at(10));
    assert_eq!(s.runs(), ["session_fresh/", "session_fresh/"]);
    assert_eq!(
        s.attempt()["observed_context"],
        json!({"session": "session_fresh", "used_tokens": 20000, "read_at": "2030-01-01T10:10Z"}),
        "the fresh session the remedy names is read"
    );
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    s.fails();
    assert_eq!(
        s.sync(&at(11)).fields["turn_reader_warnings"],
        json!(["01-a/u/cloud: turn reader exited 9; check access with turn reader"])
    );
    s.act(&at(12), begin("turn"));
}

#[test]
fn a_hung_reader_costs_a_sync_one_reader_timeout() {
    let s = Scratch::new();
    s.add_driver(DRIVER);
    s.reports("250000");
    fs::write(s.file("hang"), "").unwrap();
    let start = Instant::now();
    let synced = s.serial_sync(&at(5));
    let took = start.elapsed();
    assert!(took < Duration::from_secs(25), "{took:?}");
    assert_eq!(
        synced.fields["turn_reader_warnings"],
        json!([
            "01-a/u/cloud: turn reader did not answer within 15s",
            "lane tooling: turn reader not run: this sync's reader runs passed their shared 15s deadline",
        ])
    );
    assert_eq!(s.runs(), ["session_worker/turn-1"]);
}

#[test]
fn a_read_the_shared_deadline_cuts_says_so_and_the_reads_after_it_are_not_run() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, sha) = campaign_fixture::repo(checkout.path());
    let s = Scratch::with_gh(&open_pr_gh(&sha));
    s.add_driver(DRIVER);
    s.reports("150000");
    launch_with(
        &s,
        "01-a/u/solo",
        [begin("session"), record("session", "session_solo")],
    );
    launch_with(
        &s,
        "01-a/u/zz",
        [
            begin("session"),
            record("session", "session_zz"),
            begin("turn"),
            record("turn", "turn-1"),
        ],
    );
    open_pr(&s, &at(1), "01-a/u/zz", &repo, &sha);
    fs::write(s.file("hang-session_solo"), "").unwrap();
    let start = Instant::now();
    let synced = s.serial_sync(&at(5));
    let took = start.elapsed();
    assert!(took < Duration::from_secs(25), "{took:?}");
    let not_run = "turn reader not run: this sync's reader runs passed their shared 15s deadline";
    assert_eq!(
        synced.fields["turn_reader_warnings"],
        json!([
            "01-a/u/solo: turn reader did not answer by this sync's shared 15s reader deadline",
            format!("01-a/u/zz: {not_run}"),
            format!("lane tooling: {not_run}"),
        ])
    );
    assert_eq!(s.runs(), ["session_worker/turn-1", "session_solo/"]);
}

#[test]
fn a_session_recorded_again_in_its_cse_spelling_keeps_its_value() {
    let s = Scratch::session_only("exit 1");
    s.reports("150000");
    s.sync(&at(5));
    s.record_session(&at(6), "cse_worker");
    let before = s.unit_state();
    s.sync(&at(10));
    assert_eq!(s.unit_state(), before);
    s.fails();
    s.sync(&at(15));
    assert_eq!(
        s.attempt()["observed_context"],
        json!({"session": WORKER, "used_tokens": 150000, "read_at": "2030-01-01T10:05Z", "unread_since": "2030-01-01T10:15Z"})
    );
}

#[test]
fn a_sync_that_cannot_take_the_context_lock_in_time_reads_no_context() {
    let s = Scratch::new();
    s.add_driver(DRIVER);
    s.reports("250000");
    s.sync(&at(5));
    s.runs();
    let held = fs::File::open(s.c.root.join(".cm")).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let synced = s.serial_sync(&at(6));
    let lost = "context not read: another cm sync held the context lock past this sync's shared 15s reader deadline";
    assert_eq!(
        synced.fields["turn_reader_warnings"],
        json!([
            format!("01-a/u/cloud: {lost}"),
            format!("lane tooling: {lost}")
        ])
    );
    assert_eq!(
        s.runs(),
        ["session_worker/turn-1"],
        "its turn read still runs"
    );
    assert_eq!(
        s.attempt()["observed_context"]["unread_since"],
        "2030-01-01T10:06Z"
    );
    assert_eq!(
        s.lane_context()["tooling"]["unread_since"],
        "2030-01-01T10:06Z"
    );
    let resumed = s.try_act(&at(7), begin("turn"));
    assert!(resumed.ok(), "{:?}", resumed.violations);
}

#[test]
fn the_capped_session_is_refused_as_a_thread_and_by_its_cse_label() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    s.act(&at(6), begin("thread"));
    for id in [WORKER, "cse_worker"] {
        let refused = s.try_act(&at(7), record("thread", id));
        assert_eq!(rules(&refused), ["attempt-context-over"], "{id}");
        assert!(
            refused.violations[0].message.ends_with(&format!(
                "never resume it; {REMEDY}, then `cm attempt 01-a/u/cloud record thread '<fresh-session>'`"
            )),
            "{}",
            refused.violations[0].message
        );
    }
    s.act(&at(8), record("thread", "session_fresh"));
    s.act(&at(9), begin("session"));
    assert_eq!(
        rules(&s.try_act(&at(10), record("session", "cse_worker"))),
        ["attempt-context-over"]
    );
}

#[test]
fn context_over_follows_turn_ended_and_the_kinds_before_it_on_its_own_step() {
    let s = Scratch::new();
    launch_with(
        &s,
        "01-a/u/zz",
        [
            begin("session"),
            record("session", "session_zz"),
            begin("turn"),
            record("turn", "turn-1"),
        ],
    );
    s.reports("150000");
    s.reports_for(WORKER, "250000");
    fs::write(s.file("ended-session_zz"), "").unwrap();
    s.sync(&at(5));
    let out = s.cm_as("cse_other", &["next", "--act"]);
    let lines = String::from_utf8(out.stdout).unwrap();
    let kinds: Vec<_> = lines
        .lines()
        .map(|line| line.split(' ').take(2).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        kinds,
        [
            "turn-ended 01-a/u/zz:",
            "earlier-coordinator 01-a/u/cloud:",
            "context-over 01-a/u/cloud:",
            "earlier-coordinator 01-a/u/zz:",
        ],
        "{lines}"
    );
}

#[test]
fn a_driver_read_that_fails_goes_stale_and_warns() {
    let s = Scratch::new();
    s.reports("150000");
    s.add_driver(DRIVER);
    let registry = s.c.root.join(".cm/lane-drivers.json");
    let hand_merged = fs::read_to_string(&registry)
        .unwrap()
        .replace(DRIVER, "cse_driver");
    fs::write(&registry, hand_merged).unwrap();
    s.reports_for(DRIVER, "260000");
    s.sync(&at(5));
    assert_eq!(
        s.runs(),
        [
            "session_worker/turn-1",
            "session_driver/",
            "session_worker/turn-1",
            "session_driver/"
        ],
        "a cse_ registration is read as its session_ label"
    );
    assert_eq!(
        s.lane_context(),
        json!({"tooling": {"session": DRIVER, "used_tokens": 260000, "read_at": "2030-01-01T10:05Z"}})
    );
    let line = |stale: &str| {
        format!(
            "context-over lane:tooling: session_driver at 260k > 200k cap (read {stale}); hand off to a fresh driver session from the lane's handoff — `cm lane driver add tooling --session '<fresh-session>' --unit 01-a/u`"
        )
    };
    assert_eq!(s.context_lines(), [line("2030-01-01T10:05Z")]);

    fs::write(s.file("fail-session_driver"), "").unwrap();
    let failed = s.sync(&at(10));
    assert_eq!(
        failed.fields["turn_reader_warnings"],
        json!(["lane tooling: turn reader exited 9; check access with turn reader"])
    );
    assert_eq!(
        s.context_lines(),
        [line(
            "2030-01-01T10:05Z; stale: unread since 2030-01-01T10:10Z"
        )]
    );
    fs::remove_file(s.file("fail-session_driver")).unwrap();
    s.sync(&at(15));
    assert_eq!(
        s.lane_context()["tooling"],
        json!({"session": DRIVER, "used_tokens": 260000, "read_at": "2030-01-01T10:15Z"})
    );

    s.reports_for(DRIVER, "null");
    let missing = s.sync(&at(20));
    assert_eq!(
        missing.fields["turn_reader_warnings"],
        json!([
            "lane tooling: turn reader reported no integer context_used_tokens for session_driver; its 200k context cap is unchecked"
        ])
    );
    assert_eq!(
        s.lane_context()["tooling"]["unread_since"],
        "2030-01-01T10:20Z"
    );

    s.reports_for(DRIVER, "260000");
    fs::write(s.file("thread-session_driver"), "session_other").unwrap();
    let mismatch = s.sync(&at(25));
    assert_eq!(
        mismatch.fields["turn_reader_warnings"],
        json!(["lane tooling: turn reader returned JSON cm cannot read"])
    );
    assert_eq!(
        s.lane_context()["tooling"]["unread_since"],
        "2030-01-01T10:20Z"
    );

    fs::remove_file(s.file("thread-session_driver")).unwrap();
    s.sync(&at(30));
    assert_eq!(s.lane_context()["tooling"]["unread_since"], Value::Null);
    s.runs();
    s.remove_reader();
    s.sync(&at(35));
    assert!(s.runs().is_empty());
    assert_eq!(
        s.lane_context()["tooling"]["unread_since"],
        "2030-01-01T10:35Z",
        "a sync that reads no driver stales its value"
    );
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
}

#[test]
fn a_restart_clears_the_value() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    s.act(
        &at(6),
        Act::End {
            outcome: AttemptOutcome::Unknown,
            file: None,
        },
    );
    s.act(
        &at(7),
        Act::Restart {
            lane: Lane::ClaudeCloud,
            file: None,
        },
    );
    assert_eq!(s.attempt()["observed_context"], Value::Null);
}

#[test]
fn a_newer_sessions_read_replaces_the_earlier_sessions_value() {
    let s = Scratch::session_only("exit 1");
    s.reports("250000");
    s.sync(&at(5));
    s.record_session(&at(6), "session_second");
    s.reports_for("session_second", "255000");
    s.sync(&at(10));
    assert_eq!(
        s.attempt()["observed_context"],
        json!({"session": "session_second", "used_tokens": 255000, "read_at": "2030-01-01T10:10Z"}),
        "within 10k of the earlier session's value, but another session's"
    );
    assert_eq!(
        s.context_lines(),
        [format!(
            "context-over 01-a/u/cloud: session_second at 255k > 200k cap (read 2030-01-01T10:10Z); {REMEDY} — `cm attempt 01-a/u/cloud begin session`"
        )]
    );
    s.record_session(&at(11), "session_third");
    fs::write(s.file("fail-session_third"), "").unwrap();
    s.sync(&at(15));
    assert_eq!(
        s.attempt()["observed_context"],
        Value::Null,
        "a failed read of a newer session drops the earlier session's value"
    );
}

#[test]
fn a_value_moves_in_state_only_by_10k_or_across_the_cap() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    let lane_context = s.c.root.join(".cm/lane-context.json");
    assert!(!lane_context.exists(), "no driver, no lane-context file");
    s.add_driver(DRIVER);
    s.sync(&at(6));
    let (state, lanes) = (s.unit_state(), fs::read(&lane_context).unwrap());
    for tokens in ["259999", "240001"] {
        s.reports(tokens);
        s.sync(&at(10));
        assert_eq!(s.unit_state(), state, "{tokens}");
        assert_eq!(fs::read(&lane_context).unwrap(), lanes, "{tokens}");
    }
    let recorded = |tokens: &str, minute: u32| {
        s.reports(tokens);
        s.sync(&at(minute));
        let value = |context: &Value| {
            (
                context["used_tokens"].as_u64().unwrap(),
                context["read_at"].as_str().unwrap().to_owned(),
            )
        };
        let attempt = value(&s.attempt()["observed_context"]);
        assert_eq!(value(&s.lane_context()["tooling"]), attempt, "{tokens}");
        attempt
    };
    let stamp = |minute: u32| format!("2030-01-01T10:{minute:02}Z");
    assert_eq!(recorded("260000", 15), (260000, stamp(15)));
    assert_eq!(recorded("201000", 20), (201000, stamp(20)));
    assert_eq!(
        recorded("199500", 25),
        (199500, stamp(25)),
        "across the cap"
    );
    assert_eq!(recorded("200000", 30), (199500, stamp(25)));
    assert_eq!(
        recorded("200001", 35),
        (200001, stamp(35)),
        "across the cap"
    );
}

#[test]
fn a_value_no_sync_reads_goes_stale_and_never_refuses() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    let path = cm::store::state_path(&s.c.root.join("01-a/u"));
    let mut state: Value = cm::store::read_json(&path).unwrap();
    state["steps"]["cloud"]["state"] = json!("superseded");
    cm::store::write_json(&path, &state).unwrap();
    let resumed = s.try_act(&at(6), begin("turn"));
    assert!(resumed.ok(), "{:?}", resumed.violations);
    s.sync(&at(10));
    assert_eq!(
        s.attempt()["observed_context"]["unread_since"],
        "2030-01-01T10:10Z"
    );
}

#[test]
fn a_sync_while_a_fresh_session_is_created_keeps_reading_the_capped_one() {
    let s = Scratch::new();
    s.reports("250000");
    s.sync(&at(5));
    s.runs();
    s.act(&at(6), begin("session"));
    assert!(s.context_lines().is_empty(), "{:?}", s.act_lines());
    s.sync(&at(10));
    assert_eq!(s.runs(), ["session_worker/", "session_worker/"]);
    assert_eq!(
        s.attempt()["observed_context"],
        json!({"session": WORKER, "used_tokens": 250000, "read_at": "2030-01-01T10:05Z"})
    );
    assert_eq!(
        rules(&s.try_act(&at(11), record("session", WORKER))),
        ["attempt-context-over"]
    );
    s.act(&at(12), record("session", "session_fresh"));
}

#[test]
fn a_stale_value_is_listed_for_its_session_under_either_label() {
    let s = Scratch::session_only("exit 1");
    s.record_session(&at(1), "cse_worker");
    s.reports("250000");
    s.sync(&at(5));
    s.fails();
    s.sync(&at(10));
    s.record_session(&at(11), WORKER);
    assert_eq!(
        s.context_lines(),
        [format!(
            "context-over 01-a/u/cloud: cse_worker at 250k > 200k cap (read 2030-01-01T10:05Z; stale: unread since 2030-01-01T10:10Z); {REMEDY} — `cm attempt 01-a/u/cloud begin session`"
        )]
    );
}
