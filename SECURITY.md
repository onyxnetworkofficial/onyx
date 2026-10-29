# Security policy

## Status

Onyx is a prototype on testnets only (Base Sepolia). No contract or key holds real funds. There
is no bug bounty.

## Reporting a vulnerability

Use GitHub private vulnerability reporting: open the repository's **Security** tab and choose
**Report a vulnerability**. Only the maintainers see the report. Onyx has no e-mail contact.
Do not open a public issue for a vulnerability.

Please include what is affected (crate, contract, script or page, and the commit), how to
reproduce it, and what an attacker gains. Do not open a public issue, and do not test against
anyone else's node or wallet.

What to expect: a first reply within 7 days, and a fix or a decision within 30 days. We credit
reporters who want it.

## Scope

In scope: the code in this repository (`crates/`, `adapters/`, `recipes/`, `contracts/`,
`apps/web/`, `scripts/`, `deploy/`) and the testnet deployment it describes.

Out of scope: third-party dependencies (report those upstream; see [THIRD-PARTY.md](THIRD-PARTY.md)),
testnet faucets and public RPC endpoints.

## Known threats

The threat model and every known finding, with its test or open item, are public in this
repository: [THREATS.md](THREATS.md) and [docs/audit/](docs/audit/). Incident handling:
[docs/ops/incident-response.md](docs/ops/incident-response.md).
