//! Level-2 rounds of this source (feature `round`): the shared runner
//! ([`onyx_level2`], NS3) with this adapter's template, fetcher and registry.

use crate::{registry, spec, template, GhcndFetcher};
use onyx_core::{FeedTemplate, Fetcher, ObservationSpec, StaticRegistry};
use onyx_level2::{Shown, Source};

/// This adapter as a level-2 source.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ghcnd;

impl Source for Ghcnd {
    fn template(&self) -> FeedTemplate {
        template()
    }
    fn fetcher(&self) -> &dyn Fetcher {
        &GhcndFetcher
    }
    fn spec(&self, date: &str, window_start: u64) -> Result<ObservationSpec, String> {
        spec(date, window_start)
    }
    fn registry(&self, date: &str) -> StaticRegistry {
        registry(date)
    }
    fn register_note(&self) -> &'static str {
        "GHCN-Daily TMAX at USW00003017: rights reviewed (fixtures/PROVENANCE.md), feed pinned"
    }
    fn key_label(&self) -> &'static str {
        "ncei rehearsal"
    }
    fn shown(&self) -> Shown {
        Shown {
            name: crate::SOURCE_NAME,
            terms_url: crate::TERMS_URL,
            terms_quote: crate::TERMS_QUOTE,
            unit: "degrees Celsius",
        }
    }
}

/// The recorded round's draw as the rehearsal's local test chain (anvil)
/// recorded it (`ONYX_L2_ONLY=daily scripts/level2-rehearsal.sh`), for
/// replaying the recorded round without that chain (L2RND-R1).
pub fn recorded_draw() -> onyx_coordinator::observe::DrawRecord {
    let v = serde_json::from_str(include_str!(
        "../fixtures/tmax-usw00003017-2026-09-01/draw.json"
    ))
    .expect("committed draw record parses");
    onyx_coordinator::observe::DrawRecord::from_json(&v)
        .expect("committed draw record is well formed")
}

/// One more GHCN-Daily series as a level-2 source (north-star loop NL17,
/// [`crate::series`]).
#[derive(Clone, Copy, Debug)]
pub struct GhcndSeries(pub &'static crate::series::Series);

impl Source for GhcndSeries {
    fn template(&self) -> FeedTemplate {
        self.0.template()
    }
    fn fetcher(&self) -> &dyn Fetcher {
        &crate::series::SeriesFetcher
    }
    fn spec(&self, date: &str, window_start: u64) -> Result<ObservationSpec, String> {
        self.0.spec(date, window_start)
    }
    fn registry(&self, date: &str) -> StaticRegistry {
        self.0.registry(date)
    }
    fn register_note(&self) -> &'static str {
        self.0.note
    }
    fn key_label(&self) -> &'static str {
        "ncei series rehearsal"
    }
    fn shown(&self) -> Shown {
        Shown {
            name: self.0.name,
            terms_url: crate::TERMS_URL,
            terms_quote: crate::TERMS_QUOTE,
            unit: self.0.unit,
        }
    }
}

/// The source the live examples run: the Denver TMAX feed, or with
/// `ONYX_L2_SERIES=<id>` one of [`crate::series::ALL`] (`usw00003017.prcp`,
/// `usw00094728.tmax`, `usw00094728.prcp`).
pub fn from_env() -> Result<Box<dyn Source>, String> {
    match std::env::var("ONYX_L2_SERIES") {
        Err(_) => Ok(Box::new(Ghcnd)),
        Ok(id) if id.is_empty() => Ok(Box::new(Ghcnd)),
        Ok(id) => crate::series::by_id(&id)
            .map(|s| Box::new(GhcndSeries(s)) as Box<dyn Source>)
            .ok_or_else(|| format!("ONYX_L2_SERIES={id}: not a series of this source")),
    }
}
