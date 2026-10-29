//! An append-only journal on disk (REV-11): the API's receipt log and its
//! pending self-settles survive a crash or a restart.
//!
//! The file is lines of `<sum> <json>`, where `<sum>` is the first 16 bytes
//! of keccak-256 of the JSON bytes, in hex (REV-L3-F6): a line damaged on
//! disk (a torn sector, a bit flip, a stray edit) is refused, never replayed
//! as a different value. The sum has no key: it detects damage, not a
//! deliberate edit by someone who can write the state directory and
//! recompute it (REV-L4-F4). That directory is the operator's alone; anyone
//! who can write it can also change the server's binary or key.
//! The first line is a header naming the journal's kind and what it is bound
//! to (chain id and contract, REV-L3-F4): a journal written for another
//! deployment refuses to open. Every later line is one batch of changes
//! (`{"ops": [{"del": id} | {"add": id, "v": value}, …]}`), written and
//! `fsync`ed as one line, so a batch is applied whole or not at all
//! (REV-L3-F6): a crash can leave only the last line half written, and that
//! line was never acknowledged; it is cut off. Any other bad line refuses
//! the open: a damaged journal is never read as a shorter one, which would
//! silently drop entries.
//!
//! The journal keeps its own copy of the live entries and rewrites the file
//! from it when removed entries outnumber the live ones (temp file, `fsync`,
//! rename, `fsync` of the directory). Compaction never depends on the
//! caller's view of what is live (REV-L3-F1). One process at a time: an
//! `flock` on `<file>.lock` (never renamed, so compaction keeps it) refuses a
//! second writer (REV-L3-F4). The lock dies with its process, so a lock file
//! left by a crash never blocks a restart. Opening retries the lock for up to
//! [`LOCK_WAIT`]: a child process forked at that moment holds a copy of the
//! descriptor until it execs (REV-L4-F7). Local disks only: `flock` on a
//! network file system (NFS with `nolock` or `local_lock`) locks within one
//! host, so two servers on two hosts sharing one directory are not refused.

use onyx_core::keccak256;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Rewrite the file once at least this many entries were removed and they
/// outnumber the live ones.
pub const COMPACT_AFTER: usize = 1_024;
/// Format version in the header.
const VERSION: u64 = 2;
/// Adds per line when compacting (each line is still checksummed).
const COMPACT_CHUNK: usize = 512;
/// How long [`Journal::open`] retries a held lock before refusing
/// (REV-L4-F7).
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
/// The largest entry id a journal holds (2^53, exact in every JSON reader).
/// A line naming a larger id is damage and refuses the open (FUZZ-1: an id
/// of `u64::MAX` overflowed the next-id count), and a journal never gives
/// out an id above it.
pub const MAX_ID: u64 = 1 << 53;

/// What a journal belongs to: one chain and one contract (the `OnyxRevenue`
/// whose payments it tracks). A journal opened with another binding refuses
/// to open, so receipts of an old deployment are never posted to a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// The contract the entries are for.
    pub contract: [u8; 20],
}

impl Binding {
    fn header(&self, kind: &str) -> Value {
        json!({
            "onyxJournal": VERSION,
            "kind": kind,
            "chainId": self.chain_id,
            "contract": format!("0x{}", hex::encode(self.contract)),
        })
    }
}

/// One open journal file.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: File,
    /// Held for the journal's life: the exclusive lock on `<path>.lock`.
    _lock: File,
    header: Value,
    live: BTreeMap<u64, Value>,
    next: u64,
    dead: usize,
}

fn bad(path: &Path, line: usize, why: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("journal {}: line {line}: {why}", path.display()),
    )
}

fn sum(json: &[u8]) -> String {
    hex::encode(&keccak256(json).0[..16])
}

pub(crate) fn owner_only(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
}

pub(crate) fn set_owner_only(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// `<sum> <json>\n` for `v`.
fn framed(v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let mut out = sum(body.as_bytes()).into_bytes();
    out.push(b' ');
    out.extend_from_slice(body.as_bytes());
    out.push(b'\n');
    out
}

/// The JSON of a framed line (without its newline), if its sum matches.
fn unframe(line: &[u8]) -> Option<Value> {
    let space = line.iter().position(|b| *b == b' ')?;
    let (s, body) = (&line[..space], &line[space + 1..]);
    if s != sum(body).as_bytes() {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// `fsync` the directory holding `path`, so a create or a rename in it is
/// durable.
fn sync_dir(path: &Path) -> io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    File::open(dir)?.sync_all()
}

fn lock_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".lock");
    PathBuf::from(p)
}

impl Journal {
    /// Open (or create) the journal at `path` for `kind` bound to `bind`,
    /// and replay it: the live entries, oldest first, each with its id.
    /// Refuses a journal another process holds, one bound elsewhere, and a
    /// damaged one.
    pub fn open(
        path: impl AsRef<Path>,
        kind: &str,
        bind: &Binding,
    ) -> io::Result<(Journal, Vec<(u64, Value)>)> {
        let path = path.as_ref().to_path_buf();
        let lock_path = lock_path(&path);
        let mut lock_options = OpenOptions::new();
        lock_options.write(true).create(true).truncate(false);
        owner_only(&mut lock_options);
        let lock = lock_options.open(&lock_path)?;
        set_owner_only(&lock_path)?;
        let start = std::time::Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                // A forked child may hold a copy of the descriptor for a
                // moment before it execs (REV-L4-F7): wait a little.
                Err(TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!(
                        "journal {} is in use by another process (one server per state directory)",
                        path.display()
                    ),
                    ))
                }
                Err(TryLockError::Error(e)) => return Err(e),
            }
        }
        let existed = path.exists();
        let mut file_options = OpenOptions::new();
        file_options.read(true).append(true).create(true);
        owner_only(&mut file_options);
        let mut file = file_options.open(&path)?;
        set_owner_only(&path)?;
        if !existed {
            sync_dir(&path)?;
        }
        let header = bind.header(kind);
        let mut live: BTreeMap<u64, Value> = BTreeMap::new();
        let (mut next, mut dead) = (0u64, 0usize);
        let mut good_len = 0u64;
        let mut has_header = false;
        {
            let mut r = BufReader::new(&mut file);
            r.seek(SeekFrom::Start(0))?;
            let mut buf = Vec::new();
            let mut n = 0usize;
            loop {
                buf.clear();
                let got = r.read_until(b'\n', &mut buf)?;
                if got == 0 {
                    break;
                }
                n += 1;
                if buf.last() != Some(&b'\n') {
                    // A torn last write: never acknowledged, cut off below.
                    break;
                }
                let line = unframe(&buf[..buf.len() - 1])
                    .ok_or_else(|| bad(&path, n, "checksum or JSON does not match"))?;
                if n == 1 {
                    if line != header {
                        return Err(bad(
                            &path,
                            n,
                            format!("journal is for {line}, this server is {header}"),
                        ));
                    }
                    has_header = true;
                    good_len += got as u64;
                    continue;
                }
                let ops = line["ops"]
                    .as_array()
                    .ok_or_else(|| bad(&path, n, "no ops"))?;
                // A line is one batch: check it whole before applying it.
                let mut dels = std::collections::BTreeSet::new();
                let mut adds = Vec::new();
                let mut staged_next = next;
                for op in ops {
                    if let Some(id) = op.get("add").and_then(Value::as_u64) {
                        let v = op
                            .get("v")
                            .cloned()
                            .ok_or_else(|| bad(&path, n, "no value"))?;
                        if id < staged_next || live.contains_key(&id) {
                            return Err(bad(&path, n, format!("id {id} reused")));
                        }
                        if id >= MAX_ID {
                            return Err(bad(&path, n, format!("id {id} out of range")));
                        }
                        staged_next = id + 1;
                        adds.push((id, v));
                    } else if let Some(id) = op.get("del").and_then(Value::as_u64) {
                        if !live.contains_key(&id) || !dels.insert(id) {
                            return Err(bad(&path, n, format!("removes unknown id {id}")));
                        }
                    } else {
                        return Err(bad(&path, n, "neither add nor del"));
                    }
                }
                for id in &dels {
                    live.remove(id);
                }
                live.extend(adds);
                next = staged_next;
                dead += dels.len();
                good_len += got as u64;
            }
        }
        if file.metadata()?.len() != good_len {
            file.set_len(good_len)?;
            file.sync_all()?;
        }
        if !has_header {
            // A new file (or one whose header write was torn).
            file.write_all(&framed(&header))?;
            file.sync_all()?;
        }
        let entries = live.iter().map(|(k, v)| (*k, v.clone())).collect();
        Ok((
            Journal {
                path,
                file,
                _lock: lock,
                header,
                live,
                next,
                dead,
            },
            entries,
        ))
    }

    /// The file this journal writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the entries `del` and add `add`, as one durable line. Returns
    /// the ids given to `add`, in order. Compacts the file when due. An
    /// error means the change may or may not be on disk: the caller stops
    /// using this journal.
    pub fn apply(&mut self, del: &[u64], add: &[Value]) -> io::Result<Vec<u64>> {
        if del.is_empty() && add.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen = std::collections::BTreeSet::new();
        if let Some(id) = del
            .iter()
            .find(|id| !self.live.contains_key(id) || !seen.insert(**id))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("journal {}: removes unknown id {id}", self.path.display()),
            ));
        }
        let count = u64::try_from(add.len()).unwrap_or(u64::MAX);
        if self.next.checked_add(count).is_none_or(|end| end > MAX_ID) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("journal {}: entry ids exhausted", self.path.display()),
            ));
        }
        let mut ops: Vec<Value> = del.iter().map(|id| json!({ "del": id })).collect();
        let ids: Vec<u64> = (0..count).map(|i| self.next + i).collect();
        for (id, v) in ids.iter().zip(add) {
            ops.push(json!({ "add": id, "v": v }));
        }
        self.file.write_all(&framed(&json!({ "ops": ops })))?;
        self.file.sync_data()?;
        for id in del {
            self.live.remove(id);
        }
        for (id, v) in ids.iter().zip(add) {
            self.live.insert(*id, v.clone());
        }
        self.next += count;
        self.dead += del.len();
        if self.wants_compaction() {
            self.compact()?;
        }
        Ok(ids)
    }

    /// The live entries, oldest first.
    pub fn live(&self) -> impl Iterator<Item = (u64, &Value)> {
        self.live.iter().map(|(k, v)| (*k, v))
    }

    /// Whether [`Journal::compact`] would shrink the file meaningfully.
    pub fn wants_compaction(&self) -> bool {
        self.dead >= COMPACT_AFTER && self.dead > self.live.len()
    }

    /// Rewrite the file with exactly the live entries this journal holds.
    /// Crash-safe: the old file stays in place until the new one is complete
    /// and synced, and the rename is synced through the directory.
    pub fn compact(&mut self) -> io::Result<()> {
        let tmp = self.path.with_extension("compact.tmp");
        {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            owner_only(&mut options);
            let mut f = options.open(&tmp)?;
            set_owner_only(&tmp)?;
            let mut out = framed(&self.header);
            let entries: Vec<(u64, &Value)> = self.live().collect();
            for chunk in entries.chunks(COMPACT_CHUNK) {
                let ops: Vec<Value> = chunk
                    .iter()
                    .map(|(id, v)| json!({ "add": id, "v": v }))
                    .collect();
                out.extend_from_slice(&framed(&json!({ "ops": ops })));
            }
            f.write_all(&out)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        sync_dir(&self.path)?;
        self.file = OpenOptions::new().append(true).open(&self.path)?;
        self.dead = 0;
        Ok(())
    }
}

#[cfg(test)]
impl Journal {
    /// Make every later write fail (its handle becomes read-only): the error
    /// path of every caller.
    pub(crate) fn break_for_test(&mut self) {
        self.file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .expect("existing journal file");
    }
}

/// A fresh, empty directory for a test (no external crate): under the system
/// temp dir, unique per process, call and time.
#[cfg(test)]
pub(crate) fn test_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "onyx-{tag}-{}-{}-{nanos}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).expect("test dir");
    d
}

/// The binding tests use.
#[cfg(test)]
pub(crate) const TEST_BINDING: Binding = Binding {
    chain_id: 84_532,
    contract: [0x5e; 20],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn open(p: &Path) -> io::Result<(Journal, Vec<(u64, Value)>)> {
        Journal::open(p, "test", &TEST_BINDING)
    }

    /// FUZZ-1 (cargo-fuzz `journals`): a checksummed line adding id
    /// `u64::MAX` overflowed the next-id count (a panic with overflow
    /// checks, a wrapped count without). It is refused as damage; the
    /// largest id is still read, and no id is given out past it.
    #[test]
    fn fuzz_1_an_id_out_of_range_refuses_the_open() {
        let d = test_dir("journal-fuzz-1");
        let p = d.join("j.jsonl");
        drop(open(&p).expect("create"));
        let add = |id: u64| {
            let mut f = OpenOptions::new().append(true).open(&p).expect("append");
            f.write_all(&framed(&json!({"ops": [{"add": id, "v": 1}]})))
                .expect("line");
        };
        add(MAX_ID - 1);
        {
            let (mut j, live) = open(&p).expect("the largest id reads");
            assert_eq!(live, vec![(MAX_ID - 1, json!(1))]);
            assert!(j.apply(&[], &[json!(2)]).is_err(), "no id past MAX_ID");
        }
        for id in [MAX_ID, u64::MAX] {
            let header = std::fs::read(&p).expect("read");
            add(id);
            let e = open(&p).expect_err("out of range");
            assert!(e.to_string().contains("out of range"), "{e}");
            std::fs::write(&p, &header).expect("restore");
        }
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn replays_adds_and_removes_in_order() {
        let d = test_dir("journal");
        let p = d.join("j.jsonl");
        {
            let (mut j, live) = open(&p).expect("open");
            assert!(live.is_empty());
            let ids = j
                .apply(&[], &[json!("a"), json!("b"), json!("c")])
                .expect("add");
            assert_eq!(ids, vec![0, 1, 2]);
            j.apply(&[1], &[json!("d")]).expect("del + add");
        }
        let (_, live) = open(&p).expect("reopen");
        assert_eq!(
            live,
            vec![(0, json!("a")), (2, json!("c")), (3, json!("d"))]
        );
        std::fs::remove_dir_all(d).ok();
    }

    /// A crash mid-write leaves a torn last line: it is cut off, and the
    /// journal keeps working; a damaged line in the middle refuses the open.
    #[test]
    fn a_torn_tail_is_cut_and_a_damaged_middle_refuses() {
        let d = test_dir("journal-torn");
        let p = d.join("j.jsonl");
        {
            let (mut j, _) = open(&p).expect("open");
            j.apply(&[], &[json!(1), json!(2)]).expect("add");
        }
        let full = std::fs::read(&p).expect("read");
        let mut f = OpenOptions::new().append(true).open(&p).expect("append");
        f.write_all(b"0123 {\"ops\":[{\"add\":2,\"v\":")
            .expect("torn");
        drop(f);
        {
            let (mut j, live) = open(&p).expect("torn tail");
            assert_eq!(live.len(), 2);
            assert_eq!(std::fs::read(&p).expect("read"), full, "torn line cut");
            assert_eq!(j.apply(&[], &[json!(3)]).expect("add"), vec![2]);
        }
        assert_eq!(open(&p).expect("reopen").1.len(), 3);
        // A complete line that does not parse is damage, not a torn write.
        let mut bytes = std::fs::read(&p).expect("read");
        let second = bytes.iter().position(|b| *b == b'\n').expect("header") + 1;
        bytes[second] = b'#';
        std::fs::write(&p, bytes).expect("write");
        let e = open(&p).expect_err("damaged");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-L3-F6: a batch is one line, so a crash keeps all of it or none:
    /// never the removal without the re-add.
    #[test]
    fn a_torn_batch_applies_nothing() {
        let d = test_dir("journal-batch");
        let p = d.join("j.jsonl");
        {
            let (mut j, _) = open(&p).expect("open");
            j.apply(&[], &[json!("receipt")]).expect("add");
        }
        let line = framed(&json!({"ops": [{"del": 0}, {"add": 1, "v": "receipt"}]}));
        let mut f = OpenOptions::new().append(true).open(&p).expect("append");
        f.write_all(&line[..line.len() - 5]).expect("torn");
        drop(f);
        let (_, live) = open(&p).expect("open");
        assert_eq!(live, vec![(0, json!("receipt"))], "the old state, whole");
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-L3-F6: a value damaged or edited on disk fails its checksum and
    /// refuses the open, instead of being replayed as a different value.
    /// REV-L4-F4: the sum has no key, so an edit that recomputes it loads:
    /// it detects damage, not a deliberate edit by someone who can write the
    /// state directory (out of scope; documented in THREATS).
    #[test]
    fn an_edited_line_fails_its_checksum() {
        let d = test_dir("journal-sum");
        let p = d.join("j.jsonl");
        {
            let (mut j, _) = open(&p).expect("open");
            j.apply(&[], &[json!({"amount": "2500"})]).expect("add");
        }
        let s = std::fs::read_to_string(&p)
            .expect("read")
            .replace("\"2500\"", "\"9500\"");
        std::fs::write(&p, s).expect("write");
        let e = open(&p).expect_err("edited");
        assert!(e.to_string().contains("checksum"), "{e}");
        // The same edit with the sum recomputed loads (unkeyed by design).
        let text = std::fs::read_to_string(&p).expect("read");
        let mut out = String::new();
        for (i, l) in text.lines().enumerate() {
            let body = l.split_once(' ').expect("framed").1;
            if i == 1 {
                out.push_str(&format!("{} {body}\n", sum(body.as_bytes())));
            } else {
                out.push_str(l);
                out.push('\n');
            }
        }
        std::fs::write(&p, out).expect("write");
        let (_, live) = open(&p).expect("a recomputed sum loads");
        assert_eq!(live[0].1, json!({"amount": "9500"}));
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-L4-F7: a lock file left behind by a killed holder never blocks a
    /// restart (the lock dies with the process); a live holder in another
    /// process refuses after the retry window; a holder that lets go within
    /// the window (a forked child before its exec) does not.
    #[test]
    fn a_dead_holder_never_blocks_and_a_brief_one_is_waited_for() {
        let d = test_dir("journal-holder");
        let p = d.join("j.jsonl");
        drop(open(&p).expect("create"));
        let lock = lock_path(&p);
        let hold = |secs: &str| {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg("exec 9>\"$1\"; flock -x 9; echo locked; exec sleep \"$2\"")
                .arg("sh")
                .arg(&lock)
                .arg(secs)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("flock(1) holder");
            let mut line = String::new();
            BufReader::new(child.stdout.take().expect("stdout"))
                .read_line(&mut line)
                .expect("read");
            assert_eq!(line.trim(), "locked");
            child
        };
        let mut live = hold("30");
        let e = open(&p).expect_err("live holder");
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        live.kill().expect("SIGKILL");
        live.wait().expect("wait");
        assert!(lock.exists(), "the lock file stays behind");
        drop(open(&p).expect("a dead holder's lock file is harmless"));
        let mut brief = hold("0.3");
        drop(open(&p).expect("waited for a brief holder"));
        brief.wait().expect("wait");
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-L3-F4: a second process on the same journal refuses to start;
    /// a journal of another deployment refuses to open.
    #[test]
    fn one_writer_and_one_deployment_per_journal() {
        let d = test_dir("journal-lock");
        let p = d.join("j.jsonl");
        let (first, _) = open(&p).expect("open");
        let e = open(&p).expect_err("second writer");
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock, "{e}");
        drop(first);
        open(&p).expect("free again");
        let other = Binding {
            chain_id: 84_532,
            contract: [0x77; 20],
        };
        let e = Journal::open(&p, "test", &other).expect_err("other contract");
        assert!(e.to_string().contains("journal is for"), "{e}");
        let e = Journal::open(&p, "other-kind", &TEST_BINDING).expect_err("other kind");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(d).ok();
    }

    #[cfg(unix)]
    #[test]
    fn journal_and_lock_files_are_owner_only_even_if_preexisting_modes_are_open() {
        use std::os::unix::fs::PermissionsExt;
        let d = test_dir("journal-private-mode");
        let p = d.join("j.jsonl");
        let (journal, _) = open(&p).expect("create journal");
        let lock = lock_path(&p);
        for path in [&p, &lock] {
            assert_eq!(
                std::fs::metadata(path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
                .expect("loosen for regression");
        }
        drop(journal);
        let (journal, _) = open(&p).expect("reopen and tighten");
        for path in [&p, &lock] {
            assert_eq!(
                std::fs::metadata(path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(journal);
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-L3-F1: compaction writes the journal's own live entries, so a
    /// caller's queue state can never leave one out.
    #[test]
    fn compaction_keeps_exactly_the_live_entries() {
        let d = test_dir("journal-compact");
        let p = d.join("j.jsonl");
        let (mut j, _) = open(&p).expect("open");
        let adds: Vec<Value> = (0..COMPACT_AFTER as u64 + 10).map(|i| json!(i)).collect();
        let ids = j.apply(&[], &adds).expect("add");
        // The removal and a re-add in one batch trigger compaction.
        j.apply(&ids[..COMPACT_AFTER], &[json!("again")])
            .expect("del + add");
        assert!(!j.wants_compaction(), "compacted in apply");
        assert_eq!(
            j.apply(&[], &[json!("x")]).expect("add"),
            vec![ids.len() as u64 + 1]
        );
        drop(j);
        let (_, live) = open(&p).expect("reopen");
        assert_eq!(live.len(), 12);
        assert_eq!(live[0].0, COMPACT_AFTER as u64);
        assert_eq!(live[10], (ids.len() as u64, json!("again")));
        std::fs::remove_dir_all(d).ok();
    }
}
