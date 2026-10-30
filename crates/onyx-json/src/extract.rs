//! Deterministic extraction, version 1 (ADR-0053 item 4). Any failure is
//! [`Answer::Unavailable`], never a guess; [`read`] says why.

use crate::reader::{parse, Node, ReadError};
use onyx_core::claim::Subject;
use onyx_core::feed::{canonical_u64, civil_date, day_number};
use onyx_core::feed_json::{Expect, JsonDescriptor, TimeForm, ValueRule};
use onyx_core::value::MAX_FIXED_SCALE;
use onyx_core::{Answer, ObservationKind, ObservationSpec};

/// Why a body gave no value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Why {
    /// The descriptor is invalid.
    #[error("the descriptor is invalid")]
    BadDescriptor,
    /// The spec does not name this descriptor's fetcher or pointer.
    #[error("the feed does not name this descriptor")]
    NotThisFeed,
    /// The feed's kind does not fit the value rule.
    #[error("the feed's kind does not fit the value rule")]
    WrongKind,
    /// The document was refused.
    #[error("{0}")]
    Read(ReadError),
    /// An assert did not hold.
    #[error("assert on {0} does not hold")]
    Assert(String),
    /// A date assert, but the subject names no date.
    #[error("the subject names no date")]
    NoDate,
    /// A bucket-start assert, but the subject names no bucket (ADR-0058).
    #[error("the subject names no bucket")]
    NoBucket,
    /// Nothing at the pointer.
    #[error("nothing at the pointer")]
    Missing,
    /// The value is not of the rule's form.
    #[error("the value is not of the rule's form")]
    Form,
    /// An exponent (`1e5`): never read (ADR-0009).
    #[error("the number has an exponent")]
    Exponent,
    /// Non-zero digits beyond the feed's scale: the source changed its precision.
    #[error("non-zero digits beyond the feed's scale")]
    ExtraPrecision,
    /// The mantissa does not fit `i128`.
    #[error("the value does not fit 128 bits")]
    Overflow,
    /// The text is empty, too long or holds a control character.
    #[error("the text is empty, too long or holds a control character")]
    Text,
}

impl Why {
    /// A stable short code (the vectors file uses it).
    pub fn code(&self) -> &'static str {
        match self {
            Why::BadDescriptor => "bad_descriptor",
            Why::NotThisFeed => "not_this_feed",
            Why::WrongKind => "wrong_kind",
            Why::Read(e) => e.code(),
            Why::Assert(_) => "assert",
            Why::NoDate => "no_date",
            Why::NoBucket => "no_bucket",
            Why::Missing => "missing",
            Why::Form => "form",
            Why::Exponent => "exponent",
            Why::ExtraPrecision => "extra_precision",
            Why::Overflow => "overflow",
            Why::Text => "text",
        }
    }
}

/// The `YYYY-MM-DD` date a dated subject names: the text after its last `:`, a real day.
pub fn subject_date(s: &Subject) -> Option<&str> {
    let Subject::Aggregate(id) = s else {
        return None;
    };
    let (_, date) = id.rsplit_once(':')?;
    day_number(date).map(|_| date)
}

/// The bucket start a bucketed subject names (ADR-0058): the text after its last `:`, canonical
/// decimal Unix seconds. A dated subject (`YYYY-MM-DD`) names none.
pub fn subject_bucket_start(s: &Subject) -> Option<u64> {
    let Subject::Aggregate(id) = s else {
        return None;
    };
    let (_, start) = id.rsplit_once(':')?;
    canonical_u64(start)
}

/// Extract the answer of `spec` from `body` under `d`: [`Answer::Unavailable`] on any failure.
pub fn extract(d: &JsonDescriptor, spec: &ObservationSpec, body: &[u8]) -> Answer {
    if d.validate().is_err() || spec.fetcher != d.fetcher_id() || spec.selector != d.pointer {
        return Answer::Unavailable;
    }
    read_at(
        d,
        spec.kind,
        subject_date(&spec.subject),
        subject_bucket_start(&spec.subject),
        body,
    )
    .unwrap_or(Answer::Unavailable)
}

/// The answer of `body` under `d` for a feed of `kind` whose subject names `date`, or why not.
/// Does not check the fetcher id (see [`extract`]). A subject that names a bucket goes through
/// [`read_at`].
pub fn read(
    d: &JsonDescriptor,
    kind: ObservationKind,
    date: Option<&str>,
    body: &[u8],
) -> Result<Answer, Why> {
    read_at(d, kind, date, None, body)
}

/// The UTC text `YYYY-MM-DDTHH:MM:SSZ` of Unix second `t`.
pub fn rfc3339(t: u64) -> Option<String> {
    let day = i64::try_from(t / 86_400).ok()?;
    let s = t % 86_400;
    Some(format!(
        "{}T{:02}:{:02}:{:02}Z",
        civil_date(day),
        s / 3_600,
        s / 60 % 60,
        s % 60
    ))
}

/// Whether `t` is decimal text `-?(0|[1-9][0-9]*)(\.[0-9]+)?` strictly above zero.
fn positive_text(t: &str) -> bool {
    if t.starts_with('-') {
        return false;
    }
    let (int, frac) = match t.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (t, None),
    };
    let int_ok = !int.is_empty()
        && int.bytes().all(|c| c.is_ascii_digit())
        && (int.len() == 1 || !int.starts_with('0'));
    let frac_ok = frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|c| c.is_ascii_digit()));
    int_ok && frac_ok && t.bytes().any(|c| (b'1'..=b'9').contains(&c))
}

/// [`read`] for a subject that names `date` and/or the bucket starting at `bucket_start` (Unix
/// seconds, ADR-0058).
pub fn read_at(
    d: &JsonDescriptor,
    kind: ObservationKind,
    date: Option<&str>,
    bucket_start: Option<u64>,
    body: &[u8],
) -> Result<Answer, Why> {
    d.validate().map_err(|_| Why::BadDescriptor)?;
    match (d.value, kind) {
        (ValueRule::ArrayLen, ObservationKind::Numeric { scale: 0, .. }) => {}
        (ValueRule::ArrayLen, _) => return Err(Why::WrongKind),
        (v, ObservationKind::Numeric { scale, .. }) if v.is_numeric() => {
            if scale > MAX_FIXED_SCALE {
                return Err(Why::WrongKind);
            }
        }
        (ValueRule::Text { .. } | ValueRule::Bool, ObservationKind::Exact { .. }) => {}
        _ => return Err(Why::WrongKind),
    }
    let doc = parse(body, d.max_bytes as usize).map_err(Why::Read)?;
    for a in &d.asserts {
        let node = doc.pointer(&a.pointer);
        let fail = || Why::Assert(a.pointer.clone());
        let holds = match (&a.expect, node) {
            (_, None) => false,
            (Expect::Str(s), Some(Node::Str(v))) => v == s,
            (Expect::Int(i), Some(Node::Num(t))) => int_text(t) == Some(*i),
            (Expect::SubjectDate, Some(Node::Str(v))) => {
                let Some(date) = date else {
                    return Err(Why::NoDate);
                };
                v == date
            }
            (Expect::KeysExactly(keys), Some(Node::Obj(kv))) => {
                let mut have: Vec<&str> = kv.iter().map(|(k, _)| k.as_str()).collect();
                have.sort_unstable();
                have.len() == keys.len() && have.iter().zip(keys).all(|(a, b)| *a == b)
            }
            (Expect::ArrayLen(n), Some(Node::Arr(items))) => {
                u32::try_from(items.len()).ok() == Some(*n)
            }
            // ADR-0058 item 3 (THREATS CL8-1): the venue's bucket is exactly the subject's.
            (Expect::BucketStart(form), Some(n)) => {
                let Some(start) = bucket_start else {
                    return Err(Why::NoBucket);
                };
                match (form, n) {
                    (TimeForm::UnixSecs, Node::Num(t)) => int_text(t) == Some(i128::from(start)),
                    (TimeForm::UnixMillis, Node::Num(t)) => {
                        int_text(t) == i128::from(start).checked_mul(1_000)
                    }
                    (TimeForm::Rfc3339, Node::Str(v)) => rfc3339(start).as_deref() == Some(v),
                    _ => false,
                }
            }
            // THREATS CL8-11: a bucket with no trades is no reading.
            (Expect::Positive, Some(Node::Num(t) | Node::Str(t))) => positive_text(t),
            _ => false,
        };
        if !holds {
            return Err(fail());
        }
    }
    let node = doc.pointer(&d.pointer).ok_or(Why::Missing)?;
    match (d.value, kind) {
        (ValueRule::ArrayLen, _) => match node {
            Node::Arr(items) => i128::try_from(items.len())
                .map(Answer::Number)
                .map_err(|_| Why::Overflow),
            _ => Err(Why::Form),
        },
        (rule, ObservationKind::Numeric { scale, .. }) => {
            let text = match (rule, node) {
                (ValueRule::DecimalString | ValueRule::DecimalOrNumber, Node::Str(s)) => s,
                (ValueRule::JsonNumber | ValueRule::DecimalOrNumber, Node::Num(t)) => t,
                _ => return Err(Why::Form),
            };
            mantissa(text, scale).map(Answer::Number)
        }
        (ValueRule::Bool, ObservationKind::Exact { max_len }) => match node {
            Node::Bool(b) => {
                let v: &[u8] = if *b { b"true" } else { b"false" };
                if v.len() > usize::from(max_len) {
                    return Err(Why::Text);
                }
                Ok(Answer::Bytes(v.to_vec()))
            }
            _ => Err(Why::Form),
        },
        (ValueRule::Text { trim }, ObservationKind::Exact { max_len }) => match node {
            Node::Str(s) => {
                let v = if trim {
                    s.trim_matches(|c: char| c.is_ascii_whitespace())
                } else {
                    s.as_str()
                };
                if v.is_empty() || v.len() > usize::from(max_len) || v.chars().any(char::is_control)
                {
                    return Err(Why::Text);
                }
                Ok(Answer::Bytes(v.as_bytes().to_vec()))
            }
            _ => Err(Why::Form),
        },
        _ => Err(Why::WrongKind),
    }
}

/// An integer token `-?(0|[1-9][0-9]*)` that fits `i128`.
fn int_text(t: &str) -> Option<i128> {
    let digits = t.strip_prefix('-').unwrap_or(t);
    let ok = !digits.is_empty()
        && digits.bytes().all(|c| c.is_ascii_digit())
        && (digits.len() == 1 || !digits.starts_with('0'));
    ok.then(|| t.parse().ok()).flatten()
}

/// The mantissa of decimal text `-?(0|[1-9][0-9]*)(\.[0-9]+)?` at `scale`: digits beyond the
/// scale must be zeros, the result must fit `i128`. Never through a float (ADR-0009).
pub fn mantissa(text: &str, scale: u8) -> Result<i128, Why> {
    let (neg, rest) = match text.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, text),
    };
    if rest.contains(['e', 'E']) {
        return Err(Why::Exponent);
    }
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    let int_ok = !int.is_empty()
        && int.bytes().all(|c| c.is_ascii_digit())
        && (int.len() == 1 || !int.starts_with('0'));
    let frac = frac.unwrap_or("");
    let frac_ok =
        frac.bytes().all(|c| c.is_ascii_digit()) && (!rest.contains('.') || !frac.is_empty());
    if !int_ok || !frac_ok {
        return Err(Why::Form);
    }
    let scale = usize::from(scale);
    if frac.len() > scale && frac.as_bytes()[scale..].iter().any(|c| *c != b'0') {
        return Err(Why::ExtraPrecision);
    }
    let mut m: i128 = 0;
    let push = |m: i128, c: u8| -> Result<i128, Why> {
        m.checked_mul(10)
            .and_then(|m| m.checked_add(i128::from(c - b'0')))
            .ok_or(Why::Overflow)
    };
    for c in int.bytes() {
        m = push(m, c)?;
    }
    for k in 0..scale {
        m = push(m, frac.as_bytes().get(k).copied().unwrap_or(b'0'))?;
    }
    Ok(if neg { -m } else { m })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mantissas() {
        assert_eq!(mantissa("1052530", 0), Ok(1_052_530));
        assert_eq!(mantissa("12.5", 2), Ok(1250));
        assert_eq!(mantissa("-0.07", 2), Ok(-7));
        assert_eq!(mantissa("12.500", 1), Ok(125));
        assert_eq!(mantissa("12.51", 1), Err(Why::ExtraPrecision));
        assert_eq!(mantissa("1e3", 0), Err(Why::Exponent));
        for bad in [
            "", "-", "01", "1.", ".1", "+1", "1,000", " 1", "1_0", "--1", "0x10",
        ] {
            assert_eq!(mantissa(bad, 2), Err(Why::Form), "{bad}");
        }
        assert_eq!(
            mantissa("170141183460469231731687303715884105727", 0),
            Ok(i128::MAX)
        );
        assert_eq!(
            mantissa("170141183460469231731687303715884105728", 0),
            Err(Why::Overflow)
        );
        assert_eq!(mantissa("1", 38), Ok(10i128.pow(38)));
        assert_eq!(mantissa("2", 38), Err(Why::Overflow));
    }
}
