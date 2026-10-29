//! The `ladder` field of every value answer (ADR-0048, NORTH-STAR NS5,
//! docs/spec/delivery.md "Ladder"): the value's level and the rises that
//! got it there, each naming the evidence its lane's check verifies and the
//! chain record that made it public.
//!
//! ```text
//! "ladder": {"value": "0x..", "level": 2,
//!            "steps": [{"level": 2, "name": "re-observed", "evidence": "0x..",
//!                       "at": 1761552000, "record": "OnyxRegistry.ObservationRecorded"}]}
//! ```
//!
//! `value` is [`onyx_core::ladder::value_key`], so the ladder never carries
//! the value itself. A node serves the steps its own lanes hold; a client
//! ([`check_served`]) trusts a step only through its lane's own check, so
//! today it accepts exactly the step of the answer's own lane and refuses a
//! ladder that claims a stronger step it has no check for (THREATS NS5-2).

use onyx_core::ladder::{Contradiction, Ladder, Step};
use onyx_core::{Hash32, ProofLevel};
use serde_json::{json, Value};

/// The chain record of a shard row's level: the registry's attestation
/// (the root itself is `OnyxSettlement`'s `getFinalizedRoot`).
pub const ROW_RECORD: &str = "OnyxRegistry.ShardAttested";
/// The chain record of a level-2 value.
pub const OBSERVATION_RECORD: &str = "OnyxRegistry.ObservationRecorded";
/// The chain record of a level-3 value.
pub const SIGNED_RECORD: &str = "OnyxSignedBook.ReportRecorded";

const LADDER_KEYS: &[&str] = &["value", "level", "steps"];
const STEP_KEYS: &[&str] = &["level", "name", "evidence", "at", "record"];

/// The short name of a level, as the site shows it.
pub fn level_name(level: ProofLevel) -> &'static str {
    match level {
        ProofLevel::Recomputed => "recomputed",
        ProofLevel::ReObserved => "re-observed",
        ProofLevel::SourceProven => "source-proven",
        ProofLevel::BondedClaim => "claim",
        ProofLevel::TrackRecord => "track record",
    }
}

/// The `ladder` JSON of `l`, every step from the lane whose chain record is
/// `record` ([`ROW_RECORD`], [`OBSERVATION_RECORD`]): a node holds only its
/// own lane's steps (module docs).
pub fn to_json(l: &Ladder, record: &'static str) -> Value {
    json!({
        "value": l.value.to_string(),
        "level": l.level().map(ProofLevel::number),
        "steps": l.steps.iter().map(|s| json!({
            "level": s.level.number(),
            "name": level_name(s.level),
            "evidence": s.evidence.to_string(),
            "at": s.at,
            "record": record,
        })).collect::<Vec<_>>(),
    })
}

/// The one-step `ladder` of a level-3 answer: `value` is its
/// [`onyx_core::ladder::value_key`], `report` the report hash, `at` the time
/// the book accepted it. A level-3 value cannot rise on this node (it holds
/// no stronger lane for the fact).
pub fn signed_json(value: Hash32, report: Hash32, at: u64) -> Value {
    let step = Step {
        level: ProofLevel::SourceProven,
        evidence: report,
        at,
    };
    to_json(
        &Ladder {
            value,
            steps: vec![step],
        },
        SIGNED_RECORD,
    )
}

fn hash(v: &Value, what: &str) -> Result<Hash32, String> {
    let s = v
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .ok_or_else(|| format!("{what}: not a 0x hash"))?;
    let mut a = [0u8; 32];
    hex::decode_to_slice(s, &mut a).map_err(|_| format!("{what}: not a 32-byte hash"))?;
    Ok(Hash32(a))
}

fn level(v: &Value, what: &str) -> Result<ProofLevel, String> {
    v.as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .and_then(ProofLevel::from_number)
        .ok_or_else(|| format!("{what}: not a level 1-5"))
}

fn only(v: &Value, keys: &[&str], what: &str) -> Result<(), String> {
    let m = v
        .as_object()
        .ok_or_else(|| format!("{what}: not an object"))?;
    match m.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(format!("{what}.{k}: unexpected field")),
        None => Ok(()),
    }
}

/// A served `ladder` as a [`Ladder`] and its stated level. Strict: unknown
/// fields, a missing field, a step whose name or record is not its level's
/// and lane's are refused; the shape ([`Ladder::check_stated`]) is checked
/// against the stated `level`.
pub fn parse(v: &Value, record: &'static str) -> Result<Ladder, String> {
    only(v, LADDER_KEYS, "ladder")?;
    let stated = level(&v["level"], "ladder.level")?;
    let raw = v["steps"].as_array().ok_or("ladder.steps: not an array")?;
    // Bound the work before reading the steps (one per level at most).
    if raw.len() > onyx_core::ladder::MAX_STEPS {
        return Err(format!(
            "ladder.steps: {} steps, at most {}",
            raw.len(),
            onyx_core::ladder::MAX_STEPS
        ));
    }
    let steps = raw
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let what = format!("ladder.steps[{i}]");
            only(s, STEP_KEYS, &what)?;
            let l = level(&s["level"], &what)?;
            if s["name"].as_str() != Some(level_name(l)) {
                return Err(format!("{what}.name is not level {}'s", l.number()));
            }
            if s["record"].as_str() != Some(record) {
                return Err(format!("{what}.record is not {record}"));
            }
            Ok(Step {
                level: l,
                evidence: hash(&s["evidence"], &what)?,
                at: s["at"]
                    .as_u64()
                    .ok_or_else(|| format!("{what}.at: not a time"))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let l = Ladder {
        value: hash(&v["value"], "ladder.value")?,
        steps,
    };
    l.check_stated(stated).map_err(|e| e.to_string())?;
    Ok(l)
}

/// A client's check of an answer's `ladder` (`served`, `None` for a node
/// that predates it). `own` is the one step the client verified itself
/// through the answer's lane (its value key, level, evidence and time);
/// `stated` is the answer's own level field; `record` the lane's chain
/// record; `known` the ladder the client last accepted for the same value.
///
/// Refused: a ladder of another value, one whose top is not `stated`
/// (a forged level), one without the client's own step, one with a step
/// stronger than the own step (a rise this client has no check for), and
/// one that drops or rewrites a step of `known` (a fall). Returns the
/// accepted ladder, to keep as the next `known`.
pub fn check_served(
    served: Option<&Value>,
    own: &Ladder,
    stated: ProofLevel,
    record: &'static str,
    known: Option<&Ladder>,
) -> Result<Ladder, String> {
    let l = match served {
        None => own.clone(),
        Some(v) => parse(v, record)?,
    };
    if l.value != own.value {
        return Err("the ladder is of another value".into());
    }
    l.check_stated(stated).map_err(|e| e.to_string())?;
    let mine = own.steps.last().ok_or("no own step")?;
    if !l.steps.contains(mine) {
        return Err("the ladder does not hold the answer's own proof".into());
    }
    if let Some(s) = l
        .steps
        .iter()
        .find(|s| s.level.number() < mine.level.number())
    {
        return Err(format!(
            "level {} on {} is stronger than this answer's own proof, and no check of it ran",
            s.level.number(),
            s.evidence
        ));
    }
    if let Some(k) = known {
        l.check_extends(k).map_err(|e| e.to_string())?;
    }
    Ok(l)
}

/// The `v` of a cross-lane ladder (ADR-0048 amendment 2). Served only when
/// asked (`?ladder=2`): a slice-1 client reads the default answer's
/// one-lane ladder and never sees a step it would refuse (THREATS NS5-11).
pub const LADDER_V2: u64 = 2;

const V2_KEYS: &[&str] = &["v", "value", "level", "steps", "contradictions"];
const V2_CONTRA_KEYS: &[&str] = &["level", "name", "evidence", "at", "record", "value"];

/// The chain records a step can name.
pub const RECORDS: [&str; 3] = [ROW_RECORD, OBSERVATION_RECORD, SIGNED_RECORD];

/// A step and the chain record of the lane that made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneStep {
    /// The rise.
    pub step: Step,
    /// Its lane's chain record (one of [`RECORDS`]).
    pub record: &'static str,
}

/// A contradiction and the chain record of its lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneContradiction {
    /// The stronger answer.
    pub contradiction: Contradiction,
    /// Its lane's chain record (one of [`RECORDS`]).
    pub record: &'static str,
}

/// The ladder of one value from the proofs of every lane a node holds for
/// its fact (same [`onyx_core::ladder::value_key`]): the rises in chain-time
/// order, each step keeping its lane's record. `None` when no proof names
/// evidence.
pub fn compose(value: Hash32, proofs: &[LaneStep]) -> Option<(Ladder, Vec<LaneStep>)> {
    let steps: Vec<Step> = proofs.iter().map(|p| p.step).collect();
    let l = Ladder::of(value, &steps)?;
    // Every step of `l` is one of `proofs` (`Ladder::of` only keeps them).
    let lanes: Vec<LaneStep> = l
        .steps
        .iter()
        .filter_map(|s| proofs.iter().find(|p| p.step == *s).copied())
        .collect();
    (lanes.len() == l.steps.len()).then_some((l, lanes))
}

/// The cross-lane `ladder` JSON (`v: 2`): every step with its own lane's
/// record, and the stronger answers of the same fact that state another
/// value (`contradictions`, each with that value's hash only, R-LAD2-4).
pub fn to_json_v2(
    value: Hash32,
    steps: &[LaneStep],
    contradictions: &[LaneContradiction],
) -> Value {
    json!({
        "v": LADDER_V2,
        "value": value.to_string(),
        "level": steps.last().map(|s| s.step.level.number()),
        "steps": steps.iter().map(|s| json!({
            "level": s.step.level.number(),
            "name": level_name(s.step.level),
            "evidence": s.step.evidence.to_string(),
            "at": s.step.at,
            "record": s.record,
        })).collect::<Vec<_>>(),
        "contradictions": contradictions.iter().map(|c| json!({
            "level": c.contradiction.level.number(),
            "name": level_name(c.contradiction.level),
            "evidence": c.contradiction.evidence.to_string(),
            "at": c.contradiction.at,
            "record": c.record,
            "value": c.contradiction.value.to_string(),
        })).collect::<Vec<_>>(),
    })
}

fn record_of(v: &Value, what: &str) -> Result<&'static str, String> {
    let s = v.as_str().unwrap_or_default();
    RECORDS
        .iter()
        .find(|r| **r == s)
        .copied()
        .ok_or_else(|| format!("{what}.record: not a known chain record"))
}

/// A served cross-lane ladder. Strict like [`parse`]; every record must be
/// a known lane's, and the shape is checked against the stated `level`.
pub fn parse_v2(v: &Value) -> Result<(Ladder, Vec<LaneStep>, Vec<LaneContradiction>), String> {
    only(v, V2_KEYS, "ladder")?;
    if v["v"].as_u64() != Some(LADDER_V2) {
        return Err("ladder.v: not 2".into());
    }
    let stated = level(&v["level"], "ladder.level")?;
    let raw = v["steps"].as_array().ok_or("ladder.steps: not an array")?;
    let rawc = v["contradictions"]
        .as_array()
        .ok_or("ladder.contradictions: not an array")?;
    let max = onyx_core::ladder::MAX_STEPS;
    if raw.len() > max || rawc.len() > max {
        return Err(format!(
            "ladder: at most {max} steps and {max} contradictions"
        ));
    }
    let mut steps = Vec::with_capacity(raw.len());
    for (i, s) in raw.iter().enumerate() {
        let what = format!("ladder.steps[{i}]");
        only(s, STEP_KEYS, &what)?;
        let l = level(&s["level"], &what)?;
        if s["name"].as_str() != Some(level_name(l)) {
            return Err(format!("{what}.name is not level {}'s", l.number()));
        }
        steps.push(LaneStep {
            step: Step {
                level: l,
                evidence: hash(&s["evidence"], &what)?,
                at: s["at"]
                    .as_u64()
                    .ok_or_else(|| format!("{what}.at: not a time"))?,
            },
            record: record_of(&s["record"], &what)?,
        });
    }
    let mut cs = Vec::with_capacity(rawc.len());
    for (i, c) in rawc.iter().enumerate() {
        let what = format!("ladder.contradictions[{i}]");
        only(c, V2_CONTRA_KEYS, &what)?;
        let l = level(&c["level"], &what)?;
        if c["name"].as_str() != Some(level_name(l)) {
            return Err(format!("{what}.name is not level {}'s", l.number()));
        }
        cs.push(LaneContradiction {
            contradiction: Contradiction {
                level: l,
                evidence: hash(&c["evidence"], &what)?,
                at: c["at"]
                    .as_u64()
                    .ok_or_else(|| format!("{what}.at: not a time"))?,
                value: hash(&c["value"], &format!("{what}.value"))?,
            },
            record: record_of(&c["record"], &what)?,
        });
    }
    let l = Ladder {
        value: hash(&v["value"], "ladder.value")?,
        steps: steps.iter().map(|s| s.step).collect(),
    };
    l.check_stated(stated).map_err(|e| e.to_string())?;
    Ok((l, steps, cs))
}

/// A cross-lane ladder a client accepted, and the contradictions it carried
/// (shown next to the value; they never change its level, R-LAD2-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedV2 {
    /// The accepted ladder (keep it as the next `known`).
    pub ladder: Ladder,
    /// Its steps with their lanes.
    pub steps: Vec<LaneStep>,
    /// Stronger answers of the same fact with another value, as the node
    /// stated them (shape checked, not their lanes).
    pub contradictions: Vec<LaneContradiction>,
}

/// A client's check of a cross-lane ladder (`?ladder=2`, R-LAD2-1). Like
/// [`check_served`], and every step stronger than the client's own step is
/// trusted only after `lane_check` (the client's check of that step through
/// its own lane: [`crate::client::verify_observation`] for level 2, the
/// signed check for level 3, the root and `verifyRowLeveled` for rows)
/// passed for it; a step it cannot check refuses the whole ladder, never
/// trims it. `own_value` is the client's own [`onyx_core::ladder::ladder_value_hash`]
/// of the value it holds.
#[allow(clippy::too_many_arguments)]
pub fn check_served_v2(
    served: &Value,
    own: &Ladder,
    own_record: &'static str,
    own_value: &Hash32,
    stated: ProofLevel,
    known: Option<&Ladder>,
    lane_check: &mut dyn FnMut(&LaneStep) -> Result<(), String>,
) -> Result<CheckedV2, String> {
    let (l, steps, contradictions) = parse_v2(served)?;
    if l.value != own.value {
        return Err("the ladder is of another value".into());
    }
    l.check_stated(stated).map_err(|e| e.to_string())?;
    let mine = own.steps.last().ok_or("no own step")?;
    let own_lane = LaneStep {
        step: *mine,
        record: own_record,
    };
    if !steps.contains(&own_lane) {
        return Err("the ladder does not hold the answer's own proof".into());
    }
    for s in steps
        .iter()
        .filter(|s| s.step.level.number() < mine.level.number())
    {
        lane_check(s).map_err(|e| {
            format!(
                "level {} on {} ({}) is stronger than this answer's own proof and its lane check did not pass: {e}",
                s.step.level.number(),
                s.step.evidence,
                s.record
            )
        })?;
    }
    let cs: Vec<Contradiction> = contradictions.iter().map(|c| c.contradiction).collect();
    l.check_contradictions(&cs, own_value)
        .map_err(|e| e.to_string())?;
    if let Some(k) = known {
        l.check_extends(k).map_err(|e| e.to_string())?;
    }
    Ok(CheckedV2 {
        ladder: l,
        steps,
        contradictions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::ladder::value_key;
    use ProofLevel::*;

    fn h(n: u8) -> Hash32 {
        Hash32([n; 32])
    }

    fn own() -> Ladder {
        Ladder::new(
            value_key(&h(1), &h(2)),
            Step {
                level: ReObserved,
                evidence: h(3),
                at: 100,
            },
        )
        .expect("ok")
    }

    fn served(l: &Ladder) -> Value {
        to_json(l, OBSERVATION_RECORD)
    }

    #[test]
    fn the_own_step_round_trips() {
        let o = own();
        let v = served(&o);
        assert_eq!(v["level"], 2);
        assert_eq!(v["steps"][0]["name"], "re-observed");
        assert_eq!(v["steps"][0]["record"], OBSERVATION_RECORD);
        assert_eq!(parse(&v, OBSERVATION_RECORD).expect("ok"), o);
        assert_eq!(
            check_served(Some(&v), &o, ReObserved, OBSERVATION_RECORD, None).expect("ok"),
            o
        );
        // An older node without the field: the client's own step.
        assert_eq!(
            check_served(None, &o, ReObserved, OBSERVATION_RECORD, None).expect("ok"),
            o
        );
    }

    #[test]
    fn a_forged_level_is_refused() {
        let o = own();
        let mut v = served(&o);
        // The answer states level 1 while its ladder reaches 2.
        assert!(check_served(Some(&v), &o, Recomputed, OBSERVATION_RECORD, None).is_err());
        assert!(check_served(None, &o, Recomputed, OBSERVATION_RECORD, None).is_err());
        // The ladder's own level field lifted to 1.
        v["level"] = json!(1);
        assert!(check_served(Some(&v), &o, Recomputed, OBSERVATION_RECORD, None).is_err());
        // A step relabelled level 1 on the same evidence.
        let mut w = served(&o);
        w["level"] = json!(1);
        w["steps"][0]["level"] = json!(1);
        w["steps"][0]["name"] = json!("recomputed");
        assert!(check_served(Some(&w), &o, Recomputed, OBSERVATION_RECORD, None).is_err());
        // A stronger step added on top, with nothing that checks it.
        let mut risen = o.clone();
        risen
            .raise(Step {
                level: Recomputed,
                evidence: h(4),
                at: 200,
            })
            .expect("ok");
        let err = check_served(
            Some(&served(&risen)),
            &o,
            Recomputed,
            OBSERVATION_RECORD,
            None,
        )
        .expect_err("refused");
        assert!(err.contains("no check of it ran"), "{err}");
        // A name or record that is not the level's or the lane's.
        let mut n = served(&o);
        n["steps"][0]["name"] = json!("recomputed");
        assert!(parse(&n, OBSERVATION_RECORD).is_err());
        assert!(parse(&served(&o), SIGNED_RECORD).is_err());
        let mut x = served(&o);
        x["steps"][0]["extra"] = json!(1);
        assert!(parse(&x, OBSERVATION_RECORD).is_err());
    }

    #[test]
    fn a_fall_is_refused() {
        let o = own();
        // The client saw level 2 from a weaker start; a later answer that
        // drops the level-2 step (back to the level-3 start) is a fall.
        let start = Step {
            level: SourceProven,
            evidence: h(5),
            at: 50,
        };
        let mut seen = Ladder::new(o.value, start).expect("ok");
        seen.raise(o.steps[0]).expect("ok");
        let fell = Ladder::new(o.value, start).expect("ok");
        let mine3 = Ladder::new(o.value, start).expect("ok");
        assert!(check_served(
            Some(&to_json(&fell, OBSERVATION_RECORD)),
            &mine3,
            SourceProven,
            OBSERVATION_RECORD,
            Some(&seen)
        )
        .is_err());
        // A served ladder whose steps fall is refused by its shape.
        let mut v = served(&seen);
        v["steps"] = json!([v["steps"][1], v["steps"][0]]);
        assert!(parse(&v, OBSERVATION_RECORD).is_err());
        // The same ladder again extends what was seen.
        check_served(
            Some(&served(&seen)),
            &o,
            ReObserved,
            OBSERVATION_RECORD,
            Some(&seen),
        )
        .expect("ok");
    }

    #[test]
    fn another_value_or_a_missing_own_step_is_refused() {
        let o = own();
        let other = Ladder::new(value_key(&h(1), &h(9)), o.steps[0]).expect("ok");
        assert!(check_served(
            Some(&served(&other)),
            &o,
            ReObserved,
            OBSERVATION_RECORD,
            None
        )
        .is_err());
        let moved = Ladder::new(
            o.value,
            Step {
                level: ReObserved,
                evidence: h(8),
                at: 100,
            },
        )
        .expect("ok");
        assert!(check_served(
            Some(&served(&moved)),
            &o,
            ReObserved,
            OBSERVATION_RECORD,
            None
        )
        .is_err());
    }

    fn three() -> LaneStep {
        LaneStep {
            step: Step {
                level: SourceProven,
                evidence: h(6),
                at: 50,
            },
            record: SIGNED_RECORD,
        }
    }

    fn two() -> LaneStep {
        LaneStep {
            step: own().steps[0],
            record: OBSERVATION_RECORD,
        }
    }

    fn stronger() -> LaneStep {
        LaneStep {
            step: Step {
                level: Recomputed,
                evidence: h(7),
                at: 150,
            },
            record: ROW_RECORD,
        }
    }

    #[test]
    fn a_node_with_both_lanes_serves_both_steps() {
        let o = own();
        // A level-3 report (at 50) and the level-2 round (at 100) of one fact.
        let (l, lanes) = compose(o.value, &[two(), three()]).expect("ok");
        assert_eq!(lanes, vec![three(), two()]);
        assert_eq!(l.level(), Some(ReObserved));
        let v = to_json_v2(o.value, &lanes, &[]);
        assert_eq!(v["v"], 2);
        assert_eq!(v["steps"][0]["record"], SIGNED_RECORD);
        assert_eq!(v["steps"][1]["record"], OBSERVATION_RECORD);
        let (back, steps, cs) = parse_v2(&v).expect("ok");
        assert_eq!((back, steps, cs.len()), (l, lanes, 0));
        // The level-2 client checks only its own step here: the level-3
        // step is weaker and needs no check to stand under it.
        let mut ran = 0;
        let c = check_served_v2(
            &v,
            &o,
            OBSERVATION_RECORD,
            &h(2),
            ReObserved,
            None,
            &mut |_| {
                ran += 1;
                Ok(())
            },
        )
        .expect("ok");
        assert_eq!(ran, 0);
        assert_eq!(c.steps.len(), 2);
        // A level-3 client sees the level-2 step rise above its own: it
        // passes only after the level-2 check ran for it.
        let mine3 = Ladder::new(o.value, three().step).expect("ok");
        let mut checked = vec![];
        check_served_v2(
            &v,
            &mine3,
            SIGNED_RECORD,
            &h(2),
            ReObserved,
            None,
            &mut |s| {
                checked.push(*s);
                Ok(())
            },
        )
        .expect("ok");
        assert_eq!(checked, vec![two()]);
    }

    #[test]
    fn a_cross_lane_step_without_its_lane_check_is_refused() {
        let o = own();
        let (_, lanes) = compose(o.value, &[two(), stronger()]).expect("ok");
        let v = to_json_v2(o.value, &lanes, &[]);
        let err = check_served_v2(
            &v,
            &o,
            OBSERVATION_RECORD,
            &h(2),
            Recomputed,
            None,
            &mut |_| Err("no row check on this client".into()),
        )
        .expect_err("refused");
        assert!(err.contains("lane check did not pass"), "{err}");
        // With the check, the rise stands.
        let c = check_served_v2(
            &v,
            &o,
            OBSERVATION_RECORD,
            &h(2),
            Recomputed,
            None,
            &mut |_| Ok(()),
        )
        .expect("ok");
        assert_eq!(c.ladder.level(), Some(Recomputed));
        // An unknown record, or the own step moved to another lane: refused.
        let mut x = v.clone();
        x["steps"][1]["record"] = json!("Elsewhere.Record");
        assert!(parse_v2(&x).is_err());
        let mut y = v.clone();
        y["steps"][0]["record"] = json!(SIGNED_RECORD);
        assert!(check_served_v2(
            &y,
            &o,
            OBSERVATION_RECORD,
            &h(2),
            Recomputed,
            None,
            &mut |_| Ok(())
        )
        .is_err());
    }

    #[test]
    fn a_weaker_value_names_the_stronger_contradiction() {
        let o = own();
        let c = LaneContradiction {
            contradiction: Contradiction {
                level: Recomputed,
                evidence: h(8),
                at: 120,
                value: h(9),
            },
            record: ROW_RECORD,
        };
        let v = to_json_v2(o.value, &[two()], &[c]);
        assert_eq!(v["contradictions"][0]["value"], h(9).to_string());
        let got = check_served_v2(
            &v,
            &o,
            OBSERVATION_RECORD,
            &h(2),
            ReObserved,
            None,
            &mut |_| Ok(()),
        )
        .expect("ok");
        assert_eq!(got.contradictions, vec![c]);
        // The level stays the value's own (R-LAD2-3).
        assert_eq!(got.ladder.level(), Some(ReObserved));
        // A "contradiction" that states this same value, or is weaker: refused.
        let same = to_json_v2(
            o.value,
            &[two()],
            &[LaneContradiction {
                contradiction: Contradiction {
                    value: h(2),
                    ..c.contradiction
                },
                ..c
            }],
        );
        assert!(check_served_v2(
            &same,
            &o,
            OBSERVATION_RECORD,
            &h(2),
            ReObserved,
            None,
            &mut |_| Ok(())
        )
        .is_err());
        let weak = to_json_v2(
            o.value,
            &[two()],
            &[LaneContradiction {
                contradiction: Contradiction {
                    level: SourceProven,
                    ..c.contradiction
                },
                ..c
            }],
        );
        assert!(check_served_v2(
            &weak,
            &o,
            OBSERVATION_RECORD,
            &h(2),
            ReObserved,
            None,
            &mut |_| Ok(())
        )
        .is_err());
    }

    #[test]
    fn a_v1_client_accepts_a_v2_nodes_default_answer() {
        // The default answer stays the one-lane ladder; a slice-1 client
        // (`check_served`) reads it, and refuses the v2 form it never asked for.
        let o = own();
        let default = to_json(&o, OBSERVATION_RECORD);
        check_served(Some(&default), &o, ReObserved, OBSERVATION_RECORD, None).expect("ok");
        let v2 = to_json_v2(o.value, &[two()], &[]);
        assert!(parse(&v2, OBSERVATION_RECORD).is_err());
        assert!(parse_v2(&default).is_err());
    }
}
