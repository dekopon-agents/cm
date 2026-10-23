use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Writes an executable through a child `sh`, so the test process never holds a write
/// descriptor that a sibling thread's fork could inherit (exec of it would fail with ETXTBSY).
pub fn write_executable(path: &Path, text: &str) {
    let mut child = Command::new("sh")
        .args(["-c", r#"cat >"$1" && chmod 755 "$1""#, "_"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "writing {}",
        path.display()
    );
}

pub struct FakeGh {
    dir: tempfile::TempDir,
}

impl FakeGh {
    /// A `gh` that runs `script` (sh) with `$FAKE_GH` set to a scratch folder it may write.
    pub fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        let gh = dir.path().join("bin/gh");
        let fixture = dir.path().display();
        write_executable(&gh, &format!("#!/bin/sh\nFAKE_GH='{fixture}'\n{script}"));
        FakeGh { dir }
    }

    pub fn program(&self) -> PathBuf {
        self.dir.path().join("bin/gh")
    }

    pub fn fixture(&self) -> &Path {
        self.dir.path()
    }

    /// `PATH` with this `gh` first, for a `cm` subprocess.
    pub fn path(&self) -> OsString {
        let mut paths = vec![self.dir.path().join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).unwrap()
    }
}
