use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind};
use std::ops::Bound;
use std::path::Path;
use std::time::Duration;

use crate::tick::act::{Ctx, Report};

pub const BUDGET: Duration = Duration::from_secs(10);

const STAMP_FIELD: &str = " stamp_entry=";

const PICKED_UP: &str = "— Directive picked up: ";

pub fn run(ctx: &Ctx) -> Report {
    let epoch = ctx.epoch_dir();
    let stamps = match stamps(&epoch.join("handoff")) {
        Ok(stamps) => stamps,
        Err(line) => return unread(line),
    };
    let stamp = match stamps.as_slice() {
        [] => return Report::default(),
        [stamp] => stamp,
        many => {
            let named: Vec<&str> = many
                .iter()
                .flat_map(|stamp| stamp.split('\t'))
                .filter(|word| !word.is_empty())
                .collect();
            return Report {
                lines: vec![format!(
                    "heartbeat: coordinator handoffs name more than one stamp_entry ({}); owner directives not listed",
                    named.join(" ")
                )],
                failed: false,
            };
        }
    };
    let path = epoch.join("JOURNAL.md");
    let listed = match File::open(&path) {
        Ok(file) => unpicked(BufReader::new(file), stamp),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    let lines = match listed {
        Ok(Some(lines)) => lines,
        Ok(None) => vec![format!(
            "heartbeat: no JOURNAL.md heading for stamp_entry {stamp}; owner directives not listed"
        )],
        Err(error) => return unread(could_not_read(&path, &error)),
    };
    Report {
        lines,
        failed: false,
    }
}

fn unread(line: String) -> Report {
    Report {
        lines: vec![line],
        failed: true,
    }
}

fn could_not_read(path: &Path, error: &std::io::Error) -> String {
    format!(
        "heartbeat: could not read {}: {error}; owner directives not listed",
        path.display()
    )
}

// A handoff that is gone or not a file is skipped, as heartbeat.sh's grep skipped it.
fn stamps(handoff: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(handoff) {
        Ok(entries) => entries,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(could_not_read(handoff, &error)),
    };
    let mut found = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| could_not_read(handoff, &error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("coordinator-") && name.ends_with(".md")) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(could_not_read(&path, &error)),
        };
        let text = String::from_utf8_lossy(&bytes);
        for line in text
            .split('\n')
            .filter(|line| line.starts_with("Handoff: "))
        {
            found.extend(
                stamp_fields(line)
                    .filter(|stamp| !stamp.is_empty() && *stamp != "none")
                    .map(str::to_owned),
            );
        }
    }
    Ok(found.into_iter().collect())
}

fn stamp_fields(line: &str) -> impl Iterator<Item = &str> {
    line.split(STAMP_FIELD).skip(1).map(|rest| {
        let value = rest.split(' ').next().unwrap_or_default();
        value.split('=').next().unwrap_or_default()
    })
}

// A pickup anywhere in the journal counts, as heartbeat.sh's `grep -cF` over the whole file did,
// and one pass keeps only the headings below the stamp and the text after each pickup.
fn unpicked(mut journal: impl BufRead, stamp: &str) -> std::io::Result<Option<Vec<String>>> {
    let heading = format!("## {stamp} ");
    let mut below = false;
    let mut directives = Vec::new();
    let mut picked = BTreeSet::new();
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        if journal.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(bytes.strip_suffix(b"\n").unwrap_or(&bytes));
        for (at, _) in line.match_indices(PICKED_UP) {
            picked.insert(line[at + PICKED_UP.len()..].to_owned());
        }
        if !below {
            below = line.starts_with(&heading);
        } else if is_directive(&line) {
            directives.push(line["## ".len()..].to_owned());
        }
    }
    Ok(below.then(|| {
        directives
            .into_iter()
            .filter(|heading| !picked_up(&picked, heading))
            .map(|heading| format!("owner-directive: {heading}"))
            .collect()
    }))
}

// Every text starting with the heading sorts at or after it, before any that does not.
fn picked_up(picked: &BTreeSet<String>, heading: &str) -> bool {
    picked
        .range::<str, _>((Bound::Included(heading), Bound::Unbounded))
        .next()
        .is_some_and(|rest| rest.starts_with(heading))
}

fn is_directive(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("## ") else {
        return false;
    };
    let stamp = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_digit() || b"TZ:-".contains(byte))
        .count();
    stamp > 0 && rest[stamp..].starts_with(" — Owner directive:")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: &str = "2026-11-19T17:38Z";

    fn listed(text: &str, stamp: &str) -> Option<Vec<String>> {
        unpicked(text.as_bytes(), stamp).unwrap()
    }

    fn journal(open: &str) -> String {
        format!(
            "# journal\n\n## 2026-11-18T10:00Z — Owner directive: before the stamp, never listed\n\n## {STAMP} — owner-queue answer 10. URGENT: gate review\n\n## {STAMP} — epoch working\n\n## 2026-11-20T01:00Z — Owner directive: picked up already\n\n## 2026-11-20T01:05Z — Directive picked up: 2026-11-20T01:00Z — Owner directive: picked up already\n{open}"
        )
    }

    #[test]
    fn only_unpicked_directives_below_the_first_stamp_heading_are_listed() {
        assert_eq!(listed(&journal(""), STAMP), Some(Vec::new()));
        let open = "\n## 2026-11-25T04:05Z — Owner directive: unstick throughput now\n";
        assert_eq!(
            listed(&journal(open), STAMP),
            Some(vec![
                "owner-directive: 2026-11-25T04:05Z — Owner directive: unstick throughput now"
                    .to_owned()
            ])
        );
        assert_eq!(listed(&journal(open), "2026-11-19T17:39Z"), None);
        assert_eq!(listed("", STAMP), None);
    }

    #[test]
    fn a_pickup_anywhere_in_the_journal_names_its_directive() {
        let text = format!(
            "x {PICKED_UP}2026-11-25T04:05Z — Owner directive: unstick throughput now, as asked\n## {STAMP} — stamp\n## 2026-11-25T04:05Z — Owner directive: unstick throughput now\n## 2026-11-25T04:06Z — Owner directive: other\n"
        );
        assert_eq!(
            listed(&text, STAMP),
            Some(vec![
                "owner-directive: 2026-11-25T04:06Z — Owner directive: other".to_owned()
            ])
        );
    }

    #[test]
    fn the_first_stamp_heading_bounds_the_listing_and_its_stamp_is_followed_by_a_space() {
        let text = format!(
            "## {STAMP} — first\n## 2026-11-19T18:00Z — Owner directive: between\n## {STAMP} — second\n## 2026-11-19T19:00Z — Owner directive: after\n"
        );
        assert_eq!(
            listed(&text, STAMP),
            Some(vec![
                "owner-directive: 2026-11-19T18:00Z — Owner directive: between".to_owned(),
                "owner-directive: 2026-11-19T19:00Z — Owner directive: after".to_owned(),
            ])
        );
        let longer = format!(
            "## {STAMP}5 — another stamp\n## 2026-11-25T04:05Z — Owner directive: x\n## {STAMP}\n"
        );
        assert_eq!(listed(&longer, STAMP), None);
    }

    #[test]
    fn a_pickup_counts_when_its_text_starts_with_the_whole_heading() {
        let text = format!(
            "{PICKED_UP}2026-11-25T04:05Z — Owner directive: ab, done\n{PICKED_UP}2026-11-25T04:06Z — Owner directive: c\n## {STAMP} — s\n## 2026-11-25T04:05Z — Owner directive: ab\n## 2026-11-25T04:06Z — Owner directive: cd\n## 2026-11-25T04:07Z — Owner directive: a\n"
        );
        assert_eq!(
            listed(&text, STAMP),
            Some(vec![
                "owner-directive: 2026-11-25T04:06Z — Owner directive: cd".to_owned(),
                "owner-directive: 2026-11-25T04:07Z — Owner directive: a".to_owned(),
            ])
        );
    }

    #[test]
    fn a_directive_heading_is_a_stamp_of_digits_t_z_colons_and_hyphens() {
        for yes in [
            "## 2026-11-25T04:05Z — Owner directive: x",
            "## 1 — Owner directive:",
        ] {
            assert!(is_directive(yes), "{yes}");
        }
        for no in [
            "## — Owner directive: x",
            "## 2026-11-25 04:05Z — Owner directive: x",
            "## 2026-11-25T04:05Z — Owner directive x",
            "## 2026-11-25t04:05Z — Owner directive: x",
            " ## 2026-11-25T04:05Z — Owner directive: x",
            "### 2026-11-25T04:05Z — Owner directive: x",
            "## 2026-11-25T04:05Z - Owner directive: x",
        ] {
            assert!(!is_directive(no), "{no}");
        }
    }

    #[test]
    fn a_stamp_field_ends_at_a_space_and_is_cut_at_its_first_equals() {
        let fields = |line| stamp_fields(line).collect::<Vec<_>>();
        assert_eq!(
            fields("Handoff: epoch=e role=coordinator stamp_sha=abc stamp_entry=2026-11-19T17:38Z"),
            ["2026-11-19T17:38Z"]
        );
        assert_eq!(
            fields("Handoff: stamp_entry=a=b stamp_entry= x stamp_entry=c\tz"),
            ["a", "", "c\tz"]
        );
        assert_eq!(fields("Handoff: xstamp_entry=a"), Vec::<&str>::new());
    }
}
