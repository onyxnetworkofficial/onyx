//! ROADMAP C10 on an anvil (a local one, or a fork of Base Sepolia): a
//! level-3 coordinator posts every report it takes to `OnyxSignedBook` and
//! runs its store on the latest block's time, and a running `onyx-node`
//! serves each fact only against `settleable` at its finalized block, past
//! the serving margin, with every post of the fact landed and final.
//!
//! Run by `scripts/level3-book.sh`, which starts the anvil, deploys the
//! settlement and the book with `scripts/deploy-signed-book.sh`, pins the
//! book into the manifest and starts `onyx-node` with `ONYX_SIGNED_REPORTS`
//! pointing at `--log`:
//!
//! ```text
//! level3-book-anvil --rpc URL --cast PATH --book 0x.. --reviewer 0x.. \
//!   --coordinator 0x.. --log FILE --node http://127.0.0.1:PORT
//! ```
//!
//! Local only: a loopback anvil, its public unlocked dev accounts; the
//! sources' signing keys are derived from fixed labels (test keys). Prints
//! `LEVEL3 BOOK ANVIL PASSED` at the end; any failed check exits non-zero.

use onyx_coordinator::cast_chain::is_loopback_rpc;
use onyx_coordinator::signed::{signed_fact_key, SignedSources};
use onyx_coordinator::signed_chain::SignedBookCast;
use onyx_coordinator::signed_intake::{
    BookIntake, CastPoster, Landed, PostEntry, PostError, PostStatus, SignedBookChain, TakeError,
    MAX_CHAIN_AHEAD_SECS, MAX_CHAIN_BEHIND_SECS,
};
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, Hash32, KeyProvenance, MetricId,
    MetricInfo, Redistribution, SignedPayload, SignedReport, Snapshot, SourceId, SourceKey,
    SourceRef, StaticRegistry, Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::{eip191_sign_with, secret_address};
use std::cell::Cell;
use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "example.signed";
const ORIGIN: &str = "data.example.org";
const METRIC: &str = "example.signed.reserve";
/// The API's serving margin: dispute window plus sequencer drift.
const SERVABLE_AFTER_SECS: u64 = 5_400;

type Res<T> = Result<T, String>;

fn secret(label: &str) -> [u8; 32] {
    keccak256(format!("onyx-level3-book-anvil-test-key:{label}").as_bytes()).0
}

fn ax(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn addr(s: &str) -> Res<[u8; 20]> {
    hex::decode(s.trim().trim_matches('"').trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("not an address: {s}"))
}

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(SOURCE.into()),
        attribution: None,
        terms: String::new(),
    }
}

fn reg() -> Res<StaticRegistry> {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
    .map_err(|e| e.to_string())?;
    r.add_source(rights()).map_err(|e| e.to_string())?;
    Ok(r)
}

fn source_key(label: &str) -> Res<SourceKey> {
    Ok(SourceKey {
        source: SourceId(SOURCE.into()),
        signer: secret_address(&secret(label)).ok_or("key")?,
        provenance: KeyProvenance::WellKnown {
            origin: ORIGIN.into(),
            document_hash: keccak256(b"keys document"),
            fetched_unix: 1,
        },
    })
}

fn report(label: &str, key: Hash32, value: u128, observed: u64, seq: u64) -> Res<SignedReport> {
    let payload = SignedPayload {
        source: SourceId(SOURCE.into()),
        key,
        metric: MetricId(METRIC.into()),
        subject: Subject::Entity("entity:example".into()),
        value: Value::UInt(value),
        observed_unix: observed,
        sequence: seq,
    };
    let signature = eip191_sign_with(&secret(label), payload.message().as_bytes()).ok_or("sign")?;
    Ok(SignedReport { payload, signature })
}

fn claim_for(r: &SignedReport) -> Claim {
    let h = r.hash();
    Claim {
        metric: r.payload.metric.clone(),
        subject: r.payload.subject.clone(),
        value: r.payload.value.clone(),
        source: SourceRef {
            source: r.payload.source.clone(),
            snapshot: Snapshot::Document {
                published_unix: r.payload.observed_unix,
                content_hash: h,
            },
        },
        method: VerificationMethod::SignedSource {
            key: r.payload.key,
            report: h,
        },
        label_set: None,
        rights: rights(),
        evidence: vec![EvidenceRef {
            kind: REPORT_EVIDENCE.into(),
            content_hash: h,
        }],
        confidence: None,
    }
}

/// The book through `cast`, with a switch that makes posts not land (a
/// sequencer holding them back, or a dead transport).
struct Chain {
    poster: CastPoster,
    held: Cell<bool>,
}

impl SignedBookChain for &Chain {
    fn latest(&self) -> Result<(u64, u64), String> {
        self.poster.latest()
    }
    fn post(&self, r: &SignedReport) -> Result<Landed, PostError> {
        if self.held.get() {
            return Err(PostError::Unavailable(
                "held back (rehearsal: the transaction is not mined)".into(),
            ));
        }
        self.poster.post(r)
    }
    fn recorded(&self, d: &Hash32) -> Result<bool, String> {
        self.poster.recorded(d)
    }
}

fn check(ok: bool, what: &str) -> Res<()> {
    if ok {
        println!("  ok: {what}");
        Ok(())
    } else {
        Err(format!("FAILED: {what}"))
    }
}

/// `GET {node}/v1/signed/{fact}` through curl: (status, body).
fn get(node: &str, fact: &Hash32) -> Res<(u16, serde_json::Value)> {
    let out = Command::new("curl")
        .args([
            "-s",
            "-A",
            "onyx-dev",
            "-w",
            "\n%{http_code}",
            &format!("{node}/v1/signed/{fact}"),
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = text.rsplit_once('\n').ok_or("curl: no status")?;
    let status: u16 = code.trim().parse().map_err(|_| "curl: bad status")?;
    let v = serde_json::from_str(body).map_err(|e| format!("not json ({e}): {body}"))?;
    Ok((status, v))
}

/// `GET {node}/v1/health`'s `level3` object (ROADMAP C62).
fn level3_health(node: &str) -> Res<serde_json::Value> {
    let out = Command::new("curl")
        .args(["-sf", "-A", "onyx-dev", &format!("{node}/v1/health")])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("health: not json ({e})"))?;
    Ok(v["level3"].clone())
}

fn reason(v: &serde_json::Value) -> String {
    format!(
        "{}/{}",
        v["error"]["code"].as_str().unwrap_or("-"),
        v["reason"].as_str().unwrap_or("-")
    )
}

struct Run {
    cast: SignedBookCast,
    node: String,
    log: PathBuf,
    /// Write every entry as taken `SERVABLE_AFTER_SECS` + 60 s ago by the
    /// host clock (the chain was moved on by `evm_increaseTime`).
    aged: Cell<bool>,
}

impl Run {
    fn rpc(&self, method: &str, params: &[&str]) -> Res<String> {
        let mut a = vec!["rpc".to_string(), method.to_string()];
        a.extend(params.iter().map(|s| s.to_string()));
        let out = Command::new(&self.cast.cast)
            .current_dir(&self.cast.work)
            .args(&a)
            .env("ETH_RPC_URL", &self.cast.rpc)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Move block time on by `secs` and mine past the finality lag.
    fn advance(&self, secs: u64) -> Res<()> {
        if secs > 0 {
            self.rpc("evm_increaseTime", &[&secs.to_string()])?;
        }
        self.rpc("anvil_mine", &["70"])?;
        Ok(())
    }

    fn write(&self, log: &onyx_coordinator::signed_intake::PostLog) -> Res<()> {
        let mut log = log.clone();
        if self.aged.get() {
            let back = system_now() - SERVABLE_AFTER_SECS - 60;
            for e in &mut log.entries {
                e.received_unix = e.received_unix.min(back);
            }
        }
        log.write(&self.log)?;
        // The node re-reads the file by modification time and length: make
        // sure a rewrite within the same second is seen.
        std::thread::sleep(std::time::Duration::from_millis(20));
        Ok(())
    }

    fn expect(&self, fact: &Hash32, status: u16, want: &str, what: &str) -> Res<serde_json::Value> {
        let (s, v) = get(&self.node, fact)?;
        let r = reason(&v);
        check(
            s == status && (want.is_empty() || r == want),
            &format!("{what}: {s} {r}"),
        )?;
        Ok(v)
    }
}

fn arg(args: &[String], flag: &str) -> Res<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
        .ok_or_else(|| format!("{flag} is required"))
}

fn system_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => println!("LEVEL3 BOOK ANVIL PASSED"),
        Err(e) => {
            eprintln!("level3-book-anvil: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Res<()> {
    let rpc = arg(args, "--rpc")?;
    if !is_loopback_rpc(&rpc) {
        return Err("anvil only: the RPC must be a loopback URL".into());
    }
    let cast = SignedBookCast {
        cast: PathBuf::from(arg(args, "--cast")?),
        rpc,
        work: std::env::temp_dir(),
        book: addr(&arg(args, "--book")?)?,
    };
    let reviewer = addr(&arg(args, "--reviewer")?)?;
    let coordinator = addr(&arg(args, "--coordinator")?)?;
    let run = Run {
        cast: cast.clone(),
        node: arg(args, "--node")?,
        log: PathBuf::from(arg(args, "--log")?),
        aged: Cell::new(false),
    };
    let chain_id: u64 = run
        .rpc("eth_chainId", &[])?
        .trim_matches('"')
        .trim_start_matches("0x")
        .parse::<String>()
        .ok()
        .and_then(|h| u64::from_str_radix(&h, 16).ok())
        .ok_or("chain id")?;
    let registry = reg()?;

    println!("1. the reviewer registers three keys; the coordinator's store mirrors them");
    let t0 = cast.block_time("latest")?;
    let (mut store, admin) = SignedSources::with_reviewer("reviewer");
    let mut ids = Vec::new();
    for l in ["K0", "K1", "K2"] {
        let k = source_key(l)?;
        cast.send(
            &reviewer,
            "registerKey(string,address,string,bytes32,string)",
            &[
                SOURCE.into(),
                ax(&k.signer),
                ORIGIN.into(),
                format!("0x{}", hex::encode(keccak256(b"keys document").0)),
                "rehearsal".into(),
            ],
        )?;
        ids.push(
            store
                .register_key(&admin, k, &registry, t0, "rehearsal")
                .map_err(|e| e.to_string())?,
        );
    }
    let ch = Chain {
        poster: CastPoster {
            book: cast.clone(),
            from: coordinator,
        },
        held: Cell::new(false),
    };
    let mut c = BookIntake::new(&ch, store, chain_id, cast.book);

    println!(
        "2. the coordinator takes X (fact F1) and Z (fact F2): each posted, store on block time"
    );
    let now = cast.block_time("latest")?;
    let x = report("K0", ids[0], 1, now - 60, 1)?;
    let z = report("K2", ids[2], 7, now - 90, 1)?;
    let (f1, f2) = (signed_fact_key(&x.payload), signed_fact_key(&z.payload));
    check(f1 != f2, "two facts")?;
    for r in [&x, &z] {
        let host = cast.block_time("latest")?;
        let t = c
            .take(&claim_for(r), r, &registry, host)
            .map_err(|e| e.to_string())?;
        check(
            matches!(t.chain, PostStatus::Recorded { .. }) && t.local.is_ok(),
            &format!(
                "posted and recorded on chain ({:?}); store accepted at now={}",
                t.chain, t.now
            ),
        )?;
        check(t.now >= host, "the store's now is the latest block's time")?;
    }
    run.write(&c.log)?;
    run.expect(
        &f2,
        409,
        "level3_not_servable/post_not_final",
        "F2 right after the post: the post is not in the finalized block yet",
    )?;
    run.advance(0)?;
    run.expect(
        &f2,
        409,
        "level3_not_servable/in_dispute_window",
        "F2 final but inside the dispute window",
    )?;

    println!("3. Y contradicts X (fact F1) and its post is held back");
    ch.held.set(true);
    let y = report("K1", ids[1], 2, now - 60, 1)?;
    let host = cast.block_time("latest")?;
    let t = c
        .take(&claim_for(&y), &y, &registry, host)
        .map_err(|e| e.to_string())?;
    check(
        matches!(t.chain, PostStatus::Pending { .. }),
        "Y pending: the transaction did not land",
    )?;
    run.write(&c.log)?;
    run.expect(
        &f1,
        409,
        "level3_not_servable/post_pending",
        "F1 with Y pending",
    )?;

    println!("4. one hour of block time: settleable at the head, not past the margin");
    run.advance(3_600)?;
    let latest_ok = cast.settleable(&f2, "latest")?.is_some();
    check(latest_ok, "F2 settleable at latest")?;
    let v = run.expect(
        &f2,
        409,
        "level3_not_servable/in_serving_margin",
        "F2 past the window, inside the 1800 s drift margin",
    )?;
    check(
        v["servableAt"].as_u64().is_some(),
        "the refusal says when it becomes servable",
    )?;

    println!("5. past the margin: F2 served from the finalized block; F1 held by the pending Y");
    run.advance(1_800)?;
    // The chain moved 5,400 s on; this host's clock did not.
    run.expect(
        &f2,
        409,
        "level3_not_servable/in_host_margin",
        "F2 past the margin in block time, not on the host clock",
    )?;
    // From here the rehearsal stands in for 90 minutes of host time: the log
    // says the reports were taken that long ago.
    run.aged.set(true);
    run.write(&c.log)?;
    let v = run.expect(&f2, 200, "-/-", "F2 served")?;
    check(
        v["reportHash"].as_str() == Some(z.hash().to_string().as_str())
            && v["level"] == 3
            && v["value"]["uint"] == "7",
        "the served report is Z, level 3, value 7",
    )?;
    let fin = cast.block_number("finalized")?;
    let at = v["checkedAt"]["block"].as_u64().unwrap_or(u64::MAX);
    check(
        at <= fin,
        &format!("checked at block {at}, finalized {fin}"),
    )?;
    check(
        v["checkedAt"]["blockTime"].as_u64().unwrap_or(0)
            >= v["acceptedAt"].as_u64().unwrap_or(u64::MAX) + SERVABLE_AFTER_SECS,
        "the finalized block is past acceptance + 5400 s",
    )?;
    check(
        cast.settleable(&f1, "finalized")?.is_some(),
        "on chain X is settleable at the finalized block (only Y's pending post holds F1)",
    )?;
    run.expect(
        &f1,
        409,
        "level3_not_servable/post_pending",
        "F1 still held",
    )?;

    println!("6. Y lands: F1 disputed on chain and never served");
    ch.held.set(false);
    let host = cast.block_time("latest")?;
    let left = c.retry_pending(host).map_err(|e| e.to_string())?;
    check(left == 0, "no post pending")?;
    run.write(&c.log)?;
    run.advance(0)?;
    run.expect(&f1, 409, "level3_not_servable/disputed", "F1 disputed")?;

    println!("7. a post pending past the alert time stops level 3 altogether");
    let mut log = c.log.clone();
    let w = report("K2", ids[2], 9, now - 30, 2)?;
    log.entries.push(PostEntry {
        report: w,
        received_unix: system_now() - 700,
        received_block_time: host,
        status: PostStatus::Pending {
            since: system_now() - 700,
            error: "held back".into(),
        },
    });
    run.write(&log)?;
    run.expect(
        &f2,
        503,
        "level3_posts_delayed/-",
        "F2 while a post is 700 s late",
    )?;
    // ROADMAP C62: /v1/health names the stop and the pending post's age.
    let h = level3_health(&run.node)?;
    check(
        h["stopped"] == "level3_posts_delayed"
            && h["postsPending"] == 1
            && h["oldestPendingSecs"].as_u64().is_some_and(|s| s >= 700)
            && h["breakerSecs"] == 600,
        &format!(
            "/v1/health level3: stopped {}, {} pending for {} s",
            h["stopped"], h["postsPending"], h["oldestPendingSecs"]
        ),
    )?;
    run.write(&c.log)?;
    run.expect(&f2, 200, "-/-", "F2 again once nothing is late")?;
    let h = level3_health(&run.node)?;
    check(
        h["stopped"].is_null()
            && h["postsPending"] == 0
            && h["oldestPendingSecs"].is_null()
            && h["posts"].as_u64() == Some(c.log.entries.len() as u64),
        &format!(
            "/v1/health level3 once nothing is late: {} posts, {} recorded, none pending",
            h["posts"], h["postsRecorded"]
        ),
    )?;

    println!("8. a coordinator that skips its post: its log says recorded, the chain never saw it");
    let mut log = c.log.clone();
    let s = report("K2", ids[2], 11, now - 20, 3)?;
    let f3 = signed_fact_key(&s.payload);
    log.entries.push(PostEntry {
        report: s,
        received_unix: system_now(),
        received_block_time: host,
        status: PostStatus::Recorded {
            outcome: None,
            block: 1,
            block_time: host,
        },
    });
    run.write(&log)?;
    run.expect(
        &f3,
        409,
        "level3_not_servable/post_not_final",
        "an unposted report is never served",
    )?;
    run.write(&c.log)?;

    println!("9. clock skew: a latest block far from the host clock is refused, nothing posted");
    let before = c.log.entries.len();
    let chain_t = cast.block_time("latest")?;
    let q = report("K2", ids[2], 13, chain_t - 10, 4)?;
    let e = c.take(
        &claim_for(&q),
        &q,
        &registry,
        chain_t - MAX_CHAIN_AHEAD_SECS - 1,
    );
    check(
        matches!(e, Err(TakeError::ChainAhead { .. })),
        "chain time ahead of the host by more than drift + skew",
    )?;
    let e = c.take(
        &claim_for(&q),
        &q,
        &registry,
        chain_t + MAX_CHAIN_BEHIND_SECS + 1,
    );
    check(
        matches!(e, Err(TakeError::ChainBehind { .. })),
        "chain time behind the host by more than 300 s",
    )?;
    check(c.log.entries.len() == before, "nothing logged")?;
    check(
        cast.call(
            "latest",
            "report(bytes32)((bytes32,bytes32,bytes32,bytes32,uint64,uint64,uint64))",
            &[q.payload.digest().to_string()],
        )
        .map(|o| o.starts_with(&format!("({}", Hash32::ZERO)))?,
        "nothing posted",
    )?;
    // ROADMAP C61: scripts/level3-book.sh step 7 checks F2 with the client.
    println!("FACT F2 {f2}");
    Ok(())
}
