//! Known-answer checks over `fixtures/tlock` (ROADMAP C50), shared by the
//! native test `tests/interop.rs` and the wasm32 run `examples/wasm_kat.rs`
//! (`scripts/check-tlock-wasm.sh`), so both targets run the same code on the
//! same bytes. The fixtures are compiled in (`include_str!`); every parse of
//! them is ours and returns an error, never a panic.

#![allow(dead_code)]

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine as _;
use onyx_tlock::{BeaconKey, Opening, CIPHERTEXT_LEN, MSG_LEN};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sha3::Keccak256;

/// The fixture files, by their path under `fixtures/tlock`.
pub fn file(name: &str) -> Option<&'static str> {
    Some(match name {
        "vectors.json" => include_str!("../../../../fixtures/tlock/vectors.json"),
        "onyx-zero-tail.json" => include_str!("../../../../fixtures/tlock/onyx-zero-tail.json"),
        "onyx-far-date.json" => include_str!("../../../../fixtures/tlock/onyx-far-date.json"),
        "drand/quicknet-info.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-info.json")
        }
        "drand/quicknet-t-info.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-t-info.json")
        }
        "drand/quicknet-round-1.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-round-1.json")
        }
        "drand/quicknet-round-2.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-round-2.json")
        }
        "drand/quicknet-round-1000.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-round-1000.json")
        }
        "drand/quicknet-round-746511.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-round-746511.json")
        }
        "drand/quicknet-round-33154000.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-round-33154000.json")
        }
        "drand/quicknet-t-round-5423142.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-t-round-5423142.json")
        }
        "drand/quicknet-t-round-1624910.json" => {
            include_str!("../../../../fixtures/tlock/drand/quicknet-t-round-1624910.json")
        }
        "go-tlock/lorem-tle-testnet-quicknet-t-2024-01-17-15-28.tle" => include_str!(
            "../../../../fixtures/tlock/go-tlock/lorem-tle-testnet-quicknet-t-2024-01-17-15-28.tle"
        ),
        "go-tlock/tlock_test-interop-quicknet-t-1624910.age" => include_str!(
            "../../../../fixtures/tlock/go-tlock/tlock_test-interop-quicknet-t-1624910.age"
        ),
        "tlock-js/rfc-scheme-quicknet-746511.age" => {
            include_str!("../../../../fixtures/tlock/tlock-js/rfc-scheme-quicknet-746511.age")
        }
        _ => return None,
    })
}

fn json(name: &str) -> Result<Value, String> {
    let text = file(name).ok_or(format!("no fixture {name}"))?;
    serde_json::from_str(text).map_err(|e| format!("{name}: {e}"))
}

fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or(format!("missing string {k}"))
}

fn u(v: &Value, k: &str) -> Result<u64, String> {
    v.get(k)
        .and_then(Value::as_u64)
        .ok_or(format!("missing number {k}"))
}

fn hex_of(v: &Value, k: &str) -> Result<Vec<u8>, String> {
    hex::decode(s(v, k)?).map_err(|e| format!("{k}: {e}"))
}

fn arr16(b: &[u8]) -> Result<[u8; MSG_LEN], String> {
    b.try_into()
        .map_err(|_| format!("want 16 bytes, got {}", b.len()))
}

/// A drand beacon: the chain's key from its info file, a round and its
/// published signature.
pub struct Beacon {
    pub chain: String,
    pub chain_hash: String,
    pub key: BeaconKey,
    pub round: u64,
    pub signature: Vec<u8>,
}

/// The chain's key and hash from its pinned info file; the scheme must be
/// quicknet's.
pub fn chain(name: &str) -> Result<(BeaconKey, String), String> {
    let m = json("vectors.json")?;
    let c = m
        .get("chains")
        .and_then(|c| c.get(name))
        .ok_or(format!("no chain {name}"))?;
    let info = json(s(c, "info")?)?;
    if s(&info, "schemeID")? != "bls-unchained-g1-rfc9380" {
        return Err(format!("{name}: not the quicknet scheme"));
    }
    let key = BeaconKey::from_compressed(&hex_of(&info, "public_key")?)
        .map_err(|e| format!("{name} key: {e}"))?;
    Ok((key, s(&info, "hash")?.to_string()))
}

/// A published round from its pinned file.
pub fn beacon(chain_name: &str, file_name: &str) -> Result<Beacon, String> {
    let (key, chain_hash) = chain(chain_name)?;
    let b = json(file_name)?;
    Ok(Beacon {
        chain: chain_name.to_string(),
        chain_hash,
        key,
        round: u(&b, "round")?,
        signature: hex_of(&b, "signature")?,
    })
}

/// Every published round in the manifest.
pub fn rounds() -> Result<Vec<Beacon>, String> {
    let m = json("vectors.json")?;
    let list = m
        .get("rounds")
        .and_then(Value::as_array)
        .ok_or("no rounds")?;
    list.iter()
        .map(|r| beacon(s(r, "chain")?, s(r, "file")?))
        .collect()
}

/// An age file made by drand/tlock: its one tlock stanza and its header MAC.
pub struct AgeFile {
    pub round: u64,
    pub chain_hash: String,
    pub ciphertext: Vec<u8>,
    /// The header up to and including `---` (what the MAC covers).
    pub mac_input: Vec<u8>,
    pub mac: Vec<u8>,
}

/// Parse an armored age file with one `tlock` stanza (age v1 format,
/// <https://age-encryption.org/v1>).
pub fn parse_age(armored: &str) -> Result<AgeFile, String> {
    let lines: Vec<&str> = armored.trim().lines().map(str::trim).collect();
    if lines.first() != Some(&"-----BEGIN AGE ENCRYPTED FILE-----")
        || lines.last() != Some(&"-----END AGE ENCRYPTED FILE-----")
    {
        return Err("not armored age".into());
    }
    let raw = STANDARD
        .decode(lines[1..lines.len() - 1].concat())
        .map_err(|e| format!("armor: {e}"))?;
    let end = raw
        .windows(4)
        .position(|w| w == b"\n---")
        .ok_or("no header end")?;
    let mac_input = raw[..end + 4].to_vec();
    let mac_line_end = raw[end + 1..]
        .iter()
        .position(|&b| b == b'\n')
        .ok_or("no mac line end")?
        + end
        + 1;
    let mac_text = std::str::from_utf8(&raw[end + 5..mac_line_end]).map_err(|e| e.to_string())?;
    let mac = STANDARD_NO_PAD
        .decode(mac_text)
        .map_err(|e| format!("mac: {e}"))?;
    let header = std::str::from_utf8(&raw[..end]).map_err(|e| e.to_string())?;
    let mut h = header.split('\n');
    if h.next() != Some("age-encryption.org/v1") {
        return Err("not age v1".into());
    }
    let stanza: Vec<&str> = h.next().ok_or("no stanza")?.split(' ').collect();
    if stanza.len() != 4 || stanza[0] != "->" || stanza[1] != "tlock" {
        return Err(format!("not one tlock stanza: {stanza:?}"));
    }
    let round = stanza[2].parse::<u64>().map_err(|e| e.to_string())?;
    let body: String = h.collect();
    let ciphertext = STANDARD_NO_PAD
        .decode(body)
        .map_err(|e| format!("stanza body: {e}"))?;
    Ok(AgeFile {
        round,
        chain_hash: stanza[3].to_string(),
        ciphertext,
        mac_input,
        mac,
    })
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::digest([ipad.as_slice(), msg].concat());
    Sha256::digest([opad.as_slice(), inner.as_slice()].concat()).into()
}

/// age's header MAC key: HKDF-SHA-256(ikm = file key, salt = empty,
/// info = "header"), 32 bytes (RFC 5869: an empty salt is 32 zero bytes).
fn age_header_key(file_key: &[u8]) -> [u8; 32] {
    let prk = hmac_sha256(&[0u8; 32], file_key);
    hmac_sha256(&prk, b"header\x01")
}

/// The age header MAC under `file_key`: proves `file_key` is the key the
/// file was made with, so the IBE opening is the real one end to end.
pub fn age_mac_checks(f: &AgeFile, file_key: &[u8]) -> bool {
    hmac_sha256(&age_header_key(file_key), &f.mac_input).as_slice() == f.mac.as_slice()
}

/// One interop vector checked: the beacon signature verifies under the
/// chain's key, the stanza opens, the opening is age's file key (header
/// MAC), and re-encryption gives the stanza's bytes.
pub fn check_interop(v: &Value) -> Result<Opening, String> {
    let name = s(v, "name")?;
    let f = parse_age(file(s(v, "file")?).ok_or("no file")?)?;
    let b = beacon(s(v, "chain")?, s(v, "beacon")?)?;
    if f.round != b.round || f.chain_hash != b.chain_hash {
        return Err(format!("{name}: stanza round/chain differ from the beacon"));
    }
    if f.ciphertext.len() != CIPHERTEXT_LEN {
        return Err(format!("{name}: stanza is {} bytes", f.ciphertext.len()));
    }
    if !b.key.verify(b.round, &b.signature) {
        return Err(format!("{name}: beacon signature does not verify"));
    }
    let o = b
        .key
        .open(b.round, &b.signature, &f.ciphertext)
        .map_err(|e| format!("{name}: open: {e}"))?;
    if !age_mac_checks(&f, &o.msg) {
        return Err(format!(
            "{name}: the opening is not the file key (header MAC)"
        ));
    }
    let re = b
        .key
        .lock(b.round, &o.msg, &o.sigma)
        .map_err(|e| format!("{name}: lock: {e}"))?;
    if re.as_slice() != f.ciphertext.as_slice() {
        return Err(format!("{name}: re-encryption differs"));
    }
    Ok(o)
}

/// One Onyx zero-tail vector: made by this crate (not by Go) for a
/// published quicknet round; it must open under the real signature to its
/// recorded `(sigma, msg)` and re-encrypt to its recorded bytes on every
/// target.
pub fn check_zero_tail(v: &Value) -> Result<(), String> {
    let b = beacon("quicknet", s(v, "beacon")?)?;
    if u(v, "round")? != b.round {
        return Err("round differs from its beacon".into());
    }
    let msg = arr16(&hex_of(v, "msg")?)?;
    let sigma = arr16(&hex_of(v, "sigma")?)?;
    let ct = hex_of(v, "ciphertext")?;
    let o = b
        .key
        .open(b.round, &b.signature, &ct)
        .map_err(|e| format!("round {}: open: {e}", b.round))?;
    if o.msg != msg || o.sigma != sigma {
        return Err(format!("round {}: opening differs", b.round));
    }
    let re = b
        .key
        .lock(b.round, &msg, &sigma)
        .map_err(|e| e.to_string())?;
    if re.as_slice() != ct.as_slice() {
        return Err(format!("round {}: re-encryption differs", b.round));
    }
    Ok(())
}

/// One Onyx far-date vector (ROADMAP C52, ADR-0027 amendment 15): a body key
/// `kappa` locked to a published quicknet round with `sigma =
/// keccak256("onyx-sealed-far-sigma-v1" ‖ kappa ‖ round as 8 bytes BE)[..16]`
/// (made by onyx-coordinator, not by Go). On every target: `sigma` derives
/// from `kappa`, the lock re-creates the recorded bytes, and the round's real
/// signature opens them to `(sigma, kappa)`.
pub fn check_far_date(v: &Value) -> Result<(), String> {
    let b = beacon("quicknet", s(v, "beacon")?)?;
    if u(v, "round")? != b.round {
        return Err("round differs from its beacon".into());
    }
    let kappa = arr16(&hex_of(v, "kappa")?)?;
    let sigma = arr16(&hex_of(v, "sigma")?)?;
    let ct = hex_of(v, "ciphertext")?;
    let mut pre = b"onyx-sealed-far-sigma-v1".to_vec();
    pre.extend_from_slice(&kappa);
    pre.extend_from_slice(&b.round.to_be_bytes());
    if Keccak256::digest(&pre)[..16] != sigma {
        return Err(format!("round {}: sigma is not far_sigma(kappa)", b.round));
    }
    let re = b
        .key
        .lock(b.round, &kappa, &sigma)
        .map_err(|e| e.to_string())?;
    if re.as_slice() != ct.as_slice() {
        return Err(format!("round {}: far copy differs", b.round));
    }
    let o = b
        .key
        .open(b.round, &b.signature, &ct)
        .map_err(|e| format!("round {}: open: {e}", b.round))?;
    if o.msg != kappa || o.sigma != sigma {
        return Err(format!("round {}: far opening differs", b.round));
    }
    Ok(())
}

/// What a full run checked.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub rounds: usize,
    pub interop: usize,
    pub zero_tail: usize,
    pub far_date: usize,
}

/// Every check: each published round verifies under its chain's key (and a
/// flipped bit does not), each interop vector, each zero-tail vector, each
/// far-date vector (ROADMAP C52).
pub fn run_all() -> Result<Report, String> {
    let rs = rounds()?;
    for b in &rs {
        if !b.key.verify(b.round, &b.signature) {
            return Err(format!("{} round {} does not verify", b.chain, b.round));
        }
        let mut bad = b.signature.clone();
        bad[47] ^= 1;
        if b.key.verify(b.round, &bad) || b.key.verify(b.round + 1, &b.signature) {
            return Err(format!(
                "{} round {}: a wrong signature verified",
                b.chain, b.round
            ));
        }
    }
    let m = json("vectors.json")?;
    let inter = m
        .get("interop")
        .and_then(Value::as_array)
        .ok_or("no interop")?;
    for v in inter {
        check_interop(v)?;
    }
    let zt = json(s(&m, "zero_tail")?)?;
    let zt = zt
        .get("vectors")
        .and_then(Value::as_array)
        .ok_or("no zero-tail vectors")?;
    for v in zt {
        check_zero_tail(v)?;
    }
    let fd = json(s(&m, "far_date")?)?;
    let fd = fd
        .get("vectors")
        .and_then(Value::as_array)
        .ok_or("no far-date vectors")?;
    for v in fd {
        check_far_date(v)?;
    }
    Ok(Report {
        rounds: rs.len(),
        interop: inter.len(),
        zero_tail: zt.len(),
        far_date: fd.len(),
    })
}

/// The counts the fixtures hold today; both targets must reach them.
pub const EXPECTED: Report = Report {
    rounds: 7,
    interop: 3,
    zero_tail: 8,
    far_date: 5,
};
