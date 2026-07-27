# Contributing

This guide is the operating contract for humans and automation contributing to this repository.

It applies to TypeScript, Rust, Python, and mixed-language projects using this template.

Quick links: [Code of Conduct](./CODE_OF_CONDUCT.md) · [Security Policy](./SECURITY.md) · [Pull Request Template](./PULL_REQUEST_TEMPLATE.md)

### Contents

[Agent contract](#agent-operating-contract) · [Branches](#branching-model) · [Setup](#before-you-start) · [Validation](#local-validation) · [Internal](#internal-contribution-workflow) · [External](#external-contribution-workflow) · [Pull requests](#pull-request-standards) · [Reviews](#review-and-merge-protocol) · [Security](#security-and-emergencies)

## Agent operating contract

Agents must follow these rules before changing code:

1. Read this file, `AGENTS.md`, and the relevant project documentation.
2. Inspect the current branch, worktree, remotes, and existing changes before editing.
3. Preserve user-owned changes. Never discard or overwrite unrelated work.
4. Branch from `staging` and target pull requests at `staging`; do not work directly on `main`.
5. Keep the change focused. Do not expand scope without documenting why.
6. Run the applicable format, lint, type-check, build, unit, integration, E2E, smoke, and security checks.
7. Report exact validation results, skipped checks, known limitations, and remaining risks.
8. Never commit secrets, credentials, local environment files, generated artifacts, or machine-specific paths.

Agents must not:

- Use destructive Git operations, force pushes, or history rewriting without explicit authorization.
- Bypass hooks or required checks to hide a failure.
- Change branch protections, secrets, deployments, or external systems unless that action is explicitly in scope.
- Claim completion when tests, deployment checks, or required reviews are still pending.

## Branching model

```text
                                      release PR
                                   ┌──────────────┐
                                   │              ▼
feat/*  fix/*  chore/*  ──PR──▶  staging  ──PR──▶  main
docs/*  test/*  refactor/*         │              │
                                   │              └── protected release branch
                                   └── integration branch
```

| Branch                                                         | Purpose                  | Contribution rule                                                         |
| -------------------------------------------------------------- | ------------------------ | ------------------------------------------------------------------------- |
| `main`                                                         | Protected release branch | Merge through the `staging` → `main` release PR. No direct pushes.        |
| `staging`                                                      | Integration branch       | Target normal pull requests here. Required checks must pass before merge. |
| `feat/*`, `fix/*`, `chore/*`, `refactor/*`, `docs/*`, `test/*` | Focused work             | Branch from `staging`; keep changes small and reviewable.                 |

Use squash merges unless the repository documents another strategy. Re-align `staging` with `main` after a release when needed.

## Before you start

### Toolchain

1. Install [mise](https://mise.jdx.dev/).
2. Run `bash .github/scripts/bootstrap.sh` to install the pinned toolchain, enable hooks, and validate the checkout.
3. Use `bash .github/scripts/doctor.sh` when setup, lockfiles, or hooks appear out of sync.
4. Use the repository's existing package manager and lockfile. Do not introduce a second package manager.
5. Copy `.env.example` to the appropriate local environment file when provided. Never commit the copy.

### Worktree and branch

```sh
git status --short --branch
git fetch origin
git switch staging
git pull --ff-only origin staging
git switch -c feat/short-description
```

If the worktree is dirty, stop and understand the existing changes before switching branches or editing overlapping files.

## Local validation

The template scripts detect supported tools and skip checks that do not apply:

```sh
bash .github/scripts/ci.sh format
bash .github/scripts/ci.sh lint
bash .github/scripts/ci.sh type_check
bash .github/scripts/ci.sh build
bash .github/scripts/ci.sh unit
bash .github/scripts/ci.sh integration
bash .github/scripts/ci.sh e2e
bash .github/scripts/ci.sh smoke
bash .github/scripts/security.sh
```

Run the checks relevant to the change. For a release or security-sensitive change, run the complete set. Record the commands and results in the pull request.

## Internal contribution workflow

For maintainers, trusted contributors, and automation agents:

1. Start from an up-to-date `staging` branch.
2. Create a focused branch with a descriptive prefix.
3. Inspect the relevant code and tests before making changes.
4. Implement the smallest complete change.
5. Add or update tests, documentation, configuration, and migration notes as needed.
6. Run local validation and inspect the final diff.
7. Commit with a clear message, preferably using Conventional Commits:

   ```text
   feat(auth): add passkey recovery
   fix(api): handle expired session tokens
   chore(ci): cache Rust dependencies
   ```

8. Push the branch and open a pull request into `staging`.
9. Address review feedback and failed checks on the same branch.
10. Squash-merge only after required checks pass and the change is ready.

### Internal agent handoff

Every agent handoff should state:

```text
Summary: what changed and why
Files: important files changed
Validation: exact commands and pass/fail results
Skipped: checks skipped and why
Risks: known limitations or follow-up work
Branch/PR: branch name and pull request link
```

## External contribution workflow

For contributors who do not have direct write access:

1. Fork the repository on GitHub.
2. Add the upstream repository as `upstream`.
3. Branch from the upstream `staging` branch.
4. Make a focused change and follow the local setup instructions.
5. Add tests and documentation for behavior changes.
6. Run all applicable checks locally.
7. Push to the fork and open a pull request targeting `staging`.
8. Explain the problem, proposed solution, validation, compatibility, and rollout impact.
9. Address maintainer feedback without rewriting unrelated history or scope.

External contributors should never need repository secrets or production access to validate a normal change.

## Pull request standards

Every pull request should make these questions easy to answer:

- What changed?
- Why was it needed?
- How was it tested?
- What could break?
- Does it require migration, deployment, configuration, or rollback work?
- Which files or areas deserve focused review?

Keep pull requests focused and reviewable. Include screenshots or recordings for user-facing changes. Link related issues and use `Closes #123` when appropriate. Complete the [pull request template](./PULL_REQUEST_TEMPLATE.md).

## Workflow and check behavior

| Event                            | Expected automation                      |
| -------------------------------- | ---------------------------------------- |
| Push to `main` or `staging`      | CI, Test, Security, and CodeQL workflows |
| Pull request targeting `staging` | CI, Test, Security, and CodeQL workflows |
| Push to a working branch         | Draft PR workflow                        |
| Push to `staging`                | Release PR workflow                      |
| Version tag such as `v1.2.3`     | Release workflow                         |

The workflows use separate concurrency groups. A newer run for the same branch or pull request cancels its older run, while independent CI, test, security, and CodeQL workflows continue in parallel.

Required checks are enforced by branch protection. Do not duplicate their checklists in the pull request description; document validation commands and results instead.

### Release conventions

Use Conventional Commits so the release automation can determine the next version: `fix:` produces a patch release, `feat:` produces a minor release, and `!` or `BREAKING CHANGE:` produces a major release. Add `Release-As: x.y.z` only when a deliberate version override is needed. The release workflow maintains the changelog and GitHub release after changes land on `main`; npm publication is opt-in through `.github/template.yml`.

Security checks can be skipped when repository visibility or the GitHub plan does not support a feature. A skipped optional check must not be configured as a required status check.

## Review and merge protocol

| Change            | Target    | Merge gate                                                |
| ----------------- | --------- | --------------------------------------------------------- |
| Working branch    | `staging` | All applicable required checks pass                       |
| `staging` release | `main`    | Current staging checks, release review, and rollout notes |

Reviewers focus on correctness, security, maintainability, test coverage, operational impact, and compatibility. Authors remain responsible for responding to feedback and verifying the final commit.

## Security and emergencies

Report vulnerabilities privately using [SECURITY.md](./SECURITY.md), never in a public issue or pull request.

For an urgent production or security issue:

1. Create a focused branch from `staging`.
2. Document the urgency and affected systems without exposing secrets.
3. Open a pull request and run the narrowest complete validation available.
4. Request the appropriate maintainer review.
5. Record follow-up work, remediation, and rollback information.

CI bypasses are for documented infrastructure emergencies only and require a follow-up fix. Never use a bypass to hide a code or test failure.
