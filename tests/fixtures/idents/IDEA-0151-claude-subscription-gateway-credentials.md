---
id: IDEA-0151
title: Keep Claude subscription OAuth credentials outside agent VMs
maturity: captured
current_epoch: null
sources:
  - "Owner voice conversation with dot, 2026-11-10 America/New_York / 2026-11-11 UTC, thread 01a1b3a3-a9ea-720a-be91-ff30207060fa; verbatim excerpts below"
  - "https://github.com/dekopon-agents/campaign-trail/blob/72d106737a518ffc9493f71e10a38da10e1faae8/store/ideas/IDEA-0138-claude-subscription-and-api-key-runs.md"
depends_on: []
---

# Keep Claude subscription OAuth credentials outside agent VMs

## Acceptance and dependencies

Research should return a source-backed supported path, a clearly bounded unsupported experiment, or a candid incompatibility result. A future implementation would need to demonstrate that the VM never receives reusable upstream access/refresh credentials, including through login, renewal, diagnostics, logs and error responses, while intended Claude Code runs still work.

Related: [IDEA-0138](IDEA-0138-claude-subscription-and-api-key-runs.md) (Claude Code in a VM), [IDEA-0130](IDEA-0130-native-claude-api-client.md) (Messages API), and [IDEA-0036](IDEA-0036-skylight-refreshing-credential.md) (broker credential renewal). No hard dependency or epoch admission is established by this capture. Existing IDEA-0138 design history remains intact; this is the fresh follow-up the owner requested.

## Epoch history

- 2026-11-11 UTC: captured, unassigned, from the owner's November 10 Eastern voice discussion. Idea filing only; no implementation, live credential tests, credential changes or deployment.
- 2026-11-11: owner widened this to "claude/codex permission proxying" (the IDEA-0125 rubric-review session) and ordered research first. A bounded research pass on placeholder tokens and a substituting endpoint, with local mocks and no real credentials, started the same day.
