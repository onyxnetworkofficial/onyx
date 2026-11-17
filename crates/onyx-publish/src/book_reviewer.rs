//! `book-reviewer`: the signed book reviewer's tool for publisher
//! applications (ADR-0066 items 8, 9, 10 and 12; docs/ops/publisher-review.md).
//!
//! ```text
//! book-reviewer approve --application ARTIFACT --origin ORIGIN --hash 0x…
//!     --rpc URL --chain-id N --book 0x… [--from-block N] [--logs-range N]
//!     --records deploy/publishers --limits publishers.json --max-gib N
//!     --answers answers.json --node URL --reviewer-key FILE [--gas-key FILE]
//!     [--quota-gib 1] [--datasets 16] [--dry-run]
//! book-reviewer record  --application ARTIFACT --origin ORIGIN --hash 0x…
//!     --state received|refused|expired [--reason CODE] (chain and --records as above)
//! book-reviewer request --request ARTIFACT --origin ORIGIN --hash 0x…
//!     (chain, --records; raise: --limits --max-gib --quota-gib N --datasets N;
//!      retire-key, key-stolen: --reviewer-key [--no-new-key]) [--dry-run]
//! book-reviewer scan --rpc URL --chain-id N --book 0x… [--scan-cache FILE] [--scan-pause-ms N]
//! book-reviewer pause|resume --publisher ID --limits publishers.json --records deploy/publishers
//!     [--reason CODE]
//! ```
//!
//! `approve` checks the saved application again (every check of
//! `verify-application`, with the book read from the chain node), writes the
//! publisher's entry into the node's limits file, sends `registerKey`,
//! relays the applicant's signed sample report (`postReport`), and writes
//! the record. Rules:
//!
//! - **Nothing is sent on a failure.** An application that does not pass
//!   with exit 0 ends the run before the limits file is touched.
//! - **The limit comes first** (ADR-0066 item 8 step 15, THREATS PUBON-6):
//!   the node reads the limits file only at its start, so the run that
//!   writes the entry stops there (exit 6). Every run asks the node itself
//!   (`--node`, `GET /v1/bulk?listed=<id>`) whether limits per publisher
//!   are on and the entry is there, and sends only then.
//! - **Safe to run again** after a failure at any step: an entry that is
//!   there is kept, a key the book holds for this application is not
//!   registered again, a report the book recorded is not relayed again, a
//!   decision that is written is not written twice.
//! - **A dry run sends nothing and writes nothing** (`--dry-run`): it reads
//!   the chain and the files and prints what a run would do.
//! - **The key** is read from a file only its owner can read
//!   (`--reviewer-key`), is used in this process only, and is never
//!   printed, taken from the environment or from the command line.
//! - **Test networks only**: Base Sepolia (84532) or a local chain (31337);
//!   the node at `--rpc` must answer that chain id.
//!
//! Exit: 0 done, 1 refused or failed (nothing further sent), 2 usage,
//! 3 nothing decided (the chain or the files could not be read), 4 the book
//! or the records were not read, 6 the limit was written and nothing sent.

use crate::onboard::{chain_book, review, rpc_of, verify, verify_request, Review, Source};
use crate::record;
use crate::{hx, now_unix, parse_address, parse_opts, post_report_calldata, read_key, Opts, Res};
use borsh::BorshDeserialize;
use onyx_api::bulk_registry::{abi_string, key_id, key_registered_topic};
use onyx_api::bulk_serve::PublisherLimits;
use onyx_api::evm_tx::{Mined, Sender};
use onyx_api::roots::{HttpJsonRpc, JsonRpc};
use onyx_api::x402::{BoxFuture, HyperPost};
use onyx_bulk::application::{
    strict_json, Application, KeyLife, Network, APPLICATION_PATH, DEFAULT_DATASETS,
    DEFAULT_QUOTA_GIB, MAX_APPLICATION_BYTES, REPORT_LIFE_SECS,
};
use onyx_bulk::Manifest;
use onyx_core::{keccak256, Hash32, SignedReport};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage: book-reviewer <approve|record|request|pause|resume|scan> [options]\n\
see the header of crates/onyx-publish/src/book_reviewer.rs or docs/ops/publisher-review.md";

/// Exit code: the limit was written, nothing was sent.
pub(crate) const EXIT_LIMIT_WRITTEN: u8 = 6;

/// The reason codes a person may give (ADR-0066 item 9).
pub(crate) const REASONS: &[&str] = &[
    "name-not-neutral",
    "name-taken",
    "origin-unreachable",
    "origin-overlap",
    "sample-failed",
    "rights-unclear",
    "personal-data",
    "not-hostable",
    "duplicate",
    "no-capacity",
    "no-cosignature",
];

/// Where the reviewer's transactions go.
pub(crate) trait Outbox {
    /// Send `data` to the book and wait until it is mined. `what` names the
    /// call (`registerKey`, `postReport`, `retireKey`, `markCompromised`).
    fn send<'a>(&'a self, what: &'a str, data: Vec<u8>) -> BoxFuture<'a, Res<Mined>>;
}

/// What the node says of its limits per publisher (review S3-R1 M3).
pub(crate) trait NodeProbe {
    /// `(limits are on, this publisher has an entry)` of the node.
    fn listed<'a>(&'a self, publisher: &'a str) -> BoxFuture<'a, Res<(bool, bool)>>;
}

/// The node at `--node URL`: `GET /v1/bulk?listed=<id>&limit=1`.
struct HttpNode(String);

impl NodeProbe for HttpNode {
    fn listed<'a>(&'a self, publisher: &'a str) -> BoxFuture<'a, Res<(bool, bool)>> {
        Box::pin(async move {
            let base = self.0.trim_end_matches('/');
            onyx_api::x402::check_facilitator_url(base)
                .map_err(|e| e.replacen("facilitator", "--node", 1))?;
            let url = format!("{base}/v1/bulk?listed={publisher}&limit=1");
            let c = crate::client();
            let (status, v, _) = crate::send_checked(
                &c,
                hyper::Method::GET,
                &url,
                "application/json",
                hyper::body::Bytes::new(),
            )
            .await?;
            let l = &v["limits"];
            match (status, l["on"].as_bool(), l["listed"].as_bool()) {
                (200, Some(on), Some(listed)) if l["publisher"] == publisher => Ok((on, listed)),
                _ => Err(format!("--node: {url} answered {status} without `limits` (a node from before this check, or not a bulk node)")),
            }
        })
    }
}

/// The reviewer's key (and, for the relay, a gas key) at a chain node.
struct Keys {
    book: [u8; 20],
    reviewer: Sender<HttpJsonRpc<HyperPost>>,
    gas: Option<Sender<HttpJsonRpc<HyperPost>>>,
}

impl Outbox for Keys {
    fn send<'a>(&'a self, what: &'a str, data: Vec<u8>) -> BoxFuture<'a, Res<Mined>> {
        Box::pin(async move {
            // Only the reviewer may register; anyone may relay a report.
            let from = match (&self.gas, what) {
                (Some(g), "postReport") => g,
                _ => &self.reviewer,
            };
            from.send(&self.book, &data, 0, Duration::from_secs(180))
                .await
                .map_err(|e| format!("{what}: {e}"))
        })
    }
}

fn selector(sig: &str) -> Vec<u8> {
    keccak256(sig.as_bytes()).0[..4].to_vec()
}

fn word(n: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&n.to_be_bytes());
    w
}

fn address_word(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// One ABI argument: a 32-byte word or a string.
pub(crate) enum Arg<'a> {
    Word([u8; 32]),
    Text(&'a str),
}

/// Calldata of `sig` with `args` (static words and strings).
pub(crate) fn calldata(sig: &str, args: &[Arg]) -> Vec<u8> {
    let mut head = Vec::new();
    let mut tail = Vec::new();
    for a in args {
        match a {
            Arg::Word(w) => head.extend_from_slice(w),
            Arg::Text(s) => {
                head.extend_from_slice(&word((args.len() * 32 + tail.len()) as u64));
                tail.extend_from_slice(&word(s.len() as u64));
                let mut b = s.as_bytes().to_vec();
                b.resize(s.len().div_ceil(32) * 32, 0);
                tail.extend_from_slice(&b);
            }
        }
    }
    let mut d = selector(sig);
    d.extend_from_slice(&head);
    d.extend_from_slice(&tail);
    d
}

async fn call_latest(rpc: &dyn JsonRpc, book: &[u8; 20], data: &[u8]) -> Res<Vec<u8>> {
    let r = rpc
        .call(
            "eth_call",
            json!([{"to": hx(book), "data": hx(data)}, "latest"]),
        )
        .await?;
    hex::decode(r.as_str().unwrap_or_default().trim_start_matches("0x")).map_err(|e| e.to_string())
}

/// `keyState(id)` now: the life (0 none, 1 active, 2 retired, 3 compromised).
async fn key_life(rpc: &dyn JsonRpc, book: &[u8; 20], key: &Hash32) -> Res<u8> {
    let mut d = selector("keyState(bytes32)");
    d.extend_from_slice(&key.0);
    let r = call_latest(rpc, book, &d).await?;
    r.get(31)
        .copied()
        .ok_or_else(|| "the book's keyState does not decode".to_string())
}

/// The `KeyRegistered` log of `key` in blocks `from..=to` (`to`: a number or
/// `latest`): its transaction, block and reason.
async fn registration(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    key: &Hash32,
    from: u64,
    to: Option<u64>,
) -> Res<Option<(String, u64, String)>> {
    let logs = rpc
        .call(
            "eth_getLogs",
            json!([{
                "address": hx(book),
                "fromBlock": format!("0x{from:x}"),
                "toBlock": to.map_or("latest".to_string(), |n| format!("0x{n:x}")),
                "topics": [key_registered_topic().to_string(), key.to_string()],
            }]),
        )
        .await
        .map_err(|e| format!("the registration's log, from block {from}: {e}"))?;
    let Some(l) = logs.as_array().and_then(|a| a.first()) else {
        return Ok(None);
    };
    let bad = || "the registration's log does not decode".to_string();
    let data = l["data"]
        .as_str()
        .and_then(|d| hex::decode(d.strip_prefix("0x")?).ok())
        .ok_or_else(bad)?;
    let block = l["blockNumber"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(bad)?;
    Ok(Some((
        l["transactionHash"].as_str().ok_or_else(bad)?.to_string(),
        block,
        abi_string(&data, 4).ok_or_else(bad)?,
    )))
}

// ------------------------------------------------------------------ the limits file

/// The node's limits file (`ONYX_BULK_PUBLISHERS`, ADR-0066 item 10).
pub(crate) struct Limits {
    path: PathBuf,
    doc: Value,
}

impl Limits {
    pub(crate) fn read(path: &Path) -> Res<Limits> {
        let b = std::fs::read(path).map_err(|e| format!("--limits {}: {e}", path.display()))?;
        let text = String::from_utf8(b).map_err(|_| "--limits: not text".to_string())?;
        PublisherLimits::parse(&text).map_err(|e| format!("--limits {}: {e}", path.display()))?;
        let doc = strict_json(text.as_bytes()).map_err(|e| format!("--limits: {e}"))?;
        Ok(Limits {
            path: path.to_path_buf(),
            doc,
        })
    }
    pub(crate) fn entry(&self, id: &str) -> Option<&Value> {
        self.doc["publishers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|e| e["id"] == id)
    }
    /// Put the entry of `id` (replacing one that is there).
    pub(crate) fn set(&mut self, id: &str, quota_gib: u64, datasets: u64, state: &str) {
        let e = json!({"id": id, "quota_gib": quota_gib, "datasets": datasets, "state": state});
        // `read` parsed the file: `publishers` is a list.
        if let Some(list) = self.doc["publishers"].as_array_mut() {
            match list.iter_mut().find(|x| x["id"] == id) {
                Some(x) => *x = e,
                None => list.push(e),
            }
        }
    }
    /// The file as the node would read it fits `max_gib` (the node refuses
    /// to start otherwise: `PublisherLimits::fits`).
    pub(crate) fn check(&self, max_gib: u64) -> Res<()> {
        let l = PublisherLimits::parse(&self.doc.to_string())?;
        l.fits(max_gib << 30)
    }
    pub(crate) fn save(&self) -> Res<()> {
        record::write(&self.path, &self.doc)
    }
}

// ------------------------------------------------------------------ approve

/// What `approve` works with.
pub(crate) struct Approve<'a> {
    /// The chain node, for reads of the book as it is now.
    pub rpc: &'a dyn JsonRpc,
    /// Where transactions go; `None` is a dry run.
    pub out: Option<&'a dyn Outbox>,
    /// The reviewer key's address.
    pub reviewer: [u8; 20],
    /// The saved application (the CI job's artifact folder).
    pub artifact: PathBuf,
    /// The record folder.
    pub records: PathBuf,
    /// The node's limits file.
    pub limits: PathBuf,
    /// The node's total (`ONYX_BULK_MAX_GIB`).
    pub max_gib: u64,
    /// The grant asked for on the command line (`--quota-gib`, `--datasets`).
    pub grant: (Option<u64>, Option<u64>),
    /// The node that will take the publisher's data (`--node`): asked
    /// whether its limits are on and hold this publisher's entry.
    pub node: &'a dyn NodeProbe,
    /// The person's answers 7 to 13.
    pub answers: Value,
}

/// The checklist lines a person answers and the record shows. Line 12
/// (operator grouping) is the reviewer's private note: the public record
/// must not say who runs a publisher (review S3-R1 L5).
pub(crate) const ANSWER_LINES: [&str; 6] = ["7", "8", "9", "10", "11", "13"];

/// The answers file: a JSON object with exactly the keys of
/// [`ANSWER_LINES`], each `"yes"` or `"no"`: no free text is published.
/// `yes` says the line was checked and holds; any `no` is a refusal.
pub(crate) fn read_answers(path: &str) -> Res<Value> {
    let b = std::fs::read(path).map_err(|e| format!("--answers {path}: {e}"))?;
    let v = strict_json(&b).map_err(|e| format!("--answers {path}: not JSON: {e}"))?;
    let m = v.as_object().ok_or("--answers: a JSON object")?;
    if m.len() != ANSWER_LINES.len() || !ANSWER_LINES.iter().all(|k| m.contains_key(*k)) {
        return Err("--answers: exactly the keys \"7\", \"8\", \"9\", \"10\", \"11\" and \"13\" (line 12, the operator grouping, stays in your private notes)".into());
    }
    for (k, a) in m {
        match a.as_str() {
            Some("yes") => {}
            Some("no") => return Err(format!("--answers: line {k} is answered no: refuse the application (book-reviewer record --state refused --reason <code>)")),
            _ => return Err(format!("--answers: line {k} must be \"yes\" or \"no\": the record publishes no free text")),
        }
    }
    Ok(v)
}

fn say(lines: &mut Vec<String>, text: String) {
    println!("{text}");
    lines.push(text);
}

/// Approve the application saved in `a.artifact` (the module text has the
/// rules). Returns the exit code and the lines printed.
pub(crate) async fn approve(a: &Approve<'_>, rv: &Review) -> Res<(u8, Vec<String>)> {
    let mut lines = Vec::new();
    let dry = a.out.is_none();
    let tag = if dry { "DRY RUN: would" } else { "" };
    let net = rv.net;

    // The saved application: the pointer's file, signed by its signer.
    let src = Source::Dir(a.artifact.clone());
    let bytes = src
        .read_local(APPLICATION_PATH, MAX_APPLICATION_BYTES)
        .map_err(|e| format!("--application: {e}"))?;
    let keccak = keccak256(&bytes);
    if rv.hash != Some(keccak) {
        return Err(format!(
            "the saved application hashes to {keccak}, not to --hash: nothing sent"
        ));
    }
    let app = Application::from_json(&bytes).map_err(|e| format!("the application: {e}"))?;
    let f = &app.fields;
    if app.recovered() != Some(f.signer) {
        return Err("the application is not signed by its signer: nothing sent".into());
    }
    let key = key_id(&f.publisher, &f.signer);
    let reason = format!("publisher application {keccak}");

    // The record holds this very file as refused, withdrawn or expired: a
    // closed application is never approved, also not by a run taken up
    // again (review S3-R1 M1).
    if let Some((_, state)) = rv.closed.iter().find(|(k, _)| *k == keccak.to_string()) {
        say(&mut lines, format!("NOT APPROVED: the record holds this application as {state}: nothing was sent and nothing written"));
        return Ok((1, lines));
    }

    // A run repeated after `registerKey` went through: the book holds the
    // key, active, for this very application. Its checks ran before it was
    // sent. A key that was retired or marked compromised since is not taken
    // up again: nothing of it is relayed or recorded (review S3-R1 M2).
    let finalized = rv.chain.as_ref().map(|c| &c.keys);
    let mut registered = None;
    let life = key_life(a.rpc, &net.book, &key).await?;
    if life > 1 {
        return Err(format!(
            "the book shows {} of {} as {}: nothing sent, nothing recorded. Record what happened to the key by its signed request (book-reviewer request)",
            hx(&f.signer),
            f.publisher,
            if life == 2 { "retired" } else { "compromised" }
        ));
    }
    if life == 1 {
        let known = finalized
            .and_then(|k| k.keys.iter().find(|k| k.key == key))
            .map(|k| (k.tx.to_string(), k.block));
        let from = known.as_ref().map_or_else(
            || finalized.map_or(0, |k| k.at.0.saturating_add(1)),
            |(_, b)| *b,
        );
        let to = known.as_ref().map(|(_, b)| *b);
        let log = registration(a.rpc, &net.book, &key, from, to).await?;
        match log {
            Some((tx, block, why)) if why == reason => registered = Some((tx, block)),
            Some(_) => {
                return Err(format!(
                    "{} is registered for {} already, by another decision than this application: nothing sent",
                    hx(&f.signer),
                    f.publisher
                ))
            }
            None => return Err("the book holds this key but its registration's log was not found: run again once the registration is final".into()),
        }
        say(
            &mut lines,
            format!(
                "REGISTERED already for this application: {} in block {}",
                registered.as_ref().map_or("", |r| r.0.as_str()),
                registered.as_ref().map_or(0, |r| r.1)
            ),
        );
    }

    // 1. Every check again, on the saved bytes (item 8 step 14).
    let mut standing = if app.cosignature.is_some() {
        "second-key"
    } else {
        "first"
    }
    .to_string();
    let mut cosigner = json!(app.cosigner().map(|c| hx(&c)));
    if registered.is_none() {
        let (report, code) = verify(&src, rv).await;
        if code != 0 {
            say(
                &mut lines,
                format!(
                    "NOT APPROVED (exit {code}): {}: nothing was sent and nothing written",
                    report["refused"]["reason"].as_str().unwrap_or(
                        "the book or the records were not read (never approve on exit 4)"
                    )
                ),
            );
            return Ok((code, lines));
        }
        standing = report["standing"].as_str().unwrap_or("first").to_string();
        cosigner = report["cosigner"].clone();
        say(
            &mut lines,
            format!(
                "VERIFIED {} ({standing}) {} at {} signer {}",
                keccak,
                f.publisher,
                f.origin,
                hx(&f.signer)
            ),
        );
    }

    // The key that will send is the book's reviewer.
    let on_book = call_latest(a.rpc, &net.book, &selector("reviewer()")).await?;
    if on_book.len() != 32 || on_book[12..] != a.reviewer {
        return Err(format!(
            "the book's reviewer is not this key's {}: nothing sent",
            hx(&a.reviewer)
        ));
    }

    // The node itself says whether it runs with limits and holds this
    // entry: a node without a limits file gives every registered key the
    // node's one quota (review S3-R1 M3). Asked before anything is sent.
    let (on, listed) = a
        .node
        .listed(&f.publisher)
        .await
        .map_err(|e| format!("{e}: nothing sent"))?;
    if !on {
        return Err("the node runs without limits per publisher (no ONYX_BULK_PUBLISHERS): a registered key would get the node's one quota. Start the node with the limits file: nothing sent".into());
    }

    // 2. The limit, before the key (item 8 step 15).
    let mut limits = Limits::read(&a.limits)?;
    let granted = match limits.entry(&f.publisher) {
        Some(e) => {
            let got = (
                e["quota_gib"].as_u64().unwrap_or(0),
                e["datasets"].as_u64().unwrap_or(0),
            );
            say(
                &mut lines,
                format!(
                    "LIMIT there: {} has {} GiB and {} datasets in {}",
                    f.publisher,
                    got.0,
                    got.1,
                    a.limits.display()
                ),
            );
            got
        }
        None => {
            let quota = a.grant.0.unwrap_or(u64::from(DEFAULT_QUOTA_GIB));
            let datasets = a.grant.1.unwrap_or(u64::from(DEFAULT_DATASETS));
            if quota > u64::from(f.asks.quota_gib) || datasets > u64::from(f.asks.datasets) {
                return Err(format!(
                    "the grant ({quota} GiB, {datasets} datasets) is over what the application asks ({} GiB, {}): nothing sent",
                    f.asks.quota_gib, f.asks.datasets
                ));
            }
            limits.set(&f.publisher, quota, datasets, "open");
            if let Err(e) = limits.check(a.max_gib) {
                say(&mut lines, format!("NOT APPROVED: no room for {quota} GiB more on this node ({e}): refuse with the reason no-capacity; nothing was sent and nothing written"));
                return Ok((1, lines));
            }
            if dry {
                say(
                    &mut lines,
                    format!(
                        "{tag} write the limit: {} gets {quota} GiB and {datasets} datasets in {}",
                        f.publisher,
                        a.limits.display()
                    ),
                );
            } else {
                limits.save()?;
                say(
                    &mut lines,
                    format!(
                        "LIMIT WRITTEN: {} gets {quota} GiB and {datasets} datasets in {}",
                        f.publisher,
                        a.limits.display()
                    ),
                );
            }
            (quota, datasets)
        }
    };
    if !listed {
        say(&mut lines, format!(
            "{}: the node has no entry for {} yet, so nothing was sent. Put {} on the node, restart it (it reads the file at its start), then run this again",
            if dry { "DRY RUN: a run would stop here" } else { "STOPPED after the limit" },
            f.publisher,
            a.limits.display()
        ));
        if !dry {
            return Ok((EXIT_LIMIT_WRITTEN, lines));
        }
    } else {
        say(
            &mut lines,
            format!(
                "NODE: limits per publisher are on and {} has its entry",
                f.publisher
            ),
        );
    }

    // 3. registerKey.
    if registered.is_none() {
        let data = calldata(
            "registerKey(string,address,string,bytes32,string)",
            &[
                Arg::Text(&f.publisher),
                Arg::Word(address_word(&f.signer)),
                Arg::Text(&f.origin),
                Arg::Word(f.signers_document_keccak.0),
                Arg::Text(&reason),
            ],
        );
        match a.out {
            None => say(
                &mut lines,
                format!(
                    "{tag} send registerKey({}, {}, {}, {}, \"{reason}\") from the reviewer key",
                    f.publisher,
                    hx(&f.signer),
                    f.origin,
                    f.signers_document_keccak
                ),
            ),
            Some(out) => {
                let m = out.send("registerKey", data).await?;
                say(
                    &mut lines,
                    format!(
                        "REGISTERED tx={} block={} gas={}",
                        m.hash, m.block, m.gas_used
                    ),
                );
                registered = Some((m.hash, m.block));
            }
        }
    }

    // 4. The applicant's signed sample report (item 8 step 16).
    let at = |rel: &str| format!("{}{rel}", f.sample.path);
    let manifest = src
        .read_local(
            &at("manifest.bin"),
            onyx_bulk::application::MAX_SAMPLE_MANIFEST_BYTES,
        )
        .and_then(|b| Manifest::from_bytes(&b).map_err(|e| e.to_string()))
        .map_err(|e| format!("the saved sample: {e}"))?;
    let report = src
        .read_local(&at("report.bin"), onyx_bulk::application::MAX_REPORT_BYTES)
        .and_then(|b| SignedReport::try_from_slice(&b).map_err(|e| e.to_string()))
        .map_err(|e| format!("the saved sample's report: {e}"))?;
    onyx_bulk::anchor::verify(&manifest.header, &report)
        .map_err(|e| format!("the saved report does not sign the saved manifest: {e}"))?;
    let digest = report.payload.digest();
    let mut d = selector("report(bytes32)");
    d.extend_from_slice(&digest.0);
    let rec = call_latest(a.rpc, &net.book, &d).await?;
    let relay = if rec.len() >= 32 && rec[..32] != [0u8; 32] {
        say(
            &mut lines,
            format!("RELAYED already: the book holds the sample's report {digest}"),
        );
        json!({"already_recorded": true})
    } else if rv.now
        >= manifest
            .header
            .created_unix
            .saturating_add(REPORT_LIFE_SECS)
    {
        say(&mut lines, "NOT RELAYED: the sample's report is older than 7 days. The publisher keeps the same folder and anchors it through its catalog 0 (ADR-0066 item 5)".to_string());
        json!({"not_relayed": "stale"})
    } else {
        let payload = borsh::to_vec(&report.payload).map_err(|e| e.to_string())?;
        match a.out {
            None => {
                say(
                    &mut lines,
                    format!("{tag} relay the sample's report {digest} with postReport"),
                );
                Value::Null
            }
            Some(out) => {
                let m = out
                    .send(
                        "postReport",
                        post_report_calldata(&payload, &report.signature),
                    )
                    .await?;
                say(
                    &mut lines,
                    format!(
                        "RELAYED tx={} block={} gas={} digest={digest}",
                        m.hash, m.block, m.gas_used
                    ),
                );
                json!({"tx": m.hash, "block": m.block})
            }
        }
    };

    // 5. The record (item 9).
    let folder = a.records.join(&f.publisher);
    let file = folder.join(record::DECISION);
    let have = if file.exists() {
        record::read(&file)?
    } else {
        Vec::new()
    };
    if have
        .iter()
        .any(|d| d["state"] == "approved" && d["application"] == keccak.to_string())
    {
        say(
            &mut lines,
            format!("RECORDED already in {}", file.display()),
        );
        return Ok((0, lines));
    }
    let n = have
        .iter()
        .filter(|d| d["application_file"].is_string())
        .count();
    let name = if n == 0 {
        "application.json".to_string()
    } else {
        format!("application-{}.json", n + 1)
    };
    let entry = json!({
        "state": "approved",
        "date": record::date_of(rv.now),
        "application": keccak.to_string(),
        "application_file": name,
        "publisher": f.publisher,
        "origin": f.origin,
        "signer": hx(&f.signer),
        "signers_document_keccak": f.signers_document_keccak.to_string(),
        "standing": standing,
        "cosigner": cosigner,
        "chain_id": net.chain_id,
        "book": hx(&net.book),
        "answers": a.answers,
        "limits": {"quota_gib": granted.0, "datasets": granted.1},
        "register": registered.as_ref().map(|(tx, block)| json!({"tx": tx, "block": block})),
        "relay": relay,
    });
    if dry {
        say(
            &mut lines,
            format!(
                "{tag} write {} and {}/{name}; nothing was sent and nothing written",
                file.display(),
                folder.display()
            ),
        );
        return Ok((0, lines));
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    record::write_bytes(&folder.join(&name), &bytes)?;
    record::append(&a.records, &f.publisher, &entry)?;
    // The application is no longer pending.
    let pending = a.records.join(record::hash_folder(&keccak));
    if pending.is_dir() {
        std::fs::remove_dir_all(&pending).map_err(|e| format!("{}: {e}", pending.display()))?;
    }
    say(
        &mut lines,
        format!("APPROVED {} : recorded in {}", f.publisher, file.display()),
    );
    Ok((0, lines))
}

// ------------------------------------------------------------------ the commands

const CHAIN_OPTS: &[&str] = &[
    "rpc",
    "chain-id",
    "book",
    "from-block",
    "logs-range",
    "scan-cache",
    "scan-pause-ms",
    "records",
    "now",
    "origin",
    "hash",
];

pub(crate) fn test_network(o: &Opts) -> Res<Network> {
    let chain_id: u64 = o
        .req("chain-id")?
        .parse()
        .map_err(|_| "--chain-id: not a whole number")?;
    if chain_id != 84_532 && chain_id != 31_337 {
        return Err(
            "test networks only: --chain-id 84532 (Base Sepolia) or 31337 (a local chain)".into(),
        );
    }
    Ok(Network {
        chain_id,
        book: parse_address(o.req("book")?)?,
    })
}

/// `--now` sets the clock of a rehearsal: on a local chain only. A real
/// run reads the machine's clock (review S3-R1 L2).
pub(crate) fn clock_rule(o: &Opts) -> Res<()> {
    if o.opt("now").is_some() && o.opt("chain-id") != Some("31337") {
        return Err("--now is for a rehearsal on a local chain (--chain-id 31337) only".into());
    }
    Ok(())
}

/// The review of a saved file with the book read from the chain node and
/// the records read from `--records`. `Err(code)`: nothing decided.
pub(crate) async fn chain_review(o: &Opts, what: &str) -> Res<Result<Review, u8>> {
    test_network(o)?;
    clock_rule(o)?;
    o.req("rpc")?;
    o.req("origin")?;
    o.req("hash")?;
    let records = o.req("records")?;
    if !Path::new(records).is_dir() {
        return Err(format!(
            "--records {records}: not a folder (the record folder, deploy/publishers)"
        ));
    }
    // `review` reads the records as `--decisions`.
    let mut with = vec![("decisions".to_string(), records.to_string())];
    for k in ["chain-id", "book", "origin", "hash", "now"] {
        if let Some(v) = o.opt(k) {
            with.push((k.to_string(), v.to_string()));
        }
    }
    let args: Vec<String> = with
        .into_iter()
        .flat_map(|(k, v)| [format!("--{k}"), v])
        .collect();
    let mut rv = review(&parse_opts(&args)?)?;
    rv.sequences = Some(record::sequences(Path::new(records))?);
    match chain_book(o).await {
        Ok(chain) => Ok(Ok(rv.with_chain(chain)?)),
        Err((true, e)) => Err(e),
        Err((false, e)) => {
            println!("NOT DECIDED: {e}: nothing was sent and nothing written ({what})");
            Ok(Err(3))
        }
    }
}

/// The sending keys: `--reviewer-key` and, for the relay, `--gas-key`.
fn keys(o: &Opts, net: Network) -> Res<(Keys, [u8; 20])> {
    let url = o.req("rpc")?;
    let secret = read_key(o.req("reviewer-key")?)?;
    let reviewer = Sender::new(rpc_of(url)?, net.chain_id, secret)
        .ok_or("--reviewer-key holds no valid key")?;
    let address = reviewer.address();
    let gas = match o.opt("gas-key") {
        None => None,
        Some(p) => Some(
            Sender::new(rpc_of(url)?, net.chain_id, read_key(p)?)
                .ok_or("--gas-key holds no valid key")?,
        ),
    };
    Ok((
        Keys {
            book: net.book,
            reviewer,
            gas,
        },
        address,
    ))
}

async fn approve_cmd(o: &Opts) -> Res<u8> {
    let mut allowed = CHAIN_OPTS.to_vec();
    allowed.extend([
        "application",
        "limits",
        "max-gib",
        "answers",
        "reviewer-key",
        "gas-key",
        "quota-gib",
        "datasets",
        "node",
        "dry-run",
    ]);
    o.only(&allowed)?;
    let net = test_network(o)?;
    let artifact = PathBuf::from(o.req("application")?);
    let limits = PathBuf::from(o.req("limits")?);
    let max_gib = o
        .req("max-gib")?
        .parse()
        .map_err(|_| "--max-gib: the node's ONYX_BULK_MAX_GIB, a whole number")?;
    let answers = read_answers(o.req("answers")?)?;
    let node = HttpNode(o.req("node")?.to_string());
    let num = |k: &str| -> Res<Option<u64>> {
        o.opt(k)
            .map(|v| v.parse().map_err(|_| format!("--{k}: not a whole number")))
            .transpose()
    };
    let grant = (num("quota-gib")?, num("datasets")?);
    let (keys, reviewer) = keys(o, net)?;
    let rv = match chain_review(o, "approve").await? {
        Ok(rv) => rv,
        Err(code) => return Ok(code),
    };
    let rpc = rpc_of(o.req("rpc")?)?;
    let a = Approve {
        rpc: &rpc,
        out: if o.flag("dry-run") { None } else { Some(&keys) },
        reviewer,
        artifact,
        records: PathBuf::from(o.req("records")?),
        limits,
        max_gib,
        grant,
        node: &node,
        answers,
    };
    Ok(approve(&a, &rv).await?.0)
}

/// Add `entry` to `<records>/<folder>/decision.json` unless the folder's
/// newest entry already is this state of this application: a command run
/// again writes nothing twice (review S3-R1 L9). Returns the file and
/// whether it was written.
pub(crate) fn record_once(records: &Path, folder: &str, entry: &Value) -> Res<(PathBuf, bool)> {
    let file = records.join(folder).join(record::DECISION);
    if file.is_file() {
        if let Some(last) = record::read(&file)?.last() {
            if last["state"] == entry["state"] && last["application"] == entry["application"] {
                return Ok((file, false));
            }
        }
    }
    Ok((record::append(records, folder, entry)?, true))
}

/// `record`: write what is kept of an application that is not approved.
/// The signer and its block come from the tool's own report, never from the
/// command line (review APPLY-R1 H1).
async fn record_cmd(o: &Opts) -> Res<u8> {
    let mut allowed = CHAIN_OPTS.to_vec();
    allowed.extend(["application", "state", "reason"]);
    o.only(&allowed)?;
    let state = o.req("state")?;
    let mut rv = match chain_review(o, "record").await? {
        Ok(rv) => rv,
        Err(code) => return Ok(code),
    };
    // This command writes the record: what it already holds of this file
    // is not a reason against the file.
    rv.closed.clear();
    let (report, code) = verify(&Source::Dir(PathBuf::from(o.req("application")?)), &rv).await;
    let entry = decision_of(state, o.opt("reason"), &report, code, rv.now)?;
    let keccak = report["applicationKeccak"].as_str().unwrap_or_default();
    let folder = format!(
        "application-{}",
        keccak.get(2..18).ok_or("the report names no application")?
    );
    let (file, written) = record_once(Path::new(o.req("records")?), &folder, &entry)?;
    println!(
        "RECORDED {state} {} {}. Delete the fetched bytes of a refused application.",
        if written { "in" } else { "already in" },
        file.display()
    );
    Ok(0)
}

/// The decision entry of an application that is not approved, from the
/// tool's report of it (`code`: the report's exit code).
pub(crate) fn decision_of(
    state: &str,
    reason: Option<&str>,
    report: &Value,
    code: u8,
    now: u64,
) -> Res<Value> {
    if code == 3 {
        return Err("nothing decided (the files could not be read): nothing recorded".into());
    }
    let keccak = report["applicationKeccak"]
        .as_str()
        .ok_or("the report names no application: nothing recorded")?;
    // Only a file that is its signer's and the pointer's gets a record: a
    // copy someone broke on purpose is nobody's.
    let Some(until) = report["blocksSignerUntil"].as_u64() else {
        return Err("the signature or the pointer's hash did not pass: this file is nobody's and gets no record (close its issue with one line)".into());
    };
    let signer = report["signer"]
        .as_str()
        .ok_or("the report names no signer")?;
    let tool_code = report["refused"]["code"].as_str();
    let mut e = json!({"state": state, "date": record::date_of(now), "application": keccak});
    match state {
        "received" => e["received_unix"] = json!(now),
        "refused" => {
            let why = match (tool_code, reason) {
                (Some("expired"), _) => return Err("the application expired: record it with --state expired".into()),
                (Some(c), None) => c.to_string(),
                (Some(c), Some(r)) if c == r => c.to_string(),
                (Some(c), Some(_)) => return Err(format!("the tool refused it with the code {c}: leave --reason out")),
                (None, Some(r)) if REASONS.contains(&r) => r.to_string(),
                (None, _) => return Err(format!("the tool passed it: a refusal needs --reason, one of {}", REASONS.join(", "))),
            };
            e["reason"] = json!(why);
            e["signer"] = json!(signer);
            e["blocks_signer_until"] = json!(until);
        }
        "expired" => {
            if tool_code != Some("expired") {
                return Err("the application is not expired".into());
            }
            e["signer"] = json!(signer);
            e["sample_manifest_root"] = report["sampleManifestRoot"].clone();
        }
        _ => return Err("--state: received, refused or expired (a withdrawal is a signed request: book-reviewer request)".into()),
    }
    Ok(e)
}

/// What `request` works with.
pub(crate) struct Act<'a> {
    /// The chain node, for reads of the book as it is now.
    pub rpc: &'a dyn JsonRpc,
    /// Where transactions go; `None` is a dry run.
    pub out: Option<&'a dyn Outbox>,
    /// The reviewer key's address (`--reviewer-key`), for a retirement or a theft.
    pub reviewer: Option<[u8; 20]>,
    /// The record folder.
    pub records: PathBuf,
    /// The node's limits file and total, for a raise.
    pub limits: Option<PathBuf>,
    pub max_gib: Option<u64>,
    /// The new limit, named on the command line (`--quota-gib`, `--datasets`).
    pub grant: (Option<u64>, Option<u64>),
    /// A theft without a new key: the publisher ends without one.
    pub no_new_key: bool,
}

/// Act on a request `verify_request` passed with exit 0 (`report`) and
/// write its sequence down. Returns the exit code and the lines printed.
pub(crate) async fn act(a: &Act<'_>, rv: &Review, report: &Value) -> Res<(u8, Vec<String>)> {
    let mut lines = Vec::new();
    let dry = a.out.is_none();
    let text = |k: &str| report[k].as_str().unwrap_or_default().to_string();
    let (kind, id, signer) = (text("request"), text("publisher"), text("signer"));
    let mut e = json!({
        "date": record::date_of(rv.now),
        "request": kind,
        "request_keccak": text("requestKeccak"),
        "signer": signer,
        "sequence": report["sequence"],
    });
    let reason = format!("publisher request {}", text("requestKeccak"));
    // The id's other keys the book holds as active, at its finalized block.
    let active = |skip: &str| {
        rv.registered
            .iter()
            .flatten()
            .filter(|k| k.publisher == id && k.life == KeyLife::Active && hx(&k.signer) != skip)
            .count()
    };
    let folder = match kind.as_str() {
        "withdraw" => {
            let app = report["ask"]["application"].as_str().unwrap_or_default();
            e["state"] = json!("withdrawn");
            e["application"] = json!(app);
            format!(
                "application-{}",
                app.get(2..18).ok_or("the request names no application")?
            )
        }
        "raise" => {
            let path = a
                .limits
                .as_ref()
                .ok_or("--limits is required for a raise")?;
            let max_gib = a.max_gib.ok_or("--max-gib is required for a raise")?;
            let mut limits = Limits::read(path)?;
            let asked = (
                report["ask"]["quotaGib"].as_u64().unwrap_or(0),
                report["ask"]["datasets"].as_u64().unwrap_or(0),
            );
            // The grant is the reviewer's decision, never the ask by default:
            // an ask may be up to 1024 GiB (review S3-R1 M5).
            let (Some(quota), Some(datasets)) = a.grant else {
                return Err(format!(
                    "--quota-gib and --datasets are required for a raise: name the new limit (the request asks {} GiB and {} datasets): nothing written",
                    asked.0, asked.1
                ));
            };
            if quota > asked.0 || datasets > asked.1 {
                return Err("the grant is over what the request asks: nothing written".into());
            }
            let entry = limits
                .entry(&id)
                .ok_or(format!("{id} has no entry in the limits file"))?;
            let have = (
                entry["quota_gib"].as_u64().unwrap_or(0),
                entry["datasets"].as_u64().unwrap_or(0),
            );
            let state = entry["state"].as_str().unwrap_or("open").to_string();
            if quota < have.0 || datasets < have.1 {
                return Err(format!(
                    "a raise never lowers a limit: {id} has {} GiB and {} datasets: nothing written",
                    have.0, have.1
                ));
            }
            limits.set(&id, quota, datasets, &state);
            if let Err(why) = limits.check(max_gib) {
                say(&mut lines, format!("NOT TAKEN: no room on this node ({why}): answer no-capacity; nothing written"));
                return Ok((1, lines));
            }
            if !dry {
                limits.save()?;
            }
            e["state"] = json!(if state == "paused" {
                "paused"
            } else {
                "approved"
            });
            e["limits"] = json!({"quota_gib": quota, "datasets": datasets});
            say(&mut lines, format!("{} the limit of {id}: {quota} GiB, {datasets} datasets (restart the node with the file)", if dry { "DRY RUN: would write" } else { "LIMIT WRITTEN:" }));
            id.clone()
        }
        "retire-key" | "key-stolen" => {
            let stolen = kind == "key-stolen";
            let target = if stolen {
                signer.clone()
            } else {
                report["ask"]["retire"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            };
            let at = report["ask"][if stolen { "sinceUnix" } else { "atUnix" }]
                .as_u64()
                .ok_or("the request names no time")?;
            // The key that will send is the book's reviewer, as for an
            // approval (review S3-R1 M6).
            let reviewer = a
                .reviewer
                .ok_or("--reviewer-key is required for a retirement or a theft")?;
            let on_book = call_latest(a.rpc, &rv.net.book, &selector("reviewer()")).await?;
            if on_book.len() != 32 || on_book[12..] != reviewer {
                return Err(format!(
                    "the book's reviewer is not this key's {}: nothing sent",
                    hx(&reviewer)
                ));
            }
            let left = active(&target);
            // A theft with a move to a new key: the new key first. After
            // `markCompromised` no cosignature of the stolen key counts.
            if stolen && left == 0 && !a.no_new_key {
                say(&mut lines, format!("NOT SENT: {id} has no other active key at the book's finalized block. Register the new key first (book-reviewer approve on its cosigned application), wait until it is final, then run this again. With --no-new-key the publisher ends without a key: nothing was sent"));
                return Ok((1, lines));
            }
            let key = key_id(&id, &parse_address(&target)?);
            let (sig, done) = if stolen {
                ("markCompromised(bytes32,uint64,string)", 3)
            } else {
                ("retireKey(bytes32,uint64,string)", 2)
            };
            let life = key_life(a.rpc, &rv.net.book, &key).await?;
            let call = sig.split('(').next().unwrap_or(sig);
            e["tx"] = Value::Null;
            if life == done {
                say(
                    &mut lines,
                    format!(
                        "SENT already: the book shows {target} as {}",
                        if stolen { "compromised" } else { "retired" }
                    ),
                );
            } else if life != 1 {
                return Err(format!(
                    "the book does not show {target} as active (life {life}): nothing sent"
                ));
            } else {
                match a.out {
                    None => say(&mut lines, format!("DRY RUN: would send {call}({key}, {at}, \"{reason}\") from the reviewer key")),
                    Some(out) => {
                        let m = out.send(call, calldata(sig, &[Arg::Word(key.0), Arg::Word(word(at)), Arg::Text(&reason)])).await?;
                        say(&mut lines, format!("SENT {call} tx={} block={} gas={}", m.hash, m.block, m.gas_used));
                        e["tx"] = json!({"tx": m.hash, "block": m.block});
                    }
                }
            }
            // No active key left: retired. Else the publisher stays what
            // the record says it is (a paused one stays paused).
            let now_state = record::read(&a.records.join(&id).join(record::DECISION))
                .ok()
                .and_then(|d| {
                    d.last()
                        .and_then(|x| x["state"].as_str().map(str::to_string))
                })
                .filter(|x| x == "paused")
                .unwrap_or_else(|| "approved".into());
            e["state"] = json!(if left == 0 {
                "retired".to_string()
            } else {
                now_state
            });
            e["ended_signer"] = json!(target);
            e[if stolen { "since_unix" } else { "at_unix" }] = json!(at);
            id.clone()
        }
        other => return Err(format!("an unknown request {other}")),
    };
    if dry {
        say(&mut lines, format!("DRY RUN: would record the request (sequence {}) under {}/{folder}; nothing was sent and nothing written", report["sequence"], a.records.display()));
        return Ok((0, lines));
    }
    let file = record::append(&a.records, &folder, &e)?;
    say(
        &mut lines,
        format!(
            "TAKEN {kind} of {signer}, sequence {}: recorded in {}",
            report["sequence"],
            file.display()
        ),
    );
    Ok((0, lines))
}

/// `request`: act on a later signed request (ADR-0066 item 12) and write
/// its sequence down, so it is taken once.
async fn request_cmd(o: &Opts) -> Res<u8> {
    let mut allowed = CHAIN_OPTS.to_vec();
    allowed.extend([
        "request",
        "limits",
        "max-gib",
        "quota-gib",
        "datasets",
        "reviewer-key",
        "no-new-key",
        "dry-run",
    ]);
    o.only(&allowed)?;
    let net = test_network(o)?;
    let num = |k: &str| -> Res<Option<u64>> {
        o.opt(k)
            .map(|v| v.parse().map_err(|_| format!("--{k}: not a whole number")))
            .transpose()
    };
    let (grant, max_gib) = ((num("quota-gib")?, num("datasets")?), num("max-gib")?);
    let keys = match o.opt("reviewer-key") {
        Some(_) => Some(keys(o, net)?),
        None => None,
    };
    let rv = match chain_review(o, "request").await? {
        Ok(rv) => rv,
        Err(code) => return Ok(code),
    };
    let (report, code) = verify_request(&Source::Dir(PathBuf::from(o.req("request")?)), &rv).await;
    if code != 0 {
        println!(
            "NOT TAKEN (exit {code}): {}: nothing was sent and nothing written",
            report["refused"]["reason"]
                .as_str()
                .unwrap_or("the book or the taken sequences were not read")
        );
        return Ok(code);
    }
    let rpc = rpc_of(o.req("rpc")?)?;
    let a = Act {
        rpc: &rpc,
        out: match (&keys, o.flag("dry-run")) {
            (Some((k, _)), false) => Some(k),
            _ => None,
        },
        reviewer: keys.as_ref().map(|(_, a)| *a),
        records: PathBuf::from(o.req("records")?),
        limits: o.opt("limits").map(PathBuf::from),
        max_gib,
        grant,
        no_new_key: o.flag("no-new-key"),
    };
    if a.out.is_none()
        && !o.flag("dry-run")
        && matches!(
            report["request"].as_str(),
            Some("retire-key" | "key-stolen")
        )
    {
        return Err("--reviewer-key is required for a retirement or a theft".into());
    }
    // A withdraw and a raise send nothing: they are written unless --dry-run.
    let quiet = Quiet;
    let a = Act {
        out: if o.flag("dry-run") {
            None
        } else {
            Some(a.out.unwrap_or(&quiet))
        },
        ..a
    };
    Ok(act(&a, &rv, &report).await?.0)
}

/// The outbox of a request that sends nothing (a withdraw, a raise).
struct Quiet;

impl Outbox for Quiet {
    fn send<'a>(&'a self, what: &'a str, _: Vec<u8>) -> BoxFuture<'a, Res<Mined>> {
        Box::pin(async move { Err(format!("{what}: no reviewer key was named: nothing sent")) })
    }
}

/// `pause` and `resume`: the entry's state in the limits file and a line in
/// the record. The book is not touched (ADR-0066 item 12).
pub(crate) fn pause_cmd(o: &Opts, pause: bool) -> Res<u8> {
    o.only(&[
        "publisher",
        "limits",
        "records",
        "reason",
        "now",
        "chain-id",
    ])?;
    clock_rule(o)?;
    let id = o.req("publisher")?;
    let records = PathBuf::from(o.req("records")?);
    // Everything is checked before anything is written (review S3-R1 L1).
    if let Some(r) = o.opt("reason") {
        if !REASONS.contains(&r) {
            return Err(format!("--reason: one of {}", REASONS.join(", ")));
        }
    }
    let now = o.num("now", now_unix())?;
    if !records.join(id).join(record::DECISION).is_file() {
        return Err(format!("{id} has no record in {}", records.display()));
    }
    let mut limits = Limits::read(Path::new(o.req("limits")?))?;
    let (quota, datasets) = limits
        .entry(id)
        .map(|e| {
            (
                e["quota_gib"].as_u64().unwrap_or(0),
                e["datasets"].as_u64().unwrap_or(0),
            )
        })
        .ok_or(format!("{id} has no entry in the limits file"))?;
    limits.set(id, quota, datasets, if pause { "paused" } else { "open" });
    limits.save()?;
    let mut e = json!({
        "state": if pause { "paused" } else { "approved" },
        "date": record::date_of(now),
        "action": if pause { "pause" } else { "resume" },
    });
    if let Some(r) = o.opt("reason") {
        e["reason"] = json!(r);
    }
    let file = record::append(&records, id, &e)?;
    println!("{} {id}: the limits file and {} say so; restart the node with the file. The book is not touched.", if pause { "PAUSED" } else { "RESUMED" }, file.display());
    Ok(0)
}

/// `scan`: read the book's registered keys (and fill the scan cache).
async fn scan_cmd(o: &Opts) -> Res<u8> {
    o.only(&[
        "rpc",
        "chain-id",
        "book",
        "from-block",
        "logs-range",
        "scan-cache",
        "scan-pause-ms",
    ])?;
    test_network(o)?;
    o.req("rpc")?;
    let started = std::time::Instant::now();
    let book = match chain_book(o).await {
        Ok(Some(b)) => b,
        Ok(None) => return Err("--rpc is required".into()),
        Err((true, e)) => return Err(e),
        Err((false, e)) => {
            println!("NOT READ after {} s: {e}", started.elapsed().as_secs());
            return Ok(3);
        }
    };
    for k in &book.keys.keys {
        println!(
            "KEY {} {} {} life={} suspended={} block={}",
            k.publisher,
            k.origin,
            hx(&k.signer),
            k.life,
            k.suspended,
            k.block
        );
    }
    println!(
        "SCANNED {} key(s) from block {} to the finalized block {} in {} s",
        book.keys.keys.len(),
        book.keys.from_block,
        book.keys.at.0,
        started.elapsed().as_secs()
    );
    Ok(0)
}

/// The `book-reviewer` command: the arguments of the process in, its exit
/// code out.
pub fn reviewer_main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let opts = match parse_opts(rest) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("book-reviewer: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("book-reviewer: runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let r = match cmd.as_str() {
        "approve" => rt.block_on(approve_cmd(&opts)),
        "record" => rt.block_on(record_cmd(&opts)),
        "request" => rt.block_on(request_cmd(&opts)),
        "pause" => pause_cmd(&opts, true),
        "resume" => pause_cmd(&opts, false),
        "scan" => rt.block_on(scan_cmd(&opts)),
        other => {
            eprintln!("book-reviewer: unknown command {other}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            // A wrong option is exit 2 (the text names the option).
            eprintln!("book-reviewer {cmd}: {e}");
            ExitCode::from(
                if e.starts_with("--")
                    || e.starts_with("test networks only")
                    || e.contains("is required")
                    || e.contains("not an option")
                {
                    2
                } else {
                    1
                },
            )
        }
    }
}

#[cfg(test)]
#[path = "book_reviewer_tests.rs"]
mod tests;
