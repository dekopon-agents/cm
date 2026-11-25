use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use cm::forge::{
    CheckState, CommitSha, Forge, ForgeError, GithubRepo, ImageRef, Pull, Read, TagName,
};

#[derive(Default)]
pub struct FakeForge {
    pub root: Option<std::path::PathBuf>,
    pub crates: BTreeMap<(String, String), Read>,
    pub formulas: BTreeMap<(String, String), String>,
    pub images: BTreeMap<(String, String), Read>,
    pub artifact_queries: Mutex<Vec<String>>,
    pub pulls: BTreeMap<String, Pull>,
    pub tags: BTreeMap<(String, String), BTreeSet<String>>,
    pub broken: BTreeSet<String>,
    pub calls: Mutex<usize>,
    pub containment_queries: Mutex<Vec<(String, String, String)>>,
}

// A child another test thread forks shares every lock this process holds until it execs, so a
// busy lock is no evidence; but only `store::lock` opens the lock file, and keeps it open while it
// holds the lock, so a descriptor of this process open on that file is.
pub fn lock_holder(root: &Path) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let path = root.join(cm::store::CM_DIR).join(cm::store::LOCK);
    let lock = std::fs::metadata(&path)
        .unwrap_or_else(|error| panic!("no lock file to watch at {}: {error}", path.display()));
    // A descriptor closed between the listing and its stat is gone; a holder blocked on this
    // read cannot close its own.
    std::fs::read_dir("/dev/fd")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|fd| match std::fs::metadata(fd) {
            Ok(open) => (open.dev(), open.ino()) == (lock.dev(), lock.ino()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("{}: {error}", fd.display()),
        })
}

impl FakeForge {
    #[allow(dead_code)]
    pub fn twin(&self, root: &Path) -> Self {
        FakeForge {
            root: self.root.as_ref().map(|_| root.to_path_buf()),
            crates: self.crates.clone(),
            formulas: self.formulas.clone(),
            images: self.images.clone(),
            artifact_queries: Mutex::default(),
            pulls: self.pulls.clone(),
            tags: self.tags.clone(),
            broken: self.broken.clone(),
            calls: Mutex::default(),
            containment_queries: Mutex::default(),
        }
    }

    fn artifact_query(&self, key: &str) {
        if let Some(root) = &self.root {
            assert_eq!(
                lock_holder(root),
                None,
                "publication read held the campaign lock"
            );
        }
        self.artifact_queries.lock().unwrap().push(key.to_owned());
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
        *self.calls.lock().unwrap() += 1;
        if self.broken.contains(url) {
            return Err(ForgeError::Exit(Some(1)));
        }
        self.pulls.get(url).cloned().ok_or(ForgeError::Json)
    }

    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError> {
        if self.broken.contains(&format!("exists:{tag}")) {
            return Err(ForgeError::Exit(Some(1)));
        }
        Ok(self.tags.contains_key(&(repo.to_string(), tag.to_string())))
    }

    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError> {
        self.containment_queries.lock().unwrap().push((
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
