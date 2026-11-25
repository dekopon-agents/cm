use serde_json::Value;

use crate::tick::act::{ActLine, Ctx, Executor, Outcome, last_line};
use crate::tick::exec::merge_ready::{TOOLS, shell_words, target, tmpdir};
use crate::tick::proc::{Cmd, Exit, Keep};

pub const KEY: &str = "released-undeployed";

pub struct ReleasedUndeployed;

impl Executor for ReleasedUndeployed {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome {
        deploy(line, ctx).unwrap_or_else(Outcome::Failed)
    }
}

fn deploy(line: &ActLine, ctx: &Ctx) -> Result<Outcome, String> {
    let command = line.field("command").unwrap_or_default();
    let words =
        shell_words(command).ok_or_else(|| format!("cannot split into words: {command}"))?;
    let release = match &words[..] {
        [script, release] if script.ends_with("deploy.sh") && !release.contains('<') => release,
        _ => {
            return Ok(Outcome::Left(format!(
                "{} [a deploy step's own move: yours]",
                line.text()
            )));
        }
    };
    let step = target(line)?;
    if !step.starts_with("00-release/") {
        return Err(format!("{step} is not a release step; deploy by hand"));
    }
    let tmp = tempfile::Builder::new()
        .prefix("cm-deploy-")
        .tempdir_in(tmpdir(ctx))
        .map_err(|error| format!("cannot make a directory for deploy.sh's receipt: {error}"))?;
    let receipt = tmp.path().join("receipt.json");
    let deployed = ctx
        .run(
            Cmd::new("bash")
                .arg(ctx.checkout.join(TOOLS).join("deploy.sh"))
                .args([release, "--receipt"])
                .arg(&receipt)
                .keep(Keep::Tail, Keep::Tail),
        )
        .map_err(|error| error.to_string())?;
    let said = last_line(&deployed);
    if deployed.exit == Exit::Code(3) {
        return Ok(Outcome::Left(format!(
            "released-undeployed {step}: {release} waiting on CI ({said})"
        )));
    }
    if !deployed.exit.success() {
        return Err(format!("deploy.sh {release}: {said}"));
    }
    let Some(done) = std::fs::read(&receipt)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return Err(format!(
            "deploy.sh {release} merged but wrote no receipt ({said}); record the deploy by hand"
        ));
    };
    let (Some(deployed_release), Some(digest)) = (
        done.get("release").and_then(Value::as_str),
        done.get("digest").and_then(Value::as_str),
    ) else {
        return Err(format!(
            "deploy.sh {release} merged but its receipt names no release and digest ({said}); record the deploy by hand"
        ));
    };
    let pr = shown(done.get("pr"));
    let merge_sha = shown(done.get("merge_sha"));
    let body = tmp.path().join("deploy.md");
    std::fs::write(
        &body,
        format!(
            "# Deploy of {deployed_release}\n\n- deploy PR: {}\n- merge SHA: {}\n- digest: {digest}\n- deploy.sh: {said}\n",
            pr.as_deref().unwrap_or("none: main already ran it"),
            merge_sha.as_deref().unwrap_or("none"),
        ),
    )
    .map_err(|error| format!("deployed {release}; cannot write its evidence note: {error}"))?;
    let name = format!(
        "deploy-{}",
        merge_sha
            .as_deref()
            .unwrap_or("already")
            .chars()
            .take(12)
            .collect::<String>()
    );
    let note = ctx
        .cm_cmd("evidence", &[step, "--name", &name, "--body-file"])
        .arg(&body)
        .keep(Keep::Tail, Keep::Tail);
    match ctx.run(note) {
        Ok(ran) if ran.exit.success() || last_line(&ran).to_lowercase().contains("exist") => {}
        Ok(ran) => {
            return Err(format!(
                "deployed {release}; cm evidence refused: {}",
                last_line(&ran)
            ));
        }
        Err(error) => return Err(format!("deployed {release}; cm evidence refused: {error}")),
    }
    let advance = ctx
        .cm_cmd("advance", &[step, "deployed", "--digest", digest])
        .keep(Keep::Tail, Keep::Tail);
    let refused = match ctx.run(advance) {
        Ok(ran) if ran.exit.success() => return Ok(Outcome::Done(Vec::new())),
        Ok(ran) => last_line(&ran),
        Err(error) => error.to_string(),
    };
    Err(format!(
        "deployed {release} ({}); cm advance {step} deployed refused: {refused}",
        pr.as_deref().unwrap_or("already on main")
    ))
}

// Python's `str(value)` for a value `or` would keep.
fn shown(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null | Value::Bool(false) => None,
        Value::String(text) if text.is_empty() => None,
        Value::String(text) => Some(text.clone()),
        Value::Number(number) if number.as_f64() == Some(0.0) => None,
        Value::Array(items) if items.is_empty() => None,
        Value::Object(fields) if fields.is_empty() => None,
        other => Some(other.to_string()),
    }
}
