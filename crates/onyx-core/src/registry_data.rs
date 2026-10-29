//! A registry as data (ROADMAP C23; ADR-0036 amendment 6 item 6).
//!
//! A [`StaticRegistry`] holds its address rules as code (trait objects), so it
//! has no byte form of its own. [`RegistryData`] is everything else, with each
//! rule named by its id; a client rebuilds the registry with the rule types it
//! trusts ([`RuleBuilder`], chosen at compile time like [`super::RuleSet`]) and
//! uses it only if the rebuilt [`Registry::root`] is the root it expects
//! ([`RegistryData::build_checked`]). The root hashes each rule's namespace,
//! id, vectors and results on them (R-REG-3), so a rule of the same id with
//! other vectors or other results gives another root and is refused; a rule
//! that agrees on every vector but differs elsewhere shares the root, which is
//! why a client rebuilds only with rule types it trusts (as settlement does,
//! L25-F2).
//!
//! Nothing here is priced: a registry names entities (organisations only),
//! sources with their reviewed rights, metrics, address rules, entity keys and
//! recorded revocations. Revocations name addresses ([`RegistryData::names_addresses`]):
//! a server withholds such a registry where label-derived data is withheld.

use super::{EntityKind, KeyState, MetricInfo, Registry, RegistryError, StaticRegistry};
use crate::hash::Hash32;
use crate::ids::{AddressCanon, AddressProof};
use crate::rights::DataRights;
use borsh::{BorshDeserialize, BorshSerialize};

/// Version byte of [`RegistryData`].
pub const REGISTRY_DATA_VERSION: u8 = 1;

/// The address rules a client trusts, by id: it rebuilds a served registry
/// with these and nothing else.
pub trait RuleBuilder {
    /// A fresh instance of the canon `canon_id`, if built in.
    fn canon(&self, canon_id: &str) -> Option<Box<dyn AddressCanon + Send + Sync>>;
    /// A fresh instance of the proof rule `proof_id`, if built in.
    fn proof(&self, proof_id: &str) -> Option<Box<dyn AddressProof + Send + Sync>>;
}

/// No built-in rules: rebuilds only a registry without canons or proofs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRules;

impl RuleBuilder for NoRules {
    fn canon(&self, _: &str) -> Option<Box<dyn AddressCanon + Send + Sync>> {
        None
    }
    fn proof(&self, _: &str) -> Option<Box<dyn AddressProof + Send + Sync>> {
        None
    }
}

/// One entity's keys with their states.
pub type EntityKeys = (String, Vec<([u8; 20], KeyState)>);
/// One (entity, address) with its recorded revocations (effective height,
/// key, statement hash).
pub type Revocations = ((String, String), Vec<(u64, [u8; 20], Hash32)>);

/// A registry's contents with its rules named by id (Borsh; sorted as the
/// registry keeps them).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RegistryData {
    /// [`REGISTRY_DATA_VERSION`].
    pub version: u8,
    /// The network's domain (empty: unset).
    pub domain: String,
    /// Entities by id.
    pub entities: Vec<(String, EntityKind)>,
    /// Sources' reviewed rights, by source id.
    pub sources: Vec<DataRights>,
    /// Canon ids, by namespace.
    pub canons: Vec<String>,
    /// Aggregate ids.
    pub aggregates: Vec<String>,
    /// Instrument namespaces.
    pub instrument_namespaces: Vec<String>,
    /// Metrics by id.
    pub metrics: Vec<(String, MetricInfo)>,
    /// Proof rule ids, by namespace.
    pub proofs: Vec<String>,
    /// Entity keys with their states, by entity then key.
    pub entity_keys: Vec<EntityKeys>,
    /// Recorded revocations: (entity, address) and its rows (effective height,
    /// key, statement hash).
    pub revocations: Vec<Revocations>,
}

/// Why served registry data was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryDataError {
    /// Another version.
    #[error("registry data version {0} is not supported")]
    Version(u8),
    /// A rule the client does not trust (or know).
    #[error("rule {0} is not built into this client")]
    UnknownRule(String),
    /// An entry the registry refuses.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The rebuilt registry has another root.
    #[error("the rebuilt registry has root {got}, not {want}")]
    Root {
        /// The rebuilt root.
        got: Hash32,
        /// The root the client expected.
        want: Hash32,
    },
}

impl StaticRegistry {
    /// This registry as data ([`RegistryData::build`] gives it back, with the
    /// same root, from the same rule types).
    pub fn data(&self) -> RegistryData {
        let mut canons: Vec<(&str, &str)> = self
            .canons
            .iter()
            .map(|c| (c.namespace(), c.canon_id()))
            .collect();
        canons.sort();
        let mut proofs: Vec<(&str, &str)> = self
            .proofs
            .iter()
            .map(|p| (p.namespace(), p.proof_id()))
            .collect();
        proofs.sort();
        RegistryData {
            version: REGISTRY_DATA_VERSION,
            domain: self.domain.clone(),
            entities: self.entities.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            sources: self.sources.values().cloned().collect(),
            canons: canons.into_iter().map(|(_, id)| id.to_string()).collect(),
            aggregates: self.aggregates.keys().cloned().collect(),
            instrument_namespaces: self.instrument_namespaces.keys().cloned().collect(),
            metrics: self.metrics.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            proofs: proofs.into_iter().map(|(_, id)| id.to_string()).collect(),
            entity_keys: self
                .entity_keys
                .iter()
                .map(|(e, keys)| (e.clone(), keys.iter().map(|(k, s)| (*k, *s)).collect()))
                .collect(),
            revocations: self
                .revocations
                .iter()
                .map(|(k, rows)| (k.clone(), rows.iter().copied().collect()))
                .collect(),
        }
    }
}

impl RegistryData {
    /// True when the data names an address (a recorded revocation): withheld
    /// wherever label-derived data is (ROADMAP L14, C23).
    pub fn names_addresses(&self) -> bool {
        !self.revocations.is_empty()
    }

    /// Rebuild the registry with `rules` (every canon and proof must be one of
    /// them). Every entry passes the registry's own checks again. The caller
    /// compares the root ([`RegistryData::build_checked`]).
    pub fn build(&self, rules: &dyn RuleBuilder) -> Result<StaticRegistry, RegistryDataError> {
        if self.version != REGISTRY_DATA_VERSION {
            return Err(RegistryDataError::Version(self.version));
        }
        let mut r = StaticRegistry::new();
        if !self.domain.is_empty() {
            r.set_domain(&self.domain)?;
        }
        for (id, kind) in &self.entities {
            r.add_entity(id, *kind)?;
        }
        for s in &self.sources {
            r.add_source(s.clone())?;
        }
        for id in &self.canons {
            let c = rules
                .canon(id)
                .ok_or_else(|| RegistryDataError::UnknownRule(id.clone()))?;
            r.add_canon(c)?;
        }
        for id in &self.proofs {
            let p = rules
                .proof(id)
                .ok_or_else(|| RegistryDataError::UnknownRule(id.clone()))?;
            r.add_proof(p)?;
        }
        for id in &self.aggregates {
            r.add_aggregate(id)?;
        }
        for ns in &self.instrument_namespaces {
            r.add_instrument_namespace(ns)?;
        }
        for (id, info) in &self.metrics {
            r.add_metric(id, *info)?;
        }
        // Keys start active (as registered), revocations are recorded while
        // their key is active, then each key takes its state.
        for (entity, keys) in &self.entity_keys {
            for (k, _) in keys {
                r.add_entity_key(entity, *k)?;
            }
        }
        for ((entity, address), rows) in &self.revocations {
            for (effective, key, statement) in rows {
                r.add_revocation(entity, address, *effective, *key, *statement)?;
            }
        }
        for (entity, keys) in &self.entity_keys {
            for (k, state) in keys {
                match state {
                    KeyState::Active => {}
                    KeyState::Retired { at_round } => {
                        r.retire_entity_key(entity, k, *at_round)?;
                    }
                    KeyState::Compromised => {
                        r.mark_key_compromised(entity, k)?;
                    }
                }
            }
        }
        Ok(r)
    }

    /// [`RegistryData::build`], then the rebuilt root must be `root`.
    pub fn build_checked(
        &self,
        rules: &dyn RuleBuilder,
        root: Hash32,
    ) -> Result<StaticRegistry, RegistryDataError> {
        let r = self.build(rules)?;
        let got = r.root();
        if got != root {
            return Err(RegistryDataError::Root { got, want: root });
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rights::{DataRights, Redistribution};
    use crate::source::SourceId;
    use crate::value::ValueKind;

    struct Lower;
    impl AddressCanon for Lower {
        fn namespace(&self) -> &str {
            "low"
        }
        fn canon_id(&self) -> &str {
            "test.canon.lower.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[("abc", true), ("ABC", false)]
        }
        fn is_canonical(&self, body: &str) -> bool {
            !body.is_empty() && body.bytes().all(|b| b.is_ascii_lowercase())
        }
    }

    /// Same id, other rule (digits too) with its own vectors: the root hashes
    /// each rule's vectors and results, so it differs.
    struct LooseLower;
    impl AddressCanon for LooseLower {
        fn namespace(&self) -> &str {
            "low"
        }
        fn canon_id(&self) -> &str {
            "test.canon.lower.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[("abc", true), ("ABC", false), ("a1", true)]
        }
        fn is_canonical(&self, body: &str) -> bool {
            !body.is_empty()
                && body
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        }
    }

    struct Rules(bool);
    impl RuleBuilder for Rules {
        fn canon(&self, id: &str) -> Option<Box<dyn AddressCanon + Send + Sync>> {
            match (id, self.0) {
                ("test.canon.lower.v1", true) => Some(Box::new(Lower)),
                ("test.canon.lower.v1", false) => Some(Box::new(LooseLower)),
                _ => None,
            }
        }
        fn proof(&self, _: &str) -> Option<Box<dyn AddressProof + Send + Sync>> {
            None
        }
    }

    fn full() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.set_domain("onyx.test").expect("domain");
        r.add_entity("entity:acme", EntityKind::Exchange)
            .expect("entity");
        r.add_entity("entity:bolt", EntityKind::Fund)
            .expect("entity");
        r.add_source(DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("src.a".into()),
            attribution: None,
            terms: String::new(),
        })
        .expect("source");
        r.add_canon(Box::new(Lower)).expect("canon");
        r.add_aggregate("chain.test").expect("aggregate");
        r.add_instrument_namespace("ticker").expect("ns");
        r.add_metric(
            "m.one",
            MetricInfo {
                value_kind: ValueKind::UInt,
                needs_label_set: false,
            },
        )
        .expect("metric");
        r.add_entity_key("entity:acme", [1; 20]).expect("key");
        r.add_entity_key("entity:acme", [2; 20]).expect("key");
        r.add_entity_key("entity:bolt", [3; 20]).expect("key");
        r.add_revocation("entity:acme", "low:abc", 7, [2; 20], Hash32([5; 32]))
            .expect("revocation");
        r.retire_entity_key("entity:acme", &[1; 20], 4)
            .expect("retire");
        r.mark_key_compromised("entity:acme", &[2; 20])
            .expect("compromise");
        r
    }

    /// ROADMAP C23: a registry rebuilt from its data with the same rule types
    /// has the same root, through Borsh too; every part of the data is in it.
    #[test]
    fn a_registry_rebuilt_from_its_data_has_the_same_root() {
        let r = full();
        let d = r.data();
        let bytes = borsh::to_vec(&d).expect("borsh");
        let back: RegistryData = borsh::from_slice(&bytes).expect("decode");
        assert_eq!(back, d);
        let again = back
            .build_checked(&Rules(true), r.root())
            .expect("same root");
        assert_eq!(again.root(), r.root());
        assert!(d.names_addresses());
        assert_eq!(
            again.entity_key_state("entity:acme", &[2; 20]),
            Some(KeyState::Compromised)
        );
        assert_eq!(again.revoked_from("entity:acme", "low:abc"), Some(7));
        // The empty registry and the example-like one without rules rebuild with no rules.
        let e = StaticRegistry::new();
        assert_eq!(
            e.data().build_checked(&NoRules, e.root()).map(|x| x.root()),
            Ok(e.root())
        );
    }

    /// A changed entry, an unknown rule, a rule of the same id that behaves
    /// differently, or another version: refused, never used.
    #[test]
    fn a_changed_or_unbuildable_registry_is_refused() {
        let r = full();
        let root = r.root();
        let d = r.data();
        let mut more = d.clone();
        more.aggregates.push("chain.other".into());
        assert!(matches!(
            more.build_checked(&Rules(true), root),
            Err(RegistryDataError::Root { .. })
        ));
        let mut fewer = d.clone();
        fewer.revocations.clear();
        assert!(matches!(
            fewer.build_checked(&Rules(true), root),
            Err(RegistryDataError::Root { .. })
        ));
        let mut state = d.clone();
        state.entity_keys[0].1[0].1 = KeyState::Active;
        assert!(matches!(
            state.build_checked(&Rules(true), root),
            Err(RegistryDataError::Root { .. })
        ));
        assert_eq!(
            d.build_checked(&NoRules, root).err(),
            Some(RegistryDataError::UnknownRule("test.canon.lower.v1".into()))
        );
        assert!(matches!(
            d.build_checked(&Rules(false), root),
            Err(RegistryDataError::Root { .. })
        ));
        let mut v = d.clone();
        v.version = 2;
        assert_eq!(
            v.build_checked(&Rules(true), root).err(),
            Some(RegistryDataError::Version(2))
        );
        // An entity the registry refuses (a person-like id): refused as it would be at review.
        let mut bad = d;
        bad.entities.push(("Jane Doe".into(), EntityKind::Fund));
        assert!(matches!(
            bad.build_checked(&Rules(true), root),
            Err(RegistryDataError::Registry(_))
        ));
    }
}
