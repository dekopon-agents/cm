use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

#[allow(dead_code)]
mod support;

fn cm(dir: &Path, args: &[&str]) -> Output {
    let gh = support::FakeGh::new("exit 1\n");
    Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(dir)
        .args(args)
        .env("PATH", gh.path())
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn mac_lane_add_links_the_declared_step_and_lists_it_until_done() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let step = "01-first/alpha/mac-work";
    assert!(cm(root, &["init"]).status.success());
    assert!(cm(root, &["init", step, "--lane", "mac"]).status.success());
    assert!(
        cm(root, &["mac-lane", "add", "Existing request"])
            .status
            .success()
    );

    let added = cm(
        root,
        &["--json", "mac-lane", "add", "Mac work", "--step", step],
    );
    assert!(added.status.success(), "{added:?}");
    assert_eq!(json(&added)["item"], 2);
    assert!(
        fs::read_to_string(root.join("MAC-LANE.md"))
            .unwrap()
            .ends_with(&format!(
                "1. **Existing request**\n2. **Mac work**\n\tStep: `{step}`\n"
            ))
    );
    let unit = root.join("01-first/alpha");
    let step_state = || cm::store::load_unit(&unit).unwrap().steps["mac-work"].clone();
    assert_eq!(step_state().mac_lane_items, [2]);

    let listed = cm(root, &["mac-lane", "list"]);
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        format!("1. Existing request (open)\n2. Mac work (open) — {step}\n")
    );
    let listed = cm(root, &["--json", "mac-lane", "list"]);
    assert_eq!(
        json(&listed)["items"],
        serde_json::json!([
            {"item": 1, "state": "open", "title": "Existing request"},
            {"item": 2, "state": "open", "title": "Mac work", "step": step},
        ])
    );

    let done = cm(
        root,
        &["mac-lane", "done", "2", "--receipt", "verified on Mac"],
    );
    assert!(done.status.success(), "{done:?}");
    assert_eq!(step_state().track.state, cm::state::Phase::Drafted);
    assert_eq!(step_state().mac_lane_items, [2]);
    let listed = cm(root, &["--json", "mac-lane", "list"]);
    assert_eq!(json(&listed)["items"].as_array().unwrap().len(), 1);
    let receipt = String::from_utf8(done.stdout).unwrap();
    fs::write(root.join("receipt.md"), receipt).unwrap();
    let advance = cm(root, &["advance", step, "done", "--evidence", "receipt.md"]);
    assert!(advance.status.success(), "{advance:?}");
    assert_eq!(step_state().track.state, cm::state::Phase::Done);
}

#[test]
fn mac_lane_add_refuses_an_undeclared_step_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    assert!(cm(root, &["init", "01-first/alpha"]).status.success());
    let journal = root.join("JOURNAL.md");
    let before = fs::read(&journal).unwrap();
    let target = "01-first/alpha/undeclared";
    let refused = cm(
        root,
        &["--json", "mac-lane", "add", "Mac work", "--step", target],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(json(&refused)["violations"][0]["rule"], "step-unknown");
    assert_eq!(json(&refused)["violations"][0]["path"], target);
    assert!(!root.join("MAC-LANE.md").exists());
    assert_eq!(fs::read(journal).unwrap(), before);
}

#[test]
fn mac_lane_add_restores_the_queue_if_writing_the_step_link_fails() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let step = "01-first/alpha/mac-work";
    assert!(cm(root, &["init"]).status.success());
    assert!(cm(root, &["init", step, "--lane", "mac"]).status.success());
    assert!(
        cm(root, &["mac-lane", "add", "Existing request"])
            .status
            .success()
    );
    let queue = root.join("MAC-LANE.md");
    let before = fs::read(&queue).unwrap();

    let blocked = root.join("01-first/alpha/.cm/.state.json.cm-tmp");
    fs::create_dir(&blocked).unwrap();
    let args = ["--json", "mac-lane", "add", "Mac work", "--step", step];
    assert_eq!(cm(root, &args).status.code(), Some(1));
    assert_eq!(fs::read(queue).unwrap(), before);
    fs::remove_dir(blocked).unwrap();
    let retry = cm(root, &args);
    assert!(retry.status.success(), "{retry:?}");
    assert_eq!(json(&retry)["item"], 2);
}

#[test]
fn mac_lane_round_trips_through_declare_amend_and_state_render() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let unit = root.join("01-first/alpha");
    let step = "01-first/alpha/mac-work";
    assert!(cm(root, &["init"]).status.success());
    let check_lane = || {
        let state = cm::store::load_unit(&unit).unwrap();
        assert_eq!(state.steps["mac-work"].lane, Some(cm::state::Lane::Mac));
        assert!(
            fs::read_to_string(unit.join("STATE.md"))
                .unwrap()
                .contains("| mac-work | mac |")
        );
    };
    assert!(cm(root, &["init", step, "--lane", "mac"]).status.success());
    check_lane();
    assert!(
        cm(root, &["amend", step, "--lane", "coordinator"])
            .status
            .success()
    );
    assert!(cm(root, &["amend", step, "--lane", "mac"]).status.success());
    check_lane();
}

#[test]
fn init_then_lint_is_clean_and_two_hand_edits_are_both_listed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let git = Command::new("git").args(["init", "-q"]).arg(root).status();
    assert!(git.unwrap().success());
    assert_eq!(cm(root, &["init"]).status.code(), Some(0));
    assert_eq!(cm(root, &["init", "01-first/alpha"]).status.code(), Some(0));
    let clean = cm(root, &["lint", "--json"]);
    assert_eq!(clean.status.code(), Some(0));
    assert_eq!(json(&clean)["ok"], true);

    let state_md = root.join("01-first/alpha/STATE.md");
    let edited = fs::read_to_string(&state_md)
        .unwrap()
        .replace("drafted", "done");
    fs::write(&state_md, edited).unwrap();
    let mut journal = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    journal.push_str("\n## 2026-10-08T07:0xZ — typed by a model\n");
    fs::write(root.join("JOURNAL.md"), journal).unwrap();

    let dirty = cm(&root.join("01-first"), &["lint", "--json"]);
    assert_eq!(dirty.status.code(), Some(1));
    let report = json(&dirty);
    let paths: Vec<&str> = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"JOURNAL.md"), "{report}");
    assert!(paths.contains(&"01-first/alpha/STATE.md"), "{report}");

    let state_before = fs::read(root.join(".cm/state.json")).unwrap();
    let journal_before = fs::read(root.join("JOURNAL.md")).unwrap();
    let refused = cm(root, &["close", "--json"]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(json(&refused)["violations"], report["violations"]);
    assert_eq!(fs::read(root.join(".cm/state.json")).unwrap(), state_before);
    assert_eq!(fs::read(root.join("JOURNAL.md")).unwrap(), journal_before);
}

#[test]
fn journal_writes_a_stamped_heading_from_a_body_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    fs::write(root.join("body.md"), "Quoted: \"go\".\n").unwrap();
    let output = cm(
        root,
        &[
            "--json",
            "journal",
            "owner ruling",
            "--body-file",
            "body.md",
        ],
    );
    assert!(output.status.success());
    let heading = json(&output)["heading"].as_str().unwrap().to_owned();
    assert!(cm::journal::is_stamped(&heading), "{heading}");
    assert!(heading.ends_with(" — owner ruling"));
    let text = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    assert!(
        text.ends_with(&format!("{heading}\n\nQuoted: \"go\".\n")),
        "{text}"
    );
}

#[test]
fn usage_errors_exit_2_and_refusals_exit_1() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert_eq!(cm(root, &["frobnicate"]).status.code(), Some(2));
    assert_eq!(
        cm(root, &["launch", "not-a-target", "--worktree", "."])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        cm(root, &["advance", "01-a/u", "open"]).status.code(),
        Some(2)
    );
    let outside = cm(root, &["--json", "journal", "x"]);
    assert_eq!(outside.status.code(), Some(1));
    assert_eq!(json(&outside)["ok"], false);
    assert_eq!(json(&outside)["violations"], serde_json::json!([]));
    assert!(cm(root, &["init"]).status.success());
    let refused = cm(
        root,
        &["--json", "advance", "01-a/u", "merged", "--sha", "abc1234"],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(json(&refused)["violations"][0]["rule"], "unit-missing");
    let lint_outside = cm(temp.path().parent().unwrap(), &["lint", "--json", "/"]);
    assert_eq!(lint_outside.status.code(), Some(1));
}

#[test]
fn init_resolves_a_relative_folder_against_dash_c() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    fs::create_dir_all(&a).unwrap();
    assert!(cm(&a, &["init"]).status.success());
    assert!(cm(temp.path(), &["-C", "b", "init"]).status.success());
    let declared = cm(
        &a,
        &[
            "--json",
            "-C",
            b.to_str().unwrap(),
            "init",
            "01-x/u/S1",
            "--lane",
            "pi",
        ],
    );
    assert!(declared.status.success(), "{}", json(&declared));
    assert!(b.join("01-x/u/.cm/state.json").is_file());
    assert!(!a.join("01-x").exists());
}

#[test]
fn a_queue_item_without_context_is_reported_and_left_as_written() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    let queue =
        "# Owner queue\n\n## Pick one\n\nWhich path?\n\nRecommendation: the first\n\nAnswer:\n";
    fs::write(root.join("OWNER-QUEUE.md"), queue).unwrap();
    let resume = fs::read_to_string(root.join("RESUME.md")).unwrap();
    let start = resume.find("> ```").unwrap();
    let end = resume.find("\n\n# Resume").unwrap();
    let withheld = format!(
        "{}> **OWNER-QUEUE not drained: /loop withheld**{}",
        &resume[..start],
        &resume[end..]
    );
    fs::write(root.join("RESUME.md"), withheld).unwrap();

    let linted = cm(root, &["lint", "--json"]);
    assert_eq!(linted.status.code(), Some(1));
    let report = json(&linted);
    let violations = report["violations"].as_array().unwrap();
    assert_eq!(violations.len(), 1, "{report}");
    assert_eq!(violations[0]["rule"], "owner-queue-field");
    assert_eq!(
        violations[0]["message"],
        "item `Pick one` has no line starting `Context:`"
    );
    assert!(cm(root, &["init"]).status.success());
    assert_eq!(
        fs::read_to_string(root.join("OWNER-QUEUE.md")).unwrap(),
        queue
    );
}

#[test]
fn review_record_ruling_and_brief_run_beside_the_review_row() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(cm(root, &["init"]).status.success());
    assert!(
        cm(root, &["init", "01-a/u/S1", "--lane", "codex"])
            .status
            .success()
    );
    fs::write(root.join("01-a/u/S1/kickoff.md"), "Go.\n").unwrap();
    assert!(
        cm(root, &["ruling", "01-a/u/S1", "the edge is in scope"])
            .status
            .success()
    );
    let brief = json(&cm(root, &["--json", "brief", "01-a/u/S1"]));
    assert!(
        brief["brief"]
            .as_str()
            .unwrap()
            .starts_with("Go.\n\n## Coordinator scope and rulings\n")
    );
    fs::write(
        root.join("report.md"),
        "Prose.\n\n```cm-review\nhead: abcdef1\nF1 | taste | introduced | - | a nit\n```\n\nACCEPT\n",
    )
    .unwrap();
    let recorded = cm(
        root,
        &[
            "--json",
            "review",
            "record",
            "01-a/u/S1",
            "report.md",
            "--reviewer",
            "astra",
        ],
    );
    assert_eq!(recorded.status.code(), Some(0), "{recorded:?}");
    assert_eq!(json(&recorded)["verdict"], "ACCEPT");
    let row = cm(
        root,
        &[
            "review",
            "01-a/u/S1",
            "--pr",
            "https://github.com/o/r/pull/1",
            "--head",
            "abcdef1",
            "--model",
            "m",
            "--effort",
            "e",
            "--verdict",
            "accept",
            "--findings",
            "0/0",
            "--cost",
            "1",
            "--wall",
            "2",
        ],
    );
    assert_eq!(row.status.code(), Some(0), "{row:?}");
    let followups = json(&cm(root, &["--json", "followups"]));
    assert_eq!(followups["followups"][0]["id"], "R1.F1");
    assert!(
        cm(
            root,
            &["followups", "close", "R1.F1", "--by", "stays: style"]
        )
        .status
        .success()
    );
    assert_eq!(cm(root, &["lint"]).status.code(), Some(0));
}

#[test]
fn init_part_and_launch_no_parts_flags_reach_the_commands() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(cm(root, &["init"]).status.success());
    for (step, flags) in [
        ("01-a/u/S1", vec!["--part", "a", "--part", "b"]),
        ("01-a/u/S2", vec![]),
    ] {
        let mut args = vec!["init", step, "--lane", "codex"];
        args.extend(flags);
        let output = cm(root, &args);
        assert!(output.status.success(), "{output:?}");
        fs::write(root.join(step).join("kickoff.md"), "Parts: a, b\n").unwrap();
    }
    assert!(cm(root, &["launch", "01-a/u/S1"]).status.success());
    assert!(!cm(root, &["launch", "01-a/u/S2"]).status.success());
    assert!(
        !cm(root, &["launch", "01-a/u/S2", "--no-parts"])
            .status
            .success()
    );
    assert!(
        cm(
            root,
            &["launch", "01-a/u/S2", "--no-parts", "alternatives only"]
        )
        .status
        .success()
    );
    let journal = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    assert!(
        journal
            .lines()
            .any(|line| line == "- no-parts: alternatives only")
    );
}
