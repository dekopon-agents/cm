use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use cm::commands;
use cm::host::Host;
use cm::lint::lint;
use cm::outcome::Outcome;
use cm::state::{After, Evidence, Lane, Phase, UnitState, Work};
use cm::store::{self, Target, parse_target};
use jiff::Timestamp;

#[allow(dead_code)]
mod support;

#[path = "support/campaign.rs"]
mod campaign_fixture;
#[allow(dead_code)]
#[path = "support/idea.rs"]
mod idea_fixture;
#[path = "support/kickoff.rs"]
mod kickoff_fixture;
#[path = "support/lifecycle.rs"]
mod lifecycle_fixture;

#[path = "support/forge.rs"]
mod forge_fixture;
#[path = "support/resume.rs"]
mod resume_fixture;

use campaign_fixture::{Campaign, campaign, git, repo};

struct Fake {
    now: Timestamp,
    free_gib: u64,
    builds: u32,
}

impl Fake {
    fn at(now: &str) -> Self {
        Fake {
            now: now.parse().unwrap(),
            free_gib: 100,
            builds: 0,
        }
    }
}

impl Host for Fake {
    fn now(&self) -> Timestamp {
        self.now
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(self.free_gib << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(self.builds)
    }
}

fn rules(outcome: &Outcome) -> Vec<&'static str> {
    let mut rules: Vec<_> = outcome.violations.iter().map(|v| v.rule).collect();
    rules.sort_unstable();
    rules
}

fn target(text: &str) -> Target {
    parse_target(text, false).unwrap()
}

fn unit_with_step(c: &Campaign, host: &Fake, kickoff: &str) {
    let outcome = commands::init(host, &c.root.join("01-first/alpha")).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    fs::create_dir_all(c.root.join("01-first/alpha/BUILD")).unwrap();
    fs::write(c.root.join("01-first/alpha/BUILD/kickoff.md"), kickoff).unwrap();
}

fn commit_campaign(root: &Path) {
    if !root.join(".git").exists() {
        git(root, &["init", "-q"]);
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "ledger"]);
}

fn assert_clean(root: &Path) {
    let outcome = lint(root).unwrap();
    assert!(outcome.ok(), "{:#?}", outcome.violations);
}

#[test]
fn init_writes_templates_and_lints_clean() {
    let host = Fake::at("2026-10-08T07:04:31Z");
    let c = campaign(&host);
    for name in [
        "RESUME.md",
        "JOURNAL.md",
        "LIMITS.toml",
        "OWNER-QUEUE.md",
        ".cm/state.json",
    ] {
        assert!(c.root.join(name).is_file(), "{name}");
    }
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains("\n## 2026-10-08T07:04Z — cm init\n"),
        "{journal}"
    );
    assert_clean(&c.root);
    let again = commands::init(&host, &c.root).unwrap();
    assert!(again.ok());
    assert_eq!(
        fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
        journal
    );
}

#[test]
fn init_never_overwrites_and_adopts_what_lints_clean() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let template = cm::templates::RESUME;
    let fence = template.find("> ```").unwrap();
    let end = template.find("\n\n# Resume").unwrap();
    let resume = format!(
        "{}> {}{}",
        &template[..fence],
        cm::templates::LOOP_WITHHELD,
        &template[end..]
    )
    .replace("No releases recorded.", "Our own words.");
    fs::write(root.join("RESUME.md"), &resume).unwrap();
    fs::write(
        root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nAnswer:\n",
    )
    .unwrap();
    let journal = "# Journal\n\n## 2026-09-28 and 29: planning\n\n- old prose\n";
    fs::write(root.join("JOURNAL.md"), journal).unwrap();

    let outcome = commands::init(&host, root).unwrap();
    assert_eq!(
        rules(&outcome),
        vec!["owner-queue-field", "owner-queue-field"]
    );
    assert_eq!(fs::read_to_string(root.join("RESUME.md")).unwrap(), resume);
    assert_eq!(
        fs::read_to_string(root.join("OWNER-QUEUE.md")).unwrap(),
        "# Owner queue\n\n## Pick one\n\nAnswer:\n"
    );
    let state = store::load_campaign(root).unwrap();
    assert_eq!(state.adopted, vec!["RESUME.md", "JOURNAL.md"]);
    assert_eq!(state.written, vec!["LIMITS.toml"]);

    fs::write(
        root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nContext: two paths.\n\nRecommendation: the first\n\nAnswer:\n",
    )
    .unwrap();
    let outcome = commands::init(&host, root).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    assert_eq!(
        store::load_campaign(root).unwrap().adopted,
        vec!["RESUME.md", "JOURNAL.md", "OWNER-QUEUE.md"]
    );
    let text = fs::read_to_string(root.join("JOURNAL.md")).unwrap();
    assert!(text.starts_with(journal));
    assert_clean(root);
}

#[test]
fn journal_stamps_come_from_the_clock() {
    let c = campaign(&Fake::at("2026-10-08T07:00:00Z"));
    let later = Fake::at("2026-10-08T07:09:59Z");
    let outcome = commands::journal(
        &later,
        &c.root,
        "ruled on S3",
        Some("Body line.\n\n### detail\n"),
    )
    .unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    let text = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        text.ends_with("\n## 2026-10-08T07:09Z — ruled on S3\n\nBody line.\n\n### detail\n"),
        "{text}"
    );
    let forged = commands::journal(
        &later,
        &c.root,
        "x",
        Some("## 2026-10-08T07:0xZ — forged\n"),
    )
    .unwrap();
    assert_eq!(rules(&forged), vec!["journal-entry"]);
    assert_clean(&c.root);
}

#[test]
fn launch_reports_every_failed_check_at_once() {
    let mut host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    unit_with_step(
        &c,
        &host,
        "Read `/definitely/not/here.md` and `~/also/not/here`.\n",
    );
    let limits = fs::read_to_string(c.root.join("LIMITS.toml"))
        .unwrap()
        .replace("paused = false", "paused = true");
    fs::write(c.root.join("LIMITS.toml"), limits).unwrap();
    host.free_gib = 27;
    host.builds = 4;
    let outcome = commands::launch(
        &host,
        &c.root,
        &parse_target("01-first/alpha/BUILD", true).unwrap(),
        Some(&c.root.join("no-such-worktree")),
    )
    .unwrap();
    assert_eq!(
        rules(&outcome),
        vec![
            "limits-disk",
            "limits-heavy-builds",
            "limits-paused",
            "packet-path",
            "packet-path",
            "worktree-missing"
        ],
        "{:#?}",
        outcome.violations
    );
    let unit = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    assert!(unit.steps.is_empty());
    assert_eq!(unit.track.state, Phase::Drafted);
}

fn advance(host: &Fake, c: &Campaign, what: &str, to: &str, evidence: Evidence) -> Outcome {
    commands::advance(
        host,
        &c.root,
        &target(what),
        &to.parse().unwrap(),
        &evidence,
        None,
        None,
    )
    .unwrap()
}

#[test]
fn merged_releases_accept_a_descendant_merge_sha() {
    for lane in [Lane::Coordinator, Lane::Codex] {
        let host = Fake::at("2026-10-08T07:00:00Z");
        let c = campaign(&host);
        let (repo, first) = repo(c.root.parent().unwrap());
        let step = "00-release/cm/1.0.0";
        c.merged_step(&host, step, lane, &repo, &first);
        let dir = c.root.join("00-release/cm");
        let before = store::load_unit(&dir).unwrap();
        let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        git(
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "second merge"],
        );
        let second = git(&repo, &["rev-parse", "HEAD"]);
        let later = Fake::at("2026-10-08T08:00:00Z");
        let outcome = advance(
            &later,
            &c,
            step,
            "merged",
            Evidence {
                sha: Some(second[..10].into()),
                ..Evidence::default()
            },
        );
        assert!(outcome.ok(), "{:?}", outcome.violations);
        assert_eq!(outcome.fields["previous_sha"], first);
        assert_eq!(outcome.fields["evidence"]["sha"], second);
        let after = store::load_unit(&dir).unwrap();
        let mut expected = before.clone();
        expected.steps.get_mut("1.0.0").unwrap().track.evidence.sha = Some(second.clone());
        expected.history.push(cm::state::Move {
            at: "2026-10-08T08:00Z".into(),
            target: step.into(),
            from: Phase::Merged,
            to: Phase::Merged,
            evidence: Evidence {
                sha: Some(second.clone()),
                ..Evidence::default()
            },
        });
        assert_eq!(after, expected);
        assert_eq!(
            fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
            format!(
                "{journal}\n## 2026-10-08T08:00Z — advance {step}: merged → merged\n\n- evidence: sha {second}\n- previous merge sha: {first}\n"
            )
        );
    }
}

#[test]
fn merged_releases_refuse_an_unrelated_merge_sha() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, first) = repo(c.root.parent().unwrap());
    let step = "00-release/cm/1.0.0";
    c.merged_step(&host, step, Lane::Coordinator, &repo, &first);
    git(&repo, &["checkout", "-q", "--orphan", "unrelated"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "unrelated"]);
    let unrelated = git(&repo, &["rev-parse", "HEAD"]);
    let before = c.files();
    let outcome = advance(
        &host,
        &c,
        step,
        "merged",
        Evidence {
            sha: Some(unrelated),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&outcome), vec!["merge-lineage"]);
    assert_eq!(c.files(), before);
}

#[test]
fn merged_releases_refuse_unreadable_ancestry() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, first, later) =
        campaign_fixture::repo_with_missing_history(c.root.parent().unwrap());
    let step = "00-release/cm/1.0.0";
    c.merged_step(&host, step, Lane::Coordinator, &repo, &first);
    let before = c.files();
    let outcome = advance(
        &host,
        &c,
        step,
        "merged",
        Evidence {
            sha: Some(later),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&outcome), ["merge-lineage"]);
    assert_eq!(c.files(), before);
}

#[test]
fn merged_non_release_steps_refuse_a_descendant_merge_sha() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, first) = repo(c.root.parent().unwrap());
    let step = "01-first/alpha/BUILD";
    c.merged_step(&host, step, Lane::Coordinator, &repo, &first);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "second merge"],
    );
    let second = git(&repo, &["rev-parse", "HEAD"]);
    let before = c.files();
    let outcome = advance(
        &host,
        &c,
        step,
        "merged",
        Evidence {
            sha: Some(second),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&outcome), vec!["advance-refused"]);
    assert_eq!(c.files(), before);
}

#[test]
fn github_merge_time_appears_in_state_and_close_accounting() {
    for merged_at in [Some(lifecycle_fixture::MERGED_AT), None] {
        let host = Fake::at("2026-10-08T07:00:00Z");
        let c = campaign(&host);
        let (repo, sha) = repo(c.root.parent().unwrap());
        unit_with_step(&c, &host, "Go.\n");
        let step = "01-first/alpha/BUILD";
        let target = parse_target(step, true).unwrap();
        assert!(
            commands::launch(&host, &c.root, &target, Some(&repo))
                .unwrap()
                .ok()
        );
        let evidence = Evidence {
            pr: Some("https://github.com/o/r/pull/7".into()),
            sha: Some(sha.clone()),
            ..Evidence::default()
        };
        assert!(advance(&host, &c, step, "pr-open", evidence.clone()).ok());
        assert!(advance(&host, &c, step, "blocked:waiting", Evidence::default()).ok());
        let gh = lifecycle_fixture::merged_pull(&sha, merged_at);
        let forge = cm::forge::Gh {
            program: gh.program(),
            ..cm::forge::Gh::default()
        };
        let later = Fake::at("2026-10-08T20:00:00Z");
        let moved = commands::advance_gated(
            &later,
            &c.root,
            &target,
            commands::Advance {
                to: &Phase::Merged,
                given: &evidence,
                file: None,
                repo: Some(&repo),
                part: None,
                by: &[],
            },
            &commands::MergeGate {
                forge: Some(&forge),
                ..commands::MergeGate::default()
            },
        )
        .unwrap();
        assert!(moved.ok(), "{:?}", moved.violations);
        let unit = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
        assert_eq!(unit.schema, cm::state::SCHEMA);
        let track = &unit.steps["BUILD"].track;
        assert_eq!(track.evidence.merged_at.as_deref(), merged_at);
        assert_eq!(track.since, cm::host::stamp(later.now()));
        let shown = merged_at.unwrap_or(&track.since);
        let state = fs::read_to_string(c.root.join("01-first/alpha/STATE.md")).unwrap();
        let row = state
            .lines()
            .find(|line| line.starts_with("| BUILD | merged |"))
            .unwrap();
        assert_eq!(row.split('|').nth(3).unwrap().trim(), shown);
        let stored: serde_json::Value =
            store::read_json(&store::state_path(&c.root.join("01-first/alpha"))).unwrap();
        assert_eq!(
            stored["steps"]["BUILD"]["evidence"].get("merged_at"),
            merged_at.map(|at| serde_json::json!(at)).as_ref()
        );
        assert!(advance(&later, &c, step, "blocked:carried", Evidence::default()).ok());
        let restored =
            commands::unblock(&later, &c.root, &target, Some(&c.unblock_file())).unwrap();
        assert!(restored.ok(), "{:?}", restored.violations);
        assert!(advance(&later, &c, step, "blocked:carried", Evidence::default()).ok());
        assert!(
            advance(
                &later,
                &c,
                "01-first/alpha",
                "blocked:carried",
                Evidence::default()
            )
            .ok()
        );
        let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        let closed = commands::close(&later, &c.root).unwrap();
        assert!(closed.ok(), "{:?}", closed.violations);
        assert_eq!(
            fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
            format!(
                "{journal}\n## 2026-10-08T20:00Z — cm close\n\n- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n- merged: {step} at {shown}\n"
            ),
        );
    }
}

#[test]
fn close_counts_real_release_remerges_once_and_preserves_the_original_pr_time() {
    let host = Fake::at("2026-10-08T20:00:00Z");
    let c = campaign(&host);
    let (repo, first) = repo(c.root.parent().unwrap());
    let step = "00-release/cm/1.0.0";
    c.merged_step(&host, step, Lane::Coordinator, &repo, &first);
    c.record_merge_time(step, lifecycle_fixture::MERGED_AT);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "second merge"],
    );
    let second = git(&repo, &["rev-parse", "HEAD"]);
    let later = Fake::at("2026-10-09T08:00:00Z");
    let moved = advance(
        &later,
        &c,
        step,
        "merged",
        Evidence {
            sha: Some(second),
            ..Default::default()
        },
    );
    assert!(moved.ok(), "{:?}", moved.violations);
    let unit = store::load_unit(&c.root.join("00-release/cm")).unwrap();
    let track = &unit.steps["1.0.0"].track;
    assert_eq!(
        track.evidence.merged_at.as_deref(),
        Some(lifecycle_fixture::MERGED_AT)
    );
    assert_eq!(track.since, cm::host::stamp(host.now()));
    assert_eq!(unit.history.last().unwrap().evidence.merged_at, None);
    assert!(advance(&later, &c, step, "blocked:carried", Evidence::default()).ok());
    assert!(
        commands::unblock(
            &later,
            &c.root,
            &parse_target(step, true).unwrap(),
            Some(&c.unblock_file())
        )
        .unwrap()
        .ok()
    );
    assert!(advance(&later, &c, step, "blocked:carried", Evidence::default()).ok());
    assert!(
        advance(
            &later,
            &c,
            "00-release/cm",
            "blocked:carried",
            Evidence::default()
        )
        .ok()
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    let closed = commands::close(&later, &c.root).unwrap();
    assert!(closed.ok(), "{:?}", closed.violations);
    assert_eq!(
        fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
        format!(
            "{journal}\n## 2026-10-09T08:00Z — cm close\n\n- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n- merged: {step} at {}\n- merged: {step} at 2026-10-09T08:00Z\n",
            lifecycle_fixture::MERGED_AT
        ),
    );
}

#[test]
fn a_step_walks_from_launch_to_done() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, &format!("Work in `{}`.\n", repo.display()));
    let step = "01-first/alpha/BUILD";
    let outcome = commands::launch(
        &host,
        &c.root,
        &parse_target(step, true).unwrap(),
        Some(&repo),
    )
    .unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    let command = outcome.fields["launch"].as_str().unwrap();
    assert!(command.contains("pi-turn.sh "), "{command}");
    assert!(
        command.contains(" alpha-BUILD-sup openai-codex/gpt-6-sol:high "),
        "{command}"
    );
    assert!(
        command.ends_with("/sup-turn-1.jsonl kickoff.md"),
        "{command}"
    );

    let pr = Evidence {
        pr: Some("https://github.com/o/r/pull/7".into()),
        sha: Some(sha[..10].into()),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "pr-open", pr).ok());
    let merged = Evidence {
        sha: Some(sha.clone()),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "merged", merged).ok());
    git(&repo, &["tag", "v0.1.0"]);
    let released = Evidence {
        tag: Some("v0.1.0".into()),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "released", released).ok());
    let deployed = Evidence {
        digest: Some(format!("sha256:{}", "a".repeat(64))),
        ..Evidence::default()
    };
    assert!(advance(&host, &c, step, "deployed", deployed).ok());
    assert!(advance(&host, &c, step, "done", Evidence::default()).ok());

    let unit: UnitState = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    let track = &unit.steps["BUILD"].track;
    assert_eq!(track.state, Phase::Done);
    assert_eq!(track.evidence.sha.as_deref(), Some(sha.as_str()));
    assert_eq!(track.evidence.tag.as_deref(), Some("v0.1.0"));
    assert_eq!(unit.history.len(), 7);
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(
        journal.contains("## 2026-10-08T07:00Z — advance 01-first/alpha/BUILD: deployed → done\n")
    );
    assert_clean(&c.root);
}

#[test]
fn a_release_tag_must_contain_the_recorded_merge() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha, _) = campaign_fixture::release_repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, "Build the thing.\n");
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::launch(&host, &c.root, &target(step), Some(&repo))
            .unwrap()
            .ok()
    );
    assert!(
        advance(
            &host,
            &c,
            step,
            "pr-open",
            Evidence {
                pr: Some("https://github.com/o/r/pull/7".into()),
                sha: Some(sha.clone()),
                ..Evidence::default()
            }
        )
        .ok()
    );
    assert!(
        advance(
            &host,
            &c,
            step,
            "merged",
            Evidence {
                sha: Some(sha),
                ..Evidence::default()
            }
        )
        .ok()
    );
    let before = c.files();
    let refused = advance(
        &host,
        &c,
        step,
        "released",
        Evidence {
            tag: Some("v0.1.0".into()),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&refused), ["tag-lineage"]);
    assert_eq!(c.files(), before);
    fs::rename(repo.join(".git"), repo.join("unreadable-git")).unwrap();
    let unread = advance(
        &host,
        &c,
        step,
        "released",
        Evidence {
            tag: Some("v0.2.0".into()),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&unread), ["evidence-tag", "tag-lineage"]);
    assert_eq!(
        unread
            .violations
            .iter()
            .find(|violation| violation.rule == "tag-lineage")
            .unwrap()
            .message,
        "tag containment could not be read; pass --repo <a checkout of the repo>"
    );
    assert_eq!(c.files(), before);
    fs::rename(repo.join("unreadable-git"), repo.join(".git")).unwrap();
    let released = advance(
        &host,
        &c,
        step,
        "released",
        Evidence {
            tag: Some("v0.2.0".into()),
            ..Evidence::default()
        },
    );
    assert!(released.ok(), "{:?}", released.violations);
}

#[test]
fn a_tag_only_release_requires_a_sha_contained_in_the_tag() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    for launched in [false, true] {
        let c = campaign(&host);
        let (repo, merged, unshipped) = campaign_fixture::release_repo(c.root.parent().unwrap());
        let step = "00-release/widget/0.2.0";
        assert!(
            commands::declare(
                &host,
                &c.root.join(step),
                &commands::Declaration {
                    lane: Some(Lane::Coordinator),
                    ..commands::Declaration::default()
                }
            )
            .unwrap()
            .ok()
        );
        c.kickoff(step, "Cut the release.\n");
        if launched {
            assert!(
                commands::launch(&host, &c.root, &target(step), Some(&repo))
                    .unwrap()
                    .ok()
            );
        }
        let release = |sha| {
            commands::advance(
                &host,
                &c.root,
                &target(step),
                &Phase::Released,
                &Evidence {
                    sha,
                    tag: Some("v0.2.0".into()),
                    ..Evidence::default()
                },
                None,
                Some(&repo),
            )
            .unwrap()
        };
        let before = c.files();
        assert_eq!(rules(&release(None)), ["evidence-missing"]);
        assert_eq!(rules(&release(Some(unshipped))), ["tag-lineage"]);
        assert_eq!(c.files(), before);
        let released = release(Some(merged[..10].into()));
        assert!(released.ok(), "{:?}", released.violations);
        assert_eq!(released.fields["evidence"]["sha"], merged);
    }
}

#[test]
fn advance_refuses_a_skip_and_missing_or_unexpected_evidence() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, _) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, "No paths.\n");
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::launch(
            &host,
            &c.root,
            &parse_target(step, true).unwrap(),
            Some(&repo)
        )
        .unwrap()
        .ok()
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();

    let skip = advance(
        &host,
        &c,
        step,
        "released",
        Evidence {
            tag: Some("v9".into()),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&skip), vec!["advance-refused", "evidence-tag"]);
    assert!(skip.violations[0].message.contains("skips pr-open, merged"));

    let thin = advance(
        &host,
        &c,
        step,
        "pr-open",
        Evidence {
            pr: Some("https://example.com/pull/1".into()),
            digest: Some("sha256:abc".into()),
            ..Evidence::default()
        },
    );
    assert_eq!(
        rules(&thin),
        vec![
            "evidence-digest",
            "evidence-missing",
            "evidence-pr",
            "evidence-unexpected"
        ]
    );
    let relaunch = advance(&host, &c, step, "launched", Evidence::default());
    assert_eq!(rules(&relaunch), vec!["advance-refused"]);
    let unknown = advance(
        &host,
        &c,
        "01-first/alpha/NOPE",
        "pr-open",
        Evidence::default(),
    );
    assert_eq!(rules(&unknown), vec!["step-unknown"]);

    assert_eq!(
        fs::read_to_string(c.root.join("JOURNAL.md")).unwrap(),
        journal
    );
    assert_eq!(
        store::load_unit(&c.root.join("01-first/alpha"))
            .unwrap()
            .steps["BUILD"]
            .track
            .state,
        Phase::Launched
    );
}

#[test]
fn a_blocked_step_relaunches_from_its_newest_resume() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, _) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, "Go.\n");
    let step = "01-first/alpha/BUILD";
    let launch_target = parse_target(step, true).unwrap();
    assert!(
        commands::launch(&host, &c.root, &launch_target, Some(&repo))
            .unwrap()
            .ok()
    );
    let again = commands::launch(&host, &c.root, &launch_target, Some(&repo)).unwrap();
    assert_eq!(rules(&again), vec!["launch-state"]);
    assert!(advance(&host, &c, step, "blocked:driver", Evidence::default()).ok());
    for name in ["resume-2.md", "resume-10.md"] {
        fs::write(c.root.join("01-first/alpha/BUILD").join(name), "Resume.\n").unwrap();
    }
    let outcome = commands::launch(&host, &c.root, &launch_target, Some(&repo)).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    let command = outcome.fields["launch"].as_str().unwrap();
    assert!(
        command.ends_with("/sup-turn-2.jsonl resume-10.md"),
        "{command}"
    );
    assert_clean(&c.root);
}

#[test]
fn lint_lists_every_violation_in_one_run() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    unit_with_step(&c, &host, "Read `/definitely/not/here.md`.\n");
    commit_campaign(&c.root);
    let state_md = c.root.join("01-first/alpha/STATE.md");
    let edited = fs::read_to_string(&state_md)
        .unwrap()
        .replace("| drafted |", "| merged |");
    fs::write(&state_md, edited).unwrap();
    let mut journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    journal.push_str("\n## 2026-10-08T07:0xZ — typed by a model\n");
    fs::write(c.root.join("JOURNAL.md"), journal).unwrap();
    let resume = fs::read_to_string(c.root.join("RESUME.md"))
        .unwrap()
        .replace("## How to work\n", "");
    fs::write(c.root.join("RESUME.md"), resume).unwrap();
    let limits = fs::read_to_string(c.root.join("LIMITS.toml"))
        .unwrap()
        .replace("max_heavy_builds = 4\n", "");
    fs::write(c.root.join("LIMITS.toml"), limits).unwrap();
    fs::write(
        c.root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nContext: two paths.\n\nRecommendation: yes\n",
    )
    .unwrap();

    let outcome = lint(&c.root).unwrap();
    assert_eq!(
        rules(&outcome),
        vec![
            "journal-hand-append",
            "journal-unstamped-heading",
            "limits-key",
            "owner-queue-field",
            "packet-path",
            "resume-heading",
            "resume-loop",
            "unit-state-hand-edit",
        ],
        "{:#?}",
        outcome.violations
    );
}

#[test]
fn a_rewritten_journal_prefix_is_found_and_blocks_writes() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    commit_campaign(&c.root);
    let path = c.root.join("JOURNAL.md");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("— cm init", "— cm init, edited");
    fs::write(&path, text).unwrap();
    let outcome = lint(&c.root).unwrap();
    assert_eq!(rules(&outcome), vec!["journal-rewritten"]);
    assert_eq!(outcome.violations[0].line, Some(5));
    let refused = commands::journal(&host, &c.root, "next", None).unwrap();
    assert_eq!(rules(&refused), vec!["journal-rewritten"]);
}

#[test]
fn a_removed_worktree_points_advance_at_repo() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, "Go.\n");
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::launch(
            &host,
            &c.root,
            &parse_target(step, true).unwrap(),
            Some(&repo)
        )
        .unwrap()
        .ok()
    );
    let moved = c.root.parent().unwrap().join("moved");
    fs::rename(&repo, &moved).unwrap();
    let pr = Evidence {
        pr: Some("https://github.com/o/r/pull/7".into()),
        sha: Some(sha.clone()),
        ..Evidence::default()
    };
    let refused = advance(&host, &c, step, "pr-open", pr.clone());
    assert_eq!(rules(&refused), vec!["evidence-repo"]);
    assert!(
        refused.violations[0]
            .message
            .contains("is gone; pass --repo")
    );
    let outcome = commands::advance(
        &host,
        &c.root,
        &target(step),
        &"pr-open".parse().unwrap(),
        &pr,
        None,
        Some(&moved),
    )
    .unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
}

#[test]
fn hand_edit_messages_name_the_reset() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    unit_with_step(&c, &host, "Go.\n");
    commit_campaign(&c.root);
    let state_md = c.root.join("01-first/alpha/STATE.md");
    fs::write(&state_md, "edited\n").unwrap();
    let mut journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    journal.push_str("extra\n");
    fs::write(c.root.join("JOURNAL.md"), journal).unwrap();
    let outcome = lint(&c.root).unwrap();
    let messages: Vec<&str> = outcome
        .violations
        .iter()
        .map(|v| v.message.as_str())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("into a new entry with `cm journal`")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`rm 01-first/alpha/STATE.md && cm init 01-first/alpha`")),
        "{messages:?}"
    );
    fs::remove_file(&state_md).unwrap();
    git(&c.root, &["checkout", "HEAD", "--", "JOURNAL.md"]);
    assert!(
        commands::init(&host, &c.root.join("01-first/alpha"))
            .unwrap()
            .ok()
    );
    assert_clean(&c.root);
}

#[test]
fn close_requires_an_outcome_for_every_admitted_idea() {
    let c = idea_fixture::ideas();
    for id in ["IDEA-9003", "IDEA-9001"] {
        c.add_idea(id);
        assert_eq!(c.cm(&["idea", "admit", id])["ok"], true);
    }
    let open_ideas = [c.read_idea("IDEA-9003"), c.read_idea("IDEA-9001")];
    let before = c.idea_snapshot();
    let refused = commands::close(&cm::host::System, &c.root).unwrap();
    assert_eq!(
        rules(&refused),
        ["idea-outcome-missing", "idea-outcome-missing"]
    );
    for (violation, id) in refused.violations.iter().zip(["IDEA-9003", "IDEA-9001"]) {
        assert_eq!(violation.path, "EPOCH.md");
        assert!(violation.message.contains(id), "{violation}");
        assert!(violation.message.contains(&format!("cm idea outcome {id}")));
    }
    assert_eq!(c.idea_snapshot(), before);
    let refused_gaps = refused.violations;
    assert_eq!(c.cm(&["lint"])["ok"], true);

    assert_eq!(
        c.cm(&[
            "idea",
            "outcome",
            "IDEA-9003",
            "partial",
            "--to",
            "epoch-beta",
            "--note",
            "one part landed"
        ])["ok"],
        true
    );
    let refused = commands::close(&cm::host::System, &c.root).unwrap();
    assert_eq!(rules(&refused), ["idea-outcome-missing"]);
    assert!(refused.violations[0].message.contains("IDEA-9001"));
    assert_eq!(
        c.cm(&[
            "idea",
            "outcome",
            "IDEA-9001",
            "deferred",
            "--to",
            "unassigned"
        ])["ok"],
        true
    );

    let before = c.journal_text();
    let closed = commands::close(&cm::host::System, &c.root).unwrap();
    assert!(closed.ok(), "{:?}", closed.violations);
    assert_eq!(
        c.journal_text(),
        format!(
            "{before}\n{}",
            cm::journal::entry(
                closed.fields["closed"].as_str().unwrap(),
                "cm close",
                Some(
                    "- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n- idea: IDEA-9003 partial to epoch-beta\n- idea: IDEA-9001 deferred to unassigned\n"
                ),
            )
        )
    );
    assert_eq!(
        store::load_campaign(&c.root).unwrap().closed.as_deref(),
        closed.fields["closed"].as_str()
    );

    for (id, text) in ["IDEA-9003", "IDEA-9001"].into_iter().zip(open_ideas) {
        c.write_idea(id, &text);
    }
    let linted = lint(&c.root).unwrap();
    let gaps: Vec<_> = linted
        .violations
        .iter()
        .filter(|v| v.rule == "closed-ideas")
        .collect();
    assert_eq!(gaps.len(), 2, "{:?}", linted.violations);
    for (gap, original) in gaps.iter().zip(refused_gaps) {
        assert_eq!(gap.path, original.path);
        assert_eq!(gap.message, original.message);
    }
    c.duplicate_idea("IDEA-9003");
    let linted = lint(&c.root).unwrap();
    assert_eq!(
        linted
            .violations
            .iter()
            .filter(|v| v.rule == "idea-duplicate-id")
            .count(),
        2
    );
    assert_eq!(
        linted
            .violations
            .iter()
            .filter(|v| v.rule == "closed-ideas")
            .count(),
        2
    );
}

#[test]
fn close_journals_every_outcome_word_and_target() {
    let c = idea_fixture::ideas();
    for id in [
        "IDEA-9005",
        "IDEA-9004",
        "IDEA-9003",
        "IDEA-9002",
        "IDEA-9001",
    ] {
        c.add_idea(id);
        assert_eq!(c.cm(&["idea", "admit", id])["ok"], true);
    }
    for args in [
        vec!["IDEA-9005", "landed", "--note", "tool-x #12"],
        vec!["IDEA-9004", "cancelled", "--note", "no longer needed"],
        vec!["IDEA-9003", "superseded", "--by", "IDEA-9005"],
        vec![
            "IDEA-9002",
            "partial",
            "--to",
            "unassigned",
            "--note",
            "one part landed",
        ],
        vec!["IDEA-9001", "deferred", "--to", "epoch-beta"],
    ] {
        let mut command = vec!["idea", "outcome"];
        command.extend(args);
        assert_eq!(c.cm(&command)["ok"], true);
    }
    let before = c.journal_text();
    let closed = commands::close(&cm::host::System, &c.root).unwrap();
    assert!(closed.ok(), "{:?}", closed.violations);
    let entry = cm::journal::entry(
        closed.fields["closed"].as_str().unwrap(),
        "cm close",
        Some(
            "- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n- idea: IDEA-9005 landed\n- idea: IDEA-9004 cancelled\n- idea: IDEA-9003 superseded by IDEA-9005\n- idea: IDEA-9002 partial to unassigned\n- idea: IDEA-9001 deferred to epoch-beta\n",
        ),
    );
    assert_eq!(c.journal_text(), format!("{before}\n{entry}"));
}

#[test]
fn close_looks_up_the_outcome_for_this_epoch_in_history() {
    let c = idea_fixture::ideas();
    c.add_idea("IDEA-9001");
    assert_eq!(c.cm(&["idea", "admit", "IDEA-9001"])["ok"], true);
    c.history(
        "IDEA-9001",
        "- 2026-01-02T09:30:00-05:00: outcome in `epoch-beta`: landed; tool-x #12.",
    );
    let refused = commands::close(&cm::host::System, &c.root).unwrap();
    assert_eq!(rules(&refused), ["idea-outcome-missing"]);
    assert_eq!(
        c.cm(&[
            "idea",
            "outcome",
            "IDEA-9001",
            "deferred",
            "--to",
            "epoch-gamma"
        ])["ok"],
        true
    );
    c.history(
        "IDEA-9001",
        "- 2026-01-03T09:30:00-05:00: outcome in `epoch-beta`: deferred to unassigned.",
    );
    let before = c.journal_text();
    let closed = commands::close(&cm::host::System, &c.root).unwrap();
    assert!(closed.ok(), "{:?}", closed.violations);
    assert_eq!(
        c.journal_text(),
        format!(
            "{before}\n{}",
            cm::journal::entry(
                closed.fields["closed"].as_str().unwrap(),
                "cm close",
                Some(
                    "- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n- idea: IDEA-9001 deferred to epoch-gamma\n"
                ),
            )
        )
    );
}

#[test]
fn close_with_unconfigured_ideas_keeps_lint_clean_and_the_existing_journal_shape() {
    let c = idea_fixture::ideas();
    c.edit_epoch("ideas_dir: ../ideas\n", "ideas: [IDEA-9003, IDEA-9001]\n");
    assert_clean(&c.root);
    let before = c.journal_text();
    let closed = commands::close(&cm::host::System, &c.root).unwrap();
    assert!(closed.ok(), "{:?}", closed.violations);
    assert_eq!(
        c.journal_text(),
        format!(
            "{before}\n{}",
            cm::journal::entry(
                closed.fields["closed"].as_str().unwrap(),
                "cm close",
                Some(
                    "- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n"
                ),
            )
        )
    );
    let resume_path = c.root.join("RESUME.md");
    let resume = fs::read_to_string(&resume_path).unwrap();
    let end = resume.find("\n\n# Resume").unwrap();
    fs::write(
        resume_path,
        format!("> Closed; the successor governs.{}", &resume[end..]),
    )
    .unwrap();
    assert_clean(&c.root);
}

#[test]
fn close_without_an_ideas_key_keeps_the_existing_journal_shape() {
    for ideas_dir in [false, true] {
        let c = idea_fixture::ideas();
        if !ideas_dir {
            c.edit_epoch("ideas_dir: ../ideas\n", "");
        }
        let before = c.journal_text();
        let closed = commands::close(&cm::host::System, &c.root).unwrap();
        assert!(closed.ok(), "{:?}", closed.violations);
        assert_eq!(
            c.journal_text(),
            format!(
                "{before}\n{}",
                cm::journal::entry(
                    closed.fields["closed"].as_str().unwrap(),
                    "cm close",
                    Some(
                        "- sealed: unit init, launch and advance refuse from here; the journal stays open for follow-up observations\n"
                    ),
                )
            )
        );
    }
}

#[test]
fn close_seals_the_campaign() {
    let host = Fake::at("2026-10-15T18:00:00Z");
    let c = campaign(&host);
    let (repo, _) = repo(c.root.parent().unwrap());
    unit_with_step(&c, &host, &format!("Work in `{}`.\n", repo.display()));
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::launch(
            &host,
            &c.root,
            &parse_target(step, true).unwrap(),
            Some(&repo)
        )
        .unwrap()
        .ok()
    );
    fs::write(
        c.root.join("OWNER-QUEUE.md"),
        "# Owner queue\n\n## Pick one\n\nContext: two paths.\n\nRecommendation: yes\n\nAnswer:\n",
    )
    .unwrap();

    let refused = commands::close(&host, &c.root).unwrap();
    assert_eq!(
        rules(&refused),
        vec!["closed-queue", "closed-track", "closed-track"],
        "{:#?}",
        refused.violations
    );
    assert!(store::load_campaign(&c.root).unwrap().closed.is_none());

    fs::write(c.root.join("OWNER-QUEUE.md"), "# Owner queue\n").unwrap();
    assert!(advance(&host, &c, step, "blocked:superseded", Evidence::default()).ok());
    assert!(
        advance(
            &host,
            &c,
            "01-first/alpha",
            "blocked:superseded",
            Evidence::default()
        )
        .ok()
    );
    let closed = commands::close(&host, &c.root).unwrap();
    assert!(closed.ok(), "{:#?}", closed.violations);
    assert_eq!(
        store::load_campaign(&c.root).unwrap().closed.as_deref(),
        Some("2026-10-15T18:00Z")
    );
    let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
    assert!(journal.contains("## 2026-10-15T18:00Z — cm close\n"));

    assert_eq!(rules(&lint(&c.root).unwrap()), vec!["resume-loop"]);
    let resume = fs::read_to_string(c.root.join("RESUME.md")).unwrap();
    let start = resume.find("> ```").unwrap();
    let end = resume.find("\n\n# Resume").unwrap();
    fs::write(
        c.root.join("RESUME.md"),
        format!(
            "{}> Closed; the successor governs.{}",
            &resume[..start],
            &resume[end..]
        ),
    )
    .unwrap();
    assert_clean(&c.root);

    assert_eq!(
        rules(&advance(&host, &c, step, "launched", Evidence::default())),
        vec!["advance-refused", "campaign-closed"]
    );
    assert_eq!(
        rules(&commands::init(&host, &c.root.join("01-first/beta")).unwrap()),
        vec!["campaign-closed"]
    );
    assert_eq!(
        rules(&commands::close(&host, &c.root).unwrap()),
        vec!["campaign-closed"]
    );
    assert!(
        commands::journal(&host, &c.root, "follow-up", None)
            .unwrap()
            .ok()
    );
    assert_clean(&c.root);
}

#[test]
fn init_writes_a_kickoff_once_and_repairs_a_missing_kickoff() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let step = "01-first/alpha/BUILD";
    let file = kickoff_fixture::file("Build the feature.\n");
    let init = || {
        Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args(["--json", "init", step, "--lane", "pi", "--kickoff-file"])
            .arg(file.path())
            .output()
            .unwrap()
    };
    let first = init();
    assert!(first.status.success(), "{first:?}");
    let fields: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(fields["kickoff"], format!("{step}/kickoff.md"));
    assert!(fields["journal"].is_string());
    let kickoff = c.root.join(step).join("kickoff.md");
    assert_eq!(
        fs::read_to_string(&kickoff).unwrap(),
        "Build the feature.\n"
    );
    fs::write(file.path(), "A different file.\n").unwrap();
    let before = fs::read(c.root.join("JOURNAL.md")).unwrap();
    let kept = init();
    assert!(kept.status.success(), "{kept:?}");
    let fields: serde_json::Value = serde_json::from_slice(&kept.stdout).unwrap();
    assert_eq!(fields["kickoff_kept"], true);
    assert_eq!(fields["kickoff"], format!("{step}/kickoff.md"));
    assert_eq!(
        fs::read_to_string(&kickoff).unwrap(),
        "Build the feature.\n"
    );
    assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), before);
    fs::remove_dir_all(c.root.join(step)).unwrap();
    fs::remove_file(c.root.join("01-first/alpha/STATE.md")).unwrap();
    let repaired = init();
    assert!(repaired.status.success(), "{repaired:?}");
    let fields: serde_json::Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(
        fields["repaired"],
        serde_json::json!([
            format!("{step}/"),
            format!("{step}/kickoff.md"),
            "01-first/alpha/STATE.md"
        ])
    );
    assert_eq!(fs::read_to_string(&kickoff).unwrap(), "A different file.\n");
    assert_clean(&c.root);
}

#[test]
fn init_accepts_a_kickoff_on_stdin_and_rejects_empty_input() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    for text in ["", " \n\t", "Do the work.\n"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args([
                "--json",
                "init",
                "01-first/alpha/BUILD",
                "--lane",
                "pi",
                "--kickoff-file",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.success(), !text.trim().is_empty());
        if text.trim().is_empty() {
            assert!(!c.root.join("01-first/alpha").exists());
            let fields: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(fields["violations"][0]["rule"], "kickoff-file");
        }
    }
    assert_eq!(
        fs::read_to_string(c.root.join("01-first/alpha/BUILD/kickoff.md")).unwrap(),
        "Do the work.\n"
    );
}

#[test]
fn release_init_inherits_the_newest_declared_repository_and_tag_pattern() {
    use cm::commands::{Declaration, Ships};
    use cm::state::Lane;

    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let mut declaration = Declaration {
        lane: Some(Lane::Pi),
        ships: Ships {
            github: Some("example/old".parse().unwrap()),
            tag: Some("v{version}".into()),
        },
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/widget/1.9.0"), &declaration)
            .unwrap()
            .ok()
    );
    declaration.ships = Ships {
        github: Some("example/project".parse().unwrap()),
        tag: Some("cm-{version}".into()),
    };
    assert!(
        commands::declare(
            &host,
            &c.root.join("00-release/widget/1.10.0"),
            &declaration
        )
        .unwrap()
        .ok()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(&c.root)
        .args(["init", "00-release/widget/1.11.0", "--lane", "pi"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let unit = store::load_unit(&c.root.join("00-release/widget")).unwrap();
    let inherited = &unit.steps["1.11.0"];
    assert_eq!(
        inherited.github.as_ref().unwrap().to_string(),
        "example/project"
    );
    assert_eq!(inherited.tag.as_ref().unwrap().as_str(), "cm-1.11.0");
    let retry = Declaration {
        lane: Some(Lane::Pi),
        kickoff: Some("Release prep.\n".into()),
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/widget/1.10.0"), &retry)
            .unwrap()
            .ok()
    );
    assert_eq!(
        fs::read_to_string(c.root.join("00-release/widget/1.10.0/kickoff.md")).unwrap(),
        "Release prep.\n"
    );
    let explicit = Declaration {
        lane: Some(Lane::Pi),
        ships: Ships {
            github: Some("example/override".parse().unwrap()),
            tag: None,
        },
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/widget/1.12.0"), &explicit)
            .unwrap()
            .ok()
    );
    let unit = store::load_unit(&c.root.join("00-release/widget")).unwrap();
    assert_eq!(
        unit.steps["1.12.0"].tag.as_ref().unwrap().as_str(),
        "v1.12.0"
    );
    let defaults = Declaration {
        lane: Some(Lane::Pi),
        ..Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join("00-release/other/1.0.0"), &defaults)
            .unwrap()
            .ok()
    );
    let other = store::load_unit(&c.root.join("00-release/other")).unwrap();
    assert_eq!(other.steps["1.0.0"].github, None);
    assert_eq!(
        other.steps["1.0.0"].tag.as_ref().unwrap().as_str(),
        "v1.0.0"
    );
}

#[test]
fn init_reports_a_preexisting_kickoff_as_kept() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let step = "01-first/alpha/BUILD";
    c.kickoff(step, "Existing work.\n");
    let declaration = commands::Declaration {
        lane: Some(cm::state::Lane::Pi),
        kickoff: Some("Different work.\n".into()),
        ..commands::Declaration::default()
    };
    let outcome = commands::declare(&host, &c.root.join(step), &declaration).unwrap();
    assert!(outcome.ok(), "{:?}", outcome.violations);
    assert_eq!(outcome.fields["kickoff_kept"], true);
    assert_eq!(outcome.fields["kickoff"], format!("{step}/kickoff.md"));
    assert_eq!(
        fs::read_to_string(c.root.join(step).join("kickoff.md")).unwrap(),
        "Existing work.\n"
    );
}

#[test]
fn inherited_defaults_refuse_git_invalid_tags_saved_by_older_versions() {
    use cm::commands::Declaration;
    use cm::state::Lane;

    let host = Fake::at("2026-10-08T07:04:00Z");
    for tag in [
        "release/.hidden/1.2.3",
        "release/part.lock/1.2.3",
        "release/1.2.3.lock",
    ] {
        let c = campaign(&host);
        let declaration = Declaration {
            lane: Some(Lane::Pi),
            ..Declaration::default()
        };
        assert!(
            commands::declare(&host, &c.root.join("00-release/widget/1.2.3"), &declaration)
                .unwrap()
                .ok()
        );
        c.legacy_release_tag("00-release/widget", "1.2.3", tag);
        let state_path = store::state_path(&c.root.join("00-release/widget"));
        let before = fs::read(&state_path).unwrap();
        let refused =
            commands::declare(&host, &c.root.join("00-release/widget/1.2.4"), &declaration)
                .unwrap();
        assert_eq!(rules(&refused), vec!["release-tag"]);
        assert!(!c.root.join("00-release/widget/1.2.4").exists());
        assert_eq!(fs::read(&state_path).unwrap(), before);
    }
}

#[test]
fn legacy_release_tags_load_lint_and_can_be_repaired_by_amend() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let step = "00-release/widget/1.2.3";
    let declaration = commands::Declaration {
        lane: Some(cm::state::Lane::Pi),
        ..commands::Declaration::default()
    };
    assert!(
        commands::declare(&host, &c.root.join(step), &declaration)
            .unwrap()
            .ok()
    );
    let baseline = lint(&c.root).unwrap();
    c.legacy_release_tag("00-release/widget", "1.2.3", "release/1.2.3.lock");
    let unit_dir = c.root.join("00-release/widget");
    let unit = store::load_unit(&unit_dir).unwrap();
    assert_eq!(
        unit.steps["1.2.3"].tag.as_ref().unwrap().as_str(),
        "release/1.2.3.lock"
    );
    let legacy_lint = lint(&c.root).unwrap();
    assert_eq!(legacy_lint.violations, baseline.violations);
    assert_eq!(legacy_lint.fields["warnings"], baseline.fields["warnings"]);
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(&c.root)
        .args(["--json", "amend", step, "--tag", "v{version}"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let unit = store::load_unit(&unit_dir).unwrap();
    assert_eq!(unit.steps["1.2.3"].tag.as_ref().unwrap().as_str(), "v1.2.3");
    assert_clean(&c.root);
}

#[test]
fn kickoff_inputs_accept_the_byte_limit_and_refuse_larger_files_and_stdin() {
    let host = Fake::at("2026-10-08T07:04:00Z");
    let c = campaign(&host);
    let limit = 1024 * 1024;
    for command in ["init", "amend"] {
        for source in ["file", "stdin"] {
            for size in [limit, limit + 1] {
                let step = format!("01-first/alpha/{command}-{source}-{size}");
                if command == "amend" {
                    let declaration = commands::Declaration {
                        lane: Some(cm::state::Lane::Pi),
                        kickoff: Some("Existing work.\n".into()),
                        ..commands::Declaration::default()
                    };
                    assert!(
                        commands::declare(&host, &c.root.join(&step), &declaration)
                            .unwrap()
                            .ok()
                    );
                }
                let input = kickoff_fixture::file(&"x".repeat(size));
                let before = fs::read(c.root.join("JOURNAL.md")).unwrap();
                let state_path = store::state_path(&c.root.join("01-first/alpha"));
                let state_before = fs::read(&state_path).ok();
                let mut cli = Command::new(env!("CARGO_BIN_EXE_cm"));
                cli.current_dir(&c.root).args(["--json", command, &step]);
                if command == "init" {
                    cli.args(["--lane", "pi"]);
                }
                cli.arg("--kickoff-file");
                if source == "stdin" {
                    cli.arg("-").stdin(Stdio::from(input.reopen().unwrap()));
                } else {
                    cli.arg(input.path());
                }
                let output = cli.output().unwrap();
                let kickoff = c.root.join(&step).join("kickoff.md");
                if size == limit {
                    assert!(output.status.success(), "{output:?}");
                    assert_eq!(fs::read(&kickoff).unwrap(), vec![b'x'; limit]);
                } else {
                    assert_eq!(output.status.code(), Some(1));
                    let fields: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(fields["violations"][0]["rule"], "kickoff-size");
                    assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), before);
                    assert_eq!(fs::read(&state_path).ok(), state_before);
                    if command == "amend" {
                        assert_eq!(fs::read(&kickoff).unwrap(), b"Existing work.\n");
                    } else {
                        assert!(!c.root.join(&step).exists());
                    }
                }
            }
        }
    }
}

#[test]
fn coordinator_steps_without_a_pr_finish_with_evidence_from_drafted_and_launched() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    for launched in [false, true] {
        let c = campaign(&host);
        let step = "01-first/alpha/BUILD";
        let declared = commands::declare(
            &host,
            &c.root.join(step),
            &commands::Declaration {
                lane: Some(Lane::Coordinator),
                ..commands::Declaration::default()
            },
        )
        .unwrap();
        assert!(declared.ok(), "{:?}", declared.violations);
        c.kickoff(step, "Go.\n");
        if launched {
            assert!(
                commands::launch(&host, &c.root, &target(step), None)
                    .unwrap()
                    .ok()
            );
        }
        let missing = advance(&host, &c, step, "done", Evidence::default());
        assert_eq!(rules(&missing), vec!["evidence-missing"]);
        let file = c.root.join(step).join("result.md");
        fs::write(&file, "Completed.\n").unwrap();
        let finished = commands::advance(
            &host,
            &c.root,
            &target(step),
            &Phase::Done,
            &Evidence::default(),
            Some(&file),
            None,
        )
        .unwrap();
        assert!(finished.ok(), "{:?}", finished.violations);
        let unit = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
        assert_eq!(unit.steps["BUILD"].track.state, Phase::Done);
        assert_eq!(
            unit.steps["BUILD"]
                .track
                .evidence
                .file
                .as_ref()
                .unwrap()
                .as_str(),
            "01-first/alpha/BUILD/result.md"
        );
    }
}

#[test]
fn coordinator_release_and_codex_steps_keep_the_code_completion_ladder() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    for (step, lane, legacy) in [
        ("00-release/cm/1.0.0", Lane::Coordinator, false),
        ("00-release/cm/1.0.0", Lane::Coordinator, true),
        ("01-first/alpha/BUILD", Lane::Codex, false),
    ] {
        for launched in [false, true] {
            let c = campaign(&host);
            if legacy {
                c.legacy_release(&host, step);
                let amended = commands::amend(
                    &host,
                    &c.root,
                    &target(step),
                    &commands::Amendment {
                        lane: Some(lane),
                        ..commands::Amendment::default()
                    },
                )
                .unwrap();
                assert!(amended.ok(), "{:?}", amended.violations);
                let unit = store::load_unit(&c.root.join(target(step).unit)).unwrap();
                assert!(unit.steps["1.0.0"].tag.is_none());
            } else {
                assert!(
                    commands::declare(
                        &host,
                        &c.root.join(step),
                        &commands::Declaration {
                            lane: Some(lane),
                            ..commands::Declaration::default()
                        },
                    )
                    .unwrap()
                    .ok()
                );
            }
            c.kickoff(step, "Go.\n");
            if launched {
                assert!(
                    commands::launch(&host, &c.root, &target(step), None)
                        .unwrap()
                        .ok()
                );
            }
            let file = c.root.join(step).join("result.md");
            fs::write(&file, "Completed.\n").unwrap();
            let refused = commands::advance(
                &host,
                &c.root,
                &target(step),
                &Phase::Done,
                &Evidence::default(),
                Some(&file),
                None,
            )
            .unwrap();
            assert_eq!(rules(&refused), vec!["done-code-step"], "{step}");
            let unit = store::load_unit(&c.root.join(target(step).unit)).unwrap();
            assert_eq!(
                unit.steps[target(step).step.as_deref().unwrap()]
                    .track
                    .state,
                if launched {
                    Phase::Launched
                } else {
                    Phase::Drafted
                }
            );
        }
    }
}

#[test]
fn coordinator_pr_steps_keep_their_ladder_and_gates() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    let step = "01-first/alpha/BUILD";
    assert!(
        commands::declare(
            &host,
            &c.root.join(step),
            &commands::Declaration {
                lane: Some(Lane::Coordinator),
                ..commands::Declaration::default()
            },
        )
        .unwrap()
        .ok()
    );
    c.kickoff(step, "Go.\n");
    assert!(
        commands::launch(&host, &c.root, &target(step), Some(&repo))
            .unwrap()
            .ok()
    );
    let units = cm::plan::load_units(&c.root).unwrap();
    let gate = cm::plan::gate(&units, &After::Merged(step.into()));
    assert_eq!(gate.needs(), Phase::Done);
    assert!(!gate.met());
    assert!(
        advance(
            &host,
            &c,
            step,
            "pr-open",
            Evidence {
                pr: Some("https://github.com/o/r/pull/7".into()),
                sha: Some(sha.clone()),
                ..Evidence::default()
            }
        )
        .ok()
    );
    let units = cm::plan::load_units(&c.root).unwrap();
    let gate = cm::plan::gate(&units, &After::PrOpen(step.into()));
    assert_eq!(gate.needs(), Phase::PrOpen);
    assert!(gate.met());
    assert!(
        advance(
            &host,
            &c,
            step,
            "merged",
            Evidence {
                sha: Some(sha),
                ..Evidence::default()
            }
        )
        .ok()
    );
    for (keep_pr, keep_sha) in [(true, false), (false, true), (true, true)] {
        let mut units = cm::plan::load_units(&c.root).unwrap();
        let evidence = &mut units
            .get_mut("01-first/alpha")
            .unwrap()
            .steps
            .get_mut("BUILD")
            .unwrap()
            .track
            .evidence;
        if !keep_pr {
            evidence.pr = None;
        }
        if !keep_sha {
            evidence.sha = None;
        }
        let gate = cm::plan::gate(&units, &After::Merged(step.into()));
        assert_eq!(gate.needs(), Phase::Merged);
        assert!(gate.met());
    }
    git(&repo, &["tag", "v0.1.0"]);
    assert!(
        advance(
            &host,
            &c,
            step,
            "released",
            Evidence {
                tag: Some("v0.1.0".into()),
                ..Evidence::default()
            }
        )
        .ok()
    );
    assert!(
        advance(
            &host,
            &c,
            step,
            "deployed",
            Evidence {
                digest: Some(format!("sha256:{}", "a".repeat(64))),
                ..Evidence::default()
            }
        )
        .ok()
    );
    let done = advance(&host, &c, step, "done", Evidence::default());
    assert!(done.ok(), "{:?}", done.violations);
    let unit = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    assert_eq!(unit.steps["BUILD"].track.state, Phase::Done);
}

#[test]
fn coordinator_steps_can_declare_and_amend_pr_parts() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let step = "01-first/alpha/BUILD";
    let declared = commands::declare(
        &host,
        &c.root.join(step),
        &commands::Declaration {
            lane: Some(Lane::Coordinator),
            parts: vec!["first".into()],
            ..commands::Declaration::default()
        },
    )
    .unwrap();
    assert!(declared.ok(), "{:?}", declared.violations);
    let amended = commands::amend(
        &host,
        &c.root,
        &target(step),
        &commands::Amendment {
            parts: vec!["second".into()],
            ..commands::Amendment::default()
        },
    )
    .unwrap();
    assert!(amended.ok(), "{:?}", amended.violations);
    let unit = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    assert_eq!(
        unit.steps["BUILD"]
            .parts
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn resolving_or_withdrawing_the_last_watch_keeps_a_release_on_the_shipping_ladder() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    for withdraw in [false, true] {
        let c = campaign(&host);
        let (repo, sha) = repo(c.root.parent().unwrap());
        git(&repo, &["tag", "v1.0.0"]);
        let step = "00-release/cm/1.0.0";
        let target = target(step);
        assert!(
            commands::declare(
                &host,
                &c.root.join(step),
                &commands::Declaration {
                    lane: Some(Lane::Coordinator),
                    ..commands::Declaration::default()
                },
            )
            .unwrap()
            .ok()
        );
        c.kickoff(step, "Release check.\n");
        let released = commands::advance(
            &host,
            &c.root,
            &target,
            &Phase::Released,
            &Evidence {
                sha: Some(sha),
                tag: Some("v1.0.0".into()),
                ..Evidence::default()
            },
            None,
            Some(&repo),
        )
        .unwrap();
        assert!(released.ok(), "{:?}", released.violations);
        assert!(
            cm::holds::watch_add(&host, &c.root, &target, "log:release", "2026-10-09T07:00Z",)
                .unwrap()
                .ok()
        );
        let deployed = Evidence {
            digest: Some(format!("sha256:{}", "a".repeat(64))),
            ..Evidence::default()
        };
        assert_eq!(
            rules(&advance(&host, &c, step, "deployed", deployed.clone())),
            ["watch-open"]
        );
        let closed = if withdraw {
            cm::holds::watch_withdraw(&host, &c.root, &target, 1, "signal replaced")
        } else {
            cm::holds::watch_resolve(
                &host,
                &c.root,
                &target,
                1,
                &c.root.join(step).join("kickoff.md"),
            )
        }
        .unwrap();
        assert!(closed.ok(), "{:?}", closed.violations);
        let unit = store::load_unit(&c.root.join(&target.unit)).unwrap();
        let release = &unit.steps["1.0.0"];
        assert_eq!(release.unresolved_watches(&c.root).count(), 0);
        assert_eq!(release.track.state, Phase::Released);
        assert_eq!(
            rules(&advance(&host, &c, step, "done", Evidence::default())),
            ["done-code-step"]
        );
        assert!(advance(&host, &c, step, "deployed", deployed).ok());
        let done = advance(&host, &c, step, "done", Evidence::default());
        assert!(done.ok(), "{:?}", done.violations);
        let unit = store::load_unit(&c.root.join(&target.unit)).unwrap();
        assert_eq!(unit.steps["1.0.0"].track.state, Phase::Done);
    }
}

#[test]
fn sync_rewrites_only_resume_status_and_is_idempotent() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let forge = c.resume_releases(&host);
    let path = c.root.join("RESUME.md");
    fs::write(&path, resume_fixture::resume_text("Stale status.\r\n")).unwrap();

    let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    let status = "| State | Steps |\n|---|---|\n| drafted | 2 |\n| released | 2 |\n| deployed | 1 |\n| done | 1 |\n| superseded | 1 |\n| blocked | 1 |\n\n| Release line | Released | Deployed |\n|---|---|---|\n| alpha | 2.0.0 | — |\n| gizmo | 3.0.0 | 3.0.0 |\n| widget | 1.10.0 | 1.9.0 |\n\nEpoch status: working.\n";
    let expected = resume_fixture::resume_text(status).into_bytes();
    assert_eq!(fs::read(&path).unwrap(), expected);
    let later = Fake::at("2026-10-08T08:00:00Z");
    let out = cm::sync::sync(&later, &forge, &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert_eq!(fs::read(&path).unwrap(), expected);
}

#[test]
fn sync_leaves_resume_without_markers_unchanged() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let text = resume_fixture::resume_text("Owner status.\r\n")
        .replace("<!-- cm:status -->\r\n", "")
        .replace("<!-- cm:status-end -->\r\n", "");
    let path = c.root.join("RESUME.md");
    fs::write(&path, &text).unwrap();
    let out = cm::sync::sync(&host, &forge_fixture::FakeForge::default(), &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
    assert_clean(&c.root);
}

#[test]
fn sync_renders_empty_state_and_current_epoch_status() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let path = c.root.join("RESUME.md");
    fs::write(&path, resume_fixture::resume_text("Stale.\n")).unwrap();
    for epoch in [
        None,
        Some("OPEN"),
        Some("PLANNING"),
        Some("WORKING"),
        Some("CLOSED"),
    ] {
        if let Some(epoch) = epoch {
            fs::write(
                c.root.join("EPOCH.md"),
                format!("---\nstatus: {epoch}\n---\n"),
            )
            .unwrap();
        }
        let out = cm::sync::sync(&host, &forge_fixture::FakeForge::default(), &c.root).unwrap();
        assert!(out.ok(), "{:?}", out.violations);
        let epoch = epoch.unwrap_or("unknown").to_lowercase();
        let status = format!(
            "| State | Steps |\n|---|---|\n| — | 0 |\n\nNo releases recorded.\n\nEpoch status: {epoch}.\n"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            resume_fixture::resume_text(&status).as_bytes()
        );
    }
}

#[test]
fn lint_reports_malformed_resume_status_markers_and_sync_preserves_them() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let original = resume_fixture::resume_text("Old status.\n");
    let outside = original
        .replace("<!-- cm:status -->\r\n", "")
        .replace("<!-- cm:status-end -->\r\n", "")
        + "\r\n<!-- cm:status -->\r\nOld status.\r\n<!-- cm:status-end -->\r\n";
    for text in [
        original.replace("<!-- cm:status -->\r\n", ""),
        original.replace("<!-- cm:status-end -->\r\n", ""),
        original
            .replace("cm:status-end", "cm:temporary")
            .replace("cm:status", "cm:status-end")
            .replace("cm:temporary", "cm:status"),
        original.replace(
            "<!-- cm:status -->",
            "<!-- cm:status -->\n<!-- cm:status -->",
        ),
        outside,
    ] {
        fs::write(c.root.join("RESUME.md"), &text).unwrap();
        let out = lint(&c.root).unwrap();
        assert_eq!(rules(&out), vec!["resume-status"], "{:?}", out.violations);
        assert_eq!(out.violations[0].path, "RESUME.md");
        let out = cm::sync::sync(&host, &forge_fixture::FakeForge::default(), &c.root).unwrap();
        assert!(out.ok(), "{:?}", out.violations);
        assert_eq!(fs::read(c.root.join("RESUME.md")).unwrap(), text.as_bytes());
    }
}

#[test]
fn close_and_epoch_closed_refresh_resume_without_sync() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    c.resume_working_epoch();
    let path = c.root.join("RESUME.md");
    fs::write(&path, resume_fixture::resume_text("Stale status.\n")).unwrap();
    let out = cm::sync::sync(&host, &forge_fixture::FakeForge::default(), &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    let working = fs::read_to_string(&path).unwrap();
    assert!(working.contains("Epoch status: working.\n"));
    let expected = working.replace("Epoch status: working.", "Epoch status: closed.");

    let out = commands::close(&host, &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);

    // The epoch transition must refresh stale generated content itself, too.
    fs::write(&path, &working).unwrap();
    let out = cm::epoch::epoch(&host, &c.root, cm::epoch::Status::Closed, None, None).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
}

#[test]
fn sync_and_lint_ignore_fenced_marker_examples() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    for fence in ["```", "~~~"] {
        for real_span in [false, true] {
            let c = campaign(&host);
            let status = real_span.then_some("Stale status.\n");
            let text = resume_fixture::resume_with_marker_example(fence, status);
            let path = c.root.join("RESUME.md");
            fs::write(&path, &text).unwrap();
            assert_clean(&c.root);
            let out = cm::sync::sync(&host, &forge_fixture::FakeForge::default(), &c.root).unwrap();
            assert!(out.ok(), "{:?}", out.violations);
            let expected = if real_span {
                resume_fixture::resume_with_marker_example(
                    fence,
                    Some(
                        "| State | Steps |\n|---|---|\n| — | 0 |\n\nNo releases recorded.\n\nEpoch status: unknown.\n",
                    ),
                )
            } else {
                text
            };
            assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        }
    }
}

#[test]
fn superseded_records_one_or_two_successors_in_state_journal_and_card() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    for (by, successors) in [
        ("NEW", vec!["01-first/alpha/NEW"]),
        (
            "NEW,02-next/beta/NEXT",
            vec!["01-first/alpha/NEW", "02-next/beta/NEXT"],
        ),
        (
            "NEW,01-first/alpha/NEW,02-next/beta/NEXT",
            vec!["01-first/alpha/NEW", "02-next/beta/NEXT"],
        ),
    ] {
        let c = campaign(&host);
        c.supersession(&host);
        let before = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args([
                "--json",
                "advance",
                "01-first/alpha/OLD",
                "superseded",
                "--evidence",
                "replacement.md",
                "--by",
                by,
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result["evidence"]["superseded_by"],
            serde_json::json!(successors)
        );

        let dir = c.root.join("01-first/alpha");
        let state = store::load_unit(&dir).unwrap();
        let retired = &state.steps["OLD"].track;
        assert_eq!(state.schema, cm::state::SCHEMA);
        assert_eq!(retired.state, Phase::Superseded);
        assert_eq!(retired.evidence.superseded_by, successors);
        let moved = state.history.last().unwrap();
        assert_eq!(moved.evidence.superseded_by, successors);
        let round_trip: UnitState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(round_trip, state);
        let summary = format!(
            "file replacement.md; superseded by {}",
            successors.join(", ")
        );
        let rendered = fs::read_to_string(dir.join("STATE.md")).unwrap();
        assert_eq!(
            rendered
                .lines()
                .find(|line| line.starts_with("| OLD |"))
                .unwrap(),
            format!(
                "| OLD | superseded | {} | 0 | — | {summary} |",
                retired.since
            )
        );
        let journal = fs::read_to_string(c.root.join("JOURNAL.md")).unwrap();
        assert_eq!(
            journal.strip_prefix(&before).unwrap().trim_start(),
            format!(
                "{}\n\n- evidence: {summary}\n",
                result["journal"].as_str().unwrap()
            )
        );

        let synced = Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args(["--json", "sync"])
            .output()
            .unwrap();
        assert!(synced.status.success(), "{synced:?}");
        let card = fs::read_to_string(c.root.join("CARD.md")).unwrap();
        let links: Vec<_> = successors.iter().map(|step| format!("`{step}`")).collect();
        assert_eq!(
            card.lines()
                .find(|line| line.starts_with("- `01-first/alpha/OLD`"))
                .unwrap(),
            format!("- `01-first/alpha/OLD` superseded by {}", links.join(", "))
        );
    }
}

#[test]
fn superseded_refuses_undeclared_self_or_invalid_successors_without_writing() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    c.supersession(&host);
    let before = c.files();
    for by in [
        "NEW,MISSING",
        "03-absent/unit/MISSING",
        "OLD",
        "01-first/alpha/OLD",
        "NEW,01-first/alpha/OLD",
        "01-first/alpha",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args([
                "--json",
                "advance",
                "01-first/alpha/OLD",
                "superseded",
                "--evidence",
                "replacement.md",
                "--by",
                by,
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{by}: {output:?}");
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["violations"][0]["rule"], "superseded-by", "{result}");
        assert_eq!(c.files(), before, "{by}");
    }
}

#[test]
fn by_is_a_usage_error_outside_superseded_and_does_not_replace_evidence() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    c.supersession(&host);
    let before = c.files();
    for state in [
        "drafted",
        "launched",
        "pr-open",
        "merged",
        "released",
        "deployed",
        "done",
        "blocked:waiting",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args(["advance", "01-first/alpha/OLD", state, "--by", "NEW"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{state}: {output:?}");
        assert_eq!(c.files(), before, "{state}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(&c.root)
        .args([
            "--json",
            "advance",
            "01-first/alpha/OLD",
            "superseded",
            "--by",
            "NEW",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["violations"][0]["rule"], "evidence-missing");
    assert_eq!(c.files(), before);
}

#[test]
fn rollout_init_persists_its_kind_and_refuses_study_or_release() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let step = "01-first/alpha/ROLLOUT";
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .current_dir(&c.root)
        .args(["--json", "init", step, "--lane", "pi", "--kind", "rollout"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["kind"], "rollout");
    assert_eq!(result["study"], false);
    let dir = c.root.join("01-first/alpha");
    let state = store::load_unit(&dir).unwrap();
    assert_eq!(state.schema, cm::state::SCHEMA);
    assert_eq!(state.steps["ROLLOUT"].work, Work::Rollout);
    let json = serde_json::to_value(&state).unwrap();
    assert_eq!(json["steps"]["ROLLOUT"]["work"], "rollout");
    assert_eq!(serde_json::from_value::<UnitState>(json).unwrap(), state);
    let rendered = fs::read_to_string(dir.join("STATE.md")).unwrap();
    assert_eq!(
        rendered
            .lines()
            .find(|line| line.starts_with("| ROLLOUT | pi"))
            .unwrap(),
        "| ROLLOUT | pi, rollout | — |"
    );
    let before = c.files();
    for (target, extra) in [(step, vec!["--study"]), ("00-release/cm/1.0.0", vec![])] {
        let refused = Command::new(env!("CARGO_BIN_EXE_cm"))
            .current_dir(&c.root)
            .args([
                "--json", "init", target, "--lane", "pi", "--kind", "rollout",
            ])
            .args(extra)
            .output()
            .unwrap();
        assert!(!refused.status.success(), "{refused:?}");
        assert_eq!(c.files(), before);
    }
}

#[test]
fn rollouts_deploy_from_merged_with_a_sha_and_sync_receipt_without_a_digest() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    for lane in [Lane::Pi, Lane::Codex, Lane::Owner, Lane::Coordinator] {
        let c = campaign(&host);
        let (repo, sha) = repo(c.root.parent().unwrap());
        let step = "01-first/alpha/ROLLOUT";
        c.merged_rollout(&host, step, lane, &repo, &sha);
        let file = c.sync_receipt(&sha);
        let deployed = commands::advance(
            &host,
            &c.root,
            &target(step),
            &Phase::Deployed,
            &Evidence {
                sha: Some(sha[..10].into()),
                ..Evidence::default()
            },
            Some(&file),
            None,
        )
        .unwrap();
        assert!(deployed.ok(), "{:?}", deployed.violations);
        let state = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
        let track = &state.steps["ROLLOUT"].track;
        assert_eq!(track.state, Phase::Deployed);
        assert_eq!(track.evidence.sha.as_deref(), Some(sha.as_str()));
        assert_eq!(
            track.evidence.file.as_ref().unwrap().as_str(),
            "sync-receipt.md"
        );
        assert_eq!(track.evidence.digest, None);
        assert_eq!(track.evidence.tag, None);
        assert_eq!(state.history.last().unwrap().from, Phase::Merged);
        assert_clean(&c.root);
    }
}

#[test]
fn rollout_deployment_refuses_missing_or_invalid_receipts_and_missing_shas() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    let step = "01-first/alpha/ROLLOUT";
    c.merged_rollout(&host, step, Lane::Pi, &repo, &sha);
    let file = c.sync_receipt(&sha);
    let missing = c.root.join("missing.md");
    let unrelated = campaign_fixture::unrelated_commit(&repo);
    let before = c.files();
    for (given_sha, receipt, expected) in [
        (Some(sha.clone()), None, "evidence-missing"),
        (Some(sha.clone()), Some(&missing), "evidence-file"),
        (None, Some(&file), "evidence-missing"),
        (Some(unrelated), Some(&file), "merge-lineage"),
    ] {
        let refused = commands::advance(
            &host,
            &c.root,
            &target(step),
            &Phase::Deployed,
            &Evidence {
                sha: given_sha,
                ..Evidence::default()
            },
            receipt.map(|p| p.as_path()),
            None,
        )
        .unwrap();
        assert_eq!(rules(&refused), vec![expected]);
        assert_eq!(c.files(), before);
    }
    let released = advance(&host, &c, step, "released", Evidence::default());
    assert!(rules(&released).contains(&"advance-refused"));
    assert_eq!(c.files(), before);
}

#[test]
fn normal_steps_still_refuse_to_skip_released() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    let step = "01-first/alpha/BUILD";
    c.merged_step(&host, step, Lane::Pi, &repo, &sha);
    let before = c.files();
    let refused = advance(
        &host,
        &c,
        step,
        "deployed",
        Evidence {
            digest: Some(format!("sha256:{}", "d".repeat(64))),
            ..Evidence::default()
        },
    );
    assert_eq!(rules(&refused), vec!["advance-refused"]);
    assert_eq!(c.files(), before);
}

#[test]
fn sync_leaves_rollouts_merged_even_when_a_containing_release_is_deployed() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    c.merged_rollout(&host, "01-first/alpha/ROLLOUT", Lane::Pi, &repo, &sha);
    c.merged_step(&host, "01-first/alpha/BUILD", Lane::Pi, &repo, &sha);
    c.release_receipt(&host, &repo, &sha);
    let mut forge = forge_fixture::FakeForge::default();
    forge
        .tags
        .insert(("o/cm".into(), "v1.0.0".into()), [sha].into());
    let synced = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert!(synced.ok(), "{:?}", synced.violations);
    let state = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    assert_eq!(state.steps["ROLLOUT"].track.state, Phase::Merged);
    assert_eq!(state.steps["ROLLOUT"].track.evidence.tag, None);
    assert_eq!(state.steps["BUILD"].track.state, Phase::Deployed);
}

#[test]
fn multipart_rollouts_check_each_receipt_after_deployment() {
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let (repo, sha) = repo(c.root.parent().unwrap());
    let step = "01-first/alpha/ROLLOUT";
    c.merged_rollout(&host, step, Lane::Pi, &repo, &sha);
    c.rollout_parts(
        step,
        &[
            ("east", Some(lifecycle_fixture::MERGED_AT)),
            ("west", Some("2026-10-08T07:05:00Z")),
        ],
    );
    let files = c.part_sync_receipts(&host, step, &sha);
    for ((part, expected), file) in [("east", Phase::Merged), ("west", Phase::Deployed)]
        .into_iter()
        .zip(&files)
    {
        let deployed = commands::advance_gated(
            &host,
            &c.root,
            &target(step),
            commands::Advance {
                to: &Phase::Deployed,
                given: &Evidence {
                    sha: Some(sha.clone()),
                    ..Evidence::default()
                },
                file: Some(file),
                repo: None,
                part: Some(part),
                by: &[],
            },
            &commands::MergeGate::default(),
        )
        .unwrap();
        assert!(deployed.ok(), "{:?}", deployed.violations);
        let state = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
        assert_eq!(state.steps["ROLLOUT"].track.state, expected);
        assert_eq!(state.steps["ROLLOUT"].parts[part].state, Phase::Deployed);
    }
    let state = store::load_unit(&c.root.join("01-first/alpha")).unwrap();
    assert_eq!(
        state.steps["ROLLOUT"].track.evidence.merged_at.as_deref(),
        Some(lifecycle_fixture::MERGED_AT),
    );
    assert_clean(&c.root);
    let units = cm::plan::load_units(&c.root).unwrap();
    let after = After::Merged(step.into());
    assert!(cm::plan::gate(&units, &after).met());
    assert_eq!(
        units["01-first/alpha"].steps["ROLLOUT"]
            .track
            .evidence
            .file
            .as_ref()
            .unwrap()
            .as_str(),
        store::relative(&c.root, &files[1])
    );

    c.receipt_stale(step, &files[0], Some("candidate advanced"));
    let units = cm::plan::load_units(&c.root).unwrap();
    let gate = cm::plan::gate(&units, &after);
    assert!(!gate.met());
    assert_eq!(gate.stale, [store::relative(&c.root, &files[0])]);
    let stale = lint(&c.root).unwrap();
    assert!(
        stale
            .violations
            .iter()
            .any(|v| v.path == step && v.rule == "evidence-stale")
    );

    c.receipt_stale(step, &files[0], None);
    assert_clean(&c.root);
    fs::remove_file(&files[0]).unwrap();
    let missing = lint(&c.root).unwrap();
    assert!(
        missing
            .violations
            .iter()
            .any(|v| v.path == format!("{step}#east") && v.rule == "evidence-file"),
        "{:?}",
        missing.violations
    );
}

#[allow(dead_code)]
#[path = "support/publication.rs"]
mod publication_fixture;

#[test]
fn release_sync_records_every_publication_in_one_move() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    let mut forge = p::present();
    forge.root = Some(c.root.clone());
    let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    let step = p::step(&c);
    assert_eq!(step.track.state, Phase::Released);
    let expected = vec![
        "crate:acme-sdk@1.4.0",
        "tap:o/homebrew-tap/widget@blob-sha",
        "image:ghcr.io/acme/widget@sha256:digest",
    ];
    assert_eq!(step.track.evidence.published, expected);
    assert_eq!(out.fields["moves"].as_array().unwrap().len(), 1);
    assert_eq!(out.fields["moves"][0]["evidence"]["tag"], p::TAG);
    assert_eq!(
        out.fields["moves"][0]["evidence"]["published"],
        serde_json::json!(expected)
    );
    assert_eq!(
        *forge.artifact_queries.borrow(),
        [
            "crate:acme-sdk",
            "tap:o/homebrew-tap/widget",
            "image:ghcr.io/acme/widget"
        ]
    );
}

#[test]
fn missing_yanked_or_stale_publications_stay_merged_and_are_retried() {
    use cm::forge::Read;
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    for fault in 0..6 {
        let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
        let mut forge = p::present();
        match fault {
            0..=2 => {
                forge.crates.insert(
                    ("acme-sdk".into(), "1.4.0".into()),
                    match fault {
                        0 => Read::Missing,
                        1 => Read::Yanked,
                        _ => Read::Stale("1.3.0".into()),
                    },
                );
            }
            3 => {
                forge.formulas.clear();
            }
            4 => {
                forge.formulas.insert(
                    ("o/homebrew-tap".into(), "widget".into()),
                    p::formula("widget-1.3.0"),
                );
            }
            _ => {
                forge.images.clear();
            }
        }
        let before = p::step(&c);
        let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
        assert_eq!(rules(&out), ["release-unpublished"]);
        assert_eq!(p::step(&c), before);
        assert_eq!(out.fields["moves"], serde_json::json!([]));
        let forge = p::present();
        let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
        assert!(out.ok(), "{:?}", out.violations);
        assert_eq!(p::step(&c).track.state, Phase::Released);
    }
}

#[test]
fn unread_publication_journals_unknown_without_partial_evidence() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    let before = p::unit(&c);
    let mut forge = p::present();
    forge.broken.insert("image:ghcr.io/acme/widget".into());
    let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert_eq!(rules(&out), ["sync-unknown"]);
    assert_eq!(p::unit(&c), before);
    assert!(out.fields.contains_key("journal"));
    assert_eq!(out.fields["moves"], serde_json::json!([]));
}

#[test]
fn tag_containment_is_checked_before_publication() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    let mut forge = p::present();
    forge
        .tags
        .insert(("o/widget".into(), p::TAG.into()), Default::default());
    let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert_eq!(rules(&out), ["release-tag"]);
    assert!(forge.artifact_queries.borrow().is_empty());
}

#[test]
fn undeclared_artifacts_warn_early_and_keep_the_existing_release_behavior() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Drafted, &[]);
    let out = lint(&c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert!(out.fields["warnings"].as_array().unwrap().contains(&serde_json::json!(format!("release-artifacts-undeclared: {} declares no artifacts; cm amend {} --artifact <spec>", p::STEP, p::STEP))));
    let c = p::campaign(&host, Phase::Merged, &[]);
    let forge = p::present();
    let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert_eq!(p::step(&c).track.state, Phase::Released);
    assert!(forge.artifact_queries.borrow().is_empty());
    assert!(p::step(&c).track.evidence.published.is_empty());
}

#[test]
fn artifact_flags_refuse_malformed_specs_duplicates_and_non_release_steps() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = campaign(&host);
    let before = c.files();
    for specs in [
        vec!["other:widget"],
        vec!["crate:"],
        vec!["crate:has.dot"],
        vec!["crate:has/slash"],
        vec!["tap:owner/formula"],
        vec!["tap:owner//widget"],
        vec!["image:docker.io/o/widget"],
        vec!["image:ghcr.io/o/widget:v1"],
        vec!["image:ghcr.io/o/widget@sha256:abc"],
        vec!["crate:widget", "crate:widget"],
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cm"));
        cmd.arg("-C")
            .arg(&c.root)
            .args(["--json", "init", p::STEP, "--lane", "codex"]);
        for spec in specs {
            cmd.args(["--artifact", spec]);
        }
        let output = cmd.output().unwrap();
        assert!(!output.status.success());
        let out: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(out["violations"][0]["rule"], "artifact");
        assert_eq!(c.files(), before);
    }
    let out = commands::declare(
        &host,
        &c.root.join("01-first/alpha/BUILD"),
        &commands::Declaration {
            lane: Some(Lane::Codex),
            artifacts: vec!["crate:widget".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rules(&out), ["artifact-step"]);
    assert_eq!(c.files(), before);
}

#[test]
fn artifacts_persist_in_order_and_can_only_be_removed_by_amend_before_released() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    let stored: serde_json::Value =
        store::read_json(&store::state_path(&c.root.join("00-release/widget"))).unwrap();
    assert_eq!(stored["schema"], cm::state::SCHEMA);
    assert_eq!(
        stored["steps"]["1.4.0"]["artifacts"],
        serde_json::json!(p::ARTIFACTS)
    );
    let amendment = commands::Amendment {
        drop_artifacts: vec!["tap:widget".into()],
        artifacts: vec!["tap:acme/custom-tap/widget".into()],
        ..Default::default()
    };
    let out = commands::amend(&host, &c.root, &target(p::STEP), &amendment).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert_eq!(
        out.fields["changes"],
        serde_json::json!([
            "- artifact: - tap:widget",
            "- artifact: + tap:acme/custom-tap/widget"
        ])
    );
    assert!(out.fields.contains_key("journal"));
    let mut forge = p::present();
    forge.formulas.insert(
        ("acme/custom-tap".into(), "widget".into()),
        p::formula(p::TAG),
    );
    assert!(cm::sync::sync(&host, &forge, &c.root).unwrap().ok());
    let before = c.files();
    let refused = commands::amend(
        &host,
        &c.root,
        &target(p::STEP),
        &commands::Amendment {
            drop_artifacts: vec!["crate:acme-sdk".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rules(&refused), ["amend-state"]);
    assert_eq!(c.files(), before);
}

#[test]
fn an_unpublished_release_ships_no_other_merged_steps() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    c.publication_consumer(&host);
    let mut forge = p::present();
    forge.images.clear();
    let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert_eq!(rules(&out), ["release-unpublished"]);
    let code = || {
        store::load_unit(&c.root.join("01-build/widget"))
            .unwrap()
            .steps
            .remove("BUILD")
            .unwrap()
            .track
            .state
    };
    assert_eq!(code(), Phase::Merged);
    assert_eq!(out.fields["moves"], serde_json::json!([]));
    let out = cm::sync::sync(&host, &p::present(), &c.root).unwrap();
    assert!(out.ok(), "{:?}", out.violations);
    assert_eq!(code(), Phase::Released);
}

#[test]
fn publication_cache_never_satisfies_another_version_at_the_same_tag() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let (c, forge) = p::colliding_versions(&host);
    let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
    assert_eq!(rules(&out), ["release-unpublished"]);
    assert_eq!(out.violations[0].path, "00-release/widget/4.0.0");
    let unit = p::unit(&c);
    assert_eq!(unit.steps["1.4.0"].track.state, Phase::Released);
    assert_eq!(
        unit.steps["1.4.0"].track.evidence.published,
        ["crate:acme-sdk@1.4.0"]
    );
    assert_eq!(unit.steps["4.0.0"].track.state, Phase::Merged);
    assert!(unit.steps["4.0.0"].track.evidence.published.is_empty());
    assert_eq!(forge.artifact_queries.borrow().len(), 2);
}

#[test]
fn unread_pr_or_tag_without_artifacts_does_not_journal() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    for phase in [Phase::PrOpen, Phase::Merged] {
        let c = p::campaign(&host, phase.clone(), &[]);
        let mut forge = p::present();
        forge.broken.insert(
            if phase == Phase::PrOpen {
                "https://github.com/o/widget/pull/7"
            } else {
                p::TAG
            }
            .into(),
        );
        let before = fs::read(c.root.join("JOURNAL.md")).unwrap();
        let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
        assert_eq!(rules(&out), ["sync-unknown"]);
        assert!(!out.fields.contains_key("journal"));
        assert_eq!(fs::read(c.root.join("JOURNAL.md")).unwrap(), before);
        assert_eq!(p::step(&c).track.state, phase);
    }
}

#[test]
fn image_publication_passes_the_pull_token_on_stdin() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &["image:ghcr.io/acme/widget"]);
    let processes = lifecycle_fixture::image_read_processes();
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("-C")
        .arg(&c.root)
        .args(["--json", "advance", p::STEP, "released", "--tag", p::TAG])
        .env("PATH", processes.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let args = fs::read_to_string(processes.fixture().join("manifest-args")).unwrap();
    let args: Vec<_> = args.split_terminator('\0').collect();
    assert!(args.windows(2).any(|pair| pair == ["--header", "@-"]));
    assert!(args.iter().all(|arg| !arg.contains("synthetic-pull-token")));
    assert_eq!(
        fs::read_to_string(processes.fixture().join("manifest-headers")).unwrap(),
        "Authorization: Bearer synthetic-pull-token\n"
    );
    assert_eq!(
        p::step(&c).track.evidence.published,
        ["image:ghcr.io/acme/widget@sha256:synthetic-digest"]
    );
}

#[path = "support/formula.rs"]
mod formula_fixture;

#[test]
fn unsupported_formula_arguments_journal_unknown_and_leave_the_release_merged() {
    use publication_fixture as p;
    let host = Fake::at("2026-10-08T07:00:00Z");
    let c = p::campaign(&host, Phase::Merged, &p::ARTIFACTS);
    let before = p::unit(&c);
    for (key, value) in [
        (
            "url",
            "https://github.com/o/widget/releases/download/widget-1.4.0/widget.tar.gz",
        ),
        ("version", "1.4.0"),
    ] {
        for (name, argument) in formula_fixture::unsupported_arguments(value) {
            let call = format!("{key}({argument})");
            let declaration = if key == "url" {
                format!("{call}\nsha256 \"{}\"\n", "b".repeat(64))
            } else {
                call
            };
            let body = p::formula(p::TAG).replace("\nend\n", &format!("\n{declaration}\nend\n"));
            let mut forge = p::present();
            forge
                .formulas
                .insert(("o/homebrew-tap".into(), "widget".into()), body);
            let out = cm::sync::sync(&host, &forge, &c.root).unwrap();
            assert_eq!(rules(&out), ["sync-unknown"], "{key}: {name}");
            assert!(out.fields.contains_key("journal"));
            assert_eq!(out.fields["moves"], serde_json::json!([]));
            assert_eq!(p::unit(&c), before);
        }
    }
}
