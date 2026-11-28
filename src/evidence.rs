use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

pub(crate) fn exact_sha(sha: &str) -> bool {
    sha.len() == 40 && crate::state::is_commit(sha)
}

pub(crate) struct ReviewRange<'a> {
    pub pr: &'a str,
    pub base: &'a str,
    pub head: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(crate) enum RustComparison {
    PreExisting {
        evidence: CampaignFile,
        sha256: String,
        tests: BTreeSet<FailedTest>,
    },
    Refused {
        evidence: String,
        reason: &'static str,
        #[serde(skip_serializing_if = "BTreeSet::is_empty")]
        tests: BTreeSet<FailedTest>,
    },
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FailedTest {
    harness: String,
    name: String,
}

#[derive(Deserialize, PartialEq, Eq)]
struct Conditions {
    rustc_commit: String,
    cargo_commit: String,
    target: String,
    environment_sha256: String,
}

impl Conditions {
    fn pinned(&self) -> bool {
        exact_sha(&self.rustc_commit)
            && exact_sha(&self.cargo_commit)
            && !self.target.trim().is_empty()
            && self.environment_sha256.len() == 64
            && self
                .environment_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
    }
}

#[derive(Deserialize)]
struct TestRun {
    revision: String,
    conditions: Conditions,
    argv: Vec<String>,
    exit: Option<i32>,
    completed: bool,
    failed_tests: BTreeSet<FailedTest>,
}

impl TestRun {
    fn locked_test(&self) -> bool {
        let [cargo, test, args @ ..] = self.argv.as_slice() else {
            return false;
        };
        cargo == "cargo"
            && test == "test"
            && args
                .iter()
                .take_while(|arg| *arg != "--")
                .any(|arg| arg == "--locked")
    }

    fn named_failures(&self) -> bool {
        self.completed
            && match self.exit {
                Some(0) => self.failed_tests.is_empty(),
                Some(101) => {
                    !self.failed_tests.is_empty()
                        && self.failed_tests.iter().all(|test| {
                            !test.harness.trim().is_empty() && !test.name.trim().is_empty()
                        })
                }
                _ => false,
            }
    }
}

#[derive(Deserialize)]
struct ComparisonRuns {
    base: TestRun,
    head: TestRun,
}

#[derive(Deserialize)]
struct ComparisonEvidence {
    schema: u32,
    pr: String,
    primary: ComparisonRuns,
    #[serde(default)]
    reruns: Vec<ComparisonRuns>,
}

impl ComparisonEvidence {
    fn disposition(
        &self,
        range: ReviewRange<'_>,
    ) -> Result<BTreeSet<FailedTest>, (&'static str, BTreeSet<FailedTest>)> {
        let refuse = |reason| Err((reason, BTreeSet::new()));
        if self.schema != 1 {
            return refuse("comparison-schema");
        }
        if self.pr != range.pr {
            return refuse("comparison-pr");
        }
        if self.primary.head.exit == Some(0) {
            return refuse("head-test-passed");
        }

        let mut failed = BTreeSet::new();
        let mut matched = BTreeSet::new();
        let pairs = std::iter::once(&self.primary).chain(&self.reruns);
        for ComparisonRuns { base, head } in pairs {
            if base.revision != range.base || head.revision != range.head {
                return refuse("comparison-range");
            }
            if base.conditions != head.conditions
                || head.conditions != self.primary.head.conditions
                || !head.conditions.pinned()
                || base.argv != head.argv
                || !head.locked_test()
            {
                return refuse("comparison-conditions");
            }
            if !base.named_failures() || !head.named_failures() {
                return refuse("incomplete-test-results");
            }
            failed.extend(&head.failed_tests);
            matched.extend(base.failed_tests.intersection(&head.failed_tests));
        }

        let head_only: BTreeSet<_> = failed.difference(&matched).copied().cloned().collect();
        if !head_only.is_empty() {
            return Err(("head-only-failures", head_only));
        }
        Ok(failed.into_iter().cloned().collect())
    }
}

impl RustComparison {
    pub(crate) fn read(
        root: &Path,
        target: &str,
        part: Option<&str>,
        range: ReviewRange<'_>,
    ) -> Self {
        let head = range.head;
        let name = match part {
            Some(part) => format!("rust-comparison-{head}-{part}.json"),
            None => format!("rust-comparison-{head}.json"),
        };
        let path = Path::new(target).join("evidence").join(name);
        let refuse = |reason, tests| Self::Refused {
            evidence: path.display().to_string(),
            reason,
            tests,
        };
        let Ok(evidence) = CampaignFile::resolve(root, &root.join(&path)) else {
            return refuse("comparison-evidence-unavailable", BTreeSet::new());
        };
        let Ok(bytes) = fs::read(root.join(evidence.as_str())) else {
            return refuse("comparison-evidence-unavailable", BTreeSet::new());
        };
        let Ok(comparison) = serde_json::from_slice::<ComparisonEvidence>(&bytes) else {
            return refuse("comparison-evidence-unreadable", BTreeSet::new());
        };
        match comparison.disposition(range) {
            Ok(tests) => Self::PreExisting {
                evidence,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                tests,
            },
            Err((reason, tests)) => refuse(reason, tests),
        }
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
