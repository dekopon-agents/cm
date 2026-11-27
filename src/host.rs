use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use jiff::Timestamp;

pub trait Host {
    fn now(&self) -> Timestamp;
    fn free_bytes(&self, volume: &Path) -> io::Result<u64>;
    fn heavy_builds(&self) -> io::Result<u32>;
    fn github_rate_limit(&self) -> Option<String> {
        None
    }
    // The invocation as `cm …`, without `--json`, for a refusal that re-issues it; None when it
    // is not UTF-8.
    fn argv(&self) -> Option<Vec<String>> {
        None
    }
    // What every printed move starts with: `cm`, or `cm -C <root>` when a campaign was selected;
    // None when that root is not UTF-8, since no printed word can name it.
    fn prefix(&self) -> Option<Vec<String>> {
        Some(vec!["cm".to_owned()])
    }
}

pub struct System;

pub struct Invoked {
    pub argv: Option<Vec<String>>,
    pub root: Option<PathBuf>,
}

impl Invoked {
    // Drops `--json` only where it is an option: not after `--`, nor as the value of -C/--root.
    pub fn record(args: impl IntoIterator<Item: Into<OsString>>) -> Self {
        let mut argv = Some(vec!["cm".to_owned()]);
        let (mut ended, mut value) = (false, false);
        for arg in args.into_iter().skip(1).map(Into::into) {
            let option = !ended && !value;
            value = option && (arg == "-C" || arg == "--root");
            if option && arg == "--json" {
                continue;
            }
            ended |= option && arg == "--";
            argv = argv.and_then(|mut argv| {
                argv.push(arg.into_string().ok()?);
                Some(argv)
            });
        }
        Invoked { argv, root: None }
    }

    pub fn in_campaign(mut self, root: &Path) -> Self {
        self.root = Some(std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()));
        self
    }
}

impl Host for Invoked {
    fn now(&self) -> Timestamp {
        System.now()
    }

    fn free_bytes(&self, volume: &Path) -> io::Result<u64> {
        System.free_bytes(volume)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        System.heavy_builds()
    }

    fn github_rate_limit(&self) -> Option<String> {
        System.github_rate_limit()
    }

    fn argv(&self) -> Option<Vec<String>> {
        self.argv.clone()
    }

    fn prefix(&self) -> Option<Vec<String>> {
        match &self.root {
            None => Some(vec!["cm".to_owned()]),
            Some(root) => Some(vec![
                "cm".to_owned(),
                "-C".to_owned(),
                root.to_str()?.to_owned(),
            ]),
        }
    }
}

impl Host for System {
    fn github_rate_limit(&self) -> Option<String> {
        use crate::forge::{Forge as _, Gh};
        Gh::default().rate_limit()
    }

    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn free_bytes(&self, volume: &Path) -> io::Result<u64> {
        let stat = rustix::fs::statvfs(volume)?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        let output = Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,comm="])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("ps failed"));
        }
        Ok(count_builds(&String::from_utf8_lossy(&output.stdout)))
    }
}

pub fn stamp(now: Timestamp) -> String {
    now.strftime("%Y-%m-%dT%H:%MZ").to_string()
}

pub fn is_stamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    bytes.len() == 17
        && digits(0..4)
        && bytes[4] == b'-'
        && digits(5..7)
        && bytes[7] == b'-'
        && digits(8..10)
        && bytes[10] == b'T'
        && digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b'Z'
}

pub fn count_builds(ps: &str) -> u32 {
    let processes: Vec<(u32, u32, &str)> = ps
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            let rest = line.trim_start();
            let rest = rest[rest.find(char::is_whitespace)?..].trim_start();
            let comm = rest[rest.find(char::is_whitespace)?..].trim();
            let name = comm.rsplit('/').next().unwrap_or(comm);
            Some((pid, ppid, name))
        })
        .collect();
    let cargo: HashSet<u32> = processes
        .iter()
        .filter(|(_, _, name)| *name == "cargo")
        .map(|(pid, _, _)| *pid)
        .collect();
    processes
        .iter()
        .filter(|(pid, ppid, _)| cargo.contains(pid) && !cargo.contains(ppid))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_top_level_cargo_only() {
        let ps = "\
    1     0 /sbin/launchd
  100     1 /Users/x/.rustup/toolchains/1.98.1/bin/cargo
  101   100 /Users/x/.rustup/toolchains/1.98.1/bin/cargo
  102   101 rustc
  200     1 cargo
  300     1 /Applications/Some App.app/Contents/MacOS/cargo
  400     1 rustc
";
        assert_eq!(count_builds(ps), 3);
    }

    #[test]
    fn stamp_shape() {
        let now: Timestamp = "2026-10-08T07:04:59Z".parse().unwrap();
        assert_eq!(stamp(now), "2026-10-08T07:04Z");
        assert!(is_stamp("2026-10-08T07:04Z"));
        assert!(!is_stamp("2026-10-08T07:0xZ"));
        assert!(!is_stamp("2026-10-08 07:04Z"));
    }
}
