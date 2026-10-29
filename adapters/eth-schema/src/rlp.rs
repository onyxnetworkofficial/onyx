//! Recursive Length Prefix (Ethereum Yellow Paper, appendix B): a strict
//! decoder and the encoder the trie and the adapter use.
//!
//! The decoder refuses every non-canonical form (a single byte below 0x80
//! wrapped in a string header, a long form for a payload of 55 bytes or
//! less, a length with leading zeros, a payload that runs past its buffer),
//! so one item has exactly one encoding. Integers are big endian without
//! leading zeros; zero is the empty string.

use alloc::vec::Vec;

/// Why an RLP buffer was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RlpError {
    /// The buffer ends inside an item.
    Truncated,
    /// A valid item uses a non-canonical length form.
    NonCanonical,
    /// Bytes are left after the item.
    Trailing,
    /// A string where a list was wanted, or the reverse.
    WrongKind,
    /// An integer with a leading zero byte or wider than its target type.
    BadInt,
    /// A list with another number of items than the format has.
    WrongCount,
}

/// One decoded item: a byte string, or a list's payload (its items' encodings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item<'a> {
    /// A byte string.
    Bytes(&'a [u8]),
    /// A list; the slice is its payload (the concatenated item encodings).
    List(&'a [u8]),
}

fn be_len(b: &[u8]) -> Result<usize, RlpError> {
    if b.first() == Some(&0) {
        return Err(RlpError::NonCanonical);
    }
    if b.len() > core::mem::size_of::<usize>() {
        return Err(RlpError::Truncated);
    }
    Ok(b.iter().fold(0usize, |a, &x| (a << 8) | usize::from(x)))
}

/// Decode the first item of `b`; returns it and the bytes it took.
pub fn decode(b: &[u8]) -> Result<(Item<'_>, usize), RlpError> {
    let &first = b.first().ok_or(RlpError::Truncated)?;
    let (is_list, head, len) = match first {
        0x00..=0x7f => return Ok((Item::Bytes(&b[..1]), 1)),
        0x80..=0xb7 => (false, 1usize, usize::from(first - 0x80)),
        0xb8..=0xbf => {
            let n = usize::from(first - 0xb7);
            let len = be_len(b.get(1..1 + n).ok_or(RlpError::Truncated)?)?;
            if len <= 55 {
                return Err(RlpError::NonCanonical);
            }
            (false, 1 + n, len)
        }
        0xc0..=0xf7 => (true, 1, usize::from(first - 0xc0)),
        0xf8..=0xff => {
            let n = usize::from(first - 0xf7);
            let len = be_len(b.get(1..1 + n).ok_or(RlpError::Truncated)?)?;
            if len <= 55 {
                return Err(RlpError::NonCanonical);
            }
            (true, 1 + n, len)
        }
    };
    let end = head.checked_add(len).ok_or(RlpError::Truncated)?;
    let payload = b.get(head..end).ok_or(RlpError::Truncated)?;
    if !is_list && len == 1 && payload[0] < 0x80 {
        return Err(RlpError::NonCanonical);
    }
    Ok((
        if is_list {
            Item::List(payload)
        } else {
            Item::Bytes(payload)
        },
        end,
    ))
}

/// Decode `b` as exactly one item.
pub fn decode_exact(b: &[u8]) -> Result<Item<'_>, RlpError> {
    let (item, used) = decode(b)?;
    if used != b.len() {
        return Err(RlpError::Trailing);
    }
    Ok(item)
}

/// The items of a list payload, in order.
pub fn items(payload: &[u8]) -> Result<Vec<Item<'_>>, RlpError> {
    let mut out = Vec::new();
    let mut rest = payload;
    while !rest.is_empty() {
        let (item, used) = decode(rest)?;
        out.push(item);
        rest = &rest[used..];
    }
    Ok(out)
}

/// `b` as exactly one list; its items.
pub fn list(b: &[u8]) -> Result<Vec<Item<'_>>, RlpError> {
    match decode_exact(b)? {
        Item::List(p) => items(p),
        Item::Bytes(_) => Err(RlpError::WrongKind),
    }
}

impl<'a> Item<'a> {
    /// The byte string, or `WrongKind`.
    pub fn bytes(self) -> Result<&'a [u8], RlpError> {
        match self {
            Item::Bytes(b) => Ok(b),
            Item::List(_) => Err(RlpError::WrongKind),
        }
    }

    /// The byte string, which must be exactly `N` bytes long.
    pub fn fixed<const N: usize>(self) -> Result<[u8; N], RlpError> {
        let b = self.bytes()?;
        let mut out = [0u8; N];
        if b.len() != N {
            return Err(RlpError::WrongKind);
        }
        out.copy_from_slice(b);
        Ok(out)
    }

    /// The list's items, or `WrongKind`.
    pub fn list(self) -> Result<Vec<Item<'a>>, RlpError> {
        match self {
            Item::List(p) => items(p),
            Item::Bytes(_) => Err(RlpError::WrongKind),
        }
    }

    /// A canonical unsigned integer of at most 16 bytes.
    pub fn u128(self) -> Result<u128, RlpError> {
        let b = self.bytes()?;
        if b.len() > 16 || b.first() == Some(&0) {
            return Err(RlpError::BadInt);
        }
        Ok(b.iter().fold(0u128, |a, &x| (a << 8) | u128::from(x)))
    }

    /// A canonical unsigned integer of at most 8 bytes.
    pub fn u64(self) -> Result<u64, RlpError> {
        let b = self.bytes()?;
        if b.len() > 8 || b.first() == Some(&0) {
            return Err(RlpError::BadInt);
        }
        Ok(b.iter().fold(0u64, |a, &x| (a << 8) | u64::from(x)))
    }
}

fn header(out: &mut Vec<u8>, short: u8, len: usize) {
    if len <= 55 {
        // len <= 55 fits the byte.
        out.push(short + len as u8);
    } else {
        let be = len.to_be_bytes();
        let skip = be.iter().take_while(|&&x| x == 0).count();
        // At most 8 length bytes, so the sum stays below 0xc0 / 0x100.
        out.push(short + 55 + (be.len() - skip) as u8);
        out.extend_from_slice(&be[skip..]);
    }
}

/// Append the encoding of a byte string.
pub fn encode_bytes(out: &mut Vec<u8>, b: &[u8]) {
    if b.len() == 1 && b[0] < 0x80 {
        out.push(b[0]);
    } else {
        header(out, 0x80, b.len());
        out.extend_from_slice(b);
    }
}

/// Append the encoding of a list whose items are already encoded in `payload`.
pub fn encode_list(out: &mut Vec<u8>, payload: &[u8]) {
    header(out, 0xc0, payload.len());
    out.extend_from_slice(payload);
}

/// Append the encoding of an unsigned integer (big endian, no leading zeros).
pub fn encode_uint(out: &mut Vec<u8>, v: u128) {
    let be = v.to_be_bytes();
    let skip = be.iter().take_while(|&&x| x == 0).count();
    encode_bytes(out, &be[skip..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn enc_b(b: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        encode_bytes(&mut o, b);
        o
    }

    #[test]
    fn yellow_paper_examples_round_trip() {
        // "dog", ["cat", "dog"], "", [], 0, 15, 1024 (ethereum.org RLP page).
        assert_eq!(enc_b(b"dog"), vec![0x83, b'd', b'o', b'g']);
        let mut p = enc_b(b"cat");
        p.extend(enc_b(b"dog"));
        let mut l = Vec::new();
        encode_list(&mut l, &p);
        assert_eq!(
            l,
            vec![0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g']
        );
        assert_eq!(enc_b(b""), vec![0x80]);
        let mut e = Vec::new();
        encode_list(&mut e, &[]);
        assert_eq!(e, vec![0xc0]);
        let u = |v| {
            let mut o = Vec::new();
            encode_uint(&mut o, v);
            o
        };
        assert_eq!(u(0), vec![0x80]);
        assert_eq!(u(15), vec![0x0f]);
        assert_eq!(u(1024), vec![0x82, 0x04, 0x00]);
        let long = [0x61u8; 56];
        let enc = enc_b(&long);
        assert_eq!(&enc[..2], &[0xb8, 56]);
        assert_eq!(decode_exact(&enc), Ok(Item::Bytes(&long[..])));
        let items_back = list(&l).expect("list");
        assert_eq!(items_back, vec![Item::Bytes(b"cat"), Item::Bytes(b"dog")]);
        assert_eq!(decode_exact(&u(1024)).and_then(Item::u64), Ok(1024));
    }

    #[test]
    fn non_canonical_forms_are_refused() {
        // A byte below 0x80 wrapped in a string header.
        assert_eq!(decode_exact(&[0x81, 0x05]), Err(RlpError::NonCanonical));
        // Long form for 3 bytes.
        assert_eq!(
            decode_exact(&[0xb8, 3, 1, 2, 3]),
            Err(RlpError::NonCanonical)
        );
        // Long-form length with a leading zero.
        let mut b = vec![0xb9, 0, 56];
        b.extend([0u8; 56]);
        assert_eq!(decode_exact(&b), Err(RlpError::NonCanonical));
        // Truncated payload, trailing bytes, a list where a string is wanted.
        assert_eq!(decode_exact(&[0x83, 1, 2]), Err(RlpError::Truncated));
        assert_eq!(decode_exact(&[0x01, 0x02]), Err(RlpError::Trailing));
        assert_eq!(
            decode_exact(&[0xc0]).and_then(Item::bytes),
            Err(RlpError::WrongKind)
        );
        // Integers: leading zero, too wide.
        assert_eq!(
            decode_exact(&[0x82, 0, 1]).and_then(Item::u64),
            Err(RlpError::BadInt)
        );
        let mut wide = vec![0x89, 1];
        wide.extend([0u8; 8]);
        assert_eq!(
            decode_exact(&wide).and_then(Item::u64),
            Err(RlpError::BadInt)
        );
        assert_eq!(decode_exact(&wide).and_then(Item::u128), Ok(1u128 << 64));
        // An empty buffer.
        assert_eq!(decode(&[]), Err(RlpError::Truncated));
    }
}
