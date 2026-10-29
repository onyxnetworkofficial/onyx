// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxSettlementFixture} from "./OnyxSettlement.t.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxMerkle} from "../src/OnyxMerkle.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";

/// ADR-0030: the registry against a real settlement (the fixture's coordinator is this test).
contract OnyxRegistryTest is OnyxSettlementFixture {
    OnyxRegistry internal reg;
    address internal gov = address(0x60F);
    bytes32 internal constant METRIC = keccak256("crypto.btc.entity_flow");
    bytes32 internal constant FEED_METRIC = keccak256("example.fixed.feed");
    /// The Treasury metric: its template is dated (`MetricKind::of`), so `DatedFeed` (REG-R2-F2).
    bytes32 internal constant DATED_METRIC = keccak256("us.treasury.total_public_debt_outstanding");
    bytes32 internal constant FEED_TEMPLATE = keccak256("feed template");
    bytes32 internal constant SUBJECT = keccak256("subject");
    uint64 internal constant WINDOW = 1_000_000;
    uint64 internal constant LIMIT = 4_000_000_000;
    uint8 internal constant LABEL_LEVEL = 4;
    /// The draw's pool hash and config hash (L2RND-R3: both committed before the beacon).
    bytes32 internal constant POOL = keccak256("pool");
    bytes32 internal constant CFG = keccak256("cfg");

    function _reg() internal {
        reg = new OnyxRegistry(gov, IRegistryLedger(address(st)));
        vm.startPrank(gov);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.Recipe, fields.recipeId, 0);
        reg.registerMetric(FEED_METRIC, OnyxRegistry.Kind.Feed, FEED_TEMPLATE, 0);
        reg.registerMetric(DATED_METRIC, OnyxRegistry.Kind.DatedFeed, FEED_TEMPLATE, 0);
        reg.registerLabelSet(fields.labelSetHash, LABEL_LEVEL);
        vm.stopPrank();
        // The fixture's coordinator is this test (SIG-R1-F1: every draw needs a receipt signer).
        reg.setReceiptSigner(address(0x5EC));
    }

    /// Announce the fact's next draw, committed to `spec` (with `POOL` and `CFG`), and fix its
    /// beacon (a recorded result needs one, and only the committed spec records).
    function _draw(bytes32 metric, bytes32 subject, uint64 we, bytes32 spec)
        internal
        returns (bytes32 fact)
    {
        fact = reg.factKey(metric, subject, we);
        uint32 n = reg.drawCount(fact);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        reg.announceDraw(metric, subject, we, n, reg.drawCommit(POOL, spec, CFG), h);
        vm.roll(h + 1);
        vm.setBlockhash(h, keccak256(abi.encode("beacon", fact, n)));
        reg.fixBeacon(fact, n);
    }

    /// The fixture shard's preimage.
    function _pre() internal view returns (OnyxRegistry.ShardPreimage memory) {
        return OnyxRegistry.ShardPreimage({
            recipeId: fields.recipeId,
            runtimeHash: fields.runtimeHash,
            labelSetHash: fields.labelSetHash,
            sourceHash: fields.sourceHash,
            schemaId: fields.schemaId,
            rightsHash: fields.rightsHash,
            paramsHash: keccak256("params"),
            registryRoot: keccak256("registry")
        });
    }

    function _attest(uint8 level) internal returns (bytes32) {
        return reg.attestShard(ID, _pre(), runtimeBytes, METRIC, 1_000, level);
    }

    function _finalize() internal {
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.roll(block.number + 200);
        assertTrue(sx().isFinal(ID, shard));
    }

    function _sib() internal view returns (bytes32[] memory sib) {
        sib = new bytes32[](1);
        sib[0] = OnyxMerkle.leafHash(rows[1]);
    }

    function test_OnlyTheOwnerRegistersAndATemplateIsFinal() public {
        _reg();
        (OnyxRegistry.Kind kind, bytes32 t,) = reg.metrics(METRIC);
        assertEq(uint8(kind), uint8(OnyxRegistry.Kind.Recipe));
        assertEq(t, fields.recipeId);
        vm.expectRevert(OnyxRegistry.NotOwner.selector);
        reg.registerMetric(keccak256("other"), OnyxRegistry.Kind.Recipe, keccak256("r"), 0);
        vm.expectRevert(OnyxRegistry.NotOwner.selector);
        reg.registerLabelSet(keccak256("other labels"), 3);
        vm.startPrank(gov);
        vm.expectRevert(OnyxRegistry.AlreadyRegistered.selector);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.Recipe, keccak256("another recipe"), 0);
        vm.expectRevert(OnyxRegistry.AlreadyRegistered.selector);
        reg.registerLabelSet(fields.labelSetHash, 3);
        vm.expectRevert(OnyxRegistry.ZeroValue.selector);
        reg.registerMetric(keccak256("x"), OnyxRegistry.Kind.None, keccak256("r"), 0);
        vm.expectRevert(OnyxRegistry.ZeroValue.selector);
        reg.registerMetric(keccak256("x"), OnyxRegistry.Kind.Recipe, bytes32(0), 0);
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        reg.registerLabelSet(keccak256("unstaked labels"), 6);
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        reg.registerLabelSet(keccak256("no level"), 0);
        vm.stopPrank();
    }

    /// REG-R1-F3: the coordinator may never own the registry.
    function test_TheCoordinatorCannotOwnTheRegistry() public {
        vm.expectRevert(OnyxRegistry.OwnerIsCoordinator.selector);
        new OnyxRegistry(address(this), IRegistryLedger(address(st)));
    }

    function test_TheShardIdIsRecomputedFromThePreimage() public {
        _reg();
        assertEq(reg.shardIdOf(_pre()), shard);
    }

    function test_AnAttestedFinalShardVerifiesWithItsLevelAndMetric() public {
        _reg();
        assertEq(_attest(4), shard);
        assertEq(reg.levelOf(ID, shard), 0, "not final yet");
        _finalize();
        assertEq(reg.levelOf(ID, shard), 4);
        (bool ok, uint8 level, bytes32 metric) =
            reg.verifyRowLeveled(ID, shard, rows[0], 0, 2, _sib());
        assertTrue(ok);
        assertEq(level, 4);
        assertEq(metric, METRIC);
        (ok, level, metric) =
            reg.verifyRowLeveled(ID, shard, bytes("row zero: forged"), 0, 2, _sib());
        assertFalse(ok);
        assertEq(level, 0, "a failed row carries no level");
        assertEq(metric, bytes32(0));
        OnyxRegistry.Attestation memory a = reg.attestationOf(ID, shard);
        assertEq(a.recipe, fields.recipeId);
        assertEq(a.labelSetHash, fields.labelSetHash);
        assertEq(a.fuelUsed, 1_000);
        assertEq(a.fuelLimit, LIMIT);
    }

    /// SER-R2-F1 / L2R2-F2: a shard opened without a preflight attestation never verifies here,
    /// and an attestation after the open is refused.
    function test_AnUnattestedShardNeverVerifiesAndLateAttestationIsRefused() public {
        _reg();
        _finalize();
        assertTrue(sx().verifyRow(ID, shard, rows[0], 0, 2, _sib()), "the ledger itself verifies");
        (bool ok, uint8 level,) = reg.verifyRowLeveled(ID, shard, rows[0], 0, 2, _sib());
        assertFalse(ok);
        assertEq(level, 0);
        vm.expectRevert(OnyxRegistry.ShardAlreadyOpen.selector);
        _attest(4);
    }

    /// REG-R1-F1 (was review proof `test_F1`): a shard that runs another recipe over a label set
    /// cannot be attested under a metric whose recipe differs, and no labelled shard is attested
    /// better than its label set's level.
    function test_AShardOfAnotherRecipeOrBetterThanItsLabelsIsRefused() public {
        reg = new OnyxRegistry(gov, IRegistryLedger(address(st)));
        vm.startPrank(gov);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.Recipe, keccak256("the registered recipe"), 0);
        vm.stopPrank();
        assertTrue(fields.recipeId != keccak256("the registered recipe"));
        // The real preimage carries the shard's real recipe: refused.
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.attestShard(ID, _pre(), runtimeBytes, METRIC, 1, 1);
        // Claiming the registered recipe in the preimage names another shard: the attestation
        // lands there, never on this one, so this shard stays unattested.
        OnyxRegistry.ShardPreimage memory lie = _pre();
        lie.recipeId = keccak256("the registered recipe");
        lie.labelSetHash = bytes32(0);
        bytes32 other = reg.attestShard(ID, lie, runtimeBytes, METRIC, 1, 1);
        assertTrue(other != shard, "a changed preimage is another shard");
        _finalize();
        (bool ok, uint8 level,) = reg.verifyRowLeveled(ID, shard, rows[0], 0, 2, _sib());
        assertFalse(ok);
        assertEq(level, 0, "the real shard has no level");
    }

    function test_ALabelledShardIsFlooredByItsLabelSet() public {
        reg = new OnyxRegistry(gov, IRegistryLedger(address(st)));
        vm.prank(gov);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.Recipe, fields.recipeId, 0);
        vm.expectRevert(OnyxRegistry.LabelSetUnregistered.selector);
        _attest(5);
        vm.prank(gov);
        reg.registerLabelSet(fields.labelSetHash, LABEL_LEVEL);
        vm.expectRevert(OnyxRegistry.BelowLabelLevel.selector);
        _attest(1);
        vm.expectRevert(OnyxRegistry.BelowLabelLevel.selector);
        _attest(3);
        _attest(LABEL_LEVEL);
        assertEq(reg.attestationOf(ID, shard).level, LABEL_LEVEL);
    }

    function test_AttestationRules() public {
        _reg();
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        _attest(4);
        OnyxRegistry.ShardPreimage memory p = _pre();
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.attestShard(ID, p, runtimeBytes, FEED_METRIC, 1, 4);
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.attestShard(ID, p, runtimeBytes, keccak256("unregistered"), 1, 4);
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        _attest(0);
        // REG-R1-F7: level 6 (an unstaked claim) is never a shard's level.
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        _attest(6);
        vm.expectRevert(OnyxRegistry.OverBudget.selector);
        reg.attestShard(ID, p, runtimeBytes, METRIC, LIMIT / 2 + 1, 4);
        // REG-R2-F1: the limit comes from the runtime the shard commits to.
        vm.expectRevert(OnyxRegistry.BadRuntime.selector);
        reg.attestShard(ID, p, _runtimeBorsh(LIMIT * 20), METRIC, 0, 4);
        reg.attestShard(ID, p, runtimeBytes, METRIC, LIMIT / 2, 5);
        vm.expectRevert(OnyxRegistry.AlreadyAttested.selector);
        _attest(4);
    }

    /// REG-R1-F2 (was review proof `test_F2`): the fact key is computed on chain from the
    /// registered template; a coordinator cannot pick the fact a substituted spec fills, and a
    /// consumer comparing its own instantiation never accepts a substituted spec.
    function test_TheFactIsComputedOnChainAndTheSpecIsCheckedByTheConsumer() public {
        _reg();
        bytes32 honestSpec = keccak256("instantiate(template, subject, window)");
        bytes32 rogueSpec = keccak256("generic json-pointer fetcher, 2001 record");
        bytes32 chosen = keccak256("whatever result the coordinator likes");
        _draw(FEED_METRIC, SUBJECT, WINDOW, rogueSpec);
        bytes32 fact =
            reg.recordObservation(FEED_METRIC, SUBJECT, WINDOW, rogueSpec, chosen, POOL, CFG);
        assertEq(fact, reg.factKeyOf(FEED_METRIC, FEED_TEMPLATE, SUBJECT, WINDOW, false));
        assertEq(reg.factKey(FEED_METRIC, SUBJECT, WINDOW), fact);
        assertEq(
            reg.observationLevel(FEED_METRIC, SUBJECT, WINDOW, honestSpec, chosen),
            0,
            "a consumer's own instantiation refuses the substituted spec"
        );
        assertEq(reg.observationLevel(FEED_METRIC, SUBJECT, WINDOW, rogueSpec, chosen), 2);
        // Another template id yields another fact: a substituted template never fills this one.
        assertTrue(
            reg.factKeyOf(FEED_METRIC, keccak256("rogue template"), SUBJECT, WINDOW, false) != fact
        );
    }

    /// L2R3-F5 across coordinators: one result per fact.
    function test_OneObservationPerFact() public {
        _reg();
        bytes32 spec = keccak256("spec");
        bytes32 r1 = keccak256("result 1");
        _draw(FEED_METRIC, SUBJECT, WINDOW, spec);
        reg.recordObservation(FEED_METRIC, SUBJECT, WINDOW, spec, r1, POOL, CFG);
        assertEq(reg.observationLevel(FEED_METRIC, SUBJECT, WINDOW, spec, r1), 2);
        assertEq(reg.observationLevel(FEED_METRIC, SUBJECT, WINDOW, spec, keccak256("r")), 0);
        assertEq(reg.observationLevel(FEED_METRIC, SUBJECT, WINDOW + 1, spec, r1), 0);
        assertEq(reg.observationLevel(FEED_METRIC, keccak256("s2"), WINDOW, spec, r1), 0);
        OnyxRegistry.Observation memory o = reg.observationOf(FEED_METRIC, SUBJECT, WINDOW);
        assertEq(o.result, r1);
        assertEq(o.template, FEED_TEMPLATE);
        vm.expectRevert(OnyxRegistry.AlreadyRecorded.selector);
        reg.recordObservation(FEED_METRIC, SUBJECT, WINDOW, spec, keccak256("r2"), POOL, CFG);
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.recordObservation(METRIC, SUBJECT, WINDOW, spec, keccak256("r3"), POOL, CFG);
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.recordObservation(
            keccak256("unregistered"), SUBJECT, WINDOW, spec, keccak256("r4"), POOL, CFG
        );
        vm.expectRevert(OnyxRegistry.ZeroValue.selector);
        reg.recordObservation(FEED_METRIC, bytes32(0), WINDOW, spec, keccak256("r5"), POOL, CFG);
        vm.expectRevert(OnyxRegistry.ZeroValue.selector);
        reg.recordObservation(FEED_METRIC, SUBJECT, 0, spec, keccak256("r5"), POOL, CFG);
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        reg.recordObservation(
            FEED_METRIC, keccak256("s6"), WINDOW, spec, keccak256("r6"), POOL, CFG
        );
    }

    /// A dated feed has one fact per subject whatever the window (L2SRC-R2-F2).
    function test_ADatedFactIgnoresTheWindow() public {
        _reg();
        bytes32 spec = keccak256("spec");
        _draw(DATED_METRIC, SUBJECT, WINDOW, spec);
        bytes32 fact =
            reg.recordObservation(DATED_METRIC, SUBJECT, WINDOW, spec, keccak256("r"), POOL, CFG);
        assertEq(reg.factKey(DATED_METRIC, SUBJECT, WINDOW + 3_600), fact);
        vm.expectRevert(OnyxRegistry.AlreadyRecorded.selector);
        reg.recordObservation(
            DATED_METRIC, SUBJECT, WINDOW + 3_600, spec, keccak256("r2"), POOL, CFG
        );
        OnyxRegistry.Observation memory o = reg.observationOf(DATED_METRIC, SUBJECT, 0);
        assertEq(o.windowEnd, WINDOW, "the consumer reads the window, then instantiates");
        assertEq(reg.observationLevel(DATED_METRIC, SUBJECT, WINDOW, spec, keccak256("r")), 2);
        assertEq(
            reg.observationLevel(DATED_METRIC, SUBJECT, WINDOW + 3_600, spec, keccak256("r")),
            0,
            "the recorded window must be the one instantiated"
        );
    }

    /// REG-R1-F5 (was review proof `test_F5`): equal values on two dates both record (the result
    /// hash binds the round, and no result is unique across facts any more).
    function test_EqualResultsOnTwoFactsBothRecord() public {
        _reg();
        bytes32 r = keccak256("value 38000000000000.00");
        _draw(DATED_METRIC, keccak256("2026-10-08"), WINDOW, keccak256("s1"));
        _draw(DATED_METRIC, keccak256("2026-10-09"), WINDOW, keccak256("s2"));
        reg.recordObservation(
            DATED_METRIC, keccak256("2026-10-08"), WINDOW, keccak256("s1"), r, POOL, CFG
        );
        reg.recordObservation(
            DATED_METRIC, keccak256("2026-10-09"), WINDOW, keccak256("s2"), r, POOL, CFG
        );
        assertEq(
            reg.observationLevel(DATED_METRIC, keccak256("2026-10-09"), WINDOW, keccak256("s2"), r),
            2
        );
    }

    /// REG-R2-F1 (was review proof `test_R2F1`): the fuel limit is read from the runtime the
    /// shard commits to; a run over half of it cannot be attested by claiming a larger limit.
    function test_TheFuelLimitIsTheCommittedRuntimes() public {
        _reg();
        assertEq(reg.runtimeFuelLimit(runtimeBytes), LIMIT);
        // 10x the pinned limit used: over budget whatever limit a caller would like to claim.
        vm.expectRevert(OnyxRegistry.OverBudget.selector);
        reg.attestShard(ID, _pre(), runtimeBytes, METRIC, LIMIT * 10, 4);
        // A runtime that claims 20x the limit is another runtime: its hash is not the shard's.
        vm.expectRevert(OnyxRegistry.BadRuntime.selector);
        reg.attestShard(ID, _pre(), _runtimeBorsh(LIMIT * 20), METRIC, LIMIT * 10, 4);
        // Malformed encodings never parse.
        vm.expectRevert(OnyxRegistry.BadRuntime.selector);
        reg.runtimeFuelLimit(hex"01000000");
        bytes memory longer = abi.encodePacked(runtimeBytes, hex"00");
        vm.expectRevert(OnyxRegistry.BadRuntime.selector);
        reg.runtimeFuelLimit(longer);
        reg.attestShard(ID, _pre(), runtimeBytes, METRIC, LIMIT / 2, 4);
        assertEq(reg.attestationOf(ID, shard).fuelLimit, LIMIT);
    }

    /// REG-R2-F2 (was review proof `test_R2F2`): the dated Treasury metric, registered with its
    /// template's own kind, has one fact per date: a second window for the same date is refused.
    function test_TheDatedTreasuryMetricHasOneFactPerDate() public {
        _reg();
        bytes32 dated = keccak256("us.treasury.federal_debt:2026-10-08");
        _draw(DATED_METRIC, dated, 1_791_706_200, keccak256("s1"));
        reg.recordObservation(
            DATED_METRIC, dated, 1_791_706_200, keccak256("s1"), keccak256("v1"), POOL, CFG
        );
        vm.expectRevert(OnyxRegistry.AlreadyRecorded.selector);
        reg.recordObservation(
            DATED_METRIC, dated, 1_791_792_600, keccak256("s2"), keccak256("v2"), POOL, CFG
        );
        assertEq(
            reg.observationLevel(
                DATED_METRIC, dated, 1_791_792_600, keccak256("s2"), keccak256("v2")
            ),
            0
        );
    }

    /// REG-R2-F3 (was review proof `test_R2F3`): reading the spec back from the chain and
    /// passing it to `observationLevel` is a tautology, so it proves nothing; a consumer passes
    /// its own instantiation (`onchain_observation_check`), which a rogue spec never matches.
    function test_ASpecReadBackFromTheChainProvesNothing() public {
        _reg();
        bytes32 rogue = keccak256("rogue spec");
        _draw(FEED_METRIC, SUBJECT, WINDOW, rogue);
        reg.recordObservation(FEED_METRIC, SUBJECT, WINDOW, rogue, keccak256("chosen"), POOL, CFG);
        OnyxRegistry.Observation memory o = reg.observationOf(FEED_METRIC, SUBJECT, WINDOW);
        assertEq(reg.observationLevel(FEED_METRIC, SUBJECT, WINDOW, o.spec, o.result), 2);
        assertEq(
            reg.observationLevel(
                FEED_METRIC, SUBJECT, WINDOW, keccak256("own instantiation"), o.result
            ),
            0
        );
    }

    /// REG-R2-F4 (was review proof `test_R2F4`, accepted residual): an unlabelled shard has no
    /// on-chain floor; its level is the coordinator's word (REG-2).
    function test_AnUnlabelledShardsLevelIsAttested() public {
        _reg();
        OnyxRegistry.ShardPreimage memory p = _pre();
        p.labelSetHash = bytes32(0);
        bytes32 s = reg.attestShard(ID, p, runtimeBytes, METRIC, 1, 1);
        assertEq(reg.attestationOf(ID, s).level, 1);
    }

    /// L2RND-R1-F1/F2: a level-2 draw is announced on chain before its beacon exists, attempts
    /// are strictly sequential, the beacon is the chain's block hash, a result needs a fixed
    /// draw, and `drawMatches` accepts only the latest attempt with exactly its pool, height and
    /// beacon (no cherry-picked attempt, no beacon tried offline).
    function test_DrawsAreAnnouncedBeforeTheirBeaconAndFixedFromTheChain() public {
        _reg();
        bytes32 fact = reg.factKey(FEED_METRIC, SUBJECT, WINDOW);
        bytes32 pool = reg.drawCommit(POOL, keccak256("s"), CFG);
        uint64 h = uint64(vm.getBlockNumber()) + 5;
        vm.expectRevert(OnyxRegistry.NoDraw.selector);
        reg.recordObservation(
            FEED_METRIC, SUBJECT, WINDOW, keccak256("s"), keccak256("r"), POOL, CFG
        );
        vm.expectRevert(OnyxRegistry.BeaconNotFuture.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, pool, uint64(vm.getBlockNumber()));
        vm.expectRevert(OnyxRegistry.BadAttempt.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, pool, h);
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, pool, h);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, pool, h);
        vm.expectRevert(OnyxRegistry.BadAttempt.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, keccak256("other pool"), h);
        vm.expectRevert(OnyxRegistry.BeaconNotFuture.selector);
        reg.fixBeacon(fact, 0);
        // Past the 256-block window the beacon is gone: the draw can never be fixed.
        vm.roll(h + 300);
        vm.expectRevert(OnyxRegistry.BeaconUnavailable.selector);
        reg.fixBeacon(fact, 0);
        vm.expectRevert(OnyxRegistry.NoDraw.selector);
        reg.recordObservation(
            FEED_METRIC, SUBJECT, WINDOW, keccak256("s"), keccak256("r"), POOL, CFG
        );
        // The next attempt, fixed in time.
        uint64 h1 = uint64(vm.getBlockNumber()) + 2;
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, pool, h1);
        bytes32 beacon = keccak256("block hash");
        vm.roll(h1 + 1);
        vm.setBlockhash(h1, beacon);
        assertEq(reg.fixBeacon(fact, 1), beacon);
        vm.expectRevert(OnyxRegistry.BeaconAlreadyFixed.selector);
        reg.fixBeacon(fact, 1);
        OnyxRegistry.Draw memory d = reg.drawOf(fact, 1);
        assertEq(d.commit, pool);
        assertEq(d.beaconHeight, h1);
        assertLt(d.announcedAt, d.beaconHeight);
        assertEq(d.beacon, beacon);
        assertTrue(reg.drawMatches(FEED_METRIC, SUBJECT, WINDOW, 1, pool, h1, beacon));
        assertFalse(
            reg.drawMatches(FEED_METRIC, SUBJECT, WINDOW, 0, pool, h, beacon), "old attempt"
        );
        assertFalse(reg.drawMatches(FEED_METRIC, SUBJECT, WINDOW, 1, keccak256("x"), h1, beacon));
        assertFalse(reg.drawMatches(FEED_METRIC, SUBJECT, WINDOW, 1, pool, h1, keccak256("tried")));
        assertFalse(reg.drawMatches(FEED_METRIC, SUBJECT, WINDOW, 1, pool, h1 + 1, beacon));
        reg.recordObservation(
            FEED_METRIC, SUBJECT, WINDOW, keccak256("s"), keccak256("r"), POOL, CFG
        );
        vm.expectRevert(OnyxRegistry.AlreadyRecorded.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 2, pool, uint64(vm.getBlockNumber()) + 1);
    }

    /// L2RND-R3-F1/F2 (was the reviewer's `R3PostBeaconGrind` proof): a dated draw is one fact
    /// for every window, so the window (in the spec id) and the config (`min_bond`) used to be
    /// free after the beacon. Now the announced draw commits pool, spec and config: every other
    /// window's spec and every other config is refused by `recordObservation` and
    /// `drawMatches`; exactly the committed one records.
    function test_ADrawReachesOnlyItsCommittedSpecAndConfig() public {
        _reg();
        bytes32 s = keccak256("dated subject");
        bytes32 committed = keccak256(abi.encode("spec@", WINDOW));
        bytes32 fact = _draw(DATED_METRIC, s, WINDOW, committed);
        uint32 last = reg.drawCount(fact) - 1;
        OnyxRegistry.Draw memory d = reg.drawOf(fact, last);
        for (uint64 i = 1; i < 1000; i += 97) {
            bytes32 other = keccak256(abi.encode("spec@", WINDOW + i));
            bytes32 c = reg.drawCommit(POOL, other, CFG);
            assertFalse(
                reg.drawMatches(DATED_METRIC, s, WINDOW + i, last, c, d.beaconHeight, d.beacon)
            );
            vm.expectRevert(OnyxRegistry.DrawMismatch.selector);
            reg.recordObservation(DATED_METRIC, s, WINDOW + i, other, keccak256("r"), POOL, CFG);
        }
        // Another config (another `min_bond`) or another pool: refused too.
        vm.expectRevert(OnyxRegistry.DrawMismatch.selector);
        reg.recordObservation(
            DATED_METRIC, s, WINDOW, committed, keccak256("r"), POOL, keccak256("cfg2")
        );
        vm.expectRevert(OnyxRegistry.DrawMismatch.selector);
        reg.recordObservation(
            DATED_METRIC, s, WINDOW, committed, keccak256("r"), keccak256("p2"), CFG
        );
        assertTrue(
            reg.drawMatches(
                DATED_METRIC,
                s,
                WINDOW,
                last,
                reg.drawCommit(POOL, committed, CFG),
                d.beaconHeight,
                d.beacon
            )
        );
        reg.recordObservation(DATED_METRIC, s, WINDOW, committed, keccak256("r"), POOL, CFG);
        assertEq(reg.observationLevel(DATED_METRIC, s, WINDOW, committed, keccak256("r")), 2);
    }

    /// L2RND-R1-F7: at most `MAX_DRAW_ATTEMPTS` (64) draws per fact, attempts 0..63, even with
    /// the owner's reopens granting the subject enough draws.
    function test_AFactTakesAtMost64Draws() public {
        _reg();
        bytes32 subject = keccak256("busy subject");
        vm.startPrank(gov);
        for (uint32 i = 0; i < 22; i++) {
            reg.reopenDraws(FEED_METRIC, subject);
        }
        vm.stopPrank();
        for (uint32 i = 0; i < 64; i++) {
            reg.announceDraw(
                FEED_METRIC, subject, WINDOW, i, keccak256("p"), uint64(vm.getBlockNumber()) + 1
            );
        }
        vm.expectRevert(OnyxRegistry.BadAttempt.selector);
        reg.announceDraw(
            FEED_METRIC, subject, WINDOW, 64, keccak256("p"), uint64(vm.getBlockNumber()) + 1
        );
    }

    function _announceAndFix(bytes32 subject, uint64 w, uint32 attempt, bytes32 beacon)
        internal
        returns (uint64 h)
    {
        h = uint64(vm.getBlockNumber()) + 1;
        reg.announceDraw(
            FEED_METRIC, subject, w, attempt, reg.drawCommit(POOL, keccak256("spec"), CFG), h
        );
        vm.roll(h + 1);
        vm.setBlockhash(h, beacon);
        reg.fixBeacon(reg.factKey(FEED_METRIC, subject, w), attempt);
    }

    /// L2RND-R2-F1 (was the reviewer's 64-try proof): dropping draws to grind public beacons
    /// buys at most `MAX_DRAWS` (3) tries per (metric, subject); the 4th announcement reverts
    /// until the owner records a reopen, which is public and grants exactly 3 more.
    function test_DroppedDrawsHitTheSubjectBoundAtTheFourthTry() public {
        _reg();
        bytes32 s = keccak256("subject");
        for (uint32 i = 0; i < 3; i++) {
            _announceAndFix(s, WINDOW, i, keccak256(abi.encode(i)));
        }
        (uint32 strikes, uint32 allowance) = reg.drawBudget(FEED_METRIC, s);
        assertEq(strikes, 3);
        assertEq(allowance, 3);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        vm.expectRevert(OnyxRegistry.DrawsExhausted.selector);
        reg.announceDraw(FEED_METRIC, s, WINDOW, 3, keccak256("pool"), h);
        // The coordinator cannot reopen; only the owner can, and it is logged.
        vm.expectRevert(OnyxRegistry.NotOwner.selector);
        reg.reopenDraws(FEED_METRIC, s);
        vm.prank(gov);
        reg.reopenDraws(FEED_METRIC, s);
        (, allowance) = reg.drawBudget(FEED_METRIC, s);
        assertEq(allowance, 6);
        _announceAndFix(s, WINDOW, 3, keccak256("b3"));
    }

    /// L2RND-R2-F2 (was the reviewer's cross-window proof): shifting a fixed feed's window makes
    /// a new fact, but every window shares the subject's bound, so 64 shifted windows are not 64
    /// free first draws: the 4th unrecorded draw of the subject reverts.
    function test_ShiftedWindowsShareTheSubjectBound() public {
        _reg();
        bytes32 s = keccak256("price subject");
        for (uint64 w = WINDOW; w < WINDOW + 3; w++) {
            _announceAndFix(s, w, 0, keccak256(abi.encode(w)));
        }
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        vm.expectRevert(OnyxRegistry.DrawsExhausted.selector);
        reg.announceDraw(FEED_METRIC, s, WINDOW + 3, 0, keccak256("pool"), h);
    }

    /// A draw that ends with a recorded value stops counting: a fixed feed observed window
    /// after window, each settling, never runs out; a dropped draw stays a strike.
    function test_RecordedDrawsDoNotCountAgainstTheSubject() public {
        _reg();
        bytes32 s = keccak256("steady subject");
        for (uint64 w = WINDOW; w < WINDOW + 10; w++) {
            _announceAndFix(s, w, 0, keccak256(abi.encode(w)));
            reg.recordObservation(
                FEED_METRIC, s, w, keccak256("spec"), keccak256(abi.encode(w)), POOL, CFG
            );
        }
        (uint32 strikes,) = reg.drawBudget(FEED_METRIC, s);
        assertEq(strikes, 0);
        _announceAndFix(s, WINDOW + 10, 0, keccak256("dropped"));
        (strikes,) = reg.drawBudget(FEED_METRIC, s);
        assertEq(strikes, 1);
    }

    /// The next attempt still needs no expired previous one (reviewer's F2 proof), but it is a
    /// strike: the subject bound, not the previous draw's state, limits re-draws.
    function test_EveryAnnouncedDrawIsAStrikeUntilRecorded() public {
        _reg();
        bytes32 s = keccak256("subject2");
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        reg.announceDraw(FEED_METRIC, s, WINDOW, 0, keccak256("pool"), h);
        vm.roll(h + 1);
        reg.announceDraw(FEED_METRIC, s, WINDOW, 1, keccak256("pool"), h + 2);
        (uint32 strikes,) = reg.drawBudget(FEED_METRIC, s);
        assertEq(strikes, 2);
    }

    function testFuzz_BudgetMatchesThePpmRule(uint64 used, uint64 limit) public {
        _reg();
        bool expect = limit != 0 && uint256(used) * 1_000_000 <= uint256(limit) * 500_000;
        assertEq(reg.withinBudget(used, limit), expect);
    }

    function testFuzz_AFactKeepsItsFirstResult(bytes32 subject, bytes32 a, bytes32 b) public {
        vm.assume(subject != bytes32(0) && a != bytes32(0) && b != bytes32(0) && a != b);
        _reg();
        _draw(FEED_METRIC, subject, WINDOW, keccak256("spec"));
        reg.recordObservation(FEED_METRIC, subject, WINDOW, keccak256("spec"), a, POOL, CFG);
        vm.expectRevert(OnyxRegistry.AlreadyRecorded.selector);
        reg.recordObservation(FEED_METRIC, subject, WINDOW, keccak256("spec"), b, POOL, CFG);
        assertEq(reg.observationLevel(FEED_METRIC, subject, WINDOW, keccak256("spec"), a), 2);
        assertEq(reg.observationLevel(FEED_METRIC, subject, WINDOW, keccak256("spec"), b), 0);
    }

    function testFuzz_ShardIdMatchesTheLibrary(OnyxRegistry.ShardPreimage memory p) public {
        _reg();
        OnyxCommit.CommitmentFields memory c = OnyxCommit.CommitmentFields({
            root: bytes32(0),
            leafCount: 0,
            recipeId: p.recipeId,
            runtimeHash: p.runtimeHash,
            labelSetHash: p.labelSetHash,
            sourceHash: p.sourceHash,
            schemaId: p.schemaId,
            rightsHash: p.rightsHash
        });
        assertEq(
            reg.shardIdOf(p),
            OnyxCommit.shardId(OnyxCommit.shardFieldsHash(c), p.paramsHash, p.registryRoot)
        );
    }

    /// AUD-2R-F3: a metric's registered minimum level floors every shard attested under it (a
    /// mining-pool shard's third-party pool table is level 4), whatever the coordinator claims.
    function test_AMetricsMinimumLevelFloorsItsShards() public {
        _reg();
        vm.expectRevert(OnyxRegistry.NotOwner.selector);
        reg.registerMetricMinLevel(METRIC, 5);
        vm.startPrank(gov);
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.registerMetricMinLevel(keccak256("unregistered metric"), 4);
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        reg.registerMetricMinLevel(METRIC, 6);
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        reg.registerMetricMinLevel(METRIC, 0);
        reg.registerMetricMinLevel(METRIC, 5);
        vm.expectRevert(OnyxRegistry.AlreadyRegistered.selector);
        reg.registerMetricMinLevel(METRIC, 4);
        vm.stopPrank();
        assertEq(reg.metricMinLevel(METRIC), 5);
        // The label set allows 4, the metric only 5: 4 is refused, 5 attests.
        vm.expectRevert(OnyxRegistry.BelowMetricLevel.selector);
        _attest(4);
        _attest(5);
    }

    /// AUD-2R-F3 / REG-F-F3 vectors: the metric keys, their recipes and minimum levels Rust
    /// registers (fixtures/registry/metric-min-levels.json), each in ONE `registerMetric` call.
    function test_MetricMinLevelVectors() public {
        _reg();
        string memory json = vm.readFile("../fixtures/registry/metric-min-levels.json");
        string[] memory ids = vm.parseJsonStringArray(json, ".metricIds");
        bytes32[] memory keys = vm.parseJsonBytes32Array(json, ".metricKeys");
        bytes32[] memory recipes = vm.parseJsonBytes32Array(json, ".recipeIds");
        uint256[] memory levels = vm.parseJsonUintArray(json, ".levels");
        assertEq(ids.length, keys.length);
        assertEq(ids.length, recipes.length);
        assertEq(ids.length, levels.length);
        assertGt(ids.length, 0);
        vm.startPrank(gov);
        for (uint256 i = 0; i < ids.length; i++) {
            assertEq(keccak256(bytes(ids[i])), keys[i]);
            assertEq(reg.metricKey(ids[i]), keys[i]);
            reg.registerMetric(keys[i], OnyxRegistry.Kind.Recipe, recipes[i], uint8(levels[i]));
            assertEq(reg.metricMinLevel(keys[i]), levels[i]);
            assertEq(reg.recipeMinLevel(recipes[i]), levels[i]);
        }
        vm.stopPrank();
    }

    /// REG-F-F1: a floor that lands after an attestation still applies when the level is read.
    function test_AFloorSetAfterAnAttestationStillApplies() public {
        _reg();
        _attest(4);
        vm.prank(gov);
        reg.registerMetricMinLevel(METRIC, 5);
        _finalize();
        assertEq(reg.levelOf(ID, shard), 5, "levelOf re-applies the recipe floor");
        (bool ok, uint8 level, bytes32 metric) =
            reg.verifyRowLeveled(ID, shard, rows[0], 0, 2, _sib());
        assertTrue(ok);
        assertEq(level, 5);
        assertEq(metric, METRIC);
    }

    /// REG-F-F1: the floor can be set in the same call that registers the metric.
    function test_RegisterMetricTakesItsFloorInOneCall() public {
        reg = new OnyxRegistry(gov, IRegistryLedger(address(st)));
        vm.startPrank(gov);
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        reg.registerMetric(FEED_METRIC, OnyxRegistry.Kind.Feed, FEED_TEMPLATE, 4);
        vm.expectRevert(OnyxRegistry.BadLevel.selector);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.Recipe, fields.recipeId, 6);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.Recipe, fields.recipeId, 5);
        reg.registerLabelSet(fields.labelSetHash, LABEL_LEVEL);
        vm.stopPrank();
        assertEq(reg.metricMinLevel(METRIC), 5);
        vm.expectRevert(OnyxRegistry.BelowMetricLevel.selector);
        _attest(4);
        _attest(5);
    }

    /// REG-F-F2: the floor is keyed by recipe: an alias metric with the same recipe cannot
    /// escape it, nor register a weaker floor for it.
    function test_AnAliasMetricWithTheSameRecipeCannotEscapeTheFloor() public {
        _reg();
        bytes32 alias_ = keccak256("crypto.btc.entity_flow.v2");
        vm.startPrank(gov);
        reg.registerMetricMinLevel(METRIC, 5);
        vm.expectRevert(OnyxRegistry.AlreadyRegistered.selector);
        reg.registerMetric(alias_, OnyxRegistry.Kind.Recipe, fields.recipeId, 4);
        reg.registerMetric(alias_, OnyxRegistry.Kind.Recipe, fields.recipeId, 0);
        vm.stopPrank();
        assertEq(reg.metricMinLevel(alias_), 5, "the alias inherits the recipe's floor");
        vm.expectRevert(OnyxRegistry.BelowMetricLevel.selector);
        reg.attestShard(ID, _pre(), runtimeBytes, alias_, 1_000, 4);
        reg.attestShard(ID, _pre(), runtimeBytes, alias_, 1_000, 5);
        _finalize();
        (bool ok, uint8 level, bytes32 metric) =
            reg.verifyRowLeveled(ID, shard, rows[0], 0, 2, _sib());
        assertTrue(ok);
        assertEq(level, 5);
        assertEq(metric, alias_);
    }

    /// REG-F-F3: a feed metric takes no floor.
    function test_AFeedMetricTakesNoFloor() public {
        _reg();
        vm.startPrank(gov);
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.registerMetricMinLevel(FEED_METRIC, 4);
        vm.expectRevert(OnyxRegistry.WrongTemplate.selector);
        reg.registerMetricMinLevel(DATED_METRIC, 4);
        vm.stopPrank();
        assertEq(reg.metricMinLevel(FEED_METRIC), 0);
    }
}

/// A ledger stub for the vectors test: only `coordinator` is read (by the constructor).
contract StubLedger {
    function coordinator() external pure returns (address) {
        return address(0xC0);
    }

    function coordinatorEpoch() external pure returns (uint64) {
        return 0;
    }
}

/// Rust <-> Solidity vectors (fixtures/registry/vectors.json); every array is read to its length.
contract OnyxRegistryVectorsTest is Test {
    function test_RustVectorsMatch() public {
        string memory json = vm.readFile("../fixtures/registry/vectors.json");
        OnyxRegistry reg = new OnyxRegistry(address(1), IRegistryLedger(address(new StubLedger())));
        assertEq(vm.parseJsonUint(json, ".preflightMaxFuelPpm"), reg.PREFLIGHT_MAX_FUEL_PPM());

        string[] memory ids = vm.parseJsonStringArray(json, ".metricIds");
        bytes32[] memory keys = vm.parseJsonBytes32Array(json, ".metricKeys");
        assertEq(ids.length, keys.length);
        assertGt(ids.length, 4);
        for (uint256 i = 0; i < ids.length; i++) {
            assertEq(reg.metricKey(ids[i]), keys[i], ids[i]);
        }

        string[] memory used = vm.parseJsonStringArray(json, ".budgetFuelUsed");
        string[] memory lim = vm.parseJsonStringArray(json, ".budgetFuelLimit");
        bool[] memory ok = vm.parseJsonBoolArray(json, ".budgetOk");
        assertEq(used.length, lim.length);
        assertEq(used.length, ok.length);
        for (uint256 i = 0; i < used.length; i++) {
            assertEq(
                reg.withinBudget(uint64(vm.parseUint(used[i])), uint64(vm.parseUint(lim[i]))), ok[i]
            );
        }

        // Shard ids: Rust's `ShardSpec::shard_id` against the on-chain recompute.
        bytes32[] memory sid = vm.parseJsonBytes32Array(json, ".shardIds");
        for (uint256 i = 0; i < sid.length; i++) {
            string memory p = string.concat(".shardPreimages[", vm.toString(i), "]");
            OnyxRegistry.ShardPreimage memory pre = OnyxRegistry.ShardPreimage({
                recipeId: vm.parseJsonBytes32(json, string.concat(p, ".recipeId")),
                runtimeHash: vm.parseJsonBytes32(json, string.concat(p, ".runtimeHash")),
                labelSetHash: vm.parseJsonBytes32(json, string.concat(p, ".labelSetHash")),
                sourceHash: vm.parseJsonBytes32(json, string.concat(p, ".sourceHash")),
                schemaId: vm.parseJsonBytes32(json, string.concat(p, ".schemaId")),
                rightsHash: vm.parseJsonBytes32(json, string.concat(p, ".rightsHash")),
                paramsHash: vm.parseJsonBytes32(json, string.concat(p, ".paramsHash")),
                registryRoot: vm.parseJsonBytes32(json, string.concat(p, ".registryRoot"))
            });
            assertEq(reg.shardIdOf(pre), sid[i]);
        }
        assertGt(sid.length, 0);

        // Fact keys: Rust's `registry_fact_key`, with the Treasury feed's template id by value.
        bytes32 tid = vm.parseJsonBytes32(json, ".treasuryFeed.templateId");
        bytes32 fmk = vm.parseJsonBytes32(json, ".treasuryFeed.metricKey");
        assertEq(reg.metricKey(vm.parseJsonString(json, ".treasuryFeed.metric")), fmk);
        bytes32[] memory subj = vm.parseJsonBytes32Array(json, ".factSubjectKeys");
        string[] memory win = vm.parseJsonStringArray(json, ".factWindowEnds");
        bool[] memory dated = vm.parseJsonBoolArray(json, ".factDated");
        bytes32[] memory fk = vm.parseJsonBytes32Array(json, ".factKeys");
        assertEq(subj.length, fk.length);
        assertGt(fk.length, 0);
        for (uint256 i = 0; i < fk.length; i++) {
            assertEq(
                reg.factKeyOf(fmk, tid, subj[i], uint64(vm.parseUint(win[i])), dated[i]), fk[i]
            );
        }
        // L2RND-R2-F1: the subject draw bound's key, Rust's `subject_draw_key`.
        bytes32[] memory sdk = vm.parseJsonBytes32Array(json, ".subjectDrawKeys");
        assertEq(sdk.length, subj.length);
        for (uint256 i = 0; i < sdk.length; i++) {
            assertEq(reg.subjectDrawKey(fmk, subj[i]), sdk[i]);
        }
        assertEq(sdk[0], sdk[1], "shifted windows share the bound");

        // REG-R2-F1: the fuel limit read from Rust's Borsh runtime bytes.
        bytes[] memory rts = vm.parseJsonBytesArray(json, ".runtimeBytes");
        string[] memory fuel = vm.parseJsonStringArray(json, ".runtimeFuelLimits");
        assertEq(rts.length, fuel.length);
        assertGt(rts.length, 0);
        for (uint256 i = 0; i < rts.length; i++) {
            assertEq(reg.runtimeFuelLimit(rts[i]), uint64(vm.parseUint(fuel[i])));
            assertEq(
                keccak256(rts[i]),
                vm.parseJsonBytes32(
                    json, string.concat(".shardPreimages[", vm.toString(i), "].runtimeHash")
                )
            );
        }

        // REG-R2-F2/F3: the Treasury metric registered with its template's own kind; the
        // honest coordinator records the round; a consumer's own rebuilt arguments give level 2,
        // a rogue spec gives 0.
        uint8 kind = uint8(vm.parseJsonUint(json, ".treasuryFeed.kind"));
        assertEq(kind, uint8(OnyxRegistry.Kind.DatedFeed));
        vm.prank(address(1));
        reg.registerMetric(fmk, OnyxRegistry.Kind(kind), tid, 0);
        bytes32 sk = vm.parseJsonBytes32(json, ".treasuryObservation.subjectKey");
        uint64 we = uint64(vm.parseUint(vm.parseJsonString(json, ".treasuryObservation.windowEnd")));
        bytes32 spec = vm.parseJsonBytes32(json, ".treasuryObservation.specId");
        bytes32 result = vm.parseJsonBytes32(json, ".treasuryObservation.result");
        _checkTreasuryDraw(reg, json, fmk, sk, we, spec, result);
    }

    /// The recorded Treasury round's draw vector (Rust's commitment, calldata and record).
    struct DrawVec {
        bytes32 commit;
        bytes32 poolHash;
        bytes32 cfgHash;
        uint64 h;
        bytes32 beacon;
        uint32 attempt;
    }

    function _checkTreasuryDraw(
        OnyxRegistry reg,
        string memory json,
        bytes32 fmk,
        bytes32 sk,
        uint64 we,
        bytes32 spec,
        bytes32 result
    ) internal {
        // L2RND-R1: the recorded round's draw, announced before its beacon height and fixed
        // from the block hash; Rust's `drawMatches` calldata is the contract's ABI and the
        // registry answers true for it, false for any other attempt, commitment or beacon.
        DrawVec memory v = DrawVec({
            commit: vm.parseJsonBytes32(json, ".treasuryDraw.commit"),
            poolHash: vm.parseJsonBytes32(json, ".treasuryDraw.poolHash"),
            cfgHash: vm.parseJsonBytes32(json, ".treasuryDraw.cfgHash"),
            h: uint64(vm.parseUint(vm.parseJsonString(json, ".treasuryDraw.beaconHeight"))),
            beacon: vm.parseJsonBytes32(json, ".treasuryDraw.beacon"),
            attempt: uint32(vm.parseJsonUint(json, ".treasuryDraw.attempt"))
        });
        // L2RND-R3: Rust's draw commitment is the contract's: pool hash, spec id, config hash.
        assertEq(reg.drawCommit(v.poolHash, spec, v.cfgHash), v.commit);
        assertEq(v.attempt, 0);
        vm.roll(v.h - 1);
        vm.prank(address(0xC0));
        reg.setReceiptSigner(address(0x5EC));
        vm.prank(address(0xC0));
        reg.announceDraw(fmk, sk, we, v.attempt, v.commit, v.h);
        vm.roll(v.h + 1);
        vm.setBlockhash(v.h, v.beacon);
        reg.fixBeacon(reg.factKey(fmk, sk, we), v.attempt);
        assertEq(
            abi.encodeCall(reg.drawMatches, (fmk, sk, we, v.attempt, v.commit, v.h, v.beacon)),
            vm.parseJsonBytes(json, ".treasuryDraw.drawMatchesCalldata")
        );
        assertTrue(reg.drawMatches(fmk, sk, we, v.attempt, v.commit, v.h, v.beacon));
        assertFalse(reg.drawMatches(fmk, sk, we, v.attempt + 1, v.commit, v.h, v.beacon));
        assertFalse(reg.drawMatches(fmk, sk, we, v.attempt, keccak256("other"), v.h, v.beacon));
        assertFalse(reg.drawMatches(fmk, sk, we, v.attempt, v.commit, v.h, keccak256("ground")));
        vm.prank(address(0xC0));
        vm.expectRevert(OnyxRegistry.DrawMismatch.selector);
        reg.recordObservation(fmk, sk, we, spec, result, v.poolHash, keccak256("other cfg"));
        vm.prank(address(0xC0));
        reg.recordObservation(fmk, sk, we, spec, result, v.poolHash, v.cfgHash);
        assertEq(reg.observationLevel(fmk, sk, we, spec, result), 2);
        assertEq(reg.observationLevel(fmk, sk, we, keccak256("rogue spec"), result), 0);
    }
}
