// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxLedger} from "../src/OnyxLedger.sol";
import {OnyxSettlement, IERC20Min} from "../src/OnyxSettlement.sol";
import {OnyxSettlementExt} from "../src/OnyxSettlementExt.sol";
import {OnyxDisputes} from "../src/OnyxDisputes.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxMerkle} from "../src/OnyxMerkle.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxSig} from "../src/OnyxSig.sol";
import {MockUSDC, ReentrantToken, IWithdraw} from "./mocks/MockUSDC.sol";

/// Setup and helpers shared by the settlement and dispute suites.
abstract contract OnyxSettlementFixture is Test {
    MockUSDC internal usdc;
    OnyxSettlement internal st;
    OnyxDisputes internal dsp;

    uint256[3] internal vKeys = [uint256(0xA11), 0xA12, 0xA13];
    address[3] internal vals;
    /// Bonded validators who never give a verdict (`_standbys`).
    address[] internal standbyList;
    uint256 internal mk1 = 0xB01;
    uint256 internal mk2 = 0xB02;
    address internal m1;
    address internal m2;

    uint64 internal constant ID = 1;
    bytes32 internal constant SALT1 = keccak256("salt 1");
    bytes32 internal constant SALT2 = keccak256("salt 2");
    bytes32 internal constant CANARY_SALT = keccak256("canary salt");
    uint128 internal constant BOND = 1_000;
    uint128 internal constant STAKE = 5_000;

    bytes32 internal shard;
    OnyxCommit.CommitmentFields internal fields;
    bytes[] internal rows;
    /// Borsh `RuntimeProfile` the fixture shard commits to (its keccak is `fields.runtimeHash`):
    /// abi 1, host "onyx-host.0.5.0.wasmi.2.0.0", fuel 4e9, 64 MiB, 1 MiB output, depth 1024.
    bytes internal runtimeBytes;

    /// Little-endian `n`-byte encoding of `v`.
    function _leBytes(uint256 v, uint256 n) internal pure returns (bytes memory out) {
        out = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = bytes1(uint8(v >> (8 * i)));
        }
    }

    /// Borsh `RuntimeProfile` with the fixture's fields and fuel limit `fuel`.
    function _runtimeBorsh(uint64 fuel) internal pure returns (bytes memory) {
        bytes memory host = bytes("onyx-host.0.5.0.wasmi.2.0.0");
        return abi.encodePacked(
            _leBytes(1, 4),
            _leBytes(host.length, 4),
            host,
            _leBytes(fuel, 8),
            _leBytes(64 << 20, 8),
            _leBytes(1 << 20, 4),
            _leBytes(1_024, 4)
        );
    }
    uint64 internal base;

    function setUp() public {
        usdc = new MockUSDC();
        dsp = new OnyxDisputes();
        st = new OnyxSettlement(
            IERC20Min(address(usdc)),
            address(this),
            address(dsp),
            "onyx.devnet",
            50,
            1_000,
            1_000,
            500
        );
        dsp.bind(sx());
        for (uint256 i = 0; i < 3; i++) {
            vals[i] = vm.addr(vKeys[i]);
            _fund(vals[i], STAKE);
            vm.prank(vals[i]);
            st.stake(STAKE);
        }
        // Bonded standbys who give no verdict: unanimity needs a drawable dispute panel plus a
        // challenger from the same pool (Rust L29-F4, M6-L2-F1).
        for (uint256 i = 0; i < _standbys(); i++) {
            address sb = vm.addr(0xD00 + i);
            _fund(sb, STAKE);
            vm.prank(sb);
            st.stake(STAKE);
            standbyList.push(sb);
        }
        m1 = vm.addr(mk1);
        m2 = vm.addr(mk2);
        _fund(m1, 10 * BOND);
        _fund(m2, 10 * BOND);
        _fund(address(this), 1_000_000);
        sx().postCoordinatorBond(2_000);
        st.fundRewards(100_000);

        rows.push(bytes("row zero: entity:okx"));
        rows.push(bytes("row one: entity:bitwise-bitb"));
        bytes32 top =
            OnyxMerkle.nodeHash(OnyxMerkle.leafHash(rows[0]), OnyxMerkle.leafHash(rows[1]));
        runtimeBytes = _runtimeBorsh(4_000_000_000);
        fields = OnyxCommit.CommitmentFields({
            root: OnyxMerkle.finalRoot(2, top),
            leafCount: 2,
            recipeId: keccak256("recipe"),
            runtimeHash: keccak256(runtimeBytes),
            labelSetHash: keccak256("labels"),
            sourceHash: keccak256("source"),
            schemaId: keccak256("schema"),
            rightsHash: keccak256("rights")
        });
        shard = OnyxCommit.shardId(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        base = uint64(block.number);
    }

    /// Standby validators staked in `setUp` (suites that manage their own panel pool use 0).
    function _standbys() internal view virtual returns (uint256) {
        return 4;
    }

    function _isStandby(address v) internal view returns (bool) {
        for (uint256 i = 0; i < standbyList.length; i++) {
            if (standbyList[i] == v) return true;
        }
        return false;
    }

    /// What the settlement owes the standbys (stake and credit), for solvency checks.
    function _standbyLiabilities() internal view returns (uint256 sum) {
        for (uint256 i = 0; i < standbyList.length; i++) {
            (uint128 sk,,,) = st.validators(standbyList[i]);
            sum += sk + st.balanceOf(standbyList[i]);
        }
    }

    /// The settlement's extension functions, called at the settlement's address (ADR-0024).
    function sx() internal view returns (OnyxSettlementExt) {
        return OnyxSettlementExt(address(st));
    }

    /// Pay out a finalized shard's held miner bonds and reward once its root is final
    /// (ADR-0024 item 2), rolling past the challenge window if needed.
    function _releaseIfFinalized() internal {
        (OnyxLedger.ShardStatus status, bool closed, uint64 at,,) = sx().shardState(ID, shard);
        if (status != OnyxLedger.ShardStatus.Finalized || closed || dsp.released(ID, shard)) {
            return;
        }
        if (block.number < uint256(at) + 200) vm.roll(uint256(at) + 200);
        dsp.release(ID, shard);
    }

    function _fund(address who, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.prank(who);
        usdc.approve(address(st), type(uint256).max);
    }

    function _cfg() internal view returns (OnyxRound.RoundConfig memory c) {
        (address[] memory members, uint128[] memory bonds) = st.activeSet();
        OnyxRound.Canary[] memory none = new OnyxRound.Canary[](0);
        c.commitClose = base + 100;
        c.revealClose = base + 200;
        c.minBond = BOND;
        c.validatorSetRoot = OnyxRound.validatorSetRoot(members, bonds);
        c.canaryCommit = OnyxRound.canaryCommit(none, CANARY_SALT);
        c.beaconHeight = base + 201;
        c.validation = OnyxRound.ValidationConfig({
            verdictCommitClose: base + 300,
            verdictRevealClose: base + 400,
            canaryOpenClose: base + 450,
            pendingClose: base + 600,
            ratePpm: 1_000_000,
            minerReward: 500,
            // 10, or less at the 128-member bounds: minBond covers a fee per member (L2-R2).
            validatorFee: uint128(members.length * 10 > BOND ? BOND / members.length : 10),
            validatorSlash: 2_000,
            missedSlash: 100,
            minCanaryPpm: 10_000
        });
        c.dispute = OnyxRound.DisputeConfig({
            challengeWindow: 200,
            minChallengeBond: 1_000,
            panelSize: 3,
            panelCommitWindow: 50,
            panelRevealWindow: 50,
            appealWindow: 50,
            maxAppeals: 1,
            panelFee: 10,
            panelSlash: 500
        });
    }

    function _open() internal {
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit({
            fieldsHash: OnyxCommit.shardFieldsHash(fields),
            paramsHash: keccak256("params"),
            registryRoot: keccak256("registry")
        });
        st.openRound(ID, _cfg(), ids, inits);
    }

    function _commitHash(address miner, bytes32 salt, bytes32 root)
        internal
        view
        returns (bytes32)
    {
        OnyxCommit.CommitmentFields memory f = fields;
        f.root = root;
        return OnyxCommit.commitHash(ID, shard, OnyxCommit.bindingHash(f), miner, salt);
    }

    function _sign(uint256 key, bytes32 commitHash, uint128 bond)
        internal
        view
        returns (bytes memory)
    {
        bytes32 digest = OnyxSig.eip191Digest(
            OnyxRound.commitMessage(st.roundTag(ID), shard, commitHash, bond)
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _commit(uint256 key, address miner, bytes32 salt, bytes32 root) internal {
        bytes32 h = _commitHash(miner, salt, root);
        st.commit(ID, shard, miner, h, BOND, _sign(key, h, BOND));
    }

    function _reveal(address miner, bytes32 salt, bytes32 root) internal {
        OnyxCommit.CommitmentFields memory f = fields;
        f.root = root;
        st.reveal(ID, shard, miner, f, salt);
    }

    /// Open, two honest commits and reveals, seed drawn.
    function _toVerdicts() internal {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        _reveal(m2, SALT2, fields.root);
        vm.roll(base + 202);
        st.drawSeed(ID);
    }

    /// M6-G4: the drawn seed is keccak256("onyx-seed-evm-v1" || round tag ||
    /// blockhash(beaconHeight)), the form the Rust coordinator now uses (`sample_seed`).
    function test_DrawnSeedIsTagAndBlockhash() public {
        _toVerdicts();
        bytes32 want =
            keccak256(abi.encodePacked("onyx-seed-evm-v1", st.roundTag(ID), blockhash(base + 201)));
        assertTrue(blockhash(base + 201) != bytes32(0), "beacon block is recent");
        assertEq(st.seedOf(ID), want);
    }

    function _vsalt(address v, address m) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("vsalt", v, m));
    }

    /// Every validator commits `kind` on every reveal, then (optionally) reveals.
    function _vote(uint8 kind, bytes32 root, bool revealAll) internal {
        bytes32 seed = st.seedOf(ID);
        address[2] memory miners = [m1, m2];
        for (uint256 i = 0; i < 3; i++) {
            for (uint256 j = 0; j < 2; j++) {
                bytes32 h = OnyxRound.verdictCommitHash(
                    seed, vals[i], shard, miners[j], kind, root, _vsalt(vals[i], miners[j])
                );
                vm.prank(vals[i]);
                st.commitVerdict(ID, shard, miners[j], h);
            }
        }
        if (!revealAll) return;
        vm.roll(base + 300);
        for (uint256 i = 0; i < 3; i++) {
            for (uint256 j = 0; j < 2; j++) {
                vm.prank(vals[i]);
                st.revealVerdict(ID, shard, miners[j], kind, root, _vsalt(vals[i], miners[j]));
            }
        }
    }

    /// Every validator commits and reveals Agree on one miner's reveal only.
    function _agreeOn(address miner) internal {
        bytes32 seed = st.seedOf(ID);
        for (uint256 i = 0; i < 3; i++) {
            bytes32 h = OnyxRound.verdictCommitHash(
                seed, vals[i], shard, miner, OnyxRound.AGREE, bytes32(0), _vsalt(vals[i], miner)
            );
            vm.prank(vals[i]);
            st.commitVerdict(ID, shard, miner, h);
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < 3; i++) {
            vm.prank(vals[i]);
            st.revealVerdict(ID, shard, miner, OnyxRound.AGREE, bytes32(0), _vsalt(vals[i], miner));
        }
    }

    function _openNoCanaries() internal {
        vm.roll(base + 400);
        OnyxRound.Canary[] memory none = new OnyxRound.Canary[](0);
        sx().openCanaries(ID, none, CANARY_SALT);
    }
}

contract OnyxSettlementTest is OnyxSettlementFixture {
    // ------------------------------------------------------------------ happy path

    function test_UnanimousShardFinalizesAndPays() public {
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        uint64 finalizedAt = uint64(block.number);
        for (uint256 i = 0; i < 3; i++) {
            assertEq(st.balanceOf(vals[i]), 10, "one fee per validator per shard");
        }

        // ADR-0024 item 2: the root is not final while it can still be challenged, and the
        // miners' reward and bonds wait in escrow until it is.
        (bytes32 root,,) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, bytes32(0), "not served inside the challenge window");
        assertFalse(sx().isFinal(ID, shard));
        assertEq(st.balanceOf(m1), 0, "reward held");
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.release(ID, shard);

        vm.roll(finalizedAt + 200); // challenge window over
        assertTrue(sx().isFinal(ID, shard));
        bytes32 binding;
        uint64 at;
        (root, binding, at) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, fields.root);
        assertEq(binding, OnyxCommit.bindingHash(fields));
        assertEq(at, finalizedAt);
        dsp.release(ID, shard);
        assertEq(st.balanceOf(m1), BOND + 500, "first reveal: bond + reward");
        assertEq(st.balanceOf(m2), BOND, "second reveal: bond back");
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.release(ID, shard);

        // Consumers verify rows against the finalized root.
        bytes32[] memory sib = new bytes32[](1);
        sib[0] = OnyxMerkle.leafHash(rows[1]);
        assertTrue(sx().verifyRow(ID, shard, rows[0], 0, 2, sib));
        assertFalse(sx().verifyRow(ID, shard, bytes("row zero: entity:okx, 1 sat more"), 0, 2, sib));

        // Pull payments.
        uint256 before = usdc.balanceOf(m1);
        vm.prank(m1);
        st.withdraw();
        assertEq(usdc.balanceOf(m1) - before, BOND + 500);
        assertEq(st.balanceOf(m1), 0);

        // The finalized shard id is never offered again.
        assertTrue(st.everFinalized(shard));
    }

    function test_BudgetReclaimedAfterFinalization() public {
        uint256 poolBefore = st.rewardPool();
        _toVerdicts();
        // Reward, every member's fee and one rerun panel's fees (ADR-0024 item 4).
        assertEq(st.rewardPool(), poolBefore - (500 + 10 * (3 + standbyList.length) + 10 * 3));
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().reclaimBudget(ID); // the held reward is still owed
        vm.roll(block.number + 200);
        dsp.release(ID, shard);
        sx().reclaimBudget(ID);
        assertEq(st.rewardPool(), poolBefore - 530);
        assertEq(sx().roundBudget(ID), 0);
    }

    // ------------------------------------------------------------------ commit rules

    function test_CommitAfterDeadlineRefused() public {
        _open();
        vm.roll(base + 100);
        bytes32 h = _commitHash(m1, SALT1, fields.root);
        bytes memory sig = _sign(mk1, h, BOND);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.commit(ID, shard, m1, h, BOND, sig);
    }

    function test_CommitWithOtherKeyRefused() public {
        _open();
        bytes32 h = _commitHash(m1, SALT1, fields.root);
        bytes memory sig = _sign(mk2, h, BOND);
        vm.expectRevert(OnyxLedger.BadSignature.selector);
        st.commit(ID, shard, m1, h, BOND, sig);
    }

    function test_CommitSignatureBindsTheBond() public {
        _open();
        bytes32 h = _commitHash(m1, SALT1, fields.root);
        bytes memory sig = _sign(mk1, h, BOND);
        vm.expectRevert(OnyxLedger.BadSignature.selector);
        st.commit(ID, shard, m1, h, BOND + 1, sig);
    }

    function test_HighSCommitSignatureRefused() public {
        _open();
        bytes32 h = _commitHash(m1, SALT1, fields.root);
        bytes32 digest =
            OnyxSig.eip191Digest(OnyxRound.commitMessage(st.roundTag(ID), shard, h, BOND));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(mk1, digest);
        uint256 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
        bytes memory twin = abi.encodePacked(r, bytes32(n - uint256(s)), v == 27 ? 28 : 27);
        vm.expectRevert(OnyxLedger.BadSignature.selector);
        st.commit(ID, shard, m1, h, BOND, twin);
    }

    function test_DoubleCommitRefused() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        bytes32 h = _commitHash(m1, SALT2, fields.root);
        bytes memory sig = _sign(mk1, h, BOND);
        vm.expectRevert(OnyxLedger.DuplicateCommit.selector);
        st.commit(ID, shard, m1, h, BOND, sig);
    }

    function test_BondBelowMinimumRefused() public {
        _open();
        bytes32 h = _commitHash(m1, SALT1, fields.root);
        bytes memory sig = _sign(mk1, h, BOND - 1);
        vm.expectRevert(OnyxLedger.BondTooLow.selector);
        st.commit(ID, shard, m1, h, BOND - 1, sig);
    }

    function test_BondIsEscrowed() public {
        _open();
        uint256 before = usdc.balanceOf(m1);
        _commit(mk1, m1, SALT1, fields.root);
        assertEq(before - usdc.balanceOf(m1), BOND);
    }

    // ------------------------------------------------------------------ reveal rules

    function test_RevealBeforeCommitCloseRefused() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        OnyxCommit.CommitmentFields memory f = fields;
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.reveal(ID, shard, m1, f, SALT1);
    }

    function test_RevealWithWrongSaltRefused() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        OnyxCommit.CommitmentFields memory f = fields;
        vm.expectRevert(OnyxLedger.RevealMismatch.selector);
        st.reveal(ID, shard, m1, f, SALT2);
    }

    function test_RevealWithOtherFieldsRefused() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        OnyxCommit.CommitmentFields memory f = fields;
        f.labelSetHash = keccak256("smaller label set");
        vm.expectRevert(OnyxLedger.RevealMismatch.selector);
        st.reveal(ID, shard, m1, f, SALT1);
    }

    function test_UnrevealedCommitForfeitsItsBond() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        vm.roll(base + 202);
        st.drawSeed(ID);
        _agreeOn(m1);
        _openNoCanaries();
        uint256 burnedBefore = st.burned();
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        assertEq(st.balanceOf(m2), 0);
        assertEq(st.burned() - burnedBefore >= BOND, true);
    }

    // ------------------------------------------------------------------ beacon

    function test_SeedCannotBeDrawnBeforeTheBeaconBlock() public {
        _open();
        vm.roll(base + 201);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.drawSeed(ID);
    }

    function test_MissedBeaconWindowVoidsTheRoundAndReturnsEverything() public {
        uint256 poolBefore = st.rewardPool();
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        vm.roll(base + 201 + 257);
        st.drawSeed(ID);
        assertEq(st.rewardPool(), poolBefore, "budget back at once");
        assertEq(st.balanceOf(m1), 0, "bonds come back per shard (M6-F7)");
        sx().refundVoidedShard(ID, shard);
        assertEq(st.balanceOf(m1), BOND);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Voided));
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().refundVoidedShard(ID, shard);
    }

    // ------------------------------------------------------------------ verdicts

    function test_VerdictFromNonMemberRefused() public {
        _toVerdicts();
        vm.prank(address(0xdead));
        vm.expectRevert(OnyxLedger.NotMember.selector);
        st.commitVerdict(ID, shard, m1, keccak256("x"));
    }

    function test_VerdictRevealMustMatchCommit() public {
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), false);
        vm.roll(base + 300);
        vm.prank(vals[0]);
        vm.expectRevert(OnyxLedger.VerdictMismatch.selector);
        st.revealVerdict(ID, shard, m1, OnyxRound.ABSTAIN, bytes32(0), _vsalt(vals[0], m1));
    }

    function test_DisagreeMakesShardPendingThenVoidReturnsBonds() public {
        _toVerdicts();
        _vote(OnyxRound.DISAGREE, keccak256("another root"), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
        (bytes32 root,,) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, bytes32(0));
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.voidShard(ID, shard);
        vm.roll(base + 600);
        st.voidShard(ID, shard);
        assertEq(st.balanceOf(m1), BOND);
        assertEq(st.balanceOf(m2), BOND);
    }

    function test_SilentAssignedValidatorIsSlashedOnce() public {
        _toVerdicts();
        // Only validators 0 and 1 vote; validator 2 stays silent on both reveals.
        bytes32 seed = st.seedOf(ID);
        address[2] memory miners = [m1, m2];
        for (uint256 i = 0; i < 2; i++) {
            for (uint256 j = 0; j < 2; j++) {
                bytes32 h = OnyxRound.verdictCommitHash(
                    seed, vals[i], shard, miners[j], 0, bytes32(0), _vsalt(vals[i], miners[j])
                );
                vm.prank(vals[i]);
                st.commitVerdict(ID, shard, miners[j], h);
            }
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < 2; i++) {
            for (uint256 j = 0; j < 2; j++) {
                vm.prank(vals[i]);
                st.revealVerdict(ID, shard, miners[j], 0, bytes32(0), _vsalt(vals[i], miners[j]));
            }
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        (uint128 stake2,,,) = st.validators(vals[2]);
        assertEq(stake2, STAKE - 100, "missed slash once per shard");
        assertEq(st.balanceOf(vals[2]), 0);
        assertEq(st.balanceOf(vals[0]), 10);
    }

    function test_UnrevealedVerdictIsSlashedOnceNotAlsoAsMissed() public {
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), false);
        vm.roll(base + 300);
        for (uint256 i = 0; i < 2; i++) {
            for (uint256 j = 0; j < 2; j++) {
                address m = j == 0 ? m1 : m2;
                vm.prank(vals[i]);
                st.revealVerdict(ID, shard, m, 0, bytes32(0), _vsalt(vals[i], m));
            }
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        // Two unrevealed commits (m1, m2): slashed once each at round level, as Rust does
        // (M6-F7), and not also as a missed task.
        (uint128 stake2,,,) = st.validators(vals[2]);
        assertEq(stake2, STAKE - 4_000);
        assertEq(st.unrevealedVerdicts(ID, vals[2]), 2);
    }

    // ------------------------------------------------------------------ canaries

    function test_CanaryDeadlineMissedSlashesCoordinator() public {
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), true);
        vm.roll(base + 400);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.finalizeShard(ID, shard);
        vm.roll(base + 450);
        uint128 bondBefore = st.coordinatorBond();
        sx().missCanaryDeadline(ID);
        assertEq(bondBefore - st.coordinatorBond(), 500);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
    }

    function test_CanaryListMustMatchItsCommitment() public {
        _toVerdicts();
        vm.roll(base + 400);
        OnyxRound.Canary[] memory list = new OnyxRound.Canary[](1);
        list[0] = OnyxRound.Canary({shard: shard, miner: m1, root: fields.root});
        vm.expectRevert(OnyxLedger.CanaryMismatch.selector);
        sx().openCanaries(ID, list, CANARY_SALT);
    }

    // ------------------------------------------------------------------ registry

    function test_ValidatorSetIsTheWholeRegistrySnapshot() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validatorSetRoot = keccak256("a set the coordinator picked");
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
        (address[] memory members, uint128[] memory bonds) = st.activeSet();
        assertEq(sx().roundSetRoot(ID), OnyxRound.validatorSetRoot(members, bonds));
        assertEq(st.roundMembers(ID).length, 3 + standbyList.length);
    }

    function test_UnbondingWaitsForDelayAndRoundLock() public {
        // Locks every member until pendingClose + dispute tail (ADR-0024 item 3): 600 + 2 x 200
        // + 2 x (256 + 50 + 50 + 50) + SETTLE_MARGIN 64 = base + 1876.
        _open();
        vm.prank(vals[0]);
        sx().requestUnbond();
        (,, uint64 unbondAt,) = st.validators(vals[0]);
        assertEq(unbondAt, base + 1876, "round lock beats the 50-block delay");
        vm.roll(base + 1875);
        vm.prank(vals[0]);
        vm.expectRevert(OnyxLedger.Locked.selector);
        sx().withdrawStake();
        vm.roll(base + 1876);
        vm.prank(vals[0]);
        sx().withdrawStake();
        assertEq(st.balanceOf(vals[0]), STAKE);
        (address[] memory members,) = st.activeSet();
        assertEq(members.length, 2 + standbyList.length);
    }

    function test_UnbondWithoutRoundUsesDelay() public {
        vm.prank(vals[1]);
        sx().requestUnbond();
        (,, uint64 unbondAt,) = st.validators(vals[1]);
        assertEq(unbondAt, uint64(block.number) + 50);
    }

    function test_OnlyCoordinatorOpensRounds() public {
        bytes32[] memory ids = new bytes32[](0);
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](0);
        OnyxRound.RoundConfig memory c = _cfg();
        vm.prank(address(0xbad));
        vm.expectRevert(OnyxLedger.NotCoordinator.selector);
        st.openRound(ID, c, ids, inits);
    }

    function test_BadConfigRefused() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.revealClose = c.commitClose;
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        st.openRound(ID, c, ids, inits);
    }

    function test_RoundIdUsedOnce() public {
        _open();
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        OnyxRound.RoundConfig memory c = _cfg();
        vm.expectRevert(OnyxLedger.RoundExists.selector);
        st.openRound(ID, c, ids, inits);
    }

    // ------------------------------------------------------------------ reentrancy

    function test_WithdrawCannotBeReentered() public {
        ReentrantToken bad = new ReentrantToken();
        OnyxSettlement s2 = new OnyxSettlement(
            IERC20Min(address(bad)), address(this), address(0), "onyx.devnet", 50, 1, 1, 1
        );
        address v = address(0x1234);
        bad.mint(v, 100);
        vm.prank(v);
        bad.approve(address(s2), 100);
        vm.prank(v);
        s2.stake(100);
        vm.prank(v);
        OnyxSettlementExt(address(s2)).requestUnbond();
        vm.roll(block.number + 50);
        vm.prank(v);
        OnyxSettlementExt(address(s2)).withdrawStake();
        bad.mint(address(s2), 100); // extra tokens a reentrant double payout would take
        bad.arm(IWithdraw(address(s2)));
        vm.prank(v);
        vm.expectRevert(OnyxLedger.Reentrancy.selector);
        s2.withdraw();
        assertEq(s2.balanceOf(v), 100, "nothing paid, nothing lost");
    }
}
