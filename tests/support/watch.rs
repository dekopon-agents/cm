use std::fs;
use std::path::PathBuf;

use super::campaign_fixture::Campaign;

impl Campaign {
    pub fn schema_three_watches(&self) -> PathBuf {
        let dir = self.root.join("01-a/u");
        fs::create_dir_all(dir.join(".cm")).unwrap();
        fs::create_dir_all(dir.join("check")).unwrap();
        self.kickoff("01-a/u/check", "Go.\n");
        fs::write(
            dir.join(".cm/state.json"),
            include_str!("watch-state-v3.json"),
        )
        .unwrap();
        fs::write(dir.join("STATE.md"), include_str!("watch-state-v3.md")).unwrap();
        dir
    }
}
