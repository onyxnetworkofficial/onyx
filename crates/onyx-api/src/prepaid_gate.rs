//! Prepaid balances over HTTP (ROADMAP S7, ADR-0046 amendment 1, docs/spec/prepaid.md "HTTP").
//!
//! A buyer with a deposit in `OnyxPrepaid` pays a paid answer with the [`PREPAID_VOUCHER`] header
//! instead of an x402 payment: its signed running total, the last total the server took plus the
//! answer's price. The server reads the buyer's account from the chain ([`AccountSource`], at
//! most [`PrepaidState::max_age`] seconds old), takes the voucher ([`PrepaidBook::charge`]) and
//! appends it to its usage journal, synced to disk, BEFORE the answer leaves: a voucher is money,
//! so one the server cannot keep is refused and nothing is served. The journal is also the record
//! the reporter redeems and allocates from (ROADMAP S7 part 2), and it holds which book each
//! voucher paid for, which the chain never sees.
//!
//! The identical retry (same header bytes, resource and client) of an answer already paid is
//! served again without a new charge, also after a restart: the journal keeps each buyer's last
//! request key.
//!
//! Holds (ADR-0049, north-star loop NL8): a voucher for a paid observation round is HELD, not
//! taken ([`PrepaidState::hold`]): journalled with its ticket and fact, never counted and never
//! redeemed. When the round is served it is charged ([`PrepaidState::commit_hold`]: an ordinary
//! usage line, billed to the fact's book); when the round fails it is released
//! ([`PrepaidState::release_hold`]) and the buyer's next voucher starts from the total before it.
//! A buyer has at most one hold, and no other voucher is taken while it is pending.

use crate::prepaid::{Account, PrepaidBook, PrepaidError, Voucher};
use crate::x402::BoxFuture;
use onyx_core::Hash32;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The request header carrying a voucher: `0x<buyer, 20 bytes>:<cumulative, decimal>:0x<65-byte
/// signature r || s || v>`.
pub const PREPAID_VOUCHER: &str = "onyx-prepaid-voucher";
/// The response header of an answer paid with a voucher: `total=<cumulative>; spendable=<left>`.
pub const PREPAID_RESPONSE: &str = "onyx-prepaid-response";
/// The longest a voucher header may be (2 + 40 + 1 + 39 digits + 1 + 2 + 130).
const MAX_HEADER: usize = 220;

/// Parse a [`PREPAID_VOUCHER`] header. `None` when it is not exactly that shape.
pub fn parse_voucher(s: &str) -> Option<Voucher> {
    if s.len() > MAX_HEADER {
        return None;
    }
    let mut it = s.split(':');
    let (b, c, sig) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    let buyer: [u8; 20] = hex::decode(b.strip_prefix("0x")?).ok()?.try_into().ok()?;
    if c.is_empty() || !c.bytes().all(|d| d.is_ascii_digit()) || (c.len() > 1 && c.starts_with('0'))
    {
        return None;
    }
    let cumulative: u128 = c.parse().ok()?;
    let signature = hex::decode(sig.strip_prefix("0x")?).ok()?;
    if signature.len() != 65 {
        return None;
    }
    Some(Voucher {
        buyer,
        cumulative,
        signature,
    })
}

/// The header a client sends for `v`.
pub fn voucher_header(v: &Voucher) -> String {
    format!(
        "0x{}:{}:0x{}",
        hex::encode(v.buyer),
        v.cumulative,
        hex::encode(&v.signature)
    )
}

/// Reads a buyer's `OnyxPrepaid.accounts` entry at a finalized block. An error is never a zero
/// balance.
pub trait AccountSource: Send + Sync {
    /// The account of `buyer`.
    fn account<'a>(&'a self, buyer: [u8; 20]) -> BoxFuture<'a, Result<Account, String>>;
}

/// Why a voucher was not taken.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChargeError {
    /// The voucher itself is refused (see [`PrepaidError`]).
    #[error(transparent)]
    Refused(#[from] PrepaidError),
    /// The usage journal could not be written: nothing was taken and nothing is served.
    #[error("the server cannot record prepaid usage durably right now; nothing was charged")]
    Unavailable,
}

/// A voucher taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charged {
    /// The buyer's total after this answer.
    pub total: u128,
    /// What the buyer can still spend.
    pub spendable: u128,
    /// The identical retry of an answer already paid: nothing new was taken.
    pub replay: bool,
}

#[derive(Default)]
struct Buyer {
    book: PrepaidBook,
    /// When the account was last read (unix seconds); 0: never in this process.
    read_at: u64,
    /// The request key the latest voucher paid for.
    last_key: Option<Hash32>,
    /// The pending hold's facts, beside `book.held`.
    hold: Option<Held>,
}

struct Inner {
    buyers: BTreeMap<[u8; 20], Buyer>,
    /// Pending holds: ticket hash -> buyer.
    tickets: BTreeMap<Hash32, [u8; 20]>,
    /// Holds that ended, in journal order (a ticket answers after a restart).
    ended: Vec<(Held, HoldEnd)>,
    journal: std::fs::File,
    write_errors: u64,
}

/// The server's prepaid books and their usage journal.
pub struct PrepaidState {
    domain: [u8; 32],
    source: std::sync::Arc<dyn AccountSource>,
    /// The oldest account read a voucher is taken against (seconds). Well below the contract's
    /// `exitDelay` (docs/spec/prepaid.md rule 2).
    pub max_age: u64,
    /// The longest a hold may last (seconds): half the contract's `exitDelay` (R-OBS-8), so a
    /// buyer who starts its exit after a hold cannot withdraw before the hold is charged or
    /// released. 0 (the default): no holds. Set with [`Self::with_hold_limit`].
    pub hold_limit: u64,
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl PrepaidState {
    /// Books for the `OnyxPrepaid` of EIP-712 domain `domain`, with the usage journal at `path`
    /// (created if missing; each buyer's latest voucher and request key are loaded back). A torn
    /// last line (a crash during its write: that answer was never served) is dropped; any other
    /// bad line refuses the journal.
    pub fn open(
        path: impl AsRef<Path>,
        domain: [u8; 32],
        source: std::sync::Arc<dyn AccountSource>,
        max_age: u64,
    ) -> Result<PrepaidState, String> {
        let path = path.as_ref().to_path_buf();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("prepaid journal {}: {e}", path.display())),
        };
        let mut buyers: BTreeMap<[u8; 20], Buyer> = BTreeMap::new();
        let mut tickets = BTreeMap::new();
        let mut ended = Vec::new();
        let mut keep = text.len();
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let mut at = 0;
        for (i, line) in lines.iter().enumerate() {
            let last = i + 1 == lines.len();
            let parsed = line_of(line.trim_end_matches('\n')).filter(|_| line.ends_with('\n'));
            let Some(parsed) = parsed else {
                if last {
                    keep = at;
                    break;
                }
                return Err(format!("prepaid journal line {}: unreadable", i + 1));
            };
            replay(&mut buyers, &mut tickets, &mut ended, parsed)
                .map_err(|e| format!("prepaid journal line {}: {e}", i + 1))?;
            at += line.len();
        }
        // Owner-only, as the intake journal: it pairs each buyer with the books it paid for,
        // which the chain never shows (THREATS PRE-8).
        let mut open = std::fs::OpenOptions::new();
        open.create(true).append(true);
        crate::journal::owner_only(&mut open);
        let journal = open
            .open(&path)
            .and_then(|f| crate::journal::set_owner_only(&path).map(|()| f))
            .map_err(|e| format!("prepaid journal {}: {e}", path.display()))?;
        if keep < text.len() {
            journal
                .set_len(keep as u64)
                .and_then(|()| journal.sync_data())
                .map_err(|e| format!("prepaid journal {}: {e}", path.display()))?;
        }
        Ok(PrepaidState {
            domain,
            source,
            max_age,
            hold_limit: 0,
            path,
            inner: Mutex::new(Inner {
                buyers,
                tickets,
                ended,
                journal,
                write_errors: 0,
            }),
        })
    }

    /// Take holds of at most `secs` seconds (half the contract's `exitDelay`, R-OBS-8).
    pub fn with_hold_limit(mut self, secs: u64) -> Self {
        self.hold_limit = secs;
        self
    }

    /// The journal's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Failed journal writes since start (reported on `/v1/health`).
    pub fn write_errors(&self) -> u64 {
        self.inner.lock().map_or(1, |i| i.write_errors)
    }

    /// Read `buyer`'s account again when the last read is older than [`Self::max_age`].
    pub async fn refresh(&self, buyer: [u8; 20], now: u64) -> Result<(), String> {
        let fresh = {
            let i = self.inner.lock().map_err(|_| "prepaid books poisoned")?;
            i.buyers
                .get(&buyer)
                .is_some_and(|b| b.read_at != 0 && now.saturating_sub(b.read_at) <= self.max_age)
        };
        if fresh {
            return Ok(());
        }
        let account = self.source.account(buyer).await?;
        let mut i = self.inner.lock().map_err(|_| "prepaid books poisoned")?;
        let b = i.buyers.entry(buyer).or_default();
        b.book.refresh(account);
        b.read_at = now;
        Ok(())
    }

    /// Read `buyer`'s account now, whatever the last read's age (the reporter, before it redeems).
    pub async fn reread(&self, buyer: [u8; 20], now: u64) -> Result<Account, String> {
        let account = self.source.account(buyer).await?;
        let mut i = self.inner.lock().map_err(|_| "prepaid books poisoned")?;
        let b = i.buyers.entry(buyer).or_default();
        b.book.refresh(account);
        b.read_at = now;
        Ok(account)
    }

    /// Every complete line of the usage journal, in order (a torn last line is not one).
    pub fn usage(&self) -> Result<Vec<Usage>, String> {
        let _held = self.inner.lock().map_err(|_| "prepaid books poisoned")?;
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("prepaid journal {}: {e}", self.path.display()))?;
        let mut out = Vec::new();
        for (i, line) in text.split_inclusive('\n').enumerate() {
            if !line.ends_with('\n') {
                break;
            }
            match line_of(line.trim_end_matches('\n')) {
                Some(Line::Usage(mut u)) => {
                    u.line = i;
                    out.push(u);
                }
                Some(Line::Hold { .. } | Line::Release { .. }) => {}
                None => return Err(format!("prepaid journal line {}: unreadable", i + 1)),
            }
        }
        Ok(out)
    }

    /// Take `v` for an answer priced `price` on the book `(round, shard)`, for the request `key`:
    /// checked, then written to the journal and synced; only then may the answer be served. The
    /// identical retry of the latest paid request is a replay and takes nothing. Blocking (file
    /// IO): call it off the async threads.
    pub fn charge(
        &self,
        v: Voucher,
        price: u128,
        bill: (u64, Hash32),
        key: Hash32,
        now: u64,
    ) -> Result<Charged, ChargeError> {
        let mut guard = self.inner.lock().map_err(|_| ChargeError::Unavailable)?;
        let inner = &mut *guard;
        let b = inner.buyers.entry(v.buyer).or_default();
        if b.read_at == 0 || now.saturating_sub(b.read_at) > self.max_age {
            // No recent chain read of this buyer (the caller refreshes first): an exit or a
            // withdrawal since would go unseen.
            return Err(ChargeError::Unavailable);
        }
        if b.book.latest.as_ref() == Some(&v) && b.last_key == Some(key) {
            return Ok(Charged {
                total: b.book.total(),
                spendable: b.book.spendable(),
                replay: true,
            });
        }
        let before = b.book.clone();
        b.book.charge(v.clone(), price, &self.domain)?;
        let line = usage_line(&v, price, bill, key, now);
        if writeln!(inner.journal, "{line}")
            .and_then(|()| inner.journal.sync_data())
            .is_err()
        {
            b.book = before;
            inner.write_errors += 1;
            return Err(ChargeError::Unavailable);
        }
        b.last_key = Some(key);
        Ok(Charged {
            total: b.book.total(),
            spendable: b.book.spendable(),
            replay: false,
        })
    }

    /// Hold `v` for a paid observation round priced `price` (ADR-0049): checked like a charge
    /// ([`PrepaidBook::hold`]: exactly the last total plus the price, within the deposit, no exit
    /// and no other hold pending), lasting at most [`Self::hold_limit`], then journalled and
    /// synced. The held voucher is never counted or redeemed until [`Self::commit_hold`].
    /// Blocking (file IO): call it off the async threads.
    pub fn hold(&self, v: Voucher, price: u128, held: Held, now: u64) -> Result<(), ChargeError> {
        if self.hold_limit == 0 || held.deadline.saturating_sub(now) > self.hold_limit {
            return Err(PrepaidError::HoldTooLong {
                limit: self.hold_limit,
            }
            .into());
        }
        let mut guard = self.inner.lock().map_err(|_| ChargeError::Unavailable)?;
        let inner = &mut *guard;
        if inner.tickets.contains_key(&held.ticket) {
            return Err(ChargeError::Unavailable);
        }
        let b = inner.buyers.entry(v.buyer).or_default();
        if b.read_at == 0 || now.saturating_sub(b.read_at) > self.max_age {
            return Err(ChargeError::Unavailable);
        }
        b.book.hold(v.clone(), price, &self.domain)?;
        let line = hold_line(&v, price, &held);
        if !append(&mut inner.journal, &line) {
            if let Some(b) = inner.buyers.get_mut(&v.buyer) {
                b.book.release_hold();
            }
            inner.write_errors += 1;
            return Err(ChargeError::Unavailable);
        }
        inner.tickets.insert(held.ticket, v.buyer);
        if let Some(b) = inner.buyers.get_mut(&v.buyer) {
            b.hold = Some(held);
        }
        Ok(())
    }

    /// The round of the hold `ticket` is served: charge it, billed to `bill` (an ordinary usage
    /// line, so the reporter redeems and allocates it like any answer). `Ok(false)` when no such
    /// hold is pending. Blocking (file IO).
    pub fn commit_hold(
        &self,
        ticket: &Hash32,
        bill: (u64, Hash32),
        now: u64,
    ) -> Result<bool, ChargeError> {
        let mut guard = self.inner.lock().map_err(|_| ChargeError::Unavailable)?;
        let inner = &mut *guard;
        let Some(buyer) = inner.tickets.get(ticket).copied() else {
            return Ok(false);
        };
        let Some(b) = inner.buyers.get_mut(&buyer) else {
            return Ok(false);
        };
        let Some(v) = b.book.held.clone() else {
            return Ok(false);
        };
        let price = v.cumulative.saturating_sub(b.book.total());
        let line = usage_line(&v, price, bill, *ticket, now);
        if !append(&mut inner.journal, &line) {
            inner.write_errors += 1;
            return Err(ChargeError::Unavailable);
        }
        let Some(b) = inner.buyers.get_mut(&buyer) else {
            return Ok(false);
        };
        b.book.commit_hold();
        b.last_key = Some(*ticket);
        let held = b.hold.take();
        inner.tickets.remove(ticket);
        if let Some(h) = held {
            inner.ended.push((h, HoldEnd::Charged));
        }
        Ok(true)
    }

    /// The round of the hold `ticket` failed or missed its deadline: release it in full; the
    /// buyer's next voucher starts from the total before it. `Ok(false)` when no such hold is
    /// pending. Blocking (file IO).
    pub fn release_hold(&self, ticket: &Hash32, now: u64) -> Result<bool, ChargeError> {
        let mut guard = self.inner.lock().map_err(|_| ChargeError::Unavailable)?;
        let inner = &mut *guard;
        let Some(buyer) = inner.tickets.get(ticket).copied() else {
            return Ok(false);
        };
        if !append(&mut inner.journal, &release_line(ticket, &buyer, now)) {
            inner.write_errors += 1;
            return Err(ChargeError::Unavailable);
        }
        inner.tickets.remove(ticket);
        let held = inner.buyers.get_mut(&buyer).and_then(|b| {
            b.book.release_hold();
            b.hold.take()
        });
        if let Some(h) = held {
            inner.ended.push((h, HoldEnd::Released));
        }
        Ok(true)
    }

    /// The pending holds, oldest first.
    pub fn holds(&self) -> Vec<Held> {
        let Ok(i) = self.inner.lock() else {
            return Vec::new();
        };
        let mut v: Vec<Held> = i.buyers.values().filter_map(|b| b.hold.clone()).collect();
        v.sort_by_key(|h| (h.created, h.ticket));
        v
    }

    /// The holds that ended, in journal order.
    pub fn ended_holds(&self) -> Vec<(Held, HoldEnd)> {
        self.inner
            .lock()
            .map_or_else(|_| Vec::new(), |i| i.ended.clone())
    }

    /// Each buyer's latest voucher above what the chain redeemed: what the reporter redeems.
    pub fn to_redeem(&self) -> Vec<Voucher> {
        self.inner.lock().map_or_else(
            |_| Vec::new(),
            |i| {
                i.buyers
                    .values()
                    .filter_map(|b| b.book.to_redeem().cloned())
                    .collect()
            },
        )
    }
}

fn usage_line(v: &Voucher, price: u128, bill: (u64, Hash32), key: Hash32, now: u64) -> Value {
    json!({
        "buyer": format!("0x{}", hex::encode(v.buyer)),
        "cumulative": v.cumulative.to_string(),
        "signature": format!("0x{}", hex::encode(&v.signature)),
        "price": price.to_string(),
        "round": bill.0,
        "shard": format!("0x{}", hex::encode(bill.1 .0)),
        "key": format!("0x{}", hex::encode(key.0)),
        "at": now,
    })
}

/// One answer paid with a voucher, as its journal line records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    /// The line's index in the journal (0 first): what the reporter marks as allocated.
    pub line: usize,
    /// The voucher taken.
    pub voucher: Voucher,
    /// The answer's price.
    pub price: u128,
    /// The book it paid for.
    pub round: u64,
    /// The book's shard.
    pub shard: Hash32,
    /// The request key.
    pub key: Hash32,
}

fn usage_of(line: &str) -> Option<Usage> {
    let v: Value = serde_json::from_str(line).ok()?;
    let o = v.as_object()?;
    if o.len() != 8 {
        return None;
    }
    let hex_of = |k: &str| hex::decode(o.get(k)?.as_str()?.strip_prefix("0x")?).ok();
    let buyer: [u8; 20] = hex_of("buyer")?.try_into().ok()?;
    let signature = hex_of("signature")?;
    let key: [u8; 32] = hex_of("key")?.try_into().ok()?;
    let shard: [u8; 32] = hex_of("shard")?.try_into().ok()?;
    let round = o.get("round")?.as_u64()?;
    o.get("at")?.as_u64()?;
    let price = o.get("price")?.as_str()?.parse::<u128>().ok()?;
    let cumulative = o.get("cumulative")?.as_str()?.parse::<u128>().ok()?;
    if signature.len() != 65 || price == 0 {
        return None;
    }
    Some(Usage {
        line: 0,
        voucher: Voucher {
            buyer,
            cumulative,
            signature,
        },
        price,
        round,
        shard: Hash32(shard),
        key: Hash32(key),
    })
}

/// A hold's facts beside its held voucher (ADR-0049): which ticket, which fact, until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The hash of the ticket's secret ([`crate::observe::ticket_hash`]).
    pub ticket: Hash32,
    /// The metric.
    pub metric: String,
    /// The subject's text.
    pub subject: String,
    /// When the hold is released if the round is not final.
    pub deadline: u64,
    /// When the hold was taken.
    pub created: u64,
}

/// How a hold ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldEnd {
    /// Its round was served and the held voucher became the buyer's latest.
    Charged,
    /// Dropped: the buyer's next voucher starts from the total before it.
    Released,
}

/// One line of the journal.
enum Line {
    Usage(Usage),
    Hold { voucher: Voucher, held: Held },
    Release { ticket: Hash32, buyer: [u8; 20] },
}

fn line_of(line: &str) -> Option<Line> {
    if let Some(u) = usage_of(line) {
        return Some(Line::Usage(u));
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let o = v.as_object()?;
    let hex_of = |k: &str| hex::decode(o.get(k)?.as_str()?.strip_prefix("0x")?).ok();
    let buyer: [u8; 20] = hex_of("buyer")?.try_into().ok()?;
    o.get("at")?.as_u64()?;
    if o.len() == 3 {
        let ticket: [u8; 32] = hex_of("release")?.try_into().ok()?;
        return Some(Line::Release {
            ticket: Hash32(ticket),
            buyer,
        });
    }
    if o.len() != 9 {
        return None;
    }
    let ticket: [u8; 32] = hex_of("hold")?.try_into().ok()?;
    let signature = hex_of("signature")?;
    let cumulative = o.get("cumulative")?.as_str()?.parse::<u128>().ok()?;
    let price = o.get("price")?.as_str()?.parse::<u128>().ok()?;
    if signature.len() != 65 || price == 0 {
        return None;
    }
    Some(Line::Hold {
        voucher: Voucher {
            buyer,
            cumulative,
            signature,
        },
        held: Held {
            ticket: Hash32(ticket),
            metric: o.get("metric")?.as_str()?.to_owned(),
            subject: o.get("subject")?.as_str()?.to_owned(),
            deadline: o.get("deadline")?.as_u64()?,
            created: o.get("at")?.as_u64()?,
        },
    })
}

fn hold_line(v: &Voucher, price: u128, h: &Held) -> Value {
    json!({
        "hold": format!("0x{}", hex::encode(h.ticket.0)),
        "buyer": format!("0x{}", hex::encode(v.buyer)),
        "cumulative": v.cumulative.to_string(),
        "signature": format!("0x{}", hex::encode(&v.signature)),
        "price": price.to_string(),
        "metric": h.metric,
        "subject": h.subject,
        "deadline": h.deadline,
        "at": h.created,
    })
}

fn release_line(ticket: &Hash32, buyer: &[u8; 20], now: u64) -> Value {
    json!({
        "release": format!("0x{}", hex::encode(ticket.0)),
        "buyer": format!("0x{}", hex::encode(buyer)),
        "at": now,
    })
}

fn append(journal: &mut std::fs::File, line: &Value) -> bool {
    writeln!(journal, "{line}")
        .and_then(|()| journal.sync_data())
        .is_ok()
}

/// Apply one journal line at load. A voucher must rise above the buyer's latest; a usage line
/// while a hold is pending must be that hold's charge; a release must name the pending hold.
fn replay(
    buyers: &mut BTreeMap<[u8; 20], Buyer>,
    tickets: &mut BTreeMap<Hash32, [u8; 20]>,
    ended: &mut Vec<(Held, HoldEnd)>,
    line: Line,
) -> Result<(), &'static str> {
    match line {
        Line::Usage(Usage {
            voucher: v, key, ..
        }) => {
            let b = buyers.entry(v.buyer).or_default();
            if b.book.held.is_some() {
                if b.book.held.as_ref() != Some(&v) {
                    return Err("a voucher taken while a hold was pending");
                }
                b.book.commit_hold();
                if let Some(h) = b.hold.take() {
                    tickets.remove(&h.ticket);
                    ended.push((h, HoldEnd::Charged));
                }
            } else if b
                .book
                .latest
                .as_ref()
                .is_some_and(|l| l.cumulative >= v.cumulative)
            {
                return Err("total not rising");
            } else {
                b.book.latest = Some(v);
            }
            b.last_key = Some(key);
        }
        Line::Hold { voucher: v, held } => {
            let b = buyers.entry(v.buyer).or_default();
            if b.book.held.is_some()
                || tickets.contains_key(&held.ticket)
                || b.book
                    .latest
                    .as_ref()
                    .is_some_and(|l| l.cumulative >= v.cumulative)
            {
                return Err("a second hold, or a hold whose total does not rise");
            }
            tickets.insert(held.ticket, v.buyer);
            b.book.held = Some(v);
            b.hold = Some(held);
        }
        Line::Release { ticket, buyer } => {
            if tickets.get(&ticket) != Some(&buyer) {
                return Err("a release of no pending hold");
            }
            tickets.remove(&ticket);
            let b = buyers.entry(buyer).or_default();
            b.book.release_hold();
            if let Some(h) = b.hold.take() {
                ended.push((h, HoldEnd::Released));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepaid::domain_separator;
    use std::sync::Arc;

    struct Fixed(Account);
    impl AccountSource for Fixed {
        fn account<'a>(&'a self, _: [u8; 20]) -> BoxFuture<'a, Result<Account, String>> {
            let a = self.0;
            Box::pin(async move { Ok(a) })
        }
    }

    fn domain() -> [u8; 32] {
        domain_separator(84532, &[0x11; 20])
    }

    fn state(path: &Path, deposited: u128) -> PrepaidState {
        let src = Arc::new(Fixed(Account {
            deposited,
            redeemed: 0,
            exiting: false,
        }));
        PrepaidState::open(path, domain(), src, 60).expect("open")
    }

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn a_header_parses_in_one_spelling_only() {
        let v = Voucher::sign(&[1; 32], &domain(), 1000).expect("sign");
        let h = voucher_header(&v);
        assert_eq!(parse_voucher(&h), Some(v.clone()));
        let b = hex::encode(v.buyer);
        let s = hex::encode(&v.signature);
        for bad in [
            format!("{b}:1000:0x{s}"),
            format!("0x{b}:01000:0x{s}"),
            format!("0x{b}:+1000:0x{s}"),
            format!("0x{b}::0x{s}"),
            format!("0x{b}:1000:0x{s}00"),
            format!("0x{b}:1000:0x{s}:x"),
            format!("0x{b}:1000000000000000000000000000000000000000:0x{s}"),
        ] {
            assert_eq!(parse_voucher(&bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn a_voucher_is_journalled_before_it_counts_and_replays_after_a_restart() {
        let dir = crate::journal::test_dir("prepaid");
        let path = dir.join("prepaid.journal");
        let s = state(&path, 5000);
        let v1 = Voucher::sign(&[1; 32], &domain(), 1000).expect("sign");
        let shard = Hash32([3; 32]);
        // No chain read yet: nothing is taken.
        assert_eq!(
            s.charge(v1.clone(), 1000, (1, shard), Hash32(KEY), 10),
            Err(ChargeError::Unavailable)
        );
        s.refresh(v1.buyer, 10).await.expect("read");
        let c = s
            .charge(v1.clone(), 1000, (1, shard), Hash32(KEY), 10)
            .expect("taken");
        assert_eq!((c.total, c.spendable, c.replay), (1000, 4000, false));
        // The identical retry is a replay; the same voucher for another request is refused.
        assert!(
            s.charge(v1.clone(), 1000, (1, shard), Hash32(KEY), 11)
                .expect("replay")
                .replay
        );
        assert!(matches!(
            s.charge(v1.clone(), 1000, (1, shard), Hash32([8; 32]), 11),
            Err(ChargeError::Refused(PrepaidError::WrongTotal {
                last: 1000,
                price: 1000
            }))
        ));
        assert_eq!(
            std::fs::read_to_string(&path)
                .expect("journal")
                .lines()
                .count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the journal is owner-only");
        }
        // A restart loads the latest voucher and its request key back.
        drop(s);
        let s = state(&path, 5000);
        s.refresh(v1.buyer, 20).await.expect("read");
        assert!(
            s.charge(v1.clone(), 1000, (1, shard), Hash32(KEY), 20)
                .expect("replay")
                .replay
        );
        let v2 = Voucher::sign(&[1; 32], &domain(), 2000).expect("sign");
        assert_eq!(
            s.charge(v2, 1000, (1, shard), Hash32([9; 32]), 21)
                .expect("taken")
                .total,
            2000
        );
        assert_eq!(
            s.to_redeem()
                .iter()
                .map(|v| v.cumulative)
                .collect::<Vec<_>>(),
            vec![2000]
        );
    }

    #[tokio::test]
    async fn a_torn_last_line_is_dropped_and_other_damage_refuses_the_journal() {
        let dir = crate::journal::test_dir("prepaid");
        let path = dir.join("prepaid.journal");
        let s = state(&path, 5000);
        let v1 = Voucher::sign(&[1; 32], &domain(), 1000).expect("sign");
        s.refresh(v1.buyer, 10).await.expect("read");
        s.charge(v1.clone(), 1000, (1, Hash32([3; 32])), Hash32(KEY), 10)
            .expect("taken");
        drop(s);
        let good = std::fs::read_to_string(&path).expect("journal");
        std::fs::write(&path, format!("{good}{{\"buyer\":\"0x")).expect("tear");
        let s = state(&path, 5000);
        assert_eq!(
            std::fs::read_to_string(&path).expect("journal"),
            good,
            "the torn line is cut"
        );
        // The journal's voucher is still owed after the restart (the reporter reads each account
        // before it redeems, so a total the chain already redeemed is not sent again).
        assert_eq!(s.to_redeem(), vec![v1]);
        drop(s);
        std::fs::write(&path, format!("not json\n{good}")).expect("damage");
        let src = Arc::new(Fixed(Account::default()));
        assert!(PrepaidState::open(&path, domain(), src.clone(), 60).is_err());
        std::fs::write(&path, format!("{good}{good}")).expect("repeat");
        assert!(
            PrepaidState::open(&path, domain(), src, 60).is_err(),
            "a total that does not rise"
        );
    }

    fn held(ticket: u8) -> Held {
        Held {
            ticket: Hash32([ticket; 32]),
            metric: "test.daily".into(),
            subject: "test.daily:2026-10-20".into(),
            deadline: 100,
            created: 10,
        }
    }

    /// ADR-0049: holds, charges and releases are journalled and come back after a restart; a
    /// journal whose lines break the hold rules is refused.
    #[tokio::test]
    async fn holds_are_journalled_and_replayed() {
        let dir = crate::journal::test_dir("prepaid-hold");
        let path = dir.join("prepaid.journal");
        let open = || state(&path, 5000).with_hold_limit(600);
        let s = open();
        let v = |total| Voucher::sign(&[1; 32], &domain(), total).expect("sign");
        s.refresh(v(1).buyer, 10).await.expect("read");
        s.hold(v(1000), 1000, held(1), 10).expect("held");
        assert_eq!(
            s.hold(v(2000), 1000, held(2), 10),
            Err(ChargeError::Refused(PrepaidError::HoldPending))
        );
        assert_eq!(s.release_hold(&Hash32([1; 32]), 20), Ok(true));
        assert_eq!(s.release_hold(&Hash32([1; 32]), 20), Ok(false));
        s.hold(v(1000), 1000, held(2), 20)
            .expect("held again at the same total");
        drop(s);
        let s = open();
        assert_eq!(s.holds(), vec![held(2)]);
        assert!(s.to_redeem().is_empty());
        assert_eq!(
            s.commit_hold(&Hash32([2; 32]), (7, Hash32([4; 32])), 30),
            Ok(true)
        );
        assert_eq!(
            s.usage()
                .expect("usage")
                .iter()
                .map(|u| (u.line, u.round, u.price))
                .collect::<Vec<_>>(),
            vec![(3, 7, 1000)],
            "only the charge is a usage line, at its journal line"
        );
        drop(s);
        let s = open();
        assert!(s.holds().is_empty());
        assert_eq!(
            s.ended_holds(),
            vec![(held(1), HoldEnd::Released), (held(2), HoldEnd::Charged)]
        );
        assert_eq!(
            s.to_redeem()
                .iter()
                .map(|v| v.cumulative)
                .collect::<Vec<_>>(),
            vec![1000]
        );
        drop(s);
        // A voucher taken while a hold is pending, or a release of no hold: refused.
        let text = std::fs::read_to_string(&path).expect("journal");
        let lines: Vec<&str> = text.lines().collect();
        let src = Arc::new(Fixed(Account::default()));
        let bad = |body: String| {
            std::fs::write(&path, body).expect("write");
            PrepaidState::open(&path, domain(), src.clone(), 60).is_err()
        };
        let other = usage_line(&v(1500), 500, (1, Hash32([3; 32])), Hash32(KEY), 25);
        assert!(bad(format!("{}\n{other}\n", lines[2])));
        assert!(bad(format!("{}\n", lines[1])));
        assert!(!bad(text.clone()), "the real journal still loads");
    }

    #[tokio::test]
    async fn a_hold_needs_a_limit_and_a_fresh_read() {
        let dir = crate::journal::test_dir("prepaid-hold");
        let v = Voucher::sign(&[1; 32], &domain(), 1000).expect("sign");
        let s = state(&dir.join("j"), 5000);
        s.refresh(v.buyer, 10).await.expect("read");
        assert_eq!(
            s.hold(v.clone(), 1000, held(1), 10),
            Err(ChargeError::Refused(PrepaidError::HoldTooLong { limit: 0 })),
            "holds are off by default"
        );
        let s = state(&dir.join("k"), 5000).with_hold_limit(600);
        assert_eq!(
            s.hold(v.clone(), 1000, held(1), 10),
            Err(ChargeError::Unavailable),
            "no chain read yet"
        );
        s.refresh(v.buyer, 10).await.expect("read");
        assert_eq!(s.hold(v, 1000, held(1), 10), Ok(()));
    }

    #[tokio::test]
    async fn an_exit_or_a_stale_read_stops_vouchers() {
        let dir = crate::journal::test_dir("prepaid");
        let src = Arc::new(Fixed(Account {
            deposited: 5000,
            redeemed: 0,
            exiting: true,
        }));
        let s = PrepaidState::open(dir.join("j"), domain(), src, 60).expect("open");
        let v = Voucher::sign(&[1; 32], &domain(), 1000).expect("sign");
        s.refresh(v.buyer, 10).await.expect("read");
        assert_eq!(
            s.charge(v.clone(), 1000, (1, Hash32([3; 32])), Hash32(KEY), 10),
            Err(ChargeError::Refused(PrepaidError::Exiting))
        );
        // A read older than max_age is not charged against.
        let dir = crate::journal::test_dir("prepaid");
        let s = state(&dir.join("j"), 5000);
        s.refresh(v.buyer, 10).await.expect("read");
        assert_eq!(
            s.charge(v.clone(), 1000, (1, Hash32([3; 32])), Hash32(KEY), 71),
            Err(ChargeError::Unavailable)
        );
        s.refresh(v.buyer, 71).await.expect("read again");
        assert!(s
            .charge(v, 1000, (1, Hash32([3; 32])), Hash32(KEY), 71)
            .is_ok());
    }
}
