/// Synthetic findings for review convergence counts.
pub const BLOCKING: [&str; 3] = [
    "F1 | contract | introduced | src/lib.rs:1 | first defect",
    "F2 | contract | exposed | src/lib.rs:2 | second defect",
    "F3 | contract | introduced | src/lib.rs:3 | third defect",
];

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
