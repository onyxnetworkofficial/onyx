#!/usr/bin/env bash
# One-command demo of staked submissions and data futures (partner tracker T10, ADR-0042).
# Runs the whole story in a local EVM (Foundry) with the tONYX test token: an honest submission
# re-observed and paid, a duplicate that earns nothing, a wrong value slashed, own data decided by
# the fallback oracle after a contested vote, and a futures market on a future Treasury value.
# Prints a ledger of every actor's net result. No chain, no keys, no network, no real money.
# Env: FOUNDRY_BIN (default ~/.foundry/bin).
set -euo pipefail
cd "$(dirname "$0")/../contracts"
export PATH="${FOUNDRY_BIN:-$HOME/.foundry/bin}:$PATH"
forge test --match-contract OnyxMarketsDemoTest -vv
