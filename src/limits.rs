use std::path::PathBuf;

use toml::{Table, Value};

use crate::outcome::Violation;

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
}
