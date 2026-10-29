# Spec: browser TLS prover (draft, NORTH-STAR NS6, north-star loop NL11, ROADMAP S2)

Status: **Draft, proposed 2026-10-27** (lane B, design only; nothing is built). **Parked 2026-10-28**
(north-star loop NL11): upstream unchanged since alpha.15; see the NL11 row for the unblock steps. Builds on the
S2b experiment (`experiments/tlsn-fiscaldata`, ROADMAP S2) and docs/research/2026-10-20-api-keys-zk.md.
Prior art and current upstream state: RESEARCH.md "Lane B: paid observation, storage tiers,
browser TLS prover (2026-10-27)".

## Scope

A contributor proves what a keyed API answered, from the browser, without the key leaving the
page. S2b did this natively: a real TLSNotary presentation of the Treasury API with a hidden
`X-Demo-Key` header, verified offline (18,553-byte presentation, 34.5 MB of prover-notary traffic,
1.34 s with the notary in the same process). NL11 moves the prover into the browser and the
verifier into Onyx.

Out of scope: sources on TLS 1.3 only (TLSNotary supports TLS 1.2 today), sources whose terms
forbid redistribution (FRED: docs/ops/data-sources.md N10), and calling the result level 3 (see
rule 4).

## Interfaces

- **Upstream pieces (2026-10-27).** `tlsnotary/tlsn` is at `v0.1.0-alpha.15` (a pre-release; the
  README says it "should not be used in production"; Apache-2.0 or MIT). `tlsn-js` is archived;
  browser work moved to `tlsnotary/tlsn-extension` (a Chrome MV3 extension with QuickJS plugins, a
  plugin SDK, and a verifier server on port 7047 with a `WS /proxy?token=<host>` endpoint, because a
  browser cannot open a TCP connection to the API). alpha.15's WASM methods take an I/O channel
  injected from JavaScript and run MPC threads in Web Workers.
- **Onyx verifier service** (new, next to the node, behind Caddy at `/tlsn/`): the TLSNotary
  verifier for an allowlisted host and target only (the S2b policy: pinned host, `GET` of the
  pinned path, every byte shown except the declared key header's value, status 200, a JSON value at
  the pointer, a maximum age), plus the WebSocket proxy to that host only. Its signing key is
  Onyx's notary key (the one trusted thing, S2b).
- **Page.** The submit page's keyed path (today the labelled stub `stub-s2`) loads the prover
  (tlsn-wasm in the page, or the extension's plugin), takes the key in a field that never leaves
  the page, runs the session through `/tlsn/proxy`, and posts only the presentation to
  `/v1/submit` with prover id `tlsn-v1`.
- **API.** `/v1/submit` accepts `tlsn-v1` for a cleared host and pinned target, runs the S2b
  verifier in its isolated child process under a memory limit (THREATS TLSN-5), stores the
  presentation and its proof hash (not the file hash, TLSN-6), and records a level-4 keyed claim.

## Rules

1. The key never reaches Onyx: the request bytes of the declared header are hidden in the
   presentation and the verifier refuses one that shows them; the page posts nothing else.
2. One host, one target, one header per policy entry; the proxy refuses any other host.
3. TLS 1.2 only, checked by the verifier; a TLS 1.3 source is refused up front.
4. **Level.** Onyx's own notary means Onyx could sign a false transcript, so a `tlsn-v1`
   submission stays **level 4 (keyed, unverified by others)** until no single party can forge it
   (several independent notaries, or a notary run by an independent operator: NS4). This is the
   research note's rule ("level 3 only when no single party can forge the proof").
5. A presentation older than the policy's maximum age, or from the future, is refused.
6. **Minimal disclosure (review NL11-1).** The presentation reveals only the request line, the
   host and the bytes of the value at the pinned pointer with the JSON structure around it; the
   rest of the response is hidden, and the policy admits only endpoints whose answer does not
   depend on whose key it is (a public dataset behind a key, checked in the rights review), so a
   key holder's own account data never reaches Onyx.
7. **The page that holds the key (review NL11-2).** The prover page loads only same-origin
   scripts with subresource integrity, under a strict Content-Security-Policy (no inline script,
   no third-party origin, `connect-src` only the page's own `/tlsn/`), and the key field is never
   written to storage, a URL or a log.
8. **The proxy is not a relay (review NL11-3).** `/tlsn/proxy` opens a connection only for a
   session the verifier started, to the session's pinned host, once, with a byte cap (above the
   measured 34.5 MB per proof) and a time cap; per-address limits on sessions.
9. **MPC sessions cost the verifier (review NL11-4).** Each session uses about 34.5 MB of
   traffic and CPU on Onyx's side, so starting one needs a prepaid hold (NL8 model) or a per-address
   limit of a few sessions per hour; at most S sessions run at once.

## Assumptions

- tlsn alpha.15's WASM build works in Chrome and Firefox on the page (not yet checked: S2b's
  README lists "whether this tlsn release builds for the browser" as unchecked). If it needs
  `SharedArrayBuffer`, the page needs cross-origin isolation (COOP and COEP headers), which the
  site's Caddy file can set for that page only.
- The MPC traffic (about 34.5 MB per proof in S2b) is acceptable over a home connection; a
  remote verifier adds round trips that S2b did not measure.
- The pre-release status is acceptable for a testnet feature that is labelled as such.

## Open questions

1. In-page WASM or the extension? In-page needs no install; the extension is where upstream
   browser work now lives. First step of the build: build alpha.15's WASM and prove the Treasury
   target from a page against a local verifier, and measure.
2. Which first keyed source? It needs TLS 1.2, a key, and terms that allow redistribution of the
   value; Treasury needs no key (S2b used a dummy header). A real keyed candidate must pass the
   rights review first.
3. Version pinning: the verifier server and the client must match versions (the extension's
   README warns of this); pin both to one tag and refuse others.

## Threats the build must test

Review NL11 (lane B, 2026-10-27): THREATS NL11-1..4.


A page that leaks the key (a test that the posted body never contains it), a proxy used to reach
other hosts (refused), a forged or bit-flipped presentation (S2b's scan in the browser path too),
a huge presentation (size cap, isolated verifier), an old presentation replayed (maximum age and
proof hash seen once), a prover and Onyx's notary colluding (rule 4 keeps the level at 4).
