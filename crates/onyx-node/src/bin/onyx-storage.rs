//! `onyx-storage`: the storage network's operator, publisher and client tool
//! (ADR-0060, docs/STORAGE-NETWORK.md, docs/ops/storage-node.md). Test
//! network only: it refuses any chain but a local anvil (31337) and Base
//! Sepolia (84532).
//!
//! ```text
//! common: --rpc URL --contract 0x… [--from-block N]   (sending commands add --chain-id N --key FILE)
//!
//! onyx-storage register      --capacity-gib N --url URL    # approve the bond, register, activate; prints NODE_ID
//! onyx-storage status        [--node 0x…] [--operator 0x…] # epoch, node, its shards now and next epoch, earnings
//! onyx-storage deal          --dir DIR --r 3 --epochs N --source-url URL   # buy storage for an anchored manifest
//! onyx-storage seed                                         # record this epoch's seed
//! onyx-storage settle        [--epoch E]                    # settle the misses of E (default: the two before now)
//! onyx-storage withdraw                                     # earnings to the operator
//! onyx-storage exit          --node 0x…
//! onyx-storage withdraw-bond --node 0x…
//! onyx-storage decline       --node 0x… --deal N [--shard I]   # give up a shard (or the whole deal) from the next epoch, no slash
//! onyx-storage close         --deal N                       # a deal past its end: the rest back to its payer
//! onyx-storage sweep                                        # slashes to the treasury
//! onyx-storage books         [--replay yes]                 # every flow, and the conservation check (yes: also rebuilt from the events)
//! onyx-storage epoch         [--epoch E]                    # who was asked for what, passed, failed, missed, paid, slashed
//! onyx-storage declines                                    # declines and slashes per node and per deal (ADR-0060 amendment 2 item 1)
//! onyx-storage read          --deal N --from K --to K       # rows from any replica, checked; fails over
//! onyx-storage prove         --deal N --slot S --node 0x… --dir DIR   # answer calldata from the publisher's files (test tool)
//! ```
//!
//! Keys are read from 0600 files and never printed. Exit 0 done, 1 refused
//! or failed, 2 usage.

use onyx_api::evm_tx::Sender;
use onyx_api::roots::{HttpJsonRpc, JsonRpc};
use onyx_api::storage::{
    epoch_report, read_rows, settle_epoch, HyperGet, Snapshot, StorageChain, TxSend,
};
use onyx_api::x402::HyperPost;
use onyx_bulk::manifest::Manifest;
use onyx_core::{Hash32, SignedReport};
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
        let common = ["rpc", "contract", "from-block", "chain-id", "key"];
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
    Ok(StorageChain::new(
        Arc::new(rpc(o)?),
        addr(o.req("contract")?, "--contract")?,
        from,
    ))
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
    let id = c
        .logs(abi::events::ACTIVATED, head)
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
    o.only(&["node", "operator"])?;
    let c = chain(o)?;
    let s = c.snapshot().await?;
    let mut out = json!({"epoch": s.epoch, "block": s.block, "nodes": s.list_at(s.epoch).len(),
                         "deals": s.deals.len(), "conserved": c.conserved().await?});
    if let Some(n) = o.opt("node") {
        let me = node(n)?;
        let info = c.node_info(&me).await?;
        let now: Vec<Value> = assignments(&s, &me, s.epoch);
        let next: Vec<Value> = assignments(&s, &me, s.epoch + 1);
        out["node"] = json!({
            "id": me.to_string(), "operator": hx(&info.operator), "status": info.status,
            "capacity_gib": info.capacity_gib, "bond": info.bond.to_string(), "misses": info.misses,
            "removed_from": info.removed_from, "jailed_from": info.jailed_from,
            "url": s.url(&me), "shards_now": now, "shards_next_epoch": next,
            "earned": c.earned(&info.operator).await?.to_string(),
        });
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

fn read_dir(dir: &Path) -> Res<(Manifest, SignedReport)> {
    let m =
        std::fs::read(dir.join("manifest.bin")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let m = Manifest::from_bytes(&m).map_err(|e| e.to_string())?;
    let r = std::fs::read(dir.join("report.bin"))
        .map_err(|e| format!("{}: {e} (sign first)", dir.display()))?;
    let r: SignedReport = borsh::from_slice(&r).map_err(|_| "report.bin does not decode")?;
    Ok((m, r))
}

async fn deal(o: &Opts) -> Res<()> {
    o.only(&["dir", "r", "epochs", "source-url"])?;
    let c = chain(o)?;
    let tx = sender(o).await?;
    let (m, r) = read_dir(Path::new(o.req("dir")?))?;
    let header = borsh::to_vec(&m.header).map_err(|e| e.to_string())?;
    let payload = borsh::to_vec(&r.payload).map_err(|e| e.to_string())?;
    let t = onyx_storage::deal::read_header(&header).map_err(|e| e.to_string())?;
    onyx_storage::deal::check_payload(&t, &payload).map_err(|e| e.to_string())?;
    let rr: u8 = o.num("r")?;
    let epochs: u32 = o.num("epochs")?;
    let p = c.params().await?;
    let billed = onyx_storage::ledger::billable_bytes(t.byte_count, t.shard_count);
    let dep = onyx_storage::ledger::deposit(&p.money, billed, rr, epochs);
    approve(&tx, &c, dep).await?;
    let id = abi::word_u64(&c.call(&abi::deal_count()).await?, 0).map_err(|e| e.to_string())?;
    let h = send(
        &tx,
        &c.contract(),
        abi::create_deal(&header, &payload, rr, epochs, o.req("source-url")?),
    )
    .await?;
    let d = c.deal_info(id).await?;
    if d.manifest_root != t.manifest_root {
        return Err("the deal on chain names another manifest".into());
    }
    println!(
        "DEAL_ID={id} tx={h} manifest={} shards={} gib={} r={rr} epochs={}..{} deposit={dep}",
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
    o.only(&["epoch"])?;
    let c = chain(o)?;
    let tx = sender(o).await?;
    let s = c.snapshot().await?;
    let epochs: Vec<u64> = match o.opt("epoch") {
        Some(e) => vec![e.parse().map_err(|_| "--epoch must be a number")?],
        None => (1..=onyx_storage::SETTLE_EPOCHS)
            .filter_map(|b| s.epoch.checked_sub(b))
            .collect(),
    };
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
    o.only(&["deal", "from", "to"])?;
    let c = chain(o)?;
    let get = HyperGet::new(Duration::from_secs(60));
    let r = read_rows(&c, &get, o.num("deal")?, o.num("from")?, o.num("to")?)
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
    o.only(&["deal", "slot", "node", "dir"])?;
    let c = chain(o)?;
    let s = c.snapshot().await?;
    let dir = Path::new(o.req("dir")?);
    let (m, _) = read_dir(dir)?;
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
            o.only(&["epoch"])?;
            let c = chain(o)?;
            let s = c.snapshot().await?;
            let e = match o.opt("epoch") {
                Some(v) => v.parse().map_err(|_| "--epoch must be a number")?,
                None => s.epoch,
            };
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
