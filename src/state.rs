use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::evidence::CampaignFile;
use crate::forge::{GithubRepo, TagName};

pub const SCHEMA: u32 = 3;

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
    Superseded,
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

    pub fn is_final(&self) -> bool {
        matches!(self, Phase::Done | Phase::Superseded)
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
            Phase::Superseded => "superseded",
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
            .chain([&Phase::Superseded])
            .find(|phase| phase.to_string() == text)
            .cloned()
            .ok_or_else(|| {
                let names: Vec<String> = ORDER.iter().map(Phase::to_string).collect();
                format!(
                    "`{text}` is not a state; states are {}, superseded, blocked:<reason>",
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<CampaignFile>,
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
        if other.file.is_some() {
            self.file.clone_from(&other.file);
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
        .chain(self.file.as_ref().map(|file| format!("file {file}")))
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
    File,
}

pub const FLAGS: [Flag; 5] = [Flag::Sha, Flag::Pr, Flag::Tag, Flag::Digest, Flag::File];

impl Flag {
    pub fn name(self) -> &'static str {
        match self {
            Flag::Sha => "--sha",
            Flag::Pr => "--pr",
            Flag::Tag => "--tag",
            Flag::Digest => "--digest",
            Flag::File => "--evidence",
        }
    }

    pub fn given(self, evidence: &Evidence, file: Option<&Path>) -> bool {
        match self {
            Flag::Sha => evidence.sha.is_some(),
            Flag::Pr => evidence.pr.is_some(),
            Flag::Tag => evidence.tag.is_some(),
            Flag::Digest => evidence.digest.is_some(),
            Flag::File => file.is_some(),
        }
    }
}

pub fn evidence_for(phase: &Phase, finish: Finish) -> (&'static [Flag], &'static [Flag]) {
    match (phase, finish) {
        (Phase::PrOpen, _) => (&[Flag::Pr, Flag::Sha], &[]),
        (Phase::Merged, _) => (&[Flag::Sha], &[]),
        (Phase::Released, _) => (&[Flag::Tag], &[Flag::Digest]),
        (Phase::Deployed, _) => (&[Flag::Digest], &[]),
        (Phase::Done, Finish::Evidence) | (Phase::Superseded, _) => (&[Flag::File], &[]),
        (
            Phase::Drafted | Phase::Launched | Phase::Blocked(_) | Phase::Done,
            Finish::Evidence | Finish::Shipped,
        ) => (&[], &[]),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Shipped,
    Evidence,
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

    pub fn check(&self, to: &Phase, finish: Finish) -> Result<(), String> {
        if self.state.is_final() {
            return Err(format!("{} is final", self.state));
        }
        match (to, finish) {
            (Phase::Blocked(_) | Phase::Superseded, Finish::Shipped | Finish::Evidence)
            | (Phase::Done, Finish::Evidence) => return Ok(()),
            (Phase::Drafted, Finish::Shipped | Finish::Evidence) => {
                return Err("drafted is where a track starts, never a move".into());
            }
            (Phase::Launched, Finish::Shipped | Finish::Evidence) => {
                return Err(
                    "launched is entered only by `cm launch`, which runs the launch checks".into(),
                );
            }
            (
                Phase::PrOpen | Phase::Merged | Phase::Released | Phase::Deployed,
                Finish::Shipped | Finish::Evidence,
            )
            | (Phase::Done, Finish::Shipped) => {}
        }
        let resting = self.resting();
        let blocked = match self.state {
            Phase::Blocked(_) => true,
            Phase::Drafted
            | Phase::Launched
            | Phase::PrOpen
            | Phase::Merged
            | Phase::Released
            | Phase::Deployed
            | Phase::Done
            | Phase::Superseded => false,
        };
        if blocked && to == resting {
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
            match self.state {
                Phase::Blocked(_) => {}
                Phase::Drafted
                | Phase::Launched
                | Phase::PrOpen
                | Phase::Merged
                | Phase::Released
                | Phase::Deployed
                | Phase::Done
                | Phase::Superseded => self.blocked_from = Some(self.state.clone()),
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
    Coordinator,
}

pub const LANES: [Lane; 6] = [
    Lane::Pi,
    Lane::Codex,
    Lane::ClaudeCloud,
    Lane::Fable,
    Lane::Owner,
    Lane::Coordinator,
];

impl Lane {
    pub fn name(self) -> &'static str {
        match self {
            Lane::Pi => "pi",
            Lane::Codex => "codex",
            Lane::ClaudeCloud => "claude-cloud",
            Lane::Fable => "fable",
            Lane::Owner => "owner",
            Lane::Coordinator => "coordinator",
        }
    }

    pub fn is_local(self) -> bool {
        match self {
            Lane::Pi => true,
            Lane::Codex | Lane::ClaudeCloud | Lane::Fable | Lane::Owner | Lane::Coordinator => {
                false
            }
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

    pub fn needs(&self, finish: Finish) -> Phase {
        match (self, finish) {
            (After::Merged(_) | After::PrOpen(_), Finish::Evidence) => Phase::Done,
            (After::Merged(_), Finish::Shipped) => Phase::Merged,
            (After::PrOpen(_), Finish::Shipped) => Phase::PrOpen,
            (After::Tag(_), Finish::Shipped | Finish::Evidence) => Phase::Released,
            (After::Deployed(_), Finish::Shipped | Finish::Evidence) => Phase::Deployed,
        }
    }

    pub fn met_by(&self, track: &Track, finish: Finish) -> bool {
        match (track.resting().rank(), self.needs(finish).rank()) {
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
    #[serde(default, skip_serializing_if = "Work::is_code")]
    pub work: Work,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<Attempt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parts: BTreeMap<String, Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<Pin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_red: Vec<KnownRed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waived: Vec<Waived>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watches: Vec<Watch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub step: String,
    pub sha: String,
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.step, self.sha)
    }
}

pub fn same_commit(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    a.len().min(b.len()) >= 7 && (a.starts_with(&b) || b.starts_with(&a))
}

pub fn is_commit(text: &str) -> bool {
    (7..=40).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub file: CampaignFile,
    pub candidate: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Revision>,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<String>,
}

impl Pin {
    /// A fresh pin renews a stale one of the same candidate and harness; a part renews only a pin
    /// of its whole step from before the split, never a sibling part's.
    pub fn renews(&self, older: &Pin) -> bool {
        let same = |new: &Revision, old: &Revision| {
            new.step == old.step
                || (!old.step.contains('#') && new.step.starts_with(&format!("{}#", old.step)))
        };
        self.stale.is_none()
            && same(&self.candidate, &older.candidate)
            && match (&self.harness, &older.harness) {
                (Some(new), Some(old)) => same(new, old),
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownRed {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub standing: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub campaign: bool,
    pub check: String,
    pub reason: String,
    pub owner: String,
    pub until: After,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
}

/// A red check an `--override` waived, kept so lint can name the ones no exception covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waived {
    pub commit: String,
    pub check: String,
    pub file: CampaignFile,
    pub at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    Span,
    Log,
    Metric,
}

impl SignalKind {
    fn name(self) -> &'static str {
        match self {
            SignalKind::Span => "span",
            SignalKind::Log => "log",
            SignalKind::Metric => "metric",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Signal {
    pub kind: SignalKind,
    pub query: String,
}

impl std::str::FromStr for Signal {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let shape = || {
            format!(
                "`{text}` is not span:<query>, log:<query> or metric:<query>; a watch reads a stored signal, never a stdout-only line"
            )
        };
        let (kind, query) = text.split_once(':').ok_or_else(shape)?;
        let kind = [SignalKind::Span, SignalKind::Log, SignalKind::Metric]
            .into_iter()
            .find(|candidate| candidate.name() == kind)
            .ok_or_else(shape)?;
        if query.trim().is_empty() {
            return Err(shape());
        }
        Ok(Signal {
            kind,
            query: query.trim().to_owned(),
        })
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.name(), self.query)
    }
}

impl TryFrom<String> for Signal {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Signal> for String {
    fn from(signal: Signal) -> String {
        signal.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub at: String,
    pub evidence: CampaignFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    pub id: u32,
    pub signal: Signal,
    pub deadline: String,
    pub added: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<Resolved>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchStatus {
    Open,
    Expired,
    Resolved,
}

impl WatchStatus {
    pub fn name(self) -> &'static str {
        match self {
            WatchStatus::Open => "open",
            WatchStatus::Expired => "expired: unresolved",
            WatchStatus::Resolved => "resolved",
        }
    }
}

impl Watch {
    pub fn status(&self, now: jiff::Timestamp, root: &Path) -> WatchStatus {
        let past = crate::plan::instant(&self.deadline).is_some_and(|deadline| deadline <= now);
        let resolved = self
            .resolved
            .as_ref()
            .filter(|resolved| resolved.evidence.check(root).is_ok());
        match (resolved, &self.expired) {
            (Some(_), _) => WatchStatus::Resolved,
            (None, Some(_)) => WatchStatus::Expired,
            (None, None) if past => WatchStatus::Expired,
            (None, None) => WatchStatus::Open,
        }
    }
}

pub fn part_needs(to: &Phase) -> Option<Phase> {
    match to {
        Phase::PrOpen | Phase::Merged | Phase::Released | Phase::Deployed => Some(to.clone()),
        Phase::Done => Some(Phase::Deployed),
        Phase::Drafted | Phase::Launched | Phase::Superseded | Phase::Blocked(_) => None,
    }
}

/// A remote launch's record: what cm knows reached the runtime, so a replacement coordinator
/// resumes one execution or names an unknown outcome instead of dispatching again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub generation: u32,
    pub coordinator: String,
    pub claimed_at: String,
    /// `<generation>:<coordinator>:<claim instant>`: what `begin`, `record` and `end` present.
    #[serde(default)]
    pub claim: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<External>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Ended>,
}

/// One external action: `began` is written before the call, `id` once it returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct External {
    pub kind: String,
    pub began: String,
    pub generation: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    pub at: String,
    pub outcome: AttemptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CampaignFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttemptOutcome {
    Complete,
    Partial,
    Unknown,
}

impl Attempt {
    pub fn mint(&mut self, now: jiff::Timestamp) {
        self.claim = format!("{}:{}:{now}", self.generation, self.coordinator);
    }
}

impl std::str::FromStr for AttemptOutcome {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "complete" => Ok(AttemptOutcome::Complete),
            "partial" => Ok(AttemptOutcome::Partial),
            "unknown" => Ok(AttemptOutcome::Unknown),
            _ => Err(format!(
                "`{text}` is not an outcome; outcomes are complete, partial, unknown"
            )),
        }
    }
}

impl fmt::Display for AttemptOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AttemptOutcome::Complete => "complete",
            AttemptOutcome::Partial => "partial",
            AttemptOutcome::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Work {
    #[default]
    Code,
    Study,
}

impl Work {
    pub fn is_code(&self) -> bool {
        match self {
            Work::Code => true,
            Work::Study => false,
        }
    }
}

impl Step {
    pub fn new(at: &str) -> Self {
        Step {
            track: Track::new(at),
            worktree: String::new(),
            launches: 0,
            lane: None,
            after: Vec::new(),
            github: None,
            tag: None,
            work: Work::Code,
            attempt: None,
            parts: BTreeMap::new(),
            pins: Vec::new(),
            known_red: Vec::new(),
            waived: Vec::new(),
            watches: Vec::new(),
        }
    }

    pub fn short_parts(&self, to: &Phase) -> Vec<(String, Phase)> {
        let Some(need) = part_needs(to).and_then(|need| need.rank()) else {
            return Vec::new();
        };
        self.parts
            .iter()
            .filter(|(_, part)| part.resting().rank().is_none_or(|at| at < need))
            .map(|(name, part)| (name.clone(), part.state.clone()))
            .collect()
    }

    /// The least advanced part's state, which the step's own track never passes.
    pub fn parts_reach(&self) -> Option<Phase> {
        self.parts
            .values()
            .filter_map(|part| part.resting().rank())
            .min()
            .and_then(|rank| ORDER.get(rank).cloned())
    }

    pub fn unresolved_watches(&self, root: &Path) -> impl Iterator<Item = &Watch> {
        self.watches.iter().filter(move |watch| {
            watch
                .resolved
                .as_ref()
                .is_none_or(|resolved| resolved.evidence.check(root).is_err())
        })
    }

    pub fn roll_up_all(
        &mut self,
        at: &str,
        evidence: &Evidence,
        root: &Path,
    ) -> Vec<(Phase, Phase)> {
        std::iter::from_fn(|| self.roll_up(at, evidence, root)).collect()
    }

    fn roll_up(&mut self, at: &str, evidence: &Evidence, root: &Path) -> Option<(Phase, Phase)> {
        let reach = self.parts_reach()?;
        let from = self.track.state.clone();
        if self.track.resting().successor().as_ref() != Some(&reach)
            || self.track.blocked_from.is_some()
            || self.track.check(&reach, self.finish()).is_err()
            || (reach == Phase::Deployed && self.unresolved_watches(root).next().is_some())
        {
            return None;
        }
        let latest = self
            .parts
            .values()
            .max_by(|a, b| a.since.cmp(&b.since))
            .map(|part| part.evidence.clone())
            .unwrap_or_default();
        let evidence = if evidence.is_empty() {
            &latest
        } else {
            evidence
        };
        self.track.apply(reach.clone(), at, evidence);
        Some((from, reach))
    }

    pub fn finish(&self) -> Finish {
        match (self.work, self.lane) {
            (Work::Study, _) | (Work::Code, Some(Lane::Owner)) => Finish::Evidence,
            (
                Work::Code,
                None
                | Some(Lane::Pi | Lane::Codex | Lane::ClaudeCloud | Lane::Fable | Lane::Coordinator),
            ) => Finish::Shipped,
        }
    }

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
        let check = |from: Phase, to: Phase| track(from).check(&to, Finish::Shipped);
        let error = check(Phase::Launched, Phase::Released).unwrap_err();
        assert!(error.contains("skips pr-open, merged"), "{error}");
        assert!(check(Phase::Launched, Phase::PrOpen).is_ok());
        assert!(check(Phase::Done, Phase::Blocked("x".into())).is_err());
        assert!(check(Phase::PrOpen, Phase::Launched).is_err());
        assert!(check(Phase::Launched, Phase::Done).is_err());
    }

    #[test]
    fn an_evidence_finish_is_done_before_any_pr() {
        let check = |from: Phase| track(from).check(&Phase::Done, Finish::Evidence);
        assert!(check(Phase::Drafted).is_ok());
        assert!(check(Phase::Launched).is_ok());
        assert!(check(Phase::Merged).is_ok());
        assert!(check(Phase::Done).is_err());
        let mut blocked = track(Phase::Launched);
        blocked.apply(Phase::Blocked("owner".into()), "t", &Evidence::default());
        assert!(blocked.check(&Phase::Done, Finish::Evidence).is_ok());
        let after = After::Merged("01-a/u/S1".into());
        assert!(!after.met_by(&track(Phase::Merged), Finish::Evidence));
        assert!(after.met_by(&track(Phase::Done), Finish::Evidence));
        assert!(after.met_by(&track(Phase::Merged), Finish::Shipped));
    }

    #[test]
    fn blocked_resumes_or_moves_on() {
        let mut blocked = track(Phase::PrOpen);
        blocked.apply(Phase::Blocked("ci".into()), "t", &Evidence::default());
        assert_eq!(blocked.blocked_from, Some(Phase::PrOpen));
        assert!(blocked.check(&Phase::PrOpen, Finish::Shipped).is_ok());
        assert!(blocked.check(&Phase::Merged, Finish::Shipped).is_ok());
        assert!(blocked.check(&Phase::Released, Finish::Shipped).is_err());
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
        let after = After::Merged("01-a/u/S1".into());
        assert!(after.met_by(&blocked, Finish::Shipped));
        assert!(!after.met_by(&track(Phase::Blocked("x".into())), Finish::Shipped));
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
