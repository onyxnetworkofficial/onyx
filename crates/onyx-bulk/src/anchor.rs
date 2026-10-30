//! The publisher's signature over a manifest root, in the level-3 scheme of
//! ADR-0028 (no new scheme): the root is the value of a `SignedPayload`
//!
//! `{ source: publisher, key: key id, metric: "bulk.manifest",
//!    subject: Aggregate(dataset), value: Bytes(root), observed_unix: created_unix, sequence }`
//!
//! signed with EIP-191 over `onyx-signed-source-v1:0x<digest>`. The signed
//! report goes on chain with `OnyxSignedBook.postReport` (one transaction per
//! version); the book checks the key is the registered one of that source
//! (the reviewer registered it from the publisher's keys document) and keeps
//! every `(key, sequence)` for good, so a report is never accepted twice
//! (THREATS CL5-3).
//!
//! The sequence is not the signer's choice: it is [`sequence_for`] the
//! dataset and version (review CL5-R1). Two different manifests for one
//! version signed by one key therefore share `(key, sequence)`, and the book
//! records the second as an equivocation and suspends the key, on chain where
//! every node and client sees it (THREATS CL5-2).

use crate::manifest::ManifestHeader;
use onyx_core::{
    keccak256, Hash32, MetricId, SignedPayload, SignedReport, SourceId, Subject, Value,
};
use onyx_labels::attest::{eip191_recover, eip191_sign_with, secret_address};

/// The metric every manifest anchor uses.
pub const ANCHOR_METRIC: &str = "bulk.manifest";

/// An anchor that does not match its manifest or signer.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnchorError {
    /// The payload names another source, metric, subject, value or time.
    #[error("the signed payload does not name this manifest: {0}")]
    Mismatch(&'static str),
    /// The signature does not recover (or is not in canonical form).
    #[error("bad signature")]
    BadSignature,
    /// The recovered signer's key id is not the payload's key.
    #[error("the signer is not the payload's key")]
    WrongKey,
    /// Not a valid secret key.
    #[error("bad secret key")]
    BadSecret,
    /// The reserved sequence (`u64::MAX`, ADR-0028).
    #[error("sequence u64::MAX is reserved")]
    ReservedSequence,
}

/// `SourceKey::id`: `keccak256("onyx-source-key-v1" ‖ borsh(source, signer))`
/// (the provenance is not part of it).
pub fn key_id(source: &SourceId, signer: &[u8; 20]) -> Hash32 {
    let mut b = onyx_core::signed::KEY_DOMAIN.to_vec();
    b.extend_from_slice(&borsh::to_vec(&(source, signer)).unwrap_or_default());
    keccak256(&b)
}

/// Domain tag of [`sequence_for`].
pub const SEQUENCE_DOMAIN: &[u8] = b"onyx-bulk-sequence-v1";

/// The one sequence a manifest of `dataset` at `version` is signed under:
/// the first 8 bytes (big-endian) of
/// `keccak256("onyx-bulk-sequence-v1" ‖ borsh(dataset, version))`, never the
/// reserved `u64::MAX`.
pub fn sequence_for(dataset: &str, version: u32) -> u64 {
    let mut b = SEQUENCE_DOMAIN.to_vec();
    b.extend_from_slice(&borsh::to_vec(&(dataset, version)).unwrap_or_default());
    let h = keccak256(&b);
    let mut w = [0u8; 8];
    w.copy_from_slice(&h.0[..8]);
    u64::from_be_bytes(w).min(u64::MAX - 1)
}

/// The payload that anchors `header` for `signer`.
pub fn payload(header: &ManifestHeader, signer: &[u8; 20]) -> SignedPayload {
    SignedPayload {
        source: header.publisher.clone(),
        key: key_id(&header.publisher, signer),
        metric: MetricId(ANCHOR_METRIC.into()),
        subject: Subject::Aggregate(header.dataset.clone()),
        value: Value::Bytes(header.root().0.to_vec()),
        observed_unix: header.created_unix,
        sequence: sequence_for(&header.dataset, header.version),
    }
}

/// The publisher's 20-byte address of a secret key.
pub fn address_of(secret: &[u8; 32]) -> Result<[u8; 20], AnchorError> {
    secret_address(secret).ok_or(AnchorError::BadSecret)
}

/// Sign `header` with `secret`.
pub fn sign(secret: &[u8; 32], header: &ManifestHeader) -> Result<SignedReport, AnchorError> {
    let signer = address_of(secret)?;
    let p = payload(header, &signer);
    let signature =
        eip191_sign_with(secret, p.message().as_bytes()).ok_or(AnchorError::BadSecret)?;
    Ok(SignedReport {
        payload: p,
        signature,
    })
}

/// Check that `report` signs exactly `header`'s root for its publisher and
/// dataset, and that the signer's key id is the payload's key. Returns the
/// signer. (Whether the chain book holds the key and accepted the report is
/// the caller's chain read.)
pub fn verify(header: &ManifestHeader, report: &SignedReport) -> Result<[u8; 20], AnchorError> {
    let p = &report.payload;
    if p.source != header.publisher {
        return Err(AnchorError::Mismatch("source"));
    }
    if p.metric.0 != ANCHOR_METRIC {
        return Err(AnchorError::Mismatch("metric"));
    }
    if p.subject != Subject::Aggregate(header.dataset.clone()) {
        return Err(AnchorError::Mismatch("subject"));
    }
    if p.value != Value::Bytes(header.root().0.to_vec()) {
        return Err(AnchorError::Mismatch("value (manifest root)"));
    }
    if p.observed_unix != header.created_unix {
        return Err(AnchorError::Mismatch("observed time"));
    }
    if p.sequence == u64::MAX {
        return Err(AnchorError::ReservedSequence);
    }
    if p.sequence != sequence_for(&header.dataset, header.version) {
        return Err(AnchorError::Mismatch(
            "sequence (fixed by dataset and version)",
        ));
    }
    let signer = eip191_recover(p.message().as_bytes(), &report.signature)
        .ok_or(AnchorError::BadSignature)?;
    if key_id(&p.source, &signer) != p.key {
        return Err(AnchorError::WrongKey);
    }
    Ok(signer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests::{entry, new_version};
    use crate::manifest::Manifest;
    use onyx_core::{KeyProvenance, SourceKey};

    const SECRET: [u8; 32] = [0x11; 32];

    #[test]
    fn key_id_is_the_level3_key_id() {
        let signer = address_of(&SECRET).expect("ok");
        let k = SourceKey {
            source: SourceId("example.publisher".into()),
            signer,
            provenance: KeyProvenance::WellKnown {
                origin: "data.example.org".into(),
                document_hash: keccak256(b"doc"),
                fetched_unix: 1,
            },
        };
        assert_eq!(key_id(&k.source, &signer), k.id());
    }

    #[test]
    fn sign_verify_and_refuse() {
        let m =
            Manifest::build(new_version(1_700_000_000), None, vec![entry(0, 1, 5)]).expect("ok");
        let r = sign(&SECRET, &m.header).expect("ok");
        assert_eq!(verify(&m.header, &r), Ok(address_of(&SECRET).expect("ok")));
        assert!(r.payload.validate().is_ok());
        // A swapped manifest (another root) is refused.
        let other =
            Manifest::build(new_version(1_700_000_001), None, vec![entry(0, 1, 5)]).expect("ok");
        assert_eq!(
            verify(&other.header, &r),
            Err(AnchorError::Mismatch("value (manifest root)"))
        );
        // A tampered signature, a payload re-pointed at another key.
        let mut bad = r.clone();
        bad.signature[10] ^= 1;
        assert!(verify(&m.header, &bad).is_err());
        let mut rekey = r.clone();
        rekey.payload.key = keccak256(b"another key");
        assert!(verify(&m.header, &rekey).is_err());
        // A sequence of the signer's choice is refused, even correctly signed
        // (it would let one key sign two manifests for a version unseen).
        let mut p = r.payload.clone();
        p.sequence += 1;
        let sig = eip191_sign_with(&SECRET, p.message().as_bytes()).expect("ok");
        let chosen = SignedReport {
            payload: p,
            signature: sig,
        };
        assert_eq!(
            verify(&m.header, &chosen),
            Err(AnchorError::Mismatch(
                "sequence (fixed by dataset and version)"
            ))
        );
    }

    /// Review CL5-R1: two manifests for one version share the sequence (so the
    /// book sees an equivocation); versions and datasets differ.
    #[test]
    fn sequences_are_fixed_by_dataset_and_version() {
        let a = Manifest::build(new_version(100), None, vec![entry(0, 1, 5)]).expect("ok");
        let b = Manifest::build(new_version(200), None, vec![entry(0, 1, 9)]).expect("ok");
        let (ra, rb) = (
            sign(&SECRET, &a.header).expect("ok"),
            sign(&SECRET, &b.header).expect("ok"),
        );
        assert_ne!(ra.payload.digest(), rb.payload.digest());
        assert_eq!(ra.payload.sequence, rb.payload.sequence);
        assert_eq!(ra.payload.key, rb.payload.key);
        assert_ne!(sequence_for("x.y", 1), sequence_for("x.y", 2));
        assert_ne!(sequence_for("x.y", 1), sequence_for("x.z", 1));
        assert_ne!(sequence_for("x.y", 1), u64::MAX);
    }
}
