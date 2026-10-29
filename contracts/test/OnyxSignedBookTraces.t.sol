// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxSignedBook} from "../src/OnyxSignedBook.sol";

/// Differential traces (fixtures/signed/traces.json, written by
/// crates/onyx-coordinator/tests/level3_chain_book.rs): every seeded event sequence the off-chain
/// store `SignedSources` ran is replayed against the contract, which must give the same result at
/// every step and hold the same fact and key states after it (ADR-0028 amendment 6). The store
/// withdraws eagerly; the contract reads withdrawals lazily: this is the check that both agree.
contract OnyxSignedBookTracesTest is Test {
    /// Field order is alphabetical: `vm.parseJson` decodes objects in key order.
    struct Step {
        uint256 arg;
        bytes32 facts;
        bytes32 keep;
        uint256 key;
        uint256 keys;
        uint256 kind;
        uint256 now_;
        uint256 outcome;
        bytes payload;
        bytes signature;
    }

    OnyxSignedBook internal book;
    address[3] internal signers;
    bytes32[3] internal ids;

    function _json() internal view returns (string memory json) {
        json = vm.readFile("../fixtures/signed/traces.json");
    }

    function setUp() public {
        string memory json = _json();
        for (uint256 k = 0; k < 3; k++) {
            signers[k] =
                vm.parseJsonAddress(json, string.concat(".keys[", vm.toString(k), "].signer"));
        }
    }

    function _code(bytes memory err) internal pure returns (uint256) {
        bytes4 s = bytes4(err);
        if (s == OnyxSignedBook.AlreadyStated.selector) return 10;
        if (s == OnyxSignedBook.Replay.selector) return 11;
        if (s == OnyxSignedBook.KeyRetiredError.selector) return 12;
        if (s == OnyxSignedBook.KeyCompromisedError.selector) return 13;
        if (s == OnyxSignedBook.KeySuspended.selector) return 14;
        if (s == OnyxSignedBook.Stale.selector) return 15;
        if (s == OnyxSignedBook.FromFuture.selector) return 16;
        if (s == OnyxSignedBook.SequenceReserved.selector) return 17;
        if (s == OnyxSignedBook.BadSignature.selector) return 18;
        if (s == OnyxSignedBook.UnknownKey.selector) return 19;
        if (s == OnyxSignedBook.KeySourceMismatch.selector) return 20;
        if (s == OnyxSignedBook.NotActive.selector) return 21;
        if (s == OnyxSignedBook.TimeInFuture.selector) return 22;
        if (s == OnyxSignedBook.NotSuspended.selector) return 23;
        if (s == OnyxSignedBook.NotDisputed.selector) return 24;
        if (s == OnyxSignedBook.BadResolution.selector) return 25;
        if (s == OnyxSignedBook.SignerTaken.selector) return 26;
        if (s == OnyxSignedBook.KeyAlreadyRegistered.selector) return 27;
        if (s == OnyxSignedBook.OriginMismatch.selector) return 28;
        if (s == OnyxSignedBook.OriginTaken.selector) return 29;
        return 99;
    }

    function _run(Step memory s, bytes32 shared, string memory json) internal returns (uint256) {
        bytes32 id = ids[s.key];
        if (s.kind == 0) {
            try book.postReport(s.payload, s.signature) returns (
                OnyxSignedBook.Outcome o, bytes32
            ) {
                return uint256(o);
            } catch (bytes memory err) {
                return _code(err);
            }
        } else if (s.kind == 1) {
            try book.retireKey(id, uint64(s.arg), "trace") {
                return 0;
            } catch (bytes memory err) {
                return _code(err);
            }
        } else if (s.kind == 2) {
            try book.markCompromised(id, uint64(s.arg), "trace") {
                return 0;
            } catch (bytes memory err) {
                return _code(err);
            }
        } else if (s.kind == 3) {
            try book.reinstateKey(id, "trace") {
                return 0;
            } catch (bytes memory err) {
                return _code(err);
            }
        } else if (s.kind == 4) {
            try book.resolveFact(shared, s.keep, "trace") {
                return 0;
            } catch (bytes memory err) {
                return _code(err);
            }
        } else if (s.kind == 7) {
            try book.registerKey(
                vm.parseJsonString(json, ".otherSource"),
                vm.parseJsonAddress(json, ".freshSigner"),
                vm.parseJsonString(json, ".otherOrigin"),
                vm.parseJsonBytes32(json, ".provenance"),
                "trace"
            ) returns (
                bytes32
            ) {
                return 0;
            } catch (bytes memory err) {
                return _code(err);
            }
        } else if (s.kind == 5) {
            try book.registerKey(
                vm.parseJsonString(json, ".otherSource"),
                signers[s.key],
                vm.parseJsonString(json, ".otherOrigin"),
                vm.parseJsonBytes32(json, ".provenance"),
                "trace"
            ) returns (
                bytes32
            ) {
                return 0;
            } catch (bytes memory err) {
                return _code(err);
            }
        }
        return 0;
    }

    function _keysWord() internal view returns (uint256 w) {
        for (uint256 k = 0; k < 3; k++) {
            (OnyxSignedBook.Life life, bool susp,,,,) = book.keyState(ids[k]);
            w |= (uint256(life) | (susp ? 4 : 0)) << (4 * k);
        }
    }

    function _factsDigest(bytes32[] memory facts) internal view returns (bytes32) {
        bytes memory b;
        for (uint256 i = 0; i < facts.length; i++) {
            (OnyxSignedBook.FactState st, bytes32 acc, uint64 at, bytes32[] memory list) =
                book.factView(facts[i]);
            (bool ok,,) = book.settleable(facts[i]);
            b = abi.encodePacked(
                b, uint8(st), acc, at, uint8(ok ? 1 : 0), uint32(list.length), list
            );
        }
        return keccak256(b);
    }

    /// Traces `[from, to)`; the suite is split so each test stays within the gas limit.
    function _replay(uint256 from, uint256 to) internal {
        string memory json = _json();
        string memory source = vm.parseJsonString(json, ".source");
        string memory origin = vm.parseJsonString(json, ".origin");
        bytes32 prov = vm.parseJsonBytes32(json, ".provenance");
        bytes32 shared = vm.parseJsonBytes32(json, ".sharedFact");
        uint256 start = vm.parseJsonUint(json, ".start");
        uint256 n = vm.parseJsonUint(json, ".count");
        assertGt(n, 100, "traces");
        if (to > n) to = n;
        assertLt(from, to, "no trace in range");
        uint256 stepsRun;
        for (uint256 t = from; t < to; t++) {
            string memory base = string.concat(".traces[", vm.toString(t), "]");
            vm.warp(start);
            // The reviewer is never the deployer (C10): another account deploys.
            vm.prank(address(0xDE9));
            book = new OnyxSignedBook(address(this));
            for (uint256 k = 0; k < 3; k++) {
                ids[k] = book.registerKey(source, signers[k], origin, prov, "setup");
            }
            bytes32[] memory facts = vm.parseJsonBytes32Array(json, string.concat(base, ".facts"));
            Step[] memory steps =
                abi.decode(vm.parseJson(json, string.concat(base, ".steps")), (Step[]));
            for (uint256 i = 0; i < steps.length; i++) {
                Step memory s = steps[i];
                vm.warp(s.now_);
                string memory at = string.concat("trace ", vm.toString(t), " step ", vm.toString(i));
                assertEq(_run(s, shared, json), s.outcome, string.concat(at, ": result"));
                assertEq(_keysWord(), s.keys, string.concat(at, ": keys"));
                assertEq(_factsDigest(facts), s.facts, string.concat(at, ": facts"));
                stepsRun++;
            }
        }
        assertEq(stepsRun, (to - from) * vm.parseJsonUint(json, ".stepsPerTrace"), "steps");
    }

    function test_L3_F7_TheChainBookMatchesTheStoreOnTraces0To39() public {
        _replay(0, 40);
    }

    function test_L3_F7_TheChainBookMatchesTheStoreOnTraces40To79() public {
        _replay(40, 80);
    }

    function test_L3_F7_TheChainBookMatchesTheStoreOnTraces80To119() public {
        _replay(80, 120);
    }

    function test_L3_F7_TheChainBookMatchesTheStoreOnTraces120To159() public {
        _replay(120, 160);
    }

    /// Every trace is replayed by exactly the four tests above.
    function test_L3_F7_TheTracesAreAllReplayed() public view {
        assertEq(vm.parseJsonUint(_json(), ".count"), 160);
    }
}
