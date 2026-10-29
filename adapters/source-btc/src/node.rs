//! Bitcoin source from a node (`scripts/fetch-btc-node.sh`): the exact block
//! bytes (`getblock <hash> 0`) plus each input's prevout (`getblock <hash> 3`).
//!
//! What is checked here (loop 18, after L16-F1/F2):
//! - every header meets a pinned minimum difficulty ([`NodePin::min_bits`],
//!   set per difficulty epoch), so a fake block costs as much work as a real
//!   one; optionally the tip must equal a pinned hash;
//! - proof of work, parent links, BIP34 height, transaction Merkle root, no
//!   duplicate txids, witness commitment; outputs come from the raw bytes;
//! - prevouts that point inside the range must equal the raw outputs they
//!   spend; every transaction's fee is >= 0; the coinbase pays at most subsidy
//!   plus fees; amounts are at most 21M BTC.
//!
//! Not checked: prevouts created before the range come from the node's index.
//! They are trusted from the node, so a validator must use its own node; two
//! validators with independent nodes then agree on the content hash, and a
//! lying mirror shows up as a content-hash mismatch against the shard pin.

use crate::{BtcAdapterError, CHAIN_ID};
use bitcoin::consensus::deserialize;
use bitcoin::pow::{CompactTarget, Target};
use bitcoin::{Block, OutPoint};
use onyx_btc_schema::{BlockDelta, BtcSource, TxDelta, TxOut, SCHEMA_VERSION};
use onyx_core::{keccak256, ChainId, Hash32, Snapshot};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

#[derive(Deserialize)]
pub(crate) struct V3Block {
    pub(crate) hash: String,
    pub(crate) height: u64,
    pub(crate) tx: Vec<V3Tx>,
}

#[derive(Deserialize)]
pub(crate) struct V3Tx {
    pub(crate) txid: String,
    pub(crate) vin: Vec<V3Vin>,
}

#[derive(Deserialize)]
pub(crate) struct V3Vin {
    #[serde(default)]
    pub(crate) coinbase: Option<String>,
    #[serde(default)]
    pub(crate) txid: Option<String>,
    #[serde(default)]
    pub(crate) vout: Option<u32>,
    #[serde(default)]
    pub(crate) prevout: Option<V3Prevout>,
}

#[derive(Deserialize)]
pub(crate) struct V3Prevout {
    pub(crate) value: Box<RawValue>,
    #[serde(rename = "scriptPubKey")]
    pub(crate) script_pub_key: V3Script,
    /// Height of the block that created the output (read by `chain`; the
    /// balance sources ignore it).
    #[serde(default)]
    pub(crate) height: Option<u64>,
}

#[derive(Deserialize)]
pub(crate) struct V3Script {
    pub(crate) hex: String,
}

fn err(s: impl Into<String>) -> BtcAdapterError {
    BtcAdapterError::Fixture(s.into())
}

/// Parse a node amount in BTC, written as a plain decimal with at most 8
/// decimals (e.g. `0.00012330`), into satoshis. No floats.
pub fn btc_to_sats(text: &str) -> Option<u64> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) || frac.len() > 8 {
        return None;
    }
    let w: u64 = whole.parse().ok()?;
    let mut f: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse().ok()?
    };
    for _ in frac.len()..8 {
        f = f.checked_mul(10)?;
    }
    let sats = w.checked_mul(100_000_000)?.checked_add(f)?;
    (sats <= MAX_MONEY).then_some(sats)
}

/// 21 million BTC in satoshis: no amount can exceed it.
pub const MAX_MONEY: u64 = 21_000_000 * 100_000_000;
/// Largest raw block file accepted, in hex characters (4 MB block, L16-F11).
pub const MAX_RAW_HEX: u64 = 2 * 4_000_000 + 2;
/// Largest decompressed verbosity-3 JSON accepted per block (L16-F11).
pub const MAX_V3_JSON: u64 = 256 << 20;

/// Default difficulty floor: a quarter of the mainnet difficulty observed at
/// heights 970,543-970,566 (bits `0x17021ef0`, about 132.7 T, read from a node
/// on 2026-10-08). Difficulty can fall at most 4x per 2016-block period, so this
/// holds for the next period; governance must raise it each period.
pub const MAINNET_MIN_BITS: u32 = 0x1708_7bc0;

/// What a node range must satisfy beyond the raw-byte checks (L16-F1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodePin {
    /// Compact target (`bits`) of the easiest allowed header: a block must be
    /// at least this hard. Pinned per difficulty epoch (governance or shard).
    pub min_bits: u32,
    /// If set, the last block must be this (height, display hash).
    pub tip: Option<(u64, String)>,
}

/// Block subsidy at a height (mainnet halving every 210,000 blocks).
pub fn subsidy(height: u64) -> u64 {
    let halvings = height / 210_000;
    if halvings >= 64 {
        0
    } else {
        (50 * 100_000_000u64) >> halvings
    }
}

/// One block's files: raw hex and gzipped verbosity-3 JSON.
pub(crate) fn read_block(dir: &Path, height: u64) -> Result<(Block, V3Block), BtcAdapterError> {
    let mut raw = String::new();
    std::fs::File::open(dir.join(format!("{height}.raw.hex")))
        .map_err(|e| err(e.to_string()))?
        .take(MAX_RAW_HEX + 1)
        .read_to_string(&mut raw)
        .map_err(|e| err(e.to_string()))?;
    if raw.len() as u64 > MAX_RAW_HEX {
        return Err(err(format!("{height}: raw block too large")));
    }
    let bytes = hex::decode(raw.trim()).map_err(|e| err(e.to_string()))?;
    // `deserialize` refuses trailing bytes, so the file is exactly one block.
    let block: Block = deserialize(&bytes).map_err(|e| err(e.to_string()))?;
    let gz = std::fs::File::open(dir.join(format!("{height}.v3.json.gz")))
        .map_err(|e| err(e.to_string()))?;
    let mut text = String::new();
    flate2::read::GzDecoder::new(gz)
        .take(MAX_V3_JSON + 1)
        .read_to_string(&mut text)
        .map_err(|e| err(e.to_string()))?;
    if text.len() as u64 > MAX_V3_JSON {
        return Err(err(format!("{height}: block JSON too large")));
    }
    let v3: V3Block = serde_json::from_str(&text).map_err(|e| err(e.to_string()))?;
    Ok((block, v3))
}

/// Load and check blocks `from..=to` from `dir`. Returns the canonical source
/// bytes (Borsh `BtcSource`) and the snapshot that pins them.
pub fn load_node_range(
    dir: &Path,
    from: u64,
    to: u64,
    pin: &NodePin,
) -> Result<(Vec<u8>, Snapshot), BtcAdapterError> {
    if from > to {
        return Err(err("empty range"));
    }
    let mut blocks = Vec::new();
    let mut prev: Option<bitcoin::BlockHash> = None;
    let mut created_in_range: BTreeMap<OutPoint, TxOut> = BTreeMap::new();
    for height in from..=to {
        let (block, v3) = read_block(dir, height)?;
        let hash = block.block_hash();
        check_header(&block.header, pin).map_err(|e| err(format!("{height}: {e}")))?;
        if prev.is_some_and(|p| p != block.header.prev_blockhash) {
            return Err(err(format!("{height}: parent link")));
        }
        prev = Some(hash);
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
        let txs = block_txs(&block, &v3, height, &mut created_in_range)?;
        let mut display = [0u8; 32];
        hex::decode_to_slice(hash.to_string(), &mut display).map_err(|e| err(e.to_string()))?;
        blocks.push(BlockDelta {
            height,
            hash: display,
            txs,
        });
    }
    let src = BtcSource {
        schema_version: SCHEMA_VERSION,
        chain: CHAIN_ID.into(),
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

/// Check one raw 80-byte header against a pinned minimum difficulty, the
/// same check [`load_node_range`] applies. Returns the block hash and the
/// parent hash, both in internal byte order. Used by the browser verifier.
pub fn check_raw_header(raw: &[u8], min_bits: u32) -> Result<([u8; 32], [u8; 32]), String> {
    if raw.len() != 80 {
        return Err("header must be 80 bytes".into());
    }
    let h: bitcoin::block::Header = deserialize(raw).map_err(|e| e.to_string())?;
    check_header(
        &h,
        &NodePin {
            min_bits,
            tip: None,
        },
    )?;
    Ok((
        bitcoin::hashes::Hash::to_byte_array(h.block_hash()),
        bitcoin::hashes::Hash::to_byte_array(h.prev_blockhash),
    ))
}

/// Blocks per mainnet difficulty period: the target may change only at a
/// height that is a multiple of this (Bitcoin Core
/// `Consensus::Params::DifficultyAdjustmentInterval`).
pub const DIFFICULTY_PERIOD: u64 = 2016;

/// Mainnet difficulty rule between a header at `height` and the one before
/// it (THREATS RES-20): inside a period the bits stay the same; at a period's
/// first block the target moves at most 4x either way (Bitcoin Core
/// `PermittedDifficultyTransition`, compact-rounded bounds, no minimum-
/// difficulty exception). Timestamps are not read, so this is the rule's
/// necessary condition, not the exact retarget.
pub fn permitted_bits_transition(height: u64, prev_bits: u32, bits: u32) -> bool {
    if !height.is_multiple_of(DIFFICULTY_PERIOD) {
        return bits == prev_bits;
    }
    let old = Target::from_compact(CompactTarget::from_consensus(prev_bits));
    let new = Target::from_compact(CompactTarget::from_consensus(bits));
    let hi = Target::from_compact(
        old.max_transition_threshold(bitcoin::Network::Bitcoin)
            .to_compact_lossy(),
    );
    let lo = Target::from_compact(old.min_transition_threshold().to_compact_lossy());
    lo <= new && new <= hi
}

/// Header checks: at least the pinned difficulty, never easier than the
/// mainnet limit, and valid proof of work for its own target.
pub(crate) fn check_header(h: &bitcoin::block::Header, pin: &NodePin) -> Result<(), String> {
    let floor = Target::from_compact(CompactTarget::from_consensus(pin.min_bits));
    if h.target() > floor || h.target() > Target::MAX_ATTAINABLE_MAINNET {
        return Err("easier than the pinned difficulty".into());
    }
    h.validate_pow(h.target())
        .map(|_| ())
        .map_err(|_| "proof of work".into())
}

/// Transactions of one block, with prevout checks (L16-F2). Outputs created in
/// earlier blocks of the range (or earlier in this block) are recorded in
/// `created_in_range`, and a prevout pointing at one must equal it exactly.
fn block_txs(
    block: &Block,
    v3: &V3Block,
    height: u64,
    created_in_range: &mut BTreeMap<OutPoint, TxOut>,
) -> Result<Vec<TxDelta>, BtcAdapterError> {
    let mut txs = Vec::with_capacity(block.txdata.len());
    let mut fees: u64 = 0;
    for (k, (tx, jt)) in block.txdata.iter().zip(&v3.tx).enumerate() {
        if tx.compute_txid().to_string() != jt.txid {
            return Err(err(format!("{height}: tx {k} differs from node index")));
        }
        if tx.is_coinbase() != (k == 0) || tx.input.len() != jt.vin.len() {
            return Err(err(format!("{height}: tx {k} inputs")));
        }
        let created: Vec<TxOut> = tx
            .output
            .iter()
            .map(|o| TxOut {
                script: o.script_pubkey.to_bytes(),
                value: o.value.to_sat(),
            })
            .collect();
        let out_sum = created
            .iter()
            .try_fold(0u64, |a, o| a.checked_add(o.value))
            .filter(|s| *s <= MAX_MONEY)
            .ok_or_else(|| err(format!("{height}: tx {k} output sum")))?;
        let mut spent = Vec::new();
        if k > 0 {
            let mut in_sum: u64 = 0;
            for (input, jv) in tx.input.iter().zip(&jt.vin) {
                let (Some(txid), Some(vout), Some(p)) = (&jv.txid, jv.vout, &jv.prevout) else {
                    return Err(err(format!("{height}: tx {k} prevout missing")));
                };
                if jv.coinbase.is_some()
                    || *txid != input.previous_output.txid.to_string()
                    || vout != input.previous_output.vout
                {
                    return Err(err(format!("{height}: tx {k} outpoint mismatch")));
                }
                let value = btc_to_sats(p.value.get())
                    .ok_or_else(|| err(format!("{height}: tx {k} prevout value")))?;
                let script = hex::decode(&p.script_pub_key.hex).map_err(|e| err(e.to_string()))?;
                let out = TxOut { script, value };
                if let Some(raw) = created_in_range.get(&input.previous_output) {
                    if *raw != out {
                        return Err(err(format!(
                            "{height}: tx {k} prevout differs from the raw output it spends"
                        )));
                    }
                }
                in_sum = in_sum
                    .checked_add(value)
                    .filter(|s| *s <= MAX_MONEY)
                    .ok_or_else(|| err(format!("{height}: tx {k} input sum")))?;
                spent.push(out);
            }
            let fee = in_sum
                .checked_sub(out_sum)
                .ok_or_else(|| err(format!("{height}: tx {k} spends more than its inputs")))?;
            fees = fees.saturating_add(fee);
        }
        let txid = tx.compute_txid();
        for (vout, o) in created.iter().enumerate() {
            let n = u32::try_from(vout).map_err(|e| err(e.to_string()))?;
            created_in_range.insert(OutPoint { txid, vout: n }, o.clone());
        }
        txs.push(TxDelta { spent, created });
    }
    // Consensus: the coinbase may claim at most subsidy plus fees.
    let coinbase_out: u64 = txs
        .first()
        .map(|t| t.created.iter().map(|o| o.value).sum())
        .unwrap_or(0);
    if coinbase_out > subsidy(height).saturating_add(fees) {
        return Err(err(format!(
            "{height}: coinbase pays more than subsidy plus fees"
        )));
    }
    Ok(txs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::absolute::LockTime;
    use bitcoin::hashes::Hash;
    use bitcoin::transaction::Version;
    use bitcoin::{Amount, ScriptBuf, Sequence, Transaction, TxIn, Txid, Witness};

    #[test]
    fn amounts_parse_exactly() {
        assert_eq!(btc_to_sats("0.00012330"), Some(12_330));
        assert_eq!(btc_to_sats("21000000.00000000"), Some(MAX_MONEY));
        assert_eq!(btc_to_sats("1"), Some(100_000_000));
        assert_eq!(btc_to_sats("0.5"), Some(50_000_000));
        // L16-F2: above 21M BTC is refused (this one would be u64::MAX).
        assert_eq!(btc_to_sats("184467440737.09551615"), None);
        assert_eq!(btc_to_sats("21000000.00000001"), None);
        for bad in ["", ".1", "1e-05", "-1", "0.000000001", "1.2.3", " 1"] {
            assert_eq!(btc_to_sats(bad), None, "{bad:?}");
        }
        assert_eq!(subsidy(0), 5_000_000_000);
        assert_eq!(subsidy(970_000), 312_500_000);
    }

    fn txout(script: &[u8], sats: u64) -> bitcoin::TxOut {
        bitcoin::TxOut {
            value: Amount::from_sat(sats),
            script_pubkey: ScriptBuf::from_bytes(script.to_vec()),
        }
    }

    fn tx(inputs: Vec<OutPoint>, outputs: Vec<bitcoin::TxOut>) -> Transaction {
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
            output: outputs,
        }
    }

    fn vin(o: &OutPoint, script: &[u8], btc: &str) -> V3Vin {
        V3Vin {
            coinbase: None,
            txid: Some(o.txid.to_string()),
            vout: Some(o.vout),
            prevout: Some(V3Prevout {
                value: RawValue::from_string(btc.into()).expect("raw"),
                script_pub_key: V3Script {
                    hex: hex::encode(script),
                },
                height: None,
            }),
        }
    }

    /// Coinbase, A spends an outside output (1000 -> 900, fee 100), B spends A:0.
    fn sample(a_prev_btc: &str, b_prev_btc: &str, coinbase_sats: u64) -> (Block, V3Block) {
        let outside = OutPoint {
            txid: Txid::from_byte_array([7; 32]),
            vout: 0,
        };
        let cb = tx(vec![OutPoint::null()], vec![txout(b"cb", coinbase_sats)]);
        let a = tx(vec![outside], vec![txout(b"okx", 900)]);
        let a0 = OutPoint {
            txid: a.compute_txid(),
            vout: 0,
        };
        let b = tx(vec![a0], vec![txout(b"other", 850)]);
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
                    vin: vec![vin(&outside, b"src", a_prev_btc)],
                },
                V3Tx {
                    txid: b.compute_txid().to_string(),
                    vin: vec![vin(&a0, b"okx", b_prev_btc)],
                },
            ],
        };
        let header = bitcoin::block::Header {
            version: bitcoin::block::Version::TWO,
            prev_blockhash: bitcoin::BlockHash::all_zeros(),
            merkle_root: bitcoin::TxMerkleNode::all_zeros(),
            time: 0,
            bits: CompactTarget::from_consensus(0x1d00ffff),
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
    fn honest_prevouts_pass() {
        let (block, v3) = sample("0.00001000", "0.00000900", 312_500_000 + 150);
        let txs = block_txs(&block, &v3, 970_000, &mut BTreeMap::new()).expect("honest");
        assert_eq!(
            txs[2].spent[0],
            TxOut {
                script: b"okx".to_vec(),
                value: 900
            }
        );
    }

    /// L16-F2: a prevout inside the range must equal the raw output it spends.
    #[test]
    fn in_range_prevout_lie_refused() {
        let (block, v3) = sample("0.00001000", "0.00000901", 312_500_000);
        let e = block_txs(&block, &v3, 970_000, &mut BTreeMap::new()).expect_err("lie");
        assert!(
            format!("{e}").contains("differs from the raw output"),
            "{e}"
        );
    }

    /// L16-F2: negative fee (inputs below outputs) is refused.
    #[test]
    fn negative_fee_refused() {
        let (block, v3) = sample("0.00000800", "0.00000900", 312_500_000);
        let e = block_txs(&block, &v3, 970_000, &mut BTreeMap::new()).expect_err("fee");
        assert!(
            format!("{e}").contains("spends more than its inputs"),
            "{e}"
        );
    }

    /// Consensus: the coinbase claims at most subsidy plus fees (here 150 sats).
    #[test]
    fn coinbase_over_subsidy_refused() {
        let (block, v3) = sample("0.00001000", "0.00000900", 312_500_000 + 151);
        let e = block_txs(&block, &v3, 970_000, &mut BTreeMap::new()).expect_err("coinbase");
        assert!(format!("{e}").contains("coinbase"), "{e}");
    }

    /// L16-F1: a header easier than the pinned difficulty is refused before any
    /// proof-of-work check; a real mainnet header passes at its own difficulty.
    #[test]
    fn difficulty_pin_refuses_cheap_blocks() {
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/btc/blocks-921080-921082.json"
            ))
            .expect("fixture"),
        )
        .expect("json");
        let hex_header = fixture["blocks"][0]["header"].as_str().expect("header");
        let header: bitcoin::block::Header =
            deserialize(&hex::decode(hex_header).expect("hex")).expect("header");
        let own = header.bits.to_consensus();
        let pin = |min_bits| NodePin {
            min_bits,
            tip: None,
        };
        assert_eq!(check_header(&header, &pin(own)), Ok(()));
        // A pin twice as hard (half the target) refuses the real header.
        let harder = (own & 0xff00_0000) | ((own & 0x00ff_ffff) / 2);
        assert!(check_header(&header, &pin(harder)).is_err());
        // The reviewer's fabricated block used difficulty 1 (bits 0x1d00ffff).
        let mut cheap = header;
        cheap.bits = CompactTarget::from_consensus(0x1d00ffff);
        assert_eq!(
            check_header(&cheap, &pin(own)),
            Err("easier than the pinned difficulty".into())
        );
    }
}
