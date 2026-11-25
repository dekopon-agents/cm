use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cm::tick::TickError;
use cm::tick::act::{ActLine, Ctx, Env, Executor, Outcome};
use cm::tick::exec::merge_ready::MergeReady;
use cm::tick::exec::released_undeployed::ReleasedUndeployed;
use cm::tick::exec::unblock_now::UnblockNow;
use cm::tick::proc::{Cmd, Proc, Ran, SystemProc};
use serde_json::{Value, json};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NEXT: &str = "cccccccccccccccccccccccccccccccccccccccc";
const PR: &str = "https://github.com/dekopon-agents/vm-runner/pull/77";
const STEP: &str = "08-egress/shopping-vm/guest-flag";
const EPOCH: &str = "store/epochs/ep";
const LIMITS: &str = "driver_model = \"gpt-6-sol\"\ndriver_effort = \"medium\"\n\n[authority]\nmerge = true\nmerge_excluded_repos = [\"scientist-hq/rx\"]\n";
const RELEASE: &str = "00-release/dekopon/0.90.0";
const DEPLOYED: &str = "deploy x: merged xrl/rpi-homelab#530 as mmmmmmmm";
const HOMELAB_PR: &str = "https://github.com/xrl/rpi-homelab/pull/530";

fn digest() -> String {
    format!("sha256:{}", "e".repeat(64))
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

// Shell-quotes text a fixture script holds; paths reach the scripts through FX_* variables.
fn q(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

struct Fx {
    root: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        let fx = Fx {
            root: tempfile::Builder::new()
                .prefix("cm fx's root ")
                .tempdir()
                .unwrap(),
        };
        for dir in [fx.bin(), fx.home(), fx.tmp(), fx.tools()] {
            fs::create_dir_all(dir).unwrap();
        }
        fx.write_state(json!({"lane": "codex", "state": "pr-open", "evidence": {"pr": PR}}));
        fx.observe(json!(HEAD), json!(BASE));
        fx.git(&["init", "-q", "-b", "main"]);
        fx.stamp(LIMITS);
        fx.fake_cm("never-a-subcommand", "");
        fx.fake_gh(BASE);
        fx.fake_merge(0);
        fx.fake_deploy(0, DEPLOYED, Some(530));
        fx
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn ct(&self) -> PathBuf {
        self.file("ct")
    }

    fn epoch(&self) -> PathBuf {
        self.ct().join(EPOCH)
    }

    fn tools(&self) -> PathBuf {
        self.ct().join("tools/cloud-env")
    }

    fn bin(&self) -> PathBuf {
        self.file("bin")
    }

    fn home(&self) -> PathBuf {
        self.file("home")
    }

    fn tmp(&self) -> PathBuf {
        self.file("tmp")
    }

    fn log(&self) -> PathBuf {
        self.file("calls")
    }

    fn note(&self) -> PathBuf {
        self.file("evidence.md")
    }

    fn child_env(&self) -> Vec<(OsString, OsString)> {
        let mut path = OsString::from(self.bin());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        [
            ("PATH", path),
            ("HOME", self.home().into()),
            ("TMPDIR", self.tmp().into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("FX_LOG", self.log().into()),
            ("FX_NOTE", self.note().into()),
            ("FX_RECEIPT", self.file("receipt.json").into()),
            ("FX_MERGED", self.file("merged").into()),
            ("FX_NEXT", self.file("next.json").into()),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
    }

    fn hermetic(&self, cmd: Cmd) -> Cmd {
        self.child_env()
            .into_iter()
            .fold(cmd, |cmd, (key, value)| cmd.env(key, value))
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.ct())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .envs(self.child_env())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn stamp(&self, limits: &str) -> String {
        write(&self.epoch().join("LIMITS.toml"), limits);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", "stamp", "--allow-empty"]);
        let stamp = self.git(&["rev-parse", "HEAD"]);
        self.front(&format!(
            "---\nstatus: WORKING\napproved_plan_revision: {stamp}\n---\n"
        ));
        stamp
    }

    fn front(&self, text: &str) {
        write(&self.epoch().join("EPOCH.md"), text);
    }

    fn observe(&self, head: Value, ci_base: Value) {
        let seen = json!({"target": STEP, "at": "x", "head": head, "base": BASE, "ci_head": head, "ci_base": ci_base});
        self.pulls(json!({PR: seen}));
    }

    fn pulls(&self, pulls: Value) {
        write(&self.epoch().join(".cm/pulls.json"), &pulls.to_string());
    }

    fn write_state(&self, step: Value) {
        write(
            &self.epoch().join("08-egress/shopping-vm/.cm/state.json"),
            &json!({"steps": {"guest-flag": step}}).to_string(),
        );
    }

    fn fake(&self, name: &str, body: &str) {
        self.script(&self.bin().join(name), body);
    }

    fn script(&self, path: &Path, body: &str) {
        write(path, &format!("#!/bin/sh\n{body}"));
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fake_cm(&self, fail_on: &str, refusal: &str) {
        let refusal = if refusal.is_empty() {
            "error: refused"
        } else {
            refusal
        };
        self.fake(
            "cm",
            &format!(
                "if [ \"$1 $2\" = '--json next' ]; then cat \"$FX_NEXT\"; exit 0; fi\n\
                 echo \"cm $*\" >> \"$FX_LOG\"\n\
                 if [ \"$1\" = evidence ]; then for a; do last=$a; done; cp \"$last\" \"$FX_NOTE\"; fi\n\
                 case \"$1\" in {fail_on}) printf '%s\\n' {} >&2; exit 1 ;; esac\n\
                 exit 0\n",
                q(refusal)
            ),
        );
    }

    fn fake_gh(&self, base: &str) {
        self.fake(
            "gh",
            &format!(
                "echo \"gh $*\" >> \"$FX_LOG\"\nprintf '%s\\n' main {}\n",
                q(base)
            ),
        );
    }

    fn fake_merge(&self, code: i32) {
        self.fake_merge_printing(
            &format!("{{\"status\": \"MERGED\", \"merge_sha\": \"{NEXT}\"}}"),
            code,
        );
    }

    // merge-ready runs it with python3 -I, so the fake is Python, and it logs the -I it ran under.
    fn fake_merge_printing(&self, stdout: &str, code: i32) {
        write(&self.file("merged"), stdout);
        write(
            &self.tools().join("merge-pr.py"),
            &format!(
                "import os, sys\n\
                 isolated = '-I ' if sys.flags.isolated else ''\n\
                 open(os.environ['FX_LOG'], 'a').write('merge ' + isolated + ' '.join(sys.argv[1:]) + '\\n')\n\
                 print(open(os.environ['FX_MERGED']).read())\n\
                 sys.exit({code})\n"
            ),
        );
    }

    fn fake_deploy(&self, code: i32, said: &str, pr: Option<u32>) {
        let receipt = json!({
            "release": "dekopon@0.90.0",
            "pr": pr.map(|pr| format!("https://github.com/xrl/rpi-homelab/pull/{pr}")),
            "merge_sha": pr.map(|_| "m".repeat(40)),
            "digest": digest(),
        });
        self.fake_deploy_writing(code, said, &receipt.to_string());
    }

    fn fake_deploy_writing(&self, code: i32, said: &str, receipt: &str) {
        let file = self.file("receipt.json");
        if receipt.is_empty() {
            let _ = fs::remove_file(&file);
        } else {
            write(&file, receipt);
        }
        self.script(
            &self.tools().join("deploy.sh"),
            &format!(
                "echo \"deploy $*\" >> \"$FX_LOG\"\n\
                 [ {code} -ne 0 ] || [ \"$2\" != --receipt ] || [ ! -f \"$FX_RECEIPT\" ] || cp \"$FX_RECEIPT\" \"$3\"\n\
                 printf '%s\\n' {}\n\
                 exit {code}\n",
                q(said)
            ),
        );
    }

    fn env(&self) -> Env {
        [("TMPDIR", self.tmp().to_str().unwrap())]
            .into_iter()
            .collect()
    }

    fn run(&self, executor: &dyn Executor, json: Value) -> Outcome {
        self.run_with(executor, json, &self.env(), &Scripts(self))
    }

    fn run_with(
        &self,
        executor: &dyn Executor,
        json: Value,
        env: &Env,
        proc: &dyn Proc,
    ) -> Outcome {
        let ct = self.ct();
        let ctx = Ctx::new(
            &ct,
            Path::new(EPOCH),
            proc,
            env,
            OsStr::new("cm"),
            Duration::from_secs(120),
        );
        executor.run(&ActLine::read(json, &self.epoch()), &ctx)
    }

    fn calls(&self, prefix: &str) -> Vec<String> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with(prefix))
            .map(str::to_owned)
            .collect()
    }

    fn merge(&self) -> Outcome {
        self.merge_line(json!({}))
    }

    fn merge_line(&self, fields: Value) -> Outcome {
        let mut line = json!({"kind": "merge-ready", "target": STEP, "command": format!("cm advance {STEP} merged --pr {PR} --sha '<merge-sha>'")});
        line.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        self.run(&MergeReady, line)
    }

    fn deploy(&self, release: &str) -> Outcome {
        self.run(&ReleasedUndeployed, deploy_line(release))
    }
}

fn deploy_line(release: &str) -> Value {
    json!({"kind": "released-undeployed", "target": RELEASE, "command": format!("tools/cloud-env/deploy.sh {release}")})
}

fn text(json: &Value) -> String {
    ActLine {
        json: json.clone(),
        lane: None,
    }
    .text()
}

fn merge_text() -> String {
    format!("merge-ready {STEP}: cm advance {STEP} merged --pr {PR} --sha '<merge-sha>'")
}

fn left(note: &str) -> Outcome {
    Outcome::Left(format!("{} [{note}]", merge_text()))
}

fn stale(ran_on: &str, now: &str) -> Outcome {
    left(&format!(
        "stale base: CI ran on {ran_on}, the base is now {now}; update the branch and wait for green CI"
    ))
}

fn done() -> Outcome {
    Outcome::Done(Vec::new())
}

fn failed(reason: &str) -> Outcome {
    Outcome::Failed(reason.to_owned())
}

fn failed_with(outcome: &Outcome, prefix: &str) -> bool {
    matches!(outcome, Outcome::Failed(reason) if reason.starts_with(prefix))
}

fn receipts(fx: &Fx, head: &str) -> PathBuf {
    fx.tmp().join(format!(
        "dekopon-merge/dekopon-agents-vm-runner-77-{}",
        &head[..12]
    ))
}

struct Scripts<'a>(&'a Fx);

impl Proc for Scripts<'_> {
    fn run(&self, cmd: &Cmd) -> cm::tick::Result<Ran> {
        SystemProc.run(&self.0.hermetic(cmd.clone()))
    }
}

struct Missing<'a>(&'a Fx, &'a str);

impl Proc for Missing<'_> {
    fn run(&self, cmd: &Cmd) -> cm::tick::Result<Ran> {
        if cmd.program_name() == self.1 {
            return Err(TickError::Spawn {
                program: self.1.to_owned(),
                source: std::io::ErrorKind::NotFound.into(),
            });
        }
        Scripts(self.0).run(cmd)
    }
}

struct ShowFails<'a>(&'a Fx);

impl Proc for ShowFails<'_> {
    fn run(&self, cmd: &Cmd) -> cm::tick::Result<Ran> {
        if cmd.program_name() == "git" && cmd.argv.iter().any(|arg| arg == "show") {
            return Ok(Ran::code(128, LIMITS).with_stderr("fatal: bad object"));
        }
        Scripts(self.0).run(cmd)
    }
}

#[test]
fn merge_ready_merges_at_the_observed_head_then_advances() {
    let fx = Fx::new();
    assert_eq!(fx.merge(), done());
    assert_eq!(
        fx.calls(""),
        [
            "gh api repos/dekopon-agents/vm-runner/pulls/77 --jq .base.ref, .base.sha".to_owned(),
            format!(
                "merge -I dekopon-agents/vm-runner 77 {HEAD} --base main --evidence {}",
                receipts(&fx, HEAD).display()
            ),
            format!("cm advance -C {EPOCH} {STEP} merged --pr {PR} --sha {NEXT}"),
        ]
    );
}

#[test]
fn fixtures_run_under_a_path_with_an_apostrophe_and_a_space() {
    let fx = Fx::new();
    let root = fx.root.path().to_str().unwrap();
    assert!(root.contains('\'') && root.contains(' '), "{root}");
    assert_eq!(fx.merge(), done());
    let line =
        json!({"kind": "unblock-now", "target": STEP, "command": format!("cm unblock {STEP}")});
    assert_eq!(fx.run(&UnblockNow, line), done());
    assert_eq!(fx.deploy(RELEASE), done());
    assert_eq!(fx.calls("merge ").len(), 1);
    assert_eq!(fx.calls("cm ").len(), 4);
    assert!(fs::read_to_string(fx.note()).unwrap().contains(DEPLOYED));
}

#[test]
fn a_stale_base_goes_to_the_model() {
    let fx = Fx::new();
    fx.observe(json!(HEAD), json!(NEXT));
    assert_eq!(fx.merge(), stale("cccccccccccc", "bbbbbbbbbbbb"));
    assert_eq!(fx.calls("merge "), Vec::<String>::new());
}

#[test]
fn a_base_that_moved_since_cm_looked_is_stale_and_never_merged() {
    let fx = Fx::new();
    fx.fake_gh(NEXT);
    assert_eq!(fx.merge(), stale("bbbbbbbbbbbb", "cccccccccccc"));
    assert_eq!(fx.calls("merge "), Vec::<String>::new());
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}

#[test]
fn a_base_cm_never_saw_ci_run_on_is_stale_and_named() {
    let fx = Fx::new();
    for ci_base in [json!(null), json!("")] {
        fx.observe(json!(HEAD), ci_base);
        assert_eq!(fx.merge(), stale("no observed base", "bbbbbbbbbbbb"));
    }
    fx.pulls(json!({PR: {"head": HEAD}}));
    assert_eq!(
        fx.merge(),
        stale("no observed base", "no observed base"),
        "neither a CI base nor a base"
    );
    assert_eq!(fx.calls("merge "), Vec::<String>::new());
}

#[test]
fn a_receipt_from_an_earlier_merge_attempt_refuses_a_second_merge() {
    let fx = Fx::new();
    write(&receipts(&fx, HEAD).join("merge-attempted.json"), "{}");
    assert_eq!(
        fx.merge(),
        failed(&format!(
            "an earlier merge attempt left {}; reconcile {PR} before any retry",
            receipts(&fx, HEAD).display()
        ))
    );
    assert_eq!(
        (fx.calls("merge "), fx.calls("cm ")),
        (Vec::new(), Vec::new())
    );
}

#[test]
fn a_receipt_directory_that_cannot_be_checked_refuses_the_merge() {
    let fx = Fx::new();
    write(&fx.tmp().join("dekopon-merge"), "a file, not a directory");
    let outcome = fx.merge();
    assert!(
        failed_with(
            &outcome,
            &format!(
                "cannot tell whether an earlier merge attempt left {}: ",
                receipts(&fx, HEAD).display()
            )
        ),
        "{outcome:?}"
    );
    assert_eq!(fx.calls("merge "), Vec::<String>::new());
}

#[test]
fn an_empty_or_relative_tmpdir_refuses_the_merge_before_any_effect() {
    let fx = Fx::new();
    for (tmpdir, reason) in [
        (
            "",
            "TMPDIR is set but empty; receipts need a private directory",
        ),
        (
            "scratch",
            "TMPDIR scratch is relative; receipts need a private directory",
        ),
    ] {
        let env = [("TMPDIR", tmpdir)].into_iter().collect();
        let line = json!({"kind": "merge-ready", "target": STEP, "command": "merge it"});
        assert_eq!(
            fx.run_with(&MergeReady, line, &env, &Scripts(&fx)),
            failed(reason)
        );
    }
    assert_eq!(
        (fx.calls("merge "), fx.calls("cm ")),
        (Vec::new(), Vec::new())
    );
}

#[test]
fn merge_ready_without_cms_advance_command_builds_it() {
    let built = format!("cm advance -C {EPOCH} {STEP} merged --pr {PR} --sha {NEXT}");
    for command in [
        "merge it".to_owned(),
        format!("cm advance {STEP} merged --pr {PR} --sha '<merge-sha>"),
        "cm advance 08-egress/shopping-vm/other merged --sha '<merge-sha>'".to_owned(),
        format!("cm advance {STEP} merged --pr {PR}"),
    ] {
        let fx = Fx::new();
        let line = json!({"kind": "merge-ready", "target": STEP, "command": command});
        assert_eq!(fx.run(&MergeReady, line), done(), "{command}");
        assert_eq!(fx.calls("cm "), std::slice::from_ref(&built), "{command}");
    }
}

#[test]
fn a_command_that_is_not_a_string_merges_then_builds_the_advance() {
    for command in [json!(5), json!(null), json!(["cm", "advance"])] {
        let fx = Fx::new();
        let line = json!({"kind": "merge-ready", "target": STEP, "command": command});
        assert_eq!(fx.run(&MergeReady, line), done(), "{command}");
        assert_eq!(
            fx.calls("cm "),
            [format!(
                "cm advance -C {EPOCH} {STEP} merged --pr {PR} --sha {NEXT}"
            )],
            "{command}"
        );
    }
}

#[test]
fn a_command_that_is_not_a_string_fails_an_unblock_or_a_deploy() {
    let fx = Fx::new();
    for command in [json!(5), json!(null), json!(["cm", "unblock", STEP])] {
        let reason = failed(&format!(
            "the act line's command is {command}, not a string"
        ));
        let line = json!({"kind": "unblock-now", "target": STEP, "command": command});
        assert_eq!(fx.run(&UnblockNow, line), reason, "{command}");
        let line = json!({"kind": "released-undeployed", "target": RELEASE, "command": command});
        assert_eq!(fx.run(&ReleasedUndeployed, line), reason, "{command}");
    }
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn an_observation_that_is_not_an_object_fails_before_any_merge() {
    for seen in [json!("x"), json!([HEAD]), json!(7)] {
        let fx = Fx::new();
        fx.pulls(json!({PR: seen}));
        let reason = failed(&format!(
            "cm's observation of {PR} in .cm/pulls.json is {seen}, not an object"
        ));
        assert_eq!(fx.merge(), reason, "{seen}");
        assert_eq!(
            fx.merge_line(json!({"head": HEAD, "base": BASE})),
            reason,
            "{seen}"
        );
        assert_eq!(fx.calls(""), Vec::<String>::new(), "{seen}");
    }
}

#[test]
fn cms_advance_command_is_run_with_the_merge_sha_in_its_place() {
    let fx = Fx::new();
    let line = json!({"kind": "merge-ready", "target": STEP, "command": format!("cm advance '{STEP}' merged --sha \"<merge-sha>\" --pr {PR} --note 'a b'")});
    assert_eq!(fx.run(&MergeReady, line), done());
    assert_eq!(
        fx.calls("cm "),
        [format!(
            "cm advance -C {EPOCH} {STEP} merged --sha {NEXT} --pr {PR} --note a b"
        )]
    );
}

#[test]
fn a_refused_advance_after_the_merge_is_one_line_naming_it() {
    let fx = Fx::new();
    fx.fake_cm("advance", "");
    assert_eq!(
        fx.merge(),
        Outcome::Done(vec![format!(
            "merge-ready {STEP}: merged {PR} as cccccccccccc; cm advance refused (error: refused); cm sync records it"
        )])
    );
    assert_eq!(fx.calls("merge ").len(), 1);
}

#[test]
fn dekopon_merges_stay_on_its_merge_queue() {
    for repo in ["dekopon-agents/dekopon", "Dekopon-Agents/DEKOPON"] {
        let fx = Fx::new();
        let pr = format!("https://github.com/{repo}/pull/1011");
        fx.write_state(json!({"lane": "codex", "state": "pr-open", "evidence": {"pr": pr}}));
        fx.front("---\nstatus: WORKING\n---\n");
        assert_eq!(
            fx.merge(),
            left(
                "dekopon merges through its merge queue: check the drop tally, then enable_pr_auto_merge"
            ),
            "{repo}"
        );
        assert_eq!(fx.calls(""), Vec::<String>::new(), "{repo}");
    }
}

#[test]
fn a_repository_the_stamp_excludes_is_not_merged() {
    for repo in ["scientist-hq/rx", "Scientist-HQ/RX"] {
        let fx = Fx::new();
        let pr = format!("https://github.com/{repo}/pull/5");
        fx.write_state(json!({"lane": "codex", "state": "pr-open", "evidence": {"pr": pr}}));
        assert_eq!(
            fx.merge(),
            left(&format!(
                "the stamp's [authority] does not let a script merge in {repo}"
            ))
        );
        assert_eq!(fx.calls(""), Vec::<String>::new(), "{repo}");
    }
}

#[test]
fn only_merge_true_at_the_stamp_lets_a_script_merge() {
    for authority in [
        "[authority]\nmerge = false\n",
        "[authority]\n",
        "[authority]\nmerge = \"true\"\n",
        "[authority]\nmerge = 1\n",
    ] {
        let fx = Fx::new();
        fx.stamp(authority);
        assert_eq!(
            fx.merge(),
            left("the stamp's [authority] does not let a script merge in dekopon-agents/vm-runner"),
            "{authority}"
        );
        assert_eq!(fx.calls(""), Vec::<String>::new(), "{authority}");
    }
}

#[test]
fn the_stamp_table_is_read_at_the_stamp_sha_not_the_working_tree() {
    let fx = Fx::new();
    write(
        &fx.epoch().join("LIMITS.toml"),
        &LIMITS.replace("[\"scientist-hq/rx\"]", "[\"dekopon-agents/vm-runner\"]"),
    );
    assert_eq!(fx.merge(), done());
    assert_eq!(fx.calls("merge ").len(), 1);

    let fx = Fx::new();
    write(
        &fx.epoch().join("LIMITS.toml"),
        "[authority]\nmerge = false\n",
    );
    fx.git(&["commit", "-qam", "after the stamp"]);
    assert_eq!(fx.merge(), done(), "HEAD is not the stamp");
    assert_eq!(fx.calls("merge ").len(), 1);
}

#[test]
fn without_a_stamp_nothing_merges() {
    let fx = Fx::new();
    let stamp = fx.git(&["rev-parse", "HEAD"]);
    for front in [
        "---\nstatus: WORKING\n---\n".to_owned(),
        format!("approved_plan_revision: {}\n", &stamp[..39]),
        format!("approved_plan_revision: {stamp} \n"),
    ] {
        fx.front(&front);
        assert_eq!(
            fx.merge(),
            failed("EPOCH.md has no approved_plan_revision; no stamp, no merge"),
            "{front}"
        );
    }
    fs::remove_file(fx.epoch().join("EPOCH.md")).unwrap();
    let outcome = fx.merge();
    assert!(
        failed_with(&outcome, &format!("cannot read {EPOCH}/EPOCH.md: ")),
        "{outcome:?}"
    );
    assert!(
        matches!(&outcome, Outcome::Failed(reason) if reason.ends_with("; no stamp, no merge"))
    );
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn an_authority_that_cannot_be_read_at_the_stamp_merges_nothing() {
    let unreadable = "cannot read [authority] from LIMITS.toml at the stamp SHA: ";
    let cases = [
        ("driver_model = \"x\"\n", "driver_model = \"x\""),
        ("authority = true\n", "authority = true"),
        ("[authority\nmerge = true\n", "merge = true"),
        (
            "[authority]\nmerge = true\nmerge_excluded_repos = \"scientist-hq/rx\"\n",
            "merge_excluded_repos is not a list",
        ),
        ("\u{feff}[authority]\nmerge = true\n", "merge = true"),
    ];
    for (limits, why) in cases {
        let fx = Fx::new();
        fx.stamp(limits);
        assert_eq!(
            fx.merge(),
            failed(&format!("{unreadable}{why}")),
            "{limits}"
        );
        assert_eq!(fx.calls(""), Vec::<String>::new(), "{limits}");
    }

    let fx = Fx::new();
    fs::remove_file(fx.epoch().join("LIMITS.toml")).unwrap();
    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-qm", "no limits"]);
    let stamp = fx.git(&["rev-parse", "HEAD"]);
    fx.front(&format!("approved_plan_revision: {stamp}\n"));
    write(&fx.epoch().join("LIMITS.toml"), LIMITS);
    let outcome = fx.merge();
    assert!(failed_with(&outcome, unreadable), "{outcome:?}");
    assert!(
        matches!(&outcome, Outcome::Failed(reason) if reason.contains(&format!("{EPOCH}/LIMITS.toml"))),
        "git's own refusal: {outcome:?}"
    );
    assert_eq!(fx.calls(""), Vec::<String>::new());

    let fx = Fx::new();
    fx.front(&format!("approved_plan_revision: {}\n", "d".repeat(40)));
    let outcome = fx.merge();
    assert!(failed_with(&outcome, unreadable), "{outcome:?}");
    assert_eq!(fx.calls(""), Vec::<String>::new());

    let fx = Fx::new();
    let line = json!({"kind": "merge-ready", "target": STEP, "command": "merge it"});
    assert_eq!(
        fx.run_with(&MergeReady, line, &fx.env(), &ShowFails(&fx)),
        failed(&format!("{unreadable}fatal: bad object")),
        "git show's output is read only when it exits 0"
    );
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn toml_1_1_is_cms_dialect_for_the_stamp() {
    let fx = Fx::new();
    fx.stamp("x = { a = 1,\n b = 2, }\nwhen = 07:32\n\n[authority]\nmerge = true\n");
    assert_eq!(fx.merge(), done());
    assert_eq!(fx.calls("merge ").len(), 1);
}

#[test]
fn a_failed_merge_fails_and_advances_nothing() {
    let fx = Fx::new();
    fx.fake_merge(1);
    assert_eq!(
        fx.merge(),
        failed(&format!(
            "merge-pr.py: {{\"status\": \"MERGED\", \"merge_sha\": \"{NEXT}\"}}"
        ))
    );
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}

#[test]
fn a_merge_without_a_json_receipt_fails_and_advances_nothing() {
    for printed in ["merged, I think", "[\"MERGED\"]"] {
        let fx = Fx::new();
        fx.fake_merge_printing(printed, 0);
        assert_eq!(
            fx.merge(),
            failed(&format!(
                "merge-pr.py merged {PR} but printed no JSON receipt ({printed}); record the merge by hand"
            ))
        );
        assert_eq!(fx.calls("merge ").len(), 1);
        assert_eq!(fx.calls("cm "), Vec::<String>::new());
    }

    let fx = Fx::new();
    fx.fake_merge_printing("{\"merge_sha\": 5}", 0);
    assert_eq!(
        fx.merge(),
        failed(&format!(
            "merge-pr.py merged {PR} but its merge_sha is 5, not a string; record the merge by hand"
        ))
    );
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}

#[test]
fn a_merge_needs_an_observed_full_head() {
    for (pulls, head) in [
        (json!({}), None),
        (json!({PR: {"ci_base": BASE, "base": BASE}}), None),
        (
            json!({PR: {"head": HEAD, "ci_base": BASE, "base": BASE}}),
            Some(json!("abc")),
        ),
        (
            json!({PR: {"head": "A".repeat(40), "ci_base": BASE, "base": BASE}}),
            None,
        ),
        (
            json!({PR: {"head": 7, "ci_base": BASE, "base": BASE}}),
            None,
        ),
    ] {
        let fx = Fx::new();
        fx.pulls(pulls.clone());
        let fields = head.map_or_else(|| json!({}), |head| json!({"head": head}));
        assert_eq!(
            fx.merge_line(fields),
            failed(&format!(
                "no observed head for {PR} in .cm/pulls.json; cm sync first"
            )),
            "{pulls}"
        );
        assert_eq!(fx.calls(""), Vec::<String>::new());
    }
}

#[test]
fn cm_017_merge_ready_lines_carry_pr_head_and_base() {
    let fx = Fx::new();
    fx.write_state(json!({"lane": "codex", "state": "pr-open"}));
    fx.pulls(json!({PR: {"head": HEAD, "base": NEXT, "ci_base": BASE}}));
    assert_eq!(
        fx.merge_line(json!({"pr": PR, "head": NEXT, "base": BASE})),
        done()
    );
    assert_eq!(
        fx.calls("merge "),
        [format!(
            "merge -I dekopon-agents/vm-runner 77 {NEXT} --base main --evidence {}",
            receipts(&fx, NEXT).display()
        )]
    );

    let fx = Fx::new();
    fx.observe(json!(HEAD), json!(BASE));
    assert_eq!(
        fx.merge_line(json!({"base": NEXT})),
        stale("bbbbbbbbbbbb", "cccccccccccc")
    );
    assert_eq!(fx.calls("merge "), Vec::<String>::new());
}

#[test]
fn a_merge_ready_line_without_a_pr_or_state_fails() {
    let fx = Fx::new();
    fx.write_state(json!({"lane": "codex", "state": "pr-open"}));
    assert_eq!(fx.merge(), failed(&format!("{STEP} records no PR URL")));
    let line =
        json!({"kind": "merge-ready", "target": "08-egress/nowhere/x", "command": "merge it"});
    assert_eq!(
        fx.run(&MergeReady, line),
        failed("no cm state for 08-egress/nowhere/x")
    );
    let line = json!({"kind": "merge-ready", "command": "merge it"});
    assert_eq!(
        fx.run(&MergeReady, line),
        failed("the act line has no target")
    );
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn a_pr_source_of_the_wrong_type_fails_as_the_script_crashed() {
    let wrong = [json!(5), json!([PR]), json!({"url": PR}), json!(true)];
    for pr in &wrong {
        let fx = Fx::new();
        assert_eq!(
            fx.merge_line(json!({"pr": pr})),
            failed(&format!("the act line's pr is {pr}, not a PR URL")),
            "{pr}"
        );
        fx.write_state(json!({"evidence": {"pr": pr}, "pr": PR}));
        assert_eq!(
            fx.merge(),
            failed(&format!("the step's evidence.pr is {pr}, not a PR URL"))
        );
        fx.write_state(json!({"pr": pr}));
        assert_eq!(
            fx.merge(),
            failed(&format!("the step's pr is {pr}, not a PR URL"))
        );
        assert_eq!(fx.calls(""), Vec::<String>::new(), "{pr}");
    }
    for evidence in [json!("x"), json!([PR]), json!(5), json!(true)] {
        let fx = Fx::new();
        fx.write_state(json!({"evidence": evidence, "pr": PR}));
        assert_eq!(
            fx.merge(),
            failed(&format!("the step's evidence is {evidence}, not an object"))
        );
        assert_eq!(fx.calls(""), Vec::<String>::new(), "{evidence}");
    }
}

#[test]
fn a_pr_source_the_script_skips_or_never_reads_is_not_checked() {
    let fx = Fx::new();
    fx.write_state(json!({"evidence": "x", "pr": 5}));
    assert_eq!(
        fx.merge_line(json!({"pr": PR})),
        done(),
        "the line's pr wins"
    );
    for skipped in [
        json!(0),
        json!(""),
        json!(null),
        json!(false),
        json!([]),
        json!({}),
    ] {
        let fx = Fx::new();
        fx.write_state(json!({"evidence": {"pr": skipped}, "pr": PR}));
        assert_eq!(fx.merge_line(json!({"pr": skipped})), done(), "{skipped}");
        fx.write_state(json!({"evidence": skipped, "pr": PR}));
        assert_eq!(fx.merge(), done(), "{skipped}");
    }
}

#[test]
fn a_parts_merge_is_the_models() {
    let fx = Fx::new();
    assert_eq!(
        fx.merge_line(json!({"part": "api"})),
        failed("a part's merge is the model's")
    );
    assert_eq!(fx.merge_line(json!({"part": null})), done());
    assert_eq!(fx.calls("merge ").len(), 1);
}

#[test]
fn a_base_branch_that_cannot_be_read_fails_before_any_merge() {
    for (gh, said) in [
        (
            "echo 'HTTP 404: Not Found' >&2\nexit 1\n".to_owned(),
            "HTTP 404: Not Found",
        ),
        (format!("printf 'main\\n{BASE}\\n'\nexit 1\n"), BASE),
        ("echo '  '\n".to_owned(), "exit 0"),
        ("echo main\n".to_owned(), "main"),
        (format!("printf ' \\n{BASE}\\n'\n"), BASE),
        ("printf 'main\\nnot-a-sha\\n'\n".to_owned(), "not-a-sha"),
        (format!("printf 'main\\n{BASE}\\nmore\\n'\n"), "more"),
    ] {
        let fx = Fx::new();
        fx.fake("gh", &gh);
        assert_eq!(
            fx.merge(),
            failed(&format!(
                "cannot read the base branch and SHA of {PR}: {said}"
            )),
            "{gh}"
        );
        assert_eq!(fx.calls(""), Vec::<String>::new());
    }
}

#[test]
fn a_tool_that_cannot_start_fails_the_line_naming_it() {
    let fx = Fx::new();
    let missing = |program| {
        let line = json!({"kind": "merge-ready", "target": STEP, "command": "merge it"});
        fx.run_with(&MergeReady, line, &fx.env(), &Missing(&fx, program))
    };
    let cannot = |program: &str| {
        failed(&format!(
            "{program}: {}",
            std::io::Error::from(std::io::ErrorKind::NotFound)
        ))
    };
    assert_eq!(missing("git"), cannot("git"));
    assert_eq!(missing("gh"), cannot("gh"));
    assert_eq!(missing("python3"), cannot("python3"));
    assert_eq!(fx.calls("merge "), Vec::<String>::new());
    assert_eq!(
        missing("cm"),
        Outcome::Done(vec![format!(
            "merge-ready {STEP}: merged {PR} as cccccccccccc; cm advance refused (cm: {}); cm sync records it",
            std::io::Error::from(std::io::ErrorKind::NotFound)
        )])
    );

    let line = deploy_line(RELEASE);
    assert_eq!(
        fx.run_with(&ReleasedUndeployed, line, &fx.env(), &Missing(&fx, "bash")),
        cannot("bash")
    );
    let line =
        json!({"kind": "unblock-now", "target": STEP, "command": format!("cm unblock {STEP}")});
    assert_eq!(
        fx.run_with(&UnblockNow, line, &fx.env(), &Missing(&fx, "cm")),
        failed(&format!(
            "cm unblock failed: cm: {}",
            std::io::Error::from(std::io::ErrorKind::NotFound)
        ))
    );
}

#[test]
fn unblock_now_runs_the_printed_unblock() {
    let fx = Fx::new();
    let line = json!({"kind": "unblock-now", "target": STEP, "command": format!("cm unblock {STEP} --evidence {STEP}/evidence/x.md")});
    assert_eq!(fx.run(&UnblockNow, line), done());
    assert_eq!(
        fx.calls("cm "),
        [format!(
            "cm unblock -C {EPOCH} {STEP} --evidence {STEP}/evidence/x.md"
        )]
    );
}

#[test]
fn unblock_now_leaves_an_owner_step_to_the_model() {
    let fx = Fx::new();
    let line = json!({"kind": "unblock-now", "target": "04-owner/rx-upstream-train/merge", "command": "cm unblock 04-owner/rx-upstream-train/merge"});
    assert_eq!(
        fx.run(&UnblockNow, line),
        Outcome::Left(
            "unblock-now 04-owner/rx-upstream-train/merge: cm unblock 04-owner/rx-upstream-train/merge [owner step: unblock it and file its OWNER-QUEUE item]"
                .to_owned()
        )
    );
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}

#[test]
fn unblock_now_refuses_a_placeholder_or_another_command() {
    let fx = Fx::new();
    for command in [
        format!("cm unblock {STEP} --evidence \"<file>\""),
        format!("cm advance {STEP} done"),
        "rm -rf /".to_owned(),
        format!("cm unblock '{STEP}"),
        "cm".to_owned(),
        "cm  unblock\\".to_owned(),
        String::new(),
    ] {
        let line = json!({"kind": "unblock-now", "target": STEP, "command": command});
        assert_eq!(
            fx.run(&UnblockNow, line),
            failed(&format!("not a runnable cm unblock: {command}"))
        );
    }
    let line = json!({"kind": "unblock-now", "target": STEP});
    assert_eq!(
        fx.run(&UnblockNow, line),
        failed("not a runnable cm unblock: ")
    );
    let line = json!({"kind": "unblock-now", "command": "cm unblock x"});
    assert_eq!(
        fx.run(&UnblockNow, line),
        failed("the act line has no target")
    );
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}

#[test]
fn a_refused_unblock_fails_naming_it() {
    let fx = Fx::new();
    fx.fake_cm("unblock", "error: the block condition does not hold");
    let line =
        json!({"kind": "unblock-now", "target": STEP, "command": format!("cm unblock {STEP}")});
    assert_eq!(
        fx.run(&UnblockNow, line),
        failed("cm unblock failed: error: the block condition does not hold")
    );
}

#[test]
fn a_merged_deploy_is_recorded_in_cm() {
    let fx = Fx::new();
    assert_eq!(fx.deploy(RELEASE), done());
    assert_eq!(
        fx.calls("deploy ")[0].split(" --receipt ").next(),
        Some(format!("deploy {RELEASE}").as_str())
    );
    let cm = fx.calls("cm ");
    assert!(
        cm[0].starts_with(&format!(
            "cm evidence -C {EPOCH} {RELEASE} --name deploy-mmmmmmmmmmmm --body-file "
        )),
        "{cm:?}"
    );
    assert_eq!(
        cm[1..],
        [format!(
            "cm advance -C {EPOCH} {RELEASE} deployed --digest {}",
            digest()
        )]
    );
    assert_eq!(
        fs::read_to_string(fx.note()).unwrap(),
        format!(
            "# Deploy of dekopon@0.90.0\n\n- deploy PR: {HOMELAB_PR}\n- merge SHA: {}\n- digest: {}\n- deploy.sh: {DEPLOYED}\n",
            "m".repeat(40),
            digest()
        )
    );
    assert_eq!(
        fs::read_dir(fx.tmp()).unwrap().count(),
        0,
        "the receipt and note are removed"
    );
}

#[test]
fn a_refused_advance_is_one_line_naming_it() {
    let fx = Fx::new();
    fx.fake_cm("advance", "");
    assert_eq!(
        fx.deploy(RELEASE),
        failed(&format!(
            "deployed {RELEASE} ({HOMELAB_PR}); cm advance {RELEASE} deployed refused: error: refused"
        ))
    );
}

#[test]
fn already_deployed_is_recorded_too() {
    let fx = Fx::new();
    fx.fake_deploy(0, "deploy x: already deployed", None);
    assert_eq!(fx.deploy(RELEASE), done());
    assert!(
        fx.calls("cm ")[0].contains("--name deploy-already "),
        "{:?}",
        fx.calls("cm ")
    );
    assert_eq!(
        fs::read_to_string(fx.note()).unwrap(),
        format!(
            "# Deploy of dekopon@0.90.0\n\n- deploy PR: none: main already ran it\n- merge SHA: none\n- digest: {}\n- deploy.sh: deploy x: already deployed\n",
            digest()
        )
    );
    fx.fake_cm("advance", "");
    assert_eq!(
        fx.deploy(RELEASE),
        failed(&format!(
            "deployed {RELEASE} (already on main); cm advance {RELEASE} deployed refused: error: refused"
        ))
    );
}

#[test]
fn a_drafted_deploy_steps_line_is_the_models() {
    let fx = Fx::new();
    for command in [
        "cm launch 13-rx/issuer-core/pin",
        "tools/cloud-env/deploy.sh '<release>'",
        "tools/cloud-env/deploy.sh a b",
        "tools/cloud-env/deploy.sh",
        "tools/cloud-env/deploy.py 00-release/dekopon/0.90.0",
    ] {
        let line = json!({"kind": "released-undeployed", "target": "13-rx/issuer-core/pin", "command": command});
        assert_eq!(
            fx.run(&ReleasedUndeployed, line.clone()),
            Outcome::Left(format!("{} [a deploy step's own move: yours]", text(&line)))
        );
    }
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn a_deploy_line_that_does_not_split_into_words_fails() {
    let fx = Fx::new();
    let line = json!({"kind": "released-undeployed", "target": RELEASE, "command": "tools/cloud-env/deploy.sh '00-release"});
    assert_eq!(
        fx.run(&ReleasedUndeployed, line),
        failed("cannot split into words: tools/cloud-env/deploy.sh '00-release")
    );
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn a_deploy_for_a_step_that_is_not_a_release_is_refused() {
    let fx = Fx::new();
    let line = json!({"kind": "released-undeployed", "target": "13-rx/issuer-core/pin", "command": format!("tools/cloud-env/deploy.sh {RELEASE}")});
    assert_eq!(
        fx.run(&ReleasedUndeployed, line),
        failed("13-rx/issuer-core/pin is not a release step; deploy by hand")
    );
    let line = json!({"kind": "released-undeployed", "command": format!("tools/cloud-env/deploy.sh {RELEASE}")});
    assert_eq!(
        fx.run(&ReleasedUndeployed, line),
        failed("the act line has no target")
    );
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn released_undeployed_passes_cms_release_name() {
    let fx = Fx::new();
    for release in [
        "00-release/dekopon/0.90.0",
        "tag:dekopon@0.90.0",
        "dekopon-provider-gh@0.13.2",
    ] {
        assert_eq!(fx.deploy(release), done(), "{release}");
    }
    assert_eq!(
        fx.calls("deploy ")
            .iter()
            .map(|call| call.split(" --receipt ").next().unwrap())
            .collect::<Vec<_>>(),
        [
            "deploy 00-release/dekopon/0.90.0",
            "deploy tag:dekopon@0.90.0",
            "deploy dekopon-provider-gh@0.13.2"
        ]
    );
}

#[test]
fn deploy_waiting_on_ci_is_one_line_and_no_failure() {
    let fx = Fx::new();
    fx.fake_deploy(3, "rpi-homelab#530 waiting on render.yml", Some(530));
    assert_eq!(
        fx.deploy(RELEASE),
        Outcome::Left(format!(
            "released-undeployed {RELEASE}: {RELEASE} waiting on CI (rpi-homelab#530 waiting on render.yml)"
        ))
    );
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}

#[test]
fn a_failed_deploy_is_a_failure() {
    let fx = Fx::new();
    for code in [1, 2] {
        fx.fake_deploy(code, "render.yml red", Some(530));
        assert_eq!(
            fx.deploy(RELEASE),
            failed(&format!("deploy.sh {RELEASE}: render.yml red"))
        );
    }
    fx.script(
        &fx.tools().join("deploy.sh"),
        "echo 'render.yml red'\nkill -KILL $$\n",
    );
    assert_eq!(
        fx.deploy(RELEASE),
        failed(&format!("deploy.sh {RELEASE}: render.yml red"))
    );
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}

#[test]
fn a_merged_deploy_without_a_usable_receipt_fails_and_records_nothing() {
    let cases = [
        ("", "wrote no receipt"),
        ("not json", "wrote no receipt"),
        ("[1]", "its receipt names no release and digest"),
        (
            "{\"release\": \"dekopon@0.90.0\", \"digest\": null}",
            "its receipt names no release and digest",
        ),
        (
            &format!("{{\"digest\": \"{}\"}}", digest()),
            "its receipt names no release and digest",
        ),
        (
            &format!(
                "{{\"release\": \"dekopon@0.90.0\", \"digest\": \"{}\", \"merge_sha\": 123}}",
                digest()
            ),
            "its receipt's merge_sha is 123, not a string",
        ),
    ];
    for (receipt, why) in cases {
        let fx = Fx::new();
        fx.fake_deploy_writing(0, DEPLOYED, receipt);
        assert_eq!(
            fx.deploy(RELEASE),
            failed(&format!(
                "deploy.sh {RELEASE} merged but {why} ({DEPLOYED}); record the deploy by hand"
            )),
            "{receipt}"
        );
        assert_eq!(fx.calls("cm "), Vec::<String>::new());
    }
}

#[test]
fn a_refused_evidence_note_fails_unless_it_already_exists() {
    let fx = Fx::new();
    fx.fake_cm("evidence", "error: refused");
    assert_eq!(
        fx.deploy(RELEASE),
        failed(&format!(
            "deployed {RELEASE}; cm evidence refused: error: refused"
        ))
    );
    assert_eq!(fx.calls("cm advance"), Vec::<String>::new());

    let fx = Fx::new();
    fx.fake_cm(
        "evidence",
        "error: evidence deploy-mmmmmmmmmmmm already EXISTS",
    );
    assert_eq!(fx.deploy(RELEASE), done());
    assert_eq!(fx.calls("cm advance").len(), 1);
}

#[test]
fn a_receipt_directory_that_cannot_be_made_deploys_nothing() {
    let fx = Fx::new();
    let file = fx.root.path().join("a-file");
    write(&file, "");
    let env = [("TMPDIR", file.to_str().unwrap())].into_iter().collect();
    let outcome = fx.run_with(
        &ReleasedUndeployed,
        deploy_line(RELEASE),
        &env,
        &Scripts(&fx),
    );
    assert!(
        failed_with(
            &outcome,
            "cannot make a directory for deploy.sh's receipt: "
        ),
        "{outcome:?}"
    );
    assert_eq!(fx.calls(""), Vec::<String>::new());
}

#[test]
fn an_evidence_note_past_the_file_size_limit_fails_its_line_and_the_tick_goes_on() {
    let fx = Fx::new();
    let said = "y".repeat(300);
    let merge_sha = "m".repeat(40);
    let mut receipt =
        json!({"release": "", "pr": HOMELAB_PR, "merge_sha": merge_sha, "digest": digest()});
    let release = "x".repeat(4000 - receipt.to_string().len());
    receipt["release"] = json!(release);
    let receipt = receipt.to_string();
    let note = format!(
        "# Deploy of {release}\n\n- deploy PR: {HOMELAB_PR}\n- merge SHA: {merge_sha}\n- digest: {}\n- deploy.sh: {said}\n",
        digest()
    );
    assert!(
        receipt.len() < 4096 && note.len() > 4096,
        "deploy.sh can write its receipt under `ulimit -f 4`, the tick cannot write its note"
    );
    fx.fake_deploy_writing(0, &said, &receipt);
    write(
        &fx.tools().join("handoff-pick.sh"),
        "echo ok store/epochs/ep/handoff/coordinator-x.md\n",
    );
    write(
        &fx.file("next.json"),
        &json!({"ok": true, "violations": [], "actionable": [deploy_line(RELEASE)]}).to_string(),
    );
    let output = Command::new("bash")
        .args(["-c", "ulimit -f 4 && exec \"$@\"", "bash"])
        .arg(env!("CARGO_BIN_EXE_cm"))
        .args(["tick", "--cm", "cm", "--checkout"])
        .arg(fx.ct())
        .env_clear()
        .envs(fx.child_env())
        .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let failed = format!(
        "{} [tick/released-undeployed failed: deployed {RELEASE}; cannot write its evidence note: its {} bytes would pass the file size limit; record the deploy by hand]",
        text(&deploy_line(RELEASE)),
        note.len()
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.lines().any(|line| line == failed), "{stdout}");
    assert_eq!(fx.calls("deploy ").len(), 1);
    assert_eq!(fx.calls("cm "), Vec::<String>::new());
}
