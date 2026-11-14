//! `onyx-publish`: publish a large dataset on the Onyx test network
//! (contributor loop CL5, ADR-0059; step by step in docs/ops/bulk-publish.md).
//!
//! ```text
//! onyx-publish keygen   --out publisher.key                   # a fresh TEST key (mode 0600); prints its address only
//! onyx-publish address  --key publisher.key
//! onyx-publish keys-doc --key publisher.key --publisher <source id> [--out onyx-source-keys.json]
//! onyx-publish synth    --out data.csv --schema-out schema.json --mib 100   # clearly synthetic test rows
//! onyx-publish build    --schema schema.json --input data.csv --dataset <id> --publisher <source id>
//!                       --license CC0-1.0 [--attribution TEXT] --out DIR [--prev DIR [--widen]] [--shard-mib 8]
//! onyx-publish schema-fit --prev DIR --input data.csv --out new.json   # one widening for new rows
//! onyx-publish sign     --key publisher.key --dir DIR
//! onyx-publish anchor   --dir DIR --rpc URL --chain-id N --book 0x… --gas-key FILE
//! onyx-publish upload   --dir DIR --node URL [--wait-secs N] [--catalog N]
//!
//! # One anchor for many manifests (ADR-0059 amendment 1):
//! onyx-publish catalog-build  --publisher <source id> --dirs LIST --out FILE [--prev FILE]
//!                             [--remove DATASET[,DATASET...]]
//! onyx-publish catalog-sign   --key publisher.key --catalog FILE
//! onyx-publish catalog-anchor --catalog FILE --rpc URL --chain-id N --book 0x… --gas-key FILE
//! onyx-publish catalog-upload --catalog FILE --node URL [--wait-secs N]
//! ```
//!
//! `catalog-build` lists the manifest of every version folder in LIST (one path a
//! line) and carries over every other dataset of `--prev`, the catalog it
//! follows; `upload --catalog N` then hands a node each manifest without its
//! own report. The catalog's report goes next to it as `FILE.report`.
//! `--remove` ends each named dataset with a tombstone (ADR-0059 amendment 4):
//! nodes stop serving it, and a later folder of it in LIST is skipped.
//!
//! A later version keeps the previous version's schema unless `build --widen`
//! widens it (ADR-0059 amendment 5): a decimal's scale grows, a non-key int
//! becomes decimal, nullable columns are added at the end; earlier shards keep
//! theirs. `schema-fit` writes the one widening new rows need, with room so it
//! lasts (a dataset has at most 8 schemas). `upload` steps back to the
//! previous version's folder when a node lacks it ("versions are taken in
//! order").
//!
//! Keys are read from files that only their owner can read and are never
//! printed. Exit 0 done, 1 refused or failed, 2 usage.

use borsh::BorshDeserialize;
use http_body_util::BodyExt;
use onyx_api::evm_tx::Sender;
use onyx_api::roots::{HttpJsonRpc, JsonRpc};
use onyx_api::x402::{check_facilitator_url, HyperPost};
use onyx_bulk::input::{Format, Records};
use onyx_bulk::manifest::{Manifest, NewVersion, ShardEntry};
use onyx_bulk::row::encode_row;
use onyx_bulk::shard::{check_shard, encode_shard, ShardBuilder, MAX_SHARD_BYTES};
use onyx_bulk::Schema;
use onyx_core::{keccak256, DataRights, Hash32, Redistribution, SignedReport, SourceId};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const USAGE: &str =
    "usage: onyx-publish <keygen|address|keys-doc|synth|build|schema-fit|sign|anchor|upload|catalog-build|catalog-sign|catalog-anchor|catalog-upload> [options]\n\
see the header of crates/onyx-publish/src/main.rs or docs/ops/bulk-publish.md";

type Res<T> = Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let opts = match parse_opts(rest) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("onyx-publish: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("onyx-publish: runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let r = match cmd.as_str() {
        "keygen" => keygen(&opts),
        "address" => address(&opts),
        "keys-doc" => keys_doc(&opts),
        "synth" => synth(&opts),
        "build" => build_cmd(&opts),
        "schema-fit" => schema_fit_cmd(&opts),
        "sign" => sign_cmd(&opts),
        "anchor" => rt.block_on(anchor_cmd(&opts)),
        "upload" => rt.block_on(upload_cmd(&opts)),
        "catalog-build" => catalog_build_cmd(&opts),
        "catalog-sign" => catalog_sign_cmd(&opts),
        "catalog-anchor" => rt.block_on(catalog_anchor_cmd(&opts)),
        "catalog-upload" => rt.block_on(catalog_upload_cmd(&opts)),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command {other}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("onyx-publish {cmd}: {e}");
            ExitCode::from(1)
        }
    }
}

struct Opts(BTreeMap<String, String>);

/// Options that take no value.
const FLAGS: &[&str] = &["widen"];
/// What a flag stores in [`Opts`] (it is given, with no value).
static FLAG_ON: String = String::new();

fn parse_opts(a: &[String]) -> Res<Opts> {
    let mut m = BTreeMap::new();
    let mut it = a.iter();
    while let Some(k) = it.next() {
        let name = k
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected {k}"))?;
        let v = if FLAGS.contains(&name) {
            &FLAG_ON
        } else {
            it.next().ok_or_else(|| format!("{k} needs a value"))?
        };
        if m.insert(name.to_string(), v.clone()).is_some() {
            return Err(format!("{k} given twice"));
        }
    }
    Ok(Opts(m))
}

impl Opts {
    fn flag(&self, k: &str) -> bool {
        self.0.contains_key(k)
    }
    fn req(&self, k: &str) -> Res<&str> {
        self.0
            .get(k)
            .map(String::as_str)
            .ok_or_else(|| format!("--{k} is required"))
    }
    fn opt(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }
    fn only(&self, allowed: &[&str]) -> Res<()> {
        match self.0.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => Err(format!("--{k} is not an option of this command")),
            None => Ok(()),
        }
    }
    fn num(&self, k: &str, default: u64) -> Res<u64> {
        self.opt(k).map_or(Ok(default), |v| {
            v.parse().map_err(|_| format!("--{k}: not a whole number"))
        })
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

// ------------------------------------------------------------------ keys

/// Read a 32-byte hex key from a file only its owner can read. The error
/// never holds the file's content.
fn read_key(path: &str) -> Res<[u8; 32]> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("key file {path}: {e}"))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "key file {path} must be readable by its owner only (chmod 600)"
            ));
        }
    }
    let t = std::fs::read_to_string(path).map_err(|e| format!("key file {path}: {e}"))?;
    let t = t.trim();
    let b = hex::decode(t.strip_prefix("0x").unwrap_or(t))
        .ok()
        .filter(|b| b.len() == 32)
        .ok_or_else(|| format!("key file {path} must hold 64 hex digits"))?;
    let mut k = [0u8; 32];
    k.copy_from_slice(&b);
    onyx_bulk::anchor::address_of(&k).map_err(|_| format!("key file {path} holds no valid key"))?;
    Ok(k)
}

fn keygen(o: &Opts) -> Res<()> {
    o.only(&["out"])?;
    let out = o.req("out")?;
    let mut k = [0u8; 32];
    loop {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut k))
            .map_err(|e| format!("randomness: {e}"))?;
        if onyx_bulk::anchor::address_of(&k).is_ok() {
            break;
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(out)
        .map_err(|e| format!("{out}: {e} (an existing key is never overwritten)"))?;
    f.write_all(format!("0x{}\n", hex::encode(k)).as_bytes())
        .map_err(|e| format!("{out}: {e}"))?;
    let a = onyx_bulk::anchor::address_of(&k).map_err(|e| e.to_string())?;
    println!("new test key in {out} (mode 0600); address {}", hx(&a));
    println!("testnet only: never put real funds on it");
    Ok(())
}

fn address(o: &Opts) -> Res<()> {
    o.only(&["key"])?;
    let k = read_key(o.req("key")?)?;
    println!(
        "{}",
        hx(&onyx_bulk::anchor::address_of(&k).map_err(|e| e.to_string())?)
    );
    Ok(())
}

/// The keys document a publisher serves at
/// `https://<its domain>/.well-known/onyx-source-keys.json` (ADR-0028).
fn keys_document(publisher: &str, signer: &[u8; 20]) -> String {
    format!(
        "{}\n",
        json!({"version": 1, "source": publisher, "keys": [{"signer": hx(signer), "scheme": "eip191"}]})
    )
}

fn keys_doc(o: &Opts) -> Res<()> {
    o.only(&["key", "publisher", "out"])?;
    let k = read_key(o.req("key")?)?;
    let publisher = o.req("publisher")?;
    if !onyx_core::ids::is_dotted_id(publisher) {
        return Err("--publisher must be a dotted id (lowercase, e.g. metrics.example)".into());
    }
    let signer = onyx_bulk::anchor::address_of(&k).map_err(|e| e.to_string())?;
    let doc = keys_document(publisher, &signer);
    if let Some(out) = o.opt("out") {
        std::fs::write(out, &doc).map_err(|e| format!("{out}: {e}"))?;
        println!("wrote {out}");
    } else {
        print!("{doc}");
    }
    eprintln!(
        "keys document keccak (the reviewer's provenance hash): {}",
        keccak256(doc.as_bytes())
    );
    Ok(())
}

// ------------------------------------------------------------------ synthetic data

/// The schema of [`synth`]'s rows.
fn synth_schema() -> Value {
    json!({"columns": [
        {"name": "t", "kind": "int"},
        {"name": "series", "kind": "text"},
        {"name": "value", "kind": "decimal", "scale": 8},
        {"name": "count", "kind": "int"}
    ], "key": "t"})
}

/// Write at least `mib` MiB of clearly synthetic CSV rows (a seeded
/// xorshift; series named `synthetic-N`); returns (rows, bytes).
fn write_synth(out: &mut impl Write, mib: u64, start: u64) -> Res<(u64, u64)> {
    let target = mib << 20;
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut bytes = 0u64;
    let mut rows = 0u64;
    let head = "t,series,value,count\n";
    out.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    bytes += head.len() as u64;
    while bytes < target {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let line = format!(
            "{},synthetic-{},{}.{:08},{}\n",
            start + rows * 12,
            rows % 5,
            x % 100_000,
            (x >> 20) % 100_000_000,
            (x >> 40) % 10_000
        );
        out.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        bytes += line.len() as u64;
        rows += 1;
    }
    Ok((rows, bytes))
}

fn synth(o: &Opts) -> Res<()> {
    o.only(&["out", "schema-out", "mib", "start"])?;
    let out = o.req("out")?;
    let mib = o.num("mib", 100)?;
    if mib == 0 || mib > 100_000 {
        return Err("--mib must be 1..=100000".into());
    }
    let start = o.num("start", 1_500_000_000)?;
    let f = std::fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
    let mut w = BufWriter::new(f);
    let t = Instant::now();
    let (rows, bytes) = write_synth(&mut w, mib, start)?;
    w.flush().map_err(|e| e.to_string())?;
    if let Some(s) = o.opt("schema-out") {
        std::fs::write(s, format!("{:#}\n", synth_schema())).map_err(|e| format!("{s}: {e}"))?;
    }
    println!(
        "SYNTH rows={rows} bytes={bytes} secs={:.1} (synthetic test data only)",
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

// ------------------------------------------------------------------ build

/// What a build produced.
struct Built {
    manifest: Manifest,
    new_shards: Vec<(ShardEntry, PathBuf)>,
    input_rows: u64,
}

fn shard_path(dir: &Path, root: &Hash32) -> PathBuf {
    dir.join("shards").join(format!("{}.bin", root.to_hex()))
}

fn write_shard(
    dir: &Path,
    schema: &Schema,
    rows: &[Vec<u8>],
    index: u32,
) -> Res<(ShardEntry, PathBuf)> {
    let file = encode_shard(rows);
    let c = check_shard(schema, &file).map_err(|e| format!("shard {index}: {e}"))?;
    let e = ShardEntry::new(index, &c.summary);
    let p = shard_path(dir, &e.root);
    std::fs::write(&p, &file).map_err(|err| format!("{}: {err}", p.display()))?;
    Ok((e, p))
}

/// Read `input`, check every record against `schema`, cut shards of about
/// `target` bytes into `dir/shards`, and build the version over them (after
/// `prev`'s shards, if any).
fn build(
    schema: &Schema,
    input: impl BufRead,
    format: Format,
    target: u64,
    prev: Option<&Manifest>,
    v: NewVersion,
    dir: &Path,
) -> Res<Built> {
    std::fs::create_dir_all(dir.join("shards")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut records = Records::new(input, schema.clone(), format)
        .map_err(|e| fit_hint(e.to_string(), prev.is_some()))?;
    let mut b = ShardBuilder::new(target);
    let mut entries: Vec<ShardEntry> = prev.map(|p| p.shards.clone()).unwrap_or_default();
    let mut new_shards = Vec::new();
    let mut rows = 0u64;
    let floor = prev.map(|p| p.header.key_max);
    while let Some((line, cells)) = records
        .next_record()
        .map_err(|e| fit_hint(e.to_string(), prev.is_some()))?
    {
        let row = encode_row(schema, &cells).map_err(|e| format!("line {line}: {e}"))?;
        let key =
            onyx_bulk::row::key_of(schema, &cells).ok_or_else(|| format!("line {line}: no key"))?;
        if floor.is_some_and(|f| key < f) {
            return Err(format!(
                "line {line}: key {key} is below the previous version's last key: a new version only adds rows at or after it"
            ));
        }
        if let Some(full) = b
            .push(row, key)
            .map_err(|e| format!("line {line}: {e} (sort the input by the key column)"))?
        {
            let i = entries.len() as u32;
            let (e, p) = write_shard(dir, schema, &full, i)?;
            entries.push(e);
            new_shards.push((e, p));
        }
        rows += 1;
    }
    if let Some(last) = b.finish() {
        let i = entries.len() as u32;
        let (e, p) = write_shard(dir, schema, &last, i)?;
        entries.push(e);
        new_shards.push((e, p));
    }
    if new_shards.is_empty() {
        return Err("the input holds no rows".into());
    }
    let manifest = Manifest::build(v, prev, entries).map_err(|e| e.to_string())?;
    Ok(Built {
        manifest,
        new_shards,
        input_rows: rows,
    })
}

/// A successor's input that does not fit the schema because it needs more
/// decimal places or has another column: how to widen (ADR-0059 amendment 5
/// item 7). The error already names the line, the column and the places.
fn fit_hint(e: String, successor: bool) -> String {
    let widen = [
        "decimal places",
        "header has",
        "header must name",
        "unknown column",
    ];
    if successor && widen.iter().any(|w| e.contains(w)) {
        format!(
            "{e}; to widen the schema for the new rows: onyx-publish schema-fit --prev DIR --input FILE --out new.json, then build --schema new.json --widen (ADR-0059 amendment 5)"
        )
    } else {
        e
    }
}

/// The first column where `new` differs from `old`, in words.
fn first_difference(old: &Schema, new: &Schema) -> String {
    use onyx_bulk::schema::kind_text;
    let n = old.columns.len().max(new.columns.len());
    for i in 0..n {
        match (old.columns.get(i), new.columns.get(i)) {
            (Some(o), Some(c)) if o != c => {
                return format!(
                    "column {}: {}{} before, column {}: {}{} now",
                    o.name,
                    kind_text(o.kind),
                    if o.nullable { " nullable" } else { "" },
                    c.name,
                    kind_text(c.kind),
                    if c.nullable { " nullable" } else { "" }
                )
            }
            (None, Some(c)) => return format!("column {} added", c.name),
            (Some(o), None) => return format!("column {} dropped", o.name),
            _ => {}
        }
    }
    "the key column differs".into()
}

fn read_manifest(dir: &Path) -> Res<Manifest> {
    let p = dir.join("manifest.bin");
    let b = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Manifest::from_bytes(&b).map_err(|e| format!("{}: {e}", p.display()))
}

fn build_cmd(o: &Opts) -> Res<()> {
    o.only(&[
        "schema",
        "input",
        "dataset",
        "publisher",
        "license",
        "attribution",
        "out",
        "prev",
        "widen",
        "created",
        "shard-mib",
    ])?;
    let schema_text =
        std::fs::read_to_string(o.req("schema")?).map_err(|e| format!("--schema: {e}"))?;
    let schema_json: Value =
        serde_json::from_str(&schema_text).map_err(|e| format!("--schema: {e}"))?;
    let schema = Schema::from_json(&schema_json).map_err(|e| e.to_string())?;
    let input = o.req("input")?;
    let format = Format::from_path(input).ok_or("--input must end in .csv, .jsonl or .ndjson")?;
    let publisher = SourceId(o.req("publisher")?.to_string());
    let rights = DataRights {
        license: o.req("license")?.to_string(),
        redistribution: Redistribution::Allowed,
        source: publisher.clone(),
        attribution: o.opt("attribution").map(str::to_string),
        terms: String::new(),
    };
    let target = o.num("shard-mib", 8)? << 20;
    if target == 0 || target > MAX_SHARD_BYTES {
        return Err("--shard-mib must be 1..=64".into());
    }
    let prev = o
        .opt("prev")
        .map(|p| read_manifest(Path::new(p)))
        .transpose()?;
    // ADR-0059 amendment 5 item 7: a later version keeps the schema unless
    // `--widen` widens it on purpose.
    let widen = o.flag("widen");
    match &prev {
        None if widen => return Err("--widen needs --prev (version 1 has nothing to widen)".into()),
        Some(p) if p.header.schema != schema && !widen => {
            return Err(format!(
                "the schema differs from version {}'s ({}): a later version keeps it unless --widen widens it (onyx-publish schema-fit writes a widening that fits the new rows; ADR-0059 amendment 5)",
                p.header.version,
                first_difference(&p.header.schema, &schema)
            ))
        }
        Some(p) if p.header.schema != schema => {
            Schema::widens(&p.header.schema, &schema).map_err(|e| {
                format!(
                    "--widen: the schema does not widen version {}'s: {e}",
                    p.header.version
                )
            })?;
        }
        _ => {}
    }
    let created = o.num("created", now_unix())?;
    let out = PathBuf::from(o.req("out")?);
    if out.join("manifest.bin").exists() {
        return Err(format!(
            "{} already holds a manifest; use a new folder per version",
            out.display()
        ));
    }
    let f = std::fs::File::open(input).map_err(|e| format!("{input}: {e}"))?;
    let t = Instant::now();
    let built = build(
        &schema,
        BufReader::with_capacity(1 << 20, f),
        format,
        target,
        prev.as_ref(),
        NewVersion {
            dataset: o.req("dataset")?.to_string(),
            publisher,
            created_unix: created,
            schema: schema.clone(),
            rights,
        },
        &out,
    )?;
    let m = &built.manifest;
    std::fs::write(out.join("manifest.bin"), m.to_bytes()).map_err(|e| e.to_string())?;
    let summary = json!({
        "dataset": m.header.dataset,
        "publisher": m.header.publisher.0,
        "version": m.header.version,
        "root": m.header.root().to_string(),
        "rows": m.header.row_count,
        "bytes": m.header.byte_count,
        "shards": m.header.shard_count,
        "new_shards": built.new_shards.len(),
        "key_min": m.header.key_min.to_string(),
        "key_max": m.header.key_max.to_string(),
        "license": m.header.rights.license,
        "format": m.header.format,
        "schemas": m.header.schemas().len(),
    });
    let mut summary = summary;
    // The folder of the previous version: `upload` steps back to it when a
    // node lacks that version (ADR-0059 amendment 5 item 8).
    if let Some(p) = o.opt("prev") {
        let abs = std::fs::canonicalize(p).map_err(|e| format!("--prev: {e}"))?;
        summary["prev_dir"] = json!(abs.display().to_string());
    }
    std::fs::write(out.join("manifest.json"), format!("{summary:#}\n"))
        .map_err(|e| e.to_string())?;
    println!(
        "BUILT version={} format={} rows={} new_rows={} shards={} new_shards={} bytes={} secs={:.1} root={}",
        m.header.version,
        m.header.format,
        m.header.row_count,
        built.input_rows,
        m.header.shard_count,
        built.new_shards.len(),
        m.header.byte_count,
        t.elapsed().as_secs_f64(),
        m.header.root()
    );
    Ok(())
}

// ------------------------------------------------------------------ schema-fit

/// Significant digits of a binary64 value written in full: the places a
/// decimal column needs to hold its smallest value's digits.
const FLOAT_DIGITS: i64 = 17;
/// Places added above that, so one widening lasts (ADR-0059 amendment 5
/// item 7, review BULK-WIDEN-R1 finding 4).
const FIT_HEADROOM: i64 = 2;
/// Lines `schema-fit` names per kind of misfit (the rest are counted).
const UNFIT_SHOWN: usize = 20;

/// Some lines, the first [`UNFIT_SHOWN`] kept and all counted.
#[derive(Default)]
struct Lines {
    first: Vec<u64>,
    count: u64,
}

impl Lines {
    fn push(&mut self, line: u64) {
        self.count += 1;
        if self.first.len() < UNFIT_SHOWN {
            self.first.push(line);
        }
    }
}

/// Values no widening can take: the first lines of each kind with why
/// (line 0: a whole column), and how many in all.
#[derive(Default)]
struct Unfit {
    named: Vec<(u64, String)>,
    count: u64,
}

impl Unfit {
    fn lines(&mut self, l: &Lines, why: String) {
        self.count += l.count;
        self.named.extend(l.first.iter().map(|x| (*x, why.clone())));
    }
    fn one(&mut self, line: u64, why: String) {
        self.count += 1;
        self.named.push((line, why));
    }
}

/// One input field: its text, and whether it came as a JSON number.
type Field = Option<(String, bool)>;

/// What one input column's values need.
#[derive(Default)]
struct Need {
    /// Non-empty values.
    values: u64,
    /// Values that are not whole numbers.
    not_int: Lines,
    /// Values that are not decimals (`-?digits[.digits]`).
    not_number: Lines,
    /// Values given as JSON numbers: whole, and with a fraction.
    json_ints: Lines,
    json_fractions: Lines,
    /// Empty fields.
    empty: Lines,
    /// Values longer than a stored decimal may be (80 characters).
    too_long: Lines,
    /// Most fraction places a value needs (trailing zeros dropped).
    places: usize,
    /// Exponent of the first significant digit of the smallest non-zero
    /// value (`0.00012` is -4, `711.5` is 2).
    min_exp: Option<i64>,
    /// Values whose integer part (with its sign) may pass 80 characters at
    /// some scale: (line, length); a few at most in any real input.
    long: Vec<(u64, usize)>,
    /// The longest integer part (with its sign) of a value.
    max_int: usize,
}

impl Need {
    fn add(&mut self, line: u64, f: &Field) {
        let Some((v, json)) = f else {
            self.empty.push(line);
            return;
        };
        self.values += 1;
        if v.len() > onyx_bulk::row::MAX_DECIMAL_CHARS {
            self.too_long.push(line);
        }
        let whole = onyx_bulk::row::parse_int(v).is_ok();
        if !whole {
            self.not_int.push(line);
        }
        if *json {
            if whole {
                self.json_ints.push(line);
            } else {
                self.json_fractions.push(line);
            }
        }
        let body = v.strip_prefix('-').unwrap_or(v);
        let (int, frac) = body.split_once('.').unwrap_or((body, ""));
        let digits = |x: &str| x.bytes().all(|c| c.is_ascii_digit());
        if int.is_empty()
            || !digits(int)
            || !digits(frac)
            || (body.contains('.') && frac.is_empty())
        {
            self.not_number.push(line);
            return;
        }
        let frac = frac.trim_end_matches('0');
        let int = int.trim_start_matches('0');
        self.places = self.places.max(frac.len());
        let exp = if !int.is_empty() {
            Some(int.len() as i64 - 1)
        } else {
            frac.bytes()
                .position(|c| c != b'0')
                .map(|p| -(p as i64) - 1)
        };
        if let Some(e) = exp {
            self.min_exp = Some(self.min_exp.map_or(e, |m| m.min(e)));
        }
        let int_len = int.len().max(1) + usize::from(v.starts_with('-'));
        self.max_int = self.max_int.max(int_len);
        if int_len + 1 + usize::from(MAX_DECIMAL_SCALE) > onyx_bulk::row::MAX_DECIMAL_CHARS
            && self.long.len() < 10_000
        {
            self.long.push((line, int_len));
        }
    }

    /// The scale meant to last: the places 17 significant digits need at the
    /// smallest non-zero magnitude, plus 2, but no more than lets the
    /// longest integer part stay within 80 characters (review BULK-WIDEN-R2
    /// finding 4), and at least the places the values need now; 1 to 38.
    fn fit_scale(&self) -> u8 {
        let float = self
            .min_exp
            .map_or(0, |e| (FLOAT_DIGITS - 1 - e).max(0) + FIT_HEADROOM);
        let room = onyx_bulk::row::MAX_DECIMAL_CHARS.saturating_sub(self.max_int + 1) as i64;
        let want = float
            .min(room)
            .max(self.places as i64)
            .clamp(1, i64::from(MAX_DECIMAL_SCALE));
        u8::try_from(want).unwrap_or(MAX_DECIMAL_SCALE)
    }

    /// Name the values of a column of kind decimal at `scale` that `build`
    /// would still refuse: written longer than 80 characters, given as JSON
    /// numbers, or past 80 characters at that scale.
    fn decimal_misfits(&self, name: &str, scale: u8, unfit: &mut Unfit) {
        unfit.lines(
            &self.too_long,
            format!("column {name}: longer than 80 characters"),
        );
        let why = format!("column {name}: a JSON number: write decimals as strings");
        unfit.lines(&self.json_ints, why.clone());
        unfit.lines(&self.json_fractions, why);
        for (l, len) in &self.long {
            if len + 1 + usize::from(scale) > onyx_bulk::row::MAX_DECIMAL_CHARS {
                unfit.one(
                    *l,
                    format!("column {name}: past 80 characters at {scale} places"),
                );
            }
        }
    }
}

use onyx_bulk::schema::{kind_text, Column, ColumnKind, MAX_DECIMAL_SCALE};

/// Read every record of `path`, passing each one's fields by name index to
/// `f`; returns the column names. CSV goes through the build's own reader
/// (quoting and line ends read exactly as `build` reads them; the header
/// gives the names); JSON Lines object by object, starting from `known` (a
/// name absent from a record is an empty field).
fn each_record(
    path: &str,
    format: Format,
    known: &[String],
    f: &mut dyn FnMut(usize, u64, &[Field]),
) -> Res<Vec<String>> {
    let open = || std::fs::File::open(path).map_err(|e| format!("{path}: {e}"));
    match format {
        Format::Csv => {
            let mut head = String::new();
            BufReader::new(open()?)
                .read_line(&mut head)
                .map_err(|e| format!("{path}: {e}"))?;
            let names = onyx_bulk::input::csv_header_names(&head)
                .map_err(|e| format!("{path}: line 1: {e}"))?;
            // Every column as nullable text: each field is kept as written
            // (an empty one as null) and nothing else is checked.
            let text = Schema {
                columns: names
                    .iter()
                    .map(|n| Column {
                        name: n.clone(),
                        kind: ColumnKind::Text,
                        nullable: true,
                    })
                    .collect(),
                key: 0,
            };
            let mut r = Records::new(
                BufReader::with_capacity(1 << 20, open()?),
                text,
                Format::Csv,
            )
            .map_err(|e| e.to_string())?;
            while let Some((line, cells)) = r.next_record().map_err(|e| e.to_string())? {
                let fields: Vec<Field> = cells
                    .into_iter()
                    .map(|c| match c {
                        onyx_bulk::Cell::Text(t) => Some((t, false)),
                        _ => None,
                    })
                    .collect();
                f(names.len(), line, &fields);
            }
            Ok(names)
        }
        Format::JsonLines => {
            let mut names: Vec<String> = known.to_vec();
            for (i, l) in BufReader::with_capacity(1 << 20, open()?)
                .lines()
                .enumerate()
            {
                let line = i as u64 + 1;
                let l = l.map_err(|e| format!("{path}: line {line}: {e}"))?;
                if l.trim().is_empty() {
                    continue;
                }
                let v: Value =
                    serde_json::from_str(&l).map_err(|e| format!("line {line}: not JSON: {e}"))?;
                let obj = v
                    .as_object()
                    .ok_or_else(|| format!("line {line}: not a JSON object"))?;
                let mut fields: Vec<Field> = vec![None; names.len()];
                for (k, x) in obj {
                    let i = names.iter().position(|n| n == k).unwrap_or_else(|| {
                        names.push(k.clone());
                        fields.push(None);
                        names.len() - 1
                    });
                    fields[i] = match x {
                        Value::Null => None,
                        Value::String(s) => Some((s.clone(), false)),
                        Value::Number(n) => Some((n.to_string(), true)),
                        _ => {
                            return Err(format!("line {line}: column {k}: not a string or number"))
                        }
                    };
                }
                f(names.len(), line, &fields);
            }
            Ok(names)
        }
    }
}

/// `schema-fit --prev DIR --input FILE --out new.json`: the one widening of
/// version `DIR`'s schema the rows of FILE need (ADR-0059 amendment 5 item
/// 7). A decimal column whose values need more places than its scale, and a
/// non-key int column holding fractions, get [`Need::fit_scale`]; a column of
/// FILE the schema lacks is added as nullable (int, decimal at the fit
/// scale, or text, by its values); every other column stays as it is, so it
/// is the narrowest widening that takes the rows and lasts. Values no
/// widening can take (more than 38 places, past 80 characters, not a number,
/// a JSON number where `build` wants a string, empty where the column is not
/// nullable) are named with their lines; the schema is written for the rest
/// and the command then fails (exit 1).
fn schema_fit_cmd(o: &Opts) -> Res<()> {
    o.only(&["prev", "input", "out"])?;
    let prev = read_manifest(Path::new(o.req("prev")?))?;
    let old = &prev.header.schema;
    let input = o.req("input")?;
    let format = Format::from_path(input).ok_or("--input must end in .csv, .jsonl or .ndjson")?;
    let out = o.req("out")?;
    let known: Vec<String> = old.columns.iter().map(|c| c.name.clone()).collect();
    let mut needs: Vec<Need> = Vec::new();
    let mut rows = 0u64;
    let names = each_record(input, format, &known, &mut |n, line, fields| {
        if needs.len() < n {
            needs.resize_with(n, Need::default);
        }
        for (need, f) in needs.iter_mut().zip(fields) {
            need.add(line, f);
        }
        rows += 1;
    })?;
    needs.resize_with(names.len(), Need::default);
    if let Some(c) = old.columns.iter().find(|c| !names.contains(&c.name)) {
        return Err(format!(
            "the input has no column {}: no widening drops a column (a new dataset id does)",
            c.name
        ));
    }
    let mut unfit = Unfit::default();
    let mut changed: Vec<String> = Vec::new();
    let mut columns = old.columns.clone();
    let key = usize::from(old.key);
    for (i, c) in columns.iter_mut().enumerate() {
        let p = names.iter().position(|n| *n == c.name).unwrap_or_default();
        let n = &needs[p];
        let before = c.kind;
        if !c.nullable && c.kind != ColumnKind::Text {
            unfit.lines(
                &n.empty,
                format!(
                    "column {}: empty, and the column is not nullable (nullability never changes)",
                    c.name
                ),
            );
        }
        if c.kind == ColumnKind::Text {
            unfit.lines(
                &n.json_ints,
                format!(
                    "column {}: a JSON number in a text column: write it as a string",
                    c.name
                ),
            );
            unfit.lines(
                &n.json_fractions,
                format!(
                    "column {}: a JSON number in a text column: write it as a string",
                    c.name
                ),
            );
            continue;
        }
        unfit.lines(&n.not_number, format!("column {}: not a number", c.name));
        if n.not_int.count > 0 && i == key {
            unfit.lines(
                &n.not_int,
                format!("column {}: not a whole number (the key stays int)", c.name),
            );
            continue;
        }
        let scale = match c.kind {
            ColumnKind::Decimal { scale } => scale,
            _ => 0,
        };
        // Values with a point but no fraction places ("5.0"): an int column
        // that holds them becomes decimal too.
        let fractions = n.not_int.count > n.not_number.count;
        if n.places > usize::from(MAX_DECIMAL_SCALE) {
            unfit.one(
                0,
                format!(
                    "column {}: a value needs {} decimal places, more than 38",
                    c.name, n.places
                ),
            );
        } else if n.places > usize::from(scale) || (c.kind == ColumnKind::Int && fractions) {
            c.kind = ColumnKind::Decimal {
                scale: n.fit_scale(),
            };
        }
        match c.kind {
            ColumnKind::Decimal { scale } => n.decimal_misfits(&c.name, scale, &mut unfit),
            _ => unfit.lines(
                &n.json_fractions,
                format!("column {}: a JSON number with a fraction", c.name),
            ),
        }
        if c.kind != before {
            changed.push(format!(
                "WIDEN {}: {} -> {} (its values need {} places)",
                c.name,
                kind_text(before),
                kind_text(c.kind),
                n.places
            ));
        }
    }
    for (p, name) in names.iter().enumerate() {
        if old.index_of(name).is_some() {
            continue;
        }
        if !onyx_bulk::schema::is_column_name(name) {
            return Err(format!(
                "the input's column {name:?} is not a column name ([a-z][a-z0-9_]*)"
            ));
        }
        let n = &needs[p];
        let numbers = n.values > 0 && n.not_number.count == 0;
        let kind = if numbers && n.not_int.count == 0 {
            ColumnKind::Int
        } else if numbers
            && n.places <= usize::from(MAX_DECIMAL_SCALE)
            && n.json_ints.count + n.json_fractions.count == 0
        {
            ColumnKind::Decimal {
                scale: n.fit_scale(),
            }
        } else {
            ColumnKind::Text
        };
        if n.values == 0 {
            eprintln!("note: column {name} holds no value: added as nullable text (edit {out} if it will hold numbers)");
        }
        match kind {
            ColumnKind::Text => {
                let why =
                    format!("column {name}: a JSON number in a text column: write it as a string");
                unfit.lines(&n.json_ints, why.clone());
                unfit.lines(&n.json_fractions, why);
            }
            // The same checks as a widened column (review BULK-WIDEN-R2
            // finding 4).
            ColumnKind::Decimal { scale } => n.decimal_misfits(name, scale, &mut unfit),
            ColumnKind::Int => {}
        }
        changed.push(format!("ADD {name}: {} (nullable)", kind_text(kind)));
        columns.push(Column {
            name: name.clone(),
            kind,
            nullable: true,
        });
    }
    let new = Schema {
        columns,
        key: old.key,
    };
    if new != *old {
        Schema::widens(old, &new).map_err(|e| format!("no widening takes this input: {e}"))?;
    }
    std::fs::write(out, format!("{:#}\n", new.to_json())).map_err(|e| format!("{out}: {e}"))?;
    for c in &changed {
        println!("{c}");
    }
    let count = unfit.count;
    let mut unfit = unfit.named;
    unfit.sort();
    for (l, why) in &unfit {
        match l {
            0 => println!("UNFIT {why}"),
            l => println!("UNFIT line {l}: {why}"),
        }
    }
    let schemas = prev.header.schemas().len() + usize::from(new != *old);
    println!(
        "FIT version={} rows={rows} changes={} schemas={schemas} (at most {}) unfit={count} out={out}",
        prev.header.version,
        changed.len(),
        onyx_bulk::manifest::MAX_EARLIER + 1,
    );
    if new == *old {
        println!(
            "the rows fit version {}'s schema: no widening needed",
            prev.header.version
        );
    }
    if count > 0 {
        return Err(format!(
            "{count} value(s) no widening can take (the first named above); {out} holds the widening for the rest"
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ sign

fn read_report(dir: &Path) -> Res<SignedReport> {
    let p = dir.join("report.bin");
    let b = std::fs::read(&p).map_err(|e| format!("{}: {e} (run sign first)", p.display()))?;
    SignedReport::try_from_slice(&b).map_err(|_| format!("{}: does not decode", p.display()))
}

fn sign_cmd(o: &Opts) -> Res<()> {
    o.only(&["key", "dir"])?;
    let dir = PathBuf::from(o.req("dir")?);
    let m = read_manifest(&dir)?;
    let k = read_key(o.req("key")?)?;
    // The sequence is fixed by the dataset and version (review CL5-R1): two
    // manifests for one version under one key would be an equivocation the
    // book suspends the key for. `anchor` checks the book before sending.
    let r = onyx_bulk::anchor::sign(&k, &m.header).map_err(|e| e.to_string())?;
    let seq = r.payload.sequence;
    std::fs::write(
        dir.join("report.bin"),
        borsh::to_vec(&r).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "SIGNED signer={} key={} sequence={seq} digest={}",
        hx(&onyx_bulk::anchor::address_of(&k).map_err(|e| e.to_string())?),
        r.payload.key,
        r.payload.digest()
    );
    Ok(())
}

// ------------------------------------------------------------------ anchor

/// Calldata of `postReport(bytes payload, bytes signature)`.
fn post_report_calldata(payload: &[u8], signature: &[u8]) -> Vec<u8> {
    fn padded(b: &[u8]) -> Vec<u8> {
        let mut v = b.to_vec();
        v.resize(b.len().div_ceil(32) * 32, 0);
        v
    }
    fn word(n: usize) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&(n as u64).to_be_bytes());
        w
    }
    let sel = keccak256(b"postReport(bytes,bytes)");
    let mut d = sel.0[..4].to_vec();
    let a = padded(payload);
    d.extend_from_slice(&word(64));
    d.extend_from_slice(&word(64 + 32 + a.len()));
    d.extend_from_slice(&word(payload.len()));
    d.extend_from_slice(&a);
    d.extend_from_slice(&word(signature.len()));
    d.extend_from_slice(&padded(signature));
    d
}

fn parse_address(s: &str) -> Res<[u8; 20]> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{s:?} is not a 20-byte 0x address"))
}

async fn eth_call(rpc: &dyn JsonRpc, to: &[u8; 20], data: &[u8]) -> Res<Vec<u8>> {
    let r = rpc
        .call(
            "eth_call",
            json!([{"to": hx(to), "data": hx(data)}, "latest"]),
        )
        .await?;
    hex::decode(r.as_str().unwrap_or_default().trim_start_matches("0x")).map_err(|e| e.to_string())
}

async fn anchor_cmd(o: &Opts) -> Res<()> {
    o.only(&["dir", "rpc", "chain-id", "book", "gas-key"])?;
    let dir = PathBuf::from(o.req("dir")?);
    let m = read_manifest(&dir)?;
    let r = read_report(&dir)?;
    onyx_bulk::anchor::verify(&m.header, &r)
        .map_err(|e| format!("report.bin does not sign manifest.bin: {e}"))?;
    let url = o.req("rpc")?;
    check_facilitator_url(url).map_err(|e| e.replacen("facilitator", "chain node", 1))?;
    let chain_id: u64 = o
        .req("chain-id")?
        .parse()
        .map_err(|_| "--chain-id: not a number")?;
    if chain_id != 84_532 && chain_id != 31_337 {
        return Err(
            "test networks only: --chain-id 84532 (Base Sepolia) or 31337 (local anvil)".into(),
        );
    }
    let book = parse_address(o.req("book")?)?;
    let gas = read_key(o.req("gas-key")?)?;
    let rpc = HttpJsonRpc::new(url.to_string(), HyperPost::for_rpc(Duration::from_secs(30)));
    let id = rpc.call("eth_chainId", json!([])).await?;
    if u64::from_str_radix(id.as_str().unwrap_or("").trim_start_matches("0x"), 16).ok()
        != Some(chain_id)
    {
        return Err(format!("the node at --rpc is not chain {chain_id}"));
    }
    let conflict = format!(
        "this key already anchored another manifest for version {} of {} (sequence {}): a second one would get the key suspended. Publish the next version instead",
        m.header.version, m.header.dataset, r.payload.sequence
    );
    post_report(rpc, chain_id, &book, gas, &r, &conflict).await
}

/// Send `r` to the book with `postReport` unless it is recorded already;
/// refuse when the key used its sequence for another payload (`conflict`).
async fn post_report(
    rpc: HttpJsonRpc<HyperPost>,
    chain_id: u64,
    book: &[u8; 20],
    gas: [u8; 32],
    r: &SignedReport,
    conflict: &str,
) -> Res<()> {
    let book = *book;
    let digest = r.payload.digest();
    // Already recorded (a re-run): nothing to send.
    let mut d = keccak256(b"report(bytes32)").0[..4].to_vec();
    d.extend_from_slice(&digest.0);
    let rec = eth_call(&rpc, &book, &d).await?;
    if rec.len() >= 32 && rec[..32] != [0u8; 32] {
        println!("ANCHORED already digest={digest}");
        return Ok(());
    }
    // Never sign two payloads under one sequence: the book would suspend
    // the key (ADR-0028 equivocation).
    let mut d = keccak256(b"sequenceOf(bytes32,uint64)").0[..4].to_vec();
    d.extend_from_slice(&r.payload.key.0);
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&r.payload.sequence.to_be_bytes());
    d.extend_from_slice(&w);
    let used = eth_call(&rpc, &book, &d).await?;
    if used.len() >= 32 && used[..32] != [0u8; 32] && used[..32] != digest.0 {
        return Err(conflict.to_string());
    }
    let payload = borsh::to_vec(&r.payload).map_err(|e| e.to_string())?;
    let sender = Sender::new(rpc, chain_id, gas).ok_or("--gas-key holds no valid key")?;
    let t = Instant::now();
    let mined = sender
        .send(
            &book,
            &post_report_calldata(&payload, &r.signature),
            0,
            Duration::from_secs(180),
        )
        .await
        .map_err(|e| format!("postReport: {e}"))?;
    let rec = eth_call(sender.rpc(), &book, &{
        let mut d = keccak256(b"report(bytes32)").0[..4].to_vec();
        d.extend_from_slice(&digest.0);
        d
    })
    .await?;
    if rec.len() < 64 || rec[..32] == [0u8; 32] {
        return Err("the transaction mined but the book has no record of the report".into());
    }
    println!(
        "ANCHORED tx={} block={} gas={} secs={:.1} digest={digest} fact={}",
        mined.hash,
        mined.block,
        mined.gas_used,
        t.elapsed().as_secs_f64(),
        Hash32(rec[32..64].try_into().map_err(|_| "fact")?)
    );
    Ok(())
}

// ------------------------------------------------------------------ upload

type Client = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Full<hyper::body::Bytes>,
>;

fn client() -> Client {
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build(https)
}

/// One request: the status, the JSON answer and its `retry-after` seconds.
async fn send(
    c: &Client,
    method: hyper::Method,
    url: &str,
    ctype: &str,
    body: hyper::body::Bytes,
) -> Res<(u16, Value, Option<u64>)> {
    check_facilitator_url(url).map_err(|e| e.replacen("facilitator", "node", 1))?;
    let req = hyper::Request::builder()
        .method(method)
        .uri(url)
        .header("content-type", ctype)
        .header("content-length", body.len().to_string())
        .body(http_body_util::Full::new(body))
        .map_err(|e| e.to_string())?;
    let r = tokio::time::timeout(Duration::from_secs(600), c.request(req))
        .await
        .map_err(|_| "the node did not answer in time")?
        .map_err(|e| format!("node unreachable: {e}"))?;
    let status = r.status().as_u16();
    let after = r
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.trim().parse::<u64>().ok());
    let b = http_body_util::Limited::new(r.into_body(), 4 << 20)
        .collect()
        .await
        .map_err(|e| format!("node answer unreadable: {e}"))?
        .to_bytes();
    Ok((
        status,
        serde_json::from_slice(&b).unwrap_or(Value::Null),
        after,
    ))
}

/// Rounds of shard uploads before `upload` gives up: after each round the
/// manifest is posted again and the node lists what it still misses.
const UPLOAD_ROUNDS: u32 = 3;

/// Passing answers in a row (a node's 503 `bulk_chain_unavailable`,
/// `overloaded` or `bulk_busy`, a 429, a proxy's 502 while the node restarts,
/// or no answer at all) tried again before a command gives up, each at least
/// [`RETRY_SECS`] apart or the node's `retry-after` (at most
/// [`MAX_RETRY_SECS`]). A not-final anchor answer between them starts the
/// count again (review of the retry, finding 2). The refresh of 2026-11-08
/// stopped after its anchor on one 503 (the public RPC behind the node
/// answered 429). Every retried request is idempotent on the node.
const TRANSIENT_TRIES: u32 = 8;
/// Seconds between two tries at least.
const RETRY_SECS: u64 = 15;
/// The longest `retry-after` followed.
const MAX_RETRY_SECS: u64 = 120;

/// What a command does with a node's answer.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Taken.
    Done,
    /// The anchor is not final yet: wait (within `--wait-secs`).
    Final,
    /// Passing: try again ([`TRANSIENT_TRIES`] in a row at most).
    Again,
    /// Refused.
    Fail,
}

/// Whether an answer will likely clear by itself. A 503 that will not (a
/// full node, `bulk_node_full`) is not one (review of the retry, finding 3).
fn passing(status: u16, v: &Value) -> bool {
    match status {
        429 | 502 => true,
        503 => matches!(
            v["error"]["code"].as_str(),
            Some("bulk_chain_unavailable" | "overloaded" | "bulk_busy")
        ),
        _ => false,
    }
}

/// The step after answer `status`/`v`, `elapsed` of `wait`, after `tries`
/// passing answers in a row.
fn next_step(status: u16, v: &Value, elapsed: Duration, wait: Duration, tries: u32) -> Next {
    match status {
        200 | 201 => Next::Done,
        409 if v["error"]["code"] == "bulk_not_anchored" && elapsed < wait => Next::Final,
        _ if passing(status, v) && tries < TRANSIENT_TRIES => Next::Again,
        _ => Next::Fail,
    }
}

/// Send `body` until the node takes it: wait for a not-final anchor within
/// `wait` (every `pause`, at most `wait`), try passing answers again (at
/// least `pause` apart), and refuse the rest as `"{what} refused ..."`.
/// `pause` is [`RETRY_SECS`] seconds except in tests.
#[allow(clippy::too_many_arguments)]
async fn send_until(
    c: &Client,
    method: hyper::Method,
    url: &str,
    ctype: &str,
    body: hyper::body::Bytes,
    wait: Duration,
    pause: Duration,
    what: &str,
) -> Res<Value> {
    let t = Instant::now();
    let mut tries = 0;
    loop {
        let (s, v, after) = match send(c, method.clone(), url, ctype, body.clone()).await {
            Ok(a) => a,
            Err(e) if tries < TRANSIENT_TRIES => {
                tries += 1;
                eprintln!("{what}: {e}; try {tries} of {TRANSIENT_TRIES} again");
                tokio::time::sleep(pause).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        match next_step(s, &v, t.elapsed(), wait, tries) {
            Next::Done => return Ok(v),
            Next::Final => {
                tries = 0;
                eprintln!(
                    "{what}: waiting for the anchor to be final ({}; {:.0} s so far)",
                    v["reason"].as_str().unwrap_or("?"),
                    t.elapsed().as_secs_f64()
                );
                tokio::time::sleep(pause.min(wait)).await;
            }
            Next::Again => {
                tries += 1;
                let d = pause.max(Duration::from_secs(after.unwrap_or(0).min(MAX_RETRY_SECS)));
                eprintln!(
                    "{what}: the node said try again ({s} {}); try {tries} of {TRANSIENT_TRIES} in {:.0} s",
                    v["error"]["code"].as_str().unwrap_or("?"),
                    d.as_secs_f64()
                );
                tokio::time::sleep(d).await;
            }
            Next::Fail => return Err(format!("{what} refused ({s}): {v}")),
        }
    }
}

/// The node's refusal of a version whose predecessor it lacks
/// (`bulk_not_successor`).
const TAKEN_IN_ORDER: &str = "versions are taken in order";
/// Versions `upload` steps back at most (one per version a node lacks).
const MAX_STEPS_BACK: u32 = 64;
/// What `upload` records in a version folder once a node took the version:
/// the catalog number it was uploaded with (a step back uploads the
/// version again with the same one).
const UPLOADED: &str = "uploaded.json";

async fn upload_cmd(o: &Opts) -> Res<()> {
    o.only(&["dir", "node", "wait-secs", "catalog"])?;
    let dir = PathBuf::from(o.req("dir")?);
    let node = o.req("node")?.trim_end_matches('/').to_string();
    let wait = Duration::from_secs(o.num("wait-secs", 0)?);
    let catalog = o
        .opt("catalog")
        .map(|n| n.parse::<u64>().map_err(|_| "--catalog: a catalog number"))
        .transpose()?;
    upload_dir(
        &client(),
        &node,
        &dir,
        catalog,
        wait,
        Duration::from_secs(RETRY_SECS),
        MAX_STEPS_BACK,
    )
    .await
}

/// The folder and catalog number of the version before `m` (in folder
/// `dir`): the folder `build --prev` named, which must hold `m`'s previous
/// root, and the catalog its own upload used (`uploaded.json`), else its own
/// report.
fn previous_version(dir: &Path, m: &Manifest) -> Res<(PathBuf, Option<u64>)> {
    let lacks = format!(
        "the node lacks version {} of {}",
        m.header.version.saturating_sub(1),
        m.header.dataset
    );
    let summary: Value = std::fs::read_to_string(dir.join("manifest.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let prev = summary["prev_dir"].as_str().map(PathBuf::from).ok_or_else(|| {
        format!(
            "{lacks}, and {} does not name its folder (built without --prev, or by an older onyx-publish): upload that version first",
            dir.display()
        )
    })?;
    let pm = read_manifest(&prev)?;
    if pm.header.root() != m.header.prev {
        return Err(format!(
            "{lacks}: {} holds another root than this version's prev",
            prev.display()
        ));
    }
    let uploaded: Value = std::fs::read_to_string(prev.join(UPLOADED))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let catalog = match uploaded["catalog"].as_u64() {
        Some(n) => Some(n),
        None if prev.join("report.bin").exists() => None,
        None => {
            return Err(format!(
                "{lacks}: {} has no report and no record of the catalog it was uploaded with: upload it with --catalog <the number of the catalog that lists it> first",
                prev.display()
            ))
        }
    };
    Ok((prev, catalog))
}

/// Upload the version in `dir`: its manifest (its own report, or catalog
/// `catalog`), then every shard the node misses, then the manifest again,
/// for at most [`UPLOAD_ROUNDS`]. A node that lacks the previous version
/// ("versions are taken in order": it upgraded after a widening it then
/// refused, ADR-0059 amendment 5 item 8) gets that version first from its
/// folder, at most `steps` versions back, and then this one again.
async fn upload_dir(
    c: &Client,
    node: &str,
    dir: &Path,
    catalog: Option<u64>,
    wait: Duration,
    pause: Duration,
    steps: u32,
) -> Res<()> {
    let m = read_manifest(dir)?;
    let body = match catalog {
        Some(n) => json!({"manifest": hx(&m.to_bytes()), "catalog": n}),
        None => {
            let r = read_report(dir)?;
            json!({"manifest": hx(&m.to_bytes()), "report": hx(&borsh::to_vec(&r).map_err(|e| e.to_string())?)})
        }
    };
    let t = Instant::now();
    let manifest_url = format!("{node}/v1/bulk/manifests");
    let body = hyper::body::Bytes::from(body.to_string().into_bytes());
    let post = || {
        send_until(
            c,
            hyper::Method::POST,
            &manifest_url,
            "application/json",
            body.clone(),
            wait,
            pause,
            "manifest",
        )
    };
    let answer = match post().await {
        // The publisher removed the dataset (ADR-0059 amendment 4): nothing
        // to send, and nothing left to do.
        Err(e) if e.contains("bulk_dataset_removed") => {
            println!(
                "REMOVED version={} root={}: the node holds the dataset as removed",
                m.header.version,
                m.header.root()
            );
            return Ok(());
        }
        Err(e) if e.contains(TAKEN_IN_ORDER) && m.header.version > 1 => {
            if steps == 0 {
                return Err(format!(
                    "{e}: the node lacks more than {MAX_STEPS_BACK} versions before this one"
                ));
            }
            let (prev, prev_catalog) = previous_version(dir, &m)?;
            eprintln!(
                "the node lacks version {}: uploading it first from {}",
                m.header.version - 1,
                prev.display()
            );
            Box::pin(upload_dir(
                c,
                node,
                &prev,
                prev_catalog,
                wait,
                pause,
                steps - 1,
            ))
            .await?;
            post().await?
        }
        a => a?,
    };
    let mut missing = answer["missing"].as_array().cloned().unwrap_or_default();
    let (mut sent, mut bytes, mut rounds) = (0u64, 0u64, 0u32);
    let manifest_time = t.elapsed();
    let tu = Instant::now();
    while !missing.is_empty() {
        if rounds == UPLOAD_ROUNDS {
            return Err(format!(
                "the node still misses {} shard(s) after {rounds} rounds",
                missing.len()
            ));
        }
        rounds += 1;
        for e in &missing {
            let root = e["root"].as_str().ok_or("node answer: missing[].root")?;
            let h = onyx_bulk::wire::hash_field(&e["root"], "root").map_err(|e| e.to_string())?;
            let p = shard_path(dir, &h);
            let file = std::fs::read(&p).map_err(|err| {
                format!("shard {root} is not in {} ({err}); upload it from the folder of the version that added it", p.display())
            })?;
            let url = format!(
                "{node}/v1/bulk/{}/{}/shards/{root}",
                m.header.publisher.0, m.header.dataset
            );
            let n = file.len() as u64;
            send_until(
                c,
                hyper::Method::PUT,
                &url,
                "application/octet-stream",
                hyper::body::Bytes::from(file),
                Duration::ZERO,
                pause,
                &format!("shard {root}"),
            )
            .await?;
            sent += 1;
            bytes += n;
        }
        // Ask again: a shard the node answered "stored" for must no longer be
        // missing (review BULK-LOSS-1: two of 8,849 were not, once).
        let v = send_until(
            c,
            hyper::Method::POST,
            &manifest_url,
            "application/json",
            body.clone(),
            wait,
            pause,
            "manifest recheck",
        )
        .await?;
        missing = v["missing"].as_array().cloned().unwrap_or_default();
        if !missing.is_empty() {
            eprintln!(
                "the node still misses {} shard(s) after round {rounds}: sending them again",
                missing.len()
            );
        }
    }
    let record = json!({"version": m.header.version, "root": m.header.root().to_string(), "catalog": catalog});
    std::fs::write(dir.join(UPLOADED), format!("{record}\n"))
        .map_err(|e| format!("{}: {e}", dir.join(UPLOADED).display()))?;
    println!(
        "UPLOADED version={} shards_sent={sent} bytes_sent={bytes} manifest_secs={:.1} shard_secs={:.1} rounds={rounds} root={}",
        m.header.version,
        manifest_time.as_secs_f64(),
        tu.elapsed().as_secs_f64(),
        m.header.root()
    );
    Ok(())
}

// ------------------------------------------------------------------ catalogs

fn read_catalog(path: &Path) -> Res<onyx_bulk::Catalog> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let c = <onyx_bulk::Catalog as BorshDeserialize>::try_from_slice(&b)
        .map_err(|_| format!("{}: not a Borsh catalog", path.display()))?;
    c.validate()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(c)
}

fn report_path(catalog: &Path) -> PathBuf {
    let mut s = catalog.as_os_str().to_owned();
    s.push(".report");
    PathBuf::from(s)
}

/// The catalog of every manifest in the version folders listed in `--dirs`,
/// plus every other dataset of `--prev`, written to `--out` (never over an
/// existing file: a catalog is kept before it is signed, review CAT-R1 M3).
fn catalog_build_cmd(o: &Opts) -> Res<()> {
    o.only(&["publisher", "dirs", "out", "prev", "remove"])?;
    let publisher = SourceId(o.req("publisher")?.to_string());
    let out = PathBuf::from(o.req("out")?);
    if out.exists() {
        return Err(format!(
            "{} exists: a catalog is never rebuilt in place",
            out.display()
        ));
    }
    let prev = o
        .opt("prev")
        .map(|p| read_catalog(Path::new(p)))
        .transpose()?;
    let list = std::fs::read_to_string(o.req("dirs")?).map_err(|e| format!("--dirs: {e}"))?;
    let mut leaves: BTreeMap<String, onyx_bulk::CatalogLeaf> = prev
        .iter()
        .flat_map(|p| p.leaves.iter().cloned())
        .map(|l| (l.dataset.clone(), l))
        .collect();
    // --remove: a tombstone for each named dataset (ADR-0059 amendment 4).
    let remove: Vec<String> = o
        .opt("remove")
        .map(|r| {
            r.split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let mut listed = 0usize;
    let (mut skipped, mut removed) = (0usize, 0usize);
    for d in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let m = read_manifest(Path::new(d))?;
        if m.header.publisher != publisher {
            return Err(format!("{d}: another publisher ({})", m.header.publisher.0));
        }
        if remove.contains(&m.header.dataset) {
            return Err(format!(
                "{d}: {} is both listed and to be removed",
                m.header.dataset
            ));
        }
        // A removed dataset stays removed: its folder is left out (review
        // CAT-TOMB-R1 M6), never put back over the tombstone.
        if leaves
            .get(&m.header.dataset)
            .is_some_and(onyx_bulk::CatalogLeaf::is_tombstone)
        {
            println!("SKIPPED {d}: {} is removed", m.header.dataset);
            skipped += 1;
            continue;
        }
        leaves.insert(
            m.header.dataset.clone(),
            onyx_bulk::CatalogLeaf::of(&m.header),
        );
        listed += 1;
    }
    for d in &remove {
        let Some(l) = leaves.get(d) else {
            return Err(format!(
                "--remove {d}: the previous catalog does not hold it"
            ));
        };
        let t = l
            .tombstone()
            .ok_or_else(|| format!("--remove {d}: removed already"))?;
        leaves.insert(d.clone(), t);
        removed += 1;
    }
    // Strictly later than the catalog it follows (the book files a fact by its time).
    let created = prev
        .as_ref()
        .map_or(now_unix(), |p| now_unix().max(p.header.created_unix + 1));
    let c = onyx_bulk::Catalog::build(
        publisher,
        prev.as_ref(),
        created,
        leaves.into_values().collect(),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(&out, borsh::to_vec(&c).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "CATALOG number={} leaves={} listed={listed} removed={removed} skipped={skipped} root={} out={}",
        c.header.number,
        c.header.leaf_count,
        c.header.root(),
        out.display()
    );
    Ok(())
}

fn catalog_sign_cmd(o: &Opts) -> Res<()> {
    o.only(&["key", "catalog"])?;
    let path = PathBuf::from(o.req("catalog")?);
    let c = read_catalog(&path)?;
    let k = read_key(o.req("key")?)?;
    let r = onyx_bulk::catalog::sign(&k, &c.header).map_err(|e| e.to_string())?;
    let rp = report_path(&path);
    if let Ok(old) = std::fs::read(&rp) {
        if old != borsh::to_vec(&r).map_err(|e| e.to_string())? {
            return Err(format!(
                "{} holds another report: never sign two catalogs under one number",
                rp.display()
            ));
        }
    }
    std::fs::write(&rp, borsh::to_vec(&r).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", rp.display()))?;
    println!(
        "SIGNED catalog={} sequence={} digest={}",
        c.header.number,
        r.payload.sequence,
        r.payload.digest()
    );
    Ok(())
}

async fn catalog_anchor_cmd(o: &Opts) -> Res<()> {
    o.only(&["catalog", "rpc", "chain-id", "book", "gas-key"])?;
    let path = PathBuf::from(o.req("catalog")?);
    let c = read_catalog(&path)?;
    let rp = report_path(&path);
    let r = SignedReport::try_from_slice(
        &std::fs::read(&rp).map_err(|e| format!("{}: {e}", rp.display()))?,
    )
    .map_err(|_| format!("{}: not a Borsh SignedReport", rp.display()))?;
    onyx_bulk::catalog::verify(&c.header, &r)
        .map_err(|e| format!("the report does not sign this catalog: {e}"))?;
    let url = o.req("rpc")?;
    check_facilitator_url(url).map_err(|e| e.replacen("facilitator", "chain node", 1))?;
    let chain_id: u64 = o
        .req("chain-id")?
        .parse()
        .map_err(|_| "--chain-id: not a number")?;
    if chain_id != 84_532 && chain_id != 31_337 {
        return Err(
            "test networks only: --chain-id 84532 (Base Sepolia) or 31337 (local anvil)".into(),
        );
    }
    let book = parse_address(o.req("book")?)?;
    let gas = read_key(o.req("gas-key")?)?;
    let rpc = HttpJsonRpc::new(url.to_string(), HyperPost::for_rpc(Duration::from_secs(30)));
    let id = rpc.call("eth_chainId", json!([])).await?;
    if u64::from_str_radix(id.as_str().unwrap_or("").trim_start_matches("0x"), 16).ok()
        != Some(chain_id)
    {
        return Err(format!("the node at --rpc is not chain {chain_id}"));
    }
    let conflict = format!(
        "this key already anchored another catalog {} (sequence {}): a second one would get the key suspended. Build the next number instead",
        c.header.number, r.payload.sequence
    );
    post_report(rpc, chain_id, &book, gas, &r, &conflict).await
}

async fn catalog_upload_cmd(o: &Opts) -> Res<()> {
    o.only(&["catalog", "node", "wait-secs"])?;
    let path = PathBuf::from(o.req("catalog")?);
    let c = read_catalog(&path)?;
    let rp = report_path(&path);
    let r = std::fs::read(&rp).map_err(|e| format!("{}: {e}", rp.display()))?;
    let node = o.req("node")?.trim_end_matches('/').to_string();
    let wait = Duration::from_secs(o.num("wait-secs", 0)?);
    let body =
        json!({"catalog": hx(&borsh::to_vec(&c).map_err(|e| e.to_string())?), "report": hx(&r)});
    let cl = client();
    send_until(
        &cl,
        hyper::Method::POST,
        &format!("{node}/v1/bulk/catalogs"),
        "application/json",
        hyper::body::Bytes::from(body.to_string().into_bytes()),
        wait,
        Duration::from_secs(RETRY_SECS),
        "catalog",
    )
    .await?;
    println!(
        "UPLOADED catalog={} leaves={} root={}",
        c.header.number,
        c.header.leaf_count,
        c.header.root()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node's passing answers are tried again, a not-final anchor is
    /// waited for within `--wait-secs`, a 503 that will not clear and the
    /// rest are refused.
    #[test]
    fn passing_answers_are_tried_again_and_the_rest_refused() {
        let none = json!(null);
        let code = |c: &str| json!({"error": {"code": c}});
        let (z, w) = (Duration::ZERO, Duration::from_secs(60));
        assert_eq!(next_step(201, &none, z, w, 0), Next::Done);
        assert_eq!(next_step(200, &none, z, z, 0), Next::Done);
        let nf = code("bulk_not_anchored");
        assert_eq!(next_step(409, &nf, z, w, 0), Next::Final);
        assert_eq!(next_step(409, &nf, w, w, 0), Next::Fail, "past --wait-secs");
        for (s, c) in [
            (503, "bulk_chain_unavailable"),
            (503, "overloaded"),
            (503, "bulk_busy"),
            (429, "rate_limited"),
            (502, "?"),
        ] {
            let v = code(c);
            assert_eq!(next_step(s, &v, z, z, 0), Next::Again, "{s} {c}");
            assert_eq!(next_step(s, &v, z, z, TRANSIENT_TRIES - 1), Next::Again);
            assert_eq!(next_step(s, &v, z, z, TRANSIENT_TRIES), Next::Fail);
        }
        assert_eq!(next_step(503, &code("bulk_node_full"), z, w, 0), Next::Fail);
        for s in [400, 403, 404, 413, 422, 500, 504] {
            assert_eq!(next_step(s, &code("x"), z, w, 0), Next::Fail, "{s}");
        }
    }

    /// A node that answers each request with the next of `answers`
    /// (status, JSON body), then closes; the requests it got, counted.
    async fn scripted_node(
        answers: Vec<(u16, &'static str)>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let (url, seen, _) = scripted_node_log(answers).await;
        (url, seen)
    }

    /// The bodies of the requests a scripted node got, in order.
    type Bodies = std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>;

    /// [`scripted_node`], keeping each request's body too.
    async fn scripted_node_log(
        answers: Vec<(u16, &'static str)>,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        Bodies,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let bodies: Bodies = Default::default();
        let log = bodies.clone();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}", l.local_addr().expect("addr"));
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = seen.clone();
        tokio::spawn(async move {
            for (status, body) in answers {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                // The head, then the body its content-length names.
                let mut buf = Vec::new();
                let mut b = [0u8; 4096];
                let head_end = loop {
                    let Ok(n) = s.read(&mut b).await else { return };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&b[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let Ok(n) = s.read(&mut b).await else { return };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&b[..n]);
                }
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut b) = log.lock() {
                    b.push(buf[head_end..].to_vec());
                }
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        (url, seen, bodies)
    }

    async fn post(url: &str, wait: Duration) -> Res<Value> {
        send_until(
            &client(),
            hyper::Method::POST,
            &format!("{url}/v1/bulk/catalogs"),
            "application/json",
            hyper::body::Bytes::from_static(b"{}"),
            wait,
            Duration::from_millis(1),
            "catalog",
        )
        .await
    }

    /// The refresh of 2026-11-08: a 503 the node marks as passing is tried
    /// again and the upload goes through; a full node is refused at once;
    /// a not-final anchor between passing answers starts their count again.
    #[tokio::test]
    async fn a_passing_503_is_tried_again_and_a_full_node_is_not() {
        let chain = r#"{"error":{"code":"bulk_chain_unavailable"}}"#;
        let (url, seen) = scripted_node(vec![
            (503, chain),
            (503, r#"{"error":{"code":"overloaded"}}"#),
            (201, r#"{"ok":true}"#),
        ])
        .await;
        let v = post(&url, Duration::ZERO).await.expect("taken");
        assert_eq!(v["ok"], json!(true));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 3);
        let (url, seen) = scripted_node(vec![
            (503, r#"{"error":{"code":"bulk_node_full"}}"#),
            (201, "{}"),
        ])
        .await;
        let e = post(&url, Duration::ZERO).await.expect_err("refused");
        assert!(e.contains("bulk_node_full"), "{e}");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Seven passing answers, a not-final anchor, seven more, then taken:
        // fourteen in all, but never eight in a row.
        let n = (TRANSIENT_TRIES - 1) as usize;
        let mut script = vec![(429, r#"{"error":{"code":"rate_limited"}}"#); n];
        script.push((409, r#"{"error":{"code":"bulk_not_anchored"}}"#));
        script.extend(vec![(503, chain); n]);
        script.push((201, "{}"));
        let (url, seen) = scripted_node(script).await;
        post(&url, Duration::from_secs(600)).await.expect("taken");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2 * n + 2);
        // Eight passing answers in a row: refused.
        let (url, _) = scripted_node(vec![(503, chain); TRANSIENT_TRIES as usize + 1]).await;
        let e = post(&url, Duration::ZERO).await.expect_err("refused");
        assert!(e.contains("bulk_chain_unavailable"), "{e}");
    }

    #[test]
    fn synth_rows_build_into_shards_and_successors() {
        let mut csv = Vec::new();
        let (rows, bytes) = write_synth(&mut csv, 1, 1_000).expect("ok");
        assert!(bytes >= 1 << 20 && rows > 10_000);
        let schema = Schema::from_json(&synth_schema()).expect("ok");
        let dir = std::env::temp_dir().join(format!("onyx-publish-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let nv = |t| NewVersion {
            dataset: "synthetic.test".into(),
            publisher: SourceId("example.publisher".into()),
            created_unix: t,
            schema: schema.clone(),
            rights: DataRights {
                license: "CC0-1.0".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId("example.publisher".into()),
                attribution: None,
                terms: String::new(),
            },
        };
        let b = build(
            &schema,
            &csv[..],
            Format::Csv,
            256 << 10,
            None,
            nv(10),
            &dir.join("v1"),
        )
        .expect("ok");
        assert_eq!(b.input_rows, rows);
        assert_eq!(b.manifest.header.row_count, rows);
        assert!(b.manifest.shards.len() >= 4, "{}", b.manifest.shards.len());
        for (e, p) in &b.new_shards {
            let f = std::fs::read(p).expect("ok");
            assert!(f.len() as u64 <= 256 << 10);
            assert_eq!(check_shard(&schema, &f).expect("ok").summary.root, e.root);
        }
        // A successor adds rows after the last key; rows before it are refused.
        let more = format!(
            "t,series,value,count\n{},synthetic-0,1.5,2\n",
            1_000 + rows * 12
        );
        let b2 = build(
            &schema,
            more.as_bytes(),
            Format::Csv,
            256 << 10,
            Some(&b.manifest),
            nv(11),
            &dir.join("v2"),
        )
        .expect("ok");
        assert_eq!(b2.manifest.header.version, 2);
        assert_eq!(b2.new_shards.len(), 1);
        let back = "t,series,value,count\n5,synthetic-0,1.5,2\n";
        assert!(build(
            &schema,
            back.as_bytes(),
            Format::Csv,
            256 << 10,
            Some(&b.manifest),
            nv(12),
            &dir.join("v3")
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADR-0059 amendment 4 (THREATS CAT-3): `catalog-build --remove` writes
    /// a tombstone, a later folder of a removed dataset is skipped (never put
    /// back over it), and a removal of a dataset not held, removed already,
    /// or listed in the same build is refused.
    #[test]
    fn catalog_build_removes_a_dataset_and_keeps_it_removed() {
        let schema = Schema::from_json(&synth_schema()).expect("ok");
        let dir = std::env::temp_dir().join(format!("onyx-publish-rm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut csv = Vec::new();
        write_synth(&mut csv, 1, 50).expect("ok");
        for d in ["a", "b"] {
            let nv = NewVersion {
                dataset: format!("synthetic.{d}"),
                publisher: SourceId("example.publisher".into()),
                created_unix: 10,
                schema: schema.clone(),
                rights: DataRights {
                    license: "CC0-1.0".into(),
                    redistribution: Redistribution::Allowed,
                    source: SourceId("example.publisher".into()),
                    attribution: None,
                    terms: String::new(),
                },
            };
            let b = build(
                &schema,
                &csv[..],
                Format::Csv,
                256 << 10,
                None,
                nv,
                &dir.join(d),
            )
            .expect("ok");
            std::fs::write(dir.join(d).join("manifest.bin"), b.manifest.to_bytes()).expect("w");
        }
        let p = |n: &str| dir.join(n).display().to_string();
        let list = |name: &str, items: &[&str]| {
            let f = dir.join(name);
            std::fs::write(&f, items.iter().map(|i| p(i) + "\n").collect::<String>()).expect("w");
            f.display().to_string()
        };
        let run = |args: &[String]| catalog_build_cmd(&parse_opts(args).expect("opts"));
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let both = list("both.txt", &["a", "b"]);
        let none = list("none.txt", &[]);
        let only_a = list("a.txt", &["a"]);
        run(&a(&[
            "--publisher",
            "example.publisher",
            "--dirs",
            &both,
            "--out",
            &p("c0.bin"),
        ]))
        .expect("c0");
        run(&a(&[
            "--publisher",
            "example.publisher",
            "--dirs",
            &none,
            "--prev",
            &p("c0.bin"),
            "--remove",
            "synthetic.a",
            "--out",
            &p("c1.bin"),
        ]))
        .expect("c1");
        let c1 = read_catalog(&dir.join("c1.bin")).expect("c1");
        let t = &c1.leaves[c1.find("synthetic.a").expect("listed")];
        assert!(t.is_tombstone() && t.version == 2);
        assert!(!c1.leaves[c1.find("synthetic.b").expect("listed")].is_tombstone());
        // The removed dataset's folder again: skipped, the tombstone stays.
        run(&a(&[
            "--publisher",
            "example.publisher",
            "--dirs",
            &only_a,
            "--prev",
            &p("c1.bin"),
            "--out",
            &p("c2.bin"),
        ]))
        .expect("c2");
        let c2 = read_catalog(&dir.join("c2.bin")).expect("c2");
        assert_eq!(c2.leaves, c1.leaves);
        for (args, why) in [
            (
                vec![
                    "--dirs",
                    &none,
                    "--prev",
                    &p("c1.bin"),
                    "--remove",
                    "synthetic.a",
                ],
                "removed already",
            ),
            (
                vec![
                    "--dirs",
                    &none,
                    "--prev",
                    &p("c1.bin"),
                    "--remove",
                    "synthetic.z",
                ],
                "does not hold",
            ),
            (
                vec![
                    "--dirs",
                    &only_a,
                    "--prev",
                    &p("c0.bin"),
                    "--remove",
                    "synthetic.a",
                ],
                "both listed",
            ),
        ] {
            let mut v = a(&["--publisher", "example.publisher", "--out", &p("cx.bin")]);
            v.extend(args.iter().map(|s| s.to_string()));
            let e = run(&v).expect_err(why);
            assert!(e.contains(why), "{e}");
            assert!(!dir.join("cx.bin").exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A temporary folder for one test, and a way to run a command in it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("onyx-publish-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `build` of `input` under `schema` into `out` (after `prev`, with
    /// `--widen` if `widen`), all in `dir`.
    fn build_in(
        dir: &Path,
        schema: &str,
        input: &str,
        out: &str,
        prev: Option<&str>,
        widen: bool,
        created: u64,
    ) -> Res<()> {
        let p = |n: &str| dir.join(n).display().to_string();
        let mut a = args(&[
            "--schema",
            &p(schema),
            "--input",
            &p(input),
            "--dataset",
            "synthetic.test",
            "--publisher",
            "example.publisher",
            "--license",
            "CC0-1.0",
            "--out",
            &p(out),
            "--created",
            &created.to_string(),
        ]);
        if let Some(prev) = prev {
            a.extend(args(&["--prev", &p(prev)]));
        }
        if widen {
            a.push("--widen".into());
        }
        build_cmd(&parse_opts(&a).expect("opts"))
    }

    /// ADR-0059 amendment 5 item 7: new rows that need more places are
    /// refused under the previous schema (naming the column and the places),
    /// a different schema needs `--widen`, `--widen` needs `--prev` and a
    /// schema that widens, and a widened build writes format 2 with the
    /// previous schema up to the previous shard count; the next version
    /// under the same schema stays format 2 without the flag.
    #[test]
    fn build_widen_needs_the_flag_and_writes_format_2() {
        let dir = scratch("widen");
        let old = json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":2},{"name":"n","kind":"int"}],"key":"t"});
        let new = json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":9},{"name":"n","kind":"decimal","scale":2},{"name":"note","kind":"text","nullable":true}],"key":"t"});
        let narrow = json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":1},{"name":"n","kind":"int"}],"key":"t"});
        for (f, v) in [
            ("old.json", &old),
            ("new.json", &new),
            ("narrow.json", &narrow),
        ] {
            std::fs::write(dir.join(f), v.to_string()).expect("w");
        }
        std::fs::write(dir.join("v1.csv"), "t,v,n\n1,1.25,3\n2,2.5,4\n").expect("w");
        std::fs::write(dir.join("v2.csv"), "t,v,n\n3,3.1234567,5\n").expect("w");
        std::fs::write(
            dir.join("v2w.csv"),
            "t,v,n,note\n3,3.1234567,5.5,x\n4,4,6,\n",
        )
        .expect("w");
        std::fs::write(dir.join("v3.csv"), "t,v,n,note\n5,5.5,7,y\n").expect("w");
        build_in(&dir, "old.json", "v1.csv", "v1", None, false, 10).expect("v1");
        let e =
            build_in(&dir, "old.json", "v2.csv", "v2a", Some("v1"), false, 20).expect_err("places");
        assert!(
            e.contains("column v")
                && e.contains("needs 7 decimal places")
                && e.contains("schema-fit"),
            "{e}"
        );
        let e = build_in(&dir, "new.json", "v2w.csv", "v2b", Some("v1"), false, 20)
            .expect_err("no flag");
        assert!(e.contains("--widen") && e.contains("column v"), "{e}");
        let e = build_in(&dir, "new.json", "v2w.csv", "v2c", None, true, 20).expect_err("no prev");
        assert!(e.contains("--widen needs --prev"), "{e}");
        let e = build_in(&dir, "narrow.json", "v1.csv", "v2d", Some("v1"), true, 20)
            .expect_err("narrower");
        assert!(e.contains("does not widen"), "{e}");
        build_in(&dir, "new.json", "v2w.csv", "v2", Some("v1"), true, 20).expect("widened");
        let v1 = read_manifest(&dir.join("v1")).expect("v1");
        let v2 = read_manifest(&dir.join("v2")).expect("v2");
        assert_eq!(v2.header.format, onyx_bulk::manifest::FORMAT_2);
        assert_eq!(
            v2.header.earlier,
            vec![onyx_bulk::EarlierSchema {
                until: v1.header.shard_count,
                schema: v1.header.schema.clone(),
            }]
        );
        assert_eq!(v2.header.schema, Schema::from_json(&new).expect("ok"));
        let summary: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("v2").join("manifest.json")).expect("r"),
        )
        .expect("json");
        assert_eq!(summary["format"], json!(2));
        assert_eq!(
            summary["prev_dir"],
            json!(std::fs::canonicalize(dir.join("v1"))
                .expect("abs")
                .display()
                .to_string())
        );
        // Every new shard checks under the newest schema, the old under theirs.
        for e in &v2.shards[v1.shards.len()..] {
            let f = std::fs::read(shard_path(&dir.join("v2"), &e.root)).expect("shard");
            assert_eq!(
                check_shard(v2.header.schema_of(e.index), &f)
                    .expect("ok")
                    .summary
                    .root,
                e.root
            );
            assert!(check_shard(&v1.header.schema, &f).is_err());
        }
        // Version 3 under the same schema: no flag needed, format 2 kept.
        build_in(&dir, "new.json", "v3.csv", "v3", Some("v2"), false, 30).expect("v3");
        let v3 = read_manifest(&dir.join("v3")).expect("v3");
        assert_eq!(
            (v3.header.format, &v3.header.earlier),
            (2, &v2.header.earlier)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Item 7 (review BULK-WIDEN-R1 finding 4): `schema-fit` widens only
    /// what the rows need, to a scale that lasts for binary-float text (17
    /// significant digits at the column's smallest non-zero magnitude, plus
    /// 2), adds new input columns as nullable, keeps columns that fit, says
    /// so when nothing needs widening, and names the values no widening can
    /// take (with the schema written for the rest).
    #[test]
    fn schema_fit_writes_the_narrowest_widening() {
        let dir = scratch("fit");
        let p = |n: &str| dir.join(n).display().to_string();
        let v1 = json!({"columns":[
            {"name":"t","kind":"int"},
            {"name":"v","kind":"decimal","scale":7},
            {"name":"n","kind":"int"},
            {"name":"s","kind":"text"},
            {"name":"z","kind":"decimal","scale":3,"nullable":true}
        ],"key":"t"});
        std::fs::write(dir.join("v1.json"), v1.to_string()).expect("w");
        std::fs::write(dir.join("v1.csv"), "t,v,n,s,z\n1,711.5473168,3,a,1.5\n").expect("w");
        build_in(&dir, "v1.json", "v1.csv", "v1", None, false, 10).expect("v1");
        // The source now writes full binary-float digits, and two columns more.
        std::fs::write(
            dir.join("v2.csv"),
            "t,v,n,s,z,extra,more\n2,711.5473167999999,4,b,,7,2.5\n3,0.0001234567890123456,5,c,0.125,8,\n",
        )
        .expect("w");
        let fit = |input: &str, prev: &str, out: &str| {
            schema_fit_cmd(
                &parse_opts(&args(&[
                    "--prev",
                    &p(prev),
                    "--input",
                    &p(input),
                    "--out",
                    &p(out),
                ]))
                .expect("opts"),
            )
        };
        fit("v2.csv", "v1", "fit.json").expect("fits");
        let got = Schema::from_json(
            &serde_json::from_str::<Value>(
                &std::fs::read_to_string(dir.join("fit.json")).expect("r"),
            )
            .expect("json"),
        )
        .expect("schema");
        // v: the smallest value's first digit is at 10^-4, so 17 digits need
        // 20 places, plus 2; n and z fit as they are; extra holds whole
        // numbers, more a decimal at 10^0 (16 places plus 2).
        let want = Schema::from_json(&json!({"columns":[
            {"name":"t","kind":"int"},
            {"name":"v","kind":"decimal","scale":22},
            {"name":"n","kind":"int"},
            {"name":"s","kind":"text"},
            {"name":"z","kind":"decimal","scale":3,"nullable":true},
            {"name":"extra","kind":"int","nullable":true},
            {"name":"more","kind":"decimal","scale":18,"nullable":true}
        ],"key":"t"}))
        .expect("ok");
        assert_eq!(got, want);
        assert_eq!(
            Schema::widens(&Schema::from_json(&v1).expect("ok"), &got),
            Ok(())
        );
        // The fitted schema builds the new rows as version 2.
        build_in(&dir, "fit.json", "v2.csv", "v2", Some("v1"), true, 20).expect("v2");
        assert_eq!(read_manifest(&dir.join("v2")).expect("v2").header.format, 2);
        // Rows that fit version 2's schema: no widening, the same schema.
        std::fs::write(
            dir.join("v3.csv"),
            "t,v,n,s,z,extra,more\n4,1.5,6,d,0.5,9,0.25\n",
        )
        .expect("w");
        fit("v3.csv", "v2", "same.json").expect("fits as is");
        assert_eq!(
            std::fs::read_to_string(dir.join("same.json")).expect("r"),
            std::fs::read_to_string(dir.join("fit.json")).expect("r")
        );
        // Values no widening takes: 39 places, a word in a decimal column, an
        // empty field in a column that is not nullable. Named; schema written.
        std::fs::write(
            dir.join("bad.csv"),
            format!("t,v,n,s,z\n5,1.{},6,d,abc\n6,2.5,,e,\n", "1".repeat(39)),
        )
        .expect("w");
        let e = fit("bad.csv", "v1", "bad.json").expect_err("unfit");
        assert!(e.contains("3 value(s)"), "{e}");
        assert!(dir.join("bad.json").exists());
        // JSON Lines: a decimal given as a JSON number is named too (build
        // wants it as a string); an absent column reads null.
        std::fs::write(
            dir.join("v2.jsonl"),
            "{\"t\":7,\"v\":\"2.123456789\",\"n\":8,\"s\":\"f\"}\n{\"t\":8,\"v\":1.5,\"n\":9,\"s\":\"g\"}\n",
        )
        .expect("w");
        let e = fit("v2.jsonl", "v1", "j.json").expect_err("a JSON number");
        assert!(e.contains("1 value(s)"), "{e}");
        let got = Schema::from_json(
            &serde_json::from_str::<Value>(
                &std::fs::read_to_string(dir.join("j.json")).expect("r"),
            )
            .expect("json"),
        )
        .expect("schema");
        assert_eq!(
            got.columns[1].kind,
            onyx_bulk::ColumnKind::Decimal { scale: 18 }
        );
        // An int column given "6.0" becomes decimal (build would refuse it as
        // int); the fit scale follows its magnitude (10^0: 16 places plus 2).
        std::fs::write(dir.join("point.csv"), "t,v,n,s,z\n10,1.5,6.0,h,\n").expect("w");
        fit("point.csv", "v1", "point.json").expect("fits");
        let got = Schema::from_json(
            &serde_json::from_str::<Value>(
                &std::fs::read_to_string(dir.join("point.json")).expect("r"),
            )
            .expect("json"),
        )
        .expect("schema");
        assert_eq!(
            got.columns[2].kind,
            onyx_bulk::ColumnKind::Decimal { scale: 18 }
        );
        assert_eq!(
            got.columns[1].kind,
            onyx_bulk::ColumnKind::Decimal { scale: 7 }
        );
        // An added decimal column is held to the 80 characters of a stored
        // decimal too (review BULK-WIDEN-R2 finding 4): with a 60-digit
        // integer part the fit scale drops from 22 to 19 so its values fit,
        // and build then takes them; a 77-digit one cannot fit at the 4
        // places its smallest value needs, and is named.
        std::fs::write(
            dir.join("big.csv"),
            format!(
                "t,v,n,s,z,big\n11,1.5,7,i,,{}\n12,1.5,8,j,,0.0001\n",
                "1".repeat(60)
            ),
        )
        .expect("w");
        fit("big.csv", "v1", "big.json").expect("fits within 80 characters");
        let got = Schema::from_json(
            &serde_json::from_str::<Value>(
                &std::fs::read_to_string(dir.join("big.json")).expect("r"),
            )
            .expect("json"),
        )
        .expect("schema");
        assert_eq!(
            got.columns[5].kind,
            onyx_bulk::ColumnKind::Decimal { scale: 19 }
        );
        build_in(&dir, "big.json", "big.csv", "vbig", Some("v1"), true, 30)
            .expect("build takes it");
        std::fs::write(
            dir.join("huge.csv"),
            format!(
                "t,v,n,s,z,huge\n13,1.5,7,k,,{}\n14,1.5,8,l,,0.0001\n",
                "9".repeat(77)
            ),
        )
        .expect("w");
        let e = fit("huge.csv", "v1", "huge.json").expect_err("past 80");
        assert!(e.contains("1 value(s)"), "{e}");
        // A column the input lacks is never dropped.
        std::fs::write(dir.join("short.csv"), "t,v,n\n9,1.5,1\n").expect("w");
        assert!(fit("short.csv", "v1", "x.json")
            .expect_err("dropped")
            .contains("no column s"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Item 8 (review BULK-WIDEN-R1 finding 9): a node that lacks the
    /// previous version answers 409 "versions are taken in order"; `upload`
    /// then uploads that version from the folder `build --prev` recorded, and
    /// this one again.
    #[tokio::test]
    async fn upload_steps_back_when_a_node_lacks_the_previous_version() {
        let dir = scratch("stepback");
        std::fs::write(
            dir.join("s.json"),
            json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":2}],"key":"t"}).to_string(),
        )
        .expect("w");
        std::fs::write(dir.join("a.csv"), "t,v\n1,1.5\n").expect("w");
        std::fs::write(dir.join("b.csv"), "t,v\n2,2.5\n").expect("w");
        build_in(&dir, "s.json", "a.csv", "v1", None, false, 10).expect("v1");
        build_in(&dir, "s.json", "b.csv", "v2", Some("v1"), false, 20).expect("v2");
        for v in ["v1", "v2"] {
            let m = read_manifest(&dir.join(v)).expect("m");
            let r = onyx_bulk::anchor::sign(&[0x22; 32], &m.header).expect("signs");
            std::fs::write(
                dir.join(v).join("report.bin"),
                borsh::to_vec(&r).expect("ok"),
            )
            .expect("w");
        }
        let taken = r#"{"error":{"code":"bulk_not_successor","message":"versions are taken in order: upload the previous version first"}}"#;
        let (url, seen, bodies) = scripted_node_log(vec![
            (409, taken),
            (201, r#"{"missing":[]}"#),
            (201, r#"{"missing":[]}"#),
        ])
        .await;
        let pause = Duration::from_millis(1);
        upload_dir(
            &client(),
            &url,
            &dir.join("v2"),
            None,
            Duration::ZERO,
            pause,
            MAX_STEPS_BACK,
        )
        .await
        .expect("uploaded");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 3);
        let manifests: Vec<u32> = bodies
            .lock()
            .expect("lock")
            .iter()
            .map(|b| {
                let v: Value = serde_json::from_slice(b).expect("json");
                let m = onyx_bulk::wire::hex_field(&v["manifest"], "m").expect("hex");
                Manifest::from_bytes(&m).expect("manifest").header.version
            })
            .collect();
        assert_eq!(manifests, vec![2, 1, 2]);
        for v in ["v1", "v2"] {
            let u: Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join(v).join(UPLOADED)).expect("r"),
            )
            .expect("json");
            assert_eq!(u["catalog"], Value::Null);
        }
        // A folder that does not name its previous one: the refusal says what to do.
        std::fs::write(dir.join("v2").join("manifest.json"), "{}").expect("w");
        let (url, _) = scripted_node(vec![(409, taken)]).await;
        let e = upload_dir(
            &client(),
            &url,
            &dir.join("v2"),
            None,
            Duration::ZERO,
            pause,
            MAX_STEPS_BACK,
        )
        .await
        .expect_err("no previous folder");
        assert!(
            e.contains("lacks version 1") && e.contains("upload that version first"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calldata_layout() {
        let d = post_report_calldata(&[1, 2, 3], &[9; 65]);
        assert_eq!(&d[..4], &keccak256(b"postReport(bytes,bytes)").0[..4]);
        assert_eq!(d.len(), 4 + 32 * 2 + 32 + 32 + 32 + 96);
        assert_eq!(d[4 + 31], 64);
        assert_eq!(d[4 + 63], 128);
        assert_eq!(d[4 + 64 + 31], 3);
    }

    #[test]
    fn keys_document_shape() {
        let doc = keys_document("metrics.example", &[1; 20]);
        let v: Value = serde_json::from_str(&doc).expect("ok");
        assert_eq!(v["version"], 1);
        assert_eq!(v["keys"][0]["scheme"], "eip191");
    }
}
