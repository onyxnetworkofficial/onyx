//! A client check of a successor answer (ADR-0059 amendment 6 item 7):
//! `onyx-check --successor <answer> --publisher P --dataset D`.
//!
//! Off chain, the answer of `GET /v1/bulk/{publisher}/{dataset}/successor`
//! is checked by the `onyx-bulk` rules: the catalog's report signs its
//! header, each links generation up to the used one is proven (a leaf and
//! its header, or an absence proof) and chosen by
//! [`choose_generation`], every row of the used links version is read as one
//! complete run pinned to its root ([`read_links_pages`]; pages after page 0
//! from the node, `rows?root=`), and the chain is followed by [`follow`] from
//! proven leaves only. The node's own `end` must be what the proofs give.
//! Then, at one finalized block, the catalog's report must read anchored, and
//! for every key the check knows (the catalog's signer, `--keys`, a key
//! scan) the book must hold no report of the next catalog (a node that
//! answered from an old catalog is caught, not believed) and no other live
//! report of this catalog's number (two are the publisher's fault). A fault
//! of a signed part is the publisher's only once the catalog reads anchored.

use crate::bulk_history::{node, publisher_fault, scan_keys, undecided, Stop, TRIES};
use crate::bulk_serve::{
    decision_block, read_anchor_at, read_digest_at, read_sequence_at, AnchorState, At, Standing,
};
use crate::roots::JsonRpc;
use crate::storage::HttpGet;
use borsh::BorshDeserialize;
use onyx_bulk::anchor::key_id;
use onyx_bulk::catalog::{sequence_for_catalog, AbsenceProof, CatalogHeader, LeafProof};
use onyx_bulk::manifest::ManifestHeader;
use onyx_bulk::successor::{
    choose_generation, follow, generation_id, read_links_pages, Chain, End, FollowError, GenFault,
    GenSkip, GenState, Listing, RunError, UsedGeneration, LINKS_PAGE_ROWS, MAX_LINK_GENERATIONS,
    MAX_SUCCESSOR_HOPS,
};
use onyx_bulk::wire::{hex0x, hex_field};
use onyx_core::{Hash32, SignedReport};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// Largest links page a reader takes from a node: 1,000 rows with a proof
/// each and a proof per shard (page 0 of one-row shards is about 2 MB).
pub const MAX_LINKS_PAGE_BYTES: usize = 8 << 20;
/// Largest successor answer a reader takes from a node: a header per
/// generation (tens of KB in hex for a widened one, at most 8) and page 0.
pub const MAX_SUCCESSOR_ANSWER: usize = 16 << 20;

/// What the caller adds to a successor check.
#[derive(Clone, Debug, Default)]
pub struct SuccessorOpts {
    /// The node's base URL (`https://host`), for the links pages after page
    /// 0 (`--node`).
    pub node: Option<String>,
    /// Signer addresses of the publisher the caller knows (`--keys`).
    pub keys: Vec<[u8; 20]>,
    /// Scan `KeyRegistered` from this block (`--scan-keys-from`).
    pub scan_from: Option<u64>,
    /// Blocks per `eth_getLogs` query of the scan.
    pub logs_range: u64,
}

/// How a successor check ended (ADR-0059 amendment 6 item 7).
#[derive(Debug)]
pub enum SuccessorOutcome {
    /// Every check held and the chain ends at a live id (exit 0).
    Live(Value),
    /// Every check held and the chain ends otherwise: removed, retracted,
    /// not listed, no links, a cycle or too long (exit 4).
    Ended(Value),
    /// Refused (exit 1): whose fault ("publisher" or "node") and why.
    Refused(&'static str, String),
    /// Nothing decided (exit 3): the chain read, a node that lags, or an
    /// answer that is not a successor answer (an old node's 404).
    Undecided(String),
    /// Checked off chain only (`--unchecked`, exit 3).
    Unchecked(Value),
}

impl SuccessorOutcome {
    /// Exit code: 0 live, 4 another end, 1 refused, 3 undecided or unchecked.
    pub fn code(&self) -> i32 {
        match self {
            SuccessorOutcome::Live(_) => 0,
            SuccessorOutcome::Ended(_) => 4,
            SuccessorOutcome::Refused(..) => 1,
            SuccessorOutcome::Undecided(_) | SuccessorOutcome::Unchecked(_) => 3,
        }
    }

    /// The JSON report.
    pub fn report(&self) -> Value {
        match self {
            SuccessorOutcome::Live(v)
            | SuccessorOutcome::Ended(v)
            | SuccessorOutcome::Unchecked(v) => v.clone(),
            SuccessorOutcome::Refused(fault, why) => json!({
                "ok": false, "decided": true, "code": "successor_refused", "fault": fault, "error": why,
            }),
            SuccessorOutcome::Undecided(why) => json!({
                "ok": false, "decided": false, "code": "successor_undecided", "error": why, "retryLater": true,
            }),
        }
    }
}

/// What a check without a key scan cannot catch.
pub const SUCCESSOR_LIMIT: &str = "a key of the publisher that did not sign this catalog and that you did not name (--keys) or scan (--scan-keys-from) is not read: a newer catalog under such a key is not caught here";

/// What a link is, in every report (THREATS CAT-SUCC-9).
pub const SUCCESSOR_NOTE: &str = "a link is the publisher's signed pointer from the removed id to the one that continues it: nothing here proves the new id holds the old rows";

/// The warning of an answer checked off chain only.
pub const SUCCESSOR_UNCHECKED: &str = "not checked on chain (--unchecked): the catalog's report is not read in the book and no newer catalog is looked for, so a node could answer from an old catalog or a key the book never accepted; run with --rpc";

/// A successor answer checked off chain, with every links row read.
#[derive(Clone, Debug)]
pub struct Found {
    /// The catalog C the answer is from.
    pub catalog: CatalogHeader,
    /// Its signed report.
    pub report: SignedReport,
    /// The key that signed it, recovered.
    pub signer: [u8; 20],
    /// The used links generation (`None`: every chain ends `no_links`).
    pub used: Option<UsedGeneration>,
    /// Why each lower generation was passed over.
    pub skipped: Vec<(u32, GenSkip)>,
    /// The chain followed from proven leaves.
    pub chain: Chain,
    /// Links pages read.
    pub pages: usize,
}

/// The answer's parts, checked, before the links pages after page 0.
struct Parsed {
    catalog: CatalogHeader,
    report: SignedReport,
    signer: [u8; 20],
    used: Option<UsedGeneration>,
    skipped: Vec<(u32, GenSkip)>,
    listings: BTreeMap<String, Listing>,
    page0: Option<Value>,
    claimed: Value,
}

/// Why the off-chain check stopped.
pub(crate) enum Off {
    /// Refused or undecided, as it stands.
    Stop(Stop),
    /// A part under the catalog's signature breaks its own rules (a links
    /// header, or rows that do not add up to it). That is the publisher's
    /// only when the catalog's report reads anchored: any key can sign a
    /// catalog naming a publisher, so before the chain read nothing says
    /// whose it is (review SUCC-B7 L2). See [`settle`].
    Signed {
        /// What breaks.
        why: String,
        /// The catalog's report.
        report: Box<SignedReport>,
        /// Its recovered signer.
        signer: [u8; 20],
        /// The catalog's number.
        number: u64,
    },
}

impl From<Stop> for Off {
    fn from(s: Stop) -> Self {
        Off::Stop(s)
    }
}

/// The verdict on `off` at block `at`: a signed part that breaks its rules
/// is the publisher's fault when its catalog's report reads anchored there,
/// and undecided otherwise (the node may have made the catalog up, or the
/// caller's chain node lags).
pub(crate) async fn settle(
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: &[u8; 20],
    at: At,
    off: Off,
) -> Stop {
    let (why, report, signer, n) = match off {
        Off::Stop(s) => return s,
        Off::Signed {
            why,
            report,
            signer,
            number,
        } => (why, report, signer, number),
    };
    match read_anchor_at(rpc, chain_id, book, &report, &signer, at).await {
        Err(e) => undecided(e),
        Ok(r) => match r.state {
            AnchorState::Anchored(_) => publisher_fault(why),
            AnchorState::Not(not) => undecided(format!(
                "{why}; catalog {n}'s report does not read anchored at block {} ({not}), so this is not known to be the publisher's: the node's answer, or your chain node lags",
                at.0
            )),
        },
    }
}

fn decode<T: BorshDeserialize>(v: &Value, what: &str) -> Result<T, Stop> {
    hex_field(v, what)
        .ok()
        .and_then(|b| T::try_from_slice(&b).ok())
        .ok_or_else(|| node(format!("the answer's {what} does not decode")))
}

/// How catalog `h` lists `id`, from proof `p` (`{"dataset", "leaf"}` or
/// `{"dataset", "absent"}`).
fn proven(h: &CatalogHeader, id: &str, p: &Value) -> Result<Listing, Stop> {
    if p["dataset"] != id {
        return Err(node(format!("a proof names {}, not {id}", p["dataset"])));
    }
    match (p.get("leaf"), p.get("absent")) {
        (Some(l), None) => {
            let lp: LeafProof = decode(l, &format!("leaf proof of {id}"))?;
            h.proves(&lp)
                .map_err(|e| node(format!("the leaf of {id}: {e}")))?;
            if lp.leaf.dataset != id {
                return Err(node(format!(
                    "the leaf proven for {id} is the leaf of {}",
                    lp.leaf.dataset
                )));
            }
            Ok(Listing::Leaf(lp.leaf))
        }
        (None, Some(a)) => {
            let ap: AbsenceProof = decode(a, &format!("absence proof of {id}"))?;
            h.proves_absent(id, &ap)
                .map_err(|e| node(format!("the absence of {id}: {e}")))?;
            Ok(Listing::Absent)
        }
        _ => Err(node(format!(
            "the proof of {id} is neither a leaf nor an absence"
        ))),
    }
}

/// Check everything of `answer` but the links pages after page 0.
fn parse(answer: &Value, publisher: &str, dataset: &str) -> Result<Parsed, Off> {
    if !answer["catalog"]["header"].is_string() || !answer["generations"].is_array() {
        return Err(undecided(
            "not a successor answer (an old node answers 404 on /successor, or another route answered): nothing decided; ask a node that serves successor links",
        )
        .into());
    }
    if answer["publisher"] != publisher || answer["dataset"] != dataset {
        return Err(node(format!(
            "the answer is about {}/{}, not the {publisher}/{dataset} asked",
            answer["publisher"], answer["dataset"]
        ))
        .into());
    }
    let catalog: CatalogHeader = decode(&answer["catalog"]["header"], "catalog header")?;
    let report: SignedReport = decode(&answer["catalog"]["report"], "catalog report")?;
    if catalog.publisher.0 != publisher {
        return Err(node("the catalog is another publisher's").into());
    }
    if answer["catalog"]["number"].as_u64() != Some(catalog.number) {
        return Err(node("the catalog number is not its header's").into());
    }
    let signer = onyx_bulk::catalog::verify(&catalog, &report).map_err(|e| {
        node(format!(
            "the catalog's report does not sign its header: {e}"
        ))
    })?;
    match answer["end"]["status"].as_str() {
        Some("pending") => {
            return Err(undecided(
                "the node has not read the links of its newest catalog yet (pending): ask again later or another node",
            ).into())
        }
        Some("forked") => {
            return Err(undecided(
                "the node marks the publisher's links forked: nothing decided until the book settles the fork",
            ).into())
        }
        _ => {}
    }
    // Each generation up to the used one, in order.
    let gens = answer["generations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if gens.len() > MAX_LINK_GENERATIONS as usize {
        return Err(node("more links generations than there are").into());
    }
    let mut states: BTreeMap<u32, GenState> = BTreeMap::new();
    for (i, g) in gens.iter().enumerate() {
        let n = i as u32 + 1;
        let id = generation_id(n).unwrap_or_default();
        if g["generation"].as_u64() != Some(u64::from(n)) {
            return Err(node(format!("generation {n} is not in its place")).into());
        }
        let st = match proven(&catalog, &id, g)? {
            Listing::Absent => GenState::Absent,
            Listing::Leaf(l) if l.is_tombstone() => GenState::Removed,
            Listing::Leaf(leaf) => {
                let header = match g.get("header") {
                    None | Some(Value::Null) => None,
                    Some(h) => Some(Box::new(decode::<ManifestHeader>(
                        h,
                        &format!("header of {id}"),
                    )?)),
                };
                GenState::Live { leaf, header }
            }
        };
        states.insert(n, st);
    }
    let mut asked = 0u32;
    let choice = choose_generation(|g| {
        asked = g;
        states.get(&g).cloned().ok_or(g)
    })
    .map_err(|e| match e {
        GenFault::Lookup(g) => Off::Stop(node(format!(
            "the answer stops before links generation {g}, and no lower one is usable"
        ))),
        GenFault::BadHeader(g, why) => Off::Signed {
            why: format!(
                "the signed catalog lists links generation {g} with a header that breaks the manifest rules: {why}"
            ),
            report: Box::new(report.clone()),
            signer,
            number: catalog.number,
        },
        other => Off::Stop(node(other.to_string())),
    })?;
    if gens.len() as u32 != asked {
        return Err(node("the answer lists links generations past the used one").into());
    }
    // Every id the chain reaches, start first.
    // A cycle names its first id again: a catalog lists one leaf per id, so
    // a second proof of it proves the same listing.
    let proofs = answer["chain"].as_array().cloned().unwrap_or_default();
    if proofs.len() > MAX_SUCCESSOR_HOPS + 1 {
        return Err(node("more chain proofs than a chain has ids").into());
    }
    let mut listings = BTreeMap::new();
    for p in &proofs {
        let id = p["dataset"]
            .as_str()
            .ok_or_else(|| node("a chain proof without its dataset"))?
            .to_string();
        let l = proven(&catalog, &id, p)?;
        listings.insert(id, l);
    }
    let page0 = match (&choice.used, &answer["links"]) {
        (Some(_), Value::Object(_)) => Some(answer["links"].clone()),
        (Some(_), _) => {
            return Err(node("the answer lacks links page 0 of the used generation").into())
        }
        (None, Value::Null) => None,
        (None, _) => return Err(node("links rows with no usable generation").into()),
    };
    Ok(Parsed {
        catalog,
        report,
        signer,
        used: choice.used,
        skipped: choice.skipped,
        listings,
        page0,
        claimed: answer["end"].clone(),
    })
}

/// GET links page `k` of version `root` of `publisher`'s `id` from `base`:
/// no page (404, 409, 410) leaves the check undecided; a busy node is tried
/// again as the history check's fetch does.
async fn fetch_page(
    get: &dyn HttpGet,
    base: &str,
    publisher: &str,
    id: &str,
    root: &Hash32,
    k: u64,
) -> Result<Value, Stop> {
    let url = format!(
        "{}/v1/bulk/{publisher}/{id}/rows?root={root}&page={k}&limit={LINKS_PAGE_ROWS}",
        base.trim_end_matches('/')
    );
    for _ in 0..TRIES {
        let (status, body) = get
            .get(&url, MAX_LINKS_PAGE_BYTES)
            .await
            .map_err(|e| undecided(format!("{}: {e}", crate::x402::url_origin(&url))))?;
        match status {
            200 => {
                return serde_json::from_slice(&body)
                    .map_err(|_| node(format!("links page {k} is not JSON")))
            }
            // An honest node answers these after a set-aside, when the
            // version's anchor no longer holds, or when `--node` is another
            // node that lags: nothing decided (review SUCC-B7 L1).
            404 | 409 | 410 => {
                return Err(undecided(format!(
                    "the node does not serve links page {k} of version {root} (status {status}): it may have set the version aside since page 0, or it is another node that lags; ask again"
                )))
            }
            429 | 503 => {
                tokio::time::sleep(Duration::from_secs(crate::bulk_history::RETRY_SECS)).await
            }
            s => {
                return Err(undecided(format!(
                    "{}: status {s}",
                    crate::x402::url_origin(&url)
                )))
            }
        }
    }
    Err(undecided("the node stayed busy"))
}

/// Read every links row from `pages` and follow the chain.
fn finish(p: Parsed, dataset: &str, pages: &[Value]) -> Result<Found, Off> {
    let links = match &p.used {
        None => None,
        Some(u) => {
            let (_, links) = read_links_pages(&u.root, pages).map_err(|e: RunError| {
                if e.node_fault() {
                    Off::Stop(node(e.to_string()))
                } else {
                    Off::Signed {
                        why: format!(
                            "the signed links version {} breaks its own header: {e}",
                            u.root
                        ),
                        report: Box::new(p.report.clone()),
                        signer: p.signer,
                        number: p.catalog.number,
                    }
                }
            })?;
            Some(links)
        }
    };
    let mut asked = BTreeSet::new();
    let chain = follow(dataset, links.as_ref(), |id| {
        asked.insert(id.to_string());
        p.listings.get(id).cloned().ok_or_else(|| id.to_string())
    })
    .map_err(|e| match e {
        FollowError::Lookup(id) => node(format!("the answer lacks the proof of {id}")),
        other => node(other.to_string()),
    })?;
    if asked.len() != p.listings.len() {
        return Err(node("the answer proves ids the chain does not reach").into());
    }
    let n = p.catalog.number;
    match &chain.end {
        End::NotRemoved { .. } => {
            return Err(node(format!(
                "catalog {n} lists {dataset} live: it has no successor"
            ))
            .into())
        }
        End::NotListed { .. } if chain.hops.is_empty() => {
            return Err(node(format!("catalog {n} does not list {dataset}")).into())
        }
        _ => {}
    }
    let (status, last) = (chain.end.status(), end_id(&chain.end, dataset));
    let hops = p.claimed["hops"].as_array().map_or(usize::MAX, Vec::len);
    if p.claimed["status"] != status || p.claimed["dataset"] != last || hops != chain.hops.len() {
        return Err(node(format!(
            "the answer's end ({} at {}, {hops} hops) is not what its proofs give ({status} at {last}, {} hops)",
            p.claimed["status"],
            p.claimed["dataset"],
            chain.hops.len()
        )).into());
    }
    Ok(Found {
        catalog: p.catalog,
        report: p.report,
        signer: p.signer,
        used: p.used,
        skipped: p.skipped,
        chain,
        pages: pages.len(),
    })
}

/// The id a chain ends at.
fn end_id<'a>(end: &'a End, start: &'a str) -> &'a str {
    match end {
        End::NotRemoved { dataset, .. }
        | End::Live { dataset, .. }
        | End::Removed { dataset }
        | End::Retracted { dataset, .. }
        | End::NotListed { dataset }
        | End::Cycle { dataset }
        | End::TooLong { dataset } => dataset,
        End::NoLinks => start,
    }
}

/// Check `answer` off chain, fetching the links pages after page 0 from
/// `base` (none: an answer whose links take more than one page is
/// undecided).
pub(crate) async fn check_off_chain(
    get: &dyn HttpGet,
    base: Option<&str>,
    answer: &Value,
    publisher: &str,
    dataset: &str,
) -> Result<Found, Off> {
    let p = parse(answer, publisher, dataset)?;
    let mut pages = Vec::new();
    if let (Some(u), Some(page0)) = (&p.used, &p.page0) {
        pages.push(page0.clone());
        let need = u.header.row_count.div_ceil(LINKS_PAGE_ROWS);
        if need > 1 {
            let base = base.ok_or_else(|| {
                undecided(format!(
                    "the links take {need} pages and the answer holds page 0: pass --node to read the others"
                ))
            })?;
            for k in 1..need {
                pages.push(fetch_page(get, base, publisher, &u.id, &u.root, k).await?);
            }
        }
    }
    finish(p, dataset, &pages)
}

/// Check `answer` (a successor answer as JSON) off chain with page 0 only,
/// for a fuzzer: `Ok` with the end's status, or why it does not hold.
pub fn check_answer_offline(
    publisher: &str,
    dataset: &str,
    answer: &Value,
) -> Result<String, String> {
    let p = parse(answer, publisher, dataset).map_err(off_text)?;
    let pages: Vec<Value> = p.page0.iter().cloned().collect();
    finish(p, dataset, &pages)
        .map(|f| f.chain.end.status().to_string())
        .map_err(off_text)
}

fn off_text(o: Off) -> String {
    match o {
        Off::Stop(Stop::Refused(f, w)) => format!("{f}: {w}"),
        Off::Stop(Stop::Undecided(w)) => w,
        Off::Signed { why, .. } => format!("signed: {why}"),
    }
}

/// The report of a checked chain, without the chain read.
pub(crate) fn found_json(f: &Found, publisher: &str, dataset: &str) -> Value {
    let end = &f.chain.end;
    let version = match end {
        End::Live { version, .. } => Some(*version),
        _ => None,
    };
    json!({
        "ok": matches!(end, End::Live { .. }),
        "decided": true,
        "publisher": publisher,
        "dataset": dataset,
        "catalog": {"number": f.catalog.number, "root": f.catalog.root().to_string()},
        "signer": hex0x(&f.signer),
        "generation": f.used.as_ref().map(|u| u.generation),
        "links": f.used.as_ref().map(|u| json!({
            "dataset": u.id,
            "version": u.header.version,
            "root": u.root.to_string(),
            "rows": u.header.row_count,
            "pages": f.pages,
        })),
        "skipped": f.skipped.iter().map(|(g, s)| json!({"generation": g, "why": match s {
            GenSkip::Absent => "not listed".to_string(),
            GenSkip::Removed => "removed".to_string(),
            GenSkip::Unusable(u) => u.to_string(),
        }})).collect::<Vec<_>>(),
        "hops": f.chain.hops.iter().map(|h| json!({
            "from": h.from, "version": h.version, "row": h.n.to_string(), "position": h.position, "to": h.to,
        })).collect::<Vec<_>>(),
        "end": {"status": end.status(), "dataset": end_id(end, dataset), "version": version},
        "note": SUCCESSOR_NOTE,
    })
}

/// Run the successor check of `answer` for `publisher`'s `dataset` on the
/// caller's chain node `rpc` (`book` on chain `chain_id`), fetching links
/// pages from `opts.node`.
#[allow(clippy::too_many_arguments)]
pub async fn check_successor(
    get: &dyn HttpGet,
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: [u8; 20],
    answer: &Value,
    publisher: &str,
    dataset: &str,
    opts: &SuccessorOpts,
) -> SuccessorOutcome {
    let f = match check_off_chain(get, opts.node.as_deref(), answer, publisher, dataset).await {
        Ok(f) => f,
        Err(Off::Stop(s)) => return stopped(s),
        // Whose fault a signed part is: only the chain says (SUCC-B7 L2).
        Err(off) => {
            return stopped(match decision_block(rpc, chain_id).await {
                Ok(at) => settle(rpc, chain_id, &book, at, off).await,
                Err(e) => undecided(e),
            })
        }
    };
    match on_chain(rpc, chain_id, book, &f, opts).await {
        Ok(chain) => {
            let mut v = found_json(&f, publisher, dataset);
            v["chainChecked"] = json!(true);
            for (k, x) in chain.as_object().into_iter().flatten() {
                v[k] = x.clone();
            }
            if matches!(f.chain.end, End::Live { .. }) {
                SuccessorOutcome::Live(v)
            } else {
                SuccessorOutcome::Ended(v)
            }
        }
        Err(s) => stopped(s),
    }
}

/// [`check_successor`] without a chain node: the off-chain part only, never
/// verified (exit 3).
pub async fn check_successor_unchecked(
    get: &dyn HttpGet,
    answer: &Value,
    publisher: &str,
    dataset: &str,
    node_url: Option<&str>,
) -> SuccessorOutcome {
    match check_off_chain(get, node_url, answer, publisher, dataset).await {
        Ok(f) => {
            let mut v = found_json(&f, publisher, dataset);
            v["ok"] = json!(false);
            v["decided"] = json!(false);
            v["chainChecked"] = json!(false);
            v["warning"] = json!(SUCCESSOR_UNCHECKED);
            SuccessorOutcome::Unchecked(v)
        }
        Err(Off::Stop(s)) => stopped(s),
        Err(Off::Signed { why, .. }) => SuccessorOutcome::Undecided(format!(
            "{why}; not checked on chain (--unchecked), so whether the publisher's key signed it is not known: run with --rpc"
        )),
    }
}

fn stopped(s: Stop) -> SuccessorOutcome {
    match s {
        Stop::Refused(f, w) => SuccessorOutcome::Refused(f, w),
        Stop::Undecided(w) => SuccessorOutcome::Undecided(w),
    }
}

/// The chain part: at one finalized block, C's report anchored, and under
/// every key the check knows no other live report of catalog `C` and none
/// of catalog `C + 1`.
async fn on_chain(
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: [u8; 20],
    f: &Found,
    opts: &SuccessorOpts,
) -> Result<Value, Stop> {
    let at = decision_block(rpc, chain_id).await.map_err(undecided)?;
    let n = f.catalog.number;
    let r = read_anchor_at(rpc, chain_id, &book, &f.report, &f.signer, at)
        .await
        .map_err(undecided)?;
    match r.state {
        AnchorState::Anchored(_) => {}
        AnchorState::Not(why) if r.withdrawn => return Err(undecided(format!(
            "catalog {n}: the book withdrew its report for good ({why}) and the node still answers from it: it has not set it aside yet; ask again later or another node"
        ))),
        AnchorState::Not(why) => return Err(undecided(format!(
            "catalog {n}: its report does not read anchored at block {} yet ({why}); your chain node may lag",
            at.0
        ))),
    }
    let publisher = &f.catalog.publisher;
    let mut keys: BTreeMap<Hash32, BTreeSet<&'static str>> = BTreeMap::new();
    keys.entry(f.report.payload.key)
        .or_default()
        .insert("catalog");
    for s in &opts.keys {
        keys.entry(key_id(publisher, s))
            .or_default()
            .insert("caller");
    }
    let mut scanned = None;
    if let Some(from) = opts.scan_from.filter(|f| *f <= at.0) {
        for (k, _) in scan_keys(rpc, &book, publisher, from, at, opts.logs_range).await? {
            keys.entry(k).or_default().insert("scan");
        }
        scanned = Some((from, at.0));
    }
    let known: Vec<Hash32> = keys.keys().copied().collect();
    one_catalog(rpc, &book, &known, n, &f.report.payload.digest(), at).await?;
    next_catalog_unheld(rpc, &book, &known, n, at).await?;
    Ok(json!({
        "checkedAt": {"number": at.0, "hash": at.1.to_string()},
        "keys": keys.iter().map(|(k, how)| json!({"key": k.to_string(), "found": how})).collect::<Vec<_>>(),
        "keysScanned": scanned.map(|(from, to)| json!({"from": from, "to": to})),
        "limit": if scanned.is_some() { Value::Null } else { json!(SUCCESSOR_LIMIT) },
    }))
}

/// No key of `keys` holds a live report of catalog `n` other than `digest`
/// (the one checked) at `at`: two live catalogs of one number are the
/// publisher's fault, as in the history check (review SUCC-B7 M2: a second
/// catalog `n` under another registered key, with other links or none).
async fn one_catalog(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    keys: &[Hash32],
    n: u64,
    digest: &Hash32,
    at: At,
) -> Result<(), Stop> {
    for k in keys {
        let d = read_sequence_at(rpc, book, k, sequence_for_catalog(n), at)
            .await
            .map_err(undecided)?;
        if d == Hash32::ZERO || d == *digest {
            continue;
        }
        match read_digest_at(rpc, book, &d, at).await.map_err(undecided)? {
            Standing::Unrecorded | Standing::Withdrawn(_) => {}
            Standing::Holds(_) => {
                return Err(publisher_fault(format!(
                    "catalog {n}: the book holds two live reports (another one, {d}, under key {k})"
                )))
            }
            Standing::Open(_, why) => {
                return Err(undecided(format!(
                    "catalog {n}: another report ({d}, key {k}) may still count: {why}"
                )))
            }
        }
    }
    Ok(())
}

/// No key of `keys` holds a live report of catalog `n + 1` at `at`: else
/// the node answered from an old catalog (undecided).
async fn next_catalog_unheld(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    keys: &[Hash32],
    n: u64,
    at: At,
) -> Result<(), Stop> {
    let next = n.saturating_add(1);
    for k in keys {
        let d = read_sequence_at(rpc, book, k, sequence_for_catalog(next), at)
            .await
            .map_err(undecided)?;
        if d == Hash32::ZERO {
            continue;
        }
        match read_digest_at(rpc, book, &d, at).await.map_err(undecided)? {
            Standing::Unrecorded | Standing::Withdrawn(_) => {}
            Standing::Holds(_) => {
                return Err(undecided(format!(
                    "the book holds catalog {next} (key {k}): the node answered from catalog {n}, an old one; ask a node that holds the newest"
                )))
            }
            Standing::Open(_, why) => {
                return Err(undecided(format!(
                    "a report of catalog {next} (key {k}) may still count: {why}"
                )))
            }
        }
    }
    Ok(())
}
