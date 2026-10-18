use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
        fs::write(&gh, format!("#!/bin/sh\nFAKE_GH='{fixture}'\n{script}")).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
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
