//! Local maintenance review (ADR-0039). Stop the node before opening its journal.
//! `cargo run -p onyx-api --example submit-review -- JOURNAL list [AFTER_ID]`
//! `... JOURNAL show ID`
//! `... JOURNAL decide ID EXPECTED_REVISION EXPECTED_HASH approved|rejected REASON`
//! Output is private operator data, never a web asset or a public API response.
use onyx_api::submit::{SubmitLimits, SubmitStore};
use serde_json::json;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--help" || s == "-h") {
        println!("submit-review JOURNAL list [AFTER_ID]\nsubmit-review JOURNAL show ID\nsubmit-review JOURNAL decide ID EXPECTED_REVISION EXPECTED_HASH approved|rejected REASON\nStop the node first. Output is private. Decisions do not verify or publish data.");
        return Ok(());
    }
    if args.len() < 2 {
        return Err("see submit-review --help".into());
    }
    if !Path::new(&args[0]).is_file() {
        return Err("the existing submission journal is required".into());
    }
    let store = SubmitStore::open(&args[0], None, SubmitLimits::default())?;
    let output = match args[1].as_str() {
        "list" if matches!(args.len(), 2 | 3) => {
            let entries = store.review_list(args.get(2).map(String::as_str), 100)?;
            let next = entries.last().map(|e| e.id.clone());
            json!({"entries":entries,"after":next})
        }
        "show" if args.len() == 3 => serde_json::to_value(store.review_item(&args[2])?)?,
        "decide" if args.len() == 7 => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let item = store.review(
                &args[2],
                args[3].parse()?,
                &args[4],
                &args[5],
                &args[6],
                now,
            )?;
            json!({"id":item.id,"status":item.status,"revision":item.revision})
        }
        _ => return Err("invalid arguments; see submit-review --help".into()),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
