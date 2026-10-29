//! Proof level 3, lane (a): data the source signs itself (ADR-0028,
//! docs/spec/proof-ladder.md "Level 3").
//!
//! A source publishes the key it signs with on its own HTTPS origin (or in its
//! DNS); the registry records that key with where it was found ([`SourceKey`],
//! [`KeyProvenance`]). Each value is a [`SignedPayload`] (source, key, metric,
//! subject, value, observation time, sequence) signed with EIP-191
//! ([`SignedPayload::message`]); payload and signature together are a
//! [`SignedReport`]. Verification, replay and equivocation rules and the claim
//! intake live in `onyx_coordinator::signed` (this crate has no signature
//! code).

use crate::claim::{MetricId, Subject};
use crate::hash::{keccak256, Hash32};
use crate::ids::is_dotted_id;
use crate::source::SourceId;
use crate::value::{Value, ValueError};
use borsh::{BorshDeserialize, BorshSerialize};

/// Domain tag of [`SourceKey::id`].
pub const KEY_DOMAIN: &[u8] = b"onyx-source-key-v1";
/// Domain tag of [`SignedPayload::digest`]; also the prefix of the signed text.
pub const PAYLOAD_DOMAIN: &str = "onyx-signed-source-v1";
/// Domain tag of [`SignedReport::hash`].
pub const REPORT_DOMAIN: &[u8] = b"onyx-signed-report-v1";
/// The RFC 8615 well-known path a source publishes its keys at.
pub const WELL_KNOWN_PATH: &str = "/.well-known/onyx-source-keys.json";
/// The DNS label a source publishes its keys under (`_onyx-source-keys.<origin>`).
pub const DNS_LABEL: &str = "_onyx-source-keys";
/// Longest DNS host name.
pub const MAX_ORIGIN_LEN: usize = 253;
/// Evidence kind a signed-source claim carries (the report hash).
pub const REPORT_EVIDENCE: &str = "signed-report";

/// Where a source's key was found when the registry reviewed it. Both forms
/// are under the source's own origin, which is what ties the key to the
/// publisher: only who controls that origin's HTTPS server or DNS zone can
/// publish there. The intake cannot re-fetch the web (it must be
/// deterministic), so this is the review record, not a live check.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum KeyProvenance {
    /// `https://<origin>/.well-known/onyx-source-keys.json` (RFC 8615) listed
    /// the key.
    WellKnown {
        /// Lowercase DNS host name of the source's origin.
        origin: String,
        /// Keccak-256 of the exact document bytes fetched.
        document_hash: Hash32,
        /// When it was fetched (unix seconds).
        fetched_unix: u64,
    },
    /// A TXT record at `_onyx-source-keys.<origin>` listed the key.
    DnsTxt {
        /// Lowercase DNS host name of the source's origin.
        origin: String,
        /// Keccak-256 of the exact TXT record data fetched.
        record_hash: Hash32,
        /// When it was fetched (unix seconds).
        fetched_unix: u64,
    },
}

impl KeyProvenance {
    /// The source origin the key was published under.
    pub fn origin(&self) -> &str {
        match self {
            KeyProvenance::WellKnown { origin, .. } | KeyProvenance::DnsTxt { origin, .. } => {
                origin
            }
        }
    }

    /// Where exactly the key was published: the well-known URL or the TXT name.
    pub fn location(&self) -> String {
        match self {
            KeyProvenance::WellKnown { origin, .. } => format!("https://{origin}{WELL_KNOWN_PATH}"),
            KeyProvenance::DnsTxt { origin, .. } => format!("{DNS_LABEL}.{origin}"),
        }
    }
}

/// A signing key of a registered source.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SourceKey {
    /// The registered source this key signs for.
    pub source: SourceId,
    /// EIP-191 signer address (secp256k1, last 20 bytes of the key's Keccak).
    pub signer: [u8; 20],
    /// Where the key was published by the source.
    pub provenance: KeyProvenance,
}

/// Malformed key, payload or report.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignedFormError {
    /// Malformed id or name.
    #[error("malformed: {0}")]
    Bad(&'static str),
    /// Bad value.
    #[error(transparent)]
    Value(#[from] ValueError),
}

/// True for a lowercase DNS host name with at least two labels: letters,
/// digits and inner hyphens, labels of 1..=63, at most 253 bytes, not all
/// digits in the last label (so never an IP address).
pub fn is_origin_host(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_ORIGIN_LEN {
        return false;
    }
    let labels: Vec<&str> = s.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    };
    labels.iter().all(label_ok)
        && labels
            .last()
            .is_some_and(|tld| !tld.bytes().all(|c| c.is_ascii_digit()))
}

impl SourceKey {
    /// `keccak256("onyx-source-key-v1" ‖ borsh(source, signer))`: the key's id.
    /// Provenance is not part of it, so re-reviewing where a key is published
    /// does not change the id claims point at.
    pub fn id(&self) -> Hash32 {
        let mut b = KEY_DOMAIN.to_vec();
        b.extend_from_slice(&borsh::to_vec(&(&self.source, &self.signer)).unwrap_or_default());
        keccak256(&b)
    }

    /// Shape check: a dotted source id, a non-zero signer, a valid origin and
    /// a non-zero fetched hash and time.
    pub fn validate(&self) -> Result<(), SignedFormError> {
        if !is_dotted_id(&self.source.0) {
            return Err(SignedFormError::Bad("source id"));
        }
        if self.signer == [0u8; 20] {
            return Err(SignedFormError::Bad("signer"));
        }
        if !is_origin_host(self.provenance.origin()) {
            return Err(SignedFormError::Bad("origin host"));
        }
        let (hash, at) = match &self.provenance {
            KeyProvenance::WellKnown {
                document_hash,
                fetched_unix,
                ..
            } => (document_hash, fetched_unix),
            KeyProvenance::DnsTxt {
                record_hash,
                fetched_unix,
                ..
            } => (record_hash, fetched_unix),
        };
        if *hash == Hash32::ZERO || *at == 0 {
            return Err(SignedFormError::Bad("provenance hash or time"));
        }
        Ok(())
    }
}

/// What a source signs: one value about one subject at one time.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SignedPayload {
    /// The registered source signing (must be the key's source).
    pub source: SourceId,
    /// [`SourceKey::id`] of the signing key (binds key and source).
    pub key: Hash32,
    /// The metric.
    pub metric: MetricId,
    /// The subject.
    pub subject: Subject,
    /// The value.
    pub value: Value,
    /// When the source says the value held (unix seconds).
    pub observed_unix: u64,
    /// Per-key sequence number, strictly increasing over accepted reports.
    pub sequence: u64,
}

impl SignedPayload {
    /// `keccak256("onyx-signed-source-v1" ‖ borsh(payload))`.
    pub fn digest(&self) -> Hash32 {
        let mut b = PAYLOAD_DOMAIN.as_bytes().to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }

    /// The exact text the key signs with EIP-191 personal_sign:
    /// `onyx-signed-source-v1:0x<digest hex>` (88 ASCII bytes). A fixed,
    /// readable form, so any EVM wallet or HSM can sign it and a human can see
    /// what it is.
    pub fn message(&self) -> String {
        format!("{PAYLOAD_DOMAIN}:0x{}", self.digest().to_hex())
    }

    /// Shape check: dotted ids, non-zero key, valid value, non-zero time.
    pub fn validate(&self) -> Result<(), SignedFormError> {
        if !is_dotted_id(&self.source.0) {
            return Err(SignedFormError::Bad("source id"));
        }
        if !is_dotted_id(&self.metric.0) {
            return Err(SignedFormError::Bad("metric"));
        }
        if self.key == Hash32::ZERO {
            return Err(SignedFormError::Bad("key id"));
        }
        if self.observed_unix == 0 {
            return Err(SignedFormError::Bad("observed time"));
        }
        self.value.validate()?;
        Ok(())
    }
}

/// A signed payload: what a source hands out and a claim points at.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SignedReport {
    /// What was signed.
    pub payload: SignedPayload,
    /// 65-byte EIP-191 signature `r ‖ s ‖ v` over [`SignedPayload::message`]
    /// (canonical form only: `v` 27 or 28, low `s`).
    pub signature: [u8; 65],
}

impl SignedReport {
    /// `keccak256("onyx-signed-report-v1" ‖ borsh(report))`: what a claim's
    /// `SignedSource { report }` and its snapshot hash name.
    pub fn hash(&self) -> Hash32 {
        let mut b = REPORT_DOMAIN.to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SourceKey {
        SourceKey {
            source: SourceId("example.signed".into()),
            signer: [7; 20],
            provenance: KeyProvenance::WellKnown {
                origin: "data.example.org".into(),
                document_hash: keccak256(b"doc"),
                fetched_unix: 1_000,
            },
        }
    }

    #[test]
    fn origin_hosts() {
        for ok in [
            "example.org",
            "data.example.org",
            "a-b.example.co.uk",
            "x1.io",
        ] {
            assert!(is_origin_host(ok), "{ok}");
        }
        for bad in [
            "",
            "localhost",
            "Example.org",
            "127.0.0.1",
            "-a.example.org",
            "a-.example.org",
            "a..b",
            "example.org.",
            "exa mple.org",
            "https://example.org",
            "example.org/path",
            "example.org:443",
        ] {
            assert!(!is_origin_host(bad), "{bad}");
        }
        assert!(!is_origin_host(&format!("{}.org", "a".repeat(64))));
        // At most MAX_ORIGIN_LEN (253) bytes, with every label valid (mutation L4).
        let host = |n: usize| {
            let mut labels = vec!["a".repeat(63); n / 64];
            labels.push("b".repeat(n - 64 * (n / 64) - 4));
            labels.push("org".into());
            labels.join(".")
        };
        assert_eq!(host(MAX_ORIGIN_LEN).len(), MAX_ORIGIN_LEN);
        assert!(is_origin_host(&host(MAX_ORIGIN_LEN)));
        assert_eq!(host(MAX_ORIGIN_LEN + 1).len(), MAX_ORIGIN_LEN + 1);
        assert!(!is_origin_host(&host(MAX_ORIGIN_LEN + 1)));
    }

    #[test]
    fn key_shape_and_id() {
        let k = key();
        assert_eq!(k.validate(), Ok(()));
        assert_eq!(
            k.provenance.location(),
            "https://data.example.org/.well-known/onyx-source-keys.json"
        );
        // Provenance is not in the id: a re-review keeps the id.
        let mut moved = k.clone();
        moved.provenance = KeyProvenance::DnsTxt {
            origin: "data.example.org".into(),
            record_hash: keccak256(b"txt"),
            fetched_unix: 2_000,
        };
        assert_eq!(moved.id(), k.id());
        assert_eq!(
            moved.provenance.location(),
            "_onyx-source-keys.data.example.org"
        );
        // Source and signer are.
        let mut other = k.clone();
        other.source = SourceId("example.other".into());
        assert_ne!(other.id(), k.id());
        let mut bad = k.clone();
        bad.signer = [0; 20];
        assert!(bad.validate().is_err());
        let mut bad = k;
        bad.provenance = KeyProvenance::WellKnown {
            origin: "data.example.org".into(),
            document_hash: Hash32::ZERO,
            fetched_unix: 1,
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn payload_message_is_fixed_text() {
        let p = SignedPayload {
            source: SourceId("example.signed".into()),
            key: key().id(),
            metric: MetricId("example.signed.reserve".into()),
            subject: Subject::Entity("entity:example".into()),
            value: Value::UInt(42),
            observed_unix: 1_700_000_000,
            sequence: 1,
        };
        assert_eq!(p.validate(), Ok(()));
        let m = p.message();
        assert_eq!(m.len(), PAYLOAD_DOMAIN.len() + 1 + 66);
        assert!(m.starts_with("onyx-signed-source-v1:0x"));
        // Every field is in the digest.
        let mut q = p.clone();
        q.sequence = 2;
        assert_ne!(q.digest(), p.digest());
        let mut q = p.clone();
        q.value = Value::UInt(43);
        assert_ne!(q.digest(), p.digest());
        let mut q = p.clone();
        q.observed_unix = 0;
        assert!(q.validate().is_err());
    }
}
