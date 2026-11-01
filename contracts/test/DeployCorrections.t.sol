// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeployCorrections} from "../script/DeployCorrections.s.sol";
import {OnyxCorrections} from "../src/OnyxCorrections.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {IStakeToken} from "../src/IOnyxMarkets.sol";
import {CoordinatorLedger} from "./OnyxObservedFacts.t.sol";

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

    /// The owner's steps 4 and 5 (`pairRecheck`): register the recheck metric and pair it, once;
    /// a second run sends nothing; another template or tolerance is refused, never overwritten.
    function test_PairRegistersTheRecheckOnceAndRefusesAnotherOne() public {
        OnyxRegistry r = new OnyxRegistry(
            address(d), IRegistryLedger(address(new CoordinatorLedger(address(0xC0))))
        );
        bytes32 m = keccak256("us.treasury.tga_total_deposits");
        bytes32 rm = keccak256("us.treasury.tga_total_deposits.recheck");
        vm.prank(address(d));
        r.registerMetric(m, OnyxRegistry.Kind.DatedFeed, keccak256("template"), 0);
        OnyxCorrections c = d.deploy(tok, r, address(0x7EA5));
        d.pair(r, c, m, rm, keccak256("recheck template"), 0);
        (OnyxRegistry.Kind k, bytes32 t,) = r.metrics(rm);
        assertEq(uint8(k), uint8(OnyxRegistry.Kind.DatedFeed));
        assertEq(t, keccak256("recheck template"));
        (bytes32 paired, uint128 tol) = c.pairs(m);
        assertEq(paired, rm);
        assertEq(tol, 0);
        d.pair(r, c, m, rm, keccak256("recheck template"), 0);
        vm.expectRevert(
            abi.encodeWithSelector(
                DeployCorrections.RecheckRegisteredOtherwise.selector, keccak256("recheck template")
            )
        );
        d.pair(r, c, m, rm, keccak256("another template"), 0);
        vm.expectRevert(abi.encodeWithSelector(DeployCorrections.PairedOtherwise.selector, rm, 0));
        d.pair(r, c, m, rm, keccak256("recheck template"), 1);
    }

    function test_DeploysOnBaseSepoliaOnly() public {
        vm.chainId(84_532);
        d.deploy(tok, reg, address(0x7EA5));
        vm.chainId(8_453);
        vm.expectRevert(abi.encodeWithSelector(DeployCorrections.WrongChain.selector, 8_453));
        d.deploy(tok, reg, address(0x7EA5));
    }
}
