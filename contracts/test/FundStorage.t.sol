// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {FundStorage} from "../script/FundStorage.s.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";

/// The storage network's funding script (ROADMAP S15), run against the local test EVM; no network.
contract FundStorageTest is Test {
    function test_topsUpToTheTargetAndMints() public {
        uint256 pk = 0xA11CE;
        address dep = vm.addr(pk);
        OnyxTestToken tok = new OnyxTestToken(dep);
        vm.deal(dep, 1 ether);
        address a = address(0xA1A1);
        address b = address(0xB2B2);
        vm.deal(b, 3e13); // b already holds some: topped up to the target only
        vm.setEnv("BASE_SEPOLIA_PRIVATE_KEY", vm.toString(pk));
        vm.setEnv("ONYX_FUND_TO", string.concat(vm.toString(a), ",", vm.toString(b)));
        vm.setEnv("ONYX_FUND_WEI", "100000000000000");
        vm.setEnv("ONYX_FUND_TONYX", "20000000000000000000");
        vm.setEnv("ONYX_STORAGE_TOKEN", vm.toString(address(tok)));
        new FundStorage().run();
        assertEq(a.balance, 1e14);
        assertEq(b.balance, 1e14);
        assertEq(dep.balance, 1 ether - 1e14 - (1e14 - 3e13));
        assertEq(tok.balanceOf(a), 20e18);
        assertEq(tok.balanceOf(b), 20e18);
        // A rerun tops up to the same targets: nothing more is sent or minted.
        new FundStorage().run();
        assertEq(a.balance, 1e14);
        assertEq(tok.balanceOf(a), 20e18);
        assertEq(dep.balance, 1 ether - 1e14 - (1e14 - 3e13));
        // A typo never empties the deployer: more than 0.001 test ETH per address is refused.
        vm.setEnv("ONYX_FUND_WEI", "1000000000000001");
        FundStorage f = new FundStorage();
        vm.expectRevert(abi.encodeWithSelector(FundStorage.TooMuch.selector, uint256(1e15 + 1)));
        f.run();
        vm.setEnv("ONYX_FUND_WEI", "100000000000000");
        vm.setEnv("ONYX_FUND_TONYX", "1000000000000000000001");
        vm.expectRevert(abi.encodeWithSelector(FundStorage.TooMuch.selector, uint256(1000e18 + 1)));
        f.run();
    }
}
