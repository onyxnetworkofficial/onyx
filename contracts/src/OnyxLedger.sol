// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxRound} from "./OnyxRound.sol";

/// @notice The dispute contract's binding (ADR-0024).
interface IDisputesBinding {
    function settlement() external view returns (address);
    function exposure(address v) external view returns (uint256);
}

/// @notice Minimal ERC-20 surface the settlement contract uses.
interface IERC20Min {
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

/// @title OnyxLedger
/// @notice State, types and shared rules of the settlement ledger (ADR-0022, ADR-0024). Both
/// `OnyxSettlement` and its extension `OnyxSettlementExt` inherit it: the settlement reaches
/// the extension by `delegatecall`, so both read and write this one storage layout. Declares
/// no state anywhere else; adds no entry points beyond its public getters and readers.
abstract contract OnyxLedger {
    // ------------------------------------------------------------------ types

    /// `InCase`: a dispute case (rerun or challenge) is deciding the shard (ADR-0024);
    /// `NoValidRoot` and `Unresolved` are how a case can end besides `Finalized`.
    enum ShardStatus {
        None,
        Open,
        Pending,
        Finalized,
        Empty,
        Voided,
        InCase,
        NoValidRoot,
        Unresolved
    }

    struct Commit {
        bytes32 commitHash;
        uint128 bond;
        uint32 seq;
        bool exists;
        bool revealed;
        /// The coordinator's own reveal, opened in `openCanaries` (judged apart, ADR-0024).
        bool canary;
    }

    struct Reveal {
        address miner;
        bytes32 root;
        bytes32 binding;
    }

    struct Shard {
        bytes32 fieldsHash;
        ShardStatus status;
        bool disputed;
        bool hasCanary;
        /// A case decided this shard: final now, never challenged again (ADR-0024).
        bool closed;
        bytes32 finalRoot;
        bytes32 finalBinding;
        uint64 finalizedAt;
        /// Paid so far from the round budget for this shard (`_spend`); never above
        /// `_shardAllot` (ADR-0022 Amendment 1, review L2-R1).
        uint128 spent;
        address[] committers;
        Reveal[] reveals;
    }

    struct VerdictRec {
        bytes32 commit;
        uint8 kind;
        bytes32 root;
        bool revealed;
    }

    struct Round {
        bool exists;
        bool voided;
        bool canariesResolved;
        /// Round-level slashes (unrevealed verdict commits) applied (M6-F7).
        bool roundSettled;
        bytes32 tag;
        bytes32 seed;
        uint128 budget;
        OnyxRound.RoundConfig cfg;
        bytes32[] shardIds;
        address[] members;
    }

    struct ShardInit {
        bytes32 fieldsHash;
        bytes32 paramsHash;
        bytes32 registryRoot;
    }

    /// Per (round, shard, validator) verdict counts, kept as verdicts arrive so settling never
    /// loops members x reveals over storage (M6-L34-F1). At most 32 per shard: fits uint16.
    struct Tally {
        uint16 committed;
        uint16 revealed;
        uint16 agrees;
    }

    struct Validator {
        uint128 stake;
        uint64 lockedUntil;
        uint64 unbondAt;
        bool active;
    }

    // ------------------------------------------------------------------ errors

    error NotCoordinator();
    error BadConfig();
    error RoundExists();
    error NoRound();
    error WrongPhase();
    error UnknownShard();
    error BadShards();
    error BadCommit();
    error TooFewMembers();
    error BondTooLow();
    error BadSignature();
    error DuplicateCommit();
    error NoCommit();
    error RevealMismatch();
    error NotMember();
    error NotAssigned();
    error DuplicateVerdict();
    error VerdictMismatch();
    error CanaryMismatch();
    error Locked();
    error TooMany();
    error InsufficientFunds();
    error TransferFailed();
    error Reentrancy();
    error AlreadyFinalized();
    error ShardLive();
    error NotDisputes();

    // ------------------------------------------------------------------ events

    event Staked(address indexed validator, uint128 amount, uint128 stake);
    event UnbondRequested(address indexed validator, uint64 unbondAt);
    event Evicted(address indexed validator, address indexed by, uint64 unbondAt);
    event RoundOpened(uint64 indexed round, bytes32 tag, bytes32 validatorSetRoot);
    event Committed(
        uint64 indexed round, bytes32 indexed shard, address indexed miner, bytes32 commitHash
    );
    event Revealed(
        uint64 indexed round, bytes32 indexed shard, address indexed miner, bytes32 root
    );
    event SeedDrawn(uint64 indexed round, bytes32 seed);
    event RoundVoided(uint64 indexed round);
    event RoundSettled(uint64 indexed round);
    event VerdictRevealed(
        uint64 indexed round, bytes32 indexed shard, address indexed validator, uint8 kind
    );
    event CanariesOpened(uint64 indexed round, uint256 count);
    event CanaryDeadlineMissed(uint64 indexed round, uint128 coordinatorSlash);
    event Finalized(uint64 indexed round, bytes32 indexed shard, bytes32 root, bytes32 binding);
    event ShardPending(uint64 indexed round, bytes32 indexed shard);
    event ShardVoided(uint64 indexed round, bytes32 indexed shard);
    /// `reason`: one of the `CR_*` codes.
    event Credited(address indexed to, uint256 amount, uint8 reason);
    /// `reason`: one of the `SL_*` codes.
    event Slashed(address indexed who, uint256 amount, uint8 reason);
    /// A rule-mandated payment of `owed` on (round, shard) found only `paid` to pay it; the rest
    /// is recorded in `owedOf(payee)`, claimable from the reward pool (`claimOwed`, ADR-0022
    /// Amendment 1).
    event SpendShort(
        uint64 indexed round,
        bytes32 indexed shard,
        address indexed payee,
        uint256 owed,
        uint256 paid
    );

    // Event reason codes (numbers, not strings, keep the runtime under EIP-170; ADR-0024).
    uint8 internal constant CR_STAKE = 0;
    uint8 internal constant CR_COORDINATOR = 1;
    uint8 internal constant CR_ROUND_VOIDED = 2;
    uint8 internal constant CR_SHARD_VOIDED = 3;
    uint8 internal constant CR_VERDICT = 4;
    uint8 internal constant CR_DISPUTE = 5;
    /// A recorded shortfall paid from the reward pool (`claimOwed`).
    uint8 internal constant CR_OWED = 6;
    /// A rerun case's extra validator fee (2nd+ revealed root) paid from the lost miner bonds.
    uint8 internal constant CR_EXTRA = 7;
    uint8 internal constant SL_UNREVEALED = 0;
    uint8 internal constant SL_MISSED = 1;
    uint8 internal constant SL_DISPUTE = 2;

    // ------------------------------------------------------------------ config

    IERC20Min public immutable token;
    address public immutable coordinator;
    /// `OnyxRegistry` binds a receipt key to (coordinator, epoch) (review REVLOG-R2 LOW-3). The
    /// coordinator here is immutable, so the epoch never changes; a ledger that can change its
    /// coordinator must bump it on every change.
    uint64 public constant coordinatorEpoch = 0;
    /// The dispute game (`OnyxDisputes`, ADR-0024): the only caller of the `d*` hooks. Its
    /// deployer binds it to this contract once (`OnyxDisputes.bind`); `openRound` refuses to
    /// open a round until it is bound here.
    address public immutable disputes;
    /// Network domain bound into every round tag (Rust `Registry::domain`).
    string public domain;
    uint64 public immutable unbondDelay;
    uint128 public immutable minValidatorStake;
    uint128 public immutable minCoordinatorBond;
    /// Taken from the coordinator bond when it does not open its canary list in time (L25-F7).
    uint128 public immutable canarySlash;

    uint256 public constant MAX_VALIDATORS = 128;
    uint256 public constant MAX_SHARDS = 64;
    uint256 public constant MAX_COMMITS_PER_SHARD = 32;
    /// A round needs at least this many bonded validators (M6-F3).
    uint256 public constant MIN_ROUND_MEMBERS = 3;
    /// Shortest allowed window, in blocks, for each phase (M6-F3).
    uint64 public constant MIN_WINDOW = OnyxRound.MIN_WINDOW;
    /// A seed must leave at least this many blocks for verdict commits; drawn later, the round
    /// is voided instead (M6-L37-F2).
    uint64 public constant MIN_VERDICT_WINDOW = OnyxRound.MIN_VERDICT_WINDOW;
    /// The seed can be drawn in at least this many blocks: `_checkConfig` requires
    /// `beaconHeight + MIN_VERDICT_WINDOW + MIN_DRAW_SLACK <= verdictCommitClose`, so the draw
    /// blocks `beaconHeight + 1 .. verdictCommitClose - MIN_VERDICT_WINDOW` number at least
    /// this many (M6-L44-F3). Mirrored in Rust (`onyx_coordinator::validate::MIN_DRAW_SLACK`).
    uint64 public constant MIN_DRAW_SLACK = OnyxRound.MIN_DRAW_SLACK;
    /// Voiding starts at least this long after the canary deadline (Rust `MIN_PENDING_GAP`).
    uint64 public constant MIN_PENDING_GAP = OnyxRound.MIN_PENDING_GAP;
    /// blockhash() only covers the 256 most recent blocks.
    uint64 public constant BEACON_WINDOW = 256;
    /// Longest a round may run, from opening to `pendingClose`, in blocks (about 2.3 days at
    /// Base's 2 s blocks). Every height in the config is ordered below `pendingClose`, so this
    /// bounds them all; without it a coordinator could lock every stake ~forever (M6-L35-F1).
    uint64 public constant MAX_ROUND_SPAN = OnyxRound.MAX_ROUND_SPAN;
    /// Longest dispute tail after `pendingClose` (`OnyxRound.disputeTail`), in blocks: stakes
    /// stay locked that long so every case can still slash (ADR-0024 item 3).
    uint64 public constant MAX_DISPUTE_TAIL = OnyxRound.MAX_DISPUTE_TAIL;

    // ------------------------------------------------------------------ state

    mapping(address => uint256) public balanceOf;
    uint256 public burned;
    uint256 public rewardPool;
    uint128 public coordinatorBond;
    uint64 public coordinatorLockedUntil;

    mapping(address => Validator) public validators;
    /// Active members in no particular order (swap-and-pop); `activeSet` sorts a copy.
    address[] internal _members;
    /// Position + 1 of an active member in `_members` (0 = not a member).
    mapping(address => uint256) internal _memberPos;

    /// round + 1 of the miner's first accepted commit (0 = none). A canary must be fresh:
    /// its first commit is in the canary's own round.
    mapping(address => uint64) public firstCommitRound;
    /// Shard ids ever finalized: never offered again.
    mapping(bytes32 => bool) public everFinalized;
    /// Shard ids a round offered that are not yet settled, still pending, in a case, or voided
    /// after their reveals became public: never offered in another round (Rust
    /// `Ledger::taken`: open, held and burned work, L24-F5, L26-F2, L30-F4, M5-L46-F1). Cleared
    /// only when the shard ends with no public reveal (`Empty`, or voided with no reveal), so
    /// the work can be offered again (L23-F4, M6-L3-F1).
    mapping(bytes32 => bool) public live;

    mapping(uint64 => Round) internal _rounds;
    mapping(uint64 => mapping(bytes32 => Shard)) internal _shards;
    mapping(uint64 => mapping(bytes32 => mapping(address => Commit))) internal commits;
    mapping(uint64 => mapping(address => bool)) public isRoundMember;
    mapping(uint64 => mapping(bytes32 => mapping(address => mapping(address => VerdictRec))))
        internal verdicts;
    mapping(uint64 => mapping(bytes32 => mapping(address => Tally))) internal tallies;
    /// Verdict commits a validator has not revealed in a round, across all its shards. Each is
    /// slashed `validatorSlash` once at round level, whatever its shard's outcome, as Rust does
    /// (M6-F7); kept as verdicts arrive so settling loops members only.
    mapping(uint64 => mapping(address => uint32)) public unrevealedVerdicts;

    uint256 internal _lock = 1;

    /// Rule-mandated payments that found no funds (`SpendShort`), per payee and in total;
    /// claimable from the reward pool (ADR-0022 Amendment 1).
    mapping(address => uint256) internal _owed;
    uint256 internal _owedTotal;

    constructor(
        IERC20Min token_,
        address coordinator_,
        address disputes_,
        string memory domain_,
        uint64 unbondDelay_,
        uint128 minValidatorStake_,
        uint128 minCoordinatorBond_,
        uint128 canarySlash_
    ) {
        token = token_;
        coordinator = coordinator_;
        disputes = disputes_;
        domain = domain_;
        unbondDelay = unbondDelay_;
        minValidatorStake = minValidatorStake_;
        minCoordinatorBond = minCoordinatorBond_;
        canarySlash = canarySlash_;
    }

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    modifier onlyDisputes() {
        if (msg.sender != disputes) revert NotDisputes();
        _;
    }

    modifier onlyCoordinator() {
        if (msg.sender != coordinator) revert NotCoordinator();
        _;
    }

    function _pull(address from, uint256 amount) internal {
        if (!token.transferFrom(from, address(this), amount)) revert TransferFailed();
    }

    function _pay(address to, uint256 amount) internal {
        if (!token.transfer(to, amount)) revert TransferFailed();
    }

    function _credit(address to, uint256 amount, uint8 reason) internal {
        if (amount == 0) return;
        balanceOf[to] += amount;
        emit Credited(to, amount, reason);
    }

    /// Leave the active set and start unbonding (never before any round lock ends).
    function _deactivate(address who, Validator storage v) internal returns (uint64 at) {
        _removeMember(who);
        v.active = false;
        at = uint64(block.number) + unbondDelay;
        if (v.lockedUntil > at) at = v.lockedUntil;
        v.unbondAt = at;
    }

    function _removeMember(address a) internal {
        uint256 pos = _memberPos[a];
        if (pos == 0) return;
        address last = _members[_members.length - 1];
        _members[pos - 1] = last;
        _memberPos[last] = pos;
        _members.pop();
        _memberPos[a] = 0;
    }

    function _slashValidator(address v, uint128 amount, uint8 reason) internal {
        Validator storage s = validators[v];
        uint128 take = amount < s.stake ? amount : s.stake;
        if (take == 0) return;
        s.stake -= take;
        burned += take;
        emit Slashed(v, take, reason);
        // Below the minimum: leave the set and start normal unbonding, so the rest can be
        // withdrawn after the delay instead of being stuck (M6-L34-F2).
        if (s.active && s.stake < minValidatorStake) {
            uint64 at = _deactivate(v, s);
            emit UnbondRequested(v, at);
        }
    }

    /// The block until which a round's members stay locked: `pendingClose` plus the dispute
    /// tail (ADR-0024 item 3). Bounded by `MAX_ROUND_SPAN + MAX_DISPUTE_TAIL` from opening.
    function _lockEnd(OnyxRound.RoundConfig memory c) internal pure returns (uint64) {
        // Fits: pendingClose <= open + MAX_ROUND_SPAN and the tail <= MAX_DISPUTE_TAIL.
        return uint64(c.validation.pendingClose + OnyxRound.disputeTail(c.dispute));
    }

    function _revealedRoot(Shard storage s, address miner) internal view returns (bytes32) {
        for (uint256 i = 0; i < s.reveals.length; i++) {
            if (s.reveals[i].miner == miner) return s.reveals[i].root;
        }
        return bytes32(0);
    }

    function _round(uint64 id) internal view returns (Round storage r) {
        r = _rounds[id];
        if (!r.exists) revert NoRound();
    }

    function _shard(uint64 id, bytes32 shard) internal view returns (Shard storage s) {
        s = _shards[id][shard];
        if (s.status == ShardStatus.None) revert UnknownShard();
    }

    function _settleRound(uint64 id, Round storage r) internal {
        if (r.roundSettled) return;
        r.roundSettled = true;
        uint256 slash = r.cfg.validation.validatorSlash;
        for (uint256 i = 0; i < r.members.length; i++) {
            address v = r.members[i];
            uint256 n = unrevealedVerdicts[id][v];
            if (n == 0) continue;
            // One slash per unrevealed commit, capped at what is left (Rust caps each at the
            // bond); n <= 2^32, so the product cannot overflow uint256.
            uint256 amount = n * slash;
            _slashValidator(
                v, amount > type(uint128).max ? type(uint128).max : uint128(amount), SL_UNREVEALED
            );
        }
        emit RoundSettled(id);
    }

    /// What one shard may draw from its round's budget: the miner reward, one fee per member
    /// and one rerun panel's fees. `openRound` moves exactly this per shard into the budget.
    function _shardAllot(Round storage r) internal view returns (uint256) {
        OnyxRound.ValidationConfig storage vc = r.cfg.validation;
        OnyxRound.DisputeConfig storage d = r.cfg.dispute;
        return uint256(vc.minerReward) + uint256(vc.validatorFee) * r.members.length
            + uint256(d.panelFee) * d.panelSize;
    }

    /// Pay a rule-mandated amount from the round budget, from (round, shard)'s own reservation
    /// only, so no shard's payouts can drain another's (ADR-0022 Amendment 1, review L2-R1).
    /// The rules never owe a shard more than its reservation from the budget (miner reward once,
    /// one fee per member, one panel's fees; extra rerun fees come from lost bonds, `dExtra`), so
    /// a short pay means a bug: it is never silent (`SpendShort`) and the rest is recorded owed.
    function _spend(
        Round storage r,
        uint64 id,
        bytes32 shard,
        address to,
        uint128 amount,
        uint8 reason
    ) internal {
        Shard storage s = _shards[id][shard];
        uint256 pay = _shardAllot(r) - s.spent;
        if (pay > r.budget) pay = r.budget;
        if (pay > amount) pay = amount;
        // pay <= amount <= uint128 max.
        s.spent += uint128(pay);
        r.budget -= uint128(pay);
        _credit(to, pay, reason);
        if (pay < amount) _owe(id, shard, to, amount, pay);
    }

    /// Record the unpaid rest of a rule-mandated payment (never silent, review L2-R1).
    function _owe(uint64 id, bytes32 shard, address to, uint256 amount, uint256 paid) internal {
        uint256 short = amount - paid;
        _owed[to] += short;
        _owedTotal += short;
        emit SpendShort(id, shard, to, amount, paid);
    }

    function _burn(uint256 amount) internal {
        burned += amount;
    }

    /// @notice True once no dispute can change the shard's root: `Finalized` and either its
    /// challenge window is over or a case decided it (ADR-0024 item 2). Readers settle on this.
    function _isFinal(uint64 id, bytes32 shard) internal view returns (bool) {
        Shard storage s = _shards[id][shard];
        if (s.status != ShardStatus.Finalized) return false;
        return s.closed
            || block.number >= uint256(s.finalizedAt) + _rounds[id].cfg.dispute.challengeWindow;
    }

    function _refundShard(uint64 id, bytes32 shard, Shard storage s) internal {
        if (s.status != ShardStatus.Open && s.status != ShardStatus.Pending) revert WrongPhase();
        s.status = ShardStatus.Voided;
        if (s.reveals.length == 0) live[shard] = false;
        for (uint256 j = 0; j < s.committers.length; j++) {
            address m = s.committers[j];
            _credit(m, commits[id][shard][m].bond, CR_ROUND_VOIDED);
        }
        emit ShardVoided(id, shard);
    }

    function _voidRound(uint64 id, Round storage r) internal {
        r.voided = true;
        rewardPool += r.budget;
        r.budget = 0;
        emit RoundVoided(id);
    }

    function _missCanaries(uint64 id, Round storage r) internal {
        r.canariesResolved = true;
        uint128 take = canarySlash < coordinatorBond ? canarySlash : coordinatorBond;
        coordinatorBond -= take;
        burned += take;
        emit CanaryDeadlineMissed(id, take);
    }

    /// @notice Ledger id of this contract: keccak256("onyx-ledger-evm-v1" || chain id ||
    /// address). Binding the chain id means a contract at the same address on another chain is
    /// another ledger, so signed commits never replay across chains (M6-F9).
    function ledgerId() public view returns (bytes32) {
        return OnyxRound.ledgerId(block.chainid, address(this));
    }

    /// @notice Sampling seed of a round (zero until drawn).
    function seedOf(uint64 id) external view returns (bytes32) {
        return _round(id).seed;
    }

    function roundMembers(uint64 id) external view returns (address[] memory) {
        return _round(id).members;
    }
}
