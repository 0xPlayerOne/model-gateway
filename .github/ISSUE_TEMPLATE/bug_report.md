---
name: Bug Report
about: Report a bug or unexpected behavior — used by both humans and agents
title: '[bug] '
labels: 'bug'
assignees: ''
---

## Environment

- **Commit:** <!-- SHA of the commit where the bug was observed -->
- **Branch:** <!-- main / staging / feature branch name -->
- **CI Run:** <!-- link if applicable -->
- **App/Workspace:** <!-- web / app / smashers / docs / ui / theme / playfab / imx-passport / eslint-config / prettier-config / typescript-config -->
- **Detected by:** <!-- human / agent-name (e.g. intern / satoshi / ye) -->

## Description

<!-- Clear, concise description of the bug. Include what you expected vs what actually happened. -->

## Steps to Reproduce

1.
2.
3.

## Error Output

```
<!-- Paste full error logs, stack traces, or CI failure output here. -->
```

## What the Agent Tried

<!-- If filed by an agent: what fix strategies were attempted before escalating? What failed? -->

- [ ] Package version rollback
- [ ] Dependency resolution alternative
- [ ] Configuration change
- [ ] Workflow step modification
- [ ] Other (describe below)

## Diagnostics

<!-- Any relevant diagnostic info: bun.lock hash diff, dependency tree changes, env diff, etc. -->

## Blocker

- [ ] Blocks a deployment
- [ ] Blocks a PR
- [ ] Low priority / cosmetic

## Related

<!-- Link to related issues, PRs, commits, or discussions. -->
