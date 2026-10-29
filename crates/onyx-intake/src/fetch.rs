//! The check-pull policy (ADR-0038 items 5-6): which endpoint a server may
//! fetch once to check a submission, which addresses it may connect to, and
//! what the preview of the body shows.
//!
//! A server pulls only from a host already rights-cleared in
//! docs/ops/data-sources.md ([`CLEARED_HOSTS`]), only on the level-2 route
//! (public, no login, rights stated), on the default https port. Every other
//! endpoint is never fetched by the server; the policy says why. The transport
//! that does the fetch (onyx-api, feature `http-client`) also refuses any
//! resolved address that is not public ([`is_public_ip`]), connects to the
//! address it checked, follows no redirect, reads at most [`MAX_BODY_BYTES`]
//! and gives up after [`FETCH_TIMEOUT_SECS`].

use crate::answers::{Outcome, Path, Route};
use crate::submission::Submission;
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value as Json};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Hosts a server may pull from, sorted. Each is a source whose rights row in
/// docs/ops/data-sources.md says OK and needs no account, key, payment or
/// contact header: S10/N3 U.S. Treasury Fiscal Data (public reuse statement,
/// no key). SEC EDGAR (S11) is OK too but its fair-access rule asks for a
/// declared contact in the User-Agent, which needs a role contact first (P2g);
/// BLS (N8) carries duties per served row. Neither is added in S1.
pub const CLEARED_HOSTS: &[&str] = &["api.fiscaldata.treasury.gov"];
/// Registrable domains whose rights row in docs/ops/data-sources.md says
/// **blocked**, sorted: a submission naming one or any subdomain in any URL is
/// held and never published, whatever the answers say. FRED (N10): its terms
/// forbid redistributing third-party data and storing or caching its content;
/// all of stlouisfed.org (FRED, ALFRED, research pages) is held (S1-R1-3).
pub const BLOCKED_HOSTS: &[&str] = &["stlouisfed.org"];

/// Whether `host` (lower case) is a blocked host or one of its subdomains.
pub fn is_blocked_host(host: &str) -> bool {
    BLOCKED_HOSTS
        .iter()
        .any(|b| host == *b || host.strip_suffix(b).is_some_and(|p| p.ends_with('.')))
}

/// Largest body read, in bytes (256 KiB).
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// The whole fetch's deadline, in seconds (resolve, connect, TLS, read).
pub const FETCH_TIMEOUT_SECS: u64 = 10;
/// Largest preview value shown, in bytes.
pub const MAX_PREVIEW_VALUE: usize = 256;

/// Why the server does not fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoFetch {
    /// No endpoint given.
    NoEndpoint,
    /// The route is refused or held: nothing is fetched for it.
    NotWhileRefusedOrHeld,
    /// Not the level-2 auto-pull route (a login is needed, or another kind).
    NotAutoPull,
    /// The host is not one already rights-cleared.
    HostNotCleared,
    /// A port other than 443.
    PortNotAllowed,
}

impl NoFetch {
    /// The stable code.
    pub fn code(self) -> &'static str {
        match self {
            NoFetch::NoEndpoint => "no_endpoint",
            NoFetch::NotWhileRefusedOrHeld => "not_while_refused_or_held",
            NoFetch::NotAutoPull => "not_auto_pull",
            NoFetch::HostNotCleared => "host_not_cleared",
            NoFetch::PortNotAllowed => "port_not_allowed",
        }
    }

    /// The sentence shown.
    pub fn text(self) -> &'static str {
        match self {
            NoFetch::NoEndpoint => "No endpoint was given, so there is nothing to pull.",
            NoFetch::NotWhileRefusedOrHeld => "Nothing is pulled for a refused or held submission.",
            NoFetch::NotAutoPull => "Only public data anyone can fetch without a login is pulled.",
            NoFetch::HostNotCleared => "Onyx pulls only from sources whose rights it has already reviewed. A reviewer reads this source's terms first; nothing is fetched until then.",
            NoFetch::PortNotAllowed => "Only the standard https port (443) is pulled.",
        }
    }

    /// Every reason, in a fixed order.
    pub const ALL: [NoFetch; 5] = [
        NoFetch::NoEndpoint,
        NoFetch::NotWhileRefusedOrHeld,
        NoFetch::NotAutoPull,
        NoFetch::HostNotCleared,
        NoFetch::PortNotAllowed,
    ];
}

/// The policy's answer for one submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchPolicy {
    /// The server may pull this URL once (an allowlisted host).
    Pull {
        /// The URL to fetch.
        url: String,
        /// Its host.
        host: String,
    },
    /// The server does not fetch.
    NoFetch(NoFetch),
}

impl FetchPolicy {
    /// JSON form: `{"pull": true, "host": ...}` or `{"pull": false, "reason": code}`.
    pub fn to_json(&self) -> Json {
        match self {
            FetchPolicy::Pull { host, .. } => json!({ "pull": true, "host": host }),
            FetchPolicy::NoFetch(r) => {
                json!({ "pull": false, "reason": r.code(), "text": r.text() })
            }
        }
    }
}

/// The policy for `s` on `route` ([`Submission::route`]) with the hosts
/// `allow` (sorted). The submission is assumed valid
/// ([`Submission::validate`]). A refused or held route is never pulled,
/// whatever its endpoint.
pub fn fetch_policy(s: &Submission, route: &Route, allow: &[&str]) -> FetchPolicy {
    if route.outcome != Outcome::Accept {
        return FetchPolicy::NoFetch(NoFetch::NotWhileRefusedOrHeld);
    }
    let Some(url) = s.endpoint() else {
        return FetchPolicy::NoFetch(NoFetch::NoEndpoint);
    };
    if route.path != Some(Path::AutoPull) {
        return FetchPolicy::NoFetch(NoFetch::NotAutoPull);
    }
    if allow.binary_search(&url.host.as_str()).is_err() {
        return FetchPolicy::NoFetch(NoFetch::HostNotCleared);
    }
    if url.port.is_some_and(|p| p != 443) {
        return FetchPolicy::NoFetch(NoFetch::PortNotAllowed);
    }
    FetchPolicy::Pull {
        url: url.url,
        host: url.host,
    }
}

/// Whether a server may connect to `ip`: a globally routable unicast address.
/// Refused: unspecified, loopback, private (RFC 1918), shared (100.64/10),
/// link-local (incl. the 169.254.169.254 cloud metadata address), benchmark,
/// documentation, multicast, reserved and broadcast IPv4; for IPv6 the same
/// kinds plus unique-local (fc00::/7, incl. fd00:ec2::254), and any form that
/// carries an IPv4 address (mapped, compatible, NAT64, 6to4, Teredo) unless
/// that address is public.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => public_v4(a),
        IpAddr::V6(a) => public_v6(a),
    }
}

fn public_v4(a: Ipv4Addr) -> bool {
    let [o0, o1, o2, _] = a.octets();
    !(o0 == 0
        || o0 == 10
        || o0 == 127
        || (o0 == 100 && (64..=127).contains(&o1))
        || (o0 == 169 && o1 == 254)
        || (o0 == 172 && (16..=31).contains(&o1))
        || (o0 == 192 && o1 == 0 && o2 == 0)
        || (o0 == 192 && o1 == 0 && o2 == 2)
        || (o0 == 192 && o1 == 88 && o2 == 99)
        || (o0 == 192 && o1 == 168)
        || (o0 == 198 && (o1 == 18 || o1 == 19))
        || (o0 == 198 && o1 == 51 && o2 == 100)
        || (o0 == 203 && o1 == 0 && o2 == 113)
        || o0 >= 224)
}

fn public_v6(a: Ipv6Addr) -> bool {
    let s = a.segments();
    let embedded = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    if a.is_unspecified() || a.is_loopback() {
        return false;
    }
    // ::ffff:a.b.c.d (mapped) and ::a.b.c.d (compatible, deprecated).
    if s[..5] == [0; 5] && (s[5] == 0xffff || s[5] == 0) {
        return public_v4(embedded(s[6], s[7]));
    }
    // 64:ff9b::/96 (NAT64) and 64:ff9b:1::/48 (local-use NAT64).
    if s[0] == 0x64 && s[1] == 0xff9b {
        return s[2] == 0 && s[3..6] == [0; 3] && public_v4(embedded(s[6], s[7]));
    }
    // 2002::/16 (6to4) carries an IPv4 address in bits 16-47.
    if s[0] == 0x2002 {
        return public_v4(embedded(s[1], s[2]));
    }
    !((s[0] & 0xfe00) == 0xfc00          // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80     // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0     // fec0::/10 site-local (deprecated)
        || (s[0] & 0xff00) == 0xff00     // ff00::/8 multicast
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        || (s[0] == 0x2001 && s[1] == 0)      // 2001::/32 Teredo
        || (s[0] == 0x0100 && s[1..4] == [0; 3]) // 100::/64 discard
        || (s[0] == 0x3fff && s[1] < 0x1000)) // 3fff::/20 documentation
}

/// What a preview shows of a pulled value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewValue {
    /// The text at the pointer (a JSON string, trimmed; an integer or a
    /// boolean as written).
    Text(String),
    /// No pointer was given.
    NoPointer,
    /// The body is not JSON.
    NotJson,
    /// Nothing at the pointer.
    Missing,
    /// A JSON number with a fraction or an exponent: Onyx reads decimals
    /// only from strings, never through binary floating point (ADR-0009).
    Fraction,
    /// An object, array or null: not one value.
    NotAValue,
    /// Longer than [`MAX_PREVIEW_VALUE`] bytes.
    TooLong,
}

impl PreviewValue {
    /// The stable code (`text` for a value).
    pub fn code(&self) -> &'static str {
        match self {
            PreviewValue::Text(_) => "text",
            PreviewValue::NoPointer => "no_pointer",
            PreviewValue::NotJson => "not_json",
            PreviewValue::Missing => "missing",
            PreviewValue::Fraction => "fraction",
            PreviewValue::NotAValue => "not_a_value",
            PreviewValue::TooLong => "too_long",
        }
    }
}

/// A preview of one pulled body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    /// Keccak-256 of the body bytes as received.
    pub body_hash: Hash32,
    /// The body's length.
    pub bytes: usize,
    /// The value at the pointer.
    pub value: PreviewValue,
}

impl Preview {
    /// JSON form.
    pub fn to_json(&self) -> Json {
        json!({
            "bodyKeccak256": format!("0x{}", hex::encode(self.body_hash.0)),
            "bytes": self.bytes,
            "value": match &self.value { PreviewValue::Text(t) => Some(t.as_str()), _ => None },
            "valueStatus": self.value.code(),
        })
    }
}

/// The preview of `body` at `pointer`: the body's Keccak-256 and the value
/// there, read with the JSON-pointer fetcher's normalisation (strings trimmed
/// of ASCII whitespace, integers and booleans as written, fractions refused).
/// For a check by eye only: no proof level comes from it.
pub fn preview(body: &[u8], pointer: Option<&str>) -> Preview {
    let value = match pointer {
        None => PreviewValue::NoPointer,
        Some(p) => match serde_json::from_slice::<Json>(body) {
            Err(_) => PreviewValue::NotJson,
            Ok(doc) => match doc.pointer(p) {
                None => PreviewValue::Missing,
                Some(Json::String(s)) => {
                    let t = s.trim_matches(|c: char| c.is_ascii_whitespace());
                    if t.len() > MAX_PREVIEW_VALUE {
                        PreviewValue::TooLong
                    } else {
                        PreviewValue::Text(t.to_string())
                    }
                }
                Some(Json::Bool(b)) => PreviewValue::Text(b.to_string()),
                Some(Json::Number(n)) if n.is_i64() || n.is_u64() => {
                    PreviewValue::Text(n.to_string())
                }
                Some(Json::Number(_)) => PreviewValue::Fraction,
                Some(_) => PreviewValue::NotAValue,
            },
        },
    };
    Preview {
        body_hash: keccak256(body),
        bytes: body.len(),
        value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleared_hosts_are_sorted_lower_case_and_unique() {
        assert!(CLEARED_HOSTS.windows(2).all(|w| w[0] < w[1]));
        assert!(CLEARED_HOSTS
            .iter()
            .all(|h| h.bytes().all(|b| !b.is_ascii_uppercase())));
    }

    #[test]
    fn only_public_addresses_pass() {
        for (ip, public) in [
            ("8.8.8.8", true),
            ("1.1.1.1", true),
            ("0.0.0.0", false),
            ("10.1.2.3", false),
            ("127.0.0.1", false),
            ("100.64.0.1", false),
            ("100.128.0.1", true),
            ("169.254.169.254", false),
            ("172.16.0.1", false),
            ("172.32.0.1", true),
            ("192.0.0.8", false),
            ("192.0.2.1", false),
            ("192.168.1.1", false),
            ("198.18.0.1", false),
            ("198.51.100.7", false),
            ("203.0.113.9", false),
            ("224.0.0.1", false),
            ("240.0.0.1", false),
            ("255.255.255.255", false),
            ("2606:4700:4700::1111", true),
            ("::", false),
            ("::1", false),
            ("::ffff:127.0.0.1", false),
            ("::ffff:8.8.8.8", true),
            ("::127.0.0.1", false),
            ("64:ff9b::a9fe:a9fe", false),
            ("64:ff9b::808:808", true),
            ("64:ff9b:1::808:808", false),
            ("2002:7f00:1::", false),
            ("2002:808:808::", true),
            ("2001::1", false),
            ("2001:db8::1", false),
            ("fc00::1", false),
            ("fd00:ec2::254", false),
            ("fe80::1", false),
            ("fec0::1", false),
            ("ff02::1", false),
            ("100::1", false),
            ("3fff::1", false),
        ] {
            let a: IpAddr = ip.parse().expect("ip");
            assert_eq!(is_public_ip(a), public, "{ip}");
        }
    }

    #[test]
    fn previews_read_like_the_json_pointer_fetcher() {
        let body = br#"{"data":[{"v":"  40305316210829.72 ","n":42,"f":1.5,"b":true,"o":{},"z":null,"e":1e3}]}"#;
        let p = |ptr: &str| preview(body, Some(ptr)).value;
        assert_eq!(
            p("/data/0/v"),
            PreviewValue::Text("40305316210829.72".into())
        );
        assert_eq!(p("/data/0/n"), PreviewValue::Text("42".into()));
        assert_eq!(p("/data/0/b"), PreviewValue::Text("true".into()));
        assert_eq!(p("/data/0/f"), PreviewValue::Fraction);
        assert_eq!(p("/data/0/e"), PreviewValue::Fraction);
        assert_eq!(p("/data/0/o"), PreviewValue::NotAValue);
        assert_eq!(p("/data/0/z"), PreviewValue::NotAValue);
        assert_eq!(p("/data/1"), PreviewValue::Missing);
        assert_eq!(preview(b"<html>", Some("/x")).value, PreviewValue::NotJson);
        assert_eq!(preview(body, None).value, PreviewValue::NoPointer);
        let long = format!(r#"{{"v":"{}"}}"#, "x".repeat(MAX_PREVIEW_VALUE + 1));
        assert_eq!(
            preview(long.as_bytes(), Some("/v")).value,
            PreviewValue::TooLong
        );
        let pv = preview(body, Some("/data/0/v"));
        assert_eq!(pv.body_hash, keccak256(body));
        assert_eq!(pv.bytes, body.len());
    }
}
