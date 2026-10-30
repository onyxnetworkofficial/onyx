//! Aligned multi-venue feeds (ADR-0058, contributor loop CL8): a composite manifest over
//! bucketed venue manifests, the venue facts read from level-2 rounds, the answer's JSON form
//! (the four clocks, the band, the flag) and the client check.
//!
//! A composite is `composite.json`: its metric, unit, scale, bucket length, `min_venues`,
//! `disagree_ppm` and the ids of its venue manifests (each `sources/<id>/manifest.json` of the
//! same repository, loaded with every manifest rule, its terms row included: a venue whose host
//! is not cleared never loads, so no composite over it loads either; THREATS CL8-12). The pure
//! rule is [`onyx_core::aligned`]; this module only feeds it and renders it.

use crate::manifest::{ClearedHosts, ManifestSource, SOURCES_DIR};
use crate::{decimal, Source};
use onyx_core::aligned::{
    aligned, check_answer, AlignedAnswer, Composite, Reason, Status, VenueFact, VenueInput,
    VenueRef,
};
use onyx_core::feed_json::{Expect, TimeForm};
use onyx_core::{ObservationKind, Redistribution, SubjectRule};
use serde_json::{json, Map, Value as Json};
use std::path::Path;

/// The only composite version.
pub const COMPOSITE_VERSION: u64 = 1;

/// A loaded composite: its id and name, the pure [`Composite`], and its venue sources in the
/// composite's order (sorted by metric).
#[derive(Debug)]
pub struct CompositeManifest {
    /// The composite's id.
    pub id: String,
    /// The name shown.
    pub name: String,
    /// The rule's inputs.
    pub composite: Composite,
    /// The venue feeds, in `composite.venues` order.
    pub venues: Vec<ManifestSource>,
}

fn field(why: impl Into<String>) -> String {
    format!("composite: {}", why.into())
}

fn get<'a>(o: &'a Map<String, Json>, k: &str) -> Result<&'a Json, String> {
    o.get(k).ok_or_else(|| field(format!("{k} missing")))
}

fn text(o: &Map<String, Json>, k: &str) -> Result<String, String> {
    get(o, k)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| field(format!("{k}: not a string")))
}

fn whole<T: TryFrom<u64>>(o: &Map<String, Json>, k: &str) -> Result<T, String> {
    get(o, k)?
        .as_u64()
        .and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| field(format!("{k}: not a whole number in range")))
}

/// The venue reference of a loaded bucketed manifest.
pub fn venue_ref(src: &ManifestSource) -> Result<VenueRef, String> {
    let m = src.manifest();
    let SubjectRule::Bucketed { bucket_secs, .. } = &m.template.subject else {
        return Err(format!(
            "{}: a venue feed is bucketed (ADR-0058 item 3)",
            m.id
        ));
    };
    let ObservationKind::Numeric { scale, .. } = m.template.kind else {
        return Err(format!("{}: a venue feed is numeric", m.id));
    };
    let host = onyx_fetch::check_template_url(&m.template.url_template)
        .map_err(|e| format!("{}: {e}", m.id))?
        .host;
    Ok(VenueRef {
        metric: m.template.metric.0.clone(),
        venue: m.template.rights.source.0.clone(),
        host,
        terms_row: m.terms_row.clone(),
        unit: m.descriptor.unit.clone(),
        scale,
        bucket_secs: *bucket_secs,
        redistribution: m.template.rights.redistribution,
    })
}

/// Load `path` (a `composite.json`) and its venue manifests from `repo`, with the terms of
/// `repo`'s docs/ops/data-sources.md. Refused unless the file is strict JSON with no unknown
/// field, every venue manifest loads (cleared, bucketed), and the composite passes
/// [`Composite::validate`] (one unit, scale and bucket; no venue or host twice; 2..=16 venues;
/// no forbidden venue; THREATS CL8-3, CL8-10, CL8-12).
pub fn load_composite(path: &Path, repo: &Path) -> Result<CompositeManifest, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    onyx_json::parse(&bytes, 1 << 16).map_err(|e| field(e.to_string()))?;
    let v: Json = serde_json::from_slice(&bytes).map_err(|e| field(e.to_string()))?;
    let o = v.as_object().ok_or_else(|| field("not an object"))?;
    let allowed = [
        "v",
        "id",
        "metric",
        "name",
        "unit",
        "scale",
        "bucket_secs",
        "min_venues",
        "disagree_ppm",
        "venues",
    ];
    if let Some(k) = o.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(field(format!("unknown field {k}")));
    }
    if whole::<u64>(o, "v")? != COMPOSITE_VERSION {
        return Err(field("v: only version 1"));
    }
    let cleared = ClearedHosts::from_repo(repo).map_err(|e| e.to_string())?;
    let mut venues = vec![];
    for id in get(o, "venues")?
        .as_array()
        .ok_or_else(|| field("venues: not a list"))?
    {
        let id = id.as_str().ok_or_else(|| field("venues: ids"))?;
        if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
            return Err(field(format!("venues: {id:?} is not a manifest id")));
        }
        let mp = repo.join(SOURCES_DIR).join(id).join("manifest.json");
        let b = std::fs::read(&mp).map_err(|e| format!("{}: {e}", mp.display()))?;
        let m = crate::manifest::load(&b, &cleared).map_err(|e| format!("venue {id}: {e}"))?;
        if m.id != id {
            return Err(field(format!("venue {id}: the manifest's id is {}", m.id)));
        }
        venues.push(ManifestSource::new(m).map_err(|e| e.to_string())?);
    }
    venues.sort_by(|a, b| {
        a.manifest()
            .template
            .metric
            .0
            .cmp(&b.manifest().template.metric.0)
    });
    let composite = Composite {
        metric: text(o, "metric")?,
        unit: text(o, "unit")?,
        scale: whole(o, "scale")?,
        bucket_secs: whole(o, "bucket_secs")?,
        venues: venues.iter().map(venue_ref).collect::<Result<_, _>>()?,
        min_venues: whole(o, "min_venues")?,
        disagree_ppm: whole(o, "disagree_ppm")?,
    };
    composite.validate().map_err(|e| e.to_string())?;
    Ok(CompositeManifest {
        id: text(o, "id")?,
        name: text(o, "name")?,
        composite,
        venues,
    })
}

/// A venue fact's chain clock: the block of `ObservationRecorded` and its time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedAt {
    /// The block number (`OnyxRegistry.observationOf(...).recordedAt`).
    pub block: u64,
    /// That block's timestamp.
    pub time: u64,
}

/// The bucket start a venue's page states, read at the manifest's `BucketStart` assert and
/// converted to Unix seconds (the venue's own clock, ADR-0058 item 4). `None` when the page or
/// the field does not read.
pub fn venue_time(src: &ManifestSource, page: &[u8]) -> Option<u64> {
    let d = &src.manifest().descriptor;
    let a = d
        .asserts
        .iter()
        .find(|a| matches!(a.expect, Expect::BucketStart(_)))?;
    let doc = onyx_json::parse(page, d.max_bytes as usize).ok()?;
    let node = doc.pointer(&a.pointer)?;
    match (&a.expect, node) {
        (Expect::BucketStart(TimeForm::UnixSecs), onyx_json::Node::Num(t)) => {
            onyx_core::feed::canonical_u64(t)
        }
        (Expect::BucketStart(TimeForm::UnixMillis), onyx_json::Node::Num(t)) => {
            let ms = onyx_core::feed::canonical_u64(t)?;
            ms.is_multiple_of(1_000).then_some(ms / 1_000)
        }
        (Expect::BucketStart(TimeForm::Rfc3339), onyx_json::Node::Str(s)) => {
            let (date, rest) = s.split_once('T')?;
            let day = u64::try_from(onyx_core::feed::day_number(date)?).ok()?;
            let hms = rest.strip_suffix('Z')?;
            let p: Vec<u64> = hms
                .split(':')
                .map(|x| {
                    (x.len() == 2 && x.bytes().all(|c| c.is_ascii_digit()))
                        .then(|| x.parse().ok())
                        .flatten()
                })
                .collect::<Option<_>>()?;
            let [h, m, sec] = p.as_slice() else {
                return None;
            };
            let t = day * 86_400 + h * 3_600 + m * 60 + sec;
            (onyx_json::rfc3339(t).as_deref() == Some(s.as_str())).then_some(t)
        }
        _ => None,
    }
}

/// One venue's level-2 round for a bucket, read from its verified results store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VenueRound {
    /// No round of this bucket in the store.
    NotObserved,
    /// The newest round settled `SourceUnavailable` (no bucket, another bucket, no trades).
    Unavailable,
    /// The newest round settled no value.
    NoResult,
    /// A value round: the fact without its chain clock, and the arguments a client checks on
    /// chain (`OnyxRegistry.observationLevel`, then `observationOf(...).recordedAt`).
    Value {
        /// The fact (`recorded_block` and `recorded_at` are 0 until the chain says).
        fact: VenueFact,
        /// The record to find on chain.
        onchain: Box<onyx_coordinator::observe::OnchainObservationCheck>,
    },
}

/// The round of `src` for the bucket starting at `bucket_start`, from the venue's results store
/// `store` (the JSON a node serves; every round in it is checked against the manifest's
/// template at `now`, as a level-2 client does: `ObservationFeed::from_json`). `page` is the
/// venue's response body when the client has it: its Keccak-256 must be an agreeing reveal's
/// page hash, and the venue's own bucket time is read from it ([`venue_time`]); without it the
/// venue time is the bucket's start, which the manifest's `BucketStart` assert (inside the
/// fetcher id every observer ran) held for every agreeing reveal.
pub fn venue_round(
    src: &ManifestSource,
    bucket_start: u64,
    store: Option<&str>,
    page: Option<&[u8]>,
    now: u64,
) -> Result<VenueRound, String> {
    let Some(text) = store else {
        return Ok(VenueRound::NotObserved);
    };
    let t = src.template();
    let m = src.manifest();
    let bucket = src
        .bucket(&bucket_start.to_string())
        .ok_or_else(|| format!("{}: {bucket_start} is not a bucket of this venue", m.id))?;
    let pins = std::collections::BTreeMap::from([(t.metric.0.clone(), t.id())]);
    let feed = onyx_api::observations::ObservationStore::from_json(text, now, &pins)
        .map_err(|e| format!("{}: {e}", m.id))?;
    let subject = match &t.subject {
        SubjectRule::Bucketed { prefix, .. } => format!("{prefix}:{bucket_start}"),
        _ => return Err(format!("{}: not bucketed", m.id)),
    };
    if let Some(r) = feed
        .records
        .iter()
        .filter(|((s, _), _)| *s == subject)
        .map(|(_, r)| r)
        .next_back()
    {
        let onyx_core::Answer::Number(value) = r.transcript.answer else {
            return Err(format!("{}: a venue value is a number", m.id));
        };
        let agreeing: Vec<&onyx_core::Observation> = r
            .transcript
            .reveals
            .iter()
            .map(|(_, o, _, _)| o)
            .filter(|o| o.answer == r.transcript.answer)
            .collect();
        let times: Vec<u64> = agreeing.iter().map(|o| o.observed_at).collect();
        let venue_time = match page {
            Some(p) => {
                let h = onyx_core::keccak256(p);
                if !agreeing.iter().any(|o| o.page_hash == h) {
                    return Err(format!(
                        "{}: the page given is not one the agreeing observers fetched",
                        m.id
                    ));
                }
                venue_time(src, p).unwrap_or(u64::MAX)
            }
            None => bucket.start,
        };
        let spec = &r.checked.spec;
        return Ok(VenueRound::Value {
            fact: VenueFact {
                value,
                venue_time,
                window_start: spec.window_start,
                window_end: spec.window_end,
                observed_from: times.iter().copied().min().ok_or("no agreeing reveal")?,
                observed_to: times.iter().copied().max().ok_or("no agreeing reveal")?,
                recorded_block: 0,
                recorded_at: 0,
            },
            onchain: Box::new(r.checked.onchain),
        });
    }
    let newest = feed
        .attempt_fact(&subject, None)
        .map(|f| feed.attempts_of(&f))
        .unwrap_or_default()
        .into_iter()
        .max_by_key(|a| a.checked.window_end);
    Ok(match newest.map(|a| a.checked.outcome) {
        None => VenueRound::NotObserved,
        Some(onyx_coordinator::observe::ObservationOutcome::NoResult) => VenueRound::NoResult,
        Some(_) => VenueRound::Unavailable,
    })
}

/// What the chain says about a venue's fact for the bucket (`OnyxRegistry.observationOf`: for a
/// dated feed the fact key ignores the window end, so one read answers it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnChain {
    /// No result recorded for the fact.
    Nothing,
    /// A result is recorded; `level2` when `observationLevel` is 2 for exactly the store's round
    /// (its spec and result), with its block and time.
    Recorded {
        /// The store's round is the recorded one.
        level2: bool,
        /// Block and time of the record.
        at: RecordedAt,
    },
}

/// Whether the bucket starting at `bucket_start` can no longer get a round of `src` at `now`:
/// past the reveal close of a round opened at the last allowed window start.
pub fn lapsed(src: &ManifestSource, bucket_start: u64, now: u64) -> Result<bool, String> {
    let t = src.template();
    let subject = src
        .spec(&bucket_start.to_string(), first_window(src, bucket_start)?)?
        .subject;
    let last = t
        .latest_window_start(&subject, 0)
        .ok_or("not a bucket of this venue")?;
    let spec = t.instantiate(&subject, last).map_err(|e| e.to_string())?;
    Ok(now > src.observe_config(&spec).reveal_close)
}

/// The first second a window may observe the bucket starting at `bucket_start`.
pub fn first_window(src: &ManifestSource, bucket_start: u64) -> Result<u64, String> {
    let b = src
        .bucket(&bucket_start.to_string())
        .ok_or("the bucket is not on this venue's grid")?;
    match &src.manifest().template.subject {
        SubjectRule::Bucketed { min_delay_secs, .. } => b
            .end
            .checked_add(u64::from(*min_delay_secs))
            .ok_or_else(|| "bucket too late".into()),
        _ => Err("not bucketed".into()),
    }
}

/// The alignment input of a venue's round, checked against the chain (THREATS CL8-4, review
/// CL8-R1-2): a value round counts only when the chain recorded exactly it; a store that shows
/// no value (no round, or a round without a value) while the chain holds a result for the fact
/// is refused, since it hides a recorded venue; a venue with no recorded round is `NotObserved`
/// (the answer is pending) until `lapsed`, then `Lapsed`.
pub fn venue_input(round: &VenueRound, chain: OnChain, lapsed: bool) -> Result<VenueInput, String> {
    match (round, chain) {
        (VenueRound::Value { fact, .. }, OnChain::Recorded { level2: true, at }) => {
            Ok(VenueInput::Recorded(VenueFact {
                recorded_block: at.block,
                recorded_at: at.time,
                ..*fact
            }))
        }
        (VenueRound::Value { .. }, OnChain::Recorded { level2: false, .. }) => {
            Err("the chain records another result for this venue's fact".into())
        }
        (_, OnChain::Recorded { .. }) => Err(
            "the venue's store shows no value, but the chain records one: a recorded venue is hidden"
                .into(),
        ),
        (VenueRound::Unavailable, OnChain::Nothing) => Ok(VenueInput::Unavailable),
        (VenueRound::NoResult, OnChain::Nothing) => Ok(VenueInput::NoResult),
        (VenueRound::Value { .. } | VenueRound::NotObserved, OnChain::Nothing) => Ok(if lapsed {
            VenueInput::Lapsed
        } else {
            VenueInput::NotObserved
        }),
    }
}

fn rights_word(r: Redistribution) -> &'static str {
    match r {
        Redistribution::Allowed => "allowed",
        Redistribution::Restricted => "restricted",
        Redistribution::Forbidden => "forbidden",
    }
}

/// The answer's JSON form (ADR-0058 items 4 to 7; `GET /v1/aligned/{metric}?bucket_start=`):
/// the bucket and `asOf`, the value and band as decimal strings, the spread and the flag, the
/// rights, and every listed venue with its value and four clocks or its reason, each with its
/// `/v1/observations` link. No field is "now".
pub fn answer_json(cm: &CompositeManifest, a: &AlignedAnswer) -> Json {
    let venues: Vec<Json> = a
        .venues
        .iter()
        .map(|l| {
            let mut v = json!({
                "metric": l.metric,
                "venue": l.venue,
                "observations": format!("/v1/observations/{}", l.metric),
            });
            let extra = match &l.outcome {
                Ok(f) => json!({
                    "included": true,
                    "value": decimal(f.value, a.scale),
                    "venueTime": f.venue_time,
                    "observedFrom": f.observed_from,
                    "observedTo": f.observed_to,
                    "window": { "start": f.window_start, "end": f.window_end },
                    "recordedAt": { "block": f.recorded_block, "time": f.recorded_at },
                }),
                Err(r) => json!({ "included": false, "reason": r.code() }),
            };
            if let (Some(o), Json::Object(e)) = (v.as_object_mut(), extra) {
                o.extend(e);
            }
            v
        })
        .collect();
    json!({
        "metric": a.metric,
        "composite": cm.id,
        "compositeId": a.composite.to_string(),
        "name": cm.name,
        "status": match a.status {
            Status::Aligned => "aligned",
            Status::Insufficient => "insufficient",
            Status::Pending => "pending",
        },
        "bucket": { "start": a.bucket.start, "end": a.bucket.end },
        "asOf": a.as_of(),
        "value": a.value.map(|v| decimal(v, a.scale)),
        "band": a.band.map(|(lo, hi)| json!({ "min": decimal(lo, a.scale), "max": decimal(hi, a.scale) })),
        "spreadPpm": a.spread_ppm.map(|p| p.to_string()),
        "disagreement": a.disagreement,
        "disagreePpm": cm.composite.disagree_ppm,
        "minVenues": cm.composite.min_venues,
        "unit": a.unit,
        "rights": rights_word(a.rights),
        "proofLevel": 2,
        "rule": "lower median of the included venues' level-2 facts for this closed UTC bucket; band = min..max; disagreement when (max - min) * 10^6 > disagreePpm * |median| (ADR-0058)",
        "venues": venues,
        "answerId": a.id().to_string(),
    })
}

/// The aligned answer of `cm` for the bucket starting at `bucket_start` from `inputs`.
pub fn align(
    cm: &CompositeManifest,
    bucket_start: u64,
    inputs: &[(String, VenueInput)],
) -> Result<AlignedAnswer, String> {
    aligned(&cm.composite, bucket_start, inputs).map_err(|e| e.to_string())
}

/// The client check of a served answer (THREATS CL8-4): the served JSON lists exactly the
/// composite's venues and equals, field for field, the answer recomputed from `inputs` (the
/// venue facts the client verified as level-2 answers).
pub fn check_answer_json(
    cm: &CompositeManifest,
    inputs: &[(String, VenueInput)],
    served: &Json,
) -> Result<(), String> {
    let listed: Vec<&str> = cm
        .composite
        .venues
        .iter()
        .map(|v| v.metric.as_str())
        .collect();
    let shown: Vec<&str> = served["venues"]
        .as_array()
        .map(|vs| vs.iter().filter_map(|v| v["metric"].as_str()).collect())
        .unwrap_or_default();
    if listed != shown {
        return Err(format!(
            "the answer lists {shown:?}, the composite {listed:?}: a listed venue is missing or added"
        ));
    }
    let start = served["bucket"]["start"]
        .as_u64()
        .ok_or("the answer names no bucket")?;
    let want = align(cm, start, inputs)?;
    check_answer(&cm.composite, inputs, &want).map_err(|e| e.to_string())?;
    let want_json = answer_json(cm, &want);
    if &want_json != served {
        return Err("the answer differs from the one recomputed from the venue facts".into());
    }
    Ok(())
}

/// The reason code of an excluded venue in a served answer, for display.
pub fn reason_of(v: &Json) -> Option<Reason> {
    Reason::from_code(v["reason"].as_str()?)
}

/// The spec of `src` for the bucket starting at `bucket_start`, observed from `window_start`.
pub fn venue_spec(
    src: &ManifestSource,
    bucket_start: u64,
    window_start: u64,
) -> Result<onyx_core::ObservationSpec, String> {
    src.spec(&bucket_start.to_string(), window_start)
}
