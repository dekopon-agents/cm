use std::path::Path;

use cm::attention::{self, Action, Check, Evidence, Github, PullRequest};
use cm::host::Host;
use cm::{commands, lint};

struct Clock;
impl Host for Clock {
    fn now(&self) -> jiff::Timestamp {
        "2026-10-10T14:00:00Z".parse().unwrap()
    }
    fn free_bytes(&self, _: &Path) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }
    fn heavy_builds(&self) -> std::io::Result<u32> {
        Ok(0)
    }
}

struct Fixture(Evidence);
impl Github for Fixture {
    fn observe(&self, _: &str) -> Evidence {
        self.0.clone()
    }
}

const URL: &str = "https://github.com/example/project/pull/41";

fn evidence() -> Evidence {
    Evidence {
        pr: Some(PullRequest {
            url: URL.into(),
            head_ref_oid: "a".repeat(40),
            base_ref_oid: "b".repeat(40),
            state: "OPEN".into(),
            is_draft: false,
            merge_state_status: "CLEAN".into(),
            updated_at: "2026-10-10T13:00:00Z".into(),
            merged_at: None,
            closed_at: None,
        }),
        required_checks: vec![Check {
            name: "test".into(),
            state: "SUCCESS".into(),
            link: format!("{URL}/checks"),
            started_at: "2026-10-10T13:00:00Z".into(),
            completed_at: "2026-10-10T13:02:00Z".into(),
        }],
        error: None,
    }
}

fn run(dir: &Path, e: Evidence, record: bool) -> cm::outcome::Outcome {
    attention::run(
        &Clock,
        &Fixture(e),
        dir,
        &[URL.into()],
        "worker",
        "coordinator",
        record,
    )
    .unwrap()
}

fn next(out: &cm::outcome::Outcome) -> Action {
    serde_json::from_value(out.fields["observations"][URL]["action"].clone()).unwrap()
}

#[test]
fn failures_route_to_worker_and_success_only_requests_review() {
    let dir = tempfile::tempdir().unwrap();
    commands::init(&Clock, dir.path()).unwrap();
    let mut e = evidence();
    e.required_checks[0].state = "FAILURE".into();
    let out = run(dir.path(), e, false);
    assert_eq!(next(&out), Action::FixCi);
    assert!(out.lines[0].contains("→ worker:"));
    let out = run(dir.path(), evidence(), false);
    assert_eq!(next(&out), Action::ReviewHead);
    assert!(out.lines[0].contains("not approval to merge"));
    assert!(!dir.path().join(".cm/attention.json").exists());
}

#[test]
fn changed_head_invalidates_readiness_and_identical_runs_do_not_append() {
    let dir = tempfile::tempdir().unwrap();
    commands::init(&Clock, dir.path()).unwrap();
    run(dir.path(), evidence(), true);
    let journal = dir.path().join("JOURNAL.md");
    let first = std::fs::read(&journal).unwrap();
    assert_eq!(run(dir.path(), evidence(), true).fields["changed"], 0);
    assert_eq!(std::fs::read(&journal).unwrap(), first);
    let mut e = evidence();
    e.pr.as_mut().unwrap().head_ref_oid = "c".repeat(40);
    assert_eq!(
        next(&run(dir.path(), e.clone(), true)),
        Action::ReviewChangedHead
    );
    let second = std::fs::read(&journal).unwrap();
    let out = run(dir.path(), e, true);
    assert_eq!(next(&out), Action::ReviewChangedHead);
    assert_eq!(out.fields["changed"], 0);
    assert_eq!(std::fs::read(&journal).unwrap(), second);
    assert!(lint::lint(dir.path()).unwrap().ok());
    assert!(!dir.path().join("01-work").exists());
    let row = &out.fields["observations"][URL];
    assert_eq!(row["observed_at"], "2026-10-10T14:00Z");
    assert_eq!(row["evidence"]["pr"]["updatedAt"], "2026-10-10T13:00:00Z");
}

#[test]
fn closed_and_merged_prs_retire_obsolete_work_without_advancing_units() {
    let dir = tempfile::tempdir().unwrap();
    commands::init(&Clock, dir.path()).unwrap();
    let before = std::fs::read(dir.path().join(".cm/state.json")).unwrap();
    for (state, expected) in [
        ("CLOSED", Action::ReconcileClosed),
        ("MERGED", Action::ReconcileMerged),
    ] {
        let mut e = evidence();
        e.pr.as_mut().unwrap().state = state.into();
        e.required_checks.clear();
        assert_eq!(next(&run(dir.path(), e, true)), expected);
    }
    assert_eq!(
        std::fs::read(dir.path().join(".cm/state.json")).unwrap(),
        before
    );
}

#[test]
fn absent_skipped_neutral_unknown_and_blocked_checks_never_pass() {
    let dir = tempfile::tempdir().unwrap();
    commands::init(&Clock, dir.path()).unwrap();
    for state in ["SKIPPED", "NEUTRAL", "NEW_UNKNOWN_STATE", ""] {
        let mut e = evidence();
        e.required_checks[0].state = state.into();
        let out = run(dir.path(), e, false);
        assert_eq!(next(&out), Action::VerifyChecks);
        assert!(!out.ok());
    }
    let mut e = evidence();
    e.required_checks.clear();
    assert_eq!(next(&run(dir.path(), e, false)), Action::VerifyChecks);
    let mut e = evidence();
    e.pr.as_mut().unwrap().merge_state_status = "BLOCKED".into();
    assert_eq!(next(&run(dir.path(), e, false)), Action::VerifyChecks);
}

#[test]
fn access_errors_cannot_reuse_green_evidence_and_preserve_last_known_head() {
    let dir = tempfile::tempdir().unwrap();
    commands::init(&Clock, dir.path()).unwrap();
    run(dir.path(), evidence(), true);
    let out = run(
        dir.path(),
        Evidence {
            pr: None,
            required_checks: vec![],
            error: Some("access denied".into()),
        },
        true,
    );
    assert_eq!(next(&out), Action::InvestigateEvidence);
    assert!(!out.ok());
    let mut e = evidence();
    e.pr.as_mut().unwrap().head_ref_oid = "d".repeat(40);
    assert_eq!(next(&run(dir.path(), e, true)), Action::ReviewChangedHead);
}

#[cfg(unix)]
fn cli_fixture(
    second_head: &str,
    check_output: &str,
    check_exit: i32,
) -> (tempfile::TempDir, std::process::Output) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let campaign = dir.path().join("campaign");
    commands::init(&Clock, &campaign).unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let mut pr = evidence().pr.unwrap();
    std::fs::write(
        dir.path().join("first.json"),
        serde_json::to_vec(&pr).unwrap(),
    )
    .unwrap();
    pr.head_ref_oid = second_head.into();
    std::fs::write(
        dir.path().join("second.json"),
        serde_json::to_vec(&pr).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("checks.json"), check_output).unwrap();
    let script = r#"#!/bin/sh
printf '%s\n' "$*" >> "$ATTENTION_FIXTURE/args"
if [ "$2" = view ]; then
  if [ -f "$ATTENTION_FIXTURE/seen" ]; then cat "$ATTENTION_FIXTURE/second.json"; else
    touch "$ATTENTION_FIXTURE/seen"
    cat "$ATTENTION_FIXTURE/first.json"
  fi
else
  cat "$ATTENTION_FIXTURE/checks.json"
  exit "$ATTENTION_CHECK_EXIT"
fi
"#;
    let gh = bin.join("gh");
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cm"))
        .args([
            "-C",
            campaign.to_str().unwrap(),
            "attention",
            "--pr",
            URL,
            "--worker",
            "worker",
            "--coordinator",
            "coordinator",
            "--json",
        ])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("ATTENTION_FIXTURE", dir.path())
        .env("ATTENTION_CHECK_EXIT", check_exit.to_string())
        .output()
        .unwrap();
    (dir, output)
}

#[test]
#[cfg(unix)]
fn cli_checks_are_required_and_a_racing_head_discards_their_results() {
    let checks = serde_json::to_string(&evidence().required_checks).unwrap();
    let (dir, output) = cli_fixture(&"c".repeat(40), &checks, 0);
    assert_eq!(output.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = &json["observations"][URL];
    assert_eq!(row["action"], "investigate-evidence");
    assert_eq!(row["evidence"]["required_checks"], serde_json::json!([]));
    assert!(
        row["evidence"]["error"]
            .as_str()
            .unwrap()
            .contains("changed during observation")
    );
    let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
    assert_eq!(args.lines().count(), 3);
    assert!(args.contains("--required --json name,state,link,startedAt,completedAt"));
}

#[test]
#[cfg(unix)]
fn cli_rejects_malformed_json_and_unsuccessful_gh_even_with_success_bytes() {
    for (payload, exit) in [
        ("not JSON".to_owned(), 0),
        (
            serde_json::to_string(&evidence().required_checks).unwrap(),
            1,
        ),
    ] {
        let (_, output) = cli_fixture(&"a".repeat(40), &payload, exit);
        assert_eq!(output.status.code(), Some(1));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["observations"][URL]["action"], "investigate-evidence");
    }
}
