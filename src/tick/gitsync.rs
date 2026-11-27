use std::time::Duration;

use crate::tick::act::{Ctx, Report};
use crate::tick::proc::{Cmd, Keep};

pub const BUDGET: Duration = Duration::from_secs(60);

pub const TRIES: u32 = 3;

pub const BETWEEN_TRIES: Duration = Duration::from_secs(2);

pub const ABORT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Always,
    IfDirty,
}

pub fn sync(ctx: &Ctx, mode: Mode) -> Report {
    let mut report = Report::default();
    let branch = output(ctx, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let branch = branch.trim_end_matches('\n');
    if branch != "main" {
        let branch = if branch.is_empty() {
            "no branch"
        } else {
            branch
        };
        report.lines.push(format!(
            "heartbeat: the checkout is on {branch}, not main; nothing committed, pulled or pushed"
        ));
        return report;
    }
    if !output(ctx, &["status", "--porcelain"])
        .trim_end_matches('\n')
        .is_empty()
    {
        let message = format!("chore({}): heartbeat sync", slug_tail(&ctx.slug()));
        if !(git(ctx, &["add", "-A"]) && git(ctx, &["commit", "-qm", &message])) {
            report.lines.push(
                "heartbeat: git commit of the tree's changes failed; commit them by hand"
                    .to_owned(),
            );
            report.failed = true;
        }
    } else if mode == Mode::IfDirty {
        return report;
    }
    push(ctx, &mut report);
    report
}

// heartbeat.sh's line, word for word, even when the budget ended the tries before the third.
fn push(ctx: &Ctx, report: &mut Report) {
    for done in 1..=TRIES {
        if done > 1 {
            if ctx.remaining().is_zero() {
                break;
            }
            ctx.pause(BETWEEN_TRIES);
        }
        if git(
            ctx,
            &["pull", "-q", "--rebase", "--autostash", "origin", "main"],
        ) {
            // A successor's handoff that arrived with this pull retires this session: heartbeat.sh
            // pushed nothing and said nothing.
            if !ctx.still_bound() {
                return;
            }
            if git(ctx, &["push", "-q", "origin", "main"]) {
                return;
            }
        }
        abort(ctx);
    }
    report.lines.push(format!(
        "heartbeat: git pull --rebase and push to main failed {TRIES} times; main does not have this checkout's commits"
    ));
    report.failed = true;
}

fn slug_tail(slug: &str) -> &str {
    slug.rsplit('-').next().unwrap_or(slug)
}

fn command(args: &[&str]) -> Cmd {
    Cmd::new("git").args(args).keep(Keep::Head, Keep::Tail)
}

// A pull the budget cut can stop mid-rebase, with the tree's change in the autostash, so the
// abort gets its own timeout rather than what is left of the spent budget.
fn abort(ctx: &Ctx) {
    let _ = ctx.proc.run(
        &command(&["rebase", "--abort"])
            .cwd(ctx.checkout)
            .timeout(ABORT_TIMEOUT),
    );
}

fn git(ctx: &Ctx, args: &[&str]) -> bool {
    ctx.run(command(args)).is_ok_and(|ran| ran.exit.success())
}

fn output(ctx: &Ctx, args: &[&str]) -> String {
    ctx.run(command(args))
        .map(|ran| ran.stdout_text())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::proc::{FakeProc, KILL_AFTER, Ran};

    const PULL: &str = "git pull -q --rebase --autostash origin main";
    const PUSH: &str = "git push -q origin main";
    const ABORT: &str = "git rebase --abort";

    const BOUND: &str = "still_bound";

    struct Git {
        branch: &'static str,
        status: &'static str,
        failing: &'static [&'static str],
        pick: &'static str,
    }

    impl Git {
        fn proc(self) -> FakeProc {
            FakeProc::new(move |cmd| {
                let line = cmd.line();
                Ok(if self.failing.iter().any(|call| line.starts_with(call)) {
                    Ran::code(1, "")
                } else if line == "git rev-parse --abbrev-ref HEAD" {
                    Ran::code(0, self.branch)
                } else if line == "git status --porcelain" {
                    Ran::code(0, self.status)
                } else if line.contains("handoff-pick.sh") {
                    Ran::code(0, self.pick)
                } else {
                    Ran::code(0, "")
                })
            })
        }
    }

    const CLEAN: Git = Git {
        branch: "main\n",
        status: "",
        failing: &[],
        pick: "ok store/epochs/eleemosynary-mnemosyne/handoff/coordinator-1.md\n",
    };

    fn checked(proc: &FakeProc) -> Vec<String> {
        let mut log = Vec::new();
        for call in proc.log() {
            if call.contains("handoff-pick.sh") {
                log.pop();
                log.push(BOUND.to_owned());
            } else if !(call.starts_with("git fetch ") || call.starts_with("git diff ")) {
                log.push(call);
            }
        }
        log
    }

    fn session() -> Env {
        [("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_1")]
            .into_iter()
            .collect()
    }

    fn synced(proc: &FakeProc, mode: Mode) -> Report {
        let env = session();
        let ctx = Ctx::new(
            Path::new("."),
            Path::new("store/epochs/eleemosynary-mnemosyne"),
            proc,
            &env,
            OsStr::new("cm"),
            BUDGET,
        );
        sync(&ctx, mode)
    }

    #[test]
    fn a_dirty_tree_is_committed_under_the_slug_tail_then_pulled_and_pushed() {
        let proc = Git {
            status: " M CARD.md\n",
            ..CLEAN
        }
        .proc();
        assert_eq!(synced(&proc, Mode::Always), Report::default());
        assert_eq!(
            checked(&proc),
            [
                "git rev-parse --abbrev-ref HEAD",
                "git status --porcelain",
                "git add -A",
                "git commit -qm chore(mnemosyne): heartbeat sync",
                PULL,
                BOUND,
                PUSH,
            ]
        );
        assert!(proc.pauses().is_empty());
    }

    #[test]
    fn a_clean_tree_is_still_pulled_and_pushed_but_not_after_executors() {
        let proc = CLEAN.proc();
        assert_eq!(synced(&proc, Mode::Always), Report::default());
        assert_eq!(
            checked(&proc),
            [
                "git rev-parse --abbrev-ref HEAD",
                "git status --porcelain",
                PULL,
                BOUND,
                PUSH
            ]
        );
        let proc = CLEAN.proc();
        assert_eq!(synced(&proc, Mode::IfDirty), Report::default());
        assert_eq!(
            proc.log(),
            ["git rev-parse --abbrev-ref HEAD", "git status --porcelain"]
        );
        let proc = Git {
            status: "?? new\n",
            ..CLEAN
        }
        .proc();
        assert_eq!(synced(&proc, Mode::IfDirty), Report::default());
        assert_eq!(proc.log().last().map(String::as_str), Some(PUSH));
    }

    #[test]
    fn off_main_nothing_is_committed_pulled_or_pushed() {
        for (branch, named) in [("tick-mvp/x\n", "tick-mvp/x"), ("", "no branch")] {
            let proc = Git {
                branch,
                status: " M x\n",
                ..CLEAN
            }
            .proc();
            assert_eq!(
                synced(&proc, Mode::Always).lines,
                [format!(
                    "heartbeat: the checkout is on {named}, not main; nothing committed, pulled or pushed"
                )]
            );
            assert_eq!(proc.log(), ["git rev-parse --abbrev-ref HEAD"]);
        }
    }

    #[test]
    fn a_failed_commit_is_one_line_and_the_pull_and_push_still_run() {
        for failing in [&["git add"][..], &["git commit"][..]] {
            let proc = Git {
                status: " M x\n",
                failing,
                ..CLEAN
            }
            .proc();
            let report = synced(&proc, Mode::Always);
            assert_eq!(
                report.lines,
                ["heartbeat: git commit of the tree's changes failed; commit them by hand"]
            );
            assert!(report.failed);
            assert_eq!(proc.log().last().map(String::as_str), Some(PUSH));
            assert_eq!(
                proc.log().iter().any(|call| call.starts_with("git commit")),
                failing == ["git commit"],
                "a failed add runs no commit"
            );
        }
    }

    #[test]
    fn a_pull_or_push_that_keeps_failing_is_tried_three_times_two_seconds_apart() {
        for failing in [&[PULL][..], &[PUSH][..]] {
            let proc = Git { failing, ..CLEAN }.proc();
            let report = synced(&proc, Mode::Always);
            assert_eq!(
                report.lines,
                [
                    "heartbeat: git pull --rebase and push to main failed 3 times; main does not have this checkout's commits"
                ]
            );
            assert!(report.failed);
            let tries: Vec<String> = checked(&proc).into_iter().skip(2).collect();
            let one: &[&str] = if failing == [PULL] {
                &[PULL, ABORT]
            } else {
                &[PULL, BOUND, PUSH, ABORT]
            };
            assert_eq!(tries, one.repeat(3));
            assert_eq!(proc.pauses(), [Duration::from_secs(2); 2]);
        }
    }

    #[test]
    fn once_the_budget_is_spent_the_abort_still_runs_and_no_try_follows() {
        let proc = Git {
            failing: &[PULL],
            ..CLEAN
        }
        .proc();
        let env = Env::default();
        let spent = Ctx::new(
            Path::new("/ct"),
            Path::new("store/epochs/eleemosynary-mnemosyne"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::ZERO,
        );
        let mut report = Report::default();
        push(&spent, &mut report);
        assert_eq!(
            report.lines,
            [
                "heartbeat: git pull --rebase and push to main failed 3 times; main does not have this checkout's commits"
            ]
        );
        assert!(report.failed);
        assert_eq!(proc.log(), [ABORT], "the spent budget runs no pull");
        let abort = proc.calls().pop().unwrap();
        assert_eq!(abort.cwd.as_deref(), Some(Path::new("/ct")));
        assert_eq!(abort.timeout, Duration::from_secs(30));
        assert!(proc.pauses().is_empty());
    }

    // README's bound: a try the deadline cuts ends within its grace, and past the deadline only
    // the abort starts, with a timeout and a grace of its own.
    #[test]
    fn gitsync_ends_at_most_40_s_past_its_budget() {
        let proc = Git {
            failing: &[PULL],
            ..CLEAN
        }
        .proc();
        let env = Env::default();
        let spent = Ctx::new(
            Path::new("/ct"),
            Path::new("store/epochs/eleemosynary-mnemosyne"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::ZERO,
        );
        push(&spent, &mut Report::default());
        let past: Duration = KILL_AFTER
            + proc
                .calls()
                .iter()
                .map(|cmd| cmd.timeout + KILL_AFTER)
                .sum::<Duration>();
        assert_eq!(past, Duration::from_secs(40));
    }

    #[test]
    fn a_try_that_pushes_ends_the_retries() {
        let pulls = std::cell::Cell::new(0);
        let proc = FakeProc::new(move |cmd| {
            let line = cmd.line();
            Ok(if line == "git rev-parse --abbrev-ref HEAD" {
                Ran::code(0, "main\n")
            } else if line == PULL {
                pulls.set(pulls.get() + 1);
                Ran::code(if pulls.get() < 2 { 1 } else { 0 }, "")
            } else if line.contains("handoff-pick.sh") {
                Ran::code(0, CLEAN.pick)
            } else {
                Ran::code(0, "")
            })
        });
        assert_eq!(synced(&proc, Mode::Always), Report::default());
        assert_eq!(
            checked(&proc).into_iter().skip(2).collect::<Vec<_>>(),
            [PULL, ABORT, PULL, BOUND, PUSH]
        );
        assert_eq!(proc.pauses(), [Duration::from_secs(2)]);
    }

    #[test]
    fn a_pull_that_retires_the_session_pushes_nothing_and_says_nothing() {
        for pick in [
            "",
            "missing other-lineage store/epochs/e/handoff/coordinator-2.md\n",
        ] {
            let proc = Git {
                status: " M x\n",
                pick,
                ..CLEAN
            }
            .proc();
            assert_eq!(synced(&proc, Mode::Always), Report::default());
            assert_eq!(checked(&proc).last().map(String::as_str), Some(BOUND));
            assert_eq!(
                checked(&proc).iter().filter(|call| *call == PULL).count(),
                1
            );
            assert!(proc.pauses().is_empty());
        }
    }

    #[test]
    fn the_slug_tail_is_what_follows_its_last_hyphen() {
        assert_eq!(slug_tail("eleemosynary-mnemosyne"), "mnemosyne");
        assert_eq!(slug_tail("a-b-c"), "c");
        assert_eq!(slug_tail("solo"), "solo");
        assert_eq!(slug_tail("trailing-"), "");
    }
}
