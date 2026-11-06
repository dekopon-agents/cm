use std::fs;
use std::path::PathBuf;
use std::process::Command;

use cm::commands;
use cm::host::Host;

pub struct Campaign {
    _temp: tempfile::TempDir,
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
        fs::write(self.root.join(target).join("kickoff.md"), text).unwrap();
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
