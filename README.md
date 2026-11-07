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
error.

| Command | Contract |
|---|---|
| `cm init [DIR]` | Writes `RESUME.md`, `JOURNAL.md`, `LIMITS.toml`, `OWNER-QUEUE.md` and `.cm/state.json` from templates embedded in the binary. Never overwrites: in an existing folder it writes only what is missing and adopts each existing file as the baseline once it lints clean. `cm init <NN-name>/<unit>` inside a campaign writes the unit's `.cm/state.json` and its rendered `STATE.md`. |
| `cm init <NN-name>/<unit>/<step> --lane L [--after A]… [--study] [--github OWNER/NAME] [--tag PATTERN] [--kickoff-file F]` | Declares a step `drafted` in lane `pi`, `codex`, `claude-cloud`, `fable`, `owner` or `coordinator` (a step the coordinator session runs itself), creating the unit if needed. `--study` marks a step that ships no PR: like an `owner`-lane step it finishes with `cm advance <step> done --evidence <path>` (see [Finishing](#finishing)). Each `--after` is one gate (see [Order](#order)); every step or release it names must already be declared, so the order has no cycles. `--kickoff-file F` writes `kickoff.md` from a non-empty file of at most 1 MiB (`-` reads stdin), never replacing an existing one, and journals one line. `--github` and `--tag` declare a release step's repository and tag (see [Releases](#releases)). A relative folder resolves against `-C` when it is given. A declaration is changed only by `cm amend`: a different one is `step-declared`, and the same one again repairs what an interrupted declaration left half written (the step folder, a missing kickoff when `--kickoff-file` is given, a missing or stale `STATE.md`) without replaying the journal, or else is a no-op. |
| `cm next [<NN-name>/<unit>/<step>] [--lane L]` | Numbers every item with its next cm command (`command` on each JSON item), shared with CARD.md’s Next, Waiting and Blocked sections; named inputs appear as quoted placeholders (see [Order](#order)). Lists the declared `drafted` steps whose every `--after` is met and whose folder has `kickoff.md` (`steps`), the rest with what each waits on (`waiting`), and the merged release steps that wait for their tag (`tags`). It also flags (`stalled`) each `codex`-lane step at `launched` with no PR whose attempt was claimed more than `codex_stall_hours` ago (LIMITS.toml, default 2; the launch stamp for a step launched without an attempt), and (`dirty`) each `pr-open` step whose PR the last `cm sync` saw with `mergeable_state: dirty`; neither is a violation. Nothing is next while `LIMITS.toml` is paused. `tags` is in version order. Given a step, it refuses (exit 1) unless that step is next, naming every unmet gate, and lists a refused step under `waiting`, never `steps`. |
| `cm sync` | Reads GitHub with read-only `gh api` calls, each killed after 60 seconds, without holding `.cm/lock`; then it takes the lock, reloads the state and applies the moves that still hold. It moves steps along: `pr-open` → `merged` when the PR merged (the merge commit and GitHub's optional `merged_at` are the evidence; `since` remains the sync time), a release step `merged` → `released` when its declared tag exists in its repository and contains the prep merge, any other step `merged` → `released` by the first release of its repository whose tag contains its merge, and `released` → `deployed` once a `deployed` release of the same line contains its merge. One journal entry lists the moves and new notifications; a sync with neither journals nothing. It refreshes the marked status span in `RESUME.md` from state (step counts, newest released and deployed version per release line, and epoch status), preserving all owner prose and silently leaving files without markers alone. It rewrites `CARD.md`, whose first line is `Last run: <stamp>`, and `.cm/pulls.json` with what it saw of each open PR (head, `mergeable_state`; a PR GitHub did not answer for keeps its last observation and stamp while its step rests at `pr-open` on it), and reports `stalled` and `dirty` as `cm next` does. A `codex`-lane step whose PR merged moves to `merged` only when its merge gate holds at the PR's head; otherwise it stays `pr-open` and each unmet condition is a `merge-gate` violation. A PR closed unmerged (`pr-closed`) or an observation `gh` could not make (`sync-unknown`) exits 1 after the rest is applied. |
| `cm epoch planning\|working\|closed [--skill-revision R] [--plan-revision S]` | Moves `EPOCH.md`'s front matter one status along `OPEN → PLANNING → WORKING → CLOSED`, stamps `planning_started_at`, `working_started_at` or `closed_at` with the date, sets the pins it is given, journals the move, and refreshes RESUME.md’s generated status span. It refuses a null pin: PLANNING needs `skill_revision`, WORKING and CLOSED also `approved_plan_revision`. `closed` follows `cm close`. |
| `cm journal "<title>" [--body-file F]` | Appends `## YYYY-MM-DDTHH:MMZ — <title>` and the body (`-` reads stdin). The only way `JOURNAL.md` grows. A body may not hold a `#` or `##` heading. |
| `cm launch <NN-name>/<unit>/<step> [--worktree PATH] [--coordinator LABEL] [--cap-min N]` | Runs the launch checks and reports every failure: `LIMITS.toml` not paused; a declared step's every `--after` met (`after-unmet`); for the local `pi` lane and undeclared steps, free disk on `disk_volume` minus `kache_headroom_gib` over `min_free_disk_gib`, live heavy builds (top-level `cargo` processes) under `max_heavy_builds` and the worktree, which they require; the step folder has `kickoff.md`; every path its packet names exists. A drafted step, a step blocked before its PR opened, or a launched step whose last attempt has ended can launch; relaunch picks the newest `resume*.md` when present and increments `launches`. The `--after` check runs on every launch, including relaunches. Then it marks the step (and a drafted unit) launched, journals the launch and, for a local step, prints the command; every other lane (`owner` and `coordinator` among them) prints only the step and its prompt and records its attempt (see [Attempts](#attempts)). It spawns nothing. When `--coordinator` is omitted, the label defaults to trimmed `CLAUDE_CODE_REMOTE_SESSION_ID` with a leading `cse_` mapped to `session_`, else `—` when unset or whitespace-only; an explicit label wins. Launch refuses an empty or multi-line label, explicit or inherited. A step whose last attempt has not ended does not launch again (`attempt-open`). |
| `cm attempt <NN-name>/<unit>/<step> [--claim TOKEN] [claim [--coordinator L] --expect G \| begin KIND \| record KIND ID \| restart --lane L [--evidence PATH] \| end complete\|partial\|unknown [--evidence PATH]]` | Shows a remote attempt and what to do next from committed state alone, or extends it; each change is journaled. See [Attempts](#attempts). |
| `cm unblock <NN-name>/<unit>[/<step>] [--evidence PATH]` | Releases a `blocked:<reason>` track to the state it was blocked from. Omit `--evidence` only for a step whose `blocked:until:<condition>` holds; the condition is journaled. Other blocks require a campaign evidence file, as for `done`. A step that rests at `drafted` or `launched` still waits on every `--after`: `cm next` and `cm launch` check them again, and the move lists what is still unmet. |
| `cm amend <NN-name>/<unit>/<step> [--study] [--lane L] [--after A]… [--drop-after A]… [--part NAME]… [--kickoff-file F] [--github OWNER/NAME] [--tag PATTERN]` | Changes a declared step that rests at `drafted` or `launched`: marks it a study, moves its lane, or removes gates with `--drop-after` before adding gates with `--after` (each declared, never closing a cycle: `after-cycle`). Removing a gate it lacks is `after-absent`. `--kickoff-file F` replaces `kickoff.md` from a non-empty file of at most 1 MiB (`-` reads stdin) and journals one line; it refuses a step past launched. `--github` and `--tag` change a release step's repository and tag pattern through `pr-open` and `merged`, refusing once released; its prep PR must be in that repository. `--part` declares required parts (see [Parts](#parts)); given alone it also amends a step that has moved on, short of final. Journals the change; a change that changes nothing is `amend-empty`. |
| `cm advance <NN-name>/<unit>[/<step>] <state> [--sha S] [--pr URL] [--tag T] [--digest D] [--evidence PATH] [--by STEP[,STEP…]] [--repo PATH] [--head SHA] [--ci-verified PATH] [--override PATH] [--part NAME]` | Moves one state along `drafted → launched → pr-open → merged → released → deployed → done`, or to `blocked:<reason>`; an `owner`-lane or study step, or a non-release `coordinator`-lane step without a recorded PR, SHA or declared parts, moves to `done` from `drafted` or `launched` with `--evidence` (see [Finishing](#finishing)). `superseded --evidence PATH [--by STEP[,STEP…]]` retires a step replaced by other work from any state short of final, recording its declared successors in state and the journal and showing them in `STATE.md` and `CARD.md`; it meets no gate, so its consumers are amended. A release step moves to `released --tag T --sha S` from `drafted` or `launched` when the tag was cut without a prep PR (tag-only); T must contain S. Other releases require T to contain the recorded merge SHA (`tag-lineage`). Refuses a skipped state and a state without its evidence, journals the move and re-renders `STATE.md`. A `codex`-lane step moves to `merged` only through its merge gate (see [Merge gate](#merge-gate)). An already `merged` release step accepts `merged --sha S` again when S descends from its recorded merge, updating only its evidence SHA and journaling both SHAs; otherwise it refuses (`merge-lineage`). `--part NAME` moves one part of a multipart step (see [Parts](#parts)). A step with an unresolved watch does not move to `deployed` or `done` (`watch-open`, see [Watches](#watches)), and `done --evidence` refuses a stale pinned file (`evidence-stale`). |
| `cm known-red <step> --commit SHA --check NAME --reason T --owner WHO --until A` | Records a known-red exception for the step's merge gate (see [Known-red exceptions](#known-red-exceptions)) and journals it. |
| `cm watch <step> add --signal span:Q\|log:Q\|metric:Q --deadline T`, `cm watch <step> resolve ID --evidence PATH`, `cm watch <step> withdraw ID --reason TEXT`, `cm watch <step> amend ID --signal SIGNAL` | Records a pending live read that holds the step below `deployed`, closes it with evidence or a withdrawal reason, or corrects its signal (see [Watches](#watches)). |
| `cm close` | Seals the campaign. Refuses, listing every gap, while `OWNER-QUEUE.md` or `MAC-LANE.md` has an open item, `MAC-LANE.md` has a numbering gap or repeat (`mac-lane-number`), or any unit or step is short of `done` or `blocked:<reason>`. Then it stamps `closed` in `.cm/state.json`, journals the close, and refreshes RESUME.md’s generated status span to show closed. From there unit `cm init`, `cm launch`, `cm advance` and a second `cm close` refuse with `campaign-closed`; `cm journal` stays open for follow-up observations. |
| `cm feedback "<title>" --urgency urgent\|queued --body-file F` | Appends `### FB-<sequence>-NNN · <urgency> · <title>` and the body to `FEEDBACK.md`, numbering on from the highest id of the letter in `EPOCH.md`'s `sequence:`, and journals a line naming it; an empty body is `feedback-empty`. `--follow-up <id> --disposition proposed\|accepted\|applied\|deferred\|rejected` appends `### <id> follow-up · <disposition>: <title>` to an existing id. Hand-written entries (dated `##` sections, `FB-` sub-entries) still parse. Log feedback whenever a file would otherwise be appended by hand. |
| `cm owner-queue add "<title>" --context T --question T --recommendation T` | Appends `## N. <title>` (numbering on from the highest item in the queue or any journaled owner-queue add, answer or strike; numbers are never reused), its `Context:` line, the question, a `Recommendation:` line and an empty `Answer:` to `OWNER-QUEUE.md`; refuses an empty field or a RESUME.md with no `>` banner (`resume-banner`), sets RESUME.md's banner to `OWNER-QUEUE not drained: /loop withheld` and journals the item. A leading `Recommendation:` in `--recommendation` (any case) is stripped once. `cm owner-queue answer N --body-file F` journals a non-empty answer and removes item N; `cm owner-queue strike N --reason T` removes it and journals the reason; `cm owner-queue edit N --body-file F` replaces its body, keeping the heading and requiring the item fields. `-` reads either body from stdin. Unknown numbers are refused; other items keep their numbers. When a removal drains the queue, the withheld banner line is removed and the session is told to print the resuming `/loop`. |
| `cm mac-lane add "<title>" [--body-file F]`, `cm mac-lane done N --receipt T`, `cm mac-lane list` | The queue of work only the owner's Mac can do, `MAC-LANE.md`: one numbered list item per request, starting at column 0: `N. **<title>** <body>`. `add` appends the next number (creating the file with `# Mac lane queue` when it is missing), indents the body's continuation lines four spaces and journals the item; a title holding `**` or beginning with a status word (`DONE`, `WITHDRAWN`, `HELD`, `RUNNING`, `Receipt`) is `mac-lane-title`, so a new request cannot be mistaken for a status. An exhausted item number is `mac-lane-number`. An item is done once its leading bold is `**DONE …**` (or `**WITHDRAWN …**`); any other item is open, `held` (a leading `**HELD …**` or a `**Receipt …**` naming a hold), `running` (`**RUNNING …**`) or plain `open`, and its title is its first other bold span. `done` writes `**DONE <YYYY-MM-DD>** (<receipt>). ` after `N. `, replacing a leading open status, keeps the remaining item text, trims trailing whitespace on a status-only first line and journals it; an absent item is `mac-lane-item`, a done one `mac-lane-closed`. `list` prints each open item as `N. <title> (<state>)` (`items` in `--json`). `cm sync` summarizes the open count and at most 10 items under `## Mac lane` on `CARD.md`, with titles limited to 80 characters and a pointer to `MAC-LANE.md` for the rest; omitted when none is open. Unreadable queues fail the read, report `mac-lane-unreadable` in lint and refuse campaign close. An unreadable or invalid lane leaves sync running and renders `Mac lane: invalid, see cm lint` on CARD.md. |
| `cm evidence <NN-name>/<unit>/<step> --name NAME --body-file F [--candidate STEP[@SHA] [--harness STEP[@SHA]]]` | Writes `<step>/evidence/<NAME>.md` once from a non-empty body and journals the path. `cm advance <step> done` without `--evidence` takes the step's one evidence file when it has exactly one. `--candidate` and `--harness` pin the file to the commits it was produced against (see [Pinned evidence](#pinned-evidence)). |
| `cm review <step> --pr URL --head SHA --model ID --effort E --verdict accept\|fix-required --findings C/F --cost USD --wall MIN [--delta FROM_SHA]`, `cm review --import` | Appends one row to the review ledger `.cm/reviews.json` and renders `REVIEW-OFF.md` from it, rows in PR order with a delta review under its PR, and journals the row. The rows a hand-written `REVIEW-OFF.md` already holds are imported once, kept cell for cell, and the text before and after the table (per-PR adjudications, rules) is kept byte for byte (`--import`, or the first `cm review`). The step must be declared (`step-unknown`); an imported delta row is the one whose `#` cell carries `Δ`. After that a hand edit is `review-hand-edit`, as for `STATE.md`. |
| `cm review record <step> <report-file> [--reviewer KIND] [--part NAME]` | Parses the `cm-review` block that ends the report (see [Review verdicts](#review-verdicts)), computes the verdict, copies the report to `<step>/evidence/reviews/R<n>[-KIND].md` and records the review in `.cm/verdicts.json` as `R<n>`, its findings as `R<n>.<id>`. Refuses a report with no block or a malformed one (`review-block`, one per problem, with its line), an undeclared step and a report already recorded for the step (`review-duplicate`). A report whose own verdict line disagrees with the computed one is recorded with the computed verdict and the disagreement printed and journaled. `--part` selects a declared part and its PR; it is required on multipart steps. Distinct FIX REQUIRED heads on the current PR count toward the ten-pass step cap; rising blocking counts request an independent reader and only a non-converging `fresh-opus` round blocks the step (see [Rounds](#rounds)). |
| `cm review resolve <step> <finding> --by <commit\|ruling-file>` | Records what resolved a blocking finding: `R<n>.<id>`, or `<id>` in the step's latest review holding it; an id whose `R<n>` is not a review number names no finding (`finding-unknown`). `--by` is a ruling file in the campaign when that path exists, else a commit SHA. A follow-up is `resolve-not-blocking`; a second resolution is `resolve-done`. |
| `cm ruling <step> "<text>"` | Appends `- YYYY-MM-DD: <text>` to `<step>/evidence/coordinator-scope.md`, creating it with `# Scope and coordinator rulings: <step>` when it is missing, and journals the line. |
| `cm brief <step>` | Prints the step's `kickoff.md`, then `## Coordinator scope and rulings` and the step's `evidence/coordinator-scope.md` (or a line saying none is recorded): the brief for `astra-review.sh start` and review packets. `--json` puts it in `brief`. Refuses a step with no `kickoff.md`. |
| `cm followups [--epoch]`, `cm followups close <id> --by <commit\|reason>` | Lists every non-blocking finding `cm review record` kept, across steps: id, step, PR, type, origin, `file:line`, summary and status (`open` or `closed`). `--epoch` lists only the open ones, grouped by repository as Markdown checklist lines, for the epoch's cleanup PR. `close` closes one with the commit that fixed it or the reason it stays, and journals it; a blocking finding is `followup-blocking`, a closed one `followup-closed`. |
| `cm lint [DIR]` | Template conformance, cm-owned files against `.cm/state.json`, the journal against git, `.cm/JOURNAL.md` and `.cm/lock` out of git (`cm-tracked`), the path check for every drafted or launched packet, a unit or step folder git ignores (`git-ignored`, or `git-ignored-unknown` when `git check-ignore` cannot answer, as outside a repository), and a unit folder holding `STATE.md` but no `.cm/state.json` (`unit-state-json-missing`), which is never counted as a smaller campaign; unique `FEEDBACK.md` ids whose follow-ups name existing ones; `MAC-LANE.md` items numbered 1, 2, 3… with no gap or repeat (`mac-lane-number`), nothing about their text; `REVIEW-OFF.md` against its ledger; stale pinned evidence (`evidence-stale`). It warns, without failing, on an expired watch and on an `--override` of a red check no known-red exception names. In a closed campaign, also an empty owner queue, no open Mac lane item and every track done or blocked; its packets are no longer checked, since nothing launches there. |

## The folder

```
<campaign>/
  RESUME.md  JOURNAL.md  LIMITS.toml  OWNER-QUEUE.md  CARD.md  [EPOCH.md]  [MAC-LANE.md]
  .cm/state.json  .cm/.gitignore
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
| `LIMITS.toml` | `paused` (bool); `runaway_ceiling_usd`, `min_free_disk_gib`, `kache_headroom_gib`, `pr_ci_expected_min`, `pr_ci_deadline_min` (non-negative numbers); optionally `codex_stall_hours` (a non-negative number, default 2); `max_heavy_builds` (non-negative integer); `driver_model`, `driver_effort`, `verifier_model`, `supervisor_model`, `supervisor_effort`, `disk_volume` (non-empty strings). Other keys are allowed. |
| `OWNER-QUEUE.md` | First heading `# Owner queue`; each `##` item has a line starting `Context:`, one starting `Recommendation:` and one starting `Answer:`. A queue written before cm 0.4 whose items lack `Context:` is reported, never rewritten. |
| `MAC-LANE.md` (optional) | Items start at column 0 as `N. title` or bare `N.`, with decimal digits for N. Other lines, including indented steps, are item text. Numbers run 1, 2, 3… in order; gaps, repeats and numbers above the supported range violate `mac-lane-number` and make `cm close` refuse. Item text is unrestricted. |
| `<unit>/STATE.md` | Byte-equal to the render of the unit's `.cm/state.json`. |
| Packets | In `kickoff.md`, `step-*.md` and `resume*.md` of a drafted or launched step, every absolute or `~/` path inside backticks exists. A span with whitespace, a glob or placeholder character (`* ? < > $ { } [ ] \|`), or a single component such as `/loop` or `//!` is not a path; a trailing `:line` or `:start-end` is dropped. Only spans that could name a file are checked: a cm id (`01-a/u/S1`, `00-release/cm/0.5.0`), a URL and a route (an absolute path with no file extension whose first directory is not on this machine, such as `/v1/sessions/abc`) are not. Fenced blocks are not read. |
| Tracked files | Warns `ledger-size` for each tracked regular file under the campaign folder exceeding 1 MiB, excluding `JOURNAL.md` and `.cm/` at any depth. Exactly 1 MiB, untracked files, and campaigns outside git are silent. Warnings do not change the exit status. |

### State files

Campaign `.cm/state.json`:

```json
{
  "schema": 5,
  "kind": "campaign",
  "initialized": "2026-10-08T07:04Z",
  "written": ["RESUME.md", "JOURNAL.md", "LIMITS.toml", "OWNER-QUEUE.md"],
  "adopted": [],
  "journal": { "baseline_lines": 3 },
  "closed": "2026-10-15T18:00Z"
}
```

`closed` appears only after `cm close`. Beside it, `.cm/verdicts.json` is the computed-verdict
ledger (see [Review verdicts](#review-verdicts)) and `.cm/pulls.json` the open PRs the last
`cm sync` saw; a campaign without them reads as having none. `written` are files cm wrote, `adopted` are files that existed and linted clean at `cm init`.
`journal.baseline_lines` is where cm's stamped region starts; an adopted journal keeps its old
headings.

Unit `.cm/state.json`:

```json
{
  "schema": 5,
  "kind": "unit",
  "unit": "01-first/alpha",
  "state": "launched",
  "since": "2026-10-08T07:04Z",
  "steps": {
    "BUILD": {
      "state": "merged",
      "since": "2026-10-08T08:10Z",
      "evidence": { "sha": "<40 hex>", "pr": "https://github.com/o/r/pull/7", "merged_at": "2026-10-08T07:50:42Z" },
      "worktree": "/abs/path",
      "launches": 1,
      "lane": "pi",
      "after": ["01-first/alpha/PLAN", "tag:dekopon@0.49.0"]
    }
  },
  "history": [
    { "at": "2026-10-08T07:04Z", "target": "01-first/alpha/BUILD", "from": "drafted", "to": "launched" }
  ]
}
```

`since` is the time cm observed or advanced the state. Optional `evidence.merged_at` is
GitHub's merge time, recorded by `cm sync` or `cm advance <step> merged --sha <sha> --pr <url>`;
`--pr` must match the recorded PR. An absent merge time stays absent. For merged tracks,
`STATE.md` and `CARD.md` show `merged_at`, falling back to `since`; `cm close` lists historical
merges in its journal entry using `merged_at`, falling back to the move's observation time.

`lane` and `after` appear on declared steps (`cm init <step> --lane`); a declared step that has not
launched has no `worktree` and `launches: 0`, and `STATE.md` adds an `Order` table. A blocked track also carries `blocked_from`. From `blocked:<reason>` a track resumes to the state it
was blocked from, or moves to that state's successor; a step blocked before its PR relaunches with
`cm launch`, which picks the newest `resume*.md`.

### Evidence

| State | Requires | Takes | Checked |
|---|---|---|---|
| `launched` | `cm launch` only | | the launch checks |
| `pr-open` | `--pr`, `--sha` | | URL is `https://github.com/<owner>/<repo>/pull/<n>`; the SHA resolves to a commit |
| `merged` | `--sha` | `--pr` | SHA resolves to a commit; a PR URL records GitHub’s merge time when available |
| `released` | `--tag`; also `--sha` for a tag-only release from `drafted` or `launched` | `--digest` | the tag exists and contains the recorded merge SHA, or the given tag-only SHA; a release step's tag is its declared tag |
| `deployed` | `--digest` | | `sha256:<64 hex>` |
| `done` | `--evidence` for an `owner`-lane or study step, or a non-release `coordinator`-lane step without a recorded PR, SHA or declared parts; nothing for a code step at `deployed` | | a non-empty regular file inside the campaign, outside `.cm/`; recorded relative to it |
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
are the same as `--after` (`<step>`, `pr-open:<step>`, `tag:<line>@<version>`,
`deployed:<line>@<version>`), plus `epoch:WORKING` and `owner-queue:<n>` (item
heading removed from OWNER-QUEUE.md). `cm next` lists satisfied blocks under
`unblock now` (`unblock_now` in JSON), with a numbered
`cm unblock <step>` command. It rechecks the condition and, when it holds, resumes without
an evidence file and journals `- condition: until:<condition> holds`. An unmet condition or
a free-text block requires `--evidence PATH`. `cm sync` journals the first observation;
it leaves the step blocked for an explicit resume. CARD shows whether the condition
holds. Lint warns `block-stale` for free-text blocks older than 24 hours without a
later journal title naming the step.

A declared step is handed out by `cm next`, and launched by `cm launch`, only once every gate is
met. `--after` takes:

| Gate | Met when |
|---|---|
| `<step>` or `<NN-name>/<unit>/<step>` | that step is `merged` or later |
| `pr-open:<step>` | that step is `pr-open` or later (a chain that builds on an open PR) |
| `tag:<line>@<version>` | the release step `00-release/<line>/<version>` is `released` or later |
| `deployed:<line>@<version>` | that release step is `deployed` or later |

A step or `pr-open:` gate on an `owner`-lane or study step, or a non-release `coordinator`-lane step
without a recorded PR, SHA or declared parts, is met only when that step is `done`.
A blocked step meets the gates of the state it was blocked from. A bare step name is in the declaring step's unit.

`cm next` numbers each action and prints its exact cm command; JSON carries the same string
as `command` on each item. CARD.md uses the same renderer and numbers its Next, Waiting and
Blocked lines across those sections. Run commands from the campaign folder, or append `-C DIR`.
Replace quoted placeholders such as `'<watch-evidence-file>'` with the named input.
Ready owner steps offer `cm advance <step> done --evidence '<owner-evidence-file>'`.
Ready local steps name a worktree placeholder. Waiting steps offer `cm next <step>` to recheck
their gates, or `cm amend --kickoff-file` when the kickoff is missing. Stalled launches offer
`cm attempt <step>` to inspect the attempt before acting; dirty PRs offer `cm attention` with
worker and coordinator session placeholders. Tags offer `cm sync` to observe the tag after
it is cut. Stale evidence offers `cm evidence` with a fresh name and body, pinned to the current
candidate and harness heads, or named SHA inputs when no head resolves. A revision whose step
became multipart also names the required part input. An `until:` block offers `cm next`
while its condition is unmet; that command lists the unblock action once it holds. Free-text
blocks offer `cm unblock`.

### Finishing

A code step finishes at `deployed`, or `merged` where it has no release; `cm advance <step> done`
after `deployed` is unchanged. An `owner`-lane step or one declared with `--study` ships no PR.
A non-release `coordinator`-lane step without a recorded PR, SHA or declared parts also finishes
with evidence:
`cm advance <step> done --evidence <path>` moves it to `done` from any state short of `done`
(blocked steps from where they rest). The path must be a non-empty regular file inside the
campaign folder and outside every `.cm/` folder (`evidence-file`); it is recorded, relative to the
campaign, in the move's evidence beside its stamp, and `cm lint` re-checks it every run. From
`drafted` the step's own `--after` must be met. `done` without `--evidence` on such a step is `evidence-missing`;
`done --evidence` on a code step, or `done` on one short of `deployed`, is `done-code-step`. A
release step cannot be a study. A `coordinator`-lane release step keeps the code-step ladder even
without a recorded tag, as does a coordinator step with a recorded PR, SHA or declared parts.

`cm advance <step> superseded --evidence <path> --by <step>[,<step>…]` retires a step
and records its successors in the track's `evidence.superseded_by`, its move history and the
journal. `STATE.md` and `CARD.md` show the successor references. Every successor must already
be declared; a short name resolves in the retiring step's unit, and a full
`<NN-name>/<unit>/<step>` can name a step in another unit. A step cannot name itself. Repeated
successors are recorded once, in the order given. `--by` is optional for retirements without a
named successor, and is a usage error with any state other than `superseded`. `--evidence`
remains required. Superseding meets no gate; amend consumers to name the replacement work.

cm 0.10 raises SCHEMA from 4 to 5 for successor references and optional merge times. It writes
every unit state, and a new campaign state, as schema 5. Older state remains readable; missing
successor references default to an empty list and missing merge times remain absent. Upgrade every reader before writing shared state with cm 0.10.

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
on the attempt. It reads `dispatch`; an expired cap refuses restart. An action begun
without an id must first be closed with `end unknown`. Restart leaves the declared step
lane unchanged. Begin, record and restart each write one stamped journal line; begin
and record include the attempt id, action kind, generation and returned id when present.

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

`cm advance <step> merged` for a `codex`-lane step reads the PR recorded at `pr-open` and refuses,
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
- (b) at least one review is recorded at that head, and each one there is ACCEPT or has every
  blocking finding resolved (`cm review resolve`);
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
The gate's flags on any other move are `evidence-unexpected`. Steps in other lanes merge as before.

### Known-red exceptions

`cm known-red <step> --commit SHA --check NAME --reason T --owner WHO --until A` lets (c) accept
one failing check at one commit. In this form every field is required: the commit is all 40 hex digits and must
be the head of a PR the step (or one of its parts) records; `--until` is a gate as `--after` takes
it, naming a declared step or release, and must not already hold. cm reads GitHub before it
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
`deployed` rolls up when the watch resolves. A release step, a study and an
owner-lane step have no parts.

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

`cm init 00-release/<line>/<version> --lane L [--github OWNER/NAME] [--tag PATTERN]` records the
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
repository and contains the prep merge, and moves the repository's merged steps that the tag
contains along with it. `cm advance <step> released --tag T` also requires T to contain the
recorded merge SHA, refusing with `tag-lineage` otherwise. A release cut without a prep PR
moves from `drafted` or `launched` with `released --tag T --sha S`; T must contain S.
These checks use `--repo` or the step's worktree, resolving the commit and tag with Git and
checking ancestry. Without a checkout, cm uses GitHub's tag comparison in the recorded PR's
repository or the release's declared `--github` repository; an unread comparison refuses the move.
The tick's smoke receipt is
`cm advance 00-release/<line>/<version> deployed --digest …`; the next sync carries `deployed` to
every step a deployed release of that line contains, so a fix-forward release deploys what the one
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
