//! The money rules of `OnyxStorage` (R-STOR-8..11), a model of its books,
//! and the replay of its money events into those books ([`Replay`]).
//!
//! Every amount is in tONYX base units (18 decimals) as an integer; rates
//! are per GiB per epoch; probabilities are ppm (ADR-0009). Every division
//! rounds down, and what rounding leaves stays in the deal and goes back to
//! the payer when the deal closes, so nothing is created or lost
//! (THREATS STORAGE-10). The prices are the deployer's placeholders
//! (docs/STORAGE-NETWORK.md "Cost model"), never market prices.

use crate::abi::{self, events, AbiError};
use crate::{GIB, PPM};
use onyx_core::Hash32;
use std::collections::BTreeMap;

/// Part of every bond `register` keeps for the treasury and never returns
/// (`OnyxStorage.REGISTRATION_FEE_PPM`, 2%; ADR-0060 amendment 1 item 2,
/// THREATS STORAGE-S16-R2-1): each try at grinding a node id costs a fee.
pub const REGISTRATION_FEE_PPM: u32 = 20_000;
/// Part of a missed answer's slash credited to whoever settled it, unless
/// that is the node's own operator (`OnyxStorage.REPORTER_PPM`, 10%;
/// amendment 1 item 3, THREATS STORAGE-S16-R2-3).
pub const REPORTER_PPM: u32 = 100_000;

/// The contract's immutable parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// Paid per GiB of the manifest, per replica, per epoch.
    pub price_per_gib_epoch: u128,
    /// Bond per declared GiB of capacity.
    pub bond_per_gib: u128,
    /// Smallest bond (a node whose bond falls below half of it is jailed).
    pub min_bond: u128,
    /// Part of the current bond a failed or missed answer takes (ppm).
    pub slash_ppm: u32,
    /// Challenge slots per deal per epoch.
    pub challenges_per_deal: u32,
    /// Failed or missed answers in a row that jail a node.
    pub max_misses: u32,
}

/// GiB a deal pays for: the manifest's bytes rounded up, at least 1.
pub fn gib(bytes: u64) -> u64 {
    bytes.div_ceil(GIB).max(1)
}

/// A shard is billed as at least this many bytes (`OnyxStorageLib.MIN_BILLED_SHARD_BYTES`).
pub const MIN_BILLED_SHARD_BYTES: u64 = 8 << 20;

/// The bytes a deal pays for: its bytes or 8 MiB per shard, whichever is more (a node's work
/// grows with shards, not only bytes; review STORAGE-R2-5). Pass this to [`deposit`].
pub fn billable_bytes(bytes: u64, shards: u32) -> u64 {
    bytes.max(u64::from(shards).saturating_mul(MIN_BILLED_SHARD_BYTES))
}

/// GiB a deal pays for, as `OnyxStorageLib.billableGiB` computes it.
pub fn billable_gib(bytes: u64, shards: u32) -> u64 {
    gib(billable_bytes(bytes, shards))
}

/// What one epoch of a deal pays (`price × gib × r`).
pub fn epoch_pay(p: &Params, bytes: u64, r: u8) -> u128 {
    p.price_per_gib_epoch
        .saturating_mul(u128::from(gib(bytes)))
        .saturating_mul(u128::from(r))
}

/// The deposit for `epochs` epochs.
pub fn deposit(p: &Params, bytes: u64, r: u8, epochs: u32) -> u128 {
    epoch_pay(p, bytes, r).saturating_mul(u128::from(epochs))
}

/// What one passed answer earns: the epoch's pay split over every
/// (slot, replica) pair, rounded down.
pub fn slot_pay(p: &Params, bytes: u64, r: u8) -> u128 {
    let parts = u128::from(p.challenges_per_deal) * u128::from(r);
    if parts == 0 {
        return 0;
    }
    epoch_pay(p, bytes, r) / parts
}

/// The bond a node declaring `capacity_gib` must post.
pub fn bond_required(p: &Params, capacity_gib: u64) -> u128 {
    p.bond_per_gib
        .saturating_mul(u128::from(capacity_gib))
        .max(p.min_bond)
}

/// What one slash takes from `bond`.
pub fn slash(p: &Params, bond: u128) -> u128 {
    bond.saturating_mul(u128::from(p.slash_ppm)) / u128::from(PPM)
}

/// The fee `register` keeps from `bond` (the node activates with, or a
/// cancel returns, `bond - fee`).
pub fn registration_fee(bond: u128) -> u128 {
    bond.saturating_mul(u128::from(REGISTRATION_FEE_PPM)) / u128::from(PPM)
}

/// What the reporter of a miss earns from a slash of `slashed`.
pub fn miss_reward(slashed: u128) -> u128 {
    slashed.saturating_mul(u128::from(REPORTER_PPM)) / u128::from(PPM)
}

/// A node whose bond falls below this is jailed: half of the bond it
/// activated with or of `min_bond`, whichever is larger (amendment 1 item 4;
/// at 10% slashes, about 7 of them whatever the bond).
pub fn jail_below(p: &Params, start_bond: u128) -> u128 {
    start_bond.max(p.min_bond) / 2
}

/// A model of the contract's books: every flow in and out, for the
/// conservation property (R-STOR-11), checked here and by the contract's
/// invariant suite.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Books {
    /// Deposits paid in by publishers.
    pub deposits: u128,
    /// Bonds paid in by nodes (whole, fee included).
    pub bonds_in: u128,
    /// Credited to nodes for passed answers.
    pub paid: u128,
    /// Taken from bonds for the treasury: slashes and registration fees.
    pub slashed: u128,
    /// Returned to payers when deals closed.
    pub refunded: u128,
    /// Bonds returned to operators.
    pub bonds_out: u128,
    /// Earnings withdrawn by operators.
    pub earnings_out: u128,
    /// Slashes sent to the treasury.
    pub swept: u128,
    /// The part of `slashed` credited to reporters of misses.
    pub rewarded: u128,
    /// Token balance of the contract.
    pub balance: u128,
}

impl Books {
    /// Held for deals (deposits not yet paid or refunded).
    pub fn deal_held(&self) -> u128 {
        self.deposits - self.paid - self.refunded
    }

    /// Held as bonds.
    pub fn bonds_held(&self) -> u128 {
        self.bonds_in - self.slashed - self.bonds_out
    }

    /// Earned (pay and reporters' rewards) and not withdrawn.
    pub fn earnings_held(&self) -> u128 {
        self.paid + self.rewarded - self.earnings_out
    }

    /// Slashed or taken as a fee, not rewarded, not yet sent to the treasury.
    pub fn treasury_held(&self) -> u128 {
        self.slashed - self.rewarded - self.swept
    }

    /// R-STOR-11 (`OnyxStorage.conserved`): deposits = paid + refunded + deal
    /// held; bonds in = slashed + bonds out + bonds held; earnings out are at
    /// most paid plus rewarded; swept plus rewarded is at most slashed; and the
    /// balance is at least what is held (tokens sent outside every flow stay as
    /// a surplus, as `OnyxStorage.conserved()` reads them; review STORAGE-S16-R4).
    pub fn conserved(&self) -> bool {
        self.paid
            .checked_add(self.refunded)
            .is_some_and(|x| x <= self.deposits)
            && self
                .slashed
                .checked_add(self.bonds_out)
                .is_some_and(|x| x <= self.bonds_in)
            && self
                .paid
                .checked_add(self.rewarded)
                .is_some_and(|x| self.earnings_out <= x)
            && self
                .swept
                .checked_add(self.rewarded)
                .is_some_and(|x| x <= self.slashed)
            && self.balance
                >= self.deal_held()
                    + self.bonds_held()
                    + self.earnings_held()
                    + self.treasury_held()
    }

    /// The nine totals in the order `OnyxStorage.books()` returns them.
    pub fn totals(&self) -> [u128; 9] {
        [
            self.deposits,
            self.bonds_in,
            self.paid,
            self.slashed,
            self.refunded,
            self.bonds_out,
            self.earnings_out,
            self.swept,
            self.rewarded,
        ]
    }
}

/// Why a log does not replay.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    /// Its data or topics do not decode.
    #[error(transparent)]
    Abi(#[from] AbiError),
    /// A cancellation of a registration not seen (logs missing or out of order).
    #[error("RegistrationCancelled for registration {0}, which was not seen")]
    UnknownRegistration(u64),
    /// A flow out larger than what is held for it (logs missing or out of order).
    #[error("{0} goes below zero")]
    Negative(&'static str),
    /// A total past 2^128 (no real contract gets there).
    #[error("a total overflows")]
    Overflow,
}

/// The money events of `OnyxStorage` replayed into [`Books`], so the totals
/// of `books()` can be rebuilt from logs alone. Every money event counts:
/// `Registered` (bond in), `RegistrationFee` (to the treasury's part),
/// `RegistrationCancelled` (the bond less the fee out), `DealCreated` and
/// `DealExtended` (deposits), `Answered` (pay), `Slashed`, `MissReward` (a
/// reporter's share of a slash), `DealClosed` (refund), `BondWithdrawn`,
/// `EarningsWithdrawn` and `TreasurySwept`. The balance is what the events
/// moved in less what they moved out. Logs go in chain order; others are
/// skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replay {
    books: Books,
    /// Registration index -> the bond it holds (after its fee) until it is
    /// activated or cancelled.
    regs: BTreeMap<u64, u128>,
}

fn topic_u64(topics: &[Hash32], i: usize) -> Result<u64, AbiError> {
    let t = topics.get(i).ok_or(AbiError("missing topic"))?;
    abi::word_u64(&t.0, 0)
}

fn add(x: &mut u128, v: u128) -> Result<(), ReplayError> {
    *x = x.checked_add(v).ok_or(ReplayError::Overflow)?;
    Ok(())
}

impl Replay {
    /// An empty replay (a contract just deployed).
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one log of the contract (`topics[0]` names its event).
    pub fn apply(&mut self, topics: &[Hash32], data: &[u8]) -> Result<(), ReplayError> {
        let Some(t) = topics.first() else {
            return Ok(());
        };
        let is = |sig: &str| *t == abi::topic(sig);
        let b = &mut self.books;
        if is(events::REGISTERED) {
            let bond = abi::word_uint(data, 1)?;
            add(&mut b.bonds_in, bond)?;
            add(&mut b.balance, bond)?;
            self.regs.insert(topic_u64(topics, 1)?, bond);
        } else if is(events::REGISTRATION_FEE) {
            let fee = abi::word_uint(data, 0)?;
            add(&mut b.slashed, fee)?;
            let i = topic_u64(topics, 1)?;
            let r = self
                .regs
                .get_mut(&i)
                .ok_or(ReplayError::UnknownRegistration(i))?;
            *r = r.checked_sub(fee).ok_or(ReplayError::Negative("a bond"))?;
        } else if is(events::ACTIVATED) {
            // The bond stays in the contract as the node's (no token moves).
            self.regs.remove(&topic_u64(topics, 2)?);
        } else if is(events::REGISTRATION_CANCELLED) {
            let i = topic_u64(topics, 1)?;
            let back = self
                .regs
                .remove(&i)
                .ok_or(ReplayError::UnknownRegistration(i))?;
            add(&mut b.bonds_out, back)?;
            b.balance = b
                .balance
                .checked_sub(back)
                .ok_or(ReplayError::Negative("the balance"))?;
        } else if is(events::DEAL_CREATED) {
            let d = abi::word_uint(data, 8)?;
            add(&mut b.deposits, d)?;
            add(&mut b.balance, d)?;
        } else if is(events::DEAL_EXTENDED) {
            let d = abi::word_uint(data, 1)?;
            add(&mut b.deposits, d)?;
            add(&mut b.balance, d)?;
        } else if is(events::ANSWERED) {
            add(&mut b.paid, abi::word_uint(data, 2)?)?;
        } else if is(events::SLASHED) {
            add(&mut b.slashed, abi::word_uint(data, 3)?)?;
        } else if is(events::MISS_REWARD) {
            add(&mut b.rewarded, abi::word_uint(data, 0)?)?;
        } else {
            let out = if is(events::DEAL_CLOSED) {
                &mut b.refunded
            } else if is(events::BOND_WITHDRAWN) {
                &mut b.bonds_out
            } else if is(events::EARNINGS_WITHDRAWN) {
                &mut b.earnings_out
            } else if is(events::TREASURY_SWEPT) {
                &mut b.swept
            } else {
                return Ok(());
            };
            let v = abi::word_uint(data, 0)?;
            add(out, v)?;
            b.balance = b
                .balance
                .checked_sub(v)
                .ok_or(ReplayError::Negative("the balance"))?;
        }
        Ok(())
    }

    /// The books so far.
    pub fn books(&self) -> &Books {
        &self.books
    }
}

/// Replay `logs` (topics, data), in chain order, into the contract's books.
pub fn replay<'a, I>(logs: I) -> Result<Books, ReplayError>
where
    I: IntoIterator<Item = (&'a [Hash32], &'a [u8])>,
{
    let mut r = Replay::new();
    for (t, d) in logs {
        r.apply(t, d)?;
    }
    Ok(r.books)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params {
            price_per_gib_epoch: 1_000_000_000_000_007, // an odd number: rounding shows
            bond_per_gib: 10u128.pow(16),
            min_bond: 10u128.pow(18),
            slash_ppm: 100_000,
            challenges_per_deal: 2,
            max_misses: 3,
        }
    }

    #[test]
    fn amounts() {
        let p = params();
        assert_eq!(gib(1), 1);
        assert_eq!(gib(GIB), 1);
        assert_eq!(gib(GIB + 1), 2);
        // Review STORAGE-R2-5: tiny shards are billed as 8 MiB each.
        assert_eq!(billable_gib(100_000, 1), 1);
        assert_eq!(billable_gib(65_536 * 8, 65_536), 512);
        assert_eq!(billable_gib(3 * GIB, 128), 3);
        assert_eq!(billable_gib(3 * GIB, 129 * 3), 4);
        assert_eq!(epoch_pay(&p, 3 * GIB, 3), p.price_per_gib_epoch * 9);
        assert_eq!(deposit(&p, 3 * GIB, 3, 10), p.price_per_gib_epoch * 90);
        // 9 × price over 2 slots × 3 replicas, rounded down.
        assert_eq!(slot_pay(&p, 3 * GIB, 3), p.price_per_gib_epoch * 9 / 6);
        assert!(slot_pay(&p, 3 * GIB, 3) * 6 <= epoch_pay(&p, 3 * GIB, 3));
        assert_eq!(bond_required(&p, 0), p.min_bond);
        assert_eq!(bond_required(&p, 1_000), 10u128.pow(19));
        assert_eq!(slash(&p, 10u128.pow(18)), 10u128.pow(17));
        assert_eq!(slash(&p, 9), 0);
        // Amendment 1: 2% of a bond is kept; 10% of a slash goes to its reporter.
        assert_eq!(registration_fee(10u128.pow(18)), 2 * 10u128.pow(16));
        assert_eq!(registration_fee(49), 0);
        assert_eq!(miss_reward(98 * 10u128.pow(15)), 98 * 10u128.pow(14));
        assert_eq!(miss_reward(9), 0);
        // Jail below half of the larger of the start bond and the minimum.
        assert_eq!(jail_below(&p, 98 * 10u128.pow(18)), 49 * 10u128.pow(18));
        assert_eq!(jail_below(&p, 98 * 10u128.pow(16)), 5 * 10u128.pow(17));
    }

    /// Amendment 1 item 4: 10% slashes from the bond a node activated with
    /// (after the 2% fee) jail it at the 7th slash, at the minimum or at 100
    /// times it; under the old rule (half of the minimum) the large one took 51.
    #[test]
    fn a_node_is_jailed_after_seven_slashes_whatever_its_bond() {
        let p = params();
        for start in [p.min_bond, 100 * p.min_bond] {
            let start = start - registration_fee(start);
            let mut bond = start;
            let mut n = 0;
            while bond >= jail_below(&p, start) {
                bond -= slash(&p, bond);
                n += 1;
            }
            assert_eq!(n, 7, "start bond {start}");
        }
        let mut bond = 98 * p.min_bond;
        let mut old = 0;
        while bond >= p.min_bond / 2 {
            bond -= slash(&p, bond);
            old += 1;
        }
        assert_eq!(old, 51);
    }

    /// A small xorshift (no floats, no clock): the property run is the same
    /// every time.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// The contract's money paths as operations on the books: a deal pays at
    /// most one slot share per (slot, replica) per epoch, a slash never
    /// exceeds the bond, closing refunds exactly what is left.
    #[test]
    fn books_conserve_under_random_operations() {
        let p = params();
        let mut rng = Rng(0x5eed);
        for _ in 0..200 {
            let mut b = Books::default();
            let mut deals: Vec<(u128, u128, bool, u128)> = Vec::new(); // deposit, paid, closed, slot
            let mut bonds: Vec<u128> = Vec::new();
            for _ in 0..200 {
                match rng.next() % 7 {
                    0 => {
                        let bytes = rng.next() % (5 * GIB) + 1;
                        let r = (rng.next() % 5 + 1) as u8;
                        let epochs = (rng.next() % 9 + 1) as u32;
                        let d = deposit(&p, bytes, r, epochs);
                        b.deposits += d;
                        b.balance += d;
                        deals.push((d, 0, false, slot_pay(&p, bytes, r)));
                    }
                    1 => {
                        // The fee is the treasury's at once; the node keeps the rest.
                        let bond = bond_required(&p, rng.next() % 5_000);
                        let fee = registration_fee(bond);
                        b.bonds_in += bond;
                        b.balance += bond;
                        b.slashed += fee;
                        bonds.push(bond - fee);
                    }
                    2 if !deals.is_empty() => {
                        let i = (rng.next() as usize) % deals.len();
                        let (dep, paid, closed, s) = deals[i];
                        if !closed && paid + s <= dep {
                            deals[i].1 += s;
                            b.paid += s;
                        }
                    }
                    3 if !bonds.is_empty() => {
                        // A slash; half of them reported by someone paid a share.
                        let i = (rng.next() as usize) % bonds.len();
                        let s = slash(&p, bonds[i]);
                        bonds[i] -= s;
                        b.slashed += s;
                        if rng.next().is_multiple_of(2) {
                            b.rewarded += miss_reward(s);
                        }
                    }
                    4 if !deals.is_empty() => {
                        let i = (rng.next() as usize) % deals.len();
                        if !deals[i].2 {
                            let left = deals[i].0 - deals[i].1;
                            deals[i].2 = true;
                            b.refunded += left;
                            b.balance -= left;
                        }
                    }
                    5 if !bonds.is_empty() => {
                        let i = (rng.next() as usize) % bonds.len();
                        b.bonds_out += bonds[i];
                        b.balance -= bonds[i];
                        bonds[i] = 0;
                    }
                    _ => {
                        let e = b.earnings_held();
                        b.earnings_out += e;
                        b.balance -= e;
                        let t = b.treasury_held();
                        b.swept += t;
                        b.balance -= t;
                    }
                }
                assert!(b.conserved(), "{b:?}");
            }
        }
    }

    use crate::abi::{b32, encode, topic, uint, word_u128, Tok};

    /// A log: its topics and its data.
    type Log = (Vec<Hash32>, Vec<u8>);

    /// One log as the contract emits it: topic 0, the indexed words, the data.
    fn log(sig: &str, indexed: &[[u8; 32]], data: &[Tok]) -> Log {
        let mut t = vec![topic(sig)];
        t.extend(indexed.iter().map(|w| Hash32(*w)));
        (t, encode(data))
    }

    fn w(v: u128) -> [u8; 32] {
        word_u128(v)
    }

    fn wa(a: u8) -> [u8; 32] {
        let mut x = [0u8; 32];
        x[31] = a;
        x
    }

    /// The money events of one life of the contract, with the amounts the
    /// contract computes: two registrations (one activated, one cancelled),
    /// a deal paid, extended and closed, a pass, a missed answer slashed and
    /// its reporter rewarded, every withdrawal and the sweep.
    fn life() -> (Vec<Log>, Books) {
        let p = params();
        let node = Hash32([0xaa; 32]);
        let bond0 = p.min_bond;
        let bond1 = bond_required(&p, 5_000);
        let (fee0, fee1) = (registration_fee(bond0), registration_fee(bond1));
        let s = slash(&p, bond0 - fee0);
        let r = miss_reward(s);
        let (dep, more, pay) = (48 * 10u128.pow(15), 24 * 10u128.pow(15), 2 * 10u128.pow(15));
        let refund = dep + more - pay;
        let sweep = fee0 + fee1 + s - r;
        let deal = |dep: u128| {
            vec![
                b32(&Hash32([1; 32])),
                b32(&Hash32([2; 32])),
                uint(2),
                uint(5 << 30),
                uint(3),
                uint(4),
                uint(8),
                b32(&Hash32([3; 32])),
                uint(dep),
                Tok::Bytes(b"synthetic.publisher".to_vec()),
                Tok::Bytes(b"synthetic.storage".to_vec()),
                Tok::Bytes(Vec::new()),
            ]
        };
        let url = || Tok::Bytes(b"http://127.0.0.1:1/".to_vec());
        let logs = vec![
            log(
                events::REGISTERED,
                &[w(0), wa(1)],
                &[uint(100), uint(bond0), url()],
            ),
            log(events::REGISTRATION_FEE, &[w(0), wa(1)], &[uint(fee0)]),
            log(events::ACTIVATED, &[node.0, w(0), wa(1)], &[]),
            log(
                events::REGISTERED,
                &[w(1), wa(2)],
                &[uint(5_000), uint(bond1), url()],
            ),
            log(events::REGISTRATION_FEE, &[w(1), wa(2)], &[uint(fee1)]),
            log(events::DEAL_CREATED, &[w(0), wa(3)], &deal(dep)),
            log(
                events::ANSWERED,
                &[w(0), w(4), node.0],
                &[uint(0), uint(1), uint(pay)],
            ),
            log(
                events::SLASHED,
                &[node.0, w(0)],
                &[uint(4), uint(1), uint(0), uint(s), uint(6), uint(3)],
            ),
            log(events::MISS_REWARD, &[node.0, wa(4)], &[uint(r)]),
            log(events::DEAL_EXTENDED, &[w(0)], &[uint(10), uint(more)]),
            log(events::REGISTRATION_CANCELLED, &[w(1)], &[]),
            log(events::DEAL_CLOSED, &[w(0)], &[uint(refund)]),
            log(events::EARNINGS_WITHDRAWN, &[wa(1)], &[uint(pay)]),
            log(events::EARNINGS_WITHDRAWN, &[wa(4)], &[uint(r)]),
            log(events::TREASURY_SWEPT, &[wa(5)], &[uint(sweep)]),
            log(
                events::BOND_WITHDRAWN,
                &[node.0, wa(1)],
                &[uint(bond0 - fee0 - s)],
            ),
            // Not a money event: skipped.
            log(events::SEED, &[w(4)], &[b32(&Hash32([9; 32]))]),
        ];
        let want = Books {
            deposits: dep + more,
            bonds_in: bond0 + bond1,
            paid: pay,
            slashed: fee0 + fee1 + s,
            refunded: refund,
            bonds_out: (bond1 - fee1) + (bond0 - fee0 - s),
            earnings_out: pay + r,
            swept: sweep,
            rewarded: r,
            balance: 0,
        };
        (logs, want)
    }

    /// Amendment 1 item 3 (THREATS STORAGE-S16-R3-5): the replay reads
    /// `RegistrationFee` and `MissReward`, so its totals are the contract's
    /// `books()` and they balance; without either event they do not.
    #[test]
    fn the_event_replay_matches_the_contracts_books() {
        let (logs, want) = life();
        let it = || logs.iter().map(|(t, d)| (t.as_slice(), d.as_slice()));
        let got = replay(it()).expect("replays");
        assert_eq!(got, want);
        assert!(got.conserved(), "{got:?}");
        assert_eq!(got.totals()[3], want.slashed);
        assert_eq!(got.totals()[8], want.rewarded);
        // A replay that skipped either new event would disagree with the contract.
        for skip in [events::REGISTRATION_FEE, events::MISS_REWARD] {
            let t = topic(skip);
            let partial = replay(it().filter(|(ts, _)| ts[0] != t));
            assert!(
                partial
                    .as_ref()
                    .map_or(true, |b| *b != want && !b.conserved()),
                "{skip}: {partial:?}"
            );
        }
        // A cancellation of a registration never seen is refused, not guessed.
        let (t, d) = log(events::REGISTRATION_CANCELLED, &[w(9)], &[]);
        assert_eq!(
            Replay::new().apply(&t, &d),
            Err(ReplayError::UnknownRegistration(9))
        );
    }
}
