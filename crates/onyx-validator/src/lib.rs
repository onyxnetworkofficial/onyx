//! Validator client (ADR-0019, M4): for each assigned reveal, recompute the
//! shard from its pinned inputs with the same code miners and settlement use
//! (`onyx_coordinator::recompute`), and commit, then reveal, a signed verdict.

use onyx_coordinator::cas::Cas;
use onyx_coordinator::dispute::{
    appeal_message, challenge_message, panel_commit_hash, panel_vote_message, ProofLevel,
    SignedAppeal, SignedChallenge, SignedPanelVote,
};
use onyx_coordinator::recompute::{classify, compute, Reference};
use onyx_coordinator::validate::{verdict_commit_hash, verdict_message, SignedVerdict, Verdict};
use onyx_core::{keccak256, Hash32, MinerAddress, Registry, ShardSpec};
use onyx_labels::attest::{eip191_digest, AttestCache};
use onyx_recipe_host::Host;

/// A validator identity: a secp256k1 key (test keys are derived from fixed labels).
pub struct Validator {
    /// Address (EVM-style, from the key).
    pub address: MinerAddress,
    key: secp256k1::SecretKey,
    secret: Hash32,
    cache: AttestCache,
}

/// Validator error.
#[derive(Debug, thiserror::Error)]
pub enum ValidatorError {
    /// Key is not a valid secp256k1 scalar.
    #[error("bad validator key")]
    BadKey,
}

impl Validator {
    /// Validator from a 32-byte secret key.
    pub fn new(secret_key: [u8; 32]) -> Result<Self, ValidatorError> {
        let key =
            secp256k1::SecretKey::from_slice(&secret_key).map_err(|_| ValidatorError::BadKey)?;
        let pk = key
            .public_key(&secp256k1::Secp256k1::new())
            .serialize_uncompressed();
        let mut address = [0u8; 20];
        address.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
        let mut seed = b"onyx-verdict-salt-key-v1".to_vec();
        seed.extend_from_slice(&secret_key);
        Ok(Validator {
            address,
            key,
            secret: keccak256(&seed),
            cache: AttestCache::new(),
        })
    }

    /// Recompute a shard and judge a revealed root. Missing data or a machine
    /// failure is `Abstain`, never a guess. A shard no root can be right for
    /// is `NoValidRoot`, which disputes the reveal (L25-F1).
    pub fn judge(
        &self,
        spec: &ShardSpec,
        source: Vec<u8>,
        revealed_root: &Hash32,
        reg: &dyn Registry,
        cas: &dyn Cas,
        host: &Host,
    ) -> Verdict {
        // Same classification settlement uses (loop 25): a deterministic
        // recipe failure has a root too; anything undecided is an Abstain.
        match classify(compute(spec, source, reg, cas, host, &self.cache)) {
            Reference::Root(root) if root == *revealed_root => Verdict::Agree,
            Reference::Root(root) => Verdict::Disagree { root },
            Reference::NoValidRoot => Verdict::NoValidRoot,
            Reference::Undecided(_) => Verdict::Abstain,
        }
    }

    /// Salt for a verdict (recomputable after a restart, unpredictable to others).
    pub fn salt(&self, seed: &Hash32, shard: &Hash32, miner: &MinerAddress) -> Hash32 {
        let mut b = b"onyx-verdict-salt-v1".to_vec();
        b.extend_from_slice(&self.secret.0);
        b.extend_from_slice(&seed.0);
        b.extend_from_slice(&shard.0);
        b.extend_from_slice(miner);
        keccak256(&b)
    }

    /// EIP-191 signature `r ‖ s ‖ v` over `msg` with this key.
    pub fn sign(&self, msg: &str) -> [u8; 65] {
        let digest = secp256k1::Message::from_digest(eip191_digest(msg.as_bytes()).0);
        let (recid, rs) = secp256k1::Secp256k1::new()
            .sign_ecdsa_recoverable(&digest, &self.key)
            .serialize_compact();
        let mut sig = [0u8; 65];
        sig[..64].copy_from_slice(&rs);
        sig[64] = 27 + u8::try_from(recid.to_i32()).unwrap_or(0);
        sig
    }

    /// A signed challenge against shard `shard` of the round tagged `tag`
    /// (M5, ADR-0021): the challenger claims `claimed_root` and posts `bond`.
    pub fn challenge(
        &self,
        round: u64,
        tag: &Hash32,
        shard: &Hash32,
        claimed_root: &Hash32,
        bond: u128,
    ) -> SignedChallenge {
        let level = ProofLevel::Recomputed;
        SignedChallenge {
            round,
            tag: *tag,
            shard: *shard,
            challenger: self.address,
            claimed_root: *claimed_root,
            bond,
            level,
            sig: self.sign(&challenge_message(tag, shard, claimed_root, bond, level)),
        }
    }

    /// Salt for a panel vote (recomputable after a restart).
    pub fn panel_salt(&self, dispute: &Hash32, level: u32) -> Hash32 {
        let mut b = b"onyx-panel-salt-v1".to_vec();
        b.extend_from_slice(&self.secret.0);
        b.extend_from_slice(&dispute.0);
        b.extend_from_slice(&level.to_be_bytes());
        keccak256(&b)
    }

    /// Signed commit for a panel vote: the root this member got by rerunning.
    pub fn panel_vote(&self, dispute: &Hash32, level: u32, root: &Hash32) -> SignedPanelVote {
        let salt = self.panel_salt(dispute, level);
        let commit = panel_commit_hash(dispute, level, &self.address, root, &salt);
        SignedPanelVote {
            dispute: *dispute,
            level,
            member: self.address,
            commit,
            sig: self.sign(&panel_vote_message(dispute, level, &commit)),
        }
    }

    /// A signed appeal against panel `level`, backing `root`.
    pub fn appeal(&self, dispute: &Hash32, level: u32, root: &Hash32, bond: u128) -> SignedAppeal {
        SignedAppeal {
            dispute: *dispute,
            level,
            appellant: self.address,
            root: *root,
            bond,
            sig: self.sign(&appeal_message(dispute, level, root, bond)),
        }
    }

    /// Signed commit for a verdict, bound to the validation's `seed`.
    pub fn commit(
        &self,
        seed: &Hash32,
        shard: &Hash32,
        miner: &MinerAddress,
        verdict: &Verdict,
    ) -> SignedVerdict {
        let salt = self.salt(seed, shard, miner);
        let commit = verdict_commit_hash(seed, &self.address, shard, miner, verdict, &salt);
        let sig = self.sign(&verdict_message(seed, shard, miner, &commit));
        SignedVerdict {
            validator: self.address,
            shard: *shard,
            miner: *miner,
            commit,
            sig,
        }
    }
}
