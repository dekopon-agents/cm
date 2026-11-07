use std::fs;
use std::path::{Path, PathBuf};

use cm::host::Host;
use cm::ledger::{self, QueueItem};
use cm::templates::OWNER_QUEUE;

use super::campaign_fixture::{self, Campaign};

pub const FIRST: &str = "## 2. First\n\nContext: One decision.\nProceed?\nRecommendation: Yes.\nAnswer:\n\n```text\n## 7. Literal heading\n```\n\n";
pub const MIDDLE: &str = "## 7. Middle\n\nContext: Another decision.\nProceed?\nRecommendation: Wait.\nAnswer: Wait for confirmation.\n\n### Details\n\nMore context.\n\n";
pub const ARCHIVED_MIDDLE: &str = "> ## 7. Middle\n>\n> Context: Another decision.\n> Proceed?\n> Recommendation: Wait.\n> Answer: Wait for confirmation.\n>\n> ### Details\n>\n> More context.\n>\n";
pub const LAST: &str =
    "## 11. Last café\n\nContext: Final decision.\nProceed?\nRecommendation: Yes.\nAnswer:";
pub const ANSWER: &str = "Proceed with the smaller change.\n\nKeep the follow-up separate.\n";
pub const REPLACEMENT: &str = "Context: Updated context.\nChoose the smaller change?\nRecommendation: Yes.\nAnswer:\n\n### Details\n\nUpdated details.";

pub fn item<'a>(title: &'a str, recommendation: &'a str) -> QueueItem<'a> {
    QueueItem {
        title,
        context: "A decision is needed.",
        question: "Proceed?",
        recommendation,
    }
}

pub fn campaign(host: &dyn Host) -> Campaign {
    let c = campaign_fixture::campaign(host);
    assert!(
        ledger::owner_queue_add(host, &c.root, &item("First", "Yes."))
            .unwrap()
            .ok()
    );
    fs::write(
        c.root.join("OWNER-QUEUE.md"),
        format!("{OWNER_QUEUE}\n{FIRST}{MIDDLE}{LAST}"),
    )
    .unwrap();
    c
}

pub fn body_file(root: &Path, body: &str) -> PathBuf {
    let path = root.parent().unwrap().join("body.md");
    fs::write(&path, body).unwrap();
    path
}

pub fn single_item(host: &dyn Host) -> Campaign {
    let c = campaign(host);
    fs::write(
        c.root.join("OWNER-QUEUE.md"),
        format!("{OWNER_QUEUE}\n{MIDDLE}"),
    )
    .unwrap();
    c
}

pub fn obstruct_write(root: &Path, name: &str) {
    fs::create_dir(root.join(format!(".{name}.cm-tmp"))).unwrap();
}

pub fn without_first_context(root: &Path) {
    let path = root.join("OWNER-QUEUE.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(path, text.replace("Context: One decision.\n", "")).unwrap();
}

pub fn renumber_last(root: &Path, number: u32) {
    let path = root.join("OWNER-QUEUE.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(
        path,
        text.replace("## 11. Last café", &format!("## {number}. Last café")),
    )
    .unwrap();
}

pub fn journal_number(host: &dyn Host, root: &Path, action: &str, number: u32) {
    assert!(
        cm::commands::journal(
            host,
            root,
            &format!("owner-queue {action} {number}. Archived"),
            None
        )
        .unwrap()
        .ok()
    );
}
