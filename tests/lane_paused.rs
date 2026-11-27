use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use cm::host::Host;
use jiff::Timestamp;
use serde_json::Value;

#[path = "support/campaign.rs"]
mod campaign_fixture;
use campaign_fixture::Campaign;

struct Fake;

impl Host for Fake {
    fn now(&self) -> Timestamp {
        "2026-11-20T00:00:00Z".parse().unwrap()
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(100 << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(0)
    }
}

// Each process a test starts gets the fixture's own HOME, global git config and TMPDIR, and none
// of the runner's GIT_DIR, GIT_WORK_TREE or GIT_INDEX_FILE. A campaign root here is a folder of
// its fixture's temporary directory, which holds them.
fn sealed<'a>(command: &'a mut Command, root: &Path) -> &'a mut Command {
    let fixture = root.parent().unwrap();
    command
        .env("HOME", fixture.join("home"))
        .env("GIT_CONFIG_GLOBAL", fixture.join("gitconfig"))
        .env("TMPDIR", fixture.join("tmp"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
}

fn outside_git() -> Campaign {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("home")).unwrap();
    fs::create_dir(temp.path().join("tmp")).unwrap();
    fs::write(temp.path().join("gitconfig"), "").unwrap();
    let root = temp.path().join("campaign");
    let init = sealed(
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("init")
            .arg(&root),
        &root,
    )
    .output()
    .unwrap();
    assert!(init.status.success(), "{init:?}");
    Campaign { _temp: temp, root }
}

fn in_git() -> Campaign {
    let c = outside_git();
    git(&c.root, "2026-11-20T00:00:00Z", &["init", "-q"]);
    c
}

fn git(root: &Path, date: &str, args: &[&str]) {
    let output = sealed(&mut Command::new("git"), root)
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=cm", "-c", "user.email=cm@example.com"])
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn write_paused(root: &Path, assignment: &str) {
    let path = root.join("LIMITS.toml");
    let text = fs::read_to_string(&path).unwrap();
    let text = text
        .lines()
        .map(|line| {
            let key = line
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"');
            if line.contains('=') && key == "paused" {
                assignment.to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, text + "\n").unwrap();
}

fn set_paused(root: &Path, paused: bool) {
    write_paused(root, &format!("paused = {paused}"));
}

fn commit_all(root: &Path, date: &str, message: &str) {
    git(root, date, &["add", "-A"]);
    git(root, date, &["commit", "-q", "-m", message]);
}

fn commit_paused(root: &Path, paused: bool, date: &str) {
    set_paused(root, paused);
    commit_all(root, date, &format!("paused = {paused}"));
}

fn journal(root: &Path, stamp: &str, title: &str, body: &str) {
    let path = root.join("JOURNAL.md");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str(&format!("\n## {stamp} — {title}\n\n{body}\n"));
    fs::write(path, text).unwrap();
}

fn lane(root: &Path, name: &str, session: &str) {
    fs::write(
        root.join(format!("LANE-{name}.md")),
        format!("# LANE-{name}\n"),
    )
    .unwrap();
    fs::create_dir_all(root.join("handoff")).unwrap();
    fs::write(
        root.join(format!("handoff/lane-{name}-2026-11-25T2335Z.md")),
        format!(
            "# {name} lane handoff\n\nHandoff: epoch=e role=lane-{name} lineage={session} written=2026-11-25T23:29Z supersedes=none brief=LANE-{name}.md\n"
        ),
    )
    .unwrap();
}

fn pause_entry(root: &Path, name: &str, stamp: &str) {
    let title = format!("Lane {name}: paused for Claude quota");
    journal(
        root,
        stamp,
        &title,
        &format!("Paused. State: handoff/lane-{name}-2026-11-25T2335Z.md."),
    );
}

fn act(root: &Path) -> (Option<i32>, Vec<Value>, String) {
    let run = |json: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        sealed(&mut command, root);
        if json {
            command.arg("--json");
        }
        command
            .arg("-C")
            .arg(root)
            .args(["next", "--act"])
            .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_current")
            .env_remove("CM_CODEX_WEEKLY_PCT")
            .output()
            .unwrap()
    };
    let json = run(true);
    let text = run(false);
    assert_eq!(json.status.code(), text.status.code());
    let value: Value =
        serde_json::from_slice(&json.stdout).unwrap_or_else(|error| panic!("{error}: {json:?}"));
    let paused = value["actionable"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["kind"] == "lane-paused")
        .cloned()
        .collect();
    (
        json.status.code(),
        paused,
        String::from_utf8(text.stdout).unwrap(),
    )
}

fn cm(root: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = sealed(&mut Command::new(env!("CARGO_BIN_EXE_cm")), root)
        .arg("-C")
        .arg(root)
        .args(args)
        .env("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_current")
        .env_remove("CM_CODEX_WEEKLY_PCT")
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

fn head(root: &Path) -> String {
    let output = sealed(&mut Command::new("git"), root)
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn eleven_26(names: &[&str]) -> Campaign {
    let c = in_git();
    commit_paused(&c.root, false, "2026-11-20T00:00:00Z");
    for name in names {
        lane(&c.root, name, &format!("session_{name}"));
    }
    commit_paused(&c.root, true, "2026-11-25T23:08:00Z");
    for name in names {
        pause_entry(&c.root, name, "2026-11-25T23:29Z");
        journal(
            &c.root,
            "2026-11-26T00:05Z",
            &format!("Lane {name}: a review recorded; the fix round waits for the resume"),
            "",
        );
    }
    commit_paused(&c.root, false, "2026-11-26T22:16:05Z");
    journal(
        &c.root,
        "2026-11-26T22:16Z",
        "Coordinator: resumed after quota reset",
        "paused = false in LIMITS.toml.",
    );
    c
}

#[test]
fn a_lane_parked_past_the_unpause_is_one_line_until_it_journals_a_resume() {
    let c = eleven_26(&["tooling"]);
    let (code, lines, text) = act(&c.root);
    assert_eq!(code, Some(1), "{text}");
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["kind"], "lane-paused");
    assert_eq!(line["target"], "tooling");
    assert_eq!(line["since"], "2026-11-26T22:16:05Z");
    assert_eq!(line["handoff"], "handoff/lane-tooling-2026-11-25T2335Z.md");
    assert_eq!(
        line["command"],
        "send_message session_tooling 'LIMITS.toml is paused = false: resume lane tooling from handoff/lane-tooling-2026-11-25T2335Z.md, then journal `Lane tooling: resumed`'"
    );
    let sha: String = head(&c.root).chars().take(12).collect();
    let summary = format!(
        "lane-paused tooling: paused at 2026-11-25T23:29Z (`Lane tooling: paused for Claude quota`), no lane entry after the unpause (LIMITS.toml commit {sha}) since 2026-11-26T22:16:05Z (0 waiting)"
    );
    assert_eq!(
        text,
        format!("{summary} — `{}`\n", line["command"].as_str().unwrap())
    );

    // Plain `cm next` prints it as a numbered item with its command, and exits 0 as ever.
    let (code, plain) = cm(&c.root, &["next"]);
    assert_eq!(code, Some(0), "{plain}");
    assert!(plain.contains(&summary), "{plain}");
    assert!(plain.contains(line["command"].as_str().unwrap()), "{plain}");

    // A `--target` or `--lane` view names steps, so it leaves the lane out.
    for args in [
        &["--json", "next", "--act", "--lane", "claude-cloud"][..],
        &["--json", "next", "--lane", "claude-cloud"][..],
    ] {
        let (_, out) = cm(&c.root, args);
        assert!(!out.contains("lane-paused"), "{args:?}: {out}");
    }

    // CARD.md, which sync renders from the units' facts, does not count it.
    let next = cm::plan::Next {
        paused: false,
        launchable: Vec::new(),
        waiting: Vec::new(),
    };
    let card = cm::render::card(&c.root, Fake.now(), "c", &Default::default(), &next);
    assert!(card.contains("Actionable: 0 (oldest: —)"), "{card}");

    // Plain `cm next` lists the same action.
    let next = sealed(&mut Command::new(env!("CARGO_BIN_EXE_cm")), &c.root)
        .arg("--json")
        .arg("-C")
        .arg(&c.root)
        .arg("next")
        .output()
        .unwrap();
    let next: Value = serde_json::from_slice(&next.stdout).unwrap();
    let listed: Vec<&Value> = next["actionable"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["kind"] == "lane-paused")
        .collect();
    assert_eq!(listed, [line]);

    // It repeats on every `--act` and writes nothing.
    let before = c.files();
    assert_eq!(act(&c.root).1.len(), 1);
    assert_eq!(c.files(), before);

    // A lane entry in the unpause's own minute is not after it; the next one is the resume.
    journal(
        &c.root,
        "2026-11-26T22:16Z",
        "Lane tooling: still reading",
        "",
    );
    assert_eq!(act(&c.root).1.len(), 1);
    journal(
        &c.root,
        "2026-11-26T23:59Z",
        "Lane tooling: resumed; cm#128 round 6 sent",
        "",
    );
    let (code, lines, text) = act(&c.root);
    assert_eq!((code, lines.len()), (Some(0), 0), "{text}");
    assert!(text.is_empty(), "{text}");
}

#[test]
fn while_paused_nothing_is_emitted() {
    let c = eleven_26(&["tooling"]);
    set_paused(&c.root, true);
    let (code, lines, text) = act(&c.root);
    assert_eq!((code, lines.len()), (Some(0), 0), "{text}");
}

#[test]
fn of_two_paused_lanes_only_the_one_not_resumed_is_listed() {
    let c = eleven_26(&["api", "tooling"]);
    journal(
        &c.root,
        "2026-11-26T22:30Z",
        "API lane: resumed from the handoff",
        "",
    );
    let (code, lines, _) = act(&c.root);
    assert_eq!(code, Some(1));
    let targets: Vec<&Value> = lines.iter().map(|line| &line["target"]).collect();
    assert_eq!(targets, ["tooling"]);
}

#[test]
fn a_lane_that_resumed_before_the_unpause_or_paused_after_it_is_not_listed() {
    let c = eleven_26(&["api", "tooling"]);
    // tooling resumed (and api paused) on its own, after the campaign's pause and before its unpause.
    journal(
        &c.root,
        "2026-11-26T05:00Z",
        "Lane tooling: resumed early",
        "",
    );
    // api's newest pause is after the unpause: a pause of its own, not the campaign's.
    pause_entry(&c.root, "api", "2026-11-26T22:20Z");
    let (code, lines, _) = act(&c.root);
    assert_eq!((code, lines.len()), (Some(0), 0), "{lines:?}");
}

#[test]
fn a_lane_with_no_paused_entry_or_no_brief_is_not_listed() {
    let c = eleven_26(&[]);
    // A brief, but the lane journaled no pause.
    fs::write(c.root.join("LANE-quiet.md"), "# LANE-quiet\n").unwrap();
    journal(
        &c.root,
        "2026-11-25T23:30Z",
        "Lane quiet: a card started",
        "",
    );
    // A pause entry for a lane with no brief and no registered driver.
    pause_entry(&c.root, "ghost", "2026-11-25T23:29Z");
    let (code, lines, _) = act(&c.root);
    assert_eq!((code, lines.len()), (Some(0), 0), "{lines:?}");
}

#[test]
fn a_registered_driver_is_a_lane_and_its_session_is_the_one_woken() {
    let c = eleven_26(&[]);
    fs::write(
        c.root.join(".cm/lane-drivers.json"),
        r#"{"drivers":{"tooling":{"session":"session_registered","units":["07-t/mvp"],"at":"2026-11-25T00:00Z","by":"session_c"}}}"#,
    )
    .unwrap();
    // No handoff named, so the line points at the journal entry.
    journal(
        &c.root,
        "2026-11-25T23:29Z",
        "Lane tooling: paused for Claude quota",
        "",
    );
    let (code, lines, _) = act(&c.root);
    assert_eq!(code, Some(1));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].get("handoff").is_none(), "{lines:?}");
    assert_eq!(
        lines[0]["command"],
        "send_message session_registered 'LIMITS.toml is paused = false: resume lane tooling from its journal entry 2026-11-25T23:29Z, then journal `Lane tooling: resumed`'"
    );
}

#[test]
fn with_no_limits_history_the_journal_names_the_unpause_or_no_line_is_given() {
    let c = outside_git();
    lane(&c.root, "tooling", "session_tooling");
    pause_entry(&c.root, "tooling", "2026-11-25T23:29Z");
    // No commit and no journal entry names the unpause: never a guessed time.
    let (code, lines, _) = act(&c.root);
    assert_eq!((code, lines.len()), (Some(0), 0), "{lines:?}");

    // The lane's own entries are not the unpause, even naming `paused = false`.
    journal(
        &c.root,
        "2026-11-26T00:05Z",
        "Lane tooling: noted",
        "waiting until `paused = false`",
    );
    assert_eq!(act(&c.root).1.len(), 0);

    journal(
        &c.root,
        "2026-11-26T21:00Z",
        "Owner: the resume plan",
        "At the reset the coordinator sets `paused = false` in LIMITS.toml.",
    );
    journal(
        &c.root,
        "2026-11-26T21:30Z",
        "Coordinator: resumed from its handoff after compaction",
        "`paused = false` waits for the reset.",
    );
    assert_eq!(act(&c.root).1.len(), 0);

    journal(
        &c.root,
        "2026-11-26T22:16Z",
        "Coordinator: resumed after quota reset",
        "`paused = false` in LIMITS.toml.",
    );
    let (code, lines, text) = act(&c.root);
    assert_eq!(code, Some(1), "{text}");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["since"], "2026-11-26T22:16:00Z");
    assert!(
        text.contains("(journal 2026-11-26T22:16Z `Coordinator: resumed after quota reset`; `git log` cannot read LIMITS.toml's history)"),
        "{text}"
    );

    journal(
        &c.root,
        "2026-11-27T01:06Z",
        "Owner: notes on the 11-26 resume",
        "The coordinator set `paused = false` in LIMITS.toml at 22:16Z.",
    );
    journal(
        &c.root,
        "2026-11-27T01:10Z",
        "Coordinator: resumed the prep thread",
        "LIMITS.toml unchanged.",
    );
    let (_, lines, text) = act(&c.root);
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["since"], "2026-11-26T22:16:00Z");
}

const RESUMED: &str = "Coordinator: resumed after quota reset";

fn unpaused_in_the_journal(body: &str) -> Campaign {
    let c = outside_git();
    lane(&c.root, "tooling", "session_tooling");
    pause_entry(&c.root, "tooling", "2026-11-25T23:29Z");
    journal(&c.root, "2026-11-26T22:16Z", RESUMED, body);
    c
}

#[test]
fn with_the_journal_fallback_a_later_note_recalling_or_planning_the_unpause_reopens_no_lane() {
    for (title, body) in [
        (
            "Owner: a paused lane must not outlive the pause",
            "The coordinator resumed at 22:16Z (`paused = false` in LIMITS.toml) and the lane waited.",
        ),
        (
            "Coordinator: resumed from handoff after compaction",
            "The previous coordinator set paused = false in LIMITS.toml at 22:16Z yesterday; LIMITS.toml is unchanged this turn.",
        ),
        (
            "Coordinator: resumed the prep thread",
            "Do not set paused = false in LIMITS.toml until the next reset.",
        ),
        (
            RESUMED,
            "The previous coordinator set paused = false in LIMITS.toml at 22:16Z yesterday.",
        ),
        (
            RESUMED,
            "Do not set `paused = false` in LIMITS.toml until the next reset.",
        ),
        (
            RESUMED,
            "The previous coordinator set\n`paused = false` in LIMITS.toml.",
        ),
        (
            RESUMED,
            "paused = false in LIMITS.toml? No; LIMITS.toml is unchanged this turn.",
        ),
        (
            RESUMED,
            "The previous coordinator wrote this yesterday; LIMITS.toml is unchanged this turn.\n\n```\npaused = false in LIMITS.toml.\n```",
        ),
        (
            RESUMED,
            "Quoted from yesterday:\n- ```\n  paused = false in LIMITS.toml.\n  ```",
        ),
        (
            RESUMED,
            "Yesterday's record:\n~~~\nResumed.\npaused = false in LIMITS.toml.\n~~~",
        ),
        (
            RESUMED,
            "Yesterday's record, quoted:\n````\n```\nResumed.\npaused = false in LIMITS.toml.\n````",
        ),
        (
            RESUMED,
            "Yesterday's record, quoted:\n~~~~\n~~~\nResumed.\npaused = false in LIMITS.toml.\n~~~~",
        ),
        (
            RESUMED,
            "Yesterday's record, quoted:\n```\n```markdown\nResumed.\npaused = false in LIMITS.toml.\n```",
        ),
        (
            RESUMED,
            "Yesterday's record, quoted:\n```\n~~~\nResumed.\npaused = false in LIMITS.toml.\n```",
        ),
        (
            RESUMED,
            "~~~~\n~~~\npaused = false in LIMITS.toml.\n~~~\n~~~~",
        ),
        (
            RESUMED,
            "Yesterday's record, quoted:\n```\n    ```\nResumed.\npaused = false in LIMITS.toml.\n```",
        ),
        (
            RESUMED,
            "Yesterday's record, quoted:\n\n    Resumed.\n    paused = false in LIMITS.toml.",
        ),
        (
            RESUMED,
            "Yesterday's coordinator wrote:\n\n    paused = false in LIMITS.toml.",
        ),
        (
            RESUMED,
            "   ~~~\n    ~~~\npaused = false in LIMITS.toml.\n   ~~~",
        ),
        (RESUMED, "1. ~~~\n~~~\npaused = false in LIMITS.toml.\n~~~"),
        (
            RESUMED,
            "Quoted from yesterday:\n* ```\n  Resumed.\n  paused = false in LIMITS.toml.\n  ```",
        ),
        (
            RESUMED,
            "- notes\n    ```\n  paused = false in LIMITS.toml.\n    ```",
        ),
        (
            RESUMED,
            "1. notes\n    ~~~\n   paused = false in LIMITS.toml.\n    ~~~",
        ),
        (
            RESUMED,
            "- notes\n    - ```\n  paused = false in LIMITS.toml.\n    ```",
        ),
        (
            RESUMED,
            "1. ~~~\n~~~~\n   ~~~\npaused = false in LIMITS.toml.\n~~~~",
        ),
        (
            RESUMED,
            "- ```\n````\n  ```\npaused = false in LIMITS.toml.\n````",
        ),
        (
            RESUMED,
            "Quoted:\n- ```\n```\nResumed.\npaused = false in LIMITS.toml.\n```",
        ),
        (
            RESUMED,
            "Yesterday's record, quoted:\n\n\tResumed.\n\tpaused = false in LIMITS.toml.",
        ),
        (
            RESUMED,
            "Quoted from yesterday:\n+ ```\n  Resumed.\n  paused = false in LIMITS.toml.\n  ```",
        ),
        (
            RESUMED,
            "Quoted from yesterday:\n1. ```\n   Resumed.\n   paused = false in LIMITS.toml.\n   ```",
        ),
        (
            RESUMED,
            "Quoted from yesterday:\n1) ```\n   Resumed.\n   paused = false in LIMITS.toml.\n   ```",
        ),
        (
            "Coordinator: resumed from handoff after compaction",
            "paused = false in LIMITS.toml.",
        ),
    ] {
        let c = unpaused_in_the_journal("paused = false in LIMITS.toml.");
        journal(
            &c.root,
            "2026-11-26T23:59Z",
            "Lane tooling: a fix round sent",
            "",
        );
        let (code, lines, text) = act(&c.root);
        assert_eq!((code, lines.len()), (Some(0), 0), "{text}");

        journal(&c.root, "2026-11-27T01:10Z", title, body);
        let (code, lines, text) = act(&c.root);
        assert_eq!((code, lines.len()), (Some(0), 0), "{title}: {body}: {text}");
    }
}

#[test]
fn with_the_journal_fallback_the_record_states_the_assignment_as_a_clause_of_its_own() {
    for body in [
        "Owner woke the coordinator by hand. paused = false in LIMITS.toml; hourly backstop re-enabled.",
        "Resumed from the handoff:\n\n- `paused=false` in LIMITS.toml\n- heartbeat re-armed",
        "Owner woke the coordinator by hand\n\npaused = false in LIMITS.toml! Lanes next.",
        "Resumed; ran\n```\ncm next --act\n```\npaused = false in LIMITS.toml.",
        "Resumed; ran\n```\ncm next --act\n`````\npaused = false in LIMITS.toml.",
        "Resumed from the handoff:\n1. `paused = false` in LIMITS.toml\n2) heartbeat re-armed",
        "Resumed:\n1. ran\n   ```\n   cm next --act\n   ```\n\npaused = false in LIMITS.toml.",
        "Ran:\n10. ```\n    cm next --act\n    ```\n\npaused = false in LIMITS.toml.",
        "Ran:\n- ```\n  cm next --act\n\n  cm lint\n  ```\n\npaused = false in LIMITS.toml.",
    ] {
        let c = unpaused_in_the_journal(body);
        let (code, lines, text) = act(&c.root);
        assert_eq!(code, Some(1), "{body}: {text}");
        assert_eq!(lines.len(), 1, "{body}: {text}");
        assert_eq!(lines[0]["since"], "2026-11-26T22:16:00Z");
    }
}

#[test]
fn with_the_journal_fallback_a_record_shaped_plan_or_restatement_is_taken_as_the_unpause() {
    for body in [
        "Next:\n- paused = false in LIMITS.toml",
        "Plan for the next reset:\n- paused = false in LIMITS.toml.",
        "Yesterday's record, restated. paused = false in LIMITS.toml.",
    ] {
        let c = unpaused_in_the_journal("paused = false in LIMITS.toml.");
        journal(
            &c.root,
            "2026-11-26T23:59Z",
            "Lane tooling: a fix round sent",
            "",
        );
        journal(&c.root, "2026-11-27T01:10Z", RESUMED, body);
        let (code, lines, text) = act(&c.root);
        assert_eq!((code, lines.len()), (Some(1), 1), "{body}: {text}");
        assert_eq!(lines[0]["since"], "2026-11-27T01:10:00Z", "{body}");
    }
}

#[test]
fn with_the_journal_fallback_an_html_block_is_read_as_text() {
    let c = unpaused_in_the_journal("paused = false in LIMITS.toml.");
    journal(
        &c.root,
        "2026-11-26T23:59Z",
        "Lane tooling: a fix round sent",
        "",
    );
    journal(
        &c.root,
        "2026-11-27T01:10Z",
        RESUMED,
        "<pre>\nResumed.\npaused = false in LIMITS.toml.\n</pre>",
    );
    let (code, lines, text) = act(&c.root);
    assert_eq!((code, lines.len()), (Some(1), 1), "{text}");
    assert_eq!(lines[0]["since"], "2026-11-27T01:10:00Z");
}

#[test]
fn an_unpause_with_a_quoted_key_is_read_from_history() {
    let c = in_git();
    commit_paused(&c.root, false, "2026-11-20T00:00:00Z");
    lane(&c.root, "tooling", "session_tooling");
    write_paused(&c.root, "\"paused\" = true");
    commit_all(&c.root, "2026-11-25T23:08:00Z", "pause");
    pause_entry(&c.root, "tooling", "2026-11-25T23:29Z");
    write_paused(&c.root, "\"paused\" = false");
    commit_all(&c.root, "2026-11-26T22:16:05Z", "unpause");
    // No journal entry names the unpause, so the line can only come from the commit.
    let (code, lines, text) = act(&c.root);
    assert_eq!(code, Some(1), "{text}");
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["since"], "2026-11-26T22:16:05Z");
    let sha: String = head(&c.root).chars().take(12).collect();
    assert!(
        text.contains(&format!("(LIMITS.toml commit {sha})")),
        "{text}"
    );
}

#[test]
fn an_unpause_a_merge_commit_makes_is_read_from_first_parent_history() {
    let c = in_git();
    commit_paused(&c.root, false, "2026-11-20T00:00:00Z");
    lane(&c.root, "tooling", "session_tooling");
    commit_paused(&c.root, true, "2026-11-25T23:08:00Z");
    pause_entry(&c.root, "tooling", "2026-11-25T23:29Z");
    commit_all(&c.root, "2026-11-25T23:30:00Z", "journal");
    let merge = |date: &str, branch: &str| {
        git(&c.root, date, &["checkout", "-q", "-"]);
        git(
            &c.root,
            date,
            &["merge", "-q", "--no-ff", "--no-commit", branch],
        );
    };

    // Both parents have `paused = true`; the merge itself writes `paused = false`.
    git(
        &c.root,
        "2026-11-26T10:00:00Z",
        &["checkout", "-q", "-b", "side"],
    );
    fs::write(c.root.join("side.md"), "side\n").unwrap();
    commit_all(&c.root, "2026-11-26T10:00:00Z", "side");
    merge("2026-11-26T22:16:05Z", "side");
    commit_paused(&c.root, false, "2026-11-26T22:16:05Z");
    let (code, lines, text) = act(&c.root);
    assert_eq!(code, Some(1), "{text}");
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["since"], "2026-11-26T22:16:05Z");
    let sha: String = head(&c.root).chars().take(12).collect();
    assert!(
        text.contains(&format!("(LIMITS.toml commit {sha})")),
        "{text}"
    );

    // A branch that sets `paused = false`, merged later: the unpause is the merge, where the
    // first-parent history changes.
    commit_paused(&c.root, true, "2026-11-27T00:00:00Z");
    pause_entry(&c.root, "tooling", "2026-11-27T00:30Z");
    commit_all(&c.root, "2026-11-27T00:30:00Z", "journal");
    git(
        &c.root,
        "2026-11-27T01:00:00Z",
        &["checkout", "-q", "-b", "unpause"],
    );
    commit_paused(&c.root, false, "2026-11-27T01:00:00Z");
    merge("2026-11-27T02:00:00Z", "unpause");
    commit_all(&c.root, "2026-11-27T02:00:00Z", "merge unpause");
    let (code, lines, text) = act(&c.root);
    assert_eq!(code, Some(1), "{text}");
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["since"], "2026-11-27T02:00:00Z");
}

#[test]
fn an_uncommitted_or_shallow_unpause_falls_back_to_the_journal() {
    let c = eleven_26(&["tooling"]);
    // Back to a committed pause, then `paused = false` only in the working tree.
    commit_paused(&c.root, true, "2026-11-26T23:00:00Z");
    set_paused(&c.root, false);
    let (_, lines, text) = act(&c.root);
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["since"], "2026-11-26T22:16:00Z");
    assert!(text.contains("`paused = false` is not committed"), "{text}");

    // A shallow clone holding only the unpause commit cannot tell when it happened.
    let c = eleven_26(&["tooling"]);
    let clone = c.root.parent().unwrap().join("shallow");
    let url = format!("file://{}", c.root.display());
    let output = sealed(&mut Command::new("git"), &c.root)
        .args(["clone", "-q", "--depth", "1", &url])
        .arg(&clone)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::copy(c.root.join("JOURNAL.md"), clone.join("JOURNAL.md")).unwrap();
    let (_, lines, text) = act(&clone);
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["since"], "2026-11-26T22:16:00Z");
    assert!(text.contains("not in this clone (shallow?)"), "{text}");
}

#[test]
fn a_title_naming_both_words_is_a_resume() {
    let c = eleven_26(&["tooling"]);
    // Before the unpause, so only the word decides: resumed after the paused window.
    journal(
        &c.root,
        "2026-11-26T06:00Z",
        "Lane tooling: resumed after the paused window",
        "",
    );
    let (code, lines, _) = act(&c.root);
    assert_eq!((code, lines.len()), (Some(0), 0), "{lines:?}");
}

#[test]
fn with_the_journal_fallback_a_lane_entry_in_the_unpause_minute_is_no_resume() {
    let c = outside_git();
    lane(&c.root, "tooling", "session_tooling");
    pause_entry(&c.root, "tooling", "2026-11-25T23:29Z");
    journal(
        &c.root,
        "2026-11-26T22:16Z",
        "Coordinator: resumed after quota reset",
        "`paused = false` in LIMITS.toml.",
    );
    journal(&c.root, "2026-11-26T22:16Z", "Lane tooling: reading", "");
    assert_eq!(act(&c.root).1.len(), 1);
    journal(&c.root, "2026-11-26T22:17Z", "Lane tooling: resumed", "");
    assert_eq!(act(&c.root).1.len(), 0);
}
