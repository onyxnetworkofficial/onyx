// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title OnyxTestToken
/// @notice "tONYX test token": a plain ERC-20 with NO value, for the S3/S6 prototype only
/// (ADR-0042 item 2). It deploys and mints only on a local anvil (31337) or Base Sepolia
/// (84532); one minter, set at deploy. No hooks, no fee on transfer, no rebase.
contract OnyxTestToken {
    string public constant name = "tONYX test token";
    string public constant symbol = "tONYX";
    uint8 public constant decimals = 18;
    uint256 internal constant ANVIL = 31_337;
    uint256 internal constant BASE_SEPOLIA = 84_532;

    address public immutable minter;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    error WrongChain(uint256 chainId);
    error NotMinter();
    error Insufficient();
    error ZeroAddress();

    constructor(address minter_) {
        _chainOk();
        if (minter_ == address(0)) revert ZeroAddress();
        minter = minter_;
    }

    function _chainOk() internal view {
        if (block.chainid != ANVIL && block.chainid != BASE_SEPOLIA) {
            revert WrongChain(block.chainid);
        }
    }

    /// @notice Mint test tokens (minter only, testnet or anvil only).
    function mint(address to, uint256 amount) external {
        _chainOk();
        if (msg.sender != minter) revert NotMinter();
        if (to == address(0)) revert ZeroAddress();
        totalSupply += amount;
        balanceOf[to] += amount;
        emit Transfer(address(0), to, amount);
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 a = allowance[from][msg.sender];
        if (a != type(uint256).max) {
            if (a < amount) revert Insufficient();
            allowance[from][msg.sender] = a - amount;
        }
        _move(from, to, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) internal {
        if (to == address(0)) revert ZeroAddress();
        uint256 b = balanceOf[from];
        if (b < amount) revert Insufficient();
        balanceOf[from] = b - amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}
