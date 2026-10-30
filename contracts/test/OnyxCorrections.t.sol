// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxObservedFacts} from "../src/OnyxObservedFacts.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {OnyxCorrections} from "../src/OnyxCorrections.sol";
import {IStakeToken} from "../src/IOnyxMarkets.sol";
import {CoordinatorLedger} from "./OnyxObservedFacts.t.sol";

/// The registry and the corrections contract wired as `scripts/corrections-anvil.sh` deploys
/// them: a separate registry owner, the test as the registry's coordinator (so it records level-2
/// results itself), a market owner that pairs metrics, and a treasury.
contract CorrectionsBase is Test {
    OnyxRegistry internal reg;
    OnyxTestToken internal tok;
    OnyxCorrections internal cor;

    address internal gov = address(0x60F);
    address internal own = address(0x0E1);
    address internal treasury = address(0x7EA5);
    address internal opener = makeAddr("opener");
    address internal yes1 = makeAddr("yes1");
    address internal yes2 = makeAddr("yes2");
    address internal no1 = makeAddr("no1");
    address internal no2 = makeAddr("no2");

    /// Daily Treasury Statement, total TGA deposits (million USD), and its paired recheck.
    bytes32 internal constant METRIC = keccak256("us.treasury.tga_total_deposits");
    bytes32 internal constant RMETRIC = keccak256("us.treasury.tga_total_deposits.recheck");
    bytes32 internal constant OTHER = keccak256("us.treasury.tga_total_withdrawals");
    bytes32 internal constant TEMPLATE = keccak256("tga template");
    bytes32 internal constant RTEMPLATE = keccak256("tga recheck template");
    bytes32 internal constant OTEMPLATE = keccak256("other template");
    bytes32 internal constant SUBJECT = keccak256("us.treasury.tga_total_deposits:2026-10-28");
    uint64 internal constant WINDOW = 1_793_250_000;
    bytes32 internal constant POOL = keccak256("pool");
    bytes32 internal constant CFG = keccak256("cfg");
    bytes32 internal constant SPEC = keccak256("spec");
    bytes32 internal constant RSPEC = keccak256("recheck spec");
    bytes32 internal constant ROUND = keccak256("round");
    bytes32 internal constant RROUND = keccak256("recheck round");
    int128 internal constant VALUE = 18_463;
    uint128 internal constant TOL = 2;
    uint128 internal constant FEE = 5e18;

    uint64 internal closeB;
    uint64 internal resolveB;

    function _params() internal pure returns (OnyxCorrections.Params memory) {
        return OnyxCorrections.Params({
            openFee: FEE,
            minBet: 1,
            maxBetPerAddress: 100e18,
            maxPoolPerMarket: 10_000e18,
            feePpm: 20_000,
            minTradingBlocks: 10,
            minRecheckBlocks: 20,
            maxMarketBlocks: 1_000,
            graceBlocks: 50
        });
    }

    function setUp() public virtual {
        vm.roll(1_000);
        reg = new OnyxRegistry(gov, IRegistryLedger(address(new CoordinatorLedger(address(this)))));
        vm.startPrank(gov);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.DatedFeed, TEMPLATE, 0);
        reg.registerMetric(RMETRIC, OnyxRegistry.Kind.DatedFeed, RTEMPLATE, 0);
        reg.registerMetric(OTHER, OnyxRegistry.Kind.DatedFeed, OTEMPLATE, 0);
        vm.stopPrank();
        reg.setReceiptSigner(address(0x5EC));
        tok = new OnyxTestToken(address(this));
        vm.prank(own);
        cor = new OnyxCorrections(IStakeToken(address(tok)), reg, treasury, _params());
        vm.prank(own);
        cor.pair(METRIC, RMETRIC, TOL);
        _record(METRIC, SPEC, ROUND, VALUE);
    }

    /// One announced draw, its beacon and the result: the coordinator's level-2 round.
    function _announce(bytes32 metric) internal returns (bytes32 fact, uint32 n) {
        fact = reg.factKey(metric, SUBJECT, WINDOW);
        n = reg.drawCount(fact);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        reg.announceDraw(metric, SUBJECT, WINDOW, n, reg.drawCommit(POOL, SPEC, CFG), h);
        vm.roll(h + 1);
        vm.setBlockhash(h, keccak256(abi.encode("beacon", fact, n)));
        reg.fixBeacon(fact, n);
    }

    function _recordAnnounced(bytes32 metric, bytes32 spec, bytes32 round, int128 value) internal {
        // The draw committed to SPEC; record under it, the result hashed with `spec`.
        reg.recordObservation(
            metric, SUBJECT, WINDOW, SPEC, cor.resultHash(spec, round, value), POOL, CFG
        );
    }

    function _record(bytes32 metric, bytes32 spec, bytes32 round, int128 value) internal {
        _announce(metric);
        _recordAnnounced(metric, spec, round, value);
    }

    function _fund(address who, uint256 a) internal {
        tok.mint(who, a);
        vm.prank(who);
        tok.approve(address(cor), type(uint256).max);
    }

    function _open() internal returns (uint256 id) {
        uint64 b = uint64(vm.getBlockNumber());
        closeB = b + 100;
        resolveB = closeB + 200;
        _fund(opener, FEE);
        vm.prank(opener);
        id = cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, closeB, resolveB);
    }

    function _bet(uint256 id, address who, uint8 side, uint128 amount) internal {
        _fund(who, amount);
        vm.prank(who);
        cor.bet(id, side, amount);
    }

    /// The recheck round after the close: its value is `rv` (the spec the draw committed to is
    /// SPEC in these tests; the recheck result hashes with it).
    function _recheck(int128 rv) internal {
        if (vm.getBlockNumber() <= closeB) vm.roll(closeB + 1);
        _record(RMETRIC, SPEC, RROUND, rv);
    }

    function _withdraw(uint256 id, address who) internal returns (uint256 got) {
        uint256 b = tok.balanceOf(who);
        vm.prank(who);
        cor.withdraw(id);
        got = tok.balanceOf(who) - b;
    }

    function _solvent() internal view {
        assertEq(tok.balanceOf(address(cor)), cor.liabilities(), "balance == liabilities");
    }
}

/// CL7 (ADR-0057): correction markets settled only by a later recheck round.
contract OnyxCorrectionsTest is CorrectionsBase {
    // ------------------------------------------------------------------ settlement

    function test_ARecheckThatDisagreesPaysNo() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 30e18);
        _bet(id, yes2, 1, 10e18);
        _bet(id, no1, 2, 15e18);
        _bet(id, no2, 2, 5e18);
        _recheck(VALUE + 3); // beyond the tolerance of 2: the source now says otherwise
        cor.resolve(id, RROUND, VALUE + 3);
        OnyxCorrections.Market memory m = cor.market(id);
        assertEq(uint8(m.state), uint8(OnyxCorrections.State.Resolved));
        assertEq(m.winner, 2);
        // Losers 40e18, fee 2% = 0.8e18, distributable 39.2e18 over a No pool of 20e18.
        assertEq(_withdraw(id, no1), 15e18 + uint256(15e18) * 39.2e18 / 20e18);
        assertEq(_withdraw(id, no2), 5e18 + uint256(5e18) * 39.2e18 / 20e18);
        vm.prank(yes1);
        vm.expectRevert(OnyxCorrections.NothingOwed.selector);
        cor.withdraw(id);
        // The fee and the open fee went to the treasury.
        cor.payTreasury();
        assertEq(tok.balanceOf(treasury), 0.8e18 + FEE);
        assertEq(tok.balanceOf(address(cor)), 0);
        _solvent();
    }

    function test_ARecheckThatAgreesPaysYes() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 30e18);
        _bet(id, no1, 2, 10e18);
        _recheck(VALUE - 2); // within the tolerance: accurate
        cor.resolve(id, RROUND, VALUE - 2);
        assertEq(cor.market(id).winner, 1);
        assertEq(_withdraw(id, yes1), 30e18 + 9.8e18);
        _solvent();
    }

    function test_ResolveRefusesAValueTheRecheckDidNotRecord() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 1e18);
        _recheck(VALUE + 10);
        vm.expectRevert(OnyxCorrections.WrongValue.selector);
        cor.resolve(id, RROUND, VALUE);
        vm.expectRevert(OnyxCorrections.WrongValue.selector);
        cor.resolve(id, ROUND, VALUE + 10);
    }

    /// The original round never settles its own market: no recheck result, nothing to resolve.
    function test_TheOriginalRoundNeverSettles() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 1e18);
        vm.roll(closeB + 5);
        vm.expectRevert(OnyxCorrections.BadRecheck.selector);
        cor.resolve(id, ROUND, VALUE);
    }

    /// Only the paired `.recheck` metric settles: a later result of another metric for the same
    /// subject (another template) changes nothing, and a pair is never changed.
    function test_OnlyThePairedRecheckSettles() public {
        uint256 id = _open();
        _bet(id, no1, 2, 1e18);
        vm.roll(closeB + 1);
        _record(OTHER, SPEC, RROUND, VALUE + 100);
        vm.expectRevert(OnyxCorrections.BadRecheck.selector);
        cor.resolve(id, RROUND, VALUE + 100);
        vm.prank(own);
        vm.expectRevert(OnyxCorrections.BadPair.selector);
        cor.pair(METRIC, OTHER, 1_000);
        // Nor can another metric take the paired recheck metric.
        vm.prank(own);
        vm.expectRevert(OnyxCorrections.BadPair.selector);
        cor.pair(OTHER, RMETRIC, 1_000);
    }

    function test_PairRefusesNonDatedOrUnregisteredMetrics() public {
        bytes32 feed = keccak256("fixed window feed");
        vm.prank(gov);
        reg.registerMetric(feed, OnyxRegistry.Kind.Feed, keccak256("t"), 0);
        vm.startPrank(own);
        vm.expectRevert(OnyxCorrections.BadPair.selector);
        cor.pair(feed, keccak256("x.recheck"), 1);
        vm.expectRevert(OnyxCorrections.BadPair.selector);
        cor.pair(OTHER, keccak256("never registered"), 1);
        vm.expectRevert(OnyxCorrections.BadPair.selector);
        cor.pair(OTHER, OTHER, 1);
        // No recheck of a recheck in v1.
        vm.expectRevert(OnyxCorrections.BadPair.selector);
        cor.pair(RMETRIC, OTHER, 1);
        vm.stopPrank();
        vm.expectRevert(OnyxCorrections.NotOwner.selector);
        cor.pair(OTHER, feed, 1);
    }

    /// THREATS CL7-4: the opener passes no tolerance; the market carries the pair's.
    function test_TheToleranceIsThePairsNotTheOpeners() public {
        uint256 id = _open();
        assertEq(cor.market(id).tolerance, TOL);
        _bet(id, yes1, 1, 1e18);
        _bet(id, no1, 2, 1e18);
        _recheck(VALUE + int128(TOL));
        cor.resolve(id, RROUND, VALUE + int128(TOL));
        assertEq(cor.market(id).winner, 1, "exactly the tolerance away is accurate");
    }

    // ------------------------------------------------------------------ opening

    /// THREATS CL7-1.
    function test_OpenRefusesAValueTheRegistryDidNotRecord() public {
        _fund(opener, FEE);
        uint64 b = uint64(vm.getBlockNumber());
        vm.startPrank(opener);
        vm.expectRevert(OnyxCorrections.WrongValue.selector);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE + 1, b + 100, b + 300);
        vm.expectRevert(OnyxCorrections.WrongValue.selector);
        cor.open(METRIC, SUBJECT, WINDOW, RROUND, VALUE, b + 100, b + 300);
        vm.expectRevert(OnyxCorrections.NoResult.selector);
        cor.open(METRIC, keccak256("another day"), WINDOW, ROUND, VALUE, b + 100, b + 300);
        vm.expectRevert(OnyxCorrections.NotPaired.selector);
        cor.open(OTHER, SUBJECT, WINDOW, ROUND, VALUE, b + 100, b + 300);
        vm.stopPrank();
    }

    /// Review CL7-R1-1 (THREATS CL7-5, CL7-12): several markets of one value may run, so a
    /// squatter's market with a hopeless close blocks nobody; one recheck settles every market
    /// it is timely for, and voids (with the fee refunded) the one it was drawn too early for.
    function test_SeveralMarketsOfOneValueSettleFromOneRecheck() public {
        uint256 b = vm.getBlockNumber();
        _fund(opener, 3 * FEE);
        vm.startPrank(opener);
        uint256 squat =
            cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, uint64(b + 980), uint64(b + 1_000));
        uint256 a = cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, uint64(b + 20), uint64(b + 300));
        uint256 c = cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, uint64(b + 40), uint64(b + 300));
        vm.stopPrank();
        _bet(a, yes1, 1, 2e18);
        _bet(a, no1, 2, 2e18);
        _bet(c, yes2, 1, 3e18);
        _bet(c, no2, 2, 1e18);
        _bet(squat, yes1, 1, 1e18);
        vm.roll(b + 41);
        _record(RMETRIC, SPEC, RROUND, VALUE + 9);
        cor.resolve(a, RROUND, VALUE + 9);
        cor.resolve(c, RROUND, VALUE + 9);
        assertEq(cor.market(a).winner, 2);
        assertEq(cor.market(c).winner, 2);
        // The squat closes after the recheck's draw: it can never settle, so it voids at once
        // and its opener gets the fee back (the recheck was not drawn for it).
        // (Before its close it cannot even be resolved; after it, BadRecheck.)
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.resolve(squat, RROUND, VALUE + 9);
        cor.void(squat);
        assertEq(cor.openerOwed(opener), FEE);
        assertEq(cor.treasuryOwed(), 2 * FEE + 0.04e18 + 0.06e18);
        assertEq(_withdraw(squat, yes1), 1e18);
        _solvent();
    }

    /// Review CL7-R1-2: a draw announced while trading is open voids nothing by itself and
    /// burns nothing: a later draw after the close can still settle the market (the recording
    /// draw is what must come after the close). A market can open while such a draw stands.
    function test_ADrawBeforeTheCloseThatRecordsNothingDoesNotBurnTheValue() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 2e18);
        _bet(id, no1, 2, 2e18);
        _announce(RMETRIC); // the coordinator's early draw, never recorded
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.void(id);
        _fund(opener, FEE);
        uint64 b = uint64(vm.getBlockNumber());
        vm.prank(opener);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 100, b + 300);
        vm.roll(closeB + 1);
        _record(RMETRIC, SPEC, RROUND, VALUE); // attempt 1, drawn after the close
        cor.resolve(id, RROUND, VALUE);
        assertEq(cor.market(id).winner, 1);
    }

    /// Review CL7-R1-2/4: a recheck recorded from a draw announced before the close voids the
    /// market at once and refunds the opener's fee: the timing was the coordinator's.
    function test_AnEarlyRecheckRefundsTheOpenFee() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 10e18);
        _record(RMETRIC, SPEC, RROUND, VALUE + 1);
        cor.void(id);
        assertEq(cor.openerOwed(opener), FEE);
        assertEq(cor.treasuryOwed(), 0);
        vm.prank(opener);
        cor.claimFeeRefund();
        assertEq(tok.balanceOf(opener), FEE);
        _solvent();
    }

    /// Review CL7-R1-3: each side holds at most half the pool cap, so a whale cannot fill one
    /// side and shut the other out at no risk.
    function test_OneSideCannotFillTheWholePool() public {
        uint256 id = _open();
        for (uint256 i = 0; i < 50; i++) {
            _bet(id, address(uint160(0xA000 + i)), 1, 100e18);
        }
        _fund(yes1, 1);
        vm.prank(yes1);
        vm.expectRevert(OnyxCorrections.CapReached.selector);
        cor.bet(id, 1, 1);
        _bet(id, no1, 2, 100e18);
        assertEq(cor.market(id).pools[1], 100e18);
    }

    /// A market opened after the recheck was recorded is refused: its value is known (CL7-2,
    /// CL7-10).
    function test_OpenRefusesAValueWhoseRecheckStarted() public {
        _announce(RMETRIC);
        _fund(opener, FEE);
        uint64 b = uint64(vm.getBlockNumber());
        _recordAnnounced(RMETRIC, SPEC, RROUND, VALUE);
        vm.prank(opener);
        vm.expectRevert(OnyxCorrections.RecheckStarted.selector);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 100, b + 300);
    }

    /// THREATS CL7-12 (tiny markets): trading and the recheck get their minimum blocks, and no
    /// market outlives `maxMarketBlocks`.
    function test_OpenRefusesTooShortOrTooLongMarkets() public {
        _fund(opener, 3 * FEE);
        uint64 b = uint64(vm.getBlockNumber());
        vm.startPrank(opener);
        vm.expectRevert(OnyxCorrections.BadMarket.selector);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 9, b + 300);
        vm.expectRevert(OnyxCorrections.BadMarket.selector);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 100, b + 119);
        vm.expectRevert(OnyxCorrections.BadMarket.selector);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 100, b + 1_001);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 10, b + 30);
        vm.stopPrank();
    }

    function test_OpenTakesTheOpenFee() public {
        uint256 id = _open();
        assertEq(tok.balanceOf(address(cor)), FEE);
        assertEq(cor.feeEscrow(), FEE);
        assertEq(cor.market(id).opener, opener);
        _solvent();
    }

    // ------------------------------------------------------------------ timing of the recheck

    /// THREATS CL7-2: a recheck whose draw was announced before the close voids at once.
    function test_ARecheckRecordedBeforeTheCloseVoids() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 10e18);
        _bet(id, no1, 2, 10e18);
        _record(RMETRIC, SPEC, RROUND, VALUE + 50);
        vm.roll(closeB + 1);
        vm.expectRevert(OnyxCorrections.BadRecheck.selector);
        cor.resolve(id, RROUND, VALUE + 50);
        cor.void(id);
        assertEq(_withdraw(id, yes1), 10e18);
        assertEq(_withdraw(id, no1), 10e18);
        // The round was drawn before the close (the coordinator's timing): the fee goes back.
        assertEq(cor.openerOwed(opener), FEE);
        assertEq(cor.treasuryOwed(), 0);
        _solvent();
    }

    /// THREATS CL7-10 (front-running the recheck): a draw announced before the close, recorded
    /// after it, still voids: its committee was public while trading was open.
    function test_ARecheckDrawnBeforeTheCloseVoidsEvenIfRecordedAfter() public {
        uint256 id = _open();
        _bet(id, no1, 2, 10e18);
        vm.roll(closeB - 5);
        _announce(RMETRIC);
        vm.roll(closeB + 1);
        _recordAnnounced(RMETRIC, SPEC, RROUND, VALUE + 50);
        vm.expectRevert(OnyxCorrections.BadRecheck.selector);
        cor.resolve(id, RROUND, VALUE + 50);
        cor.void(id);
        assertEq(uint8(cor.market(id).state), uint8(OnyxCorrections.State.Void));
    }

    /// A draw announced at the close block itself is not after the close.
    function test_ARecheckDrawnAtTheCloseBlockVoids() public {
        uint256 id = _open();
        _bet(id, no1, 2, 10e18);
        vm.roll(closeB);
        _announce(RMETRIC);
        _recordAnnounced(RMETRIC, SPEC, RROUND, VALUE + 50);
        vm.expectRevert(OnyxCorrections.BadRecheck.selector);
        cor.resolve(id, RROUND, VALUE + 50);
        cor.void(id);
    }

    function test_ARecheckRecordedAfterResolveBlockVoids() public {
        uint256 id = _open();
        _bet(id, no1, 2, 10e18);
        vm.roll(resolveB - 1);
        _announce(RMETRIC); // announced after close, recorded after resolveBlock
        vm.roll(resolveB + 1);
        _recordAnnounced(RMETRIC, SPEC, RROUND, VALUE + 50);
        vm.expectRevert(OnyxCorrections.BadRecheck.selector);
        cor.resolve(id, RROUND, VALUE + 50);
        cor.void(id);
        assertEq(_withdraw(id, no1), 10e18);
        _solvent();
    }

    /// Refunds when no recheck arrives by the deadline; the open fee too when nobody tried a
    /// round (THREATS CL7-9).
    function test_RefundsWhenNoRecheckArrivesByTheDeadline() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 7e18);
        _bet(id, no1, 2, 3e18);
        vm.roll(resolveB);
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.void(id);
        vm.roll(resolveB + 1);
        cor.void(id);
        assertEq(_withdraw(id, yes1), 7e18);
        assertEq(_withdraw(id, no1), 3e18);
        assertEq(cor.openerOwed(opener), FEE);
        vm.prank(opener);
        cor.claimFeeRefund();
        assertEq(tok.balanceOf(opener), FEE);
        assertEq(tok.balanceOf(address(cor)), 0);
        _solvent();
    }

    /// A recheck round that was drawn but recorded nothing keeps the fee (it paid for the round).
    function test_AFailedRecheckRoundKeepsTheFee() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 7e18);
        vm.roll(closeB + 1);
        _announce(RMETRIC);
        vm.roll(resolveB + 1);
        cor.void(id);
        assertEq(cor.openerOwed(opener), 0);
        assertEq(cor.treasuryOwed(), FEE);
        assertEq(_withdraw(id, yes1), 7e18);
        _solvent();
    }

    /// A timely recheck cannot be voided away by the losing side during the grace blocks; only
    /// after them (e.g. a recheck that settled no number).
    function test_ATimelyRecheckIsNotVoidedDuringTheGrace() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 7e18);
        _bet(id, no1, 2, 7e18);
        _recheck(VALUE + 99);
        vm.roll(resolveB + 1);
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.void(id);
        vm.roll(uint256(resolveB) + 50);
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.void(id);
        cor.resolve(id, RROUND, VALUE + 99);
        assertEq(cor.market(id).winner, 2);
    }

    function test_AfterTheGraceAnUnresolvedMarketVoids() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 7e18);
        _recheck(VALUE); // recorded, but nobody resolves (or it was no number)
        vm.roll(uint256(resolveB) + 51);
        cor.void(id);
        assertEq(cor.treasuryOwed(), FEE, "a round ran");
        assertEq(_withdraw(id, yes1), 7e18);
    }

    // ------------------------------------------------------------------ once only, no admin

    function test_NoDoubleSettle() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 5e18);
        _bet(id, no1, 2, 5e18);
        _recheck(VALUE + 9);
        cor.resolve(id, RROUND, VALUE + 9);
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.resolve(id, RROUND, VALUE + 9);
        vm.roll(uint256(resolveB) + 500);
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.void(id);
        _withdraw(id, no1);
        vm.prank(no1);
        vm.expectRevert(OnyxCorrections.NothingOwed.selector);
        cor.withdraw(id);
        // A settled value takes no second market: its recheck fact is recorded.
        _fund(opener, FEE);
        uint64 b = uint64(vm.getBlockNumber());
        vm.prank(opener);
        vm.expectRevert(OnyxCorrections.RecheckStarted.selector);
        cor.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 100, b + 300);
    }

    /// A market voided before any recheck frees its value for a new market (ADR-0057
    /// amendment 1): a griefer's market cannot burn the value's only correction market.
    function test_AVoidMarketWithoutARecheckFreesTheValue() public {
        uint256 id = _open();
        vm.roll(resolveB + 1);
        cor.void(id);
        uint256 id2 = _open();
        assertEq(id2, id + 1);
        assertEq(cor.marketOf(reg.factKey(METRIC, SUBJECT, WINDOW)), id2);
    }

    /// There is no admin outcome: the owner, the coordinator and the treasury may not bet, and
    /// none of them can resolve or void outside the rules anyone else follows.
    function test_NoAdminPath() public {
        uint256 id = _open();
        _fund(own, 1e18);
        _fund(address(this), 1e18);
        _fund(treasury, 1e18);
        vm.prank(own);
        vm.expectRevert(OnyxCorrections.NotAllowed.selector);
        cor.bet(id, 1, 1e18);
        vm.expectRevert(OnyxCorrections.NotAllowed.selector);
        cor.bet(id, 2, 1e18);
        vm.prank(treasury);
        vm.expectRevert(OnyxCorrections.NotAllowed.selector);
        cor.bet(id, 2, 1e18);
        _bet(id, yes1, 1, 1e18);
        vm.roll(closeB + 1);
        vm.startPrank(own);
        vm.expectRevert(OnyxCorrections.BadRecheck.selector);
        cor.resolve(id, ROUND, VALUE);
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.void(id);
        vm.stopPrank();
        assertFalse(cor.mayBet(own));
        assertFalse(cor.mayBet(address(this)));
        assertFalse(cor.mayBet(treasury));
        assertTrue(cor.mayBet(opener));
    }

    function test_BetsCloseAtTheCloseBlock() public {
        uint256 id = _open();
        vm.roll(closeB);
        _fund(yes1, 1e18);
        vm.prank(yes1);
        vm.expectRevert(OnyxCorrections.WrongPhase.selector);
        cor.bet(id, 1, 1e18);
    }

    function test_BetLimits() public {
        uint256 id = _open();
        _fund(yes1, 200e18);
        vm.startPrank(yes1);
        vm.expectRevert(OnyxCorrections.BadMarket.selector);
        cor.bet(id, 3, 1e18);
        vm.expectRevert(OnyxCorrections.BadAmount.selector);
        cor.bet(id, 1, 0);
        vm.expectRevert(OnyxCorrections.BadAmount.selector);
        cor.bet(id, 1, 100e18 + 1);
        cor.bet(id, 1, 60e18);
        vm.expectRevert(OnyxCorrections.BadAmount.selector);
        cor.bet(id, 2, 40e18 + 1);
        vm.stopPrank();
    }

    /// The winning side had no bet: every bet is refunded (the round ran: the fee is kept).
    function test_AnEmptyWinningSideRefundsEveryBet() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 4e18);
        _recheck(VALUE + 99);
        cor.resolve(id, RROUND, VALUE + 99);
        OnyxCorrections.Market memory m = cor.market(id);
        assertEq(uint8(m.state), uint8(OnyxCorrections.State.Void));
        assertEq(m.voidReason, 1);
        assertEq(_withdraw(id, yes1), 4e18);
        assertEq(cor.treasuryOwed(), FEE);
        _solvent();
    }

    /// Dust from rounding goes to the treasury after the last winner, never to a trader.
    function test_RoundingDustGoesToTheTreasury() public {
        uint256 id = _open();
        _bet(id, yes1, 1, 3);
        _bet(id, yes2, 1, 3);
        _bet(id, no1, 1, 1);
        _bet(id, no2, 2, 10);
        _recheck(VALUE);
        cor.resolve(id, RROUND, VALUE);
        // losers 10, fee 0, distributable 10 over a Yes pool of 7: 3+4, 3+4, 1+1 = 16; dust 1.
        assertEq(_withdraw(id, yes1), 7);
        assertEq(_withdraw(id, yes2), 7);
        assertEq(_withdraw(id, no1), 2);
        assertEq(cor.treasuryOwed(), FEE + 1);
        assertEq(cor.held(), 0);
        _solvent();
    }

    /// The contract's `resultHash` is the shared Rust rule (fixtures/markets/observed-facts.json).
    function test_ResultHashMatchesTheRustVectors() public view {
        string memory json = vm.readFile("../fixtures/markets/observed-facts.json");
        bytes32[] memory specs = vm.parseJsonBytes32Array(json, ".specs");
        bytes32[] memory rounds = vm.parseJsonBytes32Array(json, ".rounds");
        string[] memory values = vm.parseJsonStringArray(json, ".values");
        bytes32[] memory results = vm.parseJsonBytes32Array(json, ".results");
        assertGt(specs.length, 6);
        for (uint256 i = 0; i < specs.length; i++) {
            int128 v = int128(vm.parseInt(values[i]));
            assertEq(cor.resultHash(specs[i], rounds[i], v), results[i], values[i]);
        }
    }

    function test_ConstructorRefusesBadParams() public {
        OnyxCorrections.Params memory p = _params();
        p.feePpm = 100_001;
        vm.expectRevert(OnyxCorrections.BadParams.selector);
        new OnyxCorrections(IStakeToken(address(tok)), reg, treasury, p);
        p = _params();
        p.maxMarketBlocks = 29;
        vm.expectRevert(OnyxCorrections.BadParams.selector);
        new OnyxCorrections(IStakeToken(address(tok)), reg, treasury, p);
        p = _params();
        p.maxBetPerAddress = 5_000e18 + 1; // above half the pool cap
        vm.expectRevert(OnyxCorrections.BadParams.selector);
        new OnyxCorrections(IStakeToken(address(tok)), reg, treasury, p);
        p = _params();
        p.graceBlocks = 0;
        vm.expectRevert(OnyxCorrections.BadParams.selector);
        new OnyxCorrections(IStakeToken(address(tok)), reg, treasury, p);
    }

    // ------------------------------------------------------------------ fuzz

    /// Yes wins exactly when the recheck is within the pair's tolerance, at any magnitude
    /// (no overflow at the int128 edges).
    function testFuzz_TheToleranceDecides(int128 rv) public {
        uint256 id = _open();
        _bet(id, yes1, 1, 1e18);
        _bet(id, no1, 2, 1e18);
        _recheck(rv);
        cor.resolve(id, RROUND, rv);
        int256 d = int256(rv) - int256(VALUE);
        bool accurate = d >= -int256(uint256(TOL)) && d <= int256(uint256(TOL));
        assertEq(cor.market(id).winner, accurate ? 1 : 2);
    }

    /// Payout conservation and rounding: every winner gets at most its exact pro rata share and
    /// less than one wei below it; bets in == payouts + fee + dust, to the wei.
    function testFuzz_PayoutsConserveAndRoundDown(
        uint128[4] memory stakes,
        uint8 sides,
        bool accurate
    ) public {
        uint256 id = _open();
        address[4] memory who = [yes1, yes2, no1, no2];
        uint256 total;
        for (uint256 i = 0; i < 4; i++) {
            stakes[i] = uint128(bound(stakes[i], 1, 100e18));
            _bet(id, who[i], uint8(((sides >> i) & 1) + 1), stakes[i]);
            total += stakes[i];
        }
        int128 rv = accurate ? VALUE : VALUE + 1_000;
        _recheck(rv);
        cor.resolve(id, RROUND, rv);
        OnyxCorrections.Market memory m = cor.market(id);
        uint256 paidOut;
        for (uint256 i = 0; i < 4; i++) {
            uint256 o = cor.owed(id, who[i]);
            if (o == 0) continue;
            (uint128 y, uint128 n) = cor.betsOf(id, who[i]);
            if (m.state == OnyxCorrections.State.Resolved) {
                uint256 s = m.winner == 1 ? y : n;
                uint256 exactNum = s * (uint256(m.winningPool) + m.distributable);
                assertLe(o * m.winningPool, exactNum, "never above the exact share");
                assertGt((o + 1) * m.winningPool, exactNum, "less than one wei below it");
            } else {
                assertEq(o, uint256(y) + n, "a void market refunds the bet");
            }
            paidOut += _withdraw(id, who[i]);
        }
        uint256 fee = m.state == OnyxCorrections.State.Resolved
            ? uint256(m.total) - m.winningPool - m.distributable
            : 0;
        assertEq(paidOut + (cor.treasuryOwed() - FEE), total, "bets in == out + fee + dust");
        assertLe(cor.treasuryOwed() - FEE - fee, 4, "dust below one wei per winner");
        assertEq(cor.held(), 0);
        _solvent();
    }
}

/// A token whose transfer calls back into `withdraw`, as a hostile token or a hook would.
contract ReenteringToken {
    mapping(address => uint256) public balanceOf;
    OnyxCorrections internal target;
    uint256 internal id;
    bool internal armed;

    function mint(address to, uint256 a) external {
        balanceOf[to] += a;
    }

    function arm(OnyxCorrections t, uint256 id_) external {
        (target, id, armed) = (t, id_, true);
    }

    function disarm() external {
        armed = false;
    }

    function transfer(address to, uint256 a) external returns (bool) {
        balanceOf[msg.sender] -= a;
        balanceOf[to] += a;
        if (armed) {
            armed = false;
            target.withdraw(id);
        }
        return true;
    }

    function transferFrom(address from, address to, uint256 a) external returns (bool) {
        balanceOf[from] -= a;
        balanceOf[to] += a;
        return true;
    }
}

contract OnyxCorrectionsReentrancyTest is CorrectionsBase {
    function test_WithdrawCannotBeReentered() public {
        ReenteringToken bad = new ReenteringToken();
        vm.prank(own);
        OnyxCorrections c = new OnyxCorrections(IStakeToken(address(bad)), reg, treasury, _params());
        vm.prank(own);
        c.pair(METRIC, RMETRIC, TOL);
        uint64 b = uint64(vm.getBlockNumber());
        bad.mint(opener, FEE);
        vm.prank(opener);
        uint256 id = c.open(METRIC, SUBJECT, WINDOW, ROUND, VALUE, b + 100, b + 300);
        bad.mint(yes1, 10e18);
        vm.prank(yes1);
        c.bet(id, 1, 10e18);
        bad.mint(yes2, 10e18);
        vm.prank(yes2);
        c.bet(id, 1, 10e18);
        vm.roll(b + 300 + 1);
        c.void(id);
        bad.arm(c, id);
        vm.expectRevert(OnyxCorrections.Reentrancy.selector);
        vm.prank(yes1);
        c.withdraw(id);
        // Unarmed, the same withdrawal pays once.
        bad.disarm();
        vm.prank(yes1);
        c.withdraw(id);
        assertEq(bad.balanceOf(yes1), 10e18);
    }
}
