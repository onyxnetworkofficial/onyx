//! A level-2 client against a live registry (ROADMAP C11): read the results
//! store the live coordinator wrote, serve every fact's value round with its
//! earlier rounds (`?outcome=all`) as the query API does, and check each answer
//! as a client: off chain (`verify_observation`: transcript, every sealed post's
//! key proof, every dismissal), then on chain with the client's own arguments
//! (`observationLevel`, `drawMatches`, `drawBudget`, `transcriptComplete`, and,
//! per round, `drawRevealKey`, `revealLogComplete`, every served post against
//! `sealedRevealOf`/`sealedKeyProofOf`, `attemptMatches`; the fact's draws with
//! `drawCount`/`drawOf`; ROADMAP C51: `drawTimeLocked`, and every time-locked
//! dismissal re-encrypted from its opening or checked under the lock round's
//! signature, verified against the pinned quicknet key). Then it serves the
//! same answers doctored the ways a server could cheat (C11-4..6, C51-*) and
//! requires the client to refuse each.
//!
//! `cargo run -p onyx-source-fiscaldata --example level2-verify --
//!   --rpc URL --registry ADDR --store PATH [--cast PATH]`
//!
//! Prints a JSON summary. Rehearsal tool (`scripts/level2-sealed.sh`).

use onyx_api::client::{
    draw_count_from_return, verify_attempts_onchain, verify_observation,
    verify_observation_onchain, ObservationExpect, SealedCalls, SealedRets, VerifiedObservation,
};
use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_coordinator::cast_chain::CastChain;
use onyx_coordinator::observe::{Clock, SystemClock};
use serde_json::{json, Value};

fn main() {
    match run() {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("level2-verify: {e}");
            std::process::exit(1);
        }
    }
}

fn sealed_rets(chain: &CastChain, calls: SealedCalls) -> Result<SealedRets, String> {
    let ask = |d: &[u8]| chain.eth_call(d, None);
    let mut posts = vec![];
    for (r, p) in &calls.posts {
        posts.push((ask(r)?, ask(p)?));
    }
    Ok(SealedRets {
        time_locked: ask(&calls.time_locked)?,
        reveal_key: ask(&calls.reveal_key)?,
        reveal_log: ask(&calls.reveal_log)?,
        matches: match &calls.matches {
            Some(m) => Some(ask(m)?),
            None => None,
        },
        posts,
    })
}

/// Every on-chain check of a value answer with its earlier rounds.
fn onchain(chain: &CastChain, v: &VerifiedObservation) -> Result<(), String> {
    let ask = |d: &[u8]| chain.eth_call(d, None);
    let calls = v.onchain_calls();
    let rets: Vec<Vec<u8>> = calls.iter().map(|d| ask(d)).collect::<Result<_, _>>()?;
    let sealed = sealed_rets(chain, v.sealed_calls())?;
    verify_observation_onchain(v, &rets[0], &rets[1], &rets[2], &rets[3], &sealed)
        .map_err(|e| format!("value round: {e}"))?;
    let a = v
        .attempts
        .as_ref()
        .ok_or("no attempts (ask with outcome=all)")?;
    let count_ret = ask(&a.draw_count_calldata())?;
    let count = draw_count_from_return(&count_ret).map_err(|e| e.to_string())?;
    let draws: Vec<Vec<u8>> = (0..a.draws_without_value(count))
        .map(|i| ask(&a.draw_of_calldata(i)))
        .collect::<Result<_, _>>()?;
    let complete: Vec<Vec<u8>> = a
        .transcript_complete_calldatas()
        .iter()
        .map(|d| ask(d))
        .collect::<Result<_, _>>()?;
    let sealed: Vec<SealedRets> = a
        .sealed_calls()
        .into_iter()
        .map(|c| sealed_rets(chain, c))
        .collect::<Result<_, _>>()?;
    verify_attempts_onchain(
        a,
        &count_ret,
        &draws,
        &ask(&a.draw_budget_calldata())?,
        &complete,
        &sealed,
    )
    .map_err(|e| format!("earlier rounds: {e}"))?;
    Ok(())
}

fn run() -> Result<Value, String> {
    let mut args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let store = flag("--store").ok_or("--store PATH is required")?;
    // A client sends nothing: the sender is never used.
    args.extend([
        "--coordinator".into(),
        "0x0000000000000000000000000000000000000001".into(),
    ]);
    let chain = CastChain::from_args(&args)?.ok_or("--rpc URL is required")?;
    let t = onyx_source_fiscaldata::template();
    let pins = TemplatePins::from([(t.metric.0.clone(), t.id())]);
    let now = SystemClock.now();
    let text = std::fs::read_to_string(&store).map_err(|e| format!("{store}: {e}"))?;
    let feed = ObservationStore::from_json(&text, now, &pins).map_err(|e| e.to_string())?;
    let mut checked = vec![];
    for rec in feed.records.values() {
        let attempts = feed.attempts_of(&rec.checked.fact);
        let body = feed.body_all(rec, &attempts);
        let subject = rec.transcript.subject.clone();
        let expect = ObservationExpect {
            template: &t,
            subject: &subject,
            window_end: None,
            min_window_end: 0,
            now,
        };
        let v = verify_observation(&body, &expect).map_err(|e| format!("off chain: {e}"))?;
        onchain(&chain, &v)?;
        let a = v.attempts.as_ref().ok_or("no attempts")?;
        // Doctored answers (C11-4..6): each must be refused.
        let mut refused = vec![];
        let try_doctored = |name: &str, b: &Value, refused: &mut Vec<Value>| {
            let r = verify_observation(b, &expect)
                .map_err(|e| e.to_string())
                .and_then(|v| onchain(&chain, &v));
            refused.push(json!({ "case": name, "refused": r.is_err(), "why": r.err() }));
        };
        let sealed_of = |i: usize| body["attempts"][i].get("sealed").is_some();
        if let Some(i) = (0..a.attempts.len()).find(|i| sealed_of(*i)) {
            let mut b = body.clone();
            if let Some(m) = b["attempts"][i].as_object_mut() {
                m.remove("sealed");
            }
            try_doctored(
                "an earlier round served without its sealed posts",
                &b,
                &mut refused,
            );
            let mut b = body.clone();
            if let Some(d) = b["attempts"][i]["sealed"]["dismissals"].as_array_mut() {
                d.pop();
            }
            try_doctored(
                "an earlier round with a dismissal left out",
                &b,
                &mut refused,
            );
            let mut b = body.clone();
            b["attempts"][i]["outcome"] = json!("sourceUnavailable");
            try_doctored("an earlier round with another outcome", &b, &mut refused);
            // ROADMAP C51: a time-locked round's read.
            if body["attempts"][i]["sealed"].get("timeLock").is_some() {
                let mut b = body.clone();
                if let Some(m) = b["attempts"][i]["sealed"].as_object_mut() {
                    m.remove("timeLock");
                }
                try_doctored(
                    "an earlier time-locked round served as not time-locked",
                    &b,
                    &mut refused,
                );
                let flip = |v: &mut Value| {
                    if let Some(h) = v.as_str() {
                        let mut c: Vec<char> = h.chars().collect();
                        if let Some(last) = c.last_mut() {
                            *last = if *last == '0' { '1' } else { '0' };
                        }
                        *v = json!(c.into_iter().collect::<String>());
                    }
                };
                let mut b = body.clone();
                flip(&mut b["attempts"][i]["sealed"]["timeLock"]["beacon"]);
                try_doctored(
                    "an earlier round with a forged lock-round signature",
                    &b,
                    &mut refused,
                );
                // ROADMAP C52: another far round than the template's.
                let mut b = body.clone();
                let far = b["attempts"][i]["sealed"]["timeLock"]["farRound"]
                    .as_u64()
                    .unwrap_or(0);
                b["attempts"][i]["sealed"]["timeLock"]["farRound"] = json!(far + 1);
                try_doctored(
                    "an earlier round naming another far round",
                    &b,
                    &mut refused,
                );
                if body["attempts"][i]["sealed"]["timeLock"]["openings"]
                    .as_array()
                    .is_some_and(|o| !o.is_empty())
                {
                    let mut b = body.clone();
                    flip(&mut b["attempts"][i]["sealed"]["timeLock"]["openings"][0]["tau"]);
                    try_doctored(
                        "an earlier round with a time-lock opening changed",
                        &b,
                        &mut refused,
                    );
                    let mut b = body.clone();
                    if let Some(o) =
                        b["attempts"][i]["sealed"]["timeLock"]["openings"].as_array_mut()
                    {
                        o.remove(0);
                    }
                    try_doctored(
                        "an earlier round with a time-lock opening left out",
                        &b,
                        &mut refused,
                    );
                }
            }
        }
        if body.get("sealed").is_some() {
            let mut b = body.clone();
            if let Some(m) = b.as_object_mut() {
                m.remove("sealed");
            }
            try_doctored(
                "the value round served without its sealed posts",
                &b,
                &mut refused,
            );
            if body["sealed"].get("timeLock").is_some() {
                let mut b = body.clone();
                if let Some(m) = b["sealed"].as_object_mut() {
                    m.remove("timeLock");
                }
                try_doctored(
                    "the time-locked value round served as not time-locked",
                    &b,
                    &mut refused,
                );
            }
        }
        checked.push(json!({
            "value": v.value,
            "round": v.checked.round_id.to_string(),
            "attempt": v.checked.draw.attempt,
            "value_round_sealed_posts": v.checked.reveal_log.posts.len(),
            "value_round_time_locked": v.checked.reveal_log.time_locked,
            "earlier_rounds": a.attempts.iter().map(|c| json!({
                "attempt": c.draw.attempt,
                "outcome": format!("{:?}", c.outcome),
                "sealed_posts": c.reveal_log.posts.len(),
                "time_locked": c.reveal_log.time_locked,
                "dismissed": c.reveal_log.query.entries.iter().filter(|e| e.status == onyx_coordinator::sealed::EntryStatus::Dismissed).count(),
            })).collect::<Vec<_>>(),
            "doctored": refused,
        }));
    }
    Ok(json!({ "facts": checked, "client_checks": "passed" }))
}
