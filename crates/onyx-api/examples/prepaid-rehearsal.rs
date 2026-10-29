//! Prepaid balances on a local anvil, end to end (ROADMAP S7 part 2, scripts/prepaid-anvil.sh):
//! the server takes three vouchers against the buyer's on-chain account, then one reporter pass
//! redeems the latest voucher and pays the two books the usage journal names; a second pass does
//! nothing. Prints one JSON report per pass; the script checks the chain.
//!
//! ```sh
//! cargo run -p onyx-api --features http-client --example prepaid-rehearsal -- \
//!   RPC PREPAID_CONTRACT JOURNAL FACT_A FACT_B
//! ```
//! Keys come from the environment (anvil's public test keys in the script; they hold nothing):
//! ONYX_PREPAID_BUYER_KEY, ONYX_PREPAID_REPORTER_KEY.

use onyx_api::evm_tx::Sender;
use onyx_api::prepaid::{domain_separator, Voucher};
use onyx_api::prepaid_chain::{ChainAccounts, PrepaidReporter};
use onyx_api::prepaid_gate::PrepaidState;
use onyx_api::roots::HttpJsonRpc;
use onyx_api::x402::HyperPost;
use onyx_core::Hash32;
use std::sync::Arc;
use std::time::Duration;

/// `OnyxRevenue.OBSERVATION_ROUND`: books of registered observation facts.
const OBSERVATION_ROUND: u64 = u64::MAX;
const PRICE: u128 = 1000;

fn hex32(s: &str) -> Result<[u8; 32], String> {
    hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| format!("{s}: not 32 bytes"))
}

fn key(var: &str) -> Result<[u8; 32], String> {
    hex32(&std::env::var(var).map_err(|_| format!("{var} is not set"))?)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [rpc, contract, journal, fact_a, fact_b] = args.as_slice() else {
        return Err("usage: prepaid-rehearsal RPC PREPAID_CONTRACT JOURNAL FACT_A FACT_B".into());
    };
    if !(rpc.starts_with("http://127.0.0.1:") || rpc.starts_with("http://localhost:")) {
        return Err("the rehearsal runs on a local anvil only".into());
    }
    let contract: [u8; 20] = hex::decode(contract.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| "contract: not 20 bytes")?;
    let (a, b) = (Hash32(hex32(fact_a)?), Hash32(hex32(fact_b)?));
    let buyer = key("ONYX_PREPAID_BUYER_KEY")?;
    let reporter = key("ONYX_PREPAID_REPORTER_KEY")?;
    let node = || HttpJsonRpc::new(rpc.clone(), HyperPost::for_rpc(Duration::from_secs(10)));
    let domain = domain_separator(31_337, &contract);
    let state = Arc::new(PrepaidState::open(
        journal,
        domain,
        Arc::new(ChainAccounts::new(node(), contract)),
        60,
    )?);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    // Three answers: two on book A, one on book B, each priced PRICE.
    for (i, book) in [a, a, b].into_iter().enumerate() {
        let total = PRICE * (i as u128 + 1);
        let v = Voucher::sign(&buyer, &domain, total).ok_or("sign")?;
        state.refresh(v.buyer, now).await?;
        let c = state
            .charge(
                v,
                PRICE,
                (OBSERVATION_ROUND, book),
                Hash32([i as u8 + 1; 32]),
                now,
            )
            .map_err(|e| format!("answer {}: {e}", i + 1))?;
        println!(
            "{}",
            serde_json::json!({"answer": i + 1, "total": c.total.to_string(), "spendable": c.spendable.to_string()})
        );
    }
    let sender = Arc::new(Sender::new(node(), 31_337, reporter).ok_or("reporter key")?);
    let reader = Arc::new(ChainAccounts::latest(node(), contract));
    let r = PrepaidReporter::new(state, reader, sender, contract, Duration::from_secs(60));
    for pass in 1..=2 {
        let p = r.pass().await?;
        println!(
            "{}",
            serde_json::json!({
                "pass": pass, "redeemed": p.redeemed, "redeemTx": p.redeem_tx,
                "allocated": p.allocated, "allocatedAmount": p.allocated_amount.to_string(),
                "allocateTx": p.allocate_tx, "waiting": p.waiting,
            })
        );
    }
    Ok(())
}
