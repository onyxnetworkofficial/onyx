//! Run the recorded NCEI round and write the website summary (NS3), through
//! the shared rehearsal ([`onyx_level2::rehearse::main_for`]; flags as the
//! Treasury's `level2-round` example). Without `--rpc` it replays the recorded
//! draw; `scripts/level2-rehearsal.sh` runs it on a local anvil.

use onyx_level2::rehearse::{main_for, Recorded};
use onyx_level2::Page;
use onyx_source_ncei::round::{recorded_draw, Ghcnd};
use onyx_source_ncei::{recorded, recorded_window_start, RECORDED_BODY};

fn main() -> Result<(), String> {
    let rec = recorded();
    let args: Vec<String> = std::env::args().collect();
    main_for(
        &Ghcnd,
        Recorded {
            day: rec.day.clone(),
            window_start: recorded_window_start(),
            pages: rec
                .fetches
                .iter()
                .map(|f| Page {
                    fetched_unix: f.fetched_unix,
                    body: Some(f.body.clone()),
                })
                .collect(),
            http_dates: rec.fetches.iter().map(|f| f.http_date.clone()).collect(),
            body: RECORDED_BODY.to_vec(),
            draw: Some(recorded_draw()),
        },
        &args,
    )
}
