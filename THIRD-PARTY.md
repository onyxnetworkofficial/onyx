# Third-party code, fonts and vectors

Onyx's own code is Apache-2.0 ([LICENSE](LICENSE)). This file lists what in the repository, or
in what it builds, comes from someone else. Data sets and their terms are in
[DATA-LICENSE.md](DATA-LICENSE.md). Not legal advice.

Last checked 2026-10-17 (branch `open-source` from main `67a1296`).

## 1. Files from others committed in this repository

| Path | What | Licence | Notice shipped |
|---|---|---|---|
| `apps/web/static/fonts/ibm-plex-*.woff2` | IBM Plex Sans and Mono, Copyright 2019 IBM Corp. | SIL OFL 1.1 | `apps/web/static/fonts/OFL-ibm-plex.txt` (ships with the site) |
| `apps/web/static/fonts/instrument-serif-*.woff2` | Instrument Serif, Copyright 2022 The Instrument Serif Project Authors | SIL OFL 1.1 | `apps/web/static/fonts/OFL-instrument-serif.txt` (ships with the site) |
| `apps/web/static/licenses/LICENSE-lucide.txt` | The `LICENSE` file of @lucide/svelte 1.54.0, verbatim (ISC for Lucide; MIT for the Feather-derived icons) | ISC, MIT | is itself the notice; ships with the site (section 4) |
| `fixtures/pools/pool-allowlist.json` | 127 reviewed pools from https://github.com/bitcoin-data/mining-pools (branch `generated`) | MIT | `fixtures/pools/LICENSE-mining-pools.txt`; `scripts/build-web.sh` copies it into the site's `data/` |
| `fixtures/bip322/*.json` | BIP-322 test vectors from https://github.com/bitcoin/bips (bip-0322) | CC0-1.0 | not required; source in `fixtures/bip322/README.md` |
| `fixtures/tlock/go-tlock/*`, `fixtures/tlock/tlock-js/*` | Time-lock test ciphertexts from drand/tlock and drand/tlock-js, verbatim | Apache-2.0 OR MIT (per `fixtures/tlock/PROVENANCE.md`) | source URLs and commits in `fixtures/tlock/PROVENANCE.md` and `vectors.json` |
| `fixtures/tlock/drand/*.json` | drand quicknet and quicknet-t chain info and published round signatures | none stated (public randomness beacon) | source URLs in `fixtures/tlock/PROVENANCE.md` |

No third-party source code is copied or vendored into the tree. Two notes from the search:

- `crates/onyx-tlock` reimplements the drand/tlock scheme from its description and tests it
  against the vectors above; it does not copy Go or JavaScript source.
- "Ported from the the entity indexer logic" (`recipes/btc-entity-balance`) refers to the
  project owner's own indexer, not to third-party code.

## 2. Git submodule (not committed, fetched on clone)

| Path | Project | Licence | Use |
|---|---|---|---|
| `contracts/lib/forge-std` | foundry-rs/forge-std v1.17.0 (`f3dae6e6`, pinned in `contracts/foundry.lock`) | Apache-2.0 OR MIT (its `LICENSE-APACHE`, `LICENSE-MIT`) | contract tests and deploy scripts only; `contracts/src` imports nothing from it |

## 3. Rust dependencies (crates.io)

Enumerated with `cargo deny --locked list -l license -f json` on 2026-10-17: 202 crates from
crates.io, 30 workspace crates (all Apache-2.0), unlicensed 0. `deny.toml` is the policy
(`scripts/check-deps.sh` fails on any licence outside it); no copyleft licence is allowed.

Crates per licence term (a dual-licensed crate counts under each of its terms):

| Licence | Crates |
|---|---|
| MIT | 157 |
| Apache-2.0 | 132 |
| Apache-2.0 WITH LLVM-exception | 32 (wasmtime, cranelift) |
| CC0-1.0 | 9 |
| ISC | 5 (ring, rustls, rustls-webpki, hyper-rustls, untrusted) |
| Unlicense | 3 (memchr, termcolor, winapi-util; each also offers MIT) |
| BSD-3-Clause | 2 (matchit, subtle) |
| Zlib | 2 (foldhash, miniz_oxide) |
| 0BSD, BSD-2-Clause, BSL-1.0 | 1 each (adler2, mach2, ryu; each also offers a permitted licence) |
| CDLA-Permissive-2.0 | 1 (webpki-roots: Mozilla's root certificate list as data) |
| MITNFA | 1 (hex_lit: MIT plus "no false attribution") |
| Unicode-3.0 | 1 (unicode-ident) |

Named because they matter:

- **blst** 0.3.17 (supranational/blst, BLS12-381 in C and assembly): Apache-2.0. Only
  `crates/onyx-tlock` may depend on it (`deny.toml`); review in
  [docs/audit/2026-10-17-blst.md](docs/audit/2026-10-17-blst.md).
- Apache-2.0 section 4(d) asks redistributors to pass on a dependency's NOTICE file. Of the 202
  crates' sources, only `cfg_aliases` 0.2.2 has one (`NOTICES.md`, an MIT notice for code from
  tectonic); it is a build-time dependency of borsh and is not linked into any binary.

Binary releases (Docker images, node binaries) carry these crates compiled in. Before a binary is
published, its licence notices must ship with it (open item OSS-13 in
[docs/ops/open-source-checklist.md](docs/ops/open-source-checklist.md)).

## 4. Website dependencies (npm, `apps/web`)

All are `devDependencies`; `pnpm licenses list --json` (same `pnpm-lock.yaml` as this branch,
2026-10-17): 56 packages: MIT 45, Apache-2.0 5, ISC 2, MPL-2.0 3, BSD-3-Clause 1.

| Package | Licence | Shipped in the site build? |
|---|---|---|
| svelte 5.57.2, @sveltejs/kit 3.0.1 | MIT | yes (runtime bundled) |
| @lucide/svelte 1.54.0 | ISC, Copyright (c) 2026 Lucide Icons and Contributors. The package's `LICENSE` lists the icons derived from Feather (MIT, Copyright (c) 2013-present Cole Bemis); 8 of the 28 icons the site imports are on that list: `arrow-right`, `check`, `corner-down-left`, `link`, `lock`, `search`, `terminal`, `x` | yes (28 icons). Both notices ship verbatim as `apps/web/static/licenses/LICENSE-lucide.txt` (the package's own `LICENSE`; `scripts/build-web.sh` refuses a build where it differs from the installed copy; e2e checks it is served) |
| axe-core 4.14.0 | MPL-2.0 | no (accessibility tests only, `apps/web/tests/`) |
| lightningcss (and its native binary) | MPL-2.0 | no (Vite's CSS tool at build time) |
| playwright-core 1.64.0 | Apache-2.0 | no (browser tests only) |

## 5. Container base images

`Dockerfile` builds on `rust:1.98.0-slim-bookworm` and runs on
`gcr.io/distroless/cc-debian12:nonroot` (both pinned by digest). Their Debian packages carry
their own licences; nothing from them is committed here.
