//! The act loop's contract: what an executor and a housekeeping phase see and return.
//!
//! An executor replaces one heartbeat.sh `act.d/` script. Where the script got the act line's
//! JSON on stdin and the epoch directory as `$1`, an [`Executor`] gets the [`ActLine`] and a
//! [`Ctx`]; where it exited 0 printing what is left for the model, or 1 printing why, it returns
//! an [`Outcome`]. It starts every child through [`Ctx::run`] (or [`Ctx::cm`]), which gives the
//! child what remains of the executor's budget; a child killed at that budget makes the loop
//! print heartbeat.sh's timeout line, whatever the executor returns. Executor code itself must
//! not block: only children are bounded.
//!
//! # Frozen signatures
//!
//! The sibling cards fill these files and nothing else, against these signatures:
//!
//! - `07-tick/mvp/housekeeping`, each phase in heartbeat.sh's order, each with its own
//!   `pub const BUDGET: Duration` (the context's deadline, and the line past which `beat` prints
//!   the phase's wall time):
//!   - [`gitsync::sync`](crate::tick::gitsync::sync)`(ctx: &Ctx, mode: gitsync::Mode) -> Report`
//!     (heartbeat.sh step 1 with `Mode::Always`, step 5 with `Mode::IfDirty`);
//!   - [`pin::run`](crate::tick::pin::run)`(ctx: &Ctx) -> Report` (step 2);
//!   - [`directive::run`](crate::tick::directive::run)`(ctx: &Ctx) -> Report` (step 3);
//!   - [`collect::run`](crate::tick::collect::run)`(ctx: &Ctx) -> Report` (before the act
//!     loop);
//!   - [`claude_md::run`](crate::tick::claude_md::run)`(ctx: &Ctx) -> Report` (step 6).
//! - `07-tick/mvp/exec-merge` and `07-tick/mvp/exec-codex`: in each `exec/<name>.rs`, keep
//!   `pub const KEY: &str` (the `act.d/` name, `<kind>` or `<kind>.<lane>`) and the unit struct
//!   with `impl Executor`, whose
//!   [`run`](Executor::run)`(&self, line: &ActLine, ctx: &Ctx) -> Outcome` is the body to fill.
//!   [`exec::registry`](crate::tick::exec::registry) registers them.
//!
//! The helpers here are act_lib.py's, for executors: [`ActLine::text`], [`Ctx::step_state`],
//! [`Ctx::pull`], [`Ctx::cm`] and [`last_line`].

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::tick::Result;
use crate::tick::proc::{Cmd, Exit, Proc, Ran};

/// One object of `cm --json next --act`'s `actionable` list.
#[derive(Debug, Clone, PartialEq)]
pub struct ActLine {
    /// The object as cm printed it. cm's JSON carries `kind`, `target`, `part`, `since` and
    /// `command`, and may carry `lane`, `pr`, `head`, `base`, `ci_green` and `weight`.
    pub json: Value,
    /// `.lane`, else the step's lane in its unit's `.cm/state.json`: heartbeat.sh's lookup.
    pub lane: Option<String>,
}

impl ActLine {
    /// Reads the lane as heartbeat.sh did: the line's own non-empty `lane`, else
    /// `.steps[<step>].lane` in `<epoch_dir>/<NN-area>/<unit>/.cm/state.json`.
    pub fn read(json: Value, epoch_dir: &Path) -> Self {
        let own = json
            .get("lane")
            .and_then(Value::as_str)
            .filter(|lane| !lane.is_empty())
            .map(str::to_owned);
        let lane = own.or_else(|| {
            let target = jq_raw(json.get("target"));
            let (unit, step) = unit_and_step(&target);
            let state = read_json(&epoch_dir.join(unit).join(".cm/state.json"))?;
            state["steps"][step]["lane"]
                .as_str()
                .filter(|lane| !lane.is_empty())
                .map(str::to_owned)
        });
        ActLine { json, lane }
    }

    /// `.kind` as `jq -r` prints it.
    pub fn kind(&self) -> String {
        jq_raw(self.json.get("kind"))
    }

    /// `.target`, plus `#<part>` when the line names a part.
    pub fn target(&self) -> String {
        let target = jq_raw(self.json.get("target"));
        match self.json.get("part").and_then(Value::as_str) {
            Some(part) => format!("{target}#{part}"),
            None => target,
        }
    }

    pub fn command(&self) -> String {
        jq_raw(self.json.get("command"))
    }

    /// `<kind> <target>[#<part>]: <command>`, the line the model sees when nothing ran.
    pub fn text(&self) -> String {
        format!("{} {}: {}", self.kind(), self.target(), self.command())
    }

    /// A string field of the line, when present.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.json.get(name).and_then(Value::as_str)
    }

    /// The line as heartbeat.sh fed an executor's stdin: compact JSON and a newline.
    pub fn stdin(&self) -> Vec<u8> {
        let mut bytes = self.json.to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }
}

/// How `jq -r` prints a value: a string bare, a missing value as `null`, anything else as JSON.
fn jq_raw(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => "null".to_owned(),
    }
}

/// heartbeat.sh's `cut -d/ -f1-2` and `cut -d/ -f3` of the target without its part: a target
/// with no `/` is both.
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

/// The environment variables the tick reads, captured once so tests can set them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env(BTreeMap<String, String>);

impl Env {
    /// The process's variables whose names and values are UTF-8.
    pub fn system() -> Self {
        Env(std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect())
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// A variable's value when it is set and not empty (the shell's `${NAME:+…}`).
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

/// The longest budget a [`Ctx`] keeps; a longer one is cut to this.
pub const MAX_BUDGET: Duration = Duration::from_secs(24 * 60 * 60);

/// What an executor or phase can reach, and the budget its children share.
pub struct Ctx<'a> {
    /// The campaign-trail checkout; every child starts here unless its command says otherwise.
    pub checkout: &'a Path,
    /// The live epoch's folder relative to the checkout, `store/epochs/<slug>`: heartbeat.sh's
    /// `$1` for an executor and `cm -C`'s argument.
    pub epoch: &'a Path,
    pub proc: &'a dyn Proc,
    pub env: &'a Env,
    /// The program `cm` calls go to.
    pub cm: &'a OsStr,
    deadline: Instant,
    timed_out: Cell<bool>,
}

impl<'a> Ctx<'a> {
    /// A context whose children share `budget` from now.
    pub fn new(
        checkout: &'a Path,
        epoch: &'a Path,
        proc: &'a dyn Proc,
        env: &'a Env,
        cm: &'a OsStr,
        budget: Duration,
    ) -> Self {
        Ctx {
            checkout,
            epoch,
            proc,
            env,
            cm,
            deadline: Instant::now() + budget.min(MAX_BUDGET),
            timed_out: Cell::new(false),
        }
    }

    /// The epoch folder on disk.
    pub fn epoch_dir(&self) -> PathBuf {
        self.checkout.join(self.epoch)
    }

    /// The live epoch's slug: `eleemosynary-mnemosyne`.
    pub fn slug(&self) -> String {
        self.epoch
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// What is left of the budget.
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Whether a child was killed at the budget.
    pub fn timed_out(&self) -> bool {
        self.timed_out.get()
    }

    /// Runs `cmd` in the checkout (unless it names a directory) with at most what remains of
    /// the budget. A spent budget starts nothing and reads as a timeout.
    pub fn run(&self, mut cmd: Cmd) -> Result<Ran> {
        let remaining = self.remaining();
        if remaining.is_zero() {
            self.timed_out.set(true);
            return Ok(Ran::timed_out());
        }
        cmd.timeout = cmd.timeout.min(remaining);
        if cmd.cwd.is_none() {
            cmd.cwd = Some(self.checkout.to_path_buf());
        }
        let ran = self.proc.run(&cmd)?;
        if ran.exit == Exit::TimedOut {
            self.timed_out.set(true);
        }
        Ok(ran)
    }

    /// act_lib.py's `cm`: runs `cm <sub> -C <epoch> <args>`; a non-zero exit is
    /// `cm <sub> failed: <last line>`.
    pub fn cm(&self, sub: &str, args: &[&str]) -> std::result::Result<Ran, String> {
        let cmd = Cmd::new(self.cm)
            .arg(sub)
            .arg("-C")
            .arg(self.epoch)
            .args(args);
        match self.run(cmd) {
            Ok(ran) if ran.exit.success() => Ok(ran),
            Ok(ran) => Err(format!("cm {sub} failed: {}", last_line(&ran))),
            Err(error) => Err(format!("cm {sub} failed: {error}")),
        }
    }

    /// act_lib.py's `step_state`: `.steps[<step>]` of
    /// `<epoch>/<NN-area>/<unit>/.cm/state.json`, or `no cm state for <target>`.
    pub fn step_state(&self, line: &ActLine) -> std::result::Result<Value, String> {
        let target = line.target();
        let (unit, step) = unit_and_step(&target);
        read_json(&self.epoch_dir().join(unit).join(".cm/state.json"))
            .and_then(|state| state.get("steps")?.get(step).cloned())
            .ok_or_else(|| format!("no cm state for {target}"))
    }

    /// act_lib.py's `pull`: the step's pull request, from the line's `pr`, else the step's
    /// `evidence.pr`, else its `pr`; with what `cm sync` last saw of it in
    /// `<epoch>/.cm/pulls.json`, or an empty object.
    pub fn pull(&self, line: &ActLine, step: &Value) -> std::result::Result<Pull, String> {
        let url = line
            .field("pr")
            .or_else(|| step["evidence"]["pr"].as_str())
            .or_else(|| step["pr"].as_str())
            .unwrap_or_default();
        let (repo, number) =
            pull_url(url).ok_or_else(|| format!("no pull request URL for {}", line.target()))?;
        let observed = read_json(&self.epoch_dir().join(".cm/pulls.json"))
            .and_then(|pulls| pulls.get(url).cloned())
            .unwrap_or_else(|| Value::Object(Map::new()));
        Ok(Pull {
            repo,
            number,
            url: url.to_owned(),
            observed,
        })
    }
}

/// A step's pull request, as [`Ctx::pull`] finds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pull {
    /// `<owner>/<repo>`.
    pub repo: String,
    pub number: u64,
    pub url: String,
    /// `.cm/pulls.json[url]`, or `{}`.
    pub observed: Value,
}

/// `https://github.com/<owner>/<repo>/pull/<N>` as (`<owner>/<repo>`, N).
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

/// act_lib.py's `last_line`: the last non-empty line of stdout and stderr, at most 300 bytes;
/// `exit <status>` when there is none.
pub fn last_line(ran: &Ran) -> String {
    let mut output = ran.stdout.clone();
    output.extend_from_slice(&ran.stderr);
    let text = String::from_utf8_lossy(&output);
    match text.lines().rev().find(|line| !line.trim().is_empty()) {
        Some(line) => clip(line).to_owned(),
        None => format!("exit {}", ran.exit.shell_status()),
    }
}

/// At most 300 bytes, cut at a character boundary (heartbeat.sh's `head -c 300`).
pub(crate) fn clip(text: &str) -> &str {
    &text[..text.floor_char_boundary(300)]
}

/// What an executor did with its act line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The move is done; these lines are left for the model (none: nothing is left).
    Done(Vec<String>),
    /// The executor did not make the move; this line goes to the model as it is, normally
    /// [`ActLine::text`].
    Left(String),
    /// The move failed: the act line is printed once with this reason, cut to its last
    /// non-empty line.
    Failed(String),
}

/// One `act.d/` script's replacement.
pub trait Executor {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome;
}

/// The executors, keyed `<kind>.<lane>` or `<kind>` as heartbeat.sh named `act.d/` files.
#[derive(Default)]
pub struct Registry {
    executors: BTreeMap<String, Box<dyn Executor>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    /// Adds an executor under `key`; a later one replaces an earlier one.
    pub fn with(mut self, key: &str, executor: impl Executor + 'static) -> Self {
        self.executors.insert(key.to_owned(), Box::new(executor));
        self
    }

    /// heartbeat.sh's lookup: `<kind>.<lane>` when the line has a lane, then `<kind>`. Returns
    /// the matched key, the executor's name in the loop's lines (`tick/<key>`).
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

/// What a housekeeping phase leaves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Lines for the model, printed in order.
    pub lines: Vec<String>,
    /// The phase itself failed (not one item of it); `beat` then prints its wall time.
    pub failed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(line(json!({"kind": 3})).text(), "3 null: null");
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
    fn last_line_is_the_last_non_empty_line_cut_to_300_bytes() {
        let ran = Ran::code(1, "first\n\nno thread recorded\n\n").with_stderr("");
        assert_eq!(last_line(&ran), "no thread recorded");
        let ran = Ran::code(1, "out\n").with_stderr("err line\n");
        assert_eq!(last_line(&ran), "err line");
        assert_eq!(last_line(&Ran::code(3, "\n \n")), "exit 3");
        assert_eq!(last_line(&Ran::code(1, "é".repeat(200))).len(), 300);
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
            "http://github.com/o/r/pull/1",
        ] {
            assert_eq!(pull_url(bad), None, "{bad}");
        }
    }
}
