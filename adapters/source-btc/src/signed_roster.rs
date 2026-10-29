// SPDX-License-Identifier: Apache-2.0
//! Bitcoin signed-message roster verifier.
//!
//! A Rust re-implementation of the `verify_row` rule in the entity indexer's
//! `scripts/verify-signed-addresses.py`, ported into Onyx under Onyx's
//! Apache-2.0 licence. Verdicts and detail strings match the Python one for one,
//! so a roster checked here and there gives the same answer.
//!
//! The rule, for one published `(address, message, signatures, script)` row:
//! 1. Empty signature strings are ignored; none left is `Unverifiable`.
//! 2. Every signature must be strict base64 (alphabet, canonical padding;
//!    non-zero trailing bits are tolerated, as Python's `b64decode(validate=True)`
//!    does) of a 65-byte compact signature with header 27..=42, recovery id
//!    `(h - 27) & 3` and compressed flag `(h - 27) & 4`, over the Bitcoin signed
//!    message hash. Any signature that recovers no key fails the whole row.
//! 3. The script column, when not empty, must be hex (ASCII whitespace between
//!    bytes allowed, as Python's `bytes.fromhex`).
//! 4. Mainnet addresses only. Bech32 v0 with a 32-byte program (P2WSH) needs a
//!    script with `sha256(script) == program`; with a 20-byte program and no
//!    script (P2WPKH) a recovered key, serialized as its flag says, must hash160
//!    to the program. Witness v1 and later are `Unverifiable`. Base58 version 0
//!    with no script (P2PKH) is the same hash160 check; version 5 (P2SH) needs a
//!    script with `hash160(script) == payload`.
//! 5. The script must then bind the signers: a bare multisig
//!    (`OP_m <33|65-byte keys> OP_n OP_CHECKMULTISIG`) needs at least `m`
//!    DISTINCT script keys matched by recovered keys (compressed encoding tried
//!    first, then uncompressed); a single public key must equal a recovered key;
//!    any other script is `Unverifiable`.
//!
//! A check that could run and did not hold is `Failed`. Nothing this module
//! cannot decide is ever reported verified.

use base64::alphabet::STANDARD;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;
use bitcoin::hashes::{hash160, sha256, Hash};
use bitcoin::secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use bitcoin::secp256k1::{Message, PublicKey, Secp256k1, VerifyOnly};

/// Outcome of checking one published (address, message, signatures, script) row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The script binds to the address and enough distinct signers are in it
    /// (or, with no script, a recovered key hashes to the address).
    Verified(String),
    /// A check that could be run did not hold.
    Failed(String),
    /// This verifier cannot decide, and says so rather than guessing.
    Unverifiable(String),
}

impl Verdict {
    /// True only for [`Verdict::Verified`].
    pub fn is_verified(&self) -> bool {
        matches!(self, Verdict::Verified(_))
    }
}

/// Python's `base64.b64decode(s, validate=True)`: strict alphabet and padding,
/// non-zero trailing bits accepted.
const B64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
);

const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const BECH32_CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const BECH32_CONST: u32 = 1;
const BECH32M_CONST: u32 = 0x2BC8_30A3;

/// verify_row semantics, identical to the Python: sigs are base64 strings, empty strings ignored; script_hex may be "" (none).
pub fn verify_row(address: &str, message: &str, sigs: &[&str], script_hex: &str) -> Verdict {
    let sigs: Vec<&str> = sigs.iter().copied().filter(|s| !s.is_empty()).collect();
    if sigs.is_empty() {
        return Verdict::Unverifiable("no signature published".into());
    }

    let secp = Secp256k1::verification_only();
    let mh = bitcoin::sign_message::signed_msg_hash(message).to_byte_array();

    let mut recovered: Vec<(PublicKey, bool)> = Vec::with_capacity(sigs.len());
    for s in sigs {
        let Ok(raw) = B64.decode(s) else {
            return Verdict::Failed("signature is not valid base64".into());
        };
        let Some(got) = recover_pubkey(&secp, &mh, &raw) else {
            return Verdict::Failed("signature does not recover a public key".into());
        };
        recovered.push(got);
    }

    let script = if script_hex.is_empty() {
        None
    } else {
        match py_fromhex(script_hex) {
            Some(b) => Some(b),
            None => return Verdict::Failed("script column is not valid hex".into()),
        }
    };

    // ---- classify the address, and bind the script to it ----
    let (kind, script) = if address.starts_with("bc1") {
        let (witver, prog) = match bech32_decode_address(address) {
            Ok(v) => v,
            Err(e) => return Verdict::Failed(format!("address does not decode: {e}")),
        };
        if witver != 0 {
            return Verdict::Unverifiable(format!("witness v{witver} address, not handled"));
        }
        match prog.len() {
            32 => {
                let Some(script) = script else {
                    return Verdict::Unverifiable("P2WSH with no witness script published".into());
                };
                if sha256::Hash::hash(&script).to_byte_array().as_slice() != prog.as_slice() {
                    return Verdict::Failed("sha256(witness script) != address program".into());
                }
                ("p2wsh", script)
            }
            20 => {
                let Some(script) = script else {
                    return key_hashes_to(&recovered, &prog, "p2wpkh");
                };
                // Onyx deviation from the Python (review P2a-R1-1): a script
                // published for a key-hash address must be the key that hashes
                // to it, or anyone's key would "bind".
                if !script_key_hashes_to(&script, &prog) {
                    return Verdict::Failed(
                        "p2wpkh-script: hash160(published key) != address program".into(),
                    );
                }
                ("p2wpkh-script", script)
            }
            n => return Verdict::Unverifiable(format!("unexpected witness program length {n}")),
        }
    } else if matches!(address.chars().next(), Some('1' | '2' | '3')) {
        let (ver, payload) = match b58check_decode(address) {
            Ok(v) => v,
            Err(e) => return Verdict::Failed(format!("address does not decode: {e}")),
        };
        match ver {
            0 => {
                let Some(script) = script else {
                    return key_hashes_to(&recovered, &payload, "p2pkh");
                };
                if !script_key_hashes_to(&script, &payload) {
                    return Verdict::Failed(
                        "p2pkh-script: hash160(published key) != address hash".into(),
                    );
                }
                ("p2pkh-script", script)
            }
            5 => {
                let Some(script) = script else {
                    return Verdict::Unverifiable("P2SH with no redeem script published".into());
                };
                if hash160::Hash::hash(&script).to_byte_array().as_slice() != payload.as_slice() {
                    return Verdict::Failed("hash160(redeem script) != address hash".into());
                }
                ("p2sh", script)
            }
            v => return Verdict::Unverifiable(format!("unknown base58 version byte {v}")),
        }
    } else {
        return Verdict::Unverifiable("unrecognised address format".into());
    };

    // ---- script binds to address; now bind signatures to the script ----
    let Some((m, n, keys)) = parse_multisig(&script) else {
        // single-key script forms
        if let Some(pk) = parse_pubkey(&script) {
            if recovered.iter().any(|(pt, _)| *pt == pk) {
                return Verdict::Verified(format!("{kind}: recovered key equals published key"));
            }
            return Verdict::Failed(format!("{kind}: recovered key is not the published key"));
        }
        return Verdict::Unverifiable(format!(
            "{kind}: script is not bare multisig or a public key"
        ));
    };

    let mut signers: Vec<Vec<u8>> = Vec::new();
    for (pt, _) in &recovered {
        for enc in [ser_pubkey(pt, true), ser_pubkey(pt, false)] {
            if keys.contains(&enc.as_slice()) {
                if !signers.contains(&enc) {
                    signers.push(enc);
                }
                break;
            }
        }
    }
    let got = signers.len();
    if got >= usize::from(m) {
        return Verdict::Verified(format!(
            "{kind} {m}-of-{n}: {got} distinct signer keys in script"
        ));
    }
    Verdict::Failed(format!(
        "{kind} {m}-of-{n}: only {got} recovered key(s) are in the script"
    ))
}

/// A key-hash address's published "script" must be one public key whose
/// hash160 is the address's 20-byte hash.
fn script_key_hashes_to(script: &[u8], hash: &[u8]) -> bool {
    parse_pubkey(script).is_some() && hash160::Hash::hash(script).to_byte_array().as_slice() == hash
}

/// Does a published redeem/witness script hash to this address? Some(true/false) for P2SH (hash160) and P2WSH (sha256) addresses with a hex script; None when the address is another type or the script is not hex (e.g. an output descriptor).
///
/// An empty `script_hex` (no script published) and an address that does not
/// decode are also `None`.
pub fn script_binds(address: &str, script_hex: &str) -> Option<bool> {
    if script_hex.is_empty() {
        return None;
    }
    let script = py_fromhex(script_hex)?;
    if address.starts_with("bc1") {
        match bech32_decode_address(address) {
            Ok((0, prog)) if prog.len() == 32 => {
                Some(sha256::Hash::hash(&script).to_byte_array().as_slice() == prog.as_slice())
            }
            _ => None,
        }
    } else if matches!(address.chars().next(), Some('1' | '2' | '3')) {
        match b58check_decode(address) {
            Ok((5, payload)) => {
                Some(hash160::Hash::hash(&script).to_byte_array().as_slice() == payload.as_slice())
            }
            _ => None,
        }
    } else {
        None
    }
}

/// The P2PKH / P2WPKH no-script check: a recovered key, serialized as its
/// header flag says, must hash160 to `want`.
fn key_hashes_to(recovered: &[(PublicKey, bool)], want: &[u8], kind: &str) -> Verdict {
    for (pt, comp) in recovered {
        if hash160::Hash::hash(&ser_pubkey(pt, *comp))
            .to_byte_array()
            .as_slice()
            == want
        {
            return Verdict::Verified(format!("{kind}: recovered key hashes to address"));
        }
    }
    Verdict::Failed(format!("{kind}: no recovered key hashes to the address"))
}

fn ser_pubkey(pt: &PublicKey, compressed: bool) -> Vec<u8> {
    if compressed {
        pt.serialize().to_vec()
    } else {
        pt.serialize_uncompressed().to_vec()
    }
}

/// Python `recover_pubkey`: `(key, compressed_flag)`, or `None` when the
/// signature is structurally invalid or no key recovers.
fn recover_pubkey(
    secp: &Secp256k1<VerifyOnly>,
    msg_hash: &[u8; 32],
    sig65: &[u8],
) -> Option<(PublicKey, bool)> {
    if sig65.len() != 65 {
        return None;
    }
    let header = sig65[0];
    if !(27..=42).contains(&header) {
        return None;
    }
    let rec = (header - 27) & 3;
    let compressed = ((header - 27) & 4) != 0;
    let recid = RecoveryId::from_i32(i32::from(rec)).ok()?;
    // libsecp256k1 rejects r or s outside [1, n), an x = r + n at or above p,
    // an x off the curve and a result at infinity: the Python's checks.
    let sig = RecoverableSignature::from_compact(&sig65[1..], recid).ok()?;
    let pk = secp
        .recover_ecdsa(&Message::from_digest(*msg_hash), &sig)
        .ok()?;
    Some((pk, compressed))
}

/// Python `parse_pubkey`: a 33-byte 02/03 key or a 65-byte 04 key on the
/// curve. Hybrid 06/07 keys, which libsecp256k1 would accept, are refused.
fn parse_pubkey(b: &[u8]) -> Option<PublicKey> {
    let shape_ok = (b.len() == 33 && matches!(b[0], 2 | 3)) || (b.len() == 65 && b[0] == 4);
    if !shape_ok {
        return None;
    }
    PublicKey::from_slice(b).ok()
}

/// Python `parse_multisig`: `OP_m <pk>...<pk> OP_n OP_CHECKMULTISIG` with
/// 33- or 65-byte pushes only; `(m, n, keys)` or `None`.
fn parse_multisig(script: &[u8]) -> Option<(u8, u8, Vec<&[u8]>)> {
    if script.len() < 4 || script[script.len() - 1] != 0xAE {
        return None;
    }
    let m_op = script[0];
    let n_op = script[script.len() - 2];
    if !(0x51..=0x60).contains(&m_op) || !(0x51..=0x60).contains(&n_op) {
        return None;
    }
    let m = m_op - 0x50;
    let n = n_op - 0x50;
    let mut keys = Vec::new();
    let mut i = 1;
    let end = script.len() - 2;
    while i < end {
        let ln = usize::from(script[i]);
        if ln != 33 && ln != 65 {
            return None;
        }
        i += 1;
        if i + ln > end {
            return None;
        }
        keys.push(&script[i..i + ln]);
        i += ln;
    }
    if i != end || keys.len() != usize::from(n) || !(1..=n).contains(&m) {
        return None;
    }
    Some((m, n, keys))
}

/// Python `bytes.fromhex`: pairs of hex digits, ASCII whitespace allowed
/// between pairs (not inside one).
fn py_fromhex(s: &str) -> Option<Vec<u8>> {
    fn nib(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i < b.len() {
        if matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
            i += 1;
            continue;
        }
        let hi = nib(b[i])?;
        let lo = nib(*b.get(i + 1)?)?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

/// Python's `%r` of one character, for the base58 error detail.
fn py_repr_char(ch: char) -> String {
    match ch {
        '\'' => "\"'\"".into(),
        '\\' => "'\\\\'".into(),
        '\t' => "'\\t'".into(),
        '\n' => "'\\n'".into(),
        '\r' => "'\\r'".into(),
        c if (c as u32) < 0x20 || (0x7F..0xA0).contains(&(c as u32)) => {
            format!("'\\x{:02x}'", c as u32)
        }
        c => format!("'{c}'"),
    }
}

/// Python `b58check_decode`: `(version, payload)`.
fn b58check_decode(s: &str) -> Result<(u8, Vec<u8>), String> {
    let mut num: Vec<u8> = Vec::new(); // big-endian, minimal
    for ch in s.chars() {
        let Some(idx) = B58.iter().position(|&c| u32::from(c) == ch as u32) else {
            return Err(format!("bad base58 char {}", py_repr_char(ch)));
        };
        let mut carry = idx as u32;
        for byte in num.iter_mut().rev() {
            carry += u32::from(*byte) * 58;
            *byte = (carry & 0xFF) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            num.insert(0, (carry & 0xFF) as u8);
            carry >>= 8;
        }
    }
    let pad = s.chars().take_while(|&c| c == '1').count();
    let mut raw = vec![0u8; pad];
    raw.extend_from_slice(&num);
    if raw.len() < 5 {
        return Err("base58 payload too short".into());
    }
    let (body, chk) = raw.split_at(raw.len() - 4);
    let digest = bitcoin::hashes::sha256d::Hash::hash(body).to_byte_array();
    if &digest[..4] != chk {
        return Err("base58 checksum mismatch".into());
    }
    Ok((body[0], body[1..].to_vec()))
}

fn bech32_polymod(values: &[u32]) -> u32 {
    const GEN: [u32; 5] = [
        0x3B6A_57B2,
        0x2650_8E6D,
        0x1EA1_19FA,
        0x3D42_33DD,
        0x2A14_62B3,
    ];
    let mut chk: u32 = 1;
    for &v in values {
        let top = chk >> 25;
        chk = ((chk & 0x01FF_FFFF) << 5) ^ v;
        for (i, g) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

/// Python `_convertbits(data, 5, 8, pad=False)`.
fn convertbits_5_to_8(data: &[u32]) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut ret = Vec::new();
    for &value in data {
        if value >> 5 != 0 {
            return None;
        }
        acc = ((acc << 5) | value) & 0xFFF; // only the low bits are ever read
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            ret.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    if bits >= 5 || ((acc << (8 - bits)) & 0xFF) != 0 {
        return None;
    }
    Some(ret)
}

/// Python `bech32_decode_address`: `(witver, program)`. The HRP is not
/// checked here (the caller requires the `bc1` prefix), as in the Python.
fn bech32_decode_address(addr: &str) -> Result<(u32, Vec<u8>), &'static str> {
    if addr != addr.to_lowercase() && addr != addr.to_uppercase() {
        return Err("mixed case bech32");
    }
    let a: Vec<char> = addr.to_lowercase().chars().collect();
    let Some(pos) = a.iter().rposition(|&c| c == '1') else {
        return Err("bad bech32 layout");
    };
    if pos < 1 || pos + 7 > a.len() || a.len() > 90 {
        return Err("bad bech32 layout");
    }
    let hrp = &a[..pos];
    let mut data = Vec::with_capacity(a.len() - pos - 1);
    for &c in &a[pos + 1..] {
        let Some(idx) = BECH32_CHARSET
            .iter()
            .position(|&x| u32::from(x) == c as u32)
        else {
            return Err("bad bech32 char");
        };
        data.push(idx as u32);
    }
    let mut values: Vec<u32> = hrp.iter().map(|&c| (c as u32) >> 5).collect();
    values.push(0);
    values.extend(hrp.iter().map(|&c| (c as u32) & 31));
    values.extend_from_slice(&data);
    let witver = data[0];
    let expect = if witver == 0 {
        BECH32_CONST
    } else {
        BECH32M_CONST
    };
    if bech32_polymod(&values) != expect {
        return Err("bech32 checksum mismatch");
    }
    let payload = if data.len() > 7 {
        &data[1..data.len() - 6]
    } else {
        &[][..]
    };
    let Some(prog) = convertbits_5_to_8(payload) else {
        return Err("bad bech32 payload");
    };
    if witver == 0 && prog.len() != 20 && prog.len() != 32 {
        return Err("bad v0 program length");
    }
    Ok((witver, prog))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::SecretKey;

    // ---- Real vectors, copied from the issuers' own public PoR files ----

    // OKX por_csv_2026070700_V3.zip, okx_por_2026070700_V3.csv, height 956934.
    const OKX_MSG: &str = "I am an OKX address";
    const OKX_P2SH: &str = "31qfemWiP2VdksYRPtcX1BDr48AfxXm44L";
    const OKX_P2SH_S1: &str =
        "H7dNM9UDdH4pwFht7MO0wAomEA2sG5hAH1T/yawnDn2KC1VOiPA85EL+xsT+fLxh48xlOrcSbon27ZxCKFAU+m8=";
    const OKX_P2SH_S2: &str =
        "II2+x1ksyGHx5FC31L3WDgVrEYXhZj+oD00V4sX8AZ1LbUd6QbP71+0b+icLqCUily5MtFYhewfIelFm5djhI6M=";
    const OKX_P2SH_SCRIPT: &str = "52210310812d3e5bb4cb0645b17ada34701e905121f170a58537623012b1387fd208f82103393d7b2dbc19c0143aaa08f32d250d270a1283b77ad74cb55e58c434e95d4cd32102a8ab89a8d158a5c9adf7684551e7a195adf87106580fb836f684c173adeb600f53ae";
    const OKX_P2WSH: &str = "bc1q03407v403nv9nl47dtfqsagjts7t7twfzm2wjtmt52ey83nvzmjsgrwvqp";
    const OKX_P2WSH_S1: &str =
        "IFwbSy0RYsT5kAFk842QbDsW9sQG8C4OCsMJ0nz0o5jWePRSfUcBakgt2oC/m0hnTkeqftWWiUNRZ9PFgNwFQTE=";
    const OKX_P2WSH_S2: &str =
        "IG0Qj3C7vNM/Zb4qFQaipJAtT933DiG8HylZ6+FFgzcPCMw8ttUXMr340hjWz975W1WP/8VCtWwh8+jbS6d4RkI=";
    const OKX_P2WSH_SCRIPT: &str = "5221025d8a823cf61658ea52c453334ce7fa2dc917c28336ea2d41a9393bf9ec0e89342103292ea81ff95b7f06afa0a669c5cd2fa179af17fd0c9104c6bc24cdd0038529a12102cc8a8fce2a8e2b4f2bf3a078846cfdd5ac9fec951f240e8fb88a588b94adf91f53ae";
    const OKX_P2PKH: &str = "13jTtHxBPFwZkaCdm6BwJMMJkqvTpBZccw";
    const OKX_P2PKH_S1: &str =
        "IPUTSOQjXrOLy7WXyalVeuIpTlvQENsSjdjNETlvTNo+MB6LDF22NpGR9ragpCV8GnyyxXibUTgtrFjqR4A4acU=";
    const OKX_P2PKH_S2: &str =
        "H5PEB4/dXOxDDpzwbk0k3xjkxAeEPtnoFlzkDMiNPyApXbdkQWbzHiH/k5qn2JOuSfBGEo1s+b72iZayd/VzPe0=";

    // HTX por_20260801.csv detail section, height 960427.
    const HTX_ADDR: &str = "143gLvWYUojXaWZRrxquRKpVNTkhmr415B";
    const HTX_MSG: &str = "huobi";
    const HTX_SIG: &str =
        "H5uVlAKs/aB7sqmIZ0h84aSNv8nWAhgffOP5s9UHyuoeEG1ni+DhVpYKF9T+e4jKXsXDUEHOObevLqYocBslD4c=";

    // CoinEx 2023-12-26 workbook, direct reserve-address signature.
    const CX_ADDR: &str = "111WVAJSmeaJYgVY2nKhArNWkvrRcV3mT";
    const CX_MSG: &str = "I am a CoinEx address";
    const CX_SIG: &str =
        "IJsR4cZysJ/VLYxXaa6ACXgrETwS5haYzOdTIV4lp3MTY84feBG8C1JAkGKhs8lyn77PzI616lOZ+gPdA93aZ4U=";

    // BIP-86 first receive address: a valid witness v1 (taproot) address.
    const TAPROOT: &str = "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr";
    // BitMEX-style output descriptor published in a script column.
    const DESCRIPTOR: &str = "wpkh(xpub.../44/0/1/0/0)#2tapg52e";

    fn verified(d: &str) -> Verdict {
        Verdict::Verified(d.into())
    }
    fn failed(d: &str) -> Verdict {
        Verdict::Failed(d.into())
    }
    fn unverifiable(d: &str) -> Verdict {
        Verdict::Unverifiable(d.into())
    }

    /// Base64 of the signature with byte `i` XOR 1 (the Python self-test's tamper).
    fn flip_sig(sig: &str, i: usize) -> String {
        let mut raw = B64.decode(sig).unwrap();
        raw[i] ^= 0x01;
        B64.encode(raw)
    }

    /// Hex of the script with byte `i` XOR 0xFF.
    fn flip_script(script_hex: &str, i: usize) -> String {
        let mut raw = hex::decode(script_hex).unwrap();
        raw[i] ^= 0xFF;
        hex::encode(raw)
    }

    // Expected verdicts below were produced by the Python verifier itself
    // (`verify_row`, self-test passing) on the same inputs.

    #[test]
    fn signed_roster_okx_p2sh_2_of_3_verifies() {
        let v = verify_row(
            OKX_P2SH,
            OKX_MSG,
            &[OKX_P2SH_S1, OKX_P2SH_S2],
            OKX_P2SH_SCRIPT,
        );
        assert_eq!(v, verified("p2sh 2-of-3: 2 distinct signer keys in script"));
        assert!(v.is_verified());
        let swapped = verify_row(
            OKX_P2SH,
            OKX_MSG,
            &[OKX_P2SH_S2, OKX_P2SH_S1],
            OKX_P2SH_SCRIPT,
        );
        assert_eq!(swapped, v);
    }

    #[test]
    fn signed_roster_okx_p2wsh_2_of_3_verifies() {
        let v = verify_row(
            OKX_P2WSH,
            OKX_MSG,
            &[OKX_P2WSH_S1, OKX_P2WSH_S2],
            OKX_P2WSH_SCRIPT,
        );
        assert_eq!(
            v,
            verified("p2wsh 2-of-3: 2 distinct signer keys in script")
        );
    }

    #[test]
    fn signed_roster_okx_p2pkh_verifies() {
        let v = verify_row(OKX_P2PKH, OKX_MSG, &[OKX_P2PKH_S1, OKX_P2PKH_S2], "");
        assert_eq!(v, verified("p2pkh: recovered key hashes to address"));
        // Only the first signature's key is the address key.
        assert_eq!(
            verify_row(OKX_P2PKH, OKX_MSG, &[OKX_P2PKH_S2, ""], ""),
            failed("p2pkh: no recovered key hashes to the address")
        );
    }

    #[test]
    fn signed_roster_htx_p2pkh_verifies() {
        assert_eq!(
            verify_row(HTX_ADDR, HTX_MSG, &[HTX_SIG], ""),
            verified("p2pkh: recovered key hashes to address")
        );
    }

    #[test]
    fn signed_roster_coinex_direct_verifies() {
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &[CX_SIG], ""),
            verified("p2pkh: recovered key hashes to address")
        );
    }

    #[test]
    fn signed_roster_tampered_message_is_not_verified() {
        let v = verify_row(CX_ADDR, "I am a CoinEx address!", &[CX_SIG], "");
        assert_eq!(v, failed("p2pkh: no recovered key hashes to the address"));
        let v = verify_row(
            OKX_P2SH,
            "I am an OKX address.",
            &[OKX_P2SH_S1, OKX_P2SH_S2],
            OKX_P2SH_SCRIPT,
        );
        assert_eq!(
            v,
            failed("p2sh 2-of-3: only 0 recovered key(s) are in the script")
        );
    }

    #[test]
    fn signed_roster_tampered_signature_fails() {
        let bad = flip_sig(HTX_SIG, 40);
        assert_eq!(
            verify_row(HTX_ADDR, HTX_MSG, &[bad.as_str()], ""),
            failed("p2pkh: no recovered key hashes to the address")
        );
        let bad = flip_sig(OKX_P2SH_S2, 40);
        assert_eq!(
            verify_row(
                OKX_P2SH,
                OKX_MSG,
                &[OKX_P2SH_S1, bad.as_str()],
                OKX_P2SH_SCRIPT
            ),
            failed("p2sh 2-of-3: only 1 recovered key(s) are in the script")
        );
        // A header outside 27..=42 recovers nothing.
        let mut raw = B64.decode(CX_SIG).unwrap();
        raw[0] = 26;
        let bad = B64.encode(raw);
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &[bad.as_str()], ""),
            failed("signature does not recover a public key")
        );
        // Wrong length recovers nothing either.
        let short = B64.encode(&B64.decode(CX_SIG).unwrap()[..64]);
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &[short.as_str()], ""),
            failed("signature does not recover a public key")
        );
    }

    #[test]
    fn signed_roster_wrong_address_fails() {
        assert_eq!(
            verify_row(HTX_ADDR, CX_MSG, &[CX_SIG], ""),
            failed("p2pkh: no recovered key hashes to the address")
        );
    }

    #[test]
    fn signed_roster_multisig_threshold_shortfall_fails() {
        assert_eq!(
            verify_row(OKX_P2SH, OKX_MSG, &[OKX_P2SH_S1, ""], OKX_P2SH_SCRIPT),
            failed("p2sh 2-of-3: only 1 recovered key(s) are in the script")
        );
        // The same real signature twice is one signer.
        assert_eq!(
            verify_row(
                OKX_P2WSH,
                OKX_MSG,
                &[OKX_P2WSH_S1, OKX_P2WSH_S1],
                OKX_P2WSH_SCRIPT
            ),
            failed("p2wsh 2-of-3: only 1 recovered key(s) are in the script")
        );
    }

    #[test]
    fn signed_roster_base64_is_strict_like_python() {
        // HTX publishes "-" for an unsigned row.
        assert_eq!(
            verify_row(HTX_ADDR, HTX_MSG, &["-"], ""),
            failed("signature is not valid base64")
        );
        let unpadded = CX_SIG.trim_end_matches('=');
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &[unpadded], ""),
            failed("signature is not valid base64")
        );
        // Non-zero trailing bits decode to the same bytes, as in Python.
        let trailing = CX_SIG.replace("4U=", "4V=");
        assert_ne!(trailing, CX_SIG);
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &[trailing.as_str()], ""),
            verified("p2pkh: recovered key hashes to address")
        );
        // A bad signature anywhere fails the row, even after a good one.
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &[CX_SIG, "-"], ""),
            failed("signature is not valid base64")
        );
    }

    #[test]
    fn signed_roster_missing_script_or_signature_is_unverifiable() {
        assert_eq!(
            verify_row(OKX_P2SH, OKX_MSG, &[OKX_P2SH_S1, OKX_P2SH_S2], ""),
            unverifiable("P2SH with no redeem script published")
        );
        assert_eq!(
            verify_row(OKX_P2WSH, OKX_MSG, &[OKX_P2WSH_S1, OKX_P2WSH_S2], ""),
            unverifiable("P2WSH with no witness script published")
        );
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &["", ""], ""),
            unverifiable("no signature published")
        );
        assert_eq!(
            verify_row(CX_ADDR, CX_MSG, &[], ""),
            unverifiable("no signature published")
        );
    }

    #[test]
    fn signed_roster_taproot_is_unverifiable() {
        assert_eq!(
            verify_row(TAPROOT, OKX_MSG, &[OKX_P2SH_S1], ""),
            unverifiable("witness v1 address, not handled")
        );
    }

    #[test]
    fn signed_roster_script_not_hashing_to_address_fails() {
        let bad = flip_script(OKX_P2SH_SCRIPT, 5);
        assert_eq!(
            verify_row(OKX_P2SH, OKX_MSG, &[OKX_P2SH_S1, OKX_P2SH_S2], &bad),
            failed("hash160(redeem script) != address hash")
        );
        let bad = flip_script(OKX_P2WSH_SCRIPT, 5);
        assert_eq!(
            verify_row(OKX_P2WSH, OKX_MSG, &[OKX_P2WSH_S1, OKX_P2WSH_S2], &bad),
            failed("sha256(witness script) != address program")
        );
        assert_eq!(
            verify_row(
                OKX_P2SH,
                OKX_MSG,
                &[OKX_P2SH_S1, OKX_P2SH_S2],
                OKX_P2WSH_SCRIPT
            ),
            failed("hash160(redeem script) != address hash")
        );
        assert_eq!(
            verify_row(
                OKX_P2WSH,
                OKX_MSG,
                &[OKX_P2WSH_S1, OKX_P2WSH_S2],
                DESCRIPTOR
            ),
            failed("script column is not valid hex")
        );
    }

    #[test]
    fn signed_roster_address_decoding_matches_python() {
        let upper = OKX_P2WSH.to_uppercase();
        assert_eq!(
            verify_row(
                &upper,
                OKX_MSG,
                &[OKX_P2WSH_S1, OKX_P2WSH_S2],
                OKX_P2WSH_SCRIPT
            ),
            unverifiable("unrecognised address format")
        );
        let bad_char = format!("1{}0", &OKX_P2PKH[1..OKX_P2PKH.len() - 1]);
        assert_eq!(
            verify_row(&bad_char, OKX_MSG, &[OKX_P2PKH_S1], ""),
            failed("address does not decode: bad base58 char '0'")
        );
        let bad_ck = format!("{}x", &OKX_P2PKH[..OKX_P2PKH.len() - 1]);
        assert_eq!(
            verify_row(&bad_ck, OKX_MSG, &[OKX_P2PKH_S1], ""),
            failed("address does not decode: base58 checksum mismatch")
        );
        let bad_bech = format!("{}q", &OKX_P2WSH[..OKX_P2WSH.len() - 1]);
        assert_eq!(
            verify_row(
                &bad_bech,
                OKX_MSG,
                &[OKX_P2WSH_S1, OKX_P2WSH_S2],
                OKX_P2WSH_SCRIPT
            ),
            failed("address does not decode: bech32 checksum mismatch")
        );
        // Python's bytes.fromhex allows whitespace between bytes.
        let spaced: Vec<&str> = (0..OKX_P2SH_SCRIPT.len())
            .step_by(2)
            .map(|i| &OKX_P2SH_SCRIPT[i..i + 2])
            .collect();
        assert_eq!(
            verify_row(
                OKX_P2SH,
                OKX_MSG,
                &[OKX_P2SH_S1, OKX_P2SH_S2],
                &spaced.join(" ")
            ),
            verified("p2sh 2-of-3: 2 distinct signer keys in script")
        );
        // Known-answer checks of the decoders themselves.
        assert_eq!(
            bech32_decode_address("BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4").unwrap(),
            (
                0,
                hex::decode("751e76e8199196d454941c45d1b3a323f1433bd6").unwrap()
            )
        );
        assert_eq!(
            bech32_decode_address("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5"),
            Err("bech32 checksum mismatch")
        );
        assert_eq!(
            b58check_decode("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa")
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            b58check_decode("1A1zP1eP5QGefi2DMPTfTL5SLmv7Divfna"),
            Err("base58 checksum mismatch".into())
        );
    }

    // ---- Synthetic keys, signed in the test ----

    fn key(byte: u8) -> SecretKey {
        SecretKey::from_slice(&[byte; 32]).unwrap()
    }

    /// Bitcoin signed message signature, base64, header flagged as given.
    fn sign(sk: &SecretKey, msg: &str, compressed: bool) -> String {
        let secp = Secp256k1::signing_only();
        let mh = bitcoin::sign_message::signed_msg_hash(msg).to_byte_array();
        let sig = secp.sign_ecdsa_recoverable(&Message::from_digest(mh), sk);
        let (recid, compact) = sig.serialize_compact();
        let rec = u8::try_from(recid.to_i32()).unwrap();
        let mut raw = vec![27 + rec + if compressed { 4 } else { 0 }];
        raw.extend_from_slice(&compact);
        B64.encode(raw)
    }

    fn pubkey(sk: &SecretKey) -> PublicKey {
        PublicKey::from_secret_key(&Secp256k1::signing_only(), sk)
    }

    fn multisig_2_of_3(keys: &[PublicKey; 3]) -> bitcoin::ScriptBuf {
        let mut s = vec![0x52];
        for k in keys {
            s.push(33);
            s.extend_from_slice(&k.serialize());
        }
        s.extend_from_slice(&[0x53, 0xAE]);
        bitcoin::ScriptBuf::from_bytes(s)
    }

    fn p2sh(script: &bitcoin::Script) -> String {
        bitcoin::Address::p2sh(script, bitcoin::Network::Bitcoin)
            .unwrap()
            .to_string()
    }

    fn p2wsh(script: &bitcoin::Script) -> String {
        bitcoin::Address::p2wsh(script, bitcoin::Network::Bitcoin).to_string()
    }

    #[test]
    fn signed_roster_distinct_signer_rule() {
        let (k1, k2, k3) = (key(0x11), key(0x22), key(0x33));
        let script = multisig_2_of_3(&[pubkey(&k1), pubkey(&k2), pubkey(&k3)]);
        let script_hex = hex::encode(script.as_bytes());
        let msg = "I am a synthetic custodian address";
        let s1 = sign(&k1, msg, true);
        let s2 = sign(&k2, msg, true);
        let s1_uncompressed_flag = sign(&k1, msg, false);
        for addr in [p2sh(&script), p2wsh(&script)] {
            let kind = if addr.starts_with("bc1") {
                "p2wsh"
            } else {
                "p2sh"
            };
            assert_eq!(
                verify_row(&addr, msg, &[&s1, &s2], &script_hex),
                Verdict::Verified(format!("{kind} 2-of-3: 2 distinct signer keys in script"))
            );
            // The same key signing twice counts once, whatever its header flag.
            assert_eq!(
                verify_row(&addr, msg, &[&s1, &s1], &script_hex),
                Verdict::Failed(format!(
                    "{kind} 2-of-3: only 1 recovered key(s) are in the script"
                ))
            );
            assert_eq!(
                verify_row(&addr, msg, &[&s1, &s1_uncompressed_flag], &script_hex),
                Verdict::Failed(format!(
                    "{kind} 2-of-3: only 1 recovered key(s) are in the script"
                ))
            );
            // An outsider does not reach the threshold.
            let outsider = sign(&key(0x44), msg, true);
            assert_eq!(
                verify_row(&addr, msg, &[&s1, &outsider], &script_hex),
                Verdict::Failed(format!(
                    "{kind} 2-of-3: only 1 recovered key(s) are in the script"
                ))
            );
            assert!(!verify_row(&addr, msg, &[&s1, &s2], "").is_verified());
        }
    }

    #[test]
    fn signed_roster_uncompressed_script_keys_match() {
        let (k1, k2) = (key(0x55), key(0x66));
        let mut s = vec![0x52];
        for k in [&k1, &k2] {
            s.push(65);
            s.extend_from_slice(&pubkey(k).serialize_uncompressed());
        }
        s.extend_from_slice(&[0x52, 0xAE]);
        let script = bitcoin::ScriptBuf::from_bytes(s);
        let msg = "uncompressed multisig";
        let v = verify_row(
            &p2sh(&script),
            msg,
            &[&sign(&k1, msg, true), &sign(&k2, msg, false)],
            &hex::encode(script.as_bytes()),
        );
        assert_eq!(v, verified("p2sh 2-of-2: 2 distinct signer keys in script"));
    }

    #[test]
    fn signed_roster_single_key_and_other_scripts() {
        let k = key(0x77);
        let pk = pubkey(&k);
        let msg = "single key";
        let sig = sign(&k, msg, true);
        let script = bitcoin::ScriptBuf::from_bytes(pk.serialize().to_vec());
        let hexs = hex::encode(script.as_bytes());
        assert_eq!(
            verify_row(&p2sh(&script), msg, &[&sig], &hexs),
            verified("p2sh: recovered key equals published key")
        );
        let other = sign(&key(0x78), msg, true);
        assert_eq!(
            verify_row(&p2sh(&script), msg, &[&other], &hexs),
            failed("p2sh: recovered key is not the published key")
        );
        // OP_TRUE is neither multisig nor a key.
        let op_true = bitcoin::ScriptBuf::from_bytes(vec![0x51]);
        assert_eq!(
            verify_row(&p2wsh(&op_true), msg, &[&sig], "51"),
            unverifiable("p2wsh: script is not bare multisig or a public key")
        );
    }

    #[test]
    fn signed_roster_p2wpkh_and_p2pkh_use_the_header_flag() {
        let k = key(0x99);
        let pk = pubkey(&k);
        let msg = "flag test";
        let compressed = bitcoin::CompressedPublicKey(pk);
        let wpkh = bitcoin::Address::p2wpkh(&compressed, bitcoin::Network::Bitcoin).to_string();
        assert_eq!(
            verify_row(&wpkh, msg, &[&sign(&k, msg, true)], ""),
            verified("p2wpkh: recovered key hashes to address")
        );
        assert_eq!(
            verify_row(&wpkh, msg, &[&sign(&k, msg, false)], ""),
            failed("p2wpkh: no recovered key hashes to the address")
        );
        let legacy = bitcoin::PublicKey::new_uncompressed(pk);
        let pkh = bitcoin::Address::p2pkh(legacy, bitcoin::Network::Bitcoin).to_string();
        assert_eq!(
            verify_row(&pkh, msg, &[&sign(&k, msg, false)], ""),
            verified("p2pkh: recovered key hashes to address")
        );
        assert_eq!(
            verify_row(&pkh, msg, &[&sign(&k, msg, true)], ""),
            failed("p2pkh: no recovered key hashes to the address")
        );
    }

    #[test]
    fn signed_roster_a_key_hash_address_with_someone_elses_key_fails() {
        // Review P2a-R1-1: a 1... or bc1q... address listed with the attacker's
        // own public key as its "script" and the attacker's signature must
        // not verify (the Python rule accepted it).
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let sk = bitcoin::secp256k1::SecretKey::from_slice(&[7u8; 32]).expect("key");
        let pk = bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize();
        let msg = "I am an OKX address";
        let hash = bitcoin::sign_message::signed_msg_hash(msg);
        let m = bitcoin::secp256k1::Message::from_digest(hash.to_byte_array());
        let (rec, sig) = secp.sign_ecdsa_recoverable(&m, &sk).serialize_compact();
        let mut raw = vec![31 + rec.to_i32() as u8];
        raw.extend_from_slice(&sig);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let victim_p2pkh = "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";
        let victim_p2wpkh = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        for a in [victim_p2pkh, victim_p2wpkh] {
            let v = verify_row(a, msg, &[b64.as_str()], &hex::encode(pk));
            assert!(matches!(v, Verdict::Failed(_)), "{a}: {v:?}");
        }
    }

    #[test]
    fn signed_roster_script_binds() {
        assert_eq!(script_binds(OKX_P2SH, OKX_P2SH_SCRIPT), Some(true));
        assert_eq!(script_binds(OKX_P2WSH, OKX_P2WSH_SCRIPT), Some(true));
        assert_eq!(
            script_binds(OKX_P2SH, &flip_script(OKX_P2SH_SCRIPT, 5)),
            Some(false)
        );
        assert_eq!(
            script_binds(OKX_P2WSH, &flip_script(OKX_P2WSH_SCRIPT, 5)),
            Some(false)
        );
        assert_eq!(script_binds(OKX_P2SH, OKX_P2WSH_SCRIPT), Some(false));
        assert_eq!(script_binds(OKX_P2WSH, DESCRIPTOR), None);
        assert_eq!(script_binds(OKX_P2PKH, OKX_P2SH_SCRIPT), None);
        assert_eq!(script_binds(TAPROOT, OKX_P2SH_SCRIPT), None);
        assert_eq!(script_binds(OKX_P2SH, ""), None);
    }
}
