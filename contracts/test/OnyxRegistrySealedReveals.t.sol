// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Vm} from "forge-std/Test.sol";
import {OnyxRegistryTest} from "./OnyxRegistry.t.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// ADR-0027 amendment 10 (ROADMAP C4, REVLOG-1, REVLOG-5): observers post their reveal
/// encrypted to the draw's reveal key; `revealLogComplete` refuses a round that leaves a post
/// out; `recordAttempt` pins a round's entries and result before it is served.
contract OnyxRegistrySealedRevealsTest is OnyxRegistryTest {
    bytes32 internal constant SPEC = keccak256("spec");
    bytes32 internal constant ROUND = keccak256("round");
    uint256 internal constant RECEIPT_PK = 0x5EC5EC;
    uint256 internal constant REVEAL_PK = 0x4242;
    uint64 internal constant OPEN = 1_000;
    uint64 internal constant CLOSE = 2_000;
    uint64 internal constant RCLOSE = 3_000;

    function _signText(uint256 pk, bytes memory text) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, OnyxSig.eip191Digest(text));
        return abi.encodePacked(r, s, v);
    }

    function _setRevealKey(uint256 pk) internal {
        Vm.Wallet memory w = vm.createWallet(pk);
        reg.setRevealKey(bytes32(w.publicKeyX), bytes32(w.publicKeyY));
    }

    /// Registry with a receipt key and a reveal key, draw 0 of the feed fact announced, its
    /// beacon fixed, the clock inside the commit phase.
    function _setup() internal returns (bytes32 fact) {
        _reg();
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        _setRevealKey(REVEAL_PK);
        fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        vm.warp(1_500);
    }

    function _commit(bytes32 fact, uint256 pk, bytes32 commit) internal returns (address o) {
        o = vm.addr(pk);
        reg.postCommit(
            fact, 0, o, ROUND, commit, _signText(pk, reg.commitMessage(ROUND, 0, o, commit))
        );
    }

    /// Observer `pk` posts `ct` with the ephemeral key `ephPk`.
    function _seal(bytes32 fact, uint256 pk, uint256 ephPk, bytes memory ct)
        internal
        returns (address o)
    {
        o = vm.addr(pk);
        Vm.Wallet memory w = vm.createWallet(ephPk);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        reg.postSealedReveal(
            fact,
            0,
            o,
            ROUND,
            ex,
            ey,
            ct,
            _keyProof(ephPk, o, ROUND, 0),
            _signText(pk, reg.sealedRevealMessage(ROUND, 0, o, ex, ey, keccak256(ct)))
        );
    }

    function _entry(address o, bytes32 c, uint8 status) internal pure returns (bytes memory) {
        return abi.encodePacked(o, c, status);
    }

    /// Three entries, sorted ascending by observer (the registry requires it, C4-R1-2).
    function _e3(
        address a1,
        bytes32 c1,
        uint8 s1,
        address a2,
        bytes32 c2,
        uint8 s2,
        address a3,
        bytes32 c3,
        uint8 s3
    ) internal pure returns (bytes memory) {
        address[3] memory a = [a1, a2, a3];
        bytes32[3] memory c = [c1, c2, c3];
        uint8[3] memory st = [s1, s2, s3];
        for (uint256 i = 0; i < 3; i++) {
            for (uint256 j = 0; j + 1 < 3 - i; j++) {
                if (uint160(a[j]) > uint160(a[j + 1])) {
                    (a[j], a[j + 1]) = (a[j + 1], a[j]);
                    (c[j], c[j + 1]) = (c[j + 1], c[j]);
                    (st[j], st[j + 1]) = (st[j + 1], st[j]);
                }
            }
        }
        return bytes.concat(
            _entry(a[0], c[0], st[0]), _entry(a[1], c[1], st[1]), _entry(a[2], c[2], st[2])
        );
    }

    uint256 internal constant N =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    /// A Schnorr proof that the holder of `ephPk` knows it, for (round, attempt, o): `addr(T) ‖
    /// z` with `T = t·G`, `z = t + c·r` (review C4-R1-1).
    function _keyProof(uint256 ephPk, address o, bytes32 round, uint32 attempt)
        internal
        returns (bytes memory)
    {
        Vm.Wallet memory w = vm.createWallet(ephPk);
        uint256 t = uint256(keccak256(abi.encode("nonce", ephPk, o, round, attempt))) % (N - 1) + 1;
        address ta = vm.addr(t);
        uint256 c = reg.keyProofChallenge(
            bytes32(w.publicKeyX), bytes32(w.publicKeyY), ta, round, attempt, o
        );
        return abi.encodePacked(ta, addmod(t, mulmod(c, ephPk, N), N));
    }

    function test_TheRevealKeyIsOnTheCurveAndBoundToTheCoordinatorTerm() public {
        _reg();
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        Vm.Wallet memory w = vm.createWallet(REVEAL_PK);
        vm.expectRevert(OnyxRegistry.NotOnCurve.selector);
        reg.setRevealKey(bytes32(w.publicKeyX), bytes32(w.publicKeyY ^ 1));
        vm.expectRevert(OnyxRegistry.NotOnCurve.selector);
        reg.setRevealKey(bytes32(0), bytes32(0));
        vm.expectRevert(OnyxRegistry.NotOnCurve.selector);
        reg.setRevealKey(bytes32(type(uint256).max), bytes32(w.publicKeyY));
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        reg.setRevealKey(bytes32(w.publicKeyX), bytes32(w.publicKeyY));
        // A draw before any reveal key takes no sealed post.
        bytes32 fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        (bytes32 x0,) = reg.drawRevealKey(fact, 0);
        assertEq(x0, bytes32(0));
        vm.warp(2_500);
        {
            address o = vm.addr(0xA1);
            Vm.Wallet memory e = vm.createWallet(0xE1);
            bytes32 ex = bytes32(e.publicKeyX);
            bytes32 ey = bytes32(e.publicKeyY);
            bytes memory ks = _keyProof(0xE1, o, ROUND, 0);
            bytes memory sg =
                _signText(0xA1, reg.sealedRevealMessage(ROUND, 0, o, ex, ey, keccak256(hex"01")));
            vm.expectRevert(OnyxRegistry.NoRevealKey.selector);
            reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, hex"01", ks, sg);
        }
        // Set, snapshotted per draw, kept by the draw across a rotation.
        reg.setRevealKey(bytes32(w.publicKeyX), bytes32(w.publicKeyY));
        (bytes32 kx, bytes32 ky) = reg.revealKey();
        assertEq(kx, bytes32(w.publicKeyX));
        assertEq(ky, bytes32(w.publicKeyY));
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        _setRevealKey(0x4343);
        (bytes32 d1x,) = reg.drawRevealKey(fact, 1);
        assertEq(d1x, bytes32(w.publicKeyX));
        // Review C11-R1-2: the coordinator clears its key; later draws take none, earlier ones
        // keep theirs; only the coordinator clears.
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        reg.clearRevealKey();
        reg.clearRevealKey();
        (kx, ky) = reg.revealKey();
        assertEq(kx, bytes32(0));
        assertEq(ky, bytes32(0));
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        (bytes32 d2x,) = reg.drawRevealKey(fact, 2);
        assertEq(d2x, bytes32(0));
        (d1x,) = reg.drawRevealKey(fact, 1);
        assertEq(d1x, bytes32(w.publicKeyX));
        _setRevealKey(0x4343);
        // A coordinator change drops the key until the new coordinator sets one.
        vm.mockCall(address(st), abi.encodeWithSignature("coordinator()"), abi.encode(address(7)));
        (kx,) = reg.revealKey();
        assertEq(kx, bytes32(0));
    }

    function test_OnlyTheObserverAndItsEphemeralKeySign() public {
        bytes32 fact = _setup();
        vm.warp(2_500);
        address o = vm.addr(0xA1);
        Vm.Wallet memory w = vm.createWallet(0xE1);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        bytes memory ct = hex"c0ffee";
        bytes memory keySig = _keyProof(0xE1, o, ROUND, 0);
        bytes memory sig =
            _signText(0xA1, reg.sealedRevealMessage(ROUND, 0, o, ex, ey, keccak256(ct)));
        // Another key signing for o, or another ciphertext under o's signature.
        bytes memory forged =
            _signText(0xA2, reg.sealedRevealMessage(ROUND, 0, o, ex, ey, keccak256(ct)));
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, ct, keySig, forged);
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, hex"c0ffef", keySig, sig);
        // C4-3: o2 re-posts o's ephemeral key and ciphertext under its own name: o's key proof
        // names o, and o2 cannot sign one for itself without the ephemeral secret.
        address o2 = vm.addr(0xA2);
        bytes memory sig2 =
            _signText(0xA2, reg.sealedRevealMessage(ROUND, 0, o2, ex, ey, keccak256(ct)));
        vm.expectRevert(OnyxRegistry.BadKeyProof.selector);
        reg.postSealedReveal(fact, 0, o2, ROUND, ex, ey, ct, keySig, sig2);
        // C4-R1-1: a proof for a key o2 holds does not prove o's key; an ECDSA signature by the
        // ephemeral key is no proof at all.
        bytes memory ownKey = _keyProof(0xE2, o2, ROUND, 0);
        vm.expectRevert(OnyxRegistry.BadKeyProof.selector);
        reg.postSealedReveal(fact, 0, o2, ROUND, ex, ey, ct, ownKey, sig2);
        bytes memory ecdsa = _signText(0xE1, abi.encodePacked("any text", o2));
        vm.expectRevert(OnyxRegistry.BadKeyProof.selector);
        reg.postSealedReveal(fact, 0, o2, ROUND, ex, ey, ct, ecdsa, sig2);
        // Lengths, then anyone relays o's post once.
        vm.expectRevert(OnyxRegistry.BadCiphertext.selector);
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, "", keySig, sig);
        vm.expectRevert(OnyxRegistry.BadCiphertext.selector);
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, new bytes(1439), keySig, sig);
        vm.prank(address(0xF00));
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, ct, keySig, sig);
        vm.expectRevert(OnyxRegistry.AlreadyPosted.selector);
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, ct, keySig, sig);
        (bytes32 gx, bytes32 gy, uint64 at, bytes memory got) =
            reg.sealedRevealOf(fact, 0, o, ROUND);
        assertEq(gx, ex);
        assertEq(gy, ey);
        assertEq(at, 2_500);
        assertEq(got, ct);
        // C4-R3-2: the key proof is kept, so the coordinator re-checks it from state.
        assertEq(reg.sealedKeyProofOf(fact, 0, o, ROUND), keySig);
        assertTrue(reg.keyProofValid(gx, gy, ROUND, 0, o, reg.sealedKeyProofOf(fact, 0, o, ROUND)));
        assertEq(reg.sealedKeyProofOf(fact, 0, vm.addr(0xA2), ROUND), new bytes(52));
        // Another attempt: no beacon fixed for it.
        vm.expectRevert(OnyxRegistry.NoDraw.selector);
        reg.postSealedReveal(fact, 1, o, ROUND, ex, ey, ct, keySig, sig);
    }

    /// C4-1: a dropped sealed post fails the log and the record; posts outside (commitClose,
    /// revealClose] are ignored, never a veto; a dismissal needs an in-phase post.
    function test_ADroppedSealedPostFailsTheLogAndTheRecord() public {
        bytes32 fact = _setup();
        bytes32 c1 = keccak256("c1");
        bytes32 c2 = keccak256("c2");
        bytes32 c3 = keccak256("c3");
        _commit(fact, 0xA1, c1);
        _commit(fact, 0xA2, c2);
        _commit(fact, 0xA3, c3);
        vm.warp(CLOSE);
        address o1 = _seal(fact, 0xA1, 0xE1, hex"01"); // in the commit phase: ignored
        vm.warp(2_500);
        address o2 = _seal(fact, 0xA2, 0xE2, hex"02");
        vm.warp(RCLOSE + 1);
        address o3 = _seal(fact, 0xA3, 0xE3, hex"03"); // late: ignored
        bytes memory ok = _e3(o1, c1, 0, o2, c2, 1, o3, c3, 0);
        bytes memory dismissed = _e3(o1, c1, 0, o2, c2, 2, o3, c3, 0);
        bytes memory dropped = _e3(o1, c1, 0, o2, c2, 0, o3, c3, 0);
        assertTrue(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, ok));
        assertTrue(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, dismissed));
        assertFalse(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, dropped));
        // Dismissing a post outside the phase, a status above 2, a bad window.
        assertFalse(
            reg.revealLogComplete(
                fact, 0, ROUND, OPEN, CLOSE, RCLOSE, _e3(o1, c1, 2, o2, c2, 1, o3, c3, 0)
            )
        );
        assertFalse(
            reg.revealLogComplete(
                fact, 0, ROUND, OPEN, CLOSE, RCLOSE, _e3(o1, c1, 0, o2, c2, 3, o3, c3, 0)
            )
        );
        assertFalse(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, CLOSE, ok));
        assertFalse(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, hex"00"));
        // Amendment 8 rules still hold: a posted commit left out.
        assertFalse(
            reg.revealLogComplete(
                fact, 0, ROUND, OPEN, CLOSE, RCLOSE, _e3(o1, bytes32(0), 0, o2, c2, 1, o3, c3, 0)
            )
        );
        bytes32 result = keccak256("no result");
        vm.expectRevert(OnyxRegistry.IncompleteTranscript.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, result, dropped);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, result, ok);
        assertTrue(reg.attemptMatches(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, result, ok));
        assertFalse(reg.attemptMatches(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, result, dismissed));
        assertFalse(reg.attemptMatches(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("value"), ok));
        assertFalse(reg.attemptMatches(fact, 0, ROUND, OPEN + 1, CLOSE, RCLOSE, result, ok));
    }

    /// Review C11-R2-1: clearing (or rotating) the reveal key after a draw is announced, in its
    /// commit phase or its reveal phase, changes nothing for that draw: its posts still land
    /// under the key it snapshotted, and the log still requires every in-phase post.
    function test_ClearingTheRevealKeyMidDrawChangesNothingForTheDraw() public {
        bytes32 fact = _setup();
        bytes32 c1 = keccak256("c1");
        bytes32 c2 = keccak256("c2");
        _commit(fact, 0xA1, c1);
        _commit(fact, 0xA2, c2);
        (bytes32 kx, bytes32 ky) = reg.drawRevealKey(fact, 0);
        reg.clearRevealKey(); // in the commit phase
        vm.warp(2_500);
        address o1 = _seal(fact, 0xA1, 0xE1, hex"01");
        _setRevealKey(0x4343); // rotated in the reveal phase
        reg.clearRevealKey();
        address o2 = _seal(fact, 0xA2, 0xE2, hex"02");
        (bytes32 x, bytes32 y) = reg.drawRevealKey(fact, 0);
        assertEq(x, kx);
        assertEq(y, ky);
        vm.warp(RCLOSE + 1);
        bytes memory both = abi.encodePacked(_entry(o1, c1, 1), _entry(o2, c2, 1));
        bytes memory dropped = abi.encodePacked(_entry(o1, c1, 1), _entry(o2, c2, 0));
        if (o2 < o1) {
            both = abi.encodePacked(_entry(o2, c2, 1), _entry(o1, c1, 1));
            dropped = abi.encodePacked(_entry(o2, c2, 0), _entry(o1, c1, 1));
        }
        assertTrue(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, both));
        assertFalse(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, dropped));
        vm.expectRevert(OnyxRegistry.IncompleteTranscript.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("none"), dropped);
    }

    /// A receipted reveal can only be status 1; a dismissal of it is refused.
    function test_AReceiptedRevealIsNeverDismissed() public {
        bytes32 fact = _setup();
        bytes32 c1 = keccak256("c1");
        address o1 = _commit(fact, 0xA1, c1);
        vm.warp(2_500);
        _seal(fact, 0xA1, 0xE1, hex"01");
        reg.postReceipt(
            fact, 0, o1, ROUND, c1, _signText(RECEIPT_PK, reg.receiptMessage(ROUND, 0, o1, c1))
        );
        assertTrue(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, _entry(o1, c1, 1)));
        assertFalse(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, _entry(o1, c1, 2)));
    }

    function test_RecordAttemptRules() public {
        bytes32 fact = _setup();
        bytes memory e = _entry(vm.addr(0xA1), bytes32(0), 0);
        bytes32 result = keccak256("no result");
        vm.warp(RCLOSE);
        vm.expectRevert(OnyxRegistry.TooEarly.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, result, e);
        vm.warp(RCLOSE + 1);
        vm.expectRevert(OnyxRegistry.BadWindow.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, RCLOSE, RCLOSE, result, e);
        vm.expectRevert(OnyxRegistry.ZeroValue.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, bytes32(0), e);
        vm.expectRevert(OnyxRegistry.NoDraw.selector);
        reg.recordAttempt(fact, 1, ROUND, OPEN, CLOSE, RCLOSE, result, e);
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, result, e);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, result, e);
        vm.expectRevert(OnyxRegistry.AlreadyRecorded.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("other"), e);
        assertEq(
            reg.attemptRecord(fact, 0), reg.attemptDigest(ROUND, OPEN, CLOSE, RCLOSE, result, e)
        );
    }

    /// Gas of a sealed post (a numeric reveal is about 154 bytes) and of the checks, for
    /// docs/ops/benchmarks.md.
    function test_GasOfSealedPosts() public {
        bytes32 fact = _setup();
        bytes32 c1 = keccak256("c1");
        address o = _commit(fact, 0xA1, c1);
        vm.warp(2_500);
        Vm.Wallet memory w = vm.createWallet(0xE1);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        bytes memory ct = new bytes(154);
        bytes memory keySig = _keyProof(0xE1, o, ROUND, 0);
        bytes memory sig =
            _signText(0xA1, reg.sealedRevealMessage(ROUND, 0, o, ex, ey, keccak256(ct)));
        uint256 g = gasleft();
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, ct, keySig, sig);
        emit log_named_uint("postSealedReveal gas (154 bytes)", g - gasleft());
        bytes memory e = bytes.concat(
            _entry(address(2), bytes32(0), 0), _entry(address(3), bytes32(0), 0), _entry(o, c1, 1)
        );
        g = gasleft();
        assertTrue(reg.revealLogComplete(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, e));
        emit log_named_uint("revealLogComplete gas (3 observers)", g - gasleft());
        vm.warp(RCLOSE + 1);
        g = gasleft();
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("r"), e);
        emit log_named_uint("recordAttempt gas (3 observers)", g - gasleft());
    }

    /// `n` pseudorandom bytes: a real ciphertext is a Keccak stream, so every storage word of
    /// it is nonzero (an all-zero test ciphertext writes zero words and under-measures).
    function _pseudo(uint256 n, bytes32 seed) internal pure returns (bytes memory b) {
        b = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            if (i % 32 == 0) seed = keccak256(abi.encode(seed, i));
            b[i] = seed[i % 32];
        }
    }

    /// ROADMAP C12 (ADR-0027 amendment 12): the gas of a sealed post at today's numeric
    /// length (154 bytes), with the time-lock layer's 128 bytes in front (282 bytes) and, since
    /// ROADMAP C52, with the far-date copy and wrapped key too (426 bytes), all
    /// pseudorandom as real ciphertexts are, on the unchanged registry. The figures are the
    /// call's execution gas: they leave out the 21,000 base cost and the calldata cost of a
    /// real transaction (review C12-R1-8). Each post is measured with the registry's account
    /// and storage marked cold first (`vm.cool`), as a post sent in a transaction of its own
    /// finds them (review C12-R2: without it the setup's writes left shared slots warm and
    /// both figures were low). A third post of 154 ZERO bytes shows why amendment 10 item
    /// 13's figure was low: zero storage words cost no fresh-slot writes. Ranges catch a
    /// regression either way.
    function test_GasOfTimeLockedSizedPosts() public {
        bytes32 fact = _setup();
        _commit(fact, 0xA1, keccak256("c1"));
        _commit(fact, 0xA2, keccak256("c2"));
        _commit(fact, 0xA3, keccak256("c3"));
        _commit(fact, 0xA4, keccak256("c4"));
        vm.warp(2_500);
        uint256 g154 = _gasOfPost(fact, 0xA1, 0xE1, 154, false);
        uint256 g282 = _gasOfPost(fact, 0xA2, 0xE2, 282, false);
        uint256 gZero = _gasOfPost(fact, 0xA3, 0xE3, 154, true);
        uint256 g426 = _gasOfPost(fact, 0xA4, 0xE4, 426, false);
        emit log_named_uint("postSealedReveal gas, cold (154 random bytes)", g154);
        emit log_named_uint("postSealedReveal gas, cold (282 random bytes)", g282);
        emit log_named_uint("postSealedReveal gas, cold (154 zero bytes)", gZero);
        emit log_named_uint("postSealedReveal gas, cold (426 random bytes, C52)", g426);
        // ROADMAP C52: the far-date copy and wrapped key, 144 bytes, 4.5 more words.
        assertGt(g426 - g282, 90_000);
        assertLt(g426 - g282, 120_000);
        assertGt(g154, 340_000);
        assertLt(g154, 420_000);
        assertGt(g282, 430_000);
        assertLt(g282, 520_000);
        // Four more 32-byte words, each a fresh nonzero slot (~22,100 gas).
        assertGt(g282 - g154, 80_000);
        assertLt(g282 - g154, 100_000);
        // Five 32-byte words written nonzero instead of zero (~20,000 gas each).
        assertGt(g154 - gZero, 90_000);
        assertLt(g154 - gZero, 110_000);
    }

    /// Gas of observer `pk`'s `postSealedReveal` alone (proofs and signatures made first),
    /// with the registry cold as in a transaction of its own; `zero` posts all-zero bytes.
    function _gasOfPost(bytes32 fact, uint256 pk, uint256 ephPk, uint256 n, bool zero)
        internal
        returns (uint256 used)
    {
        address o = vm.addr(pk);
        bytes memory ct = zero ? new bytes(n) : _pseudo(n, bytes32(n));
        Vm.Wallet memory w = vm.createWallet(ephPk);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        bytes memory keySig = _keyProof(ephPk, o, ROUND, 0);
        bytes memory sig =
            _signText(pk, reg.sealedRevealMessage(ROUND, 0, o, ex, ey, keccak256(ct)));
        vm.cool(address(reg));
        uint256 g = gasleft();
        reg.postSealedReveal(fact, 0, o, ROUND, ex, ey, ct, keySig, sig);
        used = g - gasleft();
    }

    /// The texts the contract rebuilds are the ones any keys sign: fresh observer and
    /// ephemeral keys, round and ciphertext post; a swapped ciphertext is refused.
    function testFuzz_SealedTextsRoundTrip(uint256 pk, uint256 ephPk, bytes32 round, bytes32 salt)
        public
    {
        uint256 n1 = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364140;
        pk = bound(pk, 1, n1);
        ephPk = bound(ephPk, 1, n1);
        vm.assume(round != bytes32(0));
        bytes32 fact = _setup();
        vm.warp(2_500);
        address o = vm.addr(pk);
        Vm.Wallet memory w = vm.createWallet(ephPk);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        assertTrue(reg.onCurve(ex, ey));
        bytes memory ct = abi.encodePacked(salt, salt, uint8(7));
        vm.assume(uint256(ex) < N);
        bytes memory keySig = _keyProof(ephPk, o, round, 0);
        bytes memory sig =
            _signText(pk, reg.sealedRevealMessage(round, 0, o, ex, ey, keccak256(ct)));
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postSealedReveal(fact, 0, o, round, ex, ey, abi.encodePacked(ct, "x"), keySig, sig);
        reg.postSealedReveal(fact, 0, o, round, ex, ey, ct, keySig, sig);
        assertTrue(
            reg.revealLogComplete(fact, 0, round, OPEN, CLOSE, RCLOSE, _entry(o, bytes32(0), 2))
        );
        assertFalse(
            reg.revealLogComplete(fact, 0, round, OPEN, CLOSE, RCLOSE, _entry(o, bytes32(0), 0))
        );
    }

    /// Review C4-R2-3: a well-formed random proof `(T, z)` for a key the poster does not hold
    /// never verifies, nor does a real proof for `-R`; a point with `R.x >= n` is refused.
    function testFuzz_ARandomKeyProofNeverVerifies(address t, uint256 z) public {
        Vm.Wallet memory w = vm.createWallet(0xE1);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        _reg();
        address o = vm.addr(0xA1);
        bytes memory real = _keyProof(0xE1, o, ROUND, 0);
        vm.assume(keccak256(abi.encodePacked(t, z)) != keccak256(real));
        assertTrue(reg.keyProofValid(ex, ey, ROUND, 0, o, real));
        assertFalse(reg.keyProofValid(ex, ey, ROUND, 0, o, abi.encodePacked(t, z)));
        bytes32 negY = bytes32(
            0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F - w.publicKeyY
        );
        assertFalse(reg.keyProofValid(ex, negY, ROUND, 0, o, real));
        assertFalse(reg.keyProofValid(ex, ey, ROUND, 1, o, real));
        assertFalse(reg.keyProofValid(ex, ey, ROUND, 0, vm.addr(0xA2), real));
        // x = n is on the curve; `ecrecover` cannot take it as r, so it is refused.
        bytes32 hy = 0x98f66641cb0ae1776b463ebdee3d77fe2658f021db48e2c8ac7ab4c92f83621e;
        assertTrue(reg.onCurve(bytes32(N), hy));
        assertFalse(reg.keyProofValid(bytes32(N), hy, ROUND, 0, o, abi.encodePacked(t, z)));
    }

    /// A proof for (ex, ey) made honestly by the holder of `ephPk`, whatever `ey` says: the
    /// challenge hashes the `ey` given, `z = t + c·r` uses the real secret.
    function _keyProofFor(uint256 ephPk, bytes32 ex, bytes32 ey, address o)
        internal
        view
        returns (address ta, uint256 z)
    {
        uint256 t = uint256(keccak256(abi.encode("nonce", ephPk, o, ey))) % (N - 1) + 1;
        ta = vm.addr(t);
        uint256 c = reg.keyProofChallenge(ex, ey, ta, ROUND, 0, o);
        z = addmod(t, mulmod(c, ephPk, N), N);
    }

    /// Review C4-R3-1: edge cases of the key proof. An off-curve `R` whose y has the real
    /// point's parity is refused even with an honest proof (without the on-curve check
    /// `ecrecover` uses the real point and accepts it, and the coordinator's secp256k1 library
    /// then refuses the post: a slot nobody can open or dismiss, a veto of every round); the
    /// infinity encoding (0, 0), coordinates at or above p, `z` of 0 or n, and a zero
    /// `T` are refused.
    function test_KeyProofEdgeCases() public {
        _reg();
        address o = vm.addr(0xA1);
        Vm.Wallet memory w = vm.createWallet(0xE1);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        (address ta, uint256 z) = _keyProofFor(0xE1, ex, ey, o);
        assertTrue(reg.keyProofValid(ex, ey, ROUND, 0, o, abi.encodePacked(ta, z)));
        // Same parity, off the curve (only y and p - y are on it, and p - y has the other parity).
        uint256 yOff = w.publicKeyY + 2 < P_ ? w.publicKeyY + 2 : w.publicKeyY - 2;
        assertFalse(reg.onCurve(ex, bytes32(yOff)));
        (address to, uint256 zo) = _keyProofFor(0xE1, ex, bytes32(yOff), o);
        assertFalse(reg.keyProofValid(ex, bytes32(yOff), ROUND, 0, o, abi.encodePacked(to, zo)));
        // Coordinates at or above p are refused.
        assertFalse(reg.onCurve(bytes32(P_), ey));
        assertFalse(reg.onCurve(ex, bytes32(P_)));
        assertFalse(reg.onCurve(bytes32(0), bytes32(0)));
        assertFalse(reg.keyProofValid(bytes32(0), bytes32(0), ROUND, 0, o, abi.encodePacked(ta, z)));
        assertFalse(reg.keyProofValid(ex, ey, ROUND, 0, o, abi.encodePacked(ta, uint256(0))));
        assertFalse(reg.keyProofValid(ex, ey, ROUND, 0, o, abi.encodePacked(ta, N)));
        assertFalse(reg.keyProofValid(ex, ey, ROUND, 0, o, abi.encodePacked(address(0), z)));
        assertFalse(reg.keyProofValid(ex, ey, ROUND, 0, o, abi.encodePacked(ta, z, uint8(0))));
        // The same off-curve key through postSealedReveal: refused.
        bytes32 fact = _setup();
        vm.warp(2_500);
        bytes memory ct = hex"c0ffee";
        bytes memory sig =
            _signText(0xA1, reg.sealedRevealMessage(ROUND, 0, o, ex, bytes32(yOff), keccak256(ct)));
        vm.expectRevert(OnyxRegistry.BadKeyProof.selector);
        reg.postSealedReveal(
            fact, 0, o, ROUND, ex, bytes32(yOff), ct, abi.encodePacked(to, zo), sig
        );
    }

    uint256 internal constant P_ =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F;

    /// Rust <-> Solidity (fixtures/registry/sealed-reveals.json): texts, Rust's real sealed
    /// posts (both signatures), every `revealLogComplete` calldata answers as Rust's simulated
    /// registry, and the attempt record and `attemptMatches`.
    function test_RustSealedRevealVectors() public {
        string memory json = vm.readFile("../fixtures/registry/sealed-reveals.json");
        _reg();
        reg.setReceiptSigner(vm.parseJsonAddress(json, ".receiptSigner"));
        reg.setRevealKey(
            vm.parseJsonBytes32(json, ".revealKeyX"), vm.parseJsonBytes32(json, ".revealKeyY")
        );
        bytes32 fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        assertEq(fact, vm.parseJsonBytes32(json, ".fact"), "Rust registry_fact_key");
        bytes32 round = vm.parseJsonBytes32(json, ".round");
        address[] memory obs = vm.parseJsonAddressArray(json, ".observers");
        bytes32[] memory ex = vm.parseJsonBytes32Array(json, ".postEx");
        bytes32[] memory ey = vm.parseJsonBytes32Array(json, ".postEy");
        bytes[] memory ct = vm.parseJsonBytesArray(json, ".postCiphertext");
        {
            bytes[] memory kp = vm.parseJsonBytesArray(json, ".postKeyProof");
            assertEq(
                reg.keyProofChallenge(ex[0], ey[0], address(bytes20(kp[0])), round, 0, obs[0]),
                uint256(vm.parseJsonBytes32(json, ".keyProofChallenge")),
                "Rust key_proof_challenge"
            );
        }
        assertEq(
            reg.sealedRevealMessage(round, 0, obs[0], ex[0], ey[0], keccak256(ct[0])),
            vm.parseJsonBytes(json, ".sealedRevealMessage")
        );
        {
            bytes32[] memory commits = vm.parseJsonBytes32Array(json, ".commits");
            bytes[] memory csigs = vm.parseJsonBytesArray(json, ".commitSigs");
            vm.warp(vm.parseJsonUint(json, ".commitPostedAt"));
            for (uint256 i = 0; i < 3; i++) {
                reg.postCommit(fact, 0, obs[i], round, commits[i], csigs[i]);
            }
        }
        {
            bytes[] memory ks = vm.parseJsonBytesArray(json, ".postKeyProof");
            bytes[] memory ss = vm.parseJsonBytesArray(json, ".postSig");
            uint256[] memory at = vm.parseJsonUintArray(json, ".postedAt");
            for (uint256 i = 0; i < 3; i++) {
                vm.warp(at[i]);
                reg.postSealedReveal(fact, 0, obs[i], round, ex[i], ey[i], ct[i], ks[i], ss[i]);
            }
        }
        bytes[] memory calls = vm.parseJsonBytesArray(json, ".caseCalldata");
        bool[] memory want = vm.parseJsonBoolArray(json, ".caseExpect");
        string[] memory names = vm.parseJsonStringArray(json, ".caseNames");
        assertEq(calls.length, want.length);
        assertGt(calls.length, 5);
        for (uint256 i = 0; i < calls.length; i++) {
            (bool ok, bytes memory ret) = address(reg).staticcall(calls[i]);
            assertTrue(ok, names[i]);
            assertEq(abi.decode(ret, (bool)), want[i], names[i]);
        }
        uint64 rc = uint64(vm.parseJsonUint(json, ".revealClose"));
        bytes memory entries = vm.parseJsonBytes(json, ".recordEntries");
        bytes32 result = vm.parseJsonBytes32(json, ".recordResult");
        vm.warp(rc + 1);
        reg.recordAttempt(fact, 0, round, OPEN, CLOSE, rc, result, entries);
        assertEq(reg.attemptRecord(fact, 0), vm.parseJsonBytes32(json, ".recordDigest"));
        (bool ok1, bytes memory r1) =
            address(reg).staticcall(vm.parseJsonBytes(json, ".matchesCalldata"));
        assertTrue(ok1 && abi.decode(r1, (bool)), "Rust attemptMatches calldata");
        (bool ok2, bytes memory r2) =
            address(reg).staticcall(vm.parseJsonBytes(json, ".otherMatchesCalldata"));
        assertTrue(ok2 && !abi.decode(r2, (bool)), "other entries never match");
    }

    // ------------------------------------------------------------- amendment 14 (ROADMAP C51)
    // A registry-wide, one-way mode in which every draw is time-locked: `requireTimeLock`
    // (owner or coordinator) turns it on and nothing turns it off; `announceDraw` then needs a
    // reveal key and snapshots `drawTimeLocked`; a time-locked draw's posts carry the 128-byte
    // drand time-lock, the 144-byte far-date copy (ROADMAP C52) and a body; rounds of draws
    // announced before the mode are no longer recorded. The cap is 1,438 bytes (THREATS C4-7,
    // C11-R2-1, C12-7, C51-*, C52-6).

    event TimeLockRequired(address by);

    /// Observer `pk` posts `n` pseudorandom bytes to draw `attempt` (proofs made first).
    function _post(bytes32 fact, uint32 attempt, uint256 pk, uint256 ephPk, uint256 n)
        internal
        returns (bool landed)
    {
        address o = vm.addr(pk);
        bytes memory ct = _pseudo(n, keccak256(abi.encode(n, pk)));
        Vm.Wallet memory w = vm.createWallet(ephPk);
        bytes32 ex = bytes32(w.publicKeyX);
        bytes32 ey = bytes32(w.publicKeyY);
        bytes memory kp = _keyProof(ephPk, o, ROUND, attempt);
        bytes memory sig =
            _signText(pk, reg.sealedRevealMessage(ROUND, attempt, o, ex, ey, keccak256(ct)));
        try reg.postSealedReveal(fact, attempt, o, ROUND, ex, ey, ct, kp, sig) {
            landed = true;
        } catch (bytes memory err) {
            assertEq(bytes4(err), OnyxRegistry.BadCiphertext.selector, "only the length refuses");
        }
    }

    function test_TheCapIsTheLargestExactAnswerSealedAndTimeLocked() public {
        _reg();
        assertEq(reg.MAX_SEALED_BYTES(), 1438);
        assertEq(reg.TIME_LOCK_BYTES(), 128);
        assertEq(reg.FAR_COPY_BYTES(), 144);
        // 128 (time-lock) + 144 (far-date copy and wrapped body key, ROADMAP C52) + 142 (borsh
        // frame of a signed exact reveal) + 1,024 (largest value): no byte more than needed.
        assertEq(reg.MAX_SEALED_BYTES(), reg.TIME_LOCK_BYTES() + reg.FAR_COPY_BYTES() + 142 + 1024);
    }

    function test_OffByDefaultAndDrawsAreNotTimeLocked() public {
        bytes32 fact = _setup();
        assertFalse(reg.timeLockRequired());
        assertFalse(reg.drawTimeLocked(fact, 0));
        vm.warp(2_500);
        // A draw that is not time-locked takes any length up to the cap (amendment 10 posts).
        assertTrue(_post(fact, 0, 0xA1, 0xE1, 1));
        assertTrue(_post(fact, 0, 0xA2, 0xE2, 1438));
        assertFalse(_post(fact, 0, 0xA3, 0xE3, 1439));
    }

    function test_OwnerOrCoordinatorTurnItOnStrangersCannotNobodyTurnsItOff() public {
        _reg();
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotOwner.selector);
        reg.requireTimeLock();
        assertFalse(reg.timeLockRequired());
        vm.expectEmit(address(reg));
        emit TimeLockRequired(gov);
        vm.prank(gov);
        reg.requireTimeLock();
        assertTrue(reg.timeLockRequired());
        // Again, by the coordinator: no change, no event, no revert.
        vm.recordLogs();
        reg.requireTimeLock();
        assertEq(vm.getRecordedLogs().length, 0);
        assertTrue(reg.timeLockRequired());
        // Clearing the reveal key or a coordinator change leaves the mode on.
        reg.clearRevealKey();
        vm.mockCall(address(st), abi.encodeWithSignature("coordinator()"), abi.encode(address(7)));
        vm.mockCall(address(st), abi.encodeWithSignature("coordinatorEpoch()"), abi.encode(9));
        assertTrue(reg.timeLockRequired());
        vm.clearMockedCalls();
        // The coordinator alone can turn it on as well.
        _reg();
        reg.requireTimeLock();
        assertTrue(reg.timeLockRequired());
        // No function turns it off: none of the obvious names exists.
        string[4] memory off = [
            "disableTimeLock()",
            "clearTimeLock()",
            "setTimeLockRequired(bool)",
            "unrequireTimeLock()"
        ];
        for (uint256 i = 0; i < off.length; i++) {
            (bool ok,) = address(reg).call(abi.encodeWithSignature(off[i], false));
            assertFalse(ok, off[i]);
        }
        assertTrue(reg.timeLockRequired());
    }

    /// C11-R2-1 class: with the mode on, a coordinator cannot announce a draw without a reveal
    /// key (cleared, never set, or set by a former coordinator term), and every draw it does
    /// announce is time-locked.
    function test_WithTheModeOnEveryDrawIsKeyedAndTimeLocked() public {
        _reg();
        reg.requireTimeLock();
        bytes32 fact = reg.factKey(FEED_METRIC, SUBJECT, WINDOW);
        bytes32 commit = reg.drawCommit(POOL, SPEC, CFG);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        vm.expectRevert(OnyxRegistry.NoRevealKey.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, commit, h);
        _setRevealKey(REVEAL_PK);
        reg.clearRevealKey();
        vm.expectRevert(OnyxRegistry.NoRevealKey.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, commit, h);
        _setRevealKey(REVEAL_PK);
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        assertTrue(reg.drawTimeLocked(fact, 0));
        (bytes32 kx,) = reg.drawRevealKey(fact, 0);
        assertTrue(kx != bytes32(0));
        // Clearing the key mid-draw changes nothing for the draw; the next one is refused.
        reg.clearRevealKey();
        assertTrue(reg.drawTimeLocked(fact, 0));
        vm.expectRevert(OnyxRegistry.NoRevealKey.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, commit, uint64(vm.getBlockNumber()) + 1);
        // A new coordinator term: the old term's key does not count.
        _setRevealKey(REVEAL_PK);
        vm.mockCall(address(st), abi.encodeWithSignature("coordinatorEpoch()"), abi.encode(1));
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        vm.expectRevert(OnyxRegistry.NoRevealKey.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, commit, uint64(vm.getBlockNumber()) + 1);
        _setRevealKey(0x4343);
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        assertTrue(reg.drawTimeLocked(fact, 1));
    }

    /// A time-locked draw takes a post only with a time-lock, its far-date copy and the wrapped
    /// body key (ROADMAP C52) and a body: 273..=1,438 bytes.
    function test_ATimeLockedDrawsPostsCarryATimeLock() public {
        _reg();
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        _setRevealKey(REVEAL_PK);
        reg.requireTimeLock();
        bytes32 fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        vm.warp(2_500);
        assertFalse(_post(fact, 0, 0xA1, 0xE1, 1));
        assertFalse(_post(fact, 0, 0xA1, 0xE1, 128));
        // A post with a time-lock and a body but no room for the far copy no longer lands (a
        // longer one does whatever its bytes: the coordinator checks the far copy, C52-3).
        assertFalse(_post(fact, 0, 0xA1, 0xE1, 129));
        assertFalse(_post(fact, 0, 0xA1, 0xE1, 200));
        assertFalse(_post(fact, 0, 0xA1, 0xE1, 272));
        assertFalse(_post(fact, 0, 0xA1, 0xE1, 1439));
        assertTrue(_post(fact, 0, 0xA1, 0xE1, 273));
        assertTrue(_post(fact, 0, 0xA2, 0xE2, 1438));
        assertTrue(_post(fact, 0, 0xA3, 0xE3, 426));
    }

    /// The pre-announcement downgrade: draws announced before the mode was turned on (keyless,
    /// or keyed without a time-lock) are never recorded once it is on, neither a value
    /// (`recordObservation`) nor an attempt (`recordAttempt`); the fact's next draw is
    /// time-locked and records.
    function test_DrawsAnnouncedBeforeTheModeAreNeverRecordedAfterIt() public {
        bytes32 fact = _setup(); // draw 0: keyed, not time-locked
        bytes memory e = _entry(vm.addr(0xA1), bytes32(0), 0);
        reg.requireTimeLock();
        assertFalse(reg.drawTimeLocked(fact, 0));
        vm.warp(RCLOSE + 1);
        vm.expectRevert(OnyxRegistry.NotTimeLocked.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("none"), e);
        vm.expectRevert(OnyxRegistry.NotTimeLocked.selector);
        reg.recordObservation(FEED_METRIC, SUBJECT, WINDOW, SPEC, keccak256("v"), POOL, CFG);
        // The fact's next draw is time-locked: both records work for it.
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        assertTrue(reg.drawTimeLocked(fact, 1));
        reg.recordAttempt(fact, 1, ROUND, OPEN, CLOSE, RCLOSE, keccak256("none"), e);
        reg.recordObservation(FEED_METRIC, SUBJECT, WINDOW, SPEC, keccak256("v"), POOL, CFG);
        // Draw 0 still cannot be recorded afterwards.
        vm.expectRevert(OnyxRegistry.NotTimeLocked.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("none"), e);
    }

    /// With the mode off, nothing changes for records (C4/C11 behaviour).
    function test_WithTheModeOffRecordsAreAsBefore() public {
        bytes32 fact = _setup();
        bytes memory e = _entry(vm.addr(0xA1), bytes32(0), 0);
        vm.warp(RCLOSE + 1);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("none"), e);
        assertTrue(reg.attemptMatches(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("none"), e));
    }

    /// A time-locked post's length decides alone whether it lands (given valid proofs).
    function testFuzz_TimeLockedPostLengths(uint256 n) public {
        n = bound(n, 0, 1500);
        _reg();
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        _setRevealKey(REVEAL_PK);
        reg.requireTimeLock();
        bytes32 fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        vm.warp(2_500);
        assertEq(_post(fact, 0, 0xA1, 0xE1, n), n > 272 && n <= 1438);
    }

    /// Gas of a numeric time-locked post with its far-date copy (426 pseudorandom bytes, ROADMAP
    /// C52) in a time-locked draw, cold: the length rule reads the draw's flag only for posts of
    /// 272 bytes or fewer (review C51-R1-5). Measured: see amendment 15 item 10.
    function test_GasOfATimeLockedPost() public {
        _reg();
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        _setRevealKey(REVEAL_PK);
        reg.requireTimeLock();
        bytes32 fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        _commit(fact, 0xA1, keccak256("c1"));
        vm.warp(2_500);
        uint256 g = _gasOfPost(fact, 0xA1, 0xE1, 426, false);
        emit log_named_uint("postSealedReveal gas, cold, time-locked draw (426 random bytes)", g);
        assertGt(g, 530_000);
        assertLt(g, 640_000);
    }

    /// Review C51-R2: the remaining downgrade paths. A draw announced in the SAME block as
    /// `requireTimeLock` (just before it) is not time-locked and is never recorded; draws granted
    /// by `reopenDraws` are time-locked; a new coordinator (another address, a new term) announces
    /// nothing until it sets its own key, then only time-locked draws, and cannot turn the mode
    /// off; the former coordinator can no longer call `requireTimeLock`. A post over 272 bytes
    /// (128 before ROADMAP C52) lands whatever its bytes: the chain cannot tell a time-lock or a
    /// far copy, the clients check every revealed post's time-lock and the coordinator every far
    /// copy (THREATS C51-R2-1, C52-3).
    function test_C51R2_SameBlockReopenAndRotationStayTimeLocked() public {
        _reg();
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        _setRevealKey(REVEAL_PK);
        bytes32 fact = reg.factKey(FEED_METRIC, SUBJECT, WINDOW);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, reg.drawCommit(POOL, SPEC, CFG), h);
        reg.requireTimeLock(); // the same block
        assertFalse(reg.drawTimeLocked(fact, 0));
        vm.roll(h + 1);
        vm.setBlockhash(h, keccak256("c51r2 beacon"));
        reg.fixBeacon(fact, 0);
        vm.warp(2_500);
        // The chain takes any ciphertext over 272 bytes in a time-locked draw (none here: draw 0
        // is not time-locked, so even 1 byte lands).
        assertTrue(_post(fact, 0, 0xA1, 0xE1, 1));
        vm.warp(RCLOSE + 1);
        bytes memory e = _entry(vm.addr(0xA1), bytes32(0), 0);
        vm.expectRevert(OnyxRegistry.NotTimeLocked.selector);
        reg.recordAttempt(fact, 0, ROUND, OPEN, CLOSE, RCLOSE, keccak256("none"), e);
        vm.expectRevert(OnyxRegistry.NotTimeLocked.selector);
        reg.recordObservation(FEED_METRIC, SUBJECT, WINDOW, SPEC, keccak256("v"), POOL, CFG);
        // Draws 1 and 2 use up the subject's allowance; the owner's reopen grants more, each
        // time-locked.
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        assertTrue(reg.drawTimeLocked(fact, 1) && reg.drawTimeLocked(fact, 2));
        bytes32 commit = reg.drawCommit(POOL, SPEC, CFG);
        uint64 nh = uint64(vm.getBlockNumber()) + 1;
        vm.expectRevert(OnyxRegistry.DrawsExhausted.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 3, commit, nh);
        vm.prank(gov);
        reg.reopenDraws(FEED_METRIC, SUBJECT);
        _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        assertTrue(reg.drawTimeLocked(fact, 3));
        // A 154-byte post (an amendment 10 numeric post's length) no longer lands in a
        // time-locked draw (ROADMAP C52: 272 bytes come before the body), but 426 bytes of any
        // content do: the chain cannot tell a time-lock or a far copy (C51-R2-1, C52-3).
        vm.warp(2_500);
        assertFalse(_post(fact, 3, 0xA2, 0xE2, 154));
        assertTrue(_post(fact, 3, 0xA2, 0xE2, 426));
        // A new coordinator: another address, a new term.
        address next = address(0xC0DE);
        vm.mockCall(address(st), abi.encodeWithSignature("coordinator()"), abi.encode(next));
        vm.mockCall(address(st), abi.encodeWithSignature("coordinatorEpoch()"), abi.encode(7));
        vm.expectRevert(OnyxRegistry.NotOwner.selector);
        reg.requireTimeLock(); // the former coordinator
        vm.startPrank(next);
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        nh = uint64(vm.getBlockNumber()) + 1;
        vm.expectRevert(OnyxRegistry.NoRevealKey.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 4, commit, nh);
        Vm.Wallet memory w = vm.createWallet(0x4444);
        reg.setRevealKey(bytes32(w.publicKeyX), bytes32(w.publicKeyY));
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 4, commit, nh);
        reg.requireTimeLock(); // allowed, changes nothing
        vm.stopPrank();
        assertTrue(reg.drawTimeLocked(fact, 4));
        assertTrue(reg.timeLockRequired());
    }
}
