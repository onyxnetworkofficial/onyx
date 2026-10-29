// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxDisputes} from "../src/OnyxDisputes.sol";
import {OnyxSettlement, IERC20Min} from "../src/OnyxSettlement.sol";
import {OnyxSettlementExt} from "../src/OnyxSettlementExt.sol";
import {MockUSDC} from "../test/mocks/MockUSDC.sol";

/// @notice Deploys a test token (MockUSDC, free mint, testnet only), OnyxDisputes and
/// OnyxSettlement (which creates its extension), then binds the dispute game to the ledger
/// (ADR-0024). The linked `OnyxRound` library is deployed by forge.
/// Runs only on Base Sepolia (84532) or a local anvil (31337); never on a mainnet.
/// The deployer key is read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and is
/// never logged. The RPC endpoint is passed on the command line (`--rpc-url`).
/// See docs/ops/deploy-base-sepolia.md.
contract Deploy is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    error WrongChain(uint256 chainId);
    /// The node behind `--rpc-url` reports a different chain than the script runs as (for
    /// example `--chain 84532` pointed at another network). Checked before broadcasting.
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    /// An environment override does not fit the parameter's type (it would be truncated).
    error ParamOutOfRange(string name, uint256 value);

    /// Constructor parameters. Defaults are testnet values; each can be overridden from the
    /// environment (ONYX_DOMAIN, ONYX_UNBOND_DELAY, ONYX_MIN_STAKE, ONYX_MIN_COORD_BOND,
    /// ONYX_CANARY_SLASH). Amounts are in MockUSDC base units (6 decimals).
    struct Params {
        string domain;
        uint64 unbondDelay;
        uint128 minValidatorStake;
        uint128 minCoordinatorBond;
        uint128 canarySlash;
    }

    function defaults() public pure returns (Params memory) {
        return Params({
            domain: "onyx.testnet",
            unbondDelay: 1_800, // about 1 hour at 2 s blocks
            minValidatorStake: 100e6,
            minCoordinatorBond: 100e6,
            canarySlash: 10e6
        });
    }

    function params() public view returns (Params memory p) {
        p = defaults();
        p.domain = vm.envOr("ONYX_DOMAIN", p.domain);
        p.unbondDelay = uint64(
            fit("ONYX_UNBOND_DELAY", vm.envOr("ONYX_UNBOND_DELAY", uint256(p.unbondDelay)), 64)
        );
        p.minValidatorStake = uint128(
            fit("ONYX_MIN_STAKE", vm.envOr("ONYX_MIN_STAKE", uint256(p.minValidatorStake)), 128)
        );
        p.minCoordinatorBond = uint128(
            fit(
                "ONYX_MIN_COORD_BOND",
                vm.envOr("ONYX_MIN_COORD_BOND", uint256(p.minCoordinatorBond)),
                128
            )
        );
        p.canarySlash = uint128(
            fit("ONYX_CANARY_SLASH", vm.envOr("ONYX_CANARY_SLASH", uint256(p.canarySlash)), 128)
        );
    }

    /// Refuse an override that would be silently truncated by the cast to `bits` bits.
    function fit(string memory name, uint256 v, uint256 bits) public pure returns (uint256) {
        if (v >> bits != 0) revert ParamOutOfRange(name, v);
        return v;
    }

    /// Chain id reported by the node behind `--rpc-url` (`eth_chainId`), independent of any
    /// `--chain` flag, which only sets `block.chainid` for the simulation.
    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    /// Entry point for `forge script`. The deployer becomes the coordinator. Refuses unless the
    /// node's own chain id (not just `block.chainid`) is Base Sepolia or anvil.
    function run()
        external
        returns (MockUSDC usdc, OnyxSettlement settlement, OnyxDisputes disputes)
    {
        // Ask the real node first: `--chain` can make block.chainid say 84532 on any network.
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        if (real != BASE_SEPOLIA && real != ANVIL) revert WrongChain(real);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        Params memory p = params();
        vm.startBroadcast(key);
        (usdc, settlement, disputes) = deploy(deployer, p);
        vm.stopBroadcast();
    }

    /// Deploy the contracts and bind them. Refuses any chain other than Base Sepolia or anvil.
    function deploy(address coordinator, Params memory p)
        public
        returns (MockUSDC usdc, OnyxSettlement settlement, OnyxDisputes disputes)
    {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        usdc = new MockUSDC();
        disputes = new OnyxDisputes();
        settlement = new OnyxSettlement(
            IERC20Min(address(usdc)),
            coordinator,
            address(disputes),
            p.domain,
            p.unbondDelay,
            p.minValidatorStake,
            p.minCoordinatorBond,
            p.canarySlash
        );
        disputes.bind(OnyxSettlementExt(address(settlement)));
    }
}
