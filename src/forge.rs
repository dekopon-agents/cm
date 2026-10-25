use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    Spawn,
    Exit(Option<i32>),
    Timeout(Duration),
    Json,
    Url,
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForgeError::Spawn => f.write_str("could not execute gh; check installation"),
            ForgeError::Exit(Some(code)) => write!(f, "gh exited {code}; check access with gh"),
            ForgeError::Exit(None) => f.write_str("gh exited by signal"),
            ForgeError::Timeout(after) => {
                write!(
                    f,
                    "gh did not answer within {}s; killed it",
                    after.as_secs()
                )
            }
            ForgeError::Json => f.write_str("gh returned JSON cm cannot read"),
            ForgeError::Url => f.write_str("not a https://github.com/<owner>/<repo>/pull/<n> URL"),
        }
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
    /// The PR's head commit, as GitHub reports it.
    pub head: Option<CommitSha>,
    /// GitHub's `mergeable_state` (`clean`, `dirty`, `blocked`, `unknown`, …), lower case.
    pub mergeable_state: Option<String>,
}

/// One check run or commit status on a commit, its state lower case: a check run's conclusion
/// once completed (`success`, `failure`, `skipped`, …), else its status; a status's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckState {
    pub name: String,
    pub state: String,
}

impl CheckState {
    pub fn green(&self) -> bool {
        matches!(self.state.as_str(), "success" | "neutral" | "skipped")
    }
}

/// CI is green on a commit when it reports at least one check and every one is green.
pub fn ci_green(checks: &[CheckState]) -> bool {
    !checks.is_empty() && checks.iter().all(CheckState::green)
}

pub trait Forge {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError>;
    /// Every check run and commit status GitHub reports on `sha`.
    fn checks(&self, repo: &GithubRepo, sha: &CommitSha) -> Result<Vec<CheckState>, ForgeError>;
    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError>;
    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError>;
}

pub const GH_TIMEOUT: Duration = Duration::from_secs(60);

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
            let (total, items) =
                split(self.json(&["api", &format!("{path}?per_page=100&page={page}")])?);
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
        let mut child = Command::new(&self.program)
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ForgeError::Spawn)?;
        let mut stdout = child.stdout.take().ok_or(ForgeError::Spawn)?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ForgeError::Timeout(self.timeout));
                }
            }
        };
        let stdout = reader
            .join()
            .map_err(|_| ForgeError::Json)?
            .map_err(|_| ForgeError::Json)?;
        if !status.success() {
            return Err(ForgeError::Exit(status.code()));
        }
        serde_json::from_slice(&stdout).map_err(|_| ForgeError::Json)
    }
}

impl Forge for Gh {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError> {
        #[derive(Deserialize)]
        struct Rest {
            state: String,
            merged: bool,
            merge_commit_sha: Option<String>,
            #[serde(default)]
            head: Option<Head>,
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
            head: rest.head.map(|head| CommitSha::new(head.sha)),
            mergeable_state: rest.mergeable_state.map(|state| state.to_lowercase()),
        })
    }

    fn checks(&self, repo: &GithubRepo, sha: &CommitSha) -> Result<Vec<CheckState>, ForgeError> {
        #[derive(Deserialize)]
        struct Runs {
            total_count: usize,
            check_runs: Vec<Run>,
        }
        #[derive(Deserialize)]
        struct Run {
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
            context: String,
            state: String,
        }
        let runs = self.pages(
            &format!("repos/{repo}/commits/{sha}/check-runs"),
            |page: Runs| (page.total_count, page.check_runs),
        )?;
        let statuses = self.pages(
            &format!("repos/{repo}/commits/{sha}/status"),
            |page: Combined| (page.total_count, page.statuses),
        )?;
        let runs = runs.into_iter().map(|run| CheckState {
            state: match (run.status.as_str(), run.conclusion) {
                ("completed", Some(conclusion)) => conclusion.to_lowercase(),
                (status, _) => status.to_lowercase(),
            },
            name: run.name,
        });
        let statuses = statuses.into_iter().map(|status| CheckState {
            name: status.context,
            state: status.state.to_lowercase(),
        });
        Ok(runs.chain(statuses).collect())
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
    }
}
