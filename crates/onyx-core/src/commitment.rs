//! Commitments: what a miner binds itself to, and what goes onchain when final.
//!
//! The binding hash and the commit hash use fixed 32-byte packing (not Borsh)
//! so `contracts/src/OnyxCommit.sol` recomputes them with `abi.encodePacked`.
//! Shared vectors: `fixtures/commit/vectors.json` (ADR-0011).

use crate::claim::{ClaimError, LabelSetRef};
use crate::hash::{keccak256, Hash32, RecipeId};
use crate::ids::is_dotted_id;
use crate::registry::Registry;
use crate::source::{SourceError, SourceRef};
use borsh::{BorshDeserialize, BorshSerialize};

/// Identifies the row schema (the Borsh layout of one row). Keccak-256 of the schema descriptor.
pub type SchemaId = Hash32;

/// Everything about the execution environment that can change a recipe's
/// result: pinned so "my host differs" is checkable, not an excuse (R-DRAW-1, R-DET-4).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RuntimeProfile {
    /// Recipe ABI version.
    pub abi_version: u32,
    /// Host build id (dotted id), e.g. `onyx-host.0.1.0.wasmtime.38`.
    pub host: String,
    /// Fuel limit.
    pub fuel_limit: u64,
    /// Linear memory limit, bytes.
    pub max_memory_bytes: u64,
    /// Output size limit, bytes.
    pub max_output_bytes: u32,
    /// Deterministic call-depth limit (instrumented, not native stack).
    pub max_call_depth: u32,
}

/// Smallest fuel budget a shard may pin. Below this, "correct" failure rows
/// would be the only possible output (R-SHARD-1).
pub const MIN_FUEL: u64 = 1_000_000;
/// One WebAssembly page.
pub const MIN_MEMORY_BYTES: u64 = 64 * 1024;
/// Smallest call-depth limit a shard may pin.
pub const MIN_CALL_DEPTH: u32 = 64;

impl RuntimeProfile {
    /// Sanity floor. The coordinator chooses profiles; this only stops degenerate ones.
    pub fn is_sane(&self) -> bool {
        self.abi_version >= 1
            && is_dotted_id(&self.host)
            && self.fuel_limit >= MIN_FUEL
            && self.max_memory_bytes >= MIN_MEMORY_BYTES
            && self.max_output_bytes >= 1
            && self.max_call_depth >= MIN_CALL_DEPTH
    }

    /// Keccak-256 of the canonical encoding.
    pub fn hash(&self) -> Hash32 {
        keccak256(&borsh::to_vec(self).unwrap_or_default())
    }
}

/// A batch of rows a miner commits to. Every row is a leaf of the Merkle tree
/// whose root is `root` (ADR-0003).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Commitment {
    /// Merkle root over canonically encoded rows (includes the leaf count).
    pub root: Hash32,
    /// Number of rows, at least 1.
    pub leaf_count: u64,
    /// Recipe that produced the rows. `None` for lanes that do not recompute.
    pub recipe: Option<RecipeId>,
    /// Runtime profile; present exactly when `recipe` is.
    pub runtime: Option<RuntimeProfile>,
    /// Label set version used, if the recipe consumed labels.
    pub label_set: Option<LabelSetRef>,
    /// Source snapshot the rows were computed from.
    pub source: SourceRef,
    /// Row schema.
    pub schema: SchemaId,
    /// Hash of the `DataRights` that apply to every row (R-RIGHTS-1).
    pub rights_hash: Hash32,
}

/// Commitment validation error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommitmentError {
    /// Empty batch.
    #[error("commitment has no rows")]
    Empty,
    /// Recipe and runtime must both be present or both absent.
    #[error("recipe and runtime profile must come together")]
    RuntimeMismatch,
    /// Malformed runtime profile.
    #[error("runtime profile malformed")]
    BadRuntime,
    /// Zero hash where a real one is required.
    #[error("zero hash in field {0}")]
    ZeroHash(&'static str),
    /// Bad source.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// Shard judged against a different registry state.
    #[error("shard registry root differs from the registry")]
    RegistryMismatch,
    /// Shard rights hash is not the registered rights of its source.
    #[error("shard rights hash is not the registered rights of its source")]
    RightsNotRegistered,
    /// Bad label set reference.
    #[error(transparent)]
    LabelSet(#[from] ClaimError),
}

const COMMITMENT_TAG: &[u8] = b"onyx-commitment-v1";
const COMMIT_TAG: &[u8] = b"onyx-commit-v1";
const SHARD_FIELDS_TAG: &[u8] = b"onyx-shard-fields-v1";
const SHARD_TAG: &[u8] = b"onyx-shard-v1";
const CHALLENGE_TAG: &[u8] = b"onyx-challenge-v1";

fn or_zero(h: Option<Hash32>) -> Hash32 {
    h.unwrap_or(Hash32::ZERO)
}

impl Commitment {
    /// Structural check.
    pub fn validate(&self) -> Result<(), CommitmentError> {
        if self.leaf_count == 0 {
            return Err(CommitmentError::Empty);
        }
        if self.recipe.is_some() != self.runtime.is_some() {
            return Err(CommitmentError::RuntimeMismatch);
        }
        if self.runtime.as_ref().is_some_and(|r| !r.is_sane()) {
            return Err(CommitmentError::BadRuntime);
        }
        for (name, h) in [
            ("root", self.root),
            ("schema", self.schema),
            ("rights_hash", self.rights_hash),
        ] {
            if h == Hash32::ZERO {
                return Err(CommitmentError::ZeroHash(name));
            }
        }
        if self.recipe == Some(Hash32::ZERO) {
            return Err(CommitmentError::ZeroHash("recipe"));
        }
        self.source.validate()?;
        if let Some(l) = &self.label_set {
            l.validate()?;
        }
        Ok(())
    }

    /// The fields as they appear onchain, each 32 bytes except `leaf_count`.
    pub fn fields(&self) -> CommitmentFields {
        CommitmentFields {
            root: self.root,
            leaf_count: self.leaf_count,
            recipe_id: or_zero(self.recipe),
            runtime_hash: or_zero(self.runtime.as_ref().map(RuntimeProfile::hash)),
            label_set_hash: or_zero(self.label_set.as_ref().map(LabelSetRef::hash)),
            source_hash: self.source.hash(),
            schema_id: self.schema,
            rights_hash: self.rights_hash,
        }
    }

    /// Binding hash over every field that determines the rows (R-BIND-1).
    pub fn binding_hash(&self) -> Hash32 {
        self.fields().binding_hash()
    }

    /// True if this commitment was computed with exactly the shard's assigned
    /// recipe, runtime, label set, snapshot, schema and rights (R-SHARD-1).
    /// The contract performs the same check on reveal.
    pub fn matches_shard(&self, spec: &ShardSpec) -> bool {
        self.fields().shard_fields_hash() == spec.fields_hash()
    }
}

/// What a shard must be computed with. Fixed by the coordinator before the
/// round opens (registered onchain from M6). The miner chooses none of it, so
/// it cannot pick a weak runtime, its own label set or other rights (R-SHARD-1).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ShardSpec {
    /// Recipe, or `None` for lanes that do not recompute.
    pub recipe: Option<RecipeId>,
    /// Runtime profile; present exactly when `recipe` is.
    pub runtime: Option<RuntimeProfile>,
    /// Label set version to use, if the recipe needs labels.
    pub label_set: Option<LabelSetRef>,
    /// Source snapshot to compute from.
    pub source: SourceRef,
    /// Required row schema.
    pub schema: SchemaId,
    /// Hash of the registered rights for `source`.
    pub rights_hash: Hash32,
    /// Recipe-specific parameters (e.g. the subject set), canonical bytes.
    /// Stored in the CAS before the round so anyone can recompute.
    pub params: Vec<u8>,
    /// [`Registry::root`] the round is judged against (R-REG-1).
    pub registry_root: Hash32,
}

impl ShardSpec {
    /// Structural check plus registry check: the shard must be judged against
    /// `reg`, and its rights hash must be the registered rights of its source (R-SHARD-2).
    pub fn validate(&self, reg: &dyn Registry) -> Result<(), CommitmentError> {
        if self.registry_root != reg.root() {
            return Err(CommitmentError::RegistryMismatch);
        }
        match reg.source_rights(&self.source.source) {
            Some(r) if r.hash() == self.rights_hash => {}
            _ => return Err(CommitmentError::RightsNotRegistered),
        }
        let probe = Commitment {
            root: keccak256(b"probe"),
            leaf_count: 1,
            recipe: self.recipe,
            runtime: self.runtime.clone(),
            label_set: self.label_set.clone(),
            source: self.source.clone(),
            schema: self.schema,
            rights_hash: self.rights_hash,
        };
        probe.validate()
    }

    /// Hash of the fields a commitment must match (same packing as onchain).
    pub fn fields_hash(&self) -> Hash32 {
        CommitmentFields {
            root: Hash32::ZERO,
            leaf_count: 0,
            recipe_id: or_zero(self.recipe),
            runtime_hash: or_zero(self.runtime.as_ref().map(RuntimeProfile::hash)),
            label_set_hash: or_zero(self.label_set.as_ref().map(LabelSetRef::hash)),
            source_hash: self.source.hash(),
            schema_id: self.schema,
            rights_hash: self.rights_hash,
        }
        .shard_fields_hash()
    }

    /// `keccak256("onyx-shard-v1" || fields_hash || keccak256(params) || registry_root)`.
    /// Derivable onchain from the three hashes, so the contract can check a registration.
    pub fn shard_id(&self) -> Hash32 {
        shard_id(
            &self.fields_hash(),
            &keccak256(&self.params),
            &self.registry_root,
        )
    }

    /// Identity of the work itself: everything that defines the output rows
    /// (recipe, runtime, label set, source snapshot, schema, rights, params),
    /// without the registry root. Two shards with the same work id have the
    /// same answer, so a settled work id must not be offered again under a new
    /// registry root (L20-F2).
    pub fn work_id(&self) -> Hash32 {
        let mut b = b"onyx-work-v1".to_vec();
        b.extend_from_slice(&self.fields_hash().0);
        b.extend_from_slice(&keccak256(&self.params).0);
        keccak256(&b)
    }
}

/// Ledger id of a settlement contract deployed at `contract` on chain `chain_id`
/// (ADR-0022, M6-F9): `keccak256("onyx-ledger-evm-v1" ‖ chain id u256 BE ‖ address)`.
/// The contract computes the same with `abi.encodePacked`; binding the chain id
/// means a contract at the same address on another chain is another ledger, so
/// round tags (and every commit signed over them) never replay across chains.
pub fn evm_ledger_id(chain_id: u64, contract: &MinerAddress) -> Hash32 {
    let mut b = b"onyx-ledger-evm-v1".to_vec();
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&chain_id.to_be_bytes());
    b.extend_from_slice(&word);
    b.extend_from_slice(contract);
    keccak256(&b)
}

/// `keccak256("onyx-shard-v1" || fields_hash || params_hash || registry_root)`.
pub fn shard_id(fields_hash: &Hash32, params_hash: &Hash32, registry_root: &Hash32) -> Hash32 {
    let mut b = Vec::with_capacity(SHARD_TAG.len() + 96);
    b.extend_from_slice(SHARD_TAG);
    b.extend_from_slice(&fields_hash.0);
    b.extend_from_slice(&params_hash.0);
    b.extend_from_slice(&registry_root.0);
    keccak256(&b)
}

/// Commit value for phase 1 of a challenge (R-CHAL-2, R-CHAL-3):
/// `keccak256("onyx-challenge-v1" || round:u64 BE || shard || claimed_root || defender:20 ||
/// challenger:20 || salt)`. The defender address pins which reveal is challenged.
pub fn challenge_hash(
    round: u64,
    shard: &Hash32,
    claimed_root: &Hash32,
    defender: &MinerAddress,
    challenger: &MinerAddress,
    salt: &Hash32,
) -> Hash32 {
    let mut b = Vec::with_capacity(CHALLENGE_TAG.len() + 8 + 32 * 3 + 40);
    b.extend_from_slice(CHALLENGE_TAG);
    b.extend_from_slice(&round.to_be_bytes());
    b.extend_from_slice(&shard.0);
    b.extend_from_slice(&claimed_root.0);
    b.extend_from_slice(defender);
    b.extend_from_slice(challenger);
    b.extend_from_slice(&salt.0);
    keccak256(&b)
}

/// Flattened commitment as stored onchain (mirrors `OnyxCommit.CommitmentFields`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitmentFields {
    /// Merkle root.
    pub root: Hash32,
    /// Row count.
    pub leaf_count: u64,
    /// Recipe id or zero.
    pub recipe_id: Hash32,
    /// Runtime profile hash or zero.
    pub runtime_hash: Hash32,
    /// `LabelSetRef::hash` or zero.
    pub label_set_hash: Hash32,
    /// `SourceRef::hash`.
    pub source_hash: Hash32,
    /// Row schema id.
    pub schema_id: Hash32,
    /// `DataRights::hash`.
    pub rights_hash: Hash32,
}

impl CommitmentFields {
    /// `keccak256("onyx-shard-fields-v1" || recipe_id || runtime_hash || label_set_hash ||
    /// source_hash || schema_id || rights_hash)`: every bound field except root and count.
    pub fn shard_fields_hash(&self) -> Hash32 {
        let mut b = Vec::with_capacity(SHARD_FIELDS_TAG.len() + 32 * 6);
        b.extend_from_slice(SHARD_FIELDS_TAG);
        for h in [
            self.recipe_id,
            self.runtime_hash,
            self.label_set_hash,
            self.source_hash,
            self.schema_id,
            self.rights_hash,
        ] {
            b.extend_from_slice(&h.0);
        }
        keccak256(&b)
    }

    /// `keccak256("onyx-commitment-v1" || root || leaf_count:u64 BE || recipe_id ||
    /// runtime_hash || label_set_hash || source_hash || schema_id || rights_hash)`.
    pub fn binding_hash(&self) -> Hash32 {
        let mut b = Vec::with_capacity(COMMITMENT_TAG.len() + 8 + 32 * 7);
        b.extend_from_slice(COMMITMENT_TAG);
        b.extend_from_slice(&self.root.0);
        b.extend_from_slice(&self.leaf_count.to_be_bytes());
        for h in [
            self.recipe_id,
            self.runtime_hash,
            self.label_set_hash,
            self.source_hash,
            self.schema_id,
            self.rights_hash,
        ] {
            b.extend_from_slice(&h.0);
        }
        keccak256(&b)
    }
}

/// A 20-byte EVM address identifying a miner.
pub type MinerAddress = [u8; 20];

/// The value a miner posts in the commit phase (ADR-0011):
/// `keccak256("onyx-commit-v1" || round:u64 BE || shard || binding_hash || miner || salt)`.
///
/// Round and shard stop replay across rounds and shards; the miner address
/// makes a copied commit useless to anyone else; the salt hides the root.
pub fn commit_hash(
    round: u64,
    shard: &Hash32,
    binding_hash: &Hash32,
    miner: &MinerAddress,
    salt: &Hash32,
) -> Hash32 {
    let mut b = Vec::with_capacity(COMMIT_TAG.len() + 8 + 32 * 3 + 20);
    b.extend_from_slice(COMMIT_TAG);
    b.extend_from_slice(&round.to_be_bytes());
    b.extend_from_slice(&shard.0);
    b.extend_from_slice(&binding_hash.0);
    b.extend_from_slice(miner);
    b.extend_from_slice(&salt.0);
    keccak256(&b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{ChainId, Snapshot, SourceId};

    pub(crate) fn sample(n: u8) -> Commitment {
        Commitment {
            root: keccak256(&[n, 1]),
            leaf_count: u64::from(n) + 1,
            recipe: (n != 3).then(|| keccak256(&[n, 2])),
            runtime: (n != 3).then(|| RuntimeProfile {
                abi_version: 1,
                host: "onyx-host.0.1.0".into(),
                fuel_limit: 10_000_000,
                max_memory_bytes: 64 << 20,
                max_output_bytes: 1 << 20,
                max_call_depth: 1024,
            }),
            label_set: n.is_multiple_of(2).then(|| LabelSetRef {
                name: "onyx.entities.btc".into(),
                version: u32::from(n) + 1,
                content_hash: keccak256(&[n, 3]),
            }),
            source: SourceRef {
                source: SourceId("chain.btc.mainnet".into()),
                snapshot: Snapshot::Block {
                    chain: ChainId("btc.mainnet".into()),
                    height: 900_000 + u64::from(n),
                    block_hash: keccak256(&[n, 4]),
                },
            },
            schema: keccak256(&[n, 5]),
            rights_hash: keccak256(&[n, 6]),
        }
    }

    #[test]
    fn valid_sample() {
        for n in 0..4 {
            assert_eq!(sample(n).validate(), Ok(()), "{n}");
        }
    }

    #[test]
    fn recipe_without_runtime_rejected() {
        let mut c = sample(0);
        c.runtime = None;
        assert_eq!(c.validate(), Err(CommitmentError::RuntimeMismatch));
    }

    #[test]
    fn empty_and_zero_rejected() {
        let mut c = sample(0);
        c.leaf_count = 0;
        assert_eq!(c.validate(), Err(CommitmentError::Empty));
        let mut c = sample(0);
        c.rights_hash = Hash32::ZERO;
        assert_eq!(c.validate(), Err(CommitmentError::ZeroHash("rights_hash")));
    }

    /// R-BIND-1: changing any binding field changes the binding hash.
    #[test]
    fn every_field_is_bound() {
        let base = sample(0);
        let h = base.binding_hash();
        let mut variants: Vec<Commitment> = Vec::new();
        let mut c = base.clone();
        c.root = keccak256(b"x");
        variants.push(c);
        let mut c = base.clone();
        c.leaf_count += 1;
        variants.push(c);
        let mut c = base.clone();
        c.recipe = Some(keccak256(b"x"));
        variants.push(c);
        let mut c = base.clone();
        if let Some(r) = c.runtime.as_mut() {
            r.fuel_limit += 1;
        }
        variants.push(c);
        let mut c = base.clone();
        if let Some(l) = c.label_set.as_mut() {
            l.version += 1;
        }
        variants.push(c);
        let mut c = base.clone();
        c.label_set = None;
        variants.push(c);
        let mut c = base.clone();
        if let Snapshot::Block { height, .. } = &mut c.source.snapshot {
            *height += 1;
        }
        variants.push(c);
        let mut c = base.clone();
        c.schema = keccak256(b"x");
        variants.push(c);
        let mut c = base.clone();
        c.rights_hash = keccak256(b"x");
        variants.push(c);
        for v in variants {
            assert_ne!(v.binding_hash(), h, "{v:?}");
        }
    }

    /// R-COMMIT-2: round, shard, miner and salt are all bound.
    #[test]
    fn commit_hash_binds_context() {
        let b = sample(0).binding_hash();
        let shard = keccak256(b"shard");
        let m = [7u8; 20];
        let s = keccak256(b"salt");
        let h = commit_hash(1, &shard, &b, &m, &s);
        assert_ne!(h, commit_hash(2, &shard, &b, &m, &s));
        assert_ne!(h, commit_hash(1, &keccak256(b"other"), &b, &m, &s));
        assert_ne!(h, commit_hash(1, &shard, &b, &[8u8; 20], &s));
        assert_ne!(h, commit_hash(1, &shard, &b, &m, &keccak256(b"salt2")));
    }

    /// Shared vector with `contracts/test/OnyxCommit.t.sol`. Case 3 has no
    /// recipe or runtime (zero sentinels). Regenerate with `ONYX_BLESS=1 cargo test -p onyx-core`.
    #[test]
    fn solidity_commit_vectors_match() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/commit/vectors.json"
        );
        let hx = |h: Hash32| format!("0x{}", h.to_hex());
        let cases: Vec<serde_json::Value> = (0u8..4)
            .map(|n| {
                let c = sample(n);
                let f = c.fields();
                let round = 1000 + u64::from(n);
                let shard = keccak256(&[n, 9]);
                let miner: [u8; 20] = std::array::from_fn(|k| (k as u8).wrapping_mul(13) ^ n);
                let salt = keccak256(&[n, 10]);
                let params_hash = keccak256(&[n, 11]);
                let registry_root = keccak256(&[n, 12]);
                let claimed_root = keccak256(&[n, 13]);
                let challenger: [u8; 20] =
                    std::array::from_fn(|k| (k as u8).wrapping_mul(29).wrapping_add(7) ^ n);
                let challenge_salt = keccak256(&[n, 14]);
                serde_json::json!({
                    "bindingHash": hx(c.binding_hash()),
                    "commitHash": hx(commit_hash(round, &shard, &c.binding_hash(), &miner, &salt)),
                    "labelSetHash": hx(f.label_set_hash),
                    "leafCount": f.leaf_count,
                    "miner": format!("0x{}", hex::encode(miner)),
                    "paramsHash": hx(params_hash),
                    "recipeId": hx(f.recipe_id),
                    "rightsHash": hx(f.rights_hash),
                    "root": hx(f.root),
                    "round": round,
                    "runtimeHash": hx(f.runtime_hash),
                    "salt": hx(salt),
                    "schemaId": hx(f.schema_id),
                    "shard": hx(shard),
                    "shardFieldsHash": hx(f.shard_fields_hash()),
                    "registryRoot": hx(registry_root),
                    "shardId": hx(shard_id(&f.shard_fields_hash(), &params_hash, &registry_root)),
                    "claimedRoot": hx(claimed_root),
                    "defender": format!("0x{}", hex::encode(miner)),
                    "challenger": format!("0x{}", hex::encode(challenger)),
                    "challengeSalt": hx(challenge_salt),
                    "challengeHash": hx(challenge_hash(round, &shard, &claimed_root, &miner, &challenger, &challenge_salt)),
                    "sourceHash": hx(f.source_hash),
                })
            })
            .collect();
        let doc = serde_json::json!({ "count": cases.len(), "cases": cases });
        let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(path).parent().expect("parent"))
                .expect("mkdir");
            std::fs::write(path, &text).expect("write vectors");
        }
        let on_disk = std::fs::read_to_string(path)
            .expect("fixtures/commit/vectors.json missing; run with ONYX_BLESS=1");
        assert_eq!(on_disk, text, "commit vectors drifted from ADR-0011");
    }

    fn spec_for(c: &Commitment) -> ShardSpec {
        ShardSpec {
            recipe: c.recipe,
            runtime: c.runtime.clone(),
            label_set: c.label_set.clone(),
            source: c.source.clone(),
            schema: c.schema,
            rights_hash: c.rights_hash,
            params: vec![1, 2, 3],
            registry_root: keccak256(b"registry"),
        }
    }

    /// R-SHARD-1: a commitment must use exactly the shard's assigned fields.
    #[test]
    fn commitment_must_match_shard_spec() {
        let c = sample(0);
        let spec = spec_for(&c);
        assert!(c.matches_shard(&spec));
        // Miner swaps in its own label set.
        let mut own_labels = c.clone();
        if let Some(l) = own_labels.label_set.as_mut() {
            l.content_hash = keccak256(b"miner-authored labels");
        }
        assert!(!own_labels.matches_shard(&spec));
        // Miner pins a weaker runtime.
        let mut weak = c.clone();
        if let Some(r) = weak.runtime.as_mut() {
            r.fuel_limit = MIN_FUEL;
        }
        assert!(!weak.matches_shard(&spec));
        // Miner drops the recipe to escape recompute.
        let mut no_recipe = c.clone();
        no_recipe.recipe = None;
        no_recipe.runtime = None;
        assert!(!no_recipe.matches_shard(&spec));
        // Root and count are the miner's work and do not affect the match.
        let mut other_root = c.clone();
        other_root.root = keccak256(b"other");
        other_root.leaf_count += 5;
        assert!(other_root.matches_shard(&spec));
    }

    #[test]
    fn degenerate_runtime_rejected() {
        let mut c = sample(0);
        if let Some(r) = c.runtime.as_mut() {
            r.fuel_limit = 1;
        }
        assert_eq!(c.validate(), Err(CommitmentError::BadRuntime));
        let mut c = sample(0);
        if let Some(r) = c.runtime.as_mut() {
            r.max_call_depth = 0;
        }
        assert_eq!(c.validate(), Err(CommitmentError::BadRuntime));
        // The memory floor is one WebAssembly page, 65,536 bytes (mutation L4).
        assert_eq!(MIN_MEMORY_BYTES, 65_536);
        let mut c = sample(0);
        if let Some(r) = c.runtime.as_mut() {
            r.max_memory_bytes = 65_536;
        }
        assert_eq!(c.validate(), Ok(()));
        if let Some(r) = c.runtime.as_mut() {
            r.max_memory_bytes = 65_535;
        }
        assert_eq!(c.validate(), Err(CommitmentError::BadRuntime));
    }

    /// R-SHARD-2, R-REG-1.
    #[test]
    fn shard_spec_checked_against_registry() {
        use crate::registry::StaticRegistry;
        use crate::rights::DataRights;
        let mut reg = StaticRegistry::new();
        let rights = DataRights::public_chain(SourceId("chain.btc.mainnet".into()));
        reg.add_source(rights.clone()).expect("source");
        let mut c = sample(0);
        c.rights_hash = rights.hash();
        let mut spec = spec_for(&c);
        spec.registry_root = reg.root();
        assert_eq!(spec.validate(&reg), Ok(()));
        let mut stale = spec.clone();
        stale.registry_root = keccak256(b"old registry");
        assert_eq!(stale.validate(&reg), Err(CommitmentError::RegistryMismatch));
        let mut wrong_rights = spec.clone();
        wrong_rights.rights_hash = keccak256(b"public-looking rights for a vendor source");
        assert_eq!(
            wrong_rights.validate(&reg),
            Err(CommitmentError::RightsNotRegistered)
        );
        let mut other_root = spec;
        other_root.registry_root = keccak256(b"x");
        assert_ne!(other_root.shard_id(), spec_for(&c).shard_id());
    }

    #[test]
    fn challenge_hash_binds_defender_and_challenger() {
        let s = keccak256(b"shard");
        let root = keccak256(b"root");
        let salt = keccak256(b"salt");
        let h = challenge_hash(1, &s, &root, &[1; 20], &[2; 20], &salt);
        assert_ne!(h, challenge_hash(1, &s, &root, &[3; 20], &[2; 20], &salt));
        assert_ne!(h, challenge_hash(1, &s, &root, &[1; 20], &[3; 20], &salt));
        assert_ne!(
            h,
            challenge_hash(1, &s, &keccak256(b"r2"), &[1; 20], &[2; 20], &salt)
        );
    }

    #[test]
    fn shard_id_binds_params() {
        let spec = spec_for(&sample(0));
        let mut other = spec.clone();
        other.params = vec![9];
        assert_ne!(spec.shard_id(), other.shard_id());
    }
}
