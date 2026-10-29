//! The observers' reveal transport (ROADMAP B2, ADR-0027 amendment 9): a
//! spool folder the coordinator reads ([`DirInbox`], a
//! [`RevealInbox`]) and each independent observer writes its signed reveal
//! into ([`send_reveal`]).
//!
//! Layout: `<spool>/0x<observer>/<round id>.json`, one file per observer and
//! round, written atomically (temporary file, sync, rename; mode `0600`, the
//! observer's folder `0700`). Each observer writes only its own folder: on
//! one host the folder's owner, across hosts an SSH/SFTP account limited to
//! that folder, so the transport is authenticated and encrypted by the OS or
//! SSH, and every message is authenticated again by the observer's own
//! signature ([`crate::observe::reveal_signed`], checked by
//! [`crate::observe::UnfinishedRound::receive_reveal`]). Commits never go
//! through the spool: each observer posts its own on chain
//! (`OnyxRegistry.postCommit`, amendment 8), and the coordinator absorbs them
//! from the transcript log.
//!
//! Why a spool (amendment 9): the reveal intake is a poll
//! ([`crate::observe::keep_taking_reveals`]); files survive a coordinator
//! restart, so the reveals an observer sent are read again by the restarted
//! intake without the observer resending (REVLOG-R5-1d); nothing listens on
//! a port, and nothing here can be made to block the round: a file that is
//! missing, too large, not a regular file, malformed, for another round or
//! observer, or badly signed is simply not a reveal.

use crate::observe::{RevealInbox, SignedReveal};
use borsh::BorshDeserialize;
use onyx_core::{Hash32, MinerAddress, Observation};
use std::path::{Path, PathBuf};

/// The largest reveal file the coordinator reads (bytes). A reveal is a few
/// hundred bytes; anything larger is not read.
pub const MAX_REVEAL_BYTES: u64 = 16 * 1024;

/// Version of the reveal file format.
pub const REVEAL_FILE_VERSION: u64 = 1;

/// `<spool>/0x<observer>/<round id>.json`.
pub fn reveal_path(spool: &Path, observer: &MinerAddress, round: &Hash32) -> PathBuf {
    spool
        .join(format!("0x{}", hex::encode(observer)))
        .join(format!("{round}.json"))
}

/// The reveal file's text: `round`, `attempt`, `observer`, `observation`
/// (hex Borsh), `salt`, `sig` (65 bytes hex). Nothing in it is trusted: the
/// coordinator checks the observer's signature over all of it.
pub fn reveal_json(round: &Hash32, attempt: u32, reveal: &SignedReveal) -> Result<String, String> {
    let (a, o, salt, sig) = reveal;
    let v = serde_json::json!({
        "v": REVEAL_FILE_VERSION,
        "round": round.to_string(),
        "attempt": attempt,
        "observer": format!("0x{}", hex::encode(a)),
        "observation": format!("0x{}", hex::encode(borsh::to_vec(o).map_err(|e| e.to_string())?)),
        "salt": salt.to_string(),
        "sig": format!("0x{}", hex::encode(sig)),
    });
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

fn hex_bytes(v: &serde_json::Value, key: &str) -> Option<Vec<u8>> {
    hex::decode(v.get(key)?.as_str()?.strip_prefix("0x")?).ok()
}

/// Parse a reveal file for `round` and `observer`; `None` for anything else
/// (another round or observer, an unknown version, malformed fields). The
/// signature is NOT checked here (the intake checks it).
pub fn parse_reveal(text: &str, round: &Hash32, observer: &MinerAddress) -> Option<SignedReveal> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("v")?.as_u64()? != REVEAL_FILE_VERSION {
        return None;
    }
    let r: [u8; 32] = hex_bytes(&v, "round")?.try_into().ok()?;
    let a: [u8; 20] = hex_bytes(&v, "observer")?.try_into().ok()?;
    if Hash32(r) != *round || a != *observer {
        return None;
    }
    let o = Observation::try_from_slice(&hex_bytes(&v, "observation")?).ok()?;
    let salt: [u8; 32] = hex_bytes(&v, "salt")?.try_into().ok()?;
    let sig: [u8; 65] = hex_bytes(&v, "sig")?.try_into().ok()?;
    Some((a, o, Hash32(salt), sig))
}

/// An observer sends its signed reveal: written atomically to its own file
/// in `spool` ([`reveal_path`]; folder `0700`, file `0600`). Sending again
/// replaces the file; the coordinator keeps the first reveal it took.
pub fn send_reveal(
    spool: &Path,
    round: &Hash32,
    attempt: u32,
    reveal: &SignedReveal,
) -> Result<PathBuf, String> {
    let path = reveal_path(spool, &reveal.0, round);
    let dir = path.parent().ok_or("reveal path has no folder")?;
    create_private_dir(dir)?;
    write_private_atomic(&path, &reveal_json(round, attempt, reveal)?)?;
    Ok(path)
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    // Review B2-R1-M2: an existing folder must be a real folder (not a
    // symlink to elsewhere) that group and others cannot open.
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!(
            "{}: not a folder (a symlink?): refused",
            dir.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "{}: a spool folder open to group or others (mode {mode:o}): refused; chmod 700",
                dir.display()
            ));
        }
    }
    Ok(())
}

/// [`crate::observe::write_atomic`] with a `0600` file from the start: a
/// reveal holds the observer's answer, which the paid API sells.
pub fn write_private_atomic(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().ok_or("no folder")?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "reveal".into())
    ));
    // Review B2-R1-M2: a temporary file left over (a crash) or planted (a
    // symlink to the key file, a 0666 file someone can read) is removed,
    // never written through; the new one is created exclusively, 0600, and
    // never through a symlink.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", tmp.display())),
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = o
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("{}: syncing the folder: {e}", dir.display()))
}

/// Read one reveal file: a regular file (never followed through a symlink)
/// of at most [`MAX_REVEAL_BYTES`]; `None` otherwise. A missing file is
/// simply no reveal yet; any other error to open it (review B2-R1-M2: a
/// coordinator that cannot read an observer's folder) is reported on
/// stderr, never silently read as "no reveal" (it names the path only).
fn read_reveal_file(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = match o.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("reveal spool: cannot read {}: {e}", path.display());
            return None;
        }
    };
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_REVEAL_BYTES {
        return None;
    }
    let mut s = String::new();
    f.take(MAX_REVEAL_BYTES + 1).read_to_string(&mut s).ok()?;
    (s.len() as u64 <= MAX_REVEAL_BYTES).then_some(s)
}

/// The coordinator's end of the spool: a [`RevealInbox`] that reads, on
/// each poll, the reveal file of every drawn observer of the round (and
/// nothing else, so the work per poll is bounded by the committee size).
/// A file read again returns its reveal again; the intake keeps each
/// observer's first.
#[derive(Clone, Debug)]
pub struct DirInbox {
    /// The spool folder.
    pub spool: PathBuf,
}

impl DirInbox {
    /// The inbox over `spool` (created `0700` if missing; an existing spool
    /// must be a real folder closed to group and others, and readable).
    pub fn open(spool: &Path) -> Result<Self, String> {
        create_private_dir(spool)?;
        std::fs::read_dir(spool).map_err(|e| format!("{}: {e}", spool.display()))?;
        Ok(DirInbox {
            spool: spool.to_path_buf(),
        })
    }
}

impl RevealInbox for DirInbox {
    fn take(&mut self, round: &Hash32, observers: &[MinerAddress], _now: u64) -> Vec<SignedReveal> {
        observers
            .iter()
            .filter_map(|a| {
                let text = read_reveal_file(&reveal_path(&self.spool, a, round))?;
                parse_reveal(&text, round, a)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::{sign_reveal, test_keys};
    use onyx_core::Answer;

    fn spool(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("onyx-spool-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn reveal(i: u32, round: &Hash32, cents: i128) -> SignedReveal {
        let sk = test_keys::secret("b2 link", i);
        let o = Observation {
            answer: Answer::Number(cents),
            observed_at: 1_000,
            page_hash: Hash32([7; 32]),
        };
        let salt = Hash32([i as u8; 32]);
        let sig = sign_reveal(&sk, round, 0, &o, &salt).expect("sign");
        (test_keys::address("b2 link", i), o, salt, sig)
    }

    /// ROADMAP B2: a reveal sent through the spool reaches the inbox as the
    /// observer signed it; only the drawn observers' files of this round
    /// are read.
    #[test]
    fn b2_a_reveal_round_trips_through_the_spool() {
        let d = spool("trip");
        let round = Hash32([1; 32]);
        let r = reveal(1, &round, 42);
        let p = send_reveal(&d, &round, 0, &r).expect("send");
        assert_eq!(p, reveal_path(&d, &r.0, &round));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let mut inbox = DirInbox::open(&d).expect("inbox");
        assert_eq!(inbox.take(&round, &[r.0], 0), vec![r.clone()]);
        // Not drawn in this round: not read. Another round: nothing.
        assert!(inbox.take(&round, &[[9; 20]], 0).is_empty());
        assert!(inbox.take(&Hash32([2; 32]), &[r.0], 0).is_empty());
    }

    /// Review B2-R1-M2: a temporary file planted as a symlink (to a key file,
    /// say) is removed, never written through; a spool folder open to
    /// others or a symlinked observer folder is refused.
    #[cfg(unix)]
    #[test]
    fn b2_r1_the_write_path_never_follows_a_planted_link_or_opens_a_loose_folder() {
        use std::os::unix::fs::PermissionsExt;
        let d = spool("plant");
        let round = Hash32([5; 32]);
        let r = reveal(1, &round, 42);
        let path = reveal_path(&d, &r.0, &round);
        let dir = path.parent().expect("dir").to_path_buf();
        create_private_dir(&dir).expect("dir");
        let victim = d.join("victim.key");
        std::fs::write(&victim, "secret").expect("w");
        let tmp = dir.join(format!(
            ".{}.tmp",
            path.file_name().expect("name").to_string_lossy()
        ));
        std::os::unix::fs::symlink(&victim, &tmp).expect("link");
        send_reveal(&d, &round, 0, &r).expect("send");
        assert_eq!(std::fs::read_to_string(&victim).expect("r"), "secret");
        assert_eq!(
            DirInbox::open(&d).expect("inbox").take(&round, &[r.0], 0),
            vec![r.clone()]
        );
        // A folder open to others, and an observer folder that is a symlink.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(send_reveal(&d, &round, 0, &r).is_err());
        let r2 = reveal(2, &round, 42);
        let elsewhere = d.join("elsewhere");
        create_private_dir(&elsewhere).expect("dir");
        std::os::unix::fs::symlink(&elsewhere, d.join(format!("0x{}", hex::encode(r2.0))))
            .expect("link");
        assert!(send_reveal(&d, &round, 0, &r2).is_err());
    }

    /// ROADMAP B2 adversarial: a file in an observer's folder that names
    /// another observer or round, is malformed, too large, or a symlink is
    /// not a reveal.
    #[test]
    fn b2_a_misnamed_malformed_large_or_linked_file_is_no_reveal() {
        let d = spool("bad");
        let round = Hash32([3; 32]);
        let honest = reveal(1, &round, 42);
        let other = reveal(2, &round, 42);
        let path = reveal_path(&d, &honest.0, &round);
        create_private_dir(path.parent().expect("dir")).expect("mkdir");
        let mut inbox = DirInbox::open(&d).expect("inbox");
        // Observer 2's reveal dropped into observer 1's file.
        std::fs::write(&path, reveal_json(&round, 0, &other).expect("json")).expect("w");
        assert!(inbox.take(&round, &[honest.0], 0).is_empty());
        // Another round's reveal under this round's name.
        std::fs::write(
            &path,
            reveal_json(&Hash32([4; 32]), 0, &honest).expect("json"),
        )
        .expect("w");
        assert!(inbox.take(&round, &[honest.0], 0).is_empty());
        for junk in [
            "not json".to_string(),
            "{}".into(),
            reveal_json(&round, 0, &honest)
                .expect("json")
                .replace("\"v\":1", "\"v\":2"),
            format!(
                "{} {}",
                reveal_json(&round, 0, &honest).expect("json"),
                " ".repeat(20_000)
            ),
        ] {
            std::fs::write(&path, junk).expect("w");
            assert!(inbox.take(&round, &[honest.0], 0).is_empty());
        }
        #[cfg(unix)]
        {
            let target = d.join("elsewhere.json");
            std::fs::write(&target, reveal_json(&round, 0, &honest).expect("json")).expect("w");
            std::fs::remove_file(&path).expect("rm");
            std::os::unix::fs::symlink(&target, &path).expect("link");
            assert!(inbox.take(&round, &[honest.0], 0).is_empty());
        }
    }
}
