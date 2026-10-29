// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxRevenue, IRevenueToken} from "./OnyxRevenue.sol";

/// @notice The token calls the prepaid contract makes: the revenue token's, plus `approve` for
/// `OnyxRevenue.pay`.
interface IPrepaidToken is IRevenueToken {
    function approve(address spender, uint256 amount) external returns (bool);
}

/// @title OnyxPrepaid
/// @notice TESTNET ONLY. Prepaid buyer balances with batched settlement (partner tracker T7, a partner's
/// questions 7 and 8; docs/spec/prepaid.md). A buyer deposits once; each query is paid off chain
/// with a signed voucher for the buyer's running total ("I have spent up to X"); the API's
/// reporter redeems the latest voucher of many buyers in one call, and separately allocates the
/// pooled money to the shards and observation books that were bought, through `OnyxRevenue.pay`.
///
/// What the chain shows: each buyer's deposits, redeemed totals and withdrawals; and, apart from
/// them, which books were paid how much. It does not show which buyer bought which row, nor each
/// query. It still shows that an address is a buyer and roughly how much it spends: use a fresh
/// address per deposit for more.
///
/// Trust: the reporter can redeem no more than a buyer signed, and never more than the buyer
/// deposited; as with `OnyxRevenue.credit`, the reporter is trusted for the attribution of pooled
/// money to books (REV-1). A buyer leaves with `requestExit`, then `withdraw` after `exitDelay`:
/// the delay gives the reporter time to redeem the last voucher the buyer signed.
contract OnyxPrepaid {
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

    error WrongChain(uint256 chainId);
    error BadParams();
    error NotOwner();
    error NotReporter();
    error BadAmount();
    error BadVoucher();
    error TooMany();
    error OverPool();
    error NoExit();
    error TooEarly();
    error TransferFailed();
    error Reentrancy();

    event Deposited(address indexed buyer, uint128 amount, uint128 deposited);
    event Redeemed(address indexed buyer, uint128 cumulative, uint128 amount);
    event Allocated(uint64 indexed round, bytes32 indexed shard, uint128 amount);
    event ExitRequested(address indexed buyer, uint64 at);
    event Withdrawn(address indexed buyer, uint128 amount);
    event ReporterSet(address reporter);

    struct Account {
        /// Everything ever deposited, less what was withdrawn.
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

    IPrepaidToken public immutable token;
    OnyxRevenue public immutable revenue;
    address public immutable owner;
    uint64 public immutable exitDelay;
    bytes32 public immutable domainSeparator;
    address public reporter;
    mapping(address => Account) public accounts;
    /// Redeemed money not yet allocated to a book.
    uint256 public pool;
    uint256 private _lock = 1;

    constructor(IPrepaidToken token_, OnyxRevenue revenue_, address reporter_, uint64 exitDelay_) {
        if (block.chainid != ANVIL && block.chainid != BASE_SEPOLIA) {
            revert WrongChain(block.chainid);
        }
        if (
            address(token_) == address(0) || address(revenue_) == address(0)
                || reporter_ == address(0) || exitDelay_ < 1 hours || exitDelay_ > 30 days
        ) revert BadParams();
        token = token_;
        revenue = revenue_;
        owner = msg.sender;
        reporter = reporter_;
        exitDelay = exitDelay_;
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

    /// @notice The owner (treasury) rotates a leaked reporter key out, as in `OnyxRevenue`.
    function setReporter(address r) external {
        if (msg.sender != owner) revert NotOwner();
        if (r == address(0)) revert BadParams();
        reporter = r;
        emit ReporterSet(r);
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

    /// @notice Deposit `amount` (the token's smallest unit). A deposit cancels a requested exit.
    function deposit(uint128 amount) external nonReentrant {
        if (amount == 0) revert BadAmount();
        Account storage a = accounts[msg.sender];
        a.deposited += amount;
        a.exitAt = 0;
        emit Deposited(msg.sender, amount, a.deposited);
        if (!token.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
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
