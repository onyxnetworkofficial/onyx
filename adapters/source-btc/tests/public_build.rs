//! AUD-2-F1, AUD-2R-F1: a public website build (`ONYX_PUBLIC=1
//! scripts/build-web.sh`) ships no label rows. The test labels' sharing rights
//! are not cleared (fixtures/labels/PROVENANCE.md), so no address of any
//! committed label set may appear in a public build in any form.
//!
//! The guard fails closed:
//! 1. Every shipped file must match an exact allowlist of paths ([`allowed`]);
//!    anything else (a `.gz`, a `.br`, a new data file) is refused.
//! 2. Every file is scanned as found and in every encoding it could carry:
//!    base64 runs (standard or URL alphabet, any padding, any alignment), hex
//!    runs, `%XX` and `\xXX`/`\uXXXX` escapes, `data:` URIs (their payload is
//!    one of those), and gzip, zlib or Brotli streams, nested up to
//!    [`MAX_DEPTH`] layers.
//!
//! 3. L8-F1 (data audit 2026-10-13): every checksum-valid Bitcoin address in
//!    any of those forms must be on [`known_addresses`] (test-key proof
//!    vectors, the demo attestation's test key, the reviewed pool list).
//!    The label scan above only knows committed label sets; this rule also
//!    refuses addresses from lists never committed (an exchange's full PoR
//!    file, the excluded wBTC/cbBTC lists, a person's address).
//!
//! 4. L9A-8 (threat sweep 2026-10-14): no e-mail address (other than a
//!    reserved example domain or a role mailbox on [`ROLE_CONTACTS`]) and no
//!    social-media profile link, in any of those forms. These were the manual
//!    `scripts/audit-privacy-scan.py` patterns; here a hit fails the build.
//!
//! `scripts/build-web.sh` runs `a_public_build_carries_no_label_rows` over the
//! build with `ONYX_PUBLIC_BUILD_DIRS`; without it that test checks nothing.

use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// Every address of every committed label set, without its chain prefix.
fn label_addresses() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in std::fs::read_dir(root().join("fixtures/labels")).expect("labels dir") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "json") {
            let v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&p).expect("read")).expect("label set");
            // Undated sets list `labels`; the dated v3 lists `members` (ADR-0037).
            let rows = v["labels"].as_array().or_else(|| v["members"].as_array());
            for l in rows.expect("labels or members") {
                let a = l["address"].as_str().expect("address");
                out.insert(a.split_once(':').map_or(a, |(_, x)| x).to_owned());
            }
        }
    }
    assert!(out.len() > 9_000, "only {} label addresses", out.len());
    out
}

/// A token that parses as a Bitcoin address with a valid checksum (any
/// network).
fn is_btc_address(t: &str) -> bool {
    use std::str::FromStr;
    (26..=90).contains(&t.len())
        && (t.starts_with(['1', '3', 'm', 'n', '2'])
            || t.to_ascii_lowercase().starts_with("bc1")
            || t.to_ascii_lowercase().starts_with("tb1")
            || t.to_ascii_lowercase().starts_with("bcrt1"))
        && bitcoin::Address::from_str(t).is_ok()
}

/// The only Bitcoin addresses a public build may carry (L8-F1): the
/// registry's proof vectors and the BIP-322 test vectors (test keys), the
/// demo attestation's address (test key `keccak256("onyx test btc key 1")`,
/// examples/export-demo.rs), the reviewed pool list (pool companies,
/// MIT; fixtures/pools/PROVENANCE.md), and the address-spelling vectors
/// of `BtcAddressCanon` (common documentation examples and the genesis
/// coinbase's; pinned below, so a new vector needs a review; compiled into the verifier, stored back to back, so only the
/// W11 packed scan sees them; any case, as the vectors test both).
fn known_addresses() -> BTreeSet<String> {
    use bitcoin::secp256k1::{Secp256k1, SecretKey};
    use onyx_core::AddressCanon;
    let mut out: BTreeSet<String> = onyx_source_btc::BtcAddressCanon
        .vectors()
        .iter()
        .map(|(a, _)| (*a).to_owned())
        .filter(|a| is_btc_address(a))
        .collect();
    // Pinned (W10 review): a new canon vector must be reviewed before it may ship.
    assert_eq!(
        out.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "112D2adLM3UKy4Z4giRbReR6gjWuvHUqB",
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
            "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",
            "BC1QAR0SRRR7XFKVY5L643LYDNW9RE59GTZZWF5MDQ",
            "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
            "mipcBbFg9gMiCh81Kj8tqqdgoZub1ZJRfn",
        ],
        "BtcAddressCanon vectors changed: review them before they may ship in the public build"
    );
    for f in [
        "adapters/source-btc/src/proof_vectors.rs",
        "fixtures/bip322/basic-test-vectors.json",
        "fixtures/bip322/generated-test-vectors.json",
    ] {
        let raw = std::fs::read(root().join(f)).expect("vector file");
        out.extend(
            tokens(&raw)
                .filter(|t| is_btc_address(t))
                .map(str::to_owned),
        );
    }
    let sk = SecretKey::from_slice(&onyx_core::keccak256(b"onyx test btc key 1").0).expect("key");
    let pk = bitcoin::CompressedPublicKey(sk.public_key(&Secp256k1::new()));
    out.insert(bitcoin::Address::p2wpkh(&pk, bitcoin::Network::Bitcoin).to_string());
    let pools: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("fixtures/pools/pool-allowlist.json")).expect("pool list"),
    )
    .expect("pool list json");
    for p in pools.as_array().expect("pools") {
        for a in p["addresses"].as_array().expect("addresses") {
            out.insert(a.as_str().expect("address").to_owned());
        }
    }
    assert!(out.len() > 150, "only {} known addresses", out.len());
    out
}

/// Data files the site ships, by name (under `data/` in the build, or
/// directly in `static/data`).
const DATA_FILES: &[&str] = &[
    "demo.json",
    "recipe.wasm",
    "revenue-vectors.json",
    "pricing-defaults.json",
    "contributor-policy.json",
    // PP11: each dataset card's newest date (scripts/gen-freshness.py; dates only, no value).
    "freshness.json",
    // ADR-0044: the `eth-chain-stats` recipe (recipes/out), rerun in the browser.
    "eth-chain-stats.wasm",
    // The MIT notice of the pool list the pools section names (L8-F2).
    "LICENSE-mining-pools.txt",
    // ADR-0043: the `btc-chain-stats` recipe (recipes/out), rerun in the browser.
    "chain-stats.wasm",
    // ROADMAP D30 (ADR-0037 amendment 3): the synthetic salted rows the Labels page checks
    // (fixtures/dated/salted-vectors.json: example entities and `test:` addresses only).
    "salted-vectors.json",
];

/// Other exact paths the site ships.
const EXACT: &[&str] = &[
    "index.html",
    // ADR-0045: the API description (docs/api/openapi.json): routes and schemas, no data.
    "openapi.json",
    "og.jpg",
    // The Onyx mark as the browser and phone icon (drawn, no data).
    "favicon.svg",
    "favicon.ico",
    "apple-touch-icon.png",
    // PT28-6: crawler rules (two fixed lines, no data).
    "robots.txt",
    // Meeting polish MP2: whether the Add data page sends to the hosted node (one boolean, no data;
    // apps/web/src/lib/submit-api.svelte.ts).
    "intake.json",
    // CL5 (docs/ops/bulk-publish.md): the keys document of the synthetic demo publisher the signed
    // book registered for this domain (one test address, no data).
    ".well-known/onyx-source-keys.json",
    "app/version.json",
    "history/pools-week.json",
    // Reserve history (ADR-0034): entity totals and reserve rows only; the
    // per-address scan stays in fixtures/reserves/ and never ships.
    "history/reserves-week.json",
    // Label set v3 (ROADMAP P2a, examples/export-rosters.rs): per-entity flows
    // and balances, sources and grades; no address.
    "history/rosters-week.json",
    "history/week/index.json",
    // ADR-0043: the on-chain window (one `btc-chain-stats` leaf per block, counts and
    // amounts of public chain data, no label) and one block's recipe input for the rerun
    // (scripts, values and sizes; OP_RETURN payloads, coinbase text and witnesses are never
    // carried, CHAIN-4).
    "onchain/btc-chain-stats.json",
    "onchain/rerun-input.b64.txt",
    "level2/debt-to-penny.json",
    "level2/sec-holdings.json",
    // NS3: the daily sources' recorded rounds (U.S. government data, no address or label
    // total); sealed in a public build like every level-2 round (ATT-R2-F1).
    "level2/usgs-quakes.json",
    "level2/ncei-tmax.json",
    // NS5 (ADR-0048): the level history of the two Base Sepolia daily values, sealed in a
    // public build (no value key, no result hash: THREATS NS5-4; e2e `ns5` checks it).
    "level2/ladders-base-sepolia.json",
    // NS11 (ADR-0042 amendment 2): the live markets' contract addresses on Base Sepolia and each
    // market's title, record date and scale; no value, no pool (the browser reads them from the
    // chain), no address of a person.
    "markets/base-sepolia.json",
    // ROADMAP P2c: spot bitcoin ETF holdings as funds filed them with the SEC (public U.S.
    // government data, no address, no label-set total); the verifier recomputes it.
    "level2/etf-holdings.json",
    // ROADMAP C6 (review TR-S-R2-4): the level-5 example, invented ids only (`example.*`,
    // `entity:example-*`), no address; written by onyx-api's blessed C6 test.
    "level5/track-example.json",
    // ROADMAP C20: the same example as the server answers `?since=` each example commitment,
    // and its logs (`GET /v1/track/log`); example ids, invented level-3 values, no address.
    "level5/track-example-since.json",
    "level5/track-example-log.json",
    // ROADMAP C23: the example's registry as the server serves it (`GET /v1/track/registry`):
    // example ids, invented sources' CC0 rights, no address, no value.
    "level5/track-example-registry.json",
    // ROADMAP C61: one level-3 answer recorded in the local-anvil rehearsal (chain 31337, test
    // keys, the invented source `example.signed`, subject `entity:example`) with the chain
    // answers recorded with it; anvil dev addresses and the book's bytecode, no Bitcoin address.
    // Free and derived from no label set, so every build ships it (ADR-0028 amendment 8).
    "level3/signed-rehearsal.json",
    "run/live-run.json",
    // ADR-0044: the Ethereum window (one `eth-chain-stats` leaf per block: counts and wei
    // amounts of public chain data, no label) and one block's full recipe input for the
    // browser rerun (its transactions and receipts as the chain hashes them, ETH-6).
    "onchain/eth-chain-stats.json",
    "onchain/eth-rerun-input.b64.txt",
    // Full Base Sepolia runs (scripts/build-testnet-run.py): tx hashes, contract and actor
    // addresses, the driver's step lines; the script refuses any other 64-hex string.
    "run/base-sepolia-2026-10-12.json",
    "run/base-sepolia-2026-10-13.json",
    "run/base-sepolia-2026-10-14.json",
    // W4: the home page's copy of a few fields of the runs above (verdict, network, date,
    // transaction count, reserve round), written by scripts/build-web.sh; no address.
    "run/heads.json",
    // The chain indexer's read-only Base Sepolia read (ADR-0033): chain hashes only.
    "run/indexer-base-sepolia-2026-10-12.json",
    // OSS-12: the bundled Lucide icons' ISC notice and the Feather MIT notice, verbatim.
    "licenses/LICENSE-lucide.txt",
];

/// A bundler file name: `[A-Za-z0-9._-]+` ending in `ext`, no `..`.
fn bundled(name: &str, ext: &str) -> bool {
    name.len() > ext.len()
        && name.ends_with(ext)
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The exact allowlist of a public build (AUD-2R-F1), by path relative to the
/// scanned directory. Compressed copies (`.gz`, `.br`) and any file not listed
/// are refused: a new kind of file needs a reviewed entry here.
fn allowed(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.as_slice() {
        [name] if DATA_FILES.contains(name) => true,
        ["data", name] if DATA_FILES.contains(name) => true,
        ["app", "immutable", "chunks" | "nodes" | "entry", n] => bundled(n, ".js"),
        ["app", "immutable", "assets", n] => bundled(n, ".css"),
        ["app", "immutable", "workers", n] => bundled(n, ".js"),
        ["app", "immutable", "workers", "assets", n] => bundled(n, ".wasm"),
        ["fonts", n] => bundled(n, ".woff2") || (n.starts_with("OFL-") && bundled(n, ".txt")),
        ["history", "week", n] if *n != "index.json" => {
            let b = n.as_bytes();
            n.len() == 15
                && n.ends_with(".json")
                && b[..10].iter().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        *c == b'-'
                    } else {
                        c.is_ascii_digit()
                    }
                })
        }
        _ => EXACT.contains(&rel),
    }
}

/// Runs of an address alphabet (base58 and bech32 both fit) at least 25 long.
fn tokens(bytes: &[u8]) -> impl Iterator<Item = &str> {
    bytes
        .split(|b| !b.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 25)
        .filter_map(|t| std::str::from_utf8(t).ok())
}

/// Every substring of `t` that could be one address packed among others
/// (W11: wasm data and minified strings store addresses back to back, so a
/// whole token is one long run that is no address): from each bech32 hrp
/// (`bc1`, `tb1`, `bcrt1`, either case) to every end inside the bech32 run
/// after it, and from each base58 version character (`1`, `3`, `m`, `n`, `2`)
/// at every address length (26-35) inside the base58 run around it. The whole
/// token is not repeated here.
fn packed(t: &str) -> impl Iterator<Item = &str> {
    const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let b = t.as_bytes();
    let base58 = |c: u8| c.is_ascii_alphanumeric() && !b"0OIl".contains(&c);
    let mut out = Vec::new();
    for i in 0..b.len() {
        let rest = &t[i..];
        let low = rest.get(..5).unwrap_or(rest).to_ascii_lowercase();
        let hrp = ["bcrt1", "bc1", "tb1"]
            .into_iter()
            .find(|h| low.starts_with(h))
            .map(str::len);
        if let Some(h) = hrp {
            let run = b[i + h..]
                .iter()
                .take_while(|c| BECH32.contains(&c.to_ascii_lowercase()))
                .count();
            for len in (h + 8)..=(h + run).min(90) {
                out.push(&t[i..i + len]);
            }
        }
        if b"13mn2".contains(&b[i]) {
            let run = b[i..].iter().take_while(|c| base58(**c)).count();
            for len in 26..=run.min(35) {
                out.push(&t[i..i + len]);
            }
        }
    }
    out.into_iter().filter(move |c| c.len() != t.len())
}

/// Hex runs (64+ digits) decoded to bytes, at both alignments.
fn hex_runs(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for t in bytes
        .split(|b| !b.is_ascii_hexdigit())
        .filter(|t| t.len() >= 64)
    {
        for off in 0..2 {
            let t = &t[off..];
            if let Ok(d) = hex::decode(&t[..t.len() & !1]) {
                out.push(d);
            }
        }
    }
    out
}

/// Shortest base64 run decoded: 40 characters (30 bytes) is longer than any
/// address token and shorter than a label row.
const MIN_B64_RUN: usize = 40;

/// Base64 runs (standard or URL alphabet, padded or not) decoded to bytes at
/// all four alignments, so a run inside a longer token is not missed.
fn b64_runs(bytes: &[u8]) -> Vec<Vec<u8>> {
    let cfg = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    let std = GeneralPurpose::new(&base64::alphabet::STANDARD, cfg);
    let url = GeneralPurpose::new(&base64::alphabet::URL_SAFE, cfg);
    let mut out = Vec::new();
    let b64 = |b: &u8| b.is_ascii_alphanumeric() || b"+/-_".contains(b);
    for run in bytes.split(|b| !b64(b)).filter(|t| t.len() >= MIN_B64_RUN) {
        for off in 0..4 {
            let t = &run[off..];
            let t = &t[..t.len() & !3];
            for e in [&std, &url] {
                if let Ok(d) = e.decode(t) {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// `%XX`, `\xXX` and `\uXXXX` escapes decoded (a `data:` URI or a JS string
/// may carry text that way); `None` when there are none.
fn unescaped(bytes: &[u8]) -> Option<Vec<u8>> {
    let hexval = |s: &[u8]| {
        std::str::from_utf8(s)
            .ok()
            .and_then(|s| u32::from_str_radix(s, 16).ok())
    };
    let mut out = Vec::with_capacity(bytes.len());
    let mut changed = false;
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (v, n) = match rest {
            [b'%', a, b, ..] => (hexval(&[*a, *b]), 3),
            [b'\\', b'x', a, b, ..] => (hexval(&[*a, *b]), 4),
            [b'\\', b'u', a, b, c, d, ..] => (hexval(&[*a, *b, *c, *d]), 6),
            _ => (None, 1),
        };
        match v.and_then(char::from_u32) {
            Some(c) if n > 1 => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                changed = true;
                i += n;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    changed.then_some(out)
}

/// Most bytes one compressed stream may unpack to (a public build is far
/// smaller); a bigger stream is refused, not truncated.
const MAX_UNPACKED: u64 = 256 << 20;

/// A gzip, zlib or Brotli stream found at the start of `bytes`, unpacked.
/// `Err` when a stream starts but does not unpack (fail closed).
fn unpacked(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let read_all = |mut r: Box<dyn Read + '_>, what: &str| -> Result<Option<Vec<u8>>, String> {
        let mut out = Vec::new();
        r.by_ref()
            .take(MAX_UNPACKED + 1)
            .read_to_end(&mut out)
            .map_err(|e| format!("{what} stream does not unpack: {e}"))?;
        if out.len() as u64 > MAX_UNPACKED {
            return Err(format!("{what} stream unpacks past {MAX_UNPACKED} bytes"));
        }
        Ok(Some(out))
    };
    match bytes {
        [0x1f, 0x8b, ..] => read_all(Box::new(flate2::read::GzDecoder::new(bytes)), "gzip"),
        [0x78, b, ..] if [0x01, 0x5e, 0x9c, 0xda].contains(b) => {
            // zlib header; a text file starting "x^" is not a stream, so only
            // a stream that unpacks counts.
            Ok(read_all(Box::new(flate2::read::ZlibDecoder::new(bytes)), "zlib").unwrap_or(None))
        }
        _ => Ok(None),
    }
}

/// A Brotli stream has no magic number: try the whole file.
fn unbrotli(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    brotli_decompressor::Decompressor::new(bytes, 4096)
        .take(MAX_UNPACKED)
        .read_to_end(&mut out)
        .ok()
        .filter(|_| !out.is_empty())
        .map(|_| out)
}

/// Nesting depth of decodings followed (e.g. a gzip inside base64 inside a
/// `data:` URI is three).
const MAX_DEPTH: usize = 4;

/// Role mailboxes a public build may name (L9A-8): a team address, never a
/// person's own (docs/ops/label-removal.md "Contact"). Empty until the project owner picks
/// the removal contact (THREATS L8-4); add it here, lower case, when he does.
const ROLE_CONTACTS: &[&str] = &[];

/// Social-media and code hosts whose path names a person's profile (the
/// patterns of `scripts/audit-privacy-scan.py` and the L10 review, L9A-8).
/// Lower case. Any subdomain counts (`mobile.twitter.com`, `uk.linkedin.com`).
const PROFILE_HOSTS: &[&str] = &[
    "twitter.com/",
    "x.com/",
    "t.me/",
    "linkedin.com/in/",
    "facebook.com/",
    "instagram.com/",
    "reddit.com/u/",
    "reddit.com/user/",
    "github.com/",
    "bsky.app/profile/",
    "youtube.com/@",
    "youtube.com/c/",
    "youtube.com/user/",
    "youtube.com/channel/",
    "tiktok.com/@",
    "mastodon.social/@",
    "mastodon.online/@",
    "mstdn.social/@",
    "fosstodon.org/@",
    "hachyderm.io/@",
    "infosec.exchange/@",
];

/// First path segments that are a site's own pages, not a profile (share and
/// login links and the like).
const NOT_A_PROFILE: &[&str] = &[
    "intent",
    "share",
    "sharer",
    "sharer.php",
    "sharing",
    "home",
    "search",
    "hashtag",
    "i",
    "login",
    "signup",
    "explore",
    "settings",
    "about",
    "privacy",
    "tos",
    "features",
    "pricing",
    "topics",
    "marketplace",
    "sponsors",
    "orgs",
    "submit",
    "dialog",
    "watch",
    "results",
];

/// Organisations' and companies' own accounts a public build may link, as
/// `<host pattern><handle>` in lower case (never a person; reviewed when
/// added). The GitHub organisations are the licence sources of the pool list
/// (fixtures/pools/PROVENANCE.md) and of a bundled font, and the BIPs
/// repository the BIP-322 test vectors come from (fixtures/bip322/README.md,
/// linked from the Labels page, W8).
const ENTITY_HANDLES: &[&str] = &[
    "github.com/bitcoin",
    "github.com/bitcoin-data",
    "github.com/instrument",
    "x.com/okx",
];

/// Machine addresses that are not a person's (an SSH remote).
const NOT_PERSONAL_EMAILS: &[&str] = &["git@github.com", "git@gitlab.com", "git@bitbucket.org"];

/// File extensions that look like a TLD after an `@` (`logo@2x.png`).
const FILE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "avif", "ico", "js", "mjs", "cjs", "css", "json",
    "wasm", "woff", "woff2", "ttf", "txt", "html", "htm", "map", "md", "pdf",
];

/// Most of the 16 bytes on each side of `start..end` are printable text, so a
/// match is not chance bytes inside compressed or random data (a base64 run
/// decoded at the wrong alignment is noise; a real address sits in text).
fn in_text(bytes: &[u8], start: usize, end: usize) -> bool {
    let lo = start.saturating_sub(16);
    let hi = (end + 16).min(bytes.len());
    let around = bytes[lo..start].iter().chain(&bytes[end..hi]);
    let (n, printable) = around.fold((0usize, 0usize), |(n, p), b| {
        let ok = (0x20..0x7f).contains(b) || b"\t\r\n".contains(b);
        (n + 1, p + usize::from(ok))
    });
    printable * 4 >= n * 3
}

/// `bytes` with obfuscated e-mail separators written plainly: `[at]`, `(at)`,
/// `{at}` (spaces around them too), `&#64;`, `&#x40;`, `&commat;` become `@`;
/// `[dot]`, `(dot)`, `{dot}`, `&#46;` become `.`. `None` when there are none.
fn deobfuscated(bytes: &[u8]) -> Option<Vec<u8>> {
    let low = bytes.to_ascii_lowercase();
    const SUBS: &[(&[u8], u8)] = &[
        (b"[at]", b'@'),
        (b"(at)", b'@'),
        (b"{at}", b'@'),
        (b"&#64;", b'@'),
        (b"&#x40;", b'@'),
        (b"&commat;", b'@'),
        (b"[dot]", b'.'),
        (b"(dot)", b'.'),
        (b"{dot}", b'.'),
        (b"&#46;", b'.'),
    ];
    if !SUBS
        .iter()
        .any(|(p, _)| low.windows(p.len()).any(|w| w == *p))
    {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some((p, c)) = SUBS.iter().find(|(p, _)| low[i..].starts_with(p)) {
            while out.last() == Some(&b' ') {
                out.pop();
            }
            out.push(*c);
            i += p.len();
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// E-mail addresses in `bytes` (`local@domain.tld`, the TLD 2-24 letters and
/// not a file extension), also when written `[at]`/`(at)`/`&#64;`, lower-cased,
/// except reserved example domains, [`ROLE_CONTACTS`] and
/// [`NOT_PERSONAL_EMAILS`].
fn emails(bytes: &[u8]) -> BTreeSet<String> {
    let mut out = plain_emails(bytes);
    if let Some(d) = deobfuscated(bytes) {
        out.extend(plain_emails(&d));
    }
    out
}

fn plain_emails(bytes: &[u8]) -> BTreeSet<String> {
    let local = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
    let host = |b: u8| b.is_ascii_alphanumeric() || b".-".contains(&b);
    let mut out = BTreeSet::new();
    for (at, _) in bytes.iter().enumerate().filter(|(_, b)| **b == b'@') {
        let mut s = at;
        while s > 0 && at - s < 64 && local(bytes[s - 1]) {
            s -= 1;
        }
        let mut e = at + 1;
        while e < bytes.len() && e - at <= 253 && host(bytes[e]) {
            e += 1;
        }
        // Trim a trailing dot or hyphen (end of a sentence), then require a
        // letters-only TLD after the last dot.
        while e > at + 1 && b".-".contains(&bytes[e - 1]) {
            e -= 1;
        }
        let domain = &bytes[at + 1..e];
        let Some(dot) = domain.iter().rposition(|b| *b == b'.') else {
            continue;
        };
        let tld = &domain[dot + 1..];
        if s == at || dot == 0 || !(2..=24).contains(&tld.len()) {
            continue;
        }
        if !tld.iter().all(u8::is_ascii_alphabetic) || !in_text(bytes, s, e) {
            continue;
        }
        let m = String::from_utf8_lossy(&bytes[s..e]).to_ascii_lowercase();
        let d = &m[at - s + 1..];
        let tld = &d[d.rfind('.').map_or(0, |i| i + 1)..];
        let example = ["example.com", "example.org", "example.net", "example"]
            .iter()
            .any(|x| d == *x || d.ends_with(&format!(".{x}")));
        if !example
            && !FILE_EXTENSIONS.contains(&tld)
            && !ROLE_CONTACTS.contains(&m.as_str())
            && !NOT_PERSONAL_EMAILS.contains(&m.as_str())
        {
            out.insert(m);
        }
    }
    out
}

/// Profile links in `bytes` ([`PROFILE_HOSTS`], on the host itself or any
/// subdomain, followed by a handle, `@` allowed), lower-cased as
/// `<host pattern><handle>`. A host inside a longer name (`okx.com/`) is not
/// one; share and login pages ([`NOT_A_PROFILE`]) and [`ENTITY_HANDLES`] pass.
fn profile_links(bytes: &[u8]) -> BTreeSet<String> {
    let low = bytes.to_ascii_lowercase();
    let name = |b: u8| b.is_ascii_alphanumeric() || b"._-".contains(&b);
    let hostc = |b: u8| b.is_ascii_alphanumeric() || b".-".contains(&b);
    let mut out = BTreeSet::new();
    for h in PROFILE_HOSTS {
        let hb = h.as_bytes();
        for i in (0..low.len().saturating_sub(hb.len())).filter(|&i| low[i..].starts_with(hb)) {
            // The host must start a host name, or follow a subdomain's dot.
            let mut s = i;
            if s > 0 && low[s - 1] == b'.' {
                s -= 1;
                while s > 0 && hostc(low[s - 1]) {
                    s -= 1;
                }
                if s == i - 1 {
                    continue;
                }
            } else if s > 0 && (low[s - 1].is_ascii_alphanumeric() || low[s - 1] == b'-') {
                continue;
            }
            let mut b = i + hb.len();
            if b < low.len() && low[b] == b'@' {
                b += 1;
            }
            let mut e = b;
            while e < low.len() && name(low[e]) {
                e += 1;
            }
            let handle = String::from_utf8_lossy(&low[b..e]).into_owned();
            if e == b || NOT_A_PROFILE.contains(&handle.as_str()) || !in_text(&low, s, e) {
                continue;
            }
            let key = format!("{h}{handle}");
            if !ENTITY_HANDLES.contains(&key.as_str()) {
                out.insert(key);
            }
        }
    }
    out
}

/// What a public build may hold: label addresses are refused, and every other
/// Bitcoin address must be known (L8-F1).
struct Refs {
    labels: BTreeSet<String>,
    known: BTreeSet<String>,
}

/// Personal data other than addresses found by [`scan`] (L9A-8).
#[derive(Default)]
struct Personal {
    emails: BTreeSet<String>,
    profiles: BTreeSet<String>,
}

fn refs() -> Refs {
    Refs {
        labels: label_addresses(),
        known: known_addresses(),
    }
}

/// Label addresses (`hits`) and unknown Bitcoin addresses (`unknown`) found
/// in `bytes` or anything it encodes.
fn scan(
    bytes: &[u8],
    refs: &Refs,
    depth: usize,
    hits: &mut BTreeSet<String>,
    unknown: &mut BTreeSet<String>,
    personal: &mut Personal,
    bad: &mut Vec<String>,
) {
    for t in tokens(bytes).flat_map(|t| std::iter::once(t).chain(packed(t))) {
        if refs.labels.contains(t) {
            hits.insert(t.to_owned());
        } else if !refs.known.contains(t) && is_btc_address(t) {
            unknown.insert(t.to_owned());
        }
    }
    personal.emails.extend(emails(bytes));
    personal.profiles.extend(profile_links(bytes));
    if depth == MAX_DEPTH {
        return;
    }
    let mut inner = hex_runs(bytes);
    inner.extend(b64_runs(bytes));
    inner.extend(unescaped(bytes));
    match unpacked(bytes) {
        Ok(u) => inner.extend(u),
        Err(e) => bad.push(e),
    }
    if depth == 0 {
        inner.extend(unbrotli(bytes));
    }
    for c in inner {
        scan(&c, refs, depth + 1, hits, unknown, personal, bad);
    }
}

/// Every problem of one file of a public build, at `rel` under its scanned
/// directory.
fn problems(rel: &str, raw: &[u8], refs: &Refs) -> Vec<String> {
    let mut bad = Vec::new();
    if !allowed(rel) {
        bad.push("not on the public build's allowlist (AUD-2R-F1)".into());
    }
    let mut hits = BTreeSet::new();
    let mut unknown = BTreeSet::new();
    let mut personal = Personal::default();
    scan(
        raw,
        refs,
        0,
        &mut hits,
        &mut unknown,
        &mut personal,
        &mut bad,
    );
    // Counts only: the build log must not repeat the personal data it found.
    if !personal.emails.is_empty() {
        bad.push(format!(
            "{} e-mail address(es) (L9A-8)",
            personal.emails.len()
        ));
    }
    if !personal.profiles.is_empty() {
        bad.push(format!(
            "{} social-media profile link(s) (L9A-8)",
            personal.profiles.len()
        ));
    }
    if !hits.is_empty() {
        bad.push(format!("{} label address(es)", hits.len()));
    }
    if !unknown.is_empty() {
        bad.push(format!(
            "{} unlisted bitcoin address(es) (L8-F1)",
            unknown.len()
        ));
    }
    if rel.ends_with("demo.json") {
        match serde_json::from_slice::<serde_json::Value>(raw) {
            Ok(v) => {
                if v["public"] != serde_json::Value::Bool(true) {
                    bad.push("demo.json is not marked public".into());
                }
                for r in v["runs"].as_array().into_iter().flatten() {
                    if !r["input"].is_null() {
                        bad.push(format!("run {} names a recipe input", r["id"]));
                    }
                }
            }
            Err(e) => bad.push(format!("demo.json does not parse: {e}")),
        }
    }
    bad
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let p = e.expect("entry").path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The guard `scripts/build-web.sh` runs on a public build.
#[test]
fn a_public_build_carries_no_label_rows() {
    let Ok(dirs) = std::env::var("ONYX_PUBLIC_BUILD_DIRS") else {
        eprintln!("ONYX_PUBLIC_BUILD_DIRS unset: no public build to check");
        return;
    };
    let refs = refs();
    let mut checked = 0;
    let mut bad = Vec::new();
    for d in dirs.split(':').filter(|d| !d.is_empty()) {
        let base = root().join(d);
        let mut all = Vec::new();
        files(&base, &mut all);
        for f in all {
            let raw = std::fs::read(&f).expect("read");
            checked += 1;
            let rel = f.strip_prefix(&base).expect("under base").to_string_lossy();
            for p in problems(&rel, &raw, &refs) {
                bad.push(format!("{d}/{rel}: {p}"));
            }
        }
    }
    assert!(checked > 0, "no files under {dirs}");
    assert!(bad.is_empty(), "public build refused:\n{}", bad.join("\n"));
}

/// The guard refuses every form a label row could take in a build (the
/// reviewer's plants included: a `.gz` copy, a base64 JSON field, a `data:`
/// URI in a script), and passes allowlisted files with only entity rows.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_guard_refuses_label_rows_in_any_form() {
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use std::io::Write;
    let refs = refs();
    let a = refs.labels.iter().next().expect("one").clone();
    let row = format!("[{{\"address\":\"btc:{a}\",\"entity\":\"entity:x\"}}]");
    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).expect("gzip");
        e.finish().expect("gzip")
    };
    let input = STANDARD.encode(format!("\x01\x02{a}\x03"));
    let js = |body: String| format!("const x=1;{body};export{{x}}").into_bytes();
    let planted: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "data/input-full.b64.txt",
            input.clone().into_bytes(),
            "allowlist",
        ),
        // Allowlisted names, label rows hidden inside.
        (
            "data/revenue-vectors.json",
            row.clone().into_bytes(),
            "label",
        ),
        (
            "data/pricing-defaults.json",
            format!("{{\"note\":\"{}\"}}", STANDARD.encode(&row)).into_bytes(),
            "label",
        ),
        (
            "data/contributor-policy.json",
            format!("{{\"note\":\"{}\"}}", URL_SAFE_NO_PAD.encode(&row)).into_bytes(),
            "label",
        ),
        (
            "app/immutable/chunks/a1B2-c3.js",
            js(format!(
                "fetch(\"data:application/json;base64,{}\")",
                STANDARD.encode(&row)
            )),
            "label",
        ),
        (
            "app/immutable/nodes/0.x1.js",
            js(format!(
                "fetch(\"data:application/json,{}\")",
                row.bytes().map(|b| format!("%{b:02X}")).collect::<String>()
            )),
            "label",
        ),
        (
            "app/immutable/entry/app.x2.js",
            js(format!(
                "const t=\"{}\"",
                row.bytes()
                    .map(|b| format!("\\x{b:02x}"))
                    .collect::<String>()
            )),
            "label",
        ),
        (
            "app/immutable/chunks/z.js",
            js(format!(
                "const t=\"0x{}\"",
                hex::encode(format!("..{a}..").repeat(3))
            )),
            "label",
        ),
        (
            "app/immutable/chunks/g.js",
            js(format!(
                "const t=\"{}\"",
                STANDARD.encode(gz(row.as_bytes()))
            )),
            "label",
        ),
        // Compressed copies are off the allowlist, and scanned too.
        ("data/demo.json.gz", gz(row.as_bytes()), "allowlist"),
        ("history/week/index.json.br", b"x".to_vec(), "allowlist"),
        ("level2/new-feed.json", b"{}".to_vec(), "allowlist"),
        ("level5/track-real.json", b"{}".to_vec(), "allowlist"),
        ("level5/track-example.json.gz", b"x".to_vec(), "allowlist"),
        (
            "app/immutable/chunks/../../labels.js",
            b"x".to_vec(),
            "allowlist",
        ),
        ("history/week/2026-1-04.json", b"{}".to_vec(), "allowlist"),
        (
            "data/demo.json",
            b"{\"public\":true,\"runs\":[{\"id\":\"full\",\"input\":\"data/input-full.b64.txt\"}]}"
                .to_vec(),
            "recipe input",
        ),
        (
            "data/demo.json",
            b"{\"runs\":[{\"id\":\"full\",\"input\":null}]}".to_vec(),
            "not marked public",
        ),
    ];
    for (name, raw, why) in &planted {
        let p = problems(name, raw, &refs);
        assert!(
            p.iter().any(|p| p.contains(why)),
            "{name} passed or wrong reason: {p:?}"
        );
    }
    // The .gz plant is found inside as well as by name.
    assert!(problems("data/demo.json.gz", &gz(row.as_bytes()), &refs)
        .iter()
        .any(|p| p.contains("label")));
    let clean = b"{\"public\":true,\"runs\":[{\"id\":\"full\",\"input\":null,\"rows\":[{\"entity\":\"entity:binance\"}]}]}";
    assert_eq!(
        problems("data/demo.json", clean, &refs),
        Vec::<String>::new()
    );
    assert_eq!(problems("demo.json", clean, &refs), Vec::<String>::new());
    for ok in [
        "index.html",
        "app/immutable/chunks/Dq3x-9a_.js",
        "app/immutable/assets/0.AbC.css",
        "app/immutable/workers/verifier.worker-DIuKoh6u.js",
        "app/immutable/workers/assets/onyx_web_verify_bg-qVbCORbN.wasm",
        "fonts/ibm-plex-sans-latin-400-normal.woff2",
        "fonts/OFL-ibm-plex.txt",
        "licenses/LICENSE-lucide.txt",
        "history/week/2026-10-04.json",
        "history/week/index.json",
        "recipe.wasm",
    ] {
        assert!(allowed(ok), "{ok} refused");
    }
}

/// W11: addresses stored back to back (wasm data, a minified string) are one
/// alphanumeric run that is no address as a whole; the scan splits it at
/// every bech32 hrp and base58 version character, so each one is still found.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_guard_finds_addresses_packed_together() {
    use bitcoin::secp256k1::{Secp256k1, SecretKey};
    let refs = refs();
    let mut labels = refs.labels.iter();
    let (a, b) = (labels.next().expect("one"), labels.next().expect("two"));
    let sk =
        SecretKey::from_slice(&onyx_core::keccak256(b"onyx w11 packed address").0).expect("key");
    let pk = bitcoin::CompressedPublicKey(sk.public_key(&Secp256k1::new()));
    let seg = bitcoin::Address::p2wpkh(&pk, bitcoin::Network::Bitcoin).to_string();
    let legacy = bitcoin::Address::p2pkh(pk, bitcoin::Network::Bitcoin).to_string();
    let tb = bitcoin::Address::p2wpkh(&pk, bitcoin::Network::Testnet)
        .to_string()
        .to_uppercase();
    let wasm = |body: String| {
        let mut w = b"\0asm\x01\0\0\0\x0b\x83\x01\x01\0\x41\x08\x0b".to_vec();
        w.extend_from_slice(body.as_bytes());
        w.extend_from_slice(b"\0\0\x10");
        w
    };
    let wasm_path = "app/immutable/workers/assets/onyx_web_verify_bg-x1.wasm";
    // Two label addresses back to back, and one glued between identifier text.
    for body in [
        format!("{a}{b}"),
        format!("abcXYZ{a}defUVW"),
        format!("{b}{a}{b}"),
    ] {
        let p = problems(wasm_path, &wasm(body.clone()), &refs);
        assert!(
            p.iter().any(|p| p.contains("label")),
            "{body} passed: {p:?}"
        );
    }
    // Unlisted addresses packed with each other and with a known one, any network or case.
    let known = "bc1q9vza2e8x573nczrlzms0wvx3gsqjx7vavgkx0l";
    for body in [
        format!("{seg}{legacy}"),
        format!("{legacy}{seg}"),
        format!("{known}{seg}"),
        format!("{legacy}{known}"),
        format!("{tb}{known}"),
        format!("x{legacy}9"),
    ] {
        let p = problems(wasm_path, &wasm(body.clone()), &refs);
        assert!(
            p.iter().any(|p| p.contains("unlisted")),
            "{body} passed: {p:?}"
        );
    }
    // Known addresses packed together still pass, and so does a long run of other text.
    let pool = refs
        .known
        .iter()
        .find(|a| a.starts_with('1') || a.starts_with('3'))
        .expect("a pool address");
    for body in [
        format!("{known}{pool}{known}"),
        // The address-spelling vectors, as the verifier's data section stores them.
        "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdqBC1QAR0SRRR7XFKVY5L643LYDNW9RE59GTZZWF5MDQ1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".into(),
        "abcdefghijkmnopqrstuvwxyz123456789ABCDEFGHJKLMNPQRSTUVWXYZ".repeat(4),
        "0x8421123c0290c9a41205415489f6f4da945cdceedc2681dfb81a4ca665".into(),
    ] {
        assert_eq!(
            problems(wasm_path, &wasm(body.clone()), &refs),
            Vec::<String>::new(),
            "{body}"
        );
    }
}

/// L8-F1: an address that is in no committed label set (a PoR file's other
/// addresses, an excluded list, a person's address) is refused in every form
/// the label scan follows; test-key and pool-list addresses pass.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_guard_refuses_an_unlisted_address_and_passes_known_ones() {
    use base64::engine::general_purpose::STANDARD;
    use bitcoin::secp256k1::{Secp256k1, SecretKey};
    let refs = refs();
    let sk =
        SecretKey::from_slice(&onyx_core::keccak256(b"onyx l8 planted address").0).expect("key");
    let pk = bitcoin::CompressedPublicKey(sk.public_key(&Secp256k1::new()));
    let other = bitcoin::Address::p2wpkh(&pk, bitcoin::Network::Bitcoin).to_string();
    let legacy = bitcoin::Address::p2pkh(pk, bitcoin::Network::Bitcoin).to_string();
    assert!(!refs.labels.contains(&other) && !refs.known.contains(&other));
    let public = |body: String| {
        format!("{{\"public\":true,\"runs\":[{{\"id\":\"full\",\"input\":null}}],\"x\":{body}}}")
            .into_bytes()
    };
    for (name, raw) in [
        ("data/demo.json", public(format!("\"{other}\""))),
        ("data/demo.json", public(format!("\"{legacy}\""))),
        (
            "data/demo.json",
            public(format!("\"{}\"", STANDARD.encode(format!("[\"{other}\"]")))),
        ),
        (
            "history/reserves-week.json",
            format!("{{\"a\":\"{other}\"}}").into_bytes(),
        ),
        (
            "app/immutable/chunks/k.js",
            format!(
                "const a=\"{}\"",
                other
                    .bytes()
                    .map(|b| format!("\\x{b:02x}"))
                    .collect::<String>()
            )
            .into_bytes(),
        ),
    ] {
        let p = problems(name, &raw, &refs);
        assert!(
            p.iter().any(|p| p.contains("unlisted")),
            "{name} passed: {p:?}"
        );
    }
    // A testnet address is refused too (any network).
    let tb = bitcoin::Address::p2wpkh(&pk, bitcoin::Network::Testnet).to_string();
    assert!(
        problems("data/demo.json", &public(format!("\"{tb}\"")), &refs)
            .iter()
            .any(|p| p.contains("unlisted"))
    );
    // Known addresses pass: a proof vector, the demo attestation, a pool's.
    let pool = refs
        .known
        .iter()
        .find(|a| a.starts_with('1'))
        .expect("a pool address")
        .clone();
    for k in [
        "bc1q9vza2e8x573nczrlzms0wvx3gsqjx7vavgkx0l",
        "bc1qxqqpumz9saem3kyx4zc6twqmrdqeusydel9q6z",
        &pool,
    ] {
        assert!(refs.known.contains(k), "{k} not known");
        assert_eq!(
            problems("data/demo.json", &public(format!("\"{k}\"")), &refs),
            Vec::<String>::new()
        );
    }
    // Hashes and hex runs are not addresses.
    assert_eq!(
        problems(
            "data/demo.json",
            &public("\"0x8421123c0290c9a41205415489f6f4da945cdceedc2681dfb81a4ca665\"".into()),
            &refs
        ),
        Vec::<String>::new()
    );
}

/// L9A-8: an e-mail address or a social-media profile link is refused in the
/// forms the label scan follows (plain, base64, `%XX`, gzip in base64); a
/// reserved example domain, a host inside a longer name, a package name and
/// random bytes pass. The refusal names counts, never the address.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_guard_refuses_emails_and_profile_links() {
    use base64::engine::general_purpose::STANDARD;
    use std::io::Write;
    let refs = refs();
    let public = |body: &str| {
        format!(
            "{{\"public\":true,\"runs\":[{{\"id\":\"full\",\"input\":null}}],\"x\":\"{body}\"}}"
        )
        .into_bytes()
    };
    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).expect("gzip");
        e.finish().expect("gzip")
    };
    let mail = "Jane.Doe+onyx@Mail-Host.co.invalid";
    let note = format!("{{\"contact\":\"{mail}\",\"note\":\"padding padding\"}}");
    for (name, raw, why) in [
        (
            "data/demo.json",
            public(&format!("write to {mail}.")),
            "e-mail",
        ),
        ("data/demo.json", public(&STANDARD.encode(&note)), "e-mail"),
        (
            "app/immutable/chunks/m.js",
            format!(
                "fetch(\"data:text/plain,{}\")",
                note.bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect::<String>()
            )
            .into_bytes(),
            "e-mail",
        ),
        (
            "app/immutable/chunks/g.js",
            format!("const t=\"{}\"", STANDARD.encode(gz(note.as_bytes()))).into_bytes(),
            "e-mail",
        ),
        (
            "data/demo.json",
            public("see https://twitter.com/some_handle"),
            "profile",
        ),
        (
            "data/demo.json",
            public("see https://www.X.com/handle"),
            "profile",
        ),
        (
            "data/demo.json",
            public("linkedin.com/in/jane-doe-123"),
            "profile",
        ),
        ("data/demo.json", public("t.me/somechannel"), "profile"),
        (
            "data/demo.json",
            public(&STANDARD.encode("{\"who\":\"https://reddit.com/user/someone\"}")),
            "profile",
        ),
        // L10 review: obfuscated e-mails.
        (
            "data/demo.json",
            public("jane [at] mail-host [dot] invalid"),
            "e-mail",
        ),
        (
            "data/demo.json",
            public("jane(at)mail-host.invalid"),
            "e-mail",
        ),
        (
            "data/demo.json",
            public("jane&#64;mail-host.invalid"),
            "e-mail",
        ),
        // L10 review: subdomains, @handles and more hosts.
        (
            "data/demo.json",
            public("https://mobile.twitter.com/someone"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://uk.linkedin.com/in/jane-doe"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://old.reddit.com/user/someone"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://twitter.com/@someone"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://github.com/someone"),
            "profile",
        ),
        // W8 review: an allowed organisation does not open its prefix.
        (
            "data/demo.json",
            public("https://github.com/bitcoinjane"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://bsky.app/profile/someone.bsky.social"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://www.youtube.com/@someone"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://mastodon.social/@someone"),
            "profile",
        ),
        (
            "data/demo.json",
            public("https://www.tiktok.com/@someone"),
            "profile",
        ),
    ] {
        let p = problems(name, &raw, &refs);
        assert!(p.iter().any(|p| p.contains(why)), "{name} passed: {p:?}");
        assert!(
            p.iter().all(|p| !p.to_ascii_lowercase().contains("jane")),
            "the refusal repeats the data: {p:?}"
        );
    }
    // Not personal: example domains, a host inside a longer name, package
    // names and versions, a bare host, CSS at-rules.
    for ok in [
        "user@example.com, ops@node.example, a@b.example.org",
        "https://www.okx.com/proof-of-reserves and https://box.com/x",
        "@sveltejs/kit svelte@5.0.0 @media (max-width: 40rem)",
        "x.com and twitter.com, no path",
        // L10 review: an image name, an SSH remote, an entity's own account,
        // share links, the licence sources' organisations.
        "img/logo@2x.png and git@github.com:org/repo.git",
        "https://x.com/okx and https://github.com/bitcoin-data/mining-pools",
        "https://github.com/bitcoin/bips/tree/master/bip-0322",
        "https://twitter.com/intent/tweet?text=x https://www.facebook.com/sharer/sharer.php?u=x",
        "https://www.linkedin.com/sharing/share-offsite/?url=x https://t.me/share/url?url=x",
    ] {
        assert_eq!(
            problems("data/demo.json", &public(ok), &refs),
            Vec::<String>::new(),
            "{ok}"
        );
    }
    // Random bytes (as in a font, an image or a mis-aligned base64 decode)
    // carry no chance hit: 4 MiB from a keccak stream.
    let mut noise = Vec::with_capacity(4 << 20);
    let mut h = onyx_core::keccak256(b"onyx l9a-8 noise").0;
    while noise.len() < 4 << 20 {
        h = onyx_core::keccak256(&h).0;
        noise.extend_from_slice(&h);
    }
    let (mut hits, mut unknown, mut personal, mut bad) = (
        BTreeSet::new(),
        BTreeSet::new(),
        Personal::default(),
        Vec::new(),
    );
    scan(
        &noise,
        &refs,
        3,
        &mut hits,
        &mut unknown,
        &mut personal,
        &mut bad,
    );
    assert!(
        personal.emails.is_empty() && personal.profiles.is_empty(),
        "chance hits in random bytes: {} e-mail, {} profile",
        personal.emails.len(),
        personal.profiles.len()
    );
}

// ROADMAP L14, ADR-0035, THREATS L8-5: a rights-fallback build (`ONYX_RIGHTS=fallback
// ONYX_PUBLIC=1 scripts/build-web.sh`) ships no total derived from an entity label set:
// no company flow, balance, reserve or per-entity history, in any form. `build-web.sh`
// runs `a_fallback_build_carries_no_label_derived_totals` over that build with
// `ONYX_FALLBACK_BUILD_DIRS`; without it that test checks nothing.
//
// The guard fails closed, on top of the public guard above:
// 1. The label-derived history files are refused by path ([`FALLBACK_REFUSED`]).
// 2. Every file, in every encoding the public guard follows, is scanned for the totals
//    the committed outputs hold ([`fallback_totals`]): satoshi amounts (7 digits or
//    more), BTC amounts with 8 decimals (grouped or not) and row leaves (as bytes, so
//    hex, base64 or compressed copies are found too).
// 3. Every JSON file is refused if it has a field that carries a company total
//    ([`TOTAL_FIELDS`], or an `entity` naming a label-set entity), and `demo.json` must be
//    marked `rights: "fallback"` with empty run and live rows.
// 4. Review L14-R1: a figure next to a label-set company's name, in any file and any
//    encoding, whatever its value ([`entity_figures`]): a percentage, an amount in BTC,
//    sat or ₿, or a decimal with 2+ places and 3+ integer digits. Rules 1-3 know the
//    committed totals; this one catches what is derived from them (a share, a rounded
//    total, a sum) and text the bundler left in the build (THREATS L14-R1-1).

/// Paths a fallback build must not ship: every row in them is a company total.
const FALLBACK_REFUSED: &[&str] = &[
    "history/week/",
    "history/reserves-week.json",
    "history/rosters-week.json",
];

/// JSON fields that hold a company total (or the leaves of company rows).
const TOTAL_FIELDS: &[&str] = &[
    "received_sats",
    "sent_sats",
    "net_sats",
    "balance_sats",
    "base_sats",
    "theirs_sats",
    "onyx_sats",
    "okx",
    "okx_btc",
    "okx_net_btc",
    "series_leaves",
    "balance_leaves",
];

/// Leaf bytes are looked up by this many leading bytes.
const LEAF_KEY: usize = 24;

/// Bytes of a leaf that must match: past the entity name and heights, into the amounts.
const LEAF_MATCH: usize = 64;

/// Shortest satoshi amount the scan looks for (0.01 BTC): shorter runs of digits
/// (block heights, counts) are everywhere in a build.
const MIN_SATS_DIGITS: usize = 7;

/// The label-derived totals a fallback build must not carry.
#[derive(Default)]
struct Totals {
    /// Satoshi amounts, decimal, no sign, no leading zeros.
    sats: std::collections::HashSet<String>,
    /// BTC amounts, `<int>.<8 digits>`, no sign, no grouping, no leading zeros.
    btc: std::collections::HashSet<String>,
    /// Row leaves (Borsh bytes) by their first [`LEAF_KEY`] bytes.
    leaves: std::collections::HashMap<Vec<u8>, Vec<Vec<u8>>>,
}

/// `"0012"` -> `"12"`, `""`/`"000"` -> `"0"`.
fn no_lead(d: &str) -> String {
    let t = d.trim_start_matches('0');
    if t.is_empty() {
        "0".into()
    } else {
        t.into()
    }
}

impl Totals {
    fn add_sats(&mut self, raw: &str) {
        let d = raw.trim_start_matches(['-', '+', '\u{2212}']);
        if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
            return;
        }
        let d = no_lead(d);
        if d.len() < MIN_SATS_DIGITS {
            return;
        }
        let padded = format!("{d:0>9}");
        let (i, f) = padded.split_at(padded.len() - 8);
        self.btc.insert(format!("{}.{f}", no_lead(i)));
        self.sats.insert(d);
    }

    fn add_btc(&mut self, raw: &str) {
        let t: String = raw
            .trim_start_matches(['-', '+', '\u{2212}'])
            .chars()
            .filter(|c| *c != ',')
            .collect();
        let Some((i, f)) = t.split_once('.') else {
            return;
        };
        if i.is_empty()
            || f.len() != 8
            || !i.bytes().all(|b| b.is_ascii_digit())
            || !f.bytes().all(|b| b.is_ascii_digit())
        {
            return;
        }
        self.btc.insert(format!("{}.{f}", no_lead(i)));
        self.add_sats(&format!("{i}{f}"));
    }

    fn add_leaf(&mut self, hex_str: &str) {
        if let Ok(b) = hex::decode(hex_str.trim_start_matches("0x")) {
            if b.len() >= LEAF_KEY {
                self.leaves
                    .entry(b[..LEAF_KEY].to_vec())
                    .or_default()
                    .push(b);
            }
        }
    }

    fn leaf_count(&self) -> usize {
        self.leaves.values().map(Vec::len).sum()
    }

    /// Every amount and leaf in `v`: `*_sats` fields, `leaf`/`*leaves` fields, BTC
    /// amounts written in any text, and with `ints`, long integers in any text (the
    /// recount lines print raw satoshis).
    fn collect(&mut self, v: &serde_json::Value, ints: bool) {
        use serde_json::Value;
        let text = |t: &mut Totals, s: &str| {
            for b in btc_amounts(s.as_bytes()) {
                t.add_btc(&b);
            }
            if ints {
                for d in digit_runs(s.as_bytes()) {
                    t.add_sats(&d);
                }
            }
        };
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if k.ends_with("_sats") {
                        match x {
                            Value::String(s) => self.add_sats(s),
                            Value::Number(n) => self.add_sats(&n.to_string()),
                            _ => {}
                        }
                    } else if k == "leaf" || k.ends_with("leaves") {
                        match x {
                            Value::String(s) => self.add_leaf(s),
                            Value::Array(a) => {
                                for s in a.iter().filter_map(Value::as_str) {
                                    self.add_leaf(s);
                                }
                            }
                            _ => {}
                        }
                    }
                    self.collect(x, ints);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| self.collect(x, ints)),
            Value::String(s) => text(self, s),
            _ => {}
        }
    }

    /// Everything under a field whose name says OKX (the run records' figures).
    fn collect_okx(&mut self, v: &serde_json::Value) {
        use serde_json::Value;
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if k.to_ascii_lowercase().contains("okx") {
                        self.collect_all(x);
                    } else {
                        self.collect_okx(x);
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| self.collect_okx(x)),
            _ => {}
        }
    }

    fn collect_all(&mut self, v: &serde_json::Value) {
        use serde_json::Value;
        match v {
            Value::Object(m) => m.values().for_each(|x| self.collect_all(x)),
            Value::Array(a) => a.iter().for_each(|x| self.collect_all(x)),
            Value::String(s) => {
                self.add_btc(s);
                self.add_sats(s);
            }
            Value::Number(n) => {
                let s = n.to_string();
                self.add_btc(&s);
                self.add_sats(&s);
            }
            _ => {}
        }
    }
}

/// BTC amounts written with 8 decimals (`146,358.26346743`, `-107.99382189`): digits
/// and commas, a point, exactly 8 digits, no digit on either side.
fn btc_amounts(b: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for (p, _) in b.iter().enumerate().filter(|(_, c)| **c == b'.') {
        let f = &b[p + 1..];
        if f.len() < 8
            || !f[..8].iter().all(u8::is_ascii_digit)
            || f.get(8).is_some_and(u8::is_ascii_digit)
        {
            continue;
        }
        let mut s = p;
        while s > 0 && (b[s - 1].is_ascii_digit() || b[s - 1] == b',') {
            s -= 1;
        }
        while s < p && b[s] == b',' {
            s += 1;
        }
        if s < p {
            out.push(String::from_utf8_lossy(&b[s..p + 9]).into_owned());
        }
    }
    out
}

/// Runs of digits at least [`MIN_SATS_DIGITS`] long that stand alone: no letter, digit,
/// point or comma next to them (so not part of a hash, a BTC amount or a grouped count).
fn digit_runs(b: &[u8]) -> Vec<String> {
    let edge = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b',';
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let alone = (s == 0 || !edge(b[s - 1])) && (i == b.len() || !edge(b[i]));
        if alone && i - s >= MIN_SATS_DIGITS {
            out.push(String::from_utf8_lossy(&b[s..i]).into_owned());
        }
    }
    out
}

fn json_file(p: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The label-derived totals in the repository's committed outputs (the per-entity
/// history, the reserve history, the reserve cross-checks, the run records' OKX
/// figures) and, when present, in the private export the site is built from.
fn fallback_totals() -> Totals {
    let mut t = Totals::default();
    let web = root().join("apps/web/static");
    for e in std::fs::read_dir(web.join("history/week")).expect("history dir") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "json") {
            t.collect(&json_file(&p), false);
        }
    }
    t.collect(&json_file(&web.join("history/reserves-week.json")), false);
    t.collect(&json_file(&web.join("history/rosters-week.json")), false);
    t.collect(
        &json_file(&root().join("fixtures/reserves/cross-checks.json")),
        false,
    );
    for e in std::fs::read_dir(web.join("run")).expect("run dir") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "json") {
            t.collect_okx(&json_file(&p));
        }
    }
    // The private export (apps/demo/site/data, not committed): its company rows only,
    // never its blocks (block data is not label-derived).
    let demo = root().join("apps/demo/site/data/demo.json");
    if demo.exists() {
        let d = json_file(&demo);
        for r in d["runs"].as_array().into_iter().flatten() {
            t.collect(&r["rows"], true);
        }
        t.collect(&d["spread"], true);
        for k in ["rows", "per_block", "series", "python_recount"] {
            t.collect(&d["live"][k], true);
        }
    }
    t
}

/// Label-set companies as page text names them (rule 4). "Binance Pool" is a mining
/// pool (chain data), not the exchange.
const ENTITY_NAMES: &[&str] = &[
    "okx",
    "binance",
    "bitmex",
    "bitwise",
    "bitb",
    // Label set v3 (ROADMAP P2a): the issuers whose rosters it carries.
    "bybit",
    "bitfinex",
    "htx",
    "coinex",
    "mexc",
    "crypto.com",
    "deribit",
];

/// Bytes on each side of a company name searched for a figure (rule 4).
const NEAR: usize = 100;

/// Figures within [`NEAR`] bytes of a label-set company's name (rule 4): `12.5%`,
/// `146,358 BTC`, `3 sats`, `₿ 2`, `146358.26`.
fn entity_figures(b: &[u8]) -> Vec<String> {
    let low = b.to_ascii_lowercase();
    let word = |c: Option<&u8>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    let mut out = Vec::new();
    for name in ENTITY_NAMES {
        let n = name.as_bytes();
        let mut from = 0;
        while let Some(off) = low[from..].windows(n.len()).position(|w| w == n) {
            let (s, e) = (from + off, from + off + n.len());
            from = e;
            if word(s.checked_sub(1).and_then(|i| low.get(i))) || word(low.get(e)) {
                continue;
            }
            if *name == "binance" && low[e..].starts_with(b" pool") {
                continue;
            }
            let (ws, we) = (s.saturating_sub(NEAR), (e + NEAR).min(b.len()));
            out.extend(window_figures(&b[ws..we]));
        }
    }
    out
}

/// The figures of rule 4 in one window of text.
fn window_figures(w: &[u8]) -> Vec<String> {
    const BTC_SIGN: &[u8] = "₿".as_bytes();
    let lw = w.to_ascii_lowercase();
    let num = |c: u8| c.is_ascii_digit() || c == b',' || c == b'.';
    let mut out = Vec::new();
    let mut i = 0;
    while i < w.len() {
        if !w[i].is_ascii_digit() || (i > 0 && (w[i - 1].is_ascii_alphanumeric() || num(w[i - 1])))
        {
            i += 1;
            continue;
        }
        let s = i;
        while i < w.len() && num(w[i]) {
            i += 1;
        }
        // Not the comma or point of the sentence after the number.
        let mut e = i;
        while !w[e - 1].is_ascii_digit() {
            e -= 1;
        }
        let figure = &w[s..e];
        let mut j = e;
        while j < w.len() && w[j] == b' ' {
            j += 1;
        }
        let rest = &lw[j..];
        let unit = ["btc", "sats", "sat", "satoshis", "satoshi"]
            .iter()
            .find(|u| {
                rest.starts_with(u.as_bytes())
                    && !rest
                        .get(u.len())
                        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b'_')
            });
        let percent = rest.first() == Some(&b'%');
        let sign = rest.starts_with(BTC_SIGN) || {
            let before = w[..s].strip_suffix(b" ").unwrap_or(&w[..s]);
            before.ends_with(BTC_SIGN)
        };
        let mut parts = figure.split(|c| *c == b'.');
        let int = parts.next().unwrap_or_default();
        let decimal = matches!((parts.next(), parts.next()), (Some(f), None) if f.len() >= 2)
            && (int.contains(&b',') || int.len() >= 3);
        let figure = String::from_utf8_lossy(figure);
        if percent {
            out.push(format!("{figure}%"));
        } else if let Some(u) = unit {
            out.push(format!("{figure} {u}"));
        } else if sign {
            out.push(format!("₿{figure}"));
        } else if decimal {
            out.push(figure.into_owned());
        }
    }
    out
}

/// Label-derived totals found in `bytes` or anything it encodes.
fn fallback_scan(
    bytes: &[u8],
    t: &Totals,
    depth: usize,
    hits: &mut BTreeSet<String>,
    bad: &mut Vec<String>,
) {
    for b in btc_amounts(bytes) {
        let mut x = Totals::default();
        x.add_btc(&b);
        if x.btc.iter().any(|v| t.btc.contains(v)) {
            hits.insert(format!("BTC amount {b}"));
        }
    }
    for d in digit_runs(bytes) {
        if t.sats.contains(&no_lead(&d)) {
            hits.insert(format!("satoshi amount {d}"));
        }
    }
    for f in entity_figures(bytes) {
        hits.insert(format!("figure next to a company name ({f})"));
    }
    if bytes.len() >= LEAF_KEY {
        for i in 0..=bytes.len() - LEAF_KEY {
            if let Some(c) = t.leaves.get(&bytes[i..i + LEAF_KEY]) {
                // A leaf cut short (a base64 run's last partial group is dropped) still
                // counts once its first LEAF_MATCH bytes (entity, heights, amounts) match.
                let rest = &bytes[i..];
                if c.iter().any(|l| {
                    let n = l.len().min(LEAF_MATCH);
                    rest.len() >= n && rest[..n] == l[..n]
                }) {
                    hits.insert(format!("company row leaf at byte {i}"));
                }
            }
        }
    }
    if depth == MAX_DEPTH {
        return;
    }
    let mut inner = hex_runs(bytes);
    inner.extend(b64_runs(bytes));
    inner.extend(unescaped(bytes));
    match unpacked(bytes) {
        Ok(u) => inner.extend(u),
        Err(e) => bad.push(e),
    }
    if depth == 0 {
        inner.extend(unbrotli(bytes));
    }
    for c in inner {
        fallback_scan(&c, t, depth + 1, hits, bad);
    }
}

/// Fields of a JSON file that carry company totals (rule 3).
fn total_fields(v: &serde_json::Value, path: &str, out: &mut Vec<String>) {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let at = format!("{path}.{k}");
                if TOTAL_FIELDS.contains(&k.as_str()) && !x.is_null() {
                    out.push(format!("field {at} carries a company total"));
                } else if k == "entity" && x.as_str().is_some_and(|s| s.starts_with("entity:")) {
                    out.push(format!("field {at} names a label-set entity"));
                }
                total_fields(x, &at, out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                total_fields(x, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// Every problem of one file of a fallback build (on top of [`problems`]).
fn fallback_problems(rel: &str, raw: &[u8], t: &Totals) -> Vec<String> {
    let mut bad = Vec::new();
    if FALLBACK_REFUSED
        .iter()
        .any(|p| rel == *p || (p.ends_with('/') && rel.starts_with(p)))
    {
        bad.push("a label-derived history file (ROADMAP L14)".into());
    }
    let mut hits = BTreeSet::new();
    fallback_scan(raw, t, 0, &mut hits, &mut bad);
    if !hits.is_empty() {
        bad.push(format!(
            "{} label-derived total(s) (ROADMAP L14): {}",
            hits.len(),
            hits.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if rel.ends_with(".json") {
        match serde_json::from_slice::<serde_json::Value>(raw) {
            Ok(v) => {
                total_fields(&v, "", &mut bad);
                if rel.ends_with("demo.json") {
                    if v["rights"] != serde_json::Value::String("fallback".into()) {
                        bad.push("demo.json is not marked rights: fallback".into());
                    }
                    for r in v["runs"].as_array().into_iter().flatten() {
                        if r["rows"].as_array().is_none_or(|a| !a.is_empty()) {
                            bad.push(format!("run {} ships rows", r["id"]));
                        }
                        if r["proofs"].as_array().is_some_and(|a| !a.is_empty()) {
                            bad.push(format!("run {} ships proofs", r["id"]));
                        }
                    }
                    let live = &v["live"];
                    if live["rows"].as_array().is_some_and(|a| !a.is_empty()) {
                        bad.push("live ships rows".into());
                    }
                    for k in ["per_block", "series"] {
                        if !live[k].is_null() {
                            bad.push(format!("live ships {k}"));
                        }
                    }
                    if !v["spread"].is_null() {
                        bad.push("demo.json ships the label-set spread".into());
                    }
                }
            }
            Err(e) => bad.push(format!("{rel} does not parse: {e}")),
        }
    }
    bad
}

/// The reference totals are real: a guard over an empty list would pass anything.
fn checked_totals() -> Totals {
    let t = fallback_totals();
    assert!(t.leaf_count() > 2_000, "only {} leaves", t.leaf_count());
    assert!(t.sats.len() > 100, "only {} satoshi amounts", t.sats.len());
    for b in [
        "146358.26346743",
        "37290.75352194",
        "140974.44061385",
        "144931.85016221",
        "107.99382189",
    ] {
        assert!(t.btc.contains(b), "{b} is not a reference total");
    }
    t
}

/// The guard `scripts/build-web.sh` runs on a rights-fallback build.
#[test]
fn a_fallback_build_carries_no_label_derived_totals() {
    let Ok(dirs) = std::env::var("ONYX_FALLBACK_BUILD_DIRS") else {
        eprintln!("ONYX_FALLBACK_BUILD_DIRS unset: no fallback build to check");
        return;
    };
    let t = checked_totals();
    let mut checked = 0;
    let mut bad = Vec::new();
    for d in dirs.split(':').filter(|d| !d.is_empty()) {
        let base = root().join(d);
        let mut all = Vec::new();
        files(&base, &mut all);
        for f in all {
            let raw = std::fs::read(&f).expect("read");
            checked += 1;
            let rel = f.strip_prefix(&base).expect("under base").to_string_lossy();
            for p in fallback_problems(&rel, &raw, &t) {
                bad.push(format!("{d}/{rel}: {p}"));
            }
        }
    }
    assert!(checked > 0, "no files under {dirs}");
    assert!(
        bad.is_empty(),
        "fallback build refused:\n{}",
        bad.join("\n")
    );
}

/// The fallback guard refuses every form a company total could take (the real
/// history, reserve and run files as committed; a reserve figure grouped, ungrouped or
/// in satoshis; a row leaf in hex, base64 or gzip; a total field; an unmarked demo.json)
/// and passes what a fallback build keeps (blocks, pools, level 2).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_fallback_guard_refuses_label_derived_totals() {
    use base64::engine::general_purpose::STANDARD;
    use std::io::Write;
    let t = checked_totals();
    let web = root().join("apps/web/static");
    let read = |p: &str| std::fs::read(web.join(p)).expect("read");
    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).expect("gzip");
        e.finish().expect("gzip")
    };
    let day: serde_json::Value =
        serde_json::from_slice(&read("history/week/2026-10-10.json")).expect("day");
    let leaf_hex = day["shards"][0]["balance_leaves"][0]
        .as_str()
        .expect("leaf")
        .to_owned();
    let leaf = hex::decode(leaf_hex.trim_start_matches("0x")).expect("hex");
    let js = |body: String| format!("const x=1;{body};export{{x}}").into_bytes();
    let planted: Vec<(&str, Vec<u8>, &str)> = vec![
        // The committed label-derived files, as they are.
        (
            "history/week/2026-10-10.json",
            read("history/week/2026-10-10.json"),
            "history file",
        ),
        (
            "history/week/index.json",
            read("history/week/index.json"),
            "history file",
        ),
        (
            "history/reserves-week.json",
            read("history/reserves-week.json"),
            "history file",
        ),
        (
            "run/base-sepolia-2026-10-14.json",
            read("run/base-sepolia-2026-10-14.json"),
            "label-derived total",
        ),
        (
            "run/live-run.json",
            read("run/live-run.json"),
            "label-derived total",
        ),
        // A reserve figure in the bundle, grouped, plain, negative, or as satoshis in gzip.
        (
            "app/immutable/chunks/r.js",
            js("const o=\"146,358.26346743\"".into()),
            "label-derived total",
        ),
        (
            "app/immutable/chunks/r.js",
            js("const o=37290.75352194".into()),
            "label-derived total",
        ),
        (
            "app/immutable/nodes/2.js",
            js("t=\"−107.99382189 BTC\"".into()),
            "label-derived total",
        ),
        (
            "app/immutable/chunks/s.js",
            js(format!(
                "const s=\"{}\"",
                STANDARD.encode(gz(b"{\"v\":\"14635826346743\"}"))
            )),
            "label-derived total",
        ),
        // A company row leaf: hex, base64 of its bytes, gzip of its bytes.
        (
            "app/immutable/chunks/l.js",
            js(format!("const l=\"{leaf_hex}\"")),
            "label-derived total",
        ),
        (
            "app/immutable/chunks/l.js",
            js(format!("const l=\"{}\"", STANDARD.encode(&leaf))),
            "label-derived total",
        ),
        (
            "app/immutable/workers/assets/v.wasm",
            gz(&leaf),
            "label-derived total",
        ),
        // Rule 4 (review L14-R1): a figure next to a company name that no committed file
        // holds: a share, a rounded total, the ₿ sign, a short decimal, base64 of one.
        (
            "app/immutable/nodes/7.js",
            js("t=`its file covers 99.8% of OKX's labelled value`".into()),
            "figure next to a company name (99.8%)",
        ),
        (
            "app/immutable/nodes/7.js",
            js("t=\"Binance: about 610,000 BTC\"".into()),
            "figure next to a company name (610,000 btc)",
        ),
        (
            "app/immutable/nodes/7.js",
            js("t=\"BitMEX ₿ 7,580\"".into()),
            "figure next to a company name (₿7,580)",
        ),
        (
            "run/x.json",
            b"{\"s\":\"Bitwise BITB holds 37290.75 (rounded)\"}".to_vec(),
            "figure next to a company name (37290.75)",
        ),
        (
            "app/immutable/chunks/b.js",
            js(format!(
                "const s=\"{}\"",
                STANDARD.encode("in the week to block 971,199 OKX took in 12 sats net")
            )),
            "figure next to a company name (12 sats)",
        ),
        // A total field, even with a value the scan does not know.
        (
            "run/live-run.json",
            b"{\"round\":{\"okx\":{\"net_sats\":\"1\"}}}".to_vec(),
            "company total",
        ),
        (
            "level2/sec-holdings.json",
            b"{\"rows\":[{\"entity\":\"entity:okx\",\"x\":1}]}".to_vec(),
            "label-set entity",
        ),
        // demo.json: unmarked, or with rows, proofs, per-block rows, series or the spread.
        (
            "data/demo.json",
            b"{\"public\":true,\"runs\":[]}".to_vec(),
            "rights: fallback",
        ),
        (
            "data/demo.json",
            b"{\"rights\":\"fallback\",\"runs\":[{\"id\":\"full\",\"rows\":[{}]}]}".to_vec(),
            "ships rows",
        ),
        (
            "data/demo.json",
            b"{\"rights\":\"fallback\",\"runs\":[{\"id\":\"full\",\"rows\":[],\"proofs\":[{}]}]}"
                .to_vec(),
            "ships proofs",
        ),
        (
            "data/demo.json",
            b"{\"rights\":\"fallback\",\"runs\":[],\"live\":{\"rows\":[],\"per_block\":[]}}"
                .to_vec(),
            "per_block",
        ),
        (
            "data/demo.json",
            b"{\"rights\":\"fallback\",\"runs\":[],\"live\":{\"rows\":[],\"series\":{}}}".to_vec(),
            "series",
        ),
        (
            "data/demo.json",
            b"{\"rights\":\"fallback\",\"runs\":[],\"spread\":{}}".to_vec(),
            "spread",
        ),
    ];
    for (name, raw, why) in &planted {
        let p = fallback_problems(name, raw, &t);
        assert!(
            p.iter().any(|p| p.contains(why)),
            "{name} passed or wrong reason ({why}): {p:?}"
        );
    }
    // What a fallback build keeps passes: pools, level 2, the Treasury and SEC rounds,
    // the indexer read, a fallback demo.json with blocks (their output sums are block
    // data), and figures that only look like amounts.
    let demo = b"{\"public\":true,\"rights\":\"fallback\",\"runs\":[{\"id\":\"full\",\"input\":null,\"rows\":[],\"proofs\":[]}],\"live\":{\"rows\":[],\"blocks\":[{\"height\":970543,\"output_sats\":\"123456789012\"}]},\"label_set\":{\"count\":9850}}";
    let keep: Vec<(&str, Vec<u8>)> = vec![
        ("history/pools-week.json", read("history/pools-week.json")),
        ("level2/debt-to-penny.json", read("level2/debt-to-penny.json")),
        ("level2/sec-holdings.json", read("level2/sec-holdings.json")),
        ("level2/usgs-quakes.json", read("level2/usgs-quakes.json")),
        ("level2/ncei-tmax.json", read("level2/ncei-tmax.json")),
        ("level2/etf-holdings.json", read("level2/etf-holdings.json")),
        ("level5/track-example.json", read("level5/track-example.json")),
        ("level5/track-example-since.json", read("level5/track-example-since.json")),
        ("level5/track-example-log.json", read("level5/track-example-log.json")),
        ("level5/track-example-registry.json", read("level5/track-example-registry.json")),
        ("run/indexer-base-sepolia-2026-10-12.json", read("run/indexer-base-sepolia-2026-10-12.json")),
        ("data/demo.json", demo.to_vec()),
        ("app/immutable/chunks/c.js", js("const h=48113187,g=\"0x8421123c0290c9a41205415489f6f4da945cdceedc2681df\",v=\"1.00000000\"".into())),
        // Rule 4 passes what a fallback page says next to a company name: block heights,
        // HTTP statuses, prices in micro-USDC, dates, addresses, and the Binance mining pool.
        (
            "app/immutable/nodes/3.js",
            js("t=`OKX 402 unpaid, 200 paid (2500 micro-USDC) at block 971,199 on 2026-10-14 via http://127.0.0.1:8406; Binance 403; checked at block 965,949 and BITB (issuer list); OKX v1.2`".into()),
        ),
        (
            "history/x.json",
            b"{\"t\":\"Binance Pool found 12.5% of blocks, 3.125 BTC each\"}".to_vec(),
        ),
    ];
    for (name, raw) in &keep {
        assert_eq!(
            fallback_problems(name, raw, &t),
            Vec::<String>::new(),
            "{name}"
        );
    }
}
