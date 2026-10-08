use crate::state::UnitState;

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
                "| {} | {} | {} | {} | `{}` | {} |\n",
                cell(name),
                cell(&step.track.state.to_string()),
                step.track.since,
                step.launches,
                cell(&step.worktree),
                cell(&step.track.evidence.summary()),
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
