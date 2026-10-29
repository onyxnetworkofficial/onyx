//! Level-2 rounds of this source (feature `round`): the shared runner
//! ([`onyx_level2`], NS3) with this adapter's template, fetcher and registry.

use crate::{registry, spec, template, UsgsCountFetcher};
use onyx_core::{FeedTemplate, Fetcher, ObservationSpec, StaticRegistry};
use onyx_level2::{Shown, Source};

/// This adapter as a level-2 source.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usgs;

impl Source for Usgs {
    fn template(&self) -> FeedTemplate {
        template()
    }
    fn fetcher(&self) -> &dyn Fetcher {
        &UsgsCountFetcher
    }
    fn spec(&self, date: &str, window_start: u64) -> Result<ObservationSpec, String> {
        spec(date, window_start)
    }
    fn registry(&self, date: &str) -> StaticRegistry {
        registry(date)
    }
    fn register_note(&self) -> &'static str {
        "USGS earthquake count: rights reviewed (fixtures/PROVENANCE.md), feed pinned"
    }
    fn key_label(&self) -> &'static str {
        "usgs rehearsal"
    }
    fn shown(&self) -> Shown {
        Shown {
            name: crate::SOURCE_NAME,
            terms_url: crate::TERMS_URL,
            terms_quote: crate::TERMS_QUOTE,
            unit: "earthquakes",
        }
    }
}

/// The recorded round's draw as the rehearsal's local test chain (anvil)
/// recorded it (`ONYX_L2_ONLY=daily scripts/level2-rehearsal.sh`), for
/// replaying the recorded round without that chain (L2RND-R1).
pub fn recorded_draw() -> onyx_coordinator::observe::DrawRecord {
    let v = serde_json::from_str(include_str!("../fixtures/quakes-m5-2026-07-08/draw.json"))
        .expect("committed draw record parses");
    onyx_coordinator::observe::DrawRecord::from_json(&v)
        .expect("committed draw record is well formed")
}

/// A regional earthquake count as a level-2 source (north-star loop NL17,
/// [`crate::region`]).
#[derive(Clone, Copy, Debug)]
pub struct UsgsRegion(pub &'static crate::region::Region);

impl Source for UsgsRegion {
    fn template(&self) -> FeedTemplate {
        self.0.template()
    }
    fn fetcher(&self) -> &dyn Fetcher {
        &crate::region::RegionCountFetcher
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
        "usgs region rehearsal"
    }
    fn shown(&self) -> Shown {
        Shown {
            name: self.0.name,
            terms_url: crate::TERMS_URL,
            terms_quote: crate::TERMS_QUOTE,
            unit: "earthquakes",
        }
    }
}

/// The source the live examples run: the world M5+ feed, or with
/// `ONYX_L2_SERIES=<id>` one of [`crate::region::ALL`] (`la300.m2_5`).
pub fn from_env() -> Result<Box<dyn Source>, String> {
    match std::env::var("ONYX_L2_SERIES") {
        Err(_) => Ok(Box::new(Usgs)),
        Ok(id) if id.is_empty() => Ok(Box::new(Usgs)),
        Ok(id) => crate::region::by_id(&id)
            .map(|r| Box::new(UsgsRegion(r)) as Box<dyn Source>)
            .ok_or_else(|| format!("ONYX_L2_SERIES={id}: not a region of this source")),
    }
}
