// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxCommit} from "./OnyxCommit.sol";
import {OnyxLedger, IERC20Min, IDisputesBinding} from "./OnyxLedger.sol";
import {OnyxMerkle} from "./OnyxMerkle.sol";
import {OnyxRound} from "./OnyxRound.sol";
import {OnyxSettlementExt} from "./OnyxSettlementExt.sol";
import {OnyxSig} from "./OnyxSig.sol";

/// @title OnyxSettlement
/// @notice The canonical ledger of an Onyx network (ADR-0022). This contract's address is the
/// ledger id; block numbers are the only clock; transaction order is the call order. It holds
/// every bond and reward in escrow and pays out by pull withdrawals.
///
/// M6 loop 1 scope: bonded validator registry with locks and unbonding; coordinator bond;
/// reward pool; round lifecycle (open, signed commit, reveal); beacon seed from blockhash;
/// verdict commit and reveal; canary opening with a deadline; finalization of unanimous,
/// undisputed, canary-free shards with at least one assigned Agree; voiding of shards still
/// pending at `pendingClose`; finalized-root registry with Merkle row proofs. Disputed shards
/// stay `Pending`: the rerun attestation and the M5 dispute game arrive in M6 loop 2.
/// Loop 34 (review M6-F1..F9): seed required to settle, zero commit hash refused, the contract
/// snapshots its own validator set, earliest commit paid, lazy per-shard void refunds, bounded
/// registry with eviction of the lowest stake, chain id in the ledger id.
/// Loop 35 (review M6-L34): verdicts are tallied as they arrive so settling is O(members);
/// settling happens only before `pendingClose`, while every stake is still locked; any shard
/// still open then is voided; a validator slashed below the minimum starts unbonding.
/// Testnet only (Base Sepolia).
contract OnyxSettlement is OnyxLedger {
    /// Views and dispute hooks that do not fit under EIP-170 here (ADR-0024 item 1). Created by
    /// this constructor with the same parameters; every call this contract does not implement
    /// is forwarded to it by `delegatecall`, so it runs on this contract's storage.
    address public immutable ext;

    constructor(
        IERC20Min token_,
        address coordinator_,
        address disputes_,
        string memory domain_,
        uint64 unbondDelay_,
        uint128 minValidatorStake_,
        uint128 minCoordinatorBond_,
        uint128 canarySlash_
    )
        OnyxLedger(
            token_,
            coordinator_,
            disputes_,
            domain_,
            unbondDelay_,
            minValidatorStake_,
            minCoordinatorBond_,
            canarySlash_
        )
    {
        ext = address(
            new OnyxSettlementExt(
                token_,
                coordinator_,
                disputes_,
                domain_,
                unbondDelay_,
                minValidatorStake_,
                minCoordinatorBond_,
                canarySlash_
            )
        );
    }

    // ------------------------------------------------------------------ escrow

    /// @notice Withdraw everything credited to the caller (pull payment).
    function withdraw() external nonReentrant {
        uint256 amount = balanceOf[msg.sender];
        balanceOf[msg.sender] = 0;
        if (amount > 0) _pay(msg.sender, amount);
    }

    /// @notice Add to the reward pool that funds miner rewards and validator fees.
    function fundRewards(uint256 amount) external nonReentrant {
        rewardPool += amount;
        _pull(msg.sender, amount);
    }

    // ------------------------------------------------------------------ registry

    /// @notice Bond (or top up) as a validator. Joins the active set if not already in it.
    function stake(uint128 amount) external nonReentrant {
        Validator storage v = validators[msg.sender];
        if (v.unbondAt != 0) revert Locked();
        v.stake += amount;
        if (v.stake < minValidatorStake) revert BondTooLow();
        if (!v.active) {
            if (_members.length >= MAX_VALIDATORS) _evictLowest(msg.sender, v.stake);
            _insertMember(msg.sender);
            v.active = true;
        }
        emit Staked(msg.sender, amount, v.stake);
        _pull(msg.sender, amount);
    }

    /// The set is full: a newcomer must bring more stake than the lowest member, who is
    /// evicted into unbonding with the normal delay (M6-F8). Bounded by `MAX_VALIDATORS` reads.
    function _evictLowest(address newcomer, uint128 newStake) internal {
        address low = _members[0];
        uint128 lowStake = validators[low].stake;
        for (uint256 i = 1; i < _members.length; i++) {
            uint128 st_ = validators[_members[i]].stake;
            if (st_ < lowStake || (st_ == lowStake && _members[i] < low)) {
                low = _members[i];
                lowStake = st_;
            }
        }
        if (newStake <= lowStake) revert TooMany();
        uint64 at = _deactivate(low, validators[low]);
        emit Evicted(low, newcomer, at);
    }

    /// @notice Active validators and their stakes, ascending by address. Sorted in memory with
    /// a heapsort: O(n log n) on every input order, so no staking order can inflate `openRound`
    /// gas (the earlier quicksort had a 4x adversarial case, M6-L35-F4).
    function activeSet() public view returns (address[] memory members, uint128[] memory bonds) {
        members = _members;
        _heapSort(members);
        bonds = new uint128[](members.length);
        for (uint256 i = 0; i < members.length; i++) {
            bonds[i] = validators[members[i]].stake;
        }
    }

    /// In-place heapsort of distinct addresses, ascending.
    function _heapSort(address[] memory a) internal pure {
        uint256 n = a.length;
        if (n < 2) return;
        unchecked {
            for (uint256 i = n / 2; i > 0; i--) {
                _siftDown(a, i - 1, n);
            }
            for (uint256 end = n - 1; end > 0; end--) {
                (a[0], a[end]) = (a[end], a[0]);
                _siftDown(a, 0, end);
            }
        }
    }

    function _siftDown(address[] memory a, uint256 root, uint256 end) internal pure {
        unchecked {
            while (true) {
                uint256 child = 2 * root + 1;
                if (child >= end) return;
                if (child + 1 < end && a[child] < a[child + 1]) child++;
                if (a[root] >= a[child]) return;
                (a[root], a[child]) = (a[child], a[root]);
                root = child;
            }
        }
    }

    /// O(1) insert and remove: storage order does not matter (M6-F8).
    function _insertMember(address a) internal {
        _members.push(a);
        _memberPos[a] = _members.length;
    }

    // ------------------------------------------------------------------ coordinator

    // ------------------------------------------------------------------ rounds

    /// @notice Open a round. The validator set is the whole active registry at this block, so
    /// the coordinator cannot pick it (L28-F3). The contract snapshots it and writes its root
    /// into the round config and tag itself; `cfg.validatorSetRoot` is ignored, so a registry
    /// change just before this call cannot make it fail (M6-F5). Miners read the tag back with
    /// `roundTag`.
    /// Shards are given ascending by id. Rewards and fees for every shard are moved from the
    /// reward pool into the round's budget now, so payouts never depend on later funding.
    function openRound(
        uint64 id,
        OnyxRound.RoundConfig calldata cfg,
        bytes32[] calldata shardIds,
        ShardInit[] calldata inits
    ) external onlyCoordinator {
        Round storage r = _rounds[id];
        if (r.exists) revert RoundExists();
        if (IDisputesBinding(disputes).settlement() != address(this)) revert NotDisputes();
        _checkConfig(cfg);
        if (coordinatorBond < minCoordinatorBond) revert BondTooLow();
        if (shardIds.length == 0 || shardIds.length > MAX_SHARDS) revert BadShards();
        if (shardIds.length != inits.length) revert BadShards();

        _snapshotMembers(id, r, cfg);
        // Per shard: the miner reward, every member's fee, and one rerun panel's fees (a
        // Pending shard's first panel is paid by the round, ADR-0024 item 4). Each shard draws
        // only on its own share (`_spend`, ADR-0022 Amendment 1).
        uint256 budget = shardIds.length * _shardAllot(r);
        if (budget > rewardPool || budget > type(uint128).max) revert InsufficientFunds();
        rewardPool -= budget;
        r.exists = true;
        r.budget = uint128(budget);
        _initShards(id, r, shardIds, inits);
        r.tag = OnyxRound.roundTag(domain, ledgerId(), id, r.cfg, shardIds);
        emit RoundOpened(id, r.tag, r.cfg.validatorSetRoot);
    }

    /// Snapshot the whole active registry into the round (its root goes into the config and
    /// tag) and lock every member's stake, and the coordinator bond, until `pendingClose`.
    function _snapshotMembers(uint64 id, Round storage r, OnyxRound.RoundConfig calldata cfg)
        internal
    {
        (address[] memory members, uint128[] memory bonds) = activeSet();
        if (members.length < MIN_ROUND_MEMBERS) revert TooFewMembers();
        // A wrong root's bond covers one extra fee per member, so rerun extras beyond lost bonds
        // come only from the canary's root (ADR-0022 Amendment 1, review L2-R2).
        if (cfg.minBond < uint256(cfg.validation.validatorFee) * members.length) {
            revert BadConfig();
        }
        r.cfg = cfg;
        r.cfg.validatorSetRoot = OnyxRound.validatorSetRoot(members, bonds);
        r.members = members;
        // Stakes stay slashable until every dispute of the round has ended (ADR-0024 item 3).
        uint64 lockTo = _lockEnd(cfg);
        for (uint256 i = 0; i < members.length; i++) {
            isRoundMember[id][members[i]] = true;
            Validator storage v = validators[members[i]];
            if (v.lockedUntil < lockTo) v.lockedUntil = lockTo;
        }
        uint64 pc = cfg.validation.pendingClose;
        if (coordinatorLockedUntil < pc) coordinatorLockedUntil = pc;
    }

    function _initShards(
        uint64 id,
        Round storage r,
        bytes32[] calldata shardIds,
        ShardInit[] calldata inits
    ) internal {
        for (uint256 i = 0; i < shardIds.length; i++) {
            bytes32 sid = shardIds[i];
            if (i > 0 && shardIds[i - 1] >= sid) revert BadShards();
            ShardInit calldata s = inits[i];
            if (OnyxCommit.shardId(s.fieldsHash, s.paramsHash, s.registryRoot) != sid) {
                revert BadShards();
            }
            if (everFinalized[sid]) revert AlreadyFinalized();
            if (live[sid]) revert ShardLive();
            live[sid] = true;
            Shard storage sh = _shards[id][sid];
            sh.fieldsHash = s.fieldsHash;
            sh.status = ShardStatus.Open;
            r.shardIds.push(sid);
        }
    }

    /// Every phase lasts at least `MIN_WINDOW` blocks and the whole round at most
    /// `MAX_ROUND_SPAN`; the seed can be drawn in at least `MIN_DRAW_SLACK` blocks that each
    /// still leave `MIN_VERDICT_WINDOW` blocks for verdicts (M6-L44-F3); deterrence holds for miners (bond at the sampling rate) and validators (slash
    /// at the canary density), as Rust's `Round::open` and `validators_deterred` (M6-F3); and
    /// the dispute settings pass Rust's `DisputeConfig::is_valid` (M5-L46-F3).
    function _checkConfig(OnyxRound.RoundConfig calldata c) internal view {
        if (!OnyxRound.roundConfigValid(c, block.number)) revert BadConfig();
    }

    /// @notice Round tag of an open round (miners sign over it).
    function roundTag(uint64 id) external view returns (bytes32) {
        return _round(id).tag;
    }

    /// @notice Post a bonded commit signed by the miner (EIP-191 over the Rust
    /// `commit_message`). Anyone may relay it; the bond is pulled from the miner.
    function commit(
        uint64 id,
        bytes32 shard,
        address miner,
        bytes32 commitHash,
        uint128 bond,
        bytes calldata sig
    ) external nonReentrant {
        Round storage r = _round(id);
        if (r.voided || block.number >= r.cfg.commitClose) revert WrongPhase();
        Shard storage s = _shard(id, shard);
        if (bond < r.cfg.minBond) revert BondTooLow();
        if (commitHash == bytes32(0)) revert BadCommit();
        Commit storage c = commits[id][shard][miner];
        if (c.exists) revert DuplicateCommit();
        if (s.committers.length >= MAX_COMMITS_PER_SHARD) revert TooMany();
        bytes32 digest =
            OnyxSig.eip191Digest(OnyxRound.commitMessage(r.tag, shard, commitHash, bond));
        if (miner == address(0) || OnyxSig.recover(digest, sig) != miner) revert BadSignature();
        c.commitHash = commitHash;
        c.exists = true;
        c.bond = bond;
        c.seq = uint32(s.committers.length);
        s.committers.push(miner);
        if (firstCommitRound[miner] == 0) firstCommitRound[miner] = id + 1;
        emit Committed(id, shard, miner, commitHash);
        // The miner authorised this exact bond by signing the commit message; each signed
        // commit is usable once (DuplicateCommit), so pulling from `miner` is intended.
        _pull(miner, bond);
    }

    /// @notice Reveal a commitment. The contract recomputes the binding and commit hashes
    /// (ADR-0011) and requires the shard's registered fields. Rows stay offchain in the CAS.
    function reveal(
        uint64 id,
        bytes32 shard,
        address miner,
        OnyxCommit.CommitmentFields calldata fields,
        bytes32 salt
    ) external {
        Round storage r = _round(id);
        if (r.voided || block.number < r.cfg.commitClose || block.number >= r.cfg.revealClose) {
            revert WrongPhase();
        }
        Shard storage s = _shard(id, shard);
        Commit storage c = commits[id][shard][miner];
        if (!c.exists) revert NoCommit();
        if (c.revealed) revert DuplicateCommit();
        if (OnyxCommit.shardFieldsHash(fields) != s.fieldsHash) revert RevealMismatch();
        bytes32 binding = OnyxCommit.bindingHash(fields);
        if (OnyxCommit.commitHash(id, shard, binding, miner, salt) != c.commitHash) {
            revert RevealMismatch();
        }
        c.revealed = true;
        s.reveals.push(Reveal({miner: miner, root: fields.root, binding: binding}));
        emit Revealed(id, shard, miner, fields.root);
    }

    /// @notice Fix the sampling seed from blockhash(beaconHeight). If nobody did so within the
    /// 256-block window the beacon is gone: the round is voided. Bonds come back per shard with
    /// `refundVoidedShard`, so no call has to touch every commit (M6-F7).
    /// A seed drawn at or after `pendingClose` would be useless (nothing can settle any more),
    /// so the round is voided then too: an unseeded round ends the same way whichever of
    /// `drawSeed` and `voidShard` runs first (M6-L35-F2).
    function drawSeed(uint64 id) external {
        Round storage r = _round(id);
        if (r.voided || r.seed != bytes32(0)) revert WrongPhase();
        uint64 h = r.cfg.beaconHeight;
        if (block.number <= h) revert WrongPhase();
        // A seed drawn too late would leave no real verdict window (one block at worst), and
        // the caller could choose between a near-verdictless settlement and a void. Void unless
        // at least `MIN_VERDICT_WINDOW` blocks remain (M6-L36-F6, M6-L37-F2); this also covers a
        // lapsed beacon and `pendingClose` (M6-L35-F2).
        if (
            block.number - h > BEACON_WINDOW
                || uint256(block.number) + MIN_VERDICT_WINDOW > r.cfg.validation.verdictCommitClose
        ) {
            _voidRound(id, r);
            return;
        }
        r.seed = OnyxRound.seed(r.tag, blockhash(h));
        emit SeedDrawn(id, r.seed);
    }

    /// @notice Commit a verdict on (shard, miner). Only members of the round's snapshot who are
    /// assigned by the seed; the sender is the validator.
    function commitVerdict(uint64 id, bytes32 shard, address miner, bytes32 verdictCommit)
        external
    {
        Round storage r = _round(id);
        if (r.voided || r.seed == bytes32(0) || block.number >= r.cfg.validation.verdictCommitClose)
        {
            revert WrongPhase();
        }
        _shard(id, shard);
        if (!isRoundMember[id][msg.sender]) revert NotMember();
        if (!commits[id][shard][miner].revealed) revert NoCommit();
        if (!OnyxRound.assigned(r.seed, msg.sender, shard, miner, r.cfg.validation.ratePpm)) {
            revert NotAssigned();
        }
        VerdictRec storage v = verdicts[id][shard][miner][msg.sender];
        if (verdictCommit == bytes32(0)) revert VerdictMismatch();
        if (v.commit != bytes32(0)) revert DuplicateVerdict();
        v.commit = verdictCommit;
        tallies[id][shard][msg.sender].committed++;
        unrevealedVerdicts[id][msg.sender]++;
    }

    /// @notice Reveal a committed verdict (kind as Rust's Borsh tag; root only for Disagree).
    function revealVerdict(
        uint64 id,
        bytes32 shard,
        address miner,
        uint8 kind,
        bytes32 root,
        bytes32 salt
    ) external {
        Round storage r = _round(id);
        OnyxRound.ValidationConfig storage vc = r.cfg.validation;
        if (
            r.voided || block.number < vc.verdictCommitClose
                || block.number >= vc.verdictRevealClose
        ) {
            revert WrongPhase();
        }
        Shard storage s = _shard(id, shard);
        VerdictRec storage v = verdicts[id][shard][miner][msg.sender];
        if (v.commit == bytes32(0)) revert NoCommit();
        if (v.revealed) revert DuplicateVerdict();
        if (kind > OnyxRound.NO_VALID_ROOT) revert VerdictMismatch();
        bytes32 h = OnyxRound.verdictCommitHash(r.seed, msg.sender, shard, miner, kind, root, salt);
        if (h != v.commit) revert VerdictMismatch();
        if (kind == OnyxRound.DISAGREE && root == _revealedRoot(s, miner)) {
            revert VerdictMismatch();
        }
        v.revealed = true;
        v.kind = kind;
        v.root = root;
        Tally storage t = tallies[id][shard][msg.sender];
        t.revealed++;
        unrevealedVerdicts[id][msg.sender]--;
        if (kind == OnyxRound.AGREE) t.agrees++;
        if (kind == OnyxRound.DISAGREE || kind == OnyxRound.NO_VALID_ROOT) s.disputed = true;
        emit VerdictRevealed(id, shard, msg.sender, kind);
    }

    /// @notice Settle one shard after verdict reveals close and canaries are resolved, once the
    /// seed is drawn (M6-F1). A shard finalizes only if it is unanimous, undisputed, canary-free
    /// and at least one assigned validator revealed Agree (M6-F3): the earliest commit among the
    /// reveals is paid (Rust pays in commit order, M6-F4), bonds return, correct Agree verdicts
    /// earn a fee, assigned validators who answered nothing lose `missedSlash`; a committed but
    /// unrevealed verdict counts as answered here and is slashed at round level instead (once
    /// per commit, `_settleRound`, M6-F7). Any other shard becomes Pending until the rerun
    /// attestation (M6 loop 2) or `voidShard`. The first settle of a round applies the
    /// round-level slashes, as Rust's first `settle` call does.
    /// Settling is only possible before `pendingClose`, while every stake and the coordinator
    /// bond are still locked, so no slash can be escaped by withdrawing first (M6-L34-F3). If
    /// the canary deadline passed with the list unopened, the first settle applies the missed
    /// deadline itself. Cost is O(members + reveals): verdicts were tallied as they arrived.
    function finalizeShard(uint64 id, bytes32 shard) external {
        Round storage r = _round(id);
        OnyxRound.ValidationConfig storage vc = r.cfg.validation;
        if (
            r.voided || r.seed == bytes32(0) || block.number < vc.verdictRevealClose
                || block.number >= vc.pendingClose
        ) {
            revert WrongPhase();
        }
        if (!r.canariesResolved) {
            if (block.number < vc.canaryOpenClose) revert WrongPhase();
            _missCanaries(id, r);
        }
        Shard storage s = _shard(id, shard);
        if (s.status != ShardStatus.Open) revert WrongPhase();
        _settleRound(id, r);
        if (s.reveals.length == 0) {
            s.status = ShardStatus.Empty;
            live[shard] = false;
            _burnUnrevealed(id, shard, s);
            return;
        }
        bytes32 root = s.reveals[0].root;
        bool unanimous = !s.disputed && !s.hasCanary;
        for (uint256 i = 1; unanimous && i < s.reveals.length; i++) {
            if (s.reveals[i].root != root) unanimous = false;
        }
        Assessment memory a;
        if (unanimous) a = _assess(id, shard, r, s);
        // Rust L29-F4: unanimity counts only if a dispute panel could still be drawn, plus one
        // for a challenger from the same pool; otherwise the shard is rerun (M6-L2-F1).
        if (!unanimous || a.agrees == 0 || a.pool <= r.cfg.dispute.panelSize) {
            s.status = ShardStatus.Pending;
            emit ShardPending(id, shard);
            return;
        }
        uint32[] memory seqs = new uint32[](s.reveals.length);
        for (uint256 i = 0; i < s.reveals.length; i++) {
            seqs[i] = commits[id][shard][s.reveals[i].miner].seq;
        }
        uint256 win = OnyxRound.earliest(seqs);
        s.status = ShardStatus.Finalized;
        s.finalRoot = root;
        s.finalBinding = s.reveals[win].binding;
        s.finalizedAt = uint64(block.number);
        everFinalized[shard] = true;
        // The reward and the revealed bonds wait in escrow until the root is final, so a
        // challenge can still take them (ADR-0024 item 2): `OnyxDisputes.release`.
        _burnUnrevealed(id, shard, s);
        _applyAssessment(r, s, a);
        emit Finalized(id, shard, root, s.finalBinding);
    }

    /// @notice A shard still Open or Pending at `pendingClose` is voided: revealed bonds return,
    /// unrevealed ones are forfeit, nobody is paid or slashed, and the budget becomes
    /// reclaimable. Settling is closed by then, so a shard nobody settled (or one too costly to
    /// settle) can never lock its funds (M6-L34-F1, F3). Voided rounds use `refundVoidedShard`.
    /// A round whose seed was never drawn is voided as a whole instead, and this shard's bonds
    /// all come back, exactly as if `drawSeed` had voided it first; on an already voided round
    /// this is the same as `refundVoidedShard` (M6-L35-F2).
    function voidShard(uint64 id, bytes32 shard) external {
        Round storage r = _round(id);
        if (block.number < r.cfg.validation.pendingClose) revert WrongPhase();
        Shard storage s = _shard(id, shard);
        if (r.voided || r.seed == bytes32(0)) {
            if (!r.voided) _voidRound(id, r);
            _refundShard(id, shard, s);
            return;
        }
        if (s.status != ShardStatus.Pending && s.status != ShardStatus.Open) revert WrongPhase();
        // Round-level slashes apply on every money-moving path while stakes are locked
        // (M6-L44-F1, ADR-0024 item 3).
        if (block.number < _lockEnd(r.cfg)) _settleRound(id, r);
        s.status = ShardStatus.Voided;
        if (s.reveals.length == 0) live[shard] = false;
        for (uint256 j = 0; j < s.committers.length; j++) {
            address m = s.committers[j];
            Commit storage c = commits[id][shard][m];
            if (c.revealed) _credit(m, c.bond, CR_SHARD_VOIDED);
            else _burn(c.bond);
        }
        emit ShardVoided(id, shard);
    }

    /// @notice Apply a round's round-level slashes: each verdict a validator committed and never
    /// revealed costs `validatorSlash` once, whatever its shard's outcome (Pending, canary,
    /// voided at the deadline), exactly as Rust's round-level settlement (M6-F7). Anyone may
    /// call it once verdict reveals close and before `pendingClose`, while stakes are locked;
    /// the first `finalizeShard` applies it too. Never after `pendingClose` (M6-L34-F3: stakes
    /// unlock there, so a late slash would only hit whoever did not withdraw first). Cost:
    /// O(members).
    function settleRound(uint64 id) external {
        Round storage r = _round(id);
        OnyxRound.ValidationConfig storage vc = r.cfg.validation;
        if (
            r.voided || r.seed == bytes32(0) || r.roundSettled
                || block.number < vc.verdictRevealClose || block.number >= _lockEnd(r.cfg)
        ) {
            revert WrongPhase();
        }
        _settleRound(id, r);
    }

    function _burnUnrevealed(uint64 id, bytes32 shard, Shard storage s) internal {
        for (uint256 j = 0; j < s.committers.length; j++) {
            Commit storage c = commits[id][shard][s.committers[j]];
            if (!c.revealed) _burn(c.bond);
        }
    }

    /// Per-member outcome of one shard, computed before anything is written.
    struct Assessment {
        uint256 agrees;
        /// Members who could sit on a panel, counted up to `panelSize + 1` (Rust L29-F4).
        uint256 pool;
        address[] members;
        // 0 = nothing at shard level (no task, or an unrevealed commit: slashed at round level),
        // 1 = agreed (fee), 3 = assigned and answered nothing (missed). 2 is no longer used:
        // unrevealed commits moved to round level (M6-F7).
        uint8[] verdict;
    }

    /// O(members + reveals) storage reads: one tally per member; only members with no verdict
    /// at all need the assignment check, done on miner addresses held in memory (M6-L34-F1).
    /// Also counts the panel pool as Rust's settle does (L29-F4, M6-L2-F1): members who are not
    /// a revealing miner and committed no verdict here, whose stake less open panel exposure
    /// and this shard's missed-task slash covers `panelSlash` (`OnyxRound.canSit`; round-level
    /// slashes are already taken from the stake by `_settleRound`). Rust also counts missed
    /// tasks on other unsettled shards; here those shards settle on their own, and a shortfall
    /// at challenge time finds no panel and goes to the rerun (ADR-0024 item 11).
    function _assess(uint64 id, bytes32 shard, Round storage r, Shard storage s)
        internal
        view
        returns (Assessment memory a)
    {
        uint64 rate = r.cfg.validation.ratePpm;
        bytes32 seed_ = r.seed;
        uint256 n = s.reveals.length;
        address[] memory miners = new address[](n);
        for (uint256 j = 0; j < n; j++) {
            miners[j] = s.reveals[j].miner;
        }
        a.members = r.members;
        a.verdict = new uint8[](a.members.length);
        OnyxRound.DisputeConfig storage d = r.cfg.dispute;
        for (uint256 i = 0; i < a.members.length; i++) {
            address v = a.members[i];
            Tally memory t = tallies[id][shard][v];
            if (t.agrees > 0) {
                // A correct Agree earns its fee even if another verdict here went unrevealed
                // (Rust judges each revealed verdict; fee once per validator and shard).
                a.verdict[i] = 1;
                a.agrees++;
            } else if (t.committed > t.revealed) {
                // Committed but never revealed: answered here, slashed at round level (L25-F9).
                a.verdict[i] = 0;
            } else if (t.committed > 0) {
                // Committed only where assigned, revealed only Abstain: a missed task.
                a.verdict[i] = 3;
            } else {
                for (uint256 j = 0; j < n; j++) {
                    if (OnyxRound.assigned(seed_, v, shard, miners[j], rate)) {
                        a.verdict[i] = 3;
                        break;
                    }
                }
            }
            if (t.committed == 0 && a.pool <= d.panelSize && !_isMiner(miners, v)) {
                uint256 taken = IDisputesBinding(disputes).exposure(v);
                if (a.verdict[i] == 3) taken += r.cfg.validation.missedSlash;
                if (OnyxRound.canSit(validators[v].stake, taken, d.panelSlash)) a.pool++;
            }
        }
    }

    function _isMiner(address[] memory miners, address v) internal pure returns (bool) {
        for (uint256 j = 0; j < miners.length; j++) {
            if (miners[j] == v) return true;
        }
        return false;
    }

    /// Agree fees come from the shard's own reservation (ADR-0022 Amendment 1): nothing was
    /// spent on an Open shard yet and at most one fee per member is due, so it always covers
    /// them (`a.agrees` <= members).
    function _applyAssessment(Round storage r, Shard storage s, Assessment memory a) internal {
        OnyxRound.ValidationConfig storage vc = r.cfg.validation;
        uint128 fees = uint128(a.agrees * vc.validatorFee);
        s.spent = fees;
        r.budget -= fees;
        for (uint256 i = 0; i < a.members.length; i++) {
            uint8 k = a.verdict[i];
            if (k == 1) {
                _credit(a.members[i], vc.validatorFee, CR_VERDICT);
            } else if (k == 3) {
                _slashValidator(a.members[i], vc.missedSlash, SL_MISSED);
            }
        }
    }

    // ------------------------------------------------------------------ readers

    function shardStatus(uint64 id, bytes32 shard) external view returns (ShardStatus) {
        return _shards[id][shard].status;
    }

    // ------------------------------------------------------------------ dispute hooks
    // Called only by `disputes` (ADR-0024 item 1). Every token stays in this escrow.

    // ------------------------------------------------------------------ extension

    /// Forward everything else to `ext` (same storage, same `msg.sender`), returning or
    /// reverting with its data.
    fallback() external {
        address e = ext;
        assembly {
            calldatacopy(0, 0, calldatasize())
            let ok := delegatecall(gas(), e, 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            if iszero(ok) { revert(0, returndatasize()) }
            return(0, returndatasize())
        }
    }
}
