use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::os::unix::fs::MetadataExt;

use crate::tick::act::{Ctx, Env, Report};
use crate::tick::proc::FileSizeLimit;

pub const BUDGET: Duration = Duration::from_secs(5);

pub const BOOT_TIME: &str = "# dekopon-coordinator cloud environment";

pub const IMPORT: &str = "@/home/user/campaign-trail/tools/cloud-env/COORDINATOR.md";

pub fn run(ctx: &Ctx) -> Report {
    let Some(path) = claude_md(ctx.env) else {
        return Report::default();
    };
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

// `$HOME/.claude/CLAUDE.md` as heartbeat.sh spelled it: an empty HOME is `/.claude/CLAUDE.md`.
fn claude_md(env: &Env) -> Option<PathBuf> {
    env.get("HOME")
        .map(|home| PathBuf::from(format!("{home}/.claude/CLAUDE.md")))
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
// tick to replace. heartbeat.sh's `printf >` wrote in place, which kept the file's owner, group
// and other links: a file whose owner or group is not this process's, one with another link, or
// one beside which no staged file can be made is written in place, never cut short first.
fn import(path: &Path) -> std::io::Result<()> {
    let path = std::fs::canonicalize(path)?;
    let dir = path.parent().ok_or(ErrorKind::InvalidInput)?;
    let text = format!("{IMPORT}\n");
    let meta = std::fs::metadata(&path)?;
    if meta.uid() != rustix::process::geteuid().as_raw()
        || meta.gid() != rustix::process::getegid().as_raw()
        || meta.nlink() > 1
    {
        return in_place(&path, &text);
    }
    let Ok(mut staged) = tempfile::Builder::new()
        .prefix(".CLAUDE.md-")
        .tempfile_in(dir)
    else {
        return in_place(&path, &text);
    };
    within_size_limit(staged.as_file(), text.len())?;
    staged.write_all(text.as_bytes())?;
    staged.as_file().set_permissions(meta.permissions())?;
    staged.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

fn in_place(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    within_size_limit(&file, text.len())?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)
}

// A write past RLIMIT_FSIZE would end the tick with SIGXFSZ, so it is refused before it is made.
pub(crate) fn within_size_limit(file: &File, len: usize) -> std::io::Result<()> {
    match FileSizeLimit::of(file) {
        Some(limit) if !limit.fits(file, len) => Err(ErrorKind::FileTooLarge.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::tick::proc::{FakeProc, Ran};

    // Beside the test binary, so the tests need nothing of the runner's TMPDIR.
    fn own_dir() -> tempfile::TempDir {
        let exe = std::env::current_exe().unwrap();
        tempfile::Builder::new()
            .prefix("claude-md-test-")
            .tempdir_in(exe.parent().unwrap())
            .unwrap()
    }

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

    fn boot_time_file(home: &Path) -> PathBuf {
        let claude = home.join(".claude");
        fs::create_dir(&claude).unwrap();
        let path = claude.join("CLAUDE.md");
        fs::write(&path, format!("{BOOT_TIME}\nold rules\n")).unwrap();
        path
    }

    #[test]
    fn the_file_is_home_s_claude_md_and_an_empty_home_is_the_root_s() {
        let env =
            |home: Option<&str>| -> Env { home.map(|home| ("HOME", home)).into_iter().collect() };
        assert_eq!(claude_md(&env(None)), None);
        assert_eq!(
            claude_md(&env(Some(""))),
            Some(PathBuf::from("/.claude/CLAUDE.md"))
        );
        assert_eq!(
            claude_md(&env(Some("/h"))),
            Some(PathBuf::from("/h/.claude/CLAUDE.md"))
        );
    }

    #[test]
    fn a_file_with_another_link_is_written_in_place_so_every_link_gets_the_import() {
        let home = own_dir();
        let path = boot_time_file(home.path());
        let other = home.path().join("rules");
        fs::hard_link(&path, &other).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        assert_eq!(
            tick(Some(home.path())).lines,
            ["rules: now imported from tools/cloud-env/COORDINATOR.md; read it"]
        );
        let meta = fs::metadata(&path).unwrap();
        assert_eq!((meta.ino(), meta.nlink()), (inode, 2));
        assert_eq!(fs::read_to_string(&other).unwrap(), format!("{IMPORT}\n"));
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_file_another_user_owns_is_written_in_place_and_keeps_its_owner() {
        let home = own_dir();
        let path = boot_time_file(home.path());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let me = rustix::process::geteuid().as_raw();
        let other = if me == 65534 { 65533 } else { 65534 };
        // Only root can give a file away; any other user checks that its own file is renamed over.
        let given = std::os::unix::fs::chown(&path, Some(other), Some(other)).is_ok();
        let before = fs::metadata(&path).unwrap();
        assert_eq!(
            tick(Some(home.path())).lines,
            ["rules: now imported from tools/cloud-env/COORDINATOR.md; read it"]
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{IMPORT}\n"));
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
        if given {
            assert_eq!(
                (after.ino(), after.uid(), after.gid()),
                (before.ino(), other, other)
            );
        } else {
            assert_ne!(after.ino(), before.ino());
            assert_eq!(after.uid(), me);
        }
    }

    #[test]
    fn the_first_line_must_be_the_boot_time_heading_alone() {
        let dir = own_dir();
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
        let home = own_dir();
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

    #[test]
    fn an_import_written_in_place_keeps_the_file_its_mode_and_nothing_of_the_old_rules() {
        use std::os::unix::fs::MetadataExt;
        let dir = own_dir();
        let path = dir.path().join("CLAUDE.md");
        for old in [
            format!("{BOOT_TIME}\n{}\n", "old rules ".repeat(40)),
            "x".to_owned(),
        ] {
            fs::write(&path, &old).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o604)).unwrap();
            let inode = fs::metadata(&path).unwrap().ino();
            in_place(&path, &format!("{IMPORT}\n")).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), format!("{IMPORT}\n"));
            let meta = fs::metadata(&path).unwrap();
            assert_eq!(
                (meta.ino(), meta.permissions().mode() & 0o777),
                (inode, 0o604)
            );
        }
    }

    #[test]
    fn a_claude_folder_that_refuses_a_staged_file_still_gets_the_import() {
        let home = own_dir();
        let claude = home.path().join(".claude");
        fs::create_dir(&claude).unwrap();
        let path = claude.join("CLAUDE.md");
        fs::write(&path, format!("{BOOT_TIME}\nold rules\n")).unwrap();
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o555)).unwrap();
        let report = tick(Some(home.path()));
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            report.lines,
            ["rules: now imported from tools/cloud-env/COORDINATOR.md; read it"]
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{IMPORT}\n"));
        let left: Vec<_> = fs::read_dir(&claude)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["CLAUDE.md"]);
    }
}
