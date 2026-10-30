//! `onyx-publish`: publish a large dataset on the Onyx test network
//! (contributor loop CL5, ADR-0059; step by step in docs/ops/bulk-publish.md).
//!
//! ```text
//! onyx-publish keygen   --out publisher.key                   # a fresh TEST key (mode 0600); prints its address only
//! onyx-publish address  --key publisher.key
//! onyx-publish keys-doc --key publisher.key --publisher <source id> [--out onyx-source-keys.json]
//! onyx-publish synth    --out data.csv --schema-out schema.json --mib 100   # clearly synthetic test rows
//! onyx-publish build    --schema schema.json --input data.csv --dataset <id> --publisher <source id>
//!                       --license CC0-1.0 [--attribution TEXT] --out DIR [--prev DIR] [--shard-mib 8]
//! onyx-publish sign     --key publisher.key --dir DIR
//! onyx-publish anchor   --dir DIR --rpc URL --chain-id N --book 0x… --gas-key FILE
//! onyx-publish upload   --dir DIR --node URL [--wait-secs N]
//! ```
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
    "usage: onyx-publish <keygen|address|keys-doc|synth|build|sign|anchor|upload> [options]\n\
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
        "sign" => sign_cmd(&opts),
        "anchor" => rt.block_on(anchor_cmd(&opts)),
        "upload" => rt.block_on(upload_cmd(&opts)),
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

fn parse_opts(a: &[String]) -> Res<Opts> {
    let mut m = BTreeMap::new();
    let mut it = a.iter();
    while let Some(k) = it.next() {
        let name = k
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected {k}"))?;
        let v = it.next().ok_or_else(|| format!("{k} needs a value"))?;
        if m.insert(name.to_string(), v.clone()).is_some() {
            return Err(format!("{k} given twice"));
        }
    }
    Ok(Opts(m))
}

impl Opts {
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
    let mut records = Records::new(input, schema.clone(), format).map_err(|e| e.to_string())?;
    let mut b = ShardBuilder::new(target);
    let mut entries: Vec<ShardEntry> = prev.map(|p| p.shards.clone()).unwrap_or_default();
    let mut new_shards = Vec::new();
    let mut rows = 0u64;
    let floor = prev.map(|p| p.header.key_max);
    while let Some((line, cells)) = records.next_record().map_err(|e| e.to_string())? {
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
    });
    std::fs::write(out.join("manifest.json"), format!("{summary:#}\n"))
        .map_err(|e| e.to_string())?;
    println!(
        "BUILT version={} rows={} new_rows={} shards={} new_shards={} bytes={} secs={:.1} root={}",
        m.header.version,
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
        return Err(format!(
            "this key already anchored another manifest for version {} of {} (sequence {}): a second one would get the key suspended. Publish the next version instead",
            m.header.version, m.header.dataset, r.payload.sequence
        ));
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

async fn send(
    c: &Client,
    method: hyper::Method,
    url: &str,
    ctype: &str,
    body: Vec<u8>,
) -> Res<(u16, Value)> {
    check_facilitator_url(url).map_err(|e| e.replacen("facilitator", "node", 1))?;
    let req = hyper::Request::builder()
        .method(method)
        .uri(url)
        .header("content-type", ctype)
        .header("content-length", body.len().to_string())
        .body(http_body_util::Full::new(hyper::body::Bytes::from(body)))
        .map_err(|e| e.to_string())?;
    let r = tokio::time::timeout(Duration::from_secs(600), c.request(req))
        .await
        .map_err(|_| "the node did not answer in time")?
        .map_err(|e| format!("node unreachable: {e}"))?;
    let status = r.status().as_u16();
    let b = http_body_util::Limited::new(r.into_body(), 4 << 20)
        .collect()
        .await
        .map_err(|e| format!("node answer unreadable: {e}"))?
        .to_bytes();
    Ok((status, serde_json::from_slice(&b).unwrap_or(Value::Null)))
}

async fn upload_cmd(o: &Opts) -> Res<()> {
    o.only(&["dir", "node", "wait-secs"])?;
    let dir = PathBuf::from(o.req("dir")?);
    let m = read_manifest(&dir)?;
    let r = read_report(&dir)?;
    let node = o.req("node")?.trim_end_matches('/').to_string();
    let wait = Duration::from_secs(o.num("wait-secs", 0)?);
    let c = client();
    let body = json!({"manifest": hx(&m.to_bytes()), "report": hx(&borsh::to_vec(&r).map_err(|e| e.to_string())?)});
    let t = Instant::now();
    let answer = loop {
        let (s, v) = send(
            &c,
            hyper::Method::POST,
            &format!("{node}/v1/bulk/manifests"),
            "application/json",
            body.to_string().into_bytes(),
        )
        .await?;
        match s {
            200 | 201 => break v,
            409 if v["error"]["code"] == "bulk_not_anchored" && t.elapsed() < wait => {
                eprintln!(
                    "waiting for the anchor to be final ({}; {:.0} s so far)",
                    v["reason"].as_str().unwrap_or("?"),
                    t.elapsed().as_secs_f64()
                );
                tokio::time::sleep(Duration::from_secs(15).min(wait)).await;
            }
            _ => return Err(format!("manifest refused ({s}): {v}")),
        }
    };
    let missing = answer["missing"].as_array().cloned().unwrap_or_default();
    let (mut sent, mut bytes) = (0u64, 0u64);
    let manifest_time = t.elapsed();
    let tu = Instant::now();
    for e in &missing {
        let root = e["root"].as_str().ok_or("node answer: missing[].root")?;
        let h = onyx_bulk::wire::hash_field(&e["root"], "root").map_err(|e| e.to_string())?;
        let p = shard_path(&dir, &h);
        let file = std::fs::read(&p).map_err(|err| {
            format!("shard {root} is not in {} ({err}); upload it from the folder of the version that added it", p.display())
        })?;
        let url = format!(
            "{node}/v1/bulk/{}/{}/shards/{root}",
            m.header.publisher.0, m.header.dataset
        );
        let n = file.len() as u64;
        let (s, v) = send(
            &c,
            hyper::Method::PUT,
            &url,
            "application/octet-stream",
            file,
        )
        .await?;
        if s != 200 && s != 201 {
            return Err(format!("shard {root} refused ({s}): {v}"));
        }
        sent += 1;
        bytes += n;
    }
    println!(
        "UPLOADED version={} shards_sent={sent} bytes_sent={bytes} manifest_secs={:.1} shard_secs={:.1} root={}",
        m.header.version,
        manifest_time.as_secs_f64(),
        tu.elapsed().as_secs_f64(),
        m.header.root()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
