//! Calldata for `OnyxStorage` and the decoding of what its views and events
//! return (Solidity ABI: 32-byte heads, dynamic tails). Only the types the
//! contract uses: unsigned integers, `bytes32`, `address`, `bool`, `bytes`,
//! `string` and `bytes32[]`.

use crate::challenge::{entry_bytes, StorageProof};
use crate::ids::NodeId;
use onyx_core::{keccak256, Hash32};

/// One argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    /// A static 32-byte word (integers, `bytes32`, `address`, `bool`).
    Word([u8; 32]),
    /// `bytes` or `string`.
    Bytes(Vec<u8>),
    /// `bytes32[]`.
    Words(Vec<[u8; 32]>),
}

/// A word holding an unsigned integer.
pub fn uint(v: u128) -> Tok {
    Tok::Word(word_u128(v))
}

/// The 32-byte word of an unsigned integer.
pub fn word_u128(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

/// A word holding an address.
pub fn addr(a: &[u8; 20]) -> Tok {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    Tok::Word(w)
}

/// A `bytes32` word.
pub fn b32(h: &Hash32) -> Tok {
    Tok::Word(h.0)
}

/// The 4-byte selector of a signature.
pub fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h.0[0], h.0[1], h.0[2], h.0[3]]
}

/// The topic of an event signature.
pub fn topic(sig: &str) -> Hash32 {
    keccak256(sig.as_bytes())
}

fn pad32(n: usize) -> usize {
    n.div_ceil(32) * 32
}

/// ABI-encode `toks` (no selector).
pub fn encode(toks: &[Tok]) -> Vec<u8> {
    let head_len = 32 * toks.len();
    let mut head = Vec::with_capacity(head_len);
    let mut tail: Vec<u8> = Vec::new();
    for t in toks {
        match t {
            Tok::Word(w) => head.extend_from_slice(w),
            Tok::Bytes(b) => {
                head.extend_from_slice(&word_u128((head_len + tail.len()) as u128));
                tail.extend_from_slice(&word_u128(b.len() as u128));
                tail.extend_from_slice(b);
                tail.resize(tail.len() + pad32(b.len()) - b.len(), 0);
            }
            Tok::Words(ws) => {
                head.extend_from_slice(&word_u128((head_len + tail.len()) as u128));
                tail.extend_from_slice(&word_u128(ws.len() as u128));
                for w in ws {
                    tail.extend_from_slice(w);
                }
            }
        }
    }
    head.extend_from_slice(&tail);
    head
}

/// Calldata: the selector of `sig` and the encoded arguments.
pub fn call(sig: &str, toks: &[Tok]) -> Vec<u8> {
    let mut d = selector(sig).to_vec();
    d.extend_from_slice(&encode(toks));
    d
}

/// A return value or event data that does not decode.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("ABI data does not decode: {0}")]
pub struct AbiError(pub &'static str);

/// Word `i` of `data`.
pub fn word(data: &[u8], i: usize) -> Result<[u8; 32], AbiError> {
    data.get(i * 32..i * 32 + 32)
        .and_then(|s| s.try_into().ok())
        .ok_or(AbiError("short"))
}

/// Word `i` as an unsigned integer that fits a u128.
pub fn word_uint(data: &[u8], i: usize) -> Result<u128, AbiError> {
    let w = word(data, i)?;
    if w[..16].iter().any(|b| *b != 0) {
        return Err(AbiError("integer over u128"));
    }
    Ok(u128::from_be_bytes(
        w[16..].try_into().map_err(|_| AbiError("word"))?,
    ))
}

/// Word `i` as a u64.
pub fn word_u64(data: &[u8], i: usize) -> Result<u64, AbiError> {
    u64::try_from(word_uint(data, i)?).map_err(|_| AbiError("integer over u64"))
}

/// Word `i` as an address.
pub fn word_addr(data: &[u8], i: usize) -> Result<[u8; 20], AbiError> {
    let w = word(data, i)?;
    if w[..12].iter().any(|b| *b != 0) {
        return Err(AbiError("address"));
    }
    w[12..].try_into().map_err(|_| AbiError("address"))
}

fn offset(data: &[u8], i: usize) -> Result<usize, AbiError> {
    usize::try_from(word_u64(data, i)?).map_err(|_| AbiError("offset"))
}

/// The `bytes`/`string` whose offset is word `i`.
pub fn dyn_bytes(data: &[u8], i: usize) -> Result<Vec<u8>, AbiError> {
    let o = offset(data, i)?;
    let len_w: [u8; 32] = data
        .get(o..o + 32)
        .and_then(|s| s.try_into().ok())
        .ok_or(AbiError("short"))?;
    let len = word_uint(&len_w, 0)? as usize;
    data.get(o + 32..o + 32 + len)
        .map(<[u8]>::to_vec)
        .ok_or(AbiError("short bytes"))
}

/// The `bytes32[]` whose offset is word `i`.
pub fn dyn_words(data: &[u8], i: usize) -> Result<Vec<Hash32>, AbiError> {
    let o = offset(data, i)?;
    let len_w: [u8; 32] = data
        .get(o..o + 32)
        .and_then(|s| s.try_into().ok())
        .ok_or(AbiError("short"))?;
    let len = word_uint(&len_w, 0)? as usize;
    if len > 1 << 16 {
        return Err(AbiError("array too long"));
    }
    (0..len)
        .map(|k| {
            data.get(o + 32 + 32 * k..o + 64 + 32 * k)
                .and_then(|s| s.try_into().ok())
                .map(Hash32)
                .ok_or(AbiError("short array"))
        })
        .collect()
}

fn words(v: &[Hash32]) -> Tok {
    Tok::Words(v.iter().map(|h| h.0).collect())
}

fn node_words(v: &[NodeId]) -> Tok {
    Tok::Words(v.iter().map(|n| n.0 .0).collect())
}

/// `register(uint64 capacityGiB, string url)`.
pub fn register(capacity_gib: u64, url: &str) -> Vec<u8> {
    call(
        "register(uint64,string)",
        &[
            uint(capacity_gib.into()),
            Tok::Bytes(url.as_bytes().to_vec()),
        ],
    )
}

/// `activate(uint64 index)`.
pub fn activate(index: u64) -> Vec<u8> {
    call("activate(uint64)", &[uint(index.into())])
}

/// `requestExit(bytes32 nodeId)`.
pub fn request_exit(node: &NodeId) -> Vec<u8> {
    call("requestExit(bytes32)", &[b32(&node.0)])
}

/// `withdrawBond(bytes32 nodeId)`.
pub fn withdraw_bond(node: &NodeId) -> Vec<u8> {
    call("withdrawBond(bytes32)", &[b32(&node.0)])
}

/// `createDeal(bytes header, bytes payload, uint8 r, uint32 epochs, string sourceUrl)`.
pub fn create_deal(header: &[u8], payload: &[u8], r: u8, epochs: u32, source_url: &str) -> Vec<u8> {
    call(
        "createDeal(bytes,bytes,uint8,uint32,string)",
        &[
            Tok::Bytes(header.to_vec()),
            Tok::Bytes(payload.to_vec()),
            uint(r.into()),
            uint(epochs.into()),
            Tok::Bytes(source_url.as_bytes().to_vec()),
        ],
    )
}

/// `decline(uint64 dealId, uint32 index, bytes32 nodeId)` (`index` =
/// [`crate::placement::WHOLE_DEAL`] for the whole deal).
pub fn decline(deal: u64, index: u32, node: &NodeId) -> Vec<u8> {
    call(
        "decline(uint64,uint32,bytes32)",
        &[uint(deal.into()), uint(index.into()), b32(&node.0)],
    )
}

/// `closeDeal(uint64 dealId)`.
pub fn close_deal(deal: u64) -> Vec<u8> {
    call("closeDeal(uint64)", &[uint(deal.into())])
}

/// `recordSeed(uint64 epoch)`.
pub fn record_seed(epoch: u64) -> Vec<u8> {
    call("recordSeed(uint64)", &[uint(epoch.into())])
}

/// `answer(uint64 dealId, uint64 epoch, uint32 slot, bytes32 nodeId,
/// bytes32[] nodes, bytes entry, bytes32[] entrySiblings, bytes row,
/// bytes32[] rowSiblings)`.
pub fn answer(
    deal: u64,
    epoch: u64,
    slot: u32,
    node: &NodeId,
    nodes: &[NodeId],
    p: &StorageProof,
) -> Vec<u8> {
    call(
        "answer(uint64,uint64,uint32,bytes32,bytes32[],bytes,bytes32[],bytes,bytes32[])",
        &[
            uint(deal.into()),
            uint(epoch.into()),
            uint(slot.into()),
            b32(&node.0),
            node_words(nodes),
            Tok::Bytes(entry_bytes(&p.entry)),
            words(&p.entry_siblings),
            Tok::Bytes(p.row.clone()),
            words(&p.row_siblings),
        ],
    )
}

/// `settleMissed(uint64 dealId, uint64 epoch, uint32 slot, bytes32 nodeId, bytes32[] nodes)`.
pub fn settle_missed(deal: u64, epoch: u64, slot: u32, node: &NodeId, nodes: &[NodeId]) -> Vec<u8> {
    call(
        "settleMissed(uint64,uint64,uint32,bytes32,bytes32[])",
        &[
            uint(deal.into()),
            uint(epoch.into()),
            uint(slot.into()),
            b32(&node.0),
            node_words(nodes),
        ],
    )
}

/// `withdrawEarnings()`.
pub fn withdraw_earnings() -> Vec<u8> {
    call("withdrawEarnings()", &[])
}

/// `sweepTreasury()`.
pub fn sweep_treasury() -> Vec<u8> {
    call("sweepTreasury()", &[])
}

/// `currentEpoch()`.
pub fn current_epoch() -> Vec<u8> {
    call("currentEpoch()", &[])
}

/// `setHashAt(uint64 epoch)`.
pub fn set_hash_at(epoch: u64) -> Vec<u8> {
    call("setHashAt(uint64)", &[uint(epoch.into())])
}

/// `activeNodes()`.
pub fn active_nodes() -> Vec<u8> {
    call("activeNodes()", &[])
}

/// `seedOf(uint64 epoch)`.
pub fn seed_of(epoch: u64) -> Vec<u8> {
    call("seedOf(uint64)", &[uint(epoch.into())])
}

/// `dealInfo(uint64 dealId)`.
pub fn deal_info(deal: u64) -> Vec<u8> {
    call("dealInfo(uint64)", &[uint(deal.into())])
}

/// `dealCount()`.
pub fn deal_count() -> Vec<u8> {
    call("dealCount()", &[])
}

/// `nodeInfo(bytes32 nodeId)`.
pub fn node_info(node: &NodeId) -> Vec<u8> {
    call("nodeInfo(bytes32)", &[b32(&node.0)])
}

/// `exclusionFrom(uint64 dealId, uint32 index, bytes32 nodeId)`.
pub fn exclusion_from(deal: u64, index: u32, node: &NodeId) -> Vec<u8> {
    call(
        "exclusionFrom(uint64,uint32,bytes32)",
        &[uint(deal.into()), uint(index.into()), b32(&node.0)],
    )
}

/// `slotStatus(uint64 dealId, uint64 epoch, uint32 slot, bytes32 nodeId)`.
pub fn slot_status(deal: u64, epoch: u64, slot: u32, node: &NodeId) -> Vec<u8> {
    call(
        "slotStatus(uint64,uint64,uint32,bytes32)",
        &[
            uint(deal.into()),
            uint(epoch.into()),
            uint(slot.into()),
            b32(&node.0),
        ],
    )
}

/// `earned(address)`.
pub fn earned(a: &[u8; 20]) -> Vec<u8> {
    call("earned(address)", &[addr(a)])
}

/// `books()`.
pub fn books() -> Vec<u8> {
    call("books()", &[])
}

/// `ROW_VERSION()`: the challenged-row rule (2: the row is drawn with the
/// answering node's id, ADR-0060 amendment 1). Older contracts have none.
pub fn row_version() -> Vec<u8> {
    call("ROW_VERSION()", &[])
}

/// `conserved()`.
pub fn conserved() -> Vec<u8> {
    call("conserved()", &[])
}

/// `params()`.
pub fn params() -> Vec<u8> {
    call("params()", &[])
}

/// `epochStart(uint64 epoch)`.
pub fn epoch_start(epoch: u64) -> Vec<u8> {
    call("epochStart(uint64)", &[uint(epoch.into())])
}

/// A deal as `dealInfo` returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DealInfo {
    /// Who paid.
    pub payer: [u8; 20],
    /// Manifest root.
    pub manifest_root: Hash32,
    /// Shards root.
    pub shards_root: Hash32,
    /// Placement salt.
    pub salt: Hash32,
    /// Shards.
    pub shard_count: u32,
    /// Replicas.
    pub r: u8,
    /// Manifest bytes.
    pub byte_count: u64,
    /// First challenged epoch.
    pub start_epoch: u64,
    /// First epoch after the deal.
    pub end_epoch: u64,
    /// Deposit.
    pub deposit: u128,
    /// Paid out so far.
    pub paid: u128,
    /// Closed (refunded).
    pub closed: bool,
}

/// Decode `dealInfo`'s return (12 static words).
pub fn decode_deal(d: &[u8]) -> Result<DealInfo, AbiError> {
    Ok(DealInfo {
        payer: word_addr(d, 0)?,
        manifest_root: Hash32(word(d, 1)?),
        shards_root: Hash32(word(d, 2)?),
        salt: Hash32(word(d, 3)?),
        shard_count: u32::try_from(word_u64(d, 4)?).map_err(|_| AbiError("shard count"))?,
        r: u8::try_from(word_u64(d, 5)?).map_err(|_| AbiError("r"))?,
        byte_count: word_u64(d, 6)?,
        start_epoch: word_u64(d, 7)?,
        end_epoch: word_u64(d, 8)?,
        deposit: word_uint(d, 9)?,
        paid: word_uint(d, 10)?,
        closed: word_uint(d, 11)? != 0,
    })
}

/// A node as `nodeInfo` returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfo {
    /// Operator (who answers and is paid).
    pub operator: [u8; 20],
    /// 0 none, 1 active, 2 exiting, 3 jailed, 4 withdrawn.
    pub status: u8,
    /// Declared capacity.
    pub capacity_gib: u64,
    /// Bond left.
    pub bond: u128,
    /// Failed or missed answers in a row.
    pub misses: u32,
    /// Epoch from which it is out of the list (0: in).
    pub removed_from: u64,
    /// Epoch from which it is jailed (0: not).
    pub jailed_from: u64,
    /// Registration index (its URL is in the `Registered` event).
    pub index: u64,
}

/// Decode `nodeInfo`'s return (8 static words).
pub fn decode_node(d: &[u8]) -> Result<NodeInfo, AbiError> {
    Ok(NodeInfo {
        operator: word_addr(d, 0)?,
        status: u8::try_from(word_u64(d, 1)?).map_err(|_| AbiError("status"))?,
        capacity_gib: word_u64(d, 2)?,
        bond: word_uint(d, 3)?,
        misses: u32::try_from(word_u64(d, 4)?).map_err(|_| AbiError("misses"))?,
        removed_from: word_u64(d, 5)?,
        jailed_from: word_u64(d, 6)?,
        index: word_u64(d, 7)?,
    })
}

/// Event signatures (topics are their Keccak).
pub mod events {
    /// A registration (bond paid; id fixed by `activate`).
    pub const REGISTERED: &str = "Registered(uint64,address,uint64,uint256,string)";
    /// A node in the list.
    pub const ACTIVATED: &str = "Activated(bytes32,uint64,address)";
    /// The node list from an epoch on.
    pub const NODE_SET: &str = "NodeSet(uint64,bytes32,bytes32[])";
    /// A node out of the list (exit or jail).
    pub const REMOVED: &str = "Removed(bytes32,uint64,bool)";
    /// A deal.
    pub const DEAL_CREATED: &str =
        "DealCreated(uint64,address,bytes32,bytes32,uint32,uint64,uint8,uint64,uint64,bytes32,uint256,string,string,string)";
    /// An epoch's seed.
    pub const SEED: &str = "SeedRecorded(uint64,bytes32)";
    /// An answer.
    pub const ANSWERED: &str = "Answered(uint64,uint64,uint32,bytes32,bool,uint256)";
    /// A slash.
    pub const SLASHED: &str = "Slashed(bytes32,uint64,uint64,uint32,uint32,uint256,uint64,uint8)";
    /// A node gave up a shard (or a whole deal) without a slash.
    pub const DECLINED: &str = "Declined(bytes32,uint64,uint32,uint64)";
    /// The part of a bond `register` keeps for the treasury (ADR-0060 amendment 1 item 2).
    pub const REGISTRATION_FEE: &str = "RegistrationFee(uint64,address,uint256)";
    /// A registration not activated in time: its bond less the fee returned.
    pub const REGISTRATION_CANCELLED: &str = "RegistrationCancelled(uint64)";
    /// The reporter of a miss credited part of its slash (amendment 1 item 3).
    pub const MISS_REWARD: &str = "MissReward(bytes32,address,uint256)";
    /// More epochs paid for a deal.
    pub const DEAL_EXTENDED: &str = "DealExtended(uint64,uint64,uint256)";
    /// A deal closed: its remainder back to the payer.
    pub const DEAL_CLOSED: &str = "DealClosed(uint64,uint256)";
    /// A bond returned after exit or jail.
    pub const BOND_WITHDRAWN: &str = "BondWithdrawn(bytes32,address,uint256)";
    /// Earnings (pay and rewards) withdrawn.
    pub const EARNINGS_WITHDRAWN: &str = "EarningsWithdrawn(address,uint256)";
    /// Slashes and fees sent to the treasury.
    pub const TREASURY_SWEPT: &str = "TreasurySwept(address,uint256)";
    /// Every event that moves money or the books (`onyx_storage::ledger::Replay`).
    pub const MONEY: [&str; 13] = [
        REGISTERED,
        REGISTRATION_FEE,
        ACTIVATED,
        REGISTRATION_CANCELLED,
        DEAL_CREATED,
        DEAL_EXTENDED,
        ANSWERED,
        SLASHED,
        MISS_REWARD,
        DEAL_CLOSED,
        BOND_WITHDRAWN,
        EARNINGS_WITHDRAWN,
        TREASURY_SWEPT,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_dynamic_arguments() {
        // register(5, "ab"): head (5, offset 64), tail (len 2, "ab" padded).
        let d = register(5, "ab");
        assert_eq!(&d[..4], &selector("register(uint64,string)"));
        let a = &d[4..];
        assert_eq!(word_u64(a, 0).expect("w"), 5);
        assert_eq!(word_u64(a, 1).expect("w"), 64);
        assert_eq!(dyn_bytes(a, 1).expect("b"), b"ab");
        assert_eq!(a.len(), 128);
        let w = encode(&[Tok::Words(vec![[1; 32], [2; 32]]), Tok::Bytes(vec![])]);
        assert_eq!(
            dyn_words(&w, 0).expect("w"),
            vec![Hash32([1; 32]), Hash32([2; 32])]
        );
        assert_eq!(dyn_bytes(&w, 1).expect("b"), Vec::<u8>::new());
    }

    #[test]
    fn refuses_short_or_wide_words() {
        assert!(word_u64(&[0u8; 31], 0).is_err());
        assert!(word_u64(&[0xff; 32], 0).is_err());
        assert!(word_addr(&[0xff; 32], 0).is_err());
    }
}
