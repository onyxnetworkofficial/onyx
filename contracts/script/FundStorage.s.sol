// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";

interface IMintable {
    function mint(address to, uint256 amount) external;
    function balanceOf(address who) external view returns (uint256);
}

/// ROADMAP S15 (docs/ops/runbooks/deploy-storage.md): the deployer gives each storage node operator
/// (and the deal payer) test ETH for gas and tONYX for its bond or deposit. Test networks only:
/// Base Sepolia or anvil. The key comes from the environment (BASE_SEPOLIA_PRIVATE_KEY), never
/// from a command line.
///   ONYX_FUND_TO     comma-separated addresses
///   ONYX_FUND_WEI    test ETH (wei) each address is topped up to
///   ONYX_FUND_TONYX  tONYX (base units) each address is topped up to (a rerun mints nothing more)
///   ONYX_STORAGE_TOKEN  the tONYX token (its minter is the deployer)
contract FundStorage is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;
    /// At most this much test ETH per address per run (a typo never empties the deployer).
    uint256 public constant MAX_WEI = 1e15;
    /// At most this much tONYX per address.
    uint256 public constant MAX_TONYX = 1000e18;

    error WrongChain(uint256 chainId);
    error TooMuch(uint256 amount);

    function run() external {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address[] memory to = vm.envAddress("ONYX_FUND_TO", ",");
        uint256 topUp = vm.envUint("ONYX_FUND_WEI");
        uint256 tonyx = vm.envUint("ONYX_FUND_TONYX");
        IMintable token = IMintable(vm.envAddress("ONYX_STORAGE_TOKEN"));
        if (topUp > MAX_WEI) revert TooMuch(topUp);
        if (tonyx > MAX_TONYX) revert TooMuch(tonyx);
        vm.startBroadcast(key);
        for (uint256 i = 0; i < to.length; i++) {
            uint256 have = to[i].balance;
            if (have < topUp) payable(to[i]).transfer(topUp - have);
            uint256 held = token.balanceOf(to[i]);
            if (held < tonyx) token.mint(to[i], tonyx - held);
        }
        vm.stopBroadcast();
    }
}
