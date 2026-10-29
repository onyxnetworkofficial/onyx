//! `btc-entity-balance`: per-entity BTC flows over a contiguous block range.
//!
//! Input (`RecipeInput`):
//! - `source`: Borsh `BtcSource` (blocks with spent prevouts and created outputs);
//! - `labels`: Borsh label set (`LabelSetView`), addresses like `btc:bc1q...`;
//! - `params`: Borsh `BalanceParams` (optional starting balances), empty = none.
//!
//! Output: one `EntityFlowRow` per entity in the label set, keyed by the raw
//! UTF-8 bytes of the entity id (so rows sort by entity id). If a starting
//! balance is given, it must be given for exactly the labelled entities. Seed-address attribution only:
//! no clustering heuristics (those come later as a separate recipe version).
//!
//! Everything is integer satoshis. Label addresses are decoded to scripts here,
//! inside the sandbox, with checksums verified (THREATS T-ADAPT-1).

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use onyx_btc_schema::{BalanceParams, BtcSource, EntityFlowRow};
use onyx_recipe_sdk::{LabelSetView, OutputRow, RecipeInput, RecipeOutput};
use sha2::{Digest, Sha256};

/// Recipe failure codes (committed as the canonical failure leaf).
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
    /// from the labelled entities (a partial base would fabricate balances, R-RCP-BASE-1).
    pub const BAD_PARAMS: u32 = 5;
    /// Starting balance plus net flow is negative: the base is inconsistent with the chain.
    pub const NEGATIVE_BALANCE: u32 = 6;
    /// Arithmetic overflow (cannot happen with real BTC amounts).
    pub const OVERFLOW: u32 = 7;
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
    let params: BalanceParams = if input.params.is_empty() {
        BalanceParams::default()
    } else {
        borsh::from_slice(&input.params).map_err(|_| code::BAD_PARAMS)?
    };
    if !params.is_canonical() {
        return Err(code::BAD_PARAMS);
    }

    // script -> entity, and one accumulator per entity (BTreeMap: deterministic order).
    let mut owner: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    for l in &labels.labels {
        let script = address_script(&l.address).ok_or(code::BAD_ADDRESS)?;
        if let Some(prev) = owner.insert(script, l.entity.clone()) {
            if prev != l.entity {
                return Err(code::CONFLICTING_LABELS);
            }
        }
        acc.entry(l.entity.clone()).or_default();
    }
    if !params.base.is_empty()
        && !params
            .base
            .iter()
            .map(|b| b.entity.as_str())
            .eq(acc.keys().map(String::as_str))
    {
        return Err(code::BAD_PARAMS);
    }

    for block in &source.blocks {
        for tx in &block.txs {
            let mut touched: Vec<&String> = Vec::new();
            for out in &tx.spent {
                if let Some(e) = owner.get(&out.script) {
                    let a = acc.get_mut(e).ok_or(code::OVERFLOW)?;
                    a.sent = a
                        .sent
                        .checked_add(u128::from(out.value))
                        .ok_or(code::OVERFLOW)?;
                    touched.push(e);
                }
            }
            for out in &tx.created {
                if let Some(e) = owner.get(&out.script) {
                    let a = acc.get_mut(e).ok_or(code::OVERFLOW)?;
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
                let a = acc.get_mut(e).ok_or(code::OVERFLOW)?;
                a.txs += 1;
            }
        }
    }

    let from_height = source.blocks.first().map(|b| b.height).unwrap_or(0);
    let to_height = source.blocks.last().map(|b| b.height).unwrap_or(0);
    let base: BTreeMap<&str, u64> = params
        .base
        .iter()
        .map(|b| (b.entity.as_str(), b.balance_sats))
        .collect();
    let has_base = !params.base.is_empty();

    let mut rows: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for (entity, a) in acc {
        let net = i128::try_from(a.received).map_err(|_| code::OVERFLOW)?
            - i128::try_from(a.sent).map_err(|_| code::OVERFLOW)?;
        let end_balance_sats = if has_base {
            let start = i128::from(*base.get(entity.as_str()).unwrap_or(&0));
            let end = start.checked_add(net).ok_or(code::OVERFLOW)?;
            Some(u128::try_from(end).map_err(|_| code::NEGATIVE_BALANCE)?)
        } else {
            None
        };
        let row = EntityFlowRow {
            entity: entity.clone(),
            from_height,
            to_height,
            received_sats: a.received,
            sent_sats: a.sent,
            net_sats: net,
            tx_count: a.txs,
            end_balance_sats,
        };
        let key = entity.as_bytes().to_vec();
        let value = borsh::to_vec(&row).map_err(|_| code::OVERFLOW)?;
        rows.insert(key, value);
    }
    Ok(RecipeOutput {
        rows: rows
            .into_iter()
            .map(|(key, value)| OutputRow { key, value })
            .collect(),
    })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_known_addresses() {
        // BIP-173 example P2WPKH.
        assert_eq!(
            address_script("btc:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"),
            Some(
                [
                    &[0x00, 0x14][..],
                    &hex20("751e76e8199196d454941c45d1b3a323f1433bd6")
                ]
                .concat()
            )
        );
        // Genesis coinbase address (P2PKH).
        assert_eq!(
            address_script("btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"),
            Some(
                [
                    &[0x76, 0xa9, 0x14][..],
                    &hex20("62e907b15cbf27d5425399ebf6f0fb50ebb88f18"),
                    &[0x88, 0xac],
                ]
                .concat()
            )
        );
        // Canonical spelling: two leading '1's for two leading zero bytes (R-B58-1).
        assert!(address_script("btc:112D2adLM3UKy4Z4giRbReR6gjWuvHUqB").is_some());
        // BIP-350 example P2TR (v1, bech32m).
        let tr =
            address_script("btc:bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0")
                .expect("taproot");
        assert_eq!(tr.len(), 34);
        assert_eq!(&tr[..2], &[0x51, 0x20]);
    }

    #[test]
    fn rejects_bad_addresses() {
        for bad in [
            "btc:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5", // checksum
            "btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNb",         // checksum
            "btc:12D2adLM3UKy4Z4giRbReR6gjWuvHUqB",           // one '1' for two zero bytes
            "btc:tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", // testnet
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",     // no namespace
        ] {
            assert_eq!(address_script(bad), None, "{bad}");
        }
    }

    fn hex20(s: &str) -> [u8; 20] {
        let mut out = [0u8; 20];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
        }
        out
    }
}
