// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxSignedBook} from "../../src/OnyxSignedBook.sol";
import {SignedBookFixture} from "../OnyxSignedBook.t.sol";

/// Handler for the OnyxSignedBook invariants (spec R-SIG-6c on chain): three keys sign two values
/// for three facts under fresh or reused sequences; the reviewer retires, compromises, reinstates
/// and resolves; time moves. Every step checks the rules it can break and counts violations.
contract SignedBookHandler is SignedBookFixture {
    OnyxSignedBook public book;
    address public constant REVIEWER = address(0xBEEF);
    uint256[3] internal pks = [uint256(0xA11CE), 0xB0B, 0xCA401];
    bytes32[3] public ids;
    uint64[3] internal nextSeq;
    uint64[3] internal lastSeq;
    uint8[3] internal lastLife;
    bytes32[3] public facts;
    uint64[3] internal factTimes;

    /// (key index, sequence) -> the first digest the book recorded for it.
    mapping(uint256 => mapping(uint64 => bytes32)) internal firstDigest;
    uint64[] internal usedSeq;
    uint8[] internal usedKey;

    uint256 public violations;
    uint256 public posts;
    uint256 public accepted;
    uint256 public equivocations;

    constructor() {
        vm.warp(T0);
        book = new OnyxSignedBook(REVIEWER);
        vm.startPrank(REVIEWER);
        for (uint256 k = 0; k < 3; k++) {
            ids[k] = book.registerKey(SOURCE, vm.addr(pks[k]), ORIGIN, PROV, "setup");
            lastLife[k] = 1;
            nextSeq[k] = 1;
        }
        vm.stopPrank();
        for (uint256 f = 0; f < 3; f++) {
            factTimes[f] = T0 - uint64(f) * 1000;
            facts[f] = _fact(factTimes[f]);
        }
    }

    struct Snap {
        bool[3] susp;
        OnyxSignedBook.FactState[3] st;
    }

    function _snap() internal view returns (Snap memory s) {
        for (uint256 k = 0; k < 3; k++) {
            (, s.susp[k],,,,) = book.keyState(ids[k]);
        }
        for (uint256 f = 0; f < 3; f++) {
            (s.st[f],,,) = book.factView(facts[f]);
        }
    }

    /// After any action but `reinstate` of key k, a suspended key stays suspended; after any
    /// action but `resolve` of fact f, a disputed fact stays disputed (R-SIG-6c invariant 2).
    function _check(Snap memory before, uint256 reinstated, uint256 resolved) internal {
        Snap memory a = _snap();
        for (uint256 k = 0; k < 3; k++) {
            if (before.susp[k] && !a.susp[k] && k != reinstated) violations++;
            (OnyxSignedBook.Life life,,,,,) = book.keyState(ids[k]);
            // The life cycle only moves forward (invariant 3).
            if (uint8(life) < lastLife[k]) violations++;
            lastLife[k] = uint8(life);
        }
        for (uint256 f = 0; f < 3; f++) {
            if (
                before.st[f] == OnyxSignedBook.FactState.Disputed
                    && a.st[f] != OnyxSignedBook.FactState.Disputed && f != resolved
            ) violations++;
        }
        // A used sequence keeps its first payload for good (R-SIG-5, never pruned).
        for (uint256 i = 0; i < usedSeq.length; i++) {
            uint256 k = usedKey[i];
            if (book.sequenceOf(ids[k], usedSeq[i]) != firstDigest[k][usedSeq[i]]) violations++;
        }
    }

    /// Would R-SIG-3 let key k sign a report observed at `obs` now?
    function _permitted(uint256 k, uint64 obs) internal view returns (bool) {
        (OnyxSignedBook.Life life, bool susp,,, uint64 at,) = book.keyState(ids[k]);
        if (susp) return false;
        if (life == OnyxSignedBook.Life.Active) return true;
        if (life != OnyxSignedBook.Life.Retired) return false;
        uint256 lo = at > 3600 ? at - 3600 : 0;
        return obs < at && obs >= lo && block.timestamp < uint256(at) + 3600;
    }

    function post(uint256 k, uint256 f, bool two, bool reuse) external {
        k = k % 3;
        f = f % 3;
        uint64 seq;
        if (reuse && lastSeq[k] != 0) {
            seq = lastSeq[k];
        } else {
            seq = nextSeq[k]++;
            lastSeq[k] = seq;
        }
        bytes memory p = _payload(SOURCE, ids[k], _entity(SUBJECT), two ? 2 : 1, factTimes[f], seq);
        bool permitted = _permitted(k, factTimes[f]);
        Snap memory s = _snap();
        posts++;
        try book.postReport(p, _sign(pks[k], p)) returns (OnyxSignedBook.Outcome o, bytes32) {
            // Invariant 1: nothing is recorded unless the key may sign it now.
            if (!permitted) violations++;
            if (o == OnyxSignedBook.Outcome.Accepted) accepted++;
            if (o == OnyxSignedBook.Outcome.Equivocation) equivocations++;
            if (firstDigest[k][seq] == bytes32(0)) {
                firstDigest[k][seq] = _digest(p);
                usedSeq.push(seq);
                usedKey.push(uint8(k));
            }
        } catch {}
        _check(s, type(uint256).max, type(uint256).max);
    }

    function retire(uint256 k, uint16 back) external {
        k = k % 3;
        Snap memory s = _snap();
        vm.prank(REVIEWER);
        try book.retireKey(ids[k], uint64(block.timestamp) - back, "inv") {} catch {}
        _check(s, type(uint256).max, type(uint256).max);
    }

    function compromise(uint256 k, uint32 back) external {
        k = k % 3;
        Snap memory s = _snap();
        uint64 since = uint64(block.timestamp) > back ? uint64(block.timestamp) - back : 0;
        vm.prank(REVIEWER);
        try book.markCompromised(ids[k], since, "inv") {} catch {}
        _check(s, type(uint256).max, type(uint256).max);
    }

    function reinstate(uint256 k) external {
        k = k % 3;
        Snap memory s = _snap();
        vm.prank(REVIEWER);
        try book.reinstateKey(ids[k], "inv") {} catch {}
        _check(s, k, type(uint256).max);
    }

    function resolve(uint256 f, uint256 pick) external {
        f = f % 3;
        Snap memory s = _snap();
        (,,, bytes32[] memory list) = book.factView(facts[f]);
        bytes32 keep;
        if (list.length > 0 && pick % 3 != 0) {
            // factView lists report hashes; the book resolves by payload digest: find it.
            bytes32 want = list[pick % list.length];
            for (uint256 i = 0; i < usedSeq.length; i++) {
                bytes32 d = firstDigest[usedKey[i]][usedSeq[i]];
                if (book.report(d).reportHash == want) keep = d;
            }
        }
        vm.prank(REVIEWER);
        try book.resolveFact(facts[f], keep, "inv") {} catch {}
        _check(s, type(uint256).max, f);
    }

    function advance(uint16 dt) external {
        Snap memory s = _snap();
        vm.warp(block.timestamp + dt);
        _check(s, type(uint256).max, type(uint256).max);
    }
}

contract SignedBookInvariantsTest is StdInvariant, Test {
    SignedBookHandler internal h;

    function setUp() public {
        h = new SignedBookHandler();
        targetContract(address(h));
    }

    /// R-SIG-6c (1)-(3) and R-SIG-5 on chain: no unpermitted record, no suspension or dispute
    /// cleared without the reviewer, no life cycle moving back, no sequence rewritten.
    function invariant_TheBookKeepsItsRules() public view {
        assertEq(h.violations(), 0);
    }

    /// A settleable fact is accepted and has stood for the dispute window of block time.
    function invariant_SettleableMeansAcceptedForTheWindow() public view {
        OnyxSignedBook book = h.book();
        for (uint256 f = 0; f < 3; f++) {
            (bool ok,,) = book.settleable(h.facts(f));
            (OnyxSignedBook.FactState st,, uint64 at,) = book.factView(h.facts(f));
            if (ok) {
                assertEq(uint256(st), uint256(OnyxSignedBook.FactState.Accepted));
                assertGe(block.timestamp, uint256(at) + book.DISPUTE_WINDOW_SECS());
            }
        }
    }
}
