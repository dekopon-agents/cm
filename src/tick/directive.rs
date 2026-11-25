use std::collections::{BTreeSet, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::Path;
use std::time::Duration;

use crate::tick::act::{Ctx, Report};

pub const BUDGET: Duration = Duration::from_secs(10);

const STAMP_FIELD: &[u8] = b" stamp_entry=";

const PICKED_UP: &[u8] = "— Directive picked up: ".as_bytes();

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
            let named: Vec<String> = many
                .iter()
                .flat_map(|stamp| stamp.split(|byte| *byte == b'\t'))
                .filter(|word| !word.is_empty())
                .map(|word| String::from_utf8_lossy(word).into_owned())
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
            "heartbeat: no JOURNAL.md heading for stamp_entry {}; owner directives not listed",
            String::from_utf8_lossy(stamp)
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
fn stamps(handoff: &Path) -> Result<Vec<Vec<u8>>, String> {
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
        let name = name.as_encoded_bytes();
        if !(name.starts_with(b"coordinator-") && name.ends_with(b".md")) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(could_not_read(&path, &error)),
        };
        each_line(BufReader::new(file), |line| {
            if line.starts_with(b"Handoff: ") {
                for stamp in stamp_fields(line) {
                    if !stamp.is_empty() && stamp != b"none" && !found.contains(stamp) {
                        found.insert(stamp.to_vec());
                    }
                }
            }
        })
        .map_err(|error| could_not_read(&path, &error))?;
    }
    Ok(found.into_iter().collect())
}

// Lines are bytes, as grep compared them; text is decoded only to be printed.
fn each_line(mut text: impl BufRead, mut each: impl FnMut(&[u8])) -> std::io::Result<()> {
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        if text.read_until(b'\n', &mut bytes)? == 0 {
            return Ok(());
        }
        each(bytes.strip_suffix(b"\n").unwrap_or(&bytes));
    }
}

fn after_each<'a>(text: &'a [u8], marker: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
    let mut from = 0;
    std::iter::from_fn(move || {
        let at = from
            + text[from..]
                .windows(marker.len())
                .position(|window| window == marker)?;
        from = at + marker.len();
        Some(&text[from..])
    })
}

fn stamp_fields(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    after_each(line, STAMP_FIELD).map(|rest| {
        let value = rest.split(|byte| *byte == b' ').next().unwrap_or_default();
        value.split(|byte| *byte == b'=').next().unwrap_or_default()
    })
}

// A pickup anywhere in the journal counts, as heartbeat.sh's `grep -cF` over the whole file did.
// A line holding pickups is kept once, and the text after each is a slice of it.
fn unpicked(journal: impl BufRead, stamp: &[u8]) -> std::io::Result<Option<Vec<String>>> {
    let heading = [b"## ", stamp, b" "].concat();
    let mut below = false;
    let mut directives = Vec::new();
    let mut pickups = Vec::new();
    each_line(journal, |line| {
        if !below {
            below = line.starts_with(&heading);
        } else if is_directive(line) {
            directives.push(line[b"## ".len()..].to_vec());
        }
        if after_each(line, PICKED_UP).next().is_some() {
            pickups.push(line.to_vec());
        }
    })?;
    if !below {
        return Ok(None);
    }
    let index = Prefixes::of(&directives);
    let mut picked: HashSet<&[u8]> = HashSet::new();
    for line in &pickups {
        for rest in after_each(line, PICKED_UP) {
            index.starting(rest, |directive| {
                picked.insert(directive);
            });
        }
    }
    Ok(Some(
        directives
            .iter()
            .filter(|directive| !picked.contains(directive.as_slice()))
            .map(|directive| format!("owner-directive: {}", String::from_utf8_lossy(directive)))
            .collect(),
    ))
}

// Sorted by bytes, the directives starting with some text are one run, and within it those whose
// next byte is a given one are a run again: each byte of a pickup's text narrows the run by two
// binary searches, and the walk ends at the first byte no directive has there.
struct Prefixes<'a> {
    sorted: Vec<&'a [u8]>,
}

impl<'a> Prefixes<'a> {
    fn of(directives: &'a [Vec<u8>]) -> Self {
        let mut sorted: Vec<&[u8]> = directives.iter().map(Vec::as_slice).collect();
        sorted.sort_unstable();
        sorted.dedup();
        Prefixes { sorted }
    }

    fn starting(&self, text: &[u8], mut found: impl FnMut(&'a [u8])) -> usize {
        let (mut from, mut to) = (0, self.sorted.len());
        let mut looked = 0;
        for (at, byte) in text.iter().enumerate() {
            let run = &self.sorted[from..to];
            let mut next = |directive: &&[u8]| {
                looked += 1;
                directive.get(at).copied()
            };
            let start = run.partition_point(|directive| next(directive) < Some(*byte));
            let end =
                start + run[start..].partition_point(|directive| next(directive) == Some(*byte));
            (from, to) = (from + start, from + end);
            if from == to {
                break;
            }
            if self.sorted[from].len() == at + 1 {
                found(self.sorted[from]);
            }
        }
        looked
    }
}

fn stamp_len(text: &[u8]) -> usize {
    text.iter()
        .take_while(|byte| byte.is_ascii_digit() || b"TZ:-".contains(byte))
        .count()
}

fn is_directive(line: &[u8]) -> bool {
    let Some(rest) = line.strip_prefix(b"## ") else {
        return false;
    };
    let stamp = stamp_len(rest);
    stamp > 0 && rest[stamp..].starts_with(" — Owner directive:".as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: &str = "2026-11-19T17:38Z";

    const MARKER: &str = "— Directive picked up: ";

    fn listed(text: impl AsRef<[u8]>, stamp: &str) -> Option<Vec<String>> {
        unpicked(text.as_ref(), stamp.as_bytes()).unwrap()
    }

    fn journal(open: &str) -> String {
        format!(
            "# journal\n\n## 2026-11-18T10:00Z — Owner directive: before the stamp, never listed\n\n## {STAMP} — owner-queue answer 10. URGENT: gate review\n\n## {STAMP} — epoch working\n\n## 2026-11-20T01:00Z — Owner directive: picked up already\n\n## 2026-11-20T01:05Z — Directive picked up: 2026-11-20T01:00Z — Owner directive: picked up already\n{open}"
        )
    }

    #[test]
    fn only_unpicked_directives_below_the_first_stamp_heading_are_listed() {
        assert_eq!(listed(journal(""), STAMP), Some(Vec::new()));
        let open = "\n## 2026-11-25T04:05Z — Owner directive: unstick throughput now\n";
        assert_eq!(
            listed(journal(open), STAMP),
            Some(vec![
                "owner-directive: 2026-11-25T04:05Z — Owner directive: unstick throughput now"
                    .to_owned()
            ])
        );
        assert_eq!(listed(journal(open), "2026-11-19T17:39Z"), None);
        assert_eq!(listed("", STAMP), None);
    }

    #[test]
    fn a_pickup_anywhere_in_the_journal_names_its_directive() {
        let text = format!(
            "x {MARKER}2026-11-25T04:05Z — Owner directive: unstick throughput now, as asked\n## {STAMP} — stamp\n## 2026-11-25T04:05Z — Owner directive: unstick throughput now\n## 2026-11-25T04:06Z — Owner directive: other\n"
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
            "{MARKER}2026-11-25T04:05Z — Owner directive: ab, done\n{MARKER}2026-11-25T04:06Z — Owner directive: c\n## {STAMP} — s\n## 2026-11-25T04:05Z — Owner directive: ab\n## 2026-11-25T04:06Z — Owner directive: cd\n## 2026-11-25T04:07Z — Owner directive: a\n"
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
    fn every_pickup_on_a_line_counts_not_only_its_first() {
        let text = format!(
            "## {STAMP} — s\n## 2026-11-25T04:05Z — Owner directive: x\n## 2026-11-25T04:06Z — Owner directive: y\nboth: {MARKER}2026-11-24T00:00Z — Owner directive: old; {MARKER}2026-11-25T04:06Z — Owner directive: y\n"
        );
        assert_eq!(
            listed(&text, STAMP),
            Some(vec![
                "owner-directive: 2026-11-25T04:05Z — Owner directive: x".to_owned()
            ])
        );
    }

    #[test]
    fn headings_and_pickups_are_compared_as_bytes_not_as_decoded_text() {
        let mut text =
            format!("## {STAMP} — s\n## 2026-11-25T04:05Z — Owner directive: other ").into_bytes();
        text.extend_from_slice(
            b"\xfe\n## 2026-11-25T04:05Z \xe2\x80\x94 Owner directive: other \xff\n",
        );
        text.extend_from_slice(MARKER.as_bytes());
        text.extend_from_slice(b"2026-11-25T04:05Z \xe2\x80\x94 Owner directive: other \xff\n");
        assert_eq!(
            listed(&text, STAMP),
            Some(vec![
                "owner-directive: 2026-11-25T04:05Z — Owner directive: other \u{fffd}".to_owned()
            ])
        );
    }

    #[test]
    fn directives_sharing_one_stamp_are_found_by_bytes_walked_not_by_their_number() {
        for count in [1_000, 2_000, 4_000] {
            let heading = |n: usize| format!("2026-11-25T04:05Z — Owner directive: item {n}");
            let mut text = format!("## {STAMP} — s\n");
            for n in 0..count {
                text.push_str(&format!("## {}\n", heading(n)));
            }
            for n in 1..count {
                text.push_str(&format!("{MARKER}{}\n", heading(n)));
            }
            assert_eq!(
                listed(&text, STAMP),
                Some(vec![format!("owner-directive: {}", heading(0))]),
                "{count}"
            );
            let directives: Vec<Vec<u8>> = (0..count).map(|n| heading(n).into_bytes()).collect();
            let index = Prefixes::of(&directives);
            let bits = (usize::BITS - count.leading_zeros()) as usize;
            for n in 1..count {
                let pickup = heading(n);
                let mut found = Vec::new();
                let looked = index.starting(pickup.as_bytes(), |directive| found.push(directive));
                let mut starts: Vec<&[u8]> = directives
                    .iter()
                    .map(Vec::as_slice)
                    .filter(|directive| pickup.as_bytes().starts_with(directive))
                    .collect();
                starts.sort_unstable();
                found.sort_unstable();
                assert_eq!(found, starts, "{count}: {pickup}");
                assert!(
                    looked <= 2 * pickup.len() * (bits + 1),
                    "{count}: {pickup} looked at {looked} directives"
                );
            }
        }
    }

    #[test]
    fn a_directive_heading_is_a_stamp_of_digits_t_z_colons_and_hyphens() {
        for yes in [
            "## 2026-11-25T04:05Z — Owner directive: x",
            "## 1 — Owner directive:",
        ] {
            assert!(is_directive(yes.as_bytes()), "{yes}");
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
            assert!(!is_directive(no.as_bytes()), "{no}");
        }
    }

    #[test]
    fn a_stamp_field_ends_at_a_space_and_is_cut_at_its_first_equals() {
        let fields = |line: &str| {
            stamp_fields(line.as_bytes())
                .map(|field| String::from_utf8(field.to_vec()).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            fields("Handoff: epoch=e role=coordinator stamp_sha=abc stamp_entry=2026-11-19T17:38Z"),
            ["2026-11-19T17:38Z"]
        );
        assert_eq!(
            fields("Handoff: stamp_entry=a=b stamp_entry= x stamp_entry=c\tz"),
            ["a", "", "c\tz"]
        );
        assert_eq!(fields("Handoff: xstamp_entry=a"), Vec::<String>::new());
    }
}
