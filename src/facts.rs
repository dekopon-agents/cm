use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::idea;
use crate::outcome::{Outcome, Violation};
use crate::source::{self, Pin, Shape};
use crate::store::write_atomic;

const KNOWN: &[&str] = &[
    "rs", "md", "toml", "yaml", "yml", "json", "jsonl", "py", "sh", "bash", "zsh", "js", "mjs",
    "cjs", "ts", "tsx", "jsx", "go", "c", "h", "cc", "cpp", "hpp", "swift", "kt", "java", "rb",
    "lua", "sql", "proto", "html", "css", "txt", "lock", "nix", "tf", "hcl", "cfg", "ini", "conf",
    "xml", "svg", "plist", "csv", "tsv", "mdx", "tpl", "j2", "service",
];
const PROSE: &[&str] = &["Node.js", "Next.js", "Vue.js"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub path: String,
    pub ranges: Vec<(usize, usize)>,
    pub text: String,
}

fn path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_.-/".contains(c)
}

fn file_like(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, ext)) = last.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !ext.is_empty()
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic())
        && !PROSE.contains(&path)
        && (path.contains('/') || KNOWN.contains(&ext))
}

/// Line lists after a path: `:N`, `:A-B`, `:A,B`, `:36–40,143–145`. Returns the ranges and the
/// bytes consumed.
fn line_list(rest: &str) -> (Vec<(usize, usize)>, usize) {
    let Some(body) = rest.strip_prefix(':') else {
        return (Vec::new(), 0);
    };
    let mut ranges = Vec::new();
    let mut at = 0;
    let mut consumed = 0;
    loop {
        let digits = |from: usize| body[from..].bytes().take_while(u8::is_ascii_digit).count();
        let n = digits(at);
        if n == 0 {
            break;
        }
        let from: usize = body[at..at + n].parse().unwrap_or(0);
        at += n;
        let mut to = from;
        let dash = ["-", "–"]
            .iter()
            .find(|dash| body[at..].starts_with(**dash));
        if let Some(dash) = dash {
            let m = digits(at + dash.len());
            if m > 0 {
                to = body[at + dash.len()..at + dash.len() + m]
                    .parse()
                    .unwrap_or(from);
                at += dash.len() + m;
            }
        }
        if from > 0 {
            ranges.push((from, to.max(from)));
        }
        consumed = at + 1;
        if body[at..].starts_with(',') && digits(at + 1) > 0 {
            at += 1;
        } else {
            break;
        }
    }
    (ranges, consumed)
}

/// File citations in one line of Markdown: `path`, `path:N`, `path:A-B`, `path:A,B`, in backticks
/// or plain text. URLs and link targets are not citations.
pub fn citations(line: &str) -> Vec<Citation> {
    let mut out = Vec::new();
    let mut rest_at = 0;
    while let Some(offset) = line[rest_at..].find(path_char) {
        let start = rest_at + offset;
        let end = line[start..]
            .find(|c: char| !path_char(c))
            .map_or(line.len(), |n| start + n);
        rest_at = end;
        let raw = &line[start..end];
        let before = &line[..start];
        if raw.starts_with("//") || before.ends_with("](") || before.ends_with('@') {
            continue;
        }
        let kept = raw.trim_end_matches(['.', '-']);
        let trimmed = kept.trim_start_matches('-');
        if !file_like(trimmed) {
            continue;
        }
        let (ranges, consumed) = if kept.len() == raw.len() {
            line_list(&line[end..])
        } else {
            (Vec::new(), 0)
        };
        rest_at = end + consumed;
        if line[end + consumed..].starts_with("](") {
            continue;
        }
        let text = if consumed > 0 {
            format!("{trimmed}{}", &line[end..end + consumed])
        } else {
            trimmed.to_owned()
        };
        out.push(Citation {
            path: trimmed.to_owned(),
            ranges,
            text,
        });
    }
    out
}

fn normalized(path: &str) -> &str {
    let mut path = path;
    loop {
        let next = path
            .trim_start_matches("./")
            .trim_start_matches("../")
            .trim_start_matches('/');
        if next == path {
            return path;
        }
        path = next;
    }
}

struct Tree {
    pin: Pin,
    files: Vec<String>,
    exact: HashSet<String>,
}

struct Blob {
    lines: usize,
    shape: Shape,
    entries: Vec<(usize, String)>,
}

struct Resolver {
    trees: Vec<Tree>,
    blobs: HashMap<(usize, String), Option<Blob>>,
}

impl Resolver {
    /// Exact paths first, then `/path` suffixes, then a loose match whose last component is equal
    /// and whose other components end with the cited ones (`model/src/wire.rs` finds
    /// `crates/dekopon-model/src/wire.rs`).
    fn matches(&self, path: &str) -> (Vec<(usize, String)>, bool) {
        let path = normalized(path);
        let find = |keep: &dyn Fn(&str) -> bool| -> Vec<(usize, String)> {
            self.trees
                .iter()
                .enumerate()
                .flat_map(|(index, tree)| {
                    tree.files
                        .iter()
                        .filter(|file| keep(file))
                        .map(move |file| (index, file.clone()))
                })
                .collect()
        };
        let exact: Vec<(usize, String)> = self
            .trees
            .iter()
            .enumerate()
            .filter(|(_, tree)| tree.exact.contains(path))
            .map(|(index, _)| (index, path.to_owned()))
            .collect();
        if !exact.is_empty() {
            return (exact, false);
        }
        let suffix = format!("/{path}");
        let suffixed = find(&|file| file.ends_with(&suffix));
        if !suffixed.is_empty() || !path.contains('/') {
            return (suffixed, false);
        }
        let cited: Vec<&str> = path.split('/').collect();
        let loose = find(&|file| {
            let parts: Vec<&str> = file.split('/').collect();
            parts.len() >= cited.len()
                && parts[parts.len() - 1] == cited[cited.len() - 1]
                && parts[parts.len() - cited.len()..]
                    .iter()
                    .zip(&cited)
                    .all(|(part, want)| part.ends_with(want))
        });
        (loose, true)
    }

    fn blob(&mut self, tree: usize, path: &str) -> Option<&Blob> {
        let key = (tree, path.to_owned());
        if !self.blobs.contains_key(&key) {
            let pin = &self.trees[tree].pin;
            let blob = source::show(&pin.dir, &pin.sha, path).ok().map(|bytes| {
                let text = String::from_utf8_lossy(&bytes);
                let (shape, entries) = source::outline(path, &text);
                Blob {
                    lines: text.lines().count(),
                    shape,
                    entries,
                }
            });
            self.blobs.insert(key.clone(), blob);
        }
        self.blobs.get(&key).and_then(Option::as_ref)
    }
}

fn code(text: &str) -> String {
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

fn ranges_text(ranges: &[(usize, usize)]) -> String {
    ranges
        .iter()
        .map(|(from, to)| {
            if from == to {
                format!(":{from}")
            } else {
                format!(":{from}-{to}")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

struct Resolved {
    markdown: String,
    status: &'static str,
    json: Value,
}

fn resolve(resolver: &mut Resolver, citation: &Citation, seen: &[usize]) -> Resolved {
    let at = seen
        .iter()
        .map(|line| format!("L{line}"))
        .collect::<Vec<_>>()
        .join(", ");
    let (found, loose) = resolver.matches(&citation.path);
    let mut results = Vec::new();
    for (tree, path) in &found {
        let alias = resolver.trees[*tree].pin.alias.clone();
        let Some(blob) = resolver.blob(*tree, path) else {
            results.push((
                alias,
                path.clone(),
                false,
                "unreadable at the pin".to_owned(),
            ));
            continue;
        };
        let mut notes = Vec::new();
        let mut stale = false;
        for (from, to) in &citation.ranges {
            if *to > blob.lines {
                stale = true;
                notes.push(format!(
                    ":{} past EOF ({} lines)",
                    if from > &blob.lines { from } else { to },
                    blob.lines
                ));
            } else if !matches!(blob.shape, Shape::Every(_))
                && let Some(text) = source::enclosing(&blob.entries, *from)
            {
                notes.push(format!(
                    "{} in {}",
                    ranges_text(&[(*from, *to)]),
                    code(text)
                ));
            }
        }
        results.push((alias, path.clone(), !stale, notes.join("; ")));
    }
    let entry = |alias: &str, path: &str, ok: bool, notes: &str| {
        let status = match (ok, loose) {
            (true, false) => "ok",
            (true, true) => "ok (loose match)",
            (false, false) => "STALE",
            (false, true) => "STALE (loose match)",
        };
        if notes.is_empty() {
            format!("{alias} {} {status}", code(path))
        } else {
            format!("{alias} {} {status}: {notes}", code(path))
        }
    };
    let (status, markdown) = match results.as_slice() {
        [] => (
            "stale",
            format!(
                "- {at} {} — STALE: no {} at any pin",
                code(&citation.text),
                code(normalized(&citation.path))
            ),
        ),
        [(alias, path, ok, notes)] => (
            if *ok { "ok" } else { "stale" },
            format!(
                "- {at} {} — {}",
                code(&citation.text),
                entry(alias, path, *ok, notes)
            ),
        ),
        many if many.len() > 5 && citation.ranges.is_empty() && many.iter().all(|one| one.2) => {
            let all: Vec<String> = many
                .iter()
                .map(|(alias, path, _, _)| format!("{alias} {}", code(path)))
                .collect();
            (
                "ambiguous",
                format!(
                    "- {at} {} — ambiguous, {} matches, all ok: {}",
                    code(&citation.text),
                    many.len(),
                    all.join(", ")
                ),
            )
        }
        many => {
            let mut text = format!(
                "- {at} {} — ambiguous, {} matches:",
                code(&citation.text),
                many.len()
            );
            for (alias, path, ok, notes) in many {
                text.push_str(&format!("\n  - {}", entry(alias, path, *ok, notes)));
            }
            ("ambiguous", text)
        }
    };
    let json = json!({
        "citation": citation.text,
        "record_lines": seen,
        "status": status,
        "loose": loose,
        "matches": results.iter().map(|(alias, path, ok, notes)| json!({
            "alias": alias, "path": path, "ok": ok, "notes": notes,
        })).collect::<Vec<_>>(),
    });
    Resolved {
        markdown,
        status,
        json,
    }
}

pub struct FactsArgs<'a> {
    pub pins: &'a [Pin],
    pub ideas: &'a Path,
    pub ids: &'a [String],
    pub out: Option<&'a Path>,
}

/// `cm idea facts`: verify every pin, then resolve each record's file citations at the pins.
pub fn facts(args: &FactsArgs<'_>) -> Result<Outcome> {
    let mut violations = Vec::new();
    let mut seen_alias = HashSet::new();
    for pin in args.pins {
        if !seen_alias.insert(pin.alias.as_str()) {
            violations.push(Violation::new(
                &pin.alias,
                "pin-duplicate",
                "alias pinned twice",
            ));
        } else if !source::commit_exists(&pin.dir, &pin.sha) {
            violations.push(Violation::new(
                &pin.alias,
                "pin-missing",
                format!(
                    "commit {} is not in {} (`git cat-file -e {}^{{commit}}` failed)",
                    pin.sha,
                    source::shown(&pin.dir),
                    pin.sha
                ),
            ));
        }
    }
    let mut records = Vec::new();
    for id in args.ids {
        match idea::record(args.ideas, id) {
            Some(path) => records.push((id.clone(), path)),
            None => violations.push(Violation::new(
                id,
                "idea-unknown",
                format!("no {id}.md or {id}-*.md in {}", args.ideas.display()),
            )),
        }
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let mut trees = Vec::new();
    for pin in args.pins {
        let sha = source::full_sha(&pin.dir, &pin.sha).unwrap_or_else(|| pin.sha.clone());
        let files = source::files(&pin.dir, &sha)?;
        trees.push(Tree {
            pin: Pin { sha, ..pin.clone() },
            exact: files.iter().cloned().collect(),
            files,
        });
    }
    let mut resolver = Resolver {
        trees,
        blobs: HashMap::new(),
    };
    let pins_file = args
        .out
        .map_or("<this file>".to_owned(), |out| out.display().to_string());
    let mut doc = format!(
        "# Facts: {}\n\nWritten by `cm idea facts`. Every pin was verified with `git cat-file -e <sha>^{{commit}}`. Each citation is resolved at the pins: `ok`, or `STALE` when the path is missing at every pin or a cited line is past its end. Line citations name the enclosing heading or item.\n\nRead sources with `cm read --pins {pins_file} <alias>:<path>[:A-B | :#Heading] …`.\n\n## Pins\n\n| Alias | Repository | Commit |\n|---|---|---|\n",
        args.ids.join(", ")
    );
    for tree in &resolver.trees {
        doc.push_str(&format!(
            "| {} | {} | {} |\n",
            tree.pin.alias,
            source::shown(&tree.pin.dir),
            tree.pin.sha
        ));
    }
    let mut json_records = Vec::new();
    let (mut total, mut stale, mut ambiguous) = (0, 0, 0);
    for (id, path) in &records {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut grouped: BTreeMap<String, (Citation, Vec<usize>, usize)> = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            for citation in citations(line) {
                let order = grouped.len();
                grouped
                    .entry(citation.text.clone())
                    .or_insert_with(|| (citation, Vec::new(), order))
                    .1
                    .push(index + 1);
            }
        }
        let mut ordered: Vec<_> = grouped.into_values().collect();
        ordered.sort_by_key(|(_, _, order)| *order);
        let mut lines = Vec::new();
        let mut json_citations = Vec::new();
        let mut record_stale = 0;
        for (citation, seen, _) in &mut ordered {
            seen.dedup();
            let resolved = resolve(&mut resolver, citation, seen);
            match resolved.status {
                "stale" => record_stale += 1,
                "ambiguous" => ambiguous += 1,
                _ => {}
            }
            lines.push(resolved.markdown);
            json_citations.push(resolved.json);
        }
        total += ordered.len();
        stale += record_stale;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        doc.push_str(&format!(
            "\n## {id}\n\n{} — {} citations, {record_stale} STALE.\n",
            code(&name),
            ordered.len()
        ));
        if !lines.is_empty() {
            doc.push('\n');
            doc.push_str(&lines.join("\n"));
            doc.push('\n');
        }
        json_records.push(json!({ "id": id, "path": name, "citations": json_citations }));
    }
    let mut outcome = Outcome::default();
    outcome.field("records", Value::Array(json_records));
    outcome.field("citations", total);
    outcome.field("stale", stale);
    outcome.field("ambiguous", ambiguous);
    let summary = format!(
        "{} records, {total} citations, {stale} STALE, {ambiguous} ambiguous; pins {}",
        records.len(),
        resolver
            .trees
            .iter()
            .map(|tree| format!("{} {}", tree.pin.alias, tree.pin.short()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    match args.out {
        Some(out) => {
            write_atomic(out, doc.as_bytes())?;
            outcome.field("out", out.display().to_string());
            outcome.line(format!("wrote {}: {summary}", out.display()));
        }
        None => {
            outcome.field("markdown", doc.clone());
            for line in doc.lines() {
                outcome.line(line);
            }
        }
    }
    Ok(outcome)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    Env,
}

fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn spans(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(offset) = line[at..].find('`') {
        let open = at + offset;
        let run = line[open..]
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();
        let fence = &line[open..open + run];
        let body = open + run;
        let mut search = body;
        let mut closed = None;
        while let Some(found) = line[search..].find(fence) {
            let close = search + found;
            let length = line[close..]
                .bytes()
                .take_while(|byte| *byte == b'`')
                .count();
            if length == run {
                closed = Some(close);
                break;
            }
            search = close + length;
        }
        match closed {
            Some(close) => {
                let span = line[body..close].trim();
                if !span.is_empty() {
                    out.push(span);
                }
                at = close + run;
            }
            None => at = body,
        }
    }
    out
}

fn env_tokens(line: &str) -> Vec<&str> {
    line.split(|c: char| !ident_char(c))
        .filter(|word| {
            word.contains('_')
                && word.starts_with(|c: char| c.is_ascii_uppercase())
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && word.chars().filter(char::is_ascii_uppercase).count() >= 2
                && word.split('_').filter(|part| !part.is_empty()).count() >= 2
        })
        .collect()
}

struct IdeaRef {
    target: String,
    ranges: Vec<(usize, usize)>,
    text: String,
}

/// `IDEA-NNNN:line` references (any uppercase prefix), with their ranges.
fn idea_refs(line: &str) -> Vec<IdeaRef> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_uppercase() || (at > 0 && ident_char(bytes[at - 1] as char)) {
            at += 1;
            continue;
        }
        let letters = bytes[at..]
            .iter()
            .take_while(|byte| byte.is_ascii_uppercase())
            .count();
        let dash = at + letters;
        let digits = bytes.get(dash + 1..).map_or(0, |rest| {
            rest.iter().take_while(|byte| byte.is_ascii_digit()).count()
        });
        if bytes.get(dash) != Some(&b'-') || digits == 0 {
            at += letters.max(1);
            continue;
        }
        let id_end = dash + 1 + digits;
        let (ranges, consumed) = line_list(&line[id_end..]);
        if ranges.is_empty() {
            at = id_end;
            continue;
        }
        out.push(IdeaRef {
            target: line[at..id_end].to_owned(),
            ranges,
            text: line[at..id_end + consumed].to_owned(),
        });
        at = id_end + consumed;
    }
    out
}

fn bare_ids(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| {
            word.split_once('-').is_some_and(|(prefix, number)| {
                prefix.len() >= 2
                    && prefix.bytes().all(|byte| byte.is_ascii_uppercase())
                    && !number.is_empty()
                    && number.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
        .collect()
}

fn link_targets(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find("](") {
        let start = at + offset + 2;
        let end = text[start..].find(')').map_or(text.len(), |n| start + n);
        let target = text[start..end]
            .split('#')
            .next()
            .unwrap_or_default()
            .trim();
        if !target.is_empty() && !target.contains("://") && !target.starts_with("mailto:") {
            out.push(target);
        }
        at = end;
    }
    out
}

struct Linked {
    label: String,
    lines: Vec<String>,
}

fn added_lines(dir: &Path, base: &str, file: &str) -> Result<Vec<(usize, String)>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--unified=0",
            base,
            "--",
            file,
        ])
        .output()
        .with_context(|| format!("running git diff in {}", dir.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff {base} -- {file}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    let mut next = 0;
    for line in text.lines() {
        if let Some(hunk) = line.strip_prefix("@@ ") {
            next = hunk
                .split_whitespace()
                .find_map(|part| part.strip_prefix('+'))
                .and_then(|part| part.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if let Some(added) = line.strip_prefix('+') {
            if line.starts_with("+++") && next == 0 {
                continue;
            }
            out.push((next, added.to_owned()));
            next += 1;
        }
    }
    Ok(out)
}

enum Found {
    Exact(Vec<String>),
    Mismatch(Vec<(String, String)>),
    Absent,
}

fn search(token: &str, linked: &[Linked]) -> Found {
    let check_before = token.starts_with(ident_char);
    let check_after = token.ends_with(ident_char);
    let mut exact = Vec::new();
    let mut longer = Vec::new();
    for file in linked {
        for (index, line) in file.lines.iter().enumerate() {
            let mut from = 0;
            while let Some(offset) = line[from..].find(token) {
                let start = from + offset;
                let end = start + token.len();
                let before = line[..start].chars().next_back();
                let after = line[end..].chars().next();
                let embedded_before = check_before && before.is_some_and(ident_char);
                let embedded_after = check_after && after.is_some_and(ident_char);
                let place = format!("{}:{}", file.label, index + 1);
                if !embedded_before && !embedded_after {
                    exact.push(place);
                } else {
                    let left = line[..start]
                        .char_indices()
                        .rev()
                        .take_while(|(_, c)| ident_char(*c))
                        .last()
                        .map_or(start, |(at, _)| at);
                    let right = line[end..]
                        .char_indices()
                        .take_while(|(_, c)| ident_char(*c))
                        .last()
                        .map_or(end, |(at, c)| end + at + c.len_utf8());
                    longer.push((line[left..right].to_owned(), place));
                }
                from = end;
            }
        }
    }
    if !exact.is_empty() {
        Found::Exact(exact)
    } else if !longer.is_empty() {
        Found::Mismatch(longer)
    } else {
        Found::Absent
    }
}

fn record_lines(ideas: &Path, id: &str) -> Option<Vec<String>> {
    let path = idea::record(ideas, id)?;
    let text = fs::read_to_string(path).ok()?;
    Some(text.lines().map(str::to_owned).collect())
}

fn listed<T: AsRef<str>>(places: &[T]) -> String {
    let shown: Vec<&str> = places.iter().take(3).map(AsRef::as_ref).collect();
    let more = places.len().saturating_sub(3);
    if more > 0 {
        format!("{} (+{more} more)", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// `cm idea idents`: identifiers added since a base, checked against the records each record links.
pub fn idents(ideas: &Path, base: &str, ids: &[String]) -> Result<Outcome> {
    let mut violations = Vec::new();
    let Some(base_sha) = source::full_sha(ideas, base) else {
        return Ok(Outcome::refused(vec![Violation::new(
            base,
            "base-unknown",
            format!("no commit {base} in {}", ideas.display()),
        )]));
    };
    let mut records = Vec::new();
    for id in ids {
        match idea::record(ideas, id) {
            Some(path) => records.push((id.clone(), path)),
            None => violations.push(Violation::new(
                id,
                "idea-unknown",
                format!("no {id}.md or {id}-*.md in {}", ideas.display()),
            )),
        }
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    let mut outcome = Outcome::default();
    let mut json_records = Vec::new();
    let mut mismatches = 0;
    for (id, path) in &records {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut linked: Vec<Linked> = Vec::new();
        let mut linked_paths: HashSet<PathBuf> = HashSet::new();
        let own = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        linked_paths.insert(own);
        let record_dir = path.parent().unwrap_or(ideas).to_path_buf();
        let mut candidates: Vec<(PathBuf, String)> = Vec::new();
        for target in link_targets(&text) {
            let file = record_dir.join(target);
            let in_ideas = file.parent() == Some(record_dir.as_path());
            match idea::record_id(&file) {
                Some(other) if other == *id => continue,
                Some(other) if in_ideas => candidates.push((file, other)),
                _ => candidates.push((file, target.to_owned())),
            }
        }
        for other in bare_ids(&text) {
            if let Some(file) = idea::record(ideas, other) {
                candidates.push((file, other.to_owned()));
            }
        }
        for (file, label) in candidates {
            let Ok(canonical) = fs::canonicalize(&file) else {
                continue;
            };
            if !canonical.is_file() || !linked_paths.insert(canonical) {
                continue;
            }
            if let Ok(body) = fs::read_to_string(&file) {
                linked.push(Linked {
                    label,
                    lines: body.lines().map(str::to_owned).collect(),
                });
            }
        }
        let added = added_lines(ideas, &base_sha, &name)?;
        let mut tokens: Vec<(String, Kind, Vec<usize>)> = Vec::new();
        let mut refs: Vec<(IdeaRef, usize)> = Vec::new();
        let mut push = |token: &str, kind: Kind, line: usize| match tokens
            .iter_mut()
            .find(|(seen, _, _)| seen == token)
        {
            Some((_, _, lines)) => {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
            None => tokens.push((token.to_owned(), kind, vec![line])),
        };
        for (number, line) in &added {
            let line_refs = idea_refs(line);
            for span in spans(line) {
                if span.chars().count() > 200 || line_refs.iter().any(|one| one.text == span) {
                    continue;
                }
                push(span, Kind::Code, *number);
            }
            for token in env_tokens(line) {
                push(token, Kind::Env, *number);
            }
            for one in line_refs {
                refs.push((one, *number));
            }
        }
        let labels: Vec<&str> = linked.iter().map(|file| file.label.as_str()).collect();
        outcome.line(format!(
            "== {id} ({name}): {} added lines since {}; linked: {}",
            added.len(),
            &base_sha[..10],
            if labels.is_empty() {
                "none".to_owned()
            } else {
                labels.join(", ")
            }
        ));
        let mut rows = Vec::new();
        for (token, kind, lines) in &tokens {
            let at = lines
                .iter()
                .map(|n| format!("L{n}"))
                .collect::<Vec<_>>()
                .join(",");
            let kind_name = match kind {
                Kind::Code => "code",
                Kind::Env => "env",
            };
            let (status, evidence, order) = match search(token, &linked) {
                Found::Mismatch(longer) => {
                    mismatches += 1;
                    let places: Vec<String> = longer
                        .iter()
                        .map(|(word, place)| format!("`{word}` at {place}"))
                        .collect();
                    ("MISMATCH", format!("only inside {}", listed(&places)), 0)
                }
                Found::Absent => ("absent", String::new(), 1),
                Found::Exact(places) => ("exact", listed(&places), 2),
            };
            rows.push((order, status, token.clone(), kind_name, at, evidence));
        }
        rows.sort_by_key(|row| row.0);
        let mut json_tokens = Vec::new();
        for (_, status, token, kind, at, evidence) in &rows {
            let line = if evidence.is_empty() {
                format!("{status:<8}  `{token}` ({kind}, {at})")
            } else {
                format!("{status:<8}  `{token}` ({kind}, {at}): {evidence}")
            };
            outcome.line(line);
            json_tokens.push(json!({
                "token": token, "kind": kind, "lines": at, "status": status, "evidence": evidence,
            }));
        }
        let mut json_refs = Vec::new();
        for (
            IdeaRef {
                target,
                ranges,
                text,
            },
            number,
        ) in &refs
        {
            let lines = record_lines(ideas, target);
            let mut shown = Vec::new();
            let status = match &lines {
                None => "STALE: no such record".to_owned(),
                Some(lines) => {
                    let mut stale = None;
                    for (from, to) in ranges {
                        if *from > lines.len() || *to > lines.len() {
                            stale = Some(format!("STALE: past EOF ({} lines)", lines.len()));
                            continue;
                        }
                        for n in (*from..=*to).take(10) {
                            shown.push(format!(
                                "{target}:{n}: {}",
                                source::clip(&lines[n - 1], 240)
                            ));
                        }
                    }
                    stale.unwrap_or_else(|| "ok".to_owned())
                }
            };
            outcome.line(format!("ref       {text} (L{number}): {status}"));
            for line in &shown {
                outcome.line(format!("          {line}"));
            }
            json_refs.push(
                json!({ "ref": text, "line": number, "status": status, "target_lines": shown }),
            );
        }
        json_records.push(json!({
            "id": id, "path": name, "added_lines": added.len(), "linked": labels,
            "tokens": json_tokens, "refs": json_refs,
        }));
    }
    outcome.field("base", base_sha);
    outcome.field("records", Value::Array(json_records));
    outcome.field("mismatches", mismatches);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(line: &str) -> Vec<String> {
        citations(line).into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn citation_shapes() {
        assert_eq!(
            texts("see `crates/x/src/gateway.rs:418` and wake.rs:754, plus docs/design.md."),
            vec![
                "crates/x/src/gateway.rs:418",
                "wake.rs:754",
                "docs/design.md"
            ]
        );
        let found = citations("ci.yml:36–40,143–145 then a.rs:1,9 and b.rs:3, c");
        assert_eq!(found[0].ranges, vec![(36, 40), (143, 145)]);
        assert_eq!(found[1].ranges, vec![(1, 1), (9, 9)]);
        assert_eq!(found[2].ranges, vec![(3, 3)]);
        assert!(
            texts("https://github.com/o/r/blob/x/store/ideas/A.md e.g. 0.15.0 docs.github.com")
                .is_empty()
        );
        assert!(texts("[IDEA-1](IDEA-1-x.md) Node.js R1/R7 and/or").is_empty());
    }

    #[test]
    fn tokens_and_refs() {
        let line =
            "Claude needs `DISABLE_NONESSENTIAL_TRAFFIC` (see IDEA-0138:117–118, `cm read`).";
        assert_eq!(spans(line), vec!["DISABLE_NONESSENTIAL_TRAFFIC", "cm read"]);
        assert_eq!(env_tokens(line), vec!["DISABLE_NONESSENTIAL_TRAFFIC"]);
        let refs = idea_refs(line);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "IDEA-0138");
        assert_eq!(refs[0].ranges, vec![(117, 118)]);
        assert!(env_tokens("R1_ and OK A_1").is_empty());
    }

    #[test]
    fn embedded_tokens_are_mismatches() {
        let linked = vec![Linked {
            label: "IDEA-0138".into(),
            lines: vec!["placeholder `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`".into()],
        }];
        match search("DISABLE_NONESSENTIAL_TRAFFIC", &linked) {
            Found::Mismatch(longer) => assert_eq!(
                longer,
                vec![(
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
                    "IDEA-0138:1".to_owned()
                )]
            ),
            _ => panic!("expected a mismatch"),
        }
        assert!(matches!(
            search("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1", &linked),
            Found::Exact(_)
        ));
        assert!(matches!(search("NOPE_THING", &linked), Found::Absent));
    }
}
