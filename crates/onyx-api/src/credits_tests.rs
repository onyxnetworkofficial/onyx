//! Tests of the credits routes and the relay (ADR-0056, THREATS CL6). The chain and the relayer's
//! key are fakes: nothing leaves the process.

use super::*;
use crate::prepaid::{domain_separator, Account};
use crate::prepaid_gate::AccountSource;
use axum::body::Body;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use tower::ServiceExt;

const CHAIN: u64 = 84_532;
const PREPAID: [u8; 20] = [0x5a; 20];
const REGISTRY: [u8; 20] = [0x6b; 20];
const TOKEN_DOMAIN: [u8; 32] = [0x7c; 32];
const T0: u64 = 1_790_000_000;
const OBSERVER: [u8; 32] = [7; 32];
const OTHER: [u8; 32] = [8; 32];
const RECEIPT: [u8; 32] = [9; 32];

fn addr(k: &[u8; 32]) -> [u8; 20] {
    x402::exact_evm::address(k).expect("key")
}

fn domain() -> [u8; 32] {
    domain_separator(CHAIN, &PREPAID)
}

struct Src(Mutex<Account>);
impl AccountSource for Src {
    fn account<'a>(&'a self, _: [u8; 20]) -> BoxFuture<'a, Result<Account, String>> {
        let a = *self.0.lock().expect("lock");
        Box::pin(async move { Ok(a) })
    }
}

struct FakeChain {
    account: ChainAccount,
    arrivals: Vec<Arrival>,
}
impl CreditsChain for FakeChain {
    fn account<'a>(&'a self, _: [u8; 20]) -> BoxFuture<'a, Result<ChainAccount, String>> {
        let a = self.account;
        Box::pin(async move { Ok(a) })
    }
    fn arrivals<'a>(&'a self, _: [u8; 20]) -> BoxFuture<'a, Result<Vec<Arrival>, String>> {
        let a = self.arrivals.clone();
        Box::pin(async move { Ok(a) })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Mine,
    SimRevert,
    ChainRevert,
    Pending,
}

struct FakeSender {
    mode: Mutex<Mode>,
    base_fee: AtomicU64,
    gas: u64,
    sims: AtomicU32,
    sends: AtomicU32,
    last: Mutex<Option<([u8; 20], Vec<u8>)>>,
}

impl FakeSender {
    fn new(mode: Mode) -> Arc<FakeSender> {
        Arc::new(FakeSender {
            mode: Mutex::new(mode),
            base_fee: AtomicU64::new(1_000_000),
            gas: 80_000,
            sims: AtomicU32::new(0),
            sends: AtomicU32::new(0),
            last: Mutex::new(None),
        })
    }
    fn mode(&self) -> Mode {
        *self.mode.lock().expect("lock")
    }
}

impl RelaySend for FakeSender {
    fn address(&self) -> [u8; 20] {
        [0x2e; 20]
    }
    fn base_fee<'a>(&'a self) -> BoxFuture<'a, Result<u128, String>> {
        let b = u128::from(self.base_fee.load(Ordering::SeqCst));
        Box::pin(async move { Ok(b) })
    }
    fn simulate<'a>(&'a self, _: [u8; 20], _: Vec<u8>) -> BoxFuture<'a, Result<u64, SimError>> {
        self.sims.fetch_add(1, Ordering::SeqCst);
        let r = if self.mode() == Mode::SimRevert {
            Err(SimError::Revert("execution reverted: AlreadyPosted".into()))
        } else {
            Ok(self.gas)
        };
        Box::pin(async move { r })
    }
    fn send<'a>(
        &'a self,
        to: [u8; 20],
        data: Vec<u8>,
        _: Duration,
    ) -> BoxFuture<'a, Result<Mined, SendError>> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().expect("lock") = Some((to, data));
        let h = format!("0x{}", "ab".repeat(32));
        let r = match self.mode() {
            Mode::Mine | Mode::SimRevert => Ok(Mined {
                hash: h,
                block: 42,
                gas_used: 70_000,
                effective_gas_price: 1_000_000,
            }),
            Mode::ChainRevert => Err(SendError::Reverted(h)),
            Mode::Pending => Err(SendError::Pending(h)),
        };
        Box::pin(async move { r })
    }
}

struct Fx {
    st: Arc<CreditsState>,
    pp: Arc<PrepaidState>,
    sender: Arc<FakeSender>,
    now: Arc<AtomicU64>,
}

fn schedule() -> FeeSchedule {
    FeeSchedule::parse(
        "post_commit=1000,post_receipt=1000,post_sealed_reveal=3000,deposit=2000",
        1_000_000,
    )
    .expect("schedule")
}

fn fixture_with(deposited: u128, mode: Mode, policy: RelayPolicy) -> Fx {
    let dir = crate::journal::test_dir("credits");
    let src = Arc::new(Src(Mutex::new(Account {
        deposited,
        redeemed: 0,
        exiting: false,
    })));
    let pp = Arc::new(
        PrepaidState::open(dir.join("prepaid.journal"), domain(), src, 60)
            .expect("open")
            .with_hold_limit(1800),
    );
    let now = Arc::new(AtomicU64::new(T0));
    let n = now.clone();
    let clock: Clock = Arc::new(move || n.load(Ordering::SeqCst));
    let chain = Arc::new(FakeChain {
        account: ChainAccount {
            deposited,
            redeemed: 0,
            exit_at: 0,
        },
        arrivals: vec![
            Arrival {
                origin: 1,
                amount: deposited.saturating_sub(800),
                block: 10,
                tx: Hash32([1; 32]),
            },
            Arrival {
                origin: 2,
                amount: deposited.min(800),
                block: 11,
                tx: Hash32([2; 32]),
            },
        ],
    });
    let asset = Asset {
        symbol: "TEST".into(),
        decimals: 6,
        chain_id: CHAIN,
        address: [0x7d; 20],
    };
    let sender = FakeSender::new(mode);
    let relay = Relay::new(
        sender.clone(),
        schedule(),
        policy,
        Some(REGISTRY),
        Some(TOKEN_DOMAIN),
    );
    let st = CreditsState::new(pp.clone(), chain, asset, PREPAID, clock)
        .with_relay(relay)
        .expect("relay");
    Fx {
        st: Arc::new(st),
        pp,
        sender,
        now,
    }
}

fn fixture(deposited: u128, mode: Mode) -> Fx {
    fixture_with(deposited, mode, RelayPolicy::default())
}

async fn call(
    fx: &Fx,
    method: Method,
    uri: &str,
    headers: &[(&str, String)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let verb = method.to_string();
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    let resp = routes(fx.st.clone())
        .merge(relay_routes(fx.st.clone()))
        .oneshot(req.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    crate::openapi_check::check(&verb, uri, status.as_u16(), &v);
    (status, v)
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn commit_body(signer: &[u8; 32], observer: [u8; 20], commit: Hash32) -> Value {
    let round = Hash32([3; 32]);
    let m = onyx_coordinator::observe::observation_commit_message(&round, 0, &observer, &commit);
    let sig = onyx_labels::attest::eip191_sign_with(signer, m.as_bytes()).expect("sign");
    json!({"action": "post_commit", "args": {
        "fact": hx(&[4; 32]), "attempt": 0, "observer": hx(&observer), "round": hx(&round.0),
        "commit": hx(&commit.0), "signature": hx(&sig)}})
}

fn voucher(key: &[u8; 32], total: u128) -> (&'static str, String) {
    let v = Voucher::sign(key, &domain(), total).expect("sign");
    (
        prepaid_gate::PREPAID_VOUCHER,
        prepaid_gate::voucher_header(&v),
    )
}

async fn relay_commit(fx: &Fx, total: u128, commit: u8) -> (StatusCode, Value) {
    let me = addr(&OBSERVER);
    call(
        fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OBSERVER, total)],
        Some(commit_body(&OBSERVER, me, Hash32([commit; 32]))),
    )
    .await
}

/// THREATS CL6-2: an action the actor did not sign is refused before anything is held,
/// simulated or sent: a commit signed by another key, a receipt "signed" by the observer
/// itself, a deposit signed by another account; and the voucher must be the actor's.
#[tokio::test]
async fn the_relayer_refuses_an_action_without_the_actors_signature() {
    let fx = fixture(10_000, Mode::Mine);
    let me = addr(&OBSERVER);
    // A commit signed by someone else for this observer.
    let (s, v) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OBSERVER, 1000)],
        Some(commit_body(&OTHER, me, Hash32([5; 32]))),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("relay_not_signed"))
    );
    // A receipt signed by the observer itself (not a receipt signer).
    let round = Hash32([3; 32]);
    let commit = Hash32([5; 32]);
    let m = onyx_coordinator::observe::reveal_receipt_message(&round, 0, &me, &commit);
    let own = onyx_labels::attest::eip191_sign_with(&OBSERVER, m.as_bytes()).expect("sign");
    let body = json!({"action": "post_receipt", "args": {
        "fact": hx(&[4; 32]), "attempt": 0, "observer": hx(&me), "round": hx(&round.0),
        "commit": hx(&commit.0), "signature": hx(&own)}});
    let (s, _) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OBSERVER, 1000)],
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A deposit authorization signed by another key for this account.
    let d = receive_digest(
        &TOKEN_DOMAIN,
        &me,
        &PREPAID,
        5000,
        0,
        T0 + 600,
        &Hash32([6; 32]),
    );
    let forged = sign_digest(&OTHER, &d);
    let body = json!({"action": "deposit", "args": {
        "from": hx(&me), "amount": "5000", "validAfter": "0", "validBefore": (T0 + 600).to_string(),
        "nonce": hx(&[6; 32]), "signature": hx(&forged)}});
    let (s, _) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OBSERVER, 2000)],
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A correctly signed commit, paid by another account's voucher.
    let (s, v) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OTHER, 1000)],
        Some(commit_body(&OBSERVER, me, Hash32([5; 32]))),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("relay_payer_mismatch"))
    );
    assert_eq!(
        fx.sender.sims.load(Ordering::SeqCst),
        0,
        "nothing simulated"
    );
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 0, "nothing sent");
    assert!(fx.pp.usage().expect("usage").is_empty(), "nothing charged");
    assert!(fx.pp.holds().is_empty(), "nothing held");
}

/// A mined relay charges exactly the scheduled fee, as a gas line the reporter pays to the
/// relayer, and the calldata is the registry's `postCommit` with the observer's own signature.
#[tokio::test]
async fn a_mined_relay_charges_exactly_the_scheduled_fee() {
    let fx = fixture(10_000, Mode::Mine);
    // Without a voucher: 402 with the fee.
    let me = addr(&OBSERVER);
    let (s, v) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[],
        Some(commit_body(&OBSERVER, me, Hash32([5; 32]))),
    )
    .await;
    assert_eq!(
        (s, v["fee"].as_str()),
        (StatusCode::PAYMENT_REQUIRED, Some("1000"))
    );
    // A voucher adding more or less than the fee is refused before anything is sent.
    for wrong in [999u128, 1001] {
        let (s, v) = relay_commit(&fx, wrong, 5).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
        );
    }
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 0);
    let (s, v) = relay_commit(&fx, 1000, 5).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (
            v["fee"].as_str(),
            v["charged"].as_bool(),
            v["total"].as_str()
        ),
        (Some("1000"), Some(true), Some("1000"))
    );
    assert_eq!(v["spendable"].as_str(), Some("9000"));
    let (to, data) = fx.sender.last.lock().expect("lock").clone().expect("sent");
    assert_eq!(to, REGISTRY);
    assert_eq!(
        &data[..4],
        &keccak256(b"postCommit(bytes32,uint32,address,bytes32,bytes32,bytes)").0[..4]
    );
    assert_eq!(
        data.len(),
        4 + 6 * 32 + 32 + 96,
        "6 heads, the length and 65 bytes padded"
    );
    let usage = fx.pp.usage().expect("usage");
    assert_eq!(usage.len(), 1);
    assert!(
        prepaid_gate::is_gas_bill(usage[0].round, &usage[0].shard),
        "a gas line, not a book"
    );
    assert_eq!(usage[0].price, 1000);
    // The identical request again: its voucher is spent (the total moved on).
    let (s, _) = relay_commit(&fx, 1000, 5).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 1);
}

/// THREATS CL6-3: a relay that reverts on chain, would revert, or is not mined in time
/// releases its hold: nothing is charged and the balance is whole again.
#[tokio::test]
async fn a_reverted_relay_releases_its_hold() {
    for (mode, status, code) in [
        (
            Mode::ChainRevert,
            StatusCode::UNPROCESSABLE_ENTITY,
            "relay_reverted",
        ),
        (
            Mode::SimRevert,
            StatusCode::UNPROCESSABLE_ENTITY,
            "relay_would_revert",
        ),
        (Mode::Pending, StatusCode::GATEWAY_TIMEOUT, "relay_pending"),
    ] {
        let fx = fixture(10_000, mode);
        let (s, v) = relay_commit(&fx, 1000, 5).await;
        assert_eq!((s, v["error"]["code"].as_str()), (status, Some(code)));
        assert!(
            fx.pp.usage().expect("usage").is_empty(),
            "{code}: nothing charged"
        );
        assert!(fx.pp.holds().is_empty(), "{code}: the hold is released");
        let book = fx.pp.book(&addr(&OBSERVER)).expect("book");
        assert_eq!((book.total(), book.spendable()), (0, 10_000), "{code}");
        if mode == Mode::SimRevert {
            assert_eq!(
                fx.sender.sends.load(Ordering::SeqCst),
                0,
                "a failed simulation sends nothing"
            );
        }
        // The account's lines show the hold and its release, nothing charged.
        let proof = sign_account_proof(&OBSERVER, T0, fx.st.ledger()).expect("proof");
        let (s, v) = call(
            &fx,
            Method::GET,
            &format!("/v1/credits/{}/lines", hx(&addr(&OBSERVER))),
            &[(ACCOUNT_PROOF, proof)],
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let kinds: Vec<&str> = v["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .map(|l| l["kind"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(kinds, ["relay_hold", "release"], "{code}");
        assert_eq!(v["lines"][1]["amount"].as_str(), Some("1000"));
        // The next voucher starts again from the total before the hold.
        *fx.sender.mode.lock().expect("lock") = Mode::Mine;
        let (s, _) = relay_commit(&fx, 1000, 6).await;
        assert_eq!(s, StatusCode::OK, "{code}");
    }
}

/// THREATS CL6-4: relays over the hourly cap, after too many failures, above the base-fee cap or
/// past the daily gas budget are refused before anything is sent.
#[tokio::test]
async fn relays_over_the_hourly_cap_are_refused_before_sending() {
    let policy = RelayPolicy {
        per_account_per_hour: 2,
        ..RelayPolicy::default()
    };
    let fx = fixture_with(10_000, Mode::Mine, policy);
    assert_eq!(relay_commit(&fx, 1000, 1).await.0, StatusCode::OK);
    assert_eq!(relay_commit(&fx, 2000, 2).await.0, StatusCode::OK);
    let (s, v) = relay_commit(&fx, 3000, 3).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("relay_rate_limited"))
    );
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 2);
    // An hour later the account may relay again.
    fx.now.fetch_add(3600, Ordering::SeqCst);
    assert_eq!(relay_commit(&fx, 3000, 3).await.0, StatusCode::OK);

    // Failures: after `reverts_per_day` the account is refused until the next day.
    let fx = fixture(10_000, Mode::SimRevert);
    for i in 0..3u8 {
        assert_eq!(
            relay_commit(&fx, 1000, 10 + i).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let sims = fx.sender.sims.load(Ordering::SeqCst);
    assert_eq!(
        relay_commit(&fx, 1000, 20).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        fx.sender.sims.load(Ordering::SeqCst),
        sims,
        "refused before simulating"
    );

    // Base fee above the cap: 503, nothing sent, nothing charged.
    let fx = fixture(10_000, Mode::Mine);
    fx.sender.base_fee.store(200_000_000, Ordering::SeqCst);
    let (s, v) = relay_commit(&fx, 1000, 1).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("relay_busy"))
    );
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 0);
    assert!(fx.pp.usage().expect("usage").is_empty());

    // The daily gas budget: one relay's estimate (80,000 + 25%) fits a 150,000 budget once.
    let policy = RelayPolicy {
        daily_gas_budget: 150_000,
        ..RelayPolicy::default()
    };
    let fx = fixture_with(10_000, Mode::Mine, policy);
    assert_eq!(relay_commit(&fx, 1000, 1).await.0, StatusCode::OK);
    let (s, _) = relay_commit(&fx, 2000, 2).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 1);
    fx.now.fetch_add(86_400, Ordering::SeqCst);
    assert_eq!(
        relay_commit(&fx, 2000, 2).await.0,
        StatusCode::OK,
        "a new day, a new budget"
    );
}

/// R-CR-7: no relay without spendable credit; an exit pending stops relays too.
#[tokio::test]
async fn no_relay_without_spendable_credit() {
    let fx = fixture(999, Mode::Mine);
    let (s, v) = relay_commit(&fx, 1000, 1).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
    );
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 0);
}

/// A relayed deposit may pay its own fee from the deposit it makes (an account with no ETH and
/// no credit yet), never more than deposit plus the amount paid in.
#[tokio::test]
async fn a_relayed_deposit_pays_its_fee_from_what_it_pays_in() {
    let fx = fixture(0, Mode::Mine);
    let me = addr(&OBSERVER);
    let body = |amount: u128, nonce: u8| {
        let d = receive_digest(
            &TOKEN_DOMAIN,
            &me,
            &PREPAID,
            amount,
            0,
            T0 + 600,
            &Hash32([nonce; 32]),
        );
        json!({"action": "deposit", "args": {
            "from": hx(&me), "amount": amount.to_string(), "validAfter": "0",
            "validBefore": (T0 + 600).to_string(), "nonce": hx(&[nonce; 32]),
            "signature": hx(&sign_digest(&OBSERVER, &d))}})
    };
    // A deposit of 1,000 cannot pay a 2,000 fee.
    let (s, _) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OBSERVER, 2000)],
        Some(body(1000, 1)),
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    let (s, v) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OBSERVER, 2000)],
        Some(body(5000, 2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (to, data) = fx.sender.last.lock().expect("lock").clone().expect("sent");
    assert_eq!(to, PREPAID);
    assert_eq!(
        &data[..4],
        &keccak256(b"depositWithAuthorization(address,uint128,uint256,uint256,bytes32,bytes)").0
            [..4]
    );
}

/// THREATS CL6-5: an account's lines and its journal numbers need its own fresh, unused proof;
/// without one the account view shows only what the chain shows.
#[tokio::test]
async fn credits_lines_need_the_accounts_own_fresh_proof() {
    let fx = fixture(10_000, Mode::Mine);
    assert_eq!(relay_commit(&fx, 1000, 1).await.0, StatusCode::OK);
    let me = hx(&addr(&OBSERVER));
    let lines = format!("/v1/credits/{me}/lines");
    let (s, v) = call(&fx, Method::GET, &lines, &[], None).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("account_proof_required"))
    );
    // Another account's proof, an old one, one for another ledger: refused.
    let other = sign_account_proof(&OTHER, T0, fx.st.ledger()).expect("proof");
    let old = sign_account_proof(&OBSERVER, T0 - 121, fx.st.ledger()).expect("proof");
    let elsewhere = sign_account_proof(
        &OBSERVER,
        T0,
        "eip155:84532:0x0000000000000000000000000000000000000001",
    )
    .expect("proof");
    for p in [other, old, elsewhere] {
        let (s, v) = call(&fx, Method::GET, &lines, &[(ACCOUNT_PROOF, p)], None).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("invalid_account_proof"))
        );
    }
    let good = sign_account_proof(&OBSERVER, T0, fx.st.ledger()).expect("proof");
    let (s, v) = call(
        &fx,
        Method::GET,
        &lines,
        &[(ACCOUNT_PROOF, good.clone())],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v["lines"].as_array().map(Vec::len),
        Some(2),
        "relay hold, gas"
    );
    assert_eq!(v["lines"][1]["kind"].as_str(), Some("gas"));
    assert_eq!(v["lines"][1]["action"].as_str(), Some("post_commit"));
    assert_eq!(v["arrivals"].as_array().map(Vec::len), Some(2));
    // Single use.
    let (s, _) = call(&fx, Method::GET, &lines, &[(ACCOUNT_PROOF, good)], None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The public view: chain numbers only.
    let (s, v) = call(&fx, Method::GET, &format!("/v1/credits/{me}"), &[], None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["view"].as_str(), Some("public"));
    assert!(
        v.get("spent").is_none() && v.get("credited").is_none() && v.get("spendable").is_none()
    );
    // The account's own view.
    fx.now.fetch_add(1, Ordering::SeqCst);
    let p = sign_account_proof(&OBSERVER, T0 + 1, fx.st.ledger()).expect("proof");
    let (s, v) = call(
        &fx,
        Method::GET,
        &format!("/v1/credits/{me}"),
        &[(ACCOUNT_PROOF, p)],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["view"].as_str(), Some("account"));
    assert_eq!(v["spent"].as_str(), Some("1000"));
    assert_eq!(v["spendable"].as_str(), Some("9000"));
    assert_eq!(v["credited"]["earned"].as_str(), Some("800"));
    // A mixed-case or short address is not an account.
    let (s, _) = call(&fx, Method::GET, "/v1/credits/0xABCD", &[], None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Fee-schedule abuse: fees are positive, bounded, one per action, published with a hash; a
/// voucher for an old fee after a change is refused, nothing sent.
#[tokio::test]
async fn the_fee_schedule_is_bounded_and_a_changed_fee_is_never_charged() {
    assert_eq!(
        FeeSchedule::parse("post_commit=0", 10),
        Err(ScheduleError::OutOfRange {
            action: "post_commit",
            max: 10
        })
    );
    assert!(FeeSchedule::parse("post_commit=11", 10).is_err());
    assert!(FeeSchedule::parse("post_commit=1,post_commit=2", 10).is_err());
    assert!(FeeSchedule::parse("transfer=1", 10).is_err());
    assert!(FeeSchedule::parse("post_commit=-1", 10).is_err());
    assert!(FeeSchedule::parse("post_commit=1e3", 10_000).is_err());
    assert!(FeeSchedule::parse("", 10).is_err());
    let a = FeeSchedule::parse("post_commit=5,deposit=7", 10).expect("ok");
    let b = FeeSchedule::parse("deposit=7, post_commit=5", 10).expect("ok");
    assert_eq!(a.hash(), b.hash(), "order does not change the schedule");
    assert_ne!(
        a.hash(),
        FeeSchedule::parse("post_commit=6,deposit=7", 10)
            .expect("ok")
            .hash()
    );
    // The published schedule is what is charged.
    let fx = fixture(10_000, Mode::Mine);
    let (s, v) = call(&fx, Method::GET, "/v1/credits/schedule", &[], None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["actions"][0]["action"].as_str(), Some("post_commit"));
    assert_eq!(v["actions"][0]["fee"].as_str(), Some("1000"));
    assert_eq!(
        v["scheduleHash"].as_str().map(str::to_owned),
        Some(schedule().hash().to_string())
    );
    assert!(
        v.to_string().to_lowercase().find("usdc").is_none(),
        "no field names the asset"
    );
    // A client that saw an older fee of 900 signs 900: refused, nothing sent or charged.
    let (s, _) = relay_commit(&fx, 900, 1).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 0);
}

/// A relay hold left pending by a stop is released when the relay starts again.
#[tokio::test]
async fn a_stale_relay_hold_is_released_at_start() {
    let dir = crate::journal::test_dir("credits-stale");
    let src = Arc::new(Src(Mutex::new(Account {
        deposited: 5000,
        redeemed: 0,
        exiting: false,
    })));
    let pp = Arc::new(
        PrepaidState::open(dir.join("j"), domain(), src, 60)
            .expect("open")
            .with_hold_limit(1800),
    );
    pp.refresh(addr(&OBSERVER), T0).await.expect("read");
    let held = Held {
        ticket: Hash32([9; 32]),
        metric: RELAY_HOLD_METRIC.into(),
        subject: "post_commit".into(),
        deadline: T0 + 100,
        created: T0,
    };
    pp.hold(
        Voucher::sign(&OBSERVER, &domain(), 1000).expect("v"),
        1000,
        held,
        T0,
    )
    .expect("held");
    let clock: Clock = Arc::new(|| T0 + 5);
    let chain = Arc::new(FakeChain {
        account: ChainAccount::default(),
        arrivals: Vec::new(),
    });
    let asset = Asset {
        symbol: "TEST".into(),
        decimals: 6,
        chain_id: CHAIN,
        address: [0; 20],
    };
    let relay = Relay::new(
        FakeSender::new(Mode::Mine),
        schedule(),
        RelayPolicy::default(),
        Some(REGISTRY),
        None,
    );
    let _st = CreditsState::new(pp.clone(), chain, asset, PREPAID, clock)
        .with_relay(relay)
        .expect("relay");
    assert!(pp.holds().is_empty());
    assert_eq!(pp.book(&addr(&OBSERVER)).expect("book").spendable(), 5000);
}

/// Parsing is strict: unknown or missing args, uppercase hex, oversize ciphertext, zero values.
#[test]
fn relay_bodies_parse_in_one_shape_only() {
    let me = addr(&OBSERVER);
    let good = commit_body(&OBSERVER, me, Hash32([5; 32]));
    assert!(RelayAction::parse(&good).is_ok());
    let mut extra = good.clone();
    extra["args"]["value"] = json!("1");
    assert!(RelayAction::parse(&extra).is_err());
    let mut upper = good.clone();
    upper["args"]["fact"] = json!(format!("0x{}", "AB".repeat(32)));
    assert!(RelayAction::parse(&upper).is_err());
    let mut zero = good.clone();
    zero["args"]["commit"] = json!(hx(&[0; 32]));
    assert!(RelayAction::parse(&zero).is_err());
    let mut more = good.clone();
    more["extra"] = json!(1);
    assert!(RelayAction::parse(&more).is_err());
    let big = json!({"action": "post_sealed_reveal", "args": {
        "fact": hx(&[1; 32]), "attempt": 0, "observer": hx(&me), "round": hx(&[2; 32]),
        "ex": hx(&[3; 32]), "ey": hx(&[4; 32]), "ciphertext": hx(&vec![1u8; MAX_SEALED_BYTES + 1]),
        "keyProof": hx(&[5; 52]), "signature": hx(&[6; 65])}});
    assert!(RelayAction::parse(&big).is_err());
    assert!(RelayAction::parse(&json!({"action": "transfer", "args": {}})).is_err());
}

/// The relay's calldata is the ABI's (dynamic `bytes` after the heads).
#[test]
fn relay_calldata_matches_the_abi() {
    let a = RelayAction::Deposit {
        from: [0x11; 20],
        amount: 5000,
        valid_after: 0,
        valid_before: 99,
        nonce: Hash32([0x22; 32]),
        sig: [0x33; 65],
    };
    let d = a.calldata();
    assert_eq!(d.len(), 4 + 6 * 32 + 32 + 96);
    assert_eq!(
        &d[4 + 5 * 32..4 + 6 * 32],
        &word(6 * 32),
        "offset of the signature"
    );
    assert_eq!(&d[4 + 6 * 32..4 + 7 * 32], &word(65));
    assert_eq!(&d[4 + 7 * 32..4 + 7 * 32 + 65], &[0x33; 65][..]);
    assert_eq!(&d[4 + 32..4 + 64], &word(5000));
}

fn sign_digest(key: &[u8; 32], digest: &[u8; 32]) -> [u8; 65] {
    let sk = secp256k1::SecretKey::from_slice(key).expect("key");
    let (id, rs) = secp256k1::Secp256k1::signing_only()
        .sign_ecdsa_recoverable(&secp256k1::Message::from_digest(*digest), &sk)
        .serialize_compact();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&rs);
    out[64] = 27 + u8::try_from(id.to_i32()).expect("id");
    out
}

/// A receipt signed by a receipt signer for the observer is relayed, paid by the observer (the
/// chain checks the signer is the draw's).
#[tokio::test]
async fn a_receipt_for_the_observer_is_relayed_and_paid_by_it() {
    let fx = fixture(10_000, Mode::Mine);
    let me = addr(&OBSERVER);
    let round = Hash32([3; 32]);
    let commit = Hash32([5; 32]);
    let m = onyx_coordinator::observe::reveal_receipt_message(&round, 0, &me, &commit);
    let sig = onyx_labels::attest::eip191_sign_with(&RECEIPT, m.as_bytes()).expect("sign");
    let body = json!({"action": "post_receipt", "args": {
        "fact": hx(&[4; 32]), "attempt": 0, "observer": hx(&me), "round": hx(&round.0),
        "commit": hx(&commit.0), "signature": hx(&sig)}});
    let (s, v) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OBSERVER, 1000)],
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, data) = fx.sender.last.lock().expect("lock").clone().expect("sent");
    assert_eq!(
        &data[..4],
        &keccak256(b"postReceipt(bytes32,uint32,address,bytes32,bytes32,bytes)").0[..4]
    );
}

/// Review CL6-R1-2: one account's relays may use at most its share of the day's gas; another
/// account still gets its own.
#[tokio::test]
async fn one_account_cannot_spend_the_whole_daily_gas_budget() {
    let policy = RelayPolicy {
        account_gas_per_day: 150_000,
        ..RelayPolicy::default()
    };
    let fx = fixture_with(10_000, Mode::Mine, policy);
    assert_eq!(relay_commit(&fx, 1000, 1).await.0, StatusCode::OK);
    let (s, v) = relay_commit(&fx, 2000, 2).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("relay_busy"))
    );
    assert_eq!(fx.sender.sends.load(Ordering::SeqCst), 1);
    // Another observer, its own share.
    let other = addr(&OTHER);
    let (s, _) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[voucher(&OTHER, 1000)],
        Some(commit_body(&OTHER, other, Hash32([7; 32]))),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

/// Review CL6-R1-3: a deposit authorization not valid yet, or expiring before its transaction
/// can be mined, is refused before anything is held or sent.
#[tokio::test]
async fn a_deposit_expiring_before_it_can_be_mined_is_refused() {
    let fx = fixture(0, Mode::Mine);
    let me = addr(&OBSERVER);
    for (after, before) in [(0u64, T0 + 100), (T0, T0 + 600), (T0 + 5, T0 + 600)] {
        let d = receive_digest(
            &TOKEN_DOMAIN,
            &me,
            &PREPAID,
            5000,
            after,
            before,
            &Hash32([6; 32]),
        );
        let body = json!({"action": "deposit", "args": {
            "from": hx(&me), "amount": "5000", "validAfter": after.to_string(),
            "validBefore": before.to_string(), "nonce": hx(&[6; 32]),
            "signature": hx(&sign_digest(&OBSERVER, &d))}});
        let (s, v) = call(
            &fx,
            Method::POST,
            "/v1/relay",
            &[voucher(&OBSERVER, 2000)],
            Some(body),
        )
        .await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_relay")),
            "{after} {before}"
        );
    }
    assert_eq!(fx.sender.sims.load(Ordering::SeqCst), 0);
    assert!(fx.pp.holds().is_empty() && fx.pp.usage().expect("usage").is_empty());
}

/// Review CL6-R1-4: a voucher header naming an account but not signed by it spends none of the
/// account's relays.
#[tokio::test]
async fn a_forged_voucher_does_not_spend_the_actors_relays() {
    let policy = RelayPolicy {
        per_account_per_hour: 1,
        ..RelayPolicy::default()
    };
    let fx = fixture_with(10_000, Mode::Mine, policy);
    let me = addr(&OBSERVER);
    let forged = Voucher {
        buyer: me,
        ..Voucher::sign(&OTHER, &domain(), 1000).expect("sign")
    };
    for _ in 0..3 {
        let (s, v) = call(
            &fx,
            Method::POST,
            "/v1/relay",
            &[(
                prepaid_gate::PREPAID_VOUCHER,
                prepaid_gate::voucher_header(&forged),
            )],
            Some(commit_body(&OBSERVER, me, Hash32([5; 32]))),
        )
        .await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
        );
    }
    assert_eq!(
        relay_commit(&fx, 1000, 5).await.0,
        StatusCode::OK,
        "the account's one relay is intact"
    );
}

/// Review CL6-R1-5: an unsigned voucher learns nothing about the account's total or holds.
#[tokio::test]
async fn an_unsigned_voucher_learns_nothing_about_the_total() {
    let fx = fixture(10_000, Mode::Mine);
    assert_eq!(relay_commit(&fx, 1000, 1).await.0, StatusCode::OK);
    let me = addr(&OBSERVER);
    let forged = Voucher {
        buyer: me,
        ..Voucher::sign(&OTHER, &domain(), 5).expect("sign")
    };
    let (s, v) = call(
        &fx,
        Method::POST,
        "/v1/relay",
        &[(
            prepaid_gate::PREPAID_VOUCHER,
            prepaid_gate::voucher_header(&forged),
        )],
        Some(commit_body(&OBSERVER, me, Hash32([6; 32]))),
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert!(v.get("last").is_none() && v.get("price").is_none(), "{v}");
    assert!(!v.to_string().contains("1000"), "{v}");
}
