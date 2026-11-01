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
#[path = "doctor_tests.rs"]
mod tests;
