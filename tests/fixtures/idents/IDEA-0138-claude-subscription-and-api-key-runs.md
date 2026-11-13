---
id: IDEA-0138
title: "`claude -p`-style Claude runs on Dekopon, on an Anthropic subscription or an API key"
maturity: designed
current_epoch: eleemosynary-mnemosyne
sources:
  - "Owner, in Claude Code on this Mac, 2026-11-09 ~06:05Z (verbatim below)"
  - "store/ideas/IDEA-0130-native-claude-api-client.md: the API-key client this builds on"
depends_on: [IDEA-0130, IDEA-0151]
---

# Claude subscription and API-key runs


## Owner rulings on the rubric review — 2026-11-11


| Question | Ruling |
| Subscription credential | **An ephemeral placeholder in the VM, the real token in the broker** (Q2 option B, through [IDEA-0151](IDEA-0151-claude-subscription-gateway-credentials.md)'s gateway). The VM gets a per-session placeholder `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_BASE_URL` at the gateway, and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` ([FINDINGS.md](../designs/permission-proxy-design/FINDINGS.md)). The gateway swaps in the broker's one-year OAuth token and forwards to `api.anthropic.com`, so subscription calls are observed (spans, cache hit rate). The owner reports that Anthropic confirmed relayed requests are acceptable. That confirmation is why this proceeds against the published consumer terms' "intermediate Claude.ai credentials" clause (§Rubric review, R14); the owner's own agents stay the only users. |
