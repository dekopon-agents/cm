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
