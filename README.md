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
| `cm init <NN-name>/<unit>/<step> --lane L [--after A]… [--study] [--github OWNER/NAME] [--tag PATTERN]` | Declares a step `drafted` in lane `pi`, `codex`, `claude-cloud`, `fable`, `owner` or `coordinator` (a step the coordinator session runs itself), creating the unit if needed. `--study` marks a step that ships no PR: like an `owner`-lane step it finishes with `cm advance <step> done --evidence <path>` (see [Finishing](#finishing)). Each `--after` is one gate (see [Order](#order)); every step or release it names must already be declared, so the order has no cycles. `--github` and `--tag` declare a release step's repository and tag (see [Releases](#releases)). A relative folder resolves against `-C` when it is given. A declaration is changed only by `cm amend`: a different one is `step-declared`, and the same one again repairs what an interrupted declaration left half written (the step folder, a missing or stale `STATE.md`) without replaying the journal, or else is a no-op. |
| `cm next [<NN-name>/<unit>/<step>] [--lane L]` | Lists the declared `drafted` steps whose every `--after` is met and whose folder has `kickoff.md` (`steps`), the rest with what each waits on (`waiting`), and the merged release steps that wait for their tag (`tags`). It also flags (`stalled`) each `codex`-lane step at `launched` with no PR whose attempt was claimed more than `codex_stall_hours` ago (LIMITS.toml, default 2; the launch stamp for a step launched without an attempt), and (`dirty`) each `pr-open` step whose PR the last `cm sync` saw with `mergeable_state: dirty`; neither is a violation. Nothing is next while `LIMITS.toml` is paused. `tags` is in version order. Given a step, it refuses (exit 1) unless that step is next, naming every unmet gate, and lists a refused step under `waiting`, never `steps`. |
| `cm sync` | Reads GitHub with read-only `gh api` calls, each killed after 60 seconds, without holding `.cm/lock`; then it takes the lock, reloads the state and applies the moves that still hold. It moves steps along: `pr-open` → `merged` when the PR merged (the merge commit is the evidence), a release step `merged` → `released` when its declared tag exists in its repository and contains the prep merge, any other step `merged` → `released` by the first release of its repository whose tag contains its merge, and `released` → `deployed` once a `deployed` release of the same line contains its merge. One journal entry lists the moves; a sync that moves nothing journals nothing. It rewrites `CARD.md`, whose first line is `Last run: <stamp>`, and `.cm/pulls.json` with what it saw of each open PR (head, `mergeable_state`; a PR GitHub did not answer for keeps its last observation and stamp while its step rests at `pr-open` on it), and reports `stalled` and `dirty` as `cm next` does. A `codex`-lane step whose PR merged moves to `merged` only when its merge gate holds at the PR's head; otherwise it stays `pr-open` and each unmet condition is a `merge-gate` violation. A PR closed unmerged (`pr-closed`) or an observation `gh` could not make (`sync-unknown`) exits 1 after the rest is applied. |
| `cm epoch planning\|working\|closed [--skill-revision R] [--plan-revision S]` | Moves `EPOCH.md`'s front matter one status along `OPEN → PLANNING → WORKING → CLOSED`, stamps `planning_started_at`, `working_started_at` or `closed_at` with the date, sets the pins it is given, and journals the move. It refuses a null pin: PLANNING needs `skill_revision`, WORKING and CLOSED also `approved_plan_revision`. `closed` follows `cm close`. |
| `cm journal "<title>" [--body-file F]` | Appends `## YYYY-MM-DDTHH:MMZ — <title>` and the body (`-` reads stdin). The only way `JOURNAL.md` grows. A body may not hold a `#` or `##` heading. |
| `cm launch <NN-name>/<unit>/<step> [--worktree PATH] [--coordinator LABEL] [--cap-min N]` | Runs the launch checks and reports every failure: `LIMITS.toml` not paused; a declared step's every `--after` met (`after-unmet`); for the local `pi` lane and undeclared steps, free disk on `disk_volume` minus `kache_headroom_gib` over `min_free_disk_gib`, live heavy builds (top-level `cargo` processes) under `max_heavy_builds` and the worktree, which they require; the step folder has `kickoff.md`; every path its packet names exists. The `--after` check runs on every launch, a blocked step's relaunch among them. Then it marks the step (and a drafted unit) launched, journals the launch and, for a local step, prints the command; every other lane (`owner` and `coordinator` among them) prints only the step and its prompt and records its attempt (see [Attempts](#attempts)). It spawns nothing. A step whose last attempt has not ended does not launch again (`attempt-open`). |
| `cm attempt <NN-name>/<unit>/<step> [--claim TOKEN] [claim --coordinator L --expect G \| begin KIND \| record KIND ID \| end complete\|partial\|unknown [--evidence PATH]]` | Shows a remote attempt and what to do next from committed state alone, or extends it; each change is journaled. See [Attempts](#attempts). |
| `cm unblock <NN-name>/<unit>[/<step>] --evidence PATH` | Releases a `blocked:<reason>` track to the state it was blocked from, with the evidence that cleared it (a campaign file, as for `done`), journaled like any move. A step that rests at `drafted` or `launched` still waits on every `--after`: `cm next` and `cm launch` check them again, and the move lists what is still unmet. |
| `cm amend <NN-name>/<unit>/<step> [--study] [--lane L] [--after A]…` | Changes a declared step that rests at `drafted` or `launched`: marks it a study, moves its lane, or adds gates (each declared, never closing a cycle: `after-cycle`). Journals the change; a change that changes nothing is `amend-empty`. |
| `cm advance <NN-name>/<unit>[/<step>] <state> [--sha S] [--pr URL] [--tag T] [--digest D] [--evidence PATH] [--repo PATH] [--head SHA] [--ci-verified PATH] [--override PATH]` | Moves one state along `drafted → launched → pr-open → merged → released → deployed → done`, or to `blocked:<reason>`; an `owner`-lane or study step moves to `done` from `drafted` or `launched` with `--evidence` (see [Finishing](#finishing)). `superseded --evidence PATH` retires a step replaced by other work from any state short of final; it meets no gate, so its consumers are amended. A release step moves to `released --tag T` from `drafted` or `launched` when the tag was cut without a prep PR (tag-only). Refuses a skipped state and a state without its evidence, journals the move and re-renders `STATE.md`. A `codex`-lane step moves to `merged` only through its merge gate (see [Merge gate](#merge-gate)). |
| `cm close` | Seals the campaign. Refuses, listing every gap, while `OWNER-QUEUE.md` has an open item or any unit or step is short of `done` or `blocked:<reason>`. Then it stamps `closed` in `.cm/state.json` and journals the close. From there unit `cm init`, `cm launch`, `cm advance` and a second `cm close` refuse with `campaign-closed`; `cm journal` stays open for follow-up observations. |
| `cm feedback "<title>" --urgency urgent\|queued --body-file F` | Appends `### FB-<sequence>-NNN · <urgency> · <title>` and the body to `FEEDBACK.md`, numbering on from the highest id of the letter in `EPOCH.md`'s `sequence:`, and journals a line naming it; an empty body is `feedback-empty`. `--follow-up <id> --disposition proposed\|accepted\|applied\|deferred\|rejected` appends `### <id> follow-up · <disposition>: <title>` to an existing id. Hand-written entries (dated `##` sections, `FB-` sub-entries) still parse. Log feedback whenever a file would otherwise be appended by hand. |
| `cm owner-queue add "<title>" --context T --question T --recommendation T` | Appends `## N. <title>` (numbering on from the highest item), its `Context:` line, the question, a `Recommendation:` line and an empty `Answer:` to `OWNER-QUEUE.md`; refuses an empty field or a RESUME.md with no `>` banner (`resume-banner`), sets RESUME.md's banner to `OWNER-QUEUE not drained: /loop withheld` and journals the item. |
| `cm evidence <NN-name>/<unit>/<step> --name NAME --body-file F` | Writes `<step>/evidence/<NAME>.md` once from a non-empty body and journals the path. `cm advance <step> done` without `--evidence` takes the step's one evidence file when it has exactly one. |
| `cm review <step> --pr URL --head SHA --model ID --effort E --verdict accept\|fix-required --findings C/F --cost USD --wall MIN [--delta FROM_SHA]`, `cm review --import` | Appends one row to the review ledger `.cm/reviews.json` and renders `REVIEW-OFF.md` from it, rows in PR order with a delta review under its PR, and journals the row. The rows a hand-written `REVIEW-OFF.md` already holds are imported once, kept cell for cell, and the text before and after the table (per-PR adjudications, rules) is kept byte for byte (`--import`, or the first `cm review`). The step must be declared (`step-unknown`); an imported delta row is the one whose `#` cell carries `Δ`. After that a hand edit is `review-hand-edit`, as for `STATE.md`. |
| `cm review record <step> <report-file> [--reviewer KIND]` | Parses the `cm-review` block that ends the report (see [Review verdicts](#review-verdicts)), computes the verdict, copies the report to `<step>/evidence/reviews/R<n>[-KIND].md` and records the review in `.cm/verdicts.json` as `R<n>`, its findings as `R<n>.<id>`. Refuses a report with no block or a malformed one (`review-block`, one per problem, with its line), an undeclared step and a report already recorded for the step (`review-duplicate`). A report whose own verdict line disagrees with the computed one is recorded with the computed verdict and the disagreement printed and journaled. The step's third FIX REQUIRED round blocks it (see [Rounds](#rounds)). |
| `cm review resolve <step> <finding> --by <commit\|ruling-file>` | Records what resolved a blocking finding: `R<n>.<id>`, or `<id>` in the step's latest review holding it; an id whose `R<n>` is not a review number names no finding (`finding-unknown`). `--by` is a ruling file in the campaign when that path exists, else a commit SHA. A follow-up is `resolve-not-blocking`; a second resolution is `resolve-done`. |
| `cm ruling <step> "<text>"` | Appends `- YYYY-MM-DD: <text>` to `<step>/evidence/coordinator-scope.md`, creating it with `# Scope and coordinator rulings: <step>` when it is missing, and journals the line. |
| `cm brief <step>` | Prints the step's `kickoff.md`, then `## Coordinator scope and rulings` and the step's `evidence/coordinator-scope.md` (or a line saying none is recorded): the brief for `astra-review.sh start` and review packets. `--json` puts it in `brief`. Refuses a step with no `kickoff.md`. |
| `cm followups [--epoch]`, `cm followups close <id> --by <commit\|reason>` | Lists every non-blocking finding `cm review record` kept, across steps: id, step, PR, type, origin, `file:line`, summary and status (`open` or `closed`). `--epoch` lists only the open ones, grouped by repository as Markdown checklist lines, for the epoch's cleanup PR. `close` closes one with the commit that fixed it or the reason it stays, and journals it; a blocking finding is `followup-blocking`, a closed one `followup-closed`. |
| `cm lint [DIR]` | Template conformance, cm-owned files against `.cm/state.json`, the journal against git, `.cm/JOURNAL.md` and `.cm/lock` out of git (`cm-tracked`), the path check for every drafted or launched packet, a unit or step folder git ignores (`git-ignored`, or `git-ignored-unknown` when `git check-ignore` cannot answer, as outside a repository), and a unit folder holding `STATE.md` but no `.cm/state.json` (`unit-state-json-missing`), which is never counted as a smaller campaign; unique `FEEDBACK.md` ids whose follow-ups name existing ones; `REVIEW-OFF.md` against its ledger. In a closed campaign, also an empty owner queue and every track done or blocked; its packets are no longer checked, since nothing launches there. |

## The folder

```
<campaign>/
  RESUME.md  JOURNAL.md  LIMITS.toml  OWNER-QUEUE.md  CARD.md  [EPOCH.md]
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
| `RESUME.md` | Opens with a `>` banner. No `/loop` proposals until the owner queue is drained: while `OWNER-QUEUE.md` has an open item (a `##` heading), the banner carries the line `OWNER-QUEUE not drained: /loop withheld` and a fenced `/loop` is a violation; once the queue is empty or absent, the banner holds a fenced `/loop …` command and the withheld line is a violation. After `cm close` the banner holds neither: nothing resumes a closed campaign. Headings `# Resume`, `## State today`, `## How to work`, `## Read, in this order`, in that order. |
| `JOURNAL.md` | First heading `# Journal`; after the adopted baseline, every `##` heading is `## YYYY-MM-DDTHH:MMZ — <title>`; the file extends `git show HEAD:JOURNAL.md` with stamped entries only: a changed committed line is `journal-rewritten`, text added to the last committed entry is `journal-hand-append`. Outside git, or before the journal's first commit, only the headings are checked. |
| `LIMITS.toml` | `paused` (bool); `runaway_ceiling_usd`, `min_free_disk_gib`, `kache_headroom_gib`, `pr_ci_expected_min`, `pr_ci_deadline_min` (non-negative numbers); optionally `codex_stall_hours` (a non-negative number, default 2); `max_heavy_builds` (non-negative integer); `driver_model`, `driver_effort`, `verifier_model`, `supervisor_model`, `supervisor_effort`, `disk_volume` (non-empty strings). Other keys are allowed. |
| `OWNER-QUEUE.md` | First heading `# Owner queue`; each `##` item has a line starting `Context:`, one starting `Recommendation:` and one starting `Answer:`. A queue written before cm 0.4 whose items lack `Context:` is reported, never rewritten. |
| `<unit>/STATE.md` | Byte-equal to the render of the unit's `.cm/state.json`. |
| Packets | In `kickoff.md`, `step-*.md` and `resume*.md` of a drafted or launched step, every absolute or `~/` path inside backticks exists. A span with whitespace, a glob or placeholder character (`* ? < > $ { } [ ] \|`), or a single component such as `/loop` or `//!` is not a path; a trailing `:line` or `:start-end` is dropped. Only spans that could name a file are checked: a cm id (`01-a/u/S1`, `00-release/cm/0.5.0`), a URL and a route (an absolute path with no file extension whose first directory is not on this machine, such as `/v1/sessions/abc`) are not. Fenced blocks are not read. |

### State files

Campaign `.cm/state.json`:

```json
{
  "schema": 2,
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
  "schema": 2,
  "kind": "unit",
  "unit": "01-first/alpha",
  "state": "launched",
  "since": "2026-10-08T07:04Z",
  "steps": {
    "BUILD": {
      "state": "pr-open",
      "since": "2026-10-08T08:10Z",
      "evidence": { "sha": "<40 hex>", "pr": "https://github.com/o/r/pull/7" },
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

`lane` and `after` appear on declared steps (`cm init <step> --lane`); a declared step that has not
launched has no `worktree` and `launches: 0`, and `STATE.md` adds an `Order` table. A blocked track also carries `blocked_from`. From `blocked:<reason>` a track resumes to the state it
was blocked from, or moves to that state's successor; a step blocked before its PR relaunches with
`cm launch`, which picks the newest `resume*.md`.

### Evidence

| State | Requires | Takes | Checked |
|---|---|---|---|
| `launched` | `cm launch` only | | the launch checks |
| `pr-open` | `--pr`, `--sha` | | URL is `https://github.com/<owner>/<repo>/pull/<n>`; the SHA resolves to a commit |
| `merged` | `--sha` | | resolves to a commit |
| `released` | `--tag` | `--digest` | the tag resolves locally; a release step's is its declared tag |
| `deployed` | `--digest` | | `sha256:<64 hex>` |
| `done` | `--evidence` for an `owner`-lane or study step; nothing for a code step at `deployed` | | a non-empty regular file inside the campaign, outside `.cm/`; recorded relative to it |
| `superseded` | `--evidence` | | as for `done`; final like `done` |

SHAs and tags resolve in `--repo`, else the step's worktree, else (for a unit) any step's worktree.
A commit or tag made elsewhere needs a `git fetch` first. A release step's `--tag` must be the tag it
was declared with (`release-tag`), and its `pr-open --pr` must be in its repository (`release-repo`).
`cm sync` reads the same evidence from GitHub instead.

### Order

A declared step is handed out by `cm next`, and launched by `cm launch`, only once every gate is
met. `--after` takes:

| Gate | Met when |
|---|---|
| `<step>` or `<NN-name>/<unit>/<step>` | that step is `merged` or later |
| `pr-open:<step>` | that step is `pr-open` or later (a chain that builds on an open PR) |
| `tag:<line>@<version>` | the release step `00-release/<line>/<version>` is `released` or later |
| `deployed:<line>@<version>` | that release step is `deployed` or later |

A step or `pr-open:` gate on an `owner`-lane or study step is met only when that step is `done`.
A blocked step meets the gates of the state it was blocked from. A bare step name is in the declaring step's unit.

### Finishing

A code step finishes at `deployed`, or `merged` where it has no release; `cm advance <step> done`
after `deployed` is unchanged. An `owner`-lane step, or one declared with `--study`, ships no PR:
`cm advance <step> done --evidence <path>` moves it to `done` from any state short of `done`
(blocked steps from where they rest). The path must be a non-empty regular file inside the
campaign folder and outside every `.cm/` folder (`evidence-file`); it is recorded, relative to the
campaign, in the move's evidence beside its stamp, and `cm lint` re-checks it every run. From
`drafted` the step's own `--after` must be met. `done` without `--evidence` on such a step is `evidence-missing`;
`done --evidence` on a code step, or `done` on one short of `deployed`, is `done-code-step`. A
release step cannot be a study.

cm 0.5.0 writes every unit state, and a new campaign state, as schema 3 (it adds `superseded`).
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
| `expired` | the deadline passed: cancel the known turn (`begin cancel` is the one action still allowed), retrieve its partial results and `end partial --evidence <file>` |
| `ended` | `end` recorded `complete`, `partial` (with its evidence) or `unknown`; while an action has begun without an id, only `end unknown` closes the attempt |

`.cm/lock` serializes cm on one machine only, so a replacement coordinator first fences the
generation it replaces: `cm attempt <step> claim --coordinator <label> --expect <G>` succeeds only
while `G` still holds the attempt, makes `G + 1` its holder and prints the new token; `begin`,
`record` and `end` with any other token are `attempt-fenced`. Between machines the committed state
is the fence: claim, commit and push before dispatching or writing to the homelab. Two clones that
both claim `G + 1` hold different tokens, so the one whose push is refused is fenced as soon as it
takes the winner's state.

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
holds each review (`R<n>`: step, reviewer, PR, head, verdict, the report's own verdict line,
the report copy) and each finding with its resolution or closure.

### Rounds

Each reviewed head with a computed FIX REQUIRED is one round, however many reviewers reviewed it.
The `cm review record` that makes a step's third round blocks the step `blocked:third-fix` and adds
an owner-queue item (as `cm owner-queue add` does, withholding RESUME.md's `/loop`) naming the
latest two FIX REQUIRED reports. `cm unblock` releases it once the owner has answered; a fourth
round does not queue again.

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
the file and every gap it waived. The gate's flags on any other move are `evidence-unexpected`.
Steps in other lanes merge as before.

### Releases

A release is a step `00-release/<line>/<major>.<minor>.<patch>`: its packet is the prep PR, and
its name is the only place a tag version comes from. A line is one thing a repository ships under
its own tags; one repository can hold several (`runner` tagged `v<version>` and `guest` tagged
`guest-v<version>` in `xrl/vm-runner`). Each declared version must be above every release already
declared for that line.

`cm init 00-release/<line>/<version> --lane L [--github OWNER/NAME] [--tag PATTERN]` records the
release's repository and its one tag: `PATTERN` holds `{version}` once and defaults to
`v{version}`; without `--github` the prep PR must be in a repository named `<line>`. A release step
declared before cm 0.3.1 has neither recorded; declaring it again with the same lane and `--after`
records its `--tag` and `--github` once. Once the prep
PR is `merged`, `cm next --json` lists it under `tags` with the version, the merge commit and
`tags`, the one declared tag name. `cm sync` records `released` when that tag exists in that
repository and contains the prep merge, and moves the repository's merged steps that the tag
contains along with it. The tick's smoke receipt is
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
