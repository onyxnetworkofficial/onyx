// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title OnyxMerkle
/// @notice Verifier for the positional Keccak-256 Merkle layout in ADR-0003.
/// Must stay byte-for-byte compatible with crates/onyx-merkle.
///   leaf = keccak256(0x00 || row)
///   node = keccak256(0x01 || left || right)
///   odd last node at a level is promoted unchanged
///   root = keccak256(0x02 || uint64 leafCount (big-endian) || top)
library OnyxMerkle {
    function leafHash(bytes memory row) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(bytes1(0x00), row));
    }

    function nodeHash(bytes32 left, bytes32 right) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(bytes1(0x01), left, right));
    }

    function finalRoot(uint64 leafCount, bytes32 top) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(bytes1(0x02), leafCount, top));
    }

    /// @notice True if `row` is the row at `index` in a tree of `leafCount` rows under `root`.
    function verify(
        bytes32 root,
        bytes memory row,
        uint64 index,
        uint64 leafCount,
        bytes32[] memory siblings
    ) internal pure returns (bool) {
        return verifyLeaf(root, leafHash(row), index, leafCount, siblings);
    }

    /// @notice Same as `verify` with a precomputed leaf hash.
    function verifyLeaf(
        bytes32 root,
        bytes32 leaf,
        uint64 index,
        uint64 leafCount,
        bytes32[] memory siblings
    ) internal pure returns (bool) {
        if (index >= leafCount) return false;
        bytes32 h = leaf;
        uint64 idx = index;
        uint64 n = leafCount;
        uint256 k = 0;
        while (n > 1) {
            if (idx & 1 == 1) {
                if (k >= siblings.length) return false;
                h = nodeHash(siblings[k++], h);
            } else if (idx + 1 < n) {
                if (k >= siblings.length) return false;
                h = nodeHash(h, siblings[k++]);
            }
            idx >>= 1;
            n = (n >> 1) + (n & 1);
        }
        return k == siblings.length && finalRoot(leafCount, h) == root;
    }
}
