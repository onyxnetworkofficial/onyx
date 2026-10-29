//! A submission's details and their checks (ADR-0038 item 4).
//!
//! Byte caps on every field, https-only URLs with a host name (no user info,
//! no IP literal, no fragment), an RFC 6901 JSON pointer, and no personal
//! contact: there is no e-mail or contact field (unknown fields are refused),
//! and free text that holds an e-mail address is refused too.

use crate::answers::{classify, Answers, AnswersError, Kind, Outcome, Reason, Route};
use crate::fetch::is_blocked_host;
use borsh::BorshSerialize;
use onyx_core::{keccak256, Hash32};
use serde::{Deserialize, Serialize};

/// Largest title, in bytes.
pub const MAX_TITLE: usize = 120;
/// Largest description, in bytes.
pub const MAX_DESCRIPTION: usize = 2_000;
/// Largest source name, in bytes.
pub const MAX_SOURCE_NAME: usize = 120;
/// Largest URL, in bytes.
pub const MAX_URL: usize = 512;
/// Largest JSON pointer, in bytes.
pub const MAX_POINTER: usize = 256;
/// Largest unit, in bytes.
pub const MAX_UNIT: usize = 32;
/// Largest host name, in bytes (RFC 1035).
pub const MAX_HOST: usize = 253;
/// Domain tag of a submission's hash.
pub const HASH_TAG: &[u8] = b"onyx-submission:v1";
/// Payload-bearing submissions use a distinct hash domain and serialization version.
pub const HASH_TAG_V2: &[u8] = b"onyx-submission:v2";

/// One submission, as the website sends it and as the journal keeps it.
/// JSON keys are camelCase; unknown keys (an `email`, a `phone`) are refused.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, BorshSerialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Submission {
    /// The onboarding answers (their `changes` is the update frequency).
    pub answers: Answers,
    /// A short name for the data, 1 to 120 bytes.
    pub title: String,
    /// What the data is and what it means, 1 to 2,000 bytes.
    pub description: String,
    /// Who publishes it, 1 to 120 bytes.
    pub source_name: String,
    /// Where a person can read about it (https).
    pub source_url: String,
    /// Optional bounded own-data sample. Public receipt/status routes never expose it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_payload: Option<DataPayload>,
    /// Where a machine can fetch it (https), if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    /// The value inside the endpoint's JSON (RFC 6901), with an endpoint only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_pointer: Option<String>,
    /// The unit, e.g. `USD`, up to 32 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// For a keyed source: the request with the key left out, as
    /// [`KEY_PLACEHOLDER`] exactly once (https). The key itself never appears
    /// in a submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_template: Option<String>,
    /// The source's own id of the series, e.g. `CPIAUCSL`, up to 64 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_id: Option<String>,
    /// For a keyed source: what the submitter's side proved about the
    /// response (S2). In S1 only the labelled stub is taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_proof: Option<KeyProof>,
}

/// A small exact-text CSV or JSON sample (ADR-0040).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, BorshSerialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataPayload {
    /// `csv` or `json`.
    pub format: String,
    /// Exact UTF-8 source text.
    pub content: String,
    /// Exact UTF-8 byte count.
    pub bytes: u32,
    /// Lowercase `0x` Keccak-256 over the exact content bytes.
    pub keccak256: String,
}

/// Pre-U3 field layout retained so existing no-payload submission hashes and
/// reviewer-ledger replay remain stable after ADR-0040.
#[derive(BorshSerialize)]
struct SubmissionV1<'a> {
    answers: &'a Answers,
    title: &'a str,
    description: &'a str,
    source_name: &'a str,
    source_url: &'a str,
    endpoint_url: &'a Option<String>,
    json_pointer: &'a Option<String>,
    unit: &'a Option<String>,
    endpoint_template: &'a Option<String>,
    series_id: &'a Option<String>,
    key_proof: &'a Option<KeyProof>,
}

/// The placeholder where the key goes in [`Submission::endpoint_template`].
pub const KEY_PLACEHOLDER: &str = "{api_key}";
/// The only prover S1 takes: a stub that proves nothing and says so.
pub const STUB_PROVER: &str = "stub-s2";
/// Largest `responseBytes` in a key proof: 2^53 - 1, the largest integer
/// every JSON reader (the website's too) holds exactly (S1-R1-4).
pub const MAX_RESPONSE_BYTES: u64 = (1 << 53) - 1;
/// Largest series id, in bytes.
pub const MAX_SERIES_ID: usize = 64;
/// Maximum exact UTF-8 bytes in a contributor-supplied dataset sample.
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024;
/// Query parameter names that carry a secret, matched whole; a URL in a
/// submission may not give them a value (other than [`KEY_PLACEHOLDER`] in a
/// template). With [`SECRET_PARTS`] and the `x-amz-` prefix (S1-R1-5).
pub const SECRET_PARAMS: &[&str] = &["appid", "auth", "authorization", "pwd", "sig"];
/// A parameter name holding any of these is a secret (`api_key`, `app_key`,
/// `api_token`, `client_secret`, `X-Amz-Signature`, ...).
pub const SECRET_PARTS: &[&str] = &["credential", "key", "passw", "secret", "signature", "token"];

/// The proof step's output for a keyed source (ADR-0038 item 9). Onyx gets
/// this and never the key or the response. In S1 the prover is a stub:
/// `prover` is [`STUB_PROVER`], `real` is false, and nothing is proved.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, BorshSerialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyProof {
    /// Which prover made it.
    pub prover: String,
    /// Whether it proves anything. False for the stub; a submission saying
    /// true is refused until a real prover exists (S2).
    pub real: bool,
    /// Keccak-256 of the response body the submitter's side received, when
    /// it hashed one (`0x` and 64 hex digits). Not a proof: anyone can hash
    /// anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_keccak256: Option<String>,
    /// That body's length in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<u64>,
    /// The proof itself (none in S1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

/// Why a submission is refused. Each has a stable [`SubmissionError::code`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SubmissionError {
    /// Not the submission's JSON shape (a missing or unknown field, a wrong
    /// type or choice).
    #[error("not a submission: {0}")]
    BadJson(String),
    /// The answers' shape.
    #[error(transparent)]
    Answers(#[from] AnswersError),
    /// A required text is empty.
    #[error("{0} is empty")]
    Empty(&'static str),
    /// A field is over its byte cap.
    #[error("{field} is over {max} bytes")]
    TooLong {
        /// The field.
        field: &'static str,
        /// Its cap.
        max: usize,
    },
    /// Control characters (only line breaks in the description).
    #[error("{0} holds control characters")]
    Control(&'static str),
    /// Free text holds an e-mail address.
    #[error("{0} holds an e-mail address; Onyx takes no personal contact details")]
    Email(&'static str),
    /// A URL is not an https URL with a plain host name.
    #[error("{field} must be an https URL with a host name: {why}")]
    Url {
        /// The field.
        field: &'static str,
        /// What is wrong.
        why: &'static str,
    },
    /// The JSON pointer is not RFC 6901.
    #[error("the JSON pointer must be empty or start with '/', with '~' only as ~0 or ~1")]
    Pointer,
    /// A pointer without an endpoint.
    #[error("a JSON pointer needs an endpoint URL")]
    PointerWithoutEndpoint,
    /// A URL gives a value to a secret parameter (a key, token, password).
    #[error("{0} carries a secret (a key, token or password); never put your key in a submission")]
    SecretInUrl(&'static str),
    /// The endpoint template does not hold the key placeholder exactly once.
    #[error("the endpoint template must hold {{api_key}} exactly once where the key goes")]
    Template,
    /// A template or a key proof on a source that is not keyed.
    #[error("an endpoint template and a key proof go with a keyed source only")]
    NotKeyed,
    /// The series id is too long or has characters outside `A-Z a-z 0-9 . _ - :`.
    #[error("the series id must be 1 to 64 characters from A-Z, a-z, 0-9 and . _ - :")]
    SeriesId,
    /// A key proof other than the labelled stub, or a malformed one.
    #[error("only the labelled stub prover (stub-s2, real: false) is taken until a real prover exists (S2)")]
    Proof,
    /// Source URLs may be omitted only by own-data submissions.
    #[error("a source URL is required unless this is your own data")]
    MissingSourceUrl,
    /// A user payload is allowed only for own data with own-data rights.
    #[error("a data file is only accepted for your own data when you have permission to share it")]
    PayloadRoute,
    /// The payload is malformed or trips an intake safety guard.
    #[error("data sample refused: {0}")]
    Payload(&'static str),
}

impl SubmissionError {
    /// The stable code (the API and the website use it).
    pub fn code(&self) -> &'static str {
        match self {
            SubmissionError::BadJson(_) => "bad_json",
            SubmissionError::Answers(e) => e.code(),
            SubmissionError::Empty(_) => "empty_field",
            SubmissionError::TooLong { .. } => "field_too_long",
            SubmissionError::Control(_) => "control_characters",
            SubmissionError::Email(_) => "personal_contact",
            SubmissionError::Url { .. } => "bad_url",
            SubmissionError::Pointer => "bad_json_pointer",
            SubmissionError::PointerWithoutEndpoint => "pointer_without_endpoint",
            SubmissionError::SecretInUrl(_) => "secret_in_url",
            SubmissionError::Template => "bad_template",
            SubmissionError::NotKeyed => "not_keyed",
            SubmissionError::SeriesId => "bad_series_id",
            SubmissionError::Proof => "proof_not_supported",
            SubmissionError::MissingSourceUrl => "source_url_required",
            SubmissionError::PayloadRoute => "payload_route",
            SubmissionError::Payload(_) => "payload_invalid",
        }
    }
}

/// A checked https URL: lower-case host name, no user info, no IP literal, no
/// fragment, port 1-65535 when given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpsUrl {
    /// The URL as given.
    pub url: String,
    /// The host, lower case.
    pub host: String,
    /// The port, when one is written.
    pub port: Option<u16>,
    /// Path and query (`/` when empty).
    pub path_and_query: String,
}

impl HttpsUrl {
    /// Parse `s` (see the type). `why` on refusal.
    pub fn parse(s: &str) -> Result<HttpsUrl, &'static str> {
        if s.len() > MAX_URL {
            return Err("over 512 bytes");
        }
        if !s.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err("only visible ASCII, no spaces");
        }
        if s.bytes().any(|b| b"\\\"<>^`{|}".contains(&b)) {
            return Err("characters a URL may not hold");
        }
        let rest = s.strip_prefix("https://").ok_or("https only")?;
        if s.contains('#') {
            return Err("no fragment");
        }
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.contains('@') {
            return Err("no user name or password in the URL");
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("bad port");
                }
                let n: u32 = p.parse().map_err(|_| "bad port")?;
                let port = u16::try_from(n).ok().filter(|n| *n > 0).ok_or("bad port")?;
                (h, Some(port))
            }
            None => (authority, None),
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty() || host.len() > MAX_HOST {
            return Err("no host");
        }
        if host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok() {
            return Err("a host name, not an IP address");
        }
        let labels_ok = host.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
        if !labels_ok || !host.contains('.') {
            return Err("a public host name (letters, digits, dots, hyphens)");
        }
        // A last label of digits only is an IP address in disguise to some
        // resolvers (e.g. `0x7f.1`, `127.1`).
        if host
            .rsplit('.')
            .next()
            .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()) || l.starts_with("0x"))
        {
            return Err("a host name, not an IP address");
        }
        let path_and_query = if tail.is_empty() {
            "/".to_string()
        } else if tail.starts_with('?') {
            format!("/{tail}")
        } else {
            tail.to_string()
        };
        Ok(HttpsUrl {
            url: s.to_string(),
            host,
            port,
            path_and_query,
        })
    }
}

/// Whether `text` holds something shaped like an e-mail address
/// (`local@domain.tld`) or a `mailto:` link.
pub fn has_email(text: &str) -> bool {
    if text.to_ascii_lowercase().contains("mailto:") {
        return true;
    }
    let local = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
    let domain = |b: u8| b.is_ascii_alphanumeric() || b".-".contains(&b);
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'@' {
            continue;
        }
        let before = i > 0 && local(bytes[i - 1]);
        let after: Vec<u8> = bytes[i + 1..]
            .iter()
            .copied()
            .take_while(|b| domain(*b))
            .collect();
        let after = String::from_utf8_lossy(&after);
        let after = after.trim_end_matches(['.', '-']);
        let dotted = after.rsplit_once('.').is_some_and(|(h, tld)| {
            !h.is_empty() && tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic())
        });
        if before && dotted {
            return true;
        }
    }
    false
}

/// Unicode White_Space (the same list on the website, S1-R1-4).
const WHITE_SPACE: &[(u32, u32)] = &[
    (0x09, 0x0d),
    (0x20, 0x20),
    (0x85, 0x85),
    (0xa0, 0xa0),
    (0x1680, 0x1680),
    (0x2000, 0x200a),
    (0x2028, 0x2029),
    (0x202f, 0x202f),
    (0x205f, 0x205f),
    (0x3000, 0x3000),
];
/// Unicode general category Cf (format: invisible), the same list on the website.
const FORMAT: &[(u32, u32)] = &[
    (0xad, 0xad),
    (0x600, 0x605),
    (0x61c, 0x61c),
    (0x6dd, 0x6dd),
    (0x70f, 0x70f),
    (0x890, 0x891),
    (0x8e2, 0x8e2),
    (0x180e, 0x180e),
    (0x200b, 0x200f),
    (0x202a, 0x202e),
    (0x2060, 0x2064),
    (0x2066, 0x206f),
    (0xfeff, 0xfeff),
    (0xfff9, 0xfffb),
    (0x110bd, 0x110bd),
    (0x110cd, 0x110cd),
    (0x13430, 0x1343f),
    (0x1bca0, 0x1bca3),
    (0x1d173, 0x1d17a),
    (0xe0001, 0xe0001),
    (0xe0020, 0xe007f),
];

fn in_ranges(c: char, r: &[(u32, u32)]) -> bool {
    let n = u32::from(c);
    r.iter().any(|(a, b)| (*a..=*b).contains(&n))
}

/// Whether `s` holds nothing visible: only white space and format characters.
pub fn is_blank(s: &str) -> bool {
    s.chars()
        .all(|c| in_ranges(c, WHITE_SPACE) || in_ranges(c, FORMAT))
}

fn text(field: &'static str, s: &str, max: usize, lines: bool) -> Result<(), SubmissionError> {
    if is_blank(s) {
        return Err(SubmissionError::Empty(field));
    }
    opt_text(field, s, max, lines)
}

fn opt_text(field: &'static str, s: &str, max: usize, lines: bool) -> Result<(), SubmissionError> {
    if s.len() > max {
        return Err(SubmissionError::TooLong { field, max });
    }
    // Control characters (line breaks and tabs only where lines are allowed),
    // and the byte-order mark anywhere (S1-R1-4).
    if s.chars().any(|c| {
        (c.is_control() && !(lines && (c == '\n' || c == '\r' || c == '\t'))) || c == '\u{feff}'
    }) {
        return Err(SubmissionError::Control(field));
    }
    if has_email(s) {
        return Err(SubmissionError::Email(field));
    }
    Ok(())
}

fn validate_data_payload(p: &DataPayload) -> Result<(), SubmissionError> {
    if p.content.is_empty() || p.content.len() > MAX_PAYLOAD_BYTES {
        return Err(SubmissionError::Payload("empty or over 8192 UTF-8 bytes"));
    }
    if p.bytes as usize != p.content.len()
        || p.keccak256 != format!("0x{}", hex::encode(keccak256(p.content.as_bytes()).0))
    {
        return Err(SubmissionError::Payload(
            "byte count or digest does not match content",
        ));
    }
    if p.content.contains('\0') || p.content.contains('\u{feff}') {
        return Err(SubmissionError::Payload(
            "control or invisible characters are not accepted",
        ));
    }
    let lower = p.content.to_ascii_lowercase();
    if (lower.contains('<') && lower.contains('>'))
        || ["<!doctype", "<?php", "javascript:"]
            .iter()
            .any(|needle| lower.contains(needle))
    {
        return Err(SubmissionError::Payload(
            "markup or executable content is not accepted",
        ));
    }
    if [
        "-----begin ",
        "bearer ",
        "api_key",
        "apikey",
        "access_token",
        "client_secret",
        "authorization",
        "password",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Err(SubmissionError::Payload("possible credential detected"));
    }
    if has_email(&p.content) {
        return Err(SubmissionError::Payload(
            "personal contact data is not accepted",
        ));
    }
    match p.format.as_str() {
        "json" => {
            if has_duplicate_json_keys(&p.content) {
                return Err(SubmissionError::Payload("duplicate JSON object key"));
            }
            let value: serde_json::Value = serde_json::from_str(&p.content)
                .map_err(|_| SubmissionError::Payload("invalid JSON"))?;
            fn check_credentials(v: &serde_json::Value) -> Result<(), SubmissionError> {
                let markers = [
                    "-----begin ",
                    "bearer ",
                    "api_key",
                    "apikey",
                    "access_token",
                    "client_secret",
                    "authorization",
                    "password",
                ];
                let contains_marker = |s: &str| {
                    let lower = s.to_ascii_lowercase();
                    markers.iter().any(|needle| lower.contains(needle))
                };
                match v {
                    serde_json::Value::Object(map) => {
                        for (k, value) in map {
                            if contains_marker(k) {
                                return Err(SubmissionError::Payload(
                                    "possible credential detected",
                                ));
                            }
                            check_credentials(value)?;
                        }
                    }
                    serde_json::Value::Array(items) => {
                        for value in items {
                            check_credentials(value)?;
                        }
                    }
                    serde_json::Value::String(s) if contains_marker(s) => {
                        return Err(SubmissionError::Payload("possible credential detected"))
                    }
                    _ => {}
                }
                Ok(())
            }
            check_credentials(&value)?;
            fn check(v: &serde_json::Value) -> Result<(), SubmissionError> {
                match v {
                    serde_json::Value::Object(map) => {
                        for (k, val) in map {
                            let key = k.to_ascii_lowercase();
                            if [
                                "email",
                                "phone",
                                "person",
                                "person_name",
                                "full_name",
                                "first_name",
                                "last_name",
                                "customer",
                                "user",
                                "address",
                                "ssn",
                                "social_security",
                                "birth_date",
                                "dob",
                            ]
                            .contains(&key.as_str())
                            {
                                return Err(SubmissionError::Payload(
                                    "personal-record field detected",
                                ));
                            }
                            check(val)?;
                        }
                    }
                    serde_json::Value::Array(items) => {
                        for val in items {
                            check(val)?;
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            check(&value)?;
        }
        "csv" => {
            let rows = parse_csv_cells(&p.content)?;
            if rows.is_empty() || rows[0].is_empty() {
                return Err(SubmissionError::Payload("CSV needs a header row"));
            }
            let width = rows[0].len();
            if rows.iter().any(|r| r.len() != width) {
                return Err(SubmissionError::Payload(
                    "CSV rows must have the same number of columns",
                ));
            }
            let mut headers = std::collections::BTreeSet::new();
            if rows[0]
                .iter()
                .any(|h| h.trim().is_empty() || !headers.insert(h.trim().to_ascii_lowercase()))
            {
                return Err(SubmissionError::Payload(
                    "CSV headers must be non-empty and unique",
                ));
            }
            for cell in rows.iter().flatten() {
                let value = cell.trim_start();
                let lower = value.to_ascii_lowercase();
                let formula = value.starts_with(['=', '+', '@'])
                    || (value.starts_with('-')
                        && value
                            .parse::<f64>()
                            .map_or(true, |number| !number.is_finite()));
                if formula || lower.starts_with("\t=") || lower.starts_with("\r=") {
                    return Err(SubmissionError::Payload(
                        "spreadsheet formulas are not accepted",
                    ));
                }
            }
            for key in &rows[0] {
                let k = key.to_ascii_lowercase();
                if [
                    "email",
                    "phone",
                    "person",
                    "person_name",
                    "full_name",
                    "first_name",
                    "last_name",
                    "customer",
                    "user",
                    "address",
                    "ssn",
                    "social_security",
                    "birth_date",
                    "dob",
                ]
                .contains(&k.trim())
                {
                    return Err(SubmissionError::Payload("personal-record field detected"));
                }
            }
        }
        _ => return Err(SubmissionError::Payload("format must be csv or json")),
    }
    Ok(())
}

fn has_duplicate_json_keys(s: &str) -> bool {
    fn ws(b: &[u8], i: &mut usize) {
        while b.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
    }
    fn string_end(b: &[u8], i: &mut usize) -> Option<usize> {
        if b.get(*i) != Some(&b'"') {
            return None;
        }
        let start = *i;
        *i += 1;
        while let Some(&c) = b.get(*i) {
            if c == b'"' {
                *i += 1;
                return Some(start);
            }
            if c == b'\\' {
                *i += 1;
            }
            *i += 1;
        }
        None
    }
    fn value(s: &str, b: &[u8], i: &mut usize) -> bool {
        ws(b, i);
        match b.get(*i).copied() {
            Some(b'{') => {
                *i += 1;
                ws(b, i);
                let mut keys = std::collections::BTreeSet::new();
                if b.get(*i) == Some(&b'}') {
                    *i += 1;
                    return false;
                }
                loop {
                    ws(b, i);
                    let Some(start) = string_end(b, i) else {
                        return false;
                    };
                    let Ok(key) = serde_json::from_str::<String>(&s[start..*i]) else {
                        return false;
                    };
                    if !keys.insert(key) {
                        return true;
                    }
                    ws(b, i);
                    if b.get(*i) != Some(&b':') {
                        return false;
                    }
                    *i += 1;
                    if value(s, b, i) {
                        return true;
                    }
                    ws(b, i);
                    match b.get(*i) {
                        Some(b',') => *i += 1,
                        Some(b'}') => {
                            *i += 1;
                            return false;
                        }
                        _ => return false,
                    }
                }
            }
            Some(b'[') => {
                *i += 1;
                ws(b, i);
                if b.get(*i) == Some(&b']') {
                    *i += 1;
                    return false;
                }
                loop {
                    if value(s, b, i) {
                        return true;
                    }
                    ws(b, i);
                    match b.get(*i) {
                        Some(b',') => *i += 1,
                        Some(b']') => {
                            *i += 1;
                            return false;
                        }
                        _ => return false,
                    }
                }
            }
            Some(b'"') => string_end(b, i).is_none(),
            Some(_) => {
                while b
                    .get(*i)
                    .is_some_and(|c| !c.is_ascii_whitespace() && !b",]}".contains(c))
                {
                    *i += 1;
                }
                false
            }
            None => false,
        }
    }
    value(s, s.as_bytes(), &mut 0)
}

fn parse_csv_cells(s: &str) -> Result<Vec<Vec<String>>, SubmissionError> {
    let mut rows = vec![Vec::new()];
    let mut cell = String::new();
    let mut chars = s.chars().peekable();
    let mut quoted = false;
    let mut after_quote = false;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => {
                    quoted = false;
                    after_quote = true;
                }
                _ => cell.push(c),
            }
            continue;
        }
        if after_quote {
            match c {
                ',' => {
                    rows.last_mut()
                        .ok_or(SubmissionError::Payload("invalid CSV"))?
                        .push(std::mem::take(&mut cell));
                    after_quote = false;
                }
                '\n' => {
                    rows.last_mut()
                        .ok_or(SubmissionError::Payload("invalid CSV"))?
                        .push(std::mem::take(&mut cell));
                    rows.push(Vec::new());
                    after_quote = false;
                }
                '\r' if chars.peek() == Some(&'\n') => {
                    chars.next();
                    rows.last_mut()
                        .ok_or(SubmissionError::Payload("invalid CSV"))?
                        .push(std::mem::take(&mut cell));
                    rows.push(Vec::new());
                    after_quote = false;
                }
                _ => {
                    return Err(SubmissionError::Payload(
                        "invalid character after a quoted CSV field",
                    ))
                }
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() => quoted = true,
            '"' => return Err(SubmissionError::Payload("quote in an unquoted CSV field")),
            ',' => {
                rows.last_mut()
                    .ok_or(SubmissionError::Payload("invalid CSV"))?
                    .push(std::mem::take(&mut cell));
            }
            '\n' => {
                rows.last_mut()
                    .ok_or(SubmissionError::Payload("invalid CSV"))?
                    .push(std::mem::take(&mut cell));
                rows.push(Vec::new());
            }
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
                rows.last_mut()
                    .ok_or(SubmissionError::Payload("invalid CSV"))?
                    .push(std::mem::take(&mut cell));
                rows.push(Vec::new());
            }
            '\r' => {
                return Err(SubmissionError::Payload(
                    "CSV line endings must be LF or CRLF",
                ))
            }
            _ => cell.push(c),
        }
    }
    if quoted {
        return Err(SubmissionError::Payload("unclosed CSV quote"));
    }
    if !cell.is_empty() || rows.last().is_some_and(|r| !r.is_empty()) {
        rows.last_mut()
            .ok_or(SubmissionError::Payload("invalid CSV"))?
            .push(cell);
    } else if rows.len() > 1 {
        rows.pop();
    }
    Ok(rows)
}

/// A query parameter name as a server reads it: percent-decoded, ASCII
/// lower case, without an array suffix (`api_key[]`, `a[b]`).
pub fn param_name(raw: &str) -> Vec<u8> {
    let b = raw.as_bytes();
    let hex = |x: u8| char::from(x).to_digit(16);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hl = (b[i] == b'%')
            .then(|| Some((hex(*b.get(i + 1)?)?, hex(*b.get(i + 2)?)?)))
            .flatten();
        match hl {
            Some((h, l)) => {
                // Two hex digits make one byte (each is below 16).
                out.push(u8::try_from(h * 16 + l).unwrap_or(0));
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    if let Some(p) = out.iter().position(|c| *c == b'[') {
        out.truncate(p);
    }
    out.make_ascii_lowercase();
    out
}

/// Whether a (decoded) parameter name names a secret.
pub fn is_secret_name(name: &[u8]) -> bool {
    SECRET_PARAMS.iter().any(|p| name == p.as_bytes())
        || name.starts_with(b"x-amz-")
        || SECRET_PARTS
            .iter()
            .any(|p| name.windows(p.len()).any(|w| w == p.as_bytes()))
}

/// Whether the query of URL `u` gives a value to a secret parameter
/// (pairs split on `&` and `;`; names as [`param_name`] reads them).
pub fn has_secret_param(u: &str) -> bool {
    let Some((_, query)) = u.split_once('?') else {
        return false;
    };
    query.split(['&', ';']).any(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        !value.is_empty() && is_secret_name(&param_name(name))
    })
}

/// Whether `p` is an RFC 6901 JSON pointer: empty, or `/`-separated tokens in
/// which `~` appears only as `~0` or `~1`.
pub fn is_json_pointer(p: &str) -> bool {
    if p.is_empty() {
        return true;
    }
    if !p.starts_with('/') || p.chars().any(char::is_control) {
        return false;
    }
    let b = p.as_bytes();
    b.iter()
        .enumerate()
        .all(|(i, c)| *c != b'~' || matches!(b.get(i + 1), Some(b'0' | b'1')))
}

impl Submission {
    /// Parse JSON `bytes` and check every rule ([`Submission::validate`]).
    pub fn parse(bytes: &[u8]) -> Result<Submission, SubmissionError> {
        let s: Submission =
            serde_json::from_slice(bytes).map_err(|e| SubmissionError::BadJson(e.to_string()))?;
        s.validate()?;
        Ok(s)
    }

    /// Every rule above. The first broken rule, in field order.
    pub fn validate(&self) -> Result<(), SubmissionError> {
        self.answers.validate()?;
        text("title", &self.title, MAX_TITLE, false)?;
        text("description", &self.description, MAX_DESCRIPTION, true)?;
        text("sourceName", &self.source_name, MAX_SOURCE_NAME, false)?;
        if self.source_url.is_empty() {
            if self.answers.what != Kind::Own || self.answers.rights != crate::Rights::OwnData {
                return Err(SubmissionError::MissingSourceUrl);
            }
        } else {
            HttpsUrl::parse(&self.source_url).map_err(|why| SubmissionError::Url {
                field: "sourceUrl",
                why,
            })?;
        }
        if let Some(payload) = &self.data_payload {
            if self.answers.what != Kind::Own || self.answers.rights != crate::Rights::OwnData {
                return Err(SubmissionError::PayloadRoute);
            }
            validate_data_payload(payload)?;
        }
        if let Some(u) = &self.endpoint_url {
            HttpsUrl::parse(u).map_err(|why| SubmissionError::Url {
                field: "endpointUrl",
                why,
            })?;
        }
        if let Some(t) = &self.endpoint_template {
            if t.matches(KEY_PLACEHOLDER).count() != 1 {
                return Err(SubmissionError::Template);
            }
            let filled = t.replace(KEY_PLACEHOLDER, "KEY");
            HttpsUrl::parse(&filled).map_err(|why| SubmissionError::Url {
                field: "endpointTemplate",
                why,
            })?;
            // The placeholder's own parameter is the one secret allowed.
            let without = t.replace(KEY_PLACEHOLDER, "");
            if has_secret_param(&without) {
                return Err(SubmissionError::SecretInUrl("endpointTemplate"));
            }
        }
        for (field, u) in [
            (
                "sourceUrl",
                (!self.source_url.is_empty()).then_some(&self.source_url),
            ),
            ("endpointUrl", self.endpoint_url.as_ref()),
        ] {
            if u.is_some_and(|u| has_secret_param(u)) {
                return Err(SubmissionError::SecretInUrl(field));
            }
        }
        let keyed = self.answers.what == Kind::KeyedApi;
        if !keyed && (self.endpoint_template.is_some() || self.key_proof.is_some()) {
            return Err(SubmissionError::NotKeyed);
        }
        if let Some(id) = &self.series_id {
            let ok = !id.is_empty()
                && id.len() <= MAX_SERIES_ID
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b));
            if !ok {
                return Err(SubmissionError::SeriesId);
            }
        }
        if let Some(p) = &self.key_proof {
            let hash_ok = p.response_keccak256.as_ref().is_none_or(|h| {
                h.len() == 66
                    && h.starts_with("0x")
                    && h[2..]
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            });
            let bytes_ok = p.response_bytes.is_none_or(|n| n <= MAX_RESPONSE_BYTES);
            if p.prover != STUB_PROVER
                || !bytes_ok
                || p.real
                || p.proof.is_some()
                || !hash_ok
                || p.response_bytes.is_some() != p.response_keccak256.is_some()
            {
                return Err(SubmissionError::Proof);
            }
        }
        if let Some(p) = &self.json_pointer {
            if self.endpoint_url.is_none() && self.endpoint_template.is_none() {
                return Err(SubmissionError::PointerWithoutEndpoint);
            }
            if p.len() > MAX_POINTER {
                return Err(SubmissionError::TooLong {
                    field: "jsonPointer",
                    max: MAX_POINTER,
                });
            }
            if !is_json_pointer(p) {
                return Err(SubmissionError::Pointer);
            }
        }
        if let Some(u) = &self.unit {
            opt_text("unit", u, MAX_UNIT, false)?;
        }
        Ok(())
    }

    /// The route for this submission: [`classify`] of the answers, then held
    /// (never published) when any URL names a blocked host
    /// ([`crate::fetch::BLOCKED_HOSTS`]), with [`Reason::SourceBlocked`] first.
    pub fn route(&self) -> Route {
        let mut r = classify(&self.answers);
        if r.outcome == Outcome::Refused {
            return r;
        }
        let template = self
            .endpoint_template
            .as_ref()
            .map(|t| t.replace(KEY_PLACEHOLDER, "KEY"));
        let blocked = [
            (!self.source_url.is_empty()).then_some(&self.source_url),
            self.endpoint_url.as_ref(),
            template.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|u| HttpsUrl::parse(u).ok())
        .any(|u| is_blocked_host(&u.host));
        if blocked {
            r.outcome = Outcome::Held;
            r.reasons.insert(0, Reason::SourceBlocked);
        }
        r
    }

    /// The endpoint, parsed (`None` without one or when it does not parse).
    pub fn endpoint(&self) -> Option<HttpsUrl> {
        HttpsUrl::parse(self.endpoint_url.as_deref()?).ok()
    }

    /// Keccak-256 of the domain tag and the Borsh bytes (ADR-0002, ADR-0003):
    /// the submission's content hash.
    pub fn hash(&self) -> Hash32 {
        let mut bytes = if self.data_payload.is_some() {
            HASH_TAG_V2.to_vec()
        } else {
            HASH_TAG.to_vec()
        };
        // Keep the old field layout for metadata-only entries so U2 journal
        // hashes and receipts survive the U3 schema addition.
        let encoded = if self.data_payload.is_some() {
            borsh::to_vec(self)
        } else {
            borsh::to_vec(&SubmissionV1 {
                answers: &self.answers,
                title: &self.title,
                description: &self.description,
                source_name: &self.source_name,
                source_url: &self.source_url,
                endpoint_url: &self.endpoint_url,
                json_pointer: &self.json_pointer,
                unit: &self.unit,
                endpoint_template: &self.endpoint_template,
                series_id: &self.series_id,
                key_proof: &self.key_proof,
            })
        };
        bytes.extend(encoded.unwrap_or_default());
        keccak256(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own_data(content: &str, format: &str) -> Submission {
        let p = DataPayload {
            format: format.to_owned(),
            content: content.to_owned(),
            bytes: content.len() as u32,
            keccak256: format!("0x{}", hex::encode(keccak256(content.as_bytes()).0)),
        };
        Submission {
            answers: Answers {
                what: Kind::Own,
                no_login: false,
                rights: crate::Rights::OwnData,
                license_id: None,
                names_person: false,
                changes: crate::Changes::Once,
            },
            title: "Example dataset".into(),
            description: "A small sample".into(),
            source_name: "My data".into(),
            source_url: String::new(),
            data_payload: Some(p),
            endpoint_url: None,
            json_pointer: None,
            unit: None,
            endpoint_template: None,
            series_id: None,
            key_proof: None,
        }
    }

    #[test]
    fn bounded_own_data_payloads_are_checked_before_hashing_or_review() {
        let mut csv = own_data("date,value\n2026-01-01,12\n", "csv");
        csv.validate()
            .expect("valid CSV without a public source URL");
        let original_hash = csv.hash();
        csv.data_payload.as_mut().expect("payload").content = "date,value\n2026-01-01,13\n".into();
        assert_eq!(
            csv.validate()
                .expect_err("changed content has stale digest")
                .code(),
            "payload_invalid"
        );
        csv.data_payload.as_mut().expect("payload").content = "date,value\n2026-01-01,13\n".into();
        let content = csv.data_payload.as_ref().expect("payload").content.clone();
        csv.data_payload.as_mut().expect("payload").bytes = content.len() as u32;
        csv.data_payload.as_mut().expect("payload").keccak256 =
            format!("0x{}", hex::encode(keccak256(content.as_bytes()).0));
        csv.validate().expect("updated payload digest");
        assert_ne!(
            csv.hash(),
            original_hash,
            "content is committed by the submission hash"
        );

        for (format, content) in [
            ("csv", "value\n=1+1\n"),
            ("csv", "email,value\na@example.org,1\n"),
            ("csv", "value,VALUE\n1,2\n"),
            ("csv", "name,value\n\"Jane\"oops,1\n"),
            ("json", r#"{"email":"a@example.org"}"#),
            ("json", r#"{"api_key":"secret-value"}"#),
            ("json", r#"{"value":"Bearer secret"}"#),
            ("json", r#"{"api\u005fkey":"secret-value"}"#),
            ("json", r#"{"value":"Bearer\u0020secret"}"#),
            ("json", r#"{"value":1,"\u0076alue":2}"#),
            ("json", "<script>alert(1)</script>"),
            ("yaml", "a: 1"),
        ] {
            let mut bad = own_data(content, format);
            let payload = bad.data_payload.as_mut().expect("payload");
            payload.bytes = content.len() as u32;
            payload.keccak256 = format!("0x{}", hex::encode(keccak256(content.as_bytes()).0));
            assert!(bad.validate().is_err(), "{format} {content}");
        }
        let mut too_large = own_data(&"x".repeat(MAX_PAYLOAD_BYTES + 1), "json");
        let payload = too_large.data_payload.as_mut().expect("payload");
        payload.bytes = payload.content.len() as u32;
        payload.keccak256 = format!("0x{}", hex::encode(keccak256(payload.content.as_bytes()).0));
        assert_eq!(
            too_large
                .validate()
                .expect_err("oversize payload must be rejected")
                .code(),
            "payload_invalid"
        );
        let mut non_own = own_data("a,b\n1,2\n", "csv");
        non_own.answers.rights = crate::Rights::PublicDomain;
        non_own.source_url = "https://example.org/data".into();
        assert_eq!(
            non_own
                .validate()
                .expect_err("payload is only accepted on own-data route")
                .code(),
            "payload_route"
        );
        let mut missing_url = own_data("a,b\n1,2\n", "csv");
        missing_url.answers.rights = crate::Rights::PublicDomain;
        missing_url.data_payload = None;
        assert_eq!(
            missing_url
                .validate()
                .expect_err("metadata-only own data still requires a public URL")
                .code(),
            "source_url_required"
        );
    }

    #[test]
    fn urls_are_https_with_a_plain_host() {
        let u = HttpsUrl::parse("https://API.FiscalData.Treasury.gov/services/x?filter=a:eq:1")
            .expect("ok");
        assert_eq!(u.host, "api.fiscaldata.treasury.gov");
        assert_eq!(u.port, None);
        assert_eq!(u.path_and_query, "/services/x?filter=a:eq:1");
        assert_eq!(
            HttpsUrl::parse("https://example.org")
                .expect("ok")
                .path_and_query,
            "/"
        );
        assert_eq!(
            HttpsUrl::parse("https://example.org?a=1")
                .expect("ok")
                .path_and_query,
            "/?a=1"
        );
        assert_eq!(
            HttpsUrl::parse("https://example.org:8443/x")
                .expect("ok")
                .port,
            Some(8443)
        );
        for bad in [
            "http://example.org/",
            "ftp://example.org/",
            "https://",
            "https://user:pw@example.org/",
            "https://example.org@evil.test/",
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://localhost/",
            "https://127.1/",
            "https://0x7f.1/",
            "https://example.org:0/",
            "https://example.org:99999/",
            "https://example.org:/",
            "https://exa mple.org/",
            "https://example.org/a b",
            "https://example.org/#frag",
            "https://-bad.example/",
            "https://example.org/\\x",
            "https://bücher.example/",
        ] {
            assert!(HttpsUrl::parse(bad).is_err(), "{bad}");
        }
        assert!(HttpsUrl::parse(&format!("https://example.org/{}", "a".repeat(500))).is_err());
    }

    #[test]
    fn email_shapes_are_found() {
        for yes in [
            "write to someone@example.com please",
            "a.b+c@sub.example.org",
            "(x@y.io)",
            "MAILTO:abc",
        ] {
            assert!(has_email(yes), "{yes}");
        }
        for no in [
            "price @ 5 USD",
            "@handle",
            "user@localhost",
            "x@y.1",
            "",
            "plain text",
            "a@b",
        ] {
            assert!(!has_email(no), "{no}");
        }
    }

    #[test]
    fn json_pointers_follow_rfc_6901() {
        for ok in ["", "/", "/data/0/tot_pub_debt_out_amt", "/a~0b/c~1d"] {
            assert!(is_json_pointer(ok), "{ok}");
        }
        for bad in ["data/0", "/a~2", "/a~", "/a\u{0}b"] {
            assert!(!is_json_pointer(bad), "{bad}");
        }
    }
}
