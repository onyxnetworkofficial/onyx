// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxCommit} from "../../src/OnyxCommit.sol";
import {IRevenueToken, IOnyxLedgerView} from "../../src/OnyxRevenue.sol";
import {OnyxRevenueV2, IOnyxCredits} from "../../src/OnyxRevenueV2.sol";
import {OnyxPrepaidV2, ICreditsToken} from "../../src/OnyxPrepaidV2.sol";
import {MockUSDC3009Receive} from "../mocks/MockUSDC3009Receive.sol";
import {MockLedgerView} from "../OnyxRevenue.t.sol";

/// Handler for the credits ledger invariants (ADR-0056, CL6): three accounts with keys, deposits,
/// gifts (`depositFor`), signed deposits through a relayer, earned shares moved in from
/// `OnyxRevenueV2`, spends (vouchers redeemed), allocations, relay fees, exits and withdrawals,
/// time passing; and attacks that must all fail: a forged or replayed voucher, a forged
/// "earned" tag, a replayed deposit authorization, a relay fee paid by someone other than the
/// reporter or above the day's cap.
contract CreditsHandler is Test {
    MockUSDC3009Receive public cash;
    OnyxRevenueV2 public rev;
    OnyxPrepaidV2 public pre;
    address public reporter = address(0x4E9);
    address public relayer = address(0x2E1A);
    uint256[3] internal keys = [uint256(0xA11CE), 0xB0B, 0xCA401];
    address[3] public actors;
    bytes32 internal constant SHARD = bytes32(uint256(5));
    uint64 internal obs;
    bytes32 internal constant FACT = keccak256("fact");

    /// Everything that ever entered the ledger, and everything that left it.
    uint256 public totalIn;
    uint256 public totalOut;
    uint256 public relayerPaid;
    uint256 public startDay;
    /// Set if any attack succeeded.
    bool public broken;
    uint256 internal nonceSeq;
    bytes32 internal usedNonce;
    address internal usedFrom;
    uint128 internal usedValue;
    bytes internal usedSig;

    constructor() {
        cash = new MockUSDC3009Receive();
        MockLedgerView lv = new MockLedgerView();
        for (uint256 i = 0; i < 3; i++) {
            actors[i] = vm.addr(keys[i]);
        }
        rev = new OnyxRevenueV2(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(lv)),
            reporter,
            address(0x7EA5),
            [uint16(5_000), 2_000, 1_000, 2_000]
        );
        obs = rev.OBSERVATION_ROUND();
        vm.prank(reporter);
        rev.registerObservation(FACT);
        OnyxCommit.CommitmentFields memory f;
        f.root = keccak256("final");
        lv.set(f.root, OnyxCommit.bindingHash(f), false);
        lv.addReveal(actors[0], f.root);
        rev.register(1, SHARD, f);
        pre = new OnyxPrepaidV2(ICreditsToken(address(cash)), rev, reporter, relayer, 1 days, 5e6);
        for (uint256 i = 0; i < 3; i++) {
            cash.mint(actors[i], 1e12);
            vm.prank(actors[i]);
            cash.approve(address(pre), type(uint256).max);
        }
        startDay = block.timestamp / 1 days;
    }

    function actorCount() external pure returns (uint256) {
        return 3;
    }

    function deposit(uint256 who, uint128 amount) external {
        address a = actors[who % 3];
        amount = uint128(bound(amount, 1, 1e9));
        vm.prank(a);
        pre.deposit(amount);
        totalIn += amount;
    }

    function gift(uint256 from, uint256 to, uint128 amount) external {
        amount = uint128(bound(amount, 1, 1e9));
        vm.prank(actors[from % 3]);
        pre.depositFor(actors[to % 3], amount, 1);
        totalIn += amount;
    }

    function depositSigned(uint256 who, uint128 amount) external {
        uint256 k = keys[who % 3];
        address from = actors[who % 3];
        amount = uint128(bound(amount, 1, 1e9));
        bytes32 nonce = keccak256(abi.encode("n", nonceSeq++));
        uint256 until = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(k, cash.receiveDigest(from, address(pre), amount, 0, until, nonce));
        bytes memory sig = abi.encodePacked(r, s, v);
        vm.prank(relayer);
        pre.depositWithAuthorization(from, amount, 0, until, nonce, sig);
        totalIn += amount;
        (usedNonce, usedFrom, usedValue, usedSig) = (nonce, from, amount, sig);
    }

    /// A buyer pays the shard book; actor 0 (its miner) moves the earned share in.
    function earn(uint128 fee) external {
        fee = uint128(bound(fee, 10, 1e9));
        address buyer = address(0xB1);
        cash.mint(buyer, fee);
        vm.startPrank(buyer);
        cash.approve(address(rev), fee);
        rev.pay(1, SHARD, fee);
        vm.stopPrank();
        rev.claim(1, SHARD, actors[0]);
        uint256 owed = rev.balanceOf(actors[0]);
        vm.prank(actors[0]);
        rev.withdrawToCredits(IOnyxCredits(address(pre)));
        totalIn += owed;
    }

    function spend(uint256 who, uint128 amount) external {
        uint256 k = keys[who % 3];
        address a = actors[who % 3];
        (uint128 d, uint128 r,) = pre.accounts(a);
        if (d == r) return;
        amount = uint128(bound(amount, 1, d - r));
        uint128 total = r + amount;
        OnyxPrepaidV2.Voucher[] memory vs = new OnyxPrepaidV2.Voucher[](1);
        vs[0] = _voucher(k, a, total);
        vm.prank(reporter);
        pre.redeem(vs);
    }

    function allocate(uint128 amount) external {
        uint256 pool = pre.pool();
        if (pool == 0) return;
        amount = uint128(bound(amount, 1, pool));
        OnyxPrepaidV2.Item[] memory items = new OnyxPrepaidV2.Item[](1);
        items[0] = OnyxPrepaidV2.Item({round: obs, shard: FACT, amount: amount});
        vm.prank(reporter);
        pre.allocate(items);
        totalOut += amount;
    }

    function payRelayer(uint128 amount) external {
        uint256 pool = pre.pool();
        uint256 day = block.timestamp / 1 days;
        uint256 today = day == pre.relayDay() ? pre.relayPaidToday() : 0;
        uint256 room = pre.maxRelayPerDay() - today;
        uint256 most = pool < room ? pool : room;
        if (most == 0) return;
        amount = uint128(bound(amount, 1, most));
        vm.prank(reporter);
        pre.payRelayer(amount);
        totalOut += amount;
        relayerPaid += amount;
    }

    function requestExit(uint256 who) external {
        address a = actors[who % 3];
        (uint128 d, uint128 r,) = pre.accounts(a);
        if (d == r) return;
        vm.prank(a);
        pre.requestExit();
    }

    function withdraw(uint256 who) external {
        address a = actors[who % 3];
        (uint128 d, uint128 r, uint64 ex) = pre.accounts(a);
        if (ex == 0 || block.timestamp < ex) return;
        vm.prank(a);
        pre.withdraw();
        totalOut += d - r;
    }

    function warp(uint32 secs) external {
        vm.warp(block.timestamp + bound(secs, 1, 2 days));
    }

    // ------------------------------------------------------------------ attacks

    /// A voucher signed by another key, or a total at or below what was redeemed.
    function attackVoucher(uint256 who, uint256 signer, uint128 total) external {
        address a = actors[who % 3];
        (uint128 d, uint128 r,) = pre.accounts(a);
        OnyxPrepaidV2.Voucher[] memory vs = new OnyxPrepaidV2.Voucher[](1);
        if (signer % 3 != who % 3) {
            vs[0] = _voucher(keys[signer % 3], a, uint128(bound(total, 1, type(uint128).max)));
        } else {
            vs[0] = _voucher(keys[who % 3], a, uint128(bound(total, 0, r)));
        }
        vm.prank(reporter);
        try pre.redeem(vs) {
            broken = true;
        } catch {}
        // Above the deposit, even when correctly signed.
        if (d < type(uint128).max) {
            vs[0] = _voucher(keys[who % 3], a, d + 1);
            vm.prank(reporter);
            try pre.redeem(vs) {
                broken = true;
            } catch {}
        }
    }

    /// Credit tagged "earned" or "refund" by an ordinary payer.
    function attackOrigin(uint256 who, uint8 origin) external {
        origin = uint8(bound(origin, 2, 255));
        vm.prank(actors[who % 3]);
        try pre.depositFor(actors[who % 3], 1, origin) {
            broken = true;
        } catch {}
    }

    /// The last signed deposit submitted again.
    function attackReplayDeposit() external {
        if (usedFrom == address(0)) return;
        try pre.depositWithAuthorization(
            usedFrom, usedValue, 0, block.timestamp + 1 hours, usedNonce, usedSig
        ) {
            broken = true;
        } catch {}
    }

    /// Relay fees paid by anyone but the reporter, or above the day's cap or the pool.
    function attackRelayer(uint256 who, uint128 amount) external {
        vm.prank(actors[who % 3]);
        try pre.payRelayer(1) {
            broken = true;
        } catch {}
        uint256 day = block.timestamp / 1 days;
        uint256 today = day == pre.relayDay() ? pre.relayPaidToday() : 0;
        uint256 room = pre.maxRelayPerDay() - today;
        uint256 pool = pre.pool();
        uint256 most = pool < room ? pool : room;
        amount = uint128(bound(amount, most + 1, most + 1e9));
        vm.prank(reporter);
        try pre.payRelayer(amount) {
            broken = true;
        } catch {}
    }

    function _voucher(uint256 k, address buyer, uint128 total)
        internal
        view
        returns (OnyxPrepaidV2.Voucher memory)
    {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(k, pre.voucherDigest(buyer, total));
        return OnyxPrepaidV2.Voucher({
            buyer: buyer, cumulative: total, signature: abi.encodePacked(r, s, v)
        });
    }
}

/// Conservation and no-mint for the credits ledger (CL6).
contract CreditsInvariants is StdInvariant, Test {
    CreditsHandler internal h;

    function setUp() public {
        vm.warp(1_790_000_000);
        h = new CreditsHandler();
        targetContract(address(h));
    }

    /// The token held equals every account's unspent balance plus the pool.
    function invariant_BalancesPlusPoolEqualHoldings() public view {
        OnyxPrepaidV2 pre = h.pre();
        uint256 owed = pre.pool();
        for (uint256 i = 0; i < h.actorCount(); i++) {
            (uint128 d, uint128 r,) = pre.accounts(h.actors(i));
            owed += d - r;
        }
        assertEq(h.cash().balanceOf(address(pre)), owed);
    }

    /// No mint: holdings are exactly what arrived less what left (books, relayer, exits).
    function invariant_NoCreditFromNothing() public view {
        assertEq(h.cash().balanceOf(address(h.pre())), h.totalIn() - h.totalOut());
    }

    /// Redeemed never exceeds credited.
    function invariant_RedeemedNeverAboveCredited() public view {
        for (uint256 i = 0; i < h.actorCount(); i++) {
            (uint128 d, uint128 r,) = h.pre().accounts(h.actors(i));
            assertLe(r, d);
        }
    }

    /// The relayer is paid only through `payRelayer`, within the cap of every day elapsed.
    function invariant_RelayerPaidOnlyWithinTheCap() public view {
        uint256 days_ = block.timestamp / 1 days - h.startDay() + 1;
        assertEq(h.cash().balanceOf(h.relayer()), h.relayerPaid());
        assertLe(h.relayerPaid(), uint256(h.pre().maxRelayPerDay()) * days_);
    }

    /// Forged or replayed vouchers, forged tags, replayed deposits and relay-fee abuse never
    /// succeed.
    function invariant_AttacksNeverSucceed() public view {
        assertFalse(h.broken());
    }
}
