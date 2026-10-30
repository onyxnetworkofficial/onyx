//! Content-addressed storage (ADR-0018). Everything a shard pins is stored
//! under the hash that pins it, and every read rechecks that hash, so a store
//! can lose data but never serve wrong data.
//!
//! Keys use the same hash the protocol pins:
//! - recipe: `keccak256(wasm)` (the recipe id);
//! - label set: [`LabelSet::content_hash`];
//! - attestation: [`Attestation::content_hash`];
//! - params and other blobs: `keccak256(bytes)`;
//! - row batch: the Merkle root over the rows (`onyx_merkle`);
//! - published bulk shard (CL5, ADR-0059): the Merkle root over its rows, with
//!   its own cap ([`onyx_bulk::shard::MAX_SHARD_BYTES`]).

use borsh::BorshDeserialize;
use onyx_core::{keccak256, to_canonical_bytes, Hash32};
use onyx_labels::attest::Attestation;
use onyx_labels::LabelSet;
use onyx_merkle::MerkleTree;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Kind of stored object; part of the key so a hash of one kind is never
/// served as another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Recipe WASM bytes.
    Recipe,
    /// Borsh `LabelSet`.
    LabelSet,
    /// Borsh `Attestation`.
    Attestation,
    /// Raw bytes keyed by `keccak256` (params, source documents).
    Blob,
    /// Borsh `Vec<Vec<u8>>` of output rows, keyed by their Merkle root.
    Rows,
    /// A publisher's bulk shard (CL5, ADR-0059): the same layout as `Rows`,
    /// keyed by the Merkle root over its rows, at most 64 MiB and 2^18 rows.
    Shard,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Recipe => "recipe",
            Kind::LabelSet => "labels",
            Kind::Attestation => "attest",
            Kind::Blob => "blob",
            Kind::Rows => "rows",
            Kind::Shard => "bulk",
        }
    }
}

/// Largest encoded row batch. A batch stores each row with a 4-byte length,
/// and every row is at least 8 bytes, so a batch is under 1.5x the recipe
/// output it came from; this covers the host's 16 MiB output cap with room to
/// spare, so an honest result always fits (L19-F1).
pub const MAX_ROWS_BYTES: usize = (24 << 20) + 4096;
/// Largest stored object of any other kind (label sets, attestations,
/// recipes, params, documents).
pub const MAX_OBJECT_BYTES: usize = 32 << 20;

/// Cap for an object of `kind`.
pub fn max_bytes(kind: Kind) -> usize {
    match kind {
        Kind::Rows => MAX_ROWS_BYTES,
        Kind::Shard => onyx_bulk::shard::MAX_SHARD_BYTES as usize,
        _ => MAX_OBJECT_BYTES,
    }
}

/// True if `rows` encode to a batch the store accepts. Miners check this
/// before committing (L19-F1).
pub fn rows_storable(rows: &[Vec<u8>]) -> bool {
    !rows.is_empty()
        && rows.iter().all(|r| r.len() >= 8)
        && rows
            .iter()
            .try_fold(4usize, |a, r| a.checked_add(4)?.checked_add(r.len()))
            .is_some_and(|n| n <= MAX_ROWS_BYTES)
}

/// The key an object of `kind` with these bytes must be stored under, or
/// `None` if the bytes do not decode as that kind.
pub fn key_of(kind: Kind, bytes: &[u8]) -> Option<Hash32> {
    if bytes.len() > max_bytes(kind) {
        return None;
    }
    match kind {
        Kind::Recipe | Kind::Blob => Some(keccak256(bytes)),
        Kind::LabelSet => LabelSet::try_from_slice(bytes)
            .ok()
            .map(|s| s.content_hash()),
        // Oversized attestations are not storable, so a set citing one never
        // opens a shard (L16-F7).
        Kind::Attestation => Attestation::try_from_slice(bytes)
            .ok()
            .filter(|a| {
                a.statement.len() <= onyx_labels::attest::MAX_STATEMENT_LEN
                    && a.address_sig.len() <= onyx_labels::attest::MAX_ADDRESS_SIG_LEN
            })
            .map(|a| a.content_hash()),
        // Every honest row is a Borsh `OutputRow` (two u32 lengths), so at least
        // 8 bytes; this bounds the row count of a batch (L20-F3).
        Kind::Rows => Vec::<Vec<u8>>::try_from_slice(bytes)
            .ok()
            .filter(|rows| !rows.is_empty() && rows.iter().all(|r| r.len() >= 8))
            .map(|rows| MerkleTree::from_rows(&rows).root()),
        // A bulk shard's framing is read with its caps (row count, row
        // length) before anything is allocated per row (THREATS CL5-4).
        Kind::Shard => onyx_bulk::shard::split_rows(bytes)
            .ok()
            .map(|rows| MerkleTree::from_rows(&rows).root()),
    }
}

/// CAS error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CasError {
    /// The bytes do not decode as the kind given.
    #[error("bytes do not decode as {0:?}")]
    BadObject(Kind),
    /// Storage failure.
    #[error("storage: {0}")]
    Io(String),
}

/// A content-addressed store.
pub trait Cas {
    /// Store raw bytes of `kind`; returns the key they are stored under.
    fn put(&self, kind: Kind, bytes: &[u8]) -> Result<Hash32, CasError>;
    /// Bytes for `key`, only if they still hash to `key` (integrity on read).
    fn get(&self, kind: Kind, key: &Hash32) -> Option<Vec<u8>>;
    /// True if `get` would return the object.
    fn has(&self, kind: Kind, key: &Hash32) -> bool {
        self.get(kind, key).is_some()
    }
}

/// Typed helpers on any store.
pub trait CasExt: Cas {
    /// Store a label set.
    fn put_label_set(&self, set: &LabelSet) -> Result<Hash32, CasError> {
        let b = to_canonical_bytes(set).map_err(|e| CasError::Io(e.to_string()))?;
        self.put(Kind::LabelSet, &b)
    }
    /// Store an attestation.
    fn put_attestation(&self, a: &Attestation) -> Result<Hash32, CasError> {
        let b = to_canonical_bytes(a).map_err(|e| CasError::Io(e.to_string()))?;
        self.put(Kind::Attestation, &b)
    }
    /// Store a row batch; returns its Merkle root.
    fn put_rows(&self, rows: &[Vec<u8>]) -> Result<Hash32, CasError> {
        let b = to_canonical_bytes(&rows.to_vec()).map_err(|e| CasError::Io(e.to_string()))?;
        self.put(Kind::Rows, &b)
    }
    /// A stored label set.
    fn label_set(&self, key: &Hash32) -> Option<LabelSet> {
        self.get(Kind::LabelSet, key)
            .and_then(|b| LabelSet::try_from_slice(&b).ok())
    }
    /// A stored attestation.
    fn attestation(&self, key: &Hash32) -> Option<Attestation> {
        self.get(Kind::Attestation, key)
            .and_then(|b| Attestation::try_from_slice(&b).ok())
    }
    /// A stored row batch.
    fn rows(&self, root: &Hash32) -> Option<Vec<Vec<u8>>> {
        self.get(Kind::Rows, root)
            .and_then(|b| Vec::<Vec<u8>>::try_from_slice(&b).ok())
    }
}

impl<T: Cas + ?Sized> CasExt for T {}

/// In-memory store (tests, simulation).
#[derive(Default)]
pub struct MemCas {
    map: Mutex<BTreeMap<(Kind, Hash32), Vec<u8>>>,
}

impl MemCas {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Overwrite a stored object without checks (tests of integrity on read).
    #[cfg(test)]
    pub(crate) fn corrupt(&self, kind: Kind, key: Hash32, bytes: Vec<u8>) {
        if let Ok(mut m) = self.map.lock() {
            m.insert((kind, key), bytes);
        }
    }

    /// Remove an object (tests of availability).
    pub fn remove(&self, kind: Kind, key: &Hash32) {
        if let Ok(mut m) = self.map.lock() {
            m.remove(&(kind, *key));
        }
    }
}

impl Cas for MemCas {
    fn put(&self, kind: Kind, bytes: &[u8]) -> Result<Hash32, CasError> {
        let key = key_of(kind, bytes).ok_or(CasError::BadObject(kind))?;
        self.map
            .lock()
            .map_err(|e| CasError::Io(e.to_string()))?
            .insert((kind, key), bytes.to_vec());
        Ok(key)
    }

    fn get(&self, kind: Kind, key: &Hash32) -> Option<Vec<u8>> {
        let b = self.map.lock().ok()?.get(&(kind, *key)).cloned()?;
        (key_of(kind, &b) == Some(*key)).then_some(b)
    }
}

/// Directory-backed store: `<dir>/<kind>/<hex key>`.
pub struct FsCas {
    dir: PathBuf,
}

impl FsCas {
    /// Store rooted at `dir` (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, kind: Kind, key: &Hash32) -> PathBuf {
        self.dir.join(kind.tag()).join(hex::encode(key.0))
    }

    /// The file an object is stored in (storage tiers drop row batches
    /// through it, ADR-0052).
    pub(crate) fn object_path(&self, kind: Kind, key: &Hash32) -> PathBuf {
        self.path(kind, key)
    }
}

impl Cas for FsCas {
    fn put(&self, kind: Kind, bytes: &[u8]) -> Result<Hash32, CasError> {
        let key = key_of(kind, bytes).ok_or(CasError::BadObject(kind))?;
        let p = self.path(kind, &key);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CasError::Io(e.to_string()))?;
        }
        if self.get(kind, &key).is_some() {
            return Ok(key);
        }
        // Write a uniquely named new file, sync, then rename, so a reader never
        // sees a half-written object and writers never share a temp file; the
        // temp name is created fresh (no following a planted link). A leftover
        // temp file from a crash is skipped, and every error path removes the
        // temp file (L18-F6, L19-F5).
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut last_err = String::new();
        for _ in 0..16 {
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tmp = p.with_extension(format!("{}.{nanos}.{n}.tmp", std::process::id()));
            let mut f = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
            {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(CasError::Io(e.to_string())),
            };
            use std::io::Write;
            let written = f
                .write_all(bytes)
                .and_then(|()| f.sync_all())
                .and_then(|()| std::fs::rename(&tmp, &p));
            match written {
                Ok(()) => return Ok(key),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    if self.get(kind, &key).is_some() {
                        return Ok(key);
                    }
                    last_err = e.to_string();
                }
            }
        }
        Err(CasError::Io(format!(
            "could not write {}: {last_err}",
            p.display()
        )))
    }

    fn get(&self, kind: Kind, key: &Hash32) -> Option<Vec<u8>> {
        // Open without following links and without blocking (a FIFO cannot
        // hang us), then check type and size on the open handle, so nothing can
        // be swapped in between check and read (L19-F6, L20-F6).
        let path = self.path(kind, key);
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let f = opts.open(&path).ok()?;
        let meta = f.metadata().ok()?;
        if !meta.is_file() || meta.len() > max_bytes(kind) as u64 {
            return None;
        }
        let mut b = Vec::new();
        use std::io::Read;
        f.take(max_bytes(kind) as u64 + 1)
            .read_to_end(&mut b)
            .ok()?;
        (key_of(kind, &b) == Some(*key)).then_some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_integrity_on_read() {
        let cas = MemCas::new();
        let k = cas.put(Kind::Blob, b"params").expect("put");
        assert_eq!(k, keccak256(b"params"));
        assert_eq!(cas.get(Kind::Blob, &k), Some(b"params".to_vec()));
        // Same hash under another kind is not served.
        assert!(!cas.has(Kind::Recipe, &k));
        // Corrupted bytes are never served.
        cas.corrupt(Kind::Blob, k, b"other".to_vec());
        assert_eq!(cas.get(Kind::Blob, &k), None);
        // Rows are keyed by their Merkle root; an empty batch is not an object.
        let rows = vec![b"row one!".to_vec(), b"row two!".to_vec()];
        let root = cas.put_rows(&rows).expect("rows");
        assert_eq!(root, MerkleTree::from_rows(&rows).root());
        assert_eq!(cas.rows(&root), Some(rows));
        assert_eq!(cas.put_rows(&[]), Err(CasError::BadObject(Kind::Rows)));
        assert_eq!(
            cas.put(Kind::LabelSet, b"not borsh"),
            Err(CasError::BadObject(Kind::LabelSet))
        );
    }

    /// L16-F7: an oversized attestation is not storable, so a set citing one
    /// cannot open a shard.
    #[test]
    fn oversized_attestation_not_storable() {
        let a = Attestation {
            statement: "x".repeat(onyx_labels::attest::MAX_STATEMENT_LEN + 1),
            address_sig: String::new(),
            entity_key: [0; 20],
            entity_sig: [0; 65],
        };
        assert_eq!(
            MemCas::new().put_attestation(&a),
            Err(CasError::BadObject(Kind::Attestation))
        );
    }

    /// L19-F1: any honest recipe output fits the row cap; the miner checks first.
    #[test]
    fn honest_outputs_fit_the_row_cap() {
        // Worst case for the overhead: 8-byte rows (empty key and value) filling 16 MiB of output.
        let rows = vec![vec![0u8; 8]; (16 << 20) / 8];
        assert!(rows_storable(&rows));
        assert!(!rows_storable(&[]));
        // L20-F3: rows shorter than an encoded OutputRow are never honest.
        assert!(!rows_storable(&[vec![]]));
        assert_eq!(
            MemCas::new().put_rows(&[vec![], vec![]]),
            Err(CasError::BadObject(Kind::Rows))
        );
        assert!(!rows_storable(&[vec![0u8; MAX_ROWS_BYTES]]));
    }

    /// L19-F5, L19-F6: a leftover temp file does not block writes; oversized or
    /// non-regular files are not read.
    #[test]
    fn fs_store_survives_leftovers_and_refuses_big_files() {
        let dir = std::env::temp_dir().join(format!("onyx-cas-test2-{}", std::process::id()));
        let cas = FsCas::new(&dir);
        let k = keccak256(b"blob");
        let p = cas.path(Kind::Blob, &k);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
        for n in 0..4 {
            std::fs::write(
                p.with_extension(format!("{}.0.{n}.tmp", std::process::id())),
                b"x",
            )
            .expect("leftover");
        }
        assert_eq!(cas.put(Kind::Blob, b"blob"), Ok(k));
        assert_eq!(cas.get(Kind::Blob, &k), Some(b"blob".to_vec()));
        let big = keccak256(b"big");
        let f = std::fs::File::create(cas.path(Kind::Blob, &big)).expect("big");
        f.set_len(MAX_OBJECT_BYTES as u64 + 1).expect("sparse");
        assert_eq!(cas.get(Kind::Blob, &big), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CL5 (ADR-0059, THREATS CL5-1, CL5-4): a bulk shard is keyed by the
    /// Merkle root over its rows under its own kind and cap; a file that is not
    /// a shard (a lying row count, trailing bytes) is not an object, and a
    /// shard is never served as a recipe row batch.
    #[test]
    fn bulk_shards_have_their_own_kind_and_cap() {
        let cas = MemCas::new();
        let rows = vec![b"a".to_vec(), b"bc".to_vec()];
        let file = onyx_bulk::shard::encode_shard(&rows);
        let k = cas.put(Kind::Shard, &file).expect("shard");
        assert_eq!(k, MerkleTree::from_rows(&rows).root());
        assert_eq!(cas.get(Kind::Shard, &k), Some(file.clone()));
        assert!(!cas.has(Kind::Rows, &k));
        let mut trailing = file.clone();
        trailing.push(0);
        assert_eq!(
            cas.put(Kind::Shard, &trailing),
            Err(CasError::BadObject(Kind::Shard))
        );
        let mut lying = file;
        lying[0] = 0xff;
        assert_eq!(
            cas.put(Kind::Shard, &lying),
            Err(CasError::BadObject(Kind::Shard))
        );
        assert_eq!(max_bytes(Kind::Shard), 64 << 20);
        assert_eq!(key_of(Kind::Shard, &vec![0u8; (64 << 20) + 1]), None);
    }

    #[test]
    fn fs_store_round_trip() {
        let dir = std::env::temp_dir().join(format!("onyx-cas-test-{}", std::process::id()));
        let cas = FsCas::new(&dir);
        let k = cas.put(Kind::Recipe, b"\0asm").expect("put");
        assert_eq!(cas.get(Kind::Recipe, &k), Some(b"\0asm".to_vec()));
        std::fs::write(dir.join("recipe").join(hex::encode(k.0)), b"tampered").expect("write");
        assert_eq!(cas.get(Kind::Recipe, &k), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
