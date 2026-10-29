// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// Rust and Solidity agree on every round-level hash and text (ADR-0022). Vectors are
/// written by adapters/source-btc/tests/evm_vectors.rs.
contract OnyxSettlementVectorsTest is Test {
    string internal json;

    function setUp() public {
        json = vm.readFile("../fixtures/settlement/vectors.json");
    }

    function _cfg() internal view returns (OnyxRound.RoundConfig memory c) {
        c.commitClose = 100;
        c.revealClose = 200;
        c.minBond = 1_000;
        c.validatorSetRoot = vm.parseJsonBytes32(json, ".validatorSetRoot");
        c.canaryCommit = keccak256("canary commit placeholder");
        c.beaconHeight = 201;
        c.validation = OnyxRound.ValidationConfig({
            verdictCommitClose: 300,
            verdictRevealClose: 400,
            canaryOpenClose: 450,
            pendingClose: 600,
            ratePpm: 1_000_000,
            minerReward: 500,
            validatorFee: 10,
            validatorSlash: 2_000,
            missedSlash: 100,
            minCanaryPpm: 0
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

    function test_RoundTagMatchesRust() public view {
        address ledger = vm.parseJsonAddress(json, ".ledger");
        uint256 chainId = vm.parseJsonUint(json, ".chainId");
        bytes32 lid = OnyxRound.ledgerId(chainId, ledger);
        assertEq(lid, vm.parseJsonBytes32(json, ".ledgerId"), "ledger id binds the chain id");
        assertTrue(lid != OnyxRound.ledgerId(chainId + 1, ledger), "other chain, other ledger");
        bytes32[] memory shards = vm.parseJsonBytes32Array(json, ".shards");
        bytes32 tag = OnyxRound.roundTag(
            vm.parseJsonString(json, ".domain"),
            lid,
            uint64(vm.parseJsonUint(json, ".round")),
            _cfg(),
            shards
        );
        assertEq(tag, vm.parseJsonBytes32(json, ".roundTag"));
    }

    function test_ValidatorSetRootMatchesRust() public view {
        address[] memory members = vm.parseJsonAddressArray(json, ".members");
        string[] memory b = vm.parseJsonStringArray(json, ".bonds");
        uint128[] memory bonds = new uint128[](b.length);
        for (uint256 i = 0; i < b.length; i++) {
            bonds[i] = uint128(vm.parseUint(b[i]));
        }
        assertEq(
            OnyxRound.validatorSetRoot(members, bonds),
            vm.parseJsonBytes32(json, ".validatorSetRoot")
        );
    }

    function test_CommitMessageDigestAndSignatureMatchRust() public view {
        bytes memory message = OnyxRound.commitMessage(
            vm.parseJsonBytes32(json, ".roundTag"),
            vm.parseJsonBytes32(json, ".commit.shard"),
            vm.parseJsonBytes32(json, ".commit.commitHash"),
            uint128(vm.parseUint(vm.parseJsonString(json, ".commit.bond")))
        );
        assertEq(string(message), vm.parseJsonString(json, ".commit.message"));
        bytes32 digest = OnyxSig.eip191Digest(message);
        assertEq(digest, vm.parseJsonBytes32(json, ".commit.digest"));
        assertEq(
            OnyxSig.recover(digest, vm.parseJsonBytes(json, ".commit.sig")),
            vm.parseJsonAddress(json, ".commit.miner")
        );
    }

    function test_CanaryCommitMatchesRust() public view {
        OnyxRound.Canary[] memory list = new OnyxRound.Canary[](2);
        for (uint256 i = 0; i < 2; i++) {
            string memory p = string.concat(".canaries[", vm.toString(i), "]");
            list[i] = OnyxRound.Canary({
                shard: vm.parseJsonBytes32(json, string.concat(p, ".shard")),
                miner: vm.parseJsonAddress(json, string.concat(p, ".miner")),
                root: vm.parseJsonBytes32(json, string.concat(p, ".root"))
            });
        }
        assertEq(
            OnyxRound.canaryCommit(list, vm.parseJsonBytes32(json, ".canarySalt")),
            vm.parseJsonBytes32(json, ".canaryCommit")
        );
    }

    function test_VerdictCommitHashesMatchRust() public view {
        bytes32 seed = vm.parseJsonBytes32(json, ".seed");
        address validator = vm.parseJsonAddress(json, ".validator");
        bytes32 shard = vm.parseJsonBytes32Array(json, ".shards")[0];
        address miner = vm.parseJsonAddress(json, ".verdictMiner");
        bytes32 salt = vm.parseJsonBytes32(json, ".verdictSalt");
        for (uint256 i = 0; i < 4; i++) {
            string memory p = string.concat(".verdicts[", vm.toString(i), "]");
            uint8 kind = uint8(vm.parseJsonUint(json, string.concat(p, ".kind")));
            bytes32 root = vm.parseJsonBytes32(json, string.concat(p, ".root"));
            assertEq(
                OnyxRound.verdictCommitHash(seed, validator, shard, miner, kind, root, salt),
                vm.parseJsonBytes32(json, string.concat(p, ".hash"))
            );
        }
    }

    function test_AssignmentMatchesRust() public view {
        bytes32 seed = vm.parseJsonBytes32(json, ".seed");
        address validator = vm.parseJsonAddress(json, ".validator");
        bytes32 shard = vm.parseJsonBytes32(json, ".assignShard");
        uint256 hits;
        for (uint256 i = 0; i < 16; i++) {
            string memory p = string.concat(".assignments[", vm.toString(i), "]");
            address miner = vm.parseJsonAddress(json, string.concat(p, ".miner"));
            bool want = vm.parseJsonBool(json, string.concat(p, ".assigned"));
            assertEq(OnyxRound.assigned(seed, validator, shard, miner, 250_000), want);
            if (want) hits++;
        }
        assertGt(hits, 0);
        assertLt(hits, 16);
    }

    function _u(string memory k) internal view returns (uint256) {
        return vm.parseJsonUint(json, string.concat(".evmSeed.cfg.", k));
    }

    /// The `evmSeed` round's config, read back from the Rust export.
    function _evmSeedCfg() internal view returns (OnyxRound.RoundConfig memory c) {
        c.commitClose = uint64(_u("commitClose"));
        c.revealClose = uint64(_u("revealClose"));
        c.minBond = uint128(_u("minBond"));
        c.validatorSetRoot = vm.parseJsonBytes32(json, ".evmSeed.cfg.validatorSetRoot");
        c.canaryCommit = vm.parseJsonBytes32(json, ".evmSeed.cfg.canaryCommit");
        c.beaconHeight = uint64(_u("beaconHeight"));
        c.validation = OnyxRound.ValidationConfig({
            verdictCommitClose: uint64(_u("verdictCommitClose")),
            verdictRevealClose: uint64(_u("verdictRevealClose")),
            canaryOpenClose: uint64(_u("canaryOpenClose")),
            pendingClose: uint64(_u("pendingClose")),
            ratePpm: uint64(_u("ratePpm")),
            minerReward: uint128(_u("minerReward")),
            validatorFee: uint128(_u("validatorFee")),
            validatorSlash: uint128(_u("validatorSlash")),
            missedSlash: uint128(_u("missedSlash")),
            minCanaryPpm: uint64(_u("minCanaryPpm"))
        });
        c.dispute = OnyxRound.DisputeConfig({
            challengeWindow: uint64(_u("challengeWindow")),
            minChallengeBond: uint128(_u("minChallengeBond")),
            panelSize: uint32(_u("panelSize")),
            panelCommitWindow: uint64(_u("panelCommitWindow")),
            panelRevealWindow: uint64(_u("panelRevealWindow")),
            appealWindow: uint64(_u("appealWindow")),
            maxAppeals: uint32(_u("maxAppeals")),
            panelFee: uint128(_u("panelFee")),
            panelSlash: uint128(_u("panelSlash"))
        });
    }

    /// M6-G4R-F2: the contract derives the `evmSeed` round's tag itself from the same domain,
    /// ledger id, round id, config and shards, so the mirror ledger's tag is the contract's.
    function test_EvmSeedTagIsDerivedByTheContract() public view {
        bytes32 lid = OnyxRound.ledgerId(
            vm.parseJsonUint(json, ".chainId"), vm.parseJsonAddress(json, ".ledger")
        );
        assertEq(lid, vm.parseJsonBytes32(json, ".evmSeed.ledgerId"), "mirror ledger id");
        OnyxRound.RoundConfig memory c = _evmSeedCfg();
        assertEq(c.validation.ratePpm, vm.parseJsonUint(json, ".evmSeed.ratePpm"), "rate");
        bytes32 tag = OnyxRound.roundTag(
            vm.parseJsonString(json, ".evmSeed.domain"),
            lid,
            uint64(vm.parseJsonUint(json, ".evmSeed.round")),
            c,
            vm.parseJsonBytes32Array(json, ".evmSeed.shards")
        );
        assertEq(tag, vm.parseJsonBytes32(json, ".evmSeed.tag"), "tag");
    }

    /// M6-G4: a validator running the Rust coordinator on a ledger that mirrors this contract
    /// (`Ledger::mirror_of_contract`) gets the contract's seed, keccak256("onyx-seed-evm-v1" || tag
    /// || blockhash), and so exactly the tasks the contract judges (ADR-0022 item 5).
    function test_EvmSeedMatchesRust() public view {
        bytes32 tag = vm.parseJsonBytes32(json, ".evmSeed.tag");
        bytes32 beacon = vm.parseJsonBytes32(json, ".evmSeed.beacon");
        bytes32 seed = OnyxRound.seed(tag, beacon);
        assertEq(seed, vm.parseJsonBytes32(json, ".evmSeed.seed"), "seed");
        bytes32 sh = vm.parseJsonBytes32(json, ".evmSeed.shard");
        uint64 rate = uint64(vm.parseJsonUint(json, ".evmSeed.ratePpm"));
        address[] memory vs = vm.parseJsonAddressArray(json, ".evmSeed.validators");
        address[] memory ms = vm.parseJsonAddressArray(json, ".evmSeed.miners");
        uint256 hits;
        for (uint256 i = 0; i < vs.length; i++) {
            bool[] memory want =
                vm.parseJsonBoolArray(json, string.concat(".evmSeed.tasks[", vm.toString(i), "]"));
            assertEq(want.length, ms.length);
            for (uint256 j = 0; j < ms.length; j++) {
                assertEq(OnyxRound.assigned(seed, vs[i], sh, ms[j], rate), want[j], "task");
                if (want[j]) hits++;
            }
        }
        assertGt(hits, 0);
        assertLt(hits, vs.length * ms.length);
    }

    /// The reveal paid as "first correct" is the earliest commit, whatever the reveal order.
    function test_RewardOrderMatchesRust() public view {
        uint256[] memory s = vm.parseJsonUintArray(json, ".rewardOrder.revealSeqs");
        uint32[] memory seqs = new uint32[](s.length);
        for (uint256 i = 0; i < s.length; i++) {
            seqs[i] = uint32(s[i]);
        }
        assertEq(OnyxRound.earliest(seqs), vm.parseJsonUint(json, ".rewardOrder.winnerIndex"));
    }

    /// Text helpers agree with forge's formatting for every input.
    function testFuzz_TextHelpers(uint256 v, bytes32 h) public pure {
        assertEq(OnyxSig.decimal(v), vm.toString(v));
        assertEq(OnyxSig.hex32(h), vm.toString(h));
    }

    /// A high-s twin of a valid signature is refused (EIP-2), as in Rust.
    function testFuzz_HighSRefused(uint256 key, bytes32 digest) public pure {
        key = bound(key, 1, 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364140);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        assertEq(OnyxSig.recover(digest, abi.encodePacked(r, s, v)), vm.addr(key));
        uint256 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
        bytes32 highS = bytes32(n - uint256(s));
        uint8 flipped = v == 27 ? 28 : 27;
        assertEq(OnyxSig.recover(digest, abi.encodePacked(r, highS, flipped)), address(0));
    }
}
