use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::state::{CampaignState, Kind, UnitState};

pub const CM_DIR: &str = ".cm";
pub const STATE_FILE: &str = "state.json";
pub const JOURNAL: &str = "JOURNAL.md";
pub const JOURNAL_COPY: &str = ".cm/JOURNAL.md";
pub const UNIT_STATE: &str = "STATE.md";

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join(CM_DIR).join(STATE_FILE)
}

fn kind_of(dir: &Path) -> Option<Kind> {
    #[derive(serde::Deserialize)]
    struct Probe {
        kind: Kind,
    }
    let text = fs::read_to_string(state_path(dir)).ok()?;
    serde_json::from_str::<Probe>(&text)
        .ok()
        .map(|probe| probe.kind)
}

pub fn is_campaign(dir: &Path) -> bool {
    kind_of(dir) == Some(Kind::Campaign)
}

pub fn is_unit(dir: &Path) -> bool {
    kind_of(dir) == Some(Kind::Unit)
}

pub fn find_root(start: &Path) -> Option<PathBuf> {
    let start = absolute(start);
    start
        .ancestors()
        .find(|dir| is_campaign(dir))
        .map(Path::to_path_buf)
}

pub fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

pub fn root_or_cwd(root: Option<&Path>) -> Result<PathBuf> {
    let start = match root {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir()?,
    };
    match find_root(&start) {
        Some(found) => Ok(found),
        None => bail!(
            "no campaign folder at or above {}; run `cm init`",
            start.display()
        ),
    }
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write_atomic(path, text.as_bytes())
}

pub fn load_campaign(root: &Path) -> Result<CampaignState> {
    let state: CampaignState = read_json(&state_path(root))?;
    if state.kind != Kind::Campaign {
        bail!("{} is not a campaign state", state_path(root).display());
    }
    Ok(state)
}

pub fn load_unit(dir: &Path) -> Result<UnitState> {
    let state: UnitState = read_json(&state_path(dir))?;
    if state.kind != Kind::Unit {
        bail!("{} is not a unit state", state_path(dir).display());
    }
    Ok(state)
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .context("a state path has a file name")?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.cm-tmp"));
    {
        let mut file = File::create(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn create_new(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

pub struct Lock {
    _file: File,
}

pub fn lock(root: &Path) -> Result<Lock> {
    let dir = root.join(CM_DIR);
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .context("locking .cm/lock")?;
    Ok(Lock { _file: file })
}

pub fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub fn is_subcampaign_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'-'
        && is_name(&name[3..])
}

pub fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

#[derive(Debug, Clone)]
pub struct Target {
    pub unit: String,
    pub step: Option<String>,
}

pub fn parse_target(text: &str, with_step: bool) -> std::result::Result<Target, String> {
    let parts: Vec<&str> = text.trim_matches('/').split('/').collect();
    let shape = if with_step {
        "<NN-name>/<unit>/<step>"
    } else {
        "<NN-name>/<unit>[/<step>]"
    };
    let fits = match parts.len() {
        2 => !with_step,
        3 => true,
        _ => false,
    };
    if !fits || !is_subcampaign_name(parts[0]) || !parts[1..].iter().all(|part| is_name(part)) {
        return Err(format!("`{text}` is not {shape}"));
    }
    Ok(Target {
        unit: format!("{}/{}", parts[0], parts[1]),
        step: parts.get(2).map(|step| (*step).to_owned()),
    })
}
