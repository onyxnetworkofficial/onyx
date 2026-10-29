//! A [`DrawChain`] over Foundry's `cast`, for rehearsals on a local anvil
//! (L2RND-R1): the coordinator sends `announceDraw` and `fixBeacon` from an
//! unlocked anvil account (no key is read or passed), and empty blocks are
//! mined on demand to reach the beacon height. On a live chain (ROADMAP B6)
//! each sender is a [`TxSigner`] the runner adds: the key signs in-process
//! and is never put on a `cast` command line; the beacon height is waited
//! for, not mined.

use crate::keyfile::ReceiptKey;
use crate::observe::{
    test_keys, transcript_log_key, ChainView, DrawChain, DrawRecord, LogBlock, ObserverSig,
    OnchainDraw, OnchainFact, Posted, PostedCommit, RegistryDeployment, TranscriptLogQuery,
};
use onyx_core::Hash32;
use std::path::PathBuf;
use std::process::Command;

/// An `OnyxRegistry` on an anvil, driven through `cast`.
#[derive(Clone, Debug)]
pub struct CastChain {
    /// The `cast` binary.
    pub cast: PathBuf,
    /// The anvil's JSON-RPC URL.
    pub rpc: String,
    /// `cast` runs here, outside the repo: Foundry loads a `.env` from its
    /// working directory (M8-L55-F4).
    pub work: PathBuf,
    /// The registry.
    pub registry: [u8; 20],
    /// The coordinator (an unlocked anvil account).
    pub coordinator: [u8; 20],
    /// The block the transcript log is read at (review REVLOG-R3-1):
    /// `--finality finalized|safe|N`; `--confirmations N` is `N`. Default:
    /// [`Finality::Finalized`], except on a local anvil (chain id 31337),
    /// where it is [`DEFAULT_CONFIRMATIONS`] below the head.
    pub finality: Finality,
    /// [`DrawChain::deployment`], found once (review R11-1).
    pub deployed: std::sync::OnceLock<RegistryDeployment>,
    /// The receipt signer every draw this chain announces names
    /// (`setReceiptSigner` is sent first when the registry's differs: a
    /// rotation; ROADMAP B3). The public test key's address unless the
    /// runner configures the deployment's own
    /// ([`crate::keyfile::ReceiptKey`]).
    pub receipt_signer: [u8; 20],
    /// The reveal key every draw this chain announces names (ROADMAP C11):
    /// `setRevealKey` is sent first when the registry's differs (a rotation).
    /// `None`: draws announced as the registry has them (a coordinator
    /// without a reveal key sets none).
    pub reveal_key: Option<crate::sealed::Point>,
    /// [`DrawChain::records_rounds`]: the live coordinator records each round
    /// it settles (`recordAttempt`, `recordObservation`) before serving it;
    /// off for every other runner.
    pub record_rounds: bool,
    /// Where time-locked rounds read drand beacon signatures (ROADMAP C51,
    /// `--beacon drand|fixture:FILE`); `None`: a time-locked round waits.
    pub beacon: Option<Beacon>,
    /// Senders that sign their own transactions (ROADMAP B6, a live chain).
    /// A send from an address none of them holds goes as an unlocked anvil
    /// account.
    pub signers: Vec<std::sync::Arc<dyn TxSigner>>,
}

/// A sender that signs its own transactions (ROADMAP B6): on a live chain
/// nothing is unlocked. Implementations keep the key in-process; `Debug`
/// shows only the address.
pub trait TxSigner: std::fmt::Debug + Send + Sync {
    /// The sending address.
    fn address(&self) -> [u8; 20];
    /// Sign and send a call of `data` to `to`, wait for its receipt, and
    /// return its block number; a reverted call is an error.
    fn send_call(&self, to: &[u8; 20], data: &[u8]) -> Result<u64, String>;
}

/// Blocks above the chain height a draw's beacon goes on a live chain
/// (ROADMAP B6): Base Sepolia makes a block every 0.2 s, so [`DRAW_LEAD`]'s
/// 3 blocks would pass before `announceDraw` is mined; 50 blocks is about
/// 10 s there, and `fixBeacon` still has the 256-block `blockhash` window
/// (about 51 s) after the beacon height.
pub const LIVE_DRAW_LEAD: u64 = 50;

/// A coordinator's drand beacon source (ROADMAP C51). Every signature either
/// gives is verified against the pinned quicknet key before use.
#[derive(Clone, Debug)]
pub enum Beacon {
    /// drand's public HTTP relays (`--beacon drand`).
    Drand(crate::timelock::DrandHttp),
    /// Published rounds from a file (`--beacon fixture:FILE`; local anvil
    /// rehearsals only).
    Fixture(crate::timelock::FixtureBeacon),
}

impl Beacon {
    /// `--beacon drand` or `--beacon fixture:FILE` (the latter on a local
    /// anvil only: a rehearsal whose chain clock is set to the fixture's past).
    pub fn from_flag(flag: &str, chain_id: u64) -> Result<Self, String> {
        match flag.split_once(':') {
            None if flag == "drand" => Ok(Beacon::Drand(crate::timelock::DrandHttp::default())),
            Some(("fixture", path)) => {
                if chain_id != ANVIL_CHAIN_ID {
                    return Err(
                        "--beacon fixture:FILE is for a local anvil rehearsal only; use --beacon drand".into(),
                    );
                }
                Ok(Beacon::Fixture(crate::timelock::FixtureBeacon::from_file(
                    std::path::Path::new(path),
                )?))
            }
            _ => Err(format!("--beacon {flag}: use drand or fixture:FILE")),
        }
    }

    fn source(&self) -> &dyn crate::timelock::BeaconSource {
        match self {
            Beacon::Drand(d) => d,
            Beacon::Fixture(f) => f,
        }
    }
}

/// Where a [`CastChain`] reads the transcript log (review REVLOG-R3-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finality {
    /// The node's `finalized` block (OP stack: its L1 batch is finalized).
    Finalized,
    /// The node's `safe` block, for a node without `finalized`. Not final on
    /// an OP stack: [`DrawChain::draw_of_final`] refuses it (review R7).
    Safe,
    /// A pinned number of blocks below the head: local anvil only, where no
    /// reorg exists.
    Depth(u64),
}

/// Blocks below the head a local anvil's log is read at.
pub const DEFAULT_CONFIRMATIONS: u64 = 2;

/// Chain id of a local anvil.
pub const ANVIL_CHAIN_ID: u64 = 31_337;

/// The environment variables a runner's keys travel in (the deployer's, the
/// level-2 coordinator's, the markets admin's): removed from every `cast`
/// child, which never signs (THREATS B6-1).
pub const KEY_ENV_VARS: [&str; 4] = [
    "ONYX_DEPLOYER_KEY",
    "BASE_SEPOLIA_PRIVATE_KEY",
    "ONYX_L2_COORDINATOR_KEY",
    "ONYX_MARKETS_ADMIN_KEY",
];

fn ax(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn hx(h: &Hash32) -> String {
    format!("0x{}", hex::encode(h.0))
}

fn parse_h32(s: &str) -> Result<Hash32, String> {
    let mut a = [0u8; 32];
    hex::decode_to_slice(s.trim().trim_matches('"').trim_start_matches("0x"), &mut a)
        .map_err(|e| format!("not a 32-byte hash: {s} ({e})"))?;
    Ok(Hash32(a))
}

fn first_u64(s: &str) -> Result<u64, String> {
    s.split_whitespace()
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or_else(|| format!("not a number: {s}"))
}

impl CastChain {
    /// Run `cast` with `args` against the node.
    pub fn run(&self, args: &[String]) -> Result<String, String> {
        let mut cmd = Command::new(&self.cast);
        cmd.current_dir(&self.work)
            .args(args)
            .env("ETH_RPC_URL", &self.rpc);
        for k in KEY_ENV_VARS {
            cmd.env_remove(k);
        }
        let out = cmd.output().map_err(|e| format!("cast: {e}"))?;
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

    /// Send a transaction from an unlocked account; returns its receipt's
    /// block number.
    pub fn send(&self, from: &[u8; 20], sig: &str, args: &[String]) -> Result<u64, String> {
        if let Some(s) = self.signers.iter().find(|s| s.address() == *from) {
            let mut a = vec!["calldata".to_string(), sig.into()];
            a.extend(args.iter().cloned());
            let data = hex::decode(self.run(&a)?.trim().trim_start_matches("0x"))
                .map_err(|e| format!("calldata of {sig}: {e}"))?;
            return s
                .send_call(&self.registry, &data)
                .map_err(|e| format!("{sig}: {e}"));
        }
        let mut a = vec![
            "send".to_string(),
            "--unlocked".into(),
            "--from".into(),
            ax(from),
            ax(&self.registry),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        a.extend(["--json".to_string()]);
        let out = self.run(&a)?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        if v["status"].as_str() != Some("0x1") {
            return Err(format!("{sig} reverted"));
        }
        let n = v["blockNumber"].as_str().ok_or("no block number")?;
        u64::from_str_radix(n.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
    }

    /// A read-only call to the registry.
    pub fn call(&self, sig: &str, args: &[String]) -> Result<String, String> {
        let mut a = vec!["call".to_string(), ax(&self.registry), sig.into()];
        a.extend(args.iter().cloned());
        self.run(&a)
    }

    fn fact_key(&self, f: &OnchainFact) -> Result<Hash32, String> {
        self.fact_key_at(f, None)
    }

    /// `factKey` at `block` (`None`: the latest block).
    fn fact_key_at(&self, f: &OnchainFact, block: Option<u64>) -> Result<Hash32, String> {
        let mut a = vec!["call".to_string()];
        if let Some(b) = block {
            a.extend(["--block".to_string(), b.to_string()]);
        }
        a.extend([
            ax(&self.registry),
            "factKey(bytes32,bytes32,uint64)(bytes32)".into(),
            hx(&f.metric_key),
            hx(&f.subject_key),
            f.window_end.to_string(),
        ]);
        parse_h32(&self.run(&a)?)
    }
}

impl ChainView for CastChain {
    fn height(&self) -> Result<u64, String> {
        first_u64(&self.run(&["block-number".into()])?)
    }
    fn block_hash(&self, height: u64) -> Result<Option<Hash32>, String> {
        if height > self.height()? {
            return Ok(None);
        }
        let h = self.run(&[
            "block".into(),
            height.to_string(),
            "--field".into(),
            "hash".into(),
        ])?;
        parse_h32(&h).map(Some)
    }
}

impl DrawChain for CastChain {
    /// Review R11-1: chain id, registry and the hash of the registry's
    /// deploy block ([`CastChain::deploy_block`]), found once per run.
    fn deployment(&self) -> Result<Option<RegistryDeployment>, String> {
        if let Some(d) = self.deployed.get() {
            return Ok(Some(*d));
        }
        let (_, deploy_block) = self.deploy_block()?;
        let d = RegistryDeployment {
            chain_id: self.chain_id()?,
            registry: self.registry,
            deploy_block,
        };
        Ok(Some(*self.deployed.get_or_init(|| d)))
    }
    fn draw_lead(&self) -> Result<u64, String> {
        Ok(if self.chain_id()? == ANVIL_CHAIN_ID {
            crate::observe::DRAW_LEAD
        } else {
            LIVE_DRAW_LEAD
        })
    }
    fn draw_count(&self, fact: &OnchainFact) -> Result<u32, String> {
        let k = self.fact_key(fact)?;
        let n = first_u64(&self.call("drawCount(bytes32)(uint32)", &[hx(&k)])?)?;
        u32::try_from(n).map_err(|e| e.to_string())
    }

    fn draw_budget(&self, fact: &OnchainFact) -> Result<(u32, u32), String> {
        let out = self.call(
            "drawBudget(bytes32,bytes32)(uint32,uint32)",
            &[hx(&fact.metric_key), hx(&fact.subject_key)],
        )?;
        // One value per line (`cast` may append `[3e0]`).
        let mut it = out.lines().filter(|l| !l.trim().is_empty());
        let mut next = || -> Result<u32, String> {
            let v = first_u64(it.next().ok_or(format!("drawBudget returned {out}"))?)?;
            u32::try_from(v).map_err(|e| e.to_string())
        };
        Ok((next()?, next()?))
    }

    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        // A runner syncs its keys before it draws (each send mines a block,
        // and the beacon height is only `DRAW_LEAD` blocks ahead); here again,
        // a no-op unless a key changed since.
        self.sync_keys()?;
        self.send(
            &self.coordinator.clone(),
            "announceDraw(bytes32,bytes32,uint64,uint32,bytes32,uint64)",
            &[
                hx(&d.fact.metric_key),
                hx(&d.fact.subject_key),
                d.fact.window_end.to_string(),
                d.attempt.to_string(),
                hx(&d.commit),
                d.beacon_height.to_string(),
            ],
        )
        .map(|_| ())
    }

    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        let now = self.height()?;
        // fixBeacon runs in a block above the beacon height: mined on a
        // local anvil, waited for on a live chain (ROADMAP B6).
        if now <= d.beacon_height {
            if self.chain_id()? == ANVIL_CHAIN_ID {
                let n = d.beacon_height - now;
                self.run(&["rpc".into(), "anvil_mine".into(), format!("0x{n:x}")])?;
            } else {
                // At most 10 minutes; the caller reads the draw after any error.
                let mut waited = 0;
                while self.height()? <= d.beacon_height {
                    if waited > 1_200 {
                        return Err(format!("block {} not reached", d.beacon_height + 1));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    waited += 1;
                }
            }
        }
        let k = self.fact_key(&d.fact)?;
        // Reverts `BeaconAlreadyFixed` when anyone fixed it first; the
        // caller (`draw_and_open`) reads the draw before and after any error
        // (L2RND-R2-F4, review REVLOG-R5).
        self.send(
            &self.coordinator.clone(),
            "fixBeacon(bytes32,uint32)",
            &[hx(&k), d.attempt.to_string()],
        )?;
        self.draw_of(&d.fact, d.attempt)?
            .ok_or_else(|| format!("drawOf: no draw at attempt {}", d.attempt))
    }

    fn draw_of(&self, fact: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.draw_of_at(fact, attempt, None)
    }

    /// At the block [`DrawChain::log_block`] reports (finalized, or the
    /// pinned depth on anvil), every call named with `--block N`. The node
    /// must serve state at the finalized block (an archive-capable RPC: on
    /// Base Sepolia it lags the head by about 500 blocks, beyond the 128
    /// recent blocks a full node keeps); a node that cannot answers an error
    /// and every decision that needs this read stops the run, never voids a
    /// draw. Review R7 (Low): `--finality safe` is refused here: on an OP
    /// stack the safe head is derived from L1 blocks that are not final yet,
    /// so a draw shown absent there could still land. On a local anvil, which
    /// mines only on a transaction (a refused `announceDraw` mines none),
    /// one empty block is mined per read so a poll can reach a beacon height.
    fn draw_of_final(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        match self.finality {
            Finality::Safe => {
                return Err(
                    "drawOf at a final block: --finality safe is not final on an OP stack; run with --finality finalized".into(),
                )
            }
            Finality::Depth(_) => {
                self.run(&["rpc".into(), "anvil_mine".into(), "0x1".into()])?;
            }
            Finality::Finalized => {}
        }
        let b = self.log_block()?.ok_or("no final block")?.number;
        Ok((b, self.draw_of_at(fact, attempt, Some(b))?))
    }

    fn post_commit(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        let k = self.fact_key(fact)?;
        let held = |c: &Self| -> Result<Option<Hash32>, String> {
            let (h, _) = c.commit_log(&k, attempt, observer, round_id)?;
            Ok((h != Hash32::ZERO).then_some(h))
        };
        self.post_once(
            "postCommit",
            &k,
            attempt,
            observer,
            round_id,
            commit,
            sig,
            &held,
        )
    }

    fn post_receipt(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        let k = self.fact_key(fact)?;
        let slot = transcript_log_key(&k, attempt, observer, round_id);
        let held = |c: &Self| -> Result<Option<Hash32>, String> {
            let h = parse_h32(&c.call("receiptLog(bytes32)(bytes32)", &[hx(&slot)])?)?;
            Ok((h != Hash32::ZERO).then_some(h))
        };
        self.post_once(
            "postReceipt",
            &k,
            attempt,
            observer,
            round_id,
            commit,
            sig,
            &held,
        )
    }

    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        let tag = match self.finality {
            Finality::Finalized => "\"finalized\"".to_string(),
            Finality::Safe => "\"safe\"".to_string(),
            Finality::Depth(d) => format!("\"0x{:x}\"", self.height()?.saturating_sub(d)),
        };
        // One `eth_getBlockByNumber`: number and time of the same block.
        let out = self.run(&[
            "rpc".into(),
            "eth_getBlockByNumber".into(),
            tag,
            "false".into(),
        ])?;
        parse_log_block(&out).map(Some)
    }

    fn posted_commit(
        &self,
        block: u64,
        fact: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        // The fact key too is read at `block` (the registry's template).
        let k = self.fact_key_at(fact, Some(block))?;
        let (commit, posted_at, sig) =
            self.commit_log_at(&k, attempt, observer, round_id, block)?;
        if commit == Hash32::ZERO {
            return Ok(None);
        }
        Ok(Some(PostedCommit {
            commit,
            sig: sig.ok_or("commitLogOf: a posted commit without its signature")?,
            posted_at,
        }))
    }

    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        let out = self.call(
            "transcriptComplete(bytes32,uint32,bytes32,uint64,uint64,bytes)(bool)",
            &[
                hx(&q.registry_fact),
                q.attempt.to_string(),
                hx(&q.round_id),
                q.commit_open.to_string(),
                q.commit_close.to_string(),
                format!("0x{}", hex::encode(q.packed_entries())),
            ],
        )?;
        match out.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            o => Err(format!("transcriptComplete returned {o}")),
        }
    }

    fn draw_receipt_signer(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<Option<[u8; 20]>, String> {
        let k = self.fact_key(fact)?;
        let a = parse_addr(&self.call(
            "drawReceiptSigner(bytes32,uint32)(address)",
            &[hx(&k), attempt.to_string()],
        )?)?;
        Ok((a != [0; 20]).then_some(a))
    }

    fn draw_reveal_key(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<Option<crate::sealed::Point>, String> {
        let k = self.fact_key(fact)?;
        crate::sealed::reveal_key_from_return(
            &self.eth_call(&crate::sealed::draw_reveal_key_calldata(&k, attempt), None)?,
        )
    }

    /// Both slots at `block` exactly (`eth_call` with that block number; a
    /// node without it errors), decoded by the same strict ABI reader the
    /// clients use. Review C11-R2-2: refused under `--finality safe`, as
    /// [`DrawChain::draw_of_final`] is: the read is kept for good
    /// (`SealedAlreadyRead`), and on an OP stack a safe block can still
    /// reorg, adding or removing an in-phase post after the read.
    fn sealed_post(
        &self,
        block: u64,
        fact: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
    ) -> Result<Option<crate::sealed::PostedSealed>, String> {
        if self.finality == Finality::Safe {
            return Err(
                "sealed posts at a final block: --finality safe is not final on an OP stack; run with --finality finalized".into(),
            );
        }
        let k = self.fact_key_at(fact, Some(block))?;
        let r = self.eth_call(
            &crate::sealed::sealed_reveal_of_calldata(&k, attempt, observer, round_id),
            Some(block),
        )?;
        let p = self.eth_call(
            &crate::sealed::sealed_key_proof_of_calldata(&k, attempt, observer, round_id),
            Some(block),
        )?;
        crate::sealed::posted_from_returns(&r, &p)
    }

    fn reveal_log_complete(&self, q: &crate::sealed::RevealLogQuery) -> Result<bool, String> {
        crate::sealed::bool_from_return(&self.eth_call(&q.calldata(), None)?, "revealLogComplete")
    }

    fn draw_time_locked(&self, fact: &OnchainFact, attempt: u32) -> Result<bool, String> {
        let k = self.fact_key(fact)?;
        crate::sealed::bool_from_return(
            &self.eth_call(&crate::sealed::draw_time_locked_calldata(&k, attempt), None)?,
            "drawTimeLocked",
        )
    }

    fn beacon(&self) -> Option<&dyn crate::timelock::BeaconSource> {
        self.beacon.as_ref().map(Beacon::source)
    }

    fn records_rounds(&self) -> bool {
        self.record_rounds
    }

    fn record_attempt(
        &mut self,
        q: &crate::sealed::RevealLogQuery,
        result: &Hash32,
    ) -> Result<crate::observe::Recorded, String> {
        let matches = |c: &Self| -> Result<bool, String> {
            crate::sealed::bool_from_return(
                &c.eth_call(&q.matches_calldata(result), None)?,
                "attemptMatches",
            )
        };
        if matches(self)? {
            return Ok(crate::observe::Recorded::Before);
        }
        let sent = self.send(
            &self.coordinator.clone(),
            "recordAttempt(bytes32,uint32,bytes32,uint64,uint64,uint64,bytes32,bytes)",
            &[
                hx(&q.registry_fact),
                q.attempt.to_string(),
                hx(&q.round_id),
                q.commit_open.to_string(),
                q.commit_close.to_string(),
                q.reveal_close.to_string(),
                hx(result),
                format!("0x{}", hex::encode(q.packed_entries())),
            ],
        );
        match (sent, matches(self)?) {
            (Ok(_), true) => Ok(crate::observe::Recorded::Now),
            (Err(_), true) => Ok(crate::observe::Recorded::Before),
            (Ok(_), false) => Err("recordAttempt mined but attemptMatches is false".into()),
            (Err(e), false) => Err(e),
        }
    }

    fn record_observation(
        &mut self,
        rec: &crate::observe::OnchainObservationCheck,
    ) -> Result<crate::observe::Recorded, String> {
        let level = |c: &Self| -> Result<u64, String> {
            first_u64(&c.call(
                "observationLevel(bytes32,bytes32,uint64,bytes32,bytes32)(uint8)",
                &[
                    hx(&rec.metric_key),
                    hx(&rec.subject_key),
                    rec.window_end.to_string(),
                    hx(&rec.spec_id),
                    hx(&rec.result),
                ],
            )?)
        };
        if level(self)? == 2 {
            return Ok(crate::observe::Recorded::Before);
        }
        let sent = self.send(
            &self.coordinator.clone(),
            "recordObservation(bytes32,bytes32,uint64,bytes32,bytes32,bytes32,bytes32)",
            &[
                hx(&rec.metric_key),
                hx(&rec.subject_key),
                rec.window_end.to_string(),
                hx(&rec.spec_id),
                hx(&rec.result),
                hx(&rec.draw_pool),
                hx(&rec.cfg_hash),
            ],
        );
        match (sent, level(self)? == 2) {
            (Ok(_), true) => Ok(crate::observe::Recorded::Now),
            (Err(_), true) => Ok(crate::observe::Recorded::Before),
            (Ok(_), false) => Err("recordObservation mined but observationLevel is not 2".into()),
            (Err(e), false) => Err(e),
        }
    }
}

fn keccak_sel(sig: &[u8]) -> Vec<u8> {
    onyx_core::keccak256(sig).0[..4].to_vec()
}

impl CastChain {
    /// Set the registry's receipt signer and reveal key to this chain's
    /// (`setReceiptSigner`, `setRevealKey`) when they differ: a rotation, for
    /// the draws announced from now on; earlier draws keep the ones they
    /// snapshotted. ADR-0027 amendment 8: a draw needs a receipt signer;
    /// ROADMAP B3: the deployment's own hot key (the public test key in local
    /// rehearsals); ROADMAP C11: the coordinator's reveal key, when it has one, and
    /// none (`clearRevealKey`) when it has none.
    /// A runner calls this before it draws: each send mines a block, and a draw's
    /// beacon height is only `DRAW_LEAD` blocks ahead of the announcement.
    pub fn sync_keys(&mut self) -> Result<(), String> {
        let want = self.receipt_signer;
        let have = self.call("receiptSigner()(address)", &[])?;
        if parse_addr(&have)? != want {
            self.send(
                &self.coordinator.clone(),
                "setReceiptSigner(address)",
                &[ax(&want)],
            )?;
        }
        let have = self.eth_call(&keccak_sel(b"revealKey()"), None)?;
        match self.reveal_key {
            Some(k) if have.as_slice() != k.as_slice() => {
                self.send(
                    &self.coordinator.clone(),
                    "setRevealKey(bytes32,bytes32)",
                    &[
                        format!("0x{}", hex::encode(&k[..32])),
                        format!("0x{}", hex::encode(&k[32..])),
                    ],
                )?;
            }
            // Review C11-R1-2: a run without a reveal key clears one an earlier
            // run of this coordinator set, so its draws take no sealed post
            // (it could open none) and every client finds no key.
            None if have.iter().any(|b| *b != 0) => {
                self.send(&self.coordinator.clone(), "clearRevealKey()", &[])?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `timeLockRequired()` (ADR-0027 amendment 14, ROADMAP C51).
    pub fn time_lock_required(&self) -> Result<bool, String> {
        crate::sealed::bool_from_return(
            &self.eth_call(&keccak_sel(b"timeLockRequired()"), None)?,
            "timeLockRequired",
        )
    }

    /// Send `requireTimeLock()` from the coordinator unless the registry
    /// already requires time-locks (ROADMAP C51). ONE WAY: no call turns it
    /// off; every draw announced afterwards needs a reveal key and is
    /// time-locked.
    pub fn require_time_lock(&self) -> Result<(), String> {
        if !self.time_lock_required()? {
            self.send(&self.coordinator.clone(), "requireTimeLock()", &[])?;
        }
        if self.time_lock_required()? {
            Ok(())
        } else {
            Err("requireTimeLock: the registry still does not require time-locks".into())
        }
    }

    /// A raw `eth_call` to the registry with `data`, at `block` (`None`: the
    /// latest block): the return bytes. Through `cast rpc`, so the returns are
    /// decoded by the same strict readers the clients use.
    pub fn eth_call(&self, data: &[u8], block: Option<u64>) -> Result<Vec<u8>, String> {
        let tag = match block {
            Some(b) => format!("\"0x{b:x}\""),
            None => "\"latest\"".into(),
        };
        let call = format!(
            "{{\"to\":\"{}\",\"data\":\"0x{}\"}}",
            ax(&self.registry),
            hex::encode(data)
        );
        let out = self.run(&["rpc".into(), "eth_call".into(), call, tag])?;
        let h = out.trim().trim_matches('"');
        hex::decode(h.trim_start_matches("0x")).map_err(|e| format!("eth_call returned {out}: {e}"))
    }

    /// Post `post` with `postSealedReveal` (ROADMAP C11), sent from this
    /// chain's account (the observer itself in the rehearsal; anyone may
    /// relay it). A slot already filled is [`Posted::Before`] with the hash of
    /// the ciphertext it holds (the caller compares it with its own).
    pub fn post_sealed_reveal(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        round_id: &Hash32,
        post: &crate::sealed::SealedPost,
    ) -> Result<Posted, String> {
        let k = self.fact_key(fact)?;
        let held = |c: &Self| -> Result<Option<Hash32>, String> {
            let r = c.eth_call(
                &crate::sealed::sealed_reveal_of_calldata(&k, attempt, &post.observer, round_id),
                None,
            )?;
            let p = c.eth_call(
                &crate::sealed::sealed_key_proof_of_calldata(&k, attempt, &post.observer, round_id),
                None,
            )?;
            Ok(crate::sealed::posted_from_returns(&r, &p)?
                .map(|x| onyx_core::keccak256(&x.ciphertext)))
        };
        if let Some(h) = held(self)? {
            return Ok(Posted::Before(h));
        }
        let sent = self.send(
            &self.coordinator.clone(),
            "postSealedReveal(bytes32,uint32,address,bytes32,bytes32,bytes32,bytes,bytes,bytes)",
            &[
                hx(&k),
                attempt.to_string(),
                ax(&post.observer),
                hx(round_id),
                format!("0x{}", hex::encode(&post.ephemeral[..32])),
                format!("0x{}", hex::encode(&post.ephemeral[32..])),
                format!("0x{}", hex::encode(&post.ciphertext)),
                format!("0x{}", hex::encode(post.key_proof)),
                format!("0x{}", hex::encode(post.sig)),
            ],
        );
        match (sent, held(self)?) {
            (Ok(_), Some(_)) => Ok(Posted::Now),
            (Err(_), Some(h)) => Ok(Posted::Before(h)),
            (Ok(_), None) => Err("postSealedReveal mined but the slot is empty".into()),
            (Err(e), None) => Err(e),
        }
    }

    /// `observer`'s sealed post for `round_id` at the latest block (what an
    /// observer checks after posting).
    pub fn sealed_post_latest(
        &self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
    ) -> Result<Option<crate::sealed::PostedSealed>, String> {
        let b = self.height()?;
        self.sealed_post(b, fact, attempt, observer, round_id)
    }
}

impl CastChain {
    /// The commit `receiptLog` holds for `observer`'s slot in `round_id`
    /// (zero hash: no receipt yet): what an independent observer checks
    /// after it revealed (ROADMAP B2).
    pub fn receipt_posted(
        &self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
    ) -> Result<Option<Hash32>, String> {
        let k = self.fact_key(fact)?;
        let slot = transcript_log_key(&k, attempt, observer, round_id);
        let h = parse_h32(&self.call("receiptLog(bytes32)(bytes32)", &[hx(&slot)])?)?;
        Ok((h != Hash32::ZERO).then_some(h))
    }
}

impl CastChain {
    /// `drawOf` and `drawCount` at `block`, or at the latest block when
    /// `None`: every call pinned to ONE block (review R8-1: `drawCount` read
    /// at "latest" in a second call could come from another node, one block
    /// behind, and the record mix two states). A node without `block`
    /// errors (`cast call --block N`), never answers another state.
    fn draw_of_at(
        &self,
        fact: &OnchainFact,
        attempt: u32,
        block: Option<u64>,
    ) -> Result<Option<DrawRecord>, String> {
        let block = match block {
            Some(b) => b,
            None => self.height()?,
        };
        let k = self.fact_key_at(fact, Some(block))?;
        let at_block = |sig: &str, args: &[String]| -> Result<String, String> {
            let mut a = vec!["call".to_string()];
            a.extend(["--block".to_string(), block.to_string()]);
            a.extend([ax(&self.registry), sig.to_string()]);
            a.extend(args.iter().cloned());
            self.run(&a)
        };
        let out = at_block(
            "drawOf(bytes32,uint32)((bytes32,uint64,uint64,bytes32))",
            &[hx(&k), attempt.to_string()],
        )?;
        // `(0x…, 12, 9, 0x…)`
        let parts: Vec<&str> = out
            .trim_matches(|c| c == '(' || c == ')')
            .split(", ")
            .collect();
        let [commit, height, at, beacon] = parts[..] else {
            return Err(format!("drawOf returned {out}"));
        };
        let commit = parse_h32(commit)?;
        if commit == Hash32::ZERO {
            return Ok(None);
        }
        let chain_id = first_u64(&self.run(&["chain-id".into()])?)?;
        Ok(Some(DrawRecord {
            chain_id,
            registry: self.registry,
            attempt,
            draw_count: u32::try_from(first_u64(&at_block(
                "drawCount(bytes32)(uint32)",
                &[hx(&k)],
            )?)?)
            .map_err(|e| e.to_string())?,
            announced_at: first_u64(at)?,
            commit,
            beacon_height: first_u64(height)?,
            beacon: parse_h32(beacon)?,
        }))
    }

    /// `commitLogOf` at the latest block: the commit in the slot (zero when none).
    fn commit_log(
        &self,
        k: &Hash32,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
    ) -> Result<(Hash32, u64), String> {
        let (c, at, _) = self.commit_log_at(k, attempt, observer, round_id, self.height()?)?;
        Ok((c, at))
    }

    /// `commitLogOf` at `block`: commit (zero when none), block time and the
    /// observer's signature, from contract state. A node without `block`
    /// errors (`cast call --block N`), never answers an older state.
    fn commit_log_at(
        &self,
        k: &Hash32,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        block: u64,
    ) -> Result<(Hash32, u64, Option<ObserverSig>), String> {
        let out = self.run(&[
            "call".into(),
            "--block".into(),
            block.to_string(),
            ax(&self.registry),
            "commitLogOf(bytes32,uint32,address,bytes32)(bytes32,uint64,bytes)".into(),
            hx(k),
            attempt.to_string(),
            ax(observer),
            hx(round_id),
        ])?;
        parse_commit_log(&out)
    }

    /// Send a post unless its slot is filled; a slot filled first (before,
    /// or by a race with another relayer) is [`Posted::Before`].
    #[allow(clippy::too_many_arguments)]
    fn post_once(
        &mut self,
        what: &str,
        k: &Hash32,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
        held: &dyn Fn(&Self) -> Result<Option<Hash32>, String>,
    ) -> Result<Posted, String> {
        if let Some(c) = held(self)? {
            return Ok(Posted::Before(c));
        }
        let sent = self.send(
            &self.coordinator.clone(),
            &format!("{what}(bytes32,uint32,address,bytes32,bytes32,bytes)"),
            &[
                hx(k),
                attempt.to_string(),
                ax(observer),
                hx(round_id),
                hx(commit),
                format!("0x{}", hex::encode(sig)),
            ],
        );
        match (sent, held(self)?) {
            (Ok(_), _) => Ok(Posted::Now),
            (Err(_), Some(c)) => Ok(Posted::Before(c)),
            (Err(e), None) => Err(e),
        }
    }
}

impl CastChain {
    /// A chain from command-line flags `--rpc URL --registry ADDR
    /// --coordinator ADDR [--cast PATH] [--finality finalized|safe|N]
    /// [--confirmations N]`, or `None` when `--rpc` is absent.
    /// `cast` runs in a fresh temporary folder.
    pub fn from_args(args: &[String]) -> Result<Option<CastChain>, String> {
        let flag = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .map(|i| {
                    args.get(i + 1)
                        .cloned()
                        .ok_or(format!("{name} needs a value"))
                })
                .transpose()
        };
        let Some(rpc) = flag("--rpc")? else {
            return Ok(None);
        };
        let addr = |name: &str| -> Result<[u8; 20], String> {
            parse_addr(&flag(name)?.ok_or(format!("--rpc needs {name}"))?)
        };
        let cast = match flag("--cast")? {
            Some(c) => PathBuf::from(c),
            None => std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".foundry/bin/cast"))
                .filter(|p| p.exists())
                .unwrap_or_else(|| PathBuf::from("cast")),
        };
        let work = std::env::temp_dir().join(format!("onyx-cast-{}", std::process::id()));
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let mut c = CastChain {
            cast,
            rpc,
            work,
            registry: addr("--registry")?,
            coordinator: addr("--coordinator")?,
            finality: Finality::Finalized,
            deployed: std::sync::OnceLock::new(),
            receipt_signer: test_keys::receipt_signer(),
            reveal_key: None,
            record_rounds: false,
            beacon: None,
            signers: vec![],
        };
        let depth = |n: &str| -> Result<Finality, String> {
            n.parse()
                .map(Finality::Depth)
                .map_err(|e| format!("block depth {n}: {e}"))
        };
        c.finality = match (flag("--finality")?, flag("--confirmations")?) {
            (Some(f), _) => match f.as_str() {
                "finalized" => Finality::Finalized,
                "safe" => Finality::Safe,
                n => depth(n)?,
            },
            (None, Some(n)) => depth(&n)?,
            (None, None) => {
                if first_u64(&c.run(&["chain-id".into()])?)? == ANVIL_CHAIN_ID {
                    Finality::Depth(DEFAULT_CONFIRMATIONS)
                } else {
                    Finality::Finalized
                }
            }
        };
        if matches!(c.finality, Finality::Depth(_))
            && first_u64(&c.run(&["chain-id".into()])?)? != ANVIL_CHAIN_ID
        {
            return Err(
                "a pinned block depth is for a local anvil only; use --finality finalized".into(),
            );
        }
        if let Some(b) = flag("--beacon")? {
            c.beacon = Some(Beacon::from_flag(
                &b,
                first_u64(&c.run(&["chain-id".into()])?)?,
            )?);
        }
        Ok(Some(c))
    }
}

impl CastChain {
    /// Review R11-1: the block the registry was deployed in (its number and
    /// hash): the lowest block whose state holds the registry's code
    /// ([`lowest_block_with`], `cast code --block N`; the node must serve
    /// past state, as [`DrawChain::draw_of_final`] needs too). Off a local
    /// anvil that block must be final ([`DrawChain::log_block`]): a
    /// deployment a reorg could still move is not named in a results store.
    pub fn deploy_block(&self) -> Result<(u64, Hash32), String> {
        let head = self.height()?;
        let has_code = |b: u64| -> Result<bool, String> {
            let code = self.run(&[
                "code".into(),
                "--block".into(),
                b.to_string(),
                ax(&self.registry),
            ])?;
            Ok(!matches!(code.trim(), "" | "0x"))
        };
        let n = deploy_search(head, has_code)?.ok_or_else(|| {
            format!(
                "no OnyxRegistry code at {} on this chain",
                ax(&self.registry)
            )
        })?;
        if !matches!(self.finality, Finality::Depth(_)) {
            let fin = self.log_block()?.ok_or("no final block")?.number;
            if n > fin {
                return Err(format!(
                    "the registry's deploy block {n} is not final yet (final block {fin}): run again later"
                ));
            }
        }
        let h = self.block_hash(n)?.ok_or(format!("no block {n}"))?;
        Ok((n, h))
    }

    /// The node's chain id (`cast chain-id`).
    pub fn chain_id(&self) -> Result<u64, String> {
        first_u64(&self.run(&["chain-id".into()])?)
    }
}

/// The clock a level-2 runner stamps each reveal's arrival with (ADR-0027
/// amendment 8 item 4f; review REVLOG-R5, info).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockChoice {
    /// [`crate::observe::ReplayClock`]: a recorded rehearsal, replayed on the
    /// recorded pages' (past) timeline.
    Replay,
    /// [`crate::observe::SystemClock`]: a live coordinator.
    System,
}

/// Which clock a runner uses: `--clock replay|system` when given; else a
/// replay without a chain (`chain_id` `None`: the recorded draw is
/// replayed) or on a local anvil (a recorded rehearsal), and the system
/// clock on any other chain, so a live `--rpc` never stamps reveals with a
/// replayed time unless the operator says so.
pub fn clock_choice(chain_id: Option<u64>, flag: Option<&str>) -> Result<ClockChoice, String> {
    match flag {
        Some("replay") => Ok(ClockChoice::Replay),
        Some("system") => Ok(ClockChoice::System),
        Some(other) => Err(format!("--clock {other}: use replay or system")),
        None => Ok(match chain_id {
            None | Some(ANVIL_CHAIN_ID) => ClockChoice::Replay,
            Some(_) => ClockChoice::System,
        }),
    }
}

/// ROADMAP B3: a runner's receipt key from its flags, checked BEFORE
/// anything is sent. `--receipt-key FILE` gives the deployment's own hot key
/// ([`ReceiptKey::for_chain`]: without it only a replay or a local anvil
/// runs, on the public test key). It must differ from `--coordinator`,
/// `--owner`, `--reporter ADDR` (`OnyxRevenue.reporter`) and every pool
/// member in `observers`, else the run refuses to start. The chain's draws
/// then name it (`setReceiptSigner` before the next `announceDraw`, a
/// rotation when the registry's differs).
pub fn receipt_key_from_args(
    args: &[String],
    cast: Option<&mut CastChain>,
    observers: &[[u8; 20]],
) -> Result<ReceiptKey, String> {
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| {
                args.get(i + 1)
                    .cloned()
                    .ok_or(format!("{name} needs a value"))
            })
            .transpose()
    };
    let chain_id = cast.as_ref().map(|c| c.chain_id()).transpose()?;
    let key = ReceiptKey::for_chain(
        chain_id,
        flag("--receipt-key")?.as_deref().map(std::path::Path::new),
    )?;
    let mut named: Vec<(&str, [u8; 20])> = vec![];
    if let Some(c) = &cast {
        named.push(("the coordinator", c.coordinator));
        // Review B2-R1-L4: the registry's real owner, read from the chain,
        // not only the `--owner` flag.
        named.push((
            "the registry owner",
            parse_addr(&c.call("owner()(address)", &[])?)?,
        ));
        // Review B2-R1-L3: the public test key only on a LOCAL anvil: a
        // loopback RPC whose node says it is anvil (a remote node answering
        // chain id 31337 is not enough).
        // Review B2-R2-1: an anvil dev key in a key file is as public.
        if key.is_well_known() {
            let version = c.run(&["rpc".into(), "web3_clientVersion".into()])?;
            if !is_loopback_rpc(&c.rpc) || !version.to_ascii_lowercase().contains("anvil") {
                return Err(
                    "a public receipt key (the test key or an anvil dev key) runs only on a local anvil (a loopback RPC that is anvil); give --receipt-key FILE with a key of the deployment's own (ROADMAP B3)"
                        .into(),
                );
            }
        }
    }
    if let Some(o) = flag("--owner")? {
        named.push(("the registry owner", parse_addr(&o)?));
    }
    if let Some(r) = flag("--reporter")? {
        named.push(("the reporter", parse_addr(&r)?));
    }
    named.extend(observers.iter().map(|a| ("an observer", *a)));
    key.check_distinct(&named)?;
    if let Some(c) = cast {
        c.receipt_signer = key.address();
    }
    Ok(key)
}

/// Whether `rpc` is an `http(s)://` URL on this machine (127.0.0.1,
/// `localhost` or `[::1]`).
pub fn is_loopback_rpc(rpc: &str) -> bool {
    let rest = rpc
        .strip_prefix("http://")
        .or_else(|| rpc.strip_prefix("https://"))
        .unwrap_or("");
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let name = if host.starts_with('[') {
        host.split_once(']')
            .map_or(host, |(h, _)| h)
            .trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// ROADMAP B2: the clock of a coordinator whose observers are independent
/// processes. They commit and reveal in real time, so the coordinator runs
/// on [`crate::observe::SystemClock`] on every chain, a local anvil
/// included (whose block time follows the wall clock unless set); a replayed
/// clock is refused (`--clock replay`).
pub fn independent_clock(flag: Option<&str>) -> Result<ClockChoice, String> {
    match flag {
        None | Some("system") => Ok(ClockChoice::System),
        Some("replay") => Err(
            "--clock replay: independent observers reveal in real time; the coordinator runs on the system clock"
                .into(),
        ),
        Some(other) => Err(format!("--clock {other}: use system")),
    }
}

/// `--clock anvil` (ROADMAP C51): the system clock shifted to a LOCAL ANVIL's
/// latest block time, for a rehearsal whose anvil was started in the past so
/// its time-locked rounds lock to drand rounds already published (served from
/// fixtures). Refused on any other chain: a live process never runs on a
/// clock it shifted itself.
pub fn anvil_clock(chain: &CastChain) -> Result<crate::observe::ShiftedClock, String> {
    if first_u64(&chain.run(&["chain-id".into()])?)? != ANVIL_CHAIN_ID {
        return Err("--clock anvil is for a local anvil rehearsal only".into());
    }
    use crate::observe::Clock as _;
    let t = i64::try_from(chain.latest_block()?.time).map_err(|e| e.to_string())?;
    let now = i64::try_from(crate::observe::SystemClock.now()).map_err(|e| e.to_string())?;
    Ok(crate::observe::ShiftedClock { offset: t - now })
}

/// The largest gap (seconds) allowed between a live level-2 process's system
/// clock and the chain's latest block time (review B2-R2-2). The coordinator
/// stamps reveals with its own clock and the observer reveals by chain time;
/// a coordinator clock ahead of the chain by more than the reveal window
/// (120 s for the Treasury feed) would close its intake before any honest
/// observer revealed, and every honest observer would count as a
/// non-reveal. Well under the 60 s between window end and commit close.
pub const MAX_CLOCK_SKEW_SECS: u64 = 30;

/// Refuse to run a live level-2 process (ROADMAP B2) whose system clock
/// `system_now` is more than [`MAX_CLOCK_SKEW_SECS`] from the chain's
/// latest block time `chain_time`, either way (review B2-R2-2).
pub fn check_clock_skew(system_now: u64, chain_time: u64) -> Result<(), String> {
    let gap = system_now.abs_diff(chain_time);
    if gap > MAX_CLOCK_SKEW_SECS {
        return Err(format!(
            "this host's clock ({system_now}) is {gap} s {} the chain's latest block time ({chain_time}), over {MAX_CLOCK_SKEW_SECS} s: reveals are stamped by this clock and sent by chain time, so fix the clock (NTP) first (a local anvil needs --block-time)",
            if system_now > chain_time { "ahead of" } else { "behind" }
        ));
    }
    Ok(())
}

impl CastChain {
    /// The chain's latest block (number and time).
    pub fn latest_block(&self) -> Result<LogBlock, String> {
        parse_log_block(&self.run(&[
            "rpc".into(),
            "eth_getBlockByNumber".into(),
            "\"latest\"".into(),
            "false".into(),
        ])?)
    }

    /// Register `template`'s metric on the registry as `owner` (an unlocked
    /// anvil account), with the template's own kind (REG-R2-F2), unless it is
    /// registered already.
    pub fn register_metric(
        &self,
        owner: &[u8; 20],
        template: &onyx_core::FeedTemplate,
    ) -> Result<(), String> {
        use onyx_core::onchain::{metric_key, MetricKind};
        let mk = metric_key(&template.metric);
        let row = self.call("metrics(bytes32)(uint8,bytes32,uint64)", &[hx(&mk)])?;
        if first_u64(&row)? != 0 {
            return Ok(());
        }
        self.send(
            owner,
            "registerMetric(bytes32,uint8,bytes32,uint8)",
            &[
                hx(&mk),
                (MetricKind::of(template) as u8).to_string(),
                hx(&template.id()),
                "0".into(),
            ],
        )
        .map(|_| ())
    }
}

/// `cast call`'s output for `commitLogOf(..)(bytes32,uint64,bytes)`: one
/// value per line (`0x…`, `1760000000 [1.76e9]`, `0x…`); the signature is
/// `None` when the slot is empty.
pub fn parse_commit_log(out: &str) -> Result<(Hash32, u64, Option<ObserverSig>), String> {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut next = || {
        lines
            .next()
            .ok_or_else(|| format!("commitLogOf returned {out}"))
    };
    let commit = parse_h32(next()?)?;
    let at = first_u64(next()?)?;
    let sig = hex::decode(next()?.trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let sig = match sig.len() {
        0 => None,
        _ => Some(
            sig.try_into()
                .map_err(|_| format!("commitLogOf: a signature of the wrong length in {out}"))?,
        ),
    };
    Ok((commit, at, sig))
}

/// `eth_getBlockByNumber`'s answer: the block's number and time, from the
/// same JSON object.
pub fn parse_log_block(out: &str) -> Result<LogBlock, String> {
    let v: serde_json::Value =
        serde_json::from_str(out).map_err(|e| format!("eth_getBlockByNumber: {e}: {out}"))?;
    let q = |k: &str| {
        v[k].as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| format!("eth_getBlockByNumber: no {k} in {out}"))
    };
    Ok(LogBlock {
        number: q("number")?,
        time: q("timestamp")?,
    })
}

/// Review R12 (REVLOG-R12-5): [`lowest_block_with`] for the registry's
/// deploy block, with an error that says what the search needs. It reads
/// the registry's code at blocks down to the deploy block (any block from
/// genesis on): a node that keeps only recent state (a full, non-archive
/// node; public endpoints often are) cannot answer, and the run stops with
/// this message, never with a guessed tag.
pub fn deploy_search(
    top: u64,
    mut has: impl FnMut(u64) -> Result<bool, String>,
) -> Result<Option<u64>, String> {
    lowest_block_with(top, |b| {
        has(b).map_err(|e| {
            format!(
                "finding the registry's deploy block (for the results store tag, review R11-1) reads its code at past blocks, down to the deploy block; the node could not serve block {b}'s state: {e}. The search needs an RPC with full archive state (every past block's state), as the final-block reads do; a passing RPC error can simply be retried"
            )
        })
    })
}

/// Review R11-1: the lowest block in `0..=top` for which `has` holds,
/// `None` when it does not hold at `top`, by binary search: `has` must be
/// monotone (false below some block, true from it on), as a contract's
/// code is once deployed. About `log2(top)` calls.
pub fn lowest_block_with(
    top: u64,
    mut has: impl FnMut(u64) -> Result<bool, String>,
) -> Result<Option<u64>, String> {
    if !has(top)? {
        return Ok(None);
    }
    let (mut lo, mut hi) = (0, top);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if has(mid)? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(Some(hi))
}

/// A `0x`-prefixed (or bare) 20-byte address.
pub fn parse_addr(s: &str) -> Result<[u8; 20], String> {
    let mut a = [0u8; 20];
    hex::decode_to_slice(s.trim().trim_start_matches("0x"), &mut a)
        .map_err(|e| format!("not an address: {s} ({e})"))?;
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signer that records what it was asked to send.
    #[derive(Debug, Default)]
    struct Recorder(std::sync::Mutex<Vec<([u8; 20], Vec<u8>)>>);

    impl TxSigner for Recorder {
        fn address(&self) -> [u8; 20] {
            [0xab; 20]
        }
        fn send_call(&self, to: &[u8; 20], data: &[u8]) -> Result<u64, String> {
            self.0.lock().expect("lock").push((*to, data.to_vec()));
            Ok(7)
        }
    }

    /// ROADMAP B6: a send from an address a signer holds goes through that
    /// signer (signed in-process, never `cast send` with a key), with the
    /// registry as the target and the call's ABI encoding as the data.
    #[test]
    fn b6_a_send_from_a_signer_address_is_signed_by_it() {
        let cast = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".foundry/bin/cast"))
            .filter(|p| p.exists());
        let Some(cast) = cast else {
            eprintln!("skipped: no Foundry cast");
            return;
        };
        let rec = std::sync::Arc::new(Recorder::default());
        let work = std::env::temp_dir().join(format!("onyx-b6-signer-{}", std::process::id()));
        std::fs::create_dir_all(&work).expect("dir");
        let c = CastChain {
            cast,
            // Never reached: `cast calldata` reads no node.
            rpc: "http://127.0.0.1:9".into(),
            work: work.clone(),
            registry: [0x11; 20],
            coordinator: [0xab; 20],
            finality: Finality::Finalized,
            deployed: std::sync::OnceLock::new(),
            receipt_signer: [0; 20],
            reveal_key: None,
            record_rounds: false,
            beacon: None,
            signers: vec![rec.clone()],
        };
        let block = c
            .send(
                &[0xab; 20],
                "setReceiptSigner(address)",
                &[format!("0x{}", "22".repeat(20))],
            )
            .expect("sent through the signer");
        assert_eq!(block, 7);
        let sent = rec.0.lock().expect("lock").clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, [0x11; 20]);
        let mut want = keccak_sel(b"setReceiptSigner(address)");
        want.extend([0u8; 12]);
        want.extend([0x22; 20]);
        assert_eq!(sent[0].1, want);
        std::fs::remove_dir_all(&work).ok();
    }

    /// ROADMAP C51: a fixture beacon is for a local anvil rehearsal only, and
    /// only `drand` or `fixture:FILE` name a source.
    #[test]
    fn c51_a_fixture_beacon_is_for_a_local_anvil_only() {
        let dir = std::env::temp_dir().join(format!("onyx-c51-beacon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let f = dir.join("beacon.json");
        std::fs::write(
            &f,
            format!(
                "{{\"chain\": \"{}\", \"rounds\": []}}",
                hex::encode(crate::timelock::QUICKNET_CHAIN_HASH)
            ),
        )
        .expect("write");
        let flag = format!("fixture:{}", f.display());
        assert!(Beacon::from_flag(&flag, 84_532).is_err(), "Base Sepolia");
        assert!(matches!(
            Beacon::from_flag(&flag, ANVIL_CHAIN_ID),
            Ok(Beacon::Fixture(_))
        ));
        assert!(matches!(
            Beacon::from_flag("drand", 84_532),
            Ok(Beacon::Drand(_))
        ));
        assert!(Beacon::from_flag("http://evil.example", ANVIL_CHAIN_ID).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Review C11-R2-2: sealed posts are read only at a final block; a run
    /// with `--finality safe` is refused before any call (the read is kept for
    /// good, and a safe block can still reorg on an OP stack).
    #[test]
    fn c11_r2_2_sealed_posts_are_never_read_at_a_safe_block() {
        let chain = |finality| CastChain {
            cast: PathBuf::from("/nonexistent/onyx-test-cast"),
            rpc: "http://127.0.0.1:1".into(),
            work: std::env::temp_dir(),
            registry: [1; 20],
            coordinator: [2; 20],
            finality,
            deployed: std::sync::OnceLock::new(),
            receipt_signer: test_keys::receipt_signer(),
            reveal_key: None,
            record_rounds: false,
            beacon: None,
            signers: vec![],
        };
        let fact = OnchainFact {
            metric_key: Hash32::ZERO,
            subject_key: Hash32::ZERO,
            window_end: 0,
            dated: false,
            template: Hash32::ZERO,
        };
        let read = |c: &CastChain| {
            c.sealed_post(9, &fact, 0, &[3; 20], &Hash32::ZERO)
                .expect_err("no node")
        };
        let e = read(&chain(Finality::Safe));
        assert!(e.contains("--finality safe is not final"), "{e}");
        let e = read(&chain(Finality::Finalized));
        assert!(
            !e.contains("--finality safe"),
            "a finalized run asks its node: {e}"
        );
    }

    /// Review R12 (REVLOG-R12-5): a node without past state fails the
    /// deploy-block search with a message naming archive state.
    #[test]
    fn r12_5_a_non_archive_node_names_what_the_search_needs() {
        // The code is there from block 700; state before block 1,000 is pruned.
        let pruned = |b: u64| {
            if b < 1_000 {
                Err(format!("missing trie node at block {b} (path )"))
            } else {
                Ok(b >= 700)
            }
        };
        let e = deploy_search(5_000, pruned).expect_err("pruned");
        assert!(e.contains("full archive state"), "{e}");
        assert!(e.contains("missing trie node"), "{e}");
        assert_eq!(deploy_search(5_000, |b| Ok(b >= 700)), Ok(Some(700)));
    }

    /// Review REVLOG-R5 (info): a live chain gets the system clock unless
    /// the operator asks for a replay; a replay or a local anvil replays.
    #[test]
    fn a_live_chain_stamps_reveals_with_the_system_clock() {
        assert_eq!(clock_choice(None, None), Ok(ClockChoice::Replay));
        assert_eq!(
            clock_choice(Some(ANVIL_CHAIN_ID), None),
            Ok(ClockChoice::Replay)
        );
        assert_eq!(clock_choice(Some(84_532), None), Ok(ClockChoice::System));
        assert_eq!(
            clock_choice(Some(84_532), Some("replay")),
            Ok(ClockChoice::Replay)
        );
        assert_eq!(
            clock_choice(Some(ANVIL_CHAIN_ID), Some("system")),
            Ok(ClockChoice::System)
        );
        assert!(clock_choice(None, Some("wall")).is_err());
    }

    /// Review B2-R1-L3: the public test receipt key needs a loopback RPC.
    #[test]
    fn b3_only_a_loopback_rpc_is_local() {
        for ok in [
            "http://127.0.0.1:8547",
            "http://localhost:8545/",
            "http://[::1]:8545",
        ] {
            assert!(is_loopback_rpc(ok), "{ok}");
        }
        for no in [
            "https://sepolia.base.org",
            "http://127.0.0.1.evil.example:8545",
            "http://localhost.example/",
            "http://10.0.0.5:8545",
            "ws://127.0.0.1:8545",
            "http://127.0.0.1@evil.example:8545",
        ] {
            assert!(!is_loopback_rpc(no), "{no}");
        }
    }

    /// Review B2-R2-2: a live level-2 process refuses to run when its clock
    /// is more than 30 s from the chain's latest block time, either way.
    #[test]
    fn b2_r2_a_clock_far_from_the_chain_is_refused() {
        let t = 1_800_000_000;
        for ok in [t, t + MAX_CLOCK_SKEW_SECS, t - MAX_CLOCK_SKEW_SECS] {
            assert!(check_clock_skew(ok, t).is_ok(), "{ok}");
        }
        let e = check_clock_skew(t + MAX_CLOCK_SKEW_SECS + 1, t).expect_err("ahead");
        assert!(e.contains("ahead of"), "{e}");
        let e = check_clock_skew(t - 121, t).expect_err("behind");
        assert!(e.contains("behind"), "{e}");
    }

    /// ROADMAP B2: a coordinator of independent observers runs on the
    /// system clock, a local anvil included; a replayed clock is refused.
    #[test]
    fn b2_independent_observers_run_on_the_system_clock() {
        assert_eq!(independent_clock(None), Ok(ClockChoice::System));
        assert_eq!(independent_clock(Some("system")), Ok(ClockChoice::System));
        assert!(independent_clock(Some("replay")).is_err());
        assert!(independent_clock(Some("wall")).is_err());
    }

    /// Review R11-1: the registry's deploy block is the lowest block with
    /// its code, found in about log2(height) reads; none when the code is
    /// not there at the top.
    #[test]
    fn r11_1_the_deploy_block_is_the_lowest_block_with_code() {
        for (top, deployed) in [
            (0u64, 0u64),
            (1, 1),
            (2, 1),
            (1_000_000, 0),
            (30_000_000, 17_654_321),
        ] {
            let mut reads = 0;
            let n = lowest_block_with(top, |b| {
                reads += 1;
                Ok(b >= deployed)
            })
            .expect("search");
            assert_eq!(n, Some(deployed));
            assert!(reads <= 2 + 64 - top.leading_zeros(), "{reads} reads");
        }
        assert_eq!(lowest_block_with(10, |b| Ok(b > 10)), Ok(None));
        assert!(lowest_block_with(10, |_| Err("node down".into())).is_err());
    }
}
