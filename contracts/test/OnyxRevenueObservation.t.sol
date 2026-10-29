// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxRevenue, IOnyxLedgerView, IRevenueToken} from "../src/OnyxRevenue.sol";
import {MockUSDC3009} from "./mocks/MockUSDC3009.sol";

/// Level-2 observation books (R-REV-12, ADR-0027 amendment 4): the API's x402 sales of a level-2
/// fact are credited to `(OBSERVATION_ROUND, fact)`, which has no miners, validators or label set,
/// so the whole fee goes to the treasury (THREATS L2API-3).
contract OnyxRevenueObservationTest is Test {
    MockUSDC3009 internal cash;
    OnyxRevenue internal rev;
    address internal reporter = address(0x4E9);
    address internal treasury = address(0x7EA5);
    address internal payer = address(0x9A7E4);
    bytes32 internal constant FACT = keccak256("a level-2 fact");
    uint64 internal obs;

    function setUp() public {
        cash = new MockUSDC3009();
        uint16[4] memory bps = [uint16(5_000), 2_000, 1_000, 2_000];
        // The ledger is never read for an observation book.
        rev = new OnyxRevenue(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(0x1ED6E5)),
            reporter,
            treasury,
            bps
        );
        obs = rev.OBSERVATION_ROUND();
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

    function test_ObservationBookIsReporterOnlyOnceAndNonZero() public {
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRevenue.NotReporter.selector);
        rev.registerObservation(FACT);
        vm.prank(reporter);
        vm.expectRevert(OnyxRevenue.BadFields.selector);
        rev.registerObservation(bytes32(0));
        vm.expectEmit(true, false, false, false);
        emit OnyxRevenue.ObservationRegistered(FACT);
        vm.prank(reporter);
        rev.registerObservation(FACT);
        (bool registered, uint32 miners, uint32 validators,, bytes32 labelSet,,,) =
            rev.books(obs, FACT);
        assertTrue(registered);
        assertEq(miners, 0);
        assertEq(validators, 0);
        assertEq(labelSet, bytes32(0));
        vm.prank(reporter);
        vm.expectRevert(OnyxRevenue.AlreadyRegistered.selector);
        rev.registerObservation(FACT);
    }

    function test_ASelfSettledObservationSaleGoesWhollyToTheTreasury() public {
        vm.prank(reporter);
        rev.registerObservation(FACT);
        cash.mint(payer, 2_500);
        vm.prank(reporter);
        rev.settle(obs, FACT, _auth(2_500, keccak256("n1")), hex"00");
        assertEq(rev.unallocated(), 0, "credited in the same call");
        assertEq(rev.balanceOf(treasury), 2_500, "no miners, validators or labels: all treasury");
        assertEq(rev.claim(obs, FACT, payer), 0, "nobody else has a role in the book");
    }

    function test_AReceiptAndADirectPaymentCreditTheObservationBook() public {
        // A facilitator settled the payment to the contract (the x402 path).
        bytes32 nonce = keccak256("n2");
        cash.mint(payer, 1_000);
        cash.transferWithAuthorization(payer, address(rev), 1_000, nonce);
        // Unregistered: the receipt is skipped (why 4), never reverting.
        OnyxRevenue.Receipt[] memory rs = new OnyxRevenue.Receipt[](1);
        rs[0] = OnyxRevenue.Receipt({
            round: obs, shard: FACT, amount: 1_000, payer: payer, nonce: nonce
        });
        vm.prank(reporter);
        assertEq(rev.credit(rs), 0);
        vm.prank(reporter);
        rev.registerObservation(FACT);
        vm.prank(reporter);
        assertEq(rev.credit(rs), 1);
        assertEq(rev.balanceOf(treasury), 1_000);
        // A direct payment for the fact, no reporter.
        cash.mint(address(this), 500);
        cash.approve(address(rev), 500);
        rev.pay(obs, FACT, 500);
        assertEq(rev.balanceOf(treasury), 1_500);
    }

    function test_TheObservationRoundCannotBeRegisteredAsAShard() public {
        OnyxCommit.CommitmentFields memory f;
        vm.expectRevert(OnyxRevenue.BadFields.selector);
        rev.register(obs, FACT, f);
    }
}
