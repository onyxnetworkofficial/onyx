// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxLedger, IERC20Min} from "./OnyxLedger.sol";
import {OnyxMerkle} from "./OnyxMerkle.sol";
import {OnyxRound} from "./OnyxRound.sol";

/// @notice What the extension reads from the dispute game.
interface IDisputesRelease {
    function released(uint64 round, bytes32 shard) external view returns (bool);
}

/// @title OnyxSettlementExt
/// @notice The part of the settlement ledger reached by `delegatecall` from
/// `OnyxSettlement`'s fallback (ADR-0024 item 1): readers for the dispute game and the hooks
/// only `OnyxDisputes` may call. It runs on the settlement's storage; called directly it
/// refuses the hooks. Call it at the settlement's address, typed as this contract.
contract OnyxSettlementExt is OnyxLedger {
    /// This contract's own address: a direct call (not through the settlement) is refused.
    address private immutable _self;

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
        _self = address(this);
    }

    modifier delegated() {
        if (address(this) == _self) revert NotDisputes();
        _;
    }

    // ------------------------------------------------------------------ moved entry points
    // Rarely called, so they live here to keep the settlement under EIP-170 (ADR-0024 item 1).
    // Same behaviour as before; they run on the settlement's storage.

    /// @notice Open the canary list committed in the round config (once per round, so it lives
    /// here, M6-L2-F1 size). Each canary must match an
    /// actual reveal and come from an address whose first commit is in this round.
    function openCanaries(uint64 id, OnyxRound.Canary[] calldata list, bytes32 salt)
        external
        onlyCoordinator
    {
        Round storage r = _round(id);
        if (
            r.voided || r.canariesResolved || block.number < r.cfg.revealClose
                || block.number >= r.cfg.validation.canaryOpenClose
        ) revert WrongPhase();
        for (uint256 i = 1; i < list.length; i++) {
            if (
                list[i - 1].shard > list[i].shard
                    || (list[i - 1].shard == list[i].shard && list[i - 1].miner >= list[i].miner)
            ) revert CanaryMismatch();
        }
        if (OnyxRound.canaryCommit(list, salt) != r.cfg.canaryCommit) revert CanaryMismatch();
        for (uint256 i = 0; i < list.length; i++) {
            OnyxRound.Canary calldata c = list[i];
            Shard storage s = _shard(id, c.shard);
            if (!commits[id][c.shard][c.miner].revealed || _revealedRoot(s, c.miner) != c.root) {
                revert CanaryMismatch();
            }
            if (firstCommitRound[c.miner] != id + 1) revert CanaryMismatch();
            s.hasCanary = true;
            commits[id][c.shard][c.miner].canary = true;
        }
        r.canariesResolved = true;
        emit CanariesOpened(id, list.length);
    }

    /// @notice True if `row` is row `index` of `leafCount` under the finalized root.
    function verifyRow(
        uint64 id,
        bytes32 shard,
        bytes calldata row,
        uint64 index,
        uint64 leafCount,
        bytes32[] calldata siblings
    ) external view returns (bool) {
        if (!_isFinal(id, shard)) return false;
        return OnyxMerkle.verify(_shards[id][shard].finalRoot, row, index, leafCount, siblings);
    }

    /// @notice The finalized root of (round, shard): root, binding hash and the block it
    /// finalized at. Zero values when not finalized.
    function getFinalizedRoot(uint64 id, bytes32 shard)
        external
        view
        returns (bytes32 root, bytes32 binding, uint64 finalizedAt)
    {
        if (!_isFinal(id, shard)) return (bytes32(0), bytes32(0), 0);
        Shard storage s = _shards[id][shard];
        return (s.finalRoot, s.finalBinding, s.finalizedAt);
    }

    /// @notice Withdraw the coordinator bond once no round still holds it.
    function withdrawCoordinatorBond() external nonReentrant onlyCoordinator {
        if (block.number < coordinatorLockedUntil) revert Locked();
        uint128 amount = coordinatorBond;
        coordinatorBond = 0;
        _credit(msg.sender, amount, CR_COORDINATOR);
    }

    /// @notice Coordinator bond: slashed when the canary list is not opened in time.
    /// Only through the settlement (`delegated`): called on this extension directly, the tokens
    /// would land in an address that has no withdraw path (M6-L2-F6).
    function postCoordinatorBond(uint128 amount) external nonReentrant onlyCoordinator delegated {
        coordinatorBond += amount;
        _pull(msg.sender, amount);
    }

    /// @notice Return a round's unspent budget to the reward pool once every shard is final.
    function reclaimBudget(uint64 id) external {
        Round storage r = _round(id);
        for (uint256 i = 0; i < r.shardIds.length; i++) {
            bytes32 sid = r.shardIds[i];
            Shard storage s = _shards[id][sid];
            ShardStatus st = s.status;
            if (st == ShardStatus.Open || st == ShardStatus.Pending || st == ShardStatus.InCase) {
                revert WrongPhase();
            }
            // A unanimous shard's reward is paid from the budget at `release` (ADR-0024 item 2).
            if (
                st == ShardStatus.Finalized && !s.closed
                    && !IDisputesRelease(disputes).released(id, sid)
            ) revert WrongPhase();
        }
        rewardPool += r.budget;
        r.budget = 0;
    }

    /// @notice Return the bonds of one shard of a voided round (anyone may call; bounded by
    /// `MAX_COMMITS_PER_SHARD`). Only shards still Open or Pending: a settled shard is never
    /// re-credited (M6-F1). Settling needs a drawn seed, so a voided round has none.
    function refundVoidedShard(uint64 id, bytes32 shard) external {
        Round storage r = _round(id);
        if (!r.voided) revert WrongPhase();
        _refundShard(id, shard, _shard(id, shard));
    }

    /// @notice After the canary deadline, settle without canaries and slash the coordinator
    /// for not opening its list (L25-F7). Only on a live round with a drawn seed: if the beacon
    /// has lapsed the round is voided instead and nobody is slashed (M6-L34-F4). Only before
    /// `pendingClose`, while the coordinator bond is still locked (M6-L34-F3).
    function missCanaryDeadline(uint64 id) external {
        Round storage r = _round(id);
        OnyxRound.ValidationConfig storage vc = r.cfg.validation;
        if (
            r.voided || r.canariesResolved || block.number < vc.canaryOpenClose
                || block.number >= vc.pendingClose
        ) {
            revert WrongPhase();
        }
        if (r.seed == bytes32(0)) {
            if (block.number - r.cfg.beaconHeight > BEACON_WINDOW) {
                _voidRound(id, r);
                return;
            }
            revert WrongPhase();
        }
        _missCanaries(id, r);
    }

    /// @notice Leave the active set. The stake stays slashable until `unbondAt`, which is at
    /// least `unbondDelay` blocks away and never before the end of any round it is locked into.
    function requestUnbond() external {
        Validator storage v = validators[msg.sender];
        if (!v.active) revert NotMember();
        uint64 at = _deactivate(msg.sender, v);
        emit UnbondRequested(msg.sender, at);
    }

    /// @notice Withdraw an unbonded stake after its delay.
    function withdrawStake() external nonReentrant {
        Validator storage v = validators[msg.sender];
        if (v.unbondAt == 0 || block.number < v.unbondAt || block.number < v.lockedUntil) {
            revert Locked();
        }
        uint128 amount = v.stake;
        v.stake = 0;
        v.unbondAt = 0;
        _credit(msg.sender, amount, CR_STAKE);
    }

    /// @notice Validator set root the contract snapshotted when the round opened.
    function roundSetRoot(uint64 id) external view returns (bytes32) {
        return _round(id).cfg.validatorSetRoot;
    }

    function roundBudget(uint64 id) external view returns (uint128) {
        return _round(id).budget;
    }

    /// @notice What (round, shard) has drawn from the round budget so far, and the most it may
    /// draw (its reservation; ADR-0022 Amendment 1).
    function shardSpent(uint64 id, bytes32 shard)
        external
        view
        returns (uint128 spent, uint256 allot)
    {
        return (_shards[id][shard].spent, _shardAllot(_round(id)));
    }

    /// @notice Rule-mandated payments to `who` that found no funds (`SpendShort`), not claimed.
    function owedOf(address who) external view returns (uint256) {
        return _owed[who];
    }

    /// @notice Every recorded shortfall not yet claimed.
    function owedTotal() external view returns (uint256) {
        return _owedTotal;
    }

    /// @notice Pay `who` what the rules owe it (`owedOf`) from the reward pool, as far as the
    /// pool reaches (anyone may call; credited for a pull withdrawal). Round leftovers return
    /// to the pool (`reclaimBudget`), so a shortfall is paid once the pool holds it
    /// (ADR-0022 Amendment 1).
    function claimOwed(address who) external {
        uint256 amount = _owed[who];
        if (amount > rewardPool) amount = rewardPool;
        _owed[who] -= amount;
        _owedTotal -= amount;
        rewardPool -= amount;
        _credit(who, amount, CR_OWED);
    }

    // ------------------------------------------------------------------ readers

    /// @notice A miner's commit on (round, shard).
    function commitOf(uint64 id, bytes32 shard, address miner)
        external
        view
        returns (Commit memory)
    {
        return commits[id][shard][miner];
    }

    /// @notice Validator `v`'s verdict on `miner`'s reveal of (round, shard).
    function verdictOf(uint64 id, bytes32 shard, address miner, address v)
        external
        view
        returns (VerdictRec memory)
    {
        return verdicts[id][shard][miner][v];
    }

    /// @notice The reveals of (round, shard), in reveal order (miner, root, binding). Read by
    /// `OnyxRevenue` to find the miners whose root became final (ADR-0025).
    function revealsOf(uint64 id, bytes32 shard) external view returns (Reveal[] memory) {
        return _shards[id][shard].reveals;
    }

    /// @notice Validator `v`'s verdict counts on (round, shard).
    function tallyOf(uint64 id, bytes32 shard, address v) external view returns (Tally memory) {
        return tallies[id][shard][v];
    }

    /// @notice True once no dispute can change the shard's root (ADR-0024 item 2).
    function isFinal(uint64 id, bytes32 shard) external view returns (bool) {
        return _isFinal(id, shard);
    }

    function roundConfig(uint64 id) external view returns (OnyxRound.RoundConfig memory) {
        return _round(id).cfg;
    }

    /// @notice A shard's state for the dispute game: status, closed, canary, finalization.
    function shardState(uint64 id, bytes32 shard)
        external
        view
        returns (
            ShardStatus status,
            bool closed,
            uint64 finalizedAt,
            bytes32 finalRoot,
            bytes32 finalBinding
        )
    {
        Shard storage s = _shards[id][shard];
        return (s.status, s.closed, s.finalizedAt, s.finalRoot, s.finalBinding);
    }

    /// @notice A shard's committers (commit order) and reveals (reveal order).
    function shardMiners(uint64 id, bytes32 shard)
        external
        view
        returns (address[] memory committers, Reveal[] memory reveals_)
    {
        Shard storage s = _shards[id][shard];
        return (s.committers, s.reveals);
    }

    /// A case starts on the shard: a `Pending` shard before `pendingClose` (rerun case), or a
    /// `Finalized` one inside its challenge window that no case closed (challenge). The round's
    /// level slashes apply first (M6-L44-F1).
    function dBegin(uint64 id, bytes32 shard) external onlyDisputes delegated {
        Round storage r = _round(id);
        Shard storage s = _shard(id, shard);
        bool ok = s.status == ShardStatus.Pending
            ? block.number < r.cfg.validation.pendingClose
            : s.status == ShardStatus.Finalized && !s.closed
                && block.number < uint256(s.finalizedAt) + r.cfg.dispute.challengeWindow;
        if (r.voided || r.seed == bytes32(0) || !ok) revert WrongPhase();
        s.status = ShardStatus.InCase;
        _settleRound(id, r);
    }

    /// A case ended: `Finalized` (with root and binding), `NoValidRoot` or `Unresolved`. The
    /// shard is closed and its id is never offered again.
    function dEnd(uint64 id, bytes32 shard, ShardStatus status, bytes32 root, bytes32 binding)
        external
        onlyDisputes
        delegated
    {
        Shard storage s = _shard(id, shard);
        if (s.status != ShardStatus.InCase) revert WrongPhase();
        s.status = status;
        s.closed = true;
        everFinalized[shard] = true;
        if (status == ShardStatus.Finalized) {
            s.finalRoot = root;
            s.finalBinding = binding;
            s.finalizedAt = uint64(block.number);
            emit Finalized(id, shard, root, binding);
        }
    }

    /// Pull a challenge or appeal bond from its poster into escrow.
    function dPull(address from, uint256 amount) external onlyDisputes delegated nonReentrant {
        _pull(from, amount);
    }

    /// Credit escrowed funds (bonds the case holds or judged) to a payee.
    function dCredit(address to, uint256 amount) external onlyDisputes delegated {
        _credit(to, amount, CR_DISPUTE);
    }

    function dBurn(uint256 amount) external onlyDisputes delegated {
        _burn(amount);
    }

    /// Pay from (round, shard)'s share of the round budget (miner reward, first verdict fee
    /// per member, rerun-panel fees; ADR-0022 Amendment 1).
    function dSpend(uint64 id, bytes32 shard, address to, uint128 amount)
        external
        onlyDisputes
        delegated
    {
        _spend(_round(id), id, shard, to, amount, CR_DISPUTE);
    }

    /// A rerun case's extra verdict fee (a validator right on a 2nd+ revealed root): `paid`
    /// comes from the case's lost miner bonds, which the escrow holds; the rest of `amount` is
    /// recorded owed (`SpendShort`, ADR-0022 Amendment 1).
    function dExtra(uint64 id, bytes32 shard, address to, uint256 amount, uint256 paid)
        external
        onlyDisputes
        delegated
    {
        _credit(to, paid, CR_EXTRA);
        if (paid < amount) _owe(id, shard, to, amount, paid);
    }

    function dSlash(address v, uint128 amount) external onlyDisputes delegated {
        _slashValidator(v, amount, SL_DISPUTE);
    }
}
