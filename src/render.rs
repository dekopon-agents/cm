use std::collections::BTreeMap;

use jiff::Timestamp;

use crate::plan::{Next, Units};
use crate::state::{Lane, Phase, Step, UnitState, Watch, WatchStatus, Work};
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
    if state.steps.values().any(|step| !step.parts.is_empty()) {
        out.push_str(
            "## Parts\n\n| Step | Part | State | Since | Evidence |\n|---|---|---|---|---|\n",
        );
        for (name, step) in &state.steps {
            for (part, track) in &step.parts {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    cell(name),
                    cell(part),
                    cell(&track.state.to_string()),
                    track.since,
                    cell(&track.evidence.summary()),
                ));
            }
        }
        out.push('\n');
    }
    let holds: Vec<String> = state
        .steps
        .iter()
        .flat_map(|(name, step)| holds(name, step))
        .collect();
    if !holds.is_empty() {
        out.push_str("## Holds\n\n");
        for line in holds {
            out.push_str(&format!("- {line}\n"));
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

fn holds(name: &str, step: &Step) -> Vec<String> {
    let pins = step.pins.iter().map(|pin| {
        format!(
            "{name}: evidence `{}` pinned to {}{}{}",
            pin.file,
            pin.candidate,
            pin.harness
                .as_ref()
                .map(|harness| format!(" under {harness}"))
                .unwrap_or_default(),
            pin.stale
                .as_ref()
                .map(|stale| format!("; stale: {stale}"))
                .unwrap_or_default()
        )
    });
    let exceptions = step.known_red.iter().map(|exception| {
        format!(
            "{name}: known-red `{}` at {} until {}, owner {}: {}",
            exception.check,
            if exception.standing {
                if exception.campaign {
                    "any head (campaign)"
                } else {
                    "any head (step)"
                }
            } else {
                &exception.commit
            },
            exception.until,
            exception.owner,
            exception.reason
        )
    });
    let waived = step.waived.iter().map(|waived| {
        format!(
            "{name}: `{}` at {} waived by --override {}",
            waived.check, waived.commit, waived.file
        )
    });
    let watches = step.watches.iter().map(|watch| watch_record(name, watch));
    pins.chain(exceptions)
        .chain(waived)
        .chain(watches)
        .map(|line| cell(&line))
        .collect()
}

fn watch_record(name: &str, watch: &Watch) -> String {
    let status = match (&watch.withdrawn, &watch.resolved, &watch.expired) {
        (Some(at), _, _) => format!("withdrawn {at}"),
        (None, Some(resolved), _) => format!("resolved {} by {}", resolved.at, resolved.evidence),
        (None, None, Some(at)) => format!("expired: unresolved since {at}"),
        (None, None, None) => "open".to_owned(),
    };
    format!(
        "{name}#{}: watch {} by {}, {status}",
        watch.id, watch.signal, watch.deadline
    )
}

fn signal_preview(watch: &Watch) -> String {
    const MAX_CHARS: usize = 80;
    let signal = watch.signal.to_string();
    let mut chars = signal.chars().map(|ch| {
        if ch.is_control() || ch.is_whitespace() {
            ' '
        } else {
            ch
        }
    });
    let mut preview: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        preview.pop();
        preview.push('…');
    }
    preview
}

fn watch_summary(root: &std::path::Path, now: Timestamp, units: &Units) -> Vec<String> {
    let mut counts = [0usize; 3];
    let mut qualifying = Vec::new();
    for (unit, state) in units {
        for (name, step) in &state.steps {
            if step.track.state.is_final() {
                continue;
            }
            for watch in &step.watches {
                let status = watch.status(now, root);
                let (group, instant) = match status {
                    WatchStatus::Open => (0, crate::plan::instant(&watch.deadline)),
                    WatchStatus::Expired => (
                        1,
                        watch
                            .expired
                            .as_deref()
                            .and_then(crate::plan::instant)
                            .or_else(|| crate::plan::instant(&watch.deadline)),
                    ),
                    WatchStatus::Withdrawn => {
                        let Some(at) = watch.withdrawn.as_deref().and_then(crate::plan::instant)
                        else {
                            continue;
                        };
                        if at > now
                            || now.duration_since(at) > jiff::SignedDuration::from_secs(24 * 3600)
                        {
                            continue;
                        }
                        (2, Some(at))
                    }
                    WatchStatus::Resolved => continue,
                };
                counts[group] += 1;
                qualifying.push((group, instant, unit, name, watch, status));
            }
        }
    }
    qualifying.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| {
                if a.0 == 0 {
                    a.1.cmp(&b.1)
                } else {
                    b.1.cmp(&a.1)
                }
            })
            .then_with(|| (a.2, a.3, a.4.id).cmp(&(b.2, b.3, b.4.id)))
    });
    let mut rows = vec![format!(
        "- Counts: open {}; expired-unresolved {}; withdrawn-recent {}",
        counts[0], counts[1], counts[2]
    )];
    for (_, _, unit, name, watch, status) in qualifying.iter().take(10) {
        rows.push(format!(
            "- {unit}/{name}#{}: {}, deadline {}; {}",
            watch.id,
            status.name(),
            watch.deadline,
            cell(&signal_preview(watch))
        ));
    }
    if qualifying.len() > 10 {
        rows.push(format!(
            "- … and {} more: see the unit STATE.md",
            qualifying.len() - 10
        ));
    }
    rows
}

pub(crate) fn next_item(number: &mut usize, description: &str, command: &str) -> String {
    let description: String = description
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let line = format!("{number}. {description} — `{command}`");
    *number += 1;
    line
}

pub fn card(
    root: &std::path::Path,
    now: Timestamp,
    name: &str,
    units: &Units,
    next: &Next,
    moves: &[Moved],
) -> String {
    let at = crate::host::stamp(now);
    let mut out = format!(
        "Last run: {at}\n\n# {name} card\n\nRendered by `cm sync` from cm state after reading GitHub; the next sync overwrites it.\n\n"
    );
    if next.paused {
        out.push_str("**Paused:** LIMITS.toml has `paused = true`; nothing launches.\n\n");
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut flight = Vec::new();
    let mut blocked = Vec::new();
    let mut superseded = Vec::new();
    let mut number = 1;
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
                Phase::Superseded if !step.track.evidence.superseded_by.is_empty() => {
                    let successors: Vec<String> = step
                        .track
                        .evidence
                        .superseded_by
                        .iter()
                        .map(|successor| format!("`{successor}`"))
                        .collect();
                    superseded.push(format!(
                        "- `{target}` superseded by {}",
                        successors.join(", ")
                    ));
                }
                Phase::Blocked(reason) => {
                    let met = reason
                        .strip_prefix("until:")
                        .map(|condition| crate::plan::block_met(root, units, unit, condition));
                    let command = if met == Some(false) {
                        "cm next".to_owned()
                    } else {
                        crate::plan::unblock_command(&target)
                    };
                    blocked.push((
                        format!(
                            "`{target}` {} since {}{}",
                            step.track.state,
                            step.track.since,
                            met.map(|met| format!(
                                "; condition {}",
                                if met { "holds" } else { "does not hold" }
                            ))
                            .unwrap_or_default(),
                        ),
                        command,
                    ));
                }
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
        .map(|(target, lane)| {
            next_item(
                &mut number,
                &format!("`{target}` ({})", lane.map_or("—", Lane::name)),
                &crate::plan::launch_command(target, *lane),
            )
        })
        .collect();
    let waiting: Vec<String> = next
        .waiting
        .iter()
        .filter(|(_, on)| !on.is_empty())
        .map(|(target, on)| {
            next_item(
                &mut number,
                &format!("`{target}` waits on {}", on.join("; ")),
                &crate::plan::waiting_command(root, target),
            )
        })
        .collect();
    let blocked: Vec<String> = blocked
        .iter()
        .map(|(line, command)| next_item(&mut number, line, command))
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
    match crate::ledger::mac_lane_open(root) {
        Ok(mac) if crate::ledger::lint_mac_lane(root).is_empty() => {
            if !mac.is_empty() {
                let mut rows = vec![
                    format!(
                        "{} open {}",
                        mac.len(),
                        if mac.len() == 1 { "item" } else { "items" }
                    ),
                    String::new(),
                ];
                rows.extend(mac.iter().take(10).map(|item| {
                    let title: String = item
                        .title
                        .chars()
                        .map(|character| {
                            if character.is_whitespace() || character.is_control() {
                                ' '
                            } else {
                                character
                            }
                        })
                        .take(80)
                        .collect();
                    format!("- #{} {} ({})", item.number, title, item.state.name())
                }));
                if mac.len() > 10 {
                    rows.push(format!("- … and {} more: see MAC-LANE.md", mac.len() - 10));
                }
                section(&mut out, "Mac lane", &rows, "");
            }
        }
        Ok(_) | Err(_) => section(
            &mut out,
            "Mac lane",
            &["Mac lane: invalid, see cm lint".to_owned()],
            "",
        ),
    }
    section(&mut out, "Next", &ready, "Nothing is launchable.");
    section(&mut out, "Waiting", &waiting, "Nothing waits.");
    section(&mut out, "In flight", &flight, "Nothing is in flight.");
    section(&mut out, "Blocked", &blocked, "Nothing is blocked.");
    if !superseded.is_empty() {
        section(&mut out, "Superseded", &superseded, "");
    }
    section(&mut out, "Watches", &watch_summary(root, now, units), "");
    section(&mut out, "Moved this run", &moved, "No moves.");
    out
}
