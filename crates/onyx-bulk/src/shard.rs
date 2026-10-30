//! Shards: bounded batches of rows sorted by key, committed by the ADR-0003
//! Merkle root over their rows.
//!
//! A shard file is the Borsh `Vec<Vec<u8>>` of its rows (a `u32` row count,
//! then each row as a `u32` length and its bytes), the same layout as a
//! recipe row batch. [`split_rows`] reads it without allocating per row and
//! refuses a count or a length over the caps before reading further, so a
//! small file cannot claim a huge decode (THREATS CL5-4).

use crate::row::{decode_row, key_of, MAX_ROW_BYTES};
use crate::schema::Schema;
use onyx_core::Hash32;
use onyx_merkle::MerkleTree;

/// Largest shard file (bytes, framing included).
pub const MAX_SHARD_BYTES: u64 = 64 << 20;
/// Most rows in one shard (bounds the Merkle tree a node builds: 2^18 rows,
/// about 16 MiB of hashes).
pub const MAX_SHARD_ROWS: u64 = 1 << 18;
/// Size the publisher tool aims for by default: 8 MiB uploads in well under
/// the proxy's 10 s body deadline on a 10 Mbit/s line.
pub const DEFAULT_TARGET_SHARD_BYTES: u64 = 8 << 20;

/// A shard that does not hold.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShardError {
    /// Over [`MAX_SHARD_BYTES`].
    #[error("shard of {0} bytes is over {MAX_SHARD_BYTES}")]
    TooLarge(u64),
    /// Zero rows or over [`MAX_SHARD_ROWS`].
    #[error("shard row count {0} is not 1..={MAX_SHARD_ROWS}")]
    RowCount(u64),
    /// The framing does not parse (short, trailing bytes, a row over the cap).
    #[error("shard framing: {0}")]
    Framing(&'static str),
    /// A row does not fit the schema.
    #[error("row {index}: {why}")]
    Row {
        /// Row index in the shard.
        index: u64,
        /// What is wrong.
        why: String,
    },
    /// Rows are not sorted by key.
    #[error("row {0} has a smaller key than the row before it")]
    Unsorted(u64),
}

/// What a manifest says of a shard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardSummary {
    /// Merkle root over the rows.
    pub root: Hash32,
    /// Rows.
    pub rows: u64,
    /// File bytes.
    pub bytes: u64,
    /// Key of the first row.
    pub key_min: i128,
    /// Key of the last row.
    pub key_max: i128,
}

/// Bytes the shard file of rows with these lengths takes.
pub fn file_len<I: IntoIterator<Item = usize>>(row_lens: I) -> u64 {
    row_lens
        .into_iter()
        .fold(4u64, |a, l| a.saturating_add(4).saturating_add(l as u64))
}

/// The shard file of `rows`.
pub fn encode_shard<R: AsRef<[u8]>>(rows: &[R]) -> Vec<u8> {
    let len = usize::try_from(file_len(rows.iter().map(|r| r.as_ref().len()))).unwrap_or(0);
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    for r in rows {
        let r = r.as_ref();
        out.extend_from_slice(&(r.len() as u32).to_le_bytes());
        out.extend_from_slice(r);
    }
    out
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// The rows of a shard file, as slices into it. Checks the caps first: file
/// size, row count, each row's length, and that the framing ends exactly at
/// the end of the file.
pub fn split_rows(bytes: &[u8]) -> Result<Vec<&[u8]>, ShardError> {
    if bytes.len() as u64 > MAX_SHARD_BYTES {
        return Err(ShardError::TooLarge(bytes.len() as u64));
    }
    let n = u64::from(u32_at(bytes, 0).ok_or(ShardError::Framing("no row count"))?);
    if n == 0 || n > MAX_SHARD_ROWS {
        return Err(ShardError::RowCount(n));
    }
    // Every row takes at least its 4-byte length: a count the file cannot
    // hold is refused before any allocation.
    if n.saturating_mul(4).saturating_add(4) > bytes.len() as u64 {
        return Err(ShardError::Framing("row count larger than the file"));
    }
    let mut rows = Vec::with_capacity(n as usize);
    let mut at = 4usize;
    for _ in 0..n {
        let len = u32_at(bytes, at).ok_or(ShardError::Framing("short row length"))? as usize;
        if len > MAX_ROW_BYTES {
            return Err(ShardError::Framing("row over the row cap"));
        }
        at += 4;
        let end = at
            .checked_add(len)
            .filter(|e| *e <= bytes.len())
            .ok_or(ShardError::Framing("short row"))?;
        rows.push(&bytes[at..end]);
        at = end;
    }
    if at != bytes.len() {
        return Err(ShardError::Framing("bytes after the last row"));
    }
    Ok(rows)
}

/// A checked shard: its summary, rows (slices into the file) and their keys.
#[derive(Debug)]
pub struct CheckedShard<'a> {
    /// Root, counts and key range.
    pub summary: ShardSummary,
    /// Row bytes, in order.
    pub rows: Vec<&'a [u8]>,
    /// Row keys, in order (non-decreasing).
    pub keys: Vec<i128>,
    /// The Merkle tree over the rows.
    pub tree: MerkleTree,
}

/// Check a shard file against `schema`: framing and caps, every row decodes
/// and fits, keys never go down. Returns the root it commits to.
pub fn check_shard<'a>(schema: &Schema, bytes: &'a [u8]) -> Result<CheckedShard<'a>, ShardError> {
    let rows = split_rows(bytes)?;
    let mut keys = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let cells = decode_row(schema, r).map_err(|e| ShardError::Row {
            index: i as u64,
            why: e.to_string(),
        })?;
        let k = key_of(schema, &cells).ok_or(ShardError::Row {
            index: i as u64,
            why: "no key".into(),
        })?;
        if keys.last().is_some_and(|p| k < *p) {
            return Err(ShardError::Unsorted(i as u64));
        }
        keys.push(k);
    }
    let tree = MerkleTree::from_rows(&rows);
    let summary = ShardSummary {
        root: tree.root(),
        rows: rows.len() as u64,
        bytes: bytes.len() as u64,
        key_min: keys.first().copied().unwrap_or_default(),
        key_max: keys.last().copied().unwrap_or_default(),
    };
    Ok(CheckedShard {
        summary,
        rows,
        keys,
        tree,
    })
}

/// Cuts a sorted stream of encoded rows into shards no larger than a target
/// (at most [`MAX_SHARD_BYTES`]) and [`MAX_SHARD_ROWS`] rows.
pub struct ShardBuilder {
    target: u64,
    rows: Vec<Vec<u8>>,
    bytes: u64,
    last_key: Option<i128>,
    seen: u64,
}

impl ShardBuilder {
    /// A builder aiming at `target` bytes per shard (clamped to the cap).
    pub fn new(target: u64) -> Self {
        ShardBuilder {
            target: target.clamp(1, MAX_SHARD_BYTES),
            rows: Vec::new(),
            bytes: 4,
            last_key: None,
            seen: 0,
        }
    }

    /// Add one encoded row with its key. Returns the finished shard's rows
    /// when this row did not fit in the current one (the row starts the
    /// next). Rows must come sorted by key.
    pub fn push(&mut self, row: Vec<u8>, key: i128) -> Result<Option<Vec<Vec<u8>>>, ShardError> {
        if row.len() > MAX_ROW_BYTES {
            return Err(ShardError::Framing("row over the row cap"));
        }
        if self.last_key.is_some_and(|p| key < p) {
            return Err(ShardError::Unsorted(self.seen));
        }
        self.last_key = Some(key);
        self.seen += 1;
        let add = 4 + row.len() as u64;
        let full = !self.rows.is_empty()
            && (self.bytes + add > self.target || self.rows.len() as u64 >= MAX_SHARD_ROWS);
        let out = if full {
            self.bytes = 4;
            Some(std::mem::take(&mut self.rows))
        } else {
            None
        };
        self.bytes += add;
        self.rows.push(row);
        Ok(out)
    }

    /// The last shard's rows, if any.
    pub fn finish(self) -> Option<Vec<Vec<u8>>> {
        (!self.rows.is_empty()).then_some(self.rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row::{encode_row, Cell};
    use serde_json::json;

    fn schema() -> Schema {
        Schema::from_json(
            &json!({"columns":[{"name":"t","kind":"int"},{"name":"s","kind":"text"}],"key":"t"}),
        )
        .expect("ok")
    }

    fn row(t: i128, s: &str) -> Vec<u8> {
        encode_row(&schema(), &[Cell::Int(t), Cell::Text(s.into())]).expect("ok")
    }

    #[test]
    fn round_trip_and_root() {
        let rows = vec![row(1, "a"), row(2, "b"), row(2, "c")];
        let file = encode_shard(&rows);
        assert_eq!(file.len() as u64, file_len(rows.iter().map(Vec::len)));
        // The same bytes as a Borsh row batch (the CAS `Rows` layout).
        assert_eq!(file, borsh::to_vec(&rows).expect("ok"));
        let c = check_shard(&schema(), &file).expect("ok");
        assert_eq!(c.summary.rows, 3);
        assert_eq!((c.summary.key_min, c.summary.key_max), (1, 2));
        assert_eq!(c.summary.root, MerkleTree::from_rows(&rows).root());
        assert_eq!(c.summary.bytes, file.len() as u64);
    }

    #[test]
    fn refuses_bad_files() {
        let s = schema();
        let unsorted = encode_shard(&[row(2, "a"), row(1, "b")]);
        assert_eq!(
            check_shard(&s, &unsorted).expect_err("refused"),
            ShardError::Unsorted(1)
        );
        let mut trailing = encode_shard(&[row(1, "a")]);
        trailing.push(9);
        assert!(matches!(
            check_shard(&s, &trailing),
            Err(ShardError::Framing(_))
        ));
        assert_eq!(
            split_rows(&0u32.to_le_bytes()).expect_err("refused"),
            ShardError::RowCount(0)
        );
        // A 12-byte file claiming 2^18 rows: refused before allocating.
        let mut claim = (MAX_SHARD_ROWS as u32).to_le_bytes().to_vec();
        claim.extend_from_slice(&[0; 8]);
        assert!(matches!(split_rows(&claim), Err(ShardError::Framing(_))));
        let over = ((MAX_SHARD_ROWS + 1) as u32).to_le_bytes();
        assert_eq!(
            split_rows(&over).expect_err("refused"),
            ShardError::RowCount(MAX_SHARD_ROWS + 1)
        );
        // A row length over the row cap.
        let mut big = 1u32.to_le_bytes().to_vec();
        big.extend_from_slice(&((MAX_ROW_BYTES + 1) as u32).to_le_bytes());
        big.extend_from_slice(&vec![0; MAX_ROW_BYTES + 1]);
        assert!(matches!(split_rows(&big), Err(ShardError::Framing(_))));
        // A row that is not a row of the schema.
        let junk = encode_shard(&[vec![1u8, 2, 3]]);
        assert!(matches!(
            check_shard(&s, &junk),
            Err(ShardError::Row { index: 0, .. })
        ));
    }

    #[test]
    fn size_cap_is_exact() {
        // One byte over MAX_SHARD_BYTES is refused before parsing.
        let over = vec![0u8; (MAX_SHARD_BYTES + 1) as usize];
        assert_eq!(
            split_rows(&over).expect_err("refused"),
            ShardError::TooLarge(MAX_SHARD_BYTES + 1)
        );
        // A file of exactly MAX_SHARD_BYTES parses: 1,024 rows of about 64 KiB.
        let n = 1024u64;
        let body = MAX_SHARD_BYTES - 4 - 4 * n;
        let len = body.div_ceil(n);
        let last = body - len * (n - 1);
        assert!(len as usize <= MAX_ROW_BYTES);
        let mut rows: Vec<Vec<u8>> = (0..n - 1).map(|_| vec![7u8; len as usize]).collect();
        rows.push(vec![7u8; last as usize]);
        let file = encode_shard(&rows);
        assert_eq!(file.len() as u64, MAX_SHARD_BYTES);
        assert_eq!(split_rows(&file).expect("ok").len() as u64, n);
    }

    #[test]
    fn builder_cuts_at_bytes_and_rows() {
        let r = row(1, "abcdef");
        let per = 4 + r.len() as u64;
        // Target fits exactly three rows.
        let mut b = ShardBuilder::new(4 + 3 * per);
        let mut shards = Vec::new();
        for _ in 0..7 {
            if let Some(s) = b.push(r.clone(), 1).expect("ok") {
                shards.push(s);
            }
        }
        shards.extend(b.finish());
        assert_eq!(
            shards.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![3, 3, 1]
        );
        for s in &shards {
            assert!(encode_shard(s).len() as u64 <= 4 + 3 * per);
        }
        // The row cap binds with a huge target.
        let mut b = ShardBuilder::new(MAX_SHARD_BYTES);
        let tiny = row(1, "");
        let mut first = None;
        for i in 0..=MAX_SHARD_ROWS {
            if let Some(s) = b.push(tiny.clone(), 1).expect("ok") {
                first = Some((i, s.len()));
                break;
            }
        }
        assert_eq!(first, Some((MAX_SHARD_ROWS, MAX_SHARD_ROWS as usize)));
        // Sorted input only.
        let mut b = ShardBuilder::new(1 << 20);
        b.push(row(5, ""), 5).expect("ok");
        assert!(b.push(row(4, ""), 4).is_err());
    }
}
