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
