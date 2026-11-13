use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::markdown::headings;
use crate::outcome::{Outcome, Violation};

pub const WHOLE_BYTES: usize = 8 * 1024;
pub const CAP_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub alias: String,
    pub dir: PathBuf,
    pub sha: String,
}

impl std::str::FromStr for Pin {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let shape = || format!("`{text}`: expected ALIAS=DIR@SHA");
        let (alias, rest) = text.split_once('=').ok_or_else(shape)?;
        let (dir, sha) = rest.rsplit_once('@').ok_or_else(shape)?;
        if !crate::store::is_name(alias) {
            return Err(format!(
                "alias `{alias}`: use letters, digits, `.`, `_` and `-`"
            ));
        }
        if dir.is_empty() {
            return Err(shape());
        }
        if !crate::state::is_commit(sha) {
            return Err(format!(
                "`{sha}`: expected a commit SHA (7 to 40 hexadecimal digits)"
            ));
        }
        Ok(Pin {
            alias: alias.to_owned(),
            dir: expand(dir),
            sha: sha.to_ascii_lowercase(),
        })
    }
}

impl Pin {
    pub fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(10)]
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn expand(dir: &str) -> PathBuf {
    match (dir.strip_prefix('~'), home()) {
        (Some(""), Some(home)) => home,
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(dir),
    }
}

pub fn shown(dir: &Path) -> String {
    match home().and_then(|home| dir.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    }
}

/// Reads the pins table under a `Pins` heading: `| alias | repository | commit |` rows.
pub fn parse_pins(text: &str) -> Result<Vec<Pin>> {
    let mut inside = false;
    let mut pins = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            inside = line
                .trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("pins");
            continue;
        }
        let Some(row) = line.trim().strip_prefix('|') else {
            continue;
        };
        if !inside {
            continue;
        }
        let cells: Vec<&str> = row
            .trim_end_matches('|')
            .split('|')
            .map(|cell| cell.trim().trim_matches('`'))
            .collect();
        if cells.len() < 3 || cells[0].eq_ignore_ascii_case("alias") || cells[0].starts_with("--") {
            continue;
        }
        pins.push(
            format!("{}={}@{}", cells[0], cells[1], cells[2])
                .parse::<Pin>()
                .map_err(anyhow::Error::msg)?,
        );
    }
    if pins.is_empty() {
        bail!("no `| alias | repository | commit |` rows under a `Pins` heading");
    }
    Ok(pins)
}

pub fn load_pins(file: Option<&Path>, given: &[Pin]) -> Result<Vec<Pin>> {
    let mut pins = match file {
        Some(file) => parse_pins(
            &std::fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()))?,
        )
        .with_context(|| format!("reading pins from {}", file.display()))?,
        None => Vec::new(),
    };
    pins.extend(given.iter().cloned());
    Ok(pins)
}

fn git(dir: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git").arg("-C").arg(dir).args(args).output()
}

fn failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .next()
        .unwrap_or("git failed")
        .trim_start_matches("fatal: ")
        .to_owned()
}

pub fn commit_exists(dir: &Path, sha: &str) -> bool {
    git(dir, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .is_ok_and(|output| output.status.success())
}

pub fn full_sha(dir: &Path, rev: &str) -> Option<String> {
    let output = git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn files(dir: &Path, sha: &str) -> Result<Vec<String>> {
    let output = git(dir, &["ls-tree", "-r", "-z", "--name-only", sha])
        .with_context(|| format!("running git in {}", dir.display()))?;
    if !output.status.success() {
        bail!("{}: {}", dir.display(), failure(&output));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect())
}

pub fn show(dir: &Path, sha: &str, path: &str) -> Result<Vec<u8>, String> {
    let output = git(dir, &["show", "--no-color", &format!("{sha}:{path}")])
        .map_err(|error| format!("running git in {}: {error}", dir.display()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failure(&output))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Headings,
    Items,
    Every(usize),
}

const CODE: &[&str] = &[
    "rs", "py", "go", "js", "mjs", "cjs", "ts", "tsx", "jsx", "java", "kt", "kts", "swift", "c",
    "h", "cc", "cpp", "hpp", "cs", "rb", "php", "scala", "lua", "ex", "exs", "proto", "sh", "bash",
    "zsh",
];

const KEYWORDS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "union",
    "impl",
    "trait",
    "mod",
    "type",
    "macro_rules!",
    "def",
    "class",
    "func",
    "fun",
    "function",
    "interface",
    "object",
    "module",
    "protocol",
    "extension",
    "message",
    "service",
];

const MODIFIERS: &[&str] = &[
    "pub",
    "export",
    "default",
    "public",
    "private",
    "protected",
    "internal",
    "final",
    "abstract",
    "override",
    "open",
    "sealed",
    "data",
    "async",
    "unsafe",
    "extern",
    "\"C\"",
    "inline",
    "virtual",
    "static",
    "const",
];

fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn is_markdown(path: &str) -> bool {
    matches!(extension(path).as_str(), "md" | "markdown" | "mdx")
}

fn item(line: &str, shell: bool) -> bool {
    let trimmed = line.trim_start();
    let top = trimmed.len() == line.len();
    if shell && top {
        let name = trimmed
            .split(|c: char| !(c.is_ascii_alphanumeric() || "_:.-".contains(c)))
            .next()
            .unwrap_or_default();
        if !name.is_empty() && trimmed[name.len()..].trim_start().starts_with("()") {
            return true;
        }
    }
    let mut binding = false;
    for word in trimmed.split_whitespace() {
        let bare = word.split(['(', '<', '{', ':']).next().unwrap_or(word);
        if KEYWORDS.contains(&bare) {
            return true;
        }
        if MODIFIERS.contains(&bare) {
            binding |= matches!(bare, "const" | "static");
            continue;
        }
        return binding && top;
    }
    false
}

pub(crate) fn clip(line: &str, max: usize) -> String {
    let line = line.trim_end();
    match line.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_owned(),
    }
}

/// Markdown headings, item definitions for code, table headers for TOML, top-level keys for YAML,
/// and every Nth line otherwise. Shared by `cm read` and `cm idea facts`.
pub fn outline(path: &str, text: &str) -> (Shape, Vec<(usize, String)>) {
    let lines: Vec<&str> = text.lines().collect();
    let pick = |keep: &dyn Fn(&str) -> bool| -> Vec<(usize, String)> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| keep(line))
            .map(|(index, line)| (index + 1, clip(line, 160)))
            .collect()
    };
    let ext = extension(path);
    let found = if is_markdown(path) {
        let entries: Vec<(usize, String)> = headings(text)
            .iter()
            .map(|heading| (heading.line, clip(lines[heading.line - 1], 160)))
            .collect();
        Some((Shape::Headings, entries))
    } else if CODE.contains(&ext.as_str()) {
        let shell = matches!(ext.as_str(), "sh" | "bash" | "zsh");
        Some((Shape::Items, pick(&|line| item(line, shell))))
    } else if ext == "toml" {
        Some((
            Shape::Items,
            pick(&|line| line.trim_start().starts_with('[')),
        ))
    } else if matches!(ext.as_str(), "yaml" | "yml") {
        Some((
            Shape::Items,
            pick(&|line| {
                line.starts_with("---")
                    || line.split_once(':').is_some_and(|(key, _)| {
                        !key.is_empty() && !key.starts_with(['#', ' ', '-']) && !key.contains(' ')
                    })
            }),
        ))
    } else {
        None
    };
    match found {
        Some((shape, entries)) if !entries.is_empty() => (shape, entries),
        _ => {
            let every = lines.len().div_ceil(80).max(20);
            let entries = (0..lines.len())
                .step_by(every)
                .map(|index| (index + 1, clip(lines[index], 160)))
                .collect();
            (Shape::Every(every), entries)
        }
    }
}

pub fn enclosing(entries: &[(usize, String)], line: usize) -> Option<&str> {
    entries
        .iter()
        .rev()
        .find(|(at, _)| *at <= line)
        .map(|(_, text)| text.trim())
}

pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Select {
    Whole,
    Lines(usize, Option<usize>),
    Heading(String),
}

#[derive(Debug)]
struct Spec {
    label: String,
    dir: PathBuf,
    sha: String,
    path: String,
    select: Select,
}

/// `N`, `A-B` or `A-` (en dashes accepted).
pub fn parse_range(text: &str) -> Option<(usize, Option<usize>)> {
    let text = text.replace('–', "-");
    let number = |part: &str| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<usize>().ok())
            .flatten()
            .filter(|n| *n > 0)
    };
    match text.split_once('-') {
        None => number(&text).map(|n| (n, Some(n))),
        Some((from, "")) => number(from).map(|from| (from, None)),
        Some((from, to)) => {
            let (from, to) = (number(from)?, number(to)?);
            (from <= to).then_some((from, Some(to)))
        }
    }
}

fn same_repo(a: &Path, b: &Path) -> bool {
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    canonical(a) == canonical(b)
}

fn parse_spec(text: &str, pins: &[Pin]) -> Result<Spec, String> {
    let (head, rest) = text
        .split_once(':')
        .ok_or_else(|| "expected <alias>:<path>, <dir>@<sha>:<path> or <sha>:<path>".to_owned())?;
    let (label, dir, sha) = if let Some(pin) = pins.iter().find(|pin| pin.alias == head) {
        (pin.alias.clone(), pin.dir.clone(), pin.sha.clone())
    } else {
        let (dir, sha) = match head.rsplit_once('@') {
            Some((dir, sha)) if crate::state::is_commit(sha) => (expand(dir), sha),
            _ if crate::state::is_commit(head) => (PathBuf::from("."), head),
            _ => {
                let known: Vec<&str> = pins.iter().map(|pin| pin.alias.as_str()).collect();
                return Err(if known.is_empty() {
                    format!("`{head}` is not a commit; aliases need --pins or --pin")
                } else {
                    format!("unknown alias `{head}`; pinned: {}", known.join(", "))
                });
            }
        };
        let sha = sha.to_ascii_lowercase();
        if !pins.is_empty()
            && !pins.iter().any(|pin| {
                (pin.sha.starts_with(&sha) || sha.starts_with(&pin.sha))
                    && same_repo(&pin.dir, &dir)
            })
        {
            return Err(format!(
                "{} @ {sha} is not pinned; read through a pinned alias",
                shown(&dir)
            ));
        }
        (
            format!("{} @ {}", shown(&dir), &sha[..sha.len().min(10)]),
            dir,
            sha,
        )
    };
    let (path, select) = if let Some(at) = rest.find(":#") {
        let query = rest[at + 2..].trim_start_matches('#').trim();
        (&rest[..at], Select::Heading(query.to_owned()))
    } else {
        match rest.rsplit_once(':') {
            Some((path, range)) => match parse_range(range) {
                Some((from, to)) => (path, Select::Lines(from, to)),
                None => return Err(format!("`{range}`: expected N, A-B, A- or #Heading")),
            },
            None => (rest, Select::Whole),
        }
    };
    let path = path.trim_start_matches("./").trim_start_matches('/');
    if path.is_empty() {
        return Err("no path".into());
    }
    Ok(Spec {
        label,
        dir,
        sha,
        path: path.to_owned(),
        select,
    })
}

fn heading_section(text: &str, query: &str) -> Result<(usize, usize), String> {
    let all = headings(text);
    let wanted = query.to_lowercase();
    let tiers: [&dyn Fn(&str) -> bool; 3] = [
        &|heading| heading == wanted,
        &|heading| heading.starts_with(&wanted),
        &|heading| heading.contains(&wanted),
    ];
    for tier in tiers {
        let found: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, heading)| tier(&heading.text.to_lowercase()))
            .map(|(index, _)| index)
            .collect();
        match found.as_slice() {
            [] => continue,
            [index] => {
                let heading = &all[*index];
                let end = all[index + 1..]
                    .iter()
                    .find(|next| next.level <= heading.level)
                    .map_or(text.lines().count(), |next| next.line - 1);
                return Ok((heading.line, end));
            }
            many => {
                let names: Vec<String> = many
                    .iter()
                    .map(|index| format!("{} (line {})", all[*index].text, all[*index].line))
                    .collect();
                return Err(format!(
                    "heading `{query}` is ambiguous: {}",
                    names.join("; ")
                ));
            }
        }
    }
    Err(format!("no heading matches `{query}`"))
}

struct Block {
    header: String,
    lines: Vec<(usize, String)>,
    mode: &'static str,
}

fn block(spec: &Spec, text: &str, bytes: usize) -> Result<Block, (&'static str, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let size = format!(
        "{} @ {}, {} B, {} lines",
        spec.label.split(" @ ").next().unwrap_or(&spec.label),
        &spec.sha[..spec.sha.len().min(10)],
        thousands(bytes),
        thousands(total)
    );
    let numbered = |from: usize, to: usize| -> Vec<(usize, String)> {
        (from..=to).map(|n| (n, lines[n - 1].to_owned())).collect()
    };
    let (from, to, mode) = match &spec.select {
        Select::Whole if bytes <= WHOLE_BYTES => (1, total, "whole"),
        Select::Whole => {
            let (shape, entries) = outline(&spec.path, text);
            let what = match shape {
                Shape::Headings => "Markdown headings".to_owned(),
                Shape::Items => "item definitions".to_owned(),
                Shape::Every(n) => format!("every {n}th line"),
            };
            let hint = if shape == Shape::Headings {
                " or a heading (`…:#Heading`)"
            } else {
                ""
            };
            return Ok(Block {
                header: format!(
                    "{size}: over 8 KB, so an outline of {what}; give a range (`…:A-B`){hint}"
                ),
                lines: entries,
                mode: "outline",
            });
        }
        Select::Lines(from, to) => {
            if *from > total {
                return Err((
                    "read-range",
                    format!("line {from} is past the end ({total} lines)"),
                ));
            }
            (*from, to.unwrap_or(total).min(total), "range")
        }
        Select::Heading(query) => {
            if !is_markdown(&spec.path) {
                return Err((
                    "read-spec",
                    "heading specs apply to Markdown; give a range".into(),
                ));
            }
            let (from, to) =
                heading_section(text, query).map_err(|message| ("read-heading", message))?;
            (from, to, "heading")
        }
    };
    let header = if mode == "whole" {
        size
    } else {
        format!("{size}; lines {from}-{to}")
    };
    Ok(Block {
        header,
        lines: if total == 0 {
            Vec::new()
        } else {
            numbered(from, to)
        },
        mode,
    })
}

/// `cm read`: pinned reads only, never the working tree; an outline for anything over 8 KB without
/// a range; at most about 64 KB per invocation, naming whatever it cuts.
pub fn read(specs: &[String], pins: &[Pin]) -> Outcome {
    let mut outcome = Outcome::default();
    let mut budget = CAP_BYTES;
    let mut reads = Vec::new();
    let mut cut = Vec::new();
    for text in specs {
        let spec = match parse_spec(text, pins) {
            Ok(spec) => spec,
            Err(message) => {
                outcome
                    .violations
                    .push(Violation::new(text, "read-spec", message));
                continue;
            }
        };
        let bytes = match show(&spec.dir, &spec.sha, &spec.path) {
            Ok(bytes) => bytes,
            Err(message) => {
                outcome
                    .violations
                    .push(Violation::new(text, "read-missing", message));
                continue;
            }
        };
        let tree_prefix = format!("tree {}:{}", spec.sha, spec.path);
        let body = String::from_utf8_lossy(&bytes);
        let block = if body.starts_with(&tree_prefix) {
            let entries: Vec<(usize, String)> = body
                .lines()
                .skip(2)
                .enumerate()
                .map(|(index, name)| (index + 1, name.to_owned()))
                .collect();
            Block {
                header: format!("{}: a directory of {} entries", spec.label, entries.len()),
                lines: entries,
                mode: "directory",
            }
        } else if bytes[..bytes.len().min(8000)].contains(&0) {
            Block {
                header: format!(
                    "{}: binary, {} B; not shown",
                    spec.label,
                    thousands(bytes.len())
                ),
                lines: Vec::new(),
                mode: "binary",
            }
        } else {
            match block(&spec, &body, bytes.len()) {
                Ok(block) => block,
                Err((rule, message)) => {
                    outcome.violations.push(Violation::new(text, rule, message));
                    continue;
                }
            }
        };
        let header = format!("==> {text} ({})", block.header);
        budget = budget.saturating_sub(header.len() + 1);
        outcome.line(header);
        let mut shown_lines = Vec::new();
        let mut cut_from = None;
        for (n, line) in &block.lines {
            let numbered = format!("{n:>6}\t{line}");
            if numbered.len() + 1 > budget {
                cut_from = Some(*n);
                break;
            }
            budget -= numbered.len() + 1;
            outcome.line(numbered.clone());
            shown_lines.push(numbered);
        }
        if let Some(from) = cut_from {
            let last = block.lines.last().map_or(from, |(n, _)| *n);
            let notice = if shown_lines.is_empty() {
                format!("[cut: {text} not shown; the 64 KB cap per call. Read it in another call.]")
            } else {
                format!(
                    "[cut: {text} lines {from}-{last} not shown; the 64 KB cap per call. Read them in another call.]"
                )
            };
            outcome.line(notice);
            cut.push(json!({ "spec": text, "from": from, "to": last }));
        }
        reads.push(json!({
            "spec": text,
            "repo": shown(&spec.dir),
            "sha": spec.sha,
            "path": spec.path,
            "bytes": bytes.len(),
            "mode": block.mode,
            "text": shown_lines.join("\n"),
            "cut_from": cut_from,
        }));
    }
    if !cut.is_empty() {
        let names: Vec<String> = cut
            .iter()
            .filter_map(|entry| entry["spec"].as_str().map(str::to_owned))
            .collect();
        outcome.line(format!("Cut by the 64 KB cap: {}", names.join(", ")));
    }
    outcome.field("reads", Value::Array(reads));
    outcome.field("cut", Value::Array(cut));
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("12"), Some((12, Some(12))));
        assert_eq!(parse_range("360-445"), Some((360, Some(445))));
        assert_eq!(parse_range("360–445"), Some((360, Some(445))));
        assert_eq!(parse_range("7-"), Some((7, None)));
        assert_eq!(parse_range("9-3"), None);
        assert_eq!(parse_range("0"), None);
        assert_eq!(parse_range("#x"), None);
    }

    #[test]
    fn items_cover_rust_python_and_shell() {
        let rust = "use x;\npub(crate) async fn go() {}\nimpl<T> Foo for T {\n    fn inner(&self) {}\n}\npub const MAX: usize = 3;\n    let y = 2;\n// fn not\n";
        let (shape, entries) = outline("a.rs", rust);
        assert_eq!(shape, Shape::Items);
        let lines: Vec<usize> = entries.iter().map(|(n, _)| *n).collect();
        assert_eq!(lines, vec![2, 3, 4, 6]);
        let (_, py) = outline(
            "a.py",
            "import x\nclass A:\n    def b(self):\n        pass\n",
        );
        assert_eq!(py.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![2, 3]);
        let (_, sh) = outline("a.sh", "set -e\nbuild() {\n  echo\n}\n");
        assert_eq!(sh.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn plain_text_samples_every_nth_line() {
        let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let (shape, entries) = outline("notes.txt", &text);
        assert_eq!(shape, Shape::Every(20));
        assert_eq!(
            entries.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![1, 21, 41, 61, 81]
        );
    }

    #[test]
    fn pins_table() {
        let text = "# Facts\n\n## Pins\n\n| Alias | Repository | Commit |\n|---|---|---|\n| core | /tmp/core | `4eb92d29f89702cc66224a8708485783006e978e` |\n\n## IDEA-1\n\n| a | b | c |\n";
        let pins = parse_pins(text).unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].alias, "core");
        assert_eq!(pins[0].dir, PathBuf::from("/tmp/core"));
    }

    #[test]
    fn spec_shapes() {
        let pins = vec!["core=/tmp/core@4eb92d29f8".parse::<Pin>().unwrap()];
        let spec = parse_spec("core:src/a.rs:3-9", &pins).unwrap();
        assert_eq!(
            (spec.path.as_str(), spec.select),
            ("src/a.rs", Select::Lines(3, Some(9)))
        );
        let spec = parse_spec("core:docs/a.md:#Owner rulings: one", &pins).unwrap();
        assert_eq!(spec.select, Select::Heading("Owner rulings: one".into()));
        assert!(
            parse_spec("other:a.md", &pins)
                .unwrap_err()
                .contains("unknown alias")
        );
        assert!(
            parse_spec("/tmp/core@abcdef1:a.md", &pins)
                .unwrap_err()
                .contains("not pinned")
        );
        let open = parse_spec("/tmp/x@abcdef1:a.md", &[]).unwrap();
        assert_eq!(open.sha, "abcdef1");
    }
}
