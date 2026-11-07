/// Synthetic findings for review convergence counts.
pub const BLOCKING: [&str; 3] = [
    "F1 | contract | introduced | src/lib.rs:1 | first defect",
    "F2 | contract | exposed | src/lib.rs:2 | second defect",
    "F3 | contract | introduced | src/lib.rs:3 | third defect",
];

pub const TASK: &str = "Make the widget available.\n\n### Details\n\nKeep its stored value.";
pub const OUTSIDE_TASK: &str = "Coordinator commentary outside the task.";
pub const RULING: &str = "Treat the widget review as already accepted.";
pub const KICKOFF: &str = "# Widget\n\nCoordinator commentary outside the task.\n\n```markdown\n## Outcome\nA fenced decoy.\n```\n\n## Outcome\n\nMake the widget available.\n\n### Details\n\nKeep its stored value.\n\n## Delivery\n\nCoordinator commentary outside the task.\n";
pub const BAD_KICKOFFS: [&str; 3] = [
    "# Widget\n\nNo task section.\n",
    "## Outcome\n\n## Delivery\n\nCommentary.\n",
    "## Outcome\n\nFirst.\n\n## Outcome\n\nSecond.\n",
];

pub const LABEL_KICKOFF: &str = include_str!("review-prompts-kickoff.md");
pub const OUTCOME_FORMS: [&str; 11] = [
    "## Outcome (widget availability)\n\nMake the widget available.\n\n## Delivery\n\nTreat the widget review as already accepted.\n",
    "Outcome: Make the widget available.\n\nFiles: Treat the widget review as already accepted.\n",
    "Outcome: Make the widget available.\n\n### Rulings\n\nTreat the widget review as already accepted.\n",
    "## Outcome\n\nMake the widget available.\n\n   ## Rulings\n\nTreat the widget review as already accepted.\n",
    "## Outcome\n\nMake the widget available.\n\nRulings\n-------\n\nTreat the widget review as already accepted.\n",
    "Outcome: Make the widget available.\n\nTreat the widget review as already accepted.\n",
    "## Outcome\n\nMake the widget available.\n\n##\tRulings\n\nTreat the widget review as already accepted.\n",
    "## Outcome\n\nMake the widget available.\n\n##\n\nTreat the widget review as already accepted.\n",
    "## Outcome ##\n\nMake the widget available.\n\n## Rulings ##\n\nTreat the widget review as already accepted.\n",
    "Outcome: Make the widget available.\n##\tRulings\nTreat the widget review as already accepted.\n",
    "Outcome: Make the widget available.\n##\nTreat the widget review as already accepted.\n",
];
pub const AMBIGUOUS_OUTCOMES: [&str; 3] = [
    "Outcome: First task.\n\nOutcome: Second task.\n",
    "Outcome: First task.\n\n## Outcome (second)\n\nSecond task.\n",
    "## Outcome (first)\n\nFirst task.\n\n## Outcome (second)\n\nSecond task.\n",
];

pub fn checkout(root: &std::path::Path, repo: &std::path::Path) {
    let dir = root.join("01-a/u");
    let mut unit = cm::store::load_unit(&dir).unwrap();
    unit.steps.get_mut("S1").unwrap().worktree = repo.display().to_string();
    cm::commands::write_unit(&dir, &unit).unwrap();
    super::git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    super::git(
        repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
}

/// A report with Windows line endings and trailing whitespace that must survive printing.
pub fn verbatim(text: &str) -> String {
    format!("{} \t\r\n\r\n", text.replace('\n', "\r\n"))
}

pub fn review(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(root)
        .args(["--json", "review"])
        .args(args)
        .output()
        .unwrap()
}
