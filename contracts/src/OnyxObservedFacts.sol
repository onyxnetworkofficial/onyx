// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IOnyxFacts, IOnyxFactGuard} from "./IOnyxMarkets.sol";
import {OnyxRegistry} from "./OnyxRegistry.sol";

/// @title OnyxObservedFacts
/// @notice The values of level-2 facts recorded in `OnyxRegistry`, as `OnyxFutures` reads them
/// (NORTH-STAR NS11, ADR-0042 amendment 2). The registry keeps only a fact's result hash
/// (`keccak256("onyx-observe-result-v1" ‖ borsh(spec id, round id, outcome))`,
/// `onyx_coordinator::observe::result_hash_of`). Anyone may `prove` a fact's value by giving the
/// round id and the number: this contract rebuilds the result hash from the recorded spec and
/// accepts the value only when it equals the recorded result. The value is then final at level 2.
///
/// A fact is `list`ed by the owner before its value exists, with the first block a recording may
/// come from (`notBeforeBlock`): a result the registry recorded earlier is never accepted, so a
/// market cannot be opened on a value already on chain. On testnet the owner is the market owner.
/// The listing also states `closeBy`, the latest time trading on the fact may close (the source's
/// earliest publication for it, less a margin, `Source::earliest_public`): `OnyxFutures` and
/// `OnyxParametric` refuse a later close, so nobody trades on a value the source already shows
/// (THREATS NL6-10). The owner and the registry's coordinator are insiders (`isInsider`): they
/// decide when the value is recorded, so they may not bet on or insure it (NL6-12).
///
/// ENFORCED: the listed fact key is the registry's own key for (metric, subject, window) under
/// its registered template when the value is proved; the value hashes, with the recorded spec
/// and the given round, to the recorded result; the recording is at or after `notBeforeBlock`;
/// one value per fact. ATTESTED (by the registry's coordinator, as for every level-2 fact): that
/// the recorded spec is the template instantiated for this subject and window and that the
/// result is what the observers' reveals aggregate to. A client checks both off chain
/// (`level2-check`), THREATS NL6-2.
contract OnyxObservedFacts is IOnyxFacts, IOnyxFactGuard {
    struct Listed {
        bytes32 metric;
        bytes32 subjectKey;
        uint64 windowEnd;
        uint64 notBeforeBlock;
    }

    struct Fact {
        bool final_;
        int256 value;
        uint64 acceptedAt;
        bytes32 round;
    }

    /// The proof level of a value recorded by observers (`OnyxRegistry.OBSERVED_LEVEL`).
    uint8 public constant LEVEL = 2;

    OnyxRegistry public immutable registry;
    address public immutable owner;

    mapping(bytes32 => Listed) public listed;
    mapping(bytes32 => Fact) internal _facts;
    /// @inheritdoc IOnyxFactGuard
    mapping(bytes32 => uint64) public closeBy;

    error NotOwner();
    error BadFact();
    error AlreadyListed();
    error AlreadyRecorded();
    error NotListed();
    error AlreadyProved();
    error NoResult();
    error TooEarly();
    error WrongValue();

    event FactListed(
        bytes32 indexed fact,
        bytes32 metric,
        bytes32 subjectKey,
        uint64 windowEnd,
        uint64 notBefore,
        uint64 closeBy
    );
    event Proved(bytes32 indexed fact, int256 value, bytes32 round, uint64 recordedAt);

    constructor(OnyxRegistry registry_, address owner_) {
        if (address(registry_) == address(0) || owner_ == address(0)) revert BadFact();
        registry = registry_;
        owner = owner_;
    }

    /// @notice List a fact a market will settle from: `template` and `dated` give the key the
    /// registry will compute once the metric is registered (`OnyxRegistry.factKeyOf`).
    /// `notBeforeBlock` is the block at the fact's `observableAt` (THREATS NL6-11); `closeBy_` the
    /// latest trading close (NL6-10).
    function list(
        bytes32 metric,
        bytes32 template,
        bytes32 subjectKey,
        uint64 windowEnd,
        bool dated,
        uint64 notBeforeBlock,
        uint64 closeBy_
    ) external returns (bytes32 fact) {
        if (msg.sender != owner) revert NotOwner();
        if (
            metric == bytes32(0) || subjectKey == bytes32(0) || notBeforeBlock <= block.number
                || closeBy_ <= block.timestamp
        ) {
            revert BadFact();
        }
        fact = registry.factKeyOf(metric, template, subjectKey, windowEnd, dated);
        if (listed[fact].metric != bytes32(0)) revert AlreadyListed();
        // A value already on chain is no future value.
        if (registry.factKey(metric, subjectKey, windowEnd) == fact) {
            if (registry.observationOf(metric, subjectKey, windowEnd).result != bytes32(0)) {
                revert AlreadyRecorded();
            }
        }
        listed[fact] = Listed(metric, subjectKey, windowEnd, notBeforeBlock);
        closeBy[fact] = closeBy_;
        emit FactListed(fact, metric, subjectKey, windowEnd, notBeforeBlock, closeBy_);
    }

    /// @inheritdoc IOnyxFactGuard
    function isInsider(address who) external view returns (bool) {
        return who == owner || who == registry.ledger().coordinator();
    }

    /// @notice Accept `value` as the listed fact's value when it is the registry's recorded
    /// result for `round`. Anyone may call it.
    function prove(bytes32 fact, bytes32 round, int128 value) external {
        Listed storage l = listed[fact];
        if (l.metric == bytes32(0)) revert NotListed();
        if (_facts[fact].final_) revert AlreadyProved();
        // The registered template must be the listed one (else the keys differ).
        if (registry.factKey(l.metric, l.subjectKey, l.windowEnd) != fact) revert BadFact();
        OnyxRegistry.Observation memory o =
            registry.observationOf(l.metric, l.subjectKey, l.windowEnd);
        if (o.result == bytes32(0)) revert NoResult();
        if (o.recordedAt < l.notBeforeBlock) revert TooEarly();
        if (resultHash(o.spec, round, value) != o.result) revert WrongValue();
        _facts[fact] = Fact(true, value, uint64(block.timestamp), round);
        emit Proved(fact, value, round, o.recordedAt);
    }

    /// @notice `keccak256("onyx-observe-result-v1" ‖ spec ‖ round ‖ 0x00 ‖ 0x01 ‖ value as
    /// i128 little-endian)`: Borsh of (spec id, round id, `ObservationOutcome::Value(
    /// Answer::Number(value))`), vectors in fixtures/markets/observed-facts.json.
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

    /// @inheritdoc IOnyxFacts
    function factValue(bytes32 fact)
        external
        view
        returns (bool final_, int256 value, uint8 level, uint64 acceptedAt)
    {
        Fact storage f = _facts[fact];
        if (!f.final_) return (false, 0, 0, 0);
        return (true, f.value, LEVEL, f.acceptedAt);
    }

    /// @notice The round a proved value came from (zero before it is proved).
    function roundOf(bytes32 fact) external view returns (bytes32) {
        return _facts[fact].round;
    }
}
