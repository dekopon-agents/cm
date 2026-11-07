# Review prompts

Loss: coordinator commentary must not reach the reviewer.
Outcome: N2 (changes what a reviewer sees on purpose): `cm review prompt <step> --head SHA [--base SHA] [--round 2]` prints a blind prompt from a template holding only the task (the step's kickoff Outcome), base and head; `--round 2` for an Astra review adds only that reviewer's own R1 report and nothing the coordinator wrote. `cm brief` is unchanged and stays for worker packets; the prompt never calls it. N3: `cm review repair-brief <step>` prints a fixed header plus the round's reports verbatim; the header never claims a round is the last (B1 decides). Both are print-only (cm spawns nothing); template text lives in `src/templates.rs` or a new `src/prompts.rs` with a `templates/` file, held to no coordinator-supplied fields.
Files: `src/verdict.rs`, `src/cli.rs`, `src/templates.rs`.
Tests: prompt isolation and verbatim reports.
README: Review verdicts.
