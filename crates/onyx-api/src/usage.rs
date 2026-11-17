//! Usage counts with nothing about visitors (docs/READINESS.md A9; the
//! design is docs/spec/usage-counts.md, the threats THREATS.md USE-1..USE-7
//! and review USE-R1).
//!
//! The node keeps no access log. This module counts requests instead: per
//! UTC day, per route template and status class, and for the bulk dataset
//! info route and the first page of rows per dataset the node holds. One
//! request adds 1 to at most two numbers. Nothing of the request itself is
//! read: no address, no header, no query text, no time within the day, no id.
//!
//! Counting is [`counted`], a layer over the whole router. The counts live
//! under one short lock that no `await` crosses; [`Usage::flush`] writes one
//! small file per day, atomically. There is no route that reads them: the
//! operator reads the files on the host ([`read_report`], `onyx-node usage`).

use crate::Clock;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use onyx_core::feed::{civil_date, day_number};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Dataset keys counted by name per day; a use of any further dataset adds
/// to `datasets_other` (bounded memory and file size, THREATS USE-3).
pub const MAX_DATASETS_PER_DAY: usize = 2_000;
/// Route keys per day; further ones add to the key [`OTHER_ROUTE`]. The
/// node's own templates are far fewer: this only bounds a mistake.
pub const MAX_ROUTES_PER_DAY: usize = 256;
/// Days of files kept; older ones are deleted.
pub const RETENTION_DAYS: u64 = 90;
/// A dataset's count for one day under this is folded into
/// `datasets_other`: in every answer of the read route, and in a finished
/// day's file (THREATS USE-2).
pub const SMALL_COUNT: u64 = 5;
/// The stored file's `version`.
pub const FILE_VERSION: u64 = 1;
/// The key of requests that matched no route (the 404 fallback).
pub const NO_ROUTE: &str = "(no route)";
/// The route key past [`MAX_ROUTES_PER_DAY`].
pub const OTHER_ROUTE: &str = "(other)";
/// The one key of every 405 answer, whatever its method and route: the
/// methods times the templates would otherwise be more keys than
/// [`MAX_ROUTES_PER_DAY`] (review USE-R1 L8).
pub const NOT_ALLOWED: &str = "(method not allowed)";
/// A fold that covers fewer datasets than this drops their counts instead
/// of adding them to `datasets_other`: with one or two, "other" would be
/// those datasets' count under another name (review USE-R1 L1).
pub const MIN_FOLDED: usize = 3;
/// Days in memory at once: today, and days waiting for their last write.
const MAX_DAYS_IN_MEMORY: usize = 3;
/// A day's file is under 1 MiB at the caps; a larger one is not ours.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Set on a bulk response by its handler: the dataset the request named, in
/// the store's own spelling, and only when the store holds it. The layer
/// takes it off again before the response leaves.
#[derive(Clone, Debug)]
pub struct DatasetUse {
    /// The publisher id.
    pub publisher: String,
    /// The dataset id.
    pub dataset: String,
}

/// Counts of one route on one day, by status class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Classes {
    /// Answers under 300.
    #[serde(rename = "2xx")]
    pub s2: u64,
    /// Answers 300 to 399.
    #[serde(rename = "3xx")]
    pub s3: u64,
    /// Answers 400 to 499.
    #[serde(rename = "4xx")]
    pub s4: u64,
    /// Answers 500 and above.
    #[serde(rename = "5xx")]
    pub s5: u64,
}

impl Classes {
    fn add(&mut self, o: &Classes) {
        self.s2 = self.s2.saturating_add(o.s2);
        self.s3 = self.s3.saturating_add(o.s3);
        self.s4 = self.s4.saturating_add(o.s4);
        self.s5 = self.s5.saturating_add(o.s5);
    }

    fn slot(&mut self, status: StatusCode) -> &mut u64 {
        match status.as_u16() {
            0..=299 => &mut self.s2,
            300..=399 => &mut self.s3,
            400..=499 => &mut self.s4,
            _ => &mut self.s5,
        }
    }
}

/// One day's file, `<state dir>/usage/<YYYY-MM-DD>.json`: everything that is
/// stored. Unknown fields are refused when read, so a field cannot be added
/// without changing this struct (and the test that pins its keys).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayFile {
    /// [`FILE_VERSION`].
    pub version: u64,
    /// The UTC day, `YYYY-MM-DD`.
    pub day: String,
    /// `"<METHOD> <route template>"` -> counts by status class.
    pub routes: BTreeMap<String, Classes>,
    /// `"<publisher>/<dataset>"` -> answered (2xx) opens of a dataset the
    /// node held: its info route, or the first page of its rows.
    pub datasets: BTreeMap<String, u64>,
    /// Dataset opens not counted by name: past the cap, or folded for a
    /// small count (three datasets or more at once, [`MIN_FOLDED`]).
    pub datasets_other: u64,
    /// `true` only in the file of a day that is over, whose counts under
    /// [`SMALL_COUNT`] were folded and which nothing was added to since.
    pub small_folded: bool,
}

impl DayFile {
    fn empty(day: u64) -> DayFile {
        DayFile {
            version: FILE_VERSION,
            day: date(day),
            ..DayFile::default()
        }
    }

    /// Strict: the version, the day, key shapes and the caps.
    fn check(&self, day: u64) -> Result<(), String> {
        if self.version != FILE_VERSION {
            return Err(format!("version {} is not {FILE_VERSION}", self.version));
        }
        if self.day != date(day) {
            return Err(format!("day {:?} is not the file's name", self.day));
        }
        if self.routes.len() > MAX_ROUTES_PER_DAY + 1 || self.datasets.len() > MAX_DATASETS_PER_DAY
        {
            return Err("more keys than the caps".into());
        }
        if self.routes.keys().any(|k| k.len() > 512) {
            return Err("a route key is too long".into());
        }
        let dotted = onyx_core::ids::is_dotted_id;
        for k in self.datasets.keys() {
            if !k
                .split_once('/')
                .is_some_and(|(p, d)| dotted(p) && dotted(d))
            {
                return Err("a dataset key is not publisher/dataset".into());
            }
        }
        Ok(())
    }

    /// Add `other`'s counts, inside the caps. `small_folded` is not taken
    /// over: the sum of a folded day and new counts is not a folded day
    /// (review USE-R1 L4).
    fn add(&mut self, other: &DayFile) {
        for (k, c) in &other.routes {
            let key = if self.routes.contains_key(k) || self.routes.len() < MAX_ROUTES_PER_DAY {
                k.as_str()
            } else {
                OTHER_ROUTE
            };
            self.routes.entry(key.to_string()).or_default().add(c);
        }
        for (k, n) in &other.datasets {
            if let Some(have) = self.datasets.get_mut(k) {
                *have = have.saturating_add(*n);
            } else if self.datasets.len() < MAX_DATASETS_PER_DAY {
                self.datasets.insert(k.clone(), *n);
            } else {
                self.datasets_other = self.datasets_other.saturating_add(*n);
            }
        }
        self.datasets_other = self.datasets_other.saturating_add(other.datasets_other);
    }

    /// Take every dataset count under [`SMALL_COUNT`] out of the names. Three
    /// such datasets or more: their sum goes to `datasets_other`. One or
    /// two: their counts are dropped, because "other" would then be those
    /// datasets' own count (review USE-R1 L1).
    fn fold_small(&mut self) {
        let mut folded = 0u64;
        let mut names = 0usize;
        self.datasets.retain(|_, n| {
            let keep = *n >= SMALL_COUNT;
            if !keep {
                folded = folded.saturating_add(*n);
                names += 1;
            }
            keep
        });
        if names >= MIN_FOLDED {
            self.datasets_other = self.datasets_other.saturating_add(folded);
        }
        self.small_folded = true;
    }
}

/// One day in memory.
#[derive(Default)]
struct Day {
    routes: BTreeMap<String, Classes>,
    datasets: BTreeMap<(String, String), u64>,
    datasets_other: u64,
    /// The day is over, was folded, and nothing was counted on it since.
    small_folded: bool,
    /// Counts not yet in the file.
    dirty: bool,
    /// The day's file, if any, was added to these counts: from then on the
    /// memory is the whole day and replaces the file.
    merged: bool,
}

impl Day {
    fn file(&self, day: u64) -> DayFile {
        DayFile {
            version: FILE_VERSION,
            day: date(day),
            routes: self.routes.clone(),
            datasets: self
                .datasets
                .iter()
                .map(|((p, d), n)| (format!("{p}/{d}"), *n))
                .collect(),
            datasets_other: self.datasets_other,
            small_folded: self.small_folded,
        }
    }

    /// Take `f`'s counts; whether they are a folded day is `folded`.
    fn set(&mut self, f: DayFile, folded: bool) {
        self.routes = f.routes;
        self.datasets = f
            .datasets
            .into_iter()
            .filter_map(|(k, n)| {
                let (p, d) = k.split_once('/')?;
                Some(((p.to_string(), d.to_string()), n))
            })
            .collect();
        self.datasets_other = f.datasets_other;
        self.small_folded = folded;
    }
}

#[derive(Default)]
struct Inner {
    days: BTreeMap<u64, Day>,
    /// The day the files were last swept on (old ones deleted, unfinished
    /// past days folded).
    swept: Option<u64>,
}

/// The counters and their files.
pub struct Usage {
    dir: PathBuf,
    clock: Clock,
    inner: Mutex<Inner>,
    /// One flush at a time.
    io: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn date(day: u64) -> String {
    civil_date(i64::try_from(day).unwrap_or(i64::MAX / 2))
}

fn day_path(dir: &Path, day: u64) -> PathBuf {
    dir.join(format!("{}.json", date(day)))
}

/// The method as one of a fixed set of words (never the client's bytes).
fn method_name(m: &Method) -> &'static str {
    match *m {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        Method::PATCH => "PATCH",
        _ => "OTHER",
    }
}

/// Read `day`'s file in `dir`. `Ok(None)`: no file. A file that is not a
/// valid day file is an error. Reads only.
fn read_day(dir: &Path, day: u64) -> Result<Option<DayFile>, String> {
    let path = day_path(dir, day);
    let f = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut text = String::new();
    f.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!(
            "{}: larger than {MAX_FILE_BYTES} bytes",
            path.display()
        ));
    }
    let file: DayFile =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    file.check(day)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(file))
}

/// [`read_day`]; a file that is not a valid day file is first renamed to
/// `<name>.corrupt` (replacing an earlier one), so the next write starts a
/// clean file and the bad bytes stay for a look.
fn read_day_or_set_aside(dir: &Path, day: u64) -> Result<Option<DayFile>, String> {
    read_day(dir, day).map_err(|e| {
        let path = day_path(dir, day);
        let mut aside = path.clone().into_os_string();
        aside.push(".corrupt");
        match std::fs::rename(&path, &aside) {
            Ok(()) => format!("{e}; set aside as {}.json.corrupt", date(day)),
            Err(r) => format!("{e}; not set aside: {r}"),
        }
    })
}

impl Usage {
    /// Counters that keep their files in `dir` (created at the first
    /// write). Nothing is read here: a day's file is added to the counts at
    /// that day's first [`Usage::flush`].
    pub fn new(dir: PathBuf, clock: Clock) -> Usage {
        Usage {
            dir,
            clock,
            inner: Mutex::new(Inner::default()),
            io: Mutex::new(()),
        }
    }

    fn today(&self) -> u64 {
        (self.clock)() / 86_400
    }

    fn path(&self, day: u64) -> PathBuf {
        day_path(&self.dir, day)
    }

    /// Count one answered request: `method` and the matched route's
    /// `template` (`None`: no route matched), the answer's `status`, and the
    /// dataset a bulk handler named. A bounded number of map steps under one
    /// lock; the only strings kept are the server's own route templates and
    /// the ids of datasets it holds.
    pub fn record(
        &self,
        method: &Method,
        template: Option<&str>,
        status: StatusCode,
        dataset: Option<DatasetUse>,
    ) {
        let today = self.today();
        // Every 405 under one key: a caller trying each method on each
        // route cannot use up the route keys.
        let key = if status == StatusCode::METHOD_NOT_ALLOWED {
            NOT_ALLOWED.to_string()
        } else {
            format!("{} {}", method_name(method), template.unwrap_or(NO_ROUTE))
        };
        let mut inner = lock(&self.inner);
        if !inner.days.contains_key(&today) {
            // A flush that keeps failing must not let days pile up: the
            // oldest goes (its unwritten counts are lost).
            while inner.days.len() >= MAX_DAYS_IN_MEMORY {
                inner.days.pop_first();
            }
        }
        let day = inner.days.entry(today).or_default();
        day.dirty = true;
        day.small_folded = false;
        let key = if day.routes.contains_key(&key) || day.routes.len() < MAX_ROUTES_PER_DAY {
            key
        } else {
            OTHER_ROUTE.to_string()
        };
        let slot = day.routes.entry(key).or_default().slot(status);
        *slot = slot.saturating_add(1);
        if let Some(d) = dataset.filter(|_| status.is_success()) {
            let k = (d.publisher, d.dataset);
            if let Some(n) = day.datasets.get_mut(&k) {
                *n = n.saturating_add(1);
            } else if day.datasets.len() < MAX_DATASETS_PER_DAY {
                day.datasets.insert(k, 1);
            } else {
                day.datasets_other = day.datasets_other.saturating_add(1);
            }
        }
    }

    /// Write every day with new counts to its file (temporary file, fsync,
    /// rename), fold and drop finished days, and once a day sweep the
    /// files: delete those older than [`RETENTION_DAYS`] and fold a past
    /// day that was never finished. Blocking file work: call it from
    /// `spawn_blocking`, at most once a minute and at shutdown. `Err` lists
    /// what went wrong (a corrupt file set aside, a failed write); the
    /// counters keep working either way and unwritten counts stay in memory
    /// for the next call.
    pub fn flush(&self) -> Result<(), String> {
        let _one = lock(&self.io);
        let mut problems: Vec<String> = Vec::new();
        if let Err(e) = make_dir(&self.dir) {
            return Err(format!("{}: {e}", self.dir.display()));
        }
        let today = self.today();
        // 1. Days whose file is not yet in the counts: read it (no lock held).
        let unmerged: Vec<u64> = lock(&self.inner)
            .days
            .iter()
            .filter(|(_, d)| !d.merged)
            .map(|(k, _)| *k)
            .collect();
        for day in unmerged {
            let on_disk = match read_day_or_set_aside(&self.dir, day) {
                Ok(f) => f,
                Err(e) => {
                    problems.push(e);
                    None
                }
            };
            let mut inner = lock(&self.inner);
            if let Some(d) = inner.days.get_mut(&day) {
                if let Some(mut whole) = on_disk {
                    whole.add(&d.file(day));
                    // File plus new counts: not a folded day, whatever the
                    // file said (a clock that stepped back into a day that
                    // was already finished).
                    d.set(whole, false);
                }
                d.merged = true;
            }
        }
        // 2. Snapshot what changed. A day that is over is folded, again if
        //    anything was counted on it since its last fold; a live day is
        //    never written as folded.
        let mut out: Vec<(u64, DayFile)> = Vec::new();
        {
            let mut inner = lock(&self.inner);
            for (k, d) in inner.days.iter_mut() {
                let over = *k < today;
                if !d.merged || !(d.dirty || (over && !d.small_folded)) {
                    continue;
                }
                let mut f = d.file(*k);
                if over {
                    f.fold_small();
                    d.set(f.clone(), true);
                } else {
                    f.small_folded = false;
                    d.small_folded = false;
                }
                d.dirty = false;
                out.push((*k, f));
            }
        }
        // 3. Write (no lock held).
        for (day, file) in out {
            if let Err(e) = write_atomic(&self.path(day), &file) {
                problems.push(format!("{}: {e}", self.path(day).display()));
                if let Some(d) = lock(&self.inner).days.get_mut(&day) {
                    d.dirty = true;
                    d.small_folded = false;
                }
            }
        }
        // 4. Finished days that are on disk leave the memory.
        let (sweep, held) = {
            let mut inner = lock(&self.inner);
            inner.days.retain(|k, d| *k >= today || d.dirty);
            let due = inner.swept != Some(today);
            (due, inner.days.keys().copied().collect::<Vec<u64>>())
        };
        if sweep {
            match self.sweep(today, &held) {
                Ok(mut found) => {
                    if found.is_empty() {
                        lock(&self.inner).swept = Some(today);
                    }
                    problems.append(&mut found);
                }
                Err(e) => problems.push(format!("{}: {e}", self.dir.display())),
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    /// The daily sweep of this module's files (`YYYY-MM-DD.json`, also a
    /// `.corrupt` or `.tmp` beside one; other names are left alone). A day
    /// older than the retention is deleted. A past day's file that is not
    /// folded (the node was down when the day ended, review USE-R1 L3) is
    /// folded and written back, unless the day is still in memory (`held`:
    /// its own write does that). Returns what could not be read or written;
    /// the sweep runs again at the next flush until it finds nothing.
    fn sweep(&self, today: u64, held: &[u64]) -> std::io::Result<Vec<String>> {
        let mut problems = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some((stem, rest)) = name.split_at_checked(10) else {
                continue;
            };
            if !matches!(rest, ".json" | ".json.corrupt" | ".json.tmp") {
                continue;
            }
            let Some(day) = day_number(stem).and_then(|d| u64::try_from(d).ok()) else {
                continue;
            };
            if day.saturating_add(RETENTION_DAYS) <= today {
                std::fs::remove_file(entry.path())?;
            } else if rest == ".json" && day < today && !held.contains(&day) {
                match read_day_or_set_aside(&self.dir, day) {
                    Ok(Some(mut f)) if !f.small_folded => {
                        f.fold_small();
                        if let Err(e) = write_atomic(&self.path(day), &f) {
                            problems.push(format!("{}: {e}", self.path(day).display()));
                        }
                    }
                    Ok(_) => {}
                    // Set aside: reported once; the next sweep finds
                    // nothing left of it.
                    Err(e) => problems.push(e),
                }
            }
        }
        Ok(problems)
    }
}

/// The report an operator reads: the `days` UTC days (1 to
/// [`RETENTION_DAYS`]) ending with `until` (a day number), from the day
/// files in `dir` only. Every dataset count under [`SMALL_COUNT`] for a day
/// is taken out of the names, also for a day that is not over. Reads files,
/// writes nothing, asks the node nothing: counts the node has not written
/// yet (at most its last minute) are not in it. Each file is read once and
/// files are replaced whole, so a write by the node during the read cannot
/// double a count (review USE-R1 L5).
pub fn read_report(dir: &Path, until: u64, days: u64) -> Value {
    let days = days.clamp(1, RETENTION_DAYS);
    let first = until.saturating_sub(days - 1);
    let mut by_day = Vec::new();
    let mut unreadable = Vec::new();
    let mut total = DayFile::empty(until);
    for day in first..=until {
        let mut f = match read_day(dir, day) {
            Ok(Some(f)) => f,
            Ok(None) => continue,
            Err(_) => {
                unreadable.push(date(day));
                continue;
            }
        };
        f.fold_small();
        total.add(&f);
        by_day.push(json!({
            "day": f.day,
            "routes": f.routes,
            "datasets": f.datasets,
            "datasets_other": f.datasets_other,
        }));
    }
    json!({
        "version": FILE_VERSION,
        "from": date(first),
        "to": date(until),
        "small_count": SMALL_COUNT,
        "note": "Counts of requests, not of people. A dataset number counts answered opens (its info route, or the first page of its rows). A dataset under small_count for a day is not named; with fewer than three such datasets on a day their opens are left out of datasets_other too. The node's last minute may not be written yet.",
        "unreadable_days": unreadable,
        "totals": {
            "routes": total.routes,
            "datasets": total.datasets,
            "datasets_other": total.datasets_other,
        },
        "days": by_day,
    })
}

/// The folder of the day files, for its owner only.
fn make_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

fn write_atomic(path: &Path, file: &DayFile) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let mut f = o.open(&tmp)?;
        f.write_all(&serde_json::to_vec(file).map_err(std::io::Error::other)?)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// `router` with every answered request counted: the outermost layer, so
/// refusals of the layers inside it (rate limit, overload, deadline) count
/// too. A request whose client goes away before the answer is not counted.
pub fn counted(usage: Arc<Usage>, router: Router) -> Router {
    router.layer(middleware::from_fn_with_state(usage, count))
}

async fn count(State(usage): State<Arc<Usage>>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let template = req.extensions().get::<MatchedPath>().cloned();
    let mut res = next.run(req).await;
    let dataset = res.extensions_mut().remove::<DatasetUse>();
    usage.record(
        &method,
        template.as_ref().map(MatchedPath::as_str),
        res.status(),
        dataset,
    );
    res
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
