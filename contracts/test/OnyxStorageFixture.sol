// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";
import {OnyxSig} from "../src/OnyxSig.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {OnyxStorage, IStorageToken} from "../src/OnyxStorage.sol";

/// A small anchored dataset for the storage tests: two shards of two rows each, a synthetic
/// Borsh-shaped manifest header (format 1, dataset and publisher ids, filler where the schema
/// and rights sit, the 84-byte tail) whose Merkle trees are built here, and its anchor in a real
/// `OnyxSignedBook`. Five storage nodes with bonds; a publisher with tONYX.
contract StorageFixture is Test {
    string internal constant PUBLISHER = "synthetic.publisher";
    string internal constant DATASET = "synthetic.storage";
    uint64 internal constant T0 = 1_700_000_000;
    uint64 internal constant EPOCH = 10;
    uint128 internal constant PRICE = 1e15;
    uint128 internal constant BOND_PER_GIB = 1e15;
    uint128 internal constant MIN_BOND = 1e18;
    uint32 internal constant SLASH_PPM = 100_000;
    uint32 internal constant K = 2;
    uint32 internal constant MAX_MISSES = 3;
    uint64 internal constant BYTES = (3 << 30) + 5; // 4 GiB once rounded up

    OnyxTestToken public tok;
    OnyxSignedBook internal book;
    OnyxStorage public st;
    address internal reviewer = address(0xBEEF);
    address internal treasury = address(0x7EA5);
    address internal publisher = address(0xB0B);
    uint256 internal signerKey = 0x5157;
    address[] internal ops;
    bytes32[] internal ids;

    bytes[2][2] internal rows;
    bytes[2] internal entries;
    bytes internal header;
    bytes internal payload;

    function _le(uint256 v, uint256 n) internal pure returns (bytes memory out) {
        out = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = bytes1(uint8(v >> (8 * i)));
        }
    }

    function _str(bytes memory s) internal pure returns (bytes memory) {
        return abi.encodePacked(_le(s.length, 4), s);
    }

    function _leaf(bytes memory b) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(bytes1(0x00), b));
    }

    function _root2(bytes memory a, bytes memory b) internal pure returns (bytes32) {
        bytes32 top = keccak256(abi.encodePacked(bytes1(0x01), _leaf(a), _leaf(b)));
        return keccak256(abi.encodePacked(bytes1(0x02), uint64(2), top));
    }

    function _entry(uint32 index, bytes32 root) internal pure returns (bytes memory) {
        return abi.encodePacked(
            _le(index, 4),
            root,
            _le(2, 8),
            _le(BYTES / 2, 8),
            _le(index * 2, 16),
            _le(index * 2 + 1, 16)
        );
    }

    function _buildDataset() internal {
        for (uint256 s = 0; s < 2; s++) {
            for (uint256 i = 0; i < 2; i++) {
                rows[s][i] = abi.encodePacked("synthetic row ", vm.toString(s), "/", vm.toString(i));
            }
            entries[s] = _entry(uint32(s), _root2(rows[s][0], rows[s][1]));
        }
        bytes32 shardsRoot = _root2(entries[0], entries[1]);
        header = abi.encodePacked(
            uint8(1),
            _str(bytes(DATASET)),
            _str(bytes(PUBLISHER)),
            hex"0100000000000000000000000000000000000000000000000000000000000000000000000000",
            _le(2, 4),
            _le(4, 8),
            _le(BYTES, 8),
            _le(0, 16),
            _le(3, 16),
            shardsRoot
        );
    }

    function _manifestRoot() internal view returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-bulk-manifest-v1", header));
    }

    function _payloadFor(bytes32 root, uint64 seq) internal view returns (bytes memory) {
        bytes32 key = book.keyId(PUBLISHER, vm.addr(signerKey));
        return abi.encodePacked(
            _str(bytes(PUBLISHER)),
            key,
            _str("bulk.manifest"),
            uint8(3),
            _str(bytes(DATASET)),
            uint8(4),
            _le(32, 4),
            root,
            _le(T0, 8),
            _le(seq, 8)
        );
    }

    function _sign(bytes memory p) internal view returns (bytes memory) {
        bytes32 digest = keccak256(abi.encodePacked("onyx-signed-source-v1", p));
        bytes memory message = abi.encodePacked("onyx-signed-source-v1:", OnyxSig.hex32(digest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKey, OnyxSig.eip191Digest(message));
        return abi.encodePacked(r, s, v);
    }

    function _deploy() internal {
        vm.warp(T0 + 60);
        vm.roll(1_000);
        tok = new OnyxTestToken(address(this));
        book = new OnyxSignedBook(reviewer);
        vm.prank(reviewer);
        book.registerKey(
            PUBLISHER, vm.addr(signerKey), "publisher.example.org", keccak256("doc"), "test"
        );
        st = new OnyxStorage(
            IStorageToken(address(tok)),
            book,
            treasury,
            EPOCH,
            PRICE,
            BOND_PER_GIB,
            MIN_BOND,
            SLASH_PPM,
            K,
            MAX_MISSES
        );
        _buildDataset();
        payload = _payloadFor(_manifestRoot(), 42);
        book.postReport(payload, _sign(payload));
        tok.mint(publisher, 1e24);
        vm.prank(publisher);
        tok.approve(address(st), type(uint256).max);
    }

    function _addNode(uint256 i, uint64 capGiB) internal returns (bytes32 id) {
        address op = address(uint160(0xA000 + i));
        tok.mint(op, 1e22);
        vm.startPrank(op);
        tok.approve(address(st), type(uint256).max);
        uint64 idx = st.register(capGiB, "http://127.0.0.1:1/");
        vm.roll(vm.getBlockNumber() + 1);
        id = st.activate(idx);
        vm.stopPrank();
        ops.push(op);
        ids.push(id);
    }

    /// Roll to the start of epoch `e` plus `extra` blocks.
    function _toEpoch(uint64 e, uint256 extra) internal {
        vm.roll(st.epochStart(e) + extra);
    }

    function _seed(uint64 e) internal returns (bytes32) {
        _toEpoch(e, 1);
        return st.recordSeed(e);
    }

    function _opOf(bytes32 id) internal view returns (address) {
        (address op,,,,,,,) = st.nodeInfo(id);
        return op;
    }

    function _bond(bytes32 id) internal view returns (uint256 b) {
        (,,, b,,,,) = st.nodeInfo(id);
    }

    /// The node list of epoch `e - 1`, rebuilt from `activeNodes()` (only valid while the list
    /// did not change since: the tests that change it pass the list they captured).
    function _list() internal view returns (bytes32[] memory) {
        return st.activeNodes();
    }

    function _one(bytes32 h) internal pure returns (bytes32[] memory a) {
        a = new bytes32[](1);
        a[0] = h;
    }

    /// The honest answer of slot `slot` for deal `d` in an epoch with seed `s`.
    function _proof(bytes32 s, uint64 d, uint32 slot)
        internal
        view
        returns (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs)
    {
        uint256 shard =
            uint256(keccak256(abi.encodePacked("onyx-storage-slot-v1", s, d, slot))) % 2;
        uint256 r = uint256(keccak256(abi.encodePacked("onyx-storage-row-v1", s, d, slot))) % 2;
        entry = entries[shard];
        es = _one(_leaf(entries[1 - shard]));
        row = rows[shard][r];
        rs = _one(_leaf(rows[shard][1 - r]));
    }

    function _answer(bytes32 id, uint64 d, uint64 e, uint32 slot, bytes32[] memory list)
        internal
        returns (bool)
    {
        (bytes memory entry, bytes32[] memory es, bytes memory row, bytes32[] memory rs) =
            _proof(st.seedOf(e), d, slot);
        vm.prank(_opOf(id));
        return st.answer(d, e, slot, id, list, entry, es, row, rs);
    }
}
