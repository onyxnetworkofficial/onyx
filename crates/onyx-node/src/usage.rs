//! Usage counts on the node (`ONYX_USAGE=on`; docs/spec/usage-counts.md,
//! THREATS.md USE-1..USE-7, review USE-R1): the counting layer over every
//! route, the once-a-minute write, and the operator's reader.
//!
//! The node has no route that gives the counts out (review USE-R1 M2: a
//! route that trusts "the request came from this host" is open to anything
//! on the host that forwards connections). The operator reads the day files
//! instead, with `onyx-node usage`, as a user that may read the state
//! directory. `tests/caddyfile.rs` and `tests/usage_route.rs` hold that no
//! `/v1/usage` route exists.

use axum::Router;
use onyx_api::usage::Usage;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The wait between two writes of the counts.
pub const FLUSH_EVERY: Duration = Duration::from_secs(60);

const HELP: &str = "usage: onyx-node usage [--days N] [--until YYYY-MM-DD] [--state-dir DIR]
Prints the usage counts of the last N UTC days (1-90, default 7) as JSON: requests per route
template and status class, and opens per dataset (docs/spec/usage-counts.md). Counts only,
nothing about visitors. It reads the day files in DIR/usage (DIR: --state-dir, else
ONYX_STATE_DIR) and nothing else: it asks the running node nothing and writes nothing, so the
node's last minute may be missing. Run it as a user that may read the state directory.";

/// The counters of a node with `ONYX_USAGE=on` (`None` when off: nothing is
/// counted or written). Their files live in `<state dir>/usage/`.
pub fn open(on: bool, state_dir: &Path, clock: onyx_api::Clock) -> Option<Arc<Usage>> {
    on.then(|| Arc::new(Usage::new(state_dir.join("usage"), clock)))
}

/// The whole `app` counted, when usage counts are on.
pub fn counted(usage: Option<&Arc<Usage>>, app: Router) -> Router {
    match usage {
        Some(u) => onyx_api::usage::counted(u.clone(), app),
        None => app,
    }
}

/// Write the counts now; a problem is one log line, never a stop.
pub async fn flush(usage: &Arc<Usage>) {
    let u = usage.clone();
    match tokio::task::spawn_blocking(move || u.flush()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("onyx-node: usage counts: {e}"),
        Err(e) => eprintln!("onyx-node: usage counts: the write did not finish: {e}"),
    }
}

/// Write the counts every `every`, for as long as the node runs.
pub async fn run(usage: Arc<Usage>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        flush(&usage).await;
    }
}

/// What `onyx-node usage` was asked for.
#[derive(Debug, PartialEq, Eq)]
pub struct Ask {
    /// The folder of the day files (`<state dir>/usage`).
    pub dir: PathBuf,
    /// The last day of the report, as a day number.
    pub until: u64,
    /// How many days, 1 to 90.
    pub days: u64,
}

/// Parse the arguments of `onyx-node usage`. `state_dir` is `ONYX_STATE_DIR`
/// and `today` the current UTC day number. `Ok(None)`: `--help`.
pub fn parse(
    args: &[String],
    state_dir: Option<String>,
    today: u64,
) -> Result<Option<Ask>, String> {
    let (mut days, mut until, mut dir) = (7u64, today, state_dir);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--days" => {
                let v = value()?;
                days = Some(v.as_str())
                    .filter(|n| n.len() <= 2 && n.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|n| n.parse::<u64>().ok())
                    .filter(|n| (1..=onyx_api::usage::RETENTION_DAYS).contains(n))
                    .ok_or_else(|| format!("--days must be 1-90, not {v:?}"))?;
            }
            "--until" => {
                let v = value()?;
                until = onyx_core::feed::day_number(&v)
                    .and_then(|d| u64::try_from(d).ok())
                    .ok_or_else(|| format!("--until must be a day YYYY-MM-DD, not {v:?}"))?;
            }
            "--state-dir" => dir = Some(value()?),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let dir = dir
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .ok_or("no state directory: give --state-dir DIR or set ONYX_STATE_DIR")?;
    Ok(Some(Ask {
        dir: PathBuf::from(dir).join("usage"),
        until,
        days,
    }))
}

/// `onyx-node usage ...`: exit 0 with the report on stdout, 2 on a usage
/// error, 1 when the folder of the counts cannot be read.
pub fn main(args: &[String]) -> ExitCode {
    let today = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    let ask = match parse(args, std::env::var("ONYX_STATE_DIR").ok(), today) {
        Ok(Some(ask)) => ask,
        Ok(None) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("onyx-node usage: {e}\n{HELP}");
            return ExitCode::from(2);
        }
    };
    // Say why when there is nothing to read, instead of an empty report.
    if let Err(e) = std::fs::read_dir(&ask.dir) {
        eprintln!(
            "onyx-node usage: {}: {e} (no usage counts here: the node counts only with ONYX_USAGE=on, and this user must be allowed to read its state directory)",
            ask.dir.display()
        );
        return ExitCode::FAILURE;
    }
    let report = onyx_api::usage::read_report(&ask.dir, ask.until, ask.days);
    match serde_json::to_string_pretty(&report) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("onyx-node usage: {e}");
            ExitCode::FAILURE
        }
    }
}
