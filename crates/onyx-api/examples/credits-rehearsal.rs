//! The credits ledger on a local anvil, end to end (CL6, ADR-0056, scripts/credits-anvil.sh).
//!
//! An observer that holds NO ETH deposits through the relayer (an EIP-3009 authorization), posts
//! its signed commit and the receipt for it through the relayer, each paid by a voucher adding the
//! action's fixed fee; a forged commit, a repeated post and a relay over the hourly cap are
//! refused (the repeat after its simulation reverted, released and not charged). The account views
//! and lines are read with the accounts' own proofs (a miner's earned share is in its view), then
//! one reporter pass redeems the fees and pays them to the relayer with `payRelayer`. Prints one
//! JSON line per step; the script checks the chain.
//!
//! ```sh
//! cargo run -p onyx-api --features http-client --example credits-rehearsal -- \
//!   RPC PREPAID REGISTRY TOKEN JOURNAL FACT
//! ```
//! Keys come from the environment, made fresh by the script for one run (they hold anvil gas and
//! mock tokens only): ONYX_CREDITS_OBSERVER_KEY, ONYX_CREDITS_RELAYER_KEY,
//! ONYX_CREDITS_REPORTER_KEY, ONYX_CREDITS_RECEIPT_KEY, ONYX_CREDITS_MINER_KEY.

use axum::body::Body;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use onyx_api::credits::{
    receive_digest, relay_routes, routes, sign_account_proof, Asset, ChainCredits, CreditsState,
    FeeSchedule, Relay, RelayPolicy, ACCOUNT_PROOF,
};
use onyx_api::evm_tx::Sender;
use onyx_api::prepaid::{domain_separator, Voucher};
use onyx_api::prepaid_chain::{ChainAccounts, PrepaidReporter};
use onyx_api::prepaid_gate::{voucher_header, PrepaidState, PREPAID_VOUCHER};
use onyx_api::roots::{HttpJsonRpc, JsonRpc};
use onyx_api::x402::HyperPost;
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const CHAIN: u64 = 31_337;
const FEES: &str = "post_commit=1000,post_receipt=1000,post_sealed_reveal=3000,deposit=2000";
const DEPOSIT: u128 = 5_000_000;

fn hexn<const N: usize>(s: &str) -> Result<[u8; N], String> {
    hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| format!("{s}: not {N} bytes"))
}

fn key(var: &str) -> Result<[u8; 32], String> {
    hexn(&std::env::var(var).map_err(|_| format!("{var} is not set"))?)
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn sign_digest(k: &[u8; 32], d: &[u8; 32]) -> Result<[u8; 65], String> {
    let sk = secp256k1::SecretKey::from_slice(k).map_err(|e| e.to_string())?;
    let (id, rs) = secp256k1::Secp256k1::signing_only()
        .sign_ecdsa_recoverable(&secp256k1::Message::from_digest(*d), &sk)
        .serialize_compact();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&rs);
    out[64] = 27 + u8::try_from(id.to_i32()).map_err(|_| "recovery id")?;
    Ok(out)
}

struct Run {
    app: axum::Router,
    domain: [u8; 32],
    observer: [u8; 32],
    total: u128,
}

impl Run {
    async fn call(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let resp = self
            .app
            .clone()
            .oneshot(req.body(body).map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }

    /// Relay `body` paid with the observer's next voucher (`fee` above the last total).
    async fn relay(&mut self, body: Value, fee: u128) -> Result<(u16, Value), String> {
        let v = Voucher::sign(&self.observer, &self.domain, self.total + fee).ok_or("sign")?;
        let r = self
            .call(
                Method::POST,
                "/v1/relay",
                &[(PREPAID_VOUCHER, voucher_header(&v))],
                Some(body),
            )
            .await?;
        if r.0 == 200 {
            self.total += fee;
        }
        Ok(r)
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [rpc, prepaid, registry, token, journal, fact] = args.as_slice() else {
        return Err("usage: credits-rehearsal RPC PREPAID REGISTRY TOKEN JOURNAL FACT".into());
    };
    if !(rpc.starts_with("http://127.0.0.1:") || rpc.starts_with("http://localhost:")) {
        return Err("the rehearsal runs on a local anvil only".into());
    }
    let (prepaid, registry, token) = (
        hexn::<20>(prepaid)?,
        hexn::<20>(registry)?,
        hexn::<20>(token)?,
    );
    let fact = Hash32(hexn::<32>(fact)?);
    let observer = key("ONYX_CREDITS_OBSERVER_KEY")?;
    let relayer = key("ONYX_CREDITS_RELAYER_KEY")?;
    let reporter = key("ONYX_CREDITS_REPORTER_KEY")?;
    let receipt = key("ONYX_CREDITS_RECEIPT_KEY")?;
    let miner = key("ONYX_CREDITS_MINER_KEY")?;
    let me = onyx_api::x402::exact_evm::address(&observer).ok_or("observer key")?;
    let node = || HttpJsonRpc::new(rpc.clone(), HyperPost::for_rpc(Duration::from_secs(10)));
    let mine = |n: u64| {
        let r = node();
        async move { r.call("anvil_mine", json!([format!("0x{n:x}")])).await }
    };
    // A controlled clock: account reads are cached 60 s, so it moves on between steps.
    let now = Arc::new(AtomicU64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    ));
    let n2 = now.clone();
    let clock: onyx_api::Clock = Arc::new(move || n2.load(Ordering::SeqCst));
    let step = |s: u64| now.fetch_add(s, Ordering::SeqCst);
    let domain = domain_separator(CHAIN, &prepaid);
    let books = Arc::new(
        PrepaidState::open(
            journal,
            domain,
            Arc::new(ChainAccounts::new(node(), prepaid)),
            60,
        )?
        .with_hold_limit(1800),
    );
    let ds = node()
        .call("eth_call", json!([{"to": hx(&token), "data": hx(&keccak256(b"DOMAIN_SEPARATOR()").0[..4])}, "latest"]))
        .await?;
    let token_domain = hexn::<32>(ds.as_str().unwrap_or_default())?;
    let asset = Asset {
        symbol: "mUSDC".into(),
        decimals: 6,
        chain_id: CHAIN,
        address: token,
    };
    let sender = Arc::new(Sender::new(node(), CHAIN, relayer).ok_or("relayer key")?);
    let policy = RelayPolicy {
        per_account_per_hour: 4,
        ..RelayPolicy::default()
    };
    let relay = Relay::new(
        sender,
        FeeSchedule::parse(FEES, 1_000_000)?,
        policy,
        Some(registry),
        Some(token_domain),
    );
    let st = CreditsState::new(
        books.clone(),
        Arc::new(ChainCredits::new(node(), prepaid, 0)),
        asset,
        prepaid,
        clock,
    )
    .with_relay(relay)?;
    let ledger = st.ledger().to_owned();
    let st = Arc::new(st);
    let mut run = Run {
        app: routes(st.clone()).merge(relay_routes(st)),
        domain,
        observer,
        total: 0,
    };

    let (s, v) = run
        .call(Method::GET, "/v1/credits/schedule", &[], None)
        .await?;
    println!(
        "{}",
        json!({"step": "schedule", "status": s, "actions": v["actions"], "relayer": v["relayer"]})
    );

    // 1. Deposit with no ETH: the relayer submits the observer's signed authorization; the fee
    //    is paid from the deposit itself.
    let t = now.load(Ordering::SeqCst);
    let nonce = Hash32(keccak256(b"credits demo deposit").0);
    let d = receive_digest(&token_domain, &me, &prepaid, DEPOSIT, 0, t + 3600, &nonce);
    let body = json!({"action": "deposit", "args": {
        "from": hx(&me), "amount": DEPOSIT.to_string(), "validAfter": "0",
        "validBefore": (t + 3600).to_string(), "nonce": hx(&nonce.0),
        "signature": hx(&sign_digest(&observer, &d)?)}});
    let (s, v) = run.relay(body, 2000).await?;
    println!(
        "{}",
        json!({"step": "deposit", "status": s, "tx": v["tx"], "gasUsed": v["gasUsed"], "fee": v["fee"], "charged": v["charged"]})
    );
    mine(3).await?;
    step(61);

    // 2. A commit signed by another key for this observer: refused before anything is sent.
    let round = Hash32(keccak256(b"credits demo round").0);
    let commit = Hash32(keccak256(b"credits demo commit").0);
    let commit_body = |signer: &[u8; 32], c: Hash32| -> Result<Value, String> {
        let m = onyx_coordinator::observe::observation_commit_message(&round, 0, &me, &c);
        let sig = onyx_labels::attest::eip191_sign_with(signer, m.as_bytes()).ok_or("sign")?;
        Ok(json!({"action": "post_commit", "args": {
            "fact": hx(&fact.0), "attempt": 0, "observer": hx(&me), "round": hx(&round.0),
            "commit": hx(&c.0), "signature": hx(&sig)}}))
    };
    let (s, v) = run.relay(commit_body(&miner, commit)?, 1000).await?;
    println!(
        "{}",
        json!({"step": "forged_commit", "status": s, "code": v["error"]["code"]})
    );

    // 3. The observer's own commit, then the receipt for it.
    let (s, v) = run.relay(commit_body(&observer, commit)?, 1000).await?;
    println!(
        "{}",
        json!({"step": "commit", "status": s, "tx": v["tx"], "gasUsed": v["gasUsed"], "fee": v["fee"], "charged": v["charged"]})
    );
    let m = onyx_coordinator::observe::reveal_receipt_message(&round, 0, &me, &commit);
    let rsig = onyx_labels::attest::eip191_sign_with(&receipt, m.as_bytes()).ok_or("sign")?;
    let receipt_body = json!({"action": "post_receipt", "args": {
        "fact": hx(&fact.0), "attempt": 0, "observer": hx(&me), "round": hx(&round.0),
        "commit": hx(&commit.0), "signature": hx(&rsig)}});
    let (s, v) = run.relay(receipt_body, 1000).await?;
    println!(
        "{}",
        json!({"step": "receipt", "status": s, "tx": v["tx"], "gasUsed": v["gasUsed"], "fee": v["fee"], "charged": v["charged"]})
    );

    // 4. The same commit again: its simulation reverts (AlreadyPosted); released, not charged.
    let (s, v) = run.relay(commit_body(&observer, commit)?, 1000).await?;
    println!(
        "{}",
        json!({"step": "repeat_commit", "status": s, "code": v["error"]["code"]})
    );

    // 5. Over the hourly cap (4 counted: deposit, commit, receipt, repeat): refused unsent.
    let other = Hash32(keccak256(b"credits demo commit 2").0);
    let (s, v) = run.relay(commit_body(&observer, other)?, 1000).await?;
    println!(
        "{}",
        json!({"step": "over_cap", "status": s, "code": v["error"]["code"]})
    );
    mine(3).await?;
    step(61);

    // 6. Views: public, then with the accounts' own proofs.
    let t = now.load(Ordering::SeqCst);
    let (s, v) = run
        .call(Method::GET, &format!("/v1/credits/{}", hx(&me)), &[], None)
        .await?;
    println!(
        "{}",
        json!({"step": "observer_public", "status": s, "view": v})
    );
    let proof = sign_account_proof(&observer, t, &ledger).ok_or("proof")?;
    let (s, v) = run
        .call(
            Method::GET,
            &format!("/v1/credits/{}", hx(&me)),
            &[(ACCOUNT_PROOF, proof)],
            None,
        )
        .await?;
    println!(
        "{}",
        json!({"step": "observer_account", "status": s, "view": v})
    );
    let proof = sign_account_proof(&observer, t + 1, &ledger).ok_or("proof")?;
    let (s, v) = run
        .call(
            Method::GET,
            &format!("/v1/credits/{}/lines", hx(&me)),
            &[(ACCOUNT_PROOF, proof)],
            None,
        )
        .await?;
    let kinds: Vec<Value> = v["lines"]
        .as_array()
        .map(|l| l.iter().map(|x| x["kind"].clone()).collect())
        .unwrap_or_default();
    println!(
        "{}",
        json!({"step": "observer_lines", "status": s, "kinds": kinds, "arrivals": v["arrivals"]})
    );
    let (s, v) = run
        .call(
            Method::GET,
            &format!("/v1/credits/{}/lines", hx(&me)),
            &[],
            None,
        )
        .await?;
    println!(
        "{}",
        json!({"step": "lines_without_proof", "status": s, "code": v["error"]["code"]})
    );
    let m_addr = onyx_api::x402::exact_evm::address(&miner).ok_or("miner key")?;
    let proof = sign_account_proof(&miner, t, &ledger).ok_or("proof")?;
    let (s, v) = run
        .call(
            Method::GET,
            &format!("/v1/credits/{}", hx(&m_addr)),
            &[(ACCOUNT_PROOF, proof)],
            None,
        )
        .await?;
    println!(
        "{}",
        json!({"step": "miner_account", "status": s, "view": v})
    );

    // 7. The reporter's pass: redeem the observer's fees and pay them to the relayer.
    let rs = Arc::new(Sender::new(node(), CHAIN, reporter).ok_or("reporter key")?);
    let pass = PrepaidReporter::new(
        books,
        Arc::new(ChainAccounts::latest(node(), prepaid)),
        rs,
        prepaid,
        Duration::from_secs(60),
    )
    .with_relay_cap(1_000_000);
    for i in 1..=2 {
        let p = pass.pass().await?;
        println!(
            "{}",
            json!({"step": "pass", "pass": i, "redeemed": p.redeemed, "relayerPaid": p.relayer_paid.to_string(), "relayerTx": p.relayer_tx, "allocated": p.allocated})
        );
    }
    Ok(())
}
