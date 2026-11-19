// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxStorageLib as L} from "./OnyxStorageLib.sol";

/// @title OnyxStorageAnchor
/// @notice TESTNET ONLY. The two anchor reads of `OnyxStorage` (ADR-0067): what a deal takes
/// from a manifest header, and the check that a signed payload anchors that manifest, by its own
/// report or through a catalog. They live here because `OnyxStorage` with them is over the
/// 24,576-byte limit on contract code (the stop rule of ADR-0067 slice 2).
///
/// This contract has no state, no owner and no admin path, and it holds no funds. Both
/// functions are pure: they read only their arguments. `OnyxStorage` deploys it in its
/// constructor and keeps its address in an immutable, so nobody can swap it. The book's record
/// of the report (recorded, accepted, key not suspended) stays in `OnyxStorage`.
contract OnyxStorageAnchor {
    /// A manifest with its own report: `payload` is exactly the anchor of the manifest header
    /// `header`. Returns the header's fields and the payload's digest in the book.
    function own(bytes calldata header, bytes calldata payload)
        external
        pure
        returns (L.Header memory h, bytes32 digest)
    {
        h = L.readHeader(header);
        digest = L.checkPayload(header, h, payload);
    }

    /// A manifest listed in a catalog: the leaf of `header` is leaf `leafIndex` of the catalog
    /// header `catalog` (same publisher, Merkle path `siblings`), and `payload` is exactly the
    /// anchor of that catalog. Returns the header's fields, the payload's digest in the book and
    /// the sequence the next catalog of this publisher is signed under (`OnyxStorage` asks the
    /// book whether that catalog exists).
    function inCatalog(
        bytes calldata header,
        bytes calldata catalog,
        uint64 leafIndex,
        bytes32[] calldata siblings,
        bytes calldata payload
    ) external pure returns (L.Header memory h, bytes32 digest, uint64 nextSequence) {
        h = L.readHeader(header);
        digest = L.checkCatalogPayload(
            catalog, L.checkCatalog(header, h, catalog, leafIndex, siblings), payload
        );
        nextSequence = L.nextCatalogSequence(catalog);
    }
}
