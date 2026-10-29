//! ROADMAP S2b: verify a TLSNotary presentation of one U.S. Treasury Fiscal Data response with
//! one request header (a stand-in API key) hidden. This is what Onyx would run; it never sees the
//! hidden header's value. See README.md in this folder.
//!
//! The checks, in order, each with its own [`VerifyError`]:
//! 1. the file is at most [`MAX_PRESENTATION_BYTES`] and decodes exactly (no trailing bytes);
//! 2. the notary's key is the pinned one (secp256k1, SEC1 bytes) — the one thing trusted;
//! 3. the notary's signature, the server certificate chain (webpki roots compiled in, at the
//!    connection time) and the transcript commitments verify (`Presentation::verify`);
//! 4. the server name is the expected host, the TLS version is 1.2, the session is not older
//!    than the policy's maximum age nor in the future;
//! 5. request: `GET` of the expected target, `Host` the expected host, every byte revealed except
//!    the value of exactly one header named [`REDACTED_HEADER`], whose value is wholly hidden,
//!    non-empty and at most `max_redacted_len` bytes;
//! 6. response: status 200, every byte revealed except `Set-Cookie` values, `Content-Length`
//!    equal to the revealed body, the body is JSON with a string at [`JSON_POINTER`].

pub mod isolated;

use std::ops::Range;

use sha3::{Digest, Keccak256};
use tlsn::{
    attestation::{
        CryptoProvider,
        presentation::{Presentation, PresentationOutput},
        signing::KeyAlgId,
    },
    connection::{ServerName, TlsVersion},
    transcript::PartialTranscript,
};
use tlsn_formats::spansy::http::{parse_request, parse_response};

/// The prover id Onyx would store with a submission (integration plan in README.md).
pub const PROVER_ID: &str = "tlsn-v1";
/// The TLSNotary release this experiment is pinned to.
pub const TLSN_VERSION: &str = "0.1.0-alpha.15";
/// The one host proved (rights reviewed: adapters/source-fiscaldata/fixtures/PROVENANCE.md).
pub const SERVER_DOMAIN: &str = "api.fiscaldata.treasury.gov";
/// The pinned Debt to the Penny URL of the source-fiscaldata adapter (record date 2026-10-08).
pub const REQUEST_TARGET: &str = "/services/api/fiscal_service/v2/accounting/od/debt_to_penny?fields=record_date,tot_pub_debt_out_amt&filter=record_date:eq:2026-10-08";
/// User-Agent sent to the server: no person, no contact.
pub const USER_AGENT: &str = "onyx-dev";
/// The synthetic header standing in for an API key. Its value is random per run and hidden.
pub const REDACTED_HEADER: &str = "X-Demo-Key";
/// Where the value sits in the body.
pub const JSON_POINTER: &str = "/data/0/tot_pub_debt_out_amt";
/// Largest presentation Onyx would read.
pub const MAX_PRESENTATION_BYTES: usize = 1 << 20;
/// MPC-TLS caps (bytes of plaintext); exceeding one aborts the session.
pub const MAX_SENT_DATA: usize = 1 << 10;
/// See [`MAX_SENT_DATA`].
pub const MAX_RECV_DATA: usize = 1 << 12;
/// Response headers whose values the prover hides (server session cookies; not needed).
pub const HIDDEN_RESPONSE_HEADERS: &[&str] = &["set-cookie"];

/// What the verifier requires.
#[derive(Clone, Debug)]
pub struct Policy {
    /// The notary's secp256k1 public key, SEC1 bytes.
    pub notary_key: Vec<u8>,
    /// Expected server name.
    pub server_name: String,
    /// Expected request target.
    pub request_target: String,
    /// The one header whose value must be hidden.
    pub redacted_header: String,
    /// Largest hidden value, in bytes.
    pub max_redacted_len: usize,
    /// Current UNIX time.
    pub now: u64,
    /// Oldest session accepted, in seconds before `now`; `None` accepts any age.
    pub max_age_secs: Option<u64>,
}

impl Policy {
    /// The policy for this experiment's host and URL.
    pub fn fiscaldata(notary_key: Vec<u8>, now: u64, max_age_secs: Option<u64>) -> Self {
        Self {
            notary_key,
            server_name: SERVER_DOMAIN.into(),
            request_target: REQUEST_TARGET.into(),
            redacted_header: REDACTED_HEADER.into(),
            max_redacted_len: 256,
            now,
            max_age_secs,
        }
    }
}

/// A verified presentation, as Onyx would record it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub server_name: String,
    pub tls_version: String,
    pub connection_time: u64,
    pub notary_key_hex: String,
    pub request_target: String,
    /// The request as revealed, hidden bytes shown as `X`.
    pub request_shown: String,
    /// The hidden header's name, its value's byte range in the sent transcript, and length.
    pub redacted_header: String,
    pub redacted_range: Range<usize>,
    /// Count of hidden response bytes (Set-Cookie values only).
    pub hidden_response_bytes: usize,
    pub status: u16,
    pub body: Vec<u8>,
    /// `0x` + Keccak-256 of the signed attestation header (bincode). The replay key: the
    /// presentation file is malleable (a flip in an unsigned, unused field such as the body
    /// Merkle proof's `alg` still verifies), the signed header is not.
    pub attestation_id: String,
    /// `0x` + Keccak-256 of the body, the S1 stub's `responseKeccak256`.
    pub body_keccak256: String,
    pub value: String,
    pub record_date: String,
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("presentation is {0} bytes, more than {MAX_PRESENTATION_BYTES}")]
    TooLarge(usize),
    #[error("presentation does not decode: {0}")]
    Decode(String),
    #[error("notary key is not the pinned one (got {alg} {key})")]
    NotaryKey { alg: String, key: String },
    #[error("presentation does not verify: {0}")]
    Presentation(String),
    #[error("no server identity in the presentation")]
    NoServerName,
    #[error("server name {0} is not the expected host")]
    ServerName(String),
    #[error("TLS version {0} is not 1.2")]
    TlsVersion(String),
    #[error("session at {time} is older than {max_age} s (now {now})")]
    Stale { time: u64, max_age: u64, now: u64 },
    #[error("session at {time} is in the future (now {now})")]
    Future { time: u64, now: u64 },
    #[error("no transcript in the presentation")]
    NoTranscript,
    #[error("request: {0}")]
    Request(String),
    #[error("response: {0}")]
    Response(String),
}

/// Unix time skew tolerated for a session "in the future".
const FUTURE_SKEW_SECS: u64 = 300;

/// Decodes a presentation the way [`encode`] wrote it: bincode 1 fixed-int, no trailing bytes.
pub fn decode(bytes: &[u8]) -> Result<Presentation, VerifyError> {
    use bincode::Options;
    if bytes.len() > MAX_PRESENTATION_BYTES {
        return Err(VerifyError::TooLarge(bytes.len()));
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .with_limit(MAX_PRESENTATION_BYTES as u64)
        .deserialize(bytes)
        .map_err(|e| VerifyError::Decode(e.to_string()))
}

/// Encodes a presentation (bincode 1 fixed-int, as the TLSNotary examples write it).
pub fn encode(p: &Presentation) -> anyhow::Result<Vec<u8>> {
    Ok(bincode::serialize(p)?)
}

/// `0x` + lower-case hex Keccak-256.
pub fn keccak_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(Keccak256::digest(bytes)))
}

fn authed_mask(len: usize, ranges: impl Iterator<Item = Range<usize>>) -> Vec<bool> {
    let mut mask = vec![false; len];
    for r in ranges {
        for i in r {
            if i < len {
                mask[i] = true;
            }
        }
    }
    mask
}

fn shown(data: &[u8], mask: &[bool]) -> Vec<u8> {
    data.iter()
        .zip(mask)
        .map(|(b, a)| if *a { *b } else { b'X' })
        .collect()
}

fn all(mask: &[bool], ranges: impl Iterator<Item = Range<usize>>, want: bool) -> bool {
    ranges
        .flat_map(|r| r)
        .all(|i| mask.get(i).copied() == Some(want))
}

/// Runs every check in the module docs.
pub fn verify_presentation(bytes: &[u8], policy: &Policy) -> Result<Checked, VerifyError> {
    let presentation = decode(bytes)?;

    let key = presentation.verifying_key();
    if key.alg != KeyAlgId::K256 || key.data != policy.notary_key {
        return Err(VerifyError::NotaryKey {
            alg: key.alg.to_string(),
            key: hex::encode(&key.data),
        });
    }
    let notary_key_hex = hex::encode(&key.data);

    let PresentationOutput {
        attestation,
        server_name,
        connection_info,
        transcript,
        ..
    } = presentation
        .verify(&CryptoProvider::default())
        .map_err(|e| VerifyError::Presentation(e.to_string()))?;

    let attestation_id = keccak_hex(
        &bincode::serialize(&attestation.header)
            .map_err(|e| VerifyError::Presentation(e.to_string()))?,
    );
    let server_name = server_name.ok_or(VerifyError::NoServerName)?;
    let ServerName::Dns(dns) = &server_name;
    if dns.as_str() != policy.server_name {
        return Err(VerifyError::ServerName(dns.as_str().to_string()));
    }
    if connection_info.version != TlsVersion::V1_2 {
        return Err(VerifyError::TlsVersion(format!(
            "{:?}",
            connection_info.version
        )));
    }
    let time = connection_info.time;
    if time > policy.now + FUTURE_SKEW_SECS {
        return Err(VerifyError::Future {
            time,
            now: policy.now,
        });
    }
    if let Some(max_age) = policy.max_age_secs {
        if policy.now.saturating_sub(time) > max_age {
            return Err(VerifyError::Stale {
                time,
                max_age,
                now: policy.now,
            });
        }
    }

    let transcript: PartialTranscript = transcript.ok_or(VerifyError::NoTranscript)?;

    // Request.
    let sent = transcript.sent_unsafe();
    let sent_mask = authed_mask(sent.len(), transcript.sent_authed().iter());
    let sent_shown = shown(sent, &sent_mask);
    let req =
        parse_request(sent_shown.as_slice()).map_err(|e| VerifyError::Request(e.to_string()))?;
    if req.indices().end() != Some(sent.len()) {
        return Err(VerifyError::Request("bytes after the one request".into()));
    }
    if !all(&sent_mask, req.request.indices().iter(), true) {
        return Err(VerifyError::Request(
            "request line not fully revealed".into(),
        ));
    }
    if req.request.method.as_str() != "GET" {
        return Err(VerifyError::Request("method is not GET".into()));
    }
    if req.request.target.as_str() != policy.request_target {
        return Err(VerifyError::Request(format!(
            "target {} is not the expected one",
            req.request.target.as_str()
        )));
    }
    let mut redacted: Option<Range<usize>> = None;
    let mut host_ok = false;
    let mut sent_expected = vec![true; sent.len()];
    for h in &req.headers {
        let name = h.name.as_str();
        if !all(&sent_mask, h.name.indices().iter(), true) {
            return Err(VerifyError::Request("a header name is hidden".into()));
        }
        if name.eq_ignore_ascii_case(&policy.redacted_header) {
            if redacted.is_some() {
                return Err(VerifyError::Request("redacted header appears twice".into()));
            }
            let r: Vec<Range<usize>> = h.value.indices().iter().collect();
            if r.len() != 1 {
                return Err(VerifyError::Request(
                    "redacted value is not one range".into(),
                ));
            }
            let r = r[0].clone();
            if r.is_empty() || r.len() > policy.max_redacted_len {
                return Err(VerifyError::Request(format!(
                    "redacted value is {} bytes (allowed 1..={})",
                    r.len(),
                    policy.max_redacted_len
                )));
            }
            if !all(&sent_mask, std::iter::once(r.clone()), false) {
                return Err(VerifyError::Request(
                    "redacted header value is (partly) revealed".into(),
                ));
            }
            // The verified transcript holds no plaintext there: TLSNotary fills unrevealed
            // bytes with zeros. A non-zero byte would mean the library leaked or was bypassed.
            if sent[r.clone()].iter().any(|b| *b != 0) {
                return Err(VerifyError::Request("hidden bytes are not empty".into()));
            }
            for i in r.clone() {
                sent_expected[i] = false;
            }
            redacted = Some(r);
        } else if name.eq_ignore_ascii_case("host") {
            host_ok = h.value.as_bytes().as_ref() == policy.server_name.as_bytes();
        }
    }
    let redacted_range =
        redacted.ok_or_else(|| VerifyError::Request("no redacted header".into()))?;
    if !host_ok {
        return Err(VerifyError::Request(
            "Host header is not the expected host".into(),
        ));
    }
    // Hidden bytes are exactly the redacted value: nothing else may be hidden.
    if sent_mask != sent_expected {
        return Err(VerifyError::Request(
            "bytes other than the redacted value are hidden".into(),
        ));
    }

    // Response.
    let recv = transcript.received_unsafe();
    let recv_mask = authed_mask(recv.len(), transcript.received_authed().iter());
    let recv_shown = shown(recv, &recv_mask);
    let resp =
        parse_response(recv_shown.as_slice()).map_err(|e| VerifyError::Response(e.to_string()))?;
    if resp.indices().end() != Some(recv.len()) {
        return Err(VerifyError::Response("bytes after the one response".into()));
    }
    if !all(&recv_mask, resp.status.indices().iter(), true) {
        return Err(VerifyError::Response(
            "status line not fully revealed".into(),
        ));
    }
    let status: u16 = resp
        .status
        .code
        .as_str()
        .parse()
        .map_err(|_| VerifyError::Response("status code".into()))?;
    if status != 200 {
        return Err(VerifyError::Response(format!("status {status}")));
    }
    let mut recv_expected = vec![true; recv.len()];
    let mut content_length: Option<usize> = None;
    for h in &resp.headers {
        if !all(&recv_mask, h.name.indices().iter(), true) {
            return Err(VerifyError::Response("a header name is hidden".into()));
        }
        let name = h.name.as_str().to_ascii_lowercase();
        if HIDDEN_RESPONSE_HEADERS.contains(&name.as_str()) {
            // May be hidden; must then be hidden whole.
            let any_hidden = h
                .value
                .indices()
                .iter()
                .flat_map(|r| r)
                .any(|i| !recv_mask[i]);
            if any_hidden {
                if !all(&recv_mask, h.value.indices().iter(), false) {
                    return Err(VerifyError::Response(
                        "a Set-Cookie value is partly hidden".into(),
                    ));
                }
                for i in h.value.indices().iter().flat_map(|r| r) {
                    recv_expected[i] = false;
                }
            }
        } else if name == "content-length" {
            if content_length.is_some() {
                return Err(VerifyError::Response("two Content-Length headers".into()));
            }
            let v = String::from_utf8_lossy(h.value.as_bytes().as_ref()).to_string();
            content_length = Some(
                v.trim()
                    .parse()
                    .map_err(|_| VerifyError::Response("Content-Length".into()))?,
            );
        } else if name == "transfer-encoding" || name == "content-encoding" {
            return Err(VerifyError::Response(format!("{name} is not supported")));
        }
    }
    if recv_mask != recv_expected {
        return Err(VerifyError::Response(
            "bytes other than Set-Cookie values are hidden".into(),
        ));
    }
    let hidden_response_bytes = recv_expected.iter().filter(|a| !**a).count();
    let body = resp
        .body
        .as_ref()
        .ok_or_else(|| VerifyError::Response("no body".into()))?;
    let body_bytes: Vec<u8> = recv[body
        .indices()
        .min()
        .ok_or_else(|| VerifyError::Response("empty body".into()))?
        ..body.indices().end().unwrap_or(0)]
        .to_vec();
    if content_length != Some(body_bytes.len()) {
        return Err(VerifyError::Response(format!(
            "Content-Length {content_length:?} but body is {} bytes",
            body_bytes.len()
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| VerifyError::Response(format!("body is not JSON: {e}")))?;
    let value = json
        .pointer(JSON_POINTER)
        .and_then(|v| v.as_str())
        .ok_or_else(|| VerifyError::Response(format!("no string at {JSON_POINTER}")))?
        .to_string();
    let record_date = json
        .pointer("/data/0/record_date")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Checked {
        server_name: dns.as_str().to_string(),
        tls_version: "TLS 1.2".into(),
        connection_time: time,
        notary_key_hex,
        attestation_id,
        request_target: req.request.target.as_str().to_string(),
        request_shown: String::from_utf8_lossy(&sent_shown).to_string(),
        redacted_header: policy.redacted_header.clone(),
        redacted_range,
        hidden_response_bytes,
        status,
        body_keccak256: keccak_hex(&body_bytes),
        body: body_bytes,
        value,
        record_date,
    })
}
