use std::fs;
use std::path::{Path, PathBuf};

use cm::host::Host;
use cm::ledger::{self, QueueItem};
use cm::templates::OWNER_QUEUE;

#[allow(dead_code)]
#[path = "campaign.rs"]
mod campaign_fixture;
use campaign_fixture::Campaign;

pub const FIRST: &str = "## 2. First\n\nContext: One decision.\nProceed?\nRecommendation: Yes.\nAnswer:\n\n```text\n## 7. Literal heading\n```\n\n";
pub const MIDDLE: &str = "## 7. Middle\n\nContext: Another decision.\nProceed?\nRecommendation: Wait.\nAnswer:\n\n### Details\n\nMore context.\n\n";
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
