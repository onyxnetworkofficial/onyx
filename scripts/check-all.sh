#!/usr/bin/env bash
# Local mirror of CI. Run before every commit.
# Much faster with TMPDIR on a RAM disk (tmpfs), because the tests sync files to disk often: the
# onyx-api unit tests took 188 s with TMPDIR on disk and 26 s on tmpfs (2026-11-01). Not /dev/shm:
# the remote-check self-test needs TMPDIR outside what its sandbox may write.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$PATH:$HOME/.foundry/bin"
# No silent end: a step that fails without a line of its own is still named here, as the last
# line of the log (the public CI once ended in a bare `exit code 1` after a step that printed nothing).
trap 'st=$?; echo "check-all: FAILED (exit $st) at line $LINENO: $BASH_COMMAND"' ERR
# Each stage prints how long it took (`[time]` lines), so the slowest one is easy to find.
t_all=$SECONDS
t_stage=$SECONDS
stage() { echo "[time] $1: $((SECONDS - t_stage)) s (total $((SECONDS - t_all)) s)"; t_stage=$SECONDS; }
# Independent offline tests run side by side (each has its own temp folder and free loopback
# ports). Each runs in its own process group with a time limit, its whole output is printed in
# order when all end, and any failure fails the check; on an early exit (Ctrl-C included) the
# EXIT trap stops every group. Timing-sensitive tests (the smoke lock) stay outside.
sbs_pids=()
sbs_dir=
side_by_side() {
  local i st rc=0 cmds=("$@")
  sbs_dir=$(mktemp -d)
  sbs_pids=()
  echo "side by side (logs in $sbs_dir until all end):"
  for i in "${!cmds[@]}"; do
    echo "  running: ${cmds[i]}"
    setsid timeout 900 bash -c "${cmds[i]}" >"$sbs_dir/$i.log" 2>&1 &
    sbs_pids+=("$!")
  done
  for i in "${!sbs_pids[@]}"; do
    if wait "${sbs_pids[i]}"; then
      cat "$sbs_dir/$i.log"
    else
      st=$?
      cat "$sbs_dir/$i.log"
      [ "$st" = 124 ] && echo "TIMED OUT after 900 s: ${cmds[i]}" || echo "FAILED (exit $st): ${cmds[i]}"
      rc=1
    fi
  done
  sbs_pids=()
  rm -rf "$sbs_dir"
  sbs_dir=
  return "$rc"
}
# The gas regression check (see below, where its result is read) rebuilds every contract, about
# five minutes on one core: it starts now and runs beside the Rust stages. The gas profile has its
# own out-gas and cache-gas folders, so the default-profile builds in between never touch it.
gas_log=$(mktemp)
(cd contracts && exec env FOUNDRY_PROFILE=gas forge snapshot --check --force --no-match-test 'testFuzz_|invariant_' --no-match-contract 'Fork') >"$gas_log" 2>&1 &
gas_pid=$!
# On an early exit: stop it and its solc (by PID, so a rerun's --force never meets a leftover build).
# (and every side_by_side group still running, below).
trap '[ -z "$gas_pid" ] || { pkill -P "$gas_pid"; kill "$gas_pid"; } 2>/dev/null || true; rm -f "$gas_log"
for p in ${sbs_pids[@]+"${sbs_pids[@]}"}; do kill -TERM -- "-$p" 2>/dev/null || true; done
[ -z "${sbs_dir:-}" ] || rm -rf "$sbs_dir"' EXIT
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
stage "recipes, fmt, clippy"
# `cargo test --locked --workspace`, with the test binaries run in parallel (scripts/test-workspace.py:
# the same tests, from each package's folder; about 690 s one after another on 2026-11-02).
python3 -I scripts/test-workspace.py
stage "rust tests"
# ROADMAP C50: the time-lock vectors on wasm32 (blst's C built by clang, run in node).
scripts/check-tlock-wasm.sh
# CL3 (THREATS CL3-3): the generic JSON fetcher's shared vectors on wasm32, as natively.
scripts/check-json-wasm.sh
# NS8 (ADR-0047): the agent toolkit's tests, then an MCP session against two demo servers. First
# its self-test: the test counts are read from TAP, and every failing path prints its step and log.
scripts/agent-smoke.sh --self-test
scripts/agent-smoke.sh
# CL2: the contributor client (apps/contrib) with a fake provider and a fake intake on loopback:
# the API key never reaches Onyx, a tool result, a log line or a stored file (THREATS CL2).
node --test apps/contrib/test/*.test.mjs
# CL10 (ADR-0061, THREATS CL10): the contributor app server (token, Host and Origin gates, no key in
# any answer) and the single-file bundle run in the line above; here the desktop workflow's rules
# (every action pinned by SHA, only the release job writes) and the Download page's release helper.
# THREATS OPS-DEPLOY-1: what the deploy scripts install as root ends up owned by the node's user: no
# chown that takes its reference from the path it fixes, no root rsync keeping the sender's owner.
python3 -I scripts/check-deploy-owner.py --self-test
python3 -I scripts/check-deploy-owner.py
# THREATS OPS-PIPE-1: no pipe into a reader that stops early (grep -q, head, sed q, awk exit) where
# pipefail is on: the writer can die of SIGPIPE and a check is then skipped in some runs.
python3 -I scripts/check-pipefail-grep.py --self-test
python3 -I scripts/check-pipefail-grep.py
python3 -I scripts/check-workflows.py --self-test
python3 -I scripts/check-workflows.py
# ADR-0066 item 9 (THREATS PUBON-14): the public record of publisher decisions (deploy/publishers/):
# every approved application's signature, what a refusal may keep, no contact text, and the status
# list the site and `onyx-publish doctor` read. A late decision is a printed warning, never a failure.
python3 -I scripts/check-publishers.py --self-test
python3 -I scripts/check-publishers.py
node --test apps/web/tests/release.test.mjs
# The site's shared formatters print what the per-call toLocale* forms printed (W4, L5).
node --test apps/web/tests/format.test.mjs
node --test apps/web/tests/datasets.test.mjs
# ROADMAP S27 (ADR-0066 slice 4): the publish page's status list, lookup and commands.
node --test apps/web/tests/publishers.test.mjs
node --test apps/web/tests/glossary.test.mjs
stage "wasm vectors, agent, contrib, workflows"
# The service binary (docs/ops/deployment.md) builds and parses its arguments.
# (--workspace with an example: the feature set of the workspace test build above, dev
# dependencies included, so nothing compiles twice; the rehearsals below reuse the same build.)
cargo build --locked -q --workspace --bin onyx-node --bin onyx-node-btc --example pin-signed-book
"${CARGO_TARGET_DIR:-$PWD/target}/debug/onyx-node" --help >/dev/null
# The Bitcoin build with the reserve serving check (ADR-0034 Amendment 1).
"${CARGO_TARGET_DIR:-$PWD/target}/debug/onyx-node-btc" --help >/dev/null
stage "debug binaries"
# ROADMAP C5 (ADR-0036 amendment 1): the track-record intake against a local anvil on a free
# loopback port: posts, chain-time `committed_at`, sealing, the client recheck and five attacks.
scripts/track-intake-anvil.sh
stage "track-intake-anvil"
# ROADMAP C7 (ADR-0036 amendment 3): resolutions from OnyxSignedBook facts only and commitments in
# its append-only log, against a local anvil: the client recheck and eight attacks (incl. a reorg).
scripts/track-onchain-anvil.sh
stage "track-onchain-anvil"
scripts/prepaid-anvil.sh
stage "prepaid-anvil"
# CL6 (ADR-0056, THREATS CL6): the credits ledger on a local anvil: an observer with no ETH deposits
# and posts its commit and receipt through the relayer, paid from credits; a miner's earned share
# moved in; forged, repeated and over-cap relays refused; the fees paid to the relayer.
scripts/credits-anvil.sh
stage "credits-anvil"
# CL7 (ADR-0057): a correction market end to end on anvil (about 30 s once contracts are built):
# the live manifest and its recheck registered with their real template ids, a value, a market,
# bets, a recheck that disagrees, settlement and payouts to the wei; a forged value, a market after the
# recheck, an insider bet and the original round as the recheck all refused.
scripts/corrections-anvil.sh
stage "corrections-anvil"
# CL7 (ADR-0057 item 7): the recheck runner end to end on a local anvil: a real original round, a
# market and bets (one by a pool member), the plan (wait, then run), a real recheck round from the
# plan's pool without the original observers and the bettor, resolve and the payout.
scripts/recheck-anvil.sh
stage "recheck-anvil"
# ROADMAP C10 (ADR-0028 amendment 7): OnyxSignedBook deployed and pinned with the tooling the lead
# runs live (reviewer == deployer refused), onyx-node refusing broken pins and serving level 3 only
# against `settleable` at its finalized block, coordinator posts, censorship hold, clock skew.
scripts/level3-book.sh local
stage "level3-book"
# CL5 (ADR-0059): a publisher's dataset end to end on anvil (synthetic rows, a fresh test key):
# build, sign, anchor in OnyxSignedBook, upload, proven range reads checked by onyx-check --bulk,
# and the attacks (forged row and shard, unanchored swap, unknown key, compromised key and its
# recovery). Small here; `scripts/bulk-demo.sh local` alone runs the 100 MiB rehearsal.
BULK_DEMO_MIB=6 BULK_DEMO_SHARD_MIB=1 scripts/bulk-demo.sh local
stage "bulk-demo"
# ADR-0060 (docs/STORAGE-NETWORK.md): the storage network on anvil (about 2 min once built): five
# bonded storage nodes, R=3, every node fetches and checks its shards, an epoch of proofs all
# passed and paid, reads that fail over past a killed node, a node that dropped its shards and a
# forged row slashed, misses settled by a keeper, repair onto new replicas, conservation on chain.
scripts/storage-network-anvil.sh
stage "storage-network-anvil"
# ROADMAP S26 slice 3 (ADR-0065 design test 10, THREATS EARLY): the early-contributor record on anvil
# (about 30 s once built): one outside address's accepted work of four kinds, a forged proof and a
# losing bet give exactly its rows; every role key and the record's own key excluded; bulk versions
# 1 and 2 under the record's own publisher id; verify gives the same bytes and shard roots; the
# daily run's dry run with the real tools gives the same bytes from a fresh state.
scripts/early-record-anvil.sh
stage "early-record-anvil"
# ROADMAP S27 (ADR-0066 slice 3, THREATS PUBON): publisher onboarding on anvil (about 20 s once
# built): a fresh applicant applies and `doctor` passes; the reviewer verifies with the book read
# from the chain, a dry run sends nothing, the limit is written before registerKey, the sample's
# report is relayed in the same run; the node serves the sample and onyx-check proves it; a refusal
# blocks its signer; a second key needs a cosignature; a withdraw is taken once; a paused
# publisher's tombstone is taken.
scripts/publisher-onboard-anvil.sh
stage "publisher-onboard-anvil"
scripts/check-boundaries.sh
scripts/check-docs.sh
# Secret scan of the tracked tree, after its own planted-key self-test (THREATS T-KEY-1).
scripts/check-secrets.sh --self-test
scripts/check-secrets.sh
# Side by side (about 25 s, 70 s one after another; 2026-11-02):
# - ROADMAP L12: the health watch against fake node/RPC/site servers on 127.0.0.1 (every threshold
#   edge, malformed answers, RPC down, stalled, wrong chain, de-duplication; no outside network).
# - ADR-0043 (THREATS CHAIN-6): the on-chain refresh against a fake Bitcoin Core REST server on
#   127.0.0.1 (synthetic blocks): what it writes, the lies it refuses, and the pin writer.
# - THREATS STORAGE-S16-R8: scripts/storage-sepolia.sh with a fake cast (no network, no key): a chain
#   read that fails stops the step; it never reads as "nothing on chain" and sends a second bond or deposit.
# - ROADMAP E13 (THREATS REMOTE-1..10): the remote check's refusals without the host: arguments,
#   secrets, credential files and never-synced paths in the file list, symlinks and the real rsync
#   into a local folder, an unreachable host (255), check-all's status passed back unchanged even
#   when the script is rewritten mid-run, the recipes/out and submodule gates, the host's
#   load and disk rules, two build slots, --clean deleting only the branch's two folders, and the
#   job's Landlock rules.
# - ADR-0044 (THREATS ETH-7): the Ethereum window fetch against fake JSON-RPC servers on 127.0.0.1
#   serving the committed mainnet blocks: what it writes, the lies it refuses, the rate and the pins.
# - ROADMAP S26 (THREATS EARLY-4, EARLY-9, EARLY-12, EARLY-19): the early-contributor record's daily
#   run with stub tools: verify on the second node and the privacy scan gate publication, one
#   catalog a day, a rerun resumes and sends nothing twice, no node URL or key path printed.
# - THREATS OPS-BACKUP-1, OPS-BACKUP-2: the API node's backup against a local fake server (no ssh,
#   about 40 s): files 0600 and folders 0700, no key file copied, an unchanged file hard-linked to the
#   previous snapshot, the newest N kept, a dropped connection tried again, a partial copy continued,
#   a tiers.json naming a missing shard refused, a DEST that is not 0700 or sits in a git checkout
#   refused, no planted secret printed; review BAK-R1: an emptied or shrunken store refused (exit 4)
#   and nothing pruned, weekly copies kept, a store still changing or a failed drill no snapshot, the
#   server binary and unit copied, a deploy during the copy refused, a clock going back refused, the
#   copy synced before its rename, old drill folders deleted, --check-fresh and the alert units.
# - THREATS BAK-R1 (restore drill, offline, about 2 s): keccak-256 vectors, the state journal reader
#   (a damaged line or a wrong header fails, a torn last line passes, nothing printed), the
#   allow-listed local config, the node's settings passed as environment with a fixed systemd-run
#   description, the backup folder's lock.
# - READINESS A22 (docs/ops/deployment.md "Rebuild a lost deploy export", offline, about 1 s): a deploy
#   export rebuilt from a fake snapshot of the node backup and checked by deploy-api.sh's own
#   staging lines; no host line and no value printed; an existing target, a snapshot missing a
#   part, a non-testnet node and staging lines that name ssh, rsync, sudo or systemctl refused.
# - READINESS B2 (docs/ops/proposal-review.md, offline, under 1 s): the proposal reviewer's script
#   against a fake ssh, sudo and onyx-node: the command line it sends, every wrong argument refused
#   before anything is sent, each exit code with its line.
side_by_side \
  "python3 -I scripts/health-watch-tests.py" \
  "python3 -I scripts/refresh-onchain-tests.py" \
  "scripts/storage-sepolia-tests.sh" \
  "scripts/remote-check.sh --self-test" \
  "python3 -I scripts/refresh-eth-onchain-tests.py" \
  "python3 -I scripts/early-record-daily-tests.py" \
  "scripts/backup-node.sh --self-test" \
  "scripts/restore-drill.sh --self-test" \
  "scripts/export-from-backup.sh --self-test" \
  "scripts/deploy-node-gate.sh --self-test" \
  "scripts/usage-report.sh --self-test" \
  "scripts/proposal-review.sh --self-test"
# THREATS P3A-R1-5: an independent Python recount of the shipped block input (all 22 fields of
# block 970,835 from its ChainSource) equals its committed leaf; planted changes are caught.
python3 -I scripts/recount-chain-input.py --self-test
# The machine-wide lock of scripts/docker-smoke.sh (no Docker, about 14 s): two holders never hold
# it at once, a held lock times out cleanly and names its holder, a leaked child cannot keep it.
scripts/smoke-lock-tests.sh
# THREATS L9A-7: the run-page indexer reader keeps only scheme://host[:port] of its RPC URL.
python3 -I scripts/read-indexer-sepolia.py --self-test
# THREATS ETF-5: the SEC ETF recorder paces requests within SEC's 10 per second, fetches only
# data.sec.gov, follows no redirect, names no person in its User-Agent (offline, no request sent).
python3 -I scripts/record-sec-etf-holdings.py --self-test
stage "script tests"
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
stage "build-web refusals, dependencies"
# `--sizes` fails if any contract exceeds the EIP-170 runtime limit (24,576 B), which only a real
# deploy would otherwise reveal (M6-L36-F1). Test-only contracts (the L35 `Harness` extends the
# settlement contract) never deploy on chain, so they are skipped (M5-L46-F3).
(cd contracts && forge fmt --check && forge build --sizes --skip test && forge test)
stage "forge build and test"
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
# stale gas that was committed twice (C2-R2, then C4; lead fix 2026-10-17). Started at the top.
if ! wait "$gas_pid"; then
  gas_pid=; cat "$gas_log"; echo "gas regression: contracts/.gas-snapshot does not match (or the build failed)"; exit 1
fi
gas_pid=
cat "$gas_log"
stage "storage layouts, gas snapshot (waited)"
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
stage "web"
# Docker images (ROADMAP A3/L7): two release image builds and a smoke run against a local chain
# in containers (docs/ops/deployment.md "Run with Docker"); slow and needs Docker, the reserve
# tail files and Caddy (ONYX_CADDY, or caddy on PATH: step 5, behind Caddy), so opt-in:
# ONYX_DOCKER=1 scripts/check-all.sh.
if [ "${ONYX_DOCKER:-0}" = 1 ]; then
  scripts/docker-smoke.sh
else
  echo "docker gate skipped (ONYX_DOCKER=1 runs scripts/docker-smoke.sh)"
fi
stage "docker"
echo "ALL CHECKS PASSED"
