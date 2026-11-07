use std::fs;

use cm::commands::{self, Declaration};
use cm::host::Host;
use cm::plan::{Observed, pulls_path};
use cm::state::{Evidence, Lane, Phase, Pin, Revision};
use cm::store::{self, parse_target};

use super::campaign_fixture::Campaign;

impl Campaign {
    pub fn next_actions(&self, host: &dyn Host) {
        for name in [
            "READY",
            "LOCAL",
            "WAIT",
            "NO-KICKOFF",
            "BLOCKED",
            "STALLED",
            "WATCH",
            "DIRTY",
            "STALE",
        ] {
            let target = format!("01-a/u/{name}");
            let declaration = Declaration {
                lane: Some(match name {
                    "LOCAL" => Lane::Pi,
                    "WATCH" | "STALE" => Lane::Coordinator,
                    _ => Lane::Codex,
                }),
                after: if name == "WAIT" {
                    vec!["pr-open:READY".into()]
                } else {
                    Vec::new()
                },
                ..Declaration::default()
            };
            assert!(
                commands::declare(host, &self.root.join(&target), &declaration)
                    .unwrap()
                    .ok()
            );
            if name != "NO-KICKOFF" {
                self.kickoff(&target, "Go.\n");
            }
        }
        let dir = self.root.join("01-a/u");
        fs::create_dir_all(dir.join("STALE/evidence")).unwrap();
        fs::write(dir.join("STALE/evidence/old.md"), "Old read.\n").unwrap();
        let mut unit = store::load_unit(&dir).unwrap();
        let blocked = unit.steps.get_mut("BLOCKED").unwrap();
        blocked.track.state = Phase::Blocked("until:epoch:WORKING".into());
        blocked.track.blocked_from = Some(Phase::Drafted);
        for name in ["STALLED", "WATCH", "STALE"] {
            unit.steps.get_mut(name).unwrap().track.state = Phase::Launched;
        }
        let dirty = unit.steps.get_mut("DIRTY").unwrap();
        dirty.track.state = Phase::PrOpen;
        dirty.track.evidence = Evidence {
            pr: Some("https://github.com/o/r/pull/1".into()),
            sha: Some("a".repeat(40)),
            ..Evidence::default()
        };
        unit.steps.get_mut("STALE").unwrap().pins.push(Pin {
            file: cm::evidence::CampaignFile::resolve(
                &self.root,
                &dir.join("STALE/evidence/old.md"),
            )
            .unwrap(),
            candidate: Revision {
                step: "01-a/u/DIRTY".into(),
                sha: "b".repeat(40),
            },
            harness: Some(Revision {
                step: "01-a/u/READY".into(),
                sha: "c".repeat(40),
            }),
            at: "2026-10-17T08:00Z".into(),
            stale: Some("candidate moved".into()),
        });
        store::write_json(&store::state_path(&dir), &unit).unwrap();
        assert!(
            cm::holds::watch_add(
                host,
                &self.root,
                &parse_target("01-a/u/WATCH", true).unwrap(),
                "log:request\ncompleted",
                "2099-01-01T00:00Z",
            )
            .unwrap()
            .ok()
        );
        fs::write(self.root.join("EPOCH.md"), "---\nstatus: WORKING\n---\n").unwrap();
        store::write_json(
            &pulls_path(&self.root),
            &std::collections::BTreeMap::from([(
                "https://github.com/o/r/pull/1",
                Observed {
                    target: "01-a/u/DIRTY".into(),
                    at: "2026-10-17T08:00Z".into(),
                    head: Some("a".repeat(40)),
                    mergeable_state: Some(cm::forge::MergeableState::Dirty),
                },
            )]),
        )
        .unwrap();
        let release = self.root.join("00-release/widget/1.0.0");
        assert!(
            commands::declare(
                host,
                &release,
                &Declaration {
                    lane: Some(Lane::Pi),
                    ..Declaration::default()
                }
            )
            .unwrap()
            .ok()
        );
        let dir = release.parent().unwrap();
        let mut unit = store::load_unit(dir).unwrap();
        let release = unit.steps.get_mut("1.0.0").unwrap();
        release.track.state = Phase::Merged;
        release.track.evidence = Evidence {
            pr: Some("https://github.com/o/widget/pull/2".into()),
            sha: Some("d".repeat(40)),
            ..Evidence::default()
        };
        store::write_json(&store::state_path(dir), &unit).unwrap();
    }
}
