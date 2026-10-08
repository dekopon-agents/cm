use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub rule: &'static str,
    pub message: String,
}

impl Violation {
    pub fn new(path: impl Into<String>, rule: &'static str, message: impl Into<String>) -> Self {
        Violation {
            path: path.into(),
            line: None,
            rule,
            message: message.into(),
        }
    }

    pub fn at(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{}: {}: {}", self.path, line, self.rule, self.message),
            None => write!(f, "{}: {}: {}", self.path, self.rule, self.message),
        }
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub violations: Vec<Violation>,
    pub fields: Map<String, Value>,
    pub lines: Vec<String>,
}

impl Outcome {
    pub fn refused(violations: Vec<Violation>) -> Self {
        Outcome {
            violations,
            ..Outcome::default()
        }
    }

    pub fn ok(&self) -> bool {
        self.violations.is_empty()
    }

    pub fn field(&mut self, key: &str, value: impl Into<Value>) {
        self.fields.insert(key.to_owned(), value.into());
    }

    pub fn line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    pub fn to_json(&self, command: &str) -> Value {
        let mut object = Map::new();
        object.insert("command".into(), command.into());
        object.insert("ok".into(), self.ok().into());
        object.insert(
            "violations".into(),
            serde_json::to_value(&self.violations).unwrap_or(Value::Null),
        );
        for (key, value) in &self.fields {
            object.insert(key.clone(), value.clone());
        }
        Value::Object(object)
    }
}
