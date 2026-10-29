//! What an observer does with a fetched web page (untrusted bytes): the
//! JSON-pointer, Treasury FiscalData and SEC EDGAR fetchers' `extract`, then
//! `onyx_core::observe`. The first byte picks the fetcher and spec kind.
//! Never panics; a value answer is always one the spec's kind allows.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_core::{observe, Fetcher, ObservationKind, ObservationSpec};

fn run<F: Fetcher>(f: &F, mut spec: ObservationSpec, page: &[u8]) {
    let _ = f.extract(&spec, page);
    spec.fetcher = f.id();
    let t = spec.window_start;
    if let Ok(o) = observe(f, &spec, Some(page), t) {
        let _ = o;
    }
    let _ = observe(f, &spec, None, t);
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, page)) = data.split_first() else {
        return;
    };
    let fd_date = onyx_source_fiscaldata::recorded().record_date;
    let Ok(fd) =
        onyx_source_fiscaldata::spec(&fd_date, onyx_source_fiscaldata::RECORDED_WINDOW_START)
    else {
        return;
    };
    match sel % 5 {
        0 => run(&onyx_source_fiscaldata::FiscalDataFetcher, fd, page),
        1 => run(&onyx_source_page::JsonPointerFetcher, fd, page),
        2 => {
            let mut s = fd;
            s.kind = ObservationKind::Exact { max_len: 64 };
            s.selector = "/data/0/record_date".into();
            run(&onyx_source_page::JsonPointerFetcher, s, page);
        }
        3 => {
            let mut s = fd;
            s.kind = ObservationKind::Numeric {
                scale: 38,
                tolerance: u128::MAX,
            };
            run(&onyx_source_page::JsonPointerFetcher, s, page);
        }
        _ => {
            for c in onyx_source_edgar::COMPANIES {
                if let Ok(s) = onyx_source_edgar::spec(&c, "2026-03-31", 1_791_700_000) {
                    run(&onyx_source_edgar::EdgarFetcher, s, page);
                }
            }
        }
    }
});
