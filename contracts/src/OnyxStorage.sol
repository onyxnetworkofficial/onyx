// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxSignedBook} from "./OnyxSignedBook.sol";
import {OnyxStorageLib as L} from "./OnyxStorageLib.sol";

/// @notice The token calls the storage network makes (tONYX, `OnyxTestToken`).
interface IStorageToken {
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function balanceOf(address account) external view returns (uint256);
}

/// @title OnyxStorage
/// @notice TESTNET ONLY. The storage network (ADR-0060, docs/spec/storage-network.md,
/// docs/STORAGE-NETWORK.md): storage nodes bond tONYX and declare capacity; a publisher buys
/// storage for an anchored bulk manifest (ADR-0059) with a deposit per GiB per epoch for R
/// replicas; each shard is placed on R nodes by rendezvous hashing over the node list; every
/// epoch a seed from a past block hash picks challenge slots per deal, and the challenged holders
/// answer here with the picked row and its two Merkle paths, checked with Keccak. A passed answer
/// earns a share of the deal's epoch pay; a wrong or missed one slashes part of the bond to the
/// treasury and excludes the node from that shard, which moves the copy to the next node in the
/// ranking (repair).
///
/// Every parameter is immutable and there is no owner: no admin path moves funds (R-STOR-11).
/// Tokens leave only as earnings to an operator, a bond to its operator after exit, a deal's
/// remainder to its payer after the deal ends, or slashes to the immutable treasury.
/// Conservation: the token held equals deal deposits not yet paid or refunded, plus bonds held,
/// plus earnings not withdrawn, plus slashes not swept (`conserved()`).
contract OnyxStorage {
    uint256 internal constant ANVIL = 31_337;
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 public constant MAX_NODES = 128;
    uint8 public constant MAX_R = 8;
    uint64 public constant SETTLE_EPOCHS = 2;
    uint64 public constant SEED_WINDOW = 256;
    uint32 public constant MAX_DEAL_EPOCHS = 100_000;
    /// Most capacity one node may declare (1 EiB): sums stay far from overflow.
    uint64 public constant MAX_CAPACITY_GIB = 1 << 30;
    uint32 internal constant PPM = 1_000_000;
    /// `decline`'s index for every shard of a deal.
    uint32 public constant WHOLE_DEAL = type(uint32).max;
    uint64 internal constant GIB = 1 << 30;

    uint8 public constant ACTIVE = 1;
    uint8 public constant EXITING = 2;
    uint8 public constant JAILED = 3;
    uint8 public constant WITHDRAWN = 4;

    uint8 public constant PASSED = 1;
    uint8 public constant FAILED = 2;
    uint8 public constant MISSED = 3;

    error WrongChain(uint256 chainId);
    error BadParams();
    error Reentrancy();
    error TransferFailed();
    error BadAmount();
    error NotOperator();
    error NotRegistration();
    error ActivationWindow();
    error TooManyNodes();
    error NotActive();
    error TooEarly();
    error NotAnchored();
    error KeySuspended();
    error NotEnoughNodes();
    error OverCapacity();
    error NoDeal();
    error DealOver();
    error NoSeed();
    error SeedWindow();
    error SeedAlready();
    error WrongEpoch();
    error WrongNodeList();
    error NotChallenged();
    error AlreadyAnswered();
    error SettleWindow();
    error DealOpen();
    error Closed();
    error AlreadyExcluded();
    error BelowShare();

    event Registered(
        uint64 indexed index, address indexed operator, uint64 capacityGiB, uint256 bond, string url
    );
    event Activated(bytes32 indexed nodeId, uint64 indexed index, address indexed operator);
    event RegistrationCancelled(uint64 indexed index);
    event NodeSet(uint64 indexed fromEpoch, bytes32 setHash, bytes32[] nodes);
    event Removed(bytes32 indexed nodeId, uint64 fromEpoch, bool jailed);
    event BondWithdrawn(bytes32 indexed nodeId, address indexed operator, uint256 amount);
    event DealCreated(
        uint64 indexed dealId,
        address indexed payer,
        bytes32 manifestRoot,
        bytes32 shardsRoot,
        uint32 shardCount,
        uint64 byteCount,
        uint8 r,
        uint64 startEpoch,
        uint64 endEpoch,
        bytes32 salt,
        uint256 deposit,
        string publisher,
        string dataset,
        string sourceUrl
    );
    event DealExtended(uint64 indexed dealId, uint64 endEpoch, uint256 added);
    event DealClosed(uint64 indexed dealId, uint256 refund);
    event SeedRecorded(uint64 indexed epoch, bytes32 seed);
    event Answered(
        uint64 indexed dealId,
        uint64 indexed epoch,
        uint32 slot,
        bytes32 indexed nodeId,
        bool passed,
        uint256 paid
    );
    event Slashed(
        bytes32 indexed nodeId,
        uint64 indexed dealId,
        uint64 epoch,
        uint32 slot,
        uint32 shardIndex,
        uint256 amount,
        uint64 excludedFrom,
        uint8 cause
    );
    event EarningsWithdrawn(address indexed operator, uint256 amount);
    event TreasurySwept(address indexed treasury, uint256 amount);
    event Declined(bytes32 indexed nodeId, uint64 indexed dealId, uint32 index, uint64 fromEpoch);

    struct Registration {
        address operator;
        uint64 capacityGiB;
        uint64 block_;
        bool done;
        uint128 bond;
    }

    struct Node {
        address operator;
        uint8 status;
        uint32 misses;
        uint64 capacityGiB;
        uint64 removedFrom;
        uint64 jailedFrom;
        uint64 index;
        uint128 bond;
        /// Position in `_active` plus one (0: not in the list).
        uint256 pos;
    }

    struct Deal {
        address payer;
        uint8 r;
        uint32 shardCount;
        uint64 byteCount;
        uint64 gib;
        uint64 startEpoch;
        uint64 endEpoch;
        bool closed;
        bytes32 manifestRoot;
        bytes32 shardsRoot;
        bytes32 salt;
        uint128 deposit;
        uint128 paid;
    }

    IStorageToken public immutable token;
    OnyxSignedBook public immutable book;
    address public immutable treasury;
    uint256 public immutable genesis;
    uint64 public immutable epochBlocks;
    uint128 public immutable pricePerGiBEpoch;
    uint128 public immutable bondPerGiB;
    uint128 public immutable minBond;
    uint32 public immutable slashPpm;
    uint32 public immutable challengesPerDeal;
    uint32 public immutable maxMisses;

    Registration[] internal _regs;
    mapping(bytes32 => Node) internal _nodes;
    bytes32[] internal _active;
    uint64[] internal _cpFrom;
    bytes32[] internal _cpHash;
    uint64 public totalCapacityGiB;
    uint64 public committedGiB;

    Deal[] internal _deals;
    mapping(uint64 => bytes32) public seedOf;
    /// keccak256(dealId, shard index, node) -> first epoch the node no longer holds the shard.
    mapping(bytes32 => uint64) internal _exclFrom;
    /// keccak256(dealId, epoch, slot, node) -> PASSED / FAILED / MISSED.
    mapping(bytes32 => uint8) internal _slot;
    mapping(address => uint256) public earned;

    uint256 public deposits;
    uint256 public bondsIn;
    uint256 public paid;
    uint256 public slashed;
    uint256 public refunded;
    uint256 public bondsOut;
    uint256 public earningsOut;
    uint256 public swept;

    uint256 private _lock = 1;

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    constructor(
        IStorageToken token_,
        OnyxSignedBook book_,
        address treasury_,
        uint64 epochBlocks_,
        uint128 pricePerGiBEpoch_,
        uint128 bondPerGiB_,
        uint128 minBond_,
        uint32 slashPpm_,
        uint32 challengesPerDeal_,
        uint32 maxMisses_
    ) {
        if (block.chainid != ANVIL && block.chainid != BASE_SEPOLIA) {
            revert WrongChain(block.chainid);
        }
        if (
            address(token_) == address(0) || address(book_) == address(0) || treasury_ == address(0)
                || epochBlocks_ < 2 || pricePerGiBEpoch_ == 0 || bondPerGiB_ == 0 || minBond_ == 0
                || slashPpm_ == 0 || slashPpm_ > PPM || challengesPerDeal_ == 0
                || challengesPerDeal_ > 16 || maxMisses_ == 0
        ) revert BadParams();
        token = token_;
        book = book_;
        treasury = treasury_;
        genesis = block.number;
        epochBlocks = epochBlocks_;
        pricePerGiBEpoch = pricePerGiBEpoch_;
        bondPerGiB = bondPerGiB_;
        minBond = minBond_;
        slashPpm = slashPpm_;
        challengesPerDeal = challengesPerDeal_;
        maxMisses = maxMisses_;
    }

    // ------------------------------------------------------------------ time

    function currentEpoch() public view returns (uint64) {
        return uint64((block.number - genesis) / epochBlocks);
    }

    function epochStart(uint64 e) public view returns (uint256) {
        return genesis + uint256(e) * epochBlocks;
    }

    /// Record epoch `e`'s seed: `keccak256("onyx-storage-seed-v1" ‖ blockhash(start(e)) ‖ e)`,
    /// in the `SEED_WINDOW` blocks after its start block (anyone; R-STOR-5).
    function recordSeed(uint64 e) external returns (bytes32 s) {
        if (seedOf[e] != bytes32(0)) revert SeedAlready();
        uint256 st = epochStart(e);
        if (block.number <= st || block.number > st + SEED_WINDOW) revert SeedWindow();
        bytes32 bh = blockhash(st);
        if (bh == bytes32(0)) revert SeedWindow();
        s = L.seed(bh, e);
        seedOf[e] = s;
        emit SeedRecorded(e, s);
    }

    // ------------------------------------------------------------------ nodes

    /// The bond a node declaring `capacityGiB` posts.
    function bondFor(uint64 capacityGiB) public view returns (uint256) {
        uint256 b = uint256(bondPerGiB) * capacityGiB;
        return b < minBond ? minBond : b;
    }

    /// Step 1: post the bond and declare capacity. The node id is fixed by `activate` from this
    /// block's hash, unknown when the transaction was sent (THREATS STORAGE-3).
    function register(uint64 capacityGiB, string calldata url)
        external
        nonReentrant
        returns (uint64 index)
    {
        if (
            capacityGiB == 0 || capacityGiB > MAX_CAPACITY_GIB || bytes(url).length == 0
                || bytes(url).length > 256
        ) {
            revert BadParams();
        }
        uint256 bond = bondFor(capacityGiB);
        // Bonds are kept in 128 bits: never truncate what was paid in.
        if (bond > type(uint128).max) revert BadAmount();
        _pull(msg.sender, bond);
        bondsIn += bond;
        index = uint64(_regs.length);
        _regs.push(
            Registration({
                operator: msg.sender,
                capacityGiB: capacityGiB,
                block_: uint64(block.number),
                done: false,
                bond: uint128(bond)
            })
        );
        emit Registered(index, msg.sender, capacityGiB, bond, url);
    }

    /// Step 2, in a later block within `SEED_WINDOW`: the node joins the list from the next epoch.
    function activate(uint64 index) external returns (bytes32 id) {
        if (index >= _regs.length) revert NotRegistration();
        Registration storage g = _regs[index];
        if (g.operator != msg.sender) revert NotOperator();
        if (g.done) revert NotRegistration();
        if (block.number <= g.block_ || block.number > g.block_ + SEED_WINDOW) {
            revert ActivationWindow();
        }
        if (_active.length >= MAX_NODES) revert TooManyNodes();
        // Placement is even: a joining node takes about committed / (N + 1) GiB of copies, so it
        // must have declared (and bonded for) at least that (review STORAGE-R1-6). Checked here,
        // never in createDeal, so one small node cannot freeze new deals (STORAGE-R2-2).
        uint256 share = (uint256(committedGiB) + _active.length) / (_active.length + 1);
        if (g.capacityGiB < share) revert BelowShare();
        g.done = true;
        id = L.nodeId(msg.sender, index, blockhash(g.block_));
        Node storage n = _nodes[id];
        n.operator = msg.sender;
        n.status = ACTIVE;
        n.capacityGiB = g.capacityGiB;
        n.index = index;
        n.bond = g.bond;
        _active.push(id);
        n.pos = _active.length;
        totalCapacityGiB += g.capacityGiB;
        emit Activated(id, index, msg.sender);
        _checkpoint();
    }

    /// A registration not activated within its window returns its bond.
    function cancelRegistration(uint64 index) external nonReentrant {
        if (index >= _regs.length) revert NotRegistration();
        Registration storage g = _regs[index];
        if (g.operator != msg.sender) revert NotOperator();
        if (g.done) revert NotRegistration();
        if (block.number <= g.block_ + SEED_WINDOW) revert TooEarly();
        g.done = true;
        uint256 b = g.bond;
        g.bond = 0;
        bondsOut += b;
        _push(msg.sender, b);
        emit RegistrationCancelled(index);
    }

    /// Leave: out of the list from the next epoch, still challenged in that epoch (its shards'
    /// new holders fetch meanwhile); the bond returns after the settle window.
    function requestExit(bytes32 id) external {
        Node storage n = _nodes[id];
        if (n.operator != msg.sender) revert NotOperator();
        if (n.status != ACTIVE) revert NotActive();
        n.status = EXITING;
        _remove(id, n, false);
    }

    function withdrawBond(bytes32 id) external nonReentrant {
        Node storage n = _nodes[id];
        if (n.operator != msg.sender) revert NotOperator();
        if (n.status != EXITING && n.status != JAILED) revert NotActive();
        if (currentEpoch() <= n.removedFrom + SETTLE_EPOCHS) revert TooEarly();
        uint256 b = n.bond;
        n.bond = 0;
        n.status = WITHDRAWN;
        bondsOut += b;
        _push(n.operator, b);
        emit BondWithdrawn(id, n.operator, b);
    }

    function _remove(bytes32 id, Node storage n, bool jailed) internal {
        uint64 from = currentEpoch() + 1;
        if (n.pos != 0) {
            uint256 i = n.pos - 1;
            bytes32 last = _active[_active.length - 1];
            _active[i] = last;
            _nodes[last].pos = i + 1;
            _active.pop();
            n.pos = 0;
            n.removedFrom = from;
            totalCapacityGiB -= n.capacityGiB;
            _checkpoint();
        }
        if (jailed) {
            n.status = JAILED;
            n.jailedFrom = from;
        }
        emit Removed(id, n.removedFrom, jailed);
    }

    /// The list changes from the next epoch on: one checkpoint per epoch, the last change wins.
    function _checkpoint() internal {
        uint64 from = currentEpoch() + 1;
        bytes32 h = L.setHash(_active);
        uint256 k = _cpFrom.length;
        if (k > 0 && _cpFrom[k - 1] == from) {
            _cpHash[k - 1] = h;
        } else {
            _cpFrom.push(from);
            _cpHash.push(h);
        }
        emit NodeSet(from, h, _active);
    }

    /// The hash of the node list in force at epoch `e` (`keccak256("")` before any node).
    function setHashAt(uint64 e) public view returns (bytes32) {
        uint256 lo = 0;
        uint256 hi = _cpFrom.length;
        while (lo < hi) {
            uint256 mid = (lo + hi) / 2;
            if (_cpFrom[mid] <= e) lo = mid + 1;
            else hi = mid;
        }
        return lo == 0 ? keccak256("") : _cpHash[lo - 1];
    }

    function activeNodes() external view returns (bytes32[] memory) {
        return _active;
    }

    function nodeInfo(bytes32 id)
        external
        view
        returns (
            address operator,
            uint8 status,
            uint64 capacityGiB,
            uint256 bond,
            uint32 misses,
            uint64 removedFrom,
            uint64 jailedFrom,
            uint64 index
        )
    {
        Node storage n = _nodes[id];
        return (
            n.operator,
            n.status,
            n.capacityGiB,
            n.bond,
            n.misses,
            n.removedFrom,
            n.jailedFrom,
            n.index
        );
    }

    function registrationCount() external view returns (uint256) {
        return _regs.length;
    }

    // ------------------------------------------------------------------ deals

    /// Buy storage for an anchored manifest: `header` is its Borsh header, `payload` the Borsh
    /// signed payload the publisher posted to the book. The deal is challenged from two epochs
    /// on (the next epoch's node list is final when this one ends; holders fetch during it).
    function createDeal(
        bytes calldata header,
        bytes calldata payload,
        uint8 r,
        uint32 epochs,
        string calldata sourceUrl
    ) external nonReentrant returns (uint64 id) {
        if (r == 0 || r > MAX_R || epochs == 0 || epochs > MAX_DEAL_EPOCHS) {
            revert BadParams();
        }
        if (bytes(sourceUrl).length > 256) revert BadParams();
        L.Header memory h = L.readHeader(header);
        _checkAnchor(header, h, payload);
        if (_active.length < r) revert NotEnoughNodes();
        uint64 gib = L.billableGiB(h.byteCount, h.shardCount);
        if (uint256(committedGiB) + uint256(gib) * r > totalCapacityGiB) revert OverCapacity();
        committedGiB += gib * r;
        uint256 dep = uint256(pricePerGiBEpoch) * gib * r * epochs;
        if (dep > type(uint128).max) revert BadAmount();
        _pull(msg.sender, dep);
        deposits += dep;
        id = uint64(_deals.length);
        uint64 start = currentEpoch() + 2;
        bytes32 salt = L.dealSalt(blockhash(block.number - 1), id);
        _deals.push(
            Deal({
                payer: msg.sender,
                r: r,
                shardCount: h.shardCount,
                byteCount: h.byteCount,
                gib: gib,
                startEpoch: start,
                endEpoch: start + epochs,
                closed: false,
                manifestRoot: h.root,
                shardsRoot: h.shardsRoot,
                salt: salt,
                deposit: uint128(dep),
                paid: 0
            })
        );
        emit DealCreated(
            id,
            msg.sender,
            h.root,
            h.shardsRoot,
            h.shardCount,
            h.byteCount,
            r,
            start,
            start + epochs,
            salt,
            dep,
            string(header[h.pStart:h.pEnd]),
            string(header[h.dStart:h.dEnd]),
            sourceUrl
        );
    }

    /// The book recorded this payload, accepted its fact on exactly this report, and the key is
    /// registered and not suspended (R-STOR-3).
    function _checkAnchor(bytes calldata header, L.Header memory h, bytes calldata payload)
        internal
        view
    {
        bytes32 digest = L.checkPayload(header, h, payload);
        OnyxSignedBook.Rep memory rep = book.report(digest);
        if (rep.key == bytes32(0)) revert NotAnchored();
        (OnyxSignedBook.FactState st, bytes32 acc,,) = book.factView(rep.fact);
        if (st != OnyxSignedBook.FactState.Accepted || acc != rep.reportHash) revert NotAnchored();
        (OnyxSignedBook.Life life, bool suspended,,,,) = book.keyState(rep.key);
        if (life == OnyxSignedBook.Life.None) revert NotAnchored();
        if (suspended) revert KeySuspended();
    }

    /// Pay for more epochs of a running deal (anyone may pay; the remainder still returns to the
    /// deal's payer).
    function extendDeal(uint64 id, uint32 epochs) external nonReentrant {
        if (id >= _deals.length) revert NoDeal();
        Deal storage d = _deals[id];
        if (d.closed || currentEpoch() >= d.endEpoch) revert DealOver();
        if (epochs == 0 || uint256(d.endEpoch - d.startEpoch) + epochs > MAX_DEAL_EPOCHS) {
            revert BadParams();
        }
        uint256 add = uint256(pricePerGiBEpoch) * d.gib * d.r * epochs;
        // The deposit is kept in 128 bits: never truncate what was paid in.
        if (uint256(d.deposit) + add > type(uint128).max) revert BadAmount();
        _pull(msg.sender, add);
        deposits += add;
        d.deposit += uint128(add);
        d.endEpoch += epochs;
        emit DealExtended(id, d.endEpoch, add);
    }

    /// After a deal's last epoch: what was not paid out goes back to its payer (anyone may call).
    function closeDeal(uint64 id) external nonReentrant {
        if (id >= _deals.length) revert NoDeal();
        Deal storage d = _deals[id];
        if (d.closed) revert Closed();
        if (currentEpoch() < d.endEpoch) revert DealOpen();
        d.closed = true;
        committedGiB -= d.gib * d.r;
        uint256 back = uint256(d.deposit) - d.paid;
        refunded += back;
        _push(d.payer, back);
        emit DealClosed(id, back);
    }

    function dealCount() external view returns (uint256) {
        return _deals.length;
    }

    function dealInfo(uint64 id)
        external
        view
        returns (
            address payer,
            bytes32 manifestRoot,
            bytes32 shardsRoot,
            bytes32 salt,
            uint32 shardCount,
            uint8 r,
            uint64 byteCount,
            uint64 startEpoch,
            uint64 endEpoch,
            uint256 deposit,
            uint256 paidOut,
            bool closed
        )
    {
        if (id >= _deals.length) revert NoDeal();
        Deal storage d = _deals[id];
        return (
            d.payer,
            d.manifestRoot,
            d.shardsRoot,
            d.salt,
            d.shardCount,
            d.r,
            d.byteCount,
            d.startEpoch,
            d.endEpoch,
            d.deposit,
            d.paid,
            d.closed
        );
    }

    /// One passed answer's pay: the deal's epoch pay over every (slot, replica) pair, rounded
    /// down (the rest returns at close).
    function slotPay(uint64 id) public view returns (uint256) {
        Deal storage d = _deals[id];
        return uint256(pricePerGiBEpoch) * d.gib * d.r / (uint256(challengesPerDeal) * d.r);
    }

    // ------------------------------------------------------------------ challenges

    function _exKey(uint64 dealId, uint32 index, bytes32 node) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(dealId, index, node));
    }

    function _slotKey(uint64 dealId, uint64 e, uint32 slot, bytes32 node)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked(dealId, e, slot, node));
    }

    /// The first epoch `node` no longer holds shard `index` of the deal: the earlier of its
    /// exclusion from that shard (a slash or a decline) and its decline of the whole deal
    /// (`WHOLE_DEAL`); 0 when neither.
    function exclusionFrom(uint64 dealId, uint32 index, bytes32 node) public view returns (uint64) {
        uint64 a = _exclFrom[_exKey(dealId, index, node)];
        uint64 b = _exclFrom[_exKey(dealId, WHOLE_DEAL, node)];
        if (a == 0) return b;
        if (b == 0) return a;
        return a < b ? a : b;
    }

    /// A node gives up one shard of a deal (or the whole deal, `WHOLE_DEAL`) from the next epoch
    /// on, without a slash: for data it could not get (a publisher or replicas that withhold it,
    /// THREATS STORAGE-16) or no longer wants to keep. A challenge already drawn in this epoch
    /// still stands; the copy moves to the next node in the ranking.
    function decline(uint64 dealId, uint32 index, bytes32 node) external {
        if (_nodes[node].operator != msg.sender) revert NotOperator();
        if (dealId >= _deals.length) revert NoDeal();
        if (index != WHOLE_DEAL && index >= _deals[dealId].shardCount) revert BadParams();
        bytes32 k = _exKey(dealId, index, node);
        if (_exclFrom[k] != 0) revert AlreadyExcluded();
        uint64 from = currentEpoch() + 1;
        _exclFrom[k] = from;
        emit Declined(node, dealId, index, from);
    }

    function slotStatus(uint64 dealId, uint64 e, uint32 slot, bytes32 node)
        external
        view
        returns (uint8)
    {
        return _slot[_slotKey(dealId, e, slot, node)];
    }

    /// The nodes challenged for shard `index` of deal `dealId` in epoch `e` (R-STOR-6,
    /// `onyx_storage::placement::challenged`): the top R of `nodes` (the list of epoch e-1) by
    /// rendezvous weight, skipping nodes excluded from the shard by e-1; less nodes excluded
    /// from it or jailed by e.
    function _challenged(
        Deal storage d,
        uint64 dealId,
        uint64 e,
        uint32 index,
        bytes32[] calldata nodes
    ) internal view returns (bytes32[] memory out, uint256 count) {
        uint256 n = nodes.length;
        bytes32 key = L.shardKey(d.salt, index);
        uint256[] memory w = new uint256[](n);
        for (uint256 i = 0; i < n; i++) {
            w[i] = L.weight(key, nodes[i]);
        }
        out = new bytes32[](d.r);
        uint256 picked;
        uint256 found;
        while (found < d.r) {
            uint256 best = type(uint256).max;
            for (uint256 i = 0; i < n; i++) {
                if (picked & (1 << i) != 0) continue;
                if (
                    best == type(uint256).max || w[i] > w[best]
                        || (w[i] == w[best] && nodes[i] < nodes[best])
                ) best = i;
            }
            if (best == type(uint256).max) break;
            picked |= 1 << best;
            uint64 f = exclusionFrom(dealId, index, nodes[best]);
            if (f != 0 && f <= e - 1) continue;
            found++;
            if (f != 0 && f <= e) continue;
            Node storage nd = _nodes[nodes[best]];
            if (nd.jailedFrom != 0 && nd.jailedFrom <= e) continue;
            out[count++] = nodes[best];
        }
    }

    /// Who is challenged for slot `slot` of a deal in epoch `e`, given that epoch's previous node
    /// list (a view for nodes and tools).
    function challengedHolders(uint64 dealId, uint64 e, uint32 slot, bytes32[] calldata nodes)
        external
        view
        returns (uint32 index, bytes32[] memory holders)
    {
        if (dealId >= _deals.length) revert NoDeal();
        Deal storage d = _deals[dealId];
        bytes32 s = seedOf[e];
        if (s == bytes32(0)) revert NoSeed();
        if (e == 0 || L.setHash(nodes) != setHashAt(e - 1)) revert WrongNodeList();
        index = L.slotShard(s, dealId, slot, d.shardCount);
        (bytes32[] memory out, uint256 c) = _challenged(d, dealId, e, index, nodes);
        holders = new bytes32[](c);
        for (uint256 i = 0; i < c; i++) {
            holders[i] = out[i];
        }
    }

    /// The common checks of `answer` and `settleMissed`: a deal running in `e`, a seed, the slot
    /// in range, the node list of e-1, and `node` among the slot's challenged holders.
    function _ask(uint64 dealId, uint64 e, uint32 slot, bytes32 node, bytes32[] calldata nodes)
        internal
        view
        returns (Deal storage d, bytes32 s, uint32 index)
    {
        if (dealId >= _deals.length) revert NoDeal();
        d = _deals[dealId];
        if (e < d.startEpoch || e >= d.endEpoch) revert WrongEpoch();
        if (slot >= challengesPerDeal) revert NotChallenged();
        s = seedOf[e];
        if (s == bytes32(0)) revert NoSeed();
        if (keccak256(abi.encodePacked(nodes)) != setHashAt(e - 1)) revert WrongNodeList();
        index = L.slotShard(s, dealId, slot, d.shardCount);
        (bytes32[] memory out, uint256 c) = _challenged(d, dealId, e, index, nodes);
        for (uint256 i = 0; i < c; i++) {
            if (out[i] == node) return (d, s, index);
        }
        revert NotChallenged();
    }

    /// Answer a challenge in its epoch (the node's operator only). A proof that holds is paid
    /// the slot share; one that does not is recorded as failed and slashed (R-STOR-7, R-STOR-9).
    function answer(
        uint64 dealId,
        uint64 e,
        uint32 slot,
        bytes32 node,
        bytes32[] calldata nodes,
        bytes calldata entry,
        bytes32[] calldata entrySiblings,
        bytes calldata row,
        bytes32[] calldata rowSiblings
    ) external returns (bool passed) {
        if (_nodes[node].operator != msg.sender) {
            revert NotOperator();
        }
        if (e != currentEpoch()) revert WrongEpoch();
        (Deal storage d, bytes32 s, uint32 index) = _ask(dealId, e, slot, node, nodes);
        bytes32 k = _slotKey(dealId, e, slot, node);
        if (_slot[k] != 0) revert AlreadyAnswered();
        passed = L.verifyProof(
            d.shardsRoot, d.shardCount, s, dealId, slot, entry, entrySiblings, row, rowSiblings
        );
        if (passed) {
            _slot[k] = PASSED;
            uint256 pay = slotPay(dealId);
            if (uint256(d.paid) + pay > d.deposit) pay = uint256(d.deposit) - d.paid;
            d.paid += uint128(pay);
            paid += pay;
            Node storage n = _nodes[node];
            n.misses = 0;
            earned[n.operator] += pay;
            emit Answered(dealId, e, slot, node, true, pay);
        } else {
            _slot[k] = FAILED;
            emit Answered(dealId, e, slot, node, false, 0);
            _slash(node, dealId, e, slot, index, FAILED);
        }
    }

    /// After epoch `e` ends and within `SETTLE_EPOCHS`: a challenge nobody answered is a miss,
    /// slashed like a wrong answer (anyone may call; R-STOR-9).
    function settleMissed(
        uint64 dealId,
        uint64 e,
        uint32 slot,
        bytes32 node,
        bytes32[] calldata nodes
    ) external {
        uint64 cur = currentEpoch();
        if (cur <= e || cur > e + SETTLE_EPOCHS) revert SettleWindow();
        (,, uint32 index) = _ask(dealId, e, slot, node, nodes);
        bytes32 k = _slotKey(dealId, e, slot, node);
        if (_slot[k] != 0) revert AlreadyAnswered();
        _slot[k] = MISSED;
        _slash(node, dealId, e, slot, index, MISSED);
    }

    /// Take `slashPpm` of the bond for the treasury, exclude the node from the shard from the
    /// next epoch (its copy moves to the next node in the ranking), and jail the node after
    /// `maxMisses` in a row or when its bond falls below half of `minBond`.
    function _slash(bytes32 id, uint64 dealId, uint64 e, uint32 slot, uint32 index, uint8 cause)
        internal
    {
        Node storage n = _nodes[id];
        uint256 amt = uint256(n.bond) * slashPpm / PPM;
        n.bond -= uint128(amt);
        slashed += amt;
        n.misses += 1;
        bytes32 xk = _exKey(dealId, index, id);
        if (_exclFrom[xk] == 0) _exclFrom[xk] = currentEpoch() + 1;
        emit Slashed(id, dealId, e, slot, index, amt, exclusionFrom(dealId, index, id), cause);
        if (
            (n.status == ACTIVE || n.status == EXITING)
                && (n.misses >= maxMisses || n.bond < minBond / 2)
        ) {
            _remove(id, n, true);
        }
    }

    // ------------------------------------------------------------------ money out

    function withdrawEarnings() external nonReentrant {
        uint256 a = earned[msg.sender];
        if (a == 0) revert BadAmount();
        earned[msg.sender] = 0;
        earningsOut += a;
        _push(msg.sender, a);
        emit EarningsWithdrawn(msg.sender, a);
    }

    /// Send the slashed tokens to the immutable treasury (anyone may call).
    function sweepTreasury() external nonReentrant {
        uint256 a = slashed - swept;
        if (a == 0) revert BadAmount();
        swept += a;
        _push(treasury, a);
        emit TreasurySwept(treasury, a);
    }

    /// Every flow in and out (R-STOR-11).
    function books()
        external
        view
        returns (
            uint256 deposits_,
            uint256 bondsIn_,
            uint256 paid_,
            uint256 slashed_,
            uint256 refunded_,
            uint256 bondsOut_,
            uint256 earningsOut_,
            uint256 swept_
        )
    {
        return (deposits, bondsIn, paid, slashed, refunded, bondsOut, earningsOut, swept);
    }

    /// Conservation (R-STOR-11): every token held is accounted for, and no flow out exceeds its
    /// flow in.
    function conserved() external view returns (bool) {
        if (paid + refunded > deposits || slashed + bondsOut > bondsIn) return false;
        if (earningsOut > paid || swept > slashed) return false;
        uint256 held = (deposits - paid - refunded) + (bondsIn - slashed - bondsOut)
            + (paid - earningsOut) + (slashed - swept);
        return token.balanceOf(address(this)) == held;
    }

    function params()
        external
        view
        returns (
            uint64 epochBlocks_,
            uint128 pricePerGiBEpoch_,
            uint128 bondPerGiB_,
            uint128 minBond_,
            uint32 slashPpm_,
            uint32 challengesPerDeal_,
            uint32 maxMisses_,
            uint256 genesis_
        )
    {
        return (
            epochBlocks,
            pricePerGiBEpoch,
            bondPerGiB,
            minBond,
            slashPpm,
            challengesPerDeal,
            maxMisses,
            genesis
        );
    }

    function _pull(address from, uint256 amount) internal {
        uint256 before = token.balanceOf(address(this));
        if (!token.transferFrom(from, address(this), amount)) revert TransferFailed();
        if (token.balanceOf(address(this)) - before != amount) revert TransferFailed();
    }

    function _push(address to, uint256 amount) internal {
        if (amount == 0) return;
        if (!token.transfer(to, amount)) revert TransferFailed();
    }
}
