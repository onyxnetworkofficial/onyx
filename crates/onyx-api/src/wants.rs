//! The bounty board over HTTP (north-star loop NL9, ROADMAP S10, docs/spec/bounty-board.md).
//!
//! - `GET /v1/wants`: the categories shown this week ([`onyx_intake::bounty::Board::published`]):
//!   each held by at least [`MIN_ACCOUNTS`] prepaid accounts, as a bucket, unchanged until the
//!   next weekly cut (cached until then).
//! - `POST /v1/wants`: `{"want": {kind, domain, region, cadence}, "buyer": "0x…", "signature":
//!   "0x…"}`. The buyer signs [`want_message`] for this week (EIP-191) and must hold an open
//!   `OnyxPrepaid` deposit, read on chain and never charged: a deposit is the identity that makes
//!   many accounts cost something (spec assumption 1). Anything but the four closed-list values
//!   is refused before the signature is read (400 `bounty_too_specific`, R-BNT-1).
//!
//! The account is stored only as `keccak256(salt ‖ buyer)` with a node secret salt
//! (`wants.salt`, made once in the state directory) and no route returns it (R-BNT-3). Each
//! accepted want is appended to `wants.journal` and synced before the answer. Without a prepaid
//! contract (`ONYX_PREPAID` unset) the board is shown and posting answers 503 `wants_closed`.

use crate::prepaid_gate::AccountSource;
use crate::ApiError;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use onyx_core::{keccak256, Hash32};
use onyx_intake::bounty::{
    parse_want, want_message, week_of, Board, BountyError, Want, MIN_ACCOUNTS,
};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The largest POST body.
pub const MAX_WANTS_BODY: usize = 1_024;
/// The salt file in the state directory.
pub const SALT_FILE: &str = "wants.salt";
/// The journal file in the state directory.
pub const JOURNAL_FILE: &str = "wants.journal";

/// The board, its journal and the account check.
pub struct WantsStore {
    board: Mutex<Board>,
    journal: PathBuf,
    salt: [u8; 32],
    accounts: Option<Arc<dyn AccountSource>>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

fn read_salt(dir: &Path) -> Result<[u8; 32], String> {
    let path = dir.join(SALT_FILE);
    match std::fs::read(&path) {
        Ok(b) => b
            .try_into()
            .map_err(|_| format!("{}: not 32 bytes", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut salt = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut salt))
                .map_err(|e| format!("random salt: {e}"))?;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            f.write_all(&salt)
                .and_then(|()| f.sync_all())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(salt)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

impl WantsStore {
    /// Open the board in `state_dir` (salt and journal, made on first use), replaying the
    /// journal. `accounts`: the prepaid contract's account reader, or `None` (posting closed).
    pub fn open(
        state_dir: &Path,
        accounts: Option<Arc<dyn AccountSource>>,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
        let salt = read_salt(state_dir)?;
        let journal = state_dir.join(JOURNAL_FILE);
        let mut board = Board::new();
        match std::fs::read_to_string(&journal) {
            Ok(text) => {
                for (n, line) in text.lines().enumerate() {
                    let v: Value = serde_json::from_str(line)
                        .map_err(|e| format!("{}:{}: {e}", journal.display(), n + 1))?;
                    let account = v["account"]
                        .as_str()
                        .and_then(|s| parse_hex::<32>(&Value::from(s)).map(Hash32))
                        .ok_or(format!("{}:{}: account", journal.display(), n + 1))?;
                    let want: Want = serde_json::from_value(v["want"].clone())
                        .map_err(|e| format!("{}:{}: {e}", journal.display(), n + 1))?;
                    let at =
                        v["at"]
                            .as_u64()
                            .ok_or(format!("{}:{}: at", journal.display(), n + 1))?;
                    // A line the board now refuses (a lower quota) is skipped, never fatal.
                    let _ = board.add(account, want, at);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", journal.display())),
        }
        Ok(WantsStore {
            board: Mutex::new(board),
            journal,
            salt,
            accounts,
            clock,
        })
    }

    /// The salted account hash of `buyer`.
    pub fn account_hash(&self, buyer: &[u8; 20]) -> Hash32 {
        let mut b = Vec::with_capacity(52);
        b.extend_from_slice(&self.salt);
        b.extend_from_slice(buyer);
        keccak256(&b)
    }

    /// The board as shown now.
    pub fn shown(&self) -> Value {
        let now = (self.clock)();
        let board = self
            .board
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        json!({
            "minAccounts": MIN_ACCOUNTS,
            "week": week_of(now),
            "nextCut": Board::next_cut(now),
            "categories": board.published(MIN_ACCOUNTS, now),
        })
    }

    fn append(&self, account: &Hash32, want: &Want, at: u64) -> Result<(), String> {
        let line = json!({"account": account.to_string(), "want": want, "at": at});
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.journal)
            .map_err(|e| e.to_string())?;
        writeln!(f, "{line}")
            .and_then(|()| f.sync_all())
            .map_err(|e| e.to_string())
    }
}

/// `GET /v1/wants` and `POST /v1/wants`.
pub fn routes(store: Arc<WantsStore>) -> Router {
    Router::new()
        .route("/v1/wants", get(board))
        .route("/v1/wants", post(post_want))
        .with_state(store)
}

async fn board(State(store): State<Arc<WantsStore>>) -> Response {
    let body = store.shown();
    let now = (store.clock)();
    let left = Board::next_cut(now).saturating_sub(now).max(1);
    let mut r = Json(body).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("public, max-age={left}")) {
        r.headers_mut().insert(header::CACHE_CONTROL, v);
    }
    r
}

fn bad(status: StatusCode, code: &'static str, msg: impl Into<String>) -> Response {
    ApiError::new(status, code, msg).into_response()
}

fn parse_hex<const N: usize>(v: &Value) -> Option<[u8; N]> {
    let s = v.as_str()?.strip_prefix("0x")?;
    hex::decode(s).ok()?.try_into().ok()
}

async fn post_want(State(store): State<Arc<WantsStore>>, body: Body) -> Response {
    let Ok(bytes) = axum::body::to_bytes(body, MAX_WANTS_BODY).await else {
        return bad(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            format!("a want is at most {MAX_WANTS_BODY} bytes"),
        );
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return bad(
            StatusCode::BAD_REQUEST,
            "bounty_too_specific",
            BountyError::TooSpecific.to_string(),
        );
    };
    // Exactly want, buyer, signature: nothing else rides along (R-BNT-1).
    let keys: Vec<&str> = v
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if keys != ["buyer", "signature", "want"] {
        return bad(
            StatusCode::BAD_REQUEST,
            "bounty_too_specific",
            BountyError::TooSpecific.to_string(),
        );
    }
    let want = match parse_want(v["want"].to_string().as_bytes()) {
        Ok(w) => w,
        Err(e @ BountyError::BadValue(_)) => {
            return bad(StatusCode::BAD_REQUEST, "bad_want_value", e.to_string())
        }
        Err(e) => {
            return bad(
                StatusCode::BAD_REQUEST,
                "bounty_too_specific",
                e.to_string(),
            )
        }
    };
    let (Some(buyer), Some(signature)) = (
        parse_hex::<20>(&v["buyer"]),
        parse_hex::<65>(&v["signature"]),
    ) else {
        return bad(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "buyer is 0x and 20 bytes, signature 0x and 65 bytes",
        );
    };
    let Some(accounts) = store.accounts.clone() else {
        return bad(
            StatusCode::SERVICE_UNAVAILABLE,
            "wants_closed",
            "this node takes wants only from prepaid accounts, and it has no prepaid contract",
        );
    };
    let now = (store.clock)();
    let msg = want_message(&want, week_of(now));
    if onyx_labels::attest::eip191_recover(msg.as_bytes(), &signature) != Some(buyer) {
        return bad(
            StatusCode::FORBIDDEN,
            "bad_want_signature",
            "the signature is not the buyer's over this week's want text",
        );
    }
    match accounts.account(buyer).await {
        Ok(a) if a.deposited > a.redeemed && !a.exiting => {}
        Ok(_) => {
            return bad(
                StatusCode::FORBIDDEN,
                "no_prepaid_account",
                "only an open prepaid deposit may post a want",
            )
        }
        Err(e) => {
            return bad(
                StatusCode::SERVICE_UNAVAILABLE,
                "chain_unavailable",
                format!("the prepaid account could not be read: {e}"),
            )
        }
    }
    let account = store.account_hash(&buyer);
    let added = {
        let mut board = store
            .board
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A renewal in the same week changes nothing, so nothing is written: the journal grows by
        // at most one line per account, category and week.
        if board.held_since(&account, &want) == Some(week_of(now)) {
            false
        } else {
            let mut trial = board.clone();
            let new = match trial.add(account, want, now) {
                Ok(new) => new,
                Err(e) => {
                    return bad(
                        StatusCode::TOO_MANY_REQUESTS,
                        "too_many_wants",
                        e.to_string(),
                    )
                }
            };
            if let Err(e) = store.append(&account, &want, now) {
                return bad(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "journal_unavailable",
                    format!("the want could not be kept: {e}"),
                );
            }
            *board = trial;
            new
        }
    };
    let mut r = Json(json!({
        "accepted": true,
        "new": added,
        "countsFrom": Board::next_cut(now),
    }))
    .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepaid::Account;
    use crate::x402::BoxFuture;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use onyx_intake::bounty::{Cadence, Domain, Region, WEEK};
    use onyx_intake::Kind;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    const T: u64 = 1_793_145_600;

    struct Accounts;
    impl AccountSource for Accounts {
        fn account<'a>(&'a self, buyer: [u8; 20]) -> BoxFuture<'a, Result<Account, String>> {
            Box::pin(async move {
                Ok(Account {
                    // Buyers whose first byte is odd hold a deposit.
                    deposited: if buyer[0] % 2 == 1 { 1_000 } else { 0 },
                    redeemed: 0,
                    exiting: false,
                })
            })
        }
    }

    fn secret(i: u8) -> [u8; 32] {
        // Keys whose address starts with an odd byte (a deposit) or an even one (none).
        let mut k = keccak256(&[i]).0;
        loop {
            let a = onyx_labels::attest::secret_address(&k).expect("test");
            if a[0] % 2 == i % 2 {
                return k;
            }
            k = keccak256(&k).0;
        }
    }

    fn want() -> Want {
        Want {
            kind: Kind::PublicApi,
            domain: Domain::WeatherClimate,
            region: Region::Europe,
            cadence: Cadence::Daily,
        }
    }

    fn body(secret: &[u8; 32], want: Value, week: u64) -> Value {
        let w: Want = serde_json::from_value(want.clone()).expect("test");
        let sig = onyx_labels::attest::eip191_sign_with(secret, want_message(&w, week).as_bytes())
            .expect("test");
        json!({
            "want": want,
            "buyer": format!("0x{}", hex::encode(onyx_labels::attest::secret_address(secret).expect("test"))),
            "signature": format!("0x{}", hex::encode(sig)),
        })
    }

    fn store(dir: &Path, clock: Arc<AtomicU64>, accounts: bool) -> Arc<WantsStore> {
        let c = clock.clone();
        Arc::new(
            WantsStore::open(
                dir,
                accounts.then(|| Arc::new(Accounts) as Arc<dyn AccountSource>),
                Arc::new(move || c.load(Ordering::SeqCst)),
            )
            .expect("test"),
        )
    }

    async fn call(app: &Router, method: &str, b: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri("/v1/wants")
            .header("content-type", "application/json")
            .body(b.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .expect("test");
        let r = app.clone().oneshot(req).await.expect("test");
        let s = r.status();
        let bytes = r.into_body().collect().await.expect("test").to_bytes();
        (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("onyx-wants-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[tokio::test]
    async fn wants_route_publishes_buckets_only() {
        let dir = tmp("buckets");
        let clock = Arc::new(AtomicU64::new(T));
        let s = store(&dir, clock.clone(), true);
        let app = routes(s.clone());
        let w = serde_json::to_value(want()).expect("test");
        for i in 0..5u8 {
            let (st, _) = call(
                &app,
                "POST",
                Some(body(&secret(2 * i + 1), w.clone(), week_of(T))),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
        }
        // Not shown before the cut; shown after it as a bucket, with no account in the body.
        let (_, b) = call(&app, "GET", None).await;
        assert_eq!(b["categories"], json!([]));
        clock.store(T + WEEK, Ordering::SeqCst);
        let (st, b) = call(&app, "GET", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            b["categories"],
            json!([{"kind":"public_api","domain":"weather_climate","region":"europe","cadence":"daily","bucket":"5-19"}])
        );
        let text = b.to_string();
        for i in 0..5u8 {
            let a =
                hex::encode(onyx_labels::attest::secret_address(&secret(2 * i + 1)).expect("test"));
            assert!(
                !text.contains(&a)
                    && !text.contains(
                        &s.account_hash(
                            &onyx_labels::attest::secret_address(&secret(2 * i + 1)).expect("test")
                        )
                        .to_string()[2..]
                    )
            );
        }
        // The journal holds salted hashes only, and a reopened store shows the same board.
        let journal = std::fs::read_to_string(dir.join(JOURNAL_FILE)).expect("test");
        assert!(!journal.contains(&hex::encode(
            onyx_labels::attest::secret_address(&secret(1)).expect("test")
        )));
        let again = store(&dir, clock, true);
        assert_eq!(again.shown()["categories"], b["categories"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn one_account_one_want_per_category() {
        let dir = tmp("one");
        let clock = Arc::new(AtomicU64::new(T));
        let app = routes(store(&dir, clock.clone(), true));
        let w = serde_json::to_value(want()).expect("test");
        let b = body(&secret(1), w.clone(), week_of(T));
        let (_, first) = call(&app, "POST", Some(b.clone())).await;
        let (_, again) = call(&app, "POST", Some(b)).await;
        assert_eq!(
            (first["new"].clone(), again["new"].clone()),
            (json!(true), json!(false))
        );
        for _ in 0..4 {
            call(&app, "POST", Some(body(&secret(1), w.clone(), week_of(T)))).await;
        }
        clock.store(T + WEEK, Ordering::SeqCst);
        let (_, board) = call(&app, "GET", None).await;
        assert_eq!(
            board["categories"],
            json!([]),
            "one account five times is one account"
        );
        let lines = std::fs::read_to_string(dir.join(JOURNAL_FILE))
            .expect("test")
            .lines()
            .count();
        assert_eq!(lines, 1, "renewals in the same week write nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_want_is_refused_without_a_deposit_a_signature_or_closed_lists() {
        let dir = tmp("refused");
        let clock = Arc::new(AtomicU64::new(T));
        let app = routes(store(&dir, clock.clone(), true));
        let w = serde_json::to_value(want()).expect("test");
        // No deposit.
        let (st, b) = call(&app, "POST", Some(body(&secret(2), w.clone(), week_of(T)))).await;
        assert_eq!(
            (st, b["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("no_prepaid_account"))
        );
        // Last week's signature.
        let (st, b) = call(
            &app,
            "POST",
            Some(body(&secret(1), w.clone(), week_of(T) - 1)),
        )
        .await;
        assert_eq!(
            (st, b["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("bad_want_signature"))
        );
        // A subject riding along, inside the want or beside it.
        let mut extra = body(&secret(1), w.clone(), week_of(T));
        extra["subject"] = json!("noaa.ghcnd.usw00003017:2026-10-29");
        let (st, b) = call(&app, "POST", Some(extra)).await;
        assert_eq!(
            (st, b["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("bounty_too_specific"))
        );
        let mut inner = body(&secret(1), w.clone(), week_of(T));
        inner["want"]["note"] = json!("rain in Lyon");
        let (st, b) = call(&app, "POST", Some(inner)).await;
        assert_eq!(
            (st, b["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("bounty_too_specific"))
        );
        let mut value = body(&secret(1), w, week_of(T));
        value["want"]["region"] = json!("lyon");
        let (st, b) = call(&app, "POST", Some(value)).await;
        assert_eq!(
            (st, b["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("bad_want_value"))
        );
        // Without a prepaid contract the board is shown and posting is closed.
        let closed = routes(store(&tmp("closed"), clock, false));
        let (st, b) = call(
            &closed,
            "POST",
            Some(body(
                &secret(1),
                serde_json::to_value(want()).expect("test"),
                week_of(T),
            )),
        )
        .await;
        assert_eq!(
            (st, b["error"]["code"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("wants_closed"))
        );
        let (st, _) = call(&closed, "GET", None).await;
        assert_eq!(st, StatusCode::OK);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
