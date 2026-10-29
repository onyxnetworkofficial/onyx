//! Revocation of first-party attestations (ADR-0015, R-ATTEST-7).
//!
//! An entity revokes "address A is ours" from a height on, signed by any of
//! its registered entity keys. No address signature is needed: an entity that
//! lost an address key must still be able to disown the address. A revocation
//! covers every attestation for the same entity and address at heights at or
//! after `effective`.
//!
//! Revocations are recorded in the registry ([`apply_revocation`]), whose root
//! every shard pins, so all validators apply the same set (L12-F9).

use crate::attest::{eip191_recover, MAX_STATEMENT_LEN, NONCE_LEN};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::{is_dotted_id, is_entity_id, parse_address};
use onyx_core::{keccak256, to_canonical_bytes, Hash32, Registry, RegistryError, StaticRegistry};

/// First line of every revocation statement.
pub const REVOCATION_HEADER: &str = "Onyx address revocation v1";

/// Parsed revocation statement. The signed bytes are exactly [`RevocationStatement::render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevocationStatement {
    /// Network domain.
    pub domain: String,
    /// Entity disowning the address.
    pub entity: String,
    /// Namespaced canonical address.
    pub address: String,
    /// First height (in the address's chain) at which attestations no longer hold.
    pub effective: u64,
    /// Random nonce.
    pub nonce: [u8; NONCE_LEN],
}

impl RevocationStatement {
    /// Exact signed text.
    pub fn render(&self) -> String {
        format!(
            "{REVOCATION_HEADER}\ndomain: {}\nentity: {}\naddress: {}\neffective: {}\nnonce: {}",
            self.domain,
            self.entity,
            self.address,
            self.effective,
            hex::encode(self.nonce)
        )
    }

    /// Strict parse; must re-render to the same bytes.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_STATEMENT_LEN {
            return None;
        }
        let mut lines = text.split('\n');
        if lines.next() != Some(REVOCATION_HEADER) {
            return None;
        }
        let mut field = |name: &str| {
            lines
                .next()
                .and_then(|l| l.strip_prefix(name))
                .and_then(|l| l.strip_prefix(": "))
        };
        let domain = field("domain")?.to_string();
        let entity = field("entity")?.to_string();
        let address = field("address")?.to_string();
        let effective_s = field("effective")?;
        let nonce_s = field("nonce")?;
        if lines.next().is_some() {
            return None;
        }
        if !effective_s.bytes().all(|c| c.is_ascii_digit()) || nonce_s.len() != 2 * NONCE_LEN {
            return None;
        }
        let effective = effective_s.parse().ok()?;
        let mut nonce = [0u8; NONCE_LEN];
        hex::decode_to_slice(nonce_s, &mut nonce).ok()?;
        let r = RevocationStatement {
            domain,
            entity,
            address,
            effective,
            nonce,
        };
        let ok = is_dotted_id(&r.domain)
            && is_entity_id(&r.entity)
            && parse_address(&r.address).is_some()
            && r.render() == text;
        ok.then_some(r)
    }
}

/// A signed revocation.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Revocation {
    /// Revocation statement text, exactly as signed.
    pub statement: String,
    /// Registered entity key that signed it.
    pub entity_key: [u8; 20],
    /// EIP-191 signature, canonical form only.
    pub entity_sig: [u8; 65],
}

impl Revocation {
    /// Content hash of the canonical encoding.
    pub fn content_hash(&self) -> Hash32 {
        let mut b = b"onyx-revocation-v1".to_vec();
        b.extend_from_slice(&to_canonical_bytes(self).unwrap_or_default());
        keccak256(&b)
    }

    /// The parsed statement if the signature is valid, by a key registered for
    /// the statement's entity, and the domain is the registry's.
    pub fn verify(&self, reg: &dyn Registry) -> Option<RevocationStatement> {
        let st = RevocationStatement::parse(&self.statement)?;
        let ok = !reg.domain().is_empty()
            && st.domain == reg.domain()
            && reg.entity_key_known(&st.entity, &self.entity_key)
            && eip191_recover(self.statement.as_bytes(), &self.entity_sig) == Some(self.entity_key);
        ok.then_some(st)
    }
}

/// Revocation error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RevokeError {
    /// Bad text, wrong domain, unregistered key or bad signature.
    #[error("revocation invalid")]
    Invalid,
    /// Registry refused the record.
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),
}

/// Check a revocation and record it in the registry, with its signing key and
/// hash (a governance step; the new root applies to shards opened after it).
/// Removing the key later keeps the record unless governance lists its hash
/// (`StaticRegistry::remove_entity_key`, L14-F5).
pub fn apply_revocation(
    reg: &mut StaticRegistry,
    rev: &Revocation,
) -> Result<RevocationStatement, RevokeError> {
    let st = rev.verify(reg).ok_or(RevokeError::Invalid)?;
    reg.add_revocation(
        &st.entity,
        &st.address,
        st.effective,
        rev.entity_key,
        rev.content_hash(),
    )?;
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attest::test_support::*;
    use onyx_core::{AddressCanon, EntityKind, StaticRegistry};

    struct AnyCanon;
    impl AddressCanon for AnyCanon {
        fn namespace(&self) -> &str {
            "t"
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[]
        }
        fn is_canonical(&self, _: &str) -> bool {
            true
        }
    }

    fn reg() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.set_domain("onyx.devnet")
            .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
            .and_then(|r| r.add_entity("entity:other", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(AnyCanon)))
            .and_then(|r| r.add_entity_key("entity:example", key_address(&test_secret())))
            .expect("registry");
        r
    }

    fn rev(entity: &str, address: &str, effective: u64) -> Revocation {
        let text = RevocationStatement {
            domain: "onyx.devnet".into(),
            entity: entity.into(),
            address: address.into(),
            effective,
            nonce: [3; NONCE_LEN],
        }
        .render();
        Revocation {
            entity_key: key_address(&test_secret()),
            entity_sig: eip191_sign(&test_secret(), text.as_bytes()),
            statement: text,
        }
    }

    #[test]
    fn revocation_recorded_from_effective_height() {
        let mut r = reg();
        let st = apply_revocation(&mut r, &rev("entity:example", "t:a", 50)).expect("apply");
        assert_eq!(st.effective, 50);
        assert_eq!(r.revoked_from("entity:example", "t:a"), Some(50));
        assert_eq!(r.revoked_from("entity:example", "t:b"), None);
        assert_eq!(r.revoked_from("entity:other", "t:a"), None);
    }

    #[test]
    fn revocation_must_be_signed_by_entity_key() {
        let mut r = reg();
        let v = rev("entity:example", "t:a", 1);
        // entity:other has no registered key, so it cannot revoke.
        assert_eq!(
            apply_revocation(&mut r, &rev("entity:other", "t:a", 1)),
            Err(RevokeError::Invalid)
        );
        let mut other_net = StaticRegistry::new();
        other_net
            .set_domain("onyx.testnet")
            .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(AnyCanon)))
            .and_then(|r| r.add_entity_key("entity:example", key_address(&test_secret())))
            .expect("registry");
        assert_eq!(
            apply_revocation(&mut other_net, &v),
            Err(RevokeError::Invalid)
        );
        let mut t = v.clone();
        t.statement = t.statement.replace("effective: 1", "effective: 2");
        assert_eq!(apply_revocation(&mut r, &t), Err(RevokeError::Invalid));
        let mut sg = v.clone();
        sg.entity_sig[64] ^= 0b11;
        assert_eq!(apply_revocation(&mut r, &sg), Err(RevokeError::Invalid));
        assert_eq!(r.revoked_from("entity:example", "t:a"), None);
    }

    #[test]
    fn revocation_text_is_strict() {
        let good = rev("entity:example", "t:a", 7).statement;
        assert!(RevocationStatement::parse(&good).is_some());
        for bad in [
            format!("{good}\n"),
            good.replace("effective: 7", "effective: 07"),
            good.replace("effective: 7", "effective: +7"),
            good.replace("effective: 7", "effective: "),
            good.replace("revocation v1", "attestation v1"),
            good.replace("entity: entity:example", "entity: Entity:example"),
        ] {
            assert!(RevocationStatement::parse(&bad).is_none(), "{bad:?}");
        }
    }
}
