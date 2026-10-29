// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, Vm} from "forge-std/Test.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {OnyxLedger, IERC20Min} from "../../src/OnyxLedger.sol";
import {OnyxSettlement} from "../../src/OnyxSettlement.sol";
import {OnyxSettlementExt} from "../../src/OnyxSettlementExt.sol";
import {OnyxDisputes} from "../../src/OnyxDisputes.sol";
import {OnyxCommit} from "../../src/OnyxCommit.sol";
import {OnyxRound} from "../../src/OnyxRound.sol";
import {OnyxSig} from "../../src/OnyxSig.sol";
import {MockUSDC} from "../mocks/MockUSDC.sol";

/// Handler for the settlement + dispute invariants (L2 contract audit, docs/audit/2026-10-13-contracts.md).
/// It is the coordinator and the deployer (binder) of a fresh settlement/dispute pair and drives
/// random sequences of every entry point: staking, rounds (with and without a canary), signed
/// commits, reveals, seeds, verdicts, canaries, settling, voiding, rerun cases, challenges,
/// panels, appeals, arbitration, expiry, judging, release, reclaim and pull withdrawals, plus
/// privileged calls from non-role accounts that must all revert. Every call is wrapped in
/// try/catch: a revert is a refused action, never a failure. Breaches of rules the invariants
/// cannot read from state (unexpected token movement, a lock escaped, a final root changed, a
/// privileged call that succeeded) are counted in `breaches` and asserted zero.
contract SettlementHandler is Test {
    MockUSDC public usdc;
    OnyxDisputes public dsp;
    OnyxSettlement public st;
    OnyxSettlementExt public sx;

    uint64 public constant UNBOND_DELAY = 50;
    uint128 public constant MIN_STAKE = 1_000;
    uint128 public constant MIN_COORD_BOND = 1_000;
    uint128 public constant CANARY_SLASH = 500;
    uint128 public constant PANEL_SLASH = 50;
    uint128 public constant MIN_BOND = 100;
    uint256 public constant NV = 8;
    uint256 public constant NM = 4;
    uint256 public constant MAX_ROUNDS = 8;

    bytes32 public constant GOOD = keccak256("good root");
    bytes32 public constant BAD = keccak256("bad root");
    bytes32 internal constant CANARY_SALT = keccak256("canary salt");
    bytes32 internal constant CLAIM_SALT = keccak256("claim salt");
    bytes32 internal constant REGISTRY = keccak256("registry");

    address[] public vals;
    uint256[] internal vKeys;
    address[] public miners;
    uint256[] internal mKeys;
    address public constant CHALLENGER = address(0xC4A1);
    address public constant APPELLANT = address(0xA99E);
    /// Every address the settlement can credit (coordinator = this handler, included).
    address[] public actors;

    struct RoundRec {
        uint64 id;
        bytes32[] shards;
        bool canary;
        uint256 canaryKey;
        uint64 pendingClose;
        uint64 lockEnd;
    }

    struct CommitRec {
        uint64 round;
        bytes32 shard;
        address miner;
        uint128 bond;
        bytes32 salt;
        bytes32 root;
    }

    struct VerdictRec {
        uint64 round;
        bytes32 shard;
        address v;
        address miner;
        uint8 kind;
        bytes32 root;
        bytes32 salt;
    }

    struct CaseRec {
        bytes32 id;
        uint64 round;
        bytes32 shard;
        bool challenge;
    }

    struct VoteRec {
        bytes32 caseId;
        uint32 lv;
        address member;
        bytes32 root;
        bytes32 salt;
    }

    RoundRec[] internal rounds;
    CommitRec[] internal commitRecs;
    VerdictRec[] internal verdictRecs;
    CaseRec[] internal caseRecs;
    VoteRec[] internal voteRecs;
    /// Bond posted for each level of a case (level 0: the challenge bond, 0 for a rerun case).
    mapping(bytes32 => uint128[]) internal levelBonds;

    // ------------------------------------------------------------------ ghosts

    uint256 public ghostIn;
    uint256 public ghostOut;
    uint256 public ghostFunded;
    uint256 public breaches;
    string public lastBreach;
    mapping(uint64 => mapping(bytes32 => bool)) internal seenFinal;
    mapping(uint64 => mapping(bytes32 => bytes32)) internal seenRoot;
    mapping(uint64 => mapping(bytes32 => bytes32)) internal seenBinding;
    mapping(bytes32 => bool) internal seenEver;
    /// Terminal status seen (Empty, Voided, NoValidRoot, Unresolved) + 1; 0 = none yet.
    mapping(uint64 => mapping(bytes32 => uint8)) internal seenTerminal;

    /// Successful actions, for the coverage line printed after each run.
    mapping(bytes32 => uint256) public ok;

    // Per-shard payout ghosts (review L2-R1, ADR-0022 Amendment 1). `ruleOwed` is what the
    // rules owe on a shard from its reservation or the case's lost bonds (miner reward, verdict
    // fees once per validator and revealed root, a rerun's first-panel fees), computed by an
    // independent model from the handler's own records; `seenPaid` is what was measured to
    // leave the round budget, plus extra fees credited from lost bonds (`Credited` reason 7),
    // plus shortfalls recorded owed (`owedTotal`). `ruleShort` is the model's uncovered extra
    // fees; `seenOwed` the recorded ones.
    mapping(uint64 => mapping(bytes32 => uint256)) public ruleOwed;
    mapping(uint64 => mapping(bytes32 => uint256)) public seenPaid;
    mapping(uint64 => mapping(bytes32 => uint256)) public ruleShort;
    mapping(uint64 => mapping(bytes32 => uint256)) public seenOwed;
    uint128 public constant REWARD = 50;
    uint128 public constant FEE = 10;
    uint128 public constant PANEL_FEE = 5;
    uint8 internal constant CR_EXTRA = 7;
    bytes32 internal constant CREDITED_SIG = keccak256("Credited(address,uint256,uint8)");
    // Per-payee payout ghosts (review L2-R2): the same rules per payee, so a fee paid to the
    // wrong address fails `invariant_PayoutsMatchRules` even when the shard's total is right.
    // `ruleBy` is the model's amount per payee; `seenBy` what the payout calls named for that
    // payee (finalize's verdict-fee credits, the dispute game's `dSpend` and `dExtra` calls),
    // each call's payee checked against the credits and shortfalls it produced (`_payeeCheck`).
    mapping(uint64 => mapping(bytes32 => mapping(address => uint256))) public ruleBy;
    mapping(uint64 => mapping(bytes32 => mapping(address => uint256))) public seenBy;
    mapping(uint64 => mapping(bytes32 => address[])) internal payees;
    mapping(uint64 => mapping(bytes32 => mapping(address => bool))) internal isPayee;
    uint8 internal constant CR_VERDICT = 4;
    uint8 internal constant CR_DISPUTE = 5;
    bytes32 internal constant SHORT_SIG =
        keccak256("SpendShort(uint64,bytes32,address,uint256,uint256)");

    constructor() {
        usdc = new MockUSDC();
        dsp = new OnyxDisputes();
        st = new OnyxSettlement(
            IERC20Min(address(usdc)),
            address(this),
            address(dsp),
            "onyx.invariant",
            UNBOND_DELAY,
            MIN_STAKE,
            MIN_COORD_BOND,
            CANARY_SLASH
        );
        sx = OnyxSettlementExt(address(st));
        dsp.bind(sx);
        usdc.approve(address(st), type(uint256).max);
        actors.push(address(this));
        for (uint256 i = 0; i < NV; i++) {
            vKeys.push(0xA100 + i);
            vals.push(vm.addr(0xA100 + i));
            actors.push(vals[i]);
        }
        for (uint256 i = 0; i < NM; i++) {
            mKeys.push(0xB100 + i);
            miners.push(vm.addr(0xB100 + i));
            actors.push(miners[i]);
        }
        actors.push(CHALLENGER);
        actors.push(APPELLANT);
        // Seven bonded validators so rounds can open from the first call (the eighth joins
        // through `stake`).
        for (uint256 i = 0; i < 7; i++) {
            _stakeAs(vals[i], 2 * MIN_STAKE);
        }
        ghostIn = usdc.balanceOf(address(st));
    }

    // ------------------------------------------------------------------ bookkeeping

    /// `dir`: 1 = may only move tokens into the settlement, -1 = only out (withdraw), 0 = none.
    modifier tracked(int8 dir) {
        uint256 before = usdc.balanceOf(address(st));
        _;
        uint256 afterB = usdc.balanceOf(address(st));
        if (afterB > before) {
            if (dir != 1) _breach("tokens moved in on a non-deposit path");
            ghostIn += afterB - before;
        } else if (afterB < before) {
            if (dir != -1) _breach("tokens moved out on a non-withdraw path");
            ghostOut += before - afterB;
        }
        _observe();
    }

    function _breach(string memory why) internal {
        breaches++;
        lastBreach = why;
    }

    /// Monotonic finalization: a root that `isFinal` reports never changes and stays final;
    /// `everFinalized` never resets; a terminal status never changes.
    function _observe() internal {
        for (uint256 i = 0; i < rounds.length; i++) {
            RoundRec storage r = rounds[i];
            for (uint256 j = 0; j < r.shards.length; j++) {
                bytes32 s = r.shards[j];
                bool fin = sx.isFinal(r.id, s);
                (bytes32 root, bytes32 binding,) = sx.getFinalizedRoot(r.id, s);
                if (seenFinal[r.id][s]) {
                    if (!fin || root != seenRoot[r.id][s] || binding != seenBinding[r.id][s]) {
                        _breach("a final root changed or stopped being final");
                    }
                } else if (fin) {
                    seenFinal[r.id][s] = true;
                    seenRoot[r.id][s] = root;
                    seenBinding[r.id][s] = binding;
                }
                if (st.everFinalized(s)) seenEver[s] = true;
                else if (seenEver[s]) _breach("everFinalized reset");
                OnyxLedger.ShardStatus stt = st.shardStatus(r.id, s);
                uint8 t = seenTerminal[r.id][s];
                if (t != 0) {
                    if (uint8(stt) + 1 != t) _breach("a terminal shard status changed");
                } else if (
                    stt == OnyxLedger.ShardStatus.Empty || stt == OnyxLedger.ShardStatus.Voided
                        || stt == OnyxLedger.ShardStatus.NoValidRoot
                        || stt == OnyxLedger.ShardStatus.Unresolved
                ) {
                    seenTerminal[r.id][s] = uint8(stt) + 1;
                }
            }
        }
    }

    function _fields(bytes32 root) internal pure returns (OnyxCommit.CommitmentFields memory f) {
        f = OnyxCommit.CommitmentFields({
            root: root,
            leafCount: 2,
            recipeId: keccak256("recipe"),
            runtimeHash: keccak256("runtime"),
            labelSetHash: keccak256("labels"),
            sourceHash: keccak256("source"),
            schemaId: keccak256("schema"),
            rightsHash: keccak256("rights")
        });
    }

    function _cfg(bool halfRate, bool bigPanel, bytes32 canaryCommit_)
        internal
        view
        returns (OnyxRound.RoundConfig memory c)
    {
        uint64 n = uint64(block.number);
        c.commitClose = n + 20;
        c.revealClose = n + 40;
        c.minBond = MIN_BOND;
        c.canaryCommit = canaryCommit_;
        c.beaconHeight = n + 40;
        c.validation = OnyxRound.ValidationConfig({
            verdictCommitClose: n + 75,
            verdictRevealClose: n + 95,
            canaryOpenClose: n + 100,
            pendingClose: n + 200,
            ratePpm: halfRate ? 500_000 : 1_000_000,
            minerReward: 50,
            validatorFee: 10,
            validatorSlash: 300,
            missedSlash: 50,
            minCanaryPpm: 50_000
        });
        c.dispute = OnyxRound.DisputeConfig({
            challengeWindow: 60,
            minChallengeBond: 100,
            panelSize: bigPanel ? 3 : 1,
            panelCommitWindow: 10,
            panelRevealWindow: 10,
            appealWindow: 10,
            maxAppeals: 1,
            panelFee: 5,
            panelSlash: PANEL_SLASH
        });
    }

    function _canaryList(RoundRec storage r) internal view returns (OnyxRound.Canary[] memory l) {
        if (!r.canary) return new OnyxRound.Canary[](0);
        l = new OnyxRound.Canary[](1);
        l[0] = OnyxRound.Canary({shard: r.shards[0], miner: vm.addr(r.canaryKey), root: BAD});
    }

    function _round(uint256 seed) internal view returns (RoundRec storage) {
        return rounds[seed % rounds.length];
    }

    function _stakeAs(address v, uint128 amount) internal {
        usdc.mint(v, amount);
        vm.prank(v);
        usdc.approve(address(st), type(uint256).max);
        vm.prank(v);
        st.stake(amount);
    }

    // ------------------------------------------------------------------ validators and funds

    function stake(uint256 vSeed, uint256 amount) external tracked(1) {
        _stake(vSeed, amount);
    }

    function _stake(uint256 vSeed, uint256 amount) internal {
        address v = vals[vSeed % NV];
        uint128 a = uint128(MIN_STAKE + amount % (2 * MIN_STAKE));
        usdc.mint(v, a);
        vm.prank(v);
        usdc.approve(address(st), type(uint256).max);
        vm.prank(v);
        try st.stake(a) {
            ok["stake"]++;
        } catch {}
    }

    function requestUnbond(uint256 vSeed) external tracked(0) {
        _requestUnbond(vSeed);
    }

    function _requestUnbond(uint256 vSeed) internal {
        vm.prank(vals[vSeed % NV]);
        try sx.requestUnbond() {
            ok["unbond"]++;
        } catch {}
    }

    function withdrawStake(uint256 vSeed) external tracked(0) {
        _withdrawStake(vSeed);
    }

    function _withdrawStake(uint256 vSeed) internal {
        address v = vals[vSeed % NV];
        (uint128 had,,,) = st.validators(v);
        vm.prank(v);
        try sx.withdrawStake() {
            ok["withdrawStake"]++;
            // A stake leaves only after every round it was locked into can no longer slash it.
            for (uint256 i = 0; i < rounds.length; i++) {
                if (had > 0 && st.isRoundMember(rounds[i].id, v)) {
                    if (block.number < rounds[i].lockEnd) _breach("stake left before lockEnd");
                }
            }
        } catch {}
    }

    function withdraw(uint256 aSeed) external tracked(-1) {
        _withdraw(aSeed);
    }

    function _withdraw(uint256 aSeed) internal {
        address a = actors[aSeed % actors.length];
        uint256 owed = st.balanceOf(a);
        uint256 before = usdc.balanceOf(a);
        if (a != address(this)) vm.prank(a);
        try st.withdraw() {
            ok["withdraw"]++;
            if (usdc.balanceOf(a) - before != owed || st.balanceOf(a) != 0) {
                _breach("withdraw paid other than the credit");
            }
        } catch {}
    }

    function fundRewards(uint256 amount) external tracked(1) {
        _fundRewards(amount);
    }

    function _fundRewards(uint256 amount) internal {
        uint256 a = amount % 20_000;
        usdc.mint(address(this), a);
        try st.fundRewards(a) {
            ghostFunded += a;
            ok["fund"]++;
        } catch {}
    }

    function postCoordinatorBond(uint256 amount) external tracked(1) {
        _postCoordinatorBond(amount);
    }

    function _postCoordinatorBond(uint256 amount) internal {
        uint128 a = uint128(amount % 2_000);
        usdc.mint(address(this), a);
        try sx.postCoordinatorBond(a) {
            ok["postBond"]++;
        } catch {}
    }

    function withdrawCoordinatorBond() external tracked(0) {
        _withdrawCoordinatorBond();
    }

    function _withdrawCoordinatorBond() internal {
        try sx.withdrawCoordinatorBond() {
            ok["withdrawBond"]++;
            for (uint256 i = 0; i < rounds.length; i++) {
                if (block.number < rounds[i].pendingClose) {
                    _breach("coordinator bond left before pendingClose");
                }
            }
        } catch {}
    }

    // ------------------------------------------------------------------ rounds

    function openRound(uint8 nShards, bool canary, bool halfRate, bool bigPanel)
        external
        tracked(1)
    {
        _openRound(nShards, canary, halfRate, bigPanel);
    }

    function _openRound(uint8 nShards, bool canary, bool halfRate, bool bigPanel) internal {
        if (rounds.length >= MAX_ROUNDS) return;
        (address[] memory ms,) = st.activeSet();
        if (ms.length < 3) return;
        uint64 id = uint64(rounds.length + 1);
        uint256 n = 1 + nShards % 2;
        bytes32 fh = OnyxCommit.shardFieldsHash(_fields(GOOD));
        bytes32[] memory ids = new bytes32[](n);
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](n);
        for (uint256 i = 0; i < n; i++) {
            bytes32 p = keccak256(abi.encode("params", id, i));
            ids[i] = OnyxCommit.shardId(fh, p, REGISTRY);
            inits[i] = OnyxLedger.ShardInit({fieldsHash: fh, paramsHash: p, registryRoot: REGISTRY});
        }
        if (n == 2 && ids[0] > ids[1]) {
            (ids[0], ids[1]) = (ids[1], ids[0]);
            (inits[0], inits[1]) = (inits[1], inits[0]);
        }
        uint256 canaryKey = 0xCA0000 + id;
        bytes32 cc;
        {
            OnyxRound.Canary[] memory l = new OnyxRound.Canary[](canary ? 1 : 0);
            if (canary) {
                l[0] = OnyxRound.Canary({shard: ids[0], miner: vm.addr(canaryKey), root: BAD});
            }
            cc = OnyxRound.canaryCommit(l, CANARY_SALT);
        }
        OnyxRound.RoundConfig memory c = _cfg(halfRate, bigPanel, cc);
        uint256 budget = n * (uint256(50) + 10 * ms.length + uint256(5) * c.dispute.panelSize);
        if (st.rewardPool() < budget) {
            usdc.mint(address(this), budget);
            st.fundRewards(budget);
            ghostFunded += budget;
        }
        if (st.coordinatorBond() < MIN_COORD_BOND) {
            usdc.mint(address(this), MIN_COORD_BOND);
            sx.postCoordinatorBond(MIN_COORD_BOND);
        }
        try st.openRound(id, c, ids, inits) {
            ok["openRound"]++;
            rounds.push();
            RoundRec storage r = rounds[rounds.length - 1];
            r.id = id;
            for (uint256 i = 0; i < n; i++) {
                r.shards.push(ids[i]);
            }
            r.canary = canary;
            r.canaryKey = canaryKey;
            r.pendingClose = c.validation.pendingClose;
            r.lockEnd = uint64(c.validation.pendingClose + OnyxRound.disputeTail(c.dispute));
            if (canary) actors.push(vm.addr(canaryKey));
        } catch {}
    }

    function commit(uint256 rSeed, uint256 sSeed, uint256 mSeed, bool good, uint8 extra)
        external
        tracked(1)
    {
        _commit(rSeed, sSeed, mSeed, good, extra);
    }

    function _commit(uint256 rSeed, uint256 sSeed, uint256 mSeed, bool good, uint8 extra) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        bytes32 shard = r.shards[sSeed % r.shards.length];
        uint256 k;
        bytes32 root = good ? GOOD : BAD;
        if (r.canary && mSeed % 5 == 4) {
            k = r.canaryKey;
            shard = r.shards[0];
            root = BAD;
        } else {
            k = mKeys[mSeed % NM];
        }
        address m = vm.addr(k);
        uint128 bond = MIN_BOND + extra;
        bytes32 salt = keccak256(abi.encode(r.id, shard, m));
        bytes32 h =
            OnyxCommit.commitHash(r.id, shard, OnyxCommit.bindingHash(_fields(root)), m, salt);
        bytes32 digest =
            OnyxSig.eip191Digest(OnyxRound.commitMessage(st.roundTag(r.id), shard, h, bond));
        (uint8 v, bytes32 rr, bytes32 ss) = vm.sign(k, digest);
        usdc.mint(m, bond);
        vm.prank(m);
        usdc.approve(address(st), type(uint256).max);
        try st.commit(r.id, shard, m, h, bond, abi.encodePacked(rr, ss, v)) {
            ok["commit"]++;
            commitRecs.push(
                CommitRec({round: r.id, shard: shard, miner: m, bond: bond, salt: salt, root: root})
            );
        } catch {}
    }

    function reveal(uint256 cSeed) external tracked(0) {
        _reveal(cSeed);
    }

    function _reveal(uint256 cSeed) internal {
        if (commitRecs.length == 0) return;
        CommitRec storage c = commitRecs[cSeed % commitRecs.length];
        try st.reveal(c.round, c.shard, c.miner, _fields(c.root), c.salt) {
            ok["reveal"]++;
        } catch {}
    }

    function drawSeed(uint256 rSeed) external tracked(0) {
        _drawSeed(rSeed);
    }

    function _drawSeed(uint256 rSeed) internal {
        if (rounds.length == 0) return;
        try st.drawSeed(_round(rSeed).id) {
            ok["drawSeed"]++;
        } catch {}
    }

    function commitVerdict(uint256 cSeed, uint256 vSeed, uint8 kindSeed, bool skipCheck)
        external
        tracked(0)
    {
        _commitVerdict(cSeed, vSeed, kindSeed, skipCheck);
    }

    function _commitVerdict(uint256 cSeed, uint256 vSeed, uint8 kindSeed, bool skipCheck) internal {
        if (commitRecs.length == 0) return;
        CommitRec storage c = commitRecs[cSeed % commitRecs.length];
        address v = vals[vSeed % NV];
        bytes32 seed = st.seedOf(c.round);
        if (seed == bytes32(0)) return;
        uint64 rate = sx.roundConfig(c.round).validation.ratePpm;
        if (!skipCheck && !OnyxRound.assigned(seed, v, c.shard, c.miner, rate)) return;
        uint8 kind = kindSeed % 4;
        bytes32 root = kind == OnyxRound.DISAGREE ? (c.root == GOOD ? BAD : GOOD) : bytes32(0);
        bytes32 salt = keccak256(abi.encode(c.round, c.shard, v, c.miner));
        bytes32 h = OnyxRound.verdictCommitHash(seed, v, c.shard, c.miner, kind, root, salt);
        vm.prank(v);
        try st.commitVerdict(c.round, c.shard, c.miner, h) {
            ok["commitVerdict"]++;
            verdictRecs.push(
                VerdictRec({
                    round: c.round,
                    shard: c.shard,
                    v: v,
                    miner: c.miner,
                    kind: kind,
                    root: root,
                    salt: salt
                })
            );
        } catch {}
    }

    function revealVerdict(uint256 vrSeed) external tracked(0) {
        _revealVerdict(vrSeed);
    }

    function _revealVerdict(uint256 vrSeed) internal {
        if (verdictRecs.length == 0) return;
        VerdictRec storage x = verdictRecs[vrSeed % verdictRecs.length];
        vm.prank(x.v);
        try st.revealVerdict(x.round, x.shard, x.miner, x.kind, x.root, x.salt) {
            ok["revealVerdict"]++;
        } catch {}
    }

    function openCanaries(uint256 rSeed) external tracked(0) {
        _openCanaries(rSeed);
    }

    function _openCanaries(uint256 rSeed) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        OnyxRound.Canary[] memory l = _canaryList(r);
        try sx.openCanaries(r.id, l, CANARY_SALT) {
            ok["openCanaries"]++;
        } catch {}
    }

    function missCanaryDeadline(uint256 rSeed) external tracked(0) {
        _missCanaryDeadline(rSeed);
    }

    function _missCanaryDeadline(uint256 rSeed) internal {
        if (rounds.length == 0) return;
        try sx.missCanaryDeadline(_round(rSeed).id) {
            ok["missCanary"]++;
        } catch {}
    }

    function finalizeShard(uint256 rSeed, uint256 sSeed) external tracked(0) {
        _finalizeShard(rSeed, sSeed);
    }

    function _finalizeShard(uint256 rSeed, uint256 sSeed) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        bytes32 s = r.shards[sSeed % r.shards.length];
        (uint256 b0, uint256 o0) = _payStart(r.id);
        try st.finalizeShard(r.id, s) {
            ok["finalize"]++;
            if (st.shardStatus(r.id, s) == OnyxLedger.ShardStatus.Finalized) {
                ok["finalized"]++;
                // Rules: one fee per member with a revealed Agree (all reveals share the root).
                address[] memory ms = st.roundMembers(r.id);
                for (uint256 i = 0; i < ms.length; i++) {
                    if (_revealedAgree(r.id, s, ms[i])) {
                        ruleOwed[r.id][s] += FEE;
                        _ruleTo(r.id, s, ms[i], FEE);
                    }
                }
            }
            if (st.shardStatus(r.id, s) == OnyxLedger.ShardStatus.Pending) ok["pending"]++;
        } catch {}
        _payEnd(r.id, s, b0, o0);
    }

    function voidShard(uint256 rSeed, uint256 sSeed) external tracked(0) {
        _voidShard(rSeed, sSeed);
    }

    function _voidShard(uint256 rSeed, uint256 sSeed) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        try st.voidShard(r.id, r.shards[sSeed % r.shards.length]) {
            ok["voidShard"]++;
        } catch {}
    }

    function settleRound(uint256 rSeed) external tracked(0) {
        _settleRound(rSeed);
    }

    function _settleRound(uint256 rSeed) internal {
        if (rounds.length == 0) return;
        try st.settleRound(_round(rSeed).id) {
            ok["settleRound"]++;
        } catch {}
    }

    function refundVoidedShard(uint256 rSeed, uint256 sSeed) external tracked(0) {
        _refundVoidedShard(rSeed, sSeed);
    }

    function _refundVoidedShard(uint256 rSeed, uint256 sSeed) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        try sx.refundVoidedShard(r.id, r.shards[sSeed % r.shards.length]) {
            ok["refundVoided"]++;
        } catch {}
    }

    function reclaimBudget(uint256 rSeed) external tracked(0) {
        _reclaimBudget(rSeed);
    }

    function _reclaimBudget(uint256 rSeed) internal {
        if (rounds.length == 0) return;
        try sx.reclaimBudget(_round(rSeed).id) {
            ok["reclaim"]++;
        } catch {}
    }

    // ------------------------------------------------------------------ disputes

    function openRerun(uint256 rSeed, uint256 sSeed) external tracked(0) {
        _openRerun(rSeed, sSeed);
    }

    function _openRerun(uint256 rSeed, uint256 sSeed) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        bytes32 s = r.shards[sSeed % r.shards.length];
        try dsp.openRerun(r.id, s) returns (bytes32 id) {
            ok["openRerun"]++;
            caseRecs.push(CaseRec({id: id, round: r.id, shard: s, challenge: false}));
            levelBonds[id].push(0);
        } catch {}
    }

    function _pickRoot(uint256 seed, bytes32 shard) internal view returns (bytes32) {
        uint256 k = seed % 3;
        if (k == 0) return GOOD;
        if (k == 1) return BAD;
        return dsp.noValidRootMarker(shard);
    }

    function challenge(uint256 rSeed, uint256 sSeed, uint256 rootSeed, uint8 extra)
        external
        tracked(1)
    {
        _challenge(rSeed, sSeed, rootSeed, extra);
    }

    function _challenge(uint256 rSeed, uint256 sSeed, uint256 rootSeed, uint8 extra) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        bytes32 s = r.shards[sSeed % r.shards.length];
        uint128 bond = 100 + uint128(extra);
        bytes32 claim =
            dsp.claimCommit(st.ledgerId(), r.id, s, CHALLENGER, _pickRoot(rootSeed, s), CLAIM_SALT);
        usdc.mint(CHALLENGER, bond);
        vm.prank(CHALLENGER);
        usdc.approve(address(st), type(uint256).max);
        vm.prank(CHALLENGER);
        try dsp.challenge(r.id, s, claim, bond) returns (bytes32 id) {
            ok["challenge"]++;
            caseRecs.push(CaseRec({id: id, round: r.id, shard: s, challenge: true}));
            levelBonds[id].push(bond);
            claimed[id] = _pickRoot(rootSeed, s);
        } catch {}
    }

    mapping(bytes32 => bytes32) internal claimed;

    function revealClaim(uint256 cSeed) external tracked(0) {
        _revealClaim(cSeed);
    }

    function _revealClaim(uint256 cSeed) internal {
        if (caseRecs.length == 0) return;
        bytes32 id = caseRecs[cSeed % caseRecs.length].id;
        bytes32 root = claimed[id];
        vm.prank(CHALLENGER);
        try dsp.revealClaim(id, root, CLAIM_SALT) {
            ok["revealClaim"]++;
        } catch {}
    }

    function draw(uint256 cSeed) external tracked(0) {
        _draw(cSeed);
    }

    function _draw(uint256 cSeed) internal {
        if (caseRecs.length == 0) return;
        try dsp.draw(caseRecs[cSeed % caseRecs.length].id) {
            ok["draw"]++;
        } catch {}
    }

    function _level(bytes32 id) internal view returns (uint32) {
        (,,,,,, uint256 levels) = dsp.caseInfo(id);
        return uint32(levels - 1);
    }

    function commitVote(uint256 cSeed, uint256 mSeed, uint256 rootSeed) external tracked(0) {
        _commitVote(cSeed, mSeed, rootSeed);
    }

    function _commitVote(uint256 cSeed, uint256 mSeed, uint256 rootSeed) internal {
        if (caseRecs.length == 0) return;
        CaseRec storage c = caseRecs[cSeed % caseRecs.length];
        uint32 lv = _level(c.id);
        (address[] memory ms,,,) = dsp.panel(c.id, lv);
        if (ms.length == 0) return;
        address m = ms[mSeed % ms.length];
        bytes32 root = _pickRoot(rootSeed, c.shard);
        bytes32 salt = keccak256(abi.encode(c.id, lv, m));
        bytes32 h = dsp.panelCommitHash(c.id, lv, m, root, salt);
        vm.prank(m);
        try dsp.commitVote(c.id, h) {
            ok["commitVote"]++;
            voteRecs.push(VoteRec({caseId: c.id, lv: lv, member: m, root: root, salt: salt}));
        } catch {}
    }

    function revealVote(uint256 vSeed) external tracked(0) {
        _revealVote(vSeed);
    }

    function _revealVote(uint256 vSeed) internal {
        if (voteRecs.length == 0) return;
        VoteRec storage x = voteRecs[vSeed % voteRecs.length];
        vm.prank(x.member);
        try dsp.revealVote(x.caseId, x.root, x.salt) {
            ok["revealVote"]++;
        } catch {}
    }

    function appeal(uint256 cSeed, uint256 rootSeed, uint8 extra) external tracked(1) {
        _appeal(cSeed, rootSeed, extra);
    }

    function _appeal(uint256 cSeed, uint256 rootSeed, uint8 extra) internal {
        if (caseRecs.length == 0) return;
        CaseRec storage c = caseRecs[cSeed % caseRecs.length];
        uint128 bond = 200 + uint128(extra);
        bytes32 backed = _pickRoot(rootSeed, c.shard);
        usdc.mint(APPELLANT, bond);
        vm.prank(APPELLANT);
        usdc.approve(address(st), type(uint256).max);
        vm.prank(APPELLANT);
        try dsp.appeal(c.id, backed, bond) {
            ok["appeal"]++;
            levelBonds[c.id].push(bond);
        } catch {}
    }

    function resolve(uint256 cSeed) external tracked(0) {
        _resolve(cSeed);
    }

    function _resolve(uint256 cSeed) internal {
        if (caseRecs.length == 0) return;
        try dsp.resolve(caseRecs[cSeed % caseRecs.length].id) {
            ok["resolve"]++;
        } catch {}
    }

    function arbitrate(uint256 cSeed, uint256 rootSeed) external tracked(0) {
        _arbitrate(cSeed, rootSeed);
    }

    function _arbitrate(uint256 cSeed, uint256 rootSeed) internal {
        if (caseRecs.length == 0) return;
        CaseRec storage c = caseRecs[cSeed % caseRecs.length];
        try dsp.arbitrate(c.id, _pickRoot(rootSeed, c.shard)) {
            ok["arbitrate"]++;
        } catch {}
    }

    function expire(uint256 cSeed) external tracked(0) {
        _expire(cSeed);
    }

    function _expire(uint256 cSeed) internal {
        if (caseRecs.length == 0) return;
        try dsp.expire(caseRecs[cSeed % caseRecs.length].id) {
            ok["expire"]++;
        } catch {}
    }

    function settleCase(uint256 cSeed, uint8 maxMembers) external tracked(0) {
        _settleCase(cSeed, maxMembers);
    }

    function _settleCase(uint256 cSeed, uint8 maxMembers) internal {
        if (caseRecs.length == 0) return;
        _settleCaseMeasured(caseRecs[cSeed % caseRecs.length], 1 + maxMembers % 10);
    }

    function _settleCaseMeasured(CaseRec storage c, uint256 maxMembers) internal {
        (uint256 b0, uint256 o0) = _payStart(c.round);
        try dsp.settleCase(c.id, maxMembers) {
            ok["settleCase"]++;
            (,,, OnyxDisputes.State stt,,,) = dsp.caseInfo(c.id);
            if (stt == OnyxDisputes.State.Done) _modelCase(c);
        } catch {}
        _payEnd(c.round, c.shard, b0, o0);
        extraPaid[c.id] += lastExtra;
    }

    mapping(bytes32 => uint256) public extraPaid;
    uint256 internal lastExtra;

    function release(uint256 rSeed, uint256 sSeed) external tracked(0) {
        _release(rSeed, sSeed);
    }

    function _release(uint256 rSeed, uint256 sSeed) internal {
        if (rounds.length == 0) return;
        RoundRec storage r = _round(rSeed);
        _releaseCounted(r.id, r.shards[sSeed % r.shards.length]);
    }

    /// Release, counting a reward the round budget could no longer pay in full (the budget is
    /// shared by the round's shards and `_spend` caps at what is left; not a conservation
    /// breach, reported in the audit as an open item).
    function _releaseCounted(uint64 id, bytes32 shard) internal {
        (uint256 b0, uint256 o0) = _payStart(id);
        try dsp.release(id, shard) {
            ok["release"]++;
            if (b0 - sx.roundBudget(id) < REWARD) ok["rewardShort"]++;
            // Rules: the earliest reveal of a final unanimous shard earns the miner reward.
            ruleOwed[id][shard] += REWARD;
            (,,, bytes32 root,) = sx.shardState(id, shard);
            _ruleTo(id, shard, _earliest(id, shard, root), REWARD);
        } catch {}
        _payEnd(id, shard, b0, o0);
    }

    // ------------------------------------------------------------------ payout ghosts

    function _payStart(uint64 id) internal returns (uint256 b0, uint256 o0) {
        b0 = _budgetOf(id);
        o0 = sx.owedTotal();
        vm.recordLogs();
        vm.startStateDiffRecording();
    }

    function _budgetOf(uint64 id) internal view returns (uint256) {
        try sx.roundBudget(id) returns (uint128 b) {
            return b;
        } catch {
            return 0;
        }
    }

    /// Attribute what one payout call drew to its shard: budget drained, extra fees credited
    /// from lost bonds, shortfalls recorded; then, if the shard's payouts are complete,
    /// compare with the rules.
    function _payEnd(uint64 id, bytes32 shard, uint256 b0, uint256 o0) internal {
        Vm.AccountAccess[] memory acc = vm.stopAndReturnStateDiff();
        Vm.Log[] memory logs = vm.getRecordedLogs();
        _payeeCheck(id, shard, logs, acc);
        uint256 extra;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != address(st) || logs[i].topics[0] != CREDITED_SIG) continue;
            (uint256 amount, uint8 reason) = abi.decode(logs[i].data, (uint256, uint8));
            if (reason == CR_EXTRA) extra += amount;
        }
        lastExtra = extra;
        uint256 b1 = _budgetOf(id);
        uint256 o1 = sx.owedTotal();
        if (b1 > b0) _breach("a payout call raised the round budget");
        if (o1 < o0) _breach("a payout call cleared an owed record");
        seenPaid[id][shard] += (b0 - b1) + extra + (o1 - o0);
        seenOwed[id][shard] += o1 - o0;
        if (_payComplete(id, shard) && !_payMatches(id, shard)) {
            _breach("payouts differ from the rules");
        }
    }

    /// False while a case on the shard is judged part way (it has paid only some members).
    function _payComplete(uint64 id, bytes32 shard) internal view returns (bool) {
        bytes32 cid = dsp.caseOf(id, shard);
        if (cid == bytes32(0)) return true;
        (,,, OnyxDisputes.State stt,,,) = dsp.caseInfo(cid);
        return stt != OnyxDisputes.State.Decided;
    }

    function _payMatches(uint64 id, bytes32 shard) internal view returns (bool) {
        address[] storage p = payees[id][shard];
        for (uint256 i = 0; i < p.length; i++) {
            if (ruleBy[id][shard][p[i]] != seenBy[id][shard][p[i]]) return false;
        }
        return
            seenPaid[id][shard] == ruleOwed[id][shard]
                && seenOwed[id][shard] == ruleShort[id][shard];
    }

    function _payee(uint64 id, bytes32 shard, address who) internal {
        if (isPayee[id][shard][who]) return;
        isPayee[id][shard][who] = true;
        payees[id][shard].push(who);
    }

    function _ruleTo(uint64 id, bytes32 shard, address who, uint256 amount) internal {
        _payee(id, shard, who);
        ruleBy[id][shard][who] += amount;
    }

    function _seenTo(uint64 id, bytes32 shard, address who, uint256 amount) internal {
        _payee(id, shard, who);
        seenBy[id][shard][who] += amount;
    }

    /// The earliest (lowest sequence) revealed non-canary commit on `root`: the miner the
    /// rules give the reward (Rust `settle`), independent of the contract's own pick.
    function _earliest(uint64 id, bytes32 shard, bytes32 root) internal view returns (address w) {
        uint256 best = type(uint256).max;
        for (uint256 k = 0; k < commitRecs.length; k++) {
            CommitRec storage x = commitRecs[k];
            if (x.round != id || x.shard != shard || x.root != root) continue;
            OnyxLedger.Commit memory cm = sx.commitOf(id, shard, x.miner);
            if (!cm.revealed || cm.canary || cm.seq >= best) continue;
            (best, w) = (cm.seq, x.miner);
        }
    }

    /// Per payee, from one payout call: what the dispute game asked the settlement to pay
    /// (`dSpend`, `dExtra`; `dCredit` for escrow returns) and finalize's verdict fees, against
    /// what was credited (`Credited` reasons 4, 5, 7) or recorded short (`SpendShort`). A call
    /// whose credit lands on another address than it named is a breach; the named amounts per
    /// payee go to `seenBy`.
    function _payeeCheck(
        uint64 id,
        bytes32 shard,
        Vm.Log[] memory logs,
        Vm.AccountAccess[] memory acc
    ) internal {
        address[] memory who = new address[](logs.length + acc.length);
        int256[] memory net = new int256[](who.length);
        uint256 n;
        for (uint256 i = 0; i < acc.length; i++) {
            Vm.AccountAccess memory a = acc[i];
            if (
                a.kind != VmSafe.AccountAccessKind.Call || a.reverted || a.account != address(st)
                    || a.accessor != address(dsp) || a.data.length < 4
            ) continue;
            bytes4 sel = bytes4(a.data);
            bytes memory args = _args(a.data);
            address to;
            uint256 amount;
            if (sel == OnyxSettlementExt.dSpend.selector) {
                (uint64 r, bytes32 s, address t, uint128 m) =
                    abi.decode(args, (uint64, bytes32, address, uint128));
                (to, amount) = (t, m);
                _seenTo(r, s, to, amount);
            } else if (sel == OnyxSettlementExt.dExtra.selector) {
                (uint64 r, bytes32 s, address t, uint256 m,) =
                    abi.decode(args, (uint64, bytes32, address, uint256, uint256));
                (to, amount) = (t, m);
                _seenTo(r, s, to, amount);
            } else if (sel == OnyxSettlementExt.dCredit.selector) {
                (to, amount) = abi.decode(args, (address, uint256));
            } else {
                continue;
            }
            n = _addNet(who, net, n, to, int256(amount));
        }
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != address(st)) continue;
            bytes32 sig = logs[i].topics[0];
            if (sig == CREDITED_SIG) {
                address to = address(uint160(uint256(logs[i].topics[1])));
                (uint256 amount, uint8 reason) = abi.decode(logs[i].data, (uint256, uint8));
                if (reason == CR_VERDICT) {
                    _seenTo(id, shard, to, amount);
                } else if (reason == CR_DISPUTE || reason == CR_EXTRA) {
                    n = _addNet(who, net, n, to, -int256(amount));
                }
            } else if (sig == SHORT_SIG) {
                address to = address(uint160(uint256(logs[i].topics[3])));
                (uint256 owed, uint256 paid) = abi.decode(logs[i].data, (uint256, uint256));
                n = _addNet(who, net, n, to, -int256(owed - paid));
            }
        }
        for (uint256 i = 0; i < n; i++) {
            if (net[i] != 0) _breach("a payout credited another address than it named");
        }
    }

    function _addNet(address[] memory who, int256[] memory net, uint256 n, address to, int256 d)
        internal
        pure
        returns (uint256)
    {
        for (uint256 i = 0; i < n; i++) {
            if (who[i] == to) {
                net[i] += d;
                return n;
            }
        }
        (who[n], net[n]) = (to, d);
        return n + 1;
    }

    function _args(bytes memory data) internal pure returns (bytes memory out) {
        out = new bytes(data.length - 4);
        for (uint256 i = 0; i < out.length; i++) {
            out[i] = data[i + 4];
        }
    }

    function _revealedAgree(uint64 id, bytes32 shard, address v) internal view returns (bool) {
        for (uint256 k = 0; k < verdictRecs.length; k++) {
            VerdictRec storage x = verdictRecs[k];
            if (x.round != id || x.shard != shard || x.v != v || x.kind != OnyxRound.AGREE) {
                continue;
            }
            if (sx.verdictOf(id, shard, x.miner, v).revealed) return true;
        }
        return false;
    }

    function _minerRoot(uint64 id, bytes32 shard, address m) internal view returns (bytes32) {
        for (uint256 k = 0; k < commitRecs.length; k++) {
            CommitRec storage c = commitRecs[k];
            if (c.round == id && c.shard == shard && c.miner == m) return c.root;
        }
        return bytes32(0);
    }

    /// Rust `verdict_right`, written out here (independent of the contract): 0 = Abstain
    /// (not judged), 1 = right, 2 = wrong.
    function _right(uint8 kind, bytes32 vRoot, bytes32 mRoot, bytes32 ref, bool noValid)
        internal
        pure
        returns (uint8)
    {
        if (kind == OnyxRound.ABSTAIN) return 0;
        bool right;
        if (noValid) right = kind == OnyxRound.NO_VALID_ROOT;
        else if (kind == OnyxRound.AGREE) right = mRoot == ref;
        else if (kind == OnyxRound.DISAGREE) right = mRoot != ref && vRoot == ref;
        return right ? 1 : 2;
    }

    /// What the rules owe on a judged case (ADR-0024 item 8, ADR-0022 Amendment 1): the miner
    /// reward to the earliest correct reveal; on a rerun case, each member's fee once per
    /// revealed root it judged right (canaries share one key), of which the first comes from
    /// the shard's reservation and the rest from the lost miner bonds while they last (the
    /// uncovered rest is owed), and `panelFee` per first-panel member who revealed the root.
    function _modelCase(CaseRec storage c) internal {
        (,,,, bytes32 ref,,) = dsp.caseInfo(c.id);
        bytes32 marker = keccak256(abi.encodePacked("onyx-no-valid-root-v1", c.shard));
        bool noValid = ref == marker;
        uint256 owed;
        uint256 lost;
        for (uint256 k = 0; k < commitRecs.length; k++) {
            CommitRec storage x = commitRecs[k];
            if (x.round != c.round || x.shard != c.shard) continue;
            OnyxLedger.Commit memory cm = sx.commitOf(c.round, c.shard, x.miner);
            if (!cm.revealed || cm.canary) continue;
            if (x.root != ref) lost += cm.bond;
        }
        address winner = _earliest(c.round, c.shard, ref);
        if (winner != address(0)) {
            owed += REWARD;
            _ruleTo(c.round, c.shard, winner, REWARD);
        }
        if (!c.challenge) {
            uint256 extras;
            address[] memory ms = st.roundMembers(c.round);
            for (uint256 i = 0; i < ms.length; i++) {
                uint256 n = _keysRight(c, ms[i], ref, noValid);
                if (n > 0) {
                    owed += FEE * n;
                    extras += FEE * (n - 1);
                    _ruleTo(c.round, c.shard, ms[i], FEE * n);
                }
            }
            (address[] memory panel0,,,) = dsp.panel(c.id, 0);
            for (uint256 i = 0; i < panel0.length; i++) {
                (, bool revealed,, bytes32 root) = dsp.votes(c.id, 0, panel0[i]);
                if (revealed && root == ref) {
                    owed += PANEL_FEE;
                    _ruleTo(c.round, c.shard, panel0[i], PANEL_FEE);
                }
            }
            ruleShort[c.round][c.shard] += extras > lost ? extras - lost : 0;
        }
        ruleOwed[c.round][c.shard] += owed;
    }

    /// Distinct revealed roots (one key for all canaries) on which `v` revealed a right verdict.
    function _keysRight(CaseRec storage c, address v, bytes32 ref, bool noValid)
        internal
        view
        returns (uint256 n)
    {
        bytes32[] memory keys = new bytes32[](verdictRecs.length);
        for (uint256 k = 0; k < verdictRecs.length; k++) {
            VerdictRec storage x = verdictRecs[k];
            if (x.round != c.round || x.shard != c.shard || x.v != v) continue;
            if (!sx.verdictOf(c.round, c.shard, x.miner, v).revealed) continue;
            OnyxLedger.Commit memory cm = sx.commitOf(c.round, c.shard, x.miner);
            bytes32 mRoot = _minerRoot(c.round, c.shard, x.miner);
            if (_right(x.kind, x.root, mRoot, ref, noValid) != 1) continue;
            bytes32 key = cm.canary ? bytes32(0) : mRoot;
            bool seen;
            for (uint256 j = 0; j < n; j++) {
                if (keys[j] == key) seen = true;
            }
            if (!seen) keys[n++] = key;
        }
    }

    /// Shards whose completed payouts differ from the rules (a case judged part way is
    /// skipped until it is Done).
    function payMismatches() external view returns (uint256 bad) {
        for (uint256 i = 0; i < rounds.length; i++) {
            RoundRec storage r = rounds[i];
            for (uint256 j = 0; j < r.shards.length; j++) {
                bytes32 s = r.shards[j];
                if (_payComplete(r.id, s) && !_payMatches(r.id, s)) bad++;
            }
        }
    }

    /// Shortfalls measured as recorded, summed over shards (each shard's equals the model's
    /// once its payouts are complete: `payMismatches`).
    function sumSeenOwed() external view returns (uint256 t) {
        for (uint256 i = 0; i < rounds.length; i++) {
            RoundRec storage r = rounds[i];
            for (uint256 j = 0; j < r.shards.length; j++) {
                t += seenOwed[r.id][r.shards[j]];
            }
        }
    }

    function claimOwed(uint256 aSeed) external tracked(0) {
        address a = actors[aSeed % actors.length];
        uint256 o0 = sx.owedOf(a);
        uint256 c0 = st.balanceOf(a);
        try sx.claimOwed(a) {
            ok["claimOwed"]++;
            uint256 paid = st.balanceOf(a) - c0;
            if (o0 - sx.owedOf(a) != paid) _breach("claimOwed paid other than it cleared");
            ghostClaimed += paid;
        } catch {}
    }

    uint256 public ghostClaimed;

    // ------------------------------------------------------------------ time

    /// Move forward to a phase boundary of a round (or a few blocks): reveals, seed, verdicts,
    /// canaries, pendingClose, lockEnd. Never backwards.
    function warp(uint256 rSeed, uint8 which, uint8 extra) external tracked(0) {
        _warp(rSeed, which, extra);
    }

    function _warp(uint256 rSeed, uint8 which, uint8 extra) internal {
        uint256 target = block.number + 1 + extra % 40;
        if (rounds.length > 0 && which % 9 < 7) {
            RoundRec storage r = _round(rSeed);
            OnyxRound.RoundConfig memory c = sx.roundConfig(r.id);
            uint256[7] memory at = [
                uint256(c.commitClose),
                uint256(c.beaconHeight) + 1,
                uint256(c.validation.verdictCommitClose),
                uint256(c.validation.verdictRevealClose),
                uint256(c.validation.canaryOpenClose),
                uint256(c.validation.pendingClose),
                uint256(r.lockEnd)
            ];
            uint256 t = at[which % 9];
            if (t > block.number) target = t;
        }
        vm.roll(target);
    }

    // ------------------------------------------------------------------ whole lifecycles

    function _rand(uint256 seed, string memory tag, uint256 a, uint256 b)
        internal
        pure
        returns (uint256)
    {
        return uint256(keccak256(abi.encode(seed, tag, a, b)));
    }

    /// One round from open to settle in a single call, with random choices at every step
    /// (who commits, good or bad root, who reveals, verdict kinds, canary opened or missed),
    /// so deep states are reached at any depth. Random single calls interleave around it.
    function autoRound(uint256 seed) external tracked(1) {
        uint256 before = rounds.length;
        _openRound(uint8(seed), (seed >> 8) % 3 == 0, (seed >> 10) & 1 == 1, (seed >> 11) & 1 == 1);
        if (rounds.length == before) return;
        ok["autoRound"]++;
        uint256 ri = rounds.length - 1;
        RoundRec storage r = rounds[ri];
        uint256 c0 = commitRecs.length;
        uint256 nm = NM + (r.canary ? 1 : 0);
        for (uint256 si = 0; si < r.shards.length; si++) {
            for (uint256 mi = 0; mi < nm; mi++) {
                uint256 x = _rand(seed, "commit", si, mi);
                if (x % 10 < 2) continue;
                // Half the rounds have only honest miners; the rest 10% bad roots.
                _commit(
                    ri, si, mi == NM ? 4 : mi, (seed >> 40) & 1 == 0 || x % 10 < 9, uint8(x >> 8)
                );
            }
        }
        OnyxRound.RoundConfig memory c = sx.roundConfig(r.id);
        vm.roll(c.commitClose);
        for (uint256 k = c0; k < commitRecs.length; k++) {
            if (_rand(seed, "reveal", k, 0) % 10 < 8) _reveal(k);
        }
        vm.roll(uint256(c.beaconHeight) + 1);
        if (_rand(seed, "seed", 0, 0) % 20 != 0) _drawSeed(ri);
        uint256 v0 = verdictRecs.length;
        for (uint256 k = c0; k < commitRecs.length; k++) {
            for (uint256 vi = 0; vi < NV; vi++) {
                // About 40% of validators sit the round out: unanimity needs a drawable panel
                // of members who gave no verdict (Rust L29-F4).
                if (_rand(seed, "active", vi, 0) % 10 < 4) continue;
                uint256 x = _rand(seed, "verdict", k, vi);
                if (x % 100 < 10) continue;
                // Honest: Agree with a good root, Disagree with a bad one; 5% anything.
                uint8 kind = commitRecs[k].root == GOOD ? OnyxRound.AGREE : OnyxRound.DISAGREE;
                if ((x >> 8) % 20 == 0) kind = uint8((x >> 16) % 4);
                _commitVerdict(k, vi, kind, false);
            }
        }
        vm.roll(c.validation.verdictCommitClose);
        for (uint256 k = v0; k < verdictRecs.length; k++) {
            if (_rand(seed, "vreveal", k, 0) % 10 < 9) _revealVerdict(k);
        }
        vm.roll(c.validation.verdictRevealClose);
        if (_rand(seed, "canary", 0, 0) % 5 != 0) _openCanaries(ri);
        else vm.roll(c.validation.canaryOpenClose);
        for (uint256 si = 0; si < r.shards.length; si++) {
            if (_rand(seed, "settle", si, 0) % 10 != 0) _finalizeShard(ri, si);
        }
    }

    /// One dispute case from open to judging in a single call: a rerun case on a Pending
    /// shard or a challenge on a Finalized one, its panel(s) voting at random, an appeal at
    /// random, then resolve, arbitration or not, the claim revealed or not, and settleCase.
    function autoCase(uint256 seed) external tracked(1) {
        if (rounds.length == 0) return;
        uint256 ri = seed % rounds.length;
        RoundRec storage r = rounds[ri];
        uint256 si = (seed >> 8) % r.shards.length;
        uint256 ci = caseRecs.length;
        OnyxLedger.ShardStatus s = st.shardStatus(r.id, r.shards[si]);
        if (s == OnyxLedger.ShardStatus.Pending) {
            _openRerun(ri, si);
        } else if (s == OnyxLedger.ShardStatus.Finalized) {
            _challenge(ri, si, seed >> 16, uint8(seed >> 24));
        }
        if (caseRecs.length == ci) return;
        ok["autoCase"]++;
        bytes32 id = caseRecs[ci].id;
        uint32 lv;
        for (;;) {
            (,, uint64 anchor,) = dsp.panel(id, lv);
            if (block.number <= anchor) vm.roll(uint256(anchor) + 1);
            _draw(ci);
            (address[] memory ms, uint64 at,,) = dsp.panel(id, lv);
            if (ms.length == 0) break;
            uint256 w0 = voteRecs.length;
            for (uint256 j = 0; j < ms.length; j++) {
                uint256 x = _rand(seed, "vote", lv, j);
                // 30% silent: a panel that is not unanimous can be appealed.
                if (x % 100 < 30) continue;
                uint256 k = (x >> 8) % 10;
                _commitVote(ci, j, k < 7 ? 0 : (k < 9 ? 1 : 2));
            }
            vm.roll(uint256(at) + 10);
            for (uint256 k = w0; k < voteRecs.length; k++) {
                if (_rand(seed, "preveal", k, 0) % 10 < 9) _revealVote(k);
            }
            vm.roll(uint256(at) + 20);
            if (lv == 0 && _rand(seed, "appeal", 0, 0) % 3 == 0) {
                (,,,,,, uint256 n0) = dsp.caseInfo(id);
                _appeal(ci, _rand(seed, "backed", 0, 0), uint8(seed >> 32));
                (,,,,,, uint256 n1) = dsp.caseInfo(id);
                if (n1 > n0) {
                    lv++;
                    continue;
                }
            }
            break;
        }
        {
            (, uint64 drawnAt, uint64 anchor2,) = dsp.panel(id, lv);
            uint256 end = drawnAt == 0 ? uint256(anchor2) + 257 : uint256(drawnAt) + 30;
            if (end > block.number) vm.roll(end);
        }
        _resolve(ci);
        (,,, OnyxDisputes.State stt,,,) = dsp.caseInfo(id);
        if (stt == OnyxDisputes.State.RerunRequired && _rand(seed, "arb", 0, 0) % 10 < 7) {
            _arbitrate(ci, _rand(seed, "arbroot", 0, 0));
        }
        if (caseRecs[ci].challenge && _rand(seed, "claim", 0, 0) % 10 < 7) _revealClaim(ci);
        vm.roll(block.number + 16);
        _settleCase(ci, 9);
    }

    // ------------------------------------------------------------------ roles

    /// A non-role account tries a privileged entry point: every one must revert.
    function attack(uint8 which, uint256 aSeed) external tracked(0) {
        _attack(which, aSeed);
    }

    function _attack(uint8 which, uint256 aSeed) internal {
        address a = actors[1 + aSeed % (actors.length - 1)]; // never the coordinator
        bool success;
        uint256 w = which % 12;
        if (w == 0) {
            OnyxRound.RoundConfig memory c = _cfg(false, false, bytes32(0));
            bytes32[] memory ids = new bytes32[](1);
            ids[0] = keccak256("x");
            OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
            vm.prank(a);
            try st.openRound(999, c, ids, inits) {
                success = true;
            } catch {}
        } else if (w == 1 && rounds.length > 0) {
            RoundRec storage r = _round(aSeed);
            OnyxRound.Canary[] memory l = _canaryList(r);
            uint64 id = r.id;
            vm.prank(a);
            try sx.openCanaries(id, l, CANARY_SALT) {
                success = true;
            } catch {}
        } else if (w == 2) {
            vm.prank(a);
            try sx.dCredit(a, 1) {
                success = true;
            } catch {}
        } else if (w == 3) {
            address v = vals[aSeed % NV];
            vm.prank(a);
            try sx.dSlash(v, 1) {
                success = true;
            } catch {}
        } else if (w == 4) {
            vm.prank(a);
            try sx.dBurn(1) {
                success = true;
            } catch {}
        } else if (w == 5 && rounds.length > 0) {
            RoundRec storage r = _round(aSeed);
            (uint64 id, bytes32 s) = (r.id, r.shards[0]);
            vm.prank(a);
            try sx.dBegin(id, s) {
                success = true;
            } catch {}
            vm.prank(a);
            try sx.dEnd(id, s, OnyxLedger.ShardStatus.Finalized, keccak256("x"), bytes32(0)) {
                success = true;
            } catch {}
            vm.prank(a);
            try sx.dSpend(id, s, a, 1) {
                success = true;
            } catch {}
        } else if (w == 6) {
            vm.prank(a);
            try sx.dPull(a, 0) {
                success = true;
            } catch {}
        } else if (w == 7 && caseRecs.length > 0) {
            bytes32 id = caseRecs[aSeed % caseRecs.length].id;
            vm.prank(a);
            try dsp.arbitrate(id, GOOD) {
                success = true;
            } catch {}
        } else if (w == 8) {
            vm.prank(a);
            try dsp.bind(sx) {
                success = true;
            } catch {}
            // The deployer itself cannot bind a second time.
            try dsp.bind(sx) {
                success = true;
            } catch {}
        } else if (w == 9) {
            // The extension called directly (not through the settlement) refuses the hooks even
            // from the dispute contract's own address.
            OnyxSettlementExt e = OnyxSettlementExt(st.ext());
            vm.prank(address(dsp));
            try e.dCredit(a, 1) {
                success = true;
            } catch {}
            vm.prank(address(dsp));
            try e.dBurn(1) {
                success = true;
            } catch {}
        } else if (w == 10) {
            vm.prank(a);
            try sx.withdrawCoordinatorBond() {
                success = true;
            } catch {}
            vm.prank(a);
            try sx.postCoordinatorBond(0) {
                success = true;
            } catch {}
        } else if (w == 11) {
            vm.prank(a);
            try sx.missCanaryDeadline(type(uint64).max) {
                success = true;
            } catch {}
        }
        if (success) _breach("a privileged call succeeded from a non-role account");
        ok["attack"]++;
    }

    // ------------------------------------------------------------------ views for invariants

    function roundsLength() external view returns (uint256) {
        return rounds.length;
    }

    function actorsLength() external view returns (uint256) {
        return actors.length;
    }

    function roundShards(uint256 i) external view returns (uint64 id, bytes32[] memory s) {
        return (rounds[i].id, rounds[i].shards);
    }

    function _caseIsChallenge(uint64 round, bytes32 shard) internal view returns (bool) {
        bytes32 id = dsp.caseOf(round, shard);
        (,, OnyxDisputes.Kind kind,,,,) = dsp.caseInfo(id);
        return kind == OnyxDisputes.Kind.Challenge;
    }

    /// Miner and dispute bonds the settlement still holds for someone (not credited, not burned).
    function escrow() public view returns (uint256 e) {
        for (uint256 i = 0; i < commitRecs.length; i++) {
            CommitRec storage c = commitRecs[i];
            (OnyxLedger.ShardStatus s, bool closed,,,) = sx.shardState(c.round, c.shard);
            bool revealed = sx.commitOf(c.round, c.shard, c.miner).revealed;
            if (s == OnyxLedger.ShardStatus.Open || s == OnyxLedger.ShardStatus.Pending) {
                e += c.bond;
            } else if (s == OnyxLedger.ShardStatus.InCase) {
                // A challenged shard's unrevealed bonds burned at finalize.
                if (!_caseIsChallenge(c.round, c.shard) || revealed) e += c.bond;
            } else if (
                s == OnyxLedger.ShardStatus.Finalized && !closed && !dsp.released(c.round, c.shard)
                    && revealed
            ) {
                e += c.bond;
            }
        }
        for (uint256 i = 0; i < caseRecs.length; i++) {
            bytes32 id = caseRecs[i].id;
            (,,, OnyxDisputes.State s,,,) = dsp.caseInfo(id);
            if (
                s == OnyxDisputes.State.Open || s == OnyxDisputes.State.RerunRequired
                    || s == OnyxDisputes.State.Decided
            ) {
                if (s == OnyxDisputes.State.Decided) e -= extraPaid[id];
                uint128[] storage b = levelBonds[id];
                for (uint256 j = 0; j < b.length; j++) {
                    e += b[j];
                }
            }
        }
    }

    function sumBudgets() public view returns (uint256 b) {
        for (uint256 i = 0; i < rounds.length; i++) {
            b += sx.roundBudget(rounds[i].id);
        }
    }

    function sumCredits() public view returns (uint256 b) {
        for (uint256 i = 0; i < actors.length; i++) {
            b += st.balanceOf(actors[i]);
        }
    }

    function sumStakes() public view returns (uint256 b) {
        for (uint256 i = 0; i < NV; i++) {
            (uint128 s,,,) = st.validators(vals[i]);
            b += s;
        }
    }

    /// Everything the settlement owes or holds, plus what it burned: must equal its balance.
    function liabilities() public view returns (uint256) {
        return sumCredits() + sumStakes() + st.rewardPool() + st.coordinatorBond() + sumBudgets()
            + st.burned() + escrow();
    }

    /// `panelSlash` for every seat on a drawn panel of a case still running.
    function expectedExposure(address v) public view returns (uint256 x) {
        for (uint256 i = 0; i < caseRecs.length; i++) {
            bytes32 id = caseRecs[i].id;
            (,,, OnyxDisputes.State s,,, uint256 levels) = dsp.caseInfo(id);
            if (
                s != OnyxDisputes.State.Open && s != OnyxDisputes.State.RerunRequired
                    && s != OnyxDisputes.State.Decided
            ) continue;
            for (uint32 lv = 0; lv < levels; lv++) {
                (address[] memory ms,,,) = dsp.panel(id, lv);
                for (uint256 j = 0; j < ms.length; j++) {
                    if (ms[j] == v) x += PANEL_SLASH;
                }
            }
        }
    }

    // ------------------------------------------------------------------ full lifecycle

    /// Drive every round, shard and case to its end, unbond everyone and withdraw everything
    /// (called once after each run by `afterInvariant`).
    function finishAll() external {
        uint256 far = block.number;
        for (uint256 i = 0; i < rounds.length; i++) {
            if (rounds[i].lockEnd > far) far = rounds[i].lockEnd;
        }
        vm.roll(far + 1);
        for (uint256 i = 0; i < rounds.length; i++) {
            RoundRec storage r = rounds[i];
            for (uint256 j = 0; j < r.shards.length; j++) {
                try st.voidShard(r.id, r.shards[j]) {} catch {}
                try sx.refundVoidedShard(r.id, r.shards[j]) {} catch {}
            }
        }
        for (uint256 pass = 0; pass < 2; pass++) {
            for (uint256 i = 0; i < caseRecs.length; i++) {
                bytes32 id = caseRecs[i].id;
                try dsp.resolve(id) {} catch {}
                try dsp.expire(id) {} catch {}
                _settleCaseMeasured(caseRecs[i], 1_000);
            }
        }
        for (uint256 i = 0; i < rounds.length; i++) {
            RoundRec storage r = rounds[i];
            for (uint256 j = 0; j < r.shards.length; j++) {
                _releaseCounted(r.id, r.shards[j]);
            }
            try sx.reclaimBudget(r.id) {} catch {}
        }
        for (uint256 i = 0; i < NV; i++) {
            vm.prank(vals[i]);
            try sx.requestUnbond() {} catch {}
        }
        vm.roll(block.number + UNBOND_DELAY + 1);
        for (uint256 i = 0; i < NV; i++) {
            vm.prank(vals[i]);
            try sx.withdrawStake() {} catch {}
        }
        try sx.withdrawCoordinatorBond() {} catch {}
        // Shortfalls recorded owed are paid from the reward pool (round leftovers returned to
        // it by `reclaimBudget` above).
        for (uint256 i = 0; i < actors.length; i++) {
            uint256 c0 = st.balanceOf(actors[i]);
            try sx.claimOwed(actors[i]) {} catch {}
            ghostClaimed += st.balanceOf(actors[i]) - c0;
        }
        uint256 before = usdc.balanceOf(address(st));
        for (uint256 i = 0; i < actors.length; i++) {
            if (actors[i] != address(this)) vm.prank(actors[i]);
            try st.withdraw() {} catch {}
        }
        ghostOut += before - usdc.balanceOf(address(st));
    }

    /// True when every shard ended and nothing is left to judge.
    function allEnded() external view returns (bool) {
        for (uint256 i = 0; i < rounds.length; i++) {
            RoundRec storage r = rounds[i];
            for (uint256 j = 0; j < r.shards.length; j++) {
                (OnyxLedger.ShardStatus s, bool closed,,,) = sx.shardState(r.id, r.shards[j]);
                if (
                    s == OnyxLedger.ShardStatus.Open || s == OnyxLedger.ShardStatus.Pending
                        || s == OnyxLedger.ShardStatus.InCase
                ) return false;
                if (s == OnyxLedger.ShardStatus.Finalized && !closed) {
                    if (!dsp.released(r.id, r.shards[j])) return false;
                }
            }
        }
        for (uint256 i = 0; i < caseRecs.length; i++) {
            (,,, OnyxDisputes.State s,,,) = dsp.caseInfo(caseRecs[i].id);
            if (s != OnyxDisputes.State.Done && s != OnyxDisputes.State.Unresolved) return false;
        }
        return true;
    }

    function stat(string memory k) external view returns (uint256) {
        return ok[bytes32(bytes(k))];
    }
}
