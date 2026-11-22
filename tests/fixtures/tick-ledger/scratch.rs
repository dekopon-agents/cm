//! Scratch git for the `tick/ledger` tests (unit and integration): a bare origin and clones
//! shaped like campaign-trail. Standard library and `tempfile` only, so both crates include it.
//!
//! The seed commit holds what the coordinator commits in campaign-trail: a top-level
//! `.gitignore` of `*.jsonl`, the ledger's `merge=union` line in `.gitattributes`, and one epoch
//! at [`EPOCH`] with a campaign state file, `STATE.md` and a tracked `AGENTS.jsonl`.

#![allow(dead_code, reason = "each test crate uses a different part")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const EPOCH: &str = "store/epochs/scratch";
pub const UNION: &str = "store/epochs/*/.cm/tick/effects-*.jsonl merge=union\n";

/// `git -C repo args`, which must exit zero; its trimmed stdout.
pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} in {repo:?}: {output:?}"
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn git_ok(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap()
        .status
        .success()
}

/// A clone's own identity and no signing, whatever the global config says.
pub fn configure(repo: &Path) {
    git(repo, &["config", "user.name", "cm"]);
    git(repo, &["config", "user.email", "cm@example.com"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

pub struct Scratch {
    pub dir: tempfile::TempDir,
    pub origin: PathBuf,
}

impl Scratch {
    pub fn new() -> Scratch {
        Scratch::with_attributes(UNION)
    }

    pub fn with_attributes(attributes: &str) -> Scratch {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let seed = dir.path().join("seed");
        fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q", "-b", "main"]);
        configure(&seed);
        fs::write(seed.join(".gitignore"), "*.jsonl\n").unwrap();
        fs::write(seed.join(".gitattributes"), attributes).unwrap();
        let epoch = seed.join(EPOCH);
        fs::create_dir_all(epoch.join(".cm")).unwrap();
        fs::write(epoch.join(".cm/state.json"), "{\"kind\":\"campaign\"}\n").unwrap();
        fs::write(epoch.join(".cm/.gitignore"), "lock\nJOURNAL.md\n").unwrap();
        fs::write(epoch.join("STATE.md"), "# State\n\nseed\n").unwrap();
        fs::write(epoch.join("AGENTS.jsonl"), "{\"seed\":1}\n").unwrap();
        git(&seed, &["add", "--all"]);
        git(&seed, &["add", "--force", &format!("{EPOCH}/AGENTS.jsonl")]);
        git(&seed, &["commit", "-q", "-m", "seed"]);
        git(
            &seed,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&seed, &["push", "-q", "origin", "main"]);
        Scratch { dir, origin }
    }

    /// A fresh clone at `<dir>/<name>`; its checkout path.
    pub fn clone(&self, name: &str) -> PathBuf {
        let checkout = self.dir.path().join(name);
        let status = Command::new("git")
            .args(["clone", "-q"])
            .arg(&self.origin)
            .arg(&checkout)
            .status()
            .unwrap();
        assert!(status.success());
        configure(&checkout);
        checkout
    }

    /// `git show main:<path>` in the origin.
    pub fn on_main(&self, path: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&self.origin)
            .args(["show", &format!("main:{path}")])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap())
    }

    pub fn main_sha(&self) -> String {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&self.origin)
            .args(["rev-parse", "main"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A `git` that logs every call to [`Scratch::log`] and, while [`Scratch::rival`] names a
    /// checkout, has that checkout push one commit to main just before each push it runs: a real
    /// non-fast-forward reject on every try.
    pub fn logging_git(&self) -> PathBuf {
        let script = self.dir.path().join("git-wrapper");
        let log = self.dir.path().join("git.log");
        let rival = self.dir.path().join("rival-checkout");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo \"$*\" >> '{log}'\n\
                 case \" $* \" in *\" push \"*)\n\
                 if [ -f '{rival}' ]; then\n\
                 r=$(cat '{rival}')\n\
                 git -C \"$r\" commit -q --allow-empty -m rival && git -C \"$r\" push -q origin HEAD:main\n\
                 fi;;\n\
                 esac\n\
                 exec git \"$@\"\n",
                log = log.display(),
                rival = rival.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // A thread forking while the script was open for writing can make its first exec fail
        // with ETXTBSY; wait until it runs.
        for _ in 0..100 {
            if Command::new(&script).arg("--version").output().is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = fs::remove_file(&log);
        script
    }

    /// The git subcommands [`Scratch::logging_git`] ran, in order (`-C <dir>` dropped).
    pub fn log(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("git.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let words: Vec<&str> = line.split(' ').collect();
                match words.as_slice() {
                    ["-C", _, rest @ ..] => rest.join(" "),
                    rest => rest.join(" "),
                }
            })
            .collect()
    }

    pub fn set_rival(&self, checkout: Option<&Path>) {
        let rival = self.dir.path().join("rival-checkout");
        match checkout {
            Some(checkout) => fs::write(rival, checkout.to_str().unwrap()).unwrap(),
            None => {
                let _ = fs::remove_file(rival);
            }
        }
    }
}

pub fn campaign(checkout: &Path) -> PathBuf {
    checkout.join(EPOCH)
}

/// Appends `text` to a file under the checkout.
pub fn append(path: &Path, text: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// A commit of every change in the checkout, ignored files left out.
pub fn commit_all(checkout: &Path, message: &str) -> String {
    git(checkout, &["add", "--all"]);
    git(checkout, &["commit", "-q", "-m", message]);
    git(checkout, &["rev-parse", "HEAD"])
}
