//! Keyed contributions with agreement (ADR-0054, contributor loop CL4): several independent
//! holders of a provider API key, each fetching the same value with their own key, count like
//! an ordinary level-2 round; one holder alone stays level 4 and says so.
//!
//! Nothing here is a new lane: a keyed source is a manifest whose descriptor says
//! `access: Keyed` ([`placement_of`]), its pool is the holders' signed seats
//! ([`KeyedSeat`]), and the round is the ordinary one (draw, commits, sealed or spooled
//! reveals, receipts, `recordObservation`), with these keyed rules:
//!
//! - **Seats** ([`KeyedSeat`], [`sign_seat`]): a signed statement of the observer, the
//!   source, the declared operator, the hash of the terms shown ([`terms_hash`]) and when it
//!   joined. Never anything about the key: no key, no fingerprint, no hash of it (item 6).
//! - **The pool of a round** ([`keyed_snapshot`]): the seats that are signed, for this source
//!   and these terms, bonded for at least `outlier_slash` since [`KEYED_BOND_AGE_SECS`] before
//!   the window, with [`KEYED_MIN_HISTORY`] agreed public rounds, and at most ONE seat per
//!   declared operator (the operator rule of item 6, applied to the round's snapshot so the
//!   draw, the pool hash and every verifier stay unchanged). Fewer than [`KEYED_MIN_POOL`]
//!   left: no round; the value stays level 4 ([`one_holder_statement`], item 4).
//! - **Economics** ([`keyed_config`]): `outlier_slash == miss_slash` (item 7): a provider can
//!   answer keys differently, so disagreement is weak evidence of a lie.
//! - **Privacy of the holder** (items 2 and 10): the fetch slot is private and random
//!   ([`private_fetch_time`]), the reveal's `observed_at` is the window start and its page hash
//!   is blinded with a fresh random value ([`private_observation`]), so neither the published
//!   fetch time nor the page hash lets the provider match a reveal to the key that fetched.
//!   The raw page is never published (the site summary withholds it).
//! - **Shown odds** ([`collusion_odds_ppm`]): the chance a third of the pool colluding wins one
//!   draw, next to every keyed value.

use crate::Source;
use onyx_coordinator::observe::{check_pool, ObserveConfig, Observer};
use onyx_core::feed_json::{Access, KeyPlace, KeyScheme};
use onyx_core::{keccak256, Answer, Hash32, Observation, ObservationSpec};
use onyx_fetch::KeyPlacement;
use serde_json::{json, Value as Json};

/// Fewest drawable seats a keyed round draws from (ADR-0054 item 4; the rehearsal pool's 9).
pub const KEYED_MIN_POOL: usize = 9;
/// Fewest observers a keyed template draws (ADR-0054 item 4).
pub const KEYED_MIN_OBSERVERS: u16 = 3;
/// How long a seat's bond must be held before the round's window (ADR-0054 item 5: 7 days).
pub const KEYED_BOND_AGE_SECS: u64 = 7 * 86_400;
/// Agreed rounds on public sources a seat needs before it is drawable (ADR-0054 item 6).
pub const KEYED_MIN_HISTORY: u32 = 5;
/// The evidence text a keyed level-2 value carries (ADR-0054 item 3).
pub const KEYED_EVIDENCE: &str = "Level 2 \u{b7} re-observed by key holders";
/// What one key holder's value is (ADR-0054 item 4).
pub const ONE_HOLDER: &str = "stated by one key holder";
/// Longest declared operator id, in bytes.
pub const MAX_OPERATOR: usize = 64;
/// How long a seat counts after it was signed (review CL4-R1-7); the holder joins again.
pub const KEYED_SEAT_TTL_SECS: u64 = 90 * 86_400;

/// When a keyed observer posts its commit (review CL4-R1-2): the window's end, whatever its
/// private fetch second, so the commit's time on chain says nothing about when its key
/// fetched. (`commit_close` is at least the window end, ADR-0027.)
pub fn keyed_commit_time(spec: &ObservationSpec) -> u64 {
    spec.window_end
}

/// Whether `answer` holds any piece of `key` (review CL4-R1-1): such an answer is never
/// committed or revealed; the observer answers `Unavailable` instead.
pub fn answer_holds_key(answer: &Answer, key: &onyx_fetch::ApiKey) -> bool {
    match answer {
        Answer::Number(n) => key.held_in(n.to_string().as_bytes()),
        Answer::Bytes(b) => key.held_in(b),
        Answer::Unavailable => false,
    }
}

/// A keyed observer's page: `None` (no answer) when the value read from it holds a piece of
/// the key (review CL4-R1-1), so a pointer or a provider that echoes the key never puts it
/// in a commit or a reveal.
pub fn keyed_page(
    src: &dyn Source,
    spec: &ObservationSpec,
    body: Option<Vec<u8>>,
    key: &onyx_fetch::ApiKey,
) -> Option<Vec<u8>> {
    let b = body?;
    let answer = src.fetcher().extract(spec, &b);
    if answer_holds_key(&answer, key) {
        None
    } else {
        Some(b)
    }
}

/// The check a keyed observer makes before it commits (review CL4-R1-5): it needs the seats
/// file, and the pool it was given must be this round's keyed snapshot of it. Without this a
/// coordinator could draw from addresses with no seat, fresh bonds or several seats of one
/// operator.
pub fn check_observer_pool(
    src: &dyn Source,
    seats_text: Option<&str>,
    spec: &ObservationSpec,
    pool: &[Observer],
) -> Result<(), String> {
    let seats = seats_text.ok_or(
        "a keyed observer needs the round's seats (--seats FILE, or an announcement that carries them) to check the pool",
    )?;
    let s = snapshot_for(src, seats, spec).map_err(|e| e.to_string())?;
    if s.pool != pool {
        return Err("the pool is not this round's keyed snapshot of the seats: refused".into());
    }
    Ok(())
}

/// Why a seat or a keyed pool is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyedError {
    /// A malformed seats file or seat.
    #[error("keyed seats: {0}")]
    Seats(String),
    /// Fewer drawable seats than a keyed round needs: no round, the value stays level 4.
    #[error("{have} drawable keyed seat(s); a keyed round needs {need}: the value stays level 4, {ONE_HOLDER}")]
    TooFewSeats {
        /// Drawable seats.
        have: usize,
        /// [`KEYED_MIN_POOL`].
        need: usize,
    },
}

/// Where a descriptor's key goes, for the fetch; `None` for a public source.
pub fn placement_of(access: &Access) -> Option<KeyPlacement> {
    match access {
        Access::Public => None,
        Access::Keyed {
            place: KeyPlace::Header,
            name,
            scheme,
        } => Some(KeyPlacement::Header {
            name: name.clone(),
            bearer: *scheme == KeyScheme::Bearer,
        }),
        Access::Keyed {
            place: KeyPlace::Query,
            name,
            ..
        } => Some(KeyPlacement::Query { name: name.clone() }),
    }
}

/// `keccak256` of the provider terms text a holder is shown and accepts before joining (the
/// manifest's `terms_quote`, UTF-8).
pub fn terms_hash(terms_text: &str) -> Hash32 {
    keccak256(terms_text.as_bytes())
}

/// Whether `s` is a declared operator id: dot-separated labels of `a-z`, `0-9` and `-`,
/// 1..=[`MAX_OPERATOR`] bytes.
pub fn is_operator_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_OPERATOR
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

/// Whether `s` is a source id (a manifest's `id` rule).
fn is_source_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'_' || c == b'-'
        })
}

/// A keyed seat (ADR-0054 item 5): a signed statement, never anything about the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyedSeat {
    /// The observer's address (its signing key).
    pub observer: [u8; 20],
    /// The source id (the manifest's `id`).
    pub source: String,
    /// The declared operator, if any ([`is_operator_id`]).
    pub operator: Option<String>,
    /// [`terms_hash`] of the terms the holder accepted.
    pub terms_hash: Hash32,
    /// Unix seconds it joined.
    pub joined_at: u64,
    /// EIP-191 signature of [`seat_message`] by `observer`.
    pub sig: [u8; 65],
}

/// The text a seat signs (EIP-191). The operator is signed too (amendment to ADR-0054 item 5:
/// otherwise anyone relaying the seat could drop it and defeat the operator rule).
pub fn seat_message(
    source: &str,
    observer: &[u8; 20],
    operator: Option<&str>,
    terms_hash: &Hash32,
    joined_at: u64,
) -> String {
    format!(
        "Onyx keyed seat v1\nsource: {source}\nobserver: 0x{}\noperator: {}\nterms: {terms_hash}\njoined: {joined_at}",
        hex(observer),
        operator.unwrap_or("none"),
    )
}

/// A seat signed with the observer's 32-byte key.
pub fn sign_seat(
    secret: &[u8; 32],
    source: &str,
    operator: Option<&str>,
    terms_hash: Hash32,
    joined_at: u64,
) -> Result<KeyedSeat, KeyedError> {
    if !is_source_id(source) {
        return Err(KeyedError::Seats(format!("{source}: not a source id")));
    }
    if let Some(op) = operator {
        if !is_operator_id(op) {
            return Err(KeyedError::Seats(format!(
                "{op}: an operator is dot-separated a-z, 0-9 and -, at most 64 bytes"
            )));
        }
    }
    let observer = onyx_labels::attest::secret_address(secret)
        .ok_or_else(|| KeyedError::Seats("not a valid key".into()))?;
    let msg = seat_message(source, &observer, operator, &terms_hash, joined_at);
    let sig = onyx_labels::attest::eip191_sign_with(secret, msg.as_bytes())
        .ok_or_else(|| KeyedError::Seats("could not sign".into()))?;
    Ok(KeyedSeat {
        observer,
        source: source.into(),
        operator: operator.map(str::to_owned),
        terms_hash,
        joined_at,
        sig,
    })
}

impl KeyedSeat {
    /// Whether the signature is the observer's over [`seat_message`].
    pub fn verify(&self) -> bool {
        let msg = seat_message(
            &self.source,
            &self.observer,
            self.operator.as_deref(),
            &self.terms_hash,
            self.joined_at,
        );
        onyx_labels::attest::eip191_recover(msg.as_bytes(), &self.sig) == Some(self.observer)
    }

    /// The seat's JSON form (what `onyx-contrib keyed join` writes and the holder sends).
    pub fn to_json(&self) -> Json {
        json!({
            "observer": format!("0x{}", hex(&self.observer)),
            "source": self.source,
            "operator": self.operator,
            "terms_hash": self.terms_hash.to_string(),
            "joined_at": self.joined_at,
            "sig": format!("0x{}", hex(&self.sig)),
        })
    }

    /// A seat from its JSON form (strict: exactly these fields).
    pub fn from_json(v: &Json) -> Result<Self, KeyedError> {
        let bad = |w: &str| KeyedError::Seats(format!("seat: {w}"));
        let o = v.as_object().ok_or_else(|| bad("not an object"))?;
        let fields = [
            "joined_at",
            "observer",
            "operator",
            "sig",
            "source",
            "terms_hash",
        ];
        if let Some(k) = o.keys().find(|k| !fields.contains(&k.as_str())) {
            return Err(bad(&format!("unknown field {k}")));
        }
        let s = |k: &str| {
            o.get(k)
                .and_then(Json::as_str)
                .ok_or_else(|| bad(&format!("{k}: a string")))
        };
        let observer =
            onyx_coordinator::cast_chain::parse_addr(s("observer")?).map_err(|e| bad(&e))?;
        let mut terms = [0u8; 32];
        hex_exact(s("terms_hash")?, &mut terms).map_err(|_| bad("terms_hash"))?;
        let mut sig = [0u8; 65];
        hex_exact(s("sig")?, &mut sig).map_err(|_| bad("sig"))?;
        let operator = match o.get("operator") {
            None | Some(Json::Null) => None,
            Some(Json::String(op)) if is_operator_id(op) => Some(op.clone()),
            Some(_) => return Err(bad("operator")),
        };
        let source = s("source")?.to_string();
        if !is_source_id(&source) {
            return Err(bad("source"));
        }
        Ok(KeyedSeat {
            observer,
            source,
            operator,
            terms_hash: Hash32(terms),
            joined_at: o
                .get("joined_at")
                .and_then(Json::as_u64)
                .ok_or_else(|| bad("joined_at"))?,
            sig,
        })
    }
}

/// A seat and what the coordinator attests about it for one round: the bond, when it was
/// bonded, and how many public rounds it agreed in. (v1: the coordinator's word, as the bonds
/// of every pool snapshot are today; reading the bond's age from the chain is THREATS CL4-3's
/// TODO.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatEntry {
    /// The signed seat.
    pub seat: KeyedSeat,
    /// The bond, in the smallest unit.
    pub bond: u128,
    /// Unix seconds the bond was posted.
    pub bonded_at: u64,
    /// Agreed rounds on public sources.
    pub agreed_rounds: u32,
}

/// Version of the seats file.
pub const SEATS_V: u64 = 1;

/// The seats file: `{"v": 1, "seats": [{"seat": {...}, "bond": "1000", "bonded_at": N,
/// "agreed_rounds": N}, ...]}`, sorted strictly by observer address (so one seat per address).
pub fn parse_seats(text: &str) -> Result<Vec<SeatEntry>, KeyedError> {
    onyx_json::parse(text.as_bytes(), 1 << 20)
        .map_err(|e| KeyedError::Seats(format!("not strict JSON: {e}")))?;
    let v: Json = serde_json::from_str(text).map_err(|e| KeyedError::Seats(e.to_string()))?;
    if v.get("v").and_then(Json::as_u64) != Some(SEATS_V) {
        return Err(KeyedError::Seats("v: only version 1".into()));
    }
    let items = v
        .get("seats")
        .and_then(Json::as_array)
        .ok_or_else(|| KeyedError::Seats("seats: a list".into()))?;
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let bad = |w: &str| KeyedError::Seats(format!("entry: {w}"));
        let o = it.as_object().ok_or_else(|| bad("not an object"))?;
        if let Some(k) = o
            .keys()
            .find(|k| !["agreed_rounds", "bond", "bonded_at", "seat"].contains(&k.as_str()))
        {
            return Err(bad(&format!("unknown field {k}")));
        }
        let bond = o
            .get("bond")
            .and_then(Json::as_str)
            .and_then(|b| b.parse::<u128>().ok())
            .ok_or_else(|| bad("bond: a decimal string"))?;
        out.push(SeatEntry {
            seat: KeyedSeat::from_json(o.get("seat").ok_or_else(|| bad("seat"))?)?,
            bond,
            bonded_at: o
                .get("bonded_at")
                .and_then(Json::as_u64)
                .ok_or_else(|| bad("bonded_at"))?,
            agreed_rounds: o
                .get("agreed_rounds")
                .and_then(Json::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| bad("agreed_rounds"))?,
        });
    }
    if !out
        .windows(2)
        .all(|w| w[0].seat.observer < w[1].seat.observer)
    {
        return Err(KeyedError::Seats(
            "seats must be sorted by observer address, one seat per address".into(),
        ));
    }
    Ok(out)
}

/// The seats file of `entries` (sorted by observer).
pub fn seats_to_json(entries: &[SeatEntry]) -> Json {
    let mut e: Vec<&SeatEntry> = entries.iter().collect();
    e.sort_by_key(|x| x.seat.observer);
    json!({
        "v": SEATS_V,
        "seats": e.iter().map(|x| json!({
            "seat": x.seat.to_json(),
            "bond": x.bond.to_string(),
            "bonded_at": x.bonded_at,
            "agreed_rounds": x.agreed_rounds,
        })).collect::<Vec<_>>(),
    })
}

/// A keyed round's pool snapshot, and why each left-out seat is out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyedSnapshot {
    /// The drawable seats as pool members, ascending by address.
    pub pool: Vec<Observer>,
    /// Seats left out, with the reason.
    pub left_out: Vec<([u8; 20], &'static str)>,
}

/// The operator-rule key of `address` for the spec: an operator's seat with the smallest key
/// is its one seat in this round's snapshot. Fixed by public data (the spec id and the
/// address), so the coordinator does not choose it.
fn operator_rank(spec_id: &Hash32, address: &[u8; 20]) -> Hash32 {
    let mut b = b"onyx-keyed-operator-v1".to_vec();
    b.extend_from_slice(&spec_id.0);
    b.extend_from_slice(address);
    keccak256(&b)
}

/// The pool snapshot of one keyed round (see the module docs). Pure: anyone with the seats
/// file, the manifest and the spec recomputes it, and the draw commits to its hash.
pub fn keyed_snapshot(
    entries: &[SeatEntry],
    source: &str,
    terms: &Hash32,
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
) -> Result<KeyedSnapshot, KeyedError> {
    let mut left_out = vec![];
    let mut ok: Vec<&SeatEntry> = vec![];
    for e in entries {
        let s = &e.seat;
        let why = if !s.verify() {
            Some("the seat's signature is not the observer's")
        } else if s.source != source {
            Some("a seat for another source")
        } else if s.terms_hash != *terms {
            Some("a seat for other terms than the manifest's")
        } else if s.joined_at > spec.window_start {
            Some("joined after the window starts")
        } else if s.joined_at.saturating_add(KEYED_SEAT_TTL_SECS) < spec.window_start {
            // Review CL4-R1-7: a holder who stopped is not drawn (and slashed) for ever.
            Some("the seat expired (older than 90 days): join again")
        } else if e.bond < cfg.outlier_slash {
            Some("bond below the round's outlier slash")
        } else if e.bonded_at.saturating_add(KEYED_BOND_AGE_SECS) > spec.window_start {
            Some("bond younger than 7 days at the window start")
        } else if e.agreed_rounds < KEYED_MIN_HISTORY {
            Some("fewer than 5 agreed public rounds")
        } else {
            None
        };
        match why {
            Some(w) => left_out.push((s.observer, w)),
            None => ok.push(e),
        }
    }
    // One seat per declared operator (ADR-0054 item 6).
    let spec_id = spec.id();
    let mut pool: Vec<Observer> = vec![];
    for e in &ok {
        let s = &e.seat;
        let kept = match &s.operator {
            None => true,
            Some(op) => {
                let mine = operator_rank(&spec_id, &s.observer);
                !ok.iter().any(|o| {
                    o.seat.operator.as_deref() == Some(op.as_str())
                        && o.seat.observer != s.observer
                        && operator_rank(&spec_id, &o.seat.observer) < mine
                })
            }
        };
        if kept {
            pool.push(Observer {
                address: s.observer,
                bond: e.bond,
            });
        } else {
            left_out.push((
                s.observer,
                "another seat of the same operator is in this round",
            ));
        }
    }
    pool.sort_by_key(|o| o.address);
    left_out.sort();
    if pool.len() < KEYED_MIN_POOL {
        return Err(KeyedError::TooFewSeats {
            have: pool.len(),
            need: KEYED_MIN_POOL,
        });
    }
    check_pool(&pool).map_err(|e| KeyedError::Seats(e.to_string()))?;
    Ok(KeyedSnapshot { pool, left_out })
}

/// [`keyed_snapshot`] for a keyed `src` from the text of its seats file.
pub fn snapshot_for(
    src: &dyn Source,
    seats_text: &str,
    spec: &ObservationSpec,
) -> Result<KeyedSnapshot, KeyedError> {
    let (source, terms) = src
        .keyed_seat_rules()
        .ok_or_else(|| KeyedError::Seats("not a keyed source".into()))?;
    keyed_snapshot(
        &parse_seats(seats_text)?,
        &source,
        &terms,
        spec,
        &src.observe_config(spec),
    )
}

/// The pool file (`[{"address": "0x…", "bond": N}, …]`, [`crate::parse_pool`]'s form).
pub fn pool_to_json(pool: &[Observer]) -> Result<String, String> {
    let items = pool
        .iter()
        .map(|o| {
            u64::try_from(o.bond)
                .map(|b| json!({ "address": format!("0x{}", hex(&o.address)), "bond": b }))
                .map_err(|_| "a bond above u64 does not fit the pool file".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_string(&items).map_err(|e| e.to_string())
}

/// A keyed round's timing and economics: as [`crate::config`], with
/// `outlier_slash == miss_slash` (ADR-0054 item 7, THREATS CL4-4).
pub fn keyed_config(spec: &ObservationSpec) -> ObserveConfig {
    let c = crate::config(spec);
    ObserveConfig {
        outlier_slash: c.miss_slash,
        ..c
    }
}

/// What a keyed value with no round says (ADR-0054 item 4): level 4, stated by one key
/// holder, and why there is no level-2 round.
pub fn one_holder_statement(src: &dyn Source, drawable: usize, why: &str) -> Json {
    let t = src.template();
    json!({
        "metric": t.metric.0,
        "template": t.id().to_string(),
        "proof_level": 4,
        "standing": ONE_HOLDER,
        "level2_claim": false,
        "drawable_seats": drawable,
        "needed_seats": KEYED_MIN_POOL,
        "why": why,
        "note": "A key holder's own value is a level-4 claim: no other holder re-observed it. It reaches level 2 only when a keyed round of at least 9 drawable seats agrees.",
    })
}

fn choose(n: u128, k: u128) -> u128 {
    if k > n {
        return 0;
    }
    (0..k).fold(1u128, |acc, i| acc * (n - i) / (i + 1))
}

/// The odds, in parts per million (rounded down), that `colluders` members of a pool of
/// `pool` hold a strict majority of one fair draw of `draw`, and of the best of the 3 draws a
/// subject may take (`MAX_SUBJECT_DRAWS`). As `collusion_odds_of_the_rehearsal_pool`.
pub fn collusion_odds_ppm(pool: u64, draw: u64, colluders: u64) -> (u64, u64) {
    let (n, d, c) = (u128::from(pool), u128::from(draw), u128::from(colluders));
    if d == 0 || d > n {
        return (0, 0);
    }
    let majority = d / 2 + 1;
    let all = choose(n, d);
    let win: u128 = (majority..=c.min(d))
        .map(|k| choose(c, k) * choose(n - c, d - k))
        .sum();
    let one = win * 1_000_000 / all;
    let lose = all - win;
    let best3 = (all.pow(3) - lose.pow(3)) * 1_000_000 / all.pow(3);
    (
        u64::try_from(one).unwrap_or(u64::MAX),
        u64::try_from(best3).unwrap_or(u64::MAX),
    )
}

/// The keyed facts shown next to a keyed value: the evidence text and the odds that a third
/// of the pool colluding wins (ADR-0054 items 3 and 6).
pub fn keyed_shown(pool: usize, draw: u16) -> Json {
    let pool = u64::try_from(pool).unwrap_or(u64::MAX);
    let third = pool / 3;
    let (one, best3) = collusion_odds_ppm(pool, u64::from(draw), third);
    json!({
        "evidence": KEYED_EVIDENCE,
        "pool": pool,
        "drawn": draw,
        "colluders": third,
        "collusion_odds_one_draw_ppm": one,
        "collusion_odds_best_of_3_ppm": best3,
        "note": "Onyx cannot tell several keys of one person from several people (no key fingerprints are ever asked for). Testnet bonds have no value.",
    })
}

/// A private fetch time for a keyed observer (ADR-0054 item 10, THREATS CL4-7): uniformly in
/// the window but its last `FETCH_MARGIN_SECS`, from `random` (the observer's own random
/// draw), never from public data, and never published.
pub fn private_fetch_time(spec: &ObservationSpec, random: u64) -> u64 {
    let len = spec.window_end.saturating_sub(spec.window_start);
    let spread = len.saturating_sub(onyx_core::fetch::FETCH_MARGIN_SECS);
    if spread == 0 {
        return spec.window_start;
    }
    spec.window_start + random % spread
}

/// [`private_fetch_time`] with a fresh random draw from the system.
pub fn random_fetch_time(spec: &ObservationSpec) -> Result<u64, String> {
    let r = onyx_coordinator::observe::random_salt().map_err(|e| format!("random: {e}"))?;
    let mut w = [0u8; 8];
    w.copy_from_slice(&r.0[..8]);
    Ok(private_fetch_time(spec, u64::from_be_bytes(w)))
}

/// A keyed observer's observation as it is revealed (ADR-0054 items 2 and 10, THREATS
/// CL4-7, CL4-9): `observed_at` is the window start (not the private fetch second), and a
/// page hash is blinded, `keccak256("onyx-keyed-page-v1" ‖ blind ‖ page_hash)` with 32 fresh
/// random bytes that are never kept, so the provider, who knows every page it served to every
/// key, cannot match a reveal to a key by its time or its page. The answer is unchanged, so
/// agreement is unchanged.
pub fn private_observation(spec: &ObservationSpec, o: Observation) -> Result<Observation, String> {
    let page_hash = if o.page_hash == Hash32::ZERO {
        Hash32::ZERO
    } else {
        let blind = onyx_coordinator::observe::random_salt().map_err(|e| format!("random: {e}"))?;
        let mut b = b"onyx-keyed-page-v1".to_vec();
        b.extend_from_slice(&blind.0);
        b.extend_from_slice(&o.page_hash.0);
        keccak256(&b)
    };
    let out = Observation {
        observed_at: spec.window_start,
        page_hash,
        ..o
    };
    out.check(spec).map_err(|e| format!("observe: {e}"))?;
    Ok(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_exact(s: &str, out: &mut [u8]) -> Result<(), ()> {
    let s = s.strip_prefix("0x").ok_or(())?;
    if s.len() != out.len() * 2
        || !s
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(());
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| ())?;
    }
    Ok(())
}
