use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::ValueEnum;

use crate::epoch::{self, EPOCH, Status};
use crate::host::{Host, stamp};
use crate::journal;
use crate::markdown::{Fences, headings};
use crate::outcome::{Outcome, Violation};
use crate::store::{self, JOURNAL, lock, relative, write_atomic};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub(crate) enum Maturity {
    Captured,
    Exploring,
    Designed,
}

impl Maturity {
    fn label(self) -> &'static str {
        match self {
            Self::Captured => "captured",
            Self::Exploring => "exploring",
            Self::Designed => "designed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Captured, Self::Exploring, Self::Designed]
            .into_iter()
            .find(|value| value.label() == text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Word {
    Landed,
    Partial,
    Deferred,
    Superseded,
    Cancelled,
}

impl Word {
    fn label(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::Partial => "partial",
            Self::Deferred => "deferred",
            Self::Superseded => "superseded",
            Self::Cancelled => "cancelled",
        }
    }

    fn terminal(self) -> bool {
        matches!(self, Self::Landed | Self::Superseded | Self::Cancelled)
    }
}

pub(crate) enum Action<'a> {
    Admit,
    Move(Maturity),
    Outcome {
        word: Word,
        to: Option<&'a str>,
        by: Option<&'a str>,
        note: Option<&'a str>,
    },
}

fn field(text: &str, key: &str) -> Option<String> {
    let lines: Vec<_> = text.lines().map(str::to_owned).collect();
    let front = epoch::front(&lines)?;
    epoch::value(&lines, &front, key).map(|value| value.trim_matches(['\'', '"']).to_owned())
}

fn id_valid(id: &str) -> bool {
    id.rsplit_once('-').is_some_and(|(prefix, number)| {
        !prefix.is_empty()
            && prefix.bytes().all(|byte| byte.is_ascii_alphabetic())
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn epoch_valid(name: &str) -> bool {
    store::is_name(name)
}

struct Manifest {
    text: String,
    name: String,
    status: Status,
    dir: PathBuf,
    ids: Vec<String>,
}

fn read_manifest(root: &Path, out: &mut Vec<Violation>) -> Option<String> {
    match crate::ledger::read_existing(&root.join(EPOCH)) {
        Ok(text) => Some(text.unwrap_or_default()),
        Err(error) => {
            out.push(Violation::new(EPOCH, "idea-manifest", format!("{error:#}")));
            None
        }
    }
}

/// Existing campaigns without ideas_dir have no idea lint rules.
fn manifest(
    root: &Path,
    text: String,
    required: bool,
    out: &mut Vec<Violation>,
) -> Option<Manifest> {
    let Some(dir) = field(&text, "ideas_dir").filter(|value| !value.is_empty()) else {
        if required {
            out.push(Violation::new(
                EPOCH,
                "idea-dir",
                "set `ideas_dir: <relative path>` in EPOCH.md",
            ));
        }
        return None;
    };
    if Path::new(&dir).is_absolute() {
        out.push(Violation::new(
            EPOCH,
            "idea-dir",
            "ideas_dir is relative to the campaign folder",
        ));
        return None;
    }
    let name = field(&text, "epoch").unwrap_or_default();
    if !epoch_valid(&name) {
        out.push(Violation::new(
            EPOCH,
            "idea-epoch",
            "epoch: names this epoch with letters, digits, `.`, `_` and `-`",
        ));
        return None;
    }
    let Some(status) = epoch::status(&text) else {
        out.push(Violation::new(
            EPOCH,
            "epoch-status",
            "status: is OPEN, PLANNING, WORKING or CLOSED",
        ));
        return None;
    };
    let ids = match field(&text, "ideas") {
        None => Vec::new(),
        Some(list) => {
            let Some(inner) = list
                .strip_prefix('[')
                .and_then(|list| list.strip_suffix(']'))
            else {
                out.push(Violation::new(
                    EPOCH,
                    "idea-list-form",
                    "ideas: is a flow list, for example [IDEA-9003, IDEA-9001]",
                ));
                return None;
            };
            let ids: Vec<String> = if inner.trim().is_empty() {
                Vec::new()
            } else {
                inner.split(',').map(|id| id.trim().to_owned()).collect()
            };
            if ids.iter().any(|id| !id_valid(id)) {
                out.push(Violation::new(
                    EPOCH,
                    "idea-list-form",
                    "ideas: holds comma-separated idea ids",
                ));
                return None;
            }
            ids
        }
    };
    Some(Manifest {
        text,
        name,
        status,
        dir: root.join(dir),
        ids,
    })
}

struct Idea {
    path: PathBuf,
    text: String,
    id: Option<String>,
    maturity: Option<String>,
    current: Option<String>,
}

fn idea_file(path: &Path) -> bool {
    if path.extension().is_none_or(|ext| ext != "md") {
        return false;
    }
    let Some((prefix, suffix)) = path
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split_once('-'))
    else {
        return false;
    };
    let number = suffix.split('-').next().unwrap_or_default();
    id_valid(&format!("{prefix}-{number}"))
}

fn read_ideas(root: &Path, dir: &Path, out: &mut Vec<Violation>) -> Vec<Idea> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            out.push(Violation::new(
                EPOCH,
                "idea-dir",
                format!("cannot read ideas_dir {}: {error}", dir.display()),
            ));
            return Vec::new();
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() && idea_file(&path) {
                    paths.push(path);
                }
            }
            Err(error) => out.push(Violation::new(
                EPOCH,
                "idea-dir",
                format!("cannot list ideas_dir: {error}"),
            )),
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| match fs::read_to_string(&path) {
            Ok(text) => Some(Idea {
                id: field(&text, "id"),
                maturity: field(&text, "maturity"),
                current: field(&text, "current_epoch"),
                path,
                text,
            }),
            Err(error) => {
                out.push(Violation::new(
                    relative(root, &path),
                    "idea-front-matter",
                    format!("cannot read idea: {error}"),
                ));
                None
            }
        })
        .collect()
}

fn validate(root: &Path, idea: &Idea) -> Vec<Violation> {
    let path = relative(root, &idea.path);
    let mut out = Vec::new();
    if idea.id.as_deref().is_none_or(|id| !id_valid(id)) || idea.maturity.is_none() {
        out.push(Violation::new(
            &path,
            "idea-front-matter",
            "front matter needs id: <PREFIX>-<digits> and maturity:",
        ));
    }
    if let Some(maturity) = &idea.maturity
        && Maturity::parse(maturity).is_none()
    {
        out.push(Violation::new(
            &path,
            "idea-maturity",
            "maturity: is captured, exploring or designed",
        ));
    }
    if let Some(id) = &idea.id {
        let filename = idea.path.file_name().unwrap_or_default().to_string_lossy();
        if filename != format!("{id}.md") && !filename.starts_with(&format!("{id}-")) {
            out.push(Violation::new(
                &path,
                "idea-filename",
                format!("file name starts with {id}.md or {id}-"),
            ));
        }
    }
    out
}

fn resolve<'a>(
    root: &Path,
    ideas: &'a [Idea],
    id: &str,
    out: &mut Vec<Violation>,
) -> Option<&'a Idea> {
    let found: Vec<_> = ideas
        .iter()
        .filter(|idea| idea.id.as_deref() == Some(id))
        .collect();
    match found.as_slice() {
        [] => {
            out.push(Violation::new(
                EPOCH,
                "idea-unknown",
                format!("{id} has no file in ideas_dir"),
            ));
            None
        }
        [one] => Some(one),
        _ => {
            for idea in &found {
                let other = found.iter().find(|other| other.path != idea.path).unwrap();
                out.push(Violation::new(
                    relative(root, &idea.path),
                    "idea-duplicate-id",
                    format!("{id} also appears in {}", relative(root, &other.path)),
                ));
            }
            None
        }
    }
}

fn offset(text: &str, line: usize) -> usize {
    text.split_inclusive('\n').take(line).map(str::len).sum()
}

fn history(text: &str) -> Option<Range<usize>> {
    let headings = headings(text);
    let mut sections = headings
        .iter()
        .enumerate()
        .filter(|(_, heading)| heading.level == 2 && heading.text == "Epoch history");
    let (index, heading) = sections.next()?;
    if sections.next().is_some() {
        return None;
    }
    let end = headings[index + 1..]
        .iter()
        .find(|heading| heading.level <= 2)
        .map(|heading| offset(text, heading.line - 1))
        .unwrap_or(text.len());
    Some(offset(text, heading.line)..end)
}

fn entry_text(line: &str) -> Option<&str> {
    let (stamp, text) = line.strip_prefix("- ")?.split_once(": ")?;
    let valid = if stamp.len() == 10 {
        stamp.parse::<jiff::civil::Date>().is_ok()
    } else {
        stamp.parse::<jiff::Timestamp>().is_ok()
    };
    valid.then_some(text)
}

fn destination(text: &str) -> bool {
    text == "unassigned"
        || text
            .strip_prefix('`')
            .and_then(|text| text.strip_suffix('`'))
            .is_some_and(epoch_valid)
}

struct RecordedOutcome<'a> {
    epoch: &'a str,
    word: Word,
    target: Option<&'a str>,
}

fn outcome_text(text: &str) -> Option<RecordedOutcome<'_>> {
    let (name, text) = text.strip_prefix("outcome in `")?.split_once("`: ")?;
    let text = text.strip_suffix('.')?;
    let (word, target) = if text
        .strip_prefix("landed; ")
        .is_some_and(|note| !note.trim().is_empty())
    {
        (Word::Landed, None)
    } else if text
        .strip_prefix("cancelled; ")
        .is_some_and(|note| !note.trim().is_empty())
    {
        (Word::Cancelled, None)
    } else if let Some(by) = text
        .strip_prefix("superseded by ")
        .filter(|by| id_valid(by))
    {
        (Word::Superseded, Some(by))
    } else if let Some(to) = text
        .strip_prefix("deferred to ")
        .filter(|to| destination(to))
    {
        (Word::Deferred, Some(to.trim_matches('`')))
    } else if let Some((to, _)) = text
        .strip_prefix("partial, remainder to ")
        .and_then(|text| text.split_once("; "))
        .filter(|(target, note)| destination(target) && !note.trim().is_empty())
    {
        (Word::Partial, Some(to.trim_matches('`')))
    } else {
        return None;
    };
    epoch_valid(name).then_some(RecordedOutcome {
        epoch: name,
        word,
        target,
    })
}

impl Idea {
    fn entries(&self) -> Vec<&str> {
        let Some(range) = history(&self.text) else {
            return Vec::new();
        };
        let mut fences = Fences::default();
        self.text[range]
            .lines()
            .filter(|line| !fences.contains(line))
            .filter_map(entry_text)
            .collect()
    }

    fn outcomes(&self) -> Vec<(&str, Word)> {
        self.entries()
            .into_iter()
            .filter_map(outcome_text)
            .map(|outcome| (outcome.epoch, outcome.word))
            .collect()
    }

    fn admitted(&self, name: &str) -> bool {
        [
            Status::Open,
            Status::Planning,
            Status::Working,
            Status::Closed,
        ]
        .iter()
        .any(|status| {
            let text = format!("admitted to `{name}` ({}).", status.label());
            self.entries().contains(&text.as_str())
        })
    }

    fn open(&self, name: &str) -> bool {
        self.current.as_deref() == Some(name)
            && !self.outcomes().iter().any(|(epoch, _)| *epoch == name)
    }
}

/// Only cm's exact history format is state; dated free prose remains untouched.
pub(crate) fn lint(root: &Path) -> Vec<Violation> {
    let mut out = Vec::new();
    let Some(text) = read_manifest(root, &mut out) else {
        return out;
    };
    let Some(manifest) = manifest(root, text, false, &mut out) else {
        return out;
    };
    let ideas = read_ideas(root, &manifest.dir, &mut out);
    for idea in &ideas {
        out.extend(validate(root, idea));
        if let Some(id) = &idea.id {
            if let Some(other) = ideas
                .iter()
                .find(|other| other.path != idea.path && other.id == idea.id)
            {
                out.push(Violation::new(
                    relative(root, &idea.path),
                    "idea-duplicate-id",
                    format!("{id} also appears in {}", relative(root, &other.path)),
                ));
            }
            if idea.open(&manifest.name) && !manifest.ids.contains(id) {
                out.push(Violation::new(
                    relative(root, &idea.path),
                    "idea-epoch-list",
                    format!("{id} is open here but missing from ideas:; run `cm idea admit {id}`"),
                ));
            }
        }
    }
    for id in &manifest.ids {
        let found: Vec<_> = ideas
            .iter()
            .filter(|idea| idea.id.as_deref() == Some(id))
            .collect();
        let belongs = |idea: &&Idea| {
            if manifest.status == Status::Closed {
                idea.outcomes()
                    .iter()
                    .any(|(name, _)| *name == manifest.name)
            } else {
                idea.current.as_deref() == Some(&manifest.name)
            }
        };
        if found.is_empty() || !found.iter().all(belongs) {
            let message = if manifest.status == Status::Closed {
                format!(
                    "{id} has no file or no recorded outcome in `{}`",
                    manifest.name
                )
            } else {
                format!("{id} has no file or names another current_epoch; run `cm idea admit {id}`")
            };
            out.push(Violation::new(EPOCH, "idea-epoch-list", message));
        }
    }
    out
}

pub(crate) fn accounting(root: &Path, rule: &'static str, out: &mut Vec<Violation>) -> String {
    let Some(text) = read_manifest(root, out) else {
        return String::new();
    };
    if field(&text, "ideas_dir").is_none() || field(&text, "ideas").is_none() {
        return String::new();
    }
    let Some(manifest) = manifest(root, text, true, out) else {
        return String::new();
    };
    let ideas = read_ideas(root, &manifest.dir, out);
    let mut body = String::new();
    for id in &manifest.ids {
        let outcome = resolve(root, &ideas, id, out).and_then(|idea| {
            idea.entries()
                .into_iter()
                .filter_map(outcome_text)
                .find(|outcome| outcome.epoch == manifest.name)
        });
        match outcome {
            Some(outcome) => {
                body.push_str(&format!("- idea: {id} {}", outcome.word.label()));
                if let Some(target) = outcome.target {
                    let link = if outcome.word == Word::Superseded {
                        "by"
                    } else {
                        "to"
                    };
                    body.push_str(&format!(" {link} {target}"));
                }
                body.push('\n');
            }
            None => out.push(Violation::new(
                EPOCH,
                rule,
                format!(
                    "{id} has no outcome in `{}`; run `cm idea outcome {id} <word>`",
                    manifest.name
                ),
            )),
        }
    }
    body
}

fn append_history(text: &str, line: &str) -> String {
    let range = history(text).expect("history checked before writing");
    let section = &text[range.clone()];
    // Insert before trailing blank lines, keeping every existing byte in place.
    let mut at = range.start;
    let mut end = range.start;
    for line in section.split_inclusive('\n') {
        end += line.len();
        if !line.trim().is_empty() {
            at = end;
        }
    }
    let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut next = text[..at].to_owned();
    if !next.ends_with('\n') {
        next.push_str(ending);
    }
    next.push_str(line);
    next.push_str(ending);
    next.push_str(&text[at..]);
    next
}

fn journaled(root: &Path, title: &str, body: &str) -> Result<bool> {
    let text = fs::read_to_string(root.join(JOURNAL))?;
    let suffix = format!(" — {title}");
    let body = body.trim();
    let mut start = None;
    let mut at = 0;
    // Journal boundaries are raw stamped lines, even inside unfinished Markdown fences.
    for raw in text.split_inclusive('\n') {
        let line = raw
            .strip_suffix('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .unwrap_or(raw);
        if journal::is_stamped(line) {
            if start.is_some_and(|start| text[start..at].trim() == body) {
                return Ok(true);
            }
            start = line.ends_with(&suffix).then_some(at + raw.len());
        }
        at += raw.len();
    }
    Ok(start.is_some_and(|start| text[start..].trim() == body))
}

pub(crate) fn run(host: &dyn Host, root: &Path, id: &str, action: Action<'_>) -> Result<Outcome> {
    let _lock = lock(root)?;
    let campaign = store::load_campaign(root)?;
    let mut violations = journal::check(root, &campaign);
    let Some(text) = read_manifest(root, &mut violations) else {
        return Ok(Outcome::refused(violations));
    };
    let Some(mut manifest) = manifest(root, text, true, &mut violations) else {
        return Ok(Outcome::refused(violations));
    };
    let ideas = read_ideas(root, &manifest.dir, &mut violations);
    let Some(idea) = resolve(root, &ideas, id, &mut violations) else {
        return Ok(Outcome::refused(violations));
    };
    violations.extend(validate(root, idea));
    let path = relative(root, &idea.path);
    if history(&idea.text)
        .is_none_or(|range| crate::markdown::unterminated_fence(&idea.text[range]))
    {
        violations.push(Violation::new(
            &path,
            "idea-history",
            "the idea needs exactly one `## Epoch history` section with closed Markdown fences",
        ));
    }
    let at = stamp(host.now());
    let date = &at[..10];
    let mut next = idea.text.clone();
    let mut result = Outcome::default();
    result.field("id", id);
    result.field("epoch", manifest.name.clone());
    result.field("at", at.clone());
    let recorded_here = idea
        .outcomes()
        .iter()
        .any(|(name, _)| *name == manifest.name);
    let (act, record, body) = match action {
        Action::Admit => {
            violations.extend(crate::commands::closed(root, &campaign));
            if manifest.status == Status::Closed {
                violations.push(Violation::new(
                    EPOCH,
                    "epoch-closed",
                    "an idea cannot be admitted to a CLOSED epoch",
                ));
            }
            let outcomes = idea.outcomes();
            if outcomes.last().is_some_and(|(_, word)| word.terminal()) {
                violations.push(Violation::new(
                    &path,
                    "idea-terminal",
                    "the last outcome is terminal",
                ));
            }
            if let Some(current) = idea.current.as_deref().filter(|current| {
                !current.is_empty()
                    && *current != "unassigned"
                    && *current != "null"
                    && *current != "~"
                    && *current != manifest.name
            }) && !outcomes.iter().any(|(name, _)| *name == current)
            {
                violations.push(Violation::new(
                    &path,
                    "idea-open-elsewhere",
                    format!("the idea is still open in `{current}`"),
                ));
            }
            let record = (!idea.admitted(&manifest.name)).then(|| {
                format!(
                    "admitted to `{}` ({}).",
                    manifest.name,
                    manifest.status.label()
                )
            });
            if violations.is_empty() {
                next = epoch::set_text(&next, "current_epoch", &manifest.name)
                    .expect("validated front matter");
            }
            (
                "admit",
                record,
                format!("- idea: {id}\n- epoch: {}\n", manifest.name),
            )
        }
        Action::Move(to) => {
            let from = idea.maturity.as_deref().and_then(Maturity::parse);
            if from.is_some_and(|from| to <= from) {
                violations.push(Violation::new(
                    &path,
                    "idea-maturity",
                    "maturity moves forward only: captured → exploring → designed",
                ));
            }
            if violations.is_empty() {
                next =
                    epoch::set_text(&next, "maturity", to.label()).expect("validated front matter");
            }
            result.field("maturity", to.label());
            let from = from.map(Maturity::label).unwrap_or_default();
            let record = format!("maturity {from} → {} in `{}`.", to.label(), manifest.name);
            (
                "move",
                Some(record),
                format!(
                    "- idea: {id}\n- epoch: {}\n- maturity: {from} → {}\n",
                    manifest.name,
                    to.label()
                ),
            )
        }
        Action::Outcome { word, to, by, note } => {
            if recorded_here {
                violations.push(Violation::new(
                    &path,
                    "idea-outcome-twice",
                    "this idea already has an outcome in this epoch",
                ));
            }
            let needs_to = matches!(word, Word::Partial | Word::Deferred);
            let needs_note = matches!(word, Word::Landed | Word::Partial | Word::Cancelled);
            let needs_by = word == Word::Superseded;
            if needs_to != to.is_some() || needs_note != note.is_some() || needs_by != by.is_some()
            {
                violations.push(Violation::new(&path, "idea-outcome-flags", "landed/cancelled need only --note; partial needs --to and --note; deferred needs only --to; superseded needs only --by"));
            }
            if needs_to
                && let Some(target) = to
                && target != "unassigned"
            {
                violations.push(Violation::new(
                    &path,
                    "idea-outcome-flags",
                    "--to must be unassigned; a later epoch admits the idea itself with `cm idea admit`",
                ));
            }
            if let Some(note) = note
                && (note.trim().is_empty() || note.contains(['\r', '\n']))
            {
                violations.push(Violation::new(
                    &path,
                    "idea-outcome-flags",
                    "--note is one non-empty line",
                ));
            }
            if let Some(by) = by {
                if !id_valid(by) {
                    violations.push(Violation::new(
                        &path,
                        "idea-outcome-flags",
                        "--by is an idea id: <PREFIX>-<digits>",
                    ));
                } else {
                    resolve(root, &ideas, by, &mut violations);
                }
            }
            if needs_to && violations.is_empty() {
                next = epoch::set_text(&next, "current_epoch", "null")
                    .expect("validated front matter");
            }
            let target = to.unwrap_or_default();
            let details = match word {
                Word::Landed => format!("landed; {}", note.unwrap_or_default()),
                Word::Partial => format!(
                    "partial, remainder to {target}; {}",
                    note.unwrap_or_default()
                ),
                Word::Deferred => format!("deferred to {target}"),
                Word::Superseded => format!("superseded by {}", by.unwrap_or_default()),
                Word::Cancelled => format!("cancelled; {}", note.unwrap_or_default()),
            };
            let record = format!("outcome in `{}`: {details}.", manifest.name);
            result.field("outcome", word.label());
            result.field("to", to);
            result.field("by", by);
            result.field("note", note);
            (
                "outcome",
                Some(record),
                format!(
                    "- idea: {id}\n- epoch: {}\n- outcome: {details}\n",
                    manifest.name
                ),
            )
        }
    };
    if act != "admit"
        && idea.current.as_deref() != Some(&manifest.name)
        && !(act == "outcome" && recorded_here)
    {
        violations.push(Violation::new(
            &path,
            "idea-not-here",
            "current_epoch must name this epoch; run `cm idea admit` first",
        ));
    }
    if !violations.is_empty() {
        return Ok(Outcome::refused(violations));
    }
    if let Some(record) = record {
        next = append_history(&next, &format!("- {date}: {record}"));
    }
    let mut changed = next != idea.text;
    if changed {
        write_atomic(&idea.path, next.as_bytes())?;
    }
    if act == "admit" && !manifest.ids.iter().any(|listed| listed == id) {
        manifest.ids.push(id.to_owned());
        let list = format!("[{}]", manifest.ids.join(", "));
        let text = epoch::set_text(&manifest.text, "ideas", &list).expect("validated front matter");
        write_atomic(&root.join(EPOCH), text.as_bytes())?;
        changed = true;
    }
    let title = format!("idea {act} {id}");
    if act != "admit" || !journaled(root, &title, &body)? {
        let heading = crate::ledger::journal_line(root, &at, &title, &body)?;
        result.field("journal", heading);
        changed = true;
    }
    result.field("action", act);
    result.field("changed", changed);
    result.line(if changed {
        format!("idea {act} {id}")
    } else {
        format!("{id} is already admitted")
    });
    Ok(result)
}
