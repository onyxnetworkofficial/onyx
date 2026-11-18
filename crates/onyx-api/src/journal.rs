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

/// Never open through a symbolic link at the file's own name (review B2-R1 H1): whoever can
/// write the directory could otherwise point a journal's or a lock's name at any file, and a
/// more privileged reader or writer of the journal would follow it.
fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The flag's bits fit an `i32` on every Unix (`O_NOFOLLOW` is far below 2^31).
        let flag = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0);
        options.custom_flags(flag);
    }
    #[cfg(not(unix))]
    let _ = options;
}

/// Open `path` with `options`, never through a symbolic link, and only when it is a regular
/// file; owner-only mode is then set on the open file itself, never by name. Public for the
/// publisher tool's transaction record (review RETRY-R2-L2).
pub fn open_plain(options: &mut OpenOptions, path: &Path, write: bool) -> io::Result<File> {
    no_follow(options);
    let file = options.open(path).map_err(|e| {
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            not_plain(path, "a symbolic link")
        } else {
            e
        }
    })?;
    if !file.metadata()?.is_file() {
        return Err(not_plain(path, "not a regular file"));
    }
    #[cfg(unix)]
    if write {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = write;
    Ok(file)
}

fn not_plain(path: &Path, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} is {what}: a journal and its lock are regular files in the state directory, never links",
            path.display()
        ),
    )
}

/// Refuse to go on as root (review B2-R1 H1). For a tool an operator runs by hand on a
/// node's state directory and that writes there: the node's own user owns that directory, so
/// a root process could be led through a planted link, and what it creates would belong to
/// root and be unreadable to the node. Run such a tool as the directory's owner.
pub fn refuse_root(tool: &str) -> io::Result<()> {
    #[cfg(unix)]
    if rustix::process::geteuid().is_root() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{tool} does not run as root on a node's state directory: run it as the user that owns the journal (for example `sudo setpriv --reuid=UID --regid=GID --clear-groups -- {tool} ...` from inside that directory)"
            ),
        ));
    }
    let _ = tool;
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

/// Entries per line when a damaged append-only journal is written afresh.
const REPAIR_CHUNK: usize = 64;

/// What [`Journal::read_lenient`] found.
#[derive(Debug, Default)]
pub struct Lenient {
    /// The values of every good line's adds, in file order.
    pub entries: Vec<Value>,
    /// Lines that are damaged, too long, or not adds (the header counts when it is wrong).
    pub bad_lines: usize,
    /// The first line is not this journal's header: nothing of the file is taken.
    pub foreign: bool,
}

/// What [`Held::open`] gives: the journal, its entries, and where a damaged
/// file was copied when it was set aside.
pub type Reopened = (Journal, Vec<(u64, Value)>, Option<PathBuf>);

/// An append-only journal whose lock is held and that was read past damage, not yet opened
/// for writing ([`Journal::hold`]). Dropping it changes nothing on disk.
#[derive(Debug)]
pub struct Held {
    path: PathBuf,
    lock: File,
    found: Lenient,
    header: Value,
}

impl Held {
    /// The values of every good line, in file order.
    pub fn entries(&self) -> &[Value] {
        &self.found.entries
    }

    /// How many lines are damaged.
    pub fn bad_lines(&self) -> usize {
        self.found.bad_lines
    }

    /// Open the journal for writing. When it has damaged lines it is first set aside: copied
    /// to `<path>.corrupt-<stamp>` (owner-only) and replaced, by a rename, with a fresh journal
    /// holding every entry of a good line that `keep` takes. Returns the copy's path when
    /// that happened.
    pub fn open(self, stamp: u64, keep: impl Fn(&Value) -> bool) -> io::Result<Reopened> {
        let Held {
            path,
            lock,
            found,
            header,
        } = self;
        let mut aside = None;
        if found.bad_lines > 0 {
            let mut name = path.as_os_str().to_owned();
            name.push(format!(".corrupt-{stamp}"));
            let copy = PathBuf::from(name);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            owner_only(&mut options);
            let mut to = open_plain(&mut options, &copy, true)?;
            let mut from = open_plain(OpenOptions::new().read(true), &path, false)?;
            io::copy(&mut from, &mut to)?;
            to.sync_all()?;
            let tmp = path.with_extension("repair.tmp");
            let _ = std::fs::remove_file(&tmp);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            owner_only(&mut options);
            let mut f = open_plain(&mut options, &tmp, true)?;
            let mut bytes = framed(&header);
            let kept: Vec<&Value> = found.entries.iter().filter(|v| keep(v)).collect();
            let mut id = 0u64;
            for chunk in kept.chunks(REPAIR_CHUNK) {
                let ops: Vec<Value> = chunk
                    .iter()
                    .map(|v| {
                        id += 1;
                        json!({ "add": id - 1, "v": v })
                    })
                    .collect();
                bytes.extend_from_slice(&framed(&json!({ "ops": ops })));
            }
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            sync_dir(&path)?;
            aside = Some(copy);
        }
        let (journal, entries) = Journal::open_locked_with(path, lock, header)?;
        Ok((journal, entries, aside))
    }
}

/// What a replay of a journal file found.
struct Replayed {
    live: BTreeMap<u64, Value>,
    next: u64,
    dead: usize,
    /// Bytes of whole, good lines (a torn last line is past it).
    good_len: u64,
    has_header: bool,
}

/// Replay `file` from its start: the header, then every batch, each checked
/// whole. A torn last line ends the replay; any other bad line is an error.
fn replay(file: &mut File, path: &Path, header: &Value) -> io::Result<Replayed> {
    let mut live: BTreeMap<u64, Value> = BTreeMap::new();
    let (mut next, mut dead) = (0u64, 0usize);
    let mut good_len = 0u64;
    let mut has_header = false;
    {
        let mut r = BufReader::new(file);
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
                .ok_or_else(|| bad(path, n, "checksum or JSON does not match"))?;
            if n == 1 {
                if line != *header {
                    return Err(bad(
                        path,
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
                .ok_or_else(|| bad(path, n, "no ops"))?;
            // A line is one batch: check it whole before applying it.
            let mut dels = std::collections::BTreeSet::new();
            let mut adds = Vec::new();
            let mut staged_next = next;
            for op in ops {
                if let Some(id) = op.get("add").and_then(Value::as_u64) {
                    let v = op
                        .get("v")
                        .cloned()
                        .ok_or_else(|| bad(path, n, "no value"))?;
                    if id < staged_next || live.contains_key(&id) {
                        return Err(bad(path, n, format!("id {id} reused")));
                    }
                    if id >= MAX_ID {
                        return Err(bad(path, n, format!("id {id} out of range")));
                    }
                    staged_next = id + 1;
                    adds.push((id, v));
                } else if let Some(id) = op.get("del").and_then(Value::as_u64) {
                    if !live.contains_key(&id) || !dels.insert(id) {
                        return Err(bad(path, n, format!("removes unknown id {id}")));
                    }
                } else {
                    return Err(bad(path, n, "neither add nor del"));
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
    Ok(Replayed {
        live,
        next,
        dead,
        good_len,
        has_header,
    })
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
        let lock = Self::lock(&path)?;
        Self::open_locked(path, lock, kind, bind)
    }

    /// Take the exclusive lock of the journal at `path` (`<path>.lock`), waiting up to
    /// [`LOCK_WAIT`].
    fn lock(path: &Path) -> io::Result<File> {
        let lock_path = lock_path(path);
        let mut lock_options = OpenOptions::new();
        lock_options.write(true).create(true).truncate(false);
        owner_only(&mut lock_options);
        let lock = open_plain(&mut lock_options, &lock_path, true)?;
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
        Ok(lock)
    }

    fn open_locked(
        path: PathBuf,
        lock: File,
        kind: &str,
        bind: &Binding,
    ) -> io::Result<(Journal, Vec<(u64, Value)>)> {
        Self::open_locked_with(path, lock, bind.header(kind))
    }

    fn open_locked_with(
        path: PathBuf,
        lock: File,
        header: Value,
    ) -> io::Result<(Journal, Vec<(u64, Value)>)> {
        let existed = path.exists();
        let mut file_options = OpenOptions::new();
        file_options.read(true).append(true).create(true);
        owner_only(&mut file_options);
        let mut file = open_plain(&mut file_options, &path, true)?;
        if !existed {
            sync_dir(&path)?;
        }
        let Replayed {
            live,
            next,
            dead,
            good_len,
            has_header,
        } = replay(&mut file, &path, &header)?;
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

    /// Read the journal at `path` without the lock and without writing: the
    /// live entries, oldest first. For a reader beside the one writer (the
    /// proposal reviewer reads the node's intake journal while the node runs,
    /// and the node reads the reviewer's decisions; ADR-0039 amendment 3). A
    /// missing file, or one whose header is not written yet, reads as empty; a
    /// torn last line is a write not yet acknowledged and is left out (and left
    /// on disk); any other damage, or another header, is an error as in
    /// [`Journal::open`]. Only for a journal that never removes entries: a
    /// compaction renames a new file into place, which a reader holding the
    /// old one would not see.
    pub fn read_only(
        path: impl AsRef<Path>,
        kind: &str,
        bind: &Binding,
    ) -> io::Result<Vec<(u64, Value)>> {
        Self::read_only_capped(path, kind, bind, u64::MAX)
    }

    /// [`Journal::read_only`] for a file of at most `max_bytes`: a larger one is an error and
    /// is not read (review B2-R2 L6).
    pub fn read_only_capped(
        path: impl AsRef<Path>,
        kind: &str,
        bind: &Binding,
        max_bytes: u64,
    ) -> io::Result<Vec<(u64, Value)>> {
        let path = path.as_ref();
        let mut file = match open_plain(OpenOptions::new().read(true), path, false) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is {len} bytes, over the {max_bytes} this journal may hold",
                    path.display()
                ),
            ));
        }
        let found = replay(&mut file, path, &bind.header(kind))?;
        Ok(found.live.into_iter().collect())
    }

    /// Read an append-only journal (one that only ever adds) the way [`Journal::read_only`]
    /// does, but past damage (review B2-R1 M3): every line with a good checksum that adds
    /// entries is kept, in order; every other line is counted, never an error. A file over
    /// `max_bytes` is an error (nothing is read), a line over `max_line` bytes is a bad line.
    /// For a journal whose every entry is checked again by its reader, so that one damaged
    /// line does not hide the rest.
    pub fn read_lenient(
        path: impl AsRef<Path>,
        kind: &str,
        bind: &Binding,
        max_bytes: u64,
        max_line: usize,
    ) -> io::Result<Lenient> {
        let path = path.as_ref();
        let mut out = Lenient::default();
        let file = match open_plain(OpenOptions::new().read(true), path, false) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is {len} bytes, over the {max_bytes} this journal may hold",
                    path.display()
                ),
            ));
        }
        let header = bind.header(kind);
        // The size was checked above; a file that grows while it is read is still read at
        // most `max_bytes` far (review B2-R2 L6).
        let mut r = BufReader::new(io::Read::take(file, max_bytes));
        let mut buf = Vec::new();
        let (mut n, mut next) = (0usize, 0u64);
        loop {
            buf.clear();
            if r.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            n += 1;
            if buf.last() != Some(&b'\n') {
                // A torn last write: never acknowledged.
                break;
            }
            let line = (buf.len() <= max_line)
                .then(|| unframe(&buf[..buf.len() - 1]))
                .flatten();
            if n == 1 {
                // Without this journal's own header nothing in the file is taken.
                if line.as_ref() != Some(&header) {
                    out.foreign = true;
                    out.bad_lines = 1;
                    break;
                }
                continue;
            }
            // Adds only, with ids in order (what a strict replay takes too).
            let mut staged = next;
            let adds: Option<Vec<Value>> = line.as_ref().and_then(|l| {
                l["ops"]
                    .as_array()?
                    .iter()
                    .map(|op| {
                        let id = op.get("add").and_then(Value::as_u64)?;
                        if id < staged || id >= MAX_ID {
                            return None;
                        }
                        staged = id + 1;
                        op.get("v").cloned()
                    })
                    .collect()
            });
            match adds {
                Some(values) => {
                    next = staged;
                    out.entries.extend(values);
                }
                None => out.bad_lines += 1,
            }
        }
        Ok(out)
    }

    /// Take the lock of an append-only journal and read it past damage, without changing
    /// anything yet (reviews B2-R1 M3, B2-R2 M1). The caller looks at the entries, decides
    /// whether it will write at all, and only then calls [`Held::open`], which sets a damaged
    /// file aside before the journal is opened for writing. A file that does not start with
    /// this journal's header is refused here: it is not this journal, nothing of it can be
    /// kept, and it is never rewritten.
    pub fn hold(
        path: impl AsRef<Path>,
        kind: &str,
        bind: &Binding,
        limits: (u64, usize),
    ) -> io::Result<Held> {
        let path = path.as_ref().to_path_buf();
        let lock = Self::lock(&path)?;
        let found = Self::read_lenient(&path, kind, bind, limits.0, limits.1)?;
        if found.foreign {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} does not start with the header of a {kind} journal: it is left as it is and nothing is written; move it away by hand and restore the last good copy",
                    path.display()
                ),
            ));
        }
        Ok(Held {
            path,
            lock,
            found,
            header: bind.header(kind),
        })
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

    /// How many removed entries the file still holds: each is a line that added the entry
    /// and a line that removed it. Nothing of a removed entry is on disk only when this is 0.
    pub fn dead(&self) -> usize {
        self.dead
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
            // A new file of our own, never one that is there (or a link under its name).
            let _ = std::fs::remove_file(&tmp);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            owner_only(&mut options);
            let mut f = open_plain(&mut options, &tmp, true)?;
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
        self.file = open_plain(OpenOptions::new().append(true), &self.path, false)?;
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

    /// ROADMAP S5: a reader beside the one writer takes no lock and writes nothing: it works
    /// while the writer holds the journal, leaves a torn last line on disk (and out of what it
    /// returns), reads a missing file as empty, and refuses damage and another header.
    #[test]
    fn a_read_only_reader_works_beside_the_writer_and_never_writes() {
        let d = test_dir("journal-read-only");
        let p = d.join("j.jsonl");
        assert!(Journal::read_only(&p, "test", &TEST_BINDING)
            .expect("missing")
            .is_empty());
        assert!(!p.exists() && !lock_path(&p).exists(), "nothing is created");
        let (mut j, _) = open(&p).expect("writer");
        j.apply(&[], &[json!("a"), json!("b")]).expect("apply");
        let seen = Journal::read_only(&p, "test", &TEST_BINDING).expect("beside the writer");
        assert_eq!(seen, vec![(0, json!("a")), (1, json!("b"))]);
        j.apply(&[], &[json!("c")])
            .expect("the writer is not disturbed");
        drop(j);
        let mut bytes = std::fs::read(&p).expect("file");
        bytes.extend_from_slice(b"0123 {\"ops\":[{\"add\":");
        std::fs::write(&p, &bytes).expect("torn");
        assert_eq!(
            Journal::read_only(&p, "test", &TEST_BINDING)
                .expect("torn")
                .len(),
            3
        );
        assert_eq!(std::fs::read(&p).expect("file"), bytes, "never cut");
        assert!(
            Journal::read_only(&p, "other", &TEST_BINDING).is_err(),
            "another kind"
        );
        let mut damaged = bytes.clone();
        let at = damaged.iter().position(|b| *b == b'\n').expect("line") + 20;
        damaged[at] ^= 1;
        std::fs::write(&p, &damaged).expect("damage");
        assert!(
            Journal::read_only(&p, "test", &TEST_BINDING).is_err(),
            "a damaged middle"
        );
    }

    /// Review B2-R1 H1: a journal and its lock are never opened through a symbolic link at
    /// their own name, for writing or for reading, and nothing is created or changed where
    /// the link points; a directory under the name is refused too.
    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_journal_or_its_lock_is_never_followed() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let d = test_dir("journal-symlink");
        let outside = test_dir("journal-symlink-outside");
        let p = d.join("j.jsonl");
        let victim = outside.join("victim");
        std::fs::write(&victim, b"not a journal").expect("victim");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("mode");
        let unchanged = || {
            let m = std::fs::metadata(&victim).expect("victim");
            assert_eq!(m.permissions().mode() & 0o777, 0o644);
            assert_eq!(std::fs::read(&victim).expect("victim"), b"not a journal");
            assert_eq!(std::fs::read_dir(&outside).expect("dir").count(), 1);
        };
        for target in [victim.clone(), outside.join("new")] {
            symlink(&target, lock_path(&p)).expect("lock link");
            let e = open(&p).expect_err("a linked lock");
            assert!(e.to_string().contains("symbolic link"), "{e}");
            assert!(!p.exists(), "nothing is opened after a refused lock");
            std::fs::remove_file(lock_path(&p)).expect("unlink");
            symlink(&target, &p).expect("journal link");
            let e = open(&p).expect_err("a linked journal");
            assert!(e.to_string().contains("symbolic link"), "{e}");
            assert!(Journal::read_only(&p, "test", &TEST_BINDING).is_err());
            assert!(Journal::read_lenient(&p, "test", &TEST_BINDING, 1 << 20, 1 << 16).is_err());
            std::fs::remove_file(&p).expect("unlink");
            std::fs::remove_file(lock_path(&p)).expect("the refused open's lock");
            unchanged();
        }
        std::fs::create_dir(&p).expect("dir");
        assert!(open(&p).is_err(), "a directory is not a journal");
        std::fs::remove_dir(&p).expect("rmdir");
        drop(open(&p).expect("a plain file opens"));
    }

    /// Review B2-R1 M3, L2: the lenient reader of an append-only journal keeps every good
    /// line past a damaged one and counts the rest; a file or a line over its cap is not
    /// read; and a damaged journal is copied aside and written afresh from its good lines.
    #[test]
    fn a_damaged_append_only_journal_is_read_past_the_damage_and_set_aside() {
        let d = test_dir("journal-lenient");
        let p = d.join("j.journal");
        let read = |max_bytes, max_line| {
            Journal::read_lenient(&p, "test", &TEST_BINDING, max_bytes, max_line).expect("read")
        };
        assert!(
            read(1 << 20, 1 << 16).entries.is_empty(),
            "a missing file is empty"
        );
        {
            let (mut j, _) = open(&p).expect("open");
            for n in 0..5 {
                j.apply(&[], &[json!({ "n": n })]).expect("apply");
            }
        }
        let good = std::fs::read(&p).expect("file");
        let all = read(1 << 20, 1 << 16);
        assert_eq!(
            (all.entries.len(), all.bad_lines, all.foreign),
            (5, 0, false)
        );
        // The third line is damaged: the four other entries are kept, in order.
        let mut bytes = good.clone();
        let starts: Vec<usize> = std::iter::once(0)
            .chain(
                bytes
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| **b == b'\n')
                    .map(|(i, _)| i + 1),
            )
            .collect();
        bytes[starts[2] + 25] ^= 1;
        bytes.extend_from_slice(b"0123 {\"ops\":[{\"add\":");
        std::fs::write(&p, &bytes).expect("damage");
        assert!(open(&p).is_err(), "the strict open refuses it");
        let some = read(1 << 20, 1 << 16);
        assert_eq!(some.bad_lines, 1);
        assert_eq!(
            some.entries,
            [0, 2, 3, 4].map(|n| json!({ "n": n })).to_vec(),
            "the torn tail is not a bad line"
        );
        // A line over the cap is a bad line; a file over the cap is not read at all.
        let short = read(1 << 20, 40);
        assert!(
            short.foreign && short.entries.is_empty(),
            "a header over the line cap: nothing is taken"
        );
        assert!(Journal::read_lenient(&p, "test", &TEST_BINDING, 64, 1 << 16).is_err());
        // Set aside: the copy is the damaged file, the journal holds the kept entries.
        let lim = (1 << 20, 1 << 16);
        // Holding the journal changes nothing: a caller that then decides not to write leaves
        // the damaged file as it is (review B2-R2 M1).
        let held = Journal::hold(&p, "test", &TEST_BINDING, lim).expect("hold");
        assert_eq!((held.entries().len(), held.bad_lines()), (4, 1));
        drop(held);
        assert_eq!(std::fs::read(&p).expect("file"), bytes, "nothing written");
        assert!(!d.join("j.journal.corrupt-7").exists());
        let (mut j, entries, aside) = Journal::hold(&p, "test", &TEST_BINDING, lim)
            .expect("hold")
            .open(7, |v| v["n"] != 4)
            .expect("open");
        let aside = aside.expect("set aside");
        assert_eq!(aside, d.join("j.journal.corrupt-7"));
        assert_eq!(std::fs::read(&aside).expect("copy"), bytes);
        let values: Vec<Value> = entries.into_iter().map(|(_, v)| v).collect();
        assert_eq!(values, [0, 2, 3].map(|n| json!({ "n": n })).to_vec());
        j.apply(&[], &[json!({ "n": 9 })])
            .expect("the journal takes writes again");
        drop(j);
        let (_, entries, aside) = Journal::hold(&p, "test", &TEST_BINDING, lim)
            .expect("hold")
            .open(8, |_| true)
            .expect("open");
        assert_eq!(
            (entries.len(), aside),
            (4, None),
            "a good journal is left alone"
        );
        // Another journal's header: nothing is taken, and it is never held for a rewrite.
        let other =
            Journal::read_lenient(&p, "other", &TEST_BINDING, 1 << 20, 1 << 16).expect("read");
        assert_eq!(
            (other.entries.len(), other.bad_lines, other.foreign),
            (0, 1, true)
        );
        let whole = std::fs::read(&p).expect("file");
        let e = Journal::hold(&p, "other", &TEST_BINDING, lim).expect_err("foreign");
        assert!(e.to_string().contains("left as it is"), "{e}");
        assert_eq!(std::fs::read(&p).expect("file"), whole);
        // A link planted under the compaction's temporary name is not written through
        // (review B2-R2 L4).
        #[cfg(unix)]
        {
            let victim = d.join("victim");
            std::fs::write(&victim, b"untouched").expect("victim");
            let (mut j, _) = open(&p).expect("open");
            std::os::unix::fs::symlink(&victim, p.with_extension("compact.tmp")).expect("link");
            j.compact().expect("compaction makes its own new file");
            assert_eq!(std::fs::read(&victim).expect("victim"), b"untouched");
            j.apply(&[], &[json!({ "n": 10 })]).expect("and writes on");
        }
        // A journal that removes entries is not append-only: a removal is a bad line.
        std::fs::write(&p, &good).expect("restore");
        {
            let (mut j, _) = open(&p).expect("open");
            j.apply(&[0], &[]).expect("remove");
        }
        assert_eq!(read(1 << 20, 1 << 16).bad_lines, 1);
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
