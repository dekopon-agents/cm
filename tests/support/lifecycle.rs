use std::path::{Path, PathBuf};

use cm::commands::{self, Declaration};
use cm::host::{Host, stamp};
use cm::state::{Evidence, Lane, Phase, Work};
use cm::store::{self, parse_target};

use super::campaign_fixture::Campaign;

impl Campaign {
    pub fn merged_step(&self, host: &dyn Host, full: &str, lane: Lane, repo: &Path, sha: &str) {
        self.merged_work(host, full, lane, repo, sha, Work::Code);
    }

    pub fn merged_rollout(&self, host: &dyn Host, full: &str, lane: Lane, repo: &Path, sha: &str) {
        self.merged_work(host, full, lane, repo, sha, Work::Rollout);
    }

    fn merged_work(
        &self,
        host: &dyn Host,
        full: &str,
        lane: Lane,
        repo: &Path,
        sha: &str,
        work: Work,
    ) {
        let declared = commands::declare(
            host,
            &self.root.join(full),
            &Declaration {
                lane: Some(lane),
                work,
                ..Declaration::default()
            },
        )
        .unwrap();
        assert!(declared.ok(), "{:?}", declared.violations);
        self.kickoff(full, "Go.\n");
        let target = parse_target(full, true).unwrap();
        let dir = self.root.join(target.unit);
        let mut unit = store::load_unit(&dir).unwrap();
        let step = unit.steps.get_mut(target.step.as_deref().unwrap()).unwrap();
        step.worktree = repo.display().to_string();
        step.track.apply(
            Phase::Merged,
            &stamp(host.now()),
            &Evidence {
                sha: Some(sha.into()),
                pr: Some("https://github.com/o/cm/pull/7".into()),
                ..Evidence::default()
            },
        );
        commands::write_unit(&dir, &unit).unwrap();
    }

    pub fn rollout_parts(&self, full: &str, names: &[&str]) {
        let target = parse_target(full, true).unwrap();
        let dir = self.root.join(target.unit);
        let mut unit = store::load_unit(&dir).unwrap();
        let step = unit.steps.get_mut(target.step.as_deref().unwrap()).unwrap();
        for name in names {
            step.parts.insert((*name).into(), step.track.clone());
        }
        commands::write_unit(&dir, &unit).unwrap();
    }

    pub fn sync_receipt(&self, sha: &str) -> PathBuf {
        let path = self.root.join("sync-receipt.md");
        std::fs::write(&path, format!("Argo sync revision: {sha}\n")).unwrap();
        path
    }

    pub fn release_receipt(&self, host: &dyn Host, repo: &Path, sha: &str) {
        self.merged_step(host, "00-release/cm/1.0.0", Lane::Pi, repo, sha);
        let dir = self.root.join("00-release/cm");
        let mut unit = store::load_unit(&dir).unwrap();
        unit.steps.get_mut("1.0.0").unwrap().track.apply(
            Phase::Deployed,
            &stamp(host.now()),
            &Evidence {
                tag: Some("v1.0.0".into()),
                digest: Some(format!("sha256:{}", "d".repeat(64))),
                ..Evidence::default()
            },
        );
        commands::write_unit(&dir, &unit).unwrap();
    }
}
