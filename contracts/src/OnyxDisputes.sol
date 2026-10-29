// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxRound} from "./OnyxRound.sol";
import {OnyxLedger} from "./OnyxLedger.sol";
import {OnyxSettlementExt} from "./OnyxSettlementExt.sol";

/// @title OnyxDisputes
/// @notice The on-chain dispute game of one Onyx ledger (ADR-0024; rules from ADR-0021). A
/// **rerun case** gives a `Pending` shard a reference instead of a free void (M6-F10); a
/// **challenge** can overturn a `Finalized` root inside its challenge window. Both run the same
/// game: a panel of bonded validators drawn from the round reruns the shard off-chain and
/// commit-reveals the root; a panel that is not unanimous can be appealed to a `2k + 1` panel; a
/// strict majority of the last panel decides; otherwise the coordinator posts the rerun it ran
/// (q13 (a)), or the case expires `Unresolved`. The decided root is then judged exactly as Rust
/// `Validation::settle` does (`settleCase`). Every token stays in `OnyxSettlement`'s escrow;
/// this contract moves it only through the settlement's `d*` hooks. Testnet only.
contract OnyxDisputes {
    // ------------------------------------------------------------------ types

    enum Kind {
        Rerun,
        Challenge
    }

    enum State {
        None,
        Open,
        RerunRequired,
        Decided,
        Done,
        Unresolved
    }

    struct Level {
        /// Who pays this panel's fees (zero: the round budget, a rerun case's first panel).
        address funder;
        uint128 bond;
        /// The root an appellant backs (unused on level 0).
        bytes32 backed;
        /// The beacon block: the panel is drawn from `blockhash(anchor)`.
        uint64 anchor;
        /// Block the panel was drawn (0 = not yet).
        uint64 drawnAt;
        uint32 size;
        address[] members;
        /// The root with the most reveals so far and its count (`revealVote`), so `resolve`
        /// reads a strict majority in O(1) (M6-L2-F2).
        bytes32 lead;
        uint32 leadCount;
    }

    struct Case {
        uint64 round;
        bytes32 shard;
        Kind kind;
        State state;
        bool claimRevealed;
        address challenger;
        /// `claimCommit(...)` of the challenger's root; revealed with `revealClaim`.
        bytes32 claim;
        bytes32 claimedRoot;
        /// The root under challenge (zero for a rerun case).
        bytes32 oldRoot;
        /// The decided root R.
        bytes32 decided;
        uint64 rerunDeadline;
        /// Round members judged so far (`settleCase`).
        uint32 cursor;
        /// Block the case was decided (`_decide`): starts the claim grace (M6-L3-F2).
        uint64 decidedAt;
        /// A rerun case's lost miner bonds not yet spent on extra verdict fees (set by the first
        /// `settleCase` batch); `_finish` burns what is left (ADR-0022 Amendment 1).
        uint256 lostLeft;
        Level[] levels;
    }

    struct Vote {
        bool member;
        bool revealed;
        bytes32 commit;
        bytes32 root;
    }

    // ------------------------------------------------------------------ errors and events

    error WrongPhase();
    error NotMember();
    error NotCoordinator();
    error BadVote();
    error BadClaim();
    error BondTooLow();

    event CaseOpened(
        bytes32 indexed caseId, uint64 indexed round, bytes32 indexed shard, Kind kind
    );
    event PanelDrawn(bytes32 indexed caseId, uint32 level, address[] members);
    event VoteRevealed(bytes32 indexed caseId, uint32 level, address indexed member, bytes32 root);
    event Appealed(bytes32 indexed caseId, uint32 level, address indexed appellant, bytes32 backed);
    event RerunRequired(bytes32 indexed caseId, uint64 deadline);
    event Decided(bytes32 indexed caseId, bytes32 root);
    event CaseClosed(bytes32 indexed caseId, OnyxLedger.ShardStatus status);
    event Released(uint64 indexed round, bytes32 indexed shard);

    // ------------------------------------------------------------------ state

    /// The ledger this game judges for, set once by the deployer (`bind`).
    /// Typed as the extension: its functions and the settlement's own both answer at the
    /// settlement's address (ADR-0024 item 1).
    OnyxSettlementExt public settlement;
    address internal immutable _deployer;
    /// `blockhash` covers only the 256 most recent blocks; a panel is drawn within them.
    uint64 public constant DRAW_WINDOW = OnyxRound.DRAW_WINDOW;
    /// Blocks after a decision that a challenger whose claim is not revealed yet can still
    /// reveal it before anyone may judge the case, so a `resolve` front-run cannot turn an
    /// Overturned into a Corrected (M6-L3-F2). Fits inside `SETTLE_MARGIN` (64).
    uint64 public constant CLAIM_GRACE = 16;

    mapping(bytes32 => Case) internal _cases;
    /// caseId => level => member => vote.
    mapping(bytes32 => mapping(uint32 => mapping(address => Vote))) public votes;
    /// Never drawn again in this case: earlier panel members, the challenger, appellants.
    mapping(bytes32 => mapping(address => bool)) public excluded;
    /// `panelSlash` a validator stakes on open panels (ADR-0021 item 19).
    mapping(address => uint256) public exposure;
    /// Cases ever opened on (round, shard): the `n` of the case id; also blocks `release`.
    mapping(uint64 => mapping(bytes32 => uint64)) public caseCount;
    /// The latest case on (round, shard).
    mapping(uint64 => mapping(bytes32 => bytes32)) public caseOf;
    /// Deferred miner payouts of an undisputed unanimous shard were paid (`release`).
    mapping(uint64 => mapping(bytes32 => bool)) public released;
    /// caseId => level => root => reveals backing it (M6-L2-F2).
    mapping(bytes32 => mapping(uint32 => mapping(bytes32 => uint32))) public rootVotes;

    constructor() {
        _deployer = msg.sender;
    }

    /// @notice Bind to the settlement contract that names this one as its `disputes`, once,
    /// by the deployer. The settlement opens no round until the pair is bound both ways.
    function bind(OnyxSettlementExt settlement_) external {
        if (
            msg.sender != _deployer || address(settlement) != address(0)
                || settlement_.disputes() != address(this)
        ) revert WrongPhase();
        settlement = settlement_;
    }

    // ------------------------------------------------------------------ pure rules

    /// Rust `dispute_id`: keccak256("onyx-dispute-v1" || ledger || round || shard ||
    /// challenger || claim || n). Here `claim` is the claim commitment (zero for a rerun case).
    function disputeId(
        bytes32 ledger,
        uint64 round,
        bytes32 shard,
        address challenger,
        bytes32 claim,
        uint64 n
    ) public pure returns (bytes32) {
        return keccak256(
            abi.encodePacked("onyx-dispute-v1", ledger, round, shard, challenger, claim, n)
        );
    }

    /// Rust `claim_commit`.
    function claimCommit(
        bytes32 ledger,
        uint64 round,
        bytes32 shard,
        address challenger,
        bytes32 root,
        bytes32 salt
    ) public pure returns (bytes32) {
        return keccak256(
            abi.encodePacked("onyx-claim-v1", ledger, round, shard, challenger, root, salt)
        );
    }

    /// Rust `panel_commit_hash`.
    function panelCommitHash(
        bytes32 caseId,
        uint32 level,
        address member,
        bytes32 root,
        bytes32 salt
    ) public pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-panel-vote-v1", caseId, level, member, root, salt));
    }

    /// Rust `draw_panel` sort key.
    function panelKey(bytes32 seed, bytes32 caseId, uint32 level, bytes32 beacon, address c)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked("onyx-panel-v1", seed, caseId, level, beacon, c));
    }

    /// Rust `no_valid_root_marker`.
    function noValidRootMarker(bytes32 shard) public pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-no-valid-root-v1", shard));
    }

    /// Rust `verdict_right`: 0 = not judged (Abstain), 1 = right, 2 = wrong. `noValid` means
    /// the reference is "no valid root".
    function verdictRight(
        uint8 kind,
        bytes32 verdictRoot,
        bytes32 minerRoot,
        bytes32 ref,
        bool noValid
    ) public pure returns (uint8) {
        if (kind == OnyxRound.ABSTAIN) return 0;
        bool right;
        if (noValid) right = kind == OnyxRound.NO_VALID_ROOT;
        else if (kind == OnyxRound.AGREE) right = minerRoot == ref;
        else if (kind == OnyxRound.DISAGREE) right = minerRoot != ref && verdictRoot == ref;
        return right ? 1 : 2;
    }

    // ------------------------------------------------------------------ opening

    /// @notice Open a rerun case on a `Pending` shard, before `pendingClose` (anyone, no bond):
    /// a panel attests the rerun so a lone dissenting verdict cannot void the shard for free
    /// (M6-F10). Its first panel is paid from the round budget.
    function openRerun(uint64 round, bytes32 shard) external returns (bytes32 caseId) {
        (OnyxLedger.ShardStatus st,,,,) = settlement.shardState(round, shard);
        if (st != OnyxLedger.ShardStatus.Pending) revert WrongPhase();
        OnyxRound.DisputeConfig memory d = settlement.roundConfig(round).dispute;
        caseId = _open(round, shard, Kind.Rerun, address(0), bytes32(0));
        _cases[caseId].levels.push();
        Level storage l = _cases[caseId].levels[0];
        l.bond = d.minChallengeBond;
        l.anchor = uint64(block.number);
        l.size = d.panelSize;
        settlement.dBegin(round, shard);
    }

    /// @notice Challenge a `Finalized` root inside its challenge window, with a bond and a
    /// commitment to the claimed root (`claimCommit`), revealed later with `revealClaim`.
    function challenge(uint64 round, bytes32 shard, bytes32 claim, uint128 bond)
        external
        returns (bytes32 caseId)
    {
        (OnyxLedger.ShardStatus st,,, bytes32 root,) = settlement.shardState(round, shard);
        if (st != OnyxLedger.ShardStatus.Finalized) revert WrongPhase();
        OnyxRound.DisputeConfig memory d = settlement.roundConfig(round).dispute;
        if (bond < d.minChallengeBond) revert BondTooLow();
        caseId = _open(round, shard, Kind.Challenge, msg.sender, claim);
        Case storage c = _cases[caseId];
        c.oldRoot = root;
        c.levels.push();
        Level storage l = c.levels[0];
        l.funder = msg.sender;
        l.bond = bond;
        l.anchor = uint64(block.number);
        l.size = d.panelSize;
        excluded[caseId][msg.sender] = true;
        settlement.dBegin(round, shard);
        settlement.dPull(msg.sender, bond);
    }

    function _open(uint64 round, bytes32 shard, Kind kind, address challenger, bytes32 claim)
        internal
        returns (bytes32 caseId)
    {
        uint64 n = caseCount[round][shard]++;
        caseId = disputeId(settlement.ledgerId(), round, shard, challenger, claim, n);
        Case storage c = _cases[caseId];
        c.round = round;
        c.shard = shard;
        c.kind = kind;
        c.state = State.Open;
        c.challenger = challenger;
        c.claim = claim;
        caseOf[round][shard] = caseId;
        emit CaseOpened(caseId, round, shard, kind);
    }

    /// @notice The challenger reveals its claimed root: any time before the case is judged
    /// (`settleCase` waits `CLAIM_GRACE` blocks after a decision for it, M6-L3-F2); best after
    /// the last panel's reveals, so panel members cannot copy it.
    function revealClaim(bytes32 caseId, bytes32 root, bytes32 salt) external {
        Case storage c = _cases[caseId];
        if (
            c.kind != Kind.Challenge
                || (c.state != State.Open
                    && c.state != State.RerunRequired
                    && c.state != State.Decided)
        ) {
            revert WrongPhase();
        }
        if (
            msg.sender != c.challenger
                || claimCommit(settlement.ledgerId(), c.round, c.shard, c.challenger, root, salt)
                    != c.claim
        ) revert BadClaim();
        c.claimedRoot = root;
        c.claimRevealed = true;
    }

    // ------------------------------------------------------------------ panels

    /// @notice Draw the current level's panel from `blockhash(anchor)` (anyone, from
    /// `anchor + 1`). Exactly `size` members or none; too few candidates or a lapsed beacon
    /// sends the case to the coordinator's rerun (no revert, M5-L49-F3).
    function draw(bytes32 caseId) external {
        Case storage c = _cases[caseId];
        if (c.state != State.Open) revert WrongPhase();
        uint32 lv = uint32(c.levels.length - 1);
        Level storage l = c.levels[lv];
        if (l.drawnAt != 0 || block.number <= l.anchor) revert WrongPhase();
        if (block.number - l.anchor > DRAW_WINDOW) {
            _toRerun(caseId, c);
            return;
        }
        address[] memory chosen = _choose(caseId, c, lv, l.size, blockhash(l.anchor));
        if (chosen.length == 0) {
            _toRerun(caseId, c);
            return;
        }
        uint256 slash = settlement.roundConfig(c.round).dispute.panelSlash;
        for (uint256 i = 0; i < chosen.length; i++) {
            address m = chosen[i];
            l.members.push(m);
            votes[caseId][lv][m].member = true;
            excluded[caseId][m] = true;
            exposure[m] += slash;
        }
        l.drawnAt = uint64(block.number);
        emit PanelDrawn(caseId, lv, chosen);
    }

    /// The eligible round members, drawn by `drawPanel`. Eligible (as Rust `open_dispute`):
    /// not a miner who revealed on the shard, no verdict committed on it, not excluded in this case, and stake minus open
    /// exposure covers `panelSlash` (ADR-0021 items 2, 19).
    function _choose(bytes32 caseId, Case storage c, uint32 lv, uint32 size, bytes32 beacon)
        internal
        view
        returns (address[] memory)
    {
        address[] memory members = settlement.roundMembers(c.round);
        uint256 slash = settlement.roundConfig(c.round).dispute.panelSlash;
        address[] memory cand = new address[](members.length);
        uint256 n;
        for (uint256 i = 0; i < members.length; i++) {
            address v = members[i];
            if (excluded[caseId][v]) continue;
            if (settlement.commitOf(c.round, c.shard, v).revealed) continue;
            if (settlement.tallyOf(c.round, c.shard, v).committed > 0) continue;
            (uint128 stake,,,) = settlement.validators(v);
            if (uint256(stake) < exposure[v] + slash) continue;
            cand[n++] = v;
        }
        assembly ("memory-safe") {
            mstore(cand, n)
        }
        return drawPanel(settlement.seedOf(c.round), caseId, lv, beacon, cand, size);
    }

    /// Rust `draw_panel`: the `size` candidates with the smallest `panelKey`, in key order, or
    /// none when fewer are available (a panel never shrinks, L29-F2). `size` partial-selection
    /// passes over the candidates (both bounded by the registry size).
    function drawPanel(
        bytes32 seed,
        bytes32 caseId,
        uint32 level,
        bytes32 beacon,
        address[] memory cand,
        uint32 size
    ) public pure returns (address[] memory chosen) {
        uint256 n = cand.length;
        if (size == 0 || n < size) return chosen;
        bytes32[] memory keys = new bytes32[](n);
        for (uint256 i = 0; i < n; i++) {
            keys[i] = panelKey(seed, caseId, level, beacon, cand[i]);
        }
        chosen = new address[](size);
        for (uint256 k = 0; k < size; k++) {
            uint256 best = k;
            for (uint256 j = k + 1; j < n; j++) {
                if (keys[j] < keys[best]) best = j;
            }
            (keys[k], keys[best]) = (keys[best], keys[k]);
            (cand[k], cand[best]) = (cand[best], cand[k]);
            chosen[k] = cand[k];
        }
    }

    /// @notice A drawn member commits `panelCommitHash(caseId, level, member, root, salt)`
    /// within `panelCommitWindow` blocks of the draw.
    function commitVote(bytes32 caseId, bytes32 commit) external {
        (, uint32 lv, Level storage l, OnyxRound.DisputeConfig memory d) = _current(caseId);
        if (block.number >= uint256(l.drawnAt) + d.panelCommitWindow) revert WrongPhase();
        Vote storage v = votes[caseId][lv][msg.sender];
        if (!v.member) revert NotMember();
        if (commit == bytes32(0) || v.commit != bytes32(0)) revert BadVote();
        v.commit = commit;
    }

    /// @notice Reveal a committed panel vote in the reveal window.
    function revealVote(bytes32 caseId, bytes32 root, bytes32 salt) external {
        (, uint32 lv, Level storage l, OnyxRound.DisputeConfig memory d) = _current(caseId);
        uint256 commitClose = uint256(l.drawnAt) + d.panelCommitWindow;
        if (block.number < commitClose || block.number >= commitClose + d.panelRevealWindow) {
            revert WrongPhase();
        }
        Vote storage v = votes[caseId][lv][msg.sender];
        if (v.revealed || v.commit == bytes32(0)) revert BadVote();
        if (panelCommitHash(caseId, lv, msg.sender, root, salt) != v.commit) revert BadVote();
        v.revealed = true;
        v.root = root;
        uint32 n = ++rootVotes[caseId][lv][root];
        // A strict majority is unique, so tracking the most-backed root is exact for it.
        if (n > l.leadCount) {
            l.lead = root;
            l.leadCount = n;
        }
        emit VoteRevealed(caseId, lv, msg.sender, root);
    }

    /// @notice Appeal a panel that is not unanimous, after its reveals and inside its appeal
    /// window, while appeals remain: bond at least `appealFloor` (`minChallengeBond * 2^level`
    /// and the next panel's fees, M6-L2-F3), backing a root. Never refused for lack of a larger panel: the draw sends the case
    /// to the rerun instead, and the bond is judged there (M5-L49-F1).
    function appeal(bytes32 caseId, bytes32 backed, uint128 bond) external {
        (Case storage c, uint32 lv, Level storage l, OnyxRound.DisputeConfig memory d) =
            _current(caseId);
        uint256 revealClose = uint256(l.drawnAt) + d.panelCommitWindow + d.panelRevealWindow;
        if (
            block.number < revealClose || block.number >= revealClose + d.appealWindow
                || lv >= d.maxAppeals || _unanimous(caseId, lv, l)
        ) revert WrongPhase();
        uint32 next = 2 * l.size + 1;
        if (uint256(bond) < OnyxRound.appealFloor(d, lv + 1, next)) revert BondTooLow();
        c.levels.push();
        Level storage nl = c.levels[lv + 1];
        nl.funder = msg.sender;
        nl.bond = bond;
        nl.backed = backed;
        nl.anchor = uint64(block.number);
        nl.size = next;
        excluded[caseId][msg.sender] = true;
        emit Appealed(caseId, lv, msg.sender, backed);
        settlement.dPull(msg.sender, bond);
    }

    function _current(bytes32 caseId)
        internal
        view
        returns (Case storage c, uint32 lv, Level storage l, OnyxRound.DisputeConfig memory d)
    {
        c = _cases[caseId];
        if (c.state != State.Open) revert WrongPhase();
        lv = uint32(c.levels.length - 1);
        l = c.levels[lv];
        if (l.drawnAt == 0) revert WrongPhase();
        d = settlement.roundConfig(c.round).dispute;
    }

    function _unanimous(bytes32 caseId, uint32 lv, Level storage l) internal view returns (bool) {
        bytes32 first = votes[caseId][lv][l.members[0]].root;
        for (uint256 i = 0; i < l.members.length; i++) {
            Vote storage v = votes[caseId][lv][l.members[i]];
            if (!v.revealed || v.root != first) return false;
        }
        return true;
    }

    // ------------------------------------------------------------------ decision

    /// @notice After the last level's appeal window: a strict majority of that panel decides
    /// (missing reveals count against every root); otherwise, or if the panel was never drawn
    /// in its beacon window, the case waits for the coordinator's rerun.
    function resolve(bytes32 caseId) external {
        Case storage c = _cases[caseId];
        if (c.state != State.Open) revert WrongPhase();
        uint32 lv = uint32(c.levels.length - 1);
        Level storage l = c.levels[lv];
        if (l.drawnAt == 0) {
            if (block.number <= uint256(l.anchor) + DRAW_WINDOW) revert WrongPhase();
            _toRerun(caseId, c);
            return;
        }
        OnyxRound.DisputeConfig memory d = settlement.roundConfig(c.round).dispute;
        if (
            block.number
                < uint256(l.drawnAt) + d.panelCommitWindow + d.panelRevealWindow + d.appealWindow
        ) revert WrongPhase();
        // Missing reveals count against every root: the majority is of the panel's size.
        if (2 * uint256(l.leadCount) > l.members.length) {
            _decide(caseId, c, l.lead);
            return;
        }
        _toRerun(caseId, c);
    }

    /// @notice The coordinator posts the root of the rerun it ran (q13 (a)) for a case that
    /// needs one, before the deadline. It decides exactly as a majority would.
    function arbitrate(bytes32 caseId, bytes32 root) external {
        Case storage c = _cases[caseId];
        if (msg.sender != settlement.coordinator()) revert NotCoordinator();
        if (c.state != State.RerunRequired || block.number >= c.rerunDeadline) revert WrongPhase();
        _decide(caseId, c, root);
    }

    /// @notice A rerun that never came: the case ends `Unresolved` (Rust `expire_rerun`).
    /// Funders pay their panels' full fees (burned) and get the rest back; revealed miner
    /// bonds return; nobody is paid or slashed; the shard is closed and never final.
    function expire(bytes32 caseId) external {
        Case storage c = _cases[caseId];
        if (c.state != State.RerunRequired || block.number < c.rerunDeadline) revert WrongPhase();
        c.state = State.Unresolved;
        OnyxRound.DisputeConfig memory d = settlement.roundConfig(c.round).dispute;
        for (uint256 i = 0; i < c.levels.length; i++) {
            Level storage l = c.levels[i];
            _releaseExposure(l, d.panelSlash);
            if (l.funder == address(0)) continue;
            // Only a panel that sat costs its funder fees (Rust charges drawn panels, M6-L2-F5).
            uint256 fees = l.drawnAt == 0 ? 0 : uint256(d.panelFee) * l.size;
            if (fees > l.bond) fees = l.bond;
            settlement.dBurn(fees);
            settlement.dCredit(l.funder, l.bond - fees);
        }
        _settleMiners(c.round, c.shard, bytes32(0), true, c.kind == Kind.Challenge);
        settlement.dEnd(c.round, c.shard, OnyxLedger.ShardStatus.Unresolved, 0, 0);
        emit CaseClosed(caseId, OnyxLedger.ShardStatus.Unresolved);
    }

    /// The coordinator has `challengeWindow` blocks for the rerun, but never past
    /// `lockEnd - SETTLE_MARGIN`: the decided case must still be judged while stakes are locked
    /// (M6-L2-F4). A case sent to the rerun later than that can only expire.
    function _toRerun(bytes32 caseId, Case storage c) internal {
        c.state = State.RerunRequired;
        OnyxRound.RoundConfig memory cfg = settlement.roundConfig(c.round);
        uint256 deadline = block.number + cfg.dispute.challengeWindow;
        uint256 cap = uint256(cfg.validation.pendingClose) + OnyxRound.disputeTail(cfg.dispute)
            - OnyxRound.SETTLE_MARGIN;
        if (deadline > cap) deadline = cap > block.number ? cap : block.number;
        c.rerunDeadline = uint64(deadline);
        emit RerunRequired(caseId, c.rerunDeadline);
    }

    function _decide(bytes32 caseId, Case storage c, bytes32 root) internal {
        c.state = State.Decided;
        c.decided = root;
        c.decidedAt = uint64(block.number);
        emit Decided(caseId, root);
    }

    // ------------------------------------------------------------------ judging

    /// @notice Judge a decided case, `maxMembers` round members per call (the bound holds at
    /// 128 validators x 32 reveals). The last call judges miners, panels and funders and closes
    /// the shard. Honest miners and the challenger are paid only by it, so they drive it.
    function settleCase(bytes32 caseId, uint256 maxMembers) external {
        Case storage c = _cases[caseId];
        if (c.state != State.Decided) revert WrongPhase();
        // A challenge decided against the old root pays the challenger only if its claim is
        // revealed: give it `CLAIM_GRACE` blocks to do so (M6-L3-F2).
        if (
            c.kind == Kind.Challenge && !c.claimRevealed && c.decided != c.oldRoot
                && block.number < uint256(c.decidedAt) + CLAIM_GRACE
        ) revert WrongPhase();
        // An upheld challenge changes no verdict judgement: nothing to do per member.
        bool judge = c.kind == Kind.Rerun || c.decided != c.oldRoot;
        address[] memory members = settlement.roundMembers(c.round);
        uint256 end = c.cursor + maxMembers;
        if (end > members.length || !judge) end = members.length;
        if (judge && c.cursor < end) {
            Ctx memory x = _ctx(c);
            if (c.cursor == 0 && !x.slashOnly) c.lostLeft = x.lost;
            for (uint256 i = c.cursor; i < end; i++) {
                _judgeValidator(c, x, members[i]);
            }
        }
        c.cursor = uint32(end);
        if (end == members.length) _finish(caseId, c);
    }

    /// What judging a validator needs, read once per call.
    struct Ctx {
        bytes32 seed;
        bytes32 ref;
        bool noValid;
        /// Fees and missed tasks were settled at finalize (a challenged unanimous shard).
        bool slashOnly;
        OnyxRound.ValidationConfig vc;
        address[] miners;
        bytes32[] roots;
        bool[] canary;
        /// Bonds of the non-canary reveals that differ from the decided root (lost).
        uint256 lost;
    }

    function _ctx(Case storage c) internal view returns (Ctx memory x) {
        OnyxRound.RoundConfig memory cfg = settlement.roundConfig(c.round);
        x.seed = settlement.seedOf(c.round);
        x.ref = c.decided;
        x.noValid = c.decided == noValidRootMarker(c.shard);
        x.slashOnly = c.kind == Kind.Challenge;
        x.vc = cfg.validation;
        (, OnyxLedger.Reveal[] memory rv) = settlement.shardMiners(c.round, c.shard);
        uint256 n = rv.length;
        x.miners = new address[](n);
        x.roots = new bytes32[](n);
        x.canary = new bool[](n);
        for (uint256 j = 0; j < n; j++) {
            x.miners[j] = rv[j].miner;
            x.roots[j] = rv[j].root;
            OnyxLedger.Commit memory cm = settlement.commitOf(c.round, c.shard, rv[j].miner);
            x.canary[j] = cm.canary;
            if (!cm.canary && rv[j].root != x.ref) x.lost += cm.bond;
        }
    }

    /// One validator against the decided root, as Rust `Validation::settle`: a right verdict
    /// earns the fee once per revealed root (canaries share one key); each wrong verdict is
    /// slashed; assigned and unanswered is a missed task. An unrevealed commit counts as an
    /// answer (slashed at round level instead, L25-F9). The first fee is the member's share of
    /// the shard's reservation; a fee on a further root comes from the lost miner bonds, and
    /// what they cannot cover is recorded owed (ADR-0022 Amendment 1, review L2-R1).
    function _judgeValidator(Case storage c, Ctx memory x, address v) internal {
        uint16 committed = settlement.tallyOf(c.round, c.shard, v).committed;
        uint256 n = x.miners.length;
        if (committed == 0) {
            if (x.slashOnly) return;
            for (uint256 j = 0; j < n; j++) {
                if (OnyxRound.assigned(x.seed, v, c.shard, x.miners[j], x.vc.ratePpm)) {
                    settlement.dSlash(v, x.vc.missedSlash);
                    return;
                }
            }
            return;
        }
        bool answered;
        bytes32[] memory paid = new bytes32[](n);
        uint256 nPaid;
        for (uint256 j = 0; j < n; j++) {
            OnyxLedger.VerdictRec memory rec =
                settlement.verdictOf(c.round, c.shard, x.miners[j], v);
            if (rec.commit == bytes32(0)) continue;
            if (!rec.revealed) {
                answered = true;
                continue;
            }
            uint8 res = verdictRight(rec.kind, rec.root, x.roots[j], x.ref, x.noValid);
            // Abstain is not an answer (L23-F7).
            if (res == 0) continue;
            answered = true;
            if (res == 2) {
                settlement.dSlash(v, x.vc.validatorSlash);
            } else if (!x.slashOnly) {
                bytes32 key = x.canary[j] ? bytes32(0) : x.roots[j];
                bool seen;
                for (uint256 k = 0; k < nPaid; k++) {
                    if (paid[k] == key) seen = true;
                }
                if (!seen) {
                    paid[nPaid++] = key;
                    if (nPaid == 1) settlement.dSpend(c.round, c.shard, v, x.vc.validatorFee);
                    else _extraFee(c, v, x.vc.validatorFee);
                }
            }
        }
        if (!answered && !x.slashOnly) settlement.dSlash(v, x.vc.missedSlash);
    }

    /// An extra fee from the case's lost miner bonds while they last; the rest is owed.
    function _extraFee(Case storage c, address v, uint256 fee) internal {
        uint256 take = c.lostLeft < fee ? c.lostLeft : fee;
        c.lostLeft -= take;
        settlement.dExtra(c.round, c.shard, v, fee, take);
    }

    /// Miners against `root` (Rust `settle`): a reveal equal to it gets its bond back and the
    /// earliest such commit the reward; other non-canary reveals lose their bond (returned
    /// as `lost`); canary bonds return; unrevealed bonds burn unless already burned (a
    /// finalized shard). `voidAll` (an Unresolved case) returns every revealed bond, no reward.
    function _settleMiners(
        uint64 round,
        bytes32 shard,
        bytes32 root,
        bool voidAll,
        bool burnedAlready
    ) internal returns (address winner, bytes32 binding, uint256 lost) {
        (address[] memory cm, OnyxLedger.Reveal[] memory rv) = settlement.shardMiners(round, shard);
        for (uint256 i = 0; i < cm.length; i++) {
            OnyxLedger.Commit memory cmt = settlement.commitOf(round, shard, cm[i]);
            uint128 bond = cmt.bond;
            bool canary = cmt.canary;
            if (!cmt.revealed) {
                if (!burnedAlready) settlement.dBurn(bond);
                continue;
            }
            bytes32 r;
            bytes32 b;
            for (uint256 j = 0; j < rv.length; j++) {
                if (rv[j].miner == cm[i]) (r, b) = (rv[j].root, rv[j].binding);
            }
            if (voidAll || canary || r == root) {
                settlement.dCredit(cm[i], bond);
                // cm is in commit order, so the first correct one has the lowest sequence.
                if (!voidAll && !canary && winner == address(0)) (winner, binding) = (cm[i], b);
            } else {
                lost += bond;
            }
        }
        if (winner != address(0)) {
            settlement.dSpend(
                round, shard, winner, settlement.roundConfig(round).validation.minerReward
            );
        }
    }

    function _finish(bytes32 caseId, Case storage c) internal {
        c.state = State.Done;
        bytes32 root = c.decided;
        (address winner, bytes32 binding, uint256 lost) =
            _settleMiners(c.round, c.shard, root, false, c.kind == Kind.Challenge);
        // A rerun case's extra fees were paid from these bonds: only the rest burns.
        if (c.kind == Kind.Rerun) lost = c.lostLeft;
        OnyxRound.DisputeConfig memory d = settlement.roundConfig(c.round).dispute;
        for (uint256 i = 0; i < c.levels.length; i++) {
            uint256 rest = _settlePanel(caseId, c, uint32(i), root, d);
            Level storage l = c.levels[i];
            if (i == 0 && c.kind == Kind.Challenge) {
                _settleChallenger(c, rest, winner, lost);
                lost = 0;
            } else if (l.funder != address(0)) {
                if (l.backed == root) settlement.dCredit(l.funder, rest);
                else settlement.dBurn(rest);
            }
        }
        if (lost > 0) settlement.dBurn(lost);
        OnyxLedger.ShardStatus st = root == noValidRootMarker(c.shard)
            ? OnyxLedger.ShardStatus.NoValidRoot
            : OnyxLedger.ShardStatus.Finalized;
        settlement.dEnd(
            c.round,
            c.shard,
            st,
            st == OnyxLedger.ShardStatus.Finalized ? root : bytes32(0),
            binding
        );
        emit CaseClosed(caseId, st);
    }

    /// One panel against R: members who revealed R earn `panelFee` from the level's funder
    /// (the round budget for a rerun case's first panel); members who revealed another root
    /// lose `panelSlash`; silent members lose it only if someone on the panel revealed R
    /// (ADR-0021 item 24, also after a rerun: ADR-0024 item 8). A drawn panel where nobody
    /// revealed costs its funder the full panel's fees, burned, as in Rust (L30-F1,
    /// M6-L3-F3). Returns the funder's rest.
    function _settlePanel(
        bytes32 caseId,
        Case storage c,
        uint32 lv,
        bytes32 root,
        OnyxRound.DisputeConfig memory d
    ) internal returns (uint256 rest) {
        Level storage l = c.levels[lv];
        address[] storage ms = l.members;
        bool someoneRight;
        bool anyReveal;
        for (uint256 i = 0; i < ms.length; i++) {
            Vote storage v = votes[caseId][lv][ms[i]];
            if (v.revealed) anyReveal = true;
            if (v.revealed && v.root == root) someoneRight = true;
        }
        rest = l.funder == address(0) ? 0 : l.bond;
        if (rest > 0 && l.drawnAt != 0 && !anyReveal) {
            uint256 fees = uint256(d.panelFee) * l.size;
            if (fees > rest) fees = rest;
            rest -= fees;
            settlement.dBurn(fees);
        }
        for (uint256 i = 0; i < ms.length; i++) {
            address m = ms[i];
            Vote storage v = votes[caseId][lv][m];
            exposure[m] -= d.panelSlash;
            if (v.revealed && v.root == root) {
                if (l.funder == address(0)) {
                    settlement.dSpend(c.round, c.shard, m, d.panelFee);
                } else {
                    uint256 fee = d.panelFee < rest ? d.panelFee : rest;
                    rest -= fee;
                    settlement.dCredit(m, fee);
                }
            } else if (v.revealed || someoneRight) {
                settlement.dSlash(m, d.panelSlash);
            }
        }
    }

    /// The challenger's rest: Upheld (R is the old root) loses it, half to the earliest correct
    /// miner and half burned; Overturned (R differs and equals the revealed claim) keeps it and
    /// earns half of the miner bonds lost; Corrected loses it. Lost miner bonds not paid to
    /// the challenger burn.
    function _settleChallenger(Case storage c, uint256 rest, address winner, uint256 lost)
        internal
    {
        bytes32 root = c.decided;
        if (root == c.oldRoot) {
            uint256 half = winner == address(0) ? 0 : rest / 2;
            if (half > 0) settlement.dCredit(winner, half);
            settlement.dBurn(rest - half + lost);
        } else if (c.claimRevealed && c.claimedRoot == root) {
            settlement.dCredit(c.challenger, rest + lost / 2);
            settlement.dBurn(lost - lost / 2);
        } else {
            settlement.dBurn(rest + lost);
        }
    }

    function _releaseExposure(Level storage l, uint256 slash) internal {
        for (uint256 i = 0; i < l.members.length; i++) {
            exposure[l.members[i]] -= slash;
        }
    }

    // ------------------------------------------------------------------ release

    /// @notice Pay a final, never-disputed unanimous shard's miners: bonds back and the reward
    /// to the earliest commit (ADR-0024 item 2). Anyone may call it once the root is final.
    function release(uint64 round, bytes32 shard) external {
        if (
            !settlement.isFinal(round, shard) || released[round][shard]
                || caseCount[round][shard] != 0
        ) revert WrongPhase();
        released[round][shard] = true;
        (,,, bytes32 root,) = settlement.shardState(round, shard);
        // Unrevealed bonds burned at finalize; every reveal of a unanimous shard is correct.
        _settleMiners(round, shard, root, false, true);
        emit Released(round, shard);
    }

    // ------------------------------------------------------------------ readers

    function caseInfo(bytes32 caseId)
        external
        view
        returns (
            uint64 round,
            bytes32 shard,
            Kind kind,
            State state,
            bytes32 decided,
            uint64 rerunDeadline,
            uint256 levels
        )
    {
        Case storage c = _cases[caseId];
        return (c.round, c.shard, c.kind, c.state, c.decided, c.rerunDeadline, c.levels.length);
    }

    function panel(bytes32 caseId, uint32 level)
        external
        view
        returns (address[] memory members, uint64 drawnAt, uint64 anchor, address funder)
    {
        Level storage l = _cases[caseId].levels[level];
        return (l.members, l.drawnAt, l.anchor, l.funder);
    }
}
