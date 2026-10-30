// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {IOnyxLedgerView, IRevenueToken} from "../src/OnyxRevenue.sol";
import {OnyxRevenueV2} from "../src/OnyxRevenueV2.sol";
import {OnyxPrepaidV2, ICreditsToken} from "../src/OnyxPrepaidV2.sol";

/// @notice Deploys the credits ledger (ADR-0056, contributor loop CL6): `OnyxRevenueV2` next to
/// the deployed settlement, then `OnyxPrepaidV2` over it, in one broadcast. Runs only on Base
/// Sepolia (84532) or a local anvil (31337). The deployer key is read from the environment at run
/// time (BASE_SEPOLIA_PRIVATE_KEY) and never logged; it owns `OnyxPrepaidV2` (rotates its
/// reporter, names refund and payout sources). Inputs: ONYX_SETTLEMENT, ONYX_USDC (the EIP-3009
/// token; FiatToken v2.2 has `receiveWithAuthorization`), ONYX_RELAYER (the relayer's own
/// address: never the deployer, the reporter or the treasury), optional ONYX_REPORTER (default the
/// deployer), ONYX_TREASURY (default the deployer; it owns `OnyxRevenueV2.setReporter`),
/// ONYX_PREPAID_EXIT_DELAY (seconds, default 86400), ONYX_RELAY_CAP_PER_DAY (the token's
/// smallest unit, default 1000000) and ONYX_REVENUE_BPS ("m,v,l,t", default 5000,2000,1000,2000;
/// the treasury keeps at least MIN_TREASURY_BPS). See docs/ops/runbooks/deploy-credits.md.
contract DeployCredits is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    error BadBps();
    error RelayerNotOwnKey();

    struct Params {
        address usdc;
        address settlement;
        address reporter;
        address treasury;
        address relayer;
        uint64 exitDelay;
        uint128 relayCap;
        uint16[4] bps;
    }

    function rpcChainId() public returns (uint256 id) {
        bytes memory raw = vm.rpc("eth_chainId", "[]");
        for (uint256 i = 0; i < raw.length; i++) {
            id = (id << 8) | uint8(raw[i]);
        }
    }

    function shares() public view returns (uint16[4] memory bps) {
        bps = [uint16(5_000), 2_000, 1_000, 2_000];
        uint256[] memory def = new uint256[](0);
        uint256[] memory o = vm.envOr("ONYX_REVENUE_BPS", ",", def);
        if (o.length == 0) return bps;
        if (o.length != 4) revert BadBps();
        for (uint256 i = 0; i < 4; i++) {
            if (o[i] > 10_000) revert BadBps();
            bps[i] = uint16(o[i]);
        }
    }

    function run() external returns (OnyxRevenueV2 revenue, OnyxPrepaidV2 prepaid) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        if (real != BASE_SEPOLIA && real != ANVIL) revert WrongChain(real);
        uint256 key = vm.envUint("BASE_SEPOLIA_PRIVATE_KEY");
        address deployer = vm.addr(key);
        Params memory p = Params({
            usdc: vm.envAddress("ONYX_USDC"),
            settlement: vm.envAddress("ONYX_SETTLEMENT"),
            reporter: vm.envOr("ONYX_REPORTER", deployer),
            treasury: vm.envOr("ONYX_TREASURY", deployer),
            relayer: vm.envAddress("ONYX_RELAYER"),
            exitDelay: uint64(vm.envOr("ONYX_PREPAID_EXIT_DELAY", uint256(1 days))),
            relayCap: uint128(vm.envOr("ONYX_RELAY_CAP_PER_DAY", uint256(1_000_000))),
            bps: shares()
        });
        vm.startBroadcast(key);
        (revenue, prepaid) = deploy(p, deployer);
        vm.stopBroadcast();
    }

    /// The two deployments; `owner` is who will own `OnyxPrepaidV2` (the broadcaster).
    function deploy(Params memory p, address owner)
        public
        returns (OnyxRevenueV2 revenue, OnyxPrepaidV2 prepaid)
    {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        if (p.relayer == owner || p.relayer == p.reporter || p.relayer == p.treasury) {
            revert RelayerNotOwnKey();
        }
        revenue = new OnyxRevenueV2(
            IRevenueToken(p.usdc), IOnyxLedgerView(p.settlement), p.reporter, p.treasury, p.bps
        );
        prepaid = new OnyxPrepaidV2(
            ICreditsToken(p.usdc), revenue, p.reporter, p.relayer, p.exitDelay, p.relayCap
        );
    }
}
