//! Signature recovery helpers: EIP-3009 (`exact_evm::recover`, 65-byte
//! `r || s || v`) and EIP-191 (`eip191_recover`). Never panic; a signature
//! made by a key recovers to that key's address; its high-s twin and a
//! wrong `v` never recover to it.
#![no_main]

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use onyx_api::x402::exact_evm;
use onyx_labels::attest::{eip191_digest, eip191_recover, eip191_sign_with, secret_address};

#[derive(Arbitrary, Debug)]
struct Input {
    digest: [u8; 32],
    sig: Vec<u8>,
    sig65: [u8; 65],
    secret: [u8; 32],
    msg: Vec<u8>,
}

/// secp256k1 group order n.
const N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

/// n - s (s nonzero, below n), big-endian.
fn neg(s: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(N[i]) - i16::from(s[i]) - borrow;
        borrow = i16::from(d < 0);
        out[i] = d.rem_euclid(256) as u8;
    }
    out
}

fuzz_target!(|i: Input| {
    let _ = exact_evm::recover(&i.digest, &i.sig);
    let _ = exact_evm::recover(&i.digest, &i.sig65);
    let _ = eip191_recover(&i.msg, &i.sig65);
    let _ = eip191_digest(&i.msg);
    // A real signature by a fuzzed key.
    let Some(sig) = eip191_sign_with(&i.secret, &i.msg) else {
        return;
    };
    let who = secret_address(&i.secret).expect("a key that signs has an address");
    assert_eq!(
        eip191_recover(&i.msg, &sig),
        Some(who),
        "EIP-191 round trip"
    );
    let digest = eip191_digest(&i.msg).0;
    assert_eq!(
        exact_evm::recover(&digest, &sig),
        Some(who),
        "same digest, same signer"
    );
    assert_eq!(exact_evm::address(&i.secret), Some(who));
    // The high-s twin (s' = n - s, v flipped) is the same point: refused.
    let mut twin = sig;
    twin[32..64].copy_from_slice(&neg(&sig[32..64]));
    twin[64] = if sig[64] == 27 { 28 } else { 27 };
    assert_ne!(
        exact_evm::recover(&digest, &twin),
        Some(who),
        "high-s refused"
    );
    assert_ne!(
        eip191_recover(&i.msg, &twin),
        Some(who),
        "high-s refused (191)"
    );
    // Another v names another key (or none).
    let mut other = sig;
    other[64] = if sig[64] == 27 { 28 } else { 27 };
    assert_ne!(exact_evm::recover(&digest, &other), Some(who), "v flip");
    for bad_v in [2u8, 26, 29, 255] {
        let mut b = sig;
        b[64] = bad_v;
        assert_eq!(exact_evm::recover(&digest, &b), None, "v out of range");
    }
});
