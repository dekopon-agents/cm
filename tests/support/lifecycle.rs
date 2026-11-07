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

    pub fn part_sync_receipts(&self, host: &dyn Host, full: &str, sha: &str) -> [PathBuf; 2] {
        let target = parse_target(full, true).unwrap();
        let holder = format!("{}/RECEIPTS", target.unit);
        let declared = commands::declare(
            host,
            &self.root.join(&holder),
            &Declaration {
                lane: Some(Lane::Coordinator),
                work: Work::Study,
                ..Declaration::default()
            },
        )
        .unwrap();
        assert!(declared.ok(), "{:?}", declared.violations);
        self.kickoff(&holder, "Record sync receipts.\n");
        ["east", "west"].map(|part| {
            let receipt = cm::ledger::evidence_pinned(
                host,
                &self.root,
                &parse_target(&holder, true).unwrap(),
                part,
                "Argo synced the pinned revision.\n",
                &cm::ledger::PinArgs {
                    candidate: Some(&format!("{full}#{part}@{sha}")),
                    harness: None,
                },
            )
            .unwrap();
            assert!(receipt.ok(), "{:?}", receipt.violations);
            self.root.join(receipt.fields["path"].as_str().unwrap())
        })
    }

    pub fn receipt_stale(&self, full: &str, file: &Path, reason: Option<&str>) {
        let target = parse_target(full, true).unwrap();
        let dir = self.root.join(target.unit);
        let mut unit = store::load_unit(&dir).unwrap();
        let relative = store::relative(&self.root, file);
        let pin = unit
            .steps
            .values_mut()
            .flat_map(|step| &mut step.pins)
            .find(|pin| pin.file.as_str() == relative)
            .unwrap();
        pin.stale = reason.map(str::to_owned);
        commands::write_unit(&dir, &unit).unwrap();
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

pub const MERGED_AT: &str = "2026-10-08T07:15:42Z";

pub fn merged_pull(sha: &str, merged_at: Option<&str>) -> super::support::FakeGh {
    let mut pull = serde_json::json!({
        "state": "closed", "merged": true, "merge_commit_sha": sha,
        "head": { "sha": sha },
    });
    if let Some(merged_at) = merged_at {
        pull["merged_at"] = merged_at.into();
    }
    super::support::FakeGh::new(&format!(
        r#"case "$2" in
  repos/o/r/pulls/7) echo '{pull}' ;;
  */check-runs*) echo '{{"total_count":0,"check_runs":[]}}' ;;
  */status*) echo '{{"total_count":0,"statuses":[]}}' ;;
  *) exit 1 ;;
esac
"#
    ))
}

impl Campaign {
    pub fn unblock_file(&self) -> std::path::PathBuf {
        let path = self.root.join("unblock.md");
        std::fs::write(&path, "Continue the step.\n").unwrap();
        path
    }

    pub fn record_merge_time(&self, full: &str, merged_at: &str) {
        let target = parse_target(full, true).unwrap();
        let dir = self.root.join(&target.unit);
        let mut unit = store::load_unit(&dir).unwrap();
        let track = &mut unit
            .steps
            .get_mut(target.step.as_deref().unwrap())
            .unwrap()
            .track;
        track.evidence.merged_at = Some(merged_at.into());
        unit.history.push(cm::state::Move {
            at: track.since.clone(),
            target: full.into(),
            from: Phase::PrOpen,
            to: Phase::Merged,
            evidence: track.evidence.clone(),
        });
        commands::write_unit(&dir, &unit).unwrap();
    }
}
