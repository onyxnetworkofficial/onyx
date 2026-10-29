//! ROADMAP C7 on a local anvil (ADR-0036 amendment 3): a track commitment
//! resolved only from `OnyxSignedBook` facts and published in the book's
//! append-only commitment log, then checked by a client from its own reads of
//! the chain, walking the log from its first entry. An affiliate's first
//! answer leaves vendor A self-resolved until an independent answer judges
//! it (the self-shield closed). Then the attacks: an omitted and a forged resolution, a hidden
//! affiliation, a late operator declaration, a rollback to an older
//! commitment, a fork after a known one, another poster's spam, a track
//! key's level-3 status read at a post's block (C9, TR-R4-4), and a reorg
//! under a pinned head.
//!
//! Run by `scripts/track-onchain-anvil.sh` (which starts the anvil and builds
//! the contract): `cargo run -p onyx-coordinator --example track-onchain-anvil
//! -- --rpc URL --cast PATH --artifact contracts/out/OnyxSignedBook.sol/OnyxSignedBook.json`.
//! Local only: chain id 31337 on a loopback URL, anvil's public unlocked dev
//! accounts; the sources' signing keys are derived from fixed labels at run
//! time (test keys, never real ones).

use onyx_coordinator::cast_chain::{is_loopback_rpc, Finality};
use onyx_coordinator::track_chain::{parse_block, CastFacts, CommitmentLogCast};
use onyx_core::{
    keccak256, Confidence, DataRights, Hash32, MetricId, MetricInfo, SignedPayload, SignedReport,
    SourceId, StaticRegistry, Subject, Value, ValueKind,
};
use onyx_labels::attest::{eip191_sign_with, secret_address};
use onyx_track::chain::{
    check_in_log, check_latest, check_operators, derive_resolutions, verify_chain,
    verify_resolutions, ChainError, ChainStep, ChainView, CommitmentLog, FactChain, Preimages,
    Prev,
};
use onyx_track::intake::{KeyBook, KeyLife, KeyLives, TrackKey};
use onyx_track::wire::{check_record_body, record_body, Expect, Served, WireError};
use onyx_track::{
    compute, sort_statements, Close, Fact, MetricRule, OperatorOf, Outcome, Statement, TrackLog,
    TrackParams,
};
use std::path::PathBuf;
use std::process::Command;

const METRIC: &str = "example.track.amount";
const VENDOR_A: &str = "example.vendor-a";
const VENDOR_B: &str = "example.vendor-b";
const SIGNER_X: &str = "example.signer-x";
/// Same operator as vendor A (declared before its first key).
const AFFILIATE: &str = "example.vendor-a-affiliate";

fn secret(label: &str) -> [u8; 32] {
    keccak256(format!("onyx-track-onchain-anvil-test-key:{label}").as_bytes()).0
}

fn addr(label: &str) -> [u8; 20] {
    secret_address(&secret(label)).expect("test key")
}

fn ax(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn hx(h: &Hash32) -> String {
    format!("0x{}", hex::encode(h.0))
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_aggregate("btc.mainnet"))
    .and_then(|r| r.add_aggregate("eth.mainnet"))
    .expect("registry");
    for s in [VENDOR_A, VENDOR_B, SIGNER_X, AFFILIATE] {
        r.add_source(DataRights::public_chain(SourceId(s.into())))
            .expect("source");
    }
    r
}

fn key_id(source: &str, signer: &[u8; 20]) -> Hash32 {
    onyx_core::SourceKey {
        source: SourceId(source.into()),
        signer: *signer,
        provenance: onyx_core::KeyProvenance::WellKnown {
            origin: "x.example.org".into(),
            document_hash: keccak256(b"doc"),
            fetched_unix: 1,
        },
    }
    .id()
}

fn report(source: &str, f: &Fact, value: u128, seq: u64) -> SignedReport {
    let payload = SignedPayload {
        source: SourceId(source.into()),
        key: key_id(source, &addr(source)),
        metric: f.metric.clone(),
        subject: f.subject.clone(),
        value: Value::UInt(value),
        observed_unix: f.as_of,
        sequence: seq,
    };
    let signature = eip191_sign_with(&secret(source), payload.message().as_bytes()).expect("sign");
    SignedReport { payload, signature }
}

fn st(source: &str, f: &Fact, value: u128, at: u64) -> Statement {
    Statement {
        source: SourceId(source.into()),
        fact: f.clone(),
        value: Value::UInt(value),
        confidence: Confidence(900_000),
        committed_at: at,
    }
}

struct Node {
    cast: PathBuf,
    rpc: String,
    work: PathBuf,
}

impl Node {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let out = Command::new(&self.cast)
            .current_dir(&self.work)
            .args(args)
            .env("ETH_RPC_URL", &self.rpc)
            .env_remove("ONYX_DEPLOYER_KEY")
            .env_remove("BASE_SEPOLIA_PRIVATE_KEY")
            .output()
            .map_err(|e| format!("cast: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cast {}: {} {}",
                args.first().map_or("", String::as_str),
                String::from_utf8_lossy(&out.stderr).trim(),
                String::from_utf8_lossy(&out.stdout).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn rpc(&self, method: &str, params: &[&str]) -> Result<serde_json::Value, String> {
        let mut a = vec!["rpc".to_string(), method.to_string()];
        a.extend(params.iter().map(|p| (*p).to_string()));
        let out = self.run(&a)?;
        serde_json::from_str(&out).map_err(|e| format!("{method}: {e}: {out}"))
    }

    fn send(
        &self,
        from: &[u8; 20],
        to: &[u8; 20],
        sig: &str,
        args: &[String],
    ) -> Result<(), String> {
        let mut a = vec![
            "send".to_string(),
            "--unlocked".into(),
            "--from".into(),
            ax(from),
            ax(to),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        a.push("--json".into());
        let v: serde_json::Value =
            serde_json::from_str(&self.run(&a)?).map_err(|e| e.to_string())?;
        if v["status"].as_str() != Some("0x1") {
            return Err(format!("{sig} reverted"));
        }
        Ok(())
    }

    fn mine(&self, n: u32) -> Result<(), String> {
        for _ in 0..n {
            self.rpc("evm_mine", &[])?;
        }
        Ok(())
    }

    fn warp(&self, secs: u64) -> Result<(), String> {
        self.rpc("evm_increaseTime", &[&secs.to_string()])?;
        self.mine(1)
    }

    fn block(&self, tag: &str) -> Result<onyx_track::intake::Anchor, String> {
        parse_block(&self.rpc("eth_getBlockByNumber", &[&format!("\"{tag}\""), "false"])?)
    }
}

fn expect_err<T: std::fmt::Debug, E: std::fmt::Debug>(
    what: &str,
    r: Result<T, E>,
    ok: impl Fn(&E) -> bool,
) -> Result<(), String> {
    match r {
        Err(e) if ok(&e) => {
            println!("refused as expected: {what}: {e:?}");
            Ok(())
        }
        other => Err(format!("{what}: expected a refusal, got {other:?}")),
    }
}

fn tlog<'a>(
    params: &'a TrackParams,
    statements: &'a [Statement],
    resolutions: &'a [onyx_track::Resolution],
) -> TrackLog<'a> {
    TrackLog {
        params,
        statements,
        resolutions,
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let rpc = arg(&args, "--rpc").ok_or("--rpc URL")?;
    let cast = PathBuf::from(arg(&args, "--cast").unwrap_or_else(|| "cast".into()));
    let artifact = arg(&args, "--artifact").ok_or("--artifact PATH")?;
    if !is_loopback_rpc(&rpc) {
        return Err("local anvil only: the RPC must be a loopback URL".into());
    }
    let node = Node {
        cast: cast.clone(),
        rpc: rpc.clone(),
        work: std::env::temp_dir(),
    };
    if node.run(&["chain-id".into()])?.trim() != "31337" {
        return Err("not a local anvil".into());
    }
    let accounts: Vec<String> =
        serde_json::from_value(node.rpc("eth_accounts", &[])?).map_err(|e| e.to_string())?;
    let acct = |i: usize| -> Result<[u8; 20], String> {
        let mut a = [0u8; 20];
        hex::decode_to_slice(
            accounts.get(i).ok_or("account")?.trim_start_matches("0x"),
            &mut a,
        )
        .map_err(|e| e.to_string())?;
        Ok(a)
    };
    let (deployer, reviewer, relayer, poster, spammer) =
        (acct(0)?, acct(1)?, acct(2)?, acct(3)?, acct(4)?);

    // Deploy the book with the reviewer.
    let art: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&artifact).map_err(|e| format!("{artifact}: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let bytecode = art["bytecode"]["object"].as_str().ok_or("bytecode")?;
    let ctor = node.run(&[
        "abi-encode".into(),
        "constructor(address)".into(),
        ax(&reviewer),
    ])?;
    let created: serde_json::Value = serde_json::from_str(&node.run(&[
        "send".into(),
        "--unlocked".into(),
        "--from".into(),
        ax(&deployer),
        "--create".into(),
        format!("{bytecode}{}", ctor.trim_start_matches("0x")),
        "--json".into(),
    ])?)
    .map_err(|e| e.to_string())?;
    let mut book = [0u8; 20];
    hex::decode_to_slice(
        created["contractAddress"]
            .as_str()
            .ok_or("no contract address")?
            .trim_start_matches("0x"),
        &mut book,
    )
    .map_err(|e| e.to_string())?;
    println!("book deployed at {}", ax(&book));

    // The reviewer declares vendor A's operator for vendor A and its affiliate
    // (before their first keys), then registers one key per source.
    let op = keccak256(b"example operator of vendor A");
    for s in [VENDOR_A, AFFILIATE] {
        node.send(
            &reviewer,
            &book,
            "declareOperator(string,bytes32,string)",
            &[s.into(), hx(&op), "one operator".into()],
        )?;
    }
    for s in [VENDOR_A, VENDOR_B, SIGNER_X, AFFILIATE] {
        node.send(
            &reviewer,
            &book,
            "registerKey(string,address,string,bytes32,string)",
            &[
                s.into(),
                ax(&addr(s)),
                format!("{}.example.org", s.replace('.', "-")),
                hx(&keccak256(s.as_bytes())),
                "setup".into(),
            ],
        )?;
    }
    // TR-R2-4: a source with a key can no longer be grouped.
    expect_err(
        "an operator declared after the source's first key",
        node.send(
            &reviewer,
            &book,
            "declareOperator(string,bytes32,string)",
            &[VENDOR_B.into(), hx(&op), "late".into()],
        ),
        |e| e.contains("SourceHasKeys"),
    )?;

    // Statements on two facts; level-3 answers on chain: signer X answers F
    // (7: A agrees, B misses), only A's affiliate answers G.
    let now = node.block("latest")?.timestamp;
    let subj = |s: &str| Subject::Aggregate(s.into());
    let f = Fact {
        metric: MetricId(METRIC.into()),
        subject: subj("btc.mainnet"),
        as_of: now - 600,
    };
    let g = Fact {
        metric: MetricId(METRIC.into()),
        subject: subj("eth.mainnet"),
        as_of: now - 600,
    };
    let mut statements = vec![
        st(VENDOR_A, &f, 7, f.as_of - 60),
        st(VENDOR_B, &f, 8, f.as_of - 60),
        st(VENDOR_A, &g, 5, g.as_of - 60),
    ];
    sort_statements(&mut statements);
    for r in [report(SIGNER_X, &f, 7, 1), report(AFFILIATE, &g, 5, 1)] {
        let payload = borsh::to_vec(&r.payload).map_err(|e| e.to_string())?;
        node.send(
            &relayer,
            &book,
            "postReport(bytes,bytes)",
            &[
                format!("0x{}", hex::encode(&payload)),
                format!("0x{}", hex::encode(r.signature)),
            ],
        )?;
    }
    // Settleable after the dispute window, judged after the contest window.
    node.warp(7_300)?;
    node.mine(3)?;

    let chain = FactChain {
        chain_id: 31_337,
        book,
        registry: [0; 20],
    };
    let params = TrackParams::new(vec![MetricRule {
        metric: MetricId(METRIC.into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    }])
    .with_level3_resolvers_only()
    .with_chain(chain.id())
    .with_operators(vec![
        OperatorOf {
            source: SourceId(VENDOR_A.into()),
            operator: op,
        },
        OperatorOf {
            source: SourceId(AFFILIATE.into()),
            operator: op,
        },
    ]);
    let registry = reg();

    // Coordinator: pin a finalized head, derive, compute, publish.
    let facts = CastFacts::pin(
        cast.clone(),
        rpc.clone(),
        node.work.clone(),
        chain,
        Finality::Depth(2),
    )?;
    let resolutions = derive_resolutions(
        &params,
        &registry,
        &facts,
        &statements,
        None,
        &Preimages::default(),
    )
    .map_err(|e| e.to_string())?;
    check_operators(&params, &facts, &statements, &resolutions).map_err(|e| e.to_string())?;
    let book1 = compute(
        &params,
        &registry,
        &statements,
        &resolutions,
        facts.head().timestamp,
    )
    .map_err(|e| e.to_string())?;
    let outcome = |b: &onyx_track::TrackBook, source: &str, fact: &Fact| {
        b.entries()
            .iter()
            .find(|e| e.source.0 == source && e.fact == *fact)
            .map(|e| e.outcome)
    };
    if outcome(&book1, VENDOR_A, &f) != Some(Outcome::Agreed)
        || outcome(&book1, VENDOR_B, &f) != Some(Outcome::Disagreed)
        || outcome(&book1, VENDOR_A, &g) != Some(Outcome::SelfResolved)
    {
        return Err(format!("unexpected outcomes: {:?}", book1.entries()));
    }
    println!(
        "resolved from the book: A agreed, B disagreed, A's affiliate did not judge A ({} resolutions)",
        resolutions.len()
    );
    let c1 = book1.commitment(Hash32::ZERO);
    let log = CommitmentLog {
        chain_id: 31_337,
        book,
        poster,
        track: keccak256(b"example track"),
    };
    let publisher = CommitmentLogCast {
        cast: cast.clone(),
        rpc: rpc.clone(),
        work: node.work.clone(),
        log,
    };
    let i1 = publisher.post(facts.head().block, &c1)?;
    node.mine(3)?;

    // Client: its own reads at the commitment's head and the log's latest.
    let client_head = |c: &onyx_track::TrackCommitment, head: u64| -> Result<CastFacts, String> {
        // The commitment's head block (its TrackCommitted event's `head`),
        // final and timed at its as_of.
        CastFacts::at_commitment(
            cast.clone(),
            rpc.clone(),
            node.work.clone(),
            chain,
            Finality::Depth(2),
            head,
            c.as_of,
        )
    };
    let latest_head = |tag: &str| publisher.head(tag);
    let final_tag = format!("0x{:x}", node.block("latest")?.block - 2);
    let cv = client_head(&c1, facts.head().block)?;
    // Review TR-C7-R2-3: the commitment's own head (timed at its as_of) is
    // not read while it is above the finalized head (its facts could still be
    // reorged out): here a node whose finality lags far behind.
    expect_err(
        "a commitment head that is not final",
        CastFacts::at_commitment(
            cast.clone(),
            rpc.clone(),
            node.work.clone(),
            chain,
            Finality::Depth(1_000),
            facts.head().block,
            c1.as_of,
        ),
        |e| e.contains("not final"),
    )?;

    verify_resolutions(
        &registry,
        &cv,
        c1.as_of,
        tlog(&params, &statements, &resolutions),
        None,
        &Preimages::default(),
    )
    .map_err(|e| format!("client check: {e}"))?;
    check_latest(&c1, &latest_head(&final_tag)?).map_err(|e| e.to_string())?;
    check_in_log(&c1, &publisher.at(&final_tag, i1)?).map_err(|e| e.to_string())?;
    println!("client: resolutions rederived from its own reads; c1 is the latest on chain");

    // TR-5: an omitted and a forged resolution.
    let check = |r: &[onyx_track::Resolution]| {
        verify_resolutions(
            &registry,
            &cv,
            c1.as_of,
            tlog(&params, &statements, r),
            None,
            &Preimages::default(),
        )
    };
    let missed: Vec<_> = resolutions
        .iter()
        .filter(|r| r.resolver.0 != SIGNER_X)
        .cloned()
        .collect();
    expect_err("an omitted resolution", check(&missed), |e| {
        *e == ChainError::Mismatch
    })?;
    let mut forged = resolutions.clone();
    for r in forged.iter_mut().filter(|r| r.resolver.0 == SIGNER_X) {
        r.value = Value::UInt(8);
    }
    expect_err("a forged resolution", check(&forged), |e| {
        *e == ChainError::Mismatch
    })?;
    // TR-R2-4: params that hide the declared affiliation.
    let hidden = TrackParams {
        operators: Vec::new(),
        ..params.clone()
    };
    expect_err(
        "params hiding a declared affiliation",
        check_operators(&hidden, &cv, &statements, &resolutions),
        |e| matches!(e, ChainError::OperatorMismatch(_)),
    )?;

    // A later commitment: signer X now answers G too.
    let statements2 = statements.clone();
    let r = report(SIGNER_X, &g, 5, 2);
    let payload = borsh::to_vec(&r.payload).map_err(|e| e.to_string())?;
    node.send(
        &relayer,
        &book,
        "postReport(bytes,bytes)",
        &[
            format!("0x{}", hex::encode(&payload)),
            format!("0x{}", hex::encode(r.signature)),
        ],
    )?;
    // Settleable after the dispute window, judged after the contest window.
    node.warp(7_300)?;
    node.mine(3)?;
    let facts2 = CastFacts::pin(
        cast.clone(),
        rpc.clone(),
        node.work.clone(),
        chain,
        Finality::Depth(2),
    )?;
    let resolutions2 = derive_resolutions(
        &params,
        &registry,
        &facts2,
        &statements2,
        Some(Prev {
            as_of: c1.as_of,
            statements: &statements,
            resolutions: &resolutions,
        }),
        &Preimages::default(),
    )
    .map_err(|e| e.to_string())?;
    let book2 = compute(
        &params,
        &registry,
        &statements2,
        &resolutions2,
        facts2.head().timestamp,
    )
    .map_err(|e| e.to_string())?;
    if outcome(&book2, VENDOR_A, &g) != Some(Outcome::Agreed) {
        return Err("signer X's later answer should judge A on G".into());
    }
    let c2 = book2.commitment(c1.hash());
    onyx_track::check_extends(
        &c1,
        tlog(&params, &statements, &resolutions),
        &c2,
        TrackLog {
            params: &params,
            statements: &statements2,
            resolutions: &resolutions2,
        },
    )
    .map_err(|e| e.to_string())?;
    let i2 = publisher.post(facts2.head().block, &c2)?;
    node.mine(3)?;
    let final_tag = format!("0x{:x}", node.block("latest")?.block - 2);
    let head2 = latest_head(&final_tag)?;
    check_latest(&c2, &head2).map_err(|e| e.to_string())?;
    println!("c2 posted at index {i2}: chained, later, the latest on chain");
    // The whole check (review TR-C7-R1-3): every log entry from the first, each
    // at its own head.
    let cv2 = client_head(&c2, facts2.head().block)?;
    let none = Preimages::default();
    let at = |i| publisher.at(&final_tag, i);
    verify_chain(
        &[
            ChainStep {
                reg: &registry,
                view: &cv,
                commitment: &c1,
                log: tlog(&params, &statements, &resolutions),
                preimages: &none,
                at: at(i1)?,
            },
            ChainStep {
                reg: &registry,
                view: &cv2,
                commitment: &c2,
                log: tlog(&params, &statements2, &resolutions2),
                preimages: &none,
                at: at(i2)?,
            },
        ],
        &head2,
    )
    .map_err(|e| format!("chain walk: {e}"))?;
    println!("client: the whole log walked from its first entry, each at its own head");

    // TR-R2-5: a server still serving c1 (rollback) is refused, with the
    // served body too.
    expect_err("a rolled-back commitment", check_latest(&c1, &head2), |e| {
        *e == ChainError::NotLatest
    })?;
    let body = record_body(
        &Served {
            book: &book1,
            commitment: &c1,
            params: &params,
            chain: None,
        },
        &SourceId(VENDOR_A.into()),
        &MetricId(METRIC.into()),
    )
    .ok_or("body")?;
    expect_err(
        "a served body under an older commitment",
        check_record_body(
            &body,
            &Expect {
                source: VENDOR_A,
                metric: METRIC,
                min_as_of: 0,
                known: None,
                latest: Some(head2.latest),
            },
        ),
        |e| *e == WireError::NotLatest,
    )?;
    // TR-R1-11 / TR-S-14: a fork chained to c1 cannot enter the log.
    let fork = book2.commitment(c1.hash());
    let mut fork = fork;
    fork.entries_root = keccak256(b"rewritten history");
    expect_err(
        "a fork chained to a known commitment",
        publisher.post(facts2.head().block, &fork),
        |e| e.contains("TrackNotChained"),
    )?;
    // Spam: another poster fills its own log; the pinned log is unchanged
    // and nobody else appends to it.
    let spam = CommitmentLogCast {
        log: CommitmentLog {
            poster: spammer,
            ..log
        },
        ..publisher.clone()
    };
    spam.post(facts2.head().block, &book1.commitment(Hash32::ZERO))?;
    // The spammer's successor of c2 lands nowhere: its sender's log is its own.
    let mut intruder = c2.clone();
    intruder.prev = c2.hash();
    intruder.as_of = c2.as_of + 1;
    expect_err(
        "another poster extending the pinned chain",
        spam.post(facts2.head().block, &intruder),
        |e| e.contains("TrackNotChained"),
    )?;
    node.mine(3)?;
    let final_tag = format!("0x{:x}", node.block("latest")?.block - 2);
    if latest_head(&final_tag)? != head2 {
        return Err("spam changed the pinned log".into());
    }
    println!("spam stayed in the spammer's own log");

    // C9 (TR-R4-4): a track key's level-3 status at a post's block, read
    // from the book by block hash. The reviewer marks vendor B's key
    // compromised; at a block before, both keys read active; at the head after,
    // B's reads compromised and only A's still counts.
    let mut keys: Vec<TrackKey> = [VENDOR_A, VENDOR_B]
        .iter()
        .map(|s| TrackKey {
            source: SourceId((*s).into()),
            signer: addr(s),
            revoked: false,
        })
        .collect();
    keys.sort_by_key(TrackKey::id);
    let keys = KeyBook { keys };
    let before = node.block("latest")?;
    node.send(
        &reviewer,
        &book,
        "markCompromised(bytes32,uint64,string)",
        &[
            hx(&key_id(VENDOR_B, &addr(VENDOR_B))),
            before.timestamp.to_string(),
            "test: lost".into(),
        ],
    )?;
    node.mine(2)?;
    let view = CastFacts::pin(
        cast.clone(),
        rpc.clone(),
        node.work.clone(),
        chain,
        Finality::Depth(0),
    )?;
    let life = |l: &KeyLives, s: &str| {
        l.keys
            .iter()
            .find(|(id, _)| *id == key_id(s, &addr(s)))
            .map(|(_, life)| *life)
    };
    let old = view.key_lives_at(&before, &keys)?;
    let now = view.key_lives_at(&view.head, &keys)?;
    if life(&old, VENDOR_A) != Some(KeyLife::Active)
        || life(&old, VENDOR_B) != Some(KeyLife::Active)
    {
        return Err(format!("both keys should read active before: {old:?}"));
    }
    if life(&now, VENDOR_A) != Some(KeyLife::Active)
        || life(&now, VENDOR_B) != Some(KeyLife::Compromised)
    {
        return Err(format!("B's key should read compromised after: {now:?}"));
    }
    let mut stale = before;
    stale.block_hash = keccak256(b"a block of another fork");
    expect_err(
        "a post anchor that is not the canonical block",
        view.key_lives_at(&stale, &keys),
        |e| e.contains("not the canonical block"),
    )?;
    let mut above = view.head;
    above.block += 1;
    expect_err(
        "a post above the pinned head",
        view.key_lives_at(&above, &keys),
        |e| e.contains("above the pinned head"),
    )?;
    println!("key statuses read at each post's block: B's compromised key stops counting");

    // Reorg under a pinned head: snapshot, pin the latest block, revert and
    // mine other blocks; the pinned head is refused, never read through.
    let snap = node.rpc("evm_snapshot", &[])?;
    node.mine(2)?;
    let pinned = CastFacts::pin(
        cast.clone(),
        rpc.clone(),
        node.work.clone(),
        chain,
        Finality::Depth(0),
    )?;
    pinned.recheck()?;
    node.rpc("evm_revert", &[&snap.to_string()])?;
    node.warp(7)?;
    node.mine(2)?;
    expect_err("a reorged-out head (recheck)", pinned.recheck(), |e| {
        e.contains("replaced")
    })?;
    expect_err(
        "a read at a reorged-out head",
        pinned.operator_of(&SourceId(VENDOR_A.into())),
        |_| true,
    )?;

    println!("TRACK ONCHAIN ANVIL PASSED");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("track-onchain-anvil: {e}");
        std::process::exit(1);
    }
}
