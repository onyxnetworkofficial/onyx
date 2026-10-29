//! Rust <-> Solidity vectors for `OnyxRegistry` (ADR-0030): metric keys, the
//! preflight fuel rule, shard ids recomputed from their preimage, and level-2
//! fact keys built with the Treasury feed's real template id.
//! `contracts/test/OnyxRegistry.t.sol` (`OnyxRegistryVectorsTest`) reads the
//! same file and loops over every array to its length. Regenerate only with an
//! ADR change:
//! `ONYX_BLESS=1 cargo test -p onyx-source-fiscaldata --test registry_vectors`.

use onyx_coordinator::observe::RecordedDraw;
use onyx_coordinator::observe::{
    draw_matches_calldata, onchain_observation_check, ConsumerCheckError, DrawSeed, Observer,
    ObserverSig, OnchainFact,
};
use onyx_coordinator::recompute::{within_preflight_budget, PREFLIGHT_MAX_FUEL_PPM};
use onyx_core::onchain::{
    metric_key, registry_fact_key, runtime_bytes, subject_draw_key, subject_key, MetricKind,
    ShardPreimage,
};
use onyx_core::{
    keccak256, Hash32, LabelSetRef, MetricId, RuntimeProfile, ShardSpec, Snapshot, SourceId,
    SourceRef, Subject,
};
use onyx_source_fiscaldata::round::recorded_draw;
use onyx_source_fiscaldata::round::{run, store, Page, RoundRun};
use onyx_source_fiscaldata::{recorded, RECORDED_WINDOW_START};
use serde_json::json;

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/registry/vectors.json"
);

fn hex(h: &[u8]) -> String {
    let mut s = String::from("0x");
    for b in h {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Two shard specs: a labelled level-1 shard with a runtime, and an
/// unlabelled one.
fn specs() -> Vec<ShardSpec> {
    let labelled = ShardSpec {
        recipe: Some(keccak256(b"btc_entity_balance.wasm")),
        runtime: Some(RuntimeProfile {
            abi_version: 1,
            host: "onyx-host.0.5.0.wasmi.2.0.0".into(),
            fuel_limit: 4_000_000_000,
            max_memory_bytes: 64 << 20,
            max_output_bytes: 1 << 20,
            max_call_depth: 1_024,
        }),
        label_set: Some(LabelSetRef {
            name: "test.onyx.entities.btc".into(),
            version: 2,
            content_hash: keccak256(b"label set v2"),
        }),
        source: SourceRef {
            source: SourceId("btc.mainnet.node".into()),
            snapshot: Snapshot::Document {
                published_unix: 1_791_600_000,
                content_hash: keccak256(b"blocks 970543-970554"),
            },
        },
        schema: keccak256(b"onyx.schema.entity_flow.v1"),
        rights_hash: keccak256(b"rights"),
        params: b"entity:okx,entity:binance".to_vec(),
        registry_root: keccak256(b"registry root"),
    };
    let mut bare = labelled.clone();
    bare.label_set = None;
    bare.params = Vec::new();
    vec![labelled, bare]
}

/// The recorded Treasury round (record date 2026-10-08), settled in the
/// adapter's store.
fn recorded_round() -> RoundRun {
    let mut chain = RecordedDraw::new(recorded_draw());
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .into_iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body),
        })
        .collect();
    run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &pages,
        &mut store(),
        &mut chain,
    )
    .expect("recorded round")
}

/// The round's public transcript: each observer's signed commit, ascending
/// by address as the round keeps them (L2API-R2-F3, ATT-R1-F3).
fn commits(r: &RoundRun) -> Vec<([u8; 20], Hash32, ObserverSig)> {
    r.commits.clone()
}

/// REG-R2-F3: a consumer rebuilds `observationLevel`'s arguments from its own
/// copy of the template and the round's transcript; they equal what the
/// coordinator settled, and a wrong registration or a forged reveal is refused
/// before the chain is asked.
#[test]
fn a_consumer_rebuilds_the_observation_from_its_own_template() {
    let r = recorded_round();
    let template = onyx_source_fiscaldata::template();
    let kind = MetricKind::of(&template);
    assert_eq!(
        kind,
        MetricKind::DatedFeed,
        "the Treasury template is dated"
    );
    let c = commits(&r);
    let check = onchain_observation_check(
        &template,
        kind,
        &template.id(),
        &r.spec.subject,
        r.spec.window_start,
        &r.cfg,
        &r.pool,
        &r.draw,
        &r.observers,
        &c,
        &r.reveals,
    )
    .expect("consumer check");
    assert_eq!(check.spec_id, r.spec.id());
    assert_eq!(check.result, r.settlement.result_hash());
    assert_eq!(check.window_end, r.spec.window_end);
    // A metric registered as `Feed` (one fact per window) is refused (REG-R2-F2).
    let wrong_kind = onchain_observation_check(
        &template,
        MetricKind::Feed,
        &template.id(),
        &r.spec.subject,
        r.spec.window_start,
        &r.cfg,
        &r.pool,
        &r.draw,
        &r.observers,
        &c,
        &r.reveals,
    );
    assert_eq!(wrong_kind, Err(ConsumerCheckError::Registration));
    // Another registered template id is refused.
    let other = onchain_observation_check(
        &template,
        kind,
        &keccak256(b"rogue template"),
        &r.spec.subject,
        r.spec.window_start,
        &r.cfg,
        &r.pool,
        &r.draw,
        &r.observers,
        &c,
        &r.reveals,
    );
    assert_eq!(other, Err(ConsumerCheckError::Registration));
    // A reveal that does not open its commit is refused.
    let mut forged = r.reveals.clone();
    forged[0].1.observed_at += 1;
    let bad = onchain_observation_check(
        &template,
        kind,
        &template.id(),
        &r.spec.subject,
        r.spec.window_start,
        &r.cfg,
        &r.pool,
        &r.draw,
        &r.observers,
        &c,
        &forged,
    );
    assert_eq!(bad, Err(ConsumerCheckError::BadReveal));
    // ATT-R1-F3: a commit or reveal its observer did not sign is refused.
    let rebuild = |c: &[([u8; 20], Hash32, ObserverSig)], rv: &[_]| {
        onchain_observation_check(
            &template,
            kind,
            &template.id(),
            &r.spec.subject,
            r.spec.window_start,
            &r.cfg,
            &r.pool,
            &r.draw,
            &r.observers,
            c,
            rv,
        )
    };
    let mut unsigned = c.clone();
    unsigned[0].2 = unsigned[1].2;
    assert_eq!(
        rebuild(&unsigned, &r.reveals),
        Err(ConsumerCheckError::BadSignature)
    );
    let mut swapped = r.reveals.clone();
    swapped[0].3 = swapped[1].3;
    assert_eq!(rebuild(&c, &swapped), Err(ConsumerCheckError::BadSignature));
}

/// L2-F2: the observers are drawn from the bonded pool with the round's
/// beacon. A consumer recomputes the draw and refuses a hand-picked set (pool
/// members the draw did not pick), a pool the round was not drawn from, and
/// another beacon, before any reveal is checked.
#[test]
fn a_consumer_refuses_observers_the_draw_did_not_pick() {
    let r = recorded_round();
    let template = onyx_source_fiscaldata::template();
    let kind = MetricKind::of(&template);
    let c = commits(&r);
    let check = |pool: &[Observer], draw: &DrawSeed, observers: &[Observer]| {
        onchain_observation_check(
            &template,
            kind,
            &template.id(),
            &r.spec.subject,
            r.spec.window_start,
            &r.cfg,
            pool,
            draw,
            observers,
            &c,
            &r.reveals,
        )
    };
    assert!(
        r.pool.len() > r.observers.len(),
        "the pool is larger than the draw"
    );
    assert!(check(&r.pool, &r.draw, &r.observers).is_ok());
    // Hand-picked: swap one drawn observer for a pool member not drawn.
    let outsider = *r
        .pool
        .iter()
        .find(|o| !r.observers.contains(o))
        .expect("an undrawn member");
    let mut picked = r.observers.clone();
    picked[0] = outsider;
    picked.sort_by_key(|o| o.address);
    assert!(matches!(
        check(&r.pool, &r.draw, &picked),
        Err(ConsumerCheckError::BadRound(_))
    ));
    // The pool shrunk to the drawn set: not the pool the seed commits to.
    assert!(matches!(
        check(&r.observers, &r.draw, &r.observers),
        Err(ConsumerCheckError::BadRound(_))
    ));
    // Another beacon draws another set.
    let mut other = r.draw;
    other.beacon = keccak256(b"another beacon");
    assert!(matches!(
        check(&r.pool, &other, &r.observers),
        Err(ConsumerCheckError::BadRound(_))
    ));
}

#[test]
fn registry_vectors_match_the_contract_file() {
    let ids = [
        "btc.entity-flow",
        "crypto.btc.entity_flow",
        "crypto.btc.entity_flow_series",
        onyx_source_fiscaldata::METRIC,
        "",
    ];
    let keys: Vec<String> = ids
        .iter()
        .map(|m| hex(&metric_key(&MetricId((*m).to_string())).0))
        .collect();
    let limit = 4_000_000_000u64;
    let half = limit / 2;
    let budget = [
        (0, limit),
        (half, limit),
        (half + 1, limit),
        (limit, limit),
        (0, 0),
        (1, 1),
        (1, 2),
        (u64::MAX / 2, u64::MAX),
        (u64::MAX / 2 + 1, u64::MAX),
    ];
    let pre: Vec<serde_json::Value> = specs()
        .iter()
        .map(|s| {
            let p = ShardPreimage::of(s);
            assert_eq!(p.shard_id(), s.shard_id(), "preimage and spec agree");
            json!({
                "recipeId": hex(&p.recipe_id.0),
                "runtimeHash": hex(&p.runtime_hash.0),
                "labelSetHash": hex(&p.label_set_hash.0),
                "sourceHash": hex(&p.source_hash.0),
                "schemaId": hex(&p.schema_id.0),
                "rightsHash": hex(&p.rights_hash.0),
                "paramsHash": hex(&p.params_hash.0),
                "registryRoot": hex(&p.registry_root.0),
            })
        })
        .collect();
    let shard_ids: Vec<String> = specs().iter().map(|s| hex(&s.shard_id().0)).collect();
    // REG-R2-F1: the runtime bytes whose keccak is each preimage's runtime hash,
    // and the fuel limit the contract must read out of them.
    let runtimes: Vec<RuntimeProfile> = specs().iter().filter_map(|s| s.runtime.clone()).collect();
    for (s, rt) in specs().iter().zip(&runtimes) {
        assert_eq!(
            keccak256(&runtime_bytes(rt)),
            ShardPreimage::of(s).runtime_hash
        );
    }

    // Fact keys under the Treasury feed's real template id (by value).
    let template = onyx_source_fiscaldata::template();
    let mk = metric_key(&template.metric);
    let tid = template.id();
    let facts = [
        (
            "us.treasury.federal_debt:2026-10-08",
            1_791_706_200u64,
            true,
        ),
        ("us.treasury.federal_debt:2026-10-08", 1_791_792_600, true),
        ("us.treasury.federal_debt:2026-10-09", 1_791_706_200, true),
        ("us.treasury.federal_debt:2026-10-08", 1_791_706_200, false),
    ];
    let subj: Vec<String> = facts
        .iter()
        .map(|(s, _, _)| hex(&subject_key(&Subject::Aggregate((*s).into())).0))
        .collect();
    let fact_keys: Vec<String> = facts
        .iter()
        .map(|(s, w, d)| {
            let sk = subject_key(&Subject::Aggregate((*s).into()));
            hex(&registry_fact_key(&mk, &tid, &sk, *w, *d).0)
        })
        .collect();

    // REG-R2-F3: what a consumer passes to `observationLevel` for the
    // recorded round, built from its own template copy.
    let r = recorded_round();
    let obs = onchain_observation_check(
        &template,
        MetricKind::of(&template),
        &tid,
        &r.spec.subject,
        r.spec.window_start,
        &r.cfg,
        &r.pool,
        &r.draw,
        &r.observers,
        &commits(&r),
        &r.reveals,
    )
    .expect("consumer check");
    assert_eq!(obs.result, r.settlement.result_hash());

    let doc = json!({
        "about": "OnyxRegistry vectors (ADR-0030), generated by adapters/source-fiscaldata/tests/registry_vectors.rs",
        "preflightMaxFuelPpm": PREFLIGHT_MAX_FUEL_PPM.to_string(),
        "metricIds": ids,
        "metricKeys": keys,
        "budgetFuelUsed": budget.iter().map(|(u, _)| u.to_string()).collect::<Vec<_>>(),
        "budgetFuelLimit": budget.iter().map(|(_, l)| l.to_string()).collect::<Vec<_>>(),
        "budgetOk": budget.iter().map(|(u, l)| within_preflight_budget(*u, *l)).collect::<Vec<_>>(),
        "shardPreimages": pre,
        "shardIds": shard_ids,
        "runtimeBytes": runtimes.iter().map(|rt| hex(&runtime_bytes(rt))).collect::<Vec<_>>(),
        "runtimeFuelLimits": runtimes.iter().map(|rt| rt.fuel_limit.to_string()).collect::<Vec<_>>(),
        "treasuryFeed": {
            "metric": onyx_source_fiscaldata::METRIC,
            "metricKey": hex(&mk.0),
            "templateId": hex(&tid.0),
            "kind": MetricKind::of(&template) as u8,
        },
        // L2RND-R1: the recorded round's draw as the rehearsal's registry
        // recorded it, and the `drawMatches` calldata a client sends.
        "treasuryDraw": {
            "attempt": r.draw.attempt,
            "commit": hex(&obs.draw_commit().0),
            // L2RND-R3: the commitment's inputs (`OnyxRegistry.drawCommit`).
            "poolHash": hex(&r.draw.pool.0),
            "cfgHash": hex(&obs.cfg_hash.0),
            "beaconHeight": r.draw.beacon_height.to_string(),
            "beacon": hex(&r.draw.beacon.0),
            "drawMatchesCalldata": hex(&draw_matches_calldata(
                &OnchainFact {
                    metric_key: obs.metric_key,
                    subject_key: obs.subject_key,
                    window_end: obs.window_end,
                    dated: true,
                    template: onyx_source_fiscaldata::template().id(),
                },
                &r.draw,
                &obs.draw_commit(),
            )),
        },
        "treasuryObservation": {
            "subjectKey": hex(&obs.subject_key.0),
            "windowEnd": obs.window_end.to_string(),
            "specId": hex(&obs.spec_id.0),
            "result": hex(&obs.result.0),
        },
        "factSubjectKeys": subj,
        "factWindowEnds": facts.iter().map(|(_, w, _)| w.to_string()).collect::<Vec<_>>(),
        "factDated": facts.iter().map(|(_, _, d)| *d).collect::<Vec<_>>(),
        "factKeys": fact_keys,
        // L2RND-R2-F1: the per-(metric, subject) draw bound's key, one per
        // fact subject (shifted windows of a subject share it).
        "subjectDrawKeys": facts
            .iter()
            .map(|(s, _, _)| {
                let sk = subject_key(&Subject::Aggregate((*s).into()));
                hex(&subject_draw_key(&mk, &sk).0)
            })
            .collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::create_dir_all(std::path::Path::new(PATH).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(PATH, &text).expect("write vectors");
    }
    let on_disk = std::fs::read_to_string(PATH)
        .expect("fixtures/registry/vectors.json missing; run with ONYX_BLESS=1");
    assert_eq!(on_disk, text, "registry vectors drifted from ADR-0030");
}

/// The template id in the file is the Treasury template's own id (by value),
/// so the Solidity fact keys are built from the real registered template.
#[test]
fn the_vector_template_id_is_the_treasury_feeds() {
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(PATH).expect("vectors")).expect("json");
    assert_eq!(
        on_disk["treasuryFeed"]["templateId"],
        hex(&onyx_source_fiscaldata::template().id().0)
    );
}
