//! Properties of `btc-entity-flow-series` beyond its golden file (review
//! SER-R1): every series row equals `btc-entity-balance` run on that one block,
//! and the rows add up to the range row, on random blocks; a quiet range ends
//! with `SERIES_NO_ACTIVITY` on the canonical engine (F1); and the round
//! preflight refuses a shard over half its fuel budget (F2).

use borsh::BorshDeserialize;
use onyx_btc_schema::{
    BlockDelta, BtcSource, EntityBlockFlowRow, EntityFlowRow, TxDelta, TxOut, SCHEMA_VERSION,
    SERIES_NO_ACTIVITY,
};
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::recompute::{preflight, PreflightError, PREFLIGHT_MAX_FUEL_PPM};
use onyx_core::{
    keccak256, ChainId, LabelSetRef, Registry, RuntimeProfile, ShardSpec, Snapshot, SourceAdapter,
    SourceId, SourceRef,
};
use onyx_labels::attest::AttestCache;
use onyx_labels::LabelSet;
use onyx_recipe_host::{Host, Outcome, HOST_ID};
use onyx_recipe_sdk::{LabelSetView, LabelView, RecipeInput, RecipeOutput};
use onyx_source_btc::{
    fixture_registry_series, load_label_set, BtcFixtureAdapter, SCHEMA_ENTITY_FLOW_SERIES,
    SOURCE_ID,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

const BLOCKS: &str = "btc/blocks-921080-921082.json";
const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    }
}

/// Deterministic pseudo-random numbers (no rand dependency in tests).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn v2_addresses() -> Vec<String> {
    load_label_set(&root().join("fixtures"), LABELS_V2)
        .expect("labels v2")
        .labels
        .into_iter()
        .map(|l| l.address)
        .collect()
}

fn input(blocks: Vec<BlockDelta>, labels: &[(String, String)]) -> RecipeInput {
    let mut ls: Vec<LabelView> = labels
        .iter()
        .map(|(a, e)| LabelView {
            address: a.clone(),
            entity: e.clone(),
            kind: 0,
            confidence_ppm: 1_000_000,
            evidence: vec![],
        })
        .collect();
    ls.sort_by(|a, b| a.address.cmp(&b.address));
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
            labels: ls,
        })
        .expect("labels"),
        params: vec![],
    }
}

fn series(o: &RecipeOutput) -> Vec<EntityBlockFlowRow> {
    o.rows
        .iter()
        .map(|r| {
            let v = EntityBlockFlowRow::try_from_slice(&r.value).expect("series row");
            assert_eq!(r.key, EntityBlockFlowRow::key(&v.entity, v.height));
            v
        })
        .collect()
}

fn balance(o: &RecipeOutput) -> Vec<EntityFlowRow> {
    o.rows
        .iter()
        .map(|r| EntityFlowRow::try_from_slice(&r.value).expect("range row"))
        .collect()
}

/// Per block, the series row of (entity, height) equals the balance recipe
/// run on that block alone, and summed over the range it equals the balance
/// row: 400 random ranges with self-transfers, coinbase, an entity on both
/// sides, several scripts per entity, prefix entity names, zero and huge
/// values, and empty or quiet blocks.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn series_equals_balance_per_block_on_random_blocks() {
    let addr = v2_addresses();
    let labels: Vec<(String, String)> = vec![
        (addr[0].clone(), "entity:a".into()),
        (addr[1].clone(), "entity:a".into()),
        (addr[2].clone(), "entity:ab".into()),
        (addr[3].clone(), "entity:ab".into()),
        (addr[4].clone(), "entity:b".into()),
    ];
    let scripts: Vec<Vec<u8>> = labels
        .iter()
        .map(|(a, _)| btc_entity_flow_series::address_script(a).expect("script"))
        .collect();
    let mut rng = Lcg(42);
    for _ in 0..400 {
        let start = 1_000 + rng.below(1 << 40);
        let mut blocks = Vec::new();
        for i in 0..1 + rng.below(6) {
            let mut txs = Vec::new();
            for t in 0..rng.below(5) {
                let pick = |rng: &mut Lcg| -> TxOut {
                    let script = if rng.below(10) < 6 {
                        scripts[rng.below(scripts.len() as u64) as usize].clone()
                    } else {
                        vec![0x6a, rng.below(255) as u8]
                    };
                    let value = match rng.below(4) {
                        0 => 0,
                        1 => u64::MAX / 4,
                        _ => rng.below(100_000_000),
                    };
                    TxOut { script, value }
                };
                let coinbase = t == 0 && rng.below(2) == 0;
                let spent = if coinbase {
                    vec![]
                } else {
                    (0..rng.below(4)).map(|_| pick(&mut rng)).collect()
                };
                let created = (0..rng.below(4)).map(|_| pick(&mut rng)).collect();
                txs.push(TxDelta { spent, created });
            }
            blocks.push(BlockDelta {
                height: start + i,
                hash: [i as u8; 32],
                txs,
            });
        }
        let mut want: Vec<EntityBlockFlowRow> = Vec::new();
        for blk in &blocks {
            let one = btc_entity_balance::run(input(vec![blk.clone()], &labels)).expect("balance");
            for r in balance(&one) {
                if r.tx_count > 0 {
                    want.push(EntityBlockFlowRow {
                        entity: r.entity,
                        height: blk.height,
                        received_sats: r.received_sats,
                        sent_sats: r.sent_sats,
                        net_sats: r.net_sats,
                        tx_count: r.tx_count,
                    });
                }
            }
        }
        want.sort_by(|a, b| (&a.entity, a.height).cmp(&(&b.entity, b.height)));
        let got = btc_entity_flow_series::run(input(blocks.clone(), &labels));
        if want.is_empty() {
            // No labelled activity anywhere: NO_ACTIVITY, never an empty output.
            assert_eq!(got.err(), Some(SERIES_NO_ACTIVITY));
            continue;
        }
        let got = got.expect("series");
        assert!(got.rows.windows(2).all(|w| w[0].key < w[1].key));
        let rows = series(&got);
        assert_eq!(rows, want);
        let mut sums: BTreeMap<String, (u128, u128, i128, u64)> = BTreeMap::new();
        for r in &rows {
            let e = sums.entry(r.entity.clone()).or_default();
            e.0 += r.received_sats;
            e.1 += r.sent_sats;
            e.2 += r.net_sats;
            e.3 += r.tx_count;
        }
        let range = btc_entity_balance::run(input(blocks, &labels)).expect("range");
        for r in balance(&range) {
            assert_eq!(
                sums.remove(&r.entity).unwrap_or_default(),
                (r.received_sats, r.sent_sats, r.net_sats, r.tx_count)
            );
        }
        assert!(sums.is_empty());
    }
}

/// SER-R1-F1 on the canonical engine: a range with no labelled activity ends
/// with `RecipeError(SERIES_NO_ACTIVITY)` (a deterministic failure leaf every
/// honest run reaches), where it used to end with `BadOutput`.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_quiet_range_is_no_activity_on_the_canonical_engine() {
    let addr = v2_addresses();
    let labels = vec![(addr[0].clone(), "entity:a".to_string())];
    let quiet = vec![BlockDelta {
        height: 970_551,
        hash: [0; 32],
        txs: vec![TxDelta {
            spent: vec![],
            created: vec![TxOut {
                script: vec![0x6a],
                value: 5,
            }],
        }],
    }];
    let bytes = borsh::to_vec(&input(quiet, &labels)).expect("input");
    let wasm = std::fs::read(root().join("recipes/out/btc_entity_flow_series.wasm"))
        .expect("built recipe");
    let host = Host::new().expect("host");
    let r = host
        .run(&keccak256(&wasm), &wasm, &bytes, &profile())
        .expect("run");
    assert_eq!(r.outcome, Outcome::RecipeError(SERIES_NO_ACTIVITY));
    // The range recipe on the same input gives an all-zero row.
    let bw = std::fs::read(root().join("recipes/out/btc_entity_balance.wasm")).expect("balance");
    let rb = host
        .run(&keccak256(&bw), &bw, &bytes, &profile())
        .expect("run");
    assert_eq!(rb.outcome, Outcome::Ok);
}

/// A series shard over `blocks` with `labels`, ready for the preflight.
struct Prepared {
    spec: ShardSpec,
    source: Vec<u8>,
    cas: MemCas,
}

fn prepare(blocks: Vec<BlockDelta>, labels: &LabelSet) -> Prepared {
    let reg = fixture_registry_series();
    let wasm = std::fs::read(root().join("recipes/out/btc_entity_flow_series.wasm"))
        .expect("built recipe");
    let first = blocks.first().expect("blocks").height;
    let last = blocks.last().expect("blocks");
    let (to, tip) = (last.height, last.hash);
    let source = borsh::to_vec(&BtcSource {
        schema_version: SCHEMA_VERSION,
        chain: "btc.mainnet".into(),
        blocks,
    })
    .expect("source");
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm).expect("recipe");
    cas.put_label_set(labels).expect("labels");
    cas.put(Kind::Blob, &[]).expect("params");
    let spec = ShardSpec {
        recipe: Some(keccak256(&wasm)),
        runtime: Some(profile()),
        label_set: Some(LabelSetRef {
            name: labels.name.clone(),
            version: labels.version,
            content_hash: labels.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot: Snapshot::BlockRange {
                chain: ChainId("btc.mainnet".into()),
                from_height: first,
                to_height: to,
                tip_hash: onyx_core::Hash32(tip),
                content_hash: keccak256(&source),
            },
        },
        schema: keccak256(SCHEMA_ENTITY_FLOW_SERIES),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .expect("rights")
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    };
    assert_eq!(spec.validate(&reg), Ok(()));
    Prepared { spec, source, cas }
}

fn run_preflight(p: &Prepared) -> Result<u64, PreflightError> {
    preflight(
        &p.spec,
        p.source.clone(),
        &fixture_registry_series(),
        &p.cas,
        &Host::new().expect("host"),
        &AttestCache::new(),
        &allowed(),
    )
}

/// The adapter's valid recipe failures (series: no activity).
fn allowed() -> onyx_coordinator::recompute::FailureAllowList {
    onyx_source_btc::valid_failure_codes().into_iter().collect()
}

/// The fixture blocks and label set v2.
fn fixture() -> (Vec<BlockDelta>, LabelSet) {
    let fixtures = root().join("fixtures");
    let labels = load_label_set(&fixtures, LABELS_V2).expect("labels v2");
    let adapter = BtcFixtureAdapter::new(&fixtures, BLOCKS);
    let snap = adapter.range_snapshot().expect("snapshot");
    let src: BtcSource = borsh::from_slice(&adapter.load(&snap).expect("load")).expect("source");
    (src.blocks, labels)
}

/// Root of the canonical failure leaf for `o`.
fn failure_root(o: Outcome) -> onyx_core::Hash32 {
    onyx_merkle::MerkleTree::from_rows(&[onyx_recipe_host::failure_leaf(o)]).root()
}

/// An honest validator's verdict on a revealed root.
fn judge(p: &Prepared, r: &onyx_core::Hash32) -> onyx_coordinator::validate::Verdict {
    onyx_validator::Validator::new([9u8; 32])
        .expect("validator")
        .judge(
            &p.spec,
            p.source.clone(),
            r,
            &fixture_registry_series(),
            &p.cas,
            &Host::new().expect("host"),
        )
}

/// SER-R2: a lazy miner that claims a failure on an active shard is judged
/// wrong, whatever failure it claims.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_failure_claim_on_an_active_shard_is_disagreed() {
    use onyx_coordinator::validate::Verdict;
    let (blocks, labels) = fixture();
    let p = prepare(blocks, &labels);
    for o in [
        Outcome::RecipeError(SERIES_NO_ACTIVITY),
        Outcome::OutOfFuel,
        Outcome::Trap,
        Outcome::BadOutput,
    ] {
        assert!(
            matches!(judge(&p, &failure_root(o)), Verdict::Disagree { .. }),
            "{o:?}"
        );
    }
}

/// SER-R2: on a quiet shard only the exact no-activity leaf agrees; another
/// failure code, or an engine outcome, is wrong.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn on_a_quiet_shard_only_the_exact_failure_code_agrees() {
    use onyx_coordinator::validate::Verdict;
    let (mut blocks, mut labels) = fixture();
    labels.labels.retain(|l| l.entity == "entity:bitwise-bitb");
    labels.name = "probe.bitb".into();
    blocks.truncate(3);
    let p = prepare(blocks, &labels);
    assert_eq!(
        judge(&p, &failure_root(Outcome::RecipeError(SERIES_NO_ACTIVITY))),
        Verdict::Agree
    );
    for o in [
        Outcome::OutOfFuel,
        Outcome::Trap,
        Outcome::RecipeError(7),
        Outcome::BadOutput,
    ] {
        assert!(
            matches!(judge(&p, &failure_root(o)), Verdict::Disagree { .. }),
            "{o:?}"
        );
    }
    // The quiet shard is a valid round shard: the preflight passes it.
    assert!(run_preflight(&p).is_ok());
}

/// SER-R2-F1: a shard too small for its fuel limit is refused at open, and a
/// miner never commits its out-of-fuel leaf, even with the allow-list.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn an_out_of_fuel_shard_is_refused_and_never_committed() {
    let (blocks, labels) = fixture();
    let mut p = prepare(blocks, &labels);
    p.spec.runtime.as_mut().expect("runtime").fuel_limit = onyx_core::commitment::MIN_FUEL;
    assert!(matches!(
        run_preflight(&p),
        Err(PreflightError::OverBudget { .. })
    ));
    let m = onyx_miner::Miner::new([5u8; 32])
        .expect("miner")
        .with_allowed_failures(allowed());
    assert!(matches!(
        m.work(
            1,
            &p.spec,
            p.source.clone(),
            &fixture_registry_series(),
            &p.cas,
            &Host::new().expect("host"),
        ),
        Err(onyx_miner::MinerError::Failed(Outcome::OutOfFuel))
    ));
}

/// SER-R2-F2: a misbuilt shard whose canonical run fails with a code the
/// schema does not list (here BAD_PARAMS, code 5, from non-empty params) is
/// refused at open and never committed by a miner.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_misbuilt_shard_is_refused_and_never_committed() {
    let (blocks, labels) = fixture();
    let mut p = prepare(blocks, &labels);
    p.spec.params = vec![1];
    p.cas.put(Kind::Blob, &p.spec.params).expect("params");
    assert_eq!(
        run_preflight(&p),
        Err(PreflightError::RefusedFailure(Outcome::RecipeError(5)))
    );
    let m = onyx_miner::Miner::new([6u8; 32])
        .expect("miner")
        .with_allowed_failures(allowed());
    assert!(matches!(
        m.work(
            1,
            &p.spec,
            p.source.clone(),
            &fixture_registry_series(),
            &p.cas,
            &Host::new().expect("host"),
        ),
        Err(onyx_miner::MinerError::Failed(Outcome::RecipeError(5)))
    ));
}

/// SER-R1-F2: the round preflight passes the fixture range (label set v2, well
/// under half the budget) and refuses 24 blocks of fixture density with every
/// input and output doubled (about 85 % of the budget), the busy-block case
/// that would otherwise settle as an out-of-fuel failure leaf.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn preflight_refuses_a_shard_over_half_its_fuel() {
    let fixtures = root().join("fixtures");
    let labels = load_label_set(&fixtures, LABELS_V2).expect("labels v2");
    let adapter = BtcFixtureAdapter::new(&fixtures, BLOCKS);
    let snap = adapter.range_snapshot().expect("snapshot");
    let src: BtcSource =
        borsh::from_slice(&adapter.load(&snap).expect("load")).expect("fixture source");
    let small = run_preflight(&prepare(src.blocks.clone(), &labels)).expect("fixture passes");
    let limit = profile().fuel_limit;
    assert!(
        u128::from(small) * 1_000_000 <= u128::from(limit) * u128::from(PREFLIGHT_MAX_FUEL_PPM),
        "fixture fuel {small}"
    );
    let mut busy = Vec::new();
    for k in 0..8u64 {
        for (i, b) in src.blocks.iter().enumerate() {
            let mut b = b.clone();
            b.height = 1_000 + k * 3 + i as u64;
            for t in b.txs.iter_mut() {
                let created = t.created.clone();
                t.created.extend(created);
                let spent = t.spent.clone();
                t.spent.extend(spent);
            }
            busy.push(b);
        }
    }
    match run_preflight(&prepare(busy, &labels)) {
        Err(PreflightError::OverBudget {
            used,
            limit: l,
            max_ppm,
        }) => {
            assert_eq!((l, max_ppm), (limit, PREFLIGHT_MAX_FUEL_PPM));
            assert!(used * 2 > limit, "busy shard fuel {used}");
            eprintln!("preflight: fixture {small} fuel, busy 24 blocks {used} fuel (refused)");
        }
        other => panic!("busy shard should be refused, got {other:?}"),
    }
}
