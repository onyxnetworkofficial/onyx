//! The source review tool (ADR-0053 item 6, contributor loop CL3). Run on a reviewer's machine.
//!
//! `cargo run -p onyx-level2 --example onyx-source -- draft URL POINTER --out DIR [--date YYYY-MM-DD]`
//!   Fetches URL once through `onyx_fetch` (public addresses only, no redirects, 1 MiB, 20 s),
//!   saves the response as `DIR/recording/<date|draft>.json`, proposes a descriptor (the value
//!   rule from what is at POINTER; a row-pinning assert on every list element on the way, the
//!   record date as `subject_date` when `--date` is given; a key set on the row), prints it with
//!   the value it reads. The reviewer edits the asserts by hand, then writes the manifest.
//!
//! `... -- check MANIFEST [--repo DIR]`
//!   Loads the manifest with every rule (`onyx_level2::manifest::load`, the terms of
//!   `DIR/docs/ops/data-sources.md`, default the current directory), prints its ids, host and
//!   limits, and the value its recording reads (the recording's file name is its date).
//!
//! `... -- ids MANIFEST [--repo DIR]`
//!   Prints the recomputed `fetcher_id` and `template_id` of a new manifest whose stated ids
//!   are placeholders (every other rule still applies).
//!
//! `... -- recheck MANIFEST --after-days N --span-days N [--date YYYY-MM-DD] [--repo DIR]`
//!   CL7 (ADR-0057 item 2): the paired `.recheck` template of a loaded manifest
//!   (`onyx_core::recheck_of`): its metric, on-chain metric key, template id and the numeric
//!   tolerance `OnyxCorrections.pair` takes. The registry owner registers that template id for
//!   the recheck metric; a client recomputes it the same way. With `--date`, also the subject
//!   key of that record date and the first and last day a recheck window may start on.
//!
//! `... -- fetch URL`
//!   One GET exactly as an adapter source's observer makes it (`Accept: */*`, 1 MiB, 20 s):
//!   prints the size, the Keccak-256 and the first bytes.

use onyx_core::feed_json::{
    is_index, Access, Assert, Expect, JsonDescriptor, ValueRule, MAX_ASSERTS,
};
use onyx_core::ObservationKind;
use onyx_json::Node;
use onyx_level2::manifest::{load_with, ClearedHosts, ManifestSource};
use std::path::{Path, PathBuf};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("draft") => draft(&args),
        Some("check") => check(&args, true),
        Some("ids") => check(&args, false),
        Some("fetch") => fetch(&args),
        Some("recheck") => recheck(&args),
        _ => Err("usage: onyx-source draft URL POINTER --out DIR [--date YYYY-MM-DD] | check MANIFEST [--repo DIR] | ids MANIFEST [--repo DIR] | recheck MANIFEST --after-days N --span-days N [--date YYYY-MM-DD] [--repo DIR] | fetch URL".into()),
    };
    if let Err(e) = r {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// `fetch URL`: one GET exactly as an adapter source's observer fetches it
/// (`onyx_fetch::Limits::observer_any`); prints the size, the Keccak-256 and the first bytes.
fn fetch(args: &[String]) -> Result<(), String> {
    let url = args.get(1).ok_or("fetch URL")?;
    let f = onyx_fetch::HttpsFetch::new(onyx_fetch::Limits::observer_any())
        .map_err(|e| e.to_string())?;
    let body = f.get_blocking(url).map_err(|e| e.to_string())?;
    let head: String = String::from_utf8_lossy(&body).chars().take(160).collect();
    println!(
        "{} bytes, Keccak-256 {}: {head}",
        body.len(),
        onyx_core::keccak256(&body)
    );
    Ok(())
}

fn is_decimal(s: &str) -> bool {
    onyx_json::mantissa(s, 38).is_ok() || onyx_json::mantissa(s, 0).is_ok()
}

fn scale_of(s: &str) -> u8 {
    s.split_once('.')
        .map_or(0, |(_, f)| u8::try_from(f.len()).unwrap_or(u8::MAX))
}

/// Propose a descriptor for `pointer` in `doc` (see the module docs).
fn propose(doc: &Node, pointer: &str, date: Option<&str>) -> Result<(JsonDescriptor, u8), String> {
    let node = doc
        .pointer(pointer)
        .ok_or(format!("nothing at {pointer}"))?;
    let (value, scale) = match node {
        Node::Str(s) if is_decimal(s) => (ValueRule::DecimalString, scale_of(s)),
        Node::Num(t) if is_decimal(t) => (ValueRule::JsonNumber, scale_of(t)),
        Node::Arr(_) => (ValueRule::ArrayLen, 0),
        other => {
            return Err(format!(
                "{pointer} holds {other:?}: a level-2 manifest reads a number or a count"
            ))
        }
    };
    let mut asserts: Vec<Assert> = vec![];
    let toks: Vec<&str> = pointer.split('/').skip(1).collect();
    for (i, t) in toks.iter().enumerate() {
        if !is_index(t) {
            continue;
        }
        let parent = format!("/{}", toks[..i].join("/"));
        let element = format!("{parent}/{t}");
        if let Some(Node::Arr(items)) = doc.pointer(&parent) {
            if items.len() == 1 {
                asserts.push(Assert {
                    pointer: parent.clone(),
                    expect: Expect::ArrayLen(1),
                });
            }
        }
        let Some(Node::Obj(kv)) = doc.pointer(&element) else {
            return Err(format!("{element} is not an object: pin it by hand"));
        };
        let mut keys: Vec<String> = kv.iter().map(|(k, _)| k.clone()).collect();
        keys.sort();
        asserts.push(Assert {
            pointer: element.clone(),
            expect: Expect::KeysExactly(keys),
        });
        let field_ptr = |k: &str| format!("{element}/{}", k.replace('~', "~0").replace('/', "~1"));
        for (k, v) in kv {
            let p = field_ptr(k);
            if p == pointer || pointer.starts_with(&format!("{p}/")) {
                continue;
            }
            match v {
                Node::Str(s) if Some(s.as_str()) == date => asserts.push(Assert {
                    pointer: p,
                    expect: Expect::SubjectDate,
                }),
                Node::Str(s) if !is_decimal(s) && s.len() <= 256 => asserts.push(Assert {
                    pointer: p,
                    expect: Expect::Str(s.clone()),
                }),
                _ => {}
            }
        }
    }
    asserts.sort_by(|a, b| a.pointer.as_bytes().cmp(b.pointer.as_bytes()));
    asserts.dedup_by(|a, b| a.pointer == b.pointer);
    asserts.truncate(MAX_ASSERTS);
    let d = JsonDescriptor {
        version: 1,
        pointer: pointer.into(),
        value,
        unit: "UNIT".into(),
        asserts,
        max_bytes: 65_536,
        access: Access::Public,
    };
    Ok((d, scale))
}

fn draft(args: &[String]) -> Result<(), String> {
    let url = args.get(1).ok_or("draft URL POINTER --out DIR")?;
    let pointer = args.get(2).ok_or("draft URL POINTER --out DIR")?;
    let out = PathBuf::from(flag(args, "--out").ok_or("--out DIR is required")?);
    let date = flag(args, "--date");
    let target = onyx_fetch::check_template_url(url).map_err(|e| e.to_string())?;
    eprintln!("fetching {url} once (host {})", target.host);
    let f =
        onyx_fetch::HttpsFetch::new(onyx_fetch::Limits::observer(onyx_fetch::MAX_BYTES_CEILING))
            .map_err(|e| e.to_string())?;
    let body = f.get_blocking(url).map_err(|e| e.to_string())?;
    let rec = out.join("recording");
    std::fs::create_dir_all(&rec).map_err(|e| e.to_string())?;
    let file = rec.join(format!("{}.json", date.as_deref().unwrap_or("draft")));
    std::fs::write(&file, &body).map_err(|e| e.to_string())?;
    eprintln!(
        "saved {} ({} bytes, Keccak-256 {})",
        file.display(),
        body.len(),
        onyx_core::keccak256(&body)
    );
    let doc = onyx_json::parse(&body, onyx_fetch::MAX_BYTES_CEILING).map_err(|e| e.to_string())?;
    let (mut d, scale) = propose(&doc, pointer, date.as_deref())?;
    d.max_bytes = u32::try_from((body.len() * 4).clamp(4_096, onyx_fetch::MAX_BYTES_CEILING))
        .unwrap_or(65_536);
    if let Err(e) = d.validate() {
        eprintln!("the proposal needs hand edits: {e}");
    }
    let kind = ObservationKind::Numeric {
        scale,
        tolerance: 0,
    };
    let answer = onyx_json::read(&d, kind, date.as_deref(), &body);
    println!("{}", onyx_json::descriptor_to_json(&d));
    eprintln!("reads {answer:?} at scale {scale}; set the unit, review every assert, then write the manifest");
    Ok(())
}

/// `recheck MANIFEST --after-days N --span-days N`: the paired recheck template's ids.
fn recheck(args: &[String]) -> Result<(), String> {
    let path = PathBuf::from(args.get(1).ok_or("recheck MANIFEST")?);
    let repo = PathBuf::from(flag(args, "--repo").unwrap_or_else(|| ".".into()));
    let days = |name: &str| -> Result<u16, String> {
        flag(args, name)
            .ok_or(format!("{name} N"))?
            .parse()
            .map_err(|e| format!("{name}: {e}"))
    };
    let (after, span) = (days("--after-days")?, days("--span-days")?);
    let cleared = ClearedHosts::from_repo(&repo).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m = load_with(&bytes, &cleared, true).map_err(|e| e.to_string())?;
    let r = onyx_core::recheck_of(&m.template, after, span).map_err(|e| e.to_string())?;
    let ObservationKind::Numeric { tolerance, .. } = r.kind else {
        return Err("a recheck is numeric".into());
    };
    println!("original_metric      {}", m.template.metric.0);
    println!("original_template_id {}", m.template.id());
    println!(
        "original_metric_key  {}",
        onyx_core::onchain::metric_key(&m.template.metric)
    );
    println!("recheck_metric       {}", r.metric.0);
    println!("recheck_template_id  {}", r.id());
    println!(
        "recheck_metric_key   {}",
        onyx_core::onchain::metric_key(&r.metric)
    );
    println!("tolerance            {tolerance}");
    println!("after_days           {after}");
    println!("span_days            {span}");
    if let Some(date) = flag(args, "--date") {
        let prefix = match &m.template.subject {
            onyx_core::SubjectRule::Dated { prefix, .. }
            | onyx_core::SubjectRule::Daily { prefix, .. } => prefix,
            _ => return Err("a manifest subject is dated or daily".into()),
        };
        let s = onyx_core::Subject::Aggregate(format!("{prefix}:{date}"));
        let (first, last) = r
            .recheck_days(&s)
            .ok_or(format!("{date}: not a subject of this source"))?;
        println!("subject              {prefix}:{date}");
        println!(
            "subject_key          {}",
            onyx_core::onchain::subject_key(&s)
        );
        println!(
            "recheck_days         {} to {}",
            onyx_core::feed::civil_date(first),
            onyx_core::feed::civil_date(last)
        );
    }
    Ok(())
}

fn check(args: &[String], check_ids: bool) -> Result<(), String> {
    let path = PathBuf::from(args.get(1).ok_or("check MANIFEST")?);
    let repo = PathBuf::from(flag(args, "--repo").unwrap_or_else(|| ".".into()));
    let cleared = ClearedHosts::from_repo(&repo).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m = load_with(&bytes, &cleared, check_ids).map_err(|e| e.to_string())?;
    let host = match onyx_level2::keyed::placement_of(&m.descriptor.access) {
        None => onyx_fetch::check_template_url(&m.template.url_template),
        Some(p) => onyx_fetch::check_keyed_template_url(&m.template.url_template, &p),
    }
    .map_err(|e| e.to_string())?
    .host;
    println!("source      {}", m.id);
    println!("metric      {}", m.template.metric.0);
    println!("host        {host} (cleared under {})", m.terms_row);
    println!("unit        {}", m.descriptor.unit);
    println!("fetcher_id  {}", m.descriptor.fetcher_id());
    println!("template_id {}", m.template.id());
    println!(
        "limits      {} rounds/day, origin budget {}, {} s between fetches",
        m.limits.max_rounds_per_day, m.limits.origin_budget, m.limits.min_fetch_gap_secs
    );
    if !check_ids {
        return Ok(());
    }
    ManifestSource::new(m.clone()).map_err(|e| e.to_string())?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let rec = dir.join(&m.recording);
    let body = std::fs::read(&rec).map_err(|e| format!("{}: {e}", rec.display()))?;
    let date = rec
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("the recording is named by its date (or, bucketed, its bucket start)")?;
    // ADR-0058: a bucketed manifest's recording is named by its bucket's start (Unix seconds).
    let bucket = onyx_core::feed::canonical_u64(date);
    let answer = onyx_json::read_at(
        &m.descriptor,
        m.template.kind,
        bucket.is_none().then_some(date),
        bucket,
        &body,
    );
    println!("recording   {} for {date}: {answer:?}", rec.display());
    Ok(())
}
