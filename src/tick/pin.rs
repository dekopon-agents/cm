use std::fs::{File, Permissions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use crate::tick::act::{Ctx, Report};
use crate::tick::beat::FRAME_CAP;
use crate::tick::claude_md::within_size_limit;
use crate::tick::directive::examined;
use crate::tick::proc::{Cmd, Keep};

pub const BUDGET: Duration = Duration::from_secs(60);

pub const SETUP: &str = "tools/cloud-env/dekopon-coordinator-setup.sh";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pin {
    version: String,
    asset: Option<String>,
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
    pin_of(&ran.stdout_text())
}

fn pin_of(rows: &str) -> Option<Pin> {
    let mut cm_rows = rows
        .split('\n')
        .map(|row| {
            examined(row.len() + 1);
            row.split('|')
        })
        .filter_map(|mut fields| (fields.next() == Some("cm")).then_some(fields));
    let mut fields = cm_rows.next()?;
    let version = fields.next().unwrap_or_default().to_owned();
    let asset = fields.next().unwrap_or_default().to_owned();
    // heartbeat.sh read a second cm row into the asset's name, which no fetch could install.
    let asset = cm_rows.next().is_none().then_some(asset);
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
    second_words(&version)
}

// awk's `{ print $2 }` under `$(...)`: each line's second blank-separated word, trailing newlines
// dropped.
fn second_words(text: &str) -> String {
    let mut words = String::new();
    for line in text.split('\n') {
        examined(line.len() + 1);
        let word = line
            .split([' ', '\t'])
            .filter(|word| !word.is_empty())
            .nth(1)
            .unwrap_or_default();
        words.push_str(word);
        words.push('\n');
    }
    words.trim_end_matches('\n').to_owned()
}

fn install(ctx: &Ctx, pin: &Pin) -> bool {
    let target = Path::new(ctx.cm);
    let Some(name) = pin.asset.as_deref().filter(|name| plain_name(name)) else {
        return false;
    };
    if !target.is_absolute() {
        return false;
    }
    // Under a relative TMPDIR heartbeat.sh's fetch was given a path relative to the folder it
    // had moved into, so it fetched nothing and kept the running cm.
    let tmpdir = Path::new(ctx.env.non_empty("TMPDIR").unwrap_or("/tmp"));
    if !tmpdir.is_absolute() {
        return false;
    }
    let Ok(tmp) = tempfile::Builder::new()
        .prefix("cm-pin-")
        .tempdir_in(tmpdir)
    else {
        return false;
    };
    let asset = tmp.path().join(name);
    // setup.sh's own temporary folders land in ours too, so a fetch the budget kills leaves none.
    let step = |cmd: Cmd| {
        ctx.run(
            cmd.cwd(tmp.path())
                .env("TMPDIR", tmp.path())
                .keep(Keep::Tail, Keep::Tail),
        )
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
        .join(name.strip_suffix(".tar.gz").unwrap_or(name))
        .join("cm");
    fetched && replace(ctx, &unpacked, target, &pin.version).is_ok()
}

fn plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

// Staged beside the cm and renamed over it, so a running cm keeps its old file and a failure at
// any step leaves the installed cm as it was. The staged copy must be a regular file that runs
// and names the pin before it replaces anything.
fn replace(ctx: &Ctx, from: &Path, target: &Path, version: &str) -> std::io::Result<()> {
    let refused = || std::io::Error::from(ErrorKind::InvalidData);
    if !std::fs::symlink_metadata(from)?.is_file() {
        return Err(refused());
    }
    let dir = target.parent().ok_or(ErrorKind::InvalidInput)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".cm-pin-")
        .tempfile_in(dir)?;
    let source = File::open(from)?;
    let len = source.metadata()?.len();
    within_size_limit(staged.as_file(), usize::try_from(len).unwrap_or(usize::MAX))?;
    std::io::copy(&mut source.take(len), staged.as_file_mut())?;
    staged
        .as_file()
        .set_permissions(Permissions::from_mode(0o755))?;
    let staged = staged.into_temp_path();
    if !names_the_pin(ctx, &staged, version) {
        return Err(refused());
    }
    staged.persist(target).map_err(|error| error.error)?;
    Ok(())
}

// Its own cap, as the frame's children have: the phase budget may be spent by a slow fetch.
fn names_the_pin(ctx: &Ctx, staged: &Path, version: &str) -> bool {
    let check = Cmd::new(staged)
        .arg("--version")
        .cwd(ctx.checkout)
        .timeout(FRAME_CAP)
        .keep(Keep::Head, Keep::Tail);
    ctx.proc
        .run(&check)
        .is_ok_and(|ran| ran.exit.success() && second_words(&ran.stdout_text()) == version)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::Duration;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::directive::{examining, grows_with_its_input};
    use crate::tick::proc::{FakeProc, KILL_AFTER, OUTPUT_CAP, Ran};

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
                asset: Some("cm-0.17.1-x86_64-unknown-linux-musl.tar.gz".into())
            })
        );
        assert_eq!(
            pin("cm|1|a\ncm|2|b\n"),
            Some(Pin {
                version: "1".into(),
                asset: None
            })
        );
        assert_eq!(
            pin("cm|3\n"),
            Some(Pin {
                version: "3".into(),
                asset: Some(String::new())
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
    fn two_cm_pins_install_nothing_and_say_so() {
        for rows in [
            "cm|0.17.1|a.tar.gz\ncm|0.17.2|b.tar.gz\n",
            "cm|0.17.1|a.tar.gz\ncm|0.17.1|a.tar.gz\n",
        ] {
            let proc = answering(rows, "cm 0.17.0\n");
            let report = ctx_run(&proc, "/bin/cm", run);
            assert_eq!(
                report.lines,
                [
                    "heartbeat: cm 0.17.1 (setup.sh's pin) could not be fetched or installed; still running cm 0.17.0"
                ],
                "{rows:?}"
            );
            assert!(report.failed);
            assert!(
                !proc
                    .log()
                    .iter()
                    .any(|call| call.contains("--fetch-pinned"))
            );
        }
    }

    #[test]
    fn the_staged_cm_is_run_with_its_own_cap_even_when_the_budget_is_spent() {
        let proc = FakeProc::new(|cmd| Ok(Ran::code(0, format!("cm {}\n", cmd.argv.len()))));
        let env = Env::default();
        let spent = Ctx::new(
            Path::new("/ct"),
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("/bin/cm"),
            Duration::ZERO,
        );
        assert!(names_the_pin(&spent, Path::new("/bin/.cm-pin-x"), "2"));
        assert!(!names_the_pin(
            &spent,
            Path::new("/bin/.cm-pin-x"),
            "0.17.1"
        ));
        let check = proc.calls().pop().unwrap();
        assert_eq!(check.line(), ".cm-pin-x --version");
        assert_eq!(check.timeout, Duration::from_secs(30));
        assert_eq!(check.cwd.as_deref(), Some(Path::new("/ct")));
        assert_eq!(
            check.timeout + KILL_AFTER,
            Duration::from_secs(35),
            "README's bound past pin's budget"
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

    const SIZES: [usize; 3] = [1 << 18, 1 << 19, 1 << 20];

    type Output = (&'static str, fn(usize) -> String);

    const LIST_PINS: [Output; 5] = [
        ("one cm row as long as the output", |size| {
            format!("cm|0.17.1|{}\n", "a".repeat(size))
        }),
        ("a cm row on every line", |size| {
            "cm|0.17.1|a.tar.gz\n".repeat(size / 19)
        }),
        ("other rows before the cm row", |size| {
            "uv|1|a|b|c\n".repeat(size / 11) + "cm|0.17.1|a.tar.gz\n"
        }),
        ("a cm row of many fields", |size| {
            format!("cm|0.17.1{}\n", "|f".repeat(size / 2))
        }),
        ("CRLF line ends and NUL bytes", |size| {
            "x|\0|y\r\n".repeat(size / 7) + "cm|0.17.1|a\0.tar.gz\r\n"
        }),
    ];

    const VERSIONS: [Output; 4] = [
        ("a version line on every line", |size| {
            "cm 0.17.1\n".repeat(size / 10)
        }),
        ("one line of many words", |size| "cm ".repeat(size / 3)),
        ("blanks and tabs only", |size| " \t".repeat(size / 2)),
        ("CRLF line ends and NUL bytes", |size| {
            "cm 0.17\0.1\r\n".repeat(size / 11)
        }),
    ];

    #[test]
    fn every_shape_of_pin_and_version_output_costs_work_in_proportion_to_its_bytes() {
        for (shapes, read) in [
            (&LIST_PINS[..], (|text| drop(pin_of(text))) as fn(&str)),
            (&VERSIONS[..], |text| drop(second_words(text))),
        ] {
            for (shape, output) in shapes {
                let texts: Vec<String> = SIZES.iter().map(|&size| output(size)).collect();
                assert!(texts.iter().all(|text| text.len() <= OUTPUT_CAP));
                let work: Vec<usize> = texts.iter().map(|text| examining(|| read(text))).collect();
                let sizes: Vec<usize> = texts.iter().map(String::len).collect();
                grows_with_its_input(shape, &sizes, &work, 4);
            }
        }
    }

    #[test]
    fn the_pin_reader_is_handed_at_most_the_first_4_mib_of_each_child_s_output() {
        let proc = answering(PINS, "cm 0.17.1\n");
        ctx_run(&proc, "/bin/cm", run);
        let calls = proc.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|cmd| cmd.keep_stdout == Keep::Head));
    }
}
