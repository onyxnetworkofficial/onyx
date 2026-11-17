//! Tests of `book-reviewer` (ADR-0066 slice 3; THREATS PUBON-6, PUBON-19).

use super::*;
use crate::onboard::tests::{block, opts, Folder, BOOK, CHAIN, ID, ORIGIN, T0};
use std::sync::{Arc, Mutex};

const REVIEWER: [u8; 20] = [0x4e; 20];

/// The book as a fake chain node and the reviewer's outbox share it.
#[derive(Default)]
struct State {
    /// The reason of the registration the book holds, when it holds one.
    registered: Option<String>,
    /// The book holds the sample's report.
    reported: bool,
    /// The calls sent, each with the limits file's text at that moment.
    sent: Vec<(String, String)>,
    /// The next `postReport` fails.
    fail_relay: bool,
    /// The key's life the book answers once registered (0: active).
    life: u8,
    /// What the node says: limits per publisher are on; the asked
    /// publisher has its entry.
    limits_on: bool,
    listed: bool,
    reviewer: [u8; 20],
}

#[derive(Clone)]
struct Fake(Arc<Mutex<State>>, std::path::PathBuf);

fn padded(s: &str) -> Vec<u8> {
    let mut b = word(s.len() as u64).to_vec();
    let mut t = s.as_bytes().to_vec();
    t.resize(s.len().div_ceil(32) * 32, 0);
    b.extend(t);
    b
}

impl JsonRpc for Fake {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let s = self.0.lock().expect("state");
            match method {
                "eth_call" => {
                    let d = p[0]["data"].as_str().unwrap_or_default().to_string();
                    let is = |sig: &str| d.starts_with(&hx(&selector(sig)));
                    let mut out = vec![0u8; 192];
                    if is("reviewer()") {
                        out = address_word(&s.reviewer).to_vec();
                    } else if is("keyState(bytes32)") {
                        out[31] = if s.registered.is_some() {
                            s.life.max(1)
                        } else {
                            0
                        };
                    } else if is("report(bytes32)") && s.reported {
                        out[0] = 1;
                    }
                    Ok(json!(hx(&out)))
                }
                "eth_getLogs" => Ok(match &s.registered {
                    None => json!([]),
                    Some(reason) => {
                        // source, origin, provenance, by, reason.
                        let (a, b) = (padded("s"), padded("o"));
                        let mut d = Vec::new();
                        for w in [
                            word(160),
                            word(160 + a.len() as u64),
                            [7; 32],
                            [0; 32],
                            word((160 + a.len() + b.len()) as u64),
                        ] {
                            d.extend_from_slice(&w);
                        }
                        d.extend(a);
                        d.extend(b);
                        d.extend(padded(reason));
                        json!([{"data": hx(&d), "blockNumber": "0x2a", "transactionHash": hx(&[0x7a; 32])}])
                    }
                }),
                other => Err(format!("unexpected {other}")),
            }
        })
    }
}

impl NodeProbe for Fake {
    fn listed<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Res<(bool, bool)>> {
        Box::pin(async move {
            let s = self.0.lock().expect("state");
            Ok((s.limits_on, s.listed))
        })
    }
}

impl Outbox for Fake {
    fn send<'a>(&'a self, what: &'a str, data: Vec<u8>) -> BoxFuture<'a, Res<Mined>> {
        Box::pin(async move {
            let mut s = self.0.lock().expect("state");
            let limits = std::fs::read_to_string(&self.1).unwrap_or_default();
            if what == "postReport" && s.fail_relay {
                s.fail_relay = false;
                return Err("postReport: the node went away".into());
            }
            s.sent.push((what.to_string(), limits));
            match what {
                "registerKey" => {
                    // The reason is the call's last string.
                    let text = String::from_utf8_lossy(&data).to_string();
                    let at = text.find("publisher application 0x").expect("reason");
                    s.registered = Some(text[at..at + 24 + 64].to_string());
                }
                "postReport" => s.reported = true,
                "retireKey" => s.life = 2,
                "markCompromised" => s.life = 3,
                _ => {}
            }
            Ok(Mined {
                hash: hx(&[s.sent.len() as u8; 32]),
                block: 40 + s.sent.len() as u64,
                gas_used: 1,
                effective_gas_price: 1,
            })
        })
    }
}

struct Case {
    f: Folder,
    fake: Fake,
    limits: std::path::PathBuf,
    records: std::path::PathBuf,
    hash: String,
}

impl Case {
    fn new(name: &str) -> Case {
        let f = Folder::complete(name);
        let limits = f.0.join("publishers.json");
        std::fs::write(&limits, r#"{"reserve_gib": 2, "publishers": [{"id": "lake-levels.open", "quota_gib": 16, "datasets": 50000, "state": "open"}]}"#).expect("limits");
        let records = f.0.join("records");
        std::fs::create_dir_all(&records).expect("records");
        let app = std::fs::read(f.p("site/.well-known/onyx-publisher-application.json"))
            .expect("application");
        let fake = Fake(
            Arc::new(Mutex::new(State {
                reviewer: REVIEWER,
                limits_on: true,
                ..State::default()
            })),
            limits.clone(),
        );
        Case {
            hash: keccak256(&app).to_string(),
            f,
            fake,
            limits,
            records,
        }
    }
    fn review(&self, book: bool, hash: &str) -> Review {
        let (reg, _) = self.f.lists();
        let (records, now) = (self.records.display().to_string(), (T0 + 60).to_string());
        let mut a = vec![
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--origin",
            ORIGIN,
            "--hash",
            hash,
            "--decisions",
            &records,
            "--now",
            &now,
        ];
        if book {
            a.extend(["--registered", &reg]);
        }
        review(&opts(&a)).expect("review")
    }
    /// `live`: the node answers that it holds the publisher's entry.
    fn run(&self, rv: &Review, send: bool, live: bool, max_gib: u64) -> Res<(u8, Vec<String>)> {
        self.fake.0.lock().expect("state").listed = live;
        let a = Approve {
            rpc: &self.fake,
            out: if send { Some(&self.fake) } else { None },
            reviewer: REVIEWER,
            artifact: self.f.0.join("site"),
            records: self.records.clone(),
            limits: self.limits.clone(),
            max_gib,
            grant: (None, None),
            node: &self.fake,
            answers: json!({"7": "yes", "8": "yes", "9": "yes", "10": "yes", "11": "yes", "13": "yes"}),
        };
        block(approve(&a, rv))
    }
    fn sent(&self) -> Vec<String> {
        self.fake
            .0
            .lock()
            .expect("state")
            .sent
            .iter()
            .map(|(w, _)| w.clone())
            .collect()
    }
    fn limits_text(&self) -> String {
        std::fs::read_to_string(&self.limits).expect("limits")
    }
    fn untouched(&self, before: &str) {
        assert!(self.sent().is_empty(), "nothing is sent: {:?}", self.sent());
        assert_eq!(self.limits_text(), before, "the limits file is not touched");
        assert_eq!(
            std::fs::read_dir(&self.records).expect("records").count(),
            0,
            "no record is written"
        );
    }
}

#[test]
fn refuses_to_send_when_the_application_fails() {
    let c = Case::new("reviewer-refuses");
    let before = c.limits_text();

    // The book was not read: exit 4 is never an approval.
    let (code, lines) = c
        .run(&c.review(false, &c.hash), true, true, 24)
        .expect("run");
    assert_eq!(code, 4, "{lines:?}");
    c.untouched(&before);

    // Another file than the pointer names.
    let other = Hash32([0x11; 32]).to_string();
    let e = c
        .run(&c.review(true, &other), true, true, 24)
        .expect_err("hash");
    assert!(
        e.contains("not to --hash") && e.contains("nothing sent"),
        "{e}"
    );
    c.untouched(&before);

    // The key in the file is not the book's reviewer.
    c.fake.0.lock().expect("state").reviewer = [0x99; 20];
    let e = c
        .run(&c.review(true, &c.hash), true, true, 24)
        .expect_err("reviewer");
    assert!(e.contains("the book's reviewer is not this key"), "{e}");
    c.untouched(&before);
    c.fake.0.lock().expect("state").reviewer = REVIEWER;

    // No room on the node: 16 + 2 + 1 GiB do not fit 18.
    let (code, lines) = c
        .run(&c.review(true, &c.hash), true, false, 18)
        .expect("run");
    assert_eq!(code, 1);
    assert!(lines.iter().any(|l| l.contains("no-capacity")), "{lines:?}");
    c.untouched(&before);

    // A node without limits per publisher: the new key would get the
    // node's one quota (review S3-R1 M3). Asked before anything is written.
    c.fake.0.lock().expect("state").limits_on = false;
    let e = c
        .run(&c.review(true, &c.hash), true, true, 24)
        .expect_err("limits off");
    assert!(
        e.contains("runs without limits per publisher") && e.contains("nothing sent"),
        "{e}"
    );
    c.untouched(&before);
    c.fake.0.lock().expect("state").limits_on = true;

    // A sample whose bytes changed after it was signed.
    let shards = c.f.0.join("site/sample/shards");
    let shard = std::fs::read_dir(&shards)
        .expect("shards")
        .next()
        .expect("one")
        .expect("entry")
        .path();
    let mut b = std::fs::read(&shard).expect("shard");
    let last = b.len() - 1;
    b[last] ^= 1;
    std::fs::write(&shard, &b).expect("write");
    let (code, lines) = c
        .run(&c.review(true, &c.hash), true, true, 24)
        .expect("run");
    assert_eq!(code, 1, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("NOT APPROVED") && l.contains("nothing was sent")),
        "{lines:?}"
    );
    c.untouched(&before);
}

#[test]
fn writes_the_limit_before_it_registers() {
    let c = Case::new("reviewer-order");
    let before = c.limits_text();
    let rv = c.review(true, &c.hash);

    // A dry run sends nothing and writes nothing, and says what a run does.
    let (code, lines) = c.run(&rv, false, false, 24).expect("dry");
    assert_eq!(code, 0);
    for want in [
        "would write the limit",
        "would send registerKey",
        "would relay",
        "nothing was sent and nothing written",
    ] {
        assert!(lines.iter().any(|l| l.contains(want)), "{want}: {lines:?}");
    }
    c.untouched(&before);

    // The first run writes the entry and stops: the node must start with it.
    let (code, lines) = c.run(&rv, true, false, 24).expect("first");
    assert_eq!(code, EXIT_LIMIT_WRITTEN, "{lines:?}");
    assert!(c.sent().is_empty());
    let entry = json!({"id": ID, "quota_gib": 1, "datasets": 16, "state": "open"});
    let file: Value = serde_json::from_str(&c.limits_text()).expect("json");
    assert_eq!(file["publishers"][1], entry);
    assert_eq!(
        file["publishers"].as_array().map(Vec::len),
        Some(2),
        "the other entry stays"
    );
    // The same run again changes nothing.
    let once = c.limits_text();
    assert_eq!(
        c.run(&rv, true, false, 24).expect("again").0,
        EXIT_LIMIT_WRITTEN
    );
    assert_eq!(c.limits_text(), once);

    // With the node restarted: registerKey, and the relay fails.
    c.fake.0.lock().expect("state").fail_relay = true;
    let e = c.run(&rv, true, true, 24).expect_err("relay");
    assert!(e.contains("postReport"), "{e}");
    assert_eq!(c.sent(), ["registerKey"]);
    {
        let s = c.fake.0.lock().expect("state");
        let at_register: Value = serde_json::from_str(&s.sent[0].1).expect("json");
        assert_eq!(
            at_register["publishers"][1], entry,
            "the limit was in the file when registerKey went out"
        );
    }
    assert_eq!(
        std::fs::read_dir(&c.records).expect("records").count(),
        0,
        "no record before the relay"
    );

    // The run again: the key is not registered twice, the report is relayed, the record written.
    let (code, lines) = c.run(&rv, true, true, 24).expect("resume");
    assert_eq!(code, 0, "{lines:?}");
    assert_eq!(c.sent(), ["registerKey", "postReport"]);
    let folder = c.records.join(ID);
    assert_eq!(
        std::fs::read(folder.join("application.json")).expect("copy"),
        std::fs::read(c.f.p("site/.well-known/onyx-publisher-application.json"))
            .expect("application"),
        "the record keeps the application's bytes"
    );
    let d = &record::read(&folder.join("decision.json")).expect("decision")[0];
    assert_eq!(d["state"], "approved");
    assert_eq!(d["application"], c.hash);
    assert_eq!(d["publisher"], ID);
    assert_eq!(d["origin"], ORIGIN);
    assert_eq!(d["signer"], c.f.address("publisher.key"));
    assert_eq!(d["limits"], json!({"quota_gib": 1, "datasets": 16}));
    assert_eq!(d["register"], json!({"tx": hx(&[0x7a; 32]), "block": 42}));
    assert_eq!(d["relay"]["block"], 42);
    assert_eq!(d["answers"]["9"], "yes");
    assert!(d["answers"].get("12").is_none(), "line 12 is not published");
    assert_eq!(d["date"], "2026-11-17");

    // And once more: nothing is sent, nothing is written twice.
    let (code, lines) = c.run(&rv, true, true, 24).expect("third");
    assert_eq!(code, 0);
    assert!(
        lines.iter().any(|l| l.starts_with("RECORDED already")),
        "{lines:?}"
    );
    assert_eq!(c.sent().len(), 2);
    assert_eq!(
        record::read(&folder.join("decision.json"))
            .expect("decision")
            .len(),
        1
    );
}

#[test]
fn a_key_registered_by_another_decision_is_not_taken_over() {
    let c = Case::new("reviewer-other");
    c.fake.0.lock().expect("state").registered = Some("registered by hand".into());
    let e = c
        .run(&c.review(true, &c.hash), true, true, 24)
        .expect_err("other");
    assert!(
        e.contains("by another decision") && e.contains("nothing sent"),
        "{e}"
    );
    assert!(c.sent().is_empty());
}

#[test]
fn a_record_takes_its_signer_and_block_from_the_report_only() {
    let signer = "0x00000000000000000000000000000000000000aa";
    let app = Hash32([0xab; 32]).to_string();
    let refused = json!({"applicationKeccak": app, "signer": signer, "blocksSignerUntil": 1_795_000_000u64,
        "refused": {"code": "sample-failed"}, "sampleManifestRoot": Hash32([3; 32]).to_string()});
    let d = decision_of("refused", None, &refused, 1, T0).expect("refused");
    assert_eq!(
        d,
        json!({"state": "refused", "date": "2026-11-17", "application": app, "reason": "sample-failed",
        "signer": signer, "blocks_signer_until": 1_795_000_000u64})
    );
    // A file that is nobody's (a wrong signature or another hash) gets no record.
    let mut nobody = refused.clone();
    nobody["blocksSignerUntil"] = Value::Null;
    let e = decision_of("refused", None, &nobody, 1, T0).expect_err("nobody");
    assert!(e.contains("nobody's"), "{e}");
    // A person's refusal of a file the tool passed names one of the codes.
    let mut passed = refused.clone();
    passed["refused"] = Value::Null;
    assert!(decision_of("refused", None, &passed, 0, T0).is_err());
    assert!(decision_of("refused", Some("because"), &passed, 0, T0).is_err());
    assert_eq!(
        decision_of("refused", Some("rights-unclear"), &passed, 0, T0).expect("person")["reason"],
        "rights-unclear"
    );
    // The tool's own code is never overruled, and an expiry is its own state.
    assert!(decision_of("refused", Some("rights-unclear"), &refused, 1, T0).is_err());
    let mut expired = refused.clone();
    expired["refused"]["code"] = json!("expired");
    assert!(decision_of("refused", None, &expired, 1, T0).is_err());
    let d = decision_of("expired", None, &expired, 1, T0).expect("expired");
    assert_eq!(d["sample_manifest_root"], Hash32([3; 32]).to_string());
    assert_eq!(d["signer"], signer);
    // Nothing decided is nothing recorded.
    assert!(decision_of("refused", None, &refused, 3, T0).is_err());
    // What a refusal keeps, and nothing else.
    let keys: Vec<&str> = d
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "application",
            "date",
            "sample_manifest_root",
            "signer",
            "state"
        ]
    );
}

#[test]
fn calldata_is_the_abi_encoding() {
    // `cast calldata` of the same calls.
    let d = calldata(
        "registerKey(string,address,string,bytes32,string)",
        &[
            Arg::Text("river-levels.open"),
            Arg::Word(address_word(
                &[&[0u8; 19][..], &[0xaa]].concat().try_into().expect("20"),
            )),
            Arg::Text("data.example.org/river-levels"),
            Arg::Word([0x11; 32]),
            Arg::Text("publisher application 0xabc"),
        ],
    );
    assert_eq!(hex::encode(d), "a31643fe00000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000aa00000000000000000000000000000000000000000000000000000000000000e011111111111111111111111111111111111111111111111111111111111111110000000000000000000000000000000000000000000000000000000000000120000000000000000000000000000000000000000000000000000000000000001172697665722d6c6576656c732e6f70656e000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001d646174612e6578616d706c652e6f72672f72697665722d6c6576656c73000000000000000000000000000000000000000000000000000000000000000000001b7075626c6973686572206170706c69636174696f6e2030786162630000000000");
    let d = calldata(
        "retireKey(bytes32,uint64,string)",
        &[
            Arg::Word([0x22; 32]),
            Arg::Word(word(1_794_873_600)),
            Arg::Text("r"),
        ],
    );
    assert_eq!(hex::encode(d), "3092e6d02222222222222222222222222222222222222222222222222222222222222222000000000000000000000000000000000000000000000000000000006afb9900000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000017200000000000000000000000000000000000000000000000000000000000000");
}

#[test]
fn the_answers_are_yes_or_no_and_line_twelve_stays_private() {
    let f = Folder::complete("reviewer-answers");
    let yes = |skip: &str| {
        let mut a = serde_json::Map::new();
        for n in ANSWER_LINES {
            if n != skip {
                a.insert(n.to_string(), json!("yes"));
            }
        }
        a
    };
    assert!(read_answers(&f.write_json("answers.json", &Value::Object(yes("")))).is_ok());
    assert!(read_answers(&f.write_json("short.json", &Value::Object(yes("9")))).is_err());
    // Line 12 says who runs a publisher: it is not taken into the record.
    let mut twelve = yes("");
    twelve.insert("12".into(), json!("yes"));
    let e = read_answers(&f.write_json("twelve.json", &Value::Object(twelve))).expect_err("12");
    assert!(e.contains("private notes"), "{e}");
    // No free text is published, and a no is a refusal.
    let mut text = yes("");
    text.insert("8".into(), json!("reads as nobody else"));
    let e = read_answers(&f.write_json("text.json", &Value::Object(text))).expect_err("text");
    assert!(e.contains("no free text"), "{e}");
    let mut no = yes("");
    no.insert("10".into(), json!("no"));
    let e = read_answers(&f.write_json("no.json", &Value::Object(no))).expect_err("no");
    assert!(e.contains("refuse the application"), "{e}");
}

// ------------------------------------------------------------------ review S3-R1

/// S3-R1 M1: a recorded withdrawal, refusal or expiry of this very file
/// stops `approve`: nothing is sent, nothing written.
#[test]
fn a_closed_application_is_not_approved() {
    for state in ["withdrawn", "refused", "expired"] {
        let c = Case::new(&format!("s3r1-{state}"));
        let before = c.limits_text();
        let folder = c.records.join(format!("application-{}", &c.hash[2..18]));
        std::fs::create_dir_all(&folder).expect("folder");
        // A blocked signer other than the applicant's: the block is not what stops it.
        std::fs::write(
            folder.join("decision.json"),
            json!([{"state": state, "date": "2026-11-17", "application": c.hash, "request": "withdraw",
                    "request_keccak": hx(&[1u8; 32]), "signer": hx(&[2u8; 20]), "sequence": 1,
                    "blocks_signer_until": 1, "sample_manifest_root": hx(&[3u8; 32])}])
            .to_string(),
        )
        .expect("record");
        let rv = c.review(true, &c.hash);
        for live in [false, true] {
            let (code, lines) = c.run(&rv, true, live, 24).expect("run");
            assert_eq!(code, 1, "{state}: {lines:?}");
            assert!(
                lines.iter().any(|l| l.contains(&format!("as {state}"))),
                "{lines:?}"
            );
        }
        assert!(c.sent().is_empty(), "{state}: nothing is sent");
        assert_eq!(
            c.limits_text(),
            before,
            "{state}: the limits file is not touched"
        );
        // The check itself refuses the file too.
        let (report, code) = block(verify(&Source::Dir(c.f.0.join("site")), &rv));
        assert_eq!(
            (code, report["refused"]["code"].as_str()),
            (1, Some("duplicate")),
            "{report:#}"
        );
    }
}

/// S3-R1 M2: a run repeated after `registerKey` goes on only for a key the
/// book still holds as active. A key marked compromised (or retired) since
/// gets no relay and no approved record.
#[test]
fn a_key_retired_or_compromised_since_is_not_taken_up_again() {
    for (life, word) in [(3u8, "compromised"), (2, "retired")] {
        let c = Case::new(&format!("s3r1-life{life}"));
        let rv = c.review(true, &c.hash);
        assert_eq!(
            c.run(&rv, true, false, 24).expect("first").0,
            EXIT_LIMIT_WRITTEN
        );
        c.fake.0.lock().expect("state").fail_relay = true;
        c.run(&rv, true, true, 24).expect_err("relay fails");
        c.fake.0.lock().expect("state").life = life;
        let e = c.run(&rv, true, true, 24).expect_err("rerun");
        assert!(
            e.contains(word) && e.contains("nothing sent, nothing recorded"),
            "{e}"
        );
        assert_eq!(c.sent(), ["registerKey"], "no relay");
        assert!(!c.records.join(ID).exists(), "no approved record");
    }
}

/// S3-R1 L1: `pause` checks everything before it writes anything.
#[test]
fn pause_with_a_wrong_reason_writes_nothing() {
    let c = Case::new("s3r1-pause");
    let id = "lake-levels.open";
    std::fs::create_dir_all(c.records.join(id)).expect("folder");
    std::fs::write(c.records.join(id).join("decision.json"), "[]").expect("record");
    let before = c.limits_text();
    let (l, r) = (
        c.limits.display().to_string(),
        c.records.display().to_string(),
    );
    let base = ["--publisher", id, "--limits", &l, "--records", &r];
    let with = |more: &[&str]| opts(&[&base[..], more].concat());
    let e = pause_cmd(&with(&["--reason", "bogus"]), true).expect_err("refused");
    assert!(e.contains("--reason"), "{e}");
    assert_eq!(c.limits_text(), before, "the limits file is not rewritten");
    assert_eq!(
        std::fs::read_to_string(c.records.join(id).join("decision.json")).expect("r"),
        "[]"
    );
    // S3-R1 L2: the clock is set on a local chain only.
    let e = pause_cmd(&with(&["--now", "5"]), true).expect_err("now");
    assert!(e.contains("local chain"), "{e}");
    assert_eq!(c.limits_text(), before);
    // A pause, then a resume: the file and the record say so.
    let t = T0.to_string();
    let local = [
        "--now",
        t.as_str(),
        "--chain-id",
        "31337",
        "--reason",
        "rights-unclear",
    ];
    assert_eq!(pause_cmd(&with(&local), true), Ok(0));
    assert!(c.limits_text().contains("paused"));
    assert_eq!(pause_cmd(&with(&local[..4]), false), Ok(0));
    assert!(!c.limits_text().contains("paused"));
    let d = record::read(&c.records.join(id).join("decision.json")).expect("record");
    assert_eq!(
        d[0],
        json!({"state": "paused", "date": "2026-11-17", "action": "pause", "reason": "rights-unclear"})
    );
    assert_eq!(
        d[1],
        json!({"state": "approved", "date": "2026-11-17", "action": "resume"})
    );
    // A publisher without a record or an entry is not paused.
    assert!(pause_cmd(
        &opts(&[
            "--publisher",
            "unknown.open",
            "--limits",
            &l,
            "--records",
            &r
        ]),
        true
    )
    .is_err());
}

/// The later requests (ADR-0066 item 12) against a fake chain.
struct Later {
    c: Case,
    rv: Review,
}

impl Later {
    /// `other_key`: the id has a second active key in the book.
    fn new(name: &str, other_key: bool) -> Later {
        let c = Case::new(name);
        std::fs::write(&c.limits, format!(r#"{{"reserve_gib": 2, "publishers": [{{"id": "{ID}", "quota_gib": 2, "datasets": 16, "state": "open"}}]}}"#)).expect("limits");
        let mut keys = vec![
            json!({"publisher": ID, "origin": ORIGIN, "signer": c.f.address("publisher.key"), "state": "active"}),
        ];
        if other_key {
            keys.push(json!({"publisher": ID, "origin": ORIGIN, "signer": c.f.address("spare.key"), "state": "active"}));
        }
        let reg = c.f.write_json("book.json", &Value::Array(keys));
        let (records, now) = (c.records.display().to_string(), (T0 + 60).to_string());
        let rv = review(&opts(&[
            "--chain-id",
            CHAIN,
            "--book",
            BOOK,
            "--decisions",
            &records,
            "--now",
            &now,
            "--registered",
            &reg,
        ]))
        .expect("review");
        // The book holds the key the request is about, active.
        c.fake.0.lock().expect("state").registered = Some("registered".into());
        Later { c, rv }
    }
    fn report(&self, kind: &str, ask: Value) -> Value {
        json!({"request": kind, "publisher": ID, "signer": self.c.f.address("publisher.key"),
               "requestKeccak": hx(&[0x5e; 32]), "sequence": 7, "ask": ask})
    }
    fn act(
        &self,
        report: &Value,
        send: bool,
        grant: (Option<u64>, Option<u64>),
        max_gib: u64,
        no_new_key: bool,
    ) -> Res<(u8, Vec<String>)> {
        let a = Act {
            rpc: &self.c.fake,
            out: if send { Some(&self.c.fake) } else { None },
            reviewer: Some(REVIEWER),
            records: self.c.records.clone(),
            limits: Some(self.c.limits.clone()),
            max_gib: Some(max_gib),
            grant,
            no_new_key,
        };
        block(act(&a, &self.rv, report))
    }
    fn recorded(&self) -> Vec<Value> {
        let f = self.c.records.join(ID).join("decision.json");
        if f.exists() {
            record::read(&f).expect("record")
        } else {
            Vec::new()
        }
    }
}

/// S3-R1 M5, M6: a raise names its new limit, never lowers one, and fits.
#[test]
fn a_raise_names_its_limit_and_never_lowers_one() {
    let l = Later::new("s3r1-raise", false);
    let before = l.c.limits_text();
    let report = l.report("raise", json!({"quotaGib": 1024, "datasets": 64}));
    for (grant, why) in [
        ((None, None), "--quota-gib and --datasets are required"),
        ((Some(4), None), "--quota-gib and --datasets are required"),
        ((Some(2000), Some(64)), "over what the request asks"),
        ((Some(4), Some(65)), "over what the request asks"),
        ((Some(1), Some(64)), "never lowers a limit"),
        ((Some(4), Some(8)), "never lowers a limit"),
    ] {
        let e = l.act(&report, true, grant, 24, false).expect_err(why);
        assert!(e.contains(why), "{why}: {e}");
    }
    // No room beside the reserve: refused, nothing written.
    let (code, lines) = l
        .act(&report, true, (Some(23), Some(64)), 24, false)
        .expect("full");
    assert_eq!(code, 1, "{lines:?}");
    // A dry run writes nothing.
    let (code, lines) = l
        .act(&report, false, (Some(4), Some(32)), 24, false)
        .expect("dry");
    assert_eq!(code, 0);
    assert!(
        lines
            .iter()
            .any(|x| x.starts_with("DRY RUN: would write the limit")),
        "{lines:?}"
    );
    assert_eq!(l.c.limits_text(), before);
    assert!(l.recorded().is_empty() && l.c.sent().is_empty());
    // The raise: the file and the record, with the sequence.
    assert_eq!(
        l.act(&report, true, (Some(4), Some(32)), 24, false)
            .expect("raise")
            .0,
        0
    );
    let file: Value = serde_json::from_str(&l.c.limits_text()).expect("json");
    assert_eq!(
        file["publishers"][0],
        json!({"id": ID, "quota_gib": 4, "datasets": 32, "state": "open"})
    );
    let d = l.recorded();
    assert_eq!(d.len(), 1);
    assert_eq!(
        (
            d[0]["request"].as_str(),
            d[0]["sequence"].as_u64(),
            d[0]["state"].as_str()
        ),
        (Some("raise"), Some(7), Some("approved"))
    );
    assert_eq!(d[0]["limits"], json!({"quota_gib": 4, "datasets": 32}));
    assert!(l.c.sent().is_empty(), "a raise sends nothing");
}

/// S3-R1 M6: a retirement is sent by the book's reviewer only, once.
#[test]
fn a_retirement_is_sent_by_the_books_reviewer_once() {
    let l = Later::new("s3r1-retire", true);
    let key = l.c.f.address("publisher.key");
    let report = l.report("retire-key", json!({"retire": key, "atUnix": T0}));
    // Another key than the book's reviewer: nothing sent.
    l.c.fake.0.lock().expect("state").reviewer = [0x99; 20];
    let e = l
        .act(&report, true, (None, None), 24, false)
        .expect_err("reviewer");
    assert!(e.contains("the book's reviewer is not this key"), "{e}");
    l.c.fake.0.lock().expect("state").reviewer = REVIEWER;
    // A dry run.
    let (code, lines) = l.act(&report, false, (None, None), 24, false).expect("dry");
    assert_eq!(code, 0);
    assert!(
        lines
            .iter()
            .any(|x| x.starts_with("DRY RUN: would send retireKey(")),
        "{lines:?}"
    );
    assert!(l.c.sent().is_empty() && l.recorded().is_empty());
    // The run.
    assert_eq!(
        l.act(&report, true, (None, None), 24, false)
            .expect("retire")
            .0,
        0
    );
    assert_eq!(l.c.sent(), ["retireKey"]);
    let d = l.recorded();
    assert_eq!(
        (
            d[0]["state"].as_str(),
            d[0]["ended_signer"].as_str(),
            d[0]["at_unix"].as_u64()
        ),
        (Some("approved"), Some(key.as_str()), Some(T0))
    );
    assert_eq!(d[0]["tx"]["block"], 41);
    // Again: the book shows it retired; nothing is sent twice.
    let (code, lines) = l
        .act(&report, true, (None, None), 24, false)
        .expect("again");
    assert_eq!(code, 0);
    assert!(
        lines.iter().any(|x| x.starts_with("SENT already")),
        "{lines:?}"
    );
    assert_eq!(l.c.sent(), ["retireKey"]);
    // A key the book holds as compromised is not retired.
    l.c.fake.0.lock().expect("state").life = 3;
    let e = l
        .act(&report, true, (None, None), 24, false)
        .expect_err("compromised");
    assert!(
        e.contains("does not show") && e.contains("nothing sent"),
        "{e}"
    );
}

/// S3-R1 M6: a theft with a move to a new key: the new key first.
#[test]
fn a_stolen_key_is_marked_only_after_the_new_key_is_registered() {
    // The id's only key: marking it would leave the publisher without a
    // way to a new one.
    let only = Later::new("s3r1-stolen-only", false);
    let report = only.report("key-stolen", json!({"sinceUnix": T0 - 600}));
    let (code, lines) = only
        .act(&report, true, (None, None), 24, false)
        .expect("order");
    assert_eq!(code, 1);
    assert!(
        lines
            .iter()
            .any(|x| x.starts_with("NOT SENT") && x.contains("Register the new key first")),
        "{lines:?}"
    );
    assert!(only.c.sent().is_empty() && only.recorded().is_empty());
    // The publisher gives the id up: --no-new-key.
    assert_eq!(
        only.act(&report, true, (None, None), 24, true)
            .expect("no new key")
            .0,
        0
    );
    assert_eq!(only.c.sent(), ["markCompromised"]);
    let d = only.recorded();
    assert_eq!(
        (d[0]["state"].as_str(), d[0]["since_unix"].as_u64()),
        (Some("retired"), Some(T0 - 600))
    );
    // With the new key registered and final: sent, and the id goes on.
    let two = Later::new("s3r1-stolen-two", true);
    let (code, lines) = two
        .act(&report, false, (None, None), 24, false)
        .expect("dry");
    assert_eq!(code, 0);
    assert!(
        lines
            .iter()
            .any(|x| x.starts_with("DRY RUN: would send markCompromised(")),
        "{lines:?}"
    );
    assert!(two.c.sent().is_empty());
    assert_eq!(
        two.act(&report, true, (None, None), 24, false)
            .expect("stolen")
            .0,
        0
    );
    assert_eq!(two.c.sent(), ["markCompromised"]);
    assert_eq!(two.recorded()[0]["state"], "approved");
    let (_, lines) = two
        .act(&report, true, (None, None), 24, false)
        .expect("again");
    assert!(
        lines.iter().any(|x| x.starts_with("SENT already")),
        "{lines:?}"
    );
    assert_eq!(two.c.sent(), ["markCompromised"]);
}

/// S3-R1 M6, L2, L9: the commands' own rules.
#[test]
fn the_commands_take_test_networks_and_write_a_state_once() {
    let net = |chain: &str| test_network(&opts(&["--chain-id", chain, "--book", BOOK]));
    assert!(net("84532").is_ok() && net("31337").is_ok());
    for chain in ["8453", "1", "x"] {
        assert!(net(chain).is_err(), "{chain}");
    }
    assert!(clock_rule(&opts(&["--now", "5", "--chain-id", "84532"])).is_err());
    assert!(clock_rule(&opts(&["--now", "5"])).is_err());
    assert!(clock_rule(&opts(&["--now", "5", "--chain-id", "31337"])).is_ok());
    assert!(clock_rule(&opts(&["--chain-id", "84532"])).is_ok());
    // chain_review: each missing option is named before anything is read.
    let c = Case::new("s3r1-commands");
    let records = c.records.display().to_string();
    let full = [
        "--chain-id",
        "31337",
        "--book",
        BOOK,
        "--rpc",
        "http://127.0.0.1:1",
        "--origin",
        ORIGIN,
        "--hash",
        c.hash.as_str(),
        "--records",
        records.as_str(),
    ];
    for missing in ["--rpc", "--origin", "--hash", "--records"] {
        let at = full.iter().position(|x| *x == missing).expect("there");
        let mut a = full.to_vec();
        a.drain(at..at + 2);
        let e = block(chain_review(&opts(&a), "test")).err().expect("usage");
        assert!(
            e.contains(&format!("{missing} is required")),
            "{missing}: {e}"
        );
    }
    let mut real = full.to_vec();
    real[1] = "84532";
    real.extend(["--now", "5"]);
    assert!(
        block(chain_review(&opts(&real), "test")).is_err(),
        "--now on a real chain"
    );
    // A chain node that does not answer: nothing decided (exit 3).
    assert!(matches!(
        block(chain_review(&opts(&full), "test")),
        Ok(Err(3))
    ));
    // A state is written once.
    let e = json!({"state": "received", "date": "2026-11-17", "application": c.hash, "received_unix": T0});
    let (file, written) = record_once(&c.records, "application-00", &e).expect("first");
    assert!(written);
    let mut later = e.clone();
    later["date"] = json!("2026-11-18");
    later["received_unix"] = json!(T0 + 86_400);
    assert!(
        !record_once(&c.records, "application-00", &later)
            .expect("again")
            .1
    );
    assert_eq!(record::read(&file).expect("record").len(), 1);
    let mut refused = e.clone();
    refused["state"] = json!("refused");
    assert!(
        record_once(&c.records, "application-00", &refused)
            .expect("refused")
            .1
    );
    assert_eq!(record::read(&file).expect("record").len(), 2);
}
