//! Reviewed registries that claims and label sets are checked against.
//!
//! Before loop 3, rights and entities were self-declared by whoever submitted a
//! claim (THREATS R-RIGHTS-1, R-PRIV-1). A [`Registry`] is the reviewed source
//! of truth: which entities exist (organisations only), which sources exist and
//! under what rights, and which address namespaces have a canonical form.
//! Governance of registry contents: docs/ops/governance.md.

use crate::hash::{keccak256, Hash32};
use crate::ids::{is_dotted_id, is_entity_id, AddressCanon, AddressProof, ProofOutcome};
use crate::rights::{DataRights, Redistribution, RightsError};
use crate::source::SourceId;
use crate::value::ValueKind;
use borsh::{BorshDeserialize, BorshSerialize};
use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet};

/// The closed set of address rules a binary trusts when it settles (ADR-0019,
/// loop 26, L25-F2). The registry root pins only a rule's id, chain, window
/// and its results on its own vectors, so two registries with the same root
/// can run different code. Settlement therefore accepts a registry only if
/// every rule in it is exactly the Rust type this set names for that id. The
/// set is a type chosen at compile time by the binary, never a runtime value.
/// Built-in rule types must be stateless (unit structs): their behaviour may
/// depend on nothing but their type. `'static` so a ledger can record which
/// set settled a round by its `TypeId` (L26-F3).
pub trait RuleSet: 'static {
    /// Type implementing the address-control proof `proof_id`, if built in.
    fn proof_type(proof_id: &str) -> Option<TypeId>;
    /// Type implementing the canon `canon_id`, if built in.
    fn canon_type(canon_id: &str) -> Option<TypeId>;
}

/// Kinds of entity Onyx may name. There is deliberately no variant for a
/// natural person: Onyx covers organisations only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum EntityKind {
    /// Centralised exchange.
    Exchange,
    /// Exchange-traded fund or product.
    Etf,
    /// Investment fund.
    Fund,
    /// Onchain protocol or bridge.
    Protocol,
    /// Custodian.
    Custodian,
    /// Token or stablecoin issuer.
    Issuer,
    /// Mining pool.
    MiningPool,
    /// Public company or government treasury.
    Treasury,
}

/// What the registry says about a metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MetricInfo {
    /// Value variant every claim of this metric must use.
    pub value_kind: ValueKind,
    /// True if values depend on a label set (claims must pin one).
    pub needs_label_set: bool,
}

/// Lifecycle of an entity key (L15-F1, L15-F9). Keys are never deleted, so
/// history stays checkable and the root does not depend on removal order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum KeyState {
    /// Signs new attestations and revocations.
    Active,
    /// Retired by the entity from Onyx round `at_round` on: signs nothing new;
    /// what was finalized before that round stays valid as history (resolutions
    /// for scoring, revocations) (L16-F5).
    Retired {
        /// First round in which the key no longer counts.
        at_round: u64,
    },
    /// Compromised: nothing it signed counts as history; governance may drop its revocations.
    Compromised,
}

/// Read access to reviewed registries.
pub trait Registry {
    /// Kind of a registered entity, or `None` if not registered.
    fn entity_kind(&self, id: &str) -> Option<EntityKind>;
    /// Number of registered entities (`K` for calibration scoring, ADR-0016).
    fn entity_count(&self) -> u64;
    /// Reviewed rights for a registered source, or `None` if not registered.
    fn source_rights(&self, id: &SourceId) -> Option<&DataRights>;
    /// Canonical-form rule for an address namespace.
    fn address_canon(&self, namespace: &str) -> Option<&dyn AddressCanon>;
    /// True if the aggregate id (e.g. `btc.mainnet`) is registered.
    fn aggregate_known(&self, id: &str) -> bool;
    /// True if the instrument namespace (e.g. `ticker`, `series`) is registered.
    fn instrument_namespace_known(&self, namespace: &str) -> bool;
    /// Registered metric, or `None`.
    fn metric(&self, id: &str) -> Option<MetricInfo>;
    /// Address-control proof rule for a namespace (ADR-0015).
    fn address_proof(&self, namespace: &str) -> Option<&dyn AddressProof>;
    /// Proof rule whose `chain()` is `chain`, to map a snapshot to a namespace.
    fn proof_for_chain(&self, chain: &str) -> Option<&dyn AddressProof>;
    /// This network's domain (dotted id, e.g. `onyx.devnet`); attestation and
    /// revocation statements must name it. Empty until set: no attestation verifies.
    fn domain(&self) -> &str;
    /// True if `key` (a 20-byte EVM-style address) is an *active* signing key of
    /// `entity`. Entity keys authorise first-party attestations (ADR-0015).
    fn entity_key_known(&self, entity: &str, key: &[u8; 20]) -> bool {
        self.entity_key_state(entity, key) == Some(KeyState::Active)
    }
    /// State of a key ever registered for `entity`, or `None`.
    fn entity_key_state(&self, entity: &str, key: &[u8; 20]) -> Option<KeyState>;
    /// Earliest height (in the address's chain) from which `entity` has revoked
    /// its claim to `address`, or `None`. Revocations live in the registry so the
    /// shard's pinned root fixes which ones every validator applies (ADR-0015).
    fn revoked_from(&self, entity: &str, address: &str) -> Option<u64>;
    /// Hash of the full registry state. Pinned in every shard so all validators
    /// judge a round against the same registry, and a silent change is visible (R-REG-1).
    fn root(&self) -> Hash32;
}

/// One canon in the root: namespace, canon id, and its results on its own vectors.
type CanonRow<'a> = (&'a str, &'a str, Vec<(&'a str, bool)>);
/// One proof rule in the root: namespace, chain, proof id, max window, and each
/// vector in full with the rule's result on it (L13-F2: outcomes alone let a
/// different rule with different vectors share the root).
type ProofRow<'a> = (
    &'a str,
    &'a str,
    &'a str,
    u64,
    Vec<(&'a str, &'a str, &'a str, ProofOutcome)>,
);
/// One entity's keys in the root, with their states.
type KeyRow<'a> = (&'a str, Vec<([u8; 20], KeyState)>);
/// One recorded revocation: effective height, signing key, statement hash.
type RevocationRow = (u64, [u8; 20], Hash32);

/// Canonical snapshot of a registry, hashed into [`Registry::root`].
#[derive(BorshSerialize)]
struct RegistrySnapshot<'a> {
    entities: Vec<(&'a str, EntityKind)>,
    sources: Vec<&'a DataRights>,
    canons: Vec<CanonRow<'a>>,
    aggregates: Vec<&'a str>,
    instrument_namespaces: Vec<&'a str>,
    metrics: Vec<(&'a str, MetricInfo)>,
    proofs: Vec<ProofRow<'a>>,
    entity_keys: Vec<KeyRow<'a>>,
    revocations: Vec<(&'a (String, String), &'a BTreeSet<RevocationRow>)>,
    domain: &'a str,
}

/// Registry construction error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Malformed id.
    #[error("malformed registry id: {0}")]
    BadId(String),
    /// Source rights do not pass the ingest gate.
    #[error("source rights rejected: {0}")]
    Rights(#[from] RightsError),
    /// Rights declared for another source.
    #[error("rights.source must equal the registered source id: {0}")]
    SourceMismatch(String),
    /// Duplicate entry.
    #[error("duplicate registry entry: {0}")]
    Duplicate(String),
    /// Key for an entity that is not registered.
    #[error("entity not registered: {0}")]
    UnknownEntity(String),
}

/// In-memory registry, built from reviewed config. Insertion validates every entry.
#[derive(Default)]
pub struct StaticRegistry {
    entities: BTreeMap<String, EntityKind>,
    sources: BTreeMap<SourceId, DataRights>,
    canons: Vec<Box<dyn AddressCanon + Send + Sync>>,
    aggregates: BTreeMap<String, ()>,
    instrument_namespaces: BTreeMap<String, ()>,
    metrics: BTreeMap<String, MetricInfo>,
    proofs: Vec<Box<dyn AddressProof + Send + Sync>>,
    entity_keys: BTreeMap<String, BTreeMap<[u8; 20], KeyState>>,
    /// (entity, address) -> recorded revocations (effective height, key, statement hash).
    revocations: BTreeMap<(String, String), BTreeSet<RevocationRow>>,
    domain: String,
}

impl StaticRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an organisation.
    pub fn add_entity(&mut self, id: &str, kind: EntityKind) -> Result<&mut Self, RegistryError> {
        if !is_entity_id(id) {
            return Err(RegistryError::BadId(id.into()));
        }
        if self.entities.insert(id.into(), kind).is_some() {
            return Err(RegistryError::Duplicate(id.into()));
        }
        Ok(self)
    }

    /// Register a source with its reviewed rights. Forbidden sources cannot be registered.
    pub fn add_source(&mut self, rights: DataRights) -> Result<&mut Self, RegistryError> {
        rights.check_ingest()?;
        let id = rights.source.clone();
        if self.sources.contains_key(&id) {
            return Err(RegistryError::Duplicate(id.0));
        }
        self.sources.insert(id, rights);
        Ok(self)
    }

    /// True if every proof and canon in this registry is the built-in type
    /// `R` names for its id (L25-F2).
    pub fn rules_built_in<R: RuleSet>(&self) -> bool {
        self.canons.iter().all(|c| {
            let any: &dyn Any = c.as_ref();
            R::canon_type(c.canon_id()) == Some(any.type_id())
        }) && self.proofs.iter().all(|p| {
            let any: &dyn Any = p.as_ref();
            R::proof_type(p.proof_id()) == Some(any.type_id())
        })
    }

    /// Register an address canon (one per namespace).
    pub fn add_canon(
        &mut self,
        canon: Box<dyn AddressCanon + Send + Sync>,
    ) -> Result<&mut Self, RegistryError> {
        let ns = canon.namespace().to_string();
        if !is_dotted_id(canon.canon_id()) {
            return Err(RegistryError::BadId(canon.canon_id().into()));
        }
        // A canon must pass its own vectors to be registered.
        if canon
            .vectors()
            .iter()
            .any(|(body, want)| canon.is_canonical(body) != *want)
        {
            return Err(RegistryError::BadId(format!(
                "{} fails its vectors",
                canon.canon_id()
            )));
        }
        if self.canons.iter().any(|c| c.namespace() == ns) {
            return Err(RegistryError::Duplicate(ns));
        }
        self.canons.push(canon);
        Ok(self)
    }

    /// Register an aggregate id.
    pub fn add_aggregate(&mut self, id: &str) -> Result<&mut Self, RegistryError> {
        if !is_dotted_id(id) {
            return Err(RegistryError::BadId(id.into()));
        }
        self.aggregates.insert(id.into(), ());
        Ok(self)
    }

    /// Register an instrument namespace.
    pub fn add_instrument_namespace(&mut self, ns: &str) -> Result<&mut Self, RegistryError> {
        if ns.is_empty() || !ns.bytes().all(|c| c.is_ascii_lowercase()) {
            return Err(RegistryError::BadId(ns.into()));
        }
        self.instrument_namespaces.insert(ns.into(), ());
        Ok(self)
    }
}

impl StaticRegistry {
    /// Register an address-control proof rule (one per namespace). It must pass
    /// its own vectors, and its namespace must already have a canon.
    pub fn add_proof(
        &mut self,
        proof: Box<dyn AddressProof + Send + Sync>,
    ) -> Result<&mut Self, RegistryError> {
        let ns = proof.namespace().to_string();
        if !is_dotted_id(proof.proof_id()) || proof.max_window() == 0 {
            return Err(RegistryError::BadId(proof.proof_id().into()));
        }
        if !self.canons.iter().any(|c| c.namespace() == ns) {
            return Err(RegistryError::BadId(format!("no canon for namespace {ns}")));
        }
        if proof
            .vectors()
            .iter()
            .any(|v| proof.verify(v.body, v.message.as_bytes(), v.signature) != v.expect)
        {
            return Err(RegistryError::BadId(format!(
                "{} fails its vectors",
                proof.proof_id()
            )));
        }
        if self
            .proofs
            .iter()
            .any(|p| p.namespace() == ns || p.chain() == proof.chain())
        {
            return Err(RegistryError::Duplicate(ns));
        }
        self.proofs.push(proof);
        Ok(self)
    }

    /// Register a signing key for an already registered entity. Governance
    /// takes the key from the entity's own published channels (governance.md).
    pub fn add_entity_key(
        &mut self,
        entity: &str,
        key: [u8; 20],
    ) -> Result<&mut Self, RegistryError> {
        if !self.entities.contains_key(entity) {
            return Err(RegistryError::UnknownEntity(entity.into()));
        }
        let keys = self.entity_keys.entry(entity.into()).or_default();
        if keys.contains_key(&key) {
            return Err(RegistryError::Duplicate(format!(
                "{entity} key 0x{}",
                hex::encode(key)
            )));
        }
        keys.insert(key, KeyState::Active);
        Ok(self)
    }

    /// Set this network's domain (dotted id). Once only.
    pub fn set_domain(&mut self, domain: &str) -> Result<&mut Self, RegistryError> {
        if !is_dotted_id(domain) {
            return Err(RegistryError::BadId(domain.into()));
        }
        if !self.domain.is_empty() {
            return Err(RegistryError::Duplicate(domain.into()));
        }
        self.domain = domain.into();
        Ok(self)
    }

    /// Record that `entity` disowns `address` from `effective` on, signed by
    /// `key` (statement hash `statement`). The caller
    /// (`onyx_labels::revoke::apply_revocation`) has checked the signature and
    /// that `key` is registered for `entity`. The earliest height applies.
    pub fn add_revocation(
        &mut self,
        entity: &str,
        address: &str,
        effective: u64,
        key: [u8; 20],
        statement: Hash32,
    ) -> Result<&mut Self, RegistryError> {
        if !self.entity_key_known(entity, &key) {
            return Err(RegistryError::UnknownEntity(entity.into()));
        }
        if !crate::registry::is_registered_canonical_address(address, self) {
            return Err(RegistryError::BadId(address.into()));
        }
        self.revocations
            .entry((entity.into(), address.into()))
            .or_default()
            .insert((effective, key, statement));
        Ok(self)
    }

    fn set_key_state(
        &mut self,
        entity: &str,
        key: &[u8; 20],
        allowed_from: impl Fn(KeyState) -> bool,
        to: KeyState,
    ) -> Result<(), RegistryError> {
        match self
            .entity_keys
            .get_mut(entity)
            .and_then(|keys| keys.get_mut(key))
        {
            Some(state) if allowed_from(*state) => {
                *state = to;
                Ok(())
            }
            _ => Err(RegistryError::UnknownEntity(format!(
                "{entity} key 0x{}",
                hex::encode(key)
            ))),
        }
    }

    /// Retire an active key (governance, routine rotation). It signs nothing
    /// new; its past attestations still count as history for scoring and its
    /// revocations stay in force (L14-F5, L15-F1).
    pub fn retire_entity_key(
        &mut self,
        entity: &str,
        key: &[u8; 20],
        at_round: u64,
    ) -> Result<&mut Self, RegistryError> {
        self.set_key_state(
            entity,
            key,
            |s| s == KeyState::Active,
            KeyState::Retired { at_round },
        )?;
        Ok(self)
    }

    /// Mark a key compromised (governance). Nothing it signed counts any more
    /// as attestation or history. Its revocations stay until governance drops
    /// them by hash with [`StaticRegistry::drop_revocations`].
    pub fn mark_key_compromised(
        &mut self,
        entity: &str,
        key: &[u8; 20],
    ) -> Result<&mut Self, RegistryError> {
        self.set_key_state(
            entity,
            key,
            |s| s != KeyState::Compromised,
            KeyState::Compromised,
        )?;
        Ok(self)
    }

    /// Drop revocations signed by a compromised key, by statement hash (a
    /// thief's revocations, L13-F4). Possible at any time after the compromise
    /// is recorded (L15-F9). Each hash must be a revocation that key signed.
    pub fn drop_revocations(
        &mut self,
        entity: &str,
        key: &[u8; 20],
        hashes: &[Hash32],
    ) -> Result<&mut Self, RegistryError> {
        if self.entity_key_state(entity, key) != Some(KeyState::Compromised) {
            return Err(RegistryError::BadId(format!(
                "{entity} key 0x{} is not marked compromised",
                hex::encode(key)
            )));
        }
        let signed_by_key = |h: &Hash32| {
            self.revocations.iter().any(|((e, _), rows)| {
                e == entity && rows.iter().any(|(_, k, sh)| k == key && sh == h)
            })
        };
        if let Some(h) = hashes.iter().find(|h| !signed_by_key(h)) {
            return Err(RegistryError::BadId(format!(
                "revocation {h} not signed by key"
            )));
        }
        for ((e, _), rows) in self.revocations.iter_mut() {
            if e == entity {
                rows.retain(|(_, k, sh)| !(k == key && hashes.contains(sh)));
            }
        }
        self.revocations.retain(|_, rows| !rows.is_empty());
        Ok(self)
    }

    /// Register a metric with its value kind and label requirement.
    pub fn add_metric(&mut self, id: &str, info: MetricInfo) -> Result<&mut Self, RegistryError> {
        if !is_dotted_id(id) {
            return Err(RegistryError::BadId(id.into()));
        }
        if self.metrics.insert(id.into(), info).is_some() {
            return Err(RegistryError::Duplicate(id.into()));
        }
        Ok(self)
    }
}

impl Registry for StaticRegistry {
    fn entity_kind(&self, id: &str) -> Option<EntityKind> {
        self.entities.get(id).copied()
    }

    fn entity_count(&self) -> u64 {
        u64::try_from(self.entities.len()).unwrap_or(u64::MAX)
    }

    fn source_rights(&self, id: &SourceId) -> Option<&DataRights> {
        self.sources.get(id)
    }

    fn address_canon(&self, namespace: &str) -> Option<&dyn AddressCanon> {
        self.canons
            .iter()
            .find(|c| c.namespace() == namespace)
            .map(|c| c.as_ref() as &dyn AddressCanon)
    }

    fn aggregate_known(&self, id: &str) -> bool {
        self.aggregates.contains_key(id)
    }

    fn instrument_namespace_known(&self, namespace: &str) -> bool {
        self.instrument_namespaces.contains_key(namespace)
    }

    fn metric(&self, id: &str) -> Option<MetricInfo> {
        self.metrics.get(id).copied()
    }

    fn address_proof(&self, namespace: &str) -> Option<&dyn AddressProof> {
        self.proofs
            .iter()
            .find(|p| p.namespace() == namespace)
            .map(|p| p.as_ref() as &dyn AddressProof)
    }

    fn entity_key_state(&self, entity: &str, key: &[u8; 20]) -> Option<KeyState> {
        self.entity_keys
            .get(entity)
            .and_then(|keys| keys.get(key))
            .copied()
    }

    fn revoked_from(&self, entity: &str, address: &str) -> Option<u64> {
        self.revocations
            .get(&(entity.to_string(), address.to_string()))
            .and_then(|rows| rows.iter().map(|(h, _, _)| *h).min())
    }

    fn proof_for_chain(&self, chain: &str) -> Option<&dyn AddressProof> {
        self.proofs
            .iter()
            .find(|p| p.chain() == chain)
            .map(|p| p.as_ref() as &dyn AddressProof)
    }

    fn domain(&self) -> &str {
        &self.domain
    }

    fn root(&self) -> Hash32 {
        // Destructure so a new registry field is a compile error here until it is hashed.
        let StaticRegistry {
            entities,
            sources,
            canons,
            aggregates,
            instrument_namespaces,
            metrics,
            proofs,
            entity_keys,
            revocations,
            domain,
        } = self;
        let mut canon_rows: Vec<CanonRow<'_>> = canons
            .iter()
            .map(|c| {
                let results = c
                    .vectors()
                    .iter()
                    .map(|(body, _)| (*body, c.is_canonical(body)))
                    .collect();
                (c.namespace(), c.canon_id(), results)
            })
            .collect();
        canon_rows.sort_by(|a, b| a.0.cmp(b.0));
        let mut proof_rows: Vec<ProofRow<'_>> = proofs
            .iter()
            .map(|p| {
                let results = p
                    .vectors()
                    .iter()
                    .map(|v| {
                        let got = p.verify(v.body, v.message.as_bytes(), v.signature);
                        (v.body, v.message, v.signature, got)
                    })
                    .collect();
                (
                    p.namespace(),
                    p.chain(),
                    p.proof_id(),
                    p.max_window(),
                    results,
                )
            })
            .collect();
        proof_rows.sort_by(|a, b| a.0.cmp(b.0));
        let snap = RegistrySnapshot {
            entities: entities.iter().map(|(k, v)| (k.as_str(), *v)).collect(),
            sources: sources.values().collect(),
            canons: canon_rows,
            aggregates: aggregates.keys().map(String::as_str).collect(),
            instrument_namespaces: instrument_namespaces.keys().map(String::as_str).collect(),
            metrics: metrics.iter().map(|(k, v)| (k.as_str(), *v)).collect(),
            proofs: proof_rows,
            entity_keys: entity_keys
                .iter()
                .map(|(k, v)| (k.as_str(), v.iter().map(|(key, st)| (*key, *st)).collect()))
                .collect(),
            revocations: revocations.iter().collect(),
            domain,
        };
        let mut b = b"onyx-registry-v1".to_vec();
        b.extend_from_slice(&borsh::to_vec(&snap).unwrap_or_default());
        keccak256(&b)
    }
}

/// True if `s` is a `<ns>:<body>` address in canonical form under `reg`.
pub fn is_registered_canonical_address(s: &str, reg: &dyn Registry) -> bool {
    crate::ids::parse_address(s)
        .is_some_and(|(ns, body)| reg.address_canon(ns).is_some_and(|c| c.is_canonical(body)))
}

/// Rank for "most restrictive" comparisons. Forbidden never reaches here.
pub fn restrictiveness(r: Redistribution) -> u8 {
    match r {
        Redistribution::Allowed => 0,
        Redistribution::Restricted => 1,
        Redistribution::Forbidden => 2,
    }
}

#[path = "registry_data.rs"]
mod data;
pub use data::{NoRules, RegistryData, RegistryDataError, RuleBuilder, REGISTRY_DATA_VERSION};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_forbidden_source_and_bad_ids() {
        let mut r = StaticRegistry::new();
        let mut bad = DataRights::public_chain(SourceId("vendor.x".into()));
        bad.redistribution = Redistribution::Forbidden;
        assert!(matches!(r.add_source(bad), Err(RegistryError::Rights(_))));
        assert!(r.add_entity("Jane Doe", EntityKind::Fund).is_err());
        assert!(r
            .add_entity("entity:coinbase", EntityKind::Exchange)
            .is_ok());
        assert!(matches!(
            r.add_entity("entity:coinbase", EntityKind::Exchange),
            Err(RegistryError::Duplicate(_))
        ));
    }

    /// Lowercase-only canon; `true` makes it (wrongly) accept uppercase.
    struct LowerCanon(bool);
    impl AddressCanon for LowerCanon {
        fn namespace(&self) -> &str {
            "t"
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[("abc", true), ("ABC", false)]
        }
        fn is_canonical(&self, body: &str) -> bool {
            self.0 || !body.bytes().any(|c| c.is_ascii_uppercase())
        }
    }

    /// Same id and vector results as [`LowerCanon`], other code.
    struct Impostor;
    impl AddressCanon for Impostor {
        fn namespace(&self) -> &str {
            "t"
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[("abc", true), ("ABC", false)]
        }
        fn is_canonical(&self, body: &str) -> bool {
            body == "abc"
        }
    }

    struct OnlyLower;
    impl RuleSet for OnlyLower {
        fn proof_type(_: &str) -> Option<TypeId> {
            None
        }
        fn canon_type(id: &str) -> Option<TypeId> {
            (id == "onyx.canon.test.v1").then(TypeId::of::<LowerCanon>)
        }
    }

    /// L25-F2: the root cannot tell rule code apart; `rules_built_in` can.
    #[test]
    fn built_in_rules_are_checked_by_type() {
        let mut good = StaticRegistry::new();
        good.add_canon(Box::new(LowerCanon(false))).expect("canon");
        let mut bad = StaticRegistry::new();
        bad.add_canon(Box::new(Impostor)).expect("impostor");
        assert_eq!(good.root(), bad.root());
        assert!(good.rules_built_in::<OnlyLower>());
        assert!(!bad.rules_built_in::<OnlyLower>());
    }

    /// R-REG-1: any change to registry contents changes the root.
    #[test]
    fn root_tracks_every_change() {
        let mut r = StaticRegistry::new();
        assert!(r.add_canon(Box::new(LowerCanon(true))).is_err()); // fails own vectors
        let r0 = r.root();
        r.add_entity("entity:okx", EntityKind::Exchange)
            .expect("entity");
        let r1 = r.root();
        assert_ne!(r0, r1);
        r.add_source(DataRights::public_chain(SourceId(
            "chain.btc.mainnet".into(),
        )))
        .expect("source");
        let r2 = r.root();
        assert_ne!(r1, r2);
        r.add_metric(
            "crypto.btc.entity_balance",
            MetricInfo {
                value_kind: ValueKind::UInt,
                needs_label_set: true,
            },
        )
        .expect("metric");
        let r3 = r.root();
        assert_ne!(r2, r3);
        r.add_aggregate("btc.mainnet").expect("aggregate");
        let r4 = r.root();
        assert_ne!(r3, r4);
        r.add_instrument_namespace("ticker").expect("ns");
        let r5 = r.root();
        assert_ne!(r4, r5);
        r.add_canon(Box::new(LowerCanon(false))).expect("canon");
        let r6 = r.root();
        assert_ne!(r5, r6);
        // Same canon id, different behaviour on its vectors: different root (R-REG-3).
        let mut drifted = StaticRegistry::new();
        let mut honest = StaticRegistry::new();
        honest
            .add_canon(Box::new(LowerCanon(false)))
            .expect("canon");
        drifted.canons.push(Box::new(LowerCanon(true)));
        assert_ne!(honest.root(), drifted.root());
        // Insertion order does not matter.
        let mut a = StaticRegistry::new();
        a.add_entity("entity:a", EntityKind::Fund)
            .and_then(|r| r.add_entity("entity:b", EntityKind::Etf))
            .expect("a");
        let mut b = StaticRegistry::new();
        b.add_entity("entity:b", EntityKind::Etf)
            .and_then(|r| r.add_entity("entity:a", EntityKind::Fund))
            .expect("b");
        assert_eq!(a.root(), b.root());
    }

    /// Proof rule for namespace `t`: valid iff the signature equals the message.
    /// `drift` makes it (wrongly) accept everything.
    struct EchoProof {
        drift: bool,
        window: u64,
    }
    const ECHO_VECTORS: &[crate::ids::ProofVector] = &[
        crate::ids::ProofVector {
            body: "a",
            message: "m",
            signature: "m",
            expect: ProofOutcome::Valid,
        },
        crate::ids::ProofVector {
            body: "a",
            message: "m",
            signature: "x",
            expect: ProofOutcome::Invalid,
        },
    ];
    impl AddressProof for EchoProof {
        fn namespace(&self) -> &str {
            "t"
        }
        fn chain(&self) -> &str {
            "t.main"
        }
        fn proof_id(&self) -> &str {
            "onyx.proof.test.v1"
        }
        fn max_window(&self) -> u64 {
            self.window
        }
        fn vectors(&self) -> &'static [crate::ids::ProofVector] {
            ECHO_VECTORS
        }
        fn verify(&self, _: &str, msg: &[u8], sig: &str) -> ProofOutcome {
            if self.drift || msg == sig.as_bytes() {
                ProofOutcome::Valid
            } else {
                ProofOutcome::Invalid
            }
        }
    }

    /// ADR-0015: proof rules must pass their vectors, need a canon, and are in the root.
    #[test]
    fn proof_rules_and_entity_keys() {
        let honest = || {
            Box::new(EchoProof {
                drift: false,
                window: 10,
            })
        };
        let mut r = StaticRegistry::new();
        assert!(r.add_proof(honest()).is_err(), "no canon for t yet");
        r.add_canon(Box::new(LowerCanon(false))).expect("canon");
        assert!(r
            .add_proof(Box::new(EchoProof {
                drift: true,
                window: 10
            }))
            .is_err());
        assert!(r
            .add_proof(Box::new(EchoProof {
                drift: false,
                window: 0
            }))
            .is_err());
        let r0 = r.root();
        r.add_proof(honest()).expect("proof");
        assert!(matches!(
            r.add_proof(honest()),
            Err(RegistryError::Duplicate(_))
        ));
        let r1 = r.root();
        assert_ne!(r0, r1);
        assert!(r.address_proof("t").is_some() && r.address_proof("u").is_none());

        // Same proof id, other window: other root.
        let mut w = StaticRegistry::new();
        w.add_canon(Box::new(LowerCanon(false)))
            .and_then(|w| {
                w.add_proof(Box::new(EchoProof {
                    drift: false,
                    window: 11,
                }))
            })
            .expect("w");
        assert_ne!(w.root(), r1);
        // Drifted behaviour without an id bump: other root (as R-REG-3 for canons).
        let mut d = StaticRegistry::new();
        d.add_canon(Box::new(LowerCanon(false))).expect("canon");
        d.proofs.push(Box::new(EchoProof {
            drift: true,
            window: 10,
        }));
        assert_ne!(d.root(), r1);

        // Keys only for registered entities; each key once; keys are in the root.
        let k = [7u8; 20];
        assert!(matches!(
            r.add_entity_key("entity:a", k),
            Err(RegistryError::UnknownEntity(_))
        ));
        r.add_entity("entity:a", EntityKind::Fund).expect("entity");
        let r2 = r.root();
        r.add_entity_key("entity:a", k).expect("key");
        assert!(matches!(
            r.add_entity_key("entity:a", k),
            Err(RegistryError::Duplicate(_))
        ));
        assert_ne!(r.root(), r2);
        assert!(r.entity_key_known("entity:a", &k));
        assert!(!r.entity_key_known("entity:a", &[8u8; 20]));
        assert!(!r.entity_key_known("entity:b", &k));

        // Revocations: registered key of the entity, canonical address; earliest wins; in the root.
        let h = |n: u8| crate::keccak256(&[n]);
        assert!(r.add_revocation("entity:b", "t:x", 5, k, h(0)).is_err());
        assert!(r.add_revocation("entity:a", "t:X", 5, k, h(0)).is_err());
        assert!(r
            .add_revocation("entity:a", "t:x", 5, [8; 20], h(0))
            .is_err());
        let r3 = r.root();
        r.add_revocation("entity:a", "t:x", 9, k, h(1))
            .expect("rev");
        r.add_revocation("entity:a", "t:x", 7, k, h(2))
            .expect("rev");
        assert_eq!(r.revoked_from("entity:a", "t:x"), Some(7));
        assert_eq!(r.revoked_from("entity:a", "t:y"), None);
        assert_ne!(r.root(), r3);

        // L13-F4, L15-F9: a stolen key revokes from 0; once marked compromised,
        // governance can drop exactly its revocations, at any later time.
        let k2 = [9u8; 20];
        r.add_entity_key("entity:a", k2).expect("key 2");
        r.add_revocation("entity:a", "t:x", 0, k2, h(3))
            .expect("thief");
        assert_eq!(r.revoked_from("entity:a", "t:x"), Some(0));
        assert!(
            r.drop_revocations("entity:a", &k2, &[h(3)]).is_err(),
            "not yet compromised"
        );
        r.mark_key_compromised("entity:a", &k2)
            .expect("compromised");
        assert_eq!(
            r.entity_key_state("entity:a", &k2),
            Some(KeyState::Compromised)
        );
        assert!(!r.entity_key_known("entity:a", &k2));
        // Listing a revocation the key did not sign is refused.
        assert!(r.drop_revocations("entity:a", &k2, &[h(1)]).is_err());
        // The honest key's row with the same statement hash stays (mutation L4).
        r.add_revocation("entity:a", "t:z", 4, k, h(3))
            .expect("honest row");
        r.drop_revocations("entity:a", &k2, &[h(3)]).expect("drop");
        assert_eq!(r.revoked_from("entity:a", "t:x"), Some(7));
        assert_eq!(r.revoked_from("entity:a", "t:z"), Some(4));

        // L14-F5, L15-F1: retiring an honest key keeps its revocations and its history.
        r.retire_entity_key("entity:a", &k, 50).expect("retire");
        assert_eq!(
            r.entity_key_state("entity:a", &k),
            Some(KeyState::Retired { at_round: 50 })
        );
        assert!(!r.entity_key_known("entity:a", &k));
        assert_eq!(r.revoked_from("entity:a", "t:x"), Some(7));
        assert!(
            r.retire_entity_key("entity:a", &k, 60).is_err(),
            "already retired"
        );
        assert!(
            r.add_entity_key("entity:a", k).is_err(),
            "keys are never re-added"
        );

        // Key states are in the root.
        let mut active = StaticRegistry::new();
        active
            .add_entity("entity:a", EntityKind::Fund)
            .and_then(|r| r.add_entity_key("entity:a", [5; 20]))
            .expect("active");
        let before = active.root();
        active
            .retire_entity_key("entity:a", &[5; 20], 1)
            .expect("retire");
        assert_ne!(active.root(), before);
    }

    /// A canon for any namespace (mutation L4).
    struct NsCanon(&'static str);
    impl AddressCanon for NsCanon {
        fn namespace(&self) -> &str {
            self.0
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[("abc", true), ("ABC", false)]
        }
        fn is_canonical(&self, body: &str) -> bool {
            !body.bytes().any(|c| c.is_ascii_uppercase())
        }
    }

    /// [`EchoProof`]'s rule for any namespace and chain (mutation L4).
    struct NsProof(&'static str, &'static str);
    impl AddressProof for NsProof {
        fn namespace(&self) -> &str {
            self.0
        }
        fn chain(&self) -> &str {
            self.1
        }
        fn proof_id(&self) -> &str {
            "onyx.proof.test.v1"
        }
        fn max_window(&self) -> u64 {
            10
        }
        fn vectors(&self) -> &'static [crate::ids::ProofVector] {
            ECHO_VECTORS
        }
        fn verify(&self, _: &str, msg: &[u8], sig: &str) -> ProofOutcome {
            if msg == sig.as_bytes() {
                ProofOutcome::Valid
            } else {
                ProofOutcome::Invalid
            }
        }
    }

    struct LowerAndEcho;
    impl RuleSet for LowerAndEcho {
        fn proof_type(id: &str) -> Option<TypeId> {
            (id == "onyx.proof.test.v1").then(TypeId::of::<EchoProof>)
        }
        fn canon_type(id: &str) -> Option<TypeId> {
            OnlyLower::canon_type(id)
        }
    }

    /// One canon per namespace, one proof per namespace and per chain, the
    /// proof found by its chain, built-in proof types, instrument namespace
    /// ids and the entity count (mutation L4).
    #[test]
    fn registry_uniqueness_and_lookups() {
        let mut r = StaticRegistry::new();
        assert_eq!(r.entity_count(), 0);
        r.add_entity("entity:a", EntityKind::Fund)
            .and_then(|r| r.add_entity("entity:b", EntityKind::Etf))
            .expect("entities");
        assert_eq!(r.entity_count(), 2);
        assert!(r.add_instrument_namespace("").is_err());
        assert!(r.add_instrument_namespace("Ticker").is_err());
        assert!(r.add_instrument_namespace("ticker").is_ok());
        r.add_canon(Box::new(LowerCanon(false))).expect("canon t");
        assert!(matches!(
            r.add_canon(Box::new(NsCanon("t"))),
            Err(RegistryError::Duplicate(_))
        ));
        r.add_canon(Box::new(NsCanon("u"))).expect("canon u");
        r.add_proof(Box::new(EchoProof {
            drift: false,
            window: 10,
        }))
        .expect("proof t on t.main");
        assert!(
            !r.rules_built_in::<LowerAndEcho>(),
            "NsCanon is not built in"
        );
        // Another namespace on the same chain, and the same namespace on
        // another chain, are both duplicates.
        assert!(matches!(
            r.add_proof(Box::new(NsProof("u", "t.main"))),
            Err(RegistryError::Duplicate(_))
        ));
        assert!(matches!(
            r.add_proof(Box::new(NsProof("t", "u.main"))),
            Err(RegistryError::Duplicate(_))
        ));
        r.add_proof(Box::new(NsProof("u", "u.main")))
            .expect("proof u on u.main");
        assert_eq!(
            r.proof_for_chain("t.main").map(|p| p.namespace()),
            Some("t")
        );
        assert_eq!(
            r.proof_for_chain("u.main").map(|p| p.namespace()),
            Some("u")
        );
        assert!(r.proof_for_chain("v.main").is_none());
        // Built-in check by type, proofs included.
        let mut b = StaticRegistry::new();
        b.add_canon(Box::new(LowerCanon(false)))
            .and_then(|b| {
                b.add_proof(Box::new(EchoProof {
                    drift: false,
                    window: 10,
                }))
            })
            .expect("built-in");
        assert!(b.rules_built_in::<LowerAndEcho>());
        assert!(!b.rules_built_in::<OnlyLower>());
    }

    /// Domain: dotted id, set once, in the root.
    #[test]
    fn domain_is_set_once() {
        let mut r = StaticRegistry::new();
        assert_eq!(r.domain(), "");
        assert!(r.set_domain("Onyx Devnet").is_err());
        let r0 = r.root();
        r.set_domain("onyx.devnet").expect("domain");
        assert_eq!(r.domain(), "onyx.devnet");
        assert_ne!(r.root(), r0);
        assert!(r.set_domain("onyx.testnet").is_err());
    }

    /// L13-F2: a rule with different vectors but the same outcome list has another root.
    #[test]
    fn root_pins_vectors_not_only_outcomes() {
        struct Lazy;
        const LAZY: &[crate::ids::ProofVector] = &[
            crate::ids::ProofVector {
                body: "zz",
                message: "m",
                signature: "m",
                expect: ProofOutcome::Valid,
            },
            crate::ids::ProofVector {
                body: "zz",
                message: "m",
                signature: "x",
                expect: ProofOutcome::Invalid,
            },
        ];
        impl AddressProof for Lazy {
            fn namespace(&self) -> &str {
                "t"
            }
            fn chain(&self) -> &str {
                "t.main"
            }
            fn proof_id(&self) -> &str {
                "onyx.proof.test.v1"
            }
            fn max_window(&self) -> u64 {
                10
            }
            fn vectors(&self) -> &'static [crate::ids::ProofVector] {
                LAZY
            }
            fn verify(&self, _: &str, msg: &[u8], sig: &str) -> ProofOutcome {
                if msg == sig.as_bytes() {
                    ProofOutcome::Valid
                } else {
                    ProofOutcome::Invalid
                }
            }
        }
        let build = |p: Box<dyn AddressProof + Send + Sync>| {
            let mut r = StaticRegistry::new();
            r.add_canon(Box::new(LowerCanon(false)))
                .and_then(|r| r.add_proof(p))
                .expect("registry");
            r.root()
        };
        let honest = build(Box::new(EchoProof {
            drift: false,
            window: 10,
        }));
        assert_ne!(build(Box::new(Lazy)), honest);
    }
}
