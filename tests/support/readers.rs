use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cm::forge::Forge;
use cm::host::Host;
use cm::outcome::Outcome;
use cm::sync::{BUDGET, Reader, sync_with};

pub trait Twin: Forge + Sync + Sized {
    fn twin(&self, root: &Path) -> Self;
}

// The concurrent reads `cm sync` ships must report and write what the serial sync does.
pub fn both<F: Twin>(host: &dyn Host, forge: &F, root: &Path) -> anyhow::Result<Outcome> {
    let copy = tempfile::tempdir().unwrap();
    let twin = copy.path().join(root.file_name().unwrap());
    let copied = Command::new("cp")
        .arg("-a")
        .arg(root)
        .arg(&twin)
        .status()
        .unwrap();
    assert!(copied.success());
    let serial = cm::sync::sync(host, forge, root);
    let concurrent = sync_with(host, Reader::Concurrent(&forge.twin(&twin)), &twin, BUDGET);
    let (from, to) = (twin.display().to_string(), root.display().to_string());
    let same = |text: String| text.replace(&from, &to);
    match (&serial, &concurrent) {
        (Ok(serial), Ok(concurrent)) => {
            let report = |outcome: &Outcome| {
                let mut fields = outcome.fields.clone();
                fields.remove("timing");
                let lines: Vec<_> = outcome
                    .lines
                    .iter()
                    .filter(|line| !line.starts_with("sync took ") && !line.starts_with("phase "))
                    .collect();
                format!("{:?}\n{fields:?}\n{lines:?}", outcome.violations)
            };
            assert_eq!(same(report(concurrent)), report(serial));
        }
        (Err(serial), Err(concurrent)) => {
            assert_eq!(same(format!("{concurrent:#}")), format!("{serial:#}"));
        }
        _ => panic!("the readers disagree: serial {serial:?}, concurrent {concurrent:?}"),
    }
    let concurrent_files: BTreeMap<_, _> = files(&twin)
        .into_iter()
        .map(|(path, text)| (path, same(text)))
        .collect();
    assert_eq!(concurrent_files, files(root));
    serial
}

fn files(root: &Path) -> BTreeMap<PathBuf, String> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(root).unwrap().to_owned();
            if name == Path::new(".git") || name == Path::new(".cm/lock") {
                continue;
            }
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    name,
                    String::from_utf8_lossy(&fs::read(&path).unwrap()).into(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}
