// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title OnyxSig
/// @notice Text formatting and EIP-191 signature recovery that match the Rust side
/// (`Hash32` Display, `eip191_digest`, `eip191_recover` in crates/onyx-labels/src/attest.rs).
library OnyxSig {
    /// secp256k1 group order / 2: signatures with a larger s are refused (EIP-2).
    uint256 internal constant HALF_N =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

    bytes16 private constant HEX = "0123456789abcdef";

    /// "0x" followed by 64 lowercase hex digits, as Rust prints a `Hash32`.
    function hex32(bytes32 v) internal pure returns (string memory) {
        bytes memory out = new bytes(66);
        out[0] = "0";
        out[1] = "x";
        for (uint256 i = 0; i < 32; i++) {
            uint8 b = uint8(v[i]);
            out[2 + 2 * i] = HEX[b >> 4];
            out[3 + 2 * i] = HEX[b & 0x0f];
        }
        return string(out);
    }

    /// "0x" followed by 40 lowercase hex digits, as Rust prints an address
    /// (`format!("0x{}", hex::encode(a))`).
    function hex20(address a) internal pure returns (string memory) {
        bytes memory full = bytes(hex32(bytes32(bytes20(a))));
        bytes memory out = new bytes(42);
        for (uint256 i = 0; i < 42; i++) {
            out[i] = full[i];
        }
        return string(out);
    }

    /// Decimal digits of `v`, no sign, no leading zeros ("0" for zero).
    function decimal(uint256 v) internal pure returns (string memory) {
        if (v == 0) return "0";
        uint256 len;
        for (uint256 t = v; t != 0; t /= 10) {
            len++;
        }
        bytes memory out = new bytes(len);
        for (uint256 i = len; i > 0; i--) {
            out[i - 1] = bytes1(uint8(48 + (v % 10)));
            v /= 10;
        }
        return string(out);
    }

    /// keccak256("\x19Ethereum Signed Message:\n" || len(msg) in decimal || msg).
    function eip191Digest(bytes memory message) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked("\x19Ethereum Signed Message:\n", decimal(message.length), message)
        );
    }

    /// Recover the signer of a 65-byte `r || s || v` signature over `digest`.
    /// Only the canonical form counts: v in {27, 28} and s <= n/2. Returns
    /// address(0) for anything else, never reverts.
    function recover(bytes32 digest, bytes memory sig) internal pure returns (address) {
        if (sig.length != 65) return address(0);
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly ("memory-safe") {
            r := mload(add(sig, 32))
            s := mload(add(sig, 64))
            v := byte(0, mload(add(sig, 96)))
        }
        if (v != 27 && v != 28) return address(0);
        if (uint256(s) > HALF_N || uint256(s) == 0 || uint256(r) == 0) return address(0);
        return ecrecover(digest, v, r, s);
    }
}
