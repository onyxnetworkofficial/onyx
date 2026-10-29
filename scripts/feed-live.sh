#!/usr/bin/env bash
# Feed an approved Treasury submission to a LIVE observer round (pre-public loop PP9, PRODUCT-PLAN
# U4): read the approved record date from the private journal, run scripts/level2-observers.sh for
# it (seven observer processes with their own keys fetch api.fiscaldata.treasury.gov themselves,
# on a local anvil registry), then record the round's evidence in the feed ledger beside the
# journal, so the receipt shows it. Stop the node first (the journal is locked while it runs).
#
# Usage: scripts/feed-live.sh JOURNAL SUBMISSION_ID OUT_DIR
# OUT_DIR (new) keeps the round's logs and its results store (`store.json`). Local chain only;
# the date must be one the Treasury has published and within the feed's allowed lag.
set -euo pipefail
cd "$(dirname "$0")/.."
journal=${1:?usage: scripts/feed-live.sh JOURNAL SUBMISSION_ID OUT_DIR}
id=${2:?usage: scripts/feed-live.sh JOURNAL SUBMISSION_ID OUT_DIR}
out=${3:?usage: scripts/feed-live.sh JOURNAL SUBMISSION_ID OUT_DIR}
[ -e "$out" ] && { echo "$out exists; give a new folder" >&2; exit 2; }
target=${CARGO_TARGET_DIR:-$PWD/target}
cargo build -q --locked -p onyx-source-fiscaldata --example feed-approved
date=$("$target/debug/examples/feed-approved" "$journal" "$id" --date)
echo "approved record date: $date"
mkdir -p "$out"
ONYX_L2_RECORD_DATE="$date" ONYX_L2_LOG_DIR="$out" scripts/level2-observers.sh
"$target/debug/examples/feed-approved" "$journal" "$id" --from-store "$out/store.json"
echo "FEED LIVE PASSED: $id, record date $date"
