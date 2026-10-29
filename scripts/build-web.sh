#!/usr/bin/env bash
# Build the website (ADR-0020): the Rust verifier to WebAssembly, then the
# SvelteKit app to static files in apps/web/build.
# Needs: clang (secp256k1 for wasm32), wasm-bindgen-cli 0.2.129, pnpm.
# Data comes from the demo export (cargo run --release -p onyx-source-btc --example export-demo).
#
# Two modes (AUD-2-F1, docs/ops/deployment.md):
#   default         private preview: ships the recipe inputs (with the test label
#                   rows) and shows a "private preview" banner on every page.
#   ONYX_PUBLIC=1   public: ships no label rows (no recipe inputs; demo.json marked
#                   public), the label-dependent checks use the pinned roots, and the
#                   build is refused if any label address is found in it.
#
# Rights (ROADMAP L14, ADR-0035, THREATS L8-5), for a public build (required with ONYX_PUBLIC=1,
# review L14-R1: unset fails closed; the private preview takes no value and keeps every total):
#   ONYX_RIGHTS=cleared   ships what a public build always shipped, including the company
#                         totals derived from the label sets (flows, reserves, history); only
#                         once those rights are cleared in writing (launch-checklist LC-1 path A).
#   ONYX_RIGHTS=fallback  ships none of those totals and says why on the page; Bitcoin block
#                         data, pool shares, Treasury and SEC stay. The build is refused if any
#                         label-derived total is found in it. Public builds only.
set -euo pipefail
# E10 review: one build at a time per worktree. Each build deletes and rewrites apps/web/build,
# apps/web/static/data and the verifier package, which a second build (or its e2e) would read
# half-written. The lock is this script's own file, so other worktrees build freely.
exec 9<"$0"
flock -n 9 || { echo "another scripts/build-web.sh is running in this worktree; wait for it" >&2; exit 2; }
cd "$(dirname "$0")/.."
export CARGO_INCREMENTAL=0
PUBLIC="${ONYX_PUBLIC:-0}"
# THREATS L9A-6: only 0 or 1. Any other value (e.g. "true") used to build the private preview with
# its label rows, silently and with exit 0, while the operator believed it was public.
case "$PUBLIC" in
  0 | 1) ;;
  *) echo "ONYX_PUBLIC must be 0 (private preview) or 1 (public), not '$PUBLIC'" >&2; exit 2 ;;
esac
RIGHTS="${ONYX_RIGHTS:-}"
# Review L14-R1: a public build must name its rights. The label-set rights are not cleared
# (LC-1), so an unset value must not ship the totals by default.
if [ -z "$RIGHTS" ]; then
  if [ "$PUBLIC" = 1 ]; then
    echo "ONYX_PUBLIC=1 needs ONYX_RIGHTS: fallback (no label-derived totals; LC-1 path B) or cleared (only once the label-set rights are cleared in writing, LC-1 path A)" >&2
    exit 2
  fi
  RIGHTS=cleared
fi
# Same strictness as ONYX_PUBLIC (L9A-6): a typo must not build the totals silently.
case "$RIGHTS" in
  cleared | fallback) ;;
  *) echo "ONYX_RIGHTS must be cleared or fallback, not '$RIGHTS'" >&2; exit 2 ;;
esac
if [ "$RIGHTS" = fallback ] && [ "$PUBLIC" != 1 ]; then
  echo "ONYX_RIGHTS=fallback is for a public build: set ONYX_PUBLIC=1 too (the private preview keeps every total)" >&2
  exit 2
fi
# SIG-R1-F2: a public build's verifier holds no level-2 result pins.
VERIFY_FEATURES=()
[ "$PUBLIC" = 1 ] && VERIFY_FEATURES=(--features public)
# Anonymity: the verifier's panic messages and the C code's file names hold source paths. Map the
# builder's home folder to /build, as scripts/build-recipes.sh maps its own (checked below).
RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build" \
  CFLAGS_wasm32_unknown_unknown="${CFLAGS_wasm32_unknown_unknown:-} -ffile-prefix-map=$HOME=/build" \
  cargo build --locked --profile web -p onyx-web-verify --target wasm32-unknown-unknown "${VERIFY_FEATURES[@]}"
wasm-bindgen --target web --out-dir apps/web/src/lib/pkg \
  "${CARGO_TARGET_DIR:-target}"/wasm32-unknown-unknown/web/onyx_web_verify.wasm
[ -f apps/demo/site/data/demo.json ] || { echo "no demo data: run the export-demo example first"; exit 1; }
rm -rf apps/web/static/data apps/web/build
mkdir -p apps/web/static/data
cp apps/demo/site/data/* apps/web/static/data/
# The fee-split cases Rust and the contract both pass (ADR-0025), for the Run page.
cp fixtures/revenue/vectors.json apps/web/static/data/revenue-vectors.json
# ROADMAP D30 (PP3): the synthetic salted label rows the Labels page checks.
cp fixtures/dated/salted-vectors.json apps/web/static/data/salted-vectors.json
cp fixtures/revenue/contributor-policy.json apps/web/static/data/contributor-policy.json
cp fixtures/revenue/pricing-defaults.json apps/web/static/data/pricing-defaults.json
# ADR-0043: the on-chain section reruns btc-chain-stats in the browser; the recipe is the
# committed reproducible build (its Keccak is the recipe id the verifier pins).
cp recipes/out/btc_chain_stats.wasm apps/web/static/data/chain-stats.wasm
# The pools section names pools from the MIT-licensed bitcoin-data/mining-pools
# list: its notice ships with the site (L8-F2, fixtures/pools/PROVENANCE.md).
cp fixtures/pools/LICENSE-mining-pools.txt apps/web/static/data/LICENSE-mining-pools.txt
# ADR-0044: the Ethereum section reruns eth-chain-stats in the browser; the recipe is the
# committed reproducible build (its Keccak is the recipe id the verifier pins).
cp recipes/out/eth_chain_stats.wasm apps/web/static/data/eth-chain-stats.wasm
if [ "$PUBLIC" = 1 ]; then
  # The recipe inputs hold the label rows: drop them and say so in demo.json.
  rm -f apps/web/static/data/input-*.b64.txt
  python3 -I - apps/web/static/data/demo.json <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["public"] = True
for r in d["runs"]:
    r["input"] = None
json.dump(d, open(p, "w"), indent=2)
open(p, "a").write("\n")
PY
  export VITE_ONYX_PUBLIC=1
else
  export VITE_ONYX_PUBLIC=0
fi
# PP11: each dataset card's newest date, from the data files just assembled above.
python3 -I scripts/gen-freshness.py >/dev/null
if [ "$RIGHTS" = fallback ]; then
  # ROADMAP L14: no label-derived total in demo.json. The run and live rows (company flows,
  # one Borsh leaf each), the per-block and series rows, the label-set spread and the
  # recount lines go; roots, commitments, blocks and the label set's hash and count stay.
  python3 -I - apps/web/static/data/demo.json <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["rights"] = "fallback"
d.pop("spread", None)
for r in d["runs"]:
    r["rows"] = []
    r["proofs"] = []
live = d.get("live")
if live:
    live["rows"] = []
    live["python_recount"] = "held back (ONYX_RIGHTS=fallback)"
    live.pop("per_block", None)
    live.pop("series", None)
json.dump(d, open(p, "w"), indent=2)
open(p, "a").write("\n")
PY
fi
export VITE_ONYX_RIGHTS="$RIGHTS"
(
  cd apps/web
  pnpm install --frozen-lockfile
  # OSS-12: the site bundles Lucide icons (ISC; some derived from Feather, MIT). Their notices
  # ship verbatim as static/licenses/LICENSE-lucide.txt; refuse a build whose installed package
  # licence differs from the shipped copy (e.g. after an upgrade), so the notice never goes stale.
  cmp -s node_modules/@lucide/svelte/LICENSE static/licenses/LICENSE-lucide.txt || {
    echo "apps/web/static/licenses/LICENSE-lucide.txt differs from node_modules/@lucide/svelte/LICENSE: copy the installed licence verbatim" >&2
    exit 1
  }
  pnpm check
  pnpm build
)
if [ "$PUBLIC" = 1 ]; then
  # ATT-R2-F1: a public build ships a paid level-2 round sealed, as the free
  # API seals a fact that is not closed: no value, reveal, salt, status or
  # page, and no result hash (SIG-R1-F2: it is an unsalted function of the
  # value), only the signed commits (the paid API sells the value; the browser
  # still checks the draw, round id and signatures). Same
  # transform as onyx_api::observations::seal_site_round.
  # THREATS NS5-4, NS5-8, ADR-0048 amendment 1 (the owner's choice, 2026-10-27): values of free
  # public sources (Treasury Fiscal Data, USGS, NCEI) are free to read and ship unsealed, with
  # their level history (their on-chain result hash is unsalted, so a seal would not hold);
  # only the SEC holdings stay sealed.
  python3 -I - apps/web/build/level2/sec-holdings.json <<'PY'
import json, sys
def seal(r):
    commits = [{"observer": e["observer"], "commit": e["commit"], "commit_sig": e["commit_sig"]} for e in r["reveals"]]
    for k in ("reveals", "statuses", "agreed", "response_body", "result_hash"):
        r.pop(k, None)
    r.update(value="", sealed=True, commits=commits)
for p in sys.argv[1:]:
    d = json.load(open(p))
    for r in d["companies"] if "companies" in d else [d]:
        seal(r)
    json.dump(d, open(p, "w"), indent=2)
    open(p, "a").write("\n")
PY
fi
if [ "$RIGHTS" = fallback ]; then
  # ROADMAP L14: the per-entity flow history and the reserve history are label-derived
  # totals throughout; the run records lose their OKX figures (the steps keep what was done).
  rm -rf apps/web/build/history/week apps/web/build/history/reserves-week.json \
    apps/web/build/history/rosters-week.json
  python3 -I - apps/web/build/run/*.json <<'PY'
import json, re, sys
BTC = re.compile(r"[-+\u2212]?\d[\d,]*\.\d{8} BTC")
def scrub(x):
    if isinstance(x, dict):
        return {k: scrub(v) for k, v in x.items()}
    if isinstance(x, list):
        return [scrub(v) for v in x]
    if isinstance(x, str):
        return BTC.sub("[company total held back]", x)
    return x
for p in sys.argv[1:]:
    d = json.load(open(p))
    r = d.get("round") or {}
    for k in ("okx", "okx_net_btc"):
        if k in r:
            r[k] = None
    if d.get("reserve"):
        d["reserve"]["okx_btc"] = None
        # The forged base's planted extra (a test constant, not label-derived) sits beside the
        # company fields, where the guard's rule 4 cannot tell it from a total; no page reads
        # it, and the step lines already lose it with every other BTC amount.
        if "forged_extra_btc" in d["reserve"]:
            d["reserve"]["forged_extra_btc"] = None
    json.dump(scrub(d), open(p, "w"), indent=2)
    open(p, "a").write("\n")
PY
fi
# W4: the home page's proof card reads a few fields of each full run (verdict, network, date,
# transactions, whether a reserve row was sold); run/heads.json carries just those, copied from
# the run files the site lists (src/lib/runs.ts), so the home page skips 3 x 45-58 KB files.
python3 -I - apps/web/src/lib/runs.ts apps/web/build <<'PY'
import json, pathlib, re, sys
src = pathlib.Path(sys.argv[1]).read_text()
files = re.findall(r"'(run/base-sepolia-[0-9-]+\.json)'", src.split("TESTNET_RUNS", 1)[1].split(";", 1)[0])
if not files:
    sys.exit("no run files listed in src/lib/runs.ts")
heads = []
for f in files:
    d = json.loads((pathlib.Path(sys.argv[2]) / f).read_text())
    h = {"file": f, "verdict": d["verdict"], "network": d["network"], "date": d["date"], "totals": {"sent_txs": d["totals"]["sent_txs"]}}
    if d.get("reserve"):
        h["reserve"] = {"round": d["reserve"]["round"]}
    heads.append(h)
(pathlib.Path(sys.argv[2]) / "run/heads.json").write_text(json.dumps(heads, separators=(",", ":")) + "\n")
PY
# W4: ship the data files without indentation. Only whitespace outside strings is
# removed (numbers and strings stay byte for byte); the step refuses any file whose
# parsed value would change.
python3 -I - apps/web/build <<'PY'
import decimal, json, pathlib, sys
saved = 0
for p in sorted(pathlib.Path(sys.argv[1]).rglob("*.json")):
    if "app" in p.relative_to(sys.argv[1]).parts[:1]:
        continue  # Vite's own files
    text = p.read_text(encoding="utf-8")
    out, in_str, esc = [], False, False
    for ch in text:
        if in_str:
            out.append(ch)
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
            out.append(ch)
        elif ch not in " \t\r\n":
            out.append(ch)
    small = "".join(out)
    exact = lambda s: json.loads(s, parse_float=decimal.Decimal, parse_int=decimal.Decimal)
    if exact(small) != exact(text):
        sys.exit(f"minifying changed the value of {p}")
    if len(small) < len(text):
        saved += len(text.encode()) - len(small.encode())
        p.write_text(small, encoding="utf-8")
print(f"data files minified: {saved} bytes saved")
PY
# Anonymity: no file the site ships names the builder's home folder (and so the account name).
home_hits=$(grep -rlaF "$HOME" apps/web/build || true)
[ -z "$home_hits" ] || { echo "the build holds the builder's home folder path: $home_hits" >&2; exit 1; }
# No person's coinbase handle in anything the site ships (AUD-2-F7).
ONYX_SCAN_DIRS=apps/web/build:apps/web/static/data \
  cargo test --locked -q -p onyx-source-btc --test pool_review the_built_site_carries_no_persons_handle
if [ "$PUBLIC" = 1 ]; then
  # Refuse a public build that carries any label row (AUD-2-F1).
  ONYX_PUBLIC_BUILD_DIRS=apps/web/build:apps/web/static/data \
    cargo test --locked -q -p onyx-source-btc --test public_build a_public_build_carries_no_label_rows
  # Refuse a public build that ships a level-2 value or its result hash (SIG-R1-F2).
  ONYX_PUBLIC_BUILD_DIRS=apps/web/build \
    cargo test --locked -q -p onyx-api --test att_r1_attempts a_public_build_ships_no_level2_value_or_result_hash
fi
if [ "$RIGHTS" = fallback ]; then
  # Refuse a fallback build that carries any label-derived total (ROADMAP L14, ADR-0035).
  ONYX_FALLBACK_BUILD_DIRS=apps/web/build:apps/web/static/data \
    cargo test --locked -q -p onyx-source-btc --test public_build a_fallback_build_carries_no_label_derived_totals
fi
(cd apps/web && pnpm test:e2e)
if [ "$RIGHTS" = fallback ]; then echo "website built (public, rights fallback: no label rows, no label-derived totals): apps/web/build"
elif [ "$PUBLIC" = 1 ]; then echo "website built (public, no label rows): apps/web/build"
else echo "website built (private preview): apps/web/build"; fi
