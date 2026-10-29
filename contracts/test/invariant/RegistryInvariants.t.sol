// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, console, Vm} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxLedger} from "../../src/OnyxLedger.sol";
import {OnyxRegistry, IRegistryLedger} from "../../src/OnyxRegistry.sol";
import {OnyxSig} from "../../src/OnyxSig.sol";

/// A ledger for the registry: its coordinator can change (bumping the epoch, as a ledger with a
/// mutable coordinator must), and shards can be opened or finalized by the handler.
contract RegLedger is IRegistryLedger {
    address public coordinator;
    uint64 public coordinatorEpoch;
    mapping(uint64 => mapping(bytes32 => OnyxLedger.ShardStatus)) internal status;

    constructor(address c) {
        coordinator = c;
    }

    function rotate(address c) external {
        coordinator = c;
        coordinatorEpoch++;
    }

    function setStatus(uint64 r, bytes32 s, OnyxLedger.ShardStatus x) external {
        status[r][s] = x;
    }

    function isFinal(uint64 r, bytes32 s) external view returns (bool) {
        return status[r][s] == OnyxLedger.ShardStatus.Finalized;
    }

    function shardState(uint64 r, bytes32 s)
        external
        view
        returns (OnyxLedger.ShardStatus, bool, uint64, bytes32, bytes32)
    {
        return (status[r][s], false, 0, bytes32(0), bytes32(0));
    }

    function verifyRow(uint64 r, bytes32 s, bytes calldata, uint64, uint64, bytes32[] calldata)
        external
        view
        returns (bool)
    {
        return status[r][s] == OnyxLedger.ShardStatus.Finalized;
    }
}

/// Handler for the OnyxRegistry invariants (L2 contract audit): owner registrations, shard
/// attestations, level-2 draws (announce, beacon, record), subject draw bounds and reopens,
/// receipt signers across coordinator changes, the transcript log, and non-role calls.
contract RegistryHandler is Test {
    RegLedger public ledger;
    OnyxRegistry public reg;

    address public constant OWNER = address(0x0E1);
    address[2] public coords = [address(0xC01), address(0xC02)];
    uint256[2] internal signerKeys = [uint256(0x5161), 0x5162];
    uint256[3] internal observerKeys = [uint256(0x0B51), 0x0B52, 0x0B53];

    bytes32[4] public metricIds =
        [keccak256("m.recipe"), keccak256("m.feed"), keccak256("m.dated"), keccak256("m.recipe2")];
    bytes32[3] public subjects = [keccak256("s.a"), keccak256("s.b"), keccak256("s.c")];
    uint64[3] public windows = [uint64(100), 200, 300];
    bytes32[2] public labelSets = [keccak256("labels 1"), keccak256("labels 2")];
    bytes32 public constant RECIPE = keccak256("recipe id");

    // Snapshots for immutability checks.
    mapping(bytes32 => OnyxRegistry.Kind) public kindSeen;
    mapping(bytes32 => bytes32) public templateSeen;
    mapping(bytes32 => uint8) public labelLevelSeen;
    mapping(bytes32 => uint8) public recipeFloorSeen;
    bytes32[] public facts;
    mapping(bytes32 => bool) internal factKnown;
    mapping(bytes32 => mapping(uint32 => bytes32)) public drawCommitSeen;
    mapping(bytes32 => mapping(uint32 => uint64)) public drawHeightSeen;
    mapping(bytes32 => mapping(uint32 => bytes32)) public beaconSeen;
    mapping(bytes32 => bytes32) public resultSeen;
    /// The commit inputs of each fact's latest announced draw (so records can match it).
    mapping(bytes32 => uint256) internal drawX;
    mapping(bytes32 => bytes32) public specSeen;
    mapping(bytes32 => uint256) public announced;
    mapping(bytes32 => uint256) public recorded;
    mapping(bytes32 => uint256) public reopened;
    bytes32[] public attested;
    mapping(bytes32 => uint8) public attestedLevel;
    bytes32[] public logSlots;
    mapping(bytes32 => bytes32) public logSeen;
    mapping(bytes32 => bytes32) public receiptSeen;
    /// Who set the receipt signer last, in which epoch, and to what.
    address public signerBy;
    uint64 public signerEpoch;
    address public signerSet;
    /// Who set the reveal key last (amendment 10), in which epoch, and to what (x only).
    address public revealBy;
    uint64 public revealEpoch;
    bytes32 public revealSetX;
    bytes32 internal revealSetY;
    uint256[2] internal revealKeys = [uint256(0x4241), 0x4242];
    /// Sealed post slots (`logKey`) -> keccak256 of the ciphertext first posted.
    mapping(bytes32 => bytes32) public sealedSeen;
    bytes32[] public sealedSlots;
    /// (fact, attempt) records: the digest first recorded.
    mapping(bytes32 => mapping(uint32 => bytes32)) public attemptSeen;
    bytes32[] public recordKeys;
    mapping(bytes32 => bytes32) public recordFact;
    mapping(bytes32 => uint32) public recordAttemptOf;
    /// Amendment 14 (ROADMAP C51): whether `requireTimeLock` succeeded, and each draw's
    /// `drawTimeLocked` as announced.
    bool public timeLockOn;
    mapping(bytes32 => mapping(uint32 => bool)) public drawTimeLockSeen;
    uint64 public constant L_OPEN = 1;
    uint64 public constant L_CLOSE = 1_000;
    uint64 public constant L_RCLOSE = 2_000;

    uint256 public breaches;
    string public lastBreach;
    mapping(bytes32 => uint256) public ok;

    constructor() {
        ledger = new RegLedger(coords[0]);
        reg = new OnyxRegistry(OWNER, IRegistryLedger(address(ledger)));
        // A registered feed, a dated feed, a recipe metric, one label set and a receipt signer,
        // so draws and attestations are reachable from the first call; the rest is random.
        _initMetric(
            metricIds[1], OnyxRegistry.Kind.Feed, keccak256(abi.encode("template", 1, false))
        );
        _initMetric(
            metricIds[2], OnyxRegistry.Kind.DatedFeed, keccak256(abi.encode("template", 2, false))
        );
        _initMetric(metricIds[0], OnyxRegistry.Kind.Recipe, RECIPE);
        vm.prank(OWNER);
        reg.registerLabelSet(labelSets[0], 2);
        labelLevelSeen[labelSets[0]] = 2;
        address signer = vm.addr(signerKeys[0]);
        vm.prank(coords[0]);
        reg.setReceiptSigner(signer);
        (signerBy, signerEpoch, signerSet) = (coords[0], 0, signer);
    }

    function _initMetric(bytes32 m, OnyxRegistry.Kind k, bytes32 t) internal {
        vm.prank(OWNER);
        reg.registerMetric(m, k, t, 0);
        kindSeen[m] = k;
        templateSeen[m] = t;
    }

    function _breach(string memory why) internal {
        breaches++;
        lastBreach = why;
    }

    function _coord() internal view returns (address) {
        return ledger.coordinator();
    }

    // ------------------------------------------------------------------ owner

    function registerMetric(uint256 mSeed, uint8 kindSeed, bool otherTemplate, uint8 minLevel)
        external
    {
        uint256 i = mSeed % 4;
        bytes32 m = metricIds[i];
        // Recipe metrics 0 and 3 share one recipe, so a floor set through one binds the other.
        OnyxRegistry.Kind k = i == 0 || i == 3
            ? OnyxRegistry.Kind.Recipe
            : (i == 1 ? OnyxRegistry.Kind.Feed : OnyxRegistry.Kind.DatedFeed);
        if (kindSeed % 7 == 0) k = OnyxRegistry.Kind(1 + kindSeed % 3);
        bytes32 t = k == OnyxRegistry.Kind.Recipe
            ? (otherTemplate ? keccak256("another recipe") : RECIPE)
            : keccak256(abi.encode("template", i, otherTemplate));
        vm.prank(OWNER);
        try reg.registerMetric(m, k, t, minLevel % 7) {
            ok["registerMetric"]++;
            if (kindSeen[m] != OnyxRegistry.Kind.None) _breach("a metric was registered twice");
            kindSeen[m] = k;
            templateSeen[m] = t;
            if (k == OnyxRegistry.Kind.Recipe && minLevel % 7 != 0) _floor(t);
        } catch {}
    }

    function _floor(bytes32 recipe) internal {
        uint8 have = reg.recipeMinLevel(recipe);
        uint8 seen = recipeFloorSeen[recipe];
        if (seen != 0 && have != seen) _breach("a recipe floor changed");
        recipeFloorSeen[recipe] = have;
    }

    function registerLabelSet(uint256 lSeed, uint8 level) external {
        bytes32 ls = labelSets[lSeed % 2];
        vm.prank(OWNER);
        try reg.registerLabelSet(ls, level % 7) {
            ok["registerLabelSet"]++;
            if (labelLevelSeen[ls] != 0) _breach("a label set level changed");
            labelLevelSeen[ls] = level % 7;
        } catch {}
    }

    function registerMetricMinLevel(uint256 mSeed, uint8 level) external {
        bytes32 m = metricIds[mSeed % 4];
        vm.prank(OWNER);
        try reg.registerMetricMinLevel(m, level % 7) {
            ok["registerMinLevel"]++;
            _floor(templateSeen[m]);
        } catch {}
    }

    function reopenDraws(uint256 mSeed, uint256 sSeed) external {
        bytes32 m = metricIds[mSeed % 4];
        bytes32 s = subjects[sSeed % 3];
        vm.prank(OWNER);
        try reg.reopenDraws(m, s) {
            ok["reopen"]++;
            reopened[reg.subjectDrawKey(m, s)]++;
        } catch {}
    }

    // ------------------------------------------------------------------ coordinator

    function rotateCoordinator(uint256 cSeed) external {
        ledger.rotate(coords[cSeed % 2]);
        ok["rotate"]++;
    }

    function setReceiptSigner(uint256 kSeed) external {
        address signer = vm.addr(signerKeys[kSeed % 2]);
        address c = _coord();
        vm.prank(c);
        try reg.setReceiptSigner(signer) {
            ok["setSigner"]++;
            (signerBy, signerEpoch, signerSet) = (c, ledger.coordinatorEpoch(), signer);
        } catch {}
    }

    /// Little-endian `n`-byte encoding of `v`.
    function _le(uint256 v, uint256 n) internal pure returns (bytes memory out) {
        out = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = bytes1(uint8(v >> (8 * i)));
        }
    }

    /// Borsh `RuntimeProfile` with fuel limit `fuel` (the bytes a shard's runtimeHash names).
    function _runtime(uint64 fuel) internal pure returns (bytes memory) {
        bytes memory host = bytes("h");
        return abi.encodePacked(
            _le(1, 4),
            _le(host.length, 4),
            host,
            _le(fuel, 8),
            _le(1 << 20, 8),
            _le(1 << 16, 4),
            _le(64, 4)
        );
    }

    /// The coordinator attests a shard's preflight; the chain enforces the template, the
    /// budget, the label-set and recipe floors, and that the shard is not open yet.
    function attestShard(uint64 round, uint256 seed, uint64 used, uint8 level) external {
        // Mostly the registered recipe metric; sometimes another (refused unless registered).
        uint256 mi = seed % 4 == 1 ? 3 : 0;
        OnyxRegistry.ShardPreimage memory p;
        p.recipeId = (seed >> 8) % 5 == 0 ? keccak256("another recipe") : RECIPE;
        bytes memory rt = _runtime(1_000_000);
        p.runtimeHash = keccak256(rt);
        uint256 lsPick = (seed >> 12) % 4;
        p.labelSetHash = lsPick >= 2 ? bytes32(0) : labelSets[lsPick];
        p.paramsHash = keccak256(abi.encode("params", seed % 8));
        round = round % 4;
        bytes32 shard = reg.shardIdOf(p);
        uint64 fuelUsed = used % 700_000;
        uint8 lv = level % 7;
        address c = _coord();
        vm.prank(c);
        try reg.attestShard(round, p, rt, metricIds[mi], fuelUsed, lv) {
            ok["attest"]++;
            bytes32 k = keccak256(abi.encode(round, shard));
            if (attestedLevel[k] != 0) _breach("a shard was attested twice");
            attested.push(k);
            attestedLevel[k] = lv;
            if (uint256(fuelUsed) * 2 > 1_000_000) _breach("attested over the fuel budget");
            uint8 lsl = p.labelSetHash == bytes32(0) ? 0 : reg.labelSetLevel(p.labelSetHash);
            if (p.labelSetHash != bytes32(0) && (lsl == 0 || lv < lsl)) {
                _breach("attested better than the label set allows");
            }
            uint8 rf = reg.recipeMinLevel(p.recipeId);
            if (rf != 0 && lv < rf) _breach("attested better than the recipe floor");
            if (lv < 1 || lv > 5) _breach("attested an out-of-range level");
            // The level a consumer reads is never better than the floor applied now.
            ledger.setStatus(round, shard, OnyxLedger.ShardStatus.Finalized);
            uint8 got = reg.levelOf(round, shard);
            if (got < lv || got < reg.recipeMinLevel(p.recipeId)) _breach("levelOf below a floor");
            ledger.setStatus(round, shard, OnyxLedger.ShardStatus.None);
        } catch {}
    }

    function openShard(uint64 round, uint256 seed) external {
        OnyxRegistry.ShardPreimage memory p;
        p.recipeId = RECIPE;
        p.runtimeHash = keccak256(_runtime(1_000_000));
        p.labelSetHash = labelSets[(seed >> 12) % 2];
        p.paramsHash = keccak256(abi.encode("params", seed % 8));
        ledger.setStatus(round % 4, reg.shardIdOf(p), OnyxLedger.ShardStatus.Open);
    }

    // ------------------------------------------------------------------ level-2 draws

    function _fact(uint256 mSeed, uint256 sSeed, uint256 wSeed)
        internal
        view
        returns (bytes32 m, bytes32 s, uint64 w)
    {
        m = metricIds[1 + mSeed % 2];
        s = subjects[sSeed % 3];
        w = windows[wSeed % 3];
    }

    function _commitOf(uint256 x) internal pure returns (bytes32 pool, bytes32 spec, bytes32 cfg) {
        pool = keccak256(abi.encode("pool", x % 2));
        spec = keccak256(abi.encode("spec", (x >> 4) % 2));
        cfg = keccak256(abi.encode("cfg", (x >> 8) % 2));
        return (pool, spec, cfg);
    }

    function announceDraw(uint256 mSeed, uint256 sSeed, uint256 wSeed, uint256 x) external {
        (bytes32 m, bytes32 s, uint64 w) = _fact(mSeed, sSeed, wSeed);
        bytes32 fact = reg.factKey(m, s, w);
        uint32 n = fact == bytes32(0) ? 0 : reg.drawCount(fact);
        uint32 attempt = (x >> 12) % 5 == 0 ? n + 1 : n;
        uint64 h = uint64(block.number) + uint64((x >> 16) % 4); // 0: not in the future
        (bytes32 pool, bytes32 spec, bytes32 cfg) = _commitOf(x);
        bytes32 commit = reg.drawCommit(pool, spec, cfg);
        bytes32 sk = reg.subjectDrawKey(m, s);
        uint32 strikes = reg.subjectStrikes(sk);
        address c = _coord();
        vm.prank(c);
        try reg.announceDraw(m, s, w, attempt, commit, h) returns (bytes32 f) {
            ok["announce"]++;
            if (attempt != n) _breach("a draw skipped an attempt");
            if (h <= block.number) _breach("a draw announced a beacon not in the future");
            if (strikes >= 3 * (reopened[sk] + 1)) _breach("a draw beyond the subject bound");
            if (signerBy != c || signerEpoch != ledger.coordinatorEpoch()) {
                _breach("a draw without a receipt signer of this coordinator term");
            }
            if (reg.drawReceiptSigner(f, attempt) != signerSet) _breach("draw signer snapshot");
            (bytes32 rkx,) = reg.drawRevealKey(f, attempt);
            bool termKey = revealBy == c && revealEpoch == ledger.coordinatorEpoch();
            if (rkx != (termKey ? revealSetX : bytes32(0))) _breach("draw reveal key snapshot");
            // Amendment 14: once required, every draw is keyed and time-locked; before, none.
            if (reg.drawTimeLocked(f, attempt) != timeLockOn) _breach("draw time-lock snapshot");
            if (timeLockOn && rkx == bytes32(0)) _breach("a time-locked draw without a key");
            drawTimeLockSeen[f][attempt] = timeLockOn;
            if (!factKnown[f]) {
                factKnown[f] = true;
                facts.push(f);
            }
            drawCommitSeen[f][attempt] = commit;
            drawX[f] = x;
            drawHeightSeen[f][attempt] = h;
            announced[sk]++;
        } catch {}
    }

    function fixBeacon(uint256 fSeed, uint32 aSeed, bool chainHasIt) external {
        if (facts.length == 0) return;
        bytes32 f = facts[fSeed % facts.length];
        uint32 n = reg.drawCount(f);
        uint32 attempt = n == 0 ? 0 : aSeed % n;
        uint64 h = drawHeightSeen[f][attempt];
        // The chain has a hash for the beacon block only while it is among the last 256.
        if (chainHasIt && block.number > h && block.number - h <= 256) {
            vm.setBlockhash(h, keccak256(abi.encode("block", h)));
        }
        try reg.fixBeacon(f, attempt) returns (bytes32 b) {
            ok["fixBeacon"]++;
            if (block.number <= h) _breach("a beacon fixed before its block");
            if (b != blockhash(h) || b == bytes32(0)) {
                _breach("a beacon that is not the block hash");
            }
            beaconSeen[f][attempt] = b;
        } catch {}
    }

    function recordObservation(uint256 mSeed, uint256 sSeed, uint256 wSeed, uint256 x) external {
        (bytes32 m, bytes32 s, uint64 w) = _fact(mSeed, sSeed, wSeed);
        bytes32 fact = reg.factKey(m, s, w);
        // Mostly the inputs the latest draw committed to; otherwise any (refused on mismatch).
        (bytes32 pool, bytes32 spec, bytes32 cfg) = _commitOf(x % 10 < 7 ? drawX[fact] : x);
        bytes32 result = keccak256(abi.encode("result", x % 3));
        address c = _coord();
        vm.prank(c);
        try reg.recordObservation(m, s, w, spec, result, pool, cfg) returns (bytes32 f) {
            ok["record"]++;
            uint32 n = reg.drawCount(f);
            if (f != fact) _breach("fact key not the registered template's");
            if (n == 0) {
                _breach("recorded without a draw");
            } else if (beaconSeen[f][n - 1] == bytes32(0)) {
                _breach("recorded before the beacon");
            } else if (drawCommitSeen[f][n - 1] != reg.drawCommit(pool, spec, cfg)) {
                _breach("recorded a spec the latest draw did not commit to");
            }
            if (resultSeen[f] != bytes32(0)) _breach("a fact recorded twice");
            if (timeLockOn && n > 0 && !drawTimeLockSeen[f][n - 1]) {
                _breach("a draw without a time-lock recorded once time-locks were required");
            }
            resultSeen[f] = result;
            specSeen[f] = spec;
            recorded[reg.subjectDrawKey(m, s)]++;
        } catch {}
    }

    // ------------------------------------------------------------------ transcript log

    function _sign(uint256 key, bytes memory message) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, OnyxSig.eip191Digest(message));
        return abi.encodePacked(r, s, v);
    }

    function postCommit(uint256 fSeed, uint256 oSeed, uint256 rSeed, bool forged) external {
        if (facts.length == 0) return;
        bytes32 f = facts[fSeed % facts.length];
        uint32 n = reg.drawCount(f);
        uint32 attempt = n == 0 ? 0 : uint32(rSeed % n);
        address o = vm.addr(observerKeys[oSeed % 3]);
        bytes32 round = keccak256(abi.encode("round", rSeed % 2));
        bytes32 commit = keccak256(abi.encode("commit", rSeed));
        uint256 key = forged ? observerKeys[(oSeed % 3 + 1) % 3] : observerKeys[oSeed % 3];
        bytes memory sig = _sign(key, reg.commitMessage(round, attempt, o, commit));
        try reg.postCommit(f, attempt, o, round, commit, sig) {
            ok["postCommit"]++;
            if (forged) _breach("a commit posted with another key's signature");
            bytes32 k = reg.logKey(f, attempt, o, round);
            if (logSeen[k] != bytes32(0)) _breach("a commit slot was written twice");
            logSeen[k] = commit;
            logSlots.push(k);
            if (beaconSeen[f][attempt] == bytes32(0)) _breach("a commit posted before the beacon");
        } catch {}
    }

    function postReceipt(uint256 fSeed, uint256 oSeed, uint256 rSeed, uint256 kSeed) external {
        if (facts.length == 0) return;
        bytes32 f = facts[fSeed % facts.length];
        uint32 n = reg.drawCount(f);
        uint32 attempt = n == 0 ? 0 : uint32(rSeed % n);
        address o = vm.addr(observerKeys[oSeed % 3]);
        bytes32 round = keccak256(abi.encode("round", rSeed % 2));
        bytes32 commit = keccak256(abi.encode("commit", rSeed));
        uint256 key = signerKeys[kSeed % 2];
        bytes memory sig = _sign(key, reg.receiptMessage(round, attempt, o, commit));
        try reg.postReceipt(f, attempt, o, round, commit, sig) {
            ok["postReceipt"]++;
            if (vm.addr(key) != reg.drawReceiptSigner(f, attempt)) {
                _breach("a receipt signed by a key the draw did not name");
            }
            bytes32 k = reg.logKey(f, attempt, o, round);
            if (receiptSeen[k] != bytes32(0)) _breach("a receipt slot was written twice");
            receiptSeen[k] = commit;
            logSlots.push(k);
        } catch {}
    }

    // ------------------------------------------------------------------ sealed reveals

    function setRevealKey(uint256 kSeed, bool offCurve) external {
        Vm.Wallet memory w = vm.createWallet(revealKeys[kSeed % 2]);
        bytes32 x = bytes32(w.publicKeyX);
        bytes32 y = offCurve ? bytes32(w.publicKeyY ^ 1) : bytes32(w.publicKeyY);
        address c = _coord();
        vm.prank(c);
        try reg.setRevealKey(x, y) {
            ok["setRevealKey"]++;
            if (offCurve) _breach("a reveal key off the curve");
            (revealBy, revealEpoch, revealSetX, revealSetY) = (c, ledger.coordinatorEpoch(), x, y);
        } catch {}
    }

    /// An observer posts a sealed reveal: its own, one signed by another key, or one carrying
    /// another observer's ephemeral key proof (C4-3).
    function postSealed(uint256 fSeed, uint256 oSeed, uint256 rSeed, uint8 mode) external {
        if (facts.length == 0) return;
        bytes32 f = facts[fSeed % facts.length];
        uint32 n = reg.drawCount(f);
        uint32 attempt = n == 0 ? 0 : uint32(rSeed % n);
        uint256 oi = oSeed % 3;
        address o = vm.addr(observerKeys[oi]);
        bytes32 round = keccak256(abi.encode("round", rSeed % 2));
        bytes memory ct = abi.encodePacked(keccak256(abi.encode("ct", rSeed)));
        // Sometimes a time-lock's length plus a body (amendment 14).
        if ((rSeed >> 8) % 2 == 1) ct = bytes.concat(new bytes(128), ct);
        uint256 eph = 0xE0 + oi;
        Vm.Wallet memory w = vm.createWallet(eph);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        uint256 m = mode % 4;
        uint256 signer = m == 1 ? observerKeys[(oi + 1) % 3] : observerKeys[oi];
        // mode 2: the key proof names another observer (a copied ephemeral key).
        address named = m == 2 ? vm.addr(observerKeys[(oi + 1) % 3]) : o;
        bytes memory keySig = _keyProof(eph, w, named, round, attempt);
        bytes memory sig =
            _sign(signer, reg.sealedRevealMessage(round, attempt, o, ex, ey, keccak256(ct)));
        try reg.postSealedReveal(f, attempt, o, round, ex, ey, ct, keySig, sig) {
            ok["postSealed"]++;
            if (m == 1 || m == 2) _breach("a sealed post with a wrong signature");
            (bytes32 kx,) = reg.drawRevealKey(f, attempt);
            if (kx == bytes32(0)) _breach("a sealed post for a draw without a reveal key");
            if (beaconSeen[f][attempt] == bytes32(0)) _breach("a sealed post before the beacon");
            if (ct.length <= 128 && reg.drawTimeLocked(f, attempt)) {
                _breach("a time-locked draw took a post without a time-lock");
            }
            bytes32 k = reg.logKey(f, attempt, o, round);
            if (sealedSeen[k] != bytes32(0)) _breach("a sealed slot was written twice");
            sealedSeen[k] = keccak256(ct);
            sealedSlots.push(k);
        } catch {}
    }

    uint256 internal constant N =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    /// Schnorr proof of knowledge of `eph` for (round, attempt, o) (review C4-R1-1).
    function _keyProof(uint256 eph, Vm.Wallet memory w, address o, bytes32 round, uint32 attempt)
        internal
        returns (bytes memory)
    {
        uint256 t = uint256(keccak256(abi.encode("nonce", eph, o, round, attempt))) % (N - 1) + 1;
        address ta = vm.addr(t);
        uint256 c = reg.keyProofChallenge(
            bytes32(w.publicKeyX), bytes32(w.publicKeyY), ta, round, attempt, o
        );
        return abi.encodePacked(ta, addmod(t, mulmod(c, eph, N), N));
    }

    function warp(uint16 secs) external {
        vm.warp(block.timestamp + 1 + secs % 1_500);
    }

    /// The coordinator records an attempt with random statuses: a success must be complete at
    /// that moment and is written once.
    function recordAttempt(uint256 fSeed, uint256 rSeed, uint8 statuses) external {
        if (facts.length == 0) return;
        bytes32 f = facts[fSeed % facts.length];
        uint32 n = reg.drawCount(f);
        uint32 attempt = n == 0 ? 0 : uint32(rSeed % n);
        bytes32 round = keccak256(abi.encode("round", rSeed % 2));
        bytes memory e;
        address[3] memory os =
            [vm.addr(observerKeys[0]), vm.addr(observerKeys[1]), vm.addr(observerKeys[2])];
        for (uint256 i = 0; i < 3; i++) {
            for (uint256 j = 0; j + 1 < 3 - i; j++) {
                if (uint160(os[j]) > uint160(os[j + 1])) (os[j], os[j + 1]) = (os[j + 1], os[j]);
            }
        }
        for (uint256 i = 0; i < 3; i++) {
            address o = os[i];
            bytes32 c = logSeen[reg.logKey(f, attempt, o, round)];
            e = bytes.concat(e, abi.encodePacked(o, c, uint8((statuses >> (2 * i)) % 3)));
        }
        bytes32 result = keccak256(abi.encode("attempt result", rSeed % 3));
        address c = _coord();
        vm.prank(c);
        try reg.recordAttempt(f, attempt, round, L_OPEN, L_CLOSE, L_RCLOSE, result, e) {
            ok["recordAttempt"]++;
            if (block.timestamp <= L_RCLOSE) _breach("an attempt recorded before reveal close");
            if (!reg.revealLogComplete(f, attempt, round, L_OPEN, L_CLOSE, L_RCLOSE, e)) {
                _breach("an attempt recorded with an incomplete transcript");
            }
            if (attemptSeen[f][attempt] != bytes32(0)) _breach("an attempt recorded twice");
            if (timeLockOn && !drawTimeLockSeen[f][attempt]) {
                _breach("an attempt without a time-lock recorded once time-locks were required");
            }
            bytes32 d = reg.attemptDigest(round, L_OPEN, L_CLOSE, L_RCLOSE, result, e);
            attemptSeen[f][attempt] = d;
            bytes32 rk = keccak256(abi.encode(f, attempt));
            recordKeys.push(rk);
            recordFact[rk] = f;
            recordAttemptOf[rk] = attempt;
        } catch {}
    }

    /// Amendment 14: the owner, the coordinator or a stranger asks for time-locks (rarely
    /// succeeding, so most runs explore draws before the mode as well as after).
    function requireTimeLock(uint8 who, address stranger) external {
        uint256 w = who % 16;
        if (w > 2) return;
        address a = w == 0 ? OWNER : w == 1 ? _coord() : stranger;
        if (w == 2 && (a == OWNER || a == _coord())) a = address(0xBAD);
        vm.prank(a);
        try reg.requireTimeLock() {
            ok["requireTimeLock"]++;
            if (w == 2) _breach("a stranger required time-locks");
            timeLockOn = true;
        } catch {}
    }

    function sealedSlotsLength() external view returns (uint256) {
        return sealedSlots.length;
    }

    function recordKeysLength() external view returns (uint256) {
        return recordKeys.length;
    }

    function expectedRevealKeyX() external view returns (bytes32) {
        return
            revealBy == _coord() && revealEpoch == ledger.coordinatorEpoch()
                ? revealSetX
                : bytes32(0);
    }

    function roll(uint16 blocks) external {
        vm.roll(block.number + 1 + blocks % 300);
    }

    /// Non-role accounts (and the coordinator for owner powers, the owner for coordinator
    /// powers): every privileged call must revert.
    function attack(uint8 which, address a) external {
        // Cheatcode, console and precompile addresses cannot be pranked meaningfully.
        if (uint160(a) < 0x100 || a == address(vm) || a == CONSOLE) a = address(0xBAD);
        bool success;
        uint256 w = which % 10;
        address c = _coord();
        if (w < 4) {
            if (a == OWNER) a = c; // the coordinator never holds owner powers
            vm.prank(a);
            if (w == 0) {
                try reg.registerMetric(keccak256("x"), OnyxRegistry.Kind.Feed, keccak256("t"), 0) {
                    success = true;
                } catch {}
            } else if (w == 1) {
                try reg.registerLabelSet(keccak256("x"), 1) {
                    success = true;
                } catch {}
            } else if (w == 2) {
                try reg.registerMetricMinLevel(metricIds[0], 1) {
                    success = true;
                } catch {}
            } else {
                try reg.reopenDraws(metricIds[1], subjects[0]) {
                    success = true;
                } catch {}
            }
        } else {
            if (a == c) a = OWNER; // the owner never holds coordinator powers
            OnyxRegistry.ShardPreimage memory p;
            bytes memory rt = _runtime(1_000_000);
            vm.prank(a);
            if (w == 4) {
                try reg.setReceiptSigner(a) {
                    success = true;
                } catch {}
            } else if (w == 5) {
                try reg.announceDraw(metricIds[1], subjects[0], 100, 0, keccak256("c"), 1e9) {
                    success = true;
                } catch {}
            } else if (w == 8) {
                Vm.Wallet memory k = vm.createWallet(0x4241);
                try reg.setRevealKey(bytes32(k.publicKeyX), bytes32(k.publicKeyY)) {
                    success = true;
                } catch {}
            } else if (w == 9) {
                try reg.recordAttempt(
                    facts.length == 0 ? bytes32(0) : facts[0],
                    0,
                    keccak256("r"),
                    1,
                    2,
                    3,
                    keccak256("x"),
                    ""
                ) {
                    success = true;
                } catch {}
            } else if (w == 6) {
                try reg.recordObservation(
                    metricIds[1], subjects[0], 100, keccak256("s"), keccak256("r"), 0, 0
                ) {
                    success = true;
                } catch {}
            } else {
                try reg.attestShard(0, p, rt, metricIds[0], 0, 1) {
                    success = true;
                } catch {}
            }
        }
        if (success) _breach("a privileged registry call succeeded from the wrong account");
    }

    function factsLength() external view returns (uint256) {
        return facts.length;
    }

    function logSlotsLength() external view returns (uint256) {
        return logSlots.length;
    }
}

/// OnyxRegistry invariants under random sequences (L2 contract audit).
contract RegistryInvariantTest is Test {
    RegistryHandler internal h;

    function setUp() public {
        h = new RegistryHandler();
        targetContract(address(h));
        bytes4[] memory s = new bytes4[](21);
        s[0] = h.registerMetric.selector;
        s[1] = h.registerLabelSet.selector;
        s[2] = h.registerMetricMinLevel.selector;
        s[3] = h.reopenDraws.selector;
        s[4] = h.rotateCoordinator.selector;
        s[5] = h.setReceiptSigner.selector;
        s[6] = h.attestShard.selector;
        s[7] = h.openShard.selector;
        s[8] = h.announceDraw.selector;
        s[9] = h.fixBeacon.selector;
        s[10] = h.recordObservation.selector;
        s[11] = h.postCommit.selector;
        s[12] = h.postReceipt.selector;
        s[13] = h.roll.selector;
        s[14] = h.attack.selector;
        s[15] = h.announceDraw.selector;
        s[16] = h.setRevealKey.selector;
        s[17] = h.postSealed.selector;
        s[18] = h.recordAttempt.selector;
        s[19] = h.warp.selector;
        s[20] = h.requireTimeLock.selector;
        targetSelector(StdInvariant.FuzzSelector({addr: address(h), selectors: s}));
    }

    /// Draw rules: per (metric, subject), draws not ended with a value never exceed
    /// `MAX_DRAWS` per owner reopen (+1); the count is announced minus recorded.
    function invariant_SubjectDrawBound() public view {
        OnyxRegistry reg = h.reg();
        for (uint256 mi = 1; mi < 3; mi++) {
            for (uint256 si = 0; si < 3; si++) {
                bytes32 sk = reg.subjectDrawKey(h.metricIds(mi), h.subjects(si));
                uint32 strikes = reg.subjectStrikes(sk);
                assertEq(strikes, h.announced(sk) - h.recorded(sk), "strikes");
                assertLe(strikes, 3 * (reg.subjectReopens(sk) + 1), "subject bound");
                assertEq(reg.subjectReopens(sk), h.reopened(sk), "reopens");
            }
        }
    }

    /// Draws are sequential, at most 64 per fact, never change, and a fixed beacon never
    /// changes; a recorded fact keeps its first result and spec.
    function invariant_DrawsAndFactsAreImmutable() public view {
        OnyxRegistry reg = h.reg();
        for (uint256 i = 0; i < h.factsLength(); i++) {
            bytes32 f = h.facts(i);
            uint32 n = reg.drawCount(f);
            assertLe(n, reg.MAX_DRAW_ATTEMPTS(), "draw count");
            for (uint32 a = 0; a < n; a++) {
                OnyxRegistry.Draw memory d = reg.drawOf(f, a);
                assertEq(d.commit, h.drawCommitSeen(f, a), "draw commit changed");
                assertEq(d.beaconHeight, h.drawHeightSeen(f, a), "beacon height changed");
                assertGt(d.beaconHeight, d.announcedAt, "beacon not after announcement");
                if (h.beaconSeen(f, a) != bytes32(0)) {
                    assertEq(d.beacon, h.beaconSeen(f, a), "beacon");
                }
            }
            assertEq(reg.drawOf(f, n).commit, bytes32(0), "a draw past the count");
        }
    }

    /// Owner registrations are append-only: a metric's kind and template, a label set's level
    /// and a recipe's floor never change once set.
    function invariant_RegistrationsAreFinal() public view {
        OnyxRegistry reg = h.reg();
        for (uint256 i = 0; i < 4; i++) {
            bytes32 m = h.metricIds(i);
            (OnyxRegistry.Kind k, bytes32 t,) = reg.metrics(m);
            assertEq(uint8(k), uint8(h.kindSeen(m)), "metric kind");
            assertEq(t, h.templateSeen(m), "metric template");
            if (h.recipeFloorSeen(t) != 0) {
                assertEq(reg.recipeMinLevel(t), h.recipeFloorSeen(t), "floor");
            }
        }
        for (uint256 i = 0; i < 2; i++) {
            bytes32 ls = h.labelSets(i);
            assertEq(reg.labelSetLevel(ls), h.labelLevelSeen(ls), "label level");
        }
    }

    /// Roles: the owner is fixed and never the coordinator it was built against; the receipt
    /// signer reads zero unless the current coordinator set it in its current term; no
    /// privileged call from the wrong account ever succeeded.
    function invariant_Roles() public view {
        OnyxRegistry reg = h.reg();
        assertEq(reg.owner(), h.OWNER(), "owner");
        address c = h.ledger().coordinator();
        address want = h.signerBy() == c && h.signerEpoch() == h.ledger().coordinatorEpoch()
            ? h.signerSet()
            : address(0);
        assertEq(reg.receiptSigner(), want, "receipt signer");
        assertEq(h.breaches(), 0, h.lastBreach());
    }

    /// Amendment 10: a sealed slot keeps its first post, an attempt record its first digest,
    /// and the reveal key reads zero unless the current coordinator set it in its term.
    function invariant_SealedPostsAndRecordsAreWriteOnce() public view {
        OnyxRegistry reg = h.reg();
        assertEq(reg.MAX_SEALED_BYTES(), 1438);
        (bytes32 kx,) = reg.revealKey();
        assertEq(kx, h.expectedRevealKeyX(), "reveal key");
        for (uint256 i = 0; i < h.recordKeysLength(); i++) {
            bytes32 rk = h.recordKeys(i);
            bytes32 f = h.recordFact(rk);
            uint32 a = h.recordAttemptOf(rk);
            assertEq(reg.attemptRecord(f, a), h.attemptSeen(f, a), "attempt record changed");
        }
    }

    /// Amendment 14 (ROADMAP C51): time-locks, once required, stay required; every draw's
    /// `drawTimeLocked` is what it was announced with and never changes.
    function invariant_TimeLocksAreOneWayAndSnapshotted() public view {
        OnyxRegistry reg = h.reg();
        assertEq(reg.timeLockRequired(), h.timeLockOn(), "time-lock mode");
        for (uint256 i = 0; i < h.factsLength(); i++) {
            bytes32 f = h.facts(i);
            uint32 n = reg.drawCount(f);
            for (uint32 a = 0; a < n; a++) {
                assertEq(reg.drawTimeLocked(f, a), h.drawTimeLockSeen(f, a), "draw time-lock");
            }
        }
    }

    function afterInvariant() public {
        _stats(
            string.concat(
                "STATS registry facts=",
                vm.toString(h.factsLength()),
                " announced=",
                vm.toString(h.ok("announce")),
                " beacons=",
                vm.toString(h.ok("fixBeacon")),
                " recorded=",
                vm.toString(h.ok("record")),
                " attested=",
                vm.toString(h.ok("attest")),
                " logPosts=",
                vm.toString(h.logSlotsLength()),
                " sealedPosts=",
                vm.toString(h.sealedSlotsLength()),
                " attemptRecords=",
                vm.toString(h.recordKeysLength()),
                " timeLock=",
                vm.toString(h.ok("requireTimeLock"))
            )
        );
    }

    /// One line per run: printed, and appended to cache/invariant-stats/ when
    /// ONYX_INVARIANT_STATS=1 (the audit run sums them; docs/audit/2026-10-13-contracts.md).
    function _stats(string memory a) internal {
        console.log(a);
        if (vm.envOr("ONYX_INVARIANT_STATS", false)) {
            vm.writeLine("cache/invariant-stats/registry.log", a);
        }
    }
}
