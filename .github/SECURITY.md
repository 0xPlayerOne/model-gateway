# Security Policy

## Supported Versions

Only the **latest commit on `staging`** receives security patches.  
Patches are fast-forwarded to `main` on the next release cycle.

| Branch           | Supported                     |
| ---------------- | ----------------------------- |
| `staging`        | ✅                            |
| `main`           | ✅ (via staging→main release) |
| Feature branches | ❌                            |

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Report privately via one of the following channels:

1. **Discord** — DM NiftyAndy (`0xPlayerOne`) directly in the [Model Gateway Discord](https://discord.gg/niftyleague)
2. **Email** — Contact Andrew Mahoney-Fernandes at the email listed on [niftyleague.com](https://niftyleague.com)

Within 48 hours you will receive:

- Confirmation of receipt
- An assessment timeline
- A target date for the fix

## Disclosure Policy

- The reporter and the Model Gateway team coordinate disclosure.
- Public disclosure happens **after** a fix has been deployed to production.
- We appreciate responsible disclosure and will acknowledge reporters in release notes (unless anonymity is requested).

## Scope

This policy covers all code in the `0xPlayerOne/model-gateway` repository, including all apps and packages within the monorepo.  
For vulnerabilities in smart contracts, see `nifty-smart-contracts` security policy.
