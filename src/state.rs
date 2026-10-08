use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Phase {
    Drafted,
    Launched,
    PrOpen,
    Merged,
    Released,
    Deployed,
    Done,
    Blocked(String),
}

pub const ORDER: [Phase; 7] = [
    Phase::Drafted,
    Phase::Launched,
    Phase::PrOpen,
    Phase::Merged,
    Phase::Released,
    Phase::Deployed,
    Phase::Done,
];

impl Phase {
    pub fn successor(&self) -> Option<Phase> {
        let index = ORDER.iter().position(|phase| phase == self)?;
        ORDER.get(index + 1).cloned()
    }

    fn rank(&self) -> Option<usize> {
        ORDER.iter().position(|phase| phase == self)
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Phase::Drafted => "drafted",
            Phase::Launched => "launched",
            Phase::PrOpen => "pr-open",
            Phase::Merged => "merged",
            Phase::Released => "released",
            Phase::Deployed => "deployed",
            Phase::Done => "done",
            Phase::Blocked(reason) => return write!(f, "blocked:{reason}"),
        };
        f.write_str(name)
    }
}

impl TryFrom<String> for Phase {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Phase> for String {
    fn from(phase: Phase) -> String {
        phase.to_string()
    }
}

impl std::str::FromStr for Phase {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(reason) = text.strip_prefix("blocked:") {
            if reason.is_empty()
                || !reason
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            {
                return Err(format!(
                    "`{text}`: a blocked reason is one word of letters, digits, `.`, `_` or `-`"
                ));
            }
            return Ok(Phase::Blocked(reason.to_owned()));
        }
        ORDER
            .iter()
            .find(|phase| phase.to_string() == text)
            .cloned()
            .ok_or_else(|| {
                let names: Vec<String> = ORDER.iter().map(Phase::to_string).collect();
                format!(
                    "`{text}` is not a state; states are {}, blocked:<reason>",
                    names.join(", ")
                )
            })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

impl Evidence {
    pub fn is_empty(&self) -> bool {
        self == &Evidence::default()
    }

    pub fn absorb(&mut self, other: &Evidence) {
        for (mine, theirs) in [
            (&mut self.sha, &other.sha),
            (&mut self.pr, &other.pr),
            (&mut self.tag, &other.tag),
            (&mut self.digest, &other.digest),
        ] {
            if theirs.is_some() {
                mine.clone_from(theirs);
            }
        }
    }

    pub fn summary(&self) -> String {
        let parts: Vec<String> = [
            ("pr", &self.pr),
            ("sha", &self.sha),
            ("tag", &self.tag),
            ("digest", &self.digest),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.as_ref().map(|value| format!("{name} {value}")))
        .collect();
        if parts.is_empty() {
            "—".to_owned()
        } else {
            parts.join("; ")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Sha,
    Pr,
    Tag,
    Digest,
}

impl Flag {
    pub fn name(self) -> &'static str {
        match self {
            Flag::Sha => "--sha",
            Flag::Pr => "--pr",
            Flag::Tag => "--tag",
            Flag::Digest => "--digest",
        }
    }

    pub fn given(self, evidence: &Evidence) -> bool {
        match self {
            Flag::Sha => evidence.sha.is_some(),
            Flag::Pr => evidence.pr.is_some(),
            Flag::Tag => evidence.tag.is_some(),
            Flag::Digest => evidence.digest.is_some(),
        }
    }
}

pub fn evidence_for(phase: &Phase) -> (&'static [Flag], &'static [Flag]) {
    match phase {
        Phase::PrOpen => (&[Flag::Pr, Flag::Sha], &[]),
        Phase::Merged => (&[Flag::Sha], &[]),
        Phase::Released => (&[Flag::Tag], &[Flag::Digest]),
        Phase::Deployed => (&[Flag::Digest], &[]),
        _ => (&[], &[]),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub state: Phase,
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_from: Option<Phase>,
    #[serde(default, skip_serializing_if = "Evidence::is_empty")]
    pub evidence: Evidence,
}

impl Track {
    pub fn new(at: &str) -> Self {
        Track {
            state: Phase::Drafted,
            since: at.to_owned(),
            blocked_from: None,
            evidence: Evidence::default(),
        }
    }

    pub fn resting(&self) -> &Phase {
        match (&self.state, &self.blocked_from) {
            (Phase::Blocked(_), Some(from)) => from,
            (state, _) => state,
        }
    }

    pub fn check(&self, to: &Phase) -> Result<(), String> {
        if self.state == Phase::Done {
            return Err("done is final".into());
        }
        match to {
            Phase::Blocked(_) => return Ok(()),
            Phase::Drafted => return Err("drafted is where a track starts, never a move".into()),
            Phase::Launched => {
                return Err(
                    "launched is entered only by `cm launch`, which runs the launch checks".into(),
                );
            }
            _ => {}
        }
        let resting = self.resting();
        if matches!(self.state, Phase::Blocked(_)) && to == resting {
            return Ok(());
        }
        let next = resting.successor();
        if next.as_ref() == Some(to) {
            return Ok(());
        }
        let next = next.map_or_else(|| "nothing".to_owned(), |phase| phase.to_string());
        let skipped = match (resting.rank(), to.rank()) {
            (Some(from), Some(target)) if target > from + 1 => {
                let names: Vec<String> = ORDER[from + 1..target]
                    .iter()
                    .map(Phase::to_string)
                    .collect();
                format!("; it skips {}", names.join(", "))
            }
            _ => String::new(),
        };
        Err(format!(
            "{} → {to} is not a move{skipped}; the next state is {next}",
            self.state
        ))
    }

    pub fn apply(&mut self, to: Phase, at: &str, evidence: &Evidence) {
        if let Phase::Blocked(_) = to {
            if !matches!(self.state, Phase::Blocked(_)) {
                self.blocked_from = Some(self.state.clone());
            }
        } else {
            self.blocked_from = None;
        }
        self.state = to;
        self.since = at.to_owned();
        self.evidence.absorb(evidence);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Campaign,
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalMark {
    pub baseline_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignState {
    pub schema: u32,
    pub kind: Kind,
    pub initialized: String,
    #[serde(default)]
    pub written: Vec<String>,
    #[serde(default)]
    pub adopted: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<JournalMark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    #[serde(flatten)]
    pub track: Track,
    pub worktree: String,
    pub launches: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub at: String,
    pub target: String,
    pub from: Phase,
    pub to: Phase,
    #[serde(default, skip_serializing_if = "Evidence::is_empty")]
    pub evidence: Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitState {
    pub schema: u32,
    pub kind: Kind,
    pub unit: String,
    #[serde(flatten)]
    pub track: Track,
    #[serde(default)]
    pub steps: BTreeMap<String, Step>,
    #[serde(default)]
    pub history: Vec<Move>,
}

impl UnitState {
    pub fn new(unit: &str, at: &str) -> Self {
        UnitState {
            schema: SCHEMA,
            kind: Kind::Unit,
            unit: unit.to_owned(),
            track: Track::new(at),
            steps: BTreeMap::new(),
            history: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(state: Phase) -> Track {
        Track {
            state,
            since: "2026-10-08T07:04Z".into(),
            blocked_from: None,
            evidence: Evidence::default(),
        }
    }

    #[test]
    fn refuses_a_skipped_state() {
        let error = track(Phase::Launched).check(&Phase::Released).unwrap_err();
        assert!(error.contains("skips pr-open, merged"), "{error}");
        assert!(track(Phase::Launched).check(&Phase::PrOpen).is_ok());
        assert!(
            track(Phase::Done)
                .check(&Phase::Blocked("x".into()))
                .is_err()
        );
        assert!(track(Phase::PrOpen).check(&Phase::Launched).is_err());
    }

    #[test]
    fn blocked_resumes_or_moves_on() {
        let mut blocked = track(Phase::PrOpen);
        blocked.apply(Phase::Blocked("ci".into()), "t", &Evidence::default());
        assert_eq!(blocked.blocked_from, Some(Phase::PrOpen));
        assert!(blocked.check(&Phase::PrOpen).is_ok());
        assert!(blocked.check(&Phase::Merged).is_ok());
        assert!(blocked.check(&Phase::Released).is_err());
        blocked.apply(Phase::Merged, "t", &Evidence::default());
        assert_eq!(blocked.blocked_from, None);
    }

    #[test]
    fn phases_round_trip() {
        for text in ["drafted", "pr-open", "done", "blocked:third-fix"] {
            assert_eq!(text.parse::<Phase>().unwrap().to_string(), text);
        }
        assert!("blocked:".parse::<Phase>().is_err());
        assert!("blocked:a b".parse::<Phase>().is_err());
        assert!("open".parse::<Phase>().is_err());
    }
}
