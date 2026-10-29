//! What `eth-chain-stats` reads from a block's bytes: the header fields, each
//! transaction's type, recipient, value and fee fields, each receipt's
//! status and cumulative gas, and the protocol's fee rules (EIP-1559 base
//! fee, EIP-4844 blob base fee). Formats: the Yellow Paper (legacy
//! transactions and receipts), EIP-2718 (typed envelopes), EIP-2930, -1559,
//! -4844 and -7702 (transaction types 1-4), EIP-4895, -4788 and -7685 (the
//! header fields after the base fee, which are hashed but not read here).

use crate::rlp::{self, Item, RlpError};

/// Why a header, transaction or receipt was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Malformed RLP.
    Rlp(RlpError),
    /// A header older than London (no base fee).
    PreLondon,
    /// A transaction or receipt type this format does not know (0-4 only).
    UnknownType(u8),
    /// A field with the wrong length (an address, a hash, a bloom).
    BadField,
}

impl From<RlpError> for DecodeError {
    fn from(e: RlpError) -> Self {
        DecodeError::Rlp(e)
    }
}

/// Header fields the statistics read (all are hashed into the block hash).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Hash of the parent block.
    pub parent_hash: [u8; 32],
    /// Root of the transactions trie.
    pub transactions_root: [u8; 32],
    /// Root of the receipts trie.
    pub receipts_root: [u8; 32],
    /// Block number.
    pub number: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas used by every transaction of the block.
    pub gas_used: u64,
    /// Timestamp, unix seconds.
    pub timestamp: u64,
    /// Base fee per gas, wei (EIP-1559).
    pub base_fee: u128,
    /// Blob gas used (EIP-4844), when the header has the field.
    pub blob_gas_used: Option<u64>,
    /// Excess blob gas (EIP-4844), when the header has the field.
    pub excess_blob_gas: Option<u64>,
}

/// Decode a header's RLP. Needs at least the 16 fields of London; Cancun's
/// blob fields are read when present (fields 17 and 18), later fields are
/// hashed only.
pub fn header(b: &[u8]) -> Result<Header, DecodeError> {
    let f = rlp::list(b)?;
    if f.len() < 16 {
        return Err(DecodeError::PreLondon);
    }
    // Fixed-size fields are checked even when not read: the hash covers them anyway.
    f[1].fixed::<32>()?;
    f[2].fixed::<20>()?;
    f[3].fixed::<32>()?;
    if f[6].bytes()?.len() != 256 {
        return Err(DecodeError::BadField);
    }
    let blob = if f.len() >= 19 {
        (Some(f[17].u64()?), Some(f[18].u64()?))
    } else {
        (None, None)
    };
    Ok(Header {
        parent_hash: f[0].fixed()?,
        transactions_root: f[4].fixed()?,
        receipts_root: f[5].fixed()?,
        number: f[8].u64()?,
        gas_limit: f[9].u64()?,
        gas_used: f[10].u64()?,
        timestamp: f[11].u64()?,
        base_fee: f[15].u128()?,
        blob_gas_used: blob.0,
        excess_blob_gas: blob.1,
    })
}

/// A transaction's fee fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fees {
    /// Types 0 and 1: one gas price.
    GasPrice(u128),
    /// Types 2, 3 and 4: a fee cap and a priority-fee cap per gas.
    Dynamic {
        /// `maxFeePerGas`.
        max_fee: u128,
        /// `maxPriorityFeePerGas`.
        max_priority: u128,
    },
}

/// What the statistics read from one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tx {
    /// EIP-2718 type (0 for a legacy transaction).
    pub tx_type: u8,
    /// True when `to` is empty: the transaction creates a contract.
    pub creates: bool,
    /// Value sent, wei.
    pub value: u128,
    /// Gas limit of the transaction.
    pub gas: u64,
    /// Fee fields.
    pub fees: Fees,
    /// Number of blob versioned hashes (type 3 only, at least one there).
    pub blobs: u64,
}

fn recipient(item: Item<'_>, may_create: bool) -> Result<bool, DecodeError> {
    match item.bytes()?.len() {
        0 if may_create => Ok(true),
        20 => Ok(false),
        _ => Err(DecodeError::BadField),
    }
}

/// A canonical unsigned integer of at most 32 bytes (read only for its form).
fn uint256(item: Item<'_>) -> Result<(), DecodeError> {
    let b = item.bytes()?;
    if b.len() > 32 || b.first() == Some(&0) {
        return Err(RlpError::BadInt.into());
    }
    Ok(())
}

fn access_list(item: Item<'_>) -> Result<(), DecodeError> {
    for entry in item.list()? {
        let e = entry.list()?;
        if e.len() != 2 {
            return Err(RlpError::WrongCount.into());
        }
        e[0].fixed::<20>()?;
        for key in e[1].list()? {
            key.fixed::<32>()?;
        }
    }
    Ok(())
}

/// Decode one transaction as the transactions trie stores it: a legacy RLP
/// list, or a type byte (1-4) followed by exactly one RLP list. Field counts,
/// address and hash lengths and integer forms are checked; signatures are
/// not recovered (the statistics need no sender).
pub fn tx(b: &[u8]) -> Result<Tx, DecodeError> {
    let &first = b.first().ok_or(RlpError::Truncated)?;
    if first >= 0xc0 {
        let f = rlp::list(b)?;
        if f.len() != 9 {
            return Err(RlpError::WrongCount.into());
        }
        f[0].u64()?;
        f[5].bytes()?;
        f[6].u128()?;
        return Ok(Tx {
            tx_type: 0,
            fees: Fees::GasPrice(f[1].u128()?),
            gas: f[2].u64()?,
            creates: recipient(f[3], true)?,
            value: f[4].u128()?,
            blobs: 0,
        });
    }
    let f = rlp::list(&b[1..])?;
    let want = match first {
        1 => 11,
        2 => 12,
        3 => 14,
        4 => 13,
        t => return Err(DecodeError::UnknownType(t)),
    };
    if f.len() != want {
        return Err(RlpError::WrongCount.into());
    }
    f[0].u64()?;
    f[1].u64()?;
    if first == 1 {
        f[6].bytes()?;
        access_list(f[7])?;
        if f[8].u64()? > 1 {
            return Err(DecodeError::BadField);
        }
        return Ok(Tx {
            tx_type: 1,
            fees: Fees::GasPrice(f[2].u128()?),
            gas: f[3].u64()?,
            creates: recipient(f[4], true)?,
            value: f[5].u128()?,
            blobs: 0,
        });
    }
    f[7].bytes()?;
    access_list(f[8])?;
    let mut blobs = 0u64;
    let parity = match first {
        3 => {
            f[9].u128()?;
            for h in f[10].list()? {
                h.fixed::<32>()?;
                blobs += 1;
            }
            if blobs == 0 {
                return Err(DecodeError::BadField);
            }
            f[11]
        }
        4 => {
            let auths = f[9].list()?;
            if auths.is_empty() {
                return Err(DecodeError::BadField);
            }
            for a in auths {
                let t = a.list()?;
                if t.len() != 6 {
                    return Err(RlpError::WrongCount.into());
                }
                // EIP-7702 bounds: chain id below 2^256, nonce below 2^64, parity below 2^8.
                uint256(t[0])?;
                t[1].fixed::<20>()?;
                t[2].u64()?;
                if t[3].u64()? > 0xff {
                    return Err(DecodeError::BadField);
                }
                uint256(t[4])?;
                uint256(t[5])?;
            }
            f[10]
        }
        _ => f[9],
    };
    if parity.u64()? > 1 {
        return Err(DecodeError::BadField);
    }
    Ok(Tx {
        tx_type: first,
        fees: Fees::Dynamic {
            max_priority: f[2].u128()?,
            max_fee: f[3].u128()?,
        },
        gas: f[4].u64()?,
        // Blob and set-code transactions cannot create a contract.
        creates: recipient(f[5], first == 2)?,
        value: f[6].u128()?,
        blobs,
    })
}

/// What the statistics read from one receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// EIP-2718 type, as the transaction's.
    pub tx_type: u8,
    /// Status 1 (success) or 0 (reverted).
    pub success: bool,
    /// Gas used by the block up to and including this transaction.
    pub cumulative_gas: u64,
}

/// Decode one receipt as the receipts trie stores it: a legacy RLP list
/// `[status, cumulativeGas, bloom, logs]`, or a type byte (1-4) and that
/// list. Pre-Byzantium receipts (a state root instead of a status) are refused.
pub fn receipt(b: &[u8]) -> Result<Receipt, DecodeError> {
    let &first = b.first().ok_or(RlpError::Truncated)?;
    let (tx_type, body) = match first {
        0xc0.. => (0, b),
        1..=4 => (first, &b[1..]),
        t => return Err(DecodeError::UnknownType(t)),
    };
    let f = rlp::list(body)?;
    if f.len() != 4 {
        return Err(RlpError::WrongCount.into());
    }
    let success = match f[0].u64()? {
        0 => false,
        1 => true,
        _ => return Err(DecodeError::BadField),
    };
    if f[2].bytes()?.len() != 256 {
        return Err(DecodeError::BadField);
    }
    for log in f[3].list()? {
        let l = log.list()?;
        if l.len() != 3 {
            return Err(RlpError::WrongCount.into());
        }
        l[0].fixed::<20>()?;
        for topic in l[1].list()? {
            topic.fixed::<32>()?;
        }
        l[2].bytes()?;
    }
    Ok(Receipt {
        tx_type,
        success,
        cumulative_gas: f[1].u64()?,
    })
}

/// EIP-1559 constants (unchanged through Fusaka).
pub const ELASTICITY_MULTIPLIER: u64 = 2;
/// EIP-1559: the base fee moves by at most 1/8 per block.
pub const BASE_FEE_MAX_CHANGE_DENOMINATOR: u128 = 8;
/// Gas per blob (EIP-4844, unchanged by later forks).
pub const GAS_PER_BLOB: u64 = 1 << 17;
/// EIP-4844 minimum blob base fee, wei.
pub const MIN_BLOB_BASE_FEE: u128 = 1;
/// Blob base fee update fraction of BPO2 (EIP-7892 blob-parameter-only fork,
/// mainnet since 2026-01-07: target 14, maximum 21 blobs), the schedule
/// `eth-chain-stats` pins. The adapter compares the fee it gives with every
/// blob receipt's `blobGasPrice`, so a later change is refused, not absorbed.
pub const BPO2_BLOB_UPDATE_FRACTION: u128 = 11_684_671;

/// BPO2's blob target and maximum per block (EIP-7892 schedule; EIP-7607 for mainnet).
pub const BPO2_TARGET_BLOBS: u64 = 14;
/// BPO2's maximum blobs per block.
pub const BPO2_MAX_BLOBS: u64 = 21;
/// EIP-7918: the execution cost a blob's price is bounded by (`BLOB_BASE_COST`).
pub const BLOB_BASE_COST: u128 = 1 << 13;

/// The excess blob gas a block must carry after `parent` under BPO2's parameters
/// (EIP-4844 `calc_excess_blob_gas` as amended by EIP-7918, Osaka and later). `None`
/// when the parent has no blob fields or on overflow.
pub fn next_excess_blob_gas(parent: &Header) -> Option<u64> {
    let (excess, used) = (parent.excess_blob_gas?, parent.blob_gas_used?);
    let target = BPO2_TARGET_BLOBS * GAS_PER_BLOB;
    let sum = excess.checked_add(used)?;
    if sum < target {
        return Some(0);
    }
    let blob_fee = fake_exponential(
        MIN_BLOB_BASE_FEE,
        u128::from(excess),
        BPO2_BLOB_UPDATE_FRACTION,
    )?;
    if BLOB_BASE_COST.checked_mul(parent.base_fee)?
        > u128::from(GAS_PER_BLOB).checked_mul(blob_fee)?
    {
        excess.checked_add(used.checked_mul(BPO2_MAX_BLOBS - BPO2_TARGET_BLOBS)? / BPO2_MAX_BLOBS)
    } else {
        Some(sum - target)
    }
}

/// The base fee a block must carry after `parent` (EIP-1559). `None` on overflow.
pub fn next_base_fee(parent: &Header) -> Option<u128> {
    let target = u128::from(parent.gas_limit / ELASTICITY_MULTIPLIER);
    let used = u128::from(parent.gas_used);
    let base = parent.base_fee;
    if target == 0 {
        return None;
    }
    if used == target {
        Some(base)
    } else if used > target {
        let delta = base.checked_mul(used - target)? / target / BASE_FEE_MAX_CHANGE_DENOMINATOR;
        base.checked_add(delta.max(1))
    } else {
        let delta = base.checked_mul(target - used)? / target / BASE_FEE_MAX_CHANGE_DENOMINATOR;
        base.checked_sub(delta)
    }
}

/// `gas_used x 1,000,000 / gas_limit`, rounded down: how full a block is, in parts per million.
/// `None` for a zero limit or a quotient above 1,000,000 (more gas used than the limit).
pub fn fullness_ppm(gas_used: u64, gas_limit: u64) -> Option<u32> {
    let q = (u128::from(gas_used) * 1_000_000).checked_div(u128::from(gas_limit))?;
    u32::try_from(q).ok().filter(|&v| v <= 1_000_000)
}

/// A gas limit may move by less than 1/1024 of the parent's per block, and
/// never below 5,000 (Yellow Paper, eq. 47-49).
pub fn gas_limit_ok(parent: u64, child: u64) -> bool {
    let bound = parent / 1024;
    child >= 5_000 && child.abs_diff(parent) < bound
}

/// EIP-4844 `fake_exponential(factor, numerator, denominator)`: integer
/// approximation of `factor * e^(numerator / denominator)`. `None` on overflow.
pub fn fake_exponential(factor: u128, numerator: u128, denominator: u128) -> Option<u128> {
    if denominator == 0 {
        return None;
    }
    let mut i: u128 = 1;
    let mut output: u128 = 0;
    let mut acc = factor.checked_mul(denominator)?;
    while acc > 0 {
        output = output.checked_add(acc)?;
        acc = acc.checked_mul(numerator)? / denominator.checked_mul(i)?;
        i += 1;
    }
    Some(output / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_exponential_matches_the_eip_reference() {
        // The EIP-4844 reference loop on small numbers: factor * e^0 = factor.
        assert_eq!(fake_exponential(1, 0, 3_338_477), Some(1));
        assert_eq!(fake_exponential(2, 0, 1), Some(2));
        // e^1 with denominator 1: 1 + 1 + 1/2 ... in integers: acc 1,1,0 -> 2.
        assert_eq!(fake_exponential(1, 1, 1), Some(2));
        // Mainnet block 26,225,560 (fetched 2026-10-19): excess 172,147,094 under BPO2's
        // fraction 11,684,671 gives the 2,502,320 wei its receipts report as blobGasPrice.
        assert_eq!(
            fake_exponential(1, 172_147_094, 11_684_671),
            Some(2_502_320)
        );
        assert_eq!(fake_exponential(1, u128::MAX, 1), None);
    }

    fn parent(gas_limit: u64, gas_used: u64, base_fee: u128) -> Header {
        Header {
            parent_hash: [0; 32],
            transactions_root: [0; 32],
            receipts_root: [0; 32],
            number: 1,
            gas_limit,
            gas_used,
            timestamp: 0,
            base_fee,
            blob_gas_used: None,
            excess_blob_gas: None,
        }
    }

    #[test]
    fn base_fee_follows_eip_1559() {
        // At target: unchanged. Full: +1/8. Empty: -1/8. Just above target: at least +1.
        assert_eq!(
            next_base_fee(&parent(60_000_000, 30_000_000, 1_000)),
            Some(1_000)
        );
        assert_eq!(
            next_base_fee(&parent(60_000_000, 60_000_000, 1_000)),
            Some(1_125)
        );
        assert_eq!(next_base_fee(&parent(60_000_000, 0, 1_000)), Some(875));
        assert_eq!(next_base_fee(&parent(60_000_000, 30_000_001, 7)), Some(8));
        // Rounding down: 1,001 * 1 / 30,000,000 / 8 = 0 below target.
        assert_eq!(
            next_base_fee(&parent(60_000_000, 29_999_999, 1_001)),
            Some(1_001)
        );
        assert_eq!(next_base_fee(&parent(1, 0, 5)), None);
    }

    #[test]
    fn excess_blob_gas_follows_eip_7918_under_bpo2() {
        let mut p = parent(60_000_000, 30_000_000, 40_000_000);
        // Below target: 0.
        p.excess_blob_gas = Some(0);
        p.blob_gas_used = Some(13 * GAS_PER_BLOB);
        assert_eq!(next_excess_blob_gas(&p), Some(0));
        // Mainnet-like: excess 172,147,094 (blob fee 2,502,320 wei), 3 blobs, base fee 0.04 gwei:
        // 8,192 x 40,000,000 < 131,072 x 2,502,320, so excess + used - target.
        p.excess_blob_gas = Some(172_147_094);
        p.blob_gas_used = Some(3 * GAS_PER_BLOB);
        assert_eq!(
            next_excess_blob_gas(&p),
            Some(172_147_094 + 3 * GAS_PER_BLOB - 14 * GAS_PER_BLOB)
        );
        // A base fee high enough to bound the blob price: used x (max - target) / max is added.
        p.base_fee = 10u128.pow(12);
        assert_eq!(
            next_excess_blob_gas(&p),
            Some(172_147_094 + 3 * GAS_PER_BLOB * 7 / 21)
        );
        p.blob_gas_used = None;
        assert_eq!(next_excess_blob_gas(&p), None);
    }

    #[test]
    fn fullness_is_parts_per_million_rounded_down() {
        // P4A-R2-4: a full block is exactly 1,000,000; thirds round down.
        assert_eq!(fullness_ppm(60_000_000, 60_000_000), Some(1_000_000));
        assert_eq!(fullness_ppm(1, 3), Some(333_333));
        assert_eq!(fullness_ppm(2, 3), Some(666_666));
        assert_eq!(fullness_ppm(59_999_999, 60_000_000), Some(999_999));
        assert_eq!(fullness_ppm(0, 60_000_000), Some(0));
        assert_eq!(fullness_ppm(1, 0), None);
        assert_eq!(fullness_ppm(2, 1), None);
    }

    #[test]
    fn legacy_and_access_list_transactions_without_a_recipient_create() {
        // P4A-R2-4: types 0 and 1 (and 2) with an empty `to` are creations; with one, calls.
        for ty in [0u8, 1, 2] {
            let mut s = spec(ty);
            assert!(
                !tx(&synthetic::tx(&s)).expect("call").creates,
                "type {ty} call"
            );
            s.to = None;
            assert!(
                tx(&synthetic::tx(&s)).expect("creation").creates,
                "type {ty} creation"
            );
        }
    }

    #[test]
    fn gas_limit_moves_by_less_than_one_1024th() {
        assert!(gas_limit_ok(60_000_000, 60_000_000 + 58_592));
        assert!(!gas_limit_ok(60_000_000, 60_000_000 + 58_593));
        assert!(gas_limit_ok(60_000_000, 60_000_000 - 58_592));
        assert!(!gas_limit_ok(60_000_000, 60_000_000 - 58_593));
        assert!(!gas_limit_ok(5_000, 4_999));
    }

    use crate::synthetic::{self, TxSpec};

    fn spec(tx_type: u8) -> TxSpec {
        TxSpec {
            tx_type,
            to: Some([0xaa; 20]),
            value: 1,
            max_fee: 10,
            max_priority: 1,
            blobs: 1,
        }
    }

    /// Re-encode a typed transaction's list after `f` edits its items' encodings.
    fn edit(
        env: &[u8],
        f: impl Fn(&mut alloc::vec::Vec<alloc::vec::Vec<u8>>),
    ) -> alloc::vec::Vec<u8> {
        let mut items: alloc::vec::Vec<alloc::vec::Vec<u8>> = rlp::list(&env[1..])
            .expect("list")
            .into_iter()
            .map(|i| {
                let mut o = alloc::vec::Vec::new();
                match i {
                    Item::Bytes(b) => rlp::encode_bytes(&mut o, b),
                    Item::List(p) => rlp::encode_list(&mut o, p),
                }
                o
            })
            .collect();
        f(&mut items);
        let mut out = alloc::vec![env[0]];
        rlp::encode_list(&mut out, &items.concat());
        out
    }

    #[test]
    fn malformed_transactions_and_receipts_are_refused() {
        for t in 0..=4 {
            assert!(tx(&synthetic::tx(&spec(t))).is_ok(), "type {t}");
        }
        let blob = synthetic::tx(&spec(3));
        let enc = |b: &[u8]| {
            let mut o = alloc::vec::Vec::new();
            rlp::encode_bytes(&mut o, b);
            o
        };
        let empty_list = alloc::vec![0xc0u8];
        // A field dropped; an unknown type; a 19-byte recipient; parity 2; no blob hashes.
        assert_eq!(
            tx(&edit(&blob, |i| {
                i.pop();
            })),
            Err(RlpError::WrongCount.into())
        );
        let mut five = blob.clone();
        five[0] = 5;
        assert_eq!(tx(&five), Err(DecodeError::UnknownType(5)));
        assert_eq!(
            tx(&edit(&blob, |i| i[5] = enc(&[0xaa; 19]))),
            Err(DecodeError::BadField)
        );
        assert_eq!(
            tx(&edit(&blob, |i| i[11] = enc(&[2]))),
            Err(DecodeError::BadField)
        );
        assert_eq!(
            tx(&edit(&blob, |i| i[10] = empty_list.clone())),
            Err(DecodeError::BadField)
        );
        // A blob transaction without a recipient (it cannot create a contract).
        assert_eq!(
            tx(&edit(&blob, |i| i[5] = enc(&[]))),
            Err(DecodeError::BadField)
        );
        // A set-code transaction with an empty authorization list.
        let auth = synthetic::tx(&spec(4));
        assert_eq!(
            tx(&edit(&auth, |i| i[9] = empty_list.clone())),
            Err(DecodeError::BadField)
        );
        // Receipts: status 2, a short bloom, an unknown type, a pre-Byzantium state root.
        let r = synthetic::receipt(2, true, 21_000);
        assert!(receipt(&r).is_ok());
        assert_eq!(
            receipt(&edit(&r, |i| i[0] = enc(&[2]))),
            Err(DecodeError::BadField)
        );
        assert_eq!(
            receipt(&edit(&r, |i| i[2] = enc(&[0; 255]))),
            Err(DecodeError::BadField)
        );
        let mut r5 = r.clone();
        r5[0] = 5;
        assert_eq!(receipt(&r5), Err(DecodeError::UnknownType(5)));
        assert!(receipt(&edit(&r, |i| i[0] = enc(&[7; 32]))).is_err());
    }
}
