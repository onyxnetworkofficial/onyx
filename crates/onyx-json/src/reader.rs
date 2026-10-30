//! A strict JSON reader (RFC 8259) for the generic fetcher (ADR-0053 item 4).
//!
//! Stricter than a general parser where two parsers could disagree (THREATS CL3-3):
//! - the body must be valid UTF-8 with no byte-order mark, at most `max_bytes` long;
//! - a duplicate object key anywhere (compared after unescaping) refuses the whole document,
//!   since parsers disagree on which copy wins;
//! - nesting deeper than [`MAX_DEPTH`] is refused;
//! - a lone UTF-16 surrogate in a `\u` escape is refused;
//! - a number keeps its exact text: nothing here ever converts it to a binary float (ADR-0009).

use onyx_core::feed_json::MAX_DEPTH;
use std::collections::BTreeSet;

/// A parsed JSON value. Objects keep their keys in document order (unique by construction).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, as its exact text.
    Num(String),
    /// A string, unescaped.
    Str(String),
    /// An array.
    Arr(Vec<Node>),
    /// An object, keys unique.
    Obj(Vec<(String, Node)>),
}

/// Why a document is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// Longer than the cap.
    #[error("the body is over {0} bytes")]
    TooLarge(usize),
    /// Not UTF-8.
    #[error("the body is not UTF-8")]
    NotUtf8,
    /// Starts with a byte-order mark.
    #[error("the body starts with a byte-order mark")]
    ByteOrderMark,
    /// Nested deeper than [`MAX_DEPTH`].
    #[error("nested deeper than {MAX_DEPTH}")]
    TooDeep,
    /// A key appears twice in one object.
    #[error("duplicate object key {0:?}")]
    DuplicateKey(String),
    /// Not JSON at this byte offset.
    #[error("not JSON at byte {0}")]
    Syntax(usize),
}

impl ReadError {
    /// A stable short code.
    pub fn code(&self) -> &'static str {
        match self {
            ReadError::TooLarge(_) => "too_large",
            ReadError::NotUtf8 => "not_utf8",
            ReadError::ByteOrderMark => "byte_order_mark",
            ReadError::TooDeep => "too_deep",
            ReadError::DuplicateKey(_) => "duplicate_key",
            ReadError::Syntax(_) => "syntax",
        }
    }
}

/// Parse `body` strictly, refusing anything over `max_bytes`.
pub fn parse(body: &[u8], max_bytes: usize) -> Result<Node, ReadError> {
    if body.len() > max_bytes {
        return Err(ReadError::TooLarge(max_bytes));
    }
    if body.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(ReadError::ByteOrderMark);
    }
    let text = std::str::from_utf8(body).map_err(|_| ReadError::NotUtf8)?;
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(ReadError::Syntax(p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn err<T>(&self) -> Result<T, ReadError> {
        Err(ReadError::Syntax(self.i))
    }

    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.b[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, ReadError> {
        match self.b.get(self.i) {
            Some(b'{') | Some(b'[') if depth >= MAX_DEPTH => Err(ReadError::TooDeep),
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Node::Str),
            Some(b't') if self.eat(b"true") => Ok(Node::Bool(true)),
            Some(b'f') if self.eat(b"false") => Ok(Node::Bool(false)),
            Some(b'n') if self.eat(b"null") => Ok(Node::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err(),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Node, ReadError> {
        self.i += 1;
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        self.ws();
        if self.eat(b"}") {
            return Ok(Node::Obj(out));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return self.err();
            }
            let k = self.string()?;
            if !seen.insert(k.clone()) {
                return Err(ReadError::DuplicateKey(k));
            }
            self.ws();
            if !self.eat(b":") {
                return self.err();
            }
            self.ws();
            let v = self.value(depth)?;
            out.push((k, v));
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"}") {
                return Ok(Node::Obj(out));
            }
            return self.err();
        }
    }

    fn array(&mut self, depth: usize) -> Result<Node, ReadError> {
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.eat(b"]") {
            return Ok(Node::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value(depth)?);
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"]") {
                return Ok(Node::Arr(out));
            }
            return self.err();
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<Node, ReadError> {
        let start = self.i;
        self.eat(b"-");
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return self.err(),
        }
        if self.eat(b".") && self.digits() == 0 {
            return self.err();
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return self.err();
            }
        }
        // The slice is ASCII digits and signs, so it is UTF-8.
        let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
        Ok(Node::Num(text))
    }

    fn hex4(&mut self) -> Result<u32, ReadError> {
        let s = self
            .b
            .get(self.i..self.i + 4)
            .ok_or(ReadError::Syntax(self.i))?;
        let mut v = 0u32;
        for c in s {
            let d = (*c as char).to_digit(16).ok_or(ReadError::Syntax(self.i))?;
            v = v * 16 + d;
        }
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, ReadError> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return self.err();
            };
            match c {
                b'"' => {
                    self.i += 1;
                    // Built from valid UTF-8 input and whole scalar values only.
                    return String::from_utf8(out).map_err(|_| ReadError::Syntax(self.i));
                }
                0x00..=0x1f => return self.err(),
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.b.get(self.i) else {
                        return self.err();
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = match hi {
                                0xd800..=0xdbff => {
                                    if !self.eat(b"\\u") {
                                        return self.err();
                                    }
                                    let lo = self.hex4()?;
                                    if !(0xdc00..=0xdfff).contains(&lo) {
                                        return self.err();
                                    }
                                    0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                                }
                                0xdc00..=0xdfff => return self.err(),
                                v => v,
                            };
                            char::from_u32(code).ok_or(ReadError::Syntax(self.i))?
                        }
                        _ => return self.err(),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
}

impl Node {
    /// The node at RFC 6901 pointer `p` (`""` is the whole document). An array index must be
    /// `0` or digits without a leading zero, inside the array.
    pub fn pointer(&self, p: &str) -> Option<&Node> {
        if p.is_empty() {
            return Some(self);
        }
        if !p.starts_with('/') {
            return None;
        }
        let mut at = self;
        for raw in p.split('/').skip(1) {
            let t = raw.replace("~1", "/").replace("~0", "~");
            at = match at {
                Node::Obj(kv) => kv.iter().find(|(k, _)| *k == t).map(|(_, v)| v)?,
                Node::Arr(items) => {
                    if !onyx_core::feed_json::is_index(&t) {
                        return None;
                    }
                    items.get(t.parse::<usize>().ok()?)?
                }
                _ => return None,
            };
        }
        Some(at)
    }

    /// The object's field `key`.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Node, ReadError> {
        parse(s.as_bytes(), 1 << 20)
    }

    #[test]
    fn a_duplicate_key_anywhere_refuses_the_document() {
        assert!(matches!(
            p(r#"{"a":1,"a":2}"#),
            Err(ReadError::DuplicateKey(_))
        ));
        assert!(matches!(
            p(r#"{"x":[{"b":1,"c":{"d":1,"d":1}}]}"#),
            Err(ReadError::DuplicateKey(_))
        ));
        // The same key after unescaping.
        assert!(matches!(
            p(r#"{"a":1,"\u0061":2}"#),
            Err(ReadError::DuplicateKey(_))
        ));
        assert!(p(r#"{"a":{"a":1}}"#).is_ok());
    }

    #[test]
    fn grammar_edges() {
        for bad in [
            "",
            " ",
            "01",
            "1.",
            ".5",
            "-",
            "+1",
            "1e",
            "1e+",
            "[1,]",
            "{\"a\":1,}",
            "{a:1}",
            "'a'",
            "\"\\x\"",
            "\"\\ud800\"",
            "\"\\udc00x\"",
            "\"a\u{1}\"",
            "nul",
            "true false",
            "[",
            "{\"a\"}",
            "\"\\u12\"",
            "NaN",
            "Infinity",
            "[1 2]",
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
        for good in [
            "0",
            "-0",
            "1.50",
            "1e5",
            "1E-5",
            "\"\\ud83d\\ude00\"",
            "[]",
            "{}",
            " [ 1 , {\"a\" : null} ] ",
            "\"\\/\\b\\f\\n\\r\\t\\\"\\\\\"",
        ] {
            assert!(p(good).is_ok(), "{good:?}");
        }
        assert_eq!(p("1e5"), Ok(Node::Num("1e5".into())));
        assert_eq!(p("\"\\ud83d\\ude00\""), Ok(Node::Str("\u{1f600}".into())));
    }

    #[test]
    fn depth_bom_utf8_and_size() {
        let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(p(&deep(MAX_DEPTH)).is_ok());
        assert_eq!(p(&deep(MAX_DEPTH + 1)), Err(ReadError::TooDeep));
        assert_eq!(parse(b"\xef\xbb\xbf{}", 100), Err(ReadError::ByteOrderMark));
        assert_eq!(parse(b"\"\xff\"", 100), Err(ReadError::NotUtf8));
        assert_eq!(parse(b"[1,2]", 4), Err(ReadError::TooLarge(4)));
        assert!(parse(b"[1,2]", 5).is_ok());
    }

    #[test]
    fn pointers() {
        let d = p(r#"{"a":[{"b/c":1,"d~e":2}],"":3,"01":4}"#).expect("doc");
        assert_eq!(d.pointer("/a/0/b~1c"), Some(&Node::Num("1".into())));
        assert_eq!(d.pointer("/a/0/d~0e"), Some(&Node::Num("2".into())));
        assert_eq!(d.pointer("/"), Some(&Node::Num("3".into())));
        assert_eq!(d.pointer("/01"), Some(&Node::Num("4".into())));
        assert_eq!(d.pointer("/a/00"), None);
        assert_eq!(d.pointer("/a/1"), None);
        assert_eq!(d.pointer("/a/-"), None);
        assert_eq!(d.pointer("a"), None);
    }
}
