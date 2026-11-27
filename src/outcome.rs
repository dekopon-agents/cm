use std::path::Path;

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Map, Value, json};

use crate::commands::shell_word;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub line: Option<usize>,
    pub rule: &'static str,
    pub message: String,
    pub next: Next,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Run(Vec<Move>),
    // `recorded` is boxed to keep `Violation` small enough for the `Result<_, Violation>` returns.
    Missing {
        what: String,
        recorded: Option<Box<str>>,
    },
    Settled(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub when: Option<String>,
    pub argv: Vec<Arg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Lit(String),
    Input(Input),
}

// No variant may name a value cm can read itself (a step, unit, item number, claim, head, PR,
// tag or path): those are written in as `Arg::Lit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Title,
    Reason,
    Context,
    Question,
    Recommendation,
    BodyFile,
    EvidenceFile,
    KickoffFile,
    Receipt,
    Note,
    Lane,
    Checkout,
    Signal,
    Deadline,
    Revision,
}

pub const INPUTS: [Input; 15] = [
    Input::Title,
    Input::Reason,
    Input::Context,
    Input::Question,
    Input::Recommendation,
    Input::BodyFile,
    Input::EvidenceFile,
    Input::KickoffFile,
    Input::Receipt,
    Input::Note,
    Input::Lane,
    Input::Checkout,
    Input::Signal,
    Input::Deadline,
    Input::Revision,
];

impl Input {
    pub fn name(self) -> &'static str {
        match self {
            Input::Title => "title",
            Input::Reason => "reason",
            Input::Context => "context",
            Input::Question => "question",
            Input::Recommendation => "recommendation",
            Input::BodyFile => "body-file",
            Input::EvidenceFile => "evidence-file",
            Input::KickoffFile => "kickoff-file",
            Input::Receipt => "receipt",
            Input::Note => "note",
            Input::Lane => "lane",
            Input::Checkout => "checkout",
            Input::Signal => "signal",
            Input::Deadline => "deadline",
            Input::Revision => "revision",
        }
    }

    pub fn placeholder(self) -> String {
        format!("<{}>", self.name())
    }

    // What `cli::parses` puts in the input's place: a value every flag that takes it accepts.
    pub fn sample(self) -> &'static str {
        match self {
            Input::Title => "A title",
            Input::Reason => "a reason",
            Input::Context => "context",
            Input::Question => "a question?",
            Input::Recommendation => "a recommendation",
            Input::BodyFile => "body.md",
            Input::EvidenceFile => "evidence.md",
            Input::KickoffFile => "kickoff.md",
            Input::Receipt => "a receipt",
            Input::Note => "a note",
            Input::Lane => crate::state::Lane::ClaudeCloud.name(),
            Input::Checkout => ".",
            Input::Signal => "a signal",
            Input::Deadline => "2026-01-01T00:00Z",
            Input::Revision => "0000000",
        }
    }
}

impl From<&str> for Arg {
    fn from(text: &str) -> Self {
        Arg::Lit(text.to_owned())
    }
}

impl From<String> for Arg {
    fn from(text: String) -> Self {
        Arg::Lit(text)
    }
}

impl From<&String> for Arg {
    fn from(text: &String) -> Self {
        Arg::Lit(text.clone())
    }
}

impl From<Input> for Arg {
    fn from(input: Input) -> Self {
        Arg::Input(input)
    }
}

impl Arg {
    fn shown(&self) -> String {
        match self {
            Arg::Lit(text) => text.clone(),
            Arg::Input(input) => input.placeholder(),
        }
    }
}

impl Move {
    pub fn new(argv: Vec<Arg>) -> Self {
        let next = Move { when: None, argv };
        next.check();
        next
    }

    pub fn again(argv: Vec<String>) -> Self {
        Move::new(argv.into_iter().map(Arg::Lit).collect())
    }

    pub fn set(mut self, flag: &str, input: Input) -> Self {
        let inline = format!("{flag}=");
        let at = self.argv.iter().position(|arg| match arg {
            Arg::Lit(text) => text == flag || text.starts_with(&inline),
            Arg::Input(_) => false,
        });
        match at {
            Some(at) if matches!(&self.argv[at], Arg::Lit(text) if text == flag) => {
                if at + 1 < self.argv.len() {
                    self.argv[at + 1] = Arg::Input(input);
                } else {
                    self.argv.push(Arg::Input(input));
                }
            }
            Some(at) => {
                self.argv[at] = Arg::Lit(flag.to_owned());
                self.argv.insert(at + 1, Arg::Input(input));
            }
            None => {
                self.argv.push(Arg::Lit(flag.to_owned()));
                self.argv.push(Arg::Input(input));
            }
        }
        self.check();
        self
    }

    fn check(&self) {
        debug_assert!(
            crate::cli::parses(&self.sampled()),
            "a move cm does not parse: {}",
            self.command()
        );
    }

    fn sampled(&self) -> Vec<String> {
        self.argv
            .iter()
            .map(|arg| match arg {
                Arg::Lit(text) => text.clone(),
                Arg::Input(input) => input.sample().to_owned(),
            })
            .collect()
    }

    pub fn words(&self) -> Vec<String> {
        self.argv.iter().map(Arg::shown).collect()
    }

    pub fn inputs(&self) -> Vec<&'static str> {
        self.argv
            .iter()
            .filter_map(|arg| match arg {
                Arg::Input(input) => Some(input.name()),
                Arg::Lit(_) => None,
            })
            .collect()
    }

    pub fn command(&self) -> String {
        command(&self.words())
    }
}

pub fn command(words: &[String]) -> String {
    words
        .iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

impl From<Move> for Next {
    fn from(only: Move) -> Self {
        Next::Run(vec![only])
    }
}

impl Next {
    pub fn run(argv: Vec<Arg>) -> Self {
        Move::new(argv).into()
    }

    pub fn either(moves: impl IntoIterator<Item = (String, Move)>) -> Self {
        let moves: Vec<Move> = moves
            .into_iter()
            .map(|(when, mut each)| {
                assert!(
                    !when.trim().is_empty(),
                    "every move of an either has a condition"
                );
                each.when = Some(when);
                each
            })
            .collect();
        assert!(moves.len() >= 2, "an either has at least two moves");
        Next::Run(moves)
    }

    pub fn missing(what: impl Into<String>) -> Self {
        Next::Missing {
            what: what.into(),
            recorded: None,
        }
    }

    pub fn settled(why: impl Into<String>) -> Self {
        Next::Settled(why.into())
    }
}

fn feedback_recipe(what: &str) -> Vec<String> {
    [
        "cm",
        "feedback",
        &format!("missing command: {what}"),
        "--urgency",
        "queued",
        "--body-file",
        &Input::BodyFile.placeholder(),
    ]
    .map(str::to_owned)
    .to_vec()
}

impl Violation {
    #[deprecated(note = "a refusal names its next move: use Violation::refuse")]
    pub fn new(path: impl Into<String>, rule: &'static str, message: impl Into<String>) -> Self {
        Violation {
            path: path.into(),
            line: None,
            rule,
            message: message.into(),
            next: Next::Run(Vec::new()),
        }
    }

    pub fn refuse(
        path: impl Into<String>,
        rule: &'static str,
        cause: impl Into<String>,
        next: Next,
    ) -> Self {
        Violation {
            path: path.into(),
            line: None,
            rule,
            message: cause.into(),
            next,
        }
    }

    pub fn at(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    fn place(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }

    fn adopt(&self, feedback: &str) -> Vec<String> {
        ["cm", "adopt", &self.place(), "--feedback", feedback]
            .map(str::to_owned)
            .to_vec()
    }

    pub fn next_json(&self) -> Vec<Value> {
        match &self.next {
            Next::Run(moves) => moves
                .iter()
                .map(|each| {
                    json!({
                        "kind": "run",
                        "when": each.when,
                        "argv": each.words(),
                        "inputs": each.inputs(),
                        "command": each.command(),
                    })
                })
                .collect(),
            Next::Missing { what, recorded } => {
                let (argv, inputs) = match recorded {
                    Some(id) => (self.adopt(id), vec![]),
                    None => (feedback_recipe(what), vec![Input::BodyFile.name()]),
                };
                vec![json!({
                    "kind": "missing-command",
                    "what": what,
                    "recorded": recorded,
                    "argv": argv,
                    "inputs": inputs,
                    "command": command(&argv),
                })]
            }
            Next::Settled(why) => vec![json!({ "kind": "settled", "why": why })],
        }
    }
}

impl Serialize for Violation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("path", &self.path)?;
        if let Some(line) = self.line {
            map.serialize_entry("line", &line)?;
        }
        map.serialize_entry("rule", self.rule)?;
        map.serialize_entry("message", &self.message)?;
        map.serialize_entry("next", &self.next_json())?;
        map.end()
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}: {}", self.place(), self.rule, self.message)?;
        match &self.next {
            Next::Run(moves) => {
                for each in moves {
                    match &each.when {
                        Some(when) => write!(f, "\n  next, {when}:\n    {}", each.command())?,
                        None => write!(f, "\n  next: {}", each.command())?,
                    }
                }
            }
            Next::Missing {
                what,
                recorded: Some(id),
            } => write!(
                f,
                "\n  missing-command: {what} (recorded as {id})\n  next: make the change by hand, then: {}",
                command(&self.adopt(id))
            )?,
            Next::Missing {
                what,
                recorded: None,
            } => write!(
                f,
                "\n  missing-command: {what} (not yet recorded)\n  next: {}\n  then: make the change by hand, then: {}",
                command(&feedback_recipe(what)),
                command(&self.adopt("<feedback-id>"))
            )?,
            Next::Settled(why) => write!(f, "\n  next: nothing; {why}")?,
        }
        Ok(())
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

    pub fn resolve_missing(&mut self, root: &Path) {
        for violation in &mut self.violations {
            if let Next::Missing { what, recorded } = &mut violation.next
                && recorded.is_none()
            {
                *recorded = crate::ledger::missing_command_entry(root, what).map(Into::into);
            }
        }
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
