# 01-a/u state

Rendered by cm from `.cm/state.json`. Change it with `cm launch` and `cm advance`; `cm lint` fails on a hand edit.

| Unit | State | Since | Evidence |
|---|---|---|---|
| 01-a/u | launched | 2026-10-18T05:56Z | — |

## Alive

| Step | State | Since | Launches | Worktree | Evidence |
|---|---|---|---|---|---|
| BUILD | drafted | 2026-10-18T05:56Z | 0 | — | — |
| PLAN | blocked:waiting-owner | 2026-10-18T05:56Z | 1 | — | — |
| SHIP | drafted | 2026-10-18T05:56Z | 0 | — | — |
| SPIKE | drafted | 2026-10-18T05:56Z | 0 | — | — |

## Order

| Step | Lane | After |
|---|---|---|
| BUILD | pi | 01-a/u/PLAN |
| PLAN | owner | — |
| SHIP | fable | tag:cm@0.3.1 |
| SPIKE | claude-cloud | pr-open:01-a/u/BUILD |

## History

| At | Target | From | To | Evidence |
|---|---|---|---|---|
| 2026-10-18T05:56Z | 01-a/u/PLAN | drafted | launched | — |
| 2026-10-18T05:56Z | 01-a/u | drafted | launched | — |
| 2026-10-18T05:56Z | 01-a/u/PLAN | launched | blocked:waiting-owner | — |
