use std::path::PathBuf;

pub fn destination() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("kickoff.md");
    (temp, path)
}
