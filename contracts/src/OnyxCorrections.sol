// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxPayout} from "./OnyxPayout.sol";
import {IStakeToken} from "./IOnyxMarkets.sol";
import {OnyxRegistry} from "./OnyxRegistry.sol";

/// @title OnyxCorrections
/// @notice Correction markets (docs/contributor-loop.md CL7, ADR-0057): one Yes ("accurate") /
/// No market per published numeric level-2 value. The value is a result `OnyxRegistry` recorded
/// for a dated metric; its metric is paired (owner, append-only) with a `.recheck` metric whose
/// template reads the same source later (`onyx_core::recheck_of`). A market settles only from the
/// recheck fact's recorded result, from a round whose draw was announced after trading closed
/// and that was recorded by `resolveBlock`: Yes wins when the recheck value is within the
/// pair's tolerance of the original, No otherwise. Winners split the losing side pro rata after
/// the treasury fee (`OnyxPayout`, parimutuel). There is no vote, no panel and no owner answer:
/// with no valid recheck the market voids and every bet is refunded. Test network only (tONYX).
///
/// ENFORCED here: the opened value is the registry's recorded result for the original fact
/// (`resultHash(spec, round, value)`, the `OnyxObservedFacts` rule); the recheck fact had no
/// result when the market opened; several markets of one value may run (review CL7-R1-1: one
/// opener cannot block a value), each settled by the one recheck when it is timely for it; the
/// tolerance is the pair's; insiders (the owner, the registry's coordinator, the treasury) never
/// bet; each side of a market holds at most half the pool cap (CL7-R1-3); the draw that recorded
/// the recheck (the fact's latest) was announced after `closeBlock` (so its pool snapshot and its
/// beacon come after the last bet), its result recorded by `resolveBlock`; the
/// settled recheck value is the recorded result for the recheck fact's recorded spec; every token
/// in is owed to someone (`liabilities()` equals the balance), rounding goes to the treasury.
/// ATTESTED (by the registry's coordinator, as for every level-2 fact, ADR-0042 amendment 2 item
/// 5): that the recorded specs are the templates instantiated for the subject, that the recheck
/// pool left out every bettor and the original observers (ADR-0057 item 5), and that the recorded
/// results are what the reveals aggregate to. A client checks them off chain.
contract OnyxCorrections {
    enum State {
        None,
        Open,
        Resolved,
        Void
    }

    /// Placeholder economics (ADR-0057 open question 1), fixed per deployment: nobody can change
    /// them under an open market.
    struct Params {
        uint128 openFee;
        uint128 minBet;
        uint128 maxBetPerAddress;
        uint128 maxPoolPerMarket;
        uint32 feePpm;
        /// Fewest blocks from opening to the trading close.
        uint32 minTradingBlocks;
        /// Fewest blocks from the trading close to `resolveBlock` (room for the recheck round).
        uint32 minRecheckBlocks;
        /// Most blocks from opening to `resolveBlock` (`MAX_MARKET_BLOCKS`).
        uint32 maxMarketBlocks;
        /// Blocks after `resolveBlock` during which a valid recheck can still be resolved before
        /// anyone may void the market.
        uint32 graceBlocks;
    }

    struct Pair {
        bytes32 recheckMetric;
        uint128 tolerance;
    }

    struct Market {
        bytes32 metric;
        bytes32 subjectKey;
        bytes32 fact;
        bytes32 recheckFact;
        address opener;
        uint64 windowEnd;
        uint64 closeBlock;
        uint64 resolveBlock;
        State state;
        /// 1: Yes ("accurate"), 2: No.
        uint8 winner;
        /// Void: 1 the winning side had no bet, 2 no valid recheck.
        uint8 voidReason;
        uint32 bettors;
        uint32 winnersLeft;
        int128 value;
        int128 recheckValue;
        uint128 tolerance;
        uint128 openFee;
        uint128 total;
        uint128 winningPool;
        uint128 distributable;
        uint128 left;
        uint128[2] pools;
        uint32[2] counts;
    }

    uint8 public constant YES = 1;
    uint8 public constant NO = 2;
    uint32 public constant MAX_FEE_PPM = 100_000;

    error NotOwner();
    error BadParams();
    error BadPair();
    error NotPaired();
    error BadMarket();
    error NoResult();
    error WrongValue();
    error RecheckStarted();
    error WrongPhase();
    error NotAllowed();
    error BadAmount();
    error CapReached();
    error BadRecheck();
    error NothingOwed();
    error TransferFailed();
    error Reentrancy();

    event Paired(
        bytes32 indexed metric,
        bytes32 indexed recheckMetric,
        bytes32 template,
        bytes32 recheckTemplate,
        uint128 tolerance
    );
    event MarketOpened(
        uint256 indexed marketId,
        bytes32 indexed fact,
        bytes32 indexed recheckFact,
        address opener,
        int128 value,
        uint128 tolerance,
        uint64 closeBlock,
        uint64 resolveBlock
    );
    event Bet(uint256 indexed marketId, address indexed who, uint8 side, uint128 amount);
    event Resolved(uint256 indexed marketId, int128 recheckValue, uint8 winner, uint128 fee);
    event Voided(uint256 indexed marketId, uint8 reason, bool feeRefunded);
    event Paid(uint256 indexed marketId, address indexed who, uint256 amount);
    event FeeRefunded(address indexed who, uint256 amount);
    event TreasuryPaid(address indexed to, uint256 amount);

    IStakeToken public immutable token;
    OnyxRegistry public immutable registry;
    address public immutable owner;
    address public immutable treasury;

    uint128 public immutable openFee;
    uint128 public immutable minBet;
    uint128 public immutable maxBetPerAddress;
    uint128 public immutable maxPoolPerMarket;
    uint32 public immutable feePpm;
    uint32 public immutable minTradingBlocks;
    uint32 public immutable minRecheckBlocks;
    uint32 public immutable maxMarketBlocks;
    uint32 public immutable graceBlocks;

    /// Original metric -> its recheck metric and tolerance (append-only).
    mapping(bytes32 => Pair) public pairs;
    /// Recheck metric -> the original metric it rechecks (one original per recheck metric).
    mapping(bytes32 => bytes32) public recheckOf;

    uint256 public marketCount;
    mapping(uint256 => Market) internal _markets;
    /// Original fact -> its latest market (a void market without a recheck frees the value).
    mapping(bytes32 => uint256) public marketOf;
    mapping(uint256 => mapping(address => uint128[2])) internal _bets;
    mapping(uint256 => mapping(address => uint128)) public betTotal;
    mapping(uint256 => mapping(address => bool)) public paid;

    /// Bets and dust not yet paid out, over every market.
    uint256 public held;
    /// Open fees of markets not yet resolved or void.
    uint256 public feeEscrow;
    uint256 public treasuryOwed;
    uint256 public openerOwedTotal;
    mapping(address => uint256) public openerOwed;
    uint256 internal _lock = 1;

    constructor(IStakeToken token_, OnyxRegistry registry_, address treasury_, Params memory p) {
        if (
            address(token_) == address(0) || address(registry_) == address(0)
                || treasury_ == address(0) || p.minBet == 0 || p.minBet > p.maxBetPerAddress
                || p.maxBetPerAddress > p.maxPoolPerMarket / 2 || p.feePpm > MAX_FEE_PPM
                || p.minTradingBlocks == 0 || p.minRecheckBlocks == 0
                || uint256(p.minTradingBlocks) + p.minRecheckBlocks > p.maxMarketBlocks
                || p.graceBlocks == 0
        ) revert BadParams();
        token = token_;
        registry = registry_;
        owner = msg.sender;
        treasury = treasury_;
        openFee = p.openFee;
        minBet = p.minBet;
        maxBetPerAddress = p.maxBetPerAddress;
        maxPoolPerMarket = p.maxPoolPerMarket;
        feePpm = p.feePpm;
        minTradingBlocks = p.minTradingBlocks;
        minRecheckBlocks = p.minRecheckBlocks;
        maxMarketBlocks = p.maxMarketBlocks;
        graceBlocks = p.graceBlocks;
    }

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    // ------------------------------------------------------------------ pairing (owner)

    /// @notice Pair `metric` with its recheck metric (owner, append-only; ADR-0057 item 3).
    /// Both must be registered dated feeds (one fact per date); `tolerance` is the recheck
    /// template's numeric tolerance, set by review. A client recomputes `recheck_of(template)`
    /// and compares its id with the registered recheck template (THREATS CL7-8).
    function pair(bytes32 metric, bytes32 recheckMetric, uint128 tolerance) external {
        if (msg.sender != owner) revert NotOwner();
        if (metric == recheckMetric || metric == bytes32(0) || recheckMetric == bytes32(0)) {
            revert BadPair();
        }
        if (pairs[metric].recheckMetric != bytes32(0) || recheckOf[recheckMetric] != bytes32(0)) {
            revert BadPair();
        }
        // No chains of rechecks in v1 (ADR-0057 open question 3).
        if (recheckOf[metric] != bytes32(0) || pairs[recheckMetric].recheckMetric != bytes32(0)) {
            revert BadPair();
        }
        (OnyxRegistry.Kind k, bytes32 t,) = registry.metrics(metric);
        (OnyxRegistry.Kind rk, bytes32 rt,) = registry.metrics(recheckMetric);
        if (k != OnyxRegistry.Kind.DatedFeed || rk != OnyxRegistry.Kind.DatedFeed) {
            revert BadPair();
        }
        pairs[metric] = Pair(recheckMetric, tolerance);
        recheckOf[recheckMetric] = metric;
        emit Paired(metric, recheckMetric, t, rt, tolerance);
    }

    // ------------------------------------------------------------------ markets

    /// @notice Whether `who` may bet: never the owner, the treasury or the registry's coordinator
    /// (it decides when, and whether, the recheck is recorded).
    function mayBet(address who) public view returns (bool) {
        return who != owner && who != treasury && who != registry.ledger().coordinator();
    }

    /// @notice Open the correction market of a recorded value, paying `openFee`. Anyone.
    /// `round` and `value` must hash, with the recorded spec, to the original fact's recorded
    /// result (THREATS CL7-1); the recheck fact must have no result yet. Other markets of the
    /// same value may be open (review CL7-R1-1): a market with a hopeless close blocks nobody.
    function open(
        bytes32 metric,
        bytes32 subjectKey,
        uint64 windowEnd,
        bytes32 round,
        int128 value,
        uint64 closeBlock,
        uint64 resolveBlock
    ) external nonReentrant returns (uint256 id) {
        Pair memory p = pairs[metric];
        if (p.recheckMetric == bytes32(0)) revert NotPaired();
        if (
            closeBlock < block.number + minTradingBlocks
                || resolveBlock < uint256(closeBlock) + minRecheckBlocks
                || resolveBlock > block.number + maxMarketBlocks
        ) revert BadMarket();
        bytes32 fact = registry.factKey(metric, subjectKey, windowEnd);
        OnyxRegistry.Observation memory o = registry.observationOf(metric, subjectKey, windowEnd);
        if (o.result == bytes32(0)) revert NoResult();
        if (resultHash(o.spec, round, value) != o.result) revert WrongValue();
        bytes32 rfact = registry.factKey(p.recheckMetric, subjectKey, windowEnd);
        if (registry.observationOf(p.recheckMetric, subjectKey, windowEnd).result != bytes32(0)) {
            revert RecheckStarted();
        }
        id = ++marketCount;
        marketOf[fact] = id;
        Market storage m = _markets[id];
        (m.metric, m.subjectKey, m.fact, m.recheckFact) = (metric, subjectKey, fact, rfact);
        (m.opener, m.windowEnd, m.closeBlock, m.resolveBlock) =
        (msg.sender, windowEnd, closeBlock, resolveBlock);
        (m.value, m.tolerance, m.openFee) = (value, p.tolerance, openFee);
        m.state = State.Open;
        feeEscrow += openFee;
        emit MarketOpened(id, fact, rfact, msg.sender, value, p.tolerance, closeBlock, resolveBlock);
        if (openFee != 0) _pull(msg.sender, openFee);
    }

    /// @notice Bet `amount` on `side` (1 Yes "accurate", 2 No) before the trading close.
    function bet(uint256 marketId, uint8 side, uint128 amount) external nonReentrant {
        Market storage m = _markets[marketId];
        if (m.state != State.Open || block.number >= m.closeBlock) revert WrongPhase();
        if (!mayBet(msg.sender)) revert NotAllowed();
        if (side != YES && side != NO) revert BadMarket();
        uint128 mine = betTotal[marketId][msg.sender];
        if (amount < minBet || uint256(mine) + amount > maxBetPerAddress) revert BadAmount();
        // Each side at most half the cap: one side cannot be filled to shut the other out at no
        // risk (review CL7-R1-3).
        if (uint256(m.pools[side - 1]) + amount > maxPoolPerMarket / 2) revert CapReached();
        uint128[2] storage b = _bets[marketId][msg.sender];
        if (mine == 0) m.bettors += 1;
        if (b[side - 1] == 0) m.counts[side - 1] += 1;
        b[side - 1] += amount;
        betTotal[marketId][msg.sender] = mine + amount;
        m.pools[side - 1] += amount;
        m.total += amount;
        held += amount;
        emit Bet(marketId, msg.sender, side, amount);
        _pull(msg.sender, amount);
    }

    /// @notice Whether the recheck fact's recorded result (if any) can settle `m`: the draw
    /// that recorded it (the fact's latest: no draw follows a result) was announced after the
    /// close (THREATS CL7-2, CL7-10) and it was recorded by `resolveBlock`.
    function _recheckTimely(Market storage m, OnyxRegistry.Observation memory o)
        internal
        view
        returns (bool)
    {
        if (o.result == bytes32(0) || o.recordedAt > m.resolveBlock) return false;
        return _latestDrawAt(m) > m.closeBlock;
    }

    /// @notice The block the recheck fact's latest draw was announced at; 0 with no draw.
    function _latestDrawAt(Market storage m) internal view returns (uint64) {
        uint32 n = registry.drawCount(m.recheckFact);
        return n == 0 ? 0 : registry.drawOf(m.recheckFact, n - 1).announcedAt;
    }

    function _recheckObservation(Market storage m)
        internal
        view
        returns (OnyxRegistry.Observation memory)
    {
        return registry.observationOf(pairs[m.metric].recheckMetric, m.subjectKey, m.windowEnd);
    }

    /// @notice Settle from the recheck fact's recorded result: `round` and `recheckValue` must
    /// hash, with the recorded recheck spec, to it. Anyone. Yes wins when the recheck value is
    /// within the tolerance of the original value; else No.
    function resolve(uint256 marketId, bytes32 round, int128 recheckValue) external {
        Market storage m = _markets[marketId];
        if (m.state != State.Open || block.number < m.closeBlock) revert WrongPhase();
        OnyxRegistry.Observation memory o = _recheckObservation(m);
        if (!_recheckTimely(m, o)) revert BadRecheck();
        if (resultHash(o.spec, round, recheckValue) != o.result) revert WrongValue();
        int256 d = int256(recheckValue) - int256(m.value);
        uint256 dist = uint256(d < 0 ? -d : d);
        uint8 w = dist <= m.tolerance ? YES : NO;
        m.recheckValue = recheckValue;
        m.winner = w;
        // The recheck round ran: its fee is the treasury's (the operator's runner).
        feeEscrow -= m.openFee;
        treasuryOwed += m.openFee;
        uint128 wp = m.pools[w - 1];
        if (wp == 0) {
            _void(marketId, m, 1, false);
            return;
        }
        m.state = State.Resolved;
        m.winningPool = wp;
        uint256 losers = uint256(m.total) - wp;
        uint256 fee = OnyxPayout.fee(losers, feePpm);
        m.distributable = uint128(losers - fee);
        m.left = wp + m.distributable;
        m.winnersLeft = m.counts[w - 1];
        held -= fee;
        treasuryOwed += fee;
        emit Resolved(marketId, recheckValue, w, uint128(fee));
    }

    /// @notice Void a market that can no longer settle: at once when the recheck was recorded
    /// from a draw announced at or before the close, after `resolveBlock` with no timely recheck,
    /// or after `resolveBlock + graceBlocks` whatever the recheck. Anyone. Every bet is refunded;
    /// the open fee too unless a recheck round was drawn for this market (after its close):
    /// a round drawn before the close is the coordinator's timing, not the opener's (review
    /// CL7-R1-2, CL7-R1-4).
    function void(uint256 marketId) external {
        Market storage m = _markets[marketId];
        if (m.state != State.Open) revert WrongPhase();
        uint64 drawnAt = _latestDrawAt(m);
        OnyxRegistry.Observation memory o = _recheckObservation(m);
        bool early = o.result != bytes32(0) && drawnAt <= m.closeBlock;
        if (!early) {
            if (block.number <= m.resolveBlock) revert WrongPhase();
            // A timely recheck is still waiting for its resolution.
            if (block.number <= uint256(m.resolveBlock) + graceBlocks && _recheckTimely(m, o)) {
                revert WrongPhase();
            }
        }
        bool refund = drawnAt <= m.closeBlock;
        feeEscrow -= m.openFee;
        if (refund) {
            openerOwed[m.opener] += m.openFee;
            openerOwedTotal += m.openFee;
        } else {
            treasuryOwed += m.openFee;
        }
        _void(marketId, m, 2, refund);
    }

    function _void(uint256 marketId, Market storage m, uint8 reason, bool feeRefunded) internal {
        m.state = State.Void;
        m.voidReason = reason;
        m.left = m.total;
        m.winnersLeft = m.bettors;
        emit Voided(marketId, reason, feeRefunded);
    }

    /// @notice A bettor's stake on each side (Yes, No).
    function betsOf(uint256 marketId, address who) external view returns (uint128 yes, uint128 no) {
        uint128[2] storage b = _bets[marketId][who];
        return (b[0], b[1]);
    }

    /// @notice What `who` can withdraw from a resolved or void market.
    function owed(uint256 marketId, address who) public view returns (uint256) {
        Market storage m = _markets[marketId];
        if (paid[marketId][who]) return 0;
        if (m.state == State.Void) return betTotal[marketId][who];
        if (m.state != State.Resolved) return 0;
        return OnyxPayout.share(_bets[marketId][who][m.winner - 1], m.distributable, m.winningPool);
    }

    function withdraw(uint256 marketId) external nonReentrant {
        uint256 amount = owed(marketId, msg.sender);
        if (amount == 0) revert NothingOwed();
        Market storage m = _markets[marketId];
        paid[marketId][msg.sender] = true;
        m.left -= uint128(amount);
        m.winnersLeft -= 1;
        held -= amount;
        // Rounding dust goes to the treasury once the last winner is paid, never to a trader.
        if (m.winnersLeft == 0 && m.left != 0) {
            uint256 dust = m.left;
            m.left = 0;
            held -= dust;
            treasuryOwed += dust;
        }
        emit Paid(marketId, msg.sender, amount);
        _push(msg.sender, amount);
    }

    /// @notice The open fees refunded to `msg.sender` from void markets nobody tried to recheck.
    function claimFeeRefund() external nonReentrant {
        uint256 a = openerOwed[msg.sender];
        if (a == 0) revert NothingOwed();
        openerOwed[msg.sender] = 0;
        openerOwedTotal -= a;
        emit FeeRefunded(msg.sender, a);
        _push(msg.sender, a);
    }

    function payTreasury() external nonReentrant {
        uint256 a = treasuryOwed;
        if (a == 0) revert NothingOwed();
        treasuryOwed = 0;
        emit TreasuryPaid(treasury, a);
        _push(treasury, a);
    }

    function market(uint256 marketId) external view returns (Market memory) {
        return _markets[marketId];
    }

    /// @notice Every token the contract owes: unpaid bets and dust, open fees in escrow, the
    /// treasury's and the openers' refunds. Equals the token balance (nothing is ever stuck).
    function liabilities() external view returns (uint256) {
        return held + feeEscrow + treasuryOwed + openerOwedTotal;
    }

    /// @notice `keccak256("onyx-observe-result-v1" ‖ spec ‖ round ‖ 0x00 ‖ 0x01 ‖ value as
    /// i128 little-endian)`, the `OnyxObservedFacts.resultHash` rule (vectors in
    /// fixtures/markets/observed-facts.json).
    function resultHash(bytes32 spec, bytes32 round, int128 value) public pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-observe-result-v1", spec, round, uint8(0), uint8(1), _le128(value)
            )
        );
    }

    function _le128(int128 v) internal pure returns (bytes16 out) {
        uint128 u = uint128(v);
        uint128 r;
        for (uint256 i = 0; i < 16; i++) {
            r = (r << 8) | (u & 0xff);
            u >>= 8;
        }
        out = bytes16(r);
    }

    function _pull(address from, uint256 amount) internal {
        if (!token.transferFrom(from, address(this), amount)) revert TransferFailed();
    }

    function _push(address to, uint256 amount) internal {
        if (!token.transfer(to, amount)) revert TransferFailed();
    }
}
