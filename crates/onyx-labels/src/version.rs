//! Label-set versioning (north star item 6).
//!
//! Every published version of a named set is a manifest in an append-only
//! log: version `n + 1` must supersede exactly version `n` (by content hash)
//! and be published in a later round. Old versions stay retrievable while a
//! root pins them (labels.md, R-REMOVE-1). [`diff`] says what changed.

use crate::{Label, LabelSet};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{keccak256, to_canonical_bytes, Hash32};

/// One published version of a label set.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Manifest {
    /// Set name.
    pub name: String,
    /// Version, starting at 1.
    pub version: u32,
    /// [`LabelSet::content_hash`] of this version.
    pub content_hash: Hash32,
    /// Content hash of the version it replaces; `None` only for version 1.
    pub supersedes: Option<Hash32>,
    /// [`Manifest::hash`] of the previous manifest; `None` only for version 1.
    /// Chains the whole log, rounds included (L15-F7).
    pub prev_manifest: Option<Hash32>,
    /// Onyx round in which it was published.
    pub published_round: u64,
}

impl Manifest {
    /// Hash of the manifest itself (chains the log).
    pub fn hash(&self) -> Hash32 {
        let mut b = b"onyx-label-manifest-v1".to_vec();
        b.extend_from_slice(&to_canonical_bytes(self).unwrap_or_default());
        keccak256(&b)
    }
}

/// Versioning error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// The set's name differs from the log's.
    #[error("label set name differs from the log")]
    WrongName,
    /// Version is not the next one.
    #[error("version must be {0}")]
    WrongVersion(u32),
    /// Not published after the previous version.
    #[error("round must be after {0}")]
    RoundNotAfter(u64),
    /// Same content as the previous version.
    #[error("no change from the previous version")]
    Unchanged,
    /// A received log does not chain (bad links, numbers or rounds).
    #[error("version log broken at version {0}")]
    BrokenLog(u32),
}

/// Append-only log of the versions of one named set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionLog {
    entries: Vec<Manifest>,
}

impl VersionLog {
    /// Empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// All versions, oldest first.
    pub fn entries(&self) -> &[Manifest] {
        &self.entries
    }

    /// Latest version.
    pub fn latest(&self) -> Option<&Manifest> {
        self.entries.last()
    }

    /// Manifest for a version number.
    pub fn get(&self, version: u32) -> Option<&Manifest> {
        version
            .checked_sub(1)
            .and_then(|i| self.entries.get(usize::try_from(i).ok()?))
    }

    /// Append the next version. The caller has validated `set` against the
    /// registry. The set must carry the next version number, the log's name,
    /// new content, and a later round.
    pub fn append(
        &mut self,
        set: &LabelSet,
        published_round: u64,
    ) -> Result<&Manifest, VersionError> {
        let content_hash = set.content_hash();
        let prev_manifest = self.entries.last().map(Manifest::hash);
        let supersedes = match self.entries.last() {
            None => {
                if set.version != 1 {
                    return Err(VersionError::WrongVersion(1));
                }
                None
            }
            Some(last) => {
                if set.name != last.name {
                    return Err(VersionError::WrongName);
                }
                let next = last.version.saturating_add(1);
                if set.version != next {
                    return Err(VersionError::WrongVersion(next));
                }
                if published_round <= last.published_round {
                    return Err(VersionError::RoundNotAfter(last.published_round));
                }
                if same_labels(set, last) {
                    return Err(VersionError::Unchanged);
                }
                Some(last.content_hash)
            }
        };
        self.entries.push(Manifest {
            name: set.name.clone(),
            version: set.version,
            content_hash,
            supersedes,
            prev_manifest,
            published_round,
        });
        Ok(&self.entries[self.entries.len() - 1])
    }
}

impl VersionLog {
    /// Check a log received from elsewhere against a trusted head (the latest
    /// manifest hash, pinned onchain or in governance records): version 1 first
    /// with no links, then each entry names the previous content hash and
    /// manifest hash, has the next number, the same name and a later round, and
    /// the last entry hashes to `trusted_head`. Without the head, a full rewrite
    /// would also chain (L16-F6).
    pub fn verify(entries: &[Manifest], trusted_head: &Hash32) -> Result<VersionLog, VersionError> {
        if entries.last().map(Manifest::hash) != Some(*trusted_head) {
            return Err(VersionError::BrokenLog(
                entries.last().map(|m| m.version).unwrap_or(0),
            ));
        }
        for (i, m) in entries.iter().enumerate() {
            let ok = match i.checked_sub(1).and_then(|p| entries.get(p)) {
                None => m.version == 1 && m.supersedes.is_none() && m.prev_manifest.is_none(),
                Some(p) => {
                    m.name == p.name
                        && Some(m.version) == p.version.checked_add(1)
                        && m.published_round > p.published_round
                        && m.supersedes == Some(p.content_hash)
                        && m.prev_manifest == Some(p.hash())
                }
            };
            if !ok {
                return Err(VersionError::BrokenLog(m.version));
            }
        }
        Ok(VersionLog {
            entries: entries.to_vec(),
        })
    }
}

/// True if `set` has the same labels as the version `last` describes: the
/// content hash of `set` re-numbered to `last.version` equals `last`'s hash.
fn same_labels(set: &LabelSet, last: &Manifest) -> bool {
    LabelSet {
        name: set.name.clone(),
        version: last.version,
        labels: set.labels.clone(),
    }
    .content_hash()
        == last.content_hash
}

/// What changed between two versions of a set. Each list is sorted by address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabelDiff {
    /// Labels only in the new version.
    pub added: Vec<Label>,
    /// Addresses only in the old version.
    pub removed: Vec<String>,
    /// Same address, different entity, confidence or evidence: (old, new).
    pub changed: Vec<(Label, Label)>,
}

/// Diff two canonical sets (labels strictly sorted by address). Linear merge.
/// `None` if either set is not canonical (L15-F7).
pub fn diff(old: &LabelSet, new: &LabelSet) -> Option<LabelDiff> {
    let sorted = |s: &LabelSet| s.labels.windows(2).all(|w| w[0].address < w[1].address);
    if !sorted(old) || !sorted(new) {
        return None;
    }
    let mut d = LabelDiff::default();
    let (mut i, mut j) = (0, 0);
    while i < old.labels.len() || j < new.labels.len() {
        match (old.labels.get(i), new.labels.get(j)) {
            (Some(a), Some(b)) if a.address == b.address => {
                if a != b {
                    d.changed.push((a.clone(), b.clone()));
                }
                i += 1;
                j += 1;
            }
            (Some(a), Some(b)) if a.address < b.address => {
                d.removed.push(a.address.clone());
                i += 1;
            }
            (Some(_), Some(b)) => {
                d.added.push(b.clone());
                j += 1;
            }
            (Some(a), None) => {
                d.removed.push(a.address.clone());
                i += 1;
            }
            (None, Some(b)) => {
                d.added.push(b.clone());
                j += 1;
            }
            (None, None) => break,
        }
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{Confidence, EntityKind};

    fn label(addr: &str, entity: &str, ppm: u32) -> Label {
        Label {
            address: addr.into(),
            entity: entity.into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(ppm),
            evidence: vec![],
        }
    }

    fn set(version: u32, labels: Vec<Label>) -> LabelSet {
        LabelSet {
            name: "onyx.test".into(),
            version,
            labels,
        }
    }

    #[test]
    fn versions_chain_by_hash_and_round() {
        let v1 = set(1, vec![label("t:a", "entity:x", 900_000)]);
        let v2 = set(2, vec![label("t:a", "entity:x", 950_000)]);
        let mut log = VersionLog::new();
        assert_eq!(
            log.append(&v2, 1).map(|_| ()),
            Err(VersionError::WrongVersion(1))
        );
        log.append(&v1, 10).expect("v1");
        assert_eq!(
            log.append(&v2, 10).map(|_| ()),
            Err(VersionError::RoundNotAfter(10))
        );
        let m2 = log.append(&v2, 11).expect("v2").clone();
        assert_eq!(m2.supersedes, Some(v1.content_hash()));
        assert_eq!(log.get(1).map(|m| m.content_hash), Some(v1.content_hash()));
        assert_eq!(log.latest(), Some(&m2));
        assert_eq!(log.get(0), None);
        // Skipping a number, renaming, or republishing the same labels is refused.
        let v4 = set(4, vec![label("t:a", "entity:x", 1)]);
        assert_eq!(
            log.append(&v4, 12).map(|_| ()),
            Err(VersionError::WrongVersion(3))
        );
        let mut renamed = set(3, vec![label("t:a", "entity:x", 1)]);
        renamed.name = "onyx.other".into();
        assert_eq!(
            log.append(&renamed, 12).map(|_| ()),
            Err(VersionError::WrongName)
        );
        let same = set(3, v2.labels.clone());
        assert_eq!(
            log.append(&same, 12).map(|_| ()),
            Err(VersionError::Unchanged)
        );
        assert_ne!(log.entries()[0].hash(), log.entries()[1].hash());

        // A received log verifies; edits to a round or a link break it.
        let head = log.entries()[1].hash();
        assert_eq!(VersionLog::verify(log.entries(), &head), Ok(log.clone()));
        let mut forged = log.entries().to_vec();
        forged[0].published_round = 9;
        assert!(VersionLog::verify(&forged, &head).is_err());
        let mut relinked = log.entries().to_vec();
        relinked[1].supersedes = None;
        assert!(VersionLog::verify(&relinked, &head).is_err());
        // L16-F6: a full rewrite that chains correctly fails against the trusted head.
        let mut rewrite = VersionLog::new();
        rewrite.append(&v1, 10).expect("v1");
        rewrite
            .append(&set(2, vec![label("t:z", "entity:y", 1)]), 11)
            .expect("v2'");
        let fake_head = rewrite.entries()[1].hash();
        assert!(VersionLog::verify(rewrite.entries(), &fake_head).is_ok());
        assert!(VersionLog::verify(rewrite.entries(), &head).is_err());

        // Mutation L4: each first-entry rule and a repeated round on their
        // own, each against a head that matches the edited log.
        let check = |entries: &[Manifest]| {
            let head = entries.last().expect("entries").hash();
            VersionLog::verify(entries, &head)
        };
        let first = log.entries()[..1].to_vec();
        assert!(check(&first).is_ok());
        let mut e = first.clone();
        e[0].supersedes = Some(v1.content_hash());
        assert!(check(&e).is_err());
        let mut e = first.clone();
        e[0].prev_manifest = Some(e[0].hash());
        assert!(check(&e).is_err());
        let mut e = first;
        e[0].version = 2;
        assert!(check(&e).is_err());
        let mut same_round = log.entries().to_vec();
        same_round[1].published_round = same_round[0].published_round;
        assert!(check(&same_round).is_err());
    }

    #[test]
    fn diff_lists_added_removed_changed() {
        let old = set(
            1,
            vec![
                label("t:a", "entity:x", 900_000),
                label("t:b", "entity:x", 900_000),
                label("t:d", "entity:x", 900_000),
            ],
        );
        let new = set(
            2,
            vec![
                label("t:a", "entity:x", 900_000),
                label("t:b", "entity:y", 900_000),
                label("t:c", "entity:x", 800_000),
            ],
        );
        let d = diff(&old, &new).expect("canonical");
        assert_eq!(d.added, vec![label("t:c", "entity:x", 800_000)]);
        assert_eq!(d.removed, vec!["t:d".to_string()]);
        assert_eq!(
            d.changed,
            vec![(
                label("t:b", "entity:x", 900_000),
                label("t:b", "entity:y", 900_000)
            )]
        );
        assert_eq!(diff(&old, &old), Some(LabelDiff::default()));
        // Non-canonical input is refused, not misread.
        let dup = set(
            1,
            vec![label("t:a", "entity:x", 1), label("t:a", "entity:x", 1)],
        );
        assert_eq!(diff(&dup, &old), None);
        // Mutation L4: removals and additions interleaved in the middle, and
        // a tail of additions after the old set ends.
        let old = set(
            1,
            vec![label("t:a", "entity:x", 1), label("t:c", "entity:x", 1)],
        );
        let new = set(
            2,
            vec![label("t:b", "entity:x", 1), label("t:c", "entity:x", 1)],
        );
        let d = diff(&old, &new).expect("canonical");
        assert_eq!(d.removed, vec!["t:a".to_string()]);
        assert_eq!(d.added, vec![label("t:b", "entity:x", 1)]);
        assert!(d.changed.is_empty());
        let tail = set(
            2,
            vec![
                label("t:a", "entity:x", 1),
                label("t:c", "entity:x", 1),
                label("t:d", "entity:x", 1),
                label("t:e", "entity:x", 1),
            ],
        );
        let d = diff(&old, &tail).expect("canonical");
        assert_eq!(
            d.added,
            vec![label("t:d", "entity:x", 1), label("t:e", "entity:x", 1)]
        );
        assert!(d.removed.is_empty());
        let d = diff(&set(1, vec![]), &old).expect("canonical");
        assert_eq!(d.added.len(), 2);
    }
}
