//! Step 5b and step 8 (ADR-0034 Amendment 1, THREATS RES-7 and RES-1), in
//! every mode: one `btc-entity-reserve` shard on the last 8 blocks before the
//! committed scan (971,192-971,199, from the reserve tail files), settled on
//! chain like round 1 right after it (5b, so one finality wait covers both
//! roots), then found by the chain indexer, cleared by the serving check and
//! served as `btc.entity-reserve` (8): unpaid 402, paid 200 (local: the mock
//! facilitator; fork and testnet: x402 self-settled into `OnyxRevenue` and
//! booked to the shard) with a proof that verifies against the client's own
//! `getFinalizedRoot`, the contract's `verifyRow` and the registry's
//! `verifyRowLeveled` (level 4). Binance's row is refused (not independently
//! cross-checked). A spec on a forged base reruns (miner and validators would
//! agree) and the serving check refuses it.

use super::*;
use onyx_api::gate::{refresh, GateShard, ServingGate};
use onyx_reserve_schema::{EntityReserveRow, ReserveParams};
use onyx_reserve_serve::{ReserveGate, GATE, METRIC as RESERVE_METRIC};
use onyx_source_btc::reserve::{CrossChecks, HeaderChain, ScanSnapshot};
use onyx_source_btc::{fixture_registry_reserve, RESERVE_BASE_LEVEL, SCHEMA_ENTITY_RESERVE};

pub const R3: u64 = 3;
/// The served chain starts on the state after this block.
const START: u64 = 971_191;
const FROM: u64 = 971_192;
const TO: u64 = 971_199;
const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";
const HEADERS: &str = "reserves/headers-969839-971207.json";
/// 1,000 BTC: what the forged spec adds to OKX's base.
const FORGED_EXTRA: u64 = 100_000_000_000;

/// Inputs of the reserve round: the shard, the scan it lands on, the label
/// set and the committed cross-checks.
pub struct Reserve {
    d: Data,
    params: ReserveParams,
    scan: ScanSnapshot,
    set: onyx_labels::LabelSet,
    cross: CrossChecks,
    headers: HeaderChain,
}

fn fixture<T: serde::de::DeserializeOwned>(fx: &Path, rel: &str) -> Res<T> {
    onyx_source_btc::verify_checksum(fx, rel)?;
    Ok(serde_json::from_slice(&std::fs::read(fx.join(rel))?)?)
}

/// The shard's spec on the honest base: the state of the committed reserve
/// history after block 971,191 (`apps/web/static/history/reserves-week.json`,
/// its last shard's base), anchored on the committed scan (`base_ref` = the
/// scan's hash). The serving check derives that base itself from the scan
/// and the finalized rows; it never reads this one.
pub fn prepare(dir: &Path) -> Res<Option<Reserve>> {
    if !dir.join(format!("{TO}.raw.hex")).exists() || !dir.join(format!("{FROM}.raw.hex")).exists()
    {
        return Ok(None);
    }
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fx = root.join("fixtures");
    let set = load_label_set(&fx, LABELS_V2)?;
    let scan: ScanSnapshot = fixture(&fx, "reserves/scan-971199.json")?;
    let cross: CrossChecks = fixture(&fx, "reserves/cross-checks.json")?;
    scan.check_labels(&set)?;
    // RES-4 (ADR-0037): the header chain that gives the scan's block its
    // confirmations (proof of work checked again here).
    onyx_source_btc::verify_checksum(&fx, HEADERS)?;
    let headers = HeaderChain::from_json(
        &std::fs::read_to_string(fx.join(HEADERS))?,
        onyx_source_btc::node::MAINNET_MIN_BITS,
    )?;
    // RES-4: the tail's last block is the scan's block (proof of work checked).
    let raw = std::fs::read_to_string(dir.join(format!("{TO}.raw.hex")))?;
    let (hash, _) = onyx_source_btc::node::check_raw_header(
        &header_bytes(raw.trim())?,
        onyx_source_btc::node::MAINNET_MIN_BITS,
    )?;
    let mut display = hash;
    display.reverse();
    scan.check_block(&hex::encode(display))?;
    let export: Value = serde_json::from_slice(&std::fs::read(
        root.join("apps/web/static/history/reserves-week.json"),
    )?)?;
    let last = export["shards"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["from"] == FROM && s["to"] == TO))
        .ok_or("the committed reserve history has no shard 971,192-971,199")?;
    let base = last["rows"]
        .as_array()
        .ok_or("rows")?
        .iter()
        .map(|r| {
            if r["base_height"].as_u64() != Some(START) {
                return Err("base height".into());
            }
            Ok(EntityBase {
                entity: r["entity"].as_str().ok_or("entity")?.into(),
                balance_sats: r["base_sats"].as_str().ok_or("base")?.parse()?,
            })
        })
        .collect::<Res<Vec<_>>>()?;
    let params = ReserveParams {
        base_height: START,
        base_ref: scan.hash().0,
        base,
    };
    let pin = NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: Some((TO, scan.block_hash.clone())),
    };
    let (source, snapshot) = load_node_range(dir, FROM, TO, &pin)?;
    let reg = fixture_registry_reserve();
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_reserve.wasm"))?;
    let cas = Arc::new(MemCas::new());
    cas.put(Kind::Recipe, &wasm)?;
    cas.put_label_set(&set)?;
    let spec = reserve_spec(&reg, &wasm, &set, snapshot, &params)?;
    // The serving check reads the params by the hash the shard id commits to.
    cas.put(Kind::Blob, &spec.params)?;
    Ok(Some(Reserve {
        d: Data {
            metric: RESERVE_METRIC,
            floor: RESERVE_BASE_LEVEL.number(),
            reg,
            cas,
            host: Host::new()?,
            spec,
            source,
            blocks: TO - FROM + 1,
            label: format!("reserve tail blocks {FROM}-{TO}, label set v2"),
            end_header: header_bytes(raw.trim())?,
        },
        params,
        scan,
        set,
        cross,
        headers,
    }))
}

fn reserve_spec(
    reg: &onyx_core::StaticRegistry,
    wasm: &[u8],
    set: &onyx_labels::LabelSet,
    snapshot: onyx_core::Snapshot,
    params: &ReserveParams,
) -> Res<ShardSpec> {
    let spec = ShardSpec {
        recipe: Some(keccak256(wasm)),
        runtime: Some(RuntimeProfile {
            abi_version: 1,
            host: HOST_ID.into(),
            fuel_limit: 4_000_000_000,
            max_memory_bytes: 256 << 20,
            max_output_bytes: 1 << 20,
            max_call_depth: 512,
        }),
        label_set: Some(LabelSetRef {
            name: set.name.clone(),
            version: set.version,
            content_hash: set.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(SCHEMA_ENTITY_RESERVE),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("source not registered")?
            .hash(),
        params: borsh::to_vec(params)?,
        registry_root: reg.root(),
    };
    spec.validate(reg)?;
    Ok(spec)
}

fn gate_shard(round: u64, spec: &ShardSpec, root: Hash32, leaves: Vec<Vec<u8>>) -> GateShard {
    let b = onyx_api::catalog::ShardBinding::from_spec(spec);
    GateShard {
        round,
        shard: spec.shard_id(),
        root,
        source: spec.source.clone(),
        recipe_id: b.recipe_id,
        label_set_hash: b.label_set_hash,
        schema_id: b.schema_id,
        params: Some(spec.params.clone()),
        leaves,
    }
}

/// `--export-node`: add the reserve shard to the exported catalog (held,
/// with its serving check and base level) and its rows and params to the
/// exported row store, so `onyx-node-btc` serves it and plain `onyx-node`
/// refuses it (`KNOWN_GATES`; scripts/docker-smoke.sh). Round 1's export
/// (step 6) wrote the catalog first.
fn export_node(
    dir: &Path,
    d: &Data,
    rights: &onyx_core::DataRights,
    leaf_count: u64,
    root: &Hash32,
) -> Res<()> {
    use onyx_node::catalog_file::{CatalogFile, HeldEntry, Pricing};
    let path = dir.join("catalog.json");
    let mut file = CatalogFile::from_json(&std::fs::read_to_string(&path)?)?;
    let mut e = HeldEntry::new(
        RESERVE_METRIC,
        &d.spec,
        rights,
        Some(leaf_count),
        Some(&d.end_header),
    )?;
    e.input_level = Some(RESERVE_BASE_LEVEL.number());
    e.serving_check = Some(GATE.into());
    file.held.push(e);
    file.pricing.insert(
        RESERVE_METRIC.into(),
        Pricing {
            base_price_micro_usdc: PRICE_MICRO_USDC,
            fresh_window_secs: 86_400,
        },
    );
    file.to_catalog()?;
    if file.holdings_at(now_unix())?.len() != 2 {
        return Err("the exported catalog does not hold both shards".into());
    }
    std::fs::write(&path, serde_json::to_string_pretty(&file)? + "\n")?;
    let rows = d
        .cas
        .get(Kind::Rows, root)
        .ok_or("the reserve row batch is not in the store")?;
    let store = onyx_coordinator::cas::FsCas::new(dir.join("cas"));
    store.put(Kind::Rows, &rows)?;
    for p in file.params_blobs()? {
        store.put(Kind::Blob, &p)?;
    }
    println!("  exported the reserve shard to {}", dir.display());
    Ok(())
}

/// What the reserve round left on chain, for the serve step.
pub struct Settled {
    pub shard: Hash32,
    root: Hash32,
    leaf_count: u64,
}

/// Step 5b: settle the reserve round on chain like round 1 (open, commit and
/// reveal, validators rerun, finalize, the challenge window, release) and,
/// with payments (fork, testnet), register it in `OnyxRevenue` so a sale is
/// credited to it. Runs right after round 1, so one wait for Base Sepolia's
/// finalized block covers both roots (`play_all`).
pub fn settle(net: &mut Net, r: &Reserve, paid: Option<&Paid>) -> Res<(Vec<String>, Settled)> {
    let d = &r.d;
    let (coord, st) = (net.coord, net.st());
    // Round 1 spent part of the reward pool.
    net.chain.send(
        &coord,
        &st,
        "fundRewards(uint256)",
        &[(10 * USDC).to_string()],
    )?;
    let (works, recipe_secs) = mine(&net.miners, R3, d)?;
    net.mark("r3.mine")?;
    let root = works[0].commitment.root;
    let leaf_count = works[0].commitment.leaf_count;
    let p = net.play(R3, d, &works, recipe_secs)?;
    let status = net.status(R3, &p.shard)?;
    if status != FINALIZED {
        return Err(format!(
            "reserve round: status {status}, not Finalized (3); 2 = Pending (too few standbys this time: run again)"
        )
        .into());
    }
    let fin = net.chain.block()?;
    net.chain.reach(fin + p.cfg.dispute.challenge_window)?;
    let disputes = net.dep.disputes;
    net.chain.send(
        &coord,
        &disputes,
        "release(uint64,bytes32)",
        &[R3.to_string(), hx(&p.shard)],
    )?;
    net.mark("r3.challenge_window")?;
    let agrees = p
        .verdicts
        .iter()
        .filter(|(_, _, v)| matches!(v, Verdict::Agree))
        .count();
    let mut out = vec![format!(
        "5b. reserve round {R3} ({}): btc-entity-reserve on the state after block {START} (base_ref = the committed scan's hash); {MINERS} miners ran it ({recipe_secs:.1} s each), {agrees}/{} verdicts Agree; finalized at block {fin}, root {}...; released after the {}-block challenge window",
        d.label,
        p.verdicts.len(),
        &hx(&root)[..10],
        p.cfg.dispute.challenge_window
    )];
    if let Some(pd) = paid {
        let f = works[0].commitment.fields();
        net.chain.send(
            &coord,
            &pd.revenue,
            "register(uint64,bytes32,(bytes32,uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32))",
            &[
                R3.to_string(),
                hx(&p.shard),
                format!(
                    "({},{},{},{},{},{},{},{})",
                    hx(&f.root),
                    f.leaf_count,
                    hx(&f.recipe_id),
                    hx(&f.runtime_hash),
                    hx(&f.label_set_hash),
                    hx(&f.source_hash),
                    hx(&f.schema_id),
                    hx(&f.rights_hash)
                ),
            ],
        )?;
        out.push(
            "   OnyxRevenue: the reserve shard is registered (its sales are credited to it)".into(),
        );
    }
    Ok((
        out,
        Settled {
            shard: p.shard,
            root,
            leaf_count,
        },
    ))
}

/// Step 8: index, clear, serve and sell the settled reserve shard (see the
/// module doc). Local mode pays through the mock facilitator; fork and
/// testnet self-settle the x402 payment into `OnyxRevenue` with the API's
/// own facilitator (`settle`, shared with round 1's API: one nonce sequence
/// for the operator key). Returns the report lines and the JSON report.
pub async fn serve(
    net: &mut Net,
    o: &Opts,
    r: Reserve,
    s: Settled,
    paid: Option<&Paid>,
    settle: Option<&Arc<SelfSettle<HttpJsonRpc<Post>>>>,
) -> Res<(Vec<String>, Value)> {
    let Reserve {
        d,
        params,
        scan,
        set,
        cross,
        headers,
    } = r;
    let Settled {
        shard,
        root,
        leaf_count,
    } = s;
    let mut out = Vec::new();
    let st = net.st();
    // Normally at once: the wait after round 1 covered this root.
    wait_finalized(net, o, R3, &shard).await?;

    // The chain indexer admits it (ADR-0033), floored at the registry's level.
    let chain_id = net.chain.chain_id;
    let rpc = || HttpJsonRpc::new(o.rpc.clone(), node_post(o.mode));
    let pin = parse_h32(&net.chain.run(&["codehash".into(), ax(&st)])?)?;
    let roots = Arc::new(ChainRoots::connect(st, chain_id, Some(pin), rpc()).await?);
    let rights = d
        .reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .ok_or("source not registered")?
        .clone();
    if let Some(dir) = &o.export_node {
        export_node(dir, &d, &rights, leaf_count, &root)?;
    }
    let fixtures = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"));
    let label_level = label_set_proof_level(&fixtures, d.spec.label_set.as_ref().ok_or("labels")?);
    let mut meta = ShardMeta::from_spec(0, &d.spec, RESERVE_METRIC, rights)
        .ok_or("rights do not match the spec")?
        .with_label_set_level(label_level)
        .with_input_level(RESERVE_BASE_LEVEL)
        .with_serving_check(GATE);
    meta.leaf_count = Some(leaf_count);
    let mut held = Holdings::new();
    held.insert(HeldShard {
        meta,
        label_set: d.spec.label_set.clone(),
    })?;
    let mock_pay_to = parse_addr(DEMO_PAY_TO)?;
    let sd = o.deploy_dir(paid.map_or(&mock_pay_to, |p| &p.revenue));
    std::fs::create_dir_all(&sd)?;
    let path = sd.join("indexer-reserve.json");
    let _ = std::fs::remove_file(&path);
    let idx = Indexer::open(
        IndexerConfig {
            allow_test_labels: true,
            registry: net.dep.registry,
            ..IndexerConfig::new(chain_id, st, net.dep.settlement_block)
        },
        HttpJsonRpc::new(o.rpc.clone(), logs_post(o.mode)),
        held,
        d.cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(path),
    )?;
    let mut catalog = Catalog::new();
    catalog.set_pricing(
        RESERVE_METRIC,
        PricingPolicy {
            base_price_micro_usdc: PRICE_MICRO_USDC,
            fresh_window_secs: 86_400,
        },
    );
    let payer_key = match paid {
        Some(p) => p.payer_key,
        None => keccak256(DEMO_PAYER_SEED.as_bytes()).0,
    };
    let payer = ax(&exact_evm::address(&payer_key).ok_or("payer key")?);
    let recipe = d.spec.recipe.ok_or("recipe")?;
    let gate = Arc::new(ReserveGate::new(
        scan.clone(),
        set.clone(),
        START,
        recipe,
        cross,
        Vec::new(),
        headers,
    )?);
    // Local: a mock facilitator moves nothing. Fork/testnet: round 1's
    // self-settling facilitator settles into OnyxRevenue and credits this
    // shard in the same transaction (ADR-0026).
    let (pay_to, facilitator): (String, Arc<dyn Facilitator>) = match (paid, settle) {
        (None, _) => (
            DEMO_PAY_TO.to_string(),
            Arc::new(MockFacilitator::new([payer])),
        ),
        (Some(p), Some(f)) => (ax(&p.revenue), f.clone() as Arc<dyn Facilitator>),
        (Some(_), None) => return Err("the paid reserve sale needs round 1's self-settle".into()),
    };
    let mut state = ApiState::new(
        catalog,
        roots,
        d.cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(Payments {
            terms: demo_terms(pay_to),
            facilitator,
        }),
        Arc::new(now_unix),
        format!("http://127.0.0.1:{}", o.port + 2),
        ApiConfig {
            allow_test_labels: true,
            ..ApiConfig::default()
        },
    )?
    .with_gate(gate.clone())?;
    if let Some(f) = settle {
        state.receipts = f.receipts();
    }
    // A public node can fail a read; the indexer is asked again (as round
    // 1's), every 10 s on a live network, for up to 10 minutes.
    let t = Instant::now();
    loop {
        match idx.poll(&state.catalog).await {
            Ok(r) if r.admitted.contains(&(R3, shard)) => break,
            Ok(_) => {}
            Err(e) if t.elapsed() < Duration::from_secs(600) => {
                println!("   reserve indexer: {e}; trying again");
            }
            Err(e) => return Err(format!("reserve indexer: {e}").into()),
        }
        if t.elapsed() > Duration::from_secs(600) {
            return Err(format!(
                "the indexer did not admit the reserve shard: {:?}",
                idx.known()
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(if net.chain.anvil {
            200
        } else {
            10_000
        }))
        .await;
    }
    let admitted = state
        .catalog
        .read()
        .map_err(|_| "catalog lock")?
        .shard(R3, &shard)
        .cloned()
        .ok_or("not in the catalog")?;
    if admitted.proof_level != ProofLevel::BondedClaim {
        return Err(format!(
            "reserve shard at level {}, want 4",
            admitted.proof_level.number()
        )
        .into());
    }
    // The check reads the shard's root on chain; a failed read leaves it
    // pending (RESAPI-R1 F3), so it runs again a few times.
    let mut tries = 0;
    loop {
        tries += 1;
        let runs = refresh(&state).await;
        let run = runs.first().ok_or("no serving check ran")?;
        if (run.judged, run.cleared) == (1, 1) {
            break;
        }
        if tries >= 10 {
            return Err(format!("serving check: {run:?}").into());
        }
        tokio::time::sleep(Duration::from_secs(if net.chain.anvil { 1 } else { 10 })).await;
    }
    out.push(format!(
        "8. reserve shard (round {R3}) indexed: Finalized(round {R3}) found, held rows hash to the chain's root, admitted at level 4; serving check {GATE}: the chain from block {START} lands on the committed scan at {} (base derived from the scan and the finalized flows), shard cleared",
        scan.height
    ));

    // Serve it and check it as a client.
    let port = o.port + 2;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let state = Arc::new(state);
    let server = tokio::spawn({
        let state = state.clone();
        async move {
            axum::serve(
                listener,
                router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        }
    });
    let base_url = format!("http://127.0.0.1:{port}");
    let get = |subject: &str, headers: Vec<(&'static str, String)>| {
        let uri = format!("{base_url}/v1/metrics/{RESERVE_METRIC}?subject={subject}");
        tokio::task::spawn_blocking(move || http("GET", &uri, &headers, &[]))
    };
    let header = |hs: &[(String, String)], name: &str| {
        hs.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let result: Res<Value> = async {
        let (status, hs, _) = get("entity:okx", Vec::new()).await??;
        if status != 402 {
            return Err(format!("unpaid reserve request answered {status}, want 402").into());
        }
        let pr: PaymentRequired =
            decode_header(&header(&hs, PAYMENT_REQUIRED).ok_or("no PAYMENT-REQUIRED")?)?;
        // Fork/testnet: the client pins payTo from the deployment manifest
        // (REV-4, code hash checked on its own node) before signing.
        let accepted = match paid {
            Some(_) => {
                let m: Value =
                    serde_json::from_str(&std::fs::read_to_string(sd.join("deployment.json"))?)?;
                PayToPin::from_manifest(&m)?
                    .check_code(&rpc())
                    .await?
                    .select(&pr)?
                    .clone()
            }
            None => pr.accepts.first().ok_or("no payment option")?.clone(),
        };
        let auth = exact_evm::Authorization {
            from: exact_evm::address(&payer_key).ok_or("payer key")?,
            to: parse_addr(&accepted.pay_to)?,
            value: accepted.amount.parse()?,
            valid_after: 0,
            valid_before: now_unix() + 300,
            nonce: keccak256(format!("onyx demo reserve {} {}", ax(&st), now_unix()).as_bytes()).0,
        };
        let payment = encode_header(&PaymentPayload {
            x402_version: 2,
            resource: Some(pr.resource),
            payload: exact_evm::sign(&payer_key, &auth, &accepted).ok_or("signature")?,
            accepted: accepted.clone(),
        });
        let (status, hs, body) = get("entity:okx", vec![(PAYMENT_SIGNATURE, payment)]).await??;
        if status != 200 {
            return Err(format!(
                "paid reserve request answered {status}: {}",
                String::from_utf8_lossy(&body)
            )
            .into());
        }
        let settled: SettleResponse =
            decode_header(&header(&hs, PAYMENT_RESPONSE).ok_or("no PAYMENT-RESPONSE")?)?;
        if !settled.success {
            return Err("the reserve payment did not settle".into());
        }
        let b: Value = serde_json::from_slice(&body)?;
        // The default client check: the root read on the client's own node.
        let verified = ClientNode::new(rpc(), chain_id, st)
            .verify(
                &b,
                &Expect::Metric {
                    metric: RESERVE_METRIC,
                    def: MetricDef {
                        source: SOURCE_ID,
                        chain: Some(CHAIN_ID),
                        window: Window::Blocks(d.blocks),
                        recipe_id: recipe,
                        runtime_hash: d.spec.runtime.as_ref().ok_or("runtime")?.hash(),
                        schema_id: d.spec.schema,
                        params_hash: keccak256(&d.spec.params),
                        label_set: d.spec.label_set.as_ref().ok_or("labels")?.hash(),
                        registry_root: d.spec.registry_root,
                    },
                    subject: "entity:okx",
                    as_of: None,
                    min_as_of: 0,
                },
            )
            .await?;
        let claim = verified.require_cross_checked()?.clone();
        if !verified.chain_checked || verified.root != root {
            return Err("reserve client check incomplete".into());
        }
        let pr = &b["proof"];
        let siblings: Vec<String> = pr["siblings"]
            .as_array()
            .ok_or("siblings")?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        let args = vec![
            R3.to_string(),
            hx(&shard),
            b["row"].as_str().ok_or("row")?.to_string(),
            pr["index"].as_u64().ok_or("index")?.to_string(),
            pr["leafCount"].as_u64().ok_or("leafCount")?.to_string(),
            format!("[{}]", siblings.join(",")),
        ];
        let on_chain = net.chain.call(
            &st,
            "verifyRow(uint64,bytes32,bytes,uint64,uint64,bytes32[])(bool)",
            &args,
        )?;
        if on_chain != "true" {
            return Err("verifyRow refused the reserve row".into());
        }
        let reg = net.dep.registry.ok_or("no registry")?;
        let lv = net.chain.call(
            &reg,
            "verifyRowLeveled(uint64,bytes32,bytes,uint64,uint64,bytes32[])(bool,uint8,bytes32)",
            &args,
        )?;
        let want_metric = hx(&metric_key(&MetricId(RESERVE_METRIC.into())));
        let mut it = lv.lines().map(str::trim);
        if (it.next(), it.next(), it.next())
            != (Some("true"), Some("4"), Some(want_metric.as_str()))
        {
            return Err(
                format!("verifyRowLeveled gave {lv:?}, want true, 4, {want_metric}").into(),
            );
        }
        let row: OutputRow = borsh::from_slice(&hex_bytes(&b["row"])?)?;
        let okx: EntityReserveRow = borsh::from_slice(&row.value)?;
        let at_scan = scan
            .entity_totals(&set)?
            .into_iter()
            .find(|t| t.entity == "entity:okx")
            .map(|t| t.balance_sats)
            .ok_or("okx total")?;
        if okx.balance_sats != at_scan || okx.height != TO {
            return Err(format!(
                "OKX row {} at {}, the scan holds {at_scan}",
                okx.balance_sats, okx.height
            )
            .into());
        }
        // Fork/testnet: the payment landed in OnyxRevenue and was booked to
        // this shard in the settle transaction itself. The API sent it, so a
        // lagging public node may not show it yet: read again for a minute.
        if let Some(p) = paid {
            let price: u128 = accepted.amount.parse()?;
            let mut tries = 0;
            let booked = loop {
                tries += 1;
                let book = net.chain.call(
                    &p.revenue,
                    "books(uint64,bytes32)(bool,uint32,uint32,uint64,bytes32,uint128,uint128,uint128)",
                    &[R3.to_string(), hx(&shard)],
                )?;
                let booked = first_u128(book.lines().nth(7).unwrap_or(""))?;
                if booked == price || net.chain.anvil || tries >= 15 {
                    break booked;
                }
                tokio::time::sleep(Duration::from_secs(4)).await;
            };
            if booked != price {
                return Err(format!(
                    "OnyxRevenue booked {booked} micro-USDC to the reserve shard; want {price}"
                )
                .into());
            }
        }
        // Binance: no independent cross-check, refused before any 402.
        let (status, hs, body) = get("entity:binance", Vec::new()).await??;
        let e: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if status != 403
            || e["error"]["code"] != "not_cross_checked"
            || header(&hs, PAYMENT_REQUIRED).is_some()
        {
            return Err(
                format!("Binance answered {status} {e}, want 403 not_cross_checked").into(),
            );
        }
        Ok(json!({
            "round": R3,
            "shard": hx(&shard),
            "root": hx(&root),
            "blocks": format!("{FROM}-{TO}"),
            "unpaid_status": 402,
            "paid_status": 200,
            "price_micro_usdc": accepted.amount,
            "facilitator": if paid.is_some() { "self-settled into OnyxRevenue" } else { "local mock (no token moves)" },
            "settle_tx": settled.transaction,
            "client_checked_chain_root": verified.chain_checked,
            "contract_verify_row": true,
            "registry_level": 4,
            "okx_balance_sats": okx.balance_sats.to_string(),
            "okx_cross_check": claim.bases.first().map(|(b, _)| b.clone()),
            "binance_status": 403,
        }))
    }
    .await;
    server.abort();
    let mut report = result?;
    net.mark("r3.serve_pay_verify")?;
    out.push(format!(
        "   API on {base_url}: OKX unpaid -> 402; paid {} micro-USDC with x402 ({}, settled: true) -> 200, {} BTC at block {TO} (= the scan's OKX total), crossCheck {}; the client checked it against its own getFinalizedRoot, verifyRow true, verifyRowLeveled true at level 4",
        report["price_micro_usdc"].as_str().unwrap_or("?"),
        if paid.is_some() { "self-settled into OnyxRevenue" } else { "local mock facilitator" },
        btc(report["okx_balance_sats"].as_str().unwrap_or("0").parse()?),
        report["okx_cross_check"].as_str().unwrap_or("?")
    ));
    if paid.is_some() {
        out.push(format!(
            "   reserve settle tx {}: OnyxRevenue holds the USDC and booked {} micro-USDC to the reserve shard in the same transaction",
            report["settle_tx"].as_str().unwrap_or("?"),
            report["price_micro_usdc"].as_str().unwrap_or("?")
        ));
    }
    out.push("   Binance -> 403 not_cross_checked before any 402 (its base is one node's scan; no independent check, THREATS RES-1)".into());

    // A spec on a forged base: it reruns, and the serving check refuses it.
    let mut fp = params.clone();
    for b in fp.base.iter_mut().filter(|b| b.entity == "entity:okx") {
        b.balance_sats += FORGED_EXTRA;
    }
    let wasm = d.cas.get(Kind::Recipe, &recipe).ok_or("recipe")?;
    let fspec = reserve_spec(&d.reg, &wasm, &set, d.spec.source.snapshot.clone(), &fp)?;
    let fw = net.miners[0].work(R3 + 1, &fspec, d.source.clone(), &d.reg, &*d.cas, &d.host)?;
    let fw2 = net.miners[1].work(R3 + 1, &fspec, d.source.clone(), &d.reg, &*d.cas, &d.host)?;
    if fw.commitment.root != fw2.commitment.root {
        return Err("two runs of the forged spec disagree".into());
    }
    let honest_leaves = d.cas.rows(&root).ok_or("honest rows")?;
    let v = gate.judge(&[
        gate_shard(R3, &d.spec, root, honest_leaves),
        gate_shard(R3 + 1, &fspec, fw.commitment.root, fw.rows.clone()),
    ]);
    let why = match (&v[0], &v[1]) {
        (Ok(_), Err(why)) => why.clone(),
        _ => return Err(format!("serving check on honest + forged: {v:?}").into()),
    };
    out.push(format!(
        "   forged base (OKX +{} BTC in the spec's params): both miners' reruns agree on root {}..., and the serving check refuses it: {why}",
        btc(i128::from(FORGED_EXTRA)),
        &hx(&fw.commitment.root)[..10]
    ));
    report["forged_root"] = json!(hx(&fw.commitment.root));
    report["forged_refused"] = json!(why);
    Ok((out, report))
}
