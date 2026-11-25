use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::tick::act::{ActLine, Ctx, Executor, Outcome, last_line};
use crate::tick::exec::review_owed::{
    is_sha, py_lines, py_space, truthy, unbroken, universal_newlines, write_within_limit,
};
use crate::tick::proc::Cmd;

pub const KEY: &str = "launchable-not-launched.codex";

const CODEX_CLOUD_PY: &str = ".claude/skills/codex-cloud/scripts/codex_cloud.py";

const AGENTS: &str = "code/xrl/agents";

const RECORD_PY: &str = "tools/agent-ledger/record.py";

pub struct CodexDispatch;

impl Executor for CodexDispatch {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome {
        match dispatch(line, ctx) {
            Ok(lines) => Outcome::Done(lines.into_iter().map(unbroken).collect()),
            Err(reason) => Outcome::Failed(unbroken(reason)),
        }
    }
}

fn dispatch(line: &ActLine, ctx: &Ctx) -> Result<Vec<String>, String> {
    let target = line.field("target").unwrap_or_default();
    if line.json.get("part").is_some_and(truthy) {
        return Err("a part's launch is the model's".to_owned());
    }
    let kickoff = ctx.epoch.join(target).join("kickoff.md");
    let brief = std::fs::read(ctx.checkout.join(&kickoff))
        .map_err(|_| format!("no {}", kickoff.display()))?;
    let brief = String::from_utf8(universal_newlines(&brief))
        .map_err(|_| format!("{} is not UTF-8 text", kickoff.display()))?;
    if names_parts(&brief) {
        return Err(
            "the kickoff names Parts:; declare them with cm amend --part and dispatch by hand"
                .to_owned(),
        );
    }
    let Some((model, effort)) = lane_model(&brief) else {
        return Err("the kickoff's Lane: line names no model and effort (`Lane: codex-cloud (<model> <effort>)`); dispatch by hand".to_owned());
    };
    let env_id = ctx
        .env
        .non_empty("CODEX_CLOUD_ENVIRONMENT_ID")
        .ok_or("CODEX_CLOUD_ENVIRONMENT_ID is not set")?;
    let home = ctx
        .env
        .non_empty("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set, so there is no ~/code/xrl/agents")?;
    let cc = ctx
        .env
        .non_empty("CODEX_CLOUD_PY")
        .map_or_else(|| home.join(CODEX_CLOUD_PY), PathBuf::from);
    let agents = home.join(AGENTS);
    let git = |args: &[&str]| Cmd::new("git").arg("-C").arg(&agents).args(args);
    let _ = ctx.run(git(&["pull", "-q"]));
    let steering = ctx
        .run(git(&["rev-parse", "HEAD"]))
        .map(|ran| ran.stdout_text())
        .unwrap_or_default();
    let steering = steering.trim_matches(py_space);
    if !is_sha(steering) {
        return Err("no steering SHA: git -C ~/code/xrl/agents rev-parse HEAD failed".to_owned());
    }
    // Written before anything is launched, so a TMPDIR that cannot take it stops the dispatch
    // with no effect; dropped, it is deleted.
    let prompt = prompt_file(ctx, &brief.replace("<STEERING-SHA>", steering))?;

    ctx.cm("launch", &[target])?;
    let state = ctx.step_state(target)?;
    let Some(claim) = state["attempt"]["claim"]
        .as_str()
        .filter(|claim| !claim.is_empty())
    else {
        return Err(format!(
            "cm launch recorded no attempt claim for {target}; nothing dispatched"
        ));
    };
    ctx.cm("attempt", &[target, "--claim", claim, "begin", "thread"])?;
    let started = jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%S%.6f%:z")
        .to_string();
    let created = ctx.run(
        Cmd::new("uv")
            .args(["run", "-q"])
            .arg(&cc)
            .args(["--write", "jobs", "create", "--environment-id", env_id])
            .arg("--prompt-file")
            .arg(prompt.path())
            .args(["--model", model, "--effort", effort, "--title", target])
            .cwd("/tmp"),
    );
    drop(prompt);
    let (admitted, receipt, last) = match &created {
        Ok(ran) => {
            let receipt = receipt(&ran.stdout_text());
            let admitted = ran.exit.success()
                && receipt.get("stage").and_then(Value::as_str) == Some("turn-admitted");
            (admitted, receipt, last_line(ran))
        }
        Err(error) => (false, Map::new(), error.to_string()),
    };
    let thread = receipt
        .get("thread_id")
        .and_then(Value::as_str)
        .filter(|thread| !thread.is_empty());
    if let Some(thread) = thread {
        ctx.cm(
            "attempt",
            &[target, "--claim", claim, "record", "thread", thread],
        )
        .map_err(|error| {
            format!(
                "thread {thread} was created but not recorded; inspect it before any retry: {error}"
            )
        })?;
    }
    if !admitted {
        let stage = match receipt.get("stage") {
            None => "no receipt".to_owned(),
            Some(Value::String(stage)) => stage.clone(),
            Some(other) => other.to_string(),
        };
        return Err(match thread {
            Some(thread) => format!(
                "thread {thread} recorded, but jobs create stopped at {stage}; inspect it before any retry: {last}"
            ),
            None => format!(
                "no thread recorded: jobs create stopped at {stage}; check for a job before any retry: {last}"
            ),
        });
    }
    let thread = thread.unwrap_or("unknown");
    let echoed = |name: &str| {
        receipt
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
    };
    let ledger = ctx.run(
        Cmd::new("python3")
            .arg("-I")
            .arg(ctx.checkout.join(RECORD_PY))
            .args([
                "manual",
                "--step",
                target,
                "--role",
                "writer",
                "--harness",
                "codex-cloud",
                "--model-requested",
                model,
                "--model-echoed",
                echoed("model"),
                "--effort-requested",
                effort,
                "--effort-echoed",
                echoed("reasoning_effort"),
                "--session-id",
                thread,
                "--ts-start",
                &started,
                "--epoch",
                &ctx.slug(),
                "--source",
                "cm tick codex dispatch",
                "--out",
            ])
            .arg(ctx.epoch.join("AGENTS.jsonl")),
    );
    let unwritten = match ledger {
        Ok(ran) if ran.exit.success() => return Ok(Vec::new()),
        Ok(ran) => last_line(&ran),
        Err(error) => error.to_string(),
    };
    Ok(vec![format!(
        "launchable-not-launched {target}: dispatched (thread {thread}); AGENTS.jsonl row not written: {unwritten}"
    )])
}

fn prompt_file(ctx: &Ctx, prompt: &str) -> Result<tempfile::NamedTempFile, String> {
    let dir = Path::new(ctx.env.non_empty("TMPDIR").unwrap_or("/tmp"));
    // The child runs in /tmp, so a relative TMPDIR is taken from where cm tick runs.
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let failed = |error: std::io::Error| {
        format!(
            "cannot write the prompt file in {}: {error}; nothing dispatched",
            dir.display()
        )
    };
    let mut file = tempfile::Builder::new()
        .prefix("codex-prompt-")
        .suffix(".md")
        .tempfile_in(&dir)
        .map_err(failed)?;
    write_within_limit(file.as_file_mut(), prompt.as_bytes()).map_err(failed)?;
    Ok(file)
}

// As act.d read `jobs create`'s rows: the last one whose `receipt` is a non-empty object.
fn receipt(stdout: &str) -> Map<String, Value> {
    py_lines(stdout)
        .rev()
        .find_map(|row| match serde_json::from_str(row) {
            Ok(Value::Object(mut row)) => match row.remove("receipt") {
                Some(Value::Object(receipt)) if !receipt.is_empty() => Some(receipt),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_default()
}

// act.d's `^\s*(?:- )?`?Parts:` under `re.M`.
fn names_parts(brief: &str) -> bool {
    brief.split('\n').any(|line| {
        let line = line.trim_start_matches(py_space);
        let line = line.strip_prefix("- ").unwrap_or(line);
        line.strip_prefix('`').unwrap_or(line).starts_with("Parts:")
    })
}

// act.d's `^\s*(?:- )?Lane:(.*)$` under `re.M`, then
// `\b(gpt-[0-9][a-z0-9.-]*)\s+(low|medium|high|xhigh)\b` searched in its group.
fn lane_model(brief: &str) -> Option<(&str, &str)> {
    let lane = brief.split('\n').find_map(|line| {
        let line = line.trim_start_matches(py_space);
        line.strip_prefix("- ")
            .unwrap_or(line)
            .strip_prefix("Lane:")
    })?;
    let word = |c: char| c.is_alphanumeric() || c == '_';
    lane.match_indices("gpt-").find_map(|(at, _)| {
        if lane[..at].ends_with(word) {
            return None;
        }
        let rest = &lane[at + 4..];
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let end = rest
            .find(|c: char| !matches!(c, 'a'..='z' | '0'..='9' | '.' | '-'))
            .unwrap_or(rest.len());
        let after = &rest[end..];
        let spaced = after.trim_start_matches(py_space);
        if spaced.len() == after.len() {
            return None;
        }
        let effort = ["low", "medium", "high", "xhigh"]
            .into_iter()
            .find(|effort| {
                spaced
                    .strip_prefix(effort)
                    .is_some_and(|tail| !tail.starts_with(word))
            })?;
        Some((&lane[at..at + 4 + end], effort))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANE: &str = "repo: o/r\n\n- Lane: codex-cloud (gpt-6.2-sol xhigh), size XS, repo o/r.\n";

    #[test]
    fn the_lane_line_names_the_model_and_effort_as_act_d_matched_them() {
        assert_eq!(lane_model(LANE), Some(("gpt-6.2-sol", "xhigh")));
        for (brief, pair) in [
            ("Lane: gpt-6 low", Some(("gpt-6", "low"))),
            (
                "  \u{2028}\x1f - Lane:(gpt-6.1-terra\t\u{a0}medium)",
                Some(("gpt-6.1-terra", "medium")),
            ),
            ("\n \n- Lane: gpt-6. high-ish", Some(("gpt-6.", "high"))),
            ("Lane: gpt-6 xhighest, gpt-7 high", Some(("gpt-7", "high"))),
            ("Lane: gpt-gpt-6 high", Some(("gpt-6", "high"))),
            ("Lane: xgpt-6 high", None),
            ("Lane: _gpt-6 high", None),
            ("Lane: égpt-6 high", None),
            ("Lane: gpt-6.2-Sol high", None),
            ("Lane: gpt-6high", None),
            ("Lane: gpt-6 high_", None),
            ("Lane: gpt-x high", None),
            ("Lane: GPT-6 high", None),
            ("Lane: codex-cloud", None),
            ("Lane: codex-cloud\n(gpt-6 high)", None),
            ("-Lane: gpt-6 high", None),
            ("- - Lane: gpt-6 high", None),
            ("Size: XS. Lane: gpt-6 high", None),
            ("- Repo: o/r (gpt-6 high)", None),
            ("Lane: codex\nLane: gpt-6 high", None),
        ] {
            assert_eq!(lane_model(brief), pair, "{brief:?}");
        }
    }

    #[test]
    fn a_parts_line_is_bulleted_backticked_or_bare() {
        for brief in [
            "Parts: a, b",
            "- `Parts: a, b`: one PR each.",
            "x\n  - Parts: a",
            "x\n\u{2029}`Parts: a`",
        ] {
            assert!(names_parts(brief), "{brief:?}");
        }
        for brief in [
            "- Lane: x",
            "-`Parts: a`",
            "- - Parts: a",
            "Parts a",
            "Its Parts: a",
            "``Parts: a",
        ] {
            assert!(!names_parts(brief), "{brief:?}");
        }
    }

    #[test]
    fn the_receipt_is_the_last_row_carrying_one() {
        let rows = concat!(
            "{\"receipt\": {\"stage\": \"before-thread-start\"}}\n",
            "not json\n",
            "[1]\n",
            "{\"receipt\": {}}\n",
            "{\"receipt\": \"x\"}\n",
            "{\"receipt\": {\"stage\": \"turn-admitted\", \"thread_id\": \"t-1\"}}\r\n",
            "{\"other\": 1}\n",
        );
        let receipt = receipt(rows);
        assert_eq!(receipt["stage"], "turn-admitted");
        assert_eq!(receipt["thread_id"], "t-1");
        assert!(super::receipt("").is_empty());
        assert!(super::receipt("{\"receipt\": null}").is_empty());
    }
}
