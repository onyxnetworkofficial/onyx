// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxRevenue, IRevenueToken, IOnyxLedgerView} from "./OnyxRevenue.sol";

/// @notice The credits ledger's entry for an earned share (`OnyxPrepaidV2.depositFor`).
interface IOnyxCredits {
    function depositFor(address account, uint128 amount, uint8 origin) external;
}

/// @notice `approve` on the revenue token (USDC has it; `IRevenueToken` does not name it).
interface IApproveToken {
    function approve(address spender, uint256 amount) external returns (bool);
}

/// @title OnyxRevenueV2
/// @notice The revenue contract of the credits ledger (ADR-0056, contributor loop CL6): earned
/// shares move into credits, and the treasury keeps a floor. Two changes from `OnyxRevenue`:
/// 1. `withdrawToCredits(p)`: the caller's whole earned balance becomes credit on the credits
///    ledger `p` in one call (`p.depositFor(caller, amount, 2)`). The balance is zeroed and
///    counted paid out before the call, and exactly that amount must leave this contract, so an
///    earned share is never both withdrawable here and credited there (CL6-7). `p` is the
///    caller's choice, as the wallet of `withdraw` is: a wrong `p` can only take the caller's own
///    balance, and its allowance is reset to zero after the call.
/// 2. The split keeps at least `MIN_TREASURY_BPS` for the treasury (R-CR-3, the draft rule R-DB-6
///    of docs/spec/dataset-backing.md): whatever one address holds on a book (miner, validator
///    and label payee at once), it receives less than the fee, so buying one's own data to farm
///    earned credit always loses money.
contract OnyxRevenueV2 is OnyxRevenue {
    /// The treasury's floor in basis points (1%): every wash loop loses at least this.
    uint16 public constant MIN_TREASURY_BPS = 100;
    uint8 internal constant ORIGIN_EARNED = 2;

    event WithdrawnToCredits(address indexed who, address indexed credits, uint256 amount);

    error NothingOwed();
    error TooLarge();
    error NotMoved();

    constructor(
        IRevenueToken token_,
        IOnyxLedgerView ledger_,
        address reporter_,
        address treasury_,
        uint16[4] memory bps
    ) OnyxRevenue(token_, ledger_, reporter_, treasury_, bps) {
        if (bps[3] < MIN_TREASURY_BPS) revert BadSplit();
    }

    /// @notice Move the caller's whole withdrawable balance into `credits` as earned credit.
    function withdrawToCredits(IOnyxCredits credits) external nonReentrant {
        uint256 amount = balanceOf[msg.sender];
        if (amount == 0) revert NothingOwed();
        if (amount > type(uint128).max) revert TooLarge();
        balanceOf[msg.sender] = 0;
        paidOut += amount;
        uint256 before = token.balanceOf(address(this));
        if (!IApproveToken(address(token)).approve(address(credits), amount)) {
            revert TransferFailed();
        }
        credits.depositFor(msg.sender, uint128(amount), ORIGIN_EARNED);
        if (token.balanceOf(address(this)) + amount != before) revert NotMoved();
        if (!IApproveToken(address(token)).approve(address(credits), 0)) revert TransferFailed();
        emit WithdrawnToCredits(msg.sender, address(credits), amount);
    }
}
