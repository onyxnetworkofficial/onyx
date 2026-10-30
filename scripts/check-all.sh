#!/usr/bin/env bash
# Local mirror of CI. Run before every commit.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$PATH:$HOME/.foundry/bin"
scripts/build-recipes.sh >/dev/null
# Rebuilt bytes must equal what is staged or committed, and nothing may be untracked (R-REPRO-2).
if [ -n "$(git ls-files --others --exclude-standard -- recipes/out)" ] || ! git diff --quiet -- recipes/out; then
  echo "recipes/out differs from the index or has untracked files: stage the rebuild or fix reproducibility"
  git status --short -- recipes/out; exit 1
fi
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
# The test-only open path still compiles and lints (SER-R2-F1).
cargo clippy --locked -p onyx-coordinator --features unchecked-open -- -D warnings
cargo test --locked --workspace
# ROADMAP C50: the time-lock vectors on wasm32 (blst's C built by clang, run in node).
scripts/check-tlock-wasm.sh
# CL3 (THREATS CL3-3): the generic JSON fetcher's shared vectors on wasm32, as natively.
scripts/check-json-wasm.sh
# NS8 (ADR-0047): the agent toolkit's tests, then an MCP session against two demo servers.
scripts/agent-smoke.sh
# CL2: the contributor client (apps/contrib) with a fake provider and a fake intake on loopback:
# the API key never reaches Onyx, a tool result, a log line or a stored file (THREATS CL2).
node --test apps/contrib/test/*.test.mjs
# The service binary (docs/ops/deployment.md) builds and parses its arguments.
cargo run --locked -q -p onyx-node -- --help >/dev/null
# The Bitcoin build with the reserve serving check (ADR-0034 Amendment 1).
cargo run --locked -q -p onyx-reserve-serve --bin onyx-node-btc -- --help >/dev/null
# ROADMAP C5 (ADR-0036 amendment 1): the track-record intake against a local anvil on a free
# loopback port: posts, chain-time `committed_at`, sealing, the client recheck and five attacks.
scripts/track-intake-anvil.sh
# ROADMAP C7 (ADR-0036 amendment 3): resolutions from OnyxSignedBook facts only and commitments in
# its append-only log, against a local anvil: the client recheck and eight attacks (incl. a reorg).
scripts/track-onchain-anvil.sh
scripts/prepaid-anvil.sh
# CL6 (ADR-0056, THREATS CL6): the credits ledger on a local anvil: an observer with no ETH deposits
# and posts its commit and receipt through the relayer, paid from credits; a miner's earned share
# moved in; forged, repeated and over-cap relays refused; the fees paid to the relayer.
scripts/credits-anvil.sh
# CL7 (ADR-0057): a correction market end to end on anvil (about 30 s once contracts are built):
# the live manifest and its recheck registered with their real template ids, a value, a market,
# bets, a recheck that disagrees, settlement and payouts to the wei; a forged value, a market after the
# recheck, an insider bet and the original round as the recheck all refused.
scripts/corrections-anvil.sh
# ROADMAP C10 (ADR-0028 amendment 7): OnyxSignedBook deployed and pinned with the tooling the lead
# runs live (reviewer == deployer refused), onyx-node refusing broken pins and serving level 3 only
# against `settleable` at its finalized block, coordinator posts, censorship hold, clock skew.
scripts/level3-book.sh local
# CL5 (ADR-0059): a publisher's dataset end to end on anvil (synthetic rows, a fresh test key):
# build, sign, anchor in OnyxSignedBook, upload, proven range reads checked by onyx-check --bulk,
# and the attacks (forged row and shard, unanchored swap, unknown key, compromised key and its
# recovery). Small here; `scripts/bulk-demo.sh local` alone runs the 100 MiB rehearsal.
BULK_DEMO_MIB=6 BULK_DEMO_SHARD_MIB=1 scripts/bulk-demo.sh local
scripts/check-boundaries.sh
scripts/check-docs.sh
# Secret scan of the tracked tree, after its own planted-key self-test (THREATS T-KEY-1).
scripts/check-secrets.sh --self-test
scripts/check-secrets.sh
# ROADMAP L12: the health watch against fake node/RPC/site servers on 127.0.0.1 (every threshold
# edge, malformed answers, RPC down, stalled, wrong chain, de-duplication; no outside network).
python3 -I scripts/health-watch-tests.py
# ADR-0043 (THREATS CHAIN-6): the on-chain refresh against a fake Bitcoin Core REST server on
# 127.0.0.1 (synthetic blocks): what it writes, the lies it refuses, and the pin writer.
python3 -I scripts/refresh-onchain-tests.py
# THREATS P3A-R1-5: an independent Python recount of the shipped block input (all 22 fields of
# block 970,835 from its ChainSource) equals its committed leaf; planted changes are caught.
python3 -I scripts/recount-chain-input.py --self-test
# The machine-wide lock of scripts/docker-smoke.sh (no Docker, about 14 s): two holders never hold
# it at once, a held lock times out cleanly and names its holder, a leaked child cannot keep it.
scripts/smoke-lock-tests.sh
# ROADMAP E13 (THREATS REMOTE-1..10): the remote check's refusals without the host: arguments,
# secrets, credential files and never-synced paths in the file list, symlinks and the real rsync
# into a local folder, an unreachable host (255), check-all's status passed back unchanged even
# when the script is rewritten mid-run, the recipes/out and submodule gates, the host's
# load and disk rules, two build slots, --clean deleting only the branch's two folders, and the
# job's Landlock rules.
scripts/remote-check.sh --self-test
# THREATS L9A-7: the run-page indexer reader keeps only scheme://host[:port] of its RPC URL.
python3 -I scripts/read-indexer-sepolia.py --self-test
# THREATS ETF-5: the SEC ETF recorder paces requests within SEC's 10 per second, fetches only
# data.sec.gov, follows no redirect, names no person in its User-Agent (offline, no request sent).
python3 -I scripts/record-sec-etf-holdings.py --self-test
# ADR-0044 (THREATS ETH-7): the Ethereum window fetch against fake JSON-RPC servers on 127.0.0.1
# serving the committed mainnet blocks: what it writes, the lies it refuses, the rate and the pins.
python3 -I scripts/refresh-eth-onchain-tests.py
# THREATS L9A-6: a public build flag other than 0 or 1 is refused before anything builds.
if ONYX_PUBLIC=true scripts/build-web.sh >/dev/null 2>&1; then
  echo "build-web.sh accepted ONYX_PUBLIC=true (must refuse anything but 0 or 1)"; exit 1
fi
# ROADMAP L14: the rights switch is as strict, and a fallback build must be a public one.
# Review L14-R1: a public build with ONYX_RIGHTS unset or empty is refused too (fails closed).
if env -u ONYX_RIGHTS ONYX_PUBLIC=1 scripts/build-web.sh >/dev/null 2>&1; then
  echo "build-web.sh built a public site with ONYX_RIGHTS unset (must refuse)"; exit 1
fi
for bad in "ONYX_RIGHTS=Fallback ONYX_PUBLIC=1" "ONYX_RIGHTS=fallback ONYX_PUBLIC=0" "ONYX_RIGHTS= ONYX_PUBLIC=1"; do
  # shellcheck disable=SC2086 # two assignments, split on purpose
  if env $bad scripts/build-web.sh >/dev/null 2>&1; then
    echo "build-web.sh accepted $bad (must refuse)"; exit 1
  fi
done
# Dependencies (ROADMAP L1): RustSec advisories, licences, bans and sources (deny.toml) and
# the apps/web npm advisories; network checks skip with a message when offline.
scripts/check-deps.sh
# `--sizes` fails if any contract exceeds the EIP-170 runtime limit (24,576 B), which only a real
# deploy would otherwise reveal (M6-L36-F1). Test-only contracts (the L35 `Harness` extends the
# settlement contract) never deploy on chain, so they are skipped (M5-L46-F3).
(cd contracts && forge fmt --check && forge build --sizes --skip test && forge test)
# OnyxSettlement runs OnyxSettlementExt by delegatecall on its own storage (ADR-0024 item 1): the
# two storage layouts must be identical (L2 audit, Slither uninitialized-state triage S-2..6).
# Each side is captured on its own (pipefail is on, so a failing `forge inspect` or parser stops
# here) and must be non-empty before the compare: the gate never passes on two empty outputs
# (review L2-R1, THREATS L2-R1-4).
layout='import json,sys; d=json.load(sys.stdin); [print(x["slot"],x["offset"],x["label"],d["types"][x["type"]]["label"]) for x in d["storage"]]'
settle_layout=$(cd contracts && forge inspect OnyxSettlement storageLayout --json | python3 -I -c "$layout") \
  || { echo "storage layout of OnyxSettlement could not be read"; exit 1; }
ext_layout=$(cd contracts && forge inspect OnyxSettlementExt storageLayout --json | python3 -I -c "$layout") \
  || { echo "storage layout of OnyxSettlementExt could not be read"; exit 1; }
if [ -z "$settle_layout" ] || [ -z "$ext_layout" ]; then
  echo "empty storage layout (OnyxSettlement or OnyxSettlementExt)"; exit 1
fi
if [ "$settle_layout" != "$ext_layout" ]; then
  echo "OnyxSettlement and OnyxSettlementExt storage layouts differ"
  diff <(printf '%s\n' "$settle_layout") <(printf '%s\n' "$ext_layout") || true
  exit 1
fi
# Gas regression (L2 audit): unit-test gas must equal contracts/.gas-snapshot, built under the
# `gas` profile (no CBOR metadata, so paths and comments do not change gas; THREATS L2-R1-1). After an
# intended change, rerun the same command without --check and commit the new snapshot. Fuzz,
# invariant and fork tests are left out (their gas varies with inputs).
# --force rebuilds every contract first: an incremental build cache from before a merge gave
# stale gas that was committed twice (C2-R2, then C4; lead fix 2026-10-17).
(cd contracts && FOUNDRY_PROFILE=gas forge snapshot --check --force --no-match-test 'testFuzz_|invariant_' --no-match-contract 'Fork')
# Website gate (AUD-1-F6): needs pnpm, wasm-bindgen-cli and the demo export with the live blocks
# (apps/web/README.md), so it is opt-in here and absent from CI: ONYX_WEB=1 scripts/check-all.sh.
# ROADMAP E10: the public builds' guards (no label row, no level-2 value; with fallback rights no
# label-derived total) and their e2e run only with ONYX_WEB_PUBLIC=1 (opt-in like the preview:
# same tools and data; each build about as slow again). ONYX_RIGHTS is always set explicitly
# (ROADMAP L14: a public build without it is refused), and both rights modes are built. The
# private preview builds last, so apps/web/build ends as the preview (lead decision 2026-10-16);
# it is named explicitly too, so an ONYX_PUBLIC or ONYX_RIGHTS left in the shell cannot turn it
# into a third public build (E10 review). The builds run one after another (build-web.sh also
# refuses a second build in the same worktree).
if [ "${ONYX_WEB_PUBLIC:-0}" = 1 ]; then
  for rights in cleared fallback; do
    ONYX_PUBLIC=1 ONYX_RIGHTS=$rights scripts/build-web.sh
  done
else
  echo "public web gate skipped (ONYX_WEB_PUBLIC=1 runs ONYX_PUBLIC=1 scripts/build-web.sh with ONYX_RIGHTS=cleared, then fallback)"
fi
if [ "${ONYX_WEB:-0}" = 1 ] || [ "${ONYX_WEB_PUBLIC:-0}" = 1 ]; then
  ONYX_PUBLIC=0 ONYX_RIGHTS= scripts/build-web.sh
else
  echo "web gate skipped (ONYX_WEB=1 runs scripts/build-web.sh)"
fi
# Docker images (ROADMAP A3/L7): two release image builds and a smoke run against a local chain
# in containers (docs/ops/deployment.md "Run with Docker"); slow and needs Docker and the reserve
# tail files, so opt-in: ONYX_DOCKER=1 scripts/check-all.sh.
if [ "${ONYX_DOCKER:-0}" = 1 ]; then
  scripts/docker-smoke.sh
else
  echo "docker gate skipped (ONYX_DOCKER=1 runs scripts/docker-smoke.sh)"
fi
echo "ALL CHECKS PASSED"
