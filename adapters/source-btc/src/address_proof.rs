//! BIP-322 address-control proofs for the `btc` namespace (ADR-0015).
//!
//! Verification is done by the `bip322` crate (rust-bitcoin, CC0, pinned
//! exactly). Onyx adds a stricter policy on top, so one proof has one accepted
//! encoding and no proof depends on time:
//! 1. Only the `smp` (simple) and `ful` (full) variants, with their prefix.
//!    Unprefixed, legacy (BIP-137) and proof-of-funds (`pof`) proofs are refused.
//! 2. `smp` only for native segwit addresses (P2WPKH, P2WSH, P2TR), as the
//!    BIP says. The crate also accepts `smp` for P2SH-wrapped segwit with an
//!    empty scriptSig, which is not a consensus-valid spend.
//! 3. Canonical encodings only: base64 must re-encode to the same text, and the
//!    witness or transaction must re-serialize to the same bytes.
//! 4. No time locks: the crate's "valid at time T and age S" counts only when
//!    T = 0 and S = 0.
//! 5. Mainnet addresses only, with a valid checksum.
//! 6. A panic inside the verifier is "invalid" (same code, same answer everywhere).
//! 7. Consensus limits the crate does not check (L12-F3, F4, F6): a P2SH input
//!    with a witness needs its scriptSig; a 65-byte taproot signature may not
//!    use hash type 0x00; pushes are at most 520 bytes and a witness script at
//!    most 10,000 bytes.

use crate::BtcAddressCanon;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bip322::Verification;
use bitcoin::absolute::LockTime;
use bitcoin::address::AddressType;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::script::Instruction;
use bitcoin::{Address, Network, ScriptBuf, Sequence, Transaction, Witness};
use onyx_core::{AddressCanon, AddressProof, ProofOutcome, ProofVector};
use std::str::FromStr;

/// Longest validity window: about one year of Bitcoin blocks (144 per day).
pub const BTC_MAX_ATTESTATION_WINDOW: u64 = 52_560;

/// BIP-322 proof rule for the `btc` namespace.
pub struct Bip322Proof;

use crate::proof_vectors::VECTORS;

impl AddressProof for Bip322Proof {
    fn namespace(&self) -> &str {
        "btc"
    }
    fn chain(&self) -> &str {
        crate::CHAIN_ID
    }
    fn proof_id(&self) -> &str {
        "onyx.proof.btc-mainnet.bip322.v1"
    }
    fn max_window(&self) -> u64 {
        BTC_MAX_ATTESTATION_WINDOW
    }
    fn vectors(&self) -> &'static [ProofVector] {
        VECTORS
    }
    fn verify(&self, body: &str, message: &[u8], signature: &str) -> ProofOutcome {
        std::panic::catch_unwind(|| verify_policy(body, message, signature))
            .unwrap_or(ProofOutcome::Invalid)
    }
}

/// Largest stack element consensus allows (MAX_SCRIPT_ELEMENT_SIZE).
const MAX_ELEMENT: usize = 520;
/// Largest witness script for P2WSH (MAX_SCRIPT_SIZE).
const MAX_WITNESS_SCRIPT: usize = 10_000;

/// Checks on the first `to_sign` input that consensus applies and the crate skips.
fn consensus_limits_ok(address: &Address, witness: &Witness, script_sig: &ScriptBuf) -> bool {
    let kind = address.address_type();
    // L12-F4: BIP-341 forbids an explicit 0x00 hash type on a 65-byte signature.
    if kind == Some(AddressType::P2tr)
        && witness.len() == 1
        && witness
            .nth(0)
            .is_some_and(|s| s.len() == 65 && s[64] == 0x00)
    {
        return false;
    }
    // L12-F3: a wrapped segwit spend needs the redeem-script push in scriptSig.
    if kind == Some(AddressType::P2sh) && !witness.is_empty() && script_sig.is_empty() {
        return false;
    }
    // L12-F6: element and script size limits.
    let n = witness.len();
    for (i, item) in witness.iter().enumerate() {
        let limit = if i + 1 == n && kind_has_witness_script(address, witness) {
            MAX_WITNESS_SCRIPT
        } else {
            MAX_ELEMENT
        };
        if item.len() > limit {
            return false;
        }
    }
    for ins in script_sig.instructions() {
        match ins {
            Ok(Instruction::PushBytes(b)) if b.len() <= MAX_ELEMENT => {}
            _ => return false,
        }
    }
    true
}

/// True if the last witness item is a witness script (P2WSH, or P2SH-wrapped
/// P2WSH, which has more than the 2 items of wrapped P2WPKH).
fn kind_has_witness_script(address: &Address, witness: &Witness) -> bool {
    match address.address_type() {
        Some(AddressType::P2wsh) => true,
        Some(AddressType::P2sh) => witness.len() > 2,
        _ => false,
    }
}

enum Variant {
    Simple,
    Full,
}

fn verify_policy(body: &str, message: &[u8], signature: &str) -> ProofOutcome {
    use ProofOutcome::Invalid;
    if !BtcAddressCanon.is_canonical(body) {
        return Invalid;
    }
    let Ok(address) = Address::from_str(body).and_then(|a| a.require_network(Network::Bitcoin))
    else {
        return Invalid;
    };
    let (variant, b64) = if let Some(rest) = signature.strip_prefix("smp") {
        (Variant::Simple, rest)
    } else if let Some(rest) = signature.strip_prefix("ful") {
        (Variant::Full, rest)
    } else {
        return Invalid;
    };
    let Ok(bytes) = STANDARD.decode(b64) else {
        return Invalid;
    };
    if STANDARD.encode(&bytes) != b64 {
        return Invalid;
    }
    let result = match variant {
        Variant::Simple => {
            if !matches!(
                address.address_type(),
                Some(AddressType::P2wpkh | AddressType::P2wsh | AddressType::P2tr)
            ) {
                return Invalid;
            }
            let Ok(witness) = deserialize::<Witness>(&bytes) else {
                return Invalid;
            };
            if serialize(&witness) != bytes {
                return Invalid;
            }
            if !consensus_limits_ok(&address, &witness, &ScriptBuf::new()) {
                return Invalid;
            }
            bip322::verify_simple(&address, message, witness)
        }
        Variant::Full => {
            let Ok(tx) = deserialize::<Transaction>(&bytes) else {
                return Invalid;
            };
            if serialize(&tx) != bytes {
                return Invalid;
            }
            let Some(input) = tx.input.first() else {
                return Invalid;
            };
            if !consensus_limits_ok(&address, &input.witness, &input.script_sig) {
                return Invalid;
            }
            bip322::verify_full(&address, message, tx)
        }
    };
    match result {
        Ok(Verification::Valid { time, age })
            if time == LockTime::ZERO && age == Sequence::ZERO =>
        {
            ProofOutcome::Valid
        }
        Ok(Verification::Inconclusive) => ProofOutcome::Inconclusive,
        Ok(Verification::Valid { .. }) | Err(_) => Invalid,
    }
}
