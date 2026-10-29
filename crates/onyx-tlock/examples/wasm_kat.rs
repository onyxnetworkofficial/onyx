//! The known-answer checks of `tests/interop.rs` as a program, for wasm32
//! (ROADMAP C50): `scripts/check-tlock-wasm.sh` builds it for
//! `wasm32-unknown-unknown` (blst's C built by clang) and runs it in node. A
//! failed check panics, which traps, and the script fails. Natively it runs
//! the same checks (`cargo run -p onyx-tlock --example wasm_kat`).
//!
//! Built with `ONYX_TLOCK_KAT_BREAK=1`, it also requires a tampered Go
//! ciphertext to open, which must fail: the script's self-test that a failing
//! check is reported as a failure on wasm32.

#[path = "../tests/kat/mod.rs"]
mod kat;

fn main() {
    let report = match kat::run_all() {
        Ok(r) => r,
        Err(e) => panic!("tlock known-answer check failed: {e}"),
    };
    assert_eq!(report, kat::EXPECTED, "fixture counts");
    if option_env!("ONYX_TLOCK_KAT_BREAK").is_some() {
        let b = kat::beacon("quicknet", "drand/quicknet-round-746511.json").expect("beacon");
        let f = kat::parse_age(kat::file("tlock-js/rfc-scheme-quicknet-746511.age").expect("file"))
            .expect("age");
        let mut ct = f.ciphertext.clone();
        ct[100] ^= 1;
        assert!(
            b.key.open(b.round, &b.signature, &ct).is_ok(),
            "self-test: a tampered ciphertext is refused (this panic is expected)"
        );
    }
}
