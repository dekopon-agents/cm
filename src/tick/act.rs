use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::tick::Result;
use crate::tick::proc::{Cmd, Exit, Proc, Ran, WHOLE_CAP};

#[derive(Debug, Clone, PartialEq)]
pub struct ActLine {
    pub json: Value,
    pub lane: Option<String>,
}

impl ActLine {
    pub fn read(json: Value, epoch_dir: &Path) -> Self {
        let own = json
            .get("lane")
            .and_then(Value::as_str)
            .filter(|lane| !lane.is_empty())
            .map(str::to_owned);
        let lane = own.or_else(|| {
            let target = json
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (unit, step) = unit_and_step(target);
            let state = read_json(&epoch_dir.join(unit).join(".cm/state.json"))?;
            state["steps"][step]["lane"]
                .as_str()
                .filter(|lane| !lane.is_empty())
                .map(str::to_owned)
        });
        ActLine { json, lane }
    }

    pub fn kind(&self) -> String {
        jq_raw(self.json.get("kind"))
    }

    pub fn target(&self) -> String {
        let target = self
            .json
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match self.json.get("part").and_then(Value::as_str) {
            Some(part) => format!("{target}#{part}"),
            None => target.to_owned(),
        }
    }

    pub fn command(&self) -> String {
        jq_raw(self.json.get("command"))
    }

    pub fn text(&self) -> String {
        format!("{} {}: {}", self.kind(), self.target(), self.command())
    }

    pub fn field(&self, name: &str) -> Option<&str> {
        self.json.get(name).and_then(Value::as_str)
    }

    pub fn stdin(&self) -> Vec<u8> {
        let mut bytes = self.json.to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }
}

fn jq_raw(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => "null".to_owned(),
    }
}

fn unit_and_step(target: &str) -> (&str, &str) {
    let target = target.split('#').next().unwrap_or_default();
    if !target.contains('/') {
        return (target, target);
    }
    let mut fields = target.splitn(4, '/');
    let first = fields.next().unwrap_or_default();
    let second = fields.next().unwrap_or_default();
    let step = fields.next().unwrap_or_default();
    (&target[..first.len() + 1 + second.len()], step)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env(BTreeMap<String, String>);

impl Env {
    pub fn system() -> Self {
        Env::from_os(std::env::vars_os())
    }

    pub(crate) fn from_os(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Env(vars
            .into_iter()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect())
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub fn non_empty(&self, name: &str) -> Option<&str> {
        self.get(name).filter(|value| !value.is_empty())
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Env {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Self {
        Env(pairs
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect())
    }
}

pub const MAX_BUDGET: Duration = Duration::from_secs(24 * 60 * 60);

pub trait Clock {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub struct Ctx<'a> {
    pub checkout: &'a Path,
    pub epoch: &'a Path,
    pub proc: &'a dyn Proc,
    pub env: &'a Env,
    pub cm: &'a OsStr,
    clock: &'a dyn Clock,
    deadline: Cell<Instant>,
    checking: Cell<Duration>,
    timed_out: Cell<bool>,
}

impl<'a> Ctx<'a> {
    pub fn new(
        checkout: &'a Path,
        epoch: &'a Path,
        proc: &'a dyn Proc,
        env: &'a Env,
        cm: &'a OsStr,
        budget: Duration,
    ) -> Self {
        let now = Instant::now();
        Ctx {
            checkout,
            epoch,
            proc,
            env,
            cm,
            clock: &SystemClock,
            deadline: Cell::new(now + budget.min(MAX_BUDGET)),
            checking: Cell::new(Duration::ZERO),
            timed_out: Cell::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn clocked(mut self, clock: &'a dyn Clock, budget: Duration) -> Self {
        let now = clock.now();
        self.clock = clock;
        self.deadline.set(now + budget);
        self
    }

    pub(crate) fn now(&self) -> Instant {
        self.clock.now()
    }

    pub fn epoch_dir(&self) -> PathBuf {
        self.checkout.join(self.epoch)
    }

    pub fn slug(&self) -> String {
        self.epoch
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn remaining(&self) -> Duration {
        self.deadline
            .get()
            .saturating_duration_since(self.clock.now())
    }

    pub fn timed_out(&self) -> bool {
        self.timed_out.get()
    }

    // gitsync calls this after each pull, so nothing of an earlier check is kept. The check's
    // time, whatever its answer and also when it panics, is not the calling phase's.
    pub fn still_bound(&self) -> bool {
        let _exempt = Exempt {
            ctx: self,
            started: self.clock.now(),
        };
        crate::tick::beat::bound(self.checkout, self.epoch, self.proc, self.env)
    }

    fn exempt(&self, took: Duration) {
        let deadline = self.deadline.get();
        self.deadline
            .set(deadline.checked_add(took).unwrap_or(deadline));
        self.checking.set(self.checking.get().saturating_add(took));
    }

    pub(crate) fn checking(&self) -> Duration {
        self.checking.get()
    }

    pub fn run(&self, mut cmd: Cmd) -> Result<Ran> {
        let remaining = self.remaining();
        if remaining.is_zero() {
            self.timed_out.set(true);
            return Ok(Ran::timed_out());
        }
        let cut = remaining <= cmd.timeout;
        cmd.timeout = cmd.timeout.min(remaining);
        if cmd.cwd.is_none() {
            cmd.cwd = Some(self.checkout.to_path_buf());
        }
        let ran = self.proc.run(&cmd)?;
        if ran.exit == Exit::TimedOut && cut {
            self.timed_out.set(true);
        }
        Ok(ran)
    }

    pub fn pause(&self, pause: Duration) {
        self.proc.pause(pause.min(self.remaining()));
    }

    pub fn cm_cmd(&self, sub: &str, args: &[&str]) -> Cmd {
        Cmd::new(self.cm)
            .arg(sub)
            .arg("-C")
            .arg(self.epoch)
            .args(args)
    }

    pub fn cm(&self, sub: &str, args: &[&str]) -> std::result::Result<Ran, String> {
        self.cm_run(sub, self.cm_cmd(sub, args))
    }

    pub fn cm_run(&self, sub: &str, cmd: Cmd) -> std::result::Result<Ran, String> {
        match self.run(cmd) {
            Ok(ran) if ran.exit.success() => Ok(ran),
            Ok(ran) if ran.exit == Exit::TooLarge => Err(format!(
                "cm {sub}'s output was too large to keep in memory ({} MiB at most) and it was killed",
                WHOLE_CAP >> 20
            )),
            Ok(ran) => Err(format!("cm {sub} failed: {}", last_line(&ran))),
            Err(error) => Err(format!("cm {sub} failed: {error}")),
        }
    }

    pub fn step_state(&self, target: &str) -> std::result::Result<Value, String> {
        let parts: Vec<&str> = target
            .split('#')
            .next()
            .unwrap_or_default()
            .split('/')
            .collect();
        let [area, unit, step] = parts[..] else {
            return Err(format!("{target} is not <NN-name>/<unit>/<step>"));
        };
        let state = self
            .epoch_dir()
            .join(area)
            .join(unit)
            .join(".cm/state.json");
        read_json(&state)
            .and_then(|state| state.get("steps")?.get(step).cloned())
            .ok_or_else(|| format!("no cm state for {target}"))
    }

    pub fn pull(&self, line: &ActLine, step: &Value) -> std::result::Result<Pull, String> {
        fn given(value: &Value) -> Option<&str> {
            value.as_str().filter(|url| !url.is_empty())
        }
        let url = given(&line.json["pr"])
            .or_else(|| given(&step["evidence"]["pr"]))
            .or_else(|| given(&step["pr"]))
            .unwrap_or_default();
        let target = jq_raw(line.json.get("target"));
        let (repo, number) = pull_url(url).ok_or_else(|| format!("{target} records no PR URL"))?;
        let observed = read_json(&self.epoch_dir().join(".cm/pulls.json"))
            .and_then(|pulls| pulls.get(url).cloned())
            .filter(truthy)
            .unwrap_or_else(|| Value::Object(Map::new()));
        Ok(Pull {
            repo,
            number,
            url: url.to_owned(),
            observed,
        })
    }
}

struct Exempt<'c, 'a> {
    ctx: &'c Ctx<'a>,
    started: Instant,
}

impl Drop for Exempt<'_, '_> {
    fn drop(&mut self) {
        let took = self.ctx.clock.now().saturating_duration_since(self.started);
        self.ctx.exempt(took);
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pull {
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub observed: Value,
}

fn pull_url(url: &str) -> Option<(String, u64)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo, pull, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let name = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    let number = number
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| number.parse().ok())
        .flatten()?;
    (parts.next().is_none() && name(owner) && name(repo) && pull == "pull")
        .then(|| (format!("{owner}/{repo}"), number))
}

pub fn last_line(ran: &Ran) -> String {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    // Python's `str.splitlines` boundaries.
    let breaks = [
        '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
    ];
    match text.split(breaks).rfind(|line| !line.trim().is_empty()) {
        Some(line) => line.chars().take(300).collect(),
        None => format!("exit {}", ran.exit.shell_status()),
    }
}

pub(crate) fn clip(text: &str) -> &str {
    &text[..text.floor_char_boundary(300)]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done(Vec<String>),
    Left(String),
    Failed(String),
}

pub trait Executor {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome;
}

#[derive(Default)]
pub struct Registry {
    executors: BTreeMap<String, Box<dyn Executor>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    pub fn with(mut self, key: &str, executor: impl Executor + 'static) -> Self {
        self.executors.insert(key.to_owned(), Box::new(executor));
        self
    }

    pub fn lookup(&self, kind: &str, lane: Option<&str>) -> Option<(&str, &dyn Executor)> {
        let laned = lane.map(|lane| format!("{kind}.{lane}"));
        laned
            .iter()
            .map(String::as_str)
            .chain([kind])
            .find_map(|key| self.executors.get_key_value(key))
            .map(|(key, executor)| (key.as_str(), executor.as_ref()))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.executors.keys().map(String::as_str)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<String>,
    pub failed: bool,
}

// A clock that moves only when a test moves it, and the bound check's children answering
// bound, with `answer` taking over any command it returns a `Ran` for.
#[cfg(test)]
pub(crate) struct Ticking(pub std::rc::Rc<Cell<Instant>>);

#[cfg(test)]
impl Clock for Ticking {
    fn now(&self) -> Instant {
        self.0.get()
    }
}

#[cfg(test)]
pub(crate) fn bound_children(
    clock: &std::rc::Rc<Cell<Instant>>,
    answer: impl Fn(&str, &Cell<Instant>) -> Option<Ran> + 'static,
) -> crate::tick::proc::FakeProc {
    let clock = clock.clone();
    crate::tick::proc::FakeProc::new(move |cmd| {
        let word = match cmd.program_name().as_str() {
            "git" => cmd.argv[1].to_string_lossy().into_owned(),
            program => program.to_owned(),
        };
        Ok(
            answer(&word, &clock).unwrap_or_else(|| match word.as_str() {
                "rev-parse" => Ran::code(0, "main\n"),
                "sh" => Ran::code(0, "ok store/epochs/e/handoff/coordinator-1.md\n"),
                _ => Ran::code(0, ""),
            }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::rc::Rc;

    fn session() -> Env {
        [("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_1")]
            .into_iter()
            .collect()
    }

    fn later(clock: &Cell<Instant>, by: u64) {
        clock.set(clock.get() + Duration::from_secs(by));
    }

    #[test]
    fn a_check_longer_than_the_budget_spends_none_of_it() {
        let clock = Rc::new(Cell::new(Instant::now()));
        let proc = bound_children(&clock, |word, clock| {
            (word == "fetch").then(|| {
                later(clock, 62);
                Ran::code(0, "")
            })
        });
        let (env, ticking) = (session(), Ticking(clock.clone()));
        let ctx = Ctx::new(
            Path::new("."),
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::ZERO,
        )
        .clocked(&ticking, Duration::from_secs(1));
        assert!(ctx.still_bound());
        assert_eq!(ctx.remaining(), Duration::from_secs(1));
        assert_eq!(ctx.checking(), Duration::from_secs(62));
        let push = ctx
            .run(Cmd::new("git").args(["push", "-q", "origin", "main"]))
            .unwrap();
        assert_eq!((push.exit, ctx.timed_out()), (Exit::Code(0), false));
    }

    #[test]
    fn a_check_that_fails_is_cut_or_panics_is_exempt_too() {
        let clock = Rc::new(Cell::new(Instant::now()));
        let (env, ticking) = (session(), Ticking(clock.clone()));
        let budget = Duration::from_secs(60);
        let ctx_for = |proc| {
            Ctx::new(
                Path::new("."),
                Path::new("store/epochs/e"),
                proc,
                &env,
                OsStr::new("cm"),
                Duration::ZERO,
            )
            .clocked(&ticking, budget)
        };

        let refused = bound_children(&clock, |word, clock| {
            (word == "sh").then(|| {
                later(clock, 5);
                Ran::code(1, "refused superseded x\n")
            })
        });
        let ctx = ctx_for(&refused);
        assert!(!ctx.still_bound());
        assert_eq!(
            (ctx.remaining(), ctx.checking()),
            (budget, Duration::from_secs(5))
        );

        let cut = bound_children(&clock, |word, clock| {
            (word == "fetch").then(|| {
                later(clock, 30);
                Ran::timed_out()
            })
        });
        let ctx = ctx_for(&cut);
        assert!(ctx.still_bound(), "a fetch cut at its cap counts as bound");
        assert_eq!(
            (ctx.remaining(), ctx.checking()),
            (budget, Duration::from_secs(30))
        );

        let panics = bound_children(&clock, |word, clock| {
            (word == "fetch").then(|| {
                later(clock, 5);
                panic!("fetch bug")
            })
        });
        let ctx = ctx_for(&panics);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.still_bound()));
        assert!(caught.is_err());
        assert_eq!(
            (ctx.remaining(), ctx.checking()),
            (budget, Duration::from_secs(5))
        );
    }

    #[test]
    fn the_checks_of_one_phase_leave_its_budget_whole_however_many_it_runs() {
        let clock = Rc::new(Cell::new(Instant::now()));
        let start = clock.get();
        let proc = bound_children(&clock, |word, clock| {
            (word == "fetch").then(|| {
                later(clock, 30);
                Ran::timed_out()
            })
        });
        let (env, ticking) = (session(), Ticking(clock.clone()));
        let ctx = Ctx::new(
            Path::new("."),
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::ZERO,
        )
        .clocked(&ticking, Duration::from_secs(60));
        for _ in 0..3 {
            assert!(ctx.still_bound());
        }
        assert_eq!(clock.get(), start + Duration::from_secs(90));
        assert_eq!(
            ctx.remaining(),
            Duration::from_secs(60),
            "three 30 s checks spend none of it"
        );
        assert_eq!(ctx.checking(), Duration::from_secs(90));
    }

    // cm#132's fresh reader: an IfDirty gitsync 31 s into its 60 s, past the heartbeat's own
    // deadline, re-checks for 30 s; capping the check's move there timed its push out.
    #[test]
    fn a_check_late_in_a_phase_leaves_the_push_after_it_its_budget() {
        let clock = Rc::new(Cell::new(Instant::now()));
        let proc = bound_children(&clock, |word, clock| {
            (word == "fetch").then(|| {
                later(clock, 30);
                Ran::code(0, "")
            })
        });
        let (env, ticking) = (session(), Ticking(clock.clone()));
        let ctx = Ctx::new(
            Path::new("."),
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::ZERO,
        )
        .clocked(&ticking, Duration::from_secs(60));
        later(&clock, 31);
        assert!(ctx.still_bound());
        let push = ctx
            .run(Cmd::new("git").args(["push", "-q", "origin", "main"]))
            .unwrap();
        assert_eq!((push.exit, ctx.timed_out()), (Exit::Code(0), false));
        assert_eq!(ctx.remaining(), Duration::from_secs(29));
    }

    #[test]
    fn a_cm_cut_at_the_whole_cap_is_named_not_by_its_cut_last_line() {
        let proc = crate::tick::proc::FakeProc::new(|cmd| {
            Ok(match cmd.argv.get(1).and_then(|sub| sub.to_str()) {
                Some("brief") => Ran {
                    exit: Exit::TooLarge,
                    stdout_truncated: true,
                    ..Ran::code(0, "{\"cut")
                },
                _ => Ran::code(1, "").with_stderr("no thread recorded"),
            })
        });
        let env = Env::default();
        let ctx = Ctx::new(
            Path::new("."),
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::from_secs(5),
        );
        assert_eq!(
            ctx.cm("brief", &[]).unwrap_err(),
            "cm brief's output was too large to keep in memory (64 MiB at most) and it was killed"
        );
        assert_eq!(
            ctx.cm("review", &[]).unwrap_err(),
            "cm review failed: no thread recorded"
        );
    }

    #[test]
    fn text_matches_jq_for_parts_and_missing_fields() {
        let line = |json| ActLine { json, lane: None };
        assert_eq!(
            line(json!({"kind": "review-owed", "target": "01-a/u/b", "part": "api", "command": "cm brief 01-a/u/b"})).text(),
            "review-owed 01-a/u/b#api: cm brief 01-a/u/b"
        );
        assert_eq!(
            line(json!({"kind": "k", "target": "t", "part": null, "command": "c"})).text(),
            "k t: c"
        );
        assert_eq!(line(json!({"kind": 3})).text(), "3 : null");
        assert_eq!(
            line(json!({"kind": "k", "part": "p", "command": "c"})).text(),
            "k #p: c"
        );
    }

    #[test]
    fn unit_and_step_follow_cut() {
        assert_eq!(unit_and_step("01-a/u/b#p"), ("01-a/u", "b"));
        assert_eq!(unit_and_step("01-a/u/b/c"), ("01-a/u", "b"));
        assert_eq!(unit_and_step("01-a/u"), ("01-a/u", ""));
        assert_eq!(unit_and_step("solo"), ("solo", "solo"));
    }

    #[test]
    fn lookup_tries_kind_dot_lane_then_kind() {
        struct Named;
        impl Executor for Named {
            fn run(&self, line: &ActLine, _: &Ctx) -> Outcome {
                Outcome::Left(line.text())
            }
        }
        let registry = Registry::new()
            .with("merge-ready.codex", Named)
            .with("merge-ready", Named);
        let key = |lane| registry.lookup("merge-ready", lane).map(|(key, _)| key);
        assert_eq!(key(Some("codex")), Some("merge-ready.codex"));
        assert_eq!(key(Some("claude-cloud")), Some("merge-ready"));
        assert_eq!(key(None), Some("merge-ready"));
        assert!(registry.lookup("unblock-now", Some("codex")).is_none());
        assert_eq!(
            registry.keys().collect::<Vec<_>>(),
            ["merge-ready", "merge-ready.codex"]
        );
    }

    #[test]
    fn last_line_is_act_libs() {
        let ran = Ran::code(1, "first\n\nno thread recorded\n\n").with_stderr("");
        assert_eq!(last_line(&ran), "no thread recorded");
        let ran = Ran::code(1, "out").with_stderr("err line\n  \n");
        assert_eq!(last_line(&ran), "err line");
        let ran = Ran::code(1, "out").with_stderr("");
        assert_eq!(
            last_line(&ran),
            "out",
            "stdout and stderr are joined by a newline"
        );
        assert_eq!(last_line(&Ran::code(1, "a\rb\u{2028} \x0c")), "b");
        assert_eq!(last_line(&Ran::code(3, "\n \n")), "exit 3");
        assert_eq!(
            last_line(&Ran {
                exit: Exit::Signal(9),
                ..Ran::code(0, "")
            }),
            "exit 137"
        );
        assert_eq!(last_line(&Ran::code(1, "é".repeat(400))), "é".repeat(300));
    }

    #[test]
    fn truthy_is_pythons() {
        for value in [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(&value), "{value}");
        }
        for value in [
            json!(true),
            json!(1),
            json!("x"),
            json!([0]),
            json!({"a": 0}),
        ] {
            assert!(truthy(&value), "{value}");
        }
    }

    #[test]
    fn pull_urls_name_one_github_pull() {
        assert_eq!(
            pull_url("https://github.com/dekopon-agents/cm/pull/122"),
            Some(("dekopon-agents/cm".to_owned(), 122))
        );
        for bad in [
            "",
            "https://github.com/o/r/issues/1",
            "https://github.com/o/r/pull/1/files",
            "https://github.com/o/r/pull/x",
            "https://github.com/o/r/pull/+1",
            "https://github.com/o/r/pull/",
            "https://github.com/o r/r/pull/1",
            "https://github.com//r/pull/1",
            "http://github.com/o/r/pull/1",
        ] {
            assert_eq!(pull_url(bad), None, "{bad}");
        }
    }
}
