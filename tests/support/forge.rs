use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use cm::forge::{
    CheckState, CommitSha, Forge, ForgeError, GithubRepo, ImageRef, Pull, Read, TagName,
};

#[derive(Default)]
pub struct FakeForge {
    pub root: Option<std::path::PathBuf>,
    pub crates: BTreeMap<(String, String), Read>,
    pub formulas: BTreeMap<(String, String), String>,
    pub images: BTreeMap<(String, String), Read>,
    pub artifact_queries: RefCell<Vec<String>>,
    pub pulls: BTreeMap<String, Pull>,
    pub tags: BTreeMap<(String, String), BTreeSet<String>>,
    pub broken: BTreeSet<String>,
    pub calls: RefCell<usize>,
    pub containment_queries: RefCell<Vec<(String, String, String)>>,
}

impl FakeForge {
    fn artifact_query(&self, key: &str) {
        if let Some(root) = &self.root {
            let lock = std::fs::OpenOptions::new()
                .write(true)
                .open(root.join(cm::store::CM_DIR).join(cm::store::LOCK))
                .unwrap();
            assert!(
                rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                    .is_ok(),
                "publication read held the campaign lock"
            );
        }
        self.artifact_queries.borrow_mut().push(key.to_owned());
    }
}

impl Forge for FakeForge {
    fn crate_version(&self, name: &str, version: &cm::state::Version) -> Result<Read, ForgeError> {
        let key = format!("crate:{name}");
        self.artifact_query(&key);
        if self.broken.contains(&key) {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(self
            .crates
            .get(&(name.into(), version.to_string()))
            .cloned()
            .unwrap_or(Read::Missing))
    }

    fn tap_formula(
        &self,
        tap: &GithubRepo,
        formula: &str,
        repo: &GithubRepo,
        tag: &TagName,
        version: &cm::state::Version,
    ) -> Result<Read, ForgeError> {
        let key = format!("tap:{tap}/{formula}");
        self.artifact_query(&key);
        if self.broken.contains(&key) {
            return Err(ForgeError::Exit(Some(1)));
        }
        match self.formulas.get(&(tap.to_string(), formula.into())) {
            Some(body) => cm::forge::parse_formula(body, "blob-sha", repo, tag, version),
            None => Ok(Read::Missing),
        }
    }

    fn image_manifest(&self, image: &ImageRef, tag: &str) -> Result<Read, ForgeError> {
        let key = format!("image:{image}");
        self.artifact_query(&key);
        if self.broken.contains(&key) {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(self
            .images
            .get(&(image.to_string(), tag.into()))
            .cloned()
            .unwrap_or(Read::Missing))
    }

    fn pull(&self, url: &str) -> Result<Pull, ForgeError> {
        *self.calls.borrow_mut() += 1;
        if self.broken.contains(url) {
            return Err(ForgeError::Exit(Some(1)));
        }
        self.pulls.get(url).cloned().ok_or(ForgeError::Json)
    }

    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError> {
        Ok(self.tags.contains_key(&(repo.to_string(), tag.to_string())))
    }

    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError> {
        self.containment_queries.borrow_mut().push((
            repo.to_string(),
            tag.to_string(),
            sha.to_string(),
        ));
        if self.broken.contains(tag.as_str()) {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(self
            .tags
            .get(&(repo.to_string(), tag.to_string()))
            .is_some_and(|shas| shas.contains(sha.as_str())))
    }

    fn checks(&self, _: &GithubRepo, _: &CommitSha) -> Result<Vec<CheckState>, ForgeError> {
        Err(ForgeError::Spawn)
    }
}
