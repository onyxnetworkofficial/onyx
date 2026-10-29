# Contributing to Onyx

## Licence of contributions

Onyx's code is Apache-2.0 ([LICENSE](LICENSE)). Unless you say otherwise in writing, anything you
submit for inclusion is licensed under Apache-2.0 too (section 5 of the licence).

Sign each commit off with the [Developer Certificate of Origin](https://developercertificate.org/):
`git commit -s`. There is no CLA.

## Before you open a pull request

1. Read [README.md](README.md), [PLAN.md](PLAN.md) and the spec for the part you change
   (`docs/spec/`). A real design decision needs an ADR (`docs/adr/`).
2. Write the test first for any protocol rule.
3. Run `scripts/check-all.sh` (format, clippy, tests, boundaries, docs, secrets, dependencies,
   contracts). It must pass. The CI workflow runs the same script.
4. Keep commits small and say what changed and why. Never rewrite shared history.

## Code rules (short form; the full list is in [CLAUDE.md](CLAUDE.md))

- Core crates (`crates/*`) never depend on `adapters/*` or `recipes/*`.
- No floats in protocol data; amounts in smallest units, probabilities in ppm.
- Hashed structures are Borsh, hashes are Keccak-256 (ADR-0002, ADR-0003, ADR-0011).
- `unwrap()` only in tests; error enums with `thiserror`.
- No wall-clock asserts or fixed sleeps in tests: controlled time (tokio `start_paused`), a wait for the
  condition the check reads (`apps/web/tests/wait.mjs`), or a bound tied to the configured deadline.
- A change to a security assumption adds or updates a row in [THREATS.md](THREATS.md).

## Data rules (these are hard limits)

- Labels name **entities only** (exchanges, ETFs, funds, protocols, pools). Never a person, a
  person's address, name, handle or e-mail.
- Never add data whose licence forbids redistribution. A new source needs its terms quoted and
  a decision in [docs/ops/data-sources.md](docs/ops/data-sources.md) before any code.
- Data is not covered by the code licence; say where it came from and its terms
  ([DATA-LICENSE.md](DATA-LICENSE.md)).
- No invented numbers or claims about other projects; link sources.

## Secrets

Never commit `.env`, keys or credentials. Use testnet keys only. `scripts/check-secrets.sh`
fails on key-shaped values; if it flags a real secret, treat the secret as leaked and rotate it.

## Security issues

Do not open a public issue for a vulnerability. Follow [SECURITY.md](SECURITY.md).

## Corrections and removal of labels

See [README "Corrections and removal"](README.md#corrections-and-removal) and
[docs/ops/label-removal.md](docs/ops/label-removal.md).

## Conduct

Follow [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
