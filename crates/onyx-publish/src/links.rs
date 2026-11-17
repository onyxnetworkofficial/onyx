//! Successor links in `catalog-build` (ADR-0059 amendment 6 item 8): `--move
//! OLD=NEW` ends OLD as `--remove` does and writes, in the same catalog, the
//! next version of the publisher's links dataset with one row per pair;
//! `--relink OLD=NEW` and `--unlink OLD` write a row for an OLD that is
//! already a tombstone (a link after the fact, a fix, a retraction);
//! `--links-next-generation` starts the next links generation with only the
//! links that act, and ends the full one. The version is built by `build`
//! from JSON Lines rows, after `--links DIR` (the version the previous
//! catalog lists), into `--links-out DIR2`.

use super::{build, prev_dir_for, read_manifest, write_built, Opts, Res};
use onyx_bulk::catalog::{Catalog, CatalogLeaf};
use onyx_bulk::input::Format;
use onyx_bulk::manifest::{Manifest, NewVersion};
use onyx_bulk::row::{decode_row, present};
use onyx_bulk::shard::check_shard;
use onyx_bulk::successor::{
    generation_id, is_links_id, links_schema, usable, Links, LINKS_MAX_BYTES, MAX_LINK_GENERATIONS,
    MAX_LINK_ROWS, MAX_LINK_SHARDS,
};
use onyx_core::ids::is_dotted_id;
use onyx_core::{DataRights, Redistribution, SourceId};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Shard size the links versions are cut at (one shard per catalog in
/// practice: a catalog adds a few rows).
const LINKS_SHARD_TARGET: u64 = 8 << 20;

/// The links options of one `catalog-build`.
pub(crate) struct LinkOpts {
    links: Option<PathBuf>,
    out: PathBuf,
    moves: Vec<(String, String)>,
    relinks: Vec<(String, String)>,
    unlinks: Vec<String>,
    next_generation: bool,
}

fn ids(o: &Opts, k: &str) -> Vec<String> {
    o.opt(k)
        .map(|r| {
            r.split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn pairs(o: &Opts, k: &str) -> Res<Vec<(String, String)>> {
    ids(o, k)
        .into_iter()
        .map(|p| {
            let (a, b) = p
                .split_once('=')
                .ok_or_else(|| format!("--{k} {p}: write OLD=NEW"))?;
            Ok((a.trim().to_string(), b.trim().to_string()))
        })
        .collect()
}

/// The links options given, if any.
pub(crate) fn link_opts(o: &Opts) -> Res<Option<LinkOpts>> {
    let moves = pairs(o, "move")?;
    let relinks = pairs(o, "relink")?;
    let unlinks = ids(o, "unlink");
    let next_generation = o.flag("links-next-generation");
    if moves.is_empty() && relinks.is_empty() && unlinks.is_empty() && !next_generation {
        if o.opt("links").is_some() || o.opt("links-out").is_some() {
            return Err(
                "--links and --links-out go with --move, --relink, --unlink or --links-next-generation"
                    .into(),
            );
        }
        return Ok(None);
    }
    let mut seen = BTreeSet::new();
    let olds = moves.iter().chain(&relinks).map(|(a, _)| a).chain(&unlinks);
    for old in olds {
        if !seen.insert(old.clone()) {
            return Err(format!(
                "{old} is named twice in --move, --relink and --unlink"
            ));
        }
    }
    for id in moves
        .iter()
        .chain(&relinks)
        .flat_map(|(a, b)| [a, b])
        .chain(&unlinks)
    {
        if is_links_id(id) {
            return Err(format!(
                "{id} is reserved for successor links: it is never moved, linked or a link's target"
            ));
        }
        if !is_dotted_id(id) {
            return Err(format!("{id}: not a dataset id"));
        }
    }
    if let Some((a, _)) = moves.iter().chain(&relinks).find(|(a, b)| a == b) {
        return Err(format!("{a} cannot continue as itself"));
    }
    let out = PathBuf::from(o.opt("links-out").ok_or(
        "--move, --relink, --unlink and --links-next-generation write the next links version into --links-out DIR",
    )?);
    Ok(Some(LinkOpts {
        links: o.opt("links").map(PathBuf::from),
        out,
        moves,
        relinks,
        unlinks,
        next_generation,
    }))
}

impl LinkOpts {
    /// How many datasets `--move` ends.
    pub(crate) fn moves(&self) -> usize {
        self.moves.len()
    }

    /// Remove the links version written (its catalog was not built).
    pub(crate) fn discard(&self) {
        let _ = std::fs::remove_dir_all(&self.out);
    }
}

/// One new links row.
struct NewRow {
    from: String,
    version: u32,
    prev: onyx_core::Hash32,
    to: Option<String>,
}

/// The generation number of links id `id`.
fn generation_of(id: &str) -> Option<u32> {
    (1..=MAX_LINK_GENERATIONS).find(|g| generation_id(*g).as_deref() == Some(id))
}

/// Every shard file of the version in `dir`, by root: its own folder's and
/// those of the folders before it (`manifest.json`'s `prev_dir`), as `upload`
/// steps back.
fn version_files(dir: &Path) -> Res<BTreeMap<onyx_core::Hash32, PathBuf>> {
    let mut out = BTreeMap::new();
    let mut at = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for _ in 0..=MAX_LINK_SHARDS {
        if let Ok(rd) = std::fs::read_dir(at.join("shards")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let root = name
                    .strip_suffix(".bin")
                    .and_then(|h| hex::decode(h).ok())
                    .and_then(|b| <[u8; 32]>::try_from(b).ok())
                    .map(onyx_core::Hash32);
                if let Some(h) = root {
                    out.entry(h).or_insert_with(|| e.path());
                }
            }
        }
        let summary: Value = std::fs::read_to_string(at.join("manifest.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let Some(prev) = summary["prev_dir"].as_str() else {
            break;
        };
        at = std::fs::canonicalize(at.join(prev))
            .map_err(|e| format!("{}: its previous folder {prev}: {e}", at.display()))?;
    }
    Ok(out)
}

/// Every row of links version `m` (in folder `dir`), read from its shard
/// files in stored order.
fn read_links(dir: &Path, m: &Manifest) -> Res<Links> {
    let files = version_files(dir)?;
    let mut links = Links::new(&m.header.schema).map_err(|e| e.to_string())?;
    for e in &m.shards {
        let p = files.get(&e.root).ok_or_else(|| {
            format!(
                "shard {} of {} is in none of its folders: the links cannot be read",
                e.root, m.header.dataset
            )
        })?;
        let b = std::fs::read(p).map_err(|err| format!("{}: {err}", p.display()))?;
        let from = m.header.schema_of(e.index);
        // The file must be the shard its entry names: its size and its root
        // (review SUCC-B7 L3: a changed file of the same size was read).
        let root = check_shard(from, &b)
            .map_err(|err| format!("{}: {err}", p.display()))?
            .summary
            .root;
        if b.len() as u64 != e.bytes || root != e.root {
            return Err(format!(
                "{}: not shard {} of {} (its bytes do not make that root)",
                p.display(),
                e.root,
                m.header.dataset
            ));
        }
        for row in onyx_bulk::shard::split_rows(&b).map_err(|err| err.to_string())? {
            let cells = decode_row(from, row).map_err(|err| err.to_string())?;
            links
                .push(&present(from, &m.header.schema, &cells))
                .map_err(|err| err.to_string())?;
        }
    }
    Ok(links)
}

/// The JSON Lines of `rows`, numbered from `first`.
fn rows_jsonl(rows: &[NewRow], first: i128) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let v = json!({
            "n": (first + i as i128).to_string(),
            "from": r.from,
            "from_version": r.version,
            "from_prev": r.prev.to_string(),
            "to": r.to,
        });
        out.extend_from_slice(v.to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

/// Apply `lo` to the catalog being built (`leaves`, after `prev`; `listed`
/// the ids its `--dirs` list, `removed` its `--remove`): the tombstones of
/// `--move`, the links version written into `--links-out` and listed, and,
/// with `--links-next-generation`, the old generation's tombstone. Refused,
/// with nothing written, when a rule of amendment 6 item 8 does not hold.
pub(crate) fn apply(
    lo: &LinkOpts,
    publisher: &SourceId,
    prev: Option<&Catalog>,
    leaves: &mut BTreeMap<String, CatalogLeaf>,
    listed: &BTreeMap<String, Manifest>,
    removed: &[String],
    created: u64,
) -> Res<()> {
    if lo.out.exists() {
        return Err(format!(
            "--links-out {} exists: a links version is never rebuilt in place",
            lo.out.display()
        ));
    }
    // The folder is this run's from here on: whatever fails, nothing of it
    // stays to block the next run (review SUCC-B7 L3).
    apply_in(lo, publisher, prev, leaves, listed, removed, created).inspect_err(|_| lo.discard())
}

fn apply_in(
    lo: &LinkOpts,
    publisher: &SourceId,
    prev: Option<&Catalog>,
    leaves: &mut BTreeMap<String, CatalogLeaf>,
    listed: &BTreeMap<String, Manifest>,
    removed: &[String],
    created: u64,
) -> Res<()> {
    for (old, _) in &lo.moves {
        if removed.contains(old) {
            return Err(format!(
                "--move {old}: also in --remove (a move removes it)"
            ));
        }
        if listed.contains_key(old) {
            return Err(format!(
                "--move {old}: also listed by --dirs (a moved dataset gets no new version)"
            ));
        }
    }
    // The generations the previous catalog lists.
    let listing = |g: u32| -> Option<&CatalogLeaf> {
        let id = generation_id(g)?;
        prev.and_then(|p| p.find(&id).map(|i| &p.leaves[i]))
    };
    let live: Vec<u32> = (1..=MAX_LINK_GENERATIONS)
        .filter(|g| listing(*g).is_some_and(|l| !l.is_tombstone()))
        .collect();
    let dir_m = lo.links.as_ref().map(|d| read_manifest(d)).transpose()?;
    let (gen, after): (u32, Option<&Manifest>) = match &dir_m {
        Some(m) => {
            let g = generation_of(&m.header.dataset)
                .ok_or_else(|| format!("--links: {} is not a links dataset", m.header.dataset))?;
            if listing(g) != Some(&CatalogLeaf::of(&m.header)) {
                return Err(format!(
                    "--links: the previous catalog does not list this version of {} (the folder must be the one it lists)",
                    m.header.dataset
                ));
            }
            if let Some(lower) = live.iter().find(|x| **x < g) {
                return Err(format!(
                    "links generation {lower} is live below {}: its links would be the ones used",
                    m.header.dataset
                ));
            }
            if lo.next_generation {
                let next = g + 1;
                if next > MAX_LINK_GENERATIONS {
                    return Err(format!(
                        "no links generation after {g}: a publisher has {MAX_LINK_GENERATIONS}"
                    ));
                }
                if listing(next).is_some() {
                    return Err(format!(
                        "links generation {next} is listed already: it cannot start again"
                    ));
                }
                (next, None)
            } else {
                (g, Some(m))
            }
        }
        None => {
            if lo.next_generation {
                return Err("--links-next-generation needs --links: the generation it ends".into());
            }
            if let Some(g) = live.first() {
                return Err(format!(
                    "the previous catalog lists links generation {g} ({}): pass --links with the folder of that version",
                    generation_id(*g).unwrap_or_default()
                ));
            }
            let g = (1..=MAX_LINK_GENERATIONS)
                .find(|g| listing(*g).is_none())
                .ok_or("every links generation is used: no link can be written")?;
            (g, None)
        }
    };
    // The rows of this catalog.
    let mut new_rows = Vec::new();
    for (old, to) in &lo.moves {
        let t = leaves
            .get(old)
            .ok_or_else(|| format!("--move {old}: the previous catalog does not hold it"))?
            .tombstone()
            .ok_or_else(|| format!("--move {old}: removed already (use --relink)"))?;
        new_rows.push(NewRow {
            from: old.clone(),
            version: t.version,
            prev: t.prev,
            to: Some(to.clone()),
        });
        leaves.insert(old.clone(), t);
    }
    for (old, to) in lo
        .relinks
        .iter()
        .map(|(a, b)| (a, Some(b)))
        .chain(lo.unlinks.iter().map(|a| (a, None)))
    {
        let t = leaves
            .get(old)
            .filter(|l| l.is_tombstone())
            .ok_or_else(|| {
                format!("--relink/--unlink {old}: not a tombstone in the previous catalog (use --move to end a live dataset)")
            })?;
        new_rows.push(NewRow {
            from: old.clone(),
            version: t.version,
            prev: t.prev,
            to: to.cloned(),
        });
    }
    for to in new_rows.iter().filter_map(|r| r.to.as_ref()) {
        if !leaves.get(to).is_some_and(|l| !l.is_tombstone()) {
            return Err(format!(
                "{to} is not live in the catalog this builds: a link names a dataset listed live in the same catalog"
            ));
        }
    }
    // Next generation: only the links that act in this catalog, in their
    // stored order, renumbered from 1 (retractions too, so every chain ends
    // as before). An unusable generation holds no link that acts: nothing
    // to carry.
    let mut rows = Vec::new();
    let carry = dir_m.as_ref().is_some_and(|m| usable(&m.header).is_ok());
    if lo.next_generation && carry {
        let (dir, m) = lo.links.as_ref().zip(dir_m.as_ref()).ok_or("--links")?;
        let all = read_links(dir, m)?;
        let tombs: Vec<CatalogLeaf> = leaves
            .values()
            .filter(|l| l.is_tombstone())
            .cloned()
            .collect();
        let kept = all.only_for(&tombs);
        let mut kept: Vec<_> = kept.latest().cloned().collect();
        kept.sort_by_key(|r| r.position);
        rows.extend(kept.into_iter().map(|r| NewRow {
            from: r.from,
            version: r.from_version,
            prev: r.from_prev,
            to: r.to,
        }));
    }
    rows.extend(new_rows);
    if rows.is_empty() {
        return Err(
            "no link acts in this catalog and none is written: a new generation would be empty"
                .into(),
        );
    }
    let id = generation_id(gen).unwrap_or_default();
    let first = after.map_or(1, |m| m.header.key_max.saturating_add(1));
    let had = after.map_or(0, |m| m.header.row_count);
    let total = had + rows.len() as u64;
    if total > MAX_LINK_ROWS || after.is_some_and(|m| m.header.shard_count >= MAX_LINK_SHARDS) {
        return Err(format!(
            "{id} would hold {total} rows in {} shards, past its caps ({MAX_LINK_ROWS} rows, {MAX_LINK_SHARDS} shards): start the next generation with --links-next-generation",
            after.map_or(0, |m| m.header.shard_count) + 1
        ));
    }
    let schema = after.map_or_else(links_schema, |m| m.header.schema.clone());
    let rights = after.map_or_else(
        || DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: publisher.clone(),
            attribution: None,
            terms: String::new(),
        },
        |m| m.header.rights.clone(),
    );
    let prev_dir = match (&lo.links, after) {
        (Some(d), Some(_)) => Some(prev_dir_for(
            &d.display().to_string(),
            &lo.out.display().to_string(),
        )?),
        _ => None,
    };
    let created = after.map_or(created, |m| created.max(m.header.created_unix + 1));
    let built = build(
        &schema,
        &rows_jsonl(&rows, first)[..],
        Format::JsonLines,
        LINKS_SHARD_TARGET,
        after,
        NewVersion {
            dataset: id.clone(),
            publisher: publisher.clone(),
            created_unix: created,
            schema: schema.clone(),
            rights,
        },
        &lo.out,
    )
    .and_then(|b| {
        usable(&b.manifest.header).map_err(|e| {
            format!("{id}: {e}: start the next generation with --links-next-generation")
        })?;
        Ok(b)
    });
    let built = built?;
    write_built(&lo.out, &built, prev_dir)?;
    let h = &built.manifest.header;
    for (what, n, of) in [
        ("rows", h.row_count, MAX_LINK_ROWS),
        ("bytes", h.byte_count, LINKS_MAX_BYTES),
        (
            "shards",
            u64::from(h.shard_count),
            u64::from(MAX_LINK_SHARDS),
        ),
    ] {
        if n * 10 >= of * 9 {
            eprintln!(
                "warning: {id} holds {n} of its {of} {what} (90 % or more): start the next generation soon (--links-next-generation)"
            );
        }
    }
    for (old, to) in &lo.moves {
        let shown = |d: &str| match listed.get(d) {
            Some(m) => format!(
                "{d} rows={} keys={}..{}",
                m.header.row_count, m.header.key_min, m.header.key_max
            ),
            None => format!("{d} version={}", leaves.get(d).map_or(0, |l| l.version)),
        };
        println!("MOVE {} -> {}", shown(old), shown(to));
    }
    if lo.next_generation {
        if let Some(m) = &dir_m {
            let t = CatalogLeaf::of(&m.header)
                .tombstone()
                .ok_or("the old generation's tombstone")?;
            leaves.insert(m.header.dataset.clone(), t);
        }
    }
    leaves.insert(id.clone(), CatalogLeaf::of(h));
    println!(
        "LINKS dataset={id} version={} rows={} new_rows={} shards={} bytes={} root={} out={}",
        h.version,
        h.row_count,
        rows.len(),
        h.shard_count,
        h.byte_count,
        h.root(),
        lo.out.display()
    );
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::{build_cmd, catalog_build_cmd, parse_opts, read_catalog};
    use super::*;
    use onyx_bulk::catalog::Catalog;

    /// A folder for one test, with datasets `synthetic.a`, `b` and `c`
    /// built in it and catalog 0 listing all three.
    pub(crate) struct T {
        pub(crate) dir: PathBuf,
    }

    impl T {
        pub(crate) fn new(name: &str) -> T {
            let dir = std::env::temp_dir()
                .join(format!("onyx-publish-links-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("dir");
            std::fs::write(
                dir.join("schema.json"),
                r#"{"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":2}],"key":"t"}"#,
            )
            .expect("w");
            std::fs::write(dir.join("rows.csv"), "t,v\n1,1.50\n2,2.25\n").expect("w");
            let t = T { dir };
            for d in ["a", "b", "c"] {
                t.build(&format!("synthetic.{d}"), d).expect("built");
            }
            let all = t.list("all.txt", &["a", "b", "c"]);
            t.cat(&["--dirs", &all, "--out", &t.p("c0.bin")])
                .expect("c0");
            t
        }

        pub(crate) fn p(&self, n: &str) -> String {
            self.dir.join(n).display().to_string()
        }

        pub(crate) fn build(&self, dataset: &str, out: &str) -> Res<()> {
            let a: Vec<String> = [
                "--schema",
                &self.p("schema.json"),
                "--input",
                &self.p("rows.csv"),
                "--dataset",
                dataset,
                "--publisher",
                "example.publisher",
                "--license",
                "CC0-1.0",
                "--out",
                &self.p(out),
                "--created",
                "100",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            build_cmd(&parse_opts(&a).expect("opts"))
        }

        pub(crate) fn list(&self, name: &str, items: &[&str]) -> String {
            let text: String = items.iter().map(|i| self.p(i) + "\n").collect();
            std::fs::write(self.dir.join(name), text).expect("w");
            self.p(name)
        }

        /// `catalog-build --publisher example.publisher` with `args` (and
        /// an empty `--dirs` unless given).
        pub(crate) fn cat(&self, args: &[&str]) -> Res<()> {
            let mut a: Vec<String> = vec!["--publisher".into(), "example.publisher".into()];
            if !args.contains(&"--dirs") {
                a.extend(["--dirs".into(), self.list("none.txt", &[])]);
            }
            a.extend(args.iter().map(|s| s.to_string()));
            catalog_build_cmd(&parse_opts(&a)?)
        }

        pub(crate) fn catalog(&self, n: &str) -> Catalog {
            read_catalog(&self.dir.join(n)).expect("catalog")
        }

        pub(crate) fn leaf(&self, c: &str, d: &str) -> CatalogLeaf {
            let c = self.catalog(c);
            c.leaves[c.find(d).expect("listed")].clone()
        }

        /// Every row of the links version in folder `n`.
        pub(crate) fn links(&self, n: &str) -> Links {
            let d = self.dir.join(n);
            read_links(&d, &read_manifest(&d).expect("manifest")).expect("rows")
        }

        /// Catalog 1 (`c1.bin`) built by hand: `synthetic.a` moved to
        /// `synthetic.b` by the first of `rows` rows of links generation 1,
        /// version 1 (folder `l1`); the other rows name made-up tombstones.
        pub(crate) fn hand_built_move(&self, count: u64) {
            let t = self;
            let c0 = t.catalog("c0.bin");
            let tomb = t.leaf("c0.bin", "synthetic.a").tombstone().expect("t");
            // A full generation 1: the move of a, then made-up rows to the cap.
            let mut rows = vec![NewRow {
                from: "synthetic.a".into(),
                version: tomb.version,
                prev: tomb.prev,
                to: Some("synthetic.b".into()),
            }];
            rows.extend((1..count).map(|i| NewRow {
                from: format!("synthetic.f{i}"),
                version: 2,
                prev: onyx_core::Hash32([9; 32]),
                to: Some("synthetic.b".into()),
            }));
            let publisher = SourceId("example.publisher".into());
            let full = build(
                &links_schema(),
                &rows_jsonl(&rows, 1)[..],
                Format::JsonLines,
                LINKS_SHARD_TARGET,
                None,
                NewVersion {
                    dataset: "onyx.successors".into(),
                    publisher: publisher.clone(),
                    created_unix: 100,
                    schema: links_schema(),
                    rights: DataRights {
                        license: "CC0-1.0".into(),
                        redistribution: Redistribution::Allowed,
                        source: publisher.clone(),
                        attribution: None,
                        terms: String::new(),
                    },
                },
                &t.dir.join("l1"),
            )
            .expect("full");
            write_built(&t.dir.join("l1"), &full, None).expect("written");
            let mut leaves = c0.leaves.clone();
            let i = c0.find("synthetic.a").expect("a");
            leaves[i] = tomb.clone();
            leaves.push(CatalogLeaf::of(&full.manifest.header));
            let c1 = Catalog::build(publisher, Some(&c0), c0.header.created_unix + 1, leaves)
                .expect("c1");
            std::fs::write(t.dir.join("c1.bin"), borsh::to_vec(&c1).expect("b")).expect("w");
        }

        /// `args` is refused with `why`, and writes neither `cx.bin` nor
        /// the links folder `lx`.
        fn refused(&self, args: &[&str], why: &str) {
            let mut a = args.to_vec();
            let out = self.p("cx.bin");
            a.extend(["--out", &out]);
            let e = self.cat(&a).expect_err(why);
            assert!(e.contains(why), "{why}: {e}");
            assert!(!self.dir.join("cx.bin").exists(), "{why}");
            assert!(!self.dir.join("lx").exists(), "{why}");
        }
    }

    impl Drop for T {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// ADR-0059 amendment 6 item 8: `--move OLD=NEW` ends OLD with a
    /// tombstone and lists, in the same catalog, the links version written
    /// into `--links-out` (generation 1, version 1 at first; the next version
    /// after `--links`, numbered on). Refused: a NEW not live in the catalog
    /// built, OLD as NEW, OLD also in `--remove` or `--dirs`, an existing
    /// `--links-out`, a missing `--links` or one the previous catalog does
    /// not list.
    #[test]
    fn catalog_build_move_removes_and_writes_the_links_version() {
        let t = T::new("move");
        t.cat(&[
            "--prev",
            &t.p("c0.bin"),
            "--move",
            "synthetic.a=synthetic.b",
            "--links-out",
            &t.p("l1"),
            "--out",
            &t.p("c1.bin"),
        ])
        .expect("c1");
        let a0 = t.leaf("c0.bin", "synthetic.a");
        let tomb = t.leaf("c1.bin", "synthetic.a");
        assert_eq!(Some(&tomb), a0.tombstone().as_ref());
        let l1 = read_manifest(&t.dir.join("l1")).expect("l1");
        assert_eq!(
            t.leaf("c1.bin", "onyx.successors"),
            CatalogLeaf::of(&l1.header)
        );
        assert_eq!((l1.header.version, l1.header.row_count), (1, 1));
        let links = t.links("l1");
        let row = links.link_of(&tomb).expect("the link");
        assert_eq!((row.n, row.to.as_deref()), (1, Some("synthetic.b")));
        // Refused, with nothing written.
        let c1 = t.p("c1.bin");
        let l1p = t.p("l1");
        let lx = t.p("lx");
        let with_c = t.list("c.txt", &["c"]);
        for (args, why) in [
            (vec!["--move", "synthetic.c=synthetic.a"], "not live"),
            (vec!["--move", "synthetic.c=synthetic.z"], "not live"),
            (vec!["--move", "synthetic.c=synthetic.c"], "as itself"),
            (
                vec![
                    "--move",
                    "synthetic.c=synthetic.b",
                    "--remove",
                    "synthetic.c",
                ],
                "also in --remove",
            ),
            (
                vec!["--move", "synthetic.c=synthetic.b", "--dirs", &with_c],
                "also listed by --dirs",
            ),
        ] {
            let mut a = vec![
                "--prev",
                c1.as_str(),
                "--links",
                l1p.as_str(),
                "--links-out",
                lx.as_str(),
            ];
            a.extend(args);
            t.refused(&a, why);
        }
        t.refused(
            &[
                "--prev",
                &c1,
                "--move",
                "synthetic.c=synthetic.b",
                "--links-out",
                &lx,
            ],
            "pass --links",
        );
        t.refused(
            &[
                "--prev",
                &c1,
                "--links",
                &l1p,
                "--move",
                "synthetic.c=synthetic.b",
                "--links-out",
                &l1p,
            ],
            "exists",
        );
        t.refused(
            &["--prev", &c1, "--links", &l1p, "--out2", "x"],
            "not an option",
        );
        // The next version, after the one catalog 1 lists.
        t.cat(&[
            "--prev",
            &c1,
            "--links",
            &l1p,
            "--move",
            "synthetic.c=synthetic.b",
            "--links-out",
            &t.p("l2"),
            "--out",
            &t.p("c2.bin"),
        ])
        .expect("c2");
        let l2 = read_manifest(&t.dir.join("l2")).expect("l2");
        assert_eq!(
            (
                l2.header.version,
                l2.header.row_count,
                l2.header.shard_count
            ),
            (2, 2, 2)
        );
        let tomb_c = t.leaf("c2.bin", "synthetic.c");
        assert_eq!(t.links("l2").link_of(&tomb_c).map(|r| r.n), Some(2));
        let summary = std::fs::read_to_string(t.dir.join("l2").join("manifest.json")).expect("r");
        assert!(summary.contains("\"prev_dir\": \"../l1\""), "{summary}");
        // A folder the previous catalog does not list.
        t.refused(
            &[
                "--prev",
                &t.p("c2.bin"),
                "--links",
                &l1p,
                "--unlink",
                "synthetic.c",
                "--links-out",
                &lx,
            ],
            "does not list this version",
        );
    }

    /// `--relink OLD=NEW` and `--unlink OLD` write a row for an OLD that is
    /// already a tombstone (a link after the fact, a fix, a retraction): the
    /// last row wins; a live OLD is refused.
    #[test]
    fn catalog_build_relink_and_unlink_need_a_tombstone() {
        let t = T::new("relink");
        t.cat(&[
            "--prev",
            &t.p("c0.bin"),
            "--move",
            "synthetic.a=synthetic.b",
            "--links-out",
            &t.p("l1"),
            "--out",
            &t.p("c1.bin"),
        ])
        .expect("c1");
        let (c1, l1, lx) = (t.p("c1.bin"), t.p("l1"), t.p("lx"));
        for args in [
            vec!["--relink", "synthetic.c=synthetic.b"],
            vec!["--unlink", "synthetic.c"],
        ] {
            let mut a = vec![
                "--prev",
                c1.as_str(),
                "--links",
                l1.as_str(),
                "--links-out",
                lx.as_str(),
            ];
            a.extend(args);
            t.refused(&a, "not a tombstone");
        }
        t.cat(&[
            "--prev",
            &c1,
            "--links",
            &l1,
            "--relink",
            "synthetic.a=synthetic.c",
            "--links-out",
            &t.p("l2"),
            "--out",
            &t.p("c2.bin"),
        ])
        .expect("c2");
        let tomb = t.leaf("c2.bin", "synthetic.a");
        let r = t.links("l2").link_of(&tomb).cloned().expect("relinked");
        assert_eq!((r.n, r.to.as_deref()), (2, Some("synthetic.c")));
        t.cat(&[
            "--prev",
            &t.p("c2.bin"),
            "--links",
            &t.p("l2"),
            "--unlink",
            "synthetic.a",
            "--links-out",
            &t.p("l3"),
            "--out",
            &t.p("c3.bin"),
        ])
        .expect("c3");
        let r = t.links("l3").link_of(&tomb).cloned().expect("retracted");
        assert_eq!((r.n, r.to), (3, None));
    }

    /// Item 8 and SUCC-R2 M4: a links version past `MAX_LINK_ROWS` is
    /// refused (nothing written); `--links-next-generation` writes version 1
    /// of the next generation with only the links that act in the catalog
    /// built, renumbered from 1, plus this catalog's rows, and ends the full
    /// generation in the same catalog.
    #[test]
    fn catalog_build_refuses_rows_past_the_cap_and_starts_the_next_generation() {
        let t = T::new("cap");
        let tomb = t.leaf("c0.bin", "synthetic.a").tombstone().expect("t");
        t.hand_built_move(MAX_LINK_ROWS);
        let (c1p, l1, lx) = (t.p("c1.bin"), t.p("l1"), t.p("lx"));
        t.refused(
            &[
                "--prev",
                &c1p,
                "--links",
                &l1,
                "--move",
                "synthetic.c=synthetic.b",
                "--links-out",
                &lx,
            ],
            "--links-next-generation",
        );
        t.cat(&[
            "--prev",
            &c1p,
            "--links",
            &l1,
            "--links-next-generation",
            "--move",
            "synthetic.c=synthetic.b",
            "--links-out",
            &t.p("l2"),
            "--out",
            &t.p("c2.bin"),
        ])
        .expect("c2");
        assert!(t.leaf("c2.bin", "onyx.successors").is_tombstone());
        let l2 = read_manifest(&t.dir.join("l2")).expect("l2");
        assert_eq!(l2.header.dataset, "onyx.successors.2");
        assert_eq!((l2.header.version, l2.header.row_count), (1, 2));
        assert_eq!(
            t.leaf("c2.bin", "onyx.successors.2"),
            CatalogLeaf::of(&l2.header)
        );
        let links = t.links("l2");
        let a = links.link_of(&tomb).cloned().expect("a kept");
        assert_eq!((a.n, a.to.as_deref()), (1, Some("synthetic.b")));
        let c = links
            .link_of(&t.leaf("c2.bin", "synthetic.c"))
            .cloned()
            .expect("c");
        assert_eq!(c.n, 2);
    }

    /// The reserved ids are never removed, widened, listed by `--dirs`,
    /// moved, linked to, or built by hand.
    #[test]
    fn the_reserved_ids_are_never_removed_widened_or_built_by_hand() {
        let t = T::new("reserved");
        for id in ["onyx.successors", "onyx.successors.2"] {
            let e = t.build(id, "x").expect_err(id);
            assert!(e.contains("reserved"), "{e}");
            assert!(!t.dir.join("x").exists());
        }
        let a: Vec<String> = [
            "--schema",
            &t.p("schema.json"),
            "--input",
            &t.p("rows.csv"),
            "--dataset",
            "onyx.successors",
            "--publisher",
            "example.publisher",
            "--license",
            "CC0-1.0",
            "--out",
            &t.p("w"),
            "--prev",
            &t.p("a"),
            "--widen",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let e = build_cmd(&parse_opts(&a).expect("opts")).expect_err("widen");
        assert!(e.contains("reserved"), "{e}");
        t.cat(&[
            "--prev",
            &t.p("c0.bin"),
            "--move",
            "synthetic.a=synthetic.b",
            "--links-out",
            &t.p("l1"),
            "--out",
            &t.p("c1.bin"),
        ])
        .expect("c1");
        let c1 = t.p("c1.bin");
        let l1_list = t.list("l1.txt", &["l1"]);
        for (args, why) in [
            (
                vec!["--prev", c1.as_str(), "--remove", "onyx.successors"],
                "reserved",
            ),
            (vec!["--prev", c1.as_str(), "--dirs", &l1_list], "reserved"),
            (
                vec![
                    "--prev",
                    c1.as_str(),
                    "--move",
                    "onyx.successors=synthetic.b",
                ],
                "reserved",
            ),
            (
                vec![
                    "--prev",
                    c1.as_str(),
                    "--move",
                    "synthetic.c=onyx.successors.3",
                ],
                "reserved",
            ),
            (
                vec![
                    "--prev",
                    c1.as_str(),
                    "--relink",
                    "synthetic.a=onyx.successors",
                ],
                "reserved",
            ),
        ] {
            t.refused(&args, why);
        }
    }
}

#[cfg(test)]
mod b7_tests {
    use super::tests::T;

    /// Review SUCC-B7 L3: a run that fails after the links version is
    /// written (here the catalog file cannot be written) leaves no
    /// `--links-out` folder, so the same command with the mistake fixed
    /// runs; before the fix the folder stayed and every rerun was refused
    /// ("--links-out exists").
    #[test]
    fn succ_b7_a_failed_run_leaves_no_links_folder_behind() {
        let t = T::new("b7-discard");
        let args = |out: &str| {
            vec![
                "--prev".to_string(),
                t.p("c0.bin"),
                "--move".into(),
                "synthetic.a=synthetic.b".into(),
                "--links-out".into(),
                t.p("l1"),
                "--out".into(),
                t.p(out),
            ]
        };
        let run = |out: &str| {
            let a = args(out);
            t.cat(&a.iter().map(String::as_str).collect::<Vec<_>>())
        };
        run("no-such-folder/c1.bin").expect_err("the catalog cannot be written");
        assert!(!t.dir.join("l1").exists(), "the links version is discarded");
        run("c1.bin").expect("the rerun");
        assert!(t.dir.join("l1").join("manifest.bin").exists());
    }

    /// Review SUCC-B7 L3: the next generation is built from shard files
    /// that hash to their entries' roots; a file changed in place (same
    /// size) is refused, with nothing written.
    #[test]
    fn succ_b7_a_changed_links_shard_is_not_carried_into_the_next_generation() {
        let t = T::new("b7-shard");
        t.cat(&[
            "--prev",
            &t.p("c0.bin"),
            "--move",
            "synthetic.a=synthetic.b",
            "--links-out",
            &t.p("l1"),
            "--out",
            &t.p("c1.bin"),
        ])
        .expect("c1");
        // The row's target changed from synthetic.b to synthetic.c on disk.
        let shards = t.dir.join("l1").join("shards");
        let file = std::fs::read_dir(&shards)
            .expect("shards")
            .flatten()
            .next()
            .expect("one shard")
            .path();
        let mut b = std::fs::read(&file).expect("read");
        let last = b.len() - 1;
        assert_eq!(b[last], b'b');
        b[last] = b'c';
        std::fs::write(&file, b).expect("write");
        let e = t
            .cat(&[
                "--prev",
                &t.p("c1.bin"),
                "--links",
                &t.p("l1"),
                "--links-next-generation",
                "--links-out",
                &t.p("l2"),
                "--out",
                &t.p("c2.bin"),
            ])
            .expect_err("a changed shard");
        assert!(e.contains("do not make that root"), "{e}");
        assert!(!t.dir.join("l2").exists() && !t.dir.join("c2.bin").exists());
    }
}
