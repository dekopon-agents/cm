use std::collections::BTreeMap;

use crate::plan::{Next, Units};
use crate::state::{Lane, Phase, UnitState, Work};
use crate::sync::Moved;

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

pub fn unit(state: &UnitState) -> String {
    let mut out = format!(
        "# {} state\n\nRendered by cm from `.cm/state.json`. Change it with `cm launch` and `cm advance`; `cm lint` fails on a hand edit.\n\n",
        state.unit
    );
    out.push_str("| Unit | State | Since | Evidence |\n|---|---|---|---|\n");
    out.push_str(&format!(
        "| {} | {} | {} | {} |\n\n",
        cell(&state.unit),
        cell(&state.track.state.to_string()),
        state.track.since,
        cell(&state.track.evidence.summary()),
    ));
    out.push_str("## Alive\n\n");
    if state.steps.is_empty() {
        out.push_str("No step has launched.\n\n");
    } else {
        out.push_str("| Step | State | Since | Launches | Worktree | Evidence |\n|---|---|---|---|---|---|\n");
        for (name, step) in &state.steps {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                cell(name),
                cell(&step.track.state.to_string()),
                step.track.since,
                step.launches,
                if step.worktree.is_empty() {
                    "—".to_owned()
                } else {
                    format!("`{}`", cell(&step.worktree))
                },
                cell(&step.track.evidence.summary()),
            ));
        }
        out.push('\n');
    }
    if state
        .steps
        .values()
        .any(|step| step.lane.is_some() || !step.after.is_empty())
    {
        out.push_str("## Order\n\n| Step | Lane | After |\n|---|---|---|\n");
        for (name, step) in &state.steps {
            let after: Vec<String> = step.after.iter().map(ToString::to_string).collect();
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                cell(name),
                match (step.lane, step.work) {
                    (None, Work::Code) => "—".to_owned(),
                    (None, Work::Study) => "study".to_owned(),
                    (Some(lane), Work::Code) => lane.name().to_owned(),
                    (Some(lane), Work::Study) => format!("{lane}, study"),
                },
                if after.is_empty() {
                    "—".to_owned()
                } else {
                    cell(&after.join(", "))
                },
            ));
        }
        out.push('\n');
    }
    out.push_str("## History\n\n");
    if state.history.is_empty() {
        out.push_str("No moves yet.\n");
    } else {
        out.push_str("| At | Target | From | To | Evidence |\n|---|---|---|---|---|\n");
        for item in &state.history {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                item.at,
                cell(&item.target),
                cell(&item.from.to_string()),
                cell(&item.to.to_string()),
                cell(&item.evidence.summary()),
            ));
        }
    }
    out
}

pub fn card(at: &str, name: &str, units: &Units, next: &Next, moves: &[Moved]) -> String {
    let mut out = format!(
        "Last run: {at}\n\n# {name} card\n\nRendered by `cm sync` from cm state after reading GitHub; the next sync overwrites it.\n\n"
    );
    if next.paused {
        out.push_str("**Paused:** LIMITS.toml has `paused = true`; nothing launches.\n\n");
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut flight = Vec::new();
    let mut blocked = Vec::new();
    for (unit, state) in units {
        for (step_name, step) in &state.steps {
            let target = format!("{unit}/{step_name}");
            let key = match &step.track.state {
                Phase::Blocked(_) => "blocked".to_owned(),
                other => other.to_string(),
            };
            *counts.entry(key).or_default() += 1;
            match &step.track.state {
                Phase::Launched | Phase::PrOpen => flight.push(format!(
                    "- `{target}` {}{}",
                    step.track.state,
                    step.track
                        .evidence
                        .pr
                        .as_deref()
                        .map(|pr| format!(" {pr}"))
                        .unwrap_or_default()
                )),
                Phase::Blocked(_) => blocked.push(format!(
                    "- `{target}` {} since {}",
                    step.track.state, step.track.since
                )),
                _ => {}
            }
        }
    }
    out.push_str("| State | Steps |\n|---|---|\n");
    let order = crate::state::ORDER
        .iter()
        .map(ToString::to_string)
        .chain(["superseded".to_owned(), "blocked".to_owned()]);
    for state in order {
        if let Some(count) = counts.get(&state) {
            out.push_str(&format!("| {state} | {count} |\n"));
        }
    }
    if counts.is_empty() {
        out.push_str("| — | 0 |\n");
    }
    let section = |out: &mut String, title: &str, lines: &[String], empty: &str| {
        out.push_str(&format!("\n## {title}\n\n"));
        if lines.is_empty() {
            out.push_str(empty);
            out.push('\n');
        } else {
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
        }
    };
    let ready: Vec<String> = next
        .ready
        .iter()
        .map(|(target, lane)| format!("- `{target}` ({})", lane.map_or("—", Lane::name)))
        .collect();
    let waiting: Vec<String> = next
        .waiting
        .iter()
        .map(|(target, on)| format!("- `{target}` waits on {}", on.join("; ")))
        .collect();
    let moved: Vec<String> = moves
        .iter()
        .map(|moved| {
            format!(
                "- `{}` {} → {} ({})",
                moved.target,
                moved.from,
                moved.to,
                moved.evidence.summary()
            )
        })
        .collect();
    section(&mut out, "Next", &ready, "Nothing is launchable.");
    section(&mut out, "Waiting", &waiting, "Nothing waits.");
    section(&mut out, "In flight", &flight, "Nothing is in flight.");
    section(&mut out, "Blocked", &blocked, "Nothing is blocked.");
    section(&mut out, "Moved this run", &moved, "No moves.");
    out
}
