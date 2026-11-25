use std::fs::{File, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use crate::tick::act::{Ctx, Report};
use crate::tick::proc::{Cmd, Keep};

pub const BUDGET: Duration = Duration::from_secs(60);

pub const SETUP: &str = "tools/cloud-env/dekopon-coordinator-setup.sh";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pin {
    version: String,
    asset: String,
}

pub fn run(ctx: &Ctx) -> Report {
    let Some(pin) = pinned(ctx) else {
        return Report::default();
    };
    let running = running(ctx);
    if running == pin.version {
        return Report::default();
    }
    if install(ctx, &pin) {
        return Report::default();
    }
    let running = if running.is_empty() { "none" } else { &running };
    Report {
        lines: vec![format!(
            "heartbeat: cm {} (setup.sh's pin) could not be fetched or installed; still running cm {running}",
            pin.version
        )],
        failed: true,
    }
}

fn pinned(ctx: &Ctx) -> Option<Pin> {
    let ran = ctx
        .run(
            Cmd::new("bash")
                .arg(ctx.checkout.join(SETUP))
                .arg("--list-pins")
                .keep(Keep::Head, Keep::Tail),
        )
        .ok()?;
    let rows = ran.stdout_text();
    let mut fields = rows
        .split('\n')
        .map(|row| row.split('|'))
        .find_map(|mut fields| (fields.next() == Some("cm")).then_some(fields))?;
    let version = fields.next().unwrap_or_default().to_owned();
    let asset = fields.next().unwrap_or_default().to_owned();
    (!version.is_empty()).then_some(Pin { version, asset })
}

fn running(ctx: &Ctx) -> String {
    let version = ctx
        .run(
            Cmd::new(ctx.cm)
                .arg("--version")
                .keep(Keep::Head, Keep::Tail),
        )
        .map(|ran| ran.stdout_text())
        .unwrap_or_default();
    let words: Vec<&str> = version
        .split('\n')
        .map(|line| {
            line.split([' ', '\t'])
                .filter(|word| !word.is_empty())
                .nth(1)
                .unwrap_or_default()
        })
        .collect();
    words.join("\n").trim_end_matches('\n').to_owned()
}

fn install(ctx: &Ctx, pin: &Pin) -> bool {
    let target = Path::new(ctx.cm);
    if !target.is_absolute() || !plain_name(&pin.asset) {
        return false;
    }
    let Ok(tmp) = tempfile::Builder::new().prefix("cm-pin-").tempdir_in(
        ctx.checkout
            .join(ctx.env.non_empty("TMPDIR").unwrap_or("/tmp")),
    ) else {
        return false;
    };
    let asset = tmp.path().join(&pin.asset);
    let step = |cmd: Cmd| {
        ctx.run(cmd.cwd(tmp.path()).keep(Keep::Tail, Keep::Tail))
            .is_ok_and(|ran| ran.exit.success())
    };
    let fetched = step(
        Cmd::new("bash")
            .arg(ctx.checkout.join(SETUP))
            .args(["--fetch-pinned", "cm"])
            .arg(&asset),
    ) && step(Cmd::new("tar").arg("-xzf").arg(&asset));
    let unpacked = tmp
        .path()
        .join(pin.asset.strip_suffix(".tar.gz").unwrap_or(&pin.asset))
        .join("cm");
    fetched && replace(&unpacked, target).is_ok()
}

fn plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

// Staged beside the cm and renamed over it, so a running cm keeps its old file and a failure at
// any step leaves the installed cm as it was.
fn replace(from: &Path, target: &Path) -> std::io::Result<()> {
    let dir = target.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".cm-pin-")
        .tempfile_in(dir)?;
    std::io::copy(&mut File::open(from)?, staged.as_file_mut())?;
    staged
        .as_file()
        .set_permissions(Permissions::from_mode(0o755))?;
    staged.as_file().sync_all()?;
    staged.persist(target).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::Duration;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::proc::{FakeProc, Ran};

    fn ctx_run<T>(proc: &FakeProc, cm: &str, body: impl FnOnce(&Ctx) -> T) -> T {
        let env = Env::default();
        let ctx = Ctx::new(
            Path::new("/ct"),
            Path::new("store/epochs/e"),
            proc,
            &env,
            OsStr::new(cm),
            Duration::from_secs(5),
        );
        body(&ctx)
    }

    fn answering(pins: &'static str, version: &'static str) -> FakeProc {
        FakeProc::new(move |cmd| {
            Ok(match cmd.argv.get(2).and_then(|arg| arg.to_str()) {
                Some("--list-pins") => Ran::code(0, pins),
                Some("--fetch-pinned") => Ran::code(1, ""),
                _ if cmd.line().ends_with("--version") => Ran::code(0, version),
                _ => Ran::code(0, ""),
            })
        })
    }

    const PINS: &str = "cloudflared|2026.10.1|cloudflared-linux-amd64|https://x|aa\ncm|0.17.1|cm-0.17.1-x86_64-unknown-linux-musl.tar.gz|https://y|bb\n";

    #[test]
    fn the_pin_is_the_first_cm_row_of_list_pins() {
        let pin = |rows: &'static str| ctx_run(&answering(rows, ""), "/bin/cm", pinned);
        assert_eq!(
            pin(PINS),
            Some(Pin {
                version: "0.17.1".into(),
                asset: "cm-0.17.1-x86_64-unknown-linux-musl.tar.gz".into()
            })
        );
        assert_eq!(
            pin("cm|1|a\ncm|2|b\n"),
            Some(Pin {
                version: "1".into(),
                asset: "a".into()
            })
        );
        assert_eq!(
            pin("cm|3\n"),
            Some(Pin {
                version: "3".into(),
                asset: String::new()
            })
        );
        for none in ["", "cm\n", "cm||a\n", "cmx|1|a\n", " cm|1|a\n", "uv|1|a\n"] {
            assert_eq!(pin(none), None, "{none:?}");
        }
    }

    #[test]
    fn the_running_version_is_the_second_word_of_cm_version() {
        let version = |out: &'static str| ctx_run(&answering(PINS, out), "/bin/cm", running);
        assert_eq!(version("cm 0.17.1\n"), "0.17.1");
        assert_eq!(version("cm\n"), "");
        assert_eq!(version(""), "");
        assert_eq!(version("cm 1 extra\nnext 2\n"), "1\n2");
    }

    #[test]
    fn a_pinned_cm_already_running_runs_no_fetch() {
        let proc = answering(PINS, "cm 0.17.1\n");
        assert_eq!(ctx_run(&proc, "/bin/cm", run), Report::default());
        assert_eq!(
            proc.log(),
            [
                "bash /ct/tools/cloud-env/dekopon-coordinator-setup.sh --list-pins",
                "cm --version"
            ]
        );
        let proc = answering("", "cm 0.0.1\n");
        assert_eq!(ctx_run(&proc, "/bin/cm", run), Report::default());
        assert_eq!(proc.log().len(), 1, "no pin, no version check");
    }

    #[test]
    fn a_cm_named_by_no_absolute_path_is_never_replaced() {
        let proc = answering(PINS, "");
        let report = ctx_run(&proc, "cm", run);
        assert_eq!(
            report.lines,
            [
                "heartbeat: cm 0.17.1 (setup.sh's pin) could not be fetched or installed; still running cm none"
            ]
        );
        assert!(report.failed);
        assert!(
            !proc
                .log()
                .iter()
                .any(|call| call.contains("--fetch-pinned"))
        );
    }

    #[test]
    fn an_asset_that_is_not_a_plain_file_name_is_never_fetched() {
        for asset in ["", ".", "..", "../cm.tar.gz", "a/b.tar.gz"] {
            assert!(!plain_name(asset), "{asset:?}");
        }
        assert!(plain_name("cm-0.17.1-x86_64-unknown-linux-musl.tar.gz"));
        let proc = answering("cm|0.17.1|../x.tar.gz\n", "cm 0.17.0\n");
        let report = ctx_run(&proc, "/bin/cm", run);
        assert!(report.failed);
        assert!(
            !proc
                .log()
                .iter()
                .any(|call| call.contains("--fetch-pinned"))
        );
    }
}
