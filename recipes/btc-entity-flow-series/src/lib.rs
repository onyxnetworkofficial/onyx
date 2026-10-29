//! `btc-entity-flow-series`: per-entity BTC flows for every block of a
//! contiguous range, so a range becomes a time series anyone can rerun.
//!
//! Input (`RecipeInput`), the same shape as `btc-entity-balance`:
//! - `source`: Borsh `BtcSource` (blocks with spent prevouts and created outputs);
//! - `labels`: Borsh label set (`LabelSetView`), addresses like `btc:bc1q...`;
//! - `params`: must be empty (this recipe takes no parameters).
//!
//! Output: one `EntityBlockFlowRow` per (entity, block) where the entity was
//! touched, keyed by [`EntityBlockFlowRow::key`] so rows sort by entity, then
//! height. A block where an entity was not touched has no row: its flow is
//! zero (R-SERIES-1). A range where no entity was touched at all ends with
//! [`code::NO_ACTIVITY`] (a shard needs at least one leaf), which readers show
//! as an all-zero series. Summing an entity's rows over the range gives exactly the
//! `btc-entity-balance` row for the same range and label set (R-SERIES-2).
//!
//! Seed-address attribution only, integer satoshis only (ADR-0009). Label
//! addresses are decoded to scripts inside the sandbox, with checksums
//! verified (THREATS T-ADAPT-1). The address decoder is a copy of the one in
//! `btc-entity-balance`, kept separate so that recipe's bytes and id never
//! change; a host test checks both decoders agree on every fixture label.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use onyx_btc_schema::{BtcSource, EntityBlockFlowRow};
use onyx_recipe_sdk::{LabelSetView, OutputRow, RecipeInput, RecipeOutput};
use sha2::{Digest, Sha256};

/// Recipe failure codes (committed as the canonical failure leaf). The same
/// numbers as `btc-entity-balance` where the meaning is the same.
pub mod code {
    /// `source` is not a canonical `BtcSource`.
    pub const BAD_SOURCE: u32 = 1;
    /// Label set missing, undecodable, not strictly sorted, or an entity id
    /// containing a zero byte (it would break the row key order).
    pub const BAD_LABELS: u32 = 2;
    /// A label address is not a valid mainnet address.
    pub const BAD_ADDRESS: u32 = 3;
    /// Two labels map the same script to different entities.
    pub const CONFLICTING_LABELS: u32 = 4;
    /// `params` is not empty.
    pub const BAD_PARAMS: u32 = 5;
    /// Arithmetic overflow (cannot happen with real BTC amounts).
    pub const OVERFLOW: u32 = 7;
    /// No labelled entity was touched anywhere in the range: there is no row
    /// to commit, and a shard needs at least one leaf. Not an error in the
    /// data: readers show it as an all-zero series (R-SERIES-1, SER-R1-F1).
    pub const NO_ACTIVITY: u32 = onyx_btc_schema::SERIES_NO_ACTIVITY;
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

#[derive(Default)]
struct Acc {
    received: u128,
    sent: u128,
    txs: u64,
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
    if !input.params.is_empty() {
        return Err(code::BAD_PARAMS);
    }

    let mut owner: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    for l in &labels.labels {
        // A zero byte would let "a\0..." keys interleave with "a" keys (R-KEY-ORD-1).
        if l.entity.as_bytes().contains(&0) {
            return Err(code::BAD_LABELS);
        }
        let script = address_script(&l.address).ok_or(code::BAD_ADDRESS)?;
        if let Some(prev) = owner.insert(script, l.entity.clone()) {
            if prev != l.entity {
                return Err(code::CONFLICTING_LABELS);
            }
        }
    }

    // (entity, height) -> flows. BTreeMap: deterministic order, which is also
    // the row key order (EntityBlockFlowRow::key).
    let mut acc: BTreeMap<(String, u64), Acc> = BTreeMap::new();
    for block in &source.blocks {
        for tx in &block.txs {
            let mut touched: Vec<&String> = Vec::new();
            for out in &tx.spent {
                if let Some(e) = owner.get(&out.script) {
                    let a = acc.entry((e.clone(), block.height)).or_default();
                    a.sent = a
                        .sent
                        .checked_add(u128::from(out.value))
                        .ok_or(code::OVERFLOW)?;
                    touched.push(e);
                }
            }
            for out in &tx.created {
                if let Some(e) = owner.get(&out.script) {
                    let a = acc.entry((e.clone(), block.height)).or_default();
                    a.received = a
                        .received
                        .checked_add(u128::from(out.value))
                        .ok_or(code::OVERFLOW)?;
                    touched.push(e);
                }
            }
            touched.sort_unstable();
            touched.dedup();
            for e in touched {
                let a = acc
                    .get_mut(&(e.clone(), block.height))
                    .ok_or(code::OVERFLOW)?;
                a.txs = a.txs.checked_add(1).ok_or(code::OVERFLOW)?;
            }
        }
    }

    let mut rows: Vec<OutputRow> = Vec::with_capacity(acc.len());
    for ((entity, height), a) in acc {
        let net = i128::try_from(a.received).map_err(|_| code::OVERFLOW)?
            - i128::try_from(a.sent).map_err(|_| code::OVERFLOW)?;
        let key = EntityBlockFlowRow::key(&entity, height);
        let row = EntityBlockFlowRow {
            entity,
            height,
            received_sats: a.received,
            sent_sats: a.sent,
            net_sats: net,
            tx_count: a.txs,
        };
        let value = borsh::to_vec(&row).map_err(|_| code::OVERFLOW)?;
        rows.push(OutputRow { key, value });
    }
    if rows.is_empty() {
        return Err(code::NO_ACTIVITY);
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

    const A: &str = "btc:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const B: &str = "btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";

    fn label(address: &str, entity: &str) -> LabelView {
        LabelView {
            address: address.into(),
            entity: entity.into(),
            kind: 0,
            confidence_ppm: 1_000_000,
            evidence: vec![],
        }
    }

    fn out(addr: &str, value: u64) -> TxOut {
        TxOut {
            script: address_script(addr).expect("address"),
            value,
        }
    }

    fn input(blocks: Vec<BlockDelta>, mut labels: Vec<LabelView>, params: Vec<u8>) -> RecipeInput {
        labels.sort_by(|a, b| a.address.cmp(&b.address));
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
                labels,
            })
            .expect("labels"),
            params,
        }
    }

    fn decode(o: &RecipeOutput) -> Vec<EntityBlockFlowRow> {
        o.rows
            .iter()
            .map(|r| borsh::from_slice(&r.value).expect("row"))
            .collect()
    }

    #[test]
    fn one_row_per_touched_entity_and_block_in_key_order() {
        let blocks = vec![
            BlockDelta {
                height: 7,
                hash: [0; 32],
                txs: vec![TxDelta {
                    spent: vec![],
                    created: vec![out(A, 500), out(B, 40)],
                }],
            },
            // Block 8 touches nobody: no rows (R-SERIES-1).
            BlockDelta {
                height: 8,
                hash: [0; 32],
                txs: vec![],
            },
            BlockDelta {
                height: 9,
                hash: [0; 32],
                txs: vec![
                    TxDelta {
                        spent: vec![out(A, 500)],
                        created: vec![out(A, 120)],
                    },
                    TxDelta {
                        spent: vec![],
                        created: vec![out(A, 1)],
                    },
                ],
            },
        ];
        let o = run(input(
            blocks,
            vec![label(A, "entity:a"), label(B, "entity:b")],
            vec![],
        ))
        .expect("run");
        let rows = decode(&o);
        let got: Vec<(&str, u64, u128, u128, i128, u64)> = rows
            .iter()
            .map(|r| {
                (
                    r.entity.as_str(),
                    r.height,
                    r.received_sats,
                    r.sent_sats,
                    r.net_sats,
                    r.tx_count,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("entity:a", 7, 500, 0, 500, 1),
                ("entity:a", 9, 121, 500, -379, 2),
                ("entity:b", 7, 40, 0, 40, 1),
            ]
        );
        // Keys are strictly ascending and match the documented layout.
        assert!(o.rows.windows(2).all(|w| w[0].key < w[1].key));
        assert_eq!(o.rows[1].key, EntityBlockFlowRow::key("entity:a", 9));
    }

    #[test]
    fn prefix_entities_sort_by_entity_then_height() {
        // "entity:a" is a prefix of "entity:ab": the 0x00 separator keeps every
        // "entity:a" row before every "entity:ab" row, at any height.
        let blocks = (1u64..=2)
            .map(|h| BlockDelta {
                height: h,
                hash: [0; 32],
                txs: vec![TxDelta {
                    spent: vec![],
                    created: vec![out(A, h), out(B, h)],
                }],
            })
            .collect();
        let o = run(input(
            blocks,
            vec![label(A, "entity:a"), label(B, "entity:ab")],
            vec![],
        ))
        .expect("run");
        let order: Vec<(String, u64)> = decode(&o)
            .into_iter()
            .map(|r| (r.entity, r.height))
            .collect();
        assert_eq!(
            order,
            vec![
                ("entity:a".into(), 1),
                ("entity:a".into(), 2),
                ("entity:ab".into(), 1),
                ("entity:ab".into(), 2),
            ]
        );
        assert!(o.rows.windows(2).all(|w| w[0].key < w[1].key));
    }

    #[test]
    fn bad_inputs_fail_with_their_codes() {
        let blocks = || {
            vec![BlockDelta {
                height: 1,
                hash: [0; 32],
                txs: vec![],
            }]
        };
        assert_eq!(
            run(input(blocks(), vec![label(A, "entity:a")], vec![1])).err(),
            Some(code::BAD_PARAMS)
        );
        assert_eq!(
            run(input(blocks(), vec![label(A, "entity:a\0x")], vec![])).err(),
            Some(code::BAD_LABELS)
        );
        assert_eq!(
            run(input(
                blocks(),
                vec![label(
                    "btc:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5",
                    "entity:a"
                )],
                vec![]
            ))
            .err(),
            Some(code::BAD_ADDRESS)
        );
        assert_eq!(
            run(input(vec![], vec![label(A, "entity:a")], vec![])).err(),
            Some(code::BAD_SOURCE)
        );
    }

    /// SER-R1-F1: a range where no labelled entity is touched (empty blocks,
    /// or only unlabelled outputs) ends with NO_ACTIVITY, never with an empty
    /// output the host would turn into `BadOutput`.
    #[test]
    fn a_quiet_range_is_no_activity() {
        let unlabelled = TxOut {
            script: vec![0x6a, 1],
            value: 5,
        };
        let quiet = vec![
            BlockDelta {
                height: 1,
                hash: [0; 32],
                txs: vec![],
            },
            BlockDelta {
                height: 2,
                hash: [0; 32],
                txs: vec![TxDelta {
                    spent: vec![unlabelled.clone()],
                    created: vec![unlabelled],
                }],
            },
        ];
        assert_eq!(
            run(input(quiet, vec![label(A, "entity:a")], vec![])).err(),
            Some(code::NO_ACTIVITY)
        );
        assert_eq!(code::NO_ACTIVITY, 8);
    }
}
