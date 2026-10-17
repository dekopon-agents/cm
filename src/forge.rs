use std::fmt;
use std::process::Command;

use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    Spawn,
    Exit(Option<i32>),
    Json,
    Url,
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForgeError::Spawn => f.write_str("could not execute gh; check installation"),
            ForgeError::Exit(Some(code)) => write!(f, "gh exited {code}; check access with gh"),
            ForgeError::Exit(None) => f.write_str("gh exited by signal"),
            ForgeError::Json => f.write_str("gh returned JSON cm cannot read"),
            ForgeError::Url => f.write_str("not a https://github.com/<owner>/<repo>/pull/<n> URL"),
        }
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
    pub merge_commit: Option<String>,
}

pub trait Forge {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError>;
    fn tag_exists(&self, repo: &str, tag: &str) -> Result<bool, ForgeError>;
    fn tag_contains(&self, repo: &str, tag: &str, sha: &str) -> Result<bool, ForgeError>;
}

pub struct Gh;

fn gh<T: DeserializeOwned>(args: &[&str]) -> Result<T, ForgeError> {
    let output = Command::new("gh")
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .map_err(|_| ForgeError::Spawn)?;
    if !output.status.success() {
        return Err(ForgeError::Exit(output.status.code()));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| ForgeError::Json)
}

impl Forge for Gh {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError> {
        #[derive(Deserialize)]
        struct Rest {
            state: String,
            merged: bool,
            merge_commit_sha: Option<String>,
        }
        let path = url
            .strip_prefix("https://github.com/")
            .and_then(|rest| rest.split_once("/pull/"))
            .map(|(repo, number)| format!("repos/{repo}/pulls/{number}"))
            .ok_or(ForgeError::Url)?;
        let rest: Rest = gh(&["api", &path])?;
        let state = match (rest.merged, rest.state.as_str()) {
            (true, _) => PullState::Merged,
            (false, "open") => PullState::Open,
            (false, "closed") => PullState::Closed,
            _ => return Err(ForgeError::Json),
        };
        Ok(Pull {
            state,
            merge_commit: rest.merge_commit_sha.filter(|_| state == PullState::Merged),
        })
    }

    fn tag_exists(&self, repo: &str, tag: &str) -> Result<bool, ForgeError> {
        #[derive(Deserialize)]
        struct Ref {
            #[serde(rename = "ref")]
            name: String,
        }
        let refs: Vec<Ref> = gh(&["api", &format!("repos/{repo}/git/matching-refs/tags/{tag}")])?;
        let wanted = format!("refs/tags/{tag}");
        Ok(refs.iter().any(|found| found.name == wanted))
    }

    fn tag_contains(&self, repo: &str, tag: &str, sha: &str) -> Result<bool, ForgeError> {
        #[derive(Deserialize)]
        struct Compare {
            status: String,
        }
        let compare: Compare = gh(&["api", &format!("repos/{repo}/compare/{sha}...{tag}")])?;
        Ok(matches!(compare.status.as_str(), "ahead" | "identical"))
    }
}
