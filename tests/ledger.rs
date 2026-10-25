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
