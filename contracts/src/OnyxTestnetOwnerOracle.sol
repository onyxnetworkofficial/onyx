// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IOnyxFallbackOracle} from "./IOnyxMarkets.sol";

/// @title OnyxTestnetOwnerOracle
/// @notice TESTNET ONLY. The fallback oracle of the S3 prototype (ADR-0042 item 7): one owner key
/// answers a contested or low-quorum claim as correct, wrong or void. It is a stand-in, not a
/// decentralised oracle: the drawn-panel dispute of ADR-0021 (`OnyxDisputes`) judges settlement
/// shards by Merkle root and cannot take a value claim without changing an audited contract, so
/// wiring a drawn panel here is S3b. Guards: deploys only on anvil (31337) or Base Sepolia
/// (84532); answers only claims the stake contract asked about, once; the stake contract never
/// lets the `arbiter` stake, and `OnyxFutures` never lets it bet. An unanswered claim is void
/// after the stake contract's fallback window, so a silent owner refunds everyone.
/// Superseded by `OnyxPanelOracle` (S3b, ADR-0042 amendment 1); kept as the tests' and the
/// demo's simple stand-in.
contract OnyxTestnetOwnerOracle is IOnyxFallbackOracle {
    uint256 internal constant ANVIL = 31_337;
    uint256 internal constant BASE_SEPOLIA = 84_532;

    error WrongChain(uint256 chainId);
    error NotOwner();
    error NotStakes();
    error AlreadyBound();
    error NotAsked();
    error Answered();
    error BadAnswer();

    event Asked(uint256 indexed claimId);
    event Answer(uint256 indexed claimId, uint8 answer);

    address public immutable owner;
    address public stakes;
    mapping(uint256 => bool) public asked;
    mapping(uint256 => uint8) internal _answers;

    constructor() {
        if (block.chainid != ANVIL && block.chainid != BASE_SEPOLIA) {
            revert WrongChain(block.chainid);
        }
        owner = msg.sender;
    }

    function bind(address stakes_) external {
        if (msg.sender != owner) revert NotOwner();
        if (stakes != address(0)) revert AlreadyBound();
        stakes = stakes_;
    }

    /// @inheritdoc IOnyxFallbackOracle
    function arbiter() external view returns (address) {
        return owner;
    }

    /// @inheritdoc IOnyxFallbackOracle
    function request(uint256 claimId) external {
        if (msg.sender != stakes) revert NotStakes();
        asked[claimId] = true;
        emit Asked(claimId);
    }

    /// @notice The owner's answer: 1 correct, 2 wrong, 3 void. Once per claim.
    function answer(uint256 claimId, uint8 a) external {
        if (msg.sender != owner) revert NotOwner();
        if (!asked[claimId]) revert NotAsked();
        if (_answers[claimId] != 0) revert Answered();
        if (a == 0 || a > 3) revert BadAnswer();
        _answers[claimId] = a;
        emit Answer(claimId, a);
    }

    /// @inheritdoc IOnyxFallbackOracle
    function answerOf(uint256 claimId) external view returns (uint8) {
        return _answers[claimId];
    }
}
