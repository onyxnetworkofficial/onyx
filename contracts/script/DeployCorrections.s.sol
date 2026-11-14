// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {OnyxCorrections} from "../src/OnyxCorrections.sol";
import {OnyxRegistry} from "../src/OnyxRegistry.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {IStakeToken} from "../src/IOnyxMarkets.sol";

/// @notice Deploys `OnyxCorrections` (CL7, ADR-0057) next to a deployed `OnyxRegistry` and the
/// markets' tONYX test token. Runs only on Base Sepolia (84532) or a local anvil (31337). The
/// deployer key is read from the environment at run time (BASE_SEPOLIA_PRIVATE_KEY) and never
/// logged; the deployer owns the contract (it only pairs metrics; it never decides a market) and
/// is the treasury that open fees and trading fees go to (testnet only). Inputs: ONYX_REGISTRY,
/// ONYX_TONYX (both required). `pairRecheck` pairs a metric, `openMarket` opens a market and
/// `placeBet` bets in one. Steps: docs/ops/runbooks/corrections-base-sepolia.md.
contract DeployCorrections is Script {
    uint256 internal constant BASE_SEPOLIA = 84_532;
    uint256 internal constant ANVIL = 31_337;
    /// Test ETH an opener or bettor below `GAS_FLOOR_WEI` gets from the minter for gas, as a
    /// markets bettor does (`onyx_level2::markets::GAS_GRANT_WEI`, `GAS_FLOOR_WEI`).
    uint256 public constant GAS_GRANT_WEI = 50_000_000_000_000;
    uint256 public constant GAS_FLOOR_WEI = 20_000_000_000_000;

    /// One market to open (`openMarket`): the original round's value and the market's blocks.
    struct Opening {
        bytes32 metric;
        bytes32 subjectKey;
        /// The original round's `windowEnd`, `roundId` and value (its `/v1/observations` answer).
        uint64 windowEnd;
        bytes32 round;
        int128 value;
        uint64 closeBlock;
        uint64 resolveBlock;
        /// The unix second the last recheck day ends (00:00 UTC of the day after it).
        uint64 recheckEnd;
        /// The chain's measured milliseconds per block (Base Sepolia: 200).
        uint64 blockMs;
        /// The `.recheck` metric and its template the caller computed (`onyx-source recheck`
        /// with its after and span days): the contract's pair and the registry must agree.
        bytes32 recheckMetric;
        bytes32 recheckTemplate;
    }

    error WrongChain(uint256 chainId);
    error ChainMismatch(uint256 rpcChainId, uint256 scriptChainId);
    /// The registry holds another template, or another kind, for the recheck metric.
    error RecheckRegisteredOtherwise(bytes32 registered);
    /// The original is paired with another recheck metric or tolerance.
    error PairedOtherwise(bytes32 recheckMetric, uint128 tolerance);
    /// ONYX_REGISTRY_OWNER_KEY is not the key of the registry's owner.
    error NotRegistryOwner(address key, address owner);
    /// `openMarket` refuses before sending anything:
    /// the metric has no recheck pair on the contract;
    error MetricNotPaired(bytes32 metric);
    /// the opener is the owner, the treasury or the registry's coordinator (it may not bet);
    error OpenerIsInsider(address opener);
    /// the close is sooner than `minTradingBlocks` from now;
    error CloseTooSoon(uint64 closeBlock, uint256 earliest);
    /// the deadline is sooner than `minRecheckBlocks` after the close;
    error DeadlineTooSoon(uint64 resolveBlock, uint256 earliest);
    /// the deadline is further than `maxMarketBlocks` from now;
    error DeadlineTooLate(uint64 resolveBlock, uint256 latest);
    /// trading would close after the last recheck day, so no recheck could follow (CL7-13);
    error CloseAfterRecheckDays(uint256 closeTime, uint64 recheckEnd);
    /// the deadline would come before the last recheck day ends;
    error DeadlineInRecheckDays(uint256 deadlineTime, uint64 recheckEnd);
    /// the registry recorded no result for the original fact;
    error NoRecordedValue(bytes32 fact);
    /// the round and value do not hash, with the recorded spec, to the recorded result;
    error NotTheRecordedValue(bytes32 recorded, bytes32 given);
    /// the recheck fact has a result already (a market can no longer open);
    error RecheckRecorded(bytes32 recheckFact);
    /// BASE_SEPOLIA_PRIVATE_KEY is not the tONYX minter's key;
    error NotMinter(address key, address minter);
    /// an input does not fit its type, or the block time is 0.
    error OutOfRange(string name);
    /// `placeBet` refuses before sending anything: the market is not open;
    error MarketNotOpen(uint256 marketId);
    /// its trading closed;
    error MarketClosed(uint256 marketId, uint64 closeBlock);
    /// the bettor is the owner, the treasury or the registry's coordinator;
    error BettorIsInsider(address bettor);
    /// the side is neither 1 (Yes) nor 2 (No);
    error BadSide(uint8 side);
    /// the amount is under `minBet` or over what `maxBetPerAddress` leaves this bettor;
    error BadBetAmount(uint256 amount, uint256 least, uint256 most);
    /// the side would pass half the pool cap.
    error SideFull(uint256 pool, uint256 cap);

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

    /// @notice The runbook's step 6: open the correction market of a recorded value, every input
    /// from the environment (`scripts/corrections-sepolia.sh open` computes them; keys never on a
    /// command line). The opener (ONYX_OPENER_KEY: a test key, never an insider) pays the open
    /// fee. When it holds less than the fee, or less than `GAS_FLOOR_WEI` of test ETH, the
    /// deployer (BASE_SEPOLIA_PRIVATE_KEY, the tONYX minter) first mints the rest and grants gas,
    /// as for a new bettor; a dry run without that key simulates the minter (sending needs it).
    /// Inputs: ONYX_CORRECTIONS; ONYX_METRIC_KEY, ONYX_SUBJECT_KEY, ONYX_WINDOW_END, ONYX_ROUND_ID
    /// and ONYX_VALUE (the original round's); ONYX_CLOSE_BLOCK, ONYX_RESOLVE_BLOCK;
    /// ONYX_RECHECK_END and ONYX_BLOCK_MS; ONYX_RECHECK_METRIC_KEY and ONYX_RECHECK_TEMPLATE
    /// (see `Opening`). Run again when the value has a market (open, closed or settled), it sends
    /// nothing; ONYX_NEW_MARKET=true opens another one.
    function openMarket() external returns (uint256) {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        Opening memory o = Opening({
            metric: vm.envBytes32("ONYX_METRIC_KEY"),
            subjectKey: vm.envBytes32("ONYX_SUBJECT_KEY"),
            windowEnd: u64(vm.envUint("ONYX_WINDOW_END"), "ONYX_WINDOW_END"),
            round: vm.envBytes32("ONYX_ROUND_ID"),
            value: i128(vm.envInt("ONYX_VALUE")),
            closeBlock: u64(vm.envUint("ONYX_CLOSE_BLOCK"), "ONYX_CLOSE_BLOCK"),
            resolveBlock: u64(vm.envUint("ONYX_RESOLVE_BLOCK"), "ONYX_RESOLVE_BLOCK"),
            recheckEnd: u64(vm.envUint("ONYX_RECHECK_END"), "ONYX_RECHECK_END"),
            blockMs: u64(vm.envUint("ONYX_BLOCK_MS"), "ONYX_BLOCK_MS"),
            recheckMetric: vm.envBytes32("ONYX_RECHECK_METRIC_KEY"),
            recheckTemplate: vm.envBytes32("ONYX_RECHECK_TEMPLATE")
        });
        return openWith(
            OnyxCorrections(vm.envAddress("ONYX_CORRECTIONS")),
            o,
            vm.envUint("ONYX_OPENER_KEY"),
            vm.envOr("BASE_SEPOLIA_PRIVATE_KEY", uint256(0)),
            vm.envOr("ONYX_NEW_MARKET", false)
        );
    }

    /// @notice `openMarket` with its inputs given (`minterKey` 0: simulate the minter; `another`:
    /// open even when the value has a market). Refuses first what `open` would refuse and a
    /// market no recheck could settle (`checkOpening`), funds the opener if it must, approves the
    /// fee unless approved, opens.
    function openWith(
        OnyxCorrections c,
        Opening memory o,
        uint256 openerKey,
        uint256 minterKey,
        bool another
    ) public returns (uint256 id) {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        id = another ? 0 : c.marketOf(c.registry().factKey(o.metric, o.subjectKey, o.windowEnd));
        if (id != 0) {
            console.log("corrections: market %s of this value exists; nothing sent", id);
            return id;
        }
        address opener = vm.addr(openerKey);
        checkOpening(c, o, opener);
        OnyxTestToken t = OnyxTestToken(address(c.token()));
        uint256 fee = c.openFee();
        fund(t, opener, fee, minterKey);
        vm.startBroadcast(openerKey);
        if (t.allowance(opener, address(c)) < fee) t.approve(address(c), fee);
        id = c.open(
            o.metric, o.subjectKey, o.windowEnd, o.round, o.value, o.closeBlock, o.resolveBlock
        );
        vm.stopBroadcast();
        console.log(
            "corrections: market %s, close block %s, deadline block %s",
            id,
            uint256(o.closeBlock),
            uint256(o.resolveBlock)
        );
    }

    /// @notice Give `who` the gas and the tONYX it lacks for spending `need` (the minter sends,
    /// as for a markets bettor; `minterKey` 0 simulates it, a dry run only).
    function fund(OnyxTestToken t, address who, uint256 need, uint256 minterKey) internal {
        uint256 held = t.balanceOf(who);
        uint256 mint = held < need ? need - held : 0;
        uint256 grant = who.balance < GAS_FLOOR_WEI ? GAS_GRANT_WEI : 0;
        console.log(
            "corrections: %s holds %s tONYX wei, %s wei of test ETH", who, held, who.balance
        );
        if (mint == 0 && grant == 0) return;
        address minter = t.minter();
        if (minterKey == 0) {
            vm.startBroadcast(minter);
        } else {
            if (vm.addr(minterKey) != minter) revert NotMinter(vm.addr(minterKey), minter);
            vm.startBroadcast(minterKey);
        }
        if (grant != 0) payable(who).transfer(grant);
        if (mint != 0) t.mint(who, mint);
        vm.stopBroadcast();
        console.log("corrections: the minter grants %s wei of gas, mints %s tONYX wei", grant, mint);
    }

    /// @notice Bet in a correction market from an operator test key (never an insider): the
    /// bettor (ONYX_BETTOR_KEY, in the environment only) gets gas and the tONYX it lacks from the
    /// deployer (BASE_SEPOLIA_PRIVATE_KEY, the minter; a dry run without it simulates the minter),
    /// approves the amount unless approved and bets. Inputs: ONYX_CORRECTIONS, ONYX_MARKET_ID,
    /// ONYX_SIDE (1 Yes "accurate", 2 No), ONYX_AMOUNT (tONYX wei).
    function placeBet() external {
        uint256 real = rpcChainId();
        if (real != block.chainid) revert ChainMismatch(real, block.chainid);
        uint256 side = vm.envUint("ONYX_SIDE");
        if (side > type(uint8).max) revert OutOfRange("ONYX_SIDE");
        uint256 amount = vm.envUint("ONYX_AMOUNT");
        if (amount > type(uint128).max) revert OutOfRange("ONYX_AMOUNT");
        betWith(
            OnyxCorrections(vm.envAddress("ONYX_CORRECTIONS")),
            vm.envUint("ONYX_MARKET_ID"),
            uint8(side),
            uint128(amount),
            vm.envUint("ONYX_BETTOR_KEY"),
            vm.envOr("BASE_SEPOLIA_PRIVATE_KEY", uint256(0))
        );
    }

    /// @notice `placeBet` with its inputs given (`minterKey` 0: simulate the minter).
    function betWith(
        OnyxCorrections c,
        uint256 marketId,
        uint8 side,
        uint128 amount,
        uint256 bettorKey,
        uint256 minterKey
    ) public {
        if (block.chainid != BASE_SEPOLIA && block.chainid != ANVIL) {
            revert WrongChain(block.chainid);
        }
        address who = vm.addr(bettorKey);
        checkBet(c, marketId, side, amount, who);
        OnyxTestToken t = OnyxTestToken(address(c.token()));
        fund(t, who, amount, minterKey);
        vm.startBroadcast(bettorKey);
        if (t.allowance(who, address(c)) < amount) t.approve(address(c), amount);
        c.bet(marketId, side, amount);
        vm.stopBroadcast();
        console.log(
            "corrections: market %s, side %s, bet %s tONYX wei", marketId, uint256(side), amount
        );
    }

    /// @notice Refuse, with the reason and before anything is sent, what `bet` would refuse.
    function checkBet(OnyxCorrections c, uint256 marketId, uint8 side, uint128 amount, address who)
        public
        view
    {
        OnyxCorrections.Market memory m = c.market(marketId);
        if (m.state != OnyxCorrections.State.Open) revert MarketNotOpen(marketId);
        if (block.number >= m.closeBlock) revert MarketClosed(marketId, m.closeBlock);
        if (!c.mayBet(who)) revert BettorIsInsider(who);
        if (side != c.YES() && side != c.NO()) revert BadSide(side);
        uint256 most = c.maxBetPerAddress() - c.betTotal(marketId, who);
        if (amount < c.minBet() || amount > most) revert BadBetAmount(amount, c.minBet(), most);
        uint256 cap = c.maxPoolPerMarket() / 2;
        if (uint256(m.pools[side - 1]) + amount > cap) revert SideFull(m.pools[side - 1], cap);
    }

    /// @notice Refuse, with the reason and before anything is sent, what `open` would refuse, and
    /// what it does not check: the pair is the caller's recheck metric and the registry holds
    /// its template (as `pairRecheck` checks); at the chain's block time `o.blockMs`, trading must close before
    /// the last recheck day ends (else no recheck can follow, CL7-13) and the deadline fall
    /// after it (so a recheck on the last day can still settle the market).
    function checkOpening(OnyxCorrections c, Opening memory o, address opener) public view {
        (bytes32 rmetric, uint128 tol) = c.pairs(o.metric);
        if (rmetric == bytes32(0)) revert MetricNotPaired(o.metric);
        if (rmetric != o.recheckMetric) revert PairedOtherwise(rmetric, tol);
        (, bytes32 rtemplate,) = c.registry().metrics(rmetric);
        if (rtemplate != o.recheckTemplate) revert RecheckRegisteredOtherwise(rtemplate);
        if (!c.mayBet(opener)) revert OpenerIsInsider(opener);
        uint256 bound = block.number + c.minTradingBlocks();
        if (o.closeBlock < bound) revert CloseTooSoon(o.closeBlock, bound);
        bound = uint256(o.closeBlock) + c.minRecheckBlocks();
        if (o.resolveBlock < bound) revert DeadlineTooSoon(o.resolveBlock, bound);
        bound = block.number + c.maxMarketBlocks();
        if (o.resolveBlock > bound) revert DeadlineTooLate(o.resolveBlock, bound);
        if (o.blockMs == 0) revert OutOfRange("ONYX_BLOCK_MS");
        uint256 closeTime = timeOf(o.closeBlock, o.blockMs);
        if (closeTime >= o.recheckEnd) revert CloseAfterRecheckDays(closeTime, o.recheckEnd);
        uint256 deadlineTime = timeOf(o.resolveBlock, o.blockMs);
        if (deadlineTime < o.recheckEnd) revert DeadlineInRecheckDays(deadlineTime, o.recheckEnd);
        OnyxRegistry r = c.registry();
        OnyxRegistry.Observation memory v = r.observationOf(o.metric, o.subjectKey, o.windowEnd);
        if (v.result == bytes32(0)) {
            revert NoRecordedValue(r.factKey(o.metric, o.subjectKey, o.windowEnd));
        }
        bytes32 given = c.resultHash(v.spec, o.round, o.value);
        if (given != v.result) revert NotTheRecordedValue(v.result, given);
        if (r.observationOf(rmetric, o.subjectKey, o.windowEnd).result != bytes32(0)) {
            revert RecheckRecorded(r.factKey(rmetric, o.subjectKey, o.windowEnd));
        }
    }

    /// @notice The unix second block `n` (not before this one) is expected at, `blockMs` per block.
    function timeOf(uint64 n, uint64 blockMs) public view returns (uint256) {
        return block.timestamp + (uint256(n) - block.number) * blockMs / 1000;
    }

    function u64(uint256 v, string memory name) internal pure returns (uint64) {
        if (v > type(uint64).max) revert OutOfRange(name);
        return uint64(v);
    }

    function i128(int256 v) internal pure returns (int128) {
        if (v > type(int128).max || v < type(int128).min) revert OutOfRange("ONYX_VALUE");
        return int128(v);
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
