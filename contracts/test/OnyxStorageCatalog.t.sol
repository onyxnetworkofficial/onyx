// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {StorageFixture} from "./OnyxStorageFixture.sol";
import {OnyxStorage} from "../src/OnyxStorage.sol";
import {OnyxStorageLib} from "../src/OnyxStorageLib.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";

/// What the catalog tests share: the vectors of fixtures/storage/vectors.json, which
/// `crates/onyx-storage` writes with the real encoders of `onyx-bulk`, and a real
/// `OnyxSignedBook` that knows the vectors' test key.
abstract contract CatalogFixture is StorageFixture {
    /// The test key of the vectors (`[7; 32]` in `catalog_vectors.rs`; never a real key). Only
    /// the tests that need a second report from the same key sign with it here.
    uint256 internal constant VECTOR_KEY =
        0x0707070707070707070707070707070707070707070707070707070707070707;

    string internal json;
    bytes32 internal vectorKeyId;

    struct Case {
        bytes header;
        bytes catalog;
        uint64 leafIndex;
        bytes32[] siblings;
        bytes payload;
    }

    /// Five nodes, the vector key registered for the publisher, and catalog 0 anchored. A test
    /// that needs catalog 1 anchors it itself (`_post(1)`).
    function _setUpCatalogs() internal {
        _deploy();
        for (uint256 i = 0; i < 5; i++) {
            _addNode(i, 100);
        }
        json = vm.readFile("../fixtures/storage/vectors.json");
        assertEq(vm.parseJsonString(json, ".catalog.publisher"), PUBLISHER);
        address signer = vm.parseJsonAddress(json, ".catalog.signer");
        assertEq(signer, vm.addr(VECTOR_KEY));
        vm.prank(reviewer);
        vectorKeyId =
            book.registerKey(PUBLISHER, signer, "publisher.example.org", keccak256("doc"), "test");
        _post(0);
    }

    // ---------------------------------------------------------------- helpers

    function _cat(uint256 i, string memory f) internal pure returns (string memory) {
        return string.concat(".catalog.catalogs[", vm.toString(i), "]", f);
    }

    /// Post catalog `i`'s signed report (payload and signature as the publisher made them).
    function _post(uint256 i) internal returns (OnyxSignedBook.Outcome o, bytes32 fact) {
        return book.postReport(
            vm.parseJsonBytes(json, _cat(i, ".payload")),
            vm.parseJsonBytes(json, _cat(i, ".signature"))
        );
    }

    function _case(string memory p) internal view returns (Case memory c) {
        c.header = vm.parseJsonBytes(json, string.concat(p, ".header"));
        c.catalog = vm.parseJsonBytes(json, string.concat(p, ".catalog"));
        c.leafIndex = uint64(vm.parseJsonUint(json, string.concat(p, ".leafIndex")));
        c.siblings = vm.parseJsonBytes32Array(json, string.concat(p, ".siblings"));
        c.payload = vm.parseJsonBytes(json, string.concat(p, ".payload"));
    }

    function _dealPath(uint256 i) internal pure returns (string memory) {
        return string.concat(".catalog.deals[", vm.toString(i), "]");
    }

    function _open(Case memory c) internal returns (uint64) {
        vm.prank(publisher);
        return st.createDealInCatalog(
            c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 3, 4, "http://127.0.0.1:2/"
        );
    }

    function _refused(Case memory c, bytes4 err) internal {
        vm.prank(publisher);
        vm.expectRevert(err);
        st.createDealInCatalog(c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 3, 4, "");
    }

    /// A signed payload of the vector key for the publisher: any metric, subject, value, time
    /// and sequence.
    function _payloadOf(
        bytes memory metric,
        bytes memory subject,
        bytes32 value,
        uint64 time,
        uint64 seq
    ) internal view returns (bytes memory) {
        return abi.encodePacked(
            _str(bytes(PUBLISHER)),
            vectorKeyId,
            _str(metric),
            uint8(3),
            _str(subject),
            uint8(4),
            _le(32, 4),
            value,
            _le(time, 8),
            _le(seq, 8)
        );
    }

    function _postWithVectorKey(bytes memory p) internal returns (bytes32 digest) {
        book.postReport(p, _signWith(VECTOR_KEY, p));
        return keccak256(abi.encodePacked("onyx-signed-source-v1", p));
    }

    /// 16 gas for a byte that is not zero, 4 for a zero byte.
    function _calldataGas(bytes memory data) internal pure returns (uint256 gas) {
        for (uint256 i = 0; i < data.length; i++) {
            gas += data[i] == 0 ? 4 : 16;
        }
    }
}

/// ADR-0067: `createDealInCatalog`, a deal on a manifest that a catalog anchors. The headers,
/// catalogs, paths, payloads and signatures are the vectors' (the real encoders' bytes). Catalog
/// 0 has 5 leaves, catalog 1 has 9,000 (a path of 14 levels).
contract OnyxStorageCatalogTest is CatalogFixture {
    function setUp() public {
        _setUpCatalogs();
    }

    // ---------------------------------------------------------------- a deal opens

    function test_aCatalogDealOpensAndTakesTheDeposit() public {
        string memory p = _dealPath(0);
        Case memory c = _case(p);
        uint256 b = tok.balanceOf(publisher);
        vm.expectEmit(true, false, false, true, address(st));
        emit OnyxStorage.DealAnchor(0, vm.parseJsonBytes32(json, string.concat(p, ".digest")), true);
        uint64 d = _open(c);
        // 5 shards billed at 8 MiB each: 1 GiB. 1 GiB x 3 replicas x 4 epochs x price.
        assertEq(b - tok.balanceOf(publisher), uint256(PRICE) * 1 * 3 * 4);
        (
            address payer,
            bytes32 mr,
            bytes32 sr,,
            uint32 sc,
            uint8 r,
            uint64 bc,
            uint64 se,
            uint64 ee,
            uint256 dep,,
        ) = st.dealInfo(d);
        assertEq(payer, publisher);
        assertEq(mr, vm.parseJsonBytes32(json, string.concat(p, ".manifestRoot")));
        assertEq(sr, vm.parseJsonBytes32(json, string.concat(p, ".shardsRoot")));
        assertEq(sc, vm.parseJsonUint(json, string.concat(p, ".shardCount")));
        assertEq(bc, vm.parseJsonUint(json, string.concat(p, ".byteCount")));
        assertEq(r, 3);
        assertEq(se, st.currentEpoch() + 2);
        assertEq(ee, se + 4);
        assertEq(dep, uint256(PRICE) * 12);
        assertEq(st.committedGiB(), 3);
        assertTrue(st.conserved());
    }

    /// Every good case of the vectors opens a deal on its manifest root: a small catalog, a
    /// leaf whose path has a promoted level, a path of 14 levels, and a format-2 header at
    /// version 2.
    function test_everyVectorDealOpens() public {
        assertEq(vm.parseJsonUint(json, ".catalog.dealCount"), 5);
        for (uint256 i = 0; i < 5; i++) {
            // Deals 0 and 1 go through catalog 0, deals 2 and 3 through catalog 1, deal 4
            // through the catalog of one leaf: each catalog is anchored when its turn comes,
            // because a deal through a catalog that has a successor is refused.
            if (i == 2) _post(1);
            if (i == 4) _post(2);
            string memory p = _dealPath(i);
            uint64 d = _open(_case(p));
            assertEq(d, i);
            (, bytes32 mr,,,,,,,,,,) = st.dealInfo(d);
            assertEq(mr, vm.parseJsonBytes32(json, string.concat(p, ".manifestRoot")));
        }
        assertEq(vm.parseJsonUint(json, string.concat(_dealPath(1), ".leafIndex")), 4);
        assertEq(
            vm.parseJsonBytes32Array(json, string.concat(_dealPath(2), ".siblings")).length, 14
        );
        assertEq(vm.parseJsonBytes32Array(json, string.concat(_dealPath(4), ".siblings")).length, 0);
        assertTrue(st.conserved());
    }

    /// The whole calldata as `onyx_storage::abi::create_deal_in_catalog` writes it (the vectors'
    /// `call`): the contract decodes it and opens the first deal.
    function test_theToolsCalldataOpensTheDeal() public {
        bytes memory data = vm.parseJsonBytes(json, ".catalog.call.calldata");
        Case memory c = _case(_dealPath(vm.parseJsonUint(json, ".catalog.call.deal")));
        assertEq(
            data,
            abi.encodeCall(
                st.createDealInCatalog,
                (
                    c.header,
                    c.catalog,
                    c.leafIndex,
                    c.siblings,
                    c.payload,
                    uint8(vm.parseJsonUint(json, ".catalog.call.r")),
                    uint32(vm.parseJsonUint(json, ".catalog.call.epochs")),
                    vm.parseJsonString(json, ".catalog.call.sourceUrl")
                )
            )
        );
        vm.prank(publisher);
        (bool ok, bytes memory ret) = address(st).call(data);
        assertTrue(ok);
        uint64 id = abi.decode(ret, (uint64));
        assertEq(id, 0);
        (, bytes32 mr,,,, uint8 r,, uint64 start, uint64 end,,,) = st.dealInfo(id);
        assertEq(mr, vm.parseJsonBytes32(json, string.concat(_dealPath(0), ".manifestRoot")));
        assertEq(r, 3);
        assertEq(end - start, 4);
    }

    // ---------------------------------------------------------------- not through an old catalog

    /// Once the book holds the publisher's next catalog (same key), a deal through the catalog
    /// before it is refused, also for a dataset the new catalog still lists unchanged; through
    /// the new catalog it opens.
    function test_aDealThroughAReplacedCatalogIsRefused() public {
        Case memory old = _case(_dealPath(0));
        Case memory fresh = _case(_dealPath(2));
        // Catalog 1 is not in the book yet: catalog 0 is the newest.
        assertEq(_open(_case(_dealPath(1))), 0);
        _post(1);
        _refused(old, OnyxStorage.CatalogSuperseded.selector);
        assertEq(_open(fresh), 1);
        // The dataset catalog 1 removed, and the version it replaced, through catalog 0.
        assertEq(st.dealCount(), 2);
        // Catalog 2 lands: catalog 1 is old now, catalog 0 stays refused (its next is 1).
        _post(2);
        _refused(fresh, OnyxStorage.CatalogSuperseded.selector);
        _refused(old, OnyxStorage.CatalogSuperseded.selector);
        assertEq(_open(_case(_dealPath(4))), 2);
        assertTrue(st.conserved());
    }

    /// The rule for "the next catalog exists" is the rule for the anchor itself: its report
    /// holds (recorded, its fact accepted on exactly that report, the key registered and not
    /// suspended). A next catalog whose fact is disputed, or that the reviewer decided against,
    /// does not count: nodes do not take such a catalog either, so they still serve the one
    /// before it.
    function test_aNextCatalogThatDoesNotHoldDoesNotCount() public {
        uint64 t1 = uint64(vm.parseJsonUint(json, _cat(1, ".createdUnix")));
        // Another report for catalog 1's fact comes first: catalog 1's own report is then an
        // equivocation, recorded under its sequence but not accepted.
        bytes32 otherDigest = _postWithVectorKey(
            _payloadOf("bulk.catalog", "catalog", keccak256("another catalog"), t1, 77)
        );
        (OnyxSignedBook.Outcome o, bytes32 fact) = _post(1);
        assertEq(uint8(o), uint8(OnyxSignedBook.Outcome.Equivocation));
        assertEq(
            book.sequenceOf(vectorKeyId, uint64(vm.parseJsonUint(json, _cat(1, ".sequence")))),
            vm.parseJsonBytes32(json, _cat(1, ".digest"))
        );
        Case memory c = _case(_dealPath(0));
        // The key is suspended while the fact is disputed: no deal at all.
        _refused(c, OnyxStorage.KeySuspended.selector);
        // The reviewer keeps the other report: catalog 1 is withdrawn for good, and a deal
        // through catalog 0 opens.
        vm.startPrank(reviewer);
        book.reinstateKey(vectorKeyId, "test");
        book.resolveFact(fact, otherDigest, "test");
        vm.stopPrank();
        assertEq(_open(c), 0);
    }

    /// A catalog with the largest number there is has no next catalog: the contract asks the
    /// book about the reserved sequence, finds nothing, and the deal opens (no overflow).
    function test_theLargestCatalogNumberHasNoNext() public {
        Case memory c = _case(_dealPath(0));
        uint256 o = c.catalog.length - 84;
        for (uint256 i = 0; i < 8; i++) {
            c.catalog[o + i] = 0xff;
        }
        assertEq(c.catalog, vm.parseJsonBytes(json, ".catalog.lastCatalog.header"));
        // Made one second later than catalog 0, so that its report is a fact of its own.
        uint64 made = uint64(vm.parseJsonUint(json, _cat(0, ".createdUnix"))) + 1;
        bytes memory madeLe = _le(made, 8);
        for (uint256 i = 0; i < 8; i++) {
            c.catalog[o + 40 + i] = madeLe[i];
        }
        bytes memory number = new bytes(8);
        for (uint256 i = 0; i < 8; i++) {
            number[i] = 0xff;
        }
        uint64 seq =
            uint64(bytes8(keccak256(abi.encodePacked("onyx-bulk-catalog-sequence-v1", number))));
        if (seq == type(uint64).max) seq--;
        c.payload = _payloadOf(
            "bulk.catalog",
            "catalog",
            keccak256(abi.encodePacked("onyx-bulk-catalog-v1", c.catalog)),
            made,
            seq
        );
        _refused(c, OnyxStorage.NotAnchored.selector);
        _postWithVectorKey(c.payload);
        assertEq(_open(c), 0);
    }

    function test_aFormatTwoHeaderOpensACatalogDeal() public {
        _post(1);
        string memory p = _dealPath(3);
        Case memory c = _case(p);
        assertEq(uint8(c.header[0]), 2);
        assertEq(vm.parseJsonUint(json, string.concat(p, ".version")), 2);
        uint64 d = _open(c);
        (, bytes32 mr,,, uint32 sc,,,,,,,) = st.dealInfo(d);
        assertEq(mr, vm.parseJsonBytes32(json, string.concat(p, ".manifestRoot")));
        assertEq(sc, 3);
        // Format 3 is no header.
        c.header[0] = 0x03;
        _refused(c, OnyxStorageLib.BadHeader.selector);
    }

    /// `createDeal` reads a format-2 header too: the fixture's header with format byte 2 and
    /// its own report.
    function test_aFormatTwoHeaderOpensAnOwnReportDeal() public {
        header[0] = 0x02;
        // Its report has a later time: for the book it is another fact than the format-1 one.
        bytes memory p = _payloadAt(_manifestRoot(), T0 + 1, 43);
        book.postReport(p, _sign(p));
        vm.prank(publisher);
        uint64 d = st.createDeal(header, p, 3, 4, "");
        (, bytes32 mr,,,,,,,,,,) = st.dealInfo(d);
        assertEq(mr, _manifestRoot());
        header[0] = 0x03;
        vm.prank(publisher);
        vm.expectRevert(OnyxStorageLib.BadHeader.selector);
        st.createDeal(header, p, 3, 4, "");
    }

    /// `createDeal` works as before and names its own report in `DealAnchor`.
    function test_anOwnReportDealNamesItsReport() public {
        vm.expectEmit(true, false, false, true, address(st));
        emit OnyxStorage.DealAnchor(
            0, keccak256(abi.encodePacked("onyx-signed-source-v1", payload)), false
        );
        vm.prank(publisher);
        st.createDeal(header, payload, 3, 4, "");
    }

    // ---------------------------------------------------------------- checks 1 to 5

    /// Every refused case of the vectors, with the error the Rust mirror names for it: a wrong
    /// index, leaf count or sibling, a path from another catalog, a tombstone leaf, a replaced
    /// version, another publisher, a catalog header of the wrong length or format, a manifest
    /// header of format 0 or 3, and a payload with another metric, subject, value, time,
    /// sequence or source.
    function test_everyRefusedVectorIsRefused() public {
        // Both catalogs are anchored: each refusal below comes from the checks, not the book.
        _post(1);
        uint256 n;
        uint256[4] memory seen;
        while (vm.keyExistsJson(json, string.concat(".catalog.refused[", vm.toString(n), "]"))) {
            string memory p = string.concat(".catalog.refused[", vm.toString(n), "]");
            bytes32 kind = keccak256(bytes(vm.parseJsonString(json, string.concat(p, ".error"))));
            bytes4 err;
            if (kind == keccak256("header")) {
                err = OnyxStorageLib.BadHeader.selector;
                seen[0]++;
            } else if (kind == keccak256("catalog")) {
                err = OnyxStorageLib.BadCatalog.selector;
                seen[1]++;
            } else if (kind == keccak256("leaf")) {
                err = OnyxStorageLib.NotInCatalog.selector;
                seen[2]++;
            } else if (kind == keccak256("payload")) {
                err = OnyxStorageLib.BadPayload.selector;
                seen[3]++;
            } else {
                fail();
            }
            _refused(_case(p), err);
            n++;
        }
        assertEq(n, vm.parseJsonUint(json, ".catalog.refusedCount"));
        assertEq(n, 37);
        for (uint256 i = 0; i < 4; i++) {
            assertGt(seen[i], 0);
        }
        assertEq(st.dealCount(), 0);
    }

    /// The refusals the fixed list cannot hold: any one bit of the path, any other index.
    /// forge-config: default.fuzz.runs = 256
    function testFuzz_aChangedPathOrIndexIsRefused(uint256 at, uint8 bit, uint64 index) public {
        _post(1);
        Case memory c = _case(_dealPath(2));
        Case memory x = _case(_dealPath(2));
        at = bound(at, 0, c.siblings.length - 1);
        x.siblings[at] = c.siblings[at] ^ bytes32(uint256(1) << bit);
        _refused(x, OnyxStorageLib.NotInCatalog.selector);
        vm.assume(index != c.leafIndex);
        c.leafIndex = index;
        _refused(c, OnyxStorageLib.NotInCatalog.selector);
    }

    /// Any one changed byte of the catalog header or of the payload is refused (the key, which
    /// the book checks, is the one part of the payload the contract does not fix).
    /// forge-config: default.fuzz.runs = 256
    function testFuzz_aChangedCatalogOrPayloadByteIsRefused(uint256 at, uint8 flip) public {
        vm.assume(flip != 0);
        Case memory c = _case(_dealPath(0));
        uint256 i = bound(at, 0, c.catalog.length - 1);
        c.catalog[i] = c.catalog[i] ^ bytes1(flip);
        vm.prank(publisher);
        vm.expectRevert();
        st.createDealInCatalog(c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 3, 4, "");
        c = _case(_dealPath(0));
        i = bound(at, 0, c.payload.length - 1);
        c.payload[i] = c.payload[i] ^ bytes1(flip);
        uint256 keyStart = 4 + bytes(PUBLISHER).length;
        if (i >= keyStart && i < keyStart + 32) {
            // Another key: the payload still reads, and the book never recorded it.
            _refused(c, OnyxStorage.NotAnchored.selector);
        } else {
            _refused(c, OnyxStorageLib.BadPayload.selector);
        }
    }

    // ---------------------------------------------------------------- check 6: the book

    function test_refusesACatalogTheBookDidNotRecord() public {
        Case memory c = _case(_dealPath(2));
        _refused(c, OnyxStorage.NotAnchored.selector);
        // Once the publisher's report is in the book the same call opens the deal.
        _post(1);
        assertEq(_open(c), 0);
    }

    /// The catalog's report is recorded, but the book does not accept its fact on it: first
    /// while the fact is disputed, then when the reviewer keeps the other report.
    function test_refusesWhenTheFactIsNotAcceptedOnThisReport() public {
        uint64 t1 = uint64(vm.parseJsonUint(json, _cat(1, ".createdUnix")));
        // Another report for the same fact (catalog, same time) with another value comes first.
        bytes32 otherDigest = _postWithVectorKey(
            _payloadOf("bulk.catalog", "catalog", keccak256("another catalog"), t1, 77)
        );
        (OnyxSignedBook.Outcome o, bytes32 fact) = _post(1);
        assertEq(uint8(o), uint8(OnyxSignedBook.Outcome.Equivocation));
        Case memory c = _case(_dealPath(2));
        _refused(c, OnyxStorage.NotAnchored.selector);
        // The reviewer keeps the other report and lifts the suspension: the fact is accepted,
        // but not on the catalog's report.
        vm.startPrank(reviewer);
        book.reinstateKey(vectorKeyId, "test");
        book.resolveFact(fact, otherDigest, "test");
        vm.stopPrank();
        (OnyxSignedBook.FactState st_,,,) = book.factView(fact);
        assertEq(uint8(st_), uint8(OnyxSignedBook.FactState.Accepted));
        _refused(c, OnyxStorage.NotAnchored.selector);
        // Catalog 0 is another fact: its deals still open.
        assertEq(_open(_case(_dealPath(0))), 0);
    }

    /// The key is suspended (it signed two payloads under one sequence, on another metric):
    /// the catalog's fact stays accepted, and the deal is refused until the reviewer reinstates
    /// the key.
    function test_refusesASuspendedKey() public {
        _postWithVectorKey(_payloadOf("synthetic.metric", "x", keccak256("a"), T0, 5));
        _postWithVectorKey(_payloadOf("synthetic.metric", "y", keccak256("b"), T0, 5));
        (, bool suspended,,,,) = book.keyState(vectorKeyId);
        assertTrue(suspended);
        Case memory c = _case(_dealPath(0));
        _refused(c, OnyxStorage.KeySuspended.selector);
        vm.prank(reviewer);
        book.reinstateKey(vectorKeyId, "test");
        assertEq(_open(c), 0);
    }

    function test_refusesACompromisedKeysCatalog() public {
        vm.prank(reviewer);
        book.markCompromised(vectorKeyId, T0 - 1, "test");
        _refused(_case(_dealPath(0)), OnyxStorage.NotAnchored.selector);
    }

    // ---------------------------------------------------------------- check 7: the deal

    function test_catalogDealRefusesBadParamsAndTakesNothingWithoutApproval() public {
        Case memory c = _case(_dealPath(0));
        vm.startPrank(publisher);
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.createDealInCatalog(c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 0, 4, "");
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.createDealInCatalog(c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 9, 4, "");
        vm.expectRevert(OnyxStorage.BadParams.selector);
        st.createDealInCatalog(c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 3, 0, "");
        vm.expectRevert(OnyxStorage.NotEnoughNodes.selector);
        st.createDealInCatalog(c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 6, 4, "");
        vm.stopPrank();
        address poor = address(0xD00D);
        vm.prank(poor);
        vm.expectRevert();
        st.createDealInCatalog(c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 3, 4, "");
        assertEq(st.dealCount(), 0);
        assertEq(st.committedGiB(), 0);
        assertTrue(st.conserved());
    }

    /// After creation a catalog deal is a deal like any other: its holders are challenged, an
    /// honest answer is paid, and the close refunds what was not paid. This one uses the
    /// fixture's dataset (its rows are built here) under the fixture's two-leaf catalog.
    function test_aCatalogDealIsChallengedPaidAndClosedLikeAnyDeal() public {
        _anchorCatalog();
        vm.prank(publisher);
        uint64 d = st.createDealInCatalog(
            header, catalogHeader, 0, _one(catalogSibling), catalogPayload, 3, 4, ""
        );
        (, bytes32 mr,,,,,, uint64 se, uint64 ee,,,) = st.dealInfo(d);
        assertEq(mr, _manifestRoot());
        // The wrong position of the same catalog is refused.
        vm.prank(publisher);
        vm.expectRevert(OnyxStorageLib.NotInCatalog.selector);
        st.createDealInCatalog(
            header, catalogHeader, 1, _one(catalogSibling), catalogPayload, 3, 4, ""
        );
        bytes32[] memory list = _list();
        _seed(se);
        (, bytes32[] memory hs) = st.challengedHolders(d, se, 0, list);
        assertEq(hs.length, 3);
        uint256 before = st.earned(_opOf(hs[0]));
        assertTrue(_answer(hs[0], d, se, 0, list));
        assertGt(st.earned(_opOf(hs[0])), before);
        _toEpoch(ee, 0);
        uint256 b = tok.balanceOf(publisher);
        st.closeDeal(d);
        assertGt(tok.balanceOf(publisher), b);
        assertTrue(st.conserved());
    }
}

/// The gas of a deal, for ADR-0067. Both reports are in the book before the test starts, so
/// the measured call reads them cold, as a transaction does.
contract OnyxStorageCatalogGasTest is CatalogFixture {
    function setUp() public {
        _setUpCatalogs();
        _post(1);
        // The own report of the manifest that the catalogs list (the vectors' `manifest`).
        _postWithVectorKey(vm.parseJsonBytes(json, ".manifest.payload"));
    }

    /// The gas of one `createDealInCatalog` with a path of 14 levels (a catalog of 9,000
    /// leaves), for ADR-0067. Execution gas is measured around the call; the transaction adds
    /// 21,000 and its calldata.
    function test_gasOfACatalogDealWith14Levels() public {
        Case memory c = _case(_dealPath(2));
        assertEq(c.siblings.length, 14);
        bytes memory data = abi.encodeCall(
            st.createDealInCatalog,
            (c.header, c.catalog, c.leafIndex, c.siblings, c.payload, 3, 4, "http://127.0.0.1:2/")
        );
        vm.prank(publisher);
        uint256 g = gasleft();
        (bool ok,) = address(st).call(data);
        g -= gasleft();
        assertTrue(ok);
        emit log_named_uint("createDealInCatalog, 14 levels: execution gas", g);
        emit log_named_uint("createDealInCatalog, 14 levels: calldata bytes", data.length);
        emit log_named_uint(
            "createDealInCatalog, 14 levels: transaction gas (21,000 + calldata + execution)",
            21_000 + _calldataGas(data) + g
        );
        assertLt(g, 400_000);
    }

    /// The same manifest with its own report (`createDeal`), to compare: the difference is
    /// what the catalog path costs.
    function test_gasOfAnOwnReportDealOnTheSameManifest() public {
        bytes memory data = abi.encodeCall(
            st.createDeal,
            (
                vm.parseJsonBytes(json, ".manifest.header"),
                vm.parseJsonBytes(json, ".manifest.payload"),
                3,
                4,
                "http://127.0.0.1:2/"
            )
        );
        vm.prank(publisher);
        uint256 g = gasleft();
        (bool ok,) = address(st).call(data);
        g -= gasleft();
        assertTrue(ok);
        emit log_named_uint("createDeal, same manifest: execution gas", g);
        emit log_named_uint("createDeal, same manifest: calldata bytes", data.length);
        emit log_named_uint(
            "createDeal, same manifest: transaction gas (21,000 + calldata + execution)",
            21_000 + _calldataGas(data) + g
        );
    }

    /// The same measure for `createDeal` on the fixture's small header.
    function test_gasOfAnOwnReportDeal() public {
        bytes memory data =
            abi.encodeCall(st.createDeal, (header, payload, 3, 4, "http://127.0.0.1:2/"));
        vm.prank(publisher);
        uint256 g = gasleft();
        (bool ok,) = address(st).call(data);
        g -= gasleft();
        assertTrue(ok);
        emit log_named_uint("createDeal: execution gas", g);
        emit log_named_uint("createDeal: calldata bytes", data.length);
        emit log_named_uint(
            "createDeal: transaction gas (21,000 + calldata + execution)",
            21_000 + _calldataGas(data) + g
        );
    }
}
