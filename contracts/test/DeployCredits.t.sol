// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeployCredits} from "../script/DeployCredits.s.sol";
import {OnyxRevenueV2} from "../src/OnyxRevenueV2.sol";
import {OnyxPrepaidV2} from "../src/OnyxPrepaidV2.sol";
import {MockUSDC3009Receive} from "./mocks/MockUSDC3009Receive.sol";
import {MockLedgerView} from "./OnyxRevenue.t.sol";

/// The CL6 deploy script (docs/ops/runbooks/deploy-credits.md): both contracts wired to each other,
/// the token and the settlement; the relayer's key is its own; Base Sepolia or anvil only.
contract DeployCreditsTest is Test {
    DeployCredits internal s = new DeployCredits();

    function _p() internal returns (DeployCredits.Params memory) {
        return DeployCredits.Params({
            usdc: address(new MockUSDC3009Receive()),
            settlement: address(new MockLedgerView()),
            reporter: address(0x4E9),
            treasury: address(0x7EA5),
            relayer: address(0x2E1A),
            exitDelay: 1 days,
            relayCap: 1_000_000,
            bps: [uint16(5_000), 2_000, 1_000, 2_000]
        });
    }

    function test_DeploysBothWiredOnAnvilAndBaseSepolia() public {
        for (uint256 i = 0; i < 2; i++) {
            vm.chainId(i == 0 ? 31_337 : 84_532);
            DeployCredits.Params memory p = _p();
            (OnyxRevenueV2 rev, OnyxPrepaidV2 pre) = s.deploy(p, address(s));
            assertEq(address(pre.revenue()), address(rev));
            assertEq(address(pre.token()), p.usdc);
            assertEq(address(rev.token()), p.usdc);
            assertEq(address(rev.ledger()), p.settlement);
            assertEq(pre.reporter(), p.reporter);
            assertEq(rev.reporter(), p.reporter);
            assertEq(rev.treasury(), p.treasury);
            assertEq(pre.relayer(), p.relayer);
            assertEq(pre.owner(), address(s));
            assertEq(pre.exitDelay(), 1 days);
            assertEq(pre.maxRelayPerDay(), 1_000_000);
            assertEq(pre.originOf(address(rev)), 2);
        }
    }

    function test_RefusesOtherChainsAndASharedRelayerKey() public {
        DeployCredits.Params memory p = _p();
        vm.chainId(8453);
        vm.expectRevert(abi.encodeWithSelector(DeployCredits.WrongChain.selector, 8453));
        s.deploy(p, address(s));
        vm.chainId(84_532);
        p.relayer = p.reporter;
        vm.expectRevert(DeployCredits.RelayerNotOwnKey.selector);
        s.deploy(p, address(s));
        p.relayer = p.treasury;
        vm.expectRevert(DeployCredits.RelayerNotOwnKey.selector);
        s.deploy(p, address(s));
        p.relayer = address(s);
        vm.expectRevert(DeployCredits.RelayerNotOwnKey.selector);
        s.deploy(p, address(s));
        // A split without the treasury floor is refused by OnyxRevenueV2.
        p.relayer = address(0x2E1A);
        p.bps = [uint16(6_000), 3_000, 1_000, 0];
        vm.expectRevert();
        s.deploy(p, address(s));
    }
}
