# Contributing to Model Gateway

Welcome! This document is the **single source of truth** for the contribution workflow across all Model Gateway repositories.  
All agents (Hermes and other automation) must read and follow this document before working on any repo in the fleet.

- [Code of Conduct](./CODE_OF_CONDUCT.md)
- [Security Policy](./SECURITY.md)

---

## Table of Contents

1. [Repository Overview](#1-repository-overview)
2. [Branching Model](#2-branching-model)
3. [Development Setup](#3-development-setup)
4. [Contribution Workflow — Internal Contributors](#4-contribution-workflow--internal-contributors)
5. [Contribution Workflow — External Contributors](#5-contribution-workflow--external-contributors)
6. [CI & Testing Discipline](#6-ci--testing-discipline)
7. [Pull Request Guidelines](#7-pull-request-guidelines)
8. [Code Review Standards](#8-code-review-standards)
9. [Merge Protocol](#9-merge-protocol)
10. [Workflow Discipline](#10-workflow-discipline)
11. [Emergency Procedures](#11-emergency-procedures)

---

## 1. Repository Overview

This is a **pure Rust** HTTP server using Axum, Tokio, and rusqlite. No JavaScript, no Python — just a single Rust crate.

### Source layout

| Module | Role |
|---|---|
| `src/gateway.rs` | Axum routes, request handling, SSE streaming, fallback logic |
| `src/config.rs` | TOML config parsing, validation, provider/model structures |
| `src/routing.rs` | SQLite-backed routing store: catalogs, benchmarks, quotas, cooldowns |
| `src/providers.rs` | Provider profiles, catalog fetching, request preparation |
| `src/benchmarks.rs` | Benchmark import/parse, task classification, quality/cost selection |
| `src/secrets.rs` | Secret resolution: keyring, file, or environment |
| `src/storage.rs` | Private module: atomic file writes, SQLite helpers |
| `src/main.rs` | CLI (clap): setup, serve, config, credentials, catalog, benchmarks, healthcheck |

### Commands

| Command | What it does |
|---|---|
| `cargo build` | Build all targets (debug) |
| `cargo build --release` | Build release binary |
| `cargo test --all-features` | Run all unit + integration tests |
| `cargo fmt --check` | Check formatting |
| `cargo clippy --all-targets --all-features -- -D warnings` | Lint (deny warnings) |
| `cargo +stable deny check` | Dependency policy audit (requires cargo-deny) |
| `cargo doc --no-deps` | Generate documentation |

---

## 2. Branching Model

```
main  ───────────────────────────────────── (protected, releases only)
  ↑  staging→main PR (squash merge)
staging ───────────────────────────────── (integration, CI must pass)
  ↑  sub-branch → staging PR (squash merge)
feat/foo  fix/bar  chore/baz  ...          (feature branches)
```

### `main` — Protected Production Branch

- **No direct pushes.** Not by you, not by any agent, not by admin (0xPlayerOne). All pushes blocked by branch protection (`enforce_admins: true`).
- **Only accepts merges from `staging`.** No other branch may merge into `main`.
- **All CI must pass on `staging`** before a staging→main PR can merge.
- **Linear history.** No merge commits — squash merge only. Every commit on `main` is a squashed summary of a staging batch.
- **Force pushes are disabled** on `main`.

### `staging` — Integration Branch

- **Default workflow: branch → PR → merge.** Create a new branch for your work, open a PR into staging, and let CI verify it. This is the preferred path.
- **PRs are created as drafts** automatically by CI when a feature branch is pushed. Once CI passes, the PR is marked ready for review/merge.
- **Can push directly** for small fixes or urgent bugs (YOLO) when a branch+PR would be overkill.
- **CI must pass** before merging into staging (enforced by required status checks on the PR).
- **Force pushes allowed** — needed for rebasing sub-branches.
- **No review required** — you can self-merge sub-branches into staging.
- Merge failures into staging are acceptable. When a sub-branch CI fails, you may:
  - Fix the issue on the sub-branch and re-merge, OR
  - Merge anyway and fix directly on staging (this is intentionally flexible).
  - The only hard rule: **before staging→main, CI must pass.**
- **Rebase staging after main release.** After `staging` is merged into `main`, rebase `staging` on `main` to keep history aligned and prevent merge conflicts in future PRs.

### Feature Branches — `feat/*`, `fix/*`, `chore/*`, `refactor/*`

- Branch from `staging`, not `main`.
- Use conventional commit prefixes: `feat/`, `fix/`, `chore/`, `refactor/`, `docs/`, `test/`.
- All pushes to the branch trigger **full CI**.
- Open a PR into `staging` when ready.
- If the PR CI fails, either fix on the branch or merge the failing branch and fix on staging.

---

## 3. Development Setup

### Prerequisites

- Rust toolchain (pinned to **1.87.0** via `rust-toolchain.toml` — installed automatically by `rustup`)
- `cargo-deny` for dependency auditing (install via `cargo +stable install cargo-deny --version 0.20.2 --locked`)

### One-time setup

```bash
# Clone
git clone git@github.com:0xPlayerOne/model-gateway.git
cd model-gateway

# Rust toolchain is auto-installed by rustup when you enter the repo
# (reads rust-toolchain.toml)

# Build (debug)
cargo build

# Run tests to verify everything is working
cargo test --all-features
```

### Before committing

```bash
cargo fmt --check          # Formatting
cargo clippy --all-targets --all-features -- -D warnings  # Lint
cargo test --all-features  # Run all tests
cargo build --release      # Verify release builds cleanly
cargo +stable deny check   # Dependency policy audit
```

> **Note:** `cargo fmt` is run on all Rust source. Never bypass formatting or clippy warnings — fix the issue instead.

---

## 4. Contribution Workflow — Internal Contributors

For Model Gateway team members and automation agents.

### 4.1 Start from staging

```bash
git checkout staging
git pull origin staging
git checkout -b feat/my-feature
```

### 4.2 Make changes, commit, push

Use [Conventional Commits](https://www.conventionalcommits.org):

```
feat: add user profile page
fix: resolve wallet disconnect race condition
chore(deps): upgrade axios to v1.7
refactor: extract auth middleware
```

```bash
git add .
git commit -m "feat: add user profile page"
git push origin feat/my-feature
```

### 4.3 Open a Pull Request

- **Target:** `staging` (always, never `main` directly).
- **Title:** Match the branch name / commit summary format.
- **Fill out the PR template** — include issue references, screenshots, and checklist.
- If CI is still running, wait for it. If it fails, fix and push again — CI runs automatically on each push.

### 4.4 Merge into staging

- Once CI passes, squash-merge into `staging` (no review required for sub-branch→staging).
- Delete the feature branch after merge.

### 4.5 Staging → Main (release)

- **Only admins can merge into `main`.** The daily review agent may pick up PRs once they are approved by an admin on GitHub.
- All CI must be green on staging before this PR opens.
- Squash merge with a release summary message.
- The `main` branch is then deployed to production by CI.
- **After merge, rebase `staging` on `main`** to keep branch histories aligned and prevent future merge conflicts.

---

## 5. Contribution Workflow — External Contributors

For open-source contributors outside Model Gateway.

### 5.1 Fork + Clone

1. Fork the repository via GitHub UI.
2. Clone your fork:

```bash
git clone git@github.com:YOUR_USERNAME/model-gateway.git
cd model-gateway
```

### 5.2 Branch

```bash
git remote add upstream git@github.com:0xPlayerOne/model-gateway.git
git fetch upstream
git checkout -b feat/your-feature upstream/staging
```

### 5.3 Commit & Push

- Use [Conventional Commits](https://www.conventionalcommits.org).
- Keep commits focused and atomic.

```bash
git push origin feat/your-feature
```

### 5.4 Open a Pull Request

- **Target:** `staging` (not main).
- **Title:** Conventional commit style.
- **Description:** What does this change? Why? Any screenshots?
- **Link to related issue** if applicable.
- CI runs automatically. Waiting for it to pass is appreciated.

### 5.5 After merge

- Your commits will be squash-merged into `staging`.
- You can delete your feature branch after merge.

---

## 6. CI & Testing Discipline

### What runs when

| Event                                              | CI trigger                                | Reason                                                          |
| -------------------------------------------------- | ----------------------------------------- | --------------------------------------------------------------- |
| Push to `main` or `staging`                        | `push` trigger → **full CI runs**         | Covers direct pushes to staging, and staging→main merge commits |
| PR opened/synchronized targeting `staging`         | `pull_request` trigger → **full CI runs** | Covers sub-branch→staging PRs                                   |
| Push to a feature branch (`feat/*`, `fix/*`, etc.) | **No CI**                                 | CI only runs when a PR is opened into staging                   |
| Direct push to `main`                              | **Blocked by branch protection**          | Only possible via staging→main merge                            |

### Why no duplicates

CI runs on `push` events (to main/staging) and `pull_request` events (targeting main/staging). These fire on **different refs**:

- `push` trigger fires on commits pushed directly to `main` or `staging` branches
- `pull_request` trigger fires on the **head commit** of a PR (which lives on a feature branch, not main/staging)

Since a commit can never be simultaneously pushed to `main`/`staging` AND be a PR's head commit from a different branch, **no commit ever triggers CI twice**.

| Scenario                            | Push trigger?                | PR trigger?      | Double? |
| ----------------------------------- | ---------------------------- | ---------------- | ------- |
| Push to sub-branch `feat/foo`       | No (wrong branch)            | No (no PR event) | ✅ No   |
| Open PR `feat/foo` → `staging`      | No (commit is on sub-branch) | Yes              | ✅ No   |
| Push to `staging` directly          | Yes                          | No               | ✅ No   |
| Merge staging→main (push to `main`) | Yes                          | No               | ✅ No   |

### CI jobs

| Job | What it checks |
|---|---|
| `fmt` | `cargo fmt --check` — formatting compliance |
| `clippy` | `cargo clippy --all-targets --all-features -- -D warnings` — lint |
| `test` | `cargo test --all-features` — all unit + integration tests |
| `build` | `cargo build --release` — release compilation |
| `deny` | `cargo +stable deny check` — dependency/advisory audit |
| `native-smoke` | `./scripts/native-smoke.sh` — native binary smoke test |
| `docker-smoke` | `./scripts/docker-smoke.sh` — Docker image smoke test |
| `hermes-smoke` | `./scripts/hermes-smoke.sh` — gateway integration smoke test |
| `release-smoke` | `./scripts/release-smoke.sh <dir>` — release archive validation |

### If CI fails

- **On your feature branch:** Push a fix, CI re-runs automatically.
- **On staging after merge:** Fix directly on staging (push a fix commit) or revert the failing change.
- **Before staging→main:** CI must be **all green** on the staging branch's latest commit.

---

## 7. Pull Request Guidelines

Every PR must use the [pull request template](./PULL_REQUEST_TEMPLATE.md) — do not delete sections.

The template covers:

- **Description** — what changed and why
- **CI Status** — checkbox for each required check
- **Compliance Checklist** — locking, format, dep hygiene, no generated artifacts
- **Additional Context** — breaking changes, migration steps, related PRs

Since feature branches are already prefixed (`feat/`, `fix/`, `chore/`, etc.) and all PRs target `staging`, the template intentionally omits type-picker and target-branch fields — they are inferred from the branch and CI context.

### Staging→main (release) PRs

Release PRs follow the same template but add a release summary describing the batch.

- Prefer small, focused PRs (under 400 lines changed when possible).
- Large features should be broken into multiple PRs targeting staging.
- If a PR exceeds 1000 lines, consider splitting it.

---

## 8. Code Review Standards

### Internal PRs (sub-branch → staging)

- **No review required.** Self-merge is fine.
- Peer reviews are encouraged but not mandatory.
- If you want feedback, request a review explicitly.

### Staging → Main PRs

- **Only admins can merge into `main`.** The daily review agent picks up PRs once they are approved by an admin on GitHub (via review approval).
- Focused on: does CI pass? Are there breaking changes? Is the release summary complete?
- This is a release gate, not a code-level review (code review happened on sub-branch→staging).

### External PRs (fork → staging)

- **Review is required** from at least one maintainer.
- Focus on: correctness, security, style alignment, test coverage.
- External contributors should expect feedback and iteration.

---

## 9. Merge Protocol

| From                     | To        | Method       | Reviewer                          | Notes                                    |
| ------------------------ | --------- | ------------ | --------------------------------- | ---------------------------------------- |
| Sub-branch               | `staging` | Squash merge | Optional (self-merge OK)          | Delete branch after merge; auto-draft PR |
| Direct push to `staging` | `staging` | Push         | N/A                               | For small fixes or urgent bugs           |
| `staging`                | `main`    | Squash merge | Admin (0xPlayerOne) | Only when all CI passes on staging       |

### Squash merge convention

All merges use **squash merge** — every PR becomes a single commit on the target branch. This keeps history clean and linear.

When squashing, the commit message should be:

```
<type>(<scope>): <summary>

<optional body with details>
```

---

## 10. Workflow Discipline

Branch protections are a safety net, not a workflow definition. The workflow defined in sections 2–9 is authoritative regardless of whether GitHub's API enforces every rule. Always follow the documented process — do not bypass quality gates even when technically possible.

---

## 11. Emergency Procedures

### Urgent hotfix (security / production outage)

1. Create a branch off `staging`: `git checkout staging && git checkout -b hotfix/urgent-fix`
2. Fix the issue, push, open a PR into `staging`.
3. Self-merge once CI passes.
4. Open a staging→main PR and flag as urgent.
5. If staging→main merge is blocked by CI issues unrelated to your change, contact 0xPlayerOne.

### Rollback

If a staging→main merge introduces a production issue:

1. Revert the merge commit on staging: `git revert -m 1 <merge-sha>`
2. Push directly to staging: `git push origin staging`
3. Open a new staging→main PR.
4. Fix the root cause on a sub-branch and re-merge.

### Skip-CI (rare emergencies only)

In genuine emergencies where CI is blocked by infrastructure (not code), you may push with `[skip-ci]` in the commit message.  
This must be followed by a CI-fixing follow-up commit within 24 hours. Abuse of skip-ci will result in access revocation.

---

_Last updated: 2026-07-25_  
_Maintainers: Model Gateway engineering team_
