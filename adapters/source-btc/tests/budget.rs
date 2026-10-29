//! The Base Sepolia funding plan covers what the fork rehearsal measured
//! (`fixtures/ops/rehearsal-gas.json`): every per-role constant is at least
//! the measured use, and the owner and deploy transaction counts match it.

use onyx_source_btc::budget::{
    BUDGET_MARGIN, GAS_COORDINATOR, GAS_DEPLOY, GAS_LEVEL2_COORDINATOR, GAS_MINER, GAS_VALIDATOR,
    TXS_COORDINATOR, TXS_DEPLOY, TXS_LEVEL2_COORDINATOR, TXS_LEVEL2_OWNER, TXS_MINER, TXS_OWNER,
    TXS_VALIDATOR,
};
use onyx_source_btc::METRIC_MIN_LEVELS;
use serde_json::Value;

fn measured() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/ops/rehearsal-gas.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

fn n(v: &Value, k: &str) -> u128 {
    u128::from(v[k].as_u64().unwrap_or_else(|| panic!("{k} missing")))
}

#[test]
fn the_plan_covers_the_measured_fork_rehearsal() {
    let m = measured();
    assert!(GAS_DEPLOY >= n(&m, "deploy_gas"), "deploy gas");
    assert!(
        GAS_COORDINATOR >= n(&m, "coordinator_gas_after_deploy"),
        "coordinator gas"
    );
    assert!(GAS_MINER >= n(&m, "max_miner_gas"), "miner gas");
    assert!(GAS_VALIDATOR >= n(&m, "max_validator_gas"), "validator gas");
    assert!(
        TXS_COORDINATOR >= n(&m, "coordinator_txs"),
        "coordinator txs"
    );
    assert!(TXS_VALIDATOR >= n(&m, "max_validator_txs"), "validator txs");
    assert!(TXS_MINER >= n(&m, "max_miner_txs"), "miner txs");
    assert_eq!(TXS_DEPLOY, n(&m, "deploy_txs"), "deploy txs");
    // Each floored metric is one more owner `registerMetric` (REG-F-F3); the
    // rehearsal ran with `floored_metrics` of them (METRIC_MIN_LEVELS at its
    // commit), ADR-0034 Amendment 1 added the reserve recipe's floor. The
    // reserve round (ROADMAP A2) adds `btc.entity-reserve` and label set v2.
    assert_eq!(
        TXS_OWNER,
        n(&m, "owner_txs_fork") - n(&m, "floored_metrics") + METRIC_MIN_LEVELS.len() as u128,
        "owner txs on a fork or testnet run"
    );
    const { assert!(BUDGET_MARGIN >= 2) };
}

/// ROADMAP B1: the fork's `--level2` run (the level-2 Treasury round, the
/// fact's book opened by the reporter, a self-settled level-2 sale) is
/// covered too: the coordinator's and the actors' measured use with level 2
/// is within the plan with the level-2 additions, and the owner registers
/// one more template.
#[test]
fn the_plan_covers_the_measured_level2_fork_rehearsal() {
    let m = &measured()["level2"];
    assert!(
        GAS_COORDINATOR + GAS_LEVEL2_COORDINATOR >= n(m, "coordinator_gas_after_deploy"),
        "coordinator gas with level 2"
    );
    assert!(
        GAS_COORDINATOR >= n(m, "base_coordinator_gas_after_deploy"),
        "coordinator gas without level 2 (same day)"
    );
    assert!(
        TXS_COORDINATOR + TXS_LEVEL2_COORDINATOR >= n(m, "coordinator_txs"),
        "coordinator txs with level 2"
    );
    assert_eq!(
        TXS_OWNER + TXS_LEVEL2_OWNER,
        n(m, "owner_txs_fork"),
        "owner txs with level 2"
    );
    assert!(GAS_VALIDATOR >= n(m, "max_validator_gas"), "validator gas");
    assert!(GAS_MINER >= n(m, "max_miner_gas"), "miner gas");
}
