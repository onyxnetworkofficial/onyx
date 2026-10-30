//! Source manifests (ADR-0053 items 5 and 6, contributor loop CL3): a public JSON source
//! reaches level 2 as reviewed DATA, with no Rust of its own.
//!
//! A manifest is `sources/<id>/manifest.json` next to its `PROVENANCE.md` (the terms, quoted
//! with link and date) and `recording/` (one recorded response). It holds the metric, the names
//! shown, the terms row it is cleared under, every [`FeedTemplate`] field but the fetcher (the
//! subject rule and its date lag included), the [`JsonDescriptor`], the source's rate limits,
//! and the stated `fetcher_id` and `template_id`. [`load`] refuses it unless:
//! 1. it is strict JSON (no duplicate key, `onyx_json::parse`) with no unknown field;
//! 2. the descriptor is valid and fits the template (`JsonDescriptor::check_template`);
//! 3. the template is valid, numeric, dated, daily or bucketed (ADR-0058), and its URL passes
//!    `onyx_fetch::check_template_url` (a DNS host, no port, no secret parameter, not blocked);
//! 4. its rights allow redistribution;
//! 5. its host is cleared: a row of docs/ops/data-sources.md section 6 names the host, says
//!    `OK`, and points at a section 1 row whose decision is `OK` (THREATS CL3-7);
//! 6. the stated ids equal the recomputed ones (they exist for review and diffs).
//!
//! A loaded manifest is a [`ManifestSource`], an [`crate::Source`] like any adapter's, so the
//! same rehearsal, coordinator, observer and client check run it. The on-chain pin is checked
//! with [`ManifestSource::check_registered`] (THREATS CL3-8).

use crate::{Shown, Source};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::feed::{civil_date, day_number};
use onyx_core::feed_json::{DescriptorError, JsonDescriptor};
use onyx_core::{
    DataRights, DateLag, FeedTemplate, Fetcher, Hash32, MetricInfo, ObservationKind,
    ObservationSpec, Redistribution, SourceId, StaticRegistry, SubjectRule, ValueKind,
};
use onyx_json::GenericJsonFetcher;
use serde_json::{Map, Value as Json};

/// The only manifest version.
pub const MANIFEST_VERSION: u64 = 1;
/// Where reviewed manifests live, from the repository root.
pub const SOURCES_DIR: &str = "sources";
/// The terms document whose section 6 clears hosts, from the repository root.
pub const DATA_SOURCES_DOC: &str = "docs/ops/data-sources.md";
/// Section 1's heading prefix in [`DATA_SOURCES_DOC`].
const SECTION1: &str = "## 1. Current sources";
/// Section 6's heading prefix in [`DATA_SOURCES_DOC`].
const SECTION6: &str = "## 6. Hosts cleared for source manifests";

/// Why a manifest does not load.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Not strict JSON.
    #[error("manifest is not strict JSON: {0}")]
    Json(String),
    /// A field is missing, unknown or malformed.
    #[error("manifest field {0}")]
    Field(String),
    /// The descriptor is refused.
    #[error("{0}")]
    Descriptor(#[from] DescriptorError),
    /// The template is refused.
    #[error("{0}")]
    Template(#[from] onyx_core::TemplateError),
    /// The URL breaks the fetch rules.
    #[error("{0}")]
    Url(#[from] onyx_fetch::FetchError),
    /// The host has no cleared terms row (THREATS CL3-7).
    #[error("not cleared: {0}")]
    NotCleared(String),
    /// A stated id is not the recomputed one.
    #[error("stated {what} {stated} is not the recomputed {computed}")]
    StatedId {
        /// Which id.
        what: &'static str,
        /// The file's.
        stated: String,
        /// Recomputed.
        computed: Hash32,
    },
    /// Not served by manifests in version 1.
    #[error("unsupported in manifest v1: {0}")]
    Unsupported(&'static str),
}

/// A source's rate limits (ADR-0053 item 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLimits {
    /// Rounds a day over every fact of the source.
    pub max_rounds_per_day: u32,
    /// Fetches per window (at most `DEFAULT_MAX_FETCHES_PER_ORIGIN`).
    pub origin_budget: u32,
    /// Least seconds between two fetches, from the source's terms.
    pub min_fetch_gap_secs: u32,
}

fn effective_budget(l: &SourceLimits, window_secs: u64) -> u32 {
    let by_gap = window_secs / u64::from(l.min_fetch_gap_secs.max(1));
    l.origin_budget
        .min(u32::try_from(by_gap).unwrap_or(u32::MAX))
}

/// A loaded, checked manifest.
#[derive(Clone, Debug)]
pub struct SourceManifest {
    /// The source id (its directory under `sources/`).
    pub id: String,
    /// The section 1 row of docs/ops/data-sources.md it is cleared under.
    pub terms_row: String,
    /// The name shown.
    pub name: String,
    /// The terms page.
    pub terms_url: String,
    /// The terms, quoted.
    pub terms_quote: String,
    /// The reviewer's note.
    pub note: String,
    /// The template (fetcher filled from the descriptor).
    pub template: FeedTemplate,
    /// The extraction rule.
    pub descriptor: JsonDescriptor,
    /// Rate limits.
    pub limits: SourceLimits,
    /// Seconds after a date's 00:00 UTC before which the source shows nothing about it.
    pub earliest_public_offset_secs: u64,
    /// The recorded response, relative to the manifest's directory.
    pub recording: String,
}

/// Hosts cleared by docs/ops/data-sources.md (sections 1 and 6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClearedHosts {
    /// `(host, row)` of every section 6 row whose decision is `OK` and whose section 1 row's
    /// decision starts with `OK`, sorted.
    pub hosts: Vec<(String, String)>,
    /// The same for rows whose decision is exactly `OK keyed` (ADR-0054 item 9, THREATS
    /// CL4-6): the reviewer read the provider's terms and they allow passing on data obtained
    /// with a personal key. Only these hosts take keyed manifests, and a plain `OK` row never
    /// clears a keyed one.
    pub keyed_hosts: Vec<(String, String)>,
}

fn md_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().trim_matches('`').trim_matches('*').to_string())
        .collect()
}

fn md_section<'a>(md: &'a str, head: &str) -> Vec<&'a str> {
    let mut out = vec![];
    let mut inside = false;
    for line in md.lines() {
        if line.starts_with("## ") {
            inside = line.starts_with(head);
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    out
}

impl ClearedHosts {
    /// Read the cleared hosts from the text of docs/ops/data-sources.md.
    pub fn from_data_sources(md: &str) -> Result<Self, ManifestError> {
        let s6 = md_section(md, SECTION6);
        if s6.is_empty() {
            return Err(ManifestError::NotCleared(format!(
                "{DATA_SOURCES_DOC} has no section {SECTION6:?}"
            )));
        }
        let decisions: Vec<(String, String)> = md_section(md, SECTION1)
            .into_iter()
            .filter(|l| l.starts_with("| S"))
            .map(md_cells)
            .filter(|c| c.len() >= 5)
            .map(|c| (c[0].clone(), c[4].clone()))
            .collect();
        let mut hosts = vec![];
        let mut keyed_hosts = vec![];
        for row in s6.into_iter().filter(|l| l.starts_with('|')).map(md_cells) {
            // | Host | Row | Decision | Manifests |
            let [host, sid, decision, ..] = row.as_slice() else {
                continue;
            };
            if host == "Host" || host.starts_with('-') {
                continue;
            }
            let row_ok = decisions
                .iter()
                .any(|(id, d)| id == sid && d.starts_with("OK"));
            if decision == "OK" && row_ok {
                hosts.push((host.clone(), sid.clone()));
            }
            if decision == "OK keyed" && row_ok {
                keyed_hosts.push((host.clone(), sid.clone()));
            }
        }
        hosts.sort();
        hosts.dedup();
        keyed_hosts.sort();
        keyed_hosts.dedup();
        Ok(ClearedHosts { hosts, keyed_hosts })
    }

    /// Read [`DATA_SOURCES_DOC`] under `repo`.
    pub fn from_repo(repo: &std::path::Path) -> Result<Self, ManifestError> {
        let p = repo.join(DATA_SOURCES_DOC);
        let md = std::fs::read_to_string(&p)
            .map_err(|e| ManifestError::NotCleared(format!("{}: {e}", p.display())))?;
        Self::from_data_sources(&md)
    }

    /// `Ok` when `host` is cleared under `row`.
    pub fn check(&self, host: &str, row: &str) -> Result<(), ManifestError> {
        if self.hosts.iter().any(|(h, r)| h == host && r == row) {
            Ok(())
        } else {
            Err(ManifestError::NotCleared(format!(
                "{host} has no OK row for {row} in {DATA_SOURCES_DOC} sections 1 and 6"
            )))
        }
    }

    /// `Ok` when `host` is cleared for KEYED manifests under `row` (an `OK keyed` row).
    pub fn check_keyed(&self, host: &str, row: &str) -> Result<(), ManifestError> {
        if self.keyed_hosts.iter().any(|(h, r)| h == host && r == row) {
            Ok(())
        } else {
            Err(ManifestError::NotCleared(format!(
                "{host} has no `OK keyed` row for {row} in {DATA_SOURCES_DOC} section 6: a keyed source loads only when a reviewer cleared its terms for data obtained with a personal key (ADR-0054 item 9)"
            )))
        }
    }

    /// The cleared host names, sorted (the intake's pull set, ADR-0053 item 6).
    pub fn host_names(&self) -> Vec<&str> {
        let mut h: Vec<&str> = self.hosts.iter().map(|(h, _)| h.as_str()).collect();
        h.dedup();
        h
    }
}

fn field(why: impl Into<String>) -> ManifestError {
    ManifestError::Field(why.into())
}

fn obj<'a>(
    v: &'a Json,
    allowed: &[&str],
    what: &str,
) -> Result<&'a Map<String, Json>, ManifestError> {
    let o = v
        .as_object()
        .ok_or_else(|| field(format!("{what}: not an object")))?;
    if let Some(k) = o.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(field(format!("{what}: unknown field {k}")));
    }
    Ok(o)
}

fn get<'a>(o: &'a Map<String, Json>, k: &str) -> Result<&'a Json, ManifestError> {
    o.get(k).ok_or_else(|| field(format!("{k}: missing")))
}

fn s(o: &Map<String, Json>, k: &str) -> Result<String, ManifestError> {
    get(o, k)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| field(format!("{k}: not a string")))
}

fn u(o: &Map<String, Json>, k: &str) -> Result<u64, ManifestError> {
    get(o, k)?
        .as_u64()
        .ok_or_else(|| field(format!("{k}: not a whole number")))
}

fn day(v: &Json, what: &str) -> Result<i64, ManifestError> {
    v.as_str()
        .and_then(day_number)
        .ok_or_else(|| field(format!("{what}: not a YYYY-MM-DD date")))
}

fn small<T: TryFrom<u64>>(o: &Map<String, Json>, k: &str) -> Result<T, ManifestError> {
    T::try_from(u(o, k)?).map_err(|_| field(format!("{k}: out of range")))
}

fn rights_of(v: &Json) -> Result<DataRights, ManifestError> {
    let o = obj(
        v,
        &[
            "license",
            "redistribution",
            "source",
            "attribution",
            "terms",
        ],
        "rights",
    )?;
    let redistribution = match s(o, "redistribution")?.as_str() {
        "allowed" => Redistribution::Allowed,
        "restricted" => Redistribution::Restricted,
        "forbidden" => Redistribution::Forbidden,
        _ => return Err(field("rights.redistribution")),
    };
    let attribution = match get(o, "attribution")? {
        Json::Null => None,
        Json::String(a) => Some(a.clone()),
        _ => return Err(field("rights.attribution")),
    };
    let r = DataRights {
        license: s(o, "license")?,
        redistribution,
        source: SourceId(s(o, "source")?),
        attribution,
        terms: s(o, "terms")?,
    };
    r.check_ingest()
        .map_err(|e| ManifestError::NotCleared(format!("rights: {e}")))?;
    Ok(r)
}

fn subject_of(v: &Json) -> Result<SubjectRule, ManifestError> {
    let o = obj(
        v,
        &["bucketed", "dated", "daily", "fixed", "period"],
        "subject",
    )?;
    if o.len() != 1 {
        return Err(field("subject: exactly one rule"));
    }
    if let Some(d) = o.get("dated") {
        let d = obj(d, &["prefix", "lag"], "subject.dated")?;
        let l = obj(
            get(d, "lag")?,
            &[
                "min_business_days",
                "max_calendar_days",
                "first_day",
                "last_day",
                "holidays",
            ],
            "subject.dated.lag",
        )?;
        let holidays = get(l, "holidays")?
            .as_array()
            .ok_or_else(|| field("holidays: not a list"))?
            .iter()
            .map(|h| day(h, "holidays"))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(SubjectRule::Dated {
            prefix: s(d, "prefix")?,
            lag: DateLag {
                min_business_days: small(l, "min_business_days")?,
                max_calendar_days: small(l, "max_calendar_days")?,
                first_day: day(get(l, "first_day")?, "first_day")?,
                last_day: day(get(l, "last_day")?, "last_day")?,
                holidays,
            },
        });
    }
    if let Some(d) = o.get("daily") {
        let d = obj(d, &["prefix", "min_days", "max_days"], "subject.daily")?;
        return Ok(SubjectRule::Daily {
            prefix: s(d, "prefix")?,
            min_days: small(d, "min_days")?,
            max_days: small(d, "max_days")?,
        });
    }
    // ADR-0058 item 2 (contributor loop CL8): one closed bucket of one venue on the UTC grid.
    if let Some(d) = o.get("bucketed") {
        let d = obj(
            d,
            &["prefix", "bucket_secs", "min_delay_secs", "max_delay_secs"],
            "subject.bucketed",
        )?;
        return Ok(SubjectRule::Bucketed {
            prefix: s(d, "prefix")?,
            bucket_secs: small(d, "bucket_secs")?,
            min_delay_secs: small(d, "min_delay_secs")?,
            max_delay_secs: small(d, "max_delay_secs")?,
        });
    }
    Err(ManifestError::Unsupported(
        "a fixed or period subject (v1 serves dated, daily and bucketed subjects)",
    ))
}

fn kind_of(v: &Json) -> Result<ObservationKind, ManifestError> {
    let o = obj(v, &["numeric"], "kind")?;
    let n = obj(get(o, "numeric")?, &["scale", "tolerance"], "kind.numeric")?;
    let tolerance = s(n, "tolerance")?
        .parse::<u128>()
        .map_err(|_| field("kind.numeric.tolerance: a decimal string"))?;
    Ok(ObservationKind::Numeric {
        scale: small(n, "scale")?,
        tolerance,
    })
}

fn hash_field(o: &Map<String, Json>, k: &str) -> Result<String, ManifestError> {
    let h = s(o, k)?;
    let ok = h.len() == 66
        && h.starts_with("0x")
        && h[2..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
    if ok {
        Ok(h)
    } else {
        Err(field(format!("{k}: 0x and 64 lower-case hex digits")))
    }
}

/// Parse and check a manifest's bytes (see the module docs).
pub fn load(bytes: &[u8], cleared: &ClearedHosts) -> Result<SourceManifest, ManifestError> {
    load_with(bytes, cleared, true)
}

/// [`load`], optionally without comparing the stated ids (the review tool prints the
/// recomputed ones for a new manifest; nothing else may skip the comparison).
pub fn load_with(
    bytes: &[u8],
    cleared: &ClearedHosts,
    check_ids: bool,
) -> Result<SourceManifest, ManifestError> {
    // Duplicate keys refuse the file: serde_json would keep the last copy silently.
    onyx_json::parse(bytes, 1 << 20).map_err(|e| ManifestError::Json(e.to_string()))?;
    let v: Json = serde_json::from_slice(bytes).map_err(|e| ManifestError::Json(e.to_string()))?;
    let m = obj(
        &v,
        &[
            "v",
            "id",
            "metric",
            "name",
            "terms_row",
            "terms_url",
            "terms_quote",
            "note",
            "template",
            "descriptor",
            "limits",
            "earliest_public_offset_secs",
            "recording",
            "fetcher_id",
            "template_id",
        ],
        "manifest",
    )?;
    if u(m, "v")? != MANIFEST_VERSION {
        return Err(field("v: only version 1"));
    }
    let metric = s(m, "metric")?;
    let t = obj(
        get(m, "template")?,
        &[
            "url_template",
            "selector",
            "kind",
            "window_secs",
            "min_observers",
            "rights",
            "subject",
        ],
        "template",
    )?;
    let form = serde_json::to_vec(get(m, "descriptor")?).map_err(|e| field(e.to_string()))?;
    let descriptor = onyx_json::descriptor_from_json(&form).map_err(|e| match e {
        onyx_json::FormError::Invalid(d) => ManifestError::Descriptor(d),
        other => field(format!("descriptor: {other}")),
    })?;
    let template = FeedTemplate {
        feed: metric.clone(),
        metric: MetricId(metric.clone()),
        fetcher: descriptor.fetcher_id(),
        url_template: s(t, "url_template")?,
        selector: s(t, "selector")?,
        kind: kind_of(get(t, "kind")?)?,
        window_secs: u(t, "window_secs")?,
        min_observers: small(t, "min_observers")?,
        rights: rights_of(get(t, "rights")?)?,
        subject: subject_of(get(t, "subject")?)?,
    };
    template.validate()?;
    descriptor.check_template(&template)?;
    let terms_row = s(m, "terms_row")?;
    match crate::keyed::placement_of(&descriptor.access) {
        None => {
            let target = onyx_fetch::check_template_url(&template.url_template)?;
            cleared.check(&target.host, &terms_row)?;
        }
        // ADR-0054 items 4 and 9 (THREATS CL4-6): a keyed source needs an `OK keyed` terms row
        // and at least KEYED_MIN_OBSERVERS observers per round.
        Some(placement) => {
            let target = onyx_fetch::check_keyed_template_url(&template.url_template, &placement)?;
            cleared.check_keyed(&target.host, &terms_row)?;
            if template.min_observers < crate::keyed::KEYED_MIN_OBSERVERS {
                return Err(field(format!(
                    "template.min_observers: a keyed source needs at least {}",
                    crate::keyed::KEYED_MIN_OBSERVERS
                )));
            }
        }
    }
    let l = obj(
        get(m, "limits")?,
        &["max_rounds_per_day", "origin_budget", "min_fetch_gap_secs"],
        "limits",
    )?;
    let limits = SourceLimits {
        max_rounds_per_day: small(l, "max_rounds_per_day")?,
        origin_budget: small(l, "origin_budget")?,
        min_fetch_gap_secs: small(l, "min_fetch_gap_secs")?,
    };
    if limits.max_rounds_per_day == 0
        || limits.origin_budget == 0
        || limits.origin_budget > onyx_coordinator::origin_budget::DEFAULT_MAX_FETCHES_PER_ORIGIN
    {
        return Err(field("limits: rounds and budget 1..=16"));
    }
    if effective_budget(&limits, template.window_secs) < u32::from(template.min_observers) {
        return Err(field(
            "limits: the origin budget (capped at window_secs / min_fetch_gap_secs) admits no round",
        ));
    }
    for (what, computed) in [
        ("fetcher_id", descriptor.fetcher_id()),
        ("template_id", template.id()),
    ] {
        let stated = hash_field(m, what)?;
        if check_ids && stated != computed.to_string() {
            return Err(ManifestError::StatedId {
                what,
                stated,
                computed,
            });
        }
    }
    let id = s(m, "id")?;
    let id_ok = !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'_' || c == b'-'
        });
    if !id_ok {
        return Err(field("id: lower-case letters, digits, '.', '_' and '-'"));
    }
    let recording = s(m, "recording")?;
    if recording.contains("..") || recording.starts_with('/') {
        return Err(field("recording: a path inside the source's directory"));
    }
    Ok(SourceManifest {
        id,
        terms_row,
        name: s(m, "name")?,
        terms_url: s(m, "terms_url")?,
        terms_quote: s(m, "terms_quote")?,
        note: s(m, "note")?,
        template,
        descriptor,
        limits,
        earliest_public_offset_secs: u(m, "earliest_public_offset_secs")?,
        recording,
    })
}

/// A loaded manifest as a level-2 [`Source`].
#[derive(Debug)]
pub struct ManifestSource {
    m: SourceManifest,
    fetcher: GenericJsonFetcher,
    // Loaded once per process; the runner's `Source` trait shows `'static` text.
    shown: Shown,
    note: &'static str,
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

impl ManifestSource {
    /// The source of a checked manifest.
    pub fn new(m: SourceManifest) -> Result<Self, ManifestError> {
        let fetcher = GenericJsonFetcher::new(m.descriptor.clone())?;
        let shown = Shown {
            name: leak(&m.name),
            terms_url: leak(&m.terms_url),
            terms_quote: leak(&m.terms_quote),
            unit: leak(&m.descriptor.unit),
        };
        let note = leak(&m.note);
        Ok(ManifestSource {
            m,
            fetcher,
            shown,
            note,
        })
    }

    /// Load `path` (a `manifest.json`) checked against the terms of the repository at `repo`.
    pub fn from_file(
        path: &std::path::Path,
        repo: &std::path::Path,
    ) -> Result<Self, ManifestError> {
        let bytes = std::fs::read(path).map_err(|e| field(format!("{}: {e}", path.display())))?;
        Self::new(load(&bytes, &ClearedHosts::from_repo(repo)?)?)
    }

    /// The manifest.
    pub fn manifest(&self) -> &SourceManifest {
        &self.m
    }

    /// The hash of the terms a key holder is shown and accepts before taking a keyed seat
    /// (ADR-0054 item 5): `keccak256` of the manifest's `terms_quote`.
    pub fn terms_hash(&self) -> Hash32 {
        crate::keyed::terms_hash(&self.m.terms_quote)
    }

    /// The largest body an observer reads for it.
    pub fn max_bytes(&self) -> usize {
        self.m.descriptor.max_bytes as usize
    }

    /// Check the return of `OnyxRegistry.metrics(metricKey)` against this manifest's template
    /// (THREATS CL3-8): a manifest whose template id is not the registered one is refused, so
    /// a forged manifest on a site or an API is caught by any client with a node.
    pub fn check_registered(&self, ret: &[u8]) -> Result<(), String> {
        onyx_api::observations::check_registered(&self.m.template, ret).map_err(|e| e.to_string())
    }

    /// The fetches one origin may get from overlapping rounds of this source (ADR-0053 item
    /// 8): the manifest's `origin_budget`, capped at `window_secs / min_fetch_gap_secs`.
    pub fn origin_budget(&self) -> u32 {
        effective_budget(&self.m.limits, self.m.template.window_secs)
    }

    /// Admit a round of `spec` into `load` under this source's budget, or say why not
    /// (THREATS CL3-6): a round whose observers would push the origin over the budget is
    /// refused before any draw is announced.
    pub fn admit(
        &self,
        load: &mut onyx_coordinator::origin_budget::OriginLoad,
        spec: &ObservationSpec,
    ) -> Result<(), String> {
        let used = load.load(spec).map_err(|e| e.to_string())?;
        let want = u32::from(spec.min_observers);
        if used.saturating_add(want) > self.origin_budget() {
            return Err(format!(
                "{}: {used} fetches already overlap this window; this source's budget is {}",
                self.m.id,
                self.origin_budget()
            ));
        }
        load.admit(spec).map_err(|e| e.to_string())
    }

    /// The subject of `key`: a date (`YYYY-MM-DD`) for a dated or daily manifest, a bucket's
    /// start (canonical decimal Unix seconds) for a bucketed one (ADR-0058).
    fn subject(&self, key: &str) -> Result<Subject, String> {
        let prefix = match &self.m.template.subject {
            SubjectRule::Dated { prefix, .. }
            | SubjectRule::Daily { prefix, .. }
            | SubjectRule::Bucketed { prefix, .. } => prefix,
            _ => return Err("a manifest subject is dated, daily or bucketed".into()),
        };
        Ok(Subject::Aggregate(format!("{prefix}:{key}")))
    }

    /// The bucket a bucketed manifest's `key` names (ADR-0058), `None` for any other manifest.
    pub fn bucket(&self, key: &str) -> Option<onyx_core::Bucket> {
        self.m.template.bucket_of(&self.subject(key).ok()?)
    }
}

impl Source for ManifestSource {
    fn template(&self) -> FeedTemplate {
        self.m.template.clone()
    }
    fn fetcher(&self) -> &dyn Fetcher {
        &self.fetcher
    }
    fn spec(&self, date: &str, window_start: u64) -> Result<ObservationSpec, String> {
        self.m
            .template
            .instantiate(&self.subject(date)?, window_start)
            .map_err(|e| format!("date {date}: {e}"))
    }
    fn registry(&self, date: &str) -> StaticRegistry {
        let value_kind = match self.m.template.kind {
            ObservationKind::Numeric { scale: 0, .. } => ValueKind::Int,
            ObservationKind::Numeric { scale, .. } => ValueKind::Fixed { scale },
            ObservationKind::Exact { .. } => ValueKind::Bytes,
        };
        let mut r = StaticRegistry::new();
        let subject = match self.subject(date) {
            Ok(Subject::Aggregate(s)) => s,
            _ => return r,
        };
        let _ = r
            .add_metric(
                &self.m.template.metric.0,
                MetricInfo {
                    value_kind,
                    needs_label_set: false,
                },
            )
            .and_then(|r| r.add_aggregate(&subject))
            .and_then(|r| r.add_source(self.m.template.rights.clone()));
        r
    }
    fn register_note(&self) -> &'static str {
        self.note
    }
    fn key_label(&self) -> &'static str {
        "manifest rehearsal"
    }
    fn shown(&self) -> Shown {
        self.shown
    }
    fn earliest_public(&self, date: &str) -> Result<u64, String> {
        // ADR-0058: a bucket's trades start to print at its start.
        let start = match &self.m.template.subject {
            SubjectRule::Bucketed { .. } => {
                self.bucket(date)
                    .ok_or_else(|| format!("{date}: not a bucket of this feed"))?
                    .start
            }
            _ => crate::start_of_day(date)?,
        };
        start
            .checked_add(self.m.earliest_public_offset_secs)
            .ok_or_else(|| "earliest public overflows".into())
    }
    fn latest(&self, window_start: u64) -> Option<String> {
        match &self.m.template.subject {
            // ADR-0058: the newest bucket whose observable range holds the window (the
            // coordinator never picks which bucket is observed).
            SubjectRule::Bucketed {
                bucket_secs,
                min_delay_secs,
                ..
            } => {
                let len = u64::from(*bucket_secs);
                let last_end = window_start.checked_sub(u64::from(*min_delay_secs))?;
                let start = (last_end / len).checked_mul(len)?.checked_sub(len)?;
                let key = start.to_string();
                self.spec(&key, window_start).is_ok().then_some(key)
            }
            _ => {
                let today = i64::try_from(window_start / 86_400).ok()?;
                (0..=crate::LATEST_LOOKBACK_DAYS)
                    .map(|back| civil_date(today - back))
                    .find(|d| self.spec(d, window_start).is_ok())
            }
        }
    }
    fn fetch_limits(&self) -> onyx_fetch::Limits {
        onyx_fetch::Limits::observer(self.max_bytes())
    }
    fn key_placement(&self) -> Option<onyx_fetch::KeyPlacement> {
        crate::keyed::placement_of(&self.m.descriptor.access)
    }
    fn keyed_seat_rules(&self) -> Option<(String, Hash32)> {
        self.m
            .descriptor
            .access
            .is_keyed()
            .then(|| (self.m.id.clone(), self.terms_hash()))
    }
}

/// The JSON form of `t`'s fields a manifest holds (everything but `feed`, `metric` and
/// `fetcher`), for the review tool and for writing a manifest from an existing template.
pub fn template_to_json(t: &FeedTemplate) -> Json {
    let kind = match t.kind {
        ObservationKind::Numeric { scale, tolerance } => {
            serde_json::json!({ "numeric": { "scale": scale, "tolerance": tolerance.to_string() } })
        }
        ObservationKind::Exact { max_len } => {
            serde_json::json!({ "exact": { "max_len": max_len } })
        }
    };
    let r = &t.rights;
    let rights = serde_json::json!({
        "license": r.license,
        "redistribution": match r.redistribution {
            Redistribution::Allowed => "allowed",
            Redistribution::Restricted => "restricted",
            Redistribution::Forbidden => "forbidden",
        },
        "source": r.source.0,
        "attribution": r.attribution,
        "terms": r.terms,
    });
    let subject = match &t.subject {
        SubjectRule::Dated { prefix, lag } => serde_json::json!({ "dated": {
            "prefix": prefix,
            "lag": {
                "min_business_days": lag.min_business_days,
                "max_calendar_days": lag.max_calendar_days,
                "first_day": civil_date(lag.first_day),
                "last_day": civil_date(lag.last_day),
                "holidays": lag.holidays.iter().map(|d| civil_date(*d)).collect::<Vec<_>>(),
            }
        }}),
        SubjectRule::Daily {
            prefix,
            min_days,
            max_days,
        } => {
            serde_json::json!({ "daily": { "prefix": prefix, "min_days": min_days, "max_days": max_days } })
        }
        SubjectRule::Bucketed {
            prefix,
            bucket_secs,
            min_delay_secs,
            max_delay_secs,
        } => serde_json::json!({ "bucketed": {
            "prefix": prefix,
            "bucket_secs": bucket_secs,
            "min_delay_secs": min_delay_secs,
            "max_delay_secs": max_delay_secs,
        }}),
        SubjectRule::Fixed(_) => serde_json::json!({ "fixed": null }),
        SubjectRule::Period { .. } => serde_json::json!({ "period": null }),
        // CL7: a recheck shows the rule it rechecks and its days.
        SubjectRule::Recheck {
            of,
            after_days,
            span_days,
        } => {
            let inner = template_to_json(&FeedTemplate {
                subject: (**of).clone(),
                ..t.clone()
            });
            serde_json::json!({ "recheck": {
                "of": inner["subject"],
                "after_days": after_days,
                "span_days": span_days,
            }})
        }
    };
    serde_json::json!({
        "url_template": t.url_template,
        "selector": t.selector,
        "kind": kind,
        "window_secs": t.window_secs,
        "min_observers": t.min_observers,
        "rights": rights,
        "subject": subject,
    })
}
