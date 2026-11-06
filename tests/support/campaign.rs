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
    /// A synthetic release declaration from before release tags were persisted.
    #[allow(dead_code)] // Shared with suites that do not exercise legacy releases.
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
