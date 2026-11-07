use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use cm::forge::{CheckState, CommitSha, Forge, ForgeError, GithubRepo, Pull, TagName};

#[derive(Default)]
pub struct FakeForge {
    pub pulls: BTreeMap<String, Pull>,
    pub tags: BTreeMap<(String, String), BTreeSet<String>>,
    pub broken: BTreeSet<String>,
    pub calls: RefCell<usize>,
    pub containment_queries: RefCell<Vec<(String, String, String)>>,
}

impl Forge for FakeForge {
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
