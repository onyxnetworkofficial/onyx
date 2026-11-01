// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxStorageLib as L} from "../src/OnyxStorageLib.sol";
import {OnyxStorage, IStorageToken} from "../src/OnyxStorage.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";

/// Calldata wrappers around the library (its parsers read calldata).
contract LibHarness {
    function billableGiB(uint64 b, uint32 n) external pure returns (uint64) {
        return L.billableGiB(b, n);
    }

    function readHeader(bytes calldata h) external pure returns (L.Header memory) {
        return L.readHeader(h);
    }

    function checkPayload(bytes calldata h, bytes calldata p) external pure returns (bytes32) {
        return L.checkPayload(h, L.readHeader(h), p);
    }

    function verifyProof(
        bytes32 shardsRoot,
        uint32 shardCount,
        bytes32 s,
        uint64 dealId,
        uint32 slot,
        bytes calldata entry,
        bytes32[] calldata es,
        bytes calldata row,
        bytes32[] calldata rs
    ) external pure returns (bool) {
        return L.verifyProof(shardsRoot, shardCount, s, dealId, slot, entry, es, row, rs);
    }
}

/// The contract's challenged-holder rule on a deal with a chosen salt, with exclusions set
/// directly (test only).
contract PlacementHarness is OnyxStorage {
    constructor(IStorageToken t, OnyxSignedBook b)
        OnyxStorage(t, b, address(1), 10, 1, 1, 1, 1, 1, 1)
    {}

    function addDeal(bytes32 salt, uint8 r, uint32 shardCount) external returns (uint64 id) {
        id = uint64(_deals.length);
        Deal storage d = _deals.push();
        d.salt = salt;
        d.r = r;
        d.shardCount = shardCount;
    }

    function exclude(uint64 dealId, uint32 index, bytes32 node, uint64 from) external {
        _exclFrom[_exKey(dealId, index, node)] = from;
    }

    function challengedAt(uint64 dealId, uint64 e, uint32 index, bytes32[] calldata nodes)
        external
        view
        returns (bytes32[] memory h)
    {
        (bytes32[] memory out, uint256 c) = _challenged(_deals[dealId], dealId, e, index, nodes);
        h = new bytes32[](c);
        for (uint256 i = 0; i < c; i++) {
            h[i] = out[i];
        }
    }
}

/// fixtures/storage/vectors.json, written by `crates/onyx-storage` (ADR-0060): every hash the
/// contract recomputes, the header and payload reads, a storage proof, the placement and the
/// money amounts.
contract OnyxStorageVectorsTest is Test {
    string internal json;
    LibHarness internal lib;

    function setUp() public {
        json = vm.readFile("../fixtures/storage/vectors.json");
        lib = new LibHarness();
    }

    function _b(string memory k) internal view returns (bytes32) {
        return vm.parseJsonBytes32(json, k);
    }

    function _u(string memory k) internal view returns (uint256) {
        return vm.parseJsonUint(json, k);
    }

    function _k(string memory p, string memory f) internal pure returns (string memory) {
        return string.concat(p, f);
    }

    function _nodeId(uint256 i) internal view {
        string memory p = string.concat(".nodeIds[", vm.toString(i), "]");
        address op = vm.parseJsonAddress(json, _k(p, ".operator"));
        uint64 idx = uint64(_u(_k(p, ".index")));
        assertEq(L.nodeId(op, idx, _b(_k(p, ".blockHash"))), _b(_k(p, ".id")));
    }

    function _shardKey(uint256 i) internal view {
        string memory p = string.concat(".shardKeys[", vm.toString(i), "]");
        bytes32 k = L.shardKey(_b(_k(p, ".salt")), uint32(_u(_k(p, ".index"))));
        assertEq(k, _b(_k(p, ".key")));
        assertEq(bytes32(L.weight(k, _b(_k(p, ".node")))), _b(_k(p, ".weight")));
    }

    function _seed(uint256 i) internal view {
        string memory p = string.concat(".seeds[", vm.toString(i), "]");
        bytes32 s = L.seed(_b(_k(p, ".blockHash")), uint64(_u(_k(p, ".epoch"))));
        assertEq(s, _b(_k(p, ".seed")));
        uint64 d = uint64(_u(_k(p, ".dealId")));
        uint32 slot = uint32(_u(_k(p, ".slot")));
        uint32 sc = uint32(_u(_k(p, ".shardCount")));
        assertEq(L.slotShard(s, d, slot, sc), _u(_k(p, ".shard")));
        uint64 rows = uint64(_u(_k(p, ".rows")));
        assertEq(L.slotRow(s, d, slot, rows), _u(_k(p, ".row")));
    }

    function test_ids() public view {
        for (uint256 i = 0; i < 3; i++) {
            _nodeId(i);
            _seed(i);
        }
        for (uint256 i = 0; i < 4; i++) {
            _shardKey(i);
        }
        assertEq(
            L.dealSalt(_b(".dealSalt.prevHash"), uint64(_u(".dealSalt.dealId"))),
            _b(".dealSalt.salt")
        );
        assertEq(L.setHash(vm.parseJsonBytes32Array(json, ".nodes")), _b(".setHash"));
    }

    function test_headerAndPayload() public view {
        bytes memory h = vm.parseJsonBytes(json, ".manifest.header");
        L.Header memory r = lib.readHeader(h);
        assertEq(r.root, _b(".manifest.root"));
        assertEq(r.shardCount, _u(".manifest.shardCount"));
        assertEq(r.rowCount, _u(".manifest.rowCount"));
        assertEq(r.byteCount, _u(".manifest.byteCount"));
        assertEq(r.shardsRoot, _b(".manifest.shardsRoot"));
        bytes memory p = vm.parseJsonBytes(json, ".manifest.payload");
        assertEq(lib.checkPayload(h, p), _b(".manifest.digest"));
    }

    function test_headerAndPayloadRefusals() public {
        bytes memory h = vm.parseJsonBytes(json, ".manifest.header");
        bytes memory p = vm.parseJsonBytes(json, ".manifest.payload");
        bytes memory p2 = bytes.concat(p);
        p2[p2.length - 20] = bytes1(uint8(p2[p2.length - 20]) ^ 1); // inside the root
        vm.expectRevert(L.BadPayload.selector);
        lib.checkPayload(h, p2);
        bytes memory h2 = bytes.concat(h);
        h2[0] = 0x02;
        vm.expectRevert(L.BadHeader.selector);
        lib.readHeader(h2);
        // Review STORAGE-R1-7: more shards than a manifest may hold (65,537, little-endian).
        bytes memory h3 = bytes.concat(h);
        uint256 t = h3.length - 84;
        h3[t] = 0x01;
        h3[t + 1] = 0x00;
        h3[t + 2] = 0x01;
        h3[t + 3] = 0x00;
        vm.expectRevert(L.BadHeader.selector);
        lib.readHeader(h3);
        // Exactly the cap still reads with 8 bytes per shard (524,288 = 0x080000, little-endian).
        h3[t] = 0x00;
        h3[t + 12] = 0x00;
        h3[t + 13] = 0x00;
        h3[t + 14] = 0x08;
        for (uint256 i = 15; i < 20; i++) {
            h3[t + i] = 0x00;
        }
        assertEq(lib.readHeader(h3).shardCount, 65_536);
        // Review STORAGE-R2-5: fewer than 8 bytes per shard is no real manifest.
        bytes memory h4 = bytes.concat(h);
        uint256 n = lib.readHeader(h).shardCount;
        uint64 thin = uint64(8 * n - 1);
        for (uint256 i = 0; i < 8; i++) {
            h4[t + 12 + i] = bytes1(uint8(thin >> (8 * i)));
        }
        vm.expectRevert(L.BadHeader.selector);
        lib.readHeader(h4);
        thin += 1;
        h4[t + 12] = bytes1(uint8(thin));
        assertEq(lib.readHeader(h4).byteCount, thin);
        // A shard is billed as at least 8 MiB (onyx_storage::ledger::billable_gib).
        assertEq(lib.billableGiB(100_000, 1), 1);
        assertEq(lib.billableGiB(65_536 * 8, 65_536), 512);
        assertEq(lib.billableGiB(3 << 30, 128), 3);
        assertEq(lib.billableGiB(3 << 30, 387), 4);
    }

    struct P {
        bytes entry;
        bytes32[] es;
        bytes row;
        bytes32[] rs;
        bytes32 sr;
        bytes32 s;
        uint64 d;
        uint32 slot;
        uint32 sc;
    }

    function _p() internal view returns (P memory p) {
        p.entry = vm.parseJsonBytes(json, ".proof.entry");
        p.es = vm.parseJsonBytes32Array(json, ".proof.entrySiblings");
        p.row = vm.parseJsonBytes(json, ".proof.row");
        p.rs = vm.parseJsonBytes32Array(json, ".proof.rowSiblings");
        p.sr = _b(".proof.shardsRoot");
        p.s = _b(".proof.seed");
        p.d = uint64(_u(".proof.dealId"));
        p.slot = uint32(_u(".proof.slot"));
        p.sc = uint32(_u(".proof.shardCount"));
    }

    function _verify(P memory p) internal view returns (bool) {
        return lib.verifyProof(p.sr, p.sc, p.s, p.d, p.slot, p.entry, p.es, p.row, p.rs);
    }

    function test_proof() public view {
        P memory p = _p();
        assertTrue(_verify(p));
        // Another seed (another slot pick), a changed row byte: refused.
        p.s = bytes32(uint256(p.s) ^ 1);
        assertFalse(_verify(p));
        p = _p();
        p.row[p.row.length - 1] = bytes1(uint8(p.row[p.row.length - 1]) ^ 1);
        assertFalse(_verify(p));
    }

    function test_placement() public {
        OnyxTestToken t = new OnyxTestToken(address(this));
        OnyxSignedBook b = new OnyxSignedBook(address(0xBEEF));
        PlacementHarness ph = new PlacementHarness(IStorageToken(address(t)), b);
        bytes32[] memory nodes = vm.parseJsonBytes32Array(json, ".nodes");
        uint64 d = ph.addDeal(_b(".holders.salt"), uint8(_u(".holders.r")), 1000);
        // Rust `holders_at(epoch 9)` = the contract's challenged set at epoch 10 with no exclusion.
        for (uint256 i = 0; i < 6; i++) {
            string memory p = string.concat(".holders.cases[", vm.toString(i), "]");
            bytes32[] memory want = vm.parseJsonBytes32Array(json, string.concat(p, ".holders"));
            bytes32[] memory got =
                ph.challengedAt(d, 10, uint32(_u(string.concat(p, ".index"))), nodes);
            assertEq(got, want);
        }
        // Excluded from epoch 10: Rust `holders_at(epoch 10)` = the contract's set at epoch 11.
        ph.exclude(d, 3, _b(".holders.excluded.node"), uint64(_u(".holders.excluded.from")));
        assertEq(
            ph.challengedAt(d, 11, 3, nodes),
            vm.parseJsonBytes32Array(json, ".holders.excluded.after")
        );
        // A whole-deal decline (`WHOLE_DEAL`), as Rust's `Exclusions::from` reads it.
        ph.exclude(
            d, ph.WHOLE_DEAL(), _b(".holders.declined.node"), uint64(_u(".holders.declined.from"))
        );
        assertEq(
            ph.challengedAt(d, 11, 4, nodes),
            vm.parseJsonBytes32Array(json, ".holders.declined.after")
        );
    }

    function _amount(uint256 i, uint256 price, uint256 k) internal view {
        string memory p = string.concat(".amounts[", vm.toString(i), "]");
        uint256 r = _u(_k(p, ".r"));
        uint256 gib = (_u(_k(p, ".bytes")) + (1 << 30) - 1) / (1 << 30);
        assertEq(price * gib * r * _u(_k(p, ".epochs")), _u(_k(p, ".deposit")));
        assertEq(price * gib * r / (k * r), _u(_k(p, ".slotPay")));
    }

    function test_amounts() public view {
        uint256 price = _u(".params.pricePerGiBEpoch");
        uint256 k = _u(".params.challengesPerDeal");
        for (uint256 i = 0; i < 3; i++) {
            _amount(i, price, k);
        }
    }
}
