// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxCommit} from "./OnyxCommit.sol";
import {OnyxLedger} from "./OnyxLedger.sol";

/// @notice What `OnyxRevenue` reads from the settlement ledger (ADR-0024 readers).
interface IOnyxLedgerView {
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
    function revealsOf(uint64 id, bytes32 shard) external view returns (OnyxLedger.Reveal[] memory);
    function roundMembers(uint64 id) external view returns (address[] memory);
    function tallyOf(uint64 id, bytes32 shard, address v)
        external
        view
        returns (OnyxLedger.Tally memory);
}

/// @notice The token surface revenue needs: USDC (EIP-3009) on Base Sepolia.
interface IRevenueToken {
    function balanceOf(address who) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    /// EIP-3009: true once `authorizer` used `nonce`.
    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool);
    /// EIP-3009 (FiatToken v2.2 form, 65-byte or ERC-1271 signature): move `value` from `from`
    /// to `to` on `from`'s signed authorization; anyone may submit it.
    function transferWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        bytes calldata signature
    ) external;
}

/// @title OnyxRevenue
/// @notice Usage revenue of finalized shards, split by role (ADR-0025, docs/spec/revenue.md).
/// The API's x402 `payTo` is this contract. A shard is registered once it is final: its miners
/// (reveal equal to the final root) and, when no case decided it, its validators (at least one
/// Agree) are fixed then, with the label set the shard pinned. The label share goes to that set's
/// payee as named when the fee arrives, or is held for the set until a payee is named
/// (REV-L1-F3). Fees come in two ways: `credit`, where the API's reporter attributes x402
/// payments that already landed here (total-capped: never more than arrived and was not yet
/// attributed; REV-L1-F4), `settle`, where the reporter submits the client's x402 authorization
/// itself so the transfer and its shard are credited in one call (ADR-0026: recipient and amount
/// proven by the token, not attested), and `pay`, a direct on-chain payment for a shard. Each fee
/// is split as `onyx_coordinator::revenue::split`; heads claim, then withdraw.
contract OnyxRevenue {
    uint16 public constant BPS = 10_000;
    uint256 public constant MAX_RECEIPTS = 64;
    uint8 internal constant MINER = 1;
    uint8 internal constant VALIDATOR = 2;
    /// The round of every level-2 observation book (R-REV-12, ADR-0027 amendment 4): no settlement
    /// round has it, so `(OBSERVATION_ROUND, fact)` never names a shard.
    uint64 public constant OBSERVATION_ROUND = type(uint64).max;

    struct Book {
        bool registered;
        uint32 miners;
        uint32 validators;
        uint64 finalizedAt;
        /// The label set the shard pinned (`LabelSetRef::hash`), or zero.
        bytes32 labelSet;
        /// Running sum of what one miner / one validator has earned on this shard.
        uint128 perMiner;
        uint128 perValidator;
        uint128 revenue;
    }

    /// An x402 `exact` client's EIP-3009 authorization; `to` is this contract (the `payTo`).
    struct Authorization {
        address from;
        uint256 value;
        uint256 validAfter;
        uint256 validBefore;
        bytes32 nonce;
    }

    /// One x402 payment the reporter attributes to a shard.
    struct Receipt {
        uint64 round;
        bytes32 shard;
        uint128 amount;
        address payer;
        bytes32 nonce;
    }

    IRevenueToken public immutable token;
    IOnyxLedgerView public immutable ledger;
    /// The treasury: may replace the reporter (REV-L1-F4: a leaked reporter key is rotated
    /// out). Deploy with a treasury key other than the reporter's, or rotation proves nothing.
    address public immutable owner;
    /// The API operator: attributes x402 payments and names label payees.
    address public reporter;
    address public immutable treasury;
    uint16 public immutable minersBps;
    uint16 public immutable validatorsBps;
    uint16 public immutable labelsBps;
    uint16 public immutable treasuryBps;

    mapping(uint64 => mapping(bytes32 => Book)) public books;
    mapping(uint64 => mapping(bytes32 => mapping(address => uint8))) public roleOf;
    mapping(uint64 => mapping(bytes32 => mapping(address => uint128))) public claimedMiner;
    mapping(uint64 => mapping(bytes32 => mapping(address => uint128))) public claimedValidator;
    /// Newest finalization block of a shard where the address did accepted work: the
    /// contributor pass (free fresh reads) is read from here (ADR-0025 item 3).
    mapping(address => uint64) public lastAccepted;
    /// Payee of a label set, by `LabelSetRef::hash`; read when a fee is split (REV-L1-F3).
    mapping(bytes32 => address) public labelPayee;
    /// Label shares earned by a set before it had a payee; paid to the first payee named.
    mapping(bytes32 => uint256) public labelHeld;
    mapping(bytes32 => bool) public receiptUsed;
    mapping(address => uint256) public balanceOf;
    /// Every fee ever credited; `paidOut` every withdrawal. Their difference is what is owed.
    uint256 public allocated;
    uint256 public paidOut;
    bool private _locked;

    /// A level-2 observation book was opened for `fact` (`OnyxRegistry.factKeyOf`).
    event ObservationRegistered(bytes32 indexed fact);
    event ShardRegistered(
        uint64 indexed round,
        bytes32 indexed shard,
        uint32 miners,
        uint32 validators,
        bytes32 labelSet
    );
    event Credited(
        uint64 indexed round,
        bytes32 indexed shard,
        uint128 amount,
        address indexed payer,
        bytes32 nonce
    );
    event Claimed(uint64 indexed round, bytes32 indexed shard, address indexed who, uint256 amount);
    event Withdrawn(address indexed who, uint256 amount);
    event LabelPayeeSet(bytes32 indexed labelSetHash, address payee);
    event ReporterSet(address reporter);
    /// An x402 authorization the reporter settled here itself (ADR-0026): the token moved exactly
    /// `amount` from `payer` to this contract in the same call that credited the shard.
    event Settled(
        uint64 indexed round,
        bytes32 indexed shard,
        address indexed payer,
        bytes32 nonce,
        uint128 amount
    );
    /// A receipt `credit` did not apply (REV-L1-F6), with the reason: 1 reused, 2 the token
    /// does not report the authorization used, 3 more than is unallocated, 4 shard not registered.
    event ReceiptSkipped(
        uint64 indexed round, bytes32 indexed shard, address indexed payer, bytes32 nonce, uint8 why
    );

    error BadSplit();
    error NotReporter();
    error AlreadyRegistered();
    error NotRegistered();
    error NotFinal();
    error BadFields();
    error NotOwner();
    error TooMany();
    error TransferFailed();
    error Reentrancy();
    /// The token did not move exactly the authorized value to this contract.
    error NotReceived();
    /// This `(payer, nonce)` was already attributed (by a receipt or an earlier settle).
    error AlreadyCredited();

    modifier onlyReporter() {
        if (msg.sender != reporter) revert NotReporter();
        _;
    }

    modifier nonReentrant() {
        if (_locked) revert Reentrancy();
        _locked = true;
        _;
        _locked = false;
    }

    /// @param bps miners, validators, labels, treasury shares in basis points; sum 10,000.
    constructor(
        IRevenueToken token_,
        IOnyxLedgerView ledger_,
        address reporter_,
        address treasury_,
        uint16[4] memory bps
    ) {
        if (
            uint256(bps[0]) + bps[1] + bps[2] + bps[3] != BPS || treasury_ == address(0)
                || reporter_ == address(0)
        ) {
            revert BadSplit();
        }
        token = token_;
        ledger = ledger_;
        owner = treasury_;
        reporter = reporter_;
        treasury = treasury_;
        minersBps = bps[0];
        validatorsBps = bps[1];
        labelsBps = bps[2];
        treasuryBps = bps[3];
    }

    /// @notice Replace the reporter (owner only).
    function setReporter(address reporter_) external {
        if (msg.sender != owner) revert NotOwner();
        if (reporter_ == address(0)) revert BadSplit();
        reporter = reporter_;
        emit ReporterSet(reporter_);
    }

    /// @notice Name the payee of a label set's share, for every fee split from now on (all shards
    /// that pinned the set). What the set earned with no payee is paid to the payee named.
    function setLabelPayee(bytes32 labelSetHash, address payee) external onlyReporter {
        labelPayee[labelSetHash] = payee;
        uint256 held = labelHeld[labelSetHash];
        if (payee != address(0) && held != 0) {
            labelHeld[labelSetHash] = 0;
            balanceOf[payee] += held;
        }
        emit LabelPayeeSet(labelSetHash, payee);
    }

    /// @notice Fix a final shard's recipients (R-REV-1). Permissionless and once. `f` must hash to
    /// the ledger's final binding, which proves the label set the shard pinned. Miners: every
    /// reveal equal to the final root. Validators: round members with an Agree on the shard,
    /// only when no case decided it (`closed` false: unanimous and undisputed, so every Agree
    /// was right); a case paid its panel itself. Cost: O(reveals + members) view calls.
    function register(uint64 id, bytes32 shard, OnyxCommit.CommitmentFields calldata f)
        external
        nonReentrant
    {
        if (id == OBSERVATION_ROUND) revert BadFields();
        Book storage b = books[id][shard];
        if (b.registered) revert AlreadyRegistered();
        if (!ledger.isFinal(id, shard)) revert NotFinal();
        (, bool closed, uint64 finalizedAt, bytes32 finalRoot, bytes32 finalBinding) =
            ledger.shardState(id, shard);
        if (f.root != finalRoot || OnyxCommit.bindingHash(f) != finalBinding) revert BadFields();
        b.registered = true;
        b.finalizedAt = finalizedAt;
        OnyxLedger.Reveal[] memory rv = ledger.revealsOf(id, shard);
        uint32 miners;
        for (uint256 i = 0; i < rv.length; i++) {
            address m = rv[i].miner;
            if (rv[i].root != finalRoot || roleOf[id][shard][m] & MINER != 0) continue;
            roleOf[id][shard][m] |= MINER;
            miners++;
            _accept(m, finalizedAt);
        }
        uint32 validators;
        if (!closed) {
            address[] memory members = ledger.roundMembers(id);
            for (uint256 i = 0; i < members.length; i++) {
                address v = members[i];
                if (ledger.tallyOf(id, shard, v).agrees == 0) continue;
                roleOf[id][shard][v] |= VALIDATOR;
                validators++;
                _accept(v, finalizedAt);
            }
        }
        b.miners = miners;
        b.validators = validators;
        b.labelSet = f.labelSetHash;
        emit ShardRegistered(id, shard, miners, validators, f.labelSetHash);
    }

    /// @notice Open the revenue book of a level-2 fact (R-REV-12) so the API's x402 sales of it can
    /// be credited (`credit`, `settle`) or paid directly (`pay`) under `(OBSERVATION_ROUND, fact)`.
    /// Reporter-only and once. The book has no miners, validators or label set: every fee it
    /// earns goes to the treasury until observer payouts exist on chain (THREATS L2API-3). The
    /// reporter already chooses which book a payment buys (REV-1); this adds no new power over
    /// shard books, whose recipients are fixed by `register` from the ledger.
    function registerObservation(bytes32 fact) external onlyReporter {
        if (fact == bytes32(0)) revert BadFields();
        Book storage b = books[OBSERVATION_ROUND][fact];
        if (b.registered) revert AlreadyRegistered();
        b.registered = true;
        b.finalizedAt = uint64(block.number);
        emit ObservationRegistered(fact);
    }

    /// @notice Attribute x402 payments already received to the shards they bought (R-REV-3).
    /// Each `(payer, nonce)` once, only if the token reports that authorization used, and never
    /// more in total than arrived and was not yet attributed. The checks are total-capped only:
    /// `authorizationState` binds neither recipient nor amount, so the reporter is trusted for
    /// the attribution (REV-1, REV-L1-F4). A receipt that fails a check is skipped with
    /// `ReceiptSkipped`, never reverting the batch (REV-L1-F6). Returns how many applied.
    function credit(Receipt[] calldata rs)
        external
        nonReentrant
        onlyReporter
        returns (uint256 applied)
    {
        if (rs.length > MAX_RECEIPTS) revert TooMany();
        uint256 room = unallocated();
        for (uint256 i = 0; i < rs.length; i++) {
            Receipt calldata r = rs[i];
            bytes32 key = keccak256(abi.encode(r.payer, r.nonce));
            uint8 why = 0;
            if (receiptUsed[key]) why = 1;
            else if (!token.authorizationState(r.payer, r.nonce)) why = 2;
            else if (r.amount > room) why = 3;
            else if (!books[r.round][r.shard].registered) why = 4;
            if (why != 0) {
                emit ReceiptSkipped(r.round, r.shard, r.payer, r.nonce, why);
                continue;
            }
            receiptUsed[key] = true;
            room -= r.amount;
            _credit(r.round, r.shard, r.amount);
            applied++;
            emit Credited(r.round, r.shard, r.amount, r.payer, r.nonce);
        }
    }

    /// @notice Settle an x402 `exact` payment here and credit `shard` in the same call (R-REV-8,
    /// ADR-0026). The reporter submits the client's EIP-3009 authorization with `to` = this
    /// contract: the token checks the signature, the validity window and the nonce, and this
    /// contract checks that exactly `a.value` arrived. Recipient and amount are proven, not
    /// attested; only the choice of shard rests on the reporter, which is why this is
    /// reporter-only (anyone holding the signed payload could otherwise pick the shard). A third
    /// party that front-runs by submitting the same authorization to the token directly makes
    /// this call revert (nonce used); the funds still arrived here unallocated, and the receipt
    /// path (`credit`) attributes them, total-capped (REV-1).
    function settle(uint64 id, bytes32 shard, Authorization calldata a, bytes calldata signature)
        external
        nonReentrant
        onlyReporter
    {
        if (!books[id][shard].registered) revert NotRegistered();
        if (a.value > type(uint128).max) revert BadFields();
        bytes32 key = keccak256(abi.encode(a.from, a.nonce));
        if (receiptUsed[key]) revert AlreadyCredited();
        uint256 before = token.balanceOf(address(this));
        token.transferWithAuthorization(
            a.from, address(this), a.value, a.validAfter, a.validBefore, a.nonce, signature
        );
        if (token.balanceOf(address(this)) - before != a.value) revert NotReceived();
        // The (payer, nonce) can never be attributed again, by `credit` or here.
        receiptUsed[key] = true;
        _credit(id, shard, uint128(a.value));
        emit Settled(id, shard, a.from, a.nonce, uint128(a.value));
        emit Credited(id, shard, uint128(a.value), a.from, a.nonce);
    }

    /// @notice Pay for a shard directly (no x402, no reporter): the fee is split at once.
    function pay(uint64 id, bytes32 shard, uint128 amount) external nonReentrant {
        if (!books[id][shard].registered) revert NotRegistered();
        if (!token.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
        _credit(id, shard, amount);
        emit Credited(id, shard, amount, msg.sender, bytes32(0));
    }

    /// @notice Move what `who` earned on a shard into its withdrawable balance. Anyone may call.
    function claim(uint64 id, bytes32 shard, address who) external returns (uint256 owed) {
        Book storage b = books[id][shard];
        uint8 role = roleOf[id][shard][who];
        if (role & MINER != 0) {
            owed += b.perMiner - claimedMiner[id][shard][who];
            claimedMiner[id][shard][who] = b.perMiner;
        }
        if (role & VALIDATOR != 0) {
            owed += b.perValidator - claimedValidator[id][shard][who];
            claimedValidator[id][shard][who] = b.perValidator;
        }
        balanceOf[who] += owed;
        emit Claimed(id, shard, who, owed);
    }

    /// @notice Withdraw the caller's balance (pull payment).
    function withdraw() external nonReentrant {
        uint256 amount = balanceOf[msg.sender];
        balanceOf[msg.sender] = 0;
        paidOut += amount;
        if (!token.transfer(msg.sender, amount)) revert TransferFailed();
        emit Withdrawn(msg.sender, amount);
    }

    /// @notice Tokens received but not yet attributed to a shard.
    function unallocated() public view returns (uint256) {
        return token.balanceOf(address(this)) + paidOut - allocated;
    }

    /// @notice The split of `amount` for given heads (R-REV-2), as the Rust `revenue::split`.
    function split(uint256 amount, uint32 miners, uint32 validators, bool hasLabel)
        public
        view
        returns (uint256 perMiner, uint256 perValidator, uint256 label, uint256 toTreasury)
    {
        uint256 mc = amount * minersBps / BPS;
        uint256 vc = amount * validatorsBps / BPS;
        label = amount * labelsBps / BPS;
        if (validators == 0) {
            mc += vc;
            vc = 0;
        }
        if (!hasLabel) {
            mc += label;
            label = 0;
        }
        perMiner = miners == 0 ? 0 : mc / miners;
        perValidator = validators == 0 ? 0 : vc / validators;
        toTreasury = amount - perMiner * miners - perValidator * validators - label;
    }

    function _credit(uint64 id, bytes32 shard, uint128 amount) internal {
        Book storage b = books[id][shard];
        if (!b.registered) revert NotRegistered();
        (uint256 pm, uint256 pv, uint256 lc, uint256 tr) =
            split(amount, b.miners, b.validators, b.labelSet != bytes32(0));
        // Each part is at most `amount`, a uint128: the casts cannot truncate.
        b.perMiner += uint128(pm);
        b.perValidator += uint128(pv);
        b.revenue += amount;
        if (lc != 0) {
            address payee = labelPayee[b.labelSet];
            if (payee == address(0)) labelHeld[b.labelSet] += lc;
            else balanceOf[payee] += lc;
        }
        balanceOf[treasury] += tr;
        allocated += amount;
    }

    function _accept(address a, uint64 at) internal {
        if (at > lastAccepted[a]) lastAccepted[a] = at;
    }
}
