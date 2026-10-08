# cm

Campaign manager: the campaign lifecycle as one tool. A campaign is a folder of state files that a
coordinator, supervisors and drivers read and write while a multi-step code effort runs. `cm` owns
the mechanical parts of that folder: the journal, the launch checks, the state moves and the lint.
Clock stamps come from `cm`, never from an argument.

```sh
brew install dekopon-agents/tap/cm
```

## Commands

Every command takes `--json` (one JSON object on stdout with `ok` and `violations`) and `-C DIR`
(the campaign folder; found by walking up from the current directory otherwise). Exit 0 is
success, 1 is a refusal or violations (every one listed, never only the first), 2 is a usage
error.

| Command | Contract |
|---|---|
| `cm init [DIR]` | Writes `RESUME.md`, `JOURNAL.md`, `LIMITS.toml`, `OWNER-QUEUE.md` and `.cm/state.json` from templates embedded in the binary. Never overwrites: in an existing folder it writes only what is missing and adopts each existing file as the baseline once it lints clean. `cm init <NN-name>/<unit>` inside a campaign writes the unit's `.cm/state.json` and its rendered `STATE.md`. |
| `cm journal "<title>" [--body-file F]` | Appends `## YYYY-MM-DDTHH:MMZ — <title>` and the body (`-` reads stdin). The only way `JOURNAL.md` grows. A body may not hold a `#` or `##` heading. |
| `cm launch <NN-name>/<unit>/<step> --worktree PATH` | Runs the launch checks and reports every failure: `LIMITS.toml` not paused; free disk on `disk_volume` minus `kache_headroom_gib` over `min_free_disk_gib`; live heavy builds (top-level `cargo` processes) under `max_heavy_builds`; the worktree exists; the step folder has `kickoff.md`; every path its packet names exists. Then it marks the step (and a drafted unit) launched, journals the launch and prints the command. It spawns nothing. |
| `cm advance <NN-name>/<unit>[/<step>] <state> [--sha S] [--pr URL] [--tag T] [--digest D] [--repo PATH]` | Moves one state along `drafted → launched → pr-open → merged → released → deployed → done`, or to `blocked:<reason>`. Refuses a skipped state and a state without its evidence, journals the move and re-renders `STATE.md`. |
| `cm lint [DIR]` | Template conformance, cm-owned files against `.cm/state.json`, and the path check for every drafted or launched packet. |

## The folder

```
<campaign>/
  RESUME.md  JOURNAL.md  LIMITS.toml  OWNER-QUEUE.md
  .cm/state.json  .cm/JOURNAL.md
  <NN-name>/<unit>/
    STATE.md  .cm/state.json
    <step>/kickoff.md  step-*.md  resume*.md
```

`<NN-name>` is two digits, `-`, a name (`01-first`). Units and steps are names of letters, digits,
`.`, `_` and `-`.

Packet paths are checked on this machine. A path that lives elsewhere (a host path on a server, such
as `/var/lib/<service>`) goes in a fenced block or without backticks; backticked, it fails
`packet-path`.

To undo a hand edit: `cp .cm/JOURNAL.md JOURNAL.md` resets the journal (re-add the lines with
`cm journal`), and `rm <NN-name>/<unit>/STATE.md && cm init <NN-name>/<unit>` re-renders a unit's
`STATE.md`.

### Template contracts

| File | Lint requires |
|---|---|
| `RESUME.md` | Opens with a `>` banner holding a fenced `/loop …` command; headings `# Resume`, `## State today`, `## How to work`, `## Read, in this order`, in that order. |
| `JOURNAL.md` | First heading `# Journal`; after the adopted baseline, every `##` heading is `## YYYY-MM-DDTHH:MMZ — <title>`; the file equals cm's copy (`.cm/JOURNAL.md`): a changed line is `journal-rewritten`, an added line is `journal-hand-append`. |
| `LIMITS.toml` | `paused` (bool); `runaway_ceiling_usd`, `min_free_disk_gib`, `kache_headroom_gib`, `pr_ci_expected_min`, `pr_ci_deadline_min` (non-negative numbers); `max_heavy_builds` (non-negative integer); `driver_model`, `driver_effort`, `verifier_model`, `supervisor_model`, `supervisor_effort`, `disk_volume` (non-empty strings). Other keys are allowed. |
| `OWNER-QUEUE.md` | First heading `# Owner queue`; each `##` item has a line starting `Recommendation:` and one starting `Answer:`. |
| `<unit>/STATE.md` | Byte-equal to the render of the unit's `.cm/state.json`. |
| Packets | In `kickoff.md`, `step-*.md` and `resume*.md` of a drafted or launched step, every absolute or `~/` path inside backticks exists. A span with whitespace, a glob or placeholder character (`* ? < > $ { } [ ] \|`), or a single component such as `/loop` is not a path; a trailing `:line` or `:start-end` is dropped. Fenced blocks are not read. |

### State files

Campaign `.cm/state.json`:

```json
{
  "schema": 1,
  "kind": "campaign",
  "initialized": "2026-10-08T07:04Z",
  "written": ["RESUME.md", "JOURNAL.md", "LIMITS.toml", "OWNER-QUEUE.md"],
  "adopted": [],
  "journal": { "baseline_lines": 3 }
}
```

`written` are files cm wrote, `adopted` are files that existed and linted clean at `cm init`.
`journal.baseline_lines` is where cm's stamped region starts; an adopted journal keeps its old
headings.

Unit `.cm/state.json`:

```json
{
  "schema": 1,
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
      "launches": 1
    }
  },
  "history": [
    { "at": "2026-10-08T07:04Z", "target": "01-first/alpha/BUILD", "from": "drafted", "to": "launched" }
  ]
}
```

A blocked track also carries `blocked_from`. From `blocked:<reason>` a track resumes to the state it
was blocked from, or moves to that state's successor; a step blocked before its PR relaunches with
`cm launch`, which picks the newest `resume*.md`.

### Evidence

| State | Requires | Takes | Checked |
|---|---|---|---|
| `launched` | `cm launch` only | | the launch checks |
| `pr-open` | `--pr`, `--sha` | | URL is `https://github.com/<owner>/<repo>/pull/<n>`; the SHA resolves to a commit |
| `merged` | `--sha` | | resolves to a commit |
| `released` | `--tag` | `--digest` | the tag resolves locally |
| `deployed` | `--digest` | | `sha256:<64 hex>` |
| `done` | | | |

SHAs and tags resolve in `--repo`, else the step's worktree, else (for a unit) any step's worktree.
A commit or tag made elsewhere needs a `git fetch` first. Reading live artifacts is phase 2.

## Releases

Every push to `main` releases. The version is `Cargo.toml`'s `X.Y.0` when no `vX.Y.*` tag exists,
else the next patch above the newest `vX.Y.*`. It is written into the build's manifest only and
never committed, so `Cargo.toml` always reads `X.Y.0`; bump the minor there to start a new line.
The run builds `aarch64-apple-darwin`, `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl`, attests the archives, creates the release and pushes
`Formula/cm.rb` to `dekopon-agents/homebrew-tap`.

Releases run one at a time. Merges that land while a release runs coalesce: GitHub keeps only the
newest waiting run, so they ship together under the next tag, not one tag each.

## License

Licensed under either of Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT
license ([LICENSE-MIT](LICENSE-MIT)) at your option.
