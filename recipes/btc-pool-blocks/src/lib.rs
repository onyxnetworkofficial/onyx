//! `btc-pool-blocks`: how many blocks of a range each mining pool mined, by
//! the pool's coinbase payout script or coinbase tag (ADR-0032).
//!
//! Input (`RecipeInput`):
//! - `source`: Borsh `CoinbaseSource` (each block's coinbase `scriptSig` and
//!   outputs, contiguous heights);
//! - `labels`: must be empty (this metric reads no label set);
//! - `params`: Borsh `PoolTable`, the pinned pool table in priority order.
//!
//! Matching (R-POOL-2), per block, first rule that hits:
//! 1. payout script: the first pool (table order) owning a script equal to
//!    the coinbase's largest-value output script (ties: the first such
//!    output). Only the largest output counts, so a coinbase that pays many
//!    parties (e.g. a pool paying its miners directly) is never credited to
//!    a listed pool because one small output is that pool's address
//!    (POOL-R1-F3);
//! 2. tag: the pool whose tag occurs EARLIEST in the coinbase `scriptSig`
//!    (byte substring; ties at one offset: the longer tag, then table
//!    order). A pool writes its own tag first, ahead of any text it lets a
//!    miner add, so a miner on a template-sharing pool (e.g. Ocean) who puts
//!    another pool's tag in their own text does not move the block to that
//!    pool (POOL-R2-F3);
//! 3. otherwise [`UNKNOWN_POOL`].
//!
//! Output: one `PoolBlocksRow` per name with at least one block, keyed by the
//! name's UTF-8 bytes (ascending). Coinbase bytes are never copied into the
//! output: a row names a table pool or `unknown`, nothing else (R-POOL-3).
//! A range always has at least one block, so the output is never empty.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use onyx_pool_schema::{CoinbaseSource, PoolBlocksRow, PoolTable, TxOut, UNKNOWN_POOL};
use onyx_recipe_sdk::{OutputRow, RecipeInput, RecipeOutput};

/// Recipe failure codes (committed as the canonical failure leaf).
pub mod code {
    /// `source` is not a canonical `CoinbaseSource`.
    pub const BAD_SOURCE: u32 = 1;
    /// A label set was given (this metric reads none).
    pub const BAD_LABELS: u32 = 2;
    /// `params` is not a valid `PoolTable` (see [`super::table_ok`]).
    pub const BAD_PARAMS: u32 = 5;
    /// Arithmetic overflow (cannot happen: at most one count per block).
    pub const OVERFLOW: u32 = 7;
}

/// Longest pool name the table may carry (bytes).
pub const MAX_NAME: usize = 64;

/// The table is canonical and well formed: it re-encodes to the same bytes;
/// at least one pool; every name non-empty, at most [`MAX_NAME`] bytes,
/// printable ASCII, unique and not [`UNKNOWN_POOL`]; every tag and script
/// non-empty (an empty tag would match every block).
pub fn table_ok(bytes: &[u8], t: &PoolTable) -> bool {
    if borsh::to_vec(t).ok().as_deref() != Some(bytes) || t.pools.is_empty() {
        return false;
    }
    let mut names = BTreeSet::new();
    t.pools.iter().all(|p| {
        !p.name.is_empty()
            && p.name.len() <= MAX_NAME
            && p.name.bytes().all(|b| (0x20..0x7f).contains(&b))
            && p.name != UNKNOWN_POOL
            && names.insert(p.name.as_str())
            && p.tags.iter().all(|t| !t.is_empty())
            && p.scripts.iter().all(|s| !s.is_empty())
    })
}

/// The first offset of `needle` in `hay` (byte substring; `needle` non-empty).
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The coinbase output that carries the block's payout: the largest value,
/// the first one on ties; `None` if every output is zero (e.g. commitments).
pub fn payout_output(outputs: &[TxOut]) -> Option<&[u8]> {
    let mut best: Option<&TxOut> = None;
    for o in outputs {
        if o.value > 0 && best.is_none_or(|b| o.value > b.value) {
            best = Some(o);
        }
    }
    best.map(|o| o.script.as_slice())
}

/// The pool a block is attributed to (R-POOL-2), as an index into the table
/// or `None` for unknown.
pub fn attribute(t: &PoolTable, script_sig: &[u8], outputs: &[TxOut]) -> Option<usize> {
    let payout = payout_output(outputs);
    payout
        .and_then(|o| {
            t.pools
                .iter()
                .position(|p| p.scripts.iter().any(|s| s == o))
        })
        .or_else(|| {
            // (offset, longer tag first, table order): the smallest wins.
            let mut best: Option<(usize, core::cmp::Reverse<usize>, usize)> = None;
            for (i, p) in t.pools.iter().enumerate() {
                for tag in &p.tags {
                    if let Some(at) = find(script_sig, tag) {
                        let key = (at, core::cmp::Reverse(tag.len()), i);
                        if best.is_none_or(|b| key < b) {
                            best = Some(key);
                        }
                    }
                }
            }
            best.map(|(_, _, i)| i)
        })
}

/// The recipe. Pure function of its input.
pub fn run(input: RecipeInput) -> Result<RecipeOutput, u32> {
    let source: CoinbaseSource = borsh::from_slice(&input.source).map_err(|_| code::BAD_SOURCE)?;
    if !source.is_canonical() {
        return Err(code::BAD_SOURCE);
    }
    if !input.labels.is_empty() {
        return Err(code::BAD_LABELS);
    }
    let table: PoolTable = borsh::from_slice(&input.params).map_err(|_| code::BAD_PARAMS)?;
    if !table_ok(&input.params, &table) {
        return Err(code::BAD_PARAMS);
    }

    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for block in &source.blocks {
        let name = match attribute(&table, &block.script_sig, &block.outputs) {
            Some(i) => table.pools[i].name.as_str(),
            None => UNKNOWN_POOL,
        };
        let c = counts.entry(name).or_default();
        *c = c.checked_add(1).ok_or(code::OVERFLOW)?;
    }

    let from_height = source
        .blocks
        .first()
        .map(|b| b.height)
        .ok_or(code::BAD_SOURCE)?;
    let to_height = source
        .blocks
        .last()
        .map(|b| b.height)
        .ok_or(code::BAD_SOURCE)?;
    let mut rows: Vec<OutputRow> = Vec::with_capacity(counts.len());
    for (name, blocks) in counts {
        let row = PoolBlocksRow {
            pool: String::from(name),
            from_height,
            to_height,
            blocks,
        };
        let value = borsh::to_vec(&row).map_err(|_| code::OVERFLOW)?;
        rows.push(OutputRow {
            key: name.as_bytes().to_vec(),
            value,
        });
    }
    Ok(RecipeOutput { rows })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use onyx_pool_schema::{CoinbaseBlock, PoolRule, COINBASE_SCHEMA_VERSION};

    fn pool(name: &str, tags: &[&[u8]], scripts: &[&[u8]]) -> PoolRule {
        PoolRule {
            name: name.into(),
            tags: tags.iter().map(|t| t.to_vec()).collect(),
            scripts: scripts.iter().map(|s| s.to_vec()).collect(),
        }
    }

    fn table(pools: Vec<PoolRule>) -> PoolTable {
        PoolTable {
            list_hash: [7; 32],
            pools,
        }
    }

    fn block(height: u64, sig: &[u8], outs: &[&[u8]]) -> CoinbaseBlock {
        let valued: Vec<(&[u8], u64)> = outs.iter().map(|s| (*s, 1)).collect();
        block_v(height, sig, &valued)
    }

    fn block_v(height: u64, sig: &[u8], outs: &[(&[u8], u64)]) -> CoinbaseBlock {
        CoinbaseBlock {
            height,
            hash: [0; 32],
            script_sig: sig.to_vec(),
            outputs: outs
                .iter()
                .map(|(s, v)| TxOut {
                    script: s.to_vec(),
                    value: *v,
                })
                .collect(),
        }
    }

    fn input(blocks: Vec<CoinbaseBlock>, t: &PoolTable) -> RecipeInput {
        RecipeInput {
            abi_version: onyx_recipe_sdk::ABI_VERSION,
            source: borsh::to_vec(&CoinbaseSource {
                schema_version: COINBASE_SCHEMA_VERSION,
                chain: "btc.mainnet".into(),
                blocks,
            })
            .expect("source"),
            labels: vec![],
            params: borsh::to_vec(t).expect("table"),
        }
    }

    fn rows(o: &RecipeOutput) -> Vec<(String, u64)> {
        o.rows
            .iter()
            .map(|r| {
                let v: PoolBlocksRow = borsh::from_slice(&r.value).expect("row");
                assert_eq!(r.key, v.pool.as_bytes());
                (v.pool, v.blocks)
            })
            .collect()
    }

    #[test]
    fn payout_script_beats_tag_and_table_order_breaks_ties() {
        let t = table(vec![
            pool("Alpha", &[b"/alpha/"], &[b"\x00\x14aa"]),
            pool("Beta", &[b"/beta/", b"/alpha/"], &[b"\x00\x14bb"]),
        ]);
        let o = run(input(
            vec![
                // Tag says beta, payout says alpha: the payout script wins.
                block(10, b"xx/beta/yy", &[b"\x00\x14aa"]),
                // Both pools carry "/alpha/": the first in table order wins.
                block(11, b"/alpha/", &[b"\x6a\x01"]),
                // Two payout scripts of equal value: the first output wins.
                block(12, b"", &[b"\x00\x14aa", b"\x00\x14bb"]),
                block(13, b"/beta/", &[]),
            ],
            &t,
        ))
        .expect("run");
        assert_eq!(rows(&o), vec![("Alpha".into(), 3), ("Beta".into(), 1)]);
    }

    /// R-POOL-3: a coinbase that matches nothing counts as unknown, and its
    /// text (here a personal handle) never reaches the output.
    #[test]
    fn unmatched_coinbase_text_is_never_output() {
        let t = table(vec![pool("Alpha", &[b"/alpha/"], &[])]);
        let o = run(input(
            vec![
                block(1, b"Mined by example-user", &[b"\x00\x14cc"]),
                block(2, b"", &[]),
                block(3, b"/ALPHA/", &[]),
            ],
            &t,
        ))
        .expect("run");
        assert_eq!(rows(&o), vec![(UNKNOWN_POOL.into(), 3)]);
        for r in &o.rows {
            assert!(find(&r.value, b"example-user").is_none());
            assert!(find(&r.key, b"example-user").is_none());
        }
    }

    /// POOL-R1-F3: a coinbase paying many parties (a pool paying its miners
    /// directly) is not credited to a listed pool whose address is one small
    /// output; only the largest output is the payout.
    #[test]
    fn a_small_output_to_a_listed_address_does_not_credit_that_pool() {
        let t = table(vec![
            pool("Listed", &[b"/listed/"], &[b"\x00\x14ll"]),
            pool("Payer", &[b"/payer/"], &[]),
        ]);
        let mut outs: Vec<(&[u8], u64)> = vec![(b"\x00\x14m0", 900_000)];
        for _ in 0..39 {
            outs.push((b"\x00\x14mx", 1_000));
        }
        outs.push((b"\x00\x14ll", 2_000));
        let o = run(input(
            vec![
                block_v(1, b"/payer/", &outs),
                // The listed pool is the largest output: credited by payout.
                block_v(2, b"", &[(b"\x00\x14ll", 5_000), (b"\x00\x14zz", 10)]),
                // Zero-value outputs (commitments) are never the payout.
                block_v(3, b"", &[(b"\x00\x14ll", 0)]),
            ],
            &t,
        ))
        .expect("run");
        assert_eq!(
            rows(&o),
            vec![
                ("Listed".into(), 1),
                ("Payer".into(), 1),
                (UNKNOWN_POOL.into(), 1)
            ]
        );
    }

    #[test]
    fn range_heights_are_on_every_row() {
        let t = table(vec![pool("Alpha", &[b"a"], &[])]);
        let o = run(input(vec![block(5, b"a", &[]), block(6, b"z", &[])], &t)).expect("run");
        for r in &o.rows {
            let v: PoolBlocksRow = borsh::from_slice(&r.value).expect("row");
            assert_eq!((v.from_height, v.to_height), (5, 6));
        }
        assert!(o.rows.windows(2).all(|w| w[0].key < w[1].key));
    }

    #[test]
    fn bad_inputs_fail_with_their_codes() {
        let good = table(vec![pool("Alpha", &[b"a"], &[])]);
        let one = || vec![block(1, b"a", &[])];
        // Labels given.
        let mut i = input(one(), &good);
        i.labels = vec![0];
        assert_eq!(run(i).err(), Some(code::BAD_LABELS));
        // Not contiguous, empty, wrong chain.
        assert_eq!(
            run(input(vec![block(1, b"", &[]), block(3, b"", &[])], &good)).err(),
            Some(code::BAD_SOURCE)
        );
        assert_eq!(run(input(vec![], &good)).err(), Some(code::BAD_SOURCE));
        // Bad tables: empty tag, empty script, duplicate name, "unknown",
        // non-printable name, empty table, trailing bytes.
        for t in [
            table(vec![pool("Alpha", &[b""], &[])]),
            table(vec![pool("Alpha", &[], &[b""])]),
            table(vec![pool("A", &[b"a"], &[]), pool("A", &[b"b"], &[])]),
            table(vec![pool(UNKNOWN_POOL, &[b"a"], &[])]),
            table(vec![pool("A\nB", &[b"a"], &[])]),
            table(vec![]),
        ] {
            assert_eq!(run(input(one(), &t)).err(), Some(code::BAD_PARAMS));
        }
        let mut i = input(one(), &good);
        i.params.push(0);
        assert_eq!(run(i).err(), Some(code::BAD_PARAMS));
    }
}
