//! Replay the recorded spot bitcoin ETF filings (ROADMAP P2c, level 2) and
//! print (or write) the website summary.
//!
//! `cargo run -p onyx-source-edgar --example etf-holdings -- [--out PATH]`
//!
//! Every fund's recorded `companyconcept` response (fixtures/etf/, recorded by
//! `python3 -I scripts/record-sec-etf-holdings.py`) is checked against its
//! SHA-256, then each observer's fetch goes through the pinned fetcher on the
//! canonical feed of every period the 110..=730-day rule allows. Holdings are
//! satoshis converted exactly from the filed decimal; flows are the change
//! between two filed period ends. Offline: this sends nothing.

use onyx_source_edgar::etf::{recorded, sats_to_btc, series, site_summary};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .iter()
        .position(|a| a == "--out")
        .map(|i| args.get(i + 1).cloned().ok_or("--out needs a path"))
        .transpose()?;
    for rec in recorded()? {
        let (points, refused) = series(&rec)?;
        if let Some(p) = points.last() {
            eprintln!(
                "{:<5} CIK {}  {}  {:>18} BTC  {} {} filed {}  ({} periods, {} refused)",
                rec.fund.ticker,
                rec.fund.cik,
                p.period_end,
                sats_to_btc(p.reading.sats),
                p.reading.first.form,
                p.reading.first.accn,
                p.reading.first.filed,
                points.len(),
                refused.len()
            );
        }
    }
    let text = serde_json::to_string_pretty(&site_summary()?).map_err(|e| e.to_string())? + "\n";
    match out {
        Some(p) => std::fs::write(&p, text).map_err(|e| format!("{p}: {e}"))?,
        None => print!("{text}"),
    }
    Ok(())
}
