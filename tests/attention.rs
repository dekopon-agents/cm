use std::path::Path;

#[allow(dead_code)]
mod support;

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
    source_fixture(
        second_head,
        &[check_output.to_owned()],
        &[r#"{"statuses":[]}"#.to_owned()],
        r#"{"protection":{"required_status_checks":{"contexts":["test"]}}}"#,
        "[]",
        check_exit,
    )
}

#[cfg(unix)]
fn source_fixture(
    second_head: &str,
    runs: &[String],
    statuses: &[String],
    branch: &str,
    rules: &str,
    check_exit: i32,
) -> (tempfile::TempDir, std::process::Output) {
    let dir = tempfile::tempdir().unwrap();
    let campaign = dir.path().join("campaign");
    commands::init(&Clock, &campaign).unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pull = |head: &str| {
        serde_json::json!({
            "html_url": URL, "state": "open", "merged": false, "draft": false,
            "mergeable_state": "clean", "updated_at": "2026-10-10T13:00:00Z",
            "merged_at": null, "closed_at": null,
            "head": { "sha": head, "ref": "topic" },
            "base": { "sha": "b".repeat(40), "ref": "main" },
        })
        .to_string()
    };
    let fixture = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
    fixture("first.json", &pull(&"a".repeat(40)));
    fixture("second.json", &pull(second_head));
    fixture("branch.json", branch);
    fixture("rules.json", rules);
    for (page, body) in runs.iter().enumerate() {
        fixture(&format!("runs{}.json", page + 1), body);
    }
    for (page, body) in statuses.iter().enumerate() {
        fixture(&format!("status{}.json", page + 1), body);
    }
    let script = r#"#!/bin/sh
printf '%s\n' "$*" >> "$ATTENTION_FIXTURE/args"
page=1; case "$*" in *page=2*) page=2 ;; *page=3*) page=3 ;; esac
case "$4" in
  */pulls/*)
    if [ -f "$ATTENTION_FIXTURE/seen" ]; then cat "$ATTENTION_FIXTURE/second.json"; else
      touch "$ATTENTION_FIXTURE/seen"
      cat "$ATTENTION_FIXTURE/first.json"
    fi ;;
  */branches/*) case "$4" in */rules/*) cat "$ATTENTION_FIXTURE/rules.json" ;; *) cat "$ATTENTION_FIXTURE/branch.json" ;; esac ;;
  */status*) cat "$ATTENTION_FIXTURE/status$page.json" ;;
  *)
    cat "$ATTENTION_FIXTURE/runs$page.json" || exit 1
    exit "$ATTENTION_CHECK_EXIT" ;;
esac
"#;
    let gh = bin.join("gh");
    support::write_executable(&gh, script);
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
    let (dir, output) = cli_fixture(&"c".repeat(40), RUNS, 0);
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
    assert_eq!(args.lines().count(), 6);
    assert!(args.lines().all(|line| line.starts_with("api ")), "{args}");
    assert!(args.contains("repos/example/project/commits/"));
}

const RUNS: &str = r#"{"check_runs":[{"name":"test","status":"completed","conclusion":"success","html_url":"https://github.com/example/project/runs/1","started_at":"2026-10-10T13:00:00Z","completed_at":"2026-10-10T13:02:00Z"}]}"#;

#[test]
#[cfg(unix)]
fn cli_reads_github_over_rest_and_a_missing_required_check_is_not_green() {
    let (_, output) = cli_fixture(&"a".repeat(40), RUNS, 0);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = &json["observations"][URL];
    assert_eq!(row["action"], "review-head");
    assert_eq!(row["evidence"]["required_checks"][0]["state"], "SUCCESS");
    assert_eq!(row["evidence"]["pr"]["mergeStateStatus"], "CLEAN");
    let (_, output) = cli_fixture(&"a".repeat(40), r#"{"check_runs":[]}"#, 0);
    assert_eq!(output.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = &json["observations"][URL];
    assert_eq!(row["action"], "verify-checks");
    assert_eq!(row["evidence"]["required_checks"][0]["state"], "MISSING");
}

#[test]
#[cfg(unix)]
fn cli_rejects_malformed_json_and_unsuccessful_gh_even_with_success_bytes() {
    for (payload, exit) in [("not JSON".to_owned(), 0), (RUNS.to_owned(), 1)] {
        let (_, output) = cli_fixture(&"a".repeat(40), &payload, exit);
        assert_eq!(output.status.code(), Some(1));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["observations"][URL]["action"], "investigate-evidence");
    }
}

#[cfg(unix)]
fn source_row(
    runs: &[String],
    statuses: &[String],
    branch: &str,
    rules: &str,
) -> serde_json::Value {
    let (_, output) = source_fixture(&"a".repeat(40), runs, statuses, branch, rules, 0);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    json["observations"][URL].clone()
}

const REQUIRED: &str = r#"{"protection":{"required_status_checks":{"contexts":["test"]}}}"#;
const EMPTY_STATUS: &str = r#"{"statuses":[]}"#;

fn status_page(state: &str) -> String {
    serde_json::json!({"statuses": [{"context": "test", "state": state,
        "target_url": "https://ci.example/1", "created_at": "2026-10-10T13:00:00Z",
        "updated_at": "2026-10-10T13:01:00Z"}]})
    .to_string()
}

fn full_page(field: &str) -> String {
    let rows: Vec<_> = (0..100).map(|i| match field {
        "check_runs" => serde_json::json!({"name": format!("other-{i}"), "status": "completed",
            "conclusion": "success", "html_url": null, "started_at": null, "completed_at": null}),
        _ => serde_json::json!({"context": format!("other-{i}"), "state": "success",
            "target_url": null, "created_at": "2026-10-10T13:00:00Z", "updated_at": "2026-10-10T13:01:00Z"}),
    }).collect();
    serde_json::json!({field: rows, "total_count": 101}).to_string()
}

#[test]
#[cfg(unix)]
fn a_failing_status_beside_a_green_run_of_the_same_name_is_not_green() {
    let row = source_row(&[RUNS.into()], &[status_page("failure")], REQUIRED, "[]");
    assert_eq!(row["action"], "fix-ci");
}

#[test]
#[cfg(unix)]
fn a_required_run_on_the_second_page_is_found() {
    let row = source_row(
        &[full_page("check_runs"), RUNS.into()],
        &[EMPTY_STATUS.into()],
        REQUIRED,
        "[]",
    );
    assert_eq!(row["evidence"]["required_checks"][0]["state"], "SUCCESS");
}

#[test]
#[cfg(unix)]
fn a_required_status_on_the_second_page_is_found() {
    let row = source_row(
        &[r#"{"check_runs":[]}"#.into()],
        &[full_page("statuses"), status_page("success")],
        REQUIRED,
        "[]",
    );
    assert_eq!(row["action"], "review-head");
}

#[test]
#[cfg(unix)]
fn a_later_failing_run_beats_an_earlier_green_run() {
    let mut first: serde_json::Value = serde_json::from_str(&full_page("check_runs")).unwrap();
    first["check_runs"][0] =
        serde_json::from_str::<serde_json::Value>(RUNS).unwrap()["check_runs"][0].clone();
    let row = source_row(
        &[first.to_string(), RUNS.replace("success", "failure")],
        &[EMPTY_STATUS.into()],
        REQUIRED,
        "[]",
    );
    assert_eq!(row["action"], "fix-ci");
}

#[test]
#[cfg(unix)]
fn a_later_failing_status_beats_a_green_run() {
    let row = source_row(
        &[RUNS.into()],
        &[full_page("statuses"), status_page("failure")],
        REQUIRED,
        "[]",
    );
    assert_eq!(row["action"], "fix-ci");
}

#[test]
#[cfg(unix)]
fn every_app_reporting_an_unrestricted_name_must_be_green() {
    let mut runs: serde_json::Value = serde_json::from_str(RUNS).unwrap();
    runs["check_runs"][0]["app"] = serde_json::json!({"id": 1});
    let mut failed = runs["check_runs"][0].clone();
    failed["app"]["id"] = 2.into();
    failed["conclusion"] = "failure".into();
    runs["check_runs"].as_array_mut().unwrap().push(failed);
    let row = source_row(&[runs.to_string()], &[EMPTY_STATUS.into()], REQUIRED, "[]");
    assert_eq!(row["action"], "fix-ci");
}

#[test]
#[cfg(unix)]
fn a_different_app_cannot_satisfy_a_required_app() {
    let runs = RUNS.replace(r#""name":"test""#, r#""app":{"id":2},"name":"test""#);
    let branch = r#"{"protection":{"required_status_checks":{"contexts":["test"],"checks":[{"context":"test","app_id":1}]}}}"#;
    let row = source_row(&[runs], &[status_page("success")], branch, "[]");
    assert_eq!(row["action"], "verify-checks");
}

#[test]
#[cfg(unix)]
fn a_ruleset_app_constraint_excludes_other_apps() {
    let mut runs: serde_json::Value = serde_json::from_str(RUNS).unwrap();
    runs["check_runs"][0]["app"] = serde_json::json!({"id": 1});
    let mut other = runs["check_runs"][0].clone();
    other["app"]["id"] = 2.into();
    other["conclusion"] = "failure".into();
    runs["check_runs"].as_array_mut().unwrap().insert(0, other);
    let rules = r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"test","integration_id":1}]}}]"#;
    let row = source_row(&[runs.to_string()], &[status_page("success")], "{}", rules);
    assert_eq!(row["action"], "review-head");
}

#[test]
#[cfg(unix)]
fn a_pending_status_beside_a_green_run_stays_pending() {
    let row = source_row(&[RUNS.into()], &[status_page("pending")], REQUIRED, "[]");
    assert_eq!(row["action"], "wait-for-ci");
}

#[test]
#[cfg(unix)]
fn absent_sources_leave_a_required_context_missing() {
    let row = source_row(
        &[r#"{"check_runs":[]}"#.into()],
        &[EMPTY_STATUS.into()],
        REQUIRED,
        "[]",
    );
    assert_eq!(row["evidence"]["required_checks"][0]["state"], "MISSING");
    assert_eq!(row["action"], "verify-checks");
}

#[test]
#[cfg(unix)]
fn green_runs_and_statuses_allow_head_review() {
    let row = source_row(&[RUNS.into()], &[status_page("success")], REQUIRED, "[]");
    assert_eq!(row["action"], "review-head");
}

#[test]
#[cfg(unix)]
fn a_failed_second_page_discards_partial_evidence() {
    for field in ["check_runs", "statuses"] {
        let runs = if field == "check_runs" {
            vec![full_page(field)]
        } else {
            vec![RUNS.into()]
        };
        let statuses = if field == "statuses" {
            vec![full_page(field)]
        } else {
            vec![EMPTY_STATUS.into()]
        };
        let row = source_row(&runs, &statuses, REQUIRED, "[]");
        assert_eq!(row["action"], "investigate-evidence");
        assert_eq!(row["evidence"]["required_checks"], serde_json::json!([]));
    }
}

#[test]
#[cfg(unix)]
fn branch_checks_require_only_the_named_app() {
    let mut runs: serde_json::Value = serde_json::from_str(RUNS).unwrap();
    runs["check_runs"][0]["app"] = serde_json::json!({"id": 1});
    let mut other = runs["check_runs"][0].clone();
    other["app"]["id"] = 2.into();
    other["conclusion"] = "failure".into();
    runs["check_runs"].as_array_mut().unwrap().insert(0, other);
    let branch =
        r#"{"protection":{"required_status_checks":{"checks":[{"context":"test","app_id":1}]}}}"#;
    let row = source_row(&[runs.to_string()], &[EMPTY_STATUS.into()], branch, "[]");
    assert_eq!(row["action"], "review-head");
}
