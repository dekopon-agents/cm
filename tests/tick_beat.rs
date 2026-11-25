use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cm::tick::TickError;
use cm::tick::act::{ActLine, Ctx, Env, Executor, Outcome, Registry, Report};
use cm::tick::beat::{self, Beat, Phases};
use cm::tick::gitsync::Mode;
use cm::tick::proc::{
    Cmd, Exit, FakeProc, KILL_AFTER, Keep, OUTPUT_CAP, Proc, Ran, Spilled, StdoutKeep, SystemProc,
    WHOLE_CAP,
};
use serde_json::{Value, json};

const EPOCH: &str = "store/epochs/eleemosynary-mnemosyne";
const PICK_OK: &str =
    "ok store/epochs/eleemosynary-mnemosyne/handoff/coordinator-2026-11-24T2030Z.md\n";
const PRINTED: [&str; 2] = [
    "launchable-not-launched 01-a/u/b: cm launch 01-a/u/b",
    "attempt-ended 01-a/u/c: cm attempt 01-a/u/c restart",
];
const FRAME: [&str; 3] = [
    "git rev-parse --abbrev-ref HEAD",
    "handoff-pick.sh store/epochs/eleemosynary-mnemosyne coordinator --lineage session_01abc --max-age-hours 24",
    "cm --json next -C store/epochs/eleemosynary-mnemosyne --act",
];
const BOUND_CHECK: [&str; 4] = [
    "git rev-parse --abbrev-ref HEAD",
    "sh tools/cloud-env/handoff-pick.sh store/epochs/eleemosynary-mnemosyne coordinator --lineage session_01abc --max-age-hours 24",
    "git fetch -q origin main",
    "git diff --name-only -z --no-renames --diff-filter=A HEAD origin/main -- store/epochs/eleemosynary-mnemosyne/handoff/",
];
const QUEUE_LINE: &str = "owner-queue: 1 unread item(s); read OWNER-QUEUE.md and run cm owner-queue read before any merge or review";

fn lines() -> Vec<Value> {
    vec![
        json!({"kind": "launchable-not-launched", "target": "01-a/u/b", "since": "2026-11-25T01:00Z", "command": "cm launch 01-a/u/b"}),
        json!({"kind": "attempt-ended", "target": "01-a/u/c", "since": "2026-11-25T01:00Z", "command": "cm attempt 01-a/u/c restart"}),
    ]
}

fn line(kind: &str, target: &str, command: &str) -> Value {
    json!({"kind": kind, "target": target, "since": "2026-11-25T01:00Z", "command": command})
}

fn act(actionable: Vec<Value>) -> String {
    json!({"ok": true, "violations": [], "actionable": actionable}).to_string()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn script(path: &Path, body: &str) -> PathBuf {
    write(path, &format!("#!/bin/sh\n{body}"));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_owned()
}

struct Ct(tempfile::TempDir);

impl Ct {
    fn new() -> Self {
        let ct = Ct(tempfile::tempdir().unwrap());
        ct.write(
            &format!("{EPOCH}/EPOCH.md"),
            "---\nslug: eleemosynary-mnemosyne\nstatus: WORKING\n---\n",
        );
        ct.write("store/epochs/old/EPOCH.md", "---\nstatus: CLOSED\n---\n");
        ct
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, relative: &str, text: &str) {
        write(&self.path().join(relative), text);
    }

    fn state(&self) {
        self.write(
            &format!("{EPOCH}/01-a/u/.cm/state.json"),
            &json!({"steps": {"b": {"lane": "codex"}, "c": {"lane": "claude-cloud"}, "d": {}}})
                .to_string(),
        );
    }
}

#[derive(Clone)]
struct Answers {
    next: Ran,
    branch: String,
    pick: Ran,
    fetch: Ran,
    added: Ran,
    archive: Ran,
    extract: Ran,
    listing: Ran,
    origin_pick: Ran,
}

impl Answers {
    fn new(exit: i32, stdout: impl Into<Vec<u8>>) -> Self {
        Answers {
            next: Ran::code(exit, stdout),
            branch: "main\n".to_owned(),
            pick: Ran::code(0, PICK_OK),
            fetch: Ran::code(0, ""),
            added: Ran::code(0, ""),
            archive: Ran::code(0, ""),
            extract: Ran::code(0, ""),
            listing: Ran::code(0, ""),
            origin_pick: Ran::code(0, PICK_OK),
        }
    }

    fn answer(&self, cmd: &Cmd) -> Ran {
        match (
            cmd.program_name().as_str(),
            cmd.argv.get(1).and_then(|arg| arg.to_str()),
        ) {
            ("cm", Some("--json")) => self.next.clone(),
            ("git", Some("rev-parse")) => Ran::code(0, self.branch.clone()),
            ("git", Some("fetch")) => self.fetch.clone(),
            ("git", Some("diff")) => self.added.clone(),
            ("git", Some("archive")) => self.archive.clone(),
            ("tar", _) => self.extract.clone(),
            ("git", Some("ls-tree")) => self.listing.clone(),
            ("sh", Some(beat::HANDOFF_PICK)) if Path::new(&cmd.argv[2]).is_absolute() => {
                self.origin_pick.clone()
            }
            ("sh", Some(beat::HANDOFF_PICK)) => self.pick.clone(),
            _ => Ran::code(0, ""),
        }
    }

    fn proc(self) -> FakeProc {
        FakeProc::new(move |cmd| Ok(self.answer(cmd)))
    }
}

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs.iter().copied().collect()
}

fn bound_env() -> Env {
    env(&[("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")])
}

fn quiet(_: &Ctx) -> Report {
    Report::default()
}

fn quiet_gitsync(_: &Ctx, _: Mode) -> Report {
    Report::default()
}

const QUIET: Phases<'static> = Phases {
    gitsync: &quiet_gitsync,
    pin: &quiet,
    directive: &quiet,
    collect: &quiet,
    claude_md: &quiet,
};

fn tick(ct: &Ct, proc: &dyn Proc, env: &Env, registry: &Registry) -> Vec<String> {
    tick_with(ct, proc, env, registry, &QUIET)
}

fn frame_calls<'a>(log: &'a [String], wanted: &[&str]) -> Vec<&'a str> {
    // A filled phase may make the same call (gitsync reads the branch too): keep the first.
    let mut seen = Vec::new();
    for call in log.iter().map(String::as_str) {
        if wanted.contains(&call) && !seen.contains(&call) {
            seen.push(call);
        }
    }
    seen
}

fn tick_with(
    ct: &Ct,
    proc: &dyn Proc,
    env: &Env,
    registry: &Registry,
    phases: &Phases,
) -> Vec<String> {
    let mut out = Vec::new();
    let beat = Beat {
        checkout: ct.path(),
        proc,
        env,
        registry,
        phases,
        cm: OsStr::new("cm"),
        started: Instant::now(),
    };
    beat::run(&beat, &mut |line| out.push(line));
    out
}

#[derive(Debug, Clone)]
struct Call {
    json: Value,
    lane: Option<String>,
    stdin: Vec<u8>,
    epoch: PathBuf,
    remaining: Duration,
}

#[derive(Clone, Default)]
struct Seen(Rc<RefCell<Vec<Call>>>);

impl Seen {
    fn calls(&self) -> Vec<Call> {
        self.0.borrow().clone()
    }
}

struct Record {
    seen: Seen,
    outcome: Outcome,
}

impl Record {
    fn new(outcome: Outcome) -> (Self, Seen) {
        let seen = Seen::default();
        (
            Record {
                seen: seen.clone(),
                outcome,
            },
            seen,
        )
    }
}

impl Executor for Record {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome {
        self.seen.0.borrow_mut().push(Call {
            json: line.json.clone(),
            lane: line.lane.clone(),
            stdin: line.stdin(),
            epoch: ctx.epoch.to_owned(),
            remaining: ctx.remaining(),
        });
        self.outcome.clone()
    }
}

fn done() -> Outcome {
    Outcome::Done(Vec::new())
}

fn is_wall_line(line: &str, phase: &str) -> bool {
    let Some(rest) = line.strip_prefix(&format!("tick: {phase} phase took ")) else {
        return false;
    };
    let Some((wall, budget)) = rest.split_once(" s (budget ") else {
        return false;
    };
    wall.parse::<f64>().is_ok()
        && budget
            .strip_suffix(" s)")
            .is_some_and(|secs| secs.parse::<u64>().is_ok())
}

#[test]
fn nothing_to_do_prints_nothing() {
    let ct = Ct::new();
    let proc = Answers::new(0, act(Vec::new())).proc();
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &Registry::new()),
        Vec::<String>::new()
    );
    assert_eq!(
        proc.log(),
        [
            BOUND_CHECK.as_slice(),
            &BOUND_CHECK,
            &["cm --json next -C store/epochs/eleemosynary-mnemosyne --act"],
        ]
        .concat(),
        "the bound check, the re-check after gitsync, then the act list; no `cm sync`"
    );
    for call in proc.calls() {
        assert_eq!(call.cwd.as_deref(), Some(ct.path()));
        assert_eq!(call.timeout, beat::FRAME_CAP);
        assert_eq!(call.stdin, None);
    }
}

#[test]
fn actionable_lines_are_printed() {
    let ct = Ct::new();
    let proc = Answers::new(1, act(lines())).proc();
    assert_eq!(tick(&ct, &proc, &bound_env(), &Registry::new()), PRINTED);
}

#[test]
fn executors_run_by_kind_and_lane_and_failures_keep_the_line() {
    let ct = Ct::new();
    ct.state();
    let mut actionable = lines();
    actionable.push(line("unblock-now", "01-a/u/d", "cm unblock 01-a/u/d"));
    let proc = Answers::new(1, act(actionable)).proc();
    let (codex, codex_seen) = Record::new(done());
    let (ended, ended_seen) = Record::new(Outcome::Failed("first\nno thread recorded\n".into()));
    let (unblock, unblock_seen) = Record::new(Outcome::Done(vec!["must not run".into()]));
    let registry = Registry::new()
        .with("launchable-not-launched.codex", codex)
        .with("attempt-ended", ended)
        .with("unblock-now.codex", unblock);
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &registry),
        [
            format!(
                "{} [tick/attempt-ended failed: no thread recorded]",
                PRINTED[1]
            ),
            "unblock-now 01-a/u/d: cm unblock 01-a/u/d".to_owned(),
        ]
    );
    let calls = codex_seen.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].json, lines()[0]);
    assert_eq!(calls[0].json["target"], "01-a/u/b");
    assert_eq!(calls[0].epoch, Path::new(EPOCH));
    assert_eq!(calls[0].lane.as_deref(), Some("codex"));
    assert_eq!(
        calls[0].stdin,
        format!("{}\n", lines()[0]).into_bytes(),
        "the line's JSON, compact, as heartbeat.sh fed stdin"
    );
    assert_eq!(ended_seen.calls()[0].lane.as_deref(), Some("claude-cloud"));
    assert!(unblock_seen.calls().is_empty());
}

#[test]
fn a_line_names_its_own_lane_before_the_state_does() {
    let ct = Ct::new();
    ct.state();
    let mut own = line("launchable-not-launched", "01-a/u/b", "cm launch 01-a/u/b");
    own["lane"] = json!("claude-cloud");
    let mut empty = line("launchable-not-launched", "01-a/u/b", "cm launch 01-a/u/b");
    empty["lane"] = json!("");
    let mut part = line("attempt-ended", "01-a/u/b", "cm attempt 01-a/u/b");
    part["part"] = json!("api");
    let proc = Answers::new(1, act(vec![own, empty, part])).proc();
    let (cloud, cloud_seen) = Record::new(done());
    let (codex, codex_seen) = Record::new(done());
    let (ended, ended_seen) = Record::new(Outcome::Failed("no".into()));
    let registry = Registry::new()
        .with("launchable-not-launched.claude-cloud", cloud)
        .with("launchable-not-launched.codex", codex)
        .with("attempt-ended.codex", ended);
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &registry),
        ["attempt-ended 01-a/u/b#api: cm attempt 01-a/u/b [tick/attempt-ended.codex failed: no]"]
    );
    assert_eq!(cloud_seen.calls().len(), 1, "the line's own lane");
    assert_eq!(
        codex_seen.calls().len(),
        1,
        "an empty lane falls back to state"
    );
    assert_eq!(ended_seen.calls()[0].lane.as_deref(), Some("codex"));
}

#[test]
fn an_executor_prints_what_is_left_for_the_model_and_its_writes_are_pushed() {
    let ct = Ct::new();
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let answers = Answers::new(1, act(vec![lines().remove(0)]));
    let proc = {
        let log = log.clone();
        FakeProc::new(move |cmd| {
            if cmd.program_name() == "cm" {
                log.borrow_mut().push("act".to_owned());
            }
            Ok(answers.answer(cmd))
        })
    };
    let (executor, seen) = Record::new(Outcome::Done(vec!["start the Claude review now".into()]));
    let registry = Registry::new().with("launchable-not-launched", executor);
    let phase = |name: &'static str| {
        let log = log.clone();
        move |ctx: &Ctx| {
            assert_eq!(ctx.epoch, Path::new(EPOCH));
            assert_eq!(ctx.slug(), "eleemosynary-mnemosyne");
            log.borrow_mut().push(name.to_owned());
            Report::default()
        }
    };
    let gitsync = {
        let log = log.clone();
        move |_: &Ctx, mode: Mode| {
            log.borrow_mut().push(format!("gitsync {mode:?}"));
            Report::default()
        }
    };
    let (pin, directive, collect, claude_md) = (
        phase("pin"),
        phase("directive"),
        phase("collect"),
        phase("claude-md"),
    );
    let phases = Phases {
        gitsync: &gitsync,
        pin: &pin,
        directive: &directive,
        collect: &collect,
        claude_md: &claude_md,
    };
    assert_eq!(
        tick_with(&ct, &proc, &bound_env(), &registry, &phases),
        ["start the Claude review now"]
    );
    assert_eq!(seen.calls().len(), 1);
    assert_eq!(
        *log.borrow(),
        [
            "gitsync Always",
            "pin",
            "directive",
            "collect",
            "act",
            "gitsync IfDirty",
            "claude-md"
        ]
    );

    log.borrow_mut().clear();
    let proc = {
        let log = log.clone();
        let answers = Answers::new(1, act(lines()));
        FakeProc::new(move |cmd| {
            if cmd.program_name() == "cm" {
                log.borrow_mut().push("act".to_owned());
            }
            Ok(answers.answer(cmd))
        })
    };
    assert_eq!(
        tick_with(&ct, &proc, &bound_env(), &Registry::new(), &phases),
        PRINTED
    );
    assert_eq!(
        *log.borrow(),
        [
            "gitsync Always",
            "pin",
            "directive",
            "collect",
            "act",
            "claude-md"
        ],
        "no executor ran, so nothing to push"
    );

    log.borrow_mut().clear();
    let proc = {
        let log = log.clone();
        let answers = Answers {
            pick: Ran::code(4, "unbound x\n"),
            ..Answers::new(1, act(lines()))
        };
        FakeProc::new(move |cmd| {
            log.borrow_mut().push(cmd.program_name());
            Ok(answers.answer(cmd))
        })
    };
    assert_eq!(
        tick_with(&ct, &proc, &bound_env(), &Registry::new(), &phases),
        Vec::<String>::new()
    );
    assert_eq!(
        *log.borrow(),
        ["git", "sh"],
        "the bound check runs first; an unbound session runs no phase and no cm"
    );
}

#[test]
fn a_phase_that_fails_or_whose_child_times_out_prints_its_wall_time() {
    let ct = Ct::new();
    let answers = Answers::new(0, act(Vec::new()));
    let proc = FakeProc::new(move |cmd| {
        Ok(if cmd.program_name() == "slow" {
            Ran::timed_out()
        } else {
            answers.answer(cmd)
        })
    });
    let quiet = |_: &Ctx| Report::default();
    let gitsync = |_: &Ctx, _: Mode| Report::default();
    let pin = |_: &Ctx| {
        Report {
        lines: vec!["heartbeat: cm 9.9.9 (setup.sh's pin) could not be fetched or installed; still running cm 0.16.0".into()],
        failed: true,
    }
    };
    let directive = |ctx: &Ctx| {
        let ran = ctx.run(Cmd::new("slow")).unwrap();
        assert_eq!(ran.exit, Exit::TimedOut);
        assert!(ctx.timed_out());
        Report::default()
    };
    let phases = Phases {
        gitsync: &gitsync,
        pin: &pin,
        directive: &directive,
        collect: &quiet,
        claude_md: &quiet,
    };
    let out = tick_with(&ct, &proc, &bound_env(), &Registry::new(), &phases);
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(
        out[0],
        "heartbeat: cm 9.9.9 (setup.sh's pin) could not be fetched or installed; still running cm 0.16.0"
    );
    assert!(is_wall_line(&out[1], "pin"), "{out:?}");
    assert!(
        out[1].ends_with(&format!("(budget {} s)", cm::tick::pin::BUDGET.as_secs())),
        "{out:?}"
    );
    assert!(is_wall_line(&out[2], "directive"), "{out:?}");
    let slow = proc
        .calls()
        .into_iter()
        .find(|cmd| cmd.program_name() == "slow")
        .unwrap();
    assert_eq!(slow.cwd.as_deref(), Some(ct.path()));
    assert!(slow.timeout <= cm::tick::directive::BUDGET);
}

#[test]
fn unread_owner_queue_holds_the_merge_and_review_executors() {
    let ct = Ct::new();
    let next = json!({
        "ok": true, "violations": [], "owner_queue_unread": ["## 31. x"],
        "actionable": [
            line("merge-ready", "01-a/u/b", "merge it"),
            line("review-owed", "01-a/u/c", "cm brief 01-a/u/c"),
            line("launchable-not-launched", "01-a/u/e", "cm launch 01-a/u/e"),
        ]
    });
    let proc = Answers::new(1, next.to_string()).proc();
    let (merge, merge_seen) = Record::new(done());
    let (review, review_seen) = Record::new(done());
    let (launch, launch_seen) = Record::new(done());
    let registry = Registry::new()
        .with("merge-ready", merge)
        .with("review-owed", review)
        .with("launchable-not-launched", launch);
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &registry),
        [
            QUEUE_LINE,
            "merge-ready 01-a/u/b: merge it",
            "review-owed 01-a/u/c: cm brief 01-a/u/c"
        ]
    );
    assert!(merge_seen.calls().is_empty());
    assert!(review_seen.calls().is_empty());
    assert_eq!(launch_seen.calls().len(), 1, "other kinds still run");

    let proc = Answers::new(1, next.to_string()).proc();
    assert_eq!(
        tick(&ct, &proc, &Env::default(), &Registry::new()),
        Vec::<String>::new(),
        "printed only in the bound coordinator"
    );
    assert!(!proc.log().iter().any(|call| call.starts_with("cm ")));
}

#[test]
fn spent_time_budget_prints_the_line_unrun() {
    let ct = Ct::new();
    for deadline in ["0", "-5"] {
        let proc = Answers::new(1, act(vec![lines().remove(0)])).proc();
        let (executor, seen) = Record::new(done());
        let registry = Registry::new().with("launchable-not-launched", executor);
        let env = env(&[
            ("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc"),
            ("HEARTBEAT_DEADLINE", deadline),
        ]);
        assert_eq!(
            tick(&ct, &proc, &env, &registry),
            [format!(
                "{} [not run: heartbeat time budget spent]",
                PRINTED[0]
            )]
        );
        assert!(seen.calls().is_empty());
        assert!(
            proc.log()
                .contains(&format!("cm --json next -C {EPOCH} --act"))
        );
    }
}

#[test]
fn an_executor_gets_120_seconds_or_what_is_left_of_the_deadline() {
    let ct = Ct::new();
    for (deadline, most, least, warned) in [
        (None, 120, 119, false),
        (Some("500"), 120, 119, false),
        (Some(""), 120, 119, false),
        (Some("50"), 50, 49, false),
        (Some("soon"), 120, 119, true),
    ] {
        let proc = Answers::new(1, act(vec![lines().remove(0)])).proc();
        let (executor, seen) = Record::new(done());
        let registry = Registry::new().with("launchable-not-launched", executor);
        let mut pairs = vec![("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")];
        pairs.extend(deadline.map(|value| ("HEARTBEAT_DEADLINE", value)));
        let out = tick(&ct, &proc, &env(&pairs), &registry);
        if warned {
            assert_eq!(
                out,
                ["heartbeat: HEARTBEAT_DEADLINE=soon is not a whole number of seconds; using 230"]
            );
        } else {
            assert!(out.is_empty(), "{out:?}");
        }
        let remaining = seen.calls()[0].remaining;
        assert!(
            remaining <= Duration::from_secs(most) && remaining > Duration::from_secs(least),
            "{deadline:?}: {remaining:?}"
        );
    }
}

#[test]
fn executors_run_only_in_the_bound_coordinator() {
    let ct = Ct::new();
    let run = |answers: Answers, env: &Env| {
        let proc = answers.proc();
        let (executor, seen) = Record::new(done());
        let registry = Registry::new().with("launchable-not-launched", executor);
        let out = tick(&ct, &proc, env, &registry);
        (out, seen.calls().len(), proc)
    };
    let result = |answers: Answers, env: &Env| {
        let (out, ran, _) = run(answers, env);
        (out, ran)
    };
    let next = || Answers::new(1, act(vec![lines().remove(0)]));
    let acts = || (Vec::<String>::new(), 1);
    let silent = || (Vec::<String>::new(), 0);

    let (out, ran, proc) = run(next(), &bound_env());
    assert_eq!((out, ran), acts());
    let calls = proc.calls();
    let by = |program: &str| {
        calls
            .iter()
            .find(|call| call.program_name() == program)
            .unwrap()
    };
    assert_eq!(by("git").line(), "git rev-parse --abbrev-ref HEAD");
    assert_eq!(
        by("sh").argv,
        [
            "sh",
            beat::HANDOFF_PICK,
            EPOCH,
            "coordinator",
            "--lineage",
            "session_01abc",
            "--max-age-hours",
            "24"
        ]
    );
    for call in calls.iter().filter(|call| call.program_name() != "cm") {
        assert_eq!(call.cwd.as_deref(), Some(ct.path()), "{}", call.line());
        assert_eq!(call.timeout, beat::FRAME_CAP, "{}", call.line());
    }

    let picked = |pick: &str| Answers {
        pick: Ran::code(0, pick),
        ..next()
    };
    // Age never unbinds: handoff-pick's `stale` (exit 6) binds as `ok` does.
    let stale = "stale store/epochs/eleemosynary-mnemosyne/handoff/coordinator-2026-11-01T0000Z.md lineage=session_01abc written=2026-11-01T00:00:00+00:00\n";
    for answers in [
        picked("ok a\nnot read\n"),
        Answers {
            pick: Ran::code(6, stale),
            ..next()
        },
    ] {
        assert_eq!(result(answers, &bound_env()), acts());
    }
    let (out, ran, proc) = run(next(), &env(&[("CLAUDE_CODE_REMOTE_SESSION_ID", "01abc")]));
    assert_eq!((out, ran), acts());
    assert!(proc.log().contains(&BOUND_CHECK[1].to_owned()));

    // A fetch or a diff that fails counts as bound, as heartbeat.sh's `|| return 0` and its
    // empty `added` did.
    let failed = [Ran::code(128, ""), Ran::timed_out()];
    for ran in failed.clone() {
        let (out, ran, proc) = run(
            Answers {
                fetch: ran,
                ..next()
            },
            &bound_env(),
        );
        assert_eq!((out, ran), acts());
        assert!(!proc.log().iter().any(|call| call.starts_with("git diff")));
    }
    for ran in failed {
        let (out, ran, proc) = run(
            Answers {
                added: ran,
                ..next()
            },
            &bound_env(),
        );
        assert_eq!((out, ran), acts());
        assert!(
            !proc
                .log()
                .iter()
                .any(|call| call.starts_with("git archive"))
        );
    }

    let handoff = "store/epochs/eleemosynary-mnemosyne/handoff";
    let added = |names: &str| Answers {
        added: Ran::code(0, names),
        ..next()
    };
    let read_origin = |proc: &FakeProc| {
        proc.log()
            .iter()
            .filter(|call| call.starts_with("git archive") || call.starts_with("tar "))
            .count()
    };
    let (out, ran, proc) = run(
        added(&format!(
            "{handoff}/pr-loop-2026-11-26T0100Z.md\0{handoff}/coordinator-notes.txt\0{handoff}/old/coordinator\0{handoff}/xcoordinator-1.md\0{handoff}/coordinator-1.mdx\0"
        )),
        &bound_env(),
    );
    assert_eq!((out, ran), acts(), "no coordinator-*.md was added");
    assert_eq!(read_origin(&proc), 0);
    for (origin, binds) in [
        (Ran::code(0, PICK_OK), true),
        (Ran::code(6, stale), true),
        (
            Ran::code(
                3,
                "missing other-lineage x/coordinator-2026-11-26T0100Z.md\n",
            ),
            false,
        ),
    ] {
        let mut answers = added(&format!("{handoff}/coordinator-2026-11-26T0100Z.md\0"));
        answers.origin_pick = origin;
        let (out, ran, proc) = run(answers, &bound_env());
        assert_eq!((out, ran), if binds { acts() } else { silent() });
        assert_eq!(
            read_origin(&proc),
            if binds { 6 } else { 2 },
            "origin's own handoff folder decides"
        );
    }
    let (out, ran, proc) = run(
        added(&format!("{handoff}/old/coordinator-1.md\0")),
        &bound_env(),
    );
    assert_eq!((out, ran), acts());
    assert_eq!(
        read_origin(&proc),
        6,
        "a coordinator-*.md in a subfolder counts, as heartbeat.sh's /coordinator-[^/]*\\.md$ did"
    );
    // heartbeat.sh's `case` never read the status; only a failed pick's is refused.
    let (out, ran, _) = run(
        Answers {
            pick: Ran::code(0, stale),
            ..next()
        },
        &bound_env(),
    );
    assert_eq!((out, ran), acts(), "stale with exit 0");
    let mut truncated = added(&format!("{handoff}/pr-loop-a.md\0{handoff}/pr-lo"));
    truncated.added.stdout_truncated = true;
    let (out, ran, proc) = run(truncated, &bound_env());
    assert_eq!((out, ran), acts());
    assert_eq!(
        read_origin(&proc),
        6,
        "a list cut at its head may hide a coordinator handoff"
    );

    for pick in [
        "refused superseded store/epochs/eleemosynary-mnemosyne/handoff/x.md\n",
        "refused malformed store/epochs/eleemosynary-mnemosyne/handoff/coordinator-bad.md\n",
        "missing other-lineage store/epochs/eleemosynary-mnemosyne/handoff/x.md\n",
        "unbound store/epochs/eleemosynary-mnemosyne/handoff/coordinator-old.md\n",
        "ok\n",
        "okay\nok second line\n",
        "",
    ] {
        assert_eq!(
            result(
                Answers {
                    pick: Ran::code(1, pick),
                    ..next()
                },
                &bound_env()
            ),
            silent(),
            "{pick:?}"
        );
    }
    let mut late = picked(PICK_OK);
    late.pick.exit = Exit::TimedOut;
    assert_eq!(
        result(late, &bound_env()),
        silent(),
        "a pick that timed out"
    );
    for session in [None, Some("")] {
        let env: Env = session
            .map(|id| ("CLAUDE_CODE_REMOTE_SESSION_ID", id))
            .into_iter()
            .collect();
        assert_eq!(result(next(), &env), silent());
    }
    let off_main = Answers {
        branch: "feature\n".into(),
        ..next()
    };
    assert_eq!(result(off_main, &bound_env()), silent());

    let unstartable = FakeProc::new(|cmd| match cmd.program_name().as_str() {
        "cm" => Ok(Ran::code(1, act(vec![lines().remove(0)]))),
        "git" => Ok(Ran::code(0, "main\n")),
        program => Err(TickError::Spawn {
            program: program.to_owned(),
            source: std::io::ErrorKind::NotFound.into(),
        }),
    });
    assert_eq!(
        tick(&ct, &unstartable, &bound_env(), &Registry::new()),
        Vec::<String>::new()
    );
}

#[test]
fn what_an_executor_returns_is_printed_as_heartbeat_sh_printed_its_output() {
    let ct = Ct::new();
    let long = "x".repeat(400);
    for (outcome, printed) in [
        (
            Outcome::Done(vec![
                "a".into(),
                "".into(),
                "b".into(),
                "".into(),
                "".into(),
            ]),
            vec!["a".to_owned(), "".to_owned(), "b".to_owned()],
        ),
        (Outcome::Done(vec!["".into(), "".into()]), vec![]),
        (
            Outcome::Done(vec!["a\n".into(), "b".into()]),
            vec!["a".into(), "".into(), "b".into()],
        ),
        (
            Outcome::Done(vec!["two\nlines".into()]),
            vec!["two".into(), "lines".into()],
        ),
        (
            Outcome::Left("left as it is".into()),
            vec!["left as it is".into()],
        ),
        (
            Outcome::Failed(String::new()),
            vec![format!(
                "{} [tick/launchable-not-launched failed: ]",
                PRINTED[0]
            )],
        ),
        (
            Outcome::Failed(format!("first\n{long}\n\n")),
            vec![format!(
                "{} [tick/launchable-not-launched failed: {}]",
                PRINTED[0],
                &long[..300]
            )],
        ),
        (
            Outcome::Failed(format!("x{}", "é".repeat(200))),
            vec![format!(
                "{} [tick/launchable-not-launched failed: x{}]",
                PRINTED[0],
                "é".repeat(149)
            )],
        ),
    ] {
        let proc = Answers::new(1, act(vec![lines().remove(0)])).proc();
        let (executor, _) = Record::new(outcome.clone());
        let registry = Registry::new().with("launchable-not-launched", executor);
        assert_eq!(
            tick(&ct, &proc, &bound_env(), &registry),
            printed,
            "{outcome:?}"
        );
    }
}

struct PanicsOnOneLine;

impl Executor for PanicsOnOneLine {
    fn run(&self, _: &ActLine, _: &Ctx) -> Outcome {
        panic!("executor bug");
    }
}

#[test]
fn a_one_line_executor_panic_keeps_its_panicked_prefix() {
    let ct = Ct::new();
    let proc = Answers::new(1, act(vec![lines().remove(0)])).proc();
    let registry = Registry::new().with("launchable-not-launched", PanicsOnOneLine);
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &registry),
        [format!(
            "{} [tick/launchable-not-launched failed: panicked: executor bug]",
            PRINTED[0]
        )]
    );
}

struct Panics;

impl Executor for Panics {
    fn run(&self, _: &ActLine, _: &Ctx) -> Outcome {
        panic!("executor bug\nsecond line");
    }
}

#[test]
fn a_panicking_executor_or_phase_fails_its_line_and_the_tick_goes_on() {
    let ct = Ct::new();
    let proc = Answers::new(1, act(lines())).proc();
    let (ended, ended_seen) = Record::new(Outcome::Done(vec!["after".into()]));
    let registry = Registry::new()
        .with("launchable-not-launched", Panics)
        .with("attempt-ended", ended);
    let quiet = |_: &Ctx| Report::default();
    let gitsync = |_: &Ctx, _: Mode| Report::default();
    let pin = |_: &Ctx| -> Report { panic!("pin bug") };
    let phases = Phases {
        gitsync: &gitsync,
        pin: &pin,
        directive: &quiet,
        collect: &quiet,
        claude_md: &quiet,
    };
    let out = tick_with(&ct, &proc, &bound_env(), &registry, &phases);
    assert_eq!(out.len(), 4, "{out:?}");
    assert_eq!(out[0], "tick: pin phase panicked: pin bug");
    assert!(is_wall_line(&out[1], "pin"), "{out:?}");
    assert_eq!(
        out[2],
        format!(
            "{} [tick/launchable-not-launched failed: second line]",
            PRINTED[0]
        )
    );
    assert_eq!(out[3], "after");
    assert_eq!(ended_seen.calls().len(), 1);
}

struct Spawns(Cmd);

impl Executor for Spawns {
    fn run(&self, _: &ActLine, ctx: &Ctx) -> Outcome {
        let ran = ctx.run(self.0.clone());
        Outcome::Done(vec![format!("child: {:?}", ran.map(|ran| ran.exit))])
    }
}

#[test]
fn a_child_killed_at_the_budget_makes_the_timeout_line_whatever_the_executor_returns() {
    let ct = Ct::new();
    let answers = Answers::new(1, act(vec![lines().remove(0)]));
    let proc = FakeProc::new(move |cmd| {
        Ok(if cmd.program_name() == "slow" {
            Ran::timed_out()
        } else {
            answers.answer(cmd)
        })
    });
    let registry = Registry::new().with(
        "launchable-not-launched",
        Spawns(Cmd::new("slow").stdin("x")),
    );
    let env = env(&[
        ("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc"),
        ("HEARTBEAT_DEADLINE", "50"),
    ]);
    let out = tick(&ct, &proc, &env, &registry);
    assert_eq!(
        out,
        [format!(
            "{} [tick/launchable-not-launched timed out after 50 s and was killed; check what it started]",
            PRINTED[0]
        )]
    );
    let slow = proc
        .calls()
        .into_iter()
        .find(|cmd| cmd.program_name() == "slow")
        .unwrap();
    assert_eq!(slow.cwd.as_deref(), Some(ct.path()));
    assert!(slow.timeout <= Duration::from_secs(50) && slow.timeout > Duration::from_secs(49));
}

#[test]
fn cm_failing_without_a_list_is_one_line() {
    let ct = Ct::new();
    let failing = |ran: Ran| {
        let proc = Answers {
            next: ran,
            ..Answers::new(0, "")
        }
        .proc();
        tick(&ct, &proc, &bound_env(), &Registry::new())
    };
    // Exactly the one line test_heartbeat.py asserts: no wall-time line after it.
    let failed = |ran: Ran, line: &str| assert_eq!(failing(ran), [line]);
    failed(
        Ran::code(2, "error: no campaign\n"),
        "heartbeat: cm --json next --act exited 2 without an actionable list: ",
    );
    failed(
        Ran::code(2, "").with_stderr("boom\nmore\n"),
        "heartbeat: cm --json next --act exited 2 without an actionable list: boom",
    );
    failed(
        Ran::code(2, "").with_stderr("e".repeat(400)),
        &format!(
            "heartbeat: cm --json next --act exited 2 without an actionable list: {}",
            "e".repeat(300)
        ),
    );
    failed(
        Ran::code(2, act(lines())),
        "heartbeat: cm --json next --act exited 2 without an actionable list: ",
    );
    failed(
        Ran::code(0, "{\"actionable\": {}}"),
        "heartbeat: cm --json next --act exited 0 without an actionable list: ",
    );
    failed(
        Ran::code(1, "not json"),
        "heartbeat: cm --json next --act exited 1 without an actionable list: ",
    );
    failed(
        Ran {
            exit: Exit::Signal(9),
            ..Ran::code(0, "")
        },
        "heartbeat: cm --json next --act exited 137 without an actionable list: ",
    );
    failed(
        Ran::timed_out(),
        "heartbeat: cm --json next --act ran past 30 s and was killed",
    );
    assert_eq!(failing(Ran::code(0, "")), Vec::<String>::new());
    assert_eq!(failing(Ran::code(0, "\n\n")), Vec::<String>::new());

    let answers = Answers::new(0, "");
    let proc = FakeProc::new(move |cmd| match cmd.program_name().as_str() {
        "cm" => Err(TickError::Spawn {
            program: cmd.program_name(),
            source: std::io::ErrorKind::NotFound.into(),
        }),
        _ => Ok(answers.answer(cmd)),
    });
    let out = tick(&ct, &proc, &bound_env(), &Registry::new());
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(
        out[0].starts_with("heartbeat: cm --json next --act could not run: cm: "),
        "{out:?}"
    );
}

#[test]
fn more_than_one_live_epoch_stops_before_any_effect() {
    let ct = Ct::new();
    ct.write(
        "store/epochs/second/EPOCH.md",
        "---\nstatus: PLANNING\n---\n",
    );
    let proc = Answers::new(0, act(lines())).proc();
    let called = RefCell::new(false);
    let phase = |_: &Ctx| {
        *called.borrow_mut() = true;
        Report::default()
    };
    let gitsync = |_: &Ctx, _: Mode| {
        *called.borrow_mut() = true;
        Report::default()
    };
    let phases = Phases {
        gitsync: &gitsync,
        pin: &phase,
        directive: &phase,
        collect: &phase,
        claude_md: &phase,
    };
    assert_eq!(
        tick_with(&ct, &proc, &bound_env(), &Registry::new(), &phases),
        [
            "heartbeat: 2 PLANNING or WORKING epochs, not one; stay read-only until cm, PR and journal state are reconciled"
        ]
    );
    assert!(proc.log().is_empty());
    assert!(!*called.borrow());
}

#[test]
fn the_live_epoch_is_one_with_a_line_starting_status_planning_or_working() {
    let ct = Ct::new();
    assert_eq!(beat::live_epochs(ct.path()).unwrap(), [Path::new(EPOCH)]);
    ct.write("store/epochs/.hidden/EPOCH.md", "status: WORKING\n");
    ct.write("store/epochs/indented/EPOCH.md", "  status: WORKING\n");
    ct.write("store/epochs/other/EPOCH.md", "status: OPEN\n");
    fs::create_dir_all(ct.path().join("store/epochs/folder/EPOCH.md")).unwrap();
    assert_eq!(beat::live_epochs(ct.path()).unwrap(), [Path::new(EPOCH)]);
    ct.write(
        "store/epochs/later/EPOCH.md",
        "---\ntitle: x\n---\nstatus: PLANNING-ish\r\n",
    );
    assert_eq!(
        beat::live_epochs(ct.path()).unwrap(),
        [Path::new(EPOCH), Path::new("store/epochs/later")]
    );

    ct.write(&format!("{EPOCH}/EPOCH.md"), "status: CLOSED\n");
    fs::remove_file(ct.path().join("store/epochs/later/EPOCH.md")).unwrap();
    let proc = Answers::new(0, act(lines())).proc();
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &Registry::new()),
        [
            "heartbeat: 0 PLANNING or WORKING epochs, not one; stay read-only until cm, PR and journal state are reconciled"
        ]
    );
    let empty = tempfile::tempdir().unwrap();
    assert!(beat::live_epochs(empty.path()).unwrap().is_empty());
}

struct Bin(PathBuf);

impl Proc for Bin {
    fn run(&self, cmd: &Cmd) -> cm::tick::Result<Ran> {
        let mut cmd = cmd.clone();
        let fake = self.0.join(&cmd.argv[0]);
        if !cmd.argv[0].to_string_lossy().contains('/') && fake.is_file() {
            cmd.argv[0] = fake.into_os_string();
        }
        spawn(&cmd)
    }
}

fn spawn(cmd: &Cmd) -> cm::tick::Result<Ran> {
    for _ in 0..100 {
        match SystemProc.run(cmd) {
            Err(TickError::Spawn { source, .. })
                if source.kind() == std::io::ErrorKind::ExecutableFileBusy =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => return other,
        }
    }
    SystemProc.run(cmd)
}

fn gone(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z')),
    }
}

fn gone_within(pid: &str, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    gone(pid)
}

fn read_pid(path: &Path) -> String {
    let start = Instant::now();
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && text.ends_with('\n')
        {
            return text.trim().to_owned();
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "no pid in {path:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_timed_out_executor_is_killed_with_its_process_group() {
    let ct = Ct::new();
    let bin = ct.path().join("bin");
    let next = ct.path().join("next.json");
    fs::write(&next, act(vec![lines().remove(0)])).unwrap();
    let cm = script(
        &bin.join("cm"),
        &format!("cat '{}'\nexit 1\n", next.display()),
    );
    script(&bin.join("git"), "echo main\n");
    script(
        &ct.path().join(beat::HANDOFF_PICK),
        &format!("printf '{PICK_OK}'\n"),
    );
    let pidfile = ct.path().join("child.pid");
    let sleeper = script(
        &bin.join("sleeper"),
        "sleep 60 &\necho $! > \"$1\"\nsleep 60\n",
    );
    let registry = Registry::new().with(
        "launchable-not-launched",
        Spawns(Cmd::new(&sleeper).arg(&pidfile)),
    );
    let env = env(&[
        ("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc"),
        ("HEARTBEAT_DEADLINE", "3"),
    ]);
    let mut out = Vec::new();
    let started = Instant::now();
    beat::run(
        &Beat {
            checkout: ct.path(),
            proc: &Bin(bin.clone()),
            env: &env,
            registry: &registry,
            phases: &QUIET,
            cm: cm.as_os_str(),
            started,
        },
        &mut |line| out.push(line),
    );
    let took = started.elapsed();
    assert_eq!(out.len(), 1, "{out:?}");
    let rest = out[0]
        .strip_prefix(&format!(
            "{} [tick/launchable-not-launched timed out after ",
            PRINTED[0]
        ))
        .and_then(|rest| rest.strip_suffix(" s and was killed; check what it started]"))
        .unwrap_or_else(|| panic!("{out:?}"));
    assert!(matches!(rest, "1" | "2" | "3"), "{out:?}");
    assert!(took < Duration::from_secs(3 + 5 + 2), "{took:?}");
    let pid = read_pid(&pidfile);
    assert!(
        gone_within(&pid, Duration::from_secs(5)),
        "child {pid} survived"
    );
}

#[test]
fn the_real_proc_feeds_stdin_and_reads_stdout_and_stderr_apart_or_merged() {
    let dir = tempfile::tempdir().unwrap();
    let echo = script(
        &dir.path().join("echo"),
        "read line\necho \"out $line $X $(pwd)\"\necho \"err $1\" >&2\necho after\nexit 3\n",
    );
    let cmd = Cmd::new(&echo)
        .arg("arg")
        .stdin("in\n")
        .env("X", "set")
        .cwd(dir.path());
    let ran = spawn(&cmd).unwrap();
    assert_eq!(ran.exit, Exit::Code(3));
    assert_eq!(
        ran.stdout_text(),
        format!("out in set {}\nafter\n", dir.path().display())
    );
    assert_eq!(ran.stderr_text(), "err arg\n");
    let ran = spawn(&cmd.clone().merge_stderr()).unwrap();
    assert_eq!(
        ran.stdout_text(),
        format!("out in set {}\nerr arg\nafter\n", dir.path().display())
    );
    assert!(ran.stderr.is_empty());

    let count = script(&dir.path().join("count"), "wc -c\n");
    let big = vec![b'x'; 1 << 20];
    let ran = spawn(&Cmd::new(&count).stdin(big.clone())).unwrap();
    assert_eq!(ran.stdout_text().trim(), "1048576");
    let deaf = script(&dir.path().join("deaf"), "exit 0\n");
    let ran = spawn(&Cmd::new(&deaf).stdin(big)).unwrap();
    assert_eq!(ran.exit, Exit::Code(0), "a child that never reads stdin");
    let null = script(&dir.path().join("null"), "cat\n");
    let ran = spawn(&Cmd::new(&null)).unwrap();
    assert_eq!((ran.exit, ran.stdout), (Exit::Code(0), Vec::new()));

    let killed = script(&dir.path().join("killed"), "kill -9 $$\n");
    assert_eq!(spawn(&Cmd::new(&killed)).unwrap().exit, Exit::Signal(9));
    match spawn(&Cmd::new(dir.path().join("missing"))) {
        Err(TickError::Spawn { program, .. }) => assert!(program.ends_with("missing")),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        spawn(&Cmd {
            argv: Vec::new(),
            ..Cmd::new("x")
        }),
        Err(TickError::Spawn { .. })
    ));
}

#[test]
fn a_child_that_ignores_term_is_killed_after_the_grace() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let stubborn = script(
        &dir.path().join("stubborn"),
        "trap '' TERM\nsleep 60 &\necho $! > \"$1\"\nwhile :; do sleep 1; done\n",
    );
    let ran = spawn(
        &Cmd::new(&stubborn)
            .arg(&pidfile)
            .timeout(Duration::from_secs(1)),
    )
    .unwrap();
    assert_eq!(ran.exit, Exit::TimedOut);
    assert!(
        ran.wall >= Duration::from_secs(6) && ran.wall < Duration::from_secs(9),
        "{:?}",
        ran.wall
    );
    let pid = read_pid(&pidfile);
    assert!(
        gone_within(&pid, Duration::from_secs(2)),
        "child {pid} survived"
    );
}

#[test]
fn a_child_flooding_its_output_is_still_stopped_at_its_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let flood = script(&dir.path().join("flood"), "exec yes\n");
    for keep in [Keep::Tail, Keep::Head] {
        let ran = spawn(
            &Cmd::new(&flood)
                .timeout(Duration::from_secs(1))
                .keep(keep, Keep::Tail),
        )
        .unwrap();
        assert_eq!(ran.exit, Exit::TimedOut, "{keep:?}");
        assert!(ran.wall < Duration::from_secs(4), "{:?}", ran.wall);
        assert!(!ran.stdout.is_empty() && ran.stdout.len() <= OUTPUT_CAP);
    }
}

#[test]
fn a_stream_kept_whole_past_its_cap_kills_the_child_as_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let flood = script(&dir.path().join("flood"), "exec yes\n");
    let ran = spawn(&Cmd::new(&flood).timeout(Duration::from_secs(30))).unwrap();
    assert_eq!(ran.exit, Exit::TooLarge);
    assert_eq!(ran.stdout.len(), WHOLE_CAP);
    assert!(ran.wall < Duration::from_secs(20), "{:?}", ran.wall);
    let merged = spawn(
        &Cmd::new(&flood)
            .timeout(Duration::from_secs(30))
            .merge_stderr(),
    )
    .unwrap();
    assert_eq!(merged.exit, Exit::TooLarge);
}

#[test]
fn head_keeps_the_first_bytes_and_tail_the_last() {
    let dir = tempfile::tempdir().unwrap();
    let long = script(
        &dir.path().join("long"),
        "echo first\nhead -c 6000000 /dev/zero | tr '\\0' x\necho\necho last\necho err-first >&2\nhead -c 6000000 /dev/zero | tr '\\0' y >&2\necho >&2\necho err-last >&2\n",
    );
    let ran = spawn(&Cmd::new(&long).keep(Keep::Head, Keep::Tail)).unwrap();
    assert_eq!(ran.exit, Exit::Code(0));
    assert_eq!(ran.stdout.len(), OUTPUT_CAP);
    assert!(ran.stdout.starts_with(b"first\nxxx"));
    assert_eq!(ran.stderr.len(), OUTPUT_CAP);
    assert!(ran.stderr.ends_with(b"\nerr-last\n"));
    let ran = spawn(&Cmd::new(&long).keep(Keep::Tail, Keep::Head)).unwrap();
    assert!(ran.stdout.ends_with(b"\nlast\n"));
    assert!(ran.stderr.starts_with(b"err-first\nyyy"));
    let ran = spawn(&Cmd::new(&long)).unwrap();
    assert_eq!(
        ran.stdout.len(),
        6_000_000 + "first\n\nlast\n".len(),
        "whole by default"
    );
}

#[test]
fn the_act_list_spills_past_64_mib_and_the_frame_children_keep_their_heads() {
    let ct = Ct::new();
    let proc = Answers::new(1, act(lines())).proc();
    assert_eq!(tick(&ct, &proc, &bound_env(), &Registry::new()), PRINTED);
    let next = proc
        .calls()
        .into_iter()
        .find(|cmd| cmd.program_name() == "cm")
        .unwrap();
    assert_eq!(
        (next.keep_stdout, next.keep_stderr),
        (StdoutKeep::Spill, Keep::Head)
    );
    for frame in proc.calls().iter().filter(|cmd| cmd.program_name() != "cm") {
        assert_eq!(frame.keep_stdout, Keep::Head, "{}", frame.line());
    }
}

#[test]
fn a_spilled_act_list_is_read_from_its_file() {
    let ct = Ct::new();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(act(lines()).as_bytes()).unwrap();
    let mut answers = Answers::new(1, "");
    answers.next.stdout_spill = Some(Spilled::Stored(Arc::new(file)));
    assert_eq!(
        tick(&ct, &answers.proc(), &bound_env(), &Registry::new()),
        PRINTED
    );
}

#[test]
fn a_list_that_could_not_be_spilled_is_one_line_naming_the_store() {
    let ct = Ct::new();
    let error = std::io::Error::from(std::io::ErrorKind::StorageFull);
    let named = format!(
        "heartbeat: cm --json next --act's list was too large to keep in memory and could not be stored in {}: {error}; no act line was read",
        std::env::temp_dir().display()
    );
    let mut answers = Answers::new(1, "");
    answers.next.exit = Exit::TooLarge;
    answers.next.stdout_spill = Some(Spilled::Failed(Arc::new(error)));
    let (executor, seen) = Record::new(done());
    let registry = Registry::new().with("launchable-not-launched", executor);
    assert_eq!(tick(&ct, &answers.proc(), &bound_env(), &registry), [named]);
    assert!(seen.calls().is_empty());
}

#[test]
fn a_list_the_acting_pass_could_not_read_is_never_acted_on() {
    let merge = line("merge-ready", "01-a/u/b", "cm merge 01-a/u/b");
    let review = line("review-owed", "01-a/u/c", "cm brief 01-a/u/c");
    let deep = format!("{}{}", "[".repeat(130), "]".repeat(130));
    let unreadable: [(&str, Vec<u8>); 4] = [
        ("a number out of range", br#"{"n": 1e999}"#.to_vec()),
        ("nesting past 128", deep.into_bytes()),
        ("invalid UTF-8", b"{\"n\": \"\xff\"}".to_vec()),
        ("a lone surrogate", br#"{"n": "\ud800"}"#.to_vec()),
    ];
    for (what, middle) in unreadable {
        let mut list = format!(r#"{{"ok": true, "actionable": [{merge}, "#).into_bytes();
        list.extend_from_slice(&middle);
        list.extend_from_slice(format!(", {review}]}}").as_bytes());
        let ct = Ct::new();
        let (merging, merged) = Record::new(done());
        let (reviewing, reviewed) = Record::new(done());
        let registry = Registry::new()
            .with("merge-ready", merging)
            .with("review-owed", reviewing);
        assert_eq!(
            tick(&ct, &Answers::new(0, list).proc(), &bound_env(), &registry),
            ["heartbeat: cm --json next --act exited 0 without an actionable list: "],
            "{what}"
        );
        assert!(merged.calls().is_empty(), "{what}: {:?}", merged.calls());
        assert!(
            reviewed.calls().is_empty(),
            "{what}: {:?}",
            reviewed.calls()
        );
    }
}

#[test]
fn a_list_cut_short_prints_no_act_line() {
    let ct = Ct::new();
    let whole = act(lines());
    let proc = Answers::new(1, &whole.as_bytes()[..whole.len() - 3]).proc();
    let (executor, seen) = Record::new(done());
    let registry = Registry::new().with("launchable-not-launched", executor);
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &registry),
        ["heartbeat: cm --json next --act exited 1 without an actionable list: "]
    );
    assert!(
        seen.calls().is_empty(),
        "nothing ran before the list was whole"
    );
}

#[test]
fn a_valid_act_list_above_64_mib_prints_every_line() {
    let ct = Ct::new();
    let bin = ct.path().join("bin");
    let count = BIG_LIST;
    let cm = script(&bin.join("cm"), &big_list());
    bound_scripts(&ct, &bin);
    let probe = Command::new(&cm).output().unwrap();
    assert!(probe.stdout.len() > 64 << 20, "{}", probe.stdout.len());
    drop(probe);
    let mut out = Vec::new();
    beat::run(
        &Beat {
            checkout: ct.path(),
            proc: &Bin(bin.clone()),
            env: &bound_env(),
            registry: &Registry::new(),
            phases: &QUIET,
            cm: cm.as_os_str(),
            started: Instant::now(),
        },
        &mut |line| out.push(line),
    );
    assert_eq!(out.len(), count, "{:?}", out.first());
    for (n, line) in out.iter().enumerate() {
        assert_eq!(*line, big_line(n));
    }
}

fn bound_scripts(ct: &Ct, bin: &Path) {
    script(
        &bin.join("git"),
        "case $1 in rev-parse) echo main ;; esac\nexit 0\n",
    );
    script(
        &ct.path().join(beat::HANDOFF_PICK),
        &format!("printf '{PICK_OK}'\n"),
    );
}

const BIG_LIST: usize = 70_000;

// The command printing a valid list of BIG_LIST lines, more than 64 MiB, generated as it is
// printed.
fn big_list_awk() -> String {
    format!(
        "awk 'BEGIN {{ printf \"{{\\\"ok\\\":true,\\\"actionable\\\":[\"; for (i = 0; i < {BIG_LIST}; i++) {{ if (i) printf \",\"; printf \"{{\\\"kind\\\":\\\"stale-block\\\",\\\"target\\\":\\\"01-a/u/s%07d\\\",\\\"command\\\":\\\"cm sync %01000d\\\"}}\", i, i }} printf \"]}}\" }}'"
    )
}

fn big_list() -> String {
    format!("{}\nexit 1\n", big_list_awk())
}

// The same list as big_list_awk, written ahead so a test's producer only copies it.
fn write_big_list(path: &Path) {
    let mut file = std::io::BufWriter::new(fs::File::create(path).unwrap());
    write!(file, r#"{{"ok":true,"actionable":["#).unwrap();
    for n in 0..BIG_LIST {
        if n > 0 {
            write!(file, ",").unwrap();
        }
        write!(
            file,
            r#"{{"kind":"stale-block","target":"01-a/u/s{n:07}","command":"cm sync {n:01000}"}}"#
        )
        .unwrap();
    }
    write!(file, "]}}").unwrap();
    file.flush().unwrap();
}

fn big_line(n: usize) -> String {
    format!("stale-block 01-a/u/s{n:07}: cm sync {n:01000}")
}

#[test]
fn a_descendant_holding_stdout_open_runs_until_the_timeout_then_goes_with_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let leaves = script(&dir.path().join("leaves"), "sleep 60 &\necho $!\nexit 0\n");
    let started = Instant::now();
    let ran = spawn(&Cmd::new(&leaves).timeout(Duration::from_secs(1))).unwrap();
    assert_eq!(ran.exit, Exit::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(4));
    let pid = ran.stdout_text().trim().to_owned();
    assert!(
        gone_within(&pid, Duration::from_secs(2)),
        "child {pid} survived"
    );
}

#[test]
fn a_list_a_descendant_prints_after_the_leader_and_stderr_are_gone_is_read_whole() {
    for big in [false, true] {
        let ct = Ct::new();
        let bin = ct.path().join("bin");
        let list = ct.path().join("next.json");
        if big {
            write_big_list(&list);
        } else {
            fs::write(&list, act(lines())).unwrap();
        }
        let print = format!("cat '{}'", list.display());
        let cm = script(
            &bin.join("cm"),
            &format!("exec 2>&-\n( sleep 1; {print} ) &\nexit 0\n"),
        );
        bound_scripts(&ct, &bin);
        let mut out = Vec::new();
        beat::run(
            &Beat {
                checkout: ct.path(),
                proc: &Bin(bin.clone()),
                env: &bound_env(),
                registry: &Registry::new(),
                phases: &QUIET,
                cm: cm.as_os_str(),
                started: Instant::now(),
            },
            &mut |line| out.push(line),
        );
        if big {
            assert_eq!(out.len(), BIG_LIST, "{:?}", out.first());
            for (n, line) in out.iter().enumerate() {
                assert_eq!(*line, big_line(n));
            }
        } else {
            assert_eq!(out, PRINTED);
        }
    }
}

#[test]
fn a_spilling_capture_times_out_and_kills_a_descendant_that_outlives_the_leader() {
    for big in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let print = if big {
            let list = dir.path().join("big.json");
            write_big_list(&list);
            format!("cat '{}'", list.display())
        } else {
            "echo {}".to_owned()
        };
        let leaves = |name: &str, then: &str| {
            let pid_file = dir.path().join(format!("{name}.pid"));
            let leaves = script(
                &dir.path().join(name),
                &format!(
                    "exec 2>&-\n( {print}{then} ) &\necho $! > '{}'\nexit 0\n",
                    pid_file.display()
                ),
            );
            (
                Cmd::new(leaves).keep(StdoutKeep::Spill, Keep::Head),
                pid_file,
            )
        };
        // The capture sets the pace, not the producer, so the timeout is a multiple of how long
        // the same capture takes here and now, whatever else the machine is running.
        let (quick, _) = leaves("quick", "");
        let started = Instant::now();
        let ran = spawn(&quick.timeout(Duration::from_secs(3600))).unwrap();
        let capture = started.elapsed();
        assert_eq!(ran.exit, Exit::Code(0), "big: {big}");
        assert_eq!(
            matches!(ran.stdout_spill, Some(Spilled::Stored(_))),
            big,
            "big: {big}"
        );
        let timeout = (capture * 5).max(Duration::from_millis(200));
        let (slow, pid_file) = leaves("slow", "; sleep 3600");
        let started = Instant::now();
        let ran = spawn(&slow.timeout(timeout)).unwrap();
        let took = started.elapsed();
        assert_eq!(ran.exit, Exit::TimedOut, "big: {big}, capture {capture:?}");
        assert!(took >= timeout, "big: {big}");
        assert!(
            took < timeout + KILL_AFTER + Duration::from_secs(30),
            "big: {big}: {took:?} for a {timeout:?} timeout"
        );
        assert_eq!(
            matches!(ran.stdout_spill, Some(Spilled::Stored(_))),
            big,
            "big: {big}, capture {capture:?}: {:?}",
            ran.stdout_spill
        );
        let pid = read_pid(&pid_file);
        assert!(
            gone_within(&pid, Duration::from_secs(10)),
            "big: {big}: descendant {pid} survived"
        );
    }
}

struct Harness {
    ct: Ct,
    bin: PathBuf,
}

impl Harness {
    fn new(next_exit: i32, next: &str, pick: &str) -> Self {
        let ct = Ct::new();
        let bin = ct.path().join("bin");
        let log = ct.path().join("calls.log");
        fs::write(ct.path().join("next.json"), next).unwrap();
        fs::write(ct.path().join("pick"), pick).unwrap();
        fs::create_dir(ct.path().join("tmp")).unwrap();
        script(
            &bin.join("cm"),
            &format!(
                "echo \"cm $*\" >> '{log}'\nif [ \"$1 $2\" = '--json next' ]; then cat '{next}'; exit {next_exit}; fi\n",
                log = log.display(),
                next = ct.path().join("next.json").display(),
            ),
        );
        script(
            &bin.join("git"),
            &format!(
                "echo \"git $*\" >> '{}'\ncase $1 in rev-parse|symbolic-ref|branch) echo main ;; esac\nexit 0\n",
                log.display()
            ),
        );
        script(
            &ct.path().join(beat::HANDOFF_PICK),
            &format!(
                "echo \"handoff-pick.sh $*\" >> '{}'\ncat '{}'\n",
                log.display(),
                ct.path().join("pick").display()
            ),
        );
        Harness { ct, bin }
    }

    // The runner's /tmp can be too small for a 64 MiB spill, and the built cm sees no TMPDIR.
    fn tmpdir(&self) -> PathBuf {
        self.ct.path().join("tmp")
    }

    fn command(&self, session: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOME", self.ct.path())
            .env("TMPDIR", self.tmpdir())
            .env("CLAUDE_CODE_REMOTE_SESSION_ID", session)
            .current_dir(self.ct.path());
        command
    }

    fn log(&self) -> Vec<String> {
        fs::read_to_string(self.ct.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn the_built_cm_tick_runs_the_loop_with_fake_cm_and_git_on_path() {
    // Kinds with no executor, so the stubs' cards can fill theirs without touching this test.
    let actionable = vec![
        line("attempt-ended", "01-a/u/c", "cm attempt 01-a/u/c restart"),
        line("missing-kickoff", "01-a/u/b", "cm amend 01-a/u/b"),
    ];
    let harness = Harness::new(1, &act(actionable.clone()), PICK_OK);
    let mac = "# Mac notes\n@~/elsewhere.md\n";
    harness.ct.write(".claude/CLAUDE.md", mac);
    let output = harness
        .command("cse_01abc")
        .args(["tick", "--cm", "cm", "--checkout"])
        .arg(harness.ct.path())
        .current_dir("/")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "attempt-ended 01-a/u/c: cm attempt 01-a/u/c restart\nmissing-kickoff 01-a/u/b: cm amend 01-a/u/b\n"
    );
    assert_eq!(frame_calls(&harness.log(), &FRAME), FRAME);
    assert!(!harness.log().iter().any(|call| call.starts_with("cm sync")));

    assert_eq!(
        fs::read_to_string(harness.ct.path().join(".claude/CLAUDE.md")).unwrap(),
        mac,
        "another CLAUDE.md is never touched"
    );

    let refused = Harness::new(1, &act(actionable), "refused superseded x\n");
    let output = refused
        .command("cse_01abc")
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        !refused.ct.path().join(".claude").exists(),
        "a missing CLAUDE.md is not created"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let output = refused
        .command("cse_01abc")
        .args(["--json", "tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json,
        json!({"command": "tick", "ok": true, "violations": [], "cm": refused.bin.join("cm"), "lines": []}),
        "the object a tick with no lines prints"
    );
    assert_eq!(
        refused.log(),
        [
            "git rev-parse --abbrev-ref HEAD",
            "handoff-pick.sh store/epochs/eleemosynary-mnemosyne coordinator --lineage session_01abc --max-age-hours 24",
        ]
        .repeat(2),
        "two ticks, each only the bound check"
    );
}

#[test]
fn the_built_cm_tick_stops_at_two_live_epochs_and_refuses_c() {
    let harness = Harness::new(0, &act(lines()), PICK_OK);
    harness
        .ct
        .write("store/epochs/second/EPOCH.md", "status: PLANNING\n");
    let output = harness
        .command("cse_01abc")
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "heartbeat: 2 PLANNING or WORKING epochs, not one; stay read-only until cm, PR and journal state are reconciled\n"
    );
    assert!(harness.log().is_empty(), "{:?}", harness.log());

    let output = harness
        .command("cse_01abc")
        .args(["tick", "-C", EPOCH])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("name the checkout with --checkout, not -C")
    );
    assert!(harness.log().is_empty());
}

#[test]
fn the_built_cm_tick_prints_nothing_when_there_is_nothing_to_do() {
    let harness = Harness::new(0, &act(Vec::new()), PICK_OK);
    let output = harness
        .command("cse_01abc")
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(frame_calls(&harness.log(), &FRAME), FRAME);
    assert!(!harness.log().iter().any(|call| call.starts_with("cm sync")));
}

#[test]
fn the_built_cm_tick_reads_its_own_cm_next_act_by_default() {
    let harness = Harness::new(0, &act(lines()), PICK_OK);
    let epoch = harness.ct.path().join(EPOCH);
    let me = env!("CARGO_BIN_EXE_cm");
    let cm = |args: &[&str]| {
        let output = Command::new(me)
            .current_dir(&epoch)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
    };
    cm(&["init"]);
    cm(&["init", "01-a/u/nokick", "--lane", "codex"]);
    let output = harness
        .command("cse_01abc")
        .args(["tick", "--checkout"])
        .arg(harness.ct.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "missing-kickoff 01-a/u/nokick: cm amend 01-a/u/nokick --kickoff-file '<kickoff-file>'\n"
    );
    assert!(
        !harness.log().iter().any(|call| call.starts_with("cm ")),
        "the fake cm on PATH is not used: {:?}",
        harness.log()
    );
}

#[test]
fn the_ctx_helpers_are_act_libs() {
    let ct = Ct::new();
    ct.write(
        &format!("{EPOCH}/01-a/u/.cm/state.json"),
        &json!({"steps": {
            "b": {"lane": "codex", "evidence": {"pr": "https://github.com/o/r/pull/7"}},
            "c": {"pr": "https://github.com/o/r/pull/8", "evidence": {"pr": ""}},
            "d": {},
        }})
        .to_string(),
    );
    ct.write(
        &format!("{EPOCH}/.cm/pulls.json"),
        &json!({
            "https://github.com/o/r/pull/7": {"head": "abc"},
            "https://github.com/o/r/pull/8": null,
        })
        .to_string(),
    );
    let proc = FakeProc::new(|cmd| {
        Ok(match cmd.argv[1].to_str() {
            Some("ok") => Ran::code(0, "fine\n"),
            Some("bad") => Ran::code(1, "progress\n").with_stderr("error: refused\n\n"),
            _ => Ran::code(2, ""),
        })
    });
    let env = Env::default();
    let ctx = Ctx::new(
        ct.path(),
        Path::new(EPOCH),
        &proc,
        &env,
        OsStr::new("/bin/cm"),
        Duration::from_secs(10),
    );

    assert_eq!(ctx.step_state("01-a/u/b#api").unwrap()["lane"], "codex");
    assert_eq!(
        ctx.step_state("01-a/u").unwrap_err(),
        "01-a/u is not <NN-name>/<unit>/<step>"
    );
    assert_eq!(
        ctx.step_state("01-a/u/z").unwrap_err(),
        "no cm state for 01-a/u/z"
    );
    assert_eq!(
        ctx.step_state("01-a/v/b").unwrap_err(),
        "no cm state for 01-a/v/b"
    );

    let act_line = |target: &str, pr: Option<&str>| {
        let mut json = line("merge-ready", target, "merge it");
        if let Some(pr) = pr {
            json["pr"] = json!(pr);
        }
        ActLine { json, lane: None }
    };
    let b = ctx.step_state("01-a/u/b").unwrap();
    let pull = ctx.pull(&act_line("01-a/u/b", None), &b).unwrap();
    assert_eq!(
        (pull.repo.as_str(), pull.number, pull.url.as_str()),
        ("o/r", 7, "https://github.com/o/r/pull/7")
    );
    assert_eq!(pull.observed, json!({"head": "abc"}));
    let pull = ctx
        .pull(
            &act_line("01-a/u/b", Some("https://github.com/x/y/pull/9")),
            &b,
        )
        .unwrap();
    assert_eq!((pull.repo.as_str(), pull.observed), ("x/y", json!({})));
    let c = ctx.step_state("01-a/u/c").unwrap();
    let pull = ctx.pull(&act_line("01-a/u/c", Some("")), &c).unwrap();
    assert_eq!((pull.number, pull.observed), (8, json!({})));
    let d = ctx.step_state("01-a/u/d").unwrap();
    assert_eq!(
        ctx.pull(&act_line("01-a/u/d", None), &d).unwrap_err(),
        "01-a/u/d records no PR URL"
    );

    let ran = ctx.cm("ok", &["--x", "y"]).unwrap();
    assert_eq!(ran.stdout_text(), "fine\n");
    assert_eq!(
        ctx.cm("bad", &[]).unwrap_err(),
        "cm bad failed: error: refused"
    );
    assert_eq!(ctx.cm("other", &[]).unwrap_err(), "cm other failed: exit 2");
    let calls = proc.calls();
    assert_eq!(
        calls[0].argv,
        ["/bin/cm", "ok", "-C", EPOCH, "--x", "y"],
        "cm <sub> -C <epoch> <args>"
    );
    assert_eq!(calls[0].cwd.as_deref(), Some(ct.path()));
    assert!(calls[0].timeout <= Duration::from_secs(10));
    let unstartable = FakeProc::new(|_| {
        Err(TickError::Spawn {
            program: "/bin/cm".into(),
            source: std::io::ErrorKind::NotFound.into(),
        })
    });
    let ctx = Ctx::new(
        ct.path(),
        Path::new(EPOCH),
        &unstartable,
        &env,
        OsStr::new("/bin/cm"),
        Duration::from_secs(10),
    );
    assert!(
        ctx.cm("ok", &[])
            .unwrap_err()
            .starts_with("cm ok failed: /bin/cm: ")
    );
    let spent = Ctx::new(
        ct.path(),
        Path::new(EPOCH),
        &proc,
        &env,
        OsStr::new("/bin/cm"),
        Duration::ZERO,
    );
    assert_eq!(spent.run(Cmd::new("x")).unwrap().exit, Exit::TimedOut);
    assert!(spent.timed_out());
    assert_eq!(proc.calls().len(), 3, "a spent budget starts nothing");
}

#[test]
fn a_valid_act_list_above_4_mib_prints_every_line() {
    let ct = Ct::new();
    let bin = ct.path().join("bin");
    let actionable: Vec<Value> = (0..27_000)
        .map(|n| {
            line(
                "launchable-not-launched",
                &format!("01-a/u/s{n:05}"),
                &format!("cm launch 01-a/u/s{n:05} --with-a-long-enough-tail-to-pass-four-mebibytes-x{n:05}"),
            )
        })
        .collect();
    let next = ct.path().join("next.json");
    fs::write(&next, act(actionable.clone())).unwrap();
    assert!(fs::metadata(&next).unwrap().len() > 4 << 20);
    let cm = script(
        &bin.join("cm"),
        &format!("cat '{}'\nexit 1\n", next.display()),
    );
    bound_scripts(&ct, &bin);
    let mut out = Vec::new();
    beat::run(
        &Beat {
            checkout: ct.path(),
            proc: &Bin(bin.clone()),
            env: &bound_env(),
            registry: &Registry::new(),
            phases: &QUIET,
            cm: cm.as_os_str(),
            started: Instant::now(),
        },
        &mut |line| out.push(line),
    );
    assert_eq!(out.len(), actionable.len(), "{:?}", out.first());
    for (printed, json) in out.iter().zip(&actionable) {
        assert_eq!(
            *printed,
            format!(
                "launchable-not-launched {}: {}",
                json["target"].as_str().unwrap(),
                json["command"].as_str().unwrap()
            )
        );
    }
}

#[test]
fn a_first_line_answer_is_read_from_its_head_whatever_follows() {
    let ct = Ct::new();
    let bin = ct.path().join("bin");
    let next = ct.path().join("next.json");
    fs::write(&next, act(vec![lines().remove(0)])).unwrap();
    let cm = script(
        &bin.join("cm"),
        &format!("cat '{}'\nexit 1\n", next.display()),
    );
    script(&bin.join("git"), "echo main\n");
    script(
        &ct.path().join(beat::HANDOFF_PICK),
        &format!("printf '{PICK_OK}'\nhead -c 5000000 /dev/zero | tr '\\0' x\n"),
    );
    let (executor, seen) = Record::new(done());
    let registry = Registry::new().with("launchable-not-launched", executor);
    let mut out = Vec::new();
    beat::run(
        &Beat {
            checkout: ct.path(),
            proc: &Bin(bin.clone()),
            env: &bound_env(),
            registry: &registry,
            phases: &QUIET,
            cm: cm.as_os_str(),
            started: Instant::now(),
        },
        &mut |line| out.push(line),
    );
    assert_eq!(out, Vec::<String>::new());
    assert_eq!(seen.calls().len(), 1, "bound: `ok` was the first line");
}

#[test]
fn a_descendant_cleaning_up_on_term_gets_the_whole_grace() {
    let dir = tempfile::tempdir().unwrap();
    let cleaned = dir.path().join("cleaned");
    let tidy = script(
        &dir.path().join("tidy"),
        "(trap 'sleep 1; echo done > \"$1\"; exit 0' TERM; while :; do sleep 0.1; done) &\nsleep 60\n",
    );
    let ran = spawn(
        &Cmd::new(&tidy)
            .arg(&cleaned)
            .timeout(Duration::from_secs(1)),
    )
    .unwrap();
    assert_eq!(ran.exit, Exit::TimedOut);
    assert_eq!(
        fs::read_to_string(&cleaned).ok().as_deref(),
        Some("done\n"),
        "the descendant finished its cleanup; wall {:?}",
        ran.wall
    );
    assert!(
        ran.wall >= Duration::from_secs(2) && ran.wall < Duration::from_secs(5),
        "it ended when the group did, not at the grace's end: {:?}",
        ran.wall
    );
}

#[test]
fn a_descendant_ignoring_term_is_killed_when_the_grace_ends_though_the_leader_exited() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let deaf = script(
        &dir.path().join("deaf"),
        "(trap '' TERM; while :; do sleep 0.1; done) &\necho $! > \"$1\"\nsleep 60\n",
    );
    let ran = spawn(
        &Cmd::new(&deaf)
            .arg(&pidfile)
            .timeout(Duration::from_secs(1)),
    )
    .unwrap();
    assert_eq!(ran.exit, Exit::TimedOut);
    assert!(
        ran.wall >= Duration::from_secs(6) && ran.wall < Duration::from_secs(9),
        "KILL lands when the 5 s grace ends: {:?}",
        ran.wall
    );
    let pid = read_pid(&pidfile);
    assert!(
        gone_within(&pid, Duration::from_secs(2)),
        "descendant {pid} survived"
    );
}

#[test]
fn a_child_past_its_own_shorter_timeout_does_not_spend_the_budget() {
    let ct = Ct::new();
    let answers = Answers::new(1, act(vec![lines().remove(0)]));
    let proc = FakeProc::new(move |cmd| {
        Ok(if cmd.program_name() == "slow" {
            Ran::timed_out()
        } else {
            answers.answer(cmd)
        })
    });
    struct Handles;
    impl Executor for Handles {
        fn run(&self, _: &ActLine, ctx: &Ctx) -> Outcome {
            let ran = ctx
                .run(Cmd::new("slow").timeout(Duration::from_secs(1)))
                .unwrap();
            assert_eq!(ran.exit, Exit::TimedOut);
            Outcome::Done(vec!["handled the slow child".into()])
        }
    }
    let registry = Registry::new().with("launchable-not-launched", Handles);
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &registry),
        ["handled the slow child"]
    );
}

#[test]
fn a_worker_cleaning_up_under_a_waiting_leader_writes_its_marker() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let worker = script(
        &dir.path().join("worker"),
        "trap 'sleep 1; echo cleaned > \"$1\"; exit 0' TERM\nwhile :; do sleep 0.1; done\n",
    );
    let leader = script(
        &dir.path().join("leader"),
        &format!("'{}' \"$1\" &\nwait\n", worker.display()),
    );
    let ran = spawn(
        &Cmd::new(&leader)
            .arg(&marker)
            .timeout(Duration::from_secs(1)),
    )
    .unwrap();
    assert_eq!(ran.exit, Exit::TimedOut);
    assert_eq!(
        fs::read_to_string(&marker).ok().as_deref(),
        Some("cleaned\n"),
        "wall {:?}",
        ran.wall
    );
}

#[test]
fn the_built_cm_tick_run_through_a_symlink_reads_from_the_resolved_binary() {
    let harness = Harness::new(0, &act(Vec::new()), PICK_OK);
    let real = harness.ct.path().join("real");
    fs::create_dir_all(&real).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_cm"), real.join("cm")).unwrap();
    let link = harness.ct.path().join("linked");
    fs::create_dir_all(&link).unwrap();
    std::os::unix::fs::symlink(real.join("cm"), link.join("cm")).unwrap();
    let epoch = harness.ct.path().join(EPOCH);
    let init = Command::new(real.join("cm"))
        .current_dir(&epoch)
        .arg("init")
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    let output = Command::new(link.join("cm"))
        .env_clear()
        .env(
            "PATH",
            format!("{}:{}:/usr/bin:/bin", link.display(), harness.bin.display()),
        )
        .env("HOME", harness.ct.path())
        .env("TMPDIR", harness.tmpdir())
        .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")
        .current_dir(harness.ct.path())
        .args(["--json", "tick"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["cm"],
        fs::canonicalize(real.join("cm")).unwrap().to_str().unwrap(),
        "ctx.cm is the symlink's target"
    );
    assert_eq!(json["lines"], json!([]));
}

#[test]
fn ran_says_when_a_stream_was_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let nine = script(
        &dir.path().join("nine"),
        "head -c 9437184 /dev/zero\nhead -c 9437184 /dev/zero >&2\n",
    );
    let ran = spawn(&Cmd::new(&nine)).unwrap();
    assert_eq!(ran.exit, Exit::Code(0));
    assert_eq!(ran.stdout.len(), 9 << 20, "whole by default");
    assert!(!ran.stdout_truncated);
    assert_eq!(ran.stderr.len(), OUTPUT_CAP);
    assert!(ran.stderr_truncated, "stderr keeps its tail");
    let ran = spawn(&Cmd::new(&nine).keep(Keep::Head, Keep::Head)).unwrap();
    assert!(ran.stdout_truncated && ran.stderr_truncated);
    let small = script(&dir.path().join("small"), "echo out\necho err >&2\n");
    let ran = spawn(&Cmd::new(&small).keep(Keep::Head, Keep::Tail)).unwrap();
    assert!(!ran.stdout_truncated && !ran.stderr_truncated);
}

#[test]
fn the_built_cm_tick_exits_0_when_its_stdout_is_closed() {
    let harness = Harness::new(0, &act(lines()), PICK_OK);
    harness
        .ct
        .write("store/epochs/second/EPOCH.md", "status: PLANNING\n");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = harness
        .command("cse_01abc")
        .args(["tick", "--cm", "cm"])
        .stdout(writer)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "{output:?}"
    );
}

#[test]
fn ctx_removes_variables_pauses_through_the_runner_and_builds_cm_commands() {
    let dir = tempfile::tempdir().unwrap();
    let show = script(
        &dir.path().join("show"),
        "echo \"keep=${TICK_KEEP-unset} drop=${TICK_DROP-unset}\"\n",
    );
    let ran = spawn(
        &Cmd::new(&show)
            .env("TICK_KEEP", "1")
            .env("TICK_DROP", "1")
            .env_remove("PATH")
            .env_remove("TICK_DROP"),
    )
    .unwrap();
    assert_eq!(
        ran.stdout_text(),
        "keep=1 drop=1\n",
        "env adds after removes"
    );
    let ran = spawn(&Cmd::new(&show).env_remove("HOME").env("TICK_KEEP", "2")).unwrap();
    assert_eq!(ran.stdout_text(), "keep=2 drop=unset\n");

    let ct = Ct::new();
    let proc = FakeProc::new(|cmd| Ok(Ran::code(0, cmd.stdin.clone().unwrap_or_default())));
    let env = Env::default();
    let ctx = Ctx::new(
        ct.path(),
        Path::new(EPOCH),
        &proc,
        &env,
        OsStr::new("/bin/cm"),
        Duration::from_secs(10),
    );
    let started = Instant::now();
    ctx.pause(Duration::from_secs(2));
    ctx.pause(Duration::from_secs(60));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a fake does not wait"
    );
    let pauses = proc.pauses();
    assert_eq!(pauses[0], Duration::from_secs(2));
    assert!(pauses[1] <= Duration::from_secs(10), "cut to the budget");

    let cmd = ctx
        .cm_cmd("evidence", &["01-a/u/b", "--name", "x", "--body-file", "-"])
        .arg("--json")
        .stdin("body\n");
    assert_eq!(
        cmd.argv,
        [
            "/bin/cm",
            "evidence",
            "-C",
            EPOCH,
            "01-a/u/b",
            "--name",
            "x",
            "--body-file",
            "-",
            "--json"
        ]
    );
    let ran = ctx.cm_run("evidence", cmd).unwrap();
    assert_eq!(ran.stdout_text(), "body\n");
}

#[test]
fn a_line_without_a_target_prints_it_empty_as_jq_did() {
    let ct = Ct::new();
    let proc = Answers::new(
        1,
        act(vec![
            json!({"kind": "stale-block", "command": "cm sync"}),
            json!({"kind": "review-owed", "part": "api", "command": "cm brief x"}),
        ]),
    )
    .proc();
    assert_eq!(
        tick(&ct, &proc, &bound_env(), &Registry::new()),
        ["stale-block : cm sync", "review-owed #api: cm brief x"]
    );
}

#[test]
fn the_built_cm_tick_exits_0_when_its_json_stdout_is_closed_or_full() {
    let harness = Harness::new(0, &act(lines()), PICK_OK);
    harness
        .ct
        .write("store/epochs/second/EPOCH.md", "status: PLANNING\n");
    let run = |args: &[&str], stdout: std::process::Stdio| {
        let output = harness
            .command("cse_01abc")
            .args(args)
            .stdout(stdout)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked"),
            "{args:?}: {output:?}"
        );
    };
    for args in [
        &["--json", "tick", "--cm", "cm"][..],
        &["tick", "--cm", "cm"][..],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        run(args, writer.into());
        let full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        run(args, full.into());
    }
}

fn slow_failing_next(ran: Ran) -> FakeProc {
    let answers = Answers::new(0, "");
    FakeProc::new(move |cmd| {
        Ok(if cmd.program_name() == "cm" {
            std::thread::sleep(Duration::from_secs(6));
            ran.clone()
        } else {
            answers.answer(cmd)
        })
    })
}

#[test]
fn a_failed_cm_next_past_the_act_budget_is_still_its_one_line() {
    let ct = Ct::new();
    let proc = slow_failing_next(Ran::code(2, "").with_stderr("boom\n"));
    let env = env(&[
        ("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc"),
        ("HEARTBEAT_DEADLINE", "0"),
    ]);
    assert_eq!(
        tick(&ct, &proc, &env, &Registry::new()),
        ["heartbeat: cm --json next --act exited 2 without an actionable list: boom"]
    );
}

#[test]
fn a_cm_next_killed_past_the_act_budget_is_still_its_one_line() {
    let ct = Ct::new();
    let proc = slow_failing_next(Ran::timed_out());
    let env = env(&[
        ("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc"),
        ("HEARTBEAT_DEADLINE", "0"),
    ]);
    assert_eq!(
        tick(&ct, &proc, &env, &Registry::new()),
        ["heartbeat: cm --json next --act ran past 30 s and was killed"]
    );
}

#[test]
fn a_list_read_past_the_act_budget_still_prints_the_wall_time() {
    let ct = Ct::new();
    let proc = slow_failing_next(Ran::code(0, act(Vec::new())));
    let env = env(&[
        ("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc"),
        ("HEARTBEAT_DEADLINE", "0"),
    ]);
    let out = tick(&ct, &proc, &env, &Registry::new());
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(is_wall_line(&out[0], "act"), "{out:?}");
}

#[test]
fn the_built_cm_tick_prints_a_list_under_64_mib_without_a_usable_tmpdir() {
    let harness = Harness::new(1, &act(lines()), PICK_OK);
    let not_a_dir = harness.ct.path().join("a-file");
    fs::write(&not_a_dir, "").unwrap();
    for tmpdir in [harness.ct.path().join("no-such-dir"), not_a_dir] {
        let output = harness
            .command("cse_01abc")
            .env("TMPDIR", &tmpdir)
            .args(["tick", "--cm", "cm"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{tmpdir:?}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n", PRINTED.join("\n")),
            "{tmpdir:?}"
        );
    }
}

impl Harness {
    fn big(&self) {
        script(&self.bin.join("cm"), &big_list());
    }
}

fn peak_kib(pid: u32) -> Option<u64> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .trim()
        .strip_suffix(" kB")?
        .parse()
        .ok()
}

#[test]
fn the_built_cm_tick_spills_a_list_above_64_mib_and_prints_every_line_in_bounded_memory() {
    let harness = Harness::new(1, "", PICK_OK);
    harness.big();
    let tmpdir = harness.tmpdir();
    let mut child = harness
        .command("cse_01abc")
        .args(["tick", "--cm", "cm"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut peak = 0;
    let mut count = 0;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        assert_eq!(line.unwrap(), big_line(count));
        count += 1;
        if count % 1000 == 0 {
            peak = peak.max(peak_kib(pid).unwrap_or(0));
        }
    }
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert_eq!(count, BIG_LIST);
    assert!(peak > 0, "no VmHWM read for {pid}");
    // 64 MiB held before the spill, then one line at a time; reading the list whole into a
    // `Value` peaked past 400 MiB.
    assert!(peak < 160 << 10, "peak RSS {peak} KiB");
    assert_eq!(fs::read_dir(&tmpdir).unwrap().count(), 0);
}

#[test]
fn the_built_cm_tick_names_a_spill_that_could_not_be_stored() {
    let harness = Harness::new(1, "", PICK_OK);
    harness.big();
    let tmpdir = harness.ct.path().join("no-such-dir");
    let output = harness
        .command("cse_01abc")
        .env("TMPDIR", &tmpdir)
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let prefix = format!(
        "heartbeat: cm --json next --act's list was too large to keep in memory and could not be stored in {}: ",
        tmpdir.display()
    );
    assert!(stdout.starts_with(&prefix), "{stdout}");
    assert!(stdout.ends_with("; no act line was read\n"), "{stdout}");
}

#[test]
fn a_killed_cm_tick_leaves_no_spilled_list_behind() {
    let harness = Harness::new(1, "", PICK_OK);
    harness.big();
    let tmpdir = harness.tmpdir();
    let mut child = harness
        .command("cse_01abc")
        .args(["tick", "--cm", "cm"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    assert_eq!(first.trim_end(), big_line(0), "the act loop is running");
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    child.wait().unwrap();
    assert_eq!(
        fs::read_dir(&tmpdir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        Vec::<std::ffi::OsString>::new()
    );
}

#[test]
fn the_built_cm_tick_exits_0_on_a_closed_stdout_with_act_lines_to_print() {
    let harness = Harness::new(1, &act(lines()), PICK_OK);
    for args in [
        &["--json", "tick", "--cm", "cm"][..],
        &["tick", "--cm", "cm"][..],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = harness
            .command("cse_01abc")
            .args(args)
            .stdout(writer)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked"),
            "{args:?}: {output:?}"
        );
    }
}

impl Harness {
    // The built cm under a shell resource limit, e.g. `ulimit -f 1024`.
    fn limited(&self, session: &str, limit: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOME", self.ct.path())
            .env("TMPDIR", self.tmpdir())
            .env("CLAUDE_CODE_REMOTE_SESSION_ID", session)
            .current_dir(self.ct.path())
            .arg("-c")
            .arg(format!("{limit}; exec \"$0\" \"$@\""))
            .arg(env!("CARGO_BIN_EXE_cm"));
        command
    }
}

#[test]
fn the_built_cm_tick_names_a_spill_that_would_pass_the_file_size_limit() {
    let harness = Harness::new(1, "", PICK_OK);
    write_big_list(&harness.ct.path().join("next.json"));
    let output = harness
        .limited("cse_01abc", "ulimit -f 1024")
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let efbig = std::io::Error::from_raw_os_error(27);
    assert_eq!(
        stdout,
        format!(
            "heartbeat: cm --json next --act's list was too large to keep in memory and could not be stored in {}: {efbig}; no act line was read\n",
            harness.tmpdir().display()
        )
    );
}

#[test]
fn the_built_cm_tick_stops_writing_a_stdout_file_at_the_file_size_limit() {
    let wide = |target| {
        line(
            "stale-block",
            target,
            &format!("cm sync {}", "0".repeat(1200)),
        )
    };
    // The third line would fit after the first; once the second did not, nothing more is written.
    let harness = Harness::new(
        1,
        &act(vec![
            wide("01-a/u/a"),
            wide("01-a/u/b"),
            line("stale-block", "01-a/u/c", "cm sync c"),
        ]),
        PICK_OK,
    );
    let all = harness
        .command("cse_01abc")
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    let all = String::from_utf8(all.stdout).unwrap();
    let first = all.lines().next().unwrap();
    assert_eq!(all.lines().count(), 3, "{all}");
    assert!(first.len() < 2048 && 2 * (first.len() + 1) > 2048);
    let file = harness.ct.path().join("out");
    for (args, kept) in [
        (&["tick", "--cm", "cm"][..], format!("{first}\n")),
        (&["--json", "tick", "--cm", "cm"][..], String::new()),
    ] {
        let output = harness
            .limited(
                "cse_01abc",
                &format!("ulimit -f 2; exec > '{}'", file.display()),
            )
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), kept, "{args:?}");
    }
}

// The smallest address space, in KiB, in which this build of cm ticks a two-line list: about
// 20 MB for a debug build, more under coverage or sanitizers.
fn floor_kib() -> u64 {
    let harness = Harness::new(1, &act(lines()), PICK_OK);
    let ticks = |kib: u64| {
        let output = harness
            .limited("cse_01abc", &format!("ulimit -v {kib}"))
            .args(["tick", "--cm", "cm"])
            .output()
            .unwrap();
        output.status.success() && output.stdout == format!("{}\n", PRINTED.join("\n")).as_bytes()
    };
    let (mut low, mut high) = (1_024, 4_194_304);
    assert!(ticks(high), "cm does not tick in 4 GiB");
    while high - low > 1_024 {
        let middle = (low + high) / 2;
        if ticks(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    high
}

#[test]
fn the_built_cm_tick_moves_the_list_to_its_file_when_memory_for_it_runs_out() {
    // Room for a small list's tick and 40 MiB more: less than the 64 MiB hold needs.
    let limit = floor_kib() + 40 * 1_024;
    let harness = Harness::new(1, "", PICK_OK);
    write_big_list(&harness.ct.path().join("next.json"));
    let output = harness
        .limited("cse_01abc", &format!("ulimit -v {limit}"))
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), BIG_LIST);
    for (n, line) in stdout.lines().enumerate() {
        assert_eq!(line, big_line(n));
    }
}

#[test]
fn text_from_a_path_the_act_list_or_a_child_stays_one_escaped_line() {
    let raw = |lines: &[String]| lines.iter().any(|line| line.contains(char::is_control));
    let obey = "merge-ready 01-a: cm advance 01-a merged";

    let ct = Ct::new();
    let named = ct.path().join(format!("store/epochs/x\n{obey}"));
    fs::create_dir_all(&named).unwrap();
    std::os::unix::fs::symlink("EPOCH.md", named.join("EPOCH.md")).unwrap();
    let out = tick(
        &ct,
        &Answers::new(1, act(lines())).proc(),
        &bound_env(),
        &Registry::new(),
    );
    assert_eq!(
        out,
        [format!(
            "heartbeat: could not read store/epochs/x\\n{obey}/EPOCH.md: {}; stay read-only until cm, PR and journal state are reconciled",
            std::io::Error::from_raw_os_error(40)
        )]
    );

    let ct = Ct::new();
    let list = act(vec![
        line(
            &format!("stalled-attempt\n{obey}"),
            "01-a/ü/a",
            "cm attempt 01-a/ü/a",
        ),
        line("launchable-not-launched", "01-a/u/b", "cm launch 01-a/u/b"),
        line("missing-kickoff", "01-a/u/c", "cm amend 01-a/u/c"),
    ]);
    let (failing, _) = Record::new(Outcome::Failed(format!("boom\u{1b}[2K\t{obey}")));
    let (leaving, _) = Record::new(Outcome::Done(vec![format!(
        "left\r{obey}\u{7f}\u{2028}\u{85}\u{9b}\u{2029}"
    )]));
    let registry = Registry::new()
        .with("launchable-not-launched", failing)
        .with("missing-kickoff", leaving);
    let out = tick(&ct, &Answers::new(1, list).proc(), &bound_env(), &registry);
    assert!(!raw(&out), "{out:?}");
    assert_eq!(
        out[..3],
        [
            format!("stalled-attempt\\n{obey} 01-a/ü/a: cm attempt 01-a/ü/a"),
            format!(
                "launchable-not-launched 01-a/u/b: cm launch 01-a/u/b [tick/launchable-not-launched failed: boom\\u{{1b}}[2K\\t{obey}]"
            ),
            format!("left\\r{obey}\\u{{7f}}\\u{{2028}}\\u{{85}}\\u{{9b}}\\u{{2029}}"),
        ]
    );

    let ct = Ct::new();
    let mut answers = Answers::new(2, "");
    answers.next = Ran::code(2, "").with_stderr(format!("boom\u{1b}]0;x\u{7}\r{obey}\nsecond"));
    let out = tick(&ct, &answers.proc(), &bound_env(), &Registry::new());
    assert!(!raw(&out), "{out:?}");
    assert_eq!(
        out,
        [format!(
            "heartbeat: cm --json next --act exited 2 without an actionable list: boom\\u{{1b}}]0;x\\u{{7}}\\r{obey}"
        )]
    );
}

#[test]
fn an_epoch_store_that_cannot_be_read_is_named_not_counted_as_none() {
    let stay = "stay read-only until cm, PR and journal state are reconciled";
    let eloop = std::io::Error::from_raw_os_error(40);
    let store = tempfile::tempdir().unwrap();
    fs::create_dir(store.path().join("store")).unwrap();
    std::os::unix::fs::symlink("epochs", store.path().join("store/epochs")).unwrap();
    let one_epoch = Ct::new();
    fs::create_dir_all(one_epoch.path().join("store/epochs/looped")).unwrap();
    std::os::unix::fs::symlink(
        "EPOCH.md",
        one_epoch.path().join("store/epochs/looped/EPOCH.md"),
    )
    .unwrap();
    for (checkout, path) in [
        (store.path(), "store/epochs"),
        (one_epoch.path(), "store/epochs/looped/EPOCH.md"),
    ] {
        let error = beat::live_epochs(checkout).unwrap_err();
        assert_eq!(error.to_string(), format!("{path}: {eloop}"));
        let proc = Answers::new(1, act(lines())).proc();
        let mut out = Vec::new();
        beat::run(
            &Beat {
                checkout,
                proc: &proc,
                env: &bound_env(),
                registry: &Registry::new(),
                phases: &QUIET,
                cm: OsStr::new("cm"),
                started: Instant::now(),
            },
            &mut |line| out.push(line),
        );
        assert_eq!(
            out,
            [format!("heartbeat: could not read {path}: {eloop}; {stay}")]
        );
        assert!(proc.calls().is_empty());
    }
    let shapes = tempfile::tempdir().unwrap();
    assert!(beat::live_epochs(shapes.path()).unwrap().is_empty());
    write(
        &shapes.path().join("store/epochs"),
        "a file, not a folder\n",
    );
    assert!(beat::live_epochs(shapes.path()).unwrap().is_empty());
    fs::remove_file(shapes.path().join("store/epochs")).unwrap();
    write(
        &shapes.path().join("store/epochs/README"),
        "a file among epochs\n",
    );
    assert!(beat::live_epochs(shapes.path()).unwrap().is_empty());
}

#[test]
fn the_built_cm_tick_resolves_its_cm_where_it_was_given() {
    let harness = Harness::new(1, &act(lines()), PICK_OK);
    let elsewhere = harness.ct.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let tick = |cm: &str| {
        let output = harness
            .command("cse_01abc")
            .current_dir(&elsewhere)
            .args(["--json", "tick", "--checkout"])
            .arg(harness.ct.path())
            .args(["--cm", cm])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{cm}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let real = fs::canonicalize(harness.bin.join("cm")).unwrap();
    for cm in ["../bin/cm", "cm"] {
        let json = tick(cm);
        let used = Path::new(json["cm"].as_str().unwrap());
        assert!(used.is_absolute(), "{cm}: {json}");
        assert_eq!(fs::canonicalize(used).unwrap(), real, "{cm}");
        assert_eq!(json["lines"], json!(PRINTED), "{cm}");
    }
    let json = tick("no-such-cm");
    assert_eq!(json["cm"], "no-such-cm");
    let lines = json["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1, "{json}");
    assert!(
        lines[0]
            .as_str()
            .unwrap()
            .starts_with("heartbeat: cm --json next --act could not run: no-such-cm: "),
        "{json}"
    );
}

fn tick_json(harness: &Harness, dir: &Path, path: &str) -> Value {
    let output = harness
        .command("cse_01abc")
        .current_dir(dir)
        .env("PATH", path)
        .args(["--json", "tick", "--checkout"])
        .arg(harness.ct.path())
        .args(["--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{path}: {output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn the_built_cm_tick_takes_the_first_cm_on_path_its_user_may_run() {
    let harness = Harness::new(1, &act(lines()), PICK_OK);
    let working = fs::read_to_string(harness.bin.join("cm")).unwrap();
    let entries = harness.ct.path().join("entries");
    let entry = |name: &str| {
        let dir = entries.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    };
    fs::create_dir_all(entry("folder").join("cm")).unwrap();
    std::os::unix::fs::symlink(entries.join("nowhere"), entry("dangling").join("cm")).unwrap();
    for (name, mode) in [("plain", 0o644), ("others", 0o641)] {
        let cm = script(&entry(name).join("cm"), &working);
        fs::set_permissions(&cm, fs::Permissions::from_mode(mode)).unwrap();
    }
    let path = ["folder", "dangling", "plain", "others"]
        .map(|name| entries.join(name).display().to_string())
        .join(":");
    let json = tick_json(
        &harness,
        harness.ct.path(),
        &format!("{path}:{}:/usr/bin:/bin", harness.bin.display()),
    );
    // A directory, a dangling symlink and a file with no execute bit are skipped by everyone.
    // Execute for others only is not the owner's to run, but is root's (any execute bit will
    // do for root), so the first entry this user may run is decided by who runs the suite.
    let runs = if rustix::process::geteuid().is_root() {
        entries.join("others/cm")
    } else {
        harness.bin.join("cm")
    };
    assert_eq!(json["cm"], runs.display().to_string(), "{json}");
    assert_eq!(json["lines"], json!(PRINTED), "{json}");
}

#[test]
fn the_built_cm_tick_reads_empty_and_relative_path_entries_from_where_it_started() {
    let harness = Harness::new(1, &act(lines()), PICK_OK);
    let working = fs::read_to_string(harness.bin.join("cm")).unwrap();
    let started = harness.ct.path().join("started");
    script(&started.join("cm"), &working);
    script(&started.join("relative/cm"), &working);
    let bin = harness.bin.display();
    for (path, runs) in [
        (format!(":{bin}:/usr/bin:/bin"), started.join("cm")),
        (
            format!("relative:{bin}:/usr/bin:/bin"),
            started.join("relative/cm"),
        ),
    ] {
        let json = tick_json(&harness, &started, &path);
        let used = Path::new(json["cm"].as_str().unwrap());
        assert!(used.is_absolute(), "{path}: {json}");
        assert_eq!(fs::canonicalize(used).unwrap(), runs, "{path}");
        assert_eq!(json["lines"], json!(PRINTED), "{path}: {json}");
    }
}

#[test]
fn the_built_cm_tick_checks_every_spill_write_against_the_file_size_limit() {
    let harness = Harness::new(1, "", PICK_OK);
    write_big_list(&harness.ct.path().join("next.json"));
    // Room for the first write, the 64 MiB held so far, but not for the rest of the list.
    let limit_kib = (WHOLE_CAP >> 10) + 1_024;
    let output = harness
        .limited("cse_01abc", &format!("ulimit -f {limit_kib}"))
        .args(["tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let efbig = std::io::Error::from_raw_os_error(27);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "heartbeat: cm --json next --act's list was too large to keep in memory and could not be stored in {}: {efbig}; no act line was read\n",
            harness.tmpdir().display()
        )
    );
}

#[test]
fn the_built_cm_tick_streams_its_json_object_without_a_second_copy() {
    // The lines and the object built from them fit; a further copy of the object as text,
    // about 73 MB here, does not.
    let limit = floor_kib() + 187 * 1_024;
    let harness = Harness::new(1, "", PICK_OK);
    write_big_list(&harness.ct.path().join("next.json"));
    let output = harness
        .limited("cse_01abc", &format!("ulimit -v {limit}"))
        .args(["--json", "tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    let lines = json["lines"].as_array().unwrap();
    assert_eq!(lines.len(), BIG_LIST);
    assert_eq!(lines[BIG_LIST - 1], big_line(BIG_LIST - 1));
}

#[test]
fn an_unbound_session_prints_nothing_and_runs_only_the_bound_check() {
    let ct = Ct::new();
    let phased = RefCell::new(Vec::<String>::new());
    let phase = |name: &'static str| {
        let phased = &phased;
        move |_: &Ctx| {
            phased.borrow_mut().push(name.to_owned());
            Report {
                lines: vec![format!("{name} printed")],
                failed: true,
            }
        }
    };
    let gitsync = |_: &Ctx, mode: Mode| {
        phased.borrow_mut().push(format!("gitsync {mode:?}"));
        Report {
            lines: vec!["gitsync printed".into()],
            failed: true,
        }
    };
    let (pin, directive, collect, claude_md) = (
        phase("pin"),
        phase("directive"),
        phase("collect"),
        phase("claude-md"),
    );
    let phases = Phases {
        gitsync: &gitsync,
        pin: &pin,
        directive: &directive,
        collect: &collect,
        claude_md: &claude_md,
    };
    let handoff = format!("{EPOCH}/handoff");
    let next = || Answers::new(1, act(lines()));
    let refused = |pick: &str| Answers {
        pick: Ran::code(1, format!("{pick}\n")),
        ..next()
    };
    let on_origin = || Answers {
        added: Ran::code(0, format!("{handoff}/coordinator-2026-11-26T0100Z.md\0")),
        ..next()
    };
    let timed_out = |mut ran: Ran| {
        ran.exit = Exit::TimedOut;
        ran
    };
    let signalled = |mut ran: Ran| {
        ran.exit = Exit::Signal(9);
        ran
    };
    let [rev, pick, fetch, diff] = BOUND_CHECK;
    let archive = format!("git archive -o <tmp>/h.tar origin/main {handoff}");
    let tar = "tar -x -C <tmp> -f <tmp>/h.tar";
    let origin_pick = format!(
        "sh tools/cloud-env/handoff-pick.sh <tmp>/{EPOCH} coordinator --lineage session_01abc --max-age-hours 24"
    );
    let listing = format!("git ls-tree -r -z origin/main -- {handoff}");
    let read = [
        rev,
        pick,
        fetch,
        diff,
        &archive,
        tar,
        &listing,
        &origin_pick,
    ];
    let cases: Vec<(&str, Answers, Env, Vec<&str>)> = vec![
        (
            "a lane session: no session id",
            next(),
            Env::default(),
            vec![rev],
        ),
        (
            "an empty session id",
            next(),
            env(&[("CLAUDE_CODE_REMOTE_SESSION_ID", "")]),
            vec![rev],
        ),
        (
            "off main",
            Answers {
                branch: "feature\n".into(),
                ..next()
            },
            bound_env(),
            vec![rev],
        ),
        (
            "a retired coordinator",
            refused("refused superseded-by store/epochs/eleemosynary-mnemosyne/handoff/y.md"),
            bound_env(),
            vec![rev, pick],
        ),
        (
            "a successor before its start proof",
            Answers {
                pick: Ran::code(
                    3,
                    "missing other-lineage store/epochs/eleemosynary-mnemosyne/handoff/coordinator-2026-11-24T2030Z.md\n",
                ),
                ..next()
            },
            bound_env(),
            vec![rev, pick],
        ),
        (
            "a malformed coordinator handoff",
            refused(
                "refused malformed store/epochs/eleemosynary-mnemosyne/handoff/coordinator-bad.md",
            ),
            bound_env(),
            vec![rev, pick],
        ),
        (
            "only pre-binding handoffs",
            Answers {
                pick: Ran::code(4, "unbound x\n"),
                ..next()
            },
            bound_env(),
            vec![rev, pick],
        ),
        ("no answer", refused(""), bound_env(), vec![rev, pick]),
        (
            "a first line that is not ok or stale",
            refused("okay\nok second line"),
            bound_env(),
            vec![rev, pick],
        ),
        (
            "a pick that timed out",
            Answers {
                pick: timed_out(Ran::code(0, PICK_OK)),
                ..next()
            },
            bound_env(),
            vec![rev, pick],
        ),
        (
            "a successor's handoff on origin",
            Answers {
                origin_pick: Ran::code(
                    3,
                    "missing other-lineage x/coordinator-2026-11-26T0100Z.md\n",
                ),
                ..on_origin()
            },
            bound_env(),
            read.to_vec(),
        ),
        (
            "origin's pick timed out",
            Answers {
                origin_pick: timed_out(Ran::code(0, PICK_OK)),
                ..on_origin()
            },
            bound_env(),
            read.to_vec(),
        ),
        (
            "a failed archive",
            Answers {
                archive: Ran::code(128, ""),
                ..on_origin()
            },
            bound_env(),
            read[..5].to_vec(),
        ),
        (
            "an archive that timed out",
            Answers {
                archive: Ran::timed_out(),
                ..on_origin()
            },
            bound_env(),
            read[..5].to_vec(),
        ),
        (
            "a failed extract",
            Answers {
                extract: Ran::code(2, ""),
                ..on_origin()
            },
            bound_env(),
            read[..6].to_vec(),
        ),
        (
            "an extract that timed out",
            Answers {
                extract: Ran::timed_out(),
                ..on_origin()
            },
            bound_env(),
            read[..6].to_vec(),
        ),
        (
            "a branch that only ends in main",
            Answers {
                branch: "x/main\n".into(),
                ..next()
            },
            bound_env(),
            vec![rev],
        ),
        (
            "an empty first line before ok",
            Answers {
                pick: Ran::code(0, format!("\n{PICK_OK}")),
                ..next()
            },
            bound_env(),
            vec![rev, pick],
        ),
        (
            "an ok pick that exited 1",
            Answers {
                pick: Ran::code(1, PICK_OK),
                ..next()
            },
            bound_env(),
            vec![rev, pick],
        ),
        (
            "a signalled pick",
            Answers {
                pick: signalled(Ran::code(0, PICK_OK)),
                ..next()
            },
            bound_env(),
            vec![rev, pick],
        ),
        (
            "origin's ok pick that exited 1",
            Answers {
                origin_pick: Ran::code(1, PICK_OK),
                ..on_origin()
            },
            bound_env(),
            read.to_vec(),
        ),
        (
            "origin's signalled pick",
            Answers {
                origin_pick: signalled(Ran::code(6, "stale x\n")),
                ..on_origin()
            },
            bound_env(),
            read.to_vec(),
        ),
        (
            "a failed listing of origin's tree",
            Answers {
                listing: Ran::code(128, ""),
                ..on_origin()
            },
            bound_env(),
            read[..7].to_vec(),
        ),
        (
            "a listing that timed out",
            Answers {
                listing: Ran::timed_out(),
                ..on_origin()
            },
            bound_env(),
            read[..7].to_vec(),
        ),
        (
            "a listing cut at its head",
            Answers {
                listing: Ran {
                    stdout_truncated: true,
                    ..Ran::code(0, "")
                },
                ..on_origin()
            },
            bound_env(),
            read[..7].to_vec(),
        ),
        (
            "a file origin lists that the extract lacks, as export-ignore drops one",
            Answers {
                listing: Ran::code(
                    0,
                    format!(
                        "100644 blob {}\t{handoff}/coordinator-2026-11-26T0100Z.md\0",
                        "a".repeat(40)
                    ),
                ),
                ..on_origin()
            },
            bound_env(),
            read[..7].to_vec(),
        ),
    ];
    let late_branch = Answers::new(1, act(lines()));
    let late_branch = FakeProc::new(move |cmd| {
        Ok(match cmd.argv[1].to_str() {
            Some("rev-parse") => Ran {
                exit: Exit::TimedOut,
                ..Ran::code(0, "main\n")
            },
            _ => late_branch.answer(cmd),
        })
    });
    assert_eq!(
        tick_with(&ct, &late_branch, &bound_env(), &Registry::new(), &phases),
        Vec::<String>::new()
    );
    assert_eq!(late_branch.log(), [rev], "a branch read that timed out");
    assert_eq!(*phased.borrow(), Vec::<String>::new());
    for (case, answers, env, expected) in cases {
        let proc = answers.proc();
        let (executor, seen) = Record::new(done());
        let registry = Registry::new()
            .with("launchable-not-launched", executor)
            .with("attempt-ended", Panics);
        assert_eq!(
            tick_with(&ct, &proc, &env, &registry, &phases),
            Vec::<String>::new(),
            "{case}"
        );
        assert_eq!(*phased.borrow(), Vec::<String>::new(), "{case}: no phase");
        assert!(seen.calls().is_empty(), "{case}: no executor");
        let calls = proc.calls();
        let tmp = calls
            .iter()
            .find(|call| call.argv[..2] == ["git", "archive"])
            .map(|call| Path::new(&call.argv[3]).parent().unwrap().to_owned());
        if let Some(tmp) = &tmp {
            assert!(
                tmp.starts_with("/tmp"),
                "{case}: no TMPDIR is /tmp: {tmp:?}"
            );
            assert!(!tmp.exists(), "{case}: {tmp:?} is removed");
        }
        let log: Vec<String> = calls
            .iter()
            .map(|call| match &tmp {
                Some(tmp) => call.line().replace(&*tmp.to_string_lossy(), "<tmp>"),
                None => call.line(),
            })
            .collect();
        assert_eq!(log, expected, "{case}: only the bound check's own commands");
        for call in &calls {
            assert_eq!(call.cwd.as_deref(), Some(ct.path()), "{case}");
            assert_eq!(call.timeout, beat::FRAME_CAP, "{case}");
        }
    }
}

#[test]
fn the_frame_re_checks_after_each_gitsync_and_stops_once_a_successor_appears() {
    let ct = Ct::new();
    let handoff = format!("{EPOCH}/handoff");
    for appears in [None, Some(Mode::Always), Some(Mode::IfDirty)] {
        let successor = Rc::new(std::cell::Cell::new(false));
        let proc = {
            let successor = successor.clone();
            let handoff = handoff.clone();
            FakeProc::new(move |cmd| {
                let mut answers = Answers::new(1, act(vec![lines().remove(0)]));
                if successor.get() {
                    answers.added =
                        Ran::code(0, format!("{handoff}/coordinator-2026-11-26T0100Z.md\0"));
                    answers.origin_pick = Ran::code(
                        3,
                        "missing other-lineage x/coordinator-2026-11-26T0100Z.md\n",
                    );
                }
                Ok(answers.answer(cmd))
            })
        };
        let phased = RefCell::new(Vec::<String>::new());
        let gitsync = |_: &Ctx, mode: Mode| {
            phased.borrow_mut().push(format!("gitsync {mode:?}"));
            if appears == Some(mode) {
                successor.set(true);
            }
            Report {
                lines: vec![format!("gitsync {mode:?} printed")],
                failed: true,
            }
        };
        let phase = |name: &'static str| {
            let phased = &phased;
            move |_: &Ctx| {
                phased.borrow_mut().push(name.to_owned());
                Report::default()
            }
        };
        let (pin, directive, collect, claude_md) = (
            phase("pin"),
            phase("directive"),
            phase("collect"),
            phase("claude-md"),
        );
        let phases = Phases {
            gitsync: &gitsync,
            pin: &pin,
            directive: &directive,
            collect: &collect,
            claude_md: &claude_md,
        };
        let (executor, seen) = Record::new(done());
        let registry = Registry::new().with("launchable-not-launched", executor);
        let out = tick_with(&ct, &proc, &bound_env(), &registry, &phases);
        let cm_calls = proc
            .log()
            .iter()
            .filter(|call| call.starts_with("cm "))
            .count();
        let reads = proc
            .log()
            .iter()
            .filter(|call| call.starts_with("git archive"))
            .count();
        match appears {
            Some(Mode::Always) => {
                assert_eq!(
                    out,
                    ["gitsync Always printed"],
                    "the phase's lines stay; no wall time, no later line"
                );
                assert_eq!(*phased.borrow(), ["gitsync Always"]);
                assert_eq!((seen.calls().len(), cm_calls, reads), (0, 0, 1));
            }
            Some(Mode::IfDirty) => {
                assert_eq!(out.len(), 3, "{out:?}");
                assert_eq!(out[0], "gitsync Always printed");
                assert!(is_wall_line(&out[1], "gitsync"), "{out:?}");
                assert_eq!(out[2], "gitsync IfDirty printed");
                assert_eq!(
                    *phased.borrow(),
                    [
                        "gitsync Always",
                        "pin",
                        "directive",
                        "collect",
                        "gitsync IfDirty"
                    ],
                    "no claude-md once the session is retired"
                );
                assert_eq!((seen.calls().len(), cm_calls, reads), (1, 1, 1));
            }
            _ => {
                assert_eq!(out.len(), 4, "{out:?}");
                assert!(is_wall_line(&out[1], "gitsync") && is_wall_line(&out[3], "gitsync"));
                assert_eq!(
                    *phased.borrow(),
                    [
                        "gitsync Always",
                        "pin",
                        "directive",
                        "collect",
                        "gitsync IfDirty",
                        "claude-md"
                    ]
                );
                assert_eq!((seen.calls().len(), cm_calls, reads), (1, 1, 0));
                assert_eq!(
                    proc.log()
                        .iter()
                        .filter(|call| *call == BOUND_CHECK[0])
                        .count(),
                    3,
                    "the check, then the re-check after each gitsync"
                );
            }
        }
    }
}

#[test]
fn check_bound_answers_for_the_one_live_epoch_only() {
    let ct = Ct::new();
    let proc = Answers::new(0, "").proc();
    assert!(beat::check(ct.path(), &proc, &bound_env()));
    assert_eq!(
        proc.log(),
        BOUND_CHECK,
        "the bound check alone: no cm, no phase"
    );
    let proc = Answers::new(0, "").proc();
    assert!(!beat::check(ct.path(), &proc, &Env::default()));
    assert_eq!(proc.log(), [BOUND_CHECK[0]]);

    ct.write(
        "store/epochs/second/EPOCH.md",
        "---\nstatus: PLANNING\n---\n",
    );
    let empty = tempfile::tempdir().unwrap();
    let unreadable = tempfile::tempdir().unwrap();
    fs::create_dir(unreadable.path().join("store")).unwrap();
    std::os::unix::fs::symlink("epochs", unreadable.path().join("store/epochs")).unwrap();
    for checkout in [ct.path(), empty.path(), unreadable.path()] {
        let proc = Answers::new(0, "").proc();
        assert!(!beat::check(checkout, &proc, &bound_env()), "{checkout:?}");
        assert!(proc.log().is_empty(), "{checkout:?}");
    }
}

const OURS: &str = "coordinator-2026-11-24T2030Z.md";
const NEXT: &str = "coordinator-2026-11-26T0100Z.md";

// A campaign-trail checkout with real git: a bare origin, the coordinator's clone `ct`, and a
// clone `other` that pushes other sessions' handoffs. Its git and its `cm tick` runs start from
// an empty environment with their own HOME, GIT_CONFIG_GLOBAL and TMPDIR, and PATH /usr/bin:/bin.
struct Origin(tempfile::TempDir);

struct Snapshot {
    status: String,
    head: String,
    reflog: String,
    origin: String,
}

impl Origin {
    fn new() -> Self {
        let origin = Origin(tempfile::tempdir().unwrap());
        fs::create_dir_all(origin.home().join(".claude")).unwrap();
        fs::create_dir_all(origin.tmpdir()).unwrap();
        write(
            &origin.path("gitconfig"),
            "[user]\n\tname = cm tick test\n\temail = tick@example.invalid\n[commit]\n\tgpgsign = false\n",
        );
        fs::write(origin.path("next.json"), act(lines())).unwrap();
        script(
            &origin.cm(),
            &format!(
                "echo \"cm $*\" >> '{}'\nif [ \"$1 $2\" = '--json next' ]; then cat '{}'; exit 1; fi\n",
                origin.path("cm.log").display(),
                origin.path("next.json").display()
            ),
        );
        let root = origin.0.path();
        origin.git(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        origin.git(root, &["init", "-q", "-b", "main", "ct"]);
        let ct = origin.ct();
        write(
            &ct.join(EPOCH).join("EPOCH.md"),
            "---\nslug: eleemosynary-mnemosyne\nstatus: WORKING\n---\n",
        );
        write(
            &ct.join("store/epochs/old/EPOCH.md"),
            "---\nstatus: CLOSED\n---\n",
        );
        write(
            &ct.join(EPOCH).join("handoff").join(OURS),
            &handoff("session_01abc", None),
        );
        // handoff-pick.sh's chain rule over the folder it is given: a coordinator-*.md that no
        // other names in `supersedes=` is a head; this lineage's head is ok (stale when it holds
        // a line `stale`), else the other heads are another lineage's.
        write(
            &ct.join(beat::HANDOFF_PICK),
            &format!(
                r#"echo "handoff-pick.sh $*" >> '{}'
dir=$1/handoff
other=
for f in "$dir"/coordinator-*.md; do
  [ -f "$f" ] || continue
  name=${{f##*/}}
  if grep -q "^Handoff: .* supersedes=$name\$" "$dir"/coordinator-*.md; then continue; fi
  if grep -q "^Handoff: .* lineage=$4 " "$f"; then
    if grep -qx stale "$f"; then echo "stale $1/handoff/$name"; exit 6; fi
    echo "ok $1/handoff/$name"
    exit 0
  fi
  other="$other $1/handoff/$name"
done
if [ -n "$other" ]; then echo "missing other-lineage$other"; else echo missing; fi
exit 3
"#,
                origin.path("pick.log").display()
            ),
        );
        origin.git(&ct, &["add", "-A"]);
        origin.git(&ct, &["commit", "-qm", "epoch"]);
        origin.git(&ct, &["remote", "add", "origin", "../origin.git"]);
        origin.git(&ct, &["push", "-q", "-u", "origin", "main"]);
        origin.git(root, &["clone", "-q", "origin.git", "other"]);
        origin
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.path().join(relative)
    }

    fn ct(&self) -> PathBuf {
        self.path("ct")
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    fn tmpdir(&self) -> PathBuf {
        self.path("tmp")
    }

    fn cm(&self) -> PathBuf {
        self.path("bin/cm")
    }

    fn vars(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("PATH", "/usr/bin:/bin".into()),
            ("HOME", self.home()),
            ("GIT_CONFIG_GLOBAL", self.path("gitconfig")),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("TMPDIR", self.tmpdir()),
        ]
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(self.vars())
            .current_dir(self.0.path());
        command
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = self
            .command("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn push_handoff(&self, name: &str, lineage: &str, supersedes: Option<&str>) {
        let other = self.path("other");
        self.git(&other, &["pull", "-q", "--rebase", "origin", "main"]);
        write(
            &other.join(EPOCH).join("handoff").join(name),
            &handoff(lineage, supersedes),
        );
        self.git(&other, &["add", "-A"]);
        self.git(&other, &["commit", "-qm", &format!("handoff {name}")]);
        self.git(&other, &["push", "-q", "origin", "main"]);
    }

    fn tick(&self, session: Option<&str>, args: &[&str]) -> std::process::Output {
        let mut command = self.command(env!("CARGO_BIN_EXE_cm"));
        if let Some(session) = session {
            command.env("CLAUDE_CODE_REMOTE_SESSION_ID", session);
        }
        command
            .current_dir(self.0.path())
            .args(args)
            .arg("tick")
            .arg("--cm")
            .arg(self.cm())
            .arg("--checkout")
            .arg(self.ct());
        command.output().unwrap()
    }

    fn silent(&self, session: Option<&str>, args: &[&str]) {
        let output = self.tick(session, args);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }

    fn acts(&self, session: Option<&str>) {
        let output = self.tick(session, &[]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n", PRINTED.join("\n"))
        );
    }

    fn check_bound(&self, session: Option<&str>) -> Option<i32> {
        let mut codes = Vec::new();
        for args in [&[][..], &["--json"]] {
            let mut command = self.command(env!("CARGO_BIN_EXE_cm"));
            if let Some(session) = session {
                command.env("CLAUDE_CODE_REMOTE_SESSION_ID", session);
            }
            let output = command
                .current_dir(self.0.path())
                .args(args)
                .args(["tick", "--check-bound", "--cm"])
                .arg(self.cm())
                .arg("--checkout")
                .arg(self.ct())
                .output()
                .unwrap();
            assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
            assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
            assert!(
                matches!(output.status.code(), Some(0 | 1)),
                "{args:?}: {output:?}"
            );
            codes.push(output.status.code());
        }
        assert_eq!(codes[0], codes[1], "--json changes nothing");
        codes[0]
    }

    fn snapshot(&self) -> Snapshot {
        let ct = self.ct();
        Snapshot {
            status: self.git(&ct, &["status", "--porcelain", "--untracked-files=all"]),
            head: self.git(&ct, &["rev-parse", "HEAD"]),
            reflog: self.git(&ct, &["reflog", "--format=%H %gs"]),
            origin: self.git(&self.path("origin.git"), &["log", "--format=%H %s", "main"]),
        }
    }

    fn unchanged(&self, before: &Snapshot) {
        let after = self.snapshot();
        assert_eq!(after.status, before.status, "git status");
        assert_eq!(after.head, before.head, "HEAD");
        assert_eq!(after.reflog, before.reflog, "no commit, pull or rebase");
        assert_eq!(after.origin, before.origin, "no push");
    }

    fn log(&self, name: &str) -> Vec<String> {
        fs::read_to_string(self.path(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn clear_logs(&self) {
        for name in ["cm.log", "pick.log"] {
            let _ = fs::remove_file(self.path(name));
        }
    }

    fn dirty(&self) {
        let ct = self.ct();
        write(&ct.join(EPOCH).join("CARD.md"), "an untracked card\n");
        write(
            &ct.join(EPOCH).join("EPOCH.md"),
            "---\nslug: eleemosynary-mnemosyne\nstatus: WORKING\n---\nedited\n",
        );
    }

    fn tmp_is_empty(&self) {
        assert_eq!(
            fs::read_dir(self.tmpdir()).unwrap().count(),
            0,
            "the extract is removed"
        );
    }
}

fn handoff(lineage: &str, supersedes: Option<&str>) -> String {
    let supersedes = supersedes.map_or_else(String::new, |name| format!(" supersedes={name}"));
    format!(
        "# Coordinator handoff\n\nHandoff: epoch=eleemosynary-mnemosyne role=coordinator lineage={lineage} written=2026-11-24T20:30:00+00:00{supersedes}\n"
    )
}

fn local_pick(lineage: &str) -> String {
    format!("handoff-pick.sh {EPOCH} coordinator --lineage {lineage} --max-age-hours 24")
}

fn origin_pick(tmpdir: &Path, line: &str) -> bool {
    line.strip_prefix(&format!(
        "handoff-pick.sh {}",
        tmpdir.join("cm-tick-").display()
    ))
    .and_then(|rest| rest.split_once('/'))
    .is_some_and(|(_, rest)| {
        rest == format!("{EPOCH} coordinator --lineage session_01abc --max-age-hours 24")
    })
}

#[test]
fn the_built_cm_tick_acts_in_the_bound_coordinator() {
    let origin = Origin::new();
    origin.acts(Some("cse_01abc"));
    assert_eq!(
        origin.log("pick.log"),
        [local_pick("session_01abc"), local_pick("session_01abc")],
        "the check and the re-check after gitsync, from the checkout"
    );
    assert_eq!(
        origin.log("cm.log"),
        [format!("cm --json next -C {EPOCH} --act")]
    );
    let output = origin.tick(Some("cse_01abc"), &["--json"]);
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["lines"], json!(PRINTED));
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(0));
}

#[test]
fn the_built_cm_tick_gives_any_other_session_silence_and_writes_nothing() {
    let origin = Origin::new();
    origin.dirty();
    let boot = "# dekopon-coordinator cloud environment\nold rules\n";
    write(&origin.home().join(".claude/CLAUDE.md"), boot);
    let before = origin.snapshot();
    // A lane session; a successor before its start proof, whose handoff is not written yet.
    for session in [None, Some("cse_02next")] {
        origin.clear_logs();
        origin.silent(session, &[]);
        let output = origin.tick(session, &["--json"]);
        assert_eq!(output.status.code(), Some(0));
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["lines"], json!([]), "{session:?}");
        assert!(origin.log("cm.log").is_empty(), "{session:?}: no cm");
        assert_eq!(origin.check_bound(session), Some(1), "{session:?}");
    }
    assert_eq!(
        origin.log("pick.log"),
        vec![local_pick("session_02next"); 4]
    );
    origin.unchanged(&before);

    // A retired coordinator: the successor's handoff, which supersedes ours, was pulled.
    let retired = Origin::new();
    retired.push_handoff(NEXT, "session_02next", Some(OURS));
    retired.git(&retired.ct(), &["pull", "-q", "--rebase", "origin", "main"]);
    retired.dirty();
    write(&retired.home().join(".claude/CLAUDE.md"), boot);
    let before = retired.snapshot();
    retired.silent(Some("cse_01abc"), &[]);
    assert_eq!(retired.check_bound(Some("cse_01abc")), Some(1));
    retired.unchanged(&before);
    assert!(retired.log("cm.log").is_empty());
    for origin in [&origin, &retired] {
        assert_eq!(
            fs::read_to_string(origin.home().join(".claude/CLAUDE.md")).unwrap(),
            boot
        );
    }
}

#[test]
fn the_built_cm_tick_silences_a_stale_checkout_when_a_successor_handoff_is_on_origin() {
    let origin = Origin::new();
    origin.push_handoff(NEXT, "session_02next", Some(OURS));
    origin.dirty();
    let before = origin.snapshot();
    origin.silent(Some("cse_01abc"), &[]);
    origin.unchanged(&before);
    assert!(origin.log("cm.log").is_empty());
    let picks = origin.log("pick.log");
    assert_eq!(picks.len(), 2, "{picks:?}");
    assert_eq!(picks[0], local_pick("session_01abc"));
    assert!(origin_pick(&origin.tmpdir(), &picks[1]), "{picks:?}");
    origin.tmp_is_empty();
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(1));
    origin.tmp_is_empty();
}

#[test]
fn the_built_cm_tick_stays_bound_when_an_added_handoff_supersedes_another_file() {
    let origin = Origin::new();
    origin.push_handoff("coordinator-2026-11-20T0900Z.md", "session_00old", None);
    origin.git(&origin.ct(), &["pull", "-q", "--rebase", "origin", "main"]);
    origin.push_handoff(
        "coordinator-2026-11-25T1200Z.md",
        "session_00old",
        Some("coordinator-2026-11-20T0900Z.md"),
    );
    origin.clear_logs();
    origin.acts(Some("cse_01abc"));
    let picks = origin.log("pick.log");
    assert!(origin_pick(&origin.tmpdir(), &picks[1]), "{picks:?}");
    origin.tmp_is_empty();
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(0));
}

#[test]
fn the_built_cm_tick_stays_bound_for_a_newer_handoff_of_this_lineage_on_origin() {
    let origin = Origin::new();
    origin.push_handoff(NEXT, "session_01abc", Some(OURS));
    origin.acts(Some("cse_01abc"));
    let picks = origin.log("pick.log");
    assert!(origin_pick(&origin.tmpdir(), &picks[1]), "{picks:?}");
    origin.tmp_is_empty();
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(0));
}

#[test]
fn the_built_cm_tick_reads_origin_into_tmp_for_an_empty_tmpdir_and_refuses_a_relative_one() {
    let origin = Origin::new();
    origin.push_handoff(NEXT, "session_01abc", Some(OURS));
    let before = origin.snapshot();
    let tick = |tmpdir: &str, check: bool| {
        let mut command = origin.command(env!("CARGO_BIN_EXE_cm"));
        command
            .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")
            .env("TMPDIR", tmpdir)
            .current_dir(origin.ct())
            .args(["tick", "--cm"])
            .arg(origin.cm());
        if check {
            command.arg("--check-bound");
        }
        command.output().unwrap()
    };
    let output = tick("", false);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n", PRINTED.join("\n"))
    );
    assert!(
        origin_pick(Path::new("/tmp"), &origin.log("pick.log")[1]),
        "{:?}",
        origin.log("pick.log")
    );
    assert_eq!(tick("", true).status.code(), Some(0));
    for tmpdir in ["tmp", "."] {
        let output = tick(tmpdir, false);
        assert_eq!(output.status.code(), Some(0), "{tmpdir}: {output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{tmpdir}: {output:?}"
        );
        assert_eq!(tick(tmpdir, true).status.code(), Some(1), "{tmpdir}");
    }
    origin.unchanged(&before);
    assert!(!origin.ct().join("tmp").exists());
}

#[test]
fn the_built_cm_tick_fails_closed_when_origins_handoffs_cannot_be_extracted() {
    let origin = Origin::new();
    origin.push_handoff(NEXT, "session_01abc", Some(OURS));
    let before = origin.snapshot();
    let failing = origin.path("failing");
    script(&failing.join("tar"), "exit 1\n");
    let file = origin.path("a-file");
    fs::write(&file, "").unwrap();
    let cases: [(&str, OsString, PathBuf); 3] = [
        (
            "a tar that fails",
            format!("{}:/usr/bin:/bin", failing.display()).into(),
            origin.tmpdir(),
        ),
        (
            "a missing TMPDIR",
            "/usr/bin:/bin".into(),
            origin.path("nowhere"),
        ),
        ("a TMPDIR that is a file", "/usr/bin:/bin".into(), file),
    ];
    for (case, path, tmpdir) in cases {
        for check in [false, true] {
            let mut command = origin.command(env!("CARGO_BIN_EXE_cm"));
            command
                .env("PATH", &path)
                .env("TMPDIR", &tmpdir)
                .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")
                .args(["tick", "--cm"])
                .arg(origin.cm())
                .arg("--checkout")
                .arg(origin.ct());
            if check {
                command.arg("--check-bound");
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(i32::from(check)),
                "{case}: {output:?}"
            );
            assert!(output.stdout.is_empty(), "{case}: {output:?}");
            assert!(output.stderr.is_empty(), "{case}: {output:?}");
        }
    }
    origin.tmp_is_empty();
    assert!(origin.log("cm.log").is_empty());
    origin.unchanged(&before);
    // The same session binds once the extract works: the folder holds this lineage's newer head.
    origin.acts(Some("cse_01abc"));
}

#[test]
fn the_built_cm_tick_check_bound_is_false_without_exactly_one_live_epoch() {
    let origin = Origin::new();
    let second = origin.ct().join("store/epochs/second/EPOCH.md");
    write(&second, "---\nstatus: PLANNING\n---\n");
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(1));
    write(&second, "---\nstatus: CLOSED\n---\n");
    write(
        &origin.ct().join(EPOCH).join("EPOCH.md"),
        "---\nstatus: CLOSED\n---\n",
    );
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(1));
    assert!(origin.log("pick.log").is_empty(), "no bound check ran");
    assert!(origin.log("cm.log").is_empty());
}

#[test]
fn the_built_cm_tick_check_bound_answers_by_exit_status_only() {
    let origin = Origin::new();
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(0));
    assert_eq!(origin.check_bound(Some("cse_02next")), Some(1));
    write(
        &origin.ct().join(EPOCH).join("handoff").join(OURS),
        &format!("{}stale\n", handoff("session_01abc", None)),
    );
    assert_eq!(
        origin.check_bound(Some("cse_01abc")),
        Some(0),
        "stale binds"
    );
    assert!(
        origin.log("cm.log").is_empty(),
        "--cm is accepted, never run"
    );
    assert_eq!(
        origin.log("pick.log"),
        [
            local_pick("session_01abc"),
            local_pick("session_01abc"),
            local_pick("session_02next"),
            local_pick("session_02next"),
            local_pick("session_01abc"),
            local_pick("session_01abc"),
        ]
    );
}

#[test]
fn the_built_cm_tick_binds_a_stale_head_at_any_age() {
    let origin = Origin::new();
    write(
        &origin.ct().join(EPOCH).join("handoff").join(OURS),
        &format!("{}stale\n", handoff("session_01abc", None)),
    );
    origin.acts(Some("cse_01abc"));
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(0));
}

#[test]
fn the_built_cm_tick_counts_a_failed_fetch_as_bound() {
    let origin = Origin::new();
    origin.git(
        &origin.ct(),
        &["remote", "set-url", "origin", "../no-such-origin.git"],
    );
    origin.acts(Some("cse_01abc"));
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(0));
}

#[test]
fn the_built_cm_tick_is_silent_without_a_session_id() {
    let origin = Origin::new();
    for session in [None, Some("")] {
        origin.silent(session, &[]);
        assert_eq!(origin.check_bound(session), Some(1));
    }
    assert!(origin.log("pick.log").is_empty());
    assert!(origin.log("cm.log").is_empty());
}

#[test]
fn the_built_cm_tick_is_silent_off_main() {
    let origin = Origin::new();
    origin.git(&origin.ct(), &["checkout", "-q", "-b", "feature"]);
    origin.silent(Some("cse_01abc"), &[]);
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(1));
    assert!(origin.log("pick.log").is_empty());
    assert!(origin.log("cm.log").is_empty());
}

#[test]
fn the_built_cm_tick_takes_the_lineage_from_the_session_id_without_cse() {
    let origin = Origin::new();
    for session in ["cse_01abc", "01abc"] {
        origin.clear_logs();
        origin.acts(Some(session));
        assert_eq!(origin.log("pick.log")[0], local_pick("session_01abc"));
        assert_eq!(origin.check_bound(Some(session)), Some(0));
    }
    origin.clear_logs();
    origin.silent(Some("cse_cse_01abc"), &[]);
    assert_eq!(origin.log("pick.log"), [local_pick("session_cse_01abc")]);
}

#[test]
fn the_built_cm_tick_unbound_with_a_bad_heartbeat_deadline_prints_nothing() {
    let origin = Origin::new();
    let tick = |session: Option<&str>| {
        let mut command = origin.command(env!("CARGO_BIN_EXE_cm"));
        if let Some(session) = session {
            command.env("CLAUDE_CODE_REMOTE_SESSION_ID", session);
        }
        command
            .env("HEARTBEAT_DEADLINE", "soon")
            .args(["tick", "--cm"])
            .arg(origin.cm())
            .arg("--checkout")
            .arg(origin.ct())
            .output()
            .unwrap()
    };
    let output = tick(None);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "{output:?}"
    );
    assert_eq!(origin.check_bound(None), Some(1));
    assert_eq!(
        String::from_utf8(tick(Some("cse_01abc")).stdout).unwrap(),
        format!(
            "heartbeat: HEARTBEAT_DEADLINE=soon is not a whole number of seconds; using 230\n{}\n",
            PRINTED.join("\n")
        ),
        "the line moved behind the bound check"
    );
}

// SystemProc with the fixture's environment in place of the test runner's, for in-process
// ticks whose phases run real git.
struct Isolated<'a>(&'a Origin);

impl Proc for Isolated<'_> {
    fn run(&self, cmd: &Cmd) -> cm::tick::Result<Ran> {
        let mut cmd = cmd.clone();
        cmd.env_remove
            .extend(std::env::vars_os().map(|(name, _)| name));
        let own = std::mem::take(&mut cmd.env);
        cmd.env = self
            .0
            .vars()
            .into_iter()
            .map(|(name, value)| (name.into(), value.into_os_string()))
            .chain(own)
            .collect();
        spawn(&cmd)
    }
}

struct Turn {
    out: Vec<String>,
    phases: Vec<String>,
    executed: usize,
}

type Sync<'a> = dyn Fn(&Ctx, Mode, &RefCell<Vec<String>>) -> Report + 'a;

fn one_turn(origin: &Origin, sync: &Sync) -> Turn {
    let phased = RefCell::new(Vec::<String>::new());
    let gitsync = |ctx: &Ctx, mode: Mode| {
        phased.borrow_mut().push(format!("gitsync {mode:?}"));
        sync(ctx, mode, &phased)
    };
    let phase = |name: &'static str| {
        let phased = &phased;
        move |_: &Ctx| {
            phased.borrow_mut().push(name.to_owned());
            Report::default()
        }
    };
    let (pin, directive, collect, claude_md) = (
        phase("pin"),
        phase("directive"),
        phase("collect"),
        phase("claude-md"),
    );
    let phases = Phases {
        gitsync: &gitsync,
        pin: &pin,
        directive: &directive,
        collect: &collect,
        claude_md: &claude_md,
    };
    let (executor, seen) = Record::new(done());
    let registry = Registry::new().with("launchable-not-launched", executor);
    let checkout = origin.ct();
    let cm = origin.cm();
    let tmpdir = origin.tmpdir();
    let env = env(&[
        ("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc"),
        ("TMPDIR", tmpdir.to_str().unwrap()),
    ]);
    let mut out = Vec::new();
    beat::run(
        &Beat {
            checkout: &checkout,
            proc: &Isolated(origin),
            env: &env,
            registry: &registry,
            phases: &phases,
            cm: cm.as_os_str(),
            started: Instant::now(),
        },
        &mut |line| out.push(line),
    );
    Turn {
        out,
        phases: phased.into_inner(),
        executed: seen.calls().len(),
    }
}

// heartbeat.sh's git_sync, as housekeeping ports it: commit, pull, and push only while
// `still_bound` holds. A successor's handoff reaches origin just before this pull.
fn pulled_successor(origin: &Origin) -> impl Fn(&Ctx, Mode, &RefCell<Vec<String>>) -> Report + '_ {
    |ctx: &Ctx, _: Mode, phased: &RefCell<Vec<String>>| {
        origin.push_handoff(NEXT, "session_02next", Some(OURS));
        let git = |args: &[&str]| ctx.run(Cmd::new("git").args(args)).unwrap();
        if !git(&["status", "--porcelain"]).stdout.is_empty() {
            assert!(git(&["add", "-A"]).exit.success());
            assert!(
                git(&["commit", "-qm", "chore(mnemosyne): heartbeat sync"])
                    .exit
                    .success()
            );
        }
        assert!(
            git(&["pull", "-q", "--rebase", "--autostash", "origin", "main"])
                .exit
                .success()
        );
        if !ctx.still_bound() {
            phased.borrow_mut().push("retired: no push".to_owned());
            return Report::default();
        }
        assert!(git(&["push", "-q", "origin", "main"]).exit.success());
        Report::default()
    }
}

#[test]
fn a_successor_handoff_pulled_this_turn_retires_the_session_before_any_executor() {
    let origin = Origin::new();
    let sync = pulled_successor(&origin);
    let turn = one_turn(&origin, &sync);
    assert_eq!(turn.out, Vec::<String>::new());
    assert_eq!(turn.phases, ["gitsync Always", "retired: no push"]);
    assert_eq!(turn.executed, 0);
    assert!(origin.log("cm.log").is_empty(), "no cm next");
    assert!(
        origin.ct().join(EPOCH).join("handoff").join(NEXT).is_file(),
        "the pull brought the successor's handoff"
    );
}

#[test]
fn a_successor_arriving_with_the_pull_gets_no_push() {
    let origin = Origin::new();
    origin.dirty();
    let sync = pulled_successor(&origin);
    let turn = one_turn(&origin, &sync);
    assert_eq!(turn.out, Vec::<String>::new());
    assert_eq!(turn.phases, ["gitsync Always", "retired: no push"]);
    let ct = origin.ct();
    assert_eq!(
        origin.git(&ct, &["log", "-1", "--format=%s"]),
        "chore(mnemosyne): heartbeat sync\n",
        "committed and rebased onto the successor's handoff"
    );
    assert_eq!(
        origin.git(
            &origin.path("origin.git"),
            &["log", "-1", "--format=%s", "main"]
        ),
        format!("handoff {NEXT}\n"),
        "origin never got the commit"
    );
    assert!(origin.log("cm.log").is_empty());

    // The same phase pushes while this session stays the head: a handoff of its own lineage.
    let bound = Origin::new();
    bound.dirty();
    let sync = |ctx: &Ctx, mode: Mode, phased: &RefCell<Vec<String>>| {
        let git = |args: &[&str]| ctx.run(Cmd::new("git").args(args)).unwrap();
        if mode == Mode::Always {
            bound.push_handoff(NEXT, "session_01abc", Some(OURS));
            assert!(git(&["add", "-A"]).exit.success());
            assert!(
                git(&["commit", "-qm", "chore(mnemosyne): heartbeat sync"])
                    .exit
                    .success()
            );
        }
        assert!(
            git(&["pull", "-q", "--rebase", "origin", "main"])
                .exit
                .success()
        );
        assert!(ctx.still_bound());
        phased.borrow_mut().push("pushed".to_owned());
        assert!(git(&["push", "-q", "origin", "main"]).exit.success());
        Report::default()
    };
    let turn = one_turn(&bound, &sync);
    assert_eq!(turn.out, PRINTED[1..]);
    assert_eq!(turn.executed, 1);
    assert_eq!(
        turn.phases,
        [
            "gitsync Always",
            "pushed",
            "pin",
            "directive",
            "collect",
            "gitsync IfDirty",
            "pushed",
            "claude-md"
        ]
    );
    assert_eq!(
        bound.git(
            &bound.path("origin.git"),
            &["log", "-2", "--format=%s", "main"]
        ),
        "chore(mnemosyne): heartbeat sync\nhandoff coordinator-2026-11-26T0100Z.md\n"
    );
}

#[test]
fn a_successor_on_origin_after_gitsync_stops_the_tick_at_the_re_check() {
    let origin = Origin::new();
    let sync = |_: &Ctx, _: Mode, _: &RefCell<Vec<String>>| {
        origin.push_handoff(NEXT, "session_02next", Some(OURS));
        Report {
            lines: vec!["gitsync printed".into()],
            failed: true,
        }
    };
    let turn = one_turn(&origin, &sync);
    assert_eq!(
        turn.out,
        ["gitsync printed"],
        "the phase's lines stay; no wall time, no later line"
    );
    assert_eq!(turn.phases, ["gitsync Always"]);
    assert_eq!(turn.executed, 0);
    assert!(origin.log("cm.log").is_empty());
    let picks = origin.log("pick.log");
    assert_eq!(picks.len(), 3, "{picks:?}");
    assert_eq!(
        picks[..2],
        [local_pick("session_01abc"), local_pick("session_01abc")]
    );
    assert!(
        origin_pick(&origin.tmpdir(), &picks[2]),
        "origin's handoff folder, extracted in the tick's TMPDIR: {picks:?}"
    );
    origin.tmp_is_empty();
    assert_eq!(
        origin.git(&origin.ct(), &["log", "-1", "--format=%s"]),
        "epoch\n",
        "nothing pulled: origin's own view decided"
    );
}

#[test]
fn the_built_cm_tick_reads_a_non_ascii_coordinator_handoff_name_on_origin() {
    // Without -z, git quotes this name ("...coordinator-\303\251.md") and heartbeat.sh's
    // `\.md$` never matched it.
    let origin = Origin::new();
    origin.push_handoff(
        "coordinator-2026-11-26T0100Z-\u{e9}.md",
        "session_02next",
        Some(OURS),
    );
    origin.git(&origin.ct(), &["fetch", "-q", "origin", "main"]);
    let quoted = origin.git(
        &origin.ct(),
        &[
            "diff",
            "--name-only",
            "--diff-filter=A",
            "HEAD",
            "origin/main",
        ],
    );
    assert!(quoted.ends_with(".md\"\n"), "{quoted}");
    origin.silent(Some("cse_01abc"), &[]);
    assert!(origin_pick(&origin.tmpdir(), &origin.log("pick.log")[1]));
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(1));
}

fn slow_fetch(slow: Rc<std::cell::Cell<bool>>, delay: Duration) -> FakeProc {
    let answers = Answers::new(0, act(Vec::new()));
    FakeProc::new(move |cmd| {
        if slow.get() && cmd.argv.get(1).is_some_and(|arg| arg == "fetch") {
            std::thread::sleep(delay);
        }
        Ok(answers.answer(cmd))
    })
}

#[test]
fn still_bound_runs_the_whole_check_afresh_and_spends_none_of_the_budget() {
    let ct = Ct::new();
    let env = bound_env();
    let ctx = |proc: &'_ FakeProc, budget: Duration| -> (Vec<String>, bool, bool) {
        let ctx = Ctx::new(
            ct.path(),
            Path::new(EPOCH),
            proc,
            &env,
            OsStr::new("cm"),
            budget,
        );
        let bound = ctx.still_bound();
        (proc.log(), bound, ctx.timed_out())
    };

    let successor = Rc::new(std::cell::Cell::new(false));
    let proc = {
        let successor = successor.clone();
        FakeProc::new(move |cmd| {
            let mut answers = Answers::new(0, "");
            if successor.get() {
                answers.pick = Ran::code(3, "missing other-lineage x\n");
            }
            Ok(answers.answer(cmd))
        })
    };
    let one = Ctx::new(
        ct.path(),
        Path::new(EPOCH),
        &proc,
        &env,
        OsStr::new("cm"),
        Duration::from_secs(60),
    );
    assert!(one.still_bound());
    successor.set(true);
    assert!(!one.still_bound(), "nothing of the first check is kept");
    assert_eq!(
        proc.log(),
        [BOUND_CHECK.as_slice(), &BOUND_CHECK[..2]].concat()
    );

    let proc = Answers::new(0, "").proc();
    assert_eq!(
        ctx(&proc, Duration::ZERO),
        (BOUND_CHECK.map(str::to_owned).to_vec(), true, false),
        "a spent budget neither stops the check nor is marked by it"
    );
    for call in proc.calls() {
        assert_eq!(call.timeout, beat::FRAME_CAP, "{}", call.line());
    }

    let slow = Rc::new(std::cell::Cell::new(true));
    let proc = slow_fetch(slow.clone(), Duration::from_millis(1500));
    let short = Ctx::new(
        ct.path(),
        Path::new(EPOCH),
        &proc,
        &env,
        OsStr::new("cm"),
        Duration::from_secs(1),
    );
    assert!(short.still_bound());
    let push = short
        .run(Cmd::new("git").args(["push", "-q", "origin", "main"]))
        .unwrap();
    assert_eq!(
        (push.exit, short.timed_out()),
        (Exit::Code(0), false),
        "a check longer than the budget leaves the push its budget"
    );

    let proc = slow_fetch(slow, Duration::from_secs(1));
    let long = Ctx::new(
        ct.path(),
        Path::new(EPOCH),
        &proc,
        &env,
        OsStr::new("cm"),
        Duration::from_secs(30),
    );
    let before = long.remaining();
    assert!(long.still_bound());
    let after = long.remaining();
    assert!(
        before.saturating_sub(after) < Duration::from_millis(500),
        "a 1 s check took {:?} of the budget",
        before.saturating_sub(after)
    );
}

#[test]
fn a_gitsync_that_calls_still_bound_reports_only_its_own_wall_time() {
    let ct = Ct::new();
    let slow = Rc::new(std::cell::Cell::new(false));
    let proc = slow_fetch(slow.clone(), Duration::from_millis(1500));
    let gitsync = |ctx: &Ctx, _: Mode| {
        slow.set(true);
        assert!(ctx.still_bound());
        slow.set(false);
        Report {
            lines: Vec::new(),
            failed: true,
        }
    };
    let phases = Phases {
        gitsync: &gitsync,
        ..QUIET
    };
    let out = tick_with(&ct, &proc, &bound_env(), &Registry::new(), &phases);
    assert_eq!(out.len(), 1, "{out:?}");
    let wall: f64 = out[0]
        .strip_prefix("tick: gitsync phase took ")
        .and_then(|rest| rest.strip_suffix(" s (budget 60 s)"))
        .and_then(|wall| wall.parse().ok())
        .unwrap_or_else(|| panic!("{out:?}"));
    assert!(wall < 1.0, "the 1.5 s check is not the phase's: {out:?}");
}

#[test]
fn the_built_cm_tick_fails_closed_when_export_ignore_hides_a_successor_handoff_on_origin() {
    let origin = Origin::new();
    let other = origin.path("other");
    write(
        &other.join(".gitattributes"),
        &format!("{EPOCH}/handoff/{NEXT} export-ignore\n"),
    );
    origin.push_handoff(NEXT, "session_02next", Some(OURS));
    let archived = origin
        .command("git")
        .current_dir(&other)
        .args(["archive", "HEAD", &format!("{EPOCH}/handoff")])
        .output()
        .unwrap();
    assert!(archived.status.success());
    let listed = String::from_utf8_lossy(&archived.stdout).into_owned();
    assert!(
        listed.contains(OURS) && !listed.contains(NEXT),
        "git archive drops it"
    );
    let before = origin.snapshot();
    origin.silent(Some("cse_01abc"), &[]);
    origin.unchanged(&before);
    assert_eq!(
        origin.log("pick.log"),
        [local_pick("session_01abc")],
        "the extract lacks a file origin's tree lists: no pick reads it"
    );
    origin.tmp_is_empty();
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(1));
}

#[test]
fn the_built_cm_tick_counts_a_successor_handoff_renamed_in_on_origin_as_added() {
    let origin = Origin::new();
    let ct = origin.ct();
    let notes = format!("{EPOCH}/handoff/pr-loop-2026-11-25T1200Z.md");
    let body: String = (0..30)
        .map(|n| format!("- open item {n}: reconcile against cm, the PR and the journal\n"))
        .collect();
    write(
        &ct.join(&notes),
        &format!(
            "# pr-loop handoff\n\nHandoff: epoch=eleemosynary-mnemosyne role=pr-loop lineage=session_02next written=2026-11-25T12:00:00+00:00\n\n{body}"
        ),
    );
    origin.git(&ct, &["add", "-A"]);
    origin.git(&ct, &["commit", "-qm", "pr-loop notes"]);
    origin.git(&ct, &["push", "-q", "origin", "main"]);
    let other = origin.path("other");
    origin.git(&other, &["pull", "-q", "--rebase", "origin", "main"]);
    let successor = format!("{EPOCH}/handoff/{NEXT}");
    origin.git(&other, &["mv", &notes, &successor]);
    write(
        &other.join(&successor),
        &format!("{}\n{body}", handoff("session_02next", Some(OURS))),
    );
    origin.git(&other, &["add", "-A"]);
    origin.git(&other, &["commit", "-qm", "successor"]);
    origin.git(&other, &["push", "-q", "origin", "main"]);
    origin.git(&ct, &["fetch", "-q", "origin", "main"]);
    let renamed = origin.git(
        &ct,
        &[
            "diff",
            "--name-status",
            "HEAD",
            "origin/main",
            "--",
            &format!("{EPOCH}/handoff/"),
        ],
    );
    assert!(renamed.starts_with('R'), "git reports a rename: {renamed}");
    let before = origin.snapshot();
    origin.silent(Some("cse_01abc"), &[]);
    origin.unchanged(&before);
    assert!(origin_pick(&origin.tmpdir(), &origin.log("pick.log")[1]));
    assert_eq!(origin.check_bound(Some("cse_01abc")), Some(1));
}
