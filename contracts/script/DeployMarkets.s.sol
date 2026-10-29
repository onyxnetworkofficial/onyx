// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {OnyxReobserve} from "../src/OnyxReobserve.sol";
import {OnyxPanelOracle} from "../src/OnyxPanelOracle.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxFutures} from "../src/OnyxFutures.sol";
import {OnyxObservedFacts} from "../src/OnyxObservedFacts.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";
import {
    IStakeToken,
    IOnyxFacts,
    IOnyxReobserve,
    IOnyxFallbackOracle,
    IOnyxFactGuard,
    IOnyxValidatorSet
} from "../src/IOnyxMarkets.sol";

/// @notice Deploys the markets (NORTH-STAR NS11, ADR-0042 amendment 2) next to a deployed
/// `OnyxRegistry`: the tONYX test token, the stake contract with its re-observation and the
/// drawn-panel fallback, and the futures, which settle from level-2 values the registry recorded
/// (`OnyxObservedFacts`). Runs only on Base Sepolia (84532) or a local anvil (31337). The deployer
/// key is read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never logged; the
/// deployer mints tONYX, owns the futures and lists the facts markets settle from (testnet only).
/// Inputs: ONYX_REGISTRY (required). Every economic number is a placeholder (ADR-0042 item 10).
contract DeployMarkets is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    /// Panel: 5 drawn seats, 3 decide, 10 tONYX a seat, 10% lost by a no-show, 1 h windows; seats
    /// only for the settlement's staked validators (THREATS LC16-2).
    uint128 public constant SEAT_BOND = 10e18;
    uint32 public constant PANEL_SIZE = 5;
    uint32 public constant MAJORITY = 3;
    uint32 public constant NO_SHOW_PPM = 100_000;
    uint64 public constant PANEL_WINDOW = 1 hours;
    /// An observer change takes effect after 3 days, above the stakes' 1-day commit plus 1-day
    /// reveal windows (THREATS LC16-1).
    uint64 public constant OBSERVER_DELAY = 3 days;

    struct Markets {
        OnyxTestToken token;
        OnyxReobserve reobserve;
        OnyxPanelOracle panel;
        OnyxStakes stakes;
        OnyxObservedFacts facts;
        OnyxFutures futures;
    }

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function run() external returns (Markets memory m) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        OnyxRegistry registry = OnyxRegistry(vm.envAddress("ONYX_REGISTRY"));
        vm.startBroadcast(key);
        m = deploy(deployer, registry);
        vm.stopBroadcast();
    }

    /// Every contract, wired; `admin` mints, owns the futures and lists facts, and is the
    /// treasury the fees go to.
    function deploy(address admin, OnyxRegistry registry) public returns (Markets memory m) {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        m.token = new OnyxTestToken(admin);
        m.reobserve = new OnyxReobserve(OBSERVER_DELAY);
        m.panel = new OnyxPanelOracle(
            IStakeToken(address(m.token)),
            admin,
            SEAT_BOND,
            PANEL_SIZE,
            MAJORITY,
            NO_SHOW_PPM,
            PANEL_WINDOW,
            PANEL_WINDOW,
            IOnyxValidatorSet(address(registry.ledger()))
        );
        m.stakes = new OnyxStakes(
            IStakeToken(address(m.token)),
            IOnyxReobserve(address(m.reobserve)),
            IOnyxFallbackOracle(address(m.panel)),
            admin,
            _stakeDefaults()
        );
        m.reobserve.bind(address(m.stakes));
        m.panel.bind(address(m.stakes));
        m.facts = new OnyxObservedFacts(registry, admin);
        m.futures = new OnyxFutures(
            IStakeToken(address(m.token)),
            IOnyxFacts(address(m.facts)),
            IOnyxReobserve(address(m.reobserve)),
            IOnyxFallbackOracle(address(m.panel)),
            IOnyxFactGuard(address(m.facts)),
            admin,
            _futuresDefaults()
        );
    }

    /// `OnyxStakes.defaultParams` (1-day vote windows, a 3-day fallback: room for the panel's
    /// draw and two 1-hour windows).
    function _stakeDefaults() internal pure returns (OnyxStakes.Params memory) {
        return OnyxStakes.Params({
            submitBond: 10e18,
            submitReward: 1e18,
            minStake: 1e18,
            maxStakePerAddress: 100e18,
            maxStakePerClaim: 1_000e18,
            quorum: 20e18,
            decisivePpm: 666_667,
            feePpm: 20_000,
            commitWindow: 1 days,
            revealWindow: 1 days,
            fallbackWindow: 3 days
        });
    }

    /// `OnyxFutures.defaultParams`.
    function _futuresDefaults() internal pure returns (OnyxFutures.Params memory) {
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
