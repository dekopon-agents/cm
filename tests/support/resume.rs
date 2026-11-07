use std::fs;

use cm::commands::write_unit;
use cm::host::{Host, stamp};
use cm::state::{Evidence, Phase, Step, UnitState};

use super::campaign_fixture::Campaign;
use super::forge_fixture::FakeForge;

pub const BEFORE: &str = "> Owner’s banner stays exactly as written.  \r\n> ```\r\n> /loop Resume the work.\r\n> ```\r\n\r\n# Resume\r\n\r\n## State today\r\n\r\nOwner context before the status.  \r\n<!-- cm:status -->\r\n";
pub const AFTER: &str = "<!-- cm:status-end -->\r\n\r\nOwner context after the status.  \r\n\r\n## How to work\r\n\r\nKeep these instructions.\r\n\r\n## Read, in this order\r\n\r\n1. Owner notes.  ";

pub fn resume_text(status: &str) -> String {
    format!("{BEFORE}{status}{AFTER}")
}

pub fn resume_with_marker_example(fence: &str, status: Option<&str>) -> String {
    let example = format!(
        "{fence}markdown\n<!-- cm:status -->\nExample status, kept literally.\n<!-- cm:status-end -->\n{fence}\n\n"
    );
    let mut text = resume_text(status.unwrap_or_default()).replace(
        "<!-- cm:status -->\r\n",
        &format!(
            "{example}{}",
            if status.is_some() {
                "<!-- cm:status -->\r\n"
            } else {
                ""
            }
        ),
    );
    if status.is_none() {
        text = text.replace("<!-- cm:status-end -->\r\n", "");
    }
    text
}

impl Campaign {
    pub fn resume_working_epoch(&self) {
        fs::write(
            self.root.join("EPOCH.md"),
            "---\nstatus: WORKING\nskill_revision: synthetic-skill\napproved_plan_revision: synthetic-plan\n---\n",
        )
        .unwrap();
    }

    pub fn resume_releases(&self, host: &dyn Host) -> FakeForge {
        let at = stamp(host.now());
        let mut work = UnitState::new("01-first/alpha", &at);
        for (name, phase) in [
            ("DRAFT", Phase::Drafted),
            ("BLOCKED", Phase::Blocked("waiting".into())),
            ("RETIRED", Phase::Superseded),
        ] {
            let mut step = Step::new(&at);
            step.track.state = phase;
            work.steps.insert(name.into(), step);
        }
        fs::create_dir_all(self.root.join("01-first/alpha/.cm")).unwrap();
        write_unit(&self.root.join("01-first/alpha"), &work).unwrap();
        for (line, versions) in [
            (
                "widget",
                vec![
                    ("1.9.0", Phase::Deployed),
                    ("1.10.0", Phase::Merged),
                    ("1.11.0", Phase::Drafted),
                ],
            ),
            ("alpha", vec![("2.0.0", Phase::Released)]),
            ("gizmo", vec![("3.0.0", Phase::Done)]),
        ] {
            let unit = format!("00-release/{line}");
            let mut state = UnitState::new(&unit, &at);
            for (version, phase) in versions {
                let mut step = Step::new(&at);
                step.github = Some(format!("o/{line}").parse().unwrap());
                step.tag = Some(format!("v{version}").parse().unwrap());
                step.track.state = phase;
                if step.track.state != Phase::Drafted {
                    step.track.evidence = Evidence {
                        pr: Some(format!("https://github.com/o/{line}/pull/1")),
                        sha: Some("a".repeat(40)),
                        tag: Some(format!("v{version}")),
                        digest: Some("sha256:synthetic".into()),
                        ..Evidence::default()
                    };
                }
                state.steps.insert(version.into(), step);
            }
            let dir = self.root.join(unit);
            fs::create_dir_all(dir.join(".cm")).unwrap();
            write_unit(&dir, &state).unwrap();
        }
        fs::write(self.root.join("EPOCH.md"), "---\nstatus: WORKING\n---\n").unwrap();
        let mut forge = FakeForge::default();
        forge.tags.insert(
            ("o/widget".into(), "v1.10.0".into()),
            std::collections::BTreeSet::from(["a".repeat(40)]),
        );
        forge
    }
}
