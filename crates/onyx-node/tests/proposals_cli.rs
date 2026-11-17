//! `onyx-node proposals` as the reviewer runs it (ROADMAP S5, READINESS B2; THREATS
//! REV-Q-1, REV-Q-2): against the journal files alone, while a node holds the intake journal
//! open, with the exit codes the operator script relies on, and without writing the intake
//! journal. The change of user when it is started as root is checked as a rule in
//! `proposals::tests::root_drops_to_the_journals_owner_or_refuses`; the drop itself needs root
//! and is rehearsed on the server (THREATS REV-Q-9).

use onyx_api::journal::Journal;
use onyx_api::submit::decisions::decisions_path;
use onyx_api::submit::{SubmitLimits, SubmitStore, BINDING, JOURNAL_KIND};
use onyx_intake::Submission;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn proposals(journal: Option<&Path>, env: Option<&Path>, args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_onyx-node"));
    cmd.arg("proposals").args(args).env_clear();
    if let Some(j) = journal {
        cmd.arg("--journal").arg(j);
    }
    if let Some(j) = env {
        cmd.env("ONYX_SUBMISSIONS", j);
    }
    let out = cmd.output().expect("run onyx-node proposals");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("onyx-proposals-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

/// Write `n` proposals as the node records them (the same line the node's `POST /v1/submit`
/// appends) and return their receipt ids.
fn record(journal: &Path, n: u64) -> Vec<String> {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../fixtures/intake/vectors.json")).expect("json");
    let s = Submission::parse(&serde_json::to_vec(&vectors["example"]).expect("bytes"))
        .expect("the Treasury example");
    let route = s.route();
    let hash = format!("0x{}", hex::encode(s.hash().0));
    let (mut j, _) = Journal::open(journal, JOURNAL_KIND, &BINDING).expect("journal");
    (0..n)
        .map(|i| {
            let entry = json!({"kind":"submission","version":1,"hash":hash,
                "status":route.status(),"receivedUnix":1_700_000_000 + i,
                "route":route.to_json(),"submission":s});
            let seq = j.apply(&[], &[entry]).expect("append")[0];
            onyx_api::submit::submission_id(seq, &hash)
        })
        .collect()
}

const CLEAR: [&str; 10] = [
    "--terms-reread",
    "yes",
    "--no-personal-data",
    "yes",
    "--entities-only",
    "yes",
    "--source-public",
    "yes",
    "--rights-level",
    "allowed",
];

#[test]
fn the_reviewer_lists_shows_and_decides_beside_a_running_node() {
    let dir = tmp("review");
    let journal = dir.join("submissions.journal");
    let ids = record(&journal, 3);
    // A node holds the intake journal (and its lock) for the whole test.
    let node = SubmitStore::open(&journal, None, SubmitLimits::default()).expect("node");
    let intake = std::fs::read(&journal).expect("journal");
    let j = Some(journal.as_path());

    let (code, out, err) = proposals(j, None, &["list"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.starts_with("Journal: 3 of 10000 proposals recorded (0% of the cap).\n3 waiting for a decision, 0 decided."),
        "{out}"
    );
    for id in &ids {
        assert!(out.contains(id), "{id}: {out}");
    }
    assert!(out.contains("host fiscaldata.treasury.gov"), "{out}");
    assert!(
        !out.contains("public debt"),
        "the list shows no proposal text"
    );
    // ONYX_SUBMISSIONS names the journal when --journal does not.
    let (code, by_env, _) = proposals(None, Some(&journal), &["list"]);
    assert_eq!((code, by_env.lines().count()), (0, out.lines().count()));

    let (code, out, err) = proposals(j, None, &["show", &ids[0]]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("U.S. total public debt outstanding"), "{out}");
    assert!(out.contains("rights=open_license"), "{out}");

    // A clear without the whole checklist is refused, by the line's name; nothing is written.
    let (code, out, err) = proposals(j, None, &["clear", &ids[0], "--terms-reread", "yes"]);
    assert_eq!(code, 3, "{out}{err}");
    assert!(err.contains("refused, nothing written") && err.contains("--no-personal-data"));
    let mut no = CLEAR.to_vec();
    no[5] = "no";
    let (code, _, err) = proposals(j, None, &[&["clear", &ids[0]][..], &no].concat());
    assert_eq!(code, 3);
    assert!(err.contains("--entities-only"), "{err}");
    assert!(!decisions_path(&journal).exists());

    // Review B2-R1 H1: a symbolic link under the decisions journal's name is refused (exit 1)
    // and nothing is created where it points.
    #[cfg(unix)]
    {
        let outside = tmp("outside");
        let target = outside.join("planted");
        std::os::unix::fs::symlink(&target, decisions_path(&journal)).expect("link");
        let (code, out, err) = proposals(j, None, &[&["clear", &ids[0]][..], &CLEAR].concat());
        assert_eq!(code, 1, "{out}{err}");
        assert!(err.contains("symbolic link"), "{err}");
        let (code, _, err) = proposals(j, None, &["list"]);
        assert_eq!(code, 1, "{err}");
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(&outside).expect("dir").count(), 0);
        std::fs::remove_file(decisions_path(&journal)).expect("unlink");
    }

    let (code, out, err) = proposals(j, None, &[&["clear", &ids[0]][..], &CLEAR].concat());
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with(&format!("{}  cleared\n", ids[0])), "{out}");
    assert!(out.contains("Nothing is verified or published"), "{out}");
    let (code, _, err) = proposals(j, None, &[&["clear", &ids[0]][..], &CLEAR].concat());
    assert_eq!(code, 3);
    assert!(
        err.contains("already decided: the proposal is cleared"),
        "{err}"
    );

    let (code, out, _) = proposals(
        j,
        None,
        &["hold", &ids[1], "--reason", "unreachable_source"],
    );
    assert_eq!(code, 0);
    assert!(out.starts_with(&format!("{}  on_hold\n", ids[1])), "{out}");
    let (code, out, _) = proposals(j, None, &["drop", &ids[1], "--reason", "other:no_answer"]);
    assert_eq!(code, 0);
    assert!(out.starts_with(&format!("{}  dropped\n", ids[1])), "{out}");

    // Refusals: an unknown id, a reason outside the list, a decision after a drop.
    let unknown = format!("sub_{}", "0".repeat(32));
    for args in [
        &["show", unknown.as_str()][..],
        &["drop", unknown.as_str(), "--reason", "spam"],
        &["drop", ids[2].as_str(), "--reason", "my own words"],
        &["drop", ids[2].as_str(), "--reason", "other"],
        &["hold", ids[1].as_str(), "--reason", "spam"],
    ] {
        let (code, out, err) = proposals(j, None, args);
        assert_eq!(code, 3, "{args:?}: {out}{err}");
        assert!(out.is_empty(), "{args:?}: {out}");
    }
    // Usage errors.
    for args in [
        &["hold", ids[2].as_str()][..],
        &["decide", ids[2].as_str()],
        &["list", "--reason", "spam"],
        &["show"],
    ] {
        let (code, _, err) = proposals(j, None, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains("usage: onyx-node proposals"), "{err}");
    }
    let (code, _, err) = proposals(None, None, &["list"]);
    assert_eq!(code, 2, "{err}");
    // No journal there: exit 1 with the reason, and nothing created.
    let empty = tmp("none");
    let (code, _, err) = proposals(Some(&empty.join("submissions.journal")), None, &["list"]);
    assert_eq!(code, 1);
    assert!(err.contains("no intake journal here"), "{err}");
    assert_eq!(std::fs::read_dir(&empty).expect("dir").count(), 0);

    let (code, out, _) = proposals(j, None, &["list", "--all"]);
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[1], "1 waiting for a decision, 2 decided.");
    assert!(lines[2].starts_with(&ids[2]) && lines[2].contains("pending_review"));
    assert!(lines[3].starts_with(&ids[0]) && lines[3].contains("cleared"));
    assert!(lines[4].starts_with(&ids[1]) && lines[4].contains("dropped (other:no_answer)"));
    assert!(!out.contains('@'));

    assert_eq!(
        std::fs::read(&journal).expect("journal"),
        intake,
        "the reviewer never writes the intake journal"
    );
    assert_eq!(node.recorded(), 3);
    // The node the reviewer worked beside shows the decisions.
    let (_, state, reason) = node.public_status(&ids[1]).expect("status");
    assert_eq!(
        (state.as_str(), reason.as_deref()),
        ("dropped", Some("other"))
    );
    let (code, out, _) = proposals(None, None, &["--help"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("usage: onyx-node proposals list"));
    // Review B2-R2 L5: the help says what is and is not rewritten.
    assert!(!out.contains("nothing is ever rewritten"), "{out}");
    assert!(out.contains("The intake journal is never written"), "{out}");
    assert!(out.contains("copied aside"), "{out}");
}
