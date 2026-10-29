//! Example level-2 fetcher (ADR-0027): read one value from a JSON page with a
//! JSON pointer (RFC 6901).
//!
//! This crate never touches the network. An observer fetches the page itself
//! (plain HTTPS GET of the feed's `url`, no account) and hands the bytes to
//! [`JsonPointerFetcher`], which every observer runs identically. Tests use the
//! synthetic pages in `fixtures/`; see README.md for how a real source with
//! open terms plugs in.
//!
//! Normalization rules (the fetcher's contract, pinned by [`FETCHER_TAG`]):
//! - Exact feeds: a JSON string (ASCII whitespace trimmed), `true`/`false`, or a
//!   JSON integer in decimal; anything else, empty, or longer than the feed's
//!   `max_len` is `Unavailable`.
//! - Numeric feeds: a JSON string holding a plain decimal (`-?[0-9]+(\.[0-9]+)?`,
//!   no exponent, no separators) or a JSON integer. Digits beyond the feed's
//!   scale are truncated toward zero. JSON floats are refused (`Unavailable`):
//!   the parser would have rounded them through binary floating point (ADR-0009).

use onyx_core::{keccak256, Answer, Fetcher, Hash32, ObservationKind, ObservationSpec};
use serde_json::Value as Json;

/// Version tag the fetcher id is derived from. Changing any rule above means a
/// new tag, hence a new id, hence new feeds.
pub const FETCHER_TAG: &[u8] = b"onyx-fetcher:json-pointer:v1";

/// The JSON-pointer fetcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonPointerFetcher;

impl Fetcher for JsonPointerFetcher {
    fn id(&self) -> Hash32 {
        keccak256(FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        let Ok(doc) = serde_json::from_slice::<Json>(page) else {
            return Answer::Unavailable;
        };
        let Some(v) = doc.pointer(&spec.selector) else {
            return Answer::Unavailable;
        };
        match spec.kind {
            ObservationKind::Exact { max_len } => {
                let text = match v {
                    Json::String(s) => s.trim_matches(|c: char| c.is_ascii_whitespace()).to_owned(),
                    Json::Bool(b) => b.to_string(),
                    Json::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
                    _ => return Answer::Unavailable,
                };
                if text.is_empty() || text.len() > usize::from(max_len) {
                    Answer::Unavailable
                } else {
                    Answer::Bytes(text.into_bytes())
                }
            }
            ObservationKind::Numeric { scale, .. } => {
                let parsed = match v {
                    Json::String(s) => parse_decimal(s.trim(), scale),
                    Json::Number(n) => n
                        .as_i64()
                        .map(i128::from)
                        .or_else(|| n.as_u64().map(i128::from))
                        .and_then(|i| i.checked_mul(10i128.checked_pow(u32::from(scale))?)),
                    _ => None,
                };
                parsed.map_or(Answer::Unavailable, Answer::Number)
            }
        }
    }
}

/// Parse `-?[0-9]+(\.[0-9]+)?` into a mantissa at `scale` decimals, truncating
/// extra digits toward zero. `None` on any other shape or on overflow.
pub fn parse_decimal(s: &str, scale: u8) -> Option<i128> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) if !f.is_empty() => (i, f),
        Some(_) => return None,
        None => (body, ""),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut m: i128 = 0;
    for b in int.bytes() {
        m = m.checked_mul(10)?.checked_add(i128::from(b - b'0'))?;
    }
    let mut digits = frac.bytes();
    for _ in 0..scale {
        let d = digits.next().map_or(0, |b| b - b'0');
        m = m.checked_mul(10)?.checked_add(i128::from(d))?;
    }
    Some(if neg { -m } else { m })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_parse_exactly_and_truncate() {
        assert_eq!(parse_decimal("64123.45", 2), Some(6_412_345));
        assert_eq!(parse_decimal("64123.456", 2), Some(6_412_345));
        assert_eq!(parse_decimal("-1.999", 2), Some(-199));
        assert_eq!(parse_decimal("7", 3), Some(7_000));
        assert_eq!(parse_decimal("0.5", 0), Some(0));
        for bad in [
            "", "-", ".5", "5.", "1e3", "+1", "1,000.00", "12a", "1.2.3", " 1",
        ] {
            assert_eq!(parse_decimal(bad, 2), None, "{bad:?}");
        }
        assert_eq!(parse_decimal(&"9".repeat(40), 0), None);
    }
}
