//! Builders of well-formed synthetic blocks for tests (feature `synthetic`,
//! never in the recipe build): headers, transactions of types 0-4 and
//! receipts in the chain's own encodings, so a synthetic block passes every
//! check a real one does. Signatures are fixed placeholder values (nothing
//! here recovers a sender).

use crate::rlp::{encode_bytes, encode_list, encode_uint};
use crate::{keccak256, trie, EthBlock};
use alloc::vec;
use alloc::vec::Vec;

/// The header fields a test chooses; every other field is zero or empty.
#[derive(Clone, Copy, Debug)]
pub struct HeaderSpec {
    /// Parent hash.
    pub parent_hash: [u8; 32],
    /// Number.
    pub number: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas used.
    pub gas_used: u64,
    /// Timestamp.
    pub timestamp: u64,
    /// Base fee per gas.
    pub base_fee: u128,
    /// Transactions root.
    pub transactions_root: [u8; 32],
    /// Receipts root.
    pub receipts_root: [u8; 32],
    /// Blob gas used and excess blob gas (Cancun fields), if any.
    pub blob: Option<(u64, u64)>,
}

/// RLP of a header with the Prague field list (21 fields) when `blob` is set,
/// London's 16 otherwise.
pub fn header(h: &HeaderSpec) -> Vec<u8> {
    let mut p = Vec::new();
    encode_bytes(&mut p, &h.parent_hash);
    encode_bytes(&mut p, &trie::EMPTY_ROOT); // ommers hash (any 32 bytes)
    encode_bytes(&mut p, &[0x11; 20]); // fee recipient
    encode_bytes(&mut p, &[0x22; 32]); // state root
    encode_bytes(&mut p, &h.transactions_root);
    encode_bytes(&mut p, &h.receipts_root);
    encode_bytes(&mut p, &[0u8; 256]); // logs bloom
    encode_uint(&mut p, 0); // difficulty
    encode_uint(&mut p, u128::from(h.number));
    encode_uint(&mut p, u128::from(h.gas_limit));
    encode_uint(&mut p, u128::from(h.gas_used));
    encode_uint(&mut p, u128::from(h.timestamp));
    encode_bytes(&mut p, b"onyx"); // extra data
    encode_bytes(&mut p, &[0x33; 32]); // prev randao
    encode_bytes(&mut p, &[0u8; 8]); // nonce
    encode_uint(&mut p, h.base_fee);
    if let Some((used, excess)) = h.blob {
        encode_bytes(&mut p, &[0x44; 32]); // withdrawals root
        encode_uint(&mut p, u128::from(used));
        encode_uint(&mut p, u128::from(excess));
        encode_bytes(&mut p, &[0x55; 32]); // parent beacon block root
        encode_bytes(&mut p, &[0x66; 32]); // requests hash
    }
    let mut out = Vec::new();
    encode_list(&mut out, &p);
    out
}

/// A transaction a test chooses.
#[derive(Clone, Copy, Debug)]
pub struct TxSpec {
    /// Type 0-4.
    pub tx_type: u8,
    /// Recipient; `None` creates a contract (types 0-2 only).
    pub to: Option<[u8; 20]>,
    /// Value, wei.
    pub value: u128,
    /// Gas price (types 0-1) or fee cap (types 2-4).
    pub max_fee: u128,
    /// Priority-fee cap (types 2-4; ignored otherwise).
    pub max_priority: u128,
    /// Blob versioned hashes (type 3).
    pub blobs: u8,
}

/// The transaction's envelope, as the transactions trie stores it.
pub fn tx(t: &TxSpec) -> Vec<u8> {
    let mut p = Vec::new();
    let to = |p: &mut Vec<u8>| match t.to {
        Some(a) => encode_bytes(p, &a),
        None => encode_bytes(p, &[]),
    };
    let sig = |p: &mut Vec<u8>, legacy: bool| {
        encode_uint(p, if legacy { 37 } else { 1 });
        encode_bytes(p, &[0x77; 32]);
        encode_bytes(p, &[0x01; 32]);
    };
    if t.tx_type == 0 {
        encode_uint(&mut p, 7); // nonce
        encode_uint(&mut p, t.max_fee);
        encode_uint(&mut p, 21_000);
        to(&mut p);
        encode_uint(&mut p, t.value);
        encode_bytes(&mut p, &[]);
        sig(&mut p, true);
        let mut out = Vec::new();
        encode_list(&mut out, &p);
        return out;
    }
    encode_uint(&mut p, 1); // chain id
    encode_uint(&mut p, 7); // nonce
    if t.tx_type == 1 {
        encode_uint(&mut p, t.max_fee);
    } else {
        encode_uint(&mut p, t.max_priority);
        encode_uint(&mut p, t.max_fee);
    }
    encode_uint(&mut p, 100_000);
    to(&mut p);
    encode_uint(&mut p, t.value);
    encode_bytes(&mut p, &[0xab, 0xcd]);
    encode_list(&mut p, &[]); // access list
    if t.tx_type == 3 {
        encode_uint(&mut p, 1_000_000_000);
        let mut hashes = Vec::new();
        for k in 0..t.blobs {
            let mut h = [k; 32];
            h[0] = 1;
            encode_bytes(&mut hashes, &h);
        }
        encode_list(&mut p, &hashes);
    }
    if t.tx_type == 4 {
        let mut a = Vec::new();
        encode_uint(&mut a, 1);
        encode_bytes(&mut a, &[0x88; 20]);
        encode_uint(&mut a, 0);
        encode_uint(&mut a, 1);
        encode_bytes(&mut a, &[0x77; 32]);
        encode_bytes(&mut a, &[0x01; 32]);
        let mut one = Vec::new();
        encode_list(&mut one, &a);
        encode_list(&mut p, &one);
    }
    sig(&mut p, false);
    let mut out = vec![t.tx_type];
    encode_list(&mut out, &p);
    out
}

/// A receipt's envelope with one log and an empty bloom.
pub fn receipt(tx_type: u8, success: bool, cumulative_gas: u64) -> Vec<u8> {
    let mut p = Vec::new();
    encode_uint(&mut p, u128::from(success));
    encode_uint(&mut p, u128::from(cumulative_gas));
    encode_bytes(&mut p, &[0u8; 256]);
    let mut log = Vec::new();
    encode_bytes(&mut log, &[0x99; 20]);
    let mut topics = Vec::new();
    encode_bytes(&mut topics, &[0x12; 32]);
    encode_list(&mut log, &topics);
    encode_bytes(&mut log, b"data");
    let mut one = Vec::new();
    encode_list(&mut one, &log);
    let mut logs = Vec::new();
    encode_list(&mut logs, &one);
    p.extend(logs);
    let mut out = if tx_type == 0 { vec![] } else { vec![tx_type] };
    encode_list(&mut out, &p);
    out
}

/// A block over `txs` with each transaction's gas used, its roots and hash
/// filled in; `h` gives the rest of the header (its roots and gas used are replaced).
pub fn block(mut h: HeaderSpec, txs: &[(TxSpec, bool, u64)]) -> EthBlock {
    let envs: Vec<Vec<u8>> = txs.iter().map(|(t, _, _)| tx(t)).collect();
    let mut cum = 0u64;
    let receipts: Vec<Vec<u8>> = txs
        .iter()
        .map(|(t, ok, gas)| {
            cum += gas;
            receipt(t.tx_type, *ok, cum)
        })
        .collect();
    h.gas_used = cum;
    h.transactions_root = trie::ordered_root(&envs);
    h.receipts_root = trie::ordered_root(&receipts);
    let header = header(&h);
    EthBlock {
        number: h.number,
        hash: keccak256(&header),
        header,
        txs: envs,
        receipts,
    }
}
