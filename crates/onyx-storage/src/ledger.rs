//! The money rules of `OnyxStorage` (R-STOR-8..11) and a model of its books.
//!
//! Every amount is in tONYX base units (18 decimals) as an integer; rates
//! are per GiB per epoch; probabilities are ppm (ADR-0009). Every division
//! rounds down, and what rounding leaves stays in the deal and goes back to
//! the payer when the deal closes, so nothing is created or lost
//! (THREATS STORAGE-10). The prices are the deployer's placeholders
//! (docs/STORAGE-NETWORK.md "Cost model"), never market prices.

use crate::{GIB, PPM};

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

/// A model of the contract's books: every flow in and out, for the
/// conservation property (R-STOR-11), checked here and by the contract's
/// invariant suite.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Books {
    /// Deposits paid in by publishers.
    pub deposits: u128,
    /// Bonds paid in by nodes.
    pub bonds_in: u128,
    /// Credited to nodes for passed answers.
    pub paid: u128,
    /// Taken from bonds for the treasury.
    pub slashed: u128,
    /// Returned to payers when deals closed.
    pub refunded: u128,
    /// Bonds returned to operators.
    pub bonds_out: u128,
    /// Earnings withdrawn by operators.
    pub earnings_out: u128,
    /// Slashes sent to the treasury.
    pub swept: u128,
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

    /// Earned and not withdrawn.
    pub fn earnings_held(&self) -> u128 {
        self.paid - self.earnings_out
    }

    /// Slashed and not yet sent to the treasury.
    pub fn treasury_held(&self) -> u128 {
        self.slashed - self.swept
    }

    /// R-STOR-11: deposits = paid + refunded + deal held; bonds in = slashed
    /// + bonds out + bonds held; and the balance is exactly what is held.
    pub fn conserved(&self) -> bool {
        self.paid
            .checked_add(self.refunded)
            .is_some_and(|x| x <= self.deposits)
            && self
                .slashed
                .checked_add(self.bonds_out)
                .is_some_and(|x| x <= self.bonds_in)
            && self.earnings_out <= self.paid
            && self.swept <= self.slashed
            && self.balance
                == self.deal_held()
                    + self.bonds_held()
                    + self.earnings_held()
                    + self.treasury_held()
    }
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
                        let bond = bond_required(&p, rng.next() % 5_000);
                        b.bonds_in += bond;
                        b.balance += bond;
                        bonds.push(bond);
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
                        let i = (rng.next() as usize) % bonds.len();
                        let s = slash(&p, bonds[i]);
                        bonds[i] -= s;
                        b.slashed += s;
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
}
