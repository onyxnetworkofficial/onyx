// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IStakeToken, IOnyxFallbackOracle, IOnyxValidatorSet} from "./IOnyxMarkets.sol";
import {OnyxStakes} from "./OnyxStakes.sol";

/// @title OnyxPanelOracle
/// @notice TESTNET ONLY. The drawn-panel fallback oracle (ROADMAP S3b, ADR-0042 amendment 1). It
/// replaces the one owner key of `OnyxTestnetOwnerOracle`: a contested or low-quorum claim is
/// answered by a panel drawn from bonded seat holders, who commit and then reveal one answer
/// each (1 correct, 2 wrong, 3 void). `majority` matching reveals decide; anything else is void.
///
/// - Seats: anyone bonds exactly `seatBond` tokens for one seat per address (at most
///   `MAX_SEATS`). A seat on an open panel cannot leave. With a `gate` (the settlement's staked
///   validator set, THREATS LC16-2), only an active, staked validator may join, and a seat whose
///   validator left the set (unbonding, evicted) is never drawn: one funded actor cannot fill the
///   panel with fresh addresses, since each seat costs a validator bond in a token no single key
///   mints and the set is capped (it must outbid the members it evicts).
/// - Draw: from `blockhash(anchor)`, the block of the request, as `OnyxDisputes` (ADR-0021): the
///   `panelSize` eligible seats with the smallest `panelKey`. Eligible: seats that joined before
///   the anchor block (so nobody picks an address after seeing the beacon), never the claim's
///   submitter or anyone with a stake on the claim (`OnyxStakes.stakes`). A beacon older than
///   256 blocks, or too few eligible seats, answers void: a lapsed draw is never re-rolled.
/// - Votes: commit before `commitClose`, reveal before `revealClose`. A drawn seat that does not
///   reveal loses `noShowPpm` of its bond to the treasury and loses its seat; the rest is
///   refundable. A minority vote is not slashed (no herding pressure).
/// - The stake contract's `fallbackWindow` must exceed `commitWindow + revealWindow` plus the
///   draw time; otherwise the claim times out to void, which refunds everyone.
/// A panel decision is a level-4 decision: `OnyxFutures` never settles on it (ADR-0042 item 12).
contract OnyxPanelOracle is IOnyxFallbackOracle {
    uint256 internal constant ANVIL = 31_337;
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant PPM = 1_000_000;
    /// `blockhash` covers only the 256 most recent blocks.
    uint256 public constant DRAW_WINDOW = 256;
    /// Most seats: the draw reads every seat once.
    uint256 public constant MAX_SEATS = 256;
    uint8 public constant VOID = 3;
    bytes32 public constant VOTE_TAG = keccak256("onyx-panel-vote-v1");

    error WrongChain(uint256 chainId);
    error BadParams();
    error NotOwner();
    error NotStakes();
    error AlreadyBound();
    error WrongPhase();
    error NotAsked();
    error Seated();
    error NoSeat();
    error Busy();
    error Full();
    error NotValidator();
    error NotDrawn();
    error AlreadyVoted();
    error BadReveal();
    error BadAnswer();
    error NothingOwed();
    error TransferFailed();
    error Reentrancy();

    event Joined(address indexed who);
    event Left(address indexed who);
    event Asked(uint256 indexed claimId, uint64 anchor);
    event PanelDrawn(
        uint256 indexed claimId, address[] members, uint64 commitClose, uint64 revealClose
    );
    event VoteCommitted(uint256 indexed claimId, address indexed member);
    event VoteRevealed(uint256 indexed claimId, address indexed member, uint8 answer);
    event NoShow(uint256 indexed claimId, address indexed member, uint256 slashed);
    event Answer(uint256 indexed claimId, uint8 answer);

    struct Case {
        bool asked;
        uint64 anchor;
        uint64 commitClose;
        uint64 revealClose;
        uint8 answer;
        address[] members;
    }

    struct Vote {
        bool member;
        bytes32 commit;
        uint8 answer;
    }

    IStakeToken public immutable token;
    address public immutable owner;
    address public immutable treasury;
    uint128 public immutable seatBond;
    uint32 public immutable panelSize;
    uint32 public immutable majority;
    uint32 public immutable noShowPpm;
    uint64 public immutable commitWindow;
    uint64 public immutable revealWindow;
    OnyxStakes public stakes;
    /// The validator set seats must belong to; zero for open seats (local tests only).
    IOnyxValidatorSet public immutable gate;

    address[] internal _seats;
    mapping(address => uint256) internal _seatIndex; // index + 1; 0 = no seat
    /// The block a seat joined: eligible only for claims anchored after it.
    mapping(address => uint64) public joinedAt;
    /// Open panels a seat sits on; it cannot leave while this is not zero.
    mapping(address => uint32) public busy;
    /// Bonds refundable after leaving or a no-show.
    mapping(address => uint256) public refundable;
    uint256 public treasuryOwed;
    mapping(uint256 => Case) internal _cases;
    mapping(uint256 => mapping(address => Vote)) public votes;
    uint256 private _lock = 1;

    constructor(
        IStakeToken token_,
        address treasury_,
        uint128 seatBond_,
        uint32 panelSize_,
        uint32 majority_,
        uint32 noShowPpm_,
        uint64 commitWindow_,
        uint64 revealWindow_,
        IOnyxValidatorSet gate_
    ) {
        if (block.chainid != ANVIL && block.chainid != BASE_SEPOLIA) {
            revert WrongChain(block.chainid);
        }
        // A strict majority of an odd-or-even panel, so two answers can never both win.
        if (
            address(token_) == address(0) || treasury_ == address(0) || seatBond_ == 0
                || panelSize_ == 0 || panelSize_ > 31 || majority_ * 2 <= panelSize_
                || majority_ > panelSize_ || noShowPpm_ > PPM || commitWindow_ < 60
                || revealWindow_ < 60 || commitWindow_ > 30 days || revealWindow_ > 30 days
        ) revert BadParams();
        token = token_;
        owner = msg.sender;
        treasury = treasury_;
        seatBond = seatBond_;
        panelSize = panelSize_;
        majority = majority_;
        noShowPpm = noShowPpm_;
        commitWindow = commitWindow_;
        revealWindow = revealWindow_;
        gate = gate_;
    }

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    function bind(address stakes_) external {
        if (msg.sender != owner) revert NotOwner();
        if (address(stakes) != address(0)) revert AlreadyBound();
        stakes = OnyxStakes(stakes_);
    }

    /// @inheritdoc IOnyxFallbackOracle
    /// @dev This contract itself: no single key answers. Panel members are excluded per claim
    /// at the draw instead (they may stake on claims they are not drawn for).
    function arbiter() external view returns (address) {
        return address(this);
    }

    // ------------------------------------------------------------------ seats

    /// @notice Bond `seatBond` for one seat.
    function join() external nonReentrant {
        if (_seatIndex[msg.sender] != 0) revert Seated();
        if (_seats.length >= MAX_SEATS) revert Full();
        if (!isValidator(msg.sender)) revert NotValidator();
        _seats.push(msg.sender);
        _seatIndex[msg.sender] = _seats.length;
        joinedAt[msg.sender] = uint64(block.number);
        emit Joined(msg.sender);
        if (!token.transferFrom(msg.sender, address(this), seatBond)) revert TransferFailed();
    }

    /// @notice Give up a seat that sits on no open panel; the bond becomes refundable.
    function leave() external {
        if (_seatIndex[msg.sender] == 0) revert NoSeat();
        if (busy[msg.sender] != 0) revert Busy();
        _drop(msg.sender);
        refundable[msg.sender] += seatBond;
        emit Left(msg.sender);
    }

    function withdraw() external nonReentrant {
        uint256 a = refundable[msg.sender];
        if (a == 0) revert NothingOwed();
        refundable[msg.sender] = 0;
        if (!token.transfer(msg.sender, a)) revert TransferFailed();
    }

    function payTreasury() external nonReentrant {
        uint256 a = treasuryOwed;
        if (a == 0) revert NothingOwed();
        treasuryOwed = 0;
        if (!token.transfer(treasury, a)) revert TransferFailed();
    }

    function seats() external view returns (address[] memory) {
        return _seats;
    }

    function isSeated(address who) public view returns (bool) {
        return _seatIndex[who] != 0;
    }

    /// @notice Whether `who` may hold or be drawn for a seat: an active, staked member of the
    /// gate's validator set (any address without a gate).
    function isValidator(address who) public view returns (bool) {
        if (address(gate) == address(0)) return true;
        (uint128 stake,, uint64 unbondAt, bool active) = gate.validators(who);
        return active && stake != 0 && unbondAt == 0;
    }

    function _drop(address who) internal {
        uint256 i = _seatIndex[who] - 1;
        address last = _seats[_seats.length - 1];
        _seats[i] = last;
        _seatIndex[last] = i + 1;
        _seats.pop();
        _seatIndex[who] = 0;
    }

    // ------------------------------------------------------------------ requests and draws

    /// @inheritdoc IOnyxFallbackOracle
    function request(uint256 claimId) external {
        if (msg.sender != address(stakes)) revert NotStakes();
        Case storage c = _cases[claimId];
        if (c.asked) revert WrongPhase();
        c.asked = true;
        c.anchor = uint64(block.number);
        emit Asked(claimId, c.anchor);
    }

    /// Sort key of a seat for a claim's draw (smallest keys are drawn).
    function panelKey(uint256 claimId, bytes32 beacon, address seat) public pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-fallback-panel-v1", claimId, beacon, seat));
    }

    /// @notice Draw the claim's panel (anyone, from the block after the request, within
    /// `DRAW_WINDOW` blocks). A lapsed beacon or too few eligible seats answer void.
    function draw(uint256 claimId) external {
        Case storage c = _cases[claimId];
        if (!c.asked) revert NotAsked();
        if (c.answer != 0 || c.members.length != 0 || block.number <= c.anchor) {
            revert WrongPhase();
        }
        address[] memory chosen;
        if (block.number - c.anchor <= DRAW_WINDOW) {
            chosen = _choose(claimId, c.anchor, blockhash(c.anchor));
        }
        if (chosen.length < panelSize) {
            c.answer = VOID;
            emit Answer(claimId, VOID);
            return;
        }
        for (uint256 i = 0; i < chosen.length; i++) {
            c.members.push(chosen[i]);
            votes[claimId][chosen[i]].member = true;
            busy[chosen[i]] += 1;
        }
        c.commitClose = uint64(block.timestamp) + commitWindow;
        c.revealClose = c.commitClose + revealWindow;
        emit PanelDrawn(claimId, chosen, c.commitClose, c.revealClose);
    }

    /// The `panelSize` eligible seats with the smallest keys, or fewer when not enough are
    /// eligible. Eligible: seated before `anchor`, not the claim's submitter, no stake on it.
    function _choose(uint256 claimId, uint64 anchor, bytes32 beacon)
        internal
        view
        returns (address[] memory)
    {
        address submitter = stakes.claim(claimId).submitter;
        uint256 k = panelSize;
        address[] memory best = new address[](k);
        bytes32[] memory keys = new bytes32[](k);
        uint256 n;
        for (uint256 i = 0; i < _seats.length; i++) {
            address s = _seats[i];
            if (s == submitter || joinedAt[s] >= anchor || !isValidator(s)) continue;
            (uint128 staked,,,) = stakes.stakes(claimId, s);
            if (staked != 0) continue;
            bytes32 key = panelKey(claimId, beacon, s);
            if (n == k && key >= keys[k - 1]) continue;
            // Insert in key order (k is at most 31).
            uint256 j = n < k ? n++ : k - 1;
            while (j > 0 && keys[j - 1] > key) {
                keys[j] = keys[j - 1];
                best[j] = best[j - 1];
                j--;
            }
            keys[j] = key;
            best[j] = s;
        }
        assembly {
            mstore(best, n)
        }
        return best;
    }

    // ------------------------------------------------------------------ votes

    function voteHash(uint256 claimId, address member, uint8 answer_, bytes32 salt)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(VOTE_TAG, claimId, member, answer_, salt));
    }

    function commitVote(uint256 claimId, bytes32 commit) external {
        Case storage c = _cases[claimId];
        Vote storage v = votes[claimId][msg.sender];
        if (!v.member) revert NotDrawn();
        if (c.answer != 0 || block.timestamp >= c.commitClose) revert WrongPhase();
        if (v.commit != bytes32(0) || commit == bytes32(0)) revert AlreadyVoted();
        v.commit = commit;
        emit VoteCommitted(claimId, msg.sender);
    }

    function revealVote(uint256 claimId, uint8 answer_, bytes32 salt) external {
        Case storage c = _cases[claimId];
        Vote storage v = votes[claimId][msg.sender];
        if (!v.member) revert NotDrawn();
        if (c.answer != 0 || block.timestamp < c.commitClose || block.timestamp >= c.revealClose) {
            revert WrongPhase();
        }
        if (answer_ == 0 || answer_ > VOID) revert BadAnswer();
        if (v.answer != 0 || v.commit == bytes32(0)) revert BadReveal();
        if (voteHash(claimId, msg.sender, answer_, salt) != v.commit) revert BadReveal();
        v.answer = answer_;
        emit VoteRevealed(claimId, msg.sender, answer_);
    }

    /// @notice After the reveal window (anyone): the answer `majority` reveals agree on, else
    /// void; no-shows are slashed and lose their seats; every member is released.
    function finalize(uint256 claimId) external {
        Case storage c = _cases[claimId];
        if (c.members.length == 0 || c.answer != 0 || block.timestamp < c.revealClose) {
            revert WrongPhase();
        }
        uint256[4] memory tally;
        uint256 cut = uint256(seatBond) * noShowPpm / PPM;
        for (uint256 i = 0; i < c.members.length; i++) {
            address m = c.members[i];
            busy[m] -= 1;
            uint8 a = votes[claimId][m].answer;
            if (a != 0) {
                tally[a] += 1;
            } else if (_seatIndex[m] != 0) {
                // Seat lost; the bond less the cut is refundable.
                _drop(m);
                refundable[m] += seatBond - cut;
                treasuryOwed += cut;
                emit NoShow(claimId, m, cut);
            }
        }
        uint8 answer_ = VOID;
        for (uint8 a = 1; a <= VOID; a++) {
            if (tally[a] >= majority) answer_ = a;
        }
        c.answer = answer_;
        emit Answer(claimId, answer_);
    }

    /// @inheritdoc IOnyxFallbackOracle
    function answerOf(uint256 claimId) external view returns (uint8) {
        return _cases[claimId].answer;
    }

    function panel(uint256 claimId)
        external
        view
        returns (address[] memory members, uint64 anchor, uint64 commitClose, uint64 revealClose)
    {
        Case storage c = _cases[claimId];
        return (c.members, c.anchor, c.commitClose, c.revealClose);
    }
}
