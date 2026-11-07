#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm::commands;
use cm::host::Host;

pub struct Campaign {
    pub(super) _temp: tempfile::TempDir,
    pub root: PathBuf,
}

pub fn campaign_outside_git(host: &dyn Host) -> Campaign {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("campaign");
    assert!(commands::init(host, &root).unwrap().ok());
    Campaign { _temp: temp, root }
}

pub fn campaign(host: &dyn Host) -> Campaign {
    let c = campaign_outside_git(host);
    let git = Command::new("git")
        .args(["init", "-q"])
        .arg(&c.root)
        .status();
    assert!(
        git.unwrap().success(),
        "a campaign lives in a git repository"
    );
    c
}

impl Campaign {
    pub fn supersession(&self, host: &dyn Host) {
        for target in [
            "01-first/alpha/OLD",
            "01-first/alpha/NEW",
            "02-next/beta/NEXT",
        ] {
            let declared = commands::declare(
                host,
                &self.root.join(target),
                &commands::Declaration {
                    lane: Some(cm::state::Lane::Codex),
                    ..commands::Declaration::default()
                },
            )
            .unwrap();
            assert!(declared.ok(), "{:?}", declared.violations);
        }
        fs::write(
            self.root.join("replacement.md"),
            "Replaced by the declared work.\n",
        )
        .unwrap();
    }

    pub fn files(&self) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(
            root: &Path,
            dir: &Path,
            files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path == root.join(".git") {
                    continue;
                }
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_owned(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut files = std::collections::BTreeMap::new();
        visit(&self.root, &self.root, &mut files);
        files
    }

    /// A synthetic release declaration from before release tags were persisted.
    pub fn legacy_release(&self, host: &dyn Host, full: &str) {
        let target = cm::store::parse_target(full, true).unwrap();
        assert!(cm::state::Release::of_target(full).is_some());
        let at = cm::host::stamp(host.now());
        let mut unit = cm::state::UnitState::new(&target.unit, &at);
        unit.schema = 1;
        let mut step = cm::state::Step::new(&at);
        step.lane = Some(cm::state::Lane::Pi);
        unit.steps.insert(target.step.unwrap(), step);
        let dir = self.root.join(target.unit);
        fs::create_dir_all(dir.join(cm::store::CM_DIR)).unwrap();
        fs::create_dir_all(self.root.join(full)).unwrap();
        cm::store::write_json(&cm::store::state_path(&dir), &unit).unwrap();
        fs::write(dir.join(cm::store::UNIT_STATE), cm::render::unit(&unit)).unwrap();
    }

    pub fn kickoff(&self, target: &str, text: &str) {
        fs::create_dir_all(self.root.join(target)).unwrap();
        fs::write(self.root.join(target).join("kickoff.md"), text).unwrap();
    }

    pub fn legacy_release_tag(&self, unit: &str, version: &str, tag: &str) {
        let path = cm::store::state_path(&self.root.join(unit));
        let mut value: serde_json::Value = cm::store::read_json(&path).unwrap();
        value["steps"][version]["tag"] = tag.into();
        cm::store::write_json(&path, &value).unwrap();
    }

    pub fn sized_file(&self, name: &str, bytes: u64, tracked: bool) {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .unwrap();
        if name.ends_with("JOURNAL.md") {
            use std::io::Write;
            file.write_all(b"# Journal\n").unwrap();
        }
        file.set_len(bytes).unwrap();
        if tracked {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&self.root)
                    .args(["add", "-f", "--", name])
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=cm", "-c", "user.email=cm@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn repo(dir: &Path) -> (PathBuf, String) {
    let repo = dir.join("wt");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    (repo, sha)
}

pub fn release_repo(dir: &Path) -> (PathBuf, String, String) {
    let (repo, _) = repo(dir);
    git(&repo, &["tag", "v0.1.0"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "merge"]);
    let merged = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "release"]);
    git(&repo, &["tag", "-a", "v0.2.0", "-m", "release"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "unshipped"]);
    let unshipped = git(&repo, &["rev-parse", "HEAD"]);
    (repo, merged, unshipped)
}

pub fn repo_with_missing_history(dir: &Path) -> (PathBuf, String, String) {
    let (repo, first) = repo(dir);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "middle"]);
    let middle = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "later merge"],
    );
    let later = git(&repo, &["rev-parse", "HEAD"]);
    fs::remove_file(
        repo.join(".git/objects")
            .join(&middle[..2])
            .join(&middle[2..]),
    )
    .unwrap();
    (repo, first, later)
}
