//! The public record of publisher decisions (ADR-0066 item 9):
//! `deploy/publishers/`.
//!
//! ```text
//! deploy/publishers/<publisher id>/decision.json    a registered publisher: a JSON list, oldest first
//! deploy/publishers/<publisher id>/application.json the approved application's bytes (application-2.json for a further key)
//! deploy/publishers/application-<16 hex>/decision.json
//!                                                   an application that is not approved (received, refused,
//!                                                   withdrawn, expired), named by its Keccak: never by its id
//! ```
//!
//! An application that is not approved leaves no id, origin, data text or
//! sample here: its folder is named by the first 16 hex digits of its
//! Keccak. `scripts/check-publishers.py` checks every folder and writes the
//! status list. The fields are the ones `onyx_bulk::application::Record`
//! reads (`state`, `signer`, `blocks_signer_until`, `sample_manifest_root`).

use super::Res;
use onyx_bulk::application::strict_json;
use onyx_core::Hash32;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The file of one folder.
pub(crate) const DECISION: &str = "decision.json";

/// The folder of an application that is not approved.
pub(crate) fn hash_folder(application: &Hash32) -> String {
    format!("application-{}", &application.to_string()[2..18])
}

/// A calendar date (UTC) of a unix time: `2026-11-17`.
pub(crate) fn date_of(unix: u64) -> String {
    // Days to a civil date (the proleptic Gregorian calendar).
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The decisions of one file: a list, or one object.
pub(crate) fn read(file: &Path) -> Res<Vec<Value>> {
    let b = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    match strict_json(&b).map_err(|e| format!("{}: not JSON: {e}", file.display()))? {
        Value::Array(a) => Ok(a),
        one @ Value::Object(_) => Ok(vec![one]),
        _ => Err(format!("{}: not a list of decisions", file.display())),
    }
}

/// Every decision file under the record folder `dir` (or `dir` itself when
/// it is one file), sorted.
pub(crate) fn files(dir: &Path) -> Res<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let f = e
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .path()
            .join(DECISION);
        if f.is_file() {
            out.push(f);
        }
    }
    out.sort();
    Ok(out)
}

/// Every decision of the record, with the folder it stands in.
pub(crate) fn all(dir: &Path) -> Res<Vec<(String, Value)>> {
    let mut out = Vec::new();
    for f in files(dir)? {
        let folder = f
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for d in read(&f)? {
            out.push((folder.clone(), d));
        }
    }
    Ok(out)
}

fn address(v: &Value) -> Option<[u8; 20]> {
    hex::decode(v.as_str()?.strip_prefix("0x")?)
        .ok()?
        .try_into()
        .ok()
}

/// The request sequences the record holds, by signer: a request is taken
/// once (review APPLY-R1), so every taken one is written down.
pub(crate) fn sequences(dir: &Path) -> Res<Vec<([u8; 20], u64)>> {
    Ok(all(dir)?
        .iter()
        .filter_map(|(_, d)| Some((address(&d["signer"])?, d["sequence"].as_u64()?)))
        .collect())
}

/// Add `entry` as the newest decision of `<dir>/<folder>/decision.json`.
/// The same entry again (a run repeated after a failure) changes nothing.
pub(crate) fn append(dir: &Path, folder: &str, entry: &Value) -> Res<PathBuf> {
    let d = dir.join(folder);
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    let f = d.join(DECISION);
    let mut list = if f.exists() { read(&f)? } else { Vec::new() };
    if !list.contains(entry) {
        list.push(entry.clone());
    }
    write(&f, &Value::Array(list))?;
    Ok(f)
}

/// Write `v` as indented JSON with a final newline, through a temporary
/// file in the same folder: a reader never sees half a record.
pub(crate) fn write(file: &Path, v: &Value) -> Res<()> {
    write_bytes(file, format!("{v:#}\n").as_bytes())
}

/// Write `bytes` to `file` through a temporary file beside it.
pub(crate) fn write_bytes(file: &Path, bytes: &[u8]) -> Res<()> {
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(951_782_400), "2000-02-29");
        assert_eq!(date_of(1_794_873_600), "2026-11-17");
        assert_eq!(date_of(1_794_873_600 + 86_399), "2026-11-17");
    }

    #[test]
    fn the_record_keeps_every_taken_sequence_and_an_entry_once() {
        let dir = std::env::temp_dir().join(format!("onyx-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let signer = "0x00000000000000000000000000000000000000aa";
        let e1 = json!({"state": "approved", "request": "raise", "signer": signer, "sequence": 3});
        let e2 = json!({"state": "withdrawn", "signer": signer, "sequence": 5});
        append(&dir, "river-levels.open", &e1).expect("append");
        append(&dir, "river-levels.open", &e1).expect("again");
        append(&dir, &hash_folder(&Hash32([0xab; 32])), &e2).expect("append");
        let all = all(&dir).expect("all");
        assert_eq!(all.len(), 2, "an entry written twice stands once");
        assert_eq!(all[0].0, "application-abababababababab");
        let mut seq = sequences(&dir).expect("sequences");
        seq.sort();
        assert_eq!(
            seq,
            vec![
                (
                    [0u8; 19]
                        .iter()
                        .copied()
                        .chain([0xaa])
                        .collect::<Vec<_>>()
                        .try_into()
                        .expect("20"),
                    3
                ),
                (
                    [0u8; 19]
                        .iter()
                        .copied()
                        .chain([0xaa])
                        .collect::<Vec<_>>()
                        .try_into()
                        .expect("20"),
                    5
                )
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
