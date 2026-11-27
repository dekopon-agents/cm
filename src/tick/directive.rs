use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::Path;
use std::time::Duration;

use crate::tick::act::{Ctx, Report};

pub const BUDGET: Duration = Duration::from_secs(10);

const STAMP_FIELD: &[u8] = b" stamp_entry=";

const PICKED_UP: &[u8] = "— Directive picked up: ".as_bytes();

const MODULUS: u64 = (1 << 61) - 1;

const BLOCK: usize = 1 << 12;

const PROBES_PER_BYTE: usize = 64;

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

// A handoff that is gone or not a file is skipped. heartbeat.sh's grep skipped a folder too, but
// blocked on a FIFO until something wrote to it.
fn stamps(handoff: &Path) -> Result<Vec<Vec<u8>>, String> {
    let entries = match std::fs::read_dir(handoff) {
        Ok(entries) => entries,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(could_not_read(handoff, &error)),
    };
    let mut found = HashSet::new();
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
        stamps_in(BufReader::new(file), &mut found)
            .map_err(|error| could_not_read(&path, &error))?;
    }
    Ok(sorted(found))
}

fn stamps_in(handoff: impl BufRead, found: &mut HashSet<Vec<u8>>) -> std::io::Result<()> {
    each_line(handoff, |line| {
        if line.starts_with(b"Handoff: ") {
            for stamp in stamp_fields(line) {
                examined(stamp.len());
                if !stamp.is_empty() && stamp != b"none" && !found.contains(stamp) {
                    found.insert(stamp.to_vec());
                }
            }
        }
    })
}

// As `sort -u` ordered them, by bytes.
fn sorted(found: HashSet<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut found: Vec<Vec<u8>> = found.into_iter().collect();
    found.sort_unstable_by(|one, other| {
        examined(one.len().min(other.len()) + 1);
        one.cmp(other)
    });
    found
}

// Lines are bytes, as grep compared them; text is decoded only to be printed.
fn each_line(mut text: impl BufRead, mut each: impl FnMut(&[u8])) -> std::io::Result<()> {
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        if text.read_until(b'\n', &mut bytes)? == 0 {
            return Ok(());
        }
        examined(bytes.len());
        each(bytes.strip_suffix(b"\n").unwrap_or(&bytes));
    }
}

fn after_each<'a>(text: &'a [u8], marker: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
    let mut from = 0;
    std::iter::from_fn(move || {
        let at = from
            + text[from..].windows(marker.len()).position(|window| {
                examined(1);
                window == marker
            })?;
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
fn unpicked(journal: impl BufRead, stamp: &[u8]) -> std::io::Result<Option<Vec<String>>> {
    unpicked_by(journal, stamp, random_base(), PROBES_PER_BYTE)
}

fn unpicked_by(
    journal: impl BufRead,
    stamp: &[u8],
    base: u64,
    probes_per_byte: usize,
) -> std::io::Result<Option<Vec<String>>> {
    let heading = [b"## ", stamp, b" "].concat();
    let mut below = false;
    let mut directives = Vec::new();
    let mut pickups = Vec::new();
    let (mut read, mut number) = (0usize, 0usize);
    each_line(journal, |line| {
        read = read.saturating_add(line.len() + 1);
        number += 1;
        if !below {
            examined(heading.len().min(line.len()));
            below = line.starts_with(&heading);
        } else if is_directive(line) {
            directives.push(line[b"## ".len()..].to_vec());
        }
        if after_each(line, PICKED_UP).next().is_some() {
            pickups.push((number, line.to_vec()));
        }
    })?;
    if !below {
        return Ok(None);
    }
    let mut index = Pickups::of(&directives, base, read.saturating_mul(probes_per_byte));
    let mut lines = Vec::new();
    if let Some(at) = pickups.iter().position(|(_, line)| !index.scan(line)) {
        lines.push(format!(
            "heartbeat: pickups in JOURNAL.md from line {} on ({} line(s)) were not checked: the scan stops at {probes_per_byte} probes per byte; some owner directives below may be picked up already",
            pickups[at].0,
            pickups.len() - at
        ));
    }
    lines.extend(
        directives
            .iter()
            .filter(|directive| !index.picked(directive))
            .map(|directive| format!("owner-directive: {}", String::from_utf8_lossy(directive))),
    );
    Ok(Some(lines))
}

// heartbeat.sh ran one `grep -F` per directive. Here every heading is hashed once and filed by
// its length; the text after each pickup is hashed, for each length a heading has, in O(1) from
// rolling hashes of its line's prefixes, and a heading's bytes are compared only when its hash
// matches and only until it is picked up. No pickup is walked for as long as it agrees with a
// heading. Pickups times distinct heading lengths can still grow faster than the journal, so
// the probes stop at PROBES_PER_BYTE per byte read and the headings not found by then are
// listed: a directive is listed again rather than missed, as heartbeat.sh listed a heading its
// grep could not take.
struct Pickups<'a> {
    base: u64,
    ids: HashMap<&'a [u8], usize>,
    headings: Vec<&'a [u8]>,
    picked: Vec<bool>,
    lengths: Vec<Length>,
    probes: usize,
}

struct Length {
    len: usize,
    power: u64,
    hashes: Vec<(u64, usize)>,
}

impl<'a> Pickups<'a> {
    fn of(directives: &'a [Vec<u8>], base: u64, probes: usize) -> Self {
        let mut ids = HashMap::new();
        let mut headings = Vec::new();
        for directive in directives {
            examined(directive.len());
            ids.entry(directive.as_slice()).or_insert_with(|| {
                headings.push(directive.as_slice());
                headings.len() - 1
            });
        }
        let mut by_len: BTreeMap<usize, Vec<(u64, usize)>> = BTreeMap::new();
        for (id, heading) in headings.iter().enumerate() {
            examined(heading.len());
            let hash = heading.iter().fold(0, |hash, &byte| roll(hash, base, byte));
            by_len.entry(heading.len()).or_default().push((hash, id));
        }
        let lengths = by_len
            .into_iter()
            .map(|(len, mut hashes)| {
                hashes.sort_unstable();
                Length {
                    len,
                    power: power(base, len),
                    hashes,
                }
            })
            .collect();
        Pickups {
            base,
            ids,
            picked: vec![false; headings.len()],
            headings,
            lengths,
            probes,
        }
    }

    fn picked(&self, directive: &[u8]) -> bool {
        examined(directive.len());
        self.ids.get(directive).is_some_and(|&id| self.picked[id])
    }

    // A line's prefix hashes are made BLOCK positions at a time, and each pickup whose text would
    // end in the block is checked against each length then, so a line of any size holds 32 KiB
    // of hashes, not eight bytes for each of its own.
    fn scan(&mut self, line: &[u8]) -> bool {
        let Pickups {
            base,
            headings,
            picked,
            lengths,
            probes,
            ..
        } = self;
        let starts: Vec<usize> = after_each(line, PICKED_UP)
            .map(|rest| line.len() - rest.len())
            .collect();
        let fits = &lengths[..lengths.partition_point(|length| length.len <= line.len())];
        let mut next = vec![0; fits.len()];
        let mut at_start = Vec::with_capacity(starts.len());
        let mut prefix = Vec::with_capacity(BLOCK);
        let mut hash = 0;
        let mut first = 0;
        while first <= line.len() {
            let end = (first + BLOCK).min(line.len() + 1);
            prefix.clear();
            for &byte in &line[first..end.min(line.len())] {
                prefix.push(hash);
                hash = roll(hash, *base, byte);
            }
            if end > line.len() {
                prefix.push(hash);
            }
            examined(end - first);
            while let Some(&start) = starts.get(at_start.len()).filter(|&&start| start < end) {
                at_start.push(prefix[start - first]);
            }
            for (length, next) in fits.iter().zip(&mut next) {
                examined(1);
                let Some(left) = probes.checked_sub(1) else {
                    return false;
                };
                *probes = left;
                while let Some(&start) =
                    starts.get(*next).filter(|&&start| start + length.len < end)
                {
                    let Some(left) = probes.checked_sub(1) else {
                        return false;
                    };
                    *probes = left;
                    let text = minus(
                        prefix[start + length.len - first],
                        times(at_start[*next], length.power),
                    );
                    pick(
                        length,
                        text,
                        &line[start..start + length.len],
                        headings,
                        picked,
                    );
                    *next += 1;
                }
            }
            first = end;
        }
        true
    }
}

fn pick(length: &Length, hash: u64, text: &[u8], headings: &[&[u8]], picked: &mut [bool]) {
    let from = length.hashes.partition_point(|&(other, _)| {
        examined(1);
        other < hash
    });
    for &(_, id) in length.hashes[from..].iter().take_while(|&&(other, _)| {
        examined(1);
        other == hash
    }) {
        if !picked[id] {
            examined(text.len());
            picked[id] = headings[id] == text;
        }
    }
}

// Drawn afresh by each tick, so no journal can be written to make two texts' hashes agree: two
// different texts of length n agree for at most n of the 2^61 - 1 bases.
fn random_base() -> u64 {
    2 + RandomState::new().hash_one(()) % (MODULUS - 2)
}

fn reduce(sum: u64) -> u64 {
    if sum >= MODULUS { sum - MODULUS } else { sum }
}

fn times(one: u64, other: u64) -> u64 {
    let product = u128::from(one) * u128::from(other);
    reduce((product as u64 & MODULUS) + (product >> 61) as u64)
}

fn minus(one: u64, other: u64) -> u64 {
    reduce(one + MODULUS - other)
}

fn roll(hash: u64, base: u64, byte: u8) -> u64 {
    reduce(times(hash, base) + u64::from(byte))
}

fn power(base: u64, mut exponent: usize) -> u64 {
    let (mut power, mut square) = (1, base);
    while exponent > 0 {
        if exponent & 1 == 1 {
            power = times(power, square);
        }
        square = times(square, square);
        exponent >>= 1;
    }
    power
}

fn stamp_len(text: &[u8]) -> usize {
    let len = text
        .iter()
        .take_while(|byte| byte.is_ascii_digit() || b"TZ:-".contains(byte))
        .count();
    examined(len + 1);
    len
}

fn is_directive(line: &[u8]) -> bool {
    let Some(rest) = line.strip_prefix(b"## ") else {
        return false;
    };
    let stamp = stamp_len(rest);
    stamp > 0 && rest[stamp..].starts_with(" — Owner directive:".as_bytes())
}

// What the readers looked at: bytes read, windows and stamps compared, hashes made and probed.
// Tests bound it by the input's size, so work that grows faster than the input fails without a
// clock.
pub(crate) fn examined(count: usize) {
    #[cfg(test)]
    EXAMINED.with(|examined| examined.set(examined.get() + count));
    #[cfg(not(test))]
    let _ = count;
}

#[cfg(test)]
thread_local! {
    static EXAMINED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn examining(body: impl FnOnce()) -> usize {
    EXAMINED.with(|examined| examined.set(0));
    body();
    EXAMINED.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn grows_with_its_input(shape: &str, sizes: &[usize], work: &[usize], per_byte: usize) {
    for (size, work) in sizes.iter().zip(work) {
        assert!(
            *work <= per_byte * size,
            "{shape}: {work} examined for {size} bytes"
        );
    }
    for pair in work.windows(2) {
        assert!(
            4 * pair[1] <= 9 * pair[0],
            "{shape}: twice the input took {} then {}",
            pair[0],
            pair[1]
        );
    }
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

    const BASE: u64 = 0x0123_4567_89ab_cdef % MODULUS;

    const SIZES: [usize; 3] = [1 << 14, 1 << 15, 1 << 16];

    // The shapes whose probes outgrow their bytes reach PROBES_PER_BYTE only past 10 MB, so the
    // table holds every shape to one probe per byte, which the others stay under.
    const SHAPE_PROBES: usize = 1;

    fn repeated(unit: &str, size: usize) -> String {
        unit.repeat(size / unit.len() + 1)
    }

    fn below_stamp(body: impl AsRef<[u8]>) -> Vec<u8> {
        [format!("## {STAMP} — s\n").as_bytes(), body.as_ref()].concat()
    }

    fn numbered(count: usize, line: impl Fn(usize) -> String) -> String {
        (0..count).map(line).collect()
    }

    const UNIT: &str = "— Directive picked up: 1 — Owner directive: x ";

    const OVERLAP: &str = "— Directive picked up: 1 — Owner directive: ";

    type Shape = (&'static str, fn(usize) -> Vec<u8>);

    fn of_many_lengths(size: usize, under: &str) -> Vec<u8> {
        let lengths = (size as f64).sqrt() as usize;
        below_stamp(
            numbered(lengths, |n| {
                format!("## 1 — Owner directive: {}\n", "y".repeat(n))
            }) + &repeated(under, size / 2)
                + "\n",
        )
    }

    const JOURNALS: [Shape; 14] = [
        (
            "headings of as many lengths as the journal's square root, over a line of bare markers",
            |size| of_many_lengths(size, MARKER),
        ),
        (
            "headings of as many lengths, under pickups that each begin like one",
            |size| of_many_lengths(size, "— Directive picked up: 1 — Owner directive: z "),
        ),
        (
            "a heading that is its whole line, picked up again and again inside itself",
            |size| {
                below_stamp(format!(
                    "## 1 — Owner directive: x {}\n",
                    repeated(UNIT, size)
                ))
            },
        ),
        (
            "two such headings, the shorter of which fits after each pickup",
            |size| {
                below_stamp(format!(
                    "## 1 — Owner directive: x {}\n## 1 — Owner directive: x {}y\n",
                    repeated(UNIT, size / 2),
                    repeated(UNIT, size / 4)
                ))
            },
        ),
        (
            "many short directives sharing one stamp, each but the first picked up",
            |size| {
                let count = size / 100;
                let heading = |n| format!("2026-11-25T04:05Z — Owner directive: item {n}");
                below_stamp(
                    numbered(count, |n| format!("## {}\n", heading(n)))
                        + &numbered(count, |n| format!("{MARKER}{}\n", heading(n + 1))),
                )
            },
        ),
        (
            "a heading that fits after each of the pickups that overlap it",
            |size| {
                below_stamp(format!(
                    "## 1 — Owner directive: x {}\n## 1 — Owner directive: x {}\n",
                    repeated(UNIT, size / 2),
                    repeated(UNIT, size / 4)
                ))
            },
        ),
        (
            "eight headings, each a prefix of the next, under pickups of the fourth",
            |size| {
                let heading =
                    |n: usize| format!("1 — Owner directive: {}", "x".repeat(n * size / 32));
                below_stamp(
                    numbered(8, |n| format!("## {}\n", heading(n + 1)))
                        + &numbered(4, |_| format!("{MARKER}{}\n", heading(4))),
                )
            },
        ),
        ("headings that hold a pickup of a shorter heading", |size| {
            below_stamp(numbered(size / 120, |n| {
                format!(
                    "## 1 — Owner directive: a{n}\n## 1 — Owner directive: b{n} {MARKER}1 — Owner directive: a{}\n",
                    n / 2 * 2
                )
            }))
        }),
        (
            "pickups that overlap a heading they agree with to its last byte",
            |size| {
                below_stamp(format!(
                    "## 1 — Owner directive: {}z\n{}\n",
                    repeated(OVERLAP, size / 2),
                    repeated(OVERLAP, size / 2)
                ))
            },
        ),
        ("a heading the size of the journal and no pickup", |size| {
            below_stamp(format!("## 1 — Owner directive: {}\n", "x".repeat(size)))
        }),
        (
            "CRLF line ends and NUL bytes in headings and pickups",
            |size| {
                below_stamp(numbered(size / 70, |n| {
                    format!(
                        "## 1 — Owner directive: a\0{n}\r\n{MARKER}1 — Owner directive: a\0{}\r\n",
                        n / 2 * 2
                    )
                }))
            },
        ),
        ("pickups under headings of thirty-two lengths", |size| {
            let heading = |n: usize| format!("1 — Owner directive: {}", "y".repeat(n));
            below_stamp(
                numbered(32, |n| format!("## {}\n", heading(n)))
                    + &repeated(&format!("{MARKER}{} ", heading(16)), size)
                    + "\n",
            )
        }),
        ("a line of nothing but pickup markers", |size| {
            below_stamp(format!(
                "## 1 — Owner directive: q\n{}\n",
                repeated(MARKER, size)
            ))
        }),
        ("a line of near misses of the marker", |size| {
            below_stamp(format!(
                "## 1 — Owner directive: q\n{}\n",
                repeated("— Directive picked up:", size)
            ))
        }),
    ];

    #[test]
    fn every_journal_shape_costs_work_in_proportion_to_its_bytes() {
        for (shape, journal) in JOURNALS {
            let texts: Vec<Vec<u8>> = SIZES.iter().map(|&size| journal(size)).collect();
            let work: Vec<usize> = texts
                .iter()
                .map(|text| {
                    examining(|| {
                        unpicked_by(text.as_slice(), STAMP.as_bytes(), BASE, SHAPE_PROBES).unwrap();
                    })
                })
                .collect();
            let sizes: Vec<usize> = texts.iter().map(Vec::len).collect();
            grows_with_its_input(shape, &sizes, &work, 16);
            let listed = unpicked_by(texts[0].as_slice(), STAMP.as_bytes(), BASE, SHAPE_PROBES)
                .unwrap()
                .map(|lines| {
                    lines
                        .into_iter()
                        .filter(|line| !line.starts_with("heartbeat: pickups in JOURNAL.md"))
                        .collect::<Vec<_>>()
                });
            assert_eq!(
                listed,
                grep_f_per_directive(&texts[0], STAMP.as_bytes()),
                "{shape}"
            );
        }
    }

    type Handoff = (&'static str, fn(usize) -> String);

    const HANDOFFS: [Handoff; 7] = [
        ("Handoff lines naming one stamp", |size| {
            repeated(
                "Handoff: epoch=e role=coordinator stamp_entry=2026-11-19T17:38Z\n",
                size,
            )
        }),
        ("Handoff lines naming a different stamp each", |size| {
            numbered(size / 40, |n| {
                format!("Handoff: stamp_entry=2026-11-19T17:38Z-{n}\n")
            })
        }),
        ("one Handoff line of many stamp fields", |size| {
            format!("Handoff:{}\n", repeated(" stamp_entry=a", size))
        }),
        ("a stamp field as long as the handoff", |size| {
            format!("Handoff: stamp_entry={}\n", "x".repeat(size))
        }),
        ("stamp fields outside Handoff lines", |size| {
            repeated(" stamp_entry=2026-11-19T17:38Z\n", size)
        }),
        ("a Handoff line of near misses of the stamp field", |size| {
            format!("Handoff: {}\n", repeated(" stamp_entr", size))
        }),
        ("CRLF line ends and NUL bytes", |size| {
            repeated("Handoff: stamp_entry=a\0b\r\n", size)
        }),
    ];

    #[test]
    fn every_handoff_shape_is_read_in_proportion_to_its_bytes_and_sorted_as_sort_u_sorts() {
        for (shape, handoff) in HANDOFFS {
            let texts: Vec<String> = SIZES.iter().map(|&size| handoff(size)).collect();
            let mut found = Vec::new();
            let work: Vec<usize> = texts
                .iter()
                .map(|text| {
                    let mut stamps = HashSet::new();
                    let work = examining(|| stamps_in(text.as_bytes(), &mut stamps).unwrap());
                    found.push(stamps);
                    work
                })
                .collect();
            let sizes: Vec<usize> = texts.iter().map(String::len).collect();
            grows_with_its_input(shape, &sizes, &work, 4);
            for stamps in found {
                let count = stamps.len();
                let bytes: usize = stamps.iter().map(|stamp| stamp.len() + 1).sum();
                let log = (usize::BITS - count.leading_zeros()) as usize;
                let work = examining(|| drop(sorted(stamps)));
                assert!(
                    work <= 4 * bytes * (log + 1),
                    "{shape}: {work} examined to sort {count} stamps of {bytes} bytes"
                );
            }
        }
    }

    #[test]
    fn stamps_are_listed_once_each_in_byte_order() {
        let mut found = HashSet::new();
        stamps_in(
            "Handoff: stamp_entry=b\nHandoff: stamp_entry=a stamp_entry=b\nHandoff: stamp_entry=none stamp_entry= x\nHandoff: stamp_entry=é\nHandoff: stamp_entry=B\n"
                .as_bytes(),
            &mut found,
        )
        .unwrap();
        assert_eq!(sorted(found), [&b"B"[..], b"a", b"b", "\u{e9}".as_bytes()]);
    }

    // heartbeat.sh's `grep -cF` never ran for a heading past 128 KiB, an exec argument the kernel
    // refuses (MAX_ARG_STRLEN), so the script listed it even when a pickup named it.
    #[test]
    fn a_heading_over_128_kib_is_compared_like_any_other() {
        let heading = format!("1 — Owner directive: {}", "x".repeat(200 << 10));
        let picked = below_stamp(format!("## {heading}\n{MARKER}{heading}\n"));
        assert_eq!(listed(&picked, STAMP), Some(Vec::new()));
        let short = &heading[..heading.len() - 1];
        let unpicked = below_stamp(format!("## {heading}\n{MARKER}{short}\n"));
        assert_eq!(
            listed(&unpicked, STAMP),
            Some(vec![format!("owner-directive: {heading}")])
        );
    }

    #[test]
    fn past_its_probe_budget_the_scan_names_the_pickups_it_left_and_lists_what_it_has_not_found() {
        let text = below_stamp(format!(
            "## 1 — Owner directive: a\n## 1 — Owner directive: b\n{MARKER}1 — Owner directive: a\n"
        ));
        let listed = |probes_per_byte| {
            unpicked_by(text.as_slice(), STAMP.as_bytes(), BASE, probes_per_byte).unwrap()
        };
        assert_eq!(
            listed(PROBES_PER_BYTE),
            Some(vec!["owner-directive: 1 — Owner directive: b".to_owned()])
        );
        assert_eq!(
            listed(0),
            Some(vec![
                "heartbeat: pickups in JOURNAL.md from line 4 on (1 line(s)) were not checked: the scan stops at 0 probes per byte; some owner directives below may be picked up already".to_owned(),
                "owner-directive: 1 — Owner directive: a".to_owned(),
                "owner-directive: 1 — Owner directive: b".to_owned()
            ])
        );
    }

    #[test]
    fn the_scan_stops_at_the_first_probe_past_its_budget_and_says_so() {
        let directives: Vec<Vec<u8>> = ["1 — Owner directive: a", "1 — Owner directive: b"]
            .iter()
            .map(|heading| heading.as_bytes().to_vec())
            .collect();
        let line = |heading: &[u8]| [MARKER.as_bytes(), heading].concat();
        let (first, second) = (line(&directives[0]), line(&directives[1]));
        let mut index = Pickups::of(&directives, BASE, 2);
        assert!(index.scan(&first));
        assert!(!index.scan(&second));
        assert!(index.picked(&directives[0]));
        assert!(!index.picked(&directives[1]));
        let mut index = Pickups::of(&directives, BASE, 4);
        assert!(index.scan(&first) && index.scan(&second));
        assert!(index.picked(&directives[1]));
    }

    #[test]
    fn a_pickup_marker_without_its_em_dash_picks_nothing_up() {
        for marker in [
            "Directive picked up: ",
            "- Directive picked up: ",
            "-- Directive picked up: ",
            "— Directive picked up:",
        ] {
            let text = below_stamp(format!(
                "## 1 — Owner directive: a\n{marker}1 — Owner directive: a\n"
            ));
            assert_eq!(
                listed(&text, STAMP),
                Some(vec!["owner-directive: 1 — Owner directive: a".to_owned()]),
                "{marker:?}"
            );
        }
    }

    #[test]
    fn a_stamp_is_matched_as_written_not_as_a_pattern() {
        let text = "## 2026-11-19T17:38Z — s\n## 1 — Owner directive: x\n## aaa — t\n## 1 — Owner directive: y\n## a[b — u\n## 1 — Owner directive: z\n## a\\b — v\n## 1 — Owner directive: w\n";
        assert_eq!(listed(text, "2026-11-19T17.38Z"), None);
        assert_eq!(listed(text, "a*"), None);
        assert_eq!(
            listed(text, "a[b"),
            Some(vec![
                "owner-directive: 1 — Owner directive: z".to_owned(),
                "owner-directive: 1 — Owner directive: w".to_owned()
            ])
        );
        assert_eq!(
            listed(text, "a\\b"),
            Some(vec!["owner-directive: 1 — Owner directive: w".to_owned()])
        );
    }

    #[test]
    fn a_nul_in_the_journal_or_a_handoff_is_a_byte_like_any_other() {
        let text = format!("## {STAMP} — s\0\n## 1 — Owner directive: a\0b\n");
        assert_eq!(
            listed(&text, STAMP),
            Some(vec![
                "owner-directive: 1 — Owner directive: a\0b".to_owned()
            ])
        );
        let mut found = HashSet::new();
        stamps_in(&b"Handoff: x\0y stamp_entry=S\0T\n"[..], &mut found).unwrap();
        assert_eq!(sorted(found), [b"S\0T".to_vec()]);
    }

    #[test]
    fn a_hash_match_counts_only_when_the_bytes_agree_and_every_heading_with_that_hash_is_tried() {
        let one = "1 — Owner directive: ";
        for (base, first, second) in [(1, "ab", "ba"), (0, "xa", "ya")] {
            let text = below_stamp(format!(
                "## {one}{first}\n## {one}{second}\n{MARKER}{one}{second}\n"
            ));
            assert_eq!(
                unpicked_by(text.as_slice(), STAMP.as_bytes(), base, PROBES_PER_BYTE).unwrap(),
                Some(vec![format!("owner-directive: {one}{first}")]),
                "base {base}"
            );
        }
    }

    #[test]
    fn a_pickup_is_matched_whether_its_text_ends_inside_a_block_on_its_edge_or_at_its_line_s_end() {
        let heading = "1 — Owner directive: h";
        let reach = MARKER.len() + heading.len();
        for line in [3 * BLOCK, 64 * BLOCK + 7] {
            let edges = [BLOCK, 2 * BLOCK, line];
            for end in edges.into_iter().flat_map(|edge| edge - 5..=edge + 2) {
                if end > line {
                    continue;
                }
                let pad = "p".repeat(end - reach);
                let tail = "q".repeat(line - end);
                let text = below_stamp(format!(
                    "## {heading}\n## {heading}z\n{pad}{MARKER}{heading}{tail}\n{pad}{MARKER}{}{tail}\n",
                    &heading[..heading.len() - 1]
                ));
                assert_eq!(
                    unpicked_by(text.as_slice(), STAMP.as_bytes(), BASE, PROBES_PER_BYTE).unwrap(),
                    Some(vec![format!("owner-directive: {heading}z")]),
                    "a pickup ending at {end} of a {line}-byte line"
                );
            }
        }
    }

    // Each owner directive below the first stamp heading is listed unless some line holds the
    // marker followed by its heading: heartbeat.sh's grep pipeline, one `grep -cF` per directive.
    fn grep_f_per_directive<'a>(journal: &'a [u8], stamp: &[u8]) -> Option<Vec<String>> {
        let lines: Vec<&[u8]> = journal.split(|byte| *byte == b'\n').collect();
        let heading = [b"## ", stamp, b" "].concat();
        let at = lines.iter().position(|line| line.starts_with(&heading))?;
        let stamped = |line: &&'a [u8]| -> Option<&'a [u8]> {
            let rest = line.strip_prefix(b"## ")?;
            let stamp = rest
                .iter()
                .position(|byte| !(byte.is_ascii_digit() || b"TZ:-".contains(byte)))
                .unwrap_or(rest.len());
            (stamp > 0 && rest[stamp..].starts_with(" — Owner directive:".as_bytes()))
                .then_some(rest)
        };
        let unpicked = |heading: &&[u8]| {
            let pattern = [MARKER.as_bytes(), heading].concat();
            !lines
                .iter()
                .any(|line| line.windows(pattern.len()).any(|window| window == pattern))
        };
        Some(
            lines[at + 1..]
                .iter()
                .filter_map(stamped)
                .filter(unpicked)
                .map(|heading| format!("owner-directive: {}", String::from_utf8_lossy(heading)))
                .collect(),
        )
    }

    struct Xorshift(u64);

    impl Xorshift {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }
    }

    const TOKENS: [&str; 18] = [
        "## ",
        "1",
        "1 ",
        "12",
        " — Owner directive:",
        " — Owner directive: ",
        "x",
        "y",
        MARKER,
        MARKER,
        "— Directive picked up:",
        "\n",
        "\n",
        "\n## 1 — Owner directive: x",
        "\r",
        "\0",
        "—",
        " ",
    ];

    // Two million journals in all, a quarter per test so they run side by side: under a fresh
    // random base, and under bases 0, 1 and 2, whose hashes agree for many different texts.
    fn random_journals_match_one_grep_f_per_directive(seed: u64, base: impl Fn() -> u64) {
        let mut random = Xorshift(seed);
        for _ in 0..500_000 {
            let mut text = String::new();
            if random.below(4) > 0 {
                text.push_str("## 1 \n");
            }
            for _ in 0..random.below(28) {
                text.push_str(TOKENS[random.below(TOKENS.len())]);
            }
            let base = base();
            assert_eq!(
                unpicked_by(text.as_bytes(), b"1", base, PROBES_PER_BYTE).unwrap(),
                grep_f_per_directive(text.as_bytes(), b"1"),
                "base {base}: {text:?}"
            );
        }
    }

    #[test]
    fn random_journals_list_what_one_grep_f_per_directive_lists_under_a_random_base() {
        random_journals_match_one_grep_f_per_directive(0x9e37_79b9_7f4a_7c15, random_base);
    }

    #[test]
    fn random_journals_list_what_one_grep_f_per_directive_lists_under_base_0_which_hashes_the_last_byte()
     {
        random_journals_match_one_grep_f_per_directive(0x2545_f491_4f6c_dd1d, || 0);
    }

    #[test]
    fn random_journals_list_what_one_grep_f_per_directive_lists_under_base_1_which_sums_the_bytes()
    {
        random_journals_match_one_grep_f_per_directive(0xd1b5_4a32_d192_ed03, || 1);
    }

    #[test]
    fn random_journals_list_what_one_grep_f_per_directive_lists_under_base_2() {
        random_journals_match_one_grep_f_per_directive(0x8cb9_2ba7_2f3d_8dd7, || 2);
    }
}
