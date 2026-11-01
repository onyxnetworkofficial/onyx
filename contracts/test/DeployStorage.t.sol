// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeployStorage} from "../script/DeployStorage.s.sol";
import {OnyxStorage} from "../src/OnyxStorage.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";

/// The storage deploy script (ADR-0060), run against the local test EVM; no network.
contract DeployStorageTest is Test {
    DeployStorage internal d;
    OnyxTestToken internal tok;
    OnyxSignedBook internal book;

    function setUp() public {
        d = new DeployStorage();
        tok = new OnyxTestToken(address(this));
        book = new OnyxSignedBook(address(0xBEEF));
    }

    function test_deploysWithThePlaceholderParameters() public {
        OnyxStorage s = d.deploy(address(tok), address(book), address(0x7EA5), d.defaults());
        assertEq(address(s.token()), address(tok));
        assertEq(address(s.book()), address(book));
        assertEq(s.treasury(), address(0x7EA5));
        assertEq(s.epochBlocks(), d.EPOCH_BLOCKS());
        assertEq(s.pricePerGiBEpoch(), d.PRICE_PER_GIB_EPOCH());
        assertEq(s.bondPerGiB(), d.BOND_PER_GIB());
        assertEq(s.minBond(), d.MIN_BOND());
        assertEq(s.slashPpm(), d.SLASH_PPM());
        assertEq(s.challengesPerDeal(), d.CHALLENGES_PER_DEAL());
        assertEq(s.maxMisses(), d.MAX_MISSES());
        assertEq(s.genesis(), block.number);
    }

    function test_refusesAddressesWithoutCodeAndOtherChains() public {
        DeployStorage.Params memory p = d.defaults();
        vm.expectRevert(abi.encodeWithSelector(DeployStorage.NoCode.selector, address(0x1234)));
        d.deploy(address(0x1234), address(book), address(1), p);
        vm.chainId(8453);
        vm.expectRevert(abi.encodeWithSelector(DeployStorage.WrongChain.selector, 8453));
        d.deploy(address(tok), address(book), address(1), p);
    }

    function test_envOverridesTheParameters() public {
        vm.setEnv("ONYX_STORAGE_EPOCH_BLOCKS", "30");
        vm.setEnv("ONYX_STORAGE_PRICE_PER_GIB_EPOCH", "7");
        DeployStorage.Params memory p = d.fromEnv();
        assertEq(p.epochBlocks, 30);
        assertEq(p.price, 7);
        assertEq(p.minBond, d.MIN_BOND());
    }
}
