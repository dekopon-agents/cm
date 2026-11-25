use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::tick::act::{ActLine, Ctx, Env, Outcome, Registry, Report, clip};
use crate::tick::proc::{Cmd, Exit, KILL_AFTER, Keep, Proc, SystemProc, WHOLE_CAP};
use crate::tick::{claude_md, collect, directive, exec, gitsync, pin};

pub const DEADLINE_SECS: i64 = 230;

pub const EXECUTOR_CAP_SECS: i64 = 120;

pub const FRAME_CAP: Duration = Duration::from_secs(30);

pub const QUEUE_HELD: [&str; 2] = ["merge-ready", "review-owed"];

pub const HANDOFF_PICK: &str = "tools/cloud-env/handoff-pick.sh";

pub type Phase<'a> = &'a dyn Fn(&Ctx) -> Report;

pub struct Phases<'a> {
    pub gitsync: &'a dyn Fn(&Ctx, gitsync::Mode) -> Report,
    pub pin: Phase<'a>,
    pub directive: Phase<'a>,
    pub collect: Phase<'a>,
    pub claude_md: Phase<'a>,
}

impl Phases<'static> {
    pub const REAL: Phases<'static> = Phases {
        gitsync: &gitsync::sync,
        pin: &pin::run,
        directive: &directive::run,
        collect: &collect::run,
        claude_md: &claude_md::run,
    };
}

pub struct Beat<'a> {
    pub checkout: &'a Path,
    pub proc: &'a dyn Proc,
    pub env: &'a Env,
    pub registry: &'a Registry,
    pub phases: &'a Phases<'a>,
    pub cm: &'a OsStr,
    pub started: Instant,
}

impl Beat<'_> {
    fn ctx<'b>(&'b self, epoch: &'b Path, budget: Duration) -> Ctx<'b> {
        Ctx::new(self.checkout, epoch, self.proc, self.env, self.cm, budget)
    }

    fn elapsed_secs(&self) -> i64 {
        i64::try_from(self.started.elapsed().as_secs()).unwrap_or(i64::MAX)
    }
}

pub fn cli(checkout: &Path, cm: Option<&OsStr>, out: &mut dyn FnMut(String)) -> OsString {
    let started = Instant::now();
    let env = Env::system();
    let registry = exec::registry();
    let cm = cm.map_or_else(this_cm, OsStr::to_owned);
    let checkout = std::path::absolute(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let beat = Beat {
        checkout: &checkout,
        proc: &SystemProc,
        env: &env,
        registry: &registry,
        phases: &Phases::REAL,
        cm: &cm,
        started,
    };
    run(&beat, out);
    cm
}

pub fn this_cm() -> OsString {
    std::env::current_exe().map_or_else(|_| "cm".into(), PathBuf::into_os_string)
}

pub fn run(beat: &Beat, out: &mut dyn FnMut(String)) {
    let epochs = live_epochs(beat.checkout);
    let [epoch] = epochs.as_slice() else {
        out(format!(
            "heartbeat: {} PLANNING or WORKING epochs, not one; stay read-only until cm, PR and journal state are reconciled",
            epochs.len()
        ));
        return;
    };
    let deadline = deadline(beat.env).unwrap_or_else(|line| {
        out(line);
        DEADLINE_SECS
    });
    let phases = beat.phases;
    phase(beat, epoch, out, "gitsync", gitsync::BUDGET, |ctx| {
        (phases.gitsync)(ctx, gitsync::Mode::Always)
    });
    phase(beat, epoch, out, "pin", pin::BUDGET, phases.pin);
    phase(
        beat,
        epoch,
        out,
        "directive",
        directive::BUDGET,
        phases.directive,
    );
    let bound = bound(beat, epoch, out);
    if bound.is_ok() {
        phase(beat, epoch, out, "collect", collect::BUDGET, phases.collect);
    }
    let started = Instant::now();
    let left = u64::try_from(deadline.saturating_sub(beat.elapsed_secs())).unwrap_or(0);
    let budget = Duration::from_secs(left).saturating_add(KILL_AFTER);
    // A failed `cm next` prints only its own line, never the act phase's wall time.
    let act = act(beat, epoch, &bound, deadline, out);
    if !act.failed {
        wall_time(out, "act", started.elapsed(), budget, false);
    }
    if act.ran {
        phase(beat, epoch, out, "gitsync", gitsync::BUDGET, |ctx| {
            (phases.gitsync)(ctx, gitsync::Mode::IfDirty)
        });
    }
    phase(
        beat,
        epoch,
        out,
        "claude-md",
        claude_md::BUDGET,
        phases.claude_md,
    );
}

pub fn live_epochs(checkout: &Path) -> Vec<PathBuf> {
    let epochs = Path::new("store/epochs");
    let Ok(entries) = std::fs::read_dir(checkout.join(epochs)) else {
        return Vec::new();
    };
    let live = |bytes: Vec<u8>| {
        bytes.split(|byte| *byte == b'\n').any(|line| {
            line.starts_with(b"status: PLANNING") || line.starts_with(b"status: WORKING")
        })
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.file_name()))
        .filter(|name| !name.as_encoded_bytes().starts_with(b"."))
        .map(|name| epochs.join(name))
        .filter(|epoch| std::fs::read(checkout.join(epoch).join("EPOCH.md")).is_ok_and(live))
        .collect();
    found.sort();
    found
}

fn deadline(env: &Env) -> Result<i64, String> {
    match env.non_empty("HEARTBEAT_DEADLINE") {
        None => Ok(DEADLINE_SECS),
        Some(text) => text.trim().parse().map_err(|_| {
            format!(
                "heartbeat: HEARTBEAT_DEADLINE={text} is not a whole number of seconds; using {DEADLINE_SECS}"
            )
        }),
    }
}

fn phase(
    beat: &Beat,
    epoch: &Path,
    out: &mut dyn FnMut(String),
    name: &str,
    budget: Duration,
    body: impl FnOnce(&Ctx) -> Report,
) {
    let ctx = beat.ctx(epoch, budget);
    let started = Instant::now();
    let report = contained(|| body(&ctx)).unwrap_or_else(|panic| Report {
        lines: vec![format!("tick: {name} phase panicked: {panic}")],
        failed: true,
    });
    for line in report.lines {
        out(line);
    }
    wall_time(
        out,
        name,
        started.elapsed(),
        budget,
        report.failed || ctx.timed_out(),
    );
}

fn wall_time(
    out: &mut dyn FnMut(String),
    name: &str,
    wall: Duration,
    budget: Duration,
    failed: bool,
) {
    if failed || wall.as_secs() > budget.as_secs() {
        out(format!(
            "tick: {name} phase took {:.1} s (budget {} s)",
            wall.as_secs_f64(),
            budget.as_secs()
        ));
    }
}

struct Act {
    ran: bool,
    failed: bool,
}

const READ_FAILED: Act = Act {
    ran: false,
    failed: true,
};

fn act(
    beat: &Beat,
    epoch: &Path,
    bound: &Result<(), String>,
    deadline: i64,
    out: &mut dyn FnMut(String),
) -> Act {
    let next = Cmd::new(beat.cm)
        .args(["--json", "next", "-C"])
        .arg(epoch)
        .arg("--act")
        .cwd(beat.checkout)
        .timeout(FRAME_CAP)
        .keep(Keep::Whole, Keep::Head);
    let next = match beat.proc.run(&next) {
        Ok(ran) if ran.exit == Exit::TimedOut => {
            out(format!(
                "heartbeat: cm --json next --act ran past {} s and was killed",
                FRAME_CAP.as_secs()
            ));
            return READ_FAILED;
        }
        Ok(ran) if ran.exit == Exit::TooLarge => {
            out(format!(
                "heartbeat: cm --json next --act printed more than {} MiB and was killed; no act line was read",
                WHOLE_CAP >> 20
            ));
            return READ_FAILED;
        }
        Ok(ran) => ran,
        Err(error) => {
            out(format!(
                "heartbeat: cm --json next --act could not run: {error}"
            ));
            return READ_FAILED;
        }
    };
    let status = next.exit.shell_status();
    let list = serde_json::from_slice::<Value>(&next.stdout)
        .ok()
        .filter(|act| act.get("actionable").is_some_and(Value::is_array) && status <= 1);
    let Some(list) = list else {
        if status == 0 && next.stdout.iter().all(|byte| *byte == b'\n') {
            return Act {
                ran: false,
                failed: false,
            };
        }
        out(format!(
            "heartbeat: cm --json next --act exited {status} without an actionable list: {}",
            first_line(&next.stderr, 300)
        ));
        return READ_FAILED;
    };
    // The list can be tens of MiB: free the raw output and move the lines out, never copy them.
    drop(next);
    let mut list = list;
    let unread = list
        .get("owner_queue_unread")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if unread != 0 {
        out(format!(
            "owner-queue: {unread} unread item(s); read OWNER-QUEUE.md and run cm owner-queue read before any merge or review"
        ));
    }
    let lines = match list.get_mut("actionable").map(Value::take) {
        Some(Value::Array(lines)) => lines,
        _ => Vec::new(),
    };
    drop(list);
    let mut act = Act {
        ran: false,
        failed: false,
    };
    if lines.is_empty() {
        return act;
    }
    if let Err(why) = bound {
        out(format!(
            "heartbeat: executors off ({why}); every act line below is yours"
        ));
    }
    let epoch_dir = beat.checkout.join(epoch);
    for json in lines {
        let line = ActLine::read(json, &epoch_dir);
        let text = line.text();
        let kind = line.kind();
        let found = bound
            .as_ref()
            .ok()
            .and_then(|()| beat.registry.lookup(&kind, line.lane.as_deref()));
        let Some((name, executor)) = found else {
            out(text);
            continue;
        };
        if unread != 0 && QUEUE_HELD.contains(&kind.as_str()) {
            out(text);
            continue;
        }
        let cap = deadline
            .saturating_sub(beat.elapsed_secs())
            .min(EXECUTOR_CAP_SECS);
        if cap < 1 {
            out(format!("{text} [not run: heartbeat time budget spent]"));
            continue;
        }
        act.ran = true;
        let ctx = beat.ctx(epoch, Duration::from_secs(cap.unsigned_abs()));
        let outcome = contained(|| executor.run(&line, &ctx))
            .unwrap_or_else(|panic| Outcome::Failed(format!("panicked: {panic}")));
        if ctx.timed_out() {
            out(format!(
                "{text} [tick/{name} timed out after {cap} s and was killed; check what it started]"
            ));
            continue;
        }
        match outcome {
            Outcome::Done(lines) => {
                let left = lines.join("\n");
                let left = left.trim_end_matches('\n');
                if !left.is_empty() {
                    left.split('\n').for_each(|line| out(line.to_owned()));
                }
            }
            Outcome::Left(line) => out(line),
            Outcome::Failed(reason) => out(format!(
                "{text} [tick/{name} failed: {}]",
                clip(
                    reason
                        .split('\n')
                        .rfind(|line| !line.is_empty())
                        .unwrap_or("")
                )
            )),
        }
    }
    act
}

fn bound(beat: &Beat, epoch: &Path, out: &mut dyn FnMut(String)) -> Result<(), String> {
    // Both answers are read from their first line, so each keeps its head.
    let frame = |cmd: Cmd| {
        beat.proc
            .run(
                &cmd.cwd(beat.checkout)
                    .timeout(FRAME_CAP)
                    .keep(Keep::Head, Keep::Tail),
            )
            .map(|ran| ran.stdout)
            .unwrap_or_default()
    };
    let branch = frame(Cmd::new("git").args(["rev-parse", "--abbrev-ref", "HEAD"]));
    if String::from_utf8_lossy(&branch).trim_end_matches('\n') != "main" {
        return Err("the checkout is not on main".to_owned());
    }
    let Some(id) = beat.env.non_empty("CLAUDE_CODE_REMOTE_SESSION_ID") else {
        return Err("no CLAUDE_CODE_REMOTE_SESSION_ID, so no lineage".to_owned());
    };
    let lineage = format!("session_{}", id.strip_prefix("cse_").unwrap_or(id));
    let pick = frame(Cmd::new("sh").arg(HANDOFF_PICK).arg(epoch).args([
        "coordinator",
        "--lineage",
        &lineage,
        "--max-age-hours",
        "24",
    ]));
    let pick = first_line(&pick, pick.len());
    if pick.starts_with("ok ") {
        return Ok(());
    }
    if let Some(line) = malformed(&pick) {
        out(line);
    }
    let pick = if pick.is_empty() { "no answer" } else { &pick };
    Err(format!("handoff-pick.sh for {lineage}: {pick}"))
}

fn malformed(pick: &str) -> Option<String> {
    let mut words = pick.strip_prefix("refused ")?.split_whitespace().peekable();
    let mut fields = Vec::new();
    if words.next_if_eq(&"malformed").is_none() {
        while let Some(field) = words
            .next_if(|word| word.starts_with("missing-fields=") || word.starts_with("bad-fields="))
        {
            fields.push(field);
        }
        if fields.is_empty() {
            return None;
        }
    }
    let files: Vec<&str> = words.collect();
    if files.is_empty() {
        return None;
    }
    let fields = if fields.is_empty() {
        String::new()
    } else {
        format!(" ({})", fields.join(" "))
    };
    Some(format!(
        "heartbeat: malformed coordinator handoff: {}{fields}; fix its Handoff: line",
        files.join(" ")
    ))
}

fn contained<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_owned())
    })
}

fn first_line(bytes: &[u8], limit: usize) -> String {
    let head = &bytes[..bytes.len().min(limit)];
    let line = head.split(|byte| *byte == b'\n').next().unwrap_or_default();
    String::from_utf8_lossy(line).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printed(wall: Duration, budget: Duration, failed: bool) -> Vec<String> {
        let mut lines = Vec::new();
        wall_time(&mut |line| lines.push(line), "pin", wall, budget, failed);
        lines
    }

    #[test]
    fn wall_time_prints_only_on_failure_or_a_whole_second_past_the_budget() {
        let secs = Duration::from_secs_f64;
        assert!(printed(secs(60.9), secs(60.0), false).is_empty());
        assert_eq!(
            printed(secs(61.0), secs(60.0), false),
            ["tick: pin phase took 61.0 s (budget 60 s)"]
        );
        assert_eq!(
            printed(secs(0.04), secs(60.0), true),
            ["tick: pin phase took 0.0 s (budget 60 s)"]
        );
    }

    #[test]
    fn the_deadline_is_230_unless_heartbeat_deadline_is_a_whole_number() {
        let env = |value: &str| -> Env { [("HEARTBEAT_DEADLINE", value)].into_iter().collect() };
        assert_eq!(deadline(&Env::default()), Ok(230));
        assert_eq!(deadline(&env("")), Ok(230));
        assert_eq!(deadline(&env("6")), Ok(6));
        assert_eq!(deadline(&env(" -5 ")), Ok(-5));
        assert_eq!(
            deadline(&env("soon")),
            Err(
                "heartbeat: HEARTBEAT_DEADLINE=soon is not a whole number of seconds; using 230"
                    .to_owned()
            )
        );
    }

    #[test]
    fn malformed_names_the_files_handoff_pick_refused_as_malformed() {
        let dir = "store/epochs/e/handoff";
        assert_eq!(
            malformed(&format!("refused malformed {dir}/coordinator-bad.md")),
            Some(format!(
                "heartbeat: malformed coordinator handoff: {dir}/coordinator-bad.md; fix its Handoff: line"
            ))
        );
        assert_eq!(
            malformed(&format!("refused malformed {dir}/a.md {dir}/b.md")),
            Some(format!(
                "heartbeat: malformed coordinator handoff: {dir}/a.md {dir}/b.md; fix its Handoff: line"
            ))
        );
        assert_eq!(
            malformed(&format!(
                "refused missing-fields=prompt bad-fields=stamp_sha,stamp_entry {dir}/c.md"
            )),
            Some(format!(
                "heartbeat: malformed coordinator handoff: {dir}/c.md (missing-fields=prompt bad-fields=stamp_sha,stamp_entry); fix its Handoff: line"
            ))
        );
        assert_eq!(
            malformed(&format!(
                "refused missing-fields=stamp_sha,stamp_entry {dir}/c.md"
            )),
            Some(format!(
                "heartbeat: malformed coordinator handoff: {dir}/c.md (missing-fields=stamp_sha,stamp_entry); fix its Handoff: line"
            ))
        );
        assert_eq!(
            malformed(&format!("refused bad-fields=prompt {dir}/c.md")),
            Some(format!(
                "heartbeat: malformed coordinator handoff: {dir}/c.md (bad-fields=prompt); fix its Handoff: line"
            ))
        );
        for other in [
            format!("refused superseded-by {dir}/x.md"),
            format!("refused written-in-future {dir}/x.md"),
            format!("refused wrong-epoch=old {dir}/x.md"),
            format!("refused outside {dir}/x.md"),
            "refused malformed".to_owned(),
            format!("missing other-lineage {dir}/x.md"),
            format!("ok {dir}/x.md"),
            String::new(),
        ] {
            assert_eq!(malformed(&other), None, "{other}");
        }
    }

    #[test]
    fn first_line_cuts_bytes_then_takes_the_first_line() {
        assert_eq!(first_line(b"boom\nmore\n", 300), "boom");
        assert_eq!(first_line(b"\nlater", 300), "");
        assert_eq!(first_line(&[b'x'; 400], 300).len(), 300);
        assert_eq!(first_line(b"", 300), "");
    }
}
