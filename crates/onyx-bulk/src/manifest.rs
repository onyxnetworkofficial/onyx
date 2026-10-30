//! The manifest: a dataset version's header and its shard list.
//!
//! `root = keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`, and the
//! header commits to the shard list by `shards_root`, the ADR-0003 Merkle
//! root over `borsh(entry)` of every [`ShardEntry`] in index order. A served
//! row thus needs the header, its shard's entry with a proof, and the row's
//! proof, never the whole list.

use crate::schema::Schema;
use crate::shard::{ShardSummary, MAX_SHARD_BYTES, MAX_SHARD_ROWS};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::is_dotted_id;
use onyx_core::{keccak256, DataRights, Hash32, Redistribution, SourceId};
use onyx_merkle::MerkleTree;

/// Domain tag of [`ManifestHeader::root`].
pub const MANIFEST_DOMAIN: &[u8] = b"onyx-bulk-manifest-v1";
/// The one format this crate reads.
pub const FORMAT: u8 = 1;
/// Most shards in one manifest (with 64 MiB shards, 4 TiB).
pub const MAX_SHARDS: u32 = 1 << 16;
/// Largest Borsh manifest (header and every entry: 65,536 entries of 84
/// bytes plus a header, with room).
pub const MAX_MANIFEST_BYTES: usize = 8 << 20;
/// The statement a publisher signs into every manifest (THREATS CL5-7): the
/// node serves nothing whose header lacks it word for word.
pub const RIGHTS_STATEMENT: &str = "onyx-bulk-rights-v1: the publisher owns this data or holds the right to share it; its terms let anyone redistribute it; it names no individual person.";

/// One shard as the manifest lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ShardEntry {
    /// Position in the list (0, 1, 2, ...).
    pub index: u32,
    /// Merkle root over the shard's rows.
    pub root: Hash32,
    /// Rows.
    pub rows: u64,
    /// Shard file bytes.
    pub bytes: u64,
    /// Key of its first row.
    pub key_min: i128,
    /// Key of its last row.
    pub key_max: i128,
}

impl ShardEntry {
    /// The entry for shard `index` with this summary.
    pub fn new(index: u32, s: &ShardSummary) -> Self {
        ShardEntry {
            index,
            root: s.root,
            rows: s.rows,
            bytes: s.bytes,
            key_min: s.key_min,
            key_max: s.key_max,
        }
    }

    /// The leaf bytes of this entry in the shard tree.
    pub fn leaf(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }
}

/// What the publisher signs (through its root).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ManifestHeader {
    /// [`FORMAT`].
    pub format: u8,
    /// Dataset id (dotted id), unique per publisher.
    pub dataset: String,
    /// The publisher: the source whose key signs (ADR-0028), a neutral name.
    pub publisher: SourceId,
    /// 1, 2, 3, ...
    pub version: u32,
    /// Root of the previous version; zero for version 1.
    pub prev: Hash32,
    /// When the publisher made this version (unix seconds); the anchor's
    /// observation time.
    pub created_unix: u64,
    /// Columns and key.
    pub schema: Schema,
    /// Rights (license, redistribution, source = the publisher).
    pub rights: DataRights,
    /// Exactly [`RIGHTS_STATEMENT`].
    pub rights_statement: String,
    /// Shards listed.
    pub shard_count: u32,
    /// Sum of the shards' rows.
    pub row_count: u64,
    /// Sum of the shards' bytes.
    pub byte_count: u64,
    /// Smallest key (first shard's `key_min`).
    pub key_min: i128,
    /// Largest key (last shard's `key_max`).
    pub key_max: i128,
    /// Merkle root over the entries' [`ShardEntry::leaf`] bytes.
    pub shards_root: Hash32,
}

impl ManifestHeader {
    /// `keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`.
    pub fn root(&self) -> Hash32 {
        let mut b = MANIFEST_DOMAIN.to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }

    /// Header rules on their own (no shard list): format, ids, version and
    /// prev, schema, rights and the statement, counts within the caps.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.format != FORMAT {
            return Err(bad("format"));
        }
        if !is_dotted_id(&self.dataset) {
            return Err(bad("dataset id (dotted id)"));
        }
        if !is_dotted_id(&self.publisher.0) {
            return Err(bad("publisher (dotted id)"));
        }
        if self.version == 0 || (self.version == 1) != (self.prev == Hash32::ZERO) {
            return Err(bad("version 1 has no prev; later versions name it"));
        }
        if self.created_unix == 0 {
            return Err(bad("created time"));
        }
        self.schema
            .validate()
            .map_err(|e| ManifestError::Bad(e.to_string()))?;
        self.rights
            .check_ingest()
            .map_err(|e| ManifestError::Rights(e.to_string()))?;
        if self.rights.redistribution != Redistribution::Allowed {
            return Err(ManifestError::Rights(
                "only redistribution Allowed is served (public free reading)".into(),
            ));
        }
        if self.rights.source != self.publisher {
            return Err(ManifestError::Rights(
                "the rights' source must be the publisher".into(),
            ));
        }
        if self.rights_statement != RIGHTS_STATEMENT {
            return Err(ManifestError::Rights(
                "the rights statement is not the onyx-bulk-rights-v1 text".into(),
            ));
        }
        if self.shard_count == 0 || self.shard_count > MAX_SHARDS {
            return Err(bad("shard count 1..=65536"));
        }
        if self.key_min > self.key_max {
            return Err(bad("key range"));
        }
        Ok(())
    }

    /// Whether `entry` lies under this header's shard tree with `proof`.
    pub fn proves_entry(&self, entry: &ShardEntry, proof: &onyx_merkle::Proof) -> bool {
        proof.index == u64::from(entry.index)
            && proof.leaf_count == u64::from(self.shard_count)
            && onyx_merkle::verify(&self.shards_root, &entry.leaf(), proof)
    }
}

/// A manifest that does not hold.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// Bad shape.
    #[error("manifest: {0}")]
    Bad(String),
    /// Rights refused (THREATS CL5-7).
    #[error("rights: {0}")]
    Rights(String),
    /// Not an append-only successor of the previous version.
    #[error("not a successor: {0}")]
    Successor(String),
    /// Encoding.
    #[error("manifest bytes do not decode")]
    Encoding,
}

fn bad(s: &str) -> ManifestError {
    ManifestError::Bad(s.into())
}

/// A version: header and the full shard list.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Manifest {
    /// The signed part.
    pub header: ManifestHeader,
    /// Every shard, by index.
    pub shards: Vec<ShardEntry>,
}

/// The fields of a new version besides its shards.
#[derive(Clone, Debug)]
pub struct NewVersion {
    /// Dataset id.
    pub dataset: String,
    /// Publisher source id.
    pub publisher: SourceId,
    /// Creation time (unix seconds).
    pub created_unix: u64,
    /// Schema.
    pub schema: Schema,
    /// Rights.
    pub rights: DataRights,
}

/// The tree over a shard list.
pub fn shards_tree(shards: &[ShardEntry]) -> MerkleTree {
    let leaves: Vec<Vec<u8>> = shards.iter().map(ShardEntry::leaf).collect();
    MerkleTree::from_rows(&leaves)
}

impl Manifest {
    /// Version 1 (`prev` = `None`) or the successor of `prev`, over `shards`
    /// (for a successor: `prev`'s shards followed by the new ones).
    pub fn build(
        v: NewVersion,
        prev: Option<&Manifest>,
        shards: Vec<ShardEntry>,
    ) -> Result<Manifest, ManifestError> {
        let tree = shards_tree(&shards);
        let header = ManifestHeader {
            format: FORMAT,
            dataset: v.dataset,
            publisher: v.publisher,
            version: prev.map_or(1, |p| p.header.version.saturating_add(1)),
            prev: prev.map_or(Hash32::ZERO, |p| p.header.root()),
            created_unix: v.created_unix,
            schema: v.schema,
            rights: v.rights,
            rights_statement: RIGHTS_STATEMENT.into(),
            shard_count: u32::try_from(shards.len()).map_err(|_| bad("too many shards"))?,
            row_count: shards.iter().map(|s| s.rows).sum(),
            byte_count: shards.iter().map(|s| s.bytes).sum(),
            key_min: shards.first().map_or(0, |s| s.key_min),
            key_max: shards.last().map_or(0, |s| s.key_max),
            shards_root: tree.root(),
        };
        let m = Manifest { header, shards };
        m.validate()?;
        if let Some(p) = prev {
            m.check_successor(p)?;
        }
        Ok(m)
    }

    /// Header rules plus: entries indexed 0..n in order, each within the
    /// caps, key ranges ordered (`key_max` of one at most `key_min` of the
    /// next), and the header's counts, key range and `shards_root` equal to
    /// the list's.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let h = &self.header;
        h.validate()?;
        if self.shards.len() != h.shard_count as usize {
            return Err(bad("shard count differs from the list"));
        }
        let (mut rows, mut bytes) = (0u64, 0u64);
        for (i, e) in self.shards.iter().enumerate() {
            if e.index as usize != i {
                return Err(bad("shard indexes must be 0, 1, 2, ... in order"));
            }
            if e.rows == 0 || e.rows > MAX_SHARD_ROWS {
                return Err(bad("shard rows 1..=2^18"));
            }
            if e.bytes > MAX_SHARD_BYTES || e.bytes < 4 + 4 * e.rows {
                return Err(bad("shard bytes over 64 MiB or under the framing"));
            }
            if e.key_min > e.key_max {
                return Err(bad("shard key range"));
            }
            if i > 0 && self.shards[i - 1].key_max > e.key_min {
                return Err(bad("shard key ranges overlap or go down"));
            }
            rows = rows
                .checked_add(e.rows)
                .ok_or_else(|| bad("rows overflow"))?;
            bytes = bytes
                .checked_add(e.bytes)
                .ok_or_else(|| bad("bytes overflow"))?;
        }
        if rows != h.row_count || bytes != h.byte_count {
            return Err(bad("row or byte count differs from the list"));
        }
        let (first, last) = (self.shards.first(), self.shards.last());
        if first.map(|s| s.key_min) != Some(h.key_min) || last.map(|s| s.key_max) != Some(h.key_max)
        {
            return Err(bad("key range differs from the list"));
        }
        if shards_tree(&self.shards).root() != h.shards_root {
            return Err(bad("shards_root differs from the list"));
        }
        Ok(())
    }

    /// Append-only rule: same dataset, publisher, schema and rights; the next
    /// version number; `prev` is the previous root; later in time; every
    /// earlier shard kept unchanged in place and at least one added.
    pub fn check_successor(&self, prev: &Manifest) -> Result<(), ManifestError> {
        let (h, p) = (&self.header, &prev.header);
        let fail = |s: &str| Err(ManifestError::Successor(s.into()));
        if h.dataset != p.dataset || h.publisher != p.publisher {
            return fail("another dataset or publisher");
        }
        if h.schema != p.schema || h.rights != p.rights {
            return fail("the schema and rights stay the same across versions");
        }
        if Some(h.version) != p.version.checked_add(1) || h.prev != p.root() {
            return fail("version must be the previous plus one and prev its root");
        }
        if h.created_unix <= p.created_unix {
            return fail("created time must move forward");
        }
        if self.shards.len() <= prev.shards.len()
            || self.shards[..prev.shards.len()] != prev.shards[..]
        {
            return fail("every earlier shard is kept in place and at least one is added");
        }
        Ok(())
    }

    /// Borsh bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }

    /// Decode (at most [`MAX_MANIFEST_BYTES`]) and validate.
    pub fn from_bytes(b: &[u8]) -> Result<Manifest, ManifestError> {
        if b.len() > MAX_MANIFEST_BYTES {
            return Err(bad("over 8 MiB"));
        }
        let m = Manifest::try_from_slice(b).map_err(|_| ManifestError::Encoding)?;
        m.validate()?;
        Ok(m)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn rights(publisher: &str) -> DataRights {
        DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId(publisher.into()),
            attribution: None,
            terms: String::new(),
        }
    }

    pub(crate) fn new_version(created: u64) -> NewVersion {
        NewVersion {
            dataset: "synthetic.chain-metrics".into(),
            publisher: SourceId("example.publisher".into()),
            created_unix: created,
            schema: Schema::from_json(&json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"text"}],"key":"t"})).expect("ok"),
            rights: rights("example.publisher"),
        }
    }

    pub(crate) fn entry(i: u32, kmin: i128, kmax: i128) -> ShardEntry {
        ShardEntry {
            index: i,
            root: keccak256(&i.to_le_bytes()),
            rows: 10,
            bytes: 1_000,
            key_min: kmin,
            key_max: kmax,
        }
    }

    #[test]
    fn build_validate_root() {
        let m = Manifest::build(new_version(100), None, vec![entry(0, 1, 5), entry(1, 5, 9)])
            .expect("ok");
        assert_eq!(m.header.version, 1);
        assert_eq!((m.header.row_count, m.header.byte_count), (20, 2_000));
        assert_eq!(Manifest::from_bytes(&m.to_bytes()).expect("ok"), m);
        // Any header change changes the root.
        let mut h = m.header.clone();
        h.created_unix += 1;
        assert_ne!(h.root(), m.header.root());
        // Entry proofs.
        let tree = shards_tree(&m.shards);
        let p = tree.proof(1).expect("ok");
        assert!(m.header.proves_entry(&m.shards[1], &p));
        let mut forged = m.shards[1];
        forged.rows += 1;
        assert!(!m.header.proves_entry(&forged, &p));
    }

    #[test]
    fn refuses_bad_lists() {
        let ok = Manifest::build(new_version(100), None, vec![entry(0, 1, 5), entry(1, 5, 9)])
            .expect("ok");
        let mut cases: Vec<Manifest> = Vec::new();
        let mut m = ok.clone();
        m.shards.swap(0, 1);
        cases.push(m);
        let mut m = ok.clone();
        m.shards[1].key_min = 4; // overlaps shard 0
        cases.push(m);
        let mut m = ok.clone();
        m.shards[0].rows = MAX_SHARD_ROWS + 1;
        cases.push(m);
        let mut m = ok.clone();
        m.shards[0].bytes = MAX_SHARD_BYTES + 1;
        cases.push(m);
        let mut m = ok.clone();
        m.header.row_count += 1;
        cases.push(m);
        let mut m = ok.clone();
        m.header.shards_root = Hash32::ZERO;
        cases.push(m);
        let mut m = ok.clone();
        m.header.prev = keccak256(b"x");
        cases.push(m);
        for (i, c) in cases.iter().enumerate() {
            assert!(c.validate().is_err(), "case {i}");
        }
        assert!(Manifest::build(new_version(100), None, vec![]).is_err());
    }

    #[test]
    fn rights_rules() {
        let base = Manifest::build(new_version(100), None, vec![entry(0, 1, 5)]).expect("ok");
        let mut forbidden = base.clone();
        forbidden.header.rights.redistribution = Redistribution::Forbidden;
        assert!(matches!(
            forbidden.validate(),
            Err(ManifestError::Rights(_))
        ));
        let mut restricted = base.clone();
        restricted.header.rights.redistribution = Redistribution::Restricted;
        restricted.header.rights.terms = "example.terms".into();
        assert!(matches!(
            restricted.validate(),
            Err(ManifestError::Rights(_))
        ));
        let mut other = base.clone();
        other.header.rights.source = SourceId("someone.else".into());
        assert!(matches!(other.validate(), Err(ManifestError::Rights(_))));
        let mut no_statement = base.clone();
        no_statement.header.rights_statement = "we have rights".into();
        assert!(matches!(
            no_statement.validate(),
            Err(ManifestError::Rights(_))
        ));
    }

    #[test]
    fn versions_are_append_only() {
        let v1 = Manifest::build(new_version(100), None, vec![entry(0, 1, 5)]).expect("ok");
        let v2 = Manifest::build(
            new_version(200),
            Some(&v1),
            vec![entry(0, 1, 5), entry(1, 6, 9)],
        )
        .expect("ok");
        assert_eq!(v2.header.version, 2);
        assert_eq!(v2.header.prev, v1.header.root());
        // Dropping, changing or not adding a shard, or going back in time, is refused.
        assert!(Manifest::build(new_version(200), Some(&v1), vec![entry(0, 1, 5)]).is_err());
        let mut changed = entry(0, 1, 5);
        changed.root = keccak256(b"other");
        assert!(
            Manifest::build(new_version(200), Some(&v1), vec![changed, entry(1, 6, 9)]).is_err()
        );
        assert!(Manifest::build(
            new_version(100),
            Some(&v1),
            vec![entry(0, 1, 5), entry(1, 6, 9)]
        )
        .is_err());
        let mut other_schema = new_version(200);
        other_schema.schema.columns[1].nullable = true;
        assert!(Manifest::build(
            other_schema,
            Some(&v1),
            vec![entry(0, 1, 5), entry(1, 6, 9)]
        )
        .is_err());
        // A v2 that skips a version.
        let mut skip = v2.clone();
        skip.header.version = 3;
        assert!(skip.check_successor(&v1).is_err());
    }
}
