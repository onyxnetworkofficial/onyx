//! Miner client (ADR-0018): fetch everything a shard pins from the CAS, run
//! the recipe on the canonical engine, and produce a signed commit. The rows
//! stay private until the reveal phase ([`Miner::publish_rows`]): published
//! earlier, anyone reading the store could copy the answer (L18-F1).

use onyx_coordinator::cas::{Cas, CasError, CasExt};
use onyx_coordinator::recompute::{self, ComputeError, FailureAllowList};
use onyx_coordinator::round::{commit_message, SignedCommit};
use onyx_core::{commit_hash, keccak256, Commitment, Hash32, MinerAddress, Registry, ShardSpec};
use onyx_labels::attest::AttestCache;
use onyx_recipe_host::{failure_leaf, Host, HostError, Outcome};

/// Miner error.
#[derive(Debug, thiserror::Error)]
pub enum MinerError {
    /// The shard has no recipe (not a recompute lane).
    #[error("shard has no recipe")]
    NoRecipe,
    /// Something the shard pins could not be fetched.
    #[error("not in the CAS: {0}")]
    Unavailable(String),
    /// Host refused the input or the recipe.
    #[error("host: {0}")]
    Host(#[from] HostError),
    /// The recipe ran but failed. [`compute`] reports it; [`Miner::work`]
    /// commits the canonical failure leaf instead, as validators and settlement
    /// classify it (`recompute::classify`, SER-R1-F1).
    #[error("recipe outcome {0:?}")]
    Failed(Outcome),
    /// The rows would not fit in the store; committing would forfeit the bond.
    #[error("rows too large to store")]
    RowsTooLarge,
    /// The secret key is not a valid secp256k1 scalar.
    #[error("bad miner key")]
    BadKey,
    /// Storing the rows failed.
    #[error("cas: {0}")]
    Cas(#[from] CasError),
}

/// A finished shard, ready to commit and later reveal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Work {
    /// Round number.
    pub round: u64,
    /// Shard id.
    pub shard: Hash32,
    /// The full commitment (revealed later).
    pub commitment: Commitment,
    /// Salt (revealed later).
    pub salt: Hash32,
    /// What to post in the commit phase.
    pub commit_hash: Hash32,
    /// Output rows; published to the CAS only when reveals open.
    pub rows: Vec<Vec<u8>>,
}

/// Run a shard from its pinned inputs ([`onyx_coordinator::recompute::compute`]):
/// fetch the recipe, label set and cited attestations from the CAS by hash,
/// assemble the input, run the canonical engine. Miners, validators and
/// settlement call exactly this, so an honest validator gets the honest
/// miner's rows (M4).
pub fn compute(
    spec: &ShardSpec,
    source: Vec<u8>,
    reg: &dyn Registry,
    cas: &dyn Cas,
    host: &Host,
    cache: &AttestCache,
) -> Result<Vec<Vec<u8>>, MinerError> {
    recompute::compute(spec, source, reg, cas, host, cache).map_err(|e| match e {
        ComputeError::NoRecipe => MinerError::NoRecipe,
        ComputeError::Unavailable(s) => MinerError::Unavailable(s),
        ComputeError::Host(h) => MinerError::Host(h),
        ComputeError::Failed(o) => MinerError::Failed(o),
        ComputeError::RowsTooLarge => MinerError::RowsTooLarge,
    })
}

/// A miner identity: a secp256k1 key. Its EVM-style address is bound into
/// every commit hash and every commit is signed with it (L18-F2). The key also
/// derives salts; it is never revealed.
pub struct Miner {
    /// Address bound into every commit hash.
    pub address: MinerAddress,
    key: secp256k1::SecretKey,
    secret: Hash32,
    cache: AttestCache,
    allowed: FailureAllowList,
}

impl Miner {
    /// Miner from a 32-byte secret key. Test and demo keys are derived from
    /// fixed labels and hold nothing.
    pub fn new(secret_key: [u8; 32]) -> Result<Self, MinerError> {
        let key = secp256k1::SecretKey::from_slice(&secret_key).map_err(|_| MinerError::BadKey)?;
        let pk = key
            .public_key(&secp256k1::Secp256k1::new())
            .serialize_uncompressed();
        let mut address = [0u8; 20];
        address.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
        let mut salt_seed = b"onyx-salt-key-v1".to_vec();
        salt_seed.extend_from_slice(&secret_key);
        Ok(Miner {
            address,
            key,
            secret: keccak256(&salt_seed),
            cache: AttestCache::new(),
            allowed: FailureAllowList::new(),
        })
    }

    /// Commit the canonical failure leaf for these failures only (SER-R2-F2):
    /// a failure a schema defines as a valid result, such as a series range
    /// with no activity. Any other failure (out of fuel, a trap, a recipe
    /// error not on the list) is not committed: the miner abstains, because
    /// the shard was built wrong (SER-R2-F1). The default list is empty.
    pub fn with_allowed_failures(mut self, allowed: FailureAllowList) -> Self {
        self.allowed = allowed;
        self
    }

    /// Sign a commit for posting in the round tagged `round_tag`
    /// (`Round::tag`, `round::commit_message`, EIP-191).
    pub fn sign_commit(&self, work: &Work, bond: u128, round_tag: &Hash32) -> SignedCommit {
        let msg = commit_message(round_tag, &work.shard, &work.commit_hash, bond);
        let digest =
            secp256k1::Message::from_digest(onyx_labels::attest::eip191_digest(msg.as_bytes()).0);
        let (recid, rs) = secp256k1::Secp256k1::new()
            .sign_ecdsa_recoverable(&digest, &self.key)
            .serialize_compact();
        let mut sig = [0u8; 65];
        sig[..64].copy_from_slice(&rs);
        sig[64] = 27 + u8::try_from(recid.to_i32()).unwrap_or(0);
        SignedCommit {
            miner: self.address,
            shard: work.shard,
            commit_hash: work.commit_hash,
            bond,
            sig,
        }
    }

    /// Publish the rows behind a commitment. Call only once reveals are open.
    pub fn publish_rows(&self, work: &Work, cas: &dyn Cas) -> Result<Hash32, MinerError> {
        Ok(cas.put_rows(&work.rows)?)
    }

    /// Salt for (round, shard): `keccak256("onyx-salt-v1" ‖ secret ‖ round ‖ shard)`.
    /// Unpredictable to others, recomputable by the miner after a restart.
    pub fn salt(&self, round: u64, shard: &Hash32) -> Hash32 {
        let mut b = b"onyx-salt-v1".to_vec();
        b.extend_from_slice(&self.secret.0);
        b.extend_from_slice(&round.to_be_bytes());
        b.extend_from_slice(&shard.0);
        keccak256(&b)
    }

    /// Compute a shard: fetch the recipe, label set and cited attestations by
    /// hash, run, and build the commitment and commit hash. The rows are kept
    /// in the returned [`Work`] and published at reveal ([`Miner::publish_rows`]);
    /// a result too large to store is refused here, before any commit (L19-F1).
    /// `source` is the snapshot's bytes from the miner's own source (node or fixture).
    /// A deterministic recipe failure the miner's allow-list names for the
    /// shard's schema (e.g. a series range with no activity,
    /// `SERIES_NO_ACTIVITY`, [`Miner::with_allowed_failures`]) commits the
    /// canonical failure leaf: every honest machine reaches the same root, and
    /// validators agree with it (SER-R1-F1). Any other failure, out of fuel
    /// included, is returned as [`MinerError::Failed`] and nothing is
    /// committed (SER-R2-F1, F2).
    pub fn work(
        &self,
        round: u64,
        spec: &ShardSpec,
        source: Vec<u8>,
        reg: &dyn Registry,
        cas: &dyn Cas,
        host: &Host,
    ) -> Result<Work, MinerError> {
        let rows = match compute(spec, source, reg, cas, host, &self.cache) {
            Err(MinerError::Failed(o)) if self.allowed.allows(&spec.schema, &o) => {
                vec![failure_leaf(o)]
            }
            r => r?,
        };
        let root = onyx_merkle::MerkleTree::from_rows(&rows).root();
        let shard = spec.shard_id();
        let commitment = Commitment {
            root,
            leaf_count: rows.len() as u64,
            recipe: spec.recipe,
            runtime: spec.runtime.clone(),
            label_set: spec.label_set.clone(),
            source: spec.source.clone(),
            schema: spec.schema,
            rights_hash: spec.rights_hash,
        };
        let salt = self.salt(round, &shard);
        let commit_hash = commit_hash(
            round,
            &shard,
            &commitment.binding_hash(),
            &self.address,
            &salt,
        );
        Ok(Work {
            round,
            shard,
            commitment,
            salt,
            commit_hash,
            rows,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salts_differ_by_round_shard_and_secret() {
        let m = Miner::new(keccak256(b"secret").0).expect("key");
        let s1 = keccak256(b"shard 1");
        let s2 = keccak256(b"shard 2");
        assert_ne!(m.salt(1, &s1), m.salt(2, &s1));
        assert_ne!(m.salt(1, &s1), m.salt(1, &s2));
        assert_eq!(m.salt(1, &s1), m.salt(1, &s1));
        let other = Miner::new(keccak256(b"other").0).expect("key");
        assert_ne!(m.salt(1, &s1), other.salt(1, &s1));
    }
}
