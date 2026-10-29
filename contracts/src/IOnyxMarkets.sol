// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @notice The ERC-20 subset the stake and market contracts use (ADR-0042).
interface IStakeToken {
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function balanceOf(address who) external view returns (uint256);
}

/// @notice Finalized values of facts, as `OnyxFutures` reads them (ADR-0042 item 12).
interface IOnyxFacts {
    /// `final_` true when the fact has an accepted value; `level` is the proof level that
    /// decided it (2: re-observed by observers; 4: decided by a stake vote or the fallback
    /// oracle); `acceptedAt` the chain time it was accepted.
    function factValue(bytes32 fact)
        external
        view
        returns (bool final_, int256 value, uint8 level, uint64 acceptedAt);
}

/// @notice What a facts source states about a listed fact and its insiders, so the contracts that
/// settle from it can refuse a late close and an insider's bet (THREATS NL6-10, NL6-12).
interface IOnyxFactGuard {
    /// The latest time trading on `fact` may close: the source's earliest publication for it,
    /// less a margin; 0 for a fact never listed.
    function closeBy(bytes32 fact) external view returns (uint64);
    /// Whether `who` can decide or delay the fact's value (the registry's coordinator, the
    /// listing owner): such an address may not bet on or insure it.
    function isInsider(address who) external view returns (bool);
}

/// @notice The staked validator set a panel seat must belong to (`OnyxSettlement.validators`,
/// THREATS LC16-2): a seat costs a validator bond in the settlement's token, which no single key
/// mints, and the set's size cap makes filling the panel cost more than outbidding its members.
interface IOnyxValidatorSet {
    function validators(address who)
        external
        view
        returns (uint128 stake, uint64 lockedUntil, uint64 unbondAt, bool active);
}

/// @notice The oracle a contested or low-quorum claim falls back to (ADR-0042 item 7).
interface IOnyxFallbackOracle {
    /// Called by the stake contract when a claim falls back.
    function request(uint256 claimId) external;
    /// 0: no answer yet; 1: the claimed value is correct; 2: wrong; 3: cannot be decided (void).
    function answerOf(uint256 claimId) external view returns (uint8);
    /// The address that answers; it may never stake or bet.
    function arbiter() external view returns (address);
}

/// @notice The re-observation of an auto-pull claim by observers (ROADMAP S4, ADR-0042 item 5).
interface IOnyxReobserve {
    function open(uint256 taskId, bytes32 fact, uint64 commitClose, uint64 revealClose) external;
    /// After the reveal window: 1 settled (with `value`), 2 source unavailable, 3 no result;
    /// 0 before the window ends or for an unknown task.
    function outcome(uint256 taskId) external view returns (uint8 status, int256 value);
    function isObserver(address who) external view returns (bool);
}
