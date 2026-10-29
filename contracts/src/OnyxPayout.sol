// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title OnyxPayout
/// @notice The integer payout math shared by `OnyxStakes` (S3) and `OnyxFutures` (S6), mirrored
/// by the website's `markets.ts` (no Rust client yet, ADR-0042); shared vectors in
/// fixtures/markets/vectors.json (ADR-0042). Losers pay a fee in parts per million to the
/// treasury; the rest goes to the winners pro rata to their stake, rounded down; what rounding
/// leaves (dust) goes to the treasury after the last winner is paid. No floats (ADR-0009).
library OnyxPayout {
    uint256 internal constant PPM = 1_000_000;

    /// The treasury fee on the losing side's money: `floor(losers * feePpm / 1e6)`.
    function fee(uint256 losers, uint256 feePpm) internal pure returns (uint256) {
        return losers * feePpm / PPM;
    }

    /// A winner's payout: their stake back plus `floor(stake * distributable / winningPool)`.
    /// Inputs are at most 2^128 each, so the product fits in 256 bits.
    function share(uint256 stake, uint256 distributable, uint256 winningPool)
        internal
        pure
        returns (uint256)
    {
        if (winningPool == 0) return 0;
        return stake + stake * distributable / winningPool;
    }

    /// The smallest stake an attacker must reveal on the false side of a vote-decided claim to
    /// win it outright against `honest` revealed on the true side (ADR-0042 item 9): at least
    /// `decisivePpm` of everything revealed, at least `quorum` revealed in all, at least
    /// `minStake`. Returns 0 when no stake can win within `maxPerClaim` (the attack is
    /// impossible; the claim goes to the fallback oracle instead).
    function minAttackStake(
        uint256 honest,
        uint256 quorum,
        uint256 decisivePpm,
        uint256 minStake,
        uint256 maxPerClaim
    ) internal pure returns (uint256 a) {
        // A * 1e6 >= d * (A + H)  <=>  A >= d * H / (1e6 - d), rounded up.
        uint256 num = decisivePpm * honest;
        uint256 den = PPM - decisivePpm;
        a = (num + den - 1) / den;
        if (honest < quorum && quorum - honest > a) a = quorum - honest;
        if (a < minStake) a = minStake;
        if (a + honest > maxPerClaim) return 0;
    }
}
