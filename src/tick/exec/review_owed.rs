use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::tick::act::{ActLine, Ctx, Executor, Outcome, last_line};
use crate::tick::proc::{Cmd, Exit};

pub const KEY: &str = "review-owed";

pub(crate) const ASTRA: &str = "tools/cloud-env/astra-review.sh";

pub(crate) const GATE_MODEL: &str = "gpt-6-astra";

pub struct ReviewOwed;

impl Executor for ReviewOwed {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome {
        match review(line, ctx) {
            Ok(lines) => Outcome::Done(lines),
            Err(reason) => Outcome::Failed(reason),
        }
    }
}

fn review(line: &ActLine, ctx: &Ctx) -> Result<Vec<String>, String> {
    if line
        .field("command")
        .is_some_and(|command| command.starts_with("cm attention"))
    {
        return Ok(vec![format!(
            "{} [FIX REQUIRED at this head: route the findings to the fix pass]",
            line.text()
        )]);
    }
    if line.json.get("part").is_some_and(truthy) {
        return Err("a part's review is the model's".to_owned());
    }
    let target = line.field("target").unwrap_or_default();
    let pull = ctx.pull(line, &ctx.step_state(target)?)?;
    let given = |name: &str| {
        [&line.json[name], &pull.observed[name]]
            .into_iter()
            .find_map(|value| value.as_str().filter(|text| !text.is_empty()))
    };
    let (Some(head), Some(base)) = (given("head"), given("base")) else {
        return Err(no_head(&pull.url));
    };
    if !is_sha(head) || !is_sha(base) {
        return Err(no_head(&pull.url));
    }
    let evidence = ctx.epoch.join(target).join("evidence");
    let gates = gates(ctx, &evidence, &pull.repo, pull.number);
    if let Some(current) = gates.iter().find(|gate| gate.head == head) {
        if !ctx
            .checkout
            .join(&current.dir)
            .join("cm-record.txt")
            .exists()
            && collect(ctx, &current.dir)? == Collected::Pending
        {
            return Ok(Vec::new());
        }
        let report = current.dir.join("report.md");
        let verdict = fs::read(ctx.checkout.join(&report))
            .ok()
            .and_then(|bytes| last_text_line(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_else(|| "?".to_owned());
        return Ok(vec![format!(
            "review-owed {target}: Astra {verdict} recorded at {} ({}); the next move is yours (the fix pass, or the Claude review: merge-ready waits for both verdicts at this head)",
            &head[..12],
            report.display()
        )]);
    }

    let round = gates.last().map_or(1, |gate| gate.round.saturating_add(1));
    let prev = gates.last();
    if let Some(prev) = prev
        && !ctx.checkout.join(&prev.dir).join("report.md").exists()
    {
        return Err(format!(
            "round {round} needs {}/report.md as its findings",
            prev.dir.display()
        ));
    }
    let name = pull.repo.split_once('/').map_or("", |(_, name)| name);
    let out = evidence.join(format!("astra-{name}-{}-r{round}", pull.number));
    let brief = ctx.cm("brief", &[target])?.stdout;
    let brief_md = out.join("brief.md");
    fs::create_dir_all(ctx.checkout.join(&out))
        .map_err(|error| format!("cannot create {}: {error}", out.display()))?;
    fs::write(ctx.checkout.join(&brief_md), brief)
        .map_err(|error| format!("cannot write {}: {error}", brief_md.display()))?;
    let ci = if line.json.get("ci_green").is_some_and(truthy) {
        "green"
    } else {
        "unknown"
    };
    let mut start = Cmd::new(ctx.checkout.join(ASTRA))
        .arg("start-shadowed")
        .arg(&pull.repo)
        .arg(pull.number.to_string())
        .arg(base)
        .arg(head)
        .arg(&brief_md)
        .arg(&out)
        .arg("--round")
        .arg(round.to_string())
        .args(["--ci", ci]);
    if let Some(prev) = prev {
        start = start
            .arg("--prev-head")
            .arg(&prev.head)
            .arg("--findings")
            .arg(prev.dir.join("report.md"));
    }
    match ctx.run(start) {
        Ok(ran) if ran.exit.success() => Ok(vec![format!(
            "review-owed {target}: Astra started at {} ({}); start the Claude review at the same head now; merge-ready waits for both verdicts",
            &head[..12],
            out.display()
        )]),
        Ok(ran) => Err(format!(
            "astra-review.sh start-shadowed: {}",
            last_line(&ran)
        )),
        Err(error) => Err(format!("astra-review.sh start-shadowed: {error}")),
    }
}

fn no_head(url: &str) -> String {
    format!("no observed head and base for {url} in .cm/pulls.json; cm sync first")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Collected {
    Done,
    Pending,
}

pub(crate) fn collect(ctx: &Ctx, dir: &Path) -> Result<Collected, String> {
    let failed = |why: String| format!("astra-review.sh collect {}: {why}", dir.display());
    match ctx.run(Cmd::new(ctx.checkout.join(ASTRA)).arg("collect").arg(dir)) {
        Ok(ran) if ran.exit.success() => Ok(Collected::Done),
        Ok(ran) if ran.exit == Exit::Code(3) => Ok(Collected::Pending),
        Ok(ran) => Err(failed(last_line(&ran))),
        Err(error) => Err(failed(error.to_string())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Gate {
    dir: PathBuf,
    round: i64,
    head: String,
}

// `dir` is relative to the checkout, as astra-review.sh is given it.
fn gates(ctx: &Ctx, evidence: &Path, repo: &str, number: u64) -> Vec<Gate> {
    let mut gates: Vec<Gate> = listed(&ctx.checkout.join(evidence))
        .into_iter()
        .filter_map(|name| {
            let dir = evidence.join(&name);
            let request = request(&ctx.checkout.join(&dir))?;
            let head = request["head"].as_str().filter(|head| is_sha(head))?;
            let gate = gating(&name, &request)
                && request["mode"] == "pr"
                && request["repo"] == repo
                && request["pr"].as_u64() == Some(number);
            gate.then(|| Gate {
                round: request["round"]
                    .as_i64()
                    .filter(|round| *round != 0)
                    .unwrap_or(1),
                head: head.to_owned(),
                dir,
            })
        })
        .collect();
    gates.sort_by_key(|gate| gate.round);
    gates
}

// What the glob `*` lists, so no dot names; a directory that cannot be read lists nothing.
pub(crate) fn listed(dir: &Path) -> Vec<OsString> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = entries
        .flatten()
        .map(|entry| entry.file_name())
        .filter(|name| !name.as_encoded_bytes().starts_with(b"."))
        .collect();
    names.sort();
    names
}

pub(crate) fn request(dir: &Path) -> Option<Value> {
    let request: Value = serde_json::from_slice(&fs::read(dir.join("request.json")).ok()?).ok()?;
    request.is_object().then_some(request)
}

pub(crate) fn gating(name: &OsStr, request: &Value) -> bool {
    request["requested"]["model"] == GATE_MODEL && !name.to_string_lossy().contains("-shadow-")
}

pub(crate) fn is_sha(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

// Python's `str.isspace`, which is `\s` in its `re`.
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

// Python's `str.splitlines`, except that `\r\n` gives an empty line between its two halves.
pub(crate) fn py_lines(text: &str) -> impl DoubleEndedIterator<Item = &str> {
    text.split([
        '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
    ])
}

fn last_text_line(text: &str) -> Option<String> {
    py_lines(text)
        .map(|line| line.trim_matches(py_space))
        .rfind(|line| !line.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_gate_is_the_astra_model_outside_a_shadow_folder() {
        let astra = json!({"requested": {"model": "gpt-6-astra", "effort": "xhigh"}});
        let gating = |name: &str, request: &Value| gating(OsStr::new(name), request);
        assert!(gating("astra-r1", &astra));
        assert!(gating("astra-note-r3", &astra));
        assert!(!gating("astra-r1-shadow-gpt-6.2-sol-xhigh", &astra));
        assert!(!gating(
            "astra-r1",
            &json!({"requested": {"model": "gpt-6.2-astra"}})
        ));
        assert!(!gating("astra-r1", &json!({"model": "gpt-6-astra"})));
        assert!(!gating("astra-r1", &json!({"requested": null})));
    }

    #[test]
    fn a_sha_is_forty_lowercase_hex_digits() {
        assert!(is_sha(&"a".repeat(40)));
        assert!(is_sha("0123456789abcdef0123456789abcdef01234567"));
        for bad in [
            "a".repeat(39),
            "a".repeat(41),
            "A".repeat(40),
            "g".repeat(40),
            format!("{} ", "a".repeat(39)),
            String::new(),
        ] {
            assert!(!is_sha(&bad), "{bad}");
        }
    }

    #[test]
    fn the_verdict_is_the_last_line_with_text_stripped_as_python_strips() {
        assert_eq!(
            last_text_line("findings\n\nACCEPT\n"),
            Some("ACCEPT".to_owned())
        );
        assert_eq!(
            last_text_line("x\r\n  FIX REQUIRED \u{2028}\x1f \n\n"),
            Some("FIX REQUIRED".to_owned())
        );
        assert_eq!(last_text_line("\n \x0c\u{a0}\n"), None);
        assert_eq!(last_text_line(""), None);
    }

    #[test]
    fn truthy_is_pythons() {
        for value in [json!(null), json!(false), json!(0), json!(""), json!([])] {
            assert!(!truthy(&value), "{value}");
        }
        for value in [json!(true), json!(2), json!("x"), json!({"a": 0})] {
            assert!(truthy(&value), "{value}");
        }
    }
}
