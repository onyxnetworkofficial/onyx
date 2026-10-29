# History secret scan, 2026-10-15/16 (ROADMAP L13, launch checklist LC-25, THREATS T-KEY-1)

Branch `history-scan` from main `7c904cf`. Run on the dev box on the evening of 2026-10-15 (EDT).
Result: **no real secret in any commit, branch or remote-tracking ref, in any unreachable local
commit, or in main's working tree.** Every finding is a public hash, a test key or a placeholder.
The testnet deployer key was never committed: no 64-hex string ever added to history is the private
key of the deployer address (derivation check below), so there is nothing to rotate.

What this scan does **not** prove: that the CDP API key secret or an RPC provider key from `.env`
never appeared in a form the rules do not recognise. The loop never reads `.env`, so the direct
cross-check (each `.env` value against history, counts only) is a lead step:
[Lead step](#lead-step-env-values-against-history-counts-only). Rewriting history before a public
push stays the project owner's decision (LC-26, THREATS L8-H1..H3); nothing was rewritten or pushed.

Output rule: every report line below gives rule, file, short commit and line, and at most the
first 4 characters of a value. The raw reports lived in a private scratch directory and were
deleted after triage.

## Tools

| Tool | Version | Source and checksum |
|---|---|---|
| gitleaks | 8.30.1 (released 2026-03-21) | `gitleaks_8.30.1_linux_x64.tar.gz` from github.com/gitleaks/gitleaks releases; SHA-256 `551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb` matches `gitleaks_8.30.1_checksums.txt` (`sha256sum -c`: OK). Binary SHA-256 `88f91962aa2f93ac6ab281d553b9e125f5197bbbce38f9f2437f7299c32e5509`. Installed in `~/.local/opt/gitleaks-8.30.1`, linked from `~/.local/bin`; no sudo |
| trufflehog (OSS) | 3.97.9 (2026-09-24; chosen over 3.99.2, which was a week old) | `trufflehog_3.97.9_linux_amd64.tar.gz` from github.com/trufflesecurity/trufflehog releases; SHA-256 `40377e6572495412fb9ba0bc21c9401f73b72f1d2afd11b9931bc4a5ed622866` matches `trufflehog_3.97.9_checksums.txt` (OK; the cosign signature on that file was not checked). Binary SHA-256 `2e0a09c37d104c2f3069021f0c1ab33cf07287b6aca4d59eb4684d7d73fb89b4`. `~/.local/opt/trufflehog-3.97.9`. Run with `--no-verification` (no call to any third-party API); it scans a clone of the main repository, so its 10 local branches; every remote-tracking tip is contained in a local branch, so it saw every reachable commit too |
| `scripts/check-secrets.sh --history` | this branch | the repository's own 5 patterns and env/key file names over `git log --all -p` (new, repeatable without gitleaks) |
| Key derivation check | one-off Python (pure secp256k1 + Keccak-256, checked against anvil account 0 -> `0xf39F…2266` and `keccak256("")`) | every distinct 64-hex string ever added, as a private key, against every address that appears in history |
| BIP-39 word runs | english.txt from github.com/bitcoin/bips (2048 words) | lines with 12 or more consecutive BIP-39 words |

## Scope

| What | Count |
|---|---|
| Refs | 10 local branches, 65 remote-tracking (`origin`), 0 tags, 0 stashes; `git ls-remote origin` lists 65 refs, all present locally |
| Commits on all refs | 428 (323 non-merge; merges print no patch, as in gitleaks, so merge resolutions were not scanned on 2026-10-15 (SCAN-4, scanned in the review below); one non-merge commit, `61f387b`, adds only a blank line, so gitleaks counts 322) |
| Unreachable local commits | 18 (`git fsck --unreachable --no-reflogs`; never on a ref, so not in a push or a mirror of GitHub) scanned too |
| Added lines read | 471,493 (`--history`); about 29.4 MB of patches (gitleaks) |
| Main working tree | `<local path>` at `7c904cf`, 704 tracked paths, no untracked unignored file; `.env` excluded by name before copying; the `contracts/lib/forge-std` submodule's checked-out files (upstream, public) included |
| Commit messages | all 428 |

## Commands

```sh
# 1. gitleaks, all history, redacted (before the allowlist existed)
gitleaks git --log-opts="--all" --redact --report-format json --report-path $S/gitleaks-history.json --exit-code 0 .
# 2. gitleaks, main's working tree: tracked files copied without .env, then scanned
git -C ~/projects/onyx ls-files -co --exclude-standard -z | grep -zvxE '\.env' | tar --null -T - -cf - | tar -x -C $T
(cd $T && gitleaks dir --redact --report-format json --report-path $S/gitleaks-main-tree.json --exit-code 0 .)
# 3. trufflehog, all branches, no verification
trufflehog git file://<local path> --no-verification --no-update --json --results=verified,unknown,unverified > $S/trufflehog.jsonl
# 4. the repository's own patterns over history (counts; add --list for commit:file:line)
scripts/check-secrets.sh --history
# 5. unreachable commits: their added lines through gitleaks stdin and the same 5 patterns
git show -U0 --format='commit %h' $(git fsck --unreachable --no-reflogs | awk '/commit/{print $3}') | grep -E '^\+|^commit ' | gitleaks stdin --redact
# 6. after the allowlist: both scans must be clean
gitleaks git --log-opts="--all" --redact .           # no leaks found
(cd $T && gitleaks dir --config .gitleaks.toml --redact .)   # no leaks found
```

Every heavy command ran under `systemd-run --user --scope -q -p MemoryMax=4G`. `$S` and `$T` are
private scratch directories, deleted after the scan.

## Counts per rule

| Scanner, target | Rule | Findings | True secret | Test value / public hash | False positive |
|---|---|---|---|---|---|
| gitleaks, all history | `generic-api-key` | 9 | 0 | 8 | 1 |
| gitleaks, all history | every other default rule | 0 | 0 | 0 | 0 |
| gitleaks, main working tree | `generic-api-key` | 5 (same items as history) | 0 | 4 | 1 |
| gitleaks, 18 unreachable commits | all | 0 | 0 | 0 | 0 |
| gitleaks, commit messages | all | 0 | 0 | 0 | 0 |
| trufflehog, all branches (unverified) | `Circle` | 1 | 0 | 0 | 1 |
| trufflehog, all branches (unverified) | `URI` | 1 | 0 | 1 | 0 |
| check-secrets `--history` | env/key file ever added | 0 | | | |
| check-secrets `--history` | patterns 1-5 (key assignment, `--private-key`, API secret, mnemonic, PEM) | 0 each | | | |
| check-secrets patterns, unreachable commits | patterns 1-5 | 0 each | | | |
| Key derivation, 25,852 distinct 64-hex strings (+743 only in unreachable commits) vs 161 addresses | key of an address in the repo | 1 | 0 | 1 | 0 |
| BIP-39 runs of 12+ words | mnemonic-shaped text | 3 lines | 0 | 3 | 0 |
| Provider URLs (Alchemy, Infura, QuickNode, Ankr, dRPC, CDP and 12 more) and `?key=`/`apikey=` | key in a URL | 5 | 0 | 5 | 0 |
| `CDP_API_KEY_*` / `apiKeySecret` assignments | value present | 4 | 0 | 4 (empty, `...`, `s`) | 0 |
| Keystore JSON (`ciphertext`, `kdfparams`) | encrypted key file | 0 | | | |

After the committed allowlist (`.gitleaks.toml`), gitleaks reports **no leaks** on all history and
on the tree.

## Triage

| # | Rule | File | Commit(s) | Line | Value | Class | Why |
|---|---|---|---|---|---|---|---|
| 1 | gitleaks `generic-api-key` | `scripts/check-secrets.sh` | `bc1733f` (tree: line 30 on this branch) | 23 | `--pr…` | False positive | A comment listing the flag forms the scan looks for (`--private-key 0x..`); the "value" is the literal `0x..` |
| 2-5 | gitleaks `generic-api-key` | `fixtures/registry/vectors.json` | `ec5ce44` (not flagged on today's tree) | 52, 56, 60, 64 | `0x6d…`, `0x6c…`, `0xee…`, `0xc5…` | Public hash | `metrics[].key` = keccak256(metric name); all four recomputed with `cast keccak` and equal (the last is keccak256 of the empty string) |
| 6 | gitleaks `generic-api-key` | `fixtures/registry/vectors.json` | `ec5ce44` (tree: 127) | 71 | `0xee…` | Public hash | `treasuryFeed.metricKey`, the same value as row 4 |
| 7 | gitleaks `generic-api-key` | `fixtures/registry/vectors.json` | `0a0723a` (tree: 133) | 118 | `0x88…` | Public hash | `subjectKey` of the treasury observation, generated by `adapters/source-fiscaldata/tests/registry_vectors.rs`, read back by `OnyxRegistry.t.sol` |
| 8 | gitleaks `generic-api-key` | `fixtures/registry/transcript-log.json` | `130c5dd` | 43 | `0x76…` | Public hash | `OnyxRegistry.logKey(fact, attempt, observer, round)`, a storage slot key in a generated vector |
| 9 | gitleaks `generic-api-key` | `adapters/source-signed/fixtures/example-report.json` | `01c9da5` | 2 | `0x2b…` | Public hash | Key id (hash of the public key) of the synthetic source-signing test key; `level3.rs::fixtures_are_reproducible` regenerates it |
| 10 | trufflehog `Circle` | `docs/ops/deployment.md` | `4a61ab7` | 364 | `036C…` | False positive | Circle's public USDC contract address on Base Sepolia, not a Circle API key |
| 11 | trufflehog `URI` | `crates/onyx-api/src/x402.rs` | `00521de` | 1076 | `http…` | Test value | A hostile URL with a user-info part and an `.example` host, a case in the facilitator URL parser test (M7-L50-F3); no credential |
| 12 | Key derivation | `crates/onyx-api/src/evm_tx.rs` | `a3904a5` onward (tree: line 714) | 306 | `3b29…` | Test key | `const KEY` in `mod tests`: sha256 of the phrase in its own comment (recomputed: equal); it controls `0x34bf…5219`, which appears only in the same test; that address has nonce 0 and balance 0 on Base Sepolia (`cast nonce`/`cast balance`, chain id 84532 checked first). Neither gitleaks, trufflehog nor check-secrets flags a bare `KEY` const (THREATS SCAN-2) |
| 13 | BIP-39 run | `scripts/check-secrets.sh` | `bc1733f` | self-test | `test…`, `aban…` | Test value | The public "test ... junk" mnemonic (2 lines) and the self-test's planted `abandon ability ...` word list |
| 14 | Provider URL | `crates/onyx-node/src/config.rs`, `scripts/read-indexer-sepolia.py` | `3ca12ec`, `bc1733f` | tests | `/v2/SECR…` | Placeholder | `https://base-sepolia.g.alchemy.com/v2/SECRETKEY` in URL-redaction tests |
| 15 | Query key | `crates/onyx-node/tests/settle_paths.rs`, `scripts/health-watch-tests.py` | `9cccc2b`, `51ac9b5` | tests | `SECR…`, `HIDD…` | Placeholder | `apikey=SECRETQ`, `apikey=SECRET123`, `key=HIDDEN`: redaction tests |
| 16 | CDP assignment | `scripts/testnet-faucet.py`, `scripts/check-secrets.sh`, docs | `285efff`, `bc1733f`, later | - | empty, `...`, `s` | Placeholder | The faucet script reads the key from env or `.env` at run time; docs name the variables only |
| 17 | trufflehog `URI` | `docs/audit/2026-10-16-history-secret-scan.md` | `2dd2649` (WIP of this report) | 96 | `http…` | Test value | Row 11's test URL quoted in full in the first draft of this report; the row now describes it instead. Stays in that commit (no rewrite) |
| 18 | check-secrets pattern 2 (`--private-key`) | `scripts/check-secrets.sh` | `bc1733f` | 68 | `0xac…` | Test value | The pre-SCAN-1 self-test line with the mistyped copy of anvil's key; reported once `ALLOW_RE` held the correct key. Reviewed in `HISTORY_ALLOW_AT` by full commit:file:line (no value in the script) |

Not secrets but noted: `claude.ai` session links in two files (sign-in required, no token).

## Findings and decisions

| ID | Decision |
|---|---|
| Testnet deployer key | **Not committed.** Its address `0x80c9…7972` appears in history, and none of the 26,595 distinct 64-hex strings derives to it; no keystore file, no `.env*`/`*.key`/`*.pem`/`.onyx-testnet/` file was ever added. No rotation is needed on this evidence. If the lead step below finds a `.env` value in history, treat that key as compromised: a new testnet deployer (lead/the project owner action, never the loop) |
| CDP API key id/secret, RPC provider keys | Not found by gitleaks, trufflehog, the repository's patterns or the URL scan; every occurrence is a placeholder. The lead step is the only check against the real values |
| SCAN-1 | The tree scan's allowlist entry for anvil's account 0 key had one wrong digit (since `bc1733f`), so it allowlisted an unrelated value and not anvil's key. Fixed; the self-test now derives the key from the public mnemonic with `cast` and fails on a mismatch |
| SCAN-2 | A bare `const KEY: &str = "<64 hex>"` is invisible to all three rule sets. Accepted: the only one is a public test key by construction; the derivation check is the backstop and runs again before any public push |
| History rewrite | Not done and not allowed for the loop. The rows above give no reason to rewrite; L8-H1..H3 (data, not secrets) still decide "rewrite or keep private" (LC-26, the project owner) |

## Allowlist

`.gitleaks.toml` extends gitleaks' default rules with 6 entries for the reviewed items 1-9 only.
Each needs the exact path **and** the exact value (item 1: the exact comment line), each with a
comment saying why it is safe. Checked: changing one hex digit of an allowlisted value, or copying
the file to another path, is reported again (1 and 4 findings).

## Repeatable check

`scripts/check-secrets.sh --history` runs the 5 patterns and the env/key file names over every line
added on any ref, merge resolutions included (`--remerge-diff`, SCAN-4), prints counts per pattern
(`--list` adds commit:file:line, never a value), then
`gitleaks git --log-opts="--all --diff-merges=first-parent" --redact --config .gitleaks.toml` with a
report template that writes only rule, short commit, file and line; it exits 1 on any hit of
either, and also when gitleaks is missing, exits non-zero or writes no report (SCAN-5). Reviewed pattern hits sit in `HISTORY_ALLOW_AT` (full commit:file:line), so the same value
in another commit or file still fails. Its `--self-test` (inside check-all) commits a key, deletes it, and requires
the tree scan to pass, `--history` to fail and name `commit:old.txt:1`, and the value never to be
printed; it also requires a key added only in a merge's conflict resolution to fail `--history`
(and gitleaks, when installed), and a missing, crashing or silent gitleaks to fail. It takes about 2 s on this repository; it is not in check-all (history only grows; run
it with gitleaks before any public push).

## Rescan 2026-10-16

After merging main `cfb4bbd` (L12 health watch) into this branch: 442 commits on 76 refs (329
non-merge), 18 unreachable commits, this branch's tracked tree (714 files, `.env` excluded by name).
Same tools and versions; every command under `systemd-run --user --scope -q -p MemoryMax=12G`.

| Scanner, target | Findings | Change since 2026-10-15 |
|---|---|---|
| gitleaks default rules, all history | 9 `generic-api-key` | none: the same items 1-9 |
| gitleaks with `.gitleaks.toml`, all history | 0 | none |
| gitleaks default rules / with `.gitleaks.toml`, tracked tree | 5 / 0 | none (row 1 is now line 30) |
| gitleaks, 18 unreachable commits | 0 | none |
| trufflehog, all local branches, no verification | 3 unverified (`Circle` 1, `URI` 2) | +1: row 17, this report's own draft |
| `check-secrets.sh --history` | 0 (+1 reviewed: row 18) | row 18 appeared with the SCAN-1 fix; the 2026-10-15 counts ran with the mistyped `ALLOW_RE` |

Not rerun: the key-derivation check, BIP-39 runs and provider-URL scans (one-off tooling, not
kept). The new main commits add no `.env*`/key file and no gitleaks, trufflehog or pattern hit;
the derivation check is due again with the rescan before any public push.

## Review 2026-10-16 (fresh reviewer)

Tested in a throwaway clone with an obviously fake planted value (sha256 of a test phrase):

| Case | Before | After |
|---|---|---|
| Key committed then deleted on a side branch | `--history` exit 1, `commit:file:1` listed, value not printed | same |
| Key added only in a merge commit (conflict resolution, or an extra file in a clean merge) | missed by `--history` **and** by gitleaks (`git log -p` prints no patch for merges): exit 0 | both report it, exit 1 (SCAN-4) |
| gitleaks not installed | "skipped", exit 0 | exit 1 (SCAN-5) |
| gitleaks exits non-zero | exit 2 (set -e) | exit 1 with a message |
| gitleaks exits 0 but writes no report | exit 0 (empty count) | exit 1 (SCAN-5) |

Rescan of this repository with merges included: `--history` 0 (+1 reviewed) and gitleaks 0 with
`.gitleaks.toml` (also 0 with `--remerge-diff`). Not rerun on merge-only lines: trufflehog and the
key-derivation check; both are due with the rescan before any public push.

## Lead step: .env values against history (counts only)

Run from the main checkout. It prints variable names and counts, never a value; values go to grep
through a file descriptor, not argv. `-m` includes merge commits; `-i` ignores hex case.

```sh
grep -E '^(export +)?[A-Za-z0-9_]+=' .env | while IFS='=' read -r k v; do
  k=${k#export }; v=${v%$'\r'}; v=${v%[\"\']}; v=${v#[\"\']}; v=${v#0x}; [ ${#v} -ge 12 ] || continue
  n=$(git log --all -p -m --no-color | grep -ciF -f <(printf '%s\n' "$v") || true)
  echo "$k: $n"
done
```

Every count must be 0. A non-zero count for the deployer key means rotate the testnet deployer
(new key, move test funds, update roles); for the CDP or RPC key, revoke it at the provider.
