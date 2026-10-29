//! Prepaid balances on chain (ROADMAP S7 part 2, ADR-0046 amendment 2): the server's reads of
//! `OnyxPrepaid.accounts` ([`ChainAccounts`]) and the reporter's pass that redeems each buyer's
//! latest voucher and allocates the redeemed money to the books the usage journal names
//! ([`PrepaidReporter::pass`]).
//!
//! Redeem is safe to repeat: the contract refuses a total at or below what it redeemed, so a
//! pass first re-reads each buyer and sends only vouchers above it. Allocate is not: paying a
//! book twice would spend other buyers' pooled money. So the lines a pass allocates are marked in
//! a sidecar file (`<journal>.allocated`, owner-only) BEFORE the transaction is sent, and unmarked
//! only when nothing was broadcast or the transaction reverted. A crash or an unknown outcome
//! leaves them marked: the money then stays in the contract's `pool` (visible on chain) instead
//! of being paid twice.

use crate::evm_tx::{SendError, Sender};
use crate::prepaid::{Account, Voucher};
use crate::prepaid_gate::{AccountSource, PrepaidState, Usage};
use crate::roots::JsonRpc;
use crate::x402::BoxFuture;
use onyx_core::{keccak256, Hash32};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The contract's batch limit (`MAX_BATCH`).
pub const MAX_BATCH: usize = 64;

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes()).0;
    [h[0], h[1], h[2], h[3]]
}

fn word_u128(n: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&n.to_be_bytes());
    w
}

fn word_addr(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// Calldata of `accounts(address)`.
pub fn accounts_calldata(buyer: &[u8; 20]) -> Vec<u8> {
    let mut d = selector("accounts(address)").to_vec();
    d.extend_from_slice(&word_addr(buyer));
    d
}

/// Decode `accounts`' return `(uint128 deposited, uint128 redeemed, uint64 exitAt)`.
pub fn decode_account(ret: &[u8]) -> Result<Account, String> {
    if ret.len() != 96 {
        return Err(format!("accounts returned {} bytes, not 96", ret.len()));
    }
    let n = |w: &[u8]| -> Result<u128, String> {
        if w[..16].iter().any(|b| *b != 0) {
            return Err("accounts returned a word above uint128".into());
        }
        Ok(u128::from_be_bytes(
            w[16..32].try_into().map_err(|_| "word")?,
        ))
    };
    let exit_at = n(&ret[64..96])?;
    if exit_at > u128::from(u64::MAX) {
        return Err("accounts returned exitAt above uint64".into());
    }
    Ok(Account {
        deposited: n(&ret[0..32])?,
        redeemed: n(&ret[32..64])?,
        exiting: exit_at != 0,
    })
}

/// Calldata of `redeem((address,uint128,bytes)[])`.
pub fn redeem_calldata(vs: &[Voucher]) -> Vec<u8> {
    let mut d = selector("redeem((address,uint128,bytes)[])").to_vec();
    d.extend_from_slice(&word_u128(0x20));
    d.extend_from_slice(&word_u128(vs.len() as u128));
    // Each tuple holds `bytes`, so it is dynamic: the array head is one offset per tuple,
    // counted from the start of the heads.
    let tuples: Vec<Vec<u8>> = vs
        .iter()
        .map(|v| {
            let mut t = Vec::new();
            t.extend_from_slice(&word_addr(&v.buyer));
            t.extend_from_slice(&word_u128(v.cumulative));
            t.extend_from_slice(&word_u128(0x60));
            t.extend_from_slice(&word_u128(v.signature.len() as u128));
            let mut sig = v.signature.clone();
            sig.resize(v.signature.len().div_ceil(32) * 32, 0);
            t.extend_from_slice(&sig);
            t
        })
        .collect();
    let mut at = 32 * tuples.len();
    for t in &tuples {
        d.extend_from_slice(&word_u128(at as u128));
        at += t.len();
    }
    for t in tuples {
        d.extend_from_slice(&t);
    }
    d
}

/// Calldata of `allocate((uint64,bytes32,uint128)[])`.
pub fn allocate_calldata(items: &[(u64, Hash32, u128)]) -> Vec<u8> {
    let mut d = selector("allocate((uint64,bytes32,uint128)[])").to_vec();
    d.extend_from_slice(&word_u128(0x20));
    d.extend_from_slice(&word_u128(items.len() as u128));
    for (round, shard, amount) in items {
        d.extend_from_slice(&word_u128(u128::from(*round)));
        d.extend_from_slice(&shard.0);
        d.extend_from_slice(&word_u128(*amount));
    }
    d
}

/// [`AccountSource`] through a node: `eth_call` of `accounts`. The server reads at the
/// `finalized` block ([`ChainAccounts::new`]), so a deposit a reorg could undo is not spent; the
/// reporter reads `latest` ([`ChainAccounts::latest`]): what it sends, the contract rechecks.
pub struct ChainAccounts<T: JsonRpc> {
    rpc: T,
    contract: [u8; 20],
    tag: &'static str,
}

impl<T: JsonRpc> ChainAccounts<T> {
    /// Reads of the `OnyxPrepaid` at `contract` through `rpc`, at the finalized block.
    pub fn new(rpc: T, contract: [u8; 20]) -> Self {
        ChainAccounts {
            rpc,
            contract,
            tag: "finalized",
        }
    }

    /// Reads at the latest block (the reporter's).
    pub fn latest(rpc: T, contract: [u8; 20]) -> Self {
        ChainAccounts {
            rpc,
            contract,
            tag: "latest",
        }
    }
}

impl<T: JsonRpc> AccountSource for ChainAccounts<T> {
    fn account<'a>(&'a self, buyer: [u8; 20]) -> BoxFuture<'a, Result<Account, String>> {
        Box::pin(async move {
            let r = self
                .rpc
                .call(
                    "eth_call",
                    json!([{
                        "to": format!("0x{}", hex::encode(self.contract)),
                        "data": format!("0x{}", hex::encode(accounts_calldata(&buyer))),
                    }, self.tag]),
                )
                .await?;
            let ret = hex::decode(r.as_str().unwrap_or_default().trim_start_matches("0x"))
                .map_err(|e| format!("accounts: {e}"))?;
            decode_account(&ret)
        })
    }
}

/// What one reporter pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Vouchers redeemed, and the redeem transaction.
    pub redeemed: usize,
    /// The redeem transaction, when one was sent and mined.
    pub redeem_tx: Option<String>,
    /// Books paid, and the total paid to them.
    pub allocated: usize,
    /// The total allocated.
    pub allocated_amount: u128,
    /// The allocate transaction, when one was sent and mined.
    pub allocate_tx: Option<String>,
    /// Usage lines left for a later pass (their voucher is not redeemed yet).
    pub waiting: usize,
}

/// The reporter's prepaid pass: redeem, then allocate.
pub struct PrepaidReporter<T: JsonRpc> {
    state: Arc<PrepaidState>,
    reader: Arc<dyn AccountSource>,
    sender: Arc<Sender<T>>,
    contract: [u8; 20],
    marks: PathBuf,
    deadline: Duration,
}

impl<T: JsonRpc> PrepaidReporter<T> {
    /// A reporter over `state`'s journal, reading accounts with `reader` (latest block) and
    /// sending with `sender` (the contract's reporter key) to the `OnyxPrepaid` at `contract`;
    /// each transaction waits at most `deadline`. It never changes the server's books.
    pub fn new(
        state: Arc<PrepaidState>,
        reader: Arc<dyn AccountSource>,
        sender: Arc<Sender<T>>,
        contract: [u8; 20],
        deadline: Duration,
    ) -> Self {
        let mut marks = state.path().as_os_str().to_owned();
        marks.push(".allocated");
        PrepaidReporter {
            state,
            reader,
            sender,
            contract,
            marks: PathBuf::from(marks),
            deadline,
        }
    }

    fn read_marks(&self) -> Result<BTreeSet<usize>, String> {
        match std::fs::read_to_string(&self.marks) {
            Ok(t) => serde_json::from_str::<Vec<usize>>(&t)
                .map(|v| v.into_iter().collect())
                .map_err(|e| format!("{}: {e}", self.marks.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(e) => Err(format!("{}: {e}", self.marks.display())),
        }
    }

    fn write_marks(&self, marks: &BTreeSet<usize>) -> Result<(), String> {
        let tmp = self.marks.with_extension("allocated.tmp");
        let text =
            serde_json::to_string(&marks.iter().collect::<Vec<_>>()).map_err(|e| e.to_string())?;
        let mut open = std::fs::OpenOptions::new();
        open.create(true).write(true).truncate(true);
        crate::journal::owner_only(&mut open);
        let mut f = open
            .open(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::io::Write::write_all(&mut f, text.as_bytes())
            .and_then(|()| f.sync_all())
            .and_then(|()| std::fs::rename(&tmp, &self.marks))
            .map_err(|e| format!("{}: {e}", self.marks.display()))
    }

    /// One pass: read every buyer with usage, redeem each buyer's latest voucher above the
    /// chain's total (at most [`MAX_BATCH`]), then allocate the usage lines whose voucher the
    /// chain has redeemed and no earlier pass marked.
    pub async fn pass(&self) -> Result<PassReport, String> {
        let mut report = PassReport::default();
        let usage = self.state.usage()?;
        let mut accounts = BTreeMap::new();
        let mut latest: BTreeMap<[u8; 20], Voucher> = BTreeMap::new();
        for u in &usage {
            if let std::collections::btree_map::Entry::Vacant(e) = accounts.entry(u.voucher.buyer) {
                e.insert(self.reader.account(u.voucher.buyer).await?);
            }
            latest.insert(u.voucher.buyer, u.voucher.clone());
        }
        let due: Vec<Voucher> = latest
            .into_values()
            .filter(|v| {
                accounts
                    .get(&v.buyer)
                    .is_some_and(|a| v.cumulative > a.redeemed && v.cumulative <= a.deposited)
            })
            .take(MAX_BATCH)
            .collect();
        if !due.is_empty() {
            let m = self
                .sender
                .send(&self.contract, &redeem_calldata(&due), 0, self.deadline)
                .await
                .map_err(|e| format!("redeem: {e}"))?;
            report.redeemed = due.len();
            report.redeem_tx = Some(m.hash);
            // Mined: the chain now holds these totals (a reorg that undid it would make the
            // allocation revert on the pool, and nothing would be paid).
            for v in &due {
                if let Some(a) = accounts.get_mut(&v.buyer) {
                    a.redeemed = a.redeemed.max(v.cumulative);
                }
            }
        }
        // Allocate: lines not marked whose voucher total the chain has redeemed.
        let mut marks = self.read_marks()?;
        let ready: Vec<&Usage> = usage
            .iter()
            .filter(|u| !marks.contains(&u.line))
            .filter(|u| {
                accounts
                    .get(&u.voucher.buyer)
                    .is_some_and(|a| u.voucher.cumulative <= a.redeemed)
            })
            .collect();
        report.waiting = usage
            .iter()
            .filter(|u| !marks.contains(&u.line))
            .count()
            .saturating_sub(ready.len());
        let mut books: BTreeMap<(u64, Hash32), (u128, Vec<usize>)> = BTreeMap::new();
        for u in &ready {
            if books.len() == MAX_BATCH && !books.contains_key(&(u.round, u.shard)) {
                continue;
            }
            let e = books.entry((u.round, u.shard)).or_default();
            e.0 = e.0.checked_add(u.price).ok_or("allocation overflow")?;
            e.1.push(u.line);
        }
        if books.is_empty() {
            return Ok(report);
        }
        let lines: Vec<usize> = books.values().flat_map(|(_, l)| l.clone()).collect();
        let items: Vec<(u64, Hash32, u128)> =
            books.iter().map(|((r, s), (a, _))| (*r, *s, *a)).collect();
        // Marked before the send: a crash or an unknown outcome never pays these lines twice.
        marks.extend(lines.iter().copied());
        self.write_marks(&marks)?;
        match self
            .sender
            .send(&self.contract, &allocate_calldata(&items), 0, self.deadline)
            .await
        {
            Ok(m) => {
                report.allocated = items.len();
                report.allocated_amount = items.iter().map(|i| i.2).sum();
                report.allocate_tx = Some(m.hash);
                Ok(report)
            }
            Err(e) => {
                let nothing_moved = matches!(
                    e,
                    SendError::WouldRevert(_)
                        | SendError::Reverted(_)
                        | SendError::Rpc(_)
                        | SendError::Busy
                        | SendError::Stuck(_)
                );
                if nothing_moved {
                    for l in &lines {
                        marks.remove(l);
                    }
                    self.write_marks(&marks)?;
                }
                Err(format!("allocate: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calldata_matches_the_abi() {
        // keccak256("accounts(address)")[..4], keccak256("redeem((address,uint128,bytes)[])")[..4]
        // and keccak256("allocate((uint64,bytes32,uint128)[])")[..4], recomputed here.
        assert_eq!(accounts_calldata(&[0xaa; 20]).len(), 36);
        let v = Voucher {
            buyer: [0xaa; 20],
            cumulative: 1000,
            signature: vec![7; 65],
        };
        let d = redeem_calldata(&[v.clone(), v]);
        // selector, offset, length, 2 heads, 2 tuples of (3 words + length + 3 words of bytes).
        assert_eq!(d.len(), 4 + 32 * (2 + 2) + 2 * 32 * 7);
        assert_eq!(&d[4 + 64..4 + 96], &word_u128(0x40));
        assert_eq!(&d[4 + 96..4 + 128], &word_u128(0x40 + 7 * 32));
        let a = allocate_calldata(&[(1, Hash32([3; 32]), 5), (2, Hash32([4; 32]), 6)]);
        assert_eq!(a.len(), 4 + 64 + 2 * 96);
    }

    #[test]
    fn an_account_decodes_exactly() {
        let mut ret = Vec::new();
        ret.extend_from_slice(&word_u128(5000));
        ret.extend_from_slice(&word_u128(1000));
        ret.extend_from_slice(&word_u128(0));
        assert_eq!(
            decode_account(&ret),
            Ok(Account {
                deposited: 5000,
                redeemed: 1000,
                exiting: false
            })
        );
        ret[95] = 9;
        assert!(decode_account(&ret).expect("ok").exiting);
        ret[0] = 1;
        assert!(decode_account(&ret).is_err(), "above uint128");
        assert!(decode_account(&ret[..64]).is_err());
    }
}
