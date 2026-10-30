//! Prepaid buyer balances (partner tracker T7, pre-public loop PP5, docs/spec/prepaid.md).
//!
//! A buyer deposits into `OnyxPrepaid` once. Each paid answer is then paid with a [`Voucher`]: the
//! buyer's EIP-712 signature of its running total ("I have spent up to `cumulative`"), the
//! previous total plus this answer's price. The server keeps a [`PrepaidBook`] per buyer from the
//! chain's account state and the latest voucher it took, and refuses a voucher that does not add
//! exactly the price, exceeds the deposit, comes while an exit is pending, or is not the buyer's.
//! The reporter later redeems each buyer's latest voucher in one batch (`OnyxPrepaid.redeem`)
//! and allocates the pooled money to the books that were bought (`OnyxPrepaid.allocate`), so the
//! chain never links a buyer to a row.
//!
//! The digest and signature format are shared with the contract (fixtures/prepaid/vectors.json,
//! `contracts/test/OnyxPrepaid.t.sol::test_SharedVoucherVectors`).

use crate::x402::exact_evm;
use onyx_core::keccak256;

/// `keccak256("Voucher(address buyer,uint128 cumulative)")`, as the contract's `VOUCHER_TYPEHASH`.
pub fn voucher_typehash() -> [u8; 32] {
    keccak256(b"Voucher(address buyer,uint128 cumulative)").0
}

/// The EIP-712 domain separator of the `OnyxPrepaid` at `contract` on `chain_id`.
pub fn domain_separator(chain_id: u64, contract: &[u8; 20]) -> [u8; 32] {
    let mut m = Vec::with_capacity(32 * 5);
    m.extend_from_slice(
        &keccak256(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        )
        .0,
    );
    m.extend_from_slice(&keccak256(b"OnyxPrepaid").0);
    m.extend_from_slice(&keccak256(b"1").0);
    m.extend_from_slice(&word(u128::from(chain_id)));
    m.extend_from_slice(&addr_word(contract));
    keccak256(&m).0
}

fn word(n: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&n.to_be_bytes());
    w
}

fn addr_word(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// The digest a buyer signs for `cumulative` (the contract's `voucherDigest`).
pub fn voucher_digest(domain: &[u8; 32], buyer: &[u8; 20], cumulative: u128) -> [u8; 32] {
    let mut s = Vec::with_capacity(96);
    s.extend_from_slice(&voucher_typehash());
    s.extend_from_slice(&addr_word(buyer));
    s.extend_from_slice(&word(cumulative));
    let mut m = Vec::with_capacity(66);
    m.extend_from_slice(b"\x19\x01");
    m.extend_from_slice(domain);
    m.extend_from_slice(&keccak256(&s).0);
    keccak256(&m).0
}

/// One signed running total.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Voucher {
    /// The buyer's address.
    pub buyer: [u8; 20],
    /// The buyer's total spent, in the token's smallest unit.
    pub cumulative: u128,
    /// 65 bytes `r || s || v`, low s.
    pub signature: Vec<u8>,
}

impl Voucher {
    /// Whether the signature is the buyer's over this total in `domain`.
    pub fn signed(&self, domain: &[u8; 32]) -> bool {
        exact_evm::recover(
            &voucher_digest(domain, &self.buyer, self.cumulative),
            &self.signature,
        ) == Some(self.buyer)
    }

    /// Sign `cumulative` for the key `secret` (tests and the demo; test keys hold nothing).
    pub fn sign(secret: &[u8; 32], domain: &[u8; 32], cumulative: u128) -> Option<Voucher> {
        let buyer = exact_evm::address(secret)?;
        let key = secp256k1::SecretKey::from_slice(secret).ok()?;
        let digest = voucher_digest(domain, &buyer, cumulative);
        let (id, rs) = secp256k1::Secp256k1::signing_only()
            .sign_ecdsa_recoverable(&secp256k1::Message::from_digest(digest), &key)
            .serialize_compact();
        let mut signature = rs.to_vec();
        signature.push(27 + u8::try_from(id.to_i32()).ok()?);
        Some(Voucher {
            buyer,
            cumulative,
            signature,
        })
    }
}

/// A buyer's account as the contract holds it (`OnyxPrepaid.accounts`), read at a finalized block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// Deposited, less withdrawn.
    pub deposited: u128,
    /// The highest total redeemed on chain.
    pub redeemed: u128,
    /// Whether an exit is pending.
    pub exiting: bool,
}

/// Why a voucher is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrepaidError {
    /// Not the buyer's signature over this total.
    #[error("the voucher is not signed by its buyer")]
    NotSigned,
    /// The total does not add exactly the price to the last one taken.
    #[error("the voucher must add exactly the price ({price}) to the last total ({last})")]
    WrongTotal {
        /// The last total taken.
        last: u128,
        /// The price asked.
        price: u128,
    },
    /// The total exceeds what the buyer deposited.
    #[error("the voucher exceeds the deposit")]
    OverDeposit,
    /// The buyer asked to leave: no voucher is taken until the exit is done or a new deposit
    /// cancels it.
    #[error("an exit is pending")]
    Exiting,
    /// A hold for a paid observation round is pending (ADR-0049, R-OBS-7): no other voucher is
    /// taken from this buyer until it is charged or released.
    #[error("a hold for an observation round is pending; retry after it is charged or released")]
    HoldPending,
    /// A hold would outlast half the contract's `exitDelay` (R-OBS-8), or holds are off.
    #[error("a hold may last at most {limit} s")]
    HoldTooLong {
        /// The longest hold this server takes.
        limit: u64,
    },
}

/// The server's view of one buyer: the chain's account and the latest voucher it took.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrepaidBook {
    /// The account as last read from the chain.
    pub account: Account,
    /// The latest voucher taken and not yet redeemed on chain.
    pub latest: Option<Voucher>,
    /// A voucher held for a paid observation round (ADR-0049): checked like a charge, not
    /// counted, never redeemed; [`Self::commit_hold`] makes it the latest, [`Self::release_hold`]
    /// drops it.
    pub held: Option<Voucher>,
}

impl PrepaidBook {
    /// The buyer's total so far: the latest voucher taken, else what the chain redeemed. A held
    /// voucher is not in it.
    pub fn total(&self) -> u128 {
        self.latest.as_ref().map_or(self.account.redeemed, |v| {
            v.cumulative.max(self.account.redeemed)
        })
    }

    /// What the buyer can still spend: a held amount is not spendable.
    pub fn spendable(&self) -> u128 {
        if self.account.exiting {
            0
        } else {
            let used = self
                .held
                .as_ref()
                .map_or(self.total(), |h| h.cumulative.max(self.total()));
            self.account.deposited.saturating_sub(used)
        }
    }

    fn check(
        &self,
        v: &Voucher,
        price: u128,
        domain: &[u8; 32],
        incoming: u128,
    ) -> Result<(), PrepaidError> {
        // Review CL6-R1-5: the signature first, so an unsigned voucher learns nothing about the
        // buyer's total, hold or exit.
        if !v.signed(domain) {
            return Err(PrepaidError::NotSigned);
        }
        if self.account.exiting {
            return Err(PrepaidError::Exiting);
        }
        if self.held.is_some() {
            return Err(PrepaidError::HoldPending);
        }
        let last = self.total();
        if last.checked_add(price) != Some(v.cumulative) {
            return Err(PrepaidError::WrongTotal { last, price });
        }
        if v.cumulative > self.account.deposited.saturating_add(incoming) {
            return Err(PrepaidError::OverDeposit);
        }
        Ok(())
    }

    /// Take `v` for an answer priced `price`: signed by this buyer, exactly the last total plus
    /// the price, within the deposit, no exit and no hold pending. On success it becomes the
    /// latest voucher.
    pub fn charge(
        &mut self,
        v: Voucher,
        price: u128,
        domain: &[u8; 32],
    ) -> Result<(), PrepaidError> {
        self.check(&v, price, domain, 0)?;
        self.latest = Some(v);
        Ok(())
    }

    /// Hold `v` for a round priced `price` (ADR-0049): the same checks as [`Self::charge`], and
    /// at most one hold per buyer, so one deposit never backs two holds (R-OBS-7).
    pub fn hold(&mut self, v: Voucher, price: u128, domain: &[u8; 32]) -> Result<(), PrepaidError> {
        self.hold_incoming(v, price, domain, 0)
    }

    /// [`Self::hold`] for a relayed deposit (ADR-0056 item 4): the voucher may reach the deposit
    /// plus `incoming`, the amount the relayed transaction pays in. The hold is charged only
    /// once that transaction is mined, so the total never exceeds what then sits on chain.
    pub fn hold_incoming(
        &mut self,
        v: Voucher,
        price: u128,
        domain: &[u8; 32],
        incoming: u128,
    ) -> Result<(), PrepaidError> {
        self.check(&v, price, domain, incoming)?;
        self.held = Some(v);
        Ok(())
    }

    /// The held voucher's round is final and served: it becomes the latest voucher. `false`
    /// when nothing is held.
    pub fn commit_hold(&mut self) -> bool {
        match self.held.take() {
            Some(v) => {
                self.latest = Some(v);
                true
            }
            None => false,
        }
    }

    /// The held voucher's round failed: it is dropped, and the next voucher starts from the
    /// total before it. `false` when nothing is held.
    pub fn release_hold(&mut self) -> bool {
        self.held.take().is_some()
    }

    /// The voucher to redeem now, if the latest is above what the chain redeemed.
    pub fn to_redeem(&self) -> Option<&Voucher> {
        self.latest
            .as_ref()
            .filter(|v| v.cumulative > self.account.redeemed)
    }

    /// After a chain read: the new account; a voucher at or below the redeemed total is spent.
    pub fn refresh(&mut self, account: Account) {
        self.account = account;
        if self
            .latest
            .as_ref()
            .is_some_and(|v| v.cumulative <= account.redeemed)
        {
            self.latest = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAIN: u64 = 84_532;
    const CONTRACT: [u8; 20] = [0x5a; 20];

    fn key(label: &str) -> [u8; 32] {
        keccak256(label.as_bytes()).0
    }

    fn book(deposited: u128) -> PrepaidBook {
        PrepaidBook {
            account: Account {
                deposited,
                redeemed: 0,
                exiting: false,
            },
            latest: None,
            held: None,
        }
    }

    #[test]
    fn queries_draw_down_the_balance_one_exact_price_at_a_time() {
        let d = domain_separator(CHAIN, &CONTRACT);
        let k = key("prepaid buyer one");
        let mut b = book(1_000_000);
        for (i, price) in [250_000u128, 250_000, 100_000].into_iter().enumerate() {
            let total = b.total() + price;
            b.charge(Voucher::sign(&k, &d, total).expect("sign"), price, &d)
                .expect("charged");
            assert_eq!(b.total(), total, "query {i}");
        }
        assert_eq!(b.spendable(), 400_000);
        // A voucher for less, more, the same total again, or another key, is refused.
        let last = b.total();
        for bad in [last, last + 1, last + 500_001] {
            assert!(
                b.charge(Voucher::sign(&k, &d, bad).expect("sign"), 100_000, &d)
                    .is_err(),
                "{bad}"
            );
        }
        let other = Voucher::sign(&key("someone else"), &d, last + 100_000).expect("sign");
        let forged = Voucher {
            buyer: b.latest.as_ref().expect("v").buyer,
            ..other
        };
        assert_eq!(b.charge(forged, 100_000, &d), Err(PrepaidError::NotSigned));
        // A voucher for another contract or chain is not this buyer's signature here.
        let elsewhere = domain_separator(CHAIN, &[0x5b; 20]);
        assert_eq!(
            b.charge(
                Voucher::sign(&k, &elsewhere, last + 100_000).expect("sign"),
                100_000,
                &d
            ),
            Err(PrepaidError::NotSigned)
        );
        // Over the deposit.
        assert_eq!(
            b.charge(
                Voucher::sign(&k, &d, last + 400_001).expect("sign"),
                400_001,
                &d
            ),
            Err(PrepaidError::OverDeposit)
        );
    }

    #[test]
    fn redemption_and_exits_follow_the_chain() {
        let d = domain_separator(CHAIN, &CONTRACT);
        let k = key("prepaid buyer two");
        let mut b = book(500);
        b.charge(Voucher::sign(&k, &d, 200).expect("sign"), 200, &d)
            .expect("charged");
        assert_eq!(b.to_redeem().map(|v| v.cumulative), Some(200));
        // The chain redeemed it: nothing more to redeem; the total continues from the chain's.
        b.refresh(Account {
            deposited: 500,
            redeemed: 200,
            exiting: false,
        });
        assert!(b.to_redeem().is_none());
        assert_eq!(b.total(), 200);
        // An exit pending: no voucher is taken.
        b.refresh(Account {
            deposited: 500,
            redeemed: 200,
            exiting: true,
        });
        assert_eq!(b.spendable(), 0);
        assert_eq!(
            b.charge(Voucher::sign(&k, &d, 300).expect("sign"), 100, &d),
            Err(PrepaidError::Exiting)
        );
    }

    /// ADR-0049 (R-OBS-7): a held voucher is neither counted nor redeemed until charged; one
    /// hold at a time and no charge beside it; a release puts the total back.
    #[test]
    fn a_hold_is_never_counted_or_redeemed_until_charged() {
        let d = domain_separator(CHAIN, &CONTRACT);
        let k = key("prepaid holder");
        let sign = |total| Voucher::sign(&k, &d, total).expect("sign");
        let mut b = book(1000);
        b.hold(sign(400), 400, &d).expect("held");
        assert_eq!((b.total(), b.spendable()), (0, 600));
        assert!(b.to_redeem().is_none());
        assert_eq!(b.hold(sign(800), 400, &d), Err(PrepaidError::HoldPending));
        assert_eq!(b.charge(sign(400), 400, &d), Err(PrepaidError::HoldPending));
        assert!(b.release_hold());
        assert_eq!((b.total(), b.spendable()), (0, 1000));
        assert!(!b.release_hold() && !b.commit_hold(), "nothing held");
        b.hold(sign(300), 300, &d).expect("held");
        assert!(b.commit_hold());
        assert_eq!(b.total(), 300);
        assert_eq!(b.to_redeem().map(|v| v.cumulative), Some(300));
        assert_eq!(b.hold(sign(1300), 1000, &d), Err(PrepaidError::OverDeposit));
        b.refresh(Account {
            deposited: 1000,
            redeemed: 0,
            exiting: true,
        });
        assert_eq!(b.hold(sign(400), 100, &d), Err(PrepaidError::Exiting));
    }

    #[test]
    fn a_high_s_twin_is_refused() {
        let d = domain_separator(CHAIN, &CONTRACT);
        let v = Voucher::sign(&key("prepaid buyer three"), &d, 7).expect("sign");
        assert!(v.signed(&d));
        // n - s with the flipped recovery id recovers the same key; it must not pass.
        const N: [u8; 32] = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c,
            0xd0, 0x36, 0x41, 0x41,
        ];
        let mut s = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let d = i16::from(N[i]) - i16::from(v.signature[32 + i]) - borrow;
            borrow = i16::from(d < 0);
            s[i] = (d + 256 * borrow) as u8;
        }
        let mut twin = v.signature.clone();
        twin[32..64].copy_from_slice(&s);
        twin[64] = if v.signature[64] == 27 { 28 } else { 27 };
        assert!(!Voucher {
            signature: twin,
            ..v
        }
        .signed(&d));
    }

    /// Shared vectors with `OnyxPrepaid` (contracts/test/OnyxPrepaid.t.sol). Regenerate only
    /// with a format change: `ONYX_BLESS=1 cargo test -p onyx-api prepaid`.
    #[test]
    fn shared_voucher_vectors() {
        use serde_json::json;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/prepaid/vectors.json"
        );
        let d = domain_separator(CHAIN, &CONTRACT);
        let mut vs = Vec::new();
        for (label, total) in [
            ("prepaid vector buyer a", 250_000u128),
            ("prepaid vector buyer a", 500_000),
            ("prepaid vector buyer b", 1),
        ] {
            let v = Voucher::sign(&key(label), &d, total).expect("sign");
            vs.push(json!({
                "buyer": format!("0x{}", hex::encode(v.buyer)),
                "cumulative": total,
                "digest": format!("0x{}", hex::encode(voucher_digest(&d, &v.buyer, total))),
                "signature": format!("0x{}", hex::encode(&v.signature)),
            }));
        }
        let got = json!({
            "synthetic": "test keys derived from public labels; they hold nothing",
            "chainId": CHAIN,
            "verifyingContract": format!("0x{}", hex::encode(CONTRACT)),
            "domainSeparator": format!("0x{}", hex::encode(d)),
            "voucherTypehash": format!("0x{}", hex::encode(voucher_typehash())),
            "count": vs.len(),
            "vouchers": vs,
        });
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::create_dir_all(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/prepaid"
            ))
            .expect("dir");
            std::fs::write(
                path,
                serde_json::to_string_pretty(&got).expect("json") + "\n",
            )
            .expect("write");
        }
        let want: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(path)
                .expect("fixtures/prepaid/vectors.json missing; run with ONYX_BLESS=1"),
        )
        .expect("json");
        assert_eq!(got, want);
    }
}
