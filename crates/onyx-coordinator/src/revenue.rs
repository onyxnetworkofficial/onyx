//! Usage revenue split (ADR-0025, docs/spec/revenue.md): how one paid call's
//! fee is shared between the miners, validators and label contributors of the
//! shard it bought, and the treasury. Integer arithmetic only; the contract
//! `OnyxRevenue` computes the same numbers (shared vectors in
//! `fixtures/revenue/vectors.json`).

/// Basis points in a whole (100%).
pub const BPS: u32 = 10_000;

/// Shares of each fee, in basis points; they sum to [`BPS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevenueSplit {
    /// Miners whose reveal equals the final root, equally.
    pub miners_bps: u16,
    /// Validators who agreed with the final root on a shard no case decided, equally.
    pub validators_bps: u16,
    /// The label set's payee, when the shard pins a label set that has one.
    pub labels_bps: u16,
    /// The treasury; it also receives every rounding remainder.
    pub treasury_bps: u16,
}

impl Default for RevenueSplit {
    /// 50% miners, 20% validators, 10% label contributors, 20% treasury (ADR-0025).
    fn default() -> Self {
        RevenueSplit {
            miners_bps: 5_000,
            validators_bps: 2_000,
            labels_bps: 1_000,
            treasury_bps: 2_000,
        }
    }
}

impl RevenueSplit {
    /// The shares sum to exactly [`BPS`].
    pub fn is_valid(&self) -> bool {
        u32::from(self.miners_bps)
            + u32::from(self.validators_bps)
            + u32::from(self.labels_bps)
            + u32::from(self.treasury_bps)
            == BPS
    }
}

/// Who a shard's revenue goes to, fixed when the shard is registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipients {
    /// Miners whose revealed root equals the final root.
    pub miners: u32,
    /// Validators paid on this shard (zero when a case decided it).
    pub validators: u32,
    /// Whether the shard pinned a label set: its label share is paid to the
    /// set's payee, or held for the set until one is named (REV-L1-F3).
    /// Without one the label share goes to the miners.
    pub label_set: bool,
}

/// One fee, split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shares {
    /// What each miner earns.
    pub per_miner: u128,
    /// What each validator earns.
    pub per_validator: u128,
    /// The label share: paid to the label set's payee, or held for the set.
    pub label: u128,
    /// What the treasury earns, rounding remainders included.
    pub treasury: u128,
}

/// Split `amount` (micro-USDC) by `s` among `r` (R-REV-2): each role's cut is
/// `amount * bps / BPS` rounded down; a role with nobody to pay passes its cut
/// on (validators and the label share to the miners, the miners' cut to the
/// treasury); each head gets the cut divided by the heads, rounded down; every
/// remainder goes to the treasury, so the parts always sum to `amount`.
/// Saturates instead of overflowing (amounts are micro-USDC, far below 2^100).
pub fn split(amount: u128, s: &RevenueSplit, r: &Recipients) -> Shares {
    let cut = |bps: u16| amount.saturating_mul(u128::from(bps)) / u128::from(BPS);
    let mut miners_cut = cut(s.miners_bps);
    let mut validators_cut = cut(s.validators_bps);
    let mut label = cut(s.labels_bps);
    if r.validators == 0 {
        miners_cut += validators_cut;
        validators_cut = 0;
    }
    if !r.label_set {
        miners_cut += label;
        label = 0;
    }
    let per_miner = miners_cut
        .checked_div(u128::from(r.miners))
        .unwrap_or_default();
    let per_validator = validators_cut
        .checked_div(u128::from(r.validators))
        .unwrap_or_default();
    let paid = per_miner * u128::from(r.miners) + per_validator * u128::from(r.validators) + label;
    Shares {
        per_miner,
        per_validator,
        label,
        treasury: amount - paid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(sh: &Shares, r: &Recipients) -> u128 {
        sh.per_miner * u128::from(r.miners)
            + sh.per_validator * u128::from(r.validators)
            + sh.label
            + sh.treasury
    }

    #[test]
    fn default_split_is_valid_and_exact() {
        let s = RevenueSplit::default();
        assert!(s.is_valid());
        let r = Recipients {
            miners: 2,
            validators: 4,
            label_set: true,
        };
        let sh = split(10_000, &s, &r);
        assert_eq!(sh.per_miner, 2_500);
        assert_eq!(sh.per_validator, 500);
        assert_eq!(sh.label, 1_000);
        assert_eq!(sh.treasury, 2_000);
        assert_eq!(total(&sh, &r), 10_000);
    }

    #[test]
    fn missing_roles_pass_their_cut_on_and_dust_goes_to_the_treasury() {
        let s = RevenueSplit::default();
        // No validators, no label set: miners take 80%.
        let r = Recipients {
            miners: 3,
            validators: 0,
            label_set: false,
        };
        let sh = split(1_000, &s, &r);
        assert_eq!(sh.per_miner, 266);
        assert_eq!(sh.per_validator, 0);
        assert_eq!(sh.label, 0);
        assert_eq!(sh.treasury, 1_000 - 798);
        // No miner matched the final root: their cut is the treasury's.
        let none = Recipients {
            miners: 0,
            validators: 0,
            label_set: false,
        };
        assert_eq!(split(1_000, &s, &none).treasury, 1_000);
    }

    #[test]
    fn parts_always_sum_to_the_amount() {
        let s = RevenueSplit {
            miners_bps: 3_333,
            validators_bps: 3_333,
            labels_bps: 3_333,
            treasury_bps: 1,
        };
        assert!(s.is_valid());
        for amount in [0u128, 1, 7, 999, 1_000_003, u128::from(u64::MAX)] {
            for miners in [0u32, 1, 3, 32] {
                for validators in [0u32, 1, 7, 128] {
                    for label_set in [false, true] {
                        let r = Recipients {
                            miners,
                            validators,
                            label_set,
                        };
                        assert_eq!(total(&split(amount, &s, &r), &r), amount);
                    }
                }
            }
        }
        assert!(!RevenueSplit {
            treasury_bps: 2,
            ..s
        }
        .is_valid());
    }
}

#[cfg(test)]
mod vectors {
    use super::*;

    /// Shared Rust/Solidity vectors for [`split`] (`contracts/test/OnyxRevenue.t.sol`
    /// reads them). Regenerate with `ONYX_BLESS=1 cargo test -p onyx-coordinator`
    /// only with an ADR-0025 change.
    #[test]
    fn revenue_vectors_match() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/revenue/vectors.json"
        );
        let splits = [
            RevenueSplit::default(),
            RevenueSplit {
                miners_bps: 3_333,
                validators_bps: 3_333,
                labels_bps: 3_333,
                treasury_bps: 1,
            },
            RevenueSplit {
                miners_bps: 10_000,
                validators_bps: 0,
                labels_bps: 0,
                treasury_bps: 0,
            },
        ];
        let amounts: [u128; 6] = [0, 1, 2_500, 20_000, 1_000_003, 123_456_789_012];
        let heads = [
            (1u32, 4u32, true),
            (2, 0, true),
            (3, 7, false),
            (0, 5, true),
            (32, 128, false),
        ];
        let mut cases = Vec::new();
        for s in &splits {
            for amount in amounts {
                for (miners, validators, label_set) in heads {
                    let r = Recipients {
                        miners,
                        validators,
                        label_set,
                    };
                    let sh = split(amount, s, &r);
                    cases.push(serde_json::json!({
                        "amount": amount.to_string(),
                        "minersBps": s.miners_bps,
                        "validatorsBps": s.validators_bps,
                        "labelsBps": s.labels_bps,
                        "treasuryBps": s.treasury_bps,
                        "miners": miners,
                        "validators": validators,
                        "labelSet": label_set,
                        "perMiner": sh.per_miner.to_string(),
                        "perValidator": sh.per_validator.to_string(),
                        "label": sh.label.to_string(),
                        "treasury": sh.treasury.to_string(),
                    }));
                }
            }
        }
        let doc = serde_json::json!({ "count": cases.len(), "cases": cases });
        let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(path).parent().expect("parent"))
                .expect("mkdir");
            std::fs::write(path, &text).expect("write vectors");
        }
        let on_disk = std::fs::read_to_string(path)
            .expect("fixtures/revenue/vectors.json missing; run with ONYX_BLESS=1");
        assert_eq!(on_disk, text, "revenue vectors drifted from ADR-0025");
    }
}
