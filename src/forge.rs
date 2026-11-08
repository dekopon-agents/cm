use std::fmt;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    Spawn,
    Exit(Option<i32>),
    Timeout(Duration),
    Json,
    UnsupportedFormula,
    ManifestDigest,
    Http(u16),
    Curl(Box<ForgeError>),
    Url,
}

impl ForgeError {
    fn fmt_program(&self, program: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForgeError::Curl(error) => error.fmt_program("curl", f),
            ForgeError::Http(status) => {
                write!(f, "{program} HTTP {status}; retry the publication read")
            }
            ForgeError::Spawn => write!(f, "could not execute {program}; check installation"),
            ForgeError::Exit(Some(code)) => {
                write!(f, "{program} exited {code}; check access with {program}")
            }
            ForgeError::Exit(None) => write!(f, "{program} exited by signal"),
            ForgeError::Timeout(after) => write!(
                f,
                "{program} did not answer within {}s; killed it",
                after.as_secs()
            ),
            ForgeError::Json => write!(f, "{program} returned JSON cm cannot read"),
            ForgeError::UnsupportedFormula => {
                f.write_str("tap formula uses unsupported or invalid publication content")
            }
            ForgeError::ManifestDigest => {
                f.write_str("image manifest response is missing docker-content-digest")
            }
            ForgeError::Url => f.write_str("not a https://github.com/<owner>/<repo>/pull/<n> URL"),
        }
    }
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_program("gh", f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Present(String),
    Yanked,
    Missing,
    Stale(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef(String);

impl ImageRef {
    fn path(&self) -> &str {
        self.0.strip_prefix("ghcr.io/").unwrap_or_default()
    }
}

impl std::str::FromStr for ImageRef {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let path = text.strip_prefix("ghcr.io/").unwrap_or_default();
        if path.split('/').count() >= 2
            && path.split('/').all(|part| {
                !part.is_empty()
                    && part.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && part.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && part.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c)
                    })
            })
        {
            Ok(Self(text.to_owned()))
        } else {
            Err(format!(
                "`{text}` is not a ghcr.io image reference without a tag or digest"
            ))
        }
    }
}

impl fmt::Display for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GithubRepo {
    owner: String,
    name: String,
}

impl GithubRepo {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn of_pr(url: &str) -> Option<GithubRepo> {
        let rest = url.strip_prefix("https://github.com/")?;
        let mut parts = rest.split('/');
        let owner = parts.next()?;
        let name = parts.next()?;
        (parts.next() == Some("pull"))
            .then(|| format!("{owner}/{name}").parse().ok())
            .flatten()
    }
}

impl std::str::FromStr for GithubRepo {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.split_once('/') {
            Some((owner, name)) if crate::store::is_name(owner) && crate::store::is_name(name) => {
                Ok(GithubRepo {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                })
            }
            _ => Err(format!("`{text}` is not <owner>/<name>")),
        }
    }
}

impl fmt::Display for GithubRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl TryFrom<String> for GithubRepo {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<GithubRepo> for String {
    fn from(repo: GithubRepo) -> String {
        repo.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TagName(String);

impl TagName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for TagName {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let fits = !text.is_empty()
            && !text.starts_with(['-', '.', '/'])
            && !text.ends_with(['/', '.'])
            && !text.contains("..")
            && !text.contains("//")
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c));
        if fits {
            Ok(TagName(text.to_owned()))
        } else {
            Err(format!(
                "`{text}` is not a tag name of letters, digits, `.`, `_`, `-` or `/`"
            ))
        }
    }
}

impl fmt::Display for TagName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for TagName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<TagName> for String {
    fn from(tag: TagName) -> String {
        tag.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitSha(String);

impl CommitSha {
    pub fn new(sha: impl Into<String>) -> Self {
        CommitSha(sha.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pull {
    pub state: PullState,
    pub merge_commit: Option<CommitSha>,
    pub merged_at: Option<String>,
    /// The PR's head commit, as GitHub reports it.
    pub head: Option<CommitSha>,
    /// The base branch's tip, as GitHub reports it.
    pub base: Option<CommitSha>,
    /// GitHub's `mergeable_state` (`clean`, `dirty`, `blocked`, `unknown`, …), lower case.
    pub mergeable_state: Option<MergeableState>,
}

/// GitHub's `mergeable_state` as cm reads it; any value but `dirty` keeps its string, so it
/// round-trips through `.cm/pulls.json` unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum MergeableState {
    Dirty,
    Other(String),
}

impl From<String> for MergeableState {
    fn from(state: String) -> Self {
        match state.as_str() {
            "dirty" => MergeableState::Dirty,
            _ => MergeableState::Other(state),
        }
    }
}

impl From<MergeableState> for String {
    fn from(state: MergeableState) -> Self {
        match state {
            MergeableState::Dirty => "dirty".to_owned(),
            MergeableState::Other(state) => state,
        }
    }
}

/// One check run or commit status on a commit, its state lower case: a check run's conclusion
/// once completed (`success`, `failure`, `skipped`, …), else its status; a status's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckState {
    pub name: String,
    pub state: Conclusion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(into = "String")]
pub enum Conclusion {
    Success,
    Neutral,
    Skipped,
    Failure,
    Other(String),
}

impl Conclusion {
    fn as_str(&self) -> &str {
        match self {
            Conclusion::Success => "success",
            Conclusion::Neutral => "neutral",
            Conclusion::Skipped => "skipped",
            Conclusion::Failure => "failure",
            Conclusion::Other(state) => state,
        }
    }
}

impl From<String> for Conclusion {
    fn from(state: String) -> Self {
        match state.as_str() {
            "success" => Conclusion::Success,
            "neutral" => Conclusion::Neutral,
            "skipped" => Conclusion::Skipped,
            "failure" => Conclusion::Failure,
            _ => Conclusion::Other(state),
        }
    }
}

impl From<Conclusion> for String {
    fn from(state: Conclusion) -> Self {
        state.as_str().to_owned()
    }
}

impl PartialEq<&str> for Conclusion {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl fmt::Display for Conclusion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CheckState {
    pub fn green(&self) -> bool {
        match self.state {
            Conclusion::Success | Conclusion::Neutral | Conclusion::Skipped => true,
            Conclusion::Failure | Conclusion::Other(_) => false,
        }
    }
}

pub fn ci_green(checks: &[CheckState]) -> bool {
    !checks.is_empty() && checks.iter().all(CheckState::green)
}

pub trait Forge {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError>;
    /// Every check run and commit status GitHub reports on `sha`.
    fn checks(&self, repo: &GithubRepo, sha: &CommitSha) -> Result<Vec<CheckState>, ForgeError>;
    /// The named checks' latest completed results along the base branch's first-parent history.
    /// Unavailable history is an error, never evidence that a check failed.
    fn latest_completed_checks(
        &self,
        _repo: &GithubRepo,
        _base: &CommitSha,
        _names: &[String],
    ) -> Result<Vec<CheckState>, ForgeError> {
        Err(ForgeError::Json)
    }
    fn crate_version(
        &self,
        _name: &str,
        _version: &crate::state::Version,
    ) -> Result<Read, ForgeError> {
        Err(ForgeError::Json)
    }
    fn tap_formula(
        &self,
        _tap: &GithubRepo,
        _formula: &str,
        _repo: &GithubRepo,
        _tag: &TagName,
        _version: &crate::state::Version,
    ) -> Result<Read, ForgeError> {
        Err(ForgeError::Json)
    }
    fn image_manifest(&self, _image: &ImageRef, _tag: &str) -> Result<Read, ForgeError> {
        Err(ForgeError::Json)
    }
    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError>;
    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError>;
}

pub const GH_TIMEOUT: Duration = Duration::from_secs(60);

const IMAGE_ACCEPT_HEADER: &str = concat!(
    "Accept: application/vnd.oci.image.index.v1+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json, ",
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.docker.distribution.manifest.v2+json",
);

pub struct Gh {
    pub program: PathBuf,
    pub timeout: Duration,
}

impl Default for Gh {
    fn default() -> Self {
        Gh {
            program: PathBuf::from("gh"),
            timeout: GH_TIMEOUT,
        }
    }
}

/// Pages a list endpoint reads, 100 items each; more than this many pages is an error.
const MAX_PAGES: usize = 50;

impl Gh {
    fn read_checks(
        &self,
        repo: &GithubRepo,
        sha: &CommitSha,
        completed: bool,
    ) -> Result<Vec<CheckState>, ForgeError> {
        #[derive(Deserialize)]
        struct Runs {
            total_count: usize,
            check_runs: Vec<Run>,
        }
        #[derive(Deserialize)]
        struct Run {
            #[serde(default)]
            id: u64,
            name: String,
            status: String,
            conclusion: Option<String>,
        }
        #[derive(Deserialize)]
        struct Combined {
            total_count: usize,
            statuses: Vec<Status>,
        }
        #[derive(Deserialize)]
        struct Status {
            #[serde(default)]
            id: u64,
            context: String,
            state: String,
        }
        let mut runs = self.pages(
            &format!(
                "repos/{repo}/commits/{sha}/check-runs{}",
                if completed {
                    "?status=completed&filter=all"
                } else {
                    ""
                }
            ),
            |page: Runs| (page.total_count, page.check_runs),
        )?;
        let mut statuses = if completed {
            let mut statuses = Vec::new();
            for page in 1..=MAX_PAGES {
                let items: Vec<Status> = self.json(&[
                    "api",
                    &format!("repos/{repo}/commits/{sha}/statuses?per_page=100&page={page}"),
                ])?;
                let done = items.len() < 100;
                statuses.extend(items);
                if done {
                    break;
                }
                if page == MAX_PAGES {
                    return Err(ForgeError::Json);
                }
            }
            statuses
        } else {
            self.pages(
                &format!("repos/{repo}/commits/{sha}/status"),
                |page: Combined| (page.total_count, page.statuses),
            )?
        };
        if completed {
            runs.retain(|run| run.status == "completed");
            runs.sort_by_key(|run| std::cmp::Reverse(run.id));
            let mut names = std::collections::BTreeSet::new();
            runs.retain(|run| names.insert(run.name.clone()));
            statuses.retain(|status| status.state != "pending");
            statuses.sort_by_key(|status| std::cmp::Reverse(status.id));
            let mut names = std::collections::BTreeSet::new();
            statuses.retain(|status| names.insert(status.context.clone()));
        }
        let runs = runs.into_iter().map(|run| CheckState {
            state: match (run.status.as_str(), run.conclusion) {
                ("completed", Some(conclusion)) => conclusion.to_lowercase(),
                (status, _) => status.to_lowercase(),
            }
            .into(),
            name: run.name,
        });
        let statuses = statuses.into_iter().map(|status| CheckState {
            name: status.context,
            state: status.state.to_lowercase().into(),
        });
        Ok(runs.chain(statuses).collect())
    }

    /// Every item of a paginated list endpoint whose pages report `total_count`: page after page
    /// until the total is read. A page that cannot be read, or that ends the list short of its
    /// total, is an error, never a shorter list.
    fn pages<P: DeserializeOwned, T>(
        &self,
        path: &str,
        split: impl Fn(P) -> (usize, Vec<T>),
    ) -> Result<Vec<T>, ForgeError> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let (total, items) = split(self.json(&[
                "api",
                &format!(
                    "{path}{}per_page=100&page={page}",
                    if path.contains('?') { '&' } else { '?' }
                ),
            ])?);
            let empty = items.is_empty();
            out.extend(items);
            if out.len() >= total {
                return Ok(out);
            }
            if empty {
                return Err(ForgeError::Json);
            }
        }
        Err(ForgeError::Json)
    }

    fn json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, ForgeError> {
        let output = run(&self.program, args, self.timeout, None)?;
        if !output.status.success() {
            return Err(ForgeError::Exit(output.status.code()));
        }
        serde_json::from_slice(&output.stdout).map_err(|_| ForgeError::Json)
    }

    fn http(&self, args: &[&str], input: Option<&[u8]>) -> Result<(u16, String), ForgeError> {
        let mut request = vec!["-sS", "--max-time", "60", "--write-out", "\n%{http_code}"];
        request.extend_from_slice(args);
        let output = run(
            Path::new("curl"),
            &request,
            self.timeout.min(GH_TIMEOUT),
            input,
        )?;
        if !output.status.success() {
            return Err(ForgeError::Exit(output.status.code()));
        }
        let text = String::from_utf8(output.stdout).map_err(|_| ForgeError::Json)?;
        let (body, status) = text.rsplit_once('\n').ok_or(ForgeError::Json)?;
        Ok((
            status.trim().parse().map_err(|_| ForgeError::Json)?,
            body.to_owned(),
        ))
    }
}

fn run(
    program: &Path,
    args: &[&str],
    timeout: Duration,
    input: Option<&[u8]>,
) -> Result<Output, ForgeError> {
    let mut child = Command::new(program)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ForgeError::Spawn)?;
    let mut stdout = child.stdout.take().ok_or(ForgeError::Spawn)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    let writer = if let Some(input) = input {
        let mut stdin = child.stdin.take().ok_or(ForgeError::Spawn)?;
        let bytes = input.to_vec();
        Some(std::thread::spawn(move || stdin.write_all(&bytes)))
    } else {
        None
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ForgeError::Timeout(timeout));
            }
        }
    };
    let stdout = reader
        .join()
        .map_err(|_| ForgeError::Json)?
        .map_err(|_| ForgeError::Json)?;
    if let Some(writer) = writer {
        let sent = writer.join().map_err(|_| ForgeError::Spawn)?;
        if status.success() {
            sent.map_err(|_| ForgeError::Spawn)?;
        }
    }
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

fn crate_read(
    status: u16,
    body: &str,
    version: &crate::state::Version,
) -> Result<Read, ForgeError> {
    if status == 404 {
        serde_json::from_str::<serde_json::Value>(body).map_err(|_| ForgeError::Json)?;
        return Ok(Read::Missing);
    }
    if status != 200 {
        return Err(ForgeError::Http(status));
    }
    #[derive(Deserialize)]
    struct Response {
        version: Published,
    }
    #[derive(Deserialize)]
    struct Published {
        num: String,
        yanked: bool,
        #[serde(default)]
        checksum: String,
    }
    let response: Response = serde_json::from_str(body).map_err(|_| ForgeError::Json)?;
    if response.version.num != version.to_string() {
        return Err(ForgeError::Json);
    }
    if response.version.yanked {
        return Ok(Read::Yanked);
    }
    if response.version.checksum.is_empty() {
        return Err(ForgeError::Json);
    }
    Ok(Read::Present(response.version.checksum))
}

pub fn parse_formula(
    body: &str,
    sha: &str,
    repo: &GithubRepo,
    tag: &TagName,
    version: &crate::state::Version,
) -> Result<Read, ForgeError> {
    fn argument<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let rest = line.strip_prefix(key)?;
        (!rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')).then_some(rest.trim_start())
    }
    fn literal<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let rest = argument(line, key)?;
        let parenthesized = rest.starts_with('(');
        let rest = rest.strip_prefix('(').unwrap_or(rest).trim_start();
        let (value, tail) = rest.strip_prefix('"')?.split_once('"')?;
        if !value
            .chars()
            .all(|c| !c.is_control() && !matches!(c, '\\' | '#'))
        {
            return None;
        }
        let tail = if parenthesized {
            tail.trim().strip_suffix(')')?.trim()
        } else {
            tail.trim()
        };
        let download_option = key == "url"
            && tail
                .strip_prefix(',')
                .and_then(|tail| tail.trim_start().strip_prefix("using:"))
                .is_some_and(|value| value.trim() == ":curl");
        (tail.is_empty() || download_option).then_some(value)
    }
    let lines: Vec<_> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let prefix = format!("https://github.com/{repo}/releases/download/");
    let mut urls = 0;
    let mut stale = None;
    for (index, line) in lines.iter().enumerate() {
        if argument(line, "url").is_some() {
            urls += 1;
            let url = literal(line, "url").ok_or(ForgeError::UnsupportedFormula)?;
            let (named, asset) = url
                .strip_prefix(&prefix)
                .and_then(|rest| rest.rsplit_once('/'))
                .ok_or(ForgeError::UnsupportedFormula)?;
            if named != tag.as_str() {
                stale.get_or_insert(named);
            }
            if asset.is_empty() {
                return Err(ForgeError::UnsupportedFormula);
            }
            let checksum = lines
                .get(index + 1)
                .and_then(|line| literal(line, "sha256"))
                .ok_or(ForgeError::UnsupportedFormula)?;
            if checksum.len() != 64
                || !checksum
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(ForgeError::UnsupportedFormula);
            }
        }
        if argument(line, "version").is_some() {
            let named = literal(line, "version").ok_or(ForgeError::UnsupportedFormula)?;
            if named != version.to_string() {
                stale.get_or_insert(named);
            }
        }
    }
    if urls == 0 || sha.is_empty() {
        return Err(ForgeError::UnsupportedFormula);
    }
    Ok(match stale {
        Some(named) => Read::Stale(named.to_owned()),
        None => Read::Present(sha.to_owned()),
    })
}

fn image_read(status: u16, headers: &str) -> Result<Read, ForgeError> {
    match status {
        404 => Ok(Read::Missing),
        200 => headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, value)| {
                key.eq_ignore_ascii_case("docker-content-digest") && !value.trim().is_empty()
            })
            .map(|(_, value)| Read::Present(value.trim().to_owned()))
            .ok_or(ForgeError::ManifestDigest),
        _ => Err(ForgeError::Http(status)),
    }
}

#[derive(Clone, Default)]
pub(crate) struct Publication {
    pub published: Vec<String>,
    pub missing: Vec<String>,
    pub unknown: Vec<String>,
}

pub(crate) fn publication(
    forge: Option<&dyn Forge>,
    artifacts: &[crate::state::Artifact],
    repo: Option<&GithubRepo>,
    version: &crate::state::Version,
    tag: &TagName,
) -> Publication {
    use crate::state::Artifact;
    let mut out = Publication::default();
    for artifact in artifacts {
        let read = (|| {
            let forge = forge.ok_or(ForgeError::Json)?;
            match artifact {
                Artifact::Crate(name) => forge
                    .crate_version(name, version)
                    .map(|read| (read, artifact.to_string(), Some(version.to_string()))),
                Artifact::Image(image) => forge
                    .image_manifest(image, &format!("v{version}"))
                    .map(|read| (read, artifact.to_string(), None)),
                Artifact::Tap(spec) => {
                    let repo = repo.ok_or(ForgeError::Json)?;
                    let (tap, formula) = match spec.rsplit_once('/') {
                        Some((tap, formula)) => {
                            (tap.parse().map_err(|_| ForgeError::Json)?, formula)
                        }
                        None => (
                            format!("{}/homebrew-tap", repo.owner)
                                .parse()
                                .map_err(|_| ForgeError::Json)?,
                            spec.as_str(),
                        ),
                    };
                    forge
                        .tap_formula(&tap, formula, repo, tag, version)
                        .map(|read| (read, format!("tap:{tap}/{formula}"), None))
                }
            }
        })();
        match read {
            Ok((Read::Present(value), name, fixed)) => out
                .published
                .push(format!("{name}@{}", fixed.unwrap_or(value))),
            Ok((Read::Missing, _, _)) => {
                out.missing.push(format!("{artifact} {version} is missing"))
            }
            Ok((Read::Yanked, _, _)) => out.missing.push(format!("{artifact} {version} is yanked")),
            Ok((Read::Stale(named), _, _)) => out.missing.push(format!("{artifact} names {named}")),
            Err(error) => out.unknown.push(format!("{artifact}: {error}")),
        }
    }
    out
}

impl Forge for Gh {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError> {
        #[derive(Deserialize)]
        struct Rest {
            state: String,
            merged: bool,
            merge_commit_sha: Option<String>,
            #[serde(default)]
            merged_at: Option<String>,
            #[serde(default)]
            head: Option<Head>,
            #[serde(default)]
            base: Option<Head>,
            #[serde(default)]
            mergeable_state: Option<String>,
        }
        #[derive(Deserialize)]
        struct Head {
            sha: String,
        }
        let path = url
            .strip_prefix("https://github.com/")
            .and_then(|rest| rest.split_once("/pull/"))
            .map(|(repo, number)| format!("repos/{repo}/pulls/{number}"))
            .ok_or(ForgeError::Url)?;
        let rest: Rest = self.json(&["api", &path])?;
        let state = match (rest.merged, rest.state.as_str()) {
            (true, _) => PullState::Merged,
            (false, "open") => PullState::Open,
            (false, "closed") => PullState::Closed,
            _ => return Err(ForgeError::Json),
        };
        Ok(Pull {
            state,
            merge_commit: rest
                .merge_commit_sha
                .filter(|_| state == PullState::Merged)
                .map(CommitSha::new),
            merged_at: rest.merged_at.filter(|_| state == PullState::Merged),
            head: rest.head.map(|head| CommitSha::new(head.sha)),
            base: rest.base.map(|base| CommitSha::new(base.sha)),
            mergeable_state: rest
                .mergeable_state
                .map(|state| state.to_lowercase().into()),
        })
    }

    fn checks(&self, repo: &GithubRepo, sha: &CommitSha) -> Result<Vec<CheckState>, ForgeError> {
        self.read_checks(repo, sha, false)
    }

    fn latest_completed_checks(
        &self,
        repo: &GithubRepo,
        base: &CommitSha,
        names: &[String],
    ) -> Result<Vec<CheckState>, ForgeError> {
        #[derive(Deserialize)]
        struct Commit {
            parents: Vec<Parent>,
        }
        #[derive(Deserialize)]
        struct Parent {
            sha: String,
        }
        let mut remaining: std::collections::BTreeSet<_> = names.iter().cloned().collect();
        let mut found = Vec::new();
        let mut sha = base.clone();
        while !remaining.is_empty() {
            let checks = self.read_checks(repo, &sha, true)?;
            let named: Vec<_> = checks
                .into_iter()
                .filter(|check| remaining.contains(&check.name))
                .collect();
            for check in &named {
                remaining.remove(&check.name);
            }
            found.extend(named);
            if remaining.is_empty() {
                break;
            }
            let commit: Commit = self.json(&["api", &format!("repos/{repo}/commits/{sha}")])?;
            let Some(parent) = commit.parents.first() else {
                break;
            };
            sha = CommitSha::new(parent.sha.clone());
        }
        Ok(found)
    }

    fn crate_version(
        &self,
        name: &str,
        version: &crate::state::Version,
    ) -> Result<Read, ForgeError> {
        (|| {
            let (status, body) = self.http(
                &[
                    "--header",
                    concat!(
                        "User-Agent: cm/",
                        env!("CARGO_PKG_VERSION"),
                        " (+https://github.com/dekopon-agents/cm)"
                    ),
                    &format!("https://crates.io/api/v1/crates/{name}/{version}"),
                ],
                None,
            )?;
            crate_read(status, &body, version)
        })()
        .map_err(|error| ForgeError::Curl(Box::new(error)))
    }

    fn tap_formula(
        &self,
        tap: &GithubRepo,
        formula: &str,
        repo: &GithubRepo,
        tag: &TagName,
        version: &crate::state::Version,
    ) -> Result<Read, ForgeError> {
        let output = run(
            &self.program,
            &[
                "api",
                "--include",
                &format!("repos/{tap}/contents/Formula/{formula}.rb"),
                "--jq",
                "{sha, content: (.content | @base64d)}",
            ],
            self.timeout.min(GH_TIMEOUT),
            None,
        )?;
        let text = String::from_utf8(output.stdout).map_err(|_| ForgeError::Json)?;
        let (headers, body) = text
            .split_once("\r\n\r\n")
            .or_else(|| text.split_once("\n\n"))
            .ok_or(ForgeError::Json)?;
        let status: u16 = headers
            .split_whitespace()
            .nth(1)
            .ok_or(ForgeError::Json)?
            .parse()
            .map_err(|_| ForgeError::Json)?;
        if status == 404 {
            return Ok(Read::Missing);
        }
        if status != 200 {
            return Err(ForgeError::Http(status));
        }
        if !output.status.success() {
            return Err(ForgeError::Exit(output.status.code()));
        }
        #[derive(Deserialize)]
        struct Formula {
            sha: String,
            content: String,
        }
        let formula: Formula = serde_json::from_str(body).map_err(|_| ForgeError::Json)?;
        parse_formula(&formula.content, &formula.sha, repo, tag, version)
    }

    fn image_manifest(&self, image: &ImageRef, tag: &str) -> Result<Read, ForgeError> {
        (|| {
            let (status, body) = self.http(
                &[&format!(
                    "https://ghcr.io/token?scope=repository:{}:pull",
                    image.path()
                )],
                None,
            )?;
            if status != 200 {
                return Err(ForgeError::Http(status));
            }
            #[derive(Deserialize)]
            struct Token {
                token: String,
            }
            let token: Token = serde_json::from_str(&body).map_err(|_| ForgeError::Json)?;
            if token.token.is_empty() || token.token.contains(['\r', '\n']) {
                return Err(ForgeError::Json);
            }
            let (status, headers) = self.http(
                &[
                    "--head",
                    "--header",
                    "@-",
                    "--header",
                    IMAGE_ACCEPT_HEADER,
                    &format!("https://ghcr.io/v2/{}/manifests/{tag}", image.path()),
                ],
                Some(format!("Authorization: Bearer {}\n", token.token).as_bytes()),
            )?;
            image_read(status, &headers)
        })()
        .map_err(|error| ForgeError::Curl(Box::new(error)))
    }

    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError> {
        #[derive(Deserialize)]
        struct Ref {
            #[serde(rename = "ref")]
            name: String,
        }
        let refs: Vec<Ref> =
            self.json(&["api", &format!("repos/{repo}/git/matching-refs/tags/{tag}")])?;
        let wanted = format!("refs/tags/{tag}");
        Ok(refs.iter().any(|found| found.name == wanted))
    }

    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "lowercase")]
        enum Status {
            Ahead,
            Identical,
            Behind,
            Diverged,
        }
        #[derive(Deserialize)]
        struct Compare {
            status: Status,
        }
        let compare: Compare =
            self.json(&["api", &format!("repos/{repo}/compare/{sha}...{tag}")])?;
        Ok(match compare.status {
            Status::Ahead | Status::Identical => true,
            Status::Behind | Status::Diverged => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_and_tags_refuse_noise() {
        let repo = GithubRepo::of_pr("https://github.com/xrl/vm-runner/pull/5").unwrap();
        assert_eq!(
            (repo.to_string().as_str(), repo.name()),
            ("xrl/vm-runner", "vm-runner")
        );
        assert!(GithubRepo::of_pr("https://github.com/xrl/vm-runner/issues/5").is_none());
        for bad in ["vm-runner", "a/b/c", "/b", "a/"] {
            assert!(bad.parse::<GithubRepo>().is_err(), "{bad}");
        }
        for good in ["v0.7.0", "guest-v0.7.0", "release/1.0.0"] {
            assert!(good.parse::<TagName>().is_ok(), "{good}");
        }
        for bad in ["", "-v1", "a..b", "a b", "v1/", "a//b", ".v1"] {
            assert!(bad.parse::<TagName>().is_err(), "{bad}");
        }
        for legacy in [
            "release/.hidden/v1",
            "release/v1/.hidden",
            "release/part.lock/v1",
            "release/v1.lock",
            "v1.lock",
        ] {
            let parsed: TagName = legacy.parse().unwrap();
            let stored: TagName = serde_json::from_value(legacy.into()).unwrap();
            assert_eq!(stored, parsed);
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/publication_reads.rs"]
mod publication_reads;
