//! `btc-entity-reserve`: per-entity BTC balance at the end of a contiguous
//! block range, from a committed base plus the range's flows (ADR-0034).
//!
//! Input (`RecipeInput`), the same source and labels as `btc-entity-balance`:
//! - `source`: Borsh `BtcSource` (blocks with spent prevouts and created outputs);
//! - `labels`: Borsh label set (`LabelSetView`), addresses like `btc:bc1q...`;
//! - `params`: Borsh `ReserveParams` (base height, base reference, base balances).
//!
//! Output: one `EntityReserveRow` per labelled entity (also when untouched),
//! keyed by the raw UTF-8 bytes of the entity id. The rows are the next shard's
//! base, so a reserve history is a chain of shards (incremental state).
//!
//! Rules checked in the sandbox:
//! - R-RES-BASE-1: the source starts at `base_height + 1` (a base for another
//!   height is refused, `STALE_BASE`);
//! - R-RCP-BASE-1: the base names exactly the labelled entities;
//! - R-RES-NEG-1: no entity's balance goes below zero after any transaction
//!   (a base that is too low, or flows the base never saw, are refused). The
//!   check is per entity: one address short and another long by the same
//!   amount pass (the per-address check is in the base derivation);
//! - R-RES-CAP-1: the base, and the end balances, sum to at most 21 million
//!   BTC (`SUPPLY_CAP`): a base above the money supply is refused.
//!
//! Seed-address attribution only, integer satoshis only (ADR-0009). The address
//! decoder is a copy of the one in `btc-entity-balance`, kept separate so that
//! recipe's bytes and id never change; a host test checks the decoders agree.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use onyx_btc_schema::BtcSource;
use onyx_recipe_sdk::{LabelSetView, OutputRow, RecipeInput, RecipeOutput};
use onyx_reserve_schema::{EntityReserveRow, ReserveParams};
use sha2::{Digest, Sha256};

/// Recipe failure codes (committed as the canonical failure leaf). The same
/// numbers as `btc-entity-balance` where the meaning is the same.
pub mod code {
    /// `source` is not a canonical `BtcSource`.
    pub const BAD_SOURCE: u32 = 1;
    /// Label set missing, undecodable or not strictly sorted.
    pub const BAD_LABELS: u32 = 2;
    /// A label address is not a valid mainnet address.
    pub const BAD_ADDRESS: u32 = 3;
    /// Two labels map the same script to different entities.
    pub const CONFLICTING_LABELS: u32 = 4;
    /// `params` undecodable, base not strictly sorted, or base entities differ
    /// from the labelled entities (R-RCP-BASE-1).
    pub const BAD_PARAMS: u32 = 5;
    /// A balance went below zero: the base is too low for the chain (R-RES-NEG-1).
    pub const NEGATIVE_BALANCE: u32 = 6;
    /// Arithmetic overflow (cannot happen with real BTC amounts).
    pub const OVERFLOW: u32 = 7;
    /// The source does not start right after the base height (R-RES-BASE-1).
    pub const STALE_BASE: u32 = 9;
    /// The base or the end balances sum to more than 21 million BTC (R-RES-CAP-1).
    pub const ABOVE_SUPPLY: u32 = 10;
}

/// 21 million BTC in satoshis: no set of addresses can hold more (R-RES-CAP-1).
pub const SUPPLY_CAP_SATS: u128 = 2_100_000_000_000_000;

/// Sum of balances, refused above the supply cap.
fn within_supply<I: Iterator<Item = u128>>(balances: I) -> Result<(), u32> {
    let mut sum: u128 = 0;
    for b in balances {
        sum = sum.checked_add(b).ok_or(code::ABOVE_SUPPLY)?;
    }
    if sum > SUPPLY_CAP_SATS {
        return Err(code::ABOVE_SUPPLY);
    }
    Ok(())
}

const BECH32: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn polymod(values: &[u8]) -> u32 {
    const G: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut c: u32 = 1;
    for v in values {
        let b = c >> 25;
        c = ((c & 0x1ff_ffff) << 5) ^ u32::from(*v);
        for (i, g) in G.iter().enumerate() {
            if (b >> i) & 1 == 1 {
                c ^= g;
            }
        }
    }
    c
}

/// `bc1...` (mainnet, lowercase) to scriptPubKey. Bech32 for v0, Bech32m for v1+.
fn segwit_script(addr: &str) -> Option<Vec<u8>> {
    let data_part = addr.strip_prefix("bc1")?;
    if data_part.len() < 7 {
        return None;
    }
    let mut data = Vec::with_capacity(data_part.len());
    for c in data_part.bytes() {
        data.push(BECH32.iter().position(|x| *x == c)? as u8);
    }
    let mut chk = Vec::with_capacity(data.len() + 5);
    chk.extend_from_slice(&[3, 3, 0, 2, 3]); // hrp "bc" expanded
    chk.extend_from_slice(&data);
    let version = data[0];
    let want = if version == 0 { 1 } else { 0x2bc8_30a3 };
    if version > 16 || polymod(&chk) != want {
        return None;
    }
    let mut prog = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for v in &data[1..data.len() - 6] {
        acc = (acc << 5) | u32::from(*v);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            prog.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if bits >= 5 || (acc & ((1 << bits) - 1)) != 0 || !(2..=40).contains(&prog.len()) {
        return None;
    }
    if version == 0 && prog.len() != 20 && prog.len() != 32 {
        return None;
    }
    let mut script = Vec::with_capacity(prog.len() + 2);
    script.push(if version == 0 { 0 } else { 0x50 + version });
    script.push(prog.len() as u8);
    script.extend_from_slice(&prog);
    Some(script)
}

/// Base58Check `1...` (P2PKH) or `3...` (P2SH) to scriptPubKey.
fn base58_script(addr: &str) -> Option<Vec<u8>> {
    let mut n = [0u8; 25];
    for c in addr.bytes() {
        let mut carry = B58.iter().position(|x| *x == c)? as u32;
        for byte in n.iter_mut().rev() {
            carry += u32::from(*byte) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        if carry != 0 {
            return None;
        }
    }
    // Each leading '1' encodes exactly one leading zero byte; anything else is a
    // second spelling of the same payload (R-B58-1).
    let leading = addr.bytes().take_while(|c| *c == b'1').count();
    if n.iter().take_while(|b| **b == 0).count() != leading {
        return None;
    }
    let check = Sha256::digest(Sha256::digest(&n[..21]));
    if check[..4] != n[21..] {
        return None;
    }
    let mut h = [0u8; 20];
    h.copy_from_slice(&n[1..21]);
    match n[0] {
        0x00 => Some([&[0x76, 0xa9, 0x14][..], &h, &[0x88, 0xac]].concat()),
        0x05 => Some([&[0xa9, 0x14][..], &h, &[0x87]].concat()),
        _ => None,
    }
}

/// `btc:<mainnet address>` to scriptPubKey.
pub fn address_script(namespaced: &str) -> Option<Vec<u8>> {
    let addr = namespaced.strip_prefix("btc:")?;
    if addr.starts_with("bc1") {
        segwit_script(addr)
    } else {
        base58_script(addr)
    }
}

/// The recipe. Pure function of its input.
pub fn run(input: RecipeInput) -> Result<RecipeOutput, u32> {
    let source: BtcSource = borsh::from_slice(&input.source).map_err(|_| code::BAD_SOURCE)?;
    if !source.is_canonical() {
        return Err(code::BAD_SOURCE);
    }
    let labels: LabelSetView = borsh::from_slice(&input.labels).map_err(|_| code::BAD_LABELS)?;
    if !labels.is_canonical() {
        return Err(code::BAD_LABELS);
    }
    let params: ReserveParams = borsh::from_slice(&input.params).map_err(|_| code::BAD_PARAMS)?;
    if !params.is_canonical() {
        return Err(code::BAD_PARAMS);
    }

    // script -> entity index; one state per entity, in entity order.
    let mut entities: Vec<String> = labels.labels.iter().map(|l| l.entity.clone()).collect();
    entities.sort_unstable();
    entities.dedup();
    if !params
        .base
        .iter()
        .map(|b| b.entity.as_str())
        .eq(entities.iter().map(String::as_str))
    {
        return Err(code::BAD_PARAMS);
    }
    within_supply(params.base.iter().map(|b| u128::from(b.balance_sats)))?;
    let mut owner: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    for l in &labels.labels {
        let script = address_script(&l.address).ok_or(code::BAD_ADDRESS)?;
        let e = entities
            .binary_search(&l.entity)
            .map_err(|_| code::BAD_LABELS)?;
        if let Some(prev) = owner.insert(script, e) {
            if prev != e {
                return Err(code::CONFLICTING_LABELS);
            }
        }
    }
    let first = source.blocks.first().ok_or(code::BAD_SOURCE)?.height;
    if params.base_height.checked_add(1) != Some(first) {
        return Err(code::STALE_BASE);
    }

    struct State {
        balance: u128,
        received: u128,
        sent: u128,
        txs: u64,
    }
    let mut state: Vec<State> = params
        .base
        .iter()
        .map(|b| State {
            balance: u128::from(b.balance_sats),
            received: 0,
            sent: 0,
            txs: 0,
        })
        .collect();

    let mut touched: Vec<usize> = Vec::new();
    for block in &source.blocks {
        for tx in &block.txs {
            touched.clear();
            // Spends first, then outputs: a transaction can only spend outputs
            // that existed before it, so with a true base the balance always
            // covers its spends (R-RES-NEG-1). Crediting its own outputs first
            // would let a too-low base pass.
            for out in &tx.spent {
                if let Some(&e) = owner.get(&out.script) {
                    let s = state.get_mut(e).ok_or(code::OVERFLOW)?;
                    let v = u128::from(out.value);
                    s.sent = s.sent.checked_add(v).ok_or(code::OVERFLOW)?;
                    s.balance = s.balance.checked_sub(v).ok_or(code::NEGATIVE_BALANCE)?;
                    touched.push(e);
                }
            }
            for out in &tx.created {
                if let Some(&e) = owner.get(&out.script) {
                    let s = state.get_mut(e).ok_or(code::OVERFLOW)?;
                    let v = u128::from(out.value);
                    s.received = s.received.checked_add(v).ok_or(code::OVERFLOW)?;
                    s.balance = s.balance.checked_add(v).ok_or(code::OVERFLOW)?;
                    touched.push(e);
                }
            }
            touched.sort_unstable();
            touched.dedup();
            for &e in &touched {
                let s = state.get_mut(e).ok_or(code::OVERFLOW)?;
                s.txs = s.txs.checked_add(1).ok_or(code::OVERFLOW)?;
            }
        }
    }

    within_supply(state.iter().map(|s| s.balance))?;
    let height = source.blocks.last().ok_or(code::BAD_SOURCE)?.height;
    let mut rows = Vec::with_capacity(entities.len());
    for ((entity, s), b) in entities.into_iter().zip(state).zip(&params.base) {
        let row = EntityReserveRow {
            entity,
            base_height: params.base_height,
            height,
            base_ref: params.base_ref,
            base_sats: b.balance_sats,
            received_sats: s.received,
            sent_sats: s.sent,
            tx_count: s.txs,
            balance_sats: u64::try_from(s.balance).map_err(|_| code::OVERFLOW)?,
        };
        let key = row.entity.as_bytes().to_vec();
        let value = borsh::to_vec(&row).map_err(|_| code::OVERFLOW)?;
        rows.push(OutputRow { key, value });
    }
    Ok(RecipeOutput { rows })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use onyx_btc_schema::{BlockDelta, TxDelta, TxOut, SCHEMA_VERSION};
    use onyx_recipe_sdk::LabelView;
    use onyx_reserve_schema::EntityBase;

    const A: &str = "btc:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const B: &str = "btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";
    const REF: [u8; 32] = [7; 32];

    fn labels() -> Vec<LabelView> {
        let mut l = vec![
            LabelView {
                address: A.into(),
                entity: "entity:a".into(),
                kind: 0,
                confidence_ppm: 1_000_000,
                evidence: vec![],
            },
            LabelView {
                address: B.into(),
                entity: "entity:b".into(),
                kind: 0,
                confidence_ppm: 1_000_000,
                evidence: vec![],
            },
        ];
        l.sort_by(|a, b| a.address.cmp(&b.address));
        l
    }

    fn out(addr: &str, value: u64) -> TxOut {
        TxOut {
            script: address_script(addr).expect("address"),
            value,
        }
    }

    fn tx(spent: Vec<TxOut>, created: Vec<TxOut>) -> TxDelta {
        TxDelta { spent, created }
    }

    fn block(height: u64, txs: Vec<TxDelta>) -> BlockDelta {
        BlockDelta {
            height,
            hash: [0; 32],
            txs,
        }
    }

    fn params(base_height: u64, a: u64, b: u64) -> ReserveParams {
        ReserveParams {
            base_height,
            base_ref: REF,
            base: vec![
                EntityBase {
                    entity: "entity:a".into(),
                    balance_sats: a,
                },
                EntityBase {
                    entity: "entity:b".into(),
                    balance_sats: b,
                },
            ],
        }
    }

    fn input(blocks: Vec<BlockDelta>, p: &ReserveParams) -> RecipeInput {
        RecipeInput {
            abi_version: onyx_recipe_sdk::ABI_VERSION,
            source: borsh::to_vec(&BtcSource {
                schema_version: SCHEMA_VERSION,
                chain: "btc.mainnet".into(),
                blocks,
            })
            .expect("source"),
            labels: borsh::to_vec(&LabelSetView {
                name: "t".into(),
                version: 1,
                labels: labels(),
            })
            .expect("labels"),
            params: borsh::to_vec(p).expect("params"),
        }
    }

    fn rows(o: &RecipeOutput) -> Vec<EntityReserveRow> {
        o.rows
            .iter()
            .map(|r| borsh::from_slice(&r.value).expect("row"))
            .collect()
    }

    fn week() -> Vec<BlockDelta> {
        vec![
            block(11, vec![tx(vec![], vec![out(A, 500), out(B, 40)])]),
            block(12, vec![]),
            block(
                13,
                vec![
                    tx(vec![out(A, 500)], vec![out(A, 120)]),
                    tx(vec![], vec![out(A, 1)]),
                ],
            ),
        ]
    }

    #[test]
    fn balance_is_base_plus_flows_with_a_row_per_entity() {
        let o = run(input(week(), &params(10, 1_000, 0))).expect("run");
        let r = rows(&o);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (
                r[0].entity.as_str(),
                r[0].base_sats,
                r[0].received_sats,
                r[0].sent_sats
            ),
            ("entity:a", 1_000, 621, 500)
        );
        assert_eq!((r[0].balance_sats, r[0].tx_count), (1_121, 3));
        assert_eq!((r[1].balance_sats, r[1].tx_count), (40, 1));
        for row in &r {
            assert_eq!((row.base_height, row.height, row.base_ref), (10, 13, REF));
        }
        // An untouched entity still gets its row: the rows are the next base.
        let quiet = vec![block(11, vec![tx(vec![], vec![out(A, 3)])])];
        let r = rows(&run(input(quiet, &params(10, 0, 77))).expect("run"));
        assert_eq!(
            (r[1].entity.as_str(), r[1].balance_sats, r[1].tx_count),
            ("entity:b", 77, 0)
        );
        assert!(o.rows.windows(2).all(|w| w[0].key < w[1].key));
        assert_eq!(o.rows[0].key, b"entity:a".to_vec());
    }

    /// R-RES-BASE-1: a base for any height but the one before the source is refused.
    #[test]
    fn a_base_for_another_height_is_stale() {
        for h in [0, 9, 11, 12, u64::MAX] {
            assert_eq!(
                run(input(week(), &params(h, 1_000, 0))).err(),
                Some(code::STALE_BASE),
                "base height {h}"
            );
        }
    }

    /// R-RES-NEG-1: a base too low for the chain is refused, also when later
    /// inflows would hide it by the end of the range.
    #[test]
    fn a_too_low_base_is_refused_even_if_the_end_is_positive() {
        let blocks = vec![
            block(11, vec![tx(vec![out(A, 50)], vec![])]),
            block(12, vec![tx(vec![], vec![out(A, 1_000)])]),
        ];
        assert_eq!(
            run(input(blocks.clone(), &params(10, 49, 0))).err(),
            Some(code::NEGATIVE_BALANCE)
        );
        assert!(run(input(blocks, &params(10, 50, 0))).is_ok());
        // Within one transaction, its own outputs never cover its spends.
        let self_send = vec![block(11, vec![tx(vec![out(A, 10)], vec![out(A, 6)])])];
        assert_eq!(
            run(input(self_send.clone(), &params(10, 5, 0))).err(),
            Some(code::NEGATIVE_BALANCE)
        );
        let r = rows(&run(input(self_send, &params(10, 10, 0))).expect("run"));
        assert_eq!(r[0].balance_sats, 6);
    }

    /// R-RCP-BASE-1: the base names exactly the labelled entities, sorted.
    #[test]
    fn a_partial_or_unsorted_base_is_refused() {
        let mut p = params(10, 1, 2);
        p.base.pop();
        assert_eq!(run(input(week(), &p)).err(), Some(code::BAD_PARAMS));
        let mut p = params(10, 1, 2);
        p.base.reverse();
        assert_eq!(run(input(week(), &p)).err(), Some(code::BAD_PARAMS));
        let mut p = params(10, 1, 2);
        p.base.push(EntityBase {
            entity: "entity:c".into(),
            balance_sats: 0,
        });
        assert_eq!(run(input(week(), &p)).err(), Some(code::BAD_PARAMS));
        let mut i = input(week(), &params(10, 1, 2));
        i.params = vec![];
        assert_eq!(run(i).err(), Some(code::BAD_PARAMS));
    }

    /// Two chained shards (the second's base is the first's rows) end where
    /// one shard over both ranges ends: the incremental state adds up.
    #[test]
    fn chained_shards_equal_one_shard() {
        let all = week();
        let one = rows(&run(input(all.clone(), &params(10, 1_000, 5))).expect("run"));
        let first = rows(&run(input(all[..1].to_vec(), &params(10, 1_000, 5))).expect("run"));
        let next = ReserveParams {
            base_height: first[0].height,
            base_ref: [9; 32],
            base: first
                .iter()
                .map(|r| EntityBase {
                    entity: r.entity.clone(),
                    balance_sats: r.balance_sats,
                })
                .collect(),
        };
        let second = rows(&run(input(all[1..].to_vec(), &next)).expect("run"));
        for (a, b) in one.iter().zip(&second) {
            assert_eq!(
                (a.entity.as_str(), a.balance_sats),
                (b.entity.as_str(), b.balance_sats)
            );
        }
    }

    /// R-RES-CAP-1: a base summing to more than 21 million BTC is refused
    /// (review RES-R1 F2 ran a base of 21 million BTC per entity); exactly the
    /// cap passes, and end balances above the cap are refused too.
    #[test]
    fn a_base_above_the_money_supply_is_refused() {
        let cap = SUPPLY_CAP_SATS as u64;
        let quiet = vec![block(11, vec![])];
        assert_eq!(
            run(input(quiet.clone(), &params(10, cap, 1))).err(),
            Some(code::ABOVE_SUPPLY)
        );
        assert_eq!(
            run(input(quiet.clone(), &params(10, cap, cap))).err(),
            Some(code::ABOVE_SUPPLY)
        );
        assert_eq!(
            run(input(quiet.clone(), &params(10, u64::MAX, u64::MAX))).err(),
            Some(code::ABOVE_SUPPLY)
        );
        assert!(run(input(quiet, &params(10, cap - 7, 7))).is_ok());
        // At the cap, an inflow from outside the set would end above it.
        let inflow = vec![block(11, vec![tx(vec![], vec![out(B, 1)])])];
        assert_eq!(
            run(input(inflow, &params(10, cap, 0))).err(),
            Some(code::ABOVE_SUPPLY)
        );
    }

    /// R-RES-NEG-1 is per entity: one address short by N and another long by
    /// N of the same entity pass (THREATS RES-1; the per-address check is in
    /// the base derivation, `reserve::derive_base_by_address`).
    #[test]
    fn the_negative_check_is_per_entity() {
        const A2: &str = "btc:bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";
        let mut l = labels();
        l.push(LabelView {
            address: A2.into(),
            entity: "entity:a".into(),
            kind: 0,
            confidence_ppm: 1_000_000,
            evidence: vec![],
        });
        l.sort_by(|a, b| a.address.cmp(&b.address));
        // A spends 50 it never held; the base credits the 50 to entity:a as a whole.
        let blocks = vec![block(11, vec![tx(vec![out(A, 50)], vec![])])];
        let mut i = input(blocks, &params(10, 50, 0));
        i.labels = borsh::to_vec(&LabelSetView {
            name: "t".into(),
            version: 1,
            labels: l,
        })
        .expect("labels");
        assert!(run(i).is_ok());
    }

    /// Unsolicited inflows (dust, spam) count at exactly their value: on chain
    /// they are the address's coins, and raising a balance by N costs the
    /// sender N (THREATS RES-3).
    #[test]
    fn a_dust_inflow_counts_at_its_value() {
        let blocks = vec![block(11, vec![tx(vec![], vec![out(B, 1)])])];
        let r = rows(&run(input(blocks, &params(10, 0, 100))).expect("run"));
        assert_eq!((r[1].balance_sats, r[1].received_sats), (101, 1));
    }
}
