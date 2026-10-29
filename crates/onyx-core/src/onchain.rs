//! Keys shared with the on-chain registry (`contracts/src/OnyxRegistry.sol`,
//! ADR-0030). Vectors in `fixtures/registry/vectors.json` pin each one on both
//! sides.

use crate::claim::{MetricId, Subject};
use crate::commitment::{shard_id, CommitmentFields, RuntimeProfile, ShardSpec};
use crate::feed::FeedTemplate;
use crate::{keccak256, Hash32};

/// `keccak256(utf8(metric id))`: the key `OnyxRegistry.metrics` uses.
pub fn metric_key(metric: &MetricId) -> Hash32 {
    keccak256(metric.0.as_bytes())
}

/// How a registered metric is computed (`OnyxRegistry.Kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MetricKind {
    /// Level 1: the template is the recipe id.
    Recipe = 1,
    /// Level 2, one fact per (subject, window end): the template is `FeedTemplate::id`.
    Feed = 2,
    /// Level 2, one fact per dated subject whatever the window (L2SRC-R2-F2).
    DatedFeed = 3,
}

impl MetricKind {
    /// The kind a level-2 template must be registered with (REG-R2-F2): a
    /// dated subject rule is one fact per date, so `DatedFeed`; a fixed one is
    /// `Feed`. Every registration path and every consumer derives the kind
    /// here, never from a separate choice: the chain cannot read a template.
    pub fn of(template: &FeedTemplate) -> Self {
        if template.is_dated() {
            MetricKind::DatedFeed
        } else {
            MetricKind::Feed
        }
    }
}

/// The Borsh encoding of a runtime profile: the bytes whose Keccak-256 is the
/// shard's `runtimeHash`. `OnyxRegistry.attestShard` takes them and reads the
/// fuel limit out of them (REG-R2-F1), so the 50% preflight rule is checked
/// against the limit the shard commits to, never a number the coordinator
/// chooses.
pub fn runtime_bytes(profile: &RuntimeProfile) -> Vec<u8> {
    borsh::to_vec(profile).unwrap_or_default()
}

/// The fields `OnyxRegistry.attestShard` takes and recomputes the shard id
/// from (`OnyxRegistry.ShardPreimage`, REG-R1-F1): the same packing as
/// `ShardSpec::shard_id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardPreimage {
    /// `ShardSpec::recipe`, zero when none.
    pub recipe_id: Hash32,
    /// `RuntimeProfile::hash`, zero when none.
    pub runtime_hash: Hash32,
    /// `LabelSetRef::hash`, zero when the shard has no label set.
    pub label_set_hash: Hash32,
    /// `SourceRef::hash`.
    pub source_hash: Hash32,
    /// The row schema id.
    pub schema_id: Hash32,
    /// The registered rights hash.
    pub rights_hash: Hash32,
    /// `keccak256(params)`.
    pub params_hash: Hash32,
    /// The registry root the round is judged against.
    pub registry_root: Hash32,
}

impl ShardPreimage {
    /// The preimage of `spec`'s shard id.
    pub fn of(spec: &ShardSpec) -> Self {
        let z = |h: Option<Hash32>| h.unwrap_or(Hash32::ZERO);
        ShardPreimage {
            recipe_id: z(spec.recipe),
            runtime_hash: z(spec.runtime.as_ref().map(RuntimeProfile::hash)),
            label_set_hash: z(spec.label_set.as_ref().map(|l| l.hash())),
            source_hash: spec.source.hash(),
            schema_id: spec.schema,
            rights_hash: spec.rights_hash,
            params_hash: keccak256(&spec.params),
            registry_root: spec.registry_root,
        }
    }

    /// The shard id (`OnyxRegistry.shardIdOf`).
    pub fn shard_id(&self) -> Hash32 {
        let fields = CommitmentFields {
            root: Hash32::ZERO,
            leaf_count: 0,
            recipe_id: self.recipe_id,
            runtime_hash: self.runtime_hash,
            label_set_hash: self.label_set_hash,
            source_hash: self.source_hash,
            schema_id: self.schema_id,
            rights_hash: self.rights_hash,
        };
        shard_id(
            &fields.shard_fields_hash(),
            &self.params_hash,
            &self.registry_root,
        )
    }
}

/// `keccak256(borsh(subject))`: the subject key `OnyxRegistry.recordObservation`
/// takes.
pub fn subject_key(subject: &Subject) -> Hash32 {
    keccak256(&borsh::to_vec(subject).unwrap_or_default())
}

/// `keccak256("onyx-registry-fact-v1" ‖ metric ‖ template ‖ subject key ‖
/// window end (u64 BE, 0 when dated))`: `OnyxRegistry.factKeyOf`, computed on
/// chain from the metric's registered template (REG-R1-F2).
pub fn registry_fact_key(
    metric: &Hash32,
    template: &Hash32,
    subject: &Hash32,
    window_end: u64,
    dated: bool,
) -> Hash32 {
    let mut b = b"onyx-registry-fact-v1".to_vec();
    b.extend_from_slice(&metric.0);
    b.extend_from_slice(&template.0);
    b.extend_from_slice(&subject.0);
    b.extend_from_slice(&(if dated { 0 } else { window_end }).to_be_bytes());
    keccak256(&b)
}

/// `keccak256("onyx-registry-subject-draws-v1" ‖ metric ‖ subject key)`:
/// `OnyxRegistry.subjectDrawKey`, the key of a (metric, subject)'s draw bound
/// (L2RND-R2-F1/F2). Every window and attempt of the subject shares it.
pub fn subject_draw_key(metric: &Hash32, subject: &Hash32) -> Hash32 {
    let mut b = b"onyx-registry-subject-draws-v1".to_vec();
    b.extend_from_slice(&metric.0);
    b.extend_from_slice(&subject.0);
    keccak256(&b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::LabelSetRef;
    use crate::source::{SourceId, SourceRef};
    use crate::Snapshot;

    #[test]
    fn metric_key_is_the_keccak_of_the_id_bytes() {
        let m = MetricId("crypto.btc.entity_flow".into());
        assert_eq!(metric_key(&m), keccak256(b"crypto.btc.entity_flow"));
        assert_ne!(
            metric_key(&m),
            metric_key(&MetricId("crypto.btc.entity_flow ".into()))
        );
    }

    #[test]
    fn the_preimage_gives_the_spec_shard_id() {
        let spec = ShardSpec {
            recipe: Some(keccak256(b"recipe")),
            runtime: None,
            label_set: Some(LabelSetRef {
                name: "test.labels".into(),
                version: 1,
                content_hash: keccak256(b"labels"),
            }),
            source: SourceRef {
                source: SourceId("btc.mainnet.node".into()),
                snapshot: Snapshot::Document {
                    published_unix: 1,
                    content_hash: keccak256(b"doc"),
                },
            },
            schema: keccak256(b"schema"),
            rights_hash: keccak256(b"rights"),
            params: b"params".to_vec(),
            registry_root: keccak256(b"registry"),
        };
        assert_eq!(ShardPreimage::of(&spec).shard_id(), spec.shard_id());
        let mut other = spec.clone();
        other.recipe = Some(keccak256(b"another recipe"));
        assert_ne!(ShardPreimage::of(&other).shard_id(), spec.shard_id());
    }

    #[test]
    fn the_runtime_bytes_hash_to_the_runtime_hash() {
        let p = RuntimeProfile {
            abi_version: 1,
            host: "onyx-host.0.5.0.wasmi.2.0.0".into(),
            fuel_limit: 4_000_000_000,
            max_memory_bytes: 64 << 20,
            max_output_bytes: 1 << 20,
            max_call_depth: 1_024,
        };
        let b = runtime_bytes(&p);
        assert_eq!(keccak256(&b), p.hash());
        // abi u32 | host len u32 | host | fuel u64 | memory u64 | output u32 | depth u32
        let at = 8 + p.host.len();
        assert_eq!(b.len(), at + 24);
        let mut fuel = [0u8; 8];
        fuel.copy_from_slice(&b[at..at + 8]);
        assert_eq!(u64::from_le_bytes(fuel), p.fuel_limit);
    }

    #[test]
    fn a_dated_fact_ignores_the_window_and_the_template_is_in_the_key() {
        let (m, t, s) = (keccak256(b"m"), keccak256(b"t"), keccak256(b"s"));
        assert_eq!(
            registry_fact_key(&m, &t, &s, 10, true),
            registry_fact_key(&m, &t, &s, 99, true)
        );
        assert_ne!(
            registry_fact_key(&m, &t, &s, 10, false),
            registry_fact_key(&m, &t, &s, 99, false)
        );
        assert_ne!(
            registry_fact_key(&m, &t, &s, 10, false),
            registry_fact_key(&m, &keccak256(b"rogue"), &s, 10, false)
        );
    }
}
