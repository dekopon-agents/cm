//! Done-check stubs for `tick/ledger` (SKELETON.md, Done-check stubs). Each is filled by that unit;
//! none may be removed or renamed without a skeleton change.
//!
//! They run real `git` against a scratch bare origin (`tests/fixtures/tick-ledger/scratch.rs`).
//! `cm tick` itself is `tick/core`'s, so a tick here is the ledger and checkout calls its stages
//! make, in their order; `cm effects resolve` is the built binary.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use cm::host::Host;
use cm::tick::ledger::{
    Bind, Draft, EffectLedger, Family, Intended, IntentKind, IntentRow, IntentState, LedgerRow,
    Observation, Observer, TickLock, reconcile,
};
use cm::tick::push::{self, Git, PendingMark, SystemGit, TickPhase};
use cm::tick::{Capability, IntentId};
use jiff::Timestamp;

#[path = "fixtures/tick-ledger/scratch.rs"]
mod scratch;
use scratch::{EPOCH, Scratch};

struct Clock(Timestamp);

impl Host for Clock {
    fn now(&self) -> Timestamp {
        self.0
    }

    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(100 << 30)
    }

    fn heavy_builds(&self) -> io::Result<u32> {
        Ok(0)
    }
}

/// The tick lock, retried briefly: in a multi-threaded test binary a sibling test's forked
/// git child can hold a just-closed lock descriptor until it execs.
fn tick_lock(root: &Path) -> TickLock {
    for _ in 0..200 {
        if let Some(lock) = TickLock::try_acquire(root).unwrap() {
            return lock;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the tick lock stayed held");
}

fn clock() -> Clock {
    Clock("2026-11-22T18:00:00Z".parse().unwrap())
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn draft(target: &str, kind: IntentKind, capability: Option<Capability>) -> Draft {
    Draft {
        target: target.to_owned(),
        kind,
        capability,
        input: None,
        repo: Some("dekopon-agents/cm".to_owned()),
        pr: Some(42),
        head: Some(HEAD.to_owned()),
        nonce: None,
        model: None,
        effort: None,
        family: None,
        route: None,
        attempt: None,
        requested_by: None,
        follows: None,
    }
}

fn new_id(intended: Intended) -> IntentId {
    match intended {
        Intended::New(id) => id,
        Intended::AlreadyOpen(id) => panic!("{id} was already open"),
    }
}

fn segment_on_main(scratch: &Scratch) -> String {
    scratch
        .on_main(&format!("{EPOCH}/.cm/tick/effects-0001.jsonl"))
        .unwrap_or_default()
}

/// The ledger's active segment, relative to the checkout: what the tick force-adds.
fn segment(ledger: &EffectLedger) -> PathBuf {
    Path::new(EPOCH).join(ledger.active_segment())
}

fn rows_on_main(scratch: &Scratch) -> Vec<LedgerRow> {
    segment_on_main(scratch)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// `cm --json -C <campaign> effects resolve ...`, the built binary.
/// Retried while the tick lock reads held: a sibling test's forked git child can keep this
/// process's just-closed lock descriptor until it execs.
fn resolve(campaign: &Path, args: &[&str]) -> serde_json::Value {
    for _ in 0..200 {
        let json = resolve_once(campaign, args);
        if json["violations"][0]["rule"] != "tick-locked" {
            return json;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the tick lock stayed held");
}

fn resolve_once(campaign: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_cm"))
        .arg("--json")
        .arg("-C")
        .arg(campaign)
        .args(["effects", "resolve"])
        .args(args)
        .output()
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"));
    assert_eq!(output.status.success(), json["ok"] == true, "{output:?}");
    json
}

/// The positive evidence a launch leaves (a session id in the launcher's records); absence of
/// any other effect's evidence reads as not done, which the ledger's conservative rule parks.
struct Evidence<'a> {
    launches: &'a Path,
}

impl Observer for Evidence<'_> {
    fn observe(&self, row: &IntentRow) -> Observation {
        let launched = fs::read_to_string(self.launches).unwrap_or_default();
        match launched.lines().find(|line| line.starts_with(&row.id.0)) {
            Some(line) => Observation::Done(Bind::Session {
                id: line.split(' ').nth(1).unwrap_or_default().to_owned(),
            }),
            None => Observation::NotDone,
        }
    }
}

#[test]
fn cm_writes_and_pushes_an_intent_row_with_pr_head_nonce_model_and_family_before_its_effect() {
    let scratch = Scratch::new();
    let checkout = scratch.clone("tick");
    let campaign = scratch::campaign(&checkout);
    let git = SystemGit::default();
    let host = clock();
    let lock = tick_lock(&campaign);
    let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
    let mut review = draft("01-a/u/s", IntentKind::ReviewLaunch, None);
    review.model = Some("gpt-5.5-codex".to_owned());
    review.effort = Some("high".to_owned());
    review.family = Some(
        Family::writer_of(cm::state::Lane::ClaudeCloud)
            .unwrap()
            .other(),
    );
    let id = new_id(ledger.intend(&host, 7, review).unwrap());
    ledger.emitted(&host, &id, 7).unwrap();
    let nonce = ledger.intent(&id).unwrap().nonce.clone().unwrap();
    assert_eq!(nonce, ledger.mint_nonce(&id));

    let launched = Cell::new(false);
    let effect = || {
        // The effect may run only once its intent row is on main.
        let on_main = rows_on_main(&scratch);
        assert!(
            on_main
                .iter()
                .any(|row| matches!(row, LedgerRow::Intent(row) if row.id == id)),
            "the effect ran before its intent row was pushed"
        );
        launched.set(true);
    };
    assert!(rows_on_main(&scratch).is_empty());
    let pushed =
        push::commit_and_push(&git, &checkout, &[segment(&ledger)], &[], "tick 7").unwrap();
    assert_eq!(pushed.tries, 1);
    effect();
    assert!(launched.get());

    let row = rows_on_main(&scratch)
        .into_iter()
        .find_map(|row| match row {
            LedgerRow::Intent(row) if row.id == id => Some(row),
            _ => None,
        })
        .unwrap();
    assert_eq!(row.pr, Some(42));
    assert_eq!(row.head.as_deref(), Some(HEAD));
    assert_eq!(row.nonce.as_ref(), Some(&nonce));
    assert_eq!(nonce.0.len(), 32);
    assert_eq!(row.model.as_deref(), Some("gpt-5.5-codex"));
    assert_eq!(
        row.family,
        Some(Family::Gpt),
        "the reviewer is the writer's other family"
    );
    assert_eq!(row.cycle, 7, "the tick number it was written under");
    assert_eq!(row.kind, IntentKind::ReviewLaunch);
    let line = segment_on_main(&scratch);
    let first: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    assert_eq!(first["row"], "intent");
    assert_eq!(first["nonce"], nonce.0.as_str());
}

/// The killed tick of the next test, run as a child process: it pushes its first commit (intent
/// and `emitted` rows), turns the mark to `record`, launches the step, appends the bind row
/// locally, and waits to be killed before its second commit and before the workflow run.
fn killed_tick(checkout: &Path) -> ! {
    let campaign = scratch::campaign(checkout);
    let git = SystemGit::default();
    let host = clock();
    let lock = tick_lock(&campaign);
    push::begin(&git, checkout, &campaign).unwrap();
    let base = git.head(checkout).unwrap();
    let mut mark = PendingMark {
        tick: 1,
        base,
        phase: TickPhase::Decide,
        paths: Vec::new(),
    };
    mark.save(checkout).unwrap();
    let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
    let mut step = draft("01-a/u/s", IntentKind::StepLaunch, None);
    step.attempt = Some("s#1".to_owned());
    let step = new_id(ledger.intend(&host, 1, step).unwrap());
    let run = new_id(
        ledger
            .intend(
                &host,
                1,
                draft(
                    "01-a/u/r",
                    IntentKind::Effect,
                    Some(Capability::GhWorkflowRun),
                ),
            )
            .unwrap(),
    );
    for id in [&step, &run] {
        ledger.emitted(&host, id, 1).unwrap();
    }
    push::commit_and_push(&git, checkout, &[segment(&ledger)], &[], "tick 1").unwrap();
    mark.phase = TickPhase::Record;
    mark.save(checkout).unwrap();
    // Stage 10: the launch happens and leaves its session.
    scratch::append(
        &checkout.parent().unwrap().join("launches"),
        &format!("{step} session_child\n"),
    );
    ledger
        .bind(
            &host,
            &step,
            Bind::Session {
                id: "session_child".to_owned(),
            },
        )
        .unwrap();
    fs::write(checkout.parent().unwrap().join("ready"), "").unwrap();
    std::thread::sleep(Duration::from_secs(120));
    unreachable!("the parent kills this tick");
}

#[test]
fn a_restart_between_intent_and_effect_reconciles_or_parks_with_no_second_launch() {
    if let Ok(checkout) = std::env::var("CM_TICK_LEDGER_KILLED_TICK") {
        killed_tick(Path::new(&checkout));
    }
    let scratch = Scratch::new();
    let tick_clone = scratch.clone("tick");
    let second = scratch.clone("second");
    let launches = scratch.dir.path().join("launches");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_restart_between_intent_and_effect_reconciles_or_parks_with_no_second_launch",
            "--nocapture",
        ])
        .env("CM_TICK_LEDGER_KILLED_TICK", &tick_clone)
        .spawn()
        .unwrap();
    let ready = scratch.dir.path().join("ready");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "the child tick never reached its launch"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "the child tick exited early"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap(); // SIGKILL
    assert!(!child.wait().unwrap().success());

    // A second tick from a second clone reconciles from main.
    let git = SystemGit::default();
    let host = clock();
    let campaign = scratch::campaign(&second);
    let lock = tick_lock(&campaign);
    push::begin(&git, &second, &campaign).unwrap();
    let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
    let ids: Vec<IntentId> = ledger
        .open_intents()
        .iter()
        .map(|row| row.id.clone())
        .collect();
    assert_eq!(ids.len(), 2, "both intents reached main before any effect");
    let seen = reconcile(
        &host,
        &mut ledger,
        &Evidence {
            launches: &launches,
        },
    )
    .unwrap();
    assert_eq!(
        seen.bound,
        [ids[0].clone()],
        "the launch that happened binds"
    );
    assert_eq!(seen.parked, [ids[1].clone()], "the unreceipted run parks");
    assert!(seen.retry.is_empty(), "nothing is launched again");
    // The next planning pass finds the parked run already intended and writes no second intent.
    let again = ledger
        .intend(
            &host,
            2,
            draft(
                "01-a/u/r",
                IntentKind::Effect,
                Some(Capability::GhWorkflowRun),
            ),
        )
        .unwrap();
    assert_eq!(again, Intended::AlreadyOpen(ids[1].clone()));
    push::commit_and_push(&git, &second, &[segment(&ledger)], &[], "tick 2").unwrap();
    drop(lock);

    // The killed tick's clone recovers its recorded bind by the mark, and union keeps both
    // clones' rows.
    let tick_campaign = scratch::campaign(&tick_clone);
    let appended: Vec<LedgerRow> = EffectLedger::snapshot(&tick_campaign)
        .unwrap()
        .rows()
        .iter()
        .chain(ledger.rows())
        .cloned()
        .collect();
    let lock = tick_lock(&tick_campaign);
    push::begin(&git, &tick_clone, &tick_campaign).unwrap();
    assert_eq!(PendingMark::load(&tick_clone).unwrap(), None);
    let merged = EffectLedger::open(&tick_campaign, &lock).unwrap();
    let on_main = rows_on_main(&scratch);
    assert_eq!(merged.rows(), on_main.as_slice());
    for row in &appended {
        assert!(on_main.contains(row), "{row:?} lost in the merge");
    }
    assert!(
        on_main
            .iter()
            .any(|row| matches!(row, LedgerRow::Bound { intent, .. } if *intent == ids[0]))
    );
    assert!(
        on_main
            .iter()
            .any(|row| matches!(row, LedgerRow::Parked { intent, .. } if *intent == ids[1]))
    );
    assert_eq!(merged.state(&ids[0]), Some(IntentState::Bound));
    assert_eq!(merged.state(&ids[1]), Some(IntentState::Parked));
    assert_eq!(
        fs::read_to_string(&launches).unwrap().lines().count(),
        1,
        "one launch in all"
    );
}

#[test]
fn effects_resolve_closes_a_parked_intent_and_retry_reemits_it_under_the_same_id() {
    let scratch = Scratch::new();
    let tick_clone = scratch.clone("tick");
    let agent_clone = scratch.clone("agent");
    let git = SystemGit::default();
    let host = clock();
    let campaign = scratch::campaign(&tick_clone);
    let target = "01-a/u/p";
    let pin = || draft(target, IntentKind::Effect, Some(Capability::GhPinUpdate));
    let (parked, dropped) = {
        let lock = tick_lock(&campaign);
        let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
        let parked = new_id(ledger.intend(&host, 1, pin()).unwrap());
        let other = draft(
            "01-a/u/q",
            IntentKind::Effect,
            Some(Capability::GhPinUpdate),
        );
        let dropped = new_id(ledger.intend(&host, 1, other).unwrap());
        for id in [&parked, &dropped] {
            ledger.emitted(&host, id, 1).unwrap();
            ledger.park(&host, id, "undecidable").unwrap();
        }
        push::commit_and_push(&git, &tick_clone, &[segment(&ledger)], &[], "tick 1").unwrap();
        (parked, dropped)
    };

    // The coordinator agent rules from its own clone with the built cm.
    let agent = scratch::campaign(&agent_clone);
    push::begin(&git, &agent_clone, &agent).unwrap();
    let json = resolve(&agent, &[&parked.0, "--as", "retry"]);
    assert_eq!(json["ok"], true, "{json}");
    let json = resolve(
        &agent,
        &[&dropped.0, "--as", "drop", "--reason", "owner said no"],
    );
    assert_eq!(json["ok"], true, "{json}");
    let refused = resolve(&agent, &[&dropped.0, "--as", "retry"]);
    assert_eq!(
        refused["ok"], false,
        "a closed intent is not parked: {refused}"
    );

    // The tick pulls the ruling; the owner re-emits the retry under the same id.
    let lock = tick_lock(&campaign);
    push::begin(&git, &tick_clone, &campaign).unwrap();
    let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
    assert_eq!(ledger.state(&parked), Some(IntentState::Open));
    assert_eq!(ledger.state(&dropped), Some(IntentState::Closed));
    assert_eq!(
        ledger.intend(&host, 2, pin()).unwrap(),
        Intended::AlreadyOpen(parked.clone()),
        "a retry never becomes a new intent"
    );
    ledger.emitted(&host, &parked, 2).unwrap();
    assert_eq!(ledger.last_emitted(&parked), Some(2));
    ledger.park(&host, &parked, "still undecidable").unwrap();
    push::commit_and_push(&git, &tick_clone, &[segment(&ledger)], &[], "tick 2").unwrap();
    drop(lock);

    // Then a done ruling with campaign evidence closes it.
    push::begin(&git, &agent_clone, &agent).unwrap();
    fs::write(agent.join("pin-evidence.md"), "merged by hand\n").unwrap();
    let json = resolve(
        &agent,
        &[&parked.0, "--as", "done", "--evidence", "pin-evidence.md"],
    );
    assert_eq!(json["ok"], true, "{json}");
    let ledger = EffectLedger::snapshot(&agent).unwrap();
    assert_eq!(ledger.state(&parked), Some(IntentState::Closed));
    let intents = ledger
        .rows()
        .iter()
        .filter(|row| matches!(row, LedgerRow::Intent(row) if row.target == target))
        .count();
    assert_eq!(intents, 1, "one id from first emission to close");
    assert_eq!(rows_on_main(&scratch), ledger.rows());
}

#[test]
fn a_killed_tick_and_a_second_clone_recover_by_the_dirty_tree_and_union_rules() {
    let scratch = Scratch::new();
    let tick_clone = scratch.clone("tick");
    let agent_clone = scratch.clone("agent");
    let git = SystemGit::default();
    let host = clock();
    let campaign = scratch::campaign(&tick_clone);
    let agent = scratch::campaign(&agent_clone);
    let base = git.head(&tick_clone).unwrap();

    // Tick 1 is killed while deciding: its intent rows, cursor and tally row never reach main.
    {
        let lock = tick_lock(&campaign);
        PendingMark {
            tick: 1,
            base: base.clone(),
            phase: TickPhase::Decide,
            paths: vec![Path::new(EPOCH).join("RELEASE.md")],
        }
        .save(&tick_clone)
        .unwrap();
        let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
        new_id(
            ledger
                .intend(&host, 1, draft("01-a/u/s", IntentKind::StepLaunch, None))
                .unwrap(),
        );
        fs::write(campaign.join("RELEASE.md"), "| reserved |\n").unwrap();
        fs::write(campaign.join(".cm/tick/cursor.json"), "{\"tick\":1}\n").unwrap();
    }
    // Meanwhile the coordinator agent's clone appends its own rows to main.
    let ruled_elsewhere = {
        let lock = tick_lock(&agent);
        let mut ledger = EffectLedger::open(&agent, &lock).unwrap();
        let id = new_id(
            ledger
                .intend(
                    &host,
                    0,
                    draft("01-a/u/z", IntentKind::Effect, Some(Capability::GhPinMerge)),
                )
                .unwrap(),
        );
        ledger.park(&host, &id, "agent").unwrap();
        push::commit_and_push(&git, &agent_clone, &[segment(&ledger)], &[], "agent").unwrap();
        id
    };

    // Tick 2's begin discards tick 1 whole, then pulls the agent's rows.
    let lock = tick_lock(&campaign);
    push::begin(&git, &tick_clone, &campaign).unwrap();
    assert!(!campaign.join("RELEASE.md").exists());
    assert!(!campaign.join(".cm/tick/cursor.json").exists());
    let mut ledger = EffectLedger::open(&campaign, &lock).unwrap();
    assert_eq!(
        ledger.rows().len(),
        2,
        "only the agent's rows: {:?}",
        ledger.rows()
    );
    assert_eq!(ledger.state(&ruled_elsewhere), Some(IntentState::Parked));

    // Tick 2 pushes its first commit, records a bind and a cm state write, and is killed.
    let mark = PendingMark {
        tick: 2,
        base: git.head(&tick_clone).unwrap(),
        phase: TickPhase::Decide,
        paths: Vec::new(),
    };
    mark.save(&tick_clone).unwrap();
    let step = new_id(
        ledger
            .intend(&host, 2, draft("01-a/u/s", IntentKind::StepLaunch, None))
            .unwrap(),
    );
    ledger.emitted(&host, &step, 2).unwrap();
    push::commit_and_push(&git, &tick_clone, &[segment(&ledger)], &[], "tick 2").unwrap();
    PendingMark {
        phase: TickPhase::Record,
        ..mark
    }
    .save(&tick_clone)
    .unwrap();
    ledger
        .bind(
            &host,
            &step,
            Bind::Session {
                id: "session_2".to_owned(),
            },
        )
        .unwrap();
    fs::write(campaign.join("STATE.md"), "# State\n\nlaunched\n").unwrap();
    fs::write(campaign.join("scratch-notes.md"), "not cm's\n").unwrap();
    drop(ledger);
    drop(lock);

    // The agent rules on its parked intent without pulling tick 2: its push merges by union.
    let json = resolve(
        &agent,
        &[&ruled_elsewhere.0, "--as", "drop", "--reason", "stale"],
    );
    assert_eq!(json["ok"], true, "{json}");

    // Tick 3's begin commits tick 2's recorded facts and the cm write, and union merges.
    let lock = tick_lock(&campaign);
    push::begin(&git, &tick_clone, &campaign).unwrap();
    let ledger = EffectLedger::open(&campaign, &lock).unwrap();
    let on_main = rows_on_main(&scratch);
    assert_eq!(ledger.rows().len(), on_main.len());
    for row in ledger.rows() {
        assert!(on_main.contains(row), "{row:?} missing on main");
    }
    assert_eq!(ledger.state(&step), Some(IntentState::Bound));
    assert_eq!(ledger.state(&ruled_elsewhere), Some(IntentState::Closed));
    assert!(
        scratch
            .on_main(&format!("{EPOCH}/STATE.md"))
            .unwrap()
            .contains("launched")
    );
    assert_eq!(scratch.on_main(&format!("{EPOCH}/scratch-notes.md")), None);
    assert!(
        git.dirty(&tick_clone, Path::new("."))
            .unwrap()
            .contains(&Path::new(EPOCH).join("scratch-notes.md"))
    );
}
