//! Load for the soak test (ROADMAP L6, scripts/soak.sh): clients on their own
//! loopback addresses (127.0.0.10, .11, ...; each its own rate-limit bucket)
//! send a mix of unpaid and paid requests to a running `onyx-node` (and
//! unpaid ones to `onyx-node-btc`), retry what timed out with the identical
//! request, replay answered payments, and log every request as one JSON line.
//! Local only: it talks to 127.0.0.1 and signs with test keys derived from a
//! public label (`keccak256("onyx soak payer <i>")`), funded on an anvil fork.
//!
//! ```text
//! soak-load addresses --clients 8                 # payer addresses (4 per client)
//! soak-load run --node http://127.0.0.1:18752 --btc http://127.0.0.1:18753 \
//!   --revenue 0x... --clients 8 --log load.jsonl --stop-file STOP [--pace-ms 2000]
//! ```
//!
//! A 200 answer to a request sent without payment on a priced route is a
//! violation (`"kind":"VIOLATION"` in the log); so is a 200 to a payment
//! header sent for another URL. The chain-side checks (every paid answer has
//! its transfer on chain, no double credit) run after the soak
//! (scripts/soak/report.py).

use onyx_api::x402::{
    decode_header, encode_header, exact_evm, PaymentPayload, PaymentRequired, SettleResponse,
};
use onyx_core::keccak256;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Largest answer read (an export is at most 32 MiB).
const MAX_ANSWER: usize = 40 << 20;
/// Client-side deadline per request (the server's own is 12 s; a settle may
/// run on after it answers 504).
const DEADLINE: Duration = Duration::from_secs(40);
const METRIC: &str = "btc.entity-flow";
const RESERVE: &str = "btc.entity-reserve";
/// End heights of the soak shards and round 1 (blocks 970,544-970,566).
const AS_OF: std::ops::RangeInclusive<u64> = 970_544..=970_566;

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Payers per client: the server allows 4 unsettled intents per payer
/// (429 `payer_busy`), and faults leave intents open for minutes.
const PAYERS_PER_CLIENT: usize = 4;

fn payer_key(i: usize) -> [u8; 32] {
    keccak256(format!("onyx soak payer {i}").as_bytes()).0
}

/// A small xorshift: the mix needs no cryptographic randomness.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn nonce(&mut self) -> [u8; 32] {
        let seed: Vec<u8> = (0..4).flat_map(|_| self.next().to_le_bytes()).collect();
        let mut extra = seed;
        extra.extend_from_slice(&now_ms().to_le_bytes());
        keccak256(&extra).0
    }
}

struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn code(&self) -> Option<String> {
        let v: Value = serde_json::from_slice(&self.body).ok()?;
        v["error"]["code"].as_str().map(String::from)
    }
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = b.windows(2).position(|w| w == b"\r\n").ok_or("bad chunk")?;
        let size_txt = std::str::from_utf8(&b[..end]).map_err(|e| e.to_string())?;
        let size = usize::from_str_radix(size_txt.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|e| e.to_string())?;
        b = &b[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("short chunk".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// One GET over a fresh connection from `bind`.
async fn get(
    bind: IpAddr,
    base: &str,
    path: &str,
    headers: &[(&str, String)],
) -> Result<Answer, String> {
    let hostport = base
        .strip_prefix("http://")
        .ok_or("http:// only")?
        .trim_end_matches('/');
    let addr: SocketAddr = hostport.parse().map_err(|e| format!("{hostport}: {e}"))?;
    let work = async {
        let sock = tokio::net::TcpSocket::new_v4().map_err(|e| e.to_string())?;
        sock.bind(SocketAddr::new(bind, 0))
            .map_err(|e| format!("bind {bind}: {e}"))?;
        let mut s = sock
            .connect(addr)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes())
            .await
            .map_err(|e| format!("write: {e}"))?;
        let mut raw = Vec::new();
        let mut buf = vec![0u8; 64 << 10];
        loop {
            let n = s.read(&mut buf).await.map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
            if raw.len() > MAX_ANSWER {
                return Err("answer too large".to_string());
            }
        }
        let end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("no header end (connection closed early)")?;
        let head = String::from_utf8_lossy(&raw[..end]).to_string();
        let mut lines = head.split("\r\n");
        let status: u16 = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .ok_or("bad status line")?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect();
        let mut body = raw[end + 4..].to_vec();
        if headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
        }) {
            body = dechunk(&body)?;
        }
        Ok(Answer {
            status,
            headers,
            body,
        })
    };
    tokio::time::timeout(DEADLINE, work)
        .await
        .map_err(|_| "client deadline".to_string())?
}

struct Ctx {
    node: String,
    btc: Option<String>,
    revenue: String,
    log: mpsc::UnboundedSender<Value>,
    stop: Arc<AtomicBool>,
    pace: Duration,
}

/// Log one request.
#[allow(clippy::too_many_arguments)]
fn record(
    ctx: &Ctx,
    client: usize,
    kind: &str,
    node: &str,
    path: &str,
    r: &Result<Answer, String>,
    ms: u128,
    extra: Value,
) {
    let mut v =
        json!({"t": now_ms(), "c": client, "kind": kind, "node": node, "path": path, "ms": ms});
    match r {
        Ok(a) => {
            v["st"] = json!(a.status);
            if let Some(c) = a.code() {
                v["code"] = json!(c);
            }
        }
        Err(e) => v["err"] = json!(e),
    }
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in e {
            o.insert(k.clone(), x.clone());
        }
    }
    let _ = ctx.log.send(v);
}

fn violation(ctx: &Ctx, client: usize, what: &str, path: &str) {
    eprintln!("soak-load: VIOLATION client {client}: {what} ({path})");
    let _ = ctx
        .log
        .send(json!({"t": now_ms(), "c": client, "kind": "VIOLATION", "what": what, "path": path}));
}

async fn timed(
    bind: IpAddr,
    base: &str,
    path: &str,
    headers: &[(&str, String)],
) -> (Result<Answer, String>, u128) {
    let t = Instant::now();
    let r = get(bind, base, path, headers).await;
    (r, t.elapsed().as_millis())
}

fn metric_path(metric: &str, subject: &str, as_of: Option<u64>) -> String {
    match as_of {
        Some(h) => format!("/v1/metrics/{metric}?subject={subject}&as_of={h}"),
        None => format!("/v1/metrics/{metric}?subject={subject}"),
    }
}

/// An unpaid request to a priced route: anything but a 200 is fine.
async fn unpaid(ctx: &Ctx, c: usize, bind: IpAddr, kind: &str, base: &str, path: &str) {
    let (r, ms) = timed(bind, base, path, &[]).await;
    if matches!(&r, Ok(a) if a.status == 200) {
        violation(ctx, c, "200 without payment", path);
    }
    record(
        ctx,
        c,
        kind,
        if Some(base) == ctx.btc.as_deref() {
            "btc"
        } else {
            "node"
        },
        path,
        &r,
        ms,
        json!({}),
    );
}

/// Should an identical retry follow this outcome? (The answer never came,
/// or the server says the settle is still running or failed.)
fn retryable(r: &Result<Answer, String>) -> bool {
    match r {
        Err(_) => true,
        Ok(a) => {
            a.status == 504
                || (a.status == 503
                    && !matches!(a.code().as_deref(), Some("payer_busy" | "clock_skew")))
        }
    }
}

/// Retries of one purchase (the server's identical-retry window is 10
/// minutes; a `payment_pending` asks for about 30 s between tries).
const MAX_RETRIES: u32 = 8;

/// One paid purchase: offer, signed authorization, identical retries on a
/// lost answer, then sometimes a replay and a reuse on another URL.
async fn paid(ctx: &Ctx, c: usize, bind: IpAddr, rng: &mut Rng) {
    let key = payer_key(c * PAYERS_PER_CLIENT + rng.below(PAYERS_PER_CLIENT as u64) as usize);
    let Some(payer) = exact_evm::address(&key) else {
        return;
    };
    let payer_hex = format!("0x{}", hex::encode(payer));
    let as_of = (rng.below(2) != 0).then(|| *AS_OF.start() + rng.below(23));
    let path = metric_path(METRIC, "entity:okx", as_of);
    let (r, ms) = timed(bind, &ctx.node, &path, &[]).await;
    let offer = match &r {
        Ok(a) if a.status == 402 => a
            .header("payment-required")
            .and_then(|h| decode_header::<PaymentRequired>(h).ok()),
        Ok(a) if a.status == 200 => {
            violation(ctx, c, "200 without payment", &path);
            None
        }
        _ => None,
    };
    record(ctx, c, "offer", "node", &path, &r, ms, json!({}));
    let Some(pr) = offer else { return };
    let Some(accepted) = pr.accepts.first().cloned() else {
        return;
    };
    if !accepted.pay_to.eq_ignore_ascii_case(&ctx.revenue) {
        violation(ctx, c, "payTo is not OnyxRevenue", &path);
        return;
    }
    let Ok(value) = accepted.amount.parse::<u128>() else {
        return;
    };
    let nonce = rng.nonce();
    let mut to = [0u8; 20];
    if hex::decode_to_slice(ctx.revenue.trim_start_matches("0x"), &mut to).is_err() {
        return;
    }
    let auth = exact_evm::Authorization {
        from: payer,
        to,
        value,
        valid_after: 0,
        // Short-lived (the server wants at least its settle budget + 5 s,
        // capped at maxTimeoutSeconds): an intent left open by a fault is
        // resolvable about 12 minutes later (validBefore + 10 min).
        valid_before: now_ms() / 1000 + 120,
        nonce,
    };
    let Some(sig) = exact_evm::sign(&key, &auth, &accepted) else {
        return;
    };
    let header = encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource.clone()),
        accepted,
        payload: sig,
    });
    let nonce_hex = format!("0x{}", hex::encode(nonce));
    let hs = [("PAYMENT-SIGNATURE", header.clone())];
    let mut attempt = 0;
    let mut served: Option<Vec<u8>> = None;
    loop {
        let (r, ms) = timed(bind, &ctx.node, &path, &hs).await;
        let tx = r
            .as_ref()
            .ok()
            .and_then(|a| a.header("payment-response"))
            .and_then(|h| decode_header::<SettleResponse>(h).ok())
            .map(|s| s.transaction);
        let ok = matches!(&r, Ok(a) if a.status == 200);
        let bh = r
            .as_ref()
            .ok()
            .filter(|_| ok)
            .map(|a| format!("0x{}", hex::encode(&keccak256(&a.body).0[..8])));
        record(
            ctx,
            c,
            if attempt == 0 { "paid" } else { "retry" },
            "node",
            &path,
            &r,
            ms,
            json!({"payer": payer_hex, "nonce": nonce_hex, "attempt": attempt, "tx": tx, "bh": bh, "value": value.to_string()}),
        );
        if ok {
            served = r.ok().map(|a| a.body);
            break;
        }
        attempt += 1;
        if attempt > MAX_RETRIES || !retryable(&r) || ctx.stop.load(Ordering::Relaxed) {
            break;
        }
        // Honour Retry-After (at most 60 s), else 1-4 s.
        let wait = r
            .as_ref()
            .ok()
            .and_then(|a| a.header("retry-after"))
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(Duration::from_millis(1_000 + rng.below(3_000)), |s| {
                Duration::from_secs(s.min(60))
            });
        tokio::time::sleep(wait).await;
    }
    let Some(body) = served else { return };
    // Identical replays: the same answer, no second charge (run-api.md).
    if rng.below(10) < 3 {
        for _ in 0..1 + rng.below(2) {
            let (r, ms) = timed(bind, &ctx.node, &path, &hs).await;
            let same = matches!(&r, Ok(a) if a.status == 200 && a.body == body);
            record(
                ctx,
                c,
                "replay",
                "node",
                &path,
                &r,
                ms,
                json!({"payer": payer_hex, "nonce": nonce_hex, "same": same}),
            );
            if matches!(&r, Ok(a) if a.status == 200 && a.body != body) {
                violation(ctx, c, "replay answered a different body", &path);
            }
        }
    }
    // The same header on another URL must not buy that URL.
    if rng.below(10) == 0 {
        let other = metric_path(METRIC, "entity:bitwise", as_of);
        let (r, ms) = timed(bind, &ctx.node, &other, &hs).await;
        if matches!(&r, Ok(a) if a.status == 200) {
            violation(
                ctx,
                c,
                "payment header reused on another URL answered 200",
                &other,
            );
        }
        record(
            ctx,
            c,
            "reuse",
            "node",
            &other,
            &r,
            ms,
            json!({"payer": payer_hex, "nonce": nonce_hex}),
        );
    }
}

async fn client(ctx: Arc<Ctx>, c: usize) {
    let bind = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 10 + c as u8));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ ((c as u64 + 1) << 32) ^ now_ms());
    while !ctx.stop.load(Ordering::Relaxed) {
        let t = Instant::now();
        let roll = rng.below(100);
        let as_of = (rng.below(4) != 0).then(|| *AS_OF.start() + rng.below(23));
        match (roll, ctx.btc.as_deref()) {
            (0..=24, _) => {
                let p = metric_path(METRIC, "entity:okx", as_of);
                unpaid(&ctx, c, bind, "unpaid", &ctx.node, &p).await;
            }
            (25..=34, Some(btc)) => {
                let p = metric_path(RESERVE, "entity:okx", None);
                unpaid(&ctx, c, bind, "reserve", btc, &p).await;
            }
            (35..=39, Some(btc)) => {
                let p = metric_path(RESERVE, "entity:binance", None);
                unpaid(&ctx, c, bind, "binance", btc, &p).await;
            }
            (40..=46, Some(btc)) => {
                let p = metric_path(METRIC, "entity:okx", as_of);
                unpaid(&ctx, c, bind, "unpaid", btc, &p).await;
            }
            (47..=54, _) => {
                for (name, base) in [
                    ("node", Some(ctx.node.as_str())),
                    ("btc", ctx.btc.as_deref()),
                ] {
                    if let Some(b) = base {
                        let (r, ms) = timed(bind, b, "/v1/health", &[]).await;
                        record(&ctx, c, "health", name, "/v1/health", &r, ms, json!({}));
                    }
                }
            }
            _ => paid(&ctx, c, bind, &mut rng).await,
        }
        let spent = t.elapsed();
        let jitter = Duration::from_millis(rng.below(ctx.pace.as_millis() as u64 / 2 + 1));
        if let Some(rest) = (ctx.pace + jitter).checked_sub(spent) {
            tokio::time::sleep(rest).await;
        }
    }
}

async fn run(args: &[String]) -> Res<()> {
    let node = arg(args, "--node").ok_or("--node URL")?;
    let btc = arg(args, "--btc");
    let revenue = arg(args, "--revenue")
        .ok_or("--revenue 0x...")?
        .to_lowercase();
    let clients: usize = arg(args, "--clients").map_or(Ok(8), |s| s.parse())?;
    if clients == 0 || clients > 200 {
        return Err("--clients 1-200".into());
    }
    let log_path = PathBuf::from(arg(args, "--log").ok_or("--log FILE")?);
    let stop_file = PathBuf::from(arg(args, "--stop-file").ok_or("--stop-file FILE")?);
    let pace = Duration::from_millis(arg(args, "--pace-ms").map_or(Ok(2_000), |s| s.parse())?);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = Arc::new(Ctx {
        node,
        btc,
        revenue,
        log: tx,
        stop: stop.clone(),
        pace,
    });
    let writer = tokio::spawn(async move {
        use std::io::Write;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let mut w = std::io::BufWriter::new(f);
        let mut last = Instant::now();
        while let Some(v) = rx.recv().await {
            writeln!(w, "{v}")?;
            if last.elapsed() > Duration::from_secs(2) {
                w.flush()?;
                last = Instant::now();
            }
        }
        w.flush()?;
        Ok::<(), std::io::Error>(())
    });
    let tasks: Vec<_> = (0..clients)
        .map(|c| tokio::spawn(client(ctx.clone(), c)))
        .collect();
    while !stop_file.exists() {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    stop.store(true, Ordering::Relaxed);
    for t in tasks {
        let _ = t.await;
    }
    drop(ctx);
    writer.await??;
    eprintln!("soak-load: stopped");
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("addresses") => {
            let n: usize = arg(&args, "--clients").map_or(Ok(8), |s| s.parse())?;
            for i in 0..n * PAYERS_PER_CLIENT {
                let a = exact_evm::address(&payer_key(i)).ok_or("key")?;
                println!("0x{}", hex::encode(a));
            }
            Ok(())
        }
        Some("run") => run(&args).await,
        _ => Err("usage: soak-load addresses --clients N | run --node URL [--btc URL] --revenue 0x.. --clients N --log FILE --stop-file FILE [--pace-ms MS]".into()),
    }
}
