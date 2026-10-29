//! Mining-pool block metric (ADR-0032): the pinned pool table and the
//! coinbase source of `btc-pool-blocks`.
//!
//! The table comes from `fixtures/pools/pool-allowlist.json`: the reviewed
//! pool companies of the MIT-licensed `bitcoin-data/mining-pools` list, built
//! by `scripts/build-pool-allowlist.py` (see `fixtures/pools/PROVENANCE.md`).
//! The upstream list is not committed: it names people. Only reviewed pool
//! companies enter (R-POOL-1, POOL-R1-F4): a company website (not a forum
//! post, an IP address, an explorer page or an issue tracker), not run under
//! an operator's personal handle, and not a person's name. As defence in
//! depth this module refuses [`EXCLUDED_POOL_IDS`] and unsited entries again.
//! Everything else is never named: its blocks count as `unknown`.
//!
//! The coinbase source is built from raw blocks (a node directory) or from a
//! committed proof file (header, coinbase transaction, Merkle branch per
//! block). Both paths check proof of work against a pinned floor, parent
//! links, the BIP-34 height in the coinbase and the coinbase's inclusion
//! under the header's Merkle root, so the source is exactly the chain's.

use crate::node::{check_raw_header, NodePin};
use crate::{BtcAdapterError, CHAIN_ID};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256d, Hash};
use bitcoin::{Address, Block, Network, Transaction};
use onyx_core::{keccak256, ChainId, Hash32, Snapshot};
use onyx_pool_schema::{
    CoinbaseBlock, CoinbaseSource, PoolRule, PoolTable, TxOut, COINBASE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

/// The pool list, relative to the fixtures directory.
pub const POOL_LIST: &str = "pools/pool-allowlist.json";

/// Upstream list ids never named, with the reason class (R-POOL-1,
/// POOL-R1-F4). No names: some of these are people. Their blocks count as
/// `unknown`. Same table as `scripts/build-pool-allowlist.py`.
pub const EXCLUDED_POOL_IDS: &[(u32, &str)] = &[
    (21, "person"),
    (23, "no company site (forum post)"),
    (30, "person"),
    (40, "person"),
    (52, "person"),
    (53, "person"),
    (65, "no company site (bare IP)"),
    (66, "operator's personal handle"),
    (75, "operator's personal handle"),
    (
        115,
        "operator's personal handle (site on an operator-handle domain; POOL-R2-F6)",
    ),
    (118, "operator's personal handle"),
    (131, "no company site (bare IP)"),
    (154, "no company site (explorer page)"),
];

/// Longest raw block file accepted (hex characters, twice the bytes).
const MAX_RAW_HEX: u64 = 2 * 8 * 1024 * 1024;

#[derive(Deserialize)]
struct ListPool {
    id: u32,
    name: String,
    addresses: Vec<String>,
    tags: Vec<String>,
    #[serde(default)]
    link: String,
}

fn err(s: impl Into<String>) -> BtcAdapterError {
    BtcAdapterError::Fixture(s.into())
}

/// A pool the table names: its list id, name and website.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PoolInfo {
    /// Id in the source list.
    pub id: u32,
    /// Name, as rows carry it.
    pub name: String,
    /// The operator's website, from the list.
    pub link: String,
}

/// R-POOL-1: the list entry names an operating pool company.
fn reviewed(p: &ListPool) -> bool {
    let site = p.link.starts_with("https://") || p.link.starts_with("http://");
    site && !p.link.contains("github.com") && !EXCLUDED_POOL_IDS.iter().any(|(id, _)| *id == p.id)
}

/// Build the pinned pool table from the list bytes: reviewed pools in list id
/// order (the priority order), each payout address turned into its output
/// script (mainnet only), each tag as its UTF-8 bytes. Returns the table and
/// the named pools.
pub fn pool_table_from(list: &[u8]) -> Result<(PoolTable, Vec<PoolInfo>), BtcAdapterError> {
    let mut pools: Vec<ListPool> =
        serde_json::from_slice(list).map_err(|e| err(format!("pool list: {e}")))?;
    pools.sort_by_key(|p| p.id);
    if pools.windows(2).any(|w| w[0].id == w[1].id) {
        return Err(err("pool list: duplicate id"));
    }
    let mut rules = Vec::new();
    let mut info = Vec::new();
    for p in pools.iter().filter(|p| reviewed(p)) {
        let mut scripts = Vec::with_capacity(p.addresses.len());
        for a in &p.addresses {
            let addr = Address::from_str(a)
                .and_then(|x| x.require_network(Network::Bitcoin))
                .map_err(|e| err(format!("pool {}: address {a}: {e}", p.id)))?;
            scripts.push(addr.script_pubkey().to_bytes());
        }
        let tags: Vec<Vec<u8>> = p
            .tags
            .iter()
            .filter(|t| !t.is_empty())
            .map(|t| t.as_bytes().to_vec())
            .collect();
        if tags.is_empty() && scripts.is_empty() {
            continue;
        }
        rules.push(PoolRule {
            name: p.name.clone(),
            tags,
            scripts,
        });
        info.push(PoolInfo {
            id: p.id,
            name: p.name.clone(),
            link: p.link.clone(),
        });
    }
    let table = PoolTable {
        list_hash: keccak256(list).0,
        pools: rules,
    };
    Ok((table, info))
}

/// [`pool_table_from`] on `fixtures/pools/pool-list.json`, with the
/// fixtures checksum checked.
pub fn pool_table(fixtures_dir: &Path) -> Result<(PoolTable, Vec<PoolInfo>), BtcAdapterError> {
    crate::verify_checksum(fixtures_dir, POOL_LIST)?;
    let bytes = std::fs::read(fixtures_dir.join(POOL_LIST)).map_err(|e| err(e.to_string()))?;
    pool_table_from(&bytes)
}

/// BIP-34: the height pushed first in a coinbase `scriptSig`.
pub fn bip34_height(script_sig: &[u8]) -> Option<u64> {
    let n = usize::from(*script_sig.first()?);
    if !(1..=8).contains(&n) || script_sig.len() < 1 + n {
        return None;
    }
    let mut h: u64 = 0;
    for (i, b) in script_sig[1..=n].iter().enumerate() {
        h |= u64::from(*b) << (8 * i);
    }
    Some(h)
}

fn coinbase_block(height: u64, hash: [u8; 32], tx: &Transaction) -> Result<CoinbaseBlock, String> {
    if !tx.is_coinbase() {
        return Err("first transaction is not a coinbase".into());
    }
    let script_sig = tx.input[0].script_sig.to_bytes();
    if bip34_height(&script_sig) != Some(height) {
        return Err("coinbase height".into());
    }
    let mut display = hash;
    display.reverse();
    Ok(CoinbaseBlock {
        height,
        hash: display,
        script_sig,
        outputs: tx
            .output
            .iter()
            .map(|o| TxOut {
                script: o.script_pubkey.to_bytes(),
                value: o.value.to_sat(),
            })
            .collect(),
    })
}

fn finish(
    blocks: Vec<CoinbaseBlock>,
    from: u64,
    to: u64,
    pin: &NodePin,
) -> Result<(Vec<u8>, Snapshot), BtcAdapterError> {
    let src = CoinbaseSource {
        schema_version: COINBASE_SCHEMA_VERSION,
        chain: CHAIN_ID.into(),
        blocks,
    };
    if !src.is_canonical()
        || src.blocks.first().map(|b| b.height) != Some(from)
        || src.blocks.last().map(|b| b.height) != Some(to)
    {
        return Err(err("coinbase blocks not contiguous"));
    }
    if let Some((h, want)) = &pin.tip {
        let tip = src.blocks.last().map(|b| (b.height, hex::encode(b.hash)));
        if tip.as_ref() != Some(&(*h, want.clone())) {
            return Err(err("tip does not match the pinned tip"));
        }
    }
    let tip_hash = src
        .blocks
        .last()
        .map(|b| Hash32(b.hash))
        .unwrap_or(Hash32([0; 32]));
    let bytes = borsh::to_vec(&src).map_err(|e| err(e.to_string()))?;
    let snapshot = Snapshot::BlockRange {
        chain: ChainId(CHAIN_ID.into()),
        from_height: from,
        to_height: to,
        tip_hash,
        content_hash: keccak256(&bytes),
    };
    Ok((bytes, snapshot))
}

fn read_raw(dir: &Path, height: u64) -> Result<Block, BtcAdapterError> {
    let mut raw = String::new();
    std::fs::File::open(dir.join(format!("{height}.raw.hex")))
        .map_err(|e| err(format!("{height}: {e}")))?
        .take(MAX_RAW_HEX + 1)
        .read_to_string(&mut raw)
        .map_err(|e| err(e.to_string()))?;
    if raw.len() as u64 > MAX_RAW_HEX {
        return Err(err(format!("{height}: raw block too large")));
    }
    let bytes = hex::decode(raw.trim()).map_err(|e| err(e.to_string()))?;
    deserialize(&bytes).map_err(|e| err(format!("{height}: {e}")))
}

/// Load the coinbases of blocks `from..=to` from raw block files
/// (`<height>.raw.hex`, as `scripts/fetch-btc-node.sh` writes them): proof of
/// work against `pin`, parent links, BIP-34 height, and the full Merkle root.
pub fn load_coinbase_range(
    dir: &Path,
    from: u64,
    to: u64,
    pin: &NodePin,
) -> Result<(Vec<u8>, Snapshot), BtcAdapterError> {
    if from > to {
        return Err(err("empty range"));
    }
    let mut blocks = Vec::new();
    let mut prev: Option<[u8; 32]> = None;
    for height in from..=to {
        let block = read_raw(dir, height)?;
        let (hash, parent) = check_raw_header(&serialize(&block.header), pin.min_bits)
            .map_err(|e| err(format!("{height}: {e}")))?;
        if prev.is_some_and(|p| p != parent) {
            return Err(err(format!("{height}: parent link")));
        }
        prev = Some(hash);
        if !block.check_merkle_root() {
            return Err(err(format!("{height}: merkle root")));
        }
        let tx = block
            .txdata
            .first()
            .ok_or_else(|| err(format!("{height}: no tx")))?;
        blocks.push(coinbase_block(height, hash, tx).map_err(|e| err(format!("{height}: {e}")))?);
    }
    finish(blocks, from, to, pin)
}

/// One block's coinbase with what proves it: the 80-byte header, the coinbase
/// transaction and the Merkle branch from the coinbase (index 0) to the
/// header's Merkle root. Hex strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoinbaseProof {
    /// Block height.
    pub height: u64,
    /// The 80-byte header.
    pub header: String,
    /// The coinbase transaction (full serialization, witness included).
    pub coinbase: String,
    /// Sibling hashes from the coinbase txid up to the Merkle root (internal order).
    pub branch: Vec<String>,
}

/// A committed proof file: a chain id and contiguous coinbase proofs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoinbaseProofs {
    /// `btc.mainnet`.
    pub chain: String,
    /// Proofs in height order.
    pub blocks: Vec<CoinbaseProof>,
}

fn hash_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(a);
    buf[32..].copy_from_slice(b);
    sha256d::Hash::hash(&buf).to_byte_array()
}

/// The Merkle branch of transaction 0 over `txids` (internal byte order),
/// Bitcoin's rule: an odd level duplicates its last hash.
pub fn coinbase_branch(txids: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut level: Vec<[u8; 32]> = txids.to_vec();
    let mut branch = Vec::new();
    while level.len() > 1 {
        branch.push(*level.get(1).unwrap_or(&level[0]));
        let next: Vec<[u8; 32]> = level
            .chunks(2)
            .map(|c| hash_pair(&c[0], c.get(1).unwrap_or(&c[0])))
            .collect();
        level = next;
    }
    branch
}

/// Fold a coinbase txid up its branch (it is always the left child).
pub fn fold_branch(txid: [u8; 32], branch: &[[u8; 32]]) -> [u8; 32] {
    branch.iter().fold(txid, |acc, s| hash_pair(&acc, s))
}

/// Proofs for blocks `from..=to` from raw block files, checked as
/// [`load_coinbase_range`] checks them.
pub fn export_coinbase_proofs(
    dir: &Path,
    from: u64,
    to: u64,
    pin: &NodePin,
) -> Result<CoinbaseProofs, BtcAdapterError> {
    load_coinbase_range(dir, from, to, pin)?;
    let mut blocks = Vec::new();
    for height in from..=to {
        let block = read_raw(dir, height)?;
        let txids: Vec<[u8; 32]> = block
            .txdata
            .iter()
            .map(|t| t.compute_txid().to_byte_array())
            .collect();
        let branch = coinbase_branch(&txids);
        blocks.push(CoinbaseProof {
            height,
            header: hex::encode(serialize(&block.header)),
            coinbase: hex::encode(serialize(&block.txdata[0])),
            branch: branch.iter().map(hex::encode).collect(),
        });
    }
    Ok(CoinbaseProofs {
        chain: CHAIN_ID.into(),
        blocks,
    })
}

/// Load a committed proof file: per block, proof of work against `pin`,
/// parent links, the coinbase's BIP-34 height and its Merkle branch to the
/// header's root. Returns the source bytes and their snapshot.
pub fn load_coinbase_proofs(
    proofs: &CoinbaseProofs,
    pin: &NodePin,
) -> Result<(Vec<u8>, Snapshot), BtcAdapterError> {
    if proofs.chain != CHAIN_ID {
        return Err(BtcAdapterError::WrongSnapshot);
    }
    let from = proofs
        .blocks
        .first()
        .map(|b| b.height)
        .ok_or_else(|| err("no blocks"))?;
    let to = proofs
        .blocks
        .last()
        .map(|b| b.height)
        .ok_or_else(|| err("no blocks"))?;
    let mut blocks = Vec::new();
    let mut prev: Option<[u8; 32]> = None;
    for p in &proofs.blocks {
        let h = p.height;
        let header = hex::decode(&p.header).map_err(|e| err(e.to_string()))?;
        let (hash, parent) =
            check_raw_header(&header, pin.min_bits).map_err(|e| err(format!("{h}: {e}")))?;
        if prev.is_some_and(|x| x != parent) {
            return Err(err(format!("{h}: parent link")));
        }
        prev = Some(hash);
        let tx: Transaction = hex::decode(&p.coinbase)
            .ok()
            .and_then(|b| deserialize(&b).ok())
            .ok_or_else(|| err(format!("{h}: coinbase")))?;
        let mut branch = Vec::with_capacity(p.branch.len());
        for s in &p.branch {
            let mut x = [0u8; 32];
            hex::decode_to_slice(s, &mut x).map_err(|e| err(format!("{h}: branch: {e}")))?;
            branch.push(x);
        }
        let root = fold_branch(tx.compute_txid().to_byte_array(), &branch);
        if header.get(36..68) != Some(&root[..]) {
            return Err(err(format!(
                "{h}: coinbase not under the header's Merkle root"
            )));
        }
        blocks.push(coinbase_block(h, hash, &tx).map_err(|e| err(format!("{h}: {e}")))?);
    }
    finish(blocks, from, to, pin)
}

/// One pool shard of a history (one UTC day): its range and root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolShard {
    /// First height.
    pub from: u64,
    /// Last height (inclusive).
    pub to: u64,
    /// Root of `btc-pool-blocks` on the shard.
    pub root: Hash32,
}

/// The hash a website pins for a pool history: domain, the recipe id, the
/// Keccak of the pool table bytes (the shards' `params`), the tip block hash
/// (display order), then each shard as `from (u64 BE) || to (u64 BE) || root`.
pub fn pools_commitment(
    recipe: &Hash32,
    table: &Hash32,
    tip: &[u8; 32],
    shards: &[PoolShard],
) -> Hash32 {
    let mut b = Vec::with_capacity(13 + 32 * 3 + shards.len() * 48);
    b.extend_from_slice(b"onyx-pools-v1");
    b.extend_from_slice(&recipe.0);
    b.extend_from_slice(&table.0);
    b.extend_from_slice(tip);
    for s in shards {
        b.extend_from_slice(&s.from.to_be_bytes());
        b.extend_from_slice(&s.to.to_be_bytes());
        b.extend_from_slice(&s.root.0);
    }
    keccak256(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_folds_to_the_root_for_any_tx_count() {
        for n in 1usize..=9 {
            let txids: Vec<[u8; 32]> = (0..n).map(|i| [i as u8 + 1; 32]).collect();
            let mut level = txids.clone();
            while level.len() > 1 {
                level = level
                    .chunks(2)
                    .map(|c| hash_pair(&c[0], c.get(1).unwrap_or(&c[0])))
                    .collect();
            }
            assert_eq!(
                fold_branch(txids[0], &coinbase_branch(&txids)),
                level[0],
                "n={n}"
            );
        }
    }

    #[test]
    fn bip34_height_reads_the_first_push() {
        assert_eq!(bip34_height(&[3, 0x2f, 0xcf, 0x0e, 0xaa]), Some(970_543));
        assert_eq!(bip34_height(&[]), None);
        assert_eq!(bip34_height(&[0]), None);
        assert_eq!(bip34_height(&[4, 1, 2]), None);
    }

    #[test]
    fn excluded_and_unsited_pools_are_never_named() {
        let list = br#"[
          {"id":1,"name":"Good Pool","addresses":[],"tags":["/good/"],"link":"https://good.example"},
          {"id":21,"name":"Excluded Example","addresses":[],"tags":["x"],"link":"https://excluded.example"},
          {"id":3,"name":"No Site","addresses":[],"tags":["/n/"],"link":""},
          {"id":4,"name":"Issue","addresses":[],"tags":["/i/"],"link":"https://github.com/x/y/issues/1"},
          {"id":5,"name":"Empty","addresses":[],"tags":[""],"link":"https://e.example"}
        ]"#;
        let (t, info) = pool_table_from(list).expect("table");
        let names: Vec<&str> = t.pools.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Good Pool"]);
        assert_eq!(info.len(), 1);
        assert_eq!(t.list_hash, keccak256(list).0);
    }
}
