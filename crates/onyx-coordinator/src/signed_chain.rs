//! The level-3 signed book on chain (`contracts/src/OnyxSignedBook.sol`,
//! ADR-0028 amendment 6, THREATS L3-F7, L3R-F8), driven through Foundry's
//! `cast` like [`crate::cast_chain::CastChain`]: posts go from an unlocked
//! account (local anvil rehearsals; no key is read or passed), reads take a
//! block tag so a consumer reads a finalized block.
//!
//! Any coordinator posts every validly signed report it receives
//! ([`SignedBookCast::post_report`]); any coordinator or client checks a
//! claim's report against the chain ([`SignedBookCast::check_settleable`])
//! instead of trusting one coordinator's [`crate::signed::SignedSources`].

use onyx_core::{keccak256, Hash32, SignedReport};
use std::path::PathBuf;
use std::process::Command;

/// An `OnyxSignedBook` reached through `cast`.
#[derive(Clone, Debug)]
pub struct SignedBookCast {
    /// The `cast` binary.
    pub cast: PathBuf,
    /// JSON-RPC URL.
    pub rpc: String,
    /// `cast` runs here, outside the repo: Foundry loads a `.env` from its
    /// working directory (M8-L55-F4).
    pub work: PathBuf,
    /// The book's address.
    pub book: [u8; 20],
}

/// What `postReport` did with a report it took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostOutcome {
    /// The fact's accepted value.
    Accepted,
    /// A contradiction: the fact is disputed, every key involved suspended.
    Equivocation,
    /// Evidence joining a dispute it agrees with.
    FactDisputed,
}

fn ax(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// A `bytes32` from `cast` output (`0x` and 64 hex digits).
pub fn parse_h32(s: &str) -> Result<Hash32, String> {
    let mut a = [0u8; 32];
    hex::decode_to_slice(s.trim().trim_matches('"').trim_start_matches("0x"), &mut a)
        .map_err(|e| format!("not a 32-byte hash: {s} ({e})"))?;
    Ok(Hash32(a))
}

/// `cast call`'s output for `settleable(bytes32)(bool,bytes32,bytes32)`:
/// `Some((report hash, value hash))` when the fact may be settled on.
pub fn parse_settleable(out: &str) -> Result<Option<(Hash32, Hash32)>, String> {
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    match lines.as_slice() {
        ["true", r, v] => Ok(Some((parse_h32(r)?, parse_h32(v)?))),
        ["false", _, _] => Ok(None),
        _ => Err(format!("unexpected settleable output: {out}")),
    }
}

/// `keccak256(borsh(value))`: the value hash the book records.
pub fn value_hash(report: &SignedReport) -> Hash32 {
    keccak256(&borsh::to_vec(&report.payload.value).unwrap_or_default())
}

impl SignedBookCast {
    fn run(&self, args: &[String]) -> Result<String, String> {
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

    /// Send `sig(args)` to the book from an unlocked account; the receipt's
    /// logs on success, an error naming the revert otherwise.
    pub fn send(
        &self,
        from: &[u8; 20],
        sig: &str,
        args: &[String],
    ) -> Result<Vec<serde_json::Value>, String> {
        let v = self.send_receipt(from, sig, args)?;
        Ok(v["logs"].as_array().cloned().unwrap_or_default())
    }

    /// [`Self::send`], returning the whole receipt.
    pub fn send_receipt(
        &self,
        from: &[u8; 20],
        sig: &str,
        args: &[String],
    ) -> Result<serde_json::Value, String> {
        let mut a = vec![
            "send".to_string(),
            "--unlocked".into(),
            "--from".into(),
            ax(from),
            ax(&self.book),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        a.push("--json".into());
        let out = self.run(&a)?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        if v["status"].as_str() != Some("0x1") {
            return Err(format!("{sig} reverted"));
        }
        Ok(v)
    }

    /// A read at `block` (`latest`, `finalized` or a number).
    pub fn call(&self, block: &str, sig: &str, args: &[String]) -> Result<String, String> {
        let mut a = vec![
            "call".to_string(),
            "--block".into(),
            block.into(),
            ax(&self.book),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        self.run(&a)
    }

    /// Relay a signed report (anyone may). The outcome comes from the
    /// `ReportRecorded` event; refusals (replay, restatement, stale, wrong
    /// key...) are errors and change nothing.
    pub fn post_report(
        &self,
        from: &[u8; 20],
        report: &SignedReport,
    ) -> Result<PostOutcome, String> {
        self.post_report_landed(from, report).map(|(o, _)| o)
    }

    /// [`Self::post_report`], with the block the post landed in.
    pub fn post_report_landed(
        &self,
        from: &[u8; 20],
        report: &SignedReport,
    ) -> Result<(PostOutcome, u64), String> {
        let payload = borsh::to_vec(&report.payload).map_err(|e| e.to_string())?;
        let receipt = self.send_receipt(
            from,
            "postReport(bytes,bytes)",
            &[hx(&payload), hx(&report.signature)],
        )?;
        let block = receipt["blockNumber"]
            .as_str()
            .and_then(|h| u64::from_str_radix(h.trim_start_matches("0x"), 16).ok())
            .ok_or("the receipt names no block")?;
        let logs = receipt["logs"].as_array().cloned().unwrap_or_default();
        // ReportRecorded(bytes32 indexed digest, bytes32 indexed key,
        // bytes32 indexed fact, uint64 sequence, bytes32 reportHash, uint8 outcome)
        let topic =
            hx(&keccak256(b"ReportRecorded(bytes32,bytes32,bytes32,uint64,bytes32,uint8)").0);
        let log = logs
            .iter()
            .find(|l| l["topics"][0].as_str() == Some(topic.as_str()))
            .ok_or("no ReportRecorded event")?;
        let data = log["data"].as_str().ok_or("no event data")?;
        let bytes = hex::decode(data.trim_start_matches("0x")).map_err(|e| e.to_string())?;
        let outcome = match bytes.get(95) {
            Some(1) => PostOutcome::Accepted,
            Some(2) => PostOutcome::Equivocation,
            Some(3) => PostOutcome::FactDisputed,
            other => return Err(format!("unexpected outcome {other:?}")),
        };
        Ok((outcome, block))
    }

    /// The number of `block` (`latest`, `finalized`, ...).
    pub fn block_number(&self, block: &str) -> Result<u64, String> {
        let out = self.run(&[
            "block".into(),
            block.into(),
            "--field".into(),
            "number".into(),
        ])?;
        out.split_whitespace()
            .next()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| format!("not a block number: {out}"))
    }

    /// Whether the book recorded the payload with digest `digest` at `block`
    /// (`report(digest).key` is non-zero).
    pub fn recorded(&self, digest: &Hash32, block: &str) -> Result<bool, String> {
        let out = self.call(
            block,
            "report(bytes32)((bytes32,bytes32,bytes32,bytes32,uint64,uint64,uint64))",
            &[hx(&digest.0)],
        )?;
        let key = out
            .trim()
            .trim_start_matches('(')
            .split([',', ')'])
            .next()
            .unwrap_or("");
        Ok(parse_h32(key)? != Hash32::ZERO)
    }

    /// The timestamp of `block`: the clock every time rule of the book uses
    /// (L3R-F8), and the `now` a coordinator gives its own store.
    pub fn block_time(&self, block: &str) -> Result<u64, String> {
        let out = self.run(&[
            "block".into(),
            block.into(),
            "--field".into(),
            "timestamp".into(),
        ])?;
        out.split_whitespace()
            .next()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| format!("not a timestamp: {out}"))
    }

    /// `settleable(fact)` at `block`.
    pub fn settleable(
        &self,
        fact: &Hash32,
        block: &str,
    ) -> Result<Option<(Hash32, Hash32)>, String> {
        parse_settleable(&self.call(
            block,
            "settleable(bytes32)(bool,bytes32,bytes32)",
            &[hx(&fact.0)],
        )?)
    }

    /// The client check: `report` is what the chain book lets a consumer
    /// settle `report`'s fact on at `block` (read it at a finalized block):
    /// the fact is accepted, has stood for the dispute window of block time,
    /// and its accepted report and value are this report's. A fact disputed
    /// or accepted with another report on chain fails, whatever any one
    /// coordinator's store says (L3-F7). The match is on the report hash,
    /// which a claim names (R-SIG-7) and which covers the signature: the
    /// same payload under another signature does not match (review C2-R1,
    /// finding 6).
    pub fn check_settleable(&self, report: &SignedReport, block: &str) -> Result<(), String> {
        let fact = crate::signed::signed_fact_key(&report.payload);
        match self.settleable(&fact, block)? {
            None => Err("the chain book has no settleable value for this fact".into()),
            Some((r, v)) if r == report.hash() && v == value_hash(report) => Ok(()),
            Some(_) => Err("the chain book settles this fact on another report".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settleable_output_is_parsed_strictly() {
        let h = format!("0x{}", "11".repeat(32));
        let v = format!("0x{}", "22".repeat(32));
        assert_eq!(
            parse_settleable(&format!("true\n{h}\n{v}\n")),
            Ok(Some((Hash32([0x11; 32]), Hash32([0x22; 32]))))
        );
        assert_eq!(parse_settleable(&format!("false\n{h}\n{v}")), Ok(None));
        assert!(parse_settleable("true").is_err());
        assert!(parse_settleable(&format!("yes\n{h}\n{v}")).is_err());
        assert!(parse_settleable(&format!("true\n0x12\n{v}")).is_err());
    }
}
