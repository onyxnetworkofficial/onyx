//! The generic level-2 JSON descriptor (ADR-0053, contributor loop CL3).
//!
//! A [`JsonDescriptor`] is the whole extraction rule of a public JSON source: the RFC 6901
//! pointer to the value, how the value is read ([`ValueRule`]), its unit, the asserts that pin
//! the row ([`Assert`]), the largest body read and the access mode. One fetcher
//! (`onyx_json::GenericJsonFetcher`) runs every descriptor; its id is
//! [`fetcher_id`] = `keccak256("onyx-fetcher:generic-json:v1" ‖ borsh(descriptor))`, so the
//! feed template's id, and with it the on-chain pin (`OnyxRegistry.registerMetric`), commits to
//! every rule, the unit included (THREATS CL3-5). [`FeedTemplate`] does not change, so no live
//! template id moves.
//!
//! Types and checks only: no serde, no I/O (the reader and `extract` are in `onyx-json`).

use crate::feed::{FeedTemplate, SubjectRule, DATE_PLACEHOLDER};
use crate::{keccak256, Hash32, ObservationKind};
use borsh::{BorshDeserialize, BorshSerialize};

/// The only descriptor version.
pub const DESCRIPTOR_VERSION: u8 = 1;
/// Version tag the generic fetcher's id is derived from.
pub const FETCHER_TAG: &[u8] = b"onyx-fetcher:generic-json:v1";
/// Longest pointer, in bytes.
pub const MAX_POINTER: usize = 256;
/// Longest unit, in bytes.
pub const MAX_UNIT: usize = 32;
/// Most asserts.
pub const MAX_ASSERTS: usize = 8;
/// Most keys in a [`Expect::KeysExactly`].
pub const MAX_KEYS: usize = 32;
/// Longest expected string or key, in bytes.
pub const MAX_EXPECT: usize = 256;
/// Largest `max_bytes` (1 MiB).
pub const MAX_BYTES_CEILING: u32 = 1_048_576;
/// Deepest nesting a document may have.
pub const MAX_DEPTH: usize = 64;

/// How the value at the pointer is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ValueRule {
    /// A JSON string holding a decimal number (`-?(0|[1-9][0-9]*)(\.[0-9]+)?`, nothing else).
    /// A `Numeric` feed, at the template's scale.
    DecimalString,
    /// A JSON number token of the same grammar (no exponent). A `Numeric` feed.
    JsonNumber,
    /// Either of the two. A `Numeric` feed.
    DecimalOrNumber,
    /// The length of the array at the pointer: a count. A `Numeric` feed of scale 0.
    ArrayLen,
    /// A JSON string, 1..=`max_len` bytes with no control character. An `Exact` feed.
    Text {
        /// Whether leading and trailing ASCII whitespace is removed first.
        trim: bool,
    },
    /// A JSON `true` or `false`, read as the bytes `true` or `false`. An `Exact` feed.
    Bool,
}

impl ValueRule {
    /// Whether the rule reads a number (a `Numeric` feed).
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            ValueRule::DecimalString
                | ValueRule::JsonNumber
                | ValueRule::DecimalOrNumber
                | ValueRule::ArrayLen
        )
    }
}

/// What an assert expects at its pointer.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Expect {
    /// A JSON string equal to this text.
    Str(String),
    /// A JSON integer (no fraction, no exponent) equal to this.
    Int(i128),
    /// A JSON string equal to the subject's `YYYY-MM-DD` date.
    SubjectDate,
    /// An object with exactly these keys (sorted, unique, at most [`MAX_KEYS`]).
    KeysExactly(Vec<String>),
    /// An array of exactly this length.
    ArrayLen(u32),
    /// The bucket's start time, in this form, equal to the start a bucketed subject names
    /// (ADR-0058 item 3, THREATS CL8-1): a venue that answers another bucket (off by one
    /// interval, a local-time day, a clock offset) gives no value. Appended, so no existing
    /// descriptor's Borsh or fetcher id moves.
    BucketStart(TimeForm),
    /// A positive decimal (a JSON number token or a decimal string, `-?(0|[1-9][0-9]*)(\.[0-9]+)?`,
    /// strictly above zero): a bucket with no trades (a halted or stale venue repeating its
    /// last price) gives no value (THREATS CL8-11). Appended.
    Positive,
}

/// How a venue writes a bucket's start time ([`Expect::BucketStart`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TimeForm {
    /// A JSON integer of Unix seconds.
    UnixSecs,
    /// A JSON integer of Unix milliseconds.
    UnixMillis,
    /// A JSON string `YYYY-MM-DDTHH:MM:SSZ` (UTC, no fraction, no offset).
    Rfc3339,
}

impl Expect {
    /// Whether the assert pins one row of a list (a string, an integer, the date or the
    /// bucket's start).
    pub fn pins_a_row(&self) -> bool {
        matches!(
            self,
            Expect::Str(_) | Expect::Int(_) | Expect::SubjectDate | Expect::BucketStart(_)
        )
    }
}

/// One check that must hold before the value is read.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Assert {
    /// RFC 6901 pointer.
    pub pointer: String,
    /// What must be there.
    pub expect: Expect,
}

/// Who may fetch the source.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Access {
    /// Anyone, with no account, key or payment.
    Public,
    /// Only a holder of a provider API key (ADR-0054 item 1, contributor loop CL4). The value
    /// must not depend on the key (a public series behind a metering key). This states WHERE
    /// the key goes, never anything about a key: each key holder adds its own key on its own
    /// machine for that one request. Appended after `Public`, so every public descriptor's
    /// Borsh, fetcher id and template id are unchanged.
    Keyed {
        /// Where the key goes.
        place: KeyPlace,
        /// The header or query parameter name ([`is_key_name`]).
        name: String,
        /// How the key is written.
        scheme: KeyScheme,
    },
}

/// Where a keyed source's key goes (ADR-0054 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum KeyPlace {
    /// A request header (`name`, lower case).
    Header,
    /// A query parameter: the template URL holds `name=`[`KEY_PLACEHOLDER`] exactly once.
    Query,
}

/// How a keyed source's key is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum KeyScheme {
    /// The key as it is.
    Raw,
    /// `Bearer <key>` (the `authorization` header only).
    Bearer,
}

/// The placeholder a keyed template's URL holds where a query key goes. Only the key
/// holder's own client fills it, for its own request; it is never filled anywhere else.
pub const KEY_PLACEHOLDER: &str = "{api_key}";

/// Longest key header or parameter name, in bytes.
pub const MAX_KEY_NAME: usize = 64;

/// Headers a keyed source may not use for its key: the fetch sets them itself, or they
/// change how the request travels.
pub const RESERVED_HEADERS: [&str; 14] = [
    "accept",
    "accept-encoding",
    "connection",
    "content-length",
    "content-type",
    "cookie",
    "forwarded",
    "host",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
];

/// Whether `name` may carry a key at `place`: a header name is lower-case `a-z`, `0-9` and
/// `-` (not reserved, no `proxy-` or `x-forwarded-` prefix); a query name is `A-Z`, `a-z`,
/// `0-9`, `_`, `.` and `-`; 1..=[`MAX_KEY_NAME`] bytes either way.
pub fn is_key_name(place: KeyPlace, name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_KEY_NAME {
        return false;
    }
    match place {
        KeyPlace::Header => {
            name.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                && !name.starts_with('-')
                && !RESERVED_HEADERS.contains(&name)
                && !name.starts_with("proxy-")
                && !name.starts_with("x-forwarded-")
        }
        KeyPlace::Query => name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-')),
    }
}

impl Access {
    /// Whether a key is needed.
    pub fn is_keyed(&self) -> bool {
        matches!(self, Access::Keyed { .. })
    }

    /// Refuse a keyed access whose name is not a key name for its place, or `Bearer` outside
    /// the `authorization` header.
    pub fn validate(&self) -> Result<(), DescriptorError> {
        match self {
            Access::Public => Ok(()),
            Access::Keyed {
                place,
                name,
                scheme,
            } => {
                if !is_key_name(*place, name) {
                    return Err(DescriptorError::Bad("key name"));
                }
                if *scheme == KeyScheme::Bearer
                    && (*place != KeyPlace::Header || name != "authorization")
                {
                    return Err(DescriptorError::Bad(
                        "a Bearer key goes in the authorization header",
                    ));
                }
                Ok(())
            }
        }
    }
}

/// The whole extraction rule of a public JSON source (ADR-0053 item 3).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct JsonDescriptor {
    /// [`DESCRIPTOR_VERSION`].
    pub version: u8,
    /// RFC 6901 pointer to the value, 1..=[`MAX_POINTER`] visible ASCII bytes.
    pub pointer: String,
    /// How the value is read.
    pub value: ValueRule,
    /// The value's unit, 1..=[`MAX_UNIT`] printable ASCII bytes, no outer space.
    pub unit: String,
    /// Checks, sorted by pointer, unique, at most [`MAX_ASSERTS`].
    pub asserts: Vec<Assert>,
    /// Largest body read, 1..=[`MAX_BYTES_CEILING`].
    pub max_bytes: u32,
    /// Who may fetch.
    pub access: Access,
}

/// Why a descriptor, or a template it is used with, is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    /// A field is out of its bounds.
    #[error("json descriptor invalid: {0}")]
    Bad(&'static str),
    /// The value is read through an array index no assert pins (THREATS CL3-4).
    #[error("the pointer reads element {0} of a list, but no assert pins that row (a Str, Int or SubjectDate assert on it)")]
    UnpinnedRow(String),
    /// The template does not go with this descriptor.
    #[error("template does not match the descriptor: {0}")]
    Template(&'static str),
}

/// Whether `p` is a well-formed RFC 6901 pointer of 1..=[`MAX_POINTER`] visible ASCII bytes
/// (`~` only as `~0` or `~1`).
pub fn is_pointer(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() || b.len() > MAX_POINTER || b[0] != b'/' {
        return false;
    }
    if !b.iter().all(|c| (0x21..=0x7e).contains(c)) {
        return false;
    }
    b.iter()
        .enumerate()
        .all(|(i, c)| *c != b'~' || matches!(b.get(i + 1), Some(b'0' | b'1')))
}

/// The pointer's reference tokens, unescaped (`~1` is `/`, `~0` is `~`).
pub fn tokens(p: &str) -> Vec<String> {
    p.split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// Whether a token could be an array index (`0` or digits without a leading zero).
pub fn is_index(t: &str) -> bool {
    !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit()) && (t.len() == 1 || !t.starts_with('0'))
}

fn printable(s: &str) -> bool {
    s.bytes().all(|c| (0x20..=0x7e).contains(&c))
}

impl JsonDescriptor {
    /// Refuse anything out of bounds, and a value read through a list element no assert pins.
    pub fn validate(&self) -> Result<(), DescriptorError> {
        let bad = |why| Err(DescriptorError::Bad(why));
        if self.version != DESCRIPTOR_VERSION {
            return bad("version");
        }
        if !is_pointer(&self.pointer) || self.pointer.contains(DATE_PLACEHOLDER) {
            return bad("pointer");
        }
        if self.unit.is_empty()
            || self.unit.len() > MAX_UNIT
            || !printable(&self.unit)
            || self.unit.trim() != self.unit
        {
            return bad("unit");
        }
        if self.max_bytes == 0 || self.max_bytes > MAX_BYTES_CEILING {
            return bad("max_bytes");
        }
        self.access.validate()?;
        if self.asserts.len() > MAX_ASSERTS {
            return bad("too many asserts");
        }
        if !self
            .asserts
            .windows(2)
            .all(|w| w[0].pointer.as_bytes() < w[1].pointer.as_bytes())
        {
            return bad("asserts must be sorted by pointer, without duplicates");
        }
        for a in &self.asserts {
            if !is_pointer(&a.pointer) {
                return bad("assert pointer");
            }
            match &a.expect {
                Expect::Str(s) if s.len() > MAX_EXPECT || s.chars().any(char::is_control) => {
                    return bad("expected text");
                }
                Expect::KeysExactly(keys)
                    if keys.len() > MAX_KEYS
                        || keys
                            .iter()
                            .any(|k| k.len() > MAX_EXPECT || k.chars().any(char::is_control))
                        || !keys.windows(2).all(|w| w[0].as_bytes() < w[1].as_bytes()) =>
                {
                    return bad("expected keys must be sorted, unique, at most 32");
                }
                _ => {}
            }
        }
        // THREATS CL3-4: every list element on the way to the value is pinned by an assert on
        // that same element, so a list the source reorders never moves the value silently.
        let toks: Vec<&str> = self.pointer.split('/').skip(1).collect();
        for (i, t) in toks.iter().enumerate() {
            if !is_index(&t.replace("~1", "/").replace("~0", "~")) {
                continue;
            }
            let element = format!("/{}", toks[..=i].join("/"));
            let under = format!("{element}/");
            let pinned = self.asserts.iter().any(|a| {
                a.expect.pins_a_row() && (a.pointer == element || a.pointer.starts_with(&under))
            });
            if !pinned {
                return Err(DescriptorError::UnpinnedRow(element));
            }
        }
        if self
            .asserts
            .iter()
            .any(|a| a.pointer.contains(DATE_PLACEHOLDER))
        {
            return bad("assert pointer holds {date}");
        }
        Ok(())
    }

    /// Whether any assert reads the subject's date.
    pub fn uses_subject_date(&self) -> bool {
        self.asserts.iter().any(|a| a.expect == Expect::SubjectDate)
    }

    /// The generic fetcher's id for this descriptor: `keccak256(FETCHER_TAG ‖ borsh(self))`.
    pub fn fetcher_id(&self) -> Hash32 {
        let mut b = FETCHER_TAG.to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }

    /// Whether `t` is a template this descriptor can serve: it names this descriptor's fetcher,
    /// its selector is the pointer, its kind fits the value rule, and a date assert has a dated
    /// subject (a dated or daily rule; a period rule puts `{date}` in the selector, which a
    /// descriptor's pointer never holds).
    pub fn check_template(&self, t: &FeedTemplate) -> Result<(), DescriptorError> {
        self.validate()?;
        if t.fetcher != self.fetcher_id() {
            return Err(DescriptorError::Template(
                "the fetcher is not this descriptor's",
            ));
        }
        if t.selector != self.pointer {
            return Err(DescriptorError::Template("the selector is not the pointer"));
        }
        // ADR-0054 item 1: a query key's template holds `name={api_key}` exactly once, as a
        // query parameter; nothing else ever holds the placeholder.
        let holes = t.url_template.matches(KEY_PLACEHOLDER).count();
        match &self.access {
            Access::Keyed {
                place: KeyPlace::Query,
                name,
                ..
            } => {
                let query = t.url_template.split_once('?').map_or("", |(_, q)| q);
                let pair = format!("{name}={KEY_PLACEHOLDER}");
                if holes != 1 || !query.split('&').any(|p| p == pair) {
                    return Err(DescriptorError::Template(
                        "a query key's URL holds name={api_key} exactly once, in its query",
                    ));
                }
            }
            _ if holes != 0 => {
                return Err(DescriptorError::Template(
                    "only a query key's URL holds {api_key}",
                ));
            }
            _ => {}
        }
        match (self.value, t.kind) {
            (ValueRule::ArrayLen, ObservationKind::Numeric { scale: 0, .. }) => {}
            (ValueRule::ArrayLen, _) => {
                return Err(DescriptorError::Template("a count is Numeric of scale 0"));
            }
            // Review CL3-R1-7: a scale the extraction never reads would load, register and then
            // answer Unavailable in every round.
            (_, ObservationKind::Numeric { scale, .. })
                if scale > crate::value::MAX_FIXED_SCALE =>
            {
                return Err(DescriptorError::Template("scale above 38"));
            }
            (v, ObservationKind::Numeric { .. }) if v.is_numeric() => {}
            (ValueRule::Text { .. } | ValueRule::Bool, ObservationKind::Exact { max_len })
                if max_len >= 5 || matches!(self.value, ValueRule::Text { .. }) => {}
            _ => {
                return Err(DescriptorError::Template(
                    "the kind does not fit the value rule",
                ));
            }
        }
        let bucketed = matches!(t.subject, SubjectRule::Bucketed { .. });
        if self.uses_bucket_start() && !bucketed {
            return Err(DescriptorError::Template(
                "a bucket-start assert needs a bucketed subject",
            ));
        }
        // CL7: a recheck reads its original's subjects and URL, so it fits
        // where the original does.
        let rule = match &t.subject {
            SubjectRule::Recheck { of, .. } => of.as_ref(),
            r => r,
        };
        match rule {
            SubjectRule::Dated { .. } | SubjectRule::Daily { .. } => Ok(()),
            SubjectRule::Fixed(_) if !self.uses_subject_date() => Ok(()),
            SubjectRule::Fixed(_) => Err(DescriptorError::Template(
                "a date assert needs a dated subject",
            )),
            SubjectRule::Period { .. } => Err(DescriptorError::Template(
                "a period subject is not served by the generic fetcher",
            )),
            // ADR-0058 items 1 and 3: a venue's closed bucket is pinned by its start time (THREATS
            // CL8-1), is a number, and every observer reads the same finished record, so they
            // agree exactly (tolerance 0).
            SubjectRule::Bucketed { .. } => {
                if !self.uses_bucket_start() {
                    return Err(DescriptorError::Template(
                        "a bucketed subject needs a bucket-start assert",
                    ));
                }
                if self.uses_subject_date() {
                    return Err(DescriptorError::Template(
                        "a bucketed subject names no date",
                    ));
                }
                // Review CL8-R1-4, CL8-R1-5: the bucket's start and a positive volume are read
                // from the same object as the value, so a list's other row (the next bucket) or
                // an empty bucket never gives the value.
                let parent = |p: &str| p.rsplit_once('/').map_or("", |(a, _)| a).to_owned();
                let row = parent(&self.pointer);
                let on_row = |want: fn(&Expect) -> bool| {
                    self.asserts
                        .iter()
                        .any(|a| want(&a.expect) && parent(&a.pointer) == row)
                };
                if !on_row(|e| matches!(e, Expect::BucketStart(_))) {
                    return Err(DescriptorError::Template(
                        "a bucketed feed's bucket-start assert is on the value's own object",
                    ));
                }
                if !on_row(|e| *e == Expect::Positive) {
                    return Err(DescriptorError::Template(
                        "a bucketed feed asserts a positive volume on the value's own object",
                    ));
                }
                if !matches!(t.kind, ObservationKind::Numeric { tolerance: 0, .. })
                    || self.value == ValueRule::ArrayLen
                {
                    return Err(DescriptorError::Template(
                        "a bucketed feed is a price-like number read with tolerance 0",
                    ));
                }
                Ok(())
            }
            SubjectRule::Recheck { .. } => Err(DescriptorError::Template("a recheck of a recheck")),
        }
    }

    /// Whether any assert pins the bucket's start ([`Expect::BucketStart`]).
    pub fn uses_bucket_start(&self) -> bool {
        self.asserts
            .iter()
            .any(|a| matches!(a.expect, Expect::BucketStart(_)))
    }
}

/// `keccak256(FETCHER_TAG ‖ borsh(d))`.
pub fn fetcher_id(d: &JsonDescriptor) -> Hash32 {
    d.fetcher_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tga() -> JsonDescriptor {
        JsonDescriptor {
            version: 1,
            pointer: "/data/0/open_today_bal".into(),
            value: ValueRule::DecimalString,
            unit: "million USD".into(),
            asserts: vec![
                Assert {
                    pointer: "/data".into(),
                    expect: Expect::ArrayLen(1),
                },
                Assert {
                    pointer: "/data/0/account_type".into(),
                    expect: Expect::Str("Treasury General Account (TGA) Closing Balance".into()),
                },
                Assert {
                    pointer: "/data/0/record_date".into(),
                    expect: Expect::SubjectDate,
                },
            ],
            max_bytes: 65_536,
            access: Access::Public,
        }
    }

    #[test]
    fn a_valid_descriptor_passes() {
        tga().validate().expect("valid");
        assert!(tga().uses_subject_date());
    }

    /// THREATS CL3-4: a pointer through a list element needs an assert that pins that element.
    #[test]
    fn a_descriptor_reading_an_array_without_a_row_assert_is_invalid() {
        let mut d = tga();
        d.asserts.retain(|a| !a.expect.pins_a_row());
        assert_eq!(
            d.validate(),
            Err(DescriptorError::UnpinnedRow("/data/0".into()))
        );
        // A key-set or length assert does not pin a row.
        d.asserts.push(Assert {
            pointer: "/data/0".into(),
            expect: Expect::KeysExactly(vec!["a".into()]),
        });
        d.asserts.sort_by(|a, b| a.pointer.cmp(&b.pointer));
        assert!(matches!(d.validate(), Err(DescriptorError::UnpinnedRow(_))));
        // An assert on another element does not pin this one.
        let mut other = tga();
        for a in &mut other.asserts {
            a.pointer = a.pointer.replace("/data/0/", "/data/1/");
        }
        assert!(matches!(
            other.validate(),
            Err(DescriptorError::UnpinnedRow(_))
        ));
        // Nested lists need a pin per level.
        let mut nested = tga();
        nested.pointer = "/data/0/rows/3/v".into();
        assert_eq!(
            nested.validate(),
            Err(DescriptorError::UnpinnedRow("/data/0/rows/3".into()))
        );
        nested.asserts.push(Assert {
            pointer: "/data/0/rows/3/k".into(),
            expect: Expect::Int(7),
        });
        nested.asserts.sort_by(|a, b| a.pointer.cmp(&b.pointer));
        nested.validate().expect("both rows pinned");
    }

    #[test]
    fn out_of_bounds_fields_are_refused() {
        type Edit = Box<dyn Fn(&mut JsonDescriptor)>;
        let cases: Vec<Edit> = vec![
            Box::new(|d| d.version = 2),
            Box::new(|d| d.pointer = String::new()),
            Box::new(|d| d.pointer = "data".into()),
            Box::new(|d| d.pointer = "/a b".into()),
            Box::new(|d| d.pointer = "/a~2".into()),
            Box::new(|d| d.pointer = "/a~".into()),
            Box::new(|d| d.pointer = format!("/{}", "a".repeat(256))),
            Box::new(|d| d.pointer = "/{date}".into()),
            Box::new(|d| d.unit = String::new()),
            Box::new(|d| d.unit = " USD".into()),
            Box::new(|d| d.unit = "x".repeat(33)),
            Box::new(|d| d.unit = "m\u{b5}".into()),
            Box::new(|d| d.max_bytes = 0),
            Box::new(|d| d.max_bytes = MAX_BYTES_CEILING + 1),
            Box::new(|d| d.asserts.reverse()),
            Box::new(|d| {
                let a = d.asserts[0].clone();
                d.asserts.insert(0, a);
            }),
            Box::new(|d| {
                for i in 0..9 {
                    d.asserts.push(Assert {
                        pointer: format!("/z{i}"),
                        expect: Expect::Int(1),
                    });
                }
            }),
            Box::new(|d| d.asserts[0].expect = Expect::KeysExactly(vec!["b".into(), "a".into()])),
            Box::new(|d| d.asserts[1].expect = Expect::Str("a\nb".into())),
        ];
        for (i, f) in cases.iter().enumerate() {
            let mut d = tga();
            f(&mut d);
            assert!(d.validate().is_err(), "case {i}: {d:?}");
        }
    }

    fn keyed(place: KeyPlace, name: &str, scheme: KeyScheme) -> Access {
        Access::Keyed {
            place,
            name: name.into(),
            scheme,
        }
    }

    /// ADR-0054 item 1: a keyed descriptor names where the key goes, with names a header or a
    /// query can carry and that the fetch does not set itself.
    #[test]
    fn keyed_access_names_are_checked() {
        for ok in [
            keyed(KeyPlace::Header, "x-api-key", KeyScheme::Raw),
            keyed(KeyPlace::Header, "authorization", KeyScheme::Bearer),
            keyed(KeyPlace::Header, "authorization", KeyScheme::Raw),
            keyed(KeyPlace::Query, "api_key", KeyScheme::Raw),
            keyed(KeyPlace::Query, "appId.v2", KeyScheme::Raw),
        ] {
            let mut d = tga();
            d.access = ok.clone();
            d.validate().unwrap_or_else(|e| panic!("{ok:?}: {e}"));
        }
        for bad in [
            keyed(KeyPlace::Header, "", KeyScheme::Raw),
            keyed(KeyPlace::Header, "X-Api-Key", KeyScheme::Raw),
            keyed(KeyPlace::Header, "host", KeyScheme::Raw),
            keyed(KeyPlace::Header, "user-agent", KeyScheme::Raw),
            keyed(KeyPlace::Header, "cookie", KeyScheme::Raw),
            keyed(KeyPlace::Header, "proxy-authorization", KeyScheme::Raw),
            keyed(KeyPlace::Header, "x-forwarded-for", KeyScheme::Raw),
            keyed(KeyPlace::Header, "a b", KeyScheme::Raw),
            keyed(KeyPlace::Header, "x-api-key", KeyScheme::Bearer),
            keyed(KeyPlace::Query, "api_key", KeyScheme::Bearer),
            keyed(KeyPlace::Query, "a=b", KeyScheme::Raw),
            keyed(KeyPlace::Query, "{api_key}", KeyScheme::Raw),
            keyed(KeyPlace::Query, &"k".repeat(65), KeyScheme::Raw),
        ] {
            let mut d = tga();
            d.access = bad.clone();
            assert!(d.validate().is_err(), "{bad:?}");
        }
    }

    /// The keyed variant is appended: a public descriptor's Borsh (and so every live fetcher
    /// and template id) is unchanged, and the access is in the fetcher id.
    #[test]
    fn keyed_access_is_in_the_fetcher_id_and_public_bytes_are_unchanged() {
        let public = tga();
        let bytes = borsh::to_vec(&public).expect("borsh");
        assert_eq!(
            bytes.last(),
            Some(&0),
            "Access::Public is variant 0, last byte"
        );
        let mut k = tga();
        k.access = keyed(KeyPlace::Header, "x-api-key", KeyScheme::Raw);
        let mut k2 = tga();
        k2.access = keyed(KeyPlace::Header, "x-key", KeyScheme::Raw);
        assert_ne!(k.fetcher_id(), public.fetcher_id());
        assert_ne!(k.fetcher_id(), k2.fetcher_id());
    }

    /// The `{api_key}` placeholder is only in a query key's URL, once, as `name={api_key}`.
    #[test]
    fn only_a_query_key_template_holds_the_placeholder() {
        use crate::claim::MetricId;
        use crate::{DataRights, Redistribution, SourceId};
        let template = |d: &JsonDescriptor, url: &str| FeedTemplate {
            feed: "m".into(),
            metric: MetricId("m".into()),
            fetcher: d.fetcher_id(),
            url_template: url.into(),
            selector: d.pointer.clone(),
            kind: ObservationKind::Numeric {
                scale: 0,
                tolerance: 0,
            },
            window_secs: 120,
            min_observers: 3,
            rights: DataRights {
                license: "CC0-1.0".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId("s".into()),
                attribution: None,
                terms: String::new(),
            },
            subject: SubjectRule::Daily {
                prefix: "p".into(),
                min_days: 0,
                max_days: 5,
            },
        };
        let mut q = tga();
        q.access = keyed(KeyPlace::Query, "api_key", KeyScheme::Raw);
        let ok = "https://e.example.org/v?d={date}&api_key={api_key}";
        q.check_template(&template(&q, ok)).expect("query key");
        for bad in [
            "https://e.example.org/v?d={date}",
            "https://e.example.org/v?d={date}&key={api_key}",
            "https://e.example.org/{api_key}?d={date}&api_key={api_key}",
            "https://e.example.org/v?d={date}&api_key={api_key}&x={api_key}",
            "https://e.example.org/v?d={date}&api_key={api_key}x",
        ] {
            assert!(q.check_template(&template(&q, bad)).is_err(), "{bad}");
        }
        let mut h = tga();
        h.access = keyed(KeyPlace::Header, "x-api-key", KeyScheme::Raw);
        h.check_template(&template(&h, "https://e.example.org/v?d={date}"))
            .expect("header key");
        assert!(h.check_template(&template(&h, ok)).is_err());
        let p = tga();
        assert!(p.check_template(&template(&p, ok)).is_err());
    }

    /// THREATS CL3-5: the unit, the scale rule and every assert are in the fetcher id.
    #[test]
    fn every_field_moves_the_fetcher_id() {
        let base = tga().fetcher_id();
        let mut unit = tga();
        unit.unit = "USD".into();
        let mut rule = tga();
        rule.value = ValueRule::DecimalOrNumber;
        let mut cap = tga();
        cap.max_bytes += 1;
        let mut row = tga();
        row.asserts[1].expect =
            Expect::Str("Treasury General Account (TGA) Opening Balance".into());
        for d in [unit, rule, cap, row] {
            assert_ne!(d.fetcher_id(), base);
        }
        assert_eq!(tga().fetcher_id(), base);
    }
}
