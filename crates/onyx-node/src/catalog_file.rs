//! The catalog file: the shards the service serves, its markets and prices.
//!
//! Each shard is given by its Borsh `ShardSpec` and the source's Borsh
//! `DataRights`; the service derives the shard id, `asOf`, the binding and
//! the test-label flag from the spec itself (`ShardMeta::from_spec`), and
//! refuses rights whose hash is not the spec's `rights_hash`. So the file
//! cannot make the server claim a binding the spec does not hash.
//!
//! With the chain indexer on (ADR-0033) the file lists no served shards:
//! `held` names the shards the operator holds specs (and rows) for, without
//! a round, and the indexer serves one only after the settlement contract
//! finalized it and the held rows hash to its root.

use borsh::{BorshDeserialize, BorshSerialize};
use onyx_api::catalog::{Catalog, DatasetPricing, Market, PricingPolicy, ShardMeta};
use onyx_api::indexer::{HeldShard, Holdings};
use onyx_coordinator::cas::Cas;
use onyx_core::{DataRights, Hash32, ShardSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version this code reads.
pub const CATALOG_VERSION: u64 = 1;

/// One shard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShardEntry {
    /// Round.
    pub round: u64,
    /// Metric name (e.g. `btc.entity-flow`).
    pub metric: String,
    /// `0x` + hex of the Borsh `ShardSpec`.
    pub spec: String,
    /// `0x` + hex of the Borsh `DataRights` of the spec's source.
    pub rights: String,
    /// Row count of the finalized commitment (lets the server price an export
    /// before it reads any row; a wrong value fails the binding check).
    pub leaf_count: Option<u64>,
    /// Unix time of the snapshot's end. Never trusted from this file
    /// (AUD-2R-F2): for a block snapshot the time is read from `endHeader`,
    /// and this value, when present, must equal it; a document snapshot uses
    /// its bound `published_unix`. The price clock starts no later than the
    /// data time plus a day.
    #[serde(default)]
    pub snapshot_unix: Option<u64>,
    /// `0x` + hex of the 80-byte header of the snapshot's end block
    /// (AUD-2R-F2): required for a block snapshot. Its double SHA-256 must be
    /// the tip hash the shard spec pins, so its time field is the data's end
    /// time with no operator say in it.
    #[serde(default)]
    pub end_header: Option<String>,
    /// Level (1-5) of an input the recipe rerun does not check, e.g. a pinned
    /// third-party mapping table (4 for a mining-pool shard, AUD-2-F6). It can
    /// only weaken the shard's level, never strengthen it.
    #[serde(default)]
    pub input_level: Option<u8>,
    /// The serving check the shard needs before any row is served (ADR-0034
    /// Amendment 1), e.g. `btc-entity-reserve-v1`: a node without that gate
    /// refuses the shard (fail closed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving_check: Option<String>,
}

/// A shard the operator holds, for the chain indexer (ADR-0033): a
/// [`ShardEntry`] without a round. The round comes from the `Finalized`
/// event (a shard id finalizes at most once).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeldEntry {
    /// Metric name.
    pub metric: String,
    /// `0x` + hex of the Borsh `ShardSpec`.
    pub spec: String,
    /// `0x` + hex of the Borsh `DataRights` of the spec's source.
    pub rights: String,
    /// Row count (checked against the held rows and the chain's binding).
    pub leaf_count: Option<u64>,
    /// As [`ShardEntry::snapshot_unix`].
    #[serde(default)]
    pub snapshot_unix: Option<u64>,
    /// As [`ShardEntry::end_header`].
    #[serde(default)]
    pub end_header: Option<String>,
    /// As [`ShardEntry::input_level`].
    #[serde(default)]
    pub input_level: Option<u8>,
    /// As [`ShardEntry::serving_check`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving_check: Option<String>,
}

impl HeldEntry {
    /// The entry for `spec` (what a deploy driver writes).
    pub fn new(
        metric: &str,
        spec: &ShardSpec,
        rights: &DataRights,
        leaf_count: Option<u64>,
        end_header: Option<&[u8]>,
    ) -> Result<Self, String> {
        let e = ShardEntry::new(0, metric, spec, rights, leaf_count, end_header)?;
        Ok(HeldEntry {
            metric: e.metric,
            spec: e.spec,
            rights: e.rights,
            leaf_count: e.leaf_count,
            snapshot_unix: e.snapshot_unix,
            end_header: e.end_header,
            input_level: e.input_level,
            serving_check: e.serving_check,
        })
    }

    fn as_entry(&self) -> ShardEntry {
        ShardEntry {
            round: 0,
            metric: self.metric.clone(),
            spec: self.spec.clone(),
            rights: self.rights.clone(),
            leaf_count: self.leaf_count,
            snapshot_unix: self.snapshot_unix,
            end_header: self.end_header.clone(),
            input_level: self.input_level,
            serving_check: self.serving_check.clone(),
        }
    }
}

/// A market bound to one row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketEntry {
    /// Market id (URL path segment).
    pub id: String,
    /// Round.
    pub round: u64,
    /// Shard id (`0x` + 64 hex).
    pub shard: String,
    /// Row key, e.g. `entity:okx`.
    pub subject: String,
}

/// Per-row price policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pricing {
    /// Price per row while fresh, micro-USDC.
    pub base_price_micro_usdc: u64,
    /// Seconds a row stays paid after its root is final.
    pub fresh_window_secs: u64,
}

/// Dataset (export) price policy (ADR-0025 item 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dataset {
    /// Flat price of a fresh dataset, micro-USDC.
    pub flat_micro_usdc: u64,
    /// The flat price is at least this many rows.
    pub floor_rows: u64,
}

/// The file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogFile {
    /// [`CATALOG_VERSION`].
    pub version: u64,
    /// Shards served as listed (indexer off).
    pub shards: Vec<ShardEntry>,
    /// Shards held for the chain indexer (indexer on, ADR-0033).
    #[serde(default)]
    pub held: Vec<HeldEntry>,
    /// Markets.
    #[serde(default)]
    pub markets: Vec<MarketEntry>,
    /// Default per-row pricing (`None`: free unless a metric has its own).
    #[serde(default)]
    pub default_pricing: Option<Pricing>,
    /// Per-metric per-row pricing.
    #[serde(default)]
    pub pricing: BTreeMap<String, Pricing>,
    /// Default dataset pricing (`None`: an export costs its rows).
    #[serde(default)]
    pub default_dataset_pricing: Option<Dataset>,
    /// Per-metric dataset pricing.
    #[serde(default)]
    pub dataset_pricing: BTreeMap<String, Dataset>,
}

fn unhex(s: &str, what: &str) -> Result<Vec<u8>, String> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s)).map_err(|_| format!("{what} is not hex"))
}

pub(crate) fn hash32(s: &str, what: &str) -> Result<Hash32, String> {
    let b = unhex(s, what)?;
    let a: [u8; 32] = b
        .try_into()
        .map_err(|_| format!("{what} must be 32 bytes"))?;
    Ok(Hash32(a))
}

/// Data times may run ahead of the checker's clock by at most this much
/// (AUD-2R-F2): Bitcoin accepts block times up to two hours in the future.
pub const MAX_FUTURE_SECS: u64 = 7_200;

/// The time field of an 80-byte Bitcoin-format block header.
fn header_time(h: &[u8]) -> Option<u64> {
    (h.len() == 80).then(|| u64::from(u32::from_le_bytes([h[68], h[69], h[70], h[71]])))
}

/// The end time of a block snapshot, read from its end header (AUD-2R-F2).
/// The node knows one header format: Bitcoin's (chains `btc.*`); a block
/// snapshot of any other chain is refused rather than priced from an
/// operator's number. The header's double SHA-256, in display order, must be
/// the tip hash the snapshot pins.
fn block_end_time(chain: &str, tip: &Hash32, header_hex: Option<&str>) -> Result<u64, String> {
    use sha2::{Digest, Sha256};
    if !chain.starts_with("btc.") {
        return Err(format!(
            "no header format for chain {chain:?}: a block snapshot's end time must come from its header"
        ));
    }
    let h = unhex(
        header_hex
            .ok_or("endHeader (the end block's 80-byte header) is required for a block snapshot")?,
        "endHeader",
    )?;
    let t = header_time(&h).ok_or("endHeader must be 80 bytes")?;
    let mut d: [u8; 32] = Sha256::digest(Sha256::digest(&h)).into();
    d.reverse();
    if d != tip.0 {
        return Err("endHeader does not hash to the snapshot's tip hash".into());
    }
    Ok(t)
}

impl ShardEntry {
    /// The entry for `spec` in `round` (what a deploy driver writes).
    /// `end_header` is the 80-byte header of the snapshot's end block, for a
    /// block snapshot (AUD-2R-F2).
    pub fn new(
        round: u64,
        metric: &str,
        spec: &ShardSpec,
        rights: &DataRights,
        leaf_count: Option<u64>,
        end_header: Option<&[u8]>,
    ) -> Result<Self, String> {
        let enc = |v: &dyn Fn(&mut Vec<u8>) -> std::io::Result<()>| -> Result<String, String> {
            let mut b = Vec::new();
            v(&mut b).map_err(|e| e.to_string())?;
            Ok(format!("0x{}", hex::encode(b)))
        };
        Ok(ShardEntry {
            round,
            metric: metric.into(),
            spec: enc(&|b| spec.serialize(b))?,
            rights: enc(&|b| rights.serialize(b))?,
            leaf_count,
            snapshot_unix: end_header.and_then(header_time),
            end_header: end_header.map(|h| format!("0x{}", hex::encode(h))),
            input_level: None,
            serving_check: None,
        })
    }
}

/// The metadata of one entry at clock `now` (AUD-2R-F2 data-time rules),
/// and its spec.
fn entry_meta(s: &ShardEntry, now: u64) -> Result<(ShardMeta, ShardSpec), String> {
    let spec = ShardSpec::try_from_slice(&unhex(&s.spec, "spec")?)
        .map_err(|_| "spec is not a Borsh ShardSpec".to_string())?;
    let rights = DataRights::try_from_slice(&unhex(&s.rights, "rights")?)
        .map_err(|_| "rights are not Borsh DataRights".to_string())?;
    let mut meta = ShardMeta::from_spec(s.round, &spec, s.metric.clone(), rights)
        .ok_or("rights do not match the spec's rights_hash")?;
    meta.leaf_count = s.leaf_count;
    if let Some(n) = s.input_level {
        let level = onyx_core::ProofLevel::from_number(n).ok_or("inputLevel must be 1..=5")?;
        meta = meta.with_input_level(level);
    }
    if let Some(g) = &s.serving_check {
        meta = meta.with_serving_check(g.clone());
    }
    let tip = match &spec.source.snapshot {
        onyx_core::Snapshot::Block {
            chain, block_hash, ..
        } => Some((chain, block_hash)),
        onyx_core::Snapshot::BlockRange {
            chain, tip_hash, ..
        } => Some((chain, tip_hash)),
        onyx_core::Snapshot::Document { .. } => None,
    };
    let data_time = match tip {
        Some((chain, hash)) => {
            let t = block_end_time(&chain.0, hash, s.end_header.as_deref())?;
            if s.snapshot_unix.is_some_and(|x| x != t) {
                return Err(format!(
                    "snapshotUnix disagrees with the end header's time {t}"
                ));
            }
            meta = meta.with_snapshot_unix(t);
            t
        }
        None => meta.snapshot_unix.unwrap_or(0),
    };
    if data_time > now.saturating_add(MAX_FUTURE_SECS) {
        return Err(format!(
            "data time {data_time} is more than {MAX_FUTURE_SECS} s after now ({now})"
        ));
    }
    Ok((meta, spec))
}

impl CatalogFile {
    /// The shards held for the chain indexer at clock `now`, checked as
    /// listed shards are ([`Self::to_catalog_at`]); a shard id held twice
    /// is refused.
    pub fn holdings_at(&self, now: u64) -> Result<Holdings, String> {
        let mut held = Holdings::new();
        for (i, h) in self.held.iter().enumerate() {
            let (meta, spec) =
                entry_meta(&h.as_entry(), now).map_err(|e| format!("held {i}: {e}"))?;
            held.insert(HeldShard {
                meta,
                label_set: spec.label_set,
            })
            .map_err(|e| format!("held {i}: {e}"))?;
        }
        Ok(held)
    }

    /// R-LBL-FIX-1 (L9 sweep): every listed shard's pinned label set passes
    /// the indexer's label-set check against the row store
    /// (`onyx_api::indexer::check_label_set`): held under its pinned name,
    /// version and content hash, every label citing evidence the store
    /// resolves. Before this, only `test.*` sets were refused on the listed
    /// path (`ONYX_INDEXER=off`, the default), so a placeholder or unevidenced
    /// set was served. `test.*` sets pass here: the server refuses them per
    /// request outside dev mode (403 `test_labels`).
    pub fn check_listed_label_sets(&self, cas: &dyn Cas) -> Result<(), String> {
        for (i, e) in self.shards.iter().enumerate() {
            let spec = ShardSpec::try_from_slice(&unhex(&e.spec, "spec")?)
                .map_err(|_| format!("shard {i}: spec is not a Borsh ShardSpec"))?;
            if let Some(l) = &spec.label_set {
                onyx_api::indexer::check_label_set(Some(l), cas, true).map_err(|r| {
                    format!(
                        "shard {i} (round {}): label set {} v{} is not servable: {} {r:?}",
                        e.round,
                        l.name,
                        l.version,
                        r.code()
                    )
                })?;
            }
        }
        Ok(())
    }

    /// One line per listed and held entry, in file order, with the shard id
    /// its spec derives (`onyx-node --check-config` prints them): an operator
    /// finds the entry to remove when a shard must stop being served (L15
    /// runbook docs/ops/runbooks/stop-serving-shard.md).
    pub fn entry_ids(&self) -> Result<Vec<String>, String> {
        let id = |spec: &str| -> Result<String, String> {
            ShardSpec::try_from_slice(&unhex(spec, "spec")?)
                .map(|s| format!("0x{}", hex::encode(s.shard_id().0)))
                .map_err(|_| "spec is not a Borsh ShardSpec".to_string())
        };
        let mut out = Vec::with_capacity(self.shards.len() + self.held.len());
        for (i, e) in self.shards.iter().enumerate() {
            out.push(format!(
                "shards[{i}] round {} metric {} shard {}",
                e.round,
                e.metric,
                id(&e.spec)?
            ));
        }
        for (i, h) in self.held.iter().enumerate() {
            out.push(format!(
                "held[{i}] metric {} shard {}",
                h.metric,
                id(&h.spec)?
            ));
        }
        Ok(out)
    }

    /// The params of every listed and held spec: a serving check reads a
    /// shard's params from the row store by the params hash its id commits
    /// to, so the node stores them there at start (ADR-0034 Amendment 1).
    pub fn params_blobs(&self) -> Result<Vec<Vec<u8>>, String> {
        self.shards
            .iter()
            .map(|e| e.spec.as_str())
            .chain(self.held.iter().map(|h| h.spec.as_str()))
            .map(|spec| {
                ShardSpec::try_from_slice(&unhex(spec, "spec")?)
                    .map(|s| s.params)
                    .map_err(|_| "spec is not a Borsh ShardSpec".to_string())
            })
            .collect()
    }

    /// Parse JSON text.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let f: CatalogFile =
            serde_json::from_str(text).map_err(|e| format!("catalog file: {e}"))?;
        if f.version != CATALOG_VERSION {
            return Err(format!(
                "catalog file version {} (this build reads {CATALOG_VERSION})",
                f.version
            ));
        }
        Ok(f)
    }

    /// Build the API catalog at the node's clock: [`Self::to_catalog_at`].
    pub fn to_catalog(&self) -> Result<Catalog, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.to_catalog_at(now)
    }

    /// Build the API catalog at clock `now`. Refuses a spec that does not
    /// decode exactly, rights that are not the spec's, a duplicate shard, a
    /// market on a shard the file neither lists nor holds, and a data time the file
    /// could have chosen (AUD-2R-F2): a block snapshot's time comes from its
    /// end header (which must hash to the pinned tip), a stated
    /// `snapshotUnix` must equal it, and no data time may be more than
    /// [`MAX_FUTURE_SECS`] after `now`.
    pub fn to_catalog_at(&self, now: u64) -> Result<Catalog, String> {
        let mut c = Catalog::new();
        let mut seen = std::collections::BTreeSet::new();
        for (i, s) in self.shards.iter().enumerate() {
            let (meta, _) = entry_meta(s, now).map_err(|e| format!("shard {i}: {e}"))?;
            if !seen.insert((s.round, meta.shard)) {
                return Err(format!("shard {i}: round {} shard listed twice", s.round));
            }
            c.add_shard(meta);
        }
        let held = self.holdings_at(now)?;
        for m in &self.markets {
            let shard = hash32(&m.shard, "market shard")?;
            if !seen.contains(&(m.round, shard)) && held.get(&shard).is_none() {
                return Err(format!(
                    "market {:?}: its shard is not in the catalog",
                    m.id
                ));
            }
            if m.id.is_empty() || m.subject.is_empty() {
                return Err("markets need an id and a subject".into());
            }
            c.add_market(
                m.id.clone(),
                Market {
                    round: m.round,
                    shard,
                    subject: m.subject.clone(),
                },
            );
        }
        let p = |x: &Pricing| PricingPolicy {
            base_price_micro_usdc: x.base_price_micro_usdc,
            fresh_window_secs: x.fresh_window_secs,
        };
        let d = |x: &Dataset| DatasetPricing {
            flat_micro_usdc: x.flat_micro_usdc,
            floor_rows: x.floor_rows,
        };
        c.default_pricing = self.default_pricing.as_ref().map(p);
        c.default_dataset_pricing = self.default_dataset_pricing.as_ref().map(d);
        for (metric, x) in &self.pricing {
            c.set_pricing(metric.clone(), p(x));
        }
        for (metric, x) in &self.dataset_pricing {
            c.set_dataset_pricing(metric.clone(), d(x));
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_files_are_refused() {
        assert!(CatalogFile::from_json("{}").is_err());
        assert!(CatalogFile::from_json(r#"{"version":2,"shards":[]}"#).is_err());
        assert!(CatalogFile::from_json(r#"{"version":1,"shards":[],"surprise":1}"#).is_err());
        let f = CatalogFile::from_json(r#"{"version":1,"shards":[{"round":1,"metric":"m","spec":"0x00","rights":"0x00","leafCount":null}]}"#)
            .expect("parses");
        assert!(f.to_catalog().is_err(), "a spec that does not decode");
        let mut f = CatalogFile::from_json(r#"{"version":1,"shards":[]}"#).expect("parses");
        f.markets.push(MarketEntry {
            id: "m".into(),
            round: 1,
            shard: format!("0x{}", "00".repeat(32)),
            subject: "entity:x".into(),
        });
        assert!(f.to_catalog().is_err(), "a market on an unknown shard");
    }

    /// The committed fixture's end block: its header, hash and time.
    fn fixture_end() -> (Vec<u8>, onyx_core::Hash32, u64) {
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/btc/blocks-921080-921082.json"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).expect("fixture")).expect("json");
        let b = v["blocks"]
            .as_array()
            .and_then(|a| a.last())
            .expect("end block");
        let h = hex::decode(b["header"].as_str().expect("header")).expect("hex");
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hex::decode(b["hash"].as_str().expect("hash")).expect("hash hex"));
        let t = header_time(&h).expect("80 bytes");
        (h, onyx_core::Hash32(hash), t)
    }

    fn block_spec(chain: &str, tip: onyx_core::Hash32) -> (ShardSpec, DataRights) {
        use onyx_core::{keccak256, ChainId, Snapshot, SourceId, SourceRef};
        let rights = DataRights::public_chain(SourceId("chain.test".into()));
        let spec = ShardSpec {
            recipe: None,
            runtime: None,
            label_set: None,
            source: SourceRef {
                source: SourceId("chain.test".into()),
                snapshot: Snapshot::Block {
                    chain: ChainId(chain.into()),
                    height: 921_082,
                    block_hash: tip,
                },
            },
            schema: keccak256(b"schema"),
            rights_hash: rights.hash(),
            params: vec![],
            registry_root: keccak256(b"r"),
        };
        (spec, rights)
    }

    fn file_of(entry: ShardEntry) -> CatalogFile {
        CatalogFile {
            version: CATALOG_VERSION,
            shards: vec![entry],
            held: vec![],
            markets: vec![],
            default_pricing: None,
            pricing: Default::default(),
            default_dataset_pricing: None,
            dataset_pricing: Default::default(),
        }
    }

    /// L15 stop-serving runbook: `--check-config` names every entry's shard
    /// id, listed and held, so the operator can find the one to remove.
    #[test]
    fn entry_ids_name_each_listed_and_held_shard() {
        let (h, tip, _) = fixture_end();
        let (spec, rights) = block_spec("btc.mainnet", tip);
        let mut f = file_of(
            ShardEntry::new(7, "test.flow", &spec, &rights, Some(1), Some(&h)).expect("entry"),
        );
        f.held
            .push(HeldEntry::new("test.held", &spec, &rights, Some(1), Some(&h)).expect("held"));
        let want = format!("0x{}", hex::encode(spec.shard_id().0));
        assert_eq!(
            f.entry_ids().expect("ids"),
            vec![
                format!("shards[0] round 7 metric test.flow shard {want}"),
                format!("held[0] metric test.held shard {want}"),
            ]
        );
        f.held[0].spec = "zz".into();
        assert!(f.entry_ids().is_err());
    }

    /// AUD-2-F2, AUD-2R-F2: a block snapshot's data time comes from its end
    /// header, which must hash to the pinned tip; the price clock of a
    /// late-finalized round starts no later than a day after it.
    #[test]
    fn a_block_snapshots_time_comes_from_its_end_header() {
        let (h, tip, t) = fixture_end();
        let (spec, rights) = block_spec("btc.mainnet", tip);
        let now = t + 86_400 * 365;
        let entry = |hdr: Option<&[u8]>| {
            ShardEntry::new(1, "test.flow", &spec, &rights, Some(1), hdr).expect("entry")
        };
        assert!(file_of(entry(None))
            .to_catalog_at(now)
            .expect_err("no header")
            .contains("endHeader"));
        let c = file_of(entry(Some(&h)))
            .to_catalog_at(now)
            .expect("catalog");
        let meta = c.shard(1, &spec.shard_id()).expect("shard");
        assert_eq!(meta.snapshot_unix, Some(t));
        assert_eq!(meta.clock_start(t + 30 * 86_400), t + 86_400);
        assert_eq!(meta.proof_level, onyx_core::ProofLevel::Recomputed);
        // A header that is not the tip's (one byte changed) is refused.
        let mut forged = h.clone();
        forged[68] ^= 1;
        assert!(file_of(entry(Some(&forged)))
            .to_catalog_at(now)
            .expect_err("forged")
            .contains("tip hash"));
        // A stated snapshotUnix must equal the header's time.
        let mut e = entry(Some(&h));
        e.snapshot_unix = Some(t + 1);
        assert!(file_of(e)
            .to_catalog_at(now)
            .expect_err("disagree")
            .contains("disagrees"));
        // A data time more than two hours ahead of the clock is refused.
        assert!(file_of(entry(Some(&h)))
            .to_catalog_at(t - MAX_FUTURE_SECS - 1)
            .expect_err("future")
            .contains("after now"));
        // A chain whose header format the node does not know is refused.
        let (other, rights) = block_spec("test", tip);
        let e = ShardEntry::new(1, "test.flow", &other, &rights, Some(1), Some(&h)).expect("entry");
        assert!(file_of(e)
            .to_catalog_at(now)
            .expect_err("chain")
            .contains("no header format"));
        // AUD-2-F6: an unchecked input weakens the level, never strengthens it.
        let mut f = file_of(entry(Some(&h)));
        f.shards[0].input_level = Some(4);
        let c = f.to_catalog_at(now).expect("catalog");
        assert_eq!(
            c.shard(1, &spec.shard_id()).expect("shard").proof_level,
            onyx_core::ProofLevel::BondedClaim
        );
        f.shards[0].input_level = Some(9);
        assert!(f.to_catalog_at(now).is_err());
    }

    /// R-LBL-FIX-1 (L9 sweep): with the indexer off, a listed shard whose
    /// pinned non-test label set is not held, or cites evidence the store
    /// cannot resolve, stops the start; a held, evidenced set passes; a
    /// `test.*` set is left to the per-request refusal; no set passes.
    #[test]
    fn a_listed_shards_label_set_must_be_held_and_evidenced() {
        use onyx_coordinator::cas::{CasExt, Kind, MemCas};
        use onyx_core::{Confidence, EntityKind, EvidenceRef, LabelSetRef};
        use onyx_labels::{Label, LabelSet};
        let (h, tip, _) = fixture_end();
        let cas = MemCas::new();
        let doc = cas
            .put(Kind::Blob, b"published reserve list")
            .expect("blob");
        let set = |name: &str, evidence: Hash32| LabelSet {
            name: name.into(),
            version: 1,
            labels: vec![Label {
                address: "t:a".into(),
                entity: "entity:example".into(),
                kind: EntityKind::Exchange,
                confidence: Confidence(900_000),
                evidence: vec![EvidenceRef {
                    kind: "por-list".into(),
                    content_hash: evidence,
                }],
            }],
        };
        let listed = |l: Option<&LabelSet>| {
            let (mut spec, rights) = block_spec("btc.mainnet", tip);
            spec.label_set = l.map(|s| LabelSetRef {
                name: s.name.clone(),
                version: s.version,
                content_hash: s.content_hash(),
            });
            file_of(
                ShardEntry::new(3, "test.flow", &spec, &rights, Some(1), Some(&h)).expect("entry"),
            )
        };
        assert!(listed(None).check_listed_label_sets(&cas).is_ok());
        // Not in the store: refused, naming the round and the set.
        let good = set("onyx.entities.example", doc);
        let err = listed(Some(&good))
            .check_listed_label_sets(&cas)
            .expect_err("not held");
        assert!(
            err.contains("round 3") && err.contains("label_set_missing"),
            "{err}"
        );
        cas.put_label_set(&good).expect("set");
        assert!(listed(Some(&good)).check_listed_label_sets(&cas).is_ok());
        // Held, but its evidence does not resolve in the store.
        let unresolved = set("onyx.entities.example2", onyx_core::keccak256(b"nowhere"));
        cas.put_label_set(&unresolved).expect("set");
        let err = listed(Some(&unresolved))
            .check_listed_label_sets(&cas)
            .expect_err("evidence");
        assert!(err.contains("evidence_missing"), "{err}");
        // A test set is refused per request instead (403 test_labels).
        let test = set(
            "test.onyx.entities.example",
            onyx_core::keccak256(b"nowhere"),
        );
        assert!(listed(Some(&test)).check_listed_label_sets(&cas).is_ok());
    }

    /// ADR-0033: held shards are checked like listed ones, have no round,
    /// may carry markets, and are not in the served catalog.
    #[test]
    fn held_shards_are_checked_and_not_served() {
        let (h, tip, t) = fixture_end();
        let (spec, rights) = block_spec("btc.mainnet", tip);
        let now = t + 86_400;
        let held = HeldEntry::new("test.flow", &spec, &rights, Some(2), Some(&h)).expect("held");
        let mut f = file_of(
            ShardEntry::new(1, "test.flow", &spec, &rights, Some(2), Some(&h)).expect("entry"),
        );
        f.shards.clear();
        f.held = vec![held.clone()];
        f.markets.push(MarketEntry {
            id: "m".into(),
            round: 7,
            shard: spec.shard_id().to_string(),
            subject: "entity:x".into(),
        });
        let c = f.to_catalog_at(now).expect("catalog");
        assert!(
            c.shard_keys().is_empty(),
            "a held shard is not served until indexed"
        );
        let hs = f.holdings_at(now).expect("holdings");
        let got = hs.get(&spec.shard_id()).expect("held");
        assert_eq!(got.meta.leaf_count, Some(2));
        assert_eq!(got.meta.snapshot_unix, Some(t));
        let json = serde_json::to_string(&f).expect("json");
        assert_eq!(CatalogFile::from_json(&json).expect("parses"), f);
        // Twice, or with a header that is not the tip's: refused.
        f.held.push(held.clone());
        assert!(f.holdings_at(now).expect_err("twice").contains("twice"));
        f.held = vec![HeldEntry {
            end_header: Some(format!("0x{}", "00".repeat(80))),
            ..held
        }];
        assert!(f.holdings_at(now).is_err());
        assert!(f.to_catalog_at(now).is_err());
    }
}
