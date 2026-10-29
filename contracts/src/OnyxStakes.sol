// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxPayout} from "./OnyxPayout.sol";
import {IStakeToken, IOnyxFacts, IOnyxFallbackOracle, IOnyxReobserve} from "./IOnyxMarkets.sol";

/// @title OnyxStakes
/// @notice Staked data submissions (ROADMAP S3, ADR-0042). A submitter bonds test tokens that a
/// value of a fact is right. During the claim's windows anyone else commits a hidden stake for
/// "correct" or "wrong" and reveals it. After the reveal window `resolve` decides, in order:
/// 1. an `Observed` claim (auto-pull past data) by its re-observation (S4, `OnyxReobserve`):
///    a strict majority of all assigned observers on one value decides, whatever the vote;
/// 2. otherwise the stake-weighted vote, only with at least `quorum` revealed and one side at
///    least `decisivePpm` of it (the submitter's bond never counts toward either);
/// 3. otherwise the fallback oracle, which answers before `fallbackWindow` ends or the claim is
///    void and every token goes back.
/// Losers pay `feePpm` of their money to the treasury, the rest goes to the winners pro rata
/// (`OnyxPayout`); unrevealed stakes are forfeited to the treasury; rounding dust goes to the
/// treasury after the last winner is paid. A value equal to a fact's accepted (or open) value is
/// a duplicate: nothing is taken and nothing is paid. Every economic number is a `Params` field,
/// snapshotted per claim when it opens. Testnet prototype: no real money (tONYX test token).
contract OnyxStakes is IOnyxFacts {
    // ------------------------------------------------------------------ types

    enum Kind {
        Observed,
        Voted
    }

    enum State {
        None,
        Open,
        Fallback,
        Correct,
        Wrong,
        Void,
        Duplicate
    }

    /// What decided a claim.
    enum By {
        None,
        Observation,
        Vote,
        Fallback,
        Timeout
    }

    /// Every economic and timing number of a claim (ADR-0042 item 10; a partner's table).
    struct Params {
        uint128 submitBond;
        uint128 submitReward;
        uint128 minStake;
        uint128 maxStakePerAddress;
        uint128 maxStakePerClaim;
        uint128 quorum;
        uint32 decisivePpm;
        uint32 feePpm;
        uint64 commitWindow;
        uint64 revealWindow;
        uint64 fallbackWindow;
    }

    struct Claim {
        bytes32 fact;
        int256 value;
        address submitter;
        Kind kind;
        State state;
        By by;
        uint32 paramsId;
        uint64 commitClose;
        uint64 revealClose;
        uint64 fallbackDeadline;
        uint64 decidedAt;
        uint128 bond;
        uint128 committed;
        uint128 correct;
        uint128 wrong;
        uint32 stakers;
        uint32 correctCount;
        uint32 wrongCount;
        uint32 winnersLeft;
        uint128 winningPool;
        uint128 distributable;
        /// Money still owed to this claim's winners (or refunds).
        uint128 left;
        uint128 reward;
    }

    struct Stake {
        uint128 amount;
        uint8 side;
        bool paid;
        bytes32 commit;
    }

    struct Fact {
        uint256 openClaim;
        uint256 acceptedClaim;
    }

    uint8 public constant CORRECT = 1;
    uint8 public constant WRONG = 2;
    uint256 internal constant PPM = 1_000_000;
    /// Parameter bounds checked by `setParams` (ADR-0042 item 10).
    uint64 public constant MIN_WINDOW = 60;
    uint64 public constant MAX_WINDOW = 30 days;
    uint32 public constant MAX_FEE_PPM = 100_000;
    bytes32 public constant COMMIT_TAG = keccak256("onyx-stake-commit-v1");

    // ------------------------------------------------------------------ errors and events

    error NotOwner();
    error BadParams();
    error ZeroFact();
    error AlreadyFinal();
    error FactBusy();
    error WrongPhase();
    error NotAllowed();
    error BadAmount();
    error CapReached();
    error AlreadyCommitted();
    error BadReveal();
    error NothingOwed();
    error TransferFailed();
    error Reentrancy();
    error BadSide();

    event ParamsSet(uint32 indexed id, Params p);
    event Submitted(
        uint256 indexed claimId,
        bytes32 indexed fact,
        address indexed submitter,
        int256 value,
        Kind kind
    );
    event Duplicate(uint256 indexed claimId, bytes32 indexed fact, address indexed submitter);
    event StakeCommitted(uint256 indexed claimId, address indexed staker, uint128 amount);
    event StakeRevealed(
        uint256 indexed claimId, address indexed staker, uint8 side, uint128 amount
    );
    event FellBack(uint256 indexed claimId, uint64 deadline);
    event Decided(uint256 indexed claimId, State state, By by, uint128 fee, uint128 toTreasury);
    event Paid(uint256 indexed claimId, address indexed who, uint256 amount);
    event TreasuryPaid(address indexed to, uint256 amount);
    event RewardsFunded(address indexed from, uint256 amount);

    // ------------------------------------------------------------------ state

    IStakeToken public immutable token;
    IOnyxReobserve public immutable reobserve;
    IOnyxFallbackOracle public immutable oracle;
    address public immutable owner;
    address public treasury;

    Params[] internal _params;
    uint32 public currentParams;
    uint256 public claimCount;
    mapping(uint256 => Claim) internal _claims;
    mapping(uint256 => mapping(address => Stake)) public stakes;
    mapping(bytes32 => Fact) public facts;
    mapping(uint256 => bool) internal _submitterPaid;

    /// Tokens owed to the treasury, paid out by `payTreasury`.
    uint256 public treasuryOwed;
    /// Tokens set aside for submitter rewards (`fundRewards`).
    uint256 public rewardPool;
    /// Tokens held for open claims and unpaid winners, bonds and refunds.
    uint256 public held;
    uint256 internal _lock = 1;

    constructor(
        IStakeToken token_,
        IOnyxReobserve reobserve_,
        IOnyxFallbackOracle oracle_,
        address treasury_,
        Params memory p
    ) {
        token = token_;
        reobserve = reobserve_;
        oracle = oracle_;
        owner = msg.sender;
        treasury = treasury_;
        _setParams(p);
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    // ------------------------------------------------------------------ parameters

    /// @notice The placeholder defaults of the parameter table (docs/spec/stakes-and-markets.md);
    /// a partner's token-economics split replaces them.
    function defaultParams() public pure returns (Params memory) {
        return Params({
            submitBond: 10e18,
            submitReward: 1e18,
            minStake: 1e18,
            maxStakePerAddress: 100e18,
            maxStakePerClaim: 1_000e18,
            quorum: 20e18,
            decisivePpm: 666_667,
            feePpm: 20_000,
            commitWindow: 1 days,
            revealWindow: 1 days,
            fallbackWindow: 3 days
        });
    }

    function paramsValid(Params memory p) public pure returns (bool) {
        return p.submitBond > 0 && p.minStake > 0 && p.minStake <= p.maxStakePerAddress
            && p.maxStakePerAddress <= p.maxStakePerClaim && p.quorum > 0 && p.decisivePpm > PPM / 2
            && p.decisivePpm < PPM && p.feePpm <= MAX_FEE_PPM && p.commitWindow >= MIN_WINDOW
            && p.commitWindow <= MAX_WINDOW && p.revealWindow >= MIN_WINDOW
            && p.revealWindow <= MAX_WINDOW && p.fallbackWindow >= MIN_WINDOW
            && p.fallbackWindow <= MAX_WINDOW;
    }

    /// @notice New parameters, for claims opened from now on; open claims keep theirs.
    function setParams(Params calldata p) external onlyOwner {
        _setParams(p);
    }

    function _setParams(Params memory p) internal {
        if (!paramsValid(p)) revert BadParams();
        _params.push(p);
        currentParams = uint32(_params.length - 1);
        emit ParamsSet(currentParams, p);
    }

    function params(uint32 id) external view returns (Params memory) {
        return _params[id];
    }

    function setTreasury(address t) external onlyOwner {
        if (t == address(0)) revert BadParams();
        treasury = t;
    }

    // ------------------------------------------------------------------ submissions

    /// @notice The fact key of a dated level-2 feed value, identical to
    /// `OnyxRegistry.factKeyOf(metric, template, subject, 0, true)` (ADR-0042 item 4).
    function datedFactKey(bytes32 metric, bytes32 template, bytes32 subject)
        public
        pure
        returns (bytes32)
    {
        return keccak256(
            abi.encodePacked("onyx-registry-fact-v1", metric, template, subject, uint64(0))
        );
    }

    /// @notice Bond `submitBond` that `value` is the value of `fact`. A value equal to the fact's
    /// accepted or open value is a duplicate: recorded, nothing taken, nothing paid. A different
    /// value while a claim is open (vote "wrong" on it instead) or after one was accepted is
    /// refused. One exception (THREATS STK-14): an `Observed` claim may replace a value that a
    /// vote or the fallback oracle accepted (level 4), so a cheap vote cannot lock a fact that
    /// observers can still read; a value accepted by observation (level 2) is final.
    function submit(bytes32 fact, int256 value, Kind kind)
        external
        nonReentrant
        returns (uint256 id)
    {
        if (fact == bytes32(0)) revert ZeroFact();
        Fact storage f = facts[fact];
        id = ++claimCount;
        Claim storage c = _claims[id];
        (c.fact, c.value, c.submitter, c.kind) = (fact, value, msg.sender, kind);
        uint256 other = f.openClaim;
        if (
            other == 0 && f.acceptedClaim != 0
                && !(kind == Kind.Observed && _claims[f.acceptedClaim].by != By.Observation)
        ) {
            other = f.acceptedClaim;
        }
        if (other != 0) {
            if (_claims[other].value != value) {
                if (other == f.acceptedClaim) revert AlreadyFinal();
                revert FactBusy();
            }
            c.state = State.Duplicate;
            emit Duplicate(id, fact, msg.sender);
            return id;
        }
        Params storage p = _params[currentParams];
        c.state = State.Open;
        c.paramsId = currentParams;
        c.bond = p.submitBond;
        c.commitClose = uint64(block.timestamp) + p.commitWindow;
        c.revealClose = c.commitClose + p.revealWindow;
        f.openClaim = id;
        held += p.submitBond;
        emit Submitted(id, fact, msg.sender, value, kind);
        if (kind == Kind.Observed) reobserve.open(id, fact, c.commitClose, c.revealClose);
        _pull(msg.sender, p.submitBond);
    }

    // ------------------------------------------------------------------ the vote

    /// @notice The hash a staker commits: `keccak256(abi.encode(COMMIT_TAG, claimId, staker,
    /// side, salt))`; the staker's address is inside, so a commit cannot be copied.
    function commitHash(uint256 claimId, address staker, uint8 side, bytes32 salt)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(COMMIT_TAG, claimId, staker, side, salt));
    }

    /// @notice Whether `who` may stake on `claimId`: never the submitter, the fallback oracle's
    /// arbiter, or a registered observer (they decide or answer the claim).
    function mayStake(uint256 claimId, address who) public view returns (bool) {
        Claim storage c = _claims[claimId];
        return who != c.submitter && who != oracle.arbiter() && !reobserve.isObserver(who);
    }

    /// @notice Commit a hidden side with `amount` tokens, once per address per claim, before
    /// the commit window closes. The amount is public; the side is not.
    function commitStake(uint256 claimId, bytes32 commit, uint128 amount) external nonReentrant {
        Claim storage c = _claims[claimId];
        if (c.state != State.Open || block.timestamp >= c.commitClose) revert WrongPhase();
        if (!mayStake(claimId, msg.sender)) revert NotAllowed();
        Stake storage s = stakes[claimId][msg.sender];
        if (s.amount != 0) revert AlreadyCommitted();
        Params storage p = _params[c.paramsId];
        if (amount < p.minStake || amount > p.maxStakePerAddress || commit == bytes32(0)) {
            revert BadAmount();
        }
        if (uint256(c.committed) + amount > p.maxStakePerClaim) revert CapReached();
        (s.amount, s.commit) = (amount, commit);
        c.committed += amount;
        c.stakers += 1;
        held += amount;
        emit StakeCommitted(claimId, msg.sender, amount);
        _pull(msg.sender, amount);
    }

    /// @notice Reveal the committed side during the reveal window.
    function revealStake(uint256 claimId, uint8 side, bytes32 salt) external {
        Claim storage c = _claims[claimId];
        if (
            c.state != State.Open || block.timestamp < c.commitClose
                || block.timestamp >= c.revealClose
        ) revert WrongPhase();
        if (side != CORRECT && side != WRONG) revert BadSide();
        Stake storage s = stakes[claimId][msg.sender];
        if (s.amount == 0 || s.side != 0) revert BadReveal();
        if (commitHash(claimId, msg.sender, side, salt) != s.commit) revert BadReveal();
        s.side = side;
        if (side == CORRECT) {
            c.correct += s.amount;
            c.correctCount += 1;
        } else {
            c.wrong += s.amount;
            c.wrongCount += 1;
        }
        emit StakeRevealed(claimId, msg.sender, side, s.amount);
    }

    // ------------------------------------------------------------------ resolution

    /// @notice What the vote alone says for revealed totals (ADR-0042 item 6): `CORRECT` or
    /// `WRONG` when at least `quorum` is revealed and that side holds at least `decisivePpm` of
    /// it; 0 otherwise (contested or low quorum).
    function voteOutcome(uint256 correct, uint256 wrong, uint256 quorum, uint256 decisivePpm)
        public
        pure
        returns (uint8)
    {
        uint256 r = correct + wrong;
        if (r == 0 || r < quorum) return 0;
        if (correct * PPM >= decisivePpm * r) return CORRECT;
        if (wrong * PPM >= decisivePpm * r) return WRONG;
        return 0;
    }

    /// @notice Decide a claim after its reveal window: observation first (an `Observed` claim),
    /// then the vote, else the fallback oracle.
    function resolve(uint256 claimId) external nonReentrant {
        Claim storage c = _claims[claimId];
        if (c.state != State.Open || block.timestamp < c.revealClose) revert WrongPhase();
        if (c.kind == Kind.Observed) {
            (uint8 status, int256 seen) = reobserve.outcome(claimId);
            if (status == 1) {
                _finish(claimId, c, seen == c.value ? State.Correct : State.Wrong, By.Observation);
                return;
            }
        }
        Params storage p = _params[c.paramsId];
        uint8 v = voteOutcome(c.correct, c.wrong, p.quorum, p.decisivePpm);
        if (v != 0) {
            _finish(claimId, c, v == CORRECT ? State.Correct : State.Wrong, By.Vote);
            return;
        }
        c.state = State.Fallback;
        c.fallbackDeadline = uint64(block.timestamp) + p.fallbackWindow;
        emit FellBack(claimId, c.fallbackDeadline);
        oracle.request(claimId);
    }

    /// @notice Settle a fallen-back claim from the oracle's answer, or void it once the
    /// fallback window has passed with no answer.
    function settleFallback(uint256 claimId) external nonReentrant {
        Claim storage c = _claims[claimId];
        if (c.state != State.Fallback) revert WrongPhase();
        uint8 a = oracle.answerOf(claimId);
        if (a == 1) {
            _finish(claimId, c, State.Correct, By.Fallback);
        } else if (a == 2) {
            _finish(claimId, c, State.Wrong, By.Fallback);
        } else if (a == 3) {
            _finish(claimId, c, State.Void, By.Fallback);
        } else if (block.timestamp >= c.fallbackDeadline) {
            _finish(claimId, c, State.Void, By.Timeout);
        } else {
            revert WrongPhase();
        }
    }

    function _finish(uint256 claimId, Claim storage c, State outcome, By by) internal {
        c.state = outcome;
        c.by = by;
        c.decidedAt = uint64(block.timestamp);
        Fact storage f = facts[c.fact];
        f.openClaim = 0;
        uint256 toTreasury;
        uint256 fee;
        if (outcome == State.Void) {
            c.left = c.bond + c.committed;
            c.winnersLeft = c.stakers + 1;
        } else {
            // Unrevealed stakes are forfeited to the treasury.
            toTreasury = uint256(c.committed) - c.correct - c.wrong;
            uint256 losers;
            if (outcome == State.Correct) {
                f.acceptedClaim = claimId;
                c.winningPool = c.correct + c.bond;
                losers = c.wrong;
                c.winnersLeft = c.correctCount + 1;
                Params storage p = _params[c.paramsId];
                uint256 r = p.submitReward <= rewardPool ? p.submitReward : rewardPool;
                rewardPool -= r;
                held += r;
                c.reward = uint128(r);
            } else {
                c.winningPool = c.wrong;
                losers = uint256(c.correct) + c.bond;
                c.winnersLeft = c.wrongCount;
            }
            if (c.winningPool == 0) {
                fee = losers;
            } else {
                fee = OnyxPayout.fee(losers, _params[c.paramsId].feePpm);
                c.distributable = uint128(losers - fee);
                c.left = c.winningPool + c.distributable;
            }
            toTreasury += fee;
        }
        held -= toTreasury;
        treasuryOwed += toTreasury;
        emit Decided(claimId, outcome, by, uint128(fee), uint128(toTreasury));
    }

    // ------------------------------------------------------------------ payouts

    /// @notice What `who` can withdraw from a decided claim (0 when nothing or already paid).
    function owed(uint256 claimId, address who) public view returns (uint256 amount) {
        Claim storage c = _claims[claimId];
        State st = c.state;
        if (st != State.Correct && st != State.Wrong && st != State.Void) return 0;
        if (who == c.submitter && !_submitterPaid[claimId]) {
            if (st == State.Void) {
                amount += c.bond;
            } else if (st == State.Correct) {
                amount += OnyxPayout.share(c.bond, c.distributable, c.winningPool) + c.reward;
            }
        }
        Stake storage s = stakes[claimId][who];
        if (s.amount != 0 && !s.paid) {
            if (st == State.Void) {
                amount += s.amount;
            } else if (s.side == (st == State.Correct ? CORRECT : WRONG)) {
                amount += OnyxPayout.share(s.amount, c.distributable, c.winningPool);
            }
        }
    }

    /// @notice Withdraw everything owed to the caller on `claimId`.
    function withdraw(uint256 claimId) external nonReentrant {
        uint256 amount = owed(claimId, msg.sender);
        if (amount == 0) revert NothingOwed();
        Claim storage c = _claims[claimId];
        uint256 reward;
        uint32 winners;
        if (msg.sender == c.submitter && !_submitterPaid[claimId]) {
            _submitterPaid[claimId] = true;
            if (c.state != State.Wrong) winners += 1;
            reward = c.reward;
            c.reward = 0;
        }
        Stake storage s = stakes[claimId][msg.sender];
        if (s.amount != 0 && !s.paid) {
            s.paid = true;
            if (c.state == State.Void || s.side == (c.state == State.Correct ? CORRECT : WRONG)) {
                winners += 1;
            }
        }
        c.left -= uint128(amount - reward);
        c.winnersLeft -= winners;
        held -= amount;
        if (c.winnersLeft == 0 && c.left != 0) {
            // Rounding dust: the last winner is paid, what is left goes to the treasury.
            uint256 dust = c.left;
            c.left = 0;
            held -= dust;
            treasuryOwed += dust;
        }
        emit Paid(claimId, msg.sender, amount);
        _push(msg.sender, amount);
    }

    /// @notice Add tokens to the submitter reward pool.
    function fundRewards(uint256 amount) external nonReentrant {
        rewardPool += amount;
        emit RewardsFunded(msg.sender, amount);
        _pull(msg.sender, amount);
    }

    /// @notice Pay what the treasury is owed to `treasury`.
    function payTreasury() external nonReentrant {
        uint256 a = treasuryOwed;
        if (a == 0) revert NothingOwed();
        treasuryOwed = 0;
        emit TreasuryPaid(treasury, a);
        _push(treasury, a);
    }

    // ------------------------------------------------------------------ views

    function claim(uint256 claimId) external view returns (Claim memory) {
        return _claims[claimId];
    }

    /// @inheritdoc IOnyxFacts
    function factValue(bytes32 fact)
        external
        view
        returns (bool final_, int256 value, uint8 level, uint64 acceptedAt)
    {
        uint256 id = facts[fact].acceptedClaim;
        if (id == 0) return (false, 0, 0, 0);
        Claim storage c = _claims[id];
        return (true, c.value, c.by == By.Observation ? 2 : 4, c.decidedAt);
    }

    /// @notice ADR-0042 item 9: the smallest stake that wins a vote-decided claim outright
    /// against `honest` revealed on the other side, under parameter set `paramsId`; 0 when the
    /// per-claim cap makes it impossible.
    function minAttackStake(uint32 paramsId, uint256 honest) external view returns (uint256) {
        Params storage p = _params[paramsId];
        return
            OnyxPayout.minAttackStake(
                honest, p.quorum, p.decisivePpm, p.minStake, p.maxStakePerClaim
            );
    }

    // ------------------------------------------------------------------ token moves

    function _pull(address from, uint256 amount) internal {
        if (!token.transferFrom(from, address(this), amount)) revert TransferFailed();
    }

    function _push(address to, uint256 amount) internal {
        if (!token.transfer(to, amount)) revert TransferFailed();
    }
}
