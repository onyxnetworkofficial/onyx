//! Sector-agnostic domain types for Onyx.
//!
//! Everything here must make sense for crypto, equities, macro, commodities,
//! prediction-market settlement and private pooled data alike. Nothing in this
//! crate may name a chain, an exchange or a data vendor. Sector specifics live
//! in `adapters/*` and `recipes/*`, which depend on this crate, never the
//! other way round (enforced by `scripts/check-boundaries.sh`).
//!
//! All types derive Borsh and encode canonically (ADR-0002). All hashes are
//! Keccak-256 (ADR-0003). No type carries a float (ADR-0009).

pub mod canonical;
pub mod claim;
pub mod commitment;
pub mod estimate;
pub mod feed;
pub mod fetch;
pub mod hash;
pub mod ids;
pub mod ladder;
pub mod onchain;
pub mod proof;
pub mod registry;
pub mod rights;
pub mod signed;
pub mod source;
pub mod value;

pub use canonical::{from_canonical_bytes, to_canonical_bytes, CanonicalError};
pub use claim::{
    evidence_is_canonical, AttestationScheme, Claim, ClaimDecodeError, ClaimError, Confidence,
    EvidenceRef, LabelSetRef, Lane, MetricId, Subject, VerificationMethod,
};
pub use commitment::{
    challenge_hash, commit_hash, evm_ledger_id, shard_id, Commitment, CommitmentError,
    CommitmentFields, MinerAddress, RuntimeProfile, SchemaId, ShardSpec,
};
pub use estimate::{is_estimate_id, ErrorBand, Estimate, EstimateError, ESTIMATE_PREFIX};
pub use feed::{day_number, DateLag, FeedTemplate, SubjectRule, TemplateError};
pub use hash::{keccak256, Hash32, RecipeId};
pub use ids::{AddressCanon, AddressProof, ProofOutcome, ProofVector};
pub use ladder::{value_key, Contradiction, Ladder, LadderError, Step};
pub use proof::{
    observation_commit, observe, Answer, Fetcher, MarketPolicy, Observation, ObservationError,
    ObservationKind, ObservationSpec, ProofLevel,
};
pub use registry::{
    EntityKind, KeyState, MetricInfo, NoRules, Registry, RegistryData, RegistryDataError,
    RegistryError, RuleBuilder, RuleSet, StaticRegistry,
};
pub use rights::{DataRights, Redistribution, RightsError};
pub use signed::{
    is_origin_host, KeyProvenance, SignedFormError, SignedPayload, SignedReport, SourceKey,
};
pub use source::{ChainId, Snapshot, SourceAdapter, SourceError, SourceId, SourceRef};
pub use value::{Value, ValueError, ValueKind};
