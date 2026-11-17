//! Tests of `apply`, `verify-application`, `doctor` and `request`
//! (ADR-0066 slice 1; THREATS PUBON).

use super::*;
use crate::{build_cmd, keygen, keys_doc, parse_opts, sign_cmd};
use onyx_fetch::fixture::{FixedResolver, FixtureServer, LoopbackDial, Reply};
use std::sync::Arc;

const ID: &str = "river-levels.open";
const ORIGIN: &str = "data.example.org/river-levels";
const CHAIN: &str = "84532";
const BOOK: &str = "0xb0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";
const T0: u64 = 1_794_873_600;

fn opts(a: &[&str]) -> Opts {
    parse_opts(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).expect("opts")
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

/// A publisher's working folder under the temp dir.
struct Folder(PathBuf);

/// The folder holds generated key files: it goes when the test ends, also
/// a failed one (review APPLY-R1 L5).
impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Folder {
    fn new(name: &str) -> Folder {
        let d = std::env::temp_dir().join(format!("onyx-onboard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("dir");
        Folder(d)
    }
    fn p(&self, rel: &str) -> String {
        self.0.join(rel).display().to_string()
    }
    fn keygen(&self, name: &str) {
        keygen(&opts(&["--out", &self.p(name)])).expect("keygen");
    }
    /// The hex digits of a key file this test made (to assert they are never printed).
    fn secret(&self, name: &str) -> String {
        let t = std::fs::read_to_string(self.p(name)).expect("key");
        t.trim().trim_start_matches("0x").to_string()
    }
    fn address(&self, name: &str) -> String {
        hx(&address_of(&read_key(&self.p(name)).expect("key")).expect("address"))
    }
    fn keys_doc(&self, key: &str) {
        keys_doc(&opts(&[
            "--key",
            &self.p(key),
            "--publisher",
            ID,
            "--out",
            &self.p("site/.well-known/onyx-source-keys.json"),
        ]))
        .expect("keys-doc");
    }
    /// Both keys in the keys document: the working key's file and the
    /// spare's address (its key file may be offline).
    fn keys_doc_both(&self, a: &str, b: &str) {
        keys_doc(&opts(&[
            "--key",
            &self.p(a),
            "--publisher",
            ID,
            "--also-address",
            &self.address(b),
            "--out",
            &self.p("site/.well-known/onyx-source-keys.json"),
        ]))
        .expect("keys-doc");
    }
    /// A book with one unrelated key, and an empty record folder: what a
    /// reviewer hands in when nothing stands against the application.
    fn lists(&self) -> (String, String) {
        let reg = self.write_json(
            "book.json",
            &json!([{"publisher": "lake-levels.open", "origin": "lakes.example.net",
                "signer": "0x00000000000000000000000000000000000000bb", "state": "active"}]),
        );
        std::fs::create_dir_all(self.p("no-records")).expect("dir");
        (reg, self.p("no-records"))
    }
    /// `build` of `rows` daily rows into `out`.
    fn build(
        &self,
        out: &str,
        rows: u64,
        created: u64,
        prev: Option<&str>,
        dataset: &str,
    ) -> Res<()> {
        std::fs::write(
            self.p("schema.json"),
            r#"{"columns": [{"name": "t", "kind": "int"}, {"name": "level", "kind": "decimal", "scale": 2}], "key": "t"}"#,
        )
        .expect("schema");
        let start = if prev.is_some() {
            1_800_000_000
        } else {
            1_700_000_000u64
        };
        let mut csv = String::from("t,level\n");
        for i in 0..rows {
            csv.push_str(&format!("{},{}.25\n", start + i * 60, i % 900));
        }
        std::fs::write(self.p("data.csv"), csv).expect("csv");
        let (schema, input, out, site, created) = (
            self.p("schema.json"),
            self.p("data.csv"),
            self.p(out),
            self.p("site"),
            created.to_string(),
        );
        let mut a = vec![
            "--schema",
            &schema,
            "--input",
            &input,
            "--dataset",
            dataset,
            "--publisher",
            ID,
            "--license",
            "CC0-1.0",
            "--out",
            &out,
            "--created",
            &created,
            "--site",
            &site,
            "--shard-mib",
            "1",
        ];
        let prev = prev.map(|p| self.p(p));
        if let Some(p) = &prev {
            a.extend(["--prev", p]);
        }
        build_cmd(&opts(&a))
    }
    fn sign(&self, key: &str, dir: &str) {
        sign_cmd(&opts(&["--key", &self.p(key), "--dir", &self.p(dir)])).expect("sign");
    }
    fn apply(&self, key: &str, now: u64, extra: &[&str]) -> Res<String> {
        let (k, site, now) = (self.p(key), self.p("site"), now.to_string());
        let mut a = vec![
            "--key",
            &k,
            "--publisher",
            ID,
            "--origin",
            ORIGIN,
            "--site",
            &site,
            "--data",
            "Daily water levels of 40 public gauges, from the publisher's own sensors.",
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--now",
            &now,
        ];
        a.extend(extra);
        apply(&opts(&a))
    }
    /// Keys, keys document, a signed sample and the application.
    fn complete(name: &str) -> Folder {
        let f = Folder::new(name);
        f.keygen("publisher.key");
        f.keygen("spare.key");
        f.keys_doc("publisher.key");
        f.build("site/sample", 3, T0 - 60, None, "river-levels.daily")
            .expect("build");
        f.sign("publisher.key", "site/sample");
        f.apply("publisher.key", T0, &[]).expect("apply");
        f
    }
    fn verify(&self, now: u64, extra: &[&str]) -> (Value, u8) {
        let (site, now) = (self.p("site"), now.to_string());
        let mut a = vec![
            "--dir",
            &site,
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--now",
            &now,
        ];
        a.extend(extra);
        // Unless the test hands in its own: the book and the records, read.
        let (reg, none) = self.lists();
        if !extra.contains(&"--registered") && !extra.contains(&"--no-book") {
            a.extend(["--registered", &reg]);
        }
        if !extra.contains(&"--decisions") && !extra.contains(&"--no-book") {
            a.extend(["--decisions", &none]);
        }
        a.retain(|x| *x != "--no-book");
        let o = opts(&a);
        let rv = review(&o).expect("review");
        block(verify(&Source::Dir(PathBuf::from(&site)), &rv))
    }
    fn doctor(&self, extra: &[&str], live: Option<&Source>) -> (String, bool, Vec<Line>) {
        let (dir, now) = (self.p(""), T0.to_string());
        let mut a = vec!["--dir", &dir, "--now", &now];
        a.extend(extra);
        let lines = block(doctor(&opts(&a), live)).expect("doctor");
        let (text, good) = doctor_text(&lines);
        (text, good, lines)
    }
    fn write_json(&self, rel: &str, v: &Value) -> String {
        std::fs::write(self.p(rel), v.to_string()).expect("write");
        self.p(rel)
    }
    /// Every file of the site folder as a route of the origin.
    fn routes(&self) -> Vec<(String, Reply)> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Reply)>) {
            for e in std::fs::read_dir(dir).expect("dir") {
                let p = e.expect("entry").path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    let rel = p.strip_prefix(base).expect("rel").display().to_string();
                    out.push((
                        format!("/river-levels/{rel}"),
                        Reply::json(std::fs::read(&p).expect("file")),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        let site = self.0.join("site");
        walk(&site, &site, &mut out);
        out
    }
}

fn web(server: &FixtureServer) -> Source {
    Source::Web(
        server.fetch(fetch_limits()),
        Origin::parse(ORIGIN).expect("origin"),
    )
}

fn state<'a>(lines: &'a [Line], step: &str) -> (&'static str, &'a str) {
    let l = lines
        .iter()
        .find(|l| l.step == step)
        .unwrap_or_else(|| panic!("no line {step}"));
    (l.state, l.next.as_deref().unwrap_or(""))
}

#[test]
fn doctor_names_each_missing_step_and_sends_nothing() {
    // An empty folder: every step is missing, each with its next command.
    let f = Folder::new("doctor");
    let (text, good, lines) = f.doctor(&[], None);
    assert!(!good);
    for (step, next) in [
        ("key", "onyx-publish keygen --out"),
        ("spare key", "onyx-publish keygen --out"),
        ("publisher id", "onyx-publish doctor --publisher"),
        ("origin", "onyx-publish doctor --publisher"),
        ("keys document", "onyx-publish keys-doc --key"),
        ("sample", "onyx-publish build --schema"),
        ("application", "onyx-publish apply --key"),
        ("origin files", "onyx-publish doctor --live"),
    ] {
        let (st, n) = state(&lines, step);
        assert_eq!(st, "missing", "{step}\n{text}");
        assert!(n.starts_with(next), "{step}: {n}");
    }
    assert!(text.contains("nothing was sent"), "{text}");

    // Step by step, the first missing line moves on.
    f.keygen("publisher.key");
    f.keygen("spare.key");
    let with = ["--publisher", ID, "--origin", ORIGIN];
    let (_, _, lines) = f.doctor(&with, None);
    for step in ["key", "spare key", "publisher id", "origin"] {
        assert_eq!(state(&lines, step).0, "ok", "{step}");
    }
    assert_eq!(state(&lines, "keys document").0, "missing");
    f.keys_doc("publisher.key");
    f.build("site/sample", 3, T0 - 60, None, "river-levels.daily")
        .expect("build");
    let (_, _, lines) = f.doctor(&with, None);
    assert_eq!(state(&lines, "keys document").0, "ok");
    let (st, next) = state(&lines, "sample signature");
    assert_eq!(st, "missing");
    assert!(next.starts_with("onyx-publish sign --key"), "{next}");
    f.sign("publisher.key", "site/sample");
    let (_, _, lines) = f.doctor(&with, None);
    assert_eq!(state(&lines, "sample").0, "ok");
    let (st, next) = state(&lines, "application");
    assert_eq!(st, "missing");
    assert!(
        next.contains(&format!("--publisher {ID} --origin {ORIGIN}")),
        "{next}"
    );

    // A bad name is a problem with its reason.
    let (text, _, lines) = f.doctor(
        &[
            "--publisher",
            "onyx-official.data",
            "--origin",
            "someone.github.io/x",
        ],
        None,
    );
    assert_eq!(state(&lines, "publisher id").0, "problem", "{text}");
    assert_eq!(state(&lines, "origin").0, "problem", "{text}");
    // A key file others can read is a problem, and its content is not shown.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(f.p("spare.key"), std::fs::Permissions::from_mode(0o644))
            .expect("chmod");
        let (text, _, lines) = f.doctor(&with, None);
        let (st, next) = state(&lines, "spare key");
        assert_eq!(st, "problem");
        assert!(next.starts_with("chmod 600"), "{next}");
        assert!(!text.contains(&f.secret("spare.key")));
        std::fs::set_permissions(f.p("spare.key"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
    }

    // Complete on disk: only the origin is left, and the next command says so.
    f.apply("publisher.key", T0, &[]).expect("apply");
    let (text, good, lines) = f.doctor(&[], None);
    assert!(!good, "{text}");
    for l in &lines {
        let want = match l.step {
            "origin files" => "missing",
            "status" | "book" | "gas and node" => "skipped",
            _ => "ok",
        };
        assert_eq!(l.state, want, "{}\n{text}", l.step);
    }

    // Complete and live at the origin: nothing missing, and the pointer's two lines.
    let server = FixtureServer::start(f.routes()).expect("server");
    let (text, good, lines) = f.doctor(&["--spare-offline"], Some(&web(&server)));
    assert!(good, "{text}");
    assert_eq!(state(&lines, "origin files").0, "ok", "{text}");
    let keccak = keccak256(
        &std::fs::read(f.p("site/.well-known/onyx-publisher-application.json")).expect("app"),
    );
    assert!(
        text.contains(&format!("two lines: {ORIGIN} and {keccak}")),
        "{text}"
    );
    // It only read: five GETs, nothing else.
    let seen = server.seen().requests;
    assert_eq!(seen.len(), 5, "{seen:?}");
    assert!(seen.iter().all(|r| r.starts_with("GET ")), "{seen:?}");
    // A file at the origin that differs from the local one is a problem.
    std::fs::write(f.p("site/.well-known/onyx-source-keys.json"), "{}").expect("write");
    let other = FixtureServer::start(f.routes()).expect("server");
    f.keys_doc("publisher.key");
    let (text, good, lines) = f.doctor(&[], Some(&web(&other)));
    assert!(!good);
    assert_eq!(state(&lines, "origin files").0, "problem", "{text}");
    // No output ever holds a key.
    for key in ["publisher.key", "spare.key"] {
        assert!(!text.contains(&f.secret(key)));
    }
}

#[test]
fn apply_writes_the_application_and_verify_passes_it() {
    let f = Folder::complete("apply");
    let path = f.p("site/.well-known/onyx-publisher-application.json");
    let text = std::fs::read_to_string(&path).expect("application");
    assert!(text.len() < 4096);
    let v: Value = serde_json::from_str(&text).expect("json");
    let mut keys: Vec<&str> = v
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "asks",
            "book",
            "chain_id",
            "cosignature",
            "created_unix",
            "data",
            "expires_unix",
            "keys_document_keccak",
            "origin",
            "publisher",
            "sample",
            "signature",
            "signer",
            "version"
        ]
    );
    assert_eq!(v["chain_id"], 84_532);
    assert_eq!(v["book"], BOOK);
    assert_eq!(v["signer"], f.address("publisher.key"));
    assert_eq!(v["expires_unix"], T0 + 6 * 86_400);
    assert_eq!(v["asks"], json!({"quota_gib": 1, "datasets": 16}));
    assert_eq!(v["sample"]["path"], "sample/");
    assert_eq!(v["cosignature"], Value::Null);

    let (report, code) = f.verify(T0 + 60, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["ok"], true);
    assert_eq!(report["standing"], "first");
    assert_eq!(report["sample"]["rights"]["license"], "CC0-1.0");
    assert_eq!(report["sample"]["rows"], 3);
    assert_eq!(report["forThePerson"].as_array().expect("lines").len(), 7);
    let keccak = keccak256(text.as_bytes()).to_string();
    assert_eq!(report["applicationKeccak"], keccak.as_str());
    assert_eq!(f.verify(T0, &["--hash", &keccak, "--origin", ORIGIN]).1, 0);

    let refused = |r: (Value, u8), code: &str| {
        assert_eq!(r.1, 1, "{:#}", r.0);
        assert_eq!(r.0["ok"], false);
        assert_eq!(r.0["refused"]["code"], code, "{:#}", r.0);
    };
    // The pointer names another file or origin.
    refused(
        f.verify(T0, &["--hash", &format!("0x{}", "11".repeat(32))]),
        "malformed",
    );
    refused(
        f.verify(T0, &["--origin", "other.example.org"]),
        "malformed",
    );
    // Another chain, another book, expired.
    let (site, t, lists) = (f.p("site"), T0.to_string(), f.lists());
    let other = |chain: &str, book: &str| {
        let o = opts(&[
            "--dir",
            &site,
            "--chain-id",
            chain,
            "--book",
            book,
            "--now",
            &t,
            "--registered",
            &lists.0,
            "--decisions",
            &lists.1,
        ]);
        block(verify(
            &Source::Dir(PathBuf::from(&site)),
            &review(&o).expect("review"),
        ))
    };
    refused(other("8453", BOOK), "wrong-network");
    refused(
        other(CHAIN, "0xb1b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0"),
        "wrong-network",
    );
    refused(f.verify(T0 + 6 * 86_400 + 1, &[]), "expired");
    // The book's keys: an origin that overlaps a registered one, a look-alike id.
    let reg = |publisher: &str, origin: &str| {
        f.write_json(
            "registered.json",
            &json!([{"publisher": publisher, "origin": origin,
            "signer": "0x00000000000000000000000000000000000000aa", "state": "active"}]),
        )
    };
    refused(
        f.verify(
            T0,
            &["--registered", &reg("lake-levels.open", "data.example.org")],
        ),
        "origin-overlap",
    );
    refused(
        f.verify(
            T0,
            &[
                "--registered",
                &reg("river_levels.open", "other.example.org"),
            ],
        ),
        "name-taken",
    );
    assert_eq!(
        f.verify(
            T0,
            &[
                "--registered",
                &reg("lake-levels.open", "data.example.org/lakes")
            ]
        )
        .1,
        0
    );
    // A refused record blocks the signer until its report is stale, then not.
    let until = T0 - 60 + 7 * 86_400 + 300;
    std::fs::create_dir_all(f.p("decisions/river-levels.open")).expect("dir");
    f.write_json("decisions/river-levels.open/decision.json", &json!({"state": "refused",
        "reason": "rights-unclear", "signer": f.address("publisher.key"), "blocks_signer_until": until}));
    let decisions = f.p("decisions");
    refused(
        f.verify(T0 + 100, &["--decisions", &decisions]),
        "signer-blocked",
    );
    let later = Folder::complete("apply-later");
    let (_, blocked) = later.verify(T0, &["--decisions", &decisions]);
    assert_eq!(blocked, 0, "another key is never blocked");

    // A tampered application: one changed word of the data sentence.
    std::fs::write(&path, text.replace("Daily", "Hourly")).expect("write");
    refused(f.verify(T0, &[]), "bad-signature");
    // An added field.
    std::fs::write(&path, text.replacen('{', "{\"contact\": \"x\", ", 1)).expect("write");
    refused(f.verify(T0, &[]), "malformed");
    std::fs::write(&path, &text).expect("write");
    // A changed row of the sample, a changed keys document.
    let shard = std::fs::read_dir(f.p("site/sample/shards"))
        .expect("dir")
        .next()
        .expect("one")
        .expect("entry")
        .path();
    let good = std::fs::read(&shard).expect("shard");
    let mut bad = good.clone();
    let n = bad.len() - 1;
    bad[n] ^= 1;
    std::fs::write(&shard, bad).expect("write");
    refused(f.verify(T0, &[]), "sample-failed");
    std::fs::write(&shard, good).expect("write");
    let doc = f.p("site/.well-known/onyx-source-keys.json");
    let good = std::fs::read_to_string(&doc).expect("doc");
    std::fs::write(&doc, format!("{good} ")).expect("write");
    refused(f.verify(T0, &[]), "keys-document");
    std::fs::write(&doc, good).expect("write");
    assert_eq!(f.verify(T0, &[]).1, 0);
}

#[test]
fn a_second_key_is_applied_for_with_a_cosignature() {
    let f = Folder::complete("second");
    let now = T0 + 30 * 86_400;
    let reg = f.write_json(
        "registered.json",
        &json!([{"publisher": ID, "origin": ORIGIN,
        "signer": f.address("publisher.key"), "state": "active"}]),
    );
    // The spare applies alone: it must be in the keys document, and even
    // then the origin alone gives nothing.
    assert!(f.apply("spare.key", now, &[]).is_err());
    f.keys_doc_both("publisher.key", "spare.key");
    f.apply("spare.key", now, &["--reapply"])
        .expect_err("the sample is the first key's");
    // Cosigned by the active key.
    let out = f
        .apply("spare.key", now, &["--cosign-key", &f.p("publisher.key")])
        .expect("apply");
    assert!(out.starts_with("APPLIED "), "{out}");
    let (report, code) = f.verify(now, &["--registered", &reg]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["standing"], "second-key");
    assert_eq!(report["cosigner"], f.address("publisher.key"));
    // Without the cosignature: refused.
    let path = f.p("site/.well-known/onyx-publisher-application.json");
    let text = std::fs::read_to_string(&path).expect("app");
    let mut v: Value = serde_json::from_str(&text).expect("json");
    v["cosignature"] = Value::Null;
    std::fs::write(&path, v.to_string()).expect("write");
    let (report, code) = f.verify(now, &["--registered", &reg]);
    assert_eq!(
        (code, report["refused"]["code"].as_str()),
        (1, Some("no-cosignature")),
        "{report:#}"
    );
    // With it, but the cosigning key retired.
    std::fs::write(&path, &text).expect("write");
    let retired = f.write_json(
        "retired.json",
        &json!([{"publisher": ID, "origin": ORIGIN,
        "signer": f.address("publisher.key"), "state": "retired"}]),
    );
    let (report, code) = f.verify(now, &["--registered", &retired]);
    assert_eq!(
        (code, report["refused"]["code"].as_str()),
        (1, Some("no-cosignature")),
        "{report:#}"
    );
    // Doctor says which kind of application this is.
    let (text, _, lines) = f.doctor(&["--registered", &reg], None);
    assert_eq!(state(&lines, "book").0, "ok");
    assert!(text.contains("further key"), "{text}");
    for key in ["publisher.key", "spare.key"] {
        assert!(!out.contains(&f.secret(key)));
        assert!(!text.contains(&f.secret(key)));
        assert!(!report.to_string().contains(&f.secret(key)));
    }
}

#[test]
fn a_sample_must_be_version_one_and_one_shard() {
    // Two shards: about 1.3 MiB of rows cut at 1 MiB.
    let f = Folder::new("two-shards");
    f.keygen("publisher.key");
    f.keys_doc("publisher.key");
    f.build("site/sample", 90_000, T0 - 60, None, "river-levels.daily")
        .expect("build");
    f.sign("publisher.key", "site/sample");
    let e = f.apply("publisher.key", T0, &[]).expect_err("two shards");
    assert!(e.contains("one shard"), "{e}");
    let (text, _, lines) = f.doctor(&["--publisher", ID, "--origin", ORIGIN], None);
    assert_eq!(state(&lines, "sample").0, "problem", "{text}");

    // Version 2.
    let f = Folder::new("version-two");
    f.keygen("publisher.key");
    f.keys_doc("publisher.key");
    f.build("v1", 3, T0 - 120, None, "river-levels.daily")
        .expect("build");
    f.build("site/sample", 3, T0 - 60, Some("v1"), "river-levels.daily")
        .expect("build");
    f.sign("publisher.key", "site/sample");
    let e = f.apply("publisher.key", T0, &[]).expect_err("version 2");
    assert!(e.contains("version 2") || e.contains("shards"), "{e}");

    // A sample built more than an hour before the application.
    let f = Folder::new("old-sample");
    f.keygen("publisher.key");
    f.keys_doc("publisher.key");
    f.build("site/sample", 3, T0 - 3_700, None, "river-levels.daily")
        .expect("build");
    f.sign("publisher.key", "site/sample");
    let e = f.apply("publisher.key", T0, &[]).expect_err("old");
    assert!(e.contains("hour"), "{e}");
    // After an expiry the same folder is used again, on the applicant's word
    // (the reviewer's tool wants the expired record).
    f.apply("publisher.key", T0, &["--reapply"])
        .expect("reapply");
    let (report, code) = f.verify(T0, &[]);
    assert_eq!(
        (code, report["refused"]["code"].as_str()),
        (1, Some("sample-failed")),
        "{report:#}"
    );
    let root = report["applicationKeccak"]
        .as_str()
        .expect("keccak")
        .to_string();
    assert!(root.starts_with("0x"));
    let app: Value = serde_json::from_slice(
        &std::fs::read(f.p("site/.well-known/onyx-publisher-application.json")).expect("app"),
    )
    .expect("json");
    let expired = f.write_json("expired.json", &json!({"state": "expired",
        "signer": f.address("publisher.key"), "sample_manifest_root": app["sample"]["manifest_root"]}));
    let (report, code) = f.verify(T0, &["--decisions", &expired]);
    assert_eq!(
        (code, report["standing"].as_str()),
        (0, Some("reapply")),
        "{report:#}"
    );

    // `build` refuses a second version 1 of the dataset the application
    // carries; another dataset, or a later version, is built.
    let e = f
        .build("again", 3, T0, None, "river-levels.daily")
        .expect_err("second version 1");
    assert!(e.contains("equivocation"), "{e}");
    assert!(!Path::new(&f.p("again")).join("manifest.bin").exists());
    f.build("other", 3, T0, None, "river-levels.hourly")
        .expect("another dataset");
    f.build("v2", 3, T0, Some("site/sample"), "river-levels.daily")
        .expect("version 2");
}

#[test]
fn verify_refuses_a_redirect_a_private_address_and_an_oversized_sample() {
    let f = Folder::complete("fetch");
    let lists = f.lists();
    let rv = || {
        review(&opts(&[
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--now",
            &T0.to_string(),
            "--origin",
            ORIGIN,
            "--registered",
            &lists.0,
            "--decisions",
            &lists.1,
        ]))
        .expect("review")
    };
    // The honest origin passes, with the fetched bytes kept for the artifact.
    let server = FixtureServer::start(f.routes()).expect("server");
    let mut keep = rv();
    keep.save = Some(f.0.join("artifact"));
    let (report, code) = block(verify(&web(&server), &keep));
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["source"], "origin");
    assert_eq!(
        std::fs::read(f.p("artifact/.well-known/onyx-publisher-application.json")).expect("kept"),
        std::fs::read(f.p("site/.well-known/onyx-publisher-application.json")).expect("local")
    );
    assert!(server
        .seen()
        .requests
        .iter()
        .all(|r| r.starts_with("GET /river-levels/")));

    let swap = |path_end: &str, reply: Reply| {
        let mut routes = f.routes();
        let r = routes
            .iter_mut()
            .find(|(p, _)| p.ends_with(path_end))
            .expect("route");
        r.1 = reply;
        FixtureServer::start(routes).expect("server")
    };
    let refused = |server: &FixtureServer, check: &str, word: &str| {
        let (report, code) = block(verify(&web(server), &rv()));
        assert_eq!(code, 1, "{report:#}");
        assert_eq!(report["refused"]["check"], check, "{report:#}");
        let why = report["refused"]["reason"].as_str().expect("reason");
        assert!(why.contains(word), "{why}");
    };
    // A redirect is never followed: not for the application, not for the sample.
    let to = [("location", "https://internal.example.org/x")];
    refused(
        &swap("onyx-publisher-application.json", Reply::status(302, &to)),
        "application-file",
        "redirect",
    );
    refused(
        &swap("manifest.bin", Reply::status(301, &to)),
        "sample",
        "redirect",
    );
    // A shard over 8 MiB is refused by its declared length, before it is read.
    let big = Reply::json(vec![0u8; (MAX_SAMPLE_BYTES + 1) as usize]);
    let s = swap(".bin", big.clone());
    let (report, code) = block(verify(&web(&s), &rv()));
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(report["refused"]["check"], "sample", "{report:#}");
    let mut routes = f.routes();
    let shard = routes
        .iter_mut()
        .find(|(p, _)| p.contains("/shards/"))
        .expect("shard");
    shard.1 = big;
    refused(
        &FixtureServer::start(routes).expect("server"),
        "sample",
        "over",
    );
    // An application over 4 KiB.
    refused(
        &swap(
            "onyx-publisher-application.json",
            Reply::json(vec![b' '; 5_000]),
        ),
        "application-file",
        "over 4096",
    );
    // A missing file is a refusal.
    refused(
        &swap("onyx-source-keys.json", Reply::status(404, &[])),
        "keys-document",
        "404",
    );

    // A name that resolves to a private address is refused before any
    // connection: the server sees nothing.
    for ip in ["10.0.0.7", "127.0.0.1", "169.254.169.254", "192.168.1.1"] {
        let quiet = FixtureServer::start(f.routes()).expect("server");
        let private = Source::Web(
            onyx_fetch::HttpsFetch::with_transport(
                Arc::new(FixedResolver::new(vec![ip.parse().expect("ip")])),
                Arc::new(LoopbackDial::new(quiet.addr())),
                fetch_limits(),
            ),
            Origin::parse(ORIGIN).expect("origin"),
        );
        let (report, code) = block(verify(&private, &rv()));
        assert_eq!(code, 1, "{ip} {report:#}");
        assert_eq!(report["refused"]["check"], "application-file");
        assert!(quiet.seen().requests.is_empty(), "{ip}");
    }
    // An origin that cannot be reached decides nothing: exit 3, not a refusal.
    let gone = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr")
    };
    let dead = Source::Web(
        onyx_fetch::HttpsFetch::with_transport(
            Arc::new(FixedResolver::new(vec![
                onyx_fetch::fixture::FIXTURE_PUBLIC_IP,
            ])),
            Arc::new(LoopbackDial::new(gone)),
            fetch_limits(),
        ),
        Origin::parse(ORIGIN).expect("origin"),
    );
    let (report, code) = block(verify(&dead, &rv()));
    assert_eq!(code, 3, "{report:#}");
    assert_eq!(report["decided"], false);
    // The fetch itself: https only, no address literal, no other port.
    assert_eq!(fetch_limits().max_bytes as u64, MAX_SAMPLE_BYTES);
    assert!(Origin::parse("10.0.0.7/x").is_err());
    assert!(Origin::parse("data.example.org:8443").is_err());
}

#[test]
fn a_request_is_signed_and_never_prints_a_key() {
    let f = Folder::complete("request");
    let (k, site, now) = (f.p("publisher.key"), f.p("site"), T0.to_string());
    let base = [
        "--key",
        &k,
        "--publisher",
        ID,
        "--origin",
        ORIGIN,
        "--chain-id",
        CHAIN,
        "--book",
        BOOK,
        "--site",
        &site,
        "--now",
        &now,
    ];
    let run = |extra: &[&str]| {
        let mut a = base.to_vec();
        a.extend(extra);
        request(&opts(&a))
    };
    let out = run(&["--kind", "key-stolen", "--at", &(T0 - 3_600).to_string()]).expect("request");
    assert!(out.starts_with("REQUEST kind=key-stolen"), "{out}");
    assert!(out.contains("both proofs"), "{out}");
    let bytes = std::fs::read(f.p("site/.well-known/onyx-publisher-request.json")).expect("file");
    let r = Request::from_json(&bytes).expect("parse");
    assert_eq!(hx(&r.signer), f.address("publisher.key"));
    // The book holds the key; the keys document must no longer list it.
    let reg = [RegisteredKey {
        publisher: ID.into(),
        origin: ORIGIN.into(),
        signer: r.signer,
        life: onyx_bulk::application::KeyLife::Active,
    }];
    let net = Network {
        chain_id: 84_532,
        book: parse_address(BOOK).expect("book"),
    };
    let listed = std::fs::read(f.p("site/.well-known/onyx-source-keys.json")).expect("doc");
    let ctx = |doc| RequestContext {
        keys_document: doc,
        registered: &reg,
        application: None,
        last_sequence: None,
    };
    assert!(r.verify(&net, T0, &ctx(&listed)).is_err());
    let cleared = format!("{}\n", json!({"version": 1, "source": ID, "keys": []}));
    assert_eq!(r.verify(&net, T0, &ctx(cleared.as_bytes())), Ok(()));
    for kind in [
        vec!["--kind", "withdraw"],
        vec!["--kind", "raise", "--quota-gib", "4", "--datasets", "64"],
        vec!["--kind", "retire-key", "--at", "1"],
    ] {
        let o = run(&kind).expect("request");
        assert!(!o.contains(&f.secret("publisher.key")));
    }
    assert!(run(&["--kind", "raise"]).is_err());
    assert!(run(&["--kind", "delete-everything"]).is_err());
    assert!(!out.contains(&f.secret("publisher.key")));
    assert!(!String::from_utf8_lossy(&bytes).contains(&f.secret("publisher.key")));
    // Nor does the application, the keys document or apply's output.
    let applied = f.apply("publisher.key", T0, &[]).expect("apply");
    assert!(!applied.contains(&f.secret("publisher.key")));
    for file in [
        "site/.well-known/onyx-publisher-application.json",
        "site/.well-known/onyx-source-keys.json",
        "site/sample/manifest.json",
    ] {
        assert!(
            !std::fs::read_to_string(f.p(file))
                .expect("file")
                .contains(&f.secret("publisher.key")),
            "{file}"
        );
    }
    // The key files are private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for key in ["publisher.key", "spare.key"] {
            let mode = std::fs::metadata(f.p(key))
                .expect("key")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{key}");
        }
    }
}

// ---------------------------------------------------------------- review APPLY-R1

/// One signature digit of a public application changed, as text.
fn break_signature(app: &str) -> String {
    let at = app.find("\"signature\": \"0x").expect("signature") + 16;
    let digit = if &app[at..at + 1] == "0" { "1" } else { "0" };
    format!("{}{digit}{}", &app[..at], &app[at + 1..])
}

/// APPLY-R1 H1: a copy of someone's public application with a broken
/// signature is refused without naming that someone as its signer and
/// without a block time, so a reviewer who follows the guide records
/// nothing against the copied key. A block exists only once the signature
/// and the pointer's hash passed.
#[test]
fn a_forged_refusal_blocks_nobody() {
    let victim = Folder::complete("r1-victim");
    let victim_addr = victim.address("publisher.key");
    let rel = "site/.well-known/onyx-publisher-application.json";
    let app = std::fs::read_to_string(victim.p(rel)).expect("app");

    // The attacker's copy: no key of the victim is needed.
    let forged = Folder::new("r1-forger");
    std::fs::create_dir_all(forged.p("site/.well-known")).expect("dir");
    let broken = break_signature(&app);
    std::fs::write(forged.p(rel), &broken).expect("write");
    std::fs::copy(
        victim.p("site/.well-known/onyx-source-keys.json"),
        forged.p("site/.well-known/onyx-source-keys.json"),
    )
    .expect("copy");
    // The attacker's pointer names its own file, so the hash matches.
    let hash = keccak256(broken.as_bytes()).to_string();
    let (report, code) = forged.verify(T0, &["--hash", &hash, "--origin", ORIGIN]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(report["refused"]["code"], "bad-signature");
    assert!(report["signer"].is_null(), "{report:#}");
    assert!(report["publisher"].is_null(), "{report:#}");
    assert!(report["blocksSignerUntil"].is_null(), "{report:#}");
    assert!(!report.to_string().contains(&victim_addr), "{report:#}");

    // The victim's own application still passes: nothing was recorded.
    let own = keccak256(app.as_bytes()).to_string();
    let (report, code) = victim.verify(T0 + 60, &["--hash", &own]);
    assert_eq!(code, 0, "{report:#}");
    // A report names a block time only with the signature and the pointer:
    // from the sample's creation time once the sample was read ...
    assert_eq!(report["blocksSignerUntil"], T0 - 60 + 7 * 86_400 + 300);
    assert_eq!(report["signer"], json!(victim_addr));
    // ... from the application's own time when the refusal comes earlier ...
    let taken = victim.write_json(
        "taken.json",
        &json!([{"publisher": "river_levels.open", "origin": "other.example.org",
            "signer": "0x00000000000000000000000000000000000000aa", "state": "active"}]),
    );
    let (report, code) = victim.verify(T0, &["--hash", &own, "--registered", &taken]);
    assert_eq!(
        (code, report["refused"]["code"].as_str()),
        (1, Some("name-taken"))
    );
    assert_eq!(report["blocksSignerUntil"], T0 + 7 * 86_400 + 300);
    // ... and never without the pointer's hash, or with a wrong one.
    let (report, _) = victim.verify(T0, &["--registered", &taken]);
    assert!(report["blocksSignerUntil"].is_null(), "{report:#}");
    let wrong = format!("0x{}", "22".repeat(32));
    let (report, code) = victim.verify(T0, &["--hash", &wrong]);
    assert_eq!(code, 1);
    assert!(report["blocksSignerUntil"].is_null(), "{report:#}");
}

/// APPLY-R1 M3: exit 0 and "ok": true only when the book and the records
/// were read; an empty list is not a read book.
#[test]
fn no_pass_without_the_book_and_the_records() {
    let f = Folder::complete("r1-nobook");
    let (reg, none) = f.lists();
    let (report, code) = f.verify(T0, &["--no-book"]);
    assert_eq!(code, EXIT_UNREAD, "{report:#}");
    assert_eq!(report["ok"], false);
    assert_eq!(report["decided"], false);
    assert_eq!(report["passedWhatWasChecked"], true);
    assert!(report["registered"].is_null() && report["records"].is_null());
    assert!(report["notChecked"].to_string().contains("the book"));
    // An empty list: still not read, and the report says how many keys it saw.
    let empty = f.write_json("empty.json", &json!([]));
    let (report, code) = f.verify(T0, &["--registered", &empty]);
    assert_eq!(code, EXIT_UNREAD, "{report:#}");
    assert_eq!(report["registered"]["count"], 0);
    assert!(
        report["notChecked"].to_string().contains("the book"),
        "{report:#}"
    );
    // The book without the records, the records without the book.
    assert_eq!(
        f.verify(T0, &["--no-book", "--registered", &reg]).1,
        EXIT_UNREAD
    );
    assert_eq!(
        f.verify(T0, &["--no-book", "--decisions", &none]).1,
        EXIT_UNREAD
    );
    // Both: a pass, with the list's count and hash echoed.
    let (report, code) = f.verify(T0, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["ok"], true);
    assert_eq!(report["registered"]["count"], 1);
    let keccak = keccak256(&std::fs::read(&reg).expect("list")).to_string();
    assert_eq!(report["registered"]["keccak"], keccak.as_str());
    assert_eq!(report["records"]["count"], 0);
    assert!(!report["notChecked"].to_string().contains("the book"));
    // The id is in fact taken: refused with the real list.
    let taken = f.write_json(
        "taken.json",
        &json!([{"publisher": ID, "origin": "other.example.org",
                 "signer": "0x1111111111111111111111111111111111111111", "state": "active"}]),
    );
    assert_eq!(f.verify(T0, &["--registered", &taken]).1, 1);
    // A list with an origin in another spelling stops the command (usage).
    let spelled = f.write_json(
        "spelled.json",
        &json!([{"publisher": "lake-levels.open", "origin": "data.example.org/river-levels/",
                 "signer": "0x1111111111111111111111111111111111111111", "state": "active"}]),
    );
    let (site, now) = (f.p("site"), T0.to_string());
    let o = opts(&[
        "--dir",
        &site,
        "--chain-id",
        CHAIN,
        "--book",
        BOOK,
        "--now",
        &now,
        "--registered",
        &spelled,
    ]);
    assert!(review(&o).is_err());
}

/// APPLY-R1 M7: a status of the origin that clears by itself decides
/// nothing (exit 3); a missing file stays a refusal.
#[test]
fn a_temporary_server_error_decides_nothing() {
    let f = Folder::complete("r1-503");
    let lists = f.lists();
    let run = |status: u16| {
        let mut routes = f.routes();
        let r = routes
            .iter_mut()
            .find(|(p, _)| p.ends_with("manifest.bin"))
            .expect("route");
        r.1 = Reply::status(status, &[("retry-after", "30")]);
        let server = FixtureServer::start(routes).expect("server");
        let rv = review(&opts(&[
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--now",
            &T0.to_string(),
            "--origin",
            ORIGIN,
            "--registered",
            &lists.0,
            "--decisions",
            &lists.1,
        ]))
        .expect("review");
        block(verify(&web(&server), &rv))
    };
    for status in [503u16, 429, 502, 500, 504] {
        let (report, code) = run(status);
        assert_eq!(code, 3, "{status} {report:#}");
        assert_eq!(report["decided"], false);
        assert_eq!(report["refused"]["code"], "not-decided");
    }
    for status in [404u16, 403, 410] {
        let (report, code) = run(status);
        assert_eq!(code, 1, "{status} {report:#}");
        assert_eq!(report["refused"]["code"], "origin-unreachable");
    }
}

/// APPLY-R1 M8: after an expiry the guide's `apply --reapply` writes a file
/// the applicant's own doctor calls ready.
#[test]
fn a_reapply_passes_the_doctor() {
    let f = Folder::complete("r1-reapply");
    let later = T0 + LIFETIME_SECS + 3_600;
    f.apply("publisher.key", later, &["--reapply"])
        .expect("apply --reapply");
    let server = FixtureServer::start(f.routes()).expect("server");
    let (dir, now) = (f.p(""), later.to_string());
    let lines = block(doctor(
        &opts(&["--dir", &dir, "--now", &now]),
        Some(&web(&server)),
    ))
    .expect("doctor");
    let (text, good) = doctor_text(&lines);
    assert!(good, "{text}");
    assert_eq!(state(&lines, "application").0, "ok", "{text}");
    assert!(text.contains("re-apply after an expiry"), "{text}");
    assert!(lines.iter().any(|l| l.step == "pointer"), "{text}");
    // The reviewer's tool does not take the applicant's word: without the
    // expired record the old sample is refused.
    let (report, code) = f.verify(later, &[]);
    assert_eq!(
        (code, report["refused"]["code"].as_str()),
        (1, Some("sample-failed")),
        "{report:#}"
    );
}

/// APPLY-R1 L1: the guide's `doctor --live` after the spare was moved
/// offline in step 1 is complete: `keygen` left the spare's address.
#[test]
fn the_doctor_takes_a_spare_that_is_offline() {
    let f = Folder::complete("r1-spare");
    assert_eq!(
        std::fs::read_to_string(f.p("spare.key.address"))
            .expect("address")
            .trim(),
        f.address("spare.key")
    );
    std::fs::remove_file(f.p("spare.key")).expect("moved offline");
    let server = FixtureServer::start(f.routes()).expect("server");
    let (text, good, lines) = f.doctor(&[], Some(&web(&server)));
    assert!(good, "{text}");
    assert_eq!(state(&lines, "spare key").0, "ok", "{text}");
    assert!(lines.iter().any(|l| l.step == "pointer"), "{text}");
    // No spare at all is still missing; the working key's address is no spare.
    std::fs::copy(f.p("publisher.key.address"), f.p("spare.key.address")).expect("copy");
    let (_, good, lines) = f.doctor(&[], Some(&web(&server)));
    assert!(!good);
    assert_eq!(state(&lines, "spare key").0, "missing");
    std::fs::remove_file(f.p("spare.key.address")).expect("remove");
    let (_, _, lines) = f.doctor(&[], None);
    assert_eq!(state(&lines, "spare key").0, "missing");
}

/// APPLY-R1 L4: bytes that cannot be kept for the artifact stop the run.
#[test]
fn a_failed_save_decides_nothing() {
    let f = Folder::complete("r1-save");
    let (site, now, lists) = (f.p("site"), T0.to_string(), f.lists());
    // A folder cannot be made under a regular file.
    let under_a_file = f.p("schema.json/artifact");
    let o = opts(&[
        "--dir",
        &site,
        "--chain-id",
        CHAIN,
        "--book",
        BOOK,
        "--now",
        &now,
        "--registered",
        &lists.0,
        "--decisions",
        &lists.1,
        "--save",
        &under_a_file,
    ]);
    let (report, code) = block(verify(
        &Source::Dir(PathBuf::from(&site)),
        &review(&o).expect("review"),
    ));
    assert_eq!(code, 3, "{report:#}");
    assert_eq!(report["refused"]["check"], "save");
    assert_eq!(report["ok"], false);
}

/// APPLY-R1 M2: `verify-request` reads a request where it is published and
/// checks it: a withdraw against the application it names, the sequence
/// against the last one taken, a theft against the keys document.
#[test]
fn verify_request_checks_a_request_at_the_origin() {
    let f = Folder::complete("r1-request");
    let (k, site, now) = (f.p("publisher.key"), f.p("site"), T0.to_string());
    let make = |extra: &[&str]| {
        let mut a = vec![
            "--key",
            &k,
            "--publisher",
            ID,
            "--origin",
            ORIGIN,
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--site",
            &site,
            "--now",
            &now,
        ];
        a.extend(extra);
        request(&opts(&a)).expect("request")
    };
    let check = |extra: &[&str], src: &Source| {
        let mut a = vec![
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--now",
            &now,
            "--origin",
            ORIGIN,
        ];
        a.extend(extra);
        block(verify_request(src, &review(&opts(&a)).expect("review")))
    };
    let dir = Source::Dir(PathBuf::from(&site));

    // A withdraw of the pending application, read from the origin.
    make(&["--kind", "withdraw"]);
    let server = FixtureServer::start(f.routes()).expect("server");
    let (report, code) = check(&["--last-sequence", "0"], &web(&server));
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["request"], "withdraw");
    assert_eq!(report["signer"], f.address("publisher.key"));
    let app = std::fs::read(f.p("site/.well-known/onyx-publisher-application.json")).expect("app");
    assert_eq!(
        report["ask"]["application"],
        keccak256(&app).to_string().as_str()
    );
    // Without the last sequence nothing is decided; taken once, it is refused.
    let (report, code) = check(&[], &dir);
    assert_eq!(code, EXIT_UNREAD, "{report:#}");
    let seq = T0.to_string();
    let (report, code) = check(&["--last-sequence", &seq], &dir);
    assert_eq!(
        (code, report["refused"]["code"].as_str()),
        (1, Some("bad-request")),
        "{report:#}"
    );
    assert!(report["signer"].is_null(), "{report:#}");
    // The application changed since: the withdraw is not for this file.
    f.apply("publisher.key", T0 + 5, &[]).expect("apply");
    let (report, code) = check(&["--last-sequence", "0"], &dir);
    assert_eq!(code, 1, "{report:#}");
    // The pointer's hash, and a temporary error of the origin.
    let (_, code) = check(
        &[
            "--last-sequence",
            "0",
            "--hash",
            &format!("0x{}", "33".repeat(32)),
        ],
        &dir,
    );
    assert_eq!(code, 1);
    let mut routes = f.routes();
    routes.retain(|(p, _)| !p.ends_with("onyx-publisher-request.json"));
    routes.push((
        "/river-levels/.well-known/onyx-publisher-request.json".into(),
        Reply::status(503, &[]),
    ));
    let down = FixtureServer::start(routes).expect("server");
    assert_eq!(check(&["--last-sequence", "0"], &web(&down)).1, 3);

    // A theft: the book must be read, and the keys document must have changed.
    make(&["--kind", "key-stolen", "--at", &(T0 - 600).to_string()]);
    let book = f.write_json(
        "book-own.json",
        &json!([{"publisher": ID, "origin": ORIGIN, "signer": f.address("publisher.key"), "state": "active"}]),
    );
    assert_eq!(check(&["--last-sequence", "0"], &dir).1, 1);
    let (report, code) = check(&["--last-sequence", "0", "--registered", &book], &dir);
    assert_eq!(code, 1, "{report:#}");
    assert!(report["refused"]["reason"]
        .as_str()
        .expect("why")
        .contains("still lists"));
    std::fs::write(
        f.p("site/.well-known/onyx-source-keys.json"),
        format!("{}\n", json!({"version": 1, "source": ID, "keys": []})),
    )
    .expect("write");
    let (report, code) = check(&["--last-sequence", "0", "--registered", &book], &dir);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["ask"]["sinceUnix"], T0 - 600);
    // A retirement from long ago is refused while the document lists the key.
    f.keys_doc("publisher.key");
    make(&["--kind", "retire-key", "--at", "0"]);
    let (report, code) = check(&["--last-sequence", "0", "--registered", &book], &dir);
    assert_eq!(
        (code, report["refused"]["code"].as_str()),
        (1, Some("bad-request")),
        "{report:#}"
    );
    make(&["--kind", "retire-key", "--at", &now]);
    assert_eq!(
        check(&["--last-sequence", "0", "--registered", &book], &dir).1,
        0
    );
}

/// APPLY-R1 L1: `keys-doc --also-address` writes the spare into the keys
/// document without its key file.
#[test]
fn keys_doc_lists_a_spare_by_its_address() {
    let f = Folder::new("r1-keysdoc");
    f.keygen("publisher.key");
    f.keygen("spare.key");
    let note = std::fs::read_to_string(f.p("spare.key.address")).expect("address");
    let spare = note.trim();
    std::fs::remove_file(f.p("spare.key")).expect("offline");
    let (key, out) = (
        f.p("publisher.key"),
        f.p("site/.well-known/onyx-source-keys.json"),
    );
    let run = |also: &str| {
        keys_doc(&opts(&[
            "--key",
            &key,
            "--publisher",
            ID,
            "--also-address",
            also,
            "--out",
            &out,
        ]))
    };
    run(spare).expect("keys-doc");
    let doc = KeysDocument::parse(&std::fs::read(&out).expect("doc")).expect("parse");
    assert_eq!(doc.source, ID);
    assert_eq!(doc.signers.len(), 2);
    assert_eq!(hx(&doc.signers[0]), f.address("publisher.key"));
    assert_eq!(hx(&doc.signers[1]), spare);
    // The same key twice, or no address, is refused.
    assert!(run(&f.address("publisher.key")).is_err());
    assert!(run(&format!("{spare},{spare}")).is_err());
    assert!(run("spare.key").is_err());
}
