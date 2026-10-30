// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxRevenue, IRevenueToken, IOnyxLedgerView} from "../src/OnyxRevenue.sol";
import {OnyxRevenueV2, IOnyxCredits} from "../src/OnyxRevenueV2.sol";
import {OnyxPrepaid, IPrepaidToken} from "../src/OnyxPrepaid.sol";
import {OnyxPrepaidV2, ICreditsToken} from "../src/OnyxPrepaidV2.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {MockUSDC3009Receive} from "./mocks/MockUSDC3009Receive.sol";
import {MockLedgerView} from "./OnyxRevenue.t.sol";

/// Shared setup of the credits ledger tests (ADR-0056, CL6): a token with a real EIP-3009
/// `receiveWithAuthorization`, `OnyxRevenueV2` with two observation books and one shard book on
/// which `alice` is the only miner, and `OnyxPrepaidV2`.
contract CreditsBase is Test {
    MockUSDC3009Receive internal cash;
    MockLedgerView internal lv;
    OnyxRevenueV2 internal rev;
    OnyxPrepaidV2 internal pre;
    address internal reporter = address(0x4E9);
    address internal treasury = address(0x7EA5);
    address internal relayer = address(0x2E1A);
    uint256 internal aliceKey = 0xA11CE;
    uint256 internal bobKey = 0xB0B;
    address internal alice;
    address internal bob;
    bytes32 internal constant FACT_A = keccak256("fact a");
    bytes32 internal constant SHARD = bytes32(uint256(5));
    uint64 internal constant ROUND = 1;
    uint128 internal constant RELAY_CAP = 1e6;
    uint64 internal OBS;

    function setUp() public virtual {
        vm.warp(1_790_000_000);
        alice = vm.addr(aliceKey);
        bob = vm.addr(bobKey);
        cash = new MockUSDC3009Receive();
        lv = new MockLedgerView();
        rev = new OnyxRevenueV2(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(lv)),
            reporter,
            treasury,
            [uint16(5_000), 2_000, 1_000, 2_000]
        );
        OBS = rev.OBSERVATION_ROUND();
        vm.prank(reporter);
        rev.registerObservation(FACT_A);
        // A shard whose only miner is alice (no label set: the label share goes to her too).
        OnyxCommit.CommitmentFields memory f;
        f.root = keccak256("final");
        lv.set(f.root, OnyxCommit.bindingHash(f), false);
        lv.addReveal(alice, f.root);
        rev.register(ROUND, SHARD, f);
        pre = new OnyxPrepaidV2(
            ICreditsToken(address(cash)), rev, reporter, relayer, 1 days, RELAY_CAP
        );
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
        returns (OnyxPrepaidV2.Voucher memory)
    {
        return OnyxPrepaidV2.Voucher({
            buyer: buyer, cumulative: cumulative, signature: _sig(key, buyer, cumulative)
        });
    }

    function _redeem(OnyxPrepaidV2.Voucher memory a) internal {
        OnyxPrepaidV2.Voucher[] memory vs = new OnyxPrepaidV2.Voucher[](1);
        vs[0] = a;
        vm.prank(reporter);
        pre.redeem(vs);
    }

    /// A buyer pays `fee` for the shard book; alice (its only miner) earns 80% of it.
    function _earn(uint128 fee) internal {
        address buyer = address(0xB1);
        cash.mint(buyer, fee);
        vm.startPrank(buyer);
        cash.approve(address(rev), fee);
        rev.pay(ROUND, SHARD, fee);
        vm.stopPrank();
        rev.claim(ROUND, SHARD, alice);
    }

    function _auth(uint256 key, uint128 value, bytes32 nonce, uint256 validBefore)
        internal
        view
        returns (bytes memory)
    {
        address from = vm.addr(key);
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(key, cash.receiveDigest(from, address(pre), value, 0, validBefore, nonce));
        return abi.encodePacked(r, s, v);
    }

    /// Conservation: the token held equals every unspent balance plus the pool.
    function _books(address[] memory who) internal view {
        uint256 owed = pre.pool();
        for (uint256 i = 0; i < who.length; i++) {
            (uint128 d, uint128 r,) = pre.accounts(who[i]);
            owed += d - r;
        }
        assertEq(cash.balanceOf(address(pre)), owed, "books");
    }

    function _ab() internal view returns (address[] memory w) {
        w = new address[](2);
        w[0] = alice;
        w[1] = bob;
    }
}

contract OnyxPrepaidV2Test is CreditsBase {
    /// v1's flow is unchanged: deposits, vouchers redeemed in a batch, allocation to books.
    function test_V1FlowIsUnchangedAndEveryArrivalIsCredited() public {
        vm.expectEmit(true, false, false, true, address(pre));
        emit OnyxPrepaidV2.Credited(alice, 5e6, 1);
        _deposit(alice, 5e6);
        _deposit(bob, 3e6);
        OnyxPrepaidV2.Voucher[] memory vs = new OnyxPrepaidV2.Voucher[](2);
        vs[0] = _v(aliceKey, alice, 750_000);
        vs[1] = _v(bobKey, bob, 1_000_000);
        vm.prank(reporter);
        pre.redeem(vs);
        assertEq(pre.pool(), 1_750_000);
        assertEq(pre.spendable(alice), 5e6 - 750_000);
        OnyxPrepaidV2.Item[] memory items = new OnyxPrepaidV2.Item[](1);
        items[0] = OnyxPrepaidV2.Item({round: OBS, shard: FACT_A, amount: 1_750_000});
        vm.prank(reporter);
        pre.allocate(items);
        assertEq(pre.pool(), 0);
        (,,,,,,, uint128 revA) = rev.books(OBS, FACT_A);
        assertEq(revA, 1_750_000);
        _books(_ab());
    }

    /// CL6-6: `depositFor` never cancels (or sets) an exit, so nobody keeps another from leaving.
    function test_DepositForDoesNotCancelAnExit() public {
        _deposit(alice, 1e6);
        vm.prank(alice);
        pre.requestExit();
        (,, uint64 exitAt) = pre.accounts(alice);
        // A griefer pays 1 unit in for alice, again and again, during her exit delay.
        for (uint256 i = 0; i < 3; i++) {
            vm.prank(bob);
            pre.depositFor(alice, 1, 1);
            (,, uint64 ex) = pre.accounts(alice);
            assertEq(ex, exitAt, "the exit is untouched");
        }
        vm.warp(block.timestamp + 1 days);
        uint256 before = cash.balanceOf(alice);
        vm.prank(alice);
        pre.withdraw();
        assertEq(cash.balanceOf(alice) - before, 1e6 + 3, "she leaves with the gifts too");
        // depositFor never starts an exit either, and an account without an exit stays so.
        vm.prank(bob);
        pre.depositFor(alice, 5, 1);
        (,, uint64 ex2) = pre.accounts(alice);
        assertEq(ex2, 0);
        _books(_ab());
    }

    /// The origin tag is the payer's own: only `revenue` says "earned", only a source the owner
    /// named says "refund" or "payout"; everyone else only "deposit".
    function test_DepositForOriginIsFixedByThePayer() public {
        for (uint8 o = 0; o < 6; o++) {
            if (o == 1) continue;
            vm.expectRevert(OnyxPrepaidV2.BadOrigin.selector);
            vm.prank(bob);
            pre.depositFor(alice, 1, o);
        }
        address storage_ = address(0x5709);
        vm.expectRevert(OnyxPrepaidV2.NotOwner.selector);
        vm.prank(bob);
        pre.setSource(storage_, 3);
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        pre.setSource(address(rev), 3);
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        pre.setSource(storage_, 2);
        pre.setSource(storage_, 3);
        assertEq(pre.originOf(storage_), 3);
        assertEq(pre.originOf(address(rev)), 2);
        _fund(storage_, 100);
        vm.expectRevert(OnyxPrepaidV2.BadOrigin.selector);
        vm.prank(storage_);
        pre.depositFor(alice, 10, 1);
        vm.prank(storage_);
        pre.depositFor(alice, 10, 3);
        pre.setSource(storage_, 0);
        vm.prank(storage_);
        pre.depositFor(alice, 10, 1);
        assertEq(pre.spendable(alice), 20);
        vm.expectRevert(OnyxPrepaidV2.BadAmount.selector);
        vm.prank(bob);
        pre.depositFor(address(0), 1, 1);
        vm.expectRevert(OnyxPrepaidV2.BadAmount.selector);
        vm.prank(bob);
        pre.depositFor(alice, 0, 1);
    }

    /// A relayer submits alice's signed deposit: the credit goes to alice, exactly the amount
    /// that arrived. A forged, replayed, expired or redirected authorization is refused, and the
    /// authorization cannot be front-run into an uncredited transfer (only the payee submits).
    function test_DepositWithAuthorizationCreditsTheSignerOnly() public {
        bytes32 nonce = keccak256("n1");
        uint256 until = block.timestamp + 1 hours;
        bytes memory sig = _auth(aliceKey, 2e6, nonce, until);
        // Front-run: anyone submitting it to the token directly is refused (caller not payee).
        vm.expectRevert("FiatTokenV2: caller must be the payee");
        vm.prank(bob);
        cash.receiveWithAuthorization(alice, address(pre), 2e6, 0, until, nonce, sig);
        // Redirect: the same signature as bob's deposit, or for another amount, fails.
        vm.expectRevert("invalid signature");
        vm.prank(relayer);
        pre.depositWithAuthorization(bob, 2e6, 0, until, nonce, sig);
        vm.expectRevert("invalid signature");
        vm.prank(relayer);
        pre.depositWithAuthorization(alice, 3e6, 0, until, nonce, sig);
        vm.prank(relayer);
        pre.depositWithAuthorization(alice, 2e6, 0, until, nonce, sig);
        assertEq(pre.spendable(alice), 2e6);
        assertEq(pre.spendable(relayer), 0, "the relayer gets no credit");
        // Replay: the nonce is spent.
        vm.expectRevert("authorization is used or canceled");
        vm.prank(relayer);
        pre.depositWithAuthorization(alice, 2e6, 0, until, nonce, sig);
        // Expired.
        bytes32 n2 = keccak256("n2");
        bytes memory late = _auth(aliceKey, 1, n2, block.timestamp + 10);
        vm.warp(block.timestamp + 11);
        vm.expectRevert("authorization is expired");
        pre.depositWithAuthorization(alice, 1, 0, block.timestamp - 1, n2, late);
        // Signed by bob for alice's account: refused.
        bytes memory bobs = _auth(bobKey, 1, keccak256("n3"), block.timestamp + 1 hours);
        vm.expectRevert("invalid signature");
        pre.depositWithAuthorization(alice, 1, 0, block.timestamp + 1 hours, keccak256("n3"), bobs);
        _books(_ab());
    }

    /// Signature malleability: the high-s twin of a deposit authorization, and of a voucher, is
    /// refused.
    function test_HighSTwinsAreRefused() public {
        bytes32 nonce = keccak256("twin");
        uint256 until = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(aliceKey, cash.receiveDigest(alice, address(pre), 1e6, 0, until, nonce));
        bytes32 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
        bytes memory twin =
            abi.encodePacked(r, bytes32(uint256(n) - uint256(s)), v == 27 ? uint8(28) : uint8(27));
        vm.expectRevert("invalid signature");
        pre.depositWithAuthorization(alice, 1e6, 0, until, nonce, twin);
        pre.depositWithAuthorization(alice, 1e6, 0, until, nonce, abi.encodePacked(r, s, v));
        (v, r, s) = vm.sign(aliceKey, pre.voucherDigest(alice, 500));
        twin =
            abi.encodePacked(r, bytes32(uint256(n) - uint256(s)), v == 27 ? uint8(28) : uint8(27));
        assertFalse(pre.signedBy(alice, 500, twin));
        OnyxPrepaidV2.Voucher memory tv =
            OnyxPrepaidV2.Voucher({buyer: alice, cumulative: 500, signature: twin});
        vm.expectRevert(OnyxPrepaidV2.BadVoucher.selector);
        _redeem(tv);
    }

    /// A signed deposit never cancels an exit (an old authorization must not undo a later exit).
    function test_DepositWithAuthorizationDoesNotCancelAnExit() public {
        _deposit(alice, 1e6);
        bytes32 nonce = keccak256("old");
        uint256 until = block.timestamp + 2 days;
        bytes memory sig = _auth(aliceKey, 1e6, nonce, until);
        vm.prank(alice);
        pre.requestExit();
        (,, uint64 exitAt) = pre.accounts(alice);
        vm.prank(bob);
        pre.depositWithAuthorization(alice, 1e6, 0, until, nonce, sig);
        (,, uint64 ex) = pre.accounts(alice);
        assertEq(ex, exitAt);
        vm.warp(block.timestamp + 1 days);
        vm.prank(alice);
        pre.withdraw();
        assertEq(cash.balanceOf(alice), 10e6, "everything back");
    }

    /// Vouchers and spends: a voucher of the v1 contract (another verifying contract) is not
    /// valid here; replayed, lower and over-deposit totals are refused.
    function test_VouchersAreBoundToThisContractAndRiseOnlyWithinTheBalance() public {
        _deposit(alice, 1e6);
        OnyxPrepaid v1 = new OnyxPrepaid(IPrepaidToken(address(cash)), rev, reporter, 1 days);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(aliceKey, v1.voucherDigest(alice, 100));
        OnyxPrepaidV2.Voucher memory old = OnyxPrepaidV2.Voucher({
            buyer: alice, cumulative: 100, signature: abi.encodePacked(r, s, v)
        });
        vm.expectRevert(OnyxPrepaidV2.BadVoucher.selector);
        _redeem(old);
        _redeem(_v(aliceKey, alice, 400));
        OnyxPrepaidV2.Voucher memory again = _v(aliceKey, alice, 400);
        vm.expectRevert(OnyxPrepaidV2.BadAmount.selector);
        _redeem(again);
        OnyxPrepaidV2.Voucher memory lower = _v(aliceKey, alice, 300);
        vm.expectRevert(OnyxPrepaidV2.BadAmount.selector);
        _redeem(lower);
        OnyxPrepaidV2.Voucher memory over = _v(aliceKey, alice, 1e6 + 1);
        vm.expectRevert(OnyxPrepaidV2.BadAmount.selector);
        _redeem(over);
        OnyxPrepaidV2.Voucher memory forged = _v(bobKey, alice, 500);
        vm.expectRevert(OnyxPrepaidV2.BadVoucher.selector);
        _redeem(forged);
        vm.expectRevert(OnyxPrepaidV2.NotReporter.selector);
        pre.redeem(new OnyxPrepaidV2.Voucher[](0));
        // Same formula as v1 and the Rust client: only the address differs.
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

    /// Relay fees: only the reporter pays them, only to the immutable relayer, never above the
    /// pool, never above the daily cap (which resets the next UTC day).
    function test_PayRelayerStaysInThePoolTheCapAndTheRelayer() public {
        _deposit(alice, 5e6);
        _redeem(_v(aliceKey, alice, 3e6));
        vm.expectRevert(OnyxPrepaidV2.NotReporter.selector);
        vm.prank(relayer);
        pre.payRelayer(1);
        vm.startPrank(reporter);
        vm.expectRevert(OnyxPrepaidV2.BadAmount.selector);
        pre.payRelayer(0);
        pre.payRelayer(600_000);
        vm.expectRevert(OnyxPrepaidV2.OverRelayCap.selector);
        pre.payRelayer(400_001);
        pre.payRelayer(400_000);
        assertEq(cash.balanceOf(relayer), RELAY_CAP);
        vm.warp((block.timestamp / 1 days + 1) * 1 days);
        pre.payRelayer(RELAY_CAP);
        vm.expectRevert(OnyxPrepaidV2.OverPool.selector);
        pre.payRelayer(1_000_001);
        vm.stopPrank();
        assertEq(pre.pool(), 1e6);
        assertEq(cash.balanceOf(relayer), 2 * RELAY_CAP);
        _books(_ab());
    }

    /// The relayer is its own key: never the reporter or the owner, at deploy or by rotation.
    function test_TheRelayerIsNeverTheReporterOrTheOwner() public {
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        new OnyxPrepaidV2(ICreditsToken(address(cash)), rev, reporter, reporter, 1 days, 1);
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        new OnyxPrepaidV2(ICreditsToken(address(cash)), rev, reporter, address(this), 1 days, 1);
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        new OnyxPrepaidV2(ICreditsToken(address(cash)), rev, reporter, relayer, 1 days, 0);
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        new OnyxPrepaidV2(ICreditsToken(address(cash)), rev, reporter, address(0), 1 days, 1);
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        new OnyxPrepaidV2(ICreditsToken(address(cash)), rev, reporter, relayer, 59 minutes, 1);
        vm.expectRevert(OnyxPrepaidV2.BadParams.selector);
        pre.setReporter(relayer);
        vm.expectRevert(OnyxPrepaidV2.NotOwner.selector);
        vm.prank(reporter);
        pre.setReporter(address(9));
        pre.setReporter(address(9));
        assertEq(pre.reporter(), address(9));
        vm.chainId(8453);
        vm.expectRevert(abi.encodeWithSelector(OnyxPrepaidV2.WrongChain.selector, 8453));
        new OnyxPrepaidV2(ICreditsToken(address(cash)), rev, reporter, relayer, 1 days, 1);
    }

    /// The exit rules are v1's: the delay, spent money stays spent, only `deposit` cancels.
    function test_ExitsAreV1s() public {
        _deposit(alice, 2e6);
        _redeem(_v(aliceKey, alice, 500_000));
        vm.prank(alice);
        pre.requestExit();
        assertEq(pre.spendable(alice), 0);
        vm.expectRevert(OnyxPrepaidV2.TooEarly.selector);
        vm.prank(alice);
        pre.withdraw();
        _redeem(_v(aliceKey, alice, 800_000));
        _deposit(alice, 1);
        (,, uint64 ex) = pre.accounts(alice);
        assertEq(ex, 0, "her own deposit cancels her exit");
        vm.prank(alice);
        pre.requestExit();
        vm.warp(block.timestamp + 1 days);
        uint256 before = cash.balanceOf(alice);
        vm.prank(alice);
        pre.withdraw();
        assertEq(cash.balanceOf(alice) - before, 2e6 + 1 - 800_000);
        vm.expectRevert(OnyxPrepaidV2.NoExit.selector);
        vm.prank(alice);
        pre.withdraw();
        vm.expectRevert(OnyxPrepaidV2.BadAmount.selector);
        vm.prank(alice);
        pre.requestExit();
        _books(_ab());
    }

    /// No mint path: whatever sequence of deposits, gifts, signed deposits, redemptions,
    /// allocations, relay fees and exits, no balance or pool exceeds what arrived, and the token
    /// held equals every unspent balance plus the pool.
    function testFuzz_CreditOnlyFromArrivedAsset(
        uint128 da,
        uint128 gift,
        uint128 signed,
        uint128 spend,
        uint128 relayFee,
        bool exitA
    ) public {
        da = uint128(bound(da, 1, 5e6));
        gift = uint128(bound(gift, 1, 1e6));
        signed = uint128(bound(signed, 1, 2e6));
        _deposit(alice, da);
        vm.prank(bob);
        pre.depositFor(alice, gift, 1);
        bytes32 nonce = keccak256(abi.encode(signed));
        uint256 until = block.timestamp + 1 hours;
        pre.depositWithAuthorization(
            alice, signed, 0, until, nonce, _auth(aliceKey, signed, nonce, until)
        );
        uint256 arrived = uint256(da) + gift + signed;
        (uint128 d,,) = pre.accounts(alice);
        assertEq(d, arrived, "credit equals what arrived");
        spend = uint128(bound(spend, 1, arrived));
        _redeem(_v(aliceKey, alice, spend));
        relayFee = uint128(bound(relayFee, 1, spend < RELAY_CAP ? spend : RELAY_CAP));
        vm.prank(reporter);
        pre.payRelayer(relayFee);
        if (spend > relayFee) {
            OnyxPrepaidV2.Item[] memory items = new OnyxPrepaidV2.Item[](1);
            items[0] = OnyxPrepaidV2.Item({round: OBS, shard: FACT_A, amount: spend - relayFee});
            vm.prank(reporter);
            pre.allocate(items);
        }
        uint256 withdrawn;
        if (exitA && spend < arrived) {
            vm.prank(alice);
            pre.requestExit();
            vm.warp(block.timestamp + 1 days);
            uint256 before = cash.balanceOf(alice);
            vm.prank(alice);
            pre.withdraw();
            withdrawn = cash.balanceOf(alice) - before;
        }
        _books(_ab());
        assertEq(pre.pool(), 0);
        // Every unit that arrived is still held, or left to a book, the relayer or its owner.
        assertEq(
            cash.balanceOf(address(pre)) + cash.balanceOf(relayer) + cash.balanceOf(address(rev))
                + withdrawn,
            arrived,
            "every unit is accounted for"
        );
    }
}

/// A credits contract that takes nothing (or takes and re-enters), for `withdrawToCredits`.
contract BadCredits is IOnyxCredits {
    OnyxRevenueV2 internal rev;
    uint8 public mode;

    constructor(OnyxRevenueV2 r, uint8 m) {
        rev = r;
        mode = m;
    }

    function depositFor(address, uint128 amount, uint8) external {
        if (mode == 1) {
            // Take half only.
            IRevenueToken(address(rev.token())).transferFrom(msg.sender, address(this), amount / 2);
        } else if (mode == 2) {
            rev.withdraw();
        }
    }
}

contract OnyxRevenueV2Test is CreditsBase {
    /// CL6-7: an earned share becomes credit exactly once; it is then neither withdrawable from
    /// `OnyxRevenue` nor movable again; the revenue books still balance.
    function test_WithdrawToCreditsMovesTheBalanceOnce() public {
        _earn(1_000_000);
        assertEq(rev.balanceOf(alice), 800_000);
        uint256 unalloc = rev.unallocated();
        vm.expectEmit(true, false, false, true, address(pre));
        emit OnyxPrepaidV2.Credited(alice, 800_000, 2);
        vm.prank(alice);
        rev.withdrawToCredits(IOnyxCredits(address(pre)));
        assertEq(rev.balanceOf(alice), 0);
        assertEq(pre.spendable(alice), 800_000);
        assertEq(cash.allowance(address(rev), address(pre)), 0, "no allowance left");
        assertEq(rev.unallocated(), unalloc, "revenue books unchanged");
        vm.expectRevert(OnyxRevenueV2.NothingOwed.selector);
        vm.prank(alice);
        rev.withdrawToCredits(IOnyxCredits(address(pre)));
        uint256 before = cash.balanceOf(alice);
        vm.prank(alice);
        rev.withdraw();
        assertEq(cash.balanceOf(alice), before, "nothing left to withdraw");
        _books(_ab());
    }

    /// A credits contract that takes less than the balance, or re-enters, reverts the move: the
    /// balance stays with its earner.
    function test_WithdrawToCreditsRefusesAShortOrReentrantCredit() public {
        _earn(1_000_000);
        BadCredits half = new BadCredits(rev, 1);
        vm.expectRevert(OnyxRevenueV2.NotMoved.selector);
        vm.prank(alice);
        rev.withdrawToCredits(IOnyxCredits(address(half)));
        BadCredits reenter = new BadCredits(rev, 2);
        vm.expectRevert(OnyxRevenue.Reentrancy.selector);
        vm.prank(alice);
        rev.withdrawToCredits(IOnyxCredits(address(reenter)));
        assertEq(rev.balanceOf(alice), 800_000);
        // Only `revenue` may tag a credit "earned": a direct depositFor with origin 2 fails.
        vm.expectRevert(OnyxPrepaidV2.BadOrigin.selector);
        vm.prank(alice);
        pre.depositFor(alice, 1, 2);
    }

    /// R-CR-3: a split with less than MIN_TREASURY_BPS for the treasury is refused.
    function test_RevenueV2KeepsATreasuryFloor() public {
        vm.expectRevert(OnyxRevenue.BadSplit.selector);
        new OnyxRevenueV2(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(lv)),
            reporter,
            treasury,
            [uint16(6_000), 3_000, 1_000, 0]
        );
        vm.expectRevert(OnyxRevenue.BadSplit.selector);
        new OnyxRevenueV2(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(lv)),
            reporter,
            treasury,
            [uint16(6_000), 3_000, 1_001, 99]
        );
    }

    /// CL6-1 (R-CR-3): over every split the constructor accepts and every book shape, one
    /// address that is a miner, a validator and the label payee at once gets back less than
    /// the fee it paid: wash buying always loses at least MIN_TREASURY_BPS.
    function testFuzz_NoAddressEarnsBackAFullFee(
        uint16 m,
        uint16 v,
        uint16 l,
        uint128 amount,
        uint32 miners,
        uint32 validators,
        bool hasLabel
    ) public {
        uint256 floor = rev.MIN_TREASURY_BPS();
        m = uint16(bound(m, 0, 10_000 - floor));
        v = uint16(bound(v, 0, 10_000 - floor - m));
        l = uint16(bound(l, 0, 10_000 - floor - m - v));
        uint16 t = uint16(10_000 - m - v - l);
        OnyxRevenueV2 r = new OnyxRevenueV2(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(lv)),
            reporter,
            treasury,
            [m, v, l, t]
        );
        amount = uint128(bound(amount, 1, type(uint128).max / 10_000));
        miners = uint32(bound(miners, 0, 64));
        validators = uint32(bound(validators, 0, 128));
        (uint256 pm, uint256 pv, uint256 label, uint256 toTreasury) =
            r.split(amount, miners, validators, hasLabel);
        uint256 own = pm + pv + label;
        assertLt(own, amount, "one address never gets the whole fee back");
        assertEq(pm * miners + pv * validators + label + toTreasury, amount, "split is exact");
        assertGe(toTreasury, uint256(amount) * floor / 10_000, "treasury keeps its floor");
    }
}
