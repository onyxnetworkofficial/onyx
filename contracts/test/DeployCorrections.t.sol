// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeployCorrections} from "../script/DeployCorrections.s.sol";
import {OnyxCorrections} from "../src/OnyxCorrections.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";
import {IStakeToken} from "../src/IOnyxMarkets.sol";

/// The corrections deploy script (CL7), run against the local test EVM; no network.
contract DeployCorrectionsTest is Test {
    DeployCorrections internal d;
    OnyxRegistry internal reg = OnyxRegistry(address(0x5E6));
    IStakeToken internal tok = IStakeToken(address(0x70C));

    function setUp() public {
        d = new DeployCorrections();
    }

    function test_DeploysWiredWithThePlaceholders() public {
        OnyxCorrections c = d.deploy(tok, reg, address(0x7EA5));
        assertEq(address(c.registry()), address(reg));
        assertEq(address(c.token()), address(tok));
        assertEq(c.treasury(), address(0x7EA5));
        assertEq(c.owner(), address(d), "the broadcaster owns it");
        OnyxCorrections.Params memory p = d.defaults();
        assertEq(c.openFee(), p.openFee);
        assertEq(c.feePpm(), p.feePpm);
        assertEq(c.minTradingBlocks(), p.minTradingBlocks);
        assertEq(c.minRecheckBlocks(), p.minRecheckBlocks);
        assertEq(c.maxMarketBlocks(), p.maxMarketBlocks);
        assertEq(c.graceBlocks(), p.graceBlocks);
        assertEq(c.marketCount(), 0);
    }

    function test_DeploysOnBaseSepoliaOnly() public {
        vm.chainId(84_532);
        d.deploy(tok, reg, address(0x7EA5));
        vm.chainId(8_453);
        vm.expectRevert(abi.encodeWithSelector(DeployCorrections.WrongChain.selector, 8_453));
        d.deploy(tok, reg, address(0x7EA5));
    }
}
