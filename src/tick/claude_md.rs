use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tick::act::{Ctx, Report};

pub const BUDGET: Duration = Duration::from_secs(5);

pub const BOOT_TIME: &str = "# dekopon-coordinator cloud environment";

pub const IMPORT: &str = "@/home/user/campaign-trail/tools/cloud-env/COORDINATOR.md";

pub fn run(ctx: &Ctx) -> Report {
    let Some(home) = ctx.env.get("HOME") else {
        return Report::default();
    };
    let path = PathBuf::from(format!("{home}/.claude/CLAUDE.md"));
    if !boot_time(&path) {
        return Report::default();
    }
    match import(&path) {
        Ok(()) => Report {
            lines: vec![
                "rules: now imported from tools/cloud-env/COORDINATOR.md; read it".to_owned(),
            ],
            failed: false,
        },
        Err(_) => Report {
            lines: vec![format!(
                "heartbeat: could not write the COORDINATOR.md import to {}",
                path.display()
            )],
            failed: true,
        },
    }
}

fn boot_time(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut first = Vec::new();
    let limit = BOOT_TIME.len() as u64 + 2;
    if BufReader::new(file)
        .take(limit)
        .read_until(b'\n', &mut first)
        .is_err()
    {
        return false;
    }
    first.strip_suffix(b"\n").unwrap_or(&first) == BOOT_TIME.as_bytes()
}

// Renamed over from a staged file, so a failed write leaves the boot-time rules for the next
// tick to replace.
fn import(path: &Path) -> std::io::Result<()> {
    let path = std::fs::canonicalize(path)?;
    let dir = path.parent().ok_or(ErrorKind::InvalidInput)?;
    let mode = std::fs::metadata(&path)?.permissions();
    let mut staged = tempfile::Builder::new()
        .prefix(".CLAUDE.md-")
        .tempfile_in(dir)?;
    staged.write_all(format!("{IMPORT}\n").as_bytes())?;
    staged.as_file().set_permissions(mode)?;
    staged.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::tick::act::Env;
    use crate::tick::proc::{FakeProc, Ran};

    fn tick(home: Option<&Path>) -> Report {
        let proc = FakeProc::new(|_| Ok(Ran::code(0, "")));
        let env: Env = home
            .map(|home| ("HOME", home.to_string_lossy().into_owned()))
            .into_iter()
            .collect();
        let ctx = Ctx::new(
            Path::new("."),
            Path::new("store/epochs/e"),
            &proc,
            &env,
            OsStr::new("cm"),
            BUDGET,
        );
        let report = run(&ctx);
        assert!(proc.calls().is_empty(), "no child runs");
        report
    }

    #[test]
    fn the_first_line_must_be_the_boot_time_heading_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        for (text, boot) in [
            ("# dekopon-coordinator cloud environment\nold rules\n", true),
            ("# dekopon-coordinator cloud environment", true),
            ("# dekopon-coordinator cloud environment\n", true),
            ("# dekopon-coordinator cloud environment \nold\n", false),
            ("# dekopon-coordinator cloud environment\r\nold\n", false),
            ("# dekopon-coordinator cloud environments\n", false),
            ("\n# dekopon-coordinator cloud environment\n", false),
            ("# dekopon-coordinator cloud", false),
            ("", false),
        ] {
            fs::write(&path, text).unwrap();
            assert_eq!(boot_time(&path), boot, "{text:?}");
        }
        assert!(!boot_time(&dir.path().join("missing")));
        assert!(!boot_time(dir.path()), "a folder is not a CLAUDE.md");
    }

    #[test]
    fn without_home_nothing_is_read_or_written() {
        assert_eq!(tick(None), Report::default());
    }

    #[test]
    fn the_boot_time_file_is_replaced_once_keeping_its_mode_and_any_link() {
        let home = tempfile::tempdir().unwrap();
        let claude = home.path().join(".claude");
        fs::create_dir(&claude).unwrap();
        let rules = home.path().join("rules.md");
        fs::write(&rules, format!("{BOOT_TIME}\nold rules\n")).unwrap();
        fs::set_permissions(&rules, fs::Permissions::from_mode(0o640)).unwrap();
        let path = claude.join("CLAUDE.md");
        std::os::unix::fs::symlink(&rules, &path).unwrap();
        assert_eq!(
            tick(Some(home.path())).lines,
            ["rules: now imported from tools/cloud-env/COORDINATOR.md; read it"]
        );
        assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&rules).unwrap(), format!("{IMPORT}\n"));
        assert_eq!(
            fs::metadata(&rules).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(tick(Some(home.path())), Report::default(), "once");
        let left: Vec<_> = fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "no staged file is left: {left:?}");
    }
}
