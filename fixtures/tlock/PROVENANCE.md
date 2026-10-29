# Time-lock vectors (ROADMAP C50)

Known-answer vectors for `crates/onyx-tlock` (drand/tlock's IBE for quicknet). Fetched
2026-10-17 over plain HTTPS with a generic User-Agent and no account; every downloaded file was
treated as data (parsed by our own code, never executed). Checksums are in `../SHA256SUMS`;
`vectors.json` lists each file with its source URL. Checked by
`cargo test -p onyx-tlock --test interop` (native) and `scripts/check-tlock-wasm.sh` (wasm32).

| File | Source | What |
|---|---|---|
| `go-tlock/lorem-tle-testnet-quicknet-t-2024-01-17-15-28.tle` | https://raw.githubusercontent.com/drand/tlock/7b54141a9733fd6fa207587a11148280e6fb020d/testdata/lorem-tle-testnet-quicknet-t-2024-01-17-15-28.tle (drand/tlock v1.2.0 testdata, verbatim) | age file made by Go `tle`, quicknet-t round 5423142 |
| `go-tlock/tlock_test-interop-quicknet-t-1624910.age` | https://raw.githubusercontent.com/drand/tlock/7b54141a9733fd6fa207587a11148280e6fb020d/tlock_test.go (the armored block of `TestInteropWithJS` "on testnet with quicknet-t", verbatim plus a final newline) | made by tlock-js, decrypted by Go's test; quicknet-t round 1624910 |
| `tlock-js/rfc-scheme-quicknet-746511.age` | https://raw.githubusercontent.com/drand/tlock-js/17d817ee259e79381111dd75009b0f022c39ace3/test/rfc-scheme.test.ts (the concatenated string of "should pass test vectors from go codebase") | made by Go per that test's name; quicknet MAINNET round 746511 |
| `drand/quicknet-info.json`, `drand/quicknet-round-{1,2,1000,746511,33154000}.json` | https://api.drand.sh/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971/info and `/public/<round>`, verbatim | quicknet's key and published round signatures |
| `drand/quicknet-t-info.json`, `drand/quicknet-t-round-{5423142,1624910}.json` | https://pl-us.testnet.drand.sh/cc9c398442737cbd141526600919edd69f1d6f9b4adb67e4d912fbc64341a9a5/info and `/public/<round>`, verbatim | drand's quicknet-t testnet (same scheme `bls-unchained-g1-rfc9380`, another key) |
| `drand/quicknet-c51-rounds.json` | https://api.drand.sh/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971/public/<round> for 21 rounds (32964509-32964515, 33152429-33152435, 33152729-33152735), each answer verbatim in `rounds` (fetched 2026-10-17, User-Agent `onyx-drand-client`, no account) | ROADMAP C51: the lock rounds of the recorded Treasury window and of the anvil rehearsal (`scripts/level2-timelocked.sh`) with neighbours, for the time-locked runner tests and the rehearsal's fixture beacon; every signature is verified against the pinned key wherever it is used |
| `onyx-zero-tail.json` | made by `onyx-tlock` itself (`ONYX_BLESS=1 cargo test -p onyx-tlock --test interop`) | 8 ciphertexts with `tau` ending in 1-16 zero bytes, locked to published quicknet rounds above; cross-target consistency vectors, NOT interop vectors |
| `onyx-far-date.json` | made by `onyx-coordinator` (`ONYX_BLESS=1 cargo test -p onyx-coordinator c52_far_date_vectors`; ROADMAP C52, ADR-0027 amendment 15) | 5 far-date copies: a body key `kappa` locked with `sigma = keccak256("onyx-sealed-far-sigma-v1" ‖ kappa ‖ round)[..16]` to published quicknet rounds above; cross-target consistency vectors, NOT interop vectors |

Licences: drand/tlock and drand/tlock-js are Apache-2.0 OR MIT (their `LICENSE-*` files and
tlock-js `package.json`); drand beacons are public randomness served without terms.

Not available: a Go-made MAINNET quicknet vector in drand/tlock's own repository (its testdata
holds quicknet-t, testnet and fastnet files; the one mainnet quicknet vector is in tlock-js,
labelled as from the Go code); a Go-made vector whose file key ends in zero bytes (none of the
three does); the testnet "testnet-g" chain (`f3827d77…`) of
`lorem-tle-testnet-g-2024-01-17-15-31.tle`, whose chain info and rounds pl-us.testnet.drand.sh no
longer serves; fastnet files (another scheme, no longer served by api.drand.sh). Running Go
drand/tlock to make more was not done (no downloaded code executed): ROADMAP C70.
