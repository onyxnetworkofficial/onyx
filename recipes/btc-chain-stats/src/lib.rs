//! `btc-chain-stats`: per-block Bitcoin statistics from the blocks alone
//! (ADR-0043): transactions, inputs and outputs, size and weight, subsidy,
//! fees and fee rates, new supply, value moved, SegWit and Taproot use,
//! distinct active scripts and coin-blocks destroyed.
//!
//! Input (`RecipeInput`):
//! - `source`: Borsh `ChainSource` (every block of a contiguous range with
//!   each transaction's sizes, spent prevouts and created outputs, plus the
//!   parent's header);
//! - `labels`: must be empty (this metric reads no label set);
//! - `params`: must be empty (no table, no base: nothing but chain data).
//!
//! Checks (R-CHAIN-1): every block's hash is the double SHA-256 of its header
//! and its header names the previous block (the parent header for the
//! first); the coinbase, and only it, is first and spends nothing; no
//! transaction spends more than its inputs or creates an output from a later
//! block; the coinbase claims at most subsidy plus fees; sums stay within
//! 21 million BTC. Any failure ends the run with its code, never a row.
//!
//! Output (R-CHAIN-2): one `ChainStatsRow` per block, keyed by the height as
//! 8 big-endian bytes. Integers only (ADR-0009): fee rates in milli-satoshis
//! per virtual byte, rounded down; the median is the lower median.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::vec::Vec;
use onyx_chain_schema::{ChainBlock, ChainSource, ChainStatsRow};
use onyx_recipe_sdk::{OutputRow, RecipeInput, RecipeOutput};
use sha2::{Digest, Sha256};

/// Recipe failure codes (committed as the canonical failure leaf).
pub mod code {
    /// `source` is not a canonical `ChainSource`.
    pub const BAD_SOURCE: u32 = 1;
    /// A label set was given (this metric reads none).
    pub const BAD_LABELS: u32 = 2;
    /// A block hash is not its header's, or a header does not name the block before it.
    pub const BAD_HEADER: u32 = 3;
    /// A transaction breaks a block rule (coinbase position, negative fee,
    /// output spent before it exists, coinbase over subsidy plus fees, sizes).
    pub const BAD_TX: u32 = 4;
    /// `params` is not empty.
    pub const BAD_PARAMS: u32 = 5;
    /// Arithmetic overflow, or an amount above 21 million BTC.
    pub const OVERFLOW: u32 = 7;
}

/// 21 million BTC in satoshis: no amount or per-transaction sum may exceed it.
pub const MAX_MONEY: u64 = 21_000_000 * 100_000_000;

/// Block subsidy at a height (mainnet halving every 210,000 blocks).
pub fn subsidy(height: u64) -> u64 {
    let halvings = height / 210_000;
    if halvings >= 64 {
        0
    } else {
        (50 * 100_000_000u64) >> halvings
    }
}

/// Double SHA-256 (internal byte order).
pub fn sha256d(b: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(b)).into()
}

/// Header timestamp: bytes 68..72, little endian.
pub fn header_time(h: &[u8; 80]) -> u32 {
    u32::from_le_bytes([h[68], h[69], h[70], h[71]])
}

/// Header compact target (`nBits`): bytes 72..76, little endian.
pub fn header_bits(h: &[u8; 80]) -> u32 {
    u32::from_le_bytes([h[72], h[73], h[74], h[75]])
}

/// Pay-to-taproot: `OP_1 <32 bytes>` (BIP-341).
pub fn is_p2tr(script: &[u8]) -> bool {
    script.len() == 34 && script[0] == 0x51 && script[1] == 0x20
}

/// Bitcoin Core's `CScript::IsUnspendable`: starts with `OP_RETURN`, or is
/// longer than `MAX_SCRIPT_SIZE` (10,000 bytes). Such outputs are no address.
pub fn is_unspendable(script: &[u8]) -> bool {
    script.first() == Some(&0x6a) || script.len() > 10_000
}

/// Bytes of the transaction-count prefix of a serialized block (CompactSize).
pub fn compact_size_len(n: u64) -> u64 {
    match n {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

fn add(a: u64, b: u64) -> Result<u64, u32> {
    a.checked_add(b).ok_or(code::OVERFLOW)
}

/// One block's row (R-CHAIN-2), given the parent's header time. `Err` with a
/// code from [`code`] when the block breaks a rule.
pub fn block_stats(block: &ChainBlock, parent_time: u32) -> Result<ChainStatsRow, u32> {
    let height = block.height;
    let n = u64::try_from(block.txs.len()).map_err(|_| code::OVERFLOW)?;
    if n == 0 {
        return Err(code::BAD_TX);
    }
    let (mut size, mut weight) = (0u64, 0u64);
    let (mut inputs, mut outputs) = (0u64, 0u64);
    let (mut fees, mut coinbase, mut vsize_sum) = (0u64, 0u64, 0u64);
    let mut transfer: u128 = 0;
    let mut destroyed: u128 = 0;
    let (mut segwit, mut tr_in, mut tr_out) = (0u64, 0u64, 0u64);
    let mut rates: Vec<u64> = Vec::with_capacity(block.txs.len());
    let mut scripts: Vec<&[u8]> = Vec::new();
    for (k, tx) in block.txs.iter().enumerate() {
        let is_coinbase = k == 0;
        if tx.inputs.is_empty() != is_coinbase || tx.base_size == 0 || tx.total_size < tx.base_size
        {
            return Err(code::BAD_TX);
        }
        let (base, total) = (u64::from(tx.base_size), u64::from(tx.total_size));
        let tx_weight = add(base.checked_mul(3).ok_or(code::OVERFLOW)?, total)?;
        size = add(size, total)?;
        weight = add(weight, tx_weight)?;
        let mut out_sum = 0u64;
        for o in &tx.outputs {
            out_sum = add(out_sum, o.value)?;
            if is_p2tr(&o.script) {
                tr_out += 1;
            }
            if !is_unspendable(&o.script) {
                scripts.push(&o.script);
            }
        }
        if out_sum > MAX_MONEY {
            return Err(code::OVERFLOW);
        }
        outputs = add(outputs, tx.outputs.len() as u64)?;
        if is_coinbase {
            coinbase = out_sum;
            continue;
        }
        let mut in_sum = 0u64;
        for i in &tx.inputs {
            if i.created_height > height {
                return Err(code::BAD_TX);
            }
            in_sum = add(in_sum, i.value)?;
            let age = u128::from(height - i.created_height);
            destroyed = destroyed
                .checked_add(u128::from(i.value) * age)
                .ok_or(code::OVERFLOW)?;
            if is_p2tr(&i.script) {
                tr_in += 1;
            }
            if !is_unspendable(&i.script) {
                scripts.push(&i.script);
            }
        }
        if in_sum > MAX_MONEY {
            return Err(code::OVERFLOW);
        }
        let fee = in_sum.checked_sub(out_sum).ok_or(code::BAD_TX)?;
        fees = add(fees, fee)?;
        transfer = transfer
            .checked_add(u128::from(out_sum))
            .ok_or(code::OVERFLOW)?;
        inputs = add(inputs, tx.inputs.len() as u64)?;
        let vsize = tx_weight.div_ceil(4);
        vsize_sum = add(vsize_sum, vsize)?;
        // fee <= 21M BTC, so fee * 1000 fits a u64 (2.1e18 < 1.8e19).
        rates.push(fee * 1000 / vsize);
        if total > base {
            segwit += 1;
        }
    }
    if coinbase > add(subsidy(height), fees)? {
        return Err(code::BAD_TX);
    }
    let prefix = add(80, compact_size_len(n))?;
    rates.sort_unstable();
    let median = match rates.len() {
        0 => 0,
        len => rates[(len - 1) / 2],
    };
    let avg = if vsize_sum == 0 {
        0
    } else {
        // A sum of per-transaction fees: widen before scaling.
        u64::try_from(u128::from(fees) * 1000 / u128::from(vsize_sum))
            .map_err(|_| code::OVERFLOW)?
    };
    scripts.sort_unstable();
    scripts.dedup();
    let time = header_time(&block.header);
    Ok(ChainStatsRow {
        height,
        hash: block.hash,
        time,
        interval_secs: i64::from(time) - i64::from(parent_time),
        bits: header_bits(&block.header),
        txs: n,
        inputs,
        outputs,
        size_bytes: add(prefix, size)?,
        weight_wu: add(prefix.checked_mul(4).ok_or(code::OVERFLOW)?, weight)?,
        subsidy_sats: subsidy(height),
        fees_sats: fees,
        coinbase_sats: coinbase,
        // Both are at most 21M BTC each per block here, so they fit an i64.
        issued_sats: i64::try_from(coinbase).map_err(|_| code::OVERFLOW)?
            - i64::try_from(fees).map_err(|_| code::OVERFLOW)?,
        transfer_sats: transfer,
        fee_rate_median_msat_vb: median,
        fee_rate_avg_msat_vb: avg,
        segwit_txs: segwit,
        taproot_inputs: tr_in,
        taproot_outputs: tr_out,
        active_scripts: scripts.len() as u64,
        coin_blocks_destroyed: destroyed,
    })
}

/// The recipe. Pure function of its input.
pub fn run(input: RecipeInput) -> Result<RecipeOutput, u32> {
    let source: ChainSource = borsh::from_slice(&input.source).map_err(|_| code::BAD_SOURCE)?;
    if !source.is_canonical() {
        return Err(code::BAD_SOURCE);
    }
    if !input.labels.is_empty() {
        return Err(code::BAD_LABELS);
    }
    if !input.params.is_empty() {
        return Err(code::BAD_PARAMS);
    }
    let mut prev = sha256d(&source.parent_header);
    let mut prev_time = header_time(&source.parent_header);
    let mut rows = Vec::with_capacity(source.blocks.len());
    for block in &source.blocks {
        let hash = sha256d(&block.header);
        let mut display = hash;
        display.reverse();
        if display != block.hash || block.header[4..36] != prev[..] {
            return Err(code::BAD_HEADER);
        }
        let row = block_stats(block, prev_time)?;
        let value = borsh::to_vec(&row).map_err(|_| code::OVERFLOW)?;
        rows.push(OutputRow {
            key: ChainStatsRow::key(block.height),
            value,
        });
        prev = hash;
        prev_time = row.time;
    }
    Ok(RecipeOutput { rows })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use onyx_chain_schema::{ChainInput, ChainTx, TxOut, CHAIN_SCHEMA_VERSION};

    const P2TR: [u8; 34] = {
        let mut s = [7u8; 34];
        s[0] = 0x51;
        s[1] = 0x20;
        s
    };

    fn header(prev: [u8; 32], time: u32) -> [u8; 80] {
        let mut h = [0u8; 80];
        h[0] = 2;
        h[4..36].copy_from_slice(&prev);
        h[68..72].copy_from_slice(&time.to_le_bytes());
        h[72..76].copy_from_slice(&0x1702_1ef0u32.to_le_bytes());
        h
    }

    fn out(script: &[u8], value: u64) -> TxOut {
        TxOut {
            script: script.to_vec(),
            value,
        }
    }

    fn inp(script: &[u8], value: u64, created_height: u64) -> ChainInput {
        ChainInput {
            value,
            script: script.to_vec(),
            created_height,
        }
    }

    fn tx(base: u32, total: u32, inputs: Vec<ChainInput>, outputs: Vec<TxOut>) -> ChainTx {
        ChainTx {
            base_size: base,
            total_size: total,
            inputs,
            outputs,
        }
    }

    /// A chain of blocks on `parent`, each `(height, time, txs)`, with real
    /// header hashes and links.
    fn source(parent: [u8; 80], blocks: Vec<(u64, u32, Vec<ChainTx>)>) -> ChainSource {
        let mut prev = sha256d(&parent);
        let mut out = Vec::new();
        for (height, time, txs) in blocks {
            let h = header(prev, time);
            let hash = sha256d(&h);
            let mut display = hash;
            display.reverse();
            out.push(ChainBlock {
                height,
                hash: display,
                header: h,
                txs,
            });
            prev = hash;
        }
        ChainSource {
            schema_version: CHAIN_SCHEMA_VERSION,
            chain: "btc.mainnet".into(),
            parent_header: parent,
            blocks: out,
        }
    }

    fn input(s: &ChainSource) -> RecipeInput {
        RecipeInput {
            abi_version: onyx_recipe_sdk::ABI_VERSION,
            source: borsh::to_vec(s).expect("source"),
            labels: vec![],
            params: vec![],
        }
    }

    fn rows(o: &RecipeOutput) -> Vec<ChainStatsRow> {
        o.rows
            .iter()
            .map(|r| {
                let v: ChainStatsRow = borsh::from_slice(&r.value).expect("row");
                assert_eq!(r.key, ChainStatsRow::key(v.height));
                v
            })
            .collect()
    }

    const H: u64 = 970_000;
    const SUB: u64 = 312_500_000;

    /// Coinbase (subsidy + fees, a P2TR payout and an OP_RETURN commitment),
    /// a legacy tx (fee 1,000 on 250 vB) and a SegWit tx spending a P2TR
    /// output (fee 2,000 on 150 vB).
    fn sample_block() -> Vec<ChainTx> {
        vec![
            tx(
                200,
                236,
                vec![],
                vec![out(&P2TR, SUB + 3_000), out(b"\x6a\x24commitment", 0)],
            ),
            tx(
                250,
                250,
                vec![inp(b"\x76\xa9alice", 10_000, H - 10)],
                vec![out(b"\x00\x14bob", 6_000), out(b"\x76\xa9alice", 3_000)],
            ),
            tx(
                100,
                300,
                vec![inp(&P2TR, 50_000, H - 1)],
                vec![out(b"\x00\x14bob", 48_000)],
            ),
        ]
    }

    #[test]
    fn every_field_of_a_block_is_computed_by_its_rule() {
        let parent = header([9; 32], 1_000_000);
        let s = source(parent, vec![(H, 1_000_600, sample_block())]);
        let r = rows(&run(input(&s)).expect("run"));
        assert_eq!(r.len(), 1);
        let r = &r[0];
        assert_eq!(r.height, H);
        assert_eq!(r.hash, s.blocks[0].hash);
        assert_eq!(r.time, 1_000_600);
        assert_eq!(r.interval_secs, 600);
        assert_eq!(r.bits, 0x1702_1ef0);
        assert_eq!((r.txs, r.inputs, r.outputs), (3, 2, 5));
        // 80-byte header + 1-byte count + 236 + 250 + 300.
        assert_eq!(r.size_bytes, 80 + 1 + 236 + 250 + 300);
        // Header and count at 4 WU a byte; each tx base*3 + total.
        assert_eq!(
            r.weight_wu,
            4 * 81 + (200 * 3 + 236) + (250 * 3 + 250) + (100 * 3 + 300)
        );
        assert_eq!(r.subsidy_sats, SUB);
        assert_eq!(r.fees_sats, 3_000);
        assert_eq!(r.coinbase_sats, SUB + 3_000);
        assert_eq!(r.issued_sats, SUB as i64);
        assert_eq!(r.transfer_sats, 6_000 + 3_000 + 48_000);
        // Rates: 1,000 sat / 250 vB = 4,000 msat/vB; 2,000 / 150 = 13,333.
        assert_eq!(r.fee_rate_median_msat_vb, 4_000);
        assert_eq!(r.fee_rate_avg_msat_vb, 3_000 * 1000 / 400);
        assert_eq!(r.segwit_txs, 1);
        assert_eq!((r.taproot_inputs, r.taproot_outputs), (1, 1));
        // P2TR, alice, bob: the OP_RETURN is no address; repeats count once.
        assert_eq!(r.active_scripts, 3);
        assert_eq!(r.coin_blocks_destroyed, 10_000 * 10 + 50_000);
    }

    /// P3A-R2: a script that is only spent from (no output pays it) is active too,
    /// and one spent and paid in the same block counts once.
    #[test]
    fn scripts_spent_from_count_as_active() {
        let txs = vec![
            tx(100, 100, vec![], vec![out(b"\x51", SUB + 2)]),
            tx(
                100,
                100,
                vec![
                    inp(b"\x00\x14only-spent", 10, H - 1),
                    inp(b"\x51", 10, H - 1),
                ],
                vec![out(b"\x00\x14paid", 18)],
            ),
        ];
        let parent = header([1; 32], 0);
        let r = rows(&run(input(&source(parent, vec![(H, 600, txs)]))).expect("run"));
        // 0x51 (paid by the coinbase and spent), only-spent, paid.
        assert_eq!(r[0].active_scripts, 3);
    }

    #[test]
    fn rates_round_down_and_the_median_is_the_lower_one() {
        // vsize is weight / 4 rounded up: 401 WU -> 101 vB.
        let cb = tx(100, 100, vec![], vec![out(b"\x51", SUB)]);
        let t = |fee: u64, weight_base: u32| {
            tx(
                weight_base,
                weight_base + 1,
                vec![inp(b"\x51", 100_000 + fee, H - 1)],
                vec![out(b"\x51", 100_000)],
            )
        };
        // Four txs: base 100, total 101 -> 401 WU -> 101 vB.
        let txs = vec![cb, t(101, 100), t(1_010, 100), t(303, 100), t(202, 100)];
        let parent = header([1; 32], 10);
        let r = rows(&run(input(&source(parent, vec![(H, 5, txs)]))).expect("run"));
        // Rates 1,000 / 10,000 / 3,000 / 2,000 msat/vB: lower median 2,000.
        assert_eq!(r[0].fee_rate_median_msat_vb, 2_000);
        assert_eq!(r[0].fee_rate_avg_msat_vb, 1_616 * 1000 / 404);
        assert_eq!(
            r[0].interval_secs, -5,
            "a header may be older than its parent"
        );
        assert_eq!(r[0].segwit_txs, 4);
        assert_eq!(r[0].coin_blocks_destroyed, (400_000 + 1_616) as u128);
    }

    /// P3A-R1-7: a rate that does not divide exactly rounds down; a vsize rounds up.
    #[test]
    fn inexact_rates_round_down() {
        // Weight 9 (base 2, total 3) is 3 vB; a 1-sat fee is 333 msat/vB.
        let txs = vec![
            tx(100, 100, vec![], vec![out(b"\x51", SUB)]),
            tx(2, 3, vec![inp(b"\x51", 101, H)], vec![out(b"\x51", 100)]),
        ];
        let parent = header([1; 32], 0);
        let r = rows(&run(input(&source(parent, vec![(H, 1, txs)]))).expect("run"));
        assert_eq!(r[0].fee_rate_median_msat_vb, 333);
        assert_eq!(r[0].fee_rate_avg_msat_vb, 333);
    }

    #[test]
    fn the_count_prefix_grows_past_252_transactions() {
        let mut txs = vec![tx(100, 100, vec![], vec![out(b"\x51", SUB)])];
        for _ in 0..252 {
            txs.push(tx(60, 60, vec![inp(b"\x51", 5, H)], vec![out(b"\x51", 5)]));
        }
        let parent = header([1; 32], 0);
        let r = rows(&run(input(&source(parent, vec![(H, 0, txs)]))).expect("run"));
        assert_eq!(r[0].txs, 253);
        assert_eq!(r[0].size_bytes, 80 + 3 + 100 + 252 * 60);
        assert_eq!(r[0].weight_wu, 4 * 83 + 400 + 252 * 240);
        assert_eq!(r[0].active_scripts, 1);
        assert_eq!(r[0].fee_rate_median_msat_vb, 0);
        assert_eq!(compact_size_len(0xffff), 3);
        assert_eq!(compact_size_len(0x1_0000), 5);
        assert_eq!(compact_size_len(u64::MAX), 9);
    }

    #[test]
    fn a_miner_claiming_less_than_the_fees_issues_negative_supply() {
        let txs = vec![
            tx(100, 100, vec![], vec![out(b"\x51", 100)]),
            tx(100, 100, vec![inp(b"\x51", 1_000, H - 1)], vec![]),
        ];
        let parent = header([1; 32], 0);
        let r = rows(&run(input(&source(parent, vec![(H, 600, txs)]))).expect("run"));
        assert_eq!(r[0].issued_sats, 100 - 1_000);
        assert_eq!(r[0].transfer_sats, 0);
    }

    #[test]
    fn consecutive_blocks_link_and_take_the_previous_time() {
        let parent = header([1; 32], 100);
        let s = source(
            parent,
            vec![(H, 700, sample_block()), (H + 1, 1_000, sample_block())],
        );
        let r = rows(&run(input(&s)).expect("run"));
        assert_eq!(
            r.iter()
                .map(|x| (x.height, x.interval_secs))
                .collect::<Vec<_>>(),
            vec![(H, 600), (H + 1, 300)]
        );
        // Block H+1 spends outputs created 11 and 2 blocks back.
        assert_eq!(r[1].coin_blocks_destroyed, 10_000 * 11 + 50_000 * 2);
    }

    /// R-CHAIN-1: a hash that is not the header's, or a broken link (to the
    /// parent or inside the range), ends the run.
    #[test]
    fn headers_must_hash_and_link() {
        let parent = header([1; 32], 100);
        let good = source(
            parent,
            vec![(H, 700, sample_block()), (H + 1, 900, sample_block())],
        );
        let mut s = good.clone();
        s.blocks[0].hash[0] ^= 1;
        assert_eq!(run(input(&s)).err(), Some(code::BAD_HEADER));
        let mut s = good.clone();
        s.parent_header[0] ^= 1;
        assert_eq!(run(input(&s)).err(), Some(code::BAD_HEADER));
        // Block H+1 relinked to another parent, with its own hash recomputed.
        let mut s = good.clone();
        s.blocks[1].header[4] ^= 1;
        let mut d = sha256d(&s.blocks[1].header);
        d.reverse();
        s.blocks[1].hash = d;
        assert_eq!(run(input(&s)).err(), Some(code::BAD_HEADER));
    }

    /// R-CHAIN-1: block rules the recipe enforces itself.
    #[test]
    fn transactions_that_break_block_rules_end_the_run() {
        let parent = header([1; 32], 100);
        let run_with = |edit: &dyn Fn(&mut Vec<ChainTx>)| {
            let mut txs = sample_block();
            edit(&mut txs);
            run(input(&source(parent, vec![(H, 700, txs)]))).err()
        };
        assert_eq!(run_with(&|_| {}), None);
        // Negative fee.
        assert_eq!(
            run_with(&|t| t[1].outputs[0].value = 9_000),
            Some(code::BAD_TX)
        );
        // Coinbase over subsidy plus fees.
        assert_eq!(
            run_with(&|t| t[0].outputs[0].value += 1),
            Some(code::BAD_TX)
        );
        // Spends an output from a later block.
        assert_eq!(
            run_with(&|t| t[1].inputs[0].created_height = H + 1),
            Some(code::BAD_TX)
        );
        // Coinbase with an input; a non-coinbase without one.
        assert_eq!(
            run_with(&|t| t[0].inputs.push(inp(b"x", 1, 1))),
            Some(code::BAD_TX)
        );
        assert_eq!(run_with(&|t| t[2].inputs.clear()), Some(code::BAD_TX));
        // Sizes: witness size below the base, zero base.
        assert_eq!(run_with(&|t| t[2].total_size = 99), Some(code::BAD_TX));
        assert_eq!(run_with(&|t| t[1].base_size = 0), Some(code::BAD_TX));
        // No transactions at all.
        assert_eq!(run_with(&|t| t.clear()), Some(code::BAD_TX));
        // Amounts above 21M BTC.
        assert_eq!(
            run_with(&|t| {
                t[1].inputs[0].value = MAX_MONEY + 1;
                t[1].outputs[0].value = MAX_MONEY;
            }),
            Some(code::OVERFLOW)
        );
        assert_eq!(
            run_with(&|t| t[1].inputs[0].value = u64::MAX),
            Some(code::OVERFLOW)
        );
    }

    #[test]
    fn bad_inputs_fail_with_their_codes() {
        let parent = header([1; 32], 100);
        let good = source(parent, vec![(H, 700, sample_block())]);
        let mut i = input(&good);
        i.labels = vec![0];
        assert_eq!(run(i).err(), Some(code::BAD_LABELS));
        let mut i = input(&good);
        i.params = vec![0];
        assert_eq!(run(i).err(), Some(code::BAD_PARAMS));
        let mut i = input(&good);
        i.source.push(0);
        assert_eq!(run(i).err(), Some(code::BAD_SOURCE));
        let mut s = good.clone();
        s.chain = "btc.testnet".into();
        assert_eq!(run(input(&s)).err(), Some(code::BAD_SOURCE));
        let gap = source(
            parent,
            vec![(H, 700, sample_block()), (H + 2, 800, sample_block())],
        );
        assert_eq!(run(input(&gap)).err(), Some(code::BAD_SOURCE));
    }

    #[test]
    fn script_kinds_and_subsidy_follow_their_rules() {
        assert!(is_p2tr(&P2TR));
        assert!(!is_p2tr(&P2TR[..33]));
        // P3A-R2: exactly 34 bytes; a longer script that starts the same is not Taproot.
        let mut long = P2TR.to_vec();
        long.push(0);
        assert!(!is_p2tr(&long));
        let mut v0 = P2TR;
        v0[0] = 0x00;
        assert!(!is_p2tr(&v0));
        assert!(is_unspendable(b"\x6a"));
        assert!(is_unspendable(&[0x51; 10_001]));
        assert!(!is_unspendable(&[0x51; 10_000]));
        assert!(!is_unspendable(b""));
        assert_eq!(subsidy(0), 5_000_000_000);
        assert_eq!(subsidy(839_999), 625_000_000);
        assert_eq!(subsidy(840_000), 312_500_000);
        assert_eq!(subsidy(210_000 * 64), 0);
    }
}
