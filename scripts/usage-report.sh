#!/usr/bin/env bash
# Usage report of the API node (READINESS A9; docs/spec/usage-counts.md): the most used routes and
# the most opened datasets of the last N days, read from the node's own day files over ssh. Counts
# of requests only: the node keeps nothing about who asked, so this prints nothing about visitors.
#
# Usage: scripts/usage-report.sh [DAYS] [TOP]      DAYS 1-90 (default 7), TOP rows per table (default 20)
#        scripts/usage-report.sh --file REPORT.json [TOP]   print a saved report of `onyx-node usage`
#        scripts/usage-report.sh --self-test       (offline: the tables from a sample report, bad input)
# Env:   ONYX_USAGE_HOST (default ONYX_SITE_HOST, else onyx-web; `local` reads this machine, no ssh
#        and no sudo), ONYX_USAGE_BIN (default /usr/local/bin/onyx-node), ONYX_USAGE_STATE (the
#        node's ONYX_STATE_DIR, default /var/lib/onyx/state).
#
# The node has no route that gives the counts out (THREATS USE-5, review USE-R1 M2). They are read
# from the files in its state directory by `onyx-node usage`, which asks the running node nothing and
# writes nothing. The state directory belongs to the service alone, so on the server the command
# runs under `sudo -n` (the same right the node backup uses). "No usage counts here" means the
# counts are off (ONYX_USAGE=on: ONYX_API_USAGE=1 for scripts/deploy-api.sh). The node writes once a
# minute, so its last minute may be missing. A dataset number counts opens (the dataset's page, or
# the first page of its rows); a dataset under 5 a day is not named (USE-2).
set -euo pipefail
cd "$(dirname "$0")/.."

print_report() { # ANSWER.json TOP
  python3 -I - "$1" "$2" <<'PY'
import json, sys

try:
    with open(sys.argv[1], encoding="utf-8") as f:
        a = json.load(f)
    top = int(sys.argv[2])
    t = a["totals"]
    routes = {k: {c: int(v[c]) for c in ("2xx", "3xx", "4xx", "5xx")} for k, v in t["routes"].items()}
    datasets = {k: int(v) for k, v in t["datasets"].items()}
    other = int(t["datasets_other"])
    days = [d["day"] for d in a["days"]]
    head = (a["from"], a["to"], int(a["small_count"]), list(a["unreadable_days"]))
except (OSError, ValueError, KeyError, TypeError) as e:
    sys.exit(f"usage-report: not a report of onyx-node usage ({type(e).__name__}: {e})")

def clean(s):  # the node's own strings; still never let one move the terminal
    return "".join(c if c.isprintable() else "?" for c in str(s))[:160]

print(f"Usage {clean(head[0])} to {clean(head[1])} (UTC days): {len(days)} day(s) with counts")
if head[3]:
    print("Unreadable day files (skipped): " + ", ".join(clean(d) for d in head[3]))
total = sum(sum(v.values()) for v in routes.values())
print(f"\nRequests: {total}")
print(f"{'requests':>10} {'2xx':>9} {'3xx':>7} {'4xx':>9} {'5xx':>7}  route")
rows = sorted(routes.items(), key=lambda kv: (-sum(kv[1].values()), kv[0]))
for k, v in rows[:top]:
    print(f"{sum(v.values()):>10} {v['2xx']:>9} {v['3xx']:>7} {v['4xx']:>9} {v['5xx']:>7}  {clean(k)}")
if len(rows) > top:
    print(f"{'':>10} ... and {len(rows) - top} more route(s)")
named = sum(datasets.values())
print(f"\nDataset opens (the dataset's page or the first page of its rows): {named + other} ({named} by name, {other} other)")
print(f"{'requests':>10}  dataset (publisher/dataset)")
rows = sorted(datasets.items(), key=lambda kv: (-kv[1], kv[0]))
for k, n in rows[:top]:
    print(f"{n:>10}  {clean(k)}")
if len(rows) > top:
    print(f"{'':>10}  ... and {len(rows) - top} more dataset(s)")
print(f"{other:>10}  other (datasets under {head[2]} a day, or past the daily cap of names)")
print("\nCounts of requests, not of people. The node's last minute may not be written yet.")
PY
}

number() { # VALUE MIN MAX NAME
  [[ $1 =~ ^[0-9]{1,3}$ ]] && [ "$((10#$1))" -ge "$2" ] && [ "$((10#$1))" -le "$3" ] || {
    echo "usage-report: $4 must be $2-$3, not '$1'" >&2
    return 2
  }
}

self_test() {
  local t ok=0 bad=0 out
  t=$(mktemp -d "${TMPDIR:-/tmp}/onyx-usage-report.XXXXXX")
  trap 'rm -rf "$t"' RETURN
  check() { if eval "$2"; then ok=$((ok + 1)); else bad=$((bad + 1)); echo "FAIL: $1" >&2; fi; }
  cat >"$t/a.json" <<'JSON'
{"version":1,"from":"2026-11-10","to":"2026-11-16","small_count":5,"unreadable_days":["2026-11-12"],
 "totals":{"routes":{"GET /v1/health":{"2xx":40,"3xx":0,"4xx":0,"5xx":2},
                     "GET /v1/bulk/{publisher}/{dataset}/rows":{"2xx":90,"3xx":0,"4xx":10,"5xx":0},
                     "GET (no route)":{"2xx":0,"3xx":0,"4xx":7,"5xx":0}},
           "datasets":{"pub.one/set.a":60,"pub.one/set.b":25},"datasets_other":5},
 "days":[{"day":"2026-11-15","routes":{},"datasets":{},"datasets_other":0},
         {"day":"2026-11-16","routes":{},"datasets":{},"datasets_other":0}]}
JSON
  out=$(print_report "$t/a.json" 2)
  check "the header names the days" '[[ $out == *"Usage 2026-11-10 to 2026-11-16 (UTC days): 2 day(s) with counts"* ]]'
  check "an unreadable day is named" '[[ $out == *"Unreadable day files (skipped): 2026-11-12"* ]]'
  check "the request total" '[[ $out == *"Requests: 149"* ]]'
  # pipefail-ok: inside [[ $( ) ]] the status is dropped; the report is a few lines
  check "routes are ranked by requests, with their classes" \
    '[[ $(grep -n "route" <<<"$out" | head -1) ]] && [[ $(grep -c "" <<<"$out") -ge 10 ]] && [[ $(grep -A1 "5xx  route" <<<"$out" | tail -1) == *"100"*"90"*"10"*"GET /v1/bulk/{publisher}/{dataset}/rows" ]]'
  check "TOP cuts the tables and says so" '[[ $out == *"... and 1 more route(s)"* ]] && [[ $out != *"(no route)"* ]]'
  check "datasets are ranked, other is its own line" \
    '[[ $out == *"rows): 90 (85 by name, 5 other)"* ]] && [[ $(grep -A1 "dataset (publisher" <<<"$out" | tail -1) == *"60  pub.one/set.a" ]] && [[ $out == *"5  other (datasets under 5 a day"* ]] && [[ $out == *"not of people"* ]]'
  echo '{"error":{"code":"not_found"}}' >"$t/404.json"
  check "an answer that is not a report fails" '! print_report "$t/404.json" 5 2>/dev/null'
  printf 'not json' >"$t/bad.json"
  check "a broken answer fails" '! print_report "$t/bad.json" 5 2>/dev/null'
  printf '{"version":1,"from":"a","to":"b","small_count":5,"unreadable_days":[],"totals":{"routes":{"GET \\u001b[2J/x":{"2xx":1,"3xx":0,"4xx":0,"5xx":0}},"datasets":{},"datasets_other":0},"days":[]}' >"$t/esc.json"
  out=$(print_report "$t/esc.json" 5)
  check "a control character never reaches the terminal" '[[ $out != *$'"'"'\e'"'"'* ]] && [[ $out == *"GET ?[2J/x"* ]]'
  check "DAYS is 1-90" '! number 0 1 90 DAYS 2>/dev/null && ! number 91 1 90 DAYS 2>/dev/null && ! number 7x 1 90 DAYS 2>/dev/null && ! number "7;id" 1 90 DAYS 2>/dev/null && number 90 1 90 DAYS && number 08 1 90 DAYS'
  local h rc refused=0
  for h in '-oProxyCommand=x' '-v' 'a b' 'a;b' '.x'; do
    rc=0; out=$(ONYX_USAGE_HOST="$h" bash "$0" 7 2>&1) || rc=$?
    [ "$rc" = 2 ] && [[ $out == *"host name or ssh alias"* ]] && refused=$((refused + 1))
  done
  check "a host that starts like an option or is not a plain name is refused before ssh (B2-R1 L3)" '[ "$refused" = 5 ]'
  echo "usage-report self-test: $ok passed, $bad failed"
  [ "$bad" = 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit ;;
  -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  --file)
    [ $# -ge 2 ] || { echo "usage: $0 --file REPORT.json [TOP]" >&2; exit 2; }
    top=${3:-20}; number "$top" 1 999 TOP
    print_report "$2" "$((10#$top))"; exit ;;
esac
days=${1:-7}; top=${2:-20}
number "$days" 1 90 DAYS
number "$top" 1 999 TOP
host=${ONYX_USAGE_HOST:-${ONYX_SITE_HOST:-onyx-web}}
# Review B2-R1 L3: a name that starts with a letter or a digit, so it can never be read as an option.
[[ $host =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || { echo "usage-report: ONYX_USAGE_HOST is a host name or ssh alias, not '$host'" >&2; exit 2; }
bin=${ONYX_USAGE_BIN:-/usr/local/bin/onyx-node}
state=${ONYX_USAGE_STATE:-/var/lib/onyx/state}
for v in "$bin" "$state"; do
  [[ $v =~ ^/[A-Za-z0-9._/-]+$ ]] || { echo "usage-report: ONYX_USAGE_BIN and ONYX_USAGE_STATE are plain absolute paths, not '$v'" >&2; exit 2; }
done
tmp=$(mktemp "${TMPDIR:-/tmp}/onyx-usage-report.XXXXXX")
trap 'rm -f "$tmp"' EXIT
read_counts=("$bin" usage --days "$((10#$days))" --state-dir "$state")
if [ "$host" = local ]; then
  "${read_counts[@]}" >"$tmp" || { echo "usage-report: $bin usage failed (see above)" >&2; exit 1; }
else
  # shellcheck disable=SC2029  # the command is built here on purpose; every part is checked above
  ssh -o BatchMode=yes -- "$host" "sudo -n $(printf '%q ' "${read_counts[@]}")" >"$tmp" \
    || { echo "usage-report: reading the counts on $host failed (see above; it needs sudo -n for $bin)" >&2; exit 1; }
fi
print_report "$tmp" "$((10#$top))"
