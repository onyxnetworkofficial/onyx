//! Robustness scan over a presentation: flips one bit (`BIT`) of every byte from `START` and
//! runs the verifier on each. Prints `CHANGED <pos>` for a flip the verifier accepts with a
//! different verified result (a forgery), `SAME <pos>` for one it accepts with the original's
//! exact result (a byte the proof does not depend on: THREATS TLSN-6), and `DONE` at the end; prints `pos <n>` to stderr before each try, so when the process aborts
//! (tlsn-core allocates a transcript length read from the file before checking it) the
//! caller knows where and resumes after it. Offline. Used by tests/fixture.rs.
//!
//! Usage: `flipscan PRESENTATION NOTARY_KEY_HEX NOW START BIT`

use onyx_tlsn_fiscaldata::{Policy, verify_presentation};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(
        a.len(),
        5,
        "usage: flipscan PRESENTATION NOTARY_KEY_HEX NOW START BIT"
    );
    let data = std::fs::read(&a[0]).expect("read presentation");
    let key = hex::decode(&a[1]).expect("key hex");
    let now: u64 = a[2].parse().expect("now");
    let start: usize = a[3].parse().expect("start");
    let bit: u32 = a[4].parse().expect("bit");
    let p = Policy::fiscaldata(key, now, Some(600));
    let original = verify_presentation(&data, &p).expect("the unflipped presentation verifies");
    for i in start..data.len() {
        eprintln!("pos {i}");
        let mut b = data.clone();
        b[i] ^= 1 << bit;
        match verify_presentation(&b, &p) {
            Ok(c) if c == original => println!("SAME {i}"),
            Ok(_) => println!("CHANGED {i}"),
            Err(_) => {}
        }
    }
    println!("DONE");
}
