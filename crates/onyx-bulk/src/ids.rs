//! Publisher ids and origins (ADR-0066 items 3 and 4): the one grammar every
//! onboarding tool uses, so `apply`, `verify-application` and `doctor` cannot
//! disagree about a name.
//!
//! - **Id**: a dotted id ([`onyx_core::ids::is_dotted_id`]) without a reserved
//!   word and without `onyx` anywhere ([`check_publisher_id`]), and not a
//!   look-alike of a registered id ([`look_alike`]).
//! - **Origin**: an HTTPS host, or a host and one path ([`Origin::parse`]),
//!   not on a shared host, and not a prefix of a registered origin or the
//!   other way round ([`Origin::overlaps`]).
//!
//! `onyx_core::signed::is_origin_host` stays the host rule (level-2 sources
//! use it); a publisher's origin adds the path the guide always allowed.
//! Whether a name is neutral is still read by a person (THREATS PUBON-2).

use onyx_core::ids::is_dotted_id;
use onyx_core::signed::{is_origin_host, MAX_ORIGIN_LEN};
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// Words no publisher id may hold: they read as the network's own voice.
pub const RESERVED_WORDS: &[&str] = &["official", "verified", "reviewer", "operator"];
/// The network's name: no publisher id may contain it anywhere.
pub const NETWORK_NAME: &str = "onyx";
/// The most path segments an origin may have.
pub const MAX_ORIGIN_SEGMENTS: usize = 4;
/// Where the keys document is, under an origin.
pub const KEYS_DOCUMENT_PATH: &str = ".well-known/onyx-source-keys.json";

/// Shared hosts where a name that was given up can never be taken by someone
/// else. Empty: no host has been shown to promise that, so none is allowed.
pub const STABLE_SHARED_HOSTS: &[&str] = &[];

/// A label that marks a blog host under any country ending
/// (`someone.blogspot.co.uk`).
const SHARED_LABEL: &str = "blogspot";

/// The shared hosts: names under which many unrelated people publish
/// (`data/shared-hosts.txt`: compiled by hand for Onyx, with its date and
/// provenance; read from the binary, never from the network). An origin on
/// one of them is refused unless [`STABLE_SHARED_HOSTS`] lists it. The list
/// cannot be complete: the reviewer still reads the host.
pub fn shared_suffixes() -> &'static BTreeSet<&'static str> {
    static SET: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        include_str!("../data/shared-hosts.txt")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    })
}

/// Why a publisher id is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// Not a dotted id.
    #[error("the id must be lowercase letters, digits, - and _, in parts joined by . or :, at most 128 bytes")]
    Grammar,
    /// Holds a reserved word.
    #[error("the id holds the reserved word {0} (also with separators, digits for letters or an ending)")]
    Reserved(&'static str),
    /// Holds the network's name.
    #[error("the id must not contain onyx (also not with separators or digits for letters)")]
    Network,
    /// Reads like a registered id.
    #[error("the id reads like the registered id {0}")]
    LookAlike(String),
}

/// The id's words: what stands between `.`, `:`, `-` and `_`.
pub fn words(id: &str) -> impl Iterator<Item = &str> {
    id.split(['.', ':', '-', '_']).filter(|w| !w.is_empty())
}

/// The id without its separators (`river-levels.open` gives `riverlevelsopen`).
pub fn squashed(id: &str) -> String {
    words(id).collect()
}

/// A digit that stands for a letter reads as that letter, and `i` as `l`
/// (`0pen` is `open`, `leve1s` is `levels`).
fn fold_char(c: char) -> char {
    match c {
        '0' => 'o',
        '1' | 'i' => 'l',
        '3' => 'e',
        '4' => 'a',
        '5' => 's',
        '7' => 't',
        c => c,
    }
}

/// The squashed id with digits folded to the letters they stand for and
/// `ks` read as `x`: what the reserved words and the network's name are
/// matched in.
pub fn folded(id: &str) -> String {
    squashed(id)
        .chars()
        .map(fold_char)
        .collect::<String>()
        .replace("ks", "x")
}

/// What two ids are compared by for the look-alike rule: [`folded`], with
/// the plural `s` of each word dropped (`river-levels.opens`, `riverlevel.0pen`
/// and `river-levels.open` are one key).
pub fn look_alike_key(id: &str) -> String {
    words(id)
        .map(|w| {
            let w: String = w.chars().map(fold_char).collect();
            match w.strip_suffix('s') {
                Some(stem) if !stem.is_empty() => stem.to_string(),
                _ => w,
            }
        })
        .collect::<String>()
        .replace("ks", "x")
}

/// The grammar and the reserved words of a publisher id. The network's name
/// and the reserved words are found anywhere in the [`folded`] id, so a
/// separator (`on-yx`), a digit (`0fficial`) or an ending (`officials`) does
/// not hide them.
pub fn check_publisher_id(id: &str) -> Result<(), IdError> {
    if !is_dotted_id(id) {
        return Err(IdError::Grammar);
    }
    let f = folded(id);
    if f.contains(NETWORK_NAME) {
        return Err(IdError::Network);
    }
    match RESERVED_WORDS.iter().find(|r| f.contains(&folded(r))) {
        Some(r) => Err(IdError::Reserved(r)),
        None => Ok(()),
    }
}

/// The registered id that `id` reads like, if any: the same
/// [`look_alike_key`]. The same id on the same origin is no look-alike (a
/// second key).
pub fn look_alike<'a>(
    id: &str,
    origin: &str,
    registered: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), IdError> {
    let mine = look_alike_key(id);
    for (other, other_origin) in registered {
        if look_alike_key(other) == mine && !(other == id && other_origin == origin) {
            return Err(IdError::LookAlike(other.to_string()));
        }
    }
    Ok(())
}

/// Why an origin is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OriginError {
    /// Not a host, or a host and one path.
    #[error(
        "the origin must be a host name, or a host and a path (data.example.org/river-levels): {0}"
    )]
    Grammar(&'static str),
    /// On a shared host that is not on the stable list.
    #[error("the origin is on the shared host {0}: someone else could take the name once it is given up")]
    SharedHost(String),
    /// A prefix of a registered origin, or a registered origin is a prefix of it.
    #[error("the origin overlaps the registered origin {0}")]
    Overlap(String),
    /// The list of registered origins holds one that is not in the
    /// registered form, so nothing can be compared with it.
    #[error("the registered origin {0:?} is not in the registered form (lowercase host, optional path, no scheme, no trailing /): fix the list")]
    Registered(String),
}

/// A publisher's origin: a lowercase DNS host and at most
/// [`MAX_ORIGIN_SEGMENTS`] path segments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    host: String,
    segments: Vec<String>,
}

impl Origin {
    /// Parse the registered form: no scheme, port, query, trailing slash,
    /// uppercase or punycode label, so one origin has one spelling.
    pub fn parse(s: &str) -> Result<Origin, OriginError> {
        if s.len() > MAX_ORIGIN_LEN {
            return Err(OriginError::Grammar("at most 253 bytes"));
        }
        if s.contains("://") {
            return Err(OriginError::Grammar("no https:// in front"));
        }
        let (host, path) = match s.split_once('/') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        };
        if !is_origin_host(host) {
            return Err(OriginError::Grammar(
                "the host must be a lowercase DNS name with a dot, not an address, without a port",
            ));
        }
        if host.split('.').any(|l| l.starts_with("xn--")) {
            return Err(OriginError::Grammar(
                "no xn-- (punycode) host: it can read like another name",
            ));
        }
        let segments: Vec<String> = match path {
            None => Vec::new(),
            Some(p) => p.split('/').map(str::to_string).collect(),
        };
        if segments.len() > MAX_ORIGIN_SEGMENTS {
            return Err(OriginError::Grammar("at most 4 path parts"));
        }
        let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
        let seg_ok = |p: &String| {
            let b = p.as_bytes();
            !b.is_empty()
                && b.len() <= 63
                && alnum(b[0])
                && alnum(b[b.len() - 1])
                && b.iter().all(|c| alnum(*c) || matches!(c, b'-' | b'_'))
        };
        if !segments.iter().all(seg_ok) {
            return Err(OriginError::Grammar(
                "a path part is lowercase letters, digits, - and _ (no trailing /)",
            ));
        }
        Ok(Origin {
            host: host.to_string(),
            segments,
        })
    }

    /// The host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The registered text (`host` or `host/path`).
    pub fn text(&self) -> String {
        let mut t = self.host.clone();
        for s in &self.segments {
            t.push('/');
            t.push_str(s);
        }
        t
    }

    /// `https://<origin>/<rel>`.
    pub fn url(&self, rel: &str) -> String {
        format!("https://{}/{rel}", self.text())
    }

    /// Where its keys document is.
    pub fn keys_document_url(&self) -> String {
        self.url(KEYS_DOCUMENT_PATH)
    }

    /// The shared host this origin is on, when that host is not on the
    /// stable list: a suffix of [`shared_suffixes`] found as whole labels
    /// anywhere in the host (at its end, or inside it as in
    /// `someone.gitlab.io.example.dev`, which only reads like one).
    pub fn shared_host(&self) -> Option<String> {
        let labels: Vec<&str> = self.host.split('.').collect();
        if labels.contains(&SHARED_LABEL) {
            return Some(SHARED_LABEL.to_string());
        }
        let set = shared_suffixes();
        for i in 0..labels.len() {
            for j in i + 2..=labels.len() {
                let part = labels[i..j].join(".");
                if set.contains(part.as_str()) && !STABLE_SHARED_HOSTS.contains(&part.as_str()) {
                    return Some(part);
                }
            }
        }
        None
    }

    /// True when one of the two is a strict prefix of the other, part by
    /// part (`example.org` and `example.org/a`; never `example.org/a` and
    /// `example.org/ab`): whoever holds the shorter can write under the
    /// longer. Equal origins do not overlap: that is one origin.
    pub fn overlaps(&self, other: &Origin) -> bool {
        let (a, b) = (&self.segments, &other.segments);
        let n = a.len().min(b.len());
        self.host == other.host && a.len() != b.len() && a[..n] == b[..n]
    }
}

/// Parse `origin` and refuse a shared host and an overlap with any origin in
/// `registered`. A registered origin that does not parse is an error, never
/// skipped: an origin spelled another way (a trailing slash, a scheme, upper
/// case) would otherwise hide from every comparison (review APPLY-R1 M4).
pub fn check_origin<'a>(
    origin: &str,
    registered: impl IntoIterator<Item = &'a str>,
) -> Result<Origin, OriginError> {
    let o = Origin::parse(origin)?;
    if let Some(h) = o.shared_host() {
        return Err(OriginError::SharedHost(h));
    }
    for r in registered {
        let other = Origin::parse(r).map_err(|_| OriginError::Registered(r.to_string()))?;
        if o.overlaps(&other) {
            return Err(OriginError::Overlap(r.to_string()));
        }
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_words_match_inside_segments() {
        for ok in ["river-levels.open", "gauge_data:v2", "a.b", "open-rates.eu"] {
            assert_eq!(check_publisher_id(ok), Ok(()), "{ok}");
        }
        // A whole-segment match let `onyx-official.data` pass (review L1).
        assert_eq!(
            check_publisher_id("river-official.data"),
            Err(IdError::Reserved("official"))
        );
        assert_eq!(
            check_publisher_id("data.verified_rows"),
            Err(IdError::Reserved("verified"))
        );
        assert_eq!(
            check_publisher_id("reviewer"),
            Err(IdError::Reserved("reviewer"))
        );
        assert_eq!(
            check_publisher_id("node:operator-data"),
            Err(IdError::Reserved("operator"))
        );
        for onyx in ["onyx-official.data", "onyx", "my.onyxdata", "data.x_onyx"] {
            assert_eq!(check_publisher_id(onyx), Err(IdError::Network), "{onyx}");
        }
        for bad in [
            "",
            "River.open",
            "a..b",
            "a b",
            "-a",
            "a.",
            &"a".repeat(129),
        ] {
            assert_eq!(check_publisher_id(bad), Err(IdError::Grammar), "{bad:?}");
        }
    }

    #[test]
    fn the_same_id_and_origin_is_not_a_look_alike() {
        let reg = [("river-levels.open", "data.example.org/river-levels")];
        // The same pair: a second key.
        assert_eq!(
            look_alike("river-levels.open", "data.example.org/river-levels", reg),
            Ok(())
        );
        // One separator away, or the same id from another origin.
        for (id, origin) in [
            ("river_levels.open", "other.example.org"),
            ("riverlevels-open", "data.example.org/river-levels"),
            ("river.levels:open", "other.example.org"),
            ("river-levels.open", "other.example.org"),
        ] {
            assert_eq!(
                look_alike(id, origin, reg),
                Err(IdError::LookAlike("river-levels.open".into())),
                "{id} {origin}"
            );
        }
        assert_eq!(
            look_alike("lake-levels.open", "other.example.org", reg),
            Ok(())
        );
    }

    #[test]
    fn an_origin_that_is_a_prefix_of_another_is_refused() {
        let reg = ["example.org/a", "data.example.org", "example.net/x/y"];
        for bad in [
            "example.org",
            "example.org/a/b",
            "data.example.org/rivers",
            "example.net/x",
        ] {
            assert!(
                matches!(check_origin(bad, reg), Err(OriginError::Overlap(_))),
                "{bad}"
            );
        }
        // Not a prefix part by part, another host, or the same origin.
        for ok in [
            "example.org/ab",
            "example.org/b",
            "www.example.org",
            "example.org/a",
            "example.net/x/z",
        ] {
            assert!(check_origin(ok, reg).is_ok(), "{ok}");
        }
        // Two paths under one host, as the registered publishers have.
        assert!(check_origin("data.example.com/record", ["data.example.com/archive"]).is_ok());
    }

    #[test]
    fn an_origin_is_a_host_or_a_host_and_a_path() {
        let o = Origin::parse("data.example.org/river-levels").expect("origin");
        assert_eq!(o.host(), "data.example.org");
        assert_eq!(o.text(), "data.example.org/river-levels");
        assert_eq!(
            o.keys_document_url(),
            "https://data.example.org/river-levels/.well-known/onyx-source-keys.json"
        );
        assert_eq!(
            Origin::parse("example.org")
                .expect("origin")
                .keys_document_url(),
            "https://example.org/.well-known/onyx-source-keys.json"
        );
        for bad in [
            "",
            "https://example.org",
            "example.org/",
            "example.org//a",
            "example.org/a/",
            "example.org/A",
            "example.org/a?b",
            "example.org/..",
            "example.org/.well-known",
            "example.org:8443/a",
            "localhost/a",
            "10.0.0.1/a",
            "Example.org",
            "example.org/a/b/c/d/e",
        ] {
            assert!(Origin::parse(bad).is_err(), "{bad:?}");
        }
        // User information in front of the host (put together here, so no
        // address-shaped text stands in this file).
        assert!(Origin::parse(&format!("user{}example.org", '@')).is_err());
    }

    #[test]
    fn a_shared_host_is_refused() {
        for bad in [
            "someone.github.io",
            "someone.github.io/data",
            "github.io/x",
            "x.pages.dev",
        ] {
            assert!(
                matches!(check_origin(bad, []), Err(OriginError::SharedHost(_))),
                "{bad}"
            );
        }
        assert!(check_origin("notgithub.io", []).is_ok());
    }
    // ------------------------------------------------ review APPLY-R1

    /// APPLY-R1 M5: a separator, a digit for a letter or an ending does not
    /// hide the network's name or a reserved word.
    #[test]
    fn the_network_name_and_reserved_words_are_found_when_hidden() {
        for id in [
            "on-yx.data",
            "o_nyx.rivers",
            "on.yx",
            "0nyx.data",
            "onyks.data",
        ] {
            assert_eq!(check_publisher_id(id), Err(IdError::Network), "{id}");
        }
        for (id, word) in [
            ("offi-cial.data", "official"),
            ("officials.data", "official"),
            ("official1.data", "official"),
            ("0fficial.data", "official"),
            ("verif1ed.rows", "verified"),
            ("the-officialdata", "official"),
            ("reviewers.pick", "reviewer"),
            ("operators.open", "operator"),
            ("0perat0r.x", "operator"),
        ] {
            assert_eq!(check_publisher_id(id), Err(IdError::Reserved(word)), "{id}");
        }
    }

    /// APPLY-R1 M5: the look-alike rule folds digits and plurals.
    #[test]
    fn a_look_alike_with_a_digit_or_a_plural_is_refused() {
        let reg = [("river-levels.open", "data.example.org/river-levels")];
        for id in [
            "river-levels.0pen",
            "river-leve1s.open",
            "river-levels.opens",
            "riverlevel.open",
            "r1ver_level5.0pens",
        ] {
            assert_eq!(
                look_alike(id, "other.example.org", reg),
                Err(IdError::LookAlike("river-levels.open".into())),
                "{id}"
            );
        }
        assert_eq!(
            look_alike("river-flows.open", "other.example.org", reg),
            Ok(())
        );
        // The exemption is for the very same id and origin only.
        assert_eq!(
            look_alike("river-levels.open", "data.example.org/river-levels", reg),
            Ok(())
        );
        assert!(look_alike("river-levels.0pen", "data.example.org/river-levels", reg).is_err());
    }

    /// APPLY-R1 M6: hosts where anyone gets a name, from the list, also
    /// inside a host that only reads like one; punycode hosts.
    #[test]
    fn shared_hosts_of_the_list_and_punycode_are_refused() {
        for o in [
            "raw.githubusercontent.com/someone/data/main",
            "someone.blogspot.com",
            "pub-0123.r2.dev",
            "someone.trycloudflare.com",
            "someone.ngrok-free.app",
            "someone.duckdns.org",
            "someone.codeberg.page",
            "someone.onrender.com",
            "someone.fly.dev",
            "someone.azurewebsites.net",
            "d111111abcdef8.cloudfront.net",
            "someone.bitbucket.io",
            "someone.gitlab.io.example.dev",
        ] {
            assert!(
                matches!(check_origin(o, []), Err(OriginError::SharedHost(_))),
                "{o}"
            );
        }
        assert!(matches!(
            check_origin("xn--exmple-cua.org", []),
            Err(OriginError::Grammar(_))
        ));
        assert!(Origin::parse("data.xn--exmple-cua.org/a").is_err());
        // A blog host under a country ending, and a path-based code host.
        for o in [
            "someone.blogspot.co.uk",
            "github.com/someone/data",
            "sites.googleusercontent.com",
        ] {
            assert!(
                matches!(check_origin(o, []), Err(OriginError::SharedHost(_))),
                "{o}"
            );
        }
        // The list is our own file: well formed, sorted within reach of a
        // reader, no entry twice, every entry a host name of two labels or more.
        let text = include_str!("../data/shared-hosts.txt");
        let entries: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert!(entries.len() > 100);
        assert_eq!(
            entries.len(),
            shared_suffixes().len(),
            "an entry stands twice"
        );
        for e in &entries {
            assert!(is_origin_host(e), "{e}");
        }
        assert!(text.contains("NOT a copy"));
        for ok in [
            "data.example.org/river-levels",
            "example.org",
            "rivers.example.net",
        ] {
            assert!(check_origin(ok, []).is_ok(), "{ok}");
        }
    }

    /// APPLY-R1 M4: a registered origin spelled another way is an error,
    /// never skipped.
    #[test]
    fn an_unparsed_registered_origin_is_an_error() {
        for reg in [
            "example.org/a/",
            "https://example.org/a",
            "Example.org/a",
            "example.org/a/b/c/d/e",
        ] {
            assert_eq!(
                check_origin("example.org", [reg]),
                Err(OriginError::Registered(reg.to_string())),
                "{reg}"
            );
        }
    }
}
