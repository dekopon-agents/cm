use crate::tick::act::{ActLine, Ctx, Executor, Outcome};
use crate::tick::exec::merge_ready::{shell_words, target};

pub const KEY: &str = "unblock-now";

pub struct UnblockNow;

impl Executor for UnblockNow {
    fn run(&self, line: &ActLine, ctx: &Ctx) -> Outcome {
        unblock(line, ctx).unwrap_or_else(Outcome::Failed)
    }
}

fn unblock(line: &ActLine, ctx: &Ctx) -> Result<Outcome, String> {
    if target(line)?.starts_with("04-owner/") {
        return Ok(Outcome::Left(format!(
            "{} [owner step: unblock it and file its OWNER-QUEUE item]",
            line.text()
        )));
    }
    let command = line.field("command").unwrap_or_default();
    let words = shell_words(command)
        .filter(|words| {
            words.starts_with(&["cm".to_owned(), "unblock".to_owned()])
                && !words.iter().any(|word| word.contains('<'))
        })
        .ok_or_else(|| format!("not a runnable cm unblock: {command}"))?;
    let args: Vec<&str> = words[2..].iter().map(String::as_str).collect();
    ctx.cm("unblock", &args)?;
    Ok(Outcome::Done(Vec::new()))
}
