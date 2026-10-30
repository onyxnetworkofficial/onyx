// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxRevenue} from "./OnyxRevenue.sol";
import {IPrepaidToken} from "./OnyxPrepaid.sol";

/// @notice The token calls the credits ledger makes: the prepaid token's, plus EIP-3009
/// `receiveWithAuthorization` (FiatToken v2.2 form, `bytes` signature), which only the payee
/// (`to`) may submit, so a signed deposit cannot be front-run into an uncredited transfer.
interface ICreditsToken is IPrepaidToken {
    function receiveWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        bytes calldata signature
    ) external;
}

/// @title OnyxPrepaidV2
/// @notice TESTNET ONLY. The credits ledger (ADR-0056, contributor loop CL6): one spendable
/// balance per account (an EVM address), the prepaid balance of `OnyxPrepaid` (ADR-0046) with
/// three additions. The voucher type, the EIP-712 domain name and
/// version, redemption, allocation and the exit rules are v1's unchanged, so the API's header
/// and the Rust voucher code serve both; a voucher is bound to its contract by
/// `verifyingContract`, so a v1 voucher is worthless here and the reverse.
///
/// 1. `depositFor`: anyone pays the asset in for an account. The origin tag is fixed by the
///    payer, never chosen freely: `revenue` credits only "earned" (2, `OnyxRevenueV2
///    .withdrawToCredits`), a source the owner named credits only its own tag (3 refund,
///    4 payout), everyone else only "deposit" (1). It never touches `exitAt`: nobody can keep
///    another account from leaving (CL6-6).
/// 2. `depositWithAuthorization`: a deposit from a signed EIP-3009 authorization, so an account
///    with no ETH deposits through a relayer. The credit always goes to the signer (`from`), and
///    it never cancels an exit either (an old signed authorization must not undo a later exit).
/// 3. `payRelayer`: the reporter pays redeemed relay fees (gas lines, ADR-0056 item 4) from the
///    pool to the immutable `relayer`, never above the pool and never above `maxRelayPerDay` in
///    one UTC day.
///
/// No credit is created from nothing (R-CR-1): every balance is asset that arrived through one
/// of the three deposit paths; there is no mint, grant, faucet or transfer between accounts
/// (R-CR-6). Conservation: the token held here equals the sum of `deposited - redeemed` over
/// every account plus `pool`.
contract OnyxPrepaidV2 {
    uint256 internal constant ANVIL = 31_337;
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 public constant MAX_BATCH = 64;
    /// secp256k1n / 2: signatures with a larger `s` are refused (no malleable copies).
    uint256 internal constant HALF_N =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;
    bytes32 public constant VOUCHER_TYPEHASH =
        keccak256("Voucher(address buyer,uint128 cumulative)");
    bytes32 internal constant DOMAIN_TYPEHASH = keccak256(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
    );
    uint8 public constant ORIGIN_DEPOSIT = 1;
    uint8 public constant ORIGIN_EARNED = 2;
    uint8 public constant ORIGIN_REFUND = 3;
    uint8 public constant ORIGIN_PAYOUT = 4;

    error WrongChain(uint256 chainId);
    error BadParams();
    error NotOwner();
    error NotReporter();
    error BadAmount();
    error BadVoucher();
    error BadOrigin();
    error TooMany();
    error OverPool();
    error OverRelayCap();
    error NoExit();
    error TooEarly();
    error TransferFailed();
    error NotReceived();
    error Reentrancy();

    /// Every arrival of credit: a deposit (1), an earned share (2), a refund (3) or a payout (4).
    event Credited(address indexed account, uint128 amount, uint8 origin);
    event Redeemed(address indexed buyer, uint128 cumulative, uint128 amount);
    event Allocated(uint64 indexed round, bytes32 indexed shard, uint128 amount);
    event RelayerPaid(address indexed relayer, uint128 amount);
    event ExitRequested(address indexed buyer, uint64 at);
    event Withdrawn(address indexed buyer, uint128 amount);
    event ReporterSet(address reporter);
    event SourceSet(address indexed source, uint8 origin);

    struct Account {
        /// Everything ever credited, less what was withdrawn.
        uint128 deposited;
        /// The highest voucher total redeemed.
        uint128 redeemed;
        /// When a requested exit may be withdrawn (0: none).
        uint64 exitAt;
    }

    struct Voucher {
        address buyer;
        uint128 cumulative;
        bytes signature;
    }

    struct Item {
        uint64 round;
        bytes32 shard;
        uint128 amount;
    }

    ICreditsToken public immutable token;
    OnyxRevenue public immutable revenue;
    address public immutable owner;
    /// The relayer's own address (ADR-0056 item 4): the only payee of `payRelayer`. Never the
    /// reporter or the owner.
    address public immutable relayer;
    uint64 public immutable exitDelay;
    /// The most `payRelayer` moves in one UTC day: a leaked reporter key drains at most this a
    /// day through the relay path.
    uint128 public immutable maxRelayPerDay;
    bytes32 public immutable domainSeparator;
    address public reporter;
    mapping(address => Account) public accounts;
    /// The origin a payer other than `revenue` may credit with `depositFor` (0: deposit only).
    mapping(address => uint8) public sourceOrigin;
    /// Redeemed money not yet allocated to a book or paid to the relayer.
    uint256 public pool;
    /// The UTC day (`block.timestamp / 1 days`) of `relayPaidToday`.
    uint64 public relayDay;
    uint128 public relayPaidToday;
    uint256 private _lock = 1;

    constructor(
        ICreditsToken token_,
        OnyxRevenue revenue_,
        address reporter_,
        address relayer_,
        uint64 exitDelay_,
        uint128 maxRelayPerDay_
    ) {
        if (block.chainid != ANVIL && block.chainid != BASE_SEPOLIA) {
            revert WrongChain(block.chainid);
        }
        if (
            address(token_) == address(0) || address(revenue_) == address(0)
                || reporter_ == address(0) || relayer_ == address(0) || relayer_ == reporter_
                || relayer_ == msg.sender || exitDelay_ < 1 hours || exitDelay_ > 30 days
                || maxRelayPerDay_ == 0
        ) revert BadParams();
        token = token_;
        revenue = revenue_;
        owner = msg.sender;
        reporter = reporter_;
        relayer = relayer_;
        exitDelay = exitDelay_;
        maxRelayPerDay = maxRelayPerDay_;
        domainSeparator = keccak256(
            abi.encode(
                DOMAIN_TYPEHASH,
                keccak256("OnyxPrepaid"),
                keccak256("1"),
                block.chainid,
                address(this)
            )
        );
    }

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    modifier onlyReporter() {
        if (msg.sender != reporter) revert NotReporter();
        _;
    }

    /// @notice The owner (treasury) rotates a leaked reporter key out. Never to the relayer.
    function setReporter(address r) external {
        if (msg.sender != owner) revert NotOwner();
        if (r == address(0) || r == relayer) revert BadParams();
        reporter = r;
        emit ReporterSet(r);
    }

    /// @notice The owner names a contract that pays refunds (3) or payouts (4) in with
    /// `depositFor` (ADR-0055 storage refunds, ADR-0057 market payouts); 0 removes it. The tag
    /// is for the ledger's display and anti-gaming rules: it never changes what is spendable.
    function setSource(address source, uint8 origin) external {
        if (msg.sender != owner) revert NotOwner();
        if (
            source == address(0) || source == address(revenue)
                || (origin != 0 && origin != ORIGIN_REFUND && origin != ORIGIN_PAYOUT)
        ) revert BadParams();
        sourceOrigin[source] = origin;
        emit SourceSet(source, origin);
    }

    /// @notice The origin tag `payer` credits with `depositFor`.
    function originOf(address payer) public view returns (uint8) {
        if (payer == address(revenue)) return ORIGIN_EARNED;
        uint8 o = sourceOrigin[payer];
        return o == 0 ? ORIGIN_DEPOSIT : o;
    }

    /// @notice The EIP-712 digest a buyer signs for a running total.
    function voucherDigest(address buyer, uint128 cumulative) public view returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "\x19\x01",
                domainSeparator,
                keccak256(abi.encode(VOUCHER_TYPEHASH, buyer, cumulative))
            )
        );
    }

    /// @notice Whether `signature` is `buyer`'s signature of the total `cumulative`.
    function signedBy(address buyer, uint128 cumulative, bytes memory signature)
        public
        view
        returns (bool)
    {
        if (signature.length != 65 || buyer == address(0)) return false;
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := mload(add(signature, 32))
            s := mload(add(signature, 64))
            v := byte(0, mload(add(signature, 96)))
        }
        if (uint256(s) > HALF_N || (v != 27 && v != 28)) return false;
        return ecrecover(voucherDigest(buyer, cumulative), v, r, s) == buyer;
    }

    /// @notice Deposit `amount` (the token's smallest unit) for yourself. Only this call cancels
    /// your own requested exit.
    function deposit(uint128 amount) external nonReentrant {
        if (amount == 0) revert BadAmount();
        Account storage a = accounts[msg.sender];
        a.deposited += amount;
        a.exitAt = 0;
        emit Credited(msg.sender, amount, ORIGIN_DEPOSIT);
        if (!token.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
    }

    /// @notice Pay `amount` in for `account`, tagged `origin`, which must be the payer's own
    /// (`originOf`). Never touches the account's exit.
    function depositFor(address account, uint128 amount, uint8 origin) external nonReentrant {
        if (account == address(0) || amount == 0) revert BadAmount();
        if (origin != originOf(msg.sender)) revert BadOrigin();
        accounts[account].deposited += amount;
        emit Credited(account, amount, origin);
        if (!token.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
    }

    /// @notice Deposit from `from`'s signed EIP-3009 `ReceiveWithAuthorization` to this contract:
    /// anyone (a relayer) may submit it; the token checks the signature, window and nonce; the
    /// credit goes to `from` only, and exactly the amount that arrived. Never touches the exit.
    function depositWithAuthorization(
        address from,
        uint128 amount,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        bytes calldata signature
    ) external nonReentrant {
        if (from == address(0) || amount == 0) revert BadAmount();
        uint256 before = token.balanceOf(address(this));
        token.receiveWithAuthorization(
            from, address(this), amount, validAfter, validBefore, nonce, signature
        );
        if (token.balanceOf(address(this)) != before + amount) revert NotReceived();
        accounts[from].deposited += amount;
        emit Credited(from, amount, ORIGIN_DEPOSIT);
    }

    /// @notice Redeem the latest voucher of each buyer in `vs` into the pool. A voucher that is not
    /// signed by its buyer, not above what was redeemed, or above what the buyer holds, reverts
    /// the batch: the reporter checks vouchers when it takes them (`onyx_api::prepaid`).
    function redeem(Voucher[] calldata vs) external onlyReporter {
        if (vs.length > MAX_BATCH) revert TooMany();
        for (uint256 i = 0; i < vs.length; i++) {
            Voucher calldata x = vs[i];
            Account storage a = accounts[x.buyer];
            if (x.cumulative <= a.redeemed || x.cumulative > a.deposited) revert BadAmount();
            if (!signedBy(x.buyer, x.cumulative, x.signature)) revert BadVoucher();
            uint128 amount = x.cumulative - a.redeemed;
            a.redeemed = x.cumulative;
            pool += amount;
            emit Redeemed(x.buyer, x.cumulative, amount);
        }
    }

    /// @notice Pay pooled money to the books that were bought (`OnyxRevenue.pay`): no buyer is
    /// named here.
    function allocate(Item[] calldata items) external nonReentrant onlyReporter {
        if (items.length > MAX_BATCH) revert TooMany();
        uint256 total;
        for (uint256 i = 0; i < items.length; i++) {
            if (items[i].amount == 0) revert BadAmount();
            total += items[i].amount;
        }
        if (total > pool) revert OverPool();
        pool -= total;
        if (!token.approve(address(revenue), total)) revert TransferFailed();
        for (uint256 i = 0; i < items.length; i++) {
            revenue.pay(items[i].round, items[i].shard, items[i].amount);
            emit Allocated(items[i].round, items[i].shard, items[i].amount);
        }
    }

    /// @notice Pay redeemed relay fees (gas lines) to the relayer: never above the pool, never
    /// above `maxRelayPerDay` in one UTC day, never to anyone but `relayer`.
    function payRelayer(uint128 amount) external nonReentrant onlyReporter {
        if (amount == 0) revert BadAmount();
        if (amount > pool) revert OverPool();
        uint64 day = uint64(block.timestamp / 1 days);
        uint128 today = day == relayDay ? relayPaidToday : 0;
        if (uint256(today) + amount > maxRelayPerDay) revert OverRelayCap();
        relayDay = day;
        relayPaidToday = today + amount;
        pool -= amount;
        emit RelayerPaid(relayer, amount);
        if (!token.transfer(relayer, amount)) revert TransferFailed();
    }

    /// @notice Start leaving: after `exitDelay` the unspent balance can be withdrawn.
    function requestExit() external {
        Account storage a = accounts[msg.sender];
        if (a.deposited == a.redeemed) revert BadAmount();
        a.exitAt = uint64(block.timestamp) + exitDelay;
        emit ExitRequested(msg.sender, a.exitAt);
    }

    /// @notice Withdraw the unspent balance after the exit delay. Vouchers up to what was redeemed
    /// stay spent; later vouchers are worthless until the next deposit.
    function withdraw() external nonReentrant {
        Account storage a = accounts[msg.sender];
        if (a.exitAt == 0) revert NoExit();
        if (block.timestamp < a.exitAt) revert TooEarly();
        uint128 amount = a.deposited - a.redeemed;
        a.deposited = a.redeemed;
        a.exitAt = 0;
        emit Withdrawn(msg.sender, amount);
        if (!token.transfer(msg.sender, amount)) revert TransferFailed();
    }

    /// @notice What `buyer` can still spend (0 while an exit is pending: the API takes no voucher).
    function spendable(address buyer) external view returns (uint128) {
        Account storage a = accounts[buyer];
        return a.exitAt == 0 ? a.deposited - a.redeemed : 0;
    }
}
