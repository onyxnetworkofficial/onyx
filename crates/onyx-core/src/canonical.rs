//! Canonical serialization (ADR-0002).
//!
//! Borsh gives one byte string per value for the types we allow. Decoding is
//! strict: trailing bytes are an error, so two different byte strings can never
//! decode to the same value.

use borsh::{BorshDeserialize, BorshSerialize};

/// Error from canonical encode or decode.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    /// The bytes are not a canonical encoding of the target type.
    #[error("non-canonical or malformed encoding: {0}")]
    Decode(String),
    /// Encoding failed (only possible on I/O failure, which `Vec` never has).
    #[error("encode failed: {0}")]
    Encode(String),
}

/// Encode a value to its canonical bytes.
pub fn to_canonical_bytes<T: BorshSerialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    borsh::to_vec(value).map_err(|e| CanonicalError::Encode(e.to_string()))
}

/// Decode canonical bytes. Rejects trailing data.
pub fn from_canonical_bytes<T: BorshDeserialize>(bytes: &[u8]) -> Result<T, CanonicalError> {
    borsh::from_slice(bytes).map_err(|e| CanonicalError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_bytes_rejected() {
        let mut b = to_canonical_bytes(&7u64).expect("encode");
        b.push(0);
        assert!(from_canonical_bytes::<u64>(&b).is_err());
    }

    #[test]
    fn bool_other_than_0_or_1_rejected() {
        assert!(from_canonical_bytes::<bool>(&[2]).is_err());
    }
}
