//! Spread of a label-dependent metric across label sets (north star item 6).
//!
//! A label-dependent number (an exchange's net flow, an ETF's holdings) is only
//! as good as the label set behind it. So each such metric is computed against
//! at least [`MIN_SETS`] different sets and published with the value per set
//! plus the min and max: the spread is the confidence band (labels.md).

use crate::LabelSet;
use onyx_core::{Commitment, Hash32, LabelSetRef};

/// Fewest label sets a published spread may use.
pub const MIN_SETS: usize = 2;

/// One label set's values for a metric, built only from the set and the
/// run's raw output rows (Merkle leaves), so the labels hash and the root are
/// computed here, never asserted by a caller (L16-F4, L18 part B).
#[derive(Clone, Debug)]
pub struct SetValues {
    /// What produced the values apart from the label set: recipe, runtime,
    /// source snapshot, schema and rights (L20-F1).
    run: (Option<Hash32>, Hash32, Hash32, Hash32, Hash32),
    set: LabelSetRef,
    labels_hash: Hash32,
    root: Hash32,
    rows: Vec<(Vec<u8>, i128)>,
}

impl SetValues {
    /// Values from a revealed run: `commitment` must pin exactly `set` and
    /// commit to exactly `leaves` (root and count), so the set, the rows and
    /// the root belong together (L19-F7). `extract` is the schema's decoder of
    /// `(key, value)` from one row. `None` if anything does not match or decode.
    pub fn from_reveal(
        set: &LabelSet,
        commitment: &Commitment,
        leaves: &[Vec<u8>],
        extract: impl Fn(&[u8]) -> Option<(Vec<u8>, i128)>,
    ) -> Option<Self> {
        let root = onyx_merkle::MerkleTree::from_rows(leaves).root();
        let pinned = commitment
            .label_set
            .as_ref()
            .is_some_and(|p| p.content_hash == set.content_hash());
        if !pinned || root != commitment.root || commitment.leaf_count != leaves.len() as u64 {
            return None;
        }
        let rows = leaves
            .iter()
            .map(|l| extract(l))
            .collect::<Option<Vec<_>>>()?;
        let f = commitment.fields();
        Some(SetValues {
            run: (
                commitment.recipe,
                f.runtime_hash,
                f.source_hash,
                f.schema_id,
                f.rights_hash,
            ),
            set: LabelSetRef {
                name: set.name.clone(),
                version: set.version,
                content_hash: set.content_hash(),
            },
            labels_hash: set.labels_hash(),
            root,
            rows,
        })
    }

    /// Name-independent hash of the set's labels.
    pub fn labels_hash(&self) -> Hash32 {
        self.labels_hash
    }
}

/// Which set each value column came from, published with the spread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetMeta {
    /// Set reference.
    pub set: LabelSetRef,
    /// Labels-only hash.
    pub labels_hash: Hash32,
    /// Root of the run that produced the values.
    pub root: Hash32,
}

/// A published spread: the sets, in column order, and one row per key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spread {
    /// Sets, in the order of each row's `values`.
    pub sets: Vec<SetMeta>,
    /// Rows by key.
    pub rows: Vec<SpreadRow>,
}

/// The spread of one row key across sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpreadRow {
    /// Row key (for example the entity id bytes).
    pub key: Vec<u8>,
    /// Value per input set, in input order; `None` if that set has no row.
    pub values: Vec<Option<i128>>,
    /// Smallest present value.
    pub min: i128,
    /// Largest present value.
    pub max: i128,
}

impl SpreadRow {
    /// Number of sets with a value for this key.
    pub fn present(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }
}

/// Spread input error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SpreadError {
    /// Fewer than [`MIN_SETS`] sets.
    #[error("need at least {MIN_SETS} label sets")]
    TooFewSets,
    /// Sets not strictly ascending by labels hash (the same labels twice, under
    /// any names, would fake a narrow band).
    #[error("label sets must have distinct labels, ordered by labels hash")]
    SetsNotCanonical,
    /// The sets' runs differ in something other than the label set (recipe,
    /// runtime, source, schema or rights): not one metric (L20-F1).
    #[error("runs differ apart from the label set")]
    MixedRuns,
    /// A set's rows are not strictly ascending by key.
    #[error("rows not strictly ascending by key")]
    RowsNotCanonical,
}

/// Merge per-set rows into one spread row per key (union of keys, ascending).
pub fn spread(sets: &[SetValues]) -> Result<Spread, SpreadError> {
    if sets.len() < MIN_SETS {
        return Err(SpreadError::TooFewSets);
    }
    if sets
        .windows(2)
        .any(|w| w[0].labels_hash >= w[1].labels_hash)
    {
        return Err(SpreadError::SetsNotCanonical);
    }
    if sets.windows(2).any(|w| w[0].run != w[1].run) {
        return Err(SpreadError::MixedRuns);
    }
    if sets
        .iter()
        .any(|s| s.rows.windows(2).any(|w| w[0].0 >= w[1].0))
    {
        return Err(SpreadError::RowsNotCanonical);
    }
    let mut cursors = vec![0usize; sets.len()];
    let mut out = Vec::new();
    // Each pass takes the smallest key not yet consumed in any set.
    while let Some(key) = sets
        .iter()
        .zip(&cursors)
        .filter_map(|(s, &c)| s.rows.get(c).map(|r| &r.0))
        .min()
        .cloned()
    {
        let mut values = Vec::with_capacity(sets.len());
        for (s, c) in sets.iter().zip(cursors.iter_mut()) {
            match s.rows.get(*c) {
                Some((k, v)) if *k == key => {
                    values.push(Some(*v));
                    *c += 1;
                }
                _ => values.push(None),
            }
        }
        let present = values.iter().flatten();
        let min = present.clone().copied().min().unwrap_or_default();
        let max = present.copied().max().unwrap_or_default();
        out.push(SpreadRow {
            key,
            values,
            min,
            max,
        });
    }
    Ok(Spread {
        sets: sets
            .iter()
            .map(|s| SetMeta {
                set: s.set.clone(),
                labels_hash: s.labels_hash,
                root: s.root,
            })
            .collect(),
        rows: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Label;
    use onyx_core::{Confidence, EntityKind};

    /// Rows encoded as `key ‖ value (i128 LE)`; the test extractor reads them back.
    fn sv(set: &LabelSet, rows: &[(Vec<u8>, i128)]) -> SetValues {
        let leaves: Vec<Vec<u8>> = rows
            .iter()
            .map(|(k, v)| {
                let mut b = k.clone();
                b.extend_from_slice(&v.to_le_bytes());
                b
            })
            .collect();
        let commitment = Commitment {
            root: onyx_merkle::MerkleTree::from_rows(&leaves).root(),
            leaf_count: leaves.len() as u64,
            recipe: None,
            runtime: None,
            label_set: Some(LabelSetRef {
                name: set.name.clone(),
                version: set.version,
                content_hash: set.content_hash(),
            }),
            source: onyx_core::SourceRef {
                source: onyx_core::SourceId("chain.t.main".into()),
                snapshot: onyx_core::Snapshot::Document {
                    published_unix: 0,
                    content_hash: onyx_core::keccak256(b"doc"),
                },
            },
            schema: onyx_core::keccak256(b"schema"),
            rights_hash: onyx_core::keccak256(b"rights"),
        };
        SetValues::from_reveal(set, &commitment, &leaves, |l| {
            let (k, v) = l.split_at(l.len().checked_sub(16)?);
            Some((k.to_vec(), i128::from_le_bytes(v.try_into().ok()?)))
        })
        .expect("rows decode")
    }

    fn set(name: &str, addrs: &[&str]) -> LabelSet {
        LabelSet {
            name: name.into(),
            version: 1,
            labels: addrs
                .iter()
                .map(|a| Label {
                    address: (*a).into(),
                    entity: "entity:x".into(),
                    kind: EntityKind::Exchange,
                    confidence: Confidence(900_000),
                    evidence: vec![],
                })
                .collect(),
        }
    }

    fn ordered(mut v: Vec<SetValues>) -> Vec<SetValues> {
        v.sort_by_key(SetValues::labels_hash);
        v
    }

    #[test]
    fn spread_merges_keys_and_bands() {
        let (s1, s2) = (set("onyx.a", &["t:a"]), set("onyx.b", &["t:a", "t:b"]));
        let r1 = vec![(b"a".to_vec(), 10), (b"b".to_vec(), -5)];
        let r2 = vec![(b"a".to_vec(), 14), (b"c".to_vec(), 7)];
        let out = spread(&ordered(vec![sv(&s1, &r1), sv(&s2, &r2)])).expect("spread");
        assert_eq!(out.sets.len(), 2);
        assert_eq!(out.rows.len(), 3);
        let a = &out.rows[0];
        assert_eq!((a.min, a.max, a.present()), (10, 14, 2));
        assert_eq!(
            (out.rows[1].min, out.rows[1].max, out.rows[1].present()),
            (-5, -5, 1)
        );
        assert_eq!(out.rows[2].key, b"c".to_vec());
        assert!(out.sets.iter().all(|m| m.root != Hash32([0; 32])));
    }

    #[test]
    fn inputs_must_be_canonical() {
        let rows = vec![(b"a".to_vec(), 1)];
        let s1 = set("onyx.a", &["t:a"]);
        assert_eq!(spread(&[sv(&s1, &rows)]), Err(SpreadError::TooFewSets));
        // L15-F5/L16-F4: the same labels under two names cannot make a narrow band,
        // and the labels hash is computed from the set, not asserted.
        let renamed = set("onyx.other-name", &["t:a"]);
        assert_eq!(
            spread(&[sv(&s1, &rows), sv(&renamed, &rows),]),
            Err(SpreadError::SetsNotCanonical)
        );
        let unsorted = vec![(b"b".to_vec(), 1), (b"a".to_vec(), 1)];
        let s2 = set("onyx.b", &["t:b"]);
        assert_eq!(
            spread(&ordered(vec![sv(&s1, &rows), sv(&s2, &unsorted),])),
            Err(SpreadError::RowsNotCanonical)
        );
    }

    /// L19-F7: leaves from a run with another set, or another root, are refused.
    #[test]
    fn values_must_belong_to_the_revealed_run() {
        let s1 = set("onyx.a", &["t:a"]);
        let s2 = set("onyx.b", &["t:b"]);
        let leaves = vec![b"row".to_vec()];
        let c = |label_set: &LabelSet, root| Commitment {
            root,
            leaf_count: 1,
            recipe: None,
            runtime: None,
            label_set: Some(LabelSetRef {
                name: label_set.name.clone(),
                version: label_set.version,
                content_hash: label_set.content_hash(),
            }),
            source: onyx_core::SourceRef {
                source: onyx_core::SourceId("chain.t.main".into()),
                snapshot: onyx_core::Snapshot::Document {
                    published_unix: 0,
                    content_hash: onyx_core::keccak256(b"doc"),
                },
            },
            schema: onyx_core::keccak256(b"schema"),
            rights_hash: onyx_core::keccak256(b"rights"),
        };
        let root = onyx_merkle::MerkleTree::from_rows(&leaves).root();
        let x = |_: &[u8]| Some((b"k".to_vec(), 7));
        assert!(SetValues::from_reveal(&s1, &c(&s1, root), &leaves, x).is_some());
        assert!(SetValues::from_reveal(&s2, &c(&s1, root), &leaves, x).is_none());
        assert!(
            SetValues::from_reveal(&s1, &c(&s1, onyx_core::keccak256(b"other")), &leaves, x)
                .is_none()
        );
    }

    /// L20-F1: values from runs that differ apart from the label set (here the
    /// source) are not one metric and are refused.
    #[test]
    fn mixed_runs_refused() {
        let (s1, s2) = (set("onyx.a", &["t:a"]), set("onyx.b", &["t:b"]));
        let rows = vec![(b"a".to_vec(), 1)];
        let a = sv(&s1, &rows);
        let mut b = sv(&s2, &rows);
        b.run.2 = onyx_core::keccak256(b"another chain");
        assert_eq!(spread(&ordered(vec![a, b])), Err(SpreadError::MixedRuns));
    }
}
