// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {DeploySignedBook} from "../script/DeploySignedBook.s.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";

/// The signed-book deploy script (ROADMAP C10), against the local test EVM; no network.
contract DeploySignedBookTest is Test {
    DeploySignedBook internal d;
    address internal constant REVIEWER = address(0xBEEF);

    function setUp() public {
        d = new DeploySignedBook();
    }

    function test_DeploysWithItsReviewerOnAnvilAndBaseSepolia() public {
        OnyxSignedBook book = d.deploy(REVIEWER);
        assertEq(book.reviewer(), REVIEWER);
        vm.chainId(84_532);
        assertTrue(address(d.deploy(REVIEWER)).code.length > 0);
    }

    function test_RefusesOtherChains() public {
        uint256[3] memory ids = [uint256(1), uint256(8_453), uint256(11_155_111)];
        for (uint256 i = 0; i < 3; i++) {
            vm.chainId(ids[i]);
            vm.expectRevert(abi.encodeWithSelector(DeploySignedBook.WrongChain.selector, ids[i]));
            d.deploy(REVIEWER);
        }
    }

    /// THREATS C10-1: the deploy key is never the reviewer, refused before anything is signed
    /// (script) and by the book itself (constructor, with the script as sender here).
    function test_C10_1_ReviewerEqualToDeployerIsRefused() public {
        address deployer = address(0xD0);
        vm.expectRevert(DeploySignedBook.ReviewerIsDeployer.selector);
        d.checkReviewer(deployer, deployer);
        vm.expectRevert(DeploySignedBook.NoReviewer.selector);
        d.checkReviewer(address(0), deployer);
        d.checkReviewer(REVIEWER, deployer);
        vm.expectRevert(OnyxSignedBook.ReviewerIsDeployer.selector);
        d.deploy(address(d));
    }
}
