//! `onyx-node doctor` tests: memory, cgroup limits and disk kinds read right, the limits per
//! role, and the table in docs/ops/hardware.md matches the code.

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

fn facts(cpus: u64, ram_mib: u64, disk_gib: u64, free_gib: u64, disk: Option<DiskKind>) -> Facts {
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
    let a = parse_args(&s(&["--role", "node"]), Some("/var/lib/onyx/state".into())).expect("node");
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
