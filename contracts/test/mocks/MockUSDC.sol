// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @notice Test-only ERC-20 (6 decimals, free mint). Never deployed to a live network.
contract MockUSDC {
    string public constant name = "Mock USDC";
    string public constant symbol = "mUSDC";
    uint8 public constant decimals = 6;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external virtual returns (bool) {
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

interface IWithdraw {
    function withdraw() external;
}

/// @notice A token whose transfer calls back into the settlement contract, as a hostile
/// token (or an ERC-777 style hook) would. Used to prove withdraw cannot be reentered.
contract ReentrantToken is MockUSDC {
    IWithdraw public target;
    bool public armed;

    function arm(IWithdraw t) external {
        target = t;
        armed = true;
    }

    function transfer(address to, uint256 amount) external override returns (bool) {
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        if (armed) {
            armed = false;
            target.withdraw();
        }
        return true;
    }
}
