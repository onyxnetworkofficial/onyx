//! `onyx-node pointers` as the reviewer runs it (ADR-0066 item 7, READINESS B5; THREATS
//! PUBON-PTR-7): against the journal file alone, while a node holds it open, with the exit
//! codes the operator script relies on, and without writing anything. The change of user when
//! it is started as root is the proposal tool's rule
//! (`pointers::tests::root_drops_to_the_pointer_journals_owner_or_refuses`).

use onyx_api::journal::Journal;
use onyx_api::publishers::{drops_path, PointerLimits, PointerStore, BINDING, JOURNAL_KIND};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

fn pointers(journal: Option<&Path>, env: Option<&Path>, args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_onyx-node"));
    cmd.arg("pointers").args(args).env_clear();
    if let Some(j) = journal {
        cmd.arg("--journal").arg(j);
    }
    if let Some(j) = env {
        cmd.env("ONYX_PUBLISHER_POINTERS", j);
    }
    let out = cmd.output().expect("run onyx-node pointers");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("onyx-pointers-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

fn id(n: u8) -> String {
    format!("ptr_{}", hex::encode([n; 16]))
}

fn hash(n: u8) -> String {
    format!("0x{}", hex::encode([n; 32]))
}

#[test]
fn the_reviewer_lists_and_shows_beside_a_running_node() {
    let dir = tmp("list");
    let journal = dir.join("publisher-pointers.journal");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    // Three pointers as the node records them: one a week old (expired, still kept), two new
    // (half an hour inside their hour, so the printed time left does not depend on this run).
    {
        let (mut j, _) = Journal::open(&journal, JOURNAL_KIND, &BINDING).expect("journal");
        for (n, age) in [(1u8, 7 * 86_400), (2, 86_400 + 1_800), (3, 1_800)] {
            let entry = json!({"kind": "pointer", "version": 1, "id": id(n),
                "origin": format!("d{n}.example.org/river-levels"), "application": hash(n),
                "receivedUnix": now - age});
            j.apply(&[], &[entry]).expect("append");
        }
    }
    // The node holds the journal (its lock) while the reviewer reads.
    let node = PointerStore::open(&journal, PointerLimits::default()).expect("node");
    let before = std::fs::read(&journal).expect("bytes");
    let j = Some(journal.as_path());

    let (code, out, err) = pointers(j, None, &["list"]);
    assert_eq!(code, 0, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "2 pointer(s) waiting, 1 expired and kept, 0 dropped; the journal holds 3 of 1000 entries"
    );
    assert_eq!(lines.len(), 3, "{out}");
    assert!(
        lines[1].starts_with(&format!(
            "{}  4 d 23 h left  d2.example.org/river-levels  {}",
            id(2),
            hash(2)
        )),
        "{out}"
    );
    assert!(
        lines[2].starts_with(&id(3)) && lines[2].ends_with(&hash(3)),
        "{out}"
    );
    // The journal named by the node's own setting, and the expired one with --all.
    let (code, out, _) = pointers(None, Some(&journal), &["list", "--all"]);
    assert_eq!(code, 0);
    assert!(
        out.contains(&format!(
            "{}  expired  d1.example.org/river-levels  {}",
            id(1),
            hash(1)
        )),
        "{out}"
    );

    // show: exactly the two lines of a pointer file.
    let (code, out, _) = pointers(j, None, &["show", &id(2)]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "origin: d2.example.org/river-levels\napplication keccak: {}\n",
            hash(2)
        )
    );
    // An expired or unknown receipt: exit 3, nothing printed on stdout.
    for unknown in [id(1), id(9)] {
        let (code, out, err) = pointers(j, None, &["show", &unknown]);
        assert_eq!((code, out.as_str()), (3, ""), "{err}");
        assert!(err.contains("no waiting pointer"), "{err}");
    }
    // Usage errors: exit 2 with the usage.
    for args in [
        &["decide"][..],
        &["show"],
        &["show", "ptr_1"],
        &["list", "--everything"],
    ] {
        let (code, _, err) = pointers(j, None, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains("usage: onyx-node pointers"), "{err}");
    }
    let (code, _, err) = pointers(None, None, &["list"]);
    assert_eq!(code, 2, "{err}");
    let (code, out, _) = pointers(None, None, &["--help"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("usage: onyx-node pointers list"));
    assert!(out.contains("contact no origin"), "{out}");

    // Nothing was written so far, no file was added.
    assert_eq!(std::fs::read(&journal).expect("bytes"), before);
    assert!(!drops_path(&journal).exists());

    // Review PTR-R1 M2: drop marks a waiting pointer whose hash is not the file's. One line in
    // the drops journal beside the pointer journal, which itself is never written; the pointer
    // then leaves the list and cannot be shown; a second drop writes nothing.
    let (code, out, err) = pointers(j, None, &["drop", &id(3)]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with(&format!(
            "{}  dropped\nIts receipt now says: dropped",
            id(3)
        )),
        "{out}"
    );
    let drops = std::fs::read(drops_path(&journal)).expect("drops");
    let (code, out, _) = pointers(j, None, &["drop", &id(3)]);
    assert_eq!(code, 0);
    assert!(out.contains("dropped before; nothing was written"), "{out}");
    assert_eq!(std::fs::read(drops_path(&journal)).expect("drops"), drops);
    let (_, out, _) = pointers(j, None, &["list"]);
    assert!(
        out.starts_with("1 pointer(s) waiting, 1 expired and kept, 1 dropped;"),
        "{out}"
    );
    assert!(!out.contains(&id(3)), "{out}");
    let (_, out, _) = pointers(j, None, &["list", "--all"]);
    assert!(
        out.contains(&format!(
            "{}  dropped  d3.example.org/river-levels  {}",
            id(3),
            hash(3)
        )),
        "{out}"
    );
    let (code, _, _) = pointers(j, None, &["show", &id(3)]);
    assert_eq!(code, 3, "a dropped pointer is not a pointer file any more");
    // Review PTR-R2 L3: undrop takes the drop back (the list shows it waiting again), and a
    // second undrop, or one for a pointer that was never dropped, is exit 3 and writes nothing.
    let (code, out, err) = pointers(j, None, &["undrop", &id(3)]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with(&format!("{}  not dropped\nThe drop was taken back", id(3))),
        "{out}"
    );
    let (_, out, _) = pointers(j, None, &["list"]);
    assert!(
        out.starts_with("2 pointer(s) waiting, 1 expired and kept, 0 dropped;"),
        "{out}"
    );
    assert!(out.contains(&id(3)), "{out}");
    for other in [id(3), id(2)] {
        let (code, out, err) = pointers(j, None, &["undrop", &other]);
        assert_eq!((code, out.as_str()), (3, ""), "{err}");
        assert!(err.contains("no drop of the id"), "{err}");
    }
    let (code, _, _) = pointers(j, None, &["drop", &id(3)]);
    assert_eq!(code, 0);
    let drops = std::fs::read(drops_path(&journal)).expect("drops");
    // An expired or unknown one is not dropped: exit 3, nothing written.
    for other in [id(1), id(9)] {
        let (code, out, err) = pointers(j, None, &["drop", &other]);
        assert_eq!((code, out.as_str()), (3, ""), "{err}");
        assert!(err.contains("nothing written"), "{err}");
    }
    assert_eq!(std::fs::read(drops_path(&journal)).expect("drops"), drops);
    assert_eq!(std::fs::read(&journal).expect("bytes"), before);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "publisher-pointers.drops.journal",
            "publisher-pointers.drops.journal.lock",
            "publisher-pointers.journal",
            "publisher-pointers.journal.lock"
        ]
    );
    assert_eq!(node.recorded(), 3);

    // THREATS PUBON-PTR-5: the journal's lines, the list, a pointer file and the route's answers
    // hold no field a secret scanner reads as a credential (the public snapshot's scan would
    // otherwise stop on a sample of them). Run with the repository's rules when gitleaks is here.
    let (_, list, _) = pointers(j, None, &["list", "--all"]);
    let (_, shown, _) = pointers(j, None, &["show", &id(2)]);
    let samples = tmp("scan");
    std::fs::copy(&journal, samples.join("publisher-pointers.journal")).expect("copy");
    std::fs::copy(
        drops_path(&journal),
        samples.join("publisher-pointers.drops.journal"),
    )
    .expect("copy");
    std::fs::write(samples.join("list.txt"), list).expect("write");
    std::fs::write(samples.join("pointer.txt"), shown).expect("write");
    let request = json!({"origin": "d2.example.org/river-levels", "application": hash(2)});
    let receipt = json!({"id": id(2), "status": "expired", "reason": "pointer_lifetime_over",
        "expiresUnix": now, "decision": "see_status_list"});
    std::fs::write(samples.join("request.json"), request.to_string()).expect("write");
    std::fs::write(samples.join("receipt.json"), receipt.to_string()).expect("write");
    let rules = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.gitleaks.toml");
    match Command::new("gitleaks")
        .args([
            "dir",
            "--config",
            rules,
            "--redact",
            "--no-banner",
            "--exit-code",
            "1",
        ])
        .arg(&samples)
        .output()
    {
        Ok(out) => assert!(
            out.status.success(),
            "gitleaks found something in the pointer samples: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(_) => eprintln!("skipped: gitleaks is not installed"),
    }
}

/// Review PTR-R1 L2: an entry past its kept time that the node has not removed yet (the file
/// as a stopped or quiet node left it) is not shown by the tool, also not with --all.
#[test]
fn an_entry_past_its_kept_time_is_never_shown() {
    let dir = tmp("past");
    let journal = dir.join("publisher-pointers.journal");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    {
        let (mut j, _) = Journal::open(&journal, JOURNAL_KIND, &BINDING).expect("journal");
        for (n, age) in [(1u8, 40 * 86_400), (2, 86_400 + 1_800)] {
            let entry = json!({"kind": "pointer", "version": 1, "id": id(n),
                "origin": format!("d{n}.example.org"), "application": hash(n),
                "receivedUnix": now - age});
            j.apply(&[], &[entry]).expect("append");
        }
    }
    let (code, out, err) = pointers(Some(&journal), None, &["list", "--all"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("1 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 2 of 1000 entries"),
        "{out}"
    );
    assert!(
        !out.contains("d1.example.org") && !out.contains(&id(1)),
        "{out}"
    );
    assert!(out.contains("d2.example.org"), "{out}");
    for verb in ["show", "drop"] {
        let (code, out, _) = pointers(Some(&journal), None, &[verb, &id(1)]);
        assert_eq!((code, out.as_str()), (3, ""), "{verb}");
    }
}

#[test]
fn a_missing_damaged_or_linked_journal_is_exit_one() {
    // No journal there: a list of nothing (the route may never have been on), nothing created.
    let empty = tmp("none");
    let (code, out, err) = pointers(
        Some(&empty.join("publisher-pointers.journal")),
        None,
        &["list"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("0 pointer(s) waiting"), "{out}");
    assert_eq!(std::fs::read_dir(&empty).expect("dir").count(), 0);

    let dir = tmp("damaged");
    let journal = dir.join("publisher-pointers.journal");
    {
        let (mut j, _) = Journal::open(&journal, JOURNAL_KIND, &BINDING).expect("journal");
        // A line with a good checksum that is not a pointer as the node writes one.
        let entry = json!({"kind": "pointer", "version": 1, "id": id(1),
            "origin": "https://d1.example.org", "application": hash(1), "receivedUnix": 1});
        j.apply(&[], &[entry]).expect("append");
    }
    let (code, out, err) = pointers(Some(&journal), None, &["list"]);
    assert_eq!((code, out.as_str()), (1, ""), "{err}");
    assert!(err.contains("invalid pointer journal record"), "{err}");
    assert!(
        !err.contains("d1.example.org"),
        "a refused line is not printed: {err}"
    );

    #[cfg(unix)]
    {
        let link = dir.join("linked.journal");
        std::os::unix::fs::symlink(&journal, &link).expect("link");
        let (code, _, err) = pointers(Some(&link), None, &["list"]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("symbolic link"), "{err}");
    }
}
