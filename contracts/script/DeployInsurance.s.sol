// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxFutures} from "../src/OnyxFutures.sol";
import {OnyxObservedFacts} from "../src/OnyxObservedFacts.sol";
import {OnyxParametric} from "../src/OnyxParametric.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";
import {
    IStakeToken,
    IOnyxFacts,
    IOnyxFactGuard,
    IOnyxReobserve,
    IOnyxFallbackOracle
} from "../src/IOnyxMarkets.sol";

/// @notice The second markets deployment (north-star loop NL14 and NL18): a new
/// `OnyxObservedFacts` (listings carry `closeBy`; insiders named), a guarded `OnyxFutures`
/// (insiders may not bet; trading closes by the listing's `closeBy`) and `OnyxParametric`
/// (ADR-0042 amendment 3), next to the first deployment's tONYX, `OnyxReobserve` and `OnyxPanelOracle`, which
/// are reused unchanged. The first `OnyxObservedFacts` and `OnyxFutures` stay for the markets
/// already open on them. Base Sepolia (84532) or a local anvil (31337) only; the deployer key is
/// read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never logged.
/// Inputs: ONYX_REGISTRY, ONYX_MARKETS_TOKEN, ONYX_MARKETS_REOBSERVE, ONYX_MARKETS_PANEL.
contract DeployInsurance is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    struct Second {
        OnyxObservedFacts facts;
        OnyxFutures futures;
        OnyxParametric insurance;
    }

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    error NoCode(address at);

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function run() external returns (Second memory s) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        OnyxRegistry registry = OnyxRegistry(vm.envAddress("ONYX_REGISTRY"));
        address token = vm.envAddress("ONYX_MARKETS_TOKEN");
        address reobserve = vm.envAddress("ONYX_MARKETS_REOBSERVE");
        address panel = vm.envAddress("ONYX_MARKETS_PANEL");
        vm.startBroadcast(key);
        s = deploy(deployer, registry, token, reobserve, panel);
        vm.stopBroadcast();
    }

    function deploy(
        address admin,
        OnyxRegistry registry,
        address token,
        address reobserve,
        address panel
    ) public returns (Second memory s) {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        if (address(registry).code.length == 0) revert NoCode(address(registry));
        if (token.code.length == 0) revert NoCode(token);
        if (reobserve.code.length == 0) revert NoCode(reobserve);
        if (panel.code.length == 0) revert NoCode(panel);
        s.facts = new OnyxObservedFacts(registry, admin);
        s.futures = new OnyxFutures(
            IStakeToken(token),
            IOnyxFacts(address(s.facts)),
            IOnyxReobserve(reobserve),
            IOnyxFallbackOracle(panel),
            IOnyxFactGuard(address(s.facts)),
            admin,
            futuresDefaults()
        );
        s.insurance = new OnyxParametric(
            IStakeToken(token),
            IOnyxFacts(address(s.facts)),
            IOnyxFactGuard(address(s.facts)),
            IOnyxReobserve(reobserve)
        );
    }

    /// `OnyxFutures.defaultParams` (as in `DeployMarkets`).
    function futuresDefaults() public pure returns (OnyxFutures.Params memory) {
        return OnyxFutures.Params({
            minBet: 1e18,
            maxBetPerAddress: 100e18,
            maxPoolPerMarket: 10_000e18,
            feePpm: 20_000,
            minCloseLead: 1 hours,
            maxBuckets: 8,
            maxLevel: 2
        });
    }
}

/// @notice Lists a fact in the second `OnyxObservedFacts` (when it is not listed yet) and offers
/// one `OnyxParametric` product on it (`scripts/insurance-sepolia.sh offer`). The deployer key is
/// read from the environment (BASE_SEPOLIA_PRIVATE_KEY), never a command line (THREATS B6-1).
/// Inputs (env): ONYX_FACTS, ONYX_INSURANCE, P_METRIC, P_TEMPLATE, P_SUBJECT_KEY, P_NOT_BEFORE,
/// P_CLOSE_BY, P_CMP (0 at least, 1 at most), P_THRESHOLD, P_PAYOUT, P_PREMIUM (wei of tONYX),
/// P_BUY_BY, P_OBSERVABLE, P_RESOLVE_BY, P_MAX.
contract OfferPolicy is Script {
    function run() external returns (bytes32 fact, uint256 id) {
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        OnyxObservedFacts facts = OnyxObservedFacts(vm.envAddress("ONYX_FACTS"));
        OnyxParametric ins = OnyxParametric(vm.envAddress("ONYX_INSURANCE"));
        bytes32 metric = vm.envBytes32("P_METRIC");
        bytes32 template = vm.envBytes32("P_TEMPLATE");
        bytes32 subjectKey = vm.envBytes32("P_SUBJECT_KEY");
        fact = facts.registry().factKeyOf(metric, template, subjectKey, 0, true);
        vm.startBroadcast(key);
        if (facts.closeBy(fact) == 0) {
            facts.list(
                metric,
                template,
                subjectKey,
                0,
                true,
                uint64(vm.envUint("P_NOT_BEFORE")),
                uint64(vm.envUint("P_CLOSE_BY"))
            );
        }
        id = ins.offer(
            fact,
            OnyxParametric.Cmp(uint8(vm.envUint("P_CMP"))),
            vm.envInt("P_THRESHOLD"),
            uint128(vm.envUint("P_PAYOUT")),
            uint128(vm.envUint("P_PREMIUM")),
            uint64(vm.envUint("P_BUY_BY")),
            uint64(vm.envUint("P_OBSERVABLE")),
            uint64(vm.envUint("P_RESOLVE_BY")),
            uint32(vm.envUint("P_MAX"))
        );
        vm.stopBroadcast();
    }
}
