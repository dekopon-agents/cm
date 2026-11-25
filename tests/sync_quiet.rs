use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use cm::commands::{self, Declaration, Ships};
use cm::forge::{CheckState, CommitSha, Forge, ForgeError, GithubRepo, Pull, PullState, TagName};
use cm::host::Host;
use cm::outcome::Outcome;
use cm::state::{Evidence, Lane, Phase, Work};
use cm::store::{self, parse_target};
use cm::sync::{BUDGET, Reader, sync, sync_with};
use jiff::Timestamp;

mod support;

#[path = "support/campaign.rs"]
mod campaign_fixture;
use campaign_fixture::{Campaign, campaign, git};

#[allow(dead_code)]
#[path = "support/forge.rs"]
mod forge_fixture;

#[allow(dead_code)]
#[path = "support/publication.rs"]
mod publication_fixture;

struct Fake {
    now: Timestamp,
}

impl Host for Fake {
    fn now(&self) -> Timestamp {
        self.now
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(100 << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(0)
    }
}

fn at(now: &str) -> Fake {
    Fake {
        now: now.parse().unwrap(),
    }
}

const OPEN_PR: &str = "https://github.com/o/r/pull/1";
const MERGED_PR: &str = "https://github.com/o/r/pull/2";

// Like GitHub, an unchanged pull answers its stored validator with a 304.
#[derive(Default)]
struct Github {
    pulls: Mutex<BTreeMap<String, Pull>>,
    tags: BTreeMap<(String, String), BTreeSet<String>>,
    reads: Mutex<Vec<String>>,
}

impl Github {
    fn log(&self, read: String) {
        self.reads.lock().unwrap().push(read);
    }

    fn set(&self, url: &str, pull: Pull) {
        self.pulls.lock().unwrap().insert(url.to_owned(), pull);
    }

    fn reads(&self) -> Vec<String> {
        let mut reads = self.reads.lock().unwrap().clone();
        reads.sort();
        reads
    }
}

impl Forge for Github {
    fn pull(&self, url: &str) -> Result<Pull, ForgeError> {
        self.log(format!("pull {url}"));
        self.pulls
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or(ForgeError::Json)
    }

    fn pull_conditional(
        &self,
        url: &str,
        previous: &BTreeMap<String, String>,
        etags: &mut BTreeMap<String, String>,
    ) -> Result<Option<Pull>, ForgeError> {
        let pull = self.pull(url)?;
        let etag = format!("\"{pull:?}\"");
        let unchanged = previous.get(url) == Some(&etag);
        etags.insert(url.to_owned(), etag);
        Ok((!unchanged).then_some(pull))
    }

    fn checks(&self, _: &GithubRepo, sha: &CommitSha) -> Result<Vec<CheckState>, ForgeError> {
        self.log(format!("checks {sha}"));
        Ok(vec![CheckState {
            name: "ci".into(),
            state: "success".to_owned().into(),
        }])
    }

    fn rate_limit(&self) -> Option<String> {
        Some("GitHub REST core: 1/5000 used (reset 1)".into())
    }

    fn tag_exists(&self, repo: &GithubRepo, tag: &TagName) -> Result<bool, ForgeError> {
        self.log(format!("tag {repo} {tag}"));
        Ok(self.tags.contains_key(&(repo.to_string(), tag.to_string())))
    }

    fn tag_contains(
        &self,
        repo: &GithubRepo,
        tag: &TagName,
        sha: &CommitSha,
    ) -> Result<bool, ForgeError> {
        self.log(format!("contains {repo} {tag} {sha}"));
        Ok(self
            .tags
            .get(&(repo.to_string(), tag.to_string()))
            .is_some_and(|shas| shas.contains(sha.as_str())))
    }
}

fn open(head: &str) -> Pull {
    Pull {
        state: PullState::Open,
        merge_commit: None,
        merged_at: None,
        head: Some(CommitSha::new(head)),
        head_ref: None,
        base: Some(CommitSha::new("b".repeat(40))),
        mergeable_state: Some("clean".to_owned().into()),
    }
}

fn merged(sha: &str) -> Pull {
    Pull {
        state: PullState::Merged,
        merge_commit: Some(CommitSha::new(sha)),
        merged_at: Some("2026-10-17T07:30:00Z".into()),
        head: None,
        head_ref: None,
        base: None,
        mergeable_state: None,
    }
}

fn declare(host: &Fake, c: &Campaign, target: &str, after: &[&str], ships: Ships) {
    let declaration = Declaration {
        lane: Some(Lane::Pi),
        after: after.iter().map(|text| (*text).to_owned()).collect(),
        ships,
        artifacts: Vec::new(),
        parts: Vec::new(),
        work: Work::Code,
        kickoff: None,
    };
    let declared = commands::declare(host, &c.root.join(target), &declaration).unwrap();
    assert!(declared.ok(), "{:?}", declared.violations);
    fs::write(c.root.join(target).join("kickoff.md"), "Go.\n").unwrap();
}

fn advance(host: &Fake, c: &Campaign, target: &str, to: Phase, evidence: Evidence, repo: &Path) {
    let moved = commands::advance(
        host,
        &c.root,
        &parse_target(target, false).unwrap(),
        &to,
        &evidence,
        None,
        Some(repo),
    )
    .unwrap();
    assert!(moved.ok(), "{:?}", moved.violations);
}

fn open_pr(host: &Fake, c: &Campaign, worktree: &Path, target: &str, pr: &str) -> String {
    let launched = commands::launch(
        host,
        &c.root,
        &parse_target(target, true).unwrap(),
        Some(worktree),
    )
    .unwrap();
    assert!(launched.ok(), "{:?}", launched.violations);
    git(worktree, &["commit", "-q", "--allow-empty", "-m", target]);
    let head = git(worktree, &["rev-parse", "HEAD"]);
    let evidence = Evidence {
        pr: Some(pr.into()),
        sha: Some(head.clone()),
        ..Evidence::default()
    };
    advance(host, c, target, Phase::PrOpen, evidence, worktree);
    head
}

fn state(c: &Campaign, target: &str) -> Phase {
    let (unit, step) = target.rsplit_once('/').unwrap();
    store::load_unit(&c.root.join(unit)).unwrap().steps[step]
        .track
        .state
        .clone()
}

fn epoch() -> (Campaign, Github, String) {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    let worktree = c.root.parent().unwrap().join("wt");
    fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "-q"]);
    git(
        &worktree,
        &["commit", "-q", "--allow-empty", "-m", "release"],
    );
    git(&worktree, &["tag", "v1.0.0"]);
    let released = git(&worktree, &["rev-parse", "HEAD"]);
    fs::write(
        c.root.join("EPOCH.md"),
        "---\nstatus: WORKING\nskill_revision: synthetic-skill\napproved_plan_revision: synthetic-plan\n---\n",
    )
    .unwrap();

    declare(
        &host,
        &c,
        "00-release/r/1.0.0",
        &[],
        Ships {
            github: Some("o/r".parse().unwrap()),
            tag: Some("v{version}".into()),
        },
    );
    advance(
        &host,
        &c,
        "00-release/r/1.0.0",
        Phase::Released,
        Evidence {
            sha: Some(released.clone()),
            tag: Some("v1.0.0".into()),
            ..Evidence::default()
        },
        &worktree,
    );
    declare(&host, &c, "01-a/u/OPEN", &[], Ships::default());
    declare(&host, &c, "01-a/u/MERGED", &[], Ships::default());
    declare(
        &host,
        &c,
        "02-b/w/LATER",
        &["01-a/u/OPEN"],
        Ships::default(),
    );
    declare(&host, &c, "02-b/x/REVIEW", &[], Ships::default());
    let head = open_pr(&host, &c, &worktree, "01-a/u/OPEN", OPEN_PR);
    open_pr(&host, &c, &worktree, "01-a/u/MERGED", MERGED_PR);
    let dir = c.root.join("02-b/x");
    let mut unit = store::load_unit(&dir).unwrap();
    let review = unit.steps.get_mut("REVIEW").unwrap();
    review.track.state = Phase::Blocked("REVIEW_BLOCKED".into());
    review.track.blocked_from = Some(Phase::PrOpen);
    review.track.since = "2026-10-17T07:00Z".into();
    review.track.evidence.pr = Some("https://github.com/o/r/pull/3".into());
    review.track.evidence.sha = Some("a".repeat(40));
    commands::write_unit(&dir, &unit).unwrap();

    let mut forge = Github::default();
    forge
        .tags
        .insert(("o/r".into(), "v1.0.0".into()), [released].into());
    forge.set(OPEN_PR, open(&head));
    forge.set(MERGED_PR, merged(&"c".repeat(40)));
    let moved = sync(&host, &forge, &c.root).unwrap();
    assert!(moved.ok(), "{:?}", moved.violations);
    assert_eq!(state(&c, "01-a/u/MERGED"), Phase::Merged);
    commit(&c);
    (c, forge, head)
}

fn commit(c: &Campaign) {
    git(&c.root, &["add", "-A"]);
    git(&c.root, &["commit", "-q", "-m", "sync"]);
}

fn status(c: &Campaign) -> Vec<String> {
    let changed: BTreeSet<String> = git(
        &c.root,
        &[
            "ls-files",
            "--modified",
            "--deleted",
            "--others",
            "--exclude-standard",
        ],
    )
    .lines()
    .map(str::to_owned)
    .collect();
    changed.into_iter().collect()
}

const RUNTIME: [&str; 2] = [".cm/lock", ".cm/pulls-read.json"];

fn files(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(root).unwrap().to_owned();
            if name == Path::new(".git") || RUNTIME.iter().any(|runtime| name == Path::new(runtime))
            {
                continue;
            }
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                out.insert(name, (fs::read(&path).unwrap(), modified));
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

// So that any rewrite, even of the same bytes, shows in the mtime.
fn backdate(root: &Path) {
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for path in files(root).keys() {
        fs::File::options()
            .write(true)
            .open(root.join(path))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
}

fn quiet(outcome: &Outcome) {
    assert!(outcome.ok(), "{:?}", outcome.violations);
    assert_eq!(outcome.fields["moves"], serde_json::json!([]));
    assert!(
        outcome.fields.get("journal").is_none(),
        "{:?}",
        outcome.lines
    );
    assert_eq!(outcome.fields["card_written"], false);
}

#[test]
fn a_second_sync_leaves_git_status_clean() {
    let (c, forge, _) = epoch();
    assert_eq!(status(&c), Vec::<String>::new());
    // The time of each full PR read lives in an ignored file; the tracked observation does not
    // change with it.
    assert_eq!(
        fs::read_to_string(c.root.join(".cm/.gitignore")).unwrap(),
        "lock\nJOURNAL.md\npulls-read.json\n"
    );
    for runtime in RUNTIME {
        git(&c.root, &["check-ignore", "-q", runtime]);
    }
    let memo = c.root.join(".cm/pulls-read.json");
    let reads = |now: &str, reader| {
        forge.reads.lock().unwrap().clear();
        quiet(&sync_with(&at(now), reader, &c.root, BUDGET).unwrap());
        assert_eq!(status(&c), Vec::<String>::new(), "after the sync at {now}");
        forge.reads()
    };
    // Within 30 minutes of the full read at 08:00 the PR answers 304, which keeps that read's
    // time; an hour later it is read in full.
    let read_at = |memo: &Path| fs::read_to_string(memo).unwrap();
    let before = read_at(&memo);
    assert!(before.contains("\"at\": \"2026-10-17T08:00Z\""), "{before}");
    for now in ["2026-10-17T08:10:00Z", "2026-10-17T08:20:00Z"] {
        reads(now, Reader::Concurrent(&forge));
        assert_eq!(read_at(&memo), before, "a 304 at {now} is not a full read");
    }
    assert_eq!(
        reads("2026-10-17T09:00:00Z", Reader::Serial(&forge)),
        [
            format!("contains o/r v1.0.0 {}", "c".repeat(40)),
            format!("pull {OPEN_PR}"),
        ]
    );
    assert!(read_at(&memo).contains("\"at\": \"2026-10-17T09:00Z\""));
    // A fresh clone has no read times: it reads every PR in full once.
    fs::remove_file(&memo).unwrap();
    reads("2026-10-17T09:01:00Z", Reader::Serial(&forge));
    assert!(memo.exists());
}

#[test]
fn render_on_change_writes_no_file_and_no_journal_entry() {
    let (c, forge, _) = epoch();
    backdate(&c.root);
    let before = files(&c.root);
    for now in ["2026-10-17T08:05:00Z", "2026-10-18T12:00:00Z"] {
        quiet(&sync(&at(now), &forge, &c.root).unwrap());
        let after = files(&c.root);
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "a sync at {now} created or removed a file"
        );
        for (path, (bytes, modified)) in &before {
            assert_eq!(&after[path].0, bytes, "{} changed at {now}", path.display());
            assert_eq!(
                &after[path].1,
                modified,
                "{} was rewritten at {now}",
                path.display()
            );
        }
    }
}

#[test]
fn a_real_change_writes_exactly_the_views_it_affects() {
    let (c, forge, _) = epoch();
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    forge.set(OPEN_PR, merged(&"d".repeat(40)));
    let moved = sync(&at("2026-10-17T10:00:00Z"), &forge, &c.root).unwrap();
    assert!(moved.ok(), "{:?}", moved.violations);
    assert_eq!(state(&c, "01-a/u/OPEN"), Phase::Merged);
    assert_eq!(moved.fields["card_written"], true);
    assert_eq!(
        status(&c),
        [
            ".cm/pulls.json",
            "01-a/u/.cm/state.json",
            "01-a/u/STATE.md",
            "CARD.md",
            "JOURNAL.md",
            "RESUME.md",
        ]
    );
    let added = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let entries: Vec<_> = added
        .strip_prefix(journal.as_str())
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("## "))
        .collect();
    assert_eq!(
        entries,
        ["## 2026-10-17T10:00Z — cm sync: 1 move(s) from GitHub"]
    );
}

#[test]
fn every_view_renders_the_same_bytes_for_the_same_state() {
    let (c, forge, _) = epoch();
    let units = cm::plan::load_units(&c.root).unwrap();
    let next = cm::plan::summary(&c.root, &units);
    let card = |now: &str| cm::render::card(&c.root, at(now).now, "campaign", &units, &next);
    let rendered = card("2026-10-17T08:00:00Z");
    for now in [
        "2026-10-17T08:00:00Z",
        "2026-10-17T08:59:00Z",
        "2026-10-17T11:30:00Z",
    ] {
        assert_eq!(card(now), rendered, "CARD.md rendered at {now}");
    }
    assert!(rendered.starts_with("# campaign card\n\n"), "{rendered}");
    assert!(
        rendered.contains(
            "reviewer blocked 02-b/x/REVIEW: REVIEW_BLOCKED, since 2026-10-17T07:00Z (head "
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("waited"), "{rendered}");
    assert!(!rendered.contains("Last run"), "{rendered}");
    assert!(!rendered.contains("GitHub REST core"), "{rendered}");
    assert!(!rendered.contains("Moved this run"), "{rendered}");
    for (name, unit) in &units {
        let state = cm::render::unit(unit);
        assert_eq!(cm::render::unit(unit), state, "{name}/STATE.md");
        assert_eq!(
            fs::read_to_string(c.root.join(name).join(store::UNIT_STATE)).unwrap(),
            state,
            "{name}/STATE.md on disk"
        );
    }

    // RESUME.md's status and the card a sync writes are the same at any later time.
    let later = files(&c.root);
    quiet(&sync(&at("2026-10-17T13:00:00Z"), &forge, &c.root).unwrap());
    for name in ["CARD.md", "RESUME.md"] {
        assert_eq!(
            fs::read(c.root.join(name)).unwrap(),
            later[Path::new(name)].0,
            "{name}"
        );
    }
}

#[test]
fn a_concurrent_sync_makes_the_reads_and_writes_of_a_serial_one() {
    let (c, forge, _) = epoch();
    let copy = tempfile::tempdir().unwrap();
    let twin = copy.path().join("campaign");
    let copied = Command::new("cp")
        .arg("-a")
        .arg(&c.root)
        .arg(&twin)
        .status()
        .unwrap();
    assert!(copied.success());
    forge.set(OPEN_PR, merged(&"d".repeat(40)));
    let host = at("2026-10-17T10:00:00Z");
    forge.reads.lock().unwrap().clear();
    let serial = sync(&host, &forge, &c.root).unwrap();
    let serial_reads = forge.reads();
    forge.reads.lock().unwrap().clear();
    let concurrent = sync_with(&host, Reader::Concurrent(&forge), &twin, BUDGET).unwrap();
    assert_eq!(forge.reads(), serial_reads);
    assert_eq!(concurrent.violations, serial.violations);
    assert_eq!(concurrent.fields["moves"], serial.fields["moves"]);
    let strip = |files: BTreeMap<PathBuf, (Vec<u8>, SystemTime)>| {
        files
            .into_iter()
            .map(|(path, (bytes, _))| (path, bytes))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(strip(files(&twin)), strip(files(&c.root)));
}

fn heads(c: &Campaign) -> (Option<String>, Option<String>) {
    let pulls: BTreeMap<String, cm::plan::Observed> =
        store::read_json(&cm::plan::pulls_path(&c.root)).unwrap();
    let unit = store::load_unit(&c.root.join("01-a/u")).unwrap();
    (
        pulls.get(OPEN_PR).and_then(|seen| seen.head.clone()),
        unit.steps["OPEN"].track.evidence.sha.clone(),
    )
}

#[test]
fn a_read_memo_ahead_of_a_rewound_pulls_json_sends_no_validator() {
    let (c, forge, _) = epoch();
    let moved = "f".repeat(40);
    forge.set(OPEN_PR, open(&moved));
    let pushed = sync(&at("2026-10-17T08:05:00Z"), &forge, &c.root).unwrap();
    assert!(pushed.ok(), "{:?}", pushed.violations);
    assert_eq!(heads(&c), (Some(moved.clone()), Some(moved.clone())));
    commit(&c);
    // A checkout rewinds the tracked observation; the ignored read memo stays at the new head,
    // whose ETag GitHub would answer with a 304.
    git(&c.root, &["reset", "-q", "--hard", "HEAD~1"]);
    assert_ne!(heads(&c).0, Some(moved.clone()));
    let synced = sync(&at("2026-10-17T08:10:00Z"), &forge, &c.root).unwrap();
    assert!(synced.ok(), "{:?}", synced.violations);
    assert_eq!(heads(&c), (Some(moved.clone()), Some(moved)));
}

#[test]
fn a_read_memo_written_without_its_pulls_json_sends_no_validator() {
    let (c, forge, _) = epoch();
    let moved = "f".repeat(40);
    forge.set(OPEN_PR, open(&moved));
    let pushed = sync(&at("2026-10-17T08:05:00Z"), &forge, &c.root).unwrap();
    assert!(pushed.ok(), "{:?}", pushed.violations);
    // As if the sync stopped after the read memo and before `.cm/pulls.json`.
    git(&c.root, &["checkout", "-q", "--", ".cm/pulls.json"]);
    let journal = fs::read(c.root.join("JOURNAL.md")).unwrap();
    let synced = sync(&at("2026-10-17T08:10:00Z"), &forge, &c.root).unwrap();
    assert!(synced.ok(), "{:?}", synced.violations);
    assert!(synced.fields.get("journal").is_none(), "{:?}", synced.lines);
    assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), journal);
    assert_eq!(heads(&c), (Some(moved.clone()), Some(moved)));
}

#[test]
fn an_unknown_mergeable_state_keeps_the_stored_one() {
    let (c, forge, head) = epoch();
    let unknown = Pull {
        mergeable_state: Some("unknown".to_owned().into()),
        ..open(&head)
    };
    for (now, pull) in [
        ("2026-10-17T09:00:00Z", unknown.clone()),
        ("2026-10-17T09:05:00Z", unknown),
        ("2026-10-17T09:10:00Z", open(&head)),
        ("2026-10-17T10:00:00Z", open(&head)),
    ] {
        forge.set(OPEN_PR, pull);
        quiet(&sync(&at(now), &forge, &c.root).unwrap());
        assert_eq!(status(&c), Vec::<String>::new(), "after the sync at {now}");
    }
}

#[test]
fn a_failing_sync_reports_each_phase_and_a_quiet_one_does_not() {
    let (c, forge, _) = epoch();
    let quietly = sync(&at("2026-10-17T08:05:00Z"), &forge, &c.root).unwrap();
    quiet(&quietly);
    assert!(quietly.fields.get("timing").is_none());
    assert!(!quietly.lines.iter().any(|line| line.starts_with("phase ")));

    forge.set(
        OPEN_PR,
        Pull {
            state: PullState::Closed,
            ..open(&"e".repeat(40))
        },
    );
    let failed = sync(&at("2026-10-17T08:10:00Z"), &forge, &c.root).unwrap();
    assert_eq!(
        failed
            .violations
            .iter()
            .map(|violation| violation.rule)
            .collect::<Vec<_>>(),
        ["pr-closed"]
    );
    let phases: Vec<_> = failed.fields["timing"]["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|phase| phase["phase"].as_str().unwrap())
        .collect();
    assert_eq!(
        phases,
        [
            "guard", "load", "read", "lock", "apply", "write", "journal", "render", "report"
        ]
    );
    assert!(
        failed
            .lines
            .iter()
            .any(|line| line.starts_with("phase read: ") && line.contains("1 pull")),
        "{:?}",
        failed.lines
    );
}

#[test]
fn gh_reads_run_at_once() {
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    let worktree = c.root.parent().unwrap().join("wt");
    fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "-q"]);
    for number in 1..=4 {
        let target = format!("01-a/u/S{number}");
        declare(&host, &c, &target, &[], Ships::default());
        open_pr(
            &host,
            &c,
            &worktree,
            &target,
            &format!("https://github.com/o/r/pull/{number}"),
        );
    }
    // Each PR read waits, up to five seconds, until another read is in flight beside it.
    let gh = support::FakeGh::new(
        r#"if [ "$2" = rate_limit ]; then echo rate_limit >>"$FAKE_GH/calls"; exit 1; fi
touch "$FAKE_GH/in.$$"
tries=100
while :; do
    seen=$(ls "$FAKE_GH" | grep -c '^in\.')
    if [ "$seen" -ge 2 ] || [ "$tries" -eq 0 ]; then break; fi
    tries=$((tries - 1))
    sleep 0.05
done
echo "$seen" >>"$FAKE_GH/overlap"
echo pull >>"$FAKE_GH/calls"
rm "$FAKE_GH/in.$$"
echo '{"state":"open","merged":false,"head":{"sha":"0000000000000000000000000000000000000000"}}'
"#,
    );
    let forge = cm::forge::Gh {
        program: gh.program(),
        timeout: Duration::from_secs(30),
    };
    let synced = sync_with(&host, Reader::Concurrent(&forge), &c.root, BUDGET).unwrap();
    assert!(synced.ok(), "{:?}", synced.violations);
    let overlap = fs::read_to_string(gh.fixture().join("overlap")).unwrap();
    assert_eq!(overlap.lines().count(), 4, "{overlap}");
    assert!(
        overlap
            .lines()
            .all(|seen| seen.parse::<usize>().unwrap() >= 2),
        "a read ran alone: {overlap}"
    );
    // The quota sample follows the reads, so its `used` counts them.
    let calls = fs::read_to_string(gh.fixture().join("calls")).unwrap();
    assert_eq!(calls, "pull\npull\npull\npull\nrate_limit\n");
}

#[test]
fn cm_sync_prints_phases_past_its_budget() {
    let c = campaign(&at("2026-10-17T08:00:00Z"));
    let gh = support::FakeGh::new("exit 1\n");
    let run = |budget: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&c.root)
            .args(["--json", "sync", "--budget-secs", budget])
            .env("PATH", gh.path())
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let over = run("0");
    assert_eq!(over["timing"]["budget_seconds"], 0);
    assert_eq!(over["timing"]["phases"].as_array().unwrap().len(), 9);
    assert!(run("600").get("timing").is_none());
}

#[test]
fn the_card_does_not_depend_on_who_runs_sync_or_where() {
    let c = campaign(&at("2026-10-17T08:00:00Z"));
    fs::write(
        c.root.join("EPOCH.md"),
        "---\nepoch: trail-slug\nstatus: WORKING\n---\n",
    )
    .unwrap();
    let kickoff = c.root.parent().unwrap().join("kickoff.md");
    fs::write(&kickoff, "Go.\n").unwrap();
    let gh = support::FakeGh::new("exit 1\n");
    let cm = |root: &Path, args: &[&str], env: &[(&str, &str)]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cm"));
        command
            .arg("-C")
            .arg(root)
            .args(args)
            .env("PATH", gh.path())
            .env_remove("CLAUDE_CODE_REMOTE_SESSION_ID")
            .env_remove("CM_CODEX_WEEKLY_PCT")
            .envs(env.iter().copied());
        let output = command.output().unwrap();
        assert!(output.status.success(), "{args:?} {env:?}: {output:?}");
    };
    let kickoff = kickoff.to_str().unwrap();
    for (target, lane) in [("01-a/u/CLOUD", "claude-cloud"), ("01-a/u/CODEX", "codex")] {
        cm(
            &c.root,
            &["init", target, "--lane", lane, "--kickoff-file", kickoff],
            &[],
        );
    }
    cm(
        &c.root,
        &["launch", "01-a/u/CLOUD", "--coordinator", "session_one"],
        &[],
    );
    let card = |root: &Path, env: &[(&str, &str)]| {
        cm(root, &["sync"], env);
        fs::read_to_string(root.join("CARD.md")).unwrap()
    };
    let alone = card(&c.root, &[]);
    // Another session's claim is an action for that session's `cm next`, not a card fact.
    assert_eq!(
        card(&c.root, &[("CLAUDE_CODE_REMOTE_SESSION_ID", "cse_other")]),
        alone
    );
    // The Codex weekly hold is the caller's environment too; `cm next` and `cm launch` apply it.
    for used in ["0", "86", "87"] {
        assert_eq!(
            card(&c.root, &[("CM_CODEX_WEEKLY_PCT", used)]),
            alone,
            "{used}%"
        );
    }
    assert!(!alone.contains("held"), "{alone}");
    assert_eq!(card(&c.root, &[]), alone);

    assert!(alone.starts_with("# trail-slug card\n"), "{alone}");

    // A checkout under another parent, or under another folder name, renders the same card.
    let elsewhere = tempfile::tempdir().unwrap();
    for copy in [
        elsewhere.path().join("clone").join("campaign"),
        elsewhere.path().join("renamed"),
    ] {
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        let copied = Command::new("cp")
            .arg("-a")
            .arg(&c.root)
            .arg(&copy)
            .status()
            .unwrap();
        assert!(copied.success());
        assert_eq!(card(&copy, &[]), alone, "{}", copy.display());
    }
}

#[test]
fn a_sync_that_stops_with_an_error_times_the_phase_that_failed() {
    let gh = support::FakeGh::new("exit 1\n");
    for (path, phase) in [("CARD.md", "render"), (".cm/pulls.json", "write")] {
        let c = campaign(&at("2026-10-17T08:00:00Z"));
        fs::create_dir_all(c.root.join(path).join("blocker")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(&c.root)
            .args(["sync", "--budget-secs", "600"])
            .env("PATH", gh.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let last = stderr
            .lines()
            .rfind(|line| line.starts_with("phase "))
            .unwrap_or_default();
        assert!(
            last.starts_with(&format!("phase {phase}: ")) && last.ends_with(" (failed)"),
            "{path}: {stderr}"
        );
    }
}

#[test]
fn validators_live_only_in_the_ignored_read_memo() {
    let (c, _, _) = epoch();
    let tracked = fs::read_to_string(cm::plan::pulls_path(&c.root)).unwrap();
    assert!(tracked.contains(OPEN_PR), "{tracked}");
    assert!(!tracked.contains("etags"), "{tracked}");
    let memo = fs::read_to_string(c.root.join(".cm").join(cm::sync::PULLS_READ)).unwrap();
    assert!(memo.contains("etags"), "{memo}");
}

#[test]
fn an_unreadable_publication_is_journaled_once_while_it_stays_unreadable() {
    use publication_fixture as p;
    let c = p::campaign(&at("2026-10-17T08:00:00Z"), Phase::Merged, &p::ARTIFACTS);
    let mut forge = p::present();
    forge.broken.insert("crate:acme-sdk".into());
    let journal = || fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let before = journal();
    let mut entries = Vec::new();
    for now in [
        "2026-10-17T08:00:00Z",
        "2026-10-17T08:05:00Z",
        "2026-10-17T09:00:00Z",
    ] {
        let out = sync(&at(now), &forge, &c.root).unwrap();
        let rules: Vec<_> = out.violations.iter().map(|v| v.rule).collect();
        assert_eq!(rules, ["sync-unknown"], "the violation stands at {now}");
        assert!(
            out.lines
                .iter()
                .any(|line| line.starts_with("unknown 00-release/widget/1.4.0: ")),
            "{now}: {:?}",
            out.lines
        );
        entries.push(out.fields.contains_key("journal"));
    }
    assert_eq!(entries, [true, false, false]);
    let added = journal();
    let added = added.strip_prefix(before.as_str()).unwrap();
    assert_eq!(added.matches("\n## ").count(), 1, "{added}");
    assert_eq!(
        added.matches("- unknown 00-release/widget/1.4.0: ").count(),
        1,
        "{added}"
    );
}

#[test]
fn the_card_changes_with_the_clock_only_across_a_threshold() {
    let host = at("2026-10-26T04:00:00Z");
    let c = campaign(&host);
    declare(&host, &c, "01-a/u/check", &[], Ships::default());
    let target = parse_target("01-a/u/check", true).unwrap();
    let added = cm::holds::watch_add(
        &host,
        &c.root,
        &target,
        "log:withdrawn",
        "2026-11-30T06:00Z",
    );
    assert!(added.unwrap().ok());
    let withdrawn = cm::holds::watch_withdraw(
        &at("2026-10-26T07:00:00Z"),
        &c.root,
        &target,
        1,
        "wrong signal",
    );
    assert!(withdrawn.unwrap().ok());
    let units = cm::plan::load_units(&c.root).unwrap();
    let next = cm::plan::summary(&c.root, &units);
    let card = |now: &str| cm::render::card(&c.root, at(now).now, "c", &units, &next);
    // A withdrawn watch shows for 24 hours; within that window, and after it, time changes nothing.
    let shown = card("2026-10-26T07:00:01Z");
    assert!(shown.contains("withdrawn-recent 1"), "{shown}");
    for now in ["2026-10-26T12:00:00Z", "2026-10-27T07:00:00Z"] {
        assert_eq!(card(now), shown, "{now}");
    }
    let gone = card("2026-10-27T07:00:01Z");
    assert!(gone.contains("withdrawn-recent 0"), "{gone}");
    for now in ["2026-10-27T08:00:00Z", "2026-10-29T07:00:00Z"] {
        assert_eq!(card(now), gone, "{now}");
    }
}

#[test]
fn turn_readers_run_one_at_a_time() {
    use cm::attempt::{Act, attempt};
    let host = at("2026-10-17T08:00:00Z");
    let c = campaign(&host);
    let act = |target: &str, claim: Option<&str>, act: Act| {
        let target = parse_target(target, true).unwrap();
        let out = attempt(&host, &c.root, &target, claim, &act).unwrap();
        assert!(out.ok(), "{:?}", out.violations);
    };
    for name in ["one", "two", "three"] {
        let target = format!("01-a/u/{name}");
        let declared = commands::declare(
            &host,
            &c.root.join(&target),
            &Declaration {
                lane: Some(Lane::Codex),
                ..Declaration::default()
            },
        )
        .unwrap();
        assert!(declared.ok(), "{:?}", declared.violations);
        fs::write(c.root.join(&target).join("kickoff.md"), "Go.\n").unwrap();
        let launched = commands::launch_remote(
            &host,
            &c.root,
            &parse_target(&target, true).unwrap(),
            None,
            &commands::Remote {
                coordinator: Some("session_one".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(launched.ok(), "{:?}", launched.violations);
        let claim = launched.fields["attempt"]["claim"]
            .as_str()
            .unwrap()
            .to_owned();
        for (kind, id) in [("thread", name), ("turn", "latest")] {
            let begin = Act::Begin { kind: kind.into() };
            act(&target, Some(&claim), begin);
            let record = Act::Record {
                kind: kind.into(),
                id: id.into(),
            };
            act(&target, Some(&claim), record);
        }
    }
    // An operator's reader may do more than read: sync runs it alone.
    let scratch = tempfile::tempdir().unwrap();
    let reader = scratch.path().join("reader");
    support::write_executable(
        &reader,
        &format!(
            r#"#!/bin/sh
touch "{dir}/in.$$"
sleep 0.2
ls "{dir}" | grep -c '^in\.' >>"{dir}/overlap"
rm "{dir}/in.$$"
echo '{{"done":false,"successful":false,"turn_status":"inProgress"}}'
"#,
            dir = scratch.path().display()
        ),
    );
    let limits = c.root.join("LIMITS.toml");
    let argv = serde_json::json!([reader, "{thread}", "{turn}"]);
    let text = fs::read_to_string(&limits).unwrap();
    fs::write(
        &limits,
        format!("{text}\ncodex_turn_reader = {argv}\ncodex_turn_logs = [\"logs\"]\n"),
    )
    .unwrap();
    let gh = support::FakeGh::new("exit 1\n");
    let forge = cm::forge::Gh {
        program: gh.program(),
        timeout: Duration::from_secs(30),
    };
    let synced = sync_with(&host, Reader::Concurrent(&forge), &c.root, BUDGET).unwrap();
    assert!(synced.ok(), "{:?}", synced.violations);
    let overlap = fs::read_to_string(scratch.path().join("overlap")).unwrap();
    assert_eq!(overlap, "1\n1\n1\n");
}

#[test]
fn a_card_without_an_epoch_slug_keeps_its_title_under_another_folder_name() {
    let gh = support::FakeGh::new("exit 1\n");
    let synced_card = |root: &Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .arg("-C")
            .arg(root)
            .arg("sync")
            .env("PATH", gh.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        fs::read_to_string(root.join("CARD.md")).unwrap()
    };
    for epoch in [
        None,
        Some("---\nstatus: WORKING\n---\n"),
        Some("---\nepoch:\nstatus: WORKING\n---\n"),
    ] {
        let c = campaign(&at("2026-10-17T08:00:00Z"));
        if let Some(text) = epoch {
            fs::write(c.root.join("EPOCH.md"), text).unwrap();
        }
        let card = synced_card(&c.root);
        let moved = c.root.with_file_name("moved");
        let copied = Command::new("cp")
            .arg("-a")
            .arg(&c.root)
            .arg(&moved)
            .status()
            .unwrap();
        assert!(copied.success());
        assert_eq!(synced_card(&moved), card, "{epoch:?}");
        assert!(card.starts_with("# campaign card\n"), "{card}");
    }
}

#[test]
fn an_unreadable_publication_is_journaled_again_when_it_returns() {
    use publication_fixture as p;
    let host = at("2026-10-17T08:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    let (crate_key, image_key) = ("crate:acme-sdk", "image:ghcr.io/acme/widget");
    let unreadable = |keys: &[&str]| {
        let mut forge = p::present();
        forge
            .broken
            .extend(keys.iter().map(|key| (*key).to_owned()));
        forge
    };
    let mut readable = p::present();
    readable.crates.clear();
    let entries = |steps: Vec<(&str, forge_fixture::FakeForge)>| {
        steps
            .into_iter()
            .map(|(now, forge)| {
                // An unrelated entry between syncs changes nothing.
                let noted = commands::journal(&at(now), &c.root, "operator note", None).unwrap();
                assert!(noted.ok(), "{:?}", noted.violations);
                let out = sync(&at(now), &forge, &c.root).unwrap();
                out.fields
                    .get("journal")
                    .map(|_| journal_tail(&c.root))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
    };
    // Unreadable, read again, unreadable: published twice.
    let first = entries(vec![
        ("2026-10-17T08:00:00Z", unreadable(&[crate_key])),
        ("2026-10-17T08:05:00Z", readable),
        ("2026-10-17T08:10:00Z", unreadable(&[crate_key])),
    ]);
    assert!(
        first[0].contains("- unknown 00-release/widget/1.4.0: crate"),
        "{first:?}"
    );
    assert!(first[1].is_empty(), "{first:?}");
    assert!(
        first[2].contains("- unknown 00-release/widget/1.4.0: crate"),
        "{first:?}"
    );
    // Another one returning while the first stays is news; the first is listed with it.
    let second = entries(vec![
        ("2026-10-17T08:15:00Z", unreadable(&[crate_key, image_key])),
        ("2026-10-17T08:20:00Z", unreadable(&[crate_key])),
        ("2026-10-17T08:25:00Z", unreadable(&[crate_key, image_key])),
    ]);
    assert!(second[0].contains(": image"), "{second:?}");
    assert!(second[1].is_empty(), "{second:?}");
    assert!(
        second[2].contains(": image") && second[2].contains(": crate"),
        "{second:?}"
    );
}

fn journal_tail(root: &Path) -> String {
    let journal = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    let last = journal.rfind("\n## ").unwrap();
    journal[last..].to_owned()
}
