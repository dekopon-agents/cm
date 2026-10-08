use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::process::Command;

use jiff::Timestamp;

pub trait Host {
    fn now(&self) -> Timestamp;
    fn free_bytes(&self, volume: &Path) -> io::Result<u64>;
    fn heavy_builds(&self) -> io::Result<u32>;
}

pub struct System;

impl Host for System {
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
