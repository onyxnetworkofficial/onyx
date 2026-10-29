// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxSig} from "./OnyxSig.sol";

/// @title OnyxRound
/// @notice Round-level hashes and texts. Each one must equal its Rust counterpart in
/// crates/onyx-coordinator/src/{round.rs,validate.rs}; shared vectors live in
/// fixtures/settlement/vectors.json (ADR-0022).
library OnyxRound {
    /// Validation timing and economics (Rust `ValidationConfig`), in block numbers and token units.
    struct ValidationConfig {
        uint64 verdictCommitClose;
        uint64 verdictRevealClose;
        uint64 canaryOpenClose;
        uint64 pendingClose;
        uint64 ratePpm;
        uint128 minerReward;
        uint128 validatorFee;
        uint128 validatorSlash;
        uint128 missedSlash;
        uint64 minCanaryPpm;
    }

    /// Dispute timing and economics (Rust `DisputeConfig`, M5, ADR-0021). Part of the round tag
    /// so a round's dispute rules are fixed when it opens; the dispute game itself is not on
    /// chain yet (ADR-0022 Consequences).
    struct DisputeConfig {
        uint64 challengeWindow;
        uint128 minChallengeBond;
        uint32 panelSize;
        uint64 panelCommitWindow;
        uint64 panelRevealWindow;
        uint64 appealWindow;
        uint32 maxAppeals;
        uint128 panelFee;
        uint128 panelSlash;
    }

    /// Round timing and bond (Rust `RoundConfig`), in block numbers.
    struct RoundConfig {
        uint64 commitClose;
        uint64 revealClose;
        uint128 minBond;
        bytes32 validatorSetRoot;
        bytes32 canaryCommit;
        uint64 beaconHeight;
        ValidationConfig validation;
        DisputeConfig dispute;
    }

    /// One canary (Rust `Canary`): a reveal the coordinator made itself.
    struct Canary {
        bytes32 shard;
        address miner;
        bytes32 root;
    }

    uint64 internal constant PPM = 1_000_000;
    /// Rust `dispute::LAZY_PANEL_RATIO`: `panelSlash >= panelFee * 9`.
    uint256 internal constant LAZY_PANEL_RATIO = 9;
    /// Rust `dispute::MAX_APPEALS`.
    uint32 internal constant MAX_APPEALS = 4;
    /// Rust `dispute::MAX_PANEL`: the largest panel a valid config can draw, so a draw and its
    /// judging stay inside the 15M-gas budget at 128 validators (M6-L2-F2).
    uint32 internal constant MAX_PANEL = 63;
    /// Blocks kept at the end of the dispute tail for `settleCase` batches: a rerun deadline
    /// never passes `lockEnd - SETTLE_MARGIN`, so a decided case can be judged while stakes
    /// are still locked (M6-L2-F4).
    uint64 internal constant SETTLE_MARGIN = 64;

    // Round timing rules (ADR-0022 items 13, 24, 33-34; ADR-0024 item 3), re-exported by
    // `OnyxSettlement` under the same names.
    uint64 internal constant MIN_WINDOW = 20;
    uint64 internal constant MIN_VERDICT_WINDOW = MIN_WINDOW;
    uint64 internal constant MIN_DRAW_SLACK = 10;
    uint64 internal constant MIN_PENDING_GAP = 100;
    uint64 internal constant MAX_ROUND_SPAN = 100_000;
    uint64 internal constant MAX_DISPUTE_TAIL = 100_000;
    /// Blocks in which a dispute panel can be drawn from its beacon (`blockhash` reach).
    uint64 internal constant DRAW_WINDOW = 256;

    /// The round checks of `OnyxSettlement.openRound` at block `nowBlock` (linked, so its code
    /// stays out of the settlement runtime): every phase at least `MIN_WINDOW` blocks and the
    /// round at most `MAX_ROUND_SPAN`; the seed drawable in at least `MIN_DRAW_SLACK` blocks
    /// that each leave `MIN_VERDICT_WINDOW` for verdicts (M6-L44-F3); deterrence for miners
    /// (bond at the sampling rate) and validators (slash at the canary density), as Rust's
    /// `Round::open` and `validators_deterred` (M6-F3); dispute settings as Rust's
    /// `DisputeConfig::is_valid` (M5-L46-F3); a dispute tail of at most `MAX_DISPUTE_TAIL`.
    function roundConfigValid(RoundConfig calldata c, uint256 nowBlock)
        external
        pure
        returns (bool)
    {
        ValidationConfig calldata v = c.validation;
        uint256 ppm = PPM;
        return nowBlock + MIN_WINDOW <= c.commitClose
            && uint256(c.commitClose) + MIN_WINDOW <= c.revealClose
            && c.revealClose <= c.beaconHeight
            && uint256(c.beaconHeight) + MIN_VERDICT_WINDOW + MIN_DRAW_SLACK <= v.verdictCommitClose
            && uint256(v.verdictCommitClose) + MIN_WINDOW <= v.verdictRevealClose
            && v.verdictRevealClose <= v.canaryOpenClose
            && uint256(v.canaryOpenClose) + MIN_PENDING_GAP <= v.pendingClose
            && uint256(v.pendingClose) <= nowBlock + MAX_ROUND_SPAN && v.ratePpm > 0
            && v.ratePpm <= ppm && c.minBond > 0
            && uint256(c.minBond) * v.ratePpm >= uint256(v.minerReward) * ppm && v.missedSlash > 0
            && v.validatorSlash >= v.missedSlash && v.minCanaryPpm > 0 && v.minCanaryPpm <= ppm
            && uint256(v.validatorSlash) * v.minCanaryPpm >= uint256(v.validatorFee) * ppm
            && disputeConfigValid(c.dispute) && disputeTail(c.dispute) <= MAX_DISPUTE_TAIL;
    }

    /// Rust `DisputeConfig::is_valid` (M5-L46-F3): odd panel; non-zero panel windows, bond and
    /// slash; the challenge window outlasts one full panel; a lazy member cannot profit; the
    /// smallest bond funds the first panel with room to spare; bounded appeals; the last
    /// appeal's panel at most `MAX_PANEL` (M6-L2-F2). Sums and products are in uint256, so
    /// they cannot wrap where Rust's checked arithmetic fails.
    function disputeConfigValid(DisputeConfig calldata d) internal pure returns (bool) {
        return d.panelSize % 2 == 1 && d.panelCommitWindow > 0 && d.panelRevealWindow > 0
            && d.appealWindow > 0
            && uint256(d.panelCommitWindow) + d.panelRevealWindow + d.appealWindow
                < d.challengeWindow && d.minChallengeBond > 0 && d.panelSlash > 0
            && uint256(d.panelFee) * LAZY_PANEL_RATIO <= d.panelSlash
            && uint256(d.panelFee) * (uint256(d.panelSize) + 1) <= d.minChallengeBond
            && d.maxAppeals <= MAX_APPEALS
            && ((uint256(d.panelSize) + 1) << d.maxAppeals) - 1 <= MAX_PANEL;
    }

    /// Blocks after `pendingClose` during which a dispute of the round can still run
    /// (ADR-0024 item 3): the latest challenge starts `challengeWindow` after a settlement
    /// before `pendingClose`; each panel level takes at most the 256-block draw window plus its
    /// commit, reveal and appeal windows; then the coordinator has `challengeWindow` to post
    /// the rerun; then `SETTLE_MARGIN` blocks to judge it (M6-L2-F4). uint256, so it cannot
    /// wrap; the contract caps it (`MAX_DISPUTE_TAIL`).
    function disputeTail(DisputeConfig memory d) internal pure returns (uint256) {
        return 2 * uint256(d.challengeWindow) + (uint256(d.maxAppeals) + 1)
            * (uint256(DRAW_WINDOW) + d.panelCommitWindow + d.panelRevealWindow + d.appealWindow)
            + SETTLE_MARGIN;
    }

    /// Rust `DisputeConfig::appeal_floor`: the smallest bond for the appeal that opens panel
    /// `level` (>= 1) of `size` members: `minChallengeBond * 2^level`, and at least that
    /// panel's fees. Set by the config, never by the bond a challenger chose to post, so a
    /// rich challenger cannot price honest appellants out (M6-L2-F3).
    function appealFloor(DisputeConfig memory d, uint32 level, uint32 size)
        internal
        pure
        returns (uint256)
    {
        uint256 floor = uint256(d.minChallengeBond) << level;
        uint256 fees = uint256(d.panelFee) * size;
        return floor > fees ? floor : fees;
    }

    /// Rust `dispute::can_sit` (L29-F4): a validator can sit on a panel when its bond, less
    /// what is already taken or at risk, still covers `panelSlash`.
    function canSit(uint256 bond, uint256 taken, uint256 panelSlash) internal pure returns (bool) {
        return (bond > taken ? bond - taken : 0) >= panelSlash;
    }

    /// Ledger id of a deployed settlement contract: keccak256("onyx-ledger-evm-v1" ||
    /// chain id (32 bytes) || contract address (20 bytes)). Binding the chain id means the same
    /// address on another chain is another ledger, so commits never replay across chains
    /// (M6-F9). Rust: `onyx_core::evm_ledger_id` (ADR-0022).
    function ledgerId(uint256 chainId, address settlement) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-ledger-evm-v1", chainId, settlement));
    }

    /// keccak256("onyx-round-v4" || domain || 0x00 || ledger id || id || commit_close ||
    /// reveal_close || min_bond || validator_set_root || canary_commit || beacon_height ||
    /// validation fields || dispute fields || shard ids ascending). `shards` must be strictly
    /// ascending. v4 adds the M5 dispute fields (Rust `round_tag`).
    function roundTag(
        string memory domain,
        bytes32 ledger,
        uint64 id,
        RoundConfig memory c,
        bytes32[] memory shards
    ) public pure returns (bytes32) {
        bytes memory head = abi.encodePacked(
            "onyx-round-v4",
            bytes(domain),
            bytes1(0),
            ledger,
            id,
            c.commitClose,
            c.revealClose,
            c.minBond,
            c.validatorSetRoot,
            c.canaryCommit,
            c.beaconHeight
        );
        ValidationConfig memory v = c.validation;
        bytes memory val = abi.encodePacked(
            v.verdictCommitClose,
            v.verdictRevealClose,
            v.canaryOpenClose,
            v.pendingClose,
            v.ratePpm,
            v.minerReward,
            v.validatorFee,
            v.validatorSlash,
            v.missedSlash,
            v.minCanaryPpm
        );
        return
            keccak256(bytes.concat(head, val, _disputeFields(c.dispute), abi.encodePacked(shards)));
    }

    /// The dispute fields of the round tag, in Rust `round_tag` order (two parts keep the
    /// IR stack shallow).
    function _disputeFields(DisputeConfig memory d) private pure returns (bytes memory) {
        bytes memory first = abi.encodePacked(
            d.challengeWindow, d.minChallengeBond, d.panelSize, d.panelCommitWindow
        );
        bytes memory rest = abi.encodePacked(
            d.panelRevealWindow, d.appealWindow, d.maxAppeals, d.panelFee, d.panelSlash
        );
        return bytes.concat(first, rest);
    }

    /// The text a miner signs to post a commit (Rust `commit_message`).
    function commitMessage(bytes32 tag, bytes32 shard, bytes32 commitHash, uint128 bond)
        public
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(
            "Onyx commit v1\nround: ",
            OnyxSig.hex32(tag),
            "\nshard: ",
            OnyxSig.hex32(shard),
            "\ncommit: ",
            OnyxSig.hex32(commitHash),
            "\nbond: ",
            OnyxSig.decimal(bond)
        );
    }

    /// keccak256("onyx-validators-v1" || (address || bond u128 BE)*), addresses ascending
    /// (Rust `ValidatorSet::root`). One buffer written in place: linear in the member count
    /// (the earlier repeated concat was quadratic in memory, M6-L34-F5).
    function validatorSetRoot(address[] memory members, uint128[] memory bonds)
        public
        pure
        returns (bytes32 h)
    {
        uint256 n = members.length;
        uint256 len = 18 + 36 * n;
        // 32 bytes of slack: every write below is a full word.
        bytes memory b = new bytes(len + 32);
        assembly ("memory-safe") {
            let p := add(b, 32)
            // "onyx-validators-v1" (18 bytes), left-aligned in one word.
            mstore(p, 0x6f6e79782d76616c696461746f72732d76310000000000000000000000000000)
            p := add(p, 18)
            for { let i := 0 } lt(i, n) { i := add(i, 1) } {
                let off := mul(add(i, 1), 32)
                mstore(p, shl(96, mload(add(members, off))))
                mstore(add(p, 20), shl(128, mload(add(bonds, off))))
                p := add(p, 36)
            }
            mstore(b, len)
        }
        h = keccak256(b);
    }

    /// Canary list commitment (Rust `canary_commit`): keccak256("onyx-canaries-v2" ||
    /// Borsh(Vec<Canary>) || salt). Borsh: u32 little-endian length, then each
    /// (shard 32 || miner 20 || root 32).
    function canaryCommit(Canary[] memory list, bytes32 salt) public pure returns (bytes32) {
        uint32 n = uint32(list.length);
        bytes memory b = abi.encodePacked(
            "onyx-canaries-v2",
            bytes4(
                uint32(
                    (n & 0xff) << 24 | ((n >> 8) & 0xff) << 16 | ((n >> 16) & 0xff) << 8 | (n >> 24)
                )
            )
        );
        for (uint256 i = 0; i < list.length; i++) {
            b = bytes.concat(b, abi.encodePacked(list[i].shard, list[i].miner, list[i].root));
        }
        return keccak256(bytes.concat(b, salt));
    }

    /// Index of the reveal paid as "first correct": the lowest commit sequence number, so the
    /// order reveals land in a block never decides (Rust `ShardOutcome::first`, reveals sorted
    /// by commit order; M6-F4). `seqs[i]` is the commit sequence of reveal `i`; non-empty.
    function earliest(uint32[] memory seqs) internal pure returns (uint256 win) {
        uint32 best = seqs[0];
        for (uint256 i = 1; i < seqs.length; i++) {
            if (seqs[i] < best) {
                best = seqs[i];
                win = i;
            }
        }
    }

    /// Verdict kinds, numbered as Rust's Borsh enum tags.
    uint8 internal constant AGREE = 0;
    uint8 internal constant DISAGREE = 1;
    uint8 internal constant ABSTAIN = 2;
    uint8 internal constant NO_VALID_ROOT = 3;

    /// keccak256("onyx-verdict-v2" || seed || validator || shard || miner || Borsh(verdict) ||
    /// salt) (Rust `verdict_commit_hash`). Borsh(verdict) is the tag byte, plus the root for
    /// Disagree.
    function verdictCommitHash(
        bytes32 seed_,
        address validator,
        bytes32 shard,
        address miner,
        uint8 kind,
        bytes32 root,
        bytes32 salt
    ) internal pure returns (bytes32) {
        bytes memory verdict =
            kind == DISAGREE ? abi.encodePacked(kind, root) : abi.encodePacked(kind);
        return keccak256(
            abi.encodePacked("onyx-verdict-v2", seed_, validator, shard, miner, verdict, salt)
        );
    }

    /// Sampling seed for the chain (ADR-0022): keccak256("onyx-seed-evm-v1" || round tag ||
    /// blockhash(beacon_height)). The tag already covers validator set and shards; the beacon
    /// is fixed only after reveals close.
    function seed(bytes32 tag, bytes32 beacon) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-seed-evm-v1", tag, beacon));
    }

    /// True if `validator` must check `miner`'s reveal on `shard` (Rust `assigned`): the first
    /// 8 bytes of keccak256(seed || validator || shard || miner), as x / 2^64 < rate / 1e6.
    function assigned(bytes32 s, address validator, bytes32 shard, address miner, uint64 ratePpm)
        internal
        pure
        returns (bool)
    {
        if (validator == miner) return false;
        bytes32 h = keccak256(abi.encodePacked(s, validator, shard, miner));
        uint256 x = uint256(h) >> 192;
        uint256 rate = ratePpm > PPM ? PPM : ratePpm;
        return x * PPM < rate << 64;
    }
}
