//! THREATS TLSN-5: the verifier runs in its own memory-limited process, so a presentation that makes
//! tlsn-core abort is refused and the caller keeps running.

use onyx_tlsn_fiscaldata::isolated::{Isolated, verify_isolated};
use std::path::{Path, PathBuf};
use std::time::Duration;

const KEY: &str = "02bb1b2611e74b40a43a01cef678820763157d86898e6018e56c91373095819924";
const NOW: u64 = 1_792_567_595 + 60;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/presentation.bin")
}

fn flipped(at: usize, name: &str) -> PathBuf {
    let mut b = std::fs::read(fixture()).expect("fixture");
    b[at] ^= 1;
    let p = std::env::temp_dir().join(format!("onyx-tlsn-{}-{name}.bin", std::process::id()));
    std::fs::write(&p, b).expect("write");
    p
}

fn run(p: &Path) -> Isolated {
    verify_isolated(
        Path::new(env!("CARGO_BIN_EXE_verify")),
        p,
        KEY,
        NOW,
        Some(600),
        4 * 1024 * 1024,
        Duration::from_secs(60),
    )
}

#[test]
fn the_recorded_presentation_verifies_in_a_child() {
    match run(&fixture()) {
        Isolated::Verified(out) => assert!(out.contains("api.fiscaldata.treasury.gov"), "{out}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_presentation_that_aborts_tlsn_core_is_refused_and_the_caller_survives() {
    // Byte 15,228: the flip makes tlsn-core ask for about 1 TB (the bit-flip scan's first abort).
    let p = flipped(15_228, "abort");
    let got = run(&p);
    let _ = std::fs::remove_file(&p);
    assert!(matches!(got, Isolated::Crashed(_)), "{got:?}");
    // Still here: the abort ended only the child.
    assert!(matches!(run(&fixture()), Isolated::Verified(_)));
}

#[test]
fn a_wrong_notary_key_is_refused_not_crashed() {
    let wrong = format!("03{}", &KEY[2..]);
    let got = verify_isolated(
        Path::new(env!("CARGO_BIN_EXE_verify")),
        &fixture(),
        &wrong,
        NOW,
        Some(600),
        4 * 1024 * 1024,
        Duration::from_secs(60),
    );
    assert!(matches!(got, Isolated::Refused(_)), "{got:?}");
}
