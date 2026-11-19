//! `onyx-storage`: the storage network's operator, publisher and client tool
//! (ADR-0060, docs/STORAGE-NETWORK.md, docs/ops/storage-node.md). Test
//! network only: it refuses any chain but a local anvil (31337) and Base
//! Sepolia (84532).
//!
//! ```text
//! common: --rpc URL --contract 0x… [--from-block N] [--log-span N]   (sending commands add --chain-id N --key FILE)
//!
//! onyx-storage register      --capacity-gib N --url URL    # approve the bond, register, activate; prints NODE_ID
//! onyx-storage status        [--node 0x…] [--operator 0x…] [--since-block N] [--shards yes|no]   # epoch, node, its shards now and next epoch, earnings
//! onyx-storage deal          --dir DIR --r 3 --epochs N --source-url URL   # buy storage for a manifest with its own report (DIR/report.bin)
//! onyx-storage deal          --dir DIR --catalog FILE --r 3 --epochs N --source-url URL   # for a manifest a catalog anchors (FILE and FILE.report)
//! onyx-storage deal          --root 0x… --node URL --r 3 --epochs N --source-url URL      # the manifest and its anchor as a node serves them
//! onyx-storage seed                                         # record this epoch's seed
//! onyx-storage settle        [--epoch E] [--since-block N]  # settle the misses of E (default: the two before now)
//! onyx-storage withdraw                                     # earnings to the operator
//! onyx-storage exit          --node 0x…
//! onyx-storage withdraw-bond --node 0x…
//! onyx-storage decline       --node 0x… --deal N [--shard I]   # give up a shard (or the whole deal) from the next epoch, no slash
//! onyx-storage close         --deal N                       # a deal past its end: the rest back to its payer
//! onyx-storage sweep                                        # slashes to the treasury
//! onyx-storage books         [--replay yes]                 # every flow, and the conservation check (yes: also rebuilt from the events)
//! onyx-storage epoch         [--epoch E] [--since-block N]  # who was asked for what, passed, failed, missed, paid, slashed
//! onyx-storage declines                                    # declines and slashes per node and per deal (ADR-0060 amendment 2 item 1)
//! onyx-storage read          --deal N --from K --to K [--since-block N]   # rows from any replica, checked; fails over
//! onyx-storage prove         --deal N --slot S --node 0x… --dir DIR [--since-block N]   # answer calldata from the publisher's files (test tool)
//! ```
//!
//! A deal on a manifest that a catalog anchors (ADR-0067) is opened with
//! `createDealInCatalog`: `--catalog FILE` is the publisher's catalog
//! (`onyx-publish catalog-build`) with its report in `FILE.report`
//! (`catalog-sign`); `--root` with `--node` takes the manifest, the catalog
//! header, the leaf proof and the report from that node's
//! `GET /v1/bulk/manifests/{root}` and checks them first. A catalog that
//! does not list the manifest is refused before anything is sent. So is a
//! deal the contract would refuse for what the book holds: the tool asks the
//! book first whether the anchor's report holds and, for a catalog, whether
//! the publisher's next catalog exists (`CatalogSuperseded`: a deal must use
//! the newest catalog; a node that holds the catalogs serves that proof).
//!
//! What the commands read. `status`, `epoch`, `settle`, `prove`, `seed` and
//! `books` read the contract's view functions at the head block (the epoch,
//! `dealInfo`, `nodeInfo`, `activeNodes`, `setHashAt`, `exclusionFrom`,
//! `books`, `conserved`), so their cost does not grow with the contract's
//! age. `epoch` and `settle` read `exclusionFrom` only for the shards that
//! epoch's challenge slots pick; `status --node` reads it for every shard of
//! the open deals (about 3 calls a shard) only up to 1,024 shards, or with
//! `--shards yes` (else `shards_now` and `shards_next_epoch` are null).
//! Logs are read only where no view has the answer, in calls of `--log-span`
//! blocks (default 200), with a progress line on stderr every 50 calls:
//! - `epoch`: the `Answered` and `Slashed` logs of the blocks of that epoch
//!   and the two after it (the paid and slashed amounts);
//! - a node list that is no longer the newest: `NodeSet` logs, backwards
//!   from that epoch's first block until the list is found, not below
//!   `--since-block`;
//! - `status --since-block N`: also the node's URL, from the `Registered`
//!   logs of blocks N and later (without it `url` is null);
//! - `read`: node URLs and the deal's source, from the `Registered` and
//!   `DealCreated` logs since `--since-block` (default: the deploy block);
//! - `declines` and `books --replay yes` read every log since the deploy
//!   block: they report the whole history.
//!
//! Keys are read from 0600 files and never printed. Exit 0 done, 1 refused
//! or failed, 2 usage.

use onyx_api::evm_tx::Sender;
use onyx_api::roots::{HttpJsonRpc, JsonRpc};
use onyx_api::storage::{
    anchor_in_catalog, deal_call, deal_fits_rules, epoch_report, fetch_manifest, read_rows_in,
    settle_epoch, status_reads_shards, Epochs, ExclScope, HyperGet, ManifestAnchor, Snapshot,
    StorageChain, TxSend, ViewOpts, STATUS_SHARDS,
};
use onyx_api::x402::HyperPost;
use onyx_bulk::manifest::Manifest;
use onyx_core::Hash32;
use onyx_storage::abi;
use onyx_storage::NodeId;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

type Res<T> = Result<T, String>;

const USAGE: &str = "usage: onyx-storage <register|status|deal|seed|settle|withdraw|exit|withdraw-bond|decline|close|sweep|books|epoch|declines|read|prove> --rpc URL --contract 0x… [options]\n\
see the header of crates/onyx-node/src/bin/onyx-storage.rs or docs/ops/storage-node.md";

struct Opts(BTreeMap<String, String>);

impl Opts {
    fn parse(a: &[String]) -> Res<Opts> {
        let mut m = BTreeMap::new();
        let mut it = a.iter();
        while let Some(k) = it.next() {
            let name = k
                .strip_prefix("--")
                .ok_or_else(|| format!("unexpected {k}"))?;
            let v = it.next().ok_or_else(|| format!("{k} needs a value"))?;
            if m.insert(name.to_string(), v.clone()).is_some() {
                return Err(format!("{k} given twice"));
            }
        }
        Ok(Opts(m))
    }
    fn req(&self, k: &str) -> Res<&str> {
        self.0
            .get(k)
            .map(String::as_str)
            .ok_or_else(|| format!("--{k} is required"))
    }
    fn opt(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }
    fn only(&self, allowed: &[&str]) -> Res<()> {
        let common = [
            "rpc",
            "contract",
            "from-block",
            "log-span",
            "chain-id",
            "key",
        ];
        match self
            .0
            .keys()
            .find(|k| !allowed.contains(&k.as_str()) && !common.contains(&k.as_str()))
        {
            Some(k) => Err(format!("--{k} is not an option of this command")),
            None => Ok(()),
        }
    }
    fn num<T: std::str::FromStr>(&self, k: &str) -> Res<T> {
        self.req(k)?
            .parse()
            .map_err(|_| format!("--{k} must be a number"))
    }
}

fn hexn(s: &str, n: usize, what: &str) -> Res<Vec<u8>> {
    s.trim()
        .strip_prefix("0x")
        .and_then(|h| hex::decode(h).ok())
        .filter(|b| b.len() == n)
        .ok_or_else(|| format!("{what} must be 0x and {} hex digits", 2 * n))
}

fn addr(s: &str, what: &str) -> Res<[u8; 20]> {
    hexn(s, 20, what)?.try_into().map_err(|_| what.to_string())
}

fn node(s: &str) -> Res<NodeId> {
    let b: [u8; 32] = hexn(s, 32, "--node")?.try_into().map_err(|_| "--node")?;
    Ok(NodeId(Hash32(b)))
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

type Rpc = HttpJsonRpc<HyperPost>;

fn rpc(o: &Opts) -> Res<Rpc> {
    Ok(HttpJsonRpc::new(
        o.req("rpc")?,
        HyperPost::for_logs(Duration::from_secs(30)),
    ))
}

fn chain(o: &Opts) -> Res<StorageChain> {
    let from = o.opt("from-block").map_or(Ok(0), |v| {
        v.parse::<u64>()
            .map_err(|_| "--from-block must be a number".to_string())
    })?;
    let c = StorageChain::new(
        Arc::new(rpc(o)?),
        addr(o.req("contract")?, "--contract")?,
        from,
    )
    .with_progress(Arc::new(|line: &str| eprintln!("onyx-storage: {line}")));
    match o.opt("log-span") {
        None => Ok(c),
        Some(v) => match v.parse::<u64>() {
            Ok(n) if n >= 1 => Ok(c.with_log_span(n)),
            _ => Err("--log-span must be a number of blocks, 1 or more".into()),
        },
    }
}

/// `--since-block`: the lowest block a log scan may read.
fn since(o: &Opts) -> Res<Option<u64>> {
    o.opt("since-block")
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| "--since-block must be a number".to_string())
        })
        .transpose()
}

/// The chain view of a command, from the contract's view functions
/// (`StorageChain::view_snapshot`): right for `epochs`, with the node URLs
/// and deal sources only when `texts`, and the exclusions of `exclusions`.
async fn view(
    o: &Opts,
    c: &StorageChain,
    epochs: Epochs,
    texts: bool,
    exclusions: ExclScope,
) -> Res<Snapshot> {
    c.view_snapshot(ViewOpts {
        epochs,
        since: since(o)?,
        texts,
        exclusions,
    })
    .await
}

async fn chain_id_ok(r: &Rpc, want: u64) -> Res<()> {
    let v = r.call("eth_chainId", json!([])).await?;
    let id = u64::from_str_radix(v.as_str().unwrap_or_default().trim_start_matches("0x"), 16)
        .map_err(|e| e.to_string())?;
    if id != want {
        return Err(format!("the node is on chain {id}, not --chain-id {want}"));
    }
    if id != 31_337 && id != 84_532 {
        return Err(format!(
            "chain {id} is not a test network this tool sends on (31337, 84532)"
        ));
    }
    Ok(())
}

async fn sender(o: &Opts) -> Res<Sender<Rpc>> {
    let id: u64 = o.num("chain-id")?;
    let r = rpc(o)?;
    chain_id_ok(&r, id).await?;
    let k = onyx_node::config::read_key_file(Path::new(o.req("key")?))?;
    Sender::new(r, id, k).ok_or_else(|| "--key is not a valid secp256k1 key".to_string())
}

async fn send(tx: &Sender<Rpc>, to: &[u8; 20], data: Vec<u8>) -> Res<String> {
    TxSend::send(tx, to, data).await
}

/// The token `OnyxStorage` takes (its `token()`).
async fn token(c: &StorageChain) -> Res<[u8; 20]> {
    abi::word_addr(&c.call(&abi::call("token()", &[])).await?, 0).map_err(|e| e.to_string())
}

async fn approve(tx: &Sender<Rpc>, c: &StorageChain, amount: u128) -> Res<()> {
    let t = token(c).await?;
    send(
        tx,
        &t,
        abi::call(
            "approve(address,uint256)",
            &[abi::addr(&c.contract()), abi::uint(amount)],
        ),
    )
    .await
    .map(|_| ())
}

async fn register(o: &Opts) -> Res<()> {
    o.only(&["capacity-gib", "url"])?;
    let c = chain(o)?;
    let tx = sender(o).await?;
    let cap: u64 = o.num("capacity-gib")?;
    let url = o.req("url")?;
    let bond = abi::word_uint(
        &c.call(&abi::call("bondFor(uint64)", &[abi::uint(cap.into())]))
            .await?,
        0,
    )
    .map_err(|e| e.to_string())?;
    approve(&tx, &c, bond).await?;
    let index = abi::word_u64(&c.call(&abi::call("registrationCount()", &[])).await?, 0)
        .map_err(|e| e.to_string())?;
    send(&tx, &c.contract(), abi::register(cap, url)).await?;
    // Activation in a later block: the id comes from the registration block's
    // hash. Wait until the head is past it (a local anvil mines no block by
    // itself: there, after 2 s, mine one).
    let reg_block = c.block_number().await?;
    let id: u64 = o.num("chain-id")?;
    for i in 0..240 {
        if c.block_number().await? > reg_block {
            break;
        }
        if id == 31_337 && i == 4 {
            rpc(o)?.call("evm_mine", json!([])).await?;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if c.block_number().await? <= reg_block {
        return Err(
            "no new block after the registration within 2 minutes; run activate later".into(),
        );
    }
    send(&tx, &c.contract(), abi::activate(index)).await?;
    let me = tx.address();
    let head = c.block_number().await?;
    // The activation was mined after the registration block: only those
    // blocks are read, never every log since the deploy block.
    let id = c
        .logs_from(abi::events::ACTIVATED, reg_block, head)
        .await?
        .into_iter()
        .rev()
        .find(|l| {
            l.topics
                .get(2)
                .is_some_and(|t| t.0[24..] == index.to_be_bytes())
                && l.topics.get(3).is_some_and(|t| t.0[12..] == me)
        })
        .and_then(|l| l.topics.get(1).copied())
        .ok_or("activated, but the Activated event was not found")?;
    println!(
        "NODE_ID={id} index={index} operator={} bond={bond} capacity_gib={cap}",
        hx(&me)
    );
    Ok(())
}

async fn status(o: &Opts) -> Res<()> {
    o.only(&["node", "operator", "since-block", "shards"])?;
    let c = chain(o)?;
    // The node's URL is only in its `Registered` log: read with --since-block.
    let texts = since(o)?.is_some();
    let mut s = view(o, &c, Epochs::Now, texts, ExclScope::None).await?;
    let mut out = json!({"epoch": s.epoch, "block": s.block, "nodes": s.list_at(s.epoch).len(),
                         "deals": s.deals.len(), "conserved": c.conserved().await?});
    if let Some(n) = o.opt("node") {
        let me = node(n)?;
        let info = c.node_info(&me).await?;
        // Which shards the node holds needs `exclusionFrom` for every shard
        // of every open deal (about r calls a shard): read when that is
        // small, or when asked.
        let open = s.open_shards();
        let (now, next, note) = if status_reads_shards(o.opt("shards"), open)? {
            let ids: Vec<u64> = s.deals.iter().map(|d| d.id).collect();
            for id in ids {
                c.fill_exclusions(&mut s, id, None).await?;
            }
            (
                json!(assignments(&s, &me, s.epoch)),
                json!(assignments(&s, &me, s.epoch + 1)),
                Value::Null,
            )
        } else {
            (
                Value::Null,
                Value::Null,
                json!(format!(
                    "not read: the open deals have {open} shards (over {STATUS_SHARDS}, or --shards no); --shards yes reads them, about 3 calls a shard"
                )),
            )
        };
        out["node"] = json!({
            "id": me.to_string(), "operator": hx(&info.operator), "status": info.status,
            "capacity_gib": info.capacity_gib, "bond": info.bond.to_string(), "misses": info.misses,
            "removed_from": info.removed_from, "jailed_from": info.jailed_from,
            "url": s.url(&me), "shards_now": now, "shards_next_epoch": next,
            "earned": c.earned(&info.operator).await?.to_string(),
        });
        if !note.is_null() {
            out["node"]["shards_note"] = note;
        }
    }
    if let Some(a) = o.opt("operator") {
        out["earned"] = json!(c.earned(&addr(a, "--operator")?).await?.to_string());
    }
    println!("{out:#}");
    Ok(())
}

fn assignments(s: &Snapshot, me: &NodeId, e: u64) -> Vec<Value> {
    let mut v = Vec::new();
    for d in s
        .deals
        .iter()
        .filter(|d| !d.info.closed && d.info.end_epoch > s.epoch)
    {
        for i in 0..d.info.shard_count {
            if s.holders(d, i, e).contains(me) {
                v.push(json!({"deal": d.id, "shard": i}));
            }
        }
    }
    v
}

fn read_manifest(dir: &Path) -> Res<Manifest> {
    let m =
        std::fs::read(dir.join("manifest.bin")).map_err(|e| format!("{}: {e}", dir.display()))?;
    Manifest::from_bytes(&m).map_err(|e| e.to_string())
}

/// The manifest of `--dir` and its anchor: its own report (`report.bin`),
/// or with `--catalog FILE` its leaf in that catalog and `FILE.report`.
fn local_anchor(dir: &Path, catalog: Option<&str>) -> Res<(Manifest, ManifestAnchor)> {
    let m = read_manifest(dir)?;
    let Some(file) = catalog else {
        let r = std::fs::read(dir.join("report.bin")).map_err(|e| {
            format!(
                "{}: {e} (sign first, or name the catalog that lists it with --catalog)",
                dir.display()
            )
        })?;
        return Ok((m, ManifestAnchor::Own(r)));
    };
    let b = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let c: onyx_bulk::Catalog =
        borsh::from_slice(&b).map_err(|_| format!("{file}: not a Borsh catalog"))?;
    c.validate().map_err(|e| format!("{file}: {e}"))?;
    let rf = format!("{file}.report");
    let r = std::fs::read(&rf).map_err(|e| format!("{rf}: {e} (catalog-sign first)"))?;
    let r = borsh::from_slice(&r).map_err(|_| format!("{rf}: does not decode"))?;
    let a = anchor_in_catalog(&m, &c, r)?;
    Ok((m, a))
}

async fn deal(o: &Opts) -> Res<()> {
    o.only(&[
        "dir",
        "catalog",
        "root",
        "node",
        "r",
        "epochs",
        "source-url",
        "book-check",
    ])?;
    let c = chain(o)?;
    let (m, anchor) = match (o.opt("dir"), o.opt("root")) {
        (Some(dir), None) if o.opt("node").is_none() => {
            local_anchor(Path::new(dir), o.opt("catalog"))?
        }
        (None, Some(root)) if o.opt("catalog").is_none() => {
            let b: [u8; 32] = hexn(root, 32, "--root")?
                .try_into()
                .map_err(|_| "--root")?;
            let get = HyperGet::new(Duration::from_secs(60));
            fetch_manifest(&get, &[o.req("node")?.to_string()], &Hash32(b)).await?
        }
        _ => {
            return Err(
                "give --dir DIR (with --catalog FILE for a manifest a catalog anchors), or --root 0x… with --node URL"
                    .into(),
            )
        }
    };
    let rr: u8 = o.num("r")?;
    let epochs: u32 = o.num("epochs")?;
    // What this contract can open at all (rules below 4: no catalog deal, no
    // format-2 manifest), read before the approval is sent.
    deal_fits_rules(c.rules_version().await?, &m, &anchor)?;
    // Checked as the contract will check it, before anything is sent.
    let (t, call) = deal_call(&m, &anchor, rr, epochs, o.req("source-url")?)?;
    // And what the contract will ask the book (the report holds; for a
    // catalog, no next catalog holds): a refusal here costs no approval.
    // (`--book-check no` skips it: a test switch, to see the contract's own
    // refusal.)
    match o.opt("book-check") {
        None | Some("yes") => c.check_deal_in_book(&anchor).await?,
        Some("no") => {}
        Some(_) => return Err("--book-check must be yes or no".into()),
    }
    let how = match &anchor {
        ManifestAnchor::Own(_) => "own-report".to_string(),
        ManifestAnchor::Catalog(a) => format!("catalog-{}", a.header.number),
    };
    let tx = sender(o).await?;
    let p = c.params().await?;
    let billed = onyx_storage::ledger::billable_bytes(t.byte_count, t.shard_count);
    let dep = onyx_storage::ledger::deposit(&p.money, billed, rr, epochs);
    approve(&tx, &c, dep).await?;
    let id = abi::word_u64(&c.call(&abi::deal_count()).await?, 0).map_err(|e| e.to_string())?;
    let h = send(&tx, &c.contract(), call).await?;
    let d = c.deal_info(id).await?;
    if d.manifest_root != t.manifest_root {
        return Err("the deal on chain names another manifest".into());
    }
    println!(
        "DEAL_ID={id} tx={h} manifest={} anchor={how} shards={} gib={} r={rr} epochs={}..{} deposit={dep}",
        t.manifest_root,
        t.shard_count,
        onyx_storage::ledger::gib(billed),
        d.start_epoch,
        d.end_epoch
    );
    Ok(())
}

async fn simple(o: &Opts, data: Vec<u8>, what: &str) -> Res<()> {
    let c = chain(o)?;
    let tx = sender(o).await?;
    let h = send(&tx, &c.contract(), data).await?;
    println!("{what} tx={h}");
    Ok(())
}

async fn settle(o: &Opts) -> Res<()> {
    o.only(&["epoch", "since-block"])?;
    let c = chain(o)?;
    let tx = sender(o).await?;
    let now = c.current_epoch().await?;
    let epochs: Vec<u64> = match o.opt("epoch") {
        Some(e) => vec![e.parse().map_err(|_| "--epoch must be a number")?],
        None => (1..=onyx_storage::SETTLE_EPOCHS)
            .filter_map(|b| now.checked_sub(b))
            .collect(),
    };
    let (Some(lo), Some(hi)) = (epochs.iter().min(), epochs.iter().max()) else {
        println!("SETTLED 0");
        return Ok(());
    };
    // The challenges of epoch e come from the node list of e - 1.
    let s = view(
        o,
        &c,
        Epochs::Span(lo.saturating_sub(1), *hi),
        false,
        ExclScope::Slots,
    )
    .await?;
    let mut n = 0;
    for e in epochs {
        n += settle_epoch(&c, &tx, &s, e).await?;
    }
    println!("SETTLED {n}");
    Ok(())
}

async fn books(o: &Opts) -> Res<()> {
    o.only(&["replay"])?;
    let replay = match o.opt("replay") {
        None | Some("no") => false,
        Some("yes") => true,
        Some(_) => return Err("--replay must be yes or no".into()),
    };
    let c = chain(o)?;
    // One block for the books, the balance and (with --replay) the logs.
    let at = c.block_number().await?;
    let b = c.books_at(at).await?;
    let names = [
        "deposits",
        "bonds_in",
        "paid",
        "slashed",
        "refunded",
        "bonds_out",
        "earnings_out",
        "swept",
        "rewarded",
    ];
    let mut v = json!({"block": at});
    for (n, x) in names.iter().zip(b) {
        v[*n] = json!(x.to_string());
    }
    let t = token(&c).await?;
    // The balance and `conserved()` at the same block as the books, so a
    // transaction in between cannot make them disagree (review STORAGE-S16-R4).
    let bal = abi::word_uint(
        &c.call_to_at(
            &t,
            &abi::call("balanceOf(address)", &[abi::addr(&c.contract())]),
            at,
        )
        .await?,
        0,
    )
    .map_err(|e| e.to_string())?;
    let books = onyx_storage::ledger::Books {
        deposits: b[0],
        bonds_in: b[1],
        paid: b[2],
        slashed: b[3],
        refunded: b[4],
        bonds_out: b[5],
        earnings_out: b[6],
        swept: b[7],
        rewarded: b[8],
        balance: bal,
    };
    let ok = books.conserved();
    v["balance"] = json!(bal.to_string());
    v["held"] = json!(if ok {
        (books.deal_held() + books.bonds_held() + books.earnings_held() + books.treasury_held())
            .to_string()
    } else {
        "?".to_string()
    });
    let on_chain = abi::word_uint(
        &c.call_to_at(&c.contract(), &abi::conserved(), at).await?,
        0,
    )
    .map_err(|e| e.to_string())?
        == 1;
    v["conserved"] = json!(on_chain && ok);
    if replay {
        // The same totals rebuilt from the money events alone (ledger::replay).
        let r = c.replay_books(at).await?;
        // The events cannot see tokens sent outside every flow: those stay a
        // surplus over the replayed balance.
        v["replayed"] = json!(r.totals() == b && r.balance <= bal);
    }
    println!("{v:#}");
    if v["conserved"] != json!(true) {
        return Err("the books do not balance".into());
    }
    if replay && v["replayed"] != json!(true) {
        return Err("the books rebuilt from the events differ from books()".into());
    }
    Ok(())
}

/// Every node and every deal with its declines and slashes, from the chain's
/// events (ADR-0060 amendment 2 item 1: public, no rule on them before S17).
/// A node that declines much more than others with the same deals shows here.
async fn declines(o: &Opts) -> Res<()> {
    o.only(&[])?;
    let c = chain(o)?;
    let s = c.snapshot().await?;
    let r = s.excl.decline_report();
    let counts = |x: onyx_storage::placement::DeclineCount| {
        json!({"shard_declines": x.shard_declines, "whole_deal_declines": x.whole_deal_declines,
               "declines_with_notice": x.with_notice, "slashes": x.slashes})
    };
    let mut nodes = Vec::new();
    for (id, n) in &s.nodes {
        let x = r.nodes.get(id).copied().unwrap_or_default();
        let mut v = counts(x);
        v["id"] = json!(id.to_string());
        v["operator"] = json!(hx(&n.info.operator));
        v["status"] = json!(n.info.status);
        v["deals"] = json!(x.across);
        nodes.push(v);
    }
    let mut deals = Vec::new();
    for d in &s.deals {
        let x = r.deals.get(&d.id).copied().unwrap_or_default();
        let mut v = counts(x);
        v["deal"] = json!(d.id);
        v["shards"] = json!(d.info.shard_count);
        v["copies"] = json!(d.info.r);
        v["open"] = json!(true);
        v["nodes"] = json!(x.across);
        deals.push(v);
    }
    // The snapshot leaves out closed deals (and deals no node can place);
    // their declines still count for the nodes and are listed here.
    for (id, x) in &r.deals {
        if s.deals.iter().all(|d| d.id != *id) {
            let mut v = counts(*x);
            v["deal"] = json!(id);
            v["open"] = json!(false);
            v["nodes"] = json!(x.across);
            deals.push(v);
        }
    }
    println!(
        "{:#}",
        json!({"epoch": s.epoch, "block": s.block, "nodes": nodes, "deals": deals})
    );
    Ok(())
}

async fn read(o: &Opts) -> Res<()> {
    o.only(&["deal", "from", "to", "since-block"])?;
    let c = chain(o)?;
    let get = HyperGet::new(Duration::from_secs(60));
    // The replicas' URLs and the deal's source are only in the logs.
    // Exclusions are read only for the shards the key range needs.
    let mut s = view(o, &c, Epochs::Now, true, ExclScope::None).await?;
    let (deal, from, to) = (o.num("deal")?, o.num("from")?, o.num("to")?);
    let r = read_rows_in(Some(&c), &mut s, &get, deal, from, to)
        .await
        .map_err(|e| e.to_string())?;
    let keys: Vec<String> = r.rows.iter().map(|x| x.2.to_string()).collect();
    println!(
        "{:#}",
        json!({"ok": true, "rows": r.rows.len(), "first": keys.first(), "last": keys.last(),
               "served_by": r.served_by.iter().map(|(s, u)| json!({"shard": s, "url": u})).collect::<Vec<_>>(),
               "refused": r.refused.iter().map(|(u, w)| json!({"url": u, "why": w})).collect::<Vec<_>>()})
    );
    Ok(())
}

async fn prove(o: &Opts) -> Res<()> {
    o.only(&["deal", "slot", "node", "dir", "since-block"])?;
    let c = chain(o)?;
    // The answer names the node list of the epoch before; no exclusion is read.
    let s = view(o, &c, Epochs::Now, false, ExclScope::None).await?;
    let dir = Path::new(o.req("dir")?);
    let m = read_manifest(dir)?;
    let d: u64 = o.num("deal")?;
    let slot: u32 = o.num("slot")?;
    let me = node(o.req("node")?)?;
    let e = s.epoch;
    let seed = c.seed_of(e).await?;
    if seed == Hash32::ZERO {
        return Err(format!("epoch {e} has no seed yet"));
    }
    // The publisher's shard file of the shard the slot picks (onyx-publish layout).
    let index = onyx_storage::ids::slot_shard(&seed, d, slot, m.header.shard_count);
    let root = m.shards.get(index as usize).ok_or("no such shard")?.root;
    let path = dir.join("shards").join(format!("{}.bin", root.to_hex()));
    let file = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The row drawn for this node (each holder is asked for its own row).
    let p = onyx_storage::challenge::answer(&m, &file, &seed, d, slot, &me)
        .map_err(|e| e.to_string())?;
    let data = abi::answer(d, e, slot, &me, &s.list_at(e.saturating_sub(1)), &p);
    // Where the row's bytes start in the calldata (word 7 holds the row's offset).
    let off = abi::word_u64(&data[4..], 7).map_err(|e| e.to_string())? as usize;
    println!(
        "{:#}",
        json!({"epoch": e, "shard": p.entry.index, "calldata": hx(&data), "row_at": 4 + off + 32, "row_len": p.row.len()})
    );
    Ok(())
}

async fn run(cmd: &str, o: &Opts) -> Res<()> {
    match cmd {
        "register" => register(o).await,
        "status" => status(o).await,
        "deal" => deal(o).await,
        "seed" => {
            o.only(&[])?;
            let e = chain(o)?.current_epoch().await?;
            simple(o, abi::record_seed(e), &format!("SEED epoch={e}")).await
        }
        "settle" => settle(o).await,
        "withdraw" => {
            o.only(&[])?;
            simple(o, abi::withdraw_earnings(), "WITHDRAWN").await
        }
        "exit" => {
            o.only(&["node"])?;
            simple(o, abi::request_exit(&node(o.req("node")?)?), "EXITING").await
        }
        "decline" => {
            o.only(&["node", "deal", "shard"])?;
            let index = match o.opt("shard") {
                Some(v) => v.parse().map_err(|_| "--shard must be a number")?,
                None => onyx_storage::placement::WHOLE_DEAL,
            };
            simple(
                o,
                abi::decline(o.num("deal")?, index, &node(o.req("node")?)?),
                "DECLINED",
            )
            .await
        }
        "withdraw-bond" => {
            o.only(&["node"])?;
            simple(
                o,
                abi::withdraw_bond(&node(o.req("node")?)?),
                "BOND_WITHDRAWN",
            )
            .await
        }
        "close" => {
            o.only(&["deal"])?;
            simple(o, abi::close_deal(o.num("deal")?), "CLOSED").await
        }
        "sweep" => {
            o.only(&[])?;
            simple(o, abi::sweep_treasury(), "SWEPT").await
        }
        "books" => books(o).await,
        "declines" => declines(o).await,
        "read" => read(o).await,
        "prove" => prove(o).await,
        "epoch" => {
            o.only(&["epoch", "since-block"])?;
            let c = chain(o)?;
            let e: u64 = match o.opt("epoch") {
                Some(v) => v.parse().map_err(|_| "--epoch must be a number")?,
                None => c.current_epoch().await?,
            };
            // Only what epoch e needs: its node list is that of e - 1.
            // Its node list is that of e - 1, its exclusions those of the
            // shards its slots pick.
            let span = Epochs::Span(e.saturating_sub(1), e);
            let s = view(o, &c, span, false, ExclScope::Slots).await?;
            println!("{:#}", epoch_report(&c, &s, e).await?);
            Ok(())
        }
        _ => Err(format!("unknown command {cmd}")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if cmd == "--help" || cmd == "-h" {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let opts = match Opts::parse(rest) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("onyx-storage: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("onyx-storage: runtime: {e}");
            return ExitCode::from(1);
        }
    };
    match rt.block_on(run(cmd, &opts)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("onyx-storage: {e}");
            ExitCode::from(1)
        }
    }
}
