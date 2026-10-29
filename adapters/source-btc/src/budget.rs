//! Gas and transaction budget for a Base Sepolia (fork or testnet) run of the
//! demo driver (`examples/demo-local.rs --mode fork|testnet`). Each constant is
//! at least the use measured in the fork rehearsal (`fixtures/ops/rehearsal-gas.json`,
//! checked by `tests/budget.rs`), and the funding plan multiplies them by the
//! node's fee cap and [`BUDGET_MARGIN`].

use crate::METRIC_MIN_LEVELS;

/// Most L2 gas per miner, with the claim and withdraw of its revenue share
/// and the reserve round (ROADMAP A2): measured at most 1,301,541 in three
/// fork runs on branch reserve-live (2026-10-13; 957,661 at reserve-api 7eec445 before the
/// reserve round was in fork and testnet runs); about 15 % over it, as after
/// review RESAPI-R1 F5.
pub const GAS_MINER: u128 = 1_500_000;
/// Most L2 gas per validator (measured 945,534 at 62b8086, a panel member;
/// 795,493 at reserve-api 7eec445; at most 922,744 on reserve-live with
/// the reserve round: kept at the highest one).
pub const GAS_VALIDATOR: u128 = 1_050_000;
/// Coordinator work after the deploy: funding, bonds, three rounds (round 1,
/// the reserve round, the dispute round), two OnyxRevenue registers.
/// Measured 11,410,628, 11,560,858 and 11,641,518 in three fork runs on
/// reserve-live (2026-10-13; 8,794,715 at reserve-api 7eec445 with two
/// rounds); about 12.5 % over the highest.
pub const GAS_COORDINATOR: u128 = 13_100_000;
/// The three deploy scripts (Deploy.s.sol, DeployRevenue.s.sol,
/// DeployRegistry.s.sol): measured 18,364,270 gas in 6 transactions at
/// reserve-api 7eec445 (18,767,028 in 7 at 62b8086).
pub const GAS_DEPLOY: u128 = 19_500_000;
/// Deploy transactions, for the L1 data fee (measured 6, twice, at
/// reserve-api 7eec445; `tests/budget.rs` requires the measured count).
pub const TXS_DEPLOY: u128 = 6;
/// Most transactions per miner, for the L1 data fee (OP Stack): measured 10
/// on reserve-live (8 before the reserve round's commit and reveal).
pub const TXS_MINER: u128 = 12;
/// Most transactions per validator (measured 9, and 11 on reserve-live).
pub const TXS_VALIDATOR: u128 = 12;
/// Coordinator transactions after the deploy (measured 68 in all three runs
/// on reserve-live with the reserve round, 60 before).
pub const TXS_COORDINATOR: u128 = 84;
/// The API operator's settle transactions (one per paid request; the customer
/// holds enough for 4; a run makes 2: round 1's row and the reserve row), plus the standard-facilitator transfer and the
/// reporter's `credit` (REV-11).
pub const GAS_OPERATOR: u128 = 4 * 250_000 + 400_000;
/// The API operator's transactions (see [`GAS_OPERATOR`]).
pub const TXS_OPERATOR: u128 = 6;
/// The registry owner's transactions on a fork or testnet run (REG-R1-F3):
/// the entity-flow metric and its label set (v1), each floored metric with
/// its floor (REG-F-F3), and for the reserve round `btc.entity-reserve` (the
/// served metric name, floor 4) and its label set (v2). Measured 6 on
/// reserve-live. Local mode also registers the Treasury feed, which a fork or
/// testnet plan does not fund.
pub const TXS_OWNER: u128 = 4 + METRIC_MIN_LEVELS.len() as u128;
/// Gas per owner registration (measured about 100,000).
pub const GAS_OWNER: u128 = TXS_OWNER * 120_000;
/// `--level2` on a fork (ROADMAP B1, ADR-0027 amendment 8 item 8): the
/// coordinator's level-2 round on the recorded Treasury feed (announce the
/// draw, fix its beacon, post each observer's commit and receipt, record the
/// fact). Testnet runs do not run it yet (ROADMAP B2-B6). Measured on
/// branch revenue-books (2026-10-16, fork, `--dispute`): the coordinator
/// spent 13,342,878 and 13,595,005 gas after the deploy in two runs with
/// `--level2` and 11,881,398 without, so about 1.5 to 1.7 million; with
/// [`GAS_COORDINATOR`] the plan covers the level-2 runs' use with about 11 %
/// to spare
/// (`fixtures/ops/rehearsal-gas.json`, `level2`).
pub const GAS_LEVEL2_COORDINATOR: u128 = 2_000_000;
/// Coordinator transactions of the level-2 round (see
/// [`GAS_LEVEL2_COORDINATOR`]): measured 14 (82 after the deploy with
/// `--level2`, 68 without).
pub const TXS_LEVEL2_COORDINATOR: u128 = 16;
/// The API operator's level-2 transactions: `registerObservation` (the
/// fact's revenue book, THREATS L2API-4) and the sale's self-settle.
pub const GAS_LEVEL2_OPERATOR: u128 = 400_000;
/// The API operator's level-2 transactions (see [`GAS_LEVEL2_OPERATOR`]).
pub const TXS_LEVEL2_OPERATOR: u128 = 2;
/// The registry owner's level-2 registration (the Treasury feed template).
pub const TXS_LEVEL2_OWNER: u128 = 1;
/// Headroom over the measured use: fees move, and a dispute adds panels.
pub const BUDGET_MARGIN: u128 = 3;
