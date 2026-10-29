//! Reviews POOL-R1 and POOL-R2 (ADR-0032): the cases the reviewers proved,
//! asserting the fixed behaviour, plus a guard that no tracked or untracked
//! (non-ignored) file in the repo carries a person's coinbase handle
//! (POOL-R1-F1/F2, hardened in POOL-R2-F2).
//!
//! This file never spells the coinbase handle phrase as one literal, so the
//! scanner can scan it too; it builds the phrase with `concat!`.

use onyx_pool_schema::TxOut;
use onyx_source_btc::pools::{pool_table, CoinbaseProofs, EXCLUDED_POOL_IDS};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The handle phrase, lower case, never written as one literal here.
const PHRASE: &[u8] = concat!("mi", "ned by").as_bytes();
/// The same phrase as pools write it, for the planted examples below.
const PHRASE_CAP: &str = concat!("Mi", "ned by");
/// The invented handle the tests use.
const FAKE_HANDLE: &str = "example-user";
/// Pool-authored coinbase text that is not itself a list tag, reviewed by
/// hand (the text a pool writes after its tag; no handles).
const POOL_TEMPLATE_TEXT: &[&str] = &["#dropgold"];

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn out(script: Vec<u8>, value: u64) -> TxOut {
    TxOut { script, value }
}

/// POOL-R1-F3: a coinbase that pays ~40 parties (a pool paying its miners
/// directly), one of them a listed pool's address, is not credited to that
/// pool: only the largest output is the payout, so the tag decides.
#[test]
fn a_many_payout_coinbase_is_not_credited_to_a_listed_address_among_them() {
    let (t, _) = pool_table(&root().join("fixtures")).expect("table");
    let listed = t
        .pools
        .iter()
        .find(|p| p.name == "BitFuFu")
        .expect("BitFuFu")
        .scripts[0]
        .clone();
    let mut outs: Vec<TxOut> = (0..39u8)
        .map(|i| out(vec![0x00, 0x14, i], 2_000_000))
        .collect();
    outs.insert(0, out(vec![0x00, 0x14, 0xee], 50_000_000));
    outs.push(out(listed.clone(), 1_000_000));
    let i = btc_pool_blocks::attribute(&t, b"\x03\x2f\xcf\x0e/no-listed-tag/", &outs);
    assert_eq!(i, None, "a small output never credits its pool");
    // The same address as the largest output is a payout match.
    let i = btc_pool_blocks::attribute(&t, b"", &[out(listed, 9), out(vec![1], 1)]);
    assert_eq!(t.pools[i.expect("matched")].name, "BitFuFu");
}

/// POOL-R2-F3: an Ocean-style block (tag OCEAN.XYZ first, many direct miner
/// payouts) whose miner-chosen text contains another listed pool's tag stays
/// with Ocean: the tag that occurs earliest in the scriptSig wins, and a
/// pool writes its own tag ahead of any text it lets a miner add.
#[test]
fn an_ocean_block_whose_miner_text_names_another_pool_stays_with_ocean() {
    let (t, _) = pool_table(&root().join("fixtures")).expect("table");
    let outs: Vec<TxOut> = (0..30u8)
        .map(|i| out(vec![0x00, 0x14, 0xa0, i], 10_000_000 + u64::from(i)))
        .collect();
    let sig = b"\x03\x2f\xcf\x0eOCEAN.XYZ\x10/ViaBTC/ my rig".to_vec();
    let i = btc_pool_blocks::attribute(&t, &sig, &outs).expect("matched");
    assert_eq!(t.pools[i].name, "Ocean.xyz");
    let sig = b"\x03\x2f\xcf\x0eOCEAN.XYZ".to_vec();
    let i = btc_pool_blocks::attribute(&t, &sig, &outs).expect("matched");
    assert_eq!(t.pools[i].name, "Ocean.xyz");
    // The other order credits the other pool: first tag wins.
    let sig = b"\x03\x2f\xcf\x0e/ViaBTC/OCEAN.XYZ".to_vec();
    let i = btc_pool_blocks::attribute(&t, &sig, &outs).expect("matched");
    assert_eq!(t.pools[i].name, "ViaBTC");
}

/// POOL-R1-F4, POOL-R2-F6: every named pool has a company website (no bare
/// IP address, forum post, explorer page or operator-handle domain), and no
/// pool run under an operator's personal handle is named.
#[test]
fn every_named_pool_has_a_company_site() {
    let (_, info) = pool_table(&root().join("fixtures")).expect("table");
    for p in &info {
        let host = p.link.split('/').nth(2).unwrap_or("");
        assert!(
            !host.is_empty()
                && !host
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == ':')
                && !host.contains("bitcointalk.org")
                && !host.contains("mempool.space")
                && !host.contains("github.com"),
            "{} ({}) has no company site",
            p.name,
            p.link
        );
        assert!(
            !EXCLUDED_POOL_IDS.iter().any(|(id, _)| *id == p.id),
            "excluded id {} is named",
            p.id
        );
    }
    assert!(EXCLUDED_POOL_IDS.iter().any(|(id, _)| *id == 115));
}

/// What the scanner may let through: the reviewed list's tags and the text
/// those tags carry after the handle phrase (company names), the invented
/// handle, and placeholders that name nobody.
struct Allowed {
    /// Every non-empty tag of the reviewed pool list, longest first.
    tags: Vec<Vec<u8>>,
    /// Lower-case tokens allowed right after the handle phrase.
    tokens: BTreeSet<String>,
}

fn allowed() -> Allowed {
    let list: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("fixtures/pools/pool-allowlist.json")).expect("allowlist"),
    )
    .expect("json");
    let mut tags = Vec::new();
    let mut tokens: BTreeSet<String> = [FAKE_HANDLE, "...", "<user>", "<handle>", "<x>", "<name>"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for p in list.as_array().expect("array") {
        for t in p["tags"].as_array().expect("tags") {
            let t = t.as_str().expect("tag");
            if t.is_empty() {
                continue;
            }
            tokens.extend(phrase_tokens(t.as_bytes()));
            tags.push(t.as_bytes().to_vec());
        }
    }
    tags.sort_by_key(|t| std::cmp::Reverse(t.len()));
    tags.dedup();
    Allowed { tags, tokens }
}

/// The token after every handle phrase in `bytes` (case-insensitive, at a
/// word start; spaces after the phrase skipped). Anything that is not a
/// space counts: the token runs to whitespace, a quote, a slash, a backtick
/// or the end. An empty token (the phrase alone, e.g. in a regex) names
/// nobody.
fn phrase_tokens(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let n = PHRASE.len();
    let mut i = 0;
    while i + n <= bytes.len() {
        if bytes[i..i + n].eq_ignore_ascii_case(PHRASE)
            && (i == 0 || !bytes[i - 1].is_ascii_alphabetic())
        {
            let mut j = i + n;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let end = bytes[j..]
                .iter()
                .position(|b| b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'/' | b'`'))
                .map_or(bytes.len(), |k| j + k);
            let token = String::from_utf8_lossy(&bytes[j..end]).to_lowercase();
            if !token.is_empty() {
                found.push(token);
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    found
}

/// Every maximal run of hex digits (at least 16), decoded (an odd run drops
/// its last digit).
fn hex_runs(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_hexdigit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            let len = (i - start) & !1;
            if len >= 16 {
                if let Ok(d) = hex::decode(&bytes[start..start + len]) {
                    runs.push(d);
                }
            }
        } else {
            i += 1;
        }
    }
    runs
}

/// A committed scriptSig may hold only reviewed pool tags, reviewed
/// pool-authored text and short non-word runs (random extranonce bytes):
/// after blanking those, no printable run of 6+ bytes with 3+ letters stays.
fn script_sig_problems(sig: &[u8], ok: &Allowed) -> Vec<String> {
    let mut s = sig.to_vec();
    let blank = |s: &mut Vec<u8>, needle: &[u8]| {
        let mut k = 0;
        while k + needle.len() <= s.len() {
            if &s[k..k + needle.len()] == needle {
                s[k..k + needle.len()].fill(0);
                k += needle.len();
            } else {
                k += 1;
            }
        }
    };
    for t in &ok.tags {
        blank(&mut s, t);
    }
    for t in POOL_TEMPLATE_TEXT {
        blank(&mut s, t.as_bytes());
    }
    s.split(|b| !(0x20..0x7f).contains(b))
        .filter(|r| r.len() >= 6 && r.iter().filter(|b| b.is_ascii_alphabetic()).count() >= 3)
        .map(|r| format!("scriptSig text {:?}", String::from_utf8_lossy(r)))
        .collect()
}

/// Problems in one coinbase field of a JSON file: a full transaction (its
/// scriptSig, and at most two paying outputs) or a bare scriptSig.
fn coinbase_problems(value: &str, ok: &Allowed) -> Vec<String> {
    let Ok(bytes) = hex::decode(value) else {
        return Vec::new();
    };
    match bitcoin::consensus::deserialize::<bitcoin::Transaction>(&bytes) {
        Ok(tx) if !tx.input.is_empty() => {
            let mut p = script_sig_problems(tx.input[0].script_sig.as_bytes(), ok);
            let paying = tx.output.iter().filter(|o| o.value.to_sat() > 0).count();
            if paying > 2 {
                p.push(format!("coinbase pays {paying} outputs"));
            }
            p
        }
        _ => script_sig_problems(&bytes, ok),
    }
}

/// A coinbase-tag-like value (`/…/`) under a tag key must be built from
/// reviewed tags only.
fn tag_value_ok(v: &str, ok: &Allowed) -> bool {
    if !(v.len() >= 3 && v.starts_with('/') && v.ends_with('/')) {
        return true; // not a coinbase tag (an XBRL concept, a hash, ...)
    }
    let mut s = v.as_bytes().to_vec();
    for t in &ok.tags {
        let mut k = 0;
        while k + t.len() <= s.len() {
            if s[k..k + t.len()] == t[..] {
                s.drain(k..k + t.len());
            } else {
                k += 1;
            }
        }
    }
    s.iter().all(|b| matches!(b, b'/' | b'$' | b' '))
}

fn json_problems(v: &serde_json::Value, ok: &Allowed, check_tags: bool, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                let key = k.to_ascii_lowercase();
                let strings: Vec<&str> = match val {
                    serde_json::Value::String(s) => vec![s.as_str()],
                    serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect(),
                    _ => Vec::new(),
                };
                if key.contains("coinbase") || key.contains("script_sig") || key == "scriptsig" {
                    for s in &strings {
                        out.extend(coinbase_problems(s, ok));
                    }
                }
                if check_tags && key.contains("tag") {
                    for s in &strings {
                        if !tag_value_ok(s, ok) {
                            out.push(format!("unreviewed tag {s:?} under {k:?}"));
                        }
                    }
                }
                json_problems(val, ok, check_tags, out);
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                json_problems(x, ok, check_tags, out);
            }
        }
        _ => {}
    }
}

fn gunzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut c = Command::new("gzip")
        .arg("-dc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = c.stdin.take()?;
    let input = bytes.to_vec();
    let w = std::thread::spawn(move || stdin.write_all(&input));
    let o = c.wait_with_output().ok()?;
    let _ = w.join();
    o.status.success().then_some(o.stdout)
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut c = Command::new("gzip")
        .args(["-nc"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("gzip");
    let mut stdin = c.stdin.take().expect("stdin");
    let input = bytes.to_vec();
    let w = std::thread::spawn(move || stdin.write_all(&input));
    let o = c.wait_with_output().expect("gzip");
    w.join().expect("join").expect("write");
    o.stdout
}

/// Every problem in one file: a handle phrase followed by an unreviewed
/// token, in the raw bytes, in any decoded hex run, in a decoded `.b64`
/// file, also inside gzip;
/// in JSON, unreviewed coinbase text, coinbases paying more than two
/// outputs, and unreviewed `/tag/` values under tag keys.
fn scan(name: &str, raw: &[u8], ok: &Allowed) -> Vec<String> {
    let bytes = if name.ends_with(".gz") {
        match gunzip(raw) {
            Some(b) => b,
            None => return vec!["unreadable gzip".into()],
        }
    } else {
        raw.to_vec()
    };
    let mut bad = Vec::new();
    let mut chunks = vec![bytes.clone()];
    chunks.extend(hex_runs(&bytes));
    let base = name.trim_end_matches(".gz");
    if base.ends_with(".b64") || base.ends_with(".b64.txt") {
        use base64::Engine;
        let text: Vec<u8> = bytes
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        match base64::engine::general_purpose::STANDARD.decode(&text) {
            Ok(d) => chunks.push(d),
            Err(_) => bad.push("unreadable base64".into()),
        }
    }
    for c in &chunks {
        for t in phrase_tokens(c) {
            if !ok.tokens.contains(&t) {
                bad.push(format!("handle phrase + {t:?}"));
            }
        }
    }
    if base.ends_with(".json") {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            // The reviewed list defines the tags; everything else is checked.
            let check_tags = !base.ends_with("pools/pool-allowlist.json");
            json_problems(&v, ok, check_tags, &mut bad);
        }
    }
    bad
}

fn repo_files() -> Vec<String> {
    let files = Command::new("git")
        .arg("-C")
        .arg(root())
        .args(["ls-files", "-co", "--exclude-standard"])
        .output()
        .expect("git ls-files");
    assert!(files.status.success());
    String::from_utf8_lossy(&files.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// POOL-R1-F1/F2, POOL-R2-F2: no tracked or untracked (non-ignored) file
/// anywhere in the repo, of any type, carries a person's coinbase handle,
/// unreviewed coinbase text, or a committed coinbase paying more than two
/// outputs.
#[test]
fn no_committed_file_carries_a_persons_coinbase_handle() {
    let ok = allowed();
    let mut checked = 0;
    let mut coinbase_files = 0;
    for f in repo_files() {
        let path: &Path = &root().join(&f);
        let Ok(raw) = std::fs::read(path) else {
            continue; // a deleted but still listed file
        };
        checked += 1;
        if f.starts_with("fixtures/btc/coinbase-") {
            coinbase_files += 1;
            let proofs: CoinbaseProofs =
                serde_json::from_slice(&raw).expect("coinbase proofs parse");
            assert!(!proofs.blocks.is_empty(), "{f}: no blocks");
        }
        let bad = scan(&f, &raw, &ok);
        assert!(bad.is_empty(), "{f}: {bad:?}");
    }
    assert!(checked > 300, "only {checked} files checked");
    assert!(
        coinbase_files >= 1,
        "the committed coinbase proofs were not scanned"
    );
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let p = e.expect("entry").path();
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// AUD-2-F7: the same scan over the built website and the data it copies in
/// (gitignored, so the repo scan above never sees them). `scripts/build-web.sh`
/// runs this with `ONYX_SCAN_DIRS=apps/web/build:apps/web/static/data`;
/// without it the test checks nothing.
#[test]
fn the_built_site_carries_no_persons_handle() {
    let Ok(dirs) = std::env::var("ONYX_SCAN_DIRS") else {
        eprintln!("ONYX_SCAN_DIRS unset: no built site to scan");
        return;
    };
    let ok = allowed();
    let mut checked = 0;
    for d in dirs.split(':').filter(|d| !d.is_empty()) {
        let mut all = Vec::new();
        files_under(&root().join(d), &mut all);
        for f in all {
            let raw = std::fs::read(&f).expect("read");
            checked += 1;
            let name = f.to_string_lossy();
            let bad = scan(&name, &raw, &ok);
            assert!(bad.is_empty(), "{name}: {bad:?}");
        }
    }
    assert!(checked > 0, "no files under {dirs}");
}

/// POOL-R2-F2: every form the reviewer planted is caught (they all slipped
/// past the first scanner), and the plain control is caught too. The
/// handles are invented.
#[test]
fn the_scanner_catches_every_planted_form() {
    let ok = allowed();
    let m = PHRASE_CAP;
    let hex_sig = hex::encode(format!("/{m} zzsneak2/"));
    let plants: Vec<(&str, Vec<u8>)> = vec![
        // 1. case variants
        (
            "fixtures/r2-sneak/case.json",
            format!(
                "{{\"a\":\"{} zzsneak1\",\"b\":\"{} zzsneak1b\",\"c\":\"{} zzsneak1c\"}}",
                m.replacen("by", "By", 1),
                m.to_lowercase(),
                m.to_uppercase()
            )
            .into_bytes(),
        ),
        // 2. hex-encoded scriptSig in a JSON that is not a coinbase proof file
        (
            "fixtures/r2-sneak/blocks.json",
            format!("{{\"blocks\":[{{\"script_sig\":\"03aabbcc{hex_sig}\"}}]}}").into_bytes(),
        ),
        // 3. Rust byte literal with an escape right after the phrase
        (
            "fixtures/r2-sneak/lit.rs",
            format!("const X: &[u8] = b\"{m} \\x7azsneak3\";").into_bytes(),
        ),
        // 4. gzip'd export
        (
            "fixtures/r2-sneak/export.json.gz",
            gzip(format!("{{\"cb\":\"{m} zzsneak4\"}}").as_bytes()),
        ),
        // 5-6. plain text outside the old scanner's folders
        (
            "apps/web/verify/r2-sneak/note.txt",
            format!("{m} zzsneak5\n").into_bytes(),
        ),
        (
            "contracts/r2-sneak/x.sol",
            format!("// {m} zzsneak6\n").into_bytes(),
        ),
        // 7. a bare handle tag without the phrase
        (
            "fixtures/r2-sneak/bare.json",
            b"{\"tag\":\"/zzsneak7-solo/\"}".to_vec(),
        ),
        // 8. the plain control
        (
            "fixtures/r2-sneak/control.txt",
            format!("{m} zzsneakCTRL\n").into_bytes(),
        ),
        // 9. a handle with no space after the phrase
        (
            "apps/demo/x.txt",
            format!("{m}zzsneak9").replace("by", "by\t").into_bytes(),
        ),
    ];
    for (name, bytes) in &plants {
        let bad = scan(name, bytes, &ok);
        assert!(!bad.is_empty(), "plant {name} slipped past");
    }
    // A pool's own tags, the invented handle, the phrase alone and prose
    // placeholders pass; so does an XBRL concept under a "tag" key.
    for (name, bytes) in [
        ("a.txt", format!("x /{m} AntPool/ y").into_bytes()),
        ("b.rs", format!("b\"{m} {FAKE_HANDLE}\"").into_bytes()),
        ("c.mjs", format!("/{m}/.test(t)").into_bytes()),
        (
            "d.md",
            format!("a solo miner's \"{m} ...\" string").into_bytes(),
        ),
        ("e.json", b"{\"tag\":\"CryptoAssetNumberOfUnits\"}".to_vec()),
        ("f.json", b"{\"tag\":\"/ViaBTC/\"}".to_vec()),
        ("g.md", b"the order is determined by the template".to_vec()),
    ] {
        assert_eq!(scan(name, &bytes, &ok), Vec::<String>::new(), "{name}");
    }
}
