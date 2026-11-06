use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use cm::commands::{self, Declaration};
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::state::Lane;
use jiff::Timestamp;

struct Fake {
    now: Timestamp,
}

impl Host for Fake {
    fn now(&self) -> Timestamp {
        self.now
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(100 << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(0)
    }
}

fn at(now: &str) -> Fake {
    Fake {
        now: now.parse().unwrap(),
    }
}

fn rules(outcome: &Outcome) -> Vec<&'static str> {
    let mut rules: Vec<_> = outcome.violations.iter().map(|v| v.rule).collect();
    rules.sort_unstable();
    rules
}

#[path = "support/campaign.rs"]
mod campaign_fixture;
use campaign_fixture::{Campaign, campaign, campaign_outside_git};

fn declare(host: &Fake, c: &Campaign, target: &str, lane: Lane, after: &[&str]) -> Outcome {
    let declaration = Declaration {
        lane: Some(lane),
        after: after.iter().map(|text| (*text).to_owned()).collect(),
        ..Declaration::default()
    };
    let outcome = commands::declare(host, &c.root.join(target), &declaration).unwrap();
    c.kickoff(target, "Go.\n");
    outcome
}

#[test]
fn lint_rejects_a_missing_unit_state() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Codex, &[]).ok());
    assert!(declare(&host, &c, "02-b/v/S1", Lane::Codex, &[]).ok());
    assert_eq!(lint(&c.root).unwrap().fields["units"], 2);
    fs::remove_file(c.root.join("02-b/v/.cm/state.json")).unwrap();
    let linted = lint(&c.root).unwrap();
    assert_eq!(rules(&linted), vec!["unit-state-json-missing"]);
    assert_eq!(linted.violations[0].path, "02-b/v/.cm/state.json");
}

#[test]
fn lint_reports_a_folder_git_ignores() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&c.root)
            .status()
            .unwrap()
            .success()
    );
    assert!(declare(&host, &c, "21-gh-parity/gh/build", Lane::Codex, &[]).ok());
    assert!(lint(&c.root).unwrap().ok());
    fs::write(c.root.join(".gitignore"), "**/build/\n").unwrap();
    let linted = lint(&c.root).unwrap();
    assert_eq!(rules(&linted), vec!["git-ignored"]);
    assert_eq!(linted.violations[0].path, "21-gh-parity/gh/build");
}

#[test]
fn packets_check_only_tokens_that_look_like_files() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign(&host);
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Codex, &[]).ok());
    fs::write(
        c.root.join("01-a/u/S1/kickoff.md"),
        "After `01-a/u/S2` and `00-release/cm/0.5.0`, see `https://github.com/o/r/pull/1`,\n\
         call `/v1/sessions/abc` and `/repos/o/r/pulls/7`; read `01-a/u/NOPE.md` and `/var/nope/x`.\n",
    )
    .unwrap();
    let linted = lint(&c.root).unwrap();
    let flagged: Vec<&str> = linted
        .violations
        .iter()
        .map(|violation| violation.message.as_str())
        .collect();
    assert_eq!(
        flagged,
        vec![
            "`01-a/u/NOPE.md` does not exist",
            "`/var/nope/x` does not exist"
        ]
    );
}

#[test]
fn lint_cannot_clear_ignored_folders_outside_git() {
    let host = at("2026-10-19T09:00:00Z");
    let c = campaign_outside_git(&host);
    assert!(lint(&c.root).unwrap().ok());
    assert!(declare(&host, &c, "01-a/u/S1", Lane::Codex, &[]).ok());
    let linted = lint(&c.root).unwrap();
    assert_eq!(rules(&linted), vec!["git-ignored-unknown"]);
    assert!(linted.lines[0].contains("1 violation"));
}

#[test]
fn lint_warns_when_kickoff_names_more_parts_than_declared() {
    let host = at("2026-10-28T09:00:00Z");
    for label in ["Parts", "Slices"] {
        let c = campaign(&host);
        let target = "01-a/u/S1";
        assert!(declare(&host, &c, target, Lane::Codex, &[]).ok());
        fs::write(
            c.root.join(target).join("kickoff.md"),
            format!("{label}: b1, b2, b3\n"),
        )
        .unwrap();
        for count in 0..=3 {
            let outcome = lint(&c.root).unwrap();
            assert!(outcome.ok(), "{:?}", outcome.violations);
            let warnings = outcome.fields["warnings"].as_array().unwrap();
            assert_eq!(
                warnings
                    .iter()
                    .filter(|warning| warning.as_str().unwrap().starts_with("parts-undeclared:"))
                    .count(),
                usize::from(count < 3)
            );
            if count < 3 {
                let amendment = commands::Amendment {
                    parts: vec![format!("b{}", count + 1)],
                    ..Default::default()
                };
                assert!(
                    commands::amend(
                        &host,
                        &c.root,
                        &cm::store::parse_target(target, false).unwrap(),
                        &amendment
                    )
                    .unwrap()
                    .ok()
                );
            }
        }
    }
}

#[test]
fn kickoff_parts_split_only_at_top_level_commas() {
    let host = at("2026-10-28T09:00:00Z");
    for label in ["Parts", "Slices"] {
        for (names, named) in [
            ("a (x, y), b", 2),
            ("a [x, y], b", 2),
            ("`a, x`, b", 2),
            ("``a `, x` ``, b", 2),
            ("a (x [y, z], w), b", 2),
            ("a [x (y, z), w], b", 2),
            ("`pr1, pr2, pr3", 3),
            ("pr1 (draft, pr2, pr3", 3),
            ("pr1 [draft, pr2, pr3", 3),
            ("pr1 `--flag, pr2, pr3", 3),
        ] {
            let c = campaign(&host);
            let target = "01-a/u/S1";
            assert!(declare(&host, &c, target, Lane::Codex, &[]).ok());
            c.kickoff(target, &format!("{label}: {names}\n"));
            for declared in 1..=named {
                assert!(
                    commands::amend(
                        &host,
                        &c.root,
                        &cm::store::parse_target(target, false).unwrap(),
                        &commands::Amendment {
                            parts: vec![format!("p{declared}")],
                            ..Default::default()
                        },
                    )
                    .unwrap()
                    .ok()
                );
                let outcome = lint(&c.root).unwrap();
                assert!(outcome.ok(), "{:?}", outcome.violations);
                let expected = if declared < named {
                    vec![format!(
                        "parts-undeclared: {target}: kickoff names {named} parts, but {declared} are declared; use --part or launch --no-parts REASON"
                    )]
                } else {
                    Vec::new()
                };
                assert_eq!(
                    outcome.fields["warnings"],
                    serde_json::json!(expected),
                    "{label}: {names}"
                );
            }
        }
    }
}

#[test]
fn ledger_size_warns_only_for_large_tracked_campaign_files() {
    let host = at("2026-10-28T09:00:00Z");
    let c = campaign(&host);
    for (name, bytes, tracked) in [
        ("evidence/large.txt", (1 << 20) + 1, true),
        ("evidence/also large.txt", 2 << 20, true),
        ("JOURNAL.md", (1 << 20) + 1, true),
        (".cm/evidence.bin", 2 << 20, true),
        ("nested/.cm/evidence.bin", 2 << 20, true),
        ("nested/JOURNAL.md", 2 << 20, true),
        ("evidence/exact.txt", 1 << 20, true),
        ("evidence/untracked.txt", 2 << 20, false),
    ] {
        c.sized_file(name, bytes, tracked);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["lint", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["warnings"],
        serde_json::json!([
            "ledger-size: evidence/also large.txt exceeds 1 MiB; keep large evidence files outside the campaign",
            "ledger-size: evidence/large.txt exceeds 1 MiB; keep large evidence files outside the campaign",
        ])
    );
    fs::remove_dir_all(c.root.join(".git")).unwrap();
    assert_eq!(
        lint(&c.root).unwrap().fields["warnings"],
        serde_json::json!([])
    );
}
