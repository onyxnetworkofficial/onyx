//! The proposal reviewer beside a running node (ROADMAP S5, READINESS B2; THREATS REV-Q-*):
//! append-only decisions that survive a restart, refusals, the two locks, the public status,
//! the warning before the recorded cap, the checklist and what the reviewer's terminal prints.

use super::*;
use crate::journal::test_dir;
use crate::submit::{routes, SubmitLimits, SubmitStore, JOURNAL_KIND};
use crate::ClientKey;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

fn vectors() -> Value {
    serde_json::from_slice(include_bytes!("../../../fixtures/intake/vectors.json"))
        .expect("vectors")
}

fn example() -> Value {
    vectors()["example"].clone()
}

fn open_limits() -> SubmitLimits {
    SubmitLimits {
        submit_per_minute: 10_000,
        status_per_minute: 10_000,
        global_submit_per_minute: 10_000,
        submit_per_day: 10_000,
        global_submit_per_day: 10_000,
        ..SubmitLimits::default()
    }
}

fn journal_in(dir: &Path) -> PathBuf {
    dir.join("submissions.journal")
}

fn node(dir: &Path, limits: SubmitLimits) -> Arc<SubmitStore> {
    Arc::new(SubmitStore::open(journal_in(dir), None, limits).expect("open"))
}

async fn call(app: &Router, method: &str, uri: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request");
    req.extensions_mut()
        .insert(ClientKey("198.51.100.7".to_string()));
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v: Value = serde_json::from_slice(&bytes).expect("json");
    // Every answer conforms to docs/api/openapi.json (the new states and `reason` too).
    crate::openapi_check::check(method, uri, status.as_u16(), &v);
    (status, v)
}

async fn propose(app: &Router, body: &Value) -> String {
    let (s, b) = call(
        app,
        "POST",
        "/v1/submit",
        serde_json::to_vec(body).expect("bytes"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    b["id"].as_str().expect("id").to_string()
}

async fn status_of(app: &Router, id: &str) -> Value {
    let (s, b) = call(app, "GET", &format!("/v1/submit/{id}"), vec![]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}

fn yes() -> Checklist {
    Checklist {
        terms_reread_today: "yes".into(),
        no_personal_data: "yes".into(),
        entities_only: "yes".into(),
        source_public_reachable: "yes".into(),
        rights_level: "allowed".into(),
    }
}

/// A time no proposal of this test run is newer than.
fn later(n: u64) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        + 10
        + n
}

const CAP: usize = 10_000;

fn tool(dir: &Path, id: &str, action: Action, n: u64) -> Result<Proposal, DecideError> {
    decide(&journal_in(dir), CAP, id, action, later(n)).map(|done| done.proposal)
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("file")
}

#[test]
fn the_decisions_journal_sits_beside_the_intake_journal_under_a_name_the_backup_reads() {
    assert_eq!(
        decisions_path(Path::new("/var/lib/onyx/state/submissions.journal")),
        Path::new("/var/lib/onyx/state/submissions.decisions.journal")
    );
    assert_eq!(
        decisions_path(Path::new("/x/intake")),
        Path::new("/x/intake.decisions.journal")
    );
}

/// Decisions are appended, never rewritten: each earlier byte of the decisions journal stays,
/// the intake journal is not touched by the tool, and a restart of the node and a new run of
/// the tool read the same states back.
#[tokio::test]
async fn decisions_are_append_only_and_replay_across_a_restart() {
    let dir = test_dir("decisions-append");
    let (journal, book) = (journal_in(&dir), decisions_path(&journal_in(&dir)));
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let a = propose(&app, &example()).await;
    let b = propose(&app, &example()).await;
    let intake_before = read(&journal);
    assert!(!book.exists(), "reading the queue creates nothing");
    assert_eq!(read_queue(&journal, CAP).expect("queue").proposals.len(), 2);
    assert!(!book.exists(), "reading the queue creates nothing");

    let held = tool(&dir, &a, Action::Hold("unreachable_source".into()), 0).expect("hold");
    assert_eq!(held.state(), ON_HOLD);
    let after_hold = read(&book);
    let cleared = tool(&dir, &a, Action::Clear(yes()), 1).expect("clear");
    assert_eq!(cleared.state(), CLEARED);
    assert_eq!(cleared.decided.as_ref().expect("decided").revision, 2);
    let after_clear = read(&book);
    assert!(
        after_clear.starts_with(&after_hold) && after_clear.len() > after_hold.len(),
        "a decision is appended; nothing before it changes"
    );
    assert_eq!(
        read(&journal),
        intake_before,
        "the tool never writes the intake journal"
    );
    // The running node shows each decision without a restart.
    assert_eq!(
        status_of(&app, &a).await,
        json!({"id": a, "status": "cleared"})
    );
    assert_eq!(
        status_of(&app, &b).await,
        json!({"id": b, "status": "pending_review"})
    );

    // A restart of the node replays both journals.
    drop(app);
    drop(store);
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    assert_eq!(
        status_of(&app, &a).await,
        json!({"id": a, "status": "cleared"})
    );
    // A wrong clear is undone by a drop; the history stays.
    tool(&dir, &a, Action::Drop("terms_forbid".into()), 2).expect("drop after clear");
    assert!(read(&book).starts_with(&after_clear));
    assert_eq!(
        status_of(&app, &a).await,
        json!({"id": a, "status": "dropped", "reason": "terms_forbid"})
    );
    drop(app);
    drop(store);
    // The tool alone, with no node: the same states.
    let q = read_queue(&journal, CAP).expect("queue");
    let states: Vec<(&str, &str)> = q
        .proposals
        .iter()
        .map(|p| (p.item.id.as_str(), p.state()))
        .collect();
    assert_eq!(
        states,
        vec![(b.as_str(), "pending_review"), (a.as_str(), "dropped")],
        "waiting proposals come first"
    );
    assert_eq!(read(&journal), intake_before);
    let store = node(&dir, open_limits());
    assert_eq!(store.recorded(), 2, "decisions are not proposals");
}

/// A decision on an unknown id, on a proposal already decided, or on one the offline review
/// decided is refused, and a refusal writes nothing.
#[tokio::test]
async fn a_decision_on_an_unknown_or_already_decided_id_is_refused() {
    let dir = test_dir("decisions-refused");
    let book = decisions_path(&journal_in(&dir));
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let a = propose(&app, &example()).await;
    let b = propose(&app, &example()).await;
    let c = propose(&app, &example()).await;
    let held = propose(&app, &vectors()["fredExample"]).await;

    let unknown = format!("sub_{}", "0".repeat(32));
    for action in [
        Action::Clear(yes()),
        Action::Hold("spam".into()),
        Action::Drop("spam".into()),
    ] {
        assert!(matches!(
            tool(&dir, &unknown, action.clone(), 0),
            Err(DecideError::NotFound)
        ));
        assert!(matches!(
            tool(&dir, "not-an-id", action, 0),
            Err(DecideError::NotFound)
        ));
    }
    tool(&dir, &a, Action::Clear(yes()), 0).expect("clear");
    tool(&dir, &b, Action::Drop("duplicate".into()), 0).expect("drop");
    tool(&dir, &c, Action::Hold("other:asked_source".into()), 0).expect("hold");
    let before = read(&book);
    let refused = [
        (&a, Action::Clear(yes()), "cleared"),
        (&a, Action::Hold("spam".into()), "cleared"),
        (&b, Action::Clear(yes()), "dropped"),
        (&b, Action::Hold("spam".into()), "dropped"),
        (&b, Action::Drop("spam".into()), "dropped"),
        (&c, Action::Hold("spam".into()), "on_hold"),
    ];
    for (id, action, state) in refused {
        match tool(&dir, id, action.clone(), 1) {
            Err(DecideError::AlreadyDecided(s)) => assert_eq!(s, state, "{action:?}"),
            other => panic!("{action:?} on a {state} proposal: {other:?}"),
        }
    }
    // The intake's own hold is not lifted by this tool (ADR-0039 item 4); it can be dropped.
    assert_eq!(
        status_of(&app, &held).await["status"],
        "held_keyed_source_review"
    );
    assert!(matches!(
        tool(&dir, &held, Action::Clear(yes()), 1),
        Err(DecideError::NotClearable(_))
    ));
    // A reason outside the list, a bare `other`, and free text are refused.
    for reason in [
        "",
        "other",
        "other:",
        "other:Has Spaces",
        "because I say so",
        "SPAM",
    ] {
        assert!(matches!(
            tool(&dir, &c, Action::Drop(reason.into()), 1),
            Err(DecideError::Reason)
        ));
    }
    // A clock behind the proposal's last event is refused.
    assert!(matches!(
        decide(
            &journal_in(&dir),
            CAP,
            &c,
            Action::Drop("spam".into()),
            1_000
        ),
        Err(DecideError::Clock)
    ));
    assert_eq!(read(&book), before, "a refusal writes nothing");
    tool(&dir, &held, Action::Drop("terms_forbid".into()), 1).expect("drop a held one");

    // The offline review (node stopped, ADR-0039) decided: this tool refuses, and the offline
    // approval refuses a proposal this tool dropped or put on hold.
    let d = propose(&app, &example()).await;
    tool(&dir, &d, Action::Clear(yes()), 0).expect("clear before the approval");
    let hash = store.review_item(&d).expect("item").hash;
    store
        .review(&d, 0, &hash, "approved", "reviewed", later(0))
        .expect("offline approval");
    match tool(&dir, &d, Action::Drop("spam".into()), 2) {
        Err(DecideError::AlreadyDecided(s)) => assert_eq!(s, "approved"),
        other => panic!("{other:?}"),
    }
    for id in [&b, &c] {
        let hash = store.review_item(id).expect("item").hash;
        assert!(
            store
                .review(id, 0, &hash, "approved", "reviewed", later(3))
                .is_err(),
            "a dropped or held proposal is never approved offline"
        );
    }
    let hash = store.review_item(&a).expect("item").hash;
    store
        .review(&a, 0, &hash, "approved", "reviewed", later(3))
        .expect("a cleared proposal can take the offline approval");
    assert_eq!(
        status_of(&app, &a).await,
        json!({"id": a, "status": "approved"})
    );
}

/// The node keeps recording while the tool decides: the tool never takes the intake journal's
/// lock (the node holds it throughout) and the node never takes the decisions lock. A second
/// reviewer is refused while the first holds the decisions lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tool_decides_while_the_node_writes_and_two_reviewers_never_write_at_once() {
    let dir = test_dir("decisions-concurrent");
    let journal = journal_in(&dir);
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let mut first = Vec::new();
    for _ in 0..8 {
        first.push(propose(&app, &example()).await);
    }
    let writer = {
        let app = app.clone();
        tokio::spawn(async move {
            let mut ids = Vec::new();
            for _ in 0..40 {
                ids.push(propose(&app, &example()).await);
            }
            ids
        })
    };
    let reviewer = {
        let (dir, first) = (dir.clone(), first.clone());
        tokio::task::spawn_blocking(move || {
            for (n, id) in first.iter().enumerate() {
                let action = match n % 3 {
                    0 => Action::Clear(yes()),
                    1 => Action::Hold("out_of_scope".into()),
                    _ => Action::Drop("spam".into()),
                };
                tool(&dir, id, action, 0).expect("decide beside the running node");
                read_queue(&journal_in(&dir), CAP).expect("list beside the running node");
            }
        })
    };
    let later_ids = writer.await.expect("writer");
    reviewer.await.expect("reviewer");
    assert_eq!(store.recorded(), 48, "no proposal was lost to the reviewer");
    for (n, id) in first.iter().enumerate() {
        let want = ["cleared", "on_hold", "dropped"][n % 3];
        assert_eq!(status_of(&app, id).await["status"], want);
    }
    assert_eq!(
        status_of(&app, &later_ids[39]).await["status"],
        "pending_review"
    );

    // A second writer of the decisions journal is refused while one holds its lock, and the
    // node still reads and records.
    let (holder, _) = Journal::open(decisions_path(&journal), DECISIONS_KIND, &BINDING)
        .expect("the first reviewer's lock");
    let d = dir.clone();
    let id = later_ids[0].clone();
    let second = tokio::task::spawn_blocking(move || tool(&d, &id, Action::Drop("spam".into()), 1))
        .await
        .expect("join");
    match second {
        Err(DecideError::Storage(e)) => assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock),
        other => panic!("a second reviewer must be refused: {other:?}"),
    }
    propose(&app, &example()).await;
    assert_eq!(status_of(&app, &first[0]).await["status"], "cleared");
    drop(holder);
    tool(&dir, &later_ids[0], Action::Drop("spam".into()), 1).expect("after the lock is free");

    // Both journals replay whole after the node stops.
    drop(app);
    drop(store);
    let q = read_queue(&journal, CAP).expect("queue");
    assert_eq!(q.proposals.len(), 49);
    assert_eq!(q.proposals.iter().filter(|p| p.waiting()).count(), 43);
    assert_eq!(node(&dir, open_limits()).recorded(), 49);
}

/// `GET /v1/submit/{id}` shows the three new states in fixed words: the state, and for a hold or
/// a drop a reason code from the list. A reviewer's own code after `other:` never leaves the
/// node, and the answer has no other field.
#[tokio::test]
async fn the_status_route_shows_the_states_and_only_fixed_reason_codes() {
    let dir = test_dir("decisions-status");
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    for reason in REASONS {
        let given = if *reason == "other" {
            "other:private_note_7".to_string()
        } else {
            (*reason).to_string()
        };
        let hold = propose(&app, &example()).await;
        tool(&dir, &hold, Action::Hold(given.clone()), 0).expect("hold");
        assert_eq!(
            status_of(&app, &hold).await,
            json!({"id": hold, "status": "on_hold", "reason": reason})
        );
        let drop = propose(&app, &example()).await;
        tool(&dir, &drop, Action::Drop(given), 0).expect("drop");
        let body = status_of(&app, &drop).await;
        assert_eq!(
            body,
            json!({"id": drop, "status": "dropped", "reason": reason})
        );
        assert!(!body.to_string().contains("private_note"));
    }
    let clear = propose(&app, &example()).await;
    tool(&dir, &clear, Action::Clear(yes()), 0).expect("clear");
    let body = status_of(&app, &clear).await;
    assert_eq!(body, json!({"id": clear, "status": "cleared"}));
    // Nothing of the proposal or the checklist is in a public answer.
    for private in ["Treasury", "checklist", "allowed", "hash", "yes"] {
        assert!(!body.to_string().contains(private), "{private}: {body}");
    }
}

/// The warning before the recorded cap: from 80% of the cap, with its fixed tag, counts only.
#[tokio::test]
async fn the_node_warns_from_80_percent_of_the_recorded_cap() {
    assert_eq!(cap_alert(7_999, 10_000), None);
    let line = cap_alert(8_000, 10_000).expect("80% warns");
    assert!(
        line.starts_with(
            "onyx-intake-cap-alert: the intake journal holds 8000 of 10000 proposals (80%"
        ),
        "{line}"
    );
    assert!(cap_alert(10_000, 10_000).is_some());
    assert!(!cap_alert_due(7_999, 10_000));
    assert!(
        cap_alert_due(8_000, 10_000),
        "the proposal that reaches the line"
    );
    assert!(!cap_alert_due(8_001, 10_000), "not on every proposal");
    assert!(
        cap_alert_due(8_100, 10_000),
        "again at each further percent"
    );
    assert_eq!(
        (1..=10_000).filter(|n| cap_alert_due(*n, 10_000)).count(),
        21
    );
    assert_eq!(
        fill_percent(3, 0),
        100,
        "a zero cap is full, never a division"
    );
    assert!(fill_line(8_000, 10_000).contains("WARNING"));
    assert!(!fill_line(7_999, 10_000).contains("WARNING"));

    // The node itself: a cap of 5, so the 4th proposal is 80%.
    let dir = test_dir("decisions-cap");
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = lines.clone();
    let limits = SubmitLimits {
        max_recorded: 5,
        ..open_limits()
    };
    let open = |sink: Arc<Mutex<Vec<String>>>| {
        Arc::new(
            SubmitStore::open(journal_in(&dir), None, limits)
                .expect("open")
                .with_cap_alert(Arc::new(move |l| {
                    sink.lock().expect("lines").push(l.to_string());
                })),
        )
    };
    let store = open(sink);
    let app = routes(store.clone());
    for _ in 0..3 {
        propose(&app, &example()).await;
    }
    assert_eq!(store.warn_if_near_cap(), None);
    assert!(
        lines.lock().expect("lines").is_empty(),
        "no warning under 80%"
    );
    propose(&app, &example()).await;
    propose(&app, &example()).await;
    let got = lines.lock().expect("lines").clone();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].starts_with(&format!(
        "{CAP_ALERT_TAG}: the intake journal holds 4 of 5 proposals (80%"
    )));
    assert!(got[1].contains("5 of 5 proposals (100%"));
    for l in &got {
        assert!(
            !l.contains("Treasury") && !l.contains("sub_"),
            "counts only: {l}"
        );
    }
    // A node started over the line says so once at start.
    drop(app);
    drop(store);
    let again = Arc::new(Mutex::new(Vec::new()));
    let store = open(again.clone());
    assert!(store.warn_if_near_cap().is_some());
    assert_eq!(again.lock().expect("lines").len(), 1);
    // The reviewer's list shows the same fill level.
    let q = read_queue(&journal_in(&dir), 5).expect("queue");
    let text = render_list(&q, later(0), false);
    assert!(
        text.text()
            .starts_with("Journal: 5 of 5 proposals recorded (100% of the cap). WARNING"),
        "{}",
        text.text()
    );
}

/// `clear` needs every checklist line answered yes and a rights level; anything less is refused
/// before a journal is opened.
#[tokio::test]
async fn clear_without_the_full_checklist_is_refused() {
    let dir = test_dir("decisions-checklist");
    let book = decisions_path(&journal_in(&dir));
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let id = propose(&app, &example()).await;
    type Edit = fn(&mut Checklist, &str);
    let lines: [(&str, Edit); 5] = [
        ("--terms-reread", |c, v| c.terms_reread_today = v.into()),
        ("--no-personal-data", |c, v| c.no_personal_data = v.into()),
        ("--entities-only", |c, v| c.entities_only = v.into()),
        ("--source-public", |c, v| {
            c.source_public_reachable = v.into();
        }),
        ("--rights-level", |c, v| c.rights_level = v.into()),
    ];
    for (flag, edit) in lines {
        for answer in ["", "no", "YES", "y", "maybe", "forbidden"] {
            let mut list = yes();
            edit(&mut list, answer);
            match tool(&dir, &id, Action::Clear(list), 0) {
                Err(DecideError::Checklist(f)) => assert_eq!(f, flag, "{answer:?}"),
                other => panic!("{flag}={answer:?}: {other:?}"),
            }
        }
    }
    assert!(!book.exists(), "a refused clear writes nothing");
    assert_eq!(status_of(&app, &id).await["status"], "pending_review");
    let mut restricted = yes();
    restricted.rights_level = "restricted".into();
    let done = tool(&dir, &id, Action::Clear(restricted.clone()), 0).expect("clear");
    assert_eq!(
        done.decided.expect("decided").checklist,
        Some(restricted),
        "the answers are kept with the decision"
    );
    // The journal line carries the checklist, and names nobody.
    let line = String::from_utf8(read(&book)).expect("utf8");
    for part in [
        r#""checklist":{"entitiesOnly":"yes","noPersonalData":"yes","rightsLevel":"restricted","sourcePublicReachable":"yes","termsRereadToday":"yes"}"#,
        r#""from":"pending_review""#,
        r#""kind":"decision""#,
        r#""revision":1,"submissionId":"sub_"#,
        r#""to":"cleared","version":1"#,
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
}

/// Text shaped like an e-mail address, put together here so that the tree holds none.
fn address() -> String {
    ["someone", "example.org"].join("@")
}

fn forged(title: &str, url: &str) -> Proposal {
    let mut s = onyx_intake::Submission::parse(&serde_json::to_vec(&example()).expect("bytes"))
        .expect("example");
    s.title = title.into();
    s.source_url = url.into();
    Proposal {
        item: ReviewItem {
            id: format!("sub_{}", "a".repeat(32)),
            hash: format!("0x{}", "b".repeat(64)),
            status: PENDING_REVIEW.into(),
            revision: 0,
            received_unix: 1,
            reviewed_unix: None,
            reason: None,
            submission: s,
        },
        decided: None,
    }
}

/// Nothing shaped like an e-mail address or a secret reaches the reviewer's terminal. The intake
/// refuses both in the fields it checks and the replay checks every proposal again; the printer
/// is the last line: it withholds the word, counts it and prints the rest.
#[tokio::test]
async fn nothing_shaped_like_an_email_or_a_secret_is_ever_printed() {
    // Every proposal the shared vectors accept, recorded by a node and printed by the tool.
    let dir = test_dir("decisions-print");
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let mut recorded = 0;
    for v in vectors()["submissions"].as_array().expect("vectors") {
        let (s, _) = call(
            &app,
            "POST",
            "/v1/submit",
            serde_json::to_vec(&v["submission"]).expect("bytes"),
        )
        .await;
        recorded += usize::from(s == StatusCode::CREATED);
    }
    assert!(recorded >= 10, "{recorded} vectors recorded");
    let q = read_queue(&journal_in(&dir), CAP).expect("queue");
    assert_eq!(q.proposals.len(), recorded);
    let list = render_list(&q, later(0), true);
    assert_eq!(list.withheld, 0);
    assert!(!unprintable(list.text()), "{}", list.text());
    assert_eq!(list.text().lines().count(), recorded + 2);
    for p in &q.proposals {
        let shown = render_show(p, later(0));
        assert_eq!(shown.withheld, 0, "{}", shown.text());
        assert!(!unprintable(shown.text()), "{}", shown.text());
        assert!(shown.text().contains(&p.item.id));
    }

    // A journal that does hold one (written by something other than the node) is refused whole.
    let bad_dir = test_dir("decisions-print-forged");
    let mut entry = example();
    entry["description"] = json!(format!("write to {} about this", address()));
    let hash = format!("0x{}", "c".repeat(64));
    {
        let (mut j, _) = Journal::open(journal_in(&bad_dir), JOURNAL_KIND, &BINDING).expect("j");
        j.apply(
            &[],
            &[
                json!({"kind":"submission","version":1,"hash":hash,"status":"pending_review",
                "receivedUnix":1,"route":{},"submission":entry}),
            ],
        )
        .expect("forged line");
    }
    assert!(read_queue(&journal_in(&bad_dir), CAP).is_err());

    // The printer itself: a field with either shape is withheld and counted, in list and show.
    // The made-up values are joined here, so that a secret scanner reading this file sees no
    // name next to a value.
    let ask = format!("Ask {}", address());
    let (first, second) = ("0123456789abcdef", "abcdef0123456789");
    let keyed = format!("https://example.org/a?api_key={first}");
    let in_text = format!("see https://example.org/x?token={second} for it");
    let cases = [
        (ask.as_str(), "https://example.org/a"),
        ("mailto:someone", "https://example.org/a"),
        ("A title", keyed.as_str()),
        (in_text.as_str(), "https://example.org/a"),
    ];
    for (title, url) in cases {
        let p = forged(title, url);
        let shown = render_show(&p, 100);
        assert!(shown.withheld >= 1, "{title} {url}");
        assert!(shown.text().contains(WITHHELD));
        for leak in ["someone", first, second] {
            assert!(!shown.text().contains(leak), "{leak}: {}", shown.text());
        }
        assert!(!unprintable(shown.text()));
    }
    let mut p = forged("A title", "https://example.org/a");
    p.item.submission.answers.license_id = Some(address());
    let q = Queue {
        proposals: vec![p],
        capacity: CAP,
        orphans: Vec::new(),
        damaged_lines: 0,
        foreign: false,
        refused_lines: 0,
    };
    let list = render_list(&q, 100, false);
    assert_eq!(list.withheld, 1);
    assert!(!list.text().contains("someone"), "{}", list.text());
    // A control character never moves the reviewer's terminal.
    let p = forged("A \u{1b}[2J title", "https://example.org/a");
    assert!(render_show(&p, 100).text().contains("A \\u{1b}[2J title"));
    // The list shows the receipt id, the state, the age, the host and the rights answers.
    let p = forged("A title", "https://Data.Example.org/a");
    let id = p.item.id.clone();
    let q = Queue {
        proposals: vec![p],
        capacity: CAP,
        orphans: Vec::new(),
        damaged_lines: 0,
        foreign: false,
        refused_lines: 0,
    };
    let text = render_list(&q, 1 + 3 * 86_400 + 2 * 3_600, false);
    let line = text.text().lines().nth(2).expect("one proposal");
    assert!(line.starts_with(&id), "{line}");
    for part in [
        "pending_review",
        "age 3d 2h",
        "host data.example.org",
        "rights=open_license (onyx:us-treasury-fiscal-data-open)",
        "what=public_api",
        "no_login=yes",
        "names_person=no",
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
    assert!(!line.contains("A title"), "no proposal text in the list");
}

fn append_raw(book: &Path, line: Value) {
    let (mut j, _) = Journal::open(book, DECISIONS_KIND, &BINDING).expect("book");
    j.apply(&[], &[line])
        .expect("a line written around the tool");
}

fn alerts(store: &SubmitStore) -> String {
    store.decision_alerts().join("\n")
}

/// Review B2-R1 M3: nothing in the decisions journal is permanent damage. A damaged line is
/// ignored by the list and by the node (which starts, and keeps what it knew while it runs);
/// the reviewer's next decision copies the damaged file aside and writes the journal afresh
/// from its good lines. A well-formed line that breaks the rules is counted and ignored and
/// never shown; the tool goes on deciding.
#[tokio::test]
async fn a_damaged_decisions_journal_is_set_aside_and_nothing_stops() {
    let dir = test_dir("decisions-bad");
    let journal = journal_in(&dir);
    let book = decisions_path(&journal);
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let a = propose(&app, &example()).await;
    let b = propose(&app, &example()).await;
    let c = propose(&app, &example()).await;
    tool(&dir, &a, Action::Hold("duplicate".into()), 0).expect("hold");
    tool(&dir, &b, Action::Drop("spam".into()), 0).expect("drop");
    assert_eq!(status_of(&app, &a).await["status"], "on_hold");
    assert_eq!(status_of(&app, &b).await["status"], "dropped");
    assert_eq!(alerts(&store), "", "a good journal alerts nothing");

    // One byte of the second line (the hold) flips on disk.
    let mut damaged = read(&book);
    let at = damaged.iter().position(|x| *x == b'\n').expect("header") + 30;
    damaged[at] ^= 1;
    std::fs::write(&book, &damaged).expect("damage");
    // The running node keeps what it knew and says so once.
    assert_eq!(status_of(&app, &a).await["status"], "on_hold");
    assert_eq!(status_of(&app, &b).await["status"], "dropped");
    assert_eq!(status_of(&app, &a).await["status"], "on_hold");
    let said = alerts(&store);
    assert_eq!(said.matches(DECISIONS_ALERT_TAG).count(), 1, "{said}");
    assert!(said.contains("1 damaged line(s)"), "{said}");
    assert!(!said.contains("sub_"), "counts only: {said}");
    // The list still works and says what it ignored.
    let q = read_queue(&journal, CAP).expect("the list is not stopped by a damaged line");
    assert_eq!((q.damaged_lines, q.refused_lines), (1, 0));
    let text = render_list(&q, later(0), true);
    assert!(
        text.text()
            .contains("WARNING: the decisions journal has 1 damaged and 0 rule-breaking line(s)"),
        "{}",
        text.text()
    );
    // A node starts over it. The damaged decision is gone from its view; the good one stays.
    drop(app);
    drop(store);
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    assert!(alerts(&store).contains("1 damaged line(s)"));
    assert_eq!(status_of(&app, &a).await["status"], "pending_review");
    assert_eq!(status_of(&app, &b).await["status"], "dropped");

    // Review B2-R2 M1: a decision that is refused changes nothing, also over a damaged file:
    // no copy, no rewrite.
    let corrupt_copies = || {
        std::fs::read_dir(&dir)
            .expect("dir")
            .filter(|e| {
                let name = e.as_ref().expect("entry").file_name();
                name.to_string_lossy().contains(".corrupt-")
            })
            .count()
    };
    let unknown = format!("sub_{}", "0".repeat(32));
    assert!(matches!(
        decide(
            &journal,
            CAP,
            &unknown,
            Action::Drop("spam".into()),
            later(1)
        ),
        Err(DecideError::NotFound)
    ));
    assert!(matches!(
        decide(&journal, CAP, &b, Action::Hold("spam".into()), later(1)),
        Err(DecideError::AlreadyDecided(_))
    ));
    assert!(matches!(
        decide(&journal, CAP, &c, Action::Drop("my words".into()), later(1)),
        Err(DecideError::Reason)
    ));
    assert_eq!(read(&book), damaged, "a refused decision rewrites nothing");
    assert_eq!(corrupt_copies(), 0, "and sets nothing aside");

    // The next decision that is allowed sets the damaged file aside, whole, and writes the
    // journal afresh.
    let done = decide(&journal, CAP, &c, Action::Clear(yes()), later(1)).expect("decide");
    let copy = done.set_aside.expect("the damaged file was set aside");
    assert!(
        copy.to_string_lossy()
            .contains("submissions.decisions.journal.corrupt-"),
        "{copy:?}"
    );
    assert_eq!(
        read(&copy),
        damaged,
        "the copy is the damaged file, byte for byte"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&copy).expect("copy").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    Journal::read_only(&book, DECISIONS_KIND, &BINDING)
        .expect("the fresh journal replays strictly");
    let q = read_queue(&journal, CAP).expect("queue");
    assert_eq!((q.damaged_lines, q.refused_lines), (0, 0));
    let state = |q: &Queue, id: &str| {
        q.proposals
            .iter()
            .find(|p| p.item.id == id)
            .expect("proposal")
            .state()
            .to_string()
    };
    assert_eq!(state(&q, &a), "pending_review");
    assert_eq!(
        state(&q, &b),
        "dropped",
        "a good line after the damage is kept"
    );
    assert_eq!(state(&q, &c), "cleared");
    assert_eq!(status_of(&app, &c).await["status"], "cleared");
    let next = decide(
        &journal,
        CAP,
        &a,
        Action::Hold("duplicate".into()),
        later(2),
    )
    .expect("a");
    assert_eq!(
        next.set_aside, None,
        "a good journal is not set aside again"
    );
    let good = read(&book);

    // Well-formed lines that break the rules: each is counted and ignored, never applied.
    let d = propose(&app, &example()).await;
    let item = store.review_item(&d).expect("item");
    let base = json!({"kind":"decision","version":1,"submissionId":d,"hash":item.hash,
        "revision":1,"from":"pending_review","to":"dropped","reason":"spam","decidedUnix":later(0)});
    let mut no_checklist = base.clone();
    no_checklist["to"] = json!("cleared");
    no_checklist
        .as_object_mut()
        .expect("object")
        .remove("reason");
    let forgeries: [(&str, Value); 8] = [
        ("hash", json!(format!("0x{}", "0".repeat(64)))),
        ("revision", json!(2)),
        ("from", json!("on_hold")),
        ("reason", json!("the reviewer's own words")),
        ("to", json!("approved")),
        ("version", json!(2)),
        ("note", json!("free text")),
        ("decidedUnix", json!(1)),
    ];
    let mut lines: Vec<Value> = forgeries
        .into_iter()
        .map(|(field, value)| {
            let mut line = base.clone();
            line[field] = value;
            line
        })
        .collect();
    lines.push(no_checklist);
    lines.push(json!("not a decision at all"));
    for (n, line) in lines.iter().enumerate() {
        std::fs::write(&book, &good).expect("restore");
        append_raw(&book, line.clone());
        let q = read_queue(&journal, CAP).expect("queue");
        assert_eq!((q.damaged_lines, q.refused_lines), (0, 1), "forgery {n}");
        assert_eq!(
            state(&q, &d),
            "pending_review",
            "forgery {n} is never applied"
        );
        assert_eq!(
            status_of(&app, &d).await,
            json!({"id": d, "status": "pending_review"}),
            "forgery {n} is never shown"
        );
    }
    assert!(alerts(&store).contains("break the decision rules"));
    // The tool goes on deciding over a rule-breaking line, with no set-aside.
    let done = decide(&journal, CAP, &d, Action::Drop("spam".into()), later(3)).expect("drop");
    assert_eq!(done.set_aside, None);
    assert_eq!(status_of(&app, &d).await["status"], "dropped");

    // Review B2-R2 M1: a file that does not start with the decisions journal's header (another
    // journal under its name, or its header line damaged) is never rewritten: with nothing to
    // salvage, a rewrite would drop every decision from the live file. The decision fails, the
    // file stays as it is for the operator, and the running node keeps what it knew.
    let before = read(&book);
    let copies = corrupt_copies();
    let e = propose(&app, &example()).await;
    let mut header_hit = before.clone();
    header_hit[40] ^= 1;
    for (what, bytes) in [
        ("another journal", read(&journal)),
        ("a damaged header", header_hit),
    ] {
        std::fs::write(&book, &bytes).expect("swap");
        let q = read_queue(&journal, CAP).expect("the list still works");
        assert!(q.foreign, "{what}");
        assert_eq!(
            state(&q, &b),
            "pending_review",
            "{what}: nothing of it is read"
        );
        let text = render_list(&q, later(0), false);
        assert!(text.text().contains("is not a decisions journal"), "{what}");
        match decide(&journal, CAP, &e, Action::Drop("spam".into()), later(4)) {
            Err(DecideError::Storage(err)) => {
                assert!(err.to_string().contains("left as it is"), "{what}: {err}");
            }
            other => panic!("{what}: the decision must stop, got {other:?}"),
        }
        assert_eq!(read(&book), bytes, "{what}: the file is not rewritten");
        assert_eq!(corrupt_copies(), copies, "{what}: nothing is set aside");
        assert_eq!(
            status_of(&app, &b).await["status"],
            "dropped",
            "{what}: last known"
        );
        assert_eq!(status_of(&app, &e).await["status"], "pending_review");
    }
    assert!(
        alerts(&store).contains("never rewritten"),
        "{}",
        alerts(&store)
    );
    // The operator puts the last good copy back: decisions go on.
    std::fs::write(&book, &before).expect("restore");
    let done = decide(&journal, CAP, &e, Action::Drop("spam".into()), later(4)).expect("e");
    assert_eq!(done.set_aside, None);
    assert_eq!(status_of(&app, &e).await["status"], "dropped");
    assert_eq!(status_of(&app, &b).await["status"], "dropped");

    // A torn last line (a decision being written) is left out, not damage, and left on disk.
    let mut torn = read(&book);
    torn.extend_from_slice(b"0123456789abcdef {\"ops\":[{\"add\":");
    std::fs::write(&book, &torn).expect("torn");
    let q = read_queue(&journal, CAP).expect("queue");
    assert_eq!((q.damaged_lines, state(&q, &e).as_str()), (0, "dropped"));
    assert_eq!(read(&book), torn, "a reader never cuts the file");
    // No intake journal: the tool says so and creates nothing.
    let empty = test_dir("decisions-none");
    assert!(read_queue(&journal_in(&empty), CAP).is_err());
    assert!(tool(&empty, &a, Action::Drop("spam".into()), 0).is_err());
    assert_eq!(std::fs::read_dir(&empty).expect("dir").count(), 0);
}

/// Review B2-R1 H1: the node's own user owns the state directory and can plant a symbolic
/// link under the decisions journal's name, its lock's name or the intake journal's name. The
/// tool refuses each: nothing is created or changed where a link points, and the node itself
/// does not read through one.
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_under_a_journal_name_is_refused_and_never_followed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = test_dir("decisions-symlink");
    let outside = test_dir("decisions-symlink-outside");
    let journal = journal_in(&dir);
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let a = propose(&app, &example()).await;
    let book = decisions_path(&journal);
    let lock = lock_of(&book);
    let created = outside.join("created-by-the-tool");
    let victim = outside.join("victim");
    let mode = |p: &Path| std::fs::metadata(p).expect("file").permissions().mode() & 0o777;
    let plant_victim = || {
        std::fs::write(&victim, b"someone else's file").expect("victim");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("mode");
    };
    let untouched = |what: &str| {
        assert!(!created.exists(), "{what}: a file was created outside");
        assert_eq!(read(&victim), b"someone else's file", "{what}");
        assert_eq!(mode(&victim), 0o644, "{what}: a mode outside was changed");
        assert_eq!(
            std::fs::read_dir(&outside).expect("dir").count(),
            1,
            "{what}"
        );
    };
    let refused = |what: &str| {
        let e = match tool(&dir, &a, Action::Hold("duplicate".into()), 0) {
            Err(DecideError::Storage(e)) => e.to_string(),
            other => panic!("{what}: the tool must refuse a link, got {other:?}"),
        };
        assert!(e.contains("symbolic link"), "{what}: {e}");
        assert!(read_queue(&journal, CAP).is_err(), "{what}: list");
        untouched(what);
    };
    plant_victim();

    // The attack of the review: the journal's name to a new file, the lock's to a victim.
    symlink(&created, &book).expect("planted journal link");
    symlink(&victim, &lock).expect("planted lock link");
    refused("both names");
    // Each name alone, to an existing file and to nothing.
    std::fs::remove_file(&lock).expect("unlink");
    refused("the journal's name, dangling");
    std::fs::remove_file(&book).expect("unlink");
    symlink(&victim, &book).expect("planted journal link");
    refused("the journal's name, to a file");
    std::fs::remove_file(&book).expect("unlink");
    symlink(&victim, &lock).expect("planted lock link");
    refused("the lock's name, to a file");
    std::fs::remove_file(&lock).expect("unlink");
    symlink(&created, &lock).expect("planted lock link");
    refused("the lock's name, dangling");
    std::fs::remove_file(&lock).expect("unlink");
    // The journal's own open refuses a link too, with no check before it (no gap to race).
    symlink(&victim, &book).expect("planted journal link");
    assert!(Journal::open(&book, DECISIONS_KIND, &BINDING).is_err());
    assert!(Journal::read_only(&book, DECISIONS_KIND, &BINDING).is_err());
    assert!(Journal::read_lenient(&book, DECISIONS_KIND, &BINDING, u64::MAX, usize::MAX).is_err());
    std::fs::remove_file(&lock).expect("the refused open's own lock file");
    untouched("the journal's own open");
    // The running node does not read through the link: it alerts and shows what it knew.
    assert_eq!(status_of(&app, &a).await["status"], "pending_review");
    assert!(
        alerts(&store).contains("symbolic link"),
        "{}",
        alerts(&store)
    );
    std::fs::remove_file(&book).expect("unlink");
    // A directory under the name is refused as well.
    std::fs::create_dir(&book).expect("dir");
    assert!(tool(&dir, &a, Action::Hold("duplicate".into()), 0).is_err());
    std::fs::remove_dir(&book).expect("rmdir");
    // With the links gone the tool works, and its files are plain and owner-only.
    tool(&dir, &a, Action::Hold("duplicate".into()), 0).expect("hold");
    for p in [&book, &lock] {
        let m = std::fs::symlink_metadata(p).expect("file");
        assert!(m.file_type().is_file());
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
    }
    untouched("after a real decision");

    // The intake journal's own name as a link is refused before anything is opened.
    drop(app);
    drop(store);
    let elsewhere = test_dir("decisions-symlink-intake");
    let real = elsewhere.join("moved.journal");
    std::fs::rename(&journal, &real).expect("move");
    symlink(&real, &journal).expect("link");
    assert!(read_queue(&journal, CAP).is_err());
    match tool(&dir, &a, Action::Drop("spam".into()), 1) {
        Err(DecideError::Storage(e)) => assert!(e.to_string().contains("symbolic link"), "{e}"),
        other => panic!("{other:?}"),
    }
}

/// Review B2-R1 M1: the intake journal is archived (a new one starts) or restored from an
/// older copy while the decisions journal stays. The decisions on receipts the intake journal
/// no longer holds are skipped with an alert: the node starts, the list shows them as
/// orphaned, and new proposals are decided as before.
#[tokio::test]
async fn decisions_on_receipts_the_intake_journal_no_longer_holds_are_skipped() {
    let dir = test_dir("decisions-orphans");
    let journal = journal_in(&dir);
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let a = propose(&app, &example()).await;
    let older = read(&journal);
    let b = propose(&app, &example()).await;
    tool(&dir, &a, Action::Hold("duplicate".into()), 0).expect("hold");
    tool(&dir, &b, Action::Drop("personal_data".into()), 0).expect("drop");
    drop(app);
    drop(store);

    // Restored from a copy taken before `b` was proposed: `b`'s decision is orphaned.
    std::fs::write(&journal, &older).expect("restore an older intake journal");
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let said = alerts(&store);
    assert!(
        said.contains(DECISIONS_ALERT_TAG)
            && said.contains("1 decision(s) are about receipts the intake journal does not hold"),
        "{said}"
    );
    assert!(!said.contains(&b), "counts only: {said}");
    assert_eq!(status_of(&app, &a).await["status"], "on_hold");
    let q = read_queue(&journal, CAP).expect("the list works");
    assert_eq!(q.orphans, vec![(b.clone(), "dropped".to_string())]);
    let text = render_list(&q, later(0), false);
    assert!(
        text.text().contains("WARNING: 1 orphaned decision(s)"),
        "{}",
        text.text()
    );
    assert!(text.text().contains(&format!("  orphaned  {b}  dropped")));
    drop(app);
    drop(store);

    // Archived by hand, as an erase would do it: a new intake journal beside the old decisions.
    std::fs::remove_file(&journal).expect("the operator archives the intake journal");
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    assert!(alerts(&store).contains("2 decision(s) are about receipts"));
    // (Another title: the same content at the same place in a new journal would get the old
    // receipt id again, which is why both journals are archived together.)
    let mut other = example();
    other["title"] = json!("Another proposal");
    let c = propose(&app, &other).await;
    assert_eq!(status_of(&app, &c).await["status"], "pending_review");
    let q = read_queue(&journal, CAP).expect("the list works");
    assert_eq!(q.orphans.len(), 2);
    assert_eq!(q.proposals.len(), 1);
    tool(&dir, &c, Action::Drop("spam".into()), 1).expect("a new proposal is decided");
    assert_eq!(
        status_of(&app, &c).await,
        json!({"id": c, "status": "dropped", "reason": "spam"})
    );
    // An orphaned receipt id is unknown to the tool and to the public route.
    assert!(matches!(
        tool(&dir, &b, Action::Drop("spam".into()), 2),
        Err(DecideError::NotFound)
    ));
    let (code, _) = call(&app, "GET", &format!("/v1/submit/{b}"), vec![]).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

/// Review B2-R1 M2: a stranger's free text may hold a URL with a secret parameter (the intake
/// checks that in the URL fields only) and characters that hide or reorder text. `show` prints
/// the proposal with only that word withheld and every such character written out, and it
/// does not fail.
#[tokio::test]
async fn a_strangers_text_cannot_fail_the_show_or_reorder_the_terminal() {
    let dir = test_dir("decisions-stranger");
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let mut body = example();
    let secret = ["abc", "123"].concat();
    body["description"] = json!(format!(
        "see https://data.example/x?api_{}={secret} for it. The rest stays readable.",
        "key"
    ));
    body["title"] = json!("Reserves \u{202e}detsurt\u{2066} \u{200b}x");
    let id = propose(&app, &body).await;
    let q = read_queue(&journal_in(&dir), CAP).expect("queue");
    let p = q.proposals.iter().find(|p| p.item.id == id).expect("it");
    let shown = render_show(p, later(0));
    assert_eq!(shown.withheld, 1, "one word is withheld");
    let text = shown.text();
    assert!(
        text.contains("see [withheld] for it. The rest stays readable."),
        "{text}"
    );
    assert!(!text.contains(&secret), "{text}");
    assert!(
        text.contains("Reserves \\u{202e}detsurt\\u{2066} \\u{200b}x"),
        "{text}"
    );
    for c in text.chars() {
        assert!(
            c == '\n' || !invisible(c),
            "U+{:04X} reached the terminal",
            u32::from(c)
        );
    }
    // Every character a terminal could act on is written out, in every printed field.
    let mut printer = Printer::default();
    for c in [
        '\u{0}',
        '\u{7}',
        '\u{8}',
        '\u{1b}',
        '\u{7f}',
        '\u{9b}',
        '\r',
        '\n',
        '\t',
        '\u{ad}',
        '\u{61c}',
        '\u{200e}',
        '\u{200f}',
        '\u{202a}',
        '\u{202b}',
        '\u{202c}',
        '\u{202d}',
        '\u{202e}',
        '\u{2066}',
        '\u{2067}',
        '\u{2068}',
        '\u{2069}',
        '\u{2028}',
        '\u{2029}',
        '\u{feff}',
        '\u{e0001}',
    ] {
        let out = printer.safe(&format!("a{c}b"));
        assert_eq!(out, format!("a\\u{{{:x}}}b", u32::from(c)));
    }
    assert_eq!(
        printer.safe("plain words, é and 数 stay"),
        "plain words, é and 数 stay"
    );
    assert_eq!(printer.withheld, 0);
}

/// Review B2-R1 L1: the offline approval, which lets a proposal be fed to the observers, needs
/// the reviewer's clear first: no proposal is approved with no checklist on record.
#[tokio::test]
async fn the_offline_approval_needs_the_reviewers_clear() {
    let dir = test_dir("decisions-approval");
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let a = propose(&app, &example()).await;
    let before = store.review_item(&a).expect("item");
    let approve = |at: u64| {
        store.review(
            &a,
            before.revision,
            &before.hash,
            "approved",
            "reviewed",
            at,
        )
    };
    assert!(approve(later(0)).is_err(), "undecided: no approval");
    assert_eq!(status_of(&app, &a).await["status"], "pending_review");
    tool(&dir, &a, Action::Hold("unreachable_source".into()), 0).expect("hold");
    assert!(approve(later(1)).is_err(), "on hold: no approval");
    tool(&dir, &a, Action::Clear(yes()), 1).expect("clear");
    assert_eq!(
        approve(later(2)).expect("cleared: approved").status,
        "approved"
    );
    // A rejection never needed a clear.
    let b = propose(&app, &example()).await;
    let item = store.review_item(&b).expect("item");
    store
        .review(&b, 0, &item.hash, "rejected", "duplicate", later(0))
        .expect("rejected");
}

/// Review B2-R1 L2: a decisions journal that was there and vanished, or cannot be read, or is
/// over its size, is an alert and never "no decisions": the node keeps the last known states.
/// The file is read once per change, and not again after a read that failed.
#[tokio::test]
async fn a_vanished_or_unreadable_decisions_journal_keeps_the_last_known_states() {
    let dir = test_dir("decisions-vanish");
    let journal = journal_in(&dir);
    let book = decisions_path(&journal);
    let store = node(&dir, open_limits());
    let app = routes(store.clone());
    let a = propose(&app, &example()).await;
    let b = propose(&app, &example()).await;
    assert_eq!(store.decision_reads(), 0, "no file, nothing to read");
    for _ in 0..3 {
        status_of(&app, &a).await;
    }
    assert_eq!(store.decision_reads(), 0);
    tool(&dir, &a, Action::Drop("spam".into()), 0).expect("drop");
    for _ in 0..3 {
        assert_eq!(status_of(&app, &a).await["status"], "dropped");
        assert_eq!(status_of(&app, &b).await["status"], "pending_review");
    }
    assert_eq!(
        store.decision_reads(),
        1,
        "read once per change, not per request"
    );
    tool(&dir, &b, Action::Hold("duplicate".into()), 0).expect("hold");
    assert_eq!(status_of(&app, &b).await["status"], "on_hold");
    assert_eq!(status_of(&app, &b).await["status"], "on_hold");
    assert_eq!(store.decision_reads(), 2);
    assert_eq!(alerts(&store), "");
    let good = read(&book);

    // The file vanishes while the node runs.
    std::fs::remove_file(&book).expect("remove");
    for _ in 0..3 {
        assert_eq!(
            status_of(&app, &a).await["status"],
            "dropped",
            "not pending again"
        );
        assert_eq!(status_of(&app, &b).await["status"], "on_hold");
    }
    let said = alerts(&store);
    assert_eq!(
        said.matches(DECISIONS_ALERT_TAG).count(),
        1,
        "one line: {said}"
    );
    assert!(said.contains("is gone"), "{said}");
    assert_eq!(store.decision_reads(), 2, "nothing to read");
    std::fs::write(&book, &good).expect("back");
    assert_eq!(status_of(&app, &a).await["status"], "dropped");
    assert_eq!(store.decision_reads(), 3);

    // The file cannot be read (where this user's own mode can stop it: not as root).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let set = |mode| {
            std::fs::set_permissions(&book, std::fs::Permissions::from_mode(mode)).expect("mode");
        };
        set(0o000);
        if std::fs::File::open(&book).is_err() {
            for _ in 0..3 {
                assert_eq!(status_of(&app, &a).await["status"], "dropped");
            }
            assert_eq!(
                store.decision_reads(),
                4,
                "tried once, not again after it failed"
            );
            assert!(
                alerts(&store).contains("cannot be read"),
                "{}",
                alerts(&store)
            );
            set(0o600);
            assert_eq!(status_of(&app, &b).await["status"], "on_hold");
            assert_eq!(store.decision_reads(), 5, "readable again: read again");
        }
        set(0o600);
    }

    // Over its size: nothing is read (the file is sparse here), the last known states stay.
    let reads = store.decision_reads();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&book)
        .expect("open");
    file.set_len(MAX_DECISIONS_BYTES + 1).expect("grow");
    for _ in 0..3 {
        assert_eq!(status_of(&app, &a).await["status"], "dropped");
    }
    assert_eq!(store.decision_reads(), reads + 1);
    assert!(alerts(&store).contains("over the"), "{}", alerts(&store));
    assert!(read_queue(&journal, CAP).is_err(), "the tool says so too");
    assert!(tool(&dir, &b, Action::Drop("spam".into()), 1).is_err());
    assert_eq!(
        std::fs::metadata(&book).expect("file").len(),
        MAX_DECISIONS_BYTES + 1
    );
    file.set_len(good.len() as u64).expect("shrink");
    assert_eq!(read(&book), good);
    assert_eq!(status_of(&app, &b).await["status"], "on_hold");

    // A node started with no readable decisions still starts.
    drop(app);
    drop(store);
    file.set_len(MAX_DECISIONS_BYTES + 1).expect("grow");
    let store = node(&dir, open_limits());
    assert!(alerts(&store).contains("over the"));
    assert_eq!(store.recorded(), 2);
}

/// Review B2-R2 L6: the tool reads an intake journal of at most [`MAX_INTAKE_BYTES`]; a larger
/// file is refused before it is read (sparse here), for the list and for a decision.
#[tokio::test]
async fn an_intake_journal_over_its_size_is_not_read_by_the_tool() {
    use crate::submit::review::MAX_INTAKE_BYTES;
    let dir = test_dir("decisions-intake-size");
    let journal = journal_in(&dir);
    let id = {
        let store = node(&dir, open_limits());
        propose(&routes(store.clone()), &example()).await
    };
    assert_eq!(read_queue(&journal, CAP).expect("queue").proposals.len(), 1);
    let len = std::fs::metadata(&journal).expect("file").len();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&journal)
        .expect("open");
    file.set_len(MAX_INTAKE_BYTES + 1).expect("grow");
    let e = read_queue(&journal, CAP).expect_err("over its size");
    assert!(e.to_string().contains("over the"), "{e}");
    assert!(matches!(
        tool(&dir, &id, Action::Drop("spam".into()), 0),
        Err(DecideError::Storage(_))
    ));
    assert!(!decisions_path(&journal).exists(), "nothing was written");
    file.set_len(len).expect("shrink");
    tool(&dir, &id, Action::Drop("spam".into()), 0).expect("readable again");
}
