//! Observe on the first paid query in the node (ADR-0049, NORTH-STAR NS10): the
//! `ONYX_OBSERVE` file and the round command.
//!
//! The file names the operator's round command and one offer per level-2
//! metric:
//!
//! ```json
//! {
//!   "command": ["/usr/local/bin/onyx-observe-round", "--state", "/var/lib/onyx/observe"],
//!   "offers": {
//!     "usgs.earthquakes.m5_count": {
//!       "price": "2500",
//!       "deadlineSecs": 3600,
//!       "store": "/var/lib/onyx/observations/usgs.json"
//!     }
//!   }
//! }
//! ```
//!
//! For each paid fact the worker runs `command... <metric> <subject>`. The
//! command runs one level-2 round (the `onyx_level2::live` coordinator and
//! observers) and prints the path of a results store holding it; the node
//! loads that store with every check of a start (the pinned template, each
//! round's transcript) and hands the subject's round to the worker, which
//! checks it again before it serves it. A command that fails, prints no store,
//! or outlives the hold's deadline is killed and the hold released.
//!
//! An offer's `store` must be one of `ONYX_OBSERVATIONS` (the served round is
//! appended to it, so it is served again after a restart) and must be
//! writable by the node.

use onyx_api::observations::{subject_text, ObservationStore, TemplatePins, Transcript};
use onyx_api::observe::{ObserveOffer, RoundRunner};
use onyx_api::x402::BoxFuture;
use onyx_core::{FeedTemplate, Subject};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Largest `ONYX_OBSERVE` file and results store the node reads here (bytes).
const MAX_FILE: u64 = 16 * 1024 * 1024;

/// The parsed `ONYX_OBSERVE` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserveFile {
    /// The round command and its fixed arguments.
    pub command: Vec<String>,
    /// Offers by metric.
    pub offers: BTreeMap<String, ObserveOffer>,
}

fn read_capped(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FILE {
        return Err(format!("{}: larger than {MAX_FILE} bytes", path.display()));
    }
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse the `ONYX_OBSERVE` file. `stores` are the `ONYX_OBSERVATIONS` paths:
/// each offer's store must be one of them.
pub fn parse(text: &str, stores: &[PathBuf]) -> Result<ObserveFile, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("ONYX_OBSERVE: {e}"))?;
    let o = v.as_object().ok_or("ONYX_OBSERVE: not an object")?;
    if o.keys().any(|k| k != "command" && k != "offers") {
        return Err("ONYX_OBSERVE: only command and offers".into());
    }
    let command: Vec<String> = o
        .get("command")
        .and_then(Value::as_array)
        .ok_or("ONYX_OBSERVE: command must be a list of strings")?
        .iter()
        .map(|a| a.as_str().map(str::to_owned))
        .collect::<Option<_>>()
        .ok_or("ONYX_OBSERVE: command must be a list of strings")?;
    if !command.first().is_some_and(|c| c.starts_with('/')) {
        return Err("ONYX_OBSERVE: command[0] must be an absolute path".into());
    }
    let mut offers = BTreeMap::new();
    for (metric, e) in o
        .get("offers")
        .and_then(Value::as_object)
        .ok_or("ONYX_OBSERVE: offers must be an object")?
    {
        let e = e
            .as_object()
            .filter(|e| {
                e.keys()
                    .all(|k| matches!(k.as_str(), "price" | "deadlineSecs" | "store"))
            })
            .ok_or_else(|| format!("ONYX_OBSERVE {metric}: only price, deadlineSecs and store"))?;
        let price = e
            .get("price")
            .and_then(Value::as_str)
            .and_then(|p| p.parse::<u64>().ok())
            .ok_or_else(|| format!("ONYX_OBSERVE {metric}: price must be a decimal string"))?;
        let deadline_secs = e
            .get("deadlineSecs")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("ONYX_OBSERVE {metric}: deadlineSecs must be a number"))?;
        let store = e
            .get("store")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| format!("ONYX_OBSERVE {metric}: store must be a path"))?;
        if !stores.contains(&store) {
            return Err(format!(
                "ONYX_OBSERVE {metric}: store {} is not one of ONYX_OBSERVATIONS (a served round must be served again after a restart)",
                store.display()
            ));
        }
        offers.insert(
            metric.clone(),
            ObserveOffer {
                price,
                deadline_secs,
                store,
            },
        );
    }
    if offers.is_empty() {
        return Err("ONYX_OBSERVE: no offers".into());
    }
    Ok(ObserveFile { command, offers })
}

/// Read and parse the `ONYX_OBSERVE` file at `path`.
pub fn load(path: &Path, stores: &[PathBuf]) -> Result<ObserveFile, String> {
    parse(&read_capped(path)?, stores)
}

/// Runs the operator's round command for one fact (see the module docs).
pub struct CommandRunner {
    command: Vec<String>,
    pins: TemplatePins,
    clock: fn() -> u64,
}

impl CommandRunner {
    /// A runner of `command`, loading its stores under `pins`. Each command is killed at its
    /// hold's deadline.
    pub fn new(command: Vec<String>, pins: TemplatePins, clock: fn() -> u64) -> Self {
        CommandRunner {
            command,
            pins,
            clock,
        }
    }
}

/// Run `argv`, wait at most `limit`, and return its stdout. Killed past the limit.
fn run_command(argv: &[String], limit: Duration) -> Result<String, String> {
    use std::io::Read;
    let (cmd, args) = argv.split_first().ok_or("no command")?;
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("the round command did not start: {e}"))?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the round command ran past the hold's deadline".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(format!("the round command: {e}")),
        }
    };
    let mut out = String::new();
    if let Some(s) = child.stdout.take() {
        s.take(4096)
            .read_to_string(&mut out)
            .map_err(|e| format!("the round command's output: {e}"))?;
    }
    if !status.success() {
        return Err(format!("the round command failed ({status})"));
    }
    Ok(out)
}

impl RoundRunner for CommandRunner {
    fn run<'a>(
        &'a self,
        template: &'a FeedTemplate,
        subject: &'a Subject,
        _now: u64,
        deadline: u64,
    ) -> BoxFuture<'a, Result<Transcript, String>> {
        let mut argv = self.command.clone();
        argv.push(template.metric.0.clone());
        argv.push(subject_text(subject).to_owned());
        let limit = Duration::from_secs(deadline.saturating_sub((self.clock)()));
        Box::pin(async move {
            let out = tokio::task::spawn_blocking(move || run_command(&argv, limit))
                .await
                .map_err(|e| format!("the round command: {e}"))??;
            let path = out.lines().next().map(str::trim).unwrap_or_default();
            if !path.starts_with('/') {
                return Err("the round command printed no store path".into());
            }
            let text = read_capped(Path::new(path))?;
            let feed = ObservationStore::from_json(&text, (self.clock)(), &self.pins)
                .map_err(|e| format!("the round's store: {e}"))?;
            if feed.template != *template {
                return Err("the round's store is for another template".into());
            }
            feed.record(subject_text(subject), None, None)
                .map(|r| r.transcript.clone())
                .ok_or_else(|| "the round's store has no round for the subject".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores() -> Vec<PathBuf> {
        vec![PathBuf::from("/var/lib/onyx/observations/usgs.json")]
    }

    const GOOD: &str = r#"{"command":["/usr/local/bin/onyx-observe-round","--state","/var/lib/onyx/observe"],
        "offers":{"usgs.earthquakes.m5_count":{"price":"2500","deadlineSecs":3600,
        "store":"/var/lib/onyx/observations/usgs.json"}}}"#;

    #[test]
    fn the_observe_file_parses_and_refuses_what_it_does_not_know() {
        let f = parse(GOOD, &stores()).expect("parses");
        assert_eq!(f.command.len(), 3);
        assert_eq!(f.offers["usgs.earthquakes.m5_count"].price, 2500);
        for (bad, why) in [
            (GOOD.replace("/usr/local/bin/", ""), "absolute"),
            (GOOD.replace("\"2500\"", "2500"), "decimal string"),
            (
                GOOD.replace("usgs.json\"}", "usgs.json\",\"x\":1}"),
                "only price",
            ),
            (
                GOOD.replace("observations/usgs.json", "other.json"),
                "not one of",
            ),
            (
                GOOD.replace("\"offers\":", "\"extra\":1,\"offers\":"),
                "only command",
            ),
            (r#"{"command":["/x"],"offers":{}}"#.to_owned(), "no offers"),
        ] {
            let e = parse(&bad, &stores()).expect_err(why);
            assert!(e.contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn a_command_past_its_limit_is_killed_and_a_failed_one_is_an_error() {
        let sh = |s: &str| vec!["/bin/sh".to_owned(), "-c".to_owned(), s.to_owned()];
        assert_eq!(
            run_command(&sh("echo /tmp/store.json"), Duration::from_secs(5)).expect("ok"),
            "/tmp/store.json\n"
        );
        assert!(run_command(&sh("exit 3"), Duration::from_secs(5))
            .expect_err("fails")
            .contains("failed"));
        let started = std::time::Instant::now();
        assert!(run_command(&sh("sleep 30"), Duration::from_millis(300))
            .expect_err("killed")
            .contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
