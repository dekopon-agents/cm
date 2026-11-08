cm 0.13 efficiency — approach note, revision 1
Prepared: 2026-11-08T09:07:53Z
Steering first line: # Steering for GPT executors
Steering pin: xrl/agents 8620379ccb7632caa159ef416ce2b1d1d5c799e5

Baseline and boundary

Base and fetched origin/main: ec0ce78d2df0fa0588d58606672afe766307e5b6.
Fetched cm-0.12.0 resolves to that commit. The checkout is clean and detached
at the dispatched base. SCHEMA is 6 on both the base and origin/main.
Toolchain: rustc/cargo 1.98.1. No cm AGENTS.md, PR body template or local
tools/cloud-env/HELPER-RULES.md was found. The pinned steering path was absent
from the local commit; its contents were read from the supplied pinned raw URL.

Status: baseline inspection and this note only. Coding, builds, PRs, ready
stamps and repairs have not started. The coordinator-launched Astra note
review must accept this approach before implementation. The executor has no
coordinator messaging target or native usage receipt interface; cumulative
native usage is unavailable, not zero. The coordinator must attach its
existing dispatch/journal/native usage references to this note. No separate
logging system is proposed.

1. owner-queue-banner

Read OWNER-QUEUE.md using the existing Markdown heading parser and queue-only
validator (src/templates.rs); do not derive unread items from the journal,
RESUME.md, review findings or any other file. The queue contract permits
unnumbered headings, so acknowledge a snapshot of item identities rather than
assuming every heading has a numeric prefix. Each identity is the exact level-2
heading text. A newly present heading absent from the last explicit read is
"newer"; changing only its body does not make an acknowledged item new.
Initially every valid open heading is unread. Numbered headings retain their
number in the banner; legacy headings use their title. Removed headings do not
block. A missing, unreadable or queue-lint-failing file supplies no unread items
and creates no new owner-queue refusal; existing lint diagnostics remain.

Persist one additive optional owner_queue_read snapshot (at and item headings)
in campaign .cm/state.json, with serde defaults and omission when absent.
Only successful cm owner-queue read, under the existing campaign lock, updates
that snapshot and appends the explicit acknowledgement to JOURNAL.md. Use the
existing atomic-write/journal failure patterns so a journal failure cannot
silently acknowledge. Missing/invalid queue read does not alter acknowledgement.
No other command, including queue add/edit/answer/strike, changes the snapshot.

cm next and cm sync print NEW OWNER-QUEUE since last read: <items> and expose
the same items as structured JSON. With unread items, refuse merged transitions
for every lane and part, plus execution of release steps (launch and lifecycle
advances, including sync-derived moves), with owner-queue-unread. Declarations,
amendments and read-only inspection remain available. Apply this outside the
existing review-gate override, with no override, while sync still reads and
updates observations. Do not alter the review gate's existing rules or parser.
Update the next/sync/owner-queue/advance/launch rows and state-file documentation.

2. blocked-reviewer-timer

Use existing native step/part tracks as the explicit waiting source: a track
blocked:EVIDENCE-BLOCKED or blocked:REVIEW_BLOCKED denotes a pending reviewer
wait for the PR/head retained in that track's evidence. The coordinator records
that condition through existing cm advance ... blocked:<reason> [--part ...].
This adds no wait registry, text-grep gate, new command or persisted field.
Elapsed wait is now minus track.since, clamped at zero; it is the age of the
recorded block, not claimed reviewer effort. Surface target, part, PR/head,
reason, since and elapsed duration in next. The recorded PR/head/part identifies
the pending round; no unrecorded reviewer identity is invented. Unblocking or
moving that track clears the timer. Honor next's target/lane filtering.

Report this also as a lint warning to cover the requested next/lint fixture;
it never changes lint's exit or adds a merge-gate condition. Extend the existing
next/CARD rendering where they share the relevant view, and document the source
and acknowledgement-free behavior. A saved report alone does not mutate state.

3. stale-base-surface

Forge::pull already reads GitHub's PR head and base. Extend the existing
.cm/pulls.json observation additively with optional base and last-CI head/base.
Read checks through the existing Forge seam during sync's unlocked network
read phase. A successful, nonempty completed check observation establishes the
head/base pair when that head is first recorded; unknown, absent or pending
checks do not invent a baseline. Keep the pair while that head stays the same;
a later observed tested head establishes the next pair. This intentionally
requires a new head to clear a moved-base warning; a same-head CI rerun is not
assumed to have tested the newer base.

Compare the last CI base with the current observed base only for the same PR
on a still-pr-open track, including parts. Differing known values produce
stale-base beside dirty in next and as a warning in lint, with both bases,
CI head and observation time. Never gate a move or change review rules.
Preserve the existing behavior of retaining observations on an unreadable PR;
no last-CI pair means no warning. Update next/sync/lint and state-file prose.

4. review-wait

Add cm review wait <step> --head SHA --reviewer KIND --deadline RFC3339
[--part NAME]. The named round is the declared step/current PR, optional part,
reviewer and head, matching the existing .cm/verdicts.json recorded-review
fields. Reuse the existing read and SHA-matching conventions; do not modify
review-verdict parsing or require ACCEPT to signal that a round was recorded.
Validate the scope once, then reload the ledger once per second with no campaign
lock held while sleeping. Recheck at the deadline, cap the final sleep to the
remaining duration, and use elapsed monotonic time to avoid a clock adjustment
extending the wait. Missing verdicts ledger is empty; unreadable or malformed
state is unreadable. No writes, implicit review or acknowledgement are added.

Exit 0 for a recorded match, 3 for deadline, 4 for unreadable; ordinary CLI usage
retains exit 2. Scope an exit override to this subcommand so other commands keep
their contracts. JSON reports outcome and the matched recorded round, when any.
Document all three outcomes and background-command usage.

5. partial-status

Add cm mac-lane partial N --receipt TEXT by extending the existing status stamp
and span parser. Stamp **PARTIAL <host date>** (<receipt>), retain the original
instructions, and journal through the existing ledger path. Treat Partial as
open, return state partial in list, and continue to hold campaign close.
Done replaces the complete recognized partial status/receipt span while keeping
the original instruction suffix byte for byte. Do not add a second writer or a
new state file. Update the partial/done/list command rows and Mac-lane format.

Schema, PRs and gates

The first part adding persisted fields raises SCHEMA 6 to 7 only if origin/main
still has 6. Later parts merge origin/main, retain the sibling bump and add
serde defaults/skip_serializing_if for every added field. Update README's state
examples and compatibility paragraph. A non-additive state change or any review
parser/review-gate change beyond the authorized queue refusal is a real stop.

One branch and ready PR per named part, rooted at the dispatched base; merge
origin/main when a sibling lands. Never amend a pushed commit, push main,
merge a PR, create a tag, release, bump package version or advance this code step
to done. Commit user-visible changes with a physical Changelog trailer. No
optional F1–F5 follow-up is planned.

Reuse tests/support Campaign, Fake host and FakeForge; put all new synthetic
inputs in tests/support and keep public code, tests and PR prose free of private
campaign identifiers. Cover explicit read clearing/reappearing banners,
refusal before/allowance after read (including sync and override resistance),
blocked track/part timers and filtering, changed base/check baselines and
unknown reads, wait recorded/deadline/unreadable exits, and partial/list/done
preservation. Assert structured fields for telemetry. Run cargo fmt --all --check,
cargo clippy --all-targets --locked -- -D warnings and cargo test --locked on each
PR; record GitHub CI and exact PR head. Independent reviewers must also execute
the built cm. Coordinator launches Astra per ready PR, then the separate warm
Opus reviewer at ACCEPT; this executor will not supply its own review gate.

No campaign-trail checkout or real writer OWNER-QUEUE.md/MAC-LANE.md is mounted.
The six-epoch base/head lint comparison, epoch-health step-count comparison and
real producer/consumer sync/next/mac-lane-list smoke are therefore unavailable
here. Name these unrun gates in each delivery unless the coordinator supplies
the existing files/receipts. Do not claim synthetic fixtures as the real smoke.
After note acceptance, stamp coding/ready/gate/repair boundaries in the existing
coordinator journal/evidence with UTC time, revision and available native-usage
references. Use the requested smoke command/base/head/PR-head/result table when
its real inputs are available.

Choices I made

Heading snapshots define unread items (body-only edits do not re-alert); the
read acknowledgement lives additively in campaign state; queue refusal covers
release launch and lifecycle execution but permits declaration maintenance;
blocked-reviewer timers use existing blocked tracks rather than a new registry
or report scanner and are also lint warnings; last-CI base changes clear on a
new tested head, not an inferred same-head rerun; review-wait names a round by
its recorded scope/head/reviewer tuple, polls at one second, and uses exits
0/3/4; no optional follow-ups. All choices await the note review.
