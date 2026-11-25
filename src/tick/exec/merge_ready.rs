use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;

use crate::tick::act::{ActLine, Ctx, Executor, Outcome, last_line};
use crate::tick::proc::{Cmd, Keep};

pub const KEY: &str = "merge-ready";

pub(crate) const TOOLS: &str = "tools/cloud-env";

const QUEUED: &str = "dekopon-agents/dekopon";

pub struct MergeReady;

impl Executor for MergeReady {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome {
        merge(line, ctx).unwrap_or_else(Outcome::Failed)
    }
}

fn merge(line: &ActLine, ctx: &Ctx) -> Result<Outcome, String> {
    let target = target(line)?;
    if truthy(line.json.get("part")) {
        return Err("a part's merge is the model's".to_owned());
    }
    let step = ctx.step_state(target)?;
    let pull = ctx.pull(line, &step)?;
    let left = |note: String| Ok(Outcome::Left(format!("{} [{note}]", line.text())));
    if pull.repo.eq_ignore_ascii_case(QUEUED) {
        return left(
            "dekopon merges through its merge queue: check the drop tally, then enable_pr_auto_merge"
                .to_owned(),
        );
    }
    if !may_merge(ctx, &pull.repo)? {
        return left(format!(
            "the stamp's [authority] does not let a script merge in {}",
            pull.repo
        ));
    }
    let head = either(line.json.get("head"), pull.observed.get("head"))
        .and_then(Value::as_str)
        .filter(|head| is_sha(head))
        .ok_or_else(|| {
            format!(
                "no observed head for {} in .cm/pulls.json; cm sync first",
                pull.url
            )
        })?;
    let pulls = format!("repos/{}/pulls/{}", pull.repo, pull.number);
    let base_ref = ctx
        .run(Cmd::new("gh").args(["api", pulls.as_str(), "--jq", ".base.ref"]))
        .map_err(|error| error.to_string())?;
    let branch = base_ref.stdout_text().trim().to_owned();
    if !base_ref.exit.success() || branch.is_empty() {
        return Err(format!(
            "cannot read the base branch of {}: {}",
            pull.url,
            last_line(&base_ref)
        ));
    }
    let ci_base = pull.observed.get("ci_base");
    let base = either(line.json.get("base"), pull.observed.get("base"));
    if !truthy(ci_base) || ci_base != base {
        return left(format!(
            "stale base: CI ran on {}, the base is now {}; update the branch and wait for green CI",
            short(ci_base),
            short(base)
        ));
    }
    let receipts = tmpdir(ctx).join("dekopon-merge").join(format!(
        "{}-{}-{}",
        pull.repo.replace('/', "-"),
        pull.number,
        &head[..12]
    ));
    match receipts.join("merge-attempted.json").try_exists() {
        Ok(false) => {}
        Ok(true) => {
            return Err(format!(
                "an earlier merge attempt left {}; reconcile {} before any retry",
                receipts.display(),
                pull.url
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot tell whether an earlier merge attempt left {}: {error}; reconcile {} before any retry",
                receipts.display(),
                pull.url
            ));
        }
    }
    let merged = ctx
        .run(
            Cmd::new("python3")
                .arg("-I")
                .arg(ctx.checkout.join(TOOLS).join("merge-pr.py"))
                .arg(&pull.repo)
                .arg(pull.number.to_string())
                .args([head, "--base", branch.as_str(), "--evidence"])
                .arg(&receipts),
        )
        .map_err(|error| error.to_string())?;
    if !merged.exit.success() {
        return Err(format!("merge-pr.py: {}", last_line(&merged)));
    }
    let Ok(receipt) = serde_json::from_slice::<Value>(&merged.stdout) else {
        return Err(format!(
            "merge-pr.py merged {} but printed no JSON receipt ({}); record the merge by hand",
            pull.url,
            last_line(&merged)
        ));
    };
    let sha = receipt
        .get("merge_sha")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let words = shell_words(line.field("command").unwrap_or_default()).unwrap_or_default();
    let rest: Vec<&str> = match &words[..] {
        [cm, advance, named, rest @ ..]
            if cm == "cm"
                && advance == "advance"
                && named == target
                && rest.iter().any(|word| word == "<merge-sha>") =>
        {
            rest.iter()
                .map(|word| {
                    if word == "<merge-sha>" {
                        sha
                    } else {
                        word.as_str()
                    }
                })
                .collect()
        }
        _ => vec!["merged", "--pr", pull.url.as_str(), "--sha", sha],
    };
    let advance = ctx
        .cm_cmd("advance", &[target])
        .args(rest)
        .keep(Keep::Tail, Keep::Tail);
    let refused = match ctx.run(advance) {
        Ok(ran) if ran.exit.success() => return Ok(Outcome::Done(Vec::new())),
        Ok(ran) => last_line(&ran),
        Err(error) => error.to_string(),
    };
    Ok(Outcome::Done(vec![format!(
        "merge-ready {target}: merged {} as {}; cm advance refused ({refused}); cm sync records it",
        pull.url,
        sha.chars().take(12).collect::<String>()
    )]))
}

// The authority is the stamp's, read at the stamp SHA: the working tree's LIMITS.toml is not it.
fn may_merge(ctx: &Ctx, repo: &str) -> Result<bool, String> {
    let epoch = ctx.epoch.strip_prefix(ctx.checkout).unwrap_or(ctx.epoch);
    let front = std::fs::read_to_string(ctx.epoch_dir().join("EPOCH.md")).map_err(|error| {
        format!(
            "cannot read {}: {error}; no stamp, no merge",
            epoch.join("EPOCH.md").display()
        )
    })?;
    let stamp =
        stamp(&front).ok_or("EPOCH.md has no approved_plan_revision; no stamp, no merge")?;
    let mut spec = OsString::from(format!("{stamp}:"));
    spec.push(epoch.join("LIMITS.toml"));
    let shown = ctx
        .run(
            Cmd::new("git")
                .arg("-C")
                .arg(ctx.checkout)
                .arg("show")
                .arg(spec),
        )
        .map_err(|error| error.to_string())?;
    let unreadable =
        |why: String| format!("cannot read [authority] from LIMITS.toml at the stamp SHA: {why}");
    let authority = std::str::from_utf8(&shown.stdout)
        .ok()
        .filter(|_| shown.exit.success())
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|mut limits| match limits.remove("authority") {
            Some(toml::Value::Table(authority)) => Some(authority),
            _ => None,
        })
        .ok_or_else(|| unreadable(last_line(&shown)))?;
    let excluded = match authority.get("merge_excluded_repos") {
        None => false,
        Some(toml::Value::Array(repos)) => repos
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|excluded| excluded.eq_ignore_ascii_case(repo)),
        Some(_) => return Err(unreadable("merge_excluded_repos is not a list".to_owned())),
    };
    Ok(authority.get("merge") == Some(&toml::Value::Boolean(true)) && !excluded)
}

fn stamp(front: &str) -> Option<&str> {
    front
        .split(['\n', '\r'])
        .filter_map(|line| line.strip_prefix("approved_plan_revision: "))
        .find(|sha| is_sha(sha))
}

fn short(value: Option<&Value>) -> String {
    let text: String = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.chars().take(12).collect(),
        Some(other) => other.to_string().chars().take(12).collect(),
    };
    if text.is_empty() {
        "no observed base".to_owned()
    } else {
        text
    }
}

fn is_sha(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn target(line: &ActLine) -> Result<&str, String> {
    line.field("target")
        .ok_or_else(|| "the act line has no target".to_owned())
}

// TMPDIR as the children see it; a relative one is taken from the checkout they run in.
pub(crate) fn tmpdir(ctx: &Ctx) -> PathBuf {
    ctx.checkout
        .join(ctx.env.non_empty("TMPDIR").unwrap_or("/tmp"))
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
    }
}

fn either<'a>(first: Option<&'a Value>, second: Option<&'a Value>) -> Option<&'a Value> {
    if truthy(first) { first } else { second }
}

// Python's `shlex.split`: POSIX words, no comments; None where it raises.
pub(crate) fn shell_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => words.extend(word.take()),
            '\\' => word.get_or_insert_default().push(chars.next()?),
            '\'' => {
                let word = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let c = chars.next()?;
                            if c != '"' && c != '\\' {
                                word.push('\\');
                            }
                            word.push(c);
                        }
                        c => word.push(c),
                    }
                }
            }
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    Some(words)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn shell_words_split_as_shlex_does() {
        let cases: [(&str, &[&str]); 13] = [
            (
                "cm advance 01-a/u/b merged --pr x --sha '<merge-sha>'",
                &[
                    "cm",
                    "advance",
                    "01-a/u/b",
                    "merged",
                    "--pr",
                    "x",
                    "--sha",
                    "<merge-sha>",
                ],
            ),
            ("a  b\tc\nd\re", &["a", "b", "c", "d", "e"]),
            ("", &[]),
            ("  ", &[]),
            ("a''b \"\" ''", &["ab", "", ""]),
            ("a\\ b", &["a b"]),
            ("\"a\\\"b\\\\c\\$d\"", &["a\"b\\c\\$d"]),
            ("'a\\b'", &["a\\b"]),
            ("a#b #c", &["a#b", "#c"]),
            ("x\\\ny", &["x\ny"]),
            ("a\"b c\"d", &["ab cd"]),
            ("é ü", &["é", "ü"]),
            ("a\u{b}b\u{c}c", &["a\u{b}b\u{c}c"]),
        ];
        for (text, words) in cases {
            assert_eq!(shell_words(text).unwrap(), words, "{text:?}");
        }
        for text in ["a 'b", "a \"b", "a\\", "\"a\\"] {
            assert_eq!(shell_words(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_stamp_is_the_first_whole_approved_plan_revision_line() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            stamp(&format!(
                "---\nstatus: WORKING\napproved_plan_revision: {sha}\r\n---\n"
            )),
            Some(sha)
        );
        assert_eq!(
            stamp(&format!(
                "approved_plan_revision: {}\nx\rapproved_plan_revision: {sha}",
                sha.to_uppercase()
            )),
            Some(sha)
        );
        for front in [
            format!("approved_plan_revision: {sha} "),
            format!(" approved_plan_revision: {sha}"),
            format!("approved_plan_revision: {sha}0"),
            format!("approved_plan_revision:  {sha}"),
            "approved_plan_revision: ".to_owned(),
        ] {
            assert_eq!(stamp(&front), None, "{front:?}");
        }
    }

    #[test]
    fn a_missing_base_is_named_and_a_present_one_cut_to_12() {
        assert_eq!(short(None), "no observed base");
        assert_eq!(short(Some(&json!(null))), "no observed base");
        assert_eq!(short(Some(&json!(""))), "no observed base");
        assert_eq!(short(Some(&json!("b".repeat(40)))), "b".repeat(12));
        assert_eq!(short(Some(&json!("ééééééééééééé"))), "é".repeat(12));
        assert_eq!(short(Some(&json!(12))), "12");
    }

    #[test]
    fn either_is_pythons_or() {
        let (zero, one, empty) = (json!(0), json!("x"), json!(""));
        assert_eq!(either(Some(&one), Some(&zero)), Some(&one));
        assert_eq!(either(Some(&empty), Some(&zero)), Some(&zero));
        assert_eq!(either(None, Some(&one)), Some(&one));
        assert_eq!(either(Some(&json!(null)), None), None);
        assert!(!truthy(Some(&json!({}))) && truthy(Some(&json!([0]))));
    }
}
