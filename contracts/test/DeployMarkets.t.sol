// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeployMarkets} from "../script/DeployMarkets.s.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxFutures} from "../src/OnyxFutures.sol";

/// The markets deploy script (NS11), run against the local test EVM; no network.
contract DeployMarketsTest is Test {
    DeployMarkets internal d;
    OnyxRegistry internal reg = OnyxRegistry(address(0x5E6));

    address internal ledger = address(0x5E77);

    function setUp() public {
        d = new DeployMarkets();
        vm.mockCall(address(reg), abi.encodeWithSignature("ledger()"), abi.encode(ledger));
    }

    function test_DeploysEveryContractWired() public {
        DeployMarkets.Markets memory m = d.deploy(address(this), reg);
        assertEq(m.token.minter(), address(this), "the admin mints tONYX");
        assertEq(m.reobserve.stakes(), address(m.stakes), "re-observation bound to the stakes");
        assertEq(address(m.panel.stakes()), address(m.stakes), "the panel bound to the stakes");
        assertEq(address(m.stakes.oracle()), address(m.panel), "the drawn panel is the fallback");
        assertEq(address(m.futures.facts()), address(m.facts), "futures read registry facts");
        assertEq(address(m.facts.registry()), address(reg));
        assertEq(m.facts.owner(), address(this), "the admin lists facts");
        assertEq(m.futures.owner(), address(d), "the broadcaster owns the futures");
        assertEq(address(m.futures.oracle()), address(m.panel), "the arbiter may not bet");
        assertEq(m.panel.panelSize(), d.PANEL_SIZE());
        assertEq(m.panel.majority(), d.MAJORITY());
        // The placeholder defaults, unchanged (ADR-0042 item 10).
        assertEq(abi.encode(m.stakes.params(0)), abi.encode(m.stakes.defaultParams()));
        assertEq(abi.encode(m.futures.params(0)), abi.encode(m.futures.defaultParams()));
        assertTrue(m.stakes.paramsValid(m.stakes.params(0)));
        // THREATS LC16-1, LC16-2, NL6-12: delayed observer changes, validator-only seats, guarded
        // futures.
        OnyxStakes.Params memory sp = m.stakes.params(0);
        assertGt(m.reobserve.changeDelay(), sp.commitWindow + sp.revealWindow, "above a task");
        assertEq(address(m.panel.gate()), ledger, "seats from the settlement's validators");
        assertEq(address(m.futures.guard()), address(m.facts), "the futures are guarded");
    }

    function test_DeploysOnBaseSepoliaOnly() public {
        vm.chainId(84_532);
        d.deploy(address(this), reg);
        vm.chainId(8_453);
        vm.expectRevert(abi.encodeWithSelector(DeployMarkets.WrongChain.selector, 8_453));
        d.deploy(address(this), reg);
    }
}
