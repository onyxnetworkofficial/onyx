//! The estimate layer (ADR-0050, docs/spec/estimates.md, NORTH-STAR NS14).
//!
//! An estimate is a value Onyx did not check: a model's output, an
//! interpolation, a heuristic's total. It may be useful and may be served,
//! but it lives apart from every checked value:
//!
//! - its metric id starts with [`ESTIMATE_PREFIX`], and no other metric may
//!   (R-EST-1);
//! - its level is `E`, which is not a rung of the proof ladder: there is no
//!   [`crate::ProofLevel`] variant for it, so no code that matches on levels
//!   can ever meet an estimate (R-EST-4);
//! - it always carries its method (an id and the hash of the method's text or
//!   program) and its error band, `Unknown` included (R-EST-3);
//! - it is never an input of a checked value: each entry point (a shard's
//!   catalog, a recipe's source, a label set, a track-record resolution)
//!   refuses one (R-EST-2).

use crate::claim::{MetricId, Subject};
use crate::hash::{keccak256, Hash32};
use crate::ids::is_dotted_id;
use crate::source::SourceId;
use crate::value::{Value, ValueError};
use borsh::{BorshDeserialize, BorshSerialize};

/// Every estimate metric (and estimate source) id starts with this, and no
/// checked one may (R-EST-1).
pub const ESTIMATE_PREFIX: &str = "estimate.";
/// Domain of [`Estimate::hash`].
pub const ESTIMATE_DOMAIN: &[u8] = b"onyx-estimate-v1";
/// Most input hashes one estimate may name.
pub const MAX_ESTIMATE_INPUTS: usize = 64;
/// Coverage is in parts per million: 1..=999,999 (an interval that covers
/// "always" is not an error band).
pub const MAX_COVERAGE_PPM: u32 = 999_999;

/// True if `id` is in the estimate namespace (R-EST-1). Shared by every
/// entry point that refuses an estimate, so the rule has one spelling.
pub fn is_estimate_id(id: &str) -> bool {
    id.starts_with(ESTIMATE_PREFIX)
}

impl MetricId {
    /// True for an estimate metric (R-EST-1).
    pub fn is_estimate(&self) -> bool {
        is_estimate_id(&self.0)
    }
}

impl SourceId {
    /// True for an estimate used as a source: a recipe input, a resolver.
    pub fn is_estimate(&self) -> bool {
        is_estimate_id(&self.0)
    }
}

/// How sure the estimate is.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ErrorBand {
    /// `lo <= value <= hi` with the stated coverage (for example 950,000 ppm
    /// for a 95% interval). Same value kind as the value.
    Interval {
        /// Low end.
        lo: Value,
        /// High end.
        hi: Value,
        /// Stated coverage, parts per million, 1..=999,999.
        coverage_ppm: u32,
    },
    /// The method states no error. Shown as "error unknown", never left out
    /// (R-EST-3).
    Unknown,
}

/// One estimate: a value about a subject, with the method that made it and
/// its error. Never a [`crate::Claim`], never a ladder step.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Estimate {
    /// Metric; must start with [`ESTIMATE_PREFIX`].
    pub metric: MetricId,
    /// What the value is about.
    pub subject: Subject,
    /// Time the estimate is "as of" (unix seconds).
    pub as_of: u64,
    /// The estimated value.
    pub value: Value,
    /// Method id (dotted id), for example `onyx.mean-ci95.v1`.
    pub method: String,
    /// Keccak-256 of the method's text or program, so a reader can see what
    /// was done. Non-zero.
    pub method_hash: Hash32,
    /// The error band.
    pub error: ErrorBand,
    /// Hashes of what the estimate was made from (for example the checked
    /// values it averages), strictly ascending.
    pub inputs: Vec<Hash32>,
}

/// Why an estimate is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EstimateError {
    /// The metric is not in the estimate namespace (R-EST-1).
    #[error("estimate metric must start with {ESTIMATE_PREFIX}: {0}")]
    NotEstimateMetric(String),
    /// The metric id is not a dotted id.
    #[error("bad metric id: {0}")]
    BadMetric(String),
    /// The method id is not a dotted id, or its hash is zero.
    #[error("bad method")]
    BadMethod,
    /// The value (or a band end) is malformed.
    #[error("bad value: {0}")]
    BadValue(ValueError),
    /// The value is not a number: an error band needs an order.
    #[error("an estimate's value must be a number")]
    NotNumeric,
    /// A band end is of another kind than the value.
    #[error("error band ends must be of the value's kind")]
    BandKind,
    /// The band does not hold the value (`lo <= value <= hi`).
    #[error("error band does not hold the value")]
    BandDoesNotHold,
    /// Coverage outside 1..=999,999 ppm.
    #[error("coverage out of range: {0} ppm")]
    Coverage(u32),
    /// Inputs not strictly ascending, zero, or too many.
    #[error("inputs must be non-zero, strictly ascending, at most {MAX_ESTIMATE_INPUTS}")]
    Inputs,
}

/// `a <= b` for two numbers of one kind; `None` for other pairs.
fn le(a: &Value, b: &Value) -> Option<bool> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x <= y),
        (Value::UInt(x), Value::UInt(y)) => Some(x <= y),
        (
            Value::Fixed {
                mantissa: x,
                scale: s,
            },
            Value::Fixed {
                mantissa: y,
                scale: t,
            },
        ) if s == t => Some(x <= y),
        _ => None,
    }
}

/// Same variant and, for `Fixed`, same scale.
fn same_kind(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Fixed { scale: s, .. }, Value::Fixed { scale: t, .. }) => s == t,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

impl Estimate {
    /// Check the rules a served estimate keeps (R-EST-1, R-EST-3): estimate
    /// namespace, a pinned method, a numeric value of one kind with its band,
    /// coverage in range, canonical inputs.
    pub fn validate(&self) -> Result<(), EstimateError> {
        if !self.metric.is_estimate() {
            return Err(EstimateError::NotEstimateMetric(self.metric.0.clone()));
        }
        if !is_dotted_id(&self.metric.0) {
            return Err(EstimateError::BadMetric(self.metric.0.clone()));
        }
        if !is_dotted_id(&self.method) || self.method_hash == Hash32::ZERO {
            return Err(EstimateError::BadMethod);
        }
        self.value.validate().map_err(EstimateError::BadValue)?;
        if !self.value.is_numeric() {
            return Err(EstimateError::NotNumeric);
        }
        if let ErrorBand::Interval {
            lo,
            hi,
            coverage_ppm,
        } = &self.error
        {
            lo.validate().map_err(EstimateError::BadValue)?;
            hi.validate().map_err(EstimateError::BadValue)?;
            if !same_kind(lo, &self.value) || !same_kind(hi, &self.value) {
                return Err(EstimateError::BandKind);
            }
            if le(lo, &self.value) != Some(true) || le(&self.value, hi) != Some(true) {
                return Err(EstimateError::BandDoesNotHold);
            }
            if *coverage_ppm == 0 || *coverage_ppm > MAX_COVERAGE_PPM {
                return Err(EstimateError::Coverage(*coverage_ppm));
            }
        }
        if self.inputs.len() > MAX_ESTIMATE_INPUTS
            || self.inputs.contains(&Hash32::ZERO)
            || !self.inputs.windows(2).all(|w| w[0] < w[1])
        {
            return Err(EstimateError::Inputs);
        }
        Ok(())
    }

    /// `keccak256("onyx-estimate-v1" ‖ borsh(estimate))`.
    pub fn hash(&self) -> Hash32 {
        let mut b = ESTIMATE_DOMAIN.to_vec();
        b.extend(borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }
}

/// A number as text: `Int`/`UInt` in full, `Fixed` with its decimals
/// (`Fixed { mantissa: -125, scale: 2 }` is `-1.25`). No float on the way.
pub fn value_text(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::Fixed { mantissa, scale } => {
            let digits = mantissa.unsigned_abs().to_string();
            let scale = usize::from(*scale);
            let padded = format!("{digits:0>width$}", width = scale + 1);
            let (int, frac) = padded.split_at(padded.len() - scale);
            let sign = if *mantissa < 0 { "-" } else { "" };
            format!("{sign}{int}.{frac}")
        }
        Value::Bool(b) => b.to_string(),
        Value::Bytes(b) => {
            let mut s = String::from("0x");
            for x in b {
                s.push_str(&format!("{x:02x}"));
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(m: i128) -> Value {
        Value::Fixed {
            mantissa: m,
            scale: 2,
        }
    }

    fn good() -> Estimate {
        Estimate {
            metric: MetricId("estimate.example.daily_mean".into()),
            subject: Subject::Aggregate("example.network".into()),
            as_of: 1_790_000_000,
            value: fixed(1_000),
            method: "onyx.mean-ci95.v1".into(),
            method_hash: keccak256(b"method text"),
            error: ErrorBand::Interval {
                lo: fixed(900),
                hi: fixed(1_100),
                coverage_ppm: 950_000,
            },
            inputs: {
                let mut v = vec![keccak256(b"a"), keccak256(b"b")];
                v.sort();
                v
            },
        }
    }

    #[test]
    fn a_well_formed_estimate_passes() {
        assert_eq!(good().validate(), Ok(()));
        let mut e = good();
        e.error = ErrorBand::Unknown;
        assert_eq!(e.validate(), Ok(()));
    }

    /// R-EST-1: the namespace is the rule; a checked-looking id is refused.
    #[test]
    fn an_estimate_outside_the_namespace_is_refused() {
        let mut e = good();
        e.metric = MetricId("usgs.earthquakes.m5_count".into());
        assert!(matches!(
            e.validate(),
            Err(EstimateError::NotEstimateMetric(_))
        ));
        // A prefix lookalike is not the namespace.
        e.metric = MetricId("estimates.x".into());
        assert!(matches!(
            e.validate(),
            Err(EstimateError::NotEstimateMetric(_))
        ));
        assert!(is_estimate_id("estimate.x"));
        assert!(!is_estimate_id("estimatex"));
        assert!(SourceId("estimate.model".into()).is_estimate());
        assert!(!SourceId("usgs.fdsn".into()).is_estimate());
    }

    #[test]
    fn a_band_that_does_not_hold_the_value_is_refused() {
        let mut e = good();
        e.error = ErrorBand::Interval {
            lo: fixed(1_001),
            hi: fixed(1_100),
            coverage_ppm: 950_000,
        };
        assert_eq!(e.validate(), Err(EstimateError::BandDoesNotHold));
        e.error = ErrorBand::Interval {
            lo: fixed(900),
            hi: fixed(999),
            coverage_ppm: 950_000,
        };
        assert_eq!(e.validate(), Err(EstimateError::BandDoesNotHold));
        // Ends of another scale are another kind, not a looser band.
        e.error = ErrorBand::Interval {
            lo: Value::Fixed {
                mantissa: 9,
                scale: 1,
            },
            hi: fixed(1_100),
            coverage_ppm: 950_000,
        };
        assert_eq!(e.validate(), Err(EstimateError::BandKind));
        // The ends themselves are allowed (lo == value == hi).
        e.error = ErrorBand::Interval {
            lo: fixed(1_000),
            hi: fixed(1_000),
            coverage_ppm: 500_000,
        };
        assert_eq!(e.validate(), Ok(()));
    }

    #[test]
    fn coverage_method_and_inputs_are_checked() {
        for c in [0, MAX_COVERAGE_PPM + 1] {
            let mut e = good();
            e.error = ErrorBand::Interval {
                lo: fixed(900),
                hi: fixed(1_100),
                coverage_ppm: c,
            };
            assert_eq!(e.validate(), Err(EstimateError::Coverage(c)));
        }
        let mut e = good();
        e.method_hash = Hash32::ZERO;
        assert_eq!(e.validate(), Err(EstimateError::BadMethod));
        let mut e = good();
        e.inputs.reverse();
        assert_eq!(e.validate(), Err(EstimateError::Inputs));
        let mut e = good();
        e.value = Value::Bool(true);
        e.error = ErrorBand::Unknown;
        assert_eq!(e.validate(), Err(EstimateError::NotNumeric));
    }

    #[test]
    fn values_render_without_floats() {
        assert_eq!(value_text(&fixed(-125)), "-1.25");
        assert_eq!(value_text(&fixed(5)), "0.05");
        assert_eq!(value_text(&Value::UInt(42)), "42");
        assert_eq!(
            value_text(&Value::Fixed {
                mantissa: 123_456,
                scale: 8
            }),
            "0.00123456"
        );
    }
}
