use std::path::PathBuf;
use std::time::Duration;

use crate::tick::act::{Ctx, Report};
use crate::tick::exec::review_owed::{self, gating, listed, request};

pub const BUDGET: Duration = Duration::from_secs(30);

// Review-latency U4: a verdict is recorded at the next turn end, not when someone polls for it.
pub fn run(ctx: &Ctx) -> Report {
    let mut lines = Vec::new();
    for dir in uncollected(ctx) {
        let collected = review_owed::collect(ctx, &dir);
        // The budget cut it: the phase's wall-time line says so, and the next tick retries.
        if ctx.timed_out() {
            break;
        }
        if let Err(line) = collected {
            lines.push(line);
        }
    }
    Report {
        lines,
        failed: false,
    }
}

// Where astra-review.sh records a verdict in cm, `<NN-name>/<unit>/<step>/evidence/<dir>`;
// relative to the checkout, as it is given them.
fn uncollected(ctx: &Ctx) -> Vec<PathBuf> {
    let root = ctx.epoch_dir();
    let mut found = Vec::new();
    for area in listed(&root) {
        for unit in listed(&root.join(&area)) {
            for step in listed(&root.join(&area).join(&unit)) {
                let evidence = PathBuf::from(&area)
                    .join(&unit)
                    .join(&step)
                    .join("evidence");
                for name in listed(&root.join(&evidence)) {
                    let dir = root.join(&evidence).join(&name);
                    if dir.join("report.md").exists() {
                        continue;
                    }
                    if request(&dir).is_some_and(|request| gating(&name, &request)) {
                        found.push(ctx.epoch.join(&evidence).join(name));
                    }
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::proc::{FakeProc, Ran};

    fn gate(root: &Path, dir: &str, model: &str) {
        let dir = root.join("store/epochs/e").join(dir);
        fs::create_dir_all(&dir).unwrap();
        let request = json!({"mode": "pr", "requested": {"model": model, "effort": "xhigh"}});
        fs::write(dir.join("request.json"), request.to_string()).unwrap();
    }

    #[test]
    fn only_gates_without_a_report_under_a_steps_evidence_are_collected() {
        let ct = tempfile::tempdir().unwrap();
        let root = ct.path();
        gate(root, "01-a/u/s/evidence/astra-u-1-r1", "gpt-6-astra");
        gate(root, "01-a/u/s/evidence/astra-note-r1", "gpt-6-astra");
        gate(root, "01-a/u/s/evidence/astra-u-1-r2", "gpt-6-astra");
        fs::write(
            root.join("store/epochs/e/01-a/u/s/evidence/astra-u-1-r2/report.md"),
            "ACCEPT\n",
        )
        .unwrap();
        gate(
            root,
            "01-a/u/s/evidence/astra-u-1-r1-shadow-gpt-6.2-sol-xhigh",
            "gpt-6-astra",
        );
        gate(root, "01-a/u/s/evidence/astra-sol", "gpt-6.2-sol");
        gate(root, "01-a/u/s/evidence/.astra-hidden", "gpt-6-astra");
        gate(root, "01-a/u/s/elsewhere/astra-u-1-r1", "gpt-6-astra");
        gate(root, "01-a/u/s/x/evidence/astra-u-1-r1", "gpt-6-astra");
        gate(root, "00-b/v/t/evidence/astra-v-2-r1", "gpt-6-astra");
        let torn = root.join("store/epochs/e/00-b/v/t/evidence/astra-torn");
        fs::create_dir_all(&torn).unwrap();
        fs::write(torn.join("request.json"), "{\"requested\": {\"mod").unwrap();

        let proc = FakeProc::new(|_| Ok(Ran::code(0, "")));
        let env = Env::default();
        let ctx = Ctx::new(
            root,
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("cm"),
            Duration::from_secs(5),
        );
        assert_eq!(
            uncollected(&ctx),
            [
                "store/epochs/e/00-b/v/t/evidence/astra-v-2-r1",
                "store/epochs/e/01-a/u/s/evidence/astra-note-r1",
                "store/epochs/e/01-a/u/s/evidence/astra-u-1-r1",
            ]
            .map(PathBuf::from)
        );
    }
}
