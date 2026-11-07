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
