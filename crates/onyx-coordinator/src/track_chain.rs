//! The track-record intake's chain record over Foundry's `cast` (ADR-0036
//! amendment 1 item 10, ROADMAP C5): a [`FinalizedRecord`] whose posts are
//! transactions from a dedicated poster account to a sink address, each
//! carrying one batch header's hash as its 32-byte calldata. No contract:
//! the C2 signed-source book can carry the same headers later behind the
//! same trait.
//!
//! What makes it a record and not the coordinator's word:
//! - a post counts only once its block is at or below the finalized block
//!   (`finalized`, or a pinned depth on a local anvil, as [`Finality`]);
//! - each post's transaction must come from the poster, go to the sink, carry
//!   exactly the header hash and have nonce `first_nonce + seq`;
//! - the poster's nonce at the head must equal `first_nonce + posts`, so a
//!   post the intake does not show (a hedge) fails every read.
//!
//! ROADMAP C7 (ADR-0036 amendment 3): [`CastFacts`] reads the facts a
//! track commitment is resolved from (`OnyxSignedBook` level-3 books and
//! operators, `OnyxRegistry` level-2 observations) at one pinned finalized
//! head, and [`CommitmentLogCast`] posts commitments to the book's append-only
//! log and reads its latest.
//!
//! ROADMAP C20 (ADR-0036 amendment 5): [`write_publication`] writes the
//! coordinator's [`Publication`] where a node reads it
//! (`ONYX_TRACK_PUBLICATIONS`), checked and never rewriting what it wrote.
//!
//! Local only for now: an unlocked anvil account posts (no key is read or
//! passed). `cast` runs outside the repo (Foundry loads a `.env` from its
//! working directory, M8-L55-F4). Nothing here touches proof levels or money
//! (ADR-0036 item 12, `scripts/check-boundaries.sh`).

use crate::cast_chain::{is_loopback_rpc, Finality};
use onyx_core::{keccak256, Hash32, MetricId, SignedPayload, SignedReport, SourceId, Subject};
use onyx_track::chain::{
    ChainView, CommitmentLog, FactChain, LogHead, Recorded, Registered, Settled,
};
use onyx_track::intake::{Anchor, FinalizedRecord, IntakeId, KeyBook, KeyLife, KeyLives, Post};
use onyx_track::publication::Publication;
use onyx_track::TrackCommitment;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The intake's posts on an EVM chain, read and written through `cast`.
#[derive(Clone, Debug)]
pub struct CastRecord {
    /// The `cast` binary.
    pub cast: PathBuf,
    /// JSON-RPC URL.
    pub rpc: String,
    /// `cast` runs here, outside the repo.
    pub work: PathBuf,
    /// Chain id, sink, poster and the poster's first nonce.
    pub id: IntakeId,
    /// Which block is final.
    pub finality: Finality,
    /// Each post's transaction hash, in order (published with the batches).
    pub txs: Vec<Hash32>,
}

fn hex20(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn hex32(h: &Hash32) -> String {
    format!("0x{}", hex::encode(h.0))
}

fn quantity(v: &serde_json::Value, k: &str) -> Result<u64, String> {
    v[k].as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| format!("no {k} in {v}"))
}

fn bytes_field<const N: usize>(v: &serde_json::Value, k: &str) -> Result<[u8; N], String> {
    let s = v[k].as_str().ok_or_else(|| format!("no {k} in {v}"))?;
    let mut out = [0u8; N];
    hex::decode_to_slice(s.trim_start_matches("0x"), &mut out)
        .map_err(|e| format!("{k} is not {N} bytes: {s} ({e})"))?;
    Ok(out)
}

/// A block from `eth_getBlockByNumber`.
pub fn parse_block(v: &serde_json::Value) -> Result<Anchor, String> {
    Ok(Anchor {
        block: quantity(v, "number")?,
        block_hash: Hash32(bytes_field::<32>(v, "hash")?),
        timestamp: quantity(v, "timestamp")?,
    })
}

/// A post's transaction from `eth_getTransactionByHash`: its block number
/// (`None` while pending) after checking sender, recipient, nonce and data
/// against post `seq` of `header_hash`.
pub fn check_post_tx(
    v: &serde_json::Value,
    id: &IntakeId,
    seq: u64,
    header_hash: Option<&Hash32>,
) -> Result<(Option<u64>, Hash32), String> {
    let from: [u8; 20] = bytes_field(v, "from")?;
    let to: [u8; 20] = bytes_field(v, "to")?;
    let data: [u8; 32] = bytes_field(v, "input")?;
    let nonce = quantity(v, "nonce")?;
    if from != id.poster || to != id.sink {
        return Err(format!("post {seq}: not poster -> sink"));
    }
    if Some(nonce) != id.first_nonce.checked_add(seq) {
        return Err(format!(
            "post {seq}: nonce {nonce} is not first_nonce + seq"
        ));
    }
    if header_hash.is_some_and(|h| h.0 != data) {
        return Err(format!("post {seq}: data is not the header hash"));
    }
    let block = match &v["blockNumber"] {
        serde_json::Value::Null => None,
        _ => Some(quantity(v, "blockNumber")?),
    };
    Ok((block, Hash32(data)))
}

/// May a record read `chain` (the node's `eth_chainId`) at `finality` from
/// `rpc` for intake `id`? The chain id must be the intake's, and a pinned
/// depth is for a loopback anvil only (review TR-R3-4): a remote node must
/// answer for its own `finalized` block.
pub fn check_head_source(
    id: &IntakeId,
    rpc: &str,
    finality: Finality,
    chain: &serde_json::Value,
) -> Result<(), String> {
    let chain_id = chain
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| format!("eth_chainId: {chain}"))?;
    if chain_id != id.chain_id {
        return Err(format!(
            "chain id {chain_id} is not the intake's {}",
            id.chain_id
        ));
    }
    if matches!(finality, Finality::Depth(_)) && !is_loopback_rpc(rpc) {
        return Err("a pinned block depth is for a local anvil only; use finalized".into());
    }
    Ok(())
}

impl CastRecord {
    /// Run `cast` with `args` against the node.
    pub fn run(&self, args: &[String]) -> Result<String, String> {
        let out = Command::new(&self.cast)
            .current_dir(&self.work)
            .args(args)
            .env("ETH_RPC_URL", &self.rpc)
            .env_remove("ONYX_DEPLOYER_KEY")
            .env_remove("BASE_SEPOLIA_PRIVATE_KEY")
            .output()
            .map_err(|e| format!("cast: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cast {}: {} {}",
                args.first().map_or("", String::as_str),
                String::from_utf8_lossy(&out.stderr).trim(),
                String::from_utf8_lossy(&out.stdout).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// One JSON-RPC call through `cast rpc`.
    pub fn rpc(&self, method: &str, params: &[String]) -> Result<serde_json::Value, String> {
        let mut a = vec!["rpc".to_string(), method.to_string()];
        a.extend(params.iter().cloned());
        let out = self.run(&a)?;
        serde_json::from_str(&out).map_err(|e| format!("{method}: {e}: {out}"))
    }

    /// The block at `tag` (`"latest"`, `"finalized"`, or a hex number).
    pub fn block(&self, tag: &str) -> Result<Anchor, String> {
        parse_block(&self.rpc(
            "eth_getBlockByNumber",
            &[format!("\"{tag}\""), "false".into()],
        )?)
    }

    /// The poster's nonce at `block`.
    pub fn nonce_at(&self, block: u64) -> Result<u64, String> {
        let v = self.rpc(
            "eth_getTransactionCount",
            &[
                format!("\"{}\"", hex20(&self.id.poster)),
                format!("\"0x{block:x}\""),
            ],
        )?;
        v.as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| format!("eth_getTransactionCount: {v}"))
    }

    /// Post `header_hash` as the next post (from the unlocked poster); returns
    /// the transaction hash and records it. Refused unless the transaction
    /// landed with nonce `first_nonce + seq`.
    pub fn post(&mut self, header_hash: &Hash32) -> Result<Hash32, String> {
        let seq = self.txs.len() as u64;
        let out = self.run(&[
            "send".into(),
            "--unlocked".into(),
            "--from".into(),
            hex20(&self.id.poster),
            hex20(&self.id.sink),
            hex32(header_hash),
            "--json".into(),
        ])?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        if v["status"].as_str() != Some("0x1") {
            return Err(format!("post {seq} reverted"));
        }
        let tx = Hash32(bytes_field::<32>(&v, "transactionHash")?);
        let t = self.rpc("eth_getTransactionByHash", &[format!("\"{}\"", hex32(&tx))])?;
        check_post_tx(&t, &self.id, seq, Some(header_hash))?;
        self.txs.push(tx);
        Ok(tx)
    }
}

impl FinalizedRecord for CastRecord {
    fn head(&self) -> Result<Anchor, String> {
        check_head_source(
            &self.id,
            &self.rpc,
            self.finality,
            &self.rpc("eth_chainId", &[])?,
        )?;
        match self.finality {
            Finality::Finalized => self.block("finalized"),
            // An OP stack's `safe` block is not final (review R7).
            Finality::Safe => Err("a track intake reads only a final block: use finalized".into()),
            Finality::Depth(d) => {
                let latest = self.block("latest")?;
                self.block(&format!("0x{:x}", latest.block.saturating_sub(d)))
            }
        }
    }

    fn posts(&self, head: &Anchor) -> Result<Vec<Post>, String> {
        let mut posts = Vec::new();
        for (seq, tx) in self.txs.iter().enumerate() {
            let seq = seq as u64;
            let v = self.rpc("eth_getTransactionByHash", &[format!("\"{}\"", hex32(tx))])?;
            let (block, data) = check_post_tx(&v, &self.id, seq, None)?;
            match block {
                Some(b) if b <= head.block => {
                    if posts.len() as u64 != seq {
                        return Err(format!("post {seq} final before an earlier one"));
                    }
                    let anchor = self.block(&format!("0x{b:x}"))?;
                    let block_hash: [u8; 32] = bytes_field(&v, "blockHash")?;
                    if anchor.block_hash.0 != block_hash {
                        return Err(format!("post {seq}: block {b} was replaced"));
                    }
                    posts.push(Post {
                        seq,
                        header_hash: data,
                        anchor,
                    });
                }
                _ => {}
            }
        }
        let count = self.nonce_at(head.block)?;
        if Some(count) != self.id.first_nonce.checked_add(posts.len() as u64) {
            return Err(format!(
                "hidden post: the poster's nonce at block {} is {count}, the intake shows {} posts from nonce {}",
                head.block,
                posts.len(),
                self.id.first_nonce
            ));
        }
        Ok(posts)
    }
}

fn run_cast(cast: &PathBuf, work: &PathBuf, rpc: &str, args: &[String]) -> Result<String, String> {
    let out = Command::new(cast)
        .current_dir(work)
        .args(args)
        .env("ETH_RPC_URL", rpc)
        .env_remove("ONYX_DEPLOYER_KEY")
        .env_remove("BASE_SEPOLIA_PRIVATE_KEY")
        .output()
        .map_err(|e| format!("cast: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cast {}: {} {}",
            args.first().map_or("", String::as_str),
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn rpc_json(
    cast: &PathBuf,
    work: &PathBuf,
    rpc: &str,
    method: &str,
    params: &[String],
) -> Result<serde_json::Value, String> {
    let mut a = vec!["rpc".to_string(), method.to_string()];
    a.extend(params.iter().cloned());
    let out = run_cast(cast, work, rpc, &a)?;
    serde_json::from_str(&out).map_err(|e| format!("{method}: {e}: {out}"))
}

/// One `bytes32` line of `cast call` output.
pub fn parse_word(s: &str) -> Result<Hash32, String> {
    let mut a = [0u8; 32];
    hex::decode_to_slice(s.trim().trim_matches('"').trim_start_matches("0x"), &mut a)
        .map_err(|e| format!("not a 32-byte word: {s} ({e})"))?;
    Ok(Hash32(a))
}

/// One integer line of `cast call` output (`123` or `123 [1.23e2]`).
pub fn parse_uint(s: &str) -> Result<u64, String> {
    s.split_whitespace()
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or_else(|| format!("not an integer: {s}"))
}

fn lines(out: &str) -> Vec<&str> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

/// The `bytes` or `string` argument whose head word is at `word` of ABI data
/// (offset word, then length and bytes), bounds-checked.
pub fn abi_dynamic(data: &[u8], word: usize) -> Result<Vec<u8>, String> {
    let at = |i: usize| -> Result<usize, String> {
        let w = data
            .get(i..i + 32)
            .ok_or_else(|| format!("abi: no word at {i}"))?;
        if w[..24].iter().any(|b| *b != 0) {
            return Err("abi: word too large".into());
        }
        let mut b = [0u8; 8];
        b.copy_from_slice(&w[24..]);
        usize::try_from(u64::from_be_bytes(b)).map_err(|e| e.to_string())
    };
    let off = at(word * 32)?;
    let len = at(off)?;
    data.get(off + 32..off + 32 + len)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "abi: dynamic value out of bounds".into())
}

/// The report of a `postReport(bytes payload, bytes signature)` call's input
/// (selector, then the two dynamic arguments).
pub fn report_from_calldata(input: &[u8]) -> Result<SignedReport, String> {
    let selector = &keccak256(b"postReport(bytes,bytes)").0[..4];
    if input.get(..4) != Some(selector) {
        return Err("not a direct postReport call".into());
    }
    let args = &input[4..];
    let payload: SignedPayload =
        borsh::from_slice(&abi_dynamic(args, 0)?).map_err(|e| format!("payload: {e}"))?;
    let signature: [u8; 65] = abi_dynamic(args, 1)?
        .try_into()
        .map_err(|_| "signature is not 65 bytes".to_string())?;
    Ok(SignedReport { payload, signature })
}

/// A finalized head on `chain`'s chain id, read from `rpc` at `finality` (a
/// pinned depth only on a loopback anvil; review TR-R3-4's rule).
fn pinned_head(
    cast: &PathBuf,
    work: &PathBuf,
    rpc: &str,
    chain_id: u64,
    finality: Finality,
) -> Result<Anchor, String> {
    let v = rpc_json(cast, work, rpc, "eth_chainId", &[])?;
    let id = v
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| format!("eth_chainId: {v}"))?;
    if id != chain_id {
        return Err(format!("chain id {id} is not the configured {chain_id}"));
    }
    let block = |tag: &str| {
        rpc_json(
            cast,
            work,
            rpc,
            "eth_getBlockByNumber",
            &[format!("\"{tag}\""), "false".into()],
        )
        .and_then(|v| parse_block(&v))
    };
    match finality {
        Finality::Finalized => block("finalized"),
        Finality::Safe => Err("track reads take only a final block: use finalized".into()),
        Finality::Depth(d) => {
            if !is_loopback_rpc(rpc) {
                return Err("a pinned block depth is for a local anvil only; use finalized".into());
            }
            let latest = block("latest")?;
            block(&format!("0x{:x}", latest.block.saturating_sub(d)))
        }
    }
}

/// The facts of one [`FactChain`] at one pinned finalized head, read through
/// `cast` (ADR-0036 amendment 3 item 3). Contract reads name the head by its
/// block hash (EIP-1898), so a reorged-out head fails instead of reading
/// other state; log scans run to the head's number and [`CastFacts::recheck`]
/// confirms the head is still canonical after them.
#[derive(Clone, Debug)]
pub struct CastFacts {
    /// The `cast` binary.
    pub cast: PathBuf,
    /// JSON-RPC URL (the client's own node).
    pub rpc: String,
    /// `cast` runs here, outside the repo.
    pub work: PathBuf,
    /// The chain and its contracts.
    pub chain: FactChain,
    /// The pinned head.
    pub head: Anchor,
}

impl CastFacts {
    /// Pin the finalized head of `chain` (refused on another chain id).
    pub fn pin(
        cast: PathBuf,
        rpc: String,
        work: PathBuf,
        chain: FactChain,
        finality: Finality,
    ) -> Result<Self, String> {
        let head = pinned_head(&cast, &work, &rpc, chain.chain_id, finality)?;
        Ok(CastFacts {
            cast,
            rpc,
            work,
            chain,
            head,
        })
    }

    /// The view a client checks commitment `as_of` with: block `number` (the
    /// `head` of its `TrackCommitted` event), at or below the finalized head,
    /// whose timestamp is `as_of` (review TR-C7-R1-9).
    pub fn at_commitment(
        cast: PathBuf,
        rpc: String,
        work: PathBuf,
        chain: FactChain,
        finality: Finality,
        number: u64,
        as_of: u64,
    ) -> Result<Self, String> {
        let mut v = Self::pin(cast, rpc, work, chain, finality)?;
        if number > v.head.block {
            return Err(format!(
                "block {number} is not final (finalized head {})",
                v.head.block
            ));
        }
        let b = parse_block(&rpc_json(
            &v.cast,
            &v.work,
            &v.rpc,
            "eth_getBlockByNumber",
            &[format!("\"0x{number:x}\""), "false".into()],
        )?)?;
        if b.timestamp != as_of {
            return Err(format!(
                "block {number} has time {}, the commitment's as_of is {as_of}",
                b.timestamp
            ));
        }
        v.head = b;
        Ok(v)
    }

    fn call(&self, to: &[u8; 20], sig: &str, args: &[String]) -> Result<String, String> {
        let mut a = vec![
            "call".to_string(),
            "--block".into(),
            hex32(&self.head.block_hash),
            hex20(to),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        run_cast(&self.cast, &self.work, &self.rpc, &a)
    }

    fn logs(
        &self,
        address: &[u8; 20],
        topics: &[Option<Hash32>],
    ) -> Result<Vec<serde_json::Value>, String> {
        let t: Vec<serde_json::Value> = topics
            .iter()
            .map(|x| x.map_or(serde_json::Value::Null, |h| hex32(&h).into()))
            .collect();
        let filter = serde_json::json!({
            "address": hex20(address),
            "fromBlock": "0x0",
            "toBlock": format!("0x{:x}", self.head.block),
            "topics": t,
        });
        let v = rpc_json(
            &self.cast,
            &self.work,
            &self.rpc,
            "eth_getLogs",
            &[filter.to_string()],
        )?;
        let out = v
            .as_array()
            .cloned()
            .ok_or_else(|| format!("eth_getLogs: {v}"))?;
        self.recheck()?;
        Ok(out)
    }

    /// The pinned head is still the canonical block at its number (a reorg
    /// below a "finalized" head is refused, never read through).
    pub fn recheck(&self) -> Result<(), String> {
        let v = rpc_json(
            &self.cast,
            &self.work,
            &self.rpc,
            "eth_getBlockByNumber",
            &[format!("\"0x{:x}\"", self.head.block), "false".into()],
        )?;
        if parse_block(&v)? != self.head {
            return Err(format!("block {} was replaced (reorg)", self.head.block));
        }
        Ok(())
    }

    /// The level-3 status of every key of `book` at the end of `post`'s block
    /// (`OnyxSignedBook.keyState`, the block named by hash; ADR-0036
    /// amendment 4 item 3, THREATS TR-R4-4). The post must be at or below the
    /// pinned head. A client passes one per post to `verify_log`.
    pub fn key_lives_at(&self, post: &Anchor, book: &KeyBook) -> Result<KeyLives, String> {
        if post.block > self.head.block {
            return Err(format!(
                "post block {} is above the pinned head {}",
                post.block, self.head.block
            ));
        }
        // Review TR-C9-R1-9: the post's hash must be the canonical block at
        // its number (a served anchor could name a stale fork).
        let canonical = parse_block(&rpc_json(
            &self.cast,
            &self.work,
            &self.rpc,
            "eth_getBlockByNumber",
            &[format!("\"0x{:x}\"", post.block), "false".into()],
        )?)?;
        check_canonical(post, &canonical)?;
        let mut keys = Vec::with_capacity(book.keys.len());
        for k in &book.keys {
            let out = run_cast(
                &self.cast,
                &self.work,
                &self.rpc,
                &[
                    "call".to_string(),
                    "--block".into(),
                    hex32(&post.block_hash),
                    hex20(&self.chain.book),
                    "keyState(bytes32)(uint8,bool,address,bytes32,uint64,uint64)".into(),
                    hex32(&k.id()),
                ],
            )?;
            keys.push((k.id(), parse_key_state(&out, &k.signer)?));
        }
        Ok(KeyLives {
            block: post.block,
            keys,
        })
    }

    fn block_time(&self, number: u64) -> Result<u64, String> {
        let v = rpc_json(
            &self.cast,
            &self.work,
            &self.rpc,
            "eth_getBlockByNumber",
            &[format!("\"0x{number:x}\""), "false".into()],
        )?;
        Ok(parse_block(&v)?.timestamp)
    }

    /// The settled report from its direct `postReport` call's calldata, or
    /// `None` when it was relayed through a contract (then a preimage gives it,
    /// `onyx_track::chain::Preimages::reports`; TR-C7-6).
    pub fn posted_report(
        &self,
        fact: &Hash32,
        report_hash: &Hash32,
    ) -> Result<Option<SignedReport>, String> {
        // ReportRecorded(bytes32 indexed digest, bytes32 indexed key, bytes32 indexed fact,
        // uint64 sequence, bytes32 reportHash, uint8 outcome)
        let topic = keccak256(b"ReportRecorded(bytes32,bytes32,bytes32,uint64,bytes32,uint8)");
        for log in self.logs(&self.chain.book, &[Some(topic), None, None, Some(*fact)])? {
            let data = hex::decode(
                log["data"]
                    .as_str()
                    .ok_or("no log data")?
                    .trim_start_matches("0x"),
            )
            .map_err(|e| e.to_string())?;
            if data.get(32..64) != Some(&report_hash.0[..]) {
                continue;
            }
            let tx = log["transactionHash"].as_str().ok_or("no tx hash")?;
            let t = rpc_json(
                &self.cast,
                &self.work,
                &self.rpc,
                "eth_getTransactionByHash",
                &[format!("\"{tx}\"")],
            )?;
            let to: [u8; 20] = bytes_field(&t, "to")?;
            let input = hex::decode(
                t["input"]
                    .as_str()
                    .ok_or("no input")?
                    .trim_start_matches("0x"),
            )
            .map_err(|e| e.to_string())?;
            // Relayed (another contract called the book): not in this calldata.
            return match (to == self.chain.book, report_from_calldata(&input)) {
                (true, Ok(r)) if r.hash() == *report_hash => Ok(Some(r)),
                _ => Ok(None),
            };
        }
        Err("no ReportRecorded event carries the settled report".into())
    }
}

impl ChainView for CastFacts {
    fn chain(&self) -> Hash32 {
        self.chain.id()
    }

    fn head(&self) -> Anchor {
        self.head
    }

    fn has_registry(&self) -> bool {
        self.chain.registry != [0; 20]
    }

    fn book_sources(&self) -> Result<Vec<SourceId>, String> {
        // KeyRegistered(bytes32 indexed key, address indexed signer, string source,
        // string origin, bytes32 provenance, address by, string reason)
        let topic =
            keccak256(b"KeyRegistered(bytes32,address,string,string,bytes32,address,string)");
        let mut out = Vec::new();
        for log in self.logs(&self.chain.book, &[Some(topic)])? {
            let data = hex::decode(
                log["data"]
                    .as_str()
                    .ok_or("no log data")?
                    .trim_start_matches("0x"),
            )
            .map_err(|e| e.to_string())?;
            let source = String::from_utf8(abi_dynamic(&data, 0)?).map_err(|e| e.to_string())?;
            out.push(SourceId(source));
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn operator_of(&self, source: &SourceId) -> Result<Hash32, String> {
        parse_word(&self.call(
            &self.chain.book,
            "operatorOf(bytes32)(bytes32)",
            &[hex32(&keccak256(source.0.as_bytes()))],
        )?)
    }

    fn settled(&self, fact: &Hash32) -> Result<Option<Settled>, String> {
        let out = self.call(
            &self.chain.book,
            "settleable(bytes32)(bool,bytes32,bytes32)",
            &[hex32(fact)],
        )?;
        let Some((report_hash, value_hash)) = crate::signed_chain::parse_settleable(&out)? else {
            return Ok(None);
        };
        let view = self.call(
            &self.chain.book,
            "factView(bytes32)(uint8,bytes32,uint64,bytes32[])",
            &[hex32(fact)],
        )?;
        let l = lines(&view);
        if l.len() < 3 || parse_word(l[1])? != report_hash {
            return Err(format!("factView disagrees with settleable: {view}"));
        }
        let accepted_at = parse_uint(l[2])?;
        Ok(Some(Settled {
            report: self.posted_report(fact, &report_hash)?,
            report_hash,
            value_hash,
            accepted_at,
        }))
    }

    fn registered(&self, metric: &MetricId) -> Result<(Registered, Hash32), String> {
        if self.chain.registry == [0; 20] {
            return Ok((Registered::None, Hash32::ZERO));
        }
        let out = self.call(
            &self.chain.registry,
            "metrics(bytes32)(uint8,bytes32,uint64)",
            &[hex32(&onyx_core::onchain::metric_key(metric))],
        )?;
        let l = lines(&out);
        if l.len() != 3 {
            return Err(format!("metrics: {out}"));
        }
        let kind = match parse_uint(l[0])? {
            0 => Registered::None,
            1 => Registered::Recipe,
            2 => Registered::Feed,
            3 => Registered::DatedFeed,
            k => return Err(format!("unknown metric kind {k}")),
        };
        Ok((kind, parse_word(l[1])?))
    }

    fn recorded(
        &self,
        metric: &MetricId,
        subject: &Subject,
        window_end: u64,
    ) -> Result<Option<Recorded>, String> {
        if self.chain.registry == [0; 20] {
            return Ok(None);
        }
        let out = self.call(
            &self.chain.registry,
            "observationOf(bytes32,bytes32,uint64)((bytes32,bytes32,bytes32,uint64,bytes32,bytes32,uint64))",
            &[
                hex32(&onyx_core::onchain::metric_key(metric)),
                hex32(&onyx_core::onchain::subject_key(subject)),
                window_end.to_string(),
            ],
        )?;
        let r = parse_observation(&out)?;
        if r.result == Hash32::ZERO {
            return Ok(None);
        }
        Ok(Some(Recorded {
            recorded_time: self.block_time(r.recorded_time)?,
            ..r
        }))
    }
}

/// A post anchor is the canonical block at its number: same number, hash and
/// time as the block the node returns for that number (review TR-C9-R1-9).
pub fn check_canonical(post: &Anchor, canonical: &Anchor) -> Result<(), String> {
    if canonical != post {
        return Err(format!(
            "post block {} is not the canonical block",
            post.block
        ));
    }
    Ok(())
}

/// `keyState(bytes32)` output (life, suspended, signer, source, retiredAt,
/// since) as a [`KeyLife`]. `OnyxSignedBook.Life`: 0 none, 1 active, 2
/// retired, 3 compromised. A registered key must have `signer`; anything
/// unexpected is an error, never read as active.
pub fn parse_key_state(out: &str, signer: &[u8; 20]) -> Result<KeyLife, String> {
    let l = lines(out);
    if l.len() != 6 {
        return Err(format!("keyState: {out}"));
    }
    let suspended = match l[1] {
        "true" => true,
        "false" => false,
        x => return Err(format!("keyState suspended: {x}")),
    };
    let life = match parse_uint(l[0])? {
        0 => return Ok(KeyLife::Unknown),
        1 if suspended => KeyLife::Suspended,
        1 => KeyLife::Active,
        2 => KeyLife::Retired,
        3 => KeyLife::Compromised,
        x => return Err(format!("keyState life: {x}")),
    };
    if !l[2].eq_ignore_ascii_case(&hex20(signer)) {
        return Err(format!("keyState signer {} is not the key's", l[2]));
    }
    Ok(life)
}

/// `cast call` output of `observationOf` (one tuple line:
/// `(metric, template, subjectKey, windowEnd, spec, result, recordedAt)`);
/// `recorded_time` holds the recording block's NUMBER until the caller reads
/// its timestamp.
pub fn parse_observation(out: &str) -> Result<Recorded, String> {
    let t = out.trim().trim_start_matches('(').trim_end_matches(')');
    let f: Vec<&str> = t.split(',').map(str::trim).collect();
    if f.len() != 7 {
        return Err(format!("observationOf: {out}"));
    }
    Ok(Recorded {
        template: parse_word(f[1])?,
        window_end: parse_uint(f[3])?,
        spec: parse_word(f[4])?,
        result: parse_word(f[5])?,
        recorded_time: parse_uint(f[6])?,
    })
}

/// A poster's track-commitment log in `OnyxSignedBook` through `cast`
/// (ADR-0036 amendment 3 item 4).
#[derive(Clone, Debug)]
pub struct CommitmentLogCast {
    /// The `cast` binary.
    pub cast: PathBuf,
    /// JSON-RPC URL.
    pub rpc: String,
    /// `cast` runs here, outside the repo.
    pub work: PathBuf,
    /// Chain, book, poster and track.
    pub log: CommitmentLog,
}

impl CommitmentLogCast {
    /// Post `c`, computed at block `head`, from the (unlocked) poster;
    /// returns its index in the log. The book refuses anything but the
    /// successor of its latest.
    pub fn post(&self, head: u64, c: &TrackCommitment) -> Result<u64, String> {
        let out = run_cast(
            &self.cast,
            &self.work,
            &self.rpc,
            &[
                "send".into(),
                "--unlocked".into(),
                "--from".into(),
                hex20(&self.log.poster),
                hex20(&self.log.book),
                "postTrackCommitment(bytes32,uint64,bytes)".into(),
                hex32(&self.log.track),
                head.to_string(),
                format!("0x{}", hex::encode(onyx_track::chain::commitment_bytes(c))),
                "--json".into(),
            ],
        )?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        if v["status"].as_str() != Some("0x1") {
            return Err("postTrackCommitment reverted".into());
        }
        let topic = hex32(&keccak256(
            b"TrackCommitted(address,bytes32,uint64,bytes32,uint64,uint64,bytes)",
        ));
        let log = v["logs"]
            .as_array()
            .and_then(|l| {
                l.iter()
                    .find(|x| x["topics"][0].as_str() == Some(topic.as_str()))
            })
            .ok_or("no TrackCommitted event")?;
        let index = log["topics"][3].as_str().ok_or("no index topic")?;
        let w = parse_word(index)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(&w.0[24..]);
        Ok(u64::from_be_bytes(b))
    }

    fn call(&self, block: &str, sig: &str, args: &[String]) -> Result<String, String> {
        let mut a = vec![
            "call".to_string(),
            "--block".into(),
            block.into(),
            hex20(&self.log.book),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        run_cast(&self.cast, &self.work, &self.rpc, &a)
    }

    /// `trackHead(poster, track)` at `block` (read it at a finalized block),
    /// on the log's chain id only.
    pub fn head(&self, block: &str) -> Result<LogHead, String> {
        let v = rpc_json(&self.cast, &self.work, &self.rpc, "eth_chainId", &[])?;
        let id = v
            .as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        if id != Some(self.log.chain_id) {
            return Err(format!(
                "chain id {v} is not the log's {}",
                self.log.chain_id
            ));
        }
        let out = self.call(
            block,
            "trackHead(address,bytes32)(uint64,bytes32,uint64)",
            &[hex20(&self.log.poster), hex32(&self.log.track)],
        )?;
        let l = lines(&out);
        if l.len() != 3 {
            return Err(format!("trackHead: {out}"));
        }
        Ok(LogHead {
            count: parse_uint(l[0])?,
            latest: parse_word(l[1])?,
            as_of: parse_uint(l[2])?,
        })
    }

    /// `trackCommitmentAt(poster, track, index)` at `block`.
    pub fn at(&self, block: &str, index: u64) -> Result<Hash32, String> {
        parse_word(&self.call(
            block,
            "trackCommitmentAt(address,bytes32,uint64)(bytes32)",
            &[
                hex20(&self.log.poster),
                hex32(&self.log.track),
                index.to_string(),
            ],
        )?)
    }
}

/// Write `p` where a node reads it (`ONYX_TRACK_PUBLICATIONS`, ROADMAP C20):
/// checked first ([`Publication::check`]), refused when it does not start with
/// the file already there ([`Publication::starts_with`]: a node would stop
/// serving), then Borsh to a temporary file beside it, synced and renamed into
/// place, so a node never reads half a file.
pub fn write_publication(path: &Path, p: &Publication) -> Result<(), String> {
    p.check().map_err(|e| format!("publication: {e}"))?;
    match std::fs::read(path) {
        Ok(old) => {
            let old: Publication =
                borsh::from_slice(&old).map_err(|e| format!("publication on disk: {e}"))?;
            // The node's rule (review C20-R1-5): a prefix of the new file, and
            // the same steps only when nothing changed.
            if !p.starts_with(&old) || (p.steps.len() == old.steps.len() && *p != old) {
                return Err("publication: does not start with the file already written".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("publication on disk: {e}")),
    }
    let bytes = borsh::to_vec(p).map_err(|e| format!("publication: {e}"))?;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("publication: {e}"))?;
    f.write_all(&bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("publication: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("publication: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: IntakeId = IntakeId {
        chain_id: 31_337,
        sink: [0x5e; 20],
        poster: [0x90; 20],
        first_nonce: 4,
    };

    fn tx(from: [u8; 20], to: [u8; 20], nonce: u64, data: [u8; 32]) -> serde_json::Value {
        json!({
            "from": hex20(&from),
            "to": hex20(&to),
            "nonce": format!("0x{nonce:x}"),
            "input": format!("0x{}", hex::encode(data)),
            "blockNumber": "0x10",
            "blockHash": format!("0x{}", hex::encode([1u8; 32])),
        })
    }

    /// The pure layer's mirrors of coordinator hashes (ADR-0036 amendment 3):
    /// the book's fact key and the level-2 result hash.
    #[test]
    fn track_mirrors_equal_the_coordinators_hashes() {
        use crate::observe::{result_hash_of, ObservationOutcome};
        use onyx_core::{Answer, Value};
        use onyx_track::chain::{book_fact_key, Level2Preimage, ObservedOutcome};
        let p = SignedPayload {
            source: SourceId("example.signed".into()),
            key: Hash32([3; 32]),
            metric: MetricId("example.signed.reserve".into()),
            subject: Subject::Entity("entity:example".into()),
            value: Value::UInt(7),
            observed_unix: 1_700_000_000,
            sequence: 1,
        };
        assert_eq!(
            book_fact_key(&p.source, &p.metric, &p.subject, p.observed_unix),
            crate::signed::signed_fact_key(&p)
        );
        let t = onyx_core::FeedTemplate {
            feed: "example.feed".into(),
            metric: MetricId("example.feed.value".into()),
            fetcher: Hash32([1; 32]),
            url_template: "https://example.org/v".into(),
            selector: "/v".into(),
            kind: onyx_core::ObservationKind::Numeric {
                scale: 2,
                tolerance: 0,
            },
            window_secs: 3_600,
            min_observers: 3,
            rights: onyx_core::DataRights::public_chain(SourceId("example.page".into())),
            subject: onyx_core::SubjectRule::Fixed(Subject::Aggregate("btc.mainnet".into())),
        };
        let spec = t
            .instantiate(&Subject::Aggregate("btc.mainnet".into()), 7_200)
            .expect("spec");
        let round = Hash32([9; 32]);
        for (mine, theirs) in [
            (
                ObservedOutcome::Value(Answer::Number(-5)),
                ObservationOutcome::Value(Answer::Number(-5)),
            ),
            (
                ObservedOutcome::Value(Answer::Bytes(vec![1, 2])),
                ObservationOutcome::Value(Answer::Bytes(vec![1, 2])),
            ),
            (
                ObservedOutcome::SourceUnavailable,
                ObservationOutcome::SourceUnavailable,
            ),
            (ObservedOutcome::NoResult, ObservationOutcome::NoResult),
        ] {
            let pre = Level2Preimage {
                template: t.clone(),
                spec: spec.clone(),
                round,
                outcome: mine,
            };
            assert_eq!(
                pre.result_hash(),
                result_hash_of(&spec.id(), &round, &theirs)
            );
        }
    }

    fn abi_word(n: usize) -> Vec<u8> {
        let mut w = vec![0u8; 32];
        w[24..].copy_from_slice(&(n as u64).to_be_bytes());
        w
    }

    fn abi_bytes(b: &[u8]) -> Vec<u8> {
        let mut out = abi_word(b.len());
        out.extend_from_slice(b);
        out.resize(32 + b.len().div_ceil(32) * 32, 0);
        out
    }

    /// A settled report is read back from its own `postReport` calldata; a
    /// call to anything else, or a truncated argument, is refused.
    #[test]
    fn a_report_is_read_from_its_post_report_calldata() {
        let r = SignedReport {
            payload: SignedPayload {
                source: SourceId("example.signed".into()),
                key: Hash32([3; 32]),
                metric: MetricId("example.signed.reserve".into()),
                subject: Subject::Entity("entity:example".into()),
                value: onyx_core::Value::UInt(7),
                observed_unix: 1_700_000_000,
                sequence: 1,
            },
            signature: [5; 65],
        };
        let payload = borsh::to_vec(&r.payload).expect("borsh");
        let p = abi_bytes(&payload);
        let mut input = keccak256(b"postReport(bytes,bytes)").0[..4].to_vec();
        input.extend(abi_word(64));
        input.extend(abi_word(64 + p.len()));
        input.extend(p);
        input.extend(abi_bytes(&r.signature));
        assert_eq!(report_from_calldata(&input), Ok(r));
        let mut other = input.clone();
        other[0] ^= 1;
        assert!(report_from_calldata(&other).is_err());
        assert!(report_from_calldata(&input[..input.len() - 40]).is_err());
        let mut huge = input.clone();
        huge[4 + 24] = 1;
        assert!(report_from_calldata(&huge).is_err());
    }

    #[test]
    fn observation_output_is_parsed_strictly() {
        let w = |b: u8| format!("0x{}", hex::encode([b; 32]));
        let out = format!(
            "({}, {}, {}, 7200 [7.2e3], {}, {}, 42)",
            w(1),
            w(2),
            w(3),
            w(4),
            w(5)
        );
        let r = parse_observation(&out).expect("parse");
        assert_eq!(
            (r.template, r.window_end, r.spec, r.result, r.recorded_time),
            (Hash32([2; 32]), 7_200, Hash32([4; 32]), Hash32([5; 32]), 42)
        );
        assert!(parse_observation("(0x01, 2)").is_err());
    }

    /// C9 (TR-R4-4): a key's level-3 status is read strictly; only life 1
    /// and not suspended is active, and an unexpected output is an error.
    #[test]
    fn key_state_output_is_parsed_strictly() {
        let signer = [0xab; 20];
        let out = |life: &str, suspended: &str, who: &str| {
            format!("{life}\n{suspended}\n{who}\n0x{}\n0\n0\n", "11".repeat(32))
        };
        let me = hex20(&signer);
        assert_eq!(
            parse_key_state(&out("1", "false", &me), &signer),
            Ok(KeyLife::Active)
        );
        assert_eq!(
            parse_key_state(&out("1", "true", &me), &signer),
            Ok(KeyLife::Suspended)
        );
        assert_eq!(
            parse_key_state(&out("2", "false", &me), &signer),
            Ok(KeyLife::Retired)
        );
        assert_eq!(
            parse_key_state(&out("3", "true", &me), &signer),
            Ok(KeyLife::Compromised)
        );
        let zero = hex20(&[0; 20]);
        assert_eq!(
            parse_key_state(&out("0", "false", &zero), &signer),
            Ok(KeyLife::Unknown)
        );
        assert!(parse_key_state(&out("4", "false", &me), &signer).is_err());
        assert!(parse_key_state(&out("1", "maybe", &me), &signer).is_err());
        assert!(parse_key_state(&out("1", "false", &zero), &signer).is_err());
        assert!(parse_key_state("1\nfalse\n", &signer).is_err());
    }

    /// Review TR-C9-R1-9: a post anchor naming another fork's block (or
    /// another time) at a canonical number is refused.
    #[test]
    fn a_post_anchor_must_be_the_canonical_block() {
        let a = Anchor {
            block: 7,
            block_hash: Hash32([1; 32]),
            timestamp: 100,
        };
        assert_eq!(check_canonical(&a, &a), Ok(()));
        for b in [
            Anchor {
                block_hash: Hash32([2; 32]),
                ..a
            },
            Anchor {
                timestamp: 101,
                ..a
            },
        ] {
            assert!(check_canonical(&b, &a).is_err());
        }
    }

    /// Review TR-R3-4: the node must be on the intake's chain, and only a
    /// loopback anvil may use a pinned depth instead of `finalized`.
    #[test]
    fn a_head_is_read_only_from_the_intakes_chain() {
        let anvil = json!("0x7a69");
        let local = "http://127.0.0.1:8545";
        assert_eq!(
            check_head_source(&ID, local, Finality::Depth(2), &anvil),
            Ok(())
        );
        assert!(check_head_source(&ID, local, Finality::Depth(2), &json!("0x1")).is_err());
        assert!(
            check_head_source(&ID, "https://rpc.example.org", Finality::Depth(2), &anvil).is_err()
        );
        assert_eq!(
            check_head_source(&ID, "https://rpc.example.org", Finality::Finalized, &anvil),
            Ok(())
        );
    }

    /// TR-I-3: only the poster's own transaction to the sink, with the next
    /// nonce and exactly the header hash, is a post.
    #[test]
    fn a_post_is_the_posters_next_transaction_carrying_the_header() {
        let h = Hash32([7; 32]);
        let ok = tx(ID.poster, ID.sink, 6, h.0);
        assert_eq!(check_post_tx(&ok, &ID, 2, Some(&h)), Ok((Some(16), h)));
        assert!(check_post_tx(&tx([1; 20], ID.sink, 6, h.0), &ID, 2, Some(&h)).is_err());
        assert!(check_post_tx(&tx(ID.poster, [1; 20], 6, h.0), &ID, 2, Some(&h)).is_err());
        assert!(check_post_tx(&tx(ID.poster, ID.sink, 5, h.0), &ID, 2, Some(&h)).is_err());
        assert!(check_post_tx(&tx(ID.poster, ID.sink, 6, [8; 32]), &ID, 2, Some(&h)).is_err());
        let mut pending = ok.clone();
        pending["blockNumber"] = serde_json::Value::Null;
        assert_eq!(check_post_tx(&pending, &ID, 2, Some(&h)), Ok((None, h)));
        let mut long = ok;
        long["input"] = json!(format!("0x{}00", hex::encode(h.0)));
        assert!(check_post_tx(&long, &ID, 2, Some(&h)).is_err());
        let b = json!({"number": "0x10", "hash": format!("0x{}", hex::encode([1u8; 32])), "timestamp": "0x6a"});
        assert_eq!(
            parse_block(&b),
            Ok(Anchor {
                block: 16,
                block_hash: Hash32([1; 32]),
                timestamp: 106
            })
        );
    }

    /// ROADMAP C20: the coordinator writes only a checked publication that
    /// starts with the file already written, whole (renamed into place).
    #[test]
    fn a_publication_is_written_whole_and_only_forward() {
        use onyx_track::example::{self, AS_OF, AS_OF_EARLY};
        let file = |steps: &[u64]| {
            let (p, reg) = (example::params(), example::registry());
            let mut out = Publication::new();
            for &as_of in steps {
                let (s, r) = example::logs(as_of);
                let b = onyx_track::compute(&p, &reg, &s, &r, as_of).expect("compute");
                out.push(&p, &s, &r, &b, None).expect("push");
            }
            out
        };
        let dir =
            std::env::temp_dir().join(format!("onyx-track-publication-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("publication.borsh");
        let _ = std::fs::remove_file(&path);
        let (early, full) = (file(&[AS_OF_EARLY]), file(&[AS_OF_EARLY, AS_OF]));
        write_publication(&path, &early).expect("first");
        write_publication(&path, &full).expect("extends");
        let back: Publication =
            borsh::from_slice(&std::fs::read(&path).expect("read")).expect("borsh");
        assert_eq!(back, full);
        assert!(!path.with_extension("tmp").exists());
        assert!(write_publication(&path, &early)
            .err()
            .is_some_and(|e| e.contains("does not start")));
        let mut rewritten = full.clone();
        rewritten.statements.swap(0, 1);
        assert!(write_publication(&path, &rewritten).is_err());
        write_publication(&path, &full).expect("unchanged is fine");
        let mut bad = full.clone();
        bad.records.pop();
        assert!(write_publication(&path, &bad)
            .err()
            .is_some_and(|e| e.contains("book rows")));
        let _ = std::fs::remove_file(&path);
    }
}
