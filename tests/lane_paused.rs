//! `lane-paused <lane>` in `cm next --act`: a lane parked on its paused handoff after LIMITS.toml's
//! `paused = false`.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use cm::host::Host;
use jiff::Timestamp;
use serde_json::Value;

#[path = "support/campaign.rs"]
mod campaign_fixture;
use campaign_fixture::{Campaign, campaign, campaign_outside_git};

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

fn git(root: &Path, date: &str, args: &[&str]) {
    let output = Command::new("git")
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

fn set_paused(root: &Path, paused: bool) {
    let path = root.join("LIMITS.toml");
    let text = fs::read_to_string(&path).unwrap();
    let text = text
        .lines()
        .map(|line| {
            if line.starts_with("paused =") {
                format!("paused = {paused}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, text + "\n").unwrap();
}

/// Sets `paused` and commits the whole tree, authored at `date`.
fn commit_paused(root: &Path, paused: bool, date: &str) {
    set_paused(root, paused);
    git(root, date, &["add", "-A"]);
    git(
        root,
        date,
        &["commit", "-q", "-m", &format!("paused = {paused}")],
    );
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

/// The 11-25/26 shape: paused at 23:08Z, the lane's paused handoff at 23:29Z, a later lane
/// progress entry, `paused = false` committed at 22:16Z the next day.
fn eleven_26(names: &[&str]) -> Campaign {
    let c = campaign(&Fake);
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
    assert!(
        text.starts_with("lane-paused tooling: paused at 2026-11-25T23:29Z (`Lane tooling: paused for Claude quota`), no lane entry after the unpause (LIMITS.toml commit "),
        "{text}"
    );
    assert!(
        text.contains("since 2026-11-26T22:16:05Z (0 waiting)"),
        "{text}"
    );

    // Plain `cm next` lists the same action.
    let next = Command::new(env!("CARGO_BIN_EXE_cm"))
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
    let c = campaign_outside_git(&Fake);
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
    let output = Command::new("git")
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
