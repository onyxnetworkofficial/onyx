//! Unit-test world for rounds and validation (ROADMAP E8, THREATS MUT-1).
//!
//! `round.rs` and `validate.rs` were tested only end to end from onyx-source-btc
//! (real blocks, the real recipe, about two minutes a run), so mutation testing
//! of the coordinator alone left most of their mutants alive. This fixture
//! opens real rounds in a few milliseconds: a registry with one public-chain
//! source and no address rules (so any rule set counts as built in), an
//! in-memory CAS, and a WAT recipe whose one output row does not depend on its
//! input, so the rerun reference is known before any test runs.

use crate::cas::{Cas, CasExt, Kind, MemCas};
use crate::dispute::DisputeConfig;
use crate::recompute::{compute, FailureAllowList, RecomputeEnv};
use crate::round::{commit_message, Round, RoundConfig, RoundError, SignedCommit};
use crate::validate::{
    verdict_commit_hash, verdict_message, Ledger, SignedVerdict, Validation, ValidationConfig,
    ValidationError, ValidatorSet, Verdict,
};
use onyx_core::{
    commit_hash, keccak256, ChainId, Commitment, DataRights, Hash32, MinerAddress, Registry,
    RuleSet, RuntimeProfile, ShardSpec, Snapshot, SourceId, SourceRef, StaticRegistry,
};
use onyx_labels::attest::{eip191_digest, AttestCache};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{Host, HOST_ID};
use secp256k1::{Message, Secp256k1, SecretKey};
use std::any::TypeId;

/// A rule set with no address rules; the fixture registry has none either,
/// so reruns are allowed (`rules_built_in`).
pub(crate) struct Rules;
impl RuleSet for Rules {
    fn proof_type(_: &str) -> Option<TypeId> {
        None
    }
    fn canon_type(_: &str) -> Option<TypeId> {
        None
    }
}

/// The round id [`Closed`] opens (not 1, so a constant 1 is no right answer).
pub(crate) const ROUND: u64 = 3;

/// The source bytes every fixture shard pins (by hash).
pub(crate) const SOURCE: &[u8] = b"onyx fixture source bytes";
/// The fixture source id.
pub(crate) const SOURCE_ID: &str = "chain.t.main";

/// The recipe ABI around a run body (as in onyx-recipe-host's tests): the
/// data at 2048 is `RunReturn::Ok` with one row (key `[1]`, value `[2]`).
fn module(run_body: &str, extra: &str) -> Vec<u8> {
    let text = format!(
        r#"(module
          (memory (export "memory") 1)
          (data (i32.const 2048) "\00\01\00\00\00\01\00\00\00\01\01\00\00\00\02")
          (func (export "onyx_abi_version") (result i32) i32.const 1)
          (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
          (func (export "onyx_run") (param i32 i32) (result i64) {run_body})
          {extra})"#
    );
    wat::parse_str(text).expect("wat")
}

/// A recipe that returns the same one row whatever its input.
pub(crate) fn recipe_ok() -> Vec<u8> {
    module("i64.const 8796093022223", "") // (2048 << 32) | 15
}

/// A recipe the canonical engine refuses to load (a float instruction).
pub(crate) fn recipe_float() -> Vec<u8> {
    module(
        "i64.const 8796093022223",
        "(func (param f64) (result f64) local.get 0 local.get 0 f64.add)",
    )
}

/// A recipe that never ends (runs out of fuel: the preflight refuses it).
pub(crate) fn recipe_loop() -> Vec<u8> {
    module("(loop br 0) i64.const 0", "")
}

/// The runtime profile every fixture shard pins.
pub(crate) fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 10_000_000,
        max_memory_bytes: 16 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 64,
    }
}

/// Round timing and economics used by the unit tests (the same shape as the
/// end-to-end tests' config). `validator_set_root` and `canary_commit` are
/// filled per round.
pub(crate) const CFG: RoundConfig = RoundConfig {
    commit_close: 100,
    reveal_close: 200,
    min_bond: 1_000,
    validator_set_root: Hash32([0; 32]),
    canary_commit: Hash32([0; 32]),
    beacon_height: 201,
    validation: ValidationConfig {
        verdict_commit_close: 300,
        verdict_reveal_close: 400,
        canary_open_close: 450,
        pending_close: 600,
        rate_ppm: 1_000_000,
        miner_reward: 500,
        validator_fee: 10,
        validator_slash: 2_000,
        missed_slash: 100,
        min_canary_ppm: 50_000,
    },
    dispute: DisputeConfig {
        challenge_window: 200,
        min_challenge_bond: 1_000,
        panel_size: 3,
        panel_commit_window: 50,
        panel_reveal_window: 50,
        appeal_window: 50,
        max_appeals: 1,
        panel_fee: 10,
        panel_slash: 500,
    },
};

/// A key and its address.
pub(crate) struct Actor {
    sk: SecretKey,
    /// Its EVM address.
    pub addr: MinerAddress,
    label: String,
}

impl Actor {
    /// The actor named `label` (key `keccak256(label)`).
    pub(crate) fn new(label: &str) -> Self {
        let sk = SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("test key");
        let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), &sk);
        let h = keccak256(&pk.serialize_uncompressed()[1..]);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&h.0[12..]);
        Actor {
            sk,
            addr,
            label: label.into(),
        }
    }

    /// EIP-191 signature `r ‖ s ‖ v` over `msg`.
    pub(crate) fn sign(&self, msg: &[u8]) -> [u8; 65] {
        let digest = Message::from_digest(eip191_digest(msg).0);
        let (rid, bytes) = Secp256k1::new()
            .sign_ecdsa_recoverable(&digest, &self.sk)
            .serialize_compact();
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&bytes);
        out[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
        out
    }

    /// This actor's salt for `what`.
    pub(crate) fn salt(&self, what: &Hash32) -> Hash32 {
        keccak256(&[self.label.as_bytes(), &what.0].concat())
    }
}

/// `n` actors named `{prefix}{i}`.
pub(crate) fn actors(prefix: &str, n: usize) -> Vec<Actor> {
    (0..n)
        .map(|i| Actor::new(&format!("{prefix}{i}")))
        .collect()
}

/// A miner's hidden answer for one shard.
#[derive(Clone, Debug)]
pub(crate) struct Work {
    /// The revealed commitment.
    pub commitment: Commitment,
    /// Its salt.
    pub salt: Hash32,
    /// `commit_hash` over it.
    pub commit_hash: Hash32,
}

/// Registry, store and engine.
pub(crate) struct World {
    /// One public-chain source, no address rules.
    pub reg: StaticRegistry,
    /// Recipes, params and row batches.
    pub cas: MemCas,
    /// The canonical engine.
    pub host: Host,
}

/// An address canon no fixture rule set names: a registry holding it has
/// rules that are not built in for [`Rules`] (L25-F2).
pub(crate) struct ForeignCanon;
impl onyx_core::AddressCanon for ForeignCanon {
    fn namespace(&self) -> &str {
        "t"
    }
    fn canon_id(&self) -> &str {
        "onyx.canon.test.v1"
    }
    fn vectors(&self) -> &'static [(&'static str, bool)] {
        &[("a", true), ("A", false)]
    }
    fn is_canonical(&self, body: &str) -> bool {
        !body.bytes().any(|c| c.is_ascii_uppercase())
    }
}

impl World {
    /// The fixture world: every recipe stored, no address rules.
    pub(crate) fn new() -> Self {
        Self::build(false)
    }

    /// The fixture world with [`ForeignCanon`] in its registry.
    pub(crate) fn with_foreign_canon() -> Self {
        Self::build(true)
    }

    fn build(foreign_canon: bool) -> Self {
        let mut reg = StaticRegistry::new();
        reg.set_domain("onyx.devnet")
            .and_then(|r| r.add_source(DataRights::public_chain(SourceId(SOURCE_ID.into()))))
            .expect("registry");
        if foreign_canon {
            reg.add_canon(Box::new(ForeignCanon)).expect("canon");
        }
        let cas = MemCas::new();
        cas.put(Kind::Recipe, &recipe_ok()).expect("recipe");
        cas.put(Kind::Recipe, &recipe_float()).expect("recipe");
        cas.put(Kind::Recipe, &recipe_loop()).expect("recipe");
        World {
            reg,
            cas,
            host: Host::new().expect("host"),
        }
    }

    /// Shard `n` (its params are `[n]`, so each `n` is other work) run by `wasm`.
    pub(crate) fn spec_with(&self, wasm: &[u8], n: u8) -> ShardSpec {
        let params = vec![n];
        self.cas.put(Kind::Blob, &params).expect("params");
        let source = SourceId(SOURCE_ID.into());
        ShardSpec {
            recipe: Some(keccak256(wasm)),
            runtime: Some(profile()),
            label_set: None,
            rights_hash: self.reg.source_rights(&source).expect("rights").hash(),
            source: SourceRef {
                source,
                snapshot: Snapshot::BlockRange {
                    chain: ChainId("t.main".into()),
                    from_height: 1,
                    to_height: 2,
                    tip_hash: keccak256(b"tip"),
                    content_hash: keccak256(SOURCE),
                },
            },
            schema: keccak256(b"onyx.test.row.v1"),
            params,
            registry_root: self.reg.root(),
        }
    }

    /// Shard `n` run by [`recipe_ok`].
    pub(crate) fn spec(&self, n: u8) -> ShardSpec {
        self.spec_with(&recipe_ok(), n)
    }

    /// A rerun environment whose source is `source`.
    pub(crate) fn env<'a>(
        &'a self,
        source: &'a dyn Fn(&ShardSpec) -> Option<Vec<u8>>,
    ) -> RecomputeEnv<'a> {
        RecomputeEnv {
            reg: &self.reg,
            cas: &self.cas,
            host: &self.host,
            source,
        }
    }

    /// Open a round the public way (with its preflight run).
    pub(crate) fn open<R: RuleSet>(
        &self,
        id: u64,
        specs: Vec<ShardSpec>,
        cfg: RoundConfig,
        now: u64,
        ledger: &mut Ledger<R>,
    ) -> Result<Round, RoundError> {
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        let env = self.env(&src);
        Round::open_preflighted(id, specs, &env, cfg, now, ledger, &FailureAllowList::new())
    }

    /// The honest rows of a fixture shard (the rerun's).
    pub(crate) fn honest_rows(&self, spec: &ShardSpec) -> Vec<Vec<u8>> {
        compute(
            spec,
            SOURCE.to_vec(),
            &self.reg,
            &self.cas,
            &self.host,
            &AttestCache::new(),
        )
        .expect("honest run")
    }

    /// The honest root of a fixture shard.
    pub(crate) fn honest_root(&self, spec: &ShardSpec) -> Hash32 {
        MerkleTree::from_rows(&self.honest_rows(spec)).root()
    }

    /// `miner`'s work on `spec` in round `round` with `rows` (stored).
    pub(crate) fn work(
        &self,
        spec: &ShardSpec,
        round: u64,
        miner: &Actor,
        rows: &[Vec<u8>],
    ) -> Work {
        let root = self.cas.put_rows(rows).expect("rows");
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
        let shard = spec.shard_id();
        let salt = miner.salt(&shard);
        let commit_hash = commit_hash(
            round,
            &shard,
            &commitment.binding_hash(),
            &miner.addr,
            &salt,
        );
        Work {
            commitment,
            salt,
            commit_hash,
        }
    }

    /// The honest work of `miner`.
    pub(crate) fn honest(&self, spec: &ShardSpec, round: u64, miner: &Actor) -> Work {
        let rows = self.honest_rows(spec);
        self.work(spec, round, miner, &rows)
    }

    /// Work with one invented row `text` (padded to a storable row).
    pub(crate) fn wrong(&self, spec: &ShardSpec, round: u64, miner: &Actor, text: &str) -> Work {
        let row = format!("{text:<8}").into_bytes();
        self.work(spec, round, miner, &[row])
    }
}

/// `miner`'s signed commit of `work` to `shard` in `round`.
pub(crate) fn signed_commit(
    round: &Round,
    shard: &Hash32,
    miner: &Actor,
    work: &Work,
    bond: u128,
) -> SignedCommit {
    let msg = commit_message(&round.tag(), shard, &work.commit_hash, bond);
    SignedCommit {
        miner: miner.addr,
        shard: *shard,
        commit_hash: work.commit_hash,
        bond,
        sig: miner.sign(msg.as_bytes()),
    }
}

/// `v`'s signed verdict commit on `(shard, miner)` under `seed`.
pub(crate) fn signed_verdict(
    seed: &Hash32,
    v: &Actor,
    shard: &Hash32,
    miner: &MinerAddress,
    verdict: &Verdict,
) -> SignedVerdict {
    let salt = v.salt(&keccak256(&[shard.0.as_slice(), miner].concat()));
    let commit = verdict_commit_hash(seed, &v.addr, shard, miner, verdict, &salt);
    SignedVerdict {
        validator: v.addr,
        shard: *shard,
        miner: *miner,
        commit,
        sig: v.sign(verdict_message(seed, shard, miner, &commit).as_bytes()),
    }
}

/// The salt [`signed_verdict`] used.
pub(crate) fn verdict_salt(v: &Actor, shard: &Hash32, miner: &MinerAddress) -> Hash32 {
    v.salt(&keccak256(&[shard.0.as_slice(), miner].concat()))
}

/// What a fixture miner does in a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plays {
    /// Commits and reveals the honest rows.
    Honest,
    /// Commits and reveals an invented row.
    Wrong,
    /// Commits the honest rows and never reveals.
    Silent,
}

/// One closed round of one shard, ready for validation: the miners commit in
/// the order given (heights 50, 51, ...), reveal at 150, the round closes at
/// 200; the validator set and canary commitment are fixed in its config.
pub(crate) struct Closed {
    /// The world.
    pub w: World,
    /// The ledger that opened the round.
    pub ledger: Ledger<Rules>,
    /// The closed round.
    pub round: Round,
    /// Its one shard spec.
    pub spec: ShardSpec,
    /// Its shard id.
    pub shard: Hash32,
    /// Miners, in commit order.
    pub miners: Vec<Actor>,
    /// Validators (all bonded `bond`).
    pub validators: Vec<Actor>,
    /// Their set.
    pub set: ValidatorSet,
    /// The honest root.
    pub good: Hash32,
}

/// Salt of the empty canary list [`Closed::new`] commits to.
pub(crate) fn canary_salt() -> Hash32 {
    keccak256(b"canary salt")
}

impl Closed {
    /// Round [`ROUND`] with `plays` (one miner each, `miner{i}`, bond `1,000 + i`),
    /// validators `validators` bonded `bonds`, the canary commitment
    /// `canary`, and `cfg` around it.
    pub(crate) fn with(
        plays: &[Plays],
        validators: Vec<Actor>,
        bonds: &[u128],
        canary: Hash32,
        cfg: RoundConfig,
    ) -> Self {
        Self::in_world(World::new(), plays, validators, bonds, canary, cfg)
    }

    /// [`Closed::with`] in world `w`.
    pub(crate) fn in_world(
        w: World,
        plays: &[Plays],
        validators: Vec<Actor>,
        bonds: &[u128],
        canary: Hash32,
        cfg: RoundConfig,
    ) -> Self {
        let mut ledger = Ledger::<Rules>::new();
        let spec = w.spec(1);
        let shard = spec.shard_id();
        let miners = actors("miner", plays.len());
        let mut set = ValidatorSet::new();
        for (a, b) in validators.iter().zip(bonds) {
            set.add(a.addr, *b, 0).expect("validator");
        }
        let cfg = RoundConfig {
            validator_set_root: set.root(),
            canary_commit: canary,
            ..cfg
        };
        let mut round = w
            .open(ROUND, vec![spec.clone()], cfg, 10, &mut ledger)
            .expect("open");
        let works: Vec<Work> = plays
            .iter()
            .zip(&miners)
            .map(|(p, m)| match p {
                Plays::Honest | Plays::Silent => w.honest(&spec, ROUND, m),
                Plays::Wrong => w.wrong(&spec, ROUND, m, &format!("wrong {}", hex::encode(m.addr))),
            })
            .collect();
        for (i, (m, wk)) in miners.iter().zip(&works).enumerate() {
            let c = signed_commit(&round, &shard, m, wk, 1_000 + i as u128);
            round
                .commit(50 + i as u64, &c, &mut ledger)
                .expect("commit");
        }
        for ((m, wk), p) in miners.iter().zip(&works).zip(plays) {
            if *p != Plays::Silent {
                round
                    .reveal(150, m.addr, shard, wk.commitment.clone(), wk.salt, &w.cas)
                    .expect("reveal");
            }
        }
        round.close(200).expect("close");
        let good = w.honest_root(&spec);
        Closed {
            w,
            ledger,
            round,
            spec,
            shard,
            miners,
            validators,
            set,
            good,
        }
    }

    /// [`Closed::with`] with five validators bonded 10,000, an empty canary
    /// list (salt [`canary_salt`]) and [`CFG`].
    pub(crate) fn new(plays: &[Plays]) -> Self {
        let canary = crate::validate::canary_commit(&[], &canary_salt()).expect("empty list");
        Self::with(plays, actors("validator", 5), &[10_000; 5], canary, CFG)
    }

    /// The miners' revealed roots, by miner index.
    pub(crate) fn root_of(&self, m: usize) -> Hash32 {
        self.round.outcome().expect("closed")[&self.shard]
            .reveals
            .iter()
            .find(|(_, a, _)| *a == self.miners[m].addr)
            .map(|(_, _, r)| *r)
            .expect("revealed")
    }

    /// Start validation at height 210.
    pub(crate) fn validation(&mut self) -> Validation<Rules> {
        self.validation_at(210).expect("validation starts")
    }

    /// Start validation at `now`.
    pub(crate) fn validation_at(&mut self, now: u64) -> Result<Validation<Rules>, ValidationError> {
        Validation::new(
            &self.round,
            self.set.clone(),
            keccak256(b"beacon"),
            now,
            &mut self.ledger,
        )
    }

    /// Every verdict in `revealed` is committed at 250 and revealed at 350;
    /// every one in `unrevealed` is committed at 250 and never revealed.
    /// Entries are `(validator index, miner index, verdict)`.
    pub(crate) fn verdicts(
        &self,
        val: &mut Validation<Rules>,
        revealed: &[(usize, usize, Verdict)],
        unrevealed: &[(usize, usize, Verdict)],
    ) {
        for (v, m, verdict) in revealed.iter().chain(unrevealed) {
            let sv = signed_verdict(
                &val.seed(),
                &self.validators[*v],
                &self.shard,
                &self.miners[*m].addr,
                verdict,
            );
            val.commit_verdict(250, &sv).expect("verdict commit");
        }
        for (v, m, verdict) in revealed {
            let (vv, mm) = (&self.validators[*v], &self.miners[*m]);
            val.reveal_verdict(
                350,
                vv.addr,
                self.shard,
                mm.addr,
                *verdict,
                verdict_salt(vv, &self.shard, &mm.addr),
            )
            .expect("verdict reveal");
        }
    }
}
