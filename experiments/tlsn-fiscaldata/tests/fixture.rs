//! ROADMAP S2b: offline checks over the recorded presentation (fixtures/PROVENANCE.md).
//! Run inside this folder: `cargo test --locked` (no network).

use onyx_tlsn_fiscaldata::{self as x, Policy, VerifyError, verify_presentation};

const PRESENTATION: &[u8] = include_bytes!("../fixtures/presentation.bin");
const SUMMARY: &str = include_str!("../fixtures/summary.json");
/// The adapter's own recorded body of the same URL (2026-10-11), byte for byte.
const ADAPTER_BODY: &[u8] = include_bytes!(
    "../../../adapters/source-fiscaldata/fixtures/debt-to-penny-2026-10-08/response.json"
);
/// The local notary's public key for the recorded run (its signing key was dropped).
const NOTARY_KEY: &str = "02bb1b2611e74b40a43a01cef678820763157d86898e6018e56c91373095819924";
/// The recorded session's start (2026-10-21 07:26:35 UTC).
const SESSION_TIME: u64 = 1_792_567_595;

fn policy() -> Policy {
    Policy::fiscaldata(
        hex::decode(NOTARY_KEY).unwrap(),
        SESSION_TIME + 60,
        Some(600),
    )
}

#[test]
fn recorded_presentation_verifies() {
    let c = verify_presentation(PRESENTATION, &policy()).unwrap();
    assert_eq!(c.server_name, "api.fiscaldata.treasury.gov");
    assert_eq!(c.tls_version, "TLS 1.2");
    assert_eq!(c.connection_time, SESSION_TIME);
    assert_eq!(c.status, 200);
    assert_eq!(c.request_target, x::REQUEST_TARGET);
    assert_eq!(c.value, "40305316210829.72");
    assert_eq!(c.record_date, "2026-10-08");
    // The notarised body is the adapter's recorded body, so its hash is the one in
    // adapters/source-fiscaldata/fixtures/PROVENANCE.md.
    assert_eq!(c.body, ADAPTER_BODY);
    assert_eq!(
        c.body_keccak256,
        "0xf5dd7a5c490fe1584511f4629bb503fdb1a770af9c1b4c13fe07497c6d45bfc4"
    );
    assert_eq!(c.hidden_response_bytes, 260);
}

#[test]
fn summary_matches_the_verified_presentation() {
    let s: serde_json::Value = serde_json::from_str(SUMMARY).unwrap();
    let c = verify_presentation(PRESENTATION, &policy()).unwrap();
    assert_eq!(s["prover"], x::PROVER_ID);
    assert_eq!(s["tlsnVersion"], x::TLSN_VERSION);
    assert_eq!(s["notaryPublicKey"], NOTARY_KEY);
    assert_eq!(s["connectionTimeUnix"], SESSION_TIME);
    assert_eq!(s["responseKeccak256"], c.body_keccak256.as_str());
    assert_eq!(s["responseBytes"], c.body.len() as u64);
    assert_eq!(s["value"], c.value.as_str());
    assert_eq!(s["presentationBytes"], PRESENTATION.len() as u64);
    assert_eq!(
        s["presentationKeccak256"],
        x::keccak_hex(PRESENTATION).as_str()
    );
    assert_eq!(s["redactedValueBytes"], 32);
}

#[test]
fn demo_key_value_is_hidden_and_nothing_else_in_the_request() {
    let c = verify_presentation(PRESENTATION, &policy()).unwrap();
    assert_eq!(c.redacted_header, "X-Demo-Key");
    assert_eq!(c.redacted_range.len(), 32);
    let line = format!("x-demo-key: {}\r\n", "X".repeat(32));
    assert!(c.request_shown.contains(&line), "{}", c.request_shown);
    // Everything else in the request is revealed (the library refuses otherwise), and
    // the User-Agent names no person.
    // hyper sends header names in lower case, so every 'X' is a hidden byte.
    assert_eq!(c.request_shown.matches('X').count(), 32);
    assert!(c.request_shown.contains("user-agent: onyx-dev\r\n"));
}

#[test]
fn wrong_notary_key_is_refused() {
    let mut p = policy();
    p.notary_key[1] ^= 1;
    assert!(matches!(
        verify_presentation(PRESENTATION, &p),
        Err(VerifyError::NotaryKey { .. })
    ));
}

#[test]
fn old_presentation_is_refused_when_a_max_age_is_set() {
    let mut p = policy();
    p.now = SESSION_TIME + 601;
    assert!(matches!(
        verify_presentation(PRESENTATION, &p),
        Err(VerifyError::Stale { .. })
    ));
    // No max age: accepted at any later time (the API must set one; README plan).
    p.max_age_secs = None;
    p.now = SESSION_TIME + 10 * 365 * 86_400;
    assert!(verify_presentation(PRESENTATION, &p).is_ok());
}

#[test]
fn session_in_the_future_is_refused() {
    let mut p = policy();
    p.now = SESSION_TIME - 1_000;
    assert!(matches!(
        verify_presentation(PRESENTATION, &p),
        Err(VerifyError::Future { .. })
    ));
}

#[test]
fn other_host_or_target_is_refused() {
    let mut p = policy();
    p.server_name = "api.stlouisfed.org".into();
    assert!(matches!(
        verify_presentation(PRESENTATION, &p),
        Err(VerifyError::ServerName(_))
    ));
    let mut p = policy();
    p.request_target = "/services/api/fiscal_service/v2/accounting/od/debt_to_penny".into();
    assert!(matches!(
        verify_presentation(PRESENTATION, &p),
        Err(VerifyError::Request(_))
    ));
}

#[test]
fn hiding_anything_but_the_declared_header_is_refused() {
    // Declared key header differs from the hidden one: the hidden bytes are then undeclared.
    let mut p = policy();
    p.redacted_header = "Authorization".into();
    assert!(matches!(
        verify_presentation(PRESENTATION, &p),
        Err(VerifyError::Request(_))
    ));
    // A hidden value longer than allowed.
    let mut p = policy();
    p.max_redacted_len = 31;
    assert!(matches!(
        verify_presentation(PRESENTATION, &p),
        Err(VerifyError::Request(_))
    ));
}

#[test]
fn trailing_or_oversized_input_is_refused() {
    let mut b = PRESENTATION.to_vec();
    b.push(0);
    assert!(matches!(
        verify_presentation(&b, &policy()),
        Err(VerifyError::Decode(_))
    ));
    let big = vec![0u8; x::MAX_PRESENTATION_BYTES + 1];
    assert!(matches!(
        verify_presentation(&big, &policy()),
        Err(VerifyError::TooLarge(_))
    ));
    assert!(verify_presentation(&[], &policy()).is_err());
}

/// Every single-bit flip must be refused. The scan runs in a child process (`flipscan`) under a
/// 4 GiB address-space limit, because tlsn-core allocates a length read from the file before
/// checking it: such a flip aborts the process (THREATS TLSN-5), which refuses it too, but would
/// kill an in-process test. After an abort the scan resumes at the next byte. No flip may be
/// accepted with a different verified result. A flip accepted with the exact original result
/// changes a byte the proof does not depend on (THREATS TLSN-6: the presentation's hash is not
/// its identity); those and the abort positions are printed for a reviewer.
#[test]
fn no_bit_flip_changes_what_is_proved() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/presentation.bin");
    let now = (SESSION_TIME + 60).to_string();
    let mut start = 0usize;
    let mut aborted = Vec::new();
    let mut accepted = Vec::new();
    let mut same = Vec::new();
    loop {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg("ulimit -v 4194304; exec \"$0\" \"$@\"")
            .arg(env!("CARGO_BIN_EXE_flipscan"))
            .args([path, NOTARY_KEY, now.as_str(), &start.to_string(), "0"])
            .output()
            .expect("run flipscan");
        let stdout = String::from_utf8_lossy(&out.stdout);
        accepted.extend(
            stdout
                .lines()
                .filter_map(|l| l.strip_prefix("CHANGED "))
                .map(|n| n.to_string()),
        );
        same.extend(
            stdout
                .lines()
                .filter_map(|l| l.strip_prefix("SAME "))
                .map(|n| n.to_string()),
        );
        if stdout.lines().any(|l| l == "DONE") {
            break;
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let last: usize = stderr
            .lines()
            .filter_map(|l| l.strip_prefix("pos "))
            .last()
            .and_then(|n| n.parse().ok())
            .expect("flipscan stopped before its first position");
        aborted.push(last);
        start = last + 1;
        if start >= PRESENTATION.len() {
            break;
        }
    }
    eprintln!("{} flips aborted the verifier: {aborted:?}", aborted.len());
    eprintln!(
        "{} flips left the verified result unchanged: {same:?}",
        same.len()
    );
    assert!(
        accepted.is_empty(),
        "flips that change what is proved, at {accepted:?}"
    );
}
