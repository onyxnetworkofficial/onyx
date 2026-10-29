//! `onyx-node doctor`: a hardware preflight (ROADMAP P5, docs/ops/hardware.md).
//!
//! ```text
//! onyx-node doctor [--role contributor|validator|indexer|node|coordinator]
//!                  [--chain bitcoin|ethereum] [--data-dir PATH]
//! ```
//!
//! It reads the logical CPU count this process may use, the usable RAM (the
//! smaller of `/proc/meminfo` `MemTotal` and any cgroup `memory.max` (v2) or
//! `memory.limit_in_bytes` (v1) on the way to this process), the size and
//! free space of the data directory's filesystem, and the kind of its disk:
//! NVMe, another non-rotational disk (a SATA or SAS SSD, eMMC), or rotational
//! (`/sys/dev/block/<major>:<minor>`: `queue/rotational`, the parent disk's
//! for a partition, the disks under `slaves/` for device-mapper and md). It
//! compares each with the role's row in docs/ops/hardware.md ("Doctor
//! thresholds") and prints one line per check:
//!
//! - `PASS`: at or above the recommended value (or the minimum, when the row
//!   has no recommended value);
//! - `WARN`: at or above the minimum but below the recommended value, or a
//!   value the machine did not let it read on Linux, or a disk that is not
//!   NVMe where the row recommends NVMe;
//! - `FAIL`: below the minimum;
//! - `SKIP`: the row has no measured value for this check yet, or the value
//!   is not read on this OS (`/proc` and `/sys` are Linux only).
//!
//! It needs no root, opens files only for reading and writes nothing. It is
//! not protocol data; it keeps integers anyway (bytes, MiB, GiB).
//!
//! The node runs the `node` row at every start ([`preflight_node`]) and
//! prints what is not `PASS`. It refuses to start only when the state
//! directory's filesystem has less free space than the largest state
//! directory the soak measured (TRACKER 2026-10-18 P5 decision): every other
//! shortfall is a warning, since a slow machine still serves correct answers.

use std::path::{Path, PathBuf};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// A role of docs/ops/hardware.md that the doctor checks. Buyers need no
/// preflight (a browser or the CLI verifier; see the doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A miner: a full node plus Onyx's recipe runs.
    Contributor,
    /// A validator rerunning recipes on the same blocks.
    Validator,
    /// The entity indexer behind label sets and reserve bases.
    Indexer,
    /// `onyx-node` serving the API.
    Node,
    /// The coordinator settling rounds.
    Coordinator,
}

impl Role {
    /// Every role, in the doc's order.
    pub const ALL: [Role; 5] = [
        Role::Contributor,
        Role::Validator,
        Role::Indexer,
        Role::Node,
        Role::Coordinator,
    ];

    /// The name used on the command line and in the doc's table.
    pub fn name(self) -> &'static str {
        match self {
            Role::Contributor => "contributor",
            Role::Validator => "validator",
            Role::Indexer => "indexer",
            Role::Node => "node",
            Role::Coordinator => "coordinator",
        }
    }

    /// Parse a command-line name.
    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.name() == s)
    }

    /// Whether the role runs a chain client, so the chain matters.
    fn runs_a_chain_client(self) -> bool {
        matches!(self, Role::Contributor | Role::Validator | Role::Indexer)
    }
}

/// The chain a contributor, validator or indexer works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// Bitcoin mainnet (built).
    Bitcoin,
    /// Ethereum (planned, ROADMAP P4): no Onyx requirement measured yet.
    Ethereum,
}

impl Chain {
    /// Parse a command-line name.
    pub fn parse(s: &str) -> Option<Chain> {
        match s {
            "bitcoin" => Some(Chain::Bitcoin),
            "ethereum" => Some(Chain::Ethereum),
            _ => None,
        }
    }
}

/// What the disk type check asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskType {
    /// Any local disk.
    Any,
    /// NVMe is recommended (every measured machine used NVMe); any other
    /// disk warns, since nothing is measured on it.
    NvmeRecommended,
}

/// The kind of disk behind a directory, as Linux reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskKind {
    /// An NVMe disk (non-rotational).
    Nvme,
    /// Non-rotational but not NVMe: a SATA or SAS SSD, eMMC, or a
    /// device-mapper or md device over mixed disks.
    Ssd,
    /// Rotational: an HDD, or a virtual disk that reports itself rotational.
    Hdd,
}

/// One row of the doc's "Doctor thresholds" table. `None` is "not measured
/// yet": the check prints `SKIP` (or `PASS` against the minimum alone when
/// only the recommended value is missing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    /// Logical CPUs.
    pub cpus_min: Option<u64>,
    /// Logical CPUs, recommended.
    pub cpus_rec: Option<u64>,
    /// Usable RAM, MiB.
    pub ram_min_mib: Option<u64>,
    /// Usable RAM, MiB, recommended.
    pub ram_rec_mib: Option<u64>,
    /// Size of the data directory's filesystem, GiB.
    pub disk_min_gib: Option<u64>,
    /// Size of the data directory's filesystem, GiB, recommended.
    pub disk_rec_gib: Option<u64>,
    /// Free space on the data directory's filesystem, GiB.
    pub free_min_gib: Option<u64>,
    /// Free space, GiB, recommended.
    pub free_rec_gib: Option<u64>,
    /// Disk type.
    pub disk_type: DiskType,
}

/// The role's thresholds on a chain (docs/ops/hardware.md, "Doctor
/// thresholds", where each number's source is named). Ethereum is planned:
/// no Onyx requirement is measured for it, so a chain-client role on it is an
/// error, not a guess.
pub fn need(role: Role, chain: Chain) -> Result<Need, String> {
    if role.runs_a_chain_client() && chain == Chain::Ethereum {
        return Err(format!(
            "ethereum is planned (ROADMAP P4): no Onyx requirement for the {} role is measured yet; \
             docs/ops/hardware.md links reth's and geth's own requirement pages",
            role.name()
        ));
    }
    Ok(match role {
        // Bitcoin Core 2 GB (bitcoin.org, 1,907 MiB) + one recipe run at the
        // host's memory cap (2 x 2 GiB + 16 MiB, benchmarks.md) = 6,020 MiB,
        // rounded up to 6 GiB; 836 GiB = blocks 825 + chainstate 11 measured
        // on the maintainer's indexer; free space: one day (884 MB) and one
        // week (5.9 GB) of fetched blocks, measured; recommended = the
        // reference machine every Onyx benchmark ran on (12 CPUs, 60 GiB).
        Role::Contributor | Role::Validator => Need {
            cpus_min: Some(2),
            cpus_rec: Some(12),
            ram_min_mib: Some(6 * 1024),
            ram_rec_mib: Some(60 * 1024),
            disk_min_gib: Some(836),
            disk_rec_gib: None,
            free_min_gib: Some(1),
            free_rec_gib: Some(6),
            disk_type: DiskType::NvmeRecommended,
        },
        // The maintainer's indexer, measured 2026-10-18: 24 logical CPUs,
        // 91 GiB; its stores (blocks 825 + chainstate 11 + indexes 71 + BRK
        // 1,126 GiB) are the size floor. Below its CPU and RAM nothing is
        // measured, so those only warn.
        Role::Indexer => Need {
            cpus_min: None,
            cpus_rec: Some(24),
            ram_min_mib: None,
            ram_rec_mib: Some(91 * 1024),
            disk_min_gib: Some(2033),
            disk_rec_gib: None,
            free_min_gib: None,
            free_rec_gib: None,
            disk_type: DiskType::NvmeRecommended,
        },
        // RAM: the shipped unit's MemoryMax=2G is a cap, not a need (25.2
        // MiB peak RSS in the soak), and the only size the node was tested
        // in, so it is the recommended value and nothing is a minimum. 1 GiB
        // free: the image (53.8 MB) plus the soak's largest state directory
        // (542,874 bytes) is 54.4 MB; 1 GiB is a margin chosen on top, before
        // your catalog's rows.
        Role::Node => Need {
            cpus_min: Some(1),
            cpus_rec: None,
            ram_min_mib: None,
            ram_rec_mib: Some(2 * 1024),
            disk_min_gib: None,
            disk_rec_gib: None,
            free_min_gib: Some(1),
            free_rec_gib: None,
            disk_type: DiskType::Any,
        },
        // Not measured yet.
        Role::Coordinator => Need {
            cpus_min: None,
            cpus_rec: None,
            ram_min_mib: None,
            ram_rec_mib: None,
            disk_min_gib: None,
            disk_rec_gib: None,
            free_min_gib: None,
            free_rec_gib: None,
            disk_type: DiskType::Any,
        },
    })
}

/// The largest state directory the 2026-10-14 soak measured (bytes,
/// docs/audit/2026-10-14-soak.md run 2; it grew from 790 bytes in 133.5 min
/// of paid load): a node start refuses below this much free space. No code
/// limit bounds the state files (the journals compact by entry count, not
/// bytes), so this measured growth is the only number for the room the
/// node's own writes take; below it a run like the soak's would hit a full
/// disk on its state writes. It is the one shortfall that refuses.
pub const NODE_REFUSE_FREE_BYTES: u64 = 542_874;

/// Where the doctor reads `/proc` and `/sys` (fixtures in tests).
#[derive(Clone, Debug)]
pub struct Roots {
    /// `/proc`.
    pub proc: PathBuf,
    /// `/sys`.
    pub sys: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Roots {
            proc: PathBuf::from("/proc"),
            sys: PathBuf::from("/sys"),
        }
    }
}

/// What the machine reported. `None`: could not be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Logical CPUs this process may use.
    pub cpus: Option<u64>,
    /// Usable RAM in bytes.
    pub ram_bytes: Option<u64>,
    /// Where `ram_bytes` came from.
    pub ram_from: &'static str,
    /// Size of the data directory's filesystem, bytes.
    pub disk_bytes: Option<u64>,
    /// Free space for an unprivileged user, bytes.
    pub free_bytes: Option<u64>,
    /// The data directory's disk kind.
    pub disk: Option<DiskKind>,
    /// Not Linux: `/proc` and `/sys` are not read, so a missing value is a
    /// `SKIP`, not a `WARN`.
    pub not_linux: bool,
}

/// `MemTotal` from `/proc/meminfo`, in bytes.
pub fn parse_meminfo(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let mut it = line["MemTotal:".len()..].split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    match it.next() {
        Some("kB") => n.checked_mul(1024),
        _ => None,
    }
}

/// The cgroup v2 path from `/proc/self/cgroup` (the `0::` line).
pub fn parse_cgroup_path(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
}

/// The cgroup v1 memory controller's path from `/proc/self/cgroup` (the
/// `N:memory:/path` line, or a line whose controller list holds `memory`).
pub fn parse_cgroup_v1_memory_path(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut it = l.splitn(3, ':');
        let (_id, controllers, path) = (it.next()?, it.next()?, it.next()?);
        controllers
            .split(',')
            .any(|c| c == "memory")
            .then(|| path.trim().to_string())
    })
}

/// A cgroup v1 `memory.limit_in_bytes`. "No limit" is the largest page-aligned
/// `i64` (9223372036854771712 on 4 KiB pages): anything at or above 2^62
/// bytes is read as no limit (`None`).
pub fn parse_limit_in_bytes(text: &str) -> Option<u64> {
    text.trim().parse().ok().filter(|&n: &u64| n < 1 << 62)
}

/// A cgroup v2 `memory.max`: a byte count, or `max` (no limit: `None`).
pub fn parse_memory_max(text: &str) -> Option<u64> {
    let t = text.trim();
    if t == "max" {
        None
    } else {
        t.parse().ok()
    }
}

/// A `queue/rotational` file: `1` rotational, `0` not.
pub fn parse_rotational(text: &str) -> Option<bool> {
    match text.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// The smallest `memory.max` on the way from the cgroup v2 root to `path`.
pub fn cgroup_limit(sys: &Path, path: &str) -> Option<u64> {
    walk_limit(&sys.join("fs/cgroup"), path, "memory.max", parse_memory_max)
}

/// The smallest `memory.limit_in_bytes` on the way from the cgroup v1 memory
/// controller's root to `path`. In a container the mount shows the
/// container's own cgroup as the root, so the root's file holds its limit
/// and the host path below it does not exist.
pub fn cgroup_v1_limit(sys: &Path, path: &str) -> Option<u64> {
    walk_limit(
        &sys.join("fs/cgroup/memory"),
        path,
        "memory.limit_in_bytes",
        parse_limit_in_bytes,
    )
}

fn walk_limit(base: &Path, path: &str, file: &str, parse: fn(&str) -> Option<u64>) -> Option<u64> {
    let read = |dir: &Path| {
        std::fs::read_to_string(dir.join(file))
            .ok()
            .and_then(|t| parse(&t))
    };
    let mut dir = base.to_path_buf();
    let mut best = read(&dir);
    for part in path.split('/').filter(|p| !p.is_empty() && *p != "..") {
        dir.push(part);
        best = min_known(best, read(&dir));
    }
    best
}

fn min_known(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Usable RAM: the smaller of `MemTotal` and the cgroup limit (v2, or v1 on
/// a v1 or hybrid host), and which.
pub fn usable_ram(roots: &Roots) -> (Option<u64>, &'static str) {
    let total = std::fs::read_to_string(roots.proc.join("meminfo"))
        .ok()
        .and_then(|t| parse_meminfo(&t));
    let cgroup = std::fs::read_to_string(roots.proc.join("self/cgroup")).ok();
    let v2 = cgroup
        .as_deref()
        .and_then(parse_cgroup_path)
        .and_then(|p| cgroup_limit(&roots.sys, &p));
    let v1 = cgroup
        .as_deref()
        .and_then(parse_cgroup_v1_memory_path)
        .and_then(|p| cgroup_v1_limit(&roots.sys, &p));
    let limit = min_known(v2, v1);
    match (total, limit) {
        (Some(t), Some(l)) if l < t => (Some(l), "cgroup memory limit"),
        (Some(t), _) => (Some(t), "MemTotal"),
        (None, Some(l)) => (Some(l), "cgroup memory limit"),
        (None, None) => (None, "unreadable"),
    }
}

/// The kind of block device `major:minor`, from `sys`: its own
/// `queue/rotational`, or its disk's for a partition (a partition has no
/// `queue/`); a device-mapper or md device (LVM, dm-crypt, RAID) is judged by
/// the disks under its `slaves/` (NVMe only if all of them are, rotational if
/// any is); otherwise a non-rotational disk named `nvme*` is NVMe. Major 0
/// (overlay, tmpfs, btrfs subvolumes, NFS) has no block device: `None`.
pub fn disk_kind(sys: &Path, major: u32, minor: u32) -> Option<DiskKind> {
    if major == 0 {
        return None;
    }
    let dev = std::fs::canonicalize(sys.join(format!("dev/block/{major}:{minor}"))).ok()?;
    kind_of(&dev, 0)
}

fn kind_of(dev: &Path, depth: u32) -> Option<DiskKind> {
    // dm over md over a partition is three levels; a loop in a broken /sys
    // must not recurse for ever.
    if depth > 4 {
        return None;
    }
    let disk = if dev.join("queue").is_dir() {
        dev.to_path_buf()
    } else {
        dev.parent()?.to_path_buf()
    };
    let below: Vec<PathBuf> = std::fs::read_dir(disk.join("slaves"))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter_map(|e| std::fs::canonicalize(e.path()).ok())
                .collect()
        })
        .unwrap_or_default();
    if !below.is_empty() {
        let kinds: Vec<Option<DiskKind>> = below.iter().map(|b| kind_of(b, depth + 1)).collect();
        return if kinds.contains(&Some(DiskKind::Hdd)) {
            Some(DiskKind::Hdd)
        } else if kinds.iter().all(|k| *k == Some(DiskKind::Nvme)) {
            Some(DiskKind::Nvme)
        } else if kinds.iter().all(Option::is_some) {
            Some(DiskKind::Ssd)
        } else {
            None
        };
    }
    let rotational = std::fs::read_to_string(disk.join("queue/rotational"))
        .ok()
        .and_then(|t| parse_rotational(&t))?;
    let nvme = disk
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("nvme"));
    Some(match (rotational, nvme) {
        (true, _) => DiskKind::Hdd,
        (false, true) => DiskKind::Nvme,
        (false, false) => DiskKind::Ssd,
    })
}

/// Read every fact for `data_dir`.
pub fn gather(data_dir: &Path, roots: &Roots) -> Facts {
    let cpus = std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as u64);
    let (ram_bytes, ram_from) = usable_ram(roots);
    let (disk_bytes, free_bytes) = space(data_dir);
    Facts {
        cpus,
        ram_bytes,
        ram_from,
        disk_bytes,
        free_bytes,
        disk: device(data_dir).and_then(|(ma, mi)| disk_kind(&roots.sys, ma, mi)),
        not_linux: !cfg!(target_os = "linux"),
    }
}

#[cfg(unix)]
fn space(dir: &Path) -> (Option<u64>, Option<u64>) {
    match rustix::fs::statvfs(dir) {
        Ok(s) => (
            s.f_blocks.checked_mul(s.f_frsize),
            s.f_bavail.checked_mul(s.f_frsize),
        ),
        Err(_) => (None, None),
    }
}

#[cfg(not(unix))]
fn space(_dir: &Path) -> (Option<u64>, Option<u64>) {
    (None, None)
}

#[cfg(unix)]
fn device(dir: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::metadata(dir).ok()?.dev();
    Some((rustix::fs::major(dev), rustix::fs::minor(dev)))
}

#[cfg(not(unix))]
fn device(_dir: &Path) -> Option<(u32, u32)> {
    None
}

/// A check's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Meets the recommended value.
    Pass,
    /// No measured value for this check yet, or not read on this OS.
    Skip,
    /// Between the minimum and the recommended value, or unreadable.
    Warn,
    /// Below the minimum.
    Fail,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Skip => "SKIP",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

/// One printed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The result.
    pub status: Status,
    /// `cpu`, `ram`, `disk size`, `free disk` or `disk type`.
    pub what: &'static str,
    /// Why, with the numbers.
    pub why: String,
}

impl std::fmt::Display for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status.word(), self.what, self.why)
    }
}

/// `value` against `min` and `rec` (same unit). At the minimum is enough;
/// at the recommended value is a pass.
pub fn grade(value: u64, min: Option<u64>, rec: Option<u64>) -> Status {
    match (min, rec) {
        (None, None) => Status::Skip,
        (Some(m), _) if value < m => Status::Fail,
        (_, Some(r)) if value < r => Status::Warn,
        _ => Status::Pass,
    }
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `n` with its unit; the CPU unit takes a plural.
fn amount(n: u64, unit: &str) -> String {
    if unit == "CPU" && n != 1 {
        format!("{} CPUs", group(n))
    } else {
        format!("{} {unit}", group(n))
    }
}

fn limits(min: Option<u64>, rec: Option<u64>, unit: &str) -> String {
    match (min, rec) {
        (Some(m), Some(r)) => format!(
            "minimum {}, recommended {}",
            amount(m, unit),
            amount(r, unit)
        ),
        (Some(m), None) => format!("minimum {}", amount(m, unit)),
        (None, Some(r)) => format!("recommended {}, no measured minimum", amount(r, unit)),
        (None, None) => "not measured yet".to_string(),
    }
}

const NOT_READ_HERE: &str =
    "not read on this OS (the doctor reads /proc and /sys, which are Linux only)";

fn numeric(
    what: &'static str,
    value: Option<u64>,
    shown: &str,
    min: Option<u64>,
    rec: Option<u64>,
    unit: &str,
    not_linux: bool,
) -> Line {
    let status = match value {
        None if not_linux || (min.is_none() && rec.is_none()) => Status::Skip,
        None => Status::Warn,
        Some(v) => grade(v, min, rec),
    };
    let why = match (status, value) {
        (Status::Skip, None) if not_linux => format!("{NOT_READ_HERE} ({})", limits(min, rec, unit)),
        (Status::Skip, _) => format!("{shown}; no requirement measured for this role yet"),
        (_, None) => format!("could not be read ({})", limits(min, rec, unit)),
        (Status::Fail, _) => format!("{shown} is below the {}", limits(min, rec, unit)),
        (Status::Warn, _) if min.is_some() => format!(
            "{shown} meets the minimum but is below the recommended ({}); it may run slower, it is not measured below that",
            limits(min, rec, unit)
        ),
        (Status::Warn, _) => format!(
            "{shown} is below the recommended ({}); nothing is measured below that",
            limits(min, rec, unit)
        ),
        _ => format!("{shown} ({})", limits(min, rec, unit)),
    };
    Line { status, what, why }
}

/// Compare `facts` with `need` for `role`: one line per check.
pub fn check(role: Role, need: &Need, facts: &Facts) -> Vec<Line> {
    let shown = |v: Option<u64>, unit_bytes: u64, unit: &str| match v {
        Some(b) => format!("{} {unit}", group(b / unit_bytes)),
        None => "unreadable".to_string(),
    };
    let mut out = vec![
        numeric(
            "cpu",
            facts.cpus,
            &match facts.cpus {
                Some(1) => "1 logical CPU".to_string(),
                Some(n) => format!("{n} logical CPUs"),
                None => "unreadable".to_string(),
            },
            need.cpus_min,
            need.cpus_rec,
            "CPU",
            facts.not_linux,
        ),
        numeric(
            "ram",
            facts.ram_bytes.map(|b| b / MIB),
            &match facts.ram_bytes {
                Some(_) => format!(
                    "{} usable ({})",
                    shown(facts.ram_bytes, MIB, "MiB"),
                    facts.ram_from
                ),
                None => "unreadable".to_string(),
            },
            need.ram_min_mib,
            need.ram_rec_mib,
            "MiB",
            facts.not_linux,
        ),
        numeric(
            "disk size",
            facts.disk_bytes.map(|b| b / GIB),
            &format!("{} filesystem", shown(facts.disk_bytes, GIB, "GiB")),
            need.disk_min_gib,
            need.disk_rec_gib,
            "GiB",
            facts.not_linux,
        ),
        numeric(
            "free disk",
            facts.free_bytes.map(|b| b / GIB),
            &format!("{} free", shown(facts.free_bytes, GIB, "GiB")),
            need.free_min_gib,
            need.free_rec_gib,
            "GiB",
            facts.not_linux,
        ),
    ];
    let kind = match facts.disk {
        Some(DiskKind::Nvme) => "NVMe",
        Some(DiskKind::Ssd) => "non-rotational, not NVMe (a SATA or SAS SSD, eMMC, or a mapped device over mixed disks)",
        Some(DiskKind::Hdd) => "rotational (an HDD, or a virtual disk that reports itself rotational)",
        None if facts.not_linux => NOT_READ_HERE,
        None => "unknown (no block device under /sys for this directory: overlay, tmpfs, NFS, btrfs)",
    };
    out.push(match (need.disk_type, facts.disk) {
        (DiskType::Any, _) => Line {
            status: Status::Pass,
            what: "disk type",
            why: format!("{kind}; any local disk serves the {} role", role.name()),
        },
        (DiskType::NvmeRecommended, Some(DiskKind::Nvme)) => Line {
            status: Status::Pass,
            what: "disk type",
            why: format!("{kind} (recommended: NVMe)"),
        },
        (DiskType::NvmeRecommended, None) if facts.not_linux => Line {
            status: Status::Skip,
            what: "disk type",
            why: format!("{kind} (recommended: NVMe)"),
        },
        (DiskType::NvmeRecommended, _) => Line {
            status: Status::Warn,
            what: "disk type",
            why: format!("{kind}: NVMe is recommended; every measured machine used NVMe and nothing is measured on other disks (Bitcoin Core asks 100 MB/s read/write)"),
        },
    });
    out
}

/// The `node` row on `state_dir` at a node start: every line, and whether
/// the start must refuse ([`NODE_REFUSE_FREE_BYTES`]). A state directory the
/// node has not created yet is read on its nearest existing parent (the
/// filesystem it will be created on).
pub fn preflight_node(state_dir: &Path, roots: &Roots) -> (Vec<Line>, bool) {
    let need = need(Role::Node, Chain::Bitcoin).unwrap_or(NODE_FALLBACK);
    let facts = gather(existing_dir(state_dir), roots);
    let refuse = refuses(facts.free_bytes);
    (check(Role::Node, &need, &facts), refuse)
}

/// `dir`, or its nearest parent that is a directory (`.` for a relative path
/// with none).
pub fn existing_dir(dir: &Path) -> &Path {
    dir.ancestors()
        .map(|a| {
            if a.as_os_str().is_empty() {
                Path::new(".")
            } else {
                a
            }
        })
        .find(|a| a.is_dir())
        .unwrap_or(dir)
}

/// Whether a node start refuses on `free` bytes: only below
/// [`NODE_REFUSE_FREE_BYTES`], never on an unreadable value.
pub fn refuses(free: Option<u64>) -> bool {
    free.is_some_and(|f| f < NODE_REFUSE_FREE_BYTES)
}

const NODE_FALLBACK: Need = Need {
    cpus_min: None,
    cpus_rec: None,
    ram_min_mib: None,
    ram_rec_mib: None,
    disk_min_gib: None,
    disk_rec_gib: None,
    free_min_gib: None,
    free_rec_gib: None,
    disk_type: DiskType::Any,
};

/// The parsed command line of `onyx-node doctor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    /// `--role` (default `contributor`).
    pub role: Role,
    /// `--chain` (default `bitcoin`).
    pub chain: Chain,
    /// `--data-dir` (default: `ONYX_STATE_DIR` for `node`, else `.`).
    pub data_dir: PathBuf,
}

/// Usage text.
pub const USAGE: &str = "usage: onyx-node doctor [--role contributor|validator|indexer|node|coordinator] [--chain bitcoin|ethereum] [--data-dir PATH]\n\
thresholds and their sources: docs/ops/hardware.md; exit 0 with no FAIL, 1 with a FAIL, 2 on a usage error";

/// Parse the arguments after `doctor`. `state_dir` is `ONYX_STATE_DIR`.
pub fn parse_args(args: &[String], state_dir: Option<PathBuf>) -> Result<Args, String> {
    let mut role = Role::Contributor;
    let mut chain = Chain::Bitcoin;
    let mut dir: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--role" => {
                let v = value()?;
                role = Role::parse(&v).ok_or(format!("unknown role {v:?}"))?;
            }
            "--chain" => {
                let v = value()?;
                chain = Chain::parse(&v).ok_or(format!("unknown chain {v:?}"))?;
            }
            "--data-dir" => dir = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let data_dir = dir.unwrap_or_else(|| match (role, state_dir) {
        (Role::Node, Some(s)) => s,
        _ => PathBuf::from("."),
    });
    Ok(Args {
        role,
        chain,
        data_dir,
    })
}

/// `onyx-node doctor ...`: print the lines; 0 without a FAIL, 1 with one,
/// 2 on a usage error.
pub fn main(args: &[String]) -> std::process::ExitCode {
    use std::process::ExitCode;
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let state_dir = std::env::var_os("ONYX_STATE_DIR").map(PathBuf::from);
    let a = match parse_args(args, state_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("onyx-node doctor: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let need = match need(a.role, a.chain) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("onyx-node doctor: {e}");
            return ExitCode::from(2);
        }
    };
    if !a.data_dir.is_dir() {
        eprintln!(
            "onyx-node doctor: --data-dir {} is not a directory",
            a.data_dir.display()
        );
        return ExitCode::from(2);
    }
    let facts = gather(&a.data_dir, &Roots::default());
    println!(
        "onyx-node doctor: role {}, data dir {} (thresholds: docs/ops/hardware.md)",
        a.role.name(),
        a.data_dir.display()
    );
    let lines = check(a.role, &need, &facts);
    for l in &lines {
        println!("{l}");
    }
    ExitCode::from(exit_code(&lines))
}

/// The doctor's exit code for its lines: 1 with a `FAIL`, else 0 (a `WARN`
/// or a `SKIP` is not a failure).
pub fn exit_code(lines: &[Line]) -> u8 {
    u8::from(lines.iter().any(|l| l.status == Status::Fail))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str =
        "MemTotal:       63659732 kB\nMemFree:         5242880 kB\nMemAvailable:   41943040 kB\n";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("onyx-doctor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn meminfo_mem_total_is_read_in_bytes() {
        assert_eq!(parse_meminfo(MEMINFO), Some(63_659_732 * 1024));
        assert_eq!(parse_meminfo("MemFree: 1 kB\n"), None);
        assert_eq!(parse_meminfo("MemTotal: 12\n"), None, "a unit is required");
        assert_eq!(parse_meminfo("MemTotal: x kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
    }

    #[test]
    fn cgroup_files_parse() {
        let t = "0::/user.slice/user-1000.slice/run-x.scope\n";
        assert_eq!(
            parse_cgroup_path(t).as_deref(),
            Some("/user.slice/user-1000.slice/run-x.scope")
        );
        assert_eq!(
            parse_cgroup_path("12:memory:/docker/abc\n"),
            None,
            "v1 only"
        );
        assert_eq!(parse_memory_max("max\n"), None);
        assert_eq!(parse_memory_max("2147483648\n"), Some(2 * GIB));
        assert_eq!(parse_memory_max("junk"), None);
    }

    #[test]
    fn rotational_files_parse() {
        assert_eq!(parse_rotational("1\n"), Some(true));
        assert_eq!(parse_rotational("0\n"), Some(false));
        assert_eq!(parse_rotational("2"), None);
        assert_eq!(parse_rotational(""), None);
    }

    #[test]
    fn the_smallest_cgroup_limit_on_the_path_wins() {
        let d = tmp("cg");
        let sys = d.join("sys");
        write(&sys.join("fs/cgroup/a/memory.max"), "max\n");
        write(&sys.join("fs/cgroup/a/b/memory.max"), "8589934592\n");
        write(&sys.join("fs/cgroup/a/b/c/memory.max"), "12884901888\n");
        assert_eq!(cgroup_limit(&sys, "/a/b/c"), Some(8 * GIB));
        // A `..` never climbs out of the hierarchy: it is skipped.
        write(&d.join("memory.max"), "1\n");
        assert_eq!(cgroup_limit(&sys, "/../../a/b"), Some(8 * GIB));
        assert_eq!(cgroup_limit(&sys, "/a"), None, "max is no limit");
        assert_eq!(cgroup_limit(&sys, "/"), None);
        // A container sees its own cgroup as the root.
        write(&sys.join("fs/cgroup/memory.max"), "1073741824\n");
        assert_eq!(cgroup_limit(&sys, "/"), Some(GIB));
        assert_eq!(cgroup_limit(&sys, "/a/b/c"), Some(GIB));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn usable_ram_is_the_smaller_of_mem_total_and_the_cgroup() {
        let d = tmp("ram");
        let roots = Roots {
            proc: d.join("proc"),
            sys: d.join("sys"),
        };
        write(&roots.proc.join("meminfo"), MEMINFO);
        assert_eq!(usable_ram(&roots), (Some(63_659_732 * 1024), "MemTotal"));
        write(&roots.proc.join("self/cgroup"), "0::/svc\n");
        write(&roots.sys.join("fs/cgroup/svc/memory.max"), "2147483648\n");
        assert_eq!(usable_ram(&roots), (Some(2 * GIB), "cgroup memory limit"));
        write(
            &roots.sys.join("fs/cgroup/svc/memory.max"),
            &format!("{}\n", 63_659_732u64 * 1024),
        );
        assert_eq!(
            usable_ram(&roots),
            (Some(63_659_732 * 1024), "MemTotal"),
            "a limit equal to MemTotal is no cap"
        );
        write(
            &roots.sys.join("fs/cgroup/svc/memory.max"),
            "999999999999999\n",
        );
        assert_eq!(usable_ram(&roots), (Some(63_659_732 * 1024), "MemTotal"));
        std::fs::remove_file(roots.proc.join("meminfo")).expect("rm");
        write(&roots.sys.join("fs/cgroup/svc/memory.max"), "2147483648\n");
        assert_eq!(usable_ram(&roots), (Some(2 * GIB), "cgroup memory limit"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn disk_kinds_nvme_sata_ssd_hdd_partitions_and_mapped_devices() {
        let d = tmp("sys");
        let sys = d.join("sys");
        // As sysfs lays it out: /sys/dev/block/M:m links into /sys/devices; a
        // partition is a directory inside its disk's, without a queue/.
        let disk = |path: &str, rot: &str| {
            let p = sys.join("devices").join(path);
            write(&p.join("queue/rotational"), rot);
            p
        };
        let part = |p: &Path, name: &str| {
            let q = p.join(name);
            std::fs::create_dir_all(&q).expect("mkdir");
            q
        };
        std::fs::create_dir_all(sys.join("dev/block")).expect("mkdir");
        let link = |target: &Path, name: &str| {
            std::os::unix::fs::symlink(target, sys.join("dev/block").join(name)).expect("link")
        };
        let slave = |dm: &Path, target: &Path, name: &str| {
            std::fs::create_dir_all(dm.join("slaves")).expect("mkdir");
            std::os::unix::fs::symlink(target, dm.join("slaves").join(name)).expect("link")
        };
        let nvme = disk("pci0/nvme/nvme0/nvme0n1", "0\n");
        let nvme_p2 = part(&nvme, "nvme0n1p2");
        let hdd = disk("ata1/sda", "1\n");
        let hdd_p1 = part(&hdd, "sda1");
        let sata = disk("ata2/sdb", "0\n");
        let sata_p1 = part(&sata, "sdb1");
        let mmc = disk("mmc0/mmcblk0", "0\n");
        link(&nvme, "259:0");
        link(&nvme_p2, "259:2");
        link(&hdd, "8:0");
        link(&hdd_p1, "8:1");
        link(&sata, "8:16");
        link(&sata_p1, "8:17");
        link(&mmc, "179:0");
        // dm-crypt on the NVMe partition (dm reports its own flag; the disks below decide).
        let dm0 = disk("virtual/block/dm-0", "1\n");
        slave(&dm0, &nvme_p2, "nvme0n1p2");
        link(&dm0, "253:0");
        // LVM over the HDD and the SATA SSD: rotational if any disk below is.
        let dm1 = disk("virtual/block/dm-1", "0\n");
        slave(&dm1, &hdd_p1, "sda1");
        slave(&dm1, &sata_p1, "sdb1");
        link(&dm1, "253:1");
        // md over the NVMe and the SATA SSD: not all NVMe.
        let md0 = disk("virtual/block/md0", "0\n");
        slave(&md0, &nvme_p2, "nvme0n1p2");
        slave(&md0, &sata_p1, "sdb1");
        link(&md0, "9:0");
        // dm over dm-0 (two levels, all NVMe).
        let dm2 = disk("virtual/block/dm-2", "0\n");
        slave(&dm2, &dm0, "dm-0");
        link(&dm2, "253:2");
        // A slave whose disk has no rotational file: unknown, never a guess.
        let odd = sys.join("devices/odd/xda");
        std::fs::create_dir_all(odd.join("queue")).expect("mkdir");
        let dm3 = disk("virtual/block/dm-3", "0\n");
        slave(&dm3, &odd, "xda");
        link(&dm3, "253:3");
        let k = |ma, mi| disk_kind(&sys, ma, mi);
        assert_eq!(k(259, 0), Some(DiskKind::Nvme));
        assert_eq!(
            k(259, 2),
            Some(DiskKind::Nvme),
            "a partition reads its disk"
        );
        assert_eq!(k(8, 0), Some(DiskKind::Hdd));
        assert_eq!(k(8, 1), Some(DiskKind::Hdd));
        assert_eq!(k(8, 16), Some(DiskKind::Ssd), "a SATA SSD is not NVMe");
        assert_eq!(k(8, 17), Some(DiskKind::Ssd));
        assert_eq!(k(179, 0), Some(DiskKind::Ssd), "eMMC");
        assert_eq!(k(253, 0), Some(DiskKind::Nvme), "dm-crypt over NVMe");
        assert_eq!(k(253, 1), Some(DiskKind::Hdd), "LVM with an HDD below");
        assert_eq!(k(9, 0), Some(DiskKind::Ssd), "md over NVMe and SATA");
        assert_eq!(k(253, 2), Some(DiskKind::Nvme), "dm over dm over NVMe");
        // Four mapped levels over the partition are still read (the limit is
        // for loops, not for real stacks).
        let mut below = dm0.clone();
        for (i, name) in ["dm-4", "dm-5", "dm-6"].into_iter().enumerate() {
            let up = disk(&format!("virtual/block/{name}"), "0\n");
            slave(&up, &below, &format!("x{i}"));
            below = up;
        }
        link(&below, "253:6");
        assert_eq!(
            k(253, 6),
            Some(DiskKind::Nvme),
            "dm-6 > dm-5 > dm-4 > dm-0 > nvme0n1p2"
        );
        assert_eq!(k(253, 3), None, "unknown below: unknown");
        assert_eq!(k(8, 9), None, "no such device");
        assert_eq!(k(0, 51), None, "overlay, tmpfs: no block device");
        // A loop of slaves (a broken /sys) ends instead of recursing for ever.
        let a = disk("virtual/block/dm-8", "0\n");
        let b = disk("virtual/block/dm-9", "0\n");
        slave(&a, &b, "dm-9");
        slave(&b, &a, "dm-8");
        link(&a, "253:8");
        assert_eq!(k(253, 8), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cgroup_v1_files_parse() {
        let t = "12:memory:/docker/abc\n11:cpu,cpuacct:/docker/abc\n0::/\n";
        assert_eq!(
            parse_cgroup_v1_memory_path(t).as_deref(),
            Some("/docker/abc")
        );
        assert_eq!(
            parse_cgroup_v1_memory_path("4:cpuset,memory:/a\n").as_deref(),
            Some("/a"),
            "a joined controller list"
        );
        assert_eq!(
            parse_cgroup_v1_memory_path("0::/user.slice\n"),
            None,
            "v2 only"
        );
        assert_eq!(parse_cgroup_v1_memory_path("3:memoryx:/a\n"), None);
        assert_eq!(parse_limit_in_bytes("2147483648\n"), Some(2 * GIB));
        assert_eq!(
            parse_limit_in_bytes("9223372036854771712\n"),
            None,
            "unlimited"
        );
        assert_eq!(
            parse_limit_in_bytes(&((1u64 << 62) - 1).to_string()),
            Some((1 << 62) - 1)
        );
        assert_eq!(parse_limit_in_bytes(&(1u64 << 62).to_string()), None);
        assert_eq!(parse_limit_in_bytes("max"), None);
    }

    #[test]
    fn usable_ram_reads_cgroup_v1_and_hybrid_hosts() {
        let d = tmp("ram1");
        let roots = Roots {
            proc: d.join("proc"),
            sys: d.join("sys"),
        };
        write(&roots.proc.join("meminfo"), MEMINFO);
        // A v1 container: the mount's root is the container's own cgroup, the
        // host path below it does not exist.
        write(&roots.proc.join("self/cgroup"), "9:memory:/docker/abc\n");
        write(
            &roots.sys.join("fs/cgroup/memory/memory.limit_in_bytes"),
            "1073741824\n",
        );
        assert_eq!(usable_ram(&roots), (Some(GIB), "cgroup memory limit"));
        // A v1 host with no limit.
        write(
            &roots.sys.join("fs/cgroup/memory/memory.limit_in_bytes"),
            "9223372036854771712\n",
        );
        assert_eq!(usable_ram(&roots), (Some(63_659_732 * 1024), "MemTotal"));
        // Hybrid: a v2 line with no memory controller and a v1 memory line.
        write(&roots.proc.join("self/cgroup"), "9:memory:/svc\n0::/svc\n");
        write(
            &roots.sys.join("fs/cgroup/memory/svc/memory.limit_in_bytes"),
            "3221225472\n",
        );
        assert_eq!(usable_ram(&roots), (Some(3 * GIB), "cgroup memory limit"));
        // Both set: the smaller wins.
        write(&roots.sys.join("fs/cgroup/svc/memory.max"), "2147483648\n");
        assert_eq!(usable_ram(&roots), (Some(2 * GIB), "cgroup memory limit"));
        // Nothing readable at all.
        let empty = Roots {
            proc: d.join("none"),
            sys: d.join("none"),
        };
        assert_eq!(usable_ram(&empty), (None, "unreadable"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn off_linux_an_unread_value_is_a_skip_not_a_warn() {
        let n = need(Role::Contributor, Chain::Bitcoin).expect("row");
        let mut f = facts(12, 60 * 1024, 3_000, 6, None);
        f.ram_bytes = None;
        f.not_linux = true;
        let l = check(Role::Contributor, &n, &f);
        assert_eq!(
            statuses(&l),
            vec![
                ("cpu", Status::Pass),
                ("ram", Status::Skip),
                ("disk size", Status::Pass),
                ("free disk", Status::Pass),
                ("disk type", Status::Skip)
            ]
        );
        assert!(l[1].why.starts_with("not read on this OS"), "{}", l[1].why);
        assert!(l[4].why.starts_with("not read on this OS"), "{}", l[4].why);
        // A value that was read is still graded off Linux.
        f.cpus = Some(1);
        assert_eq!(check(Role::Contributor, &n, &f)[0].status, Status::Fail);
        // On Linux the same unread values warn.
        f.not_linux = false;
        let l = check(Role::Contributor, &n, &f);
        assert_eq!((l[1].status, l[4].status), (Status::Warn, Status::Warn));
        assert!(!Facts::default().not_linux);
        assert_eq!(
            gather(Path::new("."), &Roots::default()).not_linux,
            !cfg!(target_os = "linux")
        );
    }

    #[test]
    fn grade_edges() {
        assert_eq!(grade(1, Some(2), Some(12)), Status::Fail);
        assert_eq!(
            grade(2, Some(2), Some(12)),
            Status::Warn,
            "the minimum is enough"
        );
        assert_eq!(grade(11, Some(2), Some(12)), Status::Warn);
        assert_eq!(
            grade(12, Some(2), Some(12)),
            Status::Pass,
            "at the recommended value"
        );
        assert_eq!(grade(0, Some(1), None), Status::Fail);
        assert_eq!(grade(1, Some(1), None), Status::Pass);
        assert_eq!(
            grade(0, None, Some(24)),
            Status::Warn,
            "no minimum: never a fail"
        );
        assert_eq!(grade(24, None, Some(24)), Status::Pass);
        assert_eq!(grade(u64::MAX, None, None), Status::Skip);
    }

    fn facts(
        cpus: u64,
        ram_mib: u64,
        disk_gib: u64,
        free_gib: u64,
        disk: Option<DiskKind>,
    ) -> Facts {
        Facts {
            cpus: Some(cpus),
            ram_bytes: Some(ram_mib * MIB),
            ram_from: "MemTotal",
            disk_bytes: Some(disk_gib * GIB),
            free_bytes: Some(free_gib * GIB),
            disk,
            not_linux: false,
        }
    }

    fn statuses(lines: &[Line]) -> Vec<(&'static str, Status)> {
        lines.iter().map(|l| (l.what, l.status)).collect()
    }

    #[test]
    fn contributor_threshold_edges() {
        let n = need(Role::Contributor, Chain::Bitcoin).expect("row");
        let at_min = check(
            Role::Contributor,
            &n,
            &facts(2, 6 * 1024, 836, 1, Some(DiskKind::Nvme)),
        );
        assert_eq!(
            statuses(&at_min),
            vec![
                ("cpu", Status::Warn),
                ("ram", Status::Warn),
                ("disk size", Status::Pass),
                ("free disk", Status::Warn),
                ("disk type", Status::Pass)
            ]
        );
        let below = check(
            Role::Contributor,
            &n,
            &facts(1, 6 * 1024 - 1, 835, 0, Some(DiskKind::Hdd)),
        );
        assert_eq!(
            statuses(&below),
            vec![
                ("cpu", Status::Fail),
                ("ram", Status::Fail),
                ("disk size", Status::Fail),
                ("free disk", Status::Fail),
                ("disk type", Status::Warn)
            ]
        );
        assert!(below[1]
            .why
            .contains("6,143 MiB usable (MemTotal) is below the minimum 6,144 MiB"));
        assert_eq!(
            at_min[0].why,
            "2 logical CPUs meets the minimum but is below the recommended (minimum 2 CPUs, recommended 12 CPUs); it may run slower, it is not measured below that"
        );
        assert!(
            below[0]
                .why
                .contains("1 logical CPU is below the minimum 2 CPUs, recommended 12 CPUs"),
            "{}",
            below[0].why
        );
        let rec = check(
            Role::Contributor,
            &n,
            &facts(12, 60 * 1024, 3_000, 6, Some(DiskKind::Nvme)),
        );
        assert!(rec.iter().all(|l| l.status == Status::Pass), "{rec:?}");
        // One below the recommended value warns, check by check.
        for (i, f) in [
            facts(11, 60 * 1024, 3_000, 6, Some(DiskKind::Nvme)),
            facts(12, 60 * 1024 - 1, 3_000, 6, Some(DiskKind::Nvme)),
            facts(12, 60 * 1024, 3_000, 5, Some(DiskKind::Nvme)),
            facts(12, 60 * 1024, 3_000, 6, Some(DiskKind::Ssd)),
            facts(12, 60 * 1024, 3_000, 6, None),
        ]
        .iter()
        .enumerate()
        {
            let l = check(Role::Contributor, &n, f);
            let warned: Vec<_> = l.iter().filter(|l| l.status != Status::Pass).collect();
            assert_eq!(warned.len(), 1, "{l:?}");
            assert_eq!(warned[0].status, Status::Warn, "{l:?}");
            assert_eq!(
                warned[0].what,
                ["cpu", "ram", "free disk", "disk type", "disk type"][i]
            );
        }
        let sata = check(
            Role::Contributor,
            &n,
            &facts(12, 60 * 1024, 3_000, 6, Some(DiskKind::Ssd)),
        );
        assert!(
            sata[4].why.contains("not NVMe") && sata[4].why.contains("NVMe is recommended"),
            "{}",
            sata[4].why
        );
    }

    #[test]
    fn node_threshold_edges_and_unknowns() {
        let n = need(Role::Node, Chain::Bitcoin).expect("row");
        let ok = check(Role::Node, &n, &facts(1, 2 * 1024, 1, 1, None));
        assert!(ok[0].why.contains("(minimum 1 CPU)"), "{}", ok[0].why);
        assert!(
            ok[1]
                .why
                .contains("(recommended 2,048 MiB, no measured minimum)"),
            "{}",
            ok[1].why
        );
        assert_eq!(
            statuses(&ok),
            vec![
                ("cpu", Status::Pass),
                ("ram", Status::Pass),
                ("disk size", Status::Skip),
                ("free disk", Status::Pass),
                ("disk type", Status::Pass)
            ]
        );
        let small = check(
            Role::Node,
            &n,
            &facts(1, 2 * 1024 - 1, 1, 0, Some(DiskKind::Hdd)),
        );
        // The 2 GiB is the shipped cap (measured use 25.2 MiB): below it warns, never fails.
        assert_eq!(small[1].status, Status::Warn);
        assert_eq!(
            check(Role::Node, &n, &facts(1, 1, 1, 1, None))[1].status,
            Status::Warn
        );
        assert_eq!(small[3].status, Status::Fail);
        assert_eq!(small[4].status, Status::Pass, "any disk serves the API");
        assert_eq!(
            check(Role::Node, &n, &facts(0, 2 * 1024, 1, 1, None))[0].status,
            Status::Fail
        );
        let unreadable = check(Role::Node, &n, &Facts::default());
        assert_eq!(unreadable[0].status, Status::Warn);
        assert!(unreadable[0].why.starts_with("could not be read"));
        assert_eq!(unreadable[2].status, Status::Skip);
    }

    #[test]
    fn indexer_and_coordinator_never_fail_where_nothing_is_measured() {
        let n = need(Role::Indexer, Chain::Bitcoin).expect("row");
        let l = check(Role::Indexer, &n, &facts(1, 1, 2_033, 0, None));
        assert_eq!(
            l[0].why,
            "1 logical CPU is below the recommended (recommended 24 CPUs, no measured minimum); nothing is measured below that"
        );
        assert_eq!(
            l[3].why,
            "0 GiB free; no requirement measured for this role yet"
        );
        assert!(
            l[4].why.starts_with("unknown (no block device"),
            "{}",
            l[4].why
        );
        assert_eq!(
            statuses(&l),
            vec![
                ("cpu", Status::Warn),
                ("ram", Status::Warn),
                ("disk size", Status::Pass),
                ("free disk", Status::Skip),
                ("disk type", Status::Warn)
            ]
        );
        assert_eq!(
            check(
                Role::Indexer,
                &n,
                &facts(24, 91 * 1024, 2_032, 0, Some(DiskKind::Nvme))
            )[2]
            .status,
            Status::Fail
        );
        let c = need(Role::Coordinator, Chain::Bitcoin).expect("row");
        let unread = check(Role::Coordinator, &c, &Facts::default());
        assert_eq!(unread[1].status, Status::Skip);
        assert_eq!(
            unread[1].why,
            "unreadable; no requirement measured for this role yet"
        );
        let l = check(
            Role::Coordinator,
            &c,
            &facts(1, 1, 1, 0, Some(DiskKind::Hdd)),
        );
        assert!(l[..4].iter().all(|l| l.status == Status::Skip), "{l:?}");
    }

    #[test]
    fn only_a_fail_exits_1() {
        let line = |status| Line {
            status,
            what: "cpu",
            why: String::new(),
        };
        assert_eq!(exit_code(&[]), 0);
        assert_eq!(
            exit_code(&[line(Status::Pass), line(Status::Warn), line(Status::Skip)]),
            0
        );
        assert_eq!(exit_code(&[line(Status::Pass), line(Status::Fail)]), 1);
        assert_eq!(exit_code(&[line(Status::Fail), line(Status::Pass)]), 1);
    }

    #[test]
    fn ethereum_is_planned_for_chain_roles_only() {
        for r in [Role::Contributor, Role::Validator, Role::Indexer] {
            let e = need(r, Chain::Ethereum).expect_err("planned");
            assert!(e.contains("planned"), "{e}");
        }
        assert!(need(Role::Node, Chain::Ethereum).is_ok());
    }

    #[test]
    fn args_parse() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let a = parse_args(&s(&[]), None).expect("defaults");
        assert_eq!(
            a,
            Args {
                role: Role::Contributor,
                chain: Chain::Bitcoin,
                data_dir: ".".into()
            }
        );
        let a =
            parse_args(&s(&["--role", "node"]), Some("/var/lib/onyx/state".into())).expect("node");
        assert_eq!(a.data_dir, PathBuf::from("/var/lib/onyx/state"));
        let a = parse_args(
            &s(&["--role", "validator", "--data-dir", "/x"]),
            Some("/s".into()),
        )
        .expect("v");
        assert_eq!((a.role, a.data_dir), (Role::Validator, PathBuf::from("/x")));
        assert!(parse_args(&s(&["--role"]), None).is_err());
        assert!(parse_args(&s(&["--role", "buyer"]), None).is_err());
        assert!(parse_args(&s(&["--chain", "solana"]), None).is_err());
        assert_eq!(
            parse_args(&s(&["--chain", "ethereum"]), None).map(|a| a.chain),
            Ok(Chain::Ethereum)
        );
        assert_eq!(
            parse_args(&s(&["--chain", "ethereum", "--chain", "bitcoin"]), None).map(|a| a.chain),
            Ok(Chain::Bitcoin)
        );
        assert!(parse_args(&s(&["--fix"]), None).is_err());
    }

    #[test]
    fn the_node_refuses_only_below_the_measured_state_size() {
        let (lines, refuse) = preflight_node(Path::new("."), &Roots::default());
        assert_eq!(lines.len(), 5);
        // A state directory not created yet is read on its parent's filesystem.
        let d = tmp("pre");
        let later = d.join("state/not/yet");
        assert_eq!(existing_dir(&later), d.as_path());
        assert_eq!(existing_dir(&d), d.as_path());
        assert_eq!(existing_dir(Path::new("no-such-dir-p5/x")), Path::new("."));
        let (lines, refuse_later) = preflight_node(&later, &Roots::default());
        assert!(!refuse_later);
        if cfg!(target_os = "linux") {
            assert_eq!(lines[3].status, Status::Pass, "{:?}", lines[3]);
        }
        assert!(!later.exists(), "the preflight creates nothing");
        let _ = std::fs::remove_dir_all(&d);
        // The test machine has more than half a megabyte free.
        assert!(!refuse);
        assert!(refuses(Some(NODE_REFUSE_FREE_BYTES - 1)));
        assert!(
            !refuses(Some(NODE_REFUSE_FREE_BYTES)),
            "at the measured size: starts"
        );
        assert!(!refuses(None), "an unreadable free space never refuses");
        assert!(refuses(Some(0)));
    }

    #[test]
    fn gather_reads_this_machine_without_root_and_writes_nothing() {
        let d = tmp("ro");
        write(&d.join("blocks/blk00000.dat"), "x");
        let before = snapshot(&d);
        let f = gather(&d, &Roots::default());
        assert!(f.cpus.is_some_and(|n| n >= 1));
        if cfg!(target_os = "linux") {
            assert!(f.ram_bytes.is_some());
            assert!(f.disk_bytes.is_some() && f.free_bytes.is_some());
            assert!(f.free_bytes <= f.disk_bytes);
        }
        let _ = check(
            Role::Contributor,
            &need(Role::Contributor, Chain::Bitcoin).expect("row"),
            &f,
        );
        assert_eq!(snapshot(&d), before, "the data directory is left as it was");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// On Linux the device numbers match the mount that holds the directory,
    /// read independently from `/proc/self/mountinfo` (field 3, `major:minor`).
    #[cfg(target_os = "linux")]
    #[test]
    fn device_numbers_match_mountinfo() {
        let info = std::fs::read_to_string("/proc/self/mountinfo").expect("mountinfo");
        let d = tmp("dev");
        for dir in [
            d.as_path(),
            Path::new("/proc"),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        ] {
            let real = std::fs::canonicalize(dir).expect("canonicalize");
            // The longest mount point that is a prefix of the path, last mounted wins.
            let mut best: Option<(usize, (u32, u32))> = None;
            for line in info.lines() {
                let f: Vec<&str> = line.split(' ').collect();
                let (Some(mm), Some(point)) = (f.get(2), f.get(4)) else {
                    continue;
                };
                let point = point.replace("\\040", " ");
                if !real.starts_with(&point) {
                    continue;
                }
                let Some((ma, mi)) = mm.split_once(':') else {
                    continue;
                };
                let nums = (ma.parse().expect("major"), mi.parse().expect("minor"));
                if best.is_none_or(|(len, _)| point.len() >= len) {
                    best = Some((point.len(), nums));
                }
            }
            let want = best.expect("a mount holds every path").1;
            assert_eq!(device(dir), Some(want), "{}", dir.display());
        }
        assert_eq!(device(Path::new("/no/such/dir/p5")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Every path under `d` with its length and modification time.
    fn snapshot(d: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        let mut out = Vec::new();
        let mut todo = vec![d.to_path_buf()];
        while let Some(p) = todo.pop() {
            let m = std::fs::metadata(&p).expect("metadata");
            out.push((p.clone(), m.len(), m.modified().expect("mtime")));
            if m.is_dir() {
                for e in std::fs::read_dir(&p).expect("read_dir") {
                    todo.push(e.expect("entry").path());
                }
            }
        }
        out.sort();
        out
    }

    /// The doc's "Doctor thresholds" table is the source of these numbers;
    /// the code must say the same.
    #[test]
    fn the_doc_table_matches_the_code() {
        let doc = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ops/hardware.md"),
        )
        .expect("docs/ops/hardware.md");
        let section = doc
            .split("## Doctor thresholds")
            .nth(1)
            .expect("a '## Doctor thresholds' section");
        let cell = |v: Option<u64>| v.map_or("-".to_string(), group);
        let mut seen = 0;
        for line in section.lines().take_while(|l| !l.starts_with("## ")) {
            let cells: Vec<&str> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            let Some(role) = cells.first().and_then(|c| Role::parse(c.trim_matches('`'))) else {
                continue;
            };
            let n = need(role, Chain::Bitcoin).expect("row");
            let want = vec![
                cell(n.cpus_min),
                cell(n.cpus_rec),
                cell(n.ram_min_mib),
                cell(n.ram_rec_mib),
                cell(n.disk_min_gib),
                cell(n.disk_rec_gib),
                cell(n.free_min_gib),
                cell(n.free_rec_gib),
                match n.disk_type {
                    DiskType::Any => "any".to_string(),
                    DiskType::NvmeRecommended => "NVMe recommended".to_string(),
                },
            ];
            assert_eq!(cells[1..10].to_vec(), want, "row {}", role.name());
            seen += 1;
        }
        assert_eq!(seen, Role::ALL.len(), "one row per role");
    }
}
