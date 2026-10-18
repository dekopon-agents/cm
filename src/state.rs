use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::forge::{GithubRepo, TagName};

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

    pub fn rank(&self) -> Option<usize> {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Lane {
    Pi,
    Codex,
    ClaudeCloud,
    Fable,
    Owner,
}

pub const LANES: [Lane; 5] = [
    Lane::Pi,
    Lane::Codex,
    Lane::ClaudeCloud,
    Lane::Fable,
    Lane::Owner,
];

impl Lane {
    pub fn name(self) -> &'static str {
        match self {
            Lane::Pi => "pi",
            Lane::Codex => "codex",
            Lane::ClaudeCloud => "claude-cloud",
            Lane::Fable => "fable",
            Lane::Owner => "owner",
        }
    }

    pub fn is_local(self) -> bool {
        match self {
            Lane::Pi => true,
            Lane::Codex | Lane::ClaudeCloud | Lane::Fable | Lane::Owner => false,
        }
    }
}

impl fmt::Display for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Lane {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        LANES
            .into_iter()
            .find(|lane| lane.name() == text)
            .ok_or_else(|| {
                let names: Vec<&str> = LANES.iter().map(|lane| lane.name()).collect();
                format!("`{text}` is not a lane; lanes are {}", names.join(", "))
            })
    }
}

impl TryFrom<String> for Lane {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Lane> for String {
    fn from(lane: Lane) -> String {
        lane.name().to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl std::str::FromStr for Version {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = text.split('.').collect();
        let number = |part: &str| {
            (!part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0')))
            .then(|| part.parse::<u64>().ok())
            .flatten()
        };
        match parts[..] {
            [major, minor, patch] => match (number(major), number(minor), number(patch)) {
                (Some(major), Some(minor), Some(patch)) => Ok(Version {
                    major,
                    minor,
                    patch,
                }),
                _ => Err(format!("`{text}` is not a version <major>.<minor>.<patch>")),
            },
            _ => Err(format!("`{text}` is not a version <major>.<minor>.<patch>")),
        }
    }
}

pub const RELEASES: &str = "00-release";
pub const DEFAULT_TAG: &str = "v{version}";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Release {
    pub line: String,
    pub version: Version,
}

impl Release {
    pub fn target(&self) -> String {
        format!("{RELEASES}/{}/{}", self.line, self.version)
    }

    pub fn of_target(target: &str) -> Option<Release> {
        let mut parts = target.split('/');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(RELEASES), Some(line), Some(version), None) => Some(Release {
                line: line.to_owned(),
                version: version.parse().ok()?,
            }),
            _ => None,
        }
    }

    pub fn tag(&self, pattern: &str) -> Result<TagName, String> {
        if pattern.matches("{version}").count() != 1 {
            return Err(format!(
                "`{pattern}` holds `{{version}}` once, e.g. `{DEFAULT_TAG}` or `{}-{{version}}`",
                self.line
            ));
        }
        pattern
            .replace("{version}", &self.version.to_string())
            .parse()
    }

    fn parse(text: &str) -> Result<Release, String> {
        let shape = || format!("`{text}` is not <repo>@<major>.<minor>.<patch>");
        let (line, version) = text.split_once('@').ok_or_else(shape)?;
        if !crate::store::is_name(line) {
            return Err(shape());
        }
        Ok(Release {
            line: line.to_owned(),
            version: version.parse().map_err(|_| shape())?,
        })
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.line, self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum After {
    Merged(String),
    PrOpen(String),
    Tag(Release),
    Deployed(Release),
}

impl After {
    pub fn parse(text: &str, unit: &str) -> Result<After, String> {
        let step = |rest: &str| -> Result<String, String> {
            let full = if rest.contains('/') {
                rest.to_owned()
            } else {
                format!("{unit}/{rest}")
            };
            crate::store::parse_target(&full, true)
                .map(|target| format!("{}/{}", target.unit, target.step.unwrap_or_default()))
                .map_err(|_| {
                    format!(
                        "`{text}` is not <step>, <NN-name>/<unit>/<step>, pr-open:<step>, tag:<repo>@<version> or deployed:<repo>@<version>"
                    )
                })
        };
        if let Some(rest) = text.strip_prefix("tag:") {
            return Release::parse(rest).map(After::Tag);
        }
        if let Some(rest) = text.strip_prefix("deployed:") {
            return Release::parse(rest).map(After::Deployed);
        }
        if let Some(rest) = text.strip_prefix("pr-open:") {
            return step(rest).map(After::PrOpen);
        }
        step(text).map(After::Merged)
    }

    pub fn target(&self) -> String {
        match self {
            After::Merged(target) | After::PrOpen(target) => target.clone(),
            After::Tag(release) | After::Deployed(release) => release.target(),
        }
    }

    pub fn needs(&self) -> Phase {
        match self {
            After::Merged(_) => Phase::Merged,
            After::PrOpen(_) => Phase::PrOpen,
            After::Tag(_) => Phase::Released,
            After::Deployed(_) => Phase::Deployed,
        }
    }

    pub fn met_by(&self, track: &Track) -> bool {
        match (track.resting().rank(), self.needs().rank()) {
            (Some(at), Some(needed)) => at >= needed,
            _ => false,
        }
    }
}

impl fmt::Display for After {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            After::Merged(target) => f.write_str(target),
            After::PrOpen(target) => write!(f, "pr-open:{target}"),
            After::Tag(release) => write!(f, "tag:{release}"),
            After::Deployed(release) => write!(f, "deployed:{release}"),
        }
    }
}

impl TryFrom<String> for After {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !value.starts_with("tag:") && !value.starts_with("deployed:") {
            let rest = value.strip_prefix("pr-open:").unwrap_or(&value);
            if rest.split('/').count() != 3 {
                return Err(format!("`{value}` names no <NN-name>/<unit>/<step>"));
            }
        }
        After::parse(&value, "")
    }
}

impl From<After> for String {
    fn from(after: After) -> String {
        after.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    #[serde(flatten)]
    pub track: Track,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub worktree: String,
    #[serde(default)]
    pub launches: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<Lane>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<After>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubRepo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<TagName>,
}

impl Step {
    pub fn release_tag(&self, release: &Release) -> TagName {
        self.tag.clone().unwrap_or_else(|| {
            release
                .tag(DEFAULT_TAG)
                .expect("a version always forms the default tag")
        })
    }

    pub fn ships_from(&self, release: &Release, repo: &GithubRepo) -> bool {
        match &self.github {
            Some(github) => github == repo,
            None => repo.name() == release.line,
        }
    }

    pub fn release_repo(&self, release: &Release) -> String {
        match &self.github {
            Some(github) => github.to_string(),
            None => format!("<owner>/{}", release.line),
        }
    }
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
    fn after_resolves_against_the_unit() {
        let unit = "01-a/u";
        let parse = |text: &str| After::parse(text, unit).map(|after| after.to_string());
        assert_eq!(parse("S1").unwrap(), "01-a/u/S1");
        assert_eq!(parse("02-b/v/S3").unwrap(), "02-b/v/S3");
        assert_eq!(parse("pr-open:S1").unwrap(), "pr-open:01-a/u/S1");
        assert_eq!(parse("tag:dekopon@0.49.0").unwrap(), "tag:dekopon@0.49.0");
        assert_eq!(
            After::parse("deployed:dekopon@0.49.0", unit)
                .unwrap()
                .target(),
            "00-release/dekopon/0.49.0"
        );
        for bad in [
            "tag:dekopon@v0.49.0",
            "tag:@1.0.0",
            "02-b/S3",
            "a b",
            "tag:x@1.0",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let stored: After = serde_json::from_str("\"pr-open:01-a/u/S1\"").unwrap();
        assert_eq!(stored, After::PrOpen("01-a/u/S1".into()));
        assert!(serde_json::from_str::<After>("\"S1\"").is_err());
    }

    #[test]
    fn versions_order_and_refuse_noise() {
        let v = |text: &str| text.parse::<Version>();
        assert!(v("0.10.0").unwrap() > v("0.9.9").unwrap());
        for bad in ["v1.0.0", "1.0", "1.0.0-rc1", "01.0.0", ""] {
            assert!(v(bad).is_err(), "{bad}");
        }
        let release = Release::of_target("00-release/cm/0.3.0").unwrap();
        assert_eq!(release.tag(DEFAULT_TAG).unwrap().as_str(), "v0.3.0");
        assert_eq!(release.tag("cm-{version}").unwrap().as_str(), "cm-0.3.0");
        assert!(release.tag("cm-0.3.0").is_err());
        assert!(Release::of_target("01-a/cm/0.3.0").is_none());
    }

    #[test]
    fn only_the_pi_lane_is_local() {
        let local: Vec<Lane> = LANES.into_iter().filter(|lane| lane.is_local()).collect();
        assert_eq!(local, [Lane::Pi]);
    }

    #[test]
    fn a_blocked_track_meets_gates_from_where_it_rests() {
        let mut blocked = track(Phase::Merged);
        blocked.apply(Phase::Blocked("ci".into()), "t", &Evidence::default());
        assert!(After::Merged("01-a/u/S1".into()).met_by(&blocked));
        assert!(!After::Merged("01-a/u/S1".into()).met_by(&track(Phase::Blocked("x".into()))));
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
