//! The browser's level-2 round check (L2SRC-R1-F3): its mirrors of the
//! coordinator's round id and result hash match the coordinator, the recorded
//! summary checks out, and a self-consistent forgery does not.

use onyx_coordinator::observe::RecordedDraw;
use onyx_core::{keccak256, observation_commit, Answer, Hash32, Observation};
use onyx_source_fiscaldata::round::recorded_draw;
use onyx_source_fiscaldata::round::{run, store, Page};
use onyx_source_fiscaldata::{recorded, RECORDED_WINDOW_START};
use onyx_web_verify::{
    check_edgar_rounds, check_level2_round, level2_cfg_hash, level2_commit_message, level2_draw,
    level2_draw_commit, level2_draw_hash, level2_pool_hash, level2_result_hash,
    level2_reveal_message, level2_round_id,
};
use serde_json::Value;

const SUMMARY: &str = include_str!("../../static/level2/debt-to-penny.json");

fn h(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).expect("hex")
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn check(r: &Value) -> Value {
    let out = check_level2_round(&r.to_string()).unwrap_or_else(|_| panic!("check errored"));
    serde_json::from_str(&out).expect("json")
}

#[test]
fn mirrors_match_the_coordinator() {
    let mut chain = RecordedDraw::new(recorded_draw());
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let r = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &pages,
        &mut store(),
        &mut chain,
    )
    .expect("round");
    let cfg = (
        r.cfg.commit_close,
        r.cfg.reveal_close,
        r.cfg.reward,
        r.cfg.outlier_slash,
        r.cfg.miss_slash,
    );
    let obs: Vec<([u8; 20], u128)> = r.observers.iter().map(|o| (o.address, o.bond)).collect();
    // The draw (L2-F2): pool hash, draw hash and the drawn set.
    let pool: Vec<([u8; 20], u128)> = r.pool.iter().map(|o| (o.address, o.bond)).collect();
    assert!(pool.len() > obs.len(), "the pool is larger than the draw");
    assert_eq!(
        level2_pool_hash(&pool),
        onyx_coordinator::observe::pool_hash(&r.pool)
    );
    let dh = level2_draw_hash(
        &level2_pool_hash(&pool),
        r.draw.beacon_height,
        &r.draw.beacon,
        r.draw.attempt,
    );
    assert_eq!(dh, onyx_coordinator::observe::draw_hash(&r.draw));
    assert_eq!(
        level2_draw(&r.spec.id(), &pool, &dh, obs.len(), r.cfg.outlier_slash),
        obs
    );
    let rid = level2_round_id(&r.spec.id(), cfg, &obs, &dh);
    assert_eq!(rid, r.round_id);
    // L2RND-R3: the draw commitment the registry stores.
    let ch = level2_cfg_hash(cfg);
    assert_eq!(ch, onyx_coordinator::observe::cfg_hash(&r.cfg));
    let commit = level2_draw_commit(&level2_pool_hash(&pool), &r.spec.id(), &ch);
    assert_eq!(
        commit,
        onyx_coordinator::observe::draw_commit(&r.draw.pool, &r.spec.id(), &ch)
    );
    assert_eq!(commit, r.onchain_draw.commit);
    let onyx_coordinator::observe::ObservationOutcome::Value(Answer::Number(m)) =
        r.settlement.outcome()
    else {
        panic!("no value")
    };
    assert_eq!(
        level2_result_hash(&r.spec.id(), &rid, *m),
        r.settlement.result_hash()
    );
    // ATT-R1-F3: the signed messages are the coordinator's.
    for ((a, h, _), (_, o, salt, _)) in r.commits.iter().zip(&r.reveals) {
        assert_eq!(
            level2_commit_message(&rid, r.draw.attempt, a, h),
            onyx_coordinator::observe::observation_commit_message(&rid, r.draw.attempt, a, h)
        );
        assert_eq!(
            level2_reveal_message(&rid, r.draw.attempt, a, o, salt),
            onyx_coordinator::observe::observation_reveal_message(&rid, r.draw.attempt, a, o, salt)
        );
    }
}

/// What `scripts/build-web.sh` does to a round in a public build (ATT-R2-F1,
/// `onyx_api::observations::seal_site_round`): no value, reveals, statuses
/// or page, and no result hash (SIG-R1-F2); the signed commits only.
fn seal(r: &Value) -> Value {
    let mut v = r.clone();
    let commits: Vec<Value> = r["reveals"]
        .as_array()
        .expect("reveals")
        .iter()
        .map(|e| {
            serde_json::json!({
                "observer": e["observer"],
                "commit": e["commit"],
                "commit_sig": e["commit_sig"],
            })
        })
        .collect();
    let m = v.as_object_mut().expect("obj");
    for k in [
        "reveals",
        "statuses",
        "agreed",
        "response_body",
        "result_hash",
    ] {
        m.remove(k);
    }
    m.insert("value".into(), "".into());
    m.insert("sealed".into(), true.into());
    m.insert("commits".into(), commits.into());
    v
}

/// ATT-R1-F3: every commit and reveal must carry its observer's signature;
/// one moved onto another observer's entry fails.
#[test]
fn an_unsigned_commit_or_reveal_fails() {
    let good: Value = serde_json::from_str(SUMMARY).expect("json");
    for field in ["commit_sig", "reveal_sig"] {
        let mut r = good.clone();
        let other = r["reveals"][1][field].clone();
        r["reveals"][0][field] = other;
        let out = check(&r);
        assert_eq!(out["ok"], Value::Bool(false), "{field}: {out}");
        let mut r = good.clone();
        r["reveals"][0].as_object_mut().expect("obj").remove(field);
        assert_eq!(check(&r)["ok"], Value::Bool(false), "{field} missing");
    }
}

/// ATT-R2-F1: a public build ships the round sealed. It still checks (draw,
/// observers, round id, every commit signature, the pinned result hash) and
/// shows no value; a sealed round with a moved signature or another result
/// hash fails.
#[test]
fn a_sealed_round_checks_without_its_value() {
    let sealed = seal(&serde_json::from_str(SUMMARY).expect("json"));
    let text = sealed.to_string();
    assert!(!text.contains("response_body") && !text.contains("\"salt\""));
    let out = check(&sealed);
    assert_eq!(out["ok"], Value::Bool(true), "{out}");
    assert_eq!(out["sealed"], Value::Bool(true));
    assert_eq!(out["value"], "");
    assert_eq!(out["commits_checked"], 5);
    let mut moved = sealed.clone();
    moved["commits"][0]["commit_sig"] = sealed["commits"][1]["commit_sig"].clone();
    assert_eq!(check(&moved)["ok"], Value::Bool(false));
    assert_eq!(out["result_hash"], "");
    // SIG-R1-F2: a sealed round that ships a result hash is refused.
    let mut other = sealed.clone();
    other["result_hash"] = hx(&[7; 32]).into();
    assert_eq!(check(&other)["ok"], Value::Bool(false));
}

/// The SEC EDGAR card: the recorded rounds check; sealed they check without
/// values; a moved commit signature fails.
#[test]
fn the_edgar_rounds_check_signed_and_sealed() {
    let doc: Value =
        serde_json::from_str(include_str!("../../static/level2/sec-holdings.json")).expect("json");
    let run = |d: &Value| -> Value {
        serde_json::from_str(
            &check_edgar_rounds(&d.to_string()).unwrap_or_else(|_| panic!("errored")),
        )
        .expect("json")
    };
    assert_eq!(run(&doc)["ok"], Value::Bool(true));
    let mut sealed = doc.clone();
    for c in sealed["companies"].as_array_mut().expect("companies") {
        *c = seal(c);
    }
    let out = run(&sealed);
    assert_eq!(out["ok"], Value::Bool(true), "{out}");
    assert_eq!(out["companies"][0]["value"], "");
    let mut moved = doc.clone();
    let other = moved["companies"][0]["reveals"][1]["reveal_sig"].clone();
    moved["companies"][0]["reveals"][0]["reveal_sig"] = other;
    assert_eq!(run(&moved)["ok"], Value::Bool(false));
}

#[test]
fn the_recorded_summary_checks_out() {
    let r: Value = serde_json::from_str(SUMMARY).expect("json");
    let out = check(&r);
    assert_eq!(out["ok"], Value::Bool(true), "{out}");
    assert_eq!(out["commits_checked"], 5);
}

/// The reviewer's forgery: another date and value, the page hash and every
/// commit recomputed from the revealed salts. It must fail (on the pins).
#[test]
fn a_consistent_forgery_fails() {
    let mut r: Value = serde_json::from_str(SUMMARY).expect("json");
    let date = "2026-10-07";
    let body = r["response_body"]
        .as_str()
        .expect("body")
        .replace("2026-10-08", date)
        .replace("40305316210829.72", "99999999999999.99");
    let ws = r["feed"]["window_start"].as_u64().expect("ws");
    let feed = onyx_source_fiscaldata::spec(date, ws).expect("still in the lag");
    r["feed"]["record_date"] = date.into();
    r["feed"]["url"] = feed.url.clone().into();
    r["feed"]["spec_id"] = hx(&feed.id().0).into();
    r["response_body"] = body.clone().into();
    r["value"] = "99999999999999.99".into();
    let ph = keccak256(body.as_bytes());
    let rid = Hash32(
        h(r["round_id"].as_str().expect("rid"))
            .try_into()
            .expect("32"),
    );
    for v in r["reveals"].as_array_mut().expect("reveals") {
        let a: [u8; 20] = h(v["observer"].as_str().expect("a"))
            .try_into()
            .expect("20");
        let salt = Hash32(h(v["salt"].as_str().expect("salt")).try_into().expect("32"));
        let o = Observation {
            answer: Answer::Number(9_999_999_999_999_999),
            observed_at: v["observed_unix"].as_u64().expect("t"),
            page_hash: ph,
        };
        v["page_hash"] = hx(&ph.0).into();
        v["commit"] = hx(&observation_commit(&rid, &a, &o, &salt).0).into();
    }
    let out = check(&r);
    assert_eq!(out["ok"], Value::Bool(false), "{out}");
    // The browser test serves this same forgery (apps/web/tests/e2e.mjs);
    // regenerate it with ONYX_BLESS=1.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/level2-forged.json"
    );
    let text = format!("{}\n", serde_json::to_string_pretty(&r).expect("json"));
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::write(path, &text).expect("write fixture");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("fixture (ONYX_BLESS=1 to create)"),
        text,
        "e2e forgery fixture is stale (ONYX_BLESS=1)"
    );
}

#[test]
fn a_repeated_observer_or_an_edited_value_fails() {
    let mut dup: Value = serde_json::from_str(SUMMARY).expect("json");
    let first = dup["reveals"][0].clone();
    dup["reveals"][1] = first;
    assert_eq!(check(&dup)["ok"], Value::Bool(false));

    let mut val: Value = serde_json::from_str(SUMMARY).expect("json");
    val["value"] = "40305316210829.73".into();
    assert_eq!(check(&val)["ok"], Value::Bool(false));

    let mut old: Value = serde_json::from_str(SUMMARY).expect("json");
    old["feed"]["record_date"] = "2001-01-02".into();
    assert_eq!(check(&old)["ok"], Value::Bool(false));
}

/// L2SRC-R2-F3 (review probe `displayed_but_unchecked_fields`): a summary
/// that keeps every hash consistent but swaps what the card shows (source
/// name, terms link and quote, attribution, observer count, unit, subject,
/// selector) fails, and the check's own output carries only pinned values.
#[test]
fn displayed_fields_are_checked_or_pinned() {
    let mut r: Value = serde_json::from_str(SUMMARY).expect("json");
    let ok = check(&r);
    assert_eq!(ok["ok"], Value::Bool(true));
    assert_eq!(ok["terms_url"], "https://fiscaldata.treasury.gov/about-us/");
    assert_eq!(ok["observers"], 5);
    assert_eq!(ok["agreed"], 5);
    assert_eq!(ok["record_date"], "2026-10-08");
    r["source"]["name"] = "U.S. Treasury (official, certified)".into();
    r["source"]["terms_url"] = "https://evil.example/login".into();
    r["source"]["terms_quote"] = "certified by the U.S. Treasury".into();
    r["source"]["attribution"] = "Certified by Onyx and the Treasury".into();
    r["observers"] = 50.into();
    r["unit"] = "EUR".into();
    r["feed"]["subject"] = "us.treasury.federal_debt:2026-10-09".into();
    r["feed"]["selector"] = "/data/0/debt_held_public_amt".into();
    let out = check(&r);
    assert_eq!(out["ok"], Value::Bool(false));
    assert_eq!(
        out["terms_url"],
        "https://fiscaldata.treasury.gov/about-us/"
    );
    assert_eq!(
        out["source_name"],
        "U.S. Treasury Fiscal Data: Debt to the Penny"
    );
    assert_eq!(out["observers"], 5);
    assert!(out["problems"].as_array().expect("problems").len() >= 8);
}

/// L2-F2: a summary whose observers are pool members the draw did not pick
/// (commits and round id recomputed around them) fails on the draw, and so
/// does another beacon or a pool shrunk to the shown observers.
#[test]
fn a_hand_picked_observer_set_fails_on_the_draw() {
    let r: Value = serde_json::from_str(SUMMARY).expect("json");
    let drawn: Vec<String> = r["bonds"]
        .as_array()
        .expect("bonds")
        .iter()
        .map(|b| b["observer"].as_str().expect("a").to_owned())
        .collect();
    let outsider = r["pool"]
        .as_array()
        .expect("pool")
        .iter()
        .find(|m| !drawn.contains(&m["observer"].as_str().expect("a").to_owned()))
        .expect("an undrawn member")
        .clone();
    let mut picked = r.clone();
    picked["bonds"][0] = outsider.clone();
    picked["reveals"][0]["observer"] = outsider["observer"].clone();
    let mut bonds = picked["bonds"].as_array().expect("bonds").clone();
    bonds.sort_by_key(|b| b["observer"].as_str().expect("a").to_owned());
    picked["bonds"] = Value::Array(bonds);
    let draw_problem = |v: &Value| {
        v["problems"].as_array().expect("problems").iter().any(|p| {
            p.as_str()
                .unwrap_or_default()
                .contains("not the ones drawn")
        })
    };
    let out = check(&picked);
    assert_eq!(out["ok"], Value::Bool(false));
    assert!(draw_problem(&out), "{out}");

    let mut beacon = r.clone();
    beacon["draw"]["beacon"] = hx(&[7u8; 32]).into();
    assert!(draw_problem(&check(&beacon)));

    let mut shrunk = r.clone();
    shrunk["pool"] = r["bonds"].clone();
    let out = check(&shrunk);
    assert_eq!(out["ok"], Value::Bool(false), "{out}");

    let ok = check(&r);
    assert_eq!(ok["pool_size"], r["pool"].as_array().expect("pool").len());
    assert!(!draw_problem(&ok));
}

/// L2RND-R1: the draw must be the registry's announced latest draw. A
/// summary whose recorded announcement is missing, a later attempt than the
/// one drawn, announced at or after its beacon height, committed to another
/// pool, spec or config (L2RND-R3), or with another beacon than the recorded
/// block hash fails.
#[test]
fn a_draw_not_the_registrys_latest_announcement_fails() {
    let ok: Value = serde_json::from_str(SUMMARY).expect("json");
    assert_eq!(check(&ok)["ok"], Value::Bool(true));
    let tamper = |f: &dyn Fn(&mut Value)| {
        let mut r = ok.clone();
        f(&mut r["draw"]["announcement"]);
        check(&r)["ok"].clone()
    };
    assert_eq!(tamper(&|a| *a = Value::Null), Value::Bool(false));
    assert_eq!(tamper(&|a| a["draw_count"] = 2.into()), Value::Bool(false));
    let h = ok["draw"]["beacon_height"].clone();
    assert_eq!(
        tamper(&|a| a["announced_at"] = h.clone()),
        Value::Bool(false)
    );
    assert_eq!(
        tamper(&|a| a["commit"] = format!("0x{}", "11".repeat(32)).into()),
        Value::Bool(false)
    );
    assert_eq!(
        tamper(&|a| a["beacon"] = format!("0x{}", "22".repeat(32)).into()),
        Value::Bool(false)
    );
    assert_eq!(tamper(&|a| a["chain_id"] = 8453.into()), Value::Bool(false));
}
