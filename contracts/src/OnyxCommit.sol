// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title OnyxCommit
/// @notice Commitment binding hash and commit hash (ADR-0011). Must match
/// crates/onyx-core/src/commitment.rs; shared vectors in fixtures/commit/vectors.json.
library OnyxCommit {
    /// @notice Flattened commitment. Optional fields are zero when absent.
    struct CommitmentFields {
        bytes32 root;
        uint64 leafCount;
        bytes32 recipeId;
        bytes32 runtimeHash;
        bytes32 labelSetHash;
        bytes32 sourceHash;
        bytes32 schemaId;
        bytes32 rightsHash;
    }

    /// keccak256("onyx-commitment-v1" || root || leafCount (8 bytes BE) || recipeId ||
    /// runtimeHash || labelSetHash || sourceHash || schemaId || rightsHash)
    function bindingHash(CommitmentFields memory c) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-commitment-v1",
                c.root,
                c.leafCount,
                c.recipeId,
                c.runtimeHash,
                c.labelSetHash,
                c.sourceHash,
                c.schemaId,
                c.rightsHash
            )
        );
    }

    /// keccak256("onyx-shard-fields-v1" || recipeId || runtimeHash || labelSetHash ||
    /// sourceHash || schemaId || rightsHash): everything except root and count.
    /// A reveal must match the shard's registered value (R-SHARD-1).
    function shardFieldsHash(CommitmentFields memory c) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-shard-fields-v1",
                c.recipeId,
                c.runtimeHash,
                c.labelSetHash,
                c.sourceHash,
                c.schemaId,
                c.rightsHash
            )
        );
    }

    /// keccak256("onyx-shard-v1" || fieldsHash || paramsHash || registryRoot)
    function shardId(bytes32 fieldsHash, bytes32 paramsHash, bytes32 registryRoot)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked("onyx-shard-v1", fieldsHash, paramsHash, registryRoot));
    }

    /// keccak256("onyx-challenge-v1" || round || shard || claimedRoot || defender || challenger || salt).
    /// Challenges are committed first so the claimed root cannot be copied from the mempool
    /// (R-CHAL-2); the defender pins which reveal is challenged (R-CHAL-3).
    function challengeHash(
        uint64 round,
        bytes32 shard,
        bytes32 claimedRoot,
        address defender,
        address challenger,
        bytes32 salt
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "onyx-challenge-v1", round, shard, claimedRoot, defender, challenger, salt
            )
        );
    }

    /// keccak256("onyx-commit-v1" || round (8 bytes BE) || shard || binding || miner (20 bytes) || salt)
    function commitHash(uint64 round, bytes32 shard, bytes32 binding, address miner, bytes32 salt)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked("onyx-commit-v1", round, shard, binding, miner, salt));
    }
}
