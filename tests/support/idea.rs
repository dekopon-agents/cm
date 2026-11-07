use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use super::campaign_fixture::{Campaign, campaign};

const MANIFEST: &str = "---\nepoch: epoch-alpha\nstatus: PLANNING\nideas_dir: ../ideas\n---\n\n# Epoch\n\nKeep this body exactly.  \n";
const BODY: &str = "\n# An idea\n\n## Proposal\n\nKeep `inline code`, spaces  and Unicode: λ.  \n\n## Epoch history\n\n- 2026-01-01: captured by hand.  \n  A continuation line.\n  - A nested bullet.\n\n## Details\n\nKeep the final section too.\n";

pub fn ideas() -> Campaign {
    let c = campaign(&cm::host::System);
    fs::create_dir_all(c.idea_dir()).unwrap();
    c.write_epoch(MANIFEST);
    c
}

impl Campaign {
    pub fn idea_dir(&self) -> PathBuf {
        self.root.parent().unwrap().join("ideas")
    }

    pub fn idea_path(&self, id: &str) -> PathBuf {
        self.idea_dir().join(format!("{id}.md"))
    }

    pub fn add_idea(&self, id: &str) -> String {
        let text = format!(
            "---\nid: {id}\ntitle: A synthetic idea\nsources: [example]\nmaturity: captured\n---\n{BODY}"
        );
        self.write_idea(id, &text);
        text
    }

    pub fn write_idea(&self, id: &str, text: &str) {
        fs::write(self.idea_path(id), text).unwrap();
    }

    pub fn read_idea(&self, id: &str) -> String {
        fs::read_to_string(self.idea_path(id)).unwrap()
    }

    pub fn duplicate_idea(&self, id: &str) {
        fs::copy(
            self.idea_path(id),
            self.idea_dir().join(format!("{id}-duplicate.md")),
        )
        .unwrap();
    }

    pub fn edit_idea(&self, id: &str, from: &str, to: &str) {
        let text = self.read_idea(id);
        assert!(text.contains(from));
        self.write_idea(id, &text.replace(from, to));
    }

    pub fn history(&self, id: &str, lines: &str) {
        let text = self.read_idea(id);
        self.write_idea(id, &append_expected(&text, lines));
    }

    pub fn epoch_text(&self) -> String {
        fs::read_to_string(self.root.join("EPOCH.md")).unwrap()
    }

    pub fn write_epoch(&self, text: &str) {
        fs::write(self.root.join("EPOCH.md"), text).unwrap();
    }

    pub fn edit_epoch(&self, from: &str, to: &str) {
        let text = self.epoch_text();
        assert!(text.contains(from));
        self.write_epoch(&text.replace(from, to));
    }

    pub fn journal_text(&self) -> String {
        fs::read_to_string(self.root.join("JOURNAL.md")).unwrap()
    }

    pub fn restore_journal(&self, text: &str) {
        fs::write(self.root.join("JOURNAL.md"), text).unwrap();
    }

    pub fn idea_snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut snapshot = self.files();
        for file in fs::read_dir(self.idea_dir()).unwrap() {
            let path = file.unwrap().path();
            snapshot.insert(path.clone(), fs::read(path).unwrap());
        }
        snapshot
    }

    pub fn cm(&self, args: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("--json")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .unwrap();
        let json: Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"));
        assert_eq!(output.status.success(), json["ok"] == true, "{output:?}");
        json
    }
}

pub fn append_expected(text: &str, entry: &str) -> String {
    let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let marker = format!("{ending}{ending}## Details");
    assert_eq!(text.matches(&marker).count(), 1);
    text.replace(
        &marker,
        &format!("{ending}{entry}{ending}{ending}## Details"),
    )
}

pub fn rules(json: &Value) -> Vec<&str> {
    json["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["rule"].as_str().unwrap())
        .collect()
}
