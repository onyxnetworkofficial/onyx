//! Shared test helpers. Tests open rounds the way a coordinator must: with the
//! preflight run (SER-R2-F1), never with the crate-private `Round::open`.
#![allow(dead_code)]

use onyx_coordinator::cas::Cas;
use onyx_coordinator::recompute::{FailureAllowList, RecomputeEnv};
use onyx_coordinator::round::{Round, RoundConfig, RoundError};
use onyx_coordinator::validate::Ledger;
use onyx_core::{keccak256, RuleSet, ShardSpec, Snapshot, SourceAdapter, StaticRegistry};
use onyx_recipe_host::Host;
use onyx_source_btc::{valid_failure_codes, BtcFixtureAdapter};
use std::path::PathBuf;

/// The adapter's valid recipe failures (series: no activity).
pub fn allowed() -> FailureAllowList {
    valid_failure_codes().into_iter().collect()
}

/// The fixture blocks every round test pins (921080-921082).
pub fn fixture_source() -> Vec<u8> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let adapter = BtcFixtureAdapter::new(root.join("fixtures"), "btc/blocks-921080-921082.json");
    let snapshot = adapter.range_snapshot().expect("snapshot");
    adapter.load(&snapshot).expect("load")
}

/// [`Round::open_preflighted`] with the fixture source and `extra` sources:
/// each spec gets the bytes whose hash its snapshot pins.
#[allow(clippy::too_many_arguments)]
pub fn open_round_with<R: RuleSet>(
    id: u64,
    specs: Vec<ShardSpec>,
    reg: &StaticRegistry,
    cas: &dyn Cas,
    extra: &[Vec<u8>],
    cfg: RoundConfig,
    now: u64,
    ledger: &mut Ledger<R>,
) -> Result<Round, RoundError> {
    let host = Host::new().expect("host");
    let mut sources = vec![fixture_source()];
    sources.extend(extra.iter().cloned());
    let get = move |spec: &ShardSpec| -> Option<Vec<u8>> {
        let want = match &spec.source.snapshot {
            Snapshot::BlockRange { content_hash, .. } => *content_hash,
            _ => return sources.first().cloned(),
        };
        sources.iter().find(|s| keccak256(s) == want).cloned()
    };
    let env = RecomputeEnv {
        reg,
        cas,
        host: &host,
        source: &get,
    };
    Round::open_preflighted(id, specs, &env, cfg, now, ledger, &allowed())
}

/// [`open_round_with`] with only the fixture source.
pub fn open_round<R: RuleSet>(
    id: u64,
    specs: Vec<ShardSpec>,
    reg: &StaticRegistry,
    cas: &dyn Cas,
    cfg: RoundConfig,
    now: u64,
    ledger: &mut Ledger<R>,
) -> Result<Round, RoundError> {
    open_round_with(id, specs, reg, cas, &[], cfg, now, ledger)
}
