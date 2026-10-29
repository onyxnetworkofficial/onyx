// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxLedger} from "../src/OnyxLedger.sol";
import {OnyxSettlementTest} from "./OnyxSettlement.t.sol";
import {OnyxSettlement} from "../src/OnyxSettlement.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// Regression tests for the M6 loop 32 review (THREATS.md "Review findings, M6 loop 32").
/// Each started as the reviewer's proof of concept and now asserts the fixed behaviour.
contract OnyxSettlementRegressionTest is OnyxSettlementTest {
    /// M6-F1: a shard cannot finalize before the seed is drawn, so a late `drawSeed` can never
    /// void a settled shard and credit its bonds a second time.
    function test_F1_FinalizeWithoutSeedRefused() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        _reveal(m2, SALT2, fields.root);
        _openNoCanaries(); // base + 400, no seed drawn
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.finalizeShard(ID, shard);
        (uint128 s0,,,) = st.validators(vals[0]);
        assertEq(s0, STAKE, "nobody judged against seed 0");
    }

    /// M6-F2: a zero commit hash is refused, so it cannot bypass the duplicate check.
    function test_F2_ZeroCommitHashRefused() public {
        _open();
        bytes memory sig = _sign(mk1, bytes32(0), BOND);
        vm.expectRevert(OnyxLedger.BadCommit.selector);
        st.commit(ID, shard, m1, bytes32(0), BOND, sig);
    }

    /// M6-F1: after a missed beacon the round voids; each shard refunds its bonds exactly once.
    function test_F1_VoidRefundsOnceAndOnlyOpenShards() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().refundVoidedShard(ID, shard); // round not voided
        vm.roll(base + 201 + 257);
        st.drawSeed(ID);
        sx().refundVoidedShard(ID, shard);
        assertEq(st.balanceOf(m1), BOND);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().refundVoidedShard(ID, shard);
        assertEq(st.balanceOf(m1), BOND, "never twice");
    }

    /// M6-F5: a validator top-up just before `openRound` does not block it.
    function test_F5_TopUpDoesNotBlockOpenRound() public {
        OnyxRound.RoundConfig memory c = _cfg();
        _fund(vals[0], 1);
        vm.prank(vals[0]);
        st.stake(1);
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
    }

    /// M6-F4: the reward goes to the earliest commit among correct reveals, not the first
    /// reveal transaction (Rust pays in commit order).
    function test_F4_RewardFollowsCommitOrder() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        _reveal(m2, SALT2, fields.root);
        _reveal(m1, SALT1, fields.root);
        vm.roll(base + 202);
        st.drawSeed(ID);
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.roll(block.number + 200); // final (ADR-0024 item 2)
        dsp.release(ID, shard);
        assertEq(st.balanceOf(m1), BOND + 500, "earliest committer paid");
        assertEq(st.balanceOf(m2), BOND, "later committer: bond only");
    }

    /// M6-F6: a voided round cannot cost the coordinator its canary slash.
    function test_F6_CanaryDeadlineOnVoidedRoundRefused() public {
        _open();
        vm.roll(base + 201 + 257);
        st.drawSeed(ID); // voids
        vm.roll(base + 450);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().missCanaryDeadline(ID);
    }

    // ------------------------------------------------------------------ M6-F3

    /// A unanimous shard with no assigned Agree verdict does not finalize.
    function test_F3_NoAgreeStaysPending() public {
        _toVerdicts();
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
        (bytes32 root,,) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, bytes32(0));
    }

    function _openWith(OnyxRound.RoundConfig memory c) internal {
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
    }

    /// A verdict window that closes before any verdict could be committed is refused.
    function test_F3_ImpossibleVerdictWindowRefused() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.verdictCommitClose = c.beaconHeight + 1;
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        _openWith(c);
    }

    /// A sampling rate too low to deter a lazy miner is refused (Rust `deterrence_holds`).
    function test_F3_NonDeterringRateRefused() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.ratePpm = 1;
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        _openWith(c);
    }

    /// Validators must be deterred at the declared canary density (Rust `validators_deterred`).
    function test_F3_NoCanaryDensityRefused() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.minCanaryPpm = 0;
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        _openWith(c);
    }

    /// M5-L46-F3: the dispute settings pass Rust's `DisputeConfig::is_valid` (same cases as its
    /// unit test), or the round does not open.
    function test_L46F3_DisputeConfigMirrorsRust() public {
        OnyxRound.DisputeConfig memory ok = _cfg().dispute;
        OnyxRound.DisputeConfig[] memory bad = new OnyxRound.DisputeConfig[](10);
        for (uint256 i = 0; i < bad.length; i++) {
            bad[i] = OnyxRound.DisputeConfig(
                ok.challengeWindow,
                ok.minChallengeBond,
                ok.panelSize,
                ok.panelCommitWindow,
                ok.panelRevealWindow,
                ok.appealWindow,
                ok.maxAppeals,
                ok.panelFee,
                ok.panelSlash
            );
        }
        bad[0].panelSize = 4; // even panel
        bad[1].panelFee = 60; // slash 500 < 9 * 60: a lazy member profits
        bad[2].minChallengeBond = 39; // < fee 10 * (3 + 1)
        bad[3].maxAppeals = 5; // > MAX_APPEALS
        bad[4].challengeWindow = 150; // = commit + reveal + appeal windows
        bad[5].appealWindow = type(uint64).max; // the windows' sum overflows u64 in Rust
        bad[6].panelCommitWindow = 0;
        bad[7].panelSlash = 0;
        bad[8].minChallengeBond = 0;
        bad[9].panelSize = 0;
        for (uint256 i = 0; i < bad.length; i++) {
            OnyxRound.RoundConfig memory c = _cfg();
            c.dispute = bad[i];
            vm.expectRevert(OnyxLedger.BadConfig.selector);
            _openWith(c);
        }
        _openWith(_cfg());
    }

    /// A round needs at least MIN_ROUND_MEMBERS bonded validators.
    function test_F3_TooFewMembersRefused() public {
        vm.prank(vals[0]);
        sx().requestUnbond();
        for (uint256 i = 0; i < standbyList.length; i++) {
            vm.prank(standbyList[i]);
            sx().requestUnbond();
        }
        OnyxRound.RoundConfig memory c = _cfg();
        vm.expectRevert(OnyxLedger.TooFewMembers.selector);
        _openWith(c);
    }

    // ------------------------------------------------------------------ M6-F7

    /// A full round (64 shards x 32 commits) voids and refunds, each call under a 16.7M cap.
    function test_F7_FullRoundVoidsWithinGasCap() public {
        uint256 cap = 16_777_216;
        uint256 nShards = st.MAX_SHARDS();
        uint256 nMiners = st.MAX_COMMITS_PER_SHARD();
        bytes32[] memory ids = new bytes32[](nShards);
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](nShards);
        bytes32 fh = OnyxCommit.shardFieldsHash(fields);
        for (uint256 k = 0; k < nShards; k++) {
            bytes32 ph = keccak256(abi.encodePacked("params", k));
            ids[k] = OnyxCommit.shardId(fh, ph, keccak256("registry"));
            inits[k] = OnyxLedger.ShardInit(fh, ph, keccak256("registry"));
        }
        // Sort shard ids ascending, carrying their inits.
        for (uint256 i = 1; i < nShards; i++) {
            for (uint256 j = i; j > 0 && ids[j - 1] > ids[j]; j--) {
                (ids[j - 1], ids[j]) = (ids[j], ids[j - 1]);
                (inits[j - 1], inits[j]) = (inits[j], inits[j - 1]);
            }
        }
        st.openRound(ID, _cfg(), ids, inits);
        bytes32 tag = st.roundTag(ID);
        for (uint256 i = 0; i < nMiners; i++) {
            uint256 key = 0xC000 + i;
            address m = vm.addr(key);
            _fund(m, nShards * BOND);
            for (uint256 k = 0; k < nShards; k++) {
                bytes32 h = keccak256(abi.encodePacked("commit", k, i));
                bytes32 digest = OnyxSig.eip191Digest(OnyxRound.commitMessage(tag, ids[k], h, BOND));
                (uint8 v, bytes32 r, bytes32 sg) = vm.sign(key, digest);
                st.commit(ID, ids[k], m, h, BOND, abi.encodePacked(r, sg, v));
            }
        }
        vm.roll(base + 201 + 257);
        uint256 g = gasleft();
        st.drawSeed(ID);
        assertLt(g - gasleft(), cap, "void call bounded");
        uint256 worst;
        for (uint256 k = 0; k < nShards; k++) {
            g = gasleft();
            sx().refundVoidedShard(ID, ids[k]);
            uint256 used = g - gasleft();
            if (used > worst) worst = used;
        }
        assertLt(worst, cap, "each refund bounded");
        emit log_named_uint("worst refund gas", worst);
        for (uint256 i = 0; i < nMiners; i++) {
            assertEq(st.balanceOf(vm.addr(0xC000 + i)), nShards * BOND, "every bond back once");
        }
    }

    // ------------------------------------------------------------------ M6-F8

    function _fillWithSybils() internal returns (address[] memory sybils) {
        uint256 n = st.MAX_VALIDATORS() - 3 - standbyList.length;
        sybils = new address[](n);
        for (uint256 i = 0; i < n; i++) {
            address a = address(uint160(0x5000 + i));
            sybils[i] = a;
            _fund(a, 1_000);
            vm.prank(a);
            st.stake(1_000);
        }
        (address[] memory members,) = st.activeSet();
        assertEq(members.length, st.MAX_VALIDATORS());
    }

    /// A full set does not lock honest validators out: more stake evicts the lowest member,
    /// whose stake goes into normal unbonding.
    function test_F8_FullSetEvictsLowestStake() public {
        address[] memory sybils = _fillWithSybils();
        address honest = address(0xBEEF);
        _fund(honest, 2_000);
        uint256 g = gasleft();
        vm.prank(honest);
        st.stake(2_000);
        uint256 used = g - gasleft();
        assertLt(used, 2_000_000, "eviction bounded");
        (,,, bool active) = st.validators(honest);
        assertTrue(active);
        (,, uint64 unbondAt, bool evictedActive) = st.validators(sybils[0]);
        assertFalse(evictedActive, "lowest stake (lowest address on ties) evicted");
        assertEq(unbondAt, uint64(block.number) + 50);
        (address[] memory members,) = st.activeSet();
        assertEq(members.length, st.MAX_VALIDATORS());
        for (uint256 i = 1; i < members.length; i++) {
            assertLt(uint160(members[i - 1]), uint160(members[i]), "sorted by address");
        }
    }

    /// Equal stake does not evict: the newcomer must bring more than the lowest member.
    function test_F8_EqualStakeCannotEvict() public {
        _fillWithSybils();
        address late = address(0xCAFE);
        _fund(late, 1_000);
        vm.prank(late);
        vm.expectRevert(OnyxLedger.TooMany.selector);
        st.stake(1_000);
    }

    // ------------------------------------------------------------------ M6-F9

    /// The ledger id (and so every round tag and signed commit) binds the chain id.
    function test_F9_LedgerIdBindsChainId() public {
        bytes32 here = st.ledgerId();
        assertEq(here, OnyxRound.ledgerId(block.chainid, address(st)));
        vm.chainId(84_532);
        assertTrue(st.ledgerId() != here, "same address, other chain, other ledger");
    }
}
