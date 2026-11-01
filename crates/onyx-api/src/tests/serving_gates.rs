//! Serving checks (ADR-0034 Amendment 1, `gate.rs`): a gated metric serves only the rows its
//! gate cleared, a gate claims a shard by its binding, and a failed read keeps the last judged root.

use super::*;

/// A gate that clears `entity:a` and `entity:b` and refuses `entity:c`, or
/// refuses the whole shard; it records the params it was handed. It claims
/// every shard with the test schema. With `chain` = n it judges like a
/// chain: fewer than n shards refuse them all (a missing link).
struct TestGate {
    refuse_shard: bool,
    seen: std::sync::Mutex<Vec<Option<Vec<u8>>>>,
    metric: &'static str,
    chain: AtomicU64,
    /// Leave out the refused row's clearance (a gate answering short, L9A-4).
    short: AtomicBool,
}

impl gate::ServingGate for TestGate {
    fn name(&self) -> &str {
        "test-gate"
    }
    fn metric(&self) -> &str {
        self.metric
    }
    fn claims(&self, meta: &ShardMeta) -> bool {
        meta.binding.schema_id == keccak256(b"schema")
    }
    fn judge(&self, shards: &[gate::GateShard]) -> Vec<gate::Judgement> {
        if (shards.len() as u64) < self.chain.load(Ordering::SeqCst) {
            return vec![Err("the chain has a gap".into()); shards.len()];
        }
        shards
            .iter()
            .map(|s| {
                if let Ok(mut seen) = self.seen.lock() {
                    seen.push(s.params.clone());
                }
                if self.refuse_shard {
                    return Err("forged base".into());
                }
                let short = self.short.load(Ordering::SeqCst);
                Ok(s.leaves
                    .iter()
                    .filter(|l| {
                        !short || OutputRow::try_from_slice(l).expect("row").key != b"entity:c"
                    })
                    .map(|l| {
                        let o = OutputRow::try_from_slice(l).expect("row");
                        let ok = o.key != b"entity:c";
                        gate::RowClearance {
                            key: o.key,
                            cross_check: if ok {
                                Ok(gate::CrossCheck {
                                    basis: "issuer-wallet-list".into(),
                                    source: "test list".into(),
                                })
                            } else {
                                Err("entity:c is not independently cross-checked".into())
                            },
                        }
                    })
                    .collect())
            })
            .collect()
    }
}

fn gated(refuse_shard: bool, paid: bool) -> (Fixture, Arc<TestGate>) {
    let g = Arc::new(TestGate {
        refuse_shard,
        seen: std::sync::Mutex::new(Vec::new()),
        metric: "test.flow",
        chain: AtomicU64::new(0),
        short: AtomicBool::new(false),
    });
    let f = build(Setup {
        paid,
        facilitator: paid.then(mock).flatten(),
        gate: Some(g.clone()),
        ..Setup::default()
    });
    (f, g)
}

/// R-GATE-1: a gated shard is not served before its serving check ran (503
/// pending, before any 402); after it, cleared rows carry `servingCheck`
/// (and the client reads it), an uncleared row is 403 `not_cross_checked`
/// before any 402, and an export with such a row is refused.
#[tokio::test(start_paused = true)]
async fn a_gated_metric_serves_only_cleared_rows() {
    let (f, g) = gated(false, true);
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("serving_check_pending")
        )
    );
    // The params come from the store by the params hash.
    f.cas.put(Kind::Blob, b"params one").expect("params");
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 1));
    assert_eq!(
        *g.seen.lock().expect("lock"),
        vec![Some(b"params one".to_vec())]
    );
    // Refused row: 403 before the 402, nothing charged.
    let (st, h, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_cross_checked"))
    );
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
    let (st, h, _) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
    let pay = pay(&h, "payer a", 1);
    let (st, _, b) = get(
        &f,
        "/v1/metrics/test.flow?subject=entity:a",
        &[(x402::PAYMENT_SIGNATURE, pay)],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["servingCheck"]["gate"], "test-gate");
    assert_eq!(b["servingCheck"]["crossChecked"], true);
    let v = verify_response_unchecked(
        &b,
        &Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:a",
            as_of: None,
            min_as_of: 0,
        },
    )
    .expect("client check");
    let claim = v.require_cross_checked().expect("claimed");
    assert_eq!(
        claim.bases,
        vec![("issuer-wallet-list".to_string(), "test list".to_string())]
    );
    assert!(v.to_string().contains("cross_checked=true"));
    // A forged claim shape is refused by the client.
    let mut bad = b.clone();
    bad["servingCheck"]["crossChecked"] = json!(false);
    assert!(verify_response_unchecked(
        &bad,
        &Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:a",
            as_of: None,
            min_as_of: 0,
        },
    )
    .is_err());
    // Proof by index: row 2 (entity:c) is refused before any 402.
    let uri = format!("/v1/proofs/1/{}/2", f.shard);
    let (st, h, b) = get(&f, &uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_cross_checked"))
    );
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
    // An export would carry entity:c: refused.
    let uri = format!("/v1/export/1/{}", f.shard);
    let (st, _, b) = get(&f, &uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_cross_checked"))
    );
    // Health shows the run.
    let (_, _, h) = get(&f, "/v1/health", &[]).await;
    assert_eq!(h["servingChecks"][0]["cleared"], 1);
}

/// THREATS L9A-4: a gate whose clearance list is shorter than the shard
/// (here: the refused row left out) clears nothing, so an export, which walks
/// the clearance list, can never carry the uncleared row.
#[tokio::test(start_paused = true)]
async fn a_gate_answering_fewer_rows_than_leaves_clears_nothing() {
    let (f, g) = gated(false, false);
    f.cas.put(Kind::Blob, b"params one").expect("params");
    g.short.store(true, Ordering::SeqCst);
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 0));
    assert!(runs[0]
        .first_refusal
        .as_deref()
        .is_some_and(|m| m.contains("cleared 2 of 3 rows")));
    for uri in [
        format!("/v1/export/1/{}", f.shard),
        "/v1/metrics/test.flow?subject=entity:a".to_string(),
    ] {
        let (st, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("serving_check_failed")),
            "{uri}"
        );
    }
    // A full answer clears the shard again.
    g.short.store(false, Ordering::SeqCst);
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 1));
}

/// R-GATE-2: a shard the serving check refused is never served, whatever
/// the endpoint; a shard naming a gate the server lacks is refused (fail
/// closed); a client that requires a cross-check refuses an ungated answer.
#[tokio::test(start_paused = true)]
async fn a_refused_or_unchecked_shard_is_not_served() {
    let (f, _) = gated(true, false);
    gate::refresh(&f.state).await;
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_failed"))
    );
    assert!(b["error"]["message"]
        .as_str()
        .is_some_and(|m| m.contains("forged base")));
    for uri in [
        format!("/v1/proofs/1/{}/0", f.shard),
        format!("/v1/export/1/{}", f.shard),
        "/v1/settlement/okx-flow".to_string(),
    ] {
        let (st, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("serving_check_failed")),
            "{uri}"
        );
    }
    // No gate on this server, but the shard names one: refused.
    let f = build(Setup {
        serving_check: Some("btc-entity-reserve-v1"),
        ..Setup::default()
    });
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_unavailable"))
    );
    // Ungated: served without a claim, and a client requiring one refuses it.
    let f = fixture(false, None);
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::OK);
    let v = verify_response_unchecked(
        &b,
        &Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:a",
            as_of: None,
            min_as_of: 0,
        },
    )
    .expect("client check");
    assert_eq!(v.serving, None);
    assert!(v.require_cross_checked().is_err());
}

/// R-GATE-3: an answer is tied to the root it judged: when the chain's root
/// for the shard changes (the catalog row stays), the old answer no longer
/// serves it.
#[tokio::test(start_paused = true)]
async fn a_serving_answer_is_bound_to_the_root_it_judged() {
    let (f, _) = gated(false, false);
    gate::refresh(&f.state).await;
    let (st, _, _) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::OK);
    let other = f
        .cas
        .put_rows(&[row("entity:a", b"other value....")])
        .expect("rows");
    let gs = &f.state.gates;
    let m = f
        .state
        .catalog
        .read()
        .expect("lock")
        .shard(1, &f.shard)
        .cloned()
        .expect("meta");
    assert!(matches!(gate::cleared(gs, &m, &f.root), Ok(Some(_))));
    assert!(matches!(
        gate::cleared(gs, &m, &other),
        Err(gate::Refusal::Pending)
    ));
}

fn gate_with(metric: &'static str, chain: u64) -> Arc<TestGate> {
    Arc::new(TestGate {
        refuse_shard: false,
        seen: std::sync::Mutex::new(Vec::new()),
        metric,
        chain: AtomicU64::new(chain),
        short: AtomicBool::new(false),
    })
}

fn shard_meta(f: &Fixture, round: u64, shard: &Hash32) -> ShardMeta {
    f.state
        .catalog
        .read()
        .expect("lock")
        .shard(round, shard)
        .cloned()
        .expect("meta")
}

/// R-GATE-4 (review RESAPI-R1 F1): a gate claims shards by their binding,
/// not only by the catalog's metric name or `servingCheck`: a shard listed
/// under another metric, naming no check, is still judged before any row is
/// served; a shard two different gates apply to is refused.
#[tokio::test(start_paused = true)]
async fn a_gate_claims_a_shard_by_its_binding_not_its_catalog_name() {
    let f = build(Setup {
        gate: Some(gate_with("another.metric", 0)),
        ..Setup::default()
    });
    let uri = "/v1/metrics/test.flow?subject=entity:a";
    let (st, _, b) = get(&f, uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("serving_check_pending")
        ),
        "{b}"
    );
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 1));
    let (st, _, b) = get(&f, uri, &[]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["servingCheck"]["gate"], "test-gate");
    // The catalog names another check than the gate claiming the binding.
    let f = build(Setup {
        gate: Some(gate_with("another.metric", 0)),
        serving_check: Some("some-other-check"),
        ..Setup::default()
    });
    gate::refresh(&f.state).await;
    let (st, _, b) = get(&f, uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_failed"))
    );
    assert!(b["error"]["message"]
        .as_str()
        .is_some_and(|m| m.contains("several serving checks")));
}

/// R-GATE-5 (review RESAPI-R1 F1): the known gated bindings are compiled
/// into every server, so one without the gate still knows a reserve shard
/// by its schema id or recipe id.
#[test]
fn known_gates_are_well_formed_and_claim_by_binding() {
    let mut names = std::collections::BTreeSet::new();
    for k in gate::KNOWN_GATES {
        assert!(names.insert(k.name), "duplicate {}", k.name);
        assert!(!k.schemas.is_empty());
        for r in k.recipe_ids {
            let mut b = [0u8; 32];
            hex::decode_to_slice(r.trim_start_matches("0x"), &mut b).expect("32-byte hex");
            assert_ne!(b, [0u8; 32]);
        }
    }
    let mut m = meta(1, 100, b"params one", 3);
    assert_eq!(gate::known_claim(&m), None);
    m.binding.schema_id = keccak256(b"onyx.btc.entity-reserve-row.v1");
    assert_eq!(gate::known_claim(&m), Some("btc-entity-reserve-v1"));
    let mut m = meta(1, 100, b"params one", 3);
    let mut r = [0u8; 32];
    hex::decode_to_slice(
        "d0c1331f82f9a743548ca68de3693a2754ae35e6faef7e047a739ba939b49e34",
        &mut r,
    )
    .expect("hex");
    m.binding.recipe_id = Hash32(r);
    assert_eq!(gate::known_claim(&m), Some("btc-entity-reserve-v1"));
    // A server with no gate refuses it (fail closed), whatever the metric.
    let gs = gate::Gates::default();
    assert_eq!(
        gate::cleared(&gs, &m, &Hash32::ZERO).err(),
        Some(gate::Refusal::Unavailable("btc-entity-reserve-v1".into()))
    );
}

/// R-GATE-7 (review RESAPI-R2-1, the review's scenario): a later reserve
/// build that `KNOWN_GATES` does not list (another recipe id, another schema
/// id) listed under `btc.entity-reserve` on a server without the gate (plain
/// `onyx-node`) was served unchecked. The known gate now also claims its
/// metric names, so every endpoint refuses it (403
/// `serving_check_unavailable`, before any 402); the same shard under
/// another metric is not a reserve shard to this server.
#[tokio::test(start_paused = true)]
async fn a_gated_metric_name_fails_closed_on_any_server() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        metric: Some("btc.entity-reserve"),
        ..Setup::default()
    });
    let m = shard_meta(&f, 1, &f.shard);
    assert_ne!(
        m.binding.schema_id,
        keccak256(b"onyx.btc.entity-reserve-row.v1")
    );
    assert_eq!(gate::known_claim(&m), Some("btc-entity-reserve-v1"));
    for uri in [
        "/v1/metrics/btc.entity-reserve?subject=entity:a".to_string(),
        format!("/v1/proofs/1/{}/0", f.shard),
        format!("/v1/export/1/{}", f.shard),
    ] {
        let (st, h, b) = get(&f, &uri, &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("serving_check_unavailable")),
            "{uri}: {b}"
        );
        assert!(h.get(x402::PAYMENT_REQUIRED).is_none(), "{uri}: no 402");
    }
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        ..Setup::default()
    });
    let (st, _, _) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
}

/// R-GATE-8 (review RESAPI-R2-2): when the indexer takes a shard out of the
/// catalog (two reads refuted its final view), the gates run again at once,
/// so the later links of a chain judged with it lose their pass; before,
/// they kept it until the next timed run.
#[tokio::test(start_paused = true)]
async fn an_indexer_removal_runs_the_gates_again() {
    let (f, g) = gated(false, false);
    // A chain of two shards: the gate refuses a chain with a missing link.
    g.chain.store(2, Ordering::SeqCst);
    let rows = vec![row("entity:a", b"value a2.......")];
    let root2 = f.cas.put_rows(&rows).expect("rows");
    let m2 = meta(2, 200, b"params two", 1);
    let shard2 = m2.shard;
    f.roots.insert(
        2,
        shard2,
        FinalRoot {
            root: root2,
            finalized_at: 500,
            finalized_unix: 1_100,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m2);
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (2, 2), "{runs:?}");
    let uri = format!("/v1/proofs/2/{shard2}/0");
    assert_eq!(get(&f, &uri, &[]).await.0, StatusCode::OK);
    // The indexer took the first link out; a poll that changed nothing
    // else and is not a 20th poll.
    let m1 = shard_meta(&f, 1, &f.shard);
    f.state
        .catalog
        .write()
        .expect("lock")
        .remove_shard(1, &f.shard);
    let quiet = crate::indexer::PollReport::default();
    assert!(gate::after_poll(&f.state, &quiet, 3).await.is_none());
    let report = crate::indexer::PollReport {
        removed: vec![(1, f.shard)],
        ..Default::default()
    };
    let runs = gate::after_poll(&f.state, &report, 3)
        .await
        .expect("a removal runs the gates");
    assert_eq!(runs[0].cleared, 0, "{runs:?}");
    // The removed link's own answer is gone too (mutation L4).
    assert_eq!(
        gate::cleared(&f.state.gates, &m1, &f.root).err(),
        Some(gate::Refusal::Pending)
    );
    let (st, _, b) = get(&f, &uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_failed")),
        "{b}"
    );
}

/// R-GATE-6 (review RESAPI-R1 F3): a root read that fails during a refresh
/// is not "not final": the shard is judged at its last judged root, so one
/// node error does not refuse a chain.
#[tokio::test(start_paused = true)]
async fn a_failed_root_read_keeps_the_shard_at_its_last_judged_root() {
    let fail = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let f = build(Setup {
        gate: Some(gate_with("test.flow", 1)),
        fail_roots: Some(fail.clone()),
        ..Setup::default()
    });
    gate::refresh(&f.state).await;
    let m = shard_meta(&f, 1, &f.shard);
    assert!(matches!(
        gate::cleared(&f.state.gates, &m, &f.root),
        Ok(Some(_))
    ));
    fail.lock().expect("lock").insert(f.shard);
    let runs = gate::refresh(&f.state).await;
    fail.lock().expect("lock").clear();
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        (runs[0].read_errors, runs[0].judged, runs[0].cleared),
        (1, 1, 1)
    );
}

/// R-GATE-7 (review RESAPI-R1 F3): when a shard never judged cannot be read
/// (its link is missing, so a chain check would refuse everything), the run
/// stores no new verdicts: unchanged shards keep their answers, the new one
/// is pending, and the next good run judges both.
#[tokio::test(start_paused = true)]
async fn a_failed_read_of_a_new_shard_leaves_it_pending_and_keeps_the_rest() {
    let fail = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let g = gate_with("test.flow", 1);
    let f = build(Setup {
        gate: Some(g.clone()),
        fail_roots: Some(fail.clone()),
        ..Setup::default()
    });
    gate::refresh(&f.state).await;
    // A second shard of the chain is listed; its root read fails.
    let rows2 = vec![row("entity:a", b"value a two....")];
    let root2 = f.cas.put_rows(&rows2).expect("rows");
    let m2 = meta(2, 101, b"params two", 1);
    f.roots.insert(
        2,
        m2.shard,
        FinalRoot {
            root: root2,
            finalized_at: 401,
            finalized_unix: 1_001,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m2.clone());
    g.chain.store(2, Ordering::SeqCst);
    fail.lock().expect("lock").insert(m2.shard);
    let runs = gate::refresh(&f.state).await;
    let m1 = shard_meta(&f, 1, &f.shard);
    assert!(
        matches!(gate::cleared(&f.state.gates, &m1, &f.root), Ok(Some(_))),
        "the unchanged shard keeps its clearance"
    );
    assert_eq!(
        gate::cleared(&f.state.gates, &m2, &root2).err(),
        Some(gate::Refusal::Pending)
    );
    assert_eq!((runs[0].read_errors, runs[0].judged), (1, 0), "{runs:?}");
    // The unread shard is counted as skipped, and the kept clearance in
    // the run's note (mutation L4).
    assert_eq!(runs[0].skipped, 1, "{runs:?}");
    let note = runs[0].first_refusal.clone().unwrap_or_default();
    assert!(
        note.starts_with("1 root read(s) failed")
            && note.contains(": 1 previous clearance(s) kept"),
        "{note}"
    );
    fail.lock().expect("lock").clear();
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (2, 2));
    assert!(matches!(
        gate::cleared(&f.state.gates, &m2, &root2),
        Ok(Some(_))
    ));
}

/// Mutation L4: in a run left incomplete by an unread new shard, a judged
/// shard whose root changed, or that left the catalog, loses its old
/// answer (and is not counted as kept).
#[tokio::test(start_paused = true)]
async fn an_incomplete_run_drops_answers_that_no_longer_hold() {
    for removed in [false, true] {
        let fail = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
        let g = gate_with("test.flow", 1);
        let f = build(Setup {
            gate: Some(g.clone()),
            fail_roots: Some(fail.clone()),
            ..Setup::default()
        });
        gate::refresh(&f.state).await;
        let m1 = shard_meta(&f, 1, &f.shard);
        assert!(matches!(
            gate::cleared(&f.state.gates, &m1, &f.root),
            Ok(Some(_))
        ));
        if removed {
            f.state
                .catalog
                .write()
                .expect("lock")
                .remove_shard(1, &f.shard);
        } else {
            let other = f
                .cas
                .put_rows(&[row("entity:a", b"another value..")])
                .expect("rows");
            f.roots.insert(
                1,
                f.shard,
                FinalRoot {
                    root: other,
                    finalized_at: 402,
                    finalized_unix: 1_002,
                    binding: None,
                    tx: None,
                },
            );
        }
        let rows2 = vec![row("entity:a", b"value a two....")];
        let root2 = f.cas.put_rows(&rows2).expect("rows");
        let m2 = meta(2, 101, b"params two", 1);
        f.roots.insert(
            2,
            m2.shard,
            FinalRoot {
                root: root2,
                finalized_at: 401,
                finalized_unix: 1_001,
                binding: None,
                tx: None,
            },
        );
        f.state.catalog.write().expect("lock").add_shard(m2.clone());
        fail.lock().expect("lock").insert(m2.shard);
        let runs = gate::refresh(&f.state).await;
        assert_eq!(runs[0].judged, 0, "{runs:?}");
        let note = runs[0].first_refusal.clone().unwrap_or_default();
        assert!(note.contains(": 0 previous clearance(s) kept"), "{note}");
        assert_eq!(
            gate::cleared(&f.state.gates, &m1, &f.root).err(),
            Some(gate::Refusal::Pending),
            "removed: {removed}"
        );
    }
}

/// Mutation L4: a final shard whose rows are not in the store is skipped
/// (counted, never judged or cleared).
#[tokio::test(start_paused = true)]
async fn a_final_shard_without_its_rows_is_skipped() {
    let f = build(Setup {
        gate: Some(gate_with("test.flow", 0)),
        ..Setup::default()
    });
    let rows2 = vec![row("entity:a", b"value a two....")];
    let root2 = onyx_merkle::MerkleTree::from_rows(&rows2).root();
    let m2 = meta(2, 101, b"params two", 1);
    f.roots.insert(
        2,
        m2.shard,
        FinalRoot {
            root: root2,
            finalized_at: 401,
            finalized_unix: 1_001,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m2.clone());
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].skipped), (1, 1), "{runs:?}");
    assert_eq!(
        gate::cleared(&f.state.gates, &m2, &root2).err(),
        Some(gate::Refusal::Pending)
    );
}

/// Mutation L4: a gate's name and its metric are each registered once.
#[test]
fn a_gate_name_or_metric_is_registered_once() {
    let mut g = gate::Gates::default();
    assert!(g.is_empty());
    g.add(gate_with("test.flow", 0)).expect("first");
    assert!(!g.is_empty());
    // Same name ("test-gate"), another metric.
    assert!(g.add(gate_with("test.other", 0)).is_err());
    // Same metric under another name.
    struct Renamed(Arc<TestGate>);
    impl gate::ServingGate for Renamed {
        fn name(&self) -> &str {
            "another-gate"
        }
        fn metric(&self) -> &str {
            self.0.metric
        }
        fn claims(&self, meta: &ShardMeta) -> bool {
            self.0.claims(meta)
        }
        fn judge(&self, shards: &[gate::GateShard]) -> Vec<gate::Judgement> {
            self.0.judge(shards)
        }
    }
    assert!(g.add(Arc::new(Renamed(gate_with("test.flow", 0)))).is_err());
    g.add(Arc::new(Renamed(gate_with("test.third", 0))))
        .expect("new name and metric");
}

/// R-GATE-8 (review RESAPI-R1 F3): a listed shard that finalizes after the
/// start is judged by the periodic refresh, without a restart or an indexer.
#[tokio::test(start_paused = true)]
async fn a_listed_shard_that_finalizes_later_is_judged_without_a_restart() {
    let f = build(Setup {
        gate: Some(gate_with("test.flow", 0)),
        unfinal: true,
        ..Setup::default()
    });
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].skipped), (0, 1));
    let task = gate::spawn_refresh(f.state.clone(), Duration::from_millis(10), |_| {});
    f.roots.insert(
        1,
        f.shard,
        FinalRoot {
            root: f.root,
            finalized_at: 400,
            finalized_unix: 1_000,
            binding: None,
            tx: None,
        },
    );
    let uri = "/v1/metrics/test.flow?subject=entity:a";
    let mut st = StatusCode::SERVICE_UNAVAILABLE;
    for _ in 0..200 {
        st = get(&f, uri, &[]).await.0;
        if st != StatusCode::SERVICE_UNAVAILABLE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();
    assert_eq!(st, StatusCode::OK);
}
