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
