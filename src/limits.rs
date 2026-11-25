use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::outcome::Violation;
use crate::state::Lane;

#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub paused: bool,
    pub runaway_ceiling_usd: f64,
    pub disk_volume: PathBuf,
    pub min_free_disk_gib: f64,
    pub kache_headroom_gib: f64,
    pub max_heavy_builds: u32,
    pub supervisor_model: String,
    pub supervisor_effort: String,
    pub codex_stall_hours: f64,
    /// Hours a merge in a repository with a release line may wait for a release before
    /// `cm next --act` lists it. Absent, nothing is listed.
    pub release_after_hours: Option<f64>,
    /// Hours a release with a rollout may wait for its deploy before `cm next --act` lists it.
    /// Absent, nothing is listed.
    pub deploy_after_hours: Option<f64>,
    stall_limits: StallLimits,
}

/// `codex_stall_hours` when LIMITS.toml leaves it out.
pub const CODEX_STALL_HOURS: f64 = 2.0;

/// Keys a campaign may leave out; each is checked when present.
const OPTIONAL: [(&str, Shape); 3] = [
    ("codex_stall_hours", Shape::Number),
    ("release_after_hours", Shape::Number),
    ("deploy_after_hours", Shape::Number),
];

/// LIMITS.toml, or `None` when it is missing or does not check.
pub(crate) fn read(root: &Path) -> Option<Limits> {
    std::fs::read_to_string(root.join("LIMITS.toml"))
        .ok()
        .and_then(|text| check(&text).ok())
}

/// The keys for other lanes were previously ignored by lint. Invalid values still default.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StallLimits {
    hours: std::collections::BTreeMap<String, f64>,
}

impl StallLimits {
    fn from_table(table: &Table) -> Self {
        let hours = crate::state::LANES
            .into_iter()
            .filter_map(|lane| {
                let name = lane.name();
                let key = format!("{}_stall_hours", name.replace('-', "_"));
                let value = table.get(&key)?;
                let hours = match value {
                    Value::Integer(hours) => *hours as f64,
                    Value::Float(hours) => *hours,
                    _ => return None,
                };
                (hours >= 0.0 && (hours.is_finite() || lane == Lane::Codex))
                    .then(|| (name.to_owned(), hours))
            })
            .collect();
        Self { hours }
    }

    pub(crate) fn read(root: &Path) -> Self {
        std::fs::read_to_string(root.join("LIMITS.toml"))
            .ok()
            .and_then(|text| check(&text).ok())
            .map(|limits| limits.stall_limits)
            .unwrap_or_default()
    }

    pub(crate) fn hours(&self, lane: Option<Lane>) -> f64 {
        lane.and_then(|lane| self.hours.get(lane.name()).copied())
            .unwrap_or(CODEX_STALL_HOURS)
    }
}

pub(crate) struct TurnProgram(Vec<String>);

impl TurnProgram {
    fn parse(table: &Table, key: &str) -> Result<Self, String> {
        let missing = || format!("LIMITS.toml is missing `{key}` for the remote turn command pair");
        let invalid = || {
            format!("LIMITS.toml `{key}` must be a nonempty argv array with a literal executable")
        };
        let args = table
            .get(key)
            .ok_or_else(missing)?
            .as_array()
            .ok_or_else(invalid)?;
        let program = args.first().and_then(Value::as_str).ok_or_else(invalid)?;
        if program.trim().is_empty() || program.contains(['{', '}']) {
            return Err(invalid());
        }
        args.iter()
            .map(|arg| {
                arg.as_str()
                    .filter(|arg| !arg.chars().any(char::is_control))
                    .map(str::to_owned)
                    .ok_or_else(invalid)
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    pub(crate) fn argv(&self, thread: &str, turn: &str) -> Vec<String> {
        self.0
            .iter()
            .map(|arg| {
                // Substitute the template once so braces in an identifier stay literal.
                let mut expanded = String::new();
                let mut tail = arg.as_str();
                while let Some((prefix, rest)) = tail.split_once('{') {
                    expanded.push_str(prefix);
                    if let Some(rest) = rest.strip_prefix("thread}") {
                        expanded.push_str(thread);
                        tail = rest;
                    } else if let Some(rest) = rest.strip_prefix("turn}") {
                        expanded.push_str(turn);
                        tail = rest;
                    } else {
                        expanded.push('{');
                        tail = rest;
                    }
                }
                expanded.push_str(tail);
                if let Some(relative) = expanded.strip_prefix("~/")
                    && let Some(home) = std::env::var_os("HOME")
                {
                    return PathBuf::from(home)
                        .join(relative)
                        .to_string_lossy()
                        .into_owned();
                }
                expanded
            })
            .collect()
    }
}

pub(crate) struct TurnPair {
    pub reader: TurnProgram,
    pub logs: TurnProgram,
}

impl TurnPair {
    fn read(table: &Table, lane: Lane) -> Result<Option<Self>, Vec<String>> {
        let prefix = lane.name().replace('-', "_");
        let reader_key = format!("{prefix}_turn_reader");
        let logs_key = format!("{prefix}_turn_logs");
        if !table.contains_key(&reader_key) && !table.contains_key(&logs_key) {
            return Ok(None);
        }
        match (
            TurnProgram::parse(table, &reader_key),
            TurnProgram::parse(table, &logs_key),
        ) {
            (Ok(reader), Ok(logs)) => Ok(Some(Self { reader, logs })),
            (reader, logs) => Err(reader.err().into_iter().chain(logs.err()).collect()),
        }
    }
}

const TURN_READER_LANES: [Lane; 2] = [Lane::Codex, Lane::ClaudeCloud];

type TurnSetup = Result<Option<TurnPair>, Vec<String>>;

pub(crate) struct TurnCommands(Result<[TurnSetup; TURN_READER_LANES.len()], String>);

impl TurnCommands {
    pub(crate) fn read(root: &Path) -> Self {
        let table = match std::fs::read_to_string(root.join("LIMITS.toml")) {
            Ok(text) => text.parse().map_err(|error: toml::de::Error| {
                format!("cannot parse LIMITS.toml: {}", error.message())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Table::new()),
            Err(error) => Err(format!("cannot read LIMITS.toml: {error}")),
        };
        Self(table.map(|table| TURN_READER_LANES.map(|lane| TurnPair::read(&table, lane))))
    }

    pub(crate) fn for_lane(&self, lane: Lane) -> Option<&TurnPair> {
        let index = TURN_READER_LANES.iter().position(|known| *known == lane)?;
        self.0.as_ref().ok()?[index].as_ref().ok()?.as_ref()
    }

    pub(crate) fn errors(&self) -> impl Iterator<Item = &String> {
        self.0.as_ref().err().into_iter().chain(
            self.0
                .iter()
                .flatten()
                .filter_map(|configured| configured.as_ref().err())
                .flatten(),
        )
    }
}

pub(crate) fn codex_pause() -> Option<String> {
    let used = std::env::var("CM_CODEX_WEEKLY_PCT")
        .ok()?
        .parse::<f64>()
        .ok()?;
    (85.0..=100.0)
        .contains(&used)
        .then(|| format!("Codex dispatch held: weekly usage {used}% reached the 85% pause"))
}

pub(crate) fn codex_pause_violation(target: &str, reason: &str) -> Violation {
    Violation::new(target, "codex-paused", reason)
}

#[derive(Clone, Copy)]
enum Shape {
    Bool,
    Number,
    Count,
    Text,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Bool => "a boolean",
            Shape::Number => "a number",
            Shape::Count => "a non-negative integer",
            Shape::Text => "a non-empty string",
        }
    }

    fn fits(self, value: &Value) -> bool {
        match (self, value) {
            (Shape::Bool, Value::Boolean(_)) => true,
            (Shape::Number, Value::Integer(number)) => *number >= 0,
            (Shape::Number, Value::Float(number)) => *number >= 0.0,
            (Shape::Count, Value::Integer(number)) => *number >= 0,
            (Shape::Text, Value::String(text)) => !text.trim().is_empty(),
            _ => false,
        }
    }
}

const KEYS: [(&str, Shape); 13] = [
    ("paused", Shape::Bool),
    ("runaway_ceiling_usd", Shape::Number),
    ("driver_model", Shape::Text),
    ("driver_effort", Shape::Text),
    ("verifier_model", Shape::Text),
    ("supervisor_model", Shape::Text),
    ("supervisor_effort", Shape::Text),
    ("disk_volume", Shape::Text),
    ("min_free_disk_gib", Shape::Number),
    ("kache_headroom_gib", Shape::Number),
    ("max_heavy_builds", Shape::Count),
    ("pr_ci_expected_min", Shape::Number),
    ("pr_ci_deadline_min", Shape::Number),
];

fn line_of(text: &str, key: &str) -> Option<usize> {
    text.lines()
        .position(|line| {
            let line = line.trim_start();
            line.strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .map(|index| index + 1)
}

pub fn check(text: &str) -> Result<Limits, Vec<Violation>> {
    let table: Table = match text.parse() {
        Ok(table) => table,
        Err(error) => {
            let line = error
                .span()
                .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
            let mut violation =
                Violation::new("LIMITS.toml", "limits-parse", error.message().to_owned());
            violation.line = line;
            return Err(vec![violation]);
        }
    };
    let mut out = Vec::new();
    for (key, shape) in KEYS {
        match table.get(key) {
            None => out.push(Violation::new(
                "LIMITS.toml",
                "limits-key",
                format!("missing `{key}` ({})", shape.name()),
            )),
            Some(value) if !shape.fits(value) => {
                let mut violation = Violation::new(
                    "LIMITS.toml",
                    "limits-key",
                    format!("`{key}` must be {}", shape.name()),
                );
                violation.line = line_of(text, key);
                out.push(violation);
            }
            Some(_) => {}
        }
    }
    for (key, shape) in OPTIONAL {
        if let Some(value) = table.get(key)
            && !shape.fits(value)
        {
            let mut violation = Violation::new(
                "LIMITS.toml",
                "limits-key",
                format!("`{key}` must be {}", shape.name()),
            );
            violation.line = line_of(text, key);
            out.push(violation);
        }
    }
    if !out.is_empty() {
        return Err(out);
    }
    let number = |key: &str| match &table[key] {
        Value::Integer(number) => *number as f64,
        Value::Float(number) => *number,
        _ => 0.0,
    };
    let text_of = |key: &str| table[key].as_str().unwrap_or_default().to_owned();
    Ok(Limits {
        paused: table["paused"].as_bool().unwrap_or(true),
        runaway_ceiling_usd: number("runaway_ceiling_usd"),
        disk_volume: PathBuf::from(text_of("disk_volume")),
        min_free_disk_gib: number("min_free_disk_gib"),
        kache_headroom_gib: number("kache_headroom_gib"),
        max_heavy_builds: table["max_heavy_builds"]
            .as_integer()
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(0),
        supervisor_model: text_of("supervisor_model"),
        supervisor_effort: text_of("supervisor_effort"),
        stall_limits: StallLimits::from_table(&table),
        codex_stall_hours: if table.contains_key("codex_stall_hours") {
            number("codex_stall_hours")
        } else {
            CODEX_STALL_HOURS
        },
        release_after_hours: table
            .contains_key("release_after_hours")
            .then(|| number("release_after_hours")),
        deploy_after_hours: table
            .contains_key("deploy_after_hours")
            .then(|| number("deploy_after_hours")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_every_missing_or_mistyped_key() {
        let violations = check("paused = \"no\"\nmax_heavy_builds = -1\n").unwrap_err();
        assert_eq!(violations.len(), 13, "{violations:?}");
        assert_eq!(violations[0].line, Some(1));
        assert!(
            violations
                .iter()
                .any(|v| v.message.contains("missing `disk_volume`"))
        );
    }

    #[test]
    fn parse_error_has_a_line() {
        let violations = check("paused = false\nnope =\n").unwrap_err();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "limits-parse");
        assert_eq!(violations[0].line, Some(2));
    }

    #[test]
    fn codex_stall_hours_is_optional_and_checked() {
        let base = include_str!("../templates/LIMITS.toml");
        assert_eq!(check(base).unwrap().codex_stall_hours, 2.0);
        let without: String = base
            .lines()
            .filter(|line| !line.starts_with("codex_stall_hours"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(
            check(&without).unwrap().codex_stall_hours,
            CODEX_STALL_HOURS
        );
        let set = format!("{without}codex_stall_hours = 0.5\n");
        assert_eq!(check(&set).unwrap().codex_stall_hours, 0.5);
        let bad = check(&format!("{without}codex_stall_hours = \"two\"\n")).unwrap_err();
        assert_eq!(bad[0].rule, "limits-key");
        assert!(bad[0].message.contains("codex_stall_hours"), "{bad:?}");
    }

    #[test]
    fn release_and_deploy_dials_are_optional_checked_and_tolerate_unknown_keys() {
        let base = include_str!("../templates/LIMITS.toml");
        assert_eq!(check(base).unwrap().release_after_hours, None);
        assert_eq!(check(base).unwrap().deploy_after_hours, None);
        let set = format!(
            "{base}release_after_hours = 4\ndeploy_after_hours = 2.5\nsome_future_dial = 1\n"
        );
        assert_eq!(check(&set).unwrap().release_after_hours, Some(4.0));
        assert_eq!(check(&set).unwrap().deploy_after_hours, Some(2.5));
        let bad = check(&format!("{base}release_after_hours = -1\n")).unwrap_err();
        assert_eq!(bad[0].rule, "limits-key");
        assert!(bad[0].message.contains("release_after_hours"), "{bad:?}");
    }
}
