// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {Deploy} from "../script/Deploy.s.sol";
import {OnyxDisputes} from "../src/OnyxDisputes.sol";
import {MockUSDC} from "./mocks/MockUSDC.sol";
import {OnyxSettlement} from "../src/OnyxSettlement.sol";

/// The deploy script, run against the local test EVM (chain id 31337); no network.
contract DeployTest is Test {
    Deploy internal d;

    function setUp() public {
        d = new Deploy();
    }

    function test_DeploysOnAnvilWithDefaults() public {
        assertEq(block.chainid, 31_337);
        Deploy.Params memory p = d.defaults();
        (MockUSDC usdc, OnyxSettlement st, OnyxDisputes dsp) = d.deploy(address(this), p);
        assertEq(address(dsp.settlement()), address(st), "disputes bound to the ledger");
        assertEq(st.disputes(), address(dsp));
        assertEq(address(st.token()), address(usdc));
        assertEq(st.coordinator(), address(this));
        assertEq(st.domain(), "onyx.testnet");
        assertEq(st.unbondDelay(), p.unbondDelay);
        assertEq(st.minValidatorStake(), p.minValidatorStake);
        assertEq(st.minCoordinatorBond(), p.minCoordinatorBond);
        assertEq(st.canarySlash(), p.canarySlash);
        assertEq(
            st.ledgerId(),
            keccak256(abi.encodePacked("onyx-ledger-evm-v1", uint256(31_337), address(st)))
        );
    }

    function test_DeploysOnBaseSepoliaChainId() public {
        vm.chainId(84_532);
        (, OnyxSettlement st,) = d.deploy(address(this), d.defaults());
        assertTrue(address(st).code.length > 0);
    }

    function test_RefusesOtherChains() public {
        Deploy.Params memory p = d.defaults();
        uint256[3] memory ids = [uint256(1), uint256(8_453), uint256(11_155_111)];
        for (uint256 i = 0; i < 3; i++) {
            vm.chainId(ids[i]);
            vm.expectRevert(abi.encodeWithSelector(Deploy.WrongChain.selector, ids[i]));
            d.deploy(address(this), p);
        }
    }

    function test_EnvOverridesParams() public {
        vm.setEnv("ONYX_DOMAIN", "onyx.override");
        vm.setEnv("ONYX_UNBOND_DELAY", "42");
        Deploy.Params memory p = d.params();
        assertEq(p.domain, "onyx.override");
        assertEq(p.unbondDelay, 42);
        assertEq(p.minValidatorStake, d.defaults().minValidatorStake);
    }

    /// Overrides are range-checked before the cast (no silent truncation). Tested on `fit`
    /// directly: environment variables are process-wide and tests run in parallel.
    function test_EnvOverrideOutOfRangeRefused() public {
        uint256 big = (uint256(1) << 128) + 1;
        vm.expectRevert(
            abi.encodeWithSelector(Deploy.ParamOutOfRange.selector, "ONYX_MIN_STAKE", big)
        );
        d.fit("ONYX_MIN_STAKE", big, 128);
        vm.expectRevert(
            abi.encodeWithSelector(
                Deploy.ParamOutOfRange.selector, "ONYX_UNBOND_DELAY", uint256(1) << 64
            )
        );
        d.fit("ONYX_UNBOND_DELAY", uint256(1) << 64, 64);
        assertEq(d.fit("ONYX_MIN_STAKE", type(uint128).max, 128), type(uint128).max);
        assertEq(d.fit("ONYX_UNBOND_DELAY", type(uint64).max, 64), type(uint64).max);
    }

    /// EIP-170: deployed runtime code must be at most 24,576 bytes, or the broadcast fails
    /// (the local test EVM does not enforce it, so assert it here; M6-L36-F1).
    function test_ContractsFitEip170() public {
        (MockUSDC usdc, OnyxSettlement st, OnyxDisputes dsp) = d.deploy(address(this), d.defaults());
        emit log_named_uint("OnyxSettlement runtime bytes", address(st).code.length);
        emit log_named_uint("OnyxSettlementExt runtime bytes", st.ext().code.length);
        emit log_named_uint("OnyxDisputes runtime bytes", address(dsp).code.length);
        assertLe(address(st).code.length, 24_576, "OnyxSettlement over EIP-170");
        assertLe(st.ext().code.length, 24_576, "OnyxSettlementExt over EIP-170");
        assertLe(address(dsp).code.length, 24_576, "OnyxDisputes over EIP-170");
        assertLe(address(usdc).code.length, 24_576, "MockUSDC over EIP-170");
    }
}
