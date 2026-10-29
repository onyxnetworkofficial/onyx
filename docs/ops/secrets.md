# Secret handling

## Scope

Rules for every secret: private keys, RPC API keys, database passwords, facilitator credentials.

## Current state (M0)

- The only secret is the Base Sepolia testnet deployer key in `.env` (`BASE_SEPOLIA_PRIVATE_KEY`).
- `.env` and `.env.*` are in `.gitignore`; only `.env.example` (no values) is committed.
- Rules, in force now:
  1. Never print a secret: not in logs, test output, error messages, CI logs or chat.
  2. Never commit a secret. Never paste one into an issue, PR, ADR or doc.
  3. Testnet keys only. No real funds, no mainnet keys, ever, in this repo or its tools.
  4. Secrets are read from the environment at process start, never from CLI arguments (shell history leaks them).
  5. Config debug output redacts secret fields.

## Plan

- M1: a `Secret<T>` wrapper whose `Debug`/`Display` print `[redacted]`.
- M6: CI never receives the deployer key; deploys run from the operator machine only.
- Done (L9 sweep, 2026-10-14): `scripts/check-secrets.sh` in `scripts/check-all.sh` (so in CI too) fails on a
  tracked `.env`, `*.key`, `*.pem` or `.onyx-testnet/` file and on a `*_KEY`/`SECRET` assignment of 64 hex
  digits or a PEM private key in any tracked file; it prints file:line only, and its `--self-test` plants
  a fake key in a temp repo first. It scans the tree, not history: run a history scanner (for example
  gitleaks; free, open source) before the repository is ever public (THREATS L8-H1, L10).
- Done (L13, 2026-10-15): history scanned with gitleaks 8.30.1 and trufflehog 3.97.9; no real
  secret ([report](../audit/2026-10-16-history-secret-scan.md); rescanned 2026-10-16).
  `scripts/check-secrets.sh --history` repeats the pattern scan over `git log --all -p` (merge
  resolutions included) and gitleaks with `.gitleaks.toml` (counts only; `--list` for
  commit:file:line); it fails when gitleaks is missing (THREATS SCAN-4, SCAN-5).
  `.gitleaks.toml` allowlists the reviewed test values by exact path and value. Before any public
  push: rerun both, and the counts-only `.env` cross-check in the report (THREATS SCAN-3).
- Later: move secrets to a managed secret store (see [key-management.md](key-management.md)).

## Open questions

- Which history scanner to run once before going public? Answered (L13): gitleaks, with trufflehog as a second opinion.
- Will any secret ever be needed in CI (for example a testnet RPC key)? If so, use GitHub encrypted secrets, scoped to one job.
