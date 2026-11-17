//! Publisher onboarding (ADR-0066 slice 1): `apply`, `request`,
//! `verify-application` and `doctor`.
//!
//! One folder, the **site folder**, mirrors what the applicant puts on its
//! origin:
//!
//! ```text
//! site/.well-known/onyx-source-keys.json            keys-doc
//! site/.well-known/onyx-publisher-application.json  apply
//! site/sample/manifest.bin, report.bin, shards/     build, sign (version 1, one shard)
//! ```
//!
//! `verify-application` reads the same five files from a folder (`--dir`)
//! or from the origin (`--origin`: https only, no redirect followed, public
//! addresses only, a byte cap; `onyx_fetch`), and runs every check that
//! needs no person: `onyx_bulk::application`. It prints one JSON report.
//! Exit 0 passed, 1 refused, 2 usage, 3 nothing decided (a file could not
//! be reached), 4 passed what was checked but the book or the records were
//! not read (never an approval). `verify-request` does the same for a later
//! request. `doctor` sends nothing and prints one line per step.
//!
//! No command here prints a key, and none has a contact field.

use super::{hx, now_unix, parse_address, read_key, Opts, Res};
use onyx_bulk::application::{
    blocks_signer_until, strict_json, Application, Ask, Asks, Fields, KeysDocument, Network,
    Record, Refusal, RegisteredKey, Request, RequestContext, Sample, Standing, APPLICATION_PATH,
    DEFAULT_DATASETS, DEFAULT_QUOTA_GIB, LIFETIME_SECS, MAX_APPLICATION_BYTES,
    MAX_KEYS_DOCUMENT_BYTES, MAX_REPORT_BYTES, MAX_SAMPLE_BYTES, MAX_SAMPLE_MANIFEST_BYTES,
    REQUEST_PATH, SAMPLE_MAX_AGE_SECS,
};
use onyx_bulk::ids::{check_origin, check_publisher_id, look_alike, Origin, KEYS_DOCUMENT_PATH};
use onyx_bulk::Manifest;
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The User-Agent of a fetch of an applicant's files: the tool's name only.
const USER_AGENT: &str = "onyx-publish-verify/1";
/// The whole deadline of one fetch.
const FETCH_SECS: u64 = 60;

fn address_of(k: &[u8; 32]) -> Res<[u8; 20]> {
    onyx_bulk::anchor::address_of(k).map_err(|e| e.to_string())
}

/// The clock: `--now` (a rehearsal or a test) or the system's.
fn now_of(o: &Opts) -> Res<u64> {
    o.num("now", now_unix())
}

fn network(o: &Opts) -> Res<Network> {
    Ok(Network {
        chain_id: o
            .req("chain-id")?
            .parse()
            .map_err(|_| "--chain-id: not a whole number")?,
        book: parse_address(o.req("book")?)?,
    })
}

fn write_new_dirs(path: &Path, text: &str) -> Res<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The book's keys as the reviewer hands them in (`--registered FILE`).
fn registered(o: &Opts) -> Res<Option<Vec<RegisteredKey>>> {
    let Some(p) = o.opt("registered") else {
        return Ok(None);
    };
    let b = std::fs::read(p).map_err(|e| format!("--registered {p}: {e}"))?;
    RegisteredKey::list_from_json(&b)
        .map(Some)
        .map_err(|e| format!("--registered {p}: {e}"))
}

/// The decision records (`--decisions PATH`): one `decision.json`, a JSON
/// list of them, or a folder with one `<publisher id>/decision.json` each.
fn records(o: &Opts) -> Res<Option<Vec<Record>>> {
    let Some(p) = o.opt("decisions") else {
        return Ok(None);
    };
    let mut files = Vec::new();
    if Path::new(p).is_dir() {
        let dir = std::fs::read_dir(p).map_err(|e| format!("--decisions {p}: {e}"))?;
        for e in dir {
            let f = e
                .map_err(|e| format!("--decisions {p}: {e}"))?
                .path()
                .join("decision.json");
            if f.is_file() {
                files.push(f);
            }
        }
        files.sort();
    } else {
        files.push(PathBuf::from(p));
    }
    let mut out = Vec::new();
    for f in files {
        let b = std::fs::read(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let v = strict_json(&b).map_err(|e| format!("{}: not JSON: {e}", f.display()))?;
        let list = match v {
            Value::Array(a) => a,
            one => vec![one],
        };
        for d in &list {
            if let Some(r) = Record::from_json(d).map_err(|e| format!("{}: {e}", f.display()))? {
                out.push(r);
            }
        }
    }
    Ok(Some(out))
}

// ------------------------------------------------------------------ apply

/// The application `apply` writes, and what it tells the applicant.
pub(crate) fn apply(o: &Opts) -> Res<String> {
    o.only(&[
        "key",
        "publisher",
        "origin",
        "site",
        "sample",
        "data",
        "chain-id",
        "book",
        "quota-gib",
        "datasets",
        "cosign-key",
        "reapply",
        "out",
        "now",
    ])?;
    let key = read_key(o.req("key")?)?;
    let signer = address_of(&key)?;
    let site = PathBuf::from(o.opt("site").unwrap_or("site"));
    let sample_path = o.opt("sample").unwrap_or("sample/").to_string();
    let now = now_of(o)?;
    let keys_path = site.join(KEYS_DOCUMENT_PATH);
    let keys = std::fs::read(&keys_path)
        .map_err(|e| format!("{}: {e} (run keys-doc first)", keys_path.display()))?;
    let sample_dir = site.join(&sample_path);
    let (manifest_bytes, report, shard) = read_sample(&sample_dir)?;
    let m = Manifest::from_bytes(&manifest_bytes)
        .map_err(|e| format!("{}: {e}", sample_dir.display()))?;
    let small = |k: &str, d: u32| -> Res<u32> {
        u32::try_from(o.num(k, u64::from(d))?).map_err(|_| format!("--{k} is too large"))
    };
    let fields = Fields {
        version: onyx_bulk::application::VERSION,
        chain_id: network(o)?.chain_id,
        book: network(o)?.book,
        publisher: o.req("publisher")?.to_string(),
        origin: o.req("origin")?.to_string(),
        signer,
        keys_document_keccak: keccak256(&keys),
        data: o.req("data")?.to_string(),
        sample: Sample {
            dataset: m.header.dataset.clone(),
            manifest_root: m.header.root(),
            bytes: m.header.byte_count,
            path: sample_path,
        },
        asks: Asks {
            quota_gib: small("quota-gib", DEFAULT_QUOTA_GIB)?,
            datasets: small("datasets", DEFAULT_DATASETS)?,
        },
        created_unix: now,
        expires_unix: now + LIFETIME_SECS,
    };
    let mut app = Application::sign(fields, &key).map_err(|e| e.to_string())?;
    // What the reviewer's tool will check, checked here first, so a file
    // that cannot pass is never written.
    let doc = KeysDocument::parse(&keys).map_err(|e| e.to_string())?;
    if doc.source != app.fields.publisher || !doc.signers.contains(&signer) {
        return Err(format!(
            "{} does not name {} with this key's address: run keys-doc again",
            keys_path.display(),
            app.fields.publisher
        ));
    }
    let standing = match o.opt("cosign-key") {
        Some(c) => {
            let ck = read_key(c)?;
            let cosigner = address_of(&ck)?;
            if cosigner == signer {
                return Err("--cosign-key must be another key: an active key of the id".into());
            }
            if !doc.signers.contains(&cosigner) {
                return Err(
                    "the keys document must list both keys: add the new key's address to it".into(),
                );
            }
            app.cosign(&ck).map_err(|e| e.to_string())?;
            Standing::SecondKey { cosigner }
        }
        None if o.flag("reapply") => Standing::Reapply,
        None => Standing::First,
    };
    app.check_sample(&standing, &manifest_bytes, &report, &shard)
        .map_err(|e| e.to_string())?;
    if !matches!(standing, Standing::SecondKey { .. }) {
        // The keys document lists exactly this key, and the rest of what
        // the reviewer's tool checks without the book.
        app.verify(&network(o)?, now, &keys, &[], &[])
            .map_err(|e| e.to_string())?;
    }
    let text = app.to_json();
    let out = o
        .opt("out")
        .map_or_else(|| site.join(APPLICATION_PATH), PathBuf::from);
    write_new_dirs(&out, &text)?;
    let f = &app.fields;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "APPLIED publisher={} origin={} signer={} expires_unix={} keccak={}",
        f.publisher,
        f.origin,
        hx(&f.signer),
        f.expires_unix,
        keccak256(text.as_bytes())
    );
    let _ = writeln!(s, "wrote {}", out.display());
    let _ = writeln!(
        s,
        "next: copy the folder {} to https://{}/ and run: onyx-publish doctor --live",
        site.display(),
        f.origin
    );
    Ok(s)
}

/// The three files of a sample folder, each read with its cap.
fn read_sample(dir: &Path) -> Res<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let src = Source::Dir(dir.to_path_buf());
    let get = |rel: &str, cap: usize, hint: &str| {
        src.read_local(rel, cap).map_err(|e| format!("{e}{hint}"))
    };
    let manifest = get(
        "manifest.bin",
        MAX_SAMPLE_MANIFEST_BYTES,
        " (run build first)",
    )?;
    let report = get("report.bin", MAX_REPORT_BYTES, " (run sign first)")?;
    let shard = get(&shard_rel(&manifest)?, MAX_SAMPLE_BYTES as usize, "")?;
    Ok((manifest, report, shard))
}

/// The one shard's file name under a version folder.
fn shard_rel(manifest: &[u8]) -> Res<String> {
    let m = Manifest::from_bytes(manifest).map_err(|e| format!("sample: manifest: {e}"))?;
    match m.shards.as_slice() {
        [one] => Ok(format!("shards/{}.bin", one.root.to_hex())),
        s => Err(format!(
            "sample: it has {} shards: the sample is one shard of at most 8 MiB (use fewer rows)",
            s.len()
        )),
    }
}

/// `build` asks this before a version 1: the application in `site` (if
/// any) that already carries version 1 of `dataset` for `publisher`. A
/// second version 1 would have another root; once the first one's report is
/// relayed, the book suspends the key (ADR-0066 item 5).
pub(crate) fn carried_sample(site: &Path, publisher: &str, dataset: &str) -> Option<Hash32> {
    let b = Source::Dir(site.to_path_buf())
        .read_local(APPLICATION_PATH, MAX_APPLICATION_BYTES)
        .ok()?;
    let a = Application::from_json(&b).ok()?;
    (a.fields.publisher == publisher && a.fields.sample.dataset == dataset)
        .then_some(a.fields.sample.manifest_root)
}

// ------------------------------------------------------------------ request

/// The signed later request `request` writes.
pub(crate) fn request(o: &Opts) -> Res<String> {
    o.only(&[
        "key",
        "kind",
        "publisher",
        "origin",
        "chain-id",
        "book",
        "quota-gib",
        "datasets",
        "retire",
        "at",
        "site",
        "out",
        "now",
        "sequence",
    ])?;
    let key = read_key(o.req("key")?)?;
    let now = now_of(o)?;
    let site = PathBuf::from(o.opt("site").unwrap_or("site"));
    let small = |k: &str| -> Res<u32> {
        u32::try_from(
            o.req(k)?
                .parse::<u64>()
                .map_err(|_| format!("--{k}: not a whole number"))?,
        )
        .map_err(|_| format!("--{k} is too large"))
    };
    let at = || -> Res<u64> {
        o.req("at")?
            .parse()
            .map_err(|_| "--at: unix seconds".to_string())
    };
    let ask = match o.req("kind")? {
        // A withdraw names the one application it withdraws: the file in
        // the site folder, as it stands at the origin.
        "withdraw" => {
            let p = site.join(APPLICATION_PATH);
            let b = std::fs::read(&p)
                .map_err(|e| format!("{}: {e} (a withdraw names its application)", p.display()))?;
            Ask::Withdraw {
                application: keccak256(&b),
            }
        }
        "raise" => Ask::Raise(Asks {
            quota_gib: small("quota-gib")?,
            datasets: small("datasets")?,
        }),
        "key-stolen" => Ask::KeyStolen { since_unix: at()? },
        "retire-key" => Ask::RetireKey {
            key: match o.opt("retire") {
                Some(a) => parse_address(a)?,
                None => address_of(&key)?,
            },
            at_unix: at()?,
        },
        _ => return Err("--kind must be withdraw, raise, key-stolen or retire-key".into()),
    };
    let r = Request {
        chain_id: network(o)?.chain_id,
        book: network(o)?.book,
        publisher: o.req("publisher")?.to_string(),
        origin: o.req("origin")?.to_string(),
        signer: address_of(&key)?,
        ask,
        // The clock is a sequence that only grows; the reviewer takes none
        // at or below the last one taken from this key.
        sequence: o.num("sequence", now)?,
        created_unix: now,
        expires_unix: now + LIFETIME_SECS,
        signature: [0; 65],
    }
    .signed(&key)
    .map_err(|e| e.to_string())?;
    let text = r.to_json();
    let out = o
        .opt("out")
        .map_or_else(|| site.join(REQUEST_PATH), PathBuf::from);
    write_new_dirs(&out, &text)?;
    let mut s = format!(
        "REQUEST kind={} publisher={} signer={} sequence={} expires_unix={} keccak={}\nwrote {}\n",
        r.ask.name(),
        r.publisher,
        hx(&r.signer),
        r.sequence,
        r.expires_unix,
        keccak256(text.as_bytes()),
        out.display()
    );
    match r.ask {
        Ask::KeyStolen { .. } => s.push_str("next: take this key's address out of the keys document at the origin as well: the reviewer needs both proofs\n"),
        Ask::RetireKey { at_unix, .. } if at_unix + onyx_bulk::application::RETIRE_WINDOW_SECS < now => s.push_str("next: an earlier retirement time needs a second proof: take the key's address out of the keys document at the origin as well\n"),
        _ => {}
    }
    let _ = writeln!(
        s,
        "next: put the file on https://{}/{REQUEST_PATH} and open an issue with the origin and the keccak",
        r.origin
    );
    Ok(s)
}

// ------------------------------------------------------------------ the files

/// Why a file was not read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GetError {
    /// The file is refused: missing, a redirect, a private address, too large.
    Refused(String),
    /// Nothing decided: the origin could not be reached.
    Unreachable(String),
}

/// Where the applicant's files are read from.
pub(crate) enum Source {
    /// A folder that mirrors the origin.
    Dir(PathBuf),
    /// The origin itself, through the guarded fetch.
    Web(onyx_fetch::HttpsFetch, Origin),
}

impl Source {
    /// The guarded fetch of `origin`: https on 443, one resolution, public
    /// addresses only, no redirect followed, the sample's byte cap, one
    /// deadline (THREATS PUBON-12, SUB-1, SUB-11).
    pub(crate) fn web(origin: Origin) -> Res<Source> {
        let f = onyx_fetch::HttpsFetch::new(fetch_limits()).map_err(|e| e.to_string())?;
        Ok(Source::Web(f, origin))
    }

    fn read_local(&self, rel: &str, cap: usize) -> Result<Vec<u8>, String> {
        let Source::Dir(dir) = self else {
            return Err("not a folder".into());
        };
        let p = dir.join(rel);
        let len = std::fs::metadata(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?
            .len();
        if len > cap as u64 {
            return Err(format!("{}: over {cap} bytes", p.display()));
        }
        std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// The file at `rel` under the origin, at most `cap` bytes.
    pub(crate) async fn get(&self, rel: &str, cap: usize) -> Result<Vec<u8>, GetError> {
        match self {
            Source::Dir(_) => self.read_local(rel, cap).map_err(GetError::Refused),
            Source::Web(f, origin) => {
                use onyx_fetch::FetchError as E;
                let url = origin.url(rel);
                let b = f.get(&url).await.map_err(|e| match e {
                    // A status that clears by itself decides nothing (review
                    // APPLY-R1 M7): 429 and every 5xx.
                    E::Status(c) if c == 429 || c >= 500 => {
                        GetError::Unreachable(format!("{url}: {e}"))
                    }
                    E::Timeout(_) | E::Unreachable(_) => {
                        GetError::Unreachable(format!("{url}: {e}"))
                    }
                    e => GetError::Refused(format!("{url}: {e}")),
                })?;
                if b.len() > cap {
                    return Err(GetError::Refused(format!("{url}: over {cap} bytes")));
                }
                Ok(b)
            }
        }
    }
}

fn fetch_limits() -> onyx_fetch::Limits {
    onyx_fetch::Limits {
        max_bytes: MAX_SAMPLE_BYTES as usize,
        timeout: Duration::from_secs(FETCH_SECS),
        user_agent: USER_AGENT,
        accept: "*/*",
    }
}

// ------------------------------------------------------------------ verify-application

/// Exit code: every check that ran passed, but the book or the records
/// were not read. Never an approval.
pub(crate) const EXIT_UNREAD: u8 = 4;

/// What the reviewer hands the check besides the files.
pub(crate) struct Review {
    pub net: Network,
    pub now: u64,
    /// The origin the pointer names (`--origin`).
    pub origin: Option<String>,
    /// The file's Keccak from the pointer (`--hash`).
    pub hash: Option<Hash32>,
    pub registered: Option<Vec<RegisteredKey>>,
    /// Keccak of the registered list's bytes, echoed in the report.
    pub registered_keccak: Option<Hash32>,
    pub records: Option<Vec<Record>>,
    /// Keep the read bytes here (the CI job's artifact).
    pub save: Option<PathBuf>,
    /// The highest request sequence already taken from the signer.
    pub last_sequence: Option<u64>,
}

impl Review {
    /// The book counts as read with a list that holds at least one key: an
    /// empty list says nothing about what is free (review APPLY-R1 M3).
    fn book_read(&self) -> bool {
        self.registered.as_ref().is_some_and(|r| !r.is_empty())
    }
    fn keep(&self, rel: &str, b: &[u8]) -> Result<(), String> {
        let Some(d) = &self.save else {
            return Ok(());
        };
        let p = d.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("--save {}: {e}", parent.display()))?;
        }
        std::fs::write(&p, b).map_err(|e| format!("--save {}: {e}", p.display()))
    }
    /// What every report starts with.
    fn start(&self, tool: &str, src: &Source, not_checked: Vec<&str>) -> Run {
        Run {
            checks: Vec::new(),
            report: json!({
                "tool": tool,
                "ok": false,
                "source": match src { Source::Dir(_) => "folder", Source::Web(..) => "origin" },
                "chainId": self.net.chain_id,
                "book": hx(&self.net.book),
                "nowUnix": self.now,
                "registered": self.registered.as_ref().map(|r| json!({
                    "count": r.len(),
                    "keccak": self.registered_keccak.map(|k| k.to_string()),
                })),
                "records": self.records.as_ref().map(|r| json!({"count": r.len()})),
                "blocksSignerUntil": Value::Null,
                "notChecked": not_checked,
            }),
        }
    }
}

/// The lines only a person can answer (ADR-0066 item 8, 7 to 13).
const FOR_THE_PERSON: &[&str] = &[
    "7 neutral names: the id and the origin name no person, company, product or the network",
    "8 not someone else: nothing in the id, origin or data text reads as a known organisation's voice",
    "9 rights: the licence fits the data; for data the applicant did not make, re-read and quote the source's terms",
    "10 entities only: read the sample's rows and column names; no row is about one person",
    "11 not illegal to host: a plain look at the sample's text columns",
    "12 operator grouping: decide now whether this publisher belongs to a declared operator",
    "13 limits to grant: at most what asks says; by default 1 GiB and 16 datasets",
];

struct Run {
    checks: Vec<Value>,
    report: Value,
}

impl Run {
    fn ok(&mut self, check: &str, detail: String) {
        self.checks
            .push(json!({"check": check, "ok": true, "detail": detail}));
    }
    /// The final report and exit code for a refusal or an undecided run.
    fn end(mut self, check: &str, code: &str, reason: String, exit: u8) -> (Value, u8) {
        self.checks
            .push(json!({"check": check, "ok": false, "detail": reason}));
        self.report["ok"] = json!(false);
        self.report["decided"] = json!(exit != 3);
        self.report["refused"] = json!({"check": check, "code": code, "reason": reason});
        self.report["checks"] = json!(self.checks);
        (self.report, exit)
    }
    fn refuse(self, check: &str, e: &Refusal) -> (Value, u8) {
        self.end(check, e.code(), e.to_string(), 1)
    }
    fn missing(self, check: &str, e: GetError) -> (Value, u8) {
        match e {
            GetError::Refused(why) => self.end(check, "origin-unreachable", why, 1),
            GetError::Unreachable(why) => self.end(check, "not-decided", why, 3),
        }
    }
    /// The fetched bytes could not be kept: nothing is decided.
    fn unsaved(self, why: String) -> (Value, u8) {
        self.end("save", "not-decided", why, 3)
    }
    /// Every check that ran passed: exit 0 only when the book and the
    /// records (and whatever else `unread` names) were read.
    fn pass(mut self, unread: bool) -> (Value, u8) {
        self.report["ok"] = json!(!unread);
        self.report["decided"] = json!(!unread);
        self.report["passedWhatWasChecked"] = json!(true);
        self.report["refused"] = Value::Null;
        self.report["checks"] = json!(self.checks);
        (self.report, if unread { EXIT_UNREAD } else { 0 })
    }
}

/// `src.get`, kept in the artifact folder; on a failure the run's final
/// answer.
macro_rules! fetch {
    ($run:ident, $src:ident, $rv:ident, $check:expr, $rel:expr, $cap:expr) => {{
        let rel: &str = $rel;
        match $src.get(rel, $cap).await {
            Ok(b) => match $rv.keep(rel, &b) {
                Ok(()) => b,
                Err(e) => return $run.unsaved(e),
            },
            Err(e) => return $run.missing($check, e),
        }
    }};
}

/// Run every check of ADR-0066 item 8 that needs no person on the files of
/// `src`. Returns the JSON report and the exit code (0, 1, 3 or 4).
///
/// The report names the signer only once the signature is the signer's, and
/// `blocksSignerUntil` is a number only when the signature **and** the
/// pointer's hash passed: a refusal of a file that someone else copied and
/// broke must never block the key it names (review APPLY-R1 H1).
pub(crate) async fn verify(src: &Source, rv: &Review) -> (Value, u8) {
    let mut not_checked = Vec::new();
    if !rv.book_read() {
        not_checked.push("the book: whether the id, the origin and the signer are free, and a second key's cosigner (hand in --registered FILE with the book's keys; an empty list does not count)");
    }
    if rv.records.is_none() {
        not_checked.push("refused and expired records: whether this signer is blocked (hand in --decisions PATH)");
    }
    if rv.hash.is_none() {
        not_checked.push("the pointer's hash: whether this is the application the issue names (hand in --hash); without it no refusal blocks a signer");
    }
    not_checked.push("the rehearsal on a local chain and node (slice 3)");
    let mut run = rv.start("onyx-publish verify-application", src, not_checked);
    run.report["forThePerson"] = json!(FOR_THE_PERSON);

    // 1. The application: the file, its signature, the pointer.
    let bytes = fetch!(
        run,
        src,
        rv,
        "application-file",
        APPLICATION_PATH,
        MAX_APPLICATION_BYTES
    );
    let keccak = keccak256(&bytes);
    run.report["applicationKeccak"] = json!(keccak.to_string());
    let app = match Application::from_json(&bytes) {
        Ok(a) => a,
        Err(e) => return run.refuse("application-file", &e),
    };
    let f = &app.fields;
    if app.recovered() != Some(f.signer) {
        // Nothing of the file is repeated: it is not the named signer's.
        return run.refuse("application-file", &Refusal::Signature);
    }
    run.report["publisher"] = json!(f.publisher);
    run.report["origin"] = json!(f.origin);
    run.report["signer"] = json!(hx(&f.signer));
    run.report["data"] = json!(f.data);
    run.report["asks"] = json!({"quotaGib": f.asks.quota_gib, "datasets": f.asks.datasets});
    run.report["createdUnix"] = json!(f.created_unix);
    run.report["expiresUnix"] = json!(f.expires_unix);
    run.ok(
        "application-file",
        format!(
            "{} bytes, well formed, no unknown or repeated field, signed by its signer",
            bytes.len()
        ),
    );
    if let Some(h) = rv.hash {
        if h != keccak {
            return run.end(
                "pointer",
                "malformed",
                format!("the file hashes to {keccak}, the pointer names {h}"),
                1,
            );
        }
        // From here on a refusal may block this signer. Until the sample is
        // read, the bound is the application's own time: a first sample is
        // never built after it.
        run.report["blocksSignerUntil"] = json!(blocks_signer_until(f.created_unix));
    }
    if let Some(o) = &rv.origin {
        if *o != f.origin {
            return run.end(
                "pointer",
                "malformed",
                format!("the application is for the origin {}, not {o}", f.origin),
                1,
            );
        }
    }
    if rv.hash.is_some() || rv.origin.is_some() {
        run.ok(
            "pointer",
            "the pointer's origin and hash are the application's".into(),
        );
    }

    // 2. to 4. The network, expiry, keys document, id, origin, the book's
    // keys, the cosignature, the records.
    let keys = fetch!(
        run,
        src,
        rv,
        "keys-document",
        KEYS_DOCUMENT_PATH,
        MAX_KEYS_DOCUMENT_BYTES
    );
    let none_reg = Vec::new();
    let none_rec = Vec::new();
    let reg = rv.registered.as_ref().unwrap_or(&none_reg);
    let rec = rv.records.as_ref().unwrap_or(&none_rec);
    let standing = match app.verify(&rv.net, rv.now, &keys, reg, rec) {
        Ok(s) => s,
        Err(e) => return run.refuse("application", &e),
    };
    let (name, cosigner) = match standing {
        Standing::First => ("first", None),
        Standing::Reapply => ("reapply", None),
        Standing::SecondKey { cosigner } => ("second-key", Some(hx(&cosigner))),
    };
    run.report["standing"] = json!(name);
    run.report["cosigner"] = json!(cosigner);
    run.report["keysDocumentKeccak"] = json!(f.keys_document_keccak.to_string());
    run.ok(
        "application",
        format!(
            "for this chain and book; {} s left; the keys document names the id and exactly its keys; id and origin pass their rules",
            f.expires_unix.saturating_sub(rv.now)
        ),
    );

    // 5. The sample.
    let at = |rel: &str| format!("{}{rel}", f.sample.path);
    let manifest = fetch!(
        run,
        src,
        rv,
        "sample",
        &at("manifest.bin"),
        MAX_SAMPLE_MANIFEST_BYTES
    );
    let report = fetch!(run, src, rv, "sample", &at("report.bin"), MAX_REPORT_BYTES);
    let shard_name = match shard_rel(&manifest) {
        Ok(n) => at(&n),
        Err(e) => return run.end("sample", "sample-failed", e, 1),
    };
    let shard = fetch!(
        run,
        src,
        rv,
        "sample",
        &shard_name,
        MAX_SAMPLE_BYTES as usize
    );
    let facts = match app.check_sample(&standing, &manifest, &report, &shard) {
        Ok(s) => s,
        Err(e) => return run.refuse("sample", &e),
    };
    if rv.hash.is_some() {
        run.report["blocksSignerUntil"] = json!(blocks_signer_until(facts.created_unix));
    }
    run.report["sample"] = json!({
        "dataset": f.sample.dataset,
        "manifestRoot": f.sample.manifest_root.to_string(),
        "rows": facts.rows,
        "bytes": facts.bytes,
        "columns": facts.columns,
        "createdUnix": facts.created_unix,
        "relayableUntilUnix": facts.stale_unix,
        "relayable": rv.now < facts.stale_unix,
        "reportSigner": hx(&facts.report_signer),
        "rights": {
            "redistribution": "Allowed",
            "license": facts.license,
            "attribution": facts.attribution,
            "statement": onyx_bulk::RIGHTS_STATEMENT,
        },
    });
    run.ok(
        "sample",
        format!(
            "version 1, one shard, {} rows, {} bytes; it fits its schema and hashes to the manifest root; rights Allowed; the report is the signer's",
            facts.rows, facts.bytes
        ),
    );
    run.pass(!rv.book_read() || rv.records.is_none())
}

/// Check the later request at `src` (ADR-0066 item 12): the file at
/// `.well-known/onyx-publisher-request.json`, the keys document, and for a
/// withdraw the application it names. Returns the JSON report and the exit
/// code (0, 1, 3 or 4).
pub(crate) async fn verify_request(src: &Source, rv: &Review) -> (Value, u8) {
    let mut not_checked = Vec::new();
    if rv.last_sequence.is_none() {
        not_checked.push("whether this request was taken before (hand in --last-sequence N: the highest sequence taken from this key, 0 for none)");
    }
    if rv.hash.is_none() {
        not_checked.push(
            "the pointer's hash: whether this is the request the issue names (hand in --hash)",
        );
    }
    let mut run = rv.start("onyx-publish verify-request", src, Vec::new());
    let bytes = fetch!(
        run,
        src,
        rv,
        "request-file",
        REQUEST_PATH,
        MAX_APPLICATION_BYTES
    );
    let keccak = keccak256(&bytes);
    run.report["requestKeccak"] = json!(keccak.to_string());
    let r = match Request::from_json(&bytes) {
        Ok(r) => r,
        Err(e) => return run.refuse("request-file", &e),
    };
    let withdraw = matches!(r.ask, Ask::Withdraw { .. });
    if !withdraw && !rv.book_read() {
        not_checked.push("the book: whether the signer is a key of this id and may ask this (hand in --registered FILE)");
    }
    run.report["notChecked"] = json!(not_checked);
    if let Some(h) = rv.hash {
        if h != keccak {
            return run.end(
                "pointer",
                "malformed",
                format!("the file hashes to {keccak}, the pointer names {h}"),
                1,
            );
        }
    }
    if rv.origin.as_ref().is_some_and(|o| *o != r.origin) {
        return run.end(
            "pointer",
            "malformed",
            format!("the request is for the origin {}", r.origin),
            1,
        );
    }
    let keys = fetch!(
        run,
        src,
        rv,
        "keys-document",
        KEYS_DOCUMENT_PATH,
        MAX_KEYS_DOCUMENT_BYTES
    );
    let application = if withdraw {
        Some(fetch!(
            run,
            src,
            rv,
            "application-file",
            APPLICATION_PATH,
            MAX_APPLICATION_BYTES
        ))
    } else {
        None
    };
    let none_reg = Vec::new();
    let ctx = RequestContext {
        keys_document: &keys,
        registered: rv.registered.as_ref().unwrap_or(&none_reg),
        application: application.as_deref(),
        last_sequence: rv.last_sequence,
    };
    if let Err(e) = r.verify(&rv.net, rv.now, &ctx) {
        return run.refuse("request", &e);
    }
    // Only now: the request is its signer's.
    run.report["request"] = json!(r.ask.name());
    run.report["publisher"] = json!(r.publisher);
    run.report["origin"] = json!(r.origin);
    run.report["signer"] = json!(hx(&r.signer));
    run.report["sequence"] = json!(r.sequence);
    run.report["expiresUnix"] = json!(r.expires_unix);
    run.report["ask"] = match r.ask {
        Ask::Withdraw { application } => json!({"application": application.to_string()}),
        Ask::Raise(a) => json!({"quotaGib": a.quota_gib, "datasets": a.datasets}),
        Ask::KeyStolen { since_unix } => json!({"sinceUnix": since_unix}),
        Ask::RetireKey { key, at_unix } => json!({"key": hx(&key), "atUnix": at_unix}),
    };
    run.ok(
        "request",
        "signed by its signer for this chain and book, not expired, and the signer may ask this"
            .into(),
    );
    run.pass(rv.last_sequence.is_none() || (!withdraw && !rv.book_read()))
}

fn review(o: &Opts) -> Res<Review> {
    let hash = match o.opt("hash") {
        None => None,
        Some(h) => {
            let b = h
                .strip_prefix("0x")
                .and_then(|h| hex::decode(h).ok())
                .and_then(|b| <[u8; 32]>::try_from(b).ok())
                .ok_or("--hash must be 0x and 64 hex digits")?;
            Some(Hash32(b))
        }
    };
    Ok(Review {
        net: network(o)?,
        now: now_of(o)?,
        origin: o.opt("origin").map(str::to_string),
        hash,
        registered: registered(o)?,
        registered_keccak: o
            .opt("registered")
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| keccak256(&b)),
        records: records(o)?,
        save: o.opt("save").map(PathBuf::from),
        last_sequence: match o.opt("last-sequence") {
            None => None,
            Some(n) => Some(n.parse().map_err(|_| "--last-sequence: a whole number")?),
        },
    })
}

fn source(o: &Opts) -> Res<Source> {
    match (o.opt("dir"), o.opt("origin")) {
        (Some(d), _) => Ok(Source::Dir(PathBuf::from(d))),
        (None, Some(origin)) => Source::web(Origin::parse(origin).map_err(|e| e.to_string())?),
        (None, None) => Err("--dir FOLDER or --origin ORIGIN is required".into()),
    }
}

/// `verify-application`: the JSON report on stdout, the exit code back.
pub(crate) async fn verify_cmd(o: &Opts) -> Res<u8> {
    o.only(&[
        "dir",
        "origin",
        "hash",
        "chain-id",
        "book",
        "registered",
        "decisions",
        "save",
        "now",
    ])?;
    let rv = review(o)?;
    let (report, code) = verify(&source(o)?, &rv).await;
    println!("{report:#}");
    Ok(code)
}

/// `verify-request`: the JSON report on stdout, the exit code back.
pub(crate) async fn verify_request_cmd(o: &Opts) -> Res<u8> {
    o.only(&[
        "dir",
        "origin",
        "hash",
        "chain-id",
        "book",
        "registered",
        "last-sequence",
        "save",
        "now",
    ])?;
    let rv = review(o)?;
    let (report, code) = verify_request(&source(o)?, &rv).await;
    println!("{report:#}");
    Ok(code)
}

// ------------------------------------------------------------------ doctor

/// One doctor line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Line {
    /// `ok`, `missing`, `problem` or `skipped`.
    pub state: &'static str,
    pub step: &'static str,
    pub text: String,
    /// The next command to run.
    pub next: Option<String>,
}

fn line(
    state: &'static str,
    step: &'static str,
    text: impl Into<String>,
    next: Option<String>,
) -> Line {
    Line {
        state,
        step,
        text: text.into(),
        next,
    }
}

/// A key file's address when it is there and private; else the line to print.
fn key_line(step: &'static str, path: &Path, make: String) -> Result<[u8; 20], Line> {
    let shown = path.display().to_string();
    if !path.exists() {
        return Err(line(
            "missing",
            step,
            format!("no key file {shown}"),
            Some(make),
        ));
    }
    match read_key(&shown).and_then(|k| address_of(&k)) {
        Ok(a) => Ok(a),
        Err(e) => Err(line("problem", step, e, Some(format!("chmod 600 {shown}")))),
    }
}

/// `doctor`: every step of ADR-0066 item 2 for the folder, each `ok`,
/// `missing`, `problem` or `skipped`, with the next command. It reads
/// files; with `live` it also reads the origin. It sends nothing.
pub(crate) async fn doctor(o: &Opts, live: Option<&Source>) -> Res<Vec<Line>> {
    let dir = PathBuf::from(o.opt("dir").unwrap_or("."));
    let rel = |k: &str, d: &str| o.opt(k).map_or_else(|| dir.join(d), PathBuf::from);
    let (key_path, spare_path, site) = (
        rel("key", "publisher.key"),
        rel("spare", "spare.key"),
        rel("site", "site"),
    );
    let now = now_of(o)?;
    let reg = registered(o)?;
    let rec = records(o)?;
    let show = |p: &Path| p.display().to_string();
    let mut out = Vec::new();

    // 1. The keys.
    let signer = match key_line(
        "key",
        &key_path,
        format!("onyx-publish keygen --out {}", show(&key_path)),
    ) {
        Ok(a) => {
            out.push(line(
                "ok",
                "key",
                format!("{} (owner only), address {}", show(&key_path), hx(&a)),
                None,
            ));
            Some(a)
        }
        Err(l) => {
            out.push(l);
            None
        }
    };
    if o.flag("spare-offline") {
        out.push(line(
            "ok",
            "spare key",
            "kept offline (your word); register it soon after approval",
            None,
        ));
    } else {
        match key_line(
            "spare key",
            &spare_path,
            format!(
                "onyx-publish keygen --out {} (then move it to an offline place and run doctor with --spare-offline)",
                show(&spare_path)
            ),
        ) {
            Ok(a) if Some(a) == signer => out.push(line(
                "problem",
                "spare key",
                "the spare is the working key: a copy is not a spare",
                Some(format!("onyx-publish keygen --out {}.new", show(&spare_path))),
            )),
            Ok(_) => out.push(line("ok", "spare key", format!("{} (owner only); move it offline: lose the only key and the id is lost", show(&spare_path)), None)),
            Err(l) => {
                // The key is elsewhere (offline) and `keygen` left its
                // address next to where it was made.
                let note = PathBuf::from(format!("{}.address", show(&spare_path)));
                let there = std::fs::read_to_string(&note)
                    .ok()
                    .and_then(|t| parse_address(t.trim()).ok());
                match there {
                    Some(a) if l.state == "missing" && Some(a) != signer => out.push(line(
                        "ok",
                        "spare key",
                        format!("the key file is not here (kept offline); its address {} is noted in {}; register it soon after approval", hx(&a), show(&note)),
                        None,
                    )),
                    _ => out.push(l),
                }
            }
        }
    }

    // What is on disk.
    let folder = Source::Dir(site.clone());
    let app_bytes = folder
        .read_local(APPLICATION_PATH, MAX_APPLICATION_BYTES)
        .ok();
    let app = app_bytes.as_ref().map(|b| Application::from_json(b));
    let keys_bytes = folder
        .read_local(KEYS_DOCUMENT_PATH, MAX_KEYS_DOCUMENT_BYTES)
        .ok();
    let doc = keys_bytes
        .as_ref()
        .and_then(|b| KeysDocument::parse(b).ok());
    let from_app = |pick: fn(&Fields) -> &String| {
        app.as_ref()
            .and_then(|a| a.as_ref().ok())
            .map(|a| pick(&a.fields).clone())
    };

    // 2. The id and the origin.
    let id = o
        .opt("publisher")
        .map(str::to_string)
        .or_else(|| from_app(|f| &f.publisher))
        .or_else(|| doc.as_ref().map(|d| d.source.clone()));
    let origin = o
        .opt("origin")
        .map(str::to_string)
        .or_else(|| from_app(|f| &f.origin));
    let pairs = || {
        reg.iter()
            .flatten()
            .map(|k| (k.publisher.as_str(), k.origin.as_str()))
    };
    let second_key = match (&id, &origin) {
        (Some(i), Some(o)) => pairs().any(|(p, r)| p == i && r == o),
        _ => false,
    };
    match &id {
        None => out.push(line(
            "missing",
            "publisher id",
            "no id chosen: it describes the data and names no person, company, product or the network (made-up example: river-levels.open)",
            Some("onyx-publish doctor --publisher <publisher id> --origin <origin>".into()),
        )),
        Some(i) => {
            let r = check_publisher_id(i)
                .and_then(|()| look_alike(i, origin.as_deref().unwrap_or(""), pairs()));
            match r {
                Ok(()) => out.push(line("ok", "publisher id", format!("{i} passes the grammar, the reserved words and the look-alike rule; a person still reads it"), None)),
                Err(e) => out.push(line("problem", "publisher id", format!("{i}: {e}"), Some("choose another id, then: onyx-publish doctor --publisher <publisher id> --origin <origin>".into()))),
            }
        }
    }
    match &origin {
        None => out.push(line(
            "missing",
            "origin",
            "no origin chosen: a neutral https host you control, or a host and a path (made-up example: data.example.org/river-levels)",
            Some("onyx-publish doctor --publisher <publisher id> --origin <origin>".into()),
        )),
        Some(og) => {
            let others = pairs().filter(|(p, _)| Some(*p) != id.as_deref()).map(|(_, r)| r);
            let taken = pairs().any(|(p, r)| r == og && Some(p) != id.as_deref());
            match check_origin(og, others) {
                Ok(_) if taken => out.push(line("problem", "origin", format!("{og} belongs to another publisher: one origin, one id"), Some("choose another origin".into()))),
                Ok(_) => out.push(line("ok", "origin", format!("{og} passes the grammar, the shared-host list and the overlap rule; lose this origin and you lose the proof of your id"), None)),
                Err(e) => out.push(line("problem", "origin", format!("{og}: {e}"), Some("choose another origin".into()))),
            }
        }
    }
    let id_s = id.clone().unwrap_or_else(|| "<publisher id>".into());
    let origin_s = origin.clone().unwrap_or_else(|| "<origin>".into());

    // 3. The keys document.
    let keys_path = site.join(KEYS_DOCUMENT_PATH);
    let keys_cmd = format!(
        "onyx-publish keys-doc --key {} --publisher {id_s} --out {}",
        show(&key_path),
        show(&keys_path)
    );
    match (&keys_bytes, &doc) {
        (None, _) => out.push(line(
            "missing",
            "keys document",
            format!("no {}", show(&keys_path)),
            Some(keys_cmd),
        )),
        (Some(_), None) => out.push(line(
            "problem",
            "keys document",
            "it does not parse",
            Some(keys_cmd),
        )),
        (Some(_), Some(d)) => {
            if id.as_ref().is_some_and(|i| *i != d.source) {
                out.push(line(
                    "problem",
                    "keys document",
                    format!("it names {}, not {id_s}", d.source),
                    Some(keys_cmd),
                ));
            } else if signer.is_some_and(|s| !d.signers.contains(&s)) {
                out.push(line(
                    "problem",
                    "keys document",
                    "it does not list this key's address",
                    Some(keys_cmd),
                ));
            } else {
                out.push(line(
                    "ok",
                    "keys document",
                    format!(
                        "{} names {} and {} key(s)",
                        show(&keys_path),
                        d.source,
                        d.signers.len()
                    ),
                    None,
                ));
            }
        }
    }

    // 4. The sample.
    let sample_rel = app
        .as_ref()
        .and_then(|a| a.as_ref().ok())
        .map_or_else(|| "sample/".to_string(), |a| a.fields.sample.path.clone());
    let sample_dir = site.join(&sample_rel);
    let build_cmd = format!(
        "onyx-publish build --schema schema.json --input data.csv --dataset <dataset id> --publisher {id_s} --license <SPDX id> --out {}",
        show(&sample_dir)
    );
    let sample = Source::Dir(sample_dir.clone());
    let manifest = sample
        .read_local("manifest.bin", MAX_SAMPLE_MANIFEST_BYTES)
        .ok();
    let mut sample_ok = false;
    match manifest.as_ref().map(|b| Manifest::from_bytes(b)) {
        None => out.push(line(
            "missing",
            "sample",
            format!(
                "no first dataset in {}: a small real one, one shard of at most 8 MiB",
                show(&sample_dir)
            ),
            Some(build_cmd),
        )),
        Some(Err(e)) => out.push(line(
            "problem",
            "sample",
            format!("manifest: {e}"),
            Some(build_cmd),
        )),
        Some(Ok(m)) => {
            let h = &m.header;
            let stale = h.created_unix + onyx_bulk::application::REPORT_LIFE_SECS;
            if h.version != 1 || h.shard_count != 1 || h.byte_count > MAX_SAMPLE_BYTES {
                out.push(line("problem", "sample", format!("version {}, {} shard(s), {} bytes: the sample is version 1, one shard, at most 8 MiB (use fewer rows)", h.version, h.shard_count, h.byte_count), Some(format!("{build_cmd}  (into a new, empty folder)"))));
            } else if id.as_ref().is_some_and(|i| *i != h.publisher.0) {
                out.push(line(
                    "problem",
                    "sample",
                    format!("it is built for the publisher {}", h.publisher.0),
                    Some(build_cmd),
                ));
            } else if !sample_dir.join("report.bin").exists() {
                out.push(line(
                    "missing",
                    "sample signature",
                    "the sample is built but not signed",
                    Some(format!(
                        "onyx-publish sign --key {} --dir {}",
                        show(&key_path),
                        show(&sample_dir)
                    )),
                ));
            } else {
                sample_ok = true;
                let left = stale.saturating_sub(now);
                let text = format!(
                    "{} version 1, {} rows, {} bytes; its report can be relayed for {} more hours{}",
                    h.dataset,
                    h.row_count,
                    h.byte_count,
                    left / 3_600,
                    if left == 0 { ": it is stale; after approval anchor this same folder through your catalog 0, never build it again" } else { "" }
                );
                out.push(line("ok", "sample", text, None));
            }
        }
    }

    // 5. The application.
    let net = match (o.opt("chain-id"), o.opt("book")) {
        (Some(_), Some(_)) => Some(network(o)?),
        _ => None,
    };
    let apply_cmd = format!(
        "onyx-publish apply --key {} --publisher {id_s} --origin {origin_s} --site {} --data \"<one sentence about the data>\" --chain-id <chain id> --book <signed book address>{}",
        show(&key_path),
        show(&site),
        if second_key { " --cosign-key <an active key of the id>" } else { "" }
    );
    let mut ready = None;
    match (&app, &app_bytes) {
        (Some(Ok(a)), Some(bytes)) => {
            let net = net.unwrap_or(Network {
                chain_id: a.fields.chain_id,
                book: a.fields.book,
            });
            let src = Source::Dir(site.clone());
            // After an expiry the applicant applies again with the same
            // sample (`apply --reapply`). The doctor has no record of the
            // expiry, so it takes the applicant's word; the reviewer's tool
            // wants the record (review APPLY-R1 M8).
            let mut records = rec.clone();
            let old_sample = manifest
                .as_ref()
                .and_then(|b| Manifest::from_bytes(b).ok())
                .is_some_and(|m| {
                    m.header.created_unix + SAMPLE_MAX_AGE_SECS < a.fields.created_unix
                });
            let reapply = old_sample && a.cosignature.is_none();
            if reapply {
                records.get_or_insert_with(Vec::new).push(Record::Expired {
                    signer: a.fields.signer,
                    manifest_root: a.fields.sample.manifest_root,
                });
            }
            let rv = Review {
                net,
                now,
                origin: origin.clone(),
                hash: None,
                registered: reg.clone(),
                registered_keccak: None,
                records,
                save: None,
                last_sequence: None,
            };
            let (report, code) = verify(&src, &rv).await;
            if signer.is_some_and(|s| s != a.fields.signer) {
                out.push(line(
                    "problem",
                    "application",
                    "it is signed by another key than the key file",
                    Some(apply_cmd),
                ));
            } else if code == 0 || code == EXIT_UNREAD {
                let k = keccak256(bytes);
                out.push(line(
                    "ok",
                    "application",
                    format!(
                        "it passes every offline check{}; it expires in {} hours; keccak {k}",
                        if reapply { " as a re-apply after an expiry (the reviewer checks it against the expired record)" } else { "" },
                        a.fields.expires_unix.saturating_sub(now) / 3_600
                    ),
                    None,
                ));
                ready = Some(k);
            } else {
                let why = report["refused"]["reason"]
                    .as_str()
                    .unwrap_or("refused")
                    .to_string();
                let again = if report["refused"]["code"] == "expired" {
                    format!("{apply_cmd} --reapply")
                } else {
                    apply_cmd
                };
                out.push(line("problem", "application", why, Some(again)));
            }
        }
        (Some(Err(e)), _) => out.push(line(
            "problem",
            "application",
            e.to_string(),
            Some(apply_cmd),
        )),
        _ if sample_ok => out.push(line(
            "missing",
            "application",
            format!("no {}", show(&site.join(APPLICATION_PATH))),
            Some(apply_cmd),
        )),
        _ => out.push(line(
            "missing",
            "application",
            "not written yet: it needs the keys document and the signed sample first",
            Some(apply_cmd),
        )),
    }

    // 6. The files at the origin.
    match (live, ready) {
        (Some(web), Some(_)) => {
            let mut rels = vec![(KEYS_DOCUMENT_PATH.to_string(), MAX_KEYS_DOCUMENT_BYTES), (APPLICATION_PATH.to_string(), MAX_APPLICATION_BYTES)];
            if let Some(m) = &manifest {
                rels.push((format!("{sample_rel}manifest.bin"), MAX_SAMPLE_MANIFEST_BYTES));
                rels.push((format!("{sample_rel}report.bin"), MAX_REPORT_BYTES));
                if let Ok(s) = shard_rel(m) {
                    rels.push((format!("{sample_rel}{s}"), MAX_SAMPLE_BYTES as usize));
                }
            }
            let mut bad = None;
            for (r, cap) in &rels {
                let local = folder.read_local(r, *cap).ok();
                match web.get(r, *cap).await {
                    Ok(b) if Some(&b) == local.as_ref() => {}
                    Ok(_) => bad = Some(format!("{r} at the origin differs from the local file")),
                    Err(GetError::Refused(e) | GetError::Unreachable(e)) => bad = Some(e),
                }
                if bad.is_some() {
                    break;
                }
            }
            match bad {
                None => out.push(line("ok", "origin files", format!("all {} files at https://{origin_s}/ equal the local ones", rels.len()), None)),
                Some(e) => {
                    ready = None;
                    out.push(line("problem", "origin files", e, Some(format!("copy the folder {} to https://{origin_s}/ again (https, no redirect), then: onyx-publish doctor --live", show(&site)))));
                }
            }
        }
        (Some(_), None) => out.push(line("skipped", "origin files", "not read: the local files are not complete yet", None)),
        (None, _) => out.push(line("missing", "origin files", format!("not checked yet: copy the folder {} to https://{origin_s}/, then let doctor read it back", show(&site)), Some("onyx-publish doctor --live".into()))),
    }

    // 7. The status, the book, gas, the node.
    match o.opt("publishers") {
        Some(p) => {
            let v: Value = std::fs::read(p)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or(format!("--publishers {p}: not a JSON file"))?;
            let state = v["publishers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|e| Some(e["publisher"].as_str().unwrap_or("")) == id.as_deref())
                .map(|e| {
                    (
                        e["state"].as_str().unwrap_or("unknown").to_string(),
                        e["reason"].as_str().unwrap_or("").to_string(),
                    )
                });
            match state {
                Some((s, why)) => out.push(line(
                    if s == "refused" { "problem" } else { "ok" },
                    "status",
                    format!("{id_s}: {s} {why}"),
                    None,
                )),
                None => out.push(line(
                    "ok",
                    "status",
                    format!("{id_s} has no record yet"),
                    None,
                )),
            }
        }
        None => out.push(line(
            "skipped",
            "status",
            "the public record was not read (it starts with slice 3 of ADR-0066)",
            Some("onyx-publish doctor --publishers <publishers.json>".into()),
        )),
    }
    match &reg {
        Some(r) => out.push(line("ok", "book", format!("{} registered key(s) read from the list: {}", r.len(), if second_key { "this id and origin are registered: this is a further key and needs a cosignature" } else { "the id, the origin and the key are checked against it above" }), None)),
        None => out.push(line("skipped", "book", "the book was not read, so the look-alike, overlap and free-key checks ran without it", Some("onyx-publish doctor --registered <list of registered keys>".into()))),
    }
    out.push(line("skipped", "gas and node", "not checked by this version: no test ETH is needed before approval (the reviewer's tool anchors the sample); from your second anchor on, one anchor is about 250,000 gas of test ETH from a faucet", None));

    // 8. What to send.
    if let (Some(k), true) = (ready, live.is_some()) {
        out.push(line("ok", "pointer", format!("ready: open one issue titled \"Publisher application: {id_s}\" with two lines: {origin_s} and {k}"), None));
    }
    Ok(out)
}

/// The doctor's lines as text, and whether nothing is missing or wrong.
pub(crate) fn doctor_text(lines: &[Line]) -> (String, bool) {
    let mut s = String::new();
    for l in lines {
        let _ = writeln!(s, "{:<8} {}: {}", l.state, l.step, l.text);
        if let Some(n) = &l.next {
            let _ = writeln!(s, "         next: {n}");
        }
    }
    let count = |st: &str| lines.iter().filter(|l| l.state == st).count();
    let (missing, problems) = (count("missing"), count("problem"));
    let _ = writeln!(
        s,
        "doctor: {} ok, {missing} missing, {problems} problem(s), {} skipped; nothing was sent",
        count("ok"),
        count("skipped")
    );
    (s, missing == 0 && problems == 0)
}

/// `doctor`: the lines on stdout; exit 0 when nothing is missing or wrong, else 1.
pub(crate) async fn doctor_cmd(o: &Opts) -> Res<u8> {
    o.only(&[
        "dir",
        "key",
        "spare",
        "spare-offline",
        "site",
        "publisher",
        "origin",
        "chain-id",
        "book",
        "registered",
        "decisions",
        "publishers",
        "live",
        "now",
    ])?;
    let web = if o.flag("live") {
        // The origin comes from the option or the local application.
        let origin = match o.opt("origin") {
            Some(o) => Some(o.to_string()),
            None => {
                let site = o.opt("site").map_or_else(
                    || PathBuf::from(o.opt("dir").unwrap_or(".")).join("site"),
                    PathBuf::from,
                );
                Source::Dir(site)
                    .read_local(APPLICATION_PATH, MAX_APPLICATION_BYTES)
                    .ok()
                    .and_then(|b| Application::from_json(&b).ok())
                    .map(|a| a.fields.origin)
            }
        };
        match origin {
            Some(og) => Some(Source::web(Origin::parse(&og).map_err(|e| e.to_string())?)?),
            None => {
                return Err("--live needs --origin, or an application in the site folder".into())
            }
        }
    } else {
        None
    };
    let (text, good) = doctor_text(&doctor(o, web.as_ref()).await?);
    print!("{text}");
    Ok(u8::from(!good))
}

#[cfg(test)]
#[path = "onboard_tests.rs"]
mod tests;
