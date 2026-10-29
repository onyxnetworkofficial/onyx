//! Chain statistics source (ADR-0043): the `ChainSource` of `btc-chain-stats`
//! from a node's block files (`<height>.raw.hex` plus `<height>.v3.json.gz`,
//! as `scripts/fetch-btc-node.sh` and `scripts/refresh-onchain.py` write them),
//! and the hash a website pins for a run of chain-statistics shards.
//!
//! Checked here, as [`crate::node::load_node_range`] checks a balance source:
//! proof of work against the pinned floor, parent links (also from the parent
//! header to the first block), BIP-34 height, the node index's hash, the
//! transaction Merkle root with no duplicate txids, the witness commitment,
//! each input's outpoint against the raw transaction; prevouts created inside
//! the range must equal the raw outputs they spend, at the height they were
//! created; fees are never negative and the coinbase claims at most subsidy
//! plus fees. Sizes and outputs come from the raw bytes.
//!
//! Free text never enters the source (CHAIN-4): an output whose script starts
//! with `OP_RETURN` is carried as that one byte. The recipe reads only that
//! such an output is unspendable, so the rows are the same, and a block input
//! the website ships for a rerun holds no message written into the chain.
//! Coinbase `scriptSig`s and witnesses are never carried at all.
//!
//! Not checked: prevouts (value, script, creation height) of outputs created
//! before the range come from the node's index, as for every Bitcoin source
//! here (`node.rs`): a validator must use its own node, and a lying mirror
//! shows up as a content-hash mismatch against the shard pin.

use crate::node::{
    btc_to_sats, check_header, permitted_bits_transition, read_block, subsidy, NodePin, MAX_MONEY,
};
use crate::{BtcAdapterError, CHAIN_ID};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::{Block, OutPoint};
use onyx_chain_schema::{
    ChainBlock, ChainInput, ChainSource, ChainTx, TxOut, CHAIN_SCHEMA_VERSION,
};
use onyx_core::{keccak256, ChainId, Hash32, Snapshot};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

fn err(s: impl Into<String>) -> BtcAdapterError {
    BtcAdapterError::Fixture(s.into())
}

/// The first of `dirs` that holds both files of block `height`.
pub fn block_dir(dirs: &[PathBuf], height: u64) -> Option<&Path> {
    dirs.iter()
        .find(|d| {
            d.join(format!("{height}.raw.hex")).is_file()
                && d.join(format!("{height}.v3.json.gz")).is_file()
        })
        .map(PathBuf::as_path)
}

/// The first of `dirs` that holds the raw file of block `height` (a parent
/// needs only its header).
fn raw_dir(dirs: &[PathBuf], height: u64) -> Option<&Path> {
    dirs.iter()
        .find(|d| d.join(format!("{height}.raw.hex")).is_file())
        .map(PathBuf::as_path)
}

/// The 80-byte header of block `height` from its raw file, with proof of work
/// against `pin`. Returns the header and its hash (internal order).
pub fn read_header(
    dirs: &[PathBuf],
    height: u64,
    pin: &NodePin,
) -> Result<([u8; 80], bitcoin::BlockHash), BtcAdapterError> {
    let dir = raw_dir(dirs, height).ok_or_else(|| err(format!("{height}: no raw block file")))?;
    let mut head = [0u8; 160];
    std::fs::File::open(dir.join(format!("{height}.raw.hex")))
        .map_err(|e| err(format!("{height}: {e}")))?
        .read_exact(&mut head)
        .map_err(|e| err(format!("{height}: {e}")))?;
    let mut raw = [0u8; 80];
    hex::decode_to_slice(head, &mut raw).map_err(|e| err(format!("{height}: {e}")))?;
    let h: bitcoin::block::Header = deserialize(&raw).map_err(|e| err(format!("{height}: {e}")))?;
    check_header(&h, pin).map_err(|e| err(format!("{height}: {e}")))?;
    Ok((raw, h.block_hash()))
}

/// Load and check blocks `from..=to` (each from the first of `dirs` holding
/// it) and the header of block `from - 1`. Returns the canonical source bytes
/// (Borsh `ChainSource`) and the snapshot that pins them.
pub fn load_chain_range(
    dirs: &[PathBuf],
    from: u64,
    to: u64,
    pin: &NodePin,
) -> Result<(Vec<u8>, Snapshot), BtcAdapterError> {
    if from > to || from == 0 {
        return Err(err("empty range, or a range starting at genesis"));
    }
    let (parent_header, parent_hash) = read_header(dirs, from - 1, pin)?;
    let mut prev = parent_hash;
    let mut prev_bits = u32::from_le_bytes([
        parent_header[72],
        parent_header[73],
        parent_header[74],
        parent_header[75],
    ]);
    let mut blocks = Vec::new();
    // Outputs created inside the range: (output, creation height).
    let mut created: BTreeMap<OutPoint, (TxOut, u64)> = BTreeMap::new();
    for height in from..=to {
        let dir =
            block_dir(dirs, height).ok_or_else(|| err(format!("{height}: no block files")))?;
        let (block, v3) = read_block(dir, height)?;
        let hash = block.block_hash();
        check_header(&block.header, pin).map_err(|e| err(format!("{height}: {e}")))?;
        if block.header.prev_blockhash != prev {
            return Err(err(format!("{height}: parent link")));
        }
        // P3A-R1-6: the difficulty may change only at a period boundary, by at most 4x.
        let bits = block.header.bits.to_consensus();
        if !permitted_bits_transition(height, prev_bits, bits) {
            return Err(err(format!("{height}: difficulty transition")));
        }
        prev_bits = bits;
        prev = hash;
        if block.bip34_block_height().ok() != Some(height) || v3.height != height {
            return Err(err(format!("{height}: height")));
        }
        if hash.to_string() != v3.hash {
            return Err(err(format!("{height}: block hash differs from node index")));
        }
        if !block.check_merkle_root() || !block.check_witness_commitment() {
            return Err(err(format!("{height}: merkle root or witness commitment")));
        }
        let mut txids: Vec<_> = block.txdata.iter().map(|t| t.compute_txid()).collect();
        txids.sort_unstable();
        txids.dedup();
        if txids.len() != block.txdata.len() || v3.tx.len() != block.txdata.len() {
            return Err(err(format!("{height}: duplicate txids or tx count")));
        }
        let txs = block_txs(&block, &v3, height, &mut created)?;
        let mut display = [0u8; 32];
        hex::decode_to_slice(hash.to_string(), &mut display).map_err(|e| err(e.to_string()))?;
        let mut header = [0u8; 80];
        header.copy_from_slice(&serialize(&block.header));
        blocks.push(ChainBlock {
            height,
            hash: display,
            header,
            txs,
        });
    }
    let src = ChainSource {
        schema_version: CHAIN_SCHEMA_VERSION,
        chain: CHAIN_ID.into(),
        parent_header,
        blocks,
    };
    if !src.is_canonical() {
        return Err(err("blocks not contiguous"));
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

/// The script bytes the source carries for an output (CHAIN-4): `OP_RETURN`
/// alone for a script that starts with it (its payload is free text the
/// recipe never reads), the script itself otherwise.
pub fn carried_script(script: &[u8]) -> Vec<u8> {
    if script.first() == Some(&0x6a) {
        vec![0x6a]
    } else {
        script.to_vec()
    }
}

/// Transactions of one block with their sizes and spent prevouts. A prevout
/// of an output created earlier in the range must equal it, height included.
fn block_txs(
    block: &Block,
    v3: &crate::node::V3Block,
    height: u64,
    created: &mut BTreeMap<OutPoint, (TxOut, u64)>,
) -> Result<Vec<ChainTx>, BtcAdapterError> {
    let mut txs = Vec::with_capacity(block.txdata.len());
    let mut fees: u64 = 0;
    for (k, (tx, jt)) in block.txdata.iter().zip(&v3.tx).enumerate() {
        let txid = tx.compute_txid();
        if txid.to_string() != jt.txid {
            return Err(err(format!("{height}: tx {k} differs from node index")));
        }
        if tx.is_coinbase() != (k == 0) || tx.input.len() != jt.vin.len() {
            return Err(err(format!("{height}: tx {k} inputs")));
        }
        let outputs: Vec<TxOut> = tx
            .output
            .iter()
            .map(|o| TxOut {
                script: carried_script(o.script_pubkey.as_bytes()),
                value: o.value.to_sat(),
            })
            .collect();
        let out_sum = outputs
            .iter()
            .try_fold(0u64, |a, o| a.checked_add(o.value))
            .filter(|s| *s <= MAX_MONEY)
            .ok_or_else(|| err(format!("{height}: tx {k} output sum")))?;
        let mut inputs = Vec::new();
        if k > 0 {
            let mut in_sum: u64 = 0;
            for (input, jv) in tx.input.iter().zip(&jt.vin) {
                let (Some(id), Some(vout), Some(p)) = (&jv.txid, jv.vout, &jv.prevout) else {
                    return Err(err(format!("{height}: tx {k} prevout missing")));
                };
                if jv.coinbase.is_some()
                    || *id != input.previous_output.txid.to_string()
                    || vout != input.previous_output.vout
                {
                    return Err(err(format!("{height}: tx {k} outpoint mismatch")));
                }
                let value = btc_to_sats(p.value.get())
                    .ok_or_else(|| err(format!("{height}: tx {k} prevout value")))?;
                let created_height = p
                    .height
                    .filter(|h| *h <= height)
                    .ok_or_else(|| err(format!("{height}: tx {k} prevout height")))?;
                let script = hex::decode(&p.script_pub_key.hex).map_err(|e| err(e.to_string()))?;
                let out = TxOut { script, value };
                if let Some((raw, at)) = created.get(&input.previous_output) {
                    if *raw != out || *at != created_height {
                        return Err(err(format!(
                            "{height}: tx {k} prevout differs from the raw output it spends"
                        )));
                    }
                }
                in_sum = in_sum
                    .checked_add(value)
                    .filter(|s| *s <= MAX_MONEY)
                    .ok_or_else(|| err(format!("{height}: tx {k} input sum")))?;
                inputs.push(ChainInput {
                    value: out.value,
                    script: out.script,
                    created_height,
                });
            }
            let fee = in_sum
                .checked_sub(out_sum)
                .ok_or_else(|| err(format!("{height}: tx {k} spends more than its inputs")))?;
            fees = fees.saturating_add(fee);
        }
        for (vout, o) in outputs.iter().enumerate() {
            let n = u32::try_from(vout).map_err(|e| err(e.to_string()))?;
            created.insert(OutPoint { txid, vout: n }, (o.clone(), height));
        }
        let size = |n: usize| u32::try_from(n).map_err(|e| err(e.to_string()));
        txs.push(ChainTx {
            base_size: size(tx.base_size())?,
            total_size: size(tx.total_size())?,
            inputs,
            outputs,
        });
    }
    let coinbase_out: u64 = txs
        .first()
        .map(|t| t.outputs.iter().map(|o| o.value).sum())
        .unwrap_or(0);
    if coinbase_out > subsidy(height).saturating_add(fees) {
        return Err(err(format!(
            "{height}: coinbase pays more than subsidy plus fees"
        )));
    }
    Ok(txs)
}

/// One chain-statistics shard of a published window: its range and root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainShard {
    /// First height.
    pub from: u64,
    /// Last height (inclusive).
    pub to: u64,
    /// Root of `btc-chain-stats` on the shard.
    pub root: Hash32,
}

/// The hash a website pins for a window of chain statistics: domain, the
/// recipe id, the tip block hash (display order), then each shard as
/// `from (u64 BE) || to (u64 BE) || root`. Any edited row changes a root;
/// any moved, dropped or added shard changes the list.
pub fn chain_stats_commitment(recipe: &Hash32, tip: &[u8; 32], shards: &[ChainShard]) -> Hash32 {
    let mut b = Vec::with_capacity(19 + 64 + shards.len() * 48);
    b.extend_from_slice(b"onyx-chain-stats-v1");
    b.extend_from_slice(&recipe.0);
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
    fn the_commitment_moves_with_every_field() {
        let s = |from, to, a: u8| ChainShard {
            from,
            to,
            root: Hash32([a; 32]),
        };
        let base = [s(1, 1, 1), s(2, 2, 3)];
        let c = |shards: &[ChainShard], tip: [u8; 32], r: u8| {
            chain_stats_commitment(&Hash32([r; 32]), &tip, shards)
        };
        let reference = c(&base, [0; 32], 7);
        assert_ne!(reference, c(&base, [1; 32], 7));
        assert_ne!(reference, c(&base, [0; 32], 8));
        assert_ne!(reference, c(&[s(1, 1, 1)], [0; 32], 7));
        assert_ne!(reference, c(&[s(1, 2, 1), s(3, 3, 3)], [0; 32], 7));
        assert_ne!(reference, c(&[s(1, 1, 2), s(2, 2, 3)], [0; 32], 7));
    }

    use crate::node::{V3Block, V3Prevout, V3Script, V3Tx, V3Vin};
    use bitcoin::absolute::LockTime;
    use bitcoin::hashes::Hash;
    use bitcoin::transaction::Version;
    use bitcoin::{Amount, ScriptBuf, Sequence, Transaction, TxIn, Txid, Witness};
    use serde_json::value::RawValue;

    fn tx(inputs: Vec<OutPoint>, outputs: Vec<(&[u8], u64)>) -> Transaction {
        Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: inputs
                .into_iter()
                .map(|o| TxIn {
                    previous_output: o,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::new(),
                })
                .collect(),
            output: outputs
                .into_iter()
                .map(|(s, v)| bitcoin::TxOut {
                    value: Amount::from_sat(v),
                    script_pubkey: ScriptBuf::from_bytes(s.to_vec()),
                })
                .collect(),
        }
    }

    fn vin(o: &OutPoint, script: &[u8], btc: &str, height: Option<u64>) -> V3Vin {
        V3Vin {
            coinbase: None,
            txid: Some(o.txid.to_string()),
            vout: Some(o.vout),
            prevout: Some(V3Prevout {
                value: RawValue::from_string(btc.into()).expect("raw"),
                script_pub_key: V3Script {
                    hex: hex::encode(script),
                },
                height,
            }),
        }
    }

    /// Coinbase; A spends an output from block 969,000 (1,000 -> 900); B
    /// spends A:0 (created in this block, 970,000) with `b_height` as its
    /// prevout's creation height.
    fn sample(a_height: Option<u64>, b_height: Option<u64>) -> (Block, V3Block) {
        let outside = OutPoint {
            txid: Txid::from_byte_array([7; 32]),
            vout: 0,
        };
        let cb = tx(vec![OutPoint::null()], vec![(b"cb", 312_500_000 + 150)]);
        let a = tx(vec![outside], vec![(b"pay", 900)]);
        let a0 = OutPoint {
            txid: a.compute_txid(),
            vout: 0,
        };
        let b = tx(vec![a0], vec![(b"next", 850)]);
        let v3 = V3Block {
            hash: String::new(),
            height: 970_000,
            tx: vec![
                V3Tx {
                    txid: cb.compute_txid().to_string(),
                    vin: vec![V3Vin {
                        coinbase: Some("00".into()),
                        txid: None,
                        vout: None,
                        prevout: None,
                    }],
                },
                V3Tx {
                    txid: a.compute_txid().to_string(),
                    vin: vec![vin(&outside, b"src", "0.00001000", a_height)],
                },
                V3Tx {
                    txid: b.compute_txid().to_string(),
                    vin: vec![vin(&a0, b"pay", "0.00000900", b_height)],
                },
            ],
        };
        let header = bitcoin::block::Header {
            version: bitcoin::block::Version::TWO,
            prev_blockhash: bitcoin::BlockHash::all_zeros(),
            merkle_root: bitcoin::TxMerkleNode::all_zeros(),
            time: 0,
            bits: bitcoin::pow::CompactTarget::from_consensus(0x1d00ffff),
            nonce: 0,
        };
        (
            Block {
                header,
                txdata: vec![cb, a, b],
            },
            v3,
        )
    }

    #[test]
    fn honest_prevouts_carry_their_creation_heights_and_sizes() {
        let (block, v3) = sample(Some(969_000), Some(970_000));
        let txs = block_txs(&block, &v3, 970_000, &mut BTreeMap::new()).expect("honest");
        assert_eq!(txs[1].inputs[0].created_height, 969_000);
        assert_eq!(txs[2].inputs[0].created_height, 970_000);
        assert_eq!(txs[2].inputs[0].value, 900);
        let raw = serialize(&block.txdata[1]).len() as u32;
        assert_eq!((txs[1].base_size, txs[1].total_size), (raw, raw));
    }

    /// CHAIN-2: a prevout inside the range must name the height its output
    /// was created at; a missing or future height is refused.
    #[test]
    fn creation_heights_are_checked() {
        for (a, b, why) in [
            (Some(969_000), Some(969_999), "differs from the raw output"),
            (Some(969_000), None, "prevout height"),
            (Some(970_001), Some(970_000), "prevout height"),
        ] {
            let (block, v3) = sample(a, b);
            let e = block_txs(&block, &v3, 970_000, &mut BTreeMap::new()).expect_err("lie");
            assert!(format!("{e}").contains(why), "{e}");
        }
    }

    /// CHAIN-4: an OP_RETURN payload never enters the source; the recipe row is
    /// the same with or without it (it reads only that the output is unspendable).
    #[test]
    fn op_return_payloads_are_never_carried() {
        assert_eq!(carried_script(b"\x6a\x0bMined by me"), vec![0x6a]);
        assert_eq!(carried_script(b"\x00\x14abc"), b"\x00\x14abc".to_vec());
        assert_eq!(carried_script(b""), Vec::<u8>::new());
        let (mut block, mut v3) = sample(Some(969_000), Some(970_000));
        block.txdata[0].output.push(bitcoin::TxOut {
            value: Amount::from_sat(0),
            script_pubkey: ScriptBuf::from_bytes(b"\x6a\x05hello".to_vec()),
        });
        v3.tx[0].txid = block.txdata[0].compute_txid().to_string();
        let txs = block_txs(&block, &v3, 970_000, &mut BTreeMap::new()).expect("honest");
        assert_eq!(txs[0].outputs[1].script, vec![0x6a]);
        let bytes = borsh::to_vec(&txs).expect("borsh");
        assert!(!bytes.windows(5).any(|w| w == b"hello"));
    }

    #[test]
    fn a_range_needs_a_parent_and_its_files() {
        let pin = NodePin {
            min_bits: crate::node::MAINNET_MIN_BITS,
            tip: None,
        };
        let none: Vec<PathBuf> = vec![PathBuf::from("/nonexistent-onyx-dir")];
        assert!(load_chain_range(&none, 0, 0, &pin).is_err());
        assert!(load_chain_range(&none, 5, 4, &pin).is_err());
        let e = load_chain_range(&none, 10, 10, &pin).expect_err("no files");
        assert!(format!("{e}").contains("no raw block file"), "{e}");
    }
}
