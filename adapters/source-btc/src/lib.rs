//! Bitcoin source adapter: pinned block fixtures, and blocks read from a node ([`node`],
//! `scripts/fetch-btc-node.sh`), each checked before any recipe runs.
//! This crate may depend on `onyx-core`; no core crate may depend on it.

pub mod address_proof;
pub mod budget;
pub mod chain;
pub mod history;
pub mod node;
pub mod pools;
mod proof_vectors;
pub mod reserve;
pub mod rosters;
pub mod signed_roster;

pub use address_proof::{Bip322Proof, BTC_MAX_ATTESTATION_WINDOW};

use onyx_btc_schema::{BlockDelta, BtcSource, TxDelta, TxOut, SCHEMA_VERSION};
use onyx_core::{
    keccak256, AddressCanon, ChainId, DataRights, EntityKind, Hash32, LabelSetRef, MetricInfo,
    ProofLevel, Registry, Snapshot, SourceAdapter, SourceId, StaticRegistry, ValueKind,
};
use onyx_labels::{Label, LabelSet};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Source id for Bitcoin mainnet data.
pub const SOURCE_ID: &str = "chain.btc.mainnet";
/// Chain id.
pub const CHAIN_ID: &str = "btc.mainnet";
/// Network domain of the fixture registry (attestation statements name it).
pub const DEVNET_DOMAIN: &str = "onyx.devnet";
/// Metric produced by `btc-entity-balance`.
pub const METRIC_ENTITY_FLOW: &str = "crypto.btc.entity_flow";
/// Metric id of the per-block entity-flow series (`btc-entity-flow-series`).
pub const METRIC_ENTITY_FLOW_SERIES: &str = "crypto.btc.entity_flow_series";
/// Schema tag of `EntityBlockFlowRow`: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_ENTITY_FLOW_SERIES: &[u8] = b"onyx.btc.entity-block-flow-row.v1";
/// Track-record period of [`METRIC_ENTITY_FLOW`] and [`METRIC_ENTITY_FLOW_SERIES`]
/// (ADR-0036 amendment 4, docs/spec/proof-ladder.md "Close-time rules"): a
/// fact is the flow over one UTC day `[as_of - 86,400, as_of)`, `as_of` a
/// midnight; the day's blocks are those whose median time past (BIP 113, which
/// never decreases) falls in it. The rule itself is onyx-track's `Close::PeriodStart`
/// with these numbers; this crate does not depend on onyx-track (TR-11).
pub const TRACK_FLOW_PERIOD_SECS: u64 = 86_400;
/// Statements on a flow day close this long before the day starts. Bitcoin
/// Core accepts a block timestamped up to 2 hours ahead of its own clock
/// (`MAX_FUTURE_BLOCK_TIME`), so a block whose median time past is in the day
/// (six of its eleven predecessors timestamped in the day) is accepted no
/// earlier than 2 hours before the day starts.
pub const TRACK_FLOW_LEAD_SECS: u64 = 7_200;
/// Metric of the per-entity reserve balance (`btc-entity-reserve`, ADR-0034).
pub const METRIC_ENTITY_RESERVE: &str = "crypto.btc.entity_reserve";
/// Schema tag of `EntityReserveRow`: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_ENTITY_RESERVE: &[u8] = onyx_reserve_schema::SCHEMA_ENTITY_RESERVE;
/// Metric of the mining-pool block counts (`btc-pool-blocks`, ADR-0032).
pub const METRIC_POOL_BLOCKS: &str = "crypto.btc.pool_blocks";
/// Schema tag of `PoolBlocksRow`: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_POOL_BLOCKS: &[u8] = b"onyx.btc.pool-blocks-row.v1";
/// Metric of the per-block chain statistics (`btc-chain-stats`, ADR-0043).
pub const METRIC_CHAIN_STATS: &str = "crypto.btc.chain_stats";
/// Schema tag of `ChainStatsRow`: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_CHAIN_STATS: &[u8] = onyx_chain_schema::SCHEMA_CHAIN_STATS;
/// Level of a pool-block shard (AUD-2-F6, ADR-0032 amendment 1): the
/// coinbase tags are on chain and the rerun checks the counting (level 1), but
/// which company a tag means comes from the third-party mining-pools list
/// pinned in the shard's params, which nothing re-proves. Like a published
/// company list, that mapping is a claim: level 4. Under the default money
/// policy (levels 1 and 3) a market on a pool row is refused.
pub const POOL_TABLE_LEVEL: ProofLevel = ProofLevel::BondedClaim;

/// Level of a reserve shard's base (ADR-0034 Amendment 1): the base in its
/// params is one node's scan rolled back by flows, which the rerun does not
/// check (miner and validators agree on any base). The serving check binds it
/// to the committed scan off chain; on chain it is a claim: level 4, also
/// under a label set that is stronger (OKX's signed subset alone, level 3).
pub const RESERVE_BASE_LEVEL: ProofLevel = ProofLevel::BondedClaim;

/// Level of the inputs a shard's recipe rerun does not check (AUD-2R-F3),
/// read from the spec itself, never from operator config: a pool-blocks shard
/// (its schema) pins the third-party pool table in its params, so it is
/// [`POOL_TABLE_LEVEL`]; a reserve shard pins its base, [`RESERVE_BASE_LEVEL`].
/// `None` for every other shard.
pub fn input_level(spec: &onyx_core::ShardSpec) -> Option<ProofLevel> {
    if spec.schema == onyx_core::keccak256(SCHEMA_POOL_BLOCKS) {
        Some(POOL_TABLE_LEVEL)
    } else if spec.schema == onyx_core::keccak256(SCHEMA_ENTITY_RESERVE) {
        Some(RESERVE_BASE_LEVEL)
    } else {
        None
    }
}

/// Minimum levels the registry's owner registers with each metric
/// (`OnyxRegistry.registerMetric` with its floor, keyed by recipe: AUD-2R-F3, REG-F): `attestShard` refuses
/// a stronger level, so a coordinator cannot attest a pool shard at level 1.
/// Pinned on both sides by `fixtures/registry/metric-min-levels.json`.
pub const METRIC_MIN_LEVELS: [(&str, ProofLevel); 2] = [
    (METRIC_POOL_BLOCKS, POOL_TABLE_LEVEL),
    (METRIC_ENTITY_RESERVE, RESERVE_BASE_LEVEL),
];

/// Adapter error.
#[derive(Debug, thiserror::Error)]
pub enum BtcAdapterError {
    /// Snapshot is not a Bitcoin block snapshot, or names another chain.
    #[error("snapshot is not a btc.mainnet block snapshot")]
    WrongSnapshot,
    /// Snapshot height or hash does not match the fixture's last block.
    #[error("snapshot does not match fixture tip: {0}")]
    SnapshotMismatch(String),
    /// Fixture file unreadable or malformed.
    #[error("fixture: {0}")]
    Fixture(String),
    /// Fixture checksum differs from `fixtures/SHA256SUMS`.
    #[error("checksum mismatch for {0}")]
    Checksum(String),
}

#[derive(Deserialize)]
struct FixtureDoc {
    chain: String,
    blocks: Vec<FixtureBlock>,
}

#[derive(Deserialize)]
struct FixtureBlock {
    height: u64,
    hash: String,
    header: String,
    txs: Vec<FixtureTx>,
}

#[derive(Deserialize)]
struct FixtureTx {
    /// Display-order txid, as computed from the raw tx at fetch time.
    txid: String,
    /// `[prev_txid, prev_vout, prevout_script_hex, prevout_value]`; empty for coinbase.
    vin: Vec<(String, u32, String, u64)>,
    vout: Vec<(String, u64)>,
}

fn sha256d(b: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(b)).into()
}

/// Proof-of-work target from compact `nBits`, as 32 big-endian bytes.
fn target_be(bits: u32) -> [u8; 32] {
    let exp = (bits >> 24) as usize;
    let mant = bits & 0x00ff_ffff;
    let mut t = [0u8; 32];
    for (k, byte) in mant.to_be_bytes()[1..].iter().enumerate() {
        // mantissa occupies bytes [32 - exp, 32 - exp + 3)
        if let Some(pos) = (32 + k).checked_sub(exp) {
            if pos < 32 {
                t[pos] = *byte;
            }
        }
    }
    t
}

/// Mainnet proof-of-work limit (`powLimit`, compact 0x1d00ffff).
const POW_LIMIT_BITS: u32 = 0x1d00_ffff;

/// Tips confirmed by two independent APIs at fetch time (mempool.space and
/// blockstream.info, 2026-10-06). A fixture must end on one of these (R-FIX-3).
pub const ANCHORS: &[(u64, &str)] = &[(
    921_082,
    "0000000000000000000194cae50bac6b72bca5fc131413d737df8ad10992125a",
)];

/// Merkle root (internal byte order) of display-order txids, Bitcoin rules.
fn merkle_root(txids: &[[u8; 32]]) -> [u8; 32] {
    let mut level: Vec<[u8; 32]> = txids
        .iter()
        .map(|t| {
            let mut x = *t;
            x.reverse();
            x
        })
        .collect();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            if let Some(last) = level.last().copied() {
                level.push(last);
            }
        }
        level = level
            .chunks(2)
            .map(|p| {
                let mut b = [0u8; 64];
                b[..32].copy_from_slice(&p[0]);
                b[32..].copy_from_slice(&p[1]);
                sha256d(&b)
            })
            .collect();
    }
    level.first().copied().unwrap_or([0u8; 32])
}

/// Offline header checks: hash, proof of work (and target no easier than
/// powLimit), link to the previous block (R-FIX-2, R-FIX-3).
fn check_header(
    header: &[u8],
    hash_display: &[u8; 32],
    prev: Option<&[u8; 32]>,
) -> Result<(), BtcAdapterError> {
    if header.len() != 80 {
        return Err(BtcAdapterError::Fixture("header is not 80 bytes".into()));
    }
    let mut h = sha256d(header);
    h.reverse();
    if &h != hash_display {
        return Err(BtcAdapterError::Fixture("header hash mismatch".into()));
    }
    let bits = u32::from_le_bytes([header[72], header[73], header[74], header[75]]);
    let negative = bits & 0x0080_0000 != 0;
    if negative || target_be(bits) > target_be(POW_LIMIT_BITS) || h > target_be(bits) {
        return Err(BtcAdapterError::Fixture(
            "insufficient proof of work".into(),
        ));
    }
    if let Some(p) = prev {
        let mut link = [0u8; 32];
        link.copy_from_slice(&header[4..36]);
        link.reverse();
        if &link != p {
            return Err(BtcAdapterError::Fixture("prev-hash link broken".into()));
        }
    }
    Ok(())
}

fn hex32(s: &str) -> Result<[u8; 32], BtcAdapterError> {
    let v = hex::decode(s).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    v.try_into()
        .map_err(|_| BtcAdapterError::Fixture("hash is not 32 bytes".into()))
}

fn outs(list: &[(String, u64)]) -> Result<Vec<TxOut>, BtcAdapterError> {
    list.iter()
        .map(|(s, v)| {
            Ok(TxOut {
                script: hex::decode(s).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?,
                value: *v,
            })
        })
        .collect()
}

/// Check `path` against its line in `<fixtures>/SHA256SUMS`.
pub fn verify_checksum(fixtures_dir: &Path, rel: &str) -> Result<(), BtcAdapterError> {
    let sums = std::fs::read_to_string(fixtures_dir.join("SHA256SUMS"))
        .map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    let want = sums
        .lines()
        .find_map(|l| {
            l.split_once("  ")
                .filter(|(_, f)| *f == rel)
                .map(|(h, _)| h)
        })
        .ok_or_else(|| BtcAdapterError::Checksum(rel.into()))?;
    let bytes = std::fs::read(fixtures_dir.join(rel))
        .map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    if hex::encode(Sha256::digest(&bytes)) != want {
        return Err(BtcAdapterError::Checksum(rel.into()));
    }
    Ok(())
}

/// Reads a pinned block range from a checksummed fixture file.
#[derive(Debug, Clone)]
pub struct BtcFixtureAdapter {
    fixtures_dir: PathBuf,
    blocks_rel: String,
}

impl BtcFixtureAdapter {
    /// `blocks_rel` is relative to `fixtures_dir`, e.g. `btc/blocks-921080-921082.json`.
    pub fn new(fixtures_dir: impl Into<PathBuf>, blocks_rel: &str) -> Self {
        BtcFixtureAdapter {
            fixtures_dir: fixtures_dir.into(),
            blocks_rel: blocks_rel.into(),
        }
    }

    /// Load and checksum the fixture into the recipe source schema.
    pub fn source(&self) -> Result<BtcSource, BtcAdapterError> {
        verify_checksum(&self.fixtures_dir, &self.blocks_rel)?;
        let text = std::fs::read_to_string(self.fixtures_dir.join(&self.blocks_rel))
            .map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
        let doc: FixtureDoc =
            serde_json::from_str(&text).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
        if doc.chain != CHAIN_ID {
            return Err(BtcAdapterError::WrongSnapshot);
        }
        let mut blocks = Vec::with_capacity(doc.blocks.len());
        let mut prev: Option<[u8; 32]> = None;
        for b in &doc.blocks {
            let hash = hex32(&b.hash)?;
            let header =
                hex::decode(&b.header).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
            check_header(&header, &hash, prev.as_ref())?;
            prev = Some(hash);
            // Txids must hash to the header's Merkle root, with no duplicates (CVE-2012-2459).
            let txids = b
                .txs
                .iter()
                .map(|t| hex32(&t.txid))
                .collect::<Result<Vec<_>, _>>()?;
            let mut sorted = txids.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if sorted.len() != txids.len() || merkle_root(&txids)[..] != header[36..68] {
                return Err(BtcAdapterError::Fixture(
                    "txids do not match merkle root".into(),
                ));
            }
            let mut txs = Vec::with_capacity(b.txs.len());
            for (k, t) in b.txs.iter().enumerate() {
                // Coinbase (and only the coinbase) is first and spends nothing.
                if t.vin.is_empty() != (k == 0) {
                    return Err(BtcAdapterError::Fixture("coinbase position".into()));
                }
                let spent: Vec<(String, u64)> =
                    t.vin.iter().map(|(_, _, s, v)| (s.clone(), *v)).collect();
                txs.push(TxDelta {
                    spent: outs(&spent)?,
                    created: outs(&t.vout)?,
                });
            }
            blocks.push(BlockDelta {
                height: b.height,
                hash,
                txs,
            });
        }
        let src = BtcSource {
            schema_version: SCHEMA_VERSION,
            chain: CHAIN_ID.into(),
            blocks,
        };
        if !src.is_canonical() {
            return Err(BtcAdapterError::Fixture("blocks not contiguous".into()));
        }
        let tip = src.blocks.last().map(|b| (b.height, hex::encode(b.hash)));
        if !ANCHORS
            .iter()
            .any(|(h, hash)| tip.as_ref() == Some(&(*h, (*hash).to_string())))
        {
            return Err(BtcAdapterError::Fixture(
                "tip is not an anchored block".into(),
            ));
        }
        Ok(src)
    }

    /// Snapshot pinning this exact range: first and last height, tip hash, and
    /// the Keccak of the canonical source bytes (R-SRC-BIND-1).
    pub fn range_snapshot(&self) -> Result<Snapshot, BtcAdapterError> {
        let src = self.source()?;
        let bytes = borsh::to_vec(&src).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
        let (first, tip) = match (src.blocks.first(), src.blocks.last()) {
            (Some(f), Some(t)) => (f, t),
            _ => return Err(BtcAdapterError::Fixture("no blocks".into())),
        };
        Ok(Snapshot::BlockRange {
            chain: ChainId(CHAIN_ID.into()),
            from_height: first.height,
            to_height: tip.height,
            tip_hash: Hash32(tip.hash),
            content_hash: keccak256(&bytes),
        })
    }
}

impl SourceAdapter for BtcFixtureAdapter {
    type Error = BtcAdapterError;

    fn source_id(&self) -> SourceId {
        SourceId(SOURCE_ID.into())
    }

    fn rights(&self) -> DataRights {
        DataRights::public_chain(self.source_id())
    }

    /// Returns the canonical bytes only if range, tip and content all match the pin.
    fn load(&self, snapshot: &Snapshot) -> Result<Vec<u8>, Self::Error> {
        let Snapshot::BlockRange { chain, .. } = snapshot else {
            return Err(BtcAdapterError::WrongSnapshot);
        };
        if chain.0 != CHAIN_ID {
            return Err(BtcAdapterError::WrongSnapshot);
        }
        let mine = self.range_snapshot()?;
        if &mine != snapshot {
            return Err(BtcAdapterError::SnapshotMismatch(
                "range, tip or content hash differs from the pin".into(),
            ));
        }
        borsh::to_vec(&self.source()?).map_err(|e| BtcAdapterError::Fixture(e.to_string()))
    }
}

#[derive(Deserialize)]
struct LabelFile {
    name: String,
    version: u32,
    labels: Vec<LabelRow>,
}

#[derive(Deserialize)]
struct LabelRow {
    address: String,
    entity: String,
    kind: String,
    confidence_ppm: u32,
}

/// Proof level of a pinned label set (docs/spec/proof-ladder.md, L2R2-F3):
/// the weakest source in the set, from its reviewed provenance
/// (`fixtures/labels/PROVENANCE.md`). Both reviewed sets mix OKX-signed
/// proof-of-reserves addresses (level 3) with lists a company published
/// without a signature (Bitwise BITB; v2 also BitMEX and Binance: level 4), so
/// both are level 4. `set` must be exactly a reviewed set: same name, version
/// and content hash as the checksummed fixture. Anything else, or an
/// unreadable fixture, is level 5 (fail closed).
pub fn label_set_proof_level(fixtures_dir: &Path, set: &LabelSetRef) -> ProofLevel {
    const REVIEWED: [(&str, ProofLevel); 2] = [
        (
            "labels/test.onyx.entities.btc.v1.json",
            ProofLevel::BondedClaim,
        ),
        (
            "labels/test.onyx.entities.btc.v2.json",
            ProofLevel::BondedClaim,
        ),
    ];
    REVIEWED
        .iter()
        .find(|(rel, _)| {
            load_label_set(fixtures_dir, rel).is_ok_and(|l| {
                l.name == set.name
                    && l.version == set.version
                    && l.content_hash() == set.content_hash
            })
        })
        .map_or(ProofLevel::TrackRecord, |(_, level)| *level)
}

/// Proof level of a shard over blocks `from..=to` whose label set is `set`
/// (ADR-0037 item 3): level 4 (bonded claim; the weakest sources are lists
/// the issuers published without a signature) when `set` is exactly the
/// window `from..=to` of the checksummed label set v3 and holds at least one
/// label. Anything else, including the empty window, is level 5 (fail
/// closed; review P2a-R1-5: the heights are part of the check).
pub fn dated_window_proof_level(
    fixtures_dir: &Path,
    set: &LabelSetRef,
    from: u64,
    to: u64,
) -> ProofLevel {
    let ok = load_dated_label_set(fixtures_dir, rosters::LABELS_V3).is_ok_and(|v3| {
        v3.name == set.name
            && v3.version == set.version
            && v3
                .window(from, to)
                .is_ok_and(|w| !w.labels.is_empty() && w.content_hash() == set.content_hash)
    });
    if ok {
        ProofLevel::BondedClaim
    } else {
        ProofLevel::TrackRecord
    }
}

/// Entity kind of a label-fixture row. `Custodian` is accepted for the indexer
/// bridge (ADR-0031): no shipped set uses it yet (the custodial reserve lists
/// stay out until their redistribution terms are cleared, IDX-6).
pub fn label_kind(kind: &str) -> Option<EntityKind> {
    match kind {
        "Exchange" => Some(EntityKind::Exchange),
        "Etf" => Some(EntityKind::Etf),
        "Custodian" => Some(EntityKind::Custodian),
        _ => None,
    }
}

/// Load a checksummed label-set fixture (`labels/<name>.v<N>.json`).
pub fn load_label_set(fixtures_dir: &Path, rel: &str) -> Result<LabelSet, BtcAdapterError> {
    verify_checksum(fixtures_dir, rel)?;
    let text = std::fs::read_to_string(fixtures_dir.join(rel))
        .map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    let f: LabelFile =
        serde_json::from_str(&text).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    let labels = f
        .labels
        .into_iter()
        .map(|r| {
            let kind = label_kind(&r.kind)
                .ok_or_else(|| BtcAdapterError::Fixture(format!("kind {}", r.kind)))?;
            Ok(Label {
                address: r.address,
                entity: r.entity,
                kind,
                confidence: onyx_core::Confidence(r.confidence_ppm),
                evidence: vec![],
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(LabelSet {
        name: f.name,
        version: f.version,
        labels,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatedFile {
    name: String,
    version: u32,
    published: u64,
    /// Snapshot records the members cite (label set v3, ADR-0037 Amendment 1).
    #[serde(default)]
    snapshots: Vec<DatedSnapshotRow>,
    members: Vec<DatedRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatedSnapshotRow {
    source: String,
    entity: String,
    url: String,
    issued: String,
    fetched: String,
    height: u64,
    height_basis: String,
    raw_sha256: String,
    raw_keccak: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatedRow {
    address: String,
    entity: String,
    kind: String,
    confidence_ppm: u32,
    from: u64,
    to: Option<u64>,
    /// `signed` or `published`, with `evidence` (indexes into `snapshots`).
    #[serde(default)]
    grade: Option<String>,
    #[serde(default)]
    evidence: Vec<usize>,
}

fn snapshot_record(
    r: &DatedSnapshotRow,
) -> Result<onyx_labels::roster::SnapshotRecord, BtcAdapterError> {
    let h = |s: &str| -> Result<[u8; 32], BtcAdapterError> {
        let mut b = [0u8; 32];
        hex::decode_to_slice(s, &mut b).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
        Ok(b)
    };
    Ok(onyx_labels::roster::SnapshotRecord {
        source: r.source.clone(),
        entity: r.entity.clone(),
        url: r.url.clone(),
        issued: r.issued.clone(),
        fetched: r.fetched.clone(),
        height: r.height,
        height_basis: r.height_basis.clone(),
        raw_sha256: h(&r.raw_sha256)?,
        raw_keccak: Hash32(h(&r.raw_keccak)?),
    })
}

/// The snapshot records of a dated set file (empty for a file without a
/// table): where each member's evidence comes from (source URL, issuer date,
/// fetch date, height and its basis, raw hashes).
pub fn dated_snapshots_from_json(
    text: &str,
) -> Result<Vec<onyx_labels::roster::SnapshotRecord>, BtcAdapterError> {
    let f: DatedFile =
        serde_json::from_str(text).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    f.snapshots.iter().map(snapshot_record).collect()
}

/// A member's evidence from its grade and cited records: one
/// `roster.signed`/`roster.published` reference per record, sorted. The
/// record must be for the member's entity and the confidence must be the
/// grade's (ADR-0037 Amendment 1).
fn member_evidence(
    r: &DatedRow,
    records: &[onyx_labels::roster::SnapshotRecord],
    published: u64,
) -> Result<Vec<onyx_core::EvidenceRef>, BtcAdapterError> {
    use onyx_labels::roster as ro;
    let fail = |m: &str| BtcAdapterError::Fixture(format!("{}: {m}", r.address));
    let (kind, ppm) = match (r.grade.as_deref(), r.evidence.is_empty()) {
        // A file without a snapshot table (the synthetic ADR-0037 sets) has
        // no evidence; a file with one must cite it for every member.
        (None, true) if records.is_empty() => return Ok(vec![]),
        (Some("signed"), false) => (ro::EVIDENCE_SIGNED, ro::CONFIDENCE_SIGNED),
        (Some("published"), false) => (ro::EVIDENCE_PUBLISHED, ro::CONFIDENCE_PUBLISHED),
        _ => return Err(fail("grade and evidence go together (signed or published)")),
    };
    if r.confidence_ppm != ppm || r.evidence.len() > 2 {
        return Err(fail(
            "confidence or evidence count does not match the grade",
        ));
    }
    let mut ev = r
        .evidence
        .iter()
        .map(|i| {
            let rec = records.get(*i).ok_or_else(|| fail("evidence index"))?;
            if rec.entity != r.entity {
                return Err(fail("evidence is another entity's snapshot"));
            }
            // The first cited snapshot opens the range; none lies outside it.
            if rec.height < r.from || r.to.is_some_and(|t| rec.height > t) {
                return Err(fail("evidence outside the membership's range"));
            }
            Ok(onyx_core::EvidenceRef {
                kind: kind.to_string(),
                content_hash: rec.hash(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !r
        .evidence
        .iter()
        .any(|i| records.get(*i).is_some_and(|x| x.height == r.from))
    {
        return Err(fail("no cited snapshot opens the range"));
    }
    // Review P2a-R1-6: the last cited listing bounds the range (ADR-0037
    // Amendment 1 item 1): a closed range ends no later than its expiry, an
    // open one is still alive at the publication, nothing cited is past it.
    let last = r
        .evidence
        .iter()
        .filter_map(|i| records.get(*i).map(|x| x.height))
        .max()
        .unwrap_or(r.from);
    let expiry = last.saturating_add(ro::MAX_LISTING_AGE - 1);
    if last > published || r.to.map_or(expiry < published, |t| t > expiry) {
        return Err(fail(
            "range not supported by its cited listings (8,928-block rule)",
        ));
    }
    ev.sort();
    ev.dedup();
    Ok(ev)
}

/// Parse a dated label set (ADR-0037): `{name, version, published, members:
/// [{address, entity, kind, confidence_ppm, from, to}]}` (`to` null: open),
/// structure checked ([`onyx_labels::dated::DatedLabelSet::check_structure`]).
pub fn dated_label_set_from_json(
    text: &str,
) -> Result<onyx_labels::dated::DatedLabelSet, BtcAdapterError> {
    let f: DatedFile =
        serde_json::from_str(text).map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    let records = f
        .snapshots
        .iter()
        .map(snapshot_record)
        .collect::<Result<Vec<_>, _>>()?;
    // Review P2a-R1-6/7: the table is sorted by (entity, height, source) and
    // unique, every record well formed; a dated file never has the undated
    // marker height (it would read as a format-1 version in `check_history`).
    if f.published == u64::MAX {
        return Err(BtcAdapterError::Fixture(
            "a dated set cannot be published at u64::MAX".into(),
        ));
    }
    for r in &records {
        r.check_shape()
            .map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    }
    if records.windows(2).any(|w| {
        (&w[0].entity, w[0].height, &w[0].source) >= (&w[1].entity, w[1].height, &w[1].source)
    }) {
        return Err(BtcAdapterError::Fixture(
            "snapshot table not sorted and unique".into(),
        ));
    }
    let published = f.published;
    let members = f
        .members
        .into_iter()
        .map(|r| {
            let evidence = member_evidence(&r, &records, published)?;
            let kind = label_kind(&r.kind)
                .ok_or_else(|| BtcAdapterError::Fixture(format!("kind {}", r.kind)))?;
            // Review D1-R1 F6: one spelling per script, or the overlap rule
            // (never two entities at once) could be passed with another case.
            if !r
                .address
                .strip_prefix("btc:")
                .is_some_and(|b| BtcAddressCanon.is_canonical(b))
            {
                return Err(BtcAdapterError::Fixture(format!(
                    "address not canonical: {}",
                    r.address
                )));
            }
            Ok(onyx_labels::dated::Membership {
                label: Label {
                    address: r.address,
                    entity: r.entity,
                    kind,
                    confidence: onyx_core::Confidence(r.confidence_ppm),
                    evidence,
                },
                from: r.from,
                to: r.to,
            })
        })
        .collect::<Result<_, BtcAdapterError>>()?;
    let set = onyx_labels::dated::DatedLabelSet {
        name: f.name,
        version: f.version,
        published: f.published,
        members,
    };
    set.check_structure()
        .map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    Ok(set)
}

/// Load a checksummed dated label-set fixture ([`dated_label_set_from_json`]).
pub fn load_dated_label_set(
    fixtures_dir: &Path,
    rel: &str,
) -> Result<onyx_labels::dated::DatedLabelSet, BtcAdapterError> {
    verify_checksum(fixtures_dir, rel)?;
    let text = std::fs::read_to_string(fixtures_dir.join(rel))
        .map_err(|e| BtcAdapterError::Fixture(e.to_string()))?;
    dated_label_set_from_json(&text)
}

/// Registry used by the M1 fixtures and golden tests. Reviewed contents for
/// the test entities only; the production registry is governance data.
pub fn fixture_registry() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    let built = r
        .set_domain(DEVNET_DOMAIN)
        .and_then(|r| r.add_entity("entity:okx", EntityKind::Exchange))
        .and_then(|r| r.add_entity("entity:bitwise-bitb", EntityKind::Etf))
        .and_then(|r| r.add_source(DataRights::public_chain(SourceId(SOURCE_ID.into()))))
        .and_then(|r| r.add_canon(Box::new(BtcAddressCanon)))
        .and_then(|r| r.add_proof(Box::new(Bip322Proof)))
        .and_then(|r| r.add_aggregate(CHAIN_ID))
        .and_then(|r| {
            r.add_metric(
                METRIC_ENTITY_FLOW,
                MetricInfo {
                    value_kind: ValueKind::Bytes,
                    needs_label_set: true,
                },
            )
        })
        .map(|_| ());
    debug_assert!(built.is_ok(), "fixture registry is static and valid");
    r
}

/// Reviewed entities of label set v2 (`labels/test.onyx.entities.btc.v2.json`,
/// fixtures/labels/PROVENANCE.md): companies and funds only, each with
/// first-party evidence. No mining pools (LBL-V2-F1: payout addresses are not
/// proven pool-owned). A label naming anything else fails
/// [`LabelSet::validate`] against [`fixture_registry_v2`].
pub const LABELS_V2_ENTITIES: &[(&str, EntityKind)] = &[
    ("entity:binance", EntityKind::Exchange),
    ("entity:bitmex", EntityKind::Exchange),
    ("entity:okx", EntityKind::Exchange),
    ("entity:bitwise-bitb", EntityKind::Etf),
];

/// [`fixture_registry`] plus the entities of label set v2. A separate registry,
/// so the v1 registry root (pinned by the golden test, shard specs and the
/// settlement vectors) does not change.
pub fn fixture_registry_v2() -> StaticRegistry {
    let mut r = fixture_registry();
    for (id, kind) in LABELS_V2_ENTITIES {
        if r.entity_kind(id).is_none() {
            let added = r.add_entity(id, *kind).map(|_| ());
            debug_assert!(added.is_ok(), "v2 entity list is static and valid");
        }
    }
    r
}

/// Reviewed entities of label set v3 (`labels/test.onyx.entities.btc.v3.json`,
/// ROADMAP P2a): the issuers whose own dated rosters the set is built from
/// (fixtures/rosters, fixtures/labels/PROVENANCE.md v3). Companies and one
/// fund only; wBTC and cbBTC stay out (IDX-6).
pub const LABELS_V3_ENTITIES: &[(&str, EntityKind)] = &[
    ("entity:binance", EntityKind::Exchange),
    ("entity:bitfinex", EntityKind::Exchange),
    ("entity:bitmex", EntityKind::Exchange),
    ("entity:bitwise-bitb", EntityKind::Etf),
    ("entity:bybit", EntityKind::Exchange),
    ("entity:coinex", EntityKind::Exchange),
    ("entity:crypto-com", EntityKind::Exchange),
    ("entity:deribit", EntityKind::Exchange),
    ("entity:htx", EntityKind::Exchange),
    ("entity:mexc", EntityKind::Exchange),
    ("entity:okx", EntityKind::Exchange),
];

/// [`fixture_registry`] plus the entities of label set v3. A separate
/// registry, so the v1, v2, series, reserve and pool roots do not change.
pub fn fixture_registry_v3() -> StaticRegistry {
    let mut r = fixture_registry();
    for (id, kind) in LABELS_V3_ENTITIES {
        if r.entity_kind(id).is_none() {
            let added = r.add_entity(id, *kind).map(|_| ());
            debug_assert!(added.is_ok(), "v3 entity list is static and valid");
        }
    }
    r
}

/// [`fixture_registry_v3`] plus the reserve metric: shards over windows of
/// label set v3 (ADR-0037 item 3).
pub fn fixture_registry_reserve_v3() -> StaticRegistry {
    let mut r = fixture_registry_v3();
    let added = r
        .add_metric(
            METRIC_ENTITY_RESERVE,
            MetricInfo {
                value_kind: ValueKind::Bytes,
                needs_label_set: true,
            },
        )
        .map(|_| ());
    debug_assert!(added.is_ok(), "reserve metric is static and valid");
    r
}

/// Recipe failures that are valid results for this adapter's schemas, as
/// (schema hash, recipe error code) pairs (SER-R2-F2): a series range with no
/// labelled activity ends with `SERIES_NO_ACTIVITY` and settles as the
/// canonical failure leaf. Every other failure is refused at round open and
/// never committed by a miner. Feed it to
/// `onyx_coordinator::recompute::FailureAllowList` (the coordinator stays
/// sector-agnostic).
pub fn valid_failure_codes() -> Vec<(Hash32, u32)> {
    vec![(
        keccak256(SCHEMA_ENTITY_FLOW_SERIES),
        onyx_btc_schema::SERIES_NO_ACTIVITY,
    )]
}

/// [`fixture_registry_v2`] plus the per-block series metric
/// ([`METRIC_ENTITY_FLOW_SERIES`]). A separate registry, so the v1 and v2
/// roots (pinned by goldens, shard specs and the website) do not change.
pub fn fixture_registry_series() -> StaticRegistry {
    let mut r = fixture_registry_v2();
    let added = r
        .add_metric(
            METRIC_ENTITY_FLOW_SERIES,
            MetricInfo {
                value_kind: ValueKind::Bytes,
                needs_label_set: true,
            },
        )
        .map(|_| ());
    debug_assert!(added.is_ok(), "series metric is static and valid");
    r
}

/// [`fixture_registry_v2`] plus the reserve metric ([`METRIC_ENTITY_RESERVE`],
/// ADR-0034). A separate registry, so the v1, v2, series and pool roots do
/// not change.
pub fn fixture_registry_reserve() -> StaticRegistry {
    let mut r = fixture_registry_v2();
    let added = r
        .add_metric(
            METRIC_ENTITY_RESERVE,
            MetricInfo {
                value_kind: ValueKind::Bytes,
                needs_label_set: true,
            },
        )
        .map(|_| ());
    debug_assert!(added.is_ok(), "reserve metric is static and valid");
    r
}

/// [`fixture_registry`] plus the mining-pool block metric
/// ([`METRIC_POOL_BLOCKS`], no label set). A separate registry, so the v1, v2
/// and series roots do not change.
pub fn fixture_registry_pools() -> StaticRegistry {
    let mut r = fixture_registry();
    let added = r
        .add_metric(
            METRIC_POOL_BLOCKS,
            MetricInfo {
                value_kind: ValueKind::Bytes,
                needs_label_set: false,
            },
        )
        .map(|_| ());
    debug_assert!(added.is_ok(), "pool metric is static and valid");
    r
}

/// [`fixture_registry`] plus the chain statistics metric
/// ([`METRIC_CHAIN_STATS`], ADR-0043, no label set). A separate registry, so
/// the v1, v2, series, reserve and pool roots do not change.
pub fn fixture_registry_chain() -> StaticRegistry {
    let mut r = fixture_registry();
    let added = r
        .add_metric(
            METRIC_CHAIN_STATS,
            MetricInfo {
                value_kind: ValueKind::Bytes,
                needs_label_set: false,
            },
        )
        .map(|_| ());
    debug_assert!(added.is_ok(), "chain metric is static and valid");
    r
}

/// The Bitcoin address rules a settling binary trusts (ADR-0019 loop 26,
/// L25-F2): settlement refuses a registry whose BTC canon or proof is any
/// other type, even one with the same id and the same vector results.
#[derive(Debug)]
pub struct BtcRules;

impl onyx_core::RuleSet for BtcRules {
    fn proof_type(proof_id: &str) -> Option<std::any::TypeId> {
        use onyx_core::AddressProof;
        (proof_id == Bip322Proof.proof_id()).then(std::any::TypeId::of::<Bip322Proof>)
    }

    fn canon_type(canon_id: &str) -> Option<std::any::TypeId> {
        (canon_id == BtcAddressCanon.canon_id()).then(std::any::TypeId::of::<BtcAddressCanon>)
    }
}

/// The same rules as constructors (ROADMAP C23): a client rebuilds a served
/// registry ([`onyx_core::RegistryData::build_checked`]) with these and no
/// other, and uses it only when the rebuilt root is the one it expects.
impl onyx_core::RuleBuilder for BtcRules {
    fn canon(&self, canon_id: &str) -> Option<Box<dyn onyx_core::AddressCanon + Send + Sync>> {
        (canon_id == BtcAddressCanon.canon_id())
            .then(|| Box::new(BtcAddressCanon) as Box<dyn onyx_core::AddressCanon + Send + Sync>)
    }

    fn proof(&self, proof_id: &str) -> Option<Box<dyn onyx_core::AddressProof + Send + Sync>> {
        use onyx_core::AddressProof;
        (proof_id == Bip322Proof.proof_id())
            .then(|| Box::new(Bip322Proof) as Box<dyn onyx_core::AddressProof + Send + Sync>)
    }
}

/// Keccak of the fixture registry root, for logs.
pub fn fixture_registry_root() -> Hash32 {
    fixture_registry().root()
}

/// True if the number of leading '1's equals the number of leading zero bytes
/// of the 25-byte decoded payload, i.e. this is the only spelling (R-B58-1).
fn base58_leading_ones_match(body: &str) -> bool {
    const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut n = [0u8; 25];
    for c in body.bytes() {
        let Some(d) = B58.iter().position(|x| *x == c) else {
            return false;
        };
        let mut carry = d as u32;
        for byte in n.iter_mut().rev() {
            carry += u32::from(*byte) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        if carry != 0 {
            return false;
        }
    }
    let ones = body.bytes().take_while(|c| *c == b'1').count();
    n.iter().take_while(|b| **b == 0).count() == ones
}

/// Bech32 data alphabet (BIP-173).
const BECH32_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// Canonical Bitcoin **mainnet** address spelling for label sets (namespace `btc`).
///
/// - Bech32/Bech32m (`bc1`): case-insensitive by spec, so only all-lowercase is
///   canonical; data part must use the bech32 alphabet.
/// - Base58 (`1` P2PKH, `3` P2SH): case-sensitive, kept as is.
/// - Testnet and regtest spellings (`tb1`, `bcrt1`, `m`, `n`, `2`) are rejected:
///   they can encode the same script as a mainnet address, which would let one
///   script carry two labels (R-LBL-CANON-2).
///
/// Checksums and bech32-vs-bech32m by witness version are verified when labels
/// are imported (M2); this check guarantees one spelling per address.
#[derive(Debug, Default)]
pub struct BtcAddressCanon;

impl AddressCanon for BtcAddressCanon {
    fn namespace(&self) -> &str {
        "btc"
    }

    fn canon_id(&self) -> &str {
        "onyx.canon.btc-mainnet.v1"
    }

    fn vectors(&self) -> &'static [(&'static str, bool)] {
        &[
            ("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", true),
            ("BC1QAR0SRRR7XFKVY5L643LYDNW9RE59GTZZWF5MDQ", false),
            ("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", true),
            ("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy", true),
            ("tb1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", false),
            ("mipcBbFg9gMiCh81Kj8tqqdgoZub1ZJRfn", false),
            ("bc1qb", false),
            ("112D2adLM3UKy4Z4giRbReR6gjWuvHUqB", true),
            ("12D2adLM3UKy4Z4giRbReR6gjWuvHUqB", false),
        ]
    }

    fn is_canonical(&self, body: &str) -> bool {
        if let Some(data) = body.strip_prefix("bc1") {
            return !data.is_empty() && data.bytes().all(|c| BECH32_CHARSET.contains(&c));
        }
        matches!(body.as_bytes().first(), Some(b'1' | b'3'))
            && body
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() && !b"0OIl".contains(&c))
            && base58_leading_ones_match(body)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn label_kinds_include_custodian_and_refuse_unknown_kinds() {
        use super::label_kind;
        use onyx_core::EntityKind;
        assert_eq!(label_kind("Exchange"), Some(EntityKind::Exchange));
        assert_eq!(label_kind("Etf"), Some(EntityKind::Etf));
        assert_eq!(label_kind("Custodian"), Some(EntityKind::Custodian));
        // Natural persons are never a label kind (hard limit); unknown kinds fail closed.
        assert_eq!(label_kind("Person"), None);
        assert_eq!(label_kind("custodian"), None);
    }

    use super::*;

    #[test]
    fn bech32_must_be_lowercase() {
        let c = BtcAddressCanon;
        assert!(c.is_canonical("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq"));
        assert!(!c.is_canonical("BC1QAR0SRRR7XFKVY5L643LYDNW9RE59GTZZWF5MDQ"));
        assert!(!c.is_canonical("bc1Qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq"));
    }

    #[test]
    fn base58_case_preserved_and_alphabet_checked() {
        let c = BtcAddressCanon;
        assert!(c.is_canonical("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
        assert!(c.is_canonical("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy"));
        assert!(!c.is_canonical("1A1zP1eP5QGefi2DMPTfTL5SLmv7Div0Na")); // '0' not base58
        assert!(!c.is_canonical("xpub123"));
    }

    fn fixtures() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"))
    }

    const BLOCKS: &str = "btc/blocks-921080-921082.json";
    const LABELS: &str = "labels/test.onyx.entities.btc.v1.json";

    #[test]
    fn fixture_loads_and_matches_tip_snapshot() {
        let a = BtcFixtureAdapter::new(fixtures(), BLOCKS);
        let src = a.source().expect("source");
        assert_eq!(src.blocks.len(), 3);
        assert_eq!(src.blocks[0].height, 921_080);
        let txs: usize = src.blocks.iter().map(|b| b.txs.len()).sum();
        assert_eq!(txs, 3868 + 2404 + 5343);
        let snap = a.range_snapshot().expect("snap");
        let bytes = a.load(&snap).expect("load");
        // Deterministic: same snapshot, same bytes; and the pin covers the content.
        assert_eq!(bytes, a.load(&snap).expect("load again"));
        let Snapshot::BlockRange {
            chain,
            to_height,
            tip_hash,
            content_hash,
            ..
        } = snap.clone()
        else {
            panic!("range snapshot expected");
        };
        assert_eq!(content_hash, keccak256(&bytes));
        // R-SRC-BIND-1: same tip, different first height is a different pin.
        let shorter = Snapshot::BlockRange {
            chain: chain.clone(),
            from_height: 921_081,
            to_height,
            tip_hash,
            content_hash,
        };
        assert!(matches!(
            a.load(&shorter),
            Err(BtcAdapterError::SnapshotMismatch(_))
        ));
        let edited = Snapshot::BlockRange {
            chain,
            from_height: 921_080,
            to_height,
            tip_hash,
            content_hash: keccak256(b"edited content"),
        };
        assert!(matches!(
            a.load(&edited),
            Err(BtcAdapterError::SnapshotMismatch(_))
        ));
    }

    /// ROADMAP C23: every fixture registry (with the BTC canon and the BIP-322
    /// rule) rebuilds from its data with [`BtcRules`] to the same root; without
    /// them it is refused, never rebuilt with something else.
    #[test]
    fn fixture_registries_rebuild_from_their_data_with_the_btc_rules() {
        for r in [
            fixture_registry(),
            fixture_registry_v2(),
            fixture_registry_series(),
        ] {
            let d = r.data();
            let back = d.build_checked(&BtcRules, r.root()).expect("rebuilds");
            assert_eq!(back.root(), r.root());
            assert!(!d.names_addresses());
            assert!(matches!(
                d.build_checked(&onyx_core::NoRules, r.root()),
                Err(onyx_core::RegistryDataError::UnknownRule(_))
            ));
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn label_fixture_valid_against_registry() {
        let set = load_label_set(&fixtures(), LABELS).expect("labels");
        assert_eq!(set.labels.len(), 120);
        assert_eq!(set.validate(&fixture_registry()), Ok(()));
    }

    #[test]
    fn tampered_fixture_rejected() {
        let dir = std::env::temp_dir().join(format!("onyx-fixture-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("btc")).expect("mkdir");
        std::fs::copy(fixtures().join("SHA256SUMS"), dir.join("SHA256SUMS")).expect("sums");
        let mut bytes = std::fs::read(fixtures().join(BLOCKS)).expect("read");
        let last = bytes.len() - 2;
        bytes[last] ^= 1;
        std::fs::write(dir.join(BLOCKS), bytes).expect("write");
        let a = BtcFixtureAdapter::new(&dir, BLOCKS);
        assert!(matches!(a.source(), Err(BtcAdapterError::Checksum(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// R-FIX-2: offline header checks catch a forged or unlinked block.
    #[test]
    fn header_checks() {
        let src_text = std::fs::read_to_string(fixtures().join(BLOCKS)).expect("read fixture");
        let doc: FixtureDoc = serde_json::from_str(&src_text).expect("parse");
        let b0 = &doc.blocks[0];
        let b1 = &doc.blocks[1];
        let h0 = hex32(&b0.hash).expect("h0");
        let h1 = hex32(&b1.hash).expect("h1");
        let hdr0 = hex::decode(&b0.header).expect("hdr0");
        let hdr1 = hex::decode(&b1.header).expect("hdr1");
        assert!(check_header(&hdr0, &h0, None).is_ok());
        assert!(check_header(&hdr1, &h1, Some(&h0)).is_ok());
        // Wrong previous block.
        assert!(check_header(&hdr1, &h1, Some(&h1)).is_err());
        // Tampered header (nonce changed): hash no longer matches.
        let mut bad = hdr0.clone();
        bad[79] ^= 1;
        assert!(check_header(&bad, &h0, None).is_err());
        // Header that claims an impossible target: easy bits but same hash fails PoW.
        let mut easy = hdr0.clone();
        easy[72..76].copy_from_slice(&0x0300_0001u32.to_le_bytes());
        let mut eh = sha256d(&easy);
        eh.reverse();
        assert!(check_header(&easy, &eh, None).is_err());
        // R-FIX-3: bits easier than powLimit are rejected even when the hash meets them.
        let mut forged = hdr0;
        forged[72..76].copy_from_slice(&0x22ff_ffffu32.to_le_bytes());
        let mut fh = sha256d(&forged);
        fh.reverse();
        assert!(check_header(&forged, &fh, None).is_err());
        // Txids hash to the real merkle root.
        let txids: Vec<[u8; 32]> = b0
            .txs
            .iter()
            .map(|t| hex32(&t.txid).expect("txid"))
            .collect();
        let hdr0 = hex::decode(&b0.header).expect("hdr0");
        assert_eq!(merkle_root(&txids)[..], hdr0[36..68]);
        let mut dup = txids.clone();
        dup.swap(1, 2);
        assert_ne!(merkle_root(&dup)[..], hdr0[36..68]);
    }

    /// R-LBL-CANON-2: testnet/regtest spellings of a script are not accepted under `btc`.
    #[test]
    fn testnet_and_regtest_rejected() {
        let c = BtcAddressCanon;
        for t in [
            "tb1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
            "bcrt1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
            "mipcBbFg9gMiCh81Kj8tqqdgoZub1ZJRfn",
            "n3GNqMveyvaPvUbH469vDRadqpJMPc84JA",
            "2MzQwSSnBHWHqSAqtTVQ6v47XtaisrJa1Vc",
            "bc1qb", // 'b' is not in the bech32 alphabet
        ] {
            assert!(!c.is_canonical(t), "{t}");
        }
    }
}
