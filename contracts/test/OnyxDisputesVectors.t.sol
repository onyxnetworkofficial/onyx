// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxDisputes} from "../src/OnyxDisputes.sol";
import {OnyxRound} from "../src/OnyxRound.sol";

/// Calls the calldata-only config check the way `openRound` does.
contract DisputeConfigProbe {
    function valid(OnyxRound.DisputeConfig calldata d) external pure returns (bool) {
        return OnyxRound.disputeConfigValid(d);
    }
}

/// Rust and Solidity agree on the dispute game's rules and hashes (ADR-0024). Vectors are the
/// `disputes` section of fixtures/settlement/vectors.json, written by
/// adapters/source-btc/tests/evm_vectors.rs.
contract OnyxDisputesVectorsTest is Test {
    string internal json;
    OnyxDisputes internal dsp;
    DisputeConfigProbe internal probe;
    bytes32 internal ledger;

    function setUp() public {
        json = vm.readFile("../fixtures/settlement/vectors.json");
        dsp = new OnyxDisputes();
        probe = new DisputeConfigProbe();
        ledger = vm.parseJsonBytes32(json, ".ledgerId");
    }

    function _u(string memory key) internal view returns (uint256[] memory) {
        return vm.parseJsonUintArray(json, string.concat(".disputes.configValid.", key));
    }

    /// M5-L49-F5: `disputeConfigValid` equals Rust `DisputeConfig::is_valid` at every boundary
    /// (zero windows, equal sums, fee and bond ratios, appeal cap, panel cap, overflow).
    function test_DisputeConfigValidityMatchesRust() public view {
        uint256[] memory cw = _u("challengeWindow");
        uint256[] memory bond = _u("minChallengeBond");
        uint256[] memory size = _u("panelSize");
        uint256[] memory pcw = _u("panelCommitWindow");
        uint256[] memory prw = _u("panelRevealWindow");
        uint256[] memory aw = _u("appealWindow");
        uint256[] memory ap = _u("maxAppeals");
        uint256[] memory fee = _u("panelFee");
        uint256[] memory slash = _u("panelSlash");
        bool[] memory valid = vm.parseJsonBoolArray(json, ".disputes.configValid.valid");
        string[] memory names = vm.parseJsonStringArray(json, ".disputes.configValid.names");
        assertEq(valid.length, 24);
        for (uint256 i = 0; i < valid.length; i++) {
            OnyxRound.DisputeConfig memory d = OnyxRound.DisputeConfig({
                challengeWindow: uint64(cw[i]),
                minChallengeBond: uint128(bond[i]),
                panelSize: uint32(size[i]),
                panelCommitWindow: uint64(pcw[i]),
                panelRevealWindow: uint64(prw[i]),
                appealWindow: uint64(aw[i]),
                maxAppeals: uint32(ap[i]),
                panelFee: uint128(fee[i]),
                panelSlash: uint128(slash[i])
            });
            assertEq(probe.valid(d), valid[i], names[i]);
        }
    }

    /// M6-L2-F3: `OnyxRound.appealFloor` equals Rust `DisputeConfig::appeal_floor`.
    function test_AppealFloorMatchesRust() public view {
        string memory k = ".disputes.appealFloor.";
        uint256[] memory bond = vm.parseJsonUintArray(json, string.concat(k, "minChallengeBond"));
        uint256[] memory fee = vm.parseJsonUintArray(json, string.concat(k, "panelFee"));
        uint256[] memory level = vm.parseJsonUintArray(json, string.concat(k, "level"));
        uint256[] memory size = vm.parseJsonUintArray(json, string.concat(k, "size"));
        uint256[] memory floor = vm.parseJsonUintArray(json, string.concat(k, "floor"));
        assertEq(floor.length, 5);
        for (uint256 i = 0; i < floor.length; i++) {
            OnyxRound.DisputeConfig memory d;
            d.minChallengeBond = uint128(bond[i]);
            d.panelFee = uint128(fee[i]);
            assertEq(OnyxRound.appealFloor(d, uint32(level[i]), uint32(size[i])), floor[i]);
        }
    }

    /// M6-L2-F1: `OnyxRound.canSit` equals Rust `dispute::can_sit` (the L29-F4 pool test).
    function test_CanSitMatchesRust() public view {
        string memory k = ".disputes.canSit.";
        uint256[] memory bond = vm.parseJsonUintArray(json, string.concat(k, "bond"));
        uint256[] memory taken = vm.parseJsonUintArray(json, string.concat(k, "taken"));
        uint256[] memory slash = vm.parseJsonUintArray(json, string.concat(k, "panelSlash"));
        bool[] memory result = vm.parseJsonBoolArray(json, string.concat(k, "result"));
        assertEq(result.length, 6);
        for (uint256 i = 0; i < result.length; i++) {
            assertEq(OnyxRound.canSit(bond[i], taken[i], slash[i]), result[i]);
        }
    }

    function test_CaseIdsAndClaimMatchRust() public view {
        uint64 round = uint64(vm.parseJsonUint(json, ".disputes.round"));
        bytes32 shard = vm.parseJsonBytes32(json, ".disputes.shard");
        address challenger = vm.parseJsonAddress(json, ".disputes.challenger");
        bytes32 claim = dsp.claimCommit(
            ledger,
            round,
            shard,
            challenger,
            vm.parseJsonBytes32(json, ".disputes.claimedRoot"),
            vm.parseJsonBytes32(json, ".disputes.claimSalt")
        );
        assertEq(claim, vm.parseJsonBytes32(json, ".disputes.claimCommit"));
        assertEq(
            dsp.disputeId(ledger, round, shard, challenger, claim, 0),
            vm.parseJsonBytes32(json, ".disputes.caseId")
        );
        assertEq(
            dsp.disputeId(ledger, round, shard, address(0), bytes32(0), 1),
            vm.parseJsonBytes32(json, ".disputes.rerunCaseId")
        );
        assertEq(
            dsp.noValidRootMarker(shard), vm.parseJsonBytes32(json, ".disputes.noValidRootMarker")
        );
    }

    function test_PanelDrawMatchesRust() public view {
        bytes32 seed = vm.parseJsonBytes32(json, ".disputes.seed");
        bytes32 caseId = vm.parseJsonBytes32(json, ".disputes.caseId");
        bytes32 beacon = vm.parseJsonBytes32(json, ".disputes.beacon");
        address[] memory cands = vm.parseJsonAddressArray(json, ".disputes.candidates");
        _checkDraw(seed, caseId, beacon, cands, 0, 3, ".disputes.draw3");
        _checkDraw(seed, caseId, beacon, cands, 1, 7, ".disputes.draw7");
        // Too few candidates: the file says how many (none), read rather than assumed (AUD-1).
        _checkDraw(seed, caseId, beacon, cands, 2, 11, ".disputes.draw11");
        // Candidate order does not matter (Rust takes a set).
        address[] memory rev = new address[](cands.length);
        for (uint256 i = 0; i < cands.length; i++) {
            rev[i] = cands[cands.length - 1 - i];
        }
        _checkDraw(seed, caseId, beacon, rev, 0, 3, ".disputes.draw3");
    }

    function _checkDraw(
        bytes32 seed,
        bytes32 caseId,
        bytes32 beacon,
        address[] memory cands,
        uint32 level,
        uint32 size,
        string memory key
    ) internal view {
        address[] memory want = vm.parseJsonAddressArray(json, key);
        address[] memory got = dsp.drawPanel(seed, caseId, level, beacon, cands, size);
        assertEq(got.length, want.length, key);
        for (uint256 i = 0; i < want.length; i++) {
            assertEq(got[i], want[i], key);
        }
    }

    function test_PanelVoteHashMatchesRust() public view {
        assertEq(
            dsp.panelCommitHash(
                vm.parseJsonBytes32(json, ".disputes.caseId"),
                uint32(vm.parseJsonUint(json, ".disputes.voteLevel")),
                vm.parseJsonAddress(json, ".disputes.voteMember"),
                vm.parseJsonBytes32(json, ".disputes.voteRoot"),
                vm.parseJsonBytes32(json, ".disputes.voteSalt")
            ),
            vm.parseJsonBytes32(json, ".disputes.voteHash")
        );
    }

    /// The contract judges verdicts with Rust's `verdict_right` (every kind, matching and
    /// different miner roots, with a reference and with no valid root).
    function test_VerdictRuleMatchesRust() public view {
        string memory p = ".disputes.verdictRight.";
        uint256[] memory kind = vm.parseJsonUintArray(json, string.concat(p, "kind"));
        bytes32[] memory vroot = vm.parseJsonBytes32Array(json, string.concat(p, "verdictRoot"));
        bytes32[] memory mroot = vm.parseJsonBytes32Array(json, string.concat(p, "minerRoot"));
        bool[] memory none = vm.parseJsonBoolArray(json, string.concat(p, "noValidRoot"));
        bytes32[] memory ref = vm.parseJsonBytes32Array(json, string.concat(p, "reference"));
        uint256[] memory res = vm.parseJsonUintArray(json, string.concat(p, "result"));
        assertEq(kind.length, 20);
        for (uint256 i = 0; i < kind.length; i++) {
            assertEq(
                dsp.verdictRight(uint8(kind[i]), vroot[i], mroot[i], ref[i], none[i]),
                res[i],
                vm.toString(i)
            );
        }
    }
}
