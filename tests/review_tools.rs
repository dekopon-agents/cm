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
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stdout)))
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn repo(dir: &Path, files: &[(&str, &str)]) -> String {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    for (path, body) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "fixture"]);
    git(dir, &["rev-parse", "HEAD"])
}

fn big_markdown() -> String {
    let mut text = String::from("# Design\n\n");
    for section in 1..=4 {
        text.push_str(&format!("## Section {section}\n\n"));
        for n in 0..40 {
            text.push_str(&format!(
                "Line {n} of section {section}, padded with enough words to grow past the limit.\n"
            ));
        }
    }
    text
}

fn big_rust() -> String {
    let mut text = String::from("use std::fmt;\n\n");
    for n in 0..150 {
        text.push_str(&format!(
            "pub fn item_{n}(value: usize) -> usize {{\n    let doubled = value * 2;\n    doubled + {n}\n}}\n\n"
        ));
    }
    text
}

#[test]
fn read_prints_pins_never_the_working_tree() {
    let temp = tempfile::tempdir().unwrap();
    let src = temp.path().join("src");
    let sha = repo(
        &src,
        &[
            ("small.md", "# Small\n\nfirst\nsecond\n"),
            ("big.md", &big_markdown()),
            ("lib.rs", &big_rust()),
        ],
    );
    fs::write(src.join("small.md"), "# Edited in the working tree\n").unwrap();

    let whole = cm(temp.path(), &["read", &format!("src@{sha}:small.md")]);
    assert_eq!(whole.status.code(), Some(0), "{whole:?}");
    let out = text(&whole);
    assert!(out.contains("     1\t# Small\n"), "{out}");
    assert!(out.contains("     4\tsecond\n"), "{out}");
    assert!(!out.contains("Edited"), "{out}");

    let outline = text(&cm(temp.path(), &["read", &format!("src@{sha}:big.md")]));
    assert!(
        outline.contains("over 8 KB, so an outline of Markdown headings"),
        "{outline}"
    );
    assert!(outline.contains(" B, 170 lines"), "{outline}");
    assert!(outline.contains("\t## Section 3\n"), "{outline}");
    assert!(!outline.contains("Line 0 of section 1"), "{outline}");

    let items = text(&cm(temp.path(), &["read", &format!("src@{sha}:lib.rs")]));
    assert!(items.contains("an outline of item definitions"), "{items}");
    assert!(
        items.contains("     3\tpub fn item_0(value: usize) -> usize {\n"),
        "{items}"
    );
    assert!(!items.contains("doubled"), "{items}");

    let many = text(&cm(
        temp.path(),
        &[
            "read",
            &format!("src@{sha}:lib.rs:4-5"),
            &format!("src@{sha}:big.md:#Section 2"),
        ],
    ));
    assert!(
        many.contains("     4\t    let doubled = value * 2;\n     5\t    doubled + 0\n"),
        "{many}"
    );
    assert!(many.contains("lines 45-86"), "{many}");
    assert!(many.contains("\t## Section 2\n"), "{many}");
    assert!(!many.contains("## Section 3"), "{many}");
}

#[test]
fn read_resolves_aliases_refuses_off_pin_and_names_cuts() {
    let temp = tempfile::tempdir().unwrap();
    let src = temp.path().join("src");
    let first = repo(&src, &[("lib.rs", &big_rust())]);
    fs::write(src.join("later.md"), "# Later\n").unwrap();
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "later"]);
    let pins = format!(
        "# Facts\n\n## Pins\n\n| Alias | Repository | Commit |\n|---|---|---|\n| core | {} | {first} |\n",
        src.display()
    );
    fs::write(temp.path().join("FACTS.md"), pins).unwrap();

    let aliased = cm(
        temp.path(),
        &["read", "--pins", "FACTS.md", "core:lib.rs:3"],
    );
    assert_eq!(aliased.status.code(), Some(0), "{aliased:?}");
    assert!(text(&aliased).contains("     3\tpub fn item_0"));

    let head = git(&src, &["rev-parse", "HEAD"]);
    let refused = cm(
        temp.path(),
        &[
            "read",
            "--json",
            "--pins",
            "FACTS.md",
            &format!("src@{head}:later.md"),
            "core:later.md",
            "nope:lib.rs",
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    let rules: Vec<String> = json(&refused)["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| format!("{} {}", v["rule"], v["path"]))
        .collect();
    assert_eq!(
        rules,
        vec![
            format!("\"read-spec\" \"src@{head}:later.md\""),
            "\"read-missing\" \"core:later.md\"".to_owned(),
            "\"read-spec\" \"nope:lib.rs\"".to_owned(),
        ]
    );

    let mut huge = String::new();
    for n in 0..4000 {
        huge.push_str(&format!(
            "line {n} of a long file, long enough to need a cap\n"
        ));
    }
    let sha = repo(&temp.path().join("huge"), &[("log.txt", &huge)]);
    let capped = cm(
        temp.path(),
        &[
            "read",
            &format!("huge@{sha}:log.txt:1-4000"),
            &format!("huge@{sha}:log.txt:1-2"),
        ],
    );
    let out = text(&capped);
    assert!(out.len() < 66 * 1024, "{}", out.len());
    assert!(
        out.contains(&format!("[cut: huge@{sha}:log.txt:1-4000 lines ")),
        "{out}"
    );
    assert!(
        out.contains(&format!("[cut: huge@{sha}:log.txt:1-2 not shown")),
        "{out}"
    );
    assert!(out.contains("Cut by the 64 KB cap: "), "{out}");
}

#[test]
fn facts_verifies_pins_and_marks_stale_citations() {
    let temp = tempfile::tempdir().unwrap();
    let core = temp.path().join("core");
    let sha = repo(
        &core,
        &[
            ("crates/gw/src/gateway.rs", &big_rust()),
            ("crates/gw/src/lib.rs", "pub mod gateway;\n"),
            ("crates/other/src/lib.rs", "pub fn other() {}\n"),
            ("docs/design.md", "# Design\n\n## Boot\n\nboots\n"),
        ],
    );
    let ideas = temp.path().join("ideas");
    fs::create_dir_all(&ideas).unwrap();
    fs::write(
        ideas.join("IDEA-0001-sample.md"),
        "---\nid: IDEA-0001\nmaturity: exploring\n---\n\n# Sample\n\nThe tick lives at `gateway.rs:9-10`, boot in docs/design.md:5.\nGone: `crates/gw/src/wake.rs:3`, past the end: gateway.rs:9999.\nBoth `lib.rs:1` files, and [design](docs/design.md) is a link, as is https://example.com/a/b.rs.\n",
    )
    .unwrap();
    let pin = format!("core={}@{sha}", core.display());

    let missing = cm(
        temp.path(),
        &[
            "idea",
            "facts",
            "--ideas",
            "ideas",
            "--pin",
            &pin,
            "--pin",
            &format!("gone={}@deadbeef0", core.display()),
            "--out",
            "FACTS.md",
            "IDEA-0001",
        ],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        text(&missing).contains("gone: pin-missing: commit deadbeef0"),
        "{missing:?}"
    );
    assert!(!temp.path().join("FACTS.md").exists());

    let written = cm(
        temp.path(),
        &[
            "idea",
            "facts",
            "--json",
            "--ideas",
            "ideas",
            "--pin",
            &pin,
            "--out",
            "FACTS.md",
            "IDEA-0001",
        ],
    );
    assert_eq!(written.status.code(), Some(0), "{written:?}");
    let result = json(&written);
    assert_eq!(result["citations"], 5);
    assert_eq!(result["stale"], 2);
    assert_eq!(result["ambiguous"], 1);
    let facts = fs::read_to_string(temp.path().join("FACTS.md")).unwrap();
    assert!(
        facts.contains(&format!("| core | {} | {sha} |", core.display())),
        "{facts}"
    );
    assert!(facts.contains("- L8 `gateway.rs:9-10` — core `crates/gw/src/gateway.rs` ok: :9-10 in `pub fn item_1(value: usize) -> usize {`"), "{facts}");
    assert!(
        facts.contains("- L8 `docs/design.md:5` — core `docs/design.md` ok: :5 in `## Boot`"),
        "{facts}"
    );
    assert!(
        facts.contains(
            "- L9 `crates/gw/src/wake.rs:3` — STALE: no `crates/gw/src/wake.rs` at any pin"
        ),
        "{facts}"
    );
    assert!(facts.contains("- L9 `gateway.rs:9999` — core `crates/gw/src/gateway.rs` STALE: :9999 past EOF (752 lines)"), "{facts}");
    assert!(facts.contains("- L10 `lib.rs:1` — ambiguous, 2 matches:\n  - core `crates/gw/src/lib.rs` ok: :1 in `pub mod gateway;`\n  - core `crates/other/src/lib.rs` ok: :1 in `pub fn other() {}`"), "{facts}");
    assert!(!facts.contains("example.com"), "{facts}");

    let read = cm(
        temp.path(),
        &["read", "--pins", "FACTS.md", "core:docs/design.md"],
    );
    assert!(text(&read).contains("     3\t## Boot"), "{read:?}");
}

#[test]
fn idents_flag_the_r15_env_var_that_only_appears_inside_a_longer_one() {
    let temp = tempfile::tempdir().unwrap();
    let ideas = temp.path().join("ideas");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/idents");
    let record = "IDEA-0151-claude-subscription-gateway-credentials.md";
    let linked = "IDEA-0138-claude-subscription-and-api-key-runs.md";
    let base = repo(
        &ideas,
        &[
            (record, &fs::read_to_string(fixtures.join(record)).unwrap()),
            (linked, &fs::read_to_string(fixtures.join(linked)).unwrap()),
        ],
    );
    let mut changed = fs::read_to_string(ideas.join(record)).unwrap();
    changed.push_str(&fs::read_to_string(fixtures.join("IDEA-0151-added-line.md")).unwrap());
    changed.push_str("- 2026-11-12: see IDEA-0138:19 and `ANTHROPIC_BASE_URL`.\n");
    fs::write(ideas.join(record), changed).unwrap();

    let output = cm(
        temp.path(),
        &[
            "idea",
            "idents",
            "--ideas",
            "ideas",
            "--base",
            &base,
            "IDEA-0151",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let out = text(&output);
    assert!(out.contains("linked: IDEA-0138"), "{out}");
    assert!(
        out.contains("MISMATCH  `DISABLE_NONESSENTIAL_TRAFFIC` (code, L24): only inside `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` at IDEA-0138:19"),
        "{out}"
    );
    assert!(
        out.contains("exact     `ANTHROPIC_BASE_URL` (code, L25): IDEA-0138:19"),
        "{out}"
    );
    assert!(
        out.contains(
            "ref       IDEA-0138:19 (L25): ok\n          IDEA-0138:19: | Subscription credential |"
        ),
        "{out}"
    );

    let result = json(&cm(
        temp.path(),
        &[
            "idea",
            "idents",
            "--json",
            "--ideas",
            "ideas",
            "--base",
            &base,
            "IDEA-0151",
        ],
    ));
    assert_eq!(result["mismatches"], 1);
}

#[test]
fn split_history_moves_ruled_reviews_once() {
    let temp = tempfile::tempdir().unwrap();
    let ideas = temp.path().join("ideas");
    fs::create_dir_all(&ideas).unwrap();
    let review = format!(
        "## Astra review — 2026-11-12 (gpt-6-astra)\n\n**Verdict: FIX REQUIRED.** Two blockers remain open.\n\n{}\n",
        "Finding text. ".repeat(40)
    );
    let record = format!(
        "---\nid: IDEA-0002\nmaturity: exploring\n---\n\n# Two\n\n{review}## Owner rulings on the Astra review — 2026-11-13\n\nRuled.\n\n## Epoch history\n\n- 2026-11-13: ruled.\n"
    );
    fs::write(ideas.join("IDEA-0002-two.md"), &record).unwrap();
    fs::write(ideas.join("README.md"), "# Not a record\n").unwrap();
    let args = [
        "idea",
        "split-history",
        "--ideas",
        "ideas",
        "--min-bytes",
        "100",
        "--date",
        "2026-11-13",
    ];

    let dry = cm(temp.path(), &args);
    assert_eq!(
        text(&dry),
        format!(
            "IDEA-0002: {} B, moving 1 section(s), {} chars\n",
            record.len(),
            review.chars().count()
        )
    );
    assert_eq!(
        fs::read_to_string(ideas.join("IDEA-0002-two.md")).unwrap(),
        record
    );

    let mut apply = args.to_vec();
    apply.push("--apply");
    assert_eq!(cm(temp.path(), &apply).status.code(), Some(0));
    let after = fs::read_to_string(ideas.join("IDEA-0002-two.md")).unwrap();
    assert!(after.contains("## Astra review — 2026-11-12 (gpt-6-astra)\n\nMoved to [history/IDEA-0002.md](history/IDEA-0002.md#astra-review--2026-11-12-gpt-6-astra) on 2026-11-13, after the owner ruled on it. Verdict: FIX REQUIRED.\n\n## Owner rulings"), "{after}");
    let history = fs::read_to_string(ideas.join("history/IDEA-0002.md")).unwrap();
    assert!(history.starts_with("# IDEA-0002: review history\n\nReview sections moved out of [the record](../IDEA-0002-two.md)"), "{history}");
    assert!(
        history.ends_with(&format!("\n\n{}\n", review.trim_end_matches('\n'))),
        "{history}"
    );

    let again = cm(temp.path(), &apply);
    assert_eq!(text(&again), "");
    assert_eq!(
        fs::read_to_string(ideas.join("IDEA-0002-two.md")).unwrap(),
        after
    );
}
