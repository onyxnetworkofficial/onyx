//! The deployment manifest: which contracts the service trusts.
//!
//! Written by the deploy drivers (`scripts/demo-testnet.sh`, `demo-local
//! --export-node`) right after the deploy, with the runtime code hash of each
//! contract. The service checks both hashes on its own node at start
//! (`ChainRoots::connect`, `PayToPin::check_code`), so a manifest naming the
//! wrong address, or a node serving other code, stops it.
//!
//! ```json
//! { "chainId": 84532,
//!   "settlement": "0x…", "settlementCodeHash": "0x…",
//!   "revenue": "0x…", "revenueCodeHash": "0x…", "usdc": "0x…" }
//! ```
//!
//! `revenue`, `revenueCodeHash` and `usdc` are needed only with payments on.
//! `registry` and `registryCodeHash` (`OnyxRegistry`, ADR-0030) come together
//! and are needed when a level-2 store names a registry (AUD-2-F8); with them
//! every shard is floored at its attested level, after the start checks the
//! code hash and that the registry's `ledger()` is `settlement` (L16). The
//! deploy drivers' `--export-node` writes them whenever the run deployed one.
//! `deployBlock` (the settlement contract's deploy block) is where the chain
//! indexer starts (ADR-0033); needed only with `ONYX_INDEXER=on`.
//! `signedBook`, `signedBookCodeHash` and `signedBookReviewer`
//! (`OnyxSignedBook`, ADR-0028 amendment 7, ROADMAP C10) come together: the
//! level-3 book the node serves signed facts against. The start checks the
//! code hash and that `reviewer()` is `signedBookReviewer`, which may never be
//! the settlement's coordinator. `scripts/deploy-signed-book.sh --manifest`
//! writes them after its own build check.

use crate::config::Env;
use onyx_core::Hash32;
use serde_json::Value;

/// Base Sepolia.
pub const BASE_SEPOLIA_CHAIN_ID: u64 = 84_532;
/// anvil's default chain id (dev only).
pub const ANVIL_CHAIN_ID: u64 = 31_337;

/// A parsed, checked manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// The settlement contract (canonical ledger, ADR-0022).
    pub settlement: [u8; 20],
    /// keccak-256 of its runtime code.
    pub settlement_code_hash: Hash32,
    /// `OnyxRevenue` (the x402 `payTo`), when deployed.
    pub revenue: Option<[u8; 20]>,
    /// keccak-256 of its runtime code.
    pub revenue_code_hash: Option<Hash32>,
    /// The payment token.
    pub usdc: Option<[u8; 20]>,
    /// `OnyxRegistry` (ADR-0030), when deployed: the only registry a level-2
    /// store may name (AUD-2-F8).
    pub registry: Option<[u8; 20]>,
    /// keccak-256 of its runtime code.
    pub registry_code_hash: Option<Hash32>,
    /// The block the settlement contract was deployed in (ADR-0033).
    pub deploy_block: Option<u64>,
    /// `OnyxSignedBook` (ROADMAP C10): address, runtime code hash and
    /// reviewer, when deployed.
    pub signed_book: Option<([u8; 20], Hash32, [u8; 20])>,
    /// The raw manifest (for `PayToPin::from_manifest`).
    pub raw: Value,
}

fn hex_fixed<const N: usize>(v: &Value, field: &str) -> Result<Option<[u8; N]>, String> {
    match v.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let b = hex::decode(s.strip_prefix("0x").unwrap_or(s))
                .ok()
                .filter(|b| b.len() == N)
                .ok_or_else(|| format!("manifest {field} must be {N} bytes of hex"))?;
            let mut out = [0u8; N];
            out.copy_from_slice(&b);
            Ok(Some(out))
        }
        Some(_) => Err(format!("manifest {field} must be a hex string")),
    }
}

impl Deployment {
    /// Parse `v` and refuse a chain this environment may not use: Base
    /// Sepolia in testnet, anvil only in dev, anything else only with
    /// `allow_mainnet` (which this project never gives).
    pub fn from_json(v: &Value, env: Env, allow_mainnet: bool) -> Result<Self, String> {
        let chain_id = v["chainId"]
            .as_u64()
            .ok_or("manifest chainId must be a number")?;
        let ok = match (env, chain_id) {
            (_, BASE_SEPOLIA_CHAIN_ID) => true,
            (Env::Dev, ANVIL_CHAIN_ID) => true,
            _ => allow_mainnet,
        };
        if !ok {
            return Err(format!(
                "manifest chain {chain_id} is not allowed in this environment (testnet: {BASE_SEPOLIA_CHAIN_ID}; dev also {ANVIL_CHAIN_ID})"
            ));
        }
        let settlement =
            hex_fixed::<20>(v, "settlement")?.ok_or("manifest settlement is required")?;
        let settlement_code_hash = Hash32(
            hex_fixed::<32>(v, "settlementCodeHash")?
                .ok_or("manifest settlementCodeHash is required")?,
        );
        let revenue = hex_fixed::<20>(v, "revenue")?;
        let revenue_code_hash = hex_fixed::<32>(v, "revenueCodeHash")?.map(Hash32);
        let usdc = hex_fixed::<20>(v, "usdc")?;
        if revenue.is_some() != revenue_code_hash.is_some() || revenue.is_some() != usdc.is_some() {
            return Err("manifest revenue, revenueCodeHash and usdc come together".into());
        }
        let registry = hex_fixed::<20>(v, "registry")?;
        let registry_code_hash = hex_fixed::<32>(v, "registryCodeHash")?.map(Hash32);
        if registry.is_some() != registry_code_hash.is_some() {
            return Err("manifest registry and registryCodeHash come together".into());
        }
        let signed_book =
            match (
                hex_fixed::<20>(v, "signedBook")?,
                hex_fixed::<32>(v, "signedBookCodeHash")?,
                hex_fixed::<20>(v, "signedBookReviewer")?,
            ) {
                (None, None, None) => None,
                (Some(b), Some(h), Some(r)) => {
                    if r == [0u8; 20] {
                        return Err("manifest signedBookReviewer must not be zero".into());
                    }
                    Some((b, Hash32(h), r))
                }
                _ => return Err(
                    "manifest signedBook, signedBookCodeHash and signedBookReviewer come together"
                        .into(),
                ),
            };
        let deploy_block = match v.get("deployBlock") {
            None | Some(Value::Null) => None,
            Some(b) => Some(
                b.as_u64()
                    .ok_or("manifest deployBlock must be a block number")?,
            ),
        };
        Ok(Deployment {
            chain_id,
            settlement,
            settlement_code_hash,
            revenue,
            revenue_code_hash,
            usdc,
            registry,
            registry_code_hash,
            deploy_block,
            signed_book,
            raw: v.clone(),
        })
    }

    /// The CAIP-2 payment network of this chain.
    pub fn network(&self) -> String {
        format!("eip155:{}", self.chain_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn m(chain: u64) -> Value {
        json!({
            "chainId": chain,
            "settlement": format!("0x{}", "11".repeat(20)),
            "settlementCodeHash": format!("0x{}", "22".repeat(32)),
        })
    }

    #[test]
    fn chains_follow_the_environment() {
        assert!(Deployment::from_json(&m(84_532), Env::Testnet, false).is_ok());
        assert!(Deployment::from_json(&m(31_337), Env::Testnet, false).is_err());
        assert!(Deployment::from_json(&m(31_337), Env::Dev, false).is_ok());
        for mainnet in [1, 8_453, 10] {
            assert!(
                Deployment::from_json(&m(mainnet), Env::Testnet, false).is_err(),
                "{mainnet}"
            );
            assert!(
                Deployment::from_json(&m(mainnet), Env::Dev, false).is_err(),
                "{mainnet}"
            );
        }
    }

    #[test]
    fn code_hashes_are_required_and_revenue_fields_come_together() {
        let mut v = m(84_532);
        v.as_object_mut()
            .expect("object")
            .remove("settlementCodeHash");
        assert!(Deployment::from_json(&v, Env::Testnet, false).is_err());
        let mut v = m(84_532);
        v["revenue"] = json!(format!("0x{}", "33".repeat(20)));
        assert!(Deployment::from_json(&v, Env::Testnet, false).is_err());
        v["revenueCodeHash"] = json!(format!("0x{}", "44".repeat(32)));
        v["usdc"] = json!(format!("0x{}", "55".repeat(20)));
        let d = Deployment::from_json(&v, Env::Testnet, false).expect("manifest");
        assert_eq!(d.revenue, Some([0x33; 20]));
        assert_eq!(d.network(), "eip155:84532");
    }

    #[test]
    fn the_deploy_block_is_optional_and_a_number() {
        let mut v = m(84_532);
        assert_eq!(
            Deployment::from_json(&v, Env::Testnet, false)
                .expect("ok")
                .deploy_block,
            None
        );
        v["deployBlock"] = json!(48_037_567u64);
        assert_eq!(
            Deployment::from_json(&v, Env::Testnet, false)
                .expect("ok")
                .deploy_block,
            Some(48_037_567)
        );
        v["deployBlock"] = json!("0x1");
        assert!(Deployment::from_json(&v, Env::Testnet, false).is_err());
    }

    /// AUD-2-F8: the registry and its code hash come together.
    #[test]
    fn registry_fields_come_together() {
        let mut v = m(BASE_SEPOLIA_CHAIN_ID);
        v["registry"] = json!(format!("0x{}", "55".repeat(20)));
        assert!(Deployment::from_json(&v, Env::Testnet, false).is_err());
        v["registryCodeHash"] = json!(format!("0x{}", "66".repeat(32)));
        let d = Deployment::from_json(&v, Env::Testnet, false).expect("both");
        assert_eq!(d.registry, Some([0x55; 20]));
        assert_eq!(d.registry_code_hash, Some(Hash32([0x66; 32])));
    }

    #[test]
    fn the_signed_book_pin_comes_whole() {
        let mut v = m(BASE_SEPOLIA_CHAIN_ID);
        assert_eq!(
            Deployment::from_json(&v, Env::Testnet, false)
                .expect("parse")
                .signed_book,
            None
        );
        v["signedBook"] = json!(format!("0x{}", "aa".repeat(20)));
        let e = Deployment::from_json(&v, Env::Testnet, false).expect_err("partial");
        assert!(e.contains("come together"), "{e}");
        v["signedBookCodeHash"] = json!(format!("0x{}", "bb".repeat(32)));
        v["signedBookReviewer"] = json!(format!("0x{}", "00".repeat(20)));
        assert!(
            Deployment::from_json(&v, Env::Testnet, false).is_err(),
            "zero reviewer"
        );
        v["signedBookReviewer"] = json!(format!("0x{}", "cc".repeat(20)));
        let d = Deployment::from_json(&v, Env::Testnet, false).expect("parse");
        assert_eq!(
            d.signed_book,
            Some(([0xaa; 20], Hash32([0xbb; 32]), [0xcc; 20]))
        );
    }
}
