use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::CM_DIR;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CampaignFile(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    Missing(PathBuf),
    Outside { path: PathBuf, root: PathBuf },
    Runtime(PathBuf),
    NotFile(PathBuf),
    Empty(PathBuf),
    NotUtf8(PathBuf),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileError::Missing(path) => write!(f, "`{}` does not exist", path.display()),
            FileError::Outside { path, root } => write!(
                f,
                "`{}` is not inside the campaign folder {}",
                path.display(),
                root.display()
            ),
            FileError::Runtime(path) => write!(
                f,
                "`{}` is in a `{CM_DIR}/` folder, which holds cm's own state, never evidence",
                path.display()
            ),
            FileError::NotFile(path) => {
                write!(f, "`{}` is not a regular file", path.display())
            }
            FileError::Empty(path) => write!(f, "`{}` is empty", path.display()),
            FileError::NotUtf8(path) => {
                write!(f, "`{}` is not a UTF-8 path", path.display())
            }
        }
    }
}

impl std::error::Error for FileError {}

impl CampaignFile {
    pub fn resolve(root: &Path, path: &Path) -> Result<CampaignFile, FileError> {
        let (Ok(found), Ok(campaign)) = (path.canonicalize(), root.canonicalize()) else {
            return Err(FileError::Missing(path.to_path_buf()));
        };
        let outside = || FileError::Outside {
            path: path.to_path_buf(),
            root: root.to_path_buf(),
        };
        let rel = found.strip_prefix(&campaign).map_err(|_| outside())?;
        if rel.as_os_str().is_empty() {
            return Err(outside());
        }
        if rel
            .components()
            .any(|part| part == Component::Normal(CM_DIR.as_ref()))
        {
            return Err(FileError::Runtime(path.to_path_buf()));
        }
        let metadata = fs::metadata(&found).map_err(|_| FileError::Missing(path.to_path_buf()))?;
        if !metadata.is_file() {
            return Err(FileError::NotFile(path.to_path_buf()));
        }
        if metadata.len() == 0 {
            return Err(FileError::Empty(path.to_path_buf()));
        }
        rel.to_str()
            .map(|rel| CampaignFile(rel.to_owned()))
            .ok_or_else(|| FileError::NotUtf8(path.to_path_buf()))
    }

    pub fn check(&self, root: &Path) -> Result<(), FileError> {
        CampaignFile::resolve(root, &root.join(&self.0)).map(|_| ())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CampaignFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_is_a_nonempty_regular_file_in_the_campaign() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaign");
        fs::create_dir_all(root.join(".cm")).unwrap();
        fs::create_dir_all(root.join("01-a/u/notes")).unwrap();
        fs::write(root.join(".cm/state.json"), "{}").unwrap();
        fs::write(root.join("01-a/u/empty.md"), "").unwrap();
        fs::write(root.join("01-a/u/notes/found.md"), "Found.\n").unwrap();
        fs::write(temp.path().join("outside.md"), "x").unwrap();
        let resolve = |rel: &str| CampaignFile::resolve(&root, &root.join(rel));
        assert_eq!(
            resolve("01-a/u/notes/found.md").unwrap().as_str(),
            "01-a/u/notes/found.md"
        );
        assert!(matches!(
            resolve("01-a/u/notes"),
            Err(FileError::NotFile(_))
        ));
        assert!(matches!(
            resolve("01-a/u/empty.md"),
            Err(FileError::Empty(_))
        ));
        assert!(matches!(
            resolve(".cm/state.json"),
            Err(FileError::Runtime(_))
        ));
        assert!(matches!(resolve("nope.md"), Err(FileError::Missing(_))));
        assert!(matches!(resolve("."), Err(FileError::Outside { .. })));
        assert!(matches!(
            resolve("../outside.md"),
            Err(FileError::Outside { .. })
        ));
    }
}
