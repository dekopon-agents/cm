use std::fs;
use std::io;
use std::path::{Path, PathBuf};
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

struct Campaign {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

fn campaign(host: &Fake) -> Campaign {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    assert!(commands::init(host, &root).unwrap().ok());
    Campaign { _temp: temp, root }
}

fn declare(host: &Fake, c: &Campaign, target: &str, lane: Lane, after: &[&str]) -> Outcome {
    let declaration = Declaration {
        lane: Some(lane),
        after: after.iter().map(|text| (*text).to_owned()).collect(),
        ..Declaration::default()
    };
    let outcome = commands::declare(host, &c.root.join(target), &declaration).unwrap();
    fs::write(c.root.join(target).join("kickoff.md"), "Go.\n").unwrap();
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
