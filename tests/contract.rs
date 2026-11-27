use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use cm::host::{Host, Invoked};
use cm::outcome::{Input, Move, Next, Outcome, Violation};
use serde_json::{Value, json};

#[allow(dead_code)]
mod support;

fn words(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|word| (*word).to_owned()).collect()
}

fn next(violation: &Violation) -> Value {
    serde_json::to_value(violation).unwrap()["next"].clone()
}

fn resolve_watch() -> Next {
    Next::either([
        (
            "when the signal is in the store".to_owned(),
            Move::new(vec![
                "cm".into(),
                "watch".into(),
                "05-integration/drop-8/probe".into(),
                "resolve".into(),
                "1".into(),
                "--evidence".into(),
                Input::EvidenceFile.into(),
            ]),
        ),
        (
            "when the kickoff counts no event in the window as a pass".to_owned(),
            Move::new(vec![
                "cm".into(),
                "watch".into(),
                "05-integration/drop-8/probe".into(),
                "withdraw".into(),
                "1".into(),
                "--reason".into(),
                Input::Reason.into(),
            ]),
        ),
    ])
}

#[test]
fn one_move_renders_indented_under_the_unchanged_first_line_and_as_a_run_object() {
    let violation = Violation::refuse(
        "01-a/u/run",
        "packet-kickoff",
        "the step folder has no kickoff.md",
        Next::run(vec![
            "cm".into(),
            "amend".into(),
            "01-a/u/run".into(),
            "--kickoff-file".into(),
            Input::KickoffFile.into(),
        ]),
    );
    assert_eq!(
        violation.to_string(),
        "01-a/u/run: packet-kickoff: the step folder has no kickoff.md\n  next: cm amend 01-a/u/run --kickoff-file '<kickoff-file>'"
    );
    assert_eq!(
        next(&violation),
        json!([{
            "kind": "run",
            "when": null,
            "argv": ["cm", "amend", "01-a/u/run", "--kickoff-file", "<kickoff-file>"],
            "inputs": ["kickoff-file"],
            "command": "cm amend 01-a/u/run --kickoff-file '<kickoff-file>'",
        }])
    );
}

#[test]
fn either_renders_each_move_under_its_condition_in_text_and_json() {
    let violation = Violation::refuse(
        "05-integration/drop-8/probe",
        "watch-open",
        "watch 1 (span gateway.message) is pending past its deadline; the step stays released",
        resolve_watch(),
    );
    assert_eq!(
        violation.to_string(),
        "05-integration/drop-8/probe: watch-open: watch 1 (span gateway.message) is pending past its deadline; the step stays released
  next, when the signal is in the store:
    cm watch 05-integration/drop-8/probe resolve 1 --evidence '<evidence-file>'
  next, when the kickoff counts no event in the window as a pass:
    cm watch 05-integration/drop-8/probe withdraw 1 --reason '<reason>'"
    );
    assert_eq!(
        next(&violation),
        json!([
            {"kind": "run", "when": "when the signal is in the store",
             "argv": ["cm", "watch", "05-integration/drop-8/probe", "resolve", "1", "--evidence", "<evidence-file>"],
             "inputs": ["evidence-file"],
             "command": "cm watch 05-integration/drop-8/probe resolve 1 --evidence '<evidence-file>'"},
            {"kind": "run", "when": "when the kickoff counts no event in the window as a pass",
             "argv": ["cm", "watch", "05-integration/drop-8/probe", "withdraw", "1", "--reason", "<reason>"],
             "inputs": ["reason"],
             "command": "cm watch 05-integration/drop-8/probe withdraw 1 --reason '<reason>'"},
        ])
    );
}

#[test]
#[should_panic(expected = "at least two moves")]
fn an_either_of_one_move_is_refused_at_construction() {
    Next::either([(
        "when it holds".to_owned(),
        Move::new(vec!["cm".into(), "lint".into()]),
    )]);
}

#[test]
#[should_panic(expected = "condition")]
fn an_either_move_without_a_condition_is_refused_at_construction() {
    Next::either([
        (" ".to_owned(), Move::new(vec!["cm".into(), "lint".into()])),
        (
            "when".to_owned(),
            Move::new(vec!["cm".into(), "sync".into()]),
        ),
    ]);
}

fn renumber() -> Violation {
    Violation::refuse(
        "MAC-LANE.md",
        "mac-lane-number",
        "item 12 is numbered 14; items run 1, 2, 3… in order",
        Next::missing("renumber MAC-LANE.md"),
    )
    .at(41)
}

#[test]
fn an_unrecorded_missing_command_names_the_feedback_recipe_then_the_adopt() {
    let violation = renumber();
    assert_eq!(
        violation.to_string(),
        "MAC-LANE.md:41: mac-lane-number: item 12 is numbered 14; items run 1, 2, 3… in order
  missing-command: renumber MAC-LANE.md (not yet recorded)
  next: cm feedback 'missing command: renumber MAC-LANE.md' --urgency queued --body-file '<body-file>'
  then: make the change by hand, then: cm adopt MAC-LANE.md:41 --feedback '<feedback-id>'"
    );
    assert_eq!(
        next(&violation),
        json!([{
            "kind": "missing-command",
            "what": "renumber MAC-LANE.md",
            "recorded": null,
            "argv": ["cm", "feedback", "missing command: renumber MAC-LANE.md", "--urgency", "queued", "--body-file", "<body-file>"],
            "inputs": ["body-file"],
            "command": "cm feedback 'missing command: renumber MAC-LANE.md' --urgency queued --body-file '<body-file>'",
        }])
    );
    assert!(cm::cli::parses(&words(&[
        "cm",
        "feedback",
        "missing command: renumber MAC-LANE.md",
        "--urgency",
        "queued",
        "--body-file",
        "body.md"
    ])));
}

#[test]
fn a_missing_command_feedback_md_records_names_its_id_and_the_adopt() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("FEEDBACK.md"),
        "# Feedback\n\n### FB-e-020 · queued · missing command: something else\n\nBody.\n\n### FB-e-021 · queued · missing command: renumber MAC-LANE.md\n\nBody.\n",
    )
    .unwrap();
    let mut outcome = Outcome::refused(vec![renumber()]);
    outcome.resolve_missing(dir.path());
    let violation = &outcome.violations[0];
    assert_eq!(
        violation.to_string(),
        "MAC-LANE.md:41: mac-lane-number: item 12 is numbered 14; items run 1, 2, 3… in order
  missing-command: renumber MAC-LANE.md (recorded as FB-e-021)
  next: make the change by hand, then: cm adopt MAC-LANE.md:41 --feedback FB-e-021"
    );
    assert_eq!(
        next(violation),
        json!([{
            "kind": "missing-command",
            "what": "renumber MAC-LANE.md",
            "recorded": "FB-e-021",
            "argv": ["cm", "adopt", "MAC-LANE.md:41", "--feedback", "FB-e-021"],
            "inputs": [],
            "command": "cm adopt MAC-LANE.md:41 --feedback FB-e-021",
        }])
    );
}

#[test]
fn a_settled_refusal_prints_nothing_to_do_and_why() {
    let violation = Violation::refuse(
        "05-integration/drop-8/probe",
        "watch-resolved",
        "watch 1 is not pending",
        Next::settled("05-integration/drop-8/probe#1 is already resolved"),
    );
    assert_eq!(
        violation.to_string(),
        "05-integration/drop-8/probe: watch-resolved: watch 1 is not pending\n  next: nothing; 05-integration/drop-8/probe#1 is already resolved"
    );
    assert_eq!(
        next(&violation),
        json!([{"kind": "settled", "why": "05-integration/drop-8/probe#1 is already resolved"}])
    );
}

#[test]
#[allow(deprecated)]
fn an_unmigrated_refusal_renders_as_before_with_an_empty_next() {
    let violation = Violation::new("JOURNAL.md", "journal-rewritten", "the journal changed").at(3);
    assert_eq!(
        violation.to_string(),
        "JOURNAL.md:3: journal-rewritten: the journal changed"
    );
    assert_eq!(
        serde_json::to_value(&violation).unwrap(),
        json!({"path": "JOURNAL.md", "line": 3, "rule": "journal-rewritten", "message": "the journal changed", "next": []})
    );
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "a move cm does not parse")]
fn a_move_with_an_unknown_flag_trips_the_debug_assert() {
    Move::new(vec![
        "cm".into(),
        "amend".into(),
        "01-a/u/run".into(),
        "--kickoff".into(),
        Input::KickoffFile.into(),
    ]);
}

#[test]
fn every_input_renders_as_its_kebab_placeholder_and_samples_a_plain_value() {
    for input in cm::outcome::INPUTS {
        assert!(
            !input.sample().is_empty() && !input.sample().contains('<'),
            "{input:?}"
        );
        assert_eq!(input.placeholder(), format!("<{}>", input.name()));
    }
    assert!(cm::cli::parses(&words(&[
        "cm",
        "init",
        "01-a/u/s",
        "--lane",
        cm::outcome::Input::Lane.sample()
    ])));
}

#[test]
fn again_reissues_the_recorded_invocation_without_json_and_with_one_input_corrected() {
    let host = Invoked::record(words(&[
        "/usr/local/bin/cm",
        "--json",
        "advance",
        "01-a/u/s",
        "done",
        "--evidence",
        "gone.md",
    ]));
    assert_eq!(
        host.argv(),
        Some(words(&[
            "cm",
            "advance",
            "01-a/u/s",
            "done",
            "--evidence",
            "gone.md"
        ]))
    );
    let fixed = Move::again(host.argv().unwrap()).set("--evidence", Input::EvidenceFile);
    assert_eq!(
        fixed.command(),
        "cm advance 01-a/u/s done --evidence '<evidence-file>'"
    );
    let inline = Move::again(words(&[
        "cm",
        "advance",
        "01-a/u/s",
        "done",
        "--evidence=gone.md",
    ]))
    .set("--evidence", Input::EvidenceFile);
    assert_eq!(
        inline.command(),
        "cm advance 01-a/u/s done --evidence '<evidence-file>'"
    );
    let added = Move::again(words(&["cm", "advance", "01-a/u/s", "done"]))
        .set("--evidence", Input::EvidenceFile);
    assert_eq!(added.inputs(), ["evidence-file"]);
}

struct Scratch {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    gh: support::FakeGh,
}

impl Scratch {
    fn new(steps: &[(&str, &str)]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaign");
        let scratch = Scratch {
            _temp: temp,
            root,
            gh: support::FakeGh::new("exit 1\n"),
        };
        git(temp_parent(&scratch.root), &["init", "-q", "campaign"]);
        assert!(scratch.cm(&["init", "."]).status.success());
        for (step, lane) in steps {
            let declared = scratch.cm(&["init", step, "--lane", lane]);
            assert!(declared.status.success(), "{declared:?}");
        }
        scratch
    }

    fn cm(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&self.root)
            .args(args)
            .env("PATH", self.gh.path())
            .env_remove("CLAUDE_CODE_REMOTE_SESSION_ID")
            .output()
            .unwrap()
    }

    fn refused(&self, args: &[&str]) -> Value {
        let output = self.cm(args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn temp_parent(root: &Path) -> &Path {
    root.parent().unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn rule_next(json: &Value, rule: &str) -> Value {
    json["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["rule"] == rule)
        .unwrap_or_else(|| panic!("no {rule} in {json}"))["next"]
        .clone()
}

fn argvs(next: &Value) -> Vec<Value> {
    next.as_array()
        .unwrap()
        .iter()
        .map(|each| each["argv"].clone())
        .collect()
}

#[test]
fn owner_queue_not_open_on_an_empty_queue_names_the_add_that_blocks_the_step() {
    let c = Scratch::new(&[("01-a/u/park", "codex")]);
    let refused = c.refused(&[
        "--json",
        "advance",
        "01-a/u/park",
        "blocked:until:owner-queue:7",
    ]);
    let next = rule_next(&refused, "owner-queue-not-open");
    assert_eq!(
        argvs(&next),
        [json!([
            "cm",
            "owner-queue",
            "add",
            "<title>",
            "--context",
            "<context>",
            "--question",
            "<question>",
            "--recommendation",
            "<recommendation>",
            "--block",
            "01-a/u/park"
        ])]
    );
    assert_eq!(next[0]["when"], Value::Null);
}

#[test]
fn owner_queue_not_open_lists_each_open_item_with_its_title_and_block_argv() {
    let c = Scratch::new(&[("01-a/u/park", "codex")]);
    let added = c.cm(&[
        "owner-queue",
        "add",
        "Chart termination",
        "--context",
        "c",
        "--question",
        "q?",
        "--recommendation",
        "r",
    ]);
    assert!(added.status.success(), "{added:?}");
    let refused = c.refused(&[
        "--json",
        "advance",
        "01-a/u/park",
        "blocked:until:owner-queue:31",
    ]);
    let violation = &refused["violations"][0];
    assert_eq!(violation["rule"], "owner-queue-not-open");
    assert!(
        violation["message"]
            .as_str()
            .unwrap()
            .contains("1. Chart termination"),
        "{violation}"
    );
    let next = &violation["next"];
    assert_eq!(argvs(next)[0][2], "add");
    assert_eq!(
        argvs(next)[1],
        json!([
            "cm",
            "advance",
            "01-a/u/park",
            "blocked:until:owner-queue:1"
        ])
    );
    assert_eq!(next[1]["inputs"], json!([]));
    assert!(
        next[1]["when"]
            .as_str()
            .unwrap()
            .contains("item 1, `Chart termination`")
    );
    let text = c.cm(&["advance", "01-a/u/park", "blocked:until:owner-queue:31"]);
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert!(
        stdout.starts_with("01-a/u/park: owner-queue-not-open: OWNER-QUEUE.md has no open item 31"),
        "{stdout}"
    );
    assert!(stdout.contains("\n    cm advance 01-a/u/park blocked:until:owner-queue:1\n"));
    let blocked = c.cm(&["advance", "01-a/u/park", "blocked:until:owner-queue:1"]);
    assert!(blocked.status.success(), "{blocked:?}");
}

#[test]
fn packet_kickoff_names_the_amend_of_the_real_step_and_the_attempt_record_its_begin_and_claim() {
    let c = Scratch::new(&[("01-a/u/run", "claude-cloud")]);
    let refused = c.refused(&[
        "--json",
        "launch",
        "01-a/u/run",
        "--coordinator",
        "session_x",
    ]);
    let next = rule_next(&refused, "packet-kickoff");
    assert_eq!(
        argvs(&next),
        [json!([
            "cm",
            "amend",
            "01-a/u/run",
            "--kickoff-file",
            "<kickoff-file>"
        ])]
    );
    let kickoff = c.root.join("../kickoff.md");
    fs::write(&kickoff, "Do it.\n").unwrap();
    let amended = c.cm(&[
        "amend",
        "01-a/u/run",
        "--kickoff-file",
        kickoff.to_str().unwrap(),
    ]);
    assert!(amended.status.success(), "{amended:?}");
    let launched = c.cm(&[
        "--json",
        "launch",
        "01-a/u/run",
        "--coordinator",
        "session_x",
    ]);
    assert!(launched.status.success(), "{launched:?}");
    let shown: Value =
        serde_json::from_slice(&c.cm(&["--json", "attempt", "01-a/u/run"]).stdout).unwrap();
    let claim = shown["claim"].as_str().unwrap().to_owned();
    let refused = c.refused(&[
        "--json",
        "attempt",
        "01-a/u/run",
        "record",
        "turn",
        "t-1",
        "--claim",
        &claim,
    ]);
    let next = rule_next(&refused, "attempt-record");
    assert_eq!(
        argvs(&next),
        [json!([
            "cm",
            "attempt",
            "01-a/u/run",
            "begin",
            "turn",
            "--claim",
            claim
        ])]
    );
    assert_eq!(next[0]["inputs"], json!([]));
    let began = c.cm(&["attempt", "01-a/u/run", "begin", "turn", "--claim", &claim]);
    assert!(began.status.success(), "{began:?}");
}

#[test]
fn owner_queue_not_open_on_a_unit_adds_without_block_and_blocks_the_unit() {
    let c = Scratch::new(&[("01-a/u/park", "codex")]);
    let refused = c.refused(&["--json", "advance", "01-a/u", "blocked:until:owner-queue:7"]);
    let next = rule_next(&refused, "owner-queue-not-open");
    let add = &argvs(&next)[0];
    assert!(
        !add.as_array().unwrap().contains(&json!("--block")),
        "{add}"
    );
    let added = c.cm(&[
        "owner-queue",
        "add",
        "Chart termination",
        "--context",
        "c",
        "--question",
        "q?",
        "--recommendation",
        "r",
    ]);
    assert!(added.status.success(), "{added:?}");
    let refused = c.refused(&["--json", "advance", "01-a/u", "blocked:until:owner-queue:7"]);
    let next = rule_next(&refused, "owner-queue-not-open");
    assert_eq!(
        argvs(&next)[1],
        json!(["cm", "advance", "01-a/u", "blocked:until:owner-queue:1"])
    );
    let blocked = c.cm(&["advance", "01-a/u", "blocked:until:owner-queue:1"]);
    assert!(blocked.status.success(), "{blocked:?}");
}

#[test]
fn owner_queue_not_open_on_a_part_blocks_the_whole_step() {
    let c = Scratch::new(&[]);
    let declared = c.cm(&["init", "01-a/u/s", "--lane", "codex", "--part", "p"]);
    assert!(declared.status.success(), "{declared:?}");
    let added = c.cm(&[
        "owner-queue",
        "add",
        "Chart termination",
        "--context",
        "c",
        "--question",
        "q?",
        "--recommendation",
        "r",
    ]);
    assert!(added.status.success(), "{added:?}");
    let refused = c.refused(&[
        "--json",
        "advance",
        "01-a/u/s",
        "blocked:until:owner-queue:9",
        "--part",
        "p",
    ]);
    let next = rule_next(&refused, "owner-queue-not-open");
    assert_eq!(
        argvs(&next)[1],
        json!(["cm", "advance", "01-a/u/s", "blocked:until:owner-queue:1"])
    );
    let blocked = c.cm(&["advance", "01-a/u/s", "blocked:until:owner-queue:1"]);
    assert!(blocked.status.success(), "{blocked:?}");
}

#[test]
fn a_move_printed_for_a_selected_campaign_replays_in_that_campaign() {
    let a = Scratch::new(&[("01-a/u/run", "claude-cloud")]);
    let b = Scratch::new(&[("01-a/u/run", "claude-cloud")]);
    let b_root = b.root.to_str().unwrap();
    let refused = a.refused(&[
        "-C",
        b_root,
        "--json",
        "launch",
        "01-a/u/run",
        "--coordinator",
        "session_x",
    ]);
    let next = rule_next(&refused, "packet-kickoff");
    let root = std::path::absolute(&b.root).unwrap();
    assert_eq!(
        argvs(&next),
        [json!([
            "cm",
            "-C",
            root.to_str().unwrap(),
            "amend",
            "01-a/u/run",
            "--kickoff-file",
            "<kickoff-file>"
        ])]
    );
    let kickoff = a.root.join("../kickoff.md");
    fs::write(&kickoff, "Do it.\n").unwrap();
    let words: Vec<String> = argvs(&next)[0]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|word| match word.as_str().unwrap() {
            "<kickoff-file>" => kickoff.to_str().unwrap().to_owned(),
            word => word.to_owned(),
        })
        .collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let amended = a.cm(&words);
    assert!(amended.status.success(), "{amended:?}");
    let launched = b.cm(&["launch", "01-a/u/run", "--coordinator", "session_x"]);
    assert!(launched.status.success(), "{launched:?}");
    let still = a.refused(&[
        "--json",
        "launch",
        "01-a/u/run",
        "--coordinator",
        "session_x",
    ]);
    rule_next(&still, "packet-kickoff");
}

#[test]
fn a_selected_campaign_prefixes_the_feedback_and_adopt_moves() {
    let dir = tempfile::tempdir().unwrap();
    let campaign = words(&["cm", "-C", "/c"]);
    let mut outcome = Outcome::refused(vec![renumber()]);
    outcome.resolve_missing_in(dir.path(), &campaign);
    assert_eq!(
        next(&outcome.violations[0])[0]["argv"],
        json!([
            "cm",
            "-C",
            "/c",
            "feedback",
            "missing command: renumber MAC-LANE.md",
            "--urgency",
            "queued",
            "--body-file",
            "<body-file>"
        ])
    );
    assert!(
        outcome.violations[0]
            .to_string()
            .contains("then: cm -C /c adopt MAC-LANE.md:41")
    );
}

#[cfg(unix)]
#[test]
fn a_non_utf8_argument_records_no_argv_and_does_not_panic() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let odd = OsString::from_vec(b"camp\xffaign".to_vec());
    let host = Invoked::record([OsString::from("cm"), OsString::from("init"), odd.clone()]);
    assert_eq!(host.argv(), None);
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(dir.path())
        .arg("--json")
        .arg("init")
        .arg(&odd)
        .output()
        .unwrap();
    assert_ne!(output.status.code(), Some(101), "{output:?}");
}

#[test]
fn a_leading_dash_kind_is_begun_after_the_end_of_options() {
    let c = Scratch::new(&[("01-a/u/run", "claude-cloud")]);
    fs::write(c.root.join("../kickoff.md"), "Do it.\n").unwrap();
    let kickoff = c.root.join("../kickoff.md");
    assert!(
        c.cm(&[
            "amend",
            "01-a/u/run",
            "--kickoff-file",
            kickoff.to_str().unwrap()
        ])
        .status
        .success()
    );
    assert!(
        c.cm(&["launch", "01-a/u/run", "--coordinator", "session_x"])
            .status
            .success()
    );
    let shown: Value =
        serde_json::from_slice(&c.cm(&["--json", "attempt", "01-a/u/run"]).stdout).unwrap();
    let claim = shown["claim"].as_str().unwrap().to_owned();
    let refused = c.refused(&[
        "--json",
        "attempt",
        "01-a/u/run",
        "--claim",
        &claim,
        "record",
        "--",
        "-turn",
        "t-2",
    ]);
    let next = rule_next(&refused, "attempt-record");
    assert_eq!(
        argvs(&next),
        [json!([
            "cm",
            "attempt",
            "01-a/u/run",
            "begin",
            "--claim",
            claim,
            "--",
            "-turn"
        ])]
    );
    let began = c.cm(&[
        "attempt",
        "01-a/u/run",
        "begin",
        "--claim",
        &claim,
        "--",
        "-turn",
    ]);
    assert!(began.status.success(), "{began:?}");
    assert_eq!(
        cm::outcome::option("--claim", "-c"),
        [cm::outcome::Arg::Lit("--claim=-c".into())]
    );
}

#[test]
fn replay_keeps_positional_data_after_the_end_of_options() {
    let host = Invoked::record(words(&["cm", "--json", "journal", "--", "--json"]));
    assert_eq!(host.argv(), Some(words(&["cm", "journal", "--", "--json"])));
    let rooted = Invoked::record(words(&["cm", "-C", "--json", "status"]));
    assert_eq!(
        rooted.argv(),
        Some(words(&["cm", "-C", "--json", "status"]))
    );
    let moved = Move::again(words(&["cm", "advance", "--", "01-a/u/run", "done"]))
        .set("--evidence", Input::EvidenceFile);
    assert_eq!(
        moved.command(),
        "cm advance --evidence '<evidence-file>' -- 01-a/u/run done"
    );
    let dashed = Move::again(words(&["cm", "journal", "--", "--evidence"]))
        .set("--body-file", Input::BodyFile);
    assert_eq!(
        dashed.command(),
        "cm journal --body-file '<body-file>' -- --evidence"
    );
}

#[test]
fn a_missing_command_title_holding_the_ledger_delimiter_is_found_whole() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("FEEDBACK.md"),
        "# Feedback\n\n### FB-e-001 · queued · missing command: rename A · B\n\nBody.\n",
    )
    .unwrap();
    let mut outcome = Outcome::refused(vec![Violation::refuse(
        "x.md",
        "x-rule",
        "cause",
        Next::missing("rename A · B"),
    )]);
    outcome.resolve_missing(dir.path());
    assert_eq!(next(&outcome.violations[0])[0]["recorded"], "FB-e-001");
    let mut other = Outcome::refused(vec![Violation::refuse(
        "x.md",
        "x-rule",
        "cause",
        Next::missing("B"),
    )]);
    other.resolve_missing(dir.path());
    assert_eq!(next(&other.violations[0])[0]["recorded"], Value::Null);
}

#[test]
fn the_printed_outcome_resolves_missing_commands_against_the_selected_campaign() {
    let c = Scratch::new(&[]);
    fs::write(
        c.root.join("FEEDBACK.md"),
        "# Feedback\n\n### FB-e-021 · queued · missing command: renumber MAC-LANE.md\n\nBody.\n",
    )
    .unwrap();
    let host = Invoked::record(words(&["cm", "status"])).in_campaign(&c.root);
    let outcome = cm::cli::resolved(Some(&c.root), &host, Outcome::refused(vec![renumber()]));
    let root = std::path::absolute(&c.root).unwrap();
    assert_eq!(
        next(&outcome.violations[0])[0]["argv"],
        json!([
            "cm",
            "-C",
            root.to_str().unwrap(),
            "adopt",
            "MAC-LANE.md:41",
            "--feedback",
            "FB-e-021"
        ])
    );
}

#[test]
fn a_fenced_or_claimless_record_is_not_handed_the_live_claim() {
    let c = Scratch::new(&[("01-a/u/run", "claude-cloud")]);
    let kickoff = c.root.join("../kickoff.md");
    fs::write(&kickoff, "Do it.\n").unwrap();
    assert!(
        c.cm(&[
            "amend",
            "01-a/u/run",
            "--kickoff-file",
            kickoff.to_str().unwrap()
        ])
        .status
        .success()
    );
    assert!(
        c.cm(&["launch", "01-a/u/run", "--coordinator", "session_x"])
            .status
            .success()
    );
    let shown: Value =
        serde_json::from_slice(&c.cm(&["--json", "attempt", "01-a/u/run"]).stdout).unwrap();
    let claim = shown["claim"].as_str().unwrap().to_owned();
    for given in [vec!["--claim", "stale-claim"], vec![]] {
        let mut args = vec!["--json", "attempt", "01-a/u/run", "record", "turn", "t-1"];
        args.extend(given);
        let refused = c.refused(&args);
        let next = rule_next(&refused, "attempt-record");
        assert_eq!(argvs(&next), [json!(["cm", "attempt", "01-a/u/run"])]);
        assert!(
            !refused
                .to_string()
                .contains(&format!("begin turn --claim {claim}"))
        );
    }
}
