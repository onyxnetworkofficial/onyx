// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxLedger} from "./OnyxLedger.sol";
import {OnyxCommit} from "./OnyxCommit.sol";
import {OnyxSig} from "./OnyxSig.sol";

/// @notice What `OnyxRegistry` reads from the settlement (ADR-0030). `shardState`, `isFinal` and
/// `verifyRow` are served by `OnyxSettlementExt` at the settlement's address (ADR-0024).
interface IRegistryLedger {
    function coordinator() external view returns (address);
    /// Bumped on every coordinator change (review REVLOG-R2 LOW-3); constant for a ledger whose
    /// coordinator is immutable.
    function coordinatorEpoch() external view returns (uint64);
    function isFinal(uint64 id, bytes32 shard) external view returns (bool);
    function shardState(uint64 id, bytes32 shard)
        external
        view
        returns (
            OnyxLedger.ShardStatus status,
            bool closed,
            uint64 finalizedAt,
            bytes32 finalRoot,
            bytes32 finalBinding
        );
    function verifyRow(
        uint64 id,
        bytes32 shard,
        bytes calldata row,
        uint64 index,
        uint64 leafCount,
        bytes32[] calldata siblings
    ) external view returns (bool);
}

/// @title OnyxRegistry
/// @notice The on-chain registry of what each metric may be computed from, and the level a
/// consumer may rely on (ADR-0030). What the chain ENFORCES and what it only RECORDS as the
/// coordinator's word is listed per function; ADR-0030 items 3-5 give the full split.
/// - One template per metric, append-only, set by the owner (never the coordinator): a recipe
///   id for a level-1 metric, a feed template id (`onyx_core::FeedTemplate::id`) for a level-2
///   metric. One level per label set, append-only, set by the owner.
/// - `attestShard`: the coordinator posts the shard-id PREIMAGE before the open; the chain
///   recomputes the shard id from it, so the recipe and label set checked are the ones the
///   shard actually commits to (REG-R1-F1). The fuel figures and the level are attested; the
///   level is floored on chain by the label set's registered level.
/// - `recordObservation`: the fact key is computed on chain from the metric's REGISTERED
///   template, so no other template can fill it (REG-R1-F2). The spec id is recorded, not
///   derived: a consumer compares it with its own instantiation of the template.
/// The owner is a single key on testnet and may not be the coordinator (REG-R1-F3); a multisig or
/// timelock is a TODO (REG-1).
contract OnyxRegistry {
    /// How a metric is computed.
    enum Kind {
        None,
        Recipe,
        /// Level 2, one fact per (subject, window end).
        Feed,
        /// Level 2, one fact per dated subject whatever the window (L2SRC-R2-F2).
        DatedFeed
    }

    struct Metric {
        Kind kind;
        bytes32 template;
        uint64 registeredAt;
    }

    /// The fields `OnyxCommit.shardId` hashes (root and leaf count excluded): the shard id is
    /// recomputed from them on chain.
    struct ShardPreimage {
        bytes32 recipeId;
        bytes32 runtimeHash;
        bytes32 labelSetHash;
        bytes32 sourceHash;
        bytes32 schemaId;
        bytes32 rightsHash;
        bytes32 paramsHash;
        bytes32 registryRoot;
    }

    struct Attestation {
        bytes32 metric;
        bytes32 recipe;
        bytes32 labelSetHash;
        uint64 fuelUsed;
        uint64 fuelLimit;
        uint8 level;
        uint64 attestedAt;
    }

    struct Observation {
        bytes32 metric;
        bytes32 template;
        bytes32 subjectKey;
        uint64 windowEnd;
        bytes32 spec;
        bytes32 result;
        uint64 recordedAt;
    }

    /// Proof levels a shard may carry (`onyx_core::ProofLevel::number`); 6 (an unstaked claim)
    /// is never a shard's level (REG-R1-F7).
    uint8 public constant MIN_LEVEL = 1;
    uint8 public constant MAX_SHARD_LEVEL = 5;
    /// The level a recorded observation carries (`ProofLevel::ReObserved`).
    uint8 public constant OBSERVED_LEVEL = 2;
    /// Mirror of `onyx_coordinator::recompute::PREFLIGHT_MAX_FUEL_PPM`.
    uint64 public constant PREFLIGHT_MAX_FUEL_PPM = 500_000;

    address public immutable owner;
    IRegistryLedger public immutable ledger;

    mapping(bytes32 => Metric) public metrics;
    /// Label set hash (`LabelSetRef::hash`) -> its registered proof level; 0 = unregistered.
    mapping(bytes32 => uint8) public labelSetLevel;
    /// Recipe id -> the best level any shard running it may carry (AUD-2R-F3), for inputs the
    /// rerun does not check (a pinned third-party mapping table); 0 = no floor. Append-only.
    /// Keyed by recipe, not metric, so a second metric registered with the same recipe cannot
    /// escape it (REG-F-F2); applied again on every read (`levelOf`, REG-F-F1).
    mapping(bytes32 => uint8) public recipeMinLevel;
    mapping(uint64 => mapping(bytes32 => Attestation)) internal _attestations;
    /// Fact key (`factKeyOf`) -> its one recorded result.
    mapping(bytes32 => Observation) internal _facts;
    /// Fact key -> attempt -> its announced observer draw (L2RND-R1-F1/F2).
    mapping(bytes32 => mapping(uint32 => Draw)) internal _draws;
    /// Fact key -> draws announced so far (attempts 0 .. drawCount-1, strictly sequential).
    mapping(bytes32 => uint32) public drawCount;
    /// Subject draw key (`subjectDrawKey(metric, subject)`) -> draws announced for the subject
    /// that have not ended with a recorded value (L2RND-R2-F1/F2). Covers every window of a
    /// fixed feed and every attempt of every fact of the subject.
    mapping(bytes32 => uint32) public subjectStrikes;
    /// Subject draw key -> reopens the owner recorded; each grants `MAX_DRAWS` more strikes.
    mapping(bytes32 => uint32) public subjectReopens;
    /// The key that signs reveal receipts for draws announced from now on (ADR-0027 amendment
    /// 8, SIG-R1-F1). The coordinator's own hot key, never its transaction key. Bound to the
    /// coordinator that set it and the ledger's `coordinatorEpoch` then (`_receiptSignerBy`,
    /// `_receiptSignerEpoch`): after any coordinator change it reads zero until the coordinator
    /// sets a key again (review REVLOG LOW-2), even when a former coordinator returns (A to B to
    /// A, review REVLOG-R2 LOW-3), so an old key never signs for a new draw.
    address internal _receiptSigner;
    address internal _receiptSignerBy;
    uint64 internal _receiptSignerEpoch;
    /// Fact key -> attempt -> the receipt signer when the draw was announced: receipts for a
    /// draw are checked against it, so a later rotation never voids them.
    mapping(bytes32 => mapping(uint32 => address)) public drawReceiptSigner;
    /// `logKey(fact, attempt, observer, round)` -> the observer's commit posted on chain, with
    /// its signature: a coordinator reads it with `commitLogOf` (one `eth_call`), never from
    /// event logs (review REVLOG-R2 HIGH-1: RPCs cap `eth_getLogs` ranges and results).
    mapping(bytes32 => LoggedCommit) internal _commitLog;
    /// `logKey(fact, attempt, observer, round)` -> the commit a posted receipt names: the
    /// receipt signer said it holds a reveal opening that commit.
    mapping(bytes32 => bytes32) public receiptLog;
    /// The coordinator's reveal key (ADR-0027 amendment 10): observers encrypt their sealed
    /// reveal posts to it. Bound to the coordinator and epoch that set it, like the receipt
    /// signer, and snapshotted per draw (`drawRevealKey`).
    RevealKey internal _revealKey;
    address internal _revealKeyBy;
    uint64 internal _revealKeyEpoch;
    /// ADR-0027 amendment 14 (ROADMAP C51): once true, never false again. Every draw announced
    /// from then on is TIME-LOCKED (`drawTimeLocked`): it needs a reveal key, its sealed posts
    /// carry a drand time-lock and its far-date copy (more than `TIME_LOCK_BYTES + FAR_COPY_BYTES`,
    /// ROADMAP C52), and no round of a draw that is not
    /// time-locked is recorded any more, so a coordinator cannot choose a draw whose posts it
    /// opens before the lock round (THREATS C11-R2-1 class, C4-7). Packed with the key's binder.
    bool public timeLockRequired;
    /// Fact key -> attempt -> the reveal key when the draw was announced (zero: none, the draw
    /// takes no sealed post).
    mapping(bytes32 => mapping(uint32 => RevealKey)) public drawRevealKey;
    /// Fact key -> attempt -> whether the draw was announced with `timeLockRequired` on
    /// (snapshotted by `announceDraw`, like the reveal key; never changed afterwards).
    mapping(bytes32 => mapping(uint32 => bool)) public drawTimeLocked;
    /// `logKey(fact, attempt, observer, round)` -> the observer's sealed reveal post.
    mapping(bytes32 => SealedPost) internal _sealedLog;
    /// Fact key -> attempt -> `attemptDigest` of the round the coordinator recorded for that
    /// draw (REVLOG-5): which observers revealed, were dismissed or not, and the outcome.
    mapping(bytes32 => mapping(uint32 => bytes32)) public attemptRecord;

    /// An uncompressed secp256k1 public key.
    struct RevealKey {
        bytes32 x;
        bytes32 y;
    }

    /// A sealed reveal as posted: the ephemeral public key, the block time, the key proof
    /// (`addr(T) ‖ z`, kept so the coordinator re-checks it before it dismisses: review C4-R3-2)
    /// and the ciphertext.
    struct SealedPost {
        bytes32 ex;
        bytes32 ey;
        uint64 postedAt;
        address keyT;
        bytes32 keyZ;
        bytes ct;
    }

    /// An observer's commit as posted on chain, with the block time it landed and the observer's
    /// signature (`r ‖ s ‖ v`).
    struct LoggedCommit {
        bytes32 commit;
        uint64 postedAt;
        uint8 v;
        bytes32 r;
        bytes32 s;
    }

    /// A level-2 round's observer draw, announced before its beacon block exists (ADR-0027
    /// amendment 6): the draw commitment (`drawCommit`: pool snapshot, spec, config — every
    /// input of the draw but the beacon, L2RND-R3-F1/F2) and beacon height are fixed here first;
    /// the beacon is then `blockhash(beaconHeight)`, written by anyone with `fixBeacon` while
    /// the block is within the EVM's 256-block window.
    struct Draw {
        bytes32 commit;
        uint64 beaconHeight;
        uint64 announcedAt;
        bytes32 beacon;
    }

    /// Most draws a fact may take (attempts 0..63); mirrors `MAX_DRAW_ATTEMPTS`.
    uint32 public constant MAX_DRAW_ATTEMPTS = 64;
    /// Most draws a (metric, subject) may hold that did not end with a recorded value, before
    /// the owner records a reopen (L2RND-R2-F1): equal to the non-value cap
    /// (`MAX_NON_VALUE_ROUNDS`). Mirrors `MAX_SUBJECT_DRAWS`.
    uint32 public constant MAX_DRAWS = 3;
    /// Most ciphertext bytes of one sealed reveal post: an exact answer of the largest value
    /// (1,024 bytes) sealed, time-locked and with its far-date copy, 128 + 144 + 142 + 1,024
    /// (Rust `sealed::MAX_SEALED_BYTES`; review C4-R2-2, ROADMAP C51 and C52, THREATS C12-7,
    /// C52-6). Every reveal of a feed is padded to one length (`sealed::sealed_len`; a numeric one
    /// is 154, 426 time-locked).
    uint256 public constant MAX_SEALED_BYTES = 1438;
    /// Bytes of the drand time-lock that leads a time-locked post (`U` 96 ‖ `V` 16 ‖ `W` 16,
    /// Rust `timelock::TLOCK_LEN`).
    uint256 public constant TIME_LOCK_BYTES = 128;
    /// Bytes of the far-date unseal copy that follows it (ADR-0027 amendment 15, ROADMAP C52;
    /// Rust `sealed::FAR_COPY_BYTES`): the body key time-locked to the fact's far round (128) and
    /// wrapped for the coordinator (16). A post of a time-locked draw must be longer than both.
    uint256 public constant FAR_COPY_BYTES = 144;
    /// secp256k1 group order.
    uint256 internal constant N =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
    /// secp256k1 field prime.
    uint256 internal constant P =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F;

    event MetricRegistered(bytes32 indexed metric, Kind kind, bytes32 template);
    event LabelSetRegistered(bytes32 indexed labelSetHash, uint8 level);
    event RecipeMinLevelRegistered(bytes32 indexed recipe, uint8 level);
    event ShardAttested(
        uint64 indexed round,
        bytes32 indexed shard,
        bytes32 metric,
        bytes32 recipe,
        bytes32 labelSetHash,
        uint64 fuelUsed,
        uint64 fuelLimit,
        uint8 level
    );
    event DrawAnnounced(bytes32 indexed fact, uint32 attempt, bytes32 commit, uint64 beaconHeight);
    event BeaconFixed(bytes32 indexed fact, uint32 attempt, bytes32 beacon);
    event DrawsReopened(bytes32 indexed metric, bytes32 indexed subjectKey, uint32 reopens);
    event ReceiptSignerSet(address signer);
    /// For indexers only: the coordinator reads the commit and its signature from state
    /// (`commitLogOf`).
    event CommitPosted(
        bytes32 indexed fact,
        uint32 attempt,
        address indexed observer,
        bytes32 round,
        bytes32 commit
    );
    event ReceiptPosted(
        bytes32 indexed fact,
        uint32 attempt,
        address indexed observer,
        bytes32 round,
        bytes32 commit
    );
    event RevealKeySet(bytes32 x, bytes32 y);
    event TimeLockRequired(address by);
    /// For indexers only: the coordinator reads the post from state (`sealedRevealOf`).
    event SealedRevealPosted(
        bytes32 indexed fact,
        uint32 attempt,
        address indexed observer,
        bytes32 round,
        bytes32 ciphertextHash
    );
    event AttemptRecorded(bytes32 indexed fact, uint32 attempt, bytes32 round, bytes32 digest);
    event ObservationRecorded(
        bytes32 indexed fact,
        bytes32 indexed metric,
        bytes32 template,
        bytes32 subjectKey,
        uint64 windowEnd,
        bytes32 spec,
        bytes32 result
    );

    error NotOwner();
    error NotCoordinator();
    error OwnerIsCoordinator();
    error ZeroValue();
    error AlreadyRegistered();
    error WrongTemplate();
    error BadLevel();
    error LabelSetUnregistered();
    error BelowLabelLevel();
    error BelowMetricLevel();
    error OverBudget();
    error AlreadyAttested();
    error ShardAlreadyOpen();
    error AlreadyRecorded();
    error BadRuntime();
    error BadAttempt();
    error BeaconNotFuture();
    error BeaconUnavailable();
    error BeaconAlreadyFixed();
    error NoDraw();
    /// The recorded spec is not the one the fact's latest draw committed to (L2RND-R3).
    error DrawMismatch();
    error DrawsExhausted();
    error NoReceiptSigner();
    error BadSignature();
    error AlreadyPosted();
    error NotOnCurve();
    error NoRevealKey();
    error BadKeyProof();
    error BadCiphertext();
    error BadWindow();
    error TooEarly();
    error IncompleteTranscript();
    error NotTimeLocked();

    constructor(address owner_, IRegistryLedger ledger_) {
        if (owner_ == address(0) || address(ledger_) == address(0)) revert ZeroValue();
        // The coordinator must not choose the templates it is checked against (REG-R1-F3).
        if (owner_ == ledger_.coordinator()) revert OwnerIsCoordinator();
        owner = owner_;
        ledger = ledger_;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier onlyCoordinator() {
        if (msg.sender != ledger.coordinator()) revert NotCoordinator();
        _;
    }

    // ------------------------------------------------------------------ pure keys

    /// @notice `keccak256(utf8(metric id))`, mirrored by `onyx_core::onchain::metric_key`.
    function metricKey(string calldata metricId) external pure returns (bytes32) {
        return keccak256(bytes(metricId));
    }

    /// @notice The shard id `OnyxCommit.shardId` gives for `p` (mirrored by
    /// `onyx_core::onchain::ShardPreimage::shard_id` and `ShardSpec::shard_id`).
    function shardIdOf(ShardPreimage calldata p) public pure returns (bytes32) {
        OnyxCommit.CommitmentFields memory c = OnyxCommit.CommitmentFields({
            root: bytes32(0),
            leafCount: 0,
            recipeId: p.recipeId,
            runtimeHash: p.runtimeHash,
            labelSetHash: p.labelSetHash,
            sourceHash: p.sourceHash,
            schemaId: p.schemaId,
            rightsHash: p.rightsHash
        });
        return OnyxCommit.shardId(OnyxCommit.shardFieldsHash(c), p.paramsHash, p.registryRoot);
    }

    /// @notice `keccak256("onyx-registry-fact-v1" ‖ metric ‖ template ‖ subjectKey ‖
    /// windowEnd (8 bytes BE, 0 when dated))`, mirrored by `onyx_core::onchain::registry_fact_key`.
    function factKeyOf(
        bytes32 metric,
        bytes32 template,
        bytes32 subjectKey,
        uint64 windowEnd,
        bool dated
    ) public pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-registry-fact-v1", metric, template, subjectKey, dated ? uint64(0) : windowEnd
            )
        );
    }

    /// @notice `RuntimeProfile.fuel_limit` read from the profile's Borsh encoding
    /// (`abi_version u32 ‖ host string (u32 length ‖ bytes) ‖ fuel_limit u64 ‖
    /// max_memory_bytes u64 ‖ max_output_bytes u32 ‖ max_call_depth u32`, little-endian), the
    /// bytes whose keccak is the shard's `runtimeHash` (REG-R2-F1). Reverts on any other length.
    function runtimeFuelLimit(bytes calldata runtime) public pure returns (uint64) {
        if (runtime.length < 8) revert BadRuntime();
        uint256 hostLen = _le(runtime, 4, 4);
        if (runtime.length != 8 + hostLen + 24) revert BadRuntime();
        return uint64(_le(runtime, 8 + hostLen, 8));
    }

    /// Little-endian unsigned integer of `n` bytes of `b` at `at`.
    function _le(bytes calldata b, uint256 at, uint256 n) private pure returns (uint256 v) {
        for (uint256 i = 0; i < n; i++) {
            v |= uint256(uint8(b[at + i])) << (8 * i);
        }
    }

    /// @notice True when a preflight that used `fuelUsed` of `fuelLimit` passes (<= 50%).
    function withinBudget(uint64 fuelUsed, uint64 fuelLimit) public pure returns (bool) {
        return fuelLimit != 0
            && uint256(fuelUsed) * 1_000_000 <= uint256(fuelLimit) * PREFLIGHT_MAX_FUEL_PPM;
    }

    // ------------------------------------------------------------------ owner

    /// @notice Register `metric`'s one template and, for a recipe metric, its recipe's minimum
    /// level in the same transaction (REG-F-F1: no attestation can land between the two).
    /// `minLevel` 0 = no floor; a feed metric must pass 0. Append-only: a registered metric never
    /// changes, and a recipe's floor, once set, can only be restated equal.
    function registerMetric(bytes32 metric, Kind kind, bytes32 template, uint8 minLevel)
        external
        onlyOwner
    {
        if (metric == bytes32(0) || template == bytes32(0) || kind == Kind.None) {
            revert ZeroValue();
        }
        Metric storage m = metrics[metric];
        if (m.kind != Kind.None) revert AlreadyRegistered();
        if (minLevel != 0) {
            if (kind != Kind.Recipe) revert BadLevel();
            _setRecipeFloor(template, minLevel);
        }
        m.kind = kind;
        m.template = template;
        m.registeredAt = uint64(block.number);
        emit MetricRegistered(metric, kind, template);
    }

    /// @notice Register a label set's proof level (1..5). Append-only. A labelled shard's level is
    /// never better than this (`attestShard`).
    function registerLabelSet(bytes32 labelSetHash, uint8 level) external onlyOwner {
        if (labelSetHash == bytes32(0)) revert ZeroValue();
        if (level < MIN_LEVEL || level > MAX_SHARD_LEVEL) revert BadLevel();
        if (labelSetLevel[labelSetHash] != 0) revert AlreadyRegistered();
        labelSetLevel[labelSetHash] = level;
        emit LabelSetRegistered(labelSetHash, level);
    }

    /// @notice Set the floor of an already registered RECIPE metric's recipe (for a metric
    /// registered without one). Append-only; refuses feed metrics (their level is fixed at 2).
    /// `levelOf` applies the floor to shards attested before it, too (REG-F-F1).
    function registerMetricMinLevel(bytes32 metric, uint8 level) external onlyOwner {
        Metric storage m = metrics[metric];
        if (m.kind != Kind.Recipe) revert WrongTemplate();
        _setRecipeFloor(m.template, level);
    }

    /// The floor of `metric`'s recipe; 0 for none or a feed metric.
    function metricMinLevel(bytes32 metric) external view returns (uint8) {
        Metric storage m = metrics[metric];
        return m.kind == Kind.Recipe ? recipeMinLevel[m.template] : 0;
    }

    function _setRecipeFloor(bytes32 recipe, uint8 level) internal {
        if (level < MIN_LEVEL || level > MAX_SHARD_LEVEL) revert BadLevel();
        uint8 have = recipeMinLevel[recipe];
        if (have == level) return;
        if (have != 0) revert AlreadyRegistered();
        recipeMinLevel[recipe] = level;
        emit RecipeMinLevelRegistered(recipe, level);
    }

    // ------------------------------------------------------------------ level 1 shards

    /// @notice The coordinator attests its preflight of a shard before opening it.
    /// ENFORCED: the shard id is recomputed from `p`; `p.recipeId` is `metric`'s registered recipe;
    /// `runtime` hashes to `p.runtimeHash` and the fuel limit is read from it (REG-R2-F1), so
    /// `fuelUsed` <= 50% of the limit the shard commits to; a labelled shard's label set is
    /// registered and `level` is no better than its level; `level` is no better than the metric's
    /// recipe's registered minimum level (AUD-2R-F3, keyed by recipe: REG-F-F2); 1 <= level <= 5; posted before the shard opens, once.
    /// ATTESTED: `fuelUsed` (no rerun on chain), and `level` above its floor; an unlabelled
    /// shard has no floor (labels moved into params or the source are attested, REG-R2-F4).
    function attestShard(
        uint64 round,
        ShardPreimage calldata p,
        bytes calldata runtime,
        bytes32 metric,
        uint64 fuelUsed,
        uint8 level
    ) external onlyCoordinator returns (bytes32 shard) {
        shard = shardIdOf(p);
        if (keccak256(runtime) != p.runtimeHash) revert BadRuntime();
        uint64 fuelLimit = runtimeFuelLimit(runtime);
        Metric storage m = metrics[metric];
        if (m.kind != Kind.Recipe || m.template != p.recipeId) revert WrongTemplate();
        if (level < MIN_LEVEL || level > MAX_SHARD_LEVEL) revert BadLevel();
        if (p.labelSetHash != bytes32(0)) {
            uint8 floor = labelSetLevel[p.labelSetHash];
            if (floor == 0) revert LabelSetUnregistered();
            if (level < floor) revert BelowLabelLevel();
        }
        uint8 recipeFloor = recipeMinLevel[p.recipeId];
        if (recipeFloor != 0 && level < recipeFloor) revert BelowMetricLevel();
        if (!withinBudget(fuelUsed, fuelLimit)) revert OverBudget();
        Attestation storage a = _attestations[round][shard];
        if (a.level != 0) revert AlreadyAttested();
        (OnyxLedger.ShardStatus status,,,,) = ledger.shardState(round, shard);
        if (status != OnyxLedger.ShardStatus.None) revert ShardAlreadyOpen();
        a.metric = metric;
        a.recipe = p.recipeId;
        a.labelSetHash = p.labelSetHash;
        a.fuelUsed = fuelUsed;
        a.fuelLimit = fuelLimit;
        a.level = level;
        a.attestedAt = uint64(block.number);
        emit ShardAttested(
            round, shard, metric, p.recipeId, p.labelSetHash, fuelUsed, fuelLimit, level
        );
    }

    /// @notice The attestation of (round, shard); `level == 0` when none.
    function attestationOf(uint64 round, bytes32 shard) external view returns (Attestation memory) {
        return _attestations[round][shard];
    }

    /// @notice The level a consumer may rely on: for a final attested shard, the attested level
    /// floored at its recipe's current minimum (REG-F-F1: a floor set after the attestation still
    /// applies); else 0.
    function levelOf(uint64 round, bytes32 shard) public view returns (uint8) {
        Attestation storage a = _attestations[round][shard];
        uint8 level = a.level;
        if (level == 0 || !ledger.isFinal(round, shard)) return 0;
        uint8 floor = recipeMinLevel[a.recipe];
        return floor > level ? floor : level;
    }

    /// @notice `verifyRow` bound to the attestation: `(false, 0, 0)` for a shard with no
    /// attestation or not final; otherwise the row check, the level and the metric.
    function verifyRowLeveled(
        uint64 round,
        bytes32 shard,
        bytes calldata row,
        uint64 index,
        uint64 leafCount,
        bytes32[] calldata siblings
    ) external view returns (bool ok, uint8 level, bytes32 metric) {
        level = levelOf(round, shard);
        if (level == 0) return (false, 0, bytes32(0));
        ok = ledger.verifyRow(round, shard, row, index, leafCount, siblings);
        if (!ok) return (false, 0, bytes32(0));
        metric = _attestations[round][shard].metric;
    }

    // ------------------------------------------------------------------ level 2 facts

    /// @notice The fact key of (metric, subject, window end) under `metric`'s registered
    /// template; 0 for a metric that is not a feed.
    function factKey(bytes32 metric, bytes32 subjectKey, uint64 windowEnd)
        public
        view
        returns (bytes32)
    {
        Metric storage m = metrics[metric];
        if (m.kind != Kind.Feed && m.kind != Kind.DatedFeed) return bytes32(0);
        return factKeyOf(metric, m.template, subjectKey, windowEnd, m.kind == Kind.DatedFeed);
    }

    /// @notice Record a settled level-2 result, once per fact.
    /// ENFORCED: the fact key is computed here from the metric's REGISTERED template (a
    /// substituted template cannot fill this fact), one result per fact, coordinator only.
    /// ATTESTED: that `spec` (the round's `ObservationSpec::id`) is the template instantiated for
    /// this subject and window, and `result` (`ObservationSettlement::result_hash`). A consumer
    /// checks `spec` against its own instantiation (`observationLevel`); a wrong spec can block a
    /// fact (liveness) but never passes that check.
    /// ENFORCED (L2RND-R3-F1/F2): `spec` is the spec the fact's latest draw committed to before
    /// its beacon (`drawCommit(pool, spec, cfgHash)` equals the announced commitment), so the
    /// window, feed and config cannot be chosen after the beacon.
    function recordObservation(
        bytes32 metric,
        bytes32 subjectKey,
        uint64 windowEnd,
        bytes32 spec,
        bytes32 result,
        bytes32 pool,
        bytes32 cfgHash
    ) external onlyCoordinator returns (bytes32 fact) {
        if (
            subjectKey == bytes32(0) || windowEnd == 0 || spec == bytes32(0) || result == bytes32(0)
        ) {
            revert ZeroValue();
        }
        Metric storage m = metrics[metric];
        if (m.kind != Kind.Feed && m.kind != Kind.DatedFeed) revert WrongTemplate();
        fact = factKeyOf(metric, m.template, subjectKey, windowEnd, m.kind == Kind.DatedFeed);
        if (_facts[fact].result != bytes32(0)) revert AlreadyRecorded();
        // A result needs a public draw: the latest announced draw with its beacon fixed.
        uint32 n = drawCount[fact];
        if (n == 0 || _draws[fact][n - 1].beacon == bytes32(0)) revert NoDraw();
        if (_draws[fact][n - 1].commit != drawCommit(pool, spec, cfgHash)) revert DrawMismatch();
        // Amendment 14: once time-locks are required, only a time-locked draw is recorded.
        if (timeLockRequired && !drawTimeLocked[fact][n - 1]) revert NotTimeLocked();
        // The draw that settled this fact ended with a value: it is no longer a strike.
        subjectStrikes[subjectDrawKey(metric, subjectKey)] -= 1;
        _facts[fact] = Observation({
            metric: metric,
            template: m.template,
            subjectKey: subjectKey,
            windowEnd: windowEnd,
            spec: spec,
            result: result,
            recordedAt: uint64(block.number)
        });
        emit ObservationRecorded(fact, metric, m.template, subjectKey, windowEnd, spec, result);
    }

    /// @notice The recorded observation of (metric, subject, window end); `result == 0` when none.
    /// For a dated feed the window end is not part of the key: read it here, then instantiate.
    function observationOf(bytes32 metric, bytes32 subjectKey, uint64 windowEnd)
        external
        view
        returns (Observation memory)
    {
        return _facts[factKey(metric, subjectKey, windowEnd)];
    }

    /// @notice `OBSERVED_LEVEL` when (metric, subject, window end) has exactly this recorded
    /// result, under the spec the consumer instantiated itself from the registered template;
    /// else 0.
    function observationLevel(
        bytes32 metric,
        bytes32 subjectKey,
        uint64 windowEnd,
        bytes32 spec,
        bytes32 result
    ) external view returns (uint8) {
        bytes32 fact = factKey(metric, subjectKey, windowEnd);
        if (fact == bytes32(0) || result == bytes32(0)) return 0;
        Observation storage o = _facts[fact];
        if (o.result != result || o.spec != spec || o.windowEnd != windowEnd) return 0;
        return OBSERVED_LEVEL;
    }

    // ------------------------------------------------------------------ level 2 draws

    /// @notice Announce a fact's next observer draw before its beacon exists (L2RND-R1-F1).
    /// ENFORCED: coordinator only; attempts strictly sequential from 0 and below
    /// `MAX_DRAW_ATTEMPTS`; `beaconHeight` above the current block; one announcement per
    /// (fact, attempt), never changed; none after the fact has a recorded result.
    /// `commit` is `drawCommit(pool, spec, cfgHash)`: every input of the draw except the beacon
    /// (L2RND-R3-F1/F2), so one announced draw reaches exactly one committee.
    /// ATTESTED: that the committed pool hash names the snapshot the coordinator draws from.
    function announceDraw(
        bytes32 metric,
        bytes32 subjectKey,
        uint64 windowEnd,
        uint32 attempt,
        bytes32 commit,
        uint64 beaconHeight
    ) external onlyCoordinator returns (bytes32 fact) {
        if (subjectKey == bytes32(0) || commit == bytes32(0)) {
            revert ZeroValue();
        }
        fact = factKey(metric, subjectKey, windowEnd);
        if (fact == bytes32(0)) revert WrongTemplate();
        if (_facts[fact].result != bytes32(0)) revert AlreadyRecorded();
        if (attempt != drawCount[fact] || attempt >= MAX_DRAW_ATTEMPTS) revert BadAttempt();
        if (beaconHeight <= block.number) revert BeaconNotFuture();
        // L2RND-R2-F1/F2: one bound per (metric, subject), whatever the window or attempt. A
        // draw counts until it ends with a recorded value; beyond `MAX_DRAWS` (plus the owner's
        // recorded reopens) no more draws are announced, so dropping draws to grind the beacon,
        // or opening shifted windows, buys at most `MAX_DRAWS` public tries.
        // SIG-R1-F1: every draw names who signs its reveal receipts before anyone reveals; only
        // a key this coordinator set in its current term (REVLOG LOW-2, REVLOG-R2 LOW-3).
        address signer = _signerOf(msg.sender);
        if (signer == address(0)) revert NoReceiptSigner();
        bytes32 sk = subjectDrawKey(metric, subjectKey);
        uint32 strikes = subjectStrikes[sk];
        if (strikes >= _allowance(sk)) revert DrawsExhausted();
        drawReceiptSigner[fact][attempt] = signer;
        // Amendment 10: the draw's reveal key, when this coordinator term set one.
        bool keyed = _revealKeyBy == msg.sender && _revealKeyEpoch == ledger.coordinatorEpoch();
        if (keyed) {
            drawRevealKey[fact][attempt] = _revealKey;
        }
        // Amendment 14 (ROADMAP C51): with time-locks required, every draw is keyed and
        // time-locked; there is no way to announce one that is not (C11-R2-1 class).
        if (timeLockRequired) {
            if (!keyed) revert NoRevealKey();
            drawTimeLocked[fact][attempt] = true;
        }
        subjectStrikes[sk] = strikes + 1;
        _draws[fact][attempt] = Draw({
            commit: commit,
            beaconHeight: beaconHeight,
            announcedAt: uint64(block.number),
            beacon: bytes32(0)
        });
        drawCount[fact] = attempt + 1;
        emit DrawAnnounced(fact, attempt, commit, beaconHeight);
    }

    /// @notice Write a draw's beacon, `blockhash(beaconHeight)`, once the block exists and while
    /// it is within the last 256 blocks. Anyone may call it; a draw whose window passed can
    /// never be fixed, and its fact needs a new draw (the next attempt).
    function fixBeacon(bytes32 fact, uint32 attempt) external returns (bytes32 beacon) {
        Draw storage d = _draws[fact][attempt];
        if (d.commit == bytes32(0)) revert NoDraw();
        if (d.beacon != bytes32(0)) revert BeaconAlreadyFixed();
        if (block.number <= d.beaconHeight) revert BeaconNotFuture();
        beacon = blockhash(d.beaconHeight);
        if (beacon == bytes32(0)) revert BeaconUnavailable();
        d.beacon = beacon;
        emit BeaconFixed(fact, attempt, beacon);
    }

    /// @notice `keccak256("onyx-registry-subject-draws-v1" ‖ metric ‖ subject key)`: the key of
    /// a (metric, subject)'s draw bound (Rust `onchain::subject_draw_key`).
    function subjectDrawKey(bytes32 metric, bytes32 subjectKey) public pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-registry-subject-draws-v1", metric, subjectKey));
    }

    function _allowance(bytes32 sk) internal view returns (uint32) {
        return MAX_DRAWS * (subjectReopens[sk] + 1);
    }

    /// @notice (strikes, allowance) of (metric, subject): draws that did not end with a recorded
    /// value, and how many the subject may hold. A consumer refuses a draw whose attempt is not
    /// below the allowance.
    function drawBudget(bytes32 metric, bytes32 subjectKey)
        external
        view
        returns (uint32 strikes, uint32 allowance)
    {
        bytes32 sk = subjectDrawKey(metric, subjectKey);
        return (subjectStrikes[sk], _allowance(sk));
    }

    /// @notice The owner (never the coordinator) grants (metric, subject) `MAX_DRAWS` more draws
    /// after reviewing why the earlier ones ended without a value. Public and logged.
    function reopenDraws(bytes32 metric, bytes32 subjectKey) external onlyOwner {
        bytes32 sk = subjectDrawKey(metric, subjectKey);
        uint32 r = subjectReopens[sk] + 1;
        subjectReopens[sk] = r;
        emit DrawsReopened(metric, subjectKey, r);
    }

    /// @notice A fact's announced draw at `attempt` (all zero when none).
    function drawOf(bytes32 fact, uint32 attempt) external view returns (Draw memory) {
        return _draws[fact][attempt];
    }

    /// @notice `keccak256("onyx-draw-commit-v1" ‖ pool ‖ spec ‖ cfgHash)`: the commitment to
    /// every input of a level-2 draw but its beacon (Rust `observe::draw_commit`).
    function drawCommit(bytes32 pool, bytes32 spec, bytes32 cfgHash) public pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-draw-commit-v1", pool, spec, cfgHash));
    }

    /// @notice True when (metric, subject, window end)'s LATEST announced draw is `attempt`,
    /// was announced with `commit` (`drawCommit` of the pool, spec and config the consumer
    /// rebuilt) and `beaconHeight`, and has `beacon` fixed on chain: the draw a served round
    /// names is the one the chain fixed, with every input chosen before the beacon.
    function drawMatches(
        bytes32 metric,
        bytes32 subjectKey,
        uint64 windowEnd,
        uint32 attempt,
        bytes32 commit,
        uint64 beaconHeight,
        bytes32 beacon
    ) external view returns (bool) {
        bytes32 fact = factKey(metric, subjectKey, windowEnd);
        if (fact == bytes32(0) || beacon == bytes32(0)) return false;
        uint32 n = drawCount[fact];
        if (n == 0 || attempt != n - 1) return false;
        Draw storage d = _draws[fact][attempt];
        return d.commit == commit && d.beaconHeight == beaconHeight && d.beacon == beacon;
    }

    // ------------------------------------------------------------------ level 2 transcript log

    /// @notice Set the key that signs reveal receipts of the draws announced from now on.
    function setReceiptSigner(address signer) external onlyCoordinator {
        if (signer == address(0)) revert ZeroValue();
        _receiptSigner = signer;
        _receiptSignerBy = msg.sender;
        _receiptSignerEpoch = ledger.coordinatorEpoch();
        emit ReceiptSignerSet(signer);
    }

    /// @notice The receipt signer new draws snapshot: zero when none was set by the current
    /// coordinator since it last became coordinator (REVLOG LOW-2, REVLOG-R2 LOW-3).
    function receiptSigner() external view returns (address) {
        return _signerOf(ledger.coordinator());
    }

    function _signerOf(address coord) internal view returns (address) {
        return _receiptSignerBy == coord && _receiptSignerEpoch == ledger.coordinatorEpoch()
            ? _receiptSigner
            : address(0);
    }

    /// @notice `keccak256("onyx-transcript-log-v1" ‖ fact ‖ attempt ‖ observer ‖ round)`: the
    /// slot of one observer's posts for one round (Rust `observe::transcript_log_key`).
    function logKey(bytes32 fact, uint32 attempt, address observer, bytes32 round)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked("onyx-transcript-log-v1", fact, attempt, observer, round));
    }

    /// @notice The text an observer signs to commit (Rust `observation_commit_message`).
    function commitMessage(bytes32 round, uint32 attempt, address observer, bytes32 commit)
        public
        pure
        returns (bytes memory)
    {
        return bytes(
            string.concat(
                "Onyx observe commit v1\nround: ",
                OnyxSig.hex32(round),
                "\nattempt: ",
                OnyxSig.decimal(attempt),
                "\nobserver: ",
                OnyxSig.hex20(observer),
                "\ncommit: ",
                OnyxSig.hex32(commit)
            )
        );
    }

    /// @notice The text the receipt signer signs when it holds a reveal opening `commit`
    /// (Rust `reveal_receipt_message`).
    function receiptMessage(bytes32 round, uint32 attempt, address observer, bytes32 commit)
        public
        pure
        returns (bytes memory)
    {
        return bytes(
            string.concat(
                "Onyx reveal receipt v1\nround: ",
                OnyxSig.hex32(round),
                "\nattempt: ",
                OnyxSig.decimal(attempt),
                "\nobserver: ",
                OnyxSig.hex20(observer),
                "\ncommit: ",
                OnyxSig.hex32(commit)
            )
        );
    }

    /// @notice Post an observer's signed commit for a draw whose beacon is fixed (SIG-R1-F1).
    /// Anyone may relay it; only the observer's own signature fills its slot, once per round.
    /// The block time is kept: a commit posted inside the round's commit phase must be in the
    /// round's transcript (`transcriptComplete`). The commit is a salted hash: it shows no answer.
    function postCommit(
        bytes32 fact,
        uint32 attempt,
        address observer,
        bytes32 round,
        bytes32 commit,
        bytes calldata sig
    ) external {
        if (commit == bytes32(0) || round == bytes32(0)) revert ZeroValue();
        if (_draws[fact][attempt].beacon == bytes32(0)) revert NoDraw();
        bytes32 digest = OnyxSig.eip191Digest(commitMessage(round, attempt, observer, commit));
        if (observer == address(0) || OnyxSig.recover(digest, sig) != observer) {
            revert BadSignature();
        }
        bytes32 k = logKey(fact, attempt, observer, round);
        if (_commitLog[k].commit != bytes32(0)) revert AlreadyPosted();
        // `recover` accepted a 65-byte `r ‖ s ‖ v`.
        _commitLog[k] = LoggedCommit({
            commit: commit,
            postedAt: uint64(block.timestamp),
            v: uint8(sig[64]),
            r: bytes32(sig[0:32]),
            s: bytes32(sig[32:64])
        });
        emit CommitPosted(fact, attempt, observer, round, commit);
    }

    /// @notice Post a reveal receipt: the draw's receipt signer said it holds `observer`'s
    /// reveal opening `commit` in `round`. Anyone may post it (the observer, normally). Then
    /// the round's transcript must list that commit and its reveal (`transcriptComplete`).
    /// Neither the receipt nor this call shows the answer.
    function postReceipt(
        bytes32 fact,
        uint32 attempt,
        address observer,
        bytes32 round,
        bytes32 commit,
        bytes calldata sig
    ) external {
        if (commit == bytes32(0) || round == bytes32(0)) revert ZeroValue();
        address signer = drawReceiptSigner[fact][attempt];
        if (signer == address(0)) revert NoDraw();
        bytes32 digest = OnyxSig.eip191Digest(receiptMessage(round, attempt, observer, commit));
        if (OnyxSig.recover(digest, sig) != signer) revert BadSignature();
        bytes32 k = logKey(fact, attempt, observer, round);
        if (receiptLog[k] != bytes32(0)) revert AlreadyPosted();
        receiptLog[k] = commit;
        emit ReceiptPosted(fact, attempt, observer, round, commit);
    }

    /// @notice An observer's posted commit for a round, when it landed and its signature (zero
    /// and empty when none).
    function commitLogOf(bytes32 fact, uint32 attempt, address observer, bytes32 round)
        external
        view
        returns (bytes32 commit, uint64 postedAt, bytes memory sig)
    {
        LoggedCommit storage c = _commitLog[logKey(fact, attempt, observer, round)];
        if (c.commit != bytes32(0)) sig = abi.encodePacked(c.r, c.s, c.v);
        return (c.commit, c.postedAt, sig);
    }

    /// @notice True when a served transcript of `round` (draw `attempt` of `fact`) leaves out
    /// nothing the chain logged (SIG-R1-F1). `entries` lists EVERY drawn observer of the round
    /// as `observer (20 bytes) ‖ served commit (32 bytes, zero when none) ‖ revealed (1 byte,
    /// 0 or 1)`; the client builds it from the transcript it rebuilt and the draw it checked.
    /// False when, for any entry:
    /// - a commit the observer posted with a block time in [`commitOpen`, `commitClose`] (the
    ///   round's window start and commit close) is not the served commit; or
    /// - a posted receipt names a commit that is not the served commit, or the served
    ///   transcript has no reveal for it.
    /// Also false for a draw that was never announced, or a malformed `entries`.
    function transcriptComplete(
        bytes32 fact,
        uint32 attempt,
        bytes32 round,
        uint64 commitOpen,
        uint64 commitClose,
        bytes calldata entries
    ) external view returns (bool) {
        if (drawReceiptSigner[fact][attempt] == address(0) || entries.length % 53 != 0) {
            return false;
        }
        for (uint256 i = 0; i < entries.length; i += 53) {
            address observer = address(bytes20(entries[i:i + 20]));
            bytes32 served = bytes32(entries[i + 20:i + 52]);
            uint8 revealed = uint8(entries[i + 52]);
            if (revealed > 1) return false;
            bytes32 k = logKey(fact, attempt, observer, round);
            LoggedCommit storage c = _commitLog[k];
            if (
                c.commit != bytes32(0) && c.postedAt >= commitOpen && c.postedAt <= commitClose
                    && c.commit != served
            ) {
                return false;
            }
            bytes32 r = receiptLog[k];
            if (r != bytes32(0) && (r != served || revealed == 0)) return false;
        }
        return true;
    }

    // ------------------------------------------------------------------ level 2 sealed reveals

    /// @notice Set the key observers encrypt sealed reveal posts to, for the draws announced from
    /// now on (ADR-0027 amendment 10). An uncompressed secp256k1 point, checked on the curve.
    function setRevealKey(bytes32 x, bytes32 y) external onlyCoordinator {
        if (!onCurve(x, y)) revert NotOnCurve();
        _revealKey = RevealKey(x, y);
        _revealKeyBy = msg.sender;
        _revealKeyEpoch = ledger.coordinatorEpoch();
        emit RevealKeySet(x, y);
    }

    /// @notice Stop naming a reveal key in the draws announced from now on (ROADMAP C11, review
    /// C11-R1-2): a coordinator that runs without one announces draws that take no sealed post;
    /// draws announced before keep the key they snapshotted.
    function clearRevealKey() external onlyCoordinator {
        delete _revealKey;
        _revealKeyBy = address(0);
        _revealKeyEpoch = 0;
        emit RevealKeySet(bytes32(0), bytes32(0));
    }

    /// @notice Require a drand time-lock on every sealed post of every draw announced from now on
    /// (ADR-0027 amendment 14, ROADMAP C51). The owner or the coordinator may turn it on; NOBODY
    /// can turn it off (there is no such function), and it survives coordinator changes. From
    /// then on `announceDraw` needs a reveal key and snapshots `drawTimeLocked`, and rounds of
    /// draws announced before (not time-locked) are no longer recorded. Calling it again changes
    /// nothing.
    function requireTimeLock() external {
        if (msg.sender != owner && msg.sender != ledger.coordinator()) revert NotOwner();
        if (timeLockRequired) return;
        timeLockRequired = true;
        emit TimeLockRequired(msg.sender);
    }

    /// @notice The reveal key new draws snapshot: zero when the current coordinator set none in
    /// its current term.
    function revealKey() external view returns (bytes32 x, bytes32 y) {
        if (_revealKeyBy == ledger.coordinator() && _revealKeyEpoch == ledger.coordinatorEpoch()) {
            return (_revealKey.x, _revealKey.y);
        }
    }

    /// @notice True when (x, y) is a point of secp256k1 (`y² = x³ + 7 mod p`, both below p).
    function onCurve(bytes32 x, bytes32 y) public pure returns (bool) {
        uint256 px = uint256(x);
        uint256 py = uint256(y);
        if (px >= P || py >= P || (px == 0 && py == 0)) return false;
        return mulmod(py, py, P) == addmod(mulmod(mulmod(px, px, P), px, P), 7, P);
    }

    /// @notice Fiat-Shamir challenge of the Schnorr proof that the poster knows the ephemeral
    /// secret `r` of `R = (ex, ey)`, bound to the round, attempt and observer (Rust
    /// `sealed::key_proof_challenge`): `keccak256("onyx-sealed-key-pok-v1" ‖ ex ‖ ey ‖ addr(T)
    /// ‖ round ‖ attempt ‖ observer) mod n`.
    function keyProofChallenge(
        bytes32 ex,
        bytes32 ey,
        address t,
        bytes32 round,
        uint32 attempt,
        address observer
    ) public pure returns (uint256) {
        return uint256(
            keccak256(
            abi.encodePacked("onyx-sealed-key-pok-v1", ex, ey, t, round, attempt, observer)
        )
        ) % N;
    }

    /// @notice True when `proof = addr(T) (20 bytes) ‖ z (32 bytes)` is a Schnorr proof of
    /// knowledge of `r` with `R = r·G` (review C4-R1-1: an ECDSA signature is NOT one, since a
    /// signature that recovers to a point nobody knows the secret of is easy to make). Checks
    /// `z·G − c·R = T` with one `ecrecover`, which returns the address of
    /// `r_s⁻¹(s_s·P − h·G)` for the point P with x = r_s: with P = R, r_s = R.x, s_s = −c·R.x and
    /// h = −z·R.x it is the address of `z·G − c·R`.
    function keyProofValid(
        bytes32 ex,
        bytes32 ey,
        bytes32 round,
        uint32 attempt,
        address observer,
        bytes calldata proof
    ) public pure returns (bool) {
        if (proof.length != 52 || !onCurve(ex, ey)) return false;
        uint256 rx = uint256(ex);
        if (rx >= N) return false;
        address t = address(bytes20(proof[0:20]));
        uint256 z = uint256(bytes32(proof[20:52]));
        if (t == address(0) || z == 0 || z >= N) return false;
        uint256 c = keyProofChallenge(ex, ey, t, round, attempt, observer);
        if (c == 0) return false;
        uint8 v = uint256(ey) & 1 == 0 ? 27 : 28;
        bytes32 h = bytes32((N - mulmod(z, rx, N)) % N);
        bytes32 sv = bytes32(N - mulmod(c, rx, N));
        return ecrecover(h, v, ex, sv) == t;
    }

    /// @notice The text an observer signs to post a sealed reveal (Rust
    /// `sealed_reveal_message`): the ephemeral key and the ciphertext's hash.
    function sealedRevealMessage(
        bytes32 round,
        uint32 attempt,
        address observer,
        bytes32 ex,
        bytes32 ey,
        bytes32 ciphertextHash
    ) public pure returns (bytes memory) {
        bytes memory hy = bytes(OnyxSig.hex32(ey));
        bytes memory tail = new bytes(64);
        for (uint256 i = 0; i < 64; i++) {
            tail[i] = hy[i + 2];
        }
        return bytes(
            string.concat(
                "Onyx observe sealed reveal v1\nround: ",
                OnyxSig.hex32(round),
                "\nattempt: ",
                OnyxSig.decimal(attempt),
                "\nobserver: ",
                OnyxSig.hex20(observer),
                "\nkey: ",
                OnyxSig.hex32(ex),
                string(tail),
                "\nciphertext: ",
                OnyxSig.hex32(ciphertextHash)
            )
        );
    }

    /// @notice Post an observer's reveal encrypted to the draw's reveal key (ADR-0027 amendment
    /// 10, REVLOG-1). Anyone may relay it; only the observer's signature fills its slot, once
    /// per round, and `keyProof` must prove knowledge of the ephemeral secret of (ex, ey) for
    /// this round, attempt and observer (`keyProofValid`), so nobody posts a key it does not
    /// hold, another observer's or a point related to it, and a dismissal (which reveals the
    /// shared point of this key) is never a decryption oracle (review C4-R1-1). The block time is kept: a post
    /// inside the round's reveal phase must be in the round's transcript, revealed or dismissed
    /// with a proof (`revealLogComplete`). The post shows no answer.
    function postSealedReveal(
        bytes32 fact,
        uint32 attempt,
        address observer,
        bytes32 round,
        bytes32 ex,
        bytes32 ey,
        bytes calldata ct,
        bytes calldata keyProof,
        bytes calldata sig
    ) external {
        if (round == bytes32(0) || observer == address(0)) {
            revert ZeroValue();
        }
        if (_draws[fact][attempt].beacon == bytes32(0)) revert NoDraw();
        if (drawRevealKey[fact][attempt].x == bytes32(0)) revert NoRevealKey();
        if (ct.length == 0 || ct.length > MAX_SEALED_BYTES) revert BadCiphertext();
        // Amendment 14: a time-locked draw's post leads with its time-lock and has a body;
        // amendment 15 (C52): the far-date copy and the wrapped body key come before the body.
        if (ct.length <= TIME_LOCK_BYTES + FAR_COPY_BYTES && drawTimeLocked[fact][attempt]) {
            revert BadCiphertext();
        }
        bytes32 digest = OnyxSig.eip191Digest(
            sealedRevealMessage(round, attempt, observer, ex, ey, keccak256(ct))
        );
        if (OnyxSig.recover(digest, sig) != observer) revert BadSignature();
        if (!keyProofValid(ex, ey, round, attempt, observer, keyProof)) revert BadKeyProof();
        bytes32 k = logKey(fact, attempt, observer, round);
        SealedPost storage p = _sealedLog[k];
        if (p.postedAt != 0) revert AlreadyPosted();
        p.ex = ex;
        p.ey = ey;
        p.postedAt = uint64(block.timestamp);
        p.keyT = address(bytes20(keyProof[0:20]));
        p.keyZ = bytes32(keyProof[20:52]);
        p.ct = ct;
        emit SealedRevealPosted(fact, attempt, observer, round, keccak256(ct));
    }

    /// @notice An observer's sealed reveal post for a round (all zero and empty when none).
    function sealedRevealOf(bytes32 fact, uint32 attempt, address observer, bytes32 round)
        external
        view
        returns (bytes32 ex, bytes32 ey, uint64 postedAt, bytes memory ct)
    {
        SealedPost storage p = _sealedLog[logKey(fact, attempt, observer, round)];
        return (p.ex, p.ey, p.postedAt, p.ct);
    }

    /// @notice The key proof of an observer's sealed reveal post (`addr(T) ‖ z`; 52 zero bytes
    /// when none). The coordinator re-checks it (`keyProofValid`) before it dismisses a post, so a
    /// post it reads from a lying node never makes its dismissal a decryption oracle (C4-R3-2).
    function sealedKeyProofOf(bytes32 fact, uint32 attempt, address observer, bytes32 round)
        external
        view
        returns (bytes memory)
    {
        SealedPost storage p = _sealedLog[logKey(fact, attempt, observer, round)];
        return abi.encodePacked(p.keyT, p.keyZ);
    }

    /// @notice `transcriptComplete` with sealed posts (ADR-0027 amendment 10). `entries` lists
    /// EVERY drawn observer as `observer (20) ‖ served commit (32) ‖ status (1)`, status 0 not
    /// revealed, 1 revealed, 2 dismissed (its sealed post is garbage; the transcript carries the
    /// proof, which clients check). False when, for any entry:
    /// - a commit posted in [`commitOpen`, `commitClose`] is not the served commit;
    /// - a posted receipt's commit is not served, or its status is not 1;
    /// - a sealed post with a block time in (`commitClose`, `revealClose`] has status 0;
    /// - status 2 without such a post, or above 2.
    /// Also false for a draw never announced, a malformed `entries`, observers not strictly
    /// ascending, or a bad window. The chain does not know the drawn set, the round id or the
    /// window: that `entries` lists exactly the drawn observers and the window is the round's
    /// config is the CLIENT's check (review C4-R1-2).
    function revealLogComplete(
        bytes32 fact,
        uint32 attempt,
        bytes32 round,
        uint64 commitOpen,
        uint64 commitClose,
        uint64 revealClose,
        bytes calldata entries
    ) public view returns (bool) {
        if (
            drawReceiptSigner[fact][attempt] == address(0) || entries.length % 53 != 0
                || commitOpen > commitClose || commitClose >= revealClose
        ) {
            return false;
        }
        address prev;
        for (uint256 i = 0; i < entries.length; i += 53) {
            address observer = address(bytes20(entries[i:i + 20]));
            bytes32 served = bytes32(entries[i + 20:i + 52]);
            uint8 status = uint8(entries[i + 52]);
            // Strictly ascending observers (review C4-R1-2): no entry twice, none zero.
            if (status > 2 || uint160(observer) <= uint160(prev)) return false;
            prev = observer;
            bytes32 k = logKey(fact, attempt, observer, round);
            LoggedCommit storage c = _commitLog[k];
            if (
                c.commit != bytes32(0) && c.postedAt >= commitOpen && c.postedAt <= commitClose
                    && c.commit != served
            ) {
                return false;
            }
            bytes32 r = receiptLog[k];
            if (r != bytes32(0) && (r != served || status != 1)) return false;
            uint64 at = _sealedLog[k].postedAt;
            bool sealedInPhase = at > commitClose && at <= revealClose;
            if (sealedInPhase && status == 0) return false;
            if (status == 2 && !sealedInPhase) return false;
        }
        return true;
    }

    /// @notice `keccak256("onyx-attempt-record-v1" ‖ round ‖ commitOpen ‖ commitClose ‖
    /// revealClose ‖ result ‖ keccak256(entries))` (Rust `sealed::attempt_digest`).
    function attemptDigest(
        bytes32 round,
        uint64 commitOpen,
        uint64 commitClose,
        uint64 revealClose,
        bytes32 result,
        bytes calldata entries
    ) public pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-attempt-record-v1",
                round,
                commitOpen,
                commitClose,
                revealClose,
                result,
                keccak256(entries)
            )
        );
    }

    /// @notice Record a round of a draw before it is served (REVLOG-5): which observers it lists
    /// as revealed, dismissed or neither, and its result hash. Coordinator only, once per draw,
    /// with the beacon fixed, only once the chain's time is past `revealClose` (no later post
    /// can land in the window), and only when `revealLogComplete` holds now: a record never
    /// leaves out a posted commit, a receipted reveal or a sealed post.
    function recordAttempt(
        bytes32 fact,
        uint32 attempt,
        bytes32 round,
        uint64 commitOpen,
        uint64 commitClose,
        uint64 revealClose,
        bytes32 result,
        bytes calldata entries
    ) external onlyCoordinator {
        if (round == bytes32(0) || result == bytes32(0)) revert ZeroValue();
        if (_draws[fact][attempt].beacon == bytes32(0)) revert NoDraw();
        if (attemptRecord[fact][attempt] != bytes32(0)) revert AlreadyRecorded();
        if (timeLockRequired && !drawTimeLocked[fact][attempt]) revert NotTimeLocked();
        if (commitOpen > commitClose || commitClose >= revealClose) revert BadWindow();
        if (block.timestamp <= revealClose) revert TooEarly();
        if (!revealLogComplete(fact, attempt, round, commitOpen, commitClose, revealClose, entries))
        {
            revert IncompleteTranscript();
        }
        bytes32 d = attemptDigest(round, commitOpen, commitClose, revealClose, result, entries);
        attemptRecord[fact][attempt] = d;
        emit AttemptRecorded(fact, attempt, round, d);
    }

    /// @notice True when the draw's recorded round is exactly this one (round, window, result
    /// and every entry). A client asks it for a sealed round before trusting its `revealed`
    /// list, and again once the round is unsealed, with the entries it rebuilt.
    function attemptMatches(
        bytes32 fact,
        uint32 attempt,
        bytes32 round,
        uint64 commitOpen,
        uint64 commitClose,
        uint64 revealClose,
        bytes32 result,
        bytes calldata entries
    ) external view returns (bool) {
        bytes32 d = attemptRecord[fact][attempt];
        return d != bytes32(0)
            && d == attemptDigest(round, commitOpen, commitClose, revealClose, result, entries);
    }
}
