//! The manifest: a dataset version's header and its shard list.
//!
//! `root = keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`, and the
//! header commits to the shard list by `shards_root`, the ADR-0003 Merkle
//! root over `borsh(entry)` of every [`ShardEntry`] in index order. A served
//! row thus needs the header, its shard's entry with a proof, and the row's
//! proof, never the whole list.
//!
//! Format 2 (ADR-0059 amendment 5) is a dataset whose schema widened: the
//! header adds `earlier` right after `schema`, the schemas earlier shards
//! were built under with the index each one ends at, and every shard is read
//! under [`ManifestHeader::schema_of`] its index. Format-1 bytes, roots and
//! readers stay exactly as they were.

use crate::schema::Schema;
use crate::shard::{ShardSummary, MAX_SHARD_BYTES, MAX_SHARD_ROWS};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::is_dotted_id;
use onyx_core::{keccak256, DataRights, Hash32, Redistribution, SourceId};
use onyx_merkle::MerkleTree;

/// Domain tag of [`ManifestHeader::root`].
pub const MANIFEST_DOMAIN: &[u8] = b"onyx-bulk-manifest-v1";
/// Format 1: one schema for every shard (the only format `OnyxStorage`
/// reads).
pub const FORMAT: u8 = 1;
/// Format 2 (ADR-0059 amendment 5): a widened schema, with the
/// [`EarlierSchema`]s older shards keep.
pub const FORMAT_2: u8 = 2;
/// Most earlier schemas in a format-2 header (8 schemas per dataset in all,
/// at most one widening per version).
pub const MAX_EARLIER: usize = 7;
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

/// A schema the shards before `until` were built under (format 2): an
/// entry covers the shards from the previous entry's `until` (or 0) up to
/// its own `until`; the header's `schema` covers the rest.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EarlierSchema {
    /// The first shard index the next schema covers.
    pub until: u32,
    /// The schema of the shards it covers.
    pub schema: Schema,
}

/// What the publisher signs (through its root). Its Borsh form is written
/// by hand: `earlier` follows `schema` only in format 2, so a format-1
/// header is byte for byte what it always was, and the last 84 bytes stay
/// `shard_count` to `shards_root` (`OnyxStorage` reads them there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestHeader {
    /// [`FORMAT`], or [`FORMAT_2`] for a widened dataset.
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
    /// Columns and key: the newest schema. New shards are built under it,
    /// and answers show every row in it.
    pub schema: Schema,
    /// Format 2 only (empty in format 1): the schemas of older shards, oldest
    /// first, each with the index it ends at.
    pub earlier: Vec<EarlierSchema>,
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

/// The decode error of a header with more than [`MAX_EARLIER`] earlier
/// schemas (refused before any of them is read).
const TOO_MANY_SCHEMAS: &str = "more than 8 schemas";

impl BorshSerialize for ManifestHeader {
    fn serialize<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        self.format.serialize(w)?;
        self.dataset.serialize(w)?;
        self.publisher.serialize(w)?;
        self.version.serialize(w)?;
        self.prev.serialize(w)?;
        self.created_unix.serialize(w)?;
        self.schema.serialize(w)?;
        // Format 2 always writes it; any other format never has one (a
        // header that has one anyway is written with it, so its root covers
        // it, and it then decodes nowhere).
        if self.format == FORMAT_2 || !self.earlier.is_empty() {
            self.earlier.serialize(w)?;
        }
        self.rights.serialize(w)?;
        self.rights_statement.serialize(w)?;
        self.shard_count.serialize(w)?;
        self.row_count.serialize(w)?;
        self.byte_count.serialize(w)?;
        self.key_min.serialize(w)?;
        self.key_max.serialize(w)?;
        self.shards_root.serialize(w)
    }
}

impl BorshDeserialize for ManifestHeader {
    fn deserialize_reader<R: std::io::Read>(r: &mut R) -> std::io::Result<Self> {
        let format = u8::deserialize_reader(r)?;
        let dataset = String::deserialize_reader(r)?;
        let publisher = SourceId::deserialize_reader(r)?;
        let version = u32::deserialize_reader(r)?;
        let prev = Hash32::deserialize_reader(r)?;
        let created_unix = u64::deserialize_reader(r)?;
        let schema = Schema::deserialize_reader(r)?;
        let earlier = if format == FORMAT_2 {
            // The count first: more than the cap is refused before any
            // schema is decoded (review BULK-WIDEN-R1 finding 10).
            let n = u32::deserialize_reader(r)? as usize;
            if n > MAX_EARLIER {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    TOO_MANY_SCHEMAS,
                ));
            }
            let mut e = Vec::with_capacity(n);
            for _ in 0..n {
                e.push(EarlierSchema::deserialize_reader(r)?);
            }
            e
        } else {
            Vec::new()
        };
        Ok(ManifestHeader {
            format,
            dataset,
            publisher,
            version,
            prev,
            created_unix,
            schema,
            earlier,
            rights: DataRights::deserialize_reader(r)?,
            rights_statement: String::deserialize_reader(r)?,
            shard_count: u32::deserialize_reader(r)?,
            row_count: u64::deserialize_reader(r)?,
            byte_count: u64::deserialize_reader(r)?,
            key_min: i128::deserialize_reader(r)?,
            key_max: i128::deserialize_reader(r)?,
            shards_root: Hash32::deserialize_reader(r)?,
        })
    }
}

impl ManifestHeader {
    /// Borsh bytes (what [`Self::root`] hashes).
    pub fn to_bytes(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }

    /// `keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`.
    pub fn root(&self) -> Hash32 {
        root_of_header_bytes(&self.to_bytes())
    }

    /// The index of the schema shard `index` was built under: `k` for
    /// `earlier[k]`, `earlier.len()` for [`Self::schema`] (format 1: 0).
    pub fn schema_index(&self, index: u32) -> usize {
        self.earlier
            .iter()
            .position(|e| index < e.until)
            .unwrap_or(self.earlier.len())
    }

    /// The schema shard `index` was built under (and is checked and decoded
    /// under): the earlier schema whose range holds it, else the newest.
    pub fn schema_of(&self, index: u32) -> &Schema {
        self.earlier
            .get(self.schema_index(index))
            .map_or(&self.schema, |e| &e.schema)
    }

    /// Every schema of the dataset with the index it ends at, oldest first;
    /// the newest ends at `shard_count`.
    pub fn schemas(&self) -> Vec<(u32, &Schema)> {
        self.earlier
            .iter()
            .map(|e| (e.until, &e.schema))
            .chain(std::iter::once((self.shard_count, &self.schema)))
            .collect()
    }

    /// Header rules on their own (no shard list): format, ids, version and
    /// prev, schema (and in format 2 the earlier schemas: 1 to 7, `until`
    /// strictly increasing in `1..shard_count`, each widening to the next and
    /// the last to `schema`), rights and the statement, counts within the
    /// caps.
    pub fn validate(&self) -> Result<(), ManifestError> {
        match self.format {
            FORMAT if self.earlier.is_empty() => {}
            FORMAT => return Err(bad("format 1 has no earlier schemas")),
            FORMAT_2 if (1..=MAX_EARLIER).contains(&self.earlier.len()) => {}
            FORMAT_2 => {
                return Err(bad(
                    "format 2 has 1 to 7 earlier schemas (at most 8 schemas per dataset; one schema is format 1)",
                ))
            }
            _ => return Err(bad("format")),
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
        let mut from = 0u32;
        for (k, e) in self.earlier.iter().enumerate() {
            if e.until <= from || e.until >= self.shard_count {
                return Err(bad(
                    "earlier schemas: until strictly increasing, at least 1 and below shard_count",
                ));
            }
            from = e.until;
            let next = self.earlier.get(k + 1).map_or(&self.schema, |n| &n.schema);
            Schema::widens(&e.schema, next).map_err(|err| {
                ManifestError::Bad(format!(
                    "earlier schema {k} does not widen to the next: {err}"
                ))
            })?;
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

/// The manifest root of Borsh header bytes of any format:
/// `keccak256("onyx-bulk-manifest-v1" ‖ bytes)`.
pub fn root_of_header_bytes(header: &[u8]) -> Hash32 {
    let mut b = MANIFEST_DOMAIN.to_vec();
    b.extend_from_slice(header);
    keccak256(&b)
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
    ///
    /// A successor whose schema differs from `prev`'s widens it (ADR-0059
    /// amendment 5): its earlier schemas are `prev`'s followed by `prev`'s
    /// schema up to `prev`'s shard count, and it is format 2.
    pub fn build(
        v: NewVersion,
        prev: Option<&Manifest>,
        shards: Vec<ShardEntry>,
    ) -> Result<Manifest, ManifestError> {
        let earlier = match prev {
            None => Vec::new(),
            Some(p) if p.header.schema == v.schema => p.header.earlier.clone(),
            Some(p) => {
                if p.header.earlier.len() >= MAX_EARLIER {
                    return Err(ManifestError::Successor(
                        "a dataset has at most 8 schemas (7 widenings): start a new dataset id"
                            .into(),
                    ));
                }
                let mut e = p.header.earlier.clone();
                e.push(EarlierSchema {
                    until: p.header.shard_count,
                    schema: p.header.schema.clone(),
                });
                e
            }
        };
        let tree = shards_tree(&shards);
        let header = ManifestHeader {
            format: if earlier.is_empty() { FORMAT } else { FORMAT_2 },
            dataset: v.dataset,
            publisher: v.publisher,
            version: prev.map_or(1, |p| p.header.version.saturating_add(1)),
            prev: prev.map_or(Hash32::ZERO, |p| p.header.root()),
            created_unix: v.created_unix,
            schema: v.schema,
            earlier,
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
        // One root in two schema ranges would be checked under the first
        // index that names it (review BULK-WIDEN-R1 finding 8).
        if !h.earlier.is_empty() {
            let mut seen: std::collections::BTreeMap<Hash32, usize> =
                std::collections::BTreeMap::new();
            for e in &self.shards {
                let k = h.schema_index(e.index);
                if *seen.entry(e.root).or_insert(k) != k {
                    return Err(bad("a shard root appears under two schemas"));
                }
            }
        }
        Ok(())
    }

    /// Append-only rule: same dataset, publisher and rights; the next version
    /// number; `prev` is the previous root; later in time; every earlier
    /// shard kept unchanged in place and at least one added. The schema
    /// either stays (and the earlier schemas with it, byte for byte) or
    /// widens `prev`'s (ADR-0059 amendment 5 item 3): the earlier schemas
    /// are then `prev`'s followed by exactly `prev`'s schema up to `prev`'s
    /// shard count, so no earlier shard is ever relabelled.
    pub fn check_successor(&self, prev: &Manifest) -> Result<(), ManifestError> {
        let (h, p) = (&self.header, &prev.header);
        let fail = |s: &str| Err(ManifestError::Successor(s.into()));
        if h.dataset != p.dataset || h.publisher != p.publisher {
            return fail("another dataset or publisher");
        }
        if h.rights != p.rights {
            return fail("the rights stay the same across versions");
        }
        if h.schema == p.schema {
            if h.earlier != p.earlier {
                return fail("the earlier schemas and their boundaries stay the same");
            }
        } else {
            Schema::widens(&p.schema, &h.schema).map_err(|e| {
                ManifestError::Successor(format!("a new schema must widen the previous one: {e}"))
            })?;
            if p.earlier.len() >= MAX_EARLIER {
                return fail(
                    "a dataset has at most 8 schemas (7 widenings): start a new dataset id",
                );
            }
            let relabel = h.earlier.len() != p.earlier.len() + 1
                || h.earlier[..p.earlier.len()] != p.earlier[..]
                || h.earlier[p.earlier.len()].until != p.shard_count
                || h.earlier[p.earlier.len()].schema != p.schema;
            if relabel {
                return fail("a widening keeps the earlier schemas and adds the previous schema up to the previous shard count");
            }
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
        let m = Manifest::try_from_slice(b).map_err(|e| {
            if e.to_string() == TOO_MANY_SCHEMAS {
                bad("at most 8 schemas per dataset (7 earlier): start a new dataset id")
            } else {
                ManifestError::Encoding
            }
        })?;
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

    /// `t` (int key), `v` decimal at `scale`, then `extra` nullable int
    /// columns.
    pub(crate) fn dec_schema(scale: u8, extra: usize) -> Schema {
        let mut cols = vec![
            json!({"name":"t","kind":"int"}),
            json!({"name":"v","kind":"decimal","scale":scale}),
        ];
        cols.extend(
            (0..extra).map(|i| json!({"name": format!("a{i}"), "kind":"int","nullable":true})),
        );
        Schema::from_json(&json!({"columns": cols, "key": "t"})).expect("ok")
    }

    fn nv(created: u64, schema: Schema) -> NewVersion {
        NewVersion {
            schema,
            ..new_version(created)
        }
    }

    /// Shards `0..n`, one key each.
    fn entries(n: u32) -> Vec<ShardEntry> {
        (0..n)
            .map(|i| entry(i, i128::from(i), i128::from(i)))
            .collect()
    }

    /// ADR-0059 amendment 5 item 1: a dataset that never widens keeps its
    /// format-1 bytes and root (a golden root, computed with the code before
    /// format 2 existed), and its header is the derive-style Borsh of its
    /// fields with the 84-byte tail `OnyxStorage` reads last. A format-1
    /// header with a tail, and a format-2 header without an earlier schema,
    /// are refused.
    #[test]
    fn widen_1_format_1_bytes_and_roots_are_unchanged() {
        let m = Manifest::build(new_version(100), None, vec![entry(0, 1, 5), entry(1, 5, 9)])
            .expect("ok");
        assert_eq!(m.header.format, FORMAT);
        assert_eq!(
            m.header.root().to_string(),
            "0x25f63edafd7bc982cb387794fc894b44800280aebf4e021f8f6162efdc6e9881"
        );
        let h = &m.header;
        let fields = borsh::to_vec(&(
            h.format,
            &h.dataset,
            &h.publisher,
            h.version,
            h.prev,
            h.created_unix,
            &h.schema,
            &h.rights,
            &h.rights_statement,
            (
                h.shard_count,
                h.row_count,
                h.byte_count,
                h.key_min,
                h.key_max,
                h.shards_root,
            ),
        ))
        .expect("ok");
        assert_eq!(h.to_bytes(), fields);
        assert_eq!(&fields[fields.len() - 32..], &h.shards_root.0);
        let t = fields.len() - 84;
        assert_eq!(&fields[t..t + 4], &h.shard_count.to_le_bytes());
        assert_eq!(root_of_header_bytes(&fields), h.root());
        // A successor under the same schema is format 1 too, with no tail.
        let v2 = Manifest::build(
            new_version(200),
            Some(&m),
            vec![entry(0, 1, 5), entry(1, 5, 9), entry(2, 9, 9)],
        )
        .expect("ok");
        assert_eq!((v2.header.format, v2.header.earlier.len()), (FORMAT, 0));
        assert_eq!(Manifest::from_bytes(&v2.to_bytes()).expect("ok"), v2);
        // A format-1 header with a tail: the bytes of a format-2 manifest
        // with the format byte set to 1 do not decode; a format-1 value with
        // an earlier schema does not validate (and is written with it).
        let w1 = Manifest::build(nv(100, dec_schema(2, 0)), None, entries(2)).expect("ok");
        let w2 = Manifest::build(nv(200, dec_schema(4, 0)), Some(&w1), entries(3)).expect("ok");
        assert_eq!(w2.header.format, FORMAT_2);
        let mut b = w2.to_bytes();
        b[0] = FORMAT;
        assert_eq!(Manifest::from_bytes(&b), Err(ManifestError::Encoding));
        let mut tailed = w2.clone();
        tailed.header.format = FORMAT;
        assert!(tailed.validate().is_err());
        assert_ne!(tailed.header.root(), {
            let mut plain = tailed.header.clone();
            plain.earlier.clear();
            plain.root()
        });
        // A format-2 header with no earlier schema decodes and is refused.
        let mut bare = v2.clone();
        bare.header.format = FORMAT_2;
        let bb = bare.to_bytes();
        assert_eq!(
            bb.len(),
            v2.to_bytes().len() + 4,
            "the empty list is written"
        );
        assert!(matches!(
            Manifest::from_bytes(&bb),
            Err(ManifestError::Bad(_))
        ));
        // Format 1 and 2 preimages differ in their first byte.
        assert_eq!(w2.header.to_bytes()[0], FORMAT_2);
        // An unknown format is refused.
        let mut f3 = v2.clone();
        f3.header.format = 3;
        assert!(f3.validate().is_err());
    }

    /// Item 3: a widening appends exactly the previous schema up to the
    /// previous shard count, a version under the same schema keeps the
    /// earlier schemas byte for byte, and no successor relabels a shard or
    /// moves a boundary.
    #[test]
    fn widen_4_earlier_schemas_and_boundaries_are_kept_exactly() {
        let (a, b, c) = (dec_schema(2, 0), dec_schema(4, 0), dec_schema(4, 1));
        let v1 = Manifest::build(nv(100, a.clone()), None, entries(2)).expect("v1");
        let v2 = Manifest::build(nv(200, b.clone()), Some(&v1), entries(3)).expect("v2");
        let v3 = Manifest::build(nv(300, b.clone()), Some(&v2), entries(4)).expect("v3");
        let v4 = Manifest::build(nv(400, c.clone()), Some(&v3), entries(5)).expect("v4");
        let at = |s: &Schema, until| EarlierSchema {
            until,
            schema: s.clone(),
        };
        assert_eq!(v2.header.earlier, vec![at(&a, 2)]);
        assert_eq!(v3.header.earlier, v2.header.earlier);
        assert_eq!(v4.header.earlier, vec![at(&a, 2), at(&b, 4)]);
        assert_eq!(v4.header.format, FORMAT_2);
        for (i, want, k) in [(0, &a, 0), (1, &a, 0), (2, &b, 1), (3, &b, 1), (4, &c, 2)] {
            assert_eq!(v4.header.schema_of(i), want, "shard {i}");
            assert_eq!(v4.header.schema_index(i), k, "shard {i}");
        }
        assert_eq!(v4.header.schemas().len(), 3);
        for m in [&v2, &v3, &v4] {
            assert_eq!(&Manifest::from_bytes(&m.to_bytes()).expect("decodes"), m);
        }
        // Relabellings, each a valid manifest on its own.
        let with = |m: &Manifest, e: Vec<EarlierSchema>| {
            let mut x = m.clone();
            x.header.earlier = e;
            x.validate().expect("valid on its own");
            x
        };
        let other_a = dec_schema(3, 0);
        let cases = [
            ("v3 moves the boundary", with(&v3, vec![at(&a, 1)]), &v2),
            (
                "v3 relabels the old shards",
                with(&v3, vec![at(&other_a, 2)]),
                &v2,
            ),
            (
                "v4 ends b early",
                with(&v4, vec![at(&a, 2), at(&b, 3)]),
                &v3,
            ),
            (
                "v4 changes a",
                with(&v4, vec![at(&other_a, 2), at(&b, 4)]),
                &v3,
            ),
            ("v4 drops a", with(&v4, vec![at(&b, 4)]), &v3),
            (
                "v4 adds two",
                with(&v4, vec![at(&a, 1), at(&other_a, 2), at(&b, 4)]),
                &v3,
            ),
        ];
        for (name, m, prev) in &cases {
            assert!(
                matches!(m.check_successor(prev), Err(ManifestError::Successor(_))),
                "{name}"
            );
        }
        // A new schema with no earlier schema (format 1) is a relabelling.
        let mut plain = v2.clone();
        plain.header.earlier.clear();
        plain.header.format = FORMAT;
        plain.validate().expect("valid alone");
        assert!(plain.check_successor(&v1).is_err());
        // A schema that does not widen the previous one is refused.
        assert!(Manifest::build(nv(500, dec_schema(3, 1)), Some(&v4), entries(6)).is_err());
        assert!(Manifest::build(nv(500, a.clone()), Some(&v4), entries(6)).is_err());
        // A widened version still keeps every earlier shard and adds one.
        assert!(Manifest::build(nv(500, dec_schema(5, 1)), Some(&v4), entries(5)).is_err());
    }

    /// Item 6: at most 8 schemas per dataset. The ninth is refused by the
    /// successor rule, by `validate`, and by the decoder before it reads any
    /// earlier schema (review BULK-WIDEN-R1 finding 10).
    #[test]
    fn widen_5_a_ninth_schema_is_refused() {
        let mut m = Manifest::build(nv(100, dec_schema(1, 0)), None, entries(1)).expect("v1");
        for v in 2..=8u8 {
            m = Manifest::build(
                nv(100 * u64::from(v), dec_schema(v, 0)),
                Some(&m),
                entries(u32::from(v)),
            )
            .expect("widens");
        }
        assert_eq!(m.header.earlier.len(), MAX_EARLIER);
        let e =
            Manifest::build(nv(900, dec_schema(9, 0)), Some(&m), entries(9)).expect_err("ninth");
        assert!(
            matches!(&e, ManifestError::Successor(w) if w.contains("8 schemas")),
            "{e}"
        );
        // The same schema again is still a successor.
        let same = Manifest::build(nv(900, dec_schema(8, 0)), Some(&m), entries(9)).expect("same");
        assert_eq!(same.header.earlier, m.header.earlier);
        // A hand-made ninth: refused by validate, and its bytes by the decoder.
        let mut nine = same.clone();
        nine.header.earlier.push(EarlierSchema {
            until: 8,
            schema: dec_schema(8, 0),
        });
        nine.header.schema = dec_schema(9, 0);
        assert!(nine.header.validate().is_err());
        let e = Manifest::from_bytes(&nine.to_bytes()).expect_err("refused");
        assert!(
            matches!(&e, ManifestError::Bad(w) if w.contains("8 schemas")),
            "{e}"
        );
        // A count of u32::MAX with nothing after it: refused for the count,
        // not for running out of bytes (no schema was read).
        let h = &same.header;
        let at = borsh::to_vec(&(
            h.format,
            &h.dataset,
            &h.publisher,
            h.version,
            h.prev,
            h.created_unix,
            &h.schema,
        ))
        .expect("ok")
        .len();
        let mut b = same.to_bytes()[..at].to_vec();
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        let e = Manifest::from_bytes(&b).expect_err("refused");
        assert!(
            matches!(&e, ManifestError::Bad(w) if w.contains("8 schemas")),
            "{e}"
        );
        assert!(nine.check_successor(&same).is_err());
    }

    /// Format 2 `validate`: `until` strictly increasing within
    /// `1..shard_count`, each schema widening the next, the last widening the
    /// newest, and no shard root under two schemas (review BULK-WIDEN-R1
    /// finding 8).
    #[test]
    fn format_2_validate_checks_the_schema_chain() {
        let (a, b, c) = (dec_schema(2, 0), dec_schema(4, 0), dec_schema(4, 1));
        let v1 = Manifest::build(nv(100, a.clone()), None, entries(2)).expect("v1");
        let v2 = Manifest::build(nv(200, b.clone()), Some(&v1), entries(3)).expect("v2");
        let v3 = Manifest::build(nv(300, c.clone()), Some(&v2), entries(4)).expect("v3");
        v3.validate().expect("ok");
        let at = |s: &Schema, until| EarlierSchema {
            until,
            schema: s.clone(),
        };
        let with = |e: Vec<EarlierSchema>| {
            let mut x = v3.clone();
            x.header.earlier = e;
            x
        };
        for (name, m) in [
            ("until 0", with(vec![at(&a, 0), at(&b, 3)])),
            ("until shard_count", with(vec![at(&a, 2), at(&b, 4)])),
            ("until not increasing", with(vec![at(&a, 2), at(&b, 2)])),
            ("a does not widen to a", with(vec![at(&a, 1), at(&a, 2)])),
            ("c does not widen to c", with(vec![at(&a, 2), at(&c, 3)])),
            ("b before a", with(vec![at(&b, 2), at(&a, 3)])),
        ] {
            assert!(m.validate().is_err(), "{name}");
        }
        // One root in two schema ranges: shards 1 (schema a) and 2 (schema b)
        // with the same root and key.
        let mut shards = entries(4);
        shards[2].root = shards[1].root;
        shards[2].key_min = 1;
        shards[2].key_max = 1;
        let mut twice = v3.clone();
        twice.header.shards_root = shards_tree(&shards).root();
        twice.shards = shards;
        let e = twice.validate().expect_err("refused");
        assert!(e.to_string().contains("two schemas"), "{e}");
        // Within one range a repeated root stays allowed (as in format 1):
        // shards 0 and 1 (both schema a).
        let mut same_range = entries(4);
        same_range[1].root = same_range[0].root;
        same_range[1].key_min = 0;
        same_range[1].key_max = 0;
        let mut x = v3.clone();
        x.header.shards_root = shards_tree(&same_range).root();
        x.shards = same_range;
        x.validate().expect("one range");
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
