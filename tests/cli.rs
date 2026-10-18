use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn cm(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn init_then_lint_is_clean_and_two_hand_edits_are_both_listed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
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
