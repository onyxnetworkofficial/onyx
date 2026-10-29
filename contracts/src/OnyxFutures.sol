// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxPayout} from "./OnyxPayout.sol";
import {
    IStakeToken,
    IOnyxFacts,
    IOnyxFactGuard,
    IOnyxFallbackOracle,
    IOnyxReobserve
} from "./IOnyxMarkets.sol";

/// @title OnyxFutures
/// @notice Parimutuel markets on a future value of an Onyx fact (ROADMAP S6, ADR-0042 items
/// 11-14). A market names a fact (for example U.S. Treasury Debt to the Penny for a future
/// record date), bucket edges (one edge = over/under), when trading closes and when the value
/// can first be observed. Bets go into one pool per bucket until `tradingClose`, which is at
/// least `minCloseLead` before `observableAt`. The market resolves only from the fact's
/// finalized Onyx answer (`IOnyxFacts`) at a proof level of at most `maxLevel` (2: re-observed;
/// a value decided by a stake vote or the fallback oracle is level 4 and never resolves a
/// market), accepted at or after `observableAt`. Winners split the losing pools pro rata after
/// the treasury fee; a winning bucket with no bets, or no answer by `resolveBy`, voids the
/// market and refunds every bet. Observers, the fallback arbiter, the owner and the treasury may
/// not bet. With a `guard` (the facts source, `IOnyxFactGuard`), its insiders (the registry's
/// coordinator, the listing owner) may not bet either, and a market's trading closes no later
/// than the listing's `closeBy`, before the source publishes (THREATS NL6-10, NL6-12). Testnet
/// prototype: tONYX test token only.
contract OnyxFutures {
    enum State {
        None,
        Open,
        Resolved,
        Void
    }

    struct Params {
        uint128 minBet;
        uint128 maxBetPerAddress;
        uint128 maxPoolPerMarket;
        uint32 feePpm;
        uint64 minCloseLead;
        uint8 maxBuckets;
        uint8 maxLevel;
    }

    struct Market {
        bytes32 fact;
        uint64 tradingClose;
        uint64 observableAt;
        uint64 resolveBy;
        State state;
        uint8 winner;
        uint32 paramsId;
        uint32 bettors;
        uint32 winnersLeft;
        uint128 total;
        uint128 winningPool;
        uint128 distributable;
        uint128 left;
        int256 value;
        int256[] edges;
        uint128[] pools;
        uint32[] counts;
    }

    uint32 public constant MAX_FEE_PPM = 100_000;
    uint8 public constant MAX_BUCKETS = 16;

    error NotOwner();
    error BadParams();
    error BadMarket();
    error WrongPhase();
    error NotAllowed();
    error BadAmount();
    error CapReached();
    error NothingOwed();
    error TransferFailed();
    error Reentrancy();

    event ParamsSet(uint32 indexed id, Params p);
    event MarketOpened(
        uint256 indexed marketId,
        bytes32 indexed fact,
        int256[] edges,
        uint64 tradingClose,
        uint64 observableAt,
        uint64 resolveBy
    );
    event Bet(uint256 indexed marketId, address indexed who, uint8 bucket, uint128 amount);
    event Resolved(uint256 indexed marketId, int256 value, uint8 winner, uint128 fee);
    event Voided(uint256 indexed marketId, uint8 reason);
    event Paid(uint256 indexed marketId, address indexed who, uint256 amount);
    event TreasuryPaid(address indexed to, uint256 amount);

    IStakeToken public immutable token;
    IOnyxFacts public immutable facts;
    IOnyxReobserve public immutable reobserve;
    IOnyxFallbackOracle public immutable oracle;
    /// The facts source's listing rules and insiders; zero for a source without them.
    IOnyxFactGuard public immutable guard;
    address public immutable owner;
    address public treasury;

    Params[] internal _params;
    uint32 public currentParams;
    uint256 public marketCount;
    mapping(uint256 => Market) internal _markets;
    mapping(uint256 => mapping(address => mapping(uint8 => uint128))) public bets;
    mapping(uint256 => mapping(address => uint128)) public betTotal;
    mapping(uint256 => mapping(address => bool)) public paid;
    uint256 public treasuryOwed;
    uint256 public held;
    uint256 internal _lock = 1;

    constructor(
        IStakeToken token_,
        IOnyxFacts facts_,
        IOnyxReobserve reobserve_,
        IOnyxFallbackOracle oracle_,
        IOnyxFactGuard guard_,
        address treasury_,
        Params memory p
    ) {
        if (address(guard_) != address(0) && address(guard_) != address(facts_)) {
            revert BadParams();
        }
        token = token_;
        facts = facts_;
        reobserve = reobserve_;
        oracle = oracle_;
        guard = guard_;
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

    /// @notice The placeholder defaults (docs/spec/stakes-and-markets.md, a partner's table).
    function defaultParams() public pure returns (Params memory) {
        return Params({
            minBet: 1e18,
            maxBetPerAddress: 100e18,
            maxPoolPerMarket: 10_000e18,
            feePpm: 20_000,
            minCloseLead: 1 hours,
            maxBuckets: 8,
            maxLevel: 2
        });
    }

    function paramsValid(Params memory p) public pure returns (bool) {
        return p.minBet > 0 && p.minBet <= p.maxBetPerAddress
            && p.maxBetPerAddress <= p.maxPoolPerMarket && p.feePpm <= MAX_FEE_PPM
            && p.minCloseLead >= 60 && p.maxBuckets >= 2 && p.maxBuckets <= MAX_BUCKETS
            && p.maxLevel >= 1 && p.maxLevel <= 3;
    }

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

    /// @notice The bucket `value` falls in: the number of edges at or below it.
    function bucketOf(int256[] memory edges, int256 value) public pure returns (uint8 b) {
        uint256 n = edges.length;
        for (uint256 i = 0; i < n; i++) {
            if (edges[i] <= value) b++;
        }
    }

    /// @notice Open a market (owner only on testnet): edges strictly ascending, at most
    /// `maxBuckets - 1` of them; trading closes in the future and at least `minCloseLead` before
    /// the value can be observed; the answer is awaited until `resolveBy`.
    function createMarket(
        bytes32 fact,
        int256[] calldata edges,
        uint64 tradingClose,
        uint64 observableAt,
        uint64 resolveBy
    ) external onlyOwner returns (uint256 id) {
        Params storage p = _params[currentParams];
        uint256 n = edges.length;
        if (fact == bytes32(0) || n == 0 || n + 1 > p.maxBuckets) revert BadMarket();
        for (uint256 i = 1; i < n; i++) {
            if (edges[i] <= edges[i - 1]) revert BadMarket();
        }
        if (
            tradingClose <= block.timestamp || uint256(tradingClose) + p.minCloseLead > observableAt
                || resolveBy <= observableAt
        ) revert BadMarket();
        if (address(guard) != address(0)) {
            uint64 latest = guard.closeBy(fact);
            if (latest == 0 || tradingClose > latest) revert BadMarket();
        }
        id = ++marketCount;
        Market storage m = _markets[id];
        (m.fact, m.tradingClose, m.observableAt, m.resolveBy) =
        (fact, tradingClose, observableAt, resolveBy);
        m.state = State.Open;
        m.paramsId = currentParams;
        m.edges = edges;
        m.pools = new uint128[](n + 1);
        m.counts = new uint32[](n + 1);
        emit MarketOpened(id, fact, edges, tradingClose, observableAt, resolveBy);
    }

    /// @notice Whether `who` may bet: never a registered observer, the fallback arbiter, the
    /// owner, the treasury or an insider of the facts source.
    function mayBet(address who) public view returns (bool) {
        if (who == owner || who == treasury || who == oracle.arbiter()) return false;
        if (reobserve.isObserver(who)) return false;
        return address(guard) == address(0) || !guard.isInsider(who);
    }

    /// @notice Bet `amount` on `bucket` before trading closes.
    function bet(uint256 marketId, uint8 bucket, uint128 amount) external nonReentrant {
        Market storage m = _markets[marketId];
        if (m.state != State.Open || block.timestamp >= m.tradingClose) revert WrongPhase();
        if (!mayBet(msg.sender)) revert NotAllowed();
        if (bucket >= m.pools.length) revert BadMarket();
        Params storage p = _params[m.paramsId];
        uint128 mine = betTotal[marketId][msg.sender];
        if (amount < p.minBet || uint256(mine) + amount > p.maxBetPerAddress) revert BadAmount();
        if (uint256(m.total) + amount > p.maxPoolPerMarket) revert CapReached();
        if (mine == 0) m.bettors += 1;
        if (bets[marketId][msg.sender][bucket] == 0) m.counts[bucket] += 1;
        bets[marketId][msg.sender][bucket] += amount;
        betTotal[marketId][msg.sender] = mine + amount;
        m.pools[bucket] += amount;
        m.total += amount;
        held += amount;
        emit Bet(marketId, msg.sender, bucket, amount);
        _pull(msg.sender, amount);
    }

    /// @notice Resolve from the fact's finalized Onyx answer, or void: when the winning bucket
    /// is empty (reason 1) or no acceptable answer exists by `resolveBy` (reason 2).
    function resolve(uint256 marketId) external {
        Market storage m = _markets[marketId];
        if (m.state != State.Open || block.timestamp < m.tradingClose) revert WrongPhase();
        (bool fin, int256 value, uint8 level, uint64 at) = facts.factValue(m.fact);
        Params storage p = _params[m.paramsId];
        if (fin && level <= p.maxLevel && at >= m.observableAt) {
            uint8 w = bucketOf(m.edges, value);
            m.value = value;
            if (m.pools[w] == 0) {
                _void(marketId, m, 1);
                return;
            }
            m.state = State.Resolved;
            m.winner = w;
            m.winningPool = m.pools[w];
            uint256 losers = uint256(m.total) - m.winningPool;
            uint256 fee = OnyxPayout.fee(losers, p.feePpm);
            m.distributable = uint128(losers - fee);
            m.left = m.winningPool + m.distributable;
            m.winnersLeft = m.counts[w];
            held -= fee;
            treasuryOwed += fee;
            emit Resolved(marketId, value, w, uint128(fee));
        } else if (block.timestamp >= m.resolveBy) {
            _void(marketId, m, 2);
        } else {
            revert WrongPhase();
        }
    }

    function _void(uint256 marketId, Market storage m, uint8 reason) internal {
        m.state = State.Void;
        m.left = m.total;
        m.winnersLeft = m.bettors;
        emit Voided(marketId, reason);
    }

    /// @notice What `who` can withdraw from a resolved or void market.
    function owed(uint256 marketId, address who) public view returns (uint256) {
        Market storage m = _markets[marketId];
        if (paid[marketId][who]) return 0;
        if (m.state == State.Void) return betTotal[marketId][who];
        if (m.state != State.Resolved) return 0;
        return OnyxPayout.share(bets[marketId][who][m.winner], m.distributable, m.winningPool);
    }

    function withdraw(uint256 marketId) external nonReentrant {
        uint256 amount = owed(marketId, msg.sender);
        if (amount == 0) revert NothingOwed();
        Market storage m = _markets[marketId];
        paid[marketId][msg.sender] = true;
        m.left -= uint128(amount);
        m.winnersLeft -= 1;
        held -= amount;
        if (m.winnersLeft == 0 && m.left != 0) {
            uint256 dust = m.left;
            m.left = 0;
            held -= dust;
            treasuryOwed += dust;
        }
        emit Paid(marketId, msg.sender, amount);
        _push(msg.sender, amount);
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

    function _pull(address from, uint256 amount) internal {
        if (!token.transferFrom(from, address(this), amount)) revert TransferFailed();
    }

    function _push(address to, uint256 amount) internal {
        if (!token.transfer(to, amount)) revert TransferFailed();
    }
}
