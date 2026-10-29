// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxSettlementFixture} from "./OnyxSettlement.t.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxRevenue, IOnyxLedgerView, IRevenueToken} from "../src/OnyxRevenue.sol";
import {MockUSDC3009} from "./mocks/MockUSDC3009.sol";

/// Self-settled x402 payments (ADR-0026, R-REV-8): the reporter submits the client's EIP-3009
/// authorization to `OnyxRevenue.settle`, which moves the funds and credits the shard in one call.
/// The mock token checks the window and the nonce, not the signature (the Base Sepolia fork test,
/// OnyxRevenueFork.t.sol, runs the same path on the real USDC bytecode).
contract OnyxRevenueSettleTest is OnyxSettlementFixture {
    MockUSDC3009 internal cash;
    OnyxRevenue internal rev;
    address internal reporter = address(0x4E9);
    address internal treasury = address(0x7EA5);
    address internal labelOwner = address(0x1AB);
    address internal payer = address(0x9A7E4);

    function _final() internal {
        cash = new MockUSDC3009();
        uint16[4] memory bps = [uint16(5_000), 2_000, 1_000, 2_000];
        rev = new OnyxRevenue(
            IRevenueToken(address(cash)), IOnyxLedgerView(address(st)), reporter, treasury, bps
        );
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, labelOwner);
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.roll(block.number + 200);
        rev.register(ID, shard, fields);
    }

    function _auth(uint256 value, bytes32 nonce)
        internal
        view
        returns (OnyxRevenue.Authorization memory)
    {
        return OnyxRevenue.Authorization({
            from: payer,
            value: value,
            validAfter: 0,
            validBefore: block.timestamp + 600,
            nonce: nonce
        });
    }

    function test_SettleMovesTheFundsAndCreditsTheShardInOneCall() public {
        _final();
        cash.mint(payer, 10_000);
        vm.expectEmit(true, true, true, true);
        emit OnyxRevenue.Settled(ID, shard, payer, keccak256("n1"), 10_000);
        vm.prank(reporter);
        rev.settle(ID, shard, _auth(10_000, keccak256("n1")), hex"00");
        assertEq(cash.balanceOf(address(rev)), 10_000);
        assertEq(rev.unallocated(), 0, "credited in the same call, nothing left unattributed");
        (,,,,,,, uint128 revenue) = rev.books(ID, shard);
        assertEq(revenue, 10_000);
        assertEq(rev.claim(ID, shard, m1), 2_500);
        assertEq(rev.balanceOf(labelOwner), 1_000);
        assertEq(rev.balanceOf(treasury), 2_002);
    }

    function test_SettleIsReporterOnlyAndNeedsARegisteredShard() public {
        _final();
        cash.mint(payer, 1_000);
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRevenue.NotReporter.selector);
        rev.settle(ID, shard, _auth(1_000, keccak256("n1")), hex"00");
        vm.prank(reporter);
        vm.expectRevert(OnyxRevenue.NotRegistered.selector);
        rev.settle(ID, keccak256("unregistered"), _auth(1_000, keccak256("n1")), hex"00");
        assertEq(cash.balanceOf(payer), 1_000, "nothing moved");
    }

    /// A settled payment is attributed once: settle refuses the same (payer, nonce) again before
    /// calling the token, and the receipt path skips it as already credited.
    function test_SettledPaymentIsNeverAttributedTwice() public {
        _final();
        cash.mint(payer, 2_000);
        vm.startPrank(reporter);
        rev.settle(ID, shard, _auth(1_000, keccak256("n1")), hex"00");
        vm.expectRevert(OnyxRevenue.AlreadyCredited.selector);
        rev.settle(ID, shard, _auth(1_000, keccak256("n1")), hex"00");
        OnyxRevenue.Receipt[] memory rs = new OnyxRevenue.Receipt[](1);
        rs[0] = OnyxRevenue.Receipt({
            round: ID, shard: shard, amount: 1_000, payer: payer, nonce: keccak256("n1")
        });
        assertEq(rev.credit(rs), 0, "skipped: already credited");
        vm.stopPrank();
        (,,,,,,, uint128 revenue) = rev.books(ID, shard);
        assertEq(revenue, 1_000);
    }

    /// Front-running: a third party submits the authorization to the token first. The funds
    /// still land here (the signature fixes `to`), settle reverts with nothing credited, and the
    /// receipt path attributes the payment, total-capped (REV-1).
    function test_FrontRunAuthorizationFallsBackToTheReceiptPath() public {
        _final();
        cash.mint(payer, 1_000);
        vm.prank(address(0xF0E));
        cash.transferWithAuthorization(
            payer, address(rev), 1_000, 0, block.timestamp + 600, keccak256("n1"), hex"00"
        );
        assertEq(rev.unallocated(), 1_000);
        vm.startPrank(reporter);
        vm.expectRevert(bytes("authorization is used or canceled"));
        rev.settle(ID, shard, _auth(1_000, keccak256("n1")), hex"00");
        OnyxRevenue.Receipt[] memory rs = new OnyxRevenue.Receipt[](1);
        rs[0] = OnyxRevenue.Receipt({
            round: ID, shard: shard, amount: 1_000, payer: payer, nonce: keccak256("n1")
        });
        assertEq(rev.credit(rs), 1);
        vm.stopPrank();
        assertEq(rev.unallocated(), 0);
    }

    /// An authorization outside its window or for a payer without funds moves nothing.
    function test_SettleRefusesExpiredOrUnfundedAuthorizations() public {
        _final();
        OnyxRevenue.Authorization memory a = _auth(1_000, keccak256("n1"));
        a.validBefore = block.timestamp;
        cash.mint(payer, 1_000);
        vm.startPrank(reporter);
        vm.expectRevert(bytes("authorization is expired"));
        rev.settle(ID, shard, a, hex"00");
        vm.expectRevert();
        rev.settle(ID, shard, _auth(5_000, keccak256("n2")), hex"00");
        vm.stopPrank();
        assertEq(cash.balanceOf(payer), 1_000);
        assertEq(rev.allocated(), 0);
    }

    /// A token that moves less than the authorized value (fee on transfer, a broken token) is
    /// refused: the credit always equals what arrived.
    function test_SettleRefusesAShortTransfer() public {
        ShortToken t = new ShortToken();
        uint16[4] memory bps = [uint16(5_000), 2_000, 1_000, 2_000];
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.roll(block.number + 200);
        rev = new OnyxRevenue(
            IRevenueToken(address(t)), IOnyxLedgerView(address(st)), reporter, treasury, bps
        );
        rev.register(ID, shard, fields);
        t.mint(payer, 1_000);
        vm.prank(reporter);
        vm.expectRevert(OnyxRevenue.NotReceived.selector);
        rev.settle(ID, shard, _auth(1_000, keccak256("n1")), hex"00");
    }
}

/// Moves one unit less than authorized.
contract ShortToken is MockUSDC3009 {
    function transferWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256,
        uint256,
        bytes32 nonce,
        bytes calldata
    ) external override {
        authorizationState[from][nonce] = true;
        balanceOf[from] -= value;
        balanceOf[to] += value - 1;
    }
}
