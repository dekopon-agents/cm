# Area skeleton: tick

This skeleton fixes the structure of `cm tick` before any tick code unit gets a kickoff. Workers fill it in and never invent the structure. Template: `skills/campaign-trail/templates/skeleton.md`. This is the xhigh revision. It re-derives the medium draft (the previous version of this file and PR #114's first head, `7106876`) from PLAN.md, 11-rpi DECISIONS, EXECUTION.md and the cm code, and replaces it. §Changes from the medium draft lists what moved and why.

**Header**
- Area: `tick`, meaning `cm tick` in the cm repo: intake, timers, dispatch, the decision packet, the effect ledger, the verdict gate, releases and the LAN leg.
- Repository and base: `dekopon-agents/cm` at `0e38f02` (main, "chore: update rustix lock for RUSTSEC-2026-0486 (#112)"). The draft was drawn from `c740539`. This revision merges main into the branch (`719fe6c`, no force-push) and commits on top.
- Skeleton PR: `dekopon-agents/cm#114`, head `cd7865a8c538ee80b545eb9cb0c0324cf7c10a1e`, branch `design/skeleton-tick`.
- Units covered: `tick/ledger`, `tick/core`, `tick/agent-ledger`, `tick/gate-merge`, `tick/release-lan`.
  - **Consumer whose interface this skeleton must not break:** `rpi/tick-runner` (dekopon). It reaches cm only through 11-D3's two command lines and their JSON, frozen in `src/tick/wire.rs` (S-tick-10 to S-tick-13, S-tick-21). The effect inputs are checked against the provider's own schemas, provider-gh `5b1616e`; see C4.
  - **Kept:** all five starting units. Each is a cm code unit in UNITS.json (`repo: cm`), and each done check maps to the done-check stubs below.
  - **Dropped:** none.
  - **Added:** none. The cm side of 11-D3 has no unit of its own, so it goes in `tick/ledger` (C2): `--effects`, `cm effects record`, `cm effects resolve` and the interim local applier.
  - **Left out, with reasons:**
    - `rpi/cm-in-vm` (`repo: vm-runner (guest image), cm`). It changes the guest image and adds no cm source. 11-D7's clone on session create has no cm command in this skeleton (G5).
    - `05-integration/advisory-locks` touches cm only through `Cargo.lock`.
    - `tick/review-shadow` and `tick/scratch-ci` are code in campaign-trail and gh-parity-scratch, not cm.
    - `tick/rehearsal`, `interim-cutover`, `overnight-trial`, `rpi-cutover` and `gpt62-build-study` are studies, configs or cutovers. They write no cm code.
- Steering read: campaign-trail main `5b0ae28d`, re-checked at `63035bd0` (main at the end of this step). The only change in a file this design reads is an unrelated `execution.md` line on deadlocked PRs. The brief's steering SHA `630f04b1` is not fetchable ("not our ref"). Also read: provider-gh `5b1616e` (`reviews.rs` `MergeInput`, `workflows.rs` `Input`).
- Effort receipt: `printenv CLAUDE_CODE_EFFORT_LEVEL` printed `xhigh`.

## Gate

The skeleton PR passes these, run at head `cd7865a8c538ee80b545eb9cb0c0324cf7c10a1e` with Rust 1.98.1:
- `cargo fmt --all --check`
- `cargo check --all-targets --locked`
- `cargo clippy --all-targets --locked -- -D warnings`
- `cargo test --locked`: 429 passed, 0 failed, 102 ignored. The 102 are the skeleton stubs: 72 lifecycle and 30 done-check.

cm has no AGENTS.md, no `clippy.toml` and no `disallowed-methods` concurrency lints. Its lints are clippy's defaults under `-D warnings`, as CI runs them (`.github/workflows/ci.yml`, job `fmt, clippy, test`). Nothing else was there to run.

Every body a unit writes is `todo!("<unit>")`. Each skeleton file opens with `#![allow(unused_variables, reason = "skeleton: <unit> fills these bodies")]`, which the unit deletes once it fills the file. `EffectLedger`'s private fields carry `#[expect(dead_code)]`, which becomes an error the moment `tick/ledger` reads them. Each lifecycle and done-check stub is `#[ignore = "skeleton: <unit>"]`.

The skeleton writes bodies only for glue and seams, and no unit edits these:
- `cli.rs`:
  - the `tick`, `effects record`, `effects resolve` and `export` arms;
  - the flag-gated agent-ledger calls on `launch`, `attempt record`, `attempt end` and `review record`;
  - `full_sha`;
  - the `--json` guard on `tick --effects`.
- `SystemGit::default`.
- `SystemObserver::observe` (S-tick-17) and `LocalApply::apply` (S-tick-24): routing matches that send each intent kind or capability to the module of the unit that owns it.
- `forge.rs`: `run`, `Gh::json` and `Gh::pages` become `pub(crate)`.
- `verdict.rs`: `Block::nonce`, always `None` until `tick/gate-merge` parses it.

Existing commands behave exactly as before. The new agent-ledger calls run only when one of their new flags is given. The full existing suite passes.

## Module tree

- `Cargo.toml`: `thiserror = "2"`, which was already in `Cargo.lock` as a transitive dependency.
- `src/lib.rs`: `pub mod tick;`.
- `src/cli.rs` (S-tick-02), registration only:
  - `Commands::Tick { --approved <40-hex>, --effects }`;
  - `Commands::Effects { Record, Resolve { <intent>, --as done|retry|drop, --evidence, --reason } }`;
  - `Commands::Export`;
  - `Requested` flattened into `Launch`, and `Echo` flattened into `AttemptAct::Record`, `AttemptAct::End` and `ReviewAct::Record`.
- `src/forge.rs`, `src/verdict.rs`: the S-tick-04 seams.
- `src/tick/mod.rs` (S-tick-03): registration, the `pub use` list, `tick::Result` and `TICK_DIR = ".cm/tick"`.
- `src/tick/error.rs`: `TickError`, the area's one error enum (S-tick-01).
- `src/tick/wire.rs`: the 11-D3 wire contract, its typed provider inputs and its byte bounds (`tick/ledger`).
- `src/tick/ledger.rs`: the effect ledger `.cm/tick/effects.jsonl`, nonces, reconcile, the observer routing and the tick lock (`tick/ledger`).
- `src/tick/push.rs`: `Git`, `SystemGit`, the pull each tick starts with, and the commit-and-push with three tries (`tick/ledger`).
- `src/tick/record.rs`: `cm effects record` and `cm effects resolve` (`tick/ledger`).
- `src/tick/apply.rs`: `Applier`, `Apply`, the local applier for the cloud interim, and the in-order stop rule (`tick/ledger`).
- `src/tick/authority.rs`: `[authority]`, `[gates]` and `[review]` read at the approved SHA, `paused` read live, and the §Gates check rule (`tick/core`).
- `src/tick/timers.rs`: durable timers: retry, collect, review deadline and reminder (`tick/core`).
- `src/tick/intake.rs`: bounded, round-robin polling of the PRs cm tracks, and the cursor (`tick/core`).
- `src/tick/dispatch.rs`: launch routes, the launch plan before the push, the launches after it, and the launch lookups (`tick/core`).
- `src/tick/packet.rs`: the bounded decision packet, its rendered text and the seen set (`tick/core`).
- `src/tick/run.rs`: the stage order, `Deps`, `Cycle`, `tick` and the CLI seam `cli_tick` (`tick/core`). Shared with `tick/gate-merge`, which adds one stage line, and `tick/release-lan`, which adds two.
- `src/tick/gate.rs`: the verdict gate (`tick/gate-merge`).
- `src/tick/merge.rs`: the exact-head merge, the post-merge confirmation and the pin hold (`tick/gate-merge`).
- `src/tick/release.rs`: release cuts and the drop tally (`tick/release-lan`).
- `src/tick/lan.rs`: pins, readiness, the LAN leg and the probe (`tick/release-lan`).
- `src/tick/agents.rs`: `AGENTS.jsonl` rows in LEDGER.md's schema (`tick/agent-ledger`).
- `src/tick/export.rs`: the `cm export --json` graph (`tick/agent-ledger`).
- `tests/tick_{ledger,core,gate_merge,release_lan,agent_ledger}.rs`: one done-check stub file per unit. Each unit also owns `tests/fixtures/tick-<unit>/`.
- Existing files a unit changes, all of them `tick/gate-merge`'s:
  - `src/verdict.rs`: parse `nonce:`; `merge_gaps` takes the phase's `ProofRule`; the third-FIX-REQUIRED ruling text;
  - `src/commands.rs`: `cm advance`'s gate;
  - `src/sync.rs`: `gate_holds`;
  - the existing gate tests that change with it.

## Items

There are no `async` items. cm is a synchronous, one-shot process with no runtime, and none is added: no value crosses a thread except `forge::run`'s two pipe threads per child. Every outward call is a child process with a timeout. The `task` rows say who owns each one, its bound, its full-queue policy and its shutdown path.

| id | item | kind | file | notes |
|---|---|---|---|---|
| S-tick-01 | `pub enum TickError`: `Io`, `Row`, `Child`, `Cm`, `ApprovedUnreachable`, `AuthorityMissing`, `PullConflict`, `PushExhausted`, `BatchTooLarge`, `BatchInvalid`, `UnknownIntent`, `ReceiptConflict`, `LedgerFull`, `TimersFull` | error | `src/tick/error.rs` | The one `thiserror` enum. Skeleton-owned: a new variant is a skeleton change. Error policy: an error is only a failure to write durable state. Anything about one PR, step or effect is data (a refusal, a hold, an `unread` read), and the tick goes on. There is no blanket `From<ForgeError>`, so a forge read cannot abort a tick through `?`. Crosses into `anyhow` only at `cli.rs`. |
| S-tick-02 | CLI registration and glue: the four new arms, `Requested`/`Echo` flattening, `full_sha`, the `--json` guard on `tick --effects` | fn | `src/cli.rs` | Skeleton-owned. Units never edit `cli.rs`. |
| S-tick-03 | `pub mod` list, `pub use` list, `tick::Result`, `TICK_DIR` | type | `src/tick/mod.rs` | Skeleton-owned. Durable tick files: `effects.jsonl`, `cursor.json`, `timers.json`, `seen.json` and `lock` under `.cm/tick/`. |
| S-tick-04 | seams: `forge::run`, `Gh::json` and `Gh::pages` made `pub(crate)`; `verdict::Block::nonce: Option<String>` | fn | `src/forge.rs`, `src/verdict.rs` | Skeleton-owned. Every tick child runs through `forge::run`, and every tick read through `Gh::json`. No second child runner. |
| S-tick-05 | concurrency and locks | task | `src/tick/ledger.rs` (`TickLock`) | **Owner:** each `cm` process. `TickLock` (`.cm/tick/lock`, `flock` `LOCK_EX`\|`LOCK_NB`) serializes every writer of `.cm/tick/`: `cm tick`, `cm effects record` and `cm effects resolve`. **Bound:** one holder. **Full queue:** a second holder is refused at once, never queued. A second tick exits 0 with no effects. **Shutdown:** the lock dies with the process. The tick never holds `store::lock` across an existing cm call or a child process. Existing commands take `store::lock` themselves, and `astra-review.sh collect` runs `cm review record`, so holding it would deadlock. |
| S-tick-06 | full-queue policy and growth bounds | task | the modules below | Per tick: 16 effects within 64 KiB of stdout (`wire::admit`, before any intent row), 8 launch intents, 120 reads and 200 events, and a packet of 20 items or 16 KiB. Durable: 64 open intents, 256 timers, 1,024 seen keys, a cursor bounded by the open PRs, at most five ledger rows per intent (the ledger closes with its epoch), five tries then park. Past a per-tick bound, work waits for the next tick: counted, no packet item. Past a durable bound, the planning stage parks and puts one `ledger-full` item. |
| S-tick-10 | `EffectsOut`, `PacketOut`, `PacketLine`, `EffectsOut::to_line`; `RECEIPT_MAX_BYTES` 8 KiB, `BATCH_MAX_BYTES` 20 KiB, `TICK_STDOUT_MAX_BYTES` 64 KiB, `MAX_EFFECTS_PER_TICK` 16 | type | `src/tick/wire.rs` | The 11-D3 command lines, frozen in the module doc. `--effects`: exit 0 means exactly one line on stdout, and every intent row is already pushed. A tick that started and then failed still exits 0, with a `tick-aborted` item. A non-zero exit prints nothing. The runner posts `packet.text` verbatim. |
| S-tick-11 | `Capability` (11-D19's nine ids, serde-renamed), `EffectInput`, `Effect`; `Capability::id`, `EffectInput::capability`, `Effect::new` | type | `src/tick/wire.rs` | A typed input fixes its capability, so a mismatched pair cannot be built. A new capability is a skeleton change and a grant change. |
| S-tick-12 | `MergeInput { owner, repo, number, mergeMethod, expectedHeadSha }`, `MergeMethod`, `MERGE_METHOD`, `EnqueueInput`, `WorkflowRunInput { owner, repo, workflow: u64, ref, inputs: [WorkflowField] }`, `PinUpdateInput`, `PinValue`, `PinMergeInput`, `ApplicationInput`, `DeploymentInput`, `ReadyzInput` | type | `src/tick/wire.rs` | `deny_unknown_fields`, camelCase where the provider uses it. Merge and workflow inputs follow provider-gh `5b1616e`. Enqueue and pins follow 11-D15, 11-D17 and 11-D18. kubectl inputs are this skeleton's reading of 11-D20 (G6). |
| S-tick-13 | `Receipt`, `EffectOutcome` (`applied`, `refused`, `error`), `ReceiptOutput { Oversize, Inline }`, `Oversize`, `ReceiptOutput::bounded`; `Admitted`, `admit` | type | `src/tick/wire.rs` | `applied` and `refused` close an intent; `error` leaves it open for reconcile. An output that makes its receipt reach 8 KiB is recorded as `{oversize, bytes, sha256}`, never truncated. `admit` sizes each effect with the longest intent id. |
| S-tick-14 | `IntentId`, `Nonce`, `Family` (`writer_of`, `other`), `IntentKind` (`step-launch`, `review-launch`, `effect`), `IntentRow`, `Draft`, `Bind`, `Ruling`, `VerdictRow`, `VerdictProof`, `LedgerRow` (`intent`, `bound`, `parked`, `receipt`, `ruled`, `verdict`), `IntentState` (`Open`, `Handed`, `Bound`, `Parked`, `Received`, `Closed`), `Intended`, `Recorded`; `LEDGER_FILE`, `MAX_OPEN_INTENTS` 64, `INTENT_ID_MAX_LEN` 20 | type | `src/tick/ledger.rs` | Append-only JSON Lines, one tagged row each, never rewritten. An intent row carries the done check's fields (PR, head, nonce, model, family), plus the effect `input`, so a retry emits the same bytes. |
| S-tick-15 | `EffectLedger`: `open`, `path`, `rows`, `state`, `open_intents`, `parked`, `intent`, `intend`, `bind`, `park`, `record`, `rule`, `verdict`, `mint_nonce`, `by_nonce`, `nonce_consumed`, `receipt`, `verdicts_at` | type | `src/tick/ledger.rs` | Each append is one `write` of one whole line, then fsync. `open` drops a torn last line. `intend` refuses past 64 open intents and answers `AlreadyOpen` for a second intent of the same purpose. `record` is idempotent by intent. The nonce is 32 hex digits derived from the intent id. |
| S-tick-16 | `Observer`, `Observation` (`Done`, `NotDone`, `Undecidable`, `Unread`), `Reconciled`, `reconcile` | trait | `src/tick/ledger.rs` | `Done` binds; `Undecidable` parks with a packet item; `NotDone` retries under the same id, counted by timers; `Unread` (a failed lookup) leaves the intent open and parks nothing. A `Handed` intent is looked up for done, never re-emitted. |
| S-tick-17 | `SystemObserver` and its routing `impl Observer` | fn | `src/tick/ledger.rs` | Skeleton-owned glue. Routes step launches to `dispatch::observe_step`, review launches to `dispatch::observe_review`, merges and enqueues to `merge::observe`, `gh.workflow.run` to `release::observe`, and pins and kubectl to `lan::observe`. |
| S-tick-18 | `TickLock::try_acquire`, `TICK_LOCK` | type | `src/tick/ledger.rs` | Filled by `tick/ledger`; the rule is S-tick-05. |
| S-tick-19 | `SystemGit { program, timeout }` | task | `src/tick/push.rs` | **Owner:** the tick's thread; one child at a time through `forge::run`. **Bound:** `GIT_TIMEOUT` 60 s, killed on expiry. **Queue:** none. **Shutdown:** an unpushed commit stays local, and the next tick's `begin` pushes it before anything else. |
| S-tick-20 | `Git` (`toplevel`, `pull_no_rebase`, `dirty`, `commit`, `push_main`, `unpushed`, `show`, `has_commit`, `fetch_sha`), `PullResult`, `PushResult`, `Pushed`, `begin`, `commit_and_push`; `PUSH_TRIES` 3 | trait | `src/tick/push.rs` | 11-D7 and 11-D8. A conflicting pull is aborted and becomes `PullConflict`, with nothing written. A commit holds the tick's own files (`git add --force`, because campaign-trail ignores `*.jsonl`) plus what changed under the campaign during the tick, never another writer's earlier dirty file. Never `--force` on a push, never another ref. |
| S-tick-21 | `record`, `parse_batch`, `record_receipts` | fn | `src/tick/record.rs` | `cm effects record`: the batch is taken whole or not at all, every receipt is checked before any is appended, and the batch is committed and pushed before returning. The local applier records through `record_receipts`. |
| S-tick-22 | `resolve` | fn | `src/tick/record.rs` | `cm effects resolve <intent> --as done\|retry\|drop`: the ruling row 11-D22 names. Refused on an intent that is not parked. |
| S-tick-23 | `Applier` (`Runner`, `Local`), `Applied`, `Apply`, `AppliedRun`, `apply_in_order` | trait | `src/tick/apply.rs` | One cycle, two appliers (C3). The local run keeps 11-D3 (2)'s order and stops at the first refusal or error. |
| S-tick-24 | `LocalApply` and its routing `impl Apply` (glue); `defer` | fn | `src/tick/apply.rs` | The glue routes merge to `merge::apply_local`, `gh.workflow.run` to `release::apply_local`, and the rest to `defer`. `defer` (filled by `tick/ledger`) builds the packet action, including gate-merge's REST head bracket for an enqueue and the `cm effects record` line that closes it. |
| S-tick-30 | `ApprovedSha`, `Bump`, `Authority` (`canonical`, `may_merge`, `may_release`) | type | `src/tick/authority.rs` | 40 lower-case hex only. The rename rule is applied on both sides of every lookup. `Major` is never authorized. |
| S-tick-31 | `Gates` (`required`), `ChecksRead`, `checks_green`, `CLASSIFIER_CHECK` | fn | `src/tick/authority.rs` | §Gates: every named check present, every check `success`, `skipped` or `neutral`, at least one `success` besides `classify CI changes`. Zero checks is red, and a repository with no entry is red. `forge::ci_green` stays the `cm sync` rule. |
| S-tick-32 | `Phase`, `ProofRule`, `WriterProof`, `PhaseTable`, `ReviewTable` (`phase_for`, `proof`) | type | `src/tick/authority.rs` | §Lanes "Review proof per phase", read at the approved SHA. The phase comes from cm state (`starts_after` steps done; `installed` evidence), never from LIMITS live. |
| S-tick-33 | `Approved` (`read`), `paused_live` | fn | `src/tick/authority.rs` | One `git show <sha>:<campaign>/LIMITS.toml` yields all three tables, with one fetch if the SHA is absent. A missing table is `AuthorityMissing`: apply nothing, park. A working-tree LIMITS cm cannot read counts as paused. |
| S-tick-34 | `TimerKind` (`retry`, `collect`, `review-deadline`, `reminder`), `Timer`, `Timers` (`load`, `save`, `arm`, `disarm`, `take_due`), `backoff`; `MAX_TIMERS` 256, `MAX_TRIES` 5, `REMINDER_INTERVAL` 6 h, `REVIEW_DEADLINE` 90 min | type | `src/tick/timers.rs` | Arming is idempotent by key. Saved in the tick's first commit. |
| S-tick-35 | `IssueComment`, `PullReads`, `Watched`, `Event` (`Opened`, `HeadMoved`, `ReviewRequested`, `Comment`), `PrCursor`, `Cursor`, `Intake`, `watched`, `intake`; `MAX_EVENTS_PER_TICK` 200, `MAX_READS_PER_TICK` 120, `REVIEW_COMMAND` | type | `src/tick/intake.rs` | Polls only the PRs cm tracks (steps or parts at `pr-open`), with reads 11-D10 already opens. No PR list read (G9). `Cursor::last_tick` is "one heartbeat ago" for readiness. |
| S-tick-36 | `GhPullReads` and the intake read loop | task | `src/tick/intake.rs` | **Owner:** the tick's thread; one `gh` child per read, `forge::GH_TIMEOUT`. **Bound:** 120 reads, 200 events, round-robin from `Cursor::rotation`. **Full queue:** the unread PRs go first next tick, counted `deferred`, with no item. **Failure:** `unread` with its reason; the cursor is unchanged and the tick goes on. **Shutdown:** nothing in flight between ticks. |
| S-tick-37 | `LaunchRoute` (`astra-script`, `mac-lane`, `packet`), `Purpose`, `route`, `Ready`, `ReviewWant`, `Launched`, `Dispatched` | type | `src/tick/dispatch.rs` | Routes are C16. A GPT review after gpt-route and a Codex or coordinator step have no capability, so they are packet actions (G1). |
| S-tick-38 | `Launcher` (`astra_start`, `astra_collect`), `AstraStart`, `Collected`, `ScriptLauncher`; `MAX_LAUNCHES_PER_TICK` 8, `LAUNCH_TIMEOUT` 300 s | task | `src/tick/dispatch.rs` | **Owner:** the tick's thread; one `astra-review.sh` child at a time. **Bound:** 8 launch intents per tick, reviews first, then steps in `cm next` order. **Full queue:** the rest are deferred and picked again; no item. **Shutdown:** every intent is on main before its launch, so a killed launch reconciles and is never repeated. Collect polls run on `collect` timers. |
| S-tick-39 | `ready`, `reviews_owed`, `plan_stage`, `launch_stage`, `observe_step`, `observe_review` | fn | `src/tick/dispatch.rs` | Before the push: launch intents, plus `cm launch` and `cm attempt begin` for a step. After the push: the launches, binds and `cm attempt record`. `observe_step` reads `attempt::plan`: `resume` or `ended` binds, `unknown` is undecidable, `dispatch` is not done. |
| S-tick-40 | `PacketKind`, `PacketItem`, `DecisionPacket` (`push`, `is_empty`, `render`, `to_wire`), `Seen` (`load`, `deliver`, `save`); `PACKET_MAX_ITEMS` 20, `PACKET_MAX_BYTES` 16 KiB, `MAX_SEEN` 1024 | type | `src/tick/packet.rs` | Built from deltas and delivered at most once: seen keys go into the first commit. A persisting state comes back through its reminder timer. Each `cm sync` violation is one `sync` item. |
| S-tick-41 | `TickArgs`, `Deps`, `Planned`, `Counters`, `Cycle` (`plan`, `wrote`, `written`, `dirty_at_start`), `TickRun`, `tick`, `cli_tick` | fn | `src/tick/run.rs` | The stage order, 0 to 11, is in the module doc. Two commits: the first carries intents, cursor, timers and seen set, before anything external happens; the second carries binds and receipts. Paused: stages 6 to 8 plan nothing, and stage 10 launches and applies nothing. |
| S-tick-42 | the tick process | task | `src/tick/run.rs` | **Owner:** the `cm` main thread. **Bound:** one tick per campaign (S-tick-05), each stage within its own bound. **Queue:** none in memory: what waits is in the ledger, the timers or the cursor. **Shutdown:** a killed tick leaves at most an unpushed commit and a torn ledger line, which the next tick's stages 1 and 3 handle. |
| S-tick-50 | `TrustedApps`, `Printed`, `BoundVerdict`, `Refusal`, `Gated`; `REVIEW_BUDGET` 3 | type | `src/tick/gate.rs` | `Refusal` names every must-not-merge case in the done check, plus the grammar's: `NoNonce`, `ShortHead`, `NoWord` (unsure, which goes to Opus), `WordDisagrees`, `ReceiptUnfinished`, `ReportUnrecorded`. |
| S-tick-51 | `parse_comment`, `bind_comment`, `bind_astra`, `fix_required_count` | fn | `src/tick/gate.rs` | Binds only at the PR's current head (C10). Binding writes the `verdict` row that consumes the nonce. A third FIX REQUIRED on one PR is a `review-budget` item. |
| S-tick-52 | `proof_gaps`, plus the existing-file changes: `verdict.rs` (parse `nonce:`; `merge_gaps` takes `&ProofRule`; the RULING text), `commands.rs` (`cm advance`), `sync.rs` (`gate_holds` for every reviewed lane) | fn | `src/tick/gate.rs`, `src/verdict.rs`, `src/commands.rs`, `src/sync.rs` | "`cm advance` and `cm sync` take the verdict proof LIMITS names for the phase" (C15). |
| S-tick-53 | `BranchKind`, `MergeReads` (`queued`), `GhMergeReads`, `MergePlan`, `PostMerge` | type | `src/tick/merge.rs` | `queued` reads `rules/branches/{branch}` (11-D10). |
| S-tick-54 | `route`, `confirm`, `observe`, `apply_local`, `pins_held`, `stage` | fn | `src/tick/merge.rs` | Queued gets `enqueue` with `expectedHeadSha`; any other branch gets REST `merge` with it. In the interim, the enqueue is a packet action. `confirm` reads the merged PR; a mismatch parks (revert PR, every pin held, journaled; G2), and `pins_held` holds pins until a ruling. `route` asks `release::drop_admits` before a core merge (C11). |
| S-tick-60 | `CORE_DROP_CAP` 3, `CORE_REPO`, `CUT_WORKFLOW_FILE`, `ReleaseReads` (`last_tag`, `workflow_id`, `branch_head`), `GhReleaseReads`, `Cut`, `ReleaseRefusal`, `CutPlan`, `DropTally` | type | `src/tick/release.rs` | The tally is read from the open drop's `RELEASE.md` every time. `workflow_id` is G3. |
| S-tick-61 | `DropTally::read`, `drop_admits`, `plan`, `cut_input`, `observe`, `apply_local`, `stage` | fn | `src/tick/release.rs` | Cuts happen only from a ready `00-release` step, with the bump checked against the last tag and §Gates green at main's head. A refused dispatch files a MAC-LANE item with the dry-run id. A cut intent is bound by its tag. |
| S-tick-62 | `HOMELAB_REPO`, `LanReads` (`homelab_pulls`), `GhLanReads`, `HomelabPull`, `Readiness`, `Hold`, `ProbeResult` | type | `src/tick/lan.rs` | `homelab_pulls` is G4. |
| S-tick-63 | `readiness`, `readiness_hold`, `ready_homelab_prs`, `pin_update`, `pin_merge`, `lan_leg`, `probe`, `observe`, `stage` | fn | `src/tick/lan.rs` | Readiness is the newest `kubectl.get.readyz` ledger receipt. It is fresh when recorded after the previous tick (C9). A pin merge is built from the recorded update receipt. |
| S-tick-70 | `Requested` (`--model`, `--effort`, `--role` on `cm launch`), `Echo` (`--echo-*`, `--tokens-*` on `attempt record`, `attempt end` and `review record`) | type | `src/tick/agents.rs` | Registered by the skeleton. All absent means existing behaviour. |
| S-tick-71 | `Count`, `Pair`, `Harness`, `Tokens`, `AgentRow`; `AGENTS_FILE`, `UNKNOWN` | type | `src/tick/agents.rs` | LEDGER.md's §Fields exactly, plus `invocation` (cm's own key) and a flattened map that keeps LEDGER.md's optional fields. |
| S-tick-72 | `record_launch`, `echoed` | fn | `src/tick/agents.rs` | One row per invocation, rewritten in place under `store::lock`. Every other line is kept byte for byte. An unechoed value is `"unknown"`, never copied from the request. |
| S-tick-73 | `Node`, `Edge`, `Graph`, `graph`, `export` | fn | `src/tick/export.rs` | `cm export --json` carries the graph under `graph`: state and edges only (G8). |

## Lifecycle invariants

Every stateful type has its four stubs: abort mid-operation, reset or reload, the growth bound, and restart from durable state. The ledger, the checkout, the batch, intake, dispatch and the cycle carry a fifth or sixth for their own failure mode (an unread lookup, a conflicting pull, a ruling, a failed read, a launch error, the lock and the error line). `wire.rs` carries three bound checks.

| id | test | type | unit that fills it |
|---|---|---|---|
| S-tick-100 | `a_killed_append_leaves_no_partial_row_and_the_torn_line_is_dropped_on_open` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-101 | `a_reopened_ledger_reads_every_row_it_wrote_in_order_with_the_same_states` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-102 | `open_intents_never_exceed_sixty_four_and_a_target_holds_one_open_intent_per_purpose` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-103 | `a_restart_between_intent_and_effect_binds_retries_or_parks_and_never_launches_twice` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-104 | `a_minted_nonce_is_unique_in_the_ledger_and_consumed_once_its_verdict_row_is_written` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-105 | `an_unread_lookup_leaves_the_intent_open_and_parks_nothing` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-106 | `an_interrupted_push_leaves_the_commit_local_and_its_effects_unemitted` (`src/tick/push.rs`) | the checkout (`Git`, `commit_and_push`) | `tick/ledger` |
| S-tick-107 | `a_rejected_push_pulls_without_rebase_and_pushes_the_merge_from_the_new_tip` (`src/tick/push.rs`) | the checkout (`Git`, `commit_and_push`) | `tick/ledger` |
| S-tick-108 | `a_push_is_tried_at_most_three_times_then_reports_exhausted` (`src/tick/push.rs`) | the checkout (`Git`, `commit_and_push`) | `tick/ledger` |
| S-tick-109 | `a_restart_with_an_unpushed_commit_pushes_it_before_the_pull_and_before_any_effect` (`src/tick/push.rs`) | the checkout (`Git`, `commit_and_push`) | `tick/ledger` |
| S-tick-110 | `a_conflicting_pull_is_aborted_and_the_tick_writes_nothing` (`src/tick/push.rs`) | the checkout (`Git`, `commit_and_push`) | `tick/ledger` |
| S-tick-111 | `a_gitignored_tick_file_is_committed_and_another_writers_dirty_file_is_not` (`src/tick/push.rs`) | the checkout (`Git`, `commit_and_push`) | `tick/ledger` |
| S-tick-112 | `a_refused_batch_records_none_of_its_receipts` (`src/tick/record.rs`) | a receipt batch | `tick/ledger` |
| S-tick-113 | `rerecording_a_pushed_batch_is_a_no_op_by_intent_id` (`src/tick/record.rs`) | a receipt batch | `tick/ledger` |
| S-tick-114 | `a_batch_over_twenty_kib_is_refused_whole_and_an_eight_kib_output_is_recorded_oversize` (`src/tick/record.rs`) | a receipt batch | `tick/ledger` |
| S-tick-115 | `a_restart_after_a_pushed_batch_takes_the_next_batch_without_rerecording` (`src/tick/record.rs`) | a receipt batch | `tick/ledger` |
| S-tick-116 | `a_ruling_on_an_intent_that_is_not_parked_is_refused` (`src/tick/record.rs`) | a receipt batch | `tick/ledger` |
| S-tick-117 | `the_local_run_stops_at_the_first_refusal_and_leaves_later_intents_open` (`src/tick/apply.rs`) | a local apply run | `tick/ledger` |
| S-tick-118 | `a_tick_killed_mid_apply_reconciles_the_applied_effect_and_reemits_only_the_rest` (`src/tick/apply.rs`) | a local apply run | `tick/ledger` |
| S-tick-119 | `a_deferred_effect_stays_open_until_its_receipt_is_recorded` (`src/tick/apply.rs`) | a local apply run | `tick/ledger` |
| S-tick-120 | `every_input_round_trips_through_the_provider_field_names` (`src/tick/wire.rs`) | `EffectsOut` (stdout bound) | `tick/ledger` |
| S-tick-121 | `admitted_effects_and_packet_never_print_past_sixty_four_kib_or_sixteen_effects` (`src/tick/wire.rs`) | `EffectsOut` (stdout bound) | `tick/ledger` |
| S-tick-122 | `an_output_whose_receipt_reaches_eight_kib_is_recorded_oversize_with_its_hash` (`src/tick/wire.rs`) | `EffectsOut` (stdout bound) | `tick/ledger` |
| S-tick-123 | `authority_is_read_at_the_approved_sha_and_never_from_the_working_tree` (`src/tick/authority.rs`) | `Approved` | `tick/core` |
| S-tick-124 | `an_unreachable_sha_is_fetched_once_then_the_tick_applies_nothing_and_parks` (`src/tick/authority.rs`) | `Approved` | `tick/core` |
| S-tick-125 | `either_name_of_a_rename_selects_the_same_release_gate_and_phase_entries` (`src/tick/authority.rs`) | `Approved` | `tick/core` |
| S-tick-126 | `paused_read_live_stops_every_launch_and_effect_and_an_unreadable_limits_reads_paused` (`src/tick/authority.rs`) | `Approved` | `tick/core` |
| S-tick-127 | `a_missing_named_check_zero_checks_or_a_lone_classifier_success_is_red` (`src/tick/authority.rs`) | `Approved` | `tick/core` |
| S-tick-128 | `a_tick_killed_after_take_due_leaves_the_pushed_timers_file_unchanged` (`src/tick/timers.rs`) | `Timers` | `tick/core` |
| S-tick-129 | `rearming_a_key_moves_its_due_time_and_never_duplicates_it` (`src/tick/timers.rs`) | `Timers` | `tick/core` |
| S-tick-130 | `timers_never_exceed_the_cap_and_a_retry_parks_after_five_tries` (`src/tick/timers.rs`) | `Timers` | `tick/core` |
| S-tick-131 | `a_restart_fires_each_overdue_timer_once` (`src/tick/timers.rs`) | `Timers` | `tick/core` |
| S-tick-132 | `a_killed_tick_leaves_the_pushed_cursor_at_the_last_tick_that_pushed` (`src/tick/intake.rs`) | `Cursor` | `tick/core` |
| S-tick-133 | `a_reloaded_cursor_takes_in_each_comment_and_head_exactly_once` (`src/tick/intake.rs`) | `Cursor` | `tick/core` |
| S-tick-134 | `intake_reads_at_most_its_bound_round_robin_and_a_closed_pr_drops_out_of_the_cursor` (`src/tick/intake.rs`) | `Cursor` | `tick/core` |
| S-tick-135 | `a_restart_rereads_since_the_cursor_and_owes_one_review_per_head` (`src/tick/intake.rs`) | `Cursor` | `tick/core` |
| S-tick-136 | `a_failed_read_for_one_pr_is_unread_and_every_other_pr_is_still_taken_in` (`src/tick/intake.rs`) | `Cursor` | `tick/core` |
| S-tick-137 | `a_launch_killed_after_its_intent_is_pushed_is_reconciled_and_never_launched_twice` (`src/tick/dispatch.rs`) | launch intents | `tick/core` |
| S-tick-138 | `a_reloaded_ledger_makes_ready_skip_every_step_with_an_open_or_parked_launch` (`src/tick/dispatch.rs`) | launch intents | `tick/core` |
| S-tick-139 | `at_most_eight_launches_per_tick_reviews_first_and_the_rest_deferred` (`src/tick/dispatch.rs`) | launch intents | `tick/core` |
| S-tick-140 | `a_restart_starts_one_astra_review_whose_nonce_was_on_main_before_the_start` (`src/tick/dispatch.rs`) | launch intents | `tick/core` |
| S-tick-141 | `a_launch_error_parks_that_intent_and_the_next_launch_still_runs` (`src/tick/dispatch.rs`) | launch intents | `tick/core` |
| S-tick-142 | `a_tick_killed_before_its_first_push_marks_no_packet_item_seen` (`src/tick/packet.rs`) | `Seen` | `tick/core` |
| S-tick-143 | `a_reloaded_seen_set_keeps_a_delivered_delta_out_of_the_next_packet` (`src/tick/packet.rs`) | `Seen` | `tick/core` |
| S-tick-144 | `a_packet_holds_at_most_twenty_items_or_sixteen_kib_and_the_seen_set_at_most_1024_keys` (`src/tick/packet.rs`) | `Seen` | `tick/core` |
| S-tick-145 | `a_restart_brings_a_parked_intent_back_only_through_its_reminder` (`src/tick/packet.rs`) | `Seen` | `tick/core` |
| S-tick-146 | `a_tick_that_errors_before_its_first_push_leaves_ledger_cursor_timers_and_seen_set_unchanged` (`src/tick/run.rs`) | `Cycle` and `TickLock` | `tick/core` |
| S-tick-147 | `a_second_tick_over_unchanged_state_emits_no_effect_launches_nothing_and_has_an_empty_packet` (`src/tick/run.rs`) | `Cycle` and `TickLock` | `tick/core` |
| S-tick-148 | `a_tick_emits_at_most_sixteen_effects_and_its_stdout_stays_under_sixty_four_kib` (`src/tick/run.rs`) | `Cycle` and `TickLock` | `tick/core` |
| S-tick-149 | `every_effect_and_launch_has_its_intent_row_pushed_before_it_happens` (`src/tick/run.rs`) | `Cycle` and `TickLock` | `tick/core` |
| S-tick-150 | `a_second_tick_started_while_one_runs_exits_at_once_and_writes_nothing` (`src/tick/run.rs`) | `Cycle` and `TickLock` | `tick/core` |
| S-tick-151 | `with_effects_a_failing_tick_still_prints_one_line_with_a_tick_aborted_item` (`src/tick/run.rs`) | `Cycle` and `TickLock` | `tick/core` |
| S-tick-152 | `a_tick_killed_mid_bind_binds_nothing_until_its_verdict_row_is_pushed` (`src/tick/gate.rs`) | nonce use (`VerdictRow`) | `tick/gate-merge` |
| S-tick-153 | `a_reloaded_ledger_still_refuses_a_replayed_nonce` (`src/tick/gate.rs`) | nonce use (`VerdictRow`) | `tick/gate-merge` |
| S-tick-154 | `one_nonce_binds_at_most_one_verdict_and_a_third_fix_required_stops_the_lane` (`src/tick/gate.rs`) | nonce use (`VerdictRow`) | `tick/gate-merge` |
| S-tick-155 | `a_restart_still_refuses_every_must_not_merge_comment` (`src/tick/gate.rs`) | nonce use (`VerdictRow`) | `tick/gate-merge` |
| S-tick-156 | `a_merge_killed_after_its_effect_is_confirmed_from_the_merged_pr_and_never_resent` (`src/tick/merge.rs`) | merge and enqueue intents | `tick/gate-merge` |
| S-tick-157 | `a_reloaded_ledger_confirms_a_pending_merge_by_its_gated_head` (`src/tick/merge.rs`) | merge and enqueue intents | `tick/gate-merge` |
| S-tick-158 | `a_pr_holds_at_most_one_open_merge_or_enqueue_intent` (`src/tick/merge.rs`) | merge and enqueue intents | `tick/gate-merge` |
| S-tick-159 | `a_restart_after_a_mismatched_merge_keeps_every_pin_held_until_a_ruling` (`src/tick/merge.rs`) | merge and enqueue intents | `tick/gate-merge` |
| S-tick-160 | `a_cut_killed_after_dispatch_is_bound_by_its_tag_and_never_dispatched_twice` (`src/tick/release.rs`) | `DropTally` and cut intents | `tick/release-lan` |
| S-tick-161 | `a_reloaded_tally_is_the_release_md_tally_not_a_count_in_memory` (`src/tick/release.rs`) | `DropTally` and cut intents | `tick/release-lan` |
| S-tick-162 | `a_fourth_counting_core_merge_is_refused_while_the_drop_holds_three` (`src/tick/release.rs`) | `DropTally` and cut intents | `tick/release-lan` |
| S-tick-163 | `a_restart_still_refuses_the_fourth_counting_merge_and_a_cut_at_zero_required_checks` (`src/tick/release.rs`) | `DropTally` and cut intents | `tick/release-lan` |
| S-tick-164 | `a_pin_killed_between_update_and_merge_merges_from_the_recorded_receipt` (`src/tick/lan.rs`) | pin intents and `Readiness` | `tick/release-lan` |
| S-tick-165 | `a_missing_failed_or_stale_readiness_receipt_holds_the_pin` (`src/tick/lan.rs`) | pin intents and `Readiness` | `tick/release-lan` |
| S-tick-166 | `at_most_one_non_draft_homelab_pr_is_ready_at_a_time` (`src/tick/lan.rs`) | pin intents and `Readiness` | `tick/release-lan` |
| S-tick-167 | `a_restart_after_a_failed_probe_keeps_the_step_held` (`src/tick/lan.rs`) | pin intents and `Readiness` | `tick/release-lan` |
| S-tick-168 | `a_killed_rewrite_leaves_the_previous_file_whole` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-169 | `a_reloaded_file_keeps_hand_written_record_py_rows_byte_for_byte_beside_cm_rows` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-170 | `an_invocation_holds_exactly_one_row_however_many_echoes_fill_it` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-171 | `a_restart_before_the_echo_leaves_every_unechoed_value_unknown` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |

## Done-check stubs

Integration stubs, one file per unit. Together they carry each unit's UNITS.json done check. `tick/ledger`'s three extra stubs carry C2's additions.

| id | test | file | unit |
|---|---|---|---|
| S-tick-200 | `cm_writes_and_pushes_an_intent_row_with_pr_head_nonce_model_and_family_before_its_effect` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-201 | `a_restart_between_intent_and_effect_reconciles_or_parks_with_no_second_launch` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-202 | `tick_effects_stdout_parses_as_the_runner_wire_shape_with_provider_field_names` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-203 | `effects_record_takes_a_twenty_kib_batch_commits_and_pushes_it_and_records_oversize_outputs_by_hash` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-204 | `effects_resolve_closes_a_parked_intent_and_retry_reemits_it_under_the_same_id` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-205 | `tick_on_a_fixture_epoch_exits_with_an_empty_packet_when_nothing_needs_judgment` | `tests/tick_core.rs` | `tick/core` |
| S-tick-206 | `tick_launches_each_ready_step_once_across_two_ticks` | `tests/tick_core.rs` | `tick/core` |
| S-tick-207 | `tick_reads_authority_from_the_approved_sha_and_only_paused_live` | `tests/tick_core.rs` | `tick/core` |
| S-tick-208 | `tick_launches_one_astra_review_through_astra_review_sh_with_the_nonce_in_the_ledger_before_launch` | `tests/tick_core.rs` | `tick/core` |
| S-tick-209 | `tick_keeps_taking_in_and_dispatching_when_the_forge_fails_for_some_prs` | `tests/tick_core.rs` | `tick/core` |
| S-tick-210 | `one_correctly_bound_verdict_per_family_merges` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-211 | `a_hand_written_other_app_unmatched_replayed_same_family_old_head_or_misprinted_head_verdict_is_refused` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-212 | `an_astra_receipt_for_another_head_or_with_no_ledger_row_is_refused` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-213 | `advance_and_sync_take_the_proof_the_phase_names` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-214 | `a_codex_step_merges_on_a_bound_claude_verdict_with_no_astra_receipt_through_advance_and_sync` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-215 | `a_queued_branch_enqueues_with_the_gated_head_and_a_rest_branch_merges_with_sha` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-216 | `the_interim_enqueue_is_a_packet_action_bracketed_by_rest_head_reads` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-217 | `a_merged_head_that_differs_from_the_gated_head_parks_with_a_revert_and_holds_every_pin` | `tests/tick_gate_merge.rs` | `tick/gate-merge` |
| S-tick-218 | `a_release_is_cut_only_through_the_release_step_with_its_bump_checked_against_the_last_tag` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-219 | `zero_required_checks_is_red` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-220 | `a_pin_opens_then_merges_on_a_later_tick_from_the_recorded_update_receipt` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-221 | `a_failed_probe_holds_the_step_and_only_a_probe_step_that_carries_one_files_a_mac_lane_item` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-222 | `only_non_draft_homelab_prs_count` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-223 | `a_missing_failed_or_stale_readiness_receipt_holds_the_pin` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-224 | `a_fourth_counting_core_merge_is_refused_across_restarts_while_the_tally_holds_three` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-225 | `cm_launch_writes_the_requested_model_and_effort` | `tests/tick_agent_ledger.rs` | `tick/agent-ledger` |
| S-tick-226 | `attempt_record_and_review_record_fill_the_echoed_values` | `tests/tick_agent_ledger.rs` | `tick/agent-ledger` |
| S-tick-227 | `attempt_end_fills_tokens_at_close_and_an_unechoed_value_is_unknown` | `tests/tick_agent_ledger.rs` | `tick/agent-ledger` |
| S-tick-228 | `every_row_parses_in_the_ledger_md_schema` | `tests/tick_agent_ledger.rs` | `tick/agent-ledger` |
| S-tick-229 | `cm_export_json_emits_the_graph_the_dashboard_reads` | `tests/tick_agent_ledger.rs` | `tick/agent-ledger` |

## Footprint

```footprint
{"unit": "tick/ledger", "step": "07-tick/ledger/build", "files": ["src/tick/wire.rs", "src/tick/ledger.rs", "src/tick/push.rs", "src/tick/record.rs", "src/tick/apply.rs", "tests/tick_ledger.rs", "tests/fixtures/tick-ledger/"], "items": ["S-tick-10", "S-tick-11", "S-tick-12", "S-tick-13", "S-tick-14", "S-tick-15", "S-tick-16", "S-tick-18", "S-tick-19", "S-tick-20", "S-tick-21", "S-tick-22", "S-tick-23", "S-tick-24", "S-tick-100", "S-tick-101", "S-tick-102", "S-tick-103", "S-tick-104", "S-tick-105", "S-tick-106", "S-tick-107", "S-tick-108", "S-tick-109", "S-tick-110", "S-tick-111", "S-tick-112", "S-tick-113", "S-tick-114", "S-tick-115", "S-tick-116", "S-tick-117", "S-tick-118", "S-tick-119", "S-tick-120", "S-tick-121", "S-tick-122", "S-tick-200", "S-tick-201", "S-tick-202", "S-tick-203", "S-tick-204"], "rank": 1}
{"unit": "tick/core", "step": "07-tick/core/build", "files": ["src/tick/authority.rs", "src/tick/timers.rs", "src/tick/intake.rs", "src/tick/dispatch.rs", "src/tick/packet.rs", "src/tick/run.rs", "tests/tick_core.rs", "tests/fixtures/tick-core/"], "items": ["S-tick-30", "S-tick-31", "S-tick-32", "S-tick-33", "S-tick-34", "S-tick-35", "S-tick-36", "S-tick-37", "S-tick-38", "S-tick-39", "S-tick-40", "S-tick-41", "S-tick-42", "S-tick-123", "S-tick-124", "S-tick-125", "S-tick-126", "S-tick-127", "S-tick-128", "S-tick-129", "S-tick-130", "S-tick-131", "S-tick-132", "S-tick-133", "S-tick-134", "S-tick-135", "S-tick-136", "S-tick-137", "S-tick-138", "S-tick-139", "S-tick-140", "S-tick-141", "S-tick-142", "S-tick-143", "S-tick-144", "S-tick-145", "S-tick-146", "S-tick-147", "S-tick-148", "S-tick-149", "S-tick-150", "S-tick-151", "S-tick-205", "S-tick-206", "S-tick-207", "S-tick-208", "S-tick-209"], "rank": 2}
{"unit": "tick/agent-ledger", "step": "07-tick/agent-ledger/build", "files": ["src/tick/agents.rs", "src/tick/export.rs", "tests/tick_agent_ledger.rs", "tests/fixtures/tick-agent-ledger/"], "items": ["S-tick-70", "S-tick-71", "S-tick-72", "S-tick-73", "S-tick-168", "S-tick-169", "S-tick-170", "S-tick-171", "S-tick-225", "S-tick-226", "S-tick-227", "S-tick-228", "S-tick-229"], "rank": 2}
{"unit": "tick/gate-merge", "step": "07-tick/gate-merge/build", "files": ["src/tick/gate.rs", "src/tick/merge.rs", "src/tick/run.rs", "src/verdict.rs", "src/commands.rs", "src/sync.rs", "tests/tick_gate_merge.rs", "tests/verdict.rs", "tests/lifecycle.rs", "tests/support/verdict.rs", "tests/fixtures/tick-gate-merge/"], "items": ["S-tick-50", "S-tick-51", "S-tick-52", "S-tick-53", "S-tick-54", "S-tick-152", "S-tick-153", "S-tick-154", "S-tick-155", "S-tick-156", "S-tick-157", "S-tick-158", "S-tick-159", "S-tick-210", "S-tick-211", "S-tick-212", "S-tick-213", "S-tick-214", "S-tick-215", "S-tick-216", "S-tick-217"], "rank": 3}
{"unit": "tick/release-lan", "step": "07-tick/release-lan/build", "files": ["src/tick/release.rs", "src/tick/lan.rs", "src/tick/run.rs", "tests/tick_release_lan.rs", "tests/fixtures/tick-release-lan/"], "items": ["S-tick-60", "S-tick-61", "S-tick-62", "S-tick-63", "S-tick-160", "S-tick-161", "S-tick-162", "S-tick-163", "S-tick-164", "S-tick-165", "S-tick-166", "S-tick-167", "S-tick-218", "S-tick-219", "S-tick-220", "S-tick-221", "S-tick-222", "S-tick-223", "S-tick-224"], "rank": 4}
```

Units never edit `src/lib.rs`, `src/tick/mod.rs`, `src/tick/error.rs`, `src/cli.rs`, `Cargo.toml` or `src/forge.rs`. A new error variant, CLI flag, capability, read trait method or dependency is a skeleton change (standing order B2). `src/verdict.rs` beyond the S-tick-04 field is `tick/gate-merge`'s.

The `run.rs` stage lines go in this order: `merge::stage(&mut cycle)?` is stage 6, added by `tick/gate-merge`; `release::stage` then `lan::stage` are stage 7, added by `tick/release-lan`. Each of those units edits nothing else in `run.rs`.

## Named gaps

These are interface needs no unit in this area can meet inside its own footprint, and the plan does not settle them. Each is named, not decided. None blocks the skeleton or any done check in the cloud interim, where `gh` reads and writes everything. They bite on the RPi.

- **G1. GPT review request (11-D19).** After gpt-route, PLAN.md `07-tick/core` starts a GPT review with "a review-request comment the github-events route picks up". 11-D19 grants no issue-comment capability, so `dispatch::route` makes it a packet action, and nothing posts it unattended on the RPi.
- **G2. Revert PR (11-D19).** `07-tick/gate-merge` (3) opens a revert PR of a mismatched merge commit. 11-D19 builds no PR-create, so it is a packet action for the coordinator.
- **G3. Cut workflow id (11-D10, 11-D14).** The Cedar fence is on `cut-release.yml`'s numeric id. Reading it (`GET …/actions/workflows/cut-release.yml`) is not in the egress route table. Either the route opens, or the ids come stamped at the approved SHA.
- **G4. Homelab PR list (11-D10).** "Counts only non-draft homelab PRs" needs `GET /repos/xrl/rpi-homelab/pulls?state=open`. The route table opens only `pulls/{number}` there.
- **G5. Clone on session create (11-D7).** "cm clones campaign-trail" has no cm command here. `rpi/cm-in-vm` owns it. If it needs a `cm tick` flag, that is a skeleton change and a change to the runner's command line.
- **G6. kubectl inputs (11-D20).** 11-D20 fixes the four command forms, not the effect inputs. The shapes in S-tick-12 are this skeleton's reading, and `11-rpi/k8s-provider` confirms them or forces a skeleton change.
- **G7. Update-branch against exact-head binding.** The tick binds a verdict only at the PR's current head (PLAN.md `07-tick/gate-merge` (1)). `execution.md` §Branches and main has the coordinator update every open PR after each merge, which moves its head. So under the tick, each update-branch restarts that PR's review. `verdict::clean_update`'s path stays for `cm advance` and `cm sync`. Whether update-branch changes in the tick era is the coordinator's call.
- **G8. Dashboard facts (`tick/agent-ledger`).** "UNITS.json retires" needs title, wave, size and ideas, which cm does not hold. The export carries cm's state and edges only.
- **G9. Who records `pr-open` on the RPi.** Intake watches the steps cm has at `pr-open`. Writer lanes have no campaign-trail write, and no unit's done check records `pr-open` from the RPi. In the interim the coordinator records it as today.

## Checks

These are filled in by the coordinator's `check` step, not by this writer.

**Walks.**

| walk | report | open gaps |
|---|---|---|

**Adversarial read.** Pending.

**Adjudicator.** Pending.

**Fork bake-off.** None. The 11-D3 wire, the provider schemas and PLAN.md fix the shapes; no real fork was found.

## Landing

- **Train.** `footprint-edges.py` on this file, campaign-trail `5b0ae28d`, default and `--strict` alike:
  - `# train 1: tick/core -> tick/gate-merge -> tick/release-lan`
  - `cm amend 07-tick/gate-merge/build --after pr-open:07-tick/core/build`
  - `cm amend 07-tick/release-lan/build --after pr-open:07-tick/gate-merge/build`
  - `# parallel: tick/agent-ledger tick/ledger`

  The only hot file is `src/tick/run.rs`. `tick/ledger` and `tick/agent-ledger` share no file with anyone. Their order comes from UNITS.json `depends_on`: both follow `tick/ledger`, and `tick/agent-ledger` also follows `mac/cloud-usage-probe`. This matches PLAN §Shared surfaces ("the cm repo: a chain") for the train, and frees agent-ledger, a Codex unit, to run beside it.
- **Merges.** None. No pair shares three or more hot files: core with gate-merge share `run.rs`, and so do gate-merge with release-lan.
- **Relax step.** `07-tick/tick/relax`, after `tick/release-lan`'s live check. It is expected to:
  - remove any `#![allow(unused_variables)]` a unit left;
  - fold `forge::ci_green` into `authority::checks_green` if `cm sync` can take the stricter rule;
  - drop `LocalApply` and the interim packet actions once the RPi cutover leaves no cloud applier.

## Choices I made

- **C1. Units.** The five starting units are kept, none is added and none dropped. `rpi/cm-in-vm` and `05-integration/advisory-locks` are left out because neither adds cm source (Header).
- **C2. The cm side of 11-D3 goes in `tick/ledger`.** That covers `--effects`, `cm effects record`, the bounds and the batch push, as in the draft, and adds three things: `cm effects resolve` (the ruling rows 11-D22 names), the local applier, and `admit`. No UNITS.json done check names any of them, so `tests/tick_ledger.rs` carries three extra stubs. **Coordinator:** confirm this, or amend `tick/ledger`'s done check. It is not a plan conflict.
- **C3. One cycle, two appliers.** The plan has `cm tick` merging, cutting and pinning in the cloud interim and the rehearsal, before any dekopon runner exists. So:
  - `cm tick` applies locally, in the runner's order and with its stop rule. It applies a REST merge and a cut dispatch itself, through the session's `gh`. It hands every other effect to the coordinator as a packet action:
    - the enqueue, because GraphQL is 403;
    - the pins, because the coordinator is the homelab writer;
    - kubectl, through the LAN-leg runbook.
  - `cm tick --effects` prints the effects for the runner.

  Both write the same ledger, so one reconcile serves the interim and the RPi.
- **C4. The provider's schemas are the wire.**
  - `MergeInput` carries `mergeMethod`, which provider-gh requires (no default).
  - `gh.workflow.run` is `{owner, repo, workflow, ref, inputs: [{name, value}]}`.

  The draft's shapes, a merge without `mergeMethod` and a workflow input with `workflow_id` and an inputs object, would have been refused under `deny_unknown_fields` on every runner-applied merge and cut. `MERGE_METHOD` is `squash`, `merge-pr.py`'s default.
- **C5. Where the ledger lives, and what a tick commits.**
  - The ledger is `.cm/tick/effects.jsonl`, with the cursor, timers and seen set beside it.
  - campaign-trail's top-level `.gitignore` ignores `*.jsonl`, so the tick force-adds exactly its own files.
  - The other files that go into a tick's commit are what changed under the campaign during the tick: `cm sync`, `cm launch`, `cm attempt` and the Astra out-dir.
  - Without force, ignored transcripts such as `session.jsonl` stay out, and a file another writer left dirty earlier is not swept in.
- **C6. Locks.** One non-waiting tick lock covers `.cm/tick/`. The tick never holds `store::lock` across an existing cm call or a child process. `astra-review.sh collect` runs `cm review record`, and the flock is per open file, so holding it would deadlock the tick against itself.
- **C7. Step launches reconcile through the existing attempt record.** The intent row names the attempt, and `observe_step` reads `attempt::plan`. Two records of one launch cannot disagree.
- **C8. Errors.** An error means durable state could not be written. Everything per item is data. A forge outage leaves intents open and PRs `unread`; it never parks them. With `--effects`, a tick that fails still prints one line with a `tick-aborted` item and exits 0, so the runner can deliver it. Only a pre-tick failure exits non-zero with no stdout.
- **C9. Readiness.** Readiness is always a ledger receipt of `kubectl.get.readyz`: the runner's on the RPi, the LAN-leg runbook's (through `cm effects record`) in the interim. "One heartbeat" is the previous tick (`Cursor::last_tick`). This replaces the draft's 60-minute constant and needs no heartbeat fact.
- **C10. The tick binds a verdict only at the current head.** It has no checkout of the PR's repository, so `verdict::clean_update` stays a `cm advance` and `cm sync` path (G7).
- **C11. The drop cap check is called from `merge::route` into `release::drop_admits`.** This keeps UNITS.json's assignment of the cap to `tick/release-lan`. Between gate-merge's and release-lan's landings, a core `dekopon` merge through the tick would hit `todo!()` before writing any intent. That is loud and safe, and not reachable in production: the tick drives no real repository before the interim cutover.
- **C12. agent-ledger.**
  - The row is LEDGER.md's exactly, with one row per invocation, updated in place by its `invocation` key. The draft wrote three rows per invocation, against LEDGER.md's "One row per invocation", and used field names LEDGER.md does not have.
  - The CLI calls the agent-ledger functions only when their flags are given, so this unit touches no existing file and leaves the train.
  - Harvest is `cm attempt end` with `--tokens-*`.
- **C13. Bounds the plan does not fix.** These are mine, and walk 3 should test them:
  - per tick: 16 effects, 8 launches, 120 reads and 200 events, a packet of 20 items or 16 KiB;
  - durable: 64 open intents, 256 timers, 1,024 seen keys;
  - five tries;
  - a 6 h reminder, a 90 min review deadline, a 300 s launch timeout.

  The wire bounds (8 KiB, 20 KiB, 64 KiB) are 11-D3's.
- **C14. The nonce is 32 hex digits of SHA-256 over the ledger path and the intent id.** It is unique in the ledger with no RNG, which `Host` lacks. It is a correlation value, not a secret (IDEA-0063).
- **C15. `cm sync`'s gate applies the phase proof to every reviewed lane.** Today `gate_holds` gates codex steps only. My reading of "`cm advance` and `cm sync` take the verdict proof LIMITS names for the phase" covers Claude-written steps too, since LIMITS names a proof per writer. That can change existing gate tests, so `tick/gate-merge`'s footprint includes `tests/verdict.rs`, `tests/lifecycle.rs` and `tests/support/verdict.rs`. No other unit touches them.
- **C16. Launch routes.**
  - A GPT review is `astra-review.sh` until gpt-route.
  - A Claude session (a review, or a claude-cloud step) is a packet action for the coordinator's `create_session` when cm applies locally (F1), and one MAC-LANE launcher item on the RPi.
  - Codex, coordinator, fable and pi steps are packet actions, since cm holds no capability for them.
  - Owner steps are never dispatched.
- **C17. Packet delivery is at most once per delta.** Seen keys go into the first commit. Persisting states come back through reminder timers instead of repeating every tick.
- **C18. Intake polls only the PRs cm tracks, with reads 11-D10 already opens.** It reads no PR list (G9).
- **C19. Granularity and ids.** One item per type cluster with its methods: 7 skeleton-owned items (S-tick-01 to 06, and 17), 40 unit items, 72 lifecycle stubs and 30 done-check stubs. Ids are in blocks per unit: 10s ledger, 30s core, 50s gate-merge, 60s release-lan, 70s agent-ledger, 100s lifecycle, 200s done checks. Nothing cites the draft's ids yet, so they are not kept.
- **C20. Ranks.** ledger 1, core 2, agent-ledger 2, gate-merge 3, release-lan 4. Within the one train, rank orders core, then gate-merge, then release-lan, which is also the `depends_on` chain.
- **C21. Base.** Main (`0e38f02`) is merged into the existing branch. Nothing is rebased or force-pushed, so PR #114 keeps its history.

## Changes from the medium draft

For the check step. Each of these is a defect the draft had, or a gap a walk would have hit.
- **Wire (C4).** Provider-gh field names. Typed `EffectInput`. `admit` before any intent row, so stdout is bounded by construction (the draft "refused past 64 KiB" after intents were pushed). A frozen exit-code contract for the runner. The `truncated` error is dropped, since cm cannot see vm-runner's mark.
- **Ledger.**
  - Path and force-add (C5). The draft's `.cm/effects.jsonl` matched `*.jsonl` and would never have been committed.
  - `verdict`, `ruled` and `handed` rows and states, `Unread`, and torn-line handling.
  - Step launches go through the attempt record (C7).
- **Process.** The tick lock and the deadlock rule (C6). The two-commit cycle with the commit set (C5). A pull at tick start that aborts on conflict. The packet path for `cm sync` violations.
- **Appliers (C3).** The interim had no applier in the draft.
- **Errors (C8).** The draft's `From<ForgeError>` let a single read failure abort a tick. Intake is bounded and round-robin.
- **Reads.** Each unit's new GitHub reads are its own trait in its own file (`PullReads`, `MergeReads`, `ReleaseReads`, `LanReads`), on the `Gh::json` seam. The draft's `branch_kind(forge: &dyn Forge, …)` needed a `Forge` method that does not exist.
- **Review proof (S-tick-32).** All three proof rules, by phase and repository. The draft had two variants.
- **Gate refusals.** The grammar's refusals are added (no nonce, short head, no word, word disagreeing), along with the review budget.
- **agent-ledger (C12).** The schema, one row per invocation, and no train membership.
- **LAN.** Readiness through the ledger (C9), and the pin hold after a merge mismatch.
