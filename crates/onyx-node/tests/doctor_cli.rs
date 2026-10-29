//! `onyx-node doctor` as a user runs it (ROADMAP P5, docs/ops/hardware.md
//! section 5): no ONYX_* settings needed, exit 2 on a usage error, 0 or 1 by
//! the lines, and nothing written to the data directory.

use std::process::Command;

fn doctor(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_onyx-node"))
        .arg("doctor")
        .args(args)
        .env_clear()
        .output()
        .expect("run onyx-node doctor");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("onyx-doctor-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &["--role", "buyer"][..],
        &["--chain", "solana"],
        &["--role"],
        &["--fix"],
        &["--role", "contributor", "--chain", "ethereum"],
        &["--data-dir", "/no/such/dir/for/onyx/p5"],
    ] {
        let (code, out, err) = doctor(args);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert!(out.is_empty(), "{args:?} printed checks: {out}");
    }
    let (code, out, _) = doctor(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("docs/ops/hardware.md"), "{out}");
}

#[test]
fn every_role_prints_five_checks_and_exits_by_its_lines() {
    let d = tmp("roles");
    std::fs::write(d.join("keep"), "x").expect("write");
    for role in ["contributor", "validator", "indexer", "node", "coordinator"] {
        let (code, out, err) = doctor(&["--role", role, "--data-dir", d.to_str().expect("utf-8")]);
        let lines: Vec<&str> = out.lines().skip(1).collect();
        assert_eq!(lines.len(), 5, "{role}: {out}{err}");
        for (l, what) in
            lines
                .iter()
                .zip(["cpu:", "ram:", "disk size:", "free disk:", "disk type:"])
        {
            let word = l.split(' ').next().expect("word");
            assert!(
                ["PASS", "WARN", "FAIL", "SKIP"].contains(&word),
                "{role}: {l}"
            );
            assert!(l[word.len() + 1..].starts_with(what), "{role}: {l}");
        }
        let failed = lines.iter().any(|l| l.starts_with("FAIL"));
        assert_eq!(code, i32::from(failed), "{role}: {out}");
    }
    // The coordinator has nothing measured: it never fails.
    let (code, _, _) = doctor(&[
        "--role",
        "coordinator",
        "--data-dir",
        d.to_str().expect("utf-8"),
    ]);
    assert_eq!(code, 0);
    let names: Vec<_> = std::fs::read_dir(&d)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("keep")],
        "nothing written"
    );
    let _ = std::fs::remove_dir_all(&d);
}
