# Area skeleton: tick

This skeleton fixes the structure of `cm tick` before any tick code unit gets a kickoff. Workers fill it in and never invent the structure. Template: `skills/campaign-trail/templates/skeleton.md`.

This is the round-3 repair of the xhigh revision, the B1-path last pass. Round 1's check returned FIX REQUIRED (`06-designs/skeleton-tick/check/evidence/verdict.md`, campaign-trail `04babe4f`): three walks, a Fable read, and adjudicator rows R01 to R26 with follow-ups U01 to U12. §Round 2 repairs maps each of those rows to its change. Round 2's check also returned FIX REQUIRED, with three blocking items in this skeleton's scope, F1 to F3, and two cheap follow-ups, U13 and U19. §Round 3 repairs maps those five. The coordinator handles F4 (the harvesters' lock) and the F15 plan edge itself. Each repair is the smallest one that closes its row: no new capability, and no unit beyond the split R21 asks for. §Changes from the medium draft is round 1's history and is unchanged.

**Header**
- Area: `tick`, meaning `cm tick` in the cm repo: intake, timers, dispatch, the decision packet, the effect ledger, the runner wire, the verdict gate, releases and the LAN leg.
- Repository and base: `dekopon-agents/cm` at `0e38f02` (main, "chore: update rustix lock for RUSTSEC-2026-0486 (#112)"), unchanged since round 1.
- Skeleton PR: `dekopon-agents/cm#114`, code head `93ddb5e93467f468977c6b195ed2abf6f7bc0faa`, branch `design/skeleton-tick`. Round 1's code head was `cd7865a8` and round 2's was `a5e4e269`. Round 3 is one commit on top of `166af407`, with no force-push.
- Units covered: `tick/ledger`, `tick/wire`, `tick/core`, `tick/agent-ledger`, `tick/gate-merge`, `tick/release-lan`.
  - **Consumer whose interface this skeleton must not break:** `rpi/tick-runner` (dekopon). It reaches cm only through 11-D3's two command lines and their JSON. cm now uses the runner skeleton's own shapes (dekopon#983 at `63e2ab53`), and both repositories parse one conformance fixture, `tests/fixtures/tick-wire/` (R01).
  - **Kept:** all five starting units.
  - **Dropped:** none.
  - **Added:** `tick/wire`, split out of `tick/ledger` (R21). It covers `--effects`, `cm effects record`, admission, the conformance fixture and the interim local applier. **Coordinator:** add it to UNITS.json and PLAN.md with the done check proposed in §Proposed done checks, and add `tick/wire` to `tick/core`'s `depends_on`.
  - **Left out, with reasons:**
    - `rpi/cm-in-vm` (`repo: vm-runner (guest image), cm`). It changes the guest image. 11-D7's clone hand-in is now a cm flag, `cm tick --clone <url>`, registered here (G5, R25). The image's use of that flag stays in `rpi/cm-in-vm`.
    - `05-integration/advisory-locks` touches cm only through `Cargo.lock`.
    - `tick/review-shadow` and `tick/scratch-ci` are code in campaign-trail and gh-parity-scratch, not cm.
    - `tick/rehearsal`, `interim-cutover`, `overnight-trial`, `rpi-cutover` and `gpt62-build-study` are studies, configs or cutovers. They write no cm code.
- Steering read: campaign-trail main `04babe4f` (round-1 evidence) and `10667b29` (round-2 evidence: `walk-1-r2.md`, `walk-3-r2.md` and `fable-r2.md`, read for W1-16, W1-17, N3-1, N3-2, N3-3, N8 and N12). The round-2 adjudicator's F1 to F3 reached me in the coordinator's brief. Also read:
  - dekopon#983 at `63e2ab53`: `crates/dekopon-gatewayd/src/runner.rs` and `runner/receipt.rs`;
  - cm `.github/workflows/cut-release.yml` at `0e38f02`;
  - provider-gh `5b1616e`;
  - campaign-trail `.gitignore`, `.gitattributes`, `tools/agent-ledger/LEDGER.md` and `record.py`.
- Effort receipt: `printenv CLAUDE_CODE_EFFORT_LEVEL` printed `xhigh`.

## Gate

The skeleton PR passes these, run at code head `93ddb5e93467f468977c6b195ed2abf6f7bc0faa` with Rust 1.98.1:
- `cargo fmt --all --check`
- `cargo check --all-targets --locked`
- `cargo clippy --all-targets --locked -- -D warnings`
- `cargo test --locked`: 429 passed, 0 failed, 139 ignored. The 139 are the skeleton stubs: 100 lifecycle and 39 done-check.

**One flaky existing test.** `released_without_a_checkout_reads_tag_containment_from_the_forge` (`tests/verdict.rs:384`) failed its "forge read held the campaign lock" assertion in two full runs in round 2. It passed in three reruns of `--test verdict` and in every final run. In round 3's three full runs it passed. The skeleton changes neither `commands::advance_gated` nor `tests/verdict.rs`. The likely cause is a race in the test: another test thread forks a `git` child before `exec`, and that child briefly shares the closed lock file's description, so the flock outlives the close. It is recorded here and not fixed, because fixing it is new scope.

cm has no AGENTS.md, no `clippy.toml` and no `disallowed-methods` concurrency lints. Its lints are clippy's defaults under `-D warnings`, as CI runs them (`.github/workflows/ci.yml`, job `fmt, clippy, test`).

Every body a unit writes is `todo!("<unit>")`. Each skeleton file opens with `#![allow(unused_variables, reason = "skeleton: <unit> fills these bodies")]`, which the unit deletes once it fills the file. `EffectLedger`'s private fields carry `#[expect(dead_code)]`. Each lifecycle and done-check stub is `#[ignore = "skeleton: <unit>"]`.

The skeleton writes bodies only for glue and seams. No unit edits them:
- `cli.rs`:
  - the `tick`, `effects record`, `effects resolve` and `export` arms;
  - `--approved` on `sync` and `advance`: `approved_context`, then `sync_proved` or `advance_proved`, and the unchanged path without the flag (R07);
  - `--clone` on `tick` (R25);
  - the flag-gated agent-ledger calls on `launch`, `attempt record`, `attempt end` and `review record`;
  - a failed agent write after `cm launch` keeps the launch's outcome in its error (U12);
  - `full_sha`;
  - the `--json` guard on `tick --effects`.
- `SystemGit::default`.
- The three routing matches, which send each intent kind or capability to the module of the unit that owns it:
  - `SystemObserver::observe` (S-tick-17);
  - `ledger::readmit` (S-tick-25);
  - `LocalApply::apply` (S-tick-24).
- `forge.rs`: `run`, `Gh::json` and `Gh::pages` become `pub(crate)`.
- `verdict.rs`: `Block::nonce`, always `None` until `tick/gate-merge` parses it.
- `commands::advance_proved` and `sync::sync_proved` are new seams with `todo!("tick/gate-merge")` bodies. They are reached only with `--approved`.

Existing commands behave exactly as before. Without `--approved` and the new agent flags, every existing arm runs the code it ran at `0e38f02`. The full existing suite passes.

## Module tree

- `Cargo.toml`: `thiserror = "2"`, which was already in `Cargo.lock` as a transitive dependency.
- `src/lib.rs`: `pub mod tick;`.
- `src/cli.rs` (S-tick-02), registration only:
  - `Commands::Tick { --approved <40-hex>, --effects, --clone <url> }`;
  - `Commands::Sync { --approved <40-hex> }`, and `--approved` on `Commands::Advance`;
  - `Commands::Effects { Record, Resolve { <intent>, --as done|retry|drop, --evidence, --reason } }`;
  - `Commands::Export`;
  - `Requested` flattened into `Launch`, and `Echo` flattened into `AttemptAct::Record`, `AttemptAct::End` and `ReviewAct::Record`.
- `src/forge.rs`, `src/verdict.rs`: the S-tick-04 seams.
- `src/commands.rs`, `src/sync.rs`: the S-tick-52 seams `advance_proved` and `sync_proved`.
- `src/tick/mod.rs` (S-tick-03): registration, the `pub use` list, `tick::Result` and `TICK_DIR = ".cm/tick"`.
- `src/tick/error.rs`: `TickError`, the area's one error enum (S-tick-01).
- `src/tick/ledger.rs`: the segmented effect ledger, its fold and budget, nonces, reconcile, the observer and readmit routing, and the tick lock (`tick/ledger`).
- `src/tick/push.rs`: `Git`, `SystemGit`, the clone hand-in, `begin` (union check, recovery commit, pull), the owned-checkout rebuild, and the commit-and-push with three tries (`tick/ledger`).
- `src/tick/resolve.rs`: `cm effects resolve` (`tick/ledger`).
- `src/tick/wire.rs`: the 11-D3 wire contract in the runner's shapes, its typed provider inputs, its byte bounds and admission (`tick/wire`).
- `src/tick/record.rs`: `cm effects record` (`tick/wire`).
- `src/tick/apply.rs`: `Applier`, `Apply`, the local applier for the cloud interim, and the in-order stop rule (`tick/wire`).
- `src/tick/authority.rs`: `[authority]`, `[gates]` and `[review]` read at the approved SHA, the release mode, `paused` read live, and the §Gates check rule (`tick/core`).
- `src/tick/timers.rs`: durable timers: retry, collect, review deadline, reminder and action deadline (`tick/core`).
- `src/tick/intake.rs`: bounded, round-robin polling of the PRs cm tracks, and the cursor with its handled marks, requests, tick number and receipt cursor (`tick/core`).
- `src/tick/dispatch.rs`: launch routes, level-triggered review debt, the launch plan before the push, the launches after it, retry readmission and the launch lookups (`tick/core`).
- `src/tick/packet.rs`: the bounded decision packet, its rendered text and the seen set (`tick/core`).
- `src/tick/run.rs`: the stage order, `Deps`, `Cycle`, `tick` and the CLI seam `cli_tick` (`tick/core`). `tick/gate-merge` changes two lines in it: it swaps stage 4's `sync::sync` for `sync::sync_proved` and adds stage 6. `tick/release-lan` adds the two stage-7 lines.
- `src/tick/gate.rs`: the verdict gate and the approved context (`tick/gate-merge`).
- `src/tick/merge.rs`: the exact-head merge for the PR's own base branch, the post-merge confirmation and the pin hold (`tick/gate-merge`).
- `src/tick/release.rs`: release cuts, dispatch evidence, drop admission and the tally reservation (`tick/release-lan`).
- `src/tick/lan.rs`: pins from a rollout, readiness by tick number, the LAN leg and the probe (`tick/release-lan`).
- `src/tick/agents.rs`: `AGENTS.jsonl` rows in LEDGER.md's schema, invocation keys, and the tick's recorder (`tick/agent-ledger`).
- `src/tick/export.rs`: the `cm export --json` graph with plan ids (`tick/agent-ledger`).
- `tests/tick_{ledger,wire,core,gate_merge,release_lan,agent_ledger}.rs`: one done-check stub file per unit. Each unit also owns `tests/fixtures/tick-<unit>/`.
- `tests/fixtures/tick-wire/`: `tick-plan.json`, `receipts.json` and `README.md`, the conformance fixture shared with dekopon. Those three files are frozen: a change to them is a wire change in both repositories (S-tick-10). `tick/wire` may add files beside them.
- Existing files a unit changes, all of them `tick/gate-merge`'s:
  - `src/verdict.rs`: parse `nonce:`; `merge_gaps` takes the phase's `ProofRule`; the third-FIX-REQUIRED ruling text;
  - `src/commands.rs`: fills `advance_proved`;
  - `src/sync.rs`: fills `sync_proved`, whose `gate_holds` covers every reviewed lane;
  - the existing gate tests that change with it.

## Items

There are no `async` items. cm is a synchronous, one-shot process with no runtime, and none is added: no value crosses a thread except `forge::run`'s two pipe threads per child. Every outward call is a child process with a timeout. The `task` rows say who owns each one, its bound, its full-queue policy and its shutdown path.

| id | item | kind | file | notes |
|---|---|---|---|---|
| S-tick-01 | `pub enum TickError`: `Io`, `Row`, `Child`, `Cm`, `ApprovedUnreachable`, `AuthorityMissing`, `PullConflict`, `PullBlocked`, `LedgerNotUnion`, `PushExhausted`, `BatchTooLarge`, `BatchInvalid`, `UnknownIntent`, `ReceiptConflict`, `LedgerFull`, `TimersFull` | error | `src/tick/error.rs` | The one `thiserror` enum. Skeleton-owned: a new variant is a skeleton change. Error policy: an error is only a failure to write durable state. Anything about one PR, step or effect is data (a refusal, a hold, an `unread` read), and the tick goes on. There is no blanket `From<ForgeError>`, so a forge read cannot abort a tick through `?`. Crosses into `anyhow` only at `cli.rs`. |
| S-tick-02 | CLI registration and glue: the four new arms, `--approved` on `sync` and `advance`, `--clone` on `tick`, `Requested`/`Echo` flattening, `full_sha`, the `--json` guard on `tick --effects` | fn | `src/cli.rs` | Skeleton-owned. Units never edit `cli.rs`. Without a new flag, every existing arm runs its old code. |
| S-tick-03 | `pub mod` list, `pub use` list, `tick::Result`, `TICK_DIR` | type | `src/tick/mod.rs` | Skeleton-owned. Durable tick files under `.cm/tick/`: the ledger segments `effects-NNNN.jsonl`, `cursor.json`, `timers.json`, `seen.json` and `lock`. |
| S-tick-04 | seams: `forge::run`, `Gh::json` and `Gh::pages` made `pub(crate)`; `verdict::Block::nonce: Option<String>` | fn | `src/forge.rs`, `src/verdict.rs` | Skeleton-owned. Every tick child runs through `forge::run`, and every tick read through `Gh::json`. No second child runner. |
| S-tick-05 | concurrency and locks | task | `src/tick/ledger.rs` (`TickLock`) | **Owner:** each `cm` process. `TickLock` (`.cm/tick/lock`, `flock` `LOCK_EX`\|`LOCK_NB`) serializes every writer of `.cm/tick/`: `cm tick`, `cm effects record` and `cm effects resolve`. **Bound:** one holder. **Full queue:** a second holder is refused at once, never queued. A second tick exits 0 with no effects. **Shutdown:** the lock dies with the process. **Readers (R08):** a proof reader takes `EffectLedger::snapshot`, which never writes and needs no lock, so `cm advance`, `cm sync` and stage 4 inside a locked tick all read safely. **Runner (R06):** the lock covers one compute or one record, not a whole cycle. The runner runs one cycle at a time per entry (`rpi/tick-runner`), and cm parks a non-idempotent emission whose completion it cannot see. **Campaign lock:** the tick never holds `store::lock` across an existing cm call or a child process, because existing commands take it themselves and `astra-review.sh collect` runs `cm review record`. The `AGENTS.jsonl` rewrite takes it only for its own read, change and write (S-tick-72, R16). |
| S-tick-06 | full-queue policy and growth bounds | task | the modules below | Per tick: 16 effects within 64 KiB of stdout (`wire::admit`, before any intent row), 8 launch intents, 120 reads and 200 events, and a packet of 20 items or 16 KiB. Durable: 64 open intents, 256 timers, 1,024 seen keys, a cursor bounded by the open PRs, and at most `MAX_ROWS_PER_INTENT` (38) ledger rows per intent in 512 KiB segments (R19). Past a per-tick bound, work waits for the next tick: it is counted, with no packet item. Past a durable bound, the planning stage writes no new intent and puts one `ledger-full` item. Completion rows are never refused (S-tick-14). |
| S-tick-10 | `EffectsOut { effects, packet: Option<String> }`, `EffectsOut::to_line`; `OUTPUT_INLINE_MAX_BYTES` 8 KiB, `BATCH_MAX_BYTES` 20 KiB, `TICK_STDOUT_MAX_BYTES` 64 KiB, `MAX_EFFECTS_PER_TICK` 16 | type | `src/tick/wire.rs` | The runner's `TickPlan`, field for field, `deny_unknown_fields` (R01). `packet` is the rendered text the runner posts verbatim, or absent. The command lines are frozen in the module doc. `--effects`: exit 0 means exactly one line on stdout, and every intent row is already pushed. A tick that started and then failed still exits 0, with a `tick-aborted` item. A non-zero exit prints nothing. `tests/fixtures/tick-wire/` is the shared conformance fixture, and both its files re-serialize byte for byte: `input` is a `serde_json::Value`, which prints keys sorted, so the fixture's inputs have their keys sorted (U13). |
| S-tick-11 | `Capability` (11-D19's nine ids, serde-renamed), `EffectInput`, `Effect { intent, capability, input }`; `Capability::id`, `EffectInput::capability`, `Effect::new` | type | `src/tick/wire.rs` | `Effect` is the runner's, `deny_unknown_fields`. A typed input fixes its capability, so a mismatched pair cannot be built. A new capability is a skeleton change and a grant change. |
| S-tick-12 | `MergeInput { owner, repo, number, mergeMethod, expectedHeadSha }`, `MergeMethod`, `MERGE_METHOD`, `EnqueueInput`, `WorkflowRunInput { owner, repo, workflow: u64, ref, inputs: [WorkflowField] }`, `PinUpdateInput`, `PinValue`, `PinMergeInput`, `ApplicationInput`, `DeploymentInput`, `ReadyzInput` | type | `src/tick/wire.rs` | `deny_unknown_fields`, camelCase where the provider uses it. Merge and workflow inputs follow provider-gh `5b1616e`. Enqueue and pins follow 11-D15, 11-D17 and 11-D18. kubectl inputs are this skeleton's reading of 11-D20 (G6). |
| S-tick-13 | `Receipt { intent, capability, outcome, output }`, `ReceiptOutcome` (`ok`, `refused`, `failed`), `ReceiptOutput` (`Inline(String)`, `Oversize`), `Oversize { oversize, bytes, sha256 }`, `ReceiptOutput::of`; `Admitted`, `admit` | type | `src/tick/wire.rs` | The runner's `Receipt`, field for field (R01). `ok` and `refused` close an intent, and `refused` also puts one `effect-refused` item (R17). `failed` leaves it open for reconcile. One sizing rule (U07): an output of 8 KiB or more, or one that is not UTF-8, is oversize, its length and SHA-256, never truncated. The producer decides, and `cm effects record` checks again. `admit` sizes each effect with the longest intent id. |
| S-tick-14 | `IntentId`, `Nonce`, `Family` (`writer_of`, `other`), `IntentKind` (`step-launch`, `review-launch`, `effect`), `IntentRow` (with `cycle`, `requested_by`), `Draft`, `Bind`, `Ruling`, `VerdictRow`, `VerdictProof`, `LedgerRow` (`intent`, `emitted`, `bound`, `parked`, `receipt`, `ruled`, `verdict`), `IntentState` (`Open`, `Handed`, `Bound`, `Parked`, `Received`, `Closed`), `Intended`, `Recorded`; `LEDGER_DIR`, `SEGMENT_PREFIX`, `SEGMENT_MAX_BYTES` 512 KiB, `MAX_OPEN_INTENTS` 64, `MAX_RULINGS_PER_INTENT` 2, `MAX_ROWS_PER_INTENT`, `INTENT_ID_MAX_LEN` 20 | type | `src/tick/ledger.rs` | Append-only JSON Lines in segments, one tagged row each, never rewritten. An intent row carries the done check's fields (PR, head, nonce, model, family), the effect `input` and the tick number it was written under. The fold is in the module doc (U06): a late receipt closes a parked or handed intent; a different receipt on a closed intent, or one for another capability, is refused. Budget (R19): at most `MAX_TRIES` emissions between rulings and two rulings, then only `done` or `drop`. The open-intent cap gates new intents only. |
| S-tick-15 | `EffectLedger`: `snapshot`, `open(root, &TickLock)`, `rows`, `active_segment`, `state`, `open_intents`, `parked`, `intent`, `last_emitted`, `intend`, `emitted`, `bind`, `park`, `record`, `rule`, `verdict`, `mint_nonce`, `by_nonce`, `nonce_consumed`, `receipt`, `receipts_from`, `verdicts_at` | type | `src/tick/ledger.rs` | `snapshot` is read-only: it ignores a torn last line in memory (R08). `open` needs the lock, truncates a torn last line and is the only handle that appends. Each append is one `write` of one whole line, then fsync. Two clones' appends merge by `merge=union` (R03). `intend` refuses past 64 open intents and answers `AlreadyOpen` for a second intent of the same purpose. `record` is idempotent by intent. The nonce is 32 hex digits derived from the intent id. |
| S-tick-16 | `Observer`, `Observation` (`Done`, `NotDone`, `Undecidable`, `Unread`), `Reconciled`, `reconcile` | trait | `src/tick/ledger.rs` | `Done` binds; `Undecidable` parks with a packet item; `NotDone` goes to readmission (S-tick-25), counted by timers; `Unread` (a failed lookup) leaves the intent open and parks nothing. A `Handed` intent is looked up for done, never re-emitted, and parks at its action deadline (R18). The conservative rule (R06): an effect that is not idempotent, emitted by an earlier cycle and with no receipt, parks unless positive evidence binds it. Absence never reads as not done. |
| S-tick-17 | `SystemObserver` and its routing `impl Observer` | fn | `src/tick/ledger.rs` | Skeleton-owned glue. Routes step launches to `dispatch::observe_step`, review launches to `dispatch::observe_review`, merges and enqueues to `merge::observe`, `gh.workflow.run` to `release::observe`, and pins and kubectl to `lan::observe`. |
| S-tick-18 | `TickLock::try_acquire`, `TICK_LOCK` | type | `src/tick/ledger.rs` | Filled by `tick/ledger`; the rule is S-tick-05. |
| S-tick-19 | `SystemGit { program, timeout }` | task | `src/tick/push.rs` | **Owner:** the tick's thread; one child at a time through `forge::run`. **Bound:** `GIT_TIMEOUT` 60 s, killed on expiry. **Queue:** none. **Shutdown:** an unpushed commit stays local, and the next tick's `begin` pushes it before anything else. |
| S-tick-20 | `Git` (`clone_into`, `toplevel`, `merge_attr`, `pull_no_rebase`, `abort_merge`, `head`, `dirty`, `commit`, `push_main`, `unpushed`, `reset_to_origin`, `restore`, `show`, `has_commit`, `fetch_sha`), `PullResult` (`Merged`, `Blocked`, `Conflict`), `PushResult`, `Pushed`, `TickPhase` (`decide`, `record`), `PendingMark` (`tick`, `base`, `phase`, `paths`; `load`, `save`, `clear`), `cm_owned`, `ensure_checkout`, `begin`, `discard`, `rebuild`, `commit_and_push`; `PUSH_TRIES` 3, `OWNED_MARK`, `PENDING_MARK` | trait | `src/tick/push.rs` | 11-D7 and 11-D8. **Commit set (R02):** the tick's own `.cm/tick/` files with `--force`, every dirty cm-owned path (`cm_owned`), and the paths in `Cycle::wrote` without `--force`. A path no cm command writes is never committed. **Pending mark (F1):** a tick's decisions are pushed whole with its first commit or discarded whole. The mark in `.git/cm-tick-pending` holds the tick, the `base` commit `begin` left and the phase, and lists each `Cycle::wrote` path before its write. **`begin`** runs in this order: abort a leftover merge; refuse without the union attribute (`LedgerNotUnion`); recover by the mark, which is then cleared: `decide` with an unpushed first commit is `discard`ed (branch back to `base`, cm-owned and marked paths restored, `.cm/tick/` cleaned), while `record`, or no mark, commits the facts (dirty cm-owned paths, marked paths, `.cm/tick/` forced); push any unpushed commit; pull. **Conflicts (F2):** `commit_and_push` keeps its commit local on a conflicting or blocked inner pull and returns `PullConflict` or `PullBlocked`; it never rebuilds. At `begin` (steps 3 and 4) a checkout cm cloned itself (`OWNED_MARK`) is rebuilt, and any other checkout returns the error. At stage 9 the tick discards and stops; at stage 11, and in `cm effects record` and `resolve`, the commit stays local for the next `begin`. **`rebuild` (R03, F3):** keeps the ledger lines and the `AGENTS.jsonl` lines main lacks, resets to `origin/main`, appends them again, commits and pushes. **`ensure_checkout`** clones when the checkout is absent (`--clone`, R25). Never `--force` on a push, never another ref. |
| S-tick-21 | `record`, `parse_batch`, `record_receipts` | fn | `src/tick/record.rs` | `cm effects record`: the batch is taken whole or not at all, every receipt is checked against the fold before any is appended, receipts are written under the cursor's tick number, and the batch is committed and pushed before returning. The local applier records through `record_receipts`. |
| S-tick-22 | `resolve` | fn | `src/tick/resolve.rs` | `cm effects resolve <intent> --as done\|retry\|drop`: the ruling row 11-D22 names. Refused on an intent that is not parked, and for a `retry` past the ruling budget. A `retry` reopens the intent under its own id, and the next tick readmits it (S-tick-25). |
| S-tick-23 | `Applier` (`Runner`, `Local`), `Applied`, `Apply`, `AppliedRun`, `apply_in_order` | trait | `src/tick/apply.rs` | One cycle, two appliers (C3). The local run keeps 11-D3 (2)'s order and stops at the first refusal, failure or deferral (U08). The deferred effect and everything after it stay open. |
| S-tick-24 | `LocalApply` and its routing `impl Apply` (glue); `defer` | fn | `src/tick/apply.rs` | The glue routes merge to `merge::apply_local`, `gh.workflow.run` to `release::apply_local`, and the rest to `defer`. `defer` (filled by `tick/wire`) builds the packet action, including gate-merge's REST head bracket for an enqueue and the `cm effects record` line that closes it. |
| S-tick-25 | `Readmit` (`Emit(EffectInput)`, `Relaunch`, `Refuse { reason }`), `ledger::readmit` routing glue | fn | `src/tick/ledger.rs` | R05. Skeleton-owned glue: it routes a retry by kind and capability to `dispatch::readmit`, `merge::readmit`, `release::readmit` or `lan::readmit`. The owner re-checks `paused`, `[authority]` at the approved SHA, its gates and its input. `Emit` and `Relaunch` keep the intent id (`Cycle::replan`), and `Emit` still passes `wire::admit`. `Refuse` parks. Stored ledger JSON never authorizes an act. |
| S-tick-30 | `ApprovedSha`, `Bump`, `Authority` (`canonical`, `may_merge`, `may_release`) | type | `src/tick/authority.rs` | 40 lower-case hex only. The rename rule is applied on both sides of every lookup. `Major` is never authorized. |
| S-tick-31 | `Gates` (`required`), `ChecksRead`, `checks_green`, `CLASSIFIER_CHECK` | fn | `src/tick/authority.rs` | §Gates: every named check present, every check `success`, `skipped` or `neutral`, at least one `success` besides `classify CI changes`. Zero checks is red, and a repository with no entry is red. `forge::ci_green` stays the `cm sync` rule. |
| S-tick-32 | `Phase`, `ProofRule`, `WriterProof`, `PhaseTable`, `ReviewTable` (`phase_for`, `proof`) | type | `src/tick/authority.rs` | §Lanes "Review proof per phase", read at the approved SHA. The phase comes from cm state (`starts_after` steps done; `installed` evidence), never from LIMITS live. |
| S-tick-33 | `Approved` (`read`, `release_mode`), `ReleaseMode` (`Real`, `DryRun`), `paused_live` | fn | `src/tick/authority.rs` | One `git show <sha>:<campaign>/LIMITS.toml` yields all three tables, with one fetch if the SHA is absent. A missing table is `AuthorityMissing`: apply nothing, park. A working-tree LIMITS cm cannot read counts as paused. `release_mode` is `[authority] release_mode = "dry-run"` at the approved SHA only, absent meaning `Real` (U19). That is not an invented default: a real cut is what `release_repos` and `release_bumps` at that SHA already authorize. The key is not yet in EXECUTION.md's `[authority]` (R10, G10). |
| S-tick-34 | `TimerKind` (`retry`, `collect`, `review-deadline`, `reminder`, `action-deadline`), `Timer`, `Timers` (`load`, `save`, `arm`, `disarm`, `take_due`), `backoff`; `MAX_TIMERS` 256, `MAX_TRIES` 5, `REMINDER_INTERVAL` 6 h, `REVIEW_DEADLINE` 90 min, `ACTION_DEADLINE` 2 h, `BACKOFF_BASE` 5 min, `BACKOFF_MAX` 1 h | type | `src/tick/timers.rs` | Arming is idempotent by key. Saved in the tick's first commit. Backoff doubles from `BACKOFF_BASE` up to `BACKOFF_MAX` (U05). A handed intent past `action-deadline` is parked and its reminder armed (R18). |
| S-tick-35 | `IssueComment`, `PullReads`, `Watched`, `Event` (`Opened`, `HeadMoved`, `ReviewRequested`, `Comment`), `PrCursor`, `Cursor` (`prs`, `requested`, `rotation`, `tick`, `receipts_done`), `Intake` (`events`, `heads`, `deferred`, `unread`), `watched`, `intake`; `MAX_EVENTS_PER_TICK` 200, `MAX_READS_PER_TICK` 120, `REVIEW_COMMAND` | type | `src/tick/intake.rs` | Polls only the PRs cm tracks (steps or parts at `pr-open`), with reads 11-D10 already opens; no PR list read (G9). Intake owns no obligation (R04). A `@dekopon review` request stays in `Cursor::requested` until an intent names it. A comment cursor moves only past comments a stage marked handled. A PR whose lane has no reviewed family gets no comment reads (U04). `Cursor::tick` numbers ticks (R11), and `receipts_done` is the refused-receipt cursor (R17). |
| S-tick-36 | `GhPullReads` and the intake read loop | task | `src/tick/intake.rs` | **Owner:** the tick's thread; one `gh` child per read, `forge::GH_TIMEOUT`. **Bound:** 120 reads, 200 events, round-robin from `Cursor::rotation`. **Full queue:** the unread PRs go first next tick, counted `deferred`, with no item. **Failure:** `unread` with its reason; the cursor is unchanged and the tick goes on. **Shutdown:** nothing in flight between ticks. |
| S-tick-37 | `LaunchRoute` (`astra-script`, `mac-lane`, `packet`), `Purpose`, `route`, `Ready`, `ReviewWant`, `Launched`, `Dispatched` | type | `src/tick/dispatch.rs` | Routes are C16. `route` returns `None` for a gpt-route review request when the runner applies: no executor holds a comment write, so the PR waits with a `missing-capability` item (G1, R23). |
| S-tick-38 | `Launcher` (`astra_start`, `astra_collect`), `AstraStart`, `Collected`, `ScriptLauncher`; `MAX_LAUNCHES_PER_TICK` 8, `LAUNCH_TIMEOUT` 300 s | task | `src/tick/dispatch.rs` | **Owner:** the tick's thread; one `astra-review.sh` child at a time. **Bound:** 8 launch intents per tick, reviews first, then steps in `cm next` order. **Full queue:** the rest are deferred and picked again; no item. **Shutdown:** every intent is on main before its launch, so a killed launch reconciles and is never repeated. Collect polls run on `collect` timers. |
| S-tick-39 | `ready`, `reviews_owed(cycle)`, `readmit`, `plan_stage`, `launch_stage`, `observe_step`, `observe_review` | fn | `src/tick/dispatch.rs` | Review debt is level-triggered (R04): every watched PR's current head with no review intent or verdict from the other family is owed, every tick. Before the push: launch intents, plus `cm launch` and `cm attempt begin` for a step. After the push: the launches, binds and `cm attempt record`, and each launch and collected Astra receipt goes to `Deps::agents` (R15). `observe_step` reads `attempt::plan`: `resume` or `ended` binds, `unknown` is undecidable, `dispatch` is not done. |
| S-tick-40 | `PacketKind` (adds `effect-refused`, `action-overdue`, `missing-capability`, `pull-conflict`), `PacketItem`, `DecisionPacket` (`push`, `is_empty`, `render`, `to_wire() -> Option<String>`), `Seen` (`load`, `deliver`, `save`); `PACKET_MAX_ITEMS` 20, `PACKET_MAX_BYTES` 16 KiB, `MAX_SEEN` 1024 | type | `src/tick/packet.rs` | Built from deltas and delivered at most once: seen keys go into the first commit. A persisting state comes back through its reminder timer. Each `cm sync` violation is one `sync` item. `to_wire` is `None` for an empty packet (R01). |
| S-tick-41 | `TickArgs` (`approved`, `effects`, `clone`), `Deps` (with `agents`), `Planned` (with `retry`), `Counters`, `Cycle` (`plan`, `replan`, `handled`, `wrote() -> Result`; fields `tick`, `handled`, `forced`, `wrote`, `pending`), `TickRun`, `tick`, `cli_tick` | fn | `src/tick/run.rs` | The stage order, 0 to 11, is in the module doc. Two commits: the first carries intents, `emitted` rows, tally reservations, cursor, timers and seen set, before anything external happens. The second carries binds and receipts. **F1:** stage 1 saves the `decide` mark; `Cycle::wrote` is called before each write; an error before the first push discards everything the tick wrote; the pushed first commit turns the mark to `record`; the pushed second commit clears it. So "an error in stages 1 to 9 leaves the pushed state as it was, and the next tick redoes the work" holds, and so does it for a kill. The loaded tick number is the larger of `Cursor::tick` and the highest row `cycle`. Paused: stage 3's retries and stages 6 to 8 plan nothing, and stage 10 launches and applies nothing. |
| S-tick-42 | the tick process | task | `src/tick/run.rs` | **Owner:** the `cm` main thread. **Bound:** one tick per clone (S-tick-05), each stage within its own bound. **Queue:** none in memory: what waits is in the ledger, the timers or the cursor. **Shutdown:** a killed tick leaves at most its pending mark, an unpushed commit, dirty cm-owned or marked files, a merge in progress and a torn ledger line, which the next tick's stage 1 and the ledger's exclusive open handle (F1). |
| S-tick-50 | `TrustedApps`, `Printed`, `BoundVerdict`, `Refusal`, `Gated`; `REVIEW_BUDGET` 3 | type | `src/tick/gate.rs` | `Refusal` names every must-not-merge case in the done check, plus the grammar's: `NoNonce`, `ShortHead`, `NoWord` (unsure, which goes to Opus), `WordDisagrees`, `ReceiptUnfinished`, `ReportUnrecorded`. |
| S-tick-51 | `parse_comment`, `bind_comment`, `bind_astra`, `fix_required_count`, `approved_context` | fn | `src/tick/gate.rs` | Binds only at the PR's current head (C10). Binding writes the `verdict` row that consumes the nonce. A third FIX REQUIRED on one PR is a `review-budget` item. `approved_context` reads `Approved` at a given SHA for `cm advance --approved` and `cm sync --approved` (R07). Every proof read uses `EffectLedger::snapshot` (R08). |
| S-tick-52 | `proof_gaps`, the seams `commands::advance_proved` and `sync::sync_proved`, plus the existing-file changes in `verdict.rs` (parse `nonce:`; `merge_gaps` takes `&ProofRule`; the RULING text) and `sync.rs` (`gate_holds` for every reviewed lane) | fn | `src/tick/gate.rs`, `src/verdict.rs`, `src/commands.rs`, `src/sync.rs` | "`cm advance` and `cm sync` take the verdict proof LIMITS names for the phase" (C15). The approved channel is `--approved` (R07). Without it both keep today's bootstrap gate. Stage 4 calls `sync_proved` with the cycle's approved context. |
| S-tick-53 | `BranchKind`, `MergeReads` (`base_ref`, `queued`), `GhMergeReads`, `MergePlan` (`Effect`, `Packet`, `Wait`, `Missing { gap }`), `PostMerge` | type | `src/tick/merge.rs` | `base_ref` reads the PR's own base branch (`pulls/{number}` `base.ref`, R22), and `queued` reads `rules/branches/{branch}` (11-D10). `Missing` is a capability the RPi lacks (G2): a `missing-capability` item, never a workaround (R23). |
| S-tick-54 | `route`, `confirm`, `observe`, `apply_local`, `readmit`, `pins_held`, `stage` | fn | `src/tick/merge.rs` | Queued gets `enqueue` with `expectedHeadSha`; any other branch gets REST `merge` with it. In the interim the enqueue is a packet action. `confirm` reads the merged PR; a mismatch parks (every pin held, journaled), and `pins_held` holds pins until a ruling. The revert PR is a packet action in the interim and `Missing` on the RPi (G2). On core dekopon, `route` asks `release::drop_admits` and `stage` calls `release::reserve` before the intent (R09). |
| S-tick-60 | `CORE_DROP_CAP` 3, `CORE_REPO`, `CUT_WORKFLOW_FILE`, `ReleaseReads` (`last_tag`, `workflow_id`, `dispatch_runs`, `branch_head`), `DispatchRun`, `GhReleaseReads`, `Cut`, `ReleaseRefusal`, `CutPlan`, `DropTally` (`drop`, `file`, `counting_merges`, `prep_open`), `DropAdmission` (`Exempt`, `Count`, `Hold`), `TallyRow` | type | `src/tick/release.rs` | The tally is the open drop's `RELEASE.md`, one row per counting merge, read every time. `workflow_id` and `dispatch_runs` route through G3. |
| S-tick-61 | `DropTally::read`, `drop_admits(root, authority, repo, pr, prep)`, `reserve`, `plan`, `cut_input`, `observe`, `readmit`, `apply_local`, `stage` | fn | `src/tick/release.rs` | R09: the prep PR is `Exempt` and counts nothing; while it is open every other core PR is `Hold`; with no open drop a core merge waits; `reserve(cycle, tally, row)` names `RELEASE.md` through `Cycle::wrote` and then writes the counting PR's row before its merge intent, in the first commit, so a tick that dies first loses the row with its other decisions (F1). A PR that already holds a row is admitted again without counting twice, and `reserve` writes nothing for it. R10: a cut intent binds only by its tag at its SHA; with no receipt and no tag it parks, with any later dispatch run as evidence, and is never read as "not dispatched". `cut_input` always sends `dry_run`, from `Approved::release_mode`. Cuts happen only from a ready `00-release` step, with the bump checked against the last tag and §Gates green at main's head. A refused dispatch files a MAC-LANE item with the dry-run id. |
| S-tick-62 | `HOMELAB_REPO`, `LanReads` (`homelab_pulls`), `GhLanReads`, `HomelabPull`, `Readiness { intent, cycle, pass }`, `Rollout`, `Hold` (adds `ReadinessStale`, `HomelabListUnread`, `NoRolloutMapping`), `ProbeResult` | type | `src/tick/lan.rs` | `Rollout` is what one rollout must reach (R12): pins, Argo application, deployment, namespace, expected image or chart revision, and the merged pin's number and SHA. `homelab_pulls` is G4; unread, the pin holds (R24). |
| S-tick-63 | `readiness`, `readiness_hold(r, previous_tick)`, `ready_homelab_prs`, `rollout_for`, `pin_update(rollout, title)`, `pin_merge`, `lan_leg(rollout)`, `probe(rollout, receipts)`, `observe`, `readmit`, `stage` | fn | `src/tick/lan.rs` | Readiness is the newest `kubectl.get.readyz` ledger receipt, fresh when its row's tick number is at least the loaded `Cursor::tick` (R11). `rollout_for` reads G11's mapping and holds with none. A pin merge is built from the recorded update receipt. `probe` passes only receipts that show the expected revision after the merge; the result moves the step through `cm advance … deployed --evidence`. |
| S-tick-70 | `Requested` (`--model`, `--effort`, `--role` on `cm launch`), `Echo` (`--echo-*`, `--tokens-*` on `attempt record`, `attempt end` and `review record`) | type | `src/tick/agents.rs` | Registered by the skeleton. All absent means existing behaviour. |
| S-tick-71 | `Count`, `Pair`, `Harness`, `Tokens`, `AgentRow` (`parent_session_id: Option<String>`), `Invocation` (`Attempt`, `Intent`, `Review`); `AGENTS_FILE`, `UNKNOWN` | type | `src/tick/agents.rs` | LEDGER.md's §Fields exactly, with a null parent id allowed (R13), plus `invocation` and a flattened map that keeps LEDGER.md's optional fields. The key (R14) is the attempt id for a step, the intent id for a tick-launched review, or `R<number>` for a review recorded with no intent. |
| S-tick-72 | `record_launch`, `echoed`, `fold` | fn | `src/tick/agents.rs` | One row per invocation, rewritten in place under `store::lock`. **Fold (F3):** lines for one `invocation` that arrive by git (a rebuild's re-append, or a merge of two clones) fold field by field: echoed beats `"unknown"`, and the later echoed value wins. Every reader and every cm rewrite folds, so the next write leaves one row and no launch's row is lost. Every other line, parsed or not, is kept byte for byte. An unechoed value is `"unknown"`, never copied from the request. **Writer protocol (R16):** every writer of `AGENTS.jsonl` holds `<epoch>/.cm/lock` for its whole read, change and write; `record.py` takes it too (G13). |
| S-tick-73 | `Node` (with `plan_id`), `Edge`, `Graph`, `graph`, `export` | fn | `src/tick/export.rs` | `cm export --json` carries cm's state, lanes and `--after` edges under `graph`. Each node carries its UNITS.json id, and the dashboard joins on it. UNITS.json stays the plan-fact source (G8, R20). |
| S-tick-74 | `AgentRecorder` (`launched`, `astra_receipt`), `CmAgentRecorder` | trait | `src/tick/agents.rs` | R15. The tick's seam into the agent ledger: `Deps::agents`. `tick/agent-ledger` fills `CmAgentRecorder`, including the Astra `receipt.json` echo. `tick/core` calls it and tests it with a fake. |

## Lifecycle invariants

Every stateful type has its four stubs: abort mid-operation, reset or reload, the growth bound, and restart from durable state. Some types carry more stubs for their own failure modes:
- the ledger: an unread lookup, two clones, the snapshot, the row budget, a late receipt, an earlier cycle's dispatch;
- the checkout: a conflicting pull, the commit set, a killed tick's writes, a wedged checkout, an absent checkout, a discarded decision set, a conflict inside any push, a leftover merge;
- the batch, intake, dispatch and the cycle: a ruling, a failed read, review debt, readmission, a launch error, the lock and the error line.

`wire.rs` carries three bound checks and the fixture round trip. Ids S-tick-100 to S-tick-171 are round 1's and keep their numbers. Four of those stubs were renamed in round 2 because their contract changed: 111, 117, 122 and 229. Round 2's new stubs are S-tick-172 to S-tick-195. Round 3 renamed 177 and 178 and added S-tick-196 to S-tick-199.

| id | test | type | unit that fills it |
|---|---|---|---|
| S-tick-100 | `a_killed_append_leaves_no_partial_row_and_the_torn_line_is_dropped_on_open` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-101 | `a_reopened_ledger_reads_every_row_it_wrote_in_order_with_the_same_states` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-102 | `open_intents_never_exceed_sixty_four_and_a_target_holds_one_open_intent_per_purpose` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-103 | `a_restart_between_intent_and_effect_binds_retries_or_parks_and_never_launches_twice` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-104 | `a_minted_nonce_is_unique_in_the_ledger_and_consumed_once_its_verdict_row_is_written` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-105 | `an_unread_lookup_leaves_the_intent_open_and_parks_nothing` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-106 | `an_interrupted_push_leaves_the_commit_local_and_its_effects_unemitted` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-107 | `a_rejected_push_pulls_without_rebase_and_pushes_the_merge_from_the_new_tip` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-108 | `a_push_is_tried_at_most_three_times_then_reports_exhausted` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-109 | `a_restart_with_an_unpushed_commit_pushes_it_before_the_pull_and_before_any_effect` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-110 | `a_conflicting_pull_is_aborted_and_the_tick_writes_nothing` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-111 | `a_gitignored_tick_file_is_committed_and_a_path_no_cm_command_writes_is_not` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-112 | `a_refused_batch_records_none_of_its_receipts` (`src/tick/record.rs`) | a receipt batch | `tick/wire` |
| S-tick-113 | `rerecording_a_pushed_batch_is_a_no_op_by_intent_id` (`src/tick/record.rs`) | a receipt batch | `tick/wire` |
| S-tick-114 | `a_batch_over_twenty_kib_is_refused_whole_and_an_eight_kib_output_is_recorded_oversize` (`src/tick/record.rs`) | a receipt batch | `tick/wire` |
| S-tick-115 | `a_restart_after_a_pushed_batch_takes_the_next_batch_without_rerecording` (`src/tick/record.rs`) | a receipt batch | `tick/wire` |
| S-tick-116 | `a_ruling_on_an_intent_that_is_not_parked_is_refused` (`src/tick/resolve.rs`) | a ruling (`resolve`) | `tick/ledger` |
| S-tick-117 | `the_local_run_stops_at_the_first_refusal_failure_or_deferral_and_leaves_later_intents_open` (`src/tick/apply.rs`) | a local apply run | `tick/wire` |
| S-tick-118 | `a_tick_killed_mid_apply_reconciles_the_applied_effect_and_reemits_only_the_rest` (`src/tick/apply.rs`) | a local apply run | `tick/wire` |
| S-tick-119 | `a_deferred_effect_stays_open_until_its_receipt_is_recorded` (`src/tick/apply.rs`) | a local apply run | `tick/wire` |
| S-tick-120 | `every_input_round_trips_through_the_provider_field_names` (`src/tick/wire.rs`) | `EffectsOut` and `Receipt` (the wire) | `tick/wire` |
| S-tick-121 | `admitted_effects_and_packet_never_print_past_sixty_four_kib_or_sixteen_effects` (`src/tick/wire.rs`) | `EffectsOut` and `Receipt` (the wire) | `tick/wire` |
| S-tick-122 | `an_output_of_eight_kib_or_not_utf8_is_oversize_with_its_length_and_hash` (`src/tick/wire.rs`) | `EffectsOut` and `Receipt` (the wire) | `tick/wire` |
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
| S-tick-142 | `a_tick_killed_before_its_first_push_marks_no_packet_item_seen` (`src/tick/packet.rs`) | `Seen` and the receipt cursor | `tick/core` |
| S-tick-143 | `a_reloaded_seen_set_keeps_a_delivered_delta_out_of_the_next_packet` (`src/tick/packet.rs`) | `Seen` and the receipt cursor | `tick/core` |
| S-tick-144 | `a_packet_holds_at_most_twenty_items_or_sixteen_kib_and_the_seen_set_at_most_1024_keys` (`src/tick/packet.rs`) | `Seen` and the receipt cursor | `tick/core` |
| S-tick-145 | `a_restart_brings_a_parked_intent_back_only_through_its_reminder` (`src/tick/packet.rs`) | `Seen` and the receipt cursor | `tick/core` |
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
| S-tick-164 | `a_pin_killed_between_update_and_merge_merges_from_the_recorded_receipt` (`src/tick/lan.rs`) | pin intents, `Readiness` and `Rollout` | `tick/release-lan` |
| S-tick-165 | `a_missing_failed_or_stale_readiness_receipt_holds_the_pin` (`src/tick/lan.rs`) | pin intents, `Readiness` and `Rollout` | `tick/release-lan` |
| S-tick-166 | `at_most_one_non_draft_homelab_pr_is_ready_at_a_time` (`src/tick/lan.rs`) | pin intents, `Readiness` and `Rollout` | `tick/release-lan` |
| S-tick-167 | `a_restart_after_a_failed_probe_keeps_the_step_held` (`src/tick/lan.rs`) | pin intents, `Readiness` and `Rollout` | `tick/release-lan` |
| S-tick-168 | `a_killed_rewrite_leaves_the_previous_file_whole` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-169 | `a_reloaded_file_keeps_hand_written_record_py_rows_byte_for_byte_beside_cm_rows` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-170 | `an_invocation_holds_exactly_one_row_however_many_echoes_fill_it` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-171 | `a_restart_before_the_echo_leaves_every_unechoed_value_unknown` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-172 | `two_clones_appending_rows_merge_by_union_and_every_row_survives` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-173 | `a_snapshot_ignores_a_torn_line_and_never_writes` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-174 | `an_intent_stays_within_its_row_budget_and_no_segment_reaches_one_mib` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-175 | `a_late_receipt_after_park_closes_the_intent_and_a_conflicting_one_is_refused` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-176 | `an_earlier_cycles_unreceipted_dispatch_parks_and_is_never_reemitted_by_absence` (`src/tick/ledger.rs`) | `EffectLedger` | `tick/ledger` |
| S-tick-177 | `a_cm_commands_writes_and_a_killed_ticks_recorded_facts_are_committed_before_the_next_pull` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-178 | `a_wedged_owned_checkout_rebuilds_from_origin_keeping_every_ledger_and_agent_row` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-179 | `an_absent_checkout_is_cloned_and_a_reaped_one_recloned` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-180 | `a_retry_past_the_ruling_budget_is_refused_and_done_or_drop_still_close` (`src/tick/resolve.rs`) | a ruling (`resolve`) | `tick/ledger` |
| S-tick-181 | `the_shared_fixture_parses_as_the_runner_shapes_and_reserializes_byte_for_byte` (`src/tick/wire.rs`) | `EffectsOut` and `Receipt` (the wire) | `tick/wire` |
| S-tick-182 | `a_paused_or_full_tick_keeps_review_debt_and_every_unhandled_comment` (`src/tick/intake.rs`) | `Cursor` | `tick/core` |
| S-tick-183 | `a_retry_is_readmitted_by_its_owner_under_current_authority_and_keeps_its_id` (`src/tick/dispatch.rs`) | launch intents | `tick/core` |
| S-tick-184 | `each_refused_receipt_local_or_runner_puts_one_effect_refused_item_once` (`src/tick/packet.rs`) | `Seen` and the receipt cursor | `tick/core` |
| S-tick-185 | `a_handed_action_past_its_deadline_parks_and_arms_its_reminder` (`src/tick/packet.rs`) | handed intents and timers | `tick/core` |
| S-tick-186 | `a_proof_read_inside_a_locked_tick_uses_the_snapshot_and_writes_nothing` (`src/tick/gate.rs`) | ledger readers (`EffectLedger::snapshot`) | `tick/gate-merge` |
| S-tick-187 | `the_queue_rule_is_read_for_the_prs_own_base_branch` (`src/tick/merge.rs`) | merge and enqueue intents | `tick/gate-merge` |
| S-tick-188 | `a_revert_on_the_rpi_is_a_missing_capability_item_never_a_packet_action` (`src/tick/merge.rs`) | merge and enqueue intents | `tick/gate-merge` |
| S-tick-189 | `two_candidates_at_a_tally_of_two_reserve_one_row_and_the_prep_is_exempt_but_holds` (`src/tick/release.rs`) | `DropTally` and cut intents | `tick/release-lan` |
| S-tick-190 | `an_unconfirmed_dispatch_parks_and_is_never_inferred_from_a_missing_tag` (`src/tick/release.rs`) | `DropTally` and cut intents | `tick/release-lan` |
| S-tick-191 | `readiness_recorded_in_the_same_minute_as_the_previous_push_is_ordered_by_tick_number` (`src/tick/lan.rs`) | pin intents, `Readiness` and `Rollout` | `tick/release-lan` |
| S-tick-192 | `healthy_receipts_from_the_old_deployment_do_not_pass_the_probe` (`src/tick/lan.rs`) | pin intents, `Readiness` and `Rollout` | `tick/release-lan` |
| S-tick-193 | `a_null_parent_session_id_row_round_trips_and_an_unparsable_row_is_kept_raw` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-194 | `two_reviews_of_one_head_by_two_reviewers_get_two_rows` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-195 | `an_append_by_a_lock_holding_writer_during_a_cm_rewrite_is_kept` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
| S-tick-196 | `a_tick_killed_before_its_first_push_is_discarded_whole_and_its_tally_row_and_new_segment_go_with_it` (`src/tick/push.rs`) | the pending mark (`PendingMark`, `discard`) | `tick/ledger` |
| S-tick-197 | `a_conflict_inside_commit_and_push_keeps_the_commit_local_and_only_begin_rebuilds` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-198 | `a_merge_left_by_a_killed_git_child_is_aborted_before_anything_else` (`src/tick/push.rs`) | the checkout (`Git`, `begin`, `commit_and_push`) | `tick/ledger` |
| S-tick-199 | `two_lines_for_one_invocation_fold_field_by_field_and_the_next_write_leaves_one` (`src/tick/agents.rs`) | `AGENTS.jsonl` | `tick/agent-ledger` |
## Done-check stubs

Integration stubs, one file per unit. Together they carry each unit's done check: UNITS.json's for the five starting units, and §Proposed done checks for `tick/ledger`'s amendment, `tick/wire` and `tick/agent-ledger`'s amendment. S-tick-202 and S-tick-203 moved from `tests/tick_ledger.rs` to `tests/tick_wire.rs` with their ids. Round 2's new stubs are S-tick-230 to S-tick-236, and round 3's are S-tick-237 and S-tick-238.

| id | test | file | unit |
|---|---|---|---|
| S-tick-200 | `cm_writes_and_pushes_an_intent_row_with_pr_head_nonce_model_and_family_before_its_effect` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-201 | `a_restart_between_intent_and_effect_reconciles_or_parks_with_no_second_launch` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-202 | `tick_effects_stdout_parses_as_the_runner_wire_shape_with_provider_field_names` | `tests/tick_wire.rs` | `tick/wire` |
| S-tick-203 | `effects_record_takes_a_twenty_kib_batch_commits_and_pushes_it_and_records_oversize_outputs_by_hash` | `tests/tick_wire.rs` | `tick/wire` |
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
| S-tick-229 | `cm_export_json_emits_cms_graph_with_plan_ids_for_the_dashboard_to_join` | `tests/tick_agent_ledger.rs` | `tick/agent-ledger` |
| S-tick-230 | `a_killed_tick_and_a_second_clone_recover_by_the_dirty_tree_and_union_rules` | `tests/tick_ledger.rs` | `tick/ledger` |
| S-tick-231 | `the_shared_fixture_is_the_runner_skeletons_tick_plan_and_receipt_batch` | `tests/tick_wire.rs` | `tick/wire` |
| S-tick-232 | `admission_bounds_the_encoded_stdout_before_any_intent_row_is_written` | `tests/tick_wire.rs` | `tick/wire` |
| S-tick-233 | `the_local_applier_keeps_effect_order_and_stops_at_a_refusal_failure_or_deferral` | `tests/tick_wire.rs` | `tick/wire` |
| S-tick-234 | `a_paused_tick_then_a_resumed_one_still_reviews_every_head_and_handles_every_comment` | `tests/tick_core.rs` | `tick/core` |
| S-tick-235 | `a_counting_merge_reserves_its_tally_row_in_the_same_commit_as_its_intent` | `tests/tick_release_lan.rs` | `tick/release-lan` |
| S-tick-236 | `the_recorder_writes_one_row_per_tick_launch_and_fills_it_from_the_collected_astra_receipt` | `tests/tick_agent_ledger.rs` | `tick/agent-ledger` |
| S-tick-237 | `a_tick_killed_or_failing_before_its_first_push_leaves_main_as_it_was_and_the_next_redoes_it` | `tests/tick_core.rs` | `tick/core` |
| S-tick-238 | `a_killed_reservation_is_reserved_again_once_and_a_readmitted_merge_keeps_its_one_row` | `tests/tick_release_lan.rs` | `tick/release-lan` |
### Proposed done checks

**Coordinator:** these replace or add UNITS.json and PLAN.md done checks (R21, R20). They are proposals, not plan edits.

- **`07-tick/ledger`, amended (R21).** Size M, unchanged. The ledger keeps the ledger, the checkout and the ruling; the wire leaves. Done when:
  - cm writes and pushes an intent row (PR, head SHA, nonce, intended model and family) before an effect, and a test that restarts between intent and effect reconciles or parks with no second launch;
  - the dirty-tree recovery and two-clone union rules are covered: a tick that dies before its first push is discarded whole, a killed tick's recorded facts and a `cm` command's writes reach main before the next pull, and both clones' appended rows survive their merge;
  - a conflict at any push keeps its commit, only `begin` rebuilds a checkout cm owns, and a rebuild keeps every ledger and agent row main lacks;
  - `cm effects resolve` keeps the intent's id, refuses a ruling on an intent that is not parked and a retry past the ruling budget, and a retry is readmitted by its owner, never replayed;
  - a full ledger still accepts reconciliation and completion rows.

  Stubs: S-tick-200, 201, 204 and 230, with lifecycle stubs S-tick-196 to 198.
- **`07-tick/wire`, new (R21).** "cm's side of the 11-D3 runner wire" · wave 1 · cm · claude-cloud · S · `depends_on: 07-tick/ledger`. Done when:
  - `cm tick --effects` stdout and `cm effects record` stdin parse as the shared conformance fixture's shapes, the bytes dekopon's `rpi/tick-runner` tests parse;
  - admission bounds the whole encoded stdout before any intent row is written;
  - a receipt batch obeys the 8 KiB output and 20 KiB batch limits, validates atomically, records idempotently by intent, and is pushed before exit 0;
  - the local applier keeps effect order and stops at the first refusal, failure or deferral.

  Steps: `07-tick/wire/build` (claude-cloud), `00-release/cm/<next>` (coordinator). Stubs: S-tick-202, 203 and 231 to 233. `07-tick/core` gains `depends_on: 07-tick/wire`, since stage 9 admits and stage 10 records through it.
- **`07-tick/agent-ledger`, amended (R20, R15).** Replace "and `cm export --json` emits the graph the dashboard reads" with: "`cm export --json` emits cm's graph with each node's UNITS.json id, which the dashboard joins to UNITS.json, the plan-fact source; and the recorder writes one row per tick launch and fills it from the collected Astra receipt." Retiring UNITS.json leaves this unit until the owner names a plan-fact source (G8). Stubs: S-tick-225 to 229 and 236.

## Footprint

```footprint
{"unit": "tick/ledger", "step": "07-tick/ledger/build", "files": ["src/tick/ledger.rs", "src/tick/push.rs", "src/tick/resolve.rs", "tests/tick_ledger.rs", "tests/fixtures/tick-ledger/"], "items": ["S-tick-14", "S-tick-15", "S-tick-16", "S-tick-18", "S-tick-19", "S-tick-20", "S-tick-22", "S-tick-100", "S-tick-101", "S-tick-102", "S-tick-103", "S-tick-104", "S-tick-105", "S-tick-106", "S-tick-107", "S-tick-108", "S-tick-109", "S-tick-110", "S-tick-111", "S-tick-116", "S-tick-172", "S-tick-173", "S-tick-174", "S-tick-175", "S-tick-176", "S-tick-177", "S-tick-178", "S-tick-179", "S-tick-180", "S-tick-196", "S-tick-197", "S-tick-198", "S-tick-200", "S-tick-201", "S-tick-204", "S-tick-230"], "rank": 1}
{"unit": "tick/wire", "step": "07-tick/wire/build", "files": ["src/tick/wire.rs", "src/tick/record.rs", "src/tick/apply.rs", "tests/tick_wire.rs", "tests/fixtures/tick-wire/"], "items": ["S-tick-10", "S-tick-11", "S-tick-12", "S-tick-13", "S-tick-21", "S-tick-23", "S-tick-24", "S-tick-112", "S-tick-113", "S-tick-114", "S-tick-115", "S-tick-117", "S-tick-118", "S-tick-119", "S-tick-120", "S-tick-121", "S-tick-122", "S-tick-181", "S-tick-202", "S-tick-203", "S-tick-231", "S-tick-232", "S-tick-233"], "rank": 2}
{"unit": "tick/core", "step": "07-tick/core/build", "files": ["src/tick/authority.rs", "src/tick/timers.rs", "src/tick/intake.rs", "src/tick/dispatch.rs", "src/tick/packet.rs", "src/tick/run.rs", "tests/tick_core.rs", "tests/fixtures/tick-core/"], "items": ["S-tick-30", "S-tick-31", "S-tick-32", "S-tick-33", "S-tick-34", "S-tick-35", "S-tick-36", "S-tick-37", "S-tick-38", "S-tick-39", "S-tick-40", "S-tick-41", "S-tick-42", "S-tick-123", "S-tick-124", "S-tick-125", "S-tick-126", "S-tick-127", "S-tick-128", "S-tick-129", "S-tick-130", "S-tick-131", "S-tick-132", "S-tick-133", "S-tick-134", "S-tick-135", "S-tick-136", "S-tick-137", "S-tick-138", "S-tick-139", "S-tick-140", "S-tick-141", "S-tick-142", "S-tick-143", "S-tick-144", "S-tick-145", "S-tick-146", "S-tick-147", "S-tick-148", "S-tick-149", "S-tick-150", "S-tick-151", "S-tick-182", "S-tick-183", "S-tick-184", "S-tick-185", "S-tick-205", "S-tick-206", "S-tick-207", "S-tick-208", "S-tick-209", "S-tick-234", "S-tick-237"], "rank": 3}
{"unit": "tick/agent-ledger", "step": "07-tick/agent-ledger/build", "files": ["src/tick/agents.rs", "src/tick/export.rs", "tests/tick_agent_ledger.rs", "tests/fixtures/tick-agent-ledger/"], "items": ["S-tick-70", "S-tick-71", "S-tick-72", "S-tick-73", "S-tick-74", "S-tick-168", "S-tick-169", "S-tick-170", "S-tick-171", "S-tick-193", "S-tick-194", "S-tick-195", "S-tick-199", "S-tick-225", "S-tick-226", "S-tick-227", "S-tick-228", "S-tick-229", "S-tick-236"], "rank": 3}
{"unit": "tick/gate-merge", "step": "07-tick/gate-merge/build", "files": ["src/tick/gate.rs", "src/tick/merge.rs", "src/tick/run.rs", "src/verdict.rs", "src/commands.rs", "src/sync.rs", "tests/tick_gate_merge.rs", "tests/verdict.rs", "tests/lifecycle.rs", "tests/support/verdict.rs", "tests/fixtures/tick-gate-merge/"], "items": ["S-tick-50", "S-tick-51", "S-tick-52", "S-tick-53", "S-tick-54", "S-tick-152", "S-tick-153", "S-tick-154", "S-tick-155", "S-tick-156", "S-tick-157", "S-tick-158", "S-tick-159", "S-tick-186", "S-tick-187", "S-tick-188", "S-tick-210", "S-tick-211", "S-tick-212", "S-tick-213", "S-tick-214", "S-tick-215", "S-tick-216", "S-tick-217"], "rank": 4}
{"unit": "tick/release-lan", "step": "07-tick/release-lan/build", "files": ["src/tick/release.rs", "src/tick/lan.rs", "src/tick/run.rs", "tests/tick_release_lan.rs", "tests/fixtures/tick-release-lan/"], "items": ["S-tick-60", "S-tick-61", "S-tick-62", "S-tick-63", "S-tick-160", "S-tick-161", "S-tick-162", "S-tick-163", "S-tick-164", "S-tick-165", "S-tick-166", "S-tick-167", "S-tick-189", "S-tick-190", "S-tick-191", "S-tick-192", "S-tick-218", "S-tick-219", "S-tick-220", "S-tick-221", "S-tick-222", "S-tick-223", "S-tick-224", "S-tick-235", "S-tick-238"], "rank": 5}
```

Units never edit `src/lib.rs`, `src/tick/mod.rs`, `src/tick/error.rs`, `src/cli.rs`, `Cargo.toml` or `src/forge.rs`. A new error variant, CLI flag, capability, read trait method or dependency is a skeleton change (standing order B2). The three routing matches (S-tick-17, S-tick-24's glue and S-tick-25) are skeleton-owned inside their units' files. `src/verdict.rs` beyond the S-tick-04 field is `tick/gate-merge`'s. In `tests/fixtures/tick-wire/`, the three shared files are frozen (Module tree).

The `run.rs` stage lines:
- `tick/gate-merge` swaps stage 4's `sync::sync` for `sync::sync_proved` and adds `merge::stage(&mut cycle)?` as stage 6.
- `tick/release-lan` adds `release::stage` then `lan::stage` as stage 7.
- Each of those units edits nothing else in `run.rs`.

## Named gaps

These are interface needs that no unit in this area can meet inside its own footprint. Round 2 gives each a disposition or an owner stop: it is never worked around in code. None blocks the skeleton or a done check in the cloud interim, where `gh` reads and writes everything. They bite on the RPi.

- **G1. GPT review request (11-D19; R23).** After gpt-route, PLAN.md `07-tick/core` starts a GPT review with "a review-request comment the github-events route picks up". 11-D19 grants no issue-comment capability.
  - **Disposition:** a declared missing capability, not a workaround. In the interim it is a packet action, and the coordinator comments. On the RPi, `route` returns `None` and the PR waits with one `missing-capability` item.
  - **Owner stop:** a grant, or an amended gpt-route, before the RPi path is accepted.
- **G2. Revert PR (11-D19; R23).** `07-tick/gate-merge` (3) opens a revert PR of a mismatched merge commit. 11-D19 builds no PR-create.
  - **Disposition:** as G1. In the interim it is a packet action. On the RPi it is `MergePlan::Missing`: a `missing-capability` item, with every pin held and the intent parked.
  - **Owner stop:** as G1.
- **G3. Cut workflow id and dispatch runs (11-D10, 11-D14; R10).** `workflow_id` (`actions/workflows/cut-release.yml`) and `dispatch_runs` (`actions/workflows/{id}/runs`) are not in the egress route table. Either the route opens, or the ids come stamped at the approved SHA. Without them a cut parks: it is never inferred.
- **G4. Homelab PR list (11-D10; R24).** "Counts only non-draft homelab PRs" needs `GET /repos/xrl/rpi-homelab/pulls?state=open`. The route table opens only `pulls/{number}` there.
  - **Disposition:** `LanReads::homelab_pulls` is the one reader, and when it cannot read, the pin holds with `HomelabListUnread`.
  - **Owner stop:** an authorized source for the count (open that route, or a list the coordinator stamps). The reader and S-tick-166 and S-tick-222 stand either way.
- **G5. Clone on session create (11-D7; R25).**
  - **Proposed disposition:** cm owns the clone. The frozen runner command is `cm tick --effects --approved <sha> --clone <url> -C <checkout>/store/epochs/<slug>`: absent, cm clones; reaped, cm re-clones; present, cm uses it.
  - **Owner:** `rpi/cm-in-vm` puts the flag in the image's command, and `rpi/tick-runner` puts it in its command line. Acceptance: S-tick-179 (create, reap, re-clone).
  - **Coordinator:** confirm this, or rule that the image clones and drop the flag.
- **G6. kubectl inputs (11-D20).** 11-D20 fixes the four command forms, not the effect inputs. The shapes in S-tick-12 are this skeleton's reading, and `11-rpi/k8s-provider` either confirms them or forces a skeleton change.
- **G7. Update-branch against exact-head binding.** The tick binds a verdict only at the PR's current head. `execution.md` §Branches and main updates every open PR after each merge, so under the tick each update-branch restarts that PR's review. `verdict::clean_update`'s path stays for `cm advance` and `cm sync`. The coordinator decides whether update-branch changes in the tick era.
- **G8. Dashboard plan facts (R20).**
  - **Disposition:** UNITS.json stays the plan-fact source, and cm does not copy plan facts. The export carries each node's UNITS.json id, and the dashboard's adapter joins on it.
  - **Owner stop:** "UNITS.json retires" needs a plan-fact source the owner names. The amended done check above takes retirement out of `tick/agent-ledger`.
- **G9. Who records `pr-open` on the RPi (R26).** Intake watches the steps cm has at `pr-open`. Writer lanes have no campaign-trail write.
  - **Proposed consumer:** `11-rpi/coordinator-agent`, which holds the git transport to the ledger and `vm.exec` on its own jail. It records a writer lane's PR URL with `cm advance <step> pr-open --pr <url> --sha <head>` and pushes before the next tick's intake.
  - **Coordinator:** add "a writer lane's PR is recorded `pr-open` and pushed before the next tick" to that unit's done check, or name another consumer. In the interim the coordinator records it as today.
- **G10. Release mode key (R10).** `Approved::release_mode` reads `[authority] release_mode = "dry-run"` at the approved SHA. That key is not in EXECUTION.md's `[authority]`, and 11-D27 lets the probe epoch use only the stamp's keys.
  - **Owner stop:** add the key, or name another approved source. Until then a probe epoch's cuts have no approved dry-run source, and cm sends `dry_run: false` only under `Real`.
- **G11. Rollout mapping (R12).** No source in cm or the plan maps a cm release line to pin names and values, the Argo application, the deployment, the namespace and the expected revision.
  - **Disposition:** `rollout_for` reads it, and holds the step with `NoRolloutMapping` when there is none.
  - **Owner stop:** name the mapping's file and owner. The rpi-homelab pin layout is the likely source.
- **G12. Cut run identity (R10).** `cut-release.yml` has no `run-name`, so a dispatch run cannot be tied to its tags, and a cut binds only by its tag. A `run-name` naming the tags, in each repository's `cut-release.yml`, would let a run bind. That change is outside these units.
- **G13. `AGENTS.jsonl` writer lock (R16, F4).** campaign-trail `tools/agent-ledger/record.py` loaded and replaced the file without a lock; the coordinator committed its lock after round 2, and the harvesters' (F4) are the coordinator's too. The lock is per clone: lines that arrive by git fold (S-tick-72, F3).
  - **Coordinator:** a campaign-trail change so that `record.py` takes `<epoch>/.cm/lock` (`flock LOCK_EX`) around its load and replace, before `tick/agent-ledger` lands. Until then cm and `record.py` must not write one epoch's file at the same time.
  - Acceptance: S-tick-195.
- **G14. Ledger merge attribute (R03).**
  - **Coordinator:** commit `store/epochs/*/.cm/tick/effects-*.jsonl merge=union` to campaign-trail `.gitattributes`, before `tick/ledger` lands. This is the verdict's disposition.
  - `push::begin` refuses to run without it (`LedgerNotUnion`). Acceptance: S-tick-172 and S-tick-230.

## Checks

These are filled in by the coordinator's `check` step, not by this writer. Round 1's rows are copied from its verdict.

**Walks.**

| walk | report | open gaps |
|---|---|---|
| round 1, walk 1 | `check/evidence/walk-1.md` | 15 (4 high), answered by R01 to R26 |
| round 1, walk 2 | `check/evidence/walk-2.md` | 9, answered by R01 to R26 |
| round 1, walk 3 | `check/evidence/walk-3.md` | 14, answered by R01 to R26 |
| round 2, walk 1 | `check/evidence/walk-1-r2.md` | W1-16 to W1-19; the blocking ones are answered by F1 and F2 |
| round 2, walk 2 | `check/evidence/walk-2-r2.md` | see the report |
| round 2, walk 3 | `check/evidence/walk-3-r2.md` | N3-1 to N3-10; the blocking ones are answered by F2 and F3, and N3-3 by U13 |

**Adversarial read.** Round 1: `check/evidence/fable-r1.md`, FIX REQUIRED, 13 contract. Round 2: `check/evidence/fable-r2.md`, FIX REQUIRED; its N8 and N12 are answered by F1 to F3, and F14 is the coordinator's F4.

**Adjudicator.** Round 1: `check/evidence/adjudicator-r1/report.md`, FIX REQUIRED, R01 to R26. Round 2: FIX REQUIRED, F1 to F3 in this skeleton's scope (the coordinator's brief). Round 3: pending.

**Fork bake-off.** None. The runner skeleton, the provider schemas and PLAN.md fix the shapes; no real fork was found.

## Landing

- **Train.** `footprint-edges.py` on this file (the tool as at campaign-trail `10667b29`), default and `--strict` alike:
  - `# train 1: tick/core -> tick/gate-merge -> tick/release-lan`
  - `cm amend 07-tick/gate-merge/build --after pr-open:07-tick/core/build`
  - `cm amend 07-tick/release-lan/build --after pr-open:07-tick/gate-merge/build`
  - `# parallel: tick/agent-ledger tick/ledger tick/wire`

  The only hot file is `src/tick/run.rs`. `tick/ledger`, `tick/wire` and `tick/agent-ledger` share no file with anyone. Their order comes from `depends_on`:
  - `tick/wire` follows `tick/ledger` (proposed);
  - `tick/core` follows both;
  - `tick/agent-ledger` follows `tick/ledger` and `mac/cloud-usage-probe`.
- **Merges.** None. No pair shares three or more hot files: core shares `run.rs` with gate-merge, and gate-merge shares it with release-lan.
- **Relax step.** `07-tick/tick/relax`, after `tick/release-lan`'s live check. It is expected to:
  - remove any `#![allow(unused_variables)]` a unit left;
  - fold `forge::ci_green` into `authority::checks_green` if `cm sync` can take the stricter rule;
  - drop `LocalApply` and the interim packet actions once the RPi cutover leaves no cloud applier.

## Choices I made

Round 1's choices stand except where a later choice below replaces one.

- **C1. Units.** The five starting units are kept, none is dropped, and `tick/wire` is added (C22). `rpi/cm-in-vm` and `05-integration/advisory-locks` are left out because neither adds cm source.
- **C2. The cm side of 11-D3 is its own unit, `tick/wire`** (replaced in round 2 by C22). Round 1 put it in `tick/ledger`.
- **C3. One cycle, two appliers.** The plan has `cm tick` merging, cutting and pinning in the cloud interim and the rehearsal, before any dekopon runner exists. So:
  - `cm tick` applies locally, in the runner's order and with its stop rule. It applies a REST merge and a cut dispatch itself, through the session's `gh`. It hands every other effect to the coordinator as a packet action:
    - the enqueue, because GraphQL is 403;
    - the pins, because the coordinator is the homelab writer;
    - kubectl, through the LAN-leg runbook.
  - `cm tick --effects` prints the effects for the runner.

  Both write the same ledger, so one reconcile serves the interim and the RPi.
- **C4. The provider's schemas are the effect inputs, and the runner's shapes are the envelope.**
  - `MergeInput` carries `mergeMethod`, which provider-gh requires (no default). `MERGE_METHOD` is `squash`, `merge-pr.py`'s default.
  - `gh.workflow.run` is `{owner, repo, workflow, ref, inputs: [{name, value}]}`.
  - The envelope (`EffectsOut`, `Receipt`) is dekopon#983's, per the verdict's disposition (R01).
- **C5. What a tick commits.** The tick's own `.cm/tick/` files with `--force` (campaign-trail ignores `*.jsonl`), every dirty cm-owned path, and the paths the tick named. Round 2 replaced round 1's "changed during the tick" with the cm-owned set, so a killed tick's writes are recovered (R02). A path no cm command writes, such as an ignored transcript or another tool's file, is never swept in.
- **C6. Locks.** One non-waiting tick lock covers `.cm/tick/` writers. Readers use a snapshot (R08). The tick never holds `store::lock` across an existing cm call or a child process.
- **C7. Step launches reconcile through the existing attempt record.** The intent row names the attempt, and `observe_step` reads `attempt::plan`.
- **C8. Errors.** An error means durable state could not be written. Everything per item is data. With `--effects`, a tick that fails still prints one line with a `tick-aborted` item and exits 0.
- **C9. Readiness.** Readiness is always a ledger receipt of `kubectl.get.readyz`. "One heartbeat" is the previous tick, ordered by tick number (R11; round 1 used minute stamps).
- **C10. The tick binds a verdict only at the current head** (G7).
- **C11. The drop cap is checked from `merge::route` into `release::drop_admits` and `release::reserve`.** This keeps UNITS.json's assignment of the cap to `tick/release-lan`. Between gate-merge's and release-lan's landings, a core `dekopon` merge through the tick hits `todo!()` before writing any intent. That is loud and safe, and it is not reachable in production.
- **C12. agent-ledger.** The row is LEDGER.md's, with one row per invocation, keyed by `Invocation` (R14), and every unparsed line is kept (R13). The CLI calls it only when its flags are given. The tick calls it through `AgentRecorder` (R15).
- **C13. Bounds the plan does not fix.** These are mine:
  - per tick: 16 effects, 8 launches, 120 reads and 200 events, a packet of 20 items or 16 KiB;
  - durable: 64 open intents, 256 timers, 1,024 seen keys, 38 rows per intent, 512 KiB segments;
  - five tries and two rulings;
  - a 6 h reminder, a 90 min review deadline, a 2 h action deadline, a 300 s launch timeout, and a 5 min to 1 h backoff.

  The wire bounds (8 KiB, 20 KiB, 64 KiB) are 11-D3's.
- **C14. The nonce is 32 hex digits of SHA-256 over the ledger path and the intent id.** It is a correlation value, not a secret (IDEA-0063).
- **C15. `cm sync`'s gate applies the phase proof to every reviewed lane,** through `sync_proved` only (C23). That can change existing gate tests, so `tick/gate-merge`'s footprint includes `tests/verdict.rs`, `tests/lifecycle.rs` and `tests/support/verdict.rs`.
- **C16. Launch routes.**
  - A GPT review is `astra-review.sh` until gpt-route.
  - A Claude session (a review, or a claude-cloud step) is a packet action for the coordinator's `create_session` when cm applies locally, and one MAC-LANE launcher item on the RPi.
  - Codex, coordinator, fable and pi steps are packet actions.
  - A gpt-route review request has no RPi route (G1).
  - Owner steps are never dispatched.
- **C17. Packet delivery is at most once per delta.** Seen keys go into the first commit. Persisting states come back through reminder timers.
- **C18. Intake polls only the PRs cm tracks, with reads 11-D10 already opens,** and owns no obligation (R04).
- **C19. Ids.** Round 1's ids are kept. Round 2 appends:
  - items S-tick-25 and S-tick-74;
  - lifecycle stubs S-tick-172 to 195;
  - done-check stubs S-tick-230 to 236.

  A renamed stub keeps its id. The moved S-tick-202 and 203 keep theirs.
- **C20. Ranks.** ledger 1, wire 2, core 3, agent-ledger 3, gate-merge 4, release-lan 5.
- **C21. Base.** Main `0e38f02`, unchanged. Nothing is rebased or force-pushed.
- **C22. The `tick/wire` split (R21).** These files move to `tick/wire`:
  - `wire.rs`, `record.rs` and `apply.rs`;
  - `tests/tick_wire.rs`;
  - the fixture.

  The ledger keeps `ledger.rs`, `push.rs` and the new `resolve.rs`. The cut follows the seam: the wire is everything the runner sees, and the ledger is everything the reconcile needs. They share no file, so each unit's footprint is closed.
- **C23. The approved channel is a flag (R07).** `--approved <sha>` on `cm advance` and `cm sync`, the same SHA form as `cm tick`. Without the flag, both keep today's bootstrap gate, so no existing caller changes. With it, they call new seams (`advance_proved`, `sync_proved`) rather than changing `advance_gated` and `sync`, so `tick/gate-merge` changes no existing signature.
- **C24. Retries are readmitted, not replayed (R05).** A `NotDone` or ruled-`retry` intent goes back to the stage that owns its kind. That stage builds the input again under current authority and keeps the id. The stored input is used only to show it is the same intent.
- **C25. Review debt is a level, not an edge (R04).** `reviews_owed` reads current heads every tick, so nothing needs an event to survive. Comments are the one edge: a comment cursor moves only past comments a stage marked handled.
- **C26. Tick numbers order the ledger (R11).** `Cursor::tick` counts pushed ticks. Intent, `emitted` and receipt rows carry the number they were written under. Readiness freshness and the earlier-cycle rule (R06) compare numbers, never minute stamps.
- **C27. The conservative observer (R06).** An effect that is not idempotent (a merge, enqueue, dispatch or pin update), emitted by an earlier cycle with no receipt, parks unless positive evidence binds it. Combined with the runner's one-cycle-at-a-time rule, a second compute cannot re-emit work the first cycle may still apply.
- **C28. Rollouts and modes come from approved sources or hold (R10, R12; G10, G11).** No default is invented for a dry run or a pin mapping. Without a mapping, the step holds with an item that names the gap. `release_mode` absent reads as `Real` (U19). That is not a guess: `release_repos` and `release_bumps` at the approved SHA are what authorize a real cut. A probe epoch holds until G10 is ruled by naming no `release_repos`, so every cut is refused, never by a guessed mode.
- **C29. Union merge plus a rebuild for owned clones (R03).** This is the verdict's disposition. Rows are self-describing, so union order is harmless. A conflict outside the ledger in a clone cm owns is rebuilt from origin, at `begin` only (C32), keeping ledger and agent lines (C33). A conflict in the coordinator's own checkout stays a human's `PullConflict`.
- **C30. What round 2 did not change.** U01 to U03 and U11 are recorded below and not changed. Nothing beyond each R-row was added.
- **C31. A killed tick's decisions are discarded, not recovered (F1).** I chose discard and re-derive over recovering `RELEASE.md` and making every decision idempotent. Nothing external happens before the first push, so discarding loses nothing, and it makes the existing statements true as written: an error or kill before the first push leaves the pushed state as it was, the timers file is unchanged, and no packet item is marked seen. Recovering instead would push `emitted` rows for emissions that never happened, which C27 then parks for a human. The pending mark separates a tick's decisions from its recorded facts and from a `cm` command's writes, because `begin` leaves the cm-owned paths clean before the mark is saved. `Cycle::wrote` is write-ahead, so the mark also names the tally file. `reserve` and `drop_admits` are also made idempotent per PR, for readmitted merges.
- **C32. One conflict rule at every push, and a rebuild only in `begin` (F2).** `commit_and_push` never rebuilds. It keeps its commit and returns the error. Each call site's response follows from what has happened by then: stage 9 discards (nothing external yet); stage 11 and `cm effects` keep their facts for the next `begin`; `begin` rebuilds a checkout it owns. So no rebuild runs mid-tick, and stage 8's launch writes are never dropped under a live launch.
- **C33. Agent rows survive a rebuild as lines, and fold by invocation (F3).** I chose a rebuild that keeps the `AGENTS.jsonl` lines main lacks, beside the ledger's, and a fold rule in `agents`. That needs no new `.gitattributes` entry, since `merge=union` would not fit an in-place rewrite. The rebuild stays text-only, so `tick/ledger` does not depend on `tick/agent-ledger`. A merge conflict on the file in a checkout cm owns goes through the same rebuild. In the coordinator's own checkout it is `PullConflict`, as for any file.

## Round 2 repairs

| row | change | where |
|---|---|---|
| R01 | cm adopts dekopon#983's shapes: `EffectsOut { effects, packet: Option<String> }`, `ReceiptOutcome` `ok`/`refused`/`failed`, `ReceiptOutput` `Inline(String)`/`Oversize`, `deny_unknown_fields`. An inline output is the provider's bytes as text, never re-serialized. A shared fixture is added. | S-tick-10, 13, 40; S-tick-181, 231; `tests/fixtures/tick-wire/` |
| R02 | The commit set is the cm-owned dirty paths plus `Cycle::wrote`. `begin` commits a killed tick's writes before the pull, and `PullBlocked` names a non-cm path that blocks it. | S-tick-20, 41; S-tick-111, 177, 230 |
| R03 | `merge=union` precondition (`LedgerNotUnion`), owned-checkout `rebuild`, and a two-clone test. `.gitattributes` is the coordinator's (G14). | S-tick-15, 20; S-tick-172, 178, 230 |
| R04 | Level-triggered `reviews_owed(cycle)`, `Cursor::requested`, `Cycle::handled`; the cursor saves only handled marks. | S-tick-35, 39, 41; S-tick-182, 234 |
| R05 | `Readmit` and the `ledger::readmit` routing to each owner's `readmit`; `Cycle::replan` keeps the id. | S-tick-25, 22, 41; S-tick-183, 180 |
| R06 | The runner serializes cycles (stated in the frozen wire doc); `emitted` rows with tick numbers; the conservative observer. | S-tick-05, 14, 16; S-tick-176 |
| R07 | `--approved` on `advance` and `sync`, `approved_context`, and the `advance_proved` and `sync_proved` seams; stage 4 uses `sync_proved`. | S-tick-02, 51, 52 |
| R08 | `EffectLedger::snapshot` (read-only) and `open(root, &TickLock)`. | S-tick-15; S-tick-173, 186 |
| R09 | `drop_admits(root, authority, repo, pr, prep)` returns `Exempt`, `Count` or `Hold`; `reserve` writes the tally row in the first commit; with no open drop, merges wait. | S-tick-60, 61, 54; S-tick-189, 235 |
| R10 | `dispatch_runs` evidence; an unconfirmed dispatch parks and is never inferred from tag absence; `release_mode` comes from the approved SHA (G10); the run-name gap is G12. | S-tick-33, 60, 61; S-tick-190 |
| R11 | `Cursor::tick`; receipt rows carry `cycle`; `readiness_hold(r, previous_tick)`. | S-tick-35, 62, 63; S-tick-191 |
| R12 | `Rollout`, `rollout_for` (G11 hold), `pin_update(rollout)`, `lan_leg(rollout)`, `probe(rollout, receipts)`. | S-tick-62, 63; S-tick-192 |
| R13 | `parent_session_id: Option<String>`; unparsed rows are kept raw. | S-tick-71, 72; S-tick-193 |
| R14 | `Invocation` keys: attempt id, intent id, or `R<number>`. | S-tick-71; S-tick-194 |
| R15 | `AgentRecorder` in `Deps`; the tick records launches and Astra receipts. The recorder is agent-ledger's, the calls are core's. | S-tick-74, 39; S-tick-236, 208 |
| R16 | One writer protocol under `<epoch>/.cm/lock`; `record.py`'s lock is the coordinator's (G13). | S-tick-72; S-tick-195 |
| R17 | The `effect-refused` item and the `Cursor::receipts_done` cursor, for local and runner receipts. | S-tick-13, 35, 40; S-tick-184 |
| R18 | `action-deadline` timer; a handed intent past it parks with its reminder; a late receipt closes it. | S-tick-14, 16, 34; S-tick-185, 175 |
| R19 | Segments of 512 KiB; `MAX_ROWS_PER_INTENT` 38 from five tries and two rulings; the cap gates new intents only. | S-tick-06, 14; S-tick-174, 180 |
| R20 | `Node.plan_id`; UNITS.json stays the plan-fact source; amended done check proposed (G8). | S-tick-73; S-tick-229 |
| R21 | `tick/wire` split, with proposed done checks for it and the amended `tick/ledger`. | C22; §Proposed done checks |
| R22 | `MergeReads::base_ref`. | S-tick-53; S-tick-187 |
| R23 | G1 and G2 are declared missing capabilities: a `None` route and `MergePlan::Missing` on the RPi. | S-tick-37, 53, 54; S-tick-188 |
| R24 | `HomelabListUnread` hold; G4 remains an owner stop. | S-tick-62 |
| R25 | `cm tick --clone <url>` and `ensure_checkout`; proposed disposition (G5). | S-tick-02, 20; S-tick-179 |
| R26 | Proposed consumer: `11-rpi/coordinator-agent` (G9). | intake module doc |

Follow-ups:
- **Fixed:**
  - U04: no comment reads for unreviewed lanes.
  - U05: `BACKOFF_BASE` and `BACKOFF_MAX`.
  - U06: the fold rule in the ledger doc.
  - U07: one sizing rule.
  - U08: a deferral stops the local run.
  - U09: this round's commit carries a `Changelog:` trailer.
  - U10: a redundant `Cycle` field comment is cut.
  - U12: a failed agent write keeps the launch's outcome in its error.
- **Recorded, not changed:**
  - U01: only violations reach the packet. That is the stated contract; stalls stay in `cm sync`'s own output.
  - U02: idle cursor churn. An empty packet does not promise an empty history.
  - U03: no tick-wide deadline. Each child is bounded, and none was asked for.
  - U11: the rewrite cost of `AGENTS.jsonl` and full ledger reads. These are bounded per epoch by C13; the relax step may revisit them.

## Round 3 repairs

| row | change | where |
|---|---|---|
| F1 (R02; W1-16, N8, N12) | Atomic decision recovery by discard. Stage 1 saves a `decide` pending mark (`PENDING_MARK`, `PendingMark { tick, base, phase, paths }`). `Cycle::wrote(path) -> Result` adds each non-cm-owned path to the mark before its write. `release::reserve(cycle, tally, row)` names `RELEASE.md` that way. An error before the first push, or the next `begin` after a kill, calls `push::discard(git, checkout, campaign, mark)`: the branch goes back to `base` (`Git::restore`), cm-owned and marked paths return to `base`, and `.cm/tick/` is cleaned, including a new ignored segment. A pushed first commit turns the mark to `record`, whose facts are committed with `.cm/tick/` forced. `reserve` and `drop_admits` are idempotent per PR. | S-tick-20, 41, 42, 61; S-tick-177 (renamed), 196, 237, 238 |
| F2 (W1-17, N3-1) | `commit_and_push` keeps its commit on a conflicting or blocked inner pull and returns `PullConflict` or `PullBlocked`; it never rebuilds. The three tick call sites: at `begin` step 3, an owned checkout is rebuilt and any other gets the error; at stage 9 the tick discards and stops; at stage 11 the commit stays local with the mark `record`. Also `cm effects record` and `resolve`, which exit non-zero and leave the commit for the next `begin`. `begin` aborts a leftover merge first (`Git::abort_merge`). The tick number is the larger of `Cursor::tick` and the highest row `cycle`, so a rebuild never rewinds it. | S-tick-20, 41; S-tick-197, 198 |
| F3 (N3-2, N12) | `rebuild` keeps, as lines, what main lacks in the ledger segments and in `AGENTS.jsonl`, resets, and appends them again. `agents::fold(text) -> String` folds lines with one `invocation` field by field, and every reader and every cm rewrite applies it. | S-tick-20, 72; S-tick-178 (renamed), 199 |
| U13 (N3-3) | The fixture's `input` objects have sorted keys, as `serde_json::Value` prints them. Both files re-serialize byte for byte, checked with a throwaway test at this head. | `tests/fixtures/tick-wire/`; S-tick-10, 181 |
| U19 | `release_mode` absent means `Real`, because `release_repos` authorizes a real cut. A probe epoch holds by naming no `release_repos` until G10 is ruled, and G10 stays an owner stop. | S-tick-33; C28 |

Other round-2 follow-ups stay recorded, not widened.

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
