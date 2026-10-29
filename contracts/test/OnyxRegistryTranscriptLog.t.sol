// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxRegistryTest} from "./OnyxRegistry.t.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// ADR-0027 amendment 8 (SIG-R1-F1): observers post commits and the coordinator's reveal
/// receipts on chain; `transcriptComplete` refuses a served round that leaves either out.
contract OnyxRegistryTranscriptLogTest is OnyxRegistryTest {
    bytes32 internal constant SPEC = keccak256("spec");
    bytes32 internal constant ROUND = keccak256("round");
    uint256 internal constant RECEIPT_PK = 0x5EC5EC;
    uint64 internal constant OPEN = 1_000;
    uint64 internal constant CLOSE = 2_000;

    function _signText(uint256 pk, bytes memory text) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, OnyxSig.eip191Digest(text));
        return abi.encodePacked(r, s, v);
    }

    /// Registry with the receipt key `RECEIPT_PK`, and draw 0 of the feed fact announced and
    /// its beacon fixed.
    function _setup() internal returns (bytes32 fact) {
        _reg();
        reg.setReceiptSigner(vm.addr(RECEIPT_PK));
        fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        vm.warp(1_500);
    }

    function _commit(bytes32 fact, uint256 pk, bytes32 commit) internal returns (address o) {
        o = vm.addr(pk);
        reg.postCommit(
            fact, 0, o, ROUND, commit, _signText(pk, reg.commitMessage(ROUND, 0, o, commit))
        );
    }

    function _receipt(bytes32 fact, address o, bytes32 commit) internal {
        reg.postReceipt(
            fact,
            0,
            o,
            ROUND,
            commit,
            _signText(RECEIPT_PK, reg.receiptMessage(ROUND, 0, o, commit))
        );
    }

    function _entry(address o, bytes32 c, bool revealed) internal pure returns (bytes memory) {
        return abi.encodePacked(o, c, revealed ? uint8(1) : uint8(0));
    }

    function test_EveryDrawNeedsAReceiptSigner() public {
        reg = new OnyxRegistry(gov, IRegistryLedger(address(st)));
        vm.prank(gov);
        reg.registerMetric(FEED_METRIC, OnyxRegistry.Kind.Feed, FEED_TEMPLATE, 0);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        vm.expectRevert(OnyxRegistry.NoReceiptSigner.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 0, keccak256("c"), h);
        vm.prank(address(0xBAD));
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        reg.setReceiptSigner(address(0x5EC));
        vm.expectRevert(OnyxRegistry.ZeroValue.selector);
        reg.setReceiptSigner(address(0));
    }

    /// A rotation after the draw never voids the draw's receipts.
    function test_ARotationKeepsTheDrawsSigner() public {
        bytes32 fact = _setup();
        reg.setReceiptSigner(address(0x5EC));
        assertEq(reg.drawReceiptSigner(fact, 0), vm.addr(RECEIPT_PK));
        address o = _commit(fact, 0xA1, keccak256("c1"));
        _receipt(fact, o, keccak256("c1"));
        assertEq(reg.receiptLog(reg.logKey(fact, 0, o, ROUND)), keccak256("c1"));
    }

    function test_OnlyTheObserverAndTheReceiptKeySign() public {
        bytes32 fact = _setup();
        address o = vm.addr(0xA1);
        bytes32 c = keccak256("c1");
        // Another key's commit signature for this observer.
        bytes memory forged = _signText(0xA2, reg.commitMessage(ROUND, 0, o, c));
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postCommit(fact, 0, o, ROUND, c, forged);
        // A signature of another attempt or commit.
        bytes memory other = _signText(0xA1, reg.commitMessage(ROUND, 1, o, c));
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postCommit(fact, 0, o, ROUND, c, other);
        // No draw (attempt 1), or a draw without a fixed beacon, takes no post.
        bytes memory good1 = _signText(0xA1, reg.commitMessage(ROUND, 1, o, c));
        vm.expectRevert(OnyxRegistry.NoDraw.selector);
        reg.postCommit(fact, 1, o, ROUND, c, good1);
        _commit(fact, 0xA1, c);
        // Once per (observer, round).
        bytes memory again = _signText(0xA1, reg.commitMessage(ROUND, 0, o, keccak256("c2")));
        vm.expectRevert(OnyxRegistry.AlreadyPosted.selector);
        reg.postCommit(fact, 0, o, ROUND, keccak256("c2"), again);
        (bytes32 lc, uint64 at, bytes memory ls) = reg.commitLogOf(fact, 0, o, ROUND);
        assertEq(lc, c);
        assertEq(at, 1_500);
        assertEq(ls, _signText(0xA1, reg.commitMessage(ROUND, 0, o, c)));
        (lc, at, ls) = reg.commitLogOf(fact, 0, o, keccak256("r2"));
        assertEq(lc, bytes32(0));
        assertEq(ls.length, 0);
        // A receipt signed by anyone but the draw's receipt key is refused.
        bytes memory bad = _signText(0xA1, reg.receiptMessage(ROUND, 0, o, c));
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postReceipt(fact, 0, o, ROUND, c, bad);
        vm.expectRevert(OnyxRegistry.NoDraw.selector);
        reg.postReceipt(fact, 1, o, ROUND, c, bad);
        bytes memory rs = _signText(RECEIPT_PK, reg.receiptMessage(ROUND, 0, o, c));
        reg.postReceipt(fact, 0, o, ROUND, c, rs);
        vm.expectRevert(OnyxRegistry.AlreadyPosted.selector);
        reg.postReceipt(fact, 0, o, ROUND, c, rs);
    }

    function test_AWithheldCommitOrReceiptedRevealIsCaught() public {
        bytes32 fact = _setup();
        bytes32 c1 = keccak256("c1");
        bytes32 c2 = keccak256("c2");
        address o1 = _commit(fact, 0xA1, c1);
        address o2 = _commit(fact, 0xA2, c2);
        address o3 = vm.addr(0xA3);
        _receipt(fact, o1, c1);
        bytes memory full = bytes.concat(
            _entry(o1, c1, true), _entry(o2, c2, false), _entry(o3, bytes32(0), false)
        );
        assertTrue(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, full));
        // o1's receipted reveal left out.
        bytes memory noReveal = bytes.concat(
            _entry(o1, c1, false), _entry(o2, c2, false), _entry(o3, bytes32(0), false)
        );
        assertFalse(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, noReveal));
        // o2's posted commit left out.
        bytes memory noCommit = bytes.concat(
            _entry(o1, c1, true), _entry(o2, bytes32(0), false), _entry(o3, bytes32(0), false)
        );
        assertFalse(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, noCommit));
        // Posted outside the round's commit phase: not the round's commit.
        assertTrue(reg.transcriptComplete(fact, 0, ROUND, 1_600, CLOSE, noCommit));
        assertTrue(reg.transcriptComplete(fact, 0, ROUND, OPEN, 1_499, noCommit));
        // Another round's slots are empty.
        assertTrue(reg.transcriptComplete(fact, 0, keccak256("r2"), OPEN, CLOSE, noReveal));
        // A malformed entry list, or an unannounced draw, is never complete.
        assertFalse(
            reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, bytes.concat(full, hex"00"))
        );
        bytes memory flag2 = bytes.concat(_entry(o1, c1, true));
        flag2[52] = 0x02;
        assertFalse(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, flag2));
        assertFalse(reg.transcriptComplete(fact, 1, ROUND, OPEN, CLOSE, full));
    }

    /// Review REVLOG HIGH-1 (was the reviewer's `test_PocObserverVeto`): a drawn observer posts
    /// an in-phase commit on chain and sends the coordinator another one, or nothing. A
    /// transcript that ignores the log is refused for good; the coordinator path absorbs the
    /// posted commit (`observe::absorb_posted_commits`) and serves it, the griefer counts as a
    /// non-reveal, and that transcript is complete. Receipts still bind.
    function test_AGrieferPostIsAbsorbedNotAVeto() public {
        bytes32 fact = _setup();
        bytes32 c1 = keccak256("c1");
        bytes32 posted = keccak256("posted only");
        address o1 = _commit(fact, 0xA1, c1);
        address o2 = _commit(fact, 0xA2, posted);
        _receipt(fact, o1, c1);
        // What a coordinator that never reads the log serves: refused (the PoC).
        bytes memory ignored =
            bytes.concat(_entry(o1, c1, true), _entry(o2, keccak256("sent to coordinator"), true));
        assertFalse(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, ignored));
        bytes memory silent = bytes.concat(_entry(o1, c1, true), _entry(o2, bytes32(0), false));
        assertFalse(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, silent));
        // The coordinator path: the posted commit absorbed, no reveal opens it.
        bytes memory absorbed = bytes.concat(_entry(o1, c1, true), _entry(o2, posted, false));
        assertTrue(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, absorbed));
        // A griefer that also reveals its posted commit is an ordinary observer.
        _receipt(fact, o2, posted);
        assertFalse(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, absorbed));
        assertTrue(
            reg.transcriptComplete(
                fact,
                0,
                ROUND,
                OPEN,
                CLOSE,
                bytes.concat(_entry(o1, c1, true), _entry(o2, posted, true))
            )
        );
        // A post after the coordinator read the log (chain time past CLOSE) never counts.
        vm.warp(CLOSE + 1);
        address o3 = _commit(fact, 0xA3, keccak256("late"));
        assertTrue(
            reg.transcriptComplete(
                fact,
                0,
                ROUND,
                OPEN,
                CLOSE,
                bytes.concat(
                    _entry(o1, c1, true), _entry(o2, posted, true), _entry(o3, bytes32(0), false)
                )
            )
        );
    }

    /// Review REVLOG LOW-2 (was the reviewer's `test_PocReceiptKeyBricksRound`): the receipt
    /// signer is bound to the coordinator that set it. After a coordinator change it reads zero,
    /// the new coordinator cannot announce until it sets its own key, and the former
    /// coordinator cannot set one. Residual (THREATS REVLOG-4): whoever holds a draw's
    /// snapshotted receipt key can still void THAT draw's rounds with a receipt for a commit
    /// nobody revealed.
    function test_ACoordinatorChangeDropsTheReceiptSigner() public {
        bytes32 fact = _setup();
        assertEq(reg.receiptSigner(), vm.addr(RECEIPT_PK));
        address next = address(0xC0FFEE);
        vm.mockCall(address(st), abi.encodeWithSignature("coordinator()"), abi.encode(next));
        assertEq(reg.receiptSigner(), address(0), "a former coordinator's key is dropped");
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        vm.prank(next);
        vm.expectRevert(OnyxRegistry.NoReceiptSigner.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, keccak256("c"), h);
        vm.expectRevert(OnyxRegistry.NotCoordinator.selector);
        reg.setReceiptSigner(address(0xBAD));
        vm.prank(next);
        reg.setReceiptSigner(address(0x5EC2));
        assertEq(reg.receiptSigner(), address(0x5EC2));
        vm.prank(next);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, keccak256("c"), h);
        assertEq(reg.drawReceiptSigner(fact, 1), address(0x5EC2));
        // Draw 0 keeps its snapshot: the residual, as the reviewer's PoC showed it.
        assertEq(reg.drawReceiptSigner(fact, 0), vm.addr(RECEIPT_PK));
        address o1 = vm.addr(0xA1);
        bytes memory honest = _entry(o1, keccak256("c1"), true);
        assertTrue(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, honest));
        _receipt(fact, o1, keccak256("never revealed"));
        assertFalse(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, honest));
    }

    /// Review REVLOG-R2 LOW-3 (was the reviewer's `test_PocABAResurrectsOldSigner`): coordinator
    /// A to B (B never sets a key) and back to A. The ledger bumps `coordinatorEpoch` on every
    /// change, so A's old key stays dropped: A cannot announce until it sets a key again.
    function test_AReturningCoordinatorDoesNotResurrectItsOldKey() public {
        _setup();
        address a = st.coordinator();
        address b = address(0xC0FFEE);
        vm.mockCall(address(st), abi.encodeWithSignature("coordinator()"), abi.encode(b));
        vm.mockCall(address(st), abi.encodeWithSignature("coordinatorEpoch()"), abi.encode(1));
        assertEq(reg.receiptSigner(), address(0));
        vm.mockCall(address(st), abi.encodeWithSignature("coordinator()"), abi.encode(a));
        vm.mockCall(address(st), abi.encodeWithSignature("coordinatorEpoch()"), abi.encode(2));
        assertEq(reg.receiptSigner(), address(0), "A's old key stays dropped");
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        vm.prank(a);
        vm.expectRevert(OnyxRegistry.NoReceiptSigner.selector);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, keccak256("c"), h);
        vm.prank(a);
        reg.setReceiptSigner(address(0x5EC3));
        assertEq(reg.receiptSigner(), address(0x5EC3));
        vm.prank(a);
        reg.announceDraw(FEED_METRIC, SUBJECT, WINDOW, 1, keccak256("c"), h);
    }

    /// Review REVLOG-R2 HIGH-1 (was the reviewer's `test_PocUnboundedLogSpam`): an observer
    /// spams posts under arbitrary round ids, which would fill a provider's `eth_getLogs` result
    /// cap. The coordinator reads its round's slot from state: `commitLogOf` returns the commit,
    /// its block time and the observer's own signature, whatever else was posted.
    function test_TheLogIsReadFromStateNotEvents() public {
        bytes32 fact = _setup();
        address o = vm.addr(0xA1);
        for (uint256 i = 0; i < 50; i++) {
            bytes32 r = keccak256(abi.encode(i));
            bytes32 junk = keccak256("junk");
            reg.postCommit(fact, 0, o, r, junk, _signText(0xA1, reg.commitMessage(r, 0, o, junk)));
        }
        bytes32 c = keccak256("c1");
        bytes memory sig = _signText(0xA1, reg.commitMessage(ROUND, 0, o, c));
        reg.postCommit(fact, 0, o, ROUND, c, sig);
        (bytes32 lc, uint64 at, bytes memory ls) = reg.commitLogOf(fact, 0, o, ROUND);
        assertEq(lc, c);
        assertEq(at, 1_500);
        assertEq(ls, sig, "the stored signature is the posted one");
        assertEq(OnyxSig.recover(OnyxSig.eip191Digest(reg.commitMessage(ROUND, 0, o, lc)), ls), o);
    }

    /// The commit phase's bound is inclusive (the reviewer's `test_PocPostAtCloseCounts`): a
    /// post landing at exactly `commitClose` must be served. Rust `absorb_posted_commit` uses
    /// the same bound, and the coordinator reads the log only once its confirmed block time is
    /// past `commitClose`.
    function test_APostAtCommitCloseCounts() public {
        bytes32 fact = _setup();
        vm.warp(CLOSE);
        address o = _commit(fact, 0xA1, keccak256("g"));
        assertFalse(
            reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, _entry(o, keccak256("h"), false))
        );
        assertTrue(
            reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, _entry(o, keccak256("g"), false))
        );
    }

    /// Gas of the posts and of the client's check (3 observers), for docs/ops/benchmarks.md.
    function test_GasOfTheLog() public {
        bytes32 fact = _setup();
        bytes32 c = keccak256("c1");
        address o = vm.addr(0xA1);
        bytes memory sig = _signText(0xA1, reg.commitMessage(ROUND, 0, o, c));
        uint256 g = gasleft();
        reg.postCommit(fact, 0, o, ROUND, c, sig);
        emit log_named_uint("postCommit gas", g - gasleft());
        bytes memory rs = _signText(RECEIPT_PK, reg.receiptMessage(ROUND, 0, o, c));
        g = gasleft();
        reg.postReceipt(fact, 0, o, ROUND, c, rs);
        emit log_named_uint("postReceipt gas", g - gasleft());
        bytes memory e = bytes.concat(
            _entry(o, c, true), _entry(address(2), c, true), _entry(address(3), c, true)
        );
        g = gasleft();
        assertTrue(reg.transcriptComplete(fact, 0, ROUND, OPEN, CLOSE, e));
        emit log_named_uint("transcriptComplete gas (3 observers)", g - gasleft());
    }

    /// The commit and receipt texts the contract rebuilds are the ones any key signs: a fresh
    /// key, round, attempt and commit post; any other commit is refused.
    function testFuzz_SignedTextsRoundTrip(uint256 pk, bytes32 round, bytes32 commit, bytes32 other)
        public
    {
        pk = bound(pk, 1, 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364140);
        vm.assume(round != bytes32(0) && commit != bytes32(0) && other != commit);
        vm.assume(other != bytes32(0));
        bytes32 fact = _setup();
        address o = vm.addr(pk);
        bytes memory sig = _signText(pk, reg.commitMessage(round, 0, o, commit));
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postCommit(fact, 0, o, round, other, sig);
        reg.postCommit(fact, 0, o, round, commit, sig);
        bytes memory rs = _signText(RECEIPT_PK, reg.receiptMessage(round, 0, o, commit));
        vm.expectRevert(OnyxRegistry.BadSignature.selector);
        reg.postReceipt(fact, 0, o, round, other, rs);
        reg.postReceipt(fact, 0, o, round, commit, rs);
        assertTrue(reg.transcriptComplete(fact, 0, round, OPEN, CLOSE, _entry(o, commit, true)));
        assertFalse(reg.transcriptComplete(fact, 0, round, OPEN, CLOSE, _entry(o, commit, false)));
        assertFalse(reg.transcriptComplete(fact, 0, round, OPEN, CLOSE, _entry(o, other, true)));
    }

    /// Rust <-> Solidity (fixtures/registry/transcript-log.json): texts, log key, Rust's
    /// signatures, and every `transcriptComplete` calldata answers as Rust's simulated log.
    function test_RustTranscriptLogVectors() public {
        string memory json = vm.readFile("../fixtures/registry/transcript-log.json");
        _reg();
        reg.setReceiptSigner(vm.parseJsonAddress(json, ".receiptSigner"));
        bytes32 fact = _draw(FEED_METRIC, SUBJECT, WINDOW, SPEC);
        assertEq(fact, vm.parseJsonBytes32(json, ".fact"), "Rust registry_fact_key");
        bytes32 round = vm.parseJsonBytes32(json, ".round");
        address[] memory obs = vm.parseJsonAddressArray(json, ".observers");
        bytes32[] memory commits = vm.parseJsonBytes32Array(json, ".commits");
        bytes[] memory sigs = vm.parseJsonBytesArray(json, ".commitSigs");
        assertEq(
            reg.commitMessage(round, 0, obs[0], commits[0]),
            vm.parseJsonBytes(json, ".commitMessage")
        );
        assertEq(
            reg.receiptMessage(round, 0, obs[0], commits[0]),
            vm.parseJsonBytes(json, ".receiptMessage")
        );
        assertEq(reg.logKey(fact, 0, obs[0], round), vm.parseJsonBytes32(json, ".logKey"));
        vm.warp(vm.parseJsonUint(json, ".postedAt"));
        reg.postCommit(fact, 0, obs[0], round, commits[0], sigs[0]);
        reg.postCommit(fact, 0, obs[1], round, commits[1], sigs[1]);
        reg.postReceipt(fact, 0, obs[0], round, commits[0], vm.parseJsonBytes(json, ".receiptSig"));
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
    }
}
