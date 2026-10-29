//! M8 rehearsal on a local chain (docs/ops/demo-local.md): runs
//! `scripts/demo-local.sh --dispute` and checks that every step passed.
//!
//! Gated behind `ONYX_DEMO_LOCAL=1` because it needs Foundry's `anvil`,
//! `forge` and `cast`, a free local port, and about a minute. The driver is
//! the release `demo-local` example (the recipe runs at full speed), built
//! first:
//!
//! ```text
//! cargo build --release -p onyx-source-btc --example demo-local
//! ONYX_DEMO_LOCAL=1 cargo test --release -p onyx-source-btc --test demo_local
//! ```
//!
//! `ONYX_DEMO_FORK=1` also runs the Base Sepolia rehearsal
//! (`scripts/demo-testnet.sh fork --dispute`, docs/ops/demo-testnet.md): an
//! anvil fork of Base Sepolia, so it needs the network (read-only: nothing is
//! broadcast).

use std::path::PathBuf;
use std::process::Command;

fn driver() -> PathBuf {
    // The test binary is target/<profile>/deps/demo_local-*; the example
    // driver is built next to it in target/<profile>/examples.
    let exe = std::env::current_exe().expect("test path");
    let driver = exe
        .parent()
        .and_then(|deps| deps.parent())
        .map(|p| p.join("examples").join("demo-local"))
        .expect("profile dir");
    assert!(
        driver.exists(),
        "{} missing: `cargo build --release -p onyx-source-btc --example demo-local` first",
        driver.display()
    );
    driver
}

/// Self-settled x402 payments into OnyxRevenue on the real USDC bytecode,
/// per-actor keys signed in the driver, the deployer funded with exactly the
/// planned budget (ADR-0026).
#[test]
fn base_sepolia_fork_rehearsal_self_settles_into_revenue() {
    if std::env::var("ONYX_DEMO_FORK").as_deref() != Ok("1") {
        eprintln!("skipped: set ONYX_DEMO_FORK=1 (needs anvil, forge, cast and the network)");
        return;
    }
    let repo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let out = Command::new("bash")
        .arg(repo.join("scripts/demo-testnet.sh"))
        .args(["fork", "--dispute"])
        .env("ONYX_DEMO_DRIVER", driver())
        .env(
            "ONYX_DEMO_RPC_PORT",
            std::env::var("ONYX_DEMO_FORK_RPC_PORT").unwrap_or_else(|_| "18546".into()),
        )
        .env(
            "ONYX_DEMO_API_PORT",
            std::env::var("ONYX_DEMO_FORK_API_PORT").unwrap_or_else(|_| "18404".into()),
        )
        .output()
        .expect("run the fork rehearsal");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "fork rehearsal failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for step in [
        "Fork mode, chain 84532 (anvil)",
        "0. funded 2 miners and 16 validators",
        "tag computed by the contract and by Rust: equal",
        "OnyxRevenue: the shard is registered",
        "(self-settled into OnyxRevenue, settled: true) -> 200",
        "booked 2500 micro-USDC to the shard in the same transaction",
        "the contract's verifyRow says true",
        "a changed payTo is refused before signing (checked, REV-4)",
        "the receipt was on disk before the answer",
        "the reporter posted it: credit tx",
        "applied 1 of 1",
        "miner 0 claimed and withdrew",
        "is final; checked: miner A credited",
        // The reserve round (ROADMAP A2): settled right after round 1, sold
        // behind its serving check with a self-settled payment.
        "5b. reserve round 3",
        "OnyxRevenue: the reserve shard is registered",
        "8. reserve shard (round 3) indexed",
        "shard cleared",
        "(self-settled into OnyxRevenue, settled: true) -> 200, ",
        "booked 2500 micro-USDC to the reserve shard in the same transaction",
        "Binance -> 403 not_cross_checked before any 402",
        "and the serving check refuses it",
        "REHEARSAL PASSED",
    ] {
        assert!(stdout.contains(step), "missing {step:?} in:\n{stdout}");
    }
}

/// ROADMAP B1 (THREATS L2API-4): `fork --level2` runs the recorded
/// Treasury level-2 round on the fork, opens its fact's revenue book from
/// the reporter key before serving it (and again: already open, nothing
/// sent), and sells the value with a payment self-settled into that book.
/// Ports apart from the test above (the two may run at once).
#[test]
fn base_sepolia_fork_rehearsal_opens_the_level2_book_before_serving() {
    if std::env::var("ONYX_DEMO_FORK").as_deref() != Ok("1") {
        eprintln!("skipped: set ONYX_DEMO_FORK=1 (needs anvil, forge, cast and the network)");
        return;
    }
    let repo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let out = Command::new("bash")
        .arg(repo.join("scripts/demo-testnet.sh"))
        .args(["fork", "--level2"])
        .env("ONYX_DEMO_DRIVER", driver())
        .env(
            "ONYX_DEMO_RPC_PORT",
            std::env::var("ONYX_DEMO_FORK_L2_RPC_PORT").unwrap_or_else(|_| "18556".into()),
        )
        .env(
            "ONYX_DEMO_API_PORT",
            std::env::var("ONYX_DEMO_FORK_L2_API_PORT").unwrap_or_else(|_| "18414".into()),
        )
        .output()
        .expect("run the fork rehearsal with level 2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "fork rehearsal with level 2 failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for step in [
        "Fork mode, chain 84532 (anvil)",
        "6b. Level 2: the recorded Treasury round",
        "OnyxRevenue: the reporter opened the observation book of fact",
        "opening again: already open, nothing sent",
        "the registry's observationLevel, asked by the client with its own arguments, says 2",
        "booked 2500 micro-USDC to the fact's observation book in the settle transaction",
        "REHEARSAL PASSED",
    ] {
        assert!(stdout.contains(step), "missing {step:?} in:\n{stdout}");
    }
}

#[test]
fn local_rehearsal_settles_serves_and_disputes() {
    if std::env::var("ONYX_DEMO_LOCAL").as_deref() != Ok("1") {
        eprintln!("skipped: set ONYX_DEMO_LOCAL=1 (needs anvil, forge and cast)");
        return;
    }
    let driver = driver();
    let repo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let out = Command::new("bash")
        .arg(repo.join("scripts/demo-local.sh"))
        .arg("--dispute")
        .env("ONYX_DEMO_DRIVER", &driver)
        .env(
            "ONYX_DEMO_RPC_PORT",
            std::env::var("ONYX_DEMO_RPC_PORT").unwrap_or_else(|_| "18545".into()),
        )
        .env(
            "ONYX_DEMO_API_PORT",
            std::env::var("ONYX_DEMO_API_PORT").unwrap_or_else(|_| "18403".into()),
        )
        .output()
        .expect("run the rehearsal");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The script reads the live block files from ONYX_LIVE_DIR, else the
    // repo's fixtures-live/node; without them it runs on the fixture.
    // An empty ONYX_LIVE_DIR counts as unset, as in the script (M8-L56-F5).
    let live_dir = std::env::var_os("ONYX_LIVE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("fixtures-live/node"));
    let live = live_dir.join("970543.raw.hex").exists();
    let reserve_dir = std::env::var_os("ONYX_RESERVE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("fixtures-live/tail"));
    let reserve = reserve_dir.join("971199.raw.hex").exists();
    assert!(
        out.status.success(),
        "rehearsal failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for step in [
        "tag computed by the contract and by Rust: equal",
        "(contract and Rust equal)",
        "withdrawn and checked: first committer",
        "-> 402; paid 2500 micro-USDC",
        "the contract's verifyRow says true",
        "a changed payTo is refused before signing (checked, REV-4)",
        "its receipt was on disk before the answer returned",
        "the shard is Pending, not final",
        "is final; checked: miner A credited",
        if live {
            "live data: root equals the pinned v1 live root; OKX equals the Python recount (checked)"
        } else {
            "fixture mode: no live block files"
        },
        if reserve {
            "and the serving check refuses it"
        } else {
            "8. reserve step skipped"
        },
        "REHEARSAL PASSED",
    ] {
        assert!(stdout.contains(step), "missing {step:?} in:\n{stdout}");
    }
}

/// M8-L58-F1: the testnet script refuses unknown flags and a testnet run
/// without `--yes`, before it reads `.env` or reaches any node (not gated:
/// nothing starts, nothing is sent).
#[test]
fn testnet_script_refuses_typos_and_unconfirmed_runs() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/demo-testnet.sh");
    for (args, want) in [
        (&["testnet", "--dry-runx"][..], "unknown argument"),
        (&["testnet", "--plna"][..], "unknown argument"),
        (&["fork", "--dispute=1"][..], "unknown argument"),
        (&["testnet", "--dispute"][..], "--yes"),
        (&["testnet"][..], "--yes"),
        (&["mainnet", "--yes"][..], "usage"),
    ] {
        let out = Command::new("bash")
            .arg(&script)
            .args(args)
            // An unset key and an unreachable node: a run that got past the
            // checks would fail loudly instead of sending anything.
            .env_remove("ONYX_DEMO_DRIVER")
            .env("ONYX_FORK_RPC", "http://127.0.0.1:9")
            .output()
            .expect("bash");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(err.contains(want), "{args:?}: {err}");
    }
}
