// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeployCorrections} from "../script/DeployCorrections.s.sol";
import {OnyxCorrections} from "../src/OnyxCorrections.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {IStakeToken} from "../src/IOnyxMarkets.sol";
import {CoordinatorLedger} from "./OnyxObservedFacts.t.sol";
import {CorrectionsBase} from "./OnyxCorrections.t.sol";

/// The corrections deploy script (CL7), run against the local test EVM; no network.
contract DeployCorrectionsTest is Test {
    DeployCorrections internal d;
    OnyxRegistry internal reg = OnyxRegistry(address(0x5E6));
    IStakeToken internal tok = IStakeToken(address(0x70C));

    function setUp() public {
        d = new DeployCorrections();
    }

    function test_DeploysWiredWithThePlaceholders() public {
        OnyxCorrections c = d.deploy(tok, reg, address(0x7EA5));
        assertEq(address(c.registry()), address(reg));
        assertEq(address(c.token()), address(tok));
        assertEq(c.treasury(), address(0x7EA5));
        assertEq(c.owner(), address(d), "the broadcaster owns it");
        OnyxCorrections.Params memory p = d.defaults();
        assertEq(c.openFee(), p.openFee);
        assertEq(c.feePpm(), p.feePpm);
        assertEq(c.minTradingBlocks(), p.minTradingBlocks);
        assertEq(c.minRecheckBlocks(), p.minRecheckBlocks);
        assertEq(c.maxMarketBlocks(), p.maxMarketBlocks);
        assertEq(c.graceBlocks(), p.graceBlocks);
        assertEq(c.marketCount(), 0);
    }

    /// The owner's steps 4 and 5 (`pairRecheck`): register the recheck metric and pair it, once;
    /// a second run sends nothing; another template or tolerance is refused, never overwritten.
    function test_PairRegistersTheRecheckOnceAndRefusesAnotherOne() public {
        OnyxRegistry r = new OnyxRegistry(
            address(d), IRegistryLedger(address(new CoordinatorLedger(address(0xC0))))
        );
        bytes32 m = keccak256("us.treasury.tga_total_deposits");
        bytes32 rm = keccak256("us.treasury.tga_total_deposits.recheck");
        vm.prank(address(d));
        r.registerMetric(m, OnyxRegistry.Kind.DatedFeed, keccak256("template"), 0);
        OnyxCorrections c = d.deploy(tok, r, address(0x7EA5));
        d.pair(r, c, m, rm, keccak256("recheck template"), 0);
        (OnyxRegistry.Kind k, bytes32 t,) = r.metrics(rm);
        assertEq(uint8(k), uint8(OnyxRegistry.Kind.DatedFeed));
        assertEq(t, keccak256("recheck template"));
        (bytes32 paired, uint128 tol) = c.pairs(m);
        assertEq(paired, rm);
        assertEq(tol, 0);
        d.pair(r, c, m, rm, keccak256("recheck template"), 0);
        vm.expectRevert(
            abi.encodeWithSelector(
                DeployCorrections.RecheckRegisteredOtherwise.selector, keccak256("recheck template")
            )
        );
        d.pair(r, c, m, rm, keccak256("another template"), 0);
        vm.expectRevert(abi.encodeWithSelector(DeployCorrections.PairedOtherwise.selector, rm, 0));
        d.pair(r, c, m, rm, keccak256("recheck template"), 1);
    }

    function test_DeploysOnBaseSepoliaOnly() public {
        vm.chainId(84_532);
        d.deploy(tok, reg, address(0x7EA5));
        vm.chainId(8_453);
        vm.expectRevert(abi.encodeWithSelector(DeployCorrections.WrongChain.selector, 8_453));
        d.deploy(tok, reg, address(0x7EA5));
    }
}

/// The runbook's step 6 (`openMarket`, `scripts/corrections-sepolia.sh open`) against the contracts
/// wired as `scripts/corrections-anvil.sh` deploys them, with a tONYX whose minter has a key: what
/// it refuses before sending, the opener's funding, the approval and the market; no network.
contract DeployCorrectionsOpenTest is CorrectionsBase {
    DeployCorrections internal d;
    uint256 internal constant OPENER_KEY = 0xA11CE;
    uint256 internal constant MINTER_KEY = 0xB0B;
    /// The treasury's key: an insider, who may not bet, so may not open.
    uint256 internal constant INSIDER_KEY = 0x7EA5;
    address internal op = vm.addr(OPENER_KEY);
    address internal minter = vm.addr(MINTER_KEY);
    uint64 internal recheckEnd;

    function setUp() public override {
        super.setUp();
        d = new DeployCorrections();
        tok = new OnyxTestToken(minter);
        vm.prank(own);
        cor = new OnyxCorrections(IStakeToken(address(tok)), reg, vm.addr(INSIDER_KEY), _params());
        vm.prank(own);
        cor.pair(METRIC, RMETRIC, TOL);
        vm.deal(minter, 1 ether);
        vm.warp(1_794_600_000);
        recheckEnd = uint64(block.timestamp + 600);
    }

    /// One-second blocks; the contract allows trading >= 10 blocks, a recheck >= 20 blocks and a
    /// market <= 1,000 blocks; the last recheck day ends in 600 s.
    function _opening() internal view returns (DeployCorrections.Opening memory) {
        uint64 b = uint64(block.number);
        return DeployCorrections.Opening({
            metric: METRIC,
            subjectKey: SUBJECT,
            windowEnd: WINDOW,
            round: ROUND,
            value: VALUE,
            closeBlock: b + 10,
            resolveBlock: b + 700,
            recheckEnd: recheckEnd,
            blockMs: 1000,
            recheckMetric: RMETRIC,
            recheckTemplate: RTEMPLATE
        });
    }

    function _refused(
        DeployCorrections.Opening memory o,
        uint256 key,
        uint256 minterKey,
        bytes memory err
    ) internal {
        vm.expectRevert(err);
        d.openWith(cor, o, key, minterKey, false);
    }

    /// An opener with nothing: the minter grants gas and mints the fee, the opener approves
    /// exactly the fee and opens. Run again, while the market trades or after its close, nothing
    /// is sent (a lost record is found again); `another` opens a second market of the value (the
    /// fee minted again, no second grant).
    function test_OpenMarketFundsTheOpenerApprovesTheFeeAndOpens() public {
        assertEq(d.openWith(cor, _opening(), OPENER_KEY, MINTER_KEY, false), 1);
        OnyxCorrections.Market memory m = cor.market(1);
        assertEq(m.opener, op);
        assertEq(m.closeBlock, block.number + 10);
        assertEq(m.resolveBlock, block.number + 700);
        assertEq(m.value, VALUE);
        assertEq(m.fact, reg.factKey(METRIC, SUBJECT, WINDOW));
        assertEq(tok.balanceOf(address(cor)), FEE, "the minted fee was paid in");
        assertEq(tok.balanceOf(op), 0);
        assertEq(tok.allowance(op, address(cor)), 0, "exactly the fee was approved");
        assertEq(op.balance, d.GAS_GRANT_WEI(), "one gas grant");
        assertEq(d.openWith(cor, _opening(), OPENER_KEY, MINTER_KEY, false), 1, "still trading");
        assertEq(cor.marketCount(), 1);
        assertEq(tok.totalSupply(), FEE);
        vm.roll(m.closeBlock);
        assertEq(d.openWith(cor, _opening(), OPENER_KEY, MINTER_KEY, false), 1, "closed");
        assertEq(cor.marketCount(), 1);
        assertEq(d.openWith(cor, _opening(), OPENER_KEY, MINTER_KEY, true), 2);
        assertEq(tok.totalSupply(), 2 * FEE);
        assertEq(op.balance, d.GAS_GRANT_WEI(), "no second grant");
        assertEq(cor.liabilities(), tok.balanceOf(address(cor)));
    }

    /// An opener that holds the fee, approved, and gas: no minter key is needed, nothing minted.
    function test_OpenMarketUsesTheOpenersOwnTonyx() public {
        vm.prank(minter);
        tok.mint(op, FEE);
        vm.prank(op);
        tok.approve(address(cor), type(uint256).max);
        vm.deal(op, 1 ether);
        assertEq(d.openWith(cor, _opening(), OPENER_KEY, 0, false), 1);
        assertEq(tok.totalSupply(), FEE, "nothing minted");
        assertEq(op.balance, 1 ether, "no gas grant");
        assertEq(tok.allowance(op, address(cor)), type(uint256).max, "no new approval");
        assertEq(tok.balanceOf(address(cor)), FEE);
    }

    /// Every refusal comes before anything is sent: no market, nothing minted.
    function test_OpenMarketRefusesBeforeSendingAnything() public {
        uint256 b = block.number;
        uint256 t = block.timestamp;
        DeployCorrections.Opening memory o = _opening();
        o.closeBlock = uint64(b + 9);
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.CloseTooSoon.selector, b + 9, b + 10)
        );
        o = _opening();
        o.resolveBlock = o.closeBlock + 19;
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.DeadlineTooSoon.selector, b + 29, b + 30)
        );
        o = _opening();
        o.resolveBlock = uint64(b + 1_001);
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.DeadlineTooLate.selector, b + 1_001, b + 1_000)
        );
        // Trading would outlast the last recheck day; the deadline would come inside it.
        o = _opening();
        o.recheckEnd = uint64(t + 10);
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.CloseAfterRecheckDays.selector, t + 10, t + 10)
        );
        o = _opening();
        o.recheckEnd = uint64(t + 701);
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(
                DeployCorrections.DeadlineInRecheckDays.selector, t + 700, t + 701
            )
        );
        o = _opening();
        o.blockMs = 0;
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.OutOfRange.selector, "ONYX_BLOCK_MS")
        );
        // Another value, another round, a value never recorded, an unpaired metric.
        bytes32 recorded = cor.resultHash(SPEC, ROUND, VALUE);
        o = _opening();
        o.value = VALUE + 1;
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(
                DeployCorrections.NotTheRecordedValue.selector,
                recorded,
                cor.resultHash(SPEC, ROUND, VALUE + 1)
            )
        );
        o = _opening();
        o.round = RROUND;
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(
                DeployCorrections.NotTheRecordedValue.selector,
                recorded,
                cor.resultHash(SPEC, RROUND, VALUE)
            )
        );
        o = _opening();
        o.subjectKey = keccak256("us.treasury.tga_deposits:2026-10-29");
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(
                DeployCorrections.NoRecordedValue.selector,
                reg.factKey(METRIC, o.subjectKey, WINDOW)
            )
        );
        o = _opening();
        o.metric = OTHER;
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.MetricNotPaired.selector, OTHER)
        );
        // An insider opener; a key that is not the minter's; another chain.
        _refused(
            _opening(),
            INSIDER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.OpenerIsInsider.selector, vm.addr(INSIDER_KEY))
        );
        _refused(
            _opening(),
            OPENER_KEY,
            OPENER_KEY,
            abi.encodeWithSelector(DeployCorrections.NotMinter.selector, op, minter)
        );
        // A recheck metric or template other than the caller computed (other after/span days).
        o = _opening();
        o.recheckMetric = keccak256("us.treasury.tga_total_deposits.recheck-other");
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.PairedOtherwise.selector, RMETRIC, TOL)
        );
        o = _opening();
        o.recheckTemplate = keccak256("another recheck template");
        _refused(
            o,
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.RecheckRegisteredOtherwise.selector, RTEMPLATE)
        );
        vm.chainId(8_453);
        _refused(
            _opening(),
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(DeployCorrections.WrongChain.selector, 8_453)
        );
        assertEq(cor.marketCount(), 0);
        assertEq(tok.totalSupply(), 0);
        assertEq(op.balance, 0);
    }

    /// Once the recheck is recorded no market can open (the contract's `RecheckStarted`).
    function test_OpenMarketRefusesAfterTheRecheck() public {
        _record(RMETRIC, SPEC, RROUND, VALUE);
        _refused(
            _opening(),
            OPENER_KEY,
            MINTER_KEY,
            abi.encodeWithSelector(
                DeployCorrections.RecheckRecorded.selector, reg.factKey(RMETRIC, SUBJECT, WINDOW)
            )
        );
    }

    function _betRefused(uint256 id, uint8 side, uint128 amount, uint256 key, bytes memory err)
        internal
    {
        vm.expectRevert(err);
        d.betWith(cor, id, side, amount, key, MINTER_KEY);
    }

    /// A bettor with nothing gets gas and its stake from the minter, approves exactly the stake
    /// and bets; one that holds and approved tONYX gets nothing minted and approves nothing.
    function test_PlaceBetFundsTheBettorAndBets() public {
        d.openWith(cor, _opening(), OPENER_KEY, MINTER_KEY, false);
        uint256 bettorKey = 0xBE7;
        address b = vm.addr(bettorKey);
        d.betWith(cor, 1, 1, 30e18, bettorKey, MINTER_KEY);
        (uint128 yes, uint128 no) = cor.betsOf(1, b);
        assertEq(yes, 30e18);
        assertEq(no, 0);
        assertEq(tok.balanceOf(b), 0);
        assertEq(tok.allowance(b, address(cor)), 0, "exactly the stake was approved");
        assertEq(b.balance, d.GAS_GRANT_WEI());
        uint256 supply = tok.totalSupply();
        vm.prank(minter);
        tok.mint(op, 20e18);
        vm.prank(op);
        tok.approve(address(cor), type(uint256).max);
        d.betWith(cor, 1, 2, 20e18, OPENER_KEY, 0);
        (, no) = cor.betsOf(1, op);
        assertEq(no, 20e18, "the opener may bet");
        assertEq(tok.totalSupply(), supply + 20e18, "nothing minted by the script");
        assertEq(tok.allowance(op, address(cor)), type(uint256).max, "no new approval");
        OnyxCorrections.Market memory m = cor.market(1);
        assertEq(m.pools[0], 30e18);
        assertEq(m.pools[1], 20e18);
        assertEq(cor.liabilities(), tok.balanceOf(address(cor)));
    }

    /// Every bet refusal comes before anything is sent.
    function test_PlaceBetRefusesBeforeSendingAnything() public {
        _betRefused(
            1,
            1,
            1e18,
            OPENER_KEY,
            abi.encodeWithSelector(DeployCorrections.MarketNotOpen.selector, 1)
        );
        d.openWith(cor, _opening(), OPENER_KEY, MINTER_KEY, false);
        uint256 supply = tok.totalSupply();
        _betRefused(
            1,
            1,
            1e18,
            INSIDER_KEY,
            abi.encodeWithSelector(DeployCorrections.BettorIsInsider.selector, vm.addr(INSIDER_KEY))
        );
        _betRefused(
            1, 3, 1e18, OPENER_KEY, abi.encodeWithSelector(DeployCorrections.BadSide.selector, 3)
        );
        _betRefused(
            1,
            1,
            0,
            OPENER_KEY,
            abi.encodeWithSelector(DeployCorrections.BadBetAmount.selector, 0, 1, 100e18)
        );
        _betRefused(
            1,
            2,
            101e18,
            OPENER_KEY,
            abi.encodeWithSelector(DeployCorrections.BadBetAmount.selector, 101e18, 1, 100e18)
        );
        uint64 close = cor.market(1).closeBlock;
        vm.roll(close);
        _betRefused(
            1,
            1,
            1e18,
            OPENER_KEY,
            abi.encodeWithSelector(DeployCorrections.MarketClosed.selector, 1, close)
        );
        assertEq(tok.totalSupply(), supply);
        assertEq(cor.market(1).total, 0);
    }
}
