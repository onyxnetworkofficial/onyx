# Dependency audit, 2026-10-13 (ROADMAP L1)

Branch `audit-deps` from main `6665104`. Free, open-source tools only. Result: **0 open
advisories** in Rust and npm dependencies, **no licence problems** (no copyleft in anything
shipped), **no upgrades needed**. One compiler item stays open: solc 0.8.30 has 7 known bugs
fixed in later releases; none of their triggers is in `contracts/src` (THREATS DEP-1).

## Tools

| Tool | Version | Database |
|---|---|---|
| cargo-audit | 0.22.2 | RustSec advisory-db `29d1a60c` (2026-10-13T15:53:07+02:00), 1,310 advisories |
| cargo-deny | 0.20.2 (installed with `cargo install --locked cargo-deny`) | same RustSec commit |
| pnpm | 10.34.5 (node v22.23.2) | npm registry bulk advisory endpoint (GitHub Advisory Database) |
| forge | 1.8.5 (`51a52c59`) | none automatic, see Foundry below |
| rustc / cargo | 1.98.0 | |
| solc known bugs | [`bugs_by_version.json`](https://github.com/ethereum/solidity/blob/develop/docs/bugs_by_version.json), [`bugs.json`](https://github.com/ethereum/solidity/blob/develop/docs/bugs.json) from ethereum/solidity `develop`, fetched 2026-10-13 (re-read 2026-10-15 for L10: one new bug, SOL-2026-7, THREATS DEP-3) | |

## Commands (from the repo root)

```
cargo audit                                  # exit 0
cargo audit --json                           # vulnerabilities.count 0, warnings {}
cargo deny --locked check                    # exit 0: advisories ok, bans ok, licenses ok, sources ok
cargo deny --locked list -l license -f json  # licence inventory below
cargo update --dry-run                       # 13 semver-compatible updates, none security
(cd apps/web && pnpm audit)                  # exit 0: "No known vulnerabilities found"
(cd apps/web && pnpm audit --json)           # 0 info/low/moderate/high/critical, 82 packages
(cd apps/web && pnpm licenses list --json)   # licence inventory below
(cd apps/web && npm audit)                   # exit 1 ENOLOCK: the app uses pnpm, no package-lock.json
git submodule update --init                  # forge-std f3dae6e6 = v1.17.0 (foundry.lock)
gh api "/repos/foundry-rs/forge-std/security-advisories"   # []
gh api "/advisories?ecosystem=npm&affects=forge-std"       # []
python3 recursion scan of contracts/src (call-graph cycle search)  # only a false positive, see DEP-1
```

There is one Cargo lockfile: `recipes/*` and `apps/web/verify` are workspace members, so the
workspace run covers them. `apps/web` is the only `package.json` in the repo.

## Counts before and after

| Check | Before | After |
|---|---|---|
| cargo audit vulnerabilities | 0 | 0 |
| cargo audit warnings (unmaintained, unsound, yanked) | 0 | 0 |
| cargo deny advisories / licenses / bans / sources errors | no config (not run before) | 0 / 0 / 0 / 0 |
| cargo deny duplicate-version warnings | n/a | 4 (`hashbrown`, `syn`, `wasmparser`, `windows-sys`) |
| pnpm audit (apps/web), all severities | 0 | 0 |
| solc 0.8.30 known bugs | 7 | 7 (none triggered; DEP-1) |

`Cargo.lock`: 250 packages, 222 from crates.io and 28 workspace crates; no git dependencies, no
other registry.

## Advisories and decisions

| Source | Advisory | Decision |
|---|---|---|
| RustSec | none | nothing to fix; `deny.toml` `ignore = []` |
| npm (apps/web) | none | nothing to fix; no `pnpm audit --fix` run |
| solc 0.8.30 | `TransientStorageClearingHelperCollision` (high; via-IR, cancun; fixed 0.8.34) | not triggered: no `transient` variable in `contracts/src`. DEP-1 |
| solc 0.8.30 | `InheritanceOrderReversalOnStorageEndWarning` (medium; fixed 0.8.36) | not triggered: no `layout at` custom storage layout. DEP-1 |
| solc 0.8.30 | `UnsoundSpillInMutualRecursion` (medium; via-IR; fixed 0.8.36) | not triggered: no recursion (the call-graph scan's one cycle, `ledgerId`, is `OnyxLedger.ledgerId()` calling the library overload `OnyxRound.ledgerId(chainId, settlement)`). DEP-1 |
| solc 0.8.30 | `SpillSlotCollisionAcrossMutualRecursion` (low/medium; via-IR; fixed 0.8.37) | not triggered: no recursion. DEP-1 |
| solc 0.8.30 | `MemoryByteArrayElementDeleteClearsWholeWord` (low/medium; legacy codegen; fixed 0.8.37) | not triggered: the build is via-IR and `contracts/src` has no `delete`. DEP-1 |
| solc 0.8.30 | `MisorderedNamedParametersInRequireWithCustomErrors` (very low; via-IR; fixed 0.8.37) | not triggered: no `require(..., Error({...}))` with named arguments. DEP-1 |
| solc 0.8.30 | `LostStorageArrayWriteOnSlotOverflow` (low; fixed 0.8.32) | not triggered: arrays live at compiler-chosen slots, no custom layout near the end of storage. DEP-1 |

Not changed here: bumping `solc_version` changes every contract's bytecode and the deployed
Base Sepolia contracts a live testnet run is using; it belongs with the contract audit (L2),
which re-runs `forge build --sizes` and the full test and invariant suite on the new compiler.
**Review by 2026-11-13 or before any mainnet deploy, whichever is first.**

## Upgrades

None. No advisory needed one. `cargo update --dry-run` lists 13 semver-compatible updates (cc,
hyper, hyper-rustls, serde_json, smallvec, syn, tokio-rustls, toml_*, unicode-ident, want,
zeroize); none fixes an advisory, and a lockfile change can change the reproducible recipe bytes
in `recipes/out/` (R-REPRO-2), so they are left for a normal maintenance change.

## Licences

`deny.toml` allows exactly the licences in use on this date, all permissive: Apache-2.0,
Apache-2.0 WITH LLVM-exception, MIT, BSD-3-Clause, ISC, Zlib, CC0-1.0, Unicode-3.0,
CDLA-Permissive-2.0 (webpki-roots: Mozilla's root list as data), MITNFA (hex_lit: MIT plus "no
false attribution"). Dual-licensed crates pass through one of these; 0BSD, BSD-2-Clause,
BSL-1.0 and Unlicense appear only as alternatives in OR expressions and are not allowed on their
own. **No copyleft** (GPL, LGPL, AGPL, MPL, EPL, CDDL) in any Rust crate.

Rust graph (crates per licence term, a dual-licensed crate counts under each): Apache-2.0 154,
MIT 152, Apache-2.0 WITH LLVM-exception 32 (wasmtime/cranelift), CC0-1.0 9 (rust-bitcoin,
secp256k1), ISC 5, Unlicense 3, BSD-3-Clause 2, Zlib 2, 0BSD 1, BSD-2-Clause 1, BSL-1.0 1,
CDLA-Permissive-2.0 1, MITNFA 1, Unicode-3.0 1. Unlicensed: 0.

apps/web (82 packages, all devDependencies; the build bundles svelte, @sveltejs/kit and
@lucide/svelte into the site): MIT 45, Apache-2.0 5, ISC 2, BSD-3-Clause 1, MPL-2.0 2. The two
MPL-2.0 packages are `lightningcss` and its native binary: Vite's CSS tool, run at build time
and not shipped in `apps/web/build`, so the file-level copyleft has no effect on the site.

Contracts: forge-std is MIT OR Apache-2.0 and test/script only; `contracts/src` imports nothing
outside itself.

## Foundry dependencies

`contracts/lib` holds only forge-std v1.17.0 (`f3dae6e6`, pinned in `foundry.lock`; the latest
release on 2026-10-13). It is used by tests and deploy scripts, never by `contracts/src`, so no
forge-std code is deployed. No advisory feed covers Solidity libraries the way RustSec and npm
do: the forge-std GitHub security advisories and the GitHub Advisory Database return none, and
this check is manual. forge 1.8.5 is the latest Foundry release. The solc known-bugs list is
the compiler's own advisory feed; it was checked by hand above (DEP-1) and is not in the gate.

## The gate

`scripts/check-deps.sh`, called by `scripts/check-all.sh` (and so by CI, which installs
cargo-deny 0.20.2 first):

1. `cargo deny -L error --locked check advisories licenses bans sources`; when the advisory
   database cannot be reached it prints `deps: offline, RustSec advisories SKIPPED` and still
   checks licences, bans and sources (offline).
2. `pnpm audit --audit-level low` in apps/web, any severity fails; skipped with a message when
   offline or when pnpm is missing (the CI image may not have pnpm, DEP-2).

Checked: the gate passes online (exit 0) and through a dead proxy (offline messages, exit 0);
removing `MITNFA` from `deny.toml` makes it fail (`licenses FAILED`, exit 4).

## Open (THREATS)

- DEP-1 solc 0.8.30 known bugs, none triggered; bump with L2.
- DEP-2 advisory checks that are skipped: offline runs, CI without pnpm, and the manual Foundry
  and solc checks.

## Repository checks after the change

| Command | Exit |
|---|---|
| `ONYX_WEB=1 scripts/check-all.sh` (runs the new deps gate: "advisories ok, bans ok, licenses ok, sources ok", "No known vulnerabilities found") | 0 (`ALL CHECKS PASSED`, website e2e passed) |
| `ONYX_PUBLIC=1 scripts/build-web.sh` | 0 (public build, e2e passed) |
| `cd contracts && forge test` | 0 (447 passed, 0 failed, 1 skipped) |
