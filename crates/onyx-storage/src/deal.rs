//! What `OnyxStorage.createDeal` reads from a manifest and its anchor.
//!
//! The deal names the manifest by its ADR-0059 root, `keccak256(
//! "onyx-bulk-manifest-v1" ‖ borsh(header))`, which the contract recomputes
//! from the header bytes. The Borsh header ends with fixed-width fields
//! (`shard_count u32, row_count u64, byte_count u64, key_min i128,
//! key_max i128, shards_root [u8; 32]`, little-endian), so the contract reads
//! them from its last [`TAIL_BYTES`] bytes without parsing the rest; the
//! dataset and publisher ids are the two strings right after the format
//! byte. The anchor is the signed payload the book recorded: the contract
//! checks it names this publisher, the metric `bulk.manifest`, the subject
//! `Aggregate(dataset)` and the value `Bytes(root)`, then asks the book that
//! it recorded and accepted it and that its key is not suspended.

use onyx_bulk::anchor::ANCHOR_METRIC;
use onyx_bulk::manifest::{FORMAT, MANIFEST_DOMAIN};
use onyx_core::{keccak256, Hash32};

/// Bytes of the fixed-width tail of a Borsh `ManifestHeader`.
pub const TAIL_BYTES: usize = 4 + 8 + 8 + 16 + 16 + 32;

/// A header or payload `createDeal` refuses.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DealError {
    /// Not a format-1 header of the right shape.
    #[error("manifest header: {0}")]
    Header(&'static str),
    /// The payload is not the anchor of this header.
    #[error("anchor payload: {0}")]
    Payload(&'static str),
}

/// The fields `createDeal` takes from a header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderTail {
    /// The manifest root.
    pub manifest_root: Hash32,
    /// Dataset id bytes.
    pub dataset: Vec<u8>,
    /// Publisher source id bytes.
    pub publisher: Vec<u8>,
    /// Shards listed.
    pub shard_count: u32,
    /// Rows.
    pub row_count: u64,
    /// Bytes of all shard files.
    pub byte_count: u64,
    /// Merkle root over the shard entries.
    pub shards_root: Hash32,
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The manifest root of Borsh header bytes.
pub fn manifest_root(header: &[u8]) -> Hash32 {
    let mut b = MANIFEST_DOMAIN.to_vec();
    b.extend_from_slice(header);
    keccak256(&b)
}

/// Read a Borsh header the way `OnyxStorage._readHeader` does: format byte
/// 1, the dataset and publisher strings, and the tail. Refuses a shard
/// count of zero and a header too short for its own strings and tail.
pub fn read_header(header: &[u8]) -> Result<HeaderTail, DealError> {
    if header.first() != Some(&FORMAT) {
        return Err(DealError::Header("format"));
    }
    let dl = le_u32(header, 1).ok_or(DealError::Header("short"))? as usize;
    let d_end = 5usize.checked_add(dl).ok_or(DealError::Header("short"))?;
    let pl = le_u32(header, d_end).ok_or(DealError::Header("short"))? as usize;
    let p_start = d_end + 4;
    let p_end = p_start.checked_add(pl).ok_or(DealError::Header("short"))?;
    // The rest of the header (version, prev, time, schema, rights, statement)
    // lies between the publisher and the tail.
    if p_end
        .checked_add(TAIL_BYTES)
        .is_none_or(|e| e > header.len())
    {
        return Err(DealError::Header("short"));
    }
    if dl == 0 || pl == 0 || dl > 255 || pl > 255 {
        return Err(DealError::Header("ids"));
    }
    let t = header.len() - TAIL_BYTES;
    let shard_count = le_u32(header, t).ok_or(DealError::Header("short"))?;
    let row_count = le_u64(header, t + 4).ok_or(DealError::Header("short"))?;
    let byte_count = le_u64(header, t + 12).ok_or(DealError::Header("short"))?;
    let mut sr = [0u8; 32];
    sr.copy_from_slice(&header[header.len() - 32..]);
    // Every shard holds at least 8 bytes (4 + 4 per row, one row at least; review STORAGE-R2-5).
    if shard_count == 0
        || shard_count > onyx_bulk::manifest::MAX_SHARDS
        || byte_count == 0
        || byte_count < 8 * u64::from(shard_count)
    {
        return Err(DealError::Header("empty"));
    }
    Ok(HeaderTail {
        manifest_root: manifest_root(header),
        dataset: header[5..d_end].to_vec(),
        publisher: header[p_start..p_end].to_vec(),
        shard_count,
        row_count,
        byte_count,
        shards_root: Hash32(sr),
    })
}

/// The anchor payload's layout for `t` under `key` with the given time and
/// sequence (what the publisher signed, ADR-0059 item 3).
pub fn anchor_payload(t: &HeaderTail, key: &Hash32, observed: u64, sequence: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(t.publisher.len() as u32).to_le_bytes());
    p.extend_from_slice(&t.publisher);
    p.extend_from_slice(&key.0);
    p.extend_from_slice(&(ANCHOR_METRIC.len() as u32).to_le_bytes());
    p.extend_from_slice(ANCHOR_METRIC.as_bytes());
    p.push(3); // Subject::Aggregate
    p.extend_from_slice(&(t.dataset.len() as u32).to_le_bytes());
    p.extend_from_slice(&t.dataset);
    p.push(4); // Value::Bytes
    p.extend_from_slice(&32u32.to_le_bytes());
    p.extend_from_slice(&t.manifest_root.0);
    p.extend_from_slice(&observed.to_le_bytes());
    p.extend_from_slice(&sequence.to_le_bytes());
    p
}

/// Check `payload` the way `OnyxStorage._checkPayload` does: exactly
/// [`anchor_payload`] for some key, time and sequence. Returns the payload
/// digest the book records it under.
pub fn check_payload(t: &HeaderTail, payload: &[u8]) -> Result<Hash32, DealError> {
    let want_len = 4
        + t.publisher.len()
        + 32
        + 4
        + ANCHOR_METRIC.len()
        + 1
        + 4
        + t.dataset.len()
        + 1
        + 4
        + 32
        + 16;
    if payload.len() != want_len {
        return Err(DealError::Payload("length"));
    }
    let key: [u8; 32] = payload[4 + t.publisher.len()..4 + t.publisher.len() + 32]
        .try_into()
        .map_err(|_| DealError::Payload("key"))?;
    let n = payload.len();
    let observed = le_u64(payload, n - 16).ok_or(DealError::Payload("time"))?;
    let sequence = le_u64(payload, n - 8).ok_or(DealError::Payload("sequence"))?;
    if anchor_payload(t, &Hash32(key), observed, sequence) != payload {
        return Err(DealError::Payload("does not name this manifest"));
    }
    let mut b = onyx_core::signed::PAYLOAD_DOMAIN.as_bytes().to_vec();
    b.extend_from_slice(payload);
    Ok(keccak256(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_bulk::manifest::{Manifest, NewVersion, ShardEntry};
    use onyx_bulk::Schema;
    use onyx_core::{DataRights, Redistribution, SourceId};

    pub(crate) fn sample_manifest() -> Manifest {
        let schema = Schema::from_json(
            &serde_json::json!({"columns":[{"name":"t","kind":"int"}],"key":"t"}),
        )
        .expect("schema");
        let rights = DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("synthetic.publisher".into()),
            attribution: None,
            terms: String::new(),
        };
        let shards = vec![
            ShardEntry {
                index: 0,
                root: Hash32([1; 32]),
                rows: 10,
                bytes: 1 << 20,
                key_min: 1,
                key_max: 10,
            },
            ShardEntry {
                index: 1,
                root: Hash32([2; 32]),
                rows: 5,
                bytes: 1 << 19,
                key_min: 11,
                key_max: 15,
            },
        ];
        Manifest::build(
            NewVersion {
                dataset: "synthetic.chain-metrics".into(),
                publisher: SourceId("synthetic.publisher".into()),
                created_unix: 1_700_000_000,
                schema,
                rights,
            },
            None,
            shards,
        )
        .expect("manifest")
    }

    #[test]
    fn reads_the_tail_of_a_real_header() {
        let m = sample_manifest();
        let hb = borsh::to_vec(&m.header).expect("borsh");
        let t = read_header(&hb).expect("reads");
        assert_eq!(t.manifest_root, m.header.root());
        assert_eq!(t.shard_count, 2);
        assert_eq!(t.row_count, 15);
        assert_eq!(t.byte_count, (1 << 20) + (1 << 19));
        assert_eq!(t.shards_root, m.header.shards_root);
        assert_eq!(t.dataset, b"synthetic.chain-metrics");
        assert_eq!(t.publisher, b"synthetic.publisher");
        // Wrong format byte, truncated.
        let mut bad = hb.clone();
        bad[0] = 2;
        assert!(read_header(&bad).is_err());
        assert!(read_header(&hb[..40]).is_err());
        // Review STORAGE-R1-7: more shards than a manifest may hold.
        let mut big = hb.clone();
        let t = big.len() - TAIL_BYTES;
        big[t..t + 4].copy_from_slice(&(onyx_bulk::manifest::MAX_SHARDS + 1).to_le_bytes());
        assert_eq!(read_header(&big), Err(DealError::Header("empty")));
        // Review STORAGE-R2-5: fewer than 8 bytes per shard is no real manifest.
        let mut thin = hb.clone();
        thin[t + 12..t + 20].copy_from_slice(&15u64.to_le_bytes());
        assert_eq!(read_header(&thin), Err(DealError::Header("empty")));
        thin[t + 12..t + 20].copy_from_slice(&16u64.to_le_bytes());
        assert_eq!(
            read_header(&thin).expect("8 bytes per shard").byte_count,
            16
        );
    }

    #[test]
    fn payload_matches_the_publishers_signed_payload() {
        let m = sample_manifest();
        let hb = borsh::to_vec(&m.header).expect("borsh");
        let t = read_header(&hb).expect("reads");
        let secret = [7u8; 32];
        let report = onyx_bulk::anchor::sign(&secret, &m.header).expect("signs");
        let p = borsh::to_vec(&report.payload).expect("borsh");
        assert_eq!(check_payload(&t, &p).expect("ok"), report.payload.digest());
        // Another dataset's or root's payload is refused.
        let mut other = p.clone();
        let n = other.len();
        other[n - 20] ^= 1; // inside the root
        assert!(check_payload(&t, &other).is_err());
        assert!(check_payload(&t, &p[..p.len() - 1]).is_err());
    }
}
