// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IOnyxReobserve} from "./IOnyxMarkets.sol";

/// @title OnyxReobserve
/// @notice Re-observation of an accepted auto-pull submission (ROADMAP S4, ADR-0042 item 5): a
/// minimal faithful on-chain port of the level-2 round rules of ADR-0027 for exact values.
/// - Every observer registered when the task opens is assigned (at most `MAX_OBSERVERS`).
/// - Commit-reveal with the observer's address inside the commit (ADR-0027 item 3): a reveal
///   cannot be copied from another observer's commit.
/// - An explicit `UNAVAILABLE` answer (item 5).
/// - The outcome needs a strict majority of ALL assigned observers, not of reveals (item 3):
///   silence never lets a minority decide. Fewer than `MIN_OBSERVERS` assigned: no result.
/// Not ported (TODO S4b, THREATS STK-13): random draws from a larger pool, observer bonds and the
/// outlier slash (item 6), numeric tolerance bands (item 4), sealed reveals (amendment 10).
/// Observers are registered by the owner (one testnet key). A change takes effect only after
/// `changeDelay` (THREATS LC16-1): an observer added now is assigned only to tasks opened after
/// the delay, and one removed now stays assigned to tasks opened before it, so the owner cannot
/// pick the set for a task it already sees coming. Set the delay above a task's commit and reveal
/// windows. A registered observer, joining or leaving, counts as an insider (`isObserver`) from the
/// moment it is added until it is pruned. A bonded observer registry stays NORTH-STAR NS4.
contract OnyxReobserve is IOnyxReobserve {
    uint8 public constant VALUE = 1;
    uint8 public constant UNAVAILABLE = 2;
    /// Outcome statuses.
    uint8 public constant SETTLED = 1;
    uint8 public constant SOURCE_UNAVAILABLE = 2;
    uint8 public constant NO_RESULT = 3;
    uint256 public constant MAX_OBSERVERS = 16;
    uint256 public constant MIN_OBSERVERS = 3;
    bytes32 public constant COMMIT_TAG = keccak256("onyx-reobserve-commit-v1");

    struct Task {
        bytes32 fact;
        uint64 commitClose;
        uint64 revealClose;
        address[] assigned;
    }

    struct Answer {
        bytes32 commit;
        uint8 status;
        int256 value;
    }

    error NotOwner();
    error NotStakes();
    error AlreadyBound();
    error TooMany();
    error Known();
    error Unknown();
    error WrongPhase();
    error NotAssigned();
    error BadReveal();

    event ObserverAdded(address indexed observer);
    event ObserverRemoved(address indexed observer);
    event ObserverLeaving(address indexed observer, uint64 at);
    event TaskOpened(uint256 indexed taskId, bytes32 indexed fact, address[] assigned);
    event Committed(uint256 indexed taskId, address indexed observer);
    event Revealed(uint256 indexed taskId, address indexed observer, uint8 status, int256 value);

    address public immutable owner;
    /// Seconds before an added observer is assigned, or a removed one stops being assigned.
    uint64 public immutable changeDelay;
    address public stakes;
    address[] internal _observers;
    mapping(address => bool) public isObserver;
    /// When an observer is first assigned, and when it stops being assigned (0: not leaving).
    mapping(address => uint64) public activeFrom;
    mapping(address => uint64) public leavingAt;
    mapping(uint256 => Task) internal _tasks;
    mapping(uint256 => mapping(address => Answer)) public answers;

    constructor(uint64 changeDelay_) {
        owner = msg.sender;
        changeDelay = changeDelay_;
    }

    /// @notice Bind the stake contract that opens tasks, once, by the owner.
    function bind(address stakes_) external {
        if (msg.sender != owner) revert NotOwner();
        if (stakes != address(0)) revert AlreadyBound();
        stakes = stakes_;
    }

    function addObserver(address o) external {
        if (msg.sender != owner) revert NotOwner();
        if (isObserver[o]) revert Known();
        if (_observers.length >= MAX_OBSERVERS) revert TooMany();
        isObserver[o] = true;
        activeFrom[o] = uint64(block.timestamp) + changeDelay;
        _observers.push(o);
        emit ObserverAdded(o);
    }

    /// @notice Start removing `o`: it stays assigned to tasks opened before `changeDelay` has
    /// passed, then anyone may `prune` it.
    function removeObserver(address o) external {
        if (msg.sender != owner) revert NotOwner();
        if (!isObserver[o]) revert Unknown();
        if (leavingAt[o] != 0) revert Known();
        uint64 at = uint64(block.timestamp) + changeDelay;
        leavingAt[o] = at;
        emit ObserverLeaving(o, at);
        if (changeDelay == 0) _prune(o);
    }

    /// @notice Drop an observer whose removal delay has passed (anyone).
    function prune(address o) external {
        uint64 at = leavingAt[o];
        if (!isObserver[o] || at == 0 || block.timestamp < at) revert WrongPhase();
        _prune(o);
    }

    function _prune(address o) internal {
        isObserver[o] = false;
        (activeFrom[o], leavingAt[o]) = (0, 0);
        uint256 n = _observers.length;
        for (uint256 i = 0; i < n; i++) {
            if (_observers[i] == o) {
                _observers[i] = _observers[n - 1];
                _observers.pop();
                break;
            }
        }
        emit ObserverRemoved(o);
    }

    function observers() external view returns (address[] memory) {
        return _observers;
    }

    /// @notice Whether a task opened now assigns `o`: its join delay has passed and it is not
    /// past its removal.
    function isAssignable(address o) public view returns (bool) {
        if (!isObserver[o] || block.timestamp < activeFrom[o]) return false;
        uint64 at = leavingAt[o];
        return at == 0 || block.timestamp < at;
    }

    /// @inheritdoc IOnyxReobserve
    function open(uint256 taskId, bytes32 fact, uint64 commitClose, uint64 revealClose) external {
        if (msg.sender != stakes) revert NotStakes();
        Task storage t = _tasks[taskId];
        if (t.commitClose != 0) revert Known();
        (t.fact, t.commitClose, t.revealClose) = (fact, commitClose, revealClose);
        uint256 n = _observers.length;
        for (uint256 i = 0; i < n; i++) {
            address o = _observers[i];
            if (isAssignable(o)) t.assigned.push(o);
        }
        emit TaskOpened(taskId, fact, t.assigned);
    }

    function task(uint256 taskId) external view returns (Task memory) {
        return _tasks[taskId];
    }

    function commitHash(uint256 taskId, address observer, uint8 status, int256 value, bytes32 salt)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(COMMIT_TAG, taskId, observer, status, value, salt));
    }

    function _assigned(Task storage t, address o) internal view returns (bool) {
        uint256 n = t.assigned.length;
        for (uint256 i = 0; i < n; i++) {
            if (t.assigned[i] == o) return true;
        }
        return false;
    }

    /// @notice Commit an answer before the task's commit window closes.
    function commit(uint256 taskId, bytes32 h) external {
        Task storage t = _tasks[taskId];
        if (t.commitClose == 0 || block.timestamp >= t.commitClose) revert WrongPhase();
        if (!_assigned(t, msg.sender)) revert NotAssigned();
        Answer storage a = answers[taskId][msg.sender];
        if (a.commit != bytes32(0) || h == bytes32(0)) revert Known();
        a.commit = h;
        emit Committed(taskId, msg.sender);
    }

    /// @notice Reveal the committed answer during the reveal window.
    function reveal(uint256 taskId, uint8 status, int256 value, bytes32 salt) external {
        Task storage t = _tasks[taskId];
        if (block.timestamp < t.commitClose || block.timestamp >= t.revealClose) {
            revert WrongPhase();
        }
        Answer storage a = answers[taskId][msg.sender];
        if (a.commit == bytes32(0) || a.status != 0) revert BadReveal();
        if (status != VALUE && status != UNAVAILABLE) revert BadReveal();
        if (status == UNAVAILABLE && value != 0) revert BadReveal();
        if (commitHash(taskId, msg.sender, status, value, salt) != a.commit) revert BadReveal();
        (a.status, a.value) = (status, value);
        emit Revealed(taskId, msg.sender, status, value);
    }

    /// @inheritdoc IOnyxReobserve
    function outcome(uint256 taskId) external view returns (uint8 status, int256 value) {
        Task storage t = _tasks[taskId];
        if (t.revealClose == 0 || block.timestamp < t.revealClose) return (0, 0);
        uint256 n = t.assigned.length;
        if (n < MIN_OBSERVERS) return (NO_RESULT, 0);
        uint256 need = n / 2 + 1;
        uint256 unavailable;
        for (uint256 i = 0; i < n; i++) {
            Answer storage a = answers[taskId][t.assigned[i]];
            if (a.status == UNAVAILABLE) {
                unavailable++;
            } else if (a.status == VALUE) {
                uint256 same;
                for (uint256 j = 0; j < n; j++) {
                    Answer storage b = answers[taskId][t.assigned[j]];
                    if (b.status == VALUE && b.value == a.value) same++;
                }
                if (same >= need) return (SETTLED, a.value);
            }
        }
        if (unavailable >= need) return (SOURCE_UNAVAILABLE, 0);
        return (NO_RESULT, 0);
    }
}
