#!/usr/bin/env bash
# Fails if any core crate (crates/*) depends, directly or transitively, on a
# sector adapter (adapters/*) or a recipe (recipes/*). See ADR-0010.
set -euo pipefail
cd "$(dirname "$0")/.."
meta=$(cargo metadata --format-version 1 --no-deps)
sector=$(jq -r '.packages[] | select(.manifest_path | test("/(adapters|recipes)/")) | .name' <<<"$meta")
core=$(jq -r '.packages[] | select(.manifest_path | test("/crates/")) | .name' <<<"$meta")
fail=0
for c in $core; do
  deps=$(cargo tree -p "$c" --edges normal,build --prefix none --format '{p}' 2>/dev/null | awk '{print $1}' | sort -u)
  for s in $sector; do
    if grep -qx "$s" <<<"$deps"; then
      echo "BOUNDARY VIOLATION: core crate $c depends on sector crate $s"
      fail=1
    fi
  done
done
# SER-R2-F1: `unchecked-open` (Round::open_unchecked, no preflight) is defined
# by onyx-coordinator for settlement-path tests only; no manifest may enable it.
# Only tracked manifests count (AUD-1-F2): agent worktrees under .claude/ and build
# trees are not part of this checkout.
if git ls-files -z '*Cargo.toml' | xargs -0 grep -l 'unchecked-open' \
    | grep -v '^crates/onyx-coordinator/Cargo.toml$'; then
  echo "BOUNDARY VIOLATION: a manifest enables onyx-coordinator's unchecked-open feature"
  fail=1
fi
# OPS-7 (L9 sweep): every base image in a tracked Dockerfile is pinned by digest, so a tag that
# moves upstream never changes a build; bumps are deliberate (docs/ops/upgrades.md).
while IFS= read -r f; do
  if grep -nE '^[[:space:]]*FROM[[:space:]]' "$f" | grep -vE '@sha256:[0-9a-f]{64}([[:space:]]|$)'; then
    echo "UNPINNED BASE IMAGE in $f (pin FROM by @sha256 digest)"
    fail=1
  fi
  # L7-BK-1: BuildKit pulls and runs the `# syntax=` frontend image; pin it the same way.
  if grep -niE '^#[[:space:]]*syntax[[:space:]]*=' "$f" | grep -vE '@sha256:[0-9a-f]{64}[[:space:]]*$'; then
    echo "UNPINNED SYNTAX FRONTEND in $f (pin # syntax= by @sha256 digest)"
    fail=1
  fi
done < <(git ls-files | grep -E '(^|/)Dockerfile[^/]*$')
# ADR-0036 item 12 (THREATS TR-11, TR-R1-8): a track record never raises a proof level or
# reaches money. onyx-track returns no level or market policy; no other tracked Rust file that uses
# onyx_track (or onyx-coordinator's `track_chain`, which re-exports its types) may touch level or
# market-policy code; and only the manifests listed in TRACK_DEPENDENTS may depend on onyx-track,
# under any name: onyx-coordinator for the intake's chain record (ADR-0036 amendment 1 item 11),
# onyx-api to serve records and onyx-web-verify to check them in the browser (ROADMAP C6), and
# onyx-source-ncei for its NWS forecaster example only, a dev-dependency (north-star loop NL4).
# The return-type check covers every onyx-track source file but the tests (which build resolutions).
TRACK_DEPENDENTS="crates/onyx-coordinator/Cargo.toml crates/onyx-api/Cargo.toml apps/web/verify/Cargo.toml \
adapters/source-ncei/Cargo.toml"
# The names that reach track code: the crate, and each dependent's one track module (ROADMAP C6:
# onyx-api's `track_serve`, the browser verifier's `track_view`).
TRACK_NAMES='onyx_track|track_chain|track_serve|track_view'
# ROADMAP C6 (THREATS TR-S-9): the same for onyx-api (`track_serve`: the store and routes; its tests)
# and the browser verifier (`track_view`). Each module root is `file:module` and may name its module
# only as `pub mod <module>;`. onyx-api's lib.rs takes the routes as an opaque Router
# (`router_with`), so no level, pricing or settlement file names them. The node mounts them from
# app.rs (which also builds payments) on exactly one line, `onyx_api::track_serve::node_routes(..)`:
# a constructor that takes the rights and two paths (ROADMAP C20: the coordinator's publication
# file and the state directory for its served head), never data, and returns only a Router, so app.rs holds no handle to a record
# (review TR-S-R1-2: app.rs as a TRACK_FILE could have read a record beside its payment code).
TRACK_FILES="crates/onyx-coordinator/src/track_chain.rs crates/onyx-coordinator/examples/track-intake-anvil.rs \
crates/onyx-coordinator/examples/track-onchain-anvil.rs \
adapters/source-ncei/examples/track-nws.rs adapters/source-ncei/tests/track_nws.rs \
crates/onyx-api/src/track_serve.rs crates/onyx-api/src/track_serve_tests.rs apps/web/verify/src/track_view.rs \
crates/onyx-node/tests/caddyfile.rs"
TRACK_MOUNTS="crates/onyx-node/src/app.rs"
TRACK_MOD_ROOTS="crates/onyx-coordinator/src/lib.rs:track_chain crates/onyx-api/src/lib.rs:track_serve \
apps/web/verify/src/lib.rs:track_view"
# A module root names its track module only as `pub mod <module>;` (one line, nothing else of
# track code): it may hold level code, since it cannot reach the module's items without naming them.
mod_root_ok() {
  local r m
  for r in $TRACK_MOD_ROOTS; do
    m=${r#*:}
    if [ "$1" = "${r%%:*}" ] && [ "$(grep -cE "\\b($TRACK_NAMES)\\b" "$1")" = 1 ] \
      && [ "$(grep -cE "\\b$m\\b" "$1")" = 1 ] && grep -qxE "pub mod $m;" "$1"; then
      return 0
    fi
  done
  return 1
}
mount_ok() {
  case " $TRACK_MOUNTS " in *" $1 "*) ;; *) return 1 ;; esac
  [ "$(grep -cE "\\b($TRACK_NAMES)\\b" "$1")" = 1 ] \
    && grep -qE '^[[:space:]]*let [a-z_]+ = onyx_api::track_serve::node_routes\([a-z_]+, [a-z_]+, [a-z_]+\);[[:space:]]*$' "$1"
}
while IFS= read -r f; do
  if grep -nE -- '->[^{;]*\b(ProofLevel|MarketPolicy)\b|MarketPolicy::' "$f"; then
    echo "TRACK-LEVEL FENCE: $f returns or builds a level or market policy (ADR-0036 item 12, TR-11)"
    fail=1
  fi
done < <(git ls-files 'crates/onyx-track/src/*.rs' | grep -vE '(^|/)(tests|[a-z_]+_tests)\.rs$')
while IFS= read -r f; do
  mod_root_ok "$f" && continue
  mount_ok "$f" && continue
  if grep -nE 'ProofLevel|proof_level|MarketPolicy|with_attested_level|with_label_set_level|verifyRowLeveled|accepted_level' "$f"; then
    echo "TRACK-LEVEL FENCE: $f uses onyx_track and level or market-policy code (ADR-0036 item 12, TR-11)"
    fail=1
  fi
done < <(git ls-files '*.rs' | grep -v '^crates/onyx-track/' | xargs -r grep -lE "$TRACK_NAMES" || true)
# Review TR-R4-2: TRACK_DEPENDENTS lets onyx-coordinator (which settles level-2 payouts) link
# onyx-track, so the file fence alone let `pub use onyx_track as trk;` in its lib.rs reach a level
# file as `crate::trk` without naming either. Only TRACK_FILES may name onyx_track; besides them
# only the dependent's lib.rs may name track_chain, and only as `pub mod track_chain;`; and no
# TRACK_FILES file defines a macro (a `#[macro_export]` macro lands at the crate root unnamed).
while IFS= read -r f; do
  case " $TRACK_FILES " in *" $f "*) continue ;; esac
  mod_root_ok "$f" && continue
  mount_ok "$f" && continue
  grep -nE "\\b($TRACK_NAMES)\\b" "$f"
  echo "TRACK-LEVEL FENCE: $f names onyx_track or a track module and is not in TRACK_FILES (ADR-0036 item 12, TR-R4-2, ROADMAP C6)"
  fail=1
done < <(git ls-files '*.rs' | grep -v '^crates/onyx-track/' | xargs -r grep -lE "\\b($TRACK_NAMES)\\b" || true)
for f in $TRACK_FILES; do
  if grep -nE 'macro_rules!|macro_export' "$f"; then
    echo "TRACK-LEVEL FENCE: $f defines a macro (ADR-0036 item 12, TR-R4-2)"
    fail=1
  fi
done
while IFS= read -r m; do
  case " $TRACK_DEPENDENTS " in *" $m "*) continue ;; esac
  echo "TRACK-LEVEL FENCE: $m depends on onyx-track and is not in TRACK_DEPENDENTS (ADR-0036 item 12, TR-R1-8)"
  fail=1
done < <(git ls-files '*Cargo.toml' | grep -vxE 'Cargo.toml|crates/onyx-track/Cargo.toml' \
  | xargs -r grep -lE 'onyx-track|onyx_track' || true)
# TR-R2-3: a rename in the root's [workspace.dependencies] (`trk = { package = "onyx-track" }`,
# then `trk.workspace = true`) named onyx-track in no member manifest and no Rust file. Refuse any
# rename, and read the real dependents from cargo metadata (package names, whatever the alias).
TRACK_DEPENDENT_PACKAGES="onyx-coordinator onyx-api onyx-web-verify onyx-source-ncei"
if git ls-files '*Cargo.toml' | xargs -r grep -nE 'package[[:space:]]*=[[:space:]]*"onyx[-_]track"'; then
  echo "TRACK-LEVEL FENCE: onyx-track renamed in a manifest; use it as onyx-track (ADR-0036 item 12, TR-R2-3)"
  fail=1
fi
while IFS= read -r p; do
  case " $TRACK_DEPENDENT_PACKAGES " in *" $p "*) continue ;; esac
  echo "TRACK-LEVEL FENCE: package $p depends on onyx-track and is not in TRACK_DEPENDENT_PACKAGES (ADR-0036 item 12, TR-R2-3)"
  fail=1
done < <(jq -r '.packages[] | select(any(.dependencies[]; .name == "onyx-track")) | .name' <<<"$meta")
# ROADMAP C10 (ADR-0028 amendment 7 item 5, THREATS C10-6): level-3 serving never reaches money.
# `signed_serve` (the /v1/signed route) names no pricing, payment, market-policy or catalog-level
# code, and only these files name it: its module root (as `pub mod signed_serve;` only), its tests,
# the node that mounts it, the deploy pin tool and (ROADMAP C61) the browser verifier's native test
# that pins the client check to the body the route writes. The Caddy route test names this module
# only to assert request-limit and method/path parity; it does not invoke or alter signed serving.
SIGNED_FILES="crates/onyx-api/src/signed_serve.rs crates/onyx-api/src/signed_serve_tests.rs \
crates/onyx-node/src/app.rs crates/onyx-node/examples/pin-signed-book.rs \
apps/web/verify/tests/signed_rehearsal.rs crates/onyx-node/tests/caddyfile.rs"
# Review C61-R1-5: the browser's level-3 client and its native test (it links onyx-api) too.
for f in crates/onyx-api/src/signed_serve.rs crates/onyx-api/src/signed_serve_tests.rs \
  apps/web/verify/src/signed_view.rs apps/web/verify/tests/signed_rehearsal.rs; do
  if grep -nE '\b(settles_on|MarketPolicy|money_default|market_policy|ProofLevel|settlement_reply|authorize|PaymentTerms|Payments|Facilitator|price|pricing|catalog)\b' "$f"; then
    echo "SIGNED-MONEY FENCE: $f names money, pricing or level code (ADR-0028 amendment 7, C10-6)"
    fail=1
  fi
done
while read -r f; do
  case " $SIGNED_FILES " in *" $f "*) continue ;; esac
  if [ "$f" = crates/onyx-api/src/lib.rs ] \
    && [ "$(grep -cE '\bsigned_serve\b' "$f")" = 1 ] \
    && grep -qE '^pub mod signed_serve;$' "$f"; then
    continue
  fi
  echo "SIGNED-MONEY FENCE: $f names signed_serve and is not in SIGNED_FILES (ADR-0028 amendment 7, C10-6)"
  fail=1
done < <(git ls-files '*.rs' | xargs -r grep -lE '\bsigned_serve\b' || true)
# Review C10-R1-8: the same for the coordinator's post log (`signed_intake`), which the API serves
# from: it names no money or level code, and only the level-3 files name it.
INTAKE_FILES="crates/onyx-coordinator/src/signed_intake.rs crates/onyx-coordinator/src/signed_intake_tests.rs \
crates/onyx-coordinator/examples/level3-book-anvil.rs crates/onyx-api/src/signed_serve.rs \
crates/onyx-api/src/signed_serve_tests.rs apps/web/verify/tests/signed_rehearsal.rs"
for f in crates/onyx-coordinator/src/signed_intake.rs crates/onyx-coordinator/src/signed_intake_tests.rs; do
  if grep -nE '\b(settles_on|MarketPolicy|money_default|market_policy|ProofLevel|PaymentTerms|Facilitator|pricing|onyx_api)\b' "$f"; then
    echo "SIGNED-MONEY FENCE: $f names money, pricing or level code (C10-R1-8)"
    fail=1
  fi
done
while read -r f; do
  case " $INTAKE_FILES " in *" $f "*) continue ;; esac
  if [ "$f" = crates/onyx-coordinator/src/lib.rs ] \
    && [ "$(grep -cE '\bsigned_intake\b' "$f")" = 1 ] \
    && grep -qE '^pub mod signed_intake;$' "$f"; then
    continue
  fi
  echo "SIGNED-MONEY FENCE: $f names signed_intake and is not in INTAKE_FILES (C10-R1-8)"
  fail=1
done < <(git ls-files '*.rs' | xargs -r grep -lE '\bsigned_intake\b' || true)
# C50-R1-1: onyx_tlock::decrypt_unverified skips the beacon signature check; only onyx-tlock's
# own tests and the fuzz target may call it (Onyx opens through BeaconKey::open).
while IFS= read -r f; do
  echo "TLOCK FENCE: $f calls onyx_tlock::decrypt_unverified (use BeaconKey::open; THREATS C50-R1-1)"
  fail=1
done < <(git ls-files '*.rs' | grep -vE '^crates/onyx-tlock/|^fuzz/fuzz_targets/tlock_open\.rs$' \
  | xargs -r grep -lw 'decrypt_unverified' || true)
[ "$fail" -eq 0 ] && echo "boundaries ok: $(wc -w <<<"$core") core crates, $(wc -w <<<"$sector") sector crates"
exit "$fail"
