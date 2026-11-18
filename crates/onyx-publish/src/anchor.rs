//! `postReport` for `anchor` and `catalog-anchor`: send a signed report to
//! the book once, and be safe and useful to run again after any stop
//! (THREATS EARLY-21, RETRY-R1-H1, RETRY-R1-L1, RETRY-R2-M1..L3; the daily
//! record's run and the unit behind it run it again by themselves).
//!
//! What it keeps: a small JSON file beside the report (`anchor-tx.json` in a
//! version folder, `FILE.anchor-tx.json` beside a catalog) with the nonce,
//! the gas limit and, for every version of the transaction, the hash, the
//! fees, the signed bytes, the time and whether a node took it. Never a key.
//! The signed bytes are public once sent. The file is written owner-only
//! through a new name that is never a link, synced, then renamed; a version
//! is written BEFORE it is sent, so a stop between the two loses nothing.
//! Read back, every version must be exactly what this key signs for this
//! report at the recorded nonce, gas and fees, or the run refuses.
//!
//! Run again, in this order:
//! 1. The book holds the report: done, nothing sent.
//! 2. The recorded nonce is used up (the key's `latest` nonce is past it):
//!    the receipts decide (mined: done; reverted: see below; none: another
//!    transaction took the nonce, so the report is sent anew). A receipt
//!    that cannot be read is a wait, never "none".
//! 3. The recorded nonce is the key's next one. Whose transaction is
//!    pending there is asked first (`eth_getTransactionByHash` of every
//!    recorded version). None known while the node shows a pending
//!    transaction of the key: it is another's, see 4. None known and none
//!    shown (dropped, or a node that does not show its pool): the newest
//!    version goes to the node again as it was signed, no fee raised; the
//!    node's "replacement transaction underpriced" then means another
//!    transaction holds the nonce, see 4.
//!    Its own: younger than `--stuck-secs` it is waited for; older, it is
//!    replaced: the SAME report at the SAME nonce and gas with both fees
//!    raised by at least 12.5% (and to the market, if that is more), never
//!    above `--max-fee-wei`. Only replacements a node took count against
//!    `--max-bumps`; a try that failed is made again, no sooner than a sixth
//!    of `--stuck-secs` later. After the last raise went stuck too, the run
//!    refuses with the line a person needs. Only one transaction of a nonce
//!    can mine, and the book takes a report once (`Replay`), so a
//!    replacement can never make a second report.
//! 4. An unmined transaction of the gas key that is not this record's (a
//!    transfer or a cancel made by hand, another tool, an earlier run that
//!    kept no record) is never replaced and never sent over: transient
//!    while seen for less than `--stuck-secs`, then refused, naming the
//!    nonce.
//! 5. Otherwise the report is sent, and the record written before the wait.
//!
//! A revert: when the book shows the record, an earlier transaction did the
//! work and this is done. When it does not, the book is asked why
//! (`eth_call` of the same data): `Replay` means it holds the report and
//! the read was behind (`not_visible`, exit 75); another reason is a
//! refusal; no reason at all is read once more on the next run, then
//! refused.

use crate::{classes, eth_call, hx, post_report_calldata, rpc_stop, Opts, Res, Stop};
use onyx_api::evm_tx::{
    is_revert, Mined, SendError, Sender, Tx1559, DEFAULT_MAX_FEE_PER_GAS, DEFAULT_MAX_GAS,
};
use onyx_api::roots::JsonRpc;
use onyx_core::{keccak256, Hash32, SignedReport};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The options `anchor` and `catalog-anchor` take beside their own.
pub(crate) const OPTIONS: &[&str] = &["stuck-secs", "max-bumps", "max-fee-wei", "mine-secs"];
/// The transaction record's name in a version folder.
pub(crate) const TX_RECORD: &str = "anchor-tx.json";
/// The runbook section a refusal names.
const RUNBOOK: &str = "docs/ops/early-record.md \"A stuck transaction\"";
/// The most `--max-fee-wei` may be: 1000 gwei, a hundred times the default.
/// A typo must not hand the gas key's balance to one transaction.
const MAX_FEE_CEILING: u128 = 1_000_000_000_000;

/// The transaction record beside a catalog `FILE`: `FILE.anchor-tx.json`.
pub(crate) fn record_path(catalog: &Path) -> PathBuf {
    let mut p = catalog.as_os_str().to_os_string();
    p.push(".");
    p.push(TX_RECORD);
    PathBuf::from(p)
}

/// How [`post_report`] waits, when it replaces, and where it keeps its
/// record.
#[derive(Clone, Debug)]
pub(crate) struct AnchorOpts {
    /// How long one run waits for the receipt (`--mine-secs`, 180).
    pub deadline: Duration,
    /// How often it asks for the receipt (shorter in tests only).
    pub poll: Duration,
    /// How long its own transaction may be unmined before it is replaced,
    /// and how long an unmined transaction it did not send is waited for
    /// before the refusal (`--stuck-secs`, 1800). A sixth of it is the
    /// least time between two tries to replace.
    pub stuck: Duration,
    /// Replacements of one transaction at most (`--max-bumps`, 4).
    pub max_bumps: u32,
    /// The most any version may offer per gas (`--max-fee-wei`, 10 gwei).
    pub max_fee_wei: u128,
    /// The record file (always given by the commands).
    pub record: Option<PathBuf>,
    /// The time now, in unix seconds.
    pub now_unix: u64,
}

impl AnchorOpts {
    /// From the command's options; `record` is where the record goes.
    pub(crate) fn from_opts(o: &Opts, record: PathBuf) -> Res<AnchorOpts> {
        let within = |k: &str, default: u64, lo: u64, hi: u64| -> Res<u64> {
            let v = o.num(k, default)?;
            if v < lo || v > hi {
                return Err(format!("--{k}: a whole number from {lo} to {hi}"));
            }
            Ok(v)
        };
        let max_fee_wei = match o.opt("max-fee-wei") {
            None => DEFAULT_MAX_FEE_PER_GAS,
            Some(v) => v
                .parse::<u128>()
                .ok()
                .filter(|v| (1..=MAX_FEE_CEILING).contains(v))
                .ok_or(format!(
                    "--max-fee-wei: a whole number of wei from 1 to {MAX_FEE_CEILING}"
                ))?,
        };
        Ok(AnchorOpts {
            deadline: Duration::from_secs(within("mine-secs", 180, 1, 3600)?),
            poll: Duration::from_millis(500),
            stuck: Duration::from_secs(within("stuck-secs", 1800, 0, 7 * 86_400)?),
            max_bumps: u32::try_from(within("max-bumps", 4, 0, 20)?).unwrap_or(4),
            max_fee_wei,
            record: Some(record),
            now_unix: crate::now_unix(),
        })
    }

    /// The least time between two tries to replace a transaction.
    fn gap(&self) -> u64 {
        self.stuck.as_secs() / 6
    }
}

/// One version of the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    hash: String,
    max_fee: u128,
    tip: u128,
    /// The signed bytes, `0x` hex: handed to a node again as they are.
    raw: String,
    /// When a node took it (or, while `sent` is false, when it was signed).
    sent_unix: u64,
    /// A node took it. False from the write before the send until the
    /// node's answer (or its "already known") is in.
    sent: bool,
}

/// Whose record it is: a record of another report, chain, book or sender
/// is not read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    digest: Hash32,
    chain_id: u64,
    book: [u8; 20],
    from: [u8; 20],
}

/// What was sent for one report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Record {
    /// The nonce of every version in `txs`.
    nonce: Option<u64>,
    /// Their gas limit: the first send's estimate with its margin, the
    /// same for every replacement.
    gas: u64,
    /// The versions, oldest first. Only the last may be unsent.
    txs: Vec<Version>,
    /// When a replacement was last tried (sent or not).
    last_try_unix: u64,
    /// Replacements in a row a node refused as underpriced: the next one
    /// raises the fees once more for each.
    underpriced: u32,
    /// Mined reverts the book gave no reason for.
    reverts: u32,
    /// An unmined transaction this tool did not send: its nonce and when it
    /// was first seen.
    foreign: Option<(u64, u64)>,
}

/// One RLP item at the start of `b`: its payload and what follows it.
fn rlp_item(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let head = *b.first()?;
    let long = |n: usize| -> Option<(&[u8], &[u8])> {
        let len = b.get(1..1 + n)?.iter().try_fold(0usize, |a, x| {
            a.checked_mul(256)?.checked_add(usize::from(*x))
        })?;
        let rest = b.get(1 + n..)?;
        (len <= rest.len()).then(|| rest.split_at(len))
    };
    let short = |len: usize| -> Option<(&[u8], &[u8])> {
        let rest = b.get(1..)?;
        (len <= rest.len()).then(|| rest.split_at(len))
    };
    match head {
        0..=0x7f => Some((b.get(..1)?, b.get(1..)?)),
        0x80..=0xb7 => short(usize::from(head - 0x80)),
        0xb8..=0xbf => long(usize::from(head - 0xb7)),
        0xc0..=0xf7 => short(usize::from(head - 0xc0)),
        _ => long(usize::from(head - 0xf7)),
    }
}

/// The fields of a signed type-2 transaction (`0x` hex of
/// `0x02 || rlp([chain, nonce, tip, max fee, gas, to, value, data, ...])`).
fn decode_raw(raw: &str) -> Option<Tx1559> {
    let b = hex::decode(raw.strip_prefix("0x")?).ok()?;
    let (list, _) = rlp_item(b.strip_prefix(&[0x02])?)?;
    let num = |b: &[u8]| -> Option<u128> {
        (b.len() <= 16).then(|| b.iter().fold(0u128, |a, x| (a << 8) | u128::from(*x)))
    };
    let (chain_id, rest) = rlp_item(list)?;
    let (nonce, rest) = rlp_item(rest)?;
    let (tip, rest) = rlp_item(rest)?;
    let (max_fee, rest) = rlp_item(rest)?;
    let (gas, rest) = rlp_item(rest)?;
    let (to, rest) = rlp_item(rest)?;
    let (value, rest) = rlp_item(rest)?;
    let (data, _) = rlp_item(rest)?;
    Some(Tx1559 {
        chain_id: u64::try_from(num(chain_id)?).ok()?,
        nonce: u64::try_from(num(nonce)?).ok()?,
        max_priority_fee_per_gas: num(tip)?,
        max_fee_per_gas: num(max_fee)?,
        gas: u64::try_from(num(gas)?).ok()?,
        to: to.try_into().ok()?,
        value: num(value)?,
        data: data.to_vec(),
    })
}

impl Record {
    fn hashes(&self) -> Vec<String> {
        self.txs.iter().rev().map(|v| v.hash.clone()).collect()
    }

    /// Replacements a node took.
    fn raises(&self) -> usize {
        self.txs.iter().filter(|v| v.sent).count().saturating_sub(1)
    }

    fn to_json(&self, id: &Identity) -> Value {
        json!({
            "what": "the transactions onyx-publish sent for one report (no key; safe to read, see docs/ops/early-record.md)",
            "version": 1,
            "digest": id.digest.to_string(),
            "chain_id": id.chain_id,
            "book": hx(&id.book),
            "from": hx(&id.from),
            "nonce": self.nonce,
            "gas": self.gas,
            "txs": self.txs.iter().map(|v| json!({
                "hash": v.hash,
                "max_fee_per_gas": v.max_fee.to_string(),
                "max_priority_fee_per_gas": v.tip.to_string(),
                "raw": v.raw,
                "sent_unix": v.sent_unix,
                "sent": v.sent,
            })).collect::<Vec<_>>(),
            "last_try_unix": self.last_try_unix,
            "underpriced": self.underpriced,
            "reverts": self.reverts,
            "foreign": self.foreign.map(|(nonce, seen)| json!({"nonce": nonce, "first_seen_unix": seen})),
        })
    }

    /// The record in `v`, `None` when a field is missing or wrong.
    fn from_json(v: &Value) -> Option<Record> {
        let wei = |x: &Value| x.as_str()?.parse::<u128>().ok();
        let txs = v["txs"]
            .as_array()?
            .iter()
            .map(|t| {
                Some(Version {
                    hash: t["hash"].as_str()?.to_string(),
                    max_fee: wei(&t["max_fee_per_gas"])?,
                    tip: wei(&t["max_priority_fee_per_gas"])?,
                    raw: t["raw"].as_str()?.to_string(),
                    sent_unix: t["sent_unix"].as_u64()?,
                    sent: t["sent"].as_bool()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let foreign = match &v["foreign"] {
            Value::Null => None,
            f => Some((f["nonce"].as_u64()?, f["first_seen_unix"].as_u64()?)),
        };
        let nonce = match &v["nonce"] {
            Value::Null => None,
            n => Some(n.as_u64()?),
        };
        if nonce.is_none() != txs.is_empty() {
            return None;
        }
        Some(Record {
            nonce,
            gas: v["gas"].as_u64()?,
            txs,
            last_try_unix: v["last_try_unix"].as_u64()?,
            underpriced: u32::try_from(v["underpriced"].as_u64()?).ok()?,
            reverts: u32::try_from(v["reverts"].as_u64()?).ok()?,
            foreign,
        })
    }

    /// The record at `path` when it is `id`'s; an empty one when there is
    /// none or it is another report's. A symbolic link is never read. A
    /// file that is not a record (cut off, not JSON, a field missing) is
    /// moved aside as `<name>.bad-<unix>` and counts as no record only when
    /// `idle`: the chain shows no pending transaction of the key, so nothing
    /// of ours can be lost by forgetting it. Otherwise it is refused:
    /// guessing could replace a transaction that is not ours.
    fn load(path: Option<&Path>, id: &Identity, idle: bool, now: u64) -> Result<Record, Stop> {
        let Some(path) = path else {
            return Ok(Record::default());
        };
        let name = path.display();
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Record::default()),
            Err(e) => return Err(format!("{name}: {e}").into()),
            Ok(_) => {}
        }
        let text = onyx_api::journal::open_plain(
            std::fs::OpenOptions::new().read(true),
            path,
            false,
        )
        .and_then(std::io::read_to_string)
        .map_err(|e| {
            Stop::Refused(format!(
                "{name}: {e}: the transaction record is not read through a link or from anything but a plain file ({RUNBOOK})"
            ))
        })?;
        let parsed = serde_json::from_str::<Value>(&text)
            .ok()
            .filter(|v| v["version"] == json!(1));
        let same = |v: &Value| {
            v["digest"] == json!(id.digest.to_string())
                && v["chain_id"] == json!(id.chain_id)
                && v["book"] == json!(hx(&id.book))
                && v["from"] == json!(hx(&id.from))
        };
        match parsed.as_ref().map(|v| (same(v), Record::from_json(v))) {
            // Another report's (a catalog built again under the name).
            Some((false, _)) => Ok(Record::default()),
            Some((true, Some(rec))) => Ok(rec),
            _ if idle => {
                let mut aside = path.as_os_str().to_os_string();
                aside.push(format!(".bad-{now}"));
                std::fs::rename(path, &aside).map_err(|e| format!("{name}: {e}"))?;
                eprintln!(
                    "postReport: {name} is not a transaction record of this tool (cut off or changed): moved aside as {}; the gas key has no pending transaction, so the report is handled as never sent",
                    Path::new(&aside).display()
                );
                Ok(Record::default())
            }
            _ => Err(Stop::Refused(format!(
                "{name}: not a transaction record of this tool (cut off or changed) while the gas key has a pending transaction: nothing sent. Look at both, move the file aside, then run again ({RUNBOOK})"
            ))),
        }
    }

    /// Every version must be exactly what `gas_key` signs for this report:
    /// the book, the chain, the report's data, no value, the recorded
    /// nonce, gas and fees (the signature is deterministic, so signing the
    /// decoded fields again must give the same bytes and hash). A record
    /// that was edited, or belongs to other data, is refused: it would
    /// steer what is replaced and at what price. Times in the future are
    /// brought back to now (`Ok(true)`: the caller writes the record, or a
    /// transaction dated ahead would never count as stuck).
    fn check(
        &mut self,
        path: Option<&Path>,
        id: &Identity,
        calldata: &[u8],
        gas_key: &[u8; 32],
        now: u64,
    ) -> Result<bool, Stop> {
        let bad = |why: String| {
            Stop::Refused(format!(
                "{}: the transaction record does not match this report ({why}): nothing sent. Look at it, move it aside, then run again ({RUNBOOK})",
                path.map(|p| p.display().to_string()).unwrap_or_default()
            ))
        };
        let before = self.clone();
        if let Some((_, seen)) = &mut self.foreign {
            *seen = (*seen).min(now);
        }
        let Some(nonce) = self.nonce else {
            return Ok(*self != before);
        };
        if self.gas == 0 || self.gas > DEFAULT_MAX_GAS {
            return Err(bad(format!("gas {}", self.gas)));
        }
        let last = self.txs.len().saturating_sub(1);
        for (i, v) in self.txs.iter_mut().enumerate() {
            let n = i + 1;
            let tx =
                decode_raw(&v.raw).ok_or_else(|| bad(format!("version {n} does not decode")))?;
            let want = Tx1559 {
                chain_id: id.chain_id,
                nonce,
                max_priority_fee_per_gas: v.tip,
                max_fee_per_gas: v.max_fee,
                gas: self.gas,
                to: id.book,
                value: 0,
                data: calldata.to_vec(),
            };
            if tx != want {
                return Err(bad(format!(
                    "version {n} is another transaction than the record says (nonce, gas, fees, book, chain or data)"
                )));
            }
            if v.max_fee > MAX_FEE_CEILING || v.tip > v.max_fee {
                return Err(bad(format!(
                    "version {n}: fees {} and {}",
                    v.max_fee, v.tip
                )));
            }
            let signed = want.sign(gas_key).map(|(raw, hash)| (hx(&raw), hx(&hash)));
            if signed != Some((v.raw.to_ascii_lowercase(), v.hash.to_ascii_lowercase())) {
                return Err(bad(format!(
                    "version {n} is not this gas key's transaction, or its hash is wrong"
                )));
            }
            if !v.sent && i != last {
                return Err(bad(format!("version {n} of {} is marked unsent", last + 1)));
            }
            v.sent_unix = v.sent_unix.min(now);
        }
        if self.txs.first().is_some_and(|v| !v.sent) {
            return Err(bad("the first version is marked unsent".into()));
        }
        self.last_try_unix = self.last_try_unix.min(now);
        Ok(*self != before)
    }

    /// Write the record: a new file of this run's own name (never an
    /// existing one, never through a link), owner-only, synced, renamed
    /// over the record, and the folder synced.
    fn save(&self, path: Option<&Path>, id: &Identity) -> Result<(), Stop> {
        let Some(path) = path else {
            return Ok(());
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let mut tmp = path.as_os_str().to_os_string();
        tmp.push(format!(".new-{}-{nanos}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let text = format!("{:#}\n", self.to_json(id));
        let write = || -> std::io::Result<()> {
            let mut f = onyx_api::journal::open_plain(
                std::fs::OpenOptions::new().write(true).create_new(true),
                &tmp,
                true,
            )?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)?;
            let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
            std::fs::File::open(dir.unwrap_or(Path::new(".")))?.sync_all()
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Stop::Refused(format!(
                "{}: the transaction record could not be written: {e}",
                path.display()
            ))
        })
    }
}

/// The book's record of the report with `digest` (`report(bytes32)` at
/// `latest`), `None` when it holds none.
async fn recorded(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    digest: &Hash32,
) -> Result<Option<Vec<u8>>, Stop> {
    let mut d = keccak256(b"report(bytes32)").0[..4].to_vec();
    d.extend_from_slice(&digest.0);
    let rec = eth_call(rpc, book, &d).await.map_err(rpc_stop)?;
    Ok((rec.len() >= 32 && rec[..32] != [0u8; 32]).then_some(rec))
}

fn quantity(method: &str, v: &Value) -> Result<u128, Stop> {
    u128::from_str_radix(v.as_str().unwrap_or("").trim_start_matches("0x"), 16)
        .map_err(|_| Stop::Refused(format!("{method}: not a quantity: {v}")))
}

/// `eth_getTransactionCount` of `address` at `tag`.
async fn nonce_at(rpc: &dyn JsonRpc, address: &[u8; 20], tag: &str) -> Result<u64, Stop> {
    let v = rpc
        .call("eth_getTransactionCount", json!([hx(address), tag]))
        .await
        .map_err(rpc_stop)?;
    u64::try_from(quantity("eth_getTransactionCount", &v)?)
        .map_err(|_| Stop::Refused("eth_getTransactionCount: not a nonce".into()))
}

/// Whether a revert text names the book's `Replay()` (by name, or by the
/// four bytes of its selector in the error's data).
fn names_replay(e: &str) -> bool {
    e.contains("Replay") || e.contains(&hex::encode(&keccak256(b"Replay()").0[..4]))
}

/// A fee raised for a replacement: 12.5% and one wei more (nodes ask for
/// 10% at least).
fn bump(x: u128) -> u128 {
    x.saturating_add(x / 8).saturating_add(1)
}

/// What a node's refusal of a raw transaction says about the nonce.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// These very bytes are in its pool.
    Known,
    /// The nonce is mined.
    NonceUsed,
    /// Another transaction holds the nonce in its pool and this one does
    /// not offer enough more to replace it.
    Underpriced,
    /// Anything else.
    Other,
}

fn refusal(e: &str) -> Refusal {
    let l = e.to_ascii_lowercase();
    if l.contains("already known") {
        Refusal::Known
    } else if l.contains("nonce too low") {
        Refusal::NonceUsed
    } else if l.contains("replacement transaction underpriced") {
        Refusal::Underpriced
    } else {
        Refusal::Other
    }
}

/// A send that did not end in a receipt, as a [`Stop`].
fn send_stop(e: SendError) -> Stop {
    match e {
        SendError::Pending(_) => Stop::transient(classes::NOT_MINED, format!("postReport: {e}")),
        SendError::Busy | SendError::Stuck(_) => {
            Stop::transient(classes::EARLIER_PENDING, format!("postReport: {e}"))
        }
        SendError::Rpc(ref m) => {
            let l = m.to_ascii_lowercase();
            if l.contains("insufficient funds") || l.contains("insufficient balance") {
                return Stop::Refused(format!(
                    "postReport: {e}: the gas key holds too little test ETH (fund it, then run again)"
                ));
            }
            rpc_stop(format!("postReport: {e}"))
        }
        SendError::WouldRevert(_) | SendError::Reverted(_) => {
            Stop::Refused(format!("postReport: {e}"))
        }
    }
}

/// One run's fixed parts.
struct Run<'a, T: JsonRpc> {
    sender: &'a Sender<T>,
    book: [u8; 20],
    digest: Hash32,
    calldata: &'a [u8],
    o: &'a AnchorOpts,
    id: &'a Identity,
    gas_key: [u8; 32],
    started: Instant,
}

impl<T: JsonRpc> Run<'_, T> {
    fn path(&self) -> Option<&Path> {
        self.o.record.as_deref()
    }

    fn rpc(&self) -> &T {
        self.sender.rpc()
    }

    fn save(&self, rec: &Record) -> Result<(), Stop> {
        rec.save(self.path(), self.id)
    }

    /// Hand signed bytes to the node.
    async fn send_raw(&self, raw: &str) -> Result<(), String> {
        self.rpc()
            .call("eth_sendRawTransaction", json!([raw]))
            .await
            .map(|_| ())
    }

    /// A mined transaction: done when the book shows the record.
    async fn finish(&self, mined: Mined) -> Result<(), Stop> {
        let Some(rec) = recorded(self.rpc(), &self.book, &self.digest)
            .await?
            .filter(|rec| rec.len() >= 64)
        else {
            // A backend behind the head: the next run reads the record.
            return Err(Stop::transient(
                classes::NOT_VISIBLE,
                format!(
                    "transaction {} mined in block {} but the node does not show the book's record of the report yet",
                    mined.hash, mined.block
                ),
            ));
        };
        println!(
            "ANCHORED tx={} block={} gas={} secs={:.1} digest={} fact={}",
            mined.hash,
            mined.block,
            mined.gas_used,
            self.started.elapsed().as_secs_f64(),
            self.digest,
            Hash32(rec[32..64].try_into().map_err(|_| "fact")?)
        );
        Ok(())
    }

    /// The receipt of a recorded version, read strictly (RETRY-R2-L1): a
    /// receipt that cannot be read is an error, never "no receipt". `Ok` of
    /// `Err(hash)` is a version that mined and reverted.
    async fn receipt(&self, rec: &Record) -> Result<Option<Result<Mined, String>>, Stop> {
        for hash in rec.hashes() {
            let r = self
                .rpc()
                .call("eth_getTransactionReceipt", json!([hash]))
                .await
                .map_err(rpc_stop)?;
            if r.is_null() {
                continue;
            }
            if r["status"] != "0x1" {
                return Ok(Some(Err(hash)));
            }
            let num = |k: &str| quantity("eth_getTransactionReceipt", &r[k]);
            return Ok(Some(Ok(Mined {
                block: u64::try_from(num("blockNumber")?).unwrap_or(u64::MAX),
                gas_used: u64::try_from(num("gasUsed")?).unwrap_or(u64::MAX),
                effective_gas_price: num("effectiveGasPrice").unwrap_or(0),
                hash,
            })));
        }
        Ok(None)
    }

    /// Wait for a version of the recorded transaction.
    async fn wait(&self, rec: &mut Record) -> Result<(), Stop> {
        match self.sender.wait_mined(&rec.hashes(), self.o.deadline).await {
            Ok(Some((_, mined))) => self.finish(mined).await,
            Ok(None) => Err(send_stop(SendError::Pending(
                rec.txs.last().map(|v| v.hash.clone()).unwrap_or_default(),
            ))),
            Err(SendError::Reverted(h)) => {
                self.after_revert(rec, format!("transaction {h} reverted"), None)
                    .await
            }
            Err(e) => Err(send_stop(e)),
        }
    }

    /// The book refused the report, at an estimate (`estimate` is its
    /// text) or mined. Done when the book shows the record (an earlier
    /// transaction made it); else by the reason.
    async fn after_revert(
        &self,
        rec: &mut Record,
        what: String,
        estimate: Option<&str>,
    ) -> Result<(), Stop> {
        if recorded(self.rpc(), &self.book, &self.digest)
            .await?
            .is_some()
        {
            println!(
                "ANCHORED already digest={} (an earlier transaction recorded it; {what})",
                self.digest
            );
            return Ok(());
        }
        let behind = |why: &str| {
            Stop::transient(
                classes::NOT_VISIBLE,
                format!("postReport: {what}: {why}, but the node does not show the record yet"),
            )
        };
        if let Some(e) = estimate {
            if names_replay(e) {
                return Err(behind("the book says it holds the report (Replay)"));
            }
            return Err(format!("postReport: {what}").into());
        }
        // Mined and reverted: a receipt has no reason, so ask the book.
        match self.sender.call(&self.book, self.calldata).await {
            Err(e) if names_replay(&e) => Err(behind("the book says it holds the report (Replay)")),
            Err(e) if is_revert(&e) => {
                Err(format!("postReport: {what}: the book refuses the report: {e}").into())
            }
            Err(e) => Err(rpc_stop(e)),
            Ok(_) => {
                // No reason now (out of gas, or a backend behind both the
                // record and the revert): read once more, then refuse.
                rec.reverts += 1;
                self.save(rec)?;
                if self.path().is_none() || rec.reverts >= 2 {
                    return Err(format!(
                        "postReport: {what} and the book gives no reason: not sent again. Look at the transaction; to send the report anew, move the record {} aside ({RUNBOOK})",
                        self.path().map(|p| p.display().to_string()).unwrap_or_default()
                    )
                    .into());
                }
                Err(behind("the book gives no reason now"))
            }
        }
    }

    /// Whether the transaction pending at the recorded nonce `n` is this
    /// record's (RETRY-R2-M3). `shown`: the node counts a pending
    /// transaction of the key. `Ok(false)`: another's.
    async fn ours(&self, rec: &mut Record, n: u64, shown: bool) -> Result<bool, Stop> {
        for i in (0..rec.txs.len()).rev() {
            let t = self
                .rpc()
                .call("eth_getTransactionByHash", json!([rec.txs[i].hash]))
                .await
                .map_err(rpc_stop)?;
            if t.is_null() {
                continue;
            }
            if !t["blockNumber"].is_null() {
                // Mined, though the nonce read says not yet: a read behind.
                return Err(Stop::transient(
                    classes::NOT_VISIBLE,
                    format!(
                        "transaction {} is mined but the node's nonce and record reads do not show it yet",
                        rec.txs[i].hash
                    ),
                ));
            }
            if !rec.txs[i].sent {
                // Sent before a stop: the node has it.
                rec.txs[i].sent = true;
                rec.txs[i].sent_unix = self.o.now_unix;
                rec.underpriced = 0;
                self.save(rec)?;
            }
            return Ok(true);
        }
        if shown {
            return Ok(false);
        }
        // No pending transaction shown, none of ours known: dropped, or a
        // node that does not show its pool. The newest version a node took
        // goes back as it was signed (no fee raised); the answer tells.
        let Some(i) = rec.txs.iter().rposition(|v| v.sent) else {
            return Ok(false);
        };
        let v = rec.txs[i].clone();
        match self.send_raw(&v.raw).await {
            Ok(()) => {
                eprintln!(
                    "postReport: this report's transaction {} (nonce {n}) was not in the node's pool: handed back as signed",
                    v.hash
                );
                // Taken anew: it gets its time to be mined again.
                rec.txs[i].sent_unix = self.o.now_unix;
                self.save(rec)?;
                Ok(true)
            }
            Err(e) => match refusal(&e) {
                Refusal::Known => Ok(true),
                Refusal::NonceUsed => Err(rpc_stop(format!("postReport: rpc: {e}"))),
                // Something else holds the nonce in the pool: the unsent
                // newer version of this record, if the node knows its
                // bytes; else another's transaction.
                Refusal::Underpriced => match rec.txs.last().filter(|u| !u.sent).cloned() {
                    Some(u) => match self.send_raw(&u.raw).await.map_err(|e| refusal(&e)) {
                        Ok(()) | Err(Refusal::Known) => {
                            if let Some(last) = rec.txs.last_mut() {
                                last.sent = true;
                                last.sent_unix = self.o.now_unix;
                            }
                            rec.underpriced = 0;
                            self.save(rec)?;
                            Ok(true)
                        }
                        Err(_) => Ok(false),
                    },
                    None => Ok(false),
                },
                Refusal::Other => Err(rpc_stop(format!("postReport: rpc: {e}"))),
            },
        }
    }

    /// Send the newest version, which the record already holds as unsent
    /// (RETRY-R2-M2), and note what the node said. `Ok`: a node has it.
    async fn send_newest(&self, rec: &mut Record, n: u64) -> Result<(), Stop> {
        let Some(v) = rec.txs.last().cloned() else {
            return Err("the transaction record holds a nonce and no transaction".into());
        };
        let now = self.o.now_unix;
        rec.last_try_unix = now;
        let answer = self.send_raw(&v.raw).await;
        let said = answer.as_ref().err().map(|e| refusal(e));
        match (answer, said) {
            // Its own bytes "already known": sent before a stop.
            (Ok(()), _) | (Err(_), Some(Refusal::Known)) => {
                if let Some(last) = rec.txs.last_mut() {
                    last.sent = true;
                    last.sent_unix = now;
                }
                rec.underpriced = 0;
                self.save(rec)?;
                eprintln!(
                    "postReport: this report's transaction at nonce {n} is replaced by {} ({} wei per gas; raise {} of {})",
                    v.hash,
                    v.max_fee,
                    rec.raises(),
                    self.o.max_bumps
                );
                Ok(())
            }
            (Err(e), Some(Refusal::Underpriced)) => {
                // The node wants more: this version is dropped (it was
                // never in a pool) and the next one raises the fees again.
                rec.txs.pop();
                rec.underpriced = rec.underpriced.saturating_add(1);
                self.save(rec)?;
                Err(rpc_stop(format!(
                    "postReport: rpc: {e} (the replacement of nonce {n}: not counted as a raise; the next try, in {} s at the earliest, raises the fees more)",
                    self.o.gap()
                )))
            }
            (Err(e), _) => {
                // Kept as unsent: the same bytes go out at the next try.
                self.save(rec)?;
                Err(rpc_stop(format!(
                    "postReport: rpc: {e} (the replacement of nonce {n} did not reach the node: not counted as a raise; tried again in {} s at the earliest)",
                    self.o.gap()
                )))
            }
        }
    }

    /// This report's own transaction holds the key's next nonce `n`,
    /// unmined: wait for it, or replace it once it is stuck.
    async fn own_pending(&self, rec: &mut Record, n: u64) -> Result<(), Stop> {
        let Some(last) = rec.txs.last().cloned() else {
            return Err("the transaction record holds a nonce and no transaction".into());
        };
        let now = self.o.now_unix;
        let since_try = now.saturating_sub(rec.last_try_unix);
        if !last.sent {
            // Signed and written, not known to be sent (RETRY-R2-M2).
            if since_try >= self.o.gap() {
                self.send_newest(rec, n).await?;
            } else {
                eprintln!(
                    "postReport: the replacement at nonce {n} did not reach the node {since_try} s ago: tried again after {} s",
                    self.o.gap()
                );
            }
            return self.wait(rec).await;
        }
        let age = now.saturating_sub(last.sent_unix);
        if age < self.o.stuck.as_secs() {
            eprintln!(
                "postReport: this report's transaction {} (nonce {n}) was sent {age} s ago and is not mined: waiting for it (replaced with a higher fee after {} s)",
                last.hash,
                self.o.stuck.as_secs()
            );
            return self.wait(rec).await;
        }
        let raises = rec.raises();
        if raises >= usize::try_from(self.o.max_bumps).unwrap_or(usize::MAX) {
            return Err(format!(
                "this report's transaction (nonce {n}, newest version {}, {} versions) is still unmined {age} s after its last send and its fee was raised {raises} times (--max-bumps): not raised again. A person decides: {RUNBOOK}",
                last.hash,
                rec.txs.len(),
            )
            .into());
        }
        if since_try < self.o.gap() {
            eprintln!(
                "postReport: a replacement at nonce {n} was tried {since_try} s ago: the next try comes after {} s",
                self.o.gap()
            );
            return self.wait(rec).await;
        }
        // Would the book still take it? A report that went stale, or one
        // recorded meanwhile, is not worth a higher fee.
        if let Err(e) = self.sender.call(&self.book, self.calldata).await {
            if is_revert(&e) {
                return self
                    .after_revert(rec, format!("would revert, not replaced: {e}"), Some(&e))
                    .await;
            }
            return Err(rpc_stop(e));
        }
        let block = self
            .rpc()
            .call("eth_getBlockByNumber", json!(["latest", false]))
            .await
            .map_err(rpc_stop)?;
        let base = quantity("eth_getBlockByNumber", &block["baseFeePerGas"])?;
        let market_tip = match self.rpc().call("eth_maxPriorityFeePerGas", json!([])).await {
            Ok(t) => quantity("eth_maxPriorityFeePerGas", &t)?,
            Err(_) => 1_000_000,
        };
        let cap = self.o.max_fee_wei;
        // One raise, and one more for each replacement a node called
        // underpriced since the last one it took.
        let raise = |x: u128| (0..=rec.underpriced).fold(x, |x, _| bump(x));
        let floor = raise(last.max_fee);
        rec.last_try_unix = now;
        if floor > cap {
            // Not a raise and not a refusal: a fee spike passes, and the
            // report itself is refused by the book once it is stale.
            self.save(rec)?;
            return Err(Stop::transient(
                classes::FEE_CAP,
                format!(
                    "this report's transaction (nonce {n}) is stuck at {} wei per gas and a replacement needs {floor}, above the fee cap {cap} (--max-fee-wei): not replaced; tried again in {} s at the earliest",
                    last.max_fee,
                    self.o.gap()
                ),
            ));
        }
        let max_fee = floor
            .max(base.saturating_mul(2).saturating_add(market_tip))
            .min(cap);
        let tip = raise(last.tip).max(market_tip).min(max_fee);
        let tx = Tx1559 {
            chain_id: self.id.chain_id,
            nonce: n,
            max_priority_fee_per_gas: tip,
            max_fee_per_gas: max_fee,
            gas: rec.gas,
            to: self.book,
            value: 0,
            data: self.calldata.to_vec(),
        };
        let (raw, hash) = tx.sign(&self.gas_key).ok_or("signing failed")?;
        // Written before it is sent: a stop between the two is found again
        // (the node's "already known" for these bytes).
        rec.txs.push(Version {
            hash: hx(&hash),
            max_fee,
            tip,
            raw: hx(&raw),
            sent_unix: now,
            sent: false,
        });
        self.save(rec)?;
        eprintln!(
            "postReport: this report's transaction {} at nonce {n} ({} wei per gas) is unmined {age} s after it was sent: replacing it",
            last.hash, last.max_fee
        );
        self.send_newest(rec, n).await?;
        self.wait(rec).await
    }

    /// An unmined transaction of the gas key at `nonce` that the record
    /// does not hold: never replaced. Transient while seen for less than
    /// `--stuck-secs`, then refused.
    async fn foreign(&self, rec: &mut Record, nonce: u64) -> Result<(), Stop> {
        let now = self.o.now_unix;
        let seen = match rec.foreign {
            Some((n, t)) if n == nonce => t,
            _ => {
                rec.foreign = Some((nonce, now));
                self.save(rec)?;
                now
            }
        };
        let (age, bound) = (now.saturating_sub(seen), self.o.stuck.as_secs());
        if self.path().is_some() && age >= bound {
            return Err(format!(
                "an unmined transaction of the gas key {} holds nonce {nonce} and this tool did not send it (seen for {age} s): it is never replaced here and nothing was sent. A person looks it up, then lets it mine, or replaces or cancels it at nonce {nonce}: {RUNBOOK}",
                hx(&self.id.from)
            )
            .into());
        }
        Err(Stop::transient(
            classes::EARLIER_PENDING,
            format!(
                "an earlier transaction of the gas key (nonce {nonce}) is not mined yet and is not this tool's: nothing sent (seen for {age} s; refused after {bound} s)"
            ),
        ))
    }

    /// Send the report as a new transaction and write the record.
    async fn fresh(&self, rec: &mut Record) -> Result<(), Stop> {
        let sent = match self
            .sender
            .broadcast(&self.book, self.calldata, 0, self.o.deadline / 2)
            .await
        {
            Ok(s) => s,
            Err(SendError::WouldRevert(e)) => {
                return self
                    .after_revert(rec, format!("would revert, not sent: {e}"), Some(&e))
                    .await
            }
            Err(e) => return Err(send_stop(e)),
        };
        *rec = Record {
            nonce: Some(sent.tx.nonce),
            gas: sent.tx.gas,
            txs: vec![Version {
                hash: sent.hash.clone(),
                max_fee: sent.tx.max_fee_per_gas,
                tip: sent.tx.max_priority_fee_per_gas,
                raw: hx(&sent.raw),
                sent_unix: self.o.now_unix,
                sent: true,
            }],
            ..Record::default()
        };
        if let Err(e) = self.save(rec) {
            // Sent already: wait for it all the same. A later run treats
            // it as a transaction it did not send, and never replaces it.
            eprintln!(
                "postReport: transaction {} (nonce {}) is sent but its record could not be written: {}",
                sent.hash,
                sent.tx.nonce,
                e.msg()
            );
        }
        self.wait(rec).await
    }
}

/// Send `r` to the book with `postReport` unless it is recorded already;
/// refuse when the key used its sequence for another payload (`conflict`).
/// The module's header says what a second run does.
pub(crate) async fn post_report<T: JsonRpc>(
    rpc: T,
    chain_id: u64,
    book: &[u8; 20],
    gas: [u8; 32],
    r: &SignedReport,
    conflict: &str,
    o: &AnchorOpts,
) -> Result<(), Stop> {
    let book = *book;
    let digest = r.payload.digest();
    // Already recorded (a re-run): nothing to send.
    if recorded(&rpc, &book, &digest).await?.is_some() {
        println!("ANCHORED already digest={digest}");
        return Ok(());
    }
    // Never sign two payloads under one sequence: the book would suspend
    // the key (ADR-0028 equivocation).
    let mut d = keccak256(b"sequenceOf(bytes32,uint64)").0[..4].to_vec();
    d.extend_from_slice(&r.payload.key.0);
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&r.payload.sequence.to_be_bytes());
    d.extend_from_slice(&w);
    let used = eth_call(&rpc, &book, &d).await.map_err(rpc_stop)?;
    if used.len() >= 32 && used[..32] != [0u8; 32] && used[..32] != digest.0 {
        return Err(conflict.into());
    }
    let payload = borsh::to_vec(&r.payload).map_err(|e| e.to_string())?;
    let calldata = post_report_calldata(&payload, &r.signature);
    let sender = Sender::new(rpc, chain_id, gas)
        .ok_or("--gas-key holds no valid key")?
        .with_poll(o.poll)
        .with_fee_limits(o.max_fee_wei, DEFAULT_MAX_GAS);
    let id = Identity {
        digest,
        chain_id,
        book,
        from: sender.address(),
    };
    let pending = nonce_at(sender.rpc(), &id.from, "pending").await?;
    let latest = nonce_at(sender.rpc(), &id.from, "latest").await?;
    let mut rec = Record::load(o.record.as_deref(), &id, pending <= latest, o.now_unix)?;
    if rec.check(o.record.as_deref(), &id, &calldata, &gas, o.now_unix)? {
        rec.save(o.record.as_deref(), &id)?;
    }
    let run = Run {
        sender: &sender,
        book,
        digest,
        calldata: &calldata,
        o,
        id: &id,
        gas_key: gas,
        started: Instant::now(),
    };
    if let Some(n) = rec.nonce {
        if latest == n {
            return if run.ours(&mut rec, n, pending > latest).await? {
                run.own_pending(&mut rec, n).await
            } else {
                run.foreign(&mut rec, n).await
            };
        }
        if latest < n {
            // A nonce before ours is unmined: not ours to replace.
            return run.foreign(&mut rec, latest).await;
        }
        // The nonce is used up: by a version of ours, or by another
        // transaction of the key.
        match run.receipt(&rec).await? {
            Some(Ok(mined)) => return run.finish(mined).await,
            Some(Err(h)) => {
                return run
                    .after_revert(&mut rec, format!("transaction {h} reverted"), None)
                    .await
            }
            None => {
                eprintln!(
                    "postReport: nonce {n} of the gas key was used by another transaction: this report's earlier transaction can no longer be mined; sending the report anew"
                );
                rec = Record::default();
                run.save(&rec)?;
            }
        }
    }
    if pending > latest {
        return run.foreign(&mut rec, latest).await;
    }
    if rec.foreign.take().is_some() {
        run.save(&rec)?;
    }
    run.fresh(&mut rec).await
}
