//! The serving check of `btc.entity-reserve` as an `onyx-api` gate (ADR-0034
//! Amendment 1, THREATS RES-7 and RES-1).
//!
//! [`ReserveGate`] hands every finalized reserve shard the server holds to
//! `onyx_source_btc::reserve::serving_check`, which derives each shard's
//! expected params from the committed scan and the finalized rows (never from
//! a spec under test), and clears only rows whose entity has an independent
//! cross-check. A separate crate because `onyx-source-btc` also builds for the
//! browser (no server dependencies there).
//!
//! [`client`] is the same check on the client's side (ROADMAP D3, THREATS
//! RES-10): a client that does not trust the operator's `servingCheck`
//! re-runs it on answers it verified on its own node.

pub mod client;

use onyx_api::catalog::ShardMeta;
use onyx_api::gate::{CrossCheck, GateShard, Judgement, RowClearance, ServingGate};
use onyx_core::{keccak256, Hash32, Snapshot};
use onyx_labels::dated::{check_history, DatedLabelSet};
use onyx_labels::LabelSet;
use onyx_source_btc::reserve::{
    flows_from_jsonl, serving_check, AnchorLabels, CrossChecks, FinalizedShard, FlowsFile,
    HeaderChain, ScanSnapshot, SecondScan, ServingAnchor, MIN_CONFIRMATIONS,
};
use onyx_source_btc::{
    load_dated_label_set, load_label_set, node::MAINNET_MIN_BITS, verify_checksum,
    SCHEMA_ENTITY_RESERVE,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Gate name a reserve shard's `servingCheck` names.
pub const GATE: &str = "btc-entity-reserve-v1";
/// API metric served through the gate.
pub const METRIC: &str = "btc.entity-reserve";

/// The label set the gate anchors on (ADR-0037).
pub enum GateLabels {
    /// An undated set (every label at every height).
    Undated(LabelSet),
    /// A dated set, the flows file its membership changes need, and every
    /// earlier version of the set, oldest first (its history is checked:
    /// no version back-dates a membership, review D1-R1 F2).
    Dated(DatedLabelSet, Option<FlowsFile>, Vec<DatedLabelSet>),
}

/// The gate's inputs, all committed or operator-configured files.
pub struct ReserveGate {
    metric: String,
    scan: ScanSnapshot,
    labels: GateLabels,
    headers: HeaderChain,
    start_height: u64,
    recipe: Hash32,
    cross: CrossChecks,
    second: Vec<SecondScan>,
}

/// `ONYX_RESERVE_GATE`: a JSON file naming the inputs. Scan, labels and
/// cross-checks are committed fixtures, checked against the fixtures
/// directory's `SHA256SUMS`; second scans are operator files (paths relative
/// to the config file).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateConfig {
    /// API metric (default [`METRIC`]).
    #[serde(default)]
    pub metric: Option<String>,
    /// The fixtures directory (relative to the config file, or absolute).
    pub fixtures: PathBuf,
    /// Committed scan, in it (`reserves/scan-<height>.json`).
    pub scan: String,
    /// Undated label set the scan names, in it (`labels/<set>.json`). Exactly
    /// one of `labels` and `datedLabels`.
    #[serde(default)]
    pub labels: Option<String>,
    /// Dated label set the scan names, in it (ADR-0037).
    #[serde(default)]
    pub dated_labels: Option<String>,
    /// Flows file for a dated set's membership changes, in it (ADR-0037).
    #[serde(default)]
    pub membership_flows: Option<String>,
    /// Every earlier version of the dated set, oldest first, in it: the
    /// gate refuses a set whose history back-dates a membership.
    #[serde(default)]
    pub dated_history: Vec<String>,
    /// Header chain, in it (`reserves/headers-<from>-<to>.json`): proof of
    /// work from the first served block to at least five blocks past the
    /// scan (RES-4, [`MIN_CONFIRMATIONS`]).
    pub headers: String,
    /// Committed cross-checks, in it (`reserves/cross-checks.json`).
    pub cross_checks: String,
    /// Base height of the first served shard.
    pub start_height: u64,
    /// `0x` recipe id of the pinned `btc-entity-reserve` build.
    pub recipe_id: String,
    /// Re-scans on a second node, each with the flows file linking it to the
    /// committed scan (empty path list: none). The operator's statement that
    /// these come from another node is not checkable by the server.
    #[serde(default)]
    pub second_scans: Vec<SecondScanFiles>,
}

/// One configured second-node re-scan.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecondScanFiles {
    /// The scan.
    pub scan: PathBuf,
    /// Flows from the committed scan's block to this one (absent: same block).
    #[serde(default)]
    pub flows: Option<PathBuf>,
}

fn read(base: &Path, p: &Path) -> Result<Vec<u8>, String> {
    let path = base.join(p);
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

impl ReserveGate {
    /// A gate over these inputs (an undated label set).
    pub fn new(
        scan: ScanSnapshot,
        set: LabelSet,
        start_height: u64,
        recipe: Hash32,
        cross: CrossChecks,
        second: Vec<SecondScan>,
        headers: HeaderChain,
    ) -> Result<Self, String> {
        Self::with_labels(
            scan,
            GateLabels::Undated(set),
            start_height,
            recipe,
            cross,
            second,
            headers,
        )
    }

    /// A gate over these inputs, any label set. The scan must name the set
    /// and be on the header chain with [`MIN_CONFIRMATIONS`] (RES-4), and a
    /// flows file must be on it too.
    pub fn with_labels(
        scan: ScanSnapshot,
        labels: GateLabels,
        start_height: u64,
        recipe: Hash32,
        cross: CrossChecks,
        second: Vec<SecondScan>,
        headers: HeaderChain,
    ) -> Result<Self, String> {
        match &labels {
            GateLabels::Undated(set) => scan.check_labels(set),
            GateLabels::Dated(set, flows, history) => {
                let mut all = history.clone();
                all.push(set.clone());
                check_history(&all).map_err(|e| format!("dated label set history: {e}"))?;
                scan.check_dated(set).and_then(|()| {
                    flows
                        .as_ref()
                        .map_or(Ok(()), |f| f.check_on_chain(&headers, MIN_CONFIRMATIONS))
                })
            }
        }
        .map_err(|e| e.to_string())?;
        if start_height >= scan.height {
            return Err("the start height must be before the scan".into());
        }
        headers
            .require_confirmed(scan.height, &scan.block_hash, MIN_CONFIRMATIONS)
            .map_err(|e| format!("the committed scan (RES-4): {e}"))?;
        Ok(ReserveGate {
            metric: METRIC.into(),
            scan,
            labels,
            headers,
            start_height,
            recipe,
            cross,
            second,
        })
    }

    /// The gate a config file describes.
    pub fn from_config(path: &Path) -> Result<Self, String> {
        let base = path.parent().unwrap_or(Path::new("."));
        let c: GateConfig = serde_json::from_slice(&read(Path::new(""), path)?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let fx = base.join(&c.fixtures);
        let fixture = |rel: &str| -> Result<Vec<u8>, String> {
            verify_checksum(&fx, rel).map_err(|e| format!("{rel}: {e}"))?;
            read(&fx, Path::new(rel))
        };
        let scan: ScanSnapshot =
            serde_json::from_slice(&fixture(&c.scan)?).map_err(|e| format!("scan: {e}"))?;
        let headers = HeaderChain::from_json(
            &String::from_utf8(fixture(&c.headers)?).map_err(|e| format!("headers: {e}"))?,
            // The pinned floor only (review D1-R1 F4: a config cannot lower it).
            MAINNET_MIN_BITS,
        )
        .map_err(|e| format!("headers: {e}"))?;
        let labels = match (&c.labels, &c.dated_labels) {
            (Some(l), None) if c.membership_flows.is_none() && c.dated_history.is_empty() => GateLabels::Undated(
                load_label_set(&fx, l).map_err(|e| format!("labels: {e}"))?,
            ),
            (None, Some(l)) => {
                let set = load_dated_label_set(&fx, l).map_err(|e| format!("datedLabels: {e}"))?;
                let flows = match &c.membership_flows {
                    Some(f) => Some(
                        flows_from_jsonl(
                            &String::from_utf8(fixture(f)?).map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| format!("membershipFlows: {e}"))?,
                    ),
                    None => None,
                };
                let history = c
                    .dated_history
                    .iter()
                    .map(|h| {
                        // An earlier format-1 version (v1, v2 before the dated v3) reads as
                        // undated; `check_history` allows those only as a prefix
                        // (ADR-0037 Amendment 1).
                        load_dated_label_set(&fx, h)
                            .or_else(|e| {
                                load_label_set(&fx, h)
                                    .map(|u| DatedLabelSet::from_undated(&u))
                                    .map_err(|_| e)
                            })
                            .map_err(|e| format!("datedHistory: {e}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                GateLabels::Dated(set, flows, history)
            }
            _ => {
                return Err(
                    "name exactly one of labels and datedLabels (membershipFlows and datedHistory only with datedLabels)"
                        .into(),
                )
            }
        };
        let cross: CrossChecks = serde_json::from_slice(&fixture(&c.cross_checks)?)
            .map_err(|e| format!("cross-checks: {e}"))?;
        let mut second = Vec::new();
        for s in &c.second_scans {
            let scan: ScanSnapshot = serde_json::from_slice(&read(base, &s.scan)?)
                .map_err(|e| format!("second scan: {e}"))?;
            let flows = match &s.flows {
                Some(f) => {
                    let text = String::from_utf8(read(base, f)?).map_err(|e| e.to_string())?;
                    let ff = flows_from_jsonl(&text).map_err(|e| e.to_string())?;
                    // RES-4: the flows between the scans are on the header chain.
                    ff.check_on_chain(&headers, MIN_CONFIRMATIONS)
                        .map_err(|e| format!("second scan flows: {e}"))?;
                    ff.flows
                }
                None => Default::default(),
            };
            second.push(SecondScan { scan, flows });
        }
        let mut recipe = [0u8; 32];
        hex::decode_to_slice(c.recipe_id.trim_start_matches("0x"), &mut recipe)
            .map_err(|_| "recipeId must be 32 bytes of hex")?;
        let mut g = Self::with_labels(
            scan,
            labels,
            c.start_height,
            Hash32(recipe),
            cross,
            second,
            headers,
        )?;
        if let Some(m) = c.metric {
            g.metric = m;
        }
        Ok(g)
    }
}

fn not_cross_checked(entity: &str) -> String {
    format!(
        "{entity} is not independently cross-checked (THREATS RES-1): its base is one node's scan"
    )
}

impl ServingGate for ReserveGate {
    fn name(&self) -> &str {
        GATE
    }

    fn metric(&self) -> &str {
        &self.metric
    }

    /// A reserve row schema or the pinned reserve build, whatever the
    /// catalog calls the shard (review RESAPI-R1 F1).
    fn claims(&self, meta: &ShardMeta) -> bool {
        meta.binding.schema_id == keccak256(SCHEMA_ENTITY_RESERVE)
            || meta.binding.recipe_id == self.recipe
    }

    fn judge(&self, shards: &[GateShard]) -> Vec<Judgement> {
        let mut out: Vec<Option<Judgement>> = vec![None; shards.len()];
        let mut fin = Vec::new();
        let mut at = Vec::new();
        for (i, s) in shards.iter().enumerate() {
            let Snapshot::BlockRange {
                from_height,
                to_height,
                tip_hash,
                ..
            } = s.source.snapshot
            else {
                out[i] = Some(Err("not a block-range shard".into()));
                continue;
            };
            at.push(i);
            fin.push(FinalizedShard {
                round: s.round,
                shard: s.shard,
                root: s.root,
                recipe: s.recipe_id,
                label_set_hash: s.label_set_hash,
                schema: s.schema_id,
                from: from_height,
                to: to_height,
                tip: tip_hash,
                params: s.params.clone(),
                leaves: s.leaves.clone(),
            });
        }
        let labels = match &self.labels {
            GateLabels::Undated(set) => AnchorLabels::Undated(set),
            GateLabels::Dated(set, flows, _) => AnchorLabels::Dated {
                set,
                flows: flows.as_ref(),
            },
        };
        let anchor = ServingAnchor {
            scan: &self.scan,
            labels,
            headers: &self.headers,
            start_height: self.start_height,
            recipe: self.recipe,
            cross_checks: &self.cross,
            second_scans: &self.second,
        };
        match serving_check(&anchor, &fin) {
            Ok(v) => {
                for (&i, (_, _, r)) in at.iter().zip(v.shards) {
                    out[i] = Some(r.map(|rows| {
                        rows.into_iter()
                            .map(|row| RowClearance {
                                key: row.entity.as_bytes().to_vec(),
                                cross_check: row
                                    .cross_check
                                    .map(|c| CrossCheck {
                                        basis: c.basis,
                                        source: c.source,
                                    })
                                    .ok_or_else(|| not_cross_checked(&row.entity)),
                            })
                            .collect()
                    }));
                }
            }
            Err(e) => {
                for &i in &at {
                    out[i] = Some(Err(format!("the committed scan does not anchor: {e}")));
                }
            }
        }
        out.into_iter()
            .map(|o| o.unwrap_or_else(|| Err("not judged".into())))
            .collect()
    }
}
