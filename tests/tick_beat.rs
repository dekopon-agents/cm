use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cm::tick::TickError;
use cm::tick::act::{ActLine, Ctx, Env, Executor, Outcome, Registry, Report};
use cm::tick::beat::{self, Beat, Phases};
use cm::tick::gitsync::Mode;
use cm::tick::proc::{Cmd, Exit, FakeProc, Keep, OUTPUT_CAP, Proc, Ran, SystemProc, WHOLE_CAP};
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
}

impl Answers {
    fn new(exit: i32, stdout: impl Into<Vec<u8>>) -> Self {
        Answers {
            next: Ran::code(exit, stdout),
            branch: "main\n".to_owned(),
            pick: Ran::code(0, PICK_OK),
        }
    }

    fn answer(&self, cmd: &Cmd) -> Ran {
        match (
            cmd.program_name().as_str(),
            cmd.argv.get(1).and_then(|arg| arg.to_str()),
        ) {
            ("cm", Some("--json")) => self.next.clone(),
            ("git", Some("rev-parse")) => Ran::code(0, self.branch.clone()),
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
            "git rev-parse --abbrev-ref HEAD",
            "sh tools/cloud-env/handoff-pick.sh store/epochs/eleemosynary-mnemosyne coordinator --lineage session_01abc --max-age-hours 24",
            "cm --json next -C store/epochs/eleemosynary-mnemosyne --act",
        ],
        "the bound check, then the act list; no `cm sync`"
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
    let out = tick_with(&ct, &proc, &bound_env(), &Registry::new(), &phases);
    assert_eq!(
        out[0],
        "heartbeat: executors off (handoff-pick.sh for session_01abc: unbound x); every act line below is yours"
    );
    assert_eq!(
        *log.borrow(),
        [
            "gitsync Always",
            "pin",
            "directive",
            "git",
            "sh",
            "cm",
            "claude-md"
        ],
        "the bound check runs after the directives; collect only in the bound coordinator"
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
    let out = tick(&ct, &proc, &Env::default(), &Registry::new());
    assert_eq!(out[0], QUEUE_LINE, "printed in any session");
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
    let next = || Answers::new(1, act(vec![lines().remove(0)]));

    let (out, ran, proc) = run(next(), &bound_env());
    assert_eq!((out, ran), (Vec::<String>::new(), 1));
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
    for call in [by("git"), by("sh")] {
        assert_eq!(call.cwd.as_deref(), Some(ct.path()));
        assert_eq!(call.timeout, beat::FRAME_CAP);
    }

    let refused = |pick: &str| Answers {
        pick: Ran::code(1, pick),
        ..next()
    };
    let off = |why: &str| {
        vec![
            format!("heartbeat: executors off ({why}); every act line below is yours"),
            PRINTED[0].to_owned(),
        ]
    };
    let (out, ran, _) = run(
        refused("refused superseded store/epochs/eleemosynary-mnemosyne/handoff/x.md\n"),
        &bound_env(),
    );
    assert_eq!(
        (out, ran),
        (
            off(
                "handoff-pick.sh for session_01abc: refused superseded store/epochs/eleemosynary-mnemosyne/handoff/x.md"
            ),
            0
        )
    );
    let (out, ran, _) = run(refused(""), &bound_env());
    assert_eq!(
        (out, ran),
        (off("handoff-pick.sh for session_01abc: no answer"), 0)
    );
    let (out, ran, _) = run(refused("okay\nok second line\n"), &bound_env());
    assert_eq!(
        (out, ran),
        (off("handoff-pick.sh for session_01abc: okay"), 0)
    );
    let (out, ran, _) = run(
        Answers {
            pick: Ran::code(0, "ok a\nnot read\n"),
            ..next()
        },
        &env(&[("CLAUDE_CODE_REMOTE_SESSION_ID", "01abc")]),
    );
    assert_eq!((out, ran), (Vec::<String>::new(), 1));

    for session in [None, Some("")] {
        let env: Env = session
            .map(|id| ("CLAUDE_CODE_REMOTE_SESSION_ID", id))
            .into_iter()
            .collect();
        let (out, ran, proc) = run(next(), &env);
        assert_eq!(
            (out, ran),
            (off("no CLAUDE_CODE_REMOTE_SESSION_ID, so no lineage"), 0)
        );
        assert!(!proc.log().iter().any(|call| call.starts_with("sh ")));
    }

    let (out, ran, proc) = run(
        Answers {
            branch: "feature\n".into(),
            ..next()
        },
        &bound_env(),
    );
    assert_eq!((out, ran), (off("the checkout is not on main"), 0));
    assert!(!proc.log().iter().any(|call| call.starts_with("sh ")));

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
        off("handoff-pick.sh for session_01abc: no answer")
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
    let slow = proc.calls().pop().unwrap();
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

    let proc = FakeProc::new(|cmd| {
        Err(TickError::Spawn {
            program: cmd.program_name(),
            source: std::io::ErrorKind::NotFound.into(),
        })
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
    assert_eq!(beat::live_epochs(ct.path()), [Path::new(EPOCH)]);
    ct.write("store/epochs/.hidden/EPOCH.md", "status: WORKING\n");
    ct.write("store/epochs/indented/EPOCH.md", "  status: WORKING\n");
    ct.write("store/epochs/other/EPOCH.md", "status: OPEN\n");
    fs::create_dir_all(ct.path().join("store/epochs/folder/EPOCH.md")).unwrap();
    assert_eq!(beat::live_epochs(ct.path()), [Path::new(EPOCH)]);
    ct.write(
        "store/epochs/later/EPOCH.md",
        "---\ntitle: x\n---\nstatus: PLANNING-ish\r\n",
    );
    assert_eq!(
        beat::live_epochs(ct.path()),
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
    assert!(beat::live_epochs(empty.path()).is_empty());
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
fn an_act_list_too_large_to_keep_is_one_named_line() {
    let ct = Ct::new();
    let proc = Answers {
        next: Ran {
            exit: Exit::TooLarge,
            ..Ran::code(0, "{\"actionable\": [")
        },
        ..Answers::new(0, "")
    }
    .proc();
    let out = tick(&ct, &proc, &bound_env(), &Registry::new());
    assert_eq!(
        out,
        [
            "heartbeat: cm --json next --act printed more than 64 MiB and was killed; no act line was read"
        ]
    );
    let next = proc
        .calls()
        .into_iter()
        .find(|cmd| cmd.program_name() == "cm")
        .unwrap();
    assert_eq!(
        (next.keep_stdout, next.keep_stderr),
        (Keep::Whole, Keep::Head)
    );
    for frame in proc.calls().iter().filter(|cmd| cmd.program_name() != "cm") {
        assert_eq!(frame.keep_stdout, Keep::Head, "{}", frame.line());
    }
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

    fn command(&self, session: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOME", self.ct.path())
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
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "heartbeat: executors off (handoff-pick.sh for session_01abc: refused superseded x); every act line below is yours\nattempt-ended 01-a/u/c: cm attempt 01-a/u/c restart\nmissing-kickoff 01-a/u/b: cm amend 01-a/u/b\n"
    );
    let output = refused
        .command("cse_01abc")
        .args(["--json", "tick", "--cm", "cm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json,
        json!({"command": "tick", "ok": true, "violations": [], "cm": "cm", "lines": [
            "heartbeat: executors off (handoff-pick.sh for session_01abc: refused superseded x); every act line below is yours",
            "attempt-ended 01-a/u/c: cm attempt 01-a/u/c restart",
            "missing-kickoff 01-a/u/b: cm amend 01-a/u/b",
        ]})
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
    script(&bin.join("git"), "echo main\n");
    let mut out = Vec::new();
    beat::run(
        &Beat {
            checkout: ct.path(),
            proc: &Bin(bin.clone()),
            env: &Env::default(),
            registry: &Registry::new(),
            phases: &QUIET,
            cm: cm.as_os_str(),
            started: Instant::now(),
        },
        &mut |line| out.push(line),
    );
    assert_eq!(out.len(), 1 + actionable.len(), "{:?}", out.first());
    assert_eq!(
        out[0],
        "heartbeat: executors off (no CLAUDE_CODE_REMOTE_SESSION_ID, so no lineage); every act line below is yours"
    );
    for (printed, json) in out[1..].iter().zip(&actionable) {
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
fn a_malformed_coordinator_handoff_is_named_once_whatever_the_act_list_holds() {
    let ct = Ct::new();
    let bad = "store/epochs/eleemosynary-mnemosyne/handoff/coordinator-bad.md";
    let named = format!("heartbeat: malformed coordinator handoff: {bad}; fix its Handoff: line");
    let refused = |actionable: Vec<Value>, pick: String| {
        let proc = Answers {
            pick: Ran::code(1, format!("{pick}\n")),
            ..Answers::new(1, act(actionable))
        }
        .proc();
        tick(&ct, &proc, &bound_env(), &Registry::new())
    };
    assert_eq!(
        refused(Vec::new(), format!("refused malformed {bad}")),
        [named.as_str()]
    );
    assert_eq!(
        refused(vec![lines().remove(0)], format!("refused malformed {bad}")),
        [
            named,
            format!(
                "heartbeat: executors off (handoff-pick.sh for session_01abc: refused malformed {bad}); every act line below is yours"
            ),
            PRINTED[0].to_owned(),
        ]
    );
    assert_eq!(
        refused(
            Vec::new(),
            format!("refused missing-fields=prompt bad-fields=stamp_sha {bad}")
        ),
        [format!(
            "heartbeat: malformed coordinator handoff: {bad} (missing-fields=prompt bad-fields=stamp_sha); fix its Handoff: line"
        )]
    );
    assert_eq!(
        refused(Vec::new(), format!("refused superseded-by {bad}")),
        Vec::<String>::new(),
        "only a malformed handoff is named"
    );
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
