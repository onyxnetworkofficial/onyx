//! Successor links (ADR-0059 amendment 6, draft 3): where an ended dataset
//! went, as rows of a reserved links dataset the publisher lists in its
//! catalogs.
//!
//! A publisher that moves a dataset to a new id (a change no widening takes)
//! ends the old id with a tombstone and, in the same catalog, adds a row to
//! its links dataset: `(n, from, from_version, from_prev, to)`. One rule
//! ([`Links::link_of`]) reads them: the link of a removed id is the *last*
//! valid row naming exactly its tombstone (version and previous root), and a
//! last row with a null `to` retracts it. [`follow`] walks the chain from an
//! old id, at most [`MAX_SUCCESSOR_HOPS`] links, all in one catalog.
//!
//! The links live in generations (`onyx.successors`, `onyx.successors.2`,
//! ... `onyx.successors.8`): the lowest usable one in a catalog holds them
//! ([`choose_generation`]), so a generation that broke for good (removed, a
//! schema no longer readable, more than [`MAX_LINK_ROWS`] rows) gives way to
//! the next. A reader reads every row of the used version, pinned to its root
//! and proven complete from the first row to the last ([`read_links_pages`]):
//! a rows answer alone proves only the inside of a page (THREATS CL5-9).
//! A usable version holds at most [`LINKS_MAX_BYTES`] in at most
//! [`MAX_LINK_SHARDS`] shards, each file checked against its entry's bytes,
//! so reading one whole is cheap for a node and a reader alike (reviews
//! SUCC-B4, SUCC-B5).

use crate::catalog::CatalogLeaf;
use crate::manifest::{ManifestHeader, ShardEntry};
use crate::row::{decode_row, present, Cell};
use crate::schema::{Column, ColumnKind, Schema};
use crate::wire::{check_rows_answer, hex_field, WireError};
use borsh::BorshDeserialize;
use onyx_core::ids::is_dotted_id;
use onyx_core::Hash32;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The id of links generation 1; generation `g` > 1 is `onyx.successors.g`
/// ([`generation_id`]).
pub const SUCCESSORS_ID: &str = "onyx.successors";
/// Links generations a publisher may use.
pub const MAX_LINK_GENERATIONS: u32 = 8;
/// Rows a links generation may hold and stay usable (review SUCC-R2 M4).
pub const MAX_LINK_ROWS: u64 = 65_536;
/// Bytes a links version may hold and stay usable (review SUCC-B4): its
/// header's `byte_count`, the sum of its entries' `bytes`, which
/// [`Manifest::validate`](crate::manifest::Manifest::validate) checks
/// against the entries. An entry may name a file held under another entry,
/// so a node loads a shard only when its file is its entry's exact size, and
/// [`read_links_pages`] refuses rows past it (review SUCC-B5 H1). A links
/// row of the tool takes at most 383 bytes in a shard (two 128-byte ids, a
/// 66-character root, its framing, and 4 more for a shard of one row), so
/// [`MAX_LINK_ROWS`] of them fit in 24 MiB.
pub const LINKS_MAX_BYTES: u64 = 32 << 20;
/// Shards a links version may list and stay usable (review SUCC-B5 M1): the
/// shards, not only the bytes, bound a node's loads (each distinct file once
/// a job; about 0.7 s for 4,096 at the 177 µs a load review SUCC-B6
/// measured) and a reader's shard proofs. Versions only append (each adds at
/// least one shard), and the tool adds one per catalog that moves or relinks
/// something, refusing a version past this; the next generation holds the
/// links then. So a publisher writes links in at most 4,096 x 8 = 32,768
/// catalogs in all (review SUCC-B6 L2: 1,024 allowed only 8,192).
pub const MAX_LINK_SHARDS: u32 = 4_096;
/// Links a chain follows at most.
pub const MAX_SUCCESSOR_HOPS: usize = 8;
/// Rows a reader asks for in each page of links rows.
pub const LINKS_PAGE_ROWS: u64 = 1_000;

/// The id of links generation `g` (1 to [`MAX_LINK_GENERATIONS`]).
pub fn generation_id(g: u32) -> Option<String> {
    match g {
        1 => Some(SUCCESSORS_ID.into()),
        2..=MAX_LINK_GENERATIONS => Some(format!("{SUCCESSORS_ID}.{g}")),
        _ => None,
    }
}

/// Whether `id` is reserved for links: every id from `onyx.successors` on.
/// No row may name one, and the tool never builds, removes or widens one by
/// hand.
pub fn is_links_id(id: &str) -> bool {
    id.starts_with(SUCCESSORS_ID)
}

/// The schema the tool writes: `n` (the key), `from`, `from_version`,
/// `from_prev`, `to` (nullable).
pub fn links_schema() -> Schema {
    let col = |name: &str, kind, nullable| Column {
        name: name.into(),
        kind,
        nullable,
    };
    Schema {
        columns: vec![
            col("n", ColumnKind::Int, false),
            col("from", ColumnKind::Text, false),
            col("from_version", ColumnKind::Int, false),
            col("from_prev", ColumnKind::Text, false),
            col("to", ColumnKind::Text, true),
        ],
        key: 0,
    }
}

/// Where the five links columns are in a readable schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkColumns {
    n: usize,
    from: usize,
    from_version: usize,
    from_prev: usize,
    to: usize,
}

/// Why a links generation is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unusable {
    /// Its newest schema lacks a links column or gives one another kind.
    #[error("the links schema is not readable: {0}")]
    Schema(String),
    /// More rows than [`MAX_LINK_ROWS`].
    #[error("{0} link rows, more than {MAX_LINK_ROWS}")]
    TooMany(u64),
    /// More bytes than [`LINKS_MAX_BYTES`] (review SUCC-B4).
    #[error("{0} bytes of link shards, more than {LINKS_MAX_BYTES}")]
    TooLarge(u64),
    /// More shards than [`MAX_LINK_SHARDS`] (review SUCC-B5 M1).
    #[error("{0} link shards, more than {MAX_LINK_SHARDS}")]
    TooManyShards(u32),
    /// No rows at all (review SUCC-B1 L4).
    #[error("a links version with no rows")]
    Empty,
    /// A newer version that moves a links column or holds fewer rows
    /// than were read (review SUCC-B1 L4).
    #[error("the newer links version does not continue this one: {0}")]
    NotNewer(String),
}

/// The links columns of `s`, read by name: `n` an `int` and the key, `from`,
/// `from_version` (`int`) and `from_prev` not nullable, `to` nullable text.
/// Other columns (appended by a later amendment) are ignored.
pub fn read_columns(s: &Schema) -> Result<LinkColumns, Unusable> {
    let find = |name: &str, kind: ColumnKind, nullable: bool| -> Result<usize, Unusable> {
        let i = s
            .index_of(name)
            .ok_or_else(|| Unusable::Schema(format!("no column {name}")))?;
        let c = &s.columns[i];
        if c.kind != kind || c.nullable != nullable {
            return Err(Unusable::Schema(format!(
                "column {name} is not {}{kind:?}",
                if nullable { "nullable " } else { "" }
            )));
        }
        Ok(i)
    };
    let n = find("n", ColumnKind::Int, false)?;
    if usize::from(s.key) != n {
        return Err(Unusable::Schema("n is not the key".into()));
    }
    Ok(LinkColumns {
        n,
        from: find("from", ColumnKind::Text, false)?,
        from_version: find("from_version", ColumnKind::Int, false)?,
        from_prev: find("from_prev", ColumnKind::Text, false)?,
        to: find("to", ColumnKind::Text, true)?,
    })
}

/// Whether a links version can hold the links: a readable newest schema, 1
/// to [`MAX_LINK_ROWS`] rows, at most [`LINKS_MAX_BYTES`] bytes and at most
/// [`MAX_LINK_SHARDS`] shards. The bytes are the header's claim: a node checks
/// each shard file's size against its entry before it reads it, and a reader
/// bounds the rows it receives by the entry's bytes (review SUCC-B5 H1).
pub fn usable(h: &ManifestHeader) -> Result<LinkColumns, Unusable> {
    if h.row_count == 0 {
        return Err(Unusable::Empty);
    }
    if h.row_count > MAX_LINK_ROWS {
        return Err(Unusable::TooMany(h.row_count));
    }
    if h.byte_count > LINKS_MAX_BYTES {
        return Err(Unusable::TooLarge(h.byte_count));
    }
    if h.shard_count > MAX_LINK_SHARDS {
        return Err(Unusable::TooManyShards(h.shard_count));
    }
    read_columns(&h.schema)
}

/// One valid links row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRow {
    /// Its place among every row of the version (0-based, stored order).
    pub position: u64,
    /// Its link number (the key).
    pub n: i128,
    /// The ended id.
    pub from: String,
    /// The version of the tombstone it acts on.
    pub from_version: u32,
    /// The previous root that tombstone names.
    pub from_prev: Hash32,
    /// The id that continues it; `None` retracts the link.
    pub to: Option<String>,
}

/// `0x` and 64 lowercase hex digits, not zero (review SUCC-R2 L2).
fn parse_root(s: &str) -> Option<Hash32> {
    let h = s.strip_prefix("0x")?;
    if h.len() != 64
        || !h
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    let b: [u8; 32] = hex::decode(h).ok()?.try_into().ok()?;
    (b != [0; 32]).then_some(Hash32(b))
}

fn target(id: &str) -> bool {
    is_dotted_id(id) && !is_links_id(id)
}

/// Row `cells` (shown under the newest schema) at `position`, when valid:
/// `from` and a non-null `to` dotted ids, neither a links id, `from != to`,
/// `from_version` in 2..=`u32::MAX`, `from_prev` a lowercase root. `None`
/// for a row that is inert.
pub fn parse_row(cols: &LinkColumns, cells: &[Cell], position: u64) -> Option<LinkRow> {
    let text = |i: usize| match cells.get(i) {
        Some(Cell::Text(t)) => Some(t.as_str()),
        _ => None,
    };
    let Some(Cell::Int(n)) = cells.get(cols.n) else {
        return None;
    };
    let from = text(cols.from).filter(|f| target(f))?;
    let from_version = match cells.get(cols.from_version) {
        Some(Cell::Int(v)) => u32::try_from(*v).ok().filter(|v| *v >= 2)?,
        _ => return None,
    };
    let from_prev = parse_root(text(cols.from_prev)?)?;
    let to = match cells.get(cols.to) {
        Some(Cell::Null) => None,
        Some(Cell::Text(t)) if target(t) && t != from => Some(t.clone()),
        _ => return None,
    };
    Some(LinkRow {
        position,
        n: *n,
        from: from.into(),
        from_version,
        from_prev,
        to,
    })
}

/// The links of one version, read row by row in stored order. Versions
/// append, so a newer version of the same generation only adds rows: a node
/// pushes the new ones (review SUCC-R2 M4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Links {
    cols: LinkColumns,
    rows: u64,
    last: BTreeMap<(String, u32, Hash32), LinkRow>,
}

impl Links {
    /// No rows yet, under `schema` (the version's newest).
    pub fn new(schema: &Schema) -> Result<Links, Unusable> {
        Ok(Links {
            cols: read_columns(schema)?,
            rows: 0,
            last: BTreeMap::new(),
        })
    }

    /// The next row in stored order, its cells shown under the newest
    /// schema. An invalid row only counts; a valid one replaces any earlier
    /// row for the same tombstone. Past [`MAX_LINK_ROWS`], refused.
    pub fn push(&mut self, cells: &[Cell]) -> Result<(), Unusable> {
        if self.rows >= MAX_LINK_ROWS {
            return Err(Unusable::TooMany(self.rows + 1));
        }
        if let Some(r) = parse_row(&self.cols, cells, self.rows) {
            self.last
                .insert((r.from.clone(), r.from_version, r.from_prev), r);
        }
        self.rows += 1;
        Ok(())
    }

    /// Rows read so far.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// Whether `newer`, a later version of the same generation, may go on
    /// from these rows (review SUCC-B1 L4): usable, its newest schema keeps
    /// the five links columns where they are with their kinds (a widening
    /// appends columns, amendment 5), and it holds at least the rows read.
    /// Then its new shards' rows may be pushed, shown under its schema;
    /// otherwise a node reads the generation again from its first row.
    pub fn accepts(&self, newer: &ManifestHeader) -> Result<(), Unusable> {
        let cols = usable(newer)?;
        if cols != self.cols {
            return Err(Unusable::NotNewer("a links column moved".into()));
        }
        if newer.row_count < self.rows {
            return Err(Unusable::NotNewer(format!(
                "{} rows, fewer than the {} read",
                newer.row_count, self.rows
            )));
        }
        Ok(())
    }

    /// The link of `tombstone` (ADR-0059 amendment 6 item 2): the last valid
    /// row whose `from`, `from_version` and `from_prev` are exactly its
    /// dataset, version and previous root. `None` for a leaf that is no
    /// tombstone or has no row; a row with `to == None` is a retraction.
    pub fn link_of(&self, tombstone: &CatalogLeaf) -> Option<&LinkRow> {
        if !tombstone.is_tombstone() {
            return None;
        }
        self.last
            .get(&(tombstone.dataset.clone(), tombstone.version, tombstone.prev))
    }

    /// The last valid row for every tombstone any row names, in `(from,
    /// version, prev)` order: retractions too, and rows for a tombstone no
    /// catalog lists (written early, or of a line set aside), which act on
    /// nothing. For what acts in a catalog, see [`Links::effective`].
    pub fn latest(&self) -> impl Iterator<Item = &LinkRow> {
        self.last.values()
    }

    /// The links that act in a catalog: for each of its `tombstones`, the
    /// row [`Links::link_of`] gives, when it names a target (retractions
    /// left out). What a node's reverse index (`replaces`) reads.
    pub fn effective<'a>(
        &'a self,
        tombstones: impl IntoIterator<Item = &'a CatalogLeaf>,
    ) -> impl Iterator<Item = (&'a CatalogLeaf, &'a LinkRow)> {
        tombstones
            .into_iter()
            .filter_map(|t| self.link_of(t).map(|r| (t, r)))
            .filter(|(_, r)| r.to.is_some())
    }

    /// Only the rows that act on `tombstones`: each one's
    /// [`Links::link_of`], retractions included. For those tombstones,
    /// [`Links::link_of`] and [`follow`] answer exactly as with every row,
    /// and a chain in a catalog asks only for that catalog's tombstones; so a
    /// node keeps this for one catalog, bounded by its leaves, not by the rows
    /// (review SUCC-B4 H1). Not for pushing more rows to.
    pub fn only_for<'a>(&self, tombstones: impl IntoIterator<Item = &'a CatalogLeaf>) -> Links {
        let last = tombstones
            .into_iter()
            .filter_map(|t| self.link_of(t))
            .map(|r| ((r.from.clone(), r.from_version, r.from_prev), r.clone()))
            .collect();
        Links {
            cols: self.cols,
            rows: self.rows,
            last,
        }
    }

    /// Rows kept: the last valid row of each tombstone a row names.
    pub fn kept(&self) -> usize {
        self.last.len()
    }
}

/// A links generation as a catalog lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenState {
    /// Not listed.
    Absent,
    /// Listed as a tombstone.
    Removed,
    /// Listed live, with the header of the version its leaf names when the
    /// caller has it.
    Live {
        /// The catalog's leaf for the generation.
        leaf: CatalogLeaf,
        /// The header of the leaf's root.
        header: Option<Box<ManifestHeader>>,
    },
}

/// Why a lower generation was passed over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenSkip {
    /// Not listed.
    Absent,
    /// Removed.
    Removed,
    /// Live but not usable.
    Unusable(Unusable),
}

/// The generation that holds the links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsedGeneration {
    /// 1 to [`MAX_LINK_GENERATIONS`].
    pub generation: u32,
    /// Its id ([`generation_id`]).
    pub id: String,
    /// The root of the version the catalog lists.
    pub root: Hash32,
    /// Its header.
    pub header: Box<ManifestHeader>,
}

/// What [`choose_generation`] found: the used generation (none: every chain
/// ends [`End::NoLinks`]) and why each lower one was passed over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenChoice {
    /// The lowest usable generation.
    pub used: Option<UsedGeneration>,
    /// Each generation below it (or every one, when none is usable).
    pub skipped: Vec<(u32, GenSkip)>,
}

/// A generation [`choose_generation`] could not decide on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GenFault<E> {
    /// The caller's lookup failed.
    #[error("{0}")]
    Lookup(E),
    /// A live generation without its header (a node: `pending`; a reader:
    /// the answer lacks it).
    #[error("links generation {0} is listed without its manifest header")]
    NoHeader(u32),
    /// A header that is not the one the leaf names (dataset, publisher,
    /// version, previous root and root).
    #[error("links generation {0}: the header is not the one the leaf names")]
    WrongHeader(u32),
    /// A live leaf of another id, or a tombstone given as live (review
    /// SUCC-B1 L2).
    #[error("links generation {0}: the leaf is not that generation's live leaf")]
    WrongLeaf(u32),
    /// A header that does not hold its own rules ([`ManifestHeader::validate`]).
    #[error("links generation {0}: {1}")]
    BadHeader(u32, String),
}

/// The links generation of a catalog (ADR-0059 amendment 6 item 1): the
/// lowest one that `state` (asked for 1, 2, ... in turn) reports live with a
/// usable header. Generations above it are never asked about. A live leaf
/// must be that generation's id, and its header the one it names and valid.
pub fn choose_generation<E>(
    mut state: impl FnMut(u32) -> Result<GenState, E>,
) -> Result<GenChoice, GenFault<E>> {
    let mut skipped = Vec::new();
    for g in 1..=MAX_LINK_GENERATIONS {
        let skip = match state(g).map_err(GenFault::Lookup)? {
            GenState::Absent => GenSkip::Absent,
            GenState::Removed => GenSkip::Removed,
            GenState::Live { leaf, header } => {
                let id = generation_id(g).unwrap_or_default();
                if leaf.dataset != id || leaf.is_tombstone() {
                    return Err(GenFault::WrongLeaf(g));
                }
                let header = header.ok_or(GenFault::NoHeader(g))?;
                if !leaf.names(&header) {
                    return Err(GenFault::WrongHeader(g));
                }
                header
                    .validate()
                    .map_err(|e| GenFault::BadHeader(g, e.to_string()))?;
                match usable(&header) {
                    Ok(_) => {
                        return Ok(GenChoice {
                            used: Some(UsedGeneration {
                                generation: g,
                                id,
                                root: leaf.root,
                                header,
                            }),
                            skipped,
                        })
                    }
                    Err(u) => GenSkip::Unusable(u),
                }
            }
        };
        skipped.push((g, skip));
    }
    Ok(GenChoice {
        used: None,
        skipped,
    })
}

/// How a catalog lists one id, for [`follow`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    /// Not listed.
    Absent,
    /// Its leaf (live or a tombstone).
    Leaf(CatalogLeaf),
}

/// Where a chain ends (ADR-0059 amendment 6 item 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// The start is not removed: there is no chain.
    NotRemoved {
        /// The start.
        dataset: String,
        /// Its version.
        version: u32,
    },
    /// The last id is live.
    Live {
        /// The live id.
        dataset: String,
        /// Its version in the catalog.
        version: u32,
    },
    /// A tombstone with no link.
    Removed {
        /// The removed id.
        dataset: String,
    },
    /// A tombstone whose last row retracts its link.
    Retracted {
        /// The removed id.
        dataset: String,
        /// The retracting row's number.
        n: i128,
    },
    /// The catalog does not list an id the chain reaches.
    NotListed {
        /// That id.
        dataset: String,
    },
    /// No links generation is usable.
    NoLinks,
    /// An id met twice.
    Cycle {
        /// That id.
        dataset: String,
    },
    /// A link past [`MAX_SUCCESSOR_HOPS`].
    TooLong {
        /// The id whose link would be the next one.
        dataset: String,
    },
}

impl End {
    /// The status word answers and reports use.
    pub fn status(&self) -> &'static str {
        match self {
            End::NotRemoved { .. } => "not_removed",
            End::Live { .. } => "live",
            End::Removed { .. } => "removed",
            End::Retracted { .. } => "retracted",
            End::NotListed { .. } => "not_listed",
            End::NoLinks => "no_links",
            End::Cycle { .. } => "cycle",
            End::TooLong { .. } => "too_long",
        }
    }
}

/// One link followed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    /// The removed id.
    pub from: String,
    /// Its tombstone's version.
    pub version: u32,
    /// The row used.
    pub n: i128,
    /// That row's place among the version's rows.
    pub position: u64,
    /// The id it names.
    pub to: String,
}

/// A chain from an old id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    /// Every link followed, in order.
    pub hops: Vec<Hop>,
    /// Where it ends.
    pub end: End,
}

/// [`follow`] could not walk on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FollowError<E> {
    /// The caller's lookup failed.
    #[error("{0}")]
    Lookup(E),
    /// The lookup answered with the leaf of another id (review SUCC-B1 L3).
    #[error("asked for the leaf of {asked}, got the leaf of {got}")]
    WrongLeaf {
        /// The id asked for.
        asked: String,
        /// The leaf's id.
        got: String,
    },
}

/// Follow links from `start` in one catalog (ADR-0059 amendment 6 item 3).
/// `leaf` answers how the catalog lists an id; a leaf of another id is
/// refused. `links` are the used generation's ([`choose_generation`]), or
/// `None` when no generation is usable. The hop bound comes first: a ninth
/// link ends `too_long` even when it would close a cycle.
pub fn follow<E>(
    start: &str,
    links: Option<&Links>,
    mut leaf: impl FnMut(&str) -> Result<Listing, E>,
) -> Result<Chain, FollowError<E>> {
    let mut hops: Vec<Hop> = Vec::new();
    let mut seen = BTreeSet::from([start.to_string()]);
    let mut cur = start.to_string();
    let end = loop {
        let listing = leaf(&cur).map_err(FollowError::Lookup)?;
        if let Listing::Leaf(l) = &listing {
            if l.dataset != cur {
                return Err(FollowError::WrongLeaf {
                    asked: cur,
                    got: l.dataset.clone(),
                });
            }
        }
        let t = match listing {
            Listing::Absent => break End::NotListed { dataset: cur },
            Listing::Leaf(l) if !l.is_tombstone() => {
                break if hops.is_empty() {
                    End::NotRemoved {
                        dataset: cur,
                        version: l.version,
                    }
                } else {
                    End::Live {
                        dataset: cur,
                        version: l.version,
                    }
                }
            }
            Listing::Leaf(t) => t,
        };
        let Some(links) = links else {
            break End::NoLinks;
        };
        let Some(row) = links.link_of(&t) else {
            break End::Removed { dataset: cur };
        };
        let Some(to) = row.to.clone() else {
            break End::Retracted {
                dataset: cur,
                n: row.n,
            };
        };
        if hops.len() == MAX_SUCCESSOR_HOPS {
            break End::TooLong { dataset: cur };
        }
        hops.push(Hop {
            from: cur,
            version: t.version,
            n: row.n,
            position: row.position,
            to: to.clone(),
        });
        if !seen.insert(to.clone()) {
            break End::Cycle { dataset: to };
        }
        cur = to;
    };
    Ok(Chain { hops, end })
}

/// Links pages that do not make one complete, pinned run. Every variant but
/// [`RunError::Count`], [`RunError::Oversize`], [`RunError::Undersize`] and
/// [`RunError::Unusable`] (a header or entries the publisher signed) is the
/// node's fault ([`RunError::node_fault`]).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// No page.
    #[error("links pages: none")]
    NoPage,
    /// Page `k` is not a valid rows answer.
    #[error("links page {0}: {1}")]
    Page(usize, WireError),
    /// Page `k` is of another manifest root than the pinned one.
    #[error("links page {0} is of another version")]
    OtherRoot(usize),
    /// More pages than the rows need.
    #[error("{pages} links pages for {rows} rows")]
    TooManyPages {
        /// Pages sent.
        pages: usize,
        /// The version's rows.
        rows: u64,
    },
    /// A page of more than [`LINKS_PAGE_ROWS`] rows, refused before it is
    /// checked.
    #[error("links page {page} holds {rows} rows, more than {LINKS_PAGE_ROWS}")]
    PageTooLarge {
        /// The page.
        page: usize,
        /// Its rows.
        rows: usize,
    },
    /// An empty page in a version with rows.
    #[error("links page {0} is empty")]
    EmptyPage(usize),
    /// A row that is not the one right after the row before it (a gap, an
    /// overlap, or a first row that is not shard 0, row 0).
    #[error("links page {page} row {row} is shard {} row {}, not shard {} row {}", .got.0, .got.1, .want.0, .want.1)]
    Join {
        /// The page.
        page: usize,
        /// The row within the page.
        row: usize,
        /// `(shard, index)` sent.
        got: (u32, u64),
        /// `(shard, index)` the run needs there.
        want: (u32, u64),
    },
    /// The run stops before the last row of the last shard.
    #[error("the links run stops after {rows} rows, before the last row")]
    Short {
        /// Rows sent.
        rows: u64,
    },
    /// The run reaches the last row, but the header's `row_count` is
    /// another number: an inconsistent header the publisher signed, not the
    /// node's fault.
    #[error("the links run has {rows} rows, the header says {of}")]
    Count {
        /// Rows in the run.
        rows: u64,
        /// The header's `row_count`.
        of: u64,
    },
    /// The rows of a shard take more bytes than its entry says the file
    /// holds: the rows are proven, so the entry the publisher signed is false
    /// (review SUCC-B5 H1). Checked as rows come, so a reader never takes
    /// more than the header's `byte_count`.
    #[error("links shard {shard} sends more than its {of} bytes")]
    Oversize {
        /// The shard.
        shard: u32,
        /// Its entry's `bytes`.
        of: u64,
    },
    /// The rows of a shard, with its framing, take fewer bytes than its
    /// entry says the file holds: proven rows, so the entry the publisher
    /// signed is false (a node holds no such file and serves none: review
    /// SUCC-B6 L3). Checked at the shard's last row.
    #[error("links shard {shard} sends {bytes} bytes, not its {of}")]
    Undersize {
        /// The shard.
        shard: u32,
        /// Its rows with their framing.
        bytes: u64,
        /// Its entry's `bytes`.
        of: u64,
    },
    /// Page `page`'s rows, by their length as sent, take more bytes than
    /// the pinned header's `byte_count` has left: refused before any row of
    /// it is checked (review SUCC-B6 L3). A node that serves the files of a
    /// valid version never sends more.
    #[error("links page {page} brings the rows to {bytes} bytes, more than {of}")]
    TooManyBytes {
        /// The page.
        page: usize,
        /// The rows' bytes with this page.
        bytes: u64,
        /// The header's `byte_count`.
        of: u64,
    },
    /// The version cannot hold links.
    #[error(transparent)]
    Unusable(#[from] Unusable),
}

impl RunError {
    /// Whether the node that sent the pages is at fault.
    pub fn node_fault(&self) -> bool {
        !matches!(
            self,
            RunError::Count { .. }
                | RunError::Oversize { .. }
                | RunError::Undersize { .. }
                | RunError::Unusable(_)
        )
    }
}

/// Every row of links version `root`, from `pages` (rows answers for pages
/// 0, 1, ... in order), proven one complete run (ADR-0059 amendment 6 item
/// 4). Each page holds at most [`LINKS_PAGE_ROWS`] rows, is a valid answer of
/// manifest `root` whatever catalog it carries (the one that first listed
/// `root`; review SUCC-R2 M1), and is not empty; there are at most
/// `ceil(row_count / LINKS_PAGE_ROWS)` pages; the first row is shard 0, row
/// 0, and every other row is the one right after the row before it, across
/// pages too (the join: without it, pages that skip rows in one place and
/// repeat them in another pass every other check, review SUCC-B1 M1); the
/// last row is the last of shard `shard_count - 1`; the rows add up to
/// `row_count`; the rows of a shard, with its framing, take exactly its
/// entry's `bytes` (reviews SUCC-B5 H1, SUCC-B6 L3), and before a page is
/// checked its rows, by their length as sent, fit in what the pinned
/// header's `byte_count` has left (SUCC-B6 L3: no reader hashes more than
/// the version holds). Checks signatures off chain only: the caller ties
/// `root` to its catalog and reads that on chain.
pub fn read_links_pages(
    root: &Hash32,
    pages: &[Value],
) -> Result<(ManifestHeader, Links), RunError> {
    if pages.is_empty() {
        return Err(RunError::NoPage);
    }
    let mut run: Option<(ManifestHeader, Links)> = None;
    let mut entries: BTreeMap<u32, ShardEntry> = BTreeMap::new();
    let mut prev: Option<(u32, u64)> = None;
    // Row bytes taken per shard so far, and rows with their length framing
    // taken in all.
    let mut taken: BTreeMap<u32, u64> = BTreeMap::new();
    let mut spent: u64 = 0;
    for (k, page) in pages.iter().enumerate() {
        let sent = page["rows"].as_array().map_or(0, Vec::len);
        if sent as u64 > LINKS_PAGE_ROWS {
            return Err(RunError::PageTooLarge {
                page: k,
                rows: sent,
            });
        }
        // Before any row is hashed: the page's rows by their length as sent
        // against what the pinned header has left (review SUCC-B6 L3). A
        // header that is not the pinned one is refused just below.
        let budget = match &run {
            Some((h, _)) => Some(h.byte_count),
            None => pinned_header(page, root).map(|h| h.byte_count),
        };
        if let Some(of) = budget {
            let bytes = spent.saturating_add(sent_row_bytes(page));
            if bytes > of {
                return Err(RunError::TooManyBytes { page: k, bytes, of });
            }
        }
        let c = check_rows_answer(page, None, None).map_err(|e| RunError::Page(k, e))?;
        if c.manifest_root != *root {
            return Err(RunError::OtherRoot(k));
        }
        let (header, links) = match &mut run {
            Some(r) => r,
            None => {
                usable(&c.header)?;
                let links = Links::new(&c.header.schema)?;
                let most = c.header.row_count.div_ceil(LINKS_PAGE_ROWS);
                if pages.len() as u64 > most {
                    return Err(RunError::TooManyPages {
                        pages: pages.len(),
                        rows: c.header.row_count,
                    });
                }
                run.insert((c.header.clone(), links))
            }
        };
        if c.rows.is_empty() {
            return Err(RunError::EmptyPage(k));
        }
        for (i, s) in page["shards"].as_array().into_iter().flatten().enumerate() {
            let e = hex_field(&s["entry"], "entry")
                .ok()
                .and_then(|b| ShardEntry::try_from_slice(&b).ok())
                .ok_or_else(|| RunError::Page(k, WireError::Field(format!("shards[{i}].entry"))))?;
            entries.insert(e.index, e);
        }
        let rows = page["rows"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (j, &(shard, index, _)) in c.rows.iter().enumerate() {
            let want = match prev {
                None => (0, 0),
                Some((s, i)) => match entries.get(&s) {
                    Some(e) if i + 1 < e.rows => (s, i + 1),
                    _ => (s + 1, 0),
                },
            };
            if (shard, index) != want {
                return Err(RunError::Join {
                    page: k,
                    row: j,
                    got: (shard, index),
                    want,
                });
            }
            let bytes = hex_field(&rows[j]["row"], "row").map_err(|e| RunError::Page(k, e))?;
            // A shard file is its row count, then each row's length and
            // bytes: rows 0 to `index` need this much of it.
            let t = taken.entry(shard).or_default();
            *t = t.saturating_add(bytes.len() as u64);
            spent = spent.saturating_add(4 + bytes.len() as u64);
            if let Some(e) = entries.get(&shard) {
                let need = t.saturating_add(4 + 4 * (index + 1));
                if need > e.bytes {
                    return Err(RunError::Oversize { shard, of: e.bytes });
                }
                // The last row: the file is exactly its entry's size.
                if index + 1 == e.rows && need != e.bytes {
                    return Err(RunError::Undersize {
                        shard,
                        bytes: need,
                        of: e.bytes,
                    });
                }
            }
            let from = header.schema_of(shard);
            let cells = decode_row(from, &bytes)
                .map_err(|e| RunError::Page(k, WireError::False(e.to_string())))?;
            links.push(&present(from, &header.schema, &cells))?;
            prev = Some((shard, index));
        }
    }
    let (header, links) = run.ok_or(RunError::NoPage)?;
    let at_last = match (prev, header.shard_count.checked_sub(1)) {
        (Some((s, i)), Some(last)) => s == last && entries.get(&s).is_some_and(|e| i + 1 == e.rows),
        _ => false,
    };
    if !at_last {
        return Err(RunError::Short { rows: links.rows() });
    }
    if links.rows() != header.row_count {
        return Err(RunError::Count {
            rows: links.rows(),
            of: header.row_count,
        });
    }
    Ok((header, links))
}

/// The header of rows answer `page` when it is the one `root` pins
/// (decoded only, no row checked).
fn pinned_header(page: &Value, root: &Hash32) -> Option<ManifestHeader> {
    let b = hex_field(&page["manifest"]["header"], "manifest.header").ok()?;
    let h = ManifestHeader::try_from_slice(&b).ok()?;
    (h.root() == *root).then_some(h)
}

/// The bytes of `page`'s rows with their 4-byte length framing, by the
/// length of each row's hex as sent (nothing decoded).
fn sent_row_bytes(page: &Value) -> u64 {
    page["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let h = r["row"].as_str().unwrap_or_default();
            4 + (h.strip_prefix("0x").unwrap_or(h).len() / 2) as u64
        })
        .fold(0, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor;
    use crate::catalog::{self, Catalog};
    use crate::manifest::tests::rights;
    use crate::manifest::{shards_tree, Manifest, NewVersion};
    use crate::row::encode_row;
    use crate::shard::{check_shard, encode_shard};
    use crate::wire::{catalog_json, hex0x, row_json, shard_json};
    use onyx_core::SourceId;
    use serde_json::json;

    const SECRET: [u8; 32] = [0x33; 32];
    const PUB: &str = "example.publisher";

    fn root_hex(b: u8) -> String {
        Hash32([b; 32]).to_string()
    }

    fn row(n: i128, from: &str, v: i128, prev: &str, to: Option<&str>) -> Vec<Cell> {
        vec![
            Cell::Int(n),
            Cell::Text(from.into()),
            Cell::Int(v),
            Cell::Text(prev.into()),
            to.map_or(Cell::Null, |t| Cell::Text(t.into())),
        ]
    }

    fn leaf(d: &str, version: u32, prev: u8, root: u8) -> CatalogLeaf {
        CatalogLeaf {
            publisher: SourceId(PUB.into()),
            dataset: d.into(),
            version,
            prev: if prev == 0 {
                Hash32::ZERO
            } else {
                Hash32([prev; 32])
            },
            root: if root == 0 {
                Hash32::ZERO
            } else {
                Hash32([root; 32])
            },
        }
    }

    fn links_of(rows: &[Vec<Cell>]) -> Links {
        let mut l = Links::new(&links_schema()).expect("readable");
        for r in rows {
            l.push(r).expect("under the cap");
        }
        l
    }

    fn lookup(leaves: &[CatalogLeaf]) -> impl FnMut(&str) -> Result<Listing, ()> + '_ {
        |id| {
            Ok(leaves
                .iter()
                .find(|l| l.dataset == id)
                .map_or(Listing::Absent, |l| Listing::Leaf(l.clone())))
        }
    }

    #[test]
    fn cat_succ_the_links_schema_is_read_by_name_and_bad_rows_are_inert() {
        let s = links_schema();
        s.validate().expect("the tool's schema is valid");
        let cols = read_columns(&s).expect("readable");
        // Another order, an appended column: still readable by name.
        let mut other = Schema::from_json(&json!({"columns":[
            {"name":"from","kind":"text"},{"name":"n","kind":"int"},
            {"name":"to","kind":"text","nullable":true},{"name":"from_prev","kind":"text"},
            {"name":"from_version","kind":"int"},{"name":"note","kind":"text","nullable":true}],"key":"n"}))
        .expect("ok");
        read_columns(&other).expect("readable by name");
        // from_version widened to decimal, to not nullable, n not the key.
        other.columns[4].kind = ColumnKind::Decimal { scale: 2 };
        assert!(matches!(read_columns(&other), Err(Unusable::Schema(_))));
        let mut strict = links_schema();
        strict.columns[4].nullable = false;
        assert!(matches!(read_columns(&strict), Err(Unusable::Schema(_))));
        let mut keyed = links_schema();
        keyed.columns[2].nullable = false;
        keyed.key = 2;
        assert!(matches!(read_columns(&keyed), Err(Unusable::Schema(_))));
        let mut gone = links_schema();
        gone.columns.pop();
        assert!(matches!(read_columns(&gone), Err(Unusable::Schema(_))));

        let ok = row(1, "example.flows", 3, &root_hex(7), Some("example.flows-2"));
        let r = parse_row(&cols, &ok, 0).expect("valid");
        assert_eq!(r.from_prev, Hash32([7; 32]));
        assert_eq!(r.to.as_deref(), Some("example.flows-2"));
        assert!(
            parse_row(&cols, &row(2, "example.flows", 3, &root_hex(7), None), 1)
                .expect("a retraction is valid")
                .to
                .is_none()
        );
        let upper = root_hex(0xab).replace("ab", "AB");
        let max = i128::from(u32::MAX);
        for bad in [
            row(1, "example.flows", 3, &upper, Some("b.x")), // upper-case hex
            row(1, "example.flows", 3, "0x07", Some("b.x")), // short
            row(1, "example.flows", 3, &root_hex(0), Some("b.x")), // zero root
            row(1, "example.flows", 3, &root_hex(7)[2..], Some("b.x")), // no 0x
            row(1, "example.flows", 1, &root_hex(7), Some("b.x")), // version 1
            row(1, "example.flows", max + 1, &root_hex(7), Some("b.x")), // past u32
            row(1, "onyx.successors.2", 3, &root_hex(7), Some("b.x")), // a links id
            row(1, "example.flows", 3, &root_hex(7), Some("onyx.successors")),
            row(1, "example.flows", 3, &root_hex(7), Some("example.flows")), // itself
            row(1, "Example Flows", 3, &root_hex(7), Some("b.x")),           // not dotted
            row(1, "example.flows", 3, &root_hex(7), Some("B")),
        ] {
            assert_eq!(parse_row(&cols, &bad, 0), None, "{bad:?}");
        }
        assert!(parse_row(&cols, &row(1, "a.b", max, &root_hex(7), Some("b.x")), 0).is_some());
        assert!(is_links_id("onyx.successors") && is_links_id("onyx.successors.8"));
        assert!(!is_links_id("onyx.other") && !is_links_id("example.successors"));
        assert_eq!(generation_id(1).as_deref(), Some("onyx.successors"));
        assert_eq!(generation_id(8).as_deref(), Some("onyx.successors.8"));
        assert_eq!(generation_id(0), None);
        assert_eq!(generation_id(9), None);
        for g in 1..=MAX_LINK_GENERATIONS {
            let id = generation_id(g).expect("in range");
            assert!(is_dotted_id(&id) && is_links_id(&id));
        }
    }

    #[test]
    fn cat_succ_the_last_row_for_a_tombstone_wins_and_a_null_target_retracts() {
        let t = leaf("example.flows", 3, 7, 0);
        let mut l = links_of(&[
            row(1, "example.flows", 3, &root_hex(7), Some("example.typo")),
            row(2, "example.flows", 3, &root_hex(7), Some("example.flows-2")),
            // Another tombstone of the same id (one set aside, a guess):
            // ignored for this one.
            row(3, "example.flows", 3, &root_hex(8), Some("example.other")),
            row(4, "example.flows", 4, &root_hex(7), Some("example.other")),
            // An invalid row never overrides.
            row(5, "example.flows", 3, &root_hex(7), Some("onyx.successors")),
        ]);
        let r = l.link_of(&t).expect("linked");
        assert_eq!((r.n, r.position), (2, 1));
        assert_eq!(r.to.as_deref(), Some("example.flows-2"));
        assert_eq!(l.rows(), 5);
        // A live leaf has no link, whatever the rows say.
        assert_eq!(l.link_of(&leaf("example.flows", 3, 7, 9)), None);
        // A later null target retracts it.
        l.push(&row(6, "example.flows", 3, &root_hex(7), None))
            .expect("ok");
        assert_eq!(l.link_of(&t).expect("a row").to, None);
        assert_eq!(l.latest().count(), 3);
    }

    #[test]
    fn cat_succ_a_chain_follows_a_successor_that_gained_versions_and_moved_on() {
        // A was moved to B at B's version 1; B then gained versions and
        // was itself moved at version 7 to C (review SUCC-R1 M2).
        let leaves = [
            leaf("a.old", 2, 1, 0),
            leaf("b.mid", 7, 6, 0),
            leaf("c.new", 3, 2, 3),
        ];
        let l = links_of(&[
            row(1, "a.old", 2, &root_hex(1), Some("b.mid")),
            row(2, "b.mid", 7, &root_hex(6), Some("c.new")),
        ]);
        let c = follow("a.old", Some(&l), lookup(&leaves)).expect("ok");
        assert_eq!(
            c.end,
            End::Live {
                dataset: "c.new".into(),
                version: 3
            }
        );
        assert_eq!(
            c.hops
                .iter()
                .map(|h| (h.from.as_str(), h.n))
                .collect::<Vec<_>>(),
            vec![("a.old", 1), ("b.mid", 2)]
        );
    }

    #[test]
    fn cat_succ_every_end_of_a_chain() {
        let ends = |leaves: &[CatalogLeaf], rows: &[Vec<Cell>]| {
            follow("a.old", Some(&links_of(rows)), lookup(leaves))
                .expect("ok")
                .end
        };
        let a = leaf("a.old", 2, 1, 0);
        assert_eq!(
            ends(&[leaf("a.old", 1, 0, 1)], &[]),
            End::NotRemoved {
                dataset: "a.old".into(),
                version: 1
            }
        );
        assert_eq!(
            ends(&[], &[]),
            End::NotListed {
                dataset: "a.old".into()
            }
        );
        assert_eq!(
            ends(std::slice::from_ref(&a), &[]),
            End::Removed {
                dataset: "a.old".into()
            }
        );
        assert_eq!(
            ends(
                std::slice::from_ref(&a),
                &[row(9, "a.old", 2, &root_hex(1), None)]
            ),
            End::Retracted {
                dataset: "a.old".into(),
                n: 9
            }
        );
        assert_eq!(
            ends(
                std::slice::from_ref(&a),
                &[row(1, "a.old", 2, &root_hex(1), Some("b.gone"))]
            ),
            End::NotListed {
                dataset: "b.gone".into()
            }
        );
        // A successor removed later with no row of its own.
        assert_eq!(
            ends(
                &[a.clone(), leaf("b.mid", 4, 5, 0)],
                &[row(1, "a.old", 2, &root_hex(1), Some("b.mid"))]
            ),
            End::Removed {
                dataset: "b.mid".into()
            }
        );
        assert_eq!(
            follow("a.old", None, lookup(std::slice::from_ref(&a)))
                .expect("ok")
                .end,
            End::NoLinks
        );
        // A to B, B to A.
        let cyc = ends(
            &[a.clone(), leaf("b.mid", 2, 5, 0)],
            &[
                row(1, "a.old", 2, &root_hex(1), Some("b.mid")),
                row(2, "b.mid", 2, &root_hex(5), Some("a.old")),
            ],
        );
        assert_eq!(
            cyc,
            End::Cycle {
                dataset: "a.old".into()
            }
        );
        // A chain of exactly 8 links ends live; one of 9 is too long.
        let chain = |links: usize| {
            let ids: Vec<String> = (0..=links).map(|i| format!("d.n{i}")).collect();
            let mut leaves: Vec<CatalogLeaf> =
                ids[..links].iter().map(|d| leaf(d, 2, 1, 0)).collect();
            leaves.push(leaf(&ids[links], 1, 0, 2));
            let rows: Vec<Vec<Cell>> = (0..links)
                .map(|i| row(i as i128 + 1, &ids[i], 2, &root_hex(1), Some(&ids[i + 1])))
                .collect();
            follow("d.n0", Some(&links_of(&rows)), lookup(&leaves)).expect("ok")
        };
        let eight = chain(MAX_SUCCESSOR_HOPS);
        assert_eq!(eight.hops.len(), MAX_SUCCESSOR_HOPS);
        assert_eq!(
            eight.end,
            End::Live {
                dataset: "d.n8".into(),
                version: 1
            }
        );
        let nine = chain(MAX_SUCCESSOR_HOPS + 1);
        assert_eq!(nine.hops.len(), MAX_SUCCESSOR_HOPS);
        assert_eq!(
            nine.end,
            End::TooLong {
                dataset: "d.n8".into()
            }
        );
        // A ninth link that would close a cycle still ends too_long: the hop
        // bound comes first (review SUCC-B1 L4).
        let ids: Vec<String> = (0..9).map(|i| format!("d.n{i}")).collect();
        let leaves: Vec<CatalogLeaf> = ids.iter().map(|d| leaf(d, 2, 1, 0)).collect();
        let rows: Vec<Vec<Cell>> = (0..9)
            .map(|i| {
                row(
                    i as i128 + 1,
                    &ids[i],
                    2,
                    &root_hex(1),
                    Some(&ids[(i + 1) % 9]),
                )
            })
            .collect();
        let back = follow("d.n0", Some(&links_of(&rows)), lookup(&leaves)).expect("ok");
        assert_eq!(
            back.end,
            End::TooLong {
                dataset: "d.n8".into()
            }
        );
        // Every end's status word (review SUCC-B1 L4).
        let words = [
            (
                End::NotRemoved {
                    dataset: "a.b".into(),
                    version: 1,
                },
                "not_removed",
            ),
            (
                End::Live {
                    dataset: "a.b".into(),
                    version: 1,
                },
                "live",
            ),
            (
                End::Removed {
                    dataset: "a.b".into(),
                },
                "removed",
            ),
            (
                End::Retracted {
                    dataset: "a.b".into(),
                    n: 1,
                },
                "retracted",
            ),
            (
                End::NotListed {
                    dataset: "a.b".into(),
                },
                "not_listed",
            ),
            (End::NoLinks, "no_links"),
            (
                End::Cycle {
                    dataset: "a.b".into(),
                },
                "cycle",
            ),
            (
                End::TooLong {
                    dataset: "a.b".into(),
                },
                "too_long",
            ),
        ];
        for (e, w) in &words {
            assert_eq!(e.status(), *w);
        }
        assert_eq!(eight.end.status(), "live");
        assert_eq!(nine.end.status(), "too_long");
        assert_eq!(cyc.status(), "cycle");
    }

    #[test]
    fn cat_succ_a_leaf_for_another_id_is_refused() {
        // The lookup answers b.mid's leaf when asked for a.old (review
        // SUCC-B1 L3).
        let wrong = |_: &str| Ok::<_, ()>(Listing::Leaf(leaf("b.mid", 2, 1, 0)));
        assert_eq!(
            follow("a.old", Some(&links_of(&[])), wrong),
            Err(FollowError::WrongLeaf {
                asked: "a.old".into(),
                got: "b.mid".into()
            })
        );
        // And on a later hop.
        let leaves = [leaf("a.old", 2, 1, 0), leaf("b.mid", 1, 0, 2)];
        let rows = [row(1, "a.old", 2, &root_hex(1), Some("b.mid"))];
        let mut asked = 0;
        let swap = |id: &str| {
            asked += 1;
            let l = if asked == 2 {
                &leaves[0]
            } else {
                leaves.iter().find(|l| l.dataset == id).expect("listed")
            };
            Ok::<_, ()>(Listing::Leaf(l.clone()))
        };
        assert_eq!(
            follow("a.old", Some(&links_of(&rows)), swap),
            Err(FollowError::WrongLeaf {
                asked: "b.mid".into(),
                got: "a.old".into()
            })
        );
        assert_eq!(
            follow("a.old", None, |_| Err::<Listing, _>("lookup")),
            Err(FollowError::Lookup("lookup"))
        );
    }

    #[test]
    fn cat_succ_effective_links_are_only_rows_that_act_in_the_catalog() {
        let l = links_of(&[
            row(1, "a.old", 2, &root_hex(1), Some("a.new")),
            row(2, "c.old", 2, &root_hex(1), Some("c.new")),
            row(3, "c.old", 2, &root_hex(1), None),
            // Written early, for a tombstone the catalog does not list.
            row(4, "e.live", 5, &root_hex(4), Some("e.new")),
        ]);
        let tombstones = [
            leaf("a.old", 2, 1, 0),
            leaf("c.old", 2, 1, 0),
            leaf("d.old", 2, 1, 0),
            leaf("e.live", 4, 3, 9),
        ];
        let acting: Vec<(&str, Option<&str>)> = l
            .effective(&tombstones)
            .map(|(t, r)| (t.dataset.as_str(), r.to.as_deref()))
            .collect();
        assert_eq!(acting, vec![("a.old", Some("a.new"))]);
        // `latest` holds every named tombstone's last row, acting or not.
        assert_eq!(l.latest().count(), 3);
    }

    #[test]
    fn cat_succ_a_newer_version_must_keep_the_links_columns() {
        let (m, _) = links_version(3, 3);
        let mut l = Links::new(&m.header.schema).expect("ok");
        for i in 0..3 {
            l.push(&row(i + 1, "a.old", 2, &root_hex(1), Some("a.new")))
                .expect("ok");
        }
        let newer = |f: &dyn Fn(&mut ManifestHeader)| {
            let mut h = m.header.clone();
            h.row_count = 5;
            f(&mut h);
            h
        };
        l.accepts(&newer(&|_: &mut ManifestHeader| {}))
            .expect("more rows, same columns");
        // A widening that appends a column keeps the links columns.
        l.accepts(&newer(&|h: &mut ManifestHeader| {
            h.schema.columns.push(Column {
                name: "note".into(),
                kind: ColumnKind::Text,
                nullable: true,
            })
        }))
        .expect("appended column");
        assert!(matches!(
            l.accepts(&newer(&|h: &mut ManifestHeader| h
                .schema
                .columns
                .swap(1, 3))),
            Err(Unusable::NotNewer(_))
        ));
        assert!(matches!(
            l.accepts(&newer(&|h: &mut ManifestHeader| h.schema.columns[2].kind =
                ColumnKind::Decimal { scale: 1 })),
            Err(Unusable::Schema(_))
        ));
        assert!(matches!(
            l.accepts(&newer(&|h: &mut ManifestHeader| h.row_count = 2)),
            Err(Unusable::NotNewer(_))
        ));
        assert_eq!(
            l.accepts(&newer(
                &|h: &mut ManifestHeader| h.row_count = MAX_LINK_ROWS + 1
            )),
            Err(Unusable::TooMany(MAX_LINK_ROWS + 1))
        );
        assert_eq!(
            l.accepts(&newer(&|h: &mut ManifestHeader| h.row_count = 0)),
            Err(Unusable::Empty)
        );
    }

    /// A links version: `per_shard` rows a shard, `rows` rows in all, link
    /// numbers 1, 2, ...; the manifest, its checked shard files.
    fn links_version(rows: u64, per_shard: u64) -> (Manifest, Vec<Vec<u8>>) {
        links_version_of(SUCCESSORS_ID, rows, per_shard)
    }

    /// [`links_version`] of dataset `id`.
    fn links_version_of(id: &str, rows: u64, per_shard: u64) -> (Manifest, Vec<Vec<u8>>) {
        let s = links_schema();
        let all: Vec<Vec<u8>> = (0..rows)
            .map(|i| {
                let n = i128::from(i) + 1;
                let from = format!("d.old{n}");
                let to = format!("d.new{n}");
                encode_row(&s, &row(n, &from, 2, &root_hex(1), Some(&to))).expect("ok")
            })
            .collect();
        let files: Vec<Vec<u8>> = all.chunks(per_shard as usize).map(encode_shard).collect();
        let entries: Vec<ShardEntry> = files
            .iter()
            .enumerate()
            .map(|(i, f)| ShardEntry::new(i as u32, &check_shard(&s, f).expect("ok").summary))
            .collect();
        let m = Manifest::build(
            NewVersion {
                dataset: id.into(),
                publisher: SourceId(PUB.into()),
                created_unix: 1_700_000_000,
                schema: s,
                rights: rights(PUB),
            },
            None,
            entries,
        )
        .expect("ok");
        (m, files)
    }

    /// The rows answer of page `k` (positions `k * size ..`) of `m`,
    /// anchored by its own report, or through `catalog` when given.
    fn page(
        m: &Manifest,
        files: &[Vec<u8>],
        positions: std::ops::Range<u64>,
        catalog: Option<&Catalog>,
    ) -> Value {
        let s = links_schema();
        let checked: Vec<_> = files
            .iter()
            .map(|f| check_shard(&s, f).expect("ok"))
            .collect();
        let tree = shards_tree(&m.shards);
        let mut at = Vec::new();
        let mut p = 0u64;
        for (si, c) in checked.iter().enumerate() {
            for ri in 0..c.rows.len() as u64 {
                if positions.contains(&p) {
                    at.push((si as u32, ri));
                }
                p += 1;
            }
        }
        let shards: BTreeSet<u32> = at.iter().map(|x| x.0).collect();
        let rows: Vec<Value> = at
            .iter()
            .map(|&(si, ri)| {
                let c = &checked[si as usize];
                row_json(
                    &m.header,
                    si,
                    c.rows[ri as usize],
                    &c.tree.proof(ri).expect("ok"),
                )
            })
            .collect();
        let anchor = match catalog {
            None => {
                let r = anchor::sign(&SECRET, &m.header).expect("ok");
                json!({"report": hex0x(&borsh::to_vec(&r).expect("ok"))})
            }
            Some(c) => {
                let r = catalog::sign(&SECRET, &c.header).expect("ok");
                let i = c.find(SUCCESSORS_ID).expect("listed");
                json!({
                    "report": hex0x(&borsh::to_vec(&r).expect("ok")),
                    "catalog": catalog_json(&c.header, &c.leaf_proof(i).expect("ok")),
                })
            }
        };
        json!({
            "manifest": {"root": m.header.root().to_string(),
                         "header": hex0x(&borsh::to_vec(&m.header).expect("ok"))},
            "anchor": anchor,
            "shards": shards.iter().map(|&i| shard_json(&m.shards[i as usize],
                &tree.proof(u64::from(i)).expect("ok"))).collect::<Vec<_>>(),
            "rows": rows,
        })
    }

    #[test]
    fn cat_succ_a_links_version_first_listed_in_an_older_catalog_checks() {
        // The version entered the catalogs in catalog 0; the reader's C is
        // a later catalog that still lists it. Its pages carry catalog 0
        // (the one that first listed it, review SUCC-R2 M1): only the root
        // must be C's.
        let (m, files) = links_version(5, 2);
        let c0 = Catalog::build(
            SourceId(PUB.into()),
            None,
            1_700_000_100,
            vec![CatalogLeaf::of(&m.header), leaf("a.old", 2, 1, 0)],
        )
        .expect("ok");
        let c1 = Catalog::build(
            SourceId(PUB.into()),
            Some(&c0),
            1_700_086_500,
            c0.leaves.clone(),
        )
        .expect("ok");
        let root_in_c = c1.leaves[c1.find(SUCCESSORS_ID).expect("listed")].root;
        let p = page(&m, &files, 0..5, Some(&c0));
        let (h, links) = read_links_pages(&root_in_c, &[p]).expect("checks");
        assert_eq!(h.root(), m.header.root());
        assert_eq!(links.rows(), 5);
        assert_eq!(
            links
                .link_of(&leaf("d.old3", 2, 1, 0))
                .and_then(|r| r.to.as_deref()),
            Some("d.new3")
        );
    }

    #[test]
    fn cat_succ_pages_are_pinned_to_one_root_and_must_reach_the_last_row() {
        // 1,500 rows in shards of 600: two pages of 1,000 and 500.
        let (m, files) = links_version(1_500, 600);
        let root = m.header.root();
        let pg = |r: std::ops::Range<u64>| page(&m, &files, r, None);
        let (p0, p1) = (pg(0..1_000), pg(1_000..1_500));
        let (_, links) = read_links_pages(&root, &[p0.clone(), p1.clone()]).expect("complete");
        assert_eq!(links.rows(), 1_500);
        let err = |pages: &[Value]| read_links_pages(&root, pages).expect_err("refused");
        // Each failure has its own variant, so no check can go unnoticed
        // (review SUCC-B1 M2).
        assert_eq!(
            err(std::slice::from_ref(&p0)),
            RunError::Short { rows: 1_000 }
        );
        assert_eq!(
            err(&[p0.clone(), pg(1_001..1_500)]),
            RunError::Join {
                page: 1,
                row: 0,
                got: (1, 401),
                want: (1, 400)
            }
        );
        assert_eq!(
            err(&[pg(1..1_000), p1.clone()]),
            RunError::Join {
                page: 0,
                row: 0,
                got: (0, 1),
                want: (0, 0)
            }
        );
        assert_eq!(err(&[p0.clone(), pg(0..0)]), RunError::EmptyPage(1));
        assert_eq!(
            err(&[p0.clone(), p1.clone(), pg(1_500..1_500)]),
            RunError::TooManyPages {
                pages: 3,
                rows: 1_500
            }
        );
        // A page past the size, refused before it is checked; and the empty
        // page after it that the cap alone would let through (review SUCC-B1
        // L1).
        let big = RunError::PageTooLarge {
            page: 0,
            rows: 1_500,
        };
        assert_eq!(err(&[pg(0..1_500)]), big);
        assert_eq!(err(&[pg(0..1_500), pg(1_500..1_500)]), big);
        // A page of another version (served instead of the pinned root,
        // review SUCC-R2 M2), and a pin to another root.
        let (m2, files2) = links_version(1_600, 600);
        assert_eq!(
            err(&[p0.clone(), page(&m2, &files2, 1_000..1_500, None)]),
            RunError::OtherRoot(1)
        );
        assert_eq!(
            read_links_pages(&m2.header.root(), &[p0.clone(), p1.clone()]),
            Err(RunError::OtherRoot(0))
        );
        assert_eq!(err(&[]), RunError::NoPage);
        // A forged row is the page's own refusal.
        let mut bad = p1.clone();
        bad["rows"][0]["row"] = json!(hex0x(&[0u8; 8]));
        assert!(matches!(err(&[p0, bad]), RunError::Page(1, _)));
        for e in [
            RunError::NoPage,
            RunError::OtherRoot(0),
            RunError::EmptyPage(1),
            big,
        ] {
            assert!(e.node_fault());
        }
    }

    #[test]
    fn cat_succ_a_gap_and_an_overlap_with_the_right_total_are_refused() {
        // Review SUCC-B1 M1: 2,500 rows in shards of 600; pages of rows
        // 0..1,000, 1,100..2,000 and 1,900..2,500 skip 100 rows and repeat
        // 100. The first and last rows, each page's run, the total (2,500)
        // and the page cap (3) all hold: only the join refuses it.
        let (m, files) = links_version(2_500, 600);
        let pg = |r: std::ops::Range<u64>| page(&m, &files, r, None);
        let pages = [pg(0..1_000), pg(1_100..2_000), pg(1_900..2_500)];
        assert_eq!(
            read_links_pages(&m.header.root(), &pages).expect_err("refused"),
            RunError::Join {
                page: 1,
                row: 0,
                got: (1, 500),
                want: (1, 400)
            }
        );
        let honest = [pg(0..1_000), pg(1_000..2_000), pg(2_000..2_500)];
        let (_, links) = read_links_pages(&m.header.root(), &honest).expect("complete");
        assert_eq!(links.rows(), 2_500);
    }

    #[test]
    fn cat_succ_an_inconsistent_row_count_is_the_publishers() {
        // A header the publisher signed with a `row_count` that is not the
        // sum of its shards' rows (5 rows in shards of 2).
        let (m, files) = links_version(5, 2);
        let with = |count: u64| {
            let mut h = m.clone();
            h.header.row_count = count;
            h
        };
        let (more, less, none) = (with(6), with(4), with(0));
        // Every row sent, the header says 6: the publisher's, not the node's.
        let e = read_links_pages(&more.header.root(), &[page(&more, &files, 0..5, None)])
            .expect_err("refused");
        assert_eq!(e, RunError::Count { rows: 5, of: 6 });
        assert!(!e.node_fault());
        // The header says 4 and 4 rows come, but not the last: only the
        // last-row check refuses it.
        assert_eq!(
            read_links_pages(&less.header.root(), &[page(&less, &files, 0..4, None)]),
            Err(RunError::Short { rows: 4 })
        );
        // A version with no rows holds no links (review SUCC-B1 L4).
        assert_eq!(
            read_links_pages(&none.header.root(), &[page(&none, &files, 0..5, None)]),
            Err(RunError::Unusable(Unusable::Empty))
        );
    }

    #[test]
    fn cat_succ_a_generation_over_the_row_cap_unreadable_or_removed_gives_way_to_the_next() {
        let header_of = |g: u32| {
            let (m, _) = links_version_of(&generation_id(g).expect("in range"), 3, 3);
            m.header
        };
        let live = |h: &ManifestHeader| GenState::Live {
            leaf: CatalogLeaf::of(h),
            header: Some(Box::new(h.clone())),
        };
        let mut unreadable = header_of(2);
        unreadable.schema.columns[2].kind = ColumnKind::Decimal { scale: 2 };
        let mut full = header_of(3);
        full.row_count = MAX_LINK_ROWS + 1;
        let mut empty = header_of(4);
        empty.row_count = 0;
        let good = header_of(6);
        let states = [
            GenState::Removed,
            live(&unreadable),
            live(&full),
            live(&empty),
            GenState::Absent,
            live(&good),
        ];
        let mut asked = Vec::new();
        let c = choose_generation(|g| {
            asked.push(g);
            Ok::<_, ()>(
                states
                    .get(g as usize - 1)
                    .cloned()
                    .unwrap_or(GenState::Absent),
            )
        })
        .expect("decided");
        let used = c.used.expect("generation 6");
        assert_eq!(
            (used.generation, used.id.as_str()),
            (6, "onyx.successors.6")
        );
        assert_eq!(used.root, good.root());
        assert_eq!(
            asked,
            vec![1, 2, 3, 4, 5, 6],
            "nothing above the used one is asked"
        );
        assert_eq!(c.skipped[0], (1, GenSkip::Removed));
        assert!(matches!(
            c.skipped[1],
            (2, GenSkip::Unusable(Unusable::Schema(_)))
        ));
        assert_eq!(
            c.skipped[2],
            (3, GenSkip::Unusable(Unusable::TooMany(MAX_LINK_ROWS + 1)))
        );
        assert_eq!(c.skipped[3], (4, GenSkip::Unusable(Unusable::Empty)));
        assert_eq!(c.skipped[4], (5, GenSkip::Absent));
        // None usable: no links.
        let none = choose_generation(|_| Ok::<_, ()>(GenState::Removed)).expect("decided");
        assert_eq!(none.used, None);
        assert_eq!(none.skipped.len(), MAX_LINK_GENERATIONS as usize);
        // The leaf must be that generation's live leaf, its header the one
        // it names and valid (review SUCC-B1 L2).
        let g1 = header_of(1);
        let at_1 = |s: GenState| choose_generation(|_| Ok::<_, ()>(s.clone()));
        assert_eq!(
            at_1(GenState::Live {
                leaf: CatalogLeaf::of(&g1),
                header: None
            }),
            Err(GenFault::NoHeader(1))
        );
        // Generation 2's leaf offered as generation 1's.
        assert_eq!(at_1(live(&header_of(2))), Err(GenFault::WrongLeaf(1)));
        // A tombstone offered as live.
        let tomb = CatalogLeaf::of(&g1).tombstone().expect("live");
        assert_eq!(
            at_1(GenState::Live {
                leaf: tomb,
                header: Some(Box::new(g1.clone()))
            }),
            Err(GenFault::WrongLeaf(1))
        );
        // A header the leaf does not name: same root field but another
        // version, and another root.
        let mut later = g1.clone();
        later.version = 2;
        later.prev = Hash32([5; 32]);
        assert_eq!(
            at_1(GenState::Live {
                leaf: CatalogLeaf::of(&g1),
                header: Some(Box::new(later))
            }),
            Err(GenFault::WrongHeader(1))
        );
        // A header that breaks its own rules, named by its leaf.
        let mut broken = g1.clone();
        broken.created_unix = 0;
        assert!(matches!(
            at_1(live(&broken)),
            Err(GenFault::BadHeader(1, _))
        ));
        assert_eq!(
            choose_generation(|_| Err::<GenState, _>("lookup")),
            Err(GenFault::Lookup("lookup"))
        );
        // The row cap holds when rows are pushed one by one too.
        let mut l = Links::new(&links_schema()).expect("ok");
        l.rows = MAX_LINK_ROWS;
        assert_eq!(
            l.push(&row(1, "a.b", 2, &root_hex(1), None)),
            Err(Unusable::TooMany(MAX_LINK_ROWS + 1))
        );
        assert_eq!(usable(&full), Err(Unusable::TooMany(MAX_LINK_ROWS + 1)));
    }

    /// Review SUCC-B4: a links version over [`LINKS_MAX_BYTES`] is not
    /// usable (for nodes and readers alike: [`choose_generation`] passes it
    /// over), and every row count the row cap allows fits under it.
    #[test]
    fn cat_succ_a_links_version_over_the_byte_cap_is_unusable() {
        // The widest valid row: two ids of the most bytes, the widest ints,
        // in a shard of its own.
        let id = |c: &str| format!("a.{}", c.repeat(onyx_core::ids::MAX_DOTTED_LEN - 2));
        let cells = row(
            i128::MAX,
            &id("b"),
            i128::from(u32::MAX),
            &root_hex(1),
            Some(&id("c")),
        );
        let cols = read_columns(&links_schema()).expect("readable");
        assert!(parse_row(&cols, &cells, 0).is_some(), "a valid row");
        let widest = encode_row(&links_schema(), &cells).expect("ok");
        let one = encode_shard(&[&widest]).len() as u64;
        assert_eq!(one, 383);
        assert!(MAX_LINK_ROWS * one <= LINKS_MAX_BYTES);
        let (m, _) = links_version(3, 3);
        let mut big = m.header.clone();
        big.byte_count = LINKS_MAX_BYTES + 1;
        assert_eq!(usable(&big), Err(Unusable::TooLarge(LINKS_MAX_BYTES + 1)));
        let mut at = m.header.clone();
        at.byte_count = LINKS_MAX_BYTES;
        assert!(usable(&at).is_ok(), "at the cap: usable");
        let (g2, _) = links_version_of(&generation_id(2).expect("in range"), 3, 3);
        let live = |h: &ManifestHeader| GenState::Live {
            leaf: CatalogLeaf::of(h),
            header: Some(Box::new(h.clone())),
        };
        let states = [live(&big), live(&g2.header)];
        let c = choose_generation(|g| {
            Ok::<_, ()>(
                states
                    .get(g as usize - 1)
                    .cloned()
                    .unwrap_or(GenState::Absent),
            )
        })
        .expect("decided");
        assert_eq!(c.used.map(|u| u.generation), Some(2));
        assert_eq!(
            c.skipped,
            vec![(
                1,
                GenSkip::Unusable(Unusable::TooLarge(LINKS_MAX_BYTES + 1))
            )]
        );
    }

    /// Review SUCC-B5 M1: a links version of more than [`MAX_LINK_SHARDS`]
    /// shards is not usable (a node loads each shard file once a job), and
    /// the next generation holds the links.
    #[test]
    fn cat_succ_a_links_version_over_the_shard_cap_is_unusable() {
        let (m, _) = links_version(3, 3);
        let mut many = m.header.clone();
        many.shard_count = MAX_LINK_SHARDS + 1;
        assert_eq!(
            usable(&many),
            Err(Unusable::TooManyShards(MAX_LINK_SHARDS + 1))
        );
        let mut at = m.header.clone();
        at.shard_count = MAX_LINK_SHARDS;
        assert!(usable(&at).is_ok(), "at the cap: usable");
        let (g2, _) = links_version_of(&generation_id(2).expect("in range"), 3, 3);
        let live = |h: &ManifestHeader| GenState::Live {
            leaf: CatalogLeaf::of(h),
            header: Some(Box::new(h.clone())),
        };
        let states = [live(&many), live(&g2.header)];
        let c = choose_generation(|g| {
            Ok::<_, ()>(
                states
                    .get(g as usize - 1)
                    .cloned()
                    .unwrap_or(GenState::Absent),
            )
        })
        .expect("decided");
        assert_eq!(c.used.map(|u| u.generation), Some(2));
    }

    /// Review SUCC-B5 H1: entries that name one shard file with fewer bytes
    /// than it holds (one root may be named again, and one-key ranges may
    /// repeat) make a small header for big rows; a reader stops at the first
    /// row past its entry's bytes, and the fault is the publisher's. Here the
    /// header's total is true (shard 1 claims what shard 0 lacks), so only
    /// the shard check sees it.
    #[test]
    fn cat_succ_a_reader_takes_no_more_rows_than_the_entries_hold() {
        let (file, version) = four_entries_of_one_file();
        let t = file.len() as u64;
        let files = vec![file.clone(); 4];
        // Honest entries: every row is read.
        let honest = version(&[t; 4]);
        let p = page(&honest, &files, 0..12, None);
        let (_, links) = read_links_pages(&honest.header.root(), &[p]).expect("complete");
        assert_eq!(links.rows(), 12);
        // Shard 0 claims the fewest bytes three rows allow.
        let small = version(&[16, 2 * t - 16, t, t]);
        assert_eq!(small.header.byte_count, honest.header.byte_count);
        assert!(usable(&small.header).is_ok(), "the header alone passes");
        let p = page(&small, &files, 0..12, None);
        let e = read_links_pages(&small.header.root(), &[p]).expect_err("refused");
        assert_eq!(e, RunError::Oversize { shard: 0, of: 16 });
        assert!(!e.node_fault(), "the publisher signed the entries");
    }

    /// Four entries that name one file of three 1 KB rows, all of key 1, and
    /// a links version of them with the given `bytes` per entry.
    #[allow(clippy::type_complexity)]
    fn four_entries_of_one_file() -> (Vec<u8>, Box<dyn Fn(&[u64; 4]) -> Manifest>) {
        let s = links_schema();
        let fat: Vec<Vec<u8>> = (0..3)
            .map(|_| {
                encode_row(&s, &row(1, &"x".repeat(1_000), 2, &root_hex(1), None)).expect("ok")
            })
            .collect();
        let file = encode_shard(&fat);
        let sum = check_shard(&s, &file).expect("ok").summary;
        let version = move |bytes: &[u64; 4]| {
            let entries: Vec<ShardEntry> = (0..4)
                .map(|i| {
                    let mut e = ShardEntry::new(i, &sum);
                    e.bytes = bytes[i as usize];
                    e
                })
                .collect();
            Manifest::build(
                NewVersion {
                    dataset: SUCCESSORS_ID.into(),
                    publisher: SourceId(PUB.into()),
                    created_unix: 1_700_000_000,
                    schema: s.clone(),
                    rights: rights(PUB),
                },
                None,
                entries,
            )
            .expect("ok")
        };
        (file, Box::new(version))
    }

    /// Review SUCC-B6 L3: before a page's rows are hashed, their length as
    /// sent is held against what the pinned header's `byte_count` has left:
    /// entries that all claim the fewest bytes (a 64-byte header for 12 KB
    /// of rows) are refused at page 0 with no row checked, the node's fault
    /// (a node that holds the files serves none of them). Garbage rows of
    /// that size are refused the same way, not after their proofs.
    #[test]
    fn cat_succ_a_reader_hashes_no_more_than_the_header_holds() {
        let (file, version) = four_entries_of_one_file();
        let files = vec![file.clone(); 4];
        let small = version(&[16; 4]);
        let p = page(&small, &files, 0..12, None);
        let e = read_links_pages(&small.header.root(), &[p]).expect_err("refused");
        assert!(
            matches!(e, RunError::TooManyBytes { page: 0, of: 64, bytes } if bytes > 64),
            "{e:?}"
        );
        assert!(e.node_fault());
        // An honest version, and a page whose first row is noise as long as
        // the whole version.
        let t = file.len() as u64;
        let honest = version(&[t; 4]);
        let mut p = page(&honest, &files, 0..12, None);
        p["rows"][0]["row"] = json!(format!("0x{}", "ab".repeat(4 * t as usize)));
        let e = read_links_pages(&honest.header.root(), &[p]).expect_err("refused");
        assert!(matches!(e, RunError::TooManyBytes { page: 0, .. }), "{e:?}");
        // A later page is held against what the pages before it took.
        let (m, files) = links_version(1_500, 600);
        let left = m.header.byte_count;
        let p0 = page(&m, &files, 0..1_000, None);
        let mut p1 = page(&m, &files, 1_000..1_500, None);
        assert!(read_links_pages(&m.header.root(), &[p0.clone(), p1.clone()]).is_ok());
        p1["rows"][0]["row"] = json!(format!("0x{}", "ab".repeat(left as usize / 2)));
        let e = read_links_pages(&m.header.root(), &[p0, p1]).expect_err("refused");
        assert!(matches!(e, RunError::TooManyBytes { page: 1, .. }), "{e:?}");
    }

    /// Review SUCC-B6 L3: entries that claim more bytes than their rows take
    /// (a node holds no such file, so it shows `pending` everywhere) are the
    /// publisher's fault at the shard's last row, not a complete run.
    #[test]
    fn cat_succ_a_reader_refuses_a_shard_smaller_than_its_entry() {
        let (file, version) = four_entries_of_one_file();
        let t = file.len() as u64;
        let files = vec![file.clone(); 4];
        let big = version(&[t, t + 1, t, t]);
        assert!(usable(&big.header).is_ok());
        let p = page(&big, &files, 0..12, None);
        let e = read_links_pages(&big.header.root(), &[p]).expect_err("refused");
        assert_eq!(
            e,
            RunError::Undersize {
                shard: 1,
                bytes: t,
                of: t + 1
            }
        );
        assert!(!e.node_fault(), "the publisher signed the entries");
    }

    /// Review SUCC-B4 H1: the links kept for one catalog (one row at most
    /// per tombstone it lists) answer every chain of that catalog as every
    /// row does.
    #[test]
    fn cat_succ_the_links_kept_for_a_catalog_answer_as_every_row_does() {
        let l = links_of(&[
            row(1, "a.old", 2, &root_hex(1), Some("b.old")),
            row(2, "b.old", 2, &root_hex(1), Some("c.new")),
            row(3, "d.old", 2, &root_hex(1), Some("x.new")),
            row(4, "d.old", 2, &root_hex(1), None),
            // For tombstones this catalog does not list.
            row(5, "e.old", 2, &root_hex(2), Some("e.new")),
            row(6, "a.old", 3, &root_hex(5), Some("z.new")),
        ]);
        let leaves = [
            leaf("a.old", 2, 1, 0),
            leaf("b.old", 2, 1, 0),
            leaf("c.new", 1, 0, 7),
            leaf("d.old", 2, 1, 0),
            leaf("e.old", 2, 1, 0),
        ];
        let tombs: Vec<&CatalogLeaf> = leaves.iter().filter(|t| t.is_tombstone()).collect();
        let kept = l.only_for(tombs.iter().copied());
        assert_eq!((l.kept(), kept.kept()), (5, 3));
        assert_eq!(kept.rows(), l.rows());
        for t in &tombs {
            assert_eq!(kept.link_of(t), l.link_of(t));
        }
        for start in ["a.old", "b.old", "c.new", "d.old", "e.old", "f.none"] {
            assert_eq!(
                follow(start, Some(&kept), lookup(&leaves)),
                follow(start, Some(&l), lookup(&leaves)),
                "{start}"
            );
        }
        assert_eq!(
            follow("a.old", Some(&kept), lookup(&leaves))
                .expect("walk")
                .end,
            End::Live {
                dataset: "c.new".into(),
                version: 1
            }
        );
    }

    #[test]
    fn cat_succ_an_absent_id_is_proven_by_its_neighbours_and_in_an_empty_catalog() {
        let three = |r: u8| {
            Catalog::build(
                SourceId(PUB.into()),
                None,
                1_700_000_100,
                vec![
                    leaf("b.two", 1, 0, r),
                    leaf("d.four", 1, 0, 4),
                    leaf("f.six", 1, 0, 6),
                ],
            )
            .expect("ok")
        };
        let c = three(2);
        let h = &c.header;
        for id in ["a.one", "c.three", "e.five", "g.seven"] {
            let p = c.absence_proof(id).expect("absent");
            h.proves_absent(id, &p).expect("proven");
        }
        assert_eq!(c.absence_proof("d.four"), None);
        let tree = catalog::leaves_tree(&c.leaves);
        let at = |i: usize| Some((c.leaves[i].clone(), tree.proof(i as u64).expect("ok")));
        let proof = |left, right| catalog::AbsenceProof { left, right };
        let refused = |id: &str, p: catalog::AbsenceProof| {
            assert!(h.proves_absent(id, &p).is_err(), "{id}: {p:?}");
        };
        // Two-sided: the gap of c.three does not prove d.four or e.five.
        let gap = c.absence_proof("c.three").expect("absent");
        refused("d.four", gap.clone());
        refused("e.five", gap);
        // Neighbours that are not adjacent (b.two and f.six), and swapped.
        refused("d.four", proof(at(0), at(2)));
        refused("c.three", proof(at(1), at(0)));
        // A leaf that is not the last used as the last edge: it would
        // "prove" the listed d.four absent (review SUCC-B1 M3).
        refused("d.four", proof(at(0), None));
        // The last edge for an id before the last leaf.
        refused("e.five", proof(at(2), None));
        // The first edge for an id after the first leaf.
        refused("c.three", proof(None, at(0)));
        // A leaf that is not the first used as the first edge.
        refused("a.one", proof(None, at(1)));
        // An empty proof for a catalog with leaves.
        refused("c.three", proof(None, None));
        // A foreign catalog with the same leaf count: its paths do not hold
        // here.
        let foreign = three(9);
        refused("c.three", foreign.absence_proof("c.three").expect("absent"));
        refused("a.one", foreign.absence_proof("a.one").expect("absent"));
        // A catalog with no leaves: only the empty proof (review SUCC-R2 L2).
        let empty = Catalog::build(SourceId(PUB.into()), None, 1_700_000_100, vec![]).expect("ok");
        let p = empty.absence_proof("a.one").expect("absent");
        assert_eq!(p, proof(None, None));
        empty.header.proves_absent("a.one", &p).expect("proven");
        let some = c.absence_proof("a.one").expect("absent");
        assert!(empty.header.proves_absent("a.one", &some).is_err());
    }
}
