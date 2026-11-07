use std::path::Path;

use cm::commands::{self, Declaration};
use cm::host::{Host, stamp};
use cm::state::{Evidence, Lane, Phase};
use cm::store::{self, parse_target};

use super::campaign_fixture::Campaign;

impl Campaign {
    pub fn merged_step(&self, host: &dyn Host, full: &str, lane: Lane, repo: &Path, sha: &str) {
        let declared = commands::declare(
            host,
            &self.root.join(full),
            &Declaration {
                lane: Some(lane),
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
