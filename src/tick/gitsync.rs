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
        ) && git(ctx, &["push", "-q", "origin", "main"])
        {
            return report;
        }
        abort(ctx);
    }
    report.lines.push(format!(
        "heartbeat: git pull --rebase and push to main failed {TRIES} times; main does not have this checkout's commits"
    ));
    report.failed = true;
    report
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
    use crate::tick::proc::{FakeProc, Ran};

    const PULL: &str = "git pull -q --rebase --autostash origin main";
    const PUSH: &str = "git push -q origin main";
    const ABORT: &str = "git rebase --abort";

    struct Git {
        branch: &'static str,
        status: &'static str,
        failing: &'static [&'static str],
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
    };

    fn synced(proc: &FakeProc, mode: Mode) -> Report {
        let env = Env::default();
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
            proc.log(),
            [
                "git rev-parse --abbrev-ref HEAD",
                "git status --porcelain",
                "git add -A",
                "git commit -qm chore(mnemosyne): heartbeat sync",
                PULL,
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
            proc.log(),
            [
                "git rev-parse --abbrev-ref HEAD",
                "git status --porcelain",
                PULL,
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
            let tries: Vec<String> = proc.log().into_iter().skip(2).collect();
            let one: &[&str] = if failing == [PULL] {
                &[PULL, ABORT]
            } else {
                &[PULL, PUSH, ABORT]
            };
            assert_eq!(tries, one.repeat(3));
            assert_eq!(proc.pauses(), [Duration::from_secs(2); 2]);
        }
    }

    #[test]
    fn a_pull_the_budget_cut_is_still_aborted_and_not_tried_again() {
        let budget = Duration::from_millis(500);
        let proc = FakeProc::new(move |cmd| {
            let line = cmd.line();
            Ok(if line == "git rev-parse --abbrev-ref HEAD" {
                Ran::code(0, "main\n")
            } else if line == PULL {
                std::thread::sleep(budget + Duration::from_millis(100));
                Ran::timed_out()
            } else {
                Ran::code(0, "")
            })
        });
        let env = Env::default();
        let ctx = Ctx::new(
            Path::new("/ct"),
            Path::new("store/epochs/eleemosynary-mnemosyne"),
            &proc,
            &env,
            OsStr::new("cm"),
            budget,
        );
        let report = sync(&ctx, Mode::Always);
        assert_eq!(
            report.lines,
            [
                "heartbeat: git pull --rebase and push to main failed 3 times; main does not have this checkout's commits"
            ]
        );
        assert_eq!(
            proc.log().into_iter().skip(2).collect::<Vec<_>>(),
            [PULL, ABORT]
        );
        let abort = proc.calls().pop().unwrap();
        assert_eq!(abort.cwd.as_deref(), Some(Path::new("/ct")));
        assert_eq!(abort.timeout, ABORT_TIMEOUT);
        assert!(proc.pauses().is_empty());
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
            } else {
                Ran::code(0, "")
            })
        });
        assert_eq!(synced(&proc, Mode::Always), Report::default());
        assert_eq!(
            proc.log().into_iter().skip(2).collect::<Vec<_>>(),
            [PULL, ABORT, PULL, PUSH]
        );
        assert_eq!(proc.pauses(), [Duration::from_secs(2)]);
    }

    #[test]
    fn the_slug_tail_is_what_follows_its_last_hyphen() {
        assert_eq!(slug_tail("eleemosynary-mnemosyne"), "mnemosyne");
        assert_eq!(slug_tail("a-b-c"), "c");
        assert_eq!(slug_tail("solo"), "solo");
        assert_eq!(slug_tail("trailing-"), "");
    }
}
