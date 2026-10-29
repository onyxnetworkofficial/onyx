# Open-source checklist: before the repository is made public

Decision (docs/TRACKER.md, 2026-10-17): the project owner opens the code under Apache-2.0 ("Anyone should be
able to rerun and check"), but the GitHub repository **stays private** until he says otherwise.
This file lists what must be true before that switch. Making the repository public is the project owner's
explicit step; no loop or agent changes the visibility, creates a public mirror, fork or release,
or publishes a package.

Making the repository public publishes **all of its history**, not only today's tree. Every item
below is about the tree and the history together.

State words: **DONE** (checked on the date given, with the evidence named), **OPEN** (not done),
**PARTIAL** (some of it done; the rest is named). Re-check every DONE item on the exact commit
that goes public.

Last checked 2026-10-17 on branch `open-source` (from main `67a1296`).
`gh repo view --json visibility` (run in this repository) on 2026-10-17: `PRIVATE`.

## A. Blockers (all must be DONE)

| # | Item | Owner | State | Evidence |
|---|---|---|---|---|
| OSS-1 | `.env` values cross-checked against all history, counts only (THREATS SCAN-3, LC-25). The loop never reads `.env` | lead or the project owner | OPEN | [history scan "Lead step"](../audit/2026-10-16-history-secret-scan.md#lead-step-env-values-against-history-counts-only) |
| OSS-2 | History secret scan rerun right before the public push: gitleaks and trufflehog over all refs and unreachable commits, `scripts/check-secrets.sh --history`, and the 64-hex key derivation check against every address (ROADMAP L13) | loop | OPEN (last full run 2026-10-16: no real secret; must be repeated on the commit that goes public) | [history scan report](../audit/2026-10-16-history-secret-scan.md), [launch checklist LC-25](launch-checklist.md) |
| OSS-3 | History with excluded data: rewrite or keep private (THREATS L8-H1..H3, LC-26): `c12ed3a` holds 912 mining payout addresses that may belong to people; `01ebd95` holds 231 wBTC addresses (terms forbid redistribution) and 221 cbBTC addresses (terms unread); `9111db7` holds an operator's personal handle. A rewrite needs the project owner's explicit approval (CLAUDE.md: never rewrite history) | The project owner | OPEN | [data audit open items](../audit/2026-10-13-data-privacy.md#open-items-needs-the-project-owner), THREATS L8-H1..H3 |
| OSS-4 | Label-set rights (THREATS L8-5, LC-1): the tree ships label sets v1 and v2 and data derived from them ([DATA-LICENSE.md section 1](../../DATA-LICENSE.md#1-not-licensed-for-redistribution-pending-label-rights-threats-l8-5)). Either written permission from OKX and Bitwise (and Binance and BitMEX terms reviewed), or a fallback for the repository. The website fallback (ADR-0035) does **not** cover the repository: a public repository would hand out the label files themselves, and they are in history too (so the fallback here means removing them from tree and history, which is OSS-3's decision again) | The project owner | OPEN | [data-sources.md section 4](data-sources.md#4-needs-the-project-owner-rights-decisions), [ADR-0035](../adr/0035-rights-fallback.md) |
| OSS-5 | Removal contact published (THREATS L8-4, LC-2): no e-mail; GitHub issues and the private vulnerability reporting form | loop | DONE 2026-10-25 in text (the owner chose GitHub-only contacts); live when the public repository turns on private vulnerability reporting (PP13) | [label-removal.md](label-removal.md#contact) |
| OSS-6 | Security contact: GitHub private vulnerability reporting, no e-mail | loop | DONE 2026-10-25 in text; turned on with the public repository (PP13) | [SECURITY.md](../../SECURITY.md) |
| OSS-7 | No personal data in tree or history. Tree: the privacy scan reran 2026-10-17 on this branch (`scripts/audit-privacy-scan.py <root> <indexer> git`, 930 files, exit 0); its hit classes match the 2026-10-13 triage (company names that also occur in indexer lists, test e-mail strings, byte noise in images, the one filed OKX address the v2 converter drops), but they were not re-triaged name by name. History: OSS-3; commit author e-mail: OSS-18. Also public: the dev box user name in 3 places in 2 tracked files (home-directory paths in docs/audit) | The project owner (history), loop (rerun and triage) | OPEN | [data privacy audit](../audit/2026-10-13-data-privacy.md#scans) |
| OSS-18 | Commit author e-mail in the history. Checked 2026-10-17 (counts only, the address is not written here): all 610 commits reachable from any ref at `0126964` carry one author and committer e-mail, and it is a personal address, not a GitHub noreply address. Making the repository public publishes it with every commit. Options: **(a)** go public with the history as is and accept that the address becomes public; **(b)** rewrite the history first so every commit carries a noreply or role address (every commit hash changes; can be done together with OSS-3). (b) needs the project owner's explicit approval (CLAUDE.md: never rewrite history); no loop or agent does it on its own. Either way, new commits should use a noreply address before the switch | The project owner | OPEN | `git log --all --format='%ae'` and `'%ce'`, `sort -u`, counted (1 distinct, 0 noreply) |

## B. Files and licences

| # | Item | Owner | State | Evidence |
|---|---|---|---|---|
| OSS-8 | `LICENSE` with the canonical Apache-2.0 text | loop | DONE 2026-10-17: SHA-256 `cfc7749b…3d30`, identical to https://www.apache.org/licenses/LICENSE-2.0.txt | [LICENSE](../../LICENSE) |
| OSS-9 | Every package declares Apache-2.0: Cargo workspace and its 30 crates, `fuzz/` crates, `apps/web/package.json` (added 2026-10-17), the SPDX header of all 50 Solidity files. No file declares another licence | loop | DONE 2026-10-17: `cargo deny --locked list -l license` (30 workspace crates, Apache-2.0), `grep` of `license` in every `Cargo.toml` and of `SPDX-License-Identifier` | [THIRD-PARTY.md](../../THIRD-PARTY.md) |
| OSS-10 | `NOTICE`, `THIRD-PARTY.md`, `DATA-LICENSE.md` | loop | DONE 2026-10-17 (drafts) | [NOTICE](../../NOTICE), [DATA-LICENSE.md](../../DATA-LICENSE.md) |
| OSS-11 | Copyright holder in `NOTICE` (today "The Onyx authors"): keep, or name the legal entity once one exists (legal-data-rights.md open question) | The project owner | OPEN | [NOTICE](../../NOTICE) |
| OSS-12 | The website build ships the Lucide icons' ISC notice and the Feather MIT notice (8 of the 28 icons used derive from Feather) next to the OFL and mining-pools notices | loop | DONE 2026-10-17: `apps/web/static/licenses/LICENSE-lucide.txt` is the package's own `LICENSE`, verbatim; `scripts/build-web.sh` refuses a build where it differs from the installed copy; the e2e test fetches it from the built site; the public-build allowlist names it | [THIRD-PARTY.md section 4](../../THIRD-PARTY.md#4-website-dependencies-npm-appsweb) |
| OSS-13 | Any published binary or Docker image carries the licence notices of the crates compiled into it (only if binaries are published) | loop | OPEN | [THIRD-PARTY.md section 3](../../THIRD-PARTY.md#3-rust-dependencies-cratesio) |

## C. Contributor files

| # | Item | Owner | State | Evidence |
|---|---|---|---|---|
| OSS-14 | `CONTRIBUTING.md` | loop | DONE 2026-10-25: DCO sign-off, no CLA | [CONTRIBUTING.md](../../CONTRIBUTING.md) |
| OSS-15 | `SECURITY.md` | loop | DONE 2026-10-25: GitHub private reporting; first reply within 7 days, fix or decision within 30 | [SECURITY.md](../../SECURITY.md) |
| OSS-16 | `CODE_OF_CONDUCT.md` | loop | DONE 2026-10-25: own short text; reports through an issue or the private form | [CODE_OF_CONDUCT.md](../../CODE_OF_CONDUCT.md) |
| OSS-17 | CI runs again before outside pull requests (LC-24, GitHub Actions billing) | The project owner | OPEN | [launch checklist LC-24](launch-checklist.md) |

## E. Option A: a new repository from a history-free snapshot (2026-10-24)

The project owner asked (2026-10-24) to keep improving before anything goes public. One way to meet OSS-3
and OSS-18 without rewriting history: publish a new repository from today's files only.
`scripts/public-snapshot.sh OUT --without-pending` builds that snapshot in a local folder and
checks it; it never pushes or creates a repository (section D still applies).

- Left out while DATA-LICENSE section 1 is pending: the paths in
  [public-snapshot-exclude.txt](public-snapshot-exclude.txt) (46 paths at `5a75878`; 62 at
  `7260915` once label set v3 and its roster snapshots merged). Without `--without-pending` the
  script refuses while that section is pending. `scripts/public-snapshot-coverage.py` (run by the
  script and by `scripts/check-docs.sh`) fails when DATA-LICENSE section 1 names a path the list
  does not cover; it caught v3's rosters on 2026-10-25, which the first list had missed.
- 2026-10-24 at `5a75878` (branch partner-integration, 1,092 files): gitleaks over the folder found
  no secret after 4 new public values were reviewed and allowlisted (a source key id, a key-proof
  challenge and a reveal key's two coordinates); the e-mail guard found no address outside the
  reviewed kinds (reserved test domains, git remotes, image names, synthetic intake vectors, one
  organisation's published role contact). `--self-test` plants a fake personal address and a fake
  token, and both are caught.
- Open: the snapshot without the left-out paths does not yet build and test on its own (label
  fixtures feed golden and reserve tests); a fresh repository also needs OSS-4..6 and OSS-11..17.

## F. Anonymity at publish (2026-10-26)

The project owner: "everything needs to be anonymous". Checked on 2026-10-26:

- Domain: the public registration record shows the registrar's privacy service only (RDAP). The
  domain forwards mail through the registrar; any reply must come from a project address, never a
  personal one. The web archive holds no capture of the domain since 2025.
- Server: the reverse name is the host's generic one; the web server sends no `Server` header and
  holds no certificate e-mail.
- Site: the verifier's WebAssembly carried the builder's home folder path in its panic messages
  (136 places). `scripts/build-web.sh` now maps it to `/build`, and the build, `deploy-site.sh` and
  `public-snapshot.sh` refuse any file, binary or text, that still holds it.
- Commits of the public repository: author and committer "Onyx contributors" with the new
  account's GitHub noreply address; dates in UTC (`TZ=UTC`, `+0000`), since a local offset names a
  time zone; no session trailers.
- GitHub: the organization belongs to a new account with its own e-mail, not the private
  repository's account.

## D. The switch itself (the project owner only)

1. Every item in A is DONE on the commit to publish; B and C as far as the project owner wants them.
2. Re-run `scripts/check-all.sh`, `scripts/check-secrets.sh --history` and OSS-2's scans on that
   commit.
3. The project owner changes the visibility on GitHub himself. Nothing in this repository automates it.
