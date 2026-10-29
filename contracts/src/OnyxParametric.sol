// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IStakeToken, IOnyxFacts, IOnyxFactGuard, IOnyxReobserve} from "./IOnyxMarkets.sol";

/// @title OnyxParametric
/// @notice Parametric insurance on a future value of an Onyx fact (north-star loop NL18,
/// docs/spec/parametric-insurance.md, ADR-0042 amendment 3): a policy pays a fixed amount when the fact's
/// value crosses a line, with no claim and no loss assessment. Testnet demo: tONYX only; a payout
/// can differ from any real loss (basis risk).
///
/// The owner offers a product (fact, comparator, threshold, payout and premium per policy, when
/// buying closes, when the value can first be observed, when an unsettled product voids, and how
/// many policies at most). Underwriters deposit into one pool for shares; a policy is sold only
/// when the pool's free money covers its payout, which is then reserved (R-PI-5). After
/// `observableAt` anyone settles a product from the fact's finalized value at level 2 or below,
/// accepted at or after `observableAt` (R-PI-3: a stake vote or panel answer, level 4, never
/// settles a policy). Triggered: each policy holder claims the payout and the premiums join the
/// pool. Not triggered: the reserve is released and the premiums join the pool. No value by
/// `resolveBy`: each holder claims the premium back and the reserve is released.
///
/// Rules from the NL6 review: buying closes no later than the facts source's `closeBy` for the
/// fact, before the source publishes (R-PI-1, THREATS NL6-10); the fact is listed with its
/// earliest recording block at `observableAt` (R-PI-2, NL6-11); the owner, the facts source's
/// insiders and re-observation observers may neither buy nor underwrite (R-PI-4, NL6-12). An
/// underwriter leaving takes its share of the free money only, never of reserved money, so
/// leaving before a payout shifts no loss onto the others (THREATS NL18-4).
contract OnyxParametric {
    enum State {
        None,
        Open,
        Paid,
        Expired,
        Void
    }

    enum Cmp {
        AtLeast,
        AtMost
    }

    struct Product {
        bytes32 fact;
        Cmp cmp;
        State state;
        uint32 sold;
        uint32 maxPolicies;
        uint64 buyBy;
        uint64 observableAt;
        uint64 resolveBy;
        int256 threshold;
        uint128 payout;
        uint128 premium;
        int256 value;
    }

    /// The highest proof level that settles a policy (2: re-observed).
    uint8 public constant MAX_LEVEL = 2;
    /// Buying closes at least this long before the value can be observed.
    uint64 public constant MIN_LEAD = 1 hours;
    /// At most this many policies a buyer holds in one product.
    uint32 public constant MAX_PER_BUYER = 10;

    error NotOwner();
    error BadProduct();
    error WrongPhase();
    error NotAllowed();
    error BadAmount();
    error NotCovered();
    error NothingOwed();
    error TransferFailed();
    error Reentrancy();

    event Offered(
        uint256 indexed productId,
        bytes32 indexed fact,
        Cmp cmp,
        int256 threshold,
        uint128 payout,
        uint128 premium,
        uint64 buyBy,
        uint64 observableAt,
        uint64 resolveBy,
        uint32 maxPolicies
    );
    event Bought(uint256 indexed productId, address indexed who, uint32 policies);
    event Settled(uint256 indexed productId, State state, int256 value);
    event Claimed(uint256 indexed productId, address indexed who, uint256 amount);
    event Underwritten(address indexed who, uint256 amount, uint256 shares);
    event Withdrawn(address indexed who, uint256 amount, uint256 shares);

    IStakeToken public immutable token;
    IOnyxFacts public immutable facts;
    IOnyxFactGuard public immutable guard;
    IOnyxReobserve public immutable reobserve;
    address public immutable owner;

    uint256 public productCount;
    mapping(uint256 => Product) internal _products;
    mapping(uint256 => mapping(address => uint32)) public policiesOf;
    mapping(uint256 => mapping(address => bool)) public claimed;

    /// The pool's money (deposits, earned premiums, less payouts) and the part reserved for
    /// sold, unsettled policies; free money is `pool - reserved`.
    uint256 public pool;
    uint256 public reserved;
    /// Premiums of unsettled products (refunded if a product voids) and money owed to holders.
    uint256 public pending;
    uint256 public owedToHolders;
    uint256 public totalShares;
    mapping(address => uint256) public shares;
    uint256 internal _lock = 1;

    /// `guard_` must be the facts source itself (`OnyxObservedFacts`).
    constructor(
        IStakeToken token_,
        IOnyxFacts facts_,
        IOnyxFactGuard guard_,
        IOnyxReobserve reobserve_
    ) {
        if (address(guard_) != address(facts_) || address(guard_) == address(0)) {
            revert BadProduct();
        }
        token = token_;
        facts = facts_;
        guard = guard_;
        reobserve = reobserve_;
        owner = msg.sender;
    }

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    /// @notice Whether `who` may buy or underwrite: never the owner, an insider of the facts
    /// source (its listing owner, the registry's coordinator) or a re-observation observer.
    function mayTakePart(address who) public view returns (bool) {
        return who != owner && !guard.isInsider(who) && !reobserve.isObserver(who);
    }

    /// @notice Offer a product (owner only on testnet).
    function offer(
        bytes32 fact,
        Cmp cmp,
        int256 threshold,
        uint128 payout,
        uint128 premium,
        uint64 buyBy,
        uint64 observableAt,
        uint64 resolveBy,
        uint32 maxPolicies
    ) external returns (uint256 id) {
        if (msg.sender != owner) revert NotOwner();
        uint64 latest = guard.closeBy(fact);
        if (
            fact == bytes32(0) || premium == 0 || payout <= premium || maxPolicies == 0
                || buyBy <= block.timestamp || latest == 0 || buyBy > latest
                || uint256(buyBy) + MIN_LEAD > observableAt || resolveBy <= observableAt
        ) revert BadProduct();
        id = ++productCount;
        Product storage p = _products[id];
        (p.fact, p.cmp, p.threshold, p.payout, p.premium) = (fact, cmp, threshold, payout, premium);
        (p.buyBy, p.observableAt, p.resolveBy, p.maxPolicies) =
        (buyBy, observableAt, resolveBy, maxPolicies);
        p.state = State.Open;
        emit Offered(
            id, fact, cmp, threshold, payout, premium, buyBy, observableAt, resolveBy, maxPolicies
        );
    }

    /// @notice Deposit `amount` into the pool for shares at the pool's full value.
    function underwrite(uint256 amount) external nonReentrant {
        if (!mayTakePart(msg.sender)) revert NotAllowed();
        if (amount == 0) revert BadAmount();
        // One virtual share and one virtual token: a pool emptied by payouts (shares left, no
        // money) prices new money near zero for the old shares instead of handing it to them.
        uint256 s = amount * (totalShares + 1) / (pool + 1);
        if (s == 0) revert BadAmount();
        shares[msg.sender] += s;
        totalShares += s;
        pool += amount;
        emit Underwritten(msg.sender, amount, s);
        _pull(msg.sender, amount);
    }

    /// @notice Redeem `s` shares for their part of the free money only (reserved money stays
    /// with the remaining underwriters, who carry the open policies).
    function withdraw(uint256 s) external nonReentrant {
        if (s == 0 || s > shares[msg.sender]) revert BadAmount();
        uint256 amount = s * (pool - reserved) / totalShares;
        if (amount == 0) revert BadAmount();
        shares[msg.sender] -= s;
        totalShares -= s;
        pool -= amount;
        emit Withdrawn(msg.sender, amount, s);
        _push(msg.sender, amount);
    }

    /// @notice Buy `n` policies before buying closes; the free pool must cover their payout.
    function buy(uint256 productId, uint32 n) external nonReentrant {
        Product storage p = _products[productId];
        if (p.state != State.Open || block.timestamp >= p.buyBy) revert WrongPhase();
        if (!mayTakePart(msg.sender)) revert NotAllowed();
        if (
            n == 0 || p.sold + n > p.maxPolicies
                || policiesOf[productId][msg.sender] + n > MAX_PER_BUYER
        ) {
            revert BadAmount();
        }
        uint256 cover = uint256(p.payout) * n;
        if (cover > pool - reserved) revert NotCovered();
        uint256 cost = uint256(p.premium) * n;
        reserved += cover;
        pending += cost;
        p.sold += n;
        policiesOf[productId][msg.sender] += n;
        emit Bought(productId, msg.sender, n);
        _pull(msg.sender, cost);
    }

    /// @notice Settle from the fact's finalized value, or void when none came by `resolveBy`.
    function settle(uint256 productId) external {
        Product storage p = _products[productId];
        if (p.state != State.Open || block.timestamp < p.observableAt) revert WrongPhase();
        (bool fin, int256 value, uint8 level, uint64 at) = facts.factValue(p.fact);
        uint256 cover = uint256(p.payout) * p.sold;
        uint256 income = uint256(p.premium) * p.sold;
        if (fin && level >= 1 && level <= MAX_LEVEL && at >= p.observableAt) {
            p.value = value;
            bool hit = p.cmp == Cmp.AtLeast ? value >= p.threshold : value <= p.threshold;
            reserved -= cover;
            pending -= income;
            pool += income;
            if (hit) {
                p.state = State.Paid;
                pool -= cover;
                owedToHolders += cover;
            } else {
                p.state = State.Expired;
            }
            emit Settled(productId, p.state, value);
        } else if (block.timestamp >= p.resolveBy) {
            p.state = State.Void;
            reserved -= cover;
            pending -= income;
            owedToHolders += income;
            emit Settled(productId, State.Void, 0);
        } else {
            revert WrongPhase();
        }
    }

    /// @notice What `who` can claim from a settled product: the payout per policy when it
    /// paid, the premium per policy when it voided.
    function owed(uint256 productId, address who) public view returns (uint256) {
        if (claimed[productId][who]) return 0;
        Product storage p = _products[productId];
        uint256 n = policiesOf[productId][who];
        if (p.state == State.Paid) return n * p.payout;
        if (p.state == State.Void) return n * p.premium;
        return 0;
    }

    function claim(uint256 productId) external nonReentrant {
        uint256 amount = owed(productId, msg.sender);
        if (amount == 0) revert NothingOwed();
        claimed[productId][msg.sender] = true;
        owedToHolders -= amount;
        emit Claimed(productId, msg.sender, amount);
        _push(msg.sender, amount);
    }

    function product(uint256 productId) external view returns (Product memory) {
        return _products[productId];
    }

    /// @notice The money the contract must hold: the pool, unsettled premiums and claims owed.
    function liabilities() external view returns (uint256) {
        return pool + pending + owedToHolders;
    }

    function _pull(address from, uint256 amount) internal {
        if (!token.transferFrom(from, address(this), amount)) revert TransferFailed();
    }

    function _push(address to, uint256 amount) internal {
        if (!token.transfer(to, amount)) revert TransferFailed();
    }
}
