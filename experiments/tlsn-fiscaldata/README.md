# TLSNotary experiment: a Treasury response with a hidden header (ROADMAP S2b, partner tracker T11)

A real TLSNotary presentation of one U.S. Treasury Fiscal Data response ("Debt to the Penny",
record date 2026-10-08), with one request header hidden. The header, `X-Demo-Key`, stands in
for an API key: its value was 32 random bytes, generated for the run and never stored. Onyx (the
verifier) learns the server, the request and the response, and does not learn that value.

This is its own cargo workspace, excluded from the root workspace, so `scripts/check-all.sh`
never compiles the pre-release tlsn crates.

## Run

From this folder:

1. `cargo test --locked`: verifies the committed presentation offline and runs the refusal tests.
2. `cargo run --locked --release --bin verify -- fixtures/presentation.bin`: prints what the
   presentation proves.
3. `cargo run --locked --release --bin prove`: makes a new presentation. Network: only
   `api.fiscaldata.treasury.gov`, User-Agent `onyx-dev`. The notary runs in the prover's own
   process with a random key that is dropped after the run.

## What was measured (the committed run, `fixtures/summary.json`)

| Measure | Value |
|---|---|
| Server, TLS version | `api.fiscaldata.treasury.gov`, TLS 1.2 |
| Value proved | `40305316210829.72` at `/data/0/tot_pub_debt_out_amt` |
| Hidden request bytes | 32 (the `X-Demo-Key` value) |
| Hidden response bytes | 260 (`Set-Cookie` values only) |
| Presentation size | 18,553 bytes |
| Prover to notary traffic | 30,487,436 bytes |
| Notary to prover traffic | 4,005,145 bytes |
| Total time | 1.34 s (TLS and HTTP 0.62 s, MPC setup 0.32 s, prove 0.40 s, verify 0.4 ms) |

These are one run on one machine (the project owner's dev box, notary in the same process, so no network
latency between prover and notary). A remote notary adds round trips over the 34.5 MB exchanged.

## What the verifier checks (`src/lib.rs`)

1. Size at most 1 MiB, and the bytes decode exactly.
2. The notary key is the pinned one: the one thing trusted.
3. The notary signature, the server's certificate chain at the connection time, and the
   transcript commitments.
4. Server name, TLS 1.2, session age within the policy, not in the future.
5. Request: `GET` of the pinned target to the pinned host; every byte shown except the value of
   exactly the one declared header.
6. Response: status 200; every byte shown except `Set-Cookie` values; `Content-Length` matches;
   the body is JSON with a string at the pointer.

## Robustness scan (`no_bit_flip_changes_what_is_proved`)

Every one of the 18,553 single-bit flips (bit 0 of each byte) was run through the verifier in a
child process under a 4 GiB memory limit:
- 0 flips change what is proved;
- 8 flips (bytes 15,228-15,231 and 15,236-15,239) make tlsn-core ask for a huge allocation (about 1 TB in the logged one) and abort;
  `src/isolated.rs` runs the verifier in its own process under a memory limit, so such a file is
  refused and the caller survives (THREATS TLSN-5);
- 1 flip (byte 7,383) verifies with the exact same result, so the file's hash is not the
  proof's identity (THREATS TLSN-6).

## What it does not prove

- That the notary was honest. A notary that colludes with the prover can sign a false transcript.
  Running the notary ourselves (or several notaries) is the answer the TLSNotary docs give.
- Anything about TLS 1.3 servers: TLSNotary supports TLS 1.2 only.
- Freshness beyond the session time: an old presentation stays valid unless the policy sets a
  maximum age.

## Integration plan (not built)

1. Prover id `tlsn-v1` in the submission's proof field (the stub is `stub-s2`).
2. The API accepts a `tlsn-v1` presentation only for a cleared host and a pinned target, runs this
   verifier with Onyx's notary key and a maximum age, and stores the presentation and its hash.
3. The page runs the prover in the browser (whether this tlsn release builds for the browser is
   not checked yet) and sends only the presentation, never the key.
4. Trusted: Onyx's own notary key. A third-party notary is a policy choice per source.
5. Gap to production: a browser prover, a hosted notary, TLS 1.3 sources, and the source's terms
   (keyed APIs often forbid sharing their data; FRED does).
