//! Run the verifier in a separate process with an address-space limit and a deadline (THREATS
//! TLSN-5). tlsn-core allocates a transcript length read from the presentation before checking it,
//! so a crafted file can make the verifier ask for about a terabyte and abort. Inside the API that
//! abort would take the server down; here it ends one child process, and the presentation is
//! simply refused. This is the shape the integration plan asks for (README.md, step 2).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What one isolated verification gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Isolated {
    /// Every check passed; the verifier's report.
    Verified(String),
    /// A check failed; the verifier's first error line.
    Refused(String),
    /// The child died (an abort, the memory limit, a signal) or ran past the deadline: refused.
    Crashed(String),
}

/// Run `verify_bin PRESENTATION --notary-key HEX --now NOW [--max-age-secs N]` under
/// `ulimit -v mem_kib` and kill it after `deadline`.
pub fn verify_isolated(
    verify_bin: &Path,
    presentation: &Path,
    notary_key_hex: &str,
    now: u64,
    max_age_secs: Option<u64>,
    mem_kib: u64,
    deadline: Duration,
) -> Isolated {
    let mut args = vec![
        presentation.display().to_string(),
        "--notary-key".into(),
        notary_key_hex.into(),
        "--now".into(),
        now.to_string(),
    ];
    if let Some(a) = max_age_secs {
        args.push("--max-age-secs".into());
        args.push(a.to_string());
    }
    let child = Command::new("sh")
        .arg("-c")
        .arg(format!("ulimit -v {mem_kib}; exec \"$0\" \"$@\""))
        .arg(verify_bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Isolated::Crashed(format!("could not start the verifier: {e}")),
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Isolated::Crashed(format!("no answer within {} s", deadline.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Isolated::Crashed(format!("wait: {e}")),
        }
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    match status.code() {
        Some(0) => Isolated::Verified(out),
        Some(_) => Isolated::Refused(
            err.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("refused")
                .to_string(),
        ),
        None => Isolated::Crashed(format!("the verifier process died ({status})")),
    }
}
