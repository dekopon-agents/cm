use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::state::{CampaignState, Kind, SCHEMA, UnitState};

pub const CM_DIR: &str = ".cm";
pub const STATE_FILE: &str = "state.json";
pub const JOURNAL: &str = "JOURNAL.md";
pub const LOCK: &str = "lock";
pub const LEGACY_JOURNAL_COPY: &str = "JOURNAL.md";
pub const UNIT_STATE: &str = "STATE.md";

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join(CM_DIR).join(STATE_FILE)
}

fn kind_of(dir: &Path) -> Result<Option<Kind>> {
    #[derive(serde::Deserialize)]
    struct Probe {
        kind: Kind,
    }
    let path = state_path(dir);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let probe: Probe =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(probe.kind))
}

pub fn is_campaign(dir: &Path) -> bool {
    matches!(kind_of(dir), Ok(Some(Kind::Campaign)))
}

pub fn is_unit(dir: &Path) -> bool {
    matches!(kind_of(dir), Ok(Some(Kind::Unit)))
}

fn discover_root(start: &Path, strict: bool) -> Result<Option<PathBuf>> {
    let start = absolute(start);
    for dir in start.ancestors() {
        match kind_of(dir) {
            Ok(Some(Kind::Campaign)) => return Ok(Some(dir.to_path_buf())),
            Err(error) if strict => return Err(error),
            _ => {}
        }
    }
    Ok(None)
}

pub fn find_root(start: &Path) -> Option<PathBuf> {
    discover_root(start, false).ok().flatten()
}

#[derive(Debug)]
pub(crate) struct NoCampaign(PathBuf);

impl std::fmt::Display for NoCampaign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no campaign folder at or above {}; run `cm init`",
            self.0.display()
        )
    }
}

impl std::error::Error for NoCampaign {}

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
    match discover_root(&start, true)? {
        Some(found) => {
            load_campaign(&found)?;
            Ok(found)
        }
        None => Err(NoCampaign(start).into()),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewerSchema {
    pub path: PathBuf,
    pub found: u32,
}

impl std::fmt::Display for NewerSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} has schema {}, newer than the schema {SCHEMA} this cm {} reads; upgrade cm",
            self.path.display(),
            self.found,
            env!("CARGO_PKG_VERSION")
        )
    }
}

impl std::error::Error for NewerSchema {}

fn read_state<T: DeserializeOwned>(path: &Path) -> Result<T> {
    #[derive(serde::Deserialize)]
    struct Probe {
        schema: u32,
    }
    let probe: Probe = read_json(path)?;
    if probe.schema > SCHEMA {
        return Err(NewerSchema {
            path: path.to_path_buf(),
            found: probe.schema,
        }
        .into());
    }
    read_json(path)
}

pub fn load_campaign(root: &Path) -> Result<CampaignState> {
    let state: CampaignState = read_state(&state_path(root))?;
    if state.kind != Kind::Campaign {
        bail!("{} is not a campaign state", state_path(root).display());
    }
    Ok(state)
}

pub fn load_unit(dir: &Path) -> Result<UnitState> {
    let state: UnitState = read_state(&state_path(dir))?;
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
    create_new_with(path, |file| file.write_all(bytes))
}

fn create_new_with(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut temporary = builder.tempfile_in(parent)?;
    write(temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

pub struct Lock {
    _file: File,
}

pub fn lock(root: &Path) -> Result<Lock> {
    let dir = root.join(CM_DIR);
    fs::create_dir_all(&dir)?;
    create_new(
        &dir.join(".gitignore"),
        format!("{LOCK}\n{LEGACY_JOURNAL_COPY}\n").as_bytes(),
    )?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
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

#[cfg(test)]
#[path = "../tests/support/store.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interrupted_creation_leaves_no_destination_and_retries_in_full() {
        let (_temp, path) = fixture::destination();
        let error = create_new_with(&path, |file| {
            file.write_all(b"Partial")?;
            assert!(!path.exists());
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "interrupted write",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!path.exists());
        assert!(create_new(&path, b"Complete kickoff.\n").unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"Complete kickoff.\n");
    }

    #[test]
    fn publishing_a_completed_file_keeps_a_destination_created_during_the_write() {
        let (_temp, path) = fixture::destination();
        let written = create_new_with(&path, |file| {
            file.write_all(b"New kickoff.\n")?;
            fs::write(&path, b"Kept kickoff.\n")
        })
        .unwrap();
        assert!(!written);
        assert_eq!(fs::read(&path).unwrap(), b"Kept kickoff.\n");
    }
}
