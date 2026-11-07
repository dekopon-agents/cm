#[path = "support/campaign.rs"]
mod campaign_fixture;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cm::commands::{self, Declaration};
use cm::host::Host;
use cm::ledger::{self, Feedback, QueueItem};
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::review::{self, ReviewArgs};
use cm::state::{Evidence, Lane, Phase};
use cm::store::parse_target;
use cm::templates::LOOP_WITHHELD;
use jiff::Timestamp;

struct Fake;

impl Host for Fake {
    fn now(&self) -> Timestamp {
        "2026-10-23T03:10:00Z".parse().unwrap()
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(100 << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(0)
    }
}

fn rules(outcome: &Outcome) -> Vec<&'static str> {
    let mut rules: Vec<_> = outcome.violations.iter().map(|v| v.rule).collect();
    rules.sort_unstable();
    rules
}

fn campaign() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    let git = Command::new("git").args(["init", "-q"]).arg(&root).status();
    assert!(
        git.unwrap().success(),
        "a campaign lives in a git repository"
    );
    assert!(commands::init(&Fake, &root).unwrap().ok());
    fs::write(
        root.join("EPOCH.md"),
        "---\nstatus: WORKING\nsequence: d\n---\n\n# Epoch\n",
    )
    .unwrap();
    (temp, root)
}

fn entry<'a>(title: &'a str, urgency: &'a str) -> Feedback<'a> {
    Feedback {
        title,
        urgency: Some(urgency),
        follow_up: None,
        disposition: None,
        body: "What happened.\n",
    }
}

const HAND_WRITTEN: &str = "# Feedback\n\n\
## 2026-10-23: lessons from the WORKING session's first hour (session_01VKGNYi3XbPDSULoHck8HJX)\n\n\
### FB-d-001 · urgent · proposed: the deployed Wasmtime overlay had no advisory watch\n\nText.\n\n\
### FB-d-011 · queued · proposed: lessons go through cm\n\nText.\n\n\
### FB-d-011 follow-up · urgent · accepted (owner): logging a lesson goes through cm\n\nText.\n\n\
### FB-d-014 · manual-write ledger · the WORKING session, 2026-10-23T02:12Z–03:10Z\n\nText.\n";

#[test]
fn literal_fences_preserve_feedback_ids_follow_ups_and_lint() {
    let (_temp, root) = campaign();
    for (title, body, id) in [
        ("one", "````\n```\nliteral\n````\n", "FB-d-001"),
        ("two", "plain", "FB-d-002"),
        ("three", "plain", "FB-d-003"),
    ] {
        let added = ledger::feedback(
            &Fake,
            &root,
            &Feedback {
                body,
                ..entry(title, "queued")
            },
        )
        .unwrap();
        assert!(added.ok(), "{:?}", added.violations);
        assert_eq!(added.fields["id"], id);
    }
    for id in ["FB-d-001", "FB-d-002", "FB-d-003"] {
        let follow = Feedback {
            follow_up: Some(id),
            disposition: Some("accepted"),
            urgency: None,
            ..entry("reply", "queued")
        };
        let added = ledger::feedback(&Fake, &root, &follow).unwrap();
        assert!(added.ok(), "{:?}", added.violations);
    }
    assert!(lint(&root).unwrap().ok());
}

#[test]
fn feedback_fences_close_only_with_matching_bare_delimiters() {
    for (open, body, close) in [
        ("````rust", "```\n~~~\n````info\n### literal", "````` "),
        ("~~~~text", "~~~\n```\n~~~~info\n### literal", "~~~~~\t"),
    ] {
        let (_temp, root) = campaign();
        let body = format!("{open}\n{body}\n{close}\n");
        let added = ledger::feedback(
            &Fake,
            &root,
            &Feedback {
                body: &body,
                ..entry("one", "queued")
            },
        )
        .unwrap();
        assert!(added.ok(), "{:?}", added.violations);
        let next = ledger::feedback(&Fake, &root, &entry("two", "queued")).unwrap();
        assert_eq!(next.fields["id"], "FB-d-002");
    }
}

#[test]
fn unterminated_feedback_fences_are_refused_before_writing() {
    let (_temp, root) = campaign();
    let journal = fs::read(root.join("JOURNAL.md")).unwrap();
    for body in [
        "```\ntext",
        "~~~~\n~~~",
        "````\n~~~",
        "```\n```info",
        "    ```\ntext",
    ] {
        let added = ledger::feedback(
            &Fake,
            &root,
            &Feedback {
                body,
                ..entry("one", "queued")
            },
        )
        .unwrap();
        assert_eq!(rules(&added), vec!["feedback-body-fence"]);
        assert!(!root.join("FEEDBACK.md").exists());
        assert_eq!(fs::read(root.join("JOURNAL.md")).unwrap(), journal);
    }
}

#[test]
fn lint_detects_hand_written_duplicate_feedback_ids() {
    let (_temp, root) = campaign();
    fs::write(root.join("FEEDBACK.md"), "# Feedback\n\n````\n```\nliteral\n````\n\n### FB-d-001 · queued · one\n\n### FB-d-001 · queued · two\n").unwrap();
    assert_eq!(rules(&lint(&root).unwrap()), vec!["feedback-id-duplicate"]);
}

#[test]
fn concurrent_feedback_appends_get_distinct_ids() {
    let (_temp, root) = campaign();
    let body = root.parent().unwrap().join("body");
    fs::write(&body, "plain\n").unwrap();
    let children: Vec<_> = (0..4)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_cm"))
                .arg("-C")
                .arg(&root)
                .args([
                    "--json",
                    "feedback",
                    "one",
                    "--urgency",
                    "queued",
                    "--body-file",
                ])
                .arg(&body)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut ids: Vec<String> = children
        .into_iter()
        .map(|child| {
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            json["id"].as_str().unwrap().to_owned()
        })
        .collect();
    ids.sort();
    assert_eq!(ids, ["FB-d-001", "FB-d-002", "FB-d-003", "FB-d-004"]);
    assert!(lint(&root).unwrap().ok());
}

#[test]
fn feedback_ids_are_sequential_per_campaign() {
    let (_temp, root) = campaign();
    let first = ledger::feedback(&Fake, &root, &entry("one", "urgent")).unwrap();
    assert!(first.ok(), "{:?}", first.violations);
    assert_eq!(first.fields["id"], "FB-d-001");
    assert_eq!(
        ledger::feedback(&Fake, &root, &entry("two", "queued"))
            .unwrap()
            .fields["id"],
        "FB-d-002"
    );
    fs::write(root.join("FEEDBACK.md"), HAND_WRITTEN).unwrap();
    assert!(lint(&root).unwrap().ok());
    let next = ledger::feedback(&Fake, &root, &entry("after the hand", "queued")).unwrap();
    assert_eq!(next.fields["id"], "FB-d-015");
    let follow = Feedback {
        follow_up: Some("FB-d-015"),
        disposition: Some("applied"),
        urgency: None,
        ..entry("shipped in cm 0.5", "queued")
    };
    assert!(ledger::feedback(&Fake, &root, &follow).unwrap().ok());
    let unknown = Feedback {
        follow_up: Some("FB-d-099"),
        ..follow
    };
    assert_eq!(
        rules(&ledger::feedback(&Fake, &root, &unknown).unwrap()),
        vec!["feedback-follow-up-unknown"]
    );
    let text = fs::read_to_string(root.join("FEEDBACK.md")).unwrap();
    assert!(text.contains("### FB-d-015 · queued · after the hand\n"));
    assert!(text.contains("### FB-d-015 follow-up · applied: shipped in cm 0.5\n"));
    let journal = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("— feedback FB-d-015: after the hand"));
    assert!(lint(&root).unwrap().ok());
    fs::write(
        root.join("FEEDBACK.md"),
        format!("{text}\n### FB-d-001 · queued · again\n\n### FB-d-777 follow-up · applied: x\n"),
    )
    .unwrap();
    assert_eq!(
        rules(&lint(&root).unwrap()),
        vec!["feedback-follow-up-unknown", "feedback-id-duplicate"]
    );
}

#[test]
fn owner_queue_add_withholds_the_loop_banner() {
    let (_temp, root) = campaign();
    let item = |title| QueueItem {
        title,
        context: "The Pi's fork predates 49.0.4.",
        question: "Rebase the fork now?",
        recommendation: "Yes, before the next deploy.",
    };
    let empty = QueueItem {
        recommendation: " ",
        ..item("x")
    };
    assert_eq!(
        rules(&ledger::owner_queue_add(&Fake, &root, &empty).unwrap()),
        vec!["ledger-field"]
    );
    let added = ledger::owner_queue_add(&Fake, &root, &item("The Wasmtime fork")).unwrap();
    assert!(added.ok(), "{:?}", added.violations);
    assert_eq!(added.fields["item"], 1);
    let resume = fs::read_to_string(root.join("RESUME.md")).unwrap();
    assert!(resume.contains(&format!("> {LOOP_WITHHELD}")));
    assert!(!resume.contains("/loop You are"));
    let queue = fs::read_to_string(root.join("OWNER-QUEUE.md")).unwrap();
    assert!(queue.ends_with(
        "## 1. The Wasmtime fork\n\nContext: The Pi's fork predates 49.0.4.\nRebase the fork now?\nRecommendation: Yes, before the next deploy.\nAnswer:\n"
    ));
    fs::write(
        root.join("OWNER-QUEUE.md"),
        queue.replace("## 1. The", "## 12. The"),
    )
    .unwrap();
    assert_eq!(
        ledger::owner_queue_add(&Fake, &root, &item("Next"))
            .unwrap()
            .fields["item"],
        13
    );
    assert_eq!(fs::read_to_string(root.join("RESUME.md")).unwrap(), resume);
    assert!(lint(&root).unwrap().ok());
}

#[path = "support/owner_queue.rs"]
mod queue_fixture;

#[test]
fn owner_queue_answer_removes_only_its_item_and_journals_the_body() {
    let c = queue_fixture::campaign(&Fake);
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&c.root)
            .args(["add", "JOURNAL.md"])
            .status()
            .unwrap()
            .success()
    );
    let before = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let resume = fs::read(c.root.join("RESUME.md")).unwrap();
    let body = queue_fixture::body_file(&c.root, queue_fixture::ANSWER);
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["--json", "owner-queue", "answer", "7", "--body-file"])
        .arg(body)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["item"], 7);
    assert_eq!(json["queue_drained"], false);
    assert_eq!(
        fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap(),
        format!(
            "{}\n{}{}",
            cm::templates::OWNER_QUEUE,
            queue_fixture::FIRST,
            queue_fixture::LAST
        )
    );
    assert_eq!(fs::read(c.root.join("RESUME.md")).unwrap(), resume);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert_eq!(
        journal.strip_prefix(before.as_str()).unwrap(),
        format!(
            "\n{}\n\n- OWNER-QUEUE.md: `## 7. Middle`\n\nRemoved item:\n\n{}\nAnswer:\n{}",
            json["journal"].as_str().unwrap(),
            queue_fixture::ARCHIVED_MIDDLE,
            queue_fixture::ANSWER
        )
    );
    assert!(lint(&c.root).unwrap().ok());
    let checked = Command::new("git")
        .arg("-C")
        .arg(&c.root)
        .args(["diff", "--check", "--", "JOURNAL.md"])
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
}

#[test]
fn owner_queue_answer_stdin_drains_the_queue_and_requests_the_loop() {
    let c = queue_fixture::campaign(&Fake);
    assert!(
        ledger::owner_queue_strike(&Fake, &c.root, 2, "Superseded.")
            .unwrap()
            .ok()
    );
    assert!(
        ledger::owner_queue_strike(&Fake, &c.root, 11, "Superseded.")
            .unwrap()
            .ok()
    );
    let resume = fs::read_to_string(c.root.join("RESUME.md")).unwrap();
    let before = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["owner-queue", "answer", "7", "--body-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(queue_fixture::ANSWER.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "owner-queue answer 7. Middle\nOWNER-QUEUE is drained. The session must print the `/loop` command that resumes the campaign.\n"
    );
    assert_eq!(
        fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap(),
        format!("{}\n", cm::templates::OWNER_QUEUE)
    );
    assert_eq!(
        fs::read_to_string(c.root.join("RESUME.md")).unwrap(),
        resume.replacen(&format!("> {LOOP_WITHHELD}\n"), "", 1)
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let added = journal.strip_prefix(before.as_str()).unwrap();
    assert_eq!(
        added.lines().nth(1).unwrap().split_once(" — ").unwrap().1,
        "owner-queue answer 7. Middle"
    );
    assert_eq!(
        added.lines().skip(3).collect::<Vec<_>>().join("\n"),
        format!(
            "- OWNER-QUEUE.md: `## 7. Middle`\n\nRemoved item:\n\n{}\nAnswer:\n{}",
            queue_fixture::ARCHIVED_MIDDLE,
            queue_fixture::ANSWER.trim_end()
        )
    );
    let outcome = lint(&c.root).unwrap();
    assert_eq!(rules(&outcome), vec!["resume-loop"]);
    assert_eq!(
        outcome.violations[0].message,
        "OWNER-QUEUE.md has no open item, so the banner holds a fenced `/loop …` command that resumes the campaign"
    );
}

#[test]
fn owner_queue_strike_requires_a_reason_and_journals_it() {
    let c = queue_fixture::campaign(&Fake);
    let queue = fs::read(c.root.join("OWNER-QUEUE.md")).unwrap();
    let before = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["owner-queue", "strike", "7"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fs::read(c.root.join("OWNER-QUEUE.md")).unwrap(), queue);
    assert_eq!(
        fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
        before
    );
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args([
            "--json",
            "owner-queue",
            "strike",
            "7",
            "--reason",
            "Superseded.",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap(),
        format!(
            "{}\n{}{}",
            cm::templates::OWNER_QUEUE,
            queue_fixture::FIRST,
            queue_fixture::LAST
        )
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert_eq!(
        journal.strip_prefix(before.as_str()).unwrap(),
        format!(
            "\n{}\n\n- OWNER-QUEUE.md: `## 7. Middle`\n\nRemoved item:\n\n{}\nReason: Superseded.\n",
            json["journal"].as_str().unwrap(),
            queue_fixture::ARCHIVED_MIDDLE
        )
    );
}

#[test]
fn owner_queue_edit_preserves_the_heading_and_required_fields() {
    let c = queue_fixture::campaign(&Fake);
    let body = queue_fixture::body_file(&c.root, queue_fixture::REPLACEMENT);
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["owner-queue", "edit", "7", "--body-file"])
        .arg(body)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap(),
        format!(
            "{}\n{}## 7. Middle\n\n{}\n\n{}",
            cm::templates::OWNER_QUEUE,
            queue_fixture::FIRST,
            queue_fixture::REPLACEMENT,
            queue_fixture::LAST
        )
    );
    let linted = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .arg("lint")
        .output()
        .unwrap();
    assert!(linted.status.success(), "{linted:?}");
}

#[test]
fn owner_queue_edit_preserves_a_setext_heading_and_keeps_the_gate_closed() {
    let c = queue_fixture::setext_item(&Fake);
    let resume = fs::read(c.root.join("RESUME.md")).unwrap();
    let edited = ledger::owner_queue_edit(&Fake, &c.root, 7, queue_fixture::REPLACEMENT).unwrap();
    assert!(edited.ok(), "{:?}", edited.violations);
    assert_eq!(edited.fields["queue_drained"], false);
    let queue = fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap();
    assert_eq!(
        queue,
        format!(
            "{}\n{}\n{}\n\n",
            cm::templates::OWNER_QUEUE,
            queue_fixture::SETEXT_HEADING,
            queue_fixture::REPLACEMENT
        )
    );
    assert!(cm::templates::queue_open(&queue));
    assert_eq!(fs::read(c.root.join("RESUME.md")).unwrap(), resume);
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn owner_queue_unknown_items_and_empty_bodies_are_refused_without_writes() {
    let c = queue_fixture::campaign(&Fake);
    let before: Vec<_> = ["OWNER-QUEUE.md", "RESUME.md", "JOURNAL.md"]
        .map(|name| (name, fs::read(c.root.join(name)).unwrap()))
        .into();
    for outcome in [
        ledger::owner_queue_answer(&Fake, &c.root, 99, "Yes.").unwrap(),
        ledger::owner_queue_strike(&Fake, &c.root, 99, "Superseded.").unwrap(),
        ledger::owner_queue_edit(&Fake, &c.root, 99, queue_fixture::REPLACEMENT).unwrap(),
    ] {
        assert_eq!(rules(&outcome), vec!["owner-queue-unknown"]);
    }
    for body in ["", " \n\n"] {
        assert_eq!(
            rules(&ledger::owner_queue_answer(&Fake, &c.root, 7, body).unwrap()),
            vec!["owner-queue-empty"]
        );
        assert_eq!(
            rules(&ledger::owner_queue_edit(&Fake, &c.root, 7, body).unwrap()),
            vec!["owner-queue-empty"]
        );
    }
    for (name, text) in before {
        assert_eq!(fs::read(c.root.join(name)).unwrap(), text);
    }
}

#[test]
fn owner_queue_edit_refuses_missing_fields_and_section_injection() {
    let c = queue_fixture::campaign(&Fake);
    let before = fs::read(c.root.join("OWNER-QUEUE.md")).unwrap();
    for field in cm::templates::QUEUE_FIELDS {
        let body = queue_fixture::REPLACEMENT
            .lines()
            .filter(|line| !line.starts_with(field))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            rules(&ledger::owner_queue_edit(&Fake, &c.root, 7, &body).unwrap()),
            vec!["owner-queue-field"]
        );
    }
    for (body, rule) in [
        (
            format!("{}\n\n## 8. Injected", queue_fixture::REPLACEMENT),
            "ledger-body",
        ),
        (
            format!("{}\n\n```text\nunfinished", queue_fixture::REPLACEMENT),
            "owner-queue-body-fence",
        ),
    ] {
        assert_eq!(
            rules(&ledger::owner_queue_edit(&Fake, &c.root, 7, &body).unwrap()),
            vec![rule]
        );
    }
    assert_eq!(fs::read(c.root.join("OWNER-QUEUE.md")).unwrap(), before);
}

#[test]
fn owner_queue_add_strips_one_recommendation_prefix_in_any_case() {
    let c = queue_fixture::campaign(&Fake);
    for recommendation in ["Recommendation: Proceed.", "  rEcOmMeNdAtIoN: Proceed.  "] {
        let outcome =
            ledger::owner_queue_add(&Fake, &c.root, &queue_fixture::item("Next", recommendation))
                .unwrap();
        assert!(outcome.ok(), "{:?}", outcome.violations);
        let queue = fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap();
        assert_eq!(queue.lines().rev().nth(1), Some("Recommendation: Proceed."));
    }
    let queue = fs::read(c.root.join("OWNER-QUEUE.md")).unwrap();
    let empty = ledger::owner_queue_add(
        &Fake,
        &c.root,
        &queue_fixture::item("Empty", "Recommendation: "),
    )
    .unwrap();
    assert_eq!(rules(&empty), vec!["ledger-field"]);
    assert_eq!(fs::read(c.root.join("OWNER-QUEUE.md")).unwrap(), queue);
}

#[test]
fn owner_queue_removal_write_failures_preserve_the_item() {
    for (path, rule) in [
        ("JOURNAL.md", "owner-queue-journal-write"),
        ("OWNER-QUEUE.md", "owner-queue-write"),
    ] {
        for action in ["answer", "strike"] {
            let c = queue_fixture::single_item(&Fake);
            let queue = fs::read(c.root.join("OWNER-QUEUE.md")).unwrap();
            let journal = fs::read(c.root.join("JOURNAL.md")).unwrap();
            let resume = fs::read(c.root.join("RESUME.md")).unwrap();
            queue_fixture::obstruct_write(&c.root, path);
            let body = queue_fixture::body_file(&c.root, queue_fixture::ANSWER);
            let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
            command
                .arg("-C")
                .arg(&c.root)
                .args(["--json", "owner-queue", action, "7"]);
            if action == "answer" {
                command.arg("--body-file").arg(body);
            } else {
                command.args(["--reason", "Superseded."]);
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{action} {path}: {output:?}");
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(json["ok"], false);
            assert_eq!(json["violations"].as_array().unwrap().len(), 1);
            assert_eq!(json["violations"][0]["rule"], rule);
            assert_eq!(json["violations"][0]["path"], path);
            assert_eq!(fs::read(c.root.join("OWNER-QUEUE.md")).unwrap(), queue);
            if path == "JOURNAL.md" {
                assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), journal);
            } else {
                let after = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
                let added = after
                    .strip_prefix(std::str::from_utf8(&journal).unwrap())
                    .unwrap();
                let reason = if action == "answer" {
                    format!("Answer:\n{}", queue_fixture::ANSWER.trim_end())
                } else {
                    "Reason: Superseded.".to_owned()
                };
                assert_eq!(
                    added.lines().skip(3).collect::<Vec<_>>().join("\n"),
                    format!(
                        "- OWNER-QUEUE.md: `## 7. Middle`\n\nRemoved item:\n\n{}\n{reason}",
                        queue_fixture::ARCHIVED_MIDDLE
                    )
                );
            }
            assert_eq!(fs::read(c.root.join("RESUME.md")).unwrap(), resume);
            let linted = lint(&c.root).unwrap();
            assert!(linted.ok(), "{:?}", linted.violations);
        }
    }
}

#[test]
fn owner_queue_resume_write_failure_reports_the_journaled_removal() {
    for action in ["answer", "strike"] {
        let c = queue_fixture::single_item(&Fake);
        let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        let resume = fs::read(c.root.join("RESUME.md")).unwrap();
        queue_fixture::obstruct_write(&c.root, "RESUME.md");
        let body = queue_fixture::body_file(&c.root, queue_fixture::ANSWER);
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        command
            .arg("-C")
            .arg(&c.root)
            .args(["--json", "owner-queue", action, "7"]);
        if action == "answer" {
            command.arg("--body-file").arg(body);
        } else {
            command.args(["--reason", "Superseded."]);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{action}: {output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["violations"].as_array().unwrap().len(), 1);
        assert_eq!(json["violations"][0]["rule"], "owner-queue-resume-write");
        assert_eq!(json["violations"][0]["path"], "RESUME.md");
        assert!(json["violations"][0]["message"].as_str().unwrap().starts_with(
            &format!("owner-queue {action} 7 failed; the item was already removed and journaled; updating RESUME.md failed: ")));
        assert_eq!(
            fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap(),
            format!("{}\n", cm::templates::OWNER_QUEUE)
        );
        assert_eq!(fs::read(c.root.join("RESUME.md")).unwrap(), resume);
        let after = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        let added = after.strip_prefix(journal.as_str()).unwrap();
        assert_eq!(
            added.lines().nth(1).unwrap().split_once(" — ").unwrap().1,
            format!("owner-queue {action} 7. Middle")
        );
        let reason = if action == "answer" {
            format!("Answer:\n{}", queue_fixture::ANSWER.trim_end())
        } else {
            "Reason: Superseded.".to_owned()
        };
        assert_eq!(
            added.lines().skip(3).collect::<Vec<_>>().join("\n"),
            format!(
                "- OWNER-QUEUE.md: `## 7. Middle`\n\nRemoved item:\n\n{}\n{reason}",
                queue_fixture::ARCHIVED_MIDDLE
            )
        );
    }
}

#[test]
fn owner_queue_numbers_increase_after_answering_the_highest_item_and_draining() {
    let c = queue_fixture::campaign(&Fake);
    assert!(
        ledger::owner_queue_answer(&Fake, &c.root, 11, queue_fixture::ANSWER)
            .unwrap()
            .ok()
    );
    let added =
        ledger::owner_queue_add(&Fake, &c.root, &queue_fixture::item("Next", "Yes.")).unwrap();
    assert!(added.ok(), "{:?}", added.violations);
    assert_eq!(added.fields["item"], 12);
    for number in [2, 7, 12] {
        assert!(
            ledger::owner_queue_strike(&Fake, &c.root, number, "Superseded.")
                .unwrap()
                .ok()
        );
    }
    assert!(!cm::templates::queue_open_in(&c.root).unwrap());
    let added =
        ledger::owner_queue_add(&Fake, &c.root, &queue_fixture::item("After drain", "Yes."))
            .unwrap();
    assert!(added.ok(), "{:?}", added.violations);
    assert_eq!(added.fields["item"], 13);
    assert!(lint(&c.root).unwrap().ok());
}

#[test]
fn owner_queue_add_uses_numbers_from_each_journaled_action() {
    for action in ["add", "answer", "strike"] {
        let c = queue_fixture::campaign(&Fake);
        queue_fixture::journal_number(&Fake, &c.root, action, 41);
        let added =
            ledger::owner_queue_add(&Fake, &c.root, &queue_fixture::item("Next", "Yes.")).unwrap();
        assert!(added.ok(), "{:?}", added.violations);
        assert_eq!(added.fields["item"], 42, "{action}");
    }
}

#[test]
fn owner_queue_add_refuses_exhausted_numbers_without_writes() {
    for source in ["queue", "add", "answer", "strike"] {
        let c = queue_fixture::campaign(&Fake);
        if source == "queue" {
            queue_fixture::renumber_last(&c.root, u32::MAX);
        } else {
            queue_fixture::journal_number(&Fake, &c.root, source, u32::MAX);
        }
        let paths = ["OWNER-QUEUE.md", "RESUME.md", "JOURNAL.md"];
        let before = paths.map(|path| fs::read(c.root.join(path)).unwrap());
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&c.root)
            .args([
                "--json",
                "owner-queue",
                "add",
                "Next",
                "--context",
                "A decision.",
                "--question",
                "Proceed?",
                "--recommendation",
                "Yes.",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{source}: {output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            json["violations"][0]["rule"],
            "owner-queue-number-exhausted"
        );
        assert_eq!(
            paths.map(|path| fs::read(c.root.join(path)).unwrap()),
            before
        );
    }
}

#[test]
fn owner_queue_edit_ignores_another_items_missing_fields() {
    let c = queue_fixture::campaign(&Fake);
    queue_fixture::without_first_context(&c.root);
    let before = fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap();
    let edited = ledger::owner_queue_edit(&Fake, &c.root, 7, queue_fixture::REPLACEMENT).unwrap();
    assert!(edited.ok(), "{:?}", edited.violations);
    assert_eq!(
        fs::read_to_string(c.root.join("OWNER-QUEUE.md")).unwrap(),
        before.replace(
            queue_fixture::MIDDLE,
            &format!("## 7. Middle\n\n{}\n\n", queue_fixture::REPLACEMENT)
        )
    );
    let linted = lint(&c.root).unwrap();
    assert_eq!(rules(&linted), vec!["owner-queue-field"]);
    assert_eq!(
        linted.violations[0].message,
        "item `2. First` has no line starting `Context:`"
    );
}

#[test]
fn evidence_refuses_an_empty_body() {
    let (_temp, root) = campaign();
    let declaration = Declaration {
        lane: Some(Lane::Owner),
        ..Declaration::default()
    };
    assert!(
        commands::declare(&Fake, &root.join("01-a/u/answer"), &declaration)
            .unwrap()
            .ok()
    );
    let step = parse_target("01-a/u/answer", true).unwrap();
    let write = |name: &str, body: &str| ledger::evidence(&Fake, &root, &step, name, body).unwrap();
    assert_eq!(rules(&write("ANSWER", " \n")), vec!["evidence-empty"]);
    assert_eq!(rules(&write("../x", "y")), vec!["evidence-name"]);
    let written = write("ANSWER", "The owner said yes.\n");
    assert!(written.ok(), "{:?}", written.violations);
    assert_eq!(written.fields["path"], "01-a/u/answer/evidence/ANSWER.md");
    assert_eq!(rules(&write("ANSWER", "again")), vec!["evidence-exists"]);
    let done = commands::advance(
        &Fake,
        &root,
        &step,
        &Phase::Done,
        &Evidence::default(),
        None,
        None,
    )
    .unwrap();
    assert!(done.ok(), "{:?}", done.violations);
    assert_eq!(
        done.fields["evidence"]["file"],
        "01-a/u/answer/evidence/ANSWER.md"
    );
    assert!(lint(&root).unwrap().ok());
}

const REVIEW_OFF: &str = "# Review-off\n\nWho reviewed what.\n\n\
| # | PR | Kind | Model | Verdict | Confirmed | False positive | Confirmed, missed by the other | Worst severity | Cost | Wall time | Effort reported |\n\
|---|---|---|---|---|---|---|---|---|---|---|---|\n\
| 9 | dekopon-provider-decide#4 @a00bcdf0 | release prep (d) | Fable 5.1 | ACCEPT | 0 required; 1 taste (#3's squash puts its `Changelog:` line mid-body, so trailer parsing misses it; confirmed in ca85c2c's body) | 0 | 1 (the trailer-parsing note) | taste | $1.10 | 36 s run (64 s turn) | not exposed (dekopon-rust) |\n";

fn declare_build(root: &Path) {
    let declaration = Declaration {
        lane: Some(Lane::ClaudeCloud),
        ..Declaration::default()
    };
    let target = root.join("15-machinery/cm/build");
    assert!(
        commands::declare(&Fake, &target, &declaration)
            .unwrap()
            .ok()
    );
}

fn review(root: &Path, pr: &str, delta: Option<&str>) -> Outcome {
    review_step(root, "15-machinery/cm/build", pr, delta)
}

fn review_step(root: &Path, step: &str, pr: &str, delta: Option<&str>) -> Outcome {
    let step = parse_target(step, true).unwrap();
    let args = ReviewArgs {
        pr,
        head: "1234567890abcdef",
        model: "claude-opus-5-5",
        effort: "high",
        verdict: "accept",
        findings: "2/1",
        cost: "3.25",
        wall: "12",
        delta,
    };
    review::review(&Fake, root, &step, &args).unwrap()
}

#[test]
fn review_rows_render_in_pr_order() {
    let (_temp, root) = campaign();
    declare_build(&root);
    fs::write(root.join("REVIEW-OFF.md"), REVIEW_OFF).unwrap();
    let imported = review::import_reviews(&Fake, &root).unwrap();
    assert!(imported.ok(), "{:?}", imported.violations);
    assert_eq!(
        fs::read_to_string(root.join("REVIEW-OFF.md")).unwrap(),
        REVIEW_OFF
    );
    assert_eq!(
        rules(&review::import_reviews(&Fake, &root).unwrap()),
        vec!["review-imported"]
    );
    for (pr, delta) in [
        ("https://github.com/dekopon-agents/cm/pull/31", None),
        ("https://github.com/dekopon-agents/cm/pull/30", None),
        (
            "https://github.com/dekopon-agents/cm/pull/30",
            Some("abcdef0123"),
        ),
    ] {
        let added = review(&root, pr, delta);
        assert!(added.ok(), "{:?}", added.violations);
    }
    let text = fs::read_to_string(root.join("REVIEW-OFF.md")).unwrap();
    let order: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("| 1") || line.starts_with("| 9"))
        .map(|line| line.split(" | ").next().unwrap())
        .collect();
    assert_eq!(order, vec!["| 11", "| 12", "| 10", "| 9"]);
    assert!(text.contains(
        "| 12 | cm#30 @12345678 (delta from abcdef01) | 15-machinery/cm/build (delta) |"
    ));
    assert!(lint(&root).unwrap().ok());
    fs::write(
        root.join("REVIEW-OFF.md"),
        format!("{text}| 13 | by hand |\n"),
    )
    .unwrap();
    assert_eq!(rules(&lint(&root).unwrap()), vec!["review-hand-edit"]);
    assert_eq!(
        rules(&review(
            &root,
            "https://github.com/dekopon-agents/cm/pull/32",
            None
        )),
        vec!["review-hand-edit"]
    );
}

const AFTER_TABLE: &str = "\n### PR 4: dekopon-provider-decide#4\n\nBoth reviewers accepted; the taste note rides the next PR.\n\n## Rules\n\n- One row per review.\n- A delta row sits under its PR.\n";

const DELTA_ROW: &str = "| 9Δ | dekopon-provider-decide#4 @b11cdf01 | release prep (d), delta | Fable 5.1 | ACCEPT | 0 | 0 | 0 | — | $0.20 | 9 s run | not exposed |\n";

#[test]
fn review_off_keeps_what_follows_the_table_byte_for_byte() {
    let (_temp, root) = campaign();
    declare_build(&root);
    let hand = format!("{REVIEW_OFF}{DELTA_ROW}{AFTER_TABLE}");
    fs::write(root.join("REVIEW-OFF.md"), &hand).unwrap();
    let added = review(&root, "https://github.com/dekopon-agents/cm/pull/30", None);
    assert!(added.ok(), "{:?}", added.violations);
    assert_eq!(added.fields["number"], 10);
    let text = fs::read_to_string(root.join("REVIEW-OFF.md")).unwrap();
    assert!(text.ends_with(AFTER_TABLE), "{text}");
    let rows: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("| 1") || line.starts_with("| 9"))
        .map(|line| line.split(" | ").next().unwrap())
        .collect();
    assert_eq!(rows, vec!["| 10", "| 9", "| 9Δ"]);
    assert!(lint(&root).unwrap().ok());
    let (_other, fresh) = campaign();
    fs::write(fresh.join("REVIEW-OFF.md"), &hand).unwrap();
    assert!(review::import_reviews(&Fake, &fresh).unwrap().ok());
    assert_eq!(
        fs::read_to_string(fresh.join("REVIEW-OFF.md")).unwrap(),
        hand
    );
}

#[test]
fn review_refuses_an_undeclared_step() {
    let (_temp, root) = campaign();
    let refused = review_step(
        &root,
        "77-nope/u/S9",
        "https://github.com/dekopon-agents/cm/pull/30",
        None,
    );
    assert_eq!(rules(&refused), vec!["step-unknown"]);
    assert!(!root.join("REVIEW-OFF.md").exists());
}

#[test]
fn cli_feedback_reads_its_body_from_stdin() {
    let (_temp, root) = campaign();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cm"))
        .args([
            "-C",
            root.to_str().unwrap(),
            "--json",
            "feedback",
            "a lesson",
        ])
        .args(["--urgency", "urgent", "--body-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"The body.\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["id"], "FB-d-001", "{json}");
    let text = fs::read_to_string(root.join("FEEDBACK.md")).unwrap();
    assert!(text.ends_with("The body.\n"));
}

#[test]
fn feedback_refuses_an_empty_body() {
    let (_temp, root) = campaign();
    for body in ["", " \n\n"] {
        let empty = Feedback {
            body,
            ..entry("a lesson", "urgent")
        };
        assert_eq!(
            rules(&ledger::feedback(&Fake, &root, &empty).unwrap()),
            vec!["feedback-empty"]
        );
    }
    assert!(!root.join("FEEDBACK.md").exists());
}

#[test]
fn an_unreadable_ledger_is_never_rewritten_from_nothing() {
    let (_temp, root) = campaign();
    let garbled = b"# Feedback\n\n### FB-d-001 \xff\xfe\n";
    fs::write(root.join("FEEDBACK.md"), garbled).unwrap();
    assert!(ledger::feedback(&Fake, &root, &entry("a lesson", "urgent")).is_err());
    assert_eq!(fs::read(root.join("FEEDBACK.md")).unwrap(), garbled);
    let queue = fs::read(root.join("OWNER-QUEUE.md")).unwrap();
    let mut broken = queue.clone();
    broken.extend_from_slice(b"\xff\n");
    fs::write(root.join("OWNER-QUEUE.md"), &broken).unwrap();
    let item = QueueItem {
        title: "t",
        context: "c",
        question: "q",
        recommendation: "r",
    };
    assert!(ledger::owner_queue_add(&Fake, &root, &item).is_err());
    assert_eq!(fs::read(root.join("OWNER-QUEUE.md")).unwrap(), broken);
    fs::write(root.join("OWNER-QUEUE.md"), &queue).unwrap();
    let resume = fs::read_to_string(root.join("RESUME.md")).unwrap();
    let bare = &resume[resume.find("# Resume").unwrap()..];
    fs::write(root.join("RESUME.md"), bare).unwrap();
    assert_eq!(
        rules(&ledger::owner_queue_add(&Fake, &root, &item).unwrap()),
        vec!["resume-banner"]
    );
    assert_eq!(fs::read(root.join("OWNER-QUEUE.md")).unwrap(), queue);
}

fn recover_reviews(root: &Path) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(root)
        .args(["review", "render", "--json"])
        .output()
        .unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

fn interrupted_review(root: &Path, obstruction: &str) {
    declare_build(root);
    assert!(review(root, "https://github.com/o/r/pull/1", None).ok());
    fs::create_dir(root.join(obstruction)).unwrap();
    let target = parse_target("15-machinery/cm/build", true).unwrap();
    let args = ReviewArgs {
        pr: "https://github.com/o/r/pull/2",
        head: "abcdef1",
        model: "m",
        effort: "high",
        verdict: "accept",
        findings: "0/0",
        cost: "1",
        wall: "1",
        delta: None,
    };
    assert!(review::review(&Fake, root, &target, &args).is_err());
    fs::remove_dir(root.join(obstruction)).unwrap();
}

fn review_entries(root: &Path) -> usize {
    fs::read_to_string(root.join("JOURNAL.md"))
        .unwrap()
        .lines()
        .filter(|line| {
            line.starts_with("## ") && line.contains(" — review 15-machinery/cm/build #")
        })
        .count()
}

#[test]
fn a_review_interrupted_after_json_can_be_recovered_before_appending() {
    let (_temp, root) = campaign();
    interrupted_review(&root, ".REVIEW-OFF.md.cm-tmp");
    assert_eq!(review_entries(&root), 1);
    assert_eq!(recover_reviews(&root)["ok"], true);
    assert!(review(&root, "https://github.com/o/r/pull/3", None).ok());
    let rows: review::Reviews = cm::store::read_json(&root.join(".cm/reviews.json")).unwrap();
    assert_eq!(rows.rows.len(), 3);
    assert_eq!(review_entries(&root), 3);
    assert!(lint(&root).unwrap().ok());
}

#[test]
fn a_review_interrupted_after_markdown_recovers_its_journal_entry() {
    let (_temp, root) = campaign();
    interrupted_review(&root, ".JOURNAL.md.cm-tmp");
    let markdown = fs::read(root.join("REVIEW-OFF.md")).unwrap();
    assert_eq!(review_entries(&root), 1);
    assert_eq!(recover_reviews(&root)["ok"], true);
    assert_eq!(review_entries(&root), 2);
    assert_eq!(fs::read(root.join("REVIEW-OFF.md")).unwrap(), markdown);
}

#[test]
fn repeated_review_recovery_converges_without_duplicate_rows_or_entries() {
    let (_temp, root) = campaign();
    interrupted_review(&root, ".REVIEW-OFF.md.cm-tmp");
    assert_eq!(recover_reviews(&root)["ok"], true);
    let paths = [".cm/reviews.json", "REVIEW-OFF.md", "JOURNAL.md"];
    let before = paths.map(|path| fs::read(root.join(path)).unwrap());
    assert_eq!(recover_reviews(&root)["ok"], true);
    assert_eq!(paths.map(|path| fs::read(root.join(path)).unwrap()), before);
    assert_eq!(review_entries(&root), 2);
}

#[test]
fn recovery_refuses_a_genuine_hand_edit_without_overwriting_it() {
    let (_temp, root) = campaign();
    interrupted_review(&root, ".REVIEW-OFF.md.cm-tmp");
    let path = root.join("REVIEW-OFF.md");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("ACCEPT", "FIX REQUIRED");
    fs::write(&path, &edited).unwrap();
    let result = recover_reviews(&root);
    assert_eq!(result["ok"], false);
    assert_eq!(result["violations"][0]["rule"], "review-hand-edit");
    assert_eq!(fs::read_to_string(path).unwrap(), edited);
    assert_eq!(review_entries(&root), 1);
}

#[test]
fn recovery_preserves_the_preamble_and_postamble() {
    let (_temp, root) = campaign();
    fs::write(
        root.join("REVIEW-OFF.md"),
        format!("{REVIEW_OFF}{AFTER_TABLE}"),
    )
    .unwrap();
    interrupted_review(&root, ".REVIEW-OFF.md.cm-tmp");
    assert_eq!(recover_reviews(&root)["ok"], true);
    let text = fs::read_to_string(root.join("REVIEW-OFF.md")).unwrap();
    assert!(text.starts_with("# Review-off\n\nWho reviewed what.\n\n"));
    assert!(text.ends_with(AFTER_TABLE));
}

#[test]
fn an_interrupted_first_review_render_leaves_the_append_retryable() {
    let (_temp, root) = campaign();
    declare_build(&root);
    let obstruction = root.join(".REVIEW-OFF.md.cm-tmp");
    fs::create_dir(&obstruction).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&root)
        .args([
            "review",
            "15-machinery/cm/build",
            "--pr",
            "https://github.com/o/r/pull/1",
            "--head",
            "abcdef1",
            "--model",
            "m",
            "--effort",
            "high",
            "--verdict",
            "accept",
            "--findings",
            "0/0",
            "--cost",
            "1",
            "--wall",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.join(".cm/reviews.json").exists());
    assert_eq!(review_entries(&root), 0);
    fs::remove_dir(obstruction).unwrap();
    assert!(review(&root, "https://github.com/o/r/pull/1", None).ok());
    assert_eq!(recover_reviews(&root)["ok"], true);
    let rows: review::Reviews = cm::store::read_json(&root.join(".cm/reviews.json")).unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(review_entries(&root), 1);
}

#[test]
fn an_interrupted_first_review_journal_entry_can_be_recovered() {
    let (_temp, root) = campaign();
    declare_build(&root);
    let obstruction = root.join(".JOURNAL.md.cm-tmp");
    fs::create_dir(&obstruction).unwrap();
    let target = parse_target("15-machinery/cm/build", true).unwrap();
    let args = ReviewArgs {
        pr: "https://github.com/o/r/pull/1",
        head: "abcdef1",
        model: "m",
        effort: "high",
        verdict: "accept",
        findings: "0/0",
        cost: "1",
        wall: "1",
        delta: None,
    };
    assert!(review::review(&Fake, &root, &target, &args).is_err());
    fs::remove_dir(obstruction).unwrap();
    assert_eq!(review_entries(&root), 0);
    assert_eq!(recover_reviews(&root)["ok"], true);
    assert_eq!(review_entries(&root), 1);
    assert!(lint(&root).unwrap().ok());
}

const MAC_FIXTURE: &str = include_str!("fixtures/mac-lane/MAC-LANE.md");

fn mac_lane(root: &Path) -> String {
    fs::read_to_string(root.join("MAC-LANE.md")).unwrap()
}

fn card(root: &Path) -> String {
    let next = cm::plan::Next {
        paused: false,
        ready: Vec::new(),
        waiting: Vec::new(),
    };
    cm::render::card(root, Fake.now(), "c", &Default::default(), &next, &[])
}

#[test]
fn mac_lane_add_numbers_on_and_indents_the_body() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = fixture.root.clone();
    assert_eq!(
        rules(&ledger::mac_lane_add(&Fake, &root, " ", "").unwrap()),
        vec!["ledger-field"]
    );
    assert_eq!(
        rules(&ledger::mac_lane_add(&Fake, &root, "a **b**", "").unwrap()),
        vec!["mac-lane-title"]
    );
    assert!(!root.join("MAC-LANE.md").exists());
    let added = ledger::mac_lane_add(&Fake, &root, "Mint the key", "").unwrap();
    assert!(added.ok(), "{:?}", added.violations);
    assert_eq!(added.fields["item"], 1);
    let added = ledger::mac_lane_add(
        &Fake,
        &root,
        "Re-save the script",
        "Paste it.\n- Read back its sha256.\n\n- Update the State row.\n",
    )
    .unwrap();
    assert_eq!(added.fields["item"], 2);
    let text = mac_lane(&root);
    assert!(text.starts_with("# Mac lane queue\n\n"));
    assert!(text.ends_with(
        ".\n\n1. **Mint the key**\n2. **Re-save the script** Paste it.\n    - Read back its sha256.\n\n    - Update the State row.\n"
    ));
    let journal = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("mac-lane add 2. Re-save the script"));
    fs::write(root.join("MAC-LANE.md"), MAC_FIXTURE).unwrap();
    let added = ledger::mac_lane_add(&Fake, &root, "Next", "").unwrap();
    assert_eq!(added.fields["item"], 8);
    assert_eq!(mac_lane(&root), format!("{MAC_FIXTURE}8. **Next**\n"));
    assert!(lint(&root).unwrap().ok());
}

#[test]
fn mac_lane_done_replaces_an_open_status_and_refuses_a_second_done() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = fixture.root.clone();
    assert_eq!(
        rules(&ledger::mac_lane_done(&Fake, &root, 1, "r").unwrap()),
        vec!["mac-lane-item"]
    );
    fs::write(root.join("MAC-LANE.md"), MAC_FIXTURE).unwrap();
    for (number, rule) in [
        (1, "mac-lane-closed"),
        (2, "mac-lane-closed"),
        (9, "mac-lane-item"),
    ] {
        assert_eq!(
            rules(&ledger::mac_lane_done(&Fake, &root, number, "r").unwrap()),
            vec![rule]
        );
    }
    assert_eq!(
        rules(&ledger::mac_lane_done(&Fake, &root, 5, "a\nb").unwrap()),
        vec!["ledger-field"]
    );
    assert_eq!(mac_lane(&root), MAC_FIXTURE);
    let done = ledger::mac_lane_done(&Fake, &root, 5, "script landed as abc1234").unwrap();
    assert!(done.ok(), "{:?}", done.violations);
    let held = "5. **HELD 2000-01-04; owner kept it held the same day** (starts";
    let stamped = "5. **DONE 2026-10-23** (script landed as abc1234). (starts";
    assert_eq!(mac_lane(&root), MAC_FIXTURE.replace(held, stamped));
    assert_eq!(
        rules(&ledger::mac_lane_done(&Fake, &root, 5, "again").unwrap()),
        vec!["mac-lane-closed"]
    );
    for number in [4, 6, 7] {
        assert!(
            ledger::mac_lane_done(&Fake, &root, number, "r")
                .unwrap()
                .ok()
        );
    }
    let text = mac_lane(&root);
    assert!(text.contains(
        "\n4. **DONE 2026-10-23** (r). The checks would never run. **After the sample restart"
    ));
    assert!(text.contains("\n6. **DONE 2026-10-23** (r). (owner go 17:44Z). **Second sample"));
    assert!(text.ends_with(
        "\n7. **DONE 2026-10-23** (r). **Store the sample configuration with the widget rule** (owner, 2000-01-04). Same steps as item 1.\n"
    ));
    let journal = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("mac-lane done 5. Build `tools/sample-widget.sh`"));
    assert!(lint(&root).unwrap().ok());
}

#[test]
fn mac_lane_list_shows_open_items_with_their_state() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = fixture.root.clone();
    let empty = ledger::mac_lane_list(&root).unwrap();
    assert_eq!(empty.lines, vec!["no open Mac lane item"]);
    fs::write(root.join("MAC-LANE.md"), MAC_FIXTURE).unwrap();
    let listed = ledger::mac_lane_list(&root).unwrap();
    assert_eq!(
        listed.lines,
        vec![
            "4. After the sample restart (after the dry run) (held)",
            "5. Build `tools/sample-widget.sh` (held)",
            "6. Second sample widget matrix (running)",
            "7. Store the sample configuration with the widget rule (open)",
        ]
    );
    assert_eq!(
        listed.to_json("mac-lane")["items"][1],
        serde_json::json!({"item": 5, "state": "held", "title": "Build `tools/sample-widget.sh`"})
    );
}

#[test]
fn the_card_lists_open_mac_lane_items_only_when_there_are_some() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = fixture.root.clone();
    assert!(!card(&root).contains("## Mac lane"));
    fs::write(root.join("MAC-LANE.md"), MAC_FIXTURE).unwrap();
    assert!(card(&root).contains(
        "\n## Mac lane\n\n4 open items\n\n- #4 After the sample restart (after the dry run) (held)\n- #5 Build `tools/sample-widget.sh` (held)\n- #6 Second sample widget matrix (running)\n- #7 Store the sample configuration with the widget rule (open)\n\n## Next\n"
    ));
    let done = MAC_FIXTURE.split("\n4. ").next().unwrap().to_owned() + "\n";
    fs::write(root.join("MAC-LANE.md"), done).unwrap();
    assert!(!card(&root).contains("## Mac lane"));
}

#[test]
fn close_refuses_an_open_mac_lane_item() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = fixture.root.clone();
    assert!(
        ledger::mac_lane_add(&Fake, &root, "Tick the setting", "")
            .unwrap()
            .ok()
    );
    assert_eq!(
        rules(&commands::close(&Fake, &root).unwrap()),
        vec!["closed-mac-lane"]
    );
    assert!(
        ledger::mac_lane_done(&Fake, &root, 1, "ticked")
            .unwrap()
            .ok()
    );
    assert!(commands::close(&Fake, &root).unwrap().ok());
}

#[test]
fn lint_checks_mac_lane_numbering_only() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = fixture.root.clone();
    fs::write(root.join("MAC-LANE.md"), MAC_FIXTURE).unwrap();
    assert!(lint(&root).unwrap().ok());
    fs::write(
        root.join("MAC-LANE.md"),
        "# Mac lane queue\n\nIntro.\n\n2. **A**\n3. **B**\n3. **C**\n5. **D**\n6. anything at all\n",
    )
    .unwrap();
    let linted = lint(&root).unwrap();
    assert_eq!(
        rules(&linted),
        vec!["mac-lane-number", "mac-lane-number", "mac-lane-number"]
    );
    let lines: Vec<_> = linted.violations.iter().map(|v| v.line).collect();
    assert_eq!(lines, vec![Some(5), Some(7), Some(8)]);
}

#[test]
fn mac_lane_add_refuses_titles_that_would_be_parsed_as_statuses() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    let journal = fs::read(root.join("JOURNAL.md")).unwrap();
    for title in [
        "DONE checklist",
        "WITHDRAWN sample",
        "HELD request",
        "RUNNING example",
        "Receipt for sample",
    ] {
        let added = ledger::mac_lane_add(&Fake, root, title, "").unwrap();
        assert_eq!(rules(&added), vec!["mac-lane-title"], "{title}");
        assert!(!root.join("MAC-LANE.md").exists());
        assert_eq!(fs::read(root.join("JOURNAL.md")).unwrap(), journal);
    }
}

#[test]
fn mac_lane_number_exhaustion_is_reported_without_writing() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    let text = "# Mac lane queue\n\n4294967295. **Sample**\n1. **Another**\n";
    fs::write(root.join("MAC-LANE.md"), text).unwrap();
    let journal = fs::read(root.join("JOURNAL.md")).unwrap();
    let linted = ledger::lint_mac_lane(root);
    assert_eq!(linted.len(), 2);
    assert!(linted.iter().all(|v| v.rule == "mac-lane-number"));
    let added = ledger::mac_lane_add(&Fake, root, "Next", "").unwrap();
    assert_eq!(rules(&added), vec!["mac-lane-number"]);
    assert_eq!(mac_lane(root), text);
    assert_eq!(fs::read(root.join("JOURNAL.md")).unwrap(), journal);
}

#[test]
fn unreadable_mac_lane_reports_errors_and_renders_lint_guidance() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    let path = root.join("MAC-LANE.md");
    let state = fs::read(root.join(".cm/state.json")).unwrap();
    let journal = fs::read(root.join("JOURNAL.md")).unwrap();
    for directory in [false, true] {
        if directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"# Mac lane queue\n\n1. **Pending** \xff\n").unwrap();
        }
        let error = ledger::mac_lane_open(root).err().unwrap();
        assert!(error.downcast_ref::<io::Error>().is_some());
        assert!(ledger::mac_lane_list(root).is_err());
        assert!(
            card(root)
                .lines()
                .any(|line| line == "Mac lane: invalid, see cm lint")
        );
        let linted = lint(root).unwrap();
        assert_eq!(rules(&linted), vec!["mac-lane-unreadable"]);
        assert_eq!(linted.violations[0].path, "MAC-LANE.md");
        let closed = commands::close(&Fake, root).unwrap();
        assert_eq!(rules(&closed), vec!["mac-lane-unreadable"]);
        assert_eq!(fs::read(root.join(".cm/state.json")).unwrap(), state);
        assert_eq!(fs::read(root.join("JOURNAL.md")).unwrap(), journal);
        if directory {
            fs::remove_dir(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
    }
}

#[test]
fn unreadable_owner_queue_blocks_banner_lint_and_close() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    let state = fs::read(root.join(".cm/state.json")).unwrap();
    let journal = fs::read(root.join("JOURNAL.md")).unwrap();
    fs::write(
        root.join("OWNER-QUEUE.md"),
        b"# Owner queue\n\n## Pending\n\xff\n",
    )
    .unwrap();
    assert_eq!(
        cm::templates::queue_open_in(root).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(cm::templates::Banner::of(root, false).is_err());
    assert!(
        lint(root)
            .unwrap()
            .violations
            .iter()
            .any(|v| v.rule == "queue-unreadable" && v.path == "OWNER-QUEUE.md")
    );
    assert_eq!(
        rules(&commands::close(&Fake, root).unwrap()),
        vec!["queue-unreadable"]
    );
    assert_eq!(fs::read(root.join(".cm/state.json")).unwrap(), state);
    assert_eq!(fs::read(root.join("JOURNAL.md")).unwrap(), journal);
}

#[test]
fn mac_lane_prose_and_indented_steps_are_item_text() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    for prose in [
        "2000 was the year (see below).",
        "   2000-01-02: sample continuation.",
        "1.5 GiB left",
        "   1. step",
        " 1. indented request",
        "  2. indented request",
        "  999999999999999999999999999999. indented request",
        "2x. sample",
        "-2. sample",
        "2) sample",
        "2.sample",
    ] {
        let text = format!(
            "# Mac lane queue\n\n1. **Request**\n{prose}\n2. **DONE 2000-01-01** **Earlier**\n"
        );
        fs::write(root.join("MAC-LANE.md"), &text).unwrap();
        assert!(lint(root).unwrap().ok(), "{prose}");
        let listed = ledger::mac_lane_list(root).unwrap();
        assert_eq!(
            listed.fields["items"],
            serde_json::json!([{"item": 1, "state": "open", "title": "Request"}]),
            "{prose}"
        );
        let added = ledger::mac_lane_add(&Fake, root, "Next", "").unwrap();
        assert_eq!(added.fields["item"], 3, "{prose}");
        assert_eq!(mac_lane(root), format!("{text}3. **Next**\n"));
    }
}

#[test]
fn overflowing_mac_lane_numbers_are_reported_before_any_write() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    let state = fs::read(root.join(".cm/state.json")).unwrap();
    let journal = fs::read(root.join("JOURNAL.md")).unwrap();
    for heading in [
        "4294967296. **Pending**",
        "999999999999999999999999999999. **Pending**",
        "4294967296.",
    ] {
        let text = format!("# Mac lane queue\n\n1. **DONE 2000-01-01** **Earlier**\n{heading}\n");
        fs::write(root.join("MAC-LANE.md"), &text).unwrap();
        let linted = lint(root).unwrap();
        assert_eq!(rules(&linted), vec!["mac-lane-number"], "{heading}");
        assert_eq!(linted.violations[0].line, Some(4));
        let listed = ledger::mac_lane_list(root).unwrap();
        assert_eq!(rules(&listed), vec!["mac-lane-number"]);
        assert!(
            card(root)
                .lines()
                .any(|line| line == "Mac lane: invalid, see cm lint")
        );
        assert_eq!(
            rules(&commands::close(&Fake, root).unwrap()),
            vec!["mac-lane-number"]
        );
        assert_eq!(
            rules(&ledger::mac_lane_add(&Fake, root, "Next", "").unwrap()),
            vec!["mac-lane-number"]
        );
        assert_eq!(
            rules(&ledger::mac_lane_done(&Fake, root, 1, "receipt").unwrap()),
            vec!["mac-lane-number"]
        );
        assert_eq!(mac_lane(root), text);
        assert_eq!(fs::read(root.join(".cm/state.json")).unwrap(), state);
        assert_eq!(fs::read(root.join("JOURNAL.md")).unwrap(), journal);
    }
}

#[test]
fn close_names_every_open_mac_lane_item_and_keeps_the_numbers() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    fs::write(
        root.join("MAC-LANE.md"),
        "# Mac lane queue\n\n1. **First request**\n2. **Second request**\n",
    )
    .unwrap();
    let closed = commands::close(&Fake, root).unwrap();
    assert_eq!(rules(&closed), vec!["closed-mac-lane"; 2]);
    for (index, title) in ["First request", "Second request"].iter().enumerate() {
        let violation = &closed.violations[index];
        assert_eq!(violation.line, Some(index + 3));
        assert!(
            violation
                .message
                .contains(&format!("item #{} ({title})", index + 1))
        );
        assert!(violation.message.contains(&format!(
            "cm mac-lane done {} --receipt <receipt>",
            index + 1
        )));
        assert!(
            violation
                .message
                .contains("**WITHDRAWN <date>: carried to <successor>**")
        );
        assert!(violation.message.contains("keeping its number in place"));
    }
    assert!(
        ledger::mac_lane_done(&Fake, root, 1, "finished")
            .unwrap()
            .ok()
    );
    let text = mac_lane(root).replace(
        "2. **Second request**",
        "2. **WITHDRAWN 2000-01-01: carried to sample successor** **Second request**",
    );
    fs::write(root.join("MAC-LANE.md"), text).unwrap();
    assert!(ledger::lint_mac_lane(root).is_empty());
    assert!(commands::close(&Fake, root).unwrap().ok());
}

#[test]
fn card_bounds_mac_lane_rows_and_normalizes_titles() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    let long = "é".repeat(100);
    let mut text =
        format!("# Mac lane queue\n\n1. **One\tTwo\u{001b}Three\nFour**\n2. **{long}**\n");
    for number in 3..=15 {
        text.push_str(&format!("{number}. **Request {number}**\n"));
    }
    fs::write(root.join("MAC-LANE.md"), text).unwrap();
    let rendered = card(root);
    let section = rendered
        .split("## Mac lane\n\n")
        .nth(1)
        .unwrap()
        .split("\n## Next")
        .next()
        .unwrap();
    assert!(section.starts_with("15 open items\n\n"));
    let rows: Vec<_> = section
        .lines()
        .filter(|line| line.starts_with("- #"))
        .collect();
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0], "- #1 One Two Three Four (open)");
    assert_eq!(rows[1], format!("- #2 {} (open)", "é".repeat(80)));
    assert_eq!(rows[9], "- #10 Request 10 (open)");
    assert!(
        section
            .lines()
            .any(|line| line == "- … and 5 more: see MAC-LANE.md")
    );
}

#[test]
fn mac_lane_done_on_empty_headings_keeps_them_parseable_and_prints_the_number() {
    let fixture = campaign_fixture::campaign(&Fake);
    let root = &fixture.root;
    for heading in [
        "1.",
        "1.\n",
        "1.\r\n",
        "1. \n",
        "1. **HELD 2000-01-01**\n",
        "1. **RUNNING 2000-01-01**   \n",
        "1. **RUNNING 2000-01-01**   \n    Sample body.\n",
    ] {
        fs::write(
            root.join("MAC-LANE.md"),
            format!("# Mac lane queue\n\n{heading}"),
        )
        .unwrap();
        let done = ledger::mac_lane_done(&Fake, root, 1, "receipt").unwrap();
        assert!(done.ok(), "{heading:?}: {:?}", done.violations);
        assert_eq!(done.lines, vec!["1: **DONE 2026-10-23** (receipt)."]);
        let text = mac_lane(root);
        assert_eq!(
            text.lines().nth(2),
            Some("1. **DONE 2026-10-23** (receipt).")
        );
        let items = ledger::mac_lane_items(&text).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, ledger::MacState::Done, "{heading:?}");
        assert!(ledger::lint_mac_lane(root).is_empty());
        assert!(
            done.fields["journal"]
                .as_str()
                .unwrap()
                .ends_with("mac-lane done 1")
        );
    }
}
