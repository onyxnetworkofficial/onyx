// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// Borsh `SignedPayload` encoder and EIP-191 signer for the signed-book tests (the layout is the
/// one fixtures/signed/vectors.json pins against Rust).
contract SignedBookFixture is Test {
    string internal constant SOURCE = "example.signed";
    string internal constant ORIGIN = "data.example.org";
    string internal constant METRIC = "example.signed.reserve";
    string internal constant SUBJECT = "entity:example";
    bytes32 internal constant PROV = keccak256("keys document");
    uint64 internal constant T0 = 1_700_000_000;

    function _le(uint256 v, uint256 n) internal pure returns (bytes memory out) {
        out = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = bytes1(uint8(v >> (8 * i)));
        }
    }

    function _str(string memory s) internal pure returns (bytes memory) {
        return abi.encodePacked(_le(bytes(s).length, 4), s);
    }

    /// `SignedPayload { source, key, metric, subject: Entity(SUBJECT), value: UInt(value),
    /// observed_unix, sequence }` in Borsh.
    function _payload(
        string memory source,
        bytes32 key,
        bytes memory subject,
        uint128 value,
        uint64 observed,
        uint64 sequence
    ) internal pure returns (bytes memory) {
        return abi.encodePacked(
            _str(source),
            key,
            _str(METRIC),
            subject,
            uint8(1),
            _le(value, 16),
            _le(observed, 8),
            _le(sequence, 8)
        );
    }

    function _entity(string memory s) internal pure returns (bytes memory) {
        return abi.encodePacked(uint8(0), _str(s));
    }

    function _sign(uint256 pk, bytes memory payload) internal pure returns (bytes memory) {
        bytes32 digest = keccak256(abi.encodePacked("onyx-signed-source-v1", payload));
        bytes memory message = abi.encodePacked("onyx-signed-source-v1:", OnyxSig.hex32(digest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, OnyxSig.eip191Digest(message));
        return abi.encodePacked(r, s, v);
    }

    function _digest(bytes memory payload) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-signed-source-v1", payload));
    }

    function _fact(uint64 observed) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-signed-fact-v1",
                _str(SOURCE),
                _str(METRIC),
                _entity(SUBJECT),
                _le(observed, 8)
            )
        );
    }
}

contract OnyxSignedBookTest is SignedBookFixture {
    OnyxSignedBook internal book;
    address internal constant REVIEWER = address(0xBEEF);
    uint256[3] internal pks = [uint256(0xA11CE), 0xB0B, 0xCA401];
    bytes32[3] internal ids;

    function setUp() public {
        vm.warp(T0);
        book = new OnyxSignedBook(REVIEWER);
        vm.startPrank(REVIEWER);
        for (uint256 k = 0; k < 3; k++) {
            ids[k] = book.registerKey(SOURCE, vm.addr(pks[k]), ORIGIN, PROV, "setup");
        }
        vm.stopPrank();
    }

    function _p(uint256 k, uint128 value, uint64 observed, uint64 seq)
        internal
        view
        returns (bytes memory)
    {
        return _payload(SOURCE, ids[k], _entity(SUBJECT), value, observed, seq);
    }

    function _post(uint256 k, uint128 value, uint64 observed, uint64 seq)
        internal
        returns (OnyxSignedBook.Outcome o, bytes32 fact)
    {
        bytes memory p = _p(k, value, observed, seq);
        return book.postReport(p, _sign(pks[k], p));
    }

    function _suspended(uint256 k) internal view returns (bool s) {
        (, s,,,,) = book.keyState(ids[k]);
    }

    function _state(bytes32 fact) internal view returns (OnyxSignedBook.FactState st) {
        (st,,,) = book.factView(fact);
    }

    // ------------------------------------------------------------ intake basics

    function test_AcceptsAReportAndSettlesOnlyAfterTheWindow() public {
        (OnyxSignedBook.Outcome o, bytes32 fact) = _post(0, 42, T0, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
        assertEq(fact, _fact(T0));
        (bool ok,,) = book.settleable(fact);
        assertFalse(ok, "inside the dispute window");
        vm.warp(T0 + 3_599);
        (ok,,) = book.settleable(fact);
        assertFalse(ok);
        vm.warp(T0 + 3_600);
        bytes32 vh;
        (ok,, vh) = book.settleable(fact);
        assertTrue(ok);
        assertEq(vh, keccak256(abi.encodePacked(uint8(1), _le(42, 16))));
    }

    function test_KeyIdIsTheRustKeyId() public view {
        // `SourceKey::id` = keccak256("onyx-source-key-v1" ‖ borsh(source, signer)).
        assertEq(
            ids[0], keccak256(abi.encodePacked("onyx-source-key-v1", _str(SOURCE), vm.addr(pks[0])))
        );
    }

    function test_SameValueAgainIsAlreadyStatedAndChangesNothing() public {
        _post(0, 42, T0, 1);
        vm.expectRevert(OnyxSignedBook.AlreadyStated.selector);
        _post(1, 42, T0, 1);
        assertFalse(_suspended(0));
        assertFalse(_suspended(1));
        // The refused report consumed nothing: key 1 may use sequence 1 for another fact.
        (OnyxSignedBook.Outcome o,) = _post(1, 7, T0 - 10, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
    }

    function test_ReservedSequenceIsRefused() public {
        vm.expectRevert(OnyxSignedBook.SequenceReserved.selector);
        _post(0, 1, T0, type(uint64).max);
    }

    function test_OnlyTheReviewerActs() public {
        vm.expectRevert(OnyxSignedBook.NotReviewer.selector);
        book.registerKey(SOURCE, address(0x1234), ORIGIN, PROV, "x");
        vm.expectRevert(OnyxSignedBook.NotReviewer.selector);
        book.retireKey(ids[0], T0, "x");
        vm.expectRevert(OnyxSignedBook.NotReviewer.selector);
        book.markCompromised(ids[0], T0, "x");
        vm.expectRevert(OnyxSignedBook.NotReviewer.selector);
        book.reinstateKey(ids[0], "x");
        vm.expectRevert(OnyxSignedBook.NotReviewer.selector);
        book.resolveFact(bytes32(uint256(1)), bytes32(0), "x");
        vm.expectRevert(OnyxSignedBook.NotReviewer.selector);
        new OnyxSignedBook(address(0));
    }

    /// ROADMAP C10 (THREATS C10-1): the account that deploys the book can never be its reviewer.
    function test_C10_1_TheDeployerIsNeverTheReviewer() public {
        address deployer = address(0xD0);
        vm.prank(deployer);
        vm.expectRevert(OnyxSignedBook.ReviewerIsDeployer.selector);
        new OnyxSignedBook(deployer);
        vm.prank(deployer);
        OnyxSignedBook ok = new OnyxSignedBook(REVIEWER);
        assertEq(ok.reviewer(), REVIEWER);
    }

    function test_RegistrationRules() public {
        vm.startPrank(REVIEWER);
        // One signer, one registration, under any source (L3R4-F1).
        vm.expectRevert(OnyxSignedBook.SignerTaken.selector);
        book.registerKey("example.other", vm.addr(pks[0]), "other.example.org", PROV, "x");
        vm.expectRevert(OnyxSignedBook.KeyAlreadyRegistered.selector);
        book.registerKey(SOURCE, vm.addr(pks[0]), ORIGIN, PROV, "x");
        // All keys of a source share an origin; an origin serves one source (L3R-F7).
        vm.expectRevert(OnyxSignedBook.OriginMismatch.selector);
        book.registerKey(SOURCE, address(0x1234), "elsewhere.example.org", PROV, "x");
        vm.expectRevert(OnyxSignedBook.OriginTaken.selector);
        book.registerKey("example.other", address(0x1234), ORIGIN, PROV, "x");
        vm.expectRevert(OnyxSignedBook.BadKey.selector);
        book.registerKey(SOURCE, address(0), ORIGIN, PROV, "x");
        vm.expectRevert(OnyxSignedBook.BadKey.selector);
        book.registerKey(SOURCE, address(0x1234), ORIGIN, bytes32(0), "x");
        book.registerKey("example.other", address(0x1234), "other.example.org", PROV, "ok");
        vm.stopPrank();
    }

    // ------------------------------------------------------------ L3-F7: one book for every coordinator

    /// Two coordinators (any two relayers) hold the two sides of an equivocation: the second
    /// meets the first in the one book, the fact is disputed and both keys are suspended.
    function test_L3_F7_AnotherCoordinatorMeetsTheOtherSideOfAnEquivocation() public {
        bytes memory a = _p(0, 42, T0, 1);
        bytes memory b = _p(1, 43, T0, 1);
        vm.prank(address(0xC0D1));
        book.postReport(a, _sign(pks[0], a));
        vm.prank(address(0xC0D2));
        (OnyxSignedBook.Outcome o, bytes32 fact) = book.postReport(b, _sign(pks[1], b));
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Equivocation));
        assertEq(uint256(_state(fact)), uint256(OnyxSignedBook.FactState.Disputed));
        assertTrue(_suspended(0), "first signer suspended too (L3R-F3)");
        assertTrue(_suspended(1));
        (bool ok,,) = book.settleable(fact);
        assertFalse(ok);
    }

    /// A report one coordinator recorded is a replay for every other.
    function test_L3_F7_AReplayThroughAnotherCoordinatorIsRefused() public {
        bytes memory a = _p(0, 42, T0, 1);
        bytes memory sig = _sign(pks[0], a);
        vm.prank(address(0xC0D1));
        book.postReport(a, sig);
        vm.prank(address(0xC0D2));
        vm.expectRevert(OnyxSignedBook.Replay.selector);
        book.postReport(a, sig);
    }

    /// One key, one sequence, two payloads, delivered to two coordinators.
    function test_L3_F7_SequenceEquivocationAcrossCoordinators() public {
        bytes memory a = _p(0, 42, T0, 5);
        bytes memory b = _p(0, 99, T0 - 100, 5);
        vm.prank(address(0xC0D1));
        (, bytes32 fa) = book.postReport(a, _sign(pks[0], a));
        vm.prank(address(0xC0D2));
        (OnyxSignedBook.Outcome o, bytes32 f) = book.postReport(b, _sign(pks[0], b));
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Equivocation));
        assertEq(f, fa, "the withdrawn report's fact is named");
        assertTrue(_suspended(0));
        assertEq(uint256(_state(fa)), uint256(OnyxSignedBook.FactState.Disputed));
        assertEq(book.sequenceOf(ids[0], 5), _digest(a), "the first payload keeps the sequence");
    }

    /// The chain never prunes: a sequence reused after the 7-day window is still an
    /// equivocation (the off-chain store, which prunes, could no longer see it).
    function test_L3_F7_ASequenceReusedAfterTheWindowIsStillAnEquivocation() public {
        _post(0, 42, T0, 5);
        vm.warp(T0 + 30 days);
        (OnyxSignedBook.Outcome o,) = _post(0, 1, T0 + 30 days, 5);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Equivocation));
        assertTrue(_suspended(0));
    }

    /// Griefing with junk: nobody but a registered key's holder writes anything.
    function test_L3_F7_OnlyARegisteredKeyWrites() public {
        uint256 stranger = 0xDEAD;
        // A stranger's key id under the source: not registered.
        bytes32 sid = book.keyId(SOURCE, vm.addr(stranger));
        bytes memory p = _payload(SOURCE, sid, _entity(SUBJECT), 1, T0, 1);
        vm.expectRevert(OnyxSignedBook.UnknownKey.selector);
        book.postReport(p, _sign(stranger, p));
        // A stranger signing for a registered key id.
        p = _p(0, 1, T0, 1);
        vm.expectRevert(OnyxSignedBook.BadSignature.selector);
        book.postReport(p, _sign(stranger, p));
        // A registered key's payload naming another source.
        p = _payload("example.other", ids[0], _entity(SUBJECT), 1, T0, 1);
        vm.expectRevert(OnyxSignedBook.KeySourceMismatch.selector);
        book.postReport(p, _sign(pks[0], p));
        // A malleated (high-s) signature.
        p = _p(0, 1, T0, 1);
        bytes memory sig = _sign(pks[0], p);
        bytes32 s;
        assembly {
            s := mload(add(sig, 64))
        }
        uint256 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
        bytes32 hs = bytes32(n - uint256(s));
        uint8 v = uint8(sig[64]) == 27 ? 28 : 27;
        bytes memory mal = abi.encodePacked(bytes32(0), hs, v);
        for (uint256 i = 0; i < 32; i++) {
            mal[i] = sig[i];
        }
        vm.expectRevert(OnyxSignedBook.BadSignature.selector);
        book.postReport(p, mal);
        assertEq(book.sequenceOf(ids[0], 1), bytes32(0), "nothing written");
    }

    // ------------------------------------------------------------ L3R-F8: block time

    /// Freshness is block time: no caller supplies `now`.
    function test_L3R_F8_FreshnessUsesBlockTime() public {
        vm.expectRevert(OnyxSignedBook.FromFuture.selector);
        _post(0, 1, T0 + 301, 1);
        (OnyxSignedBook.Outcome o,) = _post(0, 1, T0 + 300, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
        vm.warp(T0 + 7 days + 1);
        vm.expectRevert(OnyxSignedBook.Stale.selector);
        _post(1, 2, T0, 2);
        (o,) = _post(1, 2, T0 + 1, 2);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
    }

    /// Retirement grace and the retirement point are block time (L3R-F1, L3R2-F4).
    function test_L3R_F8_RetiredGraceUsesBlockTime() public {
        vm.prank(REVIEWER);
        vm.expectRevert(OnyxSignedBook.TimeInFuture.selector);
        book.retireKey(ids[0], T0 + 1, "x");
        vm.prank(REVIEWER);
        book.retireKey(ids[0], T0, "rotation");
        vm.prank(REVIEWER);
        vm.expectRevert(OnyxSignedBook.NotActive.selector);
        book.retireKey(ids[0], T0, "again");
        // Observed before the point, posted inside the grace: counts.
        (OnyxSignedBook.Outcome o,) = _post(0, 1, T0 - 10, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
        // Observed at the point: refused.
        vm.expectRevert(OnyxSignedBook.KeyRetiredError.selector);
        _post(0, 1, T0, 2);
        // Backdated more than the grace: refused.
        vm.expectRevert(OnyxSignedBook.KeyRetiredError.selector);
        _post(0, 1, T0 - 3_601, 3);
        // After the grace, nothing.
        vm.warp(T0 + 3_600);
        vm.expectRevert(OnyxSignedBook.KeyRetiredError.selector);
        _post(0, 2, T0 - 20, 4);
    }

    /// A compromise point cannot lie in the future and only moves earlier (L3R2-F2).
    function test_L3R_F8_CompromisePointIsBoundedByBlockTime() public {
        vm.startPrank(REVIEWER);
        vm.expectRevert(OnyxSignedBook.TimeInFuture.selector);
        book.markCompromised(ids[0], T0 + 1, "x");
        book.markCompromised(ids[0], T0 - 100, "theft");
        book.markCompromised(ids[0], T0, "later point ignored");
        (,,,,, uint64 since) = book.keyState(ids[0]);
        assertEq(since, T0 - 100);
        book.markCompromised(ids[0], T0 - 200, "earlier");
        (,,,,, since) = book.keyState(ids[0]);
        assertEq(since, T0 - 200);
        vm.stopPrank();
        vm.expectRevert(OnyxSignedBook.KeyCompromisedError.selector);
        _post(0, 1, T0 - 300, 1);
    }

    // ------------------------------------------------------------ withdrawals read lazily

    /// L3R-F4 on chain: after a theft is reported, the new key restates the truth.
    function test_ACompromiseWithdrawsAndANewKeyRestates() public {
        (, bytes32 fact) = _post(0, 666, T0, 1);
        vm.warp(T0 + 10);
        vm.prank(REVIEWER);
        book.markCompromised(ids[0], T0 - 1, "theft");
        assertEq(uint256(_state(fact)), uint256(OnyxSignedBook.FactState.None), "withdrawn");
        (OnyxSignedBook.Outcome o,) = _post(1, 42, T0, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
        assertFalse(_suspended(1), "the honest key is not suspended");
        vm.warp(T0 + 10 + 3_600);
        (bool ok,,) = book.settleable(fact);
        assertTrue(ok);
    }

    /// L3R5-F1 on chain: retiring withdraws an accepted report observed at or after the point.
    function test_RetirementWithdrawsALateReport() public {
        (, bytes32 fact) = _post(0, 1, T0 + 200, 1);
        vm.warp(T0 + 100);
        vm.prank(REVIEWER);
        book.retireKey(ids[0], T0, "rotation");
        assertEq(uint256(_state(fact)), uint256(OnyxSignedBook.FactState.None));
        (OnyxSignedBook.Outcome o,) = _post(1, 2, T0 + 200, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
    }

    /// A dispute stays until the reviewer resolves it, and holds one report per key.
    function test_DisputeNeedsTheReviewerAndHoldsOneReportPerKey() public {
        _post(0, 1, T0, 1);
        (, bytes32 fact) = _post(1, 2, T0, 1);
        // Key 2 joins with key 0's value, which differs from key 1's: suspended too.
        (OnyxSignedBook.Outcome o,) = _post(2, 1, T0, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Equivocation));
        assertTrue(_suspended(2), "a contradicting joiner is suspended (L3R3-F2)");
        (,,, bytes32[] memory list) = book.factView(fact);
        assertEq(list.length, 3);
        // Resolution keeps only a report whose key may sign now.
        vm.startPrank(REVIEWER);
        vm.expectRevert(OnyxSignedBook.BadResolution.selector);
        book.resolveFact(fact, _digest(_p(0, 1, T0, 1)), "suspended key");
        book.reinstateKey(ids[0], "reviewed");
        vm.expectRevert(OnyxSignedBook.NotSuspended.selector);
        book.reinstateKey(ids[0], "again");
        vm.expectRevert(OnyxSignedBook.BadResolution.selector);
        book.resolveFact(fact, keccak256("not in the dispute"), "x");
        book.resolveFact(fact, _digest(_p(0, 1, T0, 1)), "key 0 was right");
        vm.expectRevert(OnyxSignedBook.NotDisputed.selector);
        book.resolveFact(fact, bytes32(0), "again");
        vm.stopPrank();
        assertEq(uint256(_state(fact)), uint256(OnyxSignedBook.FactState.Accepted));
        vm.warp(T0 + 3_600);
        (bool ok,,) = book.settleable(fact);
        assertTrue(ok, "the window runs from the resolution");
    }

    function test_ADisputeWithAllReportsCompromisedStaysDisputed() public {
        _post(0, 1, T0, 1);
        (, bytes32 fact) = _post(1, 2, T0, 1);
        vm.startPrank(REVIEWER);
        book.markCompromised(ids[0], 0, "theft");
        book.markCompromised(ids[1], 0, "theft");
        vm.stopPrank();
        (OnyxSignedBook.FactState st,,, bytes32[] memory list) = book.factView(fact);
        assertEq(uint256(st), uint256(OnyxSignedBook.FactState.Disputed));
        assertEq(list.length, 0);
        (bool ok,,) = book.settleable(fact);
        assertFalse(ok, "no value until the reviewer resolves it");
        // The source's new key cannot settle it either: it joins the dispute as evidence.
        vm.prank(REVIEWER);
        bytes32 k3 = book.registerKey(SOURCE, vm.addr(0xD00D), ORIGIN, PROV, "new key");
        bytes memory p = _payload(SOURCE, k3, _entity(SUBJECT), 3, T0, 1);
        (OnyxSignedBook.Outcome o,) = book.postReport(p, _sign(0xD00D, p));
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.FactDisputed));
        (ok,,) = book.settleable(fact);
        assertFalse(ok);
    }

    // ------------------------------------------------------------ review C2-R1

    /// C2-R1 finding 1 (L3C-1): any validly signed payload burns its (key, sequence) on chain,
    /// even one the off-chain intake refuses (here a value kind the metric does not use). A source
    /// that re-signs a corrected payload under the same sequence equivocates.
    function test_L3C_1_ARefusedOffChainPayloadStillBurnsItsSequence() public {
        bytes memory wrongKind = abi.encodePacked(
            _str(SOURCE),
            ids[0],
            _str(METRIC),
            _entity(SUBJECT),
            uint8(0),
            _le(42, 16),
            _le(T0, 8),
            _le(1, 8)
        );
        book.postReport(wrongKind, _sign(pks[0], wrongKind));
        assertEq(book.sequenceOf(ids[0], 1), _digest(wrongKind));
        (OnyxSignedBook.Outcome o,) = _post(0, 42, T0, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Equivocation));
        assertTrue(_suspended(0));
    }

    /// C2-R1 M16: a retired key's window includes its lower edge `at - grace`.
    function test_L3C_RetiredWindowIncludesItsLowerEdge() public {
        vm.prank(REVIEWER);
        book.retireKey(ids[0], T0, "rotation");
        (OnyxSignedBook.Outcome o,) = _post(0, 1, T0 - 3_600, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
        vm.expectRevert(OnyxSignedBook.KeyRetiredError.selector);
        _post(0, 2, T0 - 3_601, 2);
    }

    /// C2-R1 M4: a report withdrawn by retirement stays withdrawn after a later compromise whose
    /// point does not cover it.
    function test_L3C_RetiredWithdrawalSurvivesALaterCompromise() public {
        (, bytes32 fact) = _post(0, 1, T0, 1);
        vm.prank(REVIEWER);
        book.retireKey(ids[0], T0 - 100, "rotate back");
        assertEq(uint256(_state(fact)), uint256(OnyxSignedBook.FactState.None));
        vm.warp(T0 + 7200);
        vm.prank(REVIEWER);
        book.markCompromised(ids[0], T0 + 7200, "later theft");
        assertEq(uint256(_state(fact)), uint256(OnyxSignedBook.FactState.None), "stays withdrawn");
        (bool ok,,) = book.settleable(fact);
        assertFalse(ok);
    }

    /// C2-R1 M7: the second payload of a sequence equivocation adds nothing to a dispute its key
    /// is already in (Rust `contradict`).
    function test_L3C_SequenceEquivocationIntoItsOwnDisputeAddsNothing() public {
        (, bytes32 f) = _post(0, 1, T0, 1);
        _post(0, 9, T0 - 50, 1);
        (OnyxSignedBook.FactState st,,, bytes32[] memory list) = book.factView(f);
        assertEq(uint256(st), uint256(OnyxSignedBook.FactState.Disputed));
        assertEq(list.length, 1);
        (OnyxSignedBook.Outcome o,) = _post(1, 1, T0, 1);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.FactDisputed));
        _post(1, 5, T0, 1);
        (,,, list) = book.factView(f);
        assertEq(list.length, 2, "own member: the second payload is not added");
    }

    /// C2-R1 M10: a resolution keeps only a report of this fact's dispute.
    function test_L3C_ResolveRefusesAReportOfAnotherFact() public {
        (, bytes32 f) = _post(0, 1, T0, 1);
        _post(1, 2, T0, 1);
        _post(2, 7, T0 - 10, 1);
        vm.prank(REVIEWER);
        vm.expectRevert(OnyxSignedBook.BadResolution.selector);
        book.resolveFact(f, _digest(_p(2, 7, T0 - 10, 1)), "wrong fact");
    }

    /// C2-R1 M12: after a resolution the dispute window runs from the resolution.
    function test_L3C_TheWindowRunsFromTheResolution() public {
        (, bytes32 f) = _post(0, 1, T0, 1);
        _post(1, 2, T0, 1);
        vm.warp(T0 + 7200);
        vm.startPrank(REVIEWER);
        book.reinstateKey(ids[0], "ok");
        book.resolveFact(f, _digest(_p(0, 1, T0, 1)), "key 0 right");
        vm.stopPrank();
        (bool ok,,) = book.settleable(f);
        assertFalse(ok, "a full window after the resolution");
        vm.warp(T0 + 7200 + 3600);
        (ok,,) = book.settleable(f);
        assertTrue(ok);
    }

    // ------------------------------------------------------------ review C2-R2

    /// Only the canonical 65-byte form counts (L3C-8): v outside {27, 28}, a 64-byte (EIP-2098)
    /// signature, a zero r or s and an empty signature recover nobody, and nothing is written.
    function test_L3C_8_NonCanonicalSignatureFormsAreRefused() public {
        bytes memory p = _p(0, 1, T0, 1);
        bytes memory sig = _sign(pks[0], p);
        bytes32 r;
        bytes32 s;
        assembly {
            r := mload(add(sig, 32))
            s := mload(add(sig, 64))
        }
        uint8 v = uint8(sig[64]);
        bytes[6] memory bad = [
            abi.encodePacked(r, s, v - 27),
            abi.encodePacked(r, s, v + 2),
            abi.encodePacked(r, s),
            abi.encodePacked(bytes32(0), s, v),
            abi.encodePacked(r, bytes32(0), v),
            bytes("")
        ];
        for (uint256 i = 0; i < bad.length; i++) {
            vm.expectRevert(OnyxSignedBook.BadSignature.selector);
            book.postReport(p, bad[i]);
        }
        assertEq(book.sequenceOf(ids[0], 1), bytes32(0), "nothing written");
        (OnyxSignedBook.Outcome o,) = book.postReport(p, sig);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted), "the canonical form");
    }

    /// A signed report is the source's statement, not an authorization: it names no chain and no
    /// book, so it is valid in every deployment that registered its key (L3C-11). The rule that
    /// keeps this harmless is one sequence space per key across every deployment and the
    /// off-chain store (spec R-SIG-ON-1): a key registered in two books shares its sequences.
    function test_L3C_11_ASignedReportIsNotBoundToADeployment() public {
        bytes memory p = _p(0, 7, T0, 1);
        bytes memory sig = _sign(pks[0], p);
        (OnyxSignedBook.Outcome o,) = book.postReport(p, sig);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));

        vm.chainId(84_532);
        OnyxSignedBook other = new OnyxSignedBook(REVIEWER);
        vm.prank(REVIEWER);
        other.registerKey(SOURCE, vm.addr(pks[0]), ORIGIN, PROV, "same key, other book");
        (o,) = other.postReport(p, sig);
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted), "replays across books");
        // Reusing the sequence for another payload meant for "the other book" is an
        // equivocation in every book that sees both.
        bytes memory q = _p(0, 8, T0 + 1, 1);
        (o,) = other.postReport(q, _sign(pks[0], q));
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Equivocation));
        (o,) = book.postReport(q, _sign(pks[0], q));
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Equivocation));
    }

    // ------------------------------------------------------------ gas

    function test_GasAcceptAReport() public {
        bytes memory p = _p(0, 42, T0, 1);
        bytes memory sig = _sign(pks[0], p);
        uint256 g = gasleft();
        book.postReport(p, sig);
        g -= gasleft();
        // About 9 new slots plus parsing, hashing and ecrecover; pinned by .gas-snapshot.
        assertLt(g, 300_000);
    }

    // ------------------------------------------------------------ fuzz

    /// Any single-byte change to a signed payload is refused: by the layout, the key, the
    /// signature or the replay guard; nothing is recorded under the original sequence twice.
    function testFuzz_AMutatedPayloadIsNeverAccepted(uint256 at, uint8 x) public {
        bytes memory p = _p(0, 42, T0, 1);
        bytes memory sig = _sign(pks[0], p);
        at = bound(at, 0, p.length - 1);
        vm.assume(uint8(p[at]) != x);
        p[at] = bytes1(x);
        try book.postReport(p, sig) {
            fail();
        } catch {}
        assertEq(book.sequenceOf(ids[0], 1), bytes32(0));
    }

    /// Values of every size up to the bound parse; one byte more or less does not.
    function testFuzz_BytesValuesParseToTheirExactLength(uint16 len, uint64 seq) public {
        len = uint16(bound(len, 0, 1024));
        seq = uint64(bound(seq, 0, type(uint64).max - 1));
        bytes memory value = abi.encodePacked(uint8(4), _le(len, 4), new bytes(len));
        bytes memory p = abi.encodePacked(
            _str(SOURCE), ids[0], _str(METRIC), _entity(SUBJECT), value, _le(T0, 8), _le(seq, 8)
        );
        (OnyxSignedBook.Outcome o,) = book.postReport(p, _sign(pks[0], p));
        assertEq(uint256(o), uint256(OnyxSignedBook.Outcome.Accepted));
        assertEq(book.report(_digest(p)).valueHash, keccak256(value));
    }
}
