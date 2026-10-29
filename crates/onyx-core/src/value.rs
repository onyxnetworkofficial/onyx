//! Claim values. Integers and fixed-point only; no floats anywhere (ADR-0009).

use borsh::{BorshDeserialize, BorshSerialize};

/// Largest allowed fixed-point scale. 10^38 still fits in an i128 mantissa range.
pub const MAX_FIXED_SCALE: u8 = 38;
/// Largest `Bytes` value. Claims are small facts, not blobs; blobs go to the CAS as evidence.
pub const MAX_VALUE_BYTES: usize = 1024;

/// A claim value.
///
/// Amounts are always in the smallest unit of the asset (sats, wei, cents).
/// Ratios and prices use `Fixed`: `mantissa * 10^-scale`, where the metric's
/// registered [`ValueKind`] fixes the exact scale. With the scale fixed per
/// metric, every number has exactly one encoding, including whole numbers
/// (`1.00` at scale 2 is `Fixed { mantissa: 100, scale: 2 }`) (R-VAL-3).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Value {
    /// Signed integer in smallest units.
    Int(i128),
    /// Unsigned integer in smallest units.
    UInt(u128),
    /// Decimal fixed point: `mantissa * 10^-scale`, scale 1..=38 (scale 0 is `Int`).
    Fixed {
        /// Signed mantissa.
        mantissa: i128,
        /// Number of decimal places; must equal the metric's registered scale.
        scale: u8,
    },
    /// Boolean outcome (for example a settlement result).
    Bool(bool),
    /// Opaque bytes (for example a hash or an encoded row).
    Bytes(Vec<u8>),
}

/// The variant (and, for `Fixed`, the exact scale) a metric's values must use.
/// Fixed per metric in the registry so `Int(42)` and `UInt(42)` can never both be
/// valid for one metric (R-VAL-1), and `Fixed` has one scale per metric (R-VAL-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum ValueKind {
    /// `Value::Int`.
    Int,
    /// `Value::UInt`.
    UInt,
    /// `Value::Fixed` with exactly this many decimals.
    Fixed {
        /// Exact scale.
        scale: u8,
    },
    /// `Value::Bool`.
    Bool,
    /// `Value::Bytes`.
    Bytes,
}

/// Value validation error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValueError {
    /// Fixed scale above the maximum.
    #[error("fixed-point scale {0} exceeds {MAX_FIXED_SCALE}")]
    ScaleTooLarge(u8),
    /// Fixed with scale 0: use `Int`, so one number has one encoding (R-VAL-1).
    #[error("fixed-point scale 0 is not allowed; use Int")]
    ZeroScale,
    /// Bytes value too long.
    #[error("bytes value of {0} bytes exceeds {MAX_VALUE_BYTES}")]
    BytesTooLong(usize),
}

impl Value {
    /// Check the value is well formed. The metric's [`ValueKind`] then pins the
    /// variant and scale (`is_kind`), which makes the encoding unique.
    pub fn validate(&self) -> Result<(), ValueError> {
        match self {
            Value::Fixed { scale: 0, .. } => Err(ValueError::ZeroScale),
            Value::Fixed { scale, .. } if *scale > MAX_FIXED_SCALE => {
                Err(ValueError::ScaleTooLarge(*scale))
            }
            Value::Bytes(b) if b.len() > MAX_VALUE_BYTES => Err(ValueError::BytesTooLong(b.len())),
            _ => Ok(()),
        }
    }

    /// True if this value is of `kind` (same variant and, for `Fixed`, same scale).
    pub fn is_kind(&self, kind: ValueKind) -> bool {
        match (self, kind) {
            (Value::Int(_), ValueKind::Int)
            | (Value::UInt(_), ValueKind::UInt)
            | (Value::Bool(_), ValueKind::Bool)
            | (Value::Bytes(_), ValueKind::Bytes) => true,
            (Value::Fixed { scale, .. }, ValueKind::Fixed { scale: want }) => *scale == want,
            _ => false,
        }
    }

    /// True for numeric variants (the ones a quorum tolerance applies to).
    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Int(_) | Value::UInt(_) | Value::Fixed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICE: ValueKind = ValueKind::Fixed { scale: 2 };

    fn fixed(mantissa: i128, scale: u8) -> Value {
        Value::Fixed { mantissa, scale }
    }

    /// R-VAL-3: whole numbers and zero are expressible under a Fixed kind.
    #[test]
    fn whole_numbers_and_zero_valid_at_the_metric_scale() {
        for v in [fixed(0, 2), fixed(100, 2), fixed(-2500, 2), fixed(15, 2)] {
            assert_eq!(v.validate(), Ok(()), "{v:?}");
            assert!(v.is_kind(PRICE), "{v:?}");
        }
    }

    /// One encoding per number per metric: other scales do not match the kind.
    #[test]
    fn other_scales_rejected_by_kind() {
        assert!(!fixed(1, 0).is_kind(PRICE));
        assert!(!fixed(10, 1).is_kind(PRICE));
        assert!(!fixed(1000, 3).is_kind(PRICE));
        assert!(!Value::Int(1).is_kind(PRICE));
    }

    #[test]
    fn fixed_scale_zero_rejected() {
        assert_eq!(fixed(42, 0).validate(), Err(ValueError::ZeroScale));
    }

    #[test]
    fn fixed_scale_bounded() {
        assert_eq!(fixed(1, 39).validate(), Err(ValueError::ScaleTooLarge(39)));
        // The bound itself is allowed (mutation L4).
        assert_eq!(fixed(1, MAX_FIXED_SCALE).validate(), Ok(()));
    }

    #[test]
    fn bytes_capped() {
        assert!(Value::Bytes(vec![0; MAX_VALUE_BYTES]).validate().is_ok());
        assert_eq!(
            Value::Bytes(vec![0; MAX_VALUE_BYTES + 1]).validate(),
            Err(ValueError::BytesTooLong(MAX_VALUE_BYTES + 1))
        );
    }
}
