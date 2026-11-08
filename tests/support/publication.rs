use cm::commands::{self, Declaration, Ships};
use cm::forge::Read;
use cm::host::{Host, stamp};
use cm::state::{Evidence, Lane, Phase};
use cm::store::{self, parse_target};

use super::campaign_fixture::{Campaign, campaign as initialized_campaign};
use super::forge_fixture::FakeForge;

pub const STEP: &str = "00-release/widget/1.4.0";
pub const TAG: &str = "widget-1.4.0";
pub const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const ARTIFACTS: [&str; 3] = ["crate:acme-sdk", "tap:widget", "image:ghcr.io/acme/widget"];

pub fn formula(tag: &str) -> String {
    format!(
        "class Widget < Formula\n  version \"1.4.0\"\n  url \"https://github.com/o/widget/releases/download/{tag}/widget.tar.gz\"\n  sha256 \"{}\"\nend\n",
        "b".repeat(64)
    )
}

pub fn present() -> FakeForge {
    let mut forge = FakeForge::default();
    forge
        .tags
        .insert(("o/widget".into(), TAG.into()), [SHA.into()].into());
    forge.crates.insert(
        ("acme-sdk".into(), "1.4.0".into()),
        Read::Present("checksum".into()),
    );
    forge
        .formulas
        .insert(("o/homebrew-tap".into(), "widget".into()), formula(TAG));
    forge.images.insert(
        ("ghcr.io/acme/widget".into(), "v1.4.0".into()),
        Read::Present("sha256:digest".into()),
    );
    forge
}

pub fn campaign(host: &dyn Host, phase: Phase, artifacts: &[&str]) -> Campaign {
    let c = initialized_campaign(host);
    let out = commands::declare(
        host,
        &c.root.join(STEP),
        &Declaration {
            lane: Some(Lane::Codex),
            artifacts: artifacts.iter().map(|s| (*s).into()).collect(),
            ships: Ships {
                github: Some("o/widget".parse().unwrap()),
                tag: Some("widget-{version}".into()),
            },
            ..Declaration::default()
        },
    )
    .unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    c.kickoff(STEP, "Publish the declared artifacts.\n");
    let target = parse_target(STEP, true).unwrap();
    let dir = c.root.join(&target.unit);
    let mut unit = store::load_unit(&dir).unwrap();
    unit.steps
        .get_mut(target.step.as_deref().unwrap())
        .unwrap()
        .track
        .apply(
            phase,
            &stamp(host.now()),
            &Evidence {
                sha: Some(SHA.into()),
                pr: Some("https://github.com/o/widget/pull/7".into()),
                ..Evidence::default()
            },
        );
    commands::write_unit(&dir, &unit).unwrap();
    c
}

pub fn unit(c: &Campaign) -> cm::state::UnitState {
    store::load_unit(&c.root.join("00-release/widget")).unwrap()
}

pub fn step(c: &Campaign) -> cm::state::Step {
    unit(c).steps.remove("1.4.0").unwrap()
}

pub fn advance(
    host: &dyn Host,
    c: &Campaign,
    forge: &dyn cm::forge::Forge,
    tag_only: bool,
    resuming: bool,
) -> cm::outcome::Outcome {
    commands::advance_gated(
        host,
        &c.root,
        &parse_target(STEP, true).unwrap(),
        commands::Advance {
            to: &Phase::Released,
            given: &Evidence {
                tag: (!resuming).then(|| TAG.into()),
                sha: tag_only.then(|| SHA.into()),
                ..Evidence::default()
            },
            file: None,
            repo: None,
            part: None,
            by: &[],
        },
        &commands::MergeGate {
            forge: Some(forge),
            ..Default::default()
        },
    )
    .unwrap()
}
