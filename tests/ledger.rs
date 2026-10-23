use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cm::commands::{self, Declaration};
use cm::host::Host;
use cm::ledger::{self, Feedback, QueueItem};
use cm::lint::lint;
use cm::outcome::Outcome;
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
