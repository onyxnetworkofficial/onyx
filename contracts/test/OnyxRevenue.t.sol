// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxSettlementFixture} from "./OnyxSettlement.t.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxLedger} from "../src/OnyxLedger.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxRevenue, IOnyxLedgerView, IRevenueToken} from "../src/OnyxRevenue.sol";
import {MockUSDC3009} from "./mocks/MockUSDC3009.sol";
import {DeployRevenue} from "../script/DeployRevenue.s.sol";

/// Usage revenue on a real finalized shard (ADR-0025): registration from the ledger, x402
/// receipts, direct payments, claims and withdrawals. Fixture: 2 miners on the same root, 3
/// validators who Agree on both, 4 standbys who give no verdict.
contract OnyxRevenueTest is OnyxSettlementFixture {
    MockUSDC3009 internal cash;
    OnyxRevenue internal rev;
    address internal reporter = address(0x4E9);
    address internal treasury = address(0x7EA5);
    address internal labelOwner = address(0x1AB);
    address internal payer = address(0x9A7E4);

    function _deploy() internal {
        cash = new MockUSDC3009();
        uint16[4] memory bps = [uint16(5_000), 2_000, 1_000, 2_000];
        rev = new OnyxRevenue(
            IRevenueToken(address(cash)), IOnyxLedgerView(address(st)), reporter, treasury, bps
        );
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, labelOwner);
    }

    /// Unanimous, undisputed, past the challenge window: final.
    function _final() internal {
        _deploy();
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.roll(block.number + 200);
        assertTrue(sx().isFinal(ID, shard));
    }

    /// A facilitator settles an x402 payment of `amount` to the revenue contract.
    function _x402(uint256 amount, bytes32 nonce) internal {
        cash.mint(payer, amount);
        cash.transferWithAuthorization(payer, address(rev), amount, nonce);
    }

    function _receipt(uint128 amount, bytes32 nonce)
        internal
        view
        returns (OnyxRevenue.Receipt[] memory rs)
    {
        rs = new OnyxRevenue.Receipt[](1);
        rs[0] = OnyxRevenue.Receipt({
            round: ID, shard: shard, amount: amount, payer: payer, nonce: nonce
        });
    }

    function test_RegisterFixesRecipientsFromTheLedger() public {
        _final();
        rev.register(ID, shard, fields);
        (bool reg, uint32 miners, uint32 validators, uint64 at, bytes32 labelSet,,,) =
            rev.books(ID, shard);
        assertTrue(reg);
        assertEq(miners, 2);
        assertEq(validators, 3, "standbys gave no verdict and earn nothing");
        assertEq(labelSet, fields.labelSetHash);
        assertEq(rev.lastAccepted(m1), at);
        assertEq(rev.lastAccepted(vals[0]), at);
        assertEq(rev.lastAccepted(standbyList[0]), 0);
        vm.expectRevert(OnyxRevenue.AlreadyRegistered.selector);
        rev.register(ID, shard, fields);
    }

    function test_RegisterRefusesFieldsThatAreNotTheFinalBinding() public {
        _final();
        OnyxCommit.CommitmentFields memory f = fields;
        f.labelSetHash = keccak256("someone else's labels");
        vm.expectRevert(OnyxRevenue.BadFields.selector);
        rev.register(ID, shard, f);
    }

    function test_RegisterRefusesAShardStillOpenToDisputes() public {
        _deploy();
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.expectRevert(OnyxRevenue.NotFinal.selector);
        rev.register(ID, shard, fields);
    }

    function test_ReceiptSplitsClaimsAndWithdraws() public {
        _final();
        rev.register(ID, shard, fields);
        _x402(10_000, keccak256("n1"));
        assertEq(rev.unallocated(), 10_000);
        vm.prank(reporter);
        rev.credit(_receipt(10_000, keccak256("n1")));
        assertEq(rev.unallocated(), 0);
        // 50% to 2 miners, 20% to 3 validators (666 each, 2 dust), 10% label, rest treasury.
        assertEq(rev.claim(ID, shard, m1), 2_500);
        assertEq(rev.claim(ID, shard, vals[1]), 666);
        assertEq(rev.claim(ID, shard, m1), 0, "claimed once");
        assertEq(rev.claim(ID, shard, standbyList[0]), 0);
        assertEq(rev.balanceOf(labelOwner), 1_000);
        assertEq(rev.balanceOf(treasury), 2_002);
        vm.prank(m1);
        rev.withdraw();
        assertEq(cash.balanceOf(m1), 2_500);
        // Everything owed is covered by what the contract holds.
        rev.claim(ID, shard, m2);
        rev.claim(ID, shard, vals[0]);
        rev.claim(ID, shard, vals[2]);
        uint256 owed = rev.balanceOf(m2) + rev.balanceOf(vals[0]) + rev.balanceOf(vals[1])
            + rev.balanceOf(vals[2]) + rev.balanceOf(labelOwner) + rev.balanceOf(treasury);
        assertEq(owed, cash.balanceOf(address(rev)));
    }

    /// REV-L1-F6: a receipt that fails a check is skipped with an event, never reverting the
    /// batch, so one bad receipt cannot take the good ones with it.
    function test_CreditSkipsBadReceiptsAndAppliesTheRest() public {
        _final();
        rev.register(ID, shard, fields);
        _x402(1_000, keccak256("n1"));
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRevenue.NotReporter.selector);
        rev.credit(_receipt(1_000, keccak256("n1")));
        OnyxRevenue.Receipt[] memory rs = new OnyxRevenue.Receipt[](5);
        // Never settled; more than arrived; another shard not registered; good; good again.
        rs[0] = _receipt(1_000, keccak256("never"))[0];
        rs[1] = _receipt(1_001, keccak256("n1"))[0];
        rs[2] = _receipt(1_000, keccak256("n1"))[0];
        rs[2].shard = keccak256("unregistered");
        rs[3] = _receipt(1_000, keccak256("n1"))[0];
        rs[4] = _receipt(1_000, keccak256("n1"))[0];
        vm.expectEmit(true, true, true, true);
        emit OnyxRevenue.ReceiptSkipped(ID, shard, payer, keccak256("never"), 2);
        vm.expectEmit(true, true, true, true);
        emit OnyxRevenue.ReceiptSkipped(ID, shard, payer, keccak256("n1"), 3);
        vm.expectEmit(true, true, true, true);
        emit OnyxRevenue.ReceiptSkipped(ID, keccak256("unregistered"), payer, keccak256("n1"), 4);
        vm.prank(reporter);
        assertEq(rev.credit(rs), 1, "only the first good receipt applies; its repeat is reused");
        assertEq(rev.unallocated(), 0);
        assertEq(rev.claim(ID, shard, m1), 250);
        // The same payment again, even once more money has arrived: skipped as reused.
        _x402(1_000, keccak256("n2"));
        vm.prank(reporter);
        assertEq(rev.credit(_receipt(1_000, keccak256("n1"))), 0);
        assertEq(rev.unallocated(), 1_000);
    }

    /// REV-L1-F4: the receipt checks are total-capped only. `authorizationState` binds neither
    /// recipient nor amount, so the reporter can attach a nonce the payer spent elsewhere to any
    /// unattributed money (the accepted REV-1 trust, never more than arrived). A leaked reporter
    /// key is rotated out by the owner (the treasury).
    function test_ReceiptChecksAreTotalCappedAndTheReporterRotates() public {
        _final();
        rev.register(ID, shard, fields);
        cash.mint(payer, 1);
        cash.transferWithAuthorization(payer, address(0xD00D), 1, keccak256("elsewhere"));
        cash.mint(address(rev), 9_999);
        vm.prank(reporter);
        assertEq(rev.credit(_receipt(9_999, keccak256("elsewhere"))), 1);
        assertEq(rev.unallocated(), 0, "never more than arrived");
        // Rotation: only the owner; the old reporter is out at once.
        assertEq(rev.owner(), treasury);
        vm.prank(reporter);
        vm.expectRevert(OnyxRevenue.NotOwner.selector);
        rev.setReporter(address(0xBEEF));
        vm.prank(treasury);
        rev.setReporter(address(0xBEEF));
        _x402(10, keccak256("n9"));
        vm.prank(reporter);
        vm.expectRevert(OnyxRevenue.NotReporter.selector);
        rev.credit(_receipt(10, keccak256("n9")));
        vm.prank(address(0xBEEF));
        assertEq(rev.credit(_receipt(10, keccak256("n9"))), 1);
    }

    function test_DirectPaymentSplitsAtOnce() public {
        _final();
        vm.expectRevert(OnyxRevenue.NotRegistered.selector);
        rev.pay(ID, shard, 1);
        rev.register(ID, shard, fields);
        cash.mint(payer, 5_000);
        vm.startPrank(payer);
        cash.approve(address(rev), 5_000);
        rev.pay(ID, shard, 5_000);
        vm.stopPrank();
        assertEq(rev.claim(ID, shard, m2), 1_250);
        assertEq(rev.unallocated(), 0, "a direct payment is never attributable twice");
    }

    /// REV-L1-F3: registering before the label set has a payee (anyone may register the moment
    /// the shard is final) no longer moves the label share to the miners: it is held for the
    /// set and paid to the payee once named.
    function test_LabelShareIsHeldUntilAPayeeIsNamed() public {
        _final();
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, address(0)); // payee not named yet
        vm.prank(m1);
        rev.register(ID, shard, fields);
        cash.mint(payer, 10_000);
        vm.startPrank(payer);
        cash.approve(address(rev), 10_000);
        rev.pay(ID, shard, 10_000);
        vm.stopPrank();
        assertEq(rev.claim(ID, shard, m1), 2_500, "miners keep their own share only");
        assertEq(rev.labelHeld(fields.labelSetHash), 1_000);
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, labelOwner);
        assertEq(rev.balanceOf(labelOwner), 1_000, "the held share reaches the payee");
        assertEq(rev.labelHeld(fields.labelSetHash), 0);
        // Later fees go straight to the payee.
        cash.mint(payer, 10_000);
        vm.startPrank(payer);
        cash.approve(address(rev), 10_000);
        rev.pay(ID, shard, 10_000);
        vm.stopPrank();
        assertEq(rev.balanceOf(labelOwner), 2_000);
    }

    function _owedWith(address a, address b) internal view returns (uint256 o) {
        o = rev.balanceOf(m1) + rev.balanceOf(m2) + rev.balanceOf(vals[0]) + rev.balanceOf(vals[1])
            + rev.balanceOf(vals[2]) + rev.balanceOf(labelOwner) + rev.balanceOf(treasury)
            + rev.balanceOf(a) + rev.balanceOf(b) + rev.labelHeld(fields.labelSetHash);
    }

    function _claimEveryone() internal {
        rev.claim(ID, shard, m1);
        rev.claim(ID, shard, m2);
        rev.claim(ID, shard, vals[0]);
        rev.claim(ID, shard, vals[1]);
        rev.claim(ID, shard, vals[2]);
    }

    /// REV-L2 (review 2 of REV-L1-F3): a held label share is paid once, to the first payee
    /// named; renaming the payee never pays it again; what is owed always equals what is held.
    function test_HeldLabelShareIsPaidOnceNeverTwice() public {
        _final();
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, address(0));
        rev.register(ID, shard, fields);
        _x402(10_000, keccak256("a"));
        vm.prank(reporter);
        rev.credit(_receipt(10_000, keccak256("a")));
        assertEq(rev.labelHeld(fields.labelSetHash), 1_000);
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, address(0xB0B));
        assertEq(rev.balanceOf(address(0xB0B)), 1_000);
        assertEq(rev.labelHeld(fields.labelSetHash), 0);
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, address(0xE11));
        assertEq(rev.balanceOf(address(0xE11)), 0, "no second payment on rename");
        _claimEveryone();
        assertEq(_owedWith(address(0xB0B), address(0xE11)), cash.balanceOf(address(rev)));
    }

    /// REV-L2-F3 (accepted owner trust, REV-1): the owner can rotate in a reporter that names
    /// itself payee and so takes held and future label shares; credited miner and validator
    /// shares stay out of reach.
    function test_OwnerRotationReachesLabelSharesButNeverCreditedClaims() public {
        _final();
        vm.prank(reporter);
        rev.setLabelPayee(fields.labelSetHash, address(0));
        rev.register(ID, shard, fields);
        _x402(10_000, keccak256("a"));
        vm.prank(reporter);
        rev.credit(_receipt(10_000, keccak256("a")));
        address next = address(0xE11);
        vm.prank(treasury);
        rev.setReporter(next);
        vm.prank(reporter);
        vm.expectRevert(OnyxRevenue.NotReporter.selector);
        rev.setLabelPayee(fields.labelSetHash, labelOwner);
        vm.prank(next);
        rev.setLabelPayee(fields.labelSetHash, next);
        assertEq(
            rev.balanceOf(next), 1_000, "held label share follows the payee the reporter names"
        );
        assertEq(rev.claim(ID, shard, m1), 2_500, "miners' credited share untouched");
        assertEq(rev.claim(ID, shard, vals[0]), 666, "validators' credited share untouched");
    }

    /// REV-L2-F5: receipts skipped for a reason that can clear (why 2: payment not seen as
    /// used yet; why 4: shard not registered yet) apply exactly once when posted again; a
    /// duplicate in the same batch is skipped (why 1).
    function test_SkippedReceiptsPostedAgainApplyOnce() public {
        _final();
        _x402(1_000, keccak256("a"));
        vm.prank(reporter);
        assertEq(rev.credit(_receipt(1_000, keccak256("a"))), 0, "why 4");
        vm.prank(reporter);
        assertEq(rev.credit(_receipt(500, keccak256("b"))), 0, "why 2");
        rev.register(ID, shard, fields);
        _x402(500, keccak256("b"));
        OnyxRevenue.Receipt[] memory rs = new OnyxRevenue.Receipt[](3);
        rs[0] = _receipt(1_000, keccak256("a"))[0];
        rs[1] = _receipt(500, keccak256("b"))[0];
        rs[2] = _receipt(500, keccak256("b"))[0];
        vm.prank(reporter);
        assertEq(rev.credit(rs), 2, "both apply once; the duplicate is skipped");
        assertEq(rev.unallocated(), 0);
        _claimEveryone();
        assertEq(_owedWith(address(0), address(0)), cash.balanceOf(address(rev)));
    }
}

/// A ledger view whose answers each test sets (`OnyxRevenue.register` edge cases).
contract MockLedgerView is IOnyxLedgerView {
    bool public finalFlag = true;
    bool public closedFlag;
    bytes32 public root;
    bytes32 public binding;
    OnyxLedger.Reveal[] internal rv;
    address[] internal members;
    mapping(address => uint16) internal agrees;

    function set(bytes32 r, bytes32 b, bool closed) external {
        root = r;
        binding = b;
        closedFlag = closed;
    }

    function addReveal(address m, bytes32 r) external {
        rv.push(OnyxLedger.Reveal({miner: m, root: r, binding: bytes32(0)}));
    }

    function addMember(address v, uint16 a) external {
        members.push(v);
        agrees[v] = a;
    }

    function isFinal(uint64, bytes32) external view returns (bool) {
        return finalFlag;
    }

    function shardState(uint64, bytes32)
        external
        view
        returns (OnyxLedger.ShardStatus, bool, uint64, bytes32, bytes32)
    {
        return (OnyxLedger.ShardStatus.Finalized, closedFlag, 77, root, binding);
    }

    function revealsOf(uint64, bytes32) external view returns (OnyxLedger.Reveal[] memory) {
        return rv;
    }

    function roundMembers(uint64) external view returns (address[] memory) {
        return members;
    }

    function tallyOf(uint64, bytes32, address v) external view returns (OnyxLedger.Tally memory t) {
        t.agrees = agrees[v];
    }
}

/// Register rules on cases the fixture cannot reach cheaply, and Rust parity of the split.
contract OnyxRevenueUnitTest is Test {
    MockUSDC3009 internal cash = new MockUSDC3009();
    MockLedgerView internal lv = new MockLedgerView();
    OnyxCommit.CommitmentFields internal f;

    function _rev(uint16[4] memory bps) internal returns (OnyxRevenue) {
        return new OnyxRevenue(
            IRevenueToken(address(cash)),
            IOnyxLedgerView(address(lv)),
            address(this),
            address(7),
            bps
        );
    }

    function _fields(bytes32 root) internal returns (OnyxCommit.CommitmentFields memory) {
        f.root = root;
        f.labelSetHash = keccak256("labels");
        lv.set(root, OnyxCommit.bindingHash(f), false);
        return f;
    }

    function test_SplitMustSumToOneHundredPercent() public {
        vm.expectRevert(OnyxRevenue.BadSplit.selector);
        _rev([uint16(5_000), 2_000, 1_000, 1_999]);
    }

    /// Only reveals of the final root count, each miner once; a case-decided shard pays no
    /// validators (its panel was paid by the case).
    function test_RegisterCountsOnlyRightMinersAndNoValidatorsAfterACase() public {
        OnyxRevenue r = _rev([uint16(5_000), 2_000, 1_000, 2_000]);
        bytes32 root = keccak256("final");
        OnyxCommit.CommitmentFields memory c = _fields(root);
        lv.addReveal(address(0xA), root);
        lv.addReveal(address(0xB), keccak256("wrong"));
        lv.addReveal(address(0xC), root);
        lv.addMember(address(0xD), 1);
        lv.set(root, OnyxCommit.bindingHash(c), true);
        r.register(1, bytes32(uint256(1)), c);
        (, uint32 miners, uint32 validators,,,,,) = r.books(1, bytes32(uint256(1)));
        assertEq(miners, 2);
        assertEq(validators, 0);
        assertEq(r.lastAccepted(address(0xB)), 0, "a wrong miner is not a contributor");
        assertEq(r.lastAccepted(address(0xD)), 0);
    }

    /// A shard that pinned no label set has no label share: it goes to the miners.
    function test_NoLabelSetGivesTheLabelShareToTheMiners() public {
        OnyxRevenue r = _rev([uint16(5_000), 2_000, 1_000, 2_000]);
        bytes32 root = keccak256("final");
        f.root = root;
        f.labelSetHash = bytes32(0);
        lv.set(root, OnyxCommit.bindingHash(f), true);
        lv.addReveal(address(0xA), root);
        r.register(1, bytes32(uint256(3)), f);
        cash.mint(address(this), 1_000);
        cash.approve(address(r), 1_000);
        r.pay(1, bytes32(uint256(3)), 1_000);
        assertEq(r.claim(1, bytes32(uint256(3)), address(0xA)), 800, "50% + 20% + 10%");
        assertEq(r.labelHeld(bytes32(0)), 0);
    }

    /// Registration at the ledger's caps (32 reveals, 128 members, all paid) fits the repo's
    /// 15M gas budget.
    function test_RegisterBoundAtTheCaps() public {
        OnyxRevenue r = _rev([uint16(5_000), 2_000, 1_000, 2_000]);
        bytes32 root = keccak256("final");
        OnyxCommit.CommitmentFields memory c = _fields(root);
        for (uint256 i = 0; i < 32; i++) {
            lv.addReveal(address(uint160(0x1000 + i)), root);
        }
        for (uint256 i = 0; i < 128; i++) {
            lv.addMember(address(uint160(0x2000 + i)), 1);
        }
        uint256 g = gasleft();
        r.register(1, bytes32(uint256(2)), c);
        uint256 used = g - gasleft();
        emit log_named_uint("register gas at the caps", used);
        assertLt(used, 15_000_000);
    }

    /// The same split as `onyx_coordinator::revenue::split` (fixtures/revenue/vectors.json).
    function test_SplitMatchesRustVectors() public {
        string memory json = vm.readFile("../fixtures/revenue/vectors.json");
        uint256 count = vm.parseJsonUint(json, ".count");
        assertGt(count, 0);
        OnyxRevenue r;
        uint256 lastKey;
        for (uint256 i = 0; i < count; i++) {
            string memory p = string.concat(".cases[", vm.toString(i), "].");
            uint16[4] memory bps = [
                uint16(vm.parseJsonUint(json, string.concat(p, "minersBps"))),
                uint16(vm.parseJsonUint(json, string.concat(p, "validatorsBps"))),
                uint16(vm.parseJsonUint(json, string.concat(p, "labelsBps"))),
                uint16(vm.parseJsonUint(json, string.concat(p, "treasuryBps")))
            ];
            uint256 key = uint256(keccak256(abi.encode(bps)));
            if (key != lastKey) {
                r = _rev(bps);
                lastKey = key;
            }
            (uint256 pm, uint256 pv, uint256 lc, uint256 tr) = r.split(
                vm.parseUint(vm.parseJsonString(json, string.concat(p, "amount"))),
                uint32(vm.parseJsonUint(json, string.concat(p, "miners"))),
                uint32(vm.parseJsonUint(json, string.concat(p, "validators"))),
                vm.parseJsonBool(json, string.concat(p, "labelSet"))
            );
            assertEq(pm, vm.parseUint(vm.parseJsonString(json, string.concat(p, "perMiner"))));
            assertEq(pv, vm.parseUint(vm.parseJsonString(json, string.concat(p, "perValidator"))));
            assertEq(lc, vm.parseUint(vm.parseJsonString(json, string.concat(p, "label"))));
            assertEq(tr, vm.parseUint(vm.parseJsonString(json, string.concat(p, "treasury"))));
        }
    }

    /// The deploy script builds the default 50/20/10/20 split and refuses other chains.
    function test_DeployScriptUsesTheDefaultSplitOnTestnetsOnly() public {
        DeployRevenue d = new DeployRevenue();
        vm.chainId(31_337);
        OnyxRevenue r = d.deploy(address(cash), address(lv), address(1), address(2), d.shares());
        assertEq(r.minersBps(), 5_000);
        assertEq(r.validatorsBps(), 2_000);
        assertEq(r.labelsBps(), 1_000);
        assertEq(r.treasuryBps(), 2_000);
        uint16[4] memory bps = d.shares();
        vm.chainId(8_453);
        vm.expectRevert(abi.encodeWithSelector(DeployRevenue.WrongChain.selector, 8_453));
        d.deploy(address(cash), address(lv), address(1), address(2), bps);
    }

    /// Whatever the split and heads, the parts sum to the fee (dust to the treasury).
    function testFuzz_PartsSumToTheFee(uint128 amount, uint8 miners, uint8 validators, bool label)
        public
    {
        OnyxRevenue r = _rev([uint16(3_333), 3_333, 3_333, 1]);
        (uint256 pm, uint256 pv, uint256 lc, uint256 tr) =
            r.split(amount, miners, validators, label);
        assertEq(pm * miners + pv * validators + lc + tr, amount);
    }
}
