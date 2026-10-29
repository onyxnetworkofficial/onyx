//! The feed ledger beside the review journal (PRODUCT-PLAN.md U4, ADR-0039 amendment 1).
//!
//! An adapter's worker feeds an approved submission into its pinned observer round and records the
//! evidence here, one entry per receipt id: fed once (a retry returns the recorded evidence and runs
//! nothing), or failed with attempts and the last reason. The file is
//! [`ledger_path`]`(journal)` and, like the journal, is written only while the node is stopped.
//!
//! The node reads it once at startup. A receipt whose current status is `approved` and whose
//! content hash is the hash that was fed shows the round's public evidence
//! ([`FeedLedger::public_check`]): record date, round id, result hash (all of which the results
//! store serves anyway) and where the round was registered (`simulated` for a local rehearsal). Failure
//! reasons and attempts are operator data and never leave this file.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Most failed attempts one submission gets before the worker stops trying it (PRODUCT-PLAN.md U4:
/// "failures retain a reason and can retry within bounded budgets").
pub const MAX_FEED_ATTEMPTS: u32 = 3;

/// Largest ledger file the node or a worker reads, in bytes.
pub const MAX_LEDGER_BYTES: u64 = 8 * 1024 * 1024;

/// The feed ledger of the journal at `journal`: the same path with `.feed.json` appended.
pub fn ledger_path(journal: &Path) -> PathBuf {
    let mut s = journal.as_os_str().to_owned();
    s.push(".feed.json");
    PathBuf::from(s)
}

/// What one feed proved, as recorded: the evidence a client checks against the results store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FedEvidence {
    /// The submission receipt id.
    pub id: String,
    /// The submission content hash that was fed.
    pub hash: String,
    /// The record date the round observed.
    pub record_date: String,
    /// The round id (`0x` and 64 hex digits).
    pub round_id: String,
    /// The settled result hash (`0x` and 64 hex digits).
    pub result: String,
    /// The value the observers agreed on, as the feed's decimal text (`40305316210829.72`):
    /// what the source published, not a claim that it is correct (THREATS U4-6, PP11).
    pub value: String,
    /// Where the round was registered: `simulated` (a local rehearsal, nothing on chain) or
    /// `eip155:<chain id>`.
    pub registry: String,
}

/// One submission's feed history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEntry {
    /// Fed once: the evidence, kept so a retry returns it instead of running again.
    Fed {
        /// The submission content hash that was fed.
        hash: String,
        /// The record date.
        record_date: String,
        /// The round id.
        round_id: String,
        /// The settled result hash.
        result: String,
        /// The value the observers agreed on (decimal text).
        value: String,
        /// `simulated` or `eip155:<chain id>`.
        registry: String,
    },
    /// Failed so far: attempts used and the last reason.
    Failed {
        /// Attempts used.
        attempts: u32,
        /// The last reason.
        reason: String,
    },
}

/// What the worker should do with a submission, by its ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedStep {
    /// Not fed yet and attempts left: run the round.
    Run,
    /// Already fed: return this evidence, run nothing.
    AlreadyFed(FeedEntry),
    /// Out of attempts: stop, with the last reason.
    GiveUp(String),
}

/// The worker's ledger: one entry per submission id, saved as JSON beside the review journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedLedger {
    entries: BTreeMap<String, FeedEntry>,
}

impl FeedLedger {
    /// What to do with `id` whose content hash is `hash`. A fed entry with another hash means the
    /// approved content changed since it was fed: refused, never fed twice.
    pub fn step(&self, id: &str, hash: &str) -> Result<FeedStep, String> {
        Ok(match self.entries.get(id) {
            None => FeedStep::Run,
            Some(e @ FeedEntry::Fed { hash: h, .. }) if h == hash => {
                FeedStep::AlreadyFed(e.clone())
            }
            Some(FeedEntry::Fed { .. }) => return Err(format!("{id} was fed with other content")),
            Some(FeedEntry::Failed { attempts, reason }) if *attempts >= MAX_FEED_ATTEMPTS => {
                FeedStep::GiveUp(reason.clone())
            }
            Some(FeedEntry::Failed { .. }) => FeedStep::Run,
        })
    }

    /// Record a successful feed.
    pub fn fed(&mut self, e: &FedEvidence) {
        self.entries.insert(
            e.id.clone(),
            FeedEntry::Fed {
                hash: e.hash.clone(),
                record_date: e.record_date.clone(),
                round_id: e.round_id.clone(),
                result: e.result.clone(),
                value: e.value.clone(),
                registry: e.registry.clone(),
            },
        );
    }

    /// Record a failed attempt; returns the attempts used.
    pub fn failed(&mut self, id: &str, reason: &str) -> u32 {
        let attempts = match self.entries.get(id) {
            Some(FeedEntry::Failed { attempts, .. }) => attempts + 1,
            _ => 1,
        };
        self.entries.insert(
            id.to_string(),
            FeedEntry::Failed {
                attempts,
                reason: reason.to_string(),
            },
        );
        attempts
    }

    /// The public evidence for receipt `id` whose current status is `status` and content hash is
    /// `hash`, or `None`. Only an approved receipt fed with exactly this content has any: a
    /// withdrawn approval, a failed or missing feed show nothing beyond the status.
    pub fn public_check(&self, id: &str, status: &str, hash: &str) -> Option<Value> {
        match self.entries.get(id) {
            Some(FeedEntry::Fed {
                hash: h,
                record_date,
                round_id,
                result,
                value,
                registry,
            }) if status == "approved" && h == hash => Some(json!({
                "level": 2,
                "recordDate": record_date,
                "roundId": round_id,
                "result": result,
                "value": value,
                "registry": registry,
            })),
            _ => None,
        }
    }

    /// The ledger as JSON (sorted by id).
    pub fn to_json(&self) -> Value {
        let m: serde_json::Map<String, Value> = self
            .entries
            .iter()
            .map(|(id, e)| {
                let v = match e {
                    FeedEntry::Fed { hash, record_date, round_id, result, value, registry } => json!({"state": "fed", "hash": hash, "recordDate": record_date, "roundId": round_id, "result": result, "value": value, "registry": registry}),
                    FeedEntry::Failed { attempts, reason } => json!({"state": "failed", "attempts": attempts, "reason": reason}),
                };
                (id.clone(), v)
            })
            .collect();
        Value::Object(m)
    }

    /// Read a ledger written by [`Self::to_json`]; anything else is refused. Fed evidence is
    /// served publicly, so every fed field must have its exact shape.
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let obj = v.as_object().ok_or("the feed ledger is not an object")?;
        let s = |e: &Value, k: &str| {
            e.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or(format!("feed ledger: `{k}` missing"))
        };
        let shaped = |e: &Value, k: &str, ok: fn(&str) -> bool| {
            s(e, k).and_then(|x| {
                if ok(&x) {
                    Ok(x)
                } else {
                    Err(format!("feed ledger: `{k}` is malformed"))
                }
            })
        };
        let mut entries = BTreeMap::new();
        for (id, e) in obj {
            if !super::is_id(id) {
                return Err(format!("feed ledger: `{id}` is not a receipt id"));
            }
            let entry = match e.get("state").and_then(Value::as_str) {
                Some("fed") => FeedEntry::Fed {
                    hash: shaped(e, "hash", is_hash)?,
                    record_date: shaped(e, "recordDate", is_date)?,
                    round_id: shaped(e, "roundId", is_hash)?,
                    result: shaped(e, "result", is_hash)?,
                    value: shaped(e, "value", is_value)?,
                    registry: shaped(e, "registry", is_registry)?,
                },
                Some("failed") => FeedEntry::Failed {
                    attempts: e
                        .get("attempts")
                        .and_then(Value::as_u64)
                        .and_then(|a| u32::try_from(a).ok())
                        .ok_or("feed ledger: `attempts` missing")?,
                    reason: s(e, "reason")?,
                },
                _ => return Err(format!("feed ledger: {id} has no known state")),
            };
            entries.insert(id.clone(), entry);
        }
        Ok(Self { entries })
    }

    /// Read the ledger at `path`; a missing file is an empty ledger. A file that is too large or
    /// not a ledger is an error (the node refuses to start, as for a damaged journal).
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let bad = |m: String| std::io::Error::new(std::io::ErrorKind::InvalidData, m);
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if meta.len() > MAX_LEDGER_BYTES {
            return Err(bad(format!(
                "feed ledger is larger than {MAX_LEDGER_BYTES} bytes"
            )));
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)
            .map_err(|e| bad(format!("feed ledger: {e}")))?;
        Self::from_json(&v).map_err(bad)
    }

    /// Write the ledger to `path` atomically (a temporary file, then a rename), owner-only on Unix.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let text = serde_json::to_string_pretty(&self.to_json())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        // Owner-only from the first byte, not after a chmod.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        {
            use std::io::Write;
            let mut f = opts.open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // A stale `.tmp` from an earlier run keeps its mode on open: force it.
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    }
}

fn is_hash(s: &str) -> bool {
    s.strip_prefix("0x")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// A decimal as the feed writes it: an optional minus, digits without a leading zero (or one
/// zero), an optional fraction; at most 80 characters.
pub fn is_value(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = body
        .split_once('.')
        .map_or((body, None), |(i, f)| (i, Some(f)));
    s.len() <= 80
        && !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && (int == "0" || !int.starts_with('0'))
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

fn is_registry(s: &str) -> bool {
    s == "simulated"
        || s.strip_prefix("eip155:").is_some_and(|c| {
            !c.is_empty()
                && c.len() <= 20
                && !c.starts_with('0')
                && c.bytes().all(|b| b.is_ascii_digit())
        })
}

fn is_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}
