// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, Vm} from "forge-std/Test.sol";
import {OnyxRevenue, IRevenueToken, IOnyxLedgerView} from "../src/OnyxRevenue.sol";
import {OnyxPrepaid, IPrepaidToken} from "../src/OnyxPrepaid.sol";
import {MockUSDC3009} from "./mocks/MockUSDC3009.sol";
import {MockLedgerView} from "./OnyxRevenue.t.sol";

/// Partner tracker T7 (pre-public loop PP5, docs/spec/prepaid.md): deposits, signed running-total
/// vouchers redeemed in batches, allocation to revenue books without naming buyers, exits.
contract OnyxPrepaidTest is Test {
    MockUSDC3009 internal cash;
    OnyxRevenue internal rev;
    OnyxPrepaid internal pre;
    address internal reporter = address(0x4E9);
    address internal treasury = address(0x7EA5);
    uint256 internal aliceKey = 0xA11CE;
    uint256 internal bobKey = 0xB0B;
    address internal alice;
    address internal bob;
    bytes32 internal constant FACT_A = keccak256("fact a");
    bytes32 internal constant FACT_B = keccak256("fact b");
    uint64 internal OBS;

    function setUp() public {
        vm.warp(1_790_000_000);
        alice = vm.addr(aliceKey);
        bob = vm.addr(bobKey);
        cash = new MockUSDC3009();
        uint16[4] memory bps = [uint16(5_000), 2_000, 1_000, 2_000];
        rev = new OnyxRevenue(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(new MockLedgerView())),
            reporter,
            treasury,
            bps
        );
        OBS = rev.OBSERVATION_ROUND();
        vm.startPrank(reporter);
        rev.registerObservation(FACT_A);
        rev.registerObservation(FACT_B);
        vm.stopPrank();
        pre = new OnyxPrepaid(IPrepaidToken(address(cash)), rev, reporter, 1 days);
        _fund(alice, 10e6);
        _fund(bob, 10e6);
    }

    function _fund(address who, uint256 amount) internal {
        cash.mint(who, amount);
        vm.prank(who);
        cash.approve(address(pre), type(uint256).max);
    }

    function _deposit(address who, uint128 amount) internal {
        vm.prank(who);
        pre.deposit(amount);
    }

    function _sig(uint256 key, address buyer, uint128 cumulative)
        internal
        view
        returns (bytes memory)
    {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, pre.voucherDigest(buyer, cumulative));
        return abi.encodePacked(r, s, v);
    }

    function _v(uint256 key, address buyer, uint128 cumulative)
        internal
        view
        returns (OnyxPrepaid.Voucher memory)
    {
        return OnyxPrepaid.Voucher({
            buyer: buyer, cumulative: cumulative, signature: _sig(key, buyer, cumulative)
        });
    }

    function _redeem(OnyxPrepaid.Voucher memory a) internal {
        OnyxPrepaid.Voucher[] memory vs = new OnyxPrepaid.Voucher[](1);
        vs[0] = a;
        vm.prank(reporter);
        pre.redeem(vs);
    }

    /// Tokens conserved: the contract holds exactly the unspent deposits plus the pool.
    function _books() internal view {
        (uint128 da, uint128 ra,) = pre.accounts(alice);
        (uint128 db, uint128 rb,) = pre.accounts(bob);
        assertEq(cash.balanceOf(address(pre)), uint256(da - ra) + (db - rb) + pre.pool(), "books");
    }

    function test_DepositsVouchersBatchesAndAllocationsSettleIntoRevenue() public {
        _deposit(alice, 5e6);
        _deposit(bob, 3e6);
        // Off chain: alice paid 3 queries of 0.25 USDC (vouchers 0.25, 0.50, 0.75), bob one of 1.
        OnyxPrepaid.Voucher[] memory vs = new OnyxPrepaid.Voucher[](2);
        vs[0] = _v(aliceKey, alice, 750_000);
        vs[1] = _v(bobKey, bob, 1_000_000);
        vm.prank(reporter);
        pre.redeem(vs);
        assertEq(pre.pool(), 1_750_000);
        assertEq(pre.spendable(alice), 5e6 - 750_000);
        _books();
        OnyxPrepaid.Item[] memory items = new OnyxPrepaid.Item[](2);
        items[0] = OnyxPrepaid.Item({round: OBS, shard: FACT_A, amount: 1_250_000});
        items[1] = OnyxPrepaid.Item({round: OBS, shard: FACT_B, amount: 500_000});
        vm.recordLogs();
        vm.prank(reporter);
        pre.allocate(items);
        assertEq(pre.pool(), 0);
        (,,,,,,, uint128 revA) = rev.books(OBS, FACT_A);
        (,,,,,,, uint128 revB) = rev.books(OBS, FACT_B);
        assertEq(revA, 1_250_000);
        assertEq(revB, 500_000);
        assertEq(cash.balanceOf(address(rev)), 1_750_000);
        // No log of the allocation names a buyer.
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i = 0; i < logs.length; i++) {
            for (uint256 t = 0; t < logs[i].topics.length; t++) {
                assertTrue(
                    logs[i].topics[t] != bytes32(uint256(uint160(alice)))
                        && logs[i].topics[t] != bytes32(uint256(uint160(bob)))
                );
            }
            assertEq(_contains(logs[i].data, alice) || _contains(logs[i].data, bob), false);
        }
        _books();
    }

    function _contains(bytes memory data, address a) internal pure returns (bool) {
        bytes20 x = bytes20(a);
        for (uint256 i = 0; i + 20 <= data.length; i++) {
            bool same = true;
            for (uint256 j = 0; j < 20 && same; j++) {
                same = data[i + j] == x[j];
            }
            if (same) return true;
        }
        return false;
    }

    function test_OnlySignedRisingTotalsWithinTheDepositRedeem() public {
        _deposit(alice, 1e6);
        OnyxPrepaid.Voucher memory v1 = _v(bobKey, alice, 100); // signed by someone else
        vm.expectRevert(OnyxPrepaid.BadVoucher.selector);
        _redeem(v1);
        OnyxPrepaid.Voucher memory v2 = _v(aliceKey, alice, 1e6 + 1); // above the deposit
        vm.expectRevert(OnyxPrepaid.BadAmount.selector);
        _redeem(v2);
        _redeem(_v(aliceKey, alice, 400));
        OnyxPrepaid.Voucher memory v3 = _v(aliceKey, alice, 400); // replay
        vm.expectRevert(OnyxPrepaid.BadAmount.selector);
        _redeem(v3);
        OnyxPrepaid.Voucher memory v4 = _v(aliceKey, alice, 300); // an older, smaller total
        vm.expectRevert(OnyxPrepaid.BadAmount.selector);
        _redeem(v4);
        // The high-s twin of a valid signature is refused (no malleable copy).
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(aliceKey, pre.voucherDigest(alice, 500));
        bytes32 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
        bytes memory twin =
            abi.encodePacked(r, bytes32(uint256(n) - uint256(s)), v == 27 ? uint8(28) : uint8(27));
        assertFalse(pre.signedBy(alice, 500, twin));
        assertTrue(pre.signedBy(alice, 500, abi.encodePacked(r, s, v)));
        assertFalse(pre.signedBy(alice, 500, abi.encodePacked(r, s)));
        // Another contract (or chain) gives another digest: a voucher cannot be replayed there.
        OnyxPrepaid other = new OnyxPrepaid(IPrepaidToken(address(cash)), rev, reporter, 1 days);
        assertTrue(other.voucherDigest(alice, 500) != pre.voucherDigest(alice, 500));
        vm.expectRevert(OnyxPrepaid.NotReporter.selector);
        pre.redeem(new OnyxPrepaid.Voucher[](0));
        _books();
    }

    function test_AnExitWaitsForTheDelayAndLeavesSpentMoneySpent() public {
        _deposit(alice, 2e6);
        _redeem(_v(aliceKey, alice, 500_000));
        bytes memory late = _sig(aliceKey, alice, 800_000); // signed for queries already served
        vm.prank(alice);
        pre.requestExit();
        assertEq(pre.spendable(alice), 0, "no voucher is taken while an exit is pending");
        vm.expectRevert(OnyxPrepaid.TooEarly.selector);
        vm.prank(alice);
        pre.withdraw();
        // Inside the delay the reporter redeems the last voucher the buyer signed.
        _redeem(OnyxPrepaid.Voucher({buyer: alice, cumulative: 800_000, signature: late}));
        vm.warp(block.timestamp + 1 days);
        uint256 before = cash.balanceOf(alice);
        vm.prank(alice);
        pre.withdraw();
        assertEq(cash.balanceOf(alice) - before, 2e6 - 800_000);
        (uint128 d, uint128 rd, uint64 ex) = pre.accounts(alice);
        assertEq(d, rd);
        assertEq(ex, 0);
        OnyxPrepaid.Voucher memory v5 = _v(aliceKey, alice, 900_000); // nothing left to spend
        vm.expectRevert(OnyxPrepaid.BadAmount.selector);
        _redeem(v5);
        vm.expectRevert(OnyxPrepaid.NoExit.selector);
        vm.prank(alice);
        pre.withdraw();
        // A new deposit continues the running total.
        _deposit(alice, 1e6);
        _redeem(_v(aliceKey, alice, 1_000_000));
        assertEq(pre.pool(), 1_000_000);
        _books();
    }

    function test_ADepositCancelsAnExit() public {
        _deposit(alice, 1e6);
        vm.prank(alice);
        pre.requestExit();
        _deposit(alice, 1);
        (,, uint64 ex) = pre.accounts(alice);
        assertEq(ex, 0);
        vm.warp(block.timestamp + 2 days);
        vm.expectRevert(OnyxPrepaid.NoExit.selector);
        vm.prank(alice);
        pre.withdraw();
    }

    function test_AllocationNeverExceedsThePoolAndOnlyTheReporterAllocates() public {
        _deposit(alice, 1e6);
        _redeem(_v(aliceKey, alice, 100));
        OnyxPrepaid.Item[] memory items = new OnyxPrepaid.Item[](1);
        items[0] = OnyxPrepaid.Item({round: OBS, shard: FACT_A, amount: 101});
        vm.expectRevert(OnyxPrepaid.OverPool.selector);
        vm.prank(reporter);
        pre.allocate(items);
        items[0].amount = 100;
        vm.expectRevert(OnyxPrepaid.NotReporter.selector);
        pre.allocate(items);
        items[0].shard = keccak256("unregistered");
        vm.expectRevert(OnyxRevenue.NotRegistered.selector);
        vm.prank(reporter);
        pre.allocate(items);
        items[0].shard = FACT_A;
        items[0].amount = 0;
        vm.expectRevert(OnyxPrepaid.BadAmount.selector);
        vm.prank(reporter);
        pre.allocate(items);
    }

    function test_TheOwnerRotatesTheReporterAndParamsAreBounded() public {
        vm.expectRevert(OnyxPrepaid.NotOwner.selector);
        vm.prank(reporter);
        pre.setReporter(address(7));
        pre.setReporter(address(7));
        assertEq(pre.reporter(), address(7));
        vm.expectRevert(OnyxPrepaid.BadParams.selector);
        new OnyxPrepaid(IPrepaidToken(address(cash)), rev, reporter, 59 minutes);
        vm.chainId(8453);
        vm.expectRevert(abi.encodeWithSelector(OnyxPrepaid.WrongChain.selector, 8453));
        new OnyxPrepaid(IPrepaidToken(address(cash)), rev, reporter, 1 days);
    }

    /// Any order of deposits, redemptions, allocations and exits keeps the books.
    function testFuzz_BooksBalance(
        uint128 da,
        uint128 db,
        uint128 sa,
        uint128 sb,
        uint128 alloc,
        bool exitA
    ) public {
        da = uint128(bound(da, 1, 10e6));
        db = uint128(bound(db, 1, 10e6));
        sa = uint128(bound(sa, 1, da));
        sb = uint128(bound(sb, 1, db));
        _deposit(alice, da);
        _deposit(bob, db);
        OnyxPrepaid.Voucher[] memory vs = new OnyxPrepaid.Voucher[](2);
        vs[0] = _v(aliceKey, alice, sa);
        vs[1] = _v(bobKey, bob, sb);
        vm.prank(reporter);
        pre.redeem(vs);
        alloc = uint128(bound(alloc, 1, uint256(sa) + sb));
        OnyxPrepaid.Item[] memory items = new OnyxPrepaid.Item[](1);
        items[0] = OnyxPrepaid.Item({round: OBS, shard: FACT_A, amount: alloc});
        vm.prank(reporter);
        pre.allocate(items);
        if (exitA && sa < da) {
            vm.prank(alice);
            pre.requestExit();
            vm.warp(block.timestamp + 1 days);
            vm.prank(alice);
            pre.withdraw();
        }
        _books();
        assertEq(cash.balanceOf(address(rev)), alloc);
    }

    /// The voucher digest and signature are the Rust client's (fixtures/prepaid/vectors.json).
    function test_SharedVoucherVectors() public view {
        string memory json = vm.readFile("../fixtures/prepaid/vectors.json");
        uint256 chain = vm.parseJsonUint(json, ".chainId");
        address at = vm.parseJsonAddress(json, ".verifyingContract");
        bytes32 domain = keccak256(
            abi.encode(
                keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
                ),
                keccak256("OnyxPrepaid"),
                keccak256("1"),
                chain,
                at
            )
        );
        assertEq(domain, vm.parseJsonBytes32(json, ".domainSeparator"));
        uint256 n = vm.parseJsonUint(json, ".count");
        for (uint256 i = 0; i < n; i++) {
            string memory b = string.concat(".vouchers[", vm.toString(i), "]");
            address buyer = vm.parseJsonAddress(json, string.concat(b, ".buyer"));
            uint128 cum = uint128(vm.parseJsonUint(json, string.concat(b, ".cumulative")));
            bytes32 digest = keccak256(
                abi.encodePacked(
                    "\x19\x01", domain, keccak256(abi.encode(pre.VOUCHER_TYPEHASH(), buyer, cum))
                )
            );
            assertEq(digest, vm.parseJsonBytes32(json, string.concat(b, ".digest")));
            bytes memory sig = vm.parseJsonBytes(json, string.concat(b, ".signature"));
            (bytes32 r, bytes32 s) = abi.decode(sig, (bytes32, bytes32));
            uint8 v = uint8(sig[64]);
            assertEq(ecrecover(digest, v, r, s), buyer);
        }
        // The deployed contract uses the same formula with its own address and chain.
        bytes32 own = keccak256(
            abi.encode(
                keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
                ),
                keccak256("OnyxPrepaid"),
                keccak256("1"),
                block.chainid,
                address(pre)
            )
        );
        assertEq(own, pre.domainSeparator());
    }
}
