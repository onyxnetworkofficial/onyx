# ADR-0018: Offchain commit-reveal coordinator and a content-addressed store

## Status
Accepted, 2026-10-08 (M3 loop 17). Amended loops 19-20 after reviews (L18-F1..F6, L19-F1..F5): items 2, 3, 5, 6. Implemented in `crates/onyx-coordinator` (`cas`, `round`) and
`crates/onyx-miner`. End to end on real blocks: `adapters/source-btc/tests/round.rs`.

## Context
M3 needs miners that compute shards and a round that stops copying. Commit-reveal is the standard
fix: Bittensor added it against weight copying (RESEARCH.md), and ENS uses it against front-running,
with a commitment that must be between 1 minute and 24 hours old before it can be revealed
(https://docs.ens.domains/registry/eth). Onyx already fixed the hashes (ADR-0011, shared
Rust/Solidity vectors). PLAN q10 (a) requires that everything a shard pins can be fetched by hash
before the round opens. The contracts come in M6; M3 needs the same rules offchain first.

## Decision
1. CAS: objects are stored under the hash the protocol pins (recipe id, label set content hash,
   attestation content hash, `keccak256(params)`, Merkle root for row batches), with the kind in the
   key. Every read rechecks the hash, so a store can lose data but never serve wrong data. `MemCas`
   for tests, `FsCas` for disk (write then rename).
2. Round (`Round`), the offchain model of the settlement contract, times in chain heights:
   - `open`: every `ShardSpec` validates against the pinned registry, and its recipe, params, label
     set and every attestation the set cites are in the CAS; otherwise the shard does not open.
   - `commit` while `now < commit_close`: one bonded commit per (miner, shard), bond >= minimum,
     signed by the miner's key over `commit_message(round_tag, shard, commit_hash, bond)` (EIP-191),
     where `round_tag` binds domain, round, timing, bond and shard set, so nobody can take another
     miner's slot or replay a commit into another round (L18-F2, L19-F3). Settled shards are not
     re-offered (L19-F4).
   - `reveal` while `commit_close <= now < reveal_close`: `commit_hash(round, shard, binding,
     miner, salt)` must equal the commit, the commitment must match the shard's pinned fields and
     validate, and the rows behind its root must be in the CAS with the stated count (R-DA-1).
   - `close` once reveals are over: reveals per shard in commit order, roots grouped by miner,
     unrevealed commits listed as forfeited. Correctness is judged by validators (M4).
3. Miner: a secp256k1 key whose EVM-style address is bound into every commit. It fetches the
   shard's inputs from the CAS by hash, runs the canonical engine (`assemble_input_cached`), and keeps
   the rows private until reveals open (`publish_rows`): rows in the shared store during the commit
   phase would let anyone copy the answer (L18-F1). Salt:
   `keccak256("onyx-salt-v1" ‖ secret ‖ round ‖ shard)`, secret derived from the key.
4. Commit order (`seq`) stands in for block order until M6; "first valid commit" uses it.
5. Heights only move forward (`TimeWentBack`), and `close` freezes the round: later commits and
   reveals are refused and `close` returns the same outcome again (L18-F3).
6. `open` checks that pinned inputs are usable, not only present: the stored label set has the
   pinned name and version and validates under the registry, cited attestations fit the caps, and a
   document snapshot's bytes are stored (L18-F4). Row batches are capped at 24 MiB, above any encoded honest output, and miners refuse to commit a
   result they could not store (L18-F5, L19-F1).

## Consequences
- Replaying another miner's commit or revealed commitment fails: the commit hash binds the miner
  address (`CommitMismatch`) and the copier forfeits its bond. Copying the answer itself during the
  commit phase is prevented only because honest miners publish rows after commits close; a miner
  can still recompute the shard honestly, which is the intended work.
- Lazy miners must still upload rows for their root; recompute in M4 catches the wrong root.
- The coordinator is trusted for ordering only in the prototype; M6 moves ordering to block order
  onchain and bonds to real (testnet) tokens.
- Reveal timing has no minimum age yet (ENS needs one because names are public; Onyx commits are
  per shard and hidden until reveal opens for everyone at the same height).
- The commitment binding hash does not include params (the shard id does, and the commit hash binds
  the shard), so one commitment matches shards that differ only in params; within a round the commit
  is still bound to one shard (L18-F7, info).
