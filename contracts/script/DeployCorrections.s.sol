// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxCorrections} from "../src/OnyxCorrections.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";
import {IStakeToken} from "../src/IOnyxMarkets.sol";

/// @notice Deploys `OnyxCorrections` (CL7, ADR-0057) next to a deployed `OnyxRegistry` and the
/// markets' tONYX test token. Runs only on Base Sepolia (84532) or a local anvil (31337). The
/// deployer key is read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never
/// logged; the deployer owns the contract (it only pairs metrics; it never decides a market) and
/// is the treasury that open fees and trading fees go to (testnet only). Inputs: ONYX_REGISTRY,
/// ONYX_TONYX (both required). Steps: docs/ops/runbooks/corrections-base-sepolia.md.
contract DeployCorrections is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    /// The registry holds another template, or another kind, for the recheck metric.
    error RecheckRegisteredOtherwise(bytes32 registered);
    /// The original is paired with another recheck metric or tolerance.
    error PairedOtherwise(bytes32 recheckMetric, uint128 tolerance);
    /// ONYX_REGISTRY_OWNER_KEY is not the key of the registry's owner.
    error NotRegistryOwner(address key, address owner);

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function run() external returns (OnyxCorrections c) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        OnyxRegistry registry = OnyxRegistry(vm.envAddress("ONYX_REGISTRY"));
        IStakeToken token = IStakeToken(vm.envAddress("ONYX_TONYX"));
        vm.startBroadcast(key);
        c = deploy(token, registry, deployer);
        vm.stopBroadcast();
    }

    /// @notice The runbook's steps 4 and 5 in one run, the keys from the environment as in `run`
    /// (never on a command line): the registry's owner (ONYX_REGISTRY_OWNER_KEY; never the
    /// coordinator, REG-R1-F3) registers the `.recheck` metric when the registry lacks it, then
    /// the deployer (BASE_SEPOLIA_PRIVATE_KEY, the contract's owner) pairs it with the original.
    /// Run again, it sends nothing. Inputs: ONYX_REGISTRY, ONYX_CORRECTIONS, ONYX_METRIC_KEY,
    /// ONYX_RECHECK_METRIC_KEY, ONYX_RECHECK_TEMPLATE, ONYX_RECHECK_TOLERANCE (`onyx-source
    /// recheck` prints the last four).
    function pairRecheck() external {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        uint256 ownerKey = vm.envUint("ONYX_REGISTRY_OWNER_KEY");
        OnyxRegistry registry = OnyxRegistry(vm.envAddress("ONYX_REGISTRY"));
        if (vm.addr(ownerKey) != registry.owner()) {
            revert NotRegistryOwner(vm.addr(ownerKey), registry.owner());
        }
        OnyxCorrections c = OnyxCorrections(vm.envAddress("ONYX_CORRECTIONS"));
        bytes32 metric = vm.envBytes32("ONYX_METRIC_KEY");
        bytes32 rmetric = vm.envBytes32("ONYX_RECHECK_METRIC_KEY");
        bytes32 rtemplate = vm.envBytes32("ONYX_RECHECK_TEMPLATE");
        uint128 tolerance = uint128(vm.envUint("ONYX_RECHECK_TOLERANCE"));
        vm.startBroadcast(ownerKey);
        registerRecheck(registry, rmetric, rtemplate);
        vm.stopBroadcast();
        vm.startBroadcast(key);
        pairOnly(c, metric, rmetric, tolerance);
        vm.stopBroadcast();
    }

    /// @notice `registerRecheck`, then `pairOnly`, by one owner of both (tests, a local anvil).
    function pair(
        OnyxRegistry registry,
        OnyxCorrections c,
        bytes32 metric,
        bytes32 rmetric,
        bytes32 rtemplate,
        uint128 tolerance
    ) public {
        registerRecheck(registry, rmetric, rtemplate);
        pairOnly(c, metric, rmetric, tolerance);
    }

    /// @notice Register `rmetric` as a dated feed with `rtemplate` unless the registry holds it
    /// (the registry's owner sends it). Append-only: another template is refused, never
    /// overwritten.
    function registerRecheck(OnyxRegistry registry, bytes32 rmetric, bytes32 rtemplate) public {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        (OnyxRegistry.Kind kind, bytes32 t,) = registry.metrics(rmetric);
        if (kind == OnyxRegistry.Kind.None) {
            registry.registerMetric(rmetric, OnyxRegistry.Kind.DatedFeed, rtemplate, 0);
        } else if (kind != OnyxRegistry.Kind.DatedFeed || t != rtemplate) {
            revert RecheckRegisteredOtherwise(t);
        }
    }

    /// @notice Pair `metric` with `rmetric` (tolerance `tolerance`) unless already paired (the
    /// contract's owner sends it). Append-only: another pair is refused, never overwritten.
    function pairOnly(OnyxCorrections c, bytes32 metric, bytes32 rmetric, uint128 tolerance)
        public
    {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        (bytes32 paired, uint128 tol) = c.pairs(metric);
        if (paired == bytes32(0)) {
            c.pair(metric, rmetric, tolerance);
        } else if (paired != rmetric || tol != tolerance) {
            revert PairedOtherwise(paired, tol);
        }
    }

    function deploy(IStakeToken token, OnyxRegistry registry, address treasury)
        public
        returns (OnyxCorrections)
    {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        return new OnyxCorrections(token, registry, treasury, defaults());
    }

    /// Placeholders until measured round costs and the partners' numbers (ADR-0057 open
    /// question 1, NS12), in Base Sepolia blocks of 0.2 s (measured 2026-11-01: 77,211 blocks in
    /// 15,442 s; the first placeholders assumed 2 s blocks, so every span was a tenth of its
    /// aim): the open fee covers a recheck round (about 15 transactions); trading lasts at least
    /// an hour; the recheck gets at least a day; no market outlives 30 days; a recorded recheck
    /// can be resolved for a day after the deadline before anyone may void it.
    function defaults() public pure returns (OnyxCorrections.Params memory) {
        return OnyxCorrections.Params({
            openFee: 5e18,
            minBet: 1e18,
            maxBetPerAddress: 100e18,
            maxPoolPerMarket: 10_000e18,
            feePpm: 20_000,
            minTradingBlocks: 18_000,
            minRecheckBlocks: 432_000,
            maxMarketBlocks: 12_960_000,
            graceBlocks: 432_000
        });
    }
}
