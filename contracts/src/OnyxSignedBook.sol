// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxSig} from "./OnyxSig.sol";

/// @title OnyxSignedBook
/// @notice The level-3 signed-source book on chain (ADR-0028 amendment 6, docs/spec/proof-ladder.md
/// "Level 3 on chain"). One book for every coordinator and client: the key registry, the
/// (key, sequence) book, the payload book and the fact book of `onyx_coordinator::signed`, with
/// every time rule on `block.timestamp` (THREATS L3-F7, L3R-F8).
///
/// Anyone may relay a signed report (`postReport`): the contract hashes the report's Borsh payload
/// bytes exactly as the source signed them, parses the payload's fixed layout, checks the EIP-191
/// signature against the registered key and applies the spec's rules R-SIG-3..6. Retirement and
/// compromise are O(1): an accepted report is withdrawn when read if its key no longer covers it,
/// and a compromised key's reports drop out of disputes the same way, which gives the eager
/// off-chain store's results (checked by the differential traces in fixtures/signed/traces.json).
/// Nothing is ever pruned: a used (key, sequence) stays used for good. The reviewer is one
/// immutable key (multisig or timelock: TODO H1); its decisions are events.
///
/// No contract reads this book for money yet: level-3 settlement waits for ROADMAP H6.
///
/// Track records (proof level 5, ADR-0036 amendment 3) use the book twice, never for money: its
/// settleable facts are the level-3 answers a track statement is judged against, and it keeps
/// append-only logs of track commitments (`postTrackCommitment`, one per poster and track) and
/// each source's operator (`declareOperator`), so the latest commitment and every affiliation
/// are public.
contract OnyxSignedBook {
    /// Most seconds a report's observation time may lie ahead of the block time.
    uint64 public constant MAX_CLOCK_SKEW_SECS = 300;
    /// Oldest report taken in, in seconds before the block time (7 days).
    uint64 public constant MAX_REPORT_AGE_SECS = 7 * 86_400;
    /// A retired key's reports count when observed in `[at - grace, at)` and posted before
    /// `at + grace` (L3R-F1, L3R2-F4).
    uint64 public constant RETIRED_GRACE_SECS = 3_600;
    /// How long an accepted fact must stand before `settleable` returns it (L3R2-F7).
    uint64 public constant DISPUTE_WINDOW_SECS = 3_600;
    /// Sequence numbers are `0..type(uint64).max`; the top value is reserved (L3R-F8).
    uint64 public constant RESERVED_SEQUENCE = type(uint64).max;
    /// `Value::Fixed` scales 1..=38 and `Value::Bytes` up to 1024 bytes (`Value::validate`).
    uint256 internal constant MAX_FIXED_SCALE = 38;
    uint256 internal constant MAX_VALUE_BYTES = 1024;
    /// Borsh length of `onyx_track::TrackCommitment`: eight hashes and the u64 fields `as_of`,
    /// `entry_count`, `record_count`, `metric_count` (ADR-0036 amendment 3).
    uint256 public constant TRACK_COMMITMENT_BYTES = 288;

    enum Life {
        None,
        Active,
        Retired,
        Compromised
    }

    enum FactState {
        None,
        Accepted,
        Disputed
    }

    /// What `postReport` did when it changed the book (everything else reverts).
    enum Outcome {
        None,
        /// The report is the fact's accepted value.
        Accepted,
        /// A contradiction: the fact is disputed and every key involved suspended.
        Equivocation,
        /// The fact is disputed; the report joined it as evidence and agrees with every report.
        FactDisputed
    }

    /// Which rule an equivocation broke.
    enum EquivocationKind {
        Sequence,
        Fact
    }

    struct Key {
        address signer;
        Life life;
        bool suspended;
        /// Ever retired: `retiredAt` still withdraws late reports after a compromise.
        bool retired;
        uint64 retiredAt;
        uint64 since;
        uint64 compromisedAt;
        uint64 suspendedAt;
        /// keccak256 of the source id's UTF-8 bytes.
        bytes32 source;
    }

    /// A validly signed report the book recorded (accepted or not), by payload digest.
    struct Rep {
        bytes32 key;
        bytes32 fact;
        /// keccak256(borsh(value)).
        bytes32 valueHash;
        /// `SignedReport::hash`: what a claim names.
        bytes32 reportHash;
        uint64 observed;
        uint64 sequence;
        uint64 recordedAt;
    }

    struct Fact {
        FactState state;
        uint64 acceptedAt;
        /// Payload digest of the accepted report.
        bytes32 accepted;
        /// Payload digests of the dispute's reports, at most one per key, in arrival order.
        bytes32[] dispute;
    }

    /// The parsed fixed layout of a Borsh `SignedPayload`.
    struct Parsed {
        uint256 sourceEnd;
        uint256 subjectEnd;
        bytes32 key;
        uint64 observed;
        uint64 sequence;
    }

    address public immutable reviewer;

    mapping(bytes32 key => Key) internal _keys;
    /// One signer, one registration, ever (L3R4-F1).
    mapping(address signer => bytes32 key) public keyOfSigner;
    /// Source hash -> origin hash; origin hash -> source hash (R-SIG-1, L3R-F7).
    mapping(bytes32 source => bytes32 origin) public originOfSource;
    mapping(bytes32 origin => bytes32 source) public sourceOfOrigin;
    /// (key, sequence) -> payload digest, never pruned (R-SIG-5).
    mapping(bytes32 key => mapping(uint64 sequence => bytes32 digest)) public sequenceOf;
    mapping(bytes32 digest => Rep) internal _reps;
    mapping(bytes32 fact => Fact) internal _facts;

    /// One poster's append-only log of track commitments (ADR-0036 amendment 3).
    struct TrackLog {
        uint64 count;
        uint64 asOf;
        bytes32 latest;
    }

    /// Source hash -> its operator, declared by the reviewer before the source's first key and
    /// never changed (zero: the source is its own operator). Two sources with one operator never
    /// resolve each other's track statements (THREATS TR-R2-4).
    mapping(bytes32 source => bytes32 operator) public operatorOf;
    mapping(address poster => mapping(bytes32 track => TrackLog)) internal _trackLogs;
    /// (poster, track, index) -> commitment hash, never changed once written.
    mapping(
        address poster => mapping(bytes32 track => mapping(uint64 index => bytes32 commitment))
    ) public trackCommitmentAt;

    event KeyRegistered(
        bytes32 indexed key,
        address indexed signer,
        string source,
        string origin,
        bytes32 provenance,
        address by,
        string reason
    );
    event KeyRetired(bytes32 indexed key, uint64 at, address by, string reason);
    event KeyCompromised(bytes32 indexed key, uint64 since, address by, string reason);
    event KeyReinstated(bytes32 indexed key, address by, string reason);
    event FactResolved(bytes32 indexed fact, bytes32 keep, address by, string reason);
    event ReportRecorded(
        bytes32 indexed digest,
        bytes32 indexed key,
        bytes32 indexed fact,
        uint64 sequence,
        bytes32 reportHash,
        Outcome outcome
    );
    event Equivocated(
        bytes32 indexed fact, bytes32 indexed first, bytes32 indexed second, EquivocationKind kind
    );
    event Suspended(bytes32 indexed key, bytes32 cause);
    event Withdrawn(bytes32 indexed digest);
    event OperatorDeclared(
        bytes32 indexed source, bytes32 indexed operator, string sourceId, address by, string reason
    );
    /// `commitment` is the Borsh `TrackCommitment` (everything a client needs besides the logs);
    /// `head` the block whose state it was computed at.
    event TrackCommitted(
        address indexed poster,
        bytes32 indexed track,
        uint64 indexed index,
        bytes32 commitmentHash,
        uint64 asOf,
        uint64 head,
        bytes commitment
    );

    error NotReviewer();
    error BadKey();
    error KeyAlreadyRegistered();
    error SignerTaken();
    error OriginMismatch();
    error OriginTaken();
    error UnknownKey();
    error NotActive();
    error TimeInFuture();
    error NotSuspended();
    error NotDisputed();
    error BadResolution();
    error BadPayload();
    error SequenceReserved();
    error KeySourceMismatch();
    error KeyRetiredError();
    error KeyCompromisedError();
    error KeySuspended();
    error BadSignature();
    error FromFuture();
    error Stale();
    error Replay();
    error AlreadyStated();
    error BadOperator();
    error OperatorSet();
    error SourceHasKeys();
    error BadTrackCommitment();
    error TrackNotChained();
    error TrackTimeNotForward();
    error TrackHeadNotPast();
    error ReviewerIsDeployer();

    modifier onlyReviewer() {
        if (msg.sender != reviewer) revert NotReviewer();
        _;
    }

    /// The reviewer is never the account that deploys the book (ROADMAP C10, THREATS C10-1): a
    /// deploy key sits in scripts and CI, the reviewer key registers and withdraws every source.
    /// A deploy through a factory makes the factory the sender; DeploySignedBook.s.sol refuses
    /// the broadcasting key as reviewer too.
    constructor(address reviewer_) {
        if (reviewer_ == address(0)) revert NotReviewer();
        if (reviewer_ == msg.sender) revert ReviewerIsDeployer();
        reviewer = reviewer_;
    }

    // ---------------------------------------------------------------- reviewer actions

    /// `SourceKey::id`: keccak256("onyx-source-key-v1" ‖ borsh(source, signer)).
    function keyId(string calldata source, address signer) public pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-source-key-v1", _u32le(bytes(source).length), bytes(source), signer
            )
        );
    }

    /// Register a reviewed key (R-SIG-1). `origin` is the host its keys document was fetched
    /// from and `provenance` the Keccak of that document (the review record; not checked here).
    function registerKey(
        string calldata source,
        address signer,
        string calldata origin,
        bytes32 provenance,
        string calldata reason
    ) external onlyReviewer returns (bytes32 id) {
        if (signer == address(0) || bytes(source).length == 0 || bytes(origin).length == 0) {
            revert BadKey();
        }
        if (provenance == bytes32(0)) revert BadKey();
        id = keyId(source, signer);
        if (_keys[id].life != Life.None) revert KeyAlreadyRegistered();
        if (keyOfSigner[signer] != bytes32(0)) revert SignerTaken();
        bytes32 s = keccak256(bytes(source));
        bytes32 o = keccak256(bytes(origin));
        bytes32 had = originOfSource[s];
        if (had != bytes32(0) && had != o) revert OriginMismatch();
        bytes32 owner = sourceOfOrigin[o];
        if (owner != bytes32(0) && owner != s) revert OriginTaken();
        originOfSource[s] = o;
        sourceOfOrigin[o] = s;
        keyOfSigner[signer] = id;
        Key storage k = _keys[id];
        k.signer = signer;
        k.life = Life.Active;
        k.source = s;
        emit KeyRegistered(id, signer, source, origin, provenance, msg.sender, reason);
    }

    /// Rotate an active key out at `at` (not after now). Its accepted reports observed at or
    /// after `at` stop counting (read as withdrawn, L3R5-F1). A retirement is final.
    function retireKey(bytes32 id, uint64 at, string calldata reason) external onlyReviewer {
        if (at > block.timestamp) revert TimeInFuture();
        Key storage k = _keys[id];
        if (k.life == Life.None) revert UnknownKey();
        if (k.life != Life.Active) revert NotActive();
        k.life = Life.Retired;
        k.retired = true;
        k.retiredAt = at;
        emit KeyRetired(id, at, msg.sender, reason);
    }

    /// Mark a key compromised from `since` (not after now; a later call may only move it
    /// earlier). Nothing it signs is taken any more; what it stated from `since` on (observed,
    /// recorded or accepted at or after `since`) is read as withdrawn (R-SIG-6a). Disputes stay
    /// open and suspensions stay until the reviewer acts.
    function markCompromised(bytes32 id, uint64 since, string calldata reason)
        external
        onlyReviewer
    {
        if (since > block.timestamp) revert TimeInFuture();
        Key storage k = _keys[id];
        if (k.life == Life.None) revert UnknownKey();
        if (k.life == Life.Compromised) {
            if (since < k.since) k.since = since;
        } else {
            k.life = Life.Compromised;
            k.since = since;
            k.compromisedAt = uint64(block.timestamp);
        }
        emit KeyCompromised(id, k.since, msg.sender, reason);
    }

    /// Clear a key's suspension; the life cycle is untouched (L3R2-F1).
    function reinstateKey(bytes32 id, string calldata reason) external onlyReviewer {
        Key storage k = _keys[id];
        if (k.life == Life.None) revert UnknownKey();
        if (!k.suspended) revert NotSuspended();
        k.suspended = false;
        k.suspendedAt = 0;
        emit KeyReinstated(id, msg.sender, reason);
    }

    /// Settle a dispute: keep one of its reports (payload digest) whose key may sign it now, or
    /// clear the fact with `keep == 0` so the source can state it again.
    function resolveFact(bytes32 fact, bytes32 keep, string calldata reason) external onlyReviewer {
        Fact storage f = _facts[fact];
        if (f.state != FactState.Disputed) revert NotDisputed();
        if (keep == bytes32(0)) {
            delete _facts[fact];
            emit FactResolved(fact, keep, msg.sender, reason);
            return;
        }
        if (!_liveMember(f.dispute, keep)) revert BadResolution();
        Rep storage r = _reps[keep];
        if (!_permits(_keys[r.key], r.observed)) revert BadResolution();
        delete f.dispute;
        f.state = FactState.Accepted;
        f.accepted = keep;
        f.acceptedAt = uint64(block.timestamp);
        emit FactResolved(fact, keep, msg.sender, reason);
    }

    // ---------------------------------------------------------------- intake

    /// The one on-chain intake (R-SIG-2..6). `payload` is the Borsh `SignedPayload` the source
    /// signed, `signature` its 65-byte EIP-191 signature. Refusals that change nothing revert;
    /// an accepted report, an equivocation and evidence joining a dispute return their outcome.
    function postReport(bytes calldata payload, bytes calldata signature)
        external
        returns (Outcome outcome, bytes32 fact)
    {
        Parsed memory p = _parse(payload);
        if (p.sequence == RESERVED_SEQUENCE) revert SequenceReserved();
        Key storage k = _keys[p.key];
        if (k.life == Life.None) revert UnknownKey();
        if (k.source != keccak256(payload[4:p.sourceEnd])) revert KeySourceMismatch();
        _requirePermits(k, p.observed);
        bytes32 digest = keccak256(abi.encodePacked("onyx-signed-source-v1", payload));
        if (_signer(digest, signature) != k.signer) revert BadSignature();
        uint256 t = block.timestamp;
        if (p.observed > t + MAX_CLOCK_SKEW_SECS) revert FromFuture();
        if (t > p.observed && t - p.observed > MAX_REPORT_AGE_SECS) revert Stale();
        if (_reps[digest].key != bytes32(0)) revert Replay();

        fact = keccak256(
            abi.encodePacked(
                "onyx-signed-fact-v1",
                payload[0:p.sourceEnd],
                payload[p.sourceEnd + 32:p.subjectEnd],
                payload[payload.length - 16:payload.length - 8]
            )
        );
        Rep memory rep = Rep({
            key: p.key,
            fact: fact,
            valueHash: keccak256(payload[p.subjectEnd:payload.length - 16]),
            reportHash: keccak256(abi.encodePacked("onyx-signed-report-v1", payload, signature)),
            observed: p.observed,
            sequence: p.sequence,
            recordedAt: uint64(t)
        });

        bytes32 prev = sequenceOf[p.key][p.sequence];
        if (prev != bytes32(0)) {
            _record(digest, rep);
            fact = _equivocateSequence(prev, digest);
            outcome = Outcome.Equivocation;
        } else {
            outcome = _factBook(digest, rep);
        }
        emit ReportRecorded(digest, p.key, rep.fact, p.sequence, rep.reportHash, outcome);
    }

    // ---------------------------------------------------------------- track records (level 5)

    /// Declare `source`'s operator (ADR-0036 amendment 3 item 5): the reviewer, once per source,
    /// before the source's first key, so no report or statement of the source predates its
    /// grouping and no committed track outcome can change with it.
    function declareOperator(string calldata source, bytes32 operator, string calldata reason)
        external
        onlyReviewer
    {
        if (bytes(source).length == 0 || operator == bytes32(0)) revert BadOperator();
        bytes32 s = keccak256(bytes(source));
        if (operatorOf[s] != bytes32(0)) revert OperatorSet();
        if (originOfSource[s] != bytes32(0)) revert SourceHasKeys();
        operatorOf[s] = operator;
        emit OperatorDeclared(s, operator, source, msg.sender, reason);
    }

    /// Append a track commitment to the caller's log `track` (ADR-0036 amendment 3 item 4).
    /// `commitment` is the Borsh `onyx_track::TrackCommitment` (`TRACK_COMMITMENT_BYTES` bytes,
    /// `prev` first, then `as_of` as u64 LE); its hash is Rust's `TrackCommitment::hash`. The
    /// log only grows and stays one chain: `prev` must be the log's latest (zero for the first),
    /// `as_of` must rise and not pass the block time, and `head` must be a past block. Anyone may
    /// keep a log; a client reads the one poster and track it pins.
    function postTrackCommitment(bytes32 track, uint64 head, bytes calldata commitment)
        external
        returns (uint64 index)
    {
        if (track == bytes32(0) || commitment.length != TRACK_COMMITMENT_BYTES) {
            revert BadTrackCommitment();
        }
        if (head >= block.number) revert TrackHeadNotPast();
        bytes32 h = keccak256(abi.encodePacked("onyx-track-commitment-v1", commitment));
        uint64 asOf = uint64(_readU64(commitment, 32));
        TrackLog storage l = _trackLogs[msg.sender][track];
        if (bytes32(commitment[0:32]) != l.latest) revert TrackNotChained();
        if (asOf <= l.asOf) revert TrackTimeNotForward();
        if (asOf > block.timestamp) revert FromFuture();
        index = l.count;
        trackCommitmentAt[msg.sender][track][index] = h;
        l.count = index + 1;
        l.asOf = asOf;
        l.latest = h;
        emit TrackCommitted(msg.sender, track, index, h, asOf, head, commitment);
    }

    /// The log's length, latest commitment hash and its `as_of` (zeros: empty). A client reads it
    /// at a finalized block: the served commitment must be `latest` (THREATS TR-R2-5).
    function trackHead(address poster, bytes32 track)
        external
        view
        returns (uint64 count, bytes32 latest, uint64 asOf)
    {
        TrackLog storage l = _trackLogs[poster][track];
        return (l.count, l.latest, l.asOf);
    }

    // ---------------------------------------------------------------- reads

    /// A key's life cycle and suspension.
    function keyState(bytes32 id)
        external
        view
        returns (
            Life life,
            bool suspended,
            address signer,
            bytes32 source,
            uint64 retiredAt,
            uint64 since
        )
    {
        Key storage k = _keys[id];
        return (k.life, k.suspended, k.signer, k.source, k.retiredAt, k.since);
    }

    /// The record of a payload digest (zero key: never recorded).
    function report(bytes32 digest) external view returns (Rep memory) {
        return _reps[digest];
    }

    /// Where a fact stands, after reading withdrawals: the accepted report hash (zero unless
    /// accepted), when it was accepted, and the dispute's report hashes (empty unless disputed).
    function factView(bytes32 fact)
        external
        view
        returns (
            FactState state,
            bytes32 acceptedReport,
            uint64 acceptedAt,
            bytes32[] memory dispute
        )
    {
        Fact storage f = _facts[fact];
        state = _state(fact);
        if (state == FactState.Accepted) {
            acceptedReport = _reps[f.accepted].reportHash;
            acceptedAt = f.acceptedAt;
        } else if (state == FactState.Disputed) {
            uint256 n;
            bytes32[] memory tmp = new bytes32[](f.dispute.length);
            for (uint256 i = 0; i < f.dispute.length; i++) {
                if (_memberLive(f.dispute[i])) tmp[n++] = _reps[f.dispute[i]].reportHash;
            }
            dispute = new bytes32[](n);
            for (uint256 i = 0; i < n; i++) {
                dispute[i] = tmp[i];
            }
        }
    }

    /// The only value a consumer may settle on (R-SIG-6, L3R2-F7): the accepted report of `fact`
    /// once it has stood for `DISPUTE_WINDOW_SECS` of block time. Read it at a finalized block.
    function settleable(bytes32 fact)
        external
        view
        returns (bool ok, bytes32 reportHash, bytes32 valueHash)
    {
        if (_state(fact) != FactState.Accepted) return (false, bytes32(0), bytes32(0));
        Fact storage f = _facts[fact];
        if (block.timestamp < uint256(f.acceptedAt) + DISPUTE_WINDOW_SECS) {
            return (false, bytes32(0), bytes32(0));
        }
        Rep storage r = _reps[f.accepted];
        return (true, r.reportHash, r.valueHash);
    }

    // ---------------------------------------------------------------- internals

    function _record(bytes32 digest, Rep memory rep) internal {
        _reps[digest] = rep;
        if (sequenceOf[rep.key][rep.sequence] == bytes32(0)) {
            sequenceOf[rep.key][rep.sequence] = digest;
        }
    }

    /// The fact book (R-SIG-6) for a report under an unused sequence.
    function _factBook(bytes32 digest, Rep memory rep) internal returns (Outcome) {
        bytes32 fact = rep.fact;
        Fact storage f = _facts[fact];
        FactState st = _state(fact);
        if (st == FactState.None) {
            _record(digest, rep);
            delete f.dispute;
            f.state = FactState.Accepted;
            f.accepted = digest;
            f.acceptedAt = rep.recordedAt;
            return Outcome.Accepted;
        }
        if (st == FactState.Accepted) {
            bytes32 a = f.accepted;
            if (_reps[a].valueHash == rep.valueHash) revert AlreadyStated();
            _record(digest, rep);
            _suspend(_reps[a].key, fact);
            emit Equivocated(fact, a, digest, EquivocationKind.Fact);
            _suspend(rep.key, fact);
            emit Withdrawn(a);
            f.state = FactState.Disputed;
            f.accepted = bytes32(0);
            f.acceptedAt = 0;
            f.dispute.push(a);
            f.dispute.push(digest);
            return Outcome.Equivocation;
        }
        // Disputed: at most one report per key.
        bytes32 own = _ownMember(f.dispute, rep.key);
        if (own != bytes32(0)) {
            if (_reps[own].valueHash == rep.valueHash) revert AlreadyStated();
            _record(digest, rep);
            emit Equivocated(fact, own, digest, EquivocationKind.Fact);
            _suspend(rep.key, fact);
            return Outcome.Equivocation;
        }
        _record(digest, rep);
        bytes32 differs = _differingMember(f.dispute, rep.valueHash);
        f.dispute.push(digest);
        if (differs != bytes32(0)) {
            emit Equivocated(fact, differs, digest, EquivocationKind.Fact);
            _suspend(rep.key, fact);
            return Outcome.Equivocation;
        }
        return Outcome.FactDisputed;
    }

    /// One key, one sequence, two payloads (R-SIG-6, last bullet; L3R5-F2). Returns the fact
    /// the contradiction is about.
    function _equivocateSequence(bytes32 prev, bytes32 second) internal returns (bytes32 fact) {
        Rep storage first = _reps[prev];
        Rep storage sec = _reps[second];
        bytes32 firstFact = first.fact;
        bytes32 secondFact = sec.fact;
        fact = secondFact;
        emit Equivocated(firstFact, prev, second, EquivocationKind.Sequence);
        bool handled;
        if (_state(firstFact) == FactState.Accepted && _facts[firstFact].accepted == prev) {
            Fact storage f = _facts[firstFact];
            delete f.dispute;
            f.state = FactState.Disputed;
            f.accepted = bytes32(0);
            f.acceptedAt = 0;
            f.dispute.push(prev);
            if (secondFact == firstFact) {
                f.dispute.push(second);
                handled = true;
            }
            fact = firstFact;
            emit Withdrawn(prev);
        }
        _suspend(
            sec.key,
            keccak256(
                abi.encodePacked("onyx-signed-sequence-equivocation-v1", sec.key, sec.sequence)
            )
        );
        if (!handled) _contradict(secondFact, second);
    }

    /// The fact book for a recorded report that is never accepted (the second payload of a
    /// sequence equivocation).
    function _contradict(bytes32 fact, bytes32 digest) internal {
        Rep storage rep = _reps[digest];
        Fact storage f = _facts[fact];
        FactState st = _state(fact);
        if (st == FactState.Accepted) {
            bytes32 a = f.accepted;
            if (_reps[a].valueHash == rep.valueHash) return;
            _suspend(_reps[a].key, fact);
            _suspend(rep.key, fact);
            emit Equivocated(fact, a, digest, EquivocationKind.Fact);
            emit Withdrawn(a);
            f.state = FactState.Disputed;
            f.accepted = bytes32(0);
            f.acceptedAt = 0;
            f.dispute.push(a);
            f.dispute.push(digest);
        } else if (st == FactState.Disputed) {
            if (_ownMember(f.dispute, rep.key) != bytes32(0)) return;
            bytes32 differs = _differingMember(f.dispute, rep.valueHash);
            f.dispute.push(digest);
            if (differs != bytes32(0)) {
                emit Equivocated(fact, differs, digest, EquivocationKind.Fact);
                _suspend(rep.key, fact);
            }
        }
    }

    function _suspend(bytes32 id, bytes32 cause) internal {
        Key storage k = _keys[id];
        if (!k.suspended) {
            k.suspended = true;
            k.suspendedAt = uint64(block.timestamp);
        }
        emit Suspended(id, cause);
    }

    /// The fact's state after reading withdrawals: an accepted report whose key no longer covers
    /// it reads as no value. A dispute never ends without the reviewer (R-SIG-6c).
    function _state(bytes32 fact) internal view returns (FactState) {
        Fact storage f = _facts[fact];
        if (f.state == FactState.Accepted && !_acceptedLive(f.accepted, f.acceptedAt)) {
            return FactState.None;
        }
        return f.state;
    }

    /// An accepted report still counts unless its key was retired at or before its observation
    /// time (L3R5-F1) or compromised from a point at or before its observation or acceptance
    /// (R-SIG-6a).
    function _acceptedLive(bytes32 digest, uint64 acceptedAt) internal view returns (bool) {
        Rep storage r = _reps[digest];
        Key storage k = _keys[r.key];
        if (k.retired && r.observed >= k.retiredAt) return false;
        if (k.life == Life.Compromised && (r.observed >= k.since || acceptedAt >= k.since)) {
            return false;
        }
        return true;
    }

    /// A dispute's report stays in it unless its key was compromised from a point at or before
    /// its observation or recording (R-SIG-6a).
    function _memberLive(bytes32 digest) internal view returns (bool) {
        Rep storage r = _reps[digest];
        Key storage k = _keys[r.key];
        return !(k.life == Life.Compromised && (r.observed >= k.since || r.recordedAt >= k.since));
    }

    function _liveMember(bytes32[] storage list, bytes32 digest) internal view returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == digest) return _memberLive(digest);
        }
        return false;
    }

    function _ownMember(bytes32[] storage list, bytes32 id) internal view returns (bytes32) {
        for (uint256 i = 0; i < list.length; i++) {
            bytes32 d = list[i];
            if (_reps[d].key == id && _memberLive(d)) return d;
        }
        return bytes32(0);
    }

    function _differingMember(bytes32[] storage list, bytes32 valueHash)
        internal
        view
        returns (bytes32)
    {
        for (uint256 i = 0; i < list.length; i++) {
            bytes32 d = list[i];
            if (_reps[d].valueHash != valueHash && _memberLive(d)) return d;
        }
        return bytes32(0);
    }

    /// R-SIG-3: may this key sign a report observed at `observed`, posted now? The one rule for
    /// the intake (`_requirePermits`) and for resolutions.
    function _permits(Key storage k, uint64 observed) internal view returns (bool) {
        return _permitError(k, observed) == bytes4(0);
    }

    function _requirePermits(Key storage k, uint64 observed) internal view {
        bytes4 err = _permitError(k, observed);
        if (err == KeyCompromisedError.selector) revert KeyCompromisedError();
        if (err == KeyRetiredError.selector) revert KeyRetiredError();
        if (err == KeySuspended.selector) revert KeySuspended();
    }

    /// Why `k` may not sign a report observed at `observed` now, or zero: never once compromised;
    /// once retired at `at`, only observed in `[at - grace, at)` and posted before `at + grace`;
    /// never while suspended.
    function _permitError(Key storage k, uint64 observed) internal view returns (bytes4) {
        if (k.life == Life.Compromised) return KeyCompromisedError.selector;
        if (k.life == Life.Retired) {
            uint256 at = k.retiredAt;
            uint256 lo = at > RETIRED_GRACE_SECS ? at - RETIRED_GRACE_SECS : 0;
            if (!(observed < at && observed >= lo && block.timestamp < at + RETIRED_GRACE_SECS)) {
                return KeyRetiredError.selector;
            }
        } else if (k.life != Life.Active) {
            return UnknownKey.selector;
        }
        if (k.suspended) return KeySuspended.selector;
        return bytes4(0);
    }

    /// The signer of `onyx-signed-source-v1:0x<digest>` (EIP-191, canonical form only).
    function _signer(bytes32 digest, bytes calldata signature) internal pure returns (address) {
        bytes memory message = abi.encodePacked("onyx-signed-source-v1:", OnyxSig.hex32(digest));
        return OnyxSig.recover(OnyxSig.eip191Digest(message), signature);
    }

    /// Parse the Borsh `SignedPayload { source: String, key: [u8; 32], metric: String,
    /// subject: Subject, value: Value, observed_unix: u64, sequence: u64 }`. Strings are a u32 LE
    /// length and bytes; `Subject` is a tag 0..=3 and a string; `Value` is a tag and exactly its
    /// variant's bytes (`Value::validate`'s bounds). Anything else, or a zero key or observation
    /// time, is `BadPayload`. The value is the bytes between the subject and the two trailing
    /// u64s, checked to its variant's length, so the layout has one reading.
    function _parse(bytes calldata b) internal pure returns (Parsed memory p) {
        uint256 n = b.length;
        if (n < 4) revert BadPayload();
        uint256 o = 4 + _readU32(b, 0);
        p.sourceEnd = o;
        if (o + 32 + 4 > n) revert BadPayload();
        p.key = bytes32(b[o:o + 32]);
        o += 32;
        o = o + 4 + _readU32(b, o);
        if (o + 5 > n) revert BadPayload();
        if (uint8(b[o]) > 3) revert BadPayload();
        o = o + 5 + _readU32(b, o + 1);
        p.subjectEnd = o;
        // Value tag + at least one byte, then two u64s.
        if (o + 2 + 16 > n) revert BadPayload();
        uint256 end = n - 16;
        uint256 len = end - o - 1;
        uint8 tag = uint8(b[o]);
        if (tag == 0 || tag == 1) {
            if (len != 16) revert BadPayload();
        } else if (tag == 2) {
            uint8 scale = uint8(b[end - 1]);
            if (len != 17 || scale == 0 || scale > MAX_FIXED_SCALE) revert BadPayload();
        } else if (tag == 3) {
            if (len != 1 || uint8(b[o + 1]) > 1) revert BadPayload();
        } else if (tag == 4) {
            if (len < 4 || _readU32(b, o + 1) != len - 4 || len - 4 > MAX_VALUE_BYTES) {
                revert BadPayload();
            }
        } else {
            revert BadPayload();
        }
        p.observed = uint64(_readU64(b, end));
        p.sequence = uint64(_readU64(b, end + 8));
        if (p.key == bytes32(0) || p.observed == 0) revert BadPayload();
    }

    function _readU32(bytes calldata b, uint256 o) internal pure returns (uint256 v) {
        if (o + 4 > b.length) revert BadPayload();
        for (uint256 i = 0; i < 4; i++) {
            v |= uint256(uint8(b[o + i])) << (8 * i);
        }
    }

    function _readU64(bytes calldata b, uint256 o) internal pure returns (uint256 v) {
        for (uint256 i = 0; i < 8; i++) {
            v |= uint256(uint8(b[o + i])) << (8 * i);
        }
    }

    function _u32le(uint256 v) internal pure returns (bytes4) {
        return bytes4(
            uint32(
                ((v & 0xff) << 24) | (((v >> 8) & 0xff) << 16) | (((v >> 16) & 0xff) << 8)
                    | ((v >> 24) & 0xff)
            )
        );
    }
}
