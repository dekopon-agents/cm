# cm

Campaign manager: the campaign lifecycle as one tool. A campaign is a folder of state files that a
coordinator, supervisors and drivers read and write while a multi-step code effort runs. `cm` owns
the mechanical parts of that folder: the journal, the step order, the launch checks, the state
moves, the GitHub sync and the lint. Clock stamps come from `cm`, never from an argument.

```sh
brew install dekopon-agents/tap/cm
```

### From a release binary

Each release publishes `cm-<target>.tar.gz` for `x86_64-unknown-linux-musl` (Linux x86_64),
`aarch64-unknown-linux-musl` (Linux arm64) and `aarch64-apple-darwin` (macOS arm64), each with a
`.sha256` beside it. `releases/latest/download/` always serves the newest release:

```sh
target=x86_64-unknown-linux-musl
base=https://github.com/dekopon-agents/cm/releases/latest/download
curl -fsSLO "$base/cm-$target.tar.gz"
curl -fsSLO "$base/cm-$target.tar.gz.sha256"
sha256sum -c "cm-$target.tar.gz.sha256"    # macOS: shasum -a 256 -c "cm-$target.tar.gz.sha256"
mkdir -p cm-unpacked ~/.local/bin
tar -xzf "cm-$target.tar.gz" -C cm-unpacked --strip-components=1
install -m 755 cm-unpacked/cm ~/.local/bin/cm
cm --version
```

`~/.local/bin` must be on `PATH`. To check the archive was built by this repository's release
workflow, optionally run `gh attestation verify "cm-$target.tar.gz" --repo dekopon-agents/cm`.

To pin a version, download the versioned name from its tag instead, for example
`https://github.com/dekopon-agents/cm/releases/download/cm-1.2.3/cm-1.2.3-x86_64-unknown-linux-musl.tar.gz`
and its `.sha256`; the steps are the same with `cm-1.2.3-$target` in place of `cm-$target`.
Releases up to 0.2.0 were tagged `v0.2.0` and published only the versioned names.

### From source

```sh
cargo install --locked --git https://github.com/dekopon-agents/cm
```

## Commands

Every command takes `--json` (one JSON object on stdout with `ok` and `violations`) and `-C DIR`
(the campaign folder; found by walking up from the current directory otherwise). Exit 0 is
success, 1 is a refusal or violations (every one listed, never only the first), 2 is a usage
error. `review wait` additionally uses 3 for its deadline and 4 for unreadable campaign/unit/review state in that command.

| Command | Contract |
|---|---|
| `cm init [DIR]` | Writes `RESUME.md`, `JOURNAL.md`, `LIMITS.toml`, `OWNER-QUEUE.md` and `.cm/state.json` from templates embedded in the binary. Never overwrites: in an existing folder it writes only what is missing and adopts each existing file as the baseline once it lints clean. `cm init <NN-name>/<unit>` inside a campaign writes the unit's `.cm/state.json` and its rendered `STATE.md`. |
| `cm init <NN-name>/<unit>/<step> --lane L [--after A]… [--study \| --kind rollout] [--github OWNER/NAME] [--tag PATTERN] [--artifact SPEC] [--kickoff-file F]` | Declares a step `drafted` in lane `pi`, `codex`, `claude-cloud`, `fable`, `owner`, `coordinator` (run by the coordinator session) or `mac` (work that requires the owner’s Mac), creating the unit if needed. `--study` marks a step that ships no PR: like an `owner`-lane step it finishes with `cm advance <step> done --evidence <path>` (see [Finishing](#finishing)). `--kind rollout` declares a rollout step that moves directly from `merged` to `deployed` with a merge SHA and an Argo sync evidence file. It is mutually exclusive with `--study`, cannot declare a release, and refuses `--lane mac` with `mac-lane-rollout`. Each `--after` is one gate, including `pin:<step>@<sha>` for a recorded head (see [Order](#order)); every step or release it names must already be declared, so the order has no cycles. `--kickoff-file F` writes `kickoff.md` from a non-empty file of at most 1 MiB (`-` reads stdin), never replacing an existing one, and journals one line. `--github` and `--tag` declare a release step's repository and tag (see [Releases](#releases)). A relative folder resolves against `-C` when it is given. A declaration is changed only by `cm amend`: a different one is `step-declared`, and the same one again repairs what an interrupted declaration left half written (the step folder, a missing kickoff when `--kickoff-file` is given, a missing or stale `STATE.md`) without replaying the journal, or else is a no-op. |
| `cm next [<NN-name>/<unit>/<step>] [--lane L] [--act]` | Without `--act`, numbers every item with its next cm command (`command` on each JSON item), shared with CARD.md’s Next, Waiting and Blocked sections; named inputs appear as quoted placeholders (see [Order](#order)). Prints `NEW OWNER-QUEUE since last read: <items>` and JSON `owner_queue_unread` for unread headings. Lists the launchable steps, declared `drafted` with every `--after` met, `kickoff.md` written, `LIMITS.toml` not paused and no Codex quota hold (`steps`), the rest with what each waits on (`waiting`), and the merged release steps that wait for their tag (`tags`). It reports `stalled` launches without a PR after their lane’s stall limit (default two hours), and `dirty` open PRs whose last sync reported `mergeable_state: dirty`. `driver_held`, present only when there is one, notes each live attempt a registered lane driver holds (see [Lane drivers](#lane-drivers)), as an unnumbered `note:` line with no command. See [Remote turns](#remote-turns) for the fallback and `--act` output. `stale_base`, beside `dirty`, reports a still-open PR whose last observed base differs from its recorded CI base, including parts. `review_waits` lists tracks blocked as `EVIDENCE-BLOCKED` or `REVIEW_BLOCKED`, including parts, with block stamp and elapsed seconds (clamped at zero). A step-scope wait on multipart work reports no PR or head because the step evidence aggregates its parts; part-track and single-step waits retain their own PR/head. Record a reviewer wait with the existing `advance … blocked:<reason>`; unblocking clears it. CARD.md and lint also surface these waits; a timer is informational. Nothing is next while `LIMITS.toml` is paused. `tags` is in version order. Given a step without `--act`, it refuses (exit 1) unless that step is next, naming every unmet gate, and lists a refused step under `waiting`, never `steps`. Mac-lane steps stay in the Mac queue. A named Mac step reports `mac-lane-dispatch`. |
| `cm sync [--budget-secs N]` | Prints the same unread owner-queue banner as `next`, without acknowledging. Reads GitHub and tap formulas with read-only `gh api` calls, and declared crates and images with read-only `curl` requests, each killed after 60 seconds, without holding `.cm/lock`, up to eight at once; then it takes the lock, reloads the state and applies the moves that still hold. For launched Codex and Claude Cloud steps, it observes configured turn readers and looks for open PRs by recorded branch or a completed turn’s returned PR URL. With a verified PR URL and head SHA, it moves the step to `pr-open`. It also moves `pr-open` → `merged` when the PR merged (the merge commit and GitHub's optional `merged_at` are the evidence; `since` remains the sync time), a release step `merged` → `released` when its declared tag exists in its repository and contains the prep merge and every declared artifact is published at the release version (see [Releases](#releases)), any other non-rollout step `merged` → `released` by the first release of its repository whose tag contains its merge, and `released` → `deployed` once a `deployed` release of the same line contains its merge. One journal entry lists the moves, new notifications and unreadable publication observations; a sync with none journals nothing, nor does one whose only notes are unreadable publications already journaled and unreadable at every sync since that read them, which `.cm/unreadable-notified.json` records (each is still printed and a `sync-unknown`). A merged release whose publication goes unread, because it lacks its prep PR or merge commit, its prep PR is in another repository than the one it ships from, its tag read fails, or its tag is absent or lacks the merge, keeps its record until the publication is read again. A release that leaves `merged` (blocked, or released) drops its record, so one blocked and then unblocked while its publication stays unreadable journals the identical entry again. A `.cm/unreadable-notified.json` that cm cannot parse (empty, cut short, or another shape) stops the sync with an error naming the file before it writes any file but the lock it takes (`.cm/lock`, and `.cm/.gitignore`, made only when missing). Unreadable PR and tag observations alone add no journal entry. It refreshes the marked status span in `RESUME.md` from state (step counts, newest released and deployed version per release line, and epoch status), preserving all owner prose and silently leaving files without markers alone. It renders `CARD.md` with no run stamp, titled by EPOCH.md's `epoch` slug (else `campaign`, never the folder's name); the clock changes it only across a threshold (a stall limit, a watch deadline, the 24 hours a withdrawn watch stays listed). CARD also shows the actionable count, its oldest timestamp and the [guarantee lines](#card-guarantees). Sync keeps in `.cm/pulls.json` what it saw of each open PR (head, base, `mergeable_state`, last completed CI head/base; a PR GitHub did not answer for keeps its last observation and stamp while its step rests at `pr-open` on it), and reports `stalled` and `dirty` as `cm next` does. An observation is replaced only when its head, base, `mergeable_state` or CI head/base changes, so its stamp is when sync first saw it. Sync writes each file, `CARD.md` included, only when its bytes change: a sync that finds nothing new writes no file and journals nothing. An open PR read sends its stored ETag within 30 minutes of the PR's last full read, where an unchanged response can reuse the observation; past that, open PR fields are read in full. Pending CI pages send theirs while the PR's head is the stored one, at any age. A PR's ETag is sent only while `.cm/pulls.json` holds the observation it validated; a CI page's ETag is keyed by that page's own head-SHA URL and kept only for a page with a pending check. Sync keeps each open PR's last full read time and ETags in `.cm/pulls-read.json`, which it lists in `.cm/.gitignore`; without it, as in a fresh clone, every open PR is read in full once. A separate `rate_limit` read, after the other reads and before taking the lock, samples core used (counting this sync's reads), limit and reset (Unix seconds) and prints it (JSON `rate_limit`). The sample has a five-second timeout and is omitted if unavailable. A sync that fails, or takes longer than `--budget-secs` (default 30), prints its wall time and each phase's (`phase read: …`, with the forge reads it made; JSON `timing`), or prints them on stderr when it stops with an error. A model-lane step (`codex`, `claude-cloud`, `fable`, `coordinator`, other than a script pin) whose PR merged moves to `merged` only when its merge gate holds at the PR's head; otherwise it stays `pr-open` and each unmet condition is a `merge-gate` violation. A PR closed unmerged (`pr-closed`) or an observation `gh` or `curl` could not make (`sync-unknown`) exits 1 after the rest is applied. |
| `cm epoch planning\|working\|closed [--skill-revision R] [--plan-revision S]` | Moves `EPOCH.md`'s front matter one status along `OPEN → PLANNING → WORKING → CLOSED`, stamps `planning_started_at`, `working_started_at` or `closed_at` with the date, sets the pins it is given, journals the move, and refreshes RESUME.md’s generated status span. It refuses a null pin: PLANNING needs `skill_revision`, WORKING and CLOSED also `approved_plan_revision`. `closed` follows `cm close`. |
| `cm idea admit <id>` | Admits an idea to this epoch, appends its id to `EPOCH.md`'s `ideas:` list in admission order, and journals the action. Repeating it completes interrupted writes without duplicating entries. |
| `cm idea move <id> <maturity>` | Moves an admitted idea forward through `captured → exploring → designed`; skipping a maturity is allowed. |
| `cm idea outcome <id> <word> [--to unassigned] [--by ID] [--note TEXT]` | Records one outcome per idea per epoch: `landed`, `partial`, `deferred`, `superseded` or `cancelled`, and journals the action. Partial/deferred remainders become unassigned. |
| `cm idea facts --pin ALIAS=DIR@SHA… [--out F] [--ideas DIR] <id>…` | Writes a review pass's `FACTS.md`: verifies every pin with `git cat-file -e` (any missing one is `pin-missing` and nothing is written), then lists each record's file citations resolved at the pins, `ok` or `STALE`, with the enclosing heading or item. See [Review tools](#review-tools). |
| `cm idea idents --base REV [--ideas DIR] <id>…` | Checks the backticked identifiers, env vars and `IDEA-NNNN:line` refs a record gained since `REV` against the records it links: `exact`, `MISMATCH` (only inside a longer token) or `absent`, and prints each ref's target line. |
| `cm idea split-history [--apply] [--min-bytes N] [--date D] [--ideas DIR] [<id>…]` | Moves ruled-on review sections of records over N bytes (default 20,000) to `history/<id>.md`, leaving heading stubs; prints what would move unless `--apply`. Idempotent. |
| `cm read [--pins F] [--pin ALIAS=DIR@SHA]… <spec>…` | Prints files at pinned commits, never the working tree, every line numbered: whole up to 8 KB, an outline beyond that unless the spec names a range or heading, about 64 KB per call with every cut named. See [Review tools](#review-tools). |
| `cm journal "<title>" [--body-file F]` | Appends `## YYYY-MM-DDTHH:MMZ — <title>` and the body (`-` reads stdin). The only way `JOURNAL.md` grows. A body may not hold a `#` or `##` heading. |
| `cm launch <NN-name>/<unit>/<step> [--worktree PATH] [--coordinator LABEL] [--cap-min N]` | Refuses a `mac`-lane step with `mac-lane-dispatch`. Launch checks cover `LIMITS.toml` not paused; a declared step's every `--after` met (`after-unmet`); for the local `pi` lane and undeclared steps, free disk on `disk_volume` minus `kache_headroom_gib` over `min_free_disk_gib`, live heavy builds (top-level `cargo` processes) under `max_heavy_builds` and the worktree, which they require; the step folder has `kickoff.md`; every path its packet names exists. A drafted step, a step blocked before its PR opened, or a launched step whose last attempt has ended can launch; relaunch picks the newest `resume*.md` when present and increments `launches`. The `--after` check runs on every launch, including relaunches. A release step also refuses with `owner-queue-unread` while valid new queue items await `cm owner-queue read`. Then it marks the step (and a drafted unit) launched, journals the launch and, for a local step, prints the command; a remote launch prints the step and its prompt and records its attempt (see [Attempts](#attempts)). The launch report includes the sampled GitHub REST core usage when available. It spawns no lane work. When `--coordinator` is omitted, the label defaults to trimmed `CLAUDE_CODE_REMOTE_SESSION_ID` with a leading `cse_` mapped to `session_`, else `—` when unset or whitespace-only; an explicit label wins. Launch refuses an empty or multi-line label, explicit or inherited. A step whose last attempt has not ended does not launch again (`attempt-open`). |
| `cm attempt <NN-name>/<unit>/<step> [--claim TOKEN] [claim [--coordinator L] --expect G \| begin KIND \| record KIND ID \| restart --lane L [--evidence PATH] \| end complete\|partial\|unknown [--evidence PATH]]` | Shows a remote attempt and what to do next from committed state, or extends it; each change is journaled. `end` includes the sampled GitHub REST core usage in its journal and text output when available. Launch and attempt quota reads time out after five seconds. See [Attempts](#attempts). |
| `cm lane driver add <NAME> --session LABEL --unit PREFIX… [--coordinator L]`, `cm lane driver list`, `cm lane driver remove <NAME> [--coordinator L]` | The registry of lane drivers in `.cm/lane-drivers.json`: `add` records that session `LABEL` drives lane `NAME` (the `<name>` of `LANE-<name>.md`, not a step's `--lane`) for the steps under each `--unit <NN-name>[/<unit>[/<step>]]`, replacing the lane's earlier driver; `remove` drops it; both are journaled with the registering or removing label. `list` prints `<name>: <session> for <units> (registered <stamp> by <label>)` per lane and journals nothing. See [Lane drivers](#lane-drivers). |
| `cm unblock <NN-name>/<unit>[/<step>] [--evidence PATH]` | Releases a `blocked:<reason>` track to the state it was blocked from. Omit `--evidence` only for a step whose `blocked:until:<condition>` holds; the condition is journaled. Other blocks require a campaign evidence file, as for `done`. A step that rests at `drafted` or `launched` still waits on every `--after`: `cm next` and `cm launch` check them again, and the move lists what is still unmet. |
| `cm amend <NN-name>/<unit> --priority now\|whenever` | Sets a unit's priority, journaled: lines of `now` units sort first in `cm next`, `--act` and CARD, ahead of their waiting counts. The default is `whenever`; the same priority again is `amend-empty`. Given a step with no step flags, it amends the step's unit; beside step flags it changes the unit in the same amendment and journal entry. A unit given step flags is `amend-target`. |
| `cm amend <NN-name>/<unit>/<step> [--study] [--lane L] [--after A]… [--drop-after A]… [--part NAME]… [--kickoff-file F] [--github OWNER/NAME] [--tag PATTERN] [--artifact SPEC] [--drop-artifact SPEC]` | Changes a declared step that rests at `drafted` or `launched`: marks it a study, moves its lane, or removes gates with `--drop-after` before adding gates with `--after` (each declared, never closing a cycle: `after-cycle`). Removing a gate it lacks is `after-absent`. Moving a rollout to `mac` is `mac-lane-rollout`. `--kickoff-file F` replaces `kickoff.md` from a non-empty file of at most 1 MiB (`-` reads stdin) and journals one line; it refuses a step past launched. `--github` and `--tag` change a release step's repository and tag pattern through `pr-open` and `merged`, refusing once released; its prep PR must be in that repository. `--part` declares required parts (see [Parts](#parts)); given alone it also amends a step that has moved on, short of final. Journals the change; a change that changes nothing is `amend-empty`. |
| `cm advance <NN-name>/<unit>[/<step>] <state> [--sha S] [--pr URL] [--tag T] [--digest D] [--evidence PATH] [--by STEP[,STEP…]] [--repo PATH] [--head SHA] [--ci-verified PATH] [--override PATH] [--part NAME]` | Refuses merged moves in every lane/part and all release-step lifecycle moves with `owner-queue-unread` until explicit `owner-queue read`; no override. Moves one state along `drafted → launched → pr-open → merged → released → deployed → done`, or to `blocked:<reason>`; a `mac`-lane step, a non-rollout `owner`-lane step, a study, or a non-release `coordinator`-lane step without a rollout, recorded PR, SHA or declared parts moves to `done` with `--evidence` (see [Finishing](#finishing)). `superseded --evidence PATH [--by STEP[,STEP…]]` retires a step replaced by other work from any state short of final, recording its declared successors in state and the journal and showing them in `STATE.md` and `CARD.md`; it meets no gate, so its consumers are amended. A release step moves to `released --tag T --sha S` from `drafted` or `launched` when the tag was cut without a prep PR (tag-only); T must contain S. Other releases require T to contain the recorded merge SHA (`tag-lineage`). A step declared with `--kind rollout` moves `merged → deployed --sha S --evidence PATH` without a release or digest; PATH names the Argo sync revision (see [Finishing](#finishing)). Refuses any other skipped state and a state without its evidence, journals the move and re-renders `STATE.md`. A model-lane step other than a script pin moves to `merged` only through its merge gate (see [Merge gate](#merge-gate)). An already `merged` release step accepts `merged --sha S` again when S descends from its recorded merge, updating only its evidence SHA and journaling both SHAs; otherwise it refuses (`merge-lineage`). `--part NAME` moves one part of a multipart step (see [Parts](#parts)). A step with an unresolved watch does not move to `deployed` or `done` (`watch-open`, see [Watches](#watches)), and `done --evidence` refuses a stale pinned file (`evidence-stale`). |
| `cm known-red <step> --commit SHA --check NAME --reason T --owner WHO --until A` | Records a known-red exception for the step's merge gate (see [Known-red exceptions](#known-red-exceptions)) and journals it. |
| `cm watch <step> add --signal span:Q\|log:Q\|metric:Q --deadline T`, `cm watch <step> resolve ID --evidence PATH`, `cm watch <step> withdraw ID --reason TEXT`, `cm watch <step> amend ID --signal SIGNAL` | Records a pending live read that holds the step below `deployed`, closes it with evidence or a withdrawal reason, or corrects its signal (see [Watches](#watches)). |
| `cm close` | For an open campaign, requires a clean `cm lint` first (warnings do not block), then seals the campaign. Refuses, listing every gap, while `OWNER-QUEUE.md` or `MAC-LANE.md` has an open item, `MAC-LANE.md` has a numbering gap or repeat (`mac-lane-number`), any unit or step is short of `done` or `blocked:<reason>`, or an id in `EPOCH.md`’s `ideas:` list has no outcome for this epoch (`idea-outcome-missing`, one gap per id). Then it stamps `closed` in `.cm/state.json`, journals the close with one `- idea: <id> <word>[ to <target>\| by <id>]` line per admitted idea in list order, and refreshes RESUME.md’s generated status span to show closed. From there unit `cm init`, `cm launch`, `cm advance` and a second `cm close` refuse with `campaign-closed`; `cm journal` stays open for follow-up observations. |
| `cm feedback "<title>" --urgency urgent\|queued --body-file F` | Appends `### FB-<sequence>-NNN · <urgency> · <title>` and the body to `FEEDBACK.md`, numbering on from the highest id of the letter in `EPOCH.md`'s `sequence:`, and journals a line naming it; an empty body is `feedback-empty`. `--follow-up <id> --disposition proposed\|accepted\|applied\|deferred\|rejected` appends `### <id> follow-up · <disposition>: <title>` to an existing id. Hand-written entries (dated `##` sections, `FB-` sub-entries) still parse. Log feedback whenever a file would otherwise be appended by hand. |
| `cm owner-queue read` | Prints the valid queue and journals an explicit acknowledgement of its current level-2 headings in optional campaign `owner_queue_read` state. Only this command acknowledges. A new heading since the last read is unread; body-only edits are not new. Initially all valid open headings are unread, including unnumbered headings. Missing, unreadable or queue-lint-failing files create no new items or new unread refusal; an invalid explicit read does not acknowledge. |
| `cm owner-queue add "<title>" --context T --question T --recommendation T [--block STEP]` | Appends `## N. <title>` (numbering on from the highest item in the queue or any journaled owner-queue add, answer or strike; numbers are never reused), its `Context:` line, the question, a `Recommendation:` line and an empty `Answer:` to `OWNER-QUEUE.md`; refuses an empty field or a RESUME.md with no `>` banner (`resume-banner`), sets RESUME.md's banner to `OWNER-QUEUE not drained: /loop withheld` and journals the item. A leading `Recommendation:` in `--recommendation` (any case) is stripped once. `--block STEP` also moves STEP to `blocked:until:owner-queue:N` on the new item through `cm advance`'s own checks, under the same lock; when the block refuses, nothing is filed. `cm owner-queue answer N --body-file F` journals a non-empty answer and removes item N; `cm owner-queue strike N --reason T` removes it and journals the reason; `cm owner-queue edit N --body-file F` replaces its body, keeping the heading and requiring the item fields. `-` reads either body from stdin. Unknown numbers are refused; other items keep their numbers. When a removal drains the queue, the withheld banner line is removed and the session is told to print the resuming `/loop`. |
| `cm mac-lane add "<title>" [--body-file F] [--step STEP]`, `cm mac-lane partial N --receipt T`, `cm mac-lane done N --receipt T`, `cm mac-lane list` | The queue of work only the owner's Mac can do, `MAC-LANE.md`: one numbered list item per request, starting at column 0: `N. **<title>** <body>`. `add` appends the next number (creating the file with `# Mac lane queue` when it is missing), indents the body's continuation lines four spaces and journals the item; a title holding `**` or beginning with a status word (`DONE`, `WITHDRAWN`, `HELD`, `RUNNING`, `PARTIAL`, `Receipt`) is `mac-lane-title`, so a new request cannot be mistaken for a status. An exhausted item number is `mac-lane-number`. With `--step <NN-name>/<unit>/<step>`, `add` appends an indented ``Step: `<NN-name>/<unit>/<step>` `` line to the item and records the step in the add journal entry. An undeclared step is `step-unknown`. An item is done once its leading bold is `**DONE …**` (or `**WITHDRAWN …**`); any other item is open, `held` (a leading `**HELD …**` or a `**Receipt …**` naming a hold), `running` (`**RUNNING …**`) or plain `open`, and its title is its first other bold span. `partial` writes `**PARTIAL <YYYY-MM-DD>** (<receipt>). `, keeps the original instructions and leaves the item open with state `partial`. Parentheses and backslashes in its receipt are Markdown-escaped. `done` replaces the complete partial status and receipt, then writes `**DONE <YYYY-MM-DD>** (<receipt>). ` after `N. `, replacing a leading open status, keeps the remaining item text, trims trailing whitespace on a status-only first line and journals it; an absent item is `mac-lane-item`, a done one `mac-lane-closed`. Use `cm advance` separately to finish the linked step. `list` prints each open item as `N. <title> (<state>)` (`items` in `--json`). Linked items append ` — <step>` in text and include a `step` field in JSON. `cm sync` summarizes the open count and at most 10 items under `## Mac lane` on `CARD.md`, with titles limited to 80 characters and a pointer to `MAC-LANE.md` for the rest; omitted when none is open. Unreadable queues fail the read, report `mac-lane-unreadable` in lint and refuse campaign close. An unreadable or invalid lane leaves sync running and renders `Mac lane: invalid, see cm lint` on CARD.md. |
| `cm evidence <NN-name>/<unit>/<step> --name NAME --body-file F [--candidate STEP[@SHA] [--harness STEP[@SHA]]]` | Writes `<step>/evidence/<NAME>.md` once from a non-empty body and journals the path. `cm advance <step> done` without `--evidence` takes the step's one evidence file when it has exactly one. `--candidate` and `--harness` pin the file to the commits it was produced against (see [Pinned evidence](#pinned-evidence)). |
| `cm review <step> --pr URL --head SHA --model ID --effort E --verdict accept\|fix-required --findings C/F --cost USD --wall MIN [--delta FROM_SHA]`, `cm review --import` | Appends one row to the review ledger `.cm/reviews.json` and renders `REVIEW-OFF.md` from it, rows in PR order with a delta review under its PR, and journals the row. The rows a hand-written `REVIEW-OFF.md` already holds are imported once, kept cell for cell, and the text before and after the table (per-PR adjudications, rules) is kept byte for byte (`--import`, or the first `cm review`). The step must be declared (`step-unknown`); an imported delta row is the one whose `#` cell carries `Δ`. After that a hand edit is `review-hand-edit`, as for `STATE.md`. |
| `cm review wait <step> --head SHA --reviewer KIND --deadline RFC3339 [--part NAME]` | Blocks until `.cm/verdicts.json` records the named round: step, current PR, part, reviewer and head (either verdict). Re-reads the step’s current PR and review ledger once per second without holding the campaign lock; missing ledger means empty. Exit 0 / `wait_status: recorded`, 3 / `deadline`, 4 / `unreadable`; Invalid head/reviewer arguments and unknown step/part scopes exit 2 (`refused` for scope refusals); This command alone uses strict campaign discovery: read/decode errors and invalid campaign state exit 4, including a malformed inner campaign without falling through to an outer campaign; a genuinely absent campaign exits 2. Unreadable or malformed declared unit state or review ledger also exits 4. Writes nothing. Run it as a background command to await a review instead of polling. |
| `cm review record <step> <report-file> [--reviewer KIND] [--part NAME]` | Parses the `cm-review` block that ends the report (see [Review verdicts](#review-verdicts)), computes the verdict, copies the report to `<step>/evidence/reviews/R<n>[-KIND].md` and records the review in `.cm/verdicts.json` as `R<n>`, its findings as `R<n>.<id>`. Refuses a report with no block or a malformed one (`review-block`, one per problem, with its line), an undeclared step and a report already recorded for the step (`review-duplicate`). A report whose own verdict line disagrees with the computed one is recorded with the computed verdict and the disagreement printed and journaled. `--part` selects a declared part and its PR; it is required on multipart steps. Distinct FIX REQUIRED heads on the current PR count toward the ten-pass step cap; rising blocking counts request an independent reader and only a non-converging `fresh-opus` round blocks the step (see [Rounds](#rounds)). |
| `cm review resolve <step> <finding> --by <commit\|ruling-file>` | Records what resolved a blocking finding: `R<n>.<id>`, or `<id>` in the step's latest review holding it; an id whose `R<n>` is not a review number names no finding (`finding-unknown`). `--by` is a ruling file in the campaign when that path exists, else a commit SHA. A follow-up is `resolve-not-blocking`; a second resolution is `resolve-done`. |
| `cm ruling <step> "<text>"` | Appends `- YYYY-MM-DD: <text>` to `<step>/evidence/coordinator-scope.md`, creating it with `# Scope and coordinator rulings: <step>` when it is missing, and journals the line. |
| `cm brief <step>` | Prints the step's `kickoff.md`, then `## Coordinator scope and rulings` and the step's `evidence/coordinator-scope.md` (or a line saying none is recorded): the brief for `astra-review.sh start` and review packets. `--json` puts it in `brief`. Refuses a step with no `kickoff.md`. |
| `cm followups [--epoch]`, `cm followups close <id> --by <commit\|reason>` | Lists every non-blocking finding `cm review record` kept, across steps: id, step, PR, type, origin, `file:line`, summary and status (`open` or `closed`). `--epoch` lists only the open ones, grouped by repository as Markdown checklist lines, for the epoch's cleanup PR. `close` closes one with the commit that fixed it or the reason it stays, and journals it; a blocking finding is `followup-blocking`, a closed one `followup-closed`. |
| `cm tick [--checkout DIR] [--cm PROGRAM] [--check-bound]` | The coordinator's routine moves once, as tools/cloud-env/heartbeat.sh made them. In the campaign-trail checkout (default: the current directory) it chooses the one epoch whose `store/epochs/*/EPOCH.md` has a line starting `status: PLANNING` or `status: WORKING`; any other count prints `heartbeat: N PLANNING or WORKING epochs, not one; …` and stops before running anything. Then the bound check runs, and only in the bound coordinator, in heartbeat.sh's order: git sync, the pinned cm, unpicked owner directives, collect, the act loop, git sync again when an executor ran, and the CLAUDE.md import. Any other session (a lane session, a retired coordinator, a successor before its start proof) gets silence: nothing after the epoch choice is printed (not the owner-queue line, not an act line, not a bad `HEARTBEAT_DEADLINE`), no phase and no cm runs, and it exits 0; `--json` prints the object of a tick with no lines. The session is bound when the checkout is on `main`, `CLAUDE_CODE_REMOTE_SESSION_ID` is set and non-empty, and `sh tools/cloud-env/handoff-pick.sh <epoch> coordinator --lineage session_<id> --max-age-hours 24`, `<id>` without a leading `cse_`, exits 0 or 6 (its `ok` and `stale`) with a first line starting `ok ` or `stale ` (age never unbinds: an unsuperseded head of any age is still this coordinator's; any other exit status is a failed pick, whatever it printed), and then origin agrees: when `git fetch -q origin main` fails, or `git diff --name-only -z --no-renames --diff-filter=A HEAD origin/main -- <epoch>/handoff/` fails, it is bound, and so it is when that list is whole (4 MiB at most) and adds no `coordinator-*.md`, in the folder or below it; otherwise, a diff list cut at its 4 MiB head included, origin's `<epoch>/handoff` is read whole: `git archive` and `tar` into a temporary folder in `TMPDIR` (`/tmp` when it is unset, empty or not UTF-8) that is removed after, every file `git ls-tree -r -z origin/main -- <epoch>/handoff` lists must be in that copy byte for byte (`git hash-object --no-filters` of each file, or of a symlink's target, against its object; a submodule has nothing to compare and the executable bit is no part of a file's bytes), and handoff-pick.sh judges the copy as it judged the checkout. An archive leaves out what origin's `.gitattributes` marks `export-ignore` and applies its `export-subst`, `ident`, `text eol=` and `working-tree-encoding` attributes and any `filter` whose smudge command the checkout configures, so a copy that lacks a file, or holds one whose bytes differ from origin's blob (a bystander beside the handoffs included), counts as unbound, even where handoff-pick.sh would read the converted copy alike: it fails closed. The archive runs as `git -c core.autocrlf=false -c core.eol=lf -c core.attributesFile=/dev/null archive …`, so neither the checkout's eol settings nor its `core.attributesFile` (one it sets, or the default `$HOME/.config/git/attributes`) changes the copy. The checkout's own `.git/info/attributes` still applies, and no flag turns it off: one that converts or drops a handoff fails closed the same way. A failed archive, extract, listing or hash, origin's listing cut at its 4 MiB head (so a diff list past 4 MiB is never bound: origin's listing of the same folder holds each of its names, and more bytes for each), a file missing from the copy, a `TMPDIR` that cannot hold the folder, and a relative `TMPDIR` (heartbeat.sh resolved it in the checkout; cm makes nothing there) are unbound. The added names are read NUL-separated, so a name git otherwise quotes (a non-ASCII one, or one holding `"`, `\`, a tab or a newline) is matched too, and without rename detection, so a successor's handoff git would report as renamed from another file counts as added (heartbeat.sh missed both). Each command of the check runs under a 30 s cap: a fetch or diff that times out counts as failed (bound), a branch read, pick, archive, extract, listing or hash as unbound. The check runs again after each git sync (a successor's handoff pulled just now retires the session), and a session it no longer binds stops there: the lines that git sync printed stay, its wall-time line and everything after it do not. `--check-bound` runs only the check, for the Stop hook: it prints nothing, with or without `--json`, and exits 0 when bound and 1 when not or when there is not exactly one live epoch (an `EPOCH.md` it cannot read is skipped, as heartbeat.sh's grep skipped it, where a tick names it and stops); it takes `--checkout`, and accepts `--cm` without running it. The act loop reads `cm --json next -C <epoch> --act` by argv from this executable, or from `--cm PROGRAM`, resolved where it was given (a path made absolute; a bare name looked up on `PATH` as a shell would, taking the first regular file the effective user may execute, an empty or relative entry read from the directory `cm tick` started in; a name `PATH` does not hold from there is left bare, and the OS then searches `PATH` again from the checkout when the child starts, so an empty or relative `PATH` entry can find a cm there; with `PATH` unset a bare name is left bare for the C library's default search path, and a relative path given in a start directory that no longer exists stays relative, so it names a file in the checkout), which the executors' cm calls also use. Each line goes to its executor, `<kind>.<lane>` then `<kind>`, the lane taken from the line or else the step's state; a line with no executor is printed as `<kind> <target>[#<part>]: <command>`. The act list is held in memory up to 64 MiB, as heartbeat.sh held it, so while memory allows it needs no disk; past that, or once memory runs out, it spills to an unlinked temporary file in `TMPDIR`, which a killed tick leaves nothing of. It is read twice, once to check the whole document and then one line at a time, both passes parsing it alike, so a list of any size is acted on in full and a list that is not valid JSON prints one `heartbeat: cm --json next --act …` line and no act line. A list too large to keep in memory (past 64 MiB, or past what memory allows) that cannot be stored prints `heartbeat: cm --json next --act's list was too large to keep in memory and could not be stored in <TMPDIR>: <error>; no act line was read`. While `owner_queue_unread` is not empty it prints `owner-queue: N unread item(s); …` and leaves `merge-ready` and `review-owed` lines unrun. Each executor gets `min(120, deadline − elapsed)` seconds (deadline 230, `HEARTBEAT_DEADLINE` overrides); its children run in process groups of their own, sent TERM at that budget and KILL once the group has had 5 s to finish (even after the group leader exits), and the line then ends `[tick/<key> timed out after N s and was killed; check what it started]`. A failed move keeps its line once, ending `[tick/<key> failed: <reason>]`; once the budget is spent the remaining lines end `[not run: heartbeat time budget spent]`. A phase's wall time, `tick: <phase> phase took S s (budget B s)`, is printed only when a housekeeping phase failed or had a child cut at its budget, or any phase ran a whole second past its budget; a failed `cm next` prints only its own line. Exits 0 whenever the loop ran (`--check-bound` exits by its answer), also when it stopped at the epoch choice or stayed silent, and when its stdout is closed or is a file at its `RLIMIT_FSIZE` (lines that would pass it are not written, since SIGXFSZ would end the tick); a missing or misshapen `store/epochs` or `EPOCH.md` is no epoch, as heartbeat.sh's glob matched none, but one whose read fails (EMFILE, EACCES, ELOOP) is the one line `heartbeat: could not read <path>: <error>; stay read-only until cm, PR and journal state are reconciled`; `-C` is a usage error (exit 2). A control character (a line break, a tab, an escape, DEL) in text the tick prints, from a path, a child, the act list or the environment, is escaped as Rust writes it (`\n`, `\u{1b}`), so each line stays one line; other valid UTF-8, non-ASCII included, is printed as it is, and a byte that is not valid UTF-8 prints as U+FFFD. An executor's `Done` lines are joined with `\n`, trailing newlines are dropped, and the rest is split at each `\n` into lines of their own, then escaped; its `Left` text and the last line of its `Failed` reason are escaped whole. `--json` puts the lines in `lines` and the cm it read from in `cm`. A kill of `cm tick` itself does not reach a running child's own process group, so the Stop hook must bound `cm tick` by session. |
| `cm lint [DIR]` | Template conformance, cm-owned files against `.cm/state.json`, the journal against git, `.cm/JOURNAL.md` and `.cm/lock` out of git (`cm-tracked`), the path check for every drafted or launched packet, a unit or step folder git ignores (`git-ignored`, or `git-ignored-unknown` when `git check-ignore` cannot answer, as outside a repository), and a unit folder holding `STATE.md` but no `.cm/state.json` (`unit-state-json-missing`), which is never counted as a smaller campaign; unique `FEEDBACK.md` ids whose follow-ups name existing ones; `MAC-LANE.md` items numbered 1, 2, 3… with no gap or repeat (`mac-lane-number`), nothing about their text; a `.cm/lane-drivers.json` cm can parse (`lane-drivers-unreadable`); `REVIEW-OFF.md` against its ledger; stale pinned evidence (`evidence-stale`). It warns, without failing, on `stale-base` and a blocked-reviewer timer, on an expired watch and on an `--override` of a red check no known-red exception names. In a closed campaign, also an empty owner queue, no open Mac lane item and every track done or blocked; its packets are no longer checked, since nothing launches there. |

## cm tick

`cm tick` (`src/tick/`) is tools/cloud-env/heartbeat.sh in Rust, and like it acts on what `cm next --act` emits. A tick runs these steps in order:

1. Choose the epoch, stopping before any child runs unless exactly one is live.
2. The bound check; any other session stops here and prints nothing.
3. `gitsync` (`Mode::Always`), then the bound check again.
4. `pin`, `directive`, then `collect`.
5. Read `cm --json next --act` and run the act loop.
6. `gitsync` (`Mode::IfDirty`) when an executor ran, then the bound check again.
7. `claude_md`.

`Ctx::still_bound()` runs the same check afresh, nothing cached, and spends none of the calling phase's budget: its time, whatever the check answers and also when it panics, moves that `Ctx`'s deadline out by the check's whole time and is left out of the phase's wall time. Each call moves the deadline by its own check's time, so a phase that calls it N times can run N checks longer. gitsync calls it after each successful pull, before the push: once a successor's handoff has arrived it pushes nothing and prints nothing, as heartbeat.sh's `is_bound || exit 0` did (`a_pull_that_lands_a_successor_s_handoff_pushes_nothing_and_says_nothing_where_its_twin_pushes`). The frame's re-check then runs the check again: unbound, it stops the tick; bound (the check answered differently within the tick), the tick goes on and the commit is pushed the next turn (see the table below).

**Where `cm tick` and heartbeat.sh differ.** B is bound, U unbound; each row's test shows the `cm tick` side.

| Case | heartbeat.sh | `cm tick` | Test |
|---|---|---|---|
| handoff-pick.sh prints `ok ` or `stale ` but exits with another status than 0 or 6, or by a signal | B | U | `an_unbound_session_prints_nothing_and_runs_only_the_bound_check` |
| the branch read, handoff-pick.sh, or origin's archive or extract hangs | hangs | U after 30 s | `an_unbound_session_prints_nothing_and_runs_only_the_bound_check` |
| `git fetch` or `git diff` hangs | hangs | B after 30 s | `executors_run_only_in_the_bound_coordinator` |
| `git ls-tree` or `git hash-object` hangs | runs neither | U after 30 s | `an_unbound_session_prints_nothing_and_runs_only_the_bound_check`, `the_extract_is_hashed_in_chunks_and_every_object_must_match` |
| a successor whose name git quotes (`é`, `ü`, `"`, `\`, a tab, a newline) | B | U | `the_built_cm_tick_reads_a_non_ascii_coordinator_handoff_name_on_origin` |
| a successor git reports as renamed in | B | U | `the_built_cm_tick_counts_a_successor_handoff_renamed_in_on_origin_as_added` |
| a relative `TMPDIR`, when origin is read | B (the copy made in the checkout) | U | `the_built_cm_tick_refuses_a_relative_tmpdir_and_makes_nothing_in_the_checkout` |
| a `TMPDIR` that is not UTF-8 | the copy made there | the copy made in `/tmp` | `a_tmpdir_that_is_not_utf8_is_read_as_unset_so_the_extract_goes_to_tmp` |
| origin's attributes change the copy: `export-ignore`, `export-subst` (on a successor or a bystander), `text eol=crlf`, `ident`, a `filter` the checkout gives a smudge command, `working-tree-encoding` | B | U | `the_built_cm_tick_fails_closed_when_export_ignore_hides_a_successor_handoff_on_origin`, `the_built_cm_tick_fails_closed_when_export_subst_moves_a_successor_header_on_origin`, `the_built_cm_tick_fails_closed_when_an_eol_attribute_converts_origins_handoffs`, `the_built_cm_tick_with_the_real_handoff_pick_judges_origins_untransformed_folder`, `the_built_cm_tick_fails_closed_when_an_attribute_rewrites_a_bystander_on_origin` |
| the checkout's `.git/info/attributes` converts (`*.md text eol=crlf`) or drops (`export-ignore` of that one file) this lineage's newer handoff on origin | B | U | `the_built_cm_tick_fails_closed_when_the_checkouts_info_attributes_change_the_archive` |
| origin's handoff listing past 4 MiB, and so a diff list past 4 MiB | B | U | `the_built_cm_tick_is_unbound_when_origins_handoff_listing_passes_4_mib` |
| a successor's handoff that arrives with the `IfDirty` git sync | no check after it | stops there, before the CLAUDE.md import | `the_frame_re_checks_after_each_gitsync_and_stops_once_a_successor_appears` |
| a `HEARTBEAT_DEADLINE` that is not a whole number, in the bound coordinator | no line about it | `heartbeat: HEARTBEAT_DEADLINE=<value> is not a whole number of seconds; using 230` | `an_executor_gets_120_seconds_or_what_is_left_of_the_deadline` |
| an `EPOCH.md` that cannot be read, beside the one live epoch | skipped | `--check-bound` skips it; a tick prints `heartbeat: could not read …` and runs nothing | `check_bound_skips_an_epoch_md_it_cannot_read_as_heartbeat_sh_does` |
| the budget ends gitsync's tries before the third | `failed 3 times` after three tries | the same line after fewer | `once_the_budget_is_spent_the_abort_still_runs_and_no_try_follows` |
| a pull and a `git rebase --abort` that both ignore TERM, and a `still_bound` check whose fetch hangs | no deadline | gitsync ends at most 40 s past its 60 s budget plus its up to three checks' own time, 145 s past with three hung fetches | `gitsync_ends_at_most_40_s_past_its_budget`, `gitsync_ends_at_most_40_s_past_its_budget_and_the_time_of_its_three_checks` |
| a staged CLAUDE.md write, chmod or rename fails | `printf >` wrote in place | written in place after the failure | `a_staged_write_that_fails_as_on_a_full_disk_is_written_in_place` |
| a FIFO named like a coordinator handoff | grep blocks on it | skipped | `a_folder_or_fifo_named_like_a_handoff_is_skipped_though_heartbeat_sh_blocked_on_the_fifo` |
| an owner directive heading over 128 KiB | listed though picked up: exec refuses `grep -cF`'s argument | compared like any other | `a_heading_over_128_kib_is_compared_like_any_other` |
| the directive phase's reading | grep's, no deadline | in-process, no deadline; linear in the journal's bytes, with at most 64 pickup probes per byte | `every_journal_shape_costs_work_in_proportion_to_its_bytes` |
| pickups × distinct heading lengths past 64 probes per journal byte | one `grep -cF` per directive, however long | the scan stops, names the pickups it left (`heartbeat: pickups in JOURNAL.md from line L on (N line(s)) were not checked: …`) and lists every directive it has not found yet, so a directive is listed again rather than missed | `past_its_probe_budget_the_scan_names_the_pickups_it_left_and_lists_what_it_has_not_found`, `the_scan_stops_at_the_first_probe_past_its_budget_and_says_so` |
| the check answers unbound after a git sync's pull, then bound at the frame's re-check: its answer changed, or the re-check's own `git fetch` failed, which counts bound; after the `Always` or the `IfDirty` sync | exits, silent | the push is skipped and the tick goes on (directives, the act loop and its executors, the CLAUDE.md import); the commit goes with the next turn's push | `a_check_that_answers_unbound_after_the_pull_and_bound_at_the_re_check_leaves_the_commit_for_the_next_tick`, `a_re_check_whose_fetch_fails_counts_bound_after_a_check_that_retired_the_session` |
| a pull that retires the session | 2 handoff picks, before its sync and after the pull | 3, the frame's re-check too | `a_pull_that_retires_the_session_costs_three_handoff_picks_and_the_tick_says_nothing` |
| a journal that cannot be read (`ELOOP`, `EISDIR`), or a handoff folder that cannot be read | `no JOURNAL.md heading …`, or nothing for the folder | `heartbeat: could not read <path>: <error>; owner directives not listed` and the wall line | `a_handoff_folder_or_journal_that_cannot_be_read_is_named` |
| a stamp holding `.`, `*`, `[` or `\` | a BRE: `.` and `*` match more, `[` errors | matched as written | `a_stamp_is_matched_as_written_not_as_a_pattern` |
| a NUL in the journal or a handoff | grep takes the file as binary: `no JOURNAL.md heading`, or no stamp | a byte like any other, so the directives are listed | `a_nul_in_the_journal_or_a_handoff_is_a_byte_like_any_other` |
| a byte that is not valid UTF-8 in a printed heading or stamp | printed raw | printed as U+FFFD | `headings_and_pickups_are_compared_as_bytes_not_as_decoded_text` |
| the unpacked cm is a symlink, does not run, or does not print the pin | installed | refused: the running cm is kept and the line printed | `a_staged_cm_that_does_not_run_and_name_the_pin_is_never_installed` |
| CLAUDE.md's line 1 is the boot-time line with a NUL in it | `$(head -1)` drops the NUL: replaced | compared as bytes: kept | `a_claude_md_of_any_size_and_shape_is_read_no_further_than_one_boot_time_line` |
| a TMPDIR that does not exist, when the pin is fetched | `mktemp -d` fails, so the fetch writes `/<asset>` (left there when run as root) and the unpack fails: the line | nothing fetched or written: the running cm is kept and the line printed | `a_tmpdir_that_does_not_exist_refuses_the_pin_and_keeps_the_running_cm` |
| a control character (a CR, a tab, a NUL, an escape) in a printed line | printed raw | escaped as Rust writes it (`\r`, `\u{0}`), so each line stays one line | `text_from_a_path_the_act_list_or_a_child_stays_one_escaped_line` |
| a housekeeping phase fails or a child of it is cut at its budget | its line alone | its line and `tick: <phase> phase took S s (budget B s)` | `a_phase_that_fails_or_whose_child_times_out_prints_its_wall_time` |
| the cm to refresh is a bare or relative `--cm` | `command -v cm`, or `/usr/local/bin/cm` | nothing installed, the line printed | `a_cm_named_by_no_absolute_path_is_never_replaced` |
| the running version | `cm --version` from `PATH` | from the cm the act list is read from | `the_running_version_is_the_second_word_of_cm_version` |
| an asset name that is empty, `.`, `..` or holds `/` | fetched, and the tarball left in TMPDIR | refused before any fetch | `an_asset_that_is_not_a_plain_file_name_is_never_fetched` |
| two `cm` rows in `--list-pins` | the second row read into the asset name: nothing installable | nothing fetched, the line printed | `two_cm_pins_install_nothing_and_say_so` |
| a fetch past pin's 60 s budget, and setup.sh's own temporary folders | no deadline; its folders in the system TMPDIR | cut with its process group; its TMPDIR is the pin's folder, removed with it | `a_fetch_killed_mid_way_leaves_neither_our_folder_nor_setup_sh_s_own`, `the_pinned_cm_is_fetched_into_a_fresh_folder_under_tmpdir_that_is_then_removed` |
| the "more than one stamp_entry" line's order | `sort -u` in the locale's collation | byte order | `stamps_are_listed_once_each_in_byte_order` |
| HOME unset, at the CLAUDE.md import | `set -u` ends the script, exit 1 | nothing read or written | `without_home_nothing_is_read_or_written` |
| a boot-time CLAUDE.md of the tick's owner and group, with one link, in a folder it may write | `printf >` in place: the inode, xattrs and ACLs kept | staged and renamed: a new inode with the mode kept, xattrs and ACLs dropped | `the_boot_time_file_is_replaced_once_keeping_its_mode_and_any_link` |
| the import past `RLIMIT_FSIZE` | killed by SIGXFSZ, CLAUDE.md emptied | `could not write the COORDINATOR.md import …`, the old rules kept | `a_claude_md_import_that_cannot_be_written_is_one_line_and_keeps_the_old_rules` |

The checkout's `core.autocrlf`, `core.eol` and `core.attributesFile` leave `cm tick`'s copy as origin's, so a converting setting there unbinds nothing (`the_built_cm_tick_reads_origin_whole_whatever_the_checkouts_eol_config`, `the_built_cm_tick_reads_origin_whole_whatever_the_checkouts_core_attributes_file_says`).

`cm tick -h`, like the `cm --help` row, keeps the short sentence from before the bound check ("…then run each `cm --json next --act` line's executor in the bound coordinator, or print the line"), because that row is frozen; `cm tick --help` says what holds: nothing past the epoch choice is printed in any other session.

**Contract for the sibling cards.** Each card fills only its own files: the function body, and that file's `#[cfg(test)]` module, which it replaces. It keeps the file's `pub const BUDGET` (a phase) or `pub const KEY` and unit struct (an executor). Core's unit tests and the in-process tests in `tests/tick_beat.rs` run no-op or recording phases and a test-local registry, so they depend on no stub. The tests there that run the built binary (`the_built_cm_tick_*` and `a_killed_cm_tick_leaves_no_spilled_list_behind`) run `Phases::REAL`. Their spawns, the children of the in-process ticks that run real git, and the scripts the `SystemProc` tests start themselves get these environments:

| Spawn | Environment |
|---|---|
| `Harness::command`: the fake-git `cm tick` runs | emptied, then `PATH` set to the harness's `bin` folder, which holds a fake `git` (it prints `main` for `rev-parse`, `symbolic-ref` and `branch`, nothing otherwise, and exits 0) and a fake `cm`, then `/usr/bin:/bin`; `HOME` set to the checkout; `TMPDIR` set to the checkout's `tmp` folder (two tests set a missing folder or a file instead, on purpose); `CLAUDE_CODE_REMOTE_SESSION_ID=cse_01abc`. `the_built_cm_tick_names_a_spill_that_could_not_be_stored` replaces the fake `git` and `handoff-pick.sh` with ones that log nothing and name their files relative to the checkout |
| `Harness::limited`: the runs under a resource limit, and the one started in a deleted folder | the same through `bash`, which adds `PWD` and `SHLVL` (and `OLDPWD` after the deleted folder's `cd`); the file-size boundary test hands `bash` its output file and padding in `OUT` and `PAD` and unsets them before `cm` starts, and replaces the fake `cm`, `git` and `handoff-pick.sh` with ones that log nothing and name their files relative to their own folder or the checkout, so none dies at the limit, as does the stdout-file test, since the real gitsync's git calls would grow a log past it; the deleted-folder test replaces the fake `git` and `handoff-pick.sh` with ones that print `main` and the pick file and name no path |
| the symlink test and the `--cm` resolution tests | the same, with `PATH` entries of their own first; one unsets `PATH` and sets `GIT_DIR` to a one-commit repository on `main` in the checkout, pushed to a bare origin beside it, so the system `git` its default search path finds reads only that repository and the tick is bound, and replaces the fake `handoff-pick.sh` with one that names its log and pick files relative to the checkout |
| the real `git` that makes that repository | emptied, then `PATH=/usr/bin:/bin`; `HOME`, `GIT_CONFIG_GLOBAL` and `TMPDIR` in the folder that holds it and its origin; `GIT_CONFIG_NOSYSTEM=1` |
| the `cm init` calls two tests make to set up an epoch | the runner's |
| `Origin::command`: the real-git `cm tick` runs, the fixture's own `git`, `tar` and the real `handoff-pick.sh` some tests run beside the tick | emptied, then `PATH=/usr/bin:/bin`, so the real `git`, `tar`, `sh` and the `python3` the real `handoff-pick.sh` runs (one test puts a folder holding a `tar` that exits 1 first); `HOME`, `GIT_CONFIG_GLOBAL` and `TMPDIR` in the fixture's folder; `GIT_CONFIG_NOSYSTEM=1`; `TICK_FIXTURE` set to the fixture's folder, where its fake `cm` and `handoff-pick.sh` find their logs and list, so no path is quoted in a script; started in the fixture's folder. Tests override `TMPDIR` with `tmp` and `.` (started in the checkout), a missing folder and a file (an empty `TMPDIR` is a unit test of the folder it maps to, so no test writes in the runner's `/tmp`); set `CLAUDE_CODE_REMOTE_SESSION_ID` to `cse_01abc`, `cse_02next`, `01abc`, `cse_cse_01abc` or empty, or leave it unset; and set `HEARTBEAT_DEADLINE=soon` in one |
| `tick`, `tick_with`, `beat::run` and `beat::check`: the in-process ticks | an `Env` the test builds, with `TMPDIR` set to the fixture's `tick-tmp` folder (`in_fixture`), `CLAUDE_CODE_REMOTE_SESSION_ID` set to `cse_01abc`, `01abc` or empty, or unset, and `HEARTBEAT_DEADLINE` set to `0`, `-5`, `3`, `50`, `500`, empty or `soon` in some; the children they run through `Bin` or `SystemProc` get the runner's environment |
| `Isolated`: children of the in-process real-git ticks | the runner's variables removed and `Origin::command`'s set; the tick itself runs in the test process with an `Env` of `CLAUDE_CODE_REMOTE_SESSION_ID=cse_01abc` and `TMPDIR` set to the fixture's `tmp` folder, so origin's handoffs are extracted there |
| `spawn`: scripts the `SystemProc` tests start themselves, outside any tick | the runner's, with the `Cmd`'s own changes: `X=set` in one; `TICK_KEEP` and `TICK_DROP` set and `TICK_DROP` and `PATH` removed in another, then `HOME` removed and `TICK_KEEP=2` |

Each `cm tick` they spawn gets a null stdin. None of them uses the network, though the harness does not block it: each real-git origin is a bare repository in the fixture's folder. Most compare every line they print (text, or `--json`'s `lines`) exactly, some only part of them (a count, the first line, a prefix), and the three that close their stdout none; a bound real-git run asserts stdout is exactly its act lines, after gitsync's `failed 3 times` line where its origin is gone; and `the_built_cm_tick_runs_the_loop_with_fake_cm_and_git_on_path` and `the_built_cm_tick_prints_nothing_when_there_is_nothing_to_do` assert that no `cm sync` runs: a card whose phase prints, fails or calls `cm sync` in that harness must adjust those tests as well as its own files.

| Card | Files |
|---|---|
| `07-tick/mvp/housekeeping` | `gitsync.rs`, `pin.rs`, `directive.rs`, `claude_md.rs` |
| `07-tick/mvp/exec-merge` | `exec/merge_ready.rs`, `exec/unblock_now.rs`, `exec/released_undeployed.rs` |
| `07-tick/mvp/exec-codex` | `exec/codex_dispatch.rs`, `exec/review_owed.rs`, `collect.rs` |

Phases:

| File | Signature | Port of |
|---|---|---|
| `gitsync.rs` | `sync(&Ctx, Mode) -> Report` | heartbeat.sh steps 1 and 5 |
| `pin.rs` | `run(&Ctx) -> Report` | step 2; it installs the pinned cm at `ctx.cm`, the program the act list is read from, only when that is an absolute path (a `--cm` name `PATH` does not hold from the start directory stays bare, and nothing is installed for it). Like `install(1)`, it replaces a cm of any mode, 0444 included, in a folder it may write. A `cm tick` killed by SIGKILL can leave a 0755 `.cm-pin-*` beside the cm and a `cm-pin-*` folder in TMPDIR; nothing sweeps them |
| `directive.rs` | `run(&Ctx) -> Report` | step 3 |
| `collect.rs` | `run(&Ctx) -> Report` | review-latency U4 |
| `claude_md.rs` | `run(&Ctx) -> Report` | step 6 |

`Report { lines, failed }`: a phase that fails, has a child cut at its `BUDGET`, or runs a whole second past it also gets a `tick: <phase> phase took …` line. The phase's `BUDGET` is also its children's deadline: no child starts after it, and one it cuts gets the 5 s grace. Two children are exceptions, with 30 s and a grace of their own whatever is left of the budget. gitsync's `git rebase --abort` after a failed try, so a pull the budget cut mid-rebase is still aborted, is the only child gitsync starts past its budget, and no try follows it, so gitsync ends at most 40 s past its 60 s, give or take a tenth of a second of polling: a try cut at the deadline, its 5 s grace, then the abort's 30 s and grace. The `still_bound` check after each successful pull adds its own time on top, since it is not the phase's: up to three checks, one a try, and a check whose fetch hangs takes its 30 s cap and the 5 s grace, so three such checks put gitsync 145 s past its budget (`gitsync_ends_at_most_40_s_past_its_budget_and_the_time_of_its_three_checks`). In pin, the staged cm's `--version` run, so a fetch that spent most of the budget is still checked before anything is installed, starts only once `tar` has exited within the budget, so pin ends at most 35 s past its 60 s, plus the copy of the unpacked cm beside the old one. The directive phase reads in-process with no deadline; its work is linear in the journal's bytes, with at most 64 pickup probes per byte read (`PROBES_PER_BYTE`), past which it names the journal line its unchecked pickups start from and lists the directives not found yet.

Executors:

- `Executor::run(&self, &ActLine, &Ctx) -> Outcome` replaces one `act.d/` script; the `Registry` looks up `<kind>.<lane>`, then `<kind>`.
- `Outcome` is one of:
  - `Done(lines)`: the move is done, and these lines are left for the model: joined with `\n`, trailing newlines dropped, then split at each `\n` into lines of their own (`["x\n\n"]` prints `x`, `["\n\n"]` nothing, `["\nx"]` an empty line and `x`, `["a\n", "b"]` `a`, an empty line and `b`);
  - `Left(text)`: the line goes to the model as one line, its control characters escaped (`\n`, `\u{1b}`) as every printed line's are;
  - `Failed(reason)`: the act line is printed once with `[tick/<key> failed: <last line of reason>]`, that line cut to at most 300 bytes, at a character boundary, before its control characters are escaped (a reason of 300 tabs prints as 600 bytes).
- An executor that panics is taken as `Failed("panicked: <message>")` (so for a message of several lines only its last line is printed, and `panicked:` is lost), and a phase that panics prints `tick: <phase> phase panicked: <message>` and its wall time; the tick goes on. When the budget cut one of an executor's children, its line ends with the timed-out note and whatever `Outcome` the executor returned is discarded.
- `ActLine` holds the line's JSON and its lane (the line's, else the step's state). `text()` gives heartbeat.sh's `<kind> <target>[#<part>]: <command>`.
- `Ctx` holds the checkout, the epoch (relative, `store/epochs/<slug>`), `proc`, `env` and `cm` (the program cm calls go to). It offers:
  - `run(Cmd)`: in the checkout, at most what is left of the budget;
  - `cm(sub, args)`, `cm_cmd`/`cm_run` (stdin, `--json`), `step_state(target)` and `pull(line, step)`, plus `last_line(&Ran)`, all with act_lib.py's messages (a cm cut at the `Whole` cap, or when memory runs out first, fails as `cm <sub>'s output was too large to keep in memory (64 MiB at most) and it was killed`);
  - `pause(d)`, which a fake records without waiting;
  - `still_bound()`, the frame's bound check run afresh, outside the budget.
- Executor code itself must not block: only its children are bounded. Only a child the budget cut short makes the loop print the timeout line.

Children:

- `Proc::run(&Cmd) -> Result<Ran>` (`SystemProc`, or `FakeProc` in tests). `Cmd` takes argv only, never a shell string, plus cwd, `env` and `env_remove`, stdin, timeout, `merge_stderr`, a `StdoutKeep` for stdout and a `Keep` for stderr:
  - `Keep::Whole` keeps every byte up to 64 MiB; past that, or when memory for it cannot be had, the child is killed and exits `TooLarge`;
  - `Keep::Head` keeps the first 4 MiB, `Keep::Tail` the last 4 MiB; memory that cannot be had marks the stream truncated;
  - `StdoutKeep::Spill`, for stdout only, keeps up to 64 MiB in memory and moves everything to an unlinked temporary file past that (or sooner, when memory runs out), handed back as `Ran::stdout_spill` (`Spilled::Stored`). If the file cannot be made or written, or a write would pass `RLIMIT_FSIZE` (checked before writing, since the kernel's SIGXFSZ would kill the tick), the child is killed, exits `TooLarge`, and the error is `Spilled::Failed`.
- `Ran` holds the exit (`Code`, `Signal`, `TimedOut` or `TooLarge`), stdout and stderr, whether each was truncated, a spilled stdout, and the wall time.
- Stdout and stderr are always pipes, read until every process holding them has closed them, so output a descendant prints after the leader exits is kept, and a descendant holding them open keeps the child running until its timeout.
- Each child runs in a process group of its own. At its timeout the group gets TERM, then up to 5 s for every member to finish (even after the leader exits), then KILL.

**Limits:**

- A kill of `cm tick` itself does not reach a running child's own group, so the Stop hook must bound `cm tick` by session.
- A spill write that blocks (a frozen filesystem under `TMPDIR`) holds the capture, and so its timeout, until the write returns.
- A list above 64 MiB is moved to a file in `TMPDIR`; when that file cannot be written the tick prints one line saying so and no act line, even if memory could have held the list; exit 0.
- Each act line is parsed and its text formatted whole, in text and `--json` output alike: when memory for one line cannot be had, Rust's allocation abort ends the tick (exit 134). Allocation failure can occur before or after the current move; moves already made stand, later moves do not run, and output stops at the failure (under `--json`, no object is printed). Only the list's 64 MiB hold moves to a file when memory runs out; `cm next` prints lines of a few kilobytes, a few megabytes at most.
- `--json` holds every line, and the object built from them, to stream one object after the loop (never a second copy as text), so its memory grows with the list (text output does not): memory short of a few times the list's size ends the tick with the same allocation abort (exit 134), during the loop, before or after a move, or after the loop: moves already made stand, later moves do not run, and no object is printed.
- A `cm next` that floods its stdout fills the spill in `TMPDIR` until its 30 s cap and the 5 s grace after it, so a runaway producer can write gigabytes before it is killed.
- `RLIMIT_CPU` is not caught: past the soft limit the kernel's SIGXCPU ends the tick (exit 152), past the hard limit SIGKILL, as either ended heartbeat.sh; moves already made stand.

## Ideas

An idea is a Markdown file that can outlive an epoch. Set its folder explicitly in
`EPOCH.md` front matter; `ideas_dir` is relative to the campaign folder and has no default:

```yaml
epoch: epoch-alpha
status: PLANNING
ideas_dir: ../ideas
ideas: [IDEA-9003, IDEA-9001]
```

Each idea file is named `<id>.md` or `<id>-<slug>.md`, with an id of the form
`<letters>-<digits>` (for example `IDEA-9001`). Files in the ideas folder not
named `<letters>-<digits>[-slug].md` are ignored. Its front matter includes
`id: IDEA-9001` and `maturity: captured` (or `exploring` or `designed`). Admission
sets `current_epoch: epoch-alpha`. A `## Epoch history` section must already exist, with every Markdown fence closed.
cm changes only the `current_epoch` and `maturity` front-matter lines and appends
history entries; other keys, sections and existing history entries keep their bytes.

```sh
cm idea admit IDEA-9001
cm idea move IDEA-9001 exploring
cm idea outcome IDEA-9001 partial --to unassigned --note 'tool-x #12 shipped'
```

Admission refuses a closed campaign or CLOSED epoch, an idea still open in another
epoch, or an idea whose last outcome is terminal. Movement and outcomes require
`current_epoch` to name this epoch. Maturity moves forward only. A partial or
deferred outcome sets `current_epoch: null`; terminal outcomes leave that field
unchanged. All outcomes preserve the epoch's `ideas:` admission list.

| Outcome | Required flags | Terminal |
|---|---|---|
| `landed` | `--note` with evidence | yes |
| `partial` | `--to` and `--note` describing what landed | no |
| `deferred` | `--to` | no |
| `superseded` | `--by` naming an existing idea | yes |
| `cancelled` | `--note` with the reason | yes |

`--to` accepts only `unassigned`. A later epoch takes the remainder by running
its own `cm idea admit`; an outcome cannot admit it to another epoch.
`--note` is one non-empty line.
Only the flags listed for each outcome are accepted. cm writes date-only history
lines, for example ``- 2026-01-02: admitted to `epoch-alpha` (PLANNING).`` and
``- 2026-01-09: outcome in `epoch-alpha`: deferred to unassigned.`` Only cm's exact
formats count as state. Existing date or timestamp entries, continuation lines
and nested bullets remain prose.

`ideas:` is a flow list in admission order; block lists are refused with
`idea-list-form`. Admission writes the idea first, then `EPOCH.md`, then `JOURNAL.md`;
retry it after an interruption. Idea outcomes live only in the idea's history,
so they add no `.cm` fields and do not change the state schema.

When `ideas_dir` is set, `cm lint` checks duplicate ids (`idea-duplicate-id`, one
violation per file), filenames (`idea-filename`), required front matter
(`idea-front-matter`), maturity (`idea-maturity`) and epoch membership
(`idea-epoch-list`). An open idea naming this epoch must appear in `ideas:`, and
every listed id must resolve to a file that either names this epoch as
`current_epoch` or records an outcome for this epoch in `## Epoch history`.
This is one rule for OPEN, PLANNING, WORKING and CLOSED epochs: recorded
remainders may be unassigned or admitted later, but unexplained reassignments
still fail. Existing outcome history naming an epoch as its target remains
readable. Commands also refuse ambiguous ids with `idea-duplicate-id`.

## Review tools

Four commands serve a review pass over idea records. None of them needs a campaign:
`--ideas DIR` names the ideas folder, and without it the campaign's `ideas_dir` is used.

```sh
cm idea facts --pin core=~/code/dekopon/dekopon@4eb92d29f8 --pin ct=~/code/dekopon/campaign-trail@e0dc3dc5d \
  --ideas store/ideas --out store/designs/<pass>-design/FACTS.md IDEA-0151 IDEA-0138
cm read --pins store/designs/<pass>-design/FACTS.md core:crates/dekopon-gatewayd/src/gateway.rs:360-445 ct:AGENTS.md:#Regrets
cm idea idents --ideas store/ideas --base <session-start-sha> IDEA-0151
cm idea split-history --ideas store/ideas --apply
```

`cm read` takes many specs per call. A spec is `<alias>:<path>`, `<dir>@<sha>:<path>` or
`<sha>:<path>` (the current directory's repository), optionally followed by `:N`, `:A-B`, `:A-`
or `:#Heading` (Markdown only: exact, then prefix, then substring, case-insensitive; an ambiguous
heading is refused with the candidates). It reads `git show <sha>:<path>`. A file of 8 KB or
less prints whole; a bigger one without a range prints a numbered outline and its byte size:
Markdown headings, item definitions for code (`fn`, `struct`, `enum`, `impl`, `trait`, `mod`,
`class`, `def`, `func` and the like), table headers for TOML, top-level keys for YAML, and every
Nth line otherwise. A directory lists its entries. Output stops at about 64 KB per call; the
rest of a spec and any later spec are named in `[cut: …]` lines and a closing
`Cut by the 64 KB cap:` line. Aliases come from `--pins F`, the `| Alias | Repository | Commit |`
table under a `Pins` heading that `cm idea facts` writes, and from `--pin`. With pins, every spec
must resolve to one of them: an explicit `<dir>@<sha>` that is not pinned is `read-spec`.
Other refusals are `read-missing` (no such path at the pin), `read-range` and `read-heading`;
each is listed and the remaining specs still print.

`cm idea facts` extracts citations from each record: `path`, `path:N`, `path:A-B` and
`path:A,B` (en dashes accepted), in backticks or plain text, where the path has a `/` or a known
source extension. URLs and Markdown link text and targets are not citations. A path resolves
exactly at a pin's root first, then as a `/path` suffix, then loosely (the last component equal,
the others ending with the cited ones, so `model/src/wire.rs` finds
`crates/dekopon-model/src/wire.rs`, marked `loose match`). Several matches are listed as
ambiguous. A citation is `STALE` when no pin has the path or a cited line is past its end. Line
citations name the enclosing heading or item from `cm read`'s outliner. No model is involved.

`cm idea idents` reads `git diff <base> -- <record>` in the ideas folder's repository (so
uncommitted edits count) and takes the added lines. It searches the files the record links:
Markdown links that resolve to a file and bare record ids in the ideas folder. The record's own
notes and history (files named for its id outside the ideas folder) are not sources. A token
inside a longer identifier is a `MISMATCH`, naming the longer token and its line: the check that
catches `DISABLE_NONESSENTIAL_TRAFFIC` written for `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`.

`cm idea split-history` moves a `## Rubric review — <date>`, `## Astra review — <date>` or
`## Regrets sweep — <date>` section when its record is at least `--min-bytes` and an
`## Owner rulings …` heading names that kind of review and is undated or dated on or after it.
The heading stays as a stub with a link to the history file and the review's verdict line;
the text is appended verbatim to `history/<id>.md`. A stub never moves: a section with at most two
lines under its heading that names the verdict and links the full review into `history/` or a pass's
`designs/<pass>/notes/` is already in its minimal form.

## The folder

```
<campaign>/
  RESUME.md  JOURNAL.md  LIMITS.toml  OWNER-QUEUE.md  CARD.md  [EPOCH.md]  [MAC-LANE.md]
  .cm/state.json  .cm/.gitignore  (.cm/lock  .cm/pulls-read.json: runtime, ignored)
  <NN-name>/<unit>/
    STATE.md  .cm/state.json
    <step>/kickoff.md  step-*.md  resume*.md
  00-release/<repo>/
    STATE.md  .cm/state.json
    <major>.<minor>.<patch>/kickoff.md
```

cm writes `.cm/.gitignore` with its first lock, keeping the runtime `.cm/lock` (and any old
`.cm/JOURNAL.md`) out of git. A campaign from before 0.3 untracks both once:
`git rm --cached .cm/JOURNAL.md .cm/lock`.

`<NN-name>` is two digits, `-`, a name (`01-first`). Units and steps are names of letters, digits,
`.`, `_` and `-`.

Packet paths are checked on this machine, resolved against the git repository holding the campaign:
a relative path is checked when its first segment is a top-level directory of the repository or of
the campaign folder, `./` and `../` paths against the packet's own folder, and an
absolute path that does not exist is retried as the part after the repository's directory name
(`/home/a/code/<repo>/store/x` → `<repo root>/store/x`). Prefer repo-relative paths. A path that lives elsewhere (a host path on a server, such
as `/var/lib/<service>`) goes in a fenced block or without backticks; backticked, it fails
`packet-path`.

To undo a hand edit: `git checkout HEAD -- JOURNAL.md` resets the journal (re-add uncommitted
entries with `cm journal`), and `rm <NN-name>/<unit>/STATE.md && cm init <NN-name>/<unit>` re-renders a unit's
`STATE.md`.

### Template contracts

| File | Lint requires |
|---|---|
| `RESUME.md` | Opens with a `>` banner. No `/loop` proposals until the owner queue is drained: while `OWNER-QUEUE.md` has an open item (a `##` heading), the banner carries the line `OWNER-QUEUE not drained: /loop withheld` and a fenced `/loop` is a violation; once the queue is empty or absent, the banner holds a fenced `/loop …` command and the withheld line is a violation. After `cm close` the banner holds neither: nothing resumes a closed campaign. Headings `# Resume`, `## State today`, `## How to work`, `## Read, in this order`, in that order. New templates include standalone `<!-- cm:status -->` and `<!-- cm:status-end -->` lines inside `## State today`; `cm sync` replaces only their contents. Markers inside fenced code are examples and are ignored. Both markers are optional for existing files; a lone, repeated, reversed or misplaced marker is `resume-status`. `cm epoch` and `cm close` also refresh the span; a sealed campaign immediately reads closed. Epoch status is `unknown` without a recognized `EPOCH.md` status. |
| `JOURNAL.md` | First heading `# Journal`; after the adopted baseline, every `##` heading is `## YYYY-MM-DDTHH:MMZ — <title>`; the file extends `git show HEAD:JOURNAL.md` with stamped entries only: a changed committed line is `journal-rewritten`, text added to the last committed entry is `journal-hand-append`. Outside git, or before the journal's first commit, only the headings are checked. |
| `LIMITS.toml` | `paused` (bool); `runaway_ceiling_usd`, `min_free_disk_gib`, `kache_headroom_gib`, `pr_ci_expected_min`, `pr_ci_deadline_min` (non-negative numbers); optionally `codex_stall_hours` (a non-negative number, default 2), `release_after_hours` and `deploy_after_hours` (non-negative numbers; absent, their `--act` kinds are off); `max_heavy_builds` (non-negative integer); `driver_model`, `driver_effort`, `verifier_model`, `supervisor_model`, `supervisor_effort`, `disk_volume` (non-empty strings). Other keys are allowed. |
| `OWNER-QUEUE.md` | First heading `# Owner queue`; each `##` item has a line starting `Context:`, one starting `Recommendation:` and one starting `Answer:`. A queue written before cm 0.4 whose items lack `Context:` is reported, never rewritten. `cm lint` also refuses an item heading that is not cm's `## N. <title>`, or a repeated N (`owner-queue-heading`), and a `blocked:until:owner-queue:N` whose item was never filed or closed before the block (`owner-queue-dangling`). |
| `MAC-LANE.md` (optional) | Items start at column 0 as `N. title` or bare `N.`, with decimal digits for N. Other lines, including indented steps, are item text. Numbers run 1, 2, 3… in order; gaps, repeats and numbers above the supported range violate `mac-lane-number` and make `cm close` refuse. Item text is unrestricted. A leading `**PARTIAL <date>** (<receipt>). ` leaves the item open; `done` replaces that span and preserves the instructions. |
| `<unit>/STATE.md` | Byte-equal to the render of the unit's `.cm/state.json`. |
| Packets | In `kickoff.md`, `step-*.md` and `resume*.md` of a drafted or launched step, every absolute or `~/` path inside backticks exists. A span with whitespace, a glob or placeholder character (`* ? < > $ { } [ ] \|`), or a single component such as `/loop` or `//!` is not a path; a trailing `:line` or `:start-end` is dropped. Only spans that could name a file are checked: a cm id (`01-a/u/S1`, `00-release/cm/0.5.0`), a URL and a route (an absolute path with no file extension whose first directory is not on this machine, such as `/v1/sessions/abc`) are not. Fenced blocks are not read. |
| Tracked files | Warns `ledger-size` for each tracked regular file under the campaign folder exceeding 1 MiB, excluding `JOURNAL.md` and `.cm/` at any depth. Exactly 1 MiB, untracked files, and campaigns outside git are silent. Warnings do not change the exit status. |

### State files

Campaign `.cm/state.json`:

```json
{
  "schema": 7,
  "kind": "campaign",
  "initialized": "2026-10-08T07:04Z",
  "written": ["RESUME.md", "JOURNAL.md", "LIMITS.toml", "OWNER-QUEUE.md"],
  "adopted": [],
  "journal": { "baseline_lines": 3 },
  "owner_queue_read": { "at": "2026-10-15T17:00Z", "items": ["1. Choose the setting"] },
  "closed": "2026-10-15T18:00Z"
}
```

`closed` appears only after `cm close`. Optional `owner_queue_read` records the read stamp and heading identities; only an explicit queue read writes it. Beside it, `.cm/verdicts.json` is the computed-verdict
ledger (see [Review verdicts](#review-verdicts)), `.cm/lane-drivers.json` the registered lane
drivers (see [Lane drivers](#lane-drivers)) and `.cm/pulls.json` the open PRs the last
`cm sync` saw (optional `base`, `ci_head` and `ci_base` remember its base and last completed CI observation); a campaign without them reads as having none. `written` are files cm wrote, `adopted` are files that existed and linted clean at `cm init`.
`journal.baseline_lines` is where cm's stamped region starts; an adopted journal keeps its old
headings.

Unit `.cm/state.json` (a release unit):

```json
{
  "schema": 7,
  "kind": "unit",
  "unit": "00-release/widget",
  "state": "launched",
  "since": "2026-10-08T07:04Z",
  "steps": {
    "1.4.0": {
      "state": "merged",
      "since": "2026-10-08T08:10Z",
      "evidence": { "sha": "<40 hex>", "pr": "https://github.com/acme/widget/pull/7", "merged_at": "2026-10-08T07:50:42Z" },
      "worktree": "/abs/path",
      "launches": 1,
      "lane": "pi",
      "github": "acme/widget",
      "tag": "v1.4.0",
      "after": ["01-first/alpha/PLAN", "tag:widget@1.3.0"],
      "artifacts": ["crate:acme-sdk"]
    }
  },
  "history": [
    { "at": "2026-10-08T07:04Z", "target": "00-release/widget/1.4.0", "from": "drafted", "to": "launched" }
  ]
}
```

`since` is the time cm observed or advanced the state. Optional `evidence.merged_at` is
GitHub's merge time, recorded by `cm sync` or `cm advance <step> merged --sha <sha> --pr <url>`;
`--pr` must match the recorded PR when one is recorded. An absent merge time stays absent.
For merged tracks, `STATE.md` uses `merged_at`, falling back to `since`; the journal's move evidence
includes `merged_at` when present. `cm close` lists genuine historical merges using `merged_at`,
falling back to the move's observation time. Restoring a blocked merged step does not count
as another merge.
A multipart step's merge time is the latest of its parts' merge times, or absent if any is unknown.

A release remerge with `merged --sha <sha>` and no `--pr` keeps the track's existing `merged_at`
for the original PR merge. The new history move has no GitHub merge time, so its close-accounting
row uses the remerge's observation time. Each real merge has its own row.

`lane` and `after` appear on declared steps (`cm init <step> --lane`); a declared step that has not
launched has no `worktree` and `launches: 0`, and `STATE.md` adds an `Order` table. A rollout step persists
`"work": "rollout"`; a study persists `"work": "study"`, and an omitted `work` means code.
A blocked track also carries `blocked_from`. From `blocked:<reason>` a track resumes to the state it
was blocked from, or moves to that state's successor; a step blocked before its PR relaunches with
`cm launch`, which picks the newest `resume*.md`.

### Evidence

| State | Requires | Takes | Checked |
|---|---|---|---|
| `launched` | `cm launch` only | | the launch checks |
| `pr-open` | `--pr`, `--sha` | | URL is `https://github.com/<owner>/<repo>/pull/<n>`; the SHA resolves to a commit |
| `merged` | `--sha` | `--pr` | SHA resolves to a commit; a PR URL records GitHub’s merge time when available |
| `released` | `--tag`; also `--sha` for a tag-only release from `drafted` or `launched` | `--digest` | the tag exists and contains the recorded merge SHA, or the given tag-only SHA; a release step's tag is its declared tag |
| `deployed` | `--sha` and `--evidence` for a rollout; `--digest` otherwise | optional `--digest` for a rollout | rollout: SHA resolves to a commit and evidence is a non-empty campaign file naming the Argo sync revision; otherwise `sha256:<64 hex>` |
| `done` | `--evidence` for a study, a `mac`-lane step, an `owner`-lane step other than a rollout, or a non-release `coordinator`-lane step other than a rollout without a recorded PR, SHA or declared parts; nothing for a code or rollout step at `deployed` | | a non-empty regular file inside the campaign, outside `.cm/`; recorded relative to it |
| `superseded` | `--evidence` | `--by STEP[,STEP…]` | as for `done`; final like `done` |

SHAs and tags resolve in `--repo`, else the step's worktree, else (for a unit) any step's worktree.
A commit or tag made elsewhere needs a `git fetch` first when checking locally. For `released`
without a checkout, cm checks GitHub in the recorded PR's repository or the release's declared
`--github` repository: the tag must exist before its containment is checked. A branch or commit
name cannot stand in for a tag. An unread tag or ancestry check refuses the move. A tag-only
release needs `released --tag T --sha S`, with S contained in T. A release step's `--tag` must be
the tag it was declared with (`release-tag`), and its `pr-open --pr` must be in its repository
(`release-repo`).
`cm sync` reads the same evidence from GitHub instead.

### Order

`cm advance <step> blocked:until:<condition>` records a checkable block. Conditions
are the state gates from `--after` (`<step>`, `pr-open:<step>`, `tag:<line>@<version>`,
`deployed:<line>@<version>`), plus `epoch:WORKING` and `owner-queue:<n>` (item
heading removed from OWNER-QUEUE.md). `<n>` is a positive item number within `u32`, parsed by one parser for
headings, conditions, checks and lint. A block on `owner-queue:<n>` refuses unless item n is open
now (`owner-queue-not-open`); file the question and block in one command with
`cm owner-queue add … --block <step>`. `cm next` lists satisfied blocks under
`unblock now` (`unblock_now` in JSON), with a numbered
`cm unblock <step>` command. It rechecks the condition and, when it holds, resumes without
an evidence file and journals `- condition: until:<condition> holds`. An unmet condition or
a free-text block requires `--evidence PATH`. `cm sync` journals the first observation;
it leaves the step blocked for an explicit resume. CARD shows whether the condition
holds. Lint warns `block-stale` for free-text blocks older than 24 hours without a
later journal title naming the step.

For a step that uses `cm launch`, `cm next` offers the launch once every gate is met.
`cm launch` checks those gates again. `--after` takes:

| Gate | Met when |
|---|---|
| `<step>` or `<NN-name>/<unit>/<step>` | that step is `merged` or later |
| `pr-open:<step>` | that step is `pr-open` or later (a chain that builds on an open PR) |
| `pin:<step>@<sha>` | that step's recorded head matches `<sha>` (7 to 40 hex digits, case-insensitive prefix matching as for pinned evidence) |
| `tag:<line>@<version>` | the release step `00-release/<line>/<version>` is `released` or later |
| `deployed:<line>@<version>` | that release step is `deployed` or later |

A pin uses the latest synced open PR head, which sync also saves as the step's recorded SHA.
After merge, that SHA is the merge commit; a pin to the old PR head becomes stale when
those commits differ.
Drafted dependents wait on their pin before launch. When the recorded commit differs or the
source is superseded, `cm next` and CARD list launched and PR-open dependents under `waiting`
as `stale pin`, with a recheck command; `cm lint` warns `pin-unmet` in those two states.
Named `cm next` keeps that warning alongside any pending fresh review. Blocked dependents
and dependents at merged or later, done or superseded have no pin warning. A head that has
never been recorded is reported as `head not recorded`. A multipart step has no single head to pin.
Pins are refused in `known-red --until` and `blocked:until:`.

A step or `pr-open:` gate on a `mac`-lane step, an `owner`-lane step other than a rollout, a
study step, or a non-release `coordinator`-lane step without a recorded PR, SHA or declared
parts is met only when that step is `done`. Rollout gates use `merged` or `pr-open`, including
in the owner lane.
A blocked step meets the gates of the state it was blocked from. A bare step name is in the declaring step's unit.

`cm next` numbers each action and prints its exact cm command; JSON carries the same string
as `command` on each item. CARD.md uses the same renderer and numbers its Next, Waiting and
Blocked lines across those sections. Run commands from the campaign folder, or append `-C DIR`.
Replace quoted placeholders such as `'<watch-evidence-file>'` with the named input.
`ready` is the old word for `launchable`.
Launchable owner steps other than rollouts offer `cm advance <step> done --evidence '<owner-evidence-file>'`.
Launchable owner rollouts offer `cm launch <step>`.
Launchable local steps name a worktree placeholder. Waiting steps offer `cm next <step>` to recheck
their gates, or `cm amend --kickoff-file` when the kickoff is missing. Stalled launches offer
`cm attempt <step>` to inspect the attempt before acting; dirty PRs offer `cm attention` with
worker and coordinator session placeholders. Tags offer `cm sync` to observe the tag after
it is cut. Stale evidence offers `cm evidence` with a fresh name and body, pinned to the current
candidate and harness heads, or named SHA inputs when no head resolves. A revision whose step
became multipart also names the required part input. An `until:` block offers `cm next`
while its condition is unmet; that command lists the unblock action once it holds. Free-text
blocks offer `cm unblock`.

When `CM_CODEX_WEEKLY_PCT` contains a valid percentage at or above 85, `cm next`
lists launchable Codex steps as waiting and shows the reason. Named `cm next <step>` and
`cm launch <step>` refuse the held dispatch with exit 1 and `codex-paused`. The
hold appears in regular `cm next`. CARD.md shows no hold: the usage percent is the caller's
environment, not a campaign fact, so the card lists a held step as next.

### Remote turns

An attempt's stall clock runs from the step's own stamp: a launch, a restart or an unblock
resets it, and a successor coordinator's re-claim does not. A step that finishes by evidence,
with an open watch whose deadline is still ahead, is not stalled: the watch holds its next move. For a lane with a configured
reader, a later recorded turn `began` time moves it forward, including when the reader fails.
Set a lane's optional `<lane>_stall_hours` key in LIMITS.toml to override the two-hour default.
Use underscores in lane names, as in `claude_cloud_stall_hours`.

Set both `codex_turn_reader` and `codex_turn_logs` in LIMITS.toml to enable Codex turn reads.
Use `claude_cloud_turn_reader` and `claude_cloud_turn_logs` for Claude Cloud. Each value is
an argv array. cm substitutes `{thread}` and `{turn}` in arguments and expands leading `~/`.
The reader runs outside the campaign lock, one at a time and never beside sync's GitHub reads, with a 15-second deadline through stdout completion.
It returns `done`, `successful`, and `turn_status` as JSON, with optional `completed_at` and
`pr_url`. A PR URL from the reader must match the step’s repository and, when a branch was
recorded at launch, GitHub must report that branch as the PR head. Sync reports unparseable
LIMITS.toml, invalid command pairs, and reader failures in `turn_reader_warnings`.

`cm next --act` prints only actionable lines, each starting with `<kind> <step>:`. It lists
`turn-ended` first, then `review-owed`, then the other kinds by the number of steps waiting
behind them; lines of a unit amended with `--priority now` come before all others. JSON carries
`kind`, `since`, `waiting_count`, `priority` and `command` in the `actionable` list and, where
they apply, the step's `lane` and `weight` and its PR's `pr`, `head` (sync's last observation,
else the recorded one), `base` and `ci_green` (only when CI completed at that head). Plain
`cm next --json` carries the same list. `--json next --act` also carries `owner_queue_unread`, so
an executor can hold `merge-ready` while merges refuse with `owner-queue-unread`. Exit 1 means actions remain. Exit 0 means the list is
empty.

A step's `weight`, on act lines and on every `cm next --json` step, is derived from one kickoff
line, `Weight: simple|complex — repos: <list>; files: <n or list>; surface: none|<what>`: an
optional `- ` bullet, any case, `—`, `-` or `:` before the checklist, backticked values, and
`files: 12` or `12 files` for twelve. It is `simple` only when the line says simple, names one
repository, at most three footprint files and surface `none`; anything else, a missing line
included, is `complex`. `cm init --kickoff-file`, `cm amend --kickoff-file` and `cm lint` (on
steps short of final) refuse a line that does not parse as `weight-malformed`, and a `simple`
its own checklist contradicts (an empty `files:` included) as `weight-contradiction`.

| Kind | Fires when | Command |
|---|---|---|
| `turn-ended` | a launched step's configured turn reader saw its turn complete with no PR | the lane's logs command |
| `review-owed` | a `pr-open` head, the one sync last observed or else the recorded one, lacks a verdict from a review the merge gate needs (on a model lane, Astra's and a Claude reviewer's: see [Merge gate](#merge-gate)), or its verdict is FIX REQUIRED and no newer head has come. Every step but a script pin owes reviews. On a model lane both must be explicit ACCEPTs, computed and stated, bound to that PR and part; a lane with no model author needs any one verdict. A `cm review wait` marker silences it; a review ledger row names no head and silences nothing | `cm brief <step>`, or `cm attention --pr …` for the fix |
| `missing-kickoff` | a drafted step's gates are met and `kickoff.md` is missing | `cm amend <step> --kickoff-file …` |
| `stalled-attempt` | a live attempt passed its lane's stall limit (see above) | `cm attempt <step>` |
| `earlier-coordinator` | a live attempt is claimed by another coordinator | `cm attempt <step> claim --expect <generation>` |
| `launchable-not-launched` | the step is in `cm next`'s launchable list: one predicate serves both, so pauses and the Codex quota hold apply. `since` is the newest of the step's stamp and its gate targets' | the step's launch command |
| `attempt-ended` | a launched step with no PR (on it or any part) whose last attempt ended; a live attempt is `turn-ended` or `stalled-attempt` instead. Mac-lane steps are excluded | `cm attempt <step> restart --lane <lane>` after `end unknown` while the attempt's deadline is ahead, `cm attempt <step>` once it has passed; otherwise `cm advance <step> done --evidence …` for a step that finishes by evidence, or `cm advance <step> pr-open --pr … --sha …` |
| `merged-unreleased` | `release_after_hours` is set, the campaign is not paused, and the oldest merge (GitHub's `merged_at`, else the step's stamp) of a repository some `00-release/<line>/*` step ships from is older than the dial. One line per line: the repository's own name wins, as in sync. Repositories with no release line, rollouts and studies never fire. Quiet while a release of the line is launched, `pr-open` or `merged`, or while a drafted one is launchable (that is its own line). `waiting_count` is the merged steps it holds | `cm init 00-release/<line>/'<version>' … --after …`, or the drafted release's `cm next`/`cm amend --kickoff-file` |
| `released-undeployed` | `deploy_after_hours` is set, the campaign is not paused, and a line's newest release is older than the dial while no deploy step gated on its `tag:` (or a later one of the line) has merged: a pin ends at its merge. A deploy step is a rollout, a step named `…/pin` or `…/deploy`, or a step whose PR is in a repository no release line ships (a homelab pin); a code PR in a released repository that waits on the tag consumes it and deploys nothing. Lines no deploy step names never fire; quiet while a covering deploy is launched or `pr-open`, or launchable. `waiting_count` is the line's releases since its last deploy | the drafted deploy step's `cm next`/`cm amend --kickoff-file`, or `tools/cloud-env/deploy.sh 00-release/<line>/<version>` |
| `unblock-now` | each `unblock now` line `cm next` prints | its `cm unblock <step>` |
| `expired-watch` | an unresolved watch is past its deadline | `cm watch <step> resolve <id> --evidence …` (or `withdraw <id> --reason …`) |
| `merge-ready` | a `pr-open` PR whose observed head is the head CI completed green at (`ci_green` in `.cm/pulls.json`: sync re-reads finished CI in full with each full PR read, at least every 30 minutes, and clears the value on a read that finds checks still running or fails, and when the PR itself goes unread), whose `mergeable_state` is `clean`, and whose merge gate has no gaps there (only a script pin, a rollout named `…/pin` or `…/deploy`, needs no review). Before `cm next` lists it, a fresh read of GitHub must find the PR still open at that head, `mergeable_state` clean and CI green there; a failed read or no GitHub drops the line. A blocked step merges none of its parts. Each action carries its `part`. A red check leaves `mergeable_state` unstable or blocked, so a known-red merge never shows here; it stays with `cm advance … merged`. Quiet while `paused = true`, the owner's stop; the Codex quota hold, which holds dispatch only, does not quiet it | `cm advance <step> merged --pr <url> --sha '<merge-sha>'` after the merge |
| `stale-block` | a free-text `blocked:` older than 24 h: lint's `block-stale`, without lint's journal-mention silencer. Reviewer waits (`EVIDENCE-BLOCKED`, `REVIEW_BLOCKED`) are `review_waits` timers, not owed blocks | `cm unblock <step> --evidence …` |

`pr-open-unreviewed` is the old name of `review-owed`, which now also covers a new head after a
verdict and a FIX REQUIRED head. Plain `cm next` prints the actions it does not already list as
a next, unblock or watch item.

`earlier-coordinator` lists a live attempt claimed by a session other than the one running cm,
unless a registered lane driver whose units cover the step holds it ([Lane drivers](#lane-drivers));
`--act` prints no note for those. CARD.md's `Actionable:` count leaves out caller-relative items
(`earlier-coordinator`), which `cm next --act` still lists.

### CARD guarantees

`cm sync` renders these lines at the top of CARD.md, after its title, each `Label: value` with
`—` for none, so a watchdog can parse them:

```text
Actionable: <count> (oldest: <stamp>)
Deployed in 24 h: <count> (last: <stamp>)
Oldest open code card: <step> since <stamp>
Oldest merge-ready: <step> since <stamp>
Oldest unreleased merge: <step> since <stamp>
Oldest undeployed tag: <line>@<version> since <stamp>
Launchable at sync: <count> (oldest: <stamp>)
```

`Actionable` counts every `--act` kind. `Deployed in 24 h` counts the deploy steps that reached
`merged` or `deployed` in the last 24 hours by the step's own move (a part's move is not the step's deploy): a rollout, or a `tag:`-gated
pin (`…/pin`, `…/deploy`) or step whose PR is in a repository no release line ships, the set
`released-undeployed` reads. Its
`last` is the newest such move ever, inside the window or not. `Launchable at sync`'s
`oldest` is the earliest `since` of its `launchable-not-launched` lines. An open code card is a step that ships a PR, launched or
`pr-open`, measured from its first launch; the campaign's target is under 24 h. The unreleased
merge and the undeployed tag use the `merged-unreleased` and `released-undeployed` scans
without their dials.

### Finishing

An already sealed campaign refuses `cm close` with `campaign-closed` before lint.
Otherwise, `cm close` runs the same checks as `cm lint` on the campaign folder and
refuses with the full violation list if lint fails. Warnings do not block close.
It does not seal state or append its close journal entry on a lint refusal.

Before `cm close`, record an outcome with `cm idea outcome <id> <word>` for every id
in `EPOCH.md`'s `ideas:` list. Close lists every missing outcome as `idea-outcome-missing`;
`cm lint` reports the same gap on a sealed campaign as `closed-ideas`. Outcomes come
from the idea file's `## Epoch history` for this epoch, including deferred and partial
outcomes. The close journal lists each idea's outcome and any `to` or `by` target in
admission order. Without an `ideas_dir` key or an `ideas:` list, close behaves as before. The prose
`## Outcome accounting` table is still written by hand. `cm idea move` and
`cm idea outcome` remain available after sealing, subject to their usual checks.

A code step finishes at `deployed`, or `merged` where it has no release; `cm advance <step> done`
after `deployed` is unchanged. A `mac`-lane step, an `owner`-lane step other than a rollout,
or one declared with `--study` finishes with evidence. A non-release `coordinator`-lane step
other than a rollout, without a recorded PR, SHA or declared parts, also finishes with evidence.
`cm advance <step> done --evidence <path>` moves it to `done` from any state short of `done`
(blocked steps from where they rest). The path must be a non-empty regular file inside the
campaign folder and outside every `.cm/` folder (`evidence-file`); it is recorded, relative to the
campaign, in the move's evidence beside its stamp, and `cm lint` re-checks it every run. From
`drafted` the step's own `--after` must be met. Without `--evidence`, `done` uses the sole Markdown
file in the step's `evidence/` directory, if one exists. A missing evidence file is `evidence-missing`.
`done --evidence` on a code step, or `done` on one short of `deployed`, is `done-code-step`. A
release step cannot be a study. A `coordinator`-lane release step keeps the code-step ladder even
without a recorded tag, as does a coordinator step with a recorded PR, SHA or declared parts.

For linked Mac work, stamp the queue item with `cm mac-lane done N --receipt T`. Advance the
step separately with `cm advance <step> done --evidence <path>` and quote the `**DONE …**` receipt
in the evidence.

Declare a config or rollout step with `cm init <step> --lane L --kind rollout`. After it merges,
`cm advance <step> deployed --sha <merge-sha> --evidence <sync-receipt.md>` records its deployment.
The receipt names the Argo sync revision and must be a non-empty regular file inside the campaign,
outside `.cm/`; the SHA resolves in the step's worktree or `--repo` and must equal or descend
from the recorded merge SHA (`merge-lineage`). An unrelated history, including a separate
repository without that merge, is refused. A rollout skips `released`,
requires no `--digest`, and `cm sync` never advances it by release tags or deployment receipts.
It keeps the launch and merge gates and finishes with `done` after `deployed`, including in the
owner and coordinator lanes. `--kind rollout` is incompatible with `--study`, a release step,
and the `mac` lane.

`cm advance <step> superseded --evidence <path> --by <step>[,<step>…]` retires a step
and records its successors in the track's `evidence.superseded_by`, its move history and the
journal. `STATE.md` and `CARD.md` show the successor references. Every successor must already
be declared; a short name resolves in the retiring step's unit, and a full
`<NN-name>/<unit>/<step>` can name a step in another unit. A step cannot name itself. Repeated
successors are recorded once, in the order given. `--by` is optional for retirements without a
named successor, and is a usage error with any state other than `superseded`. `--evidence`
remains required. Superseding meets no gate; amend consumers to name the replacement work.

cm 0.13 raises SCHEMA from 6 to 7 for additive PR base/CI observations and an optional owner-queue read acknowledgement.
New fields default to absent and are omitted when absent; old state remains readable.
Sync stamps campaign state with schema 7 before writing observations so an older reader
cannot erase them. A first nonempty, fully completed check read records head/base;
pending or unreadable checks do not. The baseline changes with a new tested head,
not a same-head rerun. A changed base is a warning and never a lifecycle gate.
Upgrade every reader before writing shared state with cm 0.13.

cm 0.12 raises SCHEMA from 5 to 6 for release `artifacts` and `evidence.published`.
Both lists default to empty and are omitted when empty. Older state remains readable;
cm 0.11 refuses schema 6 so it cannot erase publication declarations on a rewrite.
Upgrade every reader before writing shared state with cm 0.12.

cm 0.10 raises SCHEMA from 4 to 5 for persisted pin dependency gates and `superseded_by`
successor references, and optional merge times. It writes every unit state, and a new campaign
state, as schema 5. Schema 1 through 4 state remains readable; missing successor references
default to an empty list and missing merge times remain absent. cm 0.9 refuses schema 5 as
newer state. Upgrade every reader before writing shared state with cm 0.10.

cm 0.9 raises SCHEMA from 3 to 4 for watch withdrawal and writes every unit state, and a new
campaign state, as schema 4. Units written by 0.9 cannot be read by cm 0.8.x; upgrade every
reader before writing a shared unit with 0.9. Schema 1, 2 and 3 state remains readable by 0.9.
cm 0.5.0 introduced schema 3 for `superseded`.
Since 0.4.0 cm refuses state from a newer schema than it knows, so cm 0.4.x refuses schema 3
cleanly; schema 1 and 2 state from 0.3.x and 0.4.x loads unchanged. cm
0.3.x checks no schema and drops the fields it does not know when it re-saves a unit, so a campaign
that uses studies or `--evidence` needs cm 0.4.0 or later on every machine that touches it.

### Attempts

A remote launch records `attempt` in the step's state before anything is dispatched: its id
(`<step>#<launch>`), the coordinator generation that claimed it (1 at launch, with
`--coordinator`), its claim token `<generation>:<coordinator>:<claim instant>`, and, with `--cap-min`, the absolute `deadline` a replacement inherits instead of
restarting the cap. The coordinator then wraps each external action:
`cm attempt <step> --claim TOKEN begin thread` before the call, and
`cm attempt <step> --claim TOKEN record thread <id>` once it returns, before the next action
(`turn`, `session`, `cancel`, …). `cm attempt <step>` reads the plan:

| Plan | Means |
|---|---|
| `dispatch` | nothing began: dispatch under this attempt |
| `resume` | the last action returned its id: resume that thread, turn or session |
| `unknown` | an action began and recorded no id: its outcome is unknown; never redispatch, find it in the runtime, then record its id or `end unknown` |
| `expired` | the deadline passed: inspect every recorded id newest first, cancel each admitted execution handle and inspect recorded cancellations (`begin cancel` is the one action still allowed), retrieve partial results and `end partial --evidence <file>` |
| `ended` | `end` recorded `complete`, `partial` (with its evidence) or `unknown`; while an action has begun without an id, only `end unknown` closes the attempt |

`.cm/lock` serializes cm on one machine only, so a replacement coordinator first fences the
generation it replaces: `cm attempt <step> claim [--coordinator <label>] --expect <G>` succeeds only
while `G` still holds the attempt, makes `G + 1` its holder and prints the new token; `begin`,
`record`, `restart` and `end` with any other token are `attempt-fenced`. Between machines the committed state
is the fence: claim, commit and push before dispatching or writing to the homelab. Two clones that
both claim `G + 1` hold different tokens, so the one whose push is refused is fenced as soon as it
takes the winner's state. An omitted `--coordinator` defaults to trimmed
`CLAUDE_CODE_REMOTE_SESSION_ID`, with a leading `cse_` mapped to `session_`. When that variable
is unset or whitespace-only, `claim` still requires `--coordinator` (a usage error without it);
`cm launch` keeps its `—` fallback. Explicit labels take precedence and are used as given.

A step still at `launched` can launch again once its last attempt has `ended` recorded:
`cm launch <step>` picks the newest `resume*.md` when present (otherwise `kickoff.md`),
increments `launches`, and records a new attempt. An open attempt still refuses relaunch
with `launch-state` and `attempt-open`, even after its deadline; end it first. A launched
local-lane step with no attempt record still refuses with `launch-state`.

After `end unknown`, `cm attempt <step> --claim TOKEN restart --lane <lane>
[--evidence <file>]` archives the old attempt as `relaned` in `attempt_history` and opens
`<step>#<launch+1>` with the same generation, claim and deadline, recording the new lane
on the attempt. It reads `dispatch`. An expired cap refuses restart. A requested or declared
`mac` lane refuses restart with `mac-lane-dispatch`. An action begun
without an id must first be closed with `end unknown`. Restart leaves the declared step
lane unchanged. Begin, record and restart each write one stamped journal line; begin
and record include the attempt id, action kind, generation and returned id when present.

### Lane drivers

A lane driver is a session the coordinator delegates a lane to (its `LANE-<name>.md` brief names
it); the driver claims its lane's attempts under its own label. The coordinator registers it:

```sh
cm lane driver add tooling --session session_01Df… --unit 07-tick/mvp [--unit …]
cm lane driver list
cm lane driver remove tooling
```

A unit is `<NN-name>`, `<NN-name>/<unit>` or `<NN-name>/<unit>/<step>`, and covers the steps under
it whole path segments at a time (`07-tick/mvp` covers `07-tick/mvp/core`, not
`07-tick/mvp2/core`). cm reads the registry, never a lane brief. `cm next --act` does not list
`earlier-coordinator` for a live attempt whose `coordinator` is a registered driver with a unit
covering the step; `cm next` notes it instead, one unnumbered line and one `driver_held` item
(`target`, `driver`, `driver_lane`, `generation`, `since`, `note`) with no `command`.
`driver_held` is absent while no driver holds a step:

```text
note: 07-tick/mvp/core is held by session_01Df…, driver of lane tooling (generation 2, claimed 2026-11-25T06:40Z)
```

A claim by an unregistered session, or by a driver on a step outside its units, still reads
`earlier-coordinator`, and the session running cm never sees its own claims either way. The claim
fence is unchanged: a driver's token is fenced like any other, and `claim --expect` still takes an
attempt over.

`add` records a `--session` of `cse_<id>`, as the runtime reports it, as `session_<id>`, the label
its claims carry. Labels are compared with `cse_` mapped to `session_` on both sides: a claim
recorded as `cse_<id>` (an explicit `--coordinator` is stored as given) is the registered
`session_<id>`'s, and so is a registry entry written as `cse_<id>` by hand, as in a merge. An
explicit `--coordinator` on `add` or `remove` is trimmed and mapped the same way, and the session
running cm sees no note for its own claim in either form. `add` refuses a lane name or session label
that is not letters, digits, `.`, `_` and `-` (`lane-driver-lane`, `lane-driver-session`), no
`--unit` or a malformed one (`lane-driver-units`), a registering label that is the session it
registers or a registered driver of any lane (`lane-driver-registrar`, which names `cm lane driver
remove <name>` for a session that drove a lane and coordinates now), and a registration identical to
the lane's current one (`lane-driver-unchanged`). A unit with no declared unit or step under it yet
is registered with a `warning:` line (JSON `undeclared`). A different session or units for a
registered lane replace its driver, and the journal names the one replaced. That comparison reads a
stored entry as written: adding again, as `cse_<id>` or `session_<id>`, a driver whose entry was
written as `cse_<id>` rewrites it as `session_<id>` and journals that it replaces `cse_<id>`.
`remove` of a lane with no driver is `lane-driver-unknown`.

`add` and `remove` journal before they write the registry. A journal append that fails records no
entry and changes nothing (`lane-driver-journal-write`); re-running the same command once
`JOURNAL.md` is writable journals the change for the first time. A registry write that fails after
it (`lane-driver-write`) leaves the registry as it was under a journal entry for the change, so
until the re-run a removed driver still holds its claims; re-running the same command journals the
change again and writes it. The registering label defaults, as `attempt claim`'s does, to trimmed
`CLAUDE_CODE_REMOTE_SESSION_ID` with `cse_` mapped to `session_`, and `--coordinator` is required
without it. These checks catch mistakes; they are not authentication. cm trusts the labels it is
given, and the journal names who registered or removed whom.

`cm next`, `cm next --act`, the `cm lane driver` verbs and `cm lint` refuse a registry cm cannot
parse with `lane-drivers-unreadable`, which names `git checkout HEAD -- .cm/lane-drivers.json`, or
deleting a registry that was never committed. A field cm does not know, at the top or in a driver,
is refused the same way rather than dropped by the next `add`, since a newer cm may have written a
registry this one cannot apply: upgrade cm. This fails closed, so a later cm that adds a registry
field must leave it out while unset until every session runs a cm that reads it. CARD.md's
`Actionable` count lists no `earlier-coordinator` at all, registered driver or not, since which
claims are earlier depends on the session asking; `cm sync` does not read the registry. cm
writes the registry whole, so two branches that each change it conflict when merged, like cm's other
`.cm/*.json` state: keep one side, then run `cm lane driver list` and re-run `cm lane driver add`
for the other side's driver.

### Review verdicts

A review report ends with one fenced `cm-review` block, followed by nothing but blank lines and, at
most, the final `ACCEPT` or `FIX REQUIRED` line that `astra-review.sh` reads:

````markdown
```cm-review
head: 9f5a5e7526780811171e178f4180d9050efe336d
F1 | contract | introduced | src/unavailable.rs:359-377 | whole-command refusals reject flags the registry omits
F2 | guideline | introduced | - | a Changelog trailer on unreleased behaviour
F3 | taste | inherited | src/lib.rs:178 | the guest adapter copies the SDK's export! body
```

FIX REQUIRED
````

`head:` is the reviewed commit (7 to 40 hex digits; a review is at a head its SHA prefixes). Each
other line is `<id> | <type> | <origin> | <file:line> | <summary>`: the id is letters, digits, `-`
and `_`, unique in the block; the type is `contract`, `guideline` or `taste`; the origin is
`introduced`, `exposed` (an existing defect this change makes reachable or worse) or `inherited`;
the place is `<path>:<line>`, `<path>:<start>-<end>`, or `-` for a finding outside any file; the
summary is the rest of the line. A block with no finding lines is a review with no findings. Other
fenced blocks may come before it.

The verdict is computed: FIX REQUIRED iff a finding is `contract` and `introduced` or `exposed`.
Those findings block; every other finding is a follow-up (`cm followups`). `.cm/verdicts.json`
holds each review (`R<n>`: step, reviewer, PR, optional part, head, verdict, the report's own verdict line,
the report copy) and each finding with its resolution or closure.

Blocked-reviewer waits are recorded at step scope with
`cm advance <step> blocked:REVIEW_BLOCKED` or `blocked:EVIDENCE-BLOCKED`.
A blocked transition with `--part` is refused as `part-state`. The reader can
surface blocked part tracks already present in state; its part-reader test uses
synthetic state and does not demonstrate a CLI path for recording a part wait.
Adding parts to a blocked step preserves the original block start.

### Rounds

Each distinct reviewed head of the current PR with a computed FIX REQUIRED verdict is one fix
pass, however many reviewers reviewed it. ACCEPT reports and reports on earlier PRs do not
consume passes. `rounds` counts fix passes for the selected part; `passes` counts them for the
step's current PR. A new 11th fix pass is refused with `review-cap`: `session cap of 10 passes`.
Final ACCEPT reports and additional reviewers at an existing head can still be recorded.

An ordinary review round fails to converge when it has more blocking findings than the previous
report from the same reviewer kind on the same PR and part. The first report in that scope converges; equal or
falling counts converge. Counts use the report's original blocking findings, including findings
later resolved. A finding marked fixed returning remains the reviewer's prose: cm does not
track finding identity across reports.

A non-converging round prints `converging: false` and journals the owner's ruling:

> Fix passes: when a review round closes fewer blocking findings than it opens, or a finding marked fixed returns, one fresh independent reader (Opus, high effort) reviews the head; the PR or design escalates to the owner only if that round also fails to converge. A third FIX REQUIRED by count alone escalates nothing. The session cap of 10 passes stands.

`cm next` lists a `fresh reader` item with
`cm review record <step> '<fresh-review-report>' --reviewer fresh-opus [--part=NAME]`.
Have an independent Opus reader at high effort review the head, then record its report with that
command. The item appears before the step is merged, including when no PR is recorded, and
remains until a `fresh-opus` report is recorded for that PR and part. The fresh round's baseline
is the trigger head, or the latest previously reviewed head for the same step, PR and part when
no request is pending. Its blocking count is the maximum across all reports at that head, so
recording order does not affect the fresh round's convergence. Without either baseline, `converging` is
null and no convergence verdict is printed. A failing fresh round writes an owner-queue item
and blocks the step `blocked:non-convergence` when it can change phase. An unrelated block and
its resume phase stay intact; clearing that block leaves the owner decision pending. An
existing non-convergence block or pending owner item suppresses duplicate queueing. After the
owner answers and `cm unblock` releases the step, another failing fresh round queues a new decision.
Three FIX REQUIRED rounds by count alone never block or queue. Existing `blocked:third-fix`
states still load and can be released with `cm unblock`.

### Merge gate

Every lane and part refuses `advance … merged` while valid new OWNER-QUEUE.md
headings await `cm owner-queue read` (`owner-queue-unread`). Release-step launch
and lifecycle moves also refuse, including sync moves and automatic merge rollups.
This refusal has no override; the existing review-gate override does not acknowledge
the queue. Declarations, amendments and read-only inspection stay available.
Missing, unreadable or queue-lint-failing queues create no unread refusal; ordinary
lint still reports their existing problems.

`cm advance <step> merged` for a step in a model lane (`codex`, `claude-cloud`, `fable`,
`coordinator`; owner, 2026-11-25), other than a deploy script's pin (a rollout named `…/pin` or
`…/deploy`), reads the PR recorded at `pr-open` and refuses,
one `merge-gate` violation per gap, unless at the PR's head (GitHub's, via `gh api`):

- (a) a `receipt.json` under the step's `evidence/` (as `astra-review.sh collect` writes it) records
  `effective.model` `gpt-6-astra`, `effective.effort` `xhigh`, that `head`, and, when it names
  them, the PR's `repo` and `pr`; and a review that happened: `turn.status` `completed`, null
  `thread_fresh.forkedFromId` and `thread_fresh.parentThreadId`, and `thread_fresh.turns_in_thread`
  1. The receipted report itself must be recorded at that head with `cm review record …
  --reviewer astra`: a recorded copy whose SHA-256 is the receipt's `report_sha256`. (b) then
  holds Astra's own verdict; another report or reviewer's ACCEPT never stands in for it. Every such
  receipt at the head binds: a second, unrecorded one (a rerun) refuses the gate until it too is
  recorded;
- (b) at least one review is recorded at that head for that PR and part (a review recorded for
  one part never clears another), and each one there is ACCEPT or has every blocking finding
  resolved (`cm review resolve`); and, on a model lane (`codex`, `claude-cloud`, `fable`,
  `coordinator`), Astra and a Claude reviewer (`claude`, `opus`, `fresh-opus`, `fable`, `sonnet`,
  `haiku`) have each recorded an explicit ACCEPT there, whichever family wrote the change: the
  computed verdict and the report's stated verdict line are both ACCEPT, and a FIX REQUIRED whose
  findings were resolved is no ACCEPT. (a)'s receipt binds only an Astra review recorded for that
  PR and part. `cm next --act`'s `merge-ready` uses
  the same gate;
- (c) CI is green: GitHub reports at least one check run or commit status on the head and each is
  `success`, `neutral` or `skipped`. Every page of both lists is read; a page that cannot be read,
  or a list that ends short of its `total_count`, leaves CI unknown.

A clean update from the base branch needs no new review, only green CI on the new head: (a) and
(b) may hold instead at an earlier reviewed head H1 when every commit on the first-parent chain
from H1 to the head is a merge whose second parent is on the base branch (GitHub's base tip, else
`origin/HEAD`) and whose `git show --remerge-diff` is empty. When the base tip already contains
the head (the PR has merged into it), no merge can be told apart from the PR's own commits, so
the walk does not run. A merge that resolved conflicts, or
any other commit, needs its own review. The rule only skips a review nobody did: the walk stops
at the first commit, from the head back, that has a recorded review or a finished receipt, and
that commit must clear (a) and (b) itself; an earlier ACCEPT never outweighs it. The walk runs in `--repo` or the step's worktree (for
`cm sync`, the worktree only), which must hold those commits and the base tip; without one, (a)
and (b) must hold at the head itself.

When cm cannot read the PR or its checks from GitHub, (c) refuses unless `--ci-verified <path>`
names a campaign file showing green CI; the head is then `--head`, else the SHA recorded at
`pr-open`. Given while GitHub answers, `--head` must match its head (`merge-head`).
`--override <path>` waives the gate with a non-empty campaign file; the move's journal entry names
the file and every gap it waived. It is discouraged for a red check: a red check it waives is kept
in state, and `cm lint` warns while no known-red exception names that check at that commit.
The gate's flags on any other move are `evidence-unexpected`. Steps in other lanes, and script pins, merge as before. Work that lands as a direct commit with
no PR (campaign-trail tooling) finishes as evidence-only work does: a `coordinator`-lane step
with `done --evidence`, another lane's after `cm amend <step> --study`; neither meets the gate.

### Known-red exceptions

`cm known-red <step> --commit SHA --check NAME --reason T --owner WHO --until A` lets (c) accept
one failing check at one commit. In this form every field is required: the commit is all 40 hex digits and must
be the head of a PR the step (or one of its parts) records; `--until` is a state gate as `--after` takes
it (excluding `pin:`), naming a declared step or release, and must not already hold. cm reads GitHub before it
records the exception and refuses one whose check is absent, is anything but `failure` (an
infrastructure failure: cancelled, timed out, never run), or did not also fail at the PR's base
(green there is a regression this change made; absent, pending or unread there may be one). The merge gate, in `cm advance` and `cm sync`, applies the same
test at merge time: an exception covers a red check only at its exact commit, for its exact check
name, while the check still fails and also failed at the base, and until its closing condition
holds; every other check must still be green. The move journals each exception it used, and
`cm sync` journals once when an exception's closing condition first holds; it stays closed.

Use `--standing` instead of `--commit SHA` to cover any PR head of the step.
Add `--scope campaign` to cover any step in the campaign (`--scope step` is the default).
Standing exceptions use the named check's latest completed run on the base branch.
cm walks the base tip's first-parent history, skipping commits where that check is
absent or still pending. Only a completed `failure` qualifies; a completed green
result or unavailable evidence refuses the exception. The PR head must still fail.
Per-commit exceptions continue to require failure at the current base tip. The same closing condition and
journaling apply; every other check still gates.

### Parts

`cm init <step> --lane codex --part pr1 --part pr2 --part pr3` declares parts at step
declaration time. In kickoff.md, write `Parts: pr1, pr2, pr3` (or `Slices:`). Lint warns
`parts-undeclared` when fewer parts are declared. Names split only at top-level commas:
commas inside parentheses, brackets, or backtick spans stay in the name, so
`Parts: a (x, y), b` names two parts. Backtick spans close with a matching-length run; an unclosed opener is literal.
Launch refuses unless
`--no-parts "reason"` records an explicit exception in the journal.

`cm amend <step> --part pr1 --part pr2 --part pr3` declares a step's required parts, one PR each.
Each part moves on its own (`cm advance <step> pr-open --part pr1 --pr URL --sha S`, and `cm sync`
moves parts through merged, released and deployed as it moves steps); the step's own state rolls
up behind them, moving to a state only once every part rests there, so a gate on the step stays
closed until the last part arrives. Moving the step itself past `launched` names the short parts
(`part-short`). Declaring parts on a step that already moved on hands the step's state and
evidence to the first new part and rests the step at `launched`. A step a watch held below
`deployed` rolls up when the watch resolves. A release step, a study, a Mac-lane step and
an owner-lane step other than a rollout have no parts.

### Pinned evidence

`cm evidence <step> --name N --body-file F --candidate S [--harness H]` pins the evidence to the
commits it was produced against: `S` and `H` are declared steps, each with an optional `@<sha>`
(a multipart step is named by one part, `<step>#<part>`);
without one cm takes the step's open PR head (the head `cm sync` last saw, else the one recorded at
`pr-open`), and a sha given for a step whose PR is open must be that head. When `cm sync` sees the
candidate's or the harness's PR head move off the pinned commit, it marks the pin stale and
journals it. A step stands on every file it pinned and the file it finished on; such a file is
stale while any pin of it, anywhere in the campaign, is stale and the step holds no fresh pin that
renews it. A fresh pin renews a stale one when their candidate and harness steps match exactly,
or when the stale pin names a whole step and the fresh one a part of it (`<step>#<part>`); a part
never renews a sibling part. A stale file fails `cm lint` (`evidence-stale`) on each step that
stands on it, closes every gate on those steps, is listed by `cm next` under `stale`, and refuses
`done --evidence`. A pin naming a step that later becomes multipart goes stale at the next
`cm sync`.

### Watches

`cm watch <step> add --signal log:<query> --deadline 2026-10-27T06:00Z` records a live check the
coordinator cannot cause: the signal is a stored span event, log record or metric
(`span:`, `log:` or `metric:`), never a stdout-only line, and the deadline lies ahead. While a
watch is unresolved the step does not move to `deployed` or `done`, in `cm advance` or `cm sync`.
`cm watch <step> resolve <id> --evidence <path>` closes it, for as long as that file holds: a
deleted or emptied file reopens the watch (`cm lint` names it, `evidence-file`) until resolved anew. Past its deadline an unresolved watch
reads `expired: unresolved`: never a pass, journaled once by the next `cm sync`, warned by
`cm lint`, and listed by `cm next` under `watches` with every open one, so the coordinator reads
it from state instead of polling it.

`cm watch <step> withdraw <id> --reason <text>` closes an unresolved watch as withdrawn and
journals the reason. A withdrawn watch no longer holds `advance` or `sync`, is omitted from
`cm next`'s `watches`, and remains visible as withdrawn in STATE. CARD summarizes watches on
nonfinal steps with counts for open, expired-unresolved and withdrawals within the last 24
hours, then at most 10 rows: open watches by nearest deadline, expired watches by newest
recorded expiry (or deadline before sync records expiry), then recent withdrawals. Further
qualifying watches are counted in an overflow line pointing to unit STATE.md. Rows use computed
status and signals limited to 80 characters. STATE preserves the existing open, resolved and
expired wording and adds a withdrawn row, keeping full signals and history. A resolved or
already withdrawn watch cannot be withdrawn.
`cm watch <step> amend <id> --signal <signal>` replaces an unresolved watch's signal using the
same validation as `add`. It journals the old and new signals and keeps the original deadline,
including an expired deadline. An unchanged signal is refused as `watch-amend-empty`.
When amendment clears a stale resolution, the journal records it. Resolved and withdrawn
watches cannot be amended.

### Releases

A release is a step `00-release/<line>/<major>.<minor>.<patch>`: its packet is the prep PR, and
its name is the only place a tag version comes from. A line is one thing a repository ships under
its own tags; one repository can hold several (`runner` tagged `v<version>` and `guest` tagged
`guest-v<version>` in `xrl/vm-runner`). Each declared version must be above every release already
declared for that line.

`cm init 00-release/<line>/<version> --lane L [--github OWNER/NAME] [--tag PATTERN] [--artifact SPEC]` records the
release's repository and its one tag: `PATTERN` holds `{version}` once. With neither flag,
it inherits the repository and tag pattern from the newest declared release of that line in the
same unit, replacing that release's version in its tag with `{version}`. Without an earlier release,
or when either flag is explicit, the defaults remain `v{version}` and any owner's repository named
`<line>`. `cm amend <release step> --github OWNER/NAME --tag PATTERN` changes either declaration
while the step rests at drafted, launched, pr-open or merged, refusing once released; its prep PR
must still be in that repository. A release step declared before cm 0.3.1 has neither recorded; declaring it again with the same lane and `--after`
records its `--tag` and `--github` once. Once the prep
PR is `merged`, `cm next --json` lists it under `tags` with the version, the merge commit and
`tags`, the one declared tag name. If the prep PR is re-cut, `cm advance <release step> merged --sha S`
re-records its merge while the step is already `merged`, provided S descends from the recorded
merge in the step's worktree or `--repo` checkout. It journals the old and new SHAs, preserves
the phase, timestamp and other evidence, and refuses another lineage (`merge-lineage`). This
evidence update does not repeat the merge gate; non-release steps still refuse `merged` again.
`cm sync` records `released` when that tag exists in that
repository and contains the prep merge, and moves the repository's merged steps other than
rollouts that the tag contains along with it. `cm advance <step> released --tag T` also requires T to contain the
recorded merge SHA, refusing with `tag-lineage` otherwise. A release cut without a prep PR
moves from `drafted` or `launched` with `released --tag T --sha S`; T must contain S.
These checks use `--repo` or the step's worktree, resolving the commit and tag with Git and
checking ancestry. Without a checkout, cm uses GitHub's tag comparison in the recorded PR's
repository or the release's declared `--github` repository; an unread comparison refuses the move.
Release steps can declare repeatable `--artifact crate:acme-sdk`, `--artifact tap:widget`,
and `--artifact image:ghcr.io/acme/widget` flags, stored in declared order. Duplicate or malformed
specifications refuse as `artifact`; a non-release step refuses them as `artifact-step`.
Crate names contain 1–64 ASCII letters, digits, `_` or `-`. A tap is a formula name or
`owner/repo/formula`; a bare formula uses the release repository owner's `homebrew-tap`.
Images must be on ghcr.io and carry neither a tag nor a digest.

After tag containment succeeds, `cm sync` reads every declared artifact. A crate must have the
exact version and be unyanked. Every tap `url` must name the release repository's download at
the declared tag, followed by a lowercase 64-hex `sha256`; any explicit `version` must match.
The tap is read on its default branch. Quoted and parenthesized `url` and `version` calls
are checked alike. Each argument must be exactly one double-quoted literal with no
backslash, `#`, or control character, followed only by line end or its closing parenthesis;
URL calls may also use `using: :curl` inside that call. The literal characters are read as-is.
Expressions, escapes, interpolation, percent literals, single quotes, and heredocs are
unsupported and read as unknown in the `url`, `version` and `sha256` arguments. Only lines
that start with `url`, `sha256` or `version` are read; a call elsewhere on a line, or code
that turns those lines into data, is not detected. Ruby is never evaluated.
Every URL must be a release download from that repository; unrelated
`livecheck`, `head` or `resource` URLs and unsupported Ruby expressions read as unknown.
This check expects a generated release formula rather than arbitrary Homebrew Ruby.
The checksum is not compared with a release asset.
An image must have a manifest at `v<version>`, regardless of the git tag pattern. Crates and
images use read-only `curl` requests; the tap uses `gh api`. Each request has a 60-second limit.

Only when every artifact is present does the release move, atomically recording `tag` and
`evidence.published`: crate name and version, resolved tap formula and blob SHA, or image and
digest. Missing, yanked or stale artifacts raise `release-unpublished` and leave the step
merged for the next sync. Unreadable responses, timeouts and rate limits journal `unknown`
and raise `sync-unknown`; they leave the step in place. Such a release supplies no shipped
release to other steps. With no declared artifacts, the existing tag behavior is unchanged.

Hand advance to `released`, including a tag-only release or a resume, makes the same reads.
It refuses missing artifacts as `release-unpublished`, and unreadable ones as `publication-unread`,
changing no state. There is no publication override. Add declarations with
`cm amend <release step> --artifact SPEC`, or remove them with `--drop-artifact SPEC`;
these changes are journaled and refused once released. `cm lint` warns
`release-artifacts-undeclared` for drafted or launched release steps with no declarations;
this warning is never a gate.

The tick's smoke receipt is
`cm advance 00-release/<line>/<version> deployed --digest …`; the next sync carries `deployed` to
every step other than a rollout that a deployed release of that line contains, so a fix-forward release deploys what the one
before it shipped.

## Releases

Pushing a `cm-X.Y.Z` tag releases `X.Y.Z`. To cut one, merge a PR that sets `version` in
`Cargo.toml` (and `Cargo.lock`) to `X.Y.Z`, then tag that commit on `main`:

```sh
git tag cm-X.Y.Z <sha-on-main> && git push origin cm-X.Y.Z
```

The run refuses a tag that is not `cm-<major>.<minor>.<patch>`, does not match `Cargo.toml`, or
does not point at a commit on `main`. It builds `aarch64-apple-darwin`,
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, packages each as
`cm-<version>-<target>.tar.gz` and an unversioned copy `cm-<target>.tar.gz` (each with a
`.sha256`), attests the archives, creates the release (or uploads into one already made for the tag, e.g. from the GitHub release page) and pushes `Formula/cm.rb`, rendered from
the versioned checksums only, to `dekopon-agents/homebrew-tap`. Merging to `main` runs CI only.

## License

Licensed under either of Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT
license ([LICENSE-MIT](LICENSE-MIT)) at your option.

## Attention at resume and handoff

`cm attention` makes one read-only GitHub observation for explicit PRs and routes the next
step to a named worker or coordinator. It works for prose-only campaign items; no unit
initialization or lifecycle migration is required. Use an authenticated GitHub CLI on PATH:
every read is a REST `gh api` call, so it works where GraphQL is blocked. GitHub.com PR URLs are
required. The required checks are the base branch's required status contexts, from branch
protection and rulesets, each looked up among the head's check runs and commit statuses; a
required context the head does not report is `MISSING`.

```sh
cm -C /path/to/campaign attention \
  --pr https://github.com/example/project/pull/41 \
  --worker implementation-worker --coordinator campaign-coordinator --record
```

Repeat `--pr` for up to 20 PRs with the same owners; invoke separately for other owners.
The labels name responsible people or sessions, not GitHub assignees or notification
recipients. The command sends no messages, runs no fixes, and never merges or advances
campaign state. Coordinators run it at resume and worker handoff, then route routine
work under existing authority. Genuine scope, funding and permission decisions still
belong in OWNER-QUEUE.md.

A failing required check routes to the worker. A changed head invalidates previous
readiness and routes to the coordinator for review. Closed or merged PRs prompt
reconciliation of obsolete work. Passing reported required checks only requests review
of the exact head and authority: it is never merge approval. Missing, skipped, neutral,
unknown or inaccessible check evidence is not green. A non-CLEAN GitHub merge status
also requires verification, even if the reported checks passed: missing required checks
and other merge blockers must be resolved. Drafts remain work for their worker.

PR metadata is read before and after required checks; a changed observation is unknown
and must be repeated. GitHub can change after any observation, so recheck the actual
head and gates before a later write. The command reports GitHub's required-check view,
not a replacement for repository policy or independent review.

Without `--record`, only output is produced. With it, changed evidence or ownership is
appended to JOURNAL.md through cm's lock, with the latest observation
in `.cm/attention.json`. Repeating identical evidence adds nothing. These observations
are separate from lifecycle transitions. `--json` includes owners, action, head, check
links, the local observation timestamp, and GitHub's update/merge/close/check timestamps;
a PR update time is not a CI completion or readiness time. Preserve action receipts with
`cm journal` so later retros can connect observed blockers to actual actions. Neither
elapsed observation intervals nor transition ages measure human effort.

Unknown evidence exits 1 with actionable output; normal waiting, failed CI, or a review
request exits 0 because inspection succeeded. Invalid usage is refused. Raw GitHub CLI
stderr is not copied into campaign records; diagnose access using `gh` directly.
