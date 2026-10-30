// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, console} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxRegistry, IRegistryLedger} from "../../src/OnyxRegistry.sol";
import {OnyxTestToken} from "../../src/OnyxTestToken.sol";
import {OnyxCorrections} from "../../src/OnyxCorrections.sol";
import {IStakeToken} from "../../src/IOnyxMarkets.sol";
import {CoordinatorLedger} from "../OnyxObservedFacts.t.sol";

/// Random opens, bets, recheck rounds (before, at and after the close; recorded late or never),
/// resolutions, voids, withdrawals and fee claims over a few subjects and actors (CL7, ADR-0057).
/// The handler is the registry's coordinator, so it records the original and recheck results.
contract CorrectionsHandler is Test {
    OnyxRegistry public reg;
    OnyxTestToken public tok;
    OnyxCorrections public cor;

    bytes32 public constant METRIC = keccak256("m");
    bytes32 public constant RMETRIC = keccak256("m.recheck");
    bytes32 public constant SPEC = keccak256("spec");
    bytes32 public constant ROUND = keccak256("round");
    bytes32 public constant RROUND = keccak256("recheck round");
    bytes32 internal constant POOL = keccak256("pool");
    bytes32 internal constant CFG = keccak256("cfg");
    uint64 public constant WINDOW = 1_800_000_000;
    uint128 public constant TOL = 5;
    address public constant OWNER = address(0x0E1);
    address public constant TREASURY = address(0x7EA5);

    address[5] internal actors;
    bytes32[12] public subjects;
    int128[12] public values;
    /// The recheck value recorded for a subject (valid only once recorded).
    mapping(bytes32 => int128) public recheckValueOf;
    /// Tokens paid out of each market by `withdraw`.
    mapping(uint256 => uint256) public paidOut;
    uint256 public resolvedCount;
    /// The handler's own chain height: a `vm.roll` inside one fuzz call does not carry over to
    /// the next, so every call starts from it and stores where it ended.
    uint256 public blk;

    modifier atBlock() {
        vm.roll(blk);
        _;
        blk = vm.getBlockNumber();
    }
    uint256 public voidCount;
    /// Voids of a market whose recheck had a draw (early, failed or late), and voids left to the
    /// end-of-run drain.
    uint256 public voidDrawn;
    uint256 public finishVoids;

    constructor() {
        address gov = address(0x60F);
        reg = new OnyxRegistry(gov, IRegistryLedger(address(new CoordinatorLedger(address(this)))));
        vm.startPrank(gov);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.DatedFeed, keccak256("t"), 0);
        reg.registerMetric(RMETRIC, OnyxRegistry.Kind.DatedFeed, keccak256("rt"), 0);
        vm.stopPrank();
        reg.setReceiptSigner(address(0x5EC));
        tok = new OnyxTestToken(address(this));
        vm.prank(OWNER);
        cor = new OnyxCorrections(
            IStakeToken(address(tok)),
            reg,
            TREASURY,
            OnyxCorrections.Params({
                openFee: 3e18,
                minBet: 1,
                maxBetPerAddress: 100e18,
                maxPoolPerMarket: 300e18,
                feePpm: 20_000,
                minTradingBlocks: 5,
                minRecheckBlocks: 10,
                maxMarketBlocks: 400,
                graceBlocks: 600
            })
        );
        vm.prank(OWNER);
        cor.pair(METRIC, RMETRIC, TOL);
        for (uint256 i = 0; i < actors.length; i++) {
            actors[i] = address(uint160(0xA0 + i));
            vm.prank(actors[i]);
            tok.approve(address(cor), type(uint256).max);
        }
        for (uint256 i = 0; i < 12; i++) {
            subjects[i] = keccak256(abi.encode("subject", i));
            values[i] = int128(int256(i) * 1_000 - 1_500);
            _record(METRIC, subjects[i], ROUND, values[i]);
        }
        blk = vm.getBlockNumber();
    }

    function _actor(uint256 s) internal view returns (address) {
        return actors[s % actors.length];
    }

    function _announce(bytes32 metric, bytes32 subject) internal returns (bool) {
        bytes32 fact = reg.factKey(metric, subject, WINDOW);
        uint32 n = reg.drawCount(fact);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        try reg.announceDraw(metric, subject, WINDOW, n, reg.drawCommit(POOL, SPEC, CFG), h) {}
        catch {
            return false;
        }
        vm.roll(h + 1);
        vm.setBlockhash(h, keccak256(abi.encode("beacon", fact, n)));
        reg.fixBeacon(fact, n);
        return true;
    }

    function _record(bytes32 metric, bytes32 subject, bytes32 round, int128 v) internal {
        if (!_announce(metric, subject)) return;
        reg.recordObservation(
            metric, subject, WINDOW, SPEC, cor.resultHash(SPEC, round, v), POOL, CFG
        );
    }

    function open(uint256 who, uint256 s, uint256 tradeFor, uint256 recheckFor) external atBlock {
        uint256 b = vm.getBlockNumber();
        uint64 closeB = uint64(b + bound(tradeFor, 5, 120));
        uint64 resolveB = uint64(closeB + bound(recheckFor, 10, 270));
        address a = _actor(who);
        tok.mint(a, 3e18);
        vm.prank(a);
        try cor.open(METRIC, subjects[s % 12], WINDOW, ROUND, values[s % 12], closeB, resolveB) {}
            catch {}
    }

    function bet(uint256 who, uint256 id, uint8 side, uint128 amount) external atBlock {
        if (cor.marketCount() == 0) return;
        id = bound(id, 1, cor.marketCount());
        amount = uint128(bound(amount, 1, 60e18));
        address a = _actor(who);
        tok.mint(a, amount);
        vm.prank(a);
        try cor.bet(id, uint8(bound(side, 1, 2)), amount) {} catch {}
    }

    /// The recheck round of a market's subject: mode 0 after the close (timely), 1 right now
    /// (possibly before the close), 2 after `resolveBlock` (late).
    function recheck(uint256 id, int128 delta, uint8 mode) external atBlock {
        if (cor.marketCount() == 0) return;
        id = _openFrom(bound(id, 1, cor.marketCount()));
        OnyxCorrections.Market memory m = cor.market(id);
        if (reg.observationOf(RMETRIC, m.subjectKey, WINDOW).result != bytes32(0)) return;
        mode = uint8(bound(mode, 0, 5));
        uint256 b = vm.getBlockNumber();
        // Timely modes only run while a timely recording is still possible (draw + 2 blocks).
        if (mode <= 3 && b + 3 > m.resolveBlock) return;
        if (mode <= 3 && b <= m.closeBlock) vm.roll(uint256(m.closeBlock) + 1);
        if (mode == 5 && b <= m.resolveBlock) vm.roll(uint256(m.resolveBlock) + 1);
        int128 v = m.value + int128(bound(delta, -20, 20));
        recheckValueOf[m.subjectKey] = v;
        _record(RMETRIC, m.subjectKey, RROUND, v);
    }

    /// A recheck draw that records nothing (a failed round), now.
    function failedDraw(uint256 id, uint8 often) external atBlock {
        // Rarer than a recheck: most markets should reach their recheck round.
        if (cor.marketCount() == 0 || often % 4 != 0) return;
        bytes32 subject = cor.market(bound(id, 1, cor.marketCount())).subjectKey;
        if (reg.observationOf(RMETRIC, subject, WINDOW).result != bytes32(0)) return;
        _announce(RMETRIC, subject);
    }

    function roll(uint256 n) external atBlock {
        vm.roll(vm.getBlockNumber() + bound(n, 1, 12));
    }

    /// The first open market from `id` on (wrapping), else `id`: most actions aim at a live one.
    function _openFrom(uint256 id) internal view returns (uint256) {
        uint256 n = cor.marketCount();
        for (uint256 k = 0; k < n; k++) {
            uint256 j = (id - 1 + k) % n + 1;
            if (cor.market(j).state == OnyxCorrections.State.Open) return j;
        }
        return id;
    }

    /// The first open market from `id` on (wrapping) whose recheck is recorded, else `id`.
    function _resolvableFrom(uint256 id) internal view returns (uint256) {
        uint256 n = cor.marketCount();
        for (uint256 k = 0; k < n; k++) {
            uint256 j = (id - 1 + k) % n + 1;
            OnyxCorrections.Market memory m = cor.market(j);
            if (
                m.state == OnyxCorrections.State.Open && vm.getBlockNumber() >= m.closeBlock
                    && reg.observationOf(RMETRIC, m.subjectKey, WINDOW).result != bytes32(0)
            ) return j;
        }
        return id;
    }

    function resolve(uint256 id, uint8 liar) external atBlock {
        bool lie = liar % 5 == 0;
        if (cor.marketCount() == 0) return;
        id = _resolvableFrom(bound(id, 1, cor.marketCount()));
        OnyxCorrections.Market memory m = cor.market(id);
        int128 v = recheckValueOf[m.subjectKey];
        // A liar offers the original round and value instead of the recheck's.
        try cor.resolve(id, lie ? ROUND : RROUND, lie ? m.value : v) {
            OnyxCorrections.Market memory after_ = cor.market(id);
            if (after_.state == OnyxCorrections.State.Resolved) resolvedCount++;
            else voidCount++;
        } catch {}
    }

    function voidMarket(uint256 id) external atBlock {
        if (cor.marketCount() == 0) return;
        id = bound(id, 1, cor.marketCount());
        bool drawn = reg.drawCount(cor.market(id).recheckFact) != 0;
        try cor.void(id) {
            voidCount++;
            if (drawn) voidDrawn++;
        } catch {}
    }

    function withdraw(uint256 who, uint256 id) external atBlock {
        if (cor.marketCount() == 0) return;
        id = bound(id, 1, cor.marketCount());
        address a = _actor(who);
        uint256 before = tok.balanceOf(a);
        vm.prank(a);
        try cor.withdraw(id) {
            paidOut[id] += tok.balanceOf(a) - before;
        } catch {}
    }

    function claimFeeRefund(uint256 who) external atBlock {
        vm.prank(_actor(who));
        try cor.claimFeeRefund() {} catch {}
    }

    function payTreasury() external atBlock {
        try cor.payTreasury() {} catch {}
    }

    /// Ends every market (void after the grace where needed) and pays everyone and the treasury.
    function finishAll() external atBlock {
        vm.roll(vm.getBlockNumber() + 1_000);
        for (uint256 id = 1; id <= cor.marketCount(); id++) {
            if (cor.market(id).state == OnyxCorrections.State.Open) {
                OnyxCorrections.Market memory m = cor.market(id);
                try cor.resolve(id, RROUND, recheckValueOf[m.subjectKey]) {
                    if (cor.market(id).state == OnyxCorrections.State.Resolved) resolvedCount++;
                    else voidCount++;
                } catch {
                    voidCount++;
                    finishVoids++;
                    cor.void(id);
                }
            }
            for (uint256 i = 0; i < actors.length; i++) {
                if (cor.owed(id, actors[i]) == 0) continue;
                uint256 before = tok.balanceOf(actors[i]);
                vm.prank(actors[i]);
                cor.withdraw(id);
                paidOut[id] += tok.balanceOf(actors[i]) - before;
            }
        }
        for (uint256 i = 0; i < actors.length; i++) {
            if (cor.openerOwed(actors[i]) == 0) continue;
            vm.prank(actors[i]);
            cor.claimFeeRefund();
        }
        if (cor.treasuryOwed() != 0) cor.payTreasury();
    }
}

contract CorrectionsInvariants is StdInvariant, Test {
    CorrectionsHandler internal h;

    function setUp() public {
        vm.roll(10_000);
        h = new CorrectionsHandler();
        targetContract(address(h));
        // `finishAll` is the end-of-run drain (afterInvariant), not a fuzzed action.
        bytes4[] memory skip = new bytes4[](1);
        skip[0] = CorrectionsHandler.finishAll.selector;
        excludeSelector(FuzzSelector(address(h), skip));
    }

    /// THREATS CL7-9: every token held is owed to someone, and nothing more (no stuck funds,
    /// no shortfall).
    function invariant_CorrectionsHoldWhatTheyOwe() public view {
        OnyxCorrections c = h.cor();
        assertEq(h.tok().balanceOf(address(c)), c.liabilities());
    }

    /// Payout conservation per market: never more out than in; a finished market paid every bet
    /// back (void) or its pools less the fee, short by at most one wei per winner (resolved).
    function invariant_PayoutsConserve() public view {
        OnyxCorrections c = h.cor();
        for (uint256 id = 1; id <= c.marketCount(); id++) {
            OnyxCorrections.Market memory m = c.market(id);
            uint256 out = h.paidOut(id);
            assertLe(out, m.total);
            if (m.state == OnyxCorrections.State.Void && m.winnersLeft == 0) {
                assertEq(out, m.total);
            }
            if (m.state == OnyxCorrections.State.Resolved) {
                uint256 due = uint256(m.winningPool) + m.distributable;
                assertLe(out, due);
                if (m.winnersLeft == 0) {
                    assertLt(due - out, uint256(m.counts[m.winner - 1]) + 1);
                }
            }
        }
    }

    /// Settled only from the paired recheck, drawn after the close and recorded by the resolve
    /// block, with the value that recorded result commits to; never from the original round.
    function invariant_SettledOnlyFromATimelyRecheck() public view {
        OnyxCorrections c = h.cor();
        OnyxRegistry reg = h.reg();
        for (uint256 id = 1; id <= c.marketCount(); id++) {
            OnyxCorrections.Market memory m = c.market(id);
            if (m.winner == 0) continue;
            OnyxRegistry.Observation memory o =
                reg.observationOf(h.RMETRIC(), m.subjectKey, h.WINDOW());
            // The draw that recorded the recheck (the fact's latest) came after the close.
            uint32 n = reg.drawCount(m.recheckFact);
            assertGt(reg.drawOf(m.recheckFact, n - 1).announcedAt, m.closeBlock);
            assertGt(o.recordedAt, m.closeBlock);
            assertLe(o.recordedAt, m.resolveBlock);
            assertEq(c.resultHash(o.spec, h.RROUND(), m.recheckValue), o.result);
            int256 d = int256(m.recheckValue) - int256(m.value);
            bool accurate = d >= -int256(uint256(m.tolerance)) && d <= int256(uint256(m.tolerance));
            assertEq(m.winner, accurate ? 1 : 2);
            assertEq(m.tolerance, h.TOL());
        }
    }

    /// After every market ends and everyone withdraws, the contract holds nothing.
    function afterInvariant() public {
        h.finishAll();
        OnyxCorrections c = h.cor();
        assertEq(h.tok().balanceOf(address(c)), 0, "drained");
        assertEq(c.liabilities(), 0);
        string memory a = string.concat(
            "STATS corrections markets=",
            vm.toString(c.marketCount()),
            " resolved=",
            vm.toString(h.resolvedCount()),
            " voided=",
            vm.toString(h.voidCount()),
            " voidDrawn=",
            vm.toString(h.voidDrawn()),
            " finishVoids=",
            vm.toString(h.finishVoids())
        );
        console.log(a);
        if (vm.envOr("ONYX_INVARIANT_STATS", false)) {
            vm.writeLine("cache/invariant-stats/corrections.log", a);
        }
    }

    /// Several markets of one value settle from the one recheck: every settled market of a value
    /// has the same recheck value and, with the same tolerance, the same winner.
    function invariant_OneRecheckPerValue() public view {
        OnyxCorrections c = h.cor();
        for (uint256 i = 1; i <= c.marketCount(); i++) {
            OnyxCorrections.Market memory a = c.market(i);
            if (a.winner == 0) continue;
            for (uint256 j = i + 1; j <= c.marketCount(); j++) {
                OnyxCorrections.Market memory b = c.market(j);
                if (b.winner == 0 || b.fact != a.fact) continue;
                assertEq(a.recheckValue, b.recheckValue);
                assertEq(a.winner, b.winner);
            }
        }
    }
}
