//! Keccak-256 content addressing (ADR-0003).

use borsh::{BorshDeserialize, BorshSerialize};
use sha3::{Digest, Keccak256};
use std::fmt;

/// A 32-byte Keccak-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize)]
pub struct Hash32(pub [u8; 32]);

impl Hash32 {
    /// The all-zero hash, used only as an explicit "empty" marker.
    pub const ZERO: Hash32 = Hash32([0u8; 32]);

    /// Lowercase hex without a `0x` prefix.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", self.to_hex())
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", self.to_hex())
    }
}

/// Keccak-256 (the EVM `keccak256`, not NIST SHA3-256).
pub fn keccak256(bytes: &[u8]) -> Hash32 {
    Hash32(Keccak256::digest(bytes).into())
}

/// A recipe is identified by the Keccak-256 of its exact WASM bytes.
pub type RecipeId = Hash32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keccak_matches_evm_empty_vector() {
        // keccak256("") as returned by the EVM.
        assert_eq!(
            keccak256(b"").to_hex(),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    /// Hashes print as `0x` and 64 hex digits in logs and errors (mutation L4).
    #[test]
    fn hashes_print_as_0x_hex() {
        let h = Hash32([0xab; 32]);
        let want = format!("0x{}", "ab".repeat(32));
        assert_eq!(format!("{h}"), want);
        assert_eq!(format!("{h:?}"), want);
    }
}
