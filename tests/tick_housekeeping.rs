use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cm::tick::act::{Ctx, Env, Report};
use cm::tick::gitsync::{self, Mode};
use cm::tick::proc::{FakeProc, Proc, SystemProc};
use cm::tick::{claude_md, directive, pin};

const SLUG: &str = "eleemosynary-mnemosyne";
const EPOCH: &str = "store/epochs/eleemosynary-mnemosyne";
const STAMP: &str = "2026-11-19T17:38Z";
const PINNED: &str = "0.17.1";
const ASSET: &str = "cm-0.17.1-x86_64-unknown-linux-musl.tar.gz";
const SYNC_FAILED: &str = "heartbeat: git pull --rebase and push to main failed 3 times; main does not have this checkout's commits";
const COMMIT_FAILED: &str =
    "heartbeat: git commit of the tree's changes failed; commit them by hand";
const OPEN: &str = "## 2026-11-25T04:05Z — Owner directive: unstick throughput now\n";
const LISTED: &str = "owner-directive: 2026-11-25T04:05Z — Owner directive: unstick throughput now";

fn journal() -> String {
    format!(
        "# journal\n\n## 2026-11-18T10:00Z — Owner directive: before the stamp, never listed\n\n## {STAMP} — owner-queue answer 10. URGENT: gate review\n\n## {STAMP} — epoch working\n\n## 2026-11-20T01:00Z — Owner directive: picked up already\n\n## 2026-11-20T01:05Z — Directive picked up: 2026-11-20T01:00Z — Owner directive: picked up already\n"
    )
}

fn handoff(stamp: &str) -> String {
    format!(
        "# Coordinator handoff\n\nHandoff: epoch={SLUG} role=coordinator lineage=session_x written=2026-11-24T20:30:00+00:00 prompt=p.md stamp_sha={} stamp_entry={stamp}\n",
        "7".repeat(40)
    )
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn script(path: &Path, body: &str) {
    write(path, &format!("#!/bin/sh\n{body}"));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// Every write stays under the scratch root, children included: HOME, GIT_CONFIG_GLOBAL and
// TMPDIR point into it, whatever the runner's own are, and fixture scripts find it as
// $TICK_FIXTURE. Its name holds an apostrophe and a space, so every run checks that no path is
// quoted into a script or a command line.
struct Scratch {
    root: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let scratch = Scratch {
            root: tempfile::Builder::new()
                .prefix("tick's housekeeping ")
                .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
                .unwrap(),
        };
        let root = scratch.path();
        for dir in ["home", "tmp", "bin"] {
            fs::create_dir(root.join(dir)).unwrap();
        }
        write(
            &root.join("gitconfig"),
            "[user]\n\tname = Tick Test\n\temail = tick@example.invalid\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n[advice]\n\tdetachedHead = false\n",
        );
        scratch.git(root, &["init", "-q", "--bare", "origin.git"]);
        scratch.git(root, &["clone", "-q", "origin.git", "ct"]);
        let ct = scratch.ct();
        write(
            &ct.join(EPOCH).join("EPOCH.md"),
            "---\nstatus: WORKING\n---\n",
        );
        write(&ct.join(EPOCH).join("CARD.md"), "card\n");
        write(&ct.join(EPOCH).join("JOURNAL.md"), &journal());
        scratch.git(&ct, &["add", "-A"]);
        scratch.git(&ct, &["commit", "-qm", "start"]);
        scratch.git(&ct, &["push", "-q", "origin", "main"]);
        scratch.git(root, &["clone", "-q", "origin.git", "other"]);
        scratch
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn ct(&self) -> PathBuf {
        self.path().join("ct")
    }

    fn other(&self) -> PathBuf {
        self.path().join("other")
    }

    fn child_env(&self) -> Vec<(OsString, OsString)> {
        let root = self.path();
        [
            ("HOME", root.join("home").into_os_string()),
            ("GIT_CONFIG_GLOBAL", root.join("gitconfig").into_os_string()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("TMPDIR", root.join("tmp").into_os_string()),
            ("TICK_FIXTURE", root.as_os_str().to_owned()),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .envs(self.child_env())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn head(&self, dir: &Path) -> String {
        self.git(dir, &["rev-parse", "HEAD"]).trim_end().to_owned()
    }

    fn origin_log(&self) -> Vec<String> {
        self.git(
            &self.path().join("origin.git"),
            &["log", "--format=%s", "main"],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }

    fn push_from_other(&self, file: &str, text: &str) {
        let other = self.other();
        self.git(&other, &["pull", "-q", "--rebase", "origin", "main"]);
        write(&other.join(EPOCH).join(file), text);
        self.git(&other, &["add", "-A"]);
        self.git(&other, &["commit", "-qm", &format!("other: {file}")]);
        self.git(&other, &["push", "-q", "origin", "main"]);
    }

    // The real runner with this scratch root's environment; pauses are recorded, not slept.
    fn proc(&self) -> FakeProc {
        let env = self.child_env();
        FakeProc::new(move |cmd| {
            let cmd = env
                .iter()
                .fold(cmd.clone(), |cmd, (key, value)| cmd.env(key, value));
            SystemProc.run(&cmd)
        })
    }

    fn env(&self) -> Env {
        let root = self.path();
        [("HOME", root.join("home")), ("TMPDIR", root.join("tmp"))]
            .into_iter()
            .map(|(key, value)| (key, value.to_string_lossy().into_owned()))
            .collect()
    }

    fn phase<T>(
        &self,
        proc: &dyn Proc,
        cm: &Path,
        budget: Duration,
        body: impl FnOnce(&Ctx) -> T,
    ) -> T {
        let ct = self.ct();
        let env = self.env();
        let ctx = Ctx::new(&ct, Path::new(EPOCH), proc, &env, cm.as_os_str(), budget);
        body(&ctx)
    }

    fn sync(&self, proc: &FakeProc, mode: Mode) -> Report {
        self.phase(proc, Path::new("cm"), gitsync::BUDGET, |ctx| {
            gitsync::sync(ctx, mode)
        })
    }

    fn status(&self) -> String {
        self.git(&self.ct(), &["status", "--porcelain"])
    }
}

#[test]
fn a_dirty_tree_is_committed_under_the_slug_tail_and_pushed_to_origin() {
    let scratch = Scratch::new();
    let ct = scratch.ct();
    write(&ct.join(EPOCH).join("CARD.md"), "card, synced\n");
    write(&ct.join(EPOCH).join("new.md"), "new\n");
    let proc = scratch.proc();
    assert_eq!(scratch.sync(&proc, Mode::Always), Report::default());
    assert_eq!(
        scratch.origin_log(),
        ["chore(mnemosyne): heartbeat sync", "start"]
    );
    assert_eq!(
        scratch.head(&ct),
        scratch.head(&scratch.path().join("origin.git"))
    );
    assert_eq!(scratch.status(), "");
    assert!(proc.pauses().is_empty());
}

#[test]
fn a_commit_another_writer_pushed_first_is_pulled_under_ours() {
    let scratch = Scratch::new();
    scratch.push_from_other("STATE.md", "other\n");
    let ct = scratch.ct();
    write(&ct.join(EPOCH).join("CARD.md"), "card, synced\n");
    let proc = scratch.proc();
    assert_eq!(scratch.sync(&proc, Mode::Always), Report::default());
    assert_eq!(
        scratch.origin_log(),
        [
            "chore(mnemosyne): heartbeat sync",
            "other: STATE.md",
            "start"
        ]
    );
    assert_eq!(scratch.status(), "");
}

#[test]
fn a_clean_tree_is_pulled_and_pushed_but_not_after_executors() {
    let scratch = Scratch::new();
    let ct = scratch.ct();
    let before = scratch.head(&ct);
    scratch.push_from_other("STATE.md", "other\n");
    let proc = scratch.proc();
    assert_eq!(scratch.sync(&proc, Mode::IfDirty), Report::default());
    assert_eq!(
        scratch.head(&ct),
        before,
        "a clean tree after executors is not pulled"
    );
    assert_eq!(
        proc.log(),
        ["git rev-parse --abbrev-ref HEAD", "git status --porcelain"]
    );
    assert_eq!(scratch.sync(&proc, Mode::Always), Report::default());
    assert_eq!(
        scratch.head(&ct),
        scratch.head(&scratch.path().join("origin.git"))
    );
}

#[test]
fn what_executors_wrote_is_committed_and_pushed() {
    let scratch = Scratch::new();
    write(
        &scratch.ct().join(EPOCH).join("CARD.md"),
        "card, after an executor\n",
    );
    let proc = scratch.proc();
    assert_eq!(scratch.sync(&proc, Mode::IfDirty), Report::default());
    assert_eq!(
        scratch.origin_log(),
        ["chore(mnemosyne): heartbeat sync", "start"]
    );
}

#[test]
fn a_checkout_off_main_is_neither_committed_nor_pulled_nor_pushed() {
    let scratch = Scratch::new();
    let ct = scratch.ct();
    scratch.git(&ct, &["switch", "-q", "-c", "tick-mvp/x"]);
    write(&ct.join(EPOCH).join("CARD.md"), "card, on a branch\n");
    let proc = scratch.proc();
    let report = scratch.sync(&proc, Mode::Always);
    assert_eq!(
        report.lines,
        ["heartbeat: the checkout is on tick-mvp/x, not main; nothing committed, pulled or pushed"]
    );
    assert!(!report.failed);
    assert_eq!(scratch.status(), format!(" M {EPOCH}/CARD.md\n"));
    assert_eq!(scratch.origin_log(), ["start"]);

    let head = scratch.head(&ct);
    scratch.git(&ct, &["switch", "-q", "--detach", &head]);
    assert_eq!(
        scratch.sync(&proc, Mode::Always).lines,
        ["heartbeat: the checkout is on HEAD, not main; nothing committed, pulled or pushed"]
    );
}

#[test]
fn a_commit_that_fails_is_one_line_and_the_pull_and_push_still_run() {
    let scratch = Scratch::new();
    let ct = scratch.ct();
    script(&ct.join(".git/hooks/pre-commit"), "exit 1\n");
    write(&ct.join(EPOCH).join("CARD.md"), "card, refused\n");
    scratch.push_from_other("STATE.md", "other\n");
    let proc = scratch.proc();
    let report = scratch.sync(&proc, Mode::Always);
    assert_eq!(report.lines, [COMMIT_FAILED]);
    assert!(report.failed);
    assert_eq!(
        scratch.head(&ct),
        scratch.head(&scratch.path().join("origin.git"))
    );
    assert_eq!(
        scratch.status(),
        format!(" M {EPOCH}/CARD.md\n"),
        "the autostash put it back"
    );
    assert!(proc.log().contains(&"git push -q origin main".to_owned()));
}

#[test]
fn a_push_origin_refuses_is_tried_three_times_two_seconds_apart_then_reported() {
    let scratch = Scratch::new();
    script(
        &scratch.path().join("origin.git/hooks/pre-receive"),
        "exit 1\n",
    );
    let ct = scratch.ct();
    write(&ct.join(EPOCH).join("CARD.md"), "card, unpushed\n");
    let proc = scratch.proc();
    let report = scratch.sync(&proc, Mode::Always);
    assert_eq!(report.lines, [SYNC_FAILED]);
    assert!(report.failed);
    assert_eq!(
        proc.log()
            .iter()
            .filter(|call| *call == "git push -q origin main")
            .count(),
        3
    );
    assert_eq!(proc.pauses(), [Duration::from_secs(2); 2]);
    assert_eq!(scratch.origin_log(), ["start"]);
    assert_eq!(
        scratch.git(&ct, &["log", "-1", "--format=%s"]),
        "chore(mnemosyne): heartbeat sync\n",
        "the local commit is kept"
    );
}

#[test]
fn a_rebase_that_conflicts_is_aborted_each_try_and_leaves_the_checkout_as_it_was() {
    let scratch = Scratch::new();
    scratch.push_from_other("CARD.md", "card, theirs\n");
    let ct = scratch.ct();
    write(&ct.join(EPOCH).join("CARD.md"), "card, ours\n");
    let proc = scratch.proc();
    let report = scratch.sync(&proc, Mode::Always);
    assert_eq!(report.lines, [SYNC_FAILED]);
    assert_eq!(
        proc.log()
            .iter()
            .filter(|call| *call == "git rebase --abort")
            .count(),
        3
    );
    assert!(!ct.join(".git/rebase-merge").exists());
    assert!(!ct.join(".git/rebase-apply").exists());
    assert_eq!(scratch.status(), "");
    assert_eq!(
        fs::read_to_string(ct.join(EPOCH).join("CARD.md")).unwrap(),
        "card, ours\n"
    );
    assert_eq!(scratch.origin_log(), ["other: CARD.md", "start"]);
}

#[test]
fn a_pull_the_budget_cuts_mid_rebase_is_aborted_and_the_tree_is_left_as_it_was() {
    let scratch = Scratch::new();
    let ct = scratch.ct();
    write(&ct.join(EPOCH).join("NOTES.md"), "local, unpushed\n");
    scratch.git(&ct, &["add", "-A"]);
    scratch.git(&ct, &["commit", "-qm", "local"]);
    let local = scratch.head(&ct);
    scratch.push_from_other("STATE.md", "other\n");
    script(&ct.join(".git/hooks/pre-commit"), "exit 1\n");
    script(
        &ct.join(".git/hooks/post-checkout"),
        "[ -e \"$TICK_FIXTURE/rebasing\" ] && exit 0\ntouch \"$TICK_FIXTURE/rebasing\"\nexec sleep 3600\n",
    );
    write(&ct.join(EPOCH).join("CARD.md"), "card, in the autostash\n");
    let proc = scratch.proc();
    let report = scratch.phase(&proc, Path::new("cm"), Duration::from_secs(5), |ctx| {
        gitsync::sync(ctx, Mode::Always)
    });
    assert!(
        scratch.path().join("rebasing").exists(),
        "the pull reached the rebase before the budget cut it"
    );
    assert_eq!(report.lines, [COMMIT_FAILED, SYNC_FAILED]);
    let calls = proc.log();
    let count = |call: &str| calls.iter().filter(|line| *line == call).count();
    assert_eq!(count("git pull -q --rebase --autostash origin main"), 1);
    assert_eq!(count("git rebase --abort"), 1);
    assert!(
        proc.pauses().is_empty(),
        "no try starts once the budget is spent"
    );
    assert!(!ct.join(".git/rebase-merge").exists());
    assert_eq!(
        scratch.git(&ct, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main\n"
    );
    assert_eq!(scratch.head(&ct), local);
    assert_eq!(scratch.status(), format!(" M {EPOCH}/CARD.md\n"));
    assert_eq!(
        fs::read_to_string(ct.join(EPOCH).join("CARD.md")).unwrap(),
        "card, in the autostash\n"
    );
}

const RELEASED: &str = "cp \"$TICK_FIXTURE/release.tar.gz\" \"$3\"";

struct Pinned {
    scratch: Scratch,
    cm: PathBuf,
}

impl Pinned {
    // The pinned cm answers `--json next` with a line of its own, so a tick shows which cm it read.
    fn new(fetch: &str) -> Self {
        let scratch = Scratch::new();
        let root = scratch.path();
        let cm = root.join("bin/cm");
        script(&cm, &fake_cm("0.0.1", "{\"actionable\": []}"));
        let folder = ASSET.trim_end_matches(".tar.gz");
        script(
            &root.join("release").join(folder).join("cm"),
            &fake_cm(
                PINNED,
                "{\"actionable\": [{\"kind\": \"attempt-ended\", \"target\": \"01-a/u/c\", \"command\": \"from the pinned cm\"}]}",
            ),
        );
        fs::create_dir_all(root.join("nocm").join(folder)).unwrap();
        for (tarball, from) in [("release.tar.gz", "release"), ("nocm.tar.gz", "nocm")] {
            let tar = Command::new("tar")
                .arg("-czf")
                .arg(root.join(tarball))
                .arg("-C")
                .arg(root.join(from))
                .arg(folder)
                .status()
                .unwrap();
            assert!(tar.success());
        }
        let ct = scratch.ct();
        write(
            &ct.join(pin::SETUP),
            &format!(
                "case \"$1\" in\n  --list-pins) printf 'cloudflared|2026.10.1|cloudflared-linux-amd64|u|s\\ncm|{PINNED}|{ASSET}|u|s\\n' ;;\n  --fetch-pinned) [ \"$2\" = cm ] || exit 2\n    {fetch} ;;\nesac\n"
            ),
        );
        scratch.git(&ct, &["add", "-A"]);
        scratch.git(&ct, &["commit", "-qm", "setup"]);
        scratch.git(&ct, &["push", "-q", "origin", "main"]);
        Pinned { scratch, cm }
    }

    fn released() -> Self {
        Pinned::new(RELEASED)
    }

    fn run(&self) -> Report {
        let proc = self.scratch.proc();
        self.scratch.phase(&proc, &self.cm, pin::BUDGET, pin::run)
    }

    fn version(&self) -> String {
        let output = Command::new(&self.cm).arg("--version").output().unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    fn left_alone(&self, report: Report) {
        assert_eq!(
            report.lines,
            [format!(
                "heartbeat: cm {PINNED} (setup.sh's pin) could not be fetched or installed; still running cm 0.0.1"
            )]
        );
        assert!(report.failed);
        assert_eq!(self.version(), "cm 0.0.1\n");
        assert_eq!(names(&self.scratch.path().join("bin")), ["cm"]);
        assert_eq!(
            names(&self.scratch.path().join("tmp")),
            Vec::<String>::new()
        );
    }
}

fn fake_cm(version: &str, next: &str) -> String {
    format!(
        "case \"$1\" in\n  --version) echo 'cm {version}' ;;\n  --json) echo '{next}' ;;\nesac\n"
    )
}

#[test]
fn a_stale_pin_is_fetched_unpacked_and_renamed_over_the_running_cm() {
    let pinned = Pinned::released();
    let running = pinned.scratch.path().join("running");
    fs::hard_link(&pinned.cm, &running).unwrap();
    assert_eq!(pinned.run(), Report::default());
    assert_eq!(pinned.version(), format!("cm {PINNED}\n"));
    assert_eq!(
        fs::metadata(&pinned.cm).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(
        fs::read_to_string(&running).unwrap().contains("cm 0.0.1"),
        "the running cm's file is replaced by a rename, never written over"
    );
    assert_eq!(names(&pinned.scratch.path().join("bin")), ["cm"]);
    assert_eq!(
        names(&pinned.scratch.path().join("tmp")),
        Vec::<String>::new()
    );
    let proc = pinned.scratch.proc();
    assert_eq!(
        pinned
            .scratch
            .phase(&proc, &pinned.cm, pin::BUDGET, pin::run),
        Report::default()
    );
    assert_eq!(
        proc.log(),
        [
            "bash ".to_owned()
                + &pinned.scratch.ct().join(pin::SETUP).display().to_string()
                + " --list-pins",
            "cm --version".to_owned()
        ],
        "a cm at its pin fetches nothing"
    );
}

#[test]
fn a_failed_fetch_keeps_the_running_cm_and_says_so_in_one_line() {
    let pinned = Pinned::new("exit 1");
    pinned.left_alone(pinned.run());
}

#[test]
fn an_asset_that_does_not_unpack_or_holds_no_cm_keeps_the_running_cm() {
    let garbage = Pinned::new("echo 'not a tarball' > \"$3\"");
    garbage.left_alone(garbage.run());
    let empty = Pinned::new("cp \"$TICK_FIXTURE/nocm.tar.gz\" \"$3\"");
    empty.left_alone(empty.run());
}

#[test]
fn a_missing_cm_is_still_running_cm_none_and_is_installed_at_its_path() {
    let pinned = Pinned::released();
    fs::remove_file(&pinned.cm).unwrap();
    assert_eq!(pinned.run(), Report::default());
    assert_eq!(pinned.version(), format!("cm {PINNED}\n"));

    let failing = Pinned::new("exit 1");
    fs::remove_file(&failing.cm).unwrap();
    assert_eq!(
        failing.run().lines,
        [format!(
            "heartbeat: cm {PINNED} (setup.sh's pin) could not be fetched or installed; still running cm none"
        )]
    );
    assert!(!failing.cm.exists());
}

#[test]
fn the_pinned_cm_is_fetched_into_a_fresh_folder_under_tmpdir_that_is_then_removed() {
    let pinned = Pinned::new(&format!(
        "printf '%s' \"$3\" > \"$TICK_FIXTURE/dest\"; {RELEASED}"
    ));
    assert_eq!(pinned.run(), Report::default());
    let root = pinned.scratch.path();
    let dest = PathBuf::from(fs::read_to_string(root.join("dest")).unwrap());
    assert_eq!(dest.file_name(), Some(OsStr::new(ASSET)));
    let folder = dest.parent().unwrap();
    assert_eq!(folder.parent(), Some(root.join("tmp").as_path()));
    assert!(
        folder
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("cm-pin-")
    );
    assert!(!folder.exists());
}

#[test]
fn two_cm_pins_install_nothing_and_print_the_line() {
    let pinned = Pinned::released();
    let setup = pinned.scratch.ct().join(pin::SETUP);
    let text = fs::read_to_string(&setup).unwrap().replace(
        &format!("cm|{PINNED}|{ASSET}|u|s\\n"),
        &format!("cm|{PINNED}|{ASSET}|u|s\\ncm|0.17.2|cm-0.17.2.tar.gz|u|s\\n"),
    );
    fs::write(&setup, text).unwrap();
    pinned.left_alone(pinned.run());
}

struct Directives {
    scratch: Scratch,
}

impl Directives {
    fn new() -> Self {
        let directives = Directives {
            scratch: Scratch::new(),
        };
        directives.handoff("coordinator-2026-11-24T2030Z.md", &handoff(STAMP));
        directives
    }

    fn epoch(&self) -> PathBuf {
        self.scratch.ct().join(EPOCH)
    }

    fn handoff(&self, name: &str, text: &str) {
        write(&self.epoch().join("handoff").join(name), text);
    }

    fn append(&self, text: &str) {
        let journal = self.epoch().join("JOURNAL.md");
        let old = fs::read_to_string(&journal).unwrap();
        fs::write(&journal, old + text).unwrap();
    }

    fn run(&self) -> Report {
        let proc = FakeProc::new(|cmd| panic!("no child runs: {}", cmd.line()));
        let report = self
            .scratch
            .phase(&proc, Path::new("cm"), directive::BUDGET, directive::run);
        assert!(proc.calls().is_empty());
        report
    }
}

#[test]
fn unpicked_owner_directives_below_the_stamp_entry_are_listed_one_line_each() {
    let directives = Directives::new();
    assert_eq!(directives.run(), Report::default());
    directives.append(&format!(
        "\n{OPEN}\n## 2026-11-25T05:00Z — Owner directive: second\n"
    ));
    assert_eq!(
        directives.run().lines,
        [
            LISTED,
            "owner-directive: 2026-11-25T05:00Z — Owner directive: second"
        ]
    );
    directives.append("\n## 2026-11-25T05:01Z — Directive picked up: 2026-11-25T05:00Z — Owner directive: second\n");
    assert_eq!(directives.run().lines, [LISTED]);
}

#[test]
fn handoffs_naming_none_or_no_stamp_list_nothing() {
    let directives = Directives::new();
    directives.append(&format!("\n{OPEN}"));
    fs::remove_dir_all(directives.epoch().join("handoff")).unwrap();
    assert_eq!(directives.run(), Report::default());
    directives.handoff("coordinator-a.md", &handoff("none"));
    directives.handoff(
        "coordinator-b.md",
        "# Coordinator handoff\n\nno Handoff line here\n",
    );
    directives.handoff("helper-x.md", &handoff("2026-11-01T00:00Z"));
    assert_eq!(directives.run(), Report::default());
    directives.handoff("coordinator-c.md", &handoff(STAMP));
    assert_eq!(directives.run().lines, [LISTED]);
}

#[test]
fn handoffs_naming_two_stamp_entries_list_no_directive() {
    let directives = Directives::new();
    directives.append(&format!("\n{OPEN}"));
    directives.handoff(
        "coordinator-2026-11-25T0100Z.md",
        &handoff("2026-11-25T00:59Z"),
    );
    directives.handoff("coordinator-2026-11-25T0200Z.md", &handoff(STAMP));
    let report = directives.run();
    assert_eq!(
        report.lines,
        [format!(
            "heartbeat: coordinator handoffs name more than one stamp_entry ({STAMP} 2026-11-25T00:59Z); owner directives not listed"
        )]
    );
    assert!(!report.failed);
}

#[test]
fn a_stamp_entry_with_no_journal_heading_lists_no_directive() {
    let directives = Directives::new();
    directives.handoff(
        "coordinator-2026-11-24T2030Z.md",
        &handoff("2026-11-19T17:39Z"),
    );
    let line = "heartbeat: no JOURNAL.md heading for stamp_entry 2026-11-19T17:39Z; owner directives not listed";
    assert_eq!(directives.run().lines, [line]);
    fs::remove_file(directives.epoch().join("JOURNAL.md")).unwrap();
    assert_eq!(
        directives.run().lines,
        [line],
        "a missing journal has no heading"
    );
}

#[test]
fn a_handoff_folder_or_journal_that_cannot_be_read_is_named() {
    let directives = Directives::new();
    let journal = directives.epoch().join("JOURNAL.md");
    fs::remove_file(&journal).unwrap();
    std::os::unix::fs::symlink(&journal, &journal).unwrap();
    let report = directives.run();
    assert_eq!(report.lines.len(), 1);
    assert!(
        report.lines[0].starts_with(&format!(
            "heartbeat: could not read {}: ",
            journal.display()
        )),
        "{report:?}"
    );
    assert!(report.lines[0].ends_with("; owner directives not listed"));
    assert!(report.failed);

    let handoff = directives.epoch().join("handoff");
    fs::remove_dir_all(&handoff).unwrap();
    std::os::unix::fs::symlink(&handoff, &handoff).unwrap();
    let report = directives.run();
    assert!(
        report.lines[0].starts_with(&format!(
            "heartbeat: could not read {}: ",
            handoff.display()
        )),
        "{report:?}"
    );
    assert!(report.failed);
}

impl Scratch {
    fn tick(&self, shell: Option<&str>, args: &[&OsStr]) -> std::process::Output {
        let me = env!("CARGO_BIN_EXE_cm");
        let mut command = match shell {
            Some(limit) => {
                let mut command = Command::new("bash");
                command
                    .arg("-c")
                    .arg(format!("{limit}; exec \"$0\" \"$@\""))
                    .arg(me);
                command
            }
            None => Command::new(me),
        };
        command
            .env_clear()
            .envs(self.child_env())
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path().join("bin").display()),
            )
            .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_01abc")
            .arg("tick")
            .args(args)
            .current_dir(self.path())
            .output()
            .unwrap()
    }

    fn fake_git(&self) {
        script(
            &self.path().join("bin/git"),
            "case $1 in rev-parse) echo main ;; esac\nexit 0\n",
        );
    }

    fn boot_time_claude_md(&self) -> PathBuf {
        let path = self.path().join("home/.claude/CLAUDE.md");
        write(&path, &format!("{}\nold rules\n", claude_md::BOOT_TIME));
        path
    }
}

#[test]
fn the_built_cm_tick_syncs_refreshes_its_pin_lists_directives_and_imports_the_rules() {
    let pinned = Pinned::released();
    let scratch = &pinned.scratch;
    let ct = scratch.ct();
    script(
        &ct.join("tools/cloud-env/handoff-pick.sh"),
        &format!("echo 'ok {EPOCH}/handoff/coordinator-2026-11-24T2030Z.md'\n"),
    );
    write(
        &ct.join(EPOCH)
            .join("handoff/coordinator-2026-11-24T2030Z.md"),
        &handoff(STAMP),
    );
    scratch.git(&ct, &["add", "-A"]);
    scratch.git(&ct, &["commit", "-qm", "bound"]);
    scratch.git(&ct, &["push", "-q", "origin", "main"]);
    let journal = ct.join(EPOCH).join("JOURNAL.md");
    fs::write(
        &journal,
        fs::read_to_string(&journal).unwrap() + "\n" + OPEN,
    )
    .unwrap();
    let claude = scratch.boot_time_claude_md();

    let output = scratch.tick(
        None,
        &[
            OsStr::new("--cm"),
            pinned.cm.as_os_str(),
            OsStr::new("--checkout"),
            ct.as_os_str(),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{LISTED}\nattempt-ended 01-a/u/c: from the pinned cm\nrules: now imported from tools/cloud-env/COORDINATOR.md; read it\n"
        )
    );
    assert_eq!(scratch.origin_log()[0], "chore(mnemosyne): heartbeat sync");
    assert_eq!(scratch.status(), "");
    assert_eq!(pinned.version(), format!("cm {PINNED}\n"));
    assert_eq!(
        fs::read_to_string(&claude).unwrap(),
        format!("{}\n", claude_md::IMPORT)
    );
    assert_eq!(names(&scratch.path().join("tmp")), Vec::<String>::new());
}

#[test]
fn a_claude_md_import_that_cannot_be_written_is_one_line_and_keeps_the_old_rules() {
    let scratch = Scratch::new();
    scratch.fake_git();
    script(
        &scratch.path().join("bin/cm"),
        "echo '{\"actionable\": []}'\n",
    );
    let claude = scratch.boot_time_claude_md();
    let old = fs::read(&claude).unwrap();
    let folder = scratch.path().join("home/.claude");
    // A folder that refuses a staged file sends an unprivileged tick to the in-place write.
    for mode in [0o755, 0o555] {
        fs::set_permissions(&folder, fs::Permissions::from_mode(mode)).unwrap();
        let output = scratch.tick(
            Some("ulimit -f 0"),
            &[
                OsStr::new("--cm"),
                OsStr::new("cm"),
                OsStr::new("--checkout"),
                scratch.ct().as_os_str(),
            ],
        );
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(output.status.code(), Some(0), "{mode:o}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 2, "{mode:o}: {stdout}");
        assert_eq!(
            lines[0],
            format!(
                "heartbeat: could not write the COORDINATOR.md import to {}",
                claude.display()
            )
        );
        assert!(
            lines[1].starts_with("tick: claude-md phase took ")
                && lines[1].ends_with(" s (budget 5 s)"),
            "{mode:o}: {stdout}"
        );
        assert_eq!(fs::read(&claude).unwrap(), old, "{mode:o}");
        assert_eq!(names(&folder), ["CLAUDE.md"], "{mode:o}");
    }
}

#[test]
fn a_pin_past_the_file_size_limit_keeps_the_running_cm_and_the_tick_alive() {
    let fetched = Pinned::released();
    let linked = Pinned::new("cp \"$TICK_FIXTURE/linked.tar.gz\" \"$3\"");
    let root = linked.scratch.path();
    let folder = ASSET.trim_end_matches(".tar.gz");
    fs::write(root.join("big"), vec![b'#'; 1 << 20]).unwrap();
    fs::create_dir_all(root.join("linked").join(folder)).unwrap();
    std::os::unix::fs::symlink(
        root.join("big"),
        root.join("linked").join(folder).join("cm"),
    )
    .unwrap();
    let tar = Command::new("tar")
        .arg("-czf")
        .arg(root.join("linked.tar.gz"))
        .arg("-C")
        .arg(root.join("linked"))
        .arg(folder)
        .status()
        .unwrap();
    assert!(tar.success());
    // At 0 the fetch child dies writing the tarball; at 64 KiB the tarball and its link fit and
    // only the copy into the staged cm would not.
    for (pinned, limit) in [(&fetched, "ulimit -f 0"), (&linked, "ulimit -f 64")] {
        pinned.scratch.fake_git();
        let output = pinned.scratch.tick(
            Some(limit),
            &[
                OsStr::new("--cm"),
                pinned.cm.as_os_str(),
                OsStr::new("--checkout"),
                pinned.scratch.ct().as_os_str(),
            ],
        );
        assert_eq!(output.status.code(), Some(0), "{limit}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            lines[0],
            format!(
                "heartbeat: cm {PINNED} (setup.sh's pin) could not be fetched or installed; still running cm 0.0.1"
            ),
            "{limit}"
        );
        assert!(
            lines[1].starts_with("tick: pin phase took "),
            "{limit}: {stdout}"
        );
        assert_eq!(lines.len(), 2, "{limit}: {stdout}");
        assert_eq!(pinned.version(), "cm 0.0.1\n");
        assert_eq!(names(&pinned.scratch.path().join("bin")), ["cm", "git"]);
        assert_eq!(
            names(&pinned.scratch.path().join("tmp")),
            Vec::<String>::new()
        );
    }
}
