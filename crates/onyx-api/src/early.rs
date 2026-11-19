//! The early-contributor record (ADR-0065, ROADMAP S26), slice 1: pure code,
//! no chain reads.
//!
//! A daily, append-only list of accepted test-network work: one row per
//! accepted on-chain event, each with the chain reference of the work and of
//! its acceptance. Early contributors may be rewarded later; this is a
//! record, not a promise. It holds no score, no weight and no ranking.
//!
//! - **Inputs.** [`Contracts`] (`deploy/early-record/contracts.json`: each
//!   deployment's address, the block its scan starts at and an ABI tag) and
//!   finalized logs as [`ChainLog`]s (block number and time, transaction
//!   hash, log index, address, topics, data).
//! - **Join** ([`State::apply`]). Work events are joined to the events that
//!   accept them (section 2 of ADR-0065). "Who" is always the address the
//!   contract checked (a signature or `msg.sender`), read from the event;
//!   a transaction's sender ([`ChainLog::from`]) is never read, so a
//!   relayer is never credited. A shard with a dispute case is never
//!   released and gives no row. A later Correct stake claim with another
//!   value supersedes earlier rows of its fact. Work that is not accepted
//!   yet stays in the [`State`] cache, which serializes to JSON and keeps
//!   only what a later event can still change ([`State::prune`]). Logs are
//!   sorted by (block, log index) and a repeat is dropped, so their order
//!   and repeats never change the output; two different logs at one
//!   position stop the join.
//! - **Output** ([`write_day`]). The CSV of one version of dataset
//!   [`DATASET`]: rows sorted by (accept block, accept log, kind, who), keys
//!   `(record_day << 24) + i`, one `day_close` row last. [`record_schema`]
//!   is the schema `onyx-publish build` takes for it.
//! - **Exclusions** ([`exclusion_changes`]). Dataset [`EXCLUSIONS_DATASET`]:
//!   append-only `add` and `remove` rows. Readers subtract the addresses in
//!   force. The fixed list ([`fixed_exclusions`]) holds the local test
//!   chain's public dev accounts and the placeholder addresses of the
//!   scripts; the reviewed list ([`ReviewedFile`],
//!   `deploy/early-record/exclusions.json`) the operator's own keys. Only
//!   the reviewed list may exclude a publisher key.
//!
//! Slice 2: the RPC reader (capped ranges, day bounds, the day hash check),
//! the role getters by `eth_call` ([`role_calls`] builds them), `l2_agreed`
//! rows from round transcripts and the `early-record` binary. Slice 3: the
//! anvil script in check-all and the record's own publisher id.

use onyx_bulk::schema::{Column, ColumnKind, Schema};
use onyx_core::{keccak256, Hash32};
use onyx_storage::abi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The record's dataset id.
pub const DATASET: &str = "testnet.early-contributors";
/// The exclusions dataset id.
pub const EXCLUSIONS_DATASET: &str = "testnet.early-contributors.exclusions";
/// License of both datasets.
pub const LICENSE: &str = "CC0-1.0";
/// Counting-rules version (the `rules` column).
pub const RULES: u32 = 1;
/// `contracts.json` and state file format.
pub const FORMAT: u32 = 1;
/// Most rows of one version, the `day_close` row included (the low 24 bits
/// of a key).
pub const MAX_ROWS_PER_VERSION: u64 = 1 << 24;
/// Longest text cell the writer accepts (bytes): far below the bulk limit
/// of 16 KiB, so a row always fits (the longest real cell is about 200).
pub const MAX_TEXT_CELL: usize = 256;
/// `OnyxSignedBook.MAX_CLOCK_SKEW_SECS`: a report's observed time is at most
/// this far after the block that recorded it.
pub const MAX_CLOCK_SKEW_SECS: u64 = 300;
/// The `check` cell of a row the chain alone proves.
pub const CHECK_CHAIN: &str = "chain";

const SECS_PER_DAY: u64 = 86_400;

/// `OnyxRound.AGREE`.
const AGREE: u8 = 0;
/// `OnyxSignedBook.Outcome.Accepted`.
const ACCEPTED: u8 = 1;
/// `OnyxStakes.State.Correct`.
const CORRECT: u8 = 3;
/// `OnyxStakes.By.Observation`.
const BY_OBSERVATION: u8 = 1;
/// `OnyxCorrections.NO`.
const NO: u8 = 2;
/// `OnyxStorage.MISSED` (a `Slashed` cause).
const MISSED: u8 = 3;

/// Something the record cannot accept.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EarlyError {
    /// `contracts.json` does not hold.
    #[error("contracts.json: {0}")]
    Contracts(String),
    /// A log that does not decode or does not belong.
    #[error("log: {0}")]
    Log(String),
    /// A log the join cannot place (an earlier log is missing, or comes twice).
    #[error("join: {0}")]
    Join(String),
    /// The version cannot be written.
    #[error("write: {0}")]
    Write(String),
    /// The exclusions do not hold.
    #[error("exclusions: {0}")]
    Exclusions(String),
}

fn contracts_err(s: impl Into<String>) -> EarlyError {
    EarlyError::Contracts(s.into())
}
fn log_err(s: impl Into<String>) -> EarlyError {
    EarlyError::Log(s.into())
}
fn join_err(s: impl Into<String>) -> EarlyError {
    EarlyError::Join(s.into())
}
fn write_err(s: impl Into<String>) -> EarlyError {
    EarlyError::Write(s.into())
}
fn excl_err(s: impl Into<String>) -> EarlyError {
    EarlyError::Exclusions(s.into())
}

// ------------------------------------------------------------------ events

/// The exact event signatures the rules read (from `contracts/src/*.sol`;
/// a test parses the sources and compares).
pub mod sig {
    /// `OnyxLedger` (settlement): a miner's signed commit.
    pub const COMMITTED: &str = "Committed(uint64,bytes32,address,bytes32)";
    /// `OnyxLedger`: a commit revealed.
    pub const REVEALED: &str = "Revealed(uint64,bytes32,address,bytes32)";
    /// `OnyxLedger`: a validator's verdict revealed.
    pub const VERDICT_REVEALED: &str = "VerdictRevealed(uint64,bytes32,address,uint8)";
    /// `OnyxLedger`: a shard's root is final.
    pub const FINALIZED: &str = "Finalized(uint64,bytes32,bytes32,bytes32)";
    /// `OnyxLedger`: a shard voided (bonds back, nobody paid).
    pub const SHARD_VOIDED: &str = "ShardVoided(uint64,bytes32)";
    /// `OnyxLedger`: a whole round voided.
    pub const ROUND_VOIDED: &str = "RoundVoided(uint64)";
    /// `OnyxDisputes`: a never-disputed shard's miners paid.
    pub const RELEASED: &str = "Released(uint64,bytes32)";
    /// `OnyxDisputes`: a case (a challenge, or a rerun of a Pending shard)
    /// opened on a shard: the shard can never be released.
    pub const CASE_OPENED: &str = "CaseOpened(bytes32,uint64,bytes32,uint8)";
    /// `OnyxDisputes`: a case ended (after `dEnd`, which may emit `Finalized`).
    pub const CASE_CLOSED: &str = "CaseClosed(bytes32,uint8)";
    /// `OnyxSignedBook`: a reviewed key.
    pub const KEY_REGISTERED: &str =
        "KeyRegistered(bytes32,address,string,string,bytes32,address,string)";
    /// `OnyxSignedBook`: a report recorded, with its outcome.
    pub const REPORT_RECORDED: &str =
        "ReportRecorded(bytes32,bytes32,bytes32,uint64,bytes32,uint8)";
    /// `OnyxSignedBook`: an accepted report read as withdrawn.
    pub const WITHDRAWN: &str = "Withdrawn(bytes32)";
    /// `OnyxSignedBook`: two reports contradict each other.
    pub const EQUIVOCATED: &str = "Equivocated(bytes32,bytes32,bytes32,uint8)";
    /// `OnyxSignedBook`: a key compromised from `since`.
    pub const KEY_COMPROMISED: &str = "KeyCompromised(bytes32,uint64,address,string)";
    /// `OnyxSignedBook`: a key retired at `at`.
    pub const KEY_RETIRED: &str = "KeyRetired(bytes32,uint64,address,string)";
    /// `OnyxStorage`: a node and its operator.
    pub const ACTIVATED: &str = onyx_storage::abi::events::ACTIVATED;
    /// `OnyxStorage`: a deal and its payer.
    pub const DEAL_CREATED: &str = onyx_storage::abi::events::DEAL_CREATED;
    /// `OnyxStorage`: a storage proof answered.
    pub const ANSWERED: &str = onyx_storage::abi::events::ANSWERED;
    /// `OnyxStorage`: a slash (the miss a reporter settled).
    pub const SLASHED: &str = onyx_storage::abi::events::SLASHED;
    /// `OnyxStorage`: the reporter of a miss paid.
    pub const MISS_REWARD: &str = onyx_storage::abi::events::MISS_REWARD;
    /// `OnyxStakes`: a staked claim.
    pub const SUBMITTED: &str = "Submitted(uint256,bytes32,address,int256,uint8)";
    /// `OnyxStakes`: a claim decided.
    pub const DECIDED: &str = "Decided(uint256,uint8,uint8,uint128,uint128)";
    /// `OnyxCorrections`: a market and its opener.
    pub const MARKET_OPENED: &str =
        "MarketOpened(uint256,bytes32,bytes32,address,int128,uint128,uint64,uint64)";
    /// `OnyxCorrections`: a bet.
    pub const BET: &str = "Bet(uint256,address,uint8,uint128)";
    /// `OnyxCorrections`: a market settled by its recheck.
    pub const RESOLVED: &str = "Resolved(uint256,int128,uint8,uint128)";
    /// `OnyxCorrections`: a market voided (every bet refunded).
    pub const VOIDED: &str = "Voided(uint256,uint8,bool)";
}

/// A deployment's ABI, as `contracts.json` names it. The rules decide which
/// of its events count and which role getters name operator addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Abi {
    /// `OnyxSettlement` (+ Ext): commits, reveals, verdicts, roots.
    SettlementV1,
    /// `OnyxDisputes`: `Released`.
    DisputesV1,
    /// `OnyxRegistry`: level-2 results (`l2_agreed`, slice 2).
    RegistryV1,
    /// `OnyxSignedBook`: keys and reports.
    SignedBookV1,
    /// `OnyxStorage` with `RULES_VERSION` 3.
    StorageV3,
    /// `OnyxStakes`: staked claims.
    StakesV1,
    /// `OnyxCorrections`: correction markets.
    CorrectionsV1,
    /// `OnyxRevenue`: roles only.
    RevenueV1,
    /// tONYX: roles only.
    TestTokenV1,
    /// `OnyxReobserve`: roles only in rules 1.
    ReobserveV1,
    /// `OnyxPanelOracle`: roles only in rules 1.
    PanelOracleV1,
    /// `OnyxObservedFacts`: not read in rules 1.
    ObservedFactsV1,
    /// `OnyxFutures`: not read (money, not data work).
    FuturesV1,
    /// `OnyxParametric`: not read (money, not data work).
    ParametricV1,
}

impl Abi {
    /// The events rules 1 reads from this ABI.
    pub fn events(self) -> &'static [&'static str] {
        match self {
            Abi::SettlementV1 => &[
                sig::COMMITTED,
                sig::REVEALED,
                sig::VERDICT_REVEALED,
                sig::FINALIZED,
                sig::SHARD_VOIDED,
                sig::ROUND_VOIDED,
            ],
            Abi::DisputesV1 => &[sig::RELEASED, sig::CASE_OPENED, sig::CASE_CLOSED],
            Abi::SignedBookV1 => &[
                sig::KEY_REGISTERED,
                sig::REPORT_RECORDED,
                sig::WITHDRAWN,
                sig::EQUIVOCATED,
                sig::KEY_COMPROMISED,
                sig::KEY_RETIRED,
            ],
            Abi::StorageV3 => &[
                sig::ACTIVATED,
                sig::DEAL_CREATED,
                sig::ANSWERED,
                sig::SLASHED,
                sig::MISS_REWARD,
            ],
            Abi::StakesV1 => &[sig::SUBMITTED, sig::DECIDED],
            Abi::CorrectionsV1 => &[sig::MARKET_OPENED, sig::BET, sig::RESOLVED, sig::VOIDED],
            // TODO S26 slice 2: `l2_agreed` reads `ObservationRecorded` here
            // and checks each row against the round transcript.
            Abi::RegistryV1
            | Abi::RevenueV1
            | Abi::TestTokenV1
            | Abi::ReobserveV1
            | Abi::PanelOracleV1
            | Abi::ObservedFactsV1
            | Abi::FuturesV1
            | Abi::ParametricV1 => &[],
        }
    }

    /// Whether rules 1 reads events of this ABI (its contract needs a
    /// `scan_from` block, and at most one contract may carry it).
    pub fn counted(self) -> bool {
        !self.events().is_empty()
    }

    /// The role getters whose answers are operator addresses (ADR-0065
    /// item 7.1), read at a day's last block.
    pub fn roles(self) -> &'static [&'static str] {
        match self {
            Abi::SettlementV1 => &["coordinator()"],
            Abi::RegistryV1 => &["owner()", "receiptSigner()"],
            Abi::SignedBookV1 => &["reviewer()"],
            Abi::StorageV3 => &["treasury()"],
            Abi::StakesV1 | Abi::CorrectionsV1 | Abi::PanelOracleV1 => &["owner()", "treasury()"],
            Abi::ReobserveV1 => &["owner()"],
            Abi::TestTokenV1 => &["minter()"],
            Abi::RevenueV1 => &["owner()", "reporter()", "treasury()"],
            Abi::DisputesV1 | Abi::ObservedFactsV1 | Abi::FuturesV1 | Abi::ParametricV1 => &[],
        }
    }
}

// ------------------------------------------------------------------ contracts.json

/// What a contract's `scan_from` block is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanFrom {
    /// The block its code appeared in, as a document states it with the
    /// deploy transaction or the address (slice 2 checks: no code one block
    /// before, code at it).
    Deploy,
    /// A block no counted event of it can precede: a documented block of
    /// its own deployment run, or (`bound_by`) the block of a contract it
    /// needs or was deployed after. Slice 2 finds the deploy block from
    /// `deploy_tx` when one is given.
    Bound,
}

/// One deployment in `contracts.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractEntry {
    /// Contract name (`OnyxSettlement`).
    pub name: String,
    /// ABI tag.
    pub abi: Abi,
    /// Lowercase `0x` address.
    pub address: String,
    /// The document that lists the address.
    pub doc: String,
    /// The row label in that document's table (deployment.md), if a table.
    #[serde(default)]
    pub doc_row: Option<String>,
    /// First block to read (required for an ABI whose events count).
    #[serde(default)]
    pub scan_from: Option<u64>,
    /// What `scan_from` is.
    #[serde(default)]
    pub scan_from_is: Option<ScanFrom>,
    /// The document that states `scan_from` for this contract.
    #[serde(default)]
    pub block_doc: Option<String>,
    /// The exact text of `block_doc` that states the block: it holds the
    /// number and this contract's address, or it is the paragraph above the
    /// table that lists this contract (a test checks both).
    #[serde(default)]
    pub block_quote: Option<String>,
    /// Instead of a quote: the contract whose documented block bounds this
    /// one (its events need that contract, or it was deployed after it).
    #[serde(default)]
    pub bound_by: Option<String>,
    /// Why the block holds, for readers.
    #[serde(default)]
    pub why: Option<String>,
    /// The deploy transaction, when a document names it.
    #[serde(default)]
    pub deploy_tx: Option<String>,
}

/// `deploy/early-record/contracts.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractsFile {
    /// [`FORMAT`].
    pub format: u32,
    /// Chain id.
    pub chain_id: u64,
    /// Network name.
    pub network: String,
    /// What the file is (free text for readers).
    #[serde(default)]
    pub note: String,
    /// Every deployment of the chain the record knows.
    pub contracts: Vec<ContractEntry>,
}

/// A checked `contracts.json` and the Keccak hash of its bytes (`day_close`
/// names it, so a changed list shows in the record).
#[derive(Clone, Debug)]
pub struct Contracts {
    /// The file.
    pub file: ContractsFile,
    /// keccak256 of the file's bytes.
    pub hash: Hash32,
    by_address: BTreeMap<[u8; 20], usize>,
}

/// `0x` + 40 lowercase hex digits, as an address.
pub fn parse_address(s: &str) -> Option<[u8; 20]> {
    let d = s.strip_prefix("0x")?;
    if d.len() != 40 || d.bytes().any(|c| !matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex::decode(d).ok()?.try_into().ok()
}

fn is_tx_hash(s: &str) -> bool {
    s.strip_prefix("0x")
        .is_some_and(|d| d.len() == 64 && d.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
}

/// A lowercase `0x` address.
pub fn addr_text(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn h32_text(h: &Hash32) -> String {
    format!("0x{}", h.to_hex())
}

impl Contracts {
    /// Parse and check `contracts.json`: format 1; lowercase nonzero unique
    /// addresses; every ABI whose events count has a `scan_from` block and
    /// at most one contract; a settlement and its disputes come together.
    pub fn parse(bytes: &[u8]) -> Result<Contracts, EarlyError> {
        let file: ContractsFile =
            serde_json::from_slice(bytes).map_err(|e| contracts_err(e.to_string()))?;
        if file.format != FORMAT {
            return Err(contracts_err(format!(
                "format {} (want {FORMAT})",
                file.format
            )));
        }
        if file.contracts.is_empty() {
            return Err(contracts_err("no contracts"));
        }
        let mut by_address = BTreeMap::new();
        let mut counted = BTreeSet::new();
        for (i, c) in file.contracts.iter().enumerate() {
            let what = |s: &str| contracts_err(format!("{} ({}): {s}", c.name, c.address));
            if c.name.is_empty() {
                return Err(what("no name"));
            }
            let a = parse_address(&c.address)
                .ok_or_else(|| what("address must be 0x and 40 lowercase hex digits"))?;
            if a == [0u8; 20] {
                return Err(what("the zero address"));
            }
            if by_address.insert(a, i).is_some() {
                return Err(what("listed twice"));
            }
            if c.scan_from.is_some() != c.scan_from_is.is_some() {
                return Err(what("scan_from and scan_from_is go together"));
            }
            if c.block_doc.is_some() != c.block_quote.is_some() {
                return Err(what("block_doc and block_quote go together"));
            }
            if c.scan_from.is_some() != (c.block_quote.is_some() ^ c.bound_by.is_some()) {
                return Err(what(
                    "scan_from needs exactly one of block_quote (with block_doc) and bound_by",
                ));
            }
            if c.scan_from_is == Some(ScanFrom::Deploy) && c.block_quote.is_none() {
                return Err(what("a deploy block needs the quote that states it"));
            }
            if c.block_quote
                .as_deref()
                .is_some_and(|q| c.scan_from.is_some_and(|b| !q.contains(&b.to_string())))
            {
                return Err(what("block_quote does not hold the scan_from number"));
            }
            if c.abi.counted() {
                if c.scan_from.is_none() {
                    return Err(what("its events count, so it needs scan_from"));
                }
                if !counted.insert(c.abi) {
                    return Err(what("a second contract with a counted ABI"));
                }
            }
            if c.deploy_tx.as_deref().is_some_and(|t| !is_tx_hash(t)) {
                return Err(what("deploy_tx must be 0x and 64 lowercase hex digits"));
            }
        }
        if counted.contains(&Abi::SettlementV1) != counted.contains(&Abi::DisputesV1) {
            return Err(contracts_err(
                "a settlement and its disputes contract come together",
            ));
        }
        for c in file.contracts.iter() {
            let Some(by) = &c.bound_by else { continue };
            let b = file
                .contracts
                .iter()
                .find(|o| &o.name == by && o.name != c.name)
                .ok_or_else(|| contracts_err(format!("{}: bound_by {by} is not listed", c.name)))?;
            if b.block_quote.is_none() || b.scan_from != c.scan_from {
                return Err(contracts_err(format!(
                    "{}: bound_by {by} needs {by}'s own quoted block, and the same scan_from",
                    c.name
                )));
            }
        }
        Ok(Contracts {
            file,
            hash: keccak256(bytes),
            by_address,
        })
    }

    /// Read and parse a file.
    pub fn load(path: &std::path::Path) -> Result<Contracts, EarlyError> {
        let b =
            std::fs::read(path).map_err(|e| contracts_err(format!("{}: {e}", path.display())))?;
        Contracts::parse(&b)
    }

    /// The deployment at `a`.
    pub fn get(&self, a: &[u8; 20]) -> Option<&ContractEntry> {
        self.by_address.get(a).map(|i| &self.file.contracts[*i])
    }

    /// The one contract with counted ABI `abi`.
    pub fn of(&self, abi: Abi) -> Option<&ContractEntry> {
        self.file.contracts.iter().find(|c| c.abi == abi)
    }

    /// The first block any counted contract is read from.
    pub fn first_block(&self) -> Option<u64> {
        self.file
            .contracts
            .iter()
            .filter(|c| c.abi.counted())
            .filter_map(|c| c.scan_from)
            .min()
    }

    /// What a log reader asks for: the counted contracts' addresses and
    /// their event topics (one `eth_getLogs` filter: an address array and
    /// topic 0 as a list).
    pub fn filter(&self) -> (Vec<String>, Vec<String>) {
        let mut addrs = Vec::new();
        let mut topics = BTreeSet::new();
        for c in self.file.contracts.iter().filter(|c| c.abi.counted()) {
            addrs.push(c.address.clone());
            for s in c.abi.events() {
                topics.insert(h32_text(&abi::topic(s)));
            }
        }
        addrs.sort();
        (addrs, topics.into_iter().collect())
    }
}

// ------------------------------------------------------------------ logs

/// One finalized log with what the record needs of its block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainLog {
    /// Emitting contract.
    pub address: [u8; 20],
    /// Topics (the first is the event's).
    pub topics: Vec<Hash32>,
    /// ABI data.
    pub data: Vec<u8>,
    /// Block number.
    pub block: u64,
    /// Block timestamp (unix seconds).
    pub block_time: u64,
    /// Transaction hash.
    pub tx: Hash32,
    /// Log index in the block.
    pub log_index: u64,
    /// The transaction's sender, when the reader knows it (logs do not
    /// carry it). The rules never read it: a row credits the address the
    /// contract checked, never the sender (THREATS EARLY-10).
    pub from: Option<[u8; 20]>,
}

fn rpc_u64(v: &Value, what: &str) -> Result<u64, EarlyError> {
    let s = v
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .ok_or_else(|| log_err(format!("{what}: not a 0x quantity")))?;
    if s.is_empty() || s.len() > 16 {
        return Err(log_err(format!("{what}: bad quantity")));
    }
    u64::from_str_radix(s, 16).map_err(|e| log_err(format!("{what}: {e}")))
}

fn rpc_bytes(v: &Value, what: &str) -> Result<Vec<u8>, EarlyError> {
    let s = v
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .ok_or_else(|| log_err(format!("{what}: not 0x hex")))?;
    hex::decode(s).map_err(|e| log_err(format!("{what}: {e}")))
}

fn rpc_word(v: &Value, what: &str) -> Result<Hash32, EarlyError> {
    rpc_bytes(v, what)?
        .try_into()
        .map(Hash32)
        .map_err(|_| log_err(format!("{what}: not 32 bytes")))
}

impl ChainLog {
    /// One item of an `eth_getLogs` answer. The block time is its
    /// `blockTimestamp` field (some nodes add it) or `block_time` (from the
    /// block's header); when both are there they must agree. A log marked
    /// removed is refused.
    pub fn from_rpc(v: &Value, block_time: Option<u64>) -> Result<ChainLog, EarlyError> {
        if v["removed"] == json!(true) {
            return Err(log_err("log marked removed"));
        }
        let address: [u8; 20] = rpc_bytes(&v["address"], "address")?
            .try_into()
            .map_err(|_| log_err("address: not 20 bytes"))?;
        let topics = v["topics"]
            .as_array()
            .ok_or_else(|| log_err("no topics"))?
            .iter()
            .map(|t| rpc_word(t, "topic"))
            .collect::<Result<Vec<_>, _>>()?;
        let stamp = match &v["blockTimestamp"] {
            Value::Null => None,
            t => Some(rpc_u64(t, "blockTimestamp")?),
        };
        let block_time = match (stamp, block_time) {
            (Some(a), Some(b)) if a != b => {
                return Err(log_err(format!(
                    "blockTimestamp {a} but the header says {b}"
                )))
            }
            (Some(a), _) | (None, Some(a)) => a,
            (None, None) => return Err(log_err("no block time")),
        };
        Ok(ChainLog {
            address,
            topics,
            data: rpc_bytes(&v["data"], "data")?,
            block: rpc_u64(&v["blockNumber"], "blockNumber")?,
            block_time,
            tx: rpc_word(&v["transactionHash"], "transactionHash")?,
            log_index: rpc_u64(&v["logIndex"], "logIndex")?,
            from: None,
        })
    }

    fn at(&self) -> Ref {
        Ref {
            block: self.block,
            log: self.log_index,
            tx: h32_text(&self.tx),
            time: self.block_time,
        }
    }
}

/// A chain reference: block, log index, transaction and block time.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ref {
    /// Block number.
    pub block: u64,
    /// Log index.
    pub log: u64,
    /// Transaction hash (`0x`, lowercase).
    pub tx: String,
    /// Block time (unix seconds).
    pub time: u64,
}

/// The decoded events the rules read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Committed {
        rs: String,
        miner: String,
    },
    Revealed {
        rs: String,
        miner: String,
        root: String,
    },
    Verdict {
        rs: String,
        validator: String,
        kind: u8,
    },
    Finalized {
        rs: String,
        root: String,
    },
    Released {
        rs: String,
    },
    ShardVoided {
        rs: String,
    },
    RoundVoided {
        round: u64,
    },
    CaseOpened {
        case: String,
        rs: String,
    },
    CaseClosed {
        case: String,
    },
    KeyRegistered {
        key: String,
        signer: String,
    },
    Report {
        digest: String,
        key: String,
        outcome: u8,
    },
    Withdrawn {
        digest: String,
    },
    Equivocated {
        first: String,
        second: String,
    },
    Compromised {
        key: String,
        since: u64,
    },
    Retired {
        key: String,
        at: u64,
    },
    Activated {
        node: String,
        operator: String,
    },
    Deal {
        deal: u64,
        payer: String,
    },
    Answered {
        subject: String,
        deal: u64,
        node: String,
        passed: bool,
        paid: u128,
    },
    Slashed {
        node: String,
        subject: String,
        deal: u64,
        cause: u8,
    },
    MissReward {
        node: String,
        reporter: String,
        amount: u128,
    },
    Submitted {
        claim: String,
        fact: String,
        submitter: String,
        /// The `int256` value's word, as `0x` hex (only compared).
        value: String,
    },
    Decided {
        claim: String,
        state: u8,
        by: u8,
    },
    Opened {
        market: String,
        opener: String,
    },
    Bet {
        market: String,
        who: String,
        side: u8,
        amount: u128,
    },
    Resolved {
        market: String,
        winner: u8,
    },
    MarketVoided {
        market: String,
    },
}

fn shape(
    l: &ChainLog,
    name: &str,
    topics: usize,
    data: usize,
    exact: bool,
) -> Result<(), EarlyError> {
    let ok_data = if exact {
        l.data.len() == data
    } else {
        l.data.len() >= data && l.data.len().is_multiple_of(32)
    };
    if l.topics.len() != topics || !ok_data {
        return Err(log_err(format!(
            "{name} at {}:{} has {} topics and {} data bytes (want {topics} and {}{data})",
            l.block,
            l.log_index,
            l.topics.len(),
            l.data.len(),
            if exact { "" } else { "at least " }
        )));
    }
    Ok(())
}

fn t_u64(t: &Hash32, what: &str) -> Result<u64, EarlyError> {
    if t.0[..24].iter().any(|b| *b != 0) {
        return Err(log_err(format!("{what}: topic over u64")));
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&t.0[24..]);
    Ok(u64::from_be_bytes(b))
}

fn t_uint(t: &Hash32, what: &str) -> Result<String, EarlyError> {
    if t.0[..16].iter().any(|b| *b != 0) {
        return Err(log_err(format!("{what}: topic over u128")));
    }
    let mut b = [0u8; 16];
    b.copy_from_slice(&t.0[16..]);
    Ok(u128::from_be_bytes(b).to_string())
}

fn t_addr(t: &Hash32, what: &str) -> Result<String, EarlyError> {
    if t.0[..12].iter().any(|b| *b != 0) {
        return Err(log_err(format!("{what}: topic is not an address")));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&t.0[12..]);
    Ok(addr_text(&a))
}

fn w_uint(data: &[u8], i: usize, what: &str) -> Result<u128, EarlyError> {
    abi::word_uint(data, i).map_err(|e| log_err(format!("{what}: {e}")))
}

fn w_u64(data: &[u8], i: usize, what: &str) -> Result<u64, EarlyError> {
    abi::word_u64(data, i).map_err(|e| log_err(format!("{what}: {e}")))
}

fn w_small(data: &[u8], i: usize, max: u128, what: &str) -> Result<u128, EarlyError> {
    let v = w_uint(data, i, what)?;
    if v > max {
        return Err(log_err(format!("{what}: {v} is over {max}")));
    }
    Ok(v)
}

fn w_u8(data: &[u8], i: usize, what: &str) -> Result<u8, EarlyError> {
    Ok(w_small(data, i, u128::from(u8::MAX), what)? as u8)
}

fn w_addr(data: &[u8], i: usize, what: &str) -> Result<String, EarlyError> {
    abi::word_addr(data, i)
        .map(|a| addr_text(&a))
        .map_err(|e| log_err(format!("{what}: {e}")))
}

/// Decode `l` as the event of `abi` its topic 0 names; `None` for an event
/// the rules do not read. A known event of the wrong shape is an error, so a
/// malformed answer is never half read.
fn decode(abi_tag: Abi, l: &ChainLog) -> Result<Option<Ev>, EarlyError> {
    let Some(t0) = l.topics.first() else {
        return Ok(None);
    };
    let Some(name) = abi_tag
        .events()
        .iter()
        .copied()
        .find(|s| abi::topic(s) == *t0)
    else {
        return Ok(None);
    };
    let t = &l.topics;
    let d = &l.data;
    let rs = |t: &[Hash32]| -> Result<String, EarlyError> {
        Ok(format!("{}:{}", t_u64(&t[1], "round")?, h32_text(&t[2])))
    };
    let ev = match name {
        sig::COMMITTED => {
            shape(l, name, 4, 32, true)?;
            Ev::Committed {
                rs: rs(t)?,
                miner: t_addr(&t[3], "miner")?,
            }
        }
        sig::REVEALED => {
            shape(l, name, 4, 32, true)?;
            Ev::Revealed {
                rs: rs(t)?,
                miner: t_addr(&t[3], "miner")?,
                root: h32_text(&Hash32(
                    abi::word(d, 0).map_err(|e| log_err(e.to_string()))?,
                )),
            }
        }
        sig::VERDICT_REVEALED => {
            shape(l, name, 4, 32, true)?;
            Ev::Verdict {
                rs: rs(t)?,
                validator: t_addr(&t[3], "validator")?,
                kind: w_u8(d, 0, "verdict kind")?,
            }
        }
        sig::FINALIZED => {
            shape(l, name, 3, 64, true)?;
            Ev::Finalized {
                rs: rs(t)?,
                root: h32_text(&Hash32(
                    abi::word(d, 0).map_err(|e| log_err(e.to_string()))?,
                )),
            }
        }
        sig::RELEASED => {
            shape(l, name, 3, 0, true)?;
            Ev::Released { rs: rs(t)? }
        }
        sig::SHARD_VOIDED => {
            shape(l, name, 3, 0, true)?;
            Ev::ShardVoided { rs: rs(t)? }
        }
        sig::ROUND_VOIDED => {
            shape(l, name, 2, 0, true)?;
            Ev::RoundVoided {
                round: t_u64(&t[1], "round")?,
            }
        }
        sig::CASE_OPENED => {
            shape(l, name, 4, 32, true)?;
            Ev::CaseOpened {
                case: h32_text(&t[1]),
                rs: format!("{}:{}", t_u64(&t[2], "round")?, h32_text(&t[3])),
            }
        }
        sig::CASE_CLOSED => {
            shape(l, name, 2, 32, true)?;
            Ev::CaseClosed {
                case: h32_text(&t[1]),
            }
        }
        sig::KEY_REGISTERED => {
            // Head: source, origin (offsets), provenance, by, reason (offset).
            shape(l, name, 3, 160, false)?;
            Ev::KeyRegistered {
                key: h32_text(&t[1]),
                signer: t_addr(&t[2], "signer")?,
            }
        }
        sig::REPORT_RECORDED => {
            shape(l, name, 4, 96, true)?;
            Ev::Report {
                digest: h32_text(&t[1]),
                key: h32_text(&t[2]),
                outcome: w_u8(d, 2, "outcome")?,
            }
        }
        sig::WITHDRAWN => {
            shape(l, name, 2, 0, true)?;
            Ev::Withdrawn {
                digest: h32_text(&t[1]),
            }
        }
        sig::EQUIVOCATED => {
            shape(l, name, 4, 32, true)?;
            Ev::Equivocated {
                first: h32_text(&t[2]),
                second: h32_text(&t[3]),
            }
        }
        sig::KEY_COMPROMISED => {
            shape(l, name, 2, 96, false)?;
            Ev::Compromised {
                key: h32_text(&t[1]),
                since: w_u64(d, 0, "since")?,
            }
        }
        sig::KEY_RETIRED => {
            shape(l, name, 2, 96, false)?;
            Ev::Retired {
                key: h32_text(&t[1]),
                at: w_u64(d, 0, "at")?,
            }
        }
        sig::ACTIVATED => {
            shape(l, name, 4, 0, true)?;
            Ev::Activated {
                node: h32_text(&t[1]),
                operator: t_addr(&t[3], "operator")?,
            }
        }
        sig::DEAL_CREATED => {
            // Twelve head words (three of them string offsets), then tails.
            shape(l, name, 3, 12 * 32, false)?;
            Ev::Deal {
                deal: t_u64(&t[1], "dealId")?,
                payer: t_addr(&t[2], "payer")?,
            }
        }
        sig::ANSWERED => {
            shape(l, name, 4, 128, true)?;
            let deal = t_u64(&t[1], "dealId")?;
            let node = h32_text(&t[3]);
            let slot = w_small(d, 0, u128::from(u32::MAX), "slot")?;
            Ev::Answered {
                subject: format!("{deal}:{}:{slot}:{node}", t_u64(&t[2], "epoch")?),
                deal,
                node,
                passed: w_small(d, 2, 1, "passed")? == 1,
                paid: w_uint(d, 3, "paid")?,
            }
        }
        sig::SLASHED => {
            shape(l, name, 3, 192, true)?;
            let node = h32_text(&t[1]);
            let deal = t_u64(&t[2], "dealId")?;
            let epoch = w_u64(d, 0, "epoch")?;
            let slot = w_small(d, 1, u128::from(u32::MAX), "slot")?;
            Ev::Slashed {
                subject: format!("{deal}:{epoch}:{slot}:{node}"),
                node,
                deal,
                cause: w_u8(d, 5, "cause")?,
            }
        }
        sig::MISS_REWARD => {
            shape(l, name, 3, 32, true)?;
            Ev::MissReward {
                node: h32_text(&t[1]),
                reporter: t_addr(&t[2], "reporter")?,
                amount: w_uint(d, 0, "amount")?,
            }
        }
        sig::SUBMITTED => {
            shape(l, name, 4, 64, true)?;
            Ev::Submitted {
                claim: t_uint(&t[1], "claimId")?,
                fact: h32_text(&t[2]),
                submitter: t_addr(&t[3], "submitter")?,
                value: h32_text(&Hash32(
                    abi::word(d, 0).map_err(|e| log_err(e.to_string()))?,
                )),
            }
        }
        sig::DECIDED => {
            shape(l, name, 2, 128, true)?;
            Ev::Decided {
                claim: t_uint(&t[1], "claimId")?,
                state: w_u8(d, 0, "state")?,
                by: w_u8(d, 1, "by")?,
            }
        }
        sig::MARKET_OPENED => {
            shape(l, name, 4, 160, true)?;
            Ev::Opened {
                market: t_uint(&t[1], "marketId")?,
                opener: w_addr(d, 0, "opener")?,
            }
        }
        sig::BET => {
            shape(l, name, 3, 64, true)?;
            Ev::Bet {
                market: t_uint(&t[1], "marketId")?,
                who: t_addr(&t[2], "who")?,
                side: w_u8(d, 0, "side")?,
                amount: w_uint(d, 1, "amount")?,
            }
        }
        sig::RESOLVED => {
            shape(l, name, 2, 96, true)?;
            Ev::Resolved {
                market: t_uint(&t[1], "marketId")?,
                winner: w_u8(d, 1, "winner")?,
            }
        }
        sig::VOIDED => {
            shape(l, name, 2, 64, true)?;
            Ev::MarketVoided {
                market: t_uint(&t[1], "marketId")?,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(ev))
}

// ------------------------------------------------------------------ rows

/// A row's kind (the `kind` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A miner's revealed root, final and released.
    L1Root,
    /// A validator's Agree verdict on a final, released shard.
    L1Verdict,
    /// A storage proof that passed.
    StorageProof,
    /// A miss reported and rewarded.
    StorageMissReport,
    /// A signed report the book accepted.
    SignedReport,
    /// A staked claim decided correct.
    StakeCorrect,
    /// A No bet in a correction market that No won.
    CorrectionFound,
    /// A later event withdrew an earlier row's work.
    Revoke,
    /// The last row of a version.
    DayClose,
}

impl Kind {
    /// The `kind` cell.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::L1Root => "l1_root",
            Kind::L1Verdict => "l1_verdict",
            Kind::StorageProof => "storage_proof",
            Kind::StorageMissReport => "storage_miss_report",
            Kind::SignedReport => "signed_report",
            Kind::StakeCorrect => "stake_correct",
            Kind::CorrectionFound => "correction_found",
            Kind::Revoke => "revoke",
            Kind::DayClose => "day_close",
        }
    }
}

/// What a `revoke` row revokes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Revokes {
    /// A row of an earlier version, by key.
    Key(i128),
    /// The row of this version with this kind (`signed_report` or
    /// `stake_correct`) and subject.
    SameVersion(Kind, String),
}

/// One row before its key is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Kind.
    pub kind: Kind,
    /// The address the contract checked (lowercase `0x`).
    pub who: String,
    /// The unit of work.
    pub subject: String,
    /// The contract that emitted the accept event.
    pub contract: String,
    /// The acceptance.
    pub accept: Ref,
    /// The who-evidence (may equal `accept`).
    pub work: Ref,
    /// Deal payer, market opener.
    pub counterparty: Option<String>,
    /// Smallest units: pay, reward or bet.
    pub amount: Option<u128>,
    /// One short code.
    pub detail: String,
    /// [`CHECK_CHAIN`] (slice 2 adds `chain+transcript`).
    pub check: &'static str,
    /// For a `revoke` row.
    pub revokes: Option<Revokes>,
}

impl Row {
    fn sort_key(&self) -> (u64, u64, &'static str, &str, u64, u64, &str) {
        (
            self.accept.block,
            self.accept.log,
            self.kind.as_str(),
            &self.who,
            self.work.block,
            self.work.log,
            &self.subject,
        )
    }
}

// ------------------------------------------------------------------ state

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reveal {
    miner: String,
    root: String,
    at: Ref,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Agree {
    validator: String,
    at: Ref,
}

/// One (round, shard) not released yet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct L1Shard {
    /// Miners in commit order (a miner's index is its `seq`).
    commits: Vec<String>,
    reveals: Vec<Reveal>,
    agrees: Vec<Agree>,
    finalized: Option<String>,
}

/// An accepted report's row facts, kept so a later event can revoke it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    key: String,
    who: String,
    work: Ref,
    /// The row's key once a version holds it.
    row_key: Option<i128>,
    revoked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    fact: String,
    submitter: String,
    value: String,
    work: Ref,
}

/// A `stake_correct` row a later Correct claim on its fact may supersede.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StakeRow {
    claim: String,
    value: String,
    who: String,
    work: Ref,
    row_key: Option<i128>,
    revoked: bool,
}

/// One staked fact's Correct rows. A decision by Observation is final
/// (`OnyxStakes.submit` refuses any later claim with `AlreadyFinal`), so
/// nothing changes the fact after it and it is pruned; a decision by Vote or
/// Fallback can still be superseded by a later Observed claim (`submit`,
/// OnyxStakes.sol:282-283).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FactRows {
    rows: Vec<StakeRow>,
    closed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoBet {
    who: String,
    /// Decimal (JSON numbers stop at 2^64).
    amount: String,
    work: Ref,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Market {
    opener: String,
    no_bets: Vec<NoBet>,
}

/// What the join knows between runs: the cursor, the work not accepted yet
/// and the facts later events are joined to (node operators, deal payers,
/// key signers, accepted reports). It serializes to JSON; a lost file costs
/// one rebuild from the deploy blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// [`FORMAT`].
    pub format: u32,
    /// `0x` hash of the `contracts.json` the state was built with.
    pub contracts: String,
    /// The next block to read: every log below it is applied.
    pub next_block: u64,
    shards: BTreeMap<String, L1Shard>,
    operators: BTreeMap<String, String>,
    payers: BTreeMap<u64, String>,
    signers: BTreeMap<String, String>,
    reports: BTreeMap<String, Report>,
    claims: BTreeMap<String, Claim>,
    facts: BTreeMap<String, FactRows>,
    markets: BTreeMap<String, Market>,
    /// Open dispute cases: case id -> `round:shard`. A shard with a case is
    /// never released (`release` needs `caseCount == 0`), so it gives no
    /// row; its case's end (`dEnd`) may emit a `Finalized` the join ignores.
    cases: BTreeMap<String, String>,
}

/// Facts the join takes from view calls, not from logs (slice 2 reads them
/// with `eth_call`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Views {
    /// A recorded report's observed time (`OnyxSignedBook.report(digest)`),
    /// by `0x` digest. Without it a compromise or a retirement revokes a
    /// report recorded up to [`MAX_CLOCK_SKEW_SECS`] before its time too
    /// (its observed time may be after it).
    pub observed: BTreeMap<String, u64>,
}

/// One version as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    /// The CSV (header line first).
    pub csv: String,
    /// Rows, `day_close` included.
    pub rows: u64,
    /// The key of each row a later event may revoke (`signed_report` by
    /// digest, `stake_correct` by claim id).
    pub keys: Vec<(Kind, String, i128)>,
}

/// The block range and hashes `day_close` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayClose {
    /// First block of the day read.
    pub from_block: u64,
    /// Last block of the day.
    pub to_block: u64,
    /// Hash of `to_block` (the next run checks it is still canonical).
    pub last_block_hash: Hash32,
    /// [`Contracts::hash`].
    pub contracts_hash: Hash32,
    /// [`exclusions_hash`] of the exclusions in force.
    pub exclusions_hash: Hash32,
}

/// Sort `logs` by (block, log index), drop exact repeats and refuse two
/// different logs at one position, a log outside `from..=to`, a log from an
/// address `contracts.json` does not list or before its contract's
/// `scan_from`, and block times that disagree or go back.
fn order(
    c: &Contracts,
    from: u64,
    to: u64,
    logs: &[ChainLog],
) -> Result<Vec<ChainLog>, EarlyError> {
    let mut v: Vec<&ChainLog> = logs.iter().collect();
    v.sort_by(|a, b| {
        (
            a.block,
            a.log_index,
            &a.topics,
            &a.data,
            a.address,
            a.tx,
            a.block_time,
        )
            .cmp(&(
                b.block,
                b.log_index,
                &b.topics,
                &b.data,
                b.address,
                b.tx,
                b.block_time,
            ))
    });
    // The sender (`from`) is not part of a log: a repeat that differs only
    // there is the same log.
    v.dedup_by(|a, b| {
        ChainLog {
            from: None,
            ..a.clone()
        } == ChainLog {
            from: None,
            ..b.clone()
        }
    });
    let mut out: Vec<ChainLog> = Vec::with_capacity(v.len());
    for l in v {
        if l.block < from || l.block > to {
            return Err(log_err(format!(
                "log at block {} outside the range {from}..={to}",
                l.block
            )));
        }
        let e = c.get(&l.address).ok_or_else(|| {
            log_err(format!(
                "log at {}:{} from {}, which contracts.json does not list",
                l.block,
                l.log_index,
                addr_text(&l.address)
            ))
        })?;
        if e.scan_from.is_some_and(|s| l.block < s) {
            return Err(log_err(format!(
                "log of {} at block {} is before its scan_from: contracts.json is wrong",
                e.name, l.block
            )));
        }
        if let Some(p) = out.last() {
            if p.block == l.block && p.log_index == l.log_index {
                return Err(log_err(format!(
                    "two different logs at {}:{}",
                    l.block, l.log_index
                )));
            }
            if (p.block == l.block && p.block_time != l.block_time)
                || (p.block < l.block && p.block_time > l.block_time)
            {
                return Err(log_err(format!(
                    "block times disagree or go back at block {}",
                    l.block
                )));
            }
        }
        out.push(l.clone());
    }
    Ok(out)
}

fn amount_text(v: u128) -> String {
    v.to_string()
}

/// One join step's context that lives within a batch only (the `Slashed`
/// log a `MissReward` of the same transaction names).
#[derive(Default)]
struct Batch {
    /// (tx, node) -> (subject, deal) of the last `Slashed` with cause MISSED.
    slashes: BTreeMap<(String, String), (String, u64)>,
}

impl State {
    /// An empty state bound to `c`, starting at its first counted block.
    pub fn new(c: &Contracts) -> State {
        State {
            format: FORMAT,
            contracts: h32_text(&c.hash),
            next_block: c.first_block().unwrap_or(0),
            ..State::default()
        }
    }

    /// Parse a saved state.
    pub fn from_json(b: &[u8]) -> Result<State, EarlyError> {
        let s: State = serde_json::from_slice(b).map_err(|e| join_err(format!("state: {e}")))?;
        if s.format != FORMAT {
            return Err(join_err(format!("state format {}", s.format)));
        }
        Ok(s)
    }

    /// The state as JSON (deterministic: every map is sorted).
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// The shards in the cache with no `Finalized` yet: (round, shard,
    /// `round:shard`). A shard that ends Empty emits no event, so the reader
    /// asks the settlement's `shardState` for these (EARLY-15).
    pub fn unfinalized_shards(&self) -> Vec<(u64, Hash32, String)> {
        self.shards
            .iter()
            .filter(|(_, s)| s.finalized.is_none())
            .filter_map(|(rs, _)| {
                let (r, h) = rs.split_once(':')?;
                let b: [u8; 32] = hex::decode(h.strip_prefix("0x")?).ok()?.try_into().ok()?;
                Some((r.parse().ok()?, Hash32(b), rs.clone()))
            })
            .collect()
    }

    /// Drop a shard the settlement says has ended without a root (Empty,
    /// Voided, NoValidRoot, Unresolved): it can never give a row.
    pub fn drop_shard(&mut self, rs: &str) {
        if self.shards.get(rs).is_some_and(|s| s.finalized.is_none()) {
            self.shards.remove(rs);
        }
    }

    /// The report digests whose observed time a compromise or a retirement
    /// in `logs` needs: the live reports of each key those events name, in
    /// the cache or accepted in `logs` (the reader asks the book's
    /// `report(digest)` for them and passes them as [`Views`]).
    pub fn digests_needing_observed(
        &self,
        c: &Contracts,
        logs: &[ChainLog],
    ) -> Result<BTreeSet<String>, EarlyError> {
        let mut keys = BTreeSet::new();
        let mut accepted: Vec<(String, String)> = Vec::new();
        for l in logs {
            let Some(e) = c.get(&l.address) else { continue };
            match decode(e.abi, l)? {
                Some(Ev::Compromised { key, .. } | Ev::Retired { key, .. }) => {
                    keys.insert(key);
                }
                Some(Ev::Report {
                    digest,
                    key,
                    outcome,
                }) if outcome == ACCEPTED => accepted.push((digest, key)),
                _ => {}
            }
        }
        let mut out: BTreeSet<String> = self
            .reports
            .iter()
            .filter(|(_, r)| !r.revoked && keys.contains(&r.key))
            .map(|(d, _)| d.clone())
            .collect();
        out.extend(
            accepted
                .into_iter()
                .filter(|(_, k)| keys.contains(k))
                .map(|(d, _)| d),
        );
        Ok(out)
    }

    /// Every address a reviewed key of the signed book signs with: these are
    /// publishers, and only the reviewed list may exclude one.
    pub fn publisher_signers(&self) -> BTreeSet<String> {
        self.signers.values().cloned().collect()
    }

    /// Apply the logs of blocks `from..=to` (`from` must be the cursor) and
    /// return the rows they accept. All or nothing: on an error the state
    /// is unchanged.
    pub fn apply(
        &mut self,
        c: &Contracts,
        from: u64,
        to: u64,
        logs: &[ChainLog],
        views: &Views,
    ) -> Result<Vec<Row>, EarlyError> {
        if self.contracts != h32_text(&c.hash) {
            return Err(join_err(
                "the state was built with another contracts.json: rebuild it from the deploy blocks",
            ));
        }
        if from != self.next_block || to < from {
            return Err(join_err(format!(
                "range {from}..={to} does not start at the cursor {}",
                self.next_block
            )));
        }
        let logs = order(c, from, to, logs)?;
        let mut next = self.clone();
        let mut rows = Vec::new();
        let mut batch = Batch::default();
        for l in &logs {
            let e = c
                .get(&l.address)
                .ok_or_else(|| log_err("unknown address"))?;
            if let Some(ev) = decode(e.abi, l)? {
                next.step(e, l, ev, views, &mut batch, &mut rows)?;
            }
        }
        next.next_block = to + 1;
        *self = next;
        Ok(rows)
    }

    fn revoke(&mut self, digest: &str, why: &str, contract: &str, at: &Ref, rows: &mut Vec<Row>) {
        let Some(r) = self.reports.get_mut(digest) else {
            return;
        };
        if r.revoked {
            return;
        }
        r.revoked = true;
        rows.push(Row {
            kind: Kind::Revoke,
            who: r.who.clone(),
            subject: digest.to_string(),
            contract: contract.to_string(),
            accept: at.clone(),
            work: r.work.clone(),
            counterparty: None,
            amount: None,
            detail: why.to_string(),
            check: CHECK_CHAIN,
            revokes: Some(match r.row_key {
                Some(k) => Revokes::Key(k),
                None => Revokes::SameVersion(Kind::SignedReport, digest.to_string()),
            }),
        });
    }

    /// Revoke every live report of `key` whose observed time may be at or
    /// after `t` (`also_recorded`: or that was recorded at or after it).
    #[allow(clippy::too_many_arguments)]
    fn revoke_key_from(
        &mut self,
        key: &str,
        t: u64,
        also_recorded: bool,
        why: &str,
        contract: &str,
        at: &Ref,
        views: &Views,
        rows: &mut Vec<Row>,
    ) {
        let hit: Vec<String> = self
            .reports
            .iter()
            .filter(|(_, r)| r.key == key && !r.revoked)
            .filter(|(d, r)| {
                let recorded = r.work.time;
                let observed_after = match views.observed.get(d.as_str()) {
                    Some(o) => *o >= t,
                    None => recorded.saturating_add(MAX_CLOCK_SKEW_SECS) >= t,
                };
                observed_after || (also_recorded && recorded >= t)
            })
            .map(|(d, _)| d.clone())
            .collect();
        for d in hit {
            self.revoke(&d, why, contract, at, rows);
        }
    }

    fn step(
        &mut self,
        e: &ContractEntry,
        l: &ChainLog,
        ev: Ev,
        views: &Views,
        batch: &mut Batch,
        rows: &mut Vec<Row>,
    ) -> Result<(), EarlyError> {
        let at = l.at();
        let row = |kind: Kind, who: String, subject: String, work: Ref| Row {
            kind,
            who,
            subject,
            contract: e.address.clone(),
            accept: at.clone(),
            work,
            counterparty: None,
            amount: None,
            detail: String::new(),
            check: CHECK_CHAIN,
            revokes: None,
        };
        match ev {
            Ev::Committed { rs, miner } => {
                self.shards.entry(rs).or_default().commits.push(miner);
            }
            Ev::Revealed { rs, miner, root } => {
                self.shards
                    .entry(rs)
                    .or_default()
                    .reveals
                    .push(Reveal { miner, root, at });
            }
            Ev::Verdict {
                rs,
                validator,
                kind,
            } => {
                if kind == AGREE {
                    self.shards
                        .entry(rs)
                        .or_default()
                        .agrees
                        .push(Agree { validator, at });
                }
            }
            Ev::Finalized { rs, root } => {
                if self.cases.values().any(|c| c == &rs) {
                    // The end of a case (`dEnd`, OnyxSettlementExt.sol:341):
                    // upheld (same root) or overturned (another root), the
                    // shard is never released, so it gives no row.
                    return Ok(());
                }
                let s = self.shards.entry(rs.clone()).or_default();
                if s.finalized.is_some() {
                    // A second Finalized without its CaseOpened log: only a
                    // case's end emits one, so the shard is cased. No row.
                    self.shards.remove(&rs);
                    return Ok(());
                }
                s.finalized = Some(root);
            }
            Ev::ShardVoided { rs } => {
                self.shards.remove(&rs);
            }
            Ev::RoundVoided { round } => {
                let p = format!("{round}:");
                self.shards.retain(|k, _| !k.starts_with(&p));
            }
            Ev::CaseOpened { case, rs } => {
                // A challenge of a Finalized shard or a rerun of a Pending
                // one (OnyxDisputes._open): either way `release` can never
                // run, so its work leaves the cache.
                self.shards.remove(&rs);
                if self.cases.insert(case.clone(), rs).is_some() {
                    return Err(join_err(format!("case {case} opened twice")));
                }
            }
            Ev::CaseClosed { case } => {
                if self.cases.remove(&case).is_none() {
                    return Err(join_err(format!(
                        "case {case} closed without its CaseOpened log"
                    )));
                }
            }
            Ev::Released { rs } => {
                let s = self.shards.remove(&rs).unwrap_or_default();
                let root = s.finalized.clone().ok_or_else(|| {
                    join_err(format!("Released for {rs} without its Finalized log"))
                })?;
                let seq = |m: &str| {
                    s.commits.iter().position(|c| c == m).ok_or_else(|| {
                        join_err(format!("{m} revealed on {rs} without its Committed log"))
                    })
                };
                let paid: Vec<&Reveal> = s.reveals.iter().filter(|r| r.root == root).collect();
                let mut first: Option<(usize, &str)> = None;
                for r in &paid {
                    let q = seq(&r.miner)?;
                    if first.is_none_or(|(b, _)| q < b) {
                        first = Some((q, &r.miner));
                    }
                }
                for r in paid {
                    let mut x = row(Kind::L1Root, r.miner.clone(), rs.clone(), r.at.clone());
                    x.detail = if first.is_some_and(|(_, m)| m == r.miner) {
                        "winner"
                    } else {
                        "also"
                    }
                    .into();
                    rows.push(x);
                }
                for a in &s.agrees {
                    let mut x = row(
                        Kind::L1Verdict,
                        a.validator.clone(),
                        rs.clone(),
                        a.at.clone(),
                    );
                    x.detail = "agree".into();
                    rows.push(x);
                }
            }
            Ev::KeyRegistered { key, signer } => {
                if self.signers.insert(key.clone(), signer).is_some() {
                    return Err(join_err(format!("key {key} registered twice")));
                }
            }
            Ev::Report {
                digest,
                key,
                outcome,
            } => {
                if outcome == ACCEPTED {
                    let who = self.signers.get(&key).cloned().ok_or_else(|| {
                        join_err(format!(
                            "report {digest} under key {key} without its KeyRegistered log"
                        ))
                    })?;
                    if self.reports.contains_key(&digest) {
                        return Err(join_err(format!("report {digest} accepted twice")));
                    }
                    self.reports.insert(
                        digest.clone(),
                        Report {
                            key,
                            who: who.clone(),
                            work: at.clone(),
                            row_key: None,
                            revoked: false,
                        },
                    );
                    let mut x = row(Kind::SignedReport, who, digest, at.clone());
                    x.detail = "accepted".into();
                    rows.push(x);
                }
            }
            Ev::Withdrawn { digest } => {
                self.revoke(&digest, "withdrawn", &e.address, &at, rows);
            }
            Ev::Equivocated { first, second } => {
                self.revoke(&first, "equivocated", &e.address, &at, rows);
                self.revoke(&second, "equivocated", &e.address, &at, rows);
            }
            Ev::Compromised { key, since } => {
                // R-SIG-6a: observed or accepted at or after `since`.
                self.revoke_key_from(
                    &key,
                    since,
                    true,
                    "compromised",
                    &e.address,
                    &at,
                    views,
                    rows,
                );
            }
            Ev::Retired { key, at: from } => {
                // L3R5-F1: observed at or after `at`.
                self.revoke_key_from(&key, from, false, "retired", &e.address, &at, views, rows);
            }
            Ev::Activated { node, operator } => {
                if self.operators.insert(node.clone(), operator).is_some() {
                    return Err(join_err(format!("node {node} activated twice")));
                }
            }
            Ev::Deal { deal, payer } => {
                if self.payers.insert(deal, payer).is_some() {
                    return Err(join_err(format!("deal {deal} created twice")));
                }
            }
            Ev::Answered {
                subject,
                deal,
                node,
                passed,
                paid,
            } => {
                if passed {
                    let op = self.operators.get(&node).cloned().ok_or_else(|| {
                        join_err(format!("answer by node {node} without its Activated log"))
                    })?;
                    let payer = self.payers.get(&deal).cloned().ok_or_else(|| {
                        join_err(format!("answer on deal {deal} without its DealCreated log"))
                    })?;
                    let mut x = row(Kind::StorageProof, op.clone(), subject, at.clone());
                    x.detail = if payer == op { "self" } else { "other" }.into();
                    x.counterparty = Some(payer);
                    x.amount = Some(paid);
                    rows.push(x);
                }
            }
            Ev::Slashed {
                node,
                subject,
                deal,
                cause,
            } => {
                if cause == MISSED {
                    batch.slashes.insert((at.tx.clone(), node), (subject, deal));
                }
            }
            Ev::MissReward {
                node,
                reporter,
                amount,
            } => {
                let (subject, deal) = batch
                    .slashes
                    .get(&(at.tx.clone(), node.clone()))
                    .cloned()
                    .ok_or_else(|| {
                        join_err(format!(
                            "MissReward for node {node} without the Slashed log of its transaction"
                        ))
                    })?;
                let payer = self.payers.get(&deal).cloned().ok_or_else(|| {
                    join_err(format!("miss on deal {deal} without its DealCreated log"))
                })?;
                let mut x = row(Kind::StorageMissReport, reporter, subject, at.clone());
                x.detail = "reward".into();
                x.counterparty = Some(payer);
                x.amount = Some(amount);
                rows.push(x);
            }
            Ev::Submitted {
                claim,
                fact,
                submitter,
                value,
            } => {
                if self
                    .claims
                    .insert(
                        claim.clone(),
                        Claim {
                            fact,
                            submitter,
                            value,
                            work: at,
                        },
                    )
                    .is_some()
                {
                    return Err(join_err(format!("claim {claim} submitted twice")));
                }
            }
            Ev::Decided { claim, state, by } => {
                let c = self.claims.remove(&claim).ok_or_else(|| {
                    join_err(format!("claim {claim} decided without its Submitted log"))
                })?;
                if state == CORRECT {
                    let f = self.facts.entry(c.fact.clone()).or_default();
                    if f.closed {
                        return Err(join_err(format!(
                            "claim {claim} decided Correct on fact {}, final since an observation",
                            c.fact
                        )));
                    }
                    // A Correct claim with another value supersedes every live
                    // row of the fact (its acceptedClaim moves).
                    for r in f
                        .rows
                        .iter_mut()
                        .filter(|r| !r.revoked && r.value != c.value)
                    {
                        r.revoked = true;
                        rows.push(Row {
                            kind: Kind::Revoke,
                            who: r.who.clone(),
                            subject: r.claim.clone(),
                            contract: e.address.clone(),
                            accept: at.clone(),
                            work: r.work.clone(),
                            counterparty: None,
                            amount: None,
                            detail: "superseded".into(),
                            check: CHECK_CHAIN,
                            revokes: Some(match r.row_key {
                                Some(k) => Revokes::Key(k),
                                None => Revokes::SameVersion(Kind::StakeCorrect, r.claim.clone()),
                            }),
                        });
                    }
                    f.rows.push(StakeRow {
                        claim: claim.clone(),
                        value: c.value,
                        who: c.submitter.clone(),
                        work: c.work.clone(),
                        row_key: None,
                        revoked: false,
                    });
                    f.closed = by == BY_OBSERVATION;
                    let mut x = row(Kind::StakeCorrect, c.submitter, claim, c.work);
                    x.detail = match by {
                        1 => "by=Observation",
                        2 => "by=Vote",
                        3 => "by=Fallback",
                        4 => "by=Timeout",
                        _ => "by=None",
                    }
                    .into();
                    rows.push(x);
                }
            }
            Ev::Opened { market, opener } => {
                if self
                    .markets
                    .insert(
                        market.clone(),
                        Market {
                            opener,
                            no_bets: Vec::new(),
                        },
                    )
                    .is_some()
                {
                    return Err(join_err(format!("market {market} opened twice")));
                }
            }
            Ev::Bet {
                market,
                who,
                side,
                amount,
            } => {
                let m = self.markets.get_mut(&market).ok_or_else(|| {
                    join_err(format!(
                        "bet in market {market} without its MarketOpened log"
                    ))
                })?;
                if side == NO {
                    m.no_bets.push(NoBet {
                        who,
                        amount: amount_text(amount),
                        work: at,
                    });
                }
            }
            Ev::Resolved { market, winner } => {
                let m = self.markets.remove(&market).ok_or_else(|| {
                    join_err(format!(
                        "market {market} resolved without its MarketOpened log"
                    ))
                })?;
                if winner == NO {
                    for b in m.no_bets {
                        let amount = b
                            .amount
                            .parse::<u128>()
                            .map_err(|_| join_err("bet amount in the state"))?;
                        let mut x = row(Kind::CorrectionFound, b.who, market.clone(), b.work);
                        x.detail = "no_won".into();
                        x.counterparty = Some(m.opener.clone());
                        x.amount = Some(amount);
                        rows.push(x);
                    }
                }
            }
            Ev::MarketVoided { market } => {
                if self.markets.remove(&market).is_none() {
                    return Err(join_err(format!(
                        "market {market} voided without its MarketOpened log"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Note the keys a written version gave the rows a later event may
    /// revoke, so the `revoke` row names them.
    pub fn record_keys(&mut self, keys: &[(Kind, String, i128)]) -> Result<(), EarlyError> {
        for (kind, s, k) in keys {
            let slot = match kind {
                Kind::SignedReport => self.reports.get_mut(s).map(|r| &mut r.row_key),
                Kind::StakeCorrect => self
                    .facts
                    .values_mut()
                    .flat_map(|f| f.rows.iter_mut())
                    .find(|r| &r.claim == s)
                    .map(|r| &mut r.row_key),
                _ => None,
            }
            .ok_or_else(|| join_err(format!("no {} row {s} to key", kind.as_str())))?;
            if slot.is_some() {
                return Err(join_err(format!(
                    "{} {s} already has a row key",
                    kind.as_str()
                )));
            }
            *slot = Some(*k);
        }
        Ok(())
    }

    /// Drop what no later event can turn into a row or change: revoked
    /// reports and stake rows, and facts final since an observation. (Shards
    /// released, cased or voided, decided claims, closed cases and resolved or
    /// voided markets leave at their event.) Run after [`Self::record_keys`]:
    /// output never depends on it.
    pub fn prune(&mut self) {
        self.reports.retain(|_, r| !r.revoked);
        for f in self.facts.values_mut() {
            f.rows.retain(|r| !r.revoked);
        }
        self.facts.retain(|_, f| !f.closed && !f.rows.is_empty());
    }

    /// One day end to end: apply `from..=to`, write the version for
    /// `record_day` and note its keys. All or nothing.
    pub fn record_day(
        &mut self,
        c: &Contracts,
        logs: &[ChainLog],
        views: &Views,
        record_day: u32,
        close: &DayClose,
    ) -> Result<Version, EarlyError> {
        let mut next = self.clone();
        let rows = next.apply(c, close.from_block, close.to_block, logs, views)?;
        let v = write_day(record_day, &rows, close)?;
        next.record_keys(&v.keys)?;
        next.prune();
        *self = next;
        Ok(v)
    }
}

// ------------------------------------------------------------------ days

/// The UTC day (days since 1970-01-01) of a unix time: 00:00:00 belongs to
/// the day it starts.
pub fn utc_day(unix: u64) -> u32 {
    u32::try_from(unix / SECS_PER_DAY).unwrap_or(u32::MAX)
}

/// `YYYY-MM-DD` of a day number (proleptic Gregorian, H. Hinnant's
/// `civil_from_days`).
pub fn day_text(day: u32) -> String {
    let z = i64::from(day) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// ------------------------------------------------------------------ CSV and schemas

/// The record's columns: name, kind, nullable.
pub const RECORD_COLUMNS: [(&str, ColumnKind, bool); 18] = [
    ("key", ColumnKind::Int, false),
    ("day", ColumnKind::Text, false),
    ("kind", ColumnKind::Text, false),
    ("who", ColumnKind::Text, false),
    ("subject", ColumnKind::Text, false),
    ("contract", ColumnKind::Text, false),
    ("accept_tx", ColumnKind::Text, false),
    ("accept_block", ColumnKind::Int, false),
    ("accept_log", ColumnKind::Int, false),
    ("work_tx", ColumnKind::Text, false),
    ("work_block", ColumnKind::Int, false),
    ("work_log", ColumnKind::Int, false),
    ("counterparty", ColumnKind::Text, true),
    ("amount", ColumnKind::Int, true),
    ("detail", ColumnKind::Text, false),
    ("check", ColumnKind::Text, false),
    ("revokes", ColumnKind::Int, true),
    ("rules", ColumnKind::Int, false),
];

/// The exclusions' columns.
pub const EXCLUSION_COLUMNS: [(&str, ColumnKind, bool); 6] = [
    ("key", ColumnKind::Int, false),
    ("day", ColumnKind::Text, false),
    ("address", ColumnKind::Text, false),
    ("action", ColumnKind::Text, false),
    ("reason", ColumnKind::Text, false),
    ("source", ColumnKind::Text, false),
];

fn schema_of(cols: &[(&str, ColumnKind, bool)]) -> Schema {
    Schema {
        columns: cols
            .iter()
            .map(|(n, k, null)| Column {
                name: (*n).to_string(),
                kind: *k,
                nullable: *null,
            })
            .collect(),
        key: 0,
    }
}

/// The record's schema (`deploy/early-record/schema.json`).
pub fn record_schema() -> Schema {
    schema_of(&RECORD_COLUMNS)
}

/// The exclusions' schema (`deploy/early-record/exclusions-schema.json`).
pub fn exclusions_schema() -> Schema {
    schema_of(&EXCLUSION_COLUMNS)
}

/// The shard files `onyx-publish build` cuts the record's CSV into (rows
/// checked against [`record_schema`], about `target` bytes a shard): what
/// `early-record verify` compares with a built manifest's shard roots.
pub fn shard_files(csv: &str, target: u64) -> Result<Vec<Vec<u8>>, EarlyError> {
    shard_files_of(&record_schema(), csv, target)
}

/// [`shard_files`] for any schema (the exclusions' too).
pub fn shard_files_of(schema: &Schema, csv: &str, target: u64) -> Result<Vec<Vec<u8>>, EarlyError> {
    use onyx_bulk::input::{Format, Records};
    let schema = schema.clone();
    let mut r = Records::new(csv.as_bytes(), schema.clone(), Format::Csv)
        .map_err(|e| write_err(e.to_string()))?;
    let mut b = onyx_bulk::shard::ShardBuilder::new(target);
    let mut out = Vec::new();
    while let Some((_, cells)) = r.next_record().map_err(|e| write_err(e.to_string()))? {
        let row =
            onyx_bulk::row::encode_row(&schema, &cells).map_err(|e| write_err(e.to_string()))?;
        let key = onyx_bulk::row::key_of(&schema, &cells).ok_or_else(|| write_err("no key"))?;
        if let Some(full) = b.push(row, key).map_err(|e| write_err(e.to_string()))? {
            out.push(onyx_bulk::shard::encode_shard(&full));
        }
    }
    if let Some(last) = b.finish() {
        out.push(onyx_bulk::shard::encode_shard(&last));
    }
    Ok(out)
}

/// A schema file's text as committed: pretty JSON and a line feed.
pub fn schema_file(s: &Schema) -> String {
    let mut t = serde_json::to_string_pretty(&s.to_json()).unwrap_or_default();
    t.push('\n');
    t
}

/// A cell: `None` is an empty field (null in a nullable column, empty text
/// otherwise).
enum Cell<'a> {
    Int(i128),
    Text(&'a str),
    Null,
}

fn csv_line(cells: &[Cell]) -> Result<String, EarlyError> {
    let mut out = String::new();
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match c {
            Cell::Int(v) => out.push_str(&v.to_string()),
            Cell::Null => {}
            Cell::Text(t) => {
                if t.len() > MAX_TEXT_CELL {
                    return Err(write_err(format!("a text cell over {MAX_TEXT_CELL} bytes")));
                }
                onyx_bulk::row::check_text(t).map_err(write_err)?;
                if t.contains([',', '"', '\n', '\r']) {
                    out.push('"');
                    out.push_str(&t.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(t);
                }
            }
        }
    }
    out.push('\n');
    Ok(out)
}

fn header(cols: &[(&str, ColumnKind, bool)]) -> String {
    let mut h = cols.iter().map(|c| c.0).collect::<Vec<_>>().join(",");
    h.push('\n');
    h
}

fn int(v: u64) -> Cell<'static> {
    Cell::Int(i128::from(v))
}

/// Write version `record_day` of the record: `rows` sorted by (accept block,
/// accept log, kind, who; then the work reference and subject, so the order
/// is total), keys `(record_day << 24) + i`, a `day_close` row last whose
/// subject is `from-to` and whose detail names the last block hash, the
/// hashes of `contracts.json` and of the exclusions in force, and the row
/// count. Every row must be accepted in `from..=to` on `record_day`.
pub fn write_day(record_day: u32, rows: &[Row], close: &DayClose) -> Result<Version, EarlyError> {
    if close.from_block > close.to_block {
        return Err(write_err("from_block after to_block"));
    }
    let n = rows.len() as u64;
    if n + 1 > MAX_ROWS_PER_VERSION {
        return Err(write_err(format!(
            "{} rows: a version holds at most {MAX_ROWS_PER_VERSION}",
            n + 1
        )));
    }
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Err(write_err("the same row twice"));
    }
    let base = i128::from(record_day) << 24;
    let key_of = |i: usize| base + i as i128;
    let mut csv = header(&RECORD_COLUMNS);
    let mut keys = Vec::new();
    let day = day_text(record_day);
    for (i, r) in sorted.iter().enumerate() {
        if r.kind == Kind::DayClose {
            return Err(write_err("a day_close row among the rows"));
        }
        if r.accept.block < close.from_block || r.accept.block > close.to_block {
            return Err(write_err(format!(
                "a row accepted at block {} outside {}..={}",
                r.accept.block, close.from_block, close.to_block
            )));
        }
        if utc_day(r.accept.time) != record_day {
            return Err(write_err(format!(
                "a row accepted on {} in the version of {day}",
                day_text(utc_day(r.accept.time))
            )));
        }
        let revokes = match &r.revokes {
            None => None,
            Some(Revokes::Key(k)) => Some(*k),
            Some(Revokes::SameVersion(kind, s)) => Some(
                sorted
                    .iter()
                    .position(|o| o.kind == *kind && &o.subject == s)
                    .map(key_of)
                    .ok_or_else(|| write_err(format!("a revoke of {s} with no row to name")))?,
            ),
        };
        if (r.kind == Kind::Revoke) != revokes.is_some() {
            return Err(write_err("revokes is set on revoke rows only"));
        }
        let amount = r
            .amount
            .map(|a| i128::try_from(a).map_err(|_| write_err("amount over i128")))
            .transpose()?;
        if matches!(r.kind, Kind::SignedReport | Kind::StakeCorrect) {
            keys.push((r.kind, r.subject.clone(), key_of(i)));
        }
        csv.push_str(&csv_line(&[
            Cell::Int(key_of(i)),
            Cell::Text(&day),
            Cell::Text(r.kind.as_str()),
            Cell::Text(&r.who),
            Cell::Text(&r.subject),
            Cell::Text(&r.contract),
            Cell::Text(&r.accept.tx),
            int(r.accept.block),
            int(r.accept.log),
            Cell::Text(&r.work.tx),
            int(r.work.block),
            int(r.work.log),
            r.counterparty.as_deref().map_or(Cell::Null, Cell::Text),
            amount.map_or(Cell::Null, Cell::Int),
            Cell::Text(&r.detail),
            Cell::Text(r.check),
            revokes.map_or(Cell::Null, Cell::Int),
            int(u64::from(RULES)),
        ])?);
    }
    let subject = format!("{}-{}", close.from_block, close.to_block);
    let detail = format!(
        "block={};contracts={};exclusions={};rows={}",
        h32_text(&close.last_block_hash),
        h32_text(&close.contracts_hash),
        h32_text(&close.exclusions_hash),
        n + 1
    );
    csv.push_str(&csv_line(&[
        Cell::Int(key_of(sorted.len())),
        Cell::Text(&day),
        Cell::Text(Kind::DayClose.as_str()),
        Cell::Text(""),
        Cell::Text(&subject),
        Cell::Text(""),
        Cell::Text(""),
        int(close.to_block),
        int(0),
        Cell::Text(""),
        int(close.from_block),
        int(0),
        Cell::Null,
        Cell::Null,
        Cell::Text(&detail),
        Cell::Text(CHECK_CHAIN),
        Cell::Null,
        int(u64::from(RULES)),
    ])?);
    Ok(Version {
        csv,
        rows: n + 1,
        keys,
    })
}

// ------------------------------------------------------------------ exclusions

/// Why an address is excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// A role getter answered it (an operator's address).
    Role,
    /// Its private key is public, or it is a placeholder nobody owns.
    Fixture,
    /// The reviewed list of the operator's test keys (slice 3).
    OperatorKey,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Role => "role",
            Reason::Fixture => "fixture",
            Reason::OperatorKey => "operator_key",
        }
    }
}

/// An exclusion row's action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// From this version on, readers leave the address out.
    Add,
    /// From this version on, readers count it again.
    Remove,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Action::Add => "add",
            Action::Remove => "remove",
        }
    }
}

/// One excluded address with its reason and source (a getter or a file line).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    /// Lowercase `0x` address.
    pub address: String,
    /// Why.
    pub reason: Reason,
    /// `Contract.getter()`, a file and line, or the fixture's name.
    pub source: String,
}

/// One row of the exclusions dataset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusionRow {
    /// `(record_day << 24) + i`.
    pub key: i128,
    /// `YYYY-MM-DD` of the version.
    pub day: String,
    /// Add or remove.
    pub action: Action,
    /// The exclusion.
    pub exclusion: Exclusion,
}

/// The local test chain's public dev accounts (anvil's default mnemonic,
/// indexes 0 to 9; their private keys are published, so anyone can sign
/// with them).
pub const ANVIL_ACCOUNTS: [&str; 10] = [
    "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266",
    "0x70997970c51812dc3a010c7d01b50e0d17dc79c8",
    "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
    "0x90f79bf6eb2c4f870365e785982e1f101e93b906",
    "0x15d34aaf54267db7d7c367839aaf71a00a2c6a65",
    "0x9965507d1a55bcc2695c58ba16fb37d819b0a4dc",
    "0x976ea74026e726554db657fa54763abd0c3a0aa9",
    "0x14dc79964da2c08b23698b3d3cc7ca32193d9955",
    "0x23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f",
    "0xa0ee7a142d267c1f36714e4a8f75612f20a79720",
];

/// Placeholder addresses the scripts use, with the file line that holds
/// each (a test checks the line still does).
pub const PLACEHOLDERS: [(&str, &str); 6] = [
    (
        "0x0000000000000000000000000000000000000001",
        "scripts/storage-sepolia-tests.sh:123",
    ),
    (
        "0x0000000000000000000000000000000000000002",
        "scripts/storage-sepolia-tests.sh:124",
    ),
    (
        "0x0000000000000000000000000000000000000003",
        "scripts/storage-sepolia-tests.sh:125",
    ),
    (
        "0x00000000000000000000000000000000000000aa",
        "scripts/storage-sepolia-tests.sh:126",
    ),
    (
        "0x000000000000000000000000000000000000c0de",
        "scripts/storage-sepolia-tests.sh:71",
    ),
    (
        "0x00000000000000000000000000000000000c0de1",
        "scripts/recheck-anvil.sh:79",
    ),
];

/// The fixed list: the dev accounts and the placeholders, reason `fixture`.
pub fn fixed_exclusions() -> Vec<Exclusion> {
    let mut v: Vec<Exclusion> = ANVIL_ACCOUNTS
        .iter()
        .enumerate()
        .map(|(i, a)| Exclusion {
            address: (*a).to_string(),
            reason: Reason::Fixture,
            source: format!("anvil default account {i}"),
        })
        .collect();
    v.extend(PLACEHOLDERS.iter().map(|(a, s)| Exclusion {
        address: (*a).to_string(),
        reason: Reason::Fixture,
        source: (*s).to_string(),
    }));
    v
}

/// One role getter to call at a day's last block (slice 2 sends it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleCall {
    /// The contract's name.
    pub contract: String,
    /// Its address.
    pub to: String,
    /// The getter (`owner()`).
    pub getter: &'static str,
    /// `eth_call` params: `[{to, data}, block]`.
    pub params: Value,
}

impl RoleCall {
    /// The exclusion an `eth_call` answer gives: `None` for the zero address
    /// (an unset role).
    pub fn exclusion(&self, answer: &str) -> Result<Option<Exclusion>, EarlyError> {
        let b = answer
            .strip_prefix("0x")
            .and_then(|s| hex::decode(s).ok())
            .ok_or_else(|| excl_err(format!("{}.{}: not 0x hex", self.contract, self.getter)))?;
        if b.len() != 32 {
            return Err(excl_err(format!(
                "{}.{}: {} bytes, want one word",
                self.contract,
                self.getter,
                b.len()
            )));
        }
        let a = abi::word_addr(&b, 0)
            .map_err(|e| excl_err(format!("{}.{}: {e}", self.contract, self.getter)))?;
        if a == [0u8; 20] {
            return Ok(None);
        }
        Ok(Some(Exclusion {
            address: addr_text(&a),
            reason: Reason::Role,
            source: format!("{}.{}", self.contract, self.getter),
        }))
    }
}

/// Every role getter of `c`'s contracts, as `eth_call`s at `block`.
pub fn role_calls(c: &Contracts, block: u64) -> Vec<RoleCall> {
    let mut out = Vec::new();
    for e in &c.file.contracts {
        for g in e.abi.roles() {
            out.push(RoleCall {
                contract: e.name.clone(),
                to: e.address.clone(),
                getter: g,
                params: json!([
                    {"to": e.address, "data": format!("0x{}", hex::encode(abi::selector(g)))},
                    format!("0x{block:x}")
                ]),
            });
        }
    }
    out
}

/// Replay exclusion rows (ascending keys; an add of an address in force or
/// a remove of one not in force is refused) to the exclusions in force.
pub fn exclusions_in_force(
    rows: &[ExclusionRow],
) -> Result<BTreeMap<String, Exclusion>, EarlyError> {
    let mut m = BTreeMap::new();
    let mut last: Option<i128> = None;
    for r in rows {
        if last.is_some_and(|k| r.key <= k) {
            return Err(excl_err(format!("key {} does not ascend", r.key)));
        }
        last = Some(r.key);
        let a = &r.exclusion.address;
        match r.action {
            Action::Add => {
                if m.insert(a.clone(), r.exclusion.clone()).is_some() {
                    return Err(excl_err(format!("{a} added while in force")));
                }
            }
            Action::Remove => {
                if m.remove(a).is_none() {
                    return Err(excl_err(format!("{a} removed while not in force")));
                }
            }
        }
    }
    Ok(m)
}

/// What [`exclusion_changes`] gives: the rows to append, and the wanted
/// entries it left out because they name a publisher key without the
/// reviewed list's `operator_key` reason (a hint for the reviewer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusionChanges {
    /// Rows to append (removes first, then adds).
    pub rows: Vec<ExclusionRow>,
    /// Wanted entries not excluded: they name a publisher key.
    pub skipped: Vec<Exclusion>,
}

/// The rows that move the exclusions in force to `wanted` on `record_day`:
/// a `remove` for each address in force no longer wanted, then an `add` for
/// each new one, each group sorted by address (an address wanted twice keeps
/// its first reason and source in sort order).
///
/// A publisher key (a signer of a key the signed book registered) is
/// excluded only by an `operator_key` entry, which only the reviewed list
/// ([`ReviewedFile`]) gives: the record never states who runs any other
/// publisher. A role or fixture entry naming a publisher key is skipped, not
/// refused, so an address that becomes a signer later stops no run; when it
/// was in force, a `remove` row follows.
pub fn exclusion_changes(
    in_force: &BTreeMap<String, Exclusion>,
    wanted: &[Exclusion],
    publishers: &BTreeSet<String>,
    record_day: u32,
) -> Result<ExclusionChanges, EarlyError> {
    let mut want: BTreeMap<String, Exclusion> = BTreeMap::new();
    let mut skipped = Vec::new();
    let mut sorted = wanted.to_vec();
    sorted.sort();
    for w in sorted {
        if parse_address(&w.address).is_none() {
            return Err(excl_err(format!(
                "{}: not a lowercase 0x address",
                w.address
            )));
        }
        if w.source.is_empty() || w.source.len() > MAX_TEXT_CELL {
            return Err(excl_err(format!(
                "{}: a source of 1 to {MAX_TEXT_CELL} bytes",
                w.address
            )));
        }
        if publishers.contains(&w.address) && w.reason != Reason::OperatorKey {
            skipped.push(w);
            continue;
        }
        want.entry(w.address.clone()).or_insert(w);
    }
    let day = day_text(record_day);
    let base = i128::from(record_day) << 24;
    let mut rows = Vec::new();
    let removes = in_force.iter().filter(|(a, _)| !want.contains_key(*a));
    let adds = want.iter().filter(|(a, _)| !in_force.contains_key(*a));
    for (action, (_, x)) in removes
        .map(|p| (Action::Remove, p))
        .chain(adds.map(|p| (Action::Add, p)))
    {
        rows.push(ExclusionRow {
            key: base + rows.len() as i128,
            day: day.clone(),
            action,
            exclusion: x.clone(),
        });
    }
    Ok(ExclusionChanges { rows, skipped })
}

/// One reviewed operator key: an address a document names as the
/// operator's own test key, in force on the days `from_day` (inclusive) to
/// `until_day` (exclusive, if any). A replay from the deploy blocks applies
/// each entry only on its own days, so a later entry never changes an
/// earlier day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedKey {
    /// Lowercase `0x` address.
    pub address: String,
    /// The `source` cell (a document and its row, short).
    pub source: String,
    /// The document that names the key.
    pub doc: String,
    /// The row label in that document's table.
    pub doc_row: String,
    /// The first day it is excluded (`YYYY-MM-DD`).
    pub from_day: String,
    /// The first day it is no longer excluded (`YYYY-MM-DD`): a `remove` row.
    #[serde(default)]
    pub until_day: Option<String>,
}

/// The record's own publisher key, from the day it is excluded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordKey {
    /// Lowercase `0x` address.
    pub address: String,
    /// The first day it is excluded (`YYYY-MM-DD`).
    pub from_day: String,
}

/// `deploy/early-record/exclusions.json`: the reviewed list of the
/// operator's own test keys (reason `operator_key`). It names the operator's
/// demo publisher and, once registered, the record's own publisher key. It
/// never names a data publisher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedFile {
    /// [`FORMAT`].
    pub format: u32,
    /// Free text for readers.
    #[serde(default)]
    pub note: String,
    /// The reviewed keys.
    pub operator_keys: Vec<ReviewedKey>,
    /// The record's own publisher key: `null` until slice 3 registers it.
    pub record_publisher: Option<RecordKey>,
}

/// The `source` cell of the record's own publisher key.
pub const RECORD_PUBLISHER_SOURCE: &str = "the record's own publisher key";

/// `YYYY-MM-DD` as days since 1970-01-01 (the inverse of [`day_text`]).
pub fn parse_day(s: &str) -> Option<u32> {
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 || p[0].len() != 4 || p[1].len() != 2 || p[2].len() != 2 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // H. Hinnant's days_from_civil.
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = u32::try_from(era * 146_097 + doe - 719_468).ok()?;
    (day_text(days) == s).then_some(days)
}

impl ReviewedFile {
    /// Parse and check: format 1, lowercase nonzero unique addresses, short
    /// sources, valid days (`until_day` after `from_day`).
    pub fn parse(bytes: &[u8]) -> Result<ReviewedFile, EarlyError> {
        let f: ReviewedFile =
            serde_json::from_slice(bytes).map_err(|e| excl_err(format!("exclusions.json: {e}")))?;
        if f.format != FORMAT {
            return Err(excl_err(format!("exclusions.json: format {}", f.format)));
        }
        let mut seen = BTreeSet::new();
        let all = f
            .operator_keys
            .iter()
            .map(|k| {
                (
                    k.address.as_str(),
                    k.source.as_str(),
                    k.from_day.as_str(),
                    k.until_day.as_deref(),
                )
            })
            .chain(f.record_publisher.as_ref().map(|r| {
                (
                    r.address.as_str(),
                    RECORD_PUBLISHER_SOURCE,
                    r.from_day.as_str(),
                    None,
                )
            }));
        for (a, src, from, until) in all {
            if parse_address(a).is_none_or(|b| b == [0u8; 20]) {
                return Err(excl_err(format!(
                    "exclusions.json: {a} is not a lowercase nonzero address"
                )));
            }
            if !seen.insert(a.to_string()) {
                return Err(excl_err(format!("exclusions.json: {a} twice")));
            }
            if src.is_empty() || src.len() > MAX_TEXT_CELL {
                return Err(excl_err(format!(
                    "exclusions.json: {a}: a source of 1 to {MAX_TEXT_CELL} bytes"
                )));
            }
            let f0 = parse_day(from)
                .ok_or_else(|| excl_err(format!("exclusions.json: {a}: from_day {from:?}")))?;
            if let Some(u) = until {
                if parse_day(u).is_none_or(|u| u <= f0) {
                    return Err(excl_err(format!(
                        "exclusions.json: {a}: until_day {u:?} must be a day after from_day"
                    )));
                }
            }
        }
        Ok(f)
    }

    /// The reviewed entries in force on `day`, as exclusions (reason
    /// `operator_key`).
    pub fn exclusions_on(&self, day: u32) -> Vec<Exclusion> {
        let on = |from: &str, until: Option<&str>| {
            parse_day(from).is_some_and(|f| f <= day)
                && until.and_then(parse_day).is_none_or(|u| day < u)
        };
        let mut v: Vec<Exclusion> = self
            .operator_keys
            .iter()
            .filter(|k| on(&k.from_day, k.until_day.as_deref()))
            .map(|k| Exclusion {
                address: k.address.clone(),
                reason: Reason::OperatorKey,
                source: k.source.clone(),
            })
            .collect();
        if let Some(r) = self
            .record_publisher
            .as_ref()
            .filter(|r| on(&r.from_day, None))
        {
            v.push(Exclusion {
                address: r.address.clone(),
                reason: Reason::OperatorKey,
                source: RECORD_PUBLISHER_SOURCE.into(),
            });
        }
        v
    }
}

/// The exclusions' CSV (header line first).
pub fn exclusions_csv(rows: &[ExclusionRow]) -> Result<String, EarlyError> {
    let mut csv = header(&EXCLUSION_COLUMNS);
    for r in rows {
        csv.push_str(&csv_line(&[
            Cell::Int(r.key),
            Cell::Text(&r.day),
            Cell::Text(&r.exclusion.address),
            Cell::Text(r.action.as_str()),
            Cell::Text(r.exclusion.reason.as_str()),
            Cell::Text(&r.exclusion.source),
        ])?);
    }
    Ok(csv)
}

/// The hash `day_close` names for the exclusions in force: keccak256 of
/// `onyx-early-exclusions-v1\n` and one `address,reason,source\n` line per
/// address, in address order.
pub fn exclusions_hash(in_force: &BTreeMap<String, Exclusion>) -> Hash32 {
    let mut b = b"onyx-early-exclusions-v1\n".to_vec();
    for x in in_force.values() {
        b.extend_from_slice(
            format!("{},{},{}\n", x.address, x.reason.as_str(), x.source).as_bytes(),
        );
    }
    keccak256(&b)
}

#[cfg(test)]
#[path = "early_tests.rs"]
pub(crate) mod tests;
