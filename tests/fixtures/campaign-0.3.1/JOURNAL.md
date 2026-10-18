# Journal

Append-only, written only by cm. Newest at the bottom. Stamps are UTC from cm's clock.

## 2026-10-18T05:56Z — cm init

- written: RESUME.md, JOURNAL.md, LIMITS.toml, OWNER-QUEUE.md

## 2026-10-18T05:56Z — init unit 01-a/u

- written: 01-a/u/.cm/state.json, 01-a/u/STATE.md

## 2026-10-18T05:56Z — init step 01-a/u/PLAN

- lane: owner
- after: —

## 2026-10-18T05:56Z — init step 01-a/u/BUILD

- lane: pi
- after: 01-a/u/PLAN

## 2026-10-18T05:56Z — init step 01-a/u/SPIKE

- lane: claude-cloud
- after: pr-open:01-a/u/BUILD

## 2026-10-18T05:56Z — init step 00-release/cm/0.3.1

- lane: codex
- after: —
- tag: cm-0.3.1
- github: o/cm

## 2026-10-18T05:56Z — init step 01-a/u/SHIP

- lane: fable
- after: tag:cm@0.3.1

## 2026-10-18T05:56Z — launch 01-a/u/PLAN

- packet: 1 file(s), 0 named path(s) checked
- lane: owner
- launch 1: `01-a/u/PLAN/kickoff.md`

## 2026-10-18T05:56Z — advance 01-a/u/PLAN: launched → blocked:waiting-owner

- evidence: —
