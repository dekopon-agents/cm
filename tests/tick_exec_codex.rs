use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cm::tick::TickError;
use cm::tick::act::{ActLine, Ctx, Env, Executor, Outcome, Report};
use cm::tick::beat;
use cm::tick::collect;
use cm::tick::exec::codex_dispatch::CodexDispatch;
use cm::tick::exec::review_owed::ReviewOwed;
use cm::tick::proc::{Cmd, FakeProc, Proc, Ran, SystemProc};
use serde_json::{Value, json};

const EPOCH: &str = "store/epochs/ep";
const STEP: &str = "08-egress/shopping-vm/guest-flag";
const PR: &str = "https://github.com/dekopon-agents/vm-runner/pull/77";
const CLAIM: &str = "2:session_x:2026-11-25T10:00:00Z";
const KICKOFF: &str = "repo: dekopon-agents/vm-runner

Before anything else, read https://raw.githubusercontent.com/xrl/agents/<STEERING-SHA>/steering/GPT.md.

- Lane: codex-cloud (gpt-6.2-sol xhigh), size XS, repo dekopon-agents/vm-runner.
";
const PICK_OK: &str = "ok store/epochs/ep/handoff/coordinator-2026-11-25T1700Z.md\n";

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn head() -> String {
    sha('a')
}

fn base() -> String {
    sha('b')
}

fn next_head() -> String {
    sha('c')
}

fn steering() -> String {
    sha('d')
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn script(path: &Path, body: &str) {
    write(path, &format!("#!/bin/sh\n{body}"));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

// The executors run their children through the real `SystemProc`, with the fixture's `bin` first
// on each child's PATH, so the fake cm, git, uv and python3 answer and nothing real is reached.
struct OnPath(PathBuf);

impl Proc for OnPath {
    fn run(&self, cmd: &Cmd) -> cm::tick::Result<Ran> {
        let path = format!("{}:/usr/bin:/bin", self.0.display());
        SystemProc.run(&cmd.clone().env("PATH", path))
    }

    fn pause(&self, _: Duration) {}
}

// <root>/ct is the campaign-trail checkout, <root>/bin the fakes, <root>/home HOME and
// <root>/tmp TMPDIR; every fake appends its argv to <root>/calls.
struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Fixture(tempfile::tempdir().unwrap());
        fs::create_dir_all(fixture.root().join("home")).unwrap();
        fs::create_dir_all(fixture.root().join("tmp")).unwrap();
        write(&fixture.step_dir().join("kickoff.md"), KICKOFF);
        fixture.write_state(json!({"lane": "codex", "state": "pr-open", "evidence": {"pr": PR}}));
        fixture.observe(&head());
        fixture.fake_cm("never-a-subcommand", true);
        fixture.fake_git(&steering());
        fixture.fake_uv(&[], 0);
        fixture.fake_python(0);
        fixture.fake_astra(0, 3, "");
        fixture
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn ct(&self) -> PathBuf {
        self.root().join("ct")
    }

    fn bin(&self) -> PathBuf {
        self.root().join("bin")
    }

    fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    fn epoch(&self) -> PathBuf {
        self.ct().join(EPOCH)
    }

    fn step_dir(&self) -> PathBuf {
        self.epoch().join(STEP)
    }

    fn evidence(&self) -> PathBuf {
        self.step_dir().join("evidence")
    }

    fn log(&self) -> String {
        self.root().join("calls").display().to_string()
    }

    fn write_state(&self, step: Value) {
        write(
            &self.epoch().join("08-egress/shopping-vm/.cm/state.json"),
            &json!({"steps": {"guest-flag": step}}).to_string(),
        );
    }

    fn observe(&self, head: &str) {
        let seen = json!({"target": STEP, "at": "x", "head": head, "base": base()});
        write(
            &self.epoch().join(".cm/pulls.json"),
            &json!({PR: seen}).to_string(),
        );
    }

    fn fake(&self, name: &str, body: &str) {
        script(&self.bin().join(name), body);
    }

    // `cm launch` records the attempt and its claim token in the unit's state, as cm does.
    fn fake_cm(&self, fail_on: &str, claims: bool) {
        let attempt = if claims {
            json!({"claim": CLAIM})
        } else {
            json!(null)
        };
        let launched = json!({"steps": {"guest-flag": {"lane": "codex", "state": "launched", "attempt": attempt}}});
        self.fake(
            "cm",
            &format!(
                "echo \"cm $*\" >> '{log}'
case \"$1\" in
  {fail_on}) echo 'error: refused' >&2; exit 1 ;;
  brief) echo 'the brief' ;;
  launch) printf '%s' '{launched}' > '{state}' ;;
esac
exit 0
",
                log = self.log(),
                state = self
                    .epoch()
                    .join("08-egress/shopping-vm/.cm/state.json")
                    .display(),
            ),
        );
    }

    fn fake_git(&self, steering: &str) {
        self.fake(
            "git",
            &format!(
                "echo \"git $*\" >> '{}'\n[ \"$3\" = rev-parse ] && echo '{steering}'\nexit 0\n",
                self.log()
            ),
        );
    }

    // Keeps the prompt it was given and the directory it ran in.
    fn fake_uv(&self, receipts: &[Value], code: i32) {
        let rows: Vec<String> = receipts
            .iter()
            .map(|receipt| json!({"receipt": receipt}).to_string())
            .collect();
        let root = self.root().display();
        self.fake(
            "uv",
            &format!(
                "echo \"uv $*\" >> '{log}'
pwd > '{root}/uv-cwd'
while [ $# -gt 0 ]; do
  if [ \"$1\" = --prompt-file ]; then cp \"$2\" '{root}/prompt'; echo \"$2\" > '{root}/prompt-path'; fi
  shift
done
cat <<'EOF'
{rows}
EOF
exit {code}
",
                log = self.log(),
                rows = rows.join("\n"),
            ),
        );
    }

    fn fake_python(&self, code: i32) {
        self.fake(
            "python3",
            &format!(
                "echo \"python3 $*\" >> '{}'\n[ {code} = 0 ] || echo 'bad timestamp: x' >&2\nexit {code}\n",
                self.log()
            ),
        );
    }

    // `start-shadowed` writes the gate's request.json as astra-review.sh does; `collect` exiting 0
    // writes the report and cm's record.
    fn fake_astra(&self, start: i32, collect: i32, collect_out: &str) {
        script(
            &self.ct().join("tools/cloud-env/astra-review.sh"),
            &format!(
                "echo \"astra $*\" >> '{log}'
case $1 in
start-shadowed)
  [ {start} = 0 ] || {{ echo 'brief not found: x' >&2; exit {start}; }}
  repo=$2 pr=$3 base=$4 head=$5 out=$7 round=1
  while [ $# -gt 0 ]; do [ \"$1\" = --round ] && round=$2; shift; done
  mkdir -p \"$out\"
  printf '{{\"mode\": \"pr\", \"repo\": \"%s\", \"pr\": %s, \"base\": \"%s\", \"head\": \"%s\", \"round\": %s, \"requested\": {{\"model\": \"gpt-6-astra\", \"effort\": \"xhigh\"}}}}' \"$repo\" \"$pr\" \"$base\" \"$head\" \"$round\" > \"$out/request.json\"
  echo \"started; run: astra-review.sh collect $out\" ;;
collect)
  echo '{collect_out}'
  if [ {collect} = 0 ]; then printf 'findings\\n\\nACCEPT\\n' > \"$2/report.md\"; echo ok > \"$2/cm-record.txt\"; fi
  exit {collect} ;;
esac
",
                log = self.log(),
            ),
        );
    }

    fn gate(
        &self,
        name: &str,
        head: &str,
        pr: u64,
        model: &str,
        mode: &str,
        round: u64,
    ) -> PathBuf {
        let dir = self.evidence().join(name);
        let request = json!({"mode": mode, "repo": "dekopon-agents/vm-runner", "pr": pr, "head": head,
            "round": round, "requested": {"model": model, "effort": "xhigh"}});
        write(&dir.join("request.json"), &request.to_string());
        dir
    }

    fn calls(&self, prefix: &str) -> Vec<String> {
        fs::read_to_string(self.root().join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with(prefix))
            .map(str::to_owned)
            .collect()
    }

    fn env(&self, extra: &[(&str, &str)]) -> Env {
        let home = self.home().display().to_string();
        let tmp = self.root().join("tmp").display().to_string();
        let mut pairs = vec![
            ("HOME", home.as_str()),
            ("TMPDIR", tmp.as_str()),
            ("CODEX_CLOUD_ENVIRONMENT_ID", "acct~env"),
            ("CODEX_CLOUD_PY", "/opt/cc.py"),
        ];
        pairs.retain(|(name, _)| !extra.iter().any(|(given, _)| given == name));
        pairs.extend(
            extra
                .iter()
                .copied()
                .filter(|(_, value)| *value != "<unset>"),
        );
        pairs.into_iter().collect()
    }

    fn run(&self, executor: &dyn Executor, line: Value, env: &Env) -> Outcome {
        let proc = OnPath(self.bin());
        let ct = self.ct();
        let ctx = Ctx::new(
            &ct,
            Path::new(EPOCH),
            &proc,
            env,
            OsStr::new("cm"),
            Duration::from_secs(60),
        );
        executor.run(&ActLine::read(line, &self.epoch()), &ctx)
    }

    fn codex(&self, extra: &[(&str, &str)]) -> Outcome {
        let line = json!({"kind": "launchable-not-launched", "target": STEP, "command": format!("cm launch {STEP}")});
        self.run(&CodexDispatch, line, &self.env(extra))
    }

    fn review(&self, extra: Value) -> Outcome {
        let mut line =
            json!({"kind": "review-owed", "target": STEP, "command": format!("cm brief {STEP}")});
        line.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.run(&ReviewOwed, line, &self.env(&[]))
    }

    fn tmp_is_empty(&self) -> bool {
        fs::read_dir(self.root().join("tmp"))
            .unwrap()
            .next()
            .is_none()
    }
}

fn admitted() -> Vec<Value> {
    vec![
        json!({"stage": "before-thread-start"}),
        json!({"stage": "turn-admitted", "thread_id": "t-1", "model": "gpt-6.2-sol", "reasoning_effort": "xhigh"}),
    ]
}

fn failed_with(outcome: &Outcome, text: &str) -> bool {
    matches!(outcome, Outcome::Failed(reason) if reason.contains(text))
}

fn nothing() -> Outcome {
    Outcome::Done(Vec::new())
}

// launchable-not-launched.codex

#[test]
fn a_codex_dispatch_runs_the_cm_attempt_sequence_with_the_lane_model() {
    let fixture = Fixture::new();
    fixture.write_state(json!({"lane": "codex", "state": "drafted"}));
    fixture.fake_uv(&admitted(), 0);
    assert_eq!(fixture.codex(&[]), nothing(), "a dispatch prints nothing");

    let agents = fixture.home().join("code/xrl/agents");
    let prompt_path = fs::read_to_string(fixture.root().join("prompt-path")).unwrap();
    let prompt_path = prompt_path.trim_end();
    let ts = fixture.calls("python3 ")[0]
        .split(" --ts-start ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        fs::read_to_string(fixture.root().join("calls"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            format!("git -C {} pull -q", agents.display()),
            format!("git -C {} rev-parse HEAD", agents.display()),
            format!("cm launch -C {EPOCH} {STEP}"),
            format!("cm attempt -C {EPOCH} {STEP} --claim {CLAIM} begin thread"),
            format!(
                "uv run -q /opt/cc.py --write jobs create --environment-id acct~env --prompt-file {prompt_path} --model gpt-6.2-sol --effort xhigh --title {STEP}"
            ),
            format!("cm attempt -C {EPOCH} {STEP} --claim {CLAIM} record thread t-1"),
            format!(
                "python3 -I {}/tools/agent-ledger/record.py manual --step {STEP} --role writer --harness codex-cloud --model-requested gpt-6.2-sol --model-echoed gpt-6.2-sol --effort-requested xhigh --effort-echoed xhigh --session-id t-1 --ts-start {ts} --epoch ep --source cm tick codex dispatch --out {EPOCH}/AGENTS.jsonl",
                fixture.ct().display()
            ),
        ]
    );
    let started: jiff::Timestamp = ts.parse().unwrap();
    assert!(ts.ends_with("+00:00") && ts.len() == "2026-11-25T10:00:00.000000+00:00".len());
    assert!(started <= jiff::Timestamp::now());

    let prompt = fs::read_to_string(fixture.root().join("prompt")).unwrap();
    assert!(prompt.contains(&format!("/xrl/agents/{}/steering/GPT.md", steering())));
    assert!(!prompt.contains("<STEERING-SHA>"));
    assert_eq!(prompt, KICKOFF.replace("<STEERING-SHA>", &steering()));
    assert!(Path::new(prompt_path).starts_with(fixture.root().join("tmp")));
    assert!(fixture.tmp_is_empty(), "the prompt file is removed");
    assert_eq!(
        fs::read_to_string(fixture.root().join("uv-cwd")).unwrap(),
        "/tmp\n"
    );
}

#[test]
fn a_codex_dispatch_without_a_lane_model_fails_before_any_effect() {
    let fixture = Fixture::new();
    for kickoff in [
        KICKOFF.replace(" (gpt-6.2-sol xhigh)", ""),
        KICKOFF.replace("- Lane:", "- Repo:"),
    ] {
        write(&fixture.step_dir().join("kickoff.md"), &kickoff);
        let outcome = fixture.codex(&[]);
        assert_eq!(
            outcome,
            Outcome::Failed("the kickoff's Lane: line names no model and effort (`Lane: codex-cloud (<model> <effort>)`); dispatch by hand".to_owned()),
            "{kickoff}"
        );
    }
    assert_eq!(fixture.calls(""), Vec::<String>::new());
}

#[test]
fn a_launch_failing_mid_sequence_dispatches_nothing() {
    let fixture = Fixture::new();
    fixture.fake_cm("attempt", true);
    fixture.fake_uv(&[json!({"stage": "turn-admitted", "thread_id": "t-1"})], 0);
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed("cm attempt failed: error: refused".to_owned())
    );
    assert_eq!(fixture.calls("uv "), Vec::<String>::new());
    assert!(fixture.tmp_is_empty(), "the prompt file is removed");
}

#[test]
fn a_codex_dispatch_of_a_multipart_kickoff_stays_the_models() {
    let fixture = Fixture::new();
    write(
        &fixture.step_dir().join("kickoff.md"),
        &format!("{KICKOFF}- `Parts: a, b`: one PR each.\n"),
    );
    let outcome = fixture.codex(&[]);
    assert!(failed_with(&outcome, "Parts:"), "{outcome:?}");
    assert_eq!(fixture.calls("cm "), Vec::<String>::new());

    let fixture = Fixture::new();
    let line = json!({"kind": "launchable-not-launched", "target": STEP, "part": "api", "command": "cm launch x"});
    assert_eq!(
        fixture.run(&CodexDispatch, line, &fixture.env(&[])),
        Outcome::Failed("a part's launch is the model's".to_owned())
    );
    assert_eq!(fixture.calls(""), Vec::<String>::new());
}

#[test]
fn a_create_that_never_started_a_turn_fails_with_the_thread_recorded() {
    let fixture = Fixture::new();
    fixture.fake_uv(&[json!({"stage": "thread-created", "thread_id": "t-9"})], 1);
    let outcome = fixture.codex(&[]);
    assert_eq!(
        outcome,
        Outcome::Failed(format!(
            "jobs create stopped at thread-created ({}); thread t-9 recorded; inspect it before any retry",
            json!({"receipt": {"stage": "thread-created", "thread_id": "t-9"}})
        ))
    );
    assert!(fixture.calls("cm ").contains(&format!(
        "cm attempt -C {EPOCH} {STEP} --claim {CLAIM} record thread t-9"
    )));
    assert_eq!(fixture.calls("python3 "), Vec::<String>::new());

    let fixture = Fixture::new();
    fixture.fake_uv(&admitted(), 2);
    let outcome = fixture.codex(&[]);
    assert!(
        failed_with(&outcome, "jobs create stopped at turn-admitted ("),
        "an admitted turn from a create that exited nonzero still fails: {outcome:?}"
    );
    assert!(failed_with(&outcome, "thread t-1 recorded"));

    let fixture = Fixture::new();
    fixture.fake_uv(&[], 1);
    let outcome = fixture.codex(&[]);
    assert_eq!(
        outcome,
        Outcome::Failed(
            "jobs create stopped at no receipt (exit 1); thread none recorded; inspect it before any retry"
                .to_owned()
        )
    );
    assert_eq!(fixture.calls("cm attempt").len(), 1, "only begin thread");
}

#[test]
fn a_codex_dispatch_needs_the_environment_id() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.codex(&[("CODEX_CLOUD_ENVIRONMENT_ID", "")]),
        Outcome::Failed("CODEX_CLOUD_ENVIRONMENT_ID is not set".to_owned())
    );
    assert_eq!(
        fixture.codex(&[("CODEX_CLOUD_ENVIRONMENT_ID", "<unset>")]),
        Outcome::Failed("CODEX_CLOUD_ENVIRONMENT_ID is not set".to_owned())
    );
    assert_eq!(fixture.calls(""), Vec::<String>::new());
}

#[test]
fn a_codex_dispatch_without_a_kickoff_home_or_steering_sha_fails_before_cm_launch() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.codex(&[("HOME", "<unset>")]),
        Outcome::Failed("HOME is not set, so there is no ~/code/xrl/agents".to_owned())
    );
    fixture.fake_git("");
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed(
            "no steering SHA: git -C ~/code/xrl/agents rev-parse HEAD failed".to_owned()
        )
    );
    fixture.fake_git(&"D".repeat(40));
    assert!(failed_with(&fixture.codex(&[]), "no steering SHA"));
    assert_eq!(fixture.calls("cm "), Vec::<String>::new());

    fs::remove_file(fixture.step_dir().join("kickoff.md")).unwrap();
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed(format!("no {EPOCH}/{STEP}/kickoff.md"))
    );
    fs::write(
        fixture.step_dir().join("kickoff.md"),
        b"Lane: \xff gpt-6 high\n",
    )
    .unwrap();
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed(format!("{EPOCH}/{STEP}/kickoff.md is not UTF-8 text"))
    );
}

#[test]
fn a_prompt_file_that_cannot_be_written_stops_the_dispatch_before_cm_launch() {
    let fixture = Fixture::new();
    let missing = fixture.root().join("no-such-dir");
    let outcome = fixture.codex(&[("TMPDIR", &missing.display().to_string())]);
    assert!(
        failed_with(
            &outcome,
            &format!("cannot write the prompt file in {}: ", missing.display())
        ),
        "{outcome:?}"
    );
    assert!(failed_with(&outcome, "; nothing dispatched"));
    assert_eq!(fixture.calls("cm "), Vec::<String>::new());
}

#[test]
fn a_launch_that_records_no_claim_dispatches_nothing() {
    let fixture = Fixture::new();
    fixture.fake_cm("never-a-subcommand", false);
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed(format!(
            "cm launch recorded no attempt claim for {STEP}; nothing dispatched"
        ))
    );
    assert_eq!(
        fixture.calls("cm "),
        [format!("cm launch -C {EPOCH} {STEP}")]
    );
    assert_eq!(fixture.calls("uv "), Vec::<String>::new());

    let fixture = Fixture::new();
    fixture.fake_cm("launch", true);
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed("cm launch failed: error: refused".to_owned())
    );
    fixture.fake(
        "cm",
        &format!(
            "echo \"cm $*\" >> '{}'\necho '{{' > '{}'\n",
            fixture.log(),
            fixture
                .epoch()
                .join("08-egress/shopping-vm/.cm/state.json")
                .display()
        ),
    );
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed(format!("no cm state for {STEP}"))
    );
    assert_eq!(fixture.calls("cm attempt"), Vec::<String>::new());
    assert_eq!(fixture.calls("uv "), Vec::<String>::new());
    assert!(fixture.tmp_is_empty(), "the prompt file is removed");
}

#[test]
fn a_thread_cm_does_not_record_is_named_in_the_failure() {
    let fixture = Fixture::new();
    fixture.fake_uv(&admitted(), 0);
    fixture.fake(
        "cm",
        &format!(
            "echo \"cm $*\" >> '{log}'
case \"$*\" in
  *'record thread'*) echo 'error: stale claim' >&2; exit 1 ;;
  launch*) printf '%s' '{state}' > '{path}' ;;
esac
",
            log = fixture.log(),
            state = json!({"steps": {"guest-flag": {"attempt": {"claim": CLAIM}}}}),
            path = fixture
                .epoch()
                .join("08-egress/shopping-vm/.cm/state.json")
                .display()
        ),
    );
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Failed("cm attempt failed: error: stale claim; thread t-1 was created but not recorded; inspect it before any retry".to_owned())
    );
    assert_eq!(fixture.calls("python3 "), Vec::<String>::new());
}

#[test]
fn a_ledger_row_not_written_is_one_line_after_the_dispatch() {
    let fixture = Fixture::new();
    fixture.fake_uv(&admitted(), 0);
    fixture.fake_python(1);
    assert_eq!(
        fixture.codex(&[]),
        Outcome::Done(vec![format!(
            "launchable-not-launched {STEP}: dispatched (thread t-1); AGENTS.jsonl row not written: bad timestamp: x"
        )])
    );
}

#[test]
fn codex_cloud_py_defaults_to_the_skill_under_home_and_unechoed_settings_are_unknown() {
    let fixture = Fixture::new();
    fixture.fake_uv(&[json!({"stage": "turn-admitted", "thread_id": "t-2"})], 0);
    assert_eq!(fixture.codex(&[("CODEX_CLOUD_PY", "")]), nothing());
    let uv = &fixture.calls("uv ")[0];
    assert!(
        uv.starts_with(&format!(
            "uv run -q {}/.claude/skills/codex-cloud/scripts/codex_cloud.py --write jobs create ",
            fixture.home().display()
        )),
        "{uv}"
    );
    let ledger = &fixture.calls("python3 ")[0];
    assert!(ledger.contains(" --model-echoed unknown "), "{ledger}");
    assert!(ledger.contains(" --effort-echoed unknown "), "{ledger}");
    assert!(ledger.contains(" --session-id t-2 "), "{ledger}");
}

// review-owed

fn started_line(out: &str) -> String {
    format!(
        "review-owed {STEP}: Astra started at {} ({EPOCH}/{STEP}/evidence/{out}); start the Claude review at the same head now; merge-ready waits for both verdicts",
        &head()[..12]
    )
}

#[test]
fn review_owed_starts_round_one_then_collects_and_reports_the_verdict() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.review(json!({"ci_green": true})),
        Outcome::Done(vec![started_line("astra-vm-runner-77-r1")])
    );
    let out = format!("{EPOCH}/{STEP}/evidence/astra-vm-runner-77-r1");
    assert_eq!(
        fixture.calls("astra "),
        [format!(
            "astra start-shadowed dekopon-agents/vm-runner 77 {} {} {out}/brief.md {out} --round 1 --ci green",
            base(),
            head()
        )]
    );
    assert_eq!(
        fs::read_to_string(fixture.ct().join(&out).join("brief.md")).unwrap(),
        "the brief\n"
    );
    assert_eq!(fixture.review(json!({})), nothing(), "still running");
    assert_eq!(fixture.calls("astra ")[1], format!("astra collect {out}"));
    fixture.fake_astra(0, 0, "ACCEPT {}");
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Done(vec![format!(
            "review-owed {STEP}: Astra ACCEPT recorded at {} ({out}/report.md); the next move is yours (the fix pass, or the Claude review: merge-ready waits for both verdicts at this head)",
            &head()[..12]
        )])
    );
    assert_eq!(fixture.calls("astra ").len(), 3);
    fixture.review(json!({}));
    assert_eq!(
        fixture.calls("astra ").len(),
        3,
        "a gate cm recorded is not collected again"
    );
}

#[test]
fn review_owed_finds_earlier_gates_by_request_json_whatever_their_folder() {
    let fixture = Fixture::new();
    let r1 = fixture.gate("astra-r1", &head(), 77, "gpt-6-astra", "pr", 1);
    write(&r1.join("report.md"), "F1\n\nFIX REQUIRED\n");
    fixture.gate(
        "astra-r1-shadow-gpt-6.2-sol-xhigh",
        &head(),
        77,
        "gpt-6.2-sol",
        "pr",
        1,
    );
    fixture.gate(
        "astra-vm-runner-770-r4",
        &head(),
        770,
        "gpt-6-astra",
        "pr",
        4,
    );
    fixture.gate("astra-note-r3", &head(), 77, "gpt-6-astra", "note", 3);
    fixture.gate("astra-x-r5-shadow-y", &head(), 77, "gpt-6-astra", "pr", 5);
    fixture.observe(&next_head());
    let outcome = fixture.review(json!({}));
    assert!(
        matches!(&outcome, Outcome::Done(lines) if lines.len() == 1 && lines[0].contains("astra-vm-runner-77-r2); start the Claude review")),
        "{outcome:?}"
    );
    let start = fixture.calls("astra ").pop().unwrap();
    assert!(
        start.ends_with(&format!(
            "astra-vm-runner-77-r2 --round 2 --ci unknown --prev-head {} --findings {EPOCH}/{STEP}/evidence/astra-r1/report.md",
            head()
        )),
        "{start}"
    );
    assert!(start.contains(&format!(" 77 {} {} ", base(), next_head())));
}

#[test]
fn review_owed_at_a_head_with_a_gate_collects_it_instead_of_starting_another() {
    let fixture = Fixture::new();
    fixture.gate(
        "astra-dekopon-agents-77-r2",
        &head(),
        77,
        "gpt-6-astra",
        "pr",
        2,
    );
    assert_eq!(fixture.review(json!({})), nothing());
    assert_eq!(
        fixture.calls("astra "),
        [format!(
            "astra collect {EPOCH}/{STEP}/evidence/astra-dekopon-agents-77-r2"
        )]
    );

    let recorded = fixture.evidence().join("astra-dekopon-agents-77-r2");
    write(&recorded.join("report.md"), "F1\n\nFIX REQUIRED\n\n");
    write(&recorded.join("cm-record.txt"), "ok\n");
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Done(vec![format!(
            "review-owed {STEP}: Astra FIX REQUIRED recorded at {} ({EPOCH}/{STEP}/evidence/astra-dekopon-agents-77-r2/report.md); the next move is yours (the fix pass, or the Claude review: merge-ready waits for both verdicts at this head)",
            &head()[..12]
        )])
    );
    assert_eq!(fixture.calls("astra ").len(), 1);
}

#[test]
fn a_blocked_collect_fails_with_its_reason() {
    let fixture = Fixture::new();
    fixture.fake_astra(0, 1, "REVIEW_BLOCKED: harness reported effort high");
    fixture.gate("astra-r1", &head(), 77, "gpt-6-astra", "pr", 1);
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Failed(format!(
            "astra-review.sh collect {EPOCH}/{STEP}/evidence/astra-r1: REVIEW_BLOCKED: harness reported effort high"
        ))
    );
}

#[test]
fn a_fix_routing_line_is_always_printed() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.review(json!({"command": format!("cm attention --pr {PR}")})),
        Outcome::Done(vec![format!(
            "review-owed {STEP}: cm attention --pr {PR} [FIX REQUIRED at this head: route the findings to the fix pass]"
        )])
    );
    assert_eq!(fixture.calls(""), Vec::<String>::new());
}

// U2 (owner, 2026-11-25T06:37Z): the Claude review starts with Astra at every weight.
#[test]
fn every_weight_prints_the_parallel_claude_review_when_astra_starts() {
    for weight in [
        json!({"weight": "complex"}),
        json!({"weight": "simple"}),
        json!({}),
    ] {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.review(weight.clone()),
            Outcome::Done(vec![started_line("astra-vm-runner-77-r1")]),
            "{weight}"
        );
    }
}

#[test]
fn review_owed_without_an_observed_head_fails() {
    let fixture = Fixture::new();
    write(&fixture.epoch().join(".cm/pulls.json"), "{}");
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Failed(format!(
            "no observed head and base for {PR} in .cm/pulls.json; cm sync first"
        ))
    );
    assert!(failed_with(
        &fixture.review(json!({"head": "abc", "base": base()})),
        "no observed head and base"
    ));
    assert_eq!(fixture.calls(""), Vec::<String>::new());
}

#[test]
fn a_head_and_base_on_the_line_come_before_the_observed_ones() {
    let fixture = Fixture::new();
    fixture.review(json!({"head": next_head(), "base": sha('e')}));
    let start = &fixture.calls("astra ")[0];
    assert!(
        start.contains(&format!(" 77 {} {} ", sha('e'), next_head())),
        "{start}"
    );
}

#[test]
fn a_later_round_without_the_earlier_report_fails_and_writes_nothing() {
    let fixture = Fixture::new();
    fixture.gate("astra-r1", &head(), 77, "gpt-6-astra", "pr", 1);
    fixture.observe(&next_head());
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Failed(format!(
            "round 2 needs {EPOCH}/{STEP}/evidence/astra-r1/report.md as its findings"
        ))
    );
    assert!(!fixture.evidence().join("astra-vm-runner-77-r2").exists());
    assert_eq!(fixture.calls(""), Vec::<String>::new());
}

#[test]
fn a_start_that_fails_names_its_last_line_and_a_part_stays_the_models() {
    let fixture = Fixture::new();
    fixture.fake_astra(1, 3, "");
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Failed("astra-review.sh start-shadowed: brief not found: x".to_owned())
    );
    assert_eq!(
        fixture.review(json!({"part": "api"})),
        Outcome::Failed("a part's review is the model's".to_owned())
    );
}

#[test]
fn review_owed_fails_on_a_step_cm_has_no_state_or_pr_for_and_on_a_brief_it_cannot_write() {
    let fixture = Fixture::new();
    fixture.write_state(json!({"lane": "codex", "state": "pr-open"}));
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Failed(format!("{STEP} records no PR URL"))
    );
    fs::remove_file(fixture.epoch().join("08-egress/shopping-vm/.cm/state.json")).unwrap();
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Failed(format!("no cm state for {STEP}"))
    );

    let fixture = Fixture::new();
    fixture.fake_cm("brief", true);
    assert_eq!(
        fixture.review(json!({})),
        Outcome::Failed("cm brief failed: error: refused".to_owned())
    );
    assert!(!fixture.evidence().exists(), "no out-dir without a brief");

    let fixture = Fixture::new();
    write(&fixture.evidence().join("astra-vm-runner-77-r1"), "a file");
    let outcome = fixture.review(json!({}));
    assert!(
        failed_with(
            &outcome,
            &format!("cannot create {EPOCH}/{STEP}/evidence/astra-vm-runner-77-r1: ")
        ),
        "{outcome:?}"
    );
    assert_eq!(fixture.calls("astra "), Vec::<String>::new());
}

// A child that cannot be spawned (`TickError::Spawn`) fails as a nonzero exit does, naming why.
#[test]
fn a_child_that_cannot_run_is_named_where_its_failure_would_be() {
    fn missing(program: &str) -> TickError {
        TickError::Spawn {
            program: program.to_owned(),
            source: std::io::ErrorKind::NotFound.into(),
        }
    }
    let fixture = Fixture::new();
    fixture
        .write_state(json!({"lane": "codex", "evidence": {"pr": PR}, "attempt": {"claim": CLAIM}}));
    let env = fixture.env(&[]);
    let ct = fixture.ct();
    let run = |executor: &dyn Executor, line: Value, unrunnable: &'static str| {
        let proc = FakeProc::new(move |cmd: &Cmd| {
            let program = cmd.program_name();
            if program == unrunnable {
                return Err(missing(&program));
            }
            Ok(
                match (
                    program.as_str(),
                    cmd.argv.get(3).and_then(|arg| arg.to_str()),
                ) {
                    ("git", Some("rev-parse")) => Ran::code(0, format!("{}\n", steering())),
                    ("uv", _) => Ran::code(0, json!({"receipt": admitted()[1]}).to_string()),
                    _ => Ran::code(0, ""),
                },
            )
        });
        let ctx = Ctx::new(
            &ct,
            Path::new(EPOCH),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::from_secs(60),
        );
        executor.run(&ActLine::read(line, &fixture.epoch()), &ctx)
    };
    let launch =
        || json!({"kind": "launchable-not-launched", "target": STEP, "command": "cm launch x"});
    let review = || json!({"kind": "review-owed", "target": STEP, "command": "cm brief x"});
    assert_eq!(
        run(&CodexDispatch, launch(), "uv"),
        Outcome::Failed("jobs create stopped at no receipt (uv: entity not found); thread none recorded; inspect it before any retry".to_owned())
    );
    assert_eq!(
        run(&CodexDispatch, launch(), "python3"),
        Outcome::Done(vec![format!(
            "launchable-not-launched {STEP}: dispatched (thread t-1); AGENTS.jsonl row not written: python3: entity not found"
        )])
    );
    assert_eq!(
        run(&ReviewOwed, review(), "astra-review.sh"),
        Outcome::Failed(
            "astra-review.sh start-shadowed: astra-review.sh: entity not found".to_owned()
        )
    );
    fixture.gate("astra-r1", &head(), 77, "gpt-6-astra", "pr", 1);
    let collect_failed = format!(
        "astra-review.sh collect {EPOCH}/{STEP}/evidence/astra-r1: astra-review.sh: entity not found"
    );
    assert_eq!(
        run(&ReviewOwed, review(), "astra-review.sh"),
        Outcome::Failed(collect_failed.clone())
    );
    let proc = FakeProc::new(|cmd: &Cmd| Err(missing(&cmd.program_name())));
    let ctx = Ctx::new(
        &ct,
        Path::new(EPOCH),
        &proc,
        &env,
        OsStr::new("cm"),
        collect::BUDGET,
    );
    assert_eq!(
        collect::run(&ctx),
        Report {
            lines: vec![collect_failed],
            failed: false
        }
    );
}

// collect (review-latency U4)

fn collect_fixture() -> Fixture {
    let fixture = Fixture::new();
    script(
        &fixture.ct().join("tools/cloud-env/astra-review.sh"),
        &format!(
            "echo \"astra $*\" >> '{}'
case $2 in
  *pending*) echo 'PENDING {{}}'; exit 3 ;;
  *blocked*) echo 'REVIEW_BLOCKED: harness reports effort high'; exit 1 ;;
  *unrecorded*) echo 'NOT RECORDED; fix and run: cm review record x'; exit 4 ;;
esac
printf 'ACCEPT\\n' > \"$2/report.md\"
",
            fixture.log()
        ),
    );
    for name in [
        "astra-vm-runner-77-r1-pending",
        "astra-vm-runner-77-r2-blocked",
        "astra-vm-runner-77-r3",
        "astra-vm-runner-77-r4-unrecorded",
        "astra-note-r1",
        "astra-vm-runner-77-r1-pending-shadow-gpt-6.2-sol-xhigh",
    ] {
        fixture.gate(name, &head(), 77, "gpt-6-astra", "pr", 1);
    }
    let ready = fixture.gate("astra-vm-runner-77-r0", &head(), 77, "gpt-6-astra", "pr", 1);
    write(&ready.join("report.md"), "ACCEPT\n");
    fixture.gate("astra-sol", &head(), 77, "gpt-6.2-sol", "pr", 1);
    fixture
}

fn collect_phase(fixture: &Fixture, proc: &dyn Proc, budget: Duration) -> Report {
    let env = fixture.env(&[]);
    let ct = fixture.ct();
    let ctx = Ctx::new(&ct, Path::new(EPOCH), proc, &env, OsStr::new("cm"), budget);
    collect::run(&ctx)
}

#[test]
fn the_collect_phase_collects_every_gate_without_a_report_and_names_each_failure_in_one_line() {
    let fixture = collect_fixture();
    let proc = OnPath(fixture.bin());
    let dir = |name: &str| format!("{EPOCH}/{STEP}/evidence/{name}");
    let report = collect_phase(&fixture, &proc, collect::BUDGET);
    assert_eq!(
        report,
        Report {
            lines: vec![
                format!(
                    "astra-review.sh collect {}: REVIEW_BLOCKED: harness reports effort high",
                    dir("astra-vm-runner-77-r2-blocked")
                ),
                format!(
                    "astra-review.sh collect {}: NOT RECORDED; fix and run: cm review record x",
                    dir("astra-vm-runner-77-r4-unrecorded")
                ),
            ],
            failed: false,
        }
    );
    let collected: Vec<String> = [
        "astra-note-r1",
        "astra-vm-runner-77-r1-pending",
        "astra-vm-runner-77-r2-blocked",
        "astra-vm-runner-77-r3",
        "astra-vm-runner-77-r4-unrecorded",
    ]
    .iter()
    .map(|name| format!("astra collect {}", dir(name)))
    .collect();
    assert_eq!(fixture.calls("astra "), collected);

    let report = collect_phase(&fixture, &proc, collect::BUDGET);
    assert_eq!(report.lines.len(), 2, "{report:?}");
    assert_eq!(
        fixture.calls("astra ")[collected.len()..],
        [
            format!("astra collect {}", dir("astra-vm-runner-77-r1-pending")),
            format!("astra collect {}", dir("astra-vm-runner-77-r2-blocked")),
            format!("astra collect {}", dir("astra-vm-runner-77-r4-unrecorded")),
        ],
        "a collected gate, one with a report, is left alone"
    );
}

#[test]
fn the_collect_phase_stops_at_its_budget_without_a_line() {
    let fixture = collect_fixture();
    let proc = FakeProc::new(|_| Ok(Ran::timed_out()));
    assert_eq!(
        collect_phase(&fixture, &proc, Duration::from_secs(5)),
        Report::default()
    );
    assert_eq!(proc.calls().len(), 1, "{:?}", proc.log());

    let empty = Fixture::new();
    let proc = FakeProc::new(|_| Ok(Ran::code(0, "")));
    assert_eq!(
        collect_phase(&empty, &proc, collect::BUDGET),
        Report::default()
    );
    assert!(proc.calls().is_empty());
}

// The built binary, with fake cm, git, uv, python3, codex_cloud.py and astra-review.sh.

fn built_tick(fixture: &Fixture, next: &Value) -> String {
    write(&fixture.root().join("next.json"), &next.to_string());
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", fixture.bin().display()))
        .env("HOME", fixture.home())
        .env("TMPDIR", fixture.root().join("tmp"))
        .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")
        .env("CODEX_CLOUD_ENVIRONMENT_ID", "acct~env")
        .env("CODEX_CLOUD_PY", fixture.root().join("codex_cloud.py"))
        .args(["tick", "--cm", "cm", "--checkout"])
        .arg(fixture.ct())
        .current_dir(fixture.root())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn the_built_cm_tick_dispatches_starts_astra_and_collects_at_a_later_tick() {
    let fixture = Fixture::new();
    write(
        &fixture.epoch().join("EPOCH.md"),
        "---\nslug: ep\nstatus: WORKING\n---\n",
    );
    script(
        &fixture.ct().join(beat::HANDOFF_PICK),
        &format!("printf '{PICK_OK}'\n"),
    );
    let state = fixture.epoch().join("08-egress/shopping-vm/.cm/state.json");
    let both = |attempt: Value| {
        json!({"steps": {
            "guest-flag": {"lane": "codex", "state": "pr-open", "evidence": {"pr": PR}},
            "new-flag": {"lane": "codex", "state": "drafted", "attempt": attempt},
        }})
    };
    write(&state, &both(json!(null)).to_string());
    write(
        &fixture
            .epoch()
            .join("08-egress/shopping-vm/new-flag/kickoff.md"),
        KICKOFF,
    );
    fixture.fake(
        "cm",
        &format!(
            "echo \"cm $*\" >> '{log}'
case \"$1\" in
  --json) cat '{root}/next.json' ;;
  brief) echo 'the brief' ;;
  launch) printf '%s' '{launched}' > '{state}' ;;
esac
exit 0
",
            log = fixture.log(),
            root = fixture.root().display(),
            launched = both(json!({"claim": CLAIM})),
            state = state.display(),
        ),
    );
    fixture.fake(
        "git",
        &format!(
            "echo \"git $*\" >> '{}'
[ \"$3\" = rev-parse ] && echo '{}'
case $1 in rev-parse|symbolic-ref|branch) echo main ;; esac
exit 0
",
            fixture.log(),
            steering()
        ),
    );
    fixture.fake("uv", "[ \"$1 $2\" = 'run -q' ] && shift 2\nexec \"$@\"\n");
    let receipt = json!({"receipt": {"stage": "turn-admitted", "thread_id": "t-1", "model": "gpt-6.2-sol", "reasoning_effort": "xhigh"}});
    script(
        &fixture.root().join("codex_cloud.py"),
        &format!(
            "echo \"codex_cloud.py $*\" >> '{}'\necho '{receipt}'\n",
            fixture.log()
        ),
    );
    let line = |kind: &str, target: &str, extra: Value| {
        let mut line = json!({"kind": kind, "target": target, "since": "2026-11-25T01:00Z", "command": format!("cm x {target}")});
        line.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        line
    };
    let review = line("review-owed", STEP, json!({"weight": "complex"}));
    let launch = line(
        "launchable-not-launched",
        "08-egress/shopping-vm/new-flag",
        json!({}),
    );
    let out = format!("{EPOCH}/{STEP}/evidence/astra-vm-runner-77-r1");

    let first = built_tick(
        &fixture,
        &json!({"ok": true, "actionable": [launch, review.clone()]}),
    );
    assert_eq!(
        first,
        format!("{}\n", started_line("astra-vm-runner-77-r1"))
    );
    let new = "08-egress/shopping-vm/new-flag";
    assert_eq!(
        fixture.calls("cm attempt"),
        [
            format!("cm attempt -C {EPOCH} {new} --claim {CLAIM} begin thread"),
            format!("cm attempt -C {EPOCH} {new} --claim {CLAIM} record thread t-1"),
        ]
    );
    let created = &fixture.calls("codex_cloud.py ")[0];
    assert!(
        created.starts_with(
            "codex_cloud.py --write jobs create --environment-id acct~env --prompt-file "
        ) && created.ends_with(&format!(
            " --model gpt-6.2-sol --effort xhigh --title {new}"
        )),
        "{created}"
    );
    assert_eq!(fixture.calls("python3 ").len(), 1);
    assert_eq!(
        fixture.calls("astra "),
        [format!(
            "astra start-shadowed dekopon-agents/vm-runner 77 {} {} {out}/brief.md {out} --round 1 --ci unknown",
            base(),
            head()
        )]
    );

    fixture.fake_astra(0, 0, "ACCEPT {}");
    let calls = fs::read_to_string(fixture.root().join("calls")).unwrap();
    let second = built_tick(&fixture, &json!({"ok": true, "actionable": [review]}));
    assert_eq!(
        second,
        format!(
            "review-owed {STEP}: Astra ACCEPT recorded at {} ({out}/report.md); the next move is yours (the fix pass, or the Claude review: merge-ready waits for both verdicts at this head)\n",
            &head()[..12]
        )
    );
    let all = fs::read_to_string(fixture.root().join("calls")).unwrap();
    let later: Vec<&str> = all
        .strip_prefix(calls.as_str())
        .unwrap()
        .lines()
        .filter(|call| call.starts_with("astra ") || call.starts_with("cm --json next"))
        .collect();
    assert_eq!(
        later,
        [
            format!("astra collect {out}"),
            format!("cm --json next -C {EPOCH} --act"),
        ],
        "the collect phase collects before the act list is read, and review-owed does not collect again"
    );
    assert_eq!(
        fs::read_to_string(fixture.ct().join(&out).join("report.md")).unwrap(),
        "findings\n\nACCEPT\n"
    );
}
