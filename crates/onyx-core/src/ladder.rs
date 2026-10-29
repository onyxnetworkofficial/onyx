//! The ladder of one value (ADR-0048, docs/spec/proof-ladder.md "The ladder
//! in the product", NORTH-STAR NS5).
//!
//! A value's level is the strongest public proof that backs exactly that
//! value. Proofs only accumulate (each is a chain record of its own lane:
//! a finalized shard, a recorded level-2 observation, a settled level-3
//! report), so the level only rises, and its history is the list of rises:
//! [`Step`]s, each strictly stronger than the one before, in time order.
//!
//! Nothing here decides a level by itself: every step names the evidence its
//! lane's own check verifies (a shard root, a level-2 result hash, a level-3
//! report hash). A client trusts a step only after that check passes; a
//! served ladder whose top differs from the answer's level, whose steps do
//! not rise, or that drops or rewrites a step the client saw before is
//! refused ([`Ladder::check`], [`Ladder::check_extends`]).

use crate::claim::{MetricId, Subject};
use crate::hash::{keccak256, Hash32};
use crate::proof::{Answer, ObservationKind, ProofLevel};
use crate::value::Value;

/// Domain of [`value_key`].
pub const LADDER_VALUE_DOMAIN: &[u8] = b"onyx-ladder-value-v1";
/// Domain of [`fact_key`].
pub const LADDER_FACT_DOMAIN: &[u8] = b"onyx-ladder-fact-v1";
/// Domain of [`row_key`].
pub const LADDER_ROW_DOMAIN: &[u8] = b"onyx-ladder-row-v1";

/// `keccak256("onyx-ladder-fact-v1" ‖ borsh(metric, subject, as_of))`: a fact
/// with no lane and no source in its key, so a level-2 observation (as of its
/// window end) and a level-3 report (as of its observed time) of the same
/// metric, subject and time are one fact.
pub fn fact_key(metric: &MetricId, subject: &Subject, as_of: u64) -> Hash32 {
    let mut b = LADDER_FACT_DOMAIN.to_vec();
    b.extend_from_slice(&borsh::to_vec(&(metric, subject, as_of)).unwrap_or_default());
    keccak256(&b)
}

/// `keccak256("onyx-ladder-row-v1" ‖ round (u64 BE) ‖ shard ‖ index (u64 BE))`:
/// one row of one finalized shard.
pub fn row_key(round: u64, shard: &Hash32, index: u64) -> Hash32 {
    let mut b = LADDER_ROW_DOMAIN.to_vec();
    b.extend_from_slice(&round.to_be_bytes());
    b.extend_from_slice(&shard.0);
    b.extend_from_slice(&index.to_be_bytes());
    keccak256(&b)
}

/// `keccak256(borsh(value))`: the value hash `OnyxSignedBook` records.
pub fn value_hash(value: &Value) -> Hash32 {
    keccak256(&borsh::to_vec(value).unwrap_or_default())
}

/// One form per number, so two lanes that state the same number match
/// (THREATS NS5-12, ADR-0048 amendment 2): trailing zero digits of a
/// `Fixed` are dropped (`28.30` at scale 2 is `28.3` at scale 1), a `Fixed`
/// left at scale 0 is an `Int`, and a `UInt` that fits an `i128` is an
/// `Int`. `Bool` and `Bytes` are unchanged.
pub fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Fixed {
            mantissa,
            mut scale,
        } => {
            let mut m = *mantissa;
            while scale > 0 && m % 10 == 0 {
                m /= 10;
                scale -= 1;
            }
            if scale == 0 {
                Value::Int(m)
            } else {
                Value::Fixed { mantissa: m, scale }
            }
        }
        Value::UInt(n) => i128::try_from(*n).map_or(Value::UInt(*n), Value::Int),
        v => v.clone(),
    }
}

/// [`value_hash`] of the [`canonical_value`]: the value half of a ladder's
/// [`value_key`] in every lane (the book's own `valueHash` stays
/// [`value_hash`], the hash it recorded).
pub fn ladder_value_hash(value: &Value) -> Hash32 {
    value_hash(&canonical_value(value))
}

/// The value a settled level-2 answer stands for: bytes for an exact feed,
/// `Int` at scale 0, else `Fixed` at the feed's scale; `None` for anything
/// else (the rule a level-2 claim is matched by).
pub fn observed_value(kind: &ObservationKind, answer: &Answer) -> Option<Value> {
    match (kind, answer) {
        (ObservationKind::Exact { .. }, Answer::Bytes(b)) => Some(Value::Bytes(b.clone())),
        (ObservationKind::Numeric { scale: 0, .. }, Answer::Number(n)) => Some(Value::Int(*n)),
        (ObservationKind::Numeric { scale, .. }, Answer::Number(n)) => Some(Value::Fixed {
            mantissa: *n,
            scale: *scale,
        }),
        _ => None,
    }
}
/// Most steps a ladder can have: one per level.
pub const MAX_STEPS: usize = ProofLevel::ALL.len();

/// `keccak256("onyx-ladder-value-v1" ‖ lane-free key ‖ value hash)`: the one
/// value a ladder is about. `key` names what the value answers (a level-2 or
/// level-3 fact's lane-free key, or a shard row's key); `value` is the
/// Keccak-256 of the value's bytes, so the ladder never carries the value
/// itself (a sealed answer's ladder shows no value).
pub fn value_key(key: &Hash32, value: &Hash32) -> Hash32 {
    let mut b = LADDER_VALUE_DOMAIN.to_vec();
    b.extend_from_slice(&key.0);
    b.extend_from_slice(&value.0);
    keccak256(&b)
}

/// One rise: the value reached `level` on `evidence` at `at` (unix seconds,
/// when the proof settled as its lane binds it: a level-2 round's settle
/// time, a level-3 report's acceptance, a shard's finalization).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The level this proof gives.
    pub level: ProofLevel,
    /// What the lane's check verifies: a shard root, a level-2 result hash
    /// or a level-3 report hash. Never zero.
    pub evidence: Hash32,
    /// When the proof settled.
    pub at: u64,
}

/// Why a ladder or a step was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LadderError {
    /// A ladder needs at least one step.
    #[error("a ladder has no step")]
    Empty,
    /// More steps than levels.
    #[error("{0} steps, at most {MAX_STEPS}")]
    TooLong(usize),
    /// A step names no evidence.
    #[error("step {0} names no evidence")]
    NoEvidence(usize),
    /// A step is not stronger than the one before: a level never falls, and a
    /// proof at the level already reached is no rise.
    #[error("step {index} is level {got}, not stronger than level {before}")]
    NotStronger {
        /// The step's index.
        index: usize,
        /// Its level number.
        got: u8,
        /// The level number before it.
        before: u8,
    },
    /// A step is older than the one before.
    #[error("step {index} at {got} is before the step before it ({before})")]
    TimeWentBack {
        /// The step's index.
        index: usize,
        /// Its time.
        got: u64,
        /// The time before it.
        before: u64,
    },
    /// The answer states another level than its ladder's top.
    #[error("the answer says level {stated}, its ladder reaches level {top}")]
    ForgedLevel {
        /// The level the answer states.
        stated: u8,
        /// The ladder's top level.
        top: u8,
    },
    /// A later ladder of the same value dropped or changed a step seen
    /// before (a fall, or a rewritten history).
    #[error("the ladder dropped or changed step {0} seen before")]
    Rewritten(usize),
    /// A later ladder is of another value.
    #[error("the ladder is of another value")]
    OtherValue,
    /// A contradiction that is not a stronger answer with another value.
    #[error("contradiction {index}: {why}")]
    BadContradiction {
        /// Its index.
        index: usize,
        /// What is wrong.
        why: &'static str,
    },
}

/// A stronger answer of the same fact that states another value (THREATS
/// NS5-5, ADR-0048 amendment 2). It is shown next to the weaker value and
/// never changes that value's level (R-LAD2-3); it carries the other
/// value's [`ladder_value_hash`] only, never the value (R-LAD2-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contradiction {
    /// The stronger answer's level.
    pub level: ProofLevel,
    /// What that answer's lane check verifies.
    pub evidence: Hash32,
    /// When it was recorded on chain.
    pub at: u64,
    /// The other value's [`ladder_value_hash`].
    pub value: Hash32,
}

/// The rises of one value, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ladder {
    /// [`value_key`] of the value.
    pub value: Hash32,
    /// The rises, each strictly stronger and no older than the one before.
    pub steps: Vec<Step>,
}

impl Ladder {
    /// A ladder that starts at `first`.
    pub fn new(value: Hash32, first: Step) -> Result<Ladder, LadderError> {
        let l = Ladder {
            value,
            steps: vec![first],
        };
        l.check()?;
        Ok(l)
    }

    /// The ladder of `value` from every public proof of it, in any order:
    /// by time (ties: stronger first, then by evidence), each proof that is
    /// stronger than every earlier one is a rise; the others change nothing.
    /// `None` when `proofs` is empty.
    pub fn of(value: Hash32, proofs: &[Step]) -> Option<Ladder> {
        let mut p = proofs.to_vec();
        p.sort_by_key(|s| (s.at, s.level.number(), s.evidence));
        let mut steps: Vec<Step> = Vec::new();
        for s in p {
            if s.evidence == Hash32::ZERO {
                continue;
            }
            if steps
                .last()
                .is_none_or(|l| s.level.number() < l.level.number())
            {
                steps.push(s);
            }
        }
        (!steps.is_empty()).then_some(Ladder { value, steps })
    }

    /// Adds a newer, stronger proof. Refused, and the ladder unchanged, when
    /// it is not stronger than the current level or older than the last rise.
    pub fn raise(&mut self, step: Step) -> Result<(), LadderError> {
        let mut next = self.clone();
        next.steps.push(step);
        next.check()?;
        *self = next;
        Ok(())
    }

    /// The current level: the last rise's.
    pub fn level(&self) -> Option<ProofLevel> {
        self.steps.last().map(|s| s.level)
    }

    /// The shape every served ladder must have: one to [`MAX_STEPS`] steps,
    /// each with evidence, each strictly stronger and no older than the one
    /// before.
    pub fn check(&self) -> Result<(), LadderError> {
        if self.steps.is_empty() {
            return Err(LadderError::Empty);
        }
        if self.steps.len() > MAX_STEPS {
            return Err(LadderError::TooLong(self.steps.len()));
        }
        for (i, s) in self.steps.iter().enumerate() {
            if s.evidence == Hash32::ZERO {
                return Err(LadderError::NoEvidence(i));
            }
            if i == 0 {
                continue;
            }
            let b = &self.steps[i - 1];
            if s.level.number() >= b.level.number() {
                return Err(LadderError::NotStronger {
                    index: i,
                    got: s.level.number(),
                    before: b.level.number(),
                });
            }
            if s.at < b.at {
                return Err(LadderError::TimeWentBack {
                    index: i,
                    got: s.at,
                    before: b.at,
                });
            }
        }
        Ok(())
    }

    /// [`Self::check`], and the answer's stated level is this ladder's top.
    pub fn check_stated(&self, stated: ProofLevel) -> Result<(), LadderError> {
        self.check()?;
        let top = self.level().ok_or(LadderError::Empty)?;
        if top != stated {
            return Err(LadderError::ForgedLevel {
                stated: stated.number(),
                top: top.number(),
            });
        }
        Ok(())
    }

    /// A ladder served after `prev` for the same value keeps every step of
    /// `prev`, so the level never falls and the history is never rewritten.
    /// One exception (THREATS NS5-10): a step of `prev` may be missing when a
    /// stronger step no later than it is there, because a proof found late
    /// (an indexer catching up, another lane) that settled first makes the
    /// weaker step no rise at all. That stronger step is a step like any
    /// other: a client trusts it only through its lane's check.
    pub fn check_extends(&self, prev: &Ladder) -> Result<(), LadderError> {
        self.check()?;
        if self.value != prev.value {
            return Err(LadderError::OtherValue);
        }
        for (i, s) in prev.steps.iter().enumerate() {
            if self.steps.contains(s) {
                continue;
            }
            let superseded = self
                .steps
                .iter()
                .any(|t| t.level.number() < s.level.number() && t.at <= s.at);
            // A step the client saw was changed in place, not superseded.
            let changed = self
                .steps
                .iter()
                .any(|t| t.level == s.level && t != s && t.at <= s.at);
            if !superseded || changed {
                return Err(LadderError::Rewritten(i));
            }
        }
        // Nothing older than the top the client saw may fall below it.
        if let (Some(now), Some(then)) = (self.level(), prev.level()) {
            if now.number() > then.number() {
                return Err(LadderError::Rewritten(prev.steps.len() - 1));
            }
        }
        Ok(())
    }

    /// Checks `cs` as the contradictions served next to this value: each is
    /// strictly stronger than the value's level, names evidence and a value
    /// hash other than `own_value` (this value's [`ladder_value_hash`]).
    /// The level is never changed by them (R-LAD2-3).
    pub fn check_contradictions(
        &self,
        cs: &[Contradiction],
        own_value: &Hash32,
    ) -> Result<(), LadderError> {
        let top = self.level().ok_or(LadderError::Empty)?;
        if cs.len() > MAX_STEPS {
            return Err(LadderError::TooLong(cs.len()));
        }
        for (index, c) in cs.iter().enumerate() {
            let why = if c.evidence == Hash32::ZERO {
                "names no evidence"
            } else if c.level.number() >= top.number() {
                "is not stronger than the value's level"
            } else if c.value == *own_value {
                "states the same value"
            } else {
                continue;
            };
            return Err(LadderError::BadContradiction { index, why });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProofLevel::*;

    fn h(n: u8) -> Hash32 {
        Hash32([n; 32])
    }

    fn step(level: ProofLevel, n: u8, at: u64) -> Step {
        Step {
            level,
            evidence: h(n),
            at,
        }
    }

    fn v() -> Hash32 {
        value_key(&h(9), &keccak256(b"5"))
    }

    #[test]
    fn a_stronger_newer_proof_raises_the_level() {
        let mut l = Ladder::new(v(), step(SourceProven, 1, 100)).expect("ok");
        assert_eq!(l.level(), Some(SourceProven));
        l.raise(step(ReObserved, 2, 200)).expect("ok");
        assert_eq!(l.level(), Some(ReObserved));
        l.raise(step(Recomputed, 3, 200)).expect("ok");
        assert_eq!(l.level(), Some(Recomputed));
        assert_eq!(l.steps.len(), 3);
        l.check_stated(Recomputed).expect("ok");
    }

    #[test]
    fn a_fall_or_a_repeat_is_refused_and_changes_nothing() {
        let mut l = Ladder::new(v(), step(ReObserved, 1, 100)).expect("ok");
        let before = l.clone();
        assert_eq!(
            l.raise(step(SourceProven, 2, 200)),
            Err(LadderError::NotStronger {
                index: 1,
                got: 3,
                before: 2
            })
        );
        assert!(matches!(
            l.raise(step(ReObserved, 3, 200)),
            Err(LadderError::NotStronger { .. })
        ));
        assert!(matches!(
            l.raise(step(Recomputed, 4, 99)),
            Err(LadderError::TimeWentBack { .. })
        ));
        assert!(matches!(
            l.raise(step(Recomputed, 0, 300)),
            Err(LadderError::NoEvidence(1))
        ));
        assert_eq!(l, before);
    }

    #[test]
    fn a_served_ladder_that_falls_is_refused() {
        let fell = Ladder {
            value: v(),
            steps: vec![step(ReObserved, 1, 100), step(BondedClaim, 2, 200)],
        };
        assert!(matches!(fell.check(), Err(LadderError::NotStronger { .. })));
        // A later answer that drops a rise the client saw is a fall too.
        let seen = Ladder {
            value: v(),
            steps: vec![step(SourceProven, 1, 100), step(ReObserved, 2, 200)],
        };
        let dropped = Ladder {
            value: v(),
            steps: vec![step(SourceProven, 1, 100)],
        };
        assert_eq!(dropped.check_extends(&seen), Err(LadderError::Rewritten(1)));
        let changed = Ladder {
            value: v(),
            steps: vec![step(SourceProven, 1, 100), step(ReObserved, 7, 200)],
        };
        assert_eq!(changed.check_extends(&seen), Err(LadderError::Rewritten(1)));
        let other = Ladder {
            value: value_key(&h(9), &keccak256(b"6")),
            steps: seen.steps.clone(),
        };
        assert_eq!(other.check_extends(&seen), Err(LadderError::OtherValue));
        let risen = Ladder {
            value: v(),
            steps: vec![
                step(SourceProven, 1, 100),
                step(ReObserved, 2, 200),
                step(Recomputed, 3, 300),
            ],
        };
        risen.check_extends(&seen).expect("ok");
        seen.check_extends(&seen).expect("ok");
    }

    #[test]
    fn a_forged_level_is_refused() {
        let l = Ladder::new(v(), step(ReObserved, 1, 100)).expect("ok");
        assert_eq!(
            l.check_stated(Recomputed),
            Err(LadderError::ForgedLevel { stated: 1, top: 2 })
        );
        assert_eq!(
            l.check_stated(SourceProven),
            Err(LadderError::ForgedLevel { stated: 3, top: 2 })
        );
        l.check_stated(ReObserved).expect("ok");
        let empty = Ladder {
            value: v(),
            steps: vec![],
        };
        assert_eq!(empty.check_stated(ReObserved), Err(LadderError::Empty));
    }

    #[test]
    fn the_ladder_of_proofs_keeps_only_rises_in_time_order() {
        let proofs = [
            step(ReObserved, 2, 200),
            step(BondedClaim, 5, 50),
            step(SourceProven, 1, 100),
            // weaker after level 2: no fall, no step
            step(SourceProven, 6, 300),
            // same time as a level 2: the stronger is taken first
            step(Recomputed, 3, 200),
            step(Recomputed, 0, 10),
        ];
        let l = Ladder::of(v(), &proofs).expect("ok");
        assert_eq!(
            l.steps,
            vec![
                step(BondedClaim, 5, 50),
                step(SourceProven, 1, 100),
                step(Recomputed, 3, 200),
            ]
        );
        l.check().expect("ok");
        // Any order of the same proofs gives the same ladder.
        let mut rev = proofs;
        rev.reverse();
        assert_eq!(Ladder::of(v(), &rev), Some(l));
        assert_eq!(Ladder::of(v(), &[]), None);
        assert_eq!(Ladder::of(v(), &[step(Recomputed, 0, 1)]), None);
    }

    #[test]
    fn at_most_one_step_per_level() {
        let l = Ladder {
            value: v(),
            steps: ProofLevel::ALL
                .iter()
                .rev()
                .enumerate()
                .map(|(i, l)| step(*l, i as u8 + 1, i as u64))
                .collect(),
        };
        l.check().expect("ok");
        let mut long = l.clone();
        long.steps.push(step(Recomputed, 9, 99));
        assert_eq!(long.check(), Err(LadderError::TooLong(6)));
    }

    #[test]
    fn the_same_number_in_two_forms_is_one_value() {
        let a = Value::Fixed {
            mantissa: 283,
            scale: 1,
        };
        let b = Value::Fixed {
            mantissa: 28_300,
            scale: 3,
        };
        assert_eq!(canonical_value(&b), a);
        assert_eq!(ladder_value_hash(&a), ladder_value_hash(&b));
        assert_ne!(value_hash(&a), value_hash(&b));
        let five = Value::Int(5);
        for v in [
            Value::Fixed {
                mantissa: 50,
                scale: 1,
            },
            Value::Fixed {
                mantissa: 5,
                scale: 0,
            },
            Value::UInt(5),
        ] {
            assert_eq!(canonical_value(&v), five, "{v:?}");
            assert_eq!(ladder_value_hash(&v), ladder_value_hash(&five));
        }
        assert_eq!(
            canonical_value(&Value::Fixed {
                mantissa: 0,
                scale: 1
            }),
            Value::Int(0)
        );
        // Different numbers stay different; a huge UInt keeps its form.
        assert_ne!(
            ladder_value_hash(&Value::Int(5)),
            ladder_value_hash(&Value::Int(50))
        );
        assert_eq!(
            canonical_value(&Value::UInt(u128::MAX)),
            Value::UInt(u128::MAX)
        );
        assert_eq!(
            canonical_value(&Value::Fixed {
                mantissa: -120,
                scale: 2
            }),
            Value::Fixed {
                mantissa: -12,
                scale: 1
            }
        );
    }

    #[test]
    fn a_level_3_step_joins_a_level_2_ladder_of_the_same_fact() {
        let metric = MetricId("noaa.ghcnd.usw00003017.tmax".into());
        let subject = Subject::Aggregate("noaa.ghcnd.usw00003017:2026-09-27".into());
        let fact = fact_key(&metric, &subject, 1_759_017_600);
        // The level-2 lane states 28.3 at its feed's scale 1; a level-3
        // report signs 28.30 at scale 2. One fact, one value, one ladder.
        let l2 = value_key(
            &fact,
            &ladder_value_hash(&Value::Fixed {
                mantissa: 283,
                scale: 1,
            }),
        );
        let l3 = value_key(
            &fact,
            &ladder_value_hash(&Value::Fixed {
                mantissa: 2830,
                scale: 2,
            }),
        );
        assert_eq!(l2, l3);
        let l =
            Ladder::of(l2, &[step(ReObserved, 2, 300), step(SourceProven, 3, 200)]).expect("ok");
        assert_eq!(
            l.steps,
            vec![step(SourceProven, 3, 200), step(ReObserved, 2, 300)]
        );
        l.check_stated(ReObserved).expect("ok");
    }

    #[test]
    fn steps_order_by_chain_time() {
        // The stronger proof recorded first: the weaker one, later, is no rise.
        let l =
            Ladder::of(v(), &[step(SourceProven, 3, 300), step(ReObserved, 2, 200)]).expect("ok");
        assert_eq!(l.steps, vec![step(ReObserved, 2, 200)]);
        // A served ladder out of chain-time order is refused.
        let back = Ladder {
            value: v(),
            steps: vec![step(SourceProven, 3, 300), step(ReObserved, 2, 200)],
        };
        assert!(matches!(
            back.check(),
            Err(LadderError::TimeWentBack { .. })
        ));
    }

    #[test]
    fn a_late_found_earlier_stronger_proof_is_not_a_fall() {
        let seen =
            Ladder::of(v(), &[step(SourceProven, 1, 100), step(ReObserved, 2, 200)]).expect("ok");
        // A level-1 proof that settled at 150 is found late: rebuilt from
        // every proof, the level-2 step is no rise any more.
        let rebuilt = Ladder::of(
            v(),
            &[
                step(SourceProven, 1, 100),
                step(ReObserved, 2, 200),
                step(Recomputed, 3, 150),
            ],
        )
        .expect("ok");
        assert_eq!(
            rebuilt.steps,
            vec![step(SourceProven, 1, 100), step(Recomputed, 3, 150)]
        );
        rebuilt.check_extends(&seen).expect("not a fall");
        // A stronger step later than the dropped one does not excuse it.
        let late = Ladder {
            value: v(),
            steps: vec![step(SourceProven, 1, 100), step(Recomputed, 3, 250)],
        };
        assert_eq!(late.check_extends(&seen), Err(LadderError::Rewritten(1)));
        // Nor does a weaker one, and a plain drop stays a fall.
        let weaker = Ladder {
            value: v(),
            steps: vec![step(BondedClaim, 4, 50), step(SourceProven, 1, 100)],
        };
        assert_eq!(weaker.check_extends(&seen), Err(LadderError::Rewritten(1)));
    }

    #[test]
    fn a_contradiction_never_changes_the_level() {
        let own = keccak256(b"5");
        let l = Ladder::new(v(), step(SourceProven, 1, 100)).expect("ok");
        let c = Contradiction {
            level: ReObserved,
            evidence: h(7),
            at: 200,
            value: keccak256(b"6"),
        };
        l.check_contradictions(&[c], &own).expect("ok");
        assert_eq!(l.level(), Some(SourceProven));
        l.check_stated(SourceProven).expect("level unchanged");
        // Not stronger, the same value, or no evidence: refused.
        for (bad, why) in [
            (
                Contradiction {
                    level: SourceProven,
                    ..c
                },
                "is not stronger than the value's level",
            ),
            (Contradiction { value: own, ..c }, "states the same value"),
            (
                Contradiction {
                    evidence: Hash32::ZERO,
                    ..c
                },
                "names no evidence",
            ),
        ] {
            assert_eq!(
                l.check_contradictions(&[c, bad], &own),
                Err(LadderError::BadContradiction { index: 1, why })
            );
        }
    }

    #[test]
    fn the_value_key_binds_the_key_and_the_value() {
        let a = value_key(&h(1), &h(2));
        assert_ne!(a, value_key(&h(1), &h(3)));
        assert_ne!(a, value_key(&h(3), &h(2)));
        let mut b = LADDER_VALUE_DOMAIN.to_vec();
        b.extend_from_slice(&[1; 32]);
        b.extend_from_slice(&[2; 32]);
        assert_eq!(a, keccak256(&b));
    }
}
