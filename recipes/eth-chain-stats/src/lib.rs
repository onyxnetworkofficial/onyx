//! `eth-chain-stats`: per-block Ethereum statistics from the blocks alone
//! (ADR-0044): transactions, failed transactions, contract creations, gas
//! used and limit, base fee, ETH burned, priority fees, blob gas, blob base
//! fee and blob fees, value sent.
//!
//! Input (`RecipeInput`):
//! - `source`: Borsh `EthChainSource` (every block of a contiguous range as
//!   its header RLP, transaction envelopes and receipt envelopes, plus the
//!   parent's header RLP);
//! - `labels`: must be empty (this metric reads no label set);
//! - `params`: Borsh `EthStatsParams` (from, to, tip): the range and last
//!   block hash the shard's snapshot pins; the source must be exactly those
//!   blocks (P4A-R1-1).
//!
//! Checks (R-ETH-1): each block's hash is the Keccak-256 of its header, each
//! header names the block before it (the parent header for the first) and
//! carries its number, a later time, the EIP-1559 base fee and a gas limit
//! within 1/1024 of the parent's; the transactions and the receipts hash to
//! the header's trie roots; every transaction decodes, pays at least the
//! base fee and matches its receipt's type; cumulative gas rises with every
//! receipt and ends at the header's gas used; blob hashes add up to the
//! header's blob gas. Any failure ends the run with its code, never a row.
//!
//! Output (R-ETH-2): one `EthChainStatsRow` per block, keyed by the number
//! as 8 big-endian bytes. Integers only (ADR-0009): wei as `u128`, fullness
//! in parts per million rounded down.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::vec::Vec;
use onyx_eth_schema::decode::{self, Fees, Header, GAS_PER_BLOB};
use onyx_eth_schema::{
    keccak256, trie, EthBlock, EthChainSource, EthChainStatsRow, EthStatsParams,
};
use onyx_recipe_sdk::{OutputRow, RecipeInput, RecipeOutput};

/// Recipe failure codes (committed as the canonical failure leaf).
pub mod code {
    /// `source` is not a canonical `EthChainSource`.
    pub const BAD_SOURCE: u32 = 1;
    /// A label set was given (this metric reads none).
    pub const BAD_LABELS: u32 = 2;
    /// A header that does not decode, hash to its block, link to its parent,
    /// carry its number, a later time, the EIP-1559 base fee, a gas limit
    /// within bounds or gas used within the limit.
    pub const BAD_HEADER: u32 = 3;
    /// A transaction that does not decode, pays below the base fee, or has a
    /// fee cap below its priority-fee cap.
    pub const BAD_TX: u32 = 4;
    /// `params` is not an `EthStatsParams`, or names another range or tip than the source's.
    pub const BAD_PARAMS: u32 = 5;
    /// The transactions or receipts do not hash to the header's trie root.
    pub const BAD_ROOT: u32 = 6;
    /// Arithmetic overflow.
    pub const OVERFLOW: u32 = 7;
    /// A receipt that does not decode, has another type than its
    /// transaction, gas that does not rise or does not end at the header's
    /// gas used, or blobs that do not add up to the header's blob gas.
    pub const BAD_RECEIPT: u32 = 8;
    /// A block outside the fee rules this recipe pins: before
    /// [`super::MIN_TIME`] or from [`super::MAX_TIME`] on, or without the blob fields.
    pub const UNSUPPORTED: u32 = 9;
}

/// Blob base fee update fraction of the fork schedule in force for every
/// block this recipe accepts: BPO2 (EIP-7892, active on mainnet since
/// 2026-01-07, target 14 and maximum 21 blobs). A later fork that changes it
/// needs a new recipe version (a new id); the adapter compares the result
/// with each blob receipt's `blobGasPrice` and refuses a difference.
pub const BLOB_UPDATE_FRACTION: u128 = decode::BPO2_BLOB_UPDATE_FRACTION;

/// EIP-4844 minimum blob base fee, wei.
pub const MIN_BLOB_BASE_FEE: u128 = decode::MIN_BLOB_BASE_FEE;

/// Earliest header time this recipe accepts: 2026-02-01 00:00:00 UTC, safely
/// after BPO2's activation. Earlier blocks ran under other blob parameters.
pub const MIN_TIME: u64 = 1_769_904_000;

/// First header time this recipe refuses: 2027-01-01 00:00:00 UTC (P4A-R1-2). A blob-parameter
/// fork after BPO2 would change the blob fee; the recipe would only notice through the excess
/// blob gas rule, so blocks from 2027 need this recipe reviewed against the fork schedule and
/// released under a new id.
pub const MAX_TIME: u64 = 1_798_761_600;

fn add(a: u128, b: u128) -> Result<u128, u32> {
    a.checked_add(b).ok_or(code::OVERFLOW)
}

/// Check one block against its parent header and compute its row (R-ETH-1,
/// R-ETH-2). `Err` with a code from [`code`] when the block breaks a rule.
pub fn block_stats(
    block: &EthBlock,
    parent: &Header,
    parent_hash: &[u8; 32],
) -> Result<(EthChainStatsRow, Header), u32> {
    let h = decode::header(&block.header).map_err(|_| code::BAD_HEADER)?;
    if keccak256(&block.header) != block.hash
        || h.parent_hash != *parent_hash
        || h.number != block.number
        || parent.number.checked_add(1) != Some(h.number)
        || h.timestamp <= parent.timestamp
        || h.gas_used > h.gas_limit
        || !decode::gas_limit_ok(parent.gas_limit, h.gas_limit)
        || decode::next_base_fee(parent) != Some(h.base_fee)
        || (h.excess_blob_gas.is_some()
            && decode::next_excess_blob_gas(parent) != h.excess_blob_gas)
    {
        return Err(code::BAD_HEADER);
    }
    let (Some(blob_gas_used), Some(excess_blob_gas)) = (h.blob_gas_used, h.excess_blob_gas) else {
        return Err(code::UNSUPPORTED);
    };
    if h.timestamp < MIN_TIME || h.timestamp >= MAX_TIME {
        return Err(code::UNSUPPORTED);
    }
    if block.txs.len() != block.receipts.len() {
        return Err(code::BAD_SOURCE);
    }
    if trie::ordered_root(&block.txs) != h.transactions_root
        || trie::ordered_root(&block.receipts) != h.receipts_root
    {
        return Err(code::BAD_ROOT);
    }
    let base = h.base_fee;
    let (mut failed, mut creations, mut blob_txs, mut blobs) = (0u64, 0u64, 0u64, 0u64);
    let (mut priority, mut value) = (0u128, 0u128);
    let mut cum = 0u64;
    for (env, rec) in block.txs.iter().zip(&block.receipts) {
        let t = decode::tx(env).map_err(|_| code::BAD_TX)?;
        let r = decode::receipt(rec).map_err(|_| code::BAD_RECEIPT)?;
        if r.tx_type != t.tx_type || r.cumulative_gas <= cum {
            return Err(code::BAD_RECEIPT);
        }
        let gas = r.cumulative_gas - cum;
        cum = r.cumulative_gas;
        if gas > t.gas {
            return Err(code::BAD_RECEIPT);
        }
        let tip = match t.fees {
            Fees::GasPrice(p) => p.checked_sub(base).ok_or(code::BAD_TX)?,
            Fees::Dynamic {
                max_fee,
                max_priority,
            } => {
                if max_fee < max_priority {
                    return Err(code::BAD_TX);
                }
                max_fee
                    .checked_sub(base)
                    .ok_or(code::BAD_TX)?
                    .min(max_priority)
            }
        };
        priority = add(
            priority,
            tip.checked_mul(u128::from(gas)).ok_or(code::OVERFLOW)?,
        )?;
        // A reverted transaction sends no ether and creates no contract (its gas is still paid):
        // value and creations count successful transactions only (P4A-R2-1).
        if r.success {
            value = add(value, t.value)?;
            creations += u64::from(t.creates);
        }
        failed += u64::from(!r.success);
        if t.blobs > 0 {
            blob_txs += 1;
            blobs = blobs.checked_add(t.blobs).ok_or(code::OVERFLOW)?;
        }
    }
    if cum != h.gas_used
        || blobs.checked_mul(GAS_PER_BLOB) != Some(blob_gas_used)
        || blobs > decode::BPO2_MAX_BLOBS
    {
        return Err(code::BAD_RECEIPT);
    }
    let blob_base_fee = decode::fake_exponential(
        MIN_BLOB_BASE_FEE,
        u128::from(excess_blob_gas),
        BLOB_UPDATE_FRACTION,
    )
    .ok_or(code::OVERFLOW)?;
    let row = EthChainStatsRow {
        number: h.number,
        hash: block.hash,
        time: h.timestamp,
        interval_secs: h.timestamp - parent.timestamp,
        txs: block.txs.len() as u64,
        failed_txs: failed,
        contract_creations: creations,
        gas_used: h.gas_used,
        gas_limit: h.gas_limit,
        fullness_ppm: decode::fullness_ppm(h.gas_used, h.gas_limit).ok_or(code::OVERFLOW)?,
        base_fee_wei: base,
        burned_wei: base
            .checked_mul(u128::from(h.gas_used))
            .ok_or(code::OVERFLOW)?,
        priority_fees_wei: priority,
        blob_txs,
        blob_gas_used,
        excess_blob_gas,
        blob_base_fee_wei: blob_base_fee,
        blob_fee_wei: blob_base_fee
            .checked_mul(u128::from(blob_gas_used))
            .ok_or(code::OVERFLOW)?,
        value_wei: value,
    };
    Ok((row, h))
}

/// The recipe. Pure function of its input.
pub fn run(input: RecipeInput) -> Result<RecipeOutput, u32> {
    let source: EthChainSource = borsh::from_slice(&input.source).map_err(|_| code::BAD_SOURCE)?;
    if !source.is_canonical() {
        return Err(code::BAD_SOURCE);
    }
    if !input.labels.is_empty() {
        return Err(code::BAD_LABELS);
    }
    let params: EthStatsParams = borsh::from_slice(&input.params).map_err(|_| code::BAD_PARAMS)?;
    let (first, last) = match (source.blocks.first(), source.blocks.last()) {
        (Some(f), Some(l)) => (f, l),
        _ => return Err(code::BAD_SOURCE),
    };
    if first.number != params.from || last.number != params.to || last.hash != params.tip {
        return Err(code::BAD_PARAMS);
    }
    let mut parent = decode::header(&source.parent_header).map_err(|_| code::BAD_HEADER)?;
    let mut parent_hash = keccak256(&source.parent_header);
    let mut rows = Vec::with_capacity(source.blocks.len());
    for block in &source.blocks {
        let (row, h) = block_stats(block, &parent, &parent_hash)?;
        let value = borsh::to_vec(&row).map_err(|_| code::OVERFLOW)?;
        rows.push(OutputRow {
            key: EthChainStatsRow::key(block.number),
            value,
        });
        parent = h;
        parent_hash = block.hash;
    }
    Ok(RecipeOutput { rows })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;
    use onyx_eth_schema::synthetic::{self, HeaderSpec, TxSpec};
    use onyx_eth_schema::ETH_SCHEMA_VERSION;

    const T0: u64 = MIN_TIME + 1_000_000;

    fn spec(number: u64, parent_hash: [u8; 32], time: u64, base_fee: u128) -> HeaderSpec {
        HeaderSpec {
            parent_hash,
            number,
            gas_limit: 60_000_000,
            gas_used: 0,
            timestamp: time,
            base_fee,
            transactions_root: [0; 32],
            receipts_root: [0; 32],
            blob: Some((0, 172_147_094)),
        }
    }

    fn t(
        tx_type: u8,
        to: Option<[u8; 20]>,
        value: u128,
        max_fee: u128,
        max_priority: u128,
        blobs: u8,
    ) -> TxSpec {
        TxSpec {
            tx_type,
            to,
            value,
            max_fee,
            max_priority,
            blobs,
        }
    }

    /// The excess blob gas the block after `parent` (its RLP) must carry (EIP-7918, BPO2).
    fn next_excess(parent: &[u8]) -> u64 {
        decode::next_excess_blob_gas(&decode::header(parent).expect("header")).expect("excess")
    }

    /// Parent excess blob gas whose child (no blobs used) has 172,147,094: 14 blobs above it.
    const PARENT_EXCESS: u64 = 172_147_094 + 14 * GAS_PER_BLOB;

    /// A parent at target (so the base fee stays) and two blocks over every transaction type.
    fn source() -> EthChainSource {
        let mut p = spec(99, [9; 32], T0, 1_000);
        p.gas_used = 30_000_000;
        p.blob = Some((0, PARENT_EXCESS));
        let parent = synthetic::header(&p);
        let a = Some([0xaa; 20]);
        let txs = vec![
            // Legacy at 1,500: tip 500 x 21,000.
            (t(0, a, 5, 1_500, 0, 0), true, 21_000),
            // Type 1, creation, failed, carrying 9 wei: tip 0 x 50,000; neither its value nor
            // its creation counts (it reverted, P4A-R2-1).
            (t(1, None, 9, 1_000, 0, 0), false, 50_000),
            // Type 2: tip min(2, 3,000 - 1,000) = 2 x 60,000.
            (t(2, a, 7, 3_000, 2, 0), true, 60_000),
            // Type 3 with two blobs: tip min(900, 1,500 - 1,000) = 500 x 21,000.
            (t(3, a, 0, 1_500, 900, 2), true, 21_000),
            // Type 4: tip 0.
            (t(4, a, 11, 1_000, 5, 0), true, 30_000),
        ];
        let mut s1 = spec(100, keccak256(&parent), T0 + 12, 1_000);
        s1.blob = Some((2 * GAS_PER_BLOB, next_excess(&parent)));
        let b1 = synthetic::block(s1, &txs);
        // Block 1 used 182,000 of 60 M: the base fee falls by (30 M - 182 k) / 30 M / 8.
        let base2 = 1_000 - 1_000 * (30_000_000 - 182_000) / 30_000_000 / 8;
        let mut s2 = spec(101, b1.hash, T0 + 36, base2);
        s2.blob = Some((0, next_excess(&b1.header)));
        let b2 = synthetic::block(s2, &[]);
        EthChainSource {
            schema_version: ETH_SCHEMA_VERSION,
            chain: onyx_eth_schema::ETH_MAINNET.to_string(),
            parent_header: parent,
            blocks: vec![b1, b2],
        }
    }

    fn params_of(src: &EthChainSource) -> Vec<u8> {
        let (f, l) = (&src.blocks[0], &src.blocks[src.blocks.len() - 1]);
        borsh::to_vec(&EthStatsParams {
            from: f.number,
            to: l.number,
            tip: l.hash,
        })
        .expect("params")
    }

    fn input(src: &EthChainSource) -> RecipeInput {
        RecipeInput {
            abi_version: onyx_recipe_sdk::ABI_VERSION,
            source: borsh::to_vec(src).expect("source"),
            labels: vec![],
            params: params_of(src),
        }
    }

    fn rows(src: &EthChainSource) -> Result<Vec<EthChainStatsRow>, u32> {
        run(input(src)).map(|o| {
            o.rows
                .iter()
                .map(|r| borsh::from_slice(&r.value).expect("row"))
                .collect()
        })
    }

    #[test]
    fn every_field_follows_its_definition() {
        let r = rows(&source()).expect("run");
        assert_eq!(r.len(), 2);
        let a = &r[0];
        assert_eq!((a.number, a.time, a.interval_secs), (100, T0 + 12, 12));
        assert_eq!((a.txs, a.failed_txs, a.contract_creations), (5, 1, 0));
        assert_eq!((a.gas_used, a.gas_limit), (182_000, 60_000_000));
        assert_eq!(a.fullness_ppm, 3_033); // 182,000 x 10^6 / 6 x 10^7 = 3,033.3
        assert_eq!(a.base_fee_wei, 1_000);
        assert_eq!(a.burned_wei, 182_000_000);
        assert_eq!(
            a.priority_fees_wei,
            500 * 21_000 + 2 * 60_000 + 500 * 21_000
        );
        assert_eq!((a.blob_txs, a.blob_gas_used), (1, 262_144));
        assert_eq!(a.blob_base_fee_wei, 2_502_320);
        assert_eq!(a.blob_fee_wei, 2_502_320 * 262_144);
        assert_eq!(
            a.value_wei,
            5 + 7 + 11,
            "the reverted transaction's 9 wei is not sent"
        );
        let b = &r[1];
        assert_eq!(
            (b.number, b.interval_secs, b.txs, b.gas_used),
            (101, 24, 0, 0)
        );
        assert_eq!(
            b.base_fee_wei,
            1_000 - 1_000 * (30_000_000 - 182_000) / 30_000_000 / 8
        );
        assert_eq!(
            (b.burned_wei, b.priority_fees_wei, b.blob_fee_wei),
            (0, 0, 0)
        );
    }

    #[test]
    fn a_broken_block_fails_with_its_code() {
        let good = source();
        let fails = |f: &dyn Fn(&mut EthChainSource)| {
            let mut s = good.clone();
            f(&mut s);
            rows(&s).err()
        };
        // A wrong hash, an unlinked parent.
        assert_eq!(fails(&|s| s.blocks[0].hash[0] ^= 1), Some(code::BAD_HEADER));
        assert_eq!(fails(&|s| s.parent_header[5] ^= 1), Some(code::BAD_HEADER));
        // A transaction swapped for another, or dropped with its receipt: the root moves.
        assert_eq!(fails(&|s| s.blocks[0].txs.swap(0, 2)), Some(code::BAD_ROOT));
        assert_eq!(
            fails(&|s| {
                s.blocks[0].txs.pop();
                s.blocks[0].receipts.pop();
            }),
            Some(code::BAD_ROOT)
        );
        // A receipt edited (status flipped): the receipts root moves.
        assert_eq!(
            fails(&|s| s.blocks[0].receipts[1][1] ^= 1),
            Some(code::BAD_ROOT)
        );
        // A transaction without its receipt: not canonical.
        assert_eq!(
            fails(&|s| {
                s.blocks[1].txs.push(vec![1]);
            }),
            Some(code::BAD_SOURCE)
        );
        // Labels or params.
        let mut i = input(&good);
        i.labels = vec![1];
        assert_eq!(run(i).err(), Some(code::BAD_LABELS));
        let mut i = input(&good);
        i.params = vec![1];
        assert_eq!(run(i).err(), Some(code::BAD_PARAMS));
        // P4A-R1-1: params naming another range or tip than the source's blocks.
        for edit in [
            |p: &mut EthStatsParams| p.from += 1,
            |p: &mut EthStatsParams| p.to -= 1,
            |p: &mut EthStatsParams| p.tip[31] ^= 1,
        ] {
            let mut i = input(&good);
            let mut p: EthStatsParams = borsh::from_slice(&i.params).expect("params");
            edit(&mut p);
            i.params = borsh::to_vec(&p).expect("params");
            assert_eq!(run(i).err(), Some(code::BAD_PARAMS));
        }
    }

    /// Re-seal a block after editing its transactions or header fields, so only the rule under test fails.
    fn reseal(
        s: &mut EthChainSource,
        parent_spec: HeaderSpec,
        b1: HeaderSpec,
        txs: &[(TxSpec, bool, u64)],
    ) {
        s.parent_header = synthetic::header(&parent_spec);
        let mut b1 = b1;
        b1.parent_hash = keccak256(&s.parent_header);
        if let (Some((used, _)), Some(_)) = (b1.blob, parent_spec.blob) {
            b1.blob = Some((used, next_excess(&s.parent_header)));
        }
        s.blocks = vec![synthetic::block(b1, txs)];
    }

    /// Seal raw envelopes under a header (roots and hash right), whatever their content.
    fn seal_raw(h: HeaderSpec, txs: Vec<Vec<u8>>, receipts: Vec<Vec<u8>>) -> EthBlock {
        let mut h = h;
        h.transactions_root = trie::ordered_root(&txs);
        h.receipts_root = trie::ordered_root(&receipts);
        let header = synthetic::header(&h);
        EthBlock {
            number: h.number,
            hash: keccak256(&header),
            header,
            txs,
            receipts,
        }
    }

    #[test]
    fn sealed_blocks_that_break_a_rule_fail_with_its_code() {
        let mut p = spec(99, [9; 32], T0, 1_000);
        p.gas_used = 30_000_000;
        let a = Some([0xaa; 20]);
        let ok_tx = (t(2, a, 1, 2_000, 1, 0), true, 21_000);
        let run_with = |parent: HeaderSpec, b: HeaderSpec, txs: &[(TxSpec, bool, u64)]| {
            let mut s = source();
            reseal(&mut s, parent, b, txs);
            rows(&s)
        };
        let b = spec(100, [0; 32], T0 + 12, 1_000);
        assert!(run_with(p, b, &[ok_tx]).is_ok());
        // Base fee not EIP-1559's.
        assert_eq!(
            run_with(p, spec(100, [0; 32], T0 + 12, 1_001), &[ok_tx]).err(),
            Some(code::BAD_HEADER)
        );
        // Time not after the parent's; another number.
        assert_eq!(
            run_with(p, spec(100, [0; 32], T0, 1_000), &[ok_tx]).err(),
            Some(code::BAD_HEADER)
        );
        assert_eq!(
            run_with(p, spec(101, [0; 32], T0 + 12, 1_000), &[ok_tx]).err(),
            Some(code::BAD_HEADER)
        );
        // Gas limit moved by 1/1024 or more.
        let mut g = b;
        g.gas_limit = 60_000_000 + 58_593;
        assert_eq!(run_with(p, g, &[ok_tx]).err(), Some(code::BAD_HEADER));
        // A fee below the base fee; a fee cap below the priority cap.
        assert_eq!(
            run_with(p, b, &[(t(0, a, 0, 999, 0, 0), true, 21_000)]).err(),
            Some(code::BAD_TX)
        );
        assert_eq!(
            run_with(p, b, &[(t(2, a, 0, 999, 0, 0), true, 21_000)]).err(),
            Some(code::BAD_TX)
        );
        assert_eq!(
            run_with(p, b, &[(t(2, a, 0, 2_000, 2_001, 0), true, 21_000)]).err(),
            Some(code::BAD_TX)
        );
        // Blobs that do not add up to the header's blob gas (header says none).
        assert_eq!(
            run_with(p, b, &[(t(3, a, 0, 2_000, 1, 1), true, 21_000)]).err(),
            Some(code::BAD_RECEIPT)
        );
        // A receipt using more gas than its transaction allows (21,000 for a legacy one here).
        assert_eq!(
            run_with(p, b, &[(t(0, a, 0, 1_000, 0, 0), true, 21_001)]).err(),
            Some(code::BAD_RECEIPT)
        );
        // Before the pinned fee schedule, or without blob fields.
        let mut early_p = p;
        early_p.timestamp = MIN_TIME - 100;
        let mut early = b;
        early.timestamp = MIN_TIME - 88;
        assert_eq!(
            run_with(early_p, early, &[ok_tx]).err(),
            Some(code::UNSUPPORTED)
        );
        let mut london = b;
        london.blob = None;
        assert_eq!(run_with(p, london, &[ok_tx]).err(), Some(code::UNSUPPORTED));
        // From 2027 on (P4A-R1-2).
        let mut late_p = p;
        late_p.timestamp = MAX_TIME - 5;
        let mut late = b;
        late.timestamp = MAX_TIME + 7;
        assert_eq!(
            run_with(late_p, late, &[ok_tx]).err(),
            Some(code::UNSUPPORTED)
        );
        // An excess blob gas that is not the parent rule's (P4A-R1-7).
        let mut s = source();
        reseal(&mut s, p, b, &[ok_tx]);
        let parent_h = s.parent_header.clone();
        let mut wrong = b;
        wrong.parent_hash = keccak256(&parent_h);
        wrong.blob = Some((0, next_excess(&parent_h) + 1));
        s.blocks = vec![synthetic::block(wrong, &[ok_tx])];
        assert_eq!(rows(&s).err(), Some(code::BAD_HEADER));
        // More than BPO2's 21 blobs.
        let mut many = b;
        many.blob = Some((22 * GAS_PER_BLOB, 0));
        let big = [
            (t(3, a, 0, 2_000, 1, 11), true, 21_000),
            (t(3, a, 0, 2_000, 1, 11), true, 21_000),
        ];
        assert_eq!(run_with(p, many, &big).err(), Some(code::BAD_RECEIPT));
        // Receipt rules (P4A-R1-5), sealed so only the rule under test fails.
        let raw = |txs: Vec<Vec<u8>>, receipts: Vec<Vec<u8>>, gas_used: u64, gas_limit: u64| {
            let mut s = source();
            reseal(&mut s, p, b, &[ok_tx]);
            let mut h = b;
            h.parent_hash = keccak256(&s.parent_header);
            h.gas_used = gas_used;
            h.gas_limit = gas_limit;
            h.blob = Some((0, next_excess(&s.parent_header)));
            s.blocks = vec![seal_raw(h, txs, receipts)];
            rows(&s).err()
        };
        let tx2 = synthetic::tx(&t(2, a, 1, 2_000, 1, 0));
        let ok = raw(
            vec![tx2.clone()],
            vec![synthetic::receipt(2, true, 21_000)],
            21_000,
            60_000_000,
        );
        assert_eq!(ok, None, "the raw sealing itself is valid");
        // A receipt of another type than its transaction.
        assert_eq!(
            raw(
                vec![tx2.clone()],
                vec![synthetic::receipt(1, true, 21_000)],
                21_000,
                60_000_000
            ),
            Some(code::BAD_RECEIPT)
        );
        // Cumulative gas that stays flat, or falls.
        let two = vec![tx2.clone(), tx2.clone()];
        assert_eq!(
            raw(
                two.clone(),
                vec![
                    synthetic::receipt(2, true, 21_000),
                    synthetic::receipt(2, true, 21_000)
                ],
                21_000,
                60_000_000
            ),
            Some(code::BAD_RECEIPT)
        );
        assert_eq!(
            raw(
                two,
                vec![
                    synthetic::receipt(2, true, 42_000),
                    synthetic::receipt(2, true, 21_000)
                ],
                21_000,
                60_000_000
            ),
            Some(code::BAD_RECEIPT)
        );
        // A last cumulative gas that is not the header's gas used.
        assert_eq!(
            raw(
                vec![tx2.clone()],
                vec![synthetic::receipt(2, true, 21_000)],
                21_001,
                60_000_000
            ),
            Some(code::BAD_RECEIPT)
        );
        // Gas used above the gas limit (the limit itself within its bound of the parent's).
        assert_eq!(
            raw(
                vec![tx2],
                vec![synthetic::receipt(2, true, 21_000)],
                60_000_001,
                60_000_000
            ),
            Some(code::BAD_HEADER)
        );
    }

    #[test]
    fn rows_do_not_depend_on_the_range() {
        // R-ETH-3: block 101 alone (its parent header given) gives the same row as in the pair.
        let s = source();
        let both = rows(&s).expect("pair");
        let mut alone = s.clone();
        alone.parent_header = s.blocks[0].header.clone();
        alone.blocks = vec![s.blocks[1].clone()];
        assert_eq!(rows(&alone).expect("alone"), vec![both[1].clone()]);
    }
}
