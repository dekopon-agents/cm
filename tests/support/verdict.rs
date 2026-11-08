pub const MERGED_AT: &str = "2026-10-24T17:15:42Z";

/// Synthetic findings for review convergence counts.
pub const BLOCKING: [&str; 3] = [
    "F1 | contract | introduced | src/lib.rs:1 | first defect",
    "F2 | contract | exposed | src/lib.rs:2 | second defect",
    "F3 | contract | introduced | src/lib.rs:3 | third defect",
];

pub const EARLIER_MERGED_AT: &str = "2026-10-24T15:00:00Z";
pub const OTHER_PR: &str = "https://github.com/o/r/pull/8";

pub fn multipart_campaign(host: &super::Fake) -> super::Campaign {
    let c = super::campaign(host, true);
    let amended = cm::commands::amend(
        host,
        &c.root,
        &super::target(),
        &cm::commands::Amendment {
            parts: vec!["one".into(), "two".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(amended.ok(), "{:?}", amended.violations);
    let opened = cm::commands::advance_gated(
        host,
        &c.root,
        &super::target(),
        cm::commands::Advance {
            to: &cm::state::Phase::PrOpen,
            given: &cm::state::Evidence {
                pr: Some(OTHER_PR.into()),
                sha: Some(c.head()),
                ..Default::default()
            },
            file: None,
            repo: Some(&c.repo),
            part: Some("two"),
            by: &[],
        },
        &cm::commands::MergeGate::default(),
    )
    .unwrap();
    assert!(opened.ok(), "{:?}", opened.violations);
    let dir = c.root.join("01-a/u");
    let mut unit = cm::store::load_unit(&dir).unwrap();
    unit.steps.get_mut("S1").unwrap().lane = Some(cm::state::Lane::Pi);
    cm::commands::write_unit(&dir, &unit).unwrap();
    c
}

pub fn parts_at(
    c: &super::Campaign,
    host: &super::Fake,
    phase: cm::state::Phase,
    times: [Option<&str>; 2],
) {
    let dir = c.root.join("01-a/u");
    let mut unit = cm::store::load_unit(&dir).unwrap();
    let step = unit.steps.get_mut("S1").unwrap();
    for (name, merged_at) in ["one", "two"].into_iter().zip(times) {
        step.parts.get_mut(name).unwrap().apply(
            phase.clone(),
            &cm::host::stamp(cm::host::Host::now(host)),
            &cm::state::Evidence {
                merged_at: merged_at.map(str::to_owned),
                ..Default::default()
            },
        );
    }
    cm::commands::write_unit(&dir, &unit).unwrap();
}

/// A PR-open source whose head moved after the dependent pinned it.
pub fn stale_pin(root: &std::path::Path, dependent: &str) -> String {
    let target = cm::store::parse_target(dependent, true).unwrap();
    let dir = root.join(&target.unit);
    let mut unit = cm::store::load_unit(&dir).unwrap();
    let dependent = unit.steps.get_mut(target.step.as_deref().unwrap()).unwrap();
    let source_name = format!("{}/SOURCE", target.unit);
    dependent
        .after
        .push(cm::state::After::Pin(cm::state::Revision {
            step: source_name.clone(),
            sha: "a".repeat(40),
        }));
    let mut source = cm::state::Step::new(&dependent.track.since);
    source.lane = Some(cm::state::Lane::Pi);
    source.track.state = cm::state::Phase::PrOpen;
    source.track.evidence.pr = Some("https://github.com/o/r/pull/6".into());
    source.track.evidence.sha = Some("b".repeat(40));
    unit.steps.insert("SOURCE".into(), source);
    cm::commands::write_unit(&dir, &unit).unwrap();
    format!(
        "pin:{source_name}@{} (stale pin; recorded head {})",
        "a".repeat(40),
        "b".repeat(40)
    )
}

pub fn block_multipart_parent(c: &super::Campaign, host: &super::Fake) {
    let dir = c.root.join("01-a/u");
    let mut unit = cm::store::load_unit(&dir).unwrap();
    unit.steps.get_mut("S1").unwrap().track.evidence.pr = Some(OTHER_PR.into());
    cm::commands::write_unit(&dir, &unit).unwrap();
    let blocked = cm::commands::advance(
        host,
        &c.root,
        &super::target(),
        &cm::state::Phase::Blocked("parts".into()),
        &cm::state::Evidence::default(),
        None,
        Some(&c.repo),
    )
    .unwrap();
    assert!(blocked.ok(), "{:?}", blocked.violations);
}

pub const INVALID_WAIT_HEAD: &str = "not-a-sha";
pub const INVALID_WAIT_REVIEWER: &str = "bad/name";
pub const UNKNOWN_WAIT_STEPS: [&str; 2] = ["01-a/u/MISSING", "02-missing/unit/MISSING"];

pub const MALFORMED_CAMPAIGN_STATES: [&str; 2] = ["not JSON", r#"{"kind":"campaign"}"#];
