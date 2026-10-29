//! The API's append-only journals (receipts, settle intents, pending
//! self-settles, credits in flight): operator-local, checksummed per line,
//! but a damaged or edited journal must be refused with an error at open,
//! never panic. The first byte picks the journal and the framing: the
//! target writes each input line with its checksum (and the right header)
//! so the fuzzer reaches the entry parsers, or writes the bytes raw.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::evm_tx::Sender;
use onyx_api::journal::Journal;
use onyx_api::receipts::ReceiptLog;
use onyx_fuzz_common::{frame, journal_header, secret, MethodRpc, BIND, CHAIN};
use std::sync::Arc;

const KINDS: [&str; 5] = [
    onyx_api::receipts::JOURNAL_KIND,
    onyx_api::intents::JOURNAL_KIND,
    onyx_api::self_settle::PENDING_JOURNAL_KIND,
    onyx_api::reporter::CREDITS_JOURNAL_KIND,
    "test",
];

fn sender() -> Option<Sender<MethodRpc>> {
    Sender::new(
        MethodRpc::new(&serde_json::Value::Null),
        CHAIN,
        secret("operator"),
    )
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let kind = usize::from(sel % 5);
    let mut file = Vec::new();
    if sel & 0x80 == 0 {
        if sel & 0x40 == 0 {
            file.extend(frame(journal_header(KINDS[kind]).to_string().as_bytes()));
        }
        for line in rest.split(|b| *b == b'\n') {
            file.extend(frame(line));
        }
    } else {
        file.extend_from_slice(rest);
    }
    let dir = std::env::temp_dir().join(format!("onyx-fuzz-journal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let name = if kind == 2 {
        "pending-settles.jsonl"
    } else {
        "journal.jsonl"
    };
    let path = dir.join(name);
    if std::fs::write(&path, &file).is_err() {
        return;
    }
    match kind {
        0 => {
            if let Ok(log) = ReceiptLog::open(&path, &BIND) {
                let _ = log.pending();
            }
        }
        1 => {
            if let Ok(log) = onyx_api::intents::IntentLog::open(&path, &BIND) {
                let _ = log.open_count();
                let _ = log.recovered();
            }
        }
        2 => {
            if let Some(s) = sender() {
                let ss = onyx_api::self_settle::SelfSettle::new(
                    s,
                    BIND.contract,
                    [0x33; 20],
                    onyx_api::x402::BASE_SEPOLIA,
                    std::time::Duration::from_secs(5),
                );
                let _ = ss.persist(&dir);
            }
        }
        3 => {
            if let Some(s) = sender() {
                let r = onyx_api::reporter::Reporter::new(
                    Arc::new(s),
                    BIND.contract,
                    Arc::new(ReceiptLog::new()),
                );
                let _ = r.with_state_file(&path);
            }
        }
        _ => {
            if let Ok((mut j, _live)) = Journal::open(&path, KINDS[4], &BIND) {
                let _ = j.apply(&[], &[serde_json::json!({"x": 1})]);
            }
        }
    }
});
