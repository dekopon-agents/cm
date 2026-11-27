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
        let path = match violation["rule"].as_str().unwrap() {
            "unit-missing" => typo.rsplit_once('/').unwrap().0,
            "step-unknown" => typo,
            other => panic!("{other}: {report}"),
        };
        assert_eq!(violation["path"], path);
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
            "https://github.com/o/r/pull/1",
            "--sha",
            "<revision>",
            "--repo",
            "<checkout>"
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
        [serde_json::json!([
            "cm",
            "epoch",
            "working",
            "--plan-revision",
            "<revision>"
        ])]
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

fn set_state(root: &Path, step: &str, edit: impl FnOnce(&mut cm::state::Step)) {
    let unit = root.join("09-daemon/chart-termination");
    let mut state = cm::store::load_unit(&unit).unwrap();
    edit(state.steps.get_mut(step).unwrap());
    cm::commands::write_unit(&unit, &state).unwrap();
}

fn violation<'a>(report: &'a Value, rule: &str) -> &'a Value {
    report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["rule"] == rule)
        .unwrap_or_else(|| panic!("no {rule} in {report}"))
}

fn run_argv(root: &Path, argv: &Value) -> Output {
    let words: Vec<&str> = argv.as_array().unwrap()[1..]
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    cm(root, &words)
}

#[test]
fn a_repair_edits_the_argument_it_names_whatever_text_the_other_arguments_hold() {
    let temp = chart_campaign();
    let root = temp.path();
    fs::write(
        root.join("EPOCH.md"),
        "---\nstatus: PLANNING\nskill_revision: abc\napproved_plan_revision: null\n---\n",
    )
    .unwrap();
    let epoch = cm(root, &["--json", "epoch", "epoch"]);
    assert_eq!(epoch.status.code(), Some(1), "{epoch:?}");
    assert_eq!(
        argvs(&json(&epoch)),
        [serde_json::json!([
            "cm",
            "epoch",
            "working",
            "--plan-revision",
            "<revision>"
        ])]
    );
    set_state(root, "build", |step| {
        step.track.state = cm::state::Phase::Launched
    });
    let advance = json(&cm(root, &["--json", "advance", STEP, "advance"]));
    assert_eq!(
        argvs(&advance),
        [serde_json::json!([
            "cm",
            "advance",
            STEP,
            "pr-open",
            "--pr",
            "<pr>",
            "--sha",
            "<revision>",
            "--repo",
            "<checkout>"
        ])]
    );
    let lane = json(&cm(root, &["--json", "next", "--lane", "next"]));
    assert_eq!(
        argvs(&lane),
        [serde_json::json!(["cm", "next", "--lane", "<lane>"])]
    );
    let bad = "9-daemon/chart-termination/probe";
    let commit = "a".repeat(40);
    let red = json(&cm(
        root,
        &[
            "--json",
            "known-red",
            STEP,
            "--commit",
            &commit,
            "--check",
            "ci",
            "--reason",
            bad,
            "--owner",
            "me",
            "--until",
            bad,
        ],
    ));
    assert_eq!(
        violation(&red, "after-parse")["next"][0]["argv"],
        serde_json::json!([
            "cm",
            "known-red",
            STEP,
            "--commit",
            commit,
            "--check",
            "ci",
            "--reason",
            bad,
            "--owner",
            "me",
            "--until",
            "09-daemon/chart-termination/probe"
        ])
    );
    let typo = "9-daemon/chart-termination/probe";
    let gates = json(&cm(
        root,
        &[
            "--json",
            "init",
            "09-daemon/chart-termination/x",
            "--lane",
            "codex",
            "--after",
            typo,
            "--after=9-daemon/chart-termination/build",
        ],
    ));
    let parsed: Vec<&Value> = gates["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|violation| violation["rule"] == "after-parse")
        .collect();
    assert_eq!(
        parsed[0]["next"][0]["argv"],
        serde_json::json!([
            "cm",
            "init",
            "09-daemon/chart-termination/x",
            "--lane",
            "codex",
            "--after",
            "09-daemon/chart-termination/probe",
            "--after=9-daemon/chart-termination/build"
        ])
    );
    assert_eq!(
        parsed[1]["next"][0]["argv"],
        serde_json::json!([
            "cm",
            "init",
            "09-daemon/chart-termination/x",
            "--lane",
            "codex",
            "--after",
            typo,
            "--after=09-daemon/chart-termination/build"
        ])
    );
    let slash = json(&cm(
        root,
        &[
            "--json",
            "advance",
            "09-daemon/chart-termination/biuld/",
            "blocked:x",
        ],
    ));
    assert_eq!(
        argvs(&slash)[0],
        serde_json::json!(["cm", "advance", STEP, "blocked:x"])
    );
    let block = json(&cm(
        root,
        &[
            "--json",
            "owner-queue",
            "add",
            typo,
            "--context",
            "c",
            "--question",
            "q?",
            "--recommendation",
            "r",
            "--block",
            "9-daemon/chart-termination/build",
        ],
    ));
    assert_eq!(
        argvs(&block)[0],
        serde_json::json!([
            "cm",
            "owner-queue",
            "add",
            typo,
            "--context",
            "c",
            "--question",
            "q?",
            "--recommendation",
            "r",
            "--block",
            STEP
        ])
    );
}

#[test]
fn a_path_that_begins_with_a_dash_is_named_where_clap_reads_it_as_a_value() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let missing = json(&cm(
        root,
        &["--json", "--root=-new-campaign", "journal", "x"],
    ));
    let init = serde_json::json!(["cm", "init", "--", "-new-campaign"]);
    assert_eq!(argvs(&missing), std::slice::from_ref(&init));
    let made = run_argv(root, &init);
    assert!(made.status.success(), "{made:?}");
    assert!(cm::store::is_campaign(&root.join("-new-campaign")));

    let epoch = root.join("-trail/store/epochs/live");
    fs::create_dir_all(&epoch).unwrap();
    fs::write(epoch.join("EPOCH.md"), "---\nstatus: WORKING\n---\n").unwrap();
    let live = json(&cm(root, &["--json", "--root=-trail", "journal", "x"]));
    assert_eq!(
        argvs(&live),
        [serde_json::json!([
            "cm",
            "--root=-trail/store/epochs/live",
            "journal",
            "x"
        ])]
    );
}

#[test]
fn a_gate_repair_never_keeps_the_part_that_was_refused() {
    let temp = chart_campaign();
    let root = temp.path();
    let declare = [
        "--json",
        "init",
        "09-daemon/chart-termination/x",
        "--lane",
        "codex",
    ];
    for gate in ["pin:build@oops", "pin:build"] {
        let mut args = declare.to_vec();
        args.extend(["--after", gate]);
        let refused = json(&cm(root, &args));
        assert_eq!(
            argvs(&refused),
            [serde_json::json!([
                "cm",
                "init",
                "09-daemon/chart-termination/x",
                "--lane",
                "codex"
            ])],
            "{gate}"
        );
    }
    set_state(root, "build", |step| {
        step.track.evidence.sha = Some("abcdef1234567".into())
    });
    let mut args = declare.to_vec();
    args.extend(["--after", "pin:build@oops"]);
    let pinned = json(&cm(root, &args));
    let suggested = &argvs(&pinned)[0];
    assert_eq!(
        suggested.as_array().unwrap().last().unwrap(),
        "pin:09-daemon/chart-termination/build@abcdef1234567"
    );
    let ran = run_argv(root, suggested);
    assert!(ran.status.success(), "{ran:?}");

    let dropped = json(&cm(
        root,
        &[
            "--json",
            "amend",
            "09-daemon/chart-termination/x",
            "--drop-after",
            "pin:biuld@zz",
        ],
    ));
    let suggested = &argvs(&dropped)[0];
    assert_eq!(
        suggested,
        &serde_json::json!([
            "cm",
            "amend",
            "09-daemon/chart-termination/x",
            "--drop-after",
            "pin:09-daemon/chart-termination/build@abcdef1234567"
        ])
    );
    let ran = run_argv(root, suggested);
    assert!(ran.status.success(), "{ran:?}");

    let block = json(&cm(
        root,
        &["--json", "advance", STEP, "blocked:until:pin:probe@zz"],
    ));
    assert_eq!(
        argvs(&block)[0],
        serde_json::json!([
            "cm",
            "advance",
            STEP,
            "blocked:until:09-daemon/chart-termination/probe"
        ])
    );
}

#[test]
fn a_suggested_next_state_is_one_the_advance_then_accepts() {
    let temp = chart_campaign();
    let root = temp.path();
    let mac = "09-daemon/chart-termination/mac";
    let local = "09-daemon/chart-termination/local";
    for (step, lane) in [(mac, "mac"), (local, "pi")] {
        assert!(cm(root, &["init", step, "--lane", lane]).status.success());
    }
    fs::write(root.join("ev.md"), "Done on the Mac.\n").unwrap();
    let bare = json(&cm(root, &["--json", "advance", mac, "dnoe"]));
    assert_eq!(
        argvs(&bare),
        [serde_json::json!([
            "cm",
            "advance",
            mac,
            "done",
            "--evidence",
            "<evidence-file>"
        ])]
    );
    let given = json(&cm(
        root,
        &["--json", "advance", mac, "dnoe", "--evidence", "ev.md"],
    ));
    let suggested = &argvs(&given)[0];
    assert_eq!(
        suggested,
        &serde_json::json!(["cm", "advance", mac, "done", "--evidence", "ev.md"])
    );
    let ran = run_argv(root, suggested);
    assert!(ran.status.success(), "{ran:?}");

    let launch = json(&cm(root, &["--json", "advance", local, "opn"]));
    assert_eq!(
        argvs(&launch),
        [serde_json::json!([
            "cm",
            "launch",
            local,
            "--worktree",
            "<checkout>"
        ])]
    );

    let by = cm(
        root,
        &[
            "--json",
            "advance",
            STEP,
            "supersede",
            "--by",
            "09-daemon/chart-termination/probe",
        ],
    );
    assert_eq!(by.status.code(), Some(1), "{by:?}");
    assert_eq!(
        argvs(&json(&by)),
        [serde_json::json!([
            "cm",
            "advance",
            STEP,
            "superseded",
            "--by",
            "09-daemon/chart-termination/probe",
            "--evidence",
            "<evidence-file>"
        ])]
    );

    set_state(root, "build", |step| {
        let mut part = step.track.clone();
        part.state = cm::state::Phase::Deployed;
        step.parts.insert("api".into(), part);
    });
    let part = json(&cm(
        root,
        &["--json", "advance", STEP, "dnoe", "--part", "api"],
    ));
    assert_eq!(
        part["violations"][0]["next"][0]["kind"], "settled",
        "{part}"
    );
    set_state(root, "build", |step| {
        step.track.state = cm::state::Phase::Done
    });
    for state in ["opn", "blocked:on CI", "blocked:until:owner-queue:x"] {
        let done = json(&cm(root, &["--json", "advance", STEP, state]));
        assert_eq!(
            done["violations"][0]["next"][0]["kind"], "settled",
            "{state}: {done}"
        );
    }

    fs::write(
        root.join("EPOCH.md"),
        "---\nstatus: WORKING\nskill_revision: abc\napproved_plan_revision: def\n---\n",
    )
    .unwrap();
    let closing = json(&cm(root, &["--json", "epoch", "clos"]));
    assert_eq!(argvs(&closing), [serde_json::json!(["cm", "close"])]);
}

#[test]
fn a_usage_error_names_the_subcommand_clap_was_reading_not_an_option_value() {
    let temp = chart_campaign();
    let usage = cm(
        temp.path(),
        &["--json", "review", "--model", "wait", "--bogus"],
    );
    assert_eq!(usage.status.code(), Some(2));
    let report = json(&usage);
    assert_eq!(report["violations"][0]["path"], "cm review");
    assert_eq!(argvs(&report)[0][1], "review");
    assert_ne!(argvs(&report)[0][2], "wait");
}

#[test]
fn every_value_error_moved_out_of_clap_exits_1_and_a_misplaced_by_stays_usage() {
    let temp = chart_campaign();
    let root = temp.path();
    let wait = cm(
        root,
        &[
            "--json",
            "review",
            "wait",
            "9-daemon/chart-termination/build",
            "--head",
            "abc1234",
            "--reviewer",
            "astra",
            "--deadline",
            "2026-01-01T00:00Z",
        ],
    );
    assert_eq!(wait.status.code(), Some(1), "{wait:?}");
    assert_eq!(json(&wait)["violations"][0]["rule"], "step-unknown");
    let state = cm(
        root,
        &["--json", "advance", STEP, "supersede", "--by", STEP],
    );
    assert_eq!(state.status.code(), Some(1), "{state:?}");
    let by = cm(root, &["--json", "advance", STEP, "pr-open", "--by", STEP]);
    assert_eq!(by.status.code(), Some(2), "{by:?}");
    assert_eq!(json(&by)["violations"][0]["rule"], "usage");
    assert_eq!(json(&by)["violations"][0]["path"], "cm advance");
}

#[test]
fn no_repair_offers_a_step_a_gate_or_block_on_itself_nor_reads_another_condition_as_a_step() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    assert!(
        cm(root, &["init", STEP, "--lane", "claude-cloud"])
            .status
            .success()
    );
    let before = files(root);
    for state in [
        "blocked:until:epoch:working",
        "blocked:until:ownerqueue:3",
        "blocked:until:pin:build@abc1234",
    ] {
        let refused = cm(root, &["--json", "advance", STEP, state]);
        assert_eq!(refused.status.code(), Some(1), "{state}: {refused:?}");
        let report = json(&refused);
        assert_eq!(
            report["violations"][0]["rule"], "block-condition",
            "{state}"
        );
        for argv in argvs(&report) {
            let words: Vec<&str> = argv
                .as_array()
                .unwrap()
                .iter()
                .map(|word| word.as_str().unwrap())
                .collect();
            assert!(
                !words
                    .iter()
                    .any(|word| word.starts_with("blocked:until:") && word.ends_with("/build")),
                "{state}: {words:?}"
            );
        }
    }
    let amend = json(&cm(
        root,
        &["--json", "amend", STEP, "--after", "epoch:WORKING"],
    ));
    assert_eq!(
        violation(&amend, "after-parse")["next"][0]["argv"],
        serde_json::json!(["cm", "next"])
    );
    let probe = "09-daemon/chart-termination/probe";
    let pinned = json(&cm(
        root,
        &[
            "--json",
            "init",
            probe,
            "--lane",
            "codex",
            "--after",
            "pin:probe",
        ],
    ));
    assert_eq!(
        argvs(&pinned),
        [serde_json::json!(["cm", "init", probe, "--lane", "codex"])]
    );
    assert_eq!(files(root), before);
}

#[test]
fn a_block_on_an_undeclared_step_a_release_or_the_step_itself_is_written_as_at_base() {
    let unit = "09-daemon/chart-termination";
    for (state, written) in [
        ("blocked:until:prbe", format!("blocked:until:{unit}/prbe")),
        (
            "blocked:until:pr-open:prbe",
            format!("blocked:until:pr-open:{unit}/prbe"),
        ),
        (
            "blocked:until:owner-queue",
            format!("blocked:until:{unit}/owner-queue"),
        ),
        (
            "blocked:until:09-daemon/chart-termination/astra-r2-collect-at-abc1234",
            format!("blocked:until:{unit}/astra-r2-collect-at-abc1234"),
        ),
        (
            "blocked:until:10-state/durable-deploy/mac-lane-39-done",
            "blocked:until:10-state/durable-deploy/mac-lane-39-done".to_owned(),
        ),
        (
            "blocked:until:tag:dekopon@0.100.0",
            "blocked:until:tag:dekopon@0.100.0".to_owned(),
        ),
        (
            "blocked:until:deployed:dekopon@0.100.0",
            "blocked:until:deployed:dekopon@0.100.0".to_owned(),
        ),
        ("blocked:until:build", format!("blocked:until:{unit}/build")),
        (
            "blocked:until:pr-open:build",
            format!("blocked:until:pr-open:{unit}/build"),
        ),
        (
            "blocked:until:09-daemon/chart-termination/build",
            format!("blocked:until:{unit}/build"),
        ),
    ] {
        let temp = chart_campaign();
        let root = temp.path();
        let out = cm(root, &["--json", "advance", STEP, state]);
        assert_eq!(out.status.code(), Some(0), "{state}: {out:?}");
        let state_now = cm::store::load_unit(&root.join(unit)).unwrap().steps["build"]
            .track
            .state
            .to_string();
        assert_eq!(state_now, written, "{state}");
    }
    let release = "00-release/dekopon/0.100.0";
    for state in [
        "blocked:until:tag:dekopon@0.100.0",
        "blocked:until:deployed:dekopon@0.100.0",
    ] {
        let temp = chart_campaign();
        let root = temp.path();
        assert!(
            cm(root, &["init", release, "--lane", "claude-cloud"])
                .status
                .success()
        );
        let out = cm(root, &["--json", "advance", release, state]);
        assert_eq!(out.status.code(), Some(0), "{state}: {out:?}");
        let state_now = cm::store::load_unit(&root.join("00-release/dekopon"))
            .unwrap()
            .steps["0.100.0"]
            .track
            .state
            .to_string();
        assert_eq!(state_now, state);
    }
}

#[test]
fn amend_and_unblock_name_the_nearest_declared_step_and_keep_the_refused_path() {
    let temp = chart_campaign();
    let root = temp.path();
    let typo = "09-daemon/chart-termination/biuld";
    let amend = json(&cm(root, &["--json", "amend", typo, "--lane", "codex"]));
    assert_eq!(amend["violations"][0]["rule"], "step-unknown");
    assert_eq!(
        argvs(&amend)[0],
        serde_json::json!(["cm", "amend", STEP, "--lane", "codex"])
    );
    let unblock = json(&cm(root, &["--json", "unblock", typo]));
    assert_eq!(
        argvs(&unblock)[0],
        serde_json::json!(["cm", "unblock", STEP])
    );
    let missing = json(&cm(
        root,
        &[
            "--json",
            "advance",
            "09-daemon/chart-terminaton/build",
            "blocked:waiting",
        ],
    ));
    assert_eq!(missing["violations"][0]["rule"], "unit-missing");
    assert_eq!(
        missing["violations"][0]["path"],
        "09-daemon/chart-terminaton"
    );
    set_state(root, "build", |step| {
        let part = step.track.clone();
        step.parts.insert("api".into(), part);
    });
    let part = json(&cm(
        root,
        &["--json", "advance", typo, "pr-open", "--part", "api"],
    ));
    assert_eq!(part["violations"][0]["path"], format!("{typo}#api"));
}

#[test]
fn a_value_option_right_before_json_prints_one_usage_object() {
    let temp = chart_campaign();
    let root = temp.path();
    for argv in [
        vec!["advance", STEP, "pr-open", "--part", "--json"],
        vec!["advance", STEP, "pr-open", "--sha", "--json"],
        vec!["advance", STEP, "merged", "--evidence", "--json"],
        vec!["owner-queue", "strike", "3", "--reason", "--json"],
        vec!["journal", "t", "--body-file", "--json"],
        vec!["init", "09-daemon/chart-termination/x", "--lane", "--json"],
        vec!["-C", "--json", "next"],
    ] {
        let out = cm(root, &argv);
        assert_eq!(out.status.code(), Some(2), "{argv:?}: {out:?}");
        let text = String::from_utf8(out.stdout.clone()).unwrap();
        assert_eq!(text.trim().lines().count(), 1, "{argv:?}: {text}");
        let report = json(&out);
        assert_eq!(report["violations"][0]["rule"], "usage", "{argv:?}");
        let verb = argv.iter().find(|word| !word.starts_with('-')).unwrap();
        let path = report["violations"][0]["path"].as_str().unwrap();
        assert!(
            argv[0] == "-C" || path.starts_with(&format!("cm {verb}")),
            "{argv:?}: {path}"
        );
    }
}

#[test]
fn each_suggested_state_names_the_evidence_its_advance_requires() {
    use cm::state::Phase;
    let temp = chart_campaign();
    let root = temp.path();
    for (step, kind) in [("roll", &["--kind", "rollout"][..]), ("cx", &[])] {
        let full = format!("09-daemon/chart-termination/{step}");
        let mut argv = vec!["init", &full, "--lane", "codex"];
        argv.extend(kind);
        let declared = cm(root, &argv);
        assert!(declared.status.success(), "{declared:?}");
    }
    fs::write(root.join("ev.md"), "Synced.\n").unwrap();
    let repo = root.join("repo");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    fs::create_dir(&repo).unwrap();
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "x"]);
    git(&["tag", "v1.0.0"]);
    let rev = git(&["rev-parse", "HEAD"]);
    let rev = rev.as_str();
    let checkout = repo.to_str().unwrap();
    let pr = "https://github.com/dekopon-agents/cm/pull/1";
    let samples = [
        ("<pr>", pr),
        ("<revision>", rev),
        ("<tag>", "v1.0.0"),
        (
            "<digest>",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ),
        ("<evidence-file>", "ev.md"),
        ("<checkout>", checkout),
    ];
    let at = |step: &str, phase: Phase, recorded: bool| {
        set_state(root, step, |each| {
            each.track.state = phase;
            each.track.evidence.pr = recorded.then(|| pr.to_owned());
            each.track.evidence.sha = recorded.then(|| rev.to_owned());
        })
    };
    type Case<'a> = (
        &'a str,
        Phase,
        bool,
        &'a [&'a str],
        &'a [&'a str],
        &'a [&'a str],
    );
    let pr_open: &[&str] = &[
        "pr-open",
        "--pr",
        "<pr>",
        "--sha",
        "<revision>",
        "--repo",
        "<checkout>",
    ];
    let merged: &[&str] = &["merged", "--sha", "<revision>", "--repo", "<checkout>"];
    let cases: [Case; 15] = [
        (
            "build",
            Phase::Launched,
            false,
            &["pr-opn"],
            pr_open,
            &["pr", "revision", "checkout"],
        ),
        (
            "build",
            Phase::Launched,
            false,
            &["mergd"],
            pr_open,
            &["pr", "revision", "checkout"],
        ),
        (
            "build",
            Phase::Launched,
            false,
            &["pr-opn", "--pr", pr],
            &[
                "pr-open",
                "--pr",
                pr,
                "--sha",
                "<revision>",
                "--repo",
                "<checkout>",
            ],
            &["revision", "checkout"],
        ),
        (
            "cx",
            Phase::Launched,
            false,
            &["pr-opn"],
            pr_open,
            &["pr", "revision", "checkout"],
        ),
        (
            "build",
            Phase::PrOpen,
            true,
            &["mergd"],
            merged,
            &["revision", "checkout"],
        ),
        (
            "cx",
            Phase::Launched,
            false,
            &["mergd", "--head", rev],
            pr_open,
            &["pr", "revision", "checkout"],
        ),
        (
            "cx",
            Phase::Launched,
            false,
            &["mergd", "--ci-verified", "ev.md"],
            pr_open,
            &["pr", "revision", "checkout"],
        ),
        (
            "cx",
            Phase::Launched,
            false,
            &["mergd", "--override", "ev.md"],
            pr_open,
            &["pr", "revision", "checkout"],
        ),
        (
            "cx",
            Phase::PrOpen,
            true,
            &["relased", "--head", rev],
            &[
                "merged",
                "--head",
                rev,
                "--sha",
                "<revision>",
                "--repo",
                "<checkout>",
            ],
            &["revision", "checkout"],
        ),
        (
            "build",
            Phase::PrOpen,
            true,
            &["relesed", "--tag", "v1.0.0"],
            merged,
            &["revision", "checkout"],
        ),
        (
            "cx",
            Phase::PrOpen,
            true,
            &["mergd"],
            merged,
            &["revision", "checkout"],
        ),
        (
            "build",
            Phase::Merged,
            true,
            &["relased"],
            &["released", "--tag", "<tag>"],
            &["tag"],
        ),
        (
            "build",
            Phase::Released,
            true,
            &["deplyed"],
            &["deployed", "--digest", "<digest>"],
            &["digest"],
        ),
        (
            "roll",
            Phase::Merged,
            true,
            &["deplyed"],
            &[
                "deployed",
                "--sha",
                rev,
                "--evidence",
                "<evidence-file>",
                "--repo",
                "<checkout>",
            ],
            &["evidence-file", "checkout"],
        ),
        (
            "roll",
            Phase::Launched,
            false,
            &["pr-opn"],
            pr_open,
            &["pr", "revision", "checkout"],
        ),
    ];
    for (name, phase, recorded, typed, suggested, inputs) in cases {
        let step = format!("09-daemon/chart-termination/{name}");
        at(name, phase, recorded);
        let mut argv = vec!["--json", "advance", &step];
        argv.extend(typed);
        let report = json(&cm(root, &argv));
        assert_eq!(
            report["violations"][0]["rule"], "advance-refused",
            "{typed:?}: {report}"
        );
        let cause = report["violations"][0]["message"].as_str().unwrap();
        for flag in ["--tag", "--head", "--ci-verified", "--override"] {
            if typed.contains(&flag) && !suggested.contains(&flag) {
                assert!(
                    cause.ends_with(&format!("takes no {flag}, so the move drops it")),
                    "{cause}"
                );
            }
        }
        if typed.iter().all(|word| !word.starts_with("--")) || suggested.contains(&"--head") {
            assert!(!cause.contains("takes no"), "{cause}");
        }
        let mut wanted = vec!["cm", "advance", &step];
        wanted.extend(suggested);
        assert_eq!(
            argvs(&report),
            [serde_json::json!(wanted)],
            "{name} {typed:?}: {report}"
        );
        assert_eq!(
            report["violations"][0]["next"][0]["inputs"],
            serde_json::json!(inputs),
            "{name} {typed:?}"
        );
        let filled: Vec<Value> = wanted
            .iter()
            .map(|word| {
                let sample = samples.iter().find(|(input, _)| input == word);
                Value::from(sample.map_or(*word, |(_, value)| *value))
            })
            .collect();
        let mut run = vec![Value::from("cm"), Value::from("--json")];
        run.extend(filled[1..].iter().cloned());
        let ran = json(&run_argv(root, &Value::Array(run)));
        for each in ran["violations"].as_array().into_iter().flatten() {
            assert!(
                !["evidence-missing", "evidence-repo", "evidence-unexpected"]
                    .contains(&each["rule"].as_str().unwrap()),
                "{name} {typed:?}: {ran}"
            );
        }
        // Only the merge gate, which reads GitHub, may refuse a filled move.
        if !suggested.contains(&"merged") && !suggested.contains(&"released") {
            assert_eq!(ran["ok"], true, "{name} {typed:?}: {ran}");
        }
    }
    at("build", Phase::Launched, false);
    set_state(root, "build", |each| each.worktree = checkout.to_owned());
    let report = json(&cm(root, &["--json", "advance", STEP, "pr-opn"]));
    assert_eq!(
        argvs(&report),
        [serde_json::json!([
            "cm",
            "advance",
            STEP,
            "pr-open",
            "--pr",
            "<pr>",
            "--sha",
            "<revision>"
        ])]
    );
}

#[test]
fn a_suggested_move_keeps_a_gate_flag_exactly_when_the_advance_takes_it() {
    use cm::state::Phase;
    let temp = chart_campaign();
    let root = temp.path();
    let cx = "09-daemon/chart-termination/cx";
    let release = "00-release/dekopon/0.100.0";
    for step in [cx, release] {
        let declared = cm(root, &["init", step, "--lane", "codex"]);
        assert!(declared.status.success(), "{declared:?}");
    }
    fs::write(root.join("ev.md"), "Synced.\n").unwrap();
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "x"]);
    let rev = git(&["rev-parse", "HEAD"]);
    let pr = "https://github.com/dekopon-agents/cm/pull/1";
    let fill = |word: &str| -> Value {
        Value::from(match word {
            "<pr>" => pr,
            "<revision>" => rev.as_str(),
            "<tag>" => "v0.100.0",
            "<digest>" => "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "<evidence-file>" => "ev.md",
            "<checkout>" => repo.to_str().unwrap(),
            word => word,
        })
    };
    let mut starts: Vec<(Phase, Option<Phase>)> = cm::state::ORDER
        .iter()
        .map(|phase| (phase.clone(), None))
        .collect();
    starts.push((Phase::Superseded, None));
    for resting in [Phase::Launched, Phase::PrOpen, Phase::Merged] {
        starts.push((Phase::Blocked("waiting".into()), Some(resting)));
    }
    let mut checked = Vec::new();
    for step in [cx, release] {
        let (unit, name) = step.rsplit_once('/').unwrap();
        for (phase, resting) in &starts {
            let mut at = cm::store::load_unit(&root.join(unit)).unwrap();
            let each = at.steps.get_mut(name).unwrap();
            each.track.state = phase.clone();
            each.track.blocked_from = resting.clone();
            each.track.evidence.pr = Some(pr.to_owned());
            each.track.evidence.sha = Some(rev.clone());
            cm::commands::write_unit(&root.join(unit), &at).unwrap();
            let report = json(&cm(
                root,
                &["--json", "advance", step, "mergd", "--head", &rev],
            ));
            let Some(argv) = argvs(&report).into_iter().find(Value::is_array) else {
                continue;
            };
            let words: Vec<&str> = argv
                .as_array()
                .unwrap()
                .iter()
                .map(|word| word.as_str().unwrap())
                .collect();
            if words[1] != "advance" {
                continue;
            }
            let mut run = vec![Value::from("cm"), Value::from("--json")];
            run.extend(words[1..].iter().map(|word| fill(word)));
            if !words.contains(&"--head") {
                run.extend([Value::from("--head"), Value::from(rev.as_str())]);
            }
            let ran = json(&run_argv(root, &Value::Array(run)));
            let unexpected = ran["violations"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|each| each["rule"] == "evidence-unexpected");
            assert_eq!(
                unexpected,
                !words.contains(&"--head"),
                "{step} at {phase} ({resting:?}): {words:?} -> {ran}"
            );
            checked.push((step, phase.to_string(), words.contains(&"--head")));
        }
    }
    assert!(checked.iter().any(|(_, _, kept)| *kept), "{checked:?}");
    assert!(
        checked.iter().filter(|(_, _, kept)| !kept).count() >= 5,
        "{checked:?}"
    );
}

#[test]
fn a_suggested_done_or_launch_takes_the_evidence_file_or_checkout_cm_already_has() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(cm(root, &["init"]).status.success());
    let mac = "09-daemon/x/mac";
    let pi = "09-daemon/x/pi";
    assert!(cm(root, &["init", mac, "--lane", "mac"]).status.success());
    assert!(cm(root, &["init", pi, "--lane", "pi"]).status.success());
    let asks = json(&cm(root, &["--json", "advance", mac, "dne"]));
    assert_eq!(
        argvs(&asks),
        [serde_json::json!([
            "cm",
            "advance",
            mac,
            "done",
            "--evidence",
            "<evidence-file>"
        ])]
    );
    fs::create_dir_all(root.join(mac).join("evidence")).unwrap();
    fs::write(root.join(mac).join("evidence/done.md"), "Done.\n").unwrap();
    let has = json(&cm(root, &["--json", "advance", mac, "dne"]));
    let done = serde_json::json!(["cm", "advance", mac, "done"]);
    assert_eq!(argvs(&has), std::slice::from_ref(&done));
    assert_eq!(
        has["violations"][0]["next"][0]["inputs"],
        serde_json::json!([])
    );
    let mut run = vec![Value::from("cm"), Value::from("--json")];
    run.extend(done.as_array().unwrap()[1..].iter().cloned());
    let ran = json(&run_argv(root, &Value::Array(run)));
    assert_eq!(ran["ok"], true, "{ran}");

    let unknown = json(&cm(root, &["--json", "advance", pi, "lanched"]));
    assert_eq!(
        argvs(&unknown),
        [serde_json::json!([
            "cm",
            "launch",
            pi,
            "--worktree",
            "<checkout>"
        ])]
    );
    let wt = root.join("wt");
    fs::create_dir_all(wt.join(".git")).unwrap();
    let wt = wt.to_str().unwrap();
    let known = json(&cm(
        root,
        &["--json", "advance", pi, "lanched", "--repo", wt],
    ));
    let launch = serde_json::json!(["cm", "launch", pi, "--worktree", wt]);
    assert_eq!(argvs(&known), std::slice::from_ref(&launch));
    assert_eq!(
        known["violations"][0]["next"][0]["inputs"],
        serde_json::json!([])
    );
    fs::write(root.join("k.md"), "Do it.\n").unwrap();
    assert!(
        cm(root, &["amend", pi, "--kickoff-file", "k.md"])
            .status
            .success()
    );
    let mut run = vec![Value::from("cm"), Value::from("--json")];
    run.extend(launch.as_array().unwrap()[1..].iter().cloned());
    let ran = json(&run_argv(root, &Value::Array(run)));
    assert_eq!(ran["ok"], true, "{ran}");
}

#[test]
fn a_release_steps_suggested_released_carries_its_declared_tag() {
    use cm::state::Phase;
    let temp = chart_campaign();
    let root = temp.path();
    let release = "00-release/dekopon/0.100.0";
    assert!(
        cm(root, &["init", release, "--lane", "claude-cloud"])
            .status
            .success()
    );
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "x"]);
    git(&["tag", "v0.100.0"]);
    let rev = git(&["rev-parse", "HEAD"]);
    let checkout = repo.to_str().unwrap();
    let set = |phase: Phase, recorded: bool| {
        let unit = root.join("00-release/dekopon");
        let mut state = cm::store::load_unit(&unit).unwrap();
        let step = state.steps.get_mut("0.100.0").unwrap();
        step.track.state = phase;
        step.track.evidence.sha = recorded.then(|| rev.clone());
        cm::commands::write_unit(&unit, &state).unwrap();
    };
    let fill =
        |argv: &Value| -> Value {
            let mut run = vec![Value::from("cm"), Value::from("--json")];
            run.extend(argv.as_array().unwrap()[1..].iter().map(
                |word| match word.as_str().unwrap() {
                    "<revision>" => Value::from(rev.as_str()),
                    "<checkout>" => Value::from(checkout),
                    _ => word.clone(),
                },
            ));
            json(&run_argv(root, &Value::Array(run)))
        };
    set(Phase::Merged, true);
    let merged = json(&cm(root, &["--json", "advance", release, "relased"]));
    assert_eq!(
        argvs(&merged),
        [serde_json::json!([
            "cm",
            "advance",
            release,
            "released",
            "--tag",
            "v0.100.0",
            "--repo",
            "<checkout>"
        ])]
    );
    let ran = fill(&argvs(&merged)[0]);
    assert_eq!(ran["ok"], true, "{ran}");
    for phase in [Phase::Drafted, Phase::Launched] {
        set(phase.clone(), false);
        let early = json(&cm(root, &["--json", "advance", release, "relased"]));
        assert_eq!(
            argvs(&early),
            [serde_json::json!([
                "cm",
                "advance",
                release,
                "released",
                "--tag",
                "v0.100.0",
                "--sha",
                "<revision>",
                "--repo",
                "<checkout>"
            ])],
            "{phase}"
        );
        let ran = fill(&argvs(&early)[0]);
        assert_eq!(ran["ok"], true, "{phase}: {ran}");
    }
}
