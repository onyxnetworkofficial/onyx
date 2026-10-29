// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, Vm} from "forge-std/Test.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxRevenue, IOnyxLedgerView, IRevenueToken} from "../src/OnyxRevenue.sol";
import {MockLedgerView} from "./OnyxRevenue.t.sol";

/// The parts of Circle's FiatToken (v2.2 on Base Sepolia) this test drives.
interface IFiatToken {
    function DOMAIN_SEPARATOR() external view returns (bytes32);
    function masterMinter() external view returns (address);
    function configureMinter(address minter, uint256 allowance) external returns (bool);
    function mint(address to, uint256 amount) external returns (bool);
    function balanceOf(address who) external view returns (uint256);
    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool);
    function cancelAuthorization(address authorizer, bytes32 nonce, uint8 v, bytes32 r, bytes32 s)
        external;
    function name() external view returns (string memory);
    function version() external view returns (string memory);
}

/// Self-settle (ADR-0026) on the real Base Sepolia USDC bytecode, in a local fork. Runs only when
/// ONYX_FORK_RPC names an RPC endpoint (for example https://sepolia.base.org, read-only: nothing
/// is broadcast; the fork lives in memory). Without it every test is skipped, so the offline
/// check-all never needs the network. Test USDC is minted inside the fork by pranking Circle's
/// master minter; no key or token of any real account is used.
contract OnyxRevenueForkTest is Test {
    /// Circle's USDC on Base Sepolia (developers.circle.com, "USDC contract addresses").
    address internal constant USDC = 0x036CbD53842c5426634e7929541eC2318f3dCF7e;
    bytes32 internal constant TRANSFER_WITH_AUTHORIZATION_TYPEHASH = keccak256(
        "TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)"
    );
    uint256 internal constant PAYER_KEY =
        uint256(keccak256("onyx fork test payer (holds nothing)"));
    bytes32 internal constant SHARD = bytes32(uint256(7));

    IFiatToken internal usdc = IFiatToken(USDC);
    OnyxRevenue internal rev;
    MockLedgerView internal lv;
    address internal payer;
    address internal treasury = address(0x7EA5);

    function setUp() public {
        string memory rpc = vm.envOr("ONYX_FORK_RPC", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
            return;
        }
        vm.createSelectFork(rpc);
        assertEq(block.chainid, 84_532, "ONYX_FORK_RPC must be Base Sepolia");
        assertGt(USDC.code.length, 0);
        payer = vm.addr(PAYER_KEY);
        vm.prank(usdc.masterMinter());
        usdc.configureMinter(address(this), 1_000e6);
        usdc.mint(payer, 5e6);
        lv = new MockLedgerView();
        OnyxCommit.CommitmentFields memory f;
        f.root = keccak256("fork root");
        f.labelSetHash = keccak256("fork labels");
        lv.set(f.root, OnyxCommit.bindingHash(f), false);
        lv.addReveal(address(0xA), f.root);
        lv.addMember(address(0xD), 1);
        rev = new OnyxRevenue(
            IRevenueToken(USDC),
            IOnyxLedgerView(address(lv)),
            address(this),
            treasury,
            [uint16(5_000), 2_000, 1_000, 2_000]
        );
        rev.register(1, SHARD, f);
    }

    /// The x402 `exact` client signature: EIP-712 TransferWithAuthorization over the token's
    /// domain (name "USDC", version "2"), 65 bytes r || s || v.
    function _sign(uint256 key, address to, uint256 value, bytes32 nonce)
        internal
        view
        returns (OnyxRevenue.Authorization memory a, bytes memory sig)
    {
        a = OnyxRevenue.Authorization({
            from: vm.addr(key),
            value: value,
            validAfter: 0,
            validBefore: block.timestamp + 600,
            nonce: nonce
        });
        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                usdc.DOMAIN_SEPARATOR(),
                keccak256(
                    abi.encode(
                        TRANSFER_WITH_AUTHORIZATION_TYPEHASH,
                        a.from,
                        to,
                        a.value,
                        a.validAfter,
                        a.validBefore,
                        a.nonce
                    )
                )
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        sig = abi.encodePacked(r, s, v);
    }

    function test_Fork_TokenIsUsdcVersion2() public view {
        assertEq(usdc.name(), "USDC");
        assertEq(usdc.version(), "2");
    }

    function test_Fork_SettleMovesRealUsdcAndCreditsTheShard() public {
        (OnyxRevenue.Authorization memory a, bytes memory sig) =
            _sign(PAYER_KEY, address(rev), 2_500, keccak256("fork n1"));
        rev.settle(1, SHARD, a, sig);
        assertEq(usdc.balanceOf(address(rev)), 2_500);
        assertEq(usdc.balanceOf(payer), 5e6 - 2_500);
        assertTrue(usdc.authorizationState(payer, keccak256("fork n1")));
        assertEq(rev.unallocated(), 0);
        // One miner, one validator: 50% + nothing else to the miner; 20% the validator.
        assertEq(rev.claim(1, SHARD, address(0xA)), 1_250);
        assertEq(rev.claim(1, SHARD, address(0xD)), 500);
        // 0xA is a public address: it may already hold test USDC on Base Sepolia.
        uint256 before = usdc.balanceOf(address(0xA));
        vm.prank(address(0xA));
        rev.withdraw();
        assertEq(usdc.balanceOf(address(0xA)) - before, 1_250);
    }

    /// OPS-L3-F2: the real FiatToken emits AuthorizationUsed(from, nonce) and, as the very next
    /// log, the Transfer it caused. The settle-intent resolver relies on this pairing so that one
    /// Transfer proves one authorization.
    function test_Fork_AuthorizationUsedIsFollowedByItsTransfer() public {
        bytes32 nonce = keccak256("fork pairing");
        (OnyxRevenue.Authorization memory a, bytes memory sig) =
            _sign(PAYER_KEY, address(rev), 2_500, nonce);
        vm.recordLogs();
        rev.settle(1, SHARD, a, sig);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bytes32 used = keccak256("AuthorizationUsed(address,bytes32)");
        bytes32 transfer = keccak256("Transfer(address,address,uint256)");
        uint256 found;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != USDC || logs[i].topics[0] != used) continue;
            assertEq(logs[i].topics[1], bytes32(uint256(uint160(payer))));
            assertEq(logs[i].topics[2], nonce);
            assertLt(i + 1, logs.length, "AuthorizationUsed is not the last log");
            Vm.Log memory next = logs[i + 1];
            assertEq(next.emitter, USDC);
            assertEq(next.topics[0], transfer);
            assertEq(next.topics[1], bytes32(uint256(uint160(payer))));
            assertEq(next.topics[2], bytes32(uint256(uint160(address(rev)))));
            assertEq(abi.decode(next.data, (uint256)), 2_500);
            found++;
        }
        assertEq(found, 1);
    }

    /// The real token checks the signature: an authorization signed for another recipient, or by
    /// another key, moves nothing.
    function test_Fork_SignatureIsCheckedByTheToken() public {
        (OnyxRevenue.Authorization memory a, bytes memory sig) =
            _sign(PAYER_KEY, address(0xBEEF), 2_500, keccak256("fork n2"));
        vm.expectRevert();
        rev.settle(1, SHARD, a, sig);
        (OnyxRevenue.Authorization memory b, bytes memory sig2) =
            _sign(uint256(keccak256("someone else")), address(rev), 2_500, keccak256("fork n3"));
        b.from = payer;
        vm.expectRevert();
        rev.settle(1, SHARD, b, sig2);
        assertEq(usdc.balanceOf(address(rev)), 0);
        assertEq(usdc.balanceOf(payer), 5e6);
    }

    /// OPS-L4-F1: `cancelAuthorization` on the real token marks the nonce used, moves nothing,
    /// and emits exactly one `AuthorizationCanceled(authorizer, nonce)`, the event the intent
    /// resolver searches for (onyx-api arrival.rs `authorization_canceled_topic`). It works after
    /// `validBefore` too, which is why the resolver reads cancels up to its pinned block.
    function test_Fork_CancelEmitsAuthorizationCanceled() public {
        bytes32 nonce = keccak256("fork cancel");
        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                usdc.DOMAIN_SEPARATOR(),
                keccak256(
                    abi.encode(
                        keccak256("CancelAuthorization(address authorizer,bytes32 nonce)"),
                        payer,
                        nonce
                    )
                )
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(PAYER_KEY, digest);
        vm.warp(block.timestamp + 365 days);
        vm.recordLogs();
        usdc.cancelAuthorization(payer, nonce, v, r, s);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1, "one log, no Transfer");
        assertEq(logs[0].emitter, USDC);
        assertEq(logs[0].topics[0], keccak256("AuthorizationCanceled(address,bytes32)"));
        assertEq(logs[0].topics[1], bytes32(uint256(uint160(payer))));
        assertEq(logs[0].topics[2], nonce);
        assertTrue(usdc.authorizationState(payer, nonce), "marked used");
        assertEq(usdc.balanceOf(payer), 5e6, "nothing moved");
    }
}
