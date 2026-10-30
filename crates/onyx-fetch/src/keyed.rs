//! Keyed fetches (ADR-0054 item 2, contributor loop CL4): a key holder's own provider key,
//! added on the holder's machine to its one request to the template's host.
//!
//! - [`ApiKey`] holds the key in this process only: no `Display`, a `Debug` that shows nothing
//!   of it, its bytes zeroed when dropped, and [`ApiKey::redact`] for any text that might echo
//!   it (as typed or percent-encoded).
//! - [`KeyPlacement`] is where the key goes: a header (raw or `Bearer`) or the URL's one
//!   `name=`[`KEY_PLACEHOLDER`] query pair. Nothing else of the URL can take it.
//! - [`check_keyed_template_url`]: a keyed template's URL passes the manifest rules
//!   ([`crate::check_template_url`]) once its key pair is left out, so no OTHER secret-named
//!   parameter, port or blocked host gets in.
//!
//! The rest is the plain fetch: one resolution, public addresses only, no redirect followed,
//! so the key never reaches another host (THREATS CL4-2).

use crate::{check_template_url, FetchError, Target};
use hyper::header::{HeaderName, HeaderValue};

/// The placeholder a keyed template's URL holds where a query key goes (the same text as
/// `onyx_core::feed_json::KEY_PLACEHOLDER`).
pub const KEY_PLACEHOLDER: &str = "{api_key}";
/// Shortest key accepted (onyx-contrib refuses shorter ones too: too short to check
/// outgoing text against).
pub const MIN_KEY: usize = 12;
/// Longest key accepted.
pub const MAX_KEY: usize = 4096;
/// The piece of a key [`ApiKey::held_in`] looks for (onyx-contrib's filter uses 10 too).
pub const KEY_WINDOW: usize = 10;

/// A header name and value that carry the key.
pub(crate) type KeyHeader = (HeaderName, HeaderValue);

/// A provider API key, held in this process only (see the module docs).
pub struct ApiKey(Vec<u8>);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<hidden>)")
    }
}

impl Drop for ApiKey {
    fn drop(&mut self) {
        // Best effort: the bytes this value owns do not outlive it.
        self.0.iter_mut().for_each(|b| *b = 0);
    }
}

/// Why a key is refused (the text never holds the key).
fn bad_key(why: &str) -> FetchError {
    FetchError::BadUrl(format!("the API key: {why}"))
}

impl ApiKey {
    /// A key: [`MIN_KEY`]..=[`MAX_KEY`] printable ASCII bytes without space (API keys are).
    pub fn new(key: &str) -> Result<Self, FetchError> {
        if key.len() < MIN_KEY {
            return Err(bad_key("shorter than 12 characters"));
        }
        if key.len() > MAX_KEY {
            return Err(bad_key("longer than 4096 characters"));
        }
        if !key.bytes().all(|c| (0x21..=0x7e).contains(&c)) {
            return Err(bad_key(
                "printable ASCII without spaces or line breaks only",
            ));
        }
        Ok(ApiKey(key.as_bytes().to_vec()))
    }

    /// Read a key from the first line of `r` (at most 8 KiB in all; a trailing `\r\n` or `\n`
    /// is removed). The way `onyx-contrib` hands its vault's key to an observer: on standard
    /// input, never on a command line or in the environment.
    pub fn from_reader<R: std::io::Read>(r: R) -> Result<Self, FetchError> {
        use std::io::Read as _;
        let mut buf = Vec::new();
        r.take(8 * 1024 + 1)
            .read_to_end(&mut buf)
            .map_err(|_| bad_key("could not be read from standard input"))?;
        // Held (and so zeroed on drop) from here, whatever is refused below.
        let held = ApiKey(buf);
        if held.0.len() > 8 * 1024 {
            return Err(bad_key("more than 8 KiB on standard input"));
        }
        let text = std::str::from_utf8(&held.0).map_err(|_| bad_key("not text"))?;
        let line = text.strip_suffix('\n').unwrap_or(text);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.contains(['\n', '\r']) {
            return Err(bad_key("one line only"));
        }
        Self::new(line)
    }

    fn text(&self) -> &str {
        // Checked ASCII in `new`.
        std::str::from_utf8(&self.0).unwrap_or("")
    }

    /// The key percent-encoded for a query value (everything but `A-Z a-z 0-9 - . _ ~`).
    fn query_value(&self) -> String {
        let mut out = String::with_capacity(self.0.len() * 3);
        for &c in &self.0 {
            if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(c));
            } else {
                out.push_str(&format!("%{c:02X}"));
            }
        }
        out
    }

    /// `text` with every copy of the key (as typed, percent-encoded in upper or lower case)
    /// replaced by `[key]`.
    pub fn redact(&self, text: &str) -> String {
        let enc = self.query_value();
        let mut out = text.replace(self.text(), "[key]");
        for form in [enc.clone(), enc.to_ascii_lowercase()] {
            if form != self.text() {
                out = out.replace(&form, "[key]");
            }
        }
        out
    }

    /// Whether `bytes` hold any [`KEY_WINDOW`]-byte piece of the key, in any letter case, as
    /// typed or percent-encoded (review CL4-R1-1: a value that echoes the key is never
    /// revealed).
    pub fn held_in(&self, bytes: &[u8]) -> bool {
        let hay = bytes.to_ascii_lowercase();
        let enc = self.query_value().into_bytes();
        [self.0.as_slice(), enc.as_slice()].iter().any(|form| {
            let form = form.to_ascii_lowercase();
            form.windows(KEY_WINDOW.min(form.len()))
                .any(|w| hay.windows(w.len()).any(|h| h == w))
        })
    }

    /// `e` with the key removed from its text.
    pub fn redact_error(&self, e: FetchError) -> FetchError {
        match e {
            FetchError::BadUrl(s) => FetchError::BadUrl(self.redact(&s)),
            FetchError::Refused(s) => FetchError::Refused(self.redact(&s)),
            FetchError::Encoding(s) => FetchError::Encoding(self.redact(&s)),
            FetchError::Unreachable(s) => FetchError::Unreachable(self.redact(&s)),
            other => other,
        }
    }
}

/// Where a keyed source's key goes in its one request (ADR-0054 item 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyPlacement {
    /// A header, lower-case name; `bearer` writes `Bearer <key>`.
    Header {
        /// The header name.
        name: String,
        /// `Bearer <key>` instead of the key alone.
        bearer: bool,
    },
    /// The URL's one `name=`[`KEY_PLACEHOLDER`] query pair.
    Query {
        /// The parameter name.
        name: String,
    },
}

/// `url` without its one `name={api_key}` query pair, or `None` when the URL does not hold
/// exactly one placeholder, as exactly that pair.
fn without_key_pair(url: &str, name: &str) -> Option<String> {
    if url.matches(KEY_PLACEHOLDER).count() != 1 {
        return None;
    }
    let (base, query) = url.split_once('?')?;
    let pair = format!("{name}={KEY_PLACEHOLDER}");
    let pairs: Vec<&str> = query.split('&').collect();
    if pairs.iter().filter(|p| **p == pair).count() != 1 {
        return None;
    }
    let rest: Vec<&str> = pairs.into_iter().filter(|p| *p != pair).collect();
    Some(if rest.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", rest.join("&"))
    })
}

impl KeyPlacement {
    /// The URL to request and the header to add, for `key`. A query key fills the one
    /// `name={api_key}` pair; a header key needs a URL with no placeholder.
    pub(crate) fn apply(
        &self,
        url: &str,
        key: &ApiKey,
    ) -> Result<(String, Option<KeyHeader>), FetchError> {
        match self {
            KeyPlacement::Query { name } => {
                if without_key_pair(url, name).is_none() {
                    return Err(FetchError::BadUrl(format!(
                        "a query key's URL holds {name}={KEY_PLACEHOLDER} exactly once"
                    )));
                }
                let pair = format!("{name}={KEY_PLACEHOLDER}");
                let filled = url.replacen(&pair, &format!("{name}={}", key.query_value()), 1);
                Ok((filled, None))
            }
            KeyPlacement::Header { name, bearer } => {
                if url.contains(KEY_PLACEHOLDER) {
                    return Err(FetchError::BadUrl(
                        "a header key's URL holds no {api_key}".into(),
                    ));
                }
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| FetchError::BadUrl("the key header name".into()))?;
                let text = if *bearer {
                    format!("Bearer {}", key.text())
                } else {
                    key.text().to_string()
                };
                let mut value =
                    HeaderValue::from_str(&text).map_err(|_| bad_key("not a header value"))?;
                value.set_sensitive(true);
                Ok((url.to_string(), Some((name, value))))
            }
        }
    }
}

/// The manifest rules for a keyed template's URL (ADR-0054 item 1): a query key's URL holds
/// `name={api_key}` exactly once, a header key's none; with that pair left out, the URL
/// passes [`check_template_url`] (no port, no OTHER secret-named parameter, host not
/// blocked).
pub fn check_keyed_template_url(
    url_template: &str,
    placement: &KeyPlacement,
) -> Result<Target, FetchError> {
    match placement {
        KeyPlacement::Query { name } => {
            let rest = without_key_pair(url_template, name).ok_or_else(|| {
                FetchError::BadUrl(format!(
                    "a query key's template holds {name}={KEY_PLACEHOLDER} exactly once, in its query"
                ))
            })?;
            check_template_url(&rest)
        }
        KeyPlacement::Header { .. } => {
            if url_template.contains(KEY_PLACEHOLDER) {
                return Err(FetchError::BadUrl(
                    "a header key's template holds no {api_key}".into(),
                ));
            }
            check_template_url(url_template)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "FAKE-key_0123456789+/=";

    #[test]
    fn a_key_is_printable_ascii_of_twelve_bytes_or_more_and_never_shown() {
        let k = ApiKey::new(KEY).expect("key");
        assert_eq!(format!("{k:?}"), "ApiKey(<hidden>)");
        for bad in [
            "abc123",
            "has space 0123",
            "tab\t0123456789",
            "\u{e9}0123456789ab",
        ] {
            let e = ApiKey::new(bad).expect_err(bad).to_string();
            assert!(!e.contains(bad), "{e}");
        }
        assert!(ApiKey::new(&"k".repeat(MAX_KEY + 1)).is_err());
        let mut stdin: &[u8] = b"FAKE-key_0123456789+/=\n";
        assert_eq!(ApiKey::from_reader(&mut stdin).expect("stdin").text(), KEY);
        let mut two: &[u8] = b"FAKE-key_0123456789\nsecond-line-here\n";
        assert!(ApiKey::from_reader(&mut two).is_err());
    }

    #[test]
    fn redact_removes_the_key_as_typed_and_percent_encoded() {
        let k = ApiKey::new(KEY).expect("key");
        let enc = k.query_value();
        assert_eq!(enc, "FAKE-key_0123456789%2B%2F%3D");
        let text = format!("a {KEY} b {enc} c {}", enc.to_ascii_lowercase());
        assert_eq!(k.redact(&text), "a [key] b [key] c [key]");
    }

    /// Review CL4-R1-1: any 10-byte piece of the key, in any case or percent-encoded, is found.
    #[test]
    fn held_in_finds_any_piece_of_the_key() {
        let k = ApiKey::new(KEY).expect("key");
        for hay in [
            format!("x{KEY}y"),
            "..fake-key_0123..".to_string(),
            "0123456789%2b%2f".to_string(),
        ] {
            assert!(k.held_in(hay.as_bytes()), "{hay}");
        }
        for hay in ["FAKE-key_", "12345", "1234.5"] {
            assert!(!k.held_in(hay.as_bytes()), "{hay}");
        }
    }

    #[test]
    fn placements_fill_only_their_own_place() {
        let k = ApiKey::new(KEY).expect("key");
        let q = KeyPlacement::Query {
            name: "api_key".into(),
        };
        let (url, h) = q
            .apply("https://p.example.org/v?d=1&api_key={api_key}", &k)
            .expect("query");
        assert_eq!(
            url,
            "https://p.example.org/v?d=1&api_key=FAKE-key_0123456789%2B%2F%3D"
        );
        assert!(h.is_none());
        for bad in [
            "https://p.example.org/v?d=1",
            "https://p.example.org/{api_key}?api_key={api_key}",
            "https://p.example.org/v?key={api_key}",
        ] {
            let e = q.apply(bad, &k).expect_err(bad).to_string();
            assert!(!e.contains(KEY));
        }
        let hp = KeyPlacement::Header {
            name: "authorization".into(),
            bearer: true,
        };
        let (url, h) = hp.apply("https://p.example.org/v", &k).expect("header");
        assert_eq!(url, "https://p.example.org/v");
        let (n, v) = h.expect("a header");
        assert_eq!(n.as_str(), "authorization");
        assert!(v.is_sensitive());
        assert_eq!(v.to_str().expect("ascii"), format!("Bearer {KEY}"));
        assert!(hp
            .apply("https://p.example.org/v?api_key={api_key}", &k)
            .is_err());
    }

    #[test]
    fn keyed_template_urls_follow_the_manifest_rules() {
        let q = KeyPlacement::Query {
            name: "api_key".into(),
        };
        let t = check_keyed_template_url("https://p.example.org/v?d={date}&api_key={api_key}", &q)
            .expect("ok");
        assert_eq!(t.host, "p.example.org");
        check_keyed_template_url("https://p.example.org/v?api_key={api_key}", &q).expect("ok");
        for bad in [
            "https://p.example.org/v?d={date}",
            "https://p.example.org/v?d={date}&api_key={api_key}&token=x",
            "https://p.example.org:443/v?api_key={api_key}",
            "https://fred.stlouisfed.org/v?api_key={api_key}",
            "https://p.example.org/v?api_key={api_key}&api_key={api_key}",
        ] {
            assert!(check_keyed_template_url(bad, &q).is_err(), "{bad}");
        }
        let h = KeyPlacement::Header {
            name: "x-api-key".into(),
            bearer: false,
        };
        check_keyed_template_url("https://p.example.org/v?d={date}", &h).expect("ok");
        assert!(check_keyed_template_url("https://p.example.org/v?api_key={api_key}", &h).is_err());
        assert!(check_keyed_template_url("https://p.example.org/v?apikey=abc", &h).is_err());
    }
}
