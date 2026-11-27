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
    let unit = root.join("01-first/alpha");
    let state_paths = [cm::store::state_path(&unit), unit.join("STATE.md")];
    let snapshot = || state_paths.each_ref().map(|path| fs::read(path).unwrap());
    let before = snapshot();
    let journal_path = root.join("JOURNAL.md");
    let journal_before = fs::read_to_string(&journal_path).unwrap();

    let added = cm(
        root,
        &["--json", "mac-lane", "add", "Mac work", "--step", step],
    );
    assert!(added.status.success(), "{added:?}");
    assert_eq!(json(&added)["item"], 2);
    assert_eq!(snapshot(), before);
    assert!(
        fs::read_to_string(root.join("MAC-LANE.md"))
            .unwrap()
            .ends_with(&format!(
                "1. **Existing request**\n2. **Mac work**\n\tStep: `{step}`\n"
            ))
    );
    let step_state = || cm::store::load_unit(&unit).unwrap().steps["mac-work"].clone();
    let journal = fs::read_to_string(journal_path).unwrap();
    assert!(
        journal
            .strip_prefix(&journal_before)
            .unwrap()
            .ends_with(&format!(
                "- MAC-LANE.md: `2. **Mac work**`\n- step: `{step}`\n"
            ))
    );

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
    assert_eq!(snapshot(), before);
    let listed = cm(root, &["--json", "mac-lane", "list"]);
    assert_eq!(json(&listed)["items"].as_array().unwrap().len(), 1);
    let receipt = String::from_utf8(done.stdout).unwrap();
    fs::write(root.join("receipt.md"), receipt).unwrap();
    let advance = cm(root, &["advance", step, "done", "--evidence", "receipt.md"]);
    assert!(advance.status.success(), "{advance:?}");
    assert_eq!(step_state().track.state, cm::state::Phase::Done);
}

#[test]
fn mac_lane_work_is_omitted_from_dispatch_and_refuses_direct_launch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mac = "01-first/alpha/mac-work";
    let codex = "01-first/alpha/codex-work";
    assert!(cm(root, &["init"]).status.success());
    fs::write(root.join("kickoff.md"), "Go.\n").unwrap();
    for (step, lane) in [(mac, "mac"), (codex, "codex")] {
        let declared = cm(
            root,
            &["init", step, "--lane", lane, "--kickoff-file", "kickoff.md"],
        );
        assert!(declared.status.success(), "{declared:?}");
    }
    let added = cm(root, &["mac-lane", "add", "Mac work", "--step", mac]);
    assert!(added.status.success(), "{added:?}");

    for args in [
        &["--json", "next"][..],
        &["--json", "next", "--lane", "codex"][..],
    ] {
        let next = cm(root, args);
        assert!(next.status.success(), "{next:?}");
        let report = json(&next);
        let steps = report["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1, "{report}");
        assert_eq!(steps[0]["target"], codex);
        assert_eq!(report["waiting"], serde_json::json!([]));
    }

    let state = root.join("01-first/alpha/.cm/state.json");
    let before = fs::read(&state).unwrap();
    for command in ["next", "launch"] {
        let output = cm(root, &["--json", command, mac]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(json(&output)["violations"][0]["rule"], "mac-lane-dispatch");
    }
    assert_eq!(fs::read(state).unwrap(), before);
}

#[test]
fn mac_lane_add_refuses_an_undeclared_step_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    assert!(cm(root, &["init", "01-first/alpha"]).status.success());
    let journal = root.join("JOURNAL.md");
    let target = "01-first/alpha/undeclared";
    let assert_refused = || {
        let refused = cm(
            root,
            &["--json", "mac-lane", "add", "Mac work", "--step", target],
        );
        assert_eq!(refused.status.code(), Some(1), "{refused:?}");
        let violation = &json(&refused)["violations"][0];
        assert_eq!(violation["rule"], "step-unknown");
        assert_eq!(violation["path"], target);
    };

    let before = fs::read(&journal).unwrap();
    assert_refused();
    assert!(!root.join("MAC-LANE.md").exists());
    assert_eq!(fs::read(&journal).unwrap(), before);

    let unit_dir = root.join("01-first/alpha");
    let mut unit = cm::store::load_unit(&unit_dir).unwrap();
    let mut step = cm::state::Step::new(&unit.track.since);
    step.track.state = cm::state::Phase::Launched;
    step.launches = 1;
    unit.steps.insert("undeclared".to_owned(), step);
    cm::commands::write_unit(&unit_dir, &unit).unwrap();
    assert!(
        cm(root, &["mac-lane", "add", "Existing request"])
            .status
            .success()
    );
    let paths = [
        root.join("MAC-LANE.md"),
        cm::store::state_path(&unit_dir),
        cm::store::state_path(root),
        journal,
    ];
    let snapshot = || paths.each_ref().map(|path| fs::read(path).unwrap());
    let before = snapshot();
    assert_refused();
    assert_eq!(snapshot(), before);
}

#[test]
fn init_help_advertises_all_supported_lanes() {
    let temp = tempfile::tempdir().unwrap();
    let output = cm(temp.path(), &["init", "--help"]);
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    let lane_help = help
        .lines()
        .find(|line| line.contains("--lane <LANE>"))
        .unwrap();
    let lanes = cm::state::LANES.map(cm::state::Lane::name).join(", ");
    assert!(lane_help.contains(&lanes), "{lane_help}");
}

#[test]
fn mac_lane_rejects_rollouts_on_init_and_amend() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let step = "01-first/alpha/rollout";
    assert!(cm(root, &["init"]).status.success());
    let refused = cm(
        root,
        &["--json", "init", step, "--lane", "mac", "--kind", "rollout"],
    );
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert_eq!(json(&refused)["violations"][0]["rule"], "mac-lane-rollout");
    assert!(!root.join(step).exists());

    let declared = cm(
        root,
        &["init", step, "--lane", "codex", "--kind", "rollout"],
    );
    assert!(declared.status.success(), "{declared:?}");
    let state = cm::store::state_path(&root.join("01-first/alpha"));
    let before = fs::read(&state).unwrap();
    let refused = cm(root, &["--json", "amend", step, "--lane", "mac"]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert_eq!(json(&refused)["violations"][0]["rule"], "mac-lane-rollout");
    assert_eq!(fs::read(state).unwrap(), before);
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
    let outside = cm(root, &["--json", "journal", "x"]);
    assert_eq!(outside.status.code(), Some(1));
    assert_eq!(json(&outside)["ok"], false);
    assert_eq!(json(&outside)["violations"][0]["rule"], "io");
    assert!(cm(root, &["init"]).status.success());
    for (args, rule) in [
        (
            &["--json", "launch", "not-a-target", "--worktree", "."][..],
            "step-unknown",
        ),
        (
            &["--json", "advance", "01-a/u", "open"][..],
            "advance-refused",
        ),
    ] {
        let refused = cm(root, args);
        assert_eq!(refused.status.code(), Some(1), "{refused:?}");
        assert_eq!(json(&refused)["violations"][0]["rule"], rule);
    }
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
        "# Owner queue\n\n## 1. Pick one\n\nWhich path?\n\nRecommendation: the first\n\nAnswer:\n";
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
        "item `1. Pick one` has no line starting `Context:`"
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

const STEP: &str = "09-daemon/chart-termination/build";

fn files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                found.push((name, fs::read(&path).unwrap()));
            }
        }
    }
    found.sort();
    found
}

fn chart_campaign() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    assert!(cm(temp.path(), &["init"]).status.success());
    for step in [STEP, "09-daemon/chart-termination/probe"] {
        let declared = cm(temp.path(), &["init", step, "--lane", "claude-cloud"]);
        assert!(declared.status.success(), "{declared:?}");
    }
    temp
}

fn argvs(report: &Value) -> Vec<Value> {
    report["violations"][0]["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|each| each["argv"].clone())
        .collect()
}

fn add_block() -> Value {
    serde_json::json!([
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
        STEP
    ])
}

#[test]
fn a_block_on_no_owner_queue_item_names_the_add_and_each_open_item_and_writes_nothing() {
    let temp = chart_campaign();
    let root = temp.path();
    let queue = root.join("OWNER-QUEUE.md");
    let mut text = fs::read_to_string(&queue).unwrap();
    text.push_str("\n## Capability cuts: confirm M01 cut, P02 kept\n\nContext: x\nQuestion?\nRecommendation: y\nAnswer: confirmed\n");
    fs::write(&queue, text).unwrap();
    let incident = "blocked:until:owner-queue:chart-termination-secret-leak";
    for state in [incident, "blocked:until:owner-queue:0"] {
        let before = files(root);
        let refused = cm(root, &["--json", "advance", STEP, state]);
        assert_eq!(refused.status.code(), Some(1), "{refused:?}");
        let report = json(&refused);
        assert_eq!(report["violations"][0]["rule"], "block-condition");
        assert_eq!(report["violations"][0]["path"], STEP);
        assert_eq!(argvs(&report), [add_block()]);
        assert_eq!(files(root), before, "{state}");
    }
    let text = cm(root, &["advance", STEP, incident]);
    assert_eq!(text.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(text.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        format!(
            "{STEP}: block-condition: `owner-queue:chart-termination-secret-leak` is not an item number, and OWNER-QUEUE.md has no open numbered item"
        )
    );

    for title in ["Chart termination secret leak", "Probe budget"] {
        let added = cm(
            root,
            &[
                "owner-queue",
                "add",
                title,
                "--context",
                "c",
                "--question",
                "q?",
                "--recommendation",
                "r",
            ],
        );
        assert!(added.status.success(), "{added:?}");
    }
    let before = files(root);
    let refused = cm(root, &["--json", "advance", STEP, incident]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let report = json(&refused);
    assert_eq!(report["violations"][0]["rule"], "block-condition");
    assert_eq!(
        argvs(&report),
        [
            add_block(),
            serde_json::json!(["cm", "advance", STEP, "blocked:until:owner-queue:1"]),
            serde_json::json!(["cm", "advance", STEP, "blocked:until:owner-queue:2"]),
        ]
    );
    assert_eq!(
        report["violations"][0]["next"][1]["when"],
        "when the step waits on item 1, `Chart termination secret leak`"
    );
    assert_eq!(files(root), before);
}

#[test]
fn a_typod_step_id_names_the_nearest_declared_step_and_writes_nothing() {
    let temp = chart_campaign();
    let root = temp.path();
    let before = files(root);
    for typo in [
        "09-daemon/chart-termination/biuld",
        "9-daemon/chart-termination/build",
        "09-daemon/chart-terminaton/build",
    ] {
        let refused = cm(root, &["--json", "advance", typo, "blocked:waiting"]);
        assert_eq!(refused.status.code(), Some(1), "{typo}: {refused:?}");
        let report = json(&refused);
        let violation = &report["violations"][0];
        assert_eq!(violation["path"], typo);
        assert!(
            ["step-unknown", "unit-missing"].contains(&violation["rule"].as_str().unwrap()),
            "{report}"
        );
        assert!(
            violation["message"]
                .as_str()
                .unwrap()
                .ends_with(&format!("the nearest declared ids are {STEP}")),
            "{report}"
        );
        assert_eq!(
            argvs(&report)[0],
            serde_json::json!(["cm", "advance", STEP, "blocked:waiting"])
        );
        assert_eq!(
            violation["next"][0]["when"],
            format!("when you meant `{STEP}`")
        );
    }
    assert_eq!(files(root), before);
    let undeclared = json(&cm(root, &["--json", "launch", "01-x/y/z"]));
    let rules: Vec<&Value> = undeclared["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|violation| &violation["rule"])
        .collect();
    assert!(
        !rules.contains(&&Value::from("step-unknown")),
        "{undeclared}"
    );
}

#[test]
fn a_usage_error_is_one_object_of_rule_usage_naming_the_verbs_usage_line_and_exits_2() {
    let temp = chart_campaign();
    let root = temp.path();
    for args in [
        &["--json", "advance", "--bogus"][..],
        &["advance", "--bogus", "--json"][..],
    ] {
        let usage = cm(root, args);
        assert_eq!(usage.status.code(), Some(2), "{usage:?}");
        assert!(usage.stderr.is_empty(), "{usage:?}");
        let report: Value = serde_json::from_slice(&usage.stdout).unwrap();
        assert_eq!(report["command"], "advance");
        assert_eq!(report["ok"], false);
        let violation = &report["violations"][0];
        assert_eq!(violation["rule"], "usage");
        assert_eq!(violation["path"], "cm advance");
        assert!(
            violation["message"]
                .as_str()
                .unwrap()
                .starts_with("unexpected argument '--bogus' found"),
            "{report}"
        );
        assert_eq!(
            argvs(&report),
            [serde_json::json!([
                "cm",
                "advance",
                "[OPTIONS]",
                "<TARGET>",
                "<STATE>"
            ])]
        );
    }
    let text = cm(root, &["advance", "--bogus"]);
    assert_eq!(text.status.code(), Some(2));
    assert!(text.stdout.is_empty());
    let stderr = String::from_utf8(text.stderr).unwrap();
    assert!(
        stderr.starts_with("cm advance: usage: unexpected argument '--bogus' found"),
        "{stderr}"
    );
    assert!(
        stderr.contains("\n  next: cm advance '[OPTIONS]' '<TARGET>' '<STATE>'"),
        "{stderr}"
    );

    let strike = json(&cm(root, &["--json", "owner-queue", "strike", "7"]));
    assert_eq!(strike["violations"][0]["path"], "cm owner-queue strike");
    assert_eq!(
        argvs(&strike),
        [serde_json::json!([
            "cm",
            "owner-queue",
            "strike",
            "[OPTIONS]",
            "--reason",
            "<REASON>",
            "<NUMBER>"
        ])]
    );
    let bare = cm(root, &["--json", "idea"]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(
        json(&bare)["violations"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("cm idea takes a subcommand: admit, move, outcome"),
        "{bare:?}"
    );
    let unknown = cm(root, &["--json", "frobnicate"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert_eq!(json(&unknown)["violations"][0]["path"], "cm");
    let sha = cm(
        root,
        &[
            "--json",
            "review",
            "wait",
            STEP,
            "--head",
            "zz",
            "--reviewer",
            "a",
            "--deadline",
            "2026-01-01T00:00Z",
        ],
    );
    assert_eq!(sha.status.code(), Some(2));
    assert_eq!(json(&sha)["violations"][0]["rule"], "usage");

    for args in [
        &["--help"][..],
        &["--json", "advance", "--help"][..],
        &["--version"][..],
    ] {
        let shown = cm(root, args);
        assert_eq!(shown.status.code(), Some(0), "{args:?}");
        assert!(
            serde_json::from_slice::<Value>(&shown.stdout).is_err(),
            "{args:?}"
        );
    }
}

#[test]
fn a_bad_state_word_names_the_legal_next_state_as_a_full_command() {
    let temp = chart_campaign();
    let root = temp.path();
    let before = files(root);
    let drafted = json(&cm(root, &["--json", "advance", STEP, "open"]));
    assert_eq!(drafted["violations"][0]["rule"], "advance-refused");
    assert_eq!(argvs(&drafted), [serde_json::json!(["cm", "launch", STEP])]);
    assert_eq!(files(root), before);

    let unit = root.join("09-daemon/chart-termination");
    let mut state = cm::store::load_unit(&unit).unwrap();
    state.steps.get_mut("build").unwrap().track.state = cm::state::Phase::Launched;
    cm::commands::write_unit(&unit, &state).unwrap();
    let launched = json(&cm(
        root,
        &[
            "--json",
            "advance",
            STEP,
            "in-review",
            "--pr",
            "https://github.com/o/r/pull/1",
        ],
    ));
    assert_eq!(
        argvs(&launched),
        [serde_json::json!([
            "cm",
            "advance",
            STEP,
            "pr-open",
            "--pr",
            "https://github.com/o/r/pull/1"
        ])]
    );

    let reason = json(&cm(root, &["--json", "advance", STEP, "blocked:on CI"]));
    assert_eq!(reason["violations"][0]["rule"], "block-condition");
    assert_eq!(
        argvs(&reason)[0],
        serde_json::json!(["cm", "advance", STEP, "blocked:on-CI"])
    );
}

#[test]
fn a_bad_lane_gate_or_epoch_move_names_the_values_the_campaign_holds() {
    let temp = chart_campaign();
    let root = temp.path();
    let before = files(root);
    let lane = json(&cm(
        root,
        &[
            "--json",
            "init",
            "09-daemon/chart-termination/x",
            "--lane",
            "claud-cloud",
        ],
    ));
    assert_eq!(lane["violations"][0]["rule"], "lane-unknown");
    assert_eq!(
        argvs(&lane),
        [
            serde_json::json!([
                "cm",
                "init",
                "09-daemon/chart-termination/x",
                "--lane",
                "claude-cloud"
            ]),
            serde_json::json!([
                "cm",
                "init",
                "09-daemon/chart-termination/x",
                "--lane",
                "<lane>"
            ]),
        ]
    );
    let gate = json(&cm(
        root,
        &[
            "--json",
            "init",
            "09-daemon/chart-termination/x",
            "--lane",
            "codex",
            "--after",
            "pr-open:9-daemon/chart-termination/probe",
        ],
    ));
    assert_eq!(gate["violations"][0]["rule"], "after-parse", "{gate}");
    assert_eq!(
        argvs(&gate)[0],
        serde_json::json!([
            "cm",
            "init",
            "09-daemon/chart-termination/x",
            "--lane",
            "codex",
            "--after",
            "pr-open:09-daemon/chart-termination/probe"
        ])
    );
    assert_eq!(
        argvs(&gate).last().unwrap(),
        &serde_json::json!([
            "cm",
            "init",
            "09-daemon/chart-termination/x",
            "--lane",
            "codex"
        ])
    );
    fs::write(
        root.join("EPOCH.md"),
        "---\nstatus: PLANNING\nskill_revision: abc\n---\n",
    )
    .unwrap();
    let before_epoch = files(root);
    let epoch = json(&cm(root, &["--json", "epoch", "work"]));
    assert_eq!(epoch["violations"][0]["rule"], "epoch-status");
    assert_eq!(
        argvs(&epoch),
        [serde_json::json!(["cm", "epoch", "working"])]
    );
    assert_eq!(files(root), before_epoch);
    fs::remove_file(root.join("EPOCH.md")).unwrap();
    assert_eq!(files(root), before);
}

#[test]
fn an_unreadable_campaign_state_names_the_git_checkout_that_restores_it() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    fs::create_dir(&root).unwrap();
    assert!(cm(&root, &["init"]).status.success());
    fs::write(root.join(".cm/state.json"), "not JSON").unwrap();
    let broken = cm(&root, &["--json", "journal", "x"]);
    assert_eq!(broken.status.code(), Some(1), "{broken:?}");
    let report = json(&broken);
    assert_eq!(report["violations"][0]["rule"], "io");
    assert_eq!(report["violations"][0]["path"], ".cm/state.json");
    assert_eq!(
        argvs(&report),
        [serde_json::json!([
            "git",
            "checkout",
            "HEAD",
            "--",
            ".cm/state.json"
        ])]
    );
    assert!(
        report["error"].as_str().unwrap().contains("parsing"),
        "{report}"
    );

    let elsewhere = cm(
        temp.path(),
        &["--json", "-C", root.to_str().unwrap(), "journal", "x"],
    );
    let dir = cm::store::absolute(&root);
    assert_eq!(
        argvs(&json(&elsewhere)),
        [serde_json::json!([
            "git",
            "-C",
            dir,
            "checkout",
            "HEAD",
            "--",
            ".cm/state.json"
        ])]
    );
}

#[test]
fn a_lock_cm_cannot_take_names_the_same_command_again() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    let lock = root.join(".cm/lock");
    let _ = fs::remove_file(&lock);
    fs::create_dir(&lock).unwrap();
    let refused = cm(root, &["--json", "journal", "a title"]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let report = json(&refused);
    assert_eq!(report["violations"][0]["rule"], "io");
    assert_eq!(report["violations"][0]["path"], ".cm/lock");
    assert_eq!(
        argvs(&report),
        [serde_json::json!(["cm", "journal", "a title"])]
    );
}

#[test]
fn outside_any_campaign_the_move_names_the_live_epoch_in_campaign_trail_or_cm_init() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("campaign-trail");
    for (epoch, status) in [("a", "WORKING"), ("b", "CLOSED")] {
        let dir = checkout.join("store/epochs").join(epoch);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("EPOCH.md"),
            format!("---\nstatus: {status}\n---\n"),
        )
        .unwrap();
    }
    fs::create_dir(checkout.join("tools")).unwrap();
    for (dir, epoch) in [
        (checkout.clone(), "store/epochs/a"),
        (checkout.join("tools"), "../store/epochs/a"),
    ] {
        let refused = cm(&dir, &["--json", "journal", "a title"]);
        assert_eq!(refused.status.code(), Some(1), "{refused:?}");
        let report = json(&refused);
        assert_eq!(report["violations"][0]["rule"], "io");
        assert_eq!(
            argvs(&report),
            [serde_json::json!(["cm", "-C", epoch, "journal", "a title"])]
        );
    }
    let plain = temp.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let report = json(&cm(&plain, &["--json", "journal", "a title"]));
    assert_eq!(argvs(&report), [serde_json::json!(["cm", "init", "."])]);
}

#[test]
fn next_act_keeps_exit_0_with_nothing_to_act_on_and_1_with_an_action() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    let idle = cm(root, &["--json", "next", "--act"]);
    assert_eq!(idle.status.code(), Some(0), "{idle:?}");
    assert_eq!(json(&idle)["ok"], true);
    fs::write(root.join("kickoff.md"), "Go.\n").unwrap();
    let declared = cm(
        root,
        &[
            "init",
            "01-a/u/s",
            "--lane",
            "codex",
            "--kickoff-file",
            "kickoff.md",
        ],
    );
    assert!(declared.status.success(), "{declared:?}");
    let act = cm(root, &["--json", "next", "--act"]);
    assert_eq!(act.status.code(), Some(1), "{act:?}");
    assert_eq!(json(&act)["ok"], true);
}
