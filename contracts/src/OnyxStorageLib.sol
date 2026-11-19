// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxMerkle} from "./OnyxMerkle.sol";

/// @title OnyxStorageLib
/// @notice The hashes and parsers of the storage network (ADR-0060, docs/spec/storage-network.md),
/// byte for byte the twins of `crates/onyx-storage` (`ids.rs`, `deal.rs`, `challenge.rs`);
/// fixtures/storage/vectors.json is checked by both sides. Every hash is Keccak-256 over the
/// fixed packing of ADR-0011 (an ASCII tag, then fixed-width big-endian fields), except the
/// ADR-0059 hashes read as they are: the manifest root over the Borsh header, the Borsh shard
/// entry leaf (fixed-width, little-endian), and the catalog root and catalog leaf row of
/// ADR-0067.
library OnyxStorageLib {
    /// Bytes of the fixed tail of a Borsh `ManifestHeader`: shard_count u32, row_count u64,
    /// byte_count u64, key_min i128, key_max i128, shards_root [32].
    uint256 internal constant TAIL_BYTES = 84;
    /// Bytes of a Borsh `ShardEntry`: index u32, root [32], rows u64, bytes u64, two i128.
    uint256 internal constant ENTRY_BYTES = 84;
    /// `onyx_bulk::manifest::MAX_SHARDS`.
    uint32 internal constant MAX_SHARDS = 65_536;
    /// `onyx_storage::ledger::MIN_BILLED_SHARD_BYTES`: a shard is billed as at least 8 MiB.
    uint64 internal constant MIN_BILLED_SHARD_BYTES = 8 << 20;

    /// `onyx_storage::ledger::billable_gib`: GiB a deal pays for, the larger of its bytes and
    /// 8 MiB per shard, rounded up (a node's work grows with shards, not only bytes; review
    /// STORAGE-R2-5).
    function billableGiB(uint64 byteCount, uint32 shardCount) internal pure returns (uint64) {
        uint256 b = byteCount;
        uint256 floor = uint256(shardCount) * MIN_BILLED_SHARD_BYTES;
        if (floor > b) b = floor;
        return uint64((b + (1 << 30) - 1) >> 30);
    }
    /// The anchor's metric (`onyx_bulk::ANCHOR_METRIC`).
    bytes internal constant METRIC = "bulk.manifest";

    /// The catalog anchor's metric and subject (`onyx_bulk::catalog::CATALOG_METRIC`,
    /// `CATALOG_SUBJECT`), each with its Borsh length in front, the subject after its kind byte
    /// (3: `Subject::Aggregate`).
    bytes internal constant CATALOG_NAMES =
        hex"0c00000062756c6b2e636174616c6f670307000000636174616c6f67";
    /// Bytes of a Borsh `CatalogHeader` after its publisher: number u64, prev [32],
    /// created_unix u64, leaf_count u32, leaves_root [32].
    uint256 internal constant CATALOG_FIXED = 84;

    error BadHeader();
    error BadPayload();
    error BadCatalog();
    error NotInCatalog();

    struct Header {
        bytes32 root;
        uint32 shardCount;
        uint64 rowCount;
        uint64 byteCount;
        bytes32 shardsRoot;
        /// [start, end) of the dataset and publisher ids inside the header.
        uint256 dStart;
        uint256 dEnd;
        uint256 pStart;
        uint256 pEnd;
    }

    function nodeId(address operator, uint64 index, bytes32 regBlockHash)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked("onyx-storage-node-v1", operator, index, regBlockHash));
    }

    function dealSalt(bytes32 prevBlockHash, uint64 dealId) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-storage-salt-v1", prevBlockHash, dealId));
    }

    function shardKey(bytes32 salt, uint32 index) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-storage-shard-v1", salt, index));
    }

    function weight(bytes32 key, bytes32 node) internal pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked("onyx-storage-hrw-v1", key, node)));
    }

    function seed(bytes32 startBlockHash, uint64 epoch) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("onyx-storage-seed-v1", startBlockHash, epoch));
    }

    function slotShard(bytes32 s, uint64 dealId, uint32 slot, uint32 shardCount)
        internal
        pure
        returns (uint32)
    {
        return uint32(
            uint256(keccak256(abi.encodePacked("onyx-storage-slot-v1", s, dealId, slot)))
                % shardCount
        );
    }

    /// The row a challenged holder must prove: drawn for each holder (`node`), so one answer in
    /// calldata answers for no other holder of the shard (ADR-0060 amendment 1 item 1, THREATS
    /// STORAGE-S16-R2-2).
    function slotRow(bytes32 s, uint64 dealId, uint32 slot, bytes32 node, uint64 rows)
        internal
        pure
        returns (uint64)
    {
        return uint64(
            uint256(keccak256(abi.encodePacked("onyx-storage-row-v2", s, dealId, slot, node)))
                % rows
        );
    }

    function setHash(bytes32[] memory nodes) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(nodes));
    }

    function readU32(bytes calldata b, uint256 o) internal pure returns (uint256 v) {
        for (uint256 i = 0; i < 4; i++) {
            v |= uint256(uint8(b[o + i])) << (8 * i);
        }
    }

    function readU64(bytes calldata b, uint256 o) internal pure returns (uint256 v) {
        for (uint256 i = 0; i < 8; i++) {
            v |= uint256(uint8(b[o + i])) << (8 * i);
        }
    }

    function le32(uint256 v) internal pure returns (bytes4) {
        return bytes4(
            uint32(
                ((v & 0xff) << 24) | (((v >> 8) & 0xff) << 16) | (((v >> 16) & 0xff) << 8)
                    | ((v >> 24) & 0xff)
            )
        );
    }

    /// `onyx_storage::deal::read_header`: format 1 or 2 (both have the same first fields and the
    /// same tail; ADR-0067), the dataset and publisher strings (1..=255 bytes each), the tail; a
    /// zero shard count or byte count is refused.
    function readHeader(bytes calldata h) internal pure returns (Header memory r) {
        uint256 n = h.length;
        if (n < 9 + TAIL_BYTES || (uint8(h[0]) != 1 && uint8(h[0]) != 2)) revert BadHeader();
        uint256 dl = readU32(h, 1);
        if (dl == 0 || dl > 255) revert BadHeader();
        r.dStart = 5;
        r.dEnd = 5 + dl;
        if (r.dEnd + 4 > n) revert BadHeader();
        uint256 pl = readU32(h, r.dEnd);
        if (pl == 0 || pl > 255) revert BadHeader();
        r.pStart = r.dEnd + 4;
        r.pEnd = r.pStart + pl;
        if (r.pEnd + TAIL_BYTES > n) revert BadHeader();
        uint256 t = n - TAIL_BYTES;
        r.shardCount = uint32(readU32(h, t));
        r.rowCount = uint64(readU64(h, t + 4));
        r.byteCount = uint64(readU64(h, t + 12));
        r.shardsRoot = bytes32(h[n - 32:n]);
        // ADR-0059 caps a manifest at 65,536 shards (review STORAGE-R1-7), and every shard holds
        // at least 8 bytes (4 + 4 per row, at least one row; review STORAGE-R2-5).
        if (
            r.shardCount == 0 || r.shardCount > MAX_SHARDS || r.byteCount == 0
                || r.byteCount < 8 * uint256(r.shardCount)
        ) revert BadHeader();
        r.root = keccak256(abi.encodePacked("onyx-bulk-manifest-v1", h));
    }

    /// `onyx_storage::deal::check_payload`: `payload` is exactly the anchor of the header `h`
    /// (publisher, any key, metric "bulk.manifest", subject Aggregate(dataset), value
    /// Bytes(root), any time and sequence). Returns its digest in the book.
    function checkPayload(bytes calldata h, Header memory r, bytes calldata p)
        internal
        pure
        returns (bytes32)
    {
        uint256 pl = r.pEnd - r.pStart;
        uint256 dl = r.dEnd - r.dStart;
        uint256 o = 4 + pl + 32;
        if (p.length != o + 4 + METRIC.length + 1 + 4 + dl + 1 + 4 + 32 + 16) revert BadPayload();
        bytes memory head = abi.encodePacked(le32(pl), h[r.pStart:r.pEnd]);
        if (keccak256(p[0:4 + pl]) != keccak256(head)) revert BadPayload();
        bytes memory mid = abi.encodePacked(
            le32(METRIC.length),
            METRIC,
            uint8(3),
            le32(dl),
            h[r.dStart:r.dEnd],
            uint8(4),
            le32(32),
            r.root
        );
        if (keccak256(p[o:p.length - 16]) != keccak256(mid)) revert BadPayload();
        return keccak256(abi.encodePacked("onyx-signed-source-v1", p));
    }

    /// `onyx_storage::deal::leaf_row`: the catalog leaf row of the manifest header `h`, the bytes
    /// of `onyx_bulk::CatalogLeaf::row`: the leaf tag, the publisher and dataset strings (each
    /// with its Borsh length), the version (4 bytes) and the previous root (32 bytes), which sit
    /// right after the publisher in the header, and the manifest root.
    function leafRow(bytes calldata h, Header memory r) internal pure returns (bytes memory) {
        if (r.pEnd + 36 + TAIL_BYTES > h.length) revert BadHeader();
        return abi.encodePacked(
            "onyx-bulk-catalog-leaf-v1",
            h[r.pStart - 4:r.pEnd],
            h[1:r.dEnd],
            h[r.pEnd:r.pEnd + 36],
            r.root
        );
    }

    /// `onyx_storage::deal::check_in_catalog` (ADR-0067 checks 1 to 4): `c` is a Borsh catalog
    /// header of exact length (format 1, the publisher of the manifest header `h`, 84 fixed
    /// bytes), and the leaf row of `h` (publisher, dataset, version, previous root, manifest
    /// root) is leaf `leafIndex` of its tree. Returns the catalog root.
    function checkCatalog(
        bytes calldata h,
        Header memory r,
        bytes calldata c,
        uint64 leafIndex,
        bytes32[] calldata siblings
    ) internal pure returns (bytes32) {
        bytes memory row = leafRow(h, r);
        // `o`: where the fixed part of the catalog header starts.
        uint256 o = 1 + r.pEnd - (r.pStart - 4);
        if (c.length != o + CATALOG_FIXED || uint8(c[0]) != 1) revert BadCatalog();
        // The same Borsh string (length, then bytes) names the publisher in both headers.
        if (keccak256(c[1:o]) != keccak256(h[r.pStart - 4:r.pEnd])) revert BadCatalog();
        if (!OnyxMerkle.verify(
                bytes32(c[o + 52:o + 84]), row, leafIndex, uint64(readU32(c, o + 48)), siblings
            )) revert NotInCatalog();
        return keccak256(abi.encodePacked("onyx-bulk-catalog-v1", c));
    }

    /// `onyx_storage::deal::check_catalog_payload` (ADR-0067 check 5): `p` is exactly the anchor
    /// of the catalog header `c` with root `root` (its publisher, any key, metric
    /// "bulk.catalog", subject Aggregate("catalog"), value Bytes(root), the catalog's creation
    /// time, the one sequence of its number). Returns its digest in the book. Call it after
    /// `checkCatalog`, which checks the length of `c`.
    function checkCatalogPayload(bytes calldata c, bytes32 root, bytes calldata p)
        internal
        pure
        returns (bytes32)
    {
        uint256 o = c.length - CATALOG_FIXED;
        // Publisher string and key, then the names, the value (1 + 4 + 32 bytes), the time and
        // the sequence (8 bytes each).
        uint256 k = o - 1 + 32;
        if (p.length != k + CATALOG_NAMES.length + 53) revert BadPayload();
        if (keccak256(p[0:o - 1]) != keccak256(c[1:o])) revert BadPayload();
        bytes memory rest =
            abi.encodePacked(CATALOG_NAMES, uint8(4), le32(32), root, c[o + 40:o + 48]);
        if (keccak256(p[k:p.length - 8]) != keccak256(rest)) revert BadPayload();
        // `onyx_bulk::catalog::sequence_for_catalog`: never the reserved largest value.
        uint64 s = uint64(
            bytes8(keccak256(abi.encodePacked("onyx-bulk-catalog-sequence-v1", c[o:o + 8])))
        );
        if (s == type(uint64).max) s--;
        if (readU64(p, p.length - 8) != s) revert BadPayload();
        return keccak256(abi.encodePacked("onyx-signed-source-v1", p));
    }

    /// `onyx_storage::deal::next_catalog_sequence`: the sequence the catalog after the catalog
    /// header `c` is signed under (the sequence of its number plus one), or the reserved largest
    /// value, which no report can use, when the number is the largest there is. Call it after
    /// `checkCatalog`, which checks the length of `c`.
    function nextCatalogSequence(bytes calldata c) internal pure returns (uint64 s) {
        uint256 n = readU64(c, c.length - CATALOG_FIXED);
        if (n == type(uint64).max) return type(uint64).max;
        n += 1;
        // Borsh writes the number with its lowest byte first.
        bytes memory le = new bytes(8);
        for (uint256 i = 0; i < 8; i++) {
            le[i] = bytes1(uint8(n >> (8 * i)));
        }
        s = uint64(bytes8(keccak256(abi.encodePacked("onyx-bulk-catalog-sequence-v1", le))));
        if (s == type(uint64).max) s--;
    }

    /// `onyx_storage::challenge::verify`: the entry is the challenged shard and proves to
    /// `shardsRoot` among `shardCount`; the row is row `slotRow(node, rows)` of the entry's root,
    /// the row drawn for the answering holder `node`.
    function verifyProof(
        bytes32 shardsRoot,
        uint32 shardCount,
        bytes32 s,
        uint64 dealId,
        uint32 slot,
        bytes32 node,
        bytes calldata entry,
        bytes32[] calldata entrySiblings,
        bytes calldata row,
        bytes32[] calldata rowSiblings
    ) internal pure returns (bool) {
        if (entry.length != ENTRY_BYTES) return false;
        uint32 want = slotShard(s, dealId, slot, shardCount);
        if (readU32(entry, 0) != want) return false;
        if (!OnyxMerkle.verify(shardsRoot, entry, want, shardCount, entrySiblings)) return false;
        uint64 rows = uint64(readU64(entry, 36));
        if (rows == 0) return false;
        return OnyxMerkle.verify(
            bytes32(entry[4:36]), row, slotRow(s, dealId, slot, node, rows), rows, rowSiblings
        );
    }
}
