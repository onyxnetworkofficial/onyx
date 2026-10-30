//! Level-2 feed templates (L2SRC-R2-F1, docs/spec/proof-ladder.md "Feed
//! templates").
//!
//! A level-2 metric is only as good as the feed that observed it. A round
//! whose spec names a metric but reads another fetcher, host, field or record
//! would otherwise be accepted as that metric. So each level-2 metric has
//! exactly one registered [`FeedTemplate`]: the fetcher id, the https URL
//! template, the selector, the kind, the window length, the observer count,
//! the rights and a [`SubjectRule`]. A round's spec must equal the template
//! instantiated for the spec's own subject and window start, field for field
//! ([`FeedTemplate::matches`]). Registration is a reviewer action recorded in
//! the coordinator (`onyx_coordinator::observe::ObservationResults::register_feed`).
//!
//! Sector-agnostic: a dated subject is `prefix:YYYY-MM-DD`, the URL template
//! holds one `{date}`, and the date must satisfy a [`DateLag`] built from the
//! source's own business calendar (weekends plus a holiday table the adapter
//! supplies). Outside the table's coverage every date is refused (fail
//! closed). A period subject ([`SubjectRule::Period`], SEC-1) is also
//! `prefix:YYYY-MM-DD`, but names any calendar day (a reporting period's end,
//! which may fall on a weekend), puts its `{date}` in the selector (one
//! document lists every period) and is observable only between `min_days` and
//! `max_days` calendar days after that day. A daily subject
//! ([`SubjectRule::Daily`], NS3) is a source that publishes every calendar
//! day: its URL holds `{date}` once or more (a day's start and end), every
//! one filled with the same date, and the day is observable between
//! `min_days` and `max_days` calendar days after it. A bucketed subject
//! ([`SubjectRule::Bucketed`], ADR-0058, contributor loop CL8) names one closed
//! bucket `[start, start + bucket_secs)` of a UTC grid, `prefix:<start, Unix
//! seconds>`: market data from one venue, read only from an endpoint that
//! returns that finished bucket, observable between `min_delay_secs` and
//! `max_delay_secs` after the bucket's end.

use crate::claim::{MetricId, Subject};
use crate::{keccak256, DataRights, Hash32, ObservationKind, ObservationSpec};
use borsh::{BorshDeserialize, BorshSerialize};

/// The placeholder a dated URL template holds exactly once, a period selector
/// once, and a daily URL template one to [`MAX_DAILY_HOLES`] times.
pub const DATE_PLACEHOLDER: &str = "{date}";

/// Seconds per day.
const DAY: u64 = 86_400;

/// Why a template or a spec is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The template itself is malformed.
    #[error("feed template invalid: {0}")]
    BadTemplate(&'static str),
    /// The spec is not the template instantiated for its subject and window.
    #[error("not this metric's registered feed: {0}")]
    NotThisFeed(&'static str),
    /// The subject's date is not observable in this window.
    #[error("date not observable in this window: {0}")]
    DateNotObservable(&'static str),
}

/// When a dated record may be observed, on the source's business calendar.
///
/// A record date `R` may be observed in a window whose UTC day is `W` when:
/// `R` is a business day; at least `min_business_days` business days lie
/// strictly between `R` and `W` (the source publishes `R` on a later business
/// day, so the record is out before the window opens); `W - R` is at most
/// `max_calendar_days`; and every day from `R` to `W` lies inside the
/// calendar's coverage `[first_day, last_day]`.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DateLag {
    /// Business days strictly between the record day and the window's day.
    pub min_business_days: u16,
    /// Most calendar days from the record day to the window's day.
    pub max_calendar_days: u16,
    /// First day (days since 1970-01-01) the holiday table covers.
    pub first_day: i64,
    /// Last day the holiday table covers.
    pub last_day: i64,
    /// Non-weekend days the source does not publish, strictly ascending,
    /// inside `[first_day, last_day]`.
    pub holidays: Vec<i64>,
}

impl DateLag {
    fn validate(&self) -> Result<(), TemplateError> {
        if self.max_calendar_days == 0 || self.first_day > self.last_day {
            return Err(TemplateError::BadTemplate("lag bounds"));
        }
        if !self.holidays.windows(2).all(|w| w[0] < w[1])
            || self
                .holidays
                .iter()
                .any(|d| *d < self.first_day || *d > self.last_day)
        {
            return Err(TemplateError::BadTemplate(
                "holidays must ascend inside the coverage",
            ));
        }
        Ok(())
    }

    /// Whether `day` is a business day: not a Saturday or Sunday and not a
    /// listed holiday. `None` outside the coverage.
    pub fn is_business_day(&self, day: i64) -> Option<bool> {
        if day < self.first_day || day > self.last_day {
            return None;
        }
        // 1970-01-01 was a Thursday: day 0 -> weekday 3 (Mon = 0).
        let weekday = (day + 3).rem_euclid(7);
        Some(weekday < 5 && self.holidays.binary_search(&day).is_err())
    }

    /// Check record day `record` against a window starting at `window_start`.
    pub fn check(&self, record: i64, window_start: u64) -> Result<(), TemplateError> {
        let window_day = i64::try_from(window_start / DAY)
            .map_err(|_| TemplateError::DateNotObservable("window start too large"))?;
        if record < self.first_day || window_day > self.last_day {
            return Err(TemplateError::DateNotObservable(
                "outside the business calendar's coverage",
            ));
        }
        if self.is_business_day(record) != Some(true) {
            return Err(TemplateError::DateNotObservable(
                "the record date is not a business day",
            ));
        }
        let span = window_day - record;
        if span < 1 || span > i64::from(self.max_calendar_days) {
            return Err(TemplateError::DateNotObservable(
                "the record date is too old or not yet past",
            ));
        }
        let between = (record + 1..window_day)
            .filter(|d| self.is_business_day(*d) == Some(true))
            .count();
        if between < usize::from(self.min_business_days) {
            return Err(TemplateError::DateNotObservable(
                "the record is not yet published when the window opens",
            ));
        }
        Ok(())
    }

    /// The first day (days since 1970-01-01) a window for record day `record`
    /// may start on: the earliest day [`DateLag::check`] accepts. A pure
    /// function of the lag and the record date; `None` if no day within
    /// `max_calendar_days` is allowed (L2API-R3-F1).
    pub fn earliest_window_day(&self, record: i64) -> Option<i64> {
        (1..=i64::from(self.max_calendar_days))
            .map(|k| record + k)
            .find(|day| {
                u64::try_from(*day)
                    .ok()
                    .and_then(|d| d.checked_mul(DAY))
                    .is_some_and(|start| self.check(record, start).is_ok())
            })
    }
}

/// How a template's subject is chosen.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum SubjectRule {
    /// Always this subject; the URL template holds no `{date}`. One fact per
    /// window end.
    Fixed(Subject),
    /// `Subject::Aggregate(prefix ":" YYYY-MM-DD)`; the URL template holds
    /// `{date}` once, filled with that date, and the date must satisfy `lag`.
    /// One fact per date, whatever the window (L2SRC-R2-F2).
    Dated {
        /// Dotted aggregate id before `:`.
        prefix: String,
        /// When a date may be observed.
        lag: DateLag,
    },
    /// `Subject::Aggregate(prefix ":" YYYY-MM-DD)` naming a reporting
    /// period's end, any calendar day (SEC-1). The URL holds no `{date}`; the
    /// selector holds it once and is filled with the date. Observable only in
    /// a window whose UTC day is `min_days..=max_days` calendar days after the
    /// period end (every filer's deadline has passed by `min_days`). One fact
    /// per date, whatever the window, like [`SubjectRule::Dated`].
    Period {
        /// Dotted aggregate id before `:`.
        prefix: String,
        /// Fewest calendar days from the period end to the window's day.
        min_days: u16,
        /// Most calendar days from the period end to the window's day.
        max_days: u16,
    },
    /// `Subject::Aggregate(prefix ":" YYYY-MM-DD)` naming any calendar day of
    /// a source that publishes every day (NS3: earthquakes, weather). The URL
    /// holds `{date}` 1..=[`MAX_DAILY_HOLES`] times (a query from the day's
    /// start to its end), every one filled with that date; the selector holds
    /// none. Observable only in a window whose UTC day is
    /// `min_days..=max_days` calendar days after the day (the source's
    /// revisions have settled by `min_days`). One fact per date, whatever the
    /// window, like [`SubjectRule::Dated`].
    Daily {
        /// Dotted aggregate id before `:`.
        prefix: String,
        /// Fewest calendar days from the day to the window's day.
        min_days: u16,
        /// Most calendar days from the day to the window's day.
        max_days: u16,
    },
    /// `Subject::Aggregate(prefix ":" start)` naming one closed bucket
    /// `[start, start + bucket_secs)` of the UTC grid (ADR-0058 item 2,
    /// contributor loop CL8): `start` is canonical decimal Unix seconds, a
    /// multiple of `bucket_secs`. The URL holds [`BUCKET_START_PLACEHOLDER`]
    /// or [`BUCKET_START_MS_PLACEHOLDER`] 1..=[`MAX_BUCKET_HOLES`] times in
    /// all (filled with that start) and no `{date}`; the selector holds none.
    /// Observable only in a window starting `min_delay_secs..=max_delay_secs`
    /// after the bucket's end (late trades and venue corrections settle by
    /// `min_delay_secs`; THREATS CL8-5), a range of at most
    /// [`MAX_BUCKET_RANGE_SECS`] (CL8-6). One fact per bucket, whatever the
    /// window, like [`SubjectRule::Dated`]. Appended last, so no existing
    /// template's Borsh, id or on-chain pin changes.
    Bucketed {
        /// Dotted aggregate id before `:`.
        prefix: String,
        /// The bucket length, one of [`BUCKET_SIZES`].
        bucket_secs: u32,
        /// Fewest seconds from the bucket's end to the window's start.
        min_delay_secs: u32,
        /// Most seconds from the bucket's end to the window's start.
        max_delay_secs: u32,
    },
    /// The later look at a dated, period or daily rule's facts (CL7,
    /// ADR-0057 item 2): the same subjects, URL and selector as `of`,
    /// observable only on the `span_days` UTC days that start `after_days`
    /// after `of`'s last observable day for the subject
    /// ([`FeedTemplate::last_observable_day`]). Made by [`recheck_of`] only.
    /// Appended last, so no existing template id changes.
    Recheck {
        /// The original template's rule (dated, period or daily; never a
        /// recheck: one recheck per value in v1).
        of: Box<SubjectRule>,
        /// Days from the original's last observable day to the first recheck day.
        after_days: u16,
        /// Recheck days, from the first.
        span_days: u16,
    },
}

/// The suffix [`recheck_of`] appends to a template's metric and feed ids.
pub const RECHECK_SUFFIX: &str = ".recheck";

/// The paired recheck template of `t` (CL7, ADR-0057 item 2): a pure function,
/// so anyone recomputes it and compares its id with the one registered for
/// the `.recheck` metric. Same fetcher, URL template, selector, kind, window
/// length, observer count and rights; `metric` and `feed` gain
/// [`RECHECK_SUFFIX`]; the subject rule becomes [`SubjectRule::Recheck`].
/// Refused: a fixed-window template (no "correct later" for a value of one
/// window), a byte (`Exact`) template, and a recheck of a recheck.
pub fn recheck_of(
    t: &FeedTemplate,
    after_days: u16,
    span_days: u16,
) -> Result<FeedTemplate, TemplateError> {
    t.validate()?;
    if matches!(
        t.subject,
        SubjectRule::Fixed(_) | SubjectRule::Bucketed { .. } | SubjectRule::Recheck { .. }
    ) {
        return Err(TemplateError::BadTemplate(
            "only a dated, period or daily template has a recheck",
        ));
    }
    if !matches!(t.kind, ObservationKind::Numeric { .. }) {
        return Err(TemplateError::BadTemplate(
            "only a numeric template has a recheck",
        ));
    }
    let r = FeedTemplate {
        feed: format!("{}{RECHECK_SUFFIX}", t.feed),
        metric: MetricId(format!("{}{RECHECK_SUFFIX}", t.metric.0)),
        subject: SubjectRule::Recheck {
            of: Box::new(t.subject.clone()),
            after_days,
            span_days,
        },
        ..t.clone()
    };
    r.validate()?;
    Ok(r)
}

/// Most `{date}` placeholders a daily URL template holds.
pub const MAX_DAILY_HOLES: usize = 4;

/// The placeholder a bucketed URL holds for the bucket's start in Unix seconds.
pub const BUCKET_START_PLACEHOLDER: &str = "{bucket_start}";
/// The placeholder a bucketed URL holds for the bucket's start in Unix milliseconds.
pub const BUCKET_START_MS_PLACEHOLDER: &str = "{bucket_start_ms}";
/// Most bucket placeholders (of both kinds together) a bucketed URL holds.
pub const MAX_BUCKET_HOLES: usize = 2;
/// The bucket lengths v1 serves: an hour and a day (ADR-0058 item 2; finer
/// buckets wait for batched rounds, its open question 2).
pub const BUCKET_SIZES: [u32; 2] = [3_600, 86_400];
/// The least `min_delay_secs`: a bucket is never read in its first minute
/// after it closes (late prints, THREATS CL8-5).
pub const MIN_BUCKET_DELAY_SECS: u32 = 60;
/// The widest `max_delay_secs - min_delay_secs` (THREATS CL8-6: a coordinator
/// that waits inside the range for a venue's correction has at most six hours).
pub const MAX_BUCKET_RANGE_SECS: u32 = 21_600;
/// The largest `max_delay_secs` (seven days).
pub const MAX_BUCKET_DELAY_SECS: u32 = 604_800;

/// One closed bucket of a bucketed template: `[start, end)` in UTC Unix
/// seconds, end exclusive (ADR-0058 item 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Bucket {
    /// First second of the bucket, a multiple of its length.
    pub start: u64,
    /// First second after the bucket (`start + bucket_secs`).
    pub end: u64,
}

impl Bucket {
    /// The bucket of length `bucket_secs` starting at `start`, if `start` is
    /// on that grid and the end fits.
    pub fn on_grid(start: u64, bucket_secs: u32) -> Option<Bucket> {
        let len = u64::from(bucket_secs);
        if len == 0 || !start.is_multiple_of(len) {
            return None;
        }
        Some(Bucket {
            start,
            end: start.checked_add(len)?,
        })
    }
}

/// The canonical decimal text of a `u64` (`0` or digits without a leading
/// zero), parsed; anything else is `None`.
pub fn canonical_u64(s: &str) -> Option<u64> {
    let ok = !s.is_empty()
        && s.bytes().all(|c| c.is_ascii_digit())
        && (s.len() == 1 || !s.starts_with('0'));
    ok.then(|| s.parse().ok()).flatten()
}

/// Whether a bucket ending at `end` may be observed in a window starting at
/// `window_start`: `min_delay_secs..=max_delay_secs` after the end
/// ([`SubjectRule::Bucketed`]).
fn bucket_check(
    end: u64,
    min_delay_secs: u32,
    max_delay_secs: u32,
    window_start: u64,
) -> Result<(), TemplateError> {
    let first = end
        .checked_add(u64::from(min_delay_secs))
        .ok_or(TemplateError::DateNotObservable("bucket too late"))?;
    let last = end
        .checked_add(u64::from(max_delay_secs))
        .ok_or(TemplateError::DateNotObservable("bucket too late"))?;
    if window_start < first {
        return Err(TemplateError::DateNotObservable(
            "the bucket is not closed and settled yet",
        ));
    }
    if window_start > last {
        return Err(TemplateError::DateNotObservable("the bucket is too old"));
    }
    Ok(())
}

/// The one canonical feed of a level-2 metric.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FeedTemplate {
    /// Feed id the spec carries.
    pub feed: String,
    /// The metric this template is registered for.
    pub metric: MetricId,
    /// Content hash of the one fetcher that may read it.
    pub fetcher: Hash32,
    /// `https://` URL, with `{date}` once for a dated rule, one to
    /// [`MAX_DAILY_HOLES`] times for a daily rule, and none otherwise.
    pub url_template: String,
    /// Fetcher-specific selector.
    pub selector: String,
    /// Value shape and agreement rule.
    pub kind: ObservationKind,
    /// Window length in seconds: `window_end = window_start + window_secs`.
    pub window_secs: u64,
    /// Observers per round.
    pub min_observers: u16,
    /// The source's reviewed rights.
    pub rights: DataRights,
    /// The subject rule.
    pub subject: SubjectRule,
}

/// Whether a period ending on day `record` may be observed in a window
/// starting at `window_start`: the window's UTC day is `min_days..=max_days`
/// calendar days after it ([`SubjectRule::Period`]).
fn period_check(
    record: i64,
    min_days: u16,
    max_days: u16,
    window_start: u64,
) -> Result<(), TemplateError> {
    let window_day = i64::try_from(window_start / DAY)
        .map_err(|_| TemplateError::DateNotObservable("window start too large"))?;
    let span = window_day - record;
    if span < i64::from(min_days) {
        return Err(TemplateError::DateNotObservable(
            "the period's filings are not all due yet",
        ));
    }
    if span > i64::from(max_days) {
        return Err(TemplateError::DateNotObservable("the period is too old"));
    }
    Ok(())
}

/// Whether day `record` of a daily source may be observed in a window
/// starting at `window_start`: the window's UTC day is `min_days..=max_days`
/// calendar days after it ([`SubjectRule::Daily`]).
fn daily_check(
    record: i64,
    min_days: u16,
    max_days: u16,
    window_start: u64,
) -> Result<(), TemplateError> {
    let window_day = i64::try_from(window_start / DAY)
        .map_err(|_| TemplateError::DateNotObservable("window start too large"))?;
    let span = window_day - record;
    if span < i64::from(min_days) {
        return Err(TemplateError::DateNotObservable(
            "the day's data is not settled yet",
        ));
    }
    if span > i64::from(max_days) {
        return Err(TemplateError::DateNotObservable("the day is too old"));
    }
    Ok(())
}

/// The `YYYY-MM-DD` date of a day number (days since 1970-01-01), the inverse
/// of [`day_number`] (H. Hinnant's civil-from-days algorithm).
pub fn civil_date(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 of a strict `YYYY-MM-DD` date (a real calendar day).
pub fn day_number(date: &str) -> Option<i64> {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = date.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_len = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&m) || d < 1 || d > month_len[usize::try_from(m - 1).ok()?] {
        return None;
    }
    // Days from civil (H. Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

impl FeedTemplate {
    /// Keccak-256 of `"onyx-feed-template-v1" ‖ borsh(self)`.
    pub fn id(&self) -> Hash32 {
        let mut b = b"onyx-feed-template-v1".to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }

    /// Whether facts of this template are one per date (dated, period or daily)
    /// rather than one per window end (fixed).
    pub fn is_dated(&self) -> bool {
        matches!(
            self.subject,
            SubjectRule::Dated { .. }
                | SubjectRule::Period { .. }
                | SubjectRule::Daily { .. }
                | SubjectRule::Bucketed { .. }
                | SubjectRule::Recheck { .. }
        )
    }

    /// The bucket a bucketed subject names (`prefix:<start>`), if this
    /// template is bucketed and `subject` is one of its subjects: the start is
    /// canonical decimal and on the template's grid.
    pub fn bucket_of(&self, subject: &Subject) -> Option<Bucket> {
        let SubjectRule::Bucketed {
            prefix,
            bucket_secs,
            ..
        } = &self.subject
        else {
            return None;
        };
        let Subject::Aggregate(id) = subject else {
            return None;
        };
        let start = canonical_u64(id.strip_prefix(prefix.as_str())?.strip_prefix(':')?)?;
        Bucket::on_grid(start, *bucket_secs)
    }

    /// This template with `rule` as its subject rule: the original a
    /// [`SubjectRule::Recheck`] reads the same subjects, URL and selector as.
    fn with_rule(&self, rule: &SubjectRule) -> FeedTemplate {
        FeedTemplate {
            subject: rule.clone(),
            ..self.clone()
        }
    }

    /// The first and last UTC day (days since 1970-01-01) a recheck window of
    /// `subject` may start on: `after_days` after the original rule's last
    /// observable day, for `span_days` days. `None` for a template that is no
    /// recheck or a subject it does not produce.
    pub fn recheck_days(&self, subject: &Subject) -> Option<(i64, i64)> {
        let SubjectRule::Recheck {
            of,
            after_days,
            span_days,
        } = &self.subject
        else {
            return None;
        };
        let last = self.with_rule(of).last_observable_day(subject)?;
        let first = last.checked_add(i64::from(*after_days))?;
        Some((
            first,
            first.checked_add(i64::from(*span_days))?.checked_sub(1)?,
        ))
    }

    /// Reject a malformed template.
    pub fn validate(&self) -> Result<(), TemplateError> {
        let visible = |s: &str| !s.is_empty() && s.bytes().all(|c| (0x21..=0x7e).contains(&c));
        if !visible(&self.feed) || !visible(&self.selector) || self.metric.0.is_empty() {
            return Err(TemplateError::BadTemplate("feed, metric and selector"));
        }
        if !self.url_template.starts_with("https://") || !visible(&self.url_template) {
            return Err(TemplateError::BadTemplate(
                "url must be visible-ascii https",
            ));
        }
        if self.fetcher == Hash32([0; 32]) {
            return Err(TemplateError::BadTemplate("fetcher id is zero"));
        }
        if self.window_secs == 0 || self.min_observers == 0 {
            return Err(TemplateError::BadTemplate("window and observers"));
        }
        let holes = self.url_template.matches(DATE_PLACEHOLDER).count();
        let bucket_holes = self.url_template.matches(BUCKET_START_PLACEHOLDER).count()
            + self
                .url_template
                .matches(BUCKET_START_MS_PLACEHOLDER)
                .count();
        let selector_bucket = self.selector.contains(BUCKET_START_PLACEHOLDER)
            || self.selector.contains(BUCKET_START_MS_PLACEHOLDER);
        // ADR-0058: only a bucketed template holds a bucket placeholder.
        if !matches!(self.subject, SubjectRule::Bucketed { .. })
            && (bucket_holes != 0 || selector_bucket)
        {
            return Err(TemplateError::BadTemplate(
                "only a bucketed feed holds {bucket_start} or {bucket_start_ms}",
            ));
        }
        match &self.subject {
            SubjectRule::Fixed(_) if holes != 0 => {
                Err(TemplateError::BadTemplate("a fixed feed has no {date}"))
            }
            SubjectRule::Bucketed {
                prefix,
                bucket_secs,
                min_delay_secs,
                max_delay_secs,
            } => {
                if holes != 0
                    || self.selector.contains(DATE_PLACEHOLDER)
                    || selector_bucket
                    || !(1..=MAX_BUCKET_HOLES).contains(&bucket_holes)
                {
                    return Err(TemplateError::BadTemplate(
                        "a bucketed feed has one or two bucket placeholders in its url, no {date}, and none in its selector",
                    ));
                }
                if !visible(prefix) || prefix.contains(':') {
                    return Err(TemplateError::BadTemplate("bucketed subject prefix"));
                }
                if !BUCKET_SIZES.contains(bucket_secs) {
                    return Err(TemplateError::BadTemplate(
                        "a bucket is an hour or a day (3600 or 86400 seconds)",
                    ));
                }
                if *min_delay_secs < MIN_BUCKET_DELAY_SECS
                    || min_delay_secs > max_delay_secs
                    || *max_delay_secs > MAX_BUCKET_DELAY_SECS
                    || max_delay_secs - min_delay_secs > MAX_BUCKET_RANGE_SECS
                {
                    return Err(TemplateError::BadTemplate(
                        "bucket delays: 60 s <= min <= max <= 7 days, max - min <= 6 hours",
                    ));
                }
                Ok(())
            }
            SubjectRule::Dated { prefix, lag } => {
                if holes != 1 {
                    return Err(TemplateError::BadTemplate("a dated feed has one {date}"));
                }
                if !visible(prefix) || prefix.contains(':') {
                    return Err(TemplateError::BadTemplate("dated subject prefix"));
                }
                lag.validate()
            }
            SubjectRule::Period {
                prefix,
                min_days,
                max_days,
            } => {
                if holes != 0 || self.selector.matches(DATE_PLACEHOLDER).count() != 1 {
                    return Err(TemplateError::BadTemplate(
                        "a period feed has no {date} in its url and one in its selector",
                    ));
                }
                if !visible(prefix) || prefix.contains(':') {
                    return Err(TemplateError::BadTemplate("period subject prefix"));
                }
                if *min_days == 0 || min_days > max_days {
                    return Err(TemplateError::BadTemplate("period lag bounds"));
                }
                Ok(())
            }
            SubjectRule::Daily {
                prefix,
                min_days,
                max_days,
            } => {
                if !(1..=MAX_DAILY_HOLES).contains(&holes)
                    || self.selector.contains(DATE_PLACEHOLDER)
                {
                    return Err(TemplateError::BadTemplate(
                        "a daily feed has one to four {date} in its url and none in its selector",
                    ));
                }
                if !visible(prefix) || prefix.contains(':') {
                    return Err(TemplateError::BadTemplate("daily subject prefix"));
                }
                if *min_days == 0 || min_days > max_days {
                    return Err(TemplateError::BadTemplate("daily lag bounds"));
                }
                Ok(())
            }
            SubjectRule::Recheck {
                of,
                after_days,
                span_days,
            } => {
                if matches!(**of, SubjectRule::Fixed(_) | SubjectRule::Recheck { .. }) {
                    return Err(TemplateError::BadTemplate(
                        "a recheck reads a dated, period or daily rule",
                    ));
                }
                if *after_days == 0 || *span_days == 0 {
                    return Err(TemplateError::BadTemplate("recheck days"));
                }
                if !self.metric.0.ends_with(RECHECK_SUFFIX) || !self.feed.ends_with(RECHECK_SUFFIX)
                {
                    return Err(TemplateError::BadTemplate(
                        "a recheck's metric and feed end in .recheck",
                    ));
                }
                self.with_rule(of).validate()
            }
            SubjectRule::Fixed(_) => Ok(()),
        }
    }

    /// The date a dated subject names (`prefix:YYYY-MM-DD`), if this template
    /// is dated and `subject` is one of its subjects.
    pub fn date_of<'a>(&self, subject: &'a Subject) -> Option<&'a str> {
        if let SubjectRule::Recheck { of, .. } = &self.subject {
            return self.with_rule(of).date_of(subject);
        }
        let (SubjectRule::Dated { prefix, .. }
        | SubjectRule::Period { prefix, .. }
        | SubjectRule::Daily { prefix, .. }) = &self.subject
        else {
            return None;
        };
        let Subject::Aggregate(id) = subject else {
            return None;
        };
        let date = id.strip_prefix(prefix.as_str())?.strip_prefix(':')?;
        day_number(date).map(|_| date)
    }

    /// The canonical spec for `subject`, observed from `window_start`. Refuses
    /// a subject the rule does not produce and a date the lag does not allow.
    pub fn instantiate(
        &self,
        subject: &Subject,
        window_start: u64,
    ) -> Result<ObservationSpec, TemplateError> {
        self.validate()?;
        let (url, selector) = match &self.subject {
            SubjectRule::Fixed(s) => {
                if s != subject {
                    return Err(TemplateError::NotThisFeed("subject"));
                }
                // L2RND-R2-F2: a fixed feed's windows sit on its grid, so a
                // coordinator cannot shift the window by a second to get a new
                // fact (and a new draw). The registry's per-subject draw bound
                // covers every window regardless.
                if !window_start.is_multiple_of(self.window_secs) {
                    return Err(TemplateError::NotThisFeed("window off the feed's grid"));
                }
                (self.url_template.clone(), self.selector.clone())
            }
            SubjectRule::Dated { lag, .. } => {
                let date = self
                    .date_of(subject)
                    .ok_or(TemplateError::NotThisFeed("dated subject"))?;
                let day = day_number(date).ok_or(TemplateError::NotThisFeed("dated subject"))?;
                lag.check(day, window_start)?;
                (
                    self.url_template.replacen(DATE_PLACEHOLDER, date, 1),
                    self.selector.clone(),
                )
            }
            SubjectRule::Period {
                min_days, max_days, ..
            } => {
                let date = self
                    .date_of(subject)
                    .ok_or(TemplateError::NotThisFeed("period subject"))?;
                let day = day_number(date).ok_or(TemplateError::NotThisFeed("period subject"))?;
                period_check(day, *min_days, *max_days, window_start)?;
                (
                    self.url_template.clone(),
                    self.selector.replacen(DATE_PLACEHOLDER, date, 1),
                )
            }
            SubjectRule::Daily {
                min_days, max_days, ..
            } => {
                let date = self
                    .date_of(subject)
                    .ok_or(TemplateError::NotThisFeed("daily subject"))?;
                let day = day_number(date).ok_or(TemplateError::NotThisFeed("daily subject"))?;
                daily_check(day, *min_days, *max_days, window_start)?;
                (
                    self.url_template.replace(DATE_PLACEHOLDER, date),
                    self.selector.clone(),
                )
            }
            SubjectRule::Bucketed {
                min_delay_secs,
                max_delay_secs,
                ..
            } => {
                let b = self
                    .bucket_of(subject)
                    .ok_or(TemplateError::NotThisFeed("bucketed subject"))?;
                bucket_check(b.end, *min_delay_secs, *max_delay_secs, window_start)?;
                let ms = b
                    .start
                    .checked_mul(1_000)
                    .ok_or(TemplateError::NotThisFeed("bucketed subject"))?;
                (
                    self.url_template
                        .replace(BUCKET_START_MS_PLACEHOLDER, &ms.to_string())
                        .replace(BUCKET_START_PLACEHOLDER, &b.start.to_string()),
                    self.selector.clone(),
                )
            }
            SubjectRule::Recheck { of, .. } => {
                let (first, last) = self
                    .recheck_days(subject)
                    .ok_or(TemplateError::NotThisFeed("recheck subject"))?;
                let window_day = i64::try_from(window_start / DAY)
                    .map_err(|_| TemplateError::DateNotObservable("window start too large"))?;
                if window_day < first {
                    return Err(TemplateError::DateNotObservable(
                        "the recheck is not due yet",
                    ));
                }
                if window_day > last {
                    return Err(TemplateError::DateNotObservable(
                        "the recheck's days are over",
                    ));
                }
                let date = self
                    .date_of(subject)
                    .ok_or(TemplateError::NotThisFeed("recheck subject"))?;
                // The original rule's URL and selector, filled the same way.
                match **of {
                    SubjectRule::Dated { .. } => (
                        self.url_template.replacen(DATE_PLACEHOLDER, date, 1),
                        self.selector.clone(),
                    ),
                    SubjectRule::Period { .. } => (
                        self.url_template.clone(),
                        self.selector.replacen(DATE_PLACEHOLDER, date, 1),
                    ),
                    SubjectRule::Daily { .. } => (
                        self.url_template.replace(DATE_PLACEHOLDER, date),
                        self.selector.clone(),
                    ),
                    SubjectRule::Fixed(_)
                    | SubjectRule::Bucketed { .. }
                    | SubjectRule::Recheck { .. } => {
                        return Err(TemplateError::BadTemplate(
                            "a recheck reads a dated, period or daily rule",
                        ))
                    }
                }
            }
        };
        let window_end = window_start
            .checked_add(self.window_secs)
            .ok_or(TemplateError::NotThisFeed("window"))?;
        Ok(ObservationSpec {
            feed: self.feed.clone(),
            metric: self.metric.clone(),
            subject: subject.clone(),
            fetcher: self.fetcher,
            url,
            selector,
            kind: self.kind,
            window_start,
            window_end,
            min_observers: self.min_observers,
            rights: self.rights.clone(),
        })
    }

    /// When data of `subject` starts to age for pricing: a pure function of
    /// this template and the subject, never of a round input (L2API-R3-F1).
    /// A dated subject ages from the end of the earliest window its lag allows
    /// (`earliest_window_day` + `window_secs`), however late a round observes
    /// it; a fixed subject's facts are one per window, so each ages from its
    /// own `window_end`. `None` for a subject this template does not produce.
    pub fn decay_start(&self, subject: &Subject, window_end: u64) -> Option<u64> {
        match &self.subject {
            SubjectRule::Fixed(s) => (s == subject).then_some(window_end),
            SubjectRule::Dated { lag, .. } => {
                let day = day_number(self.date_of(subject)?)?;
                let first = u64::try_from(lag.earliest_window_day(day)?).ok()?;
                first.checked_mul(DAY)?.checked_add(self.window_secs)
            }
            SubjectRule::Period { min_days, .. } | SubjectRule::Daily { min_days, .. } => {
                let day = day_number(self.date_of(subject)?)?;
                let first = u64::try_from(day.checked_add(i64::from(*min_days))?).ok()?;
                first.checked_mul(DAY)?.checked_add(self.window_secs)
            }
            // ADR-0058: a bucket ages from the end of its first observable window.
            SubjectRule::Bucketed { min_delay_secs, .. } => self
                .bucket_of(subject)?
                .end
                .checked_add(u64::from(*min_delay_secs))?
                .checked_add(self.window_secs),
            SubjectRule::Recheck { .. } => {
                let first = u64::try_from(self.recheck_days(subject)?.0).ok()?;
                first.checked_mul(DAY)?.checked_add(self.window_secs)
            }
        }
    }

    /// The subject's last observable day (days since 1970-01-01): the last
    /// UTC day a window of `subject` may start on under this template (ADR-0027
    /// amendment 15, ROADMAP C52). A dated subject: its record day plus
    /// `max_calendar_days`, never past the calendar's coverage; a period
    /// subject: its period end plus `max_days`; a daily subject: its day plus
    /// `max_days`. `None` for a fixed subject
    /// (observable every window: its facts are one per window, see
    /// [`Self::latest_window_start`]) and for a subject this template does not
    /// produce. A pure function of the template and the subject, so anyone
    /// recomputes it.
    pub fn last_observable_day(&self, subject: &Subject) -> Option<i64> {
        match &self.subject {
            SubjectRule::Fixed(_) => None,
            SubjectRule::Dated { lag, .. } => {
                let day = day_number(self.date_of(subject)?)?;
                let last = day
                    .checked_add(i64::from(lag.max_calendar_days))?
                    .min(lag.last_day);
                (last > day).then_some(last)
            }
            SubjectRule::Period { max_days, .. } | SubjectRule::Daily { max_days, .. } => {
                day_number(self.date_of(subject)?)?.checked_add(i64::from(*max_days))
            }
            SubjectRule::Bucketed { .. } => {
                i64::try_from(self.latest_window_start(subject, 0)? / DAY).ok()
            }
            SubjectRule::Recheck { .. } => self.recheck_days(subject).map(|(_, last)| last),
        }
    }

    /// The latest second a window observing the same FACT as a window of
    /// `subject` starting at `window_start` may start (ROADMAP C52): for a
    /// dated, period or daily subject (one fact per date, whatever the window) the
    /// last second of [`Self::last_observable_day`]; for a fixed subject (one
    /// fact per window) `window_start` itself. `None` when the template does
    /// not produce `subject`, or the day is before 1970.
    pub fn latest_window_start(&self, subject: &Subject, window_start: u64) -> Option<u64> {
        match &self.subject {
            SubjectRule::Fixed(s) => (s == subject).then_some(window_start),
            SubjectRule::Dated { .. }
            | SubjectRule::Period { .. }
            | SubjectRule::Daily { .. }
            | SubjectRule::Recheck { .. } => {
                let next =
                    u64::try_from(self.last_observable_day(subject)?.checked_add(1)?).ok()?;
                next.checked_mul(DAY)?.checked_sub(1)
            }
            // ADR-0058: the bucket's end plus `max_delay_secs`, to the second.
            SubjectRule::Bucketed { max_delay_secs, .. } => self
                .bucket_of(subject)?
                .end
                .checked_add(u64::from(*max_delay_secs)),
        }
    }

    /// `Ok` only when `spec` is exactly this template instantiated for the
    /// spec's own subject and window start: same metric, feed id, fetcher,
    /// url, selector, kind, window, observer count and rights.
    pub fn matches(&self, spec: &ObservationSpec) -> Result<(), TemplateError> {
        if spec.metric != self.metric {
            return Err(TemplateError::NotThisFeed("metric"));
        }
        let canon = self.instantiate(&spec.subject, spec.window_start)?;
        if &canon != spec {
            return Err(TemplateError::NotThisFeed(
                "fetcher, url, selector, kind, window, observers or rights",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ROADMAP B6: `civil_date` is the inverse of `day_number`.
    #[test]
    fn civil_date_inverts_day_number() {
        for day in -1_000..30_000 {
            assert_eq!(day_number(&civil_date(day)), Some(day), "{day}");
        }
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(20_753), "2026-10-27");
    }

    fn lag() -> DateLag {
        DateLag {
            min_business_days: 1,
            max_calendar_days: 10,
            first_day: day_number("2026-01-01").expect("date"),
            last_day: day_number("2026-12-31").expect("date"),
            holidays: vec![day_number("2026-10-12").expect("date")],
        }
    }

    fn period_template() -> FeedTemplate {
        FeedTemplate {
            feed: "f".into(),
            metric: MetricId("m".into()),
            fetcher: keccak256(b"fetcher"),
            url_template: "https://example.org/doc.json".into(),
            selector: "units/X/end={date}".into(),
            kind: ObservationKind::Numeric {
                scale: 0,
                tolerance: 0,
            },
            window_secs: 300,
            min_observers: 3,
            rights: DataRights {
                license: "l".into(),
                redistribution: crate::Redistribution::Allowed,
                source: crate::SourceId("s".into()),
                attribution: None,
                terms: String::new(),
            },
            subject: SubjectRule::Period {
                prefix: "co.x".into(),
                min_days: 95,
                max_days: 730,
            },
        }
    }

    /// SEC-1: a period subject is any calendar day, its date fills the
    /// selector, and it is observable only `min_days..=max_days` after.
    #[test]
    fn period_subjects_fill_the_selector_and_bound_the_window() {
        let t = period_template();
        t.validate().expect("valid");
        assert!(t.is_dated());
        let d = |s: &str| u64::try_from(day_number(s).expect("date")).expect("pos") * DAY;
        // 2028-09-30 is a Saturday: still a period end.
        let s = Subject::Aggregate("co.x:2028-09-30".into());
        let spec = t.instantiate(&s, d("2029-01-03")).expect("95 days after");
        assert_eq!(spec.selector, "units/X/end=2028-09-30");
        assert_eq!(spec.url, "https://example.org/doc.json");
        assert!(t.matches(&spec).is_ok());
        assert!(t.instantiate(&s, d("2029-01-02")).is_err(), "94 days");
        assert!(t.instantiate(&s, d("2030-09-30")).is_ok(), "730 days");
        assert!(t.instantiate(&s, d("2030-10-01")).is_err(), "731 days");
        for bad in [
            "co.x:2028-09-31",
            "co.x",
            "co.y:2028-09-30",
            "co.x:2028-9-30",
        ] {
            assert!(
                t.instantiate(&Subject::Aggregate(bad.into()), d("2029-02-01"))
                    .is_err(),
                "{bad}"
            );
        }
        // The price clock: the first allowed window's end, whatever the round.
        assert_eq!(
            t.decay_start(&s, 0),
            Some(d("2029-01-03") + 300),
            "record + min_days + window"
        );
        // Malformed period templates are refused.
        let mut url_hole = period_template();
        url_hole.url_template = "https://example.org/{date}.json".into();
        let mut no_hole = period_template();
        no_hole.selector = "units/X/end".into();
        let mut bounds = period_template();
        bounds.subject = SubjectRule::Period {
            prefix: "co.x".into(),
            min_days: 10,
            max_days: 9,
        };
        let mut colon = period_template();
        colon.subject = SubjectRule::Period {
            prefix: "co:x".into(),
            min_days: 1,
            max_days: 9,
        };
        for bad in [url_hole, no_hole, bounds, colon] {
            assert!(bad.validate().is_err());
        }
    }

    fn daily_template() -> FeedTemplate {
        FeedTemplate {
            url_template:
                "https://example.org/count?starttime={date}T00:00:00&endtime={date}T23:59:59.999"
                    .into(),
            selector: "body".into(),
            subject: SubjectRule::Daily {
                prefix: "quakes.m5".into(),
                min_days: 100,
                max_days: 400,
            },
            ..period_template()
        }
    }

    /// NS3: a daily subject fills every `{date}` of the URL with its day, any
    /// calendar day (a Sunday too), and is observable only `min_days..=max_days`
    /// after it.
    #[test]
    fn daily_subjects_fill_every_url_date_and_bound_the_window() {
        let t = daily_template();
        t.validate().expect("valid");
        assert!(t.is_dated());
        // One fact per day on chain too (REG-R2-F2).
        assert_eq!(
            crate::onchain::MetricKind::of(&t),
            crate::onchain::MetricKind::DatedFeed
        );
        // 2026-04-05 is a Sunday.
        let day = day_number("2026-04-05").expect("date");
        let s = Subject::Aggregate("quakes.m5:2026-04-05".into());
        let at = |d: i64| u64::try_from(d).expect("after 1970") * DAY;
        let spec = t.instantiate(&s, at(day + 100)).expect("observable");
        assert_eq!(
            spec.url,
            "https://example.org/count?starttime=2026-04-05T00:00:00&endtime=2026-04-05T23:59:59.999"
        );
        assert_eq!(spec.selector, "body");
        assert!(t.matches(&spec).is_ok());
        assert!(t.instantiate(&s, at(day + 400) + DAY - 1).is_ok());
        assert_eq!(
            t.instantiate(&s, at(day + 99) + DAY - 1),
            Err(TemplateError::DateNotObservable(
                "the day's data is not settled yet"
            ))
        );
        assert_eq!(
            t.instantiate(&s, at(day + 401)),
            Err(TemplateError::DateNotObservable("the day is too old"))
        );
        // Another prefix, a bad date or another kind of subject is not this feed.
        for bad in ["quakes.m4:2026-04-05", "quakes.m5:2026-02-30", "quakes.m5"] {
            assert!(t
                .instantiate(&Subject::Aggregate(bad.into()), at(day + 100))
                .is_err());
        }
        assert_eq!(t.decay_start(&s, 0), Some(at(day + 100) + t.window_secs));
        assert_eq!(t.last_observable_day(&s), Some(day + 400));
        // A spec whose URL has another day in one place is refused.
        let mut half = spec.clone();
        half.url = half.url.replacen("2026-04-05T23", "2026-04-06T23", 1);
        assert!(t.matches(&half).is_err());
    }

    #[test]
    fn daily_templates_are_validated() {
        let with = |url: &str, sel: &str, min: u16, max: u16| FeedTemplate {
            url_template: url.into(),
            selector: sel.into(),
            subject: SubjectRule::Daily {
                prefix: "quakes.m5".into(),
                min_days: min,
                max_days: max,
            },
            ..daily_template()
        };
        let ok = "https://example.org/c?a={date}";
        assert!(with(ok, "body", 1, 1).validate().is_ok());
        for bad in [
            with("https://example.org/c", "body", 1, 2),
            with(&format!("{ok}{}", "&b={date}".repeat(4)), "body", 1, 2),
            with(ok, "x={date}", 1, 2),
            with(ok, "body", 0, 2),
            with(ok, "body", 3, 2),
        ] {
            assert!(bad.validate().is_err(), "{:?}", bad.url_template);
        }
        let mut colon = daily_template();
        colon.subject = SubjectRule::Daily {
            prefix: "a:b".into(),
            min_days: 1,
            max_days: 2,
        };
        assert!(colon.validate().is_err());
        // A daily template is another feed than the same fields under another rule.
        let mut period = daily_template();
        period.subject = SubjectRule::Period {
            prefix: "quakes.m5".into(),
            min_days: 100,
            max_days: 400,
        };
        assert_ne!(daily_template().id(), period.id());
    }

    /// L2RND-R2-F2: a fixed feed's windows sit on its grid, so a window
    /// shifted by a second is not a new fact.
    #[test]
    fn fixed_windows_sit_on_the_feed_grid() {
        let mut t = period_template();
        let s = Subject::Aggregate("x".into());
        t.subject = SubjectRule::Fixed(s.clone());
        t.selector = "units/X".into();
        let spec = t.instantiate(&s, 3_000).expect("on the grid");
        assert!(t.matches(&spec).is_ok());
        for off in [1, 299, 3_001, 3_150] {
            assert_eq!(
                t.instantiate(&s, off),
                Err(TemplateError::NotThisFeed("window off the feed's grid")),
                "{off}"
            );
        }
        let mut shifted = spec;
        shifted.window_start += 1;
        shifted.window_end += 1;
        assert!(t.matches(&shifted).is_err());
    }

    #[test]
    fn earliest_window_day_is_the_first_day_the_lag_allows() {
        let l = lag();
        let d = |s: &str| day_number(s).expect("date");
        // Thu record: Fri is the business day between, so Sat opens.
        assert_eq!(
            l.earliest_window_day(d("2026-10-08")),
            Some(d("2026-10-10"))
        );
        // Fri record before the Monday holiday: published Tue, Wed opens.
        assert_eq!(
            l.earliest_window_day(d("2026-10-09")),
            Some(d("2026-10-14"))
        );
        // A weekend record is never observable.
        assert_eq!(l.earliest_window_day(d("2026-10-10")), None);
        // Outside the coverage.
        assert_eq!(l.earliest_window_day(d("2027-03-01")), None);
    }

    #[test]
    fn business_days_follow_the_calendar() {
        let l = lag();
        let d = |s: &str| day_number(s).expect("date");
        let at = |s: &str| u64::try_from(d(s)).expect("date") * DAY;
        assert_eq!(l.is_business_day(d("2026-10-09")), Some(true)); // Fri
        assert_eq!(l.is_business_day(d("2026-10-10")), Some(false)); // Sat
        assert_eq!(l.is_business_day(d("2026-10-12")), Some(false)); // holiday
        assert_eq!(l.is_business_day(d("2027-01-04")), None); // outside
                                                              // Thu record: published Fri, a Sat window is fine.
        assert!(l.check(d("2026-10-08"), at("2026-10-10")).is_ok());
        assert!(l.check(d("2026-10-08"), at("2026-10-09")).is_err());
        // Fri record before a Monday holiday: published Tue.
        assert!(l.check(d("2026-10-09"), at("2026-10-13")).is_err());
        assert!(l.check(d("2026-10-09"), at("2026-10-14")).is_ok());
        // Weekend records never publish; too old is refused.
        assert!(l.check(d("2026-10-10"), at("2026-10-14")).is_err());
        assert!(l.check(d("2026-10-01"), at("2026-10-12")).is_err());
        assert!(l.check(d("2026-10-02"), at("2026-10-12")).is_ok());
    }
    fn dated_template() -> FeedTemplate {
        let mut t = period_template();
        t.url_template = "https://example.org/{date}.json".into();
        t.selector = "units/X".into();
        t.subject = SubjectRule::Dated {
            prefix: "rate.x".into(),
            lag: lag(),
        };
        t
    }

    /// ROADMAP C52 (ADR-0027 amendment 15): the subject's last observable day
    /// is the last day ANY window of the fact may start on, and an allowed
    /// window exists on it: a dated subject, record day + `max_calendar_days`
    /// (cut at the calendar's coverage); a period subject, end + `max_days`; a
    /// fixed subject has none (its fact is one window, which is its own latest
    /// start). Every window `instantiate` accepts starts at or before
    /// `latest_window_start`; the day after is refused.
    #[test]
    fn c52_the_last_observable_day_bounds_every_window_of_the_fact() {
        let d = |s: &str| day_number(s).expect("date");
        let at = |day: i64| u64::try_from(day).expect("pos") * DAY;
        let dated = dated_template();
        let s = Subject::Aggregate("rate.x:2026-10-08".into());
        assert_eq!(dated.last_observable_day(&s), Some(d("2026-10-18")));
        let last = dated
            .latest_window_start(&s, at(d("2026-10-10")))
            .expect("bound");
        assert_eq!(last, at(d("2026-10-19")) - 1);
        // Every second of every allowed day is at or before the bound.
        for day in d("2026-10-08")..=d("2026-10-25") {
            for t in [at(day), at(day) + DAY - 1] {
                if dated.instantiate(&s, t).is_ok() {
                    assert!(t <= last, "a window after the last observable day");
                }
            }
        }
        assert!(
            dated.instantiate(&s, last).is_ok(),
            "the last day is allowed"
        );
        assert!(dated.instantiate(&s, last + 1).is_err());
        // Near the end of the calendar's coverage the bound is the coverage.
        let late = Subject::Aggregate("rate.x:2026-12-28".into());
        assert_eq!(dated.last_observable_day(&late), Some(d("2026-12-31")));
        // A period subject: end + max_days, for any window of the fact.
        let p = period_template();
        let ps = Subject::Aggregate("co.x:2028-09-30".into());
        assert_eq!(p.last_observable_day(&ps), Some(d("2030-09-30")));
        let pl = p
            .latest_window_start(&ps, at(d("2029-01-03")))
            .expect("bound");
        assert_eq!(pl, at(d("2030-10-01")) - 1);
        assert!(p.instantiate(&ps, pl).is_ok());
        assert!(p.instantiate(&ps, pl + 1).is_err());
        // A fixed subject: no last day; the fact's one window is its bound.
        let mut f = period_template();
        let fs = Subject::Entity("entity:alpha".into());
        f.subject = SubjectRule::Fixed(fs.clone());
        f.selector = "units/X".into();
        assert_eq!(f.last_observable_day(&fs), None);
        assert_eq!(f.latest_window_start(&fs, 600), Some(600));
        // Not this template's subject: no bound at all (fails closed).
        assert_eq!(f.latest_window_start(&s, 600), None);
        assert_eq!(dated.latest_window_start(&ps, 600), None);
        assert_eq!(p.last_observable_day(&s), None);
    }

    /// Calendar edges of `day_number` (mutation L4): the epoch, a date past
    /// 2099 (the Gregorian century rule), leap days and every format check.
    #[test]
    fn day_number_edges() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("1969-12-31"), Some(-1));
        assert_eq!(day_number("2000-03-01"), Some(11_017));
        assert_eq!(day_number("2100-03-01"), Some(47_541));
        assert_eq!(day_number("1900-03-01"), Some(-25_508));
        for ok in ["2024-02-29", "2000-02-29", "2026-12-31", "2026-01-31"] {
            assert!(day_number(ok).is_some(), "{ok}");
        }
        for bad in [
            "2023-02-29",
            "1900-02-29",
            "2100-02-29",
            "2026-00-10",
            "2026-13-10",
            "2026-04-00",
            "2026-04-31",
            "2026x10-12",
            "2026-10x12",
            "2026-10-1",
            "2026-10-123",
            "2026-1a-12",
        ] {
            assert_eq!(day_number(bad), None, "{bad}");
        }
    }

    /// `DateLag::validate`, `is_business_day` and `check` at their bounds
    /// (mutation L4).
    #[test]
    fn date_lag_bounds() {
        let d = |s: &str| day_number(s).expect("date");
        let at = |s: &str| u64::try_from(d(s)).expect("date") * DAY;
        assert_eq!(lag().validate(), Ok(()));
        let with = |f: &dyn Fn(&mut DateLag)| {
            let mut l = lag();
            f(&mut l);
            l.validate()
        };
        assert!(with(&|l| l.max_calendar_days = 0).is_err());
        assert!(with(&|l| l.first_day = l.last_day + 1).is_err());
        assert_eq!(
            with(&|l| {
                l.first_day = l.last_day;
                l.holidays.clear();
            }),
            Ok(())
        );
        // Holidays on both edges of the coverage are inside it.
        assert_eq!(
            with(&|l| l.holidays = vec![l.first_day, l.last_day]),
            Ok(())
        );
        assert!(with(&|l| l.holidays = vec![l.first_day - 1]).is_err());
        assert!(with(&|l| l.holidays = vec![l.last_day + 1]).is_err());
        assert!(with(&|l| l.holidays = vec![l.last_day, l.first_day]).is_err());
        assert!(with(&|l| l.holidays = vec![l.first_day, l.first_day]).is_err());
        // Coverage edges: 2026-01-01 (Thu) and 2026-12-31 (Thu) are inside.
        let l = lag();
        assert_eq!(l.is_business_day(l.first_day), Some(true));
        assert_eq!(l.is_business_day(l.last_day), Some(true));
        assert_eq!(l.is_business_day(l.first_day - 1), None);
        assert_eq!(l.is_business_day(l.last_day + 1), None);
        // A record on the first covered day, and a window on the last one.
        assert!(l.check(d("2026-01-01"), at("2026-01-03")).is_ok());
        assert!(l.check(d("2026-12-29"), at("2026-12-31")).is_ok());
        assert!(l.check(d("2026-12-30"), at("2027-01-01")).is_err());
        assert!(l.check(d("2025-12-31"), at("2026-01-02")).is_err());
        // Without a publication lag a window may open the next day, never
        // the same day or before the record.
        let mut l0 = lag();
        l0.min_business_days = 0;
        assert!(l0.check(d("2026-10-08"), at("2026-10-09")).is_ok());
        assert!(l0.check(d("2026-10-08"), at("2026-10-08")).is_err());
        assert!(l0.check(d("2026-10-08"), at("2026-10-07")).is_err());
        // The calendar span is inclusive of max_calendar_days (10).
        assert!(l0.check(d("2026-10-08"), at("2026-10-18")).is_ok());
        assert!(l0.check(d("2026-10-08"), at("2026-10-19")).is_err());
    }

    /// `FeedTemplate::validate` refuses each malformed field on its own, and
    /// accepts a well-formed dated template (mutation L4).
    #[test]
    fn template_validate_checks_each_field() {
        let t = dated_template();
        assert_eq!(t.validate(), Ok(()));
        assert!(t.is_dated());
        let bad = |f: &dyn Fn(&mut FeedTemplate)| {
            let mut t = dated_template();
            f(&mut t);
            t.validate().is_err()
        };
        assert!(bad(&|t| t.feed = String::new()));
        assert!(bad(&|t| t.feed = "a feed".into()));
        assert!(bad(&|t| t.selector = String::new()));
        assert!(bad(&|t| t.selector = "units X".into()));
        assert!(bad(&|t| t.metric = MetricId(String::new())));
        assert!(bad(&|t| t.url_template = "http://example.org/{date}".into()));
        assert!(bad(
            &|t| t.url_template = "https://example.org/ {date}".into()
        ));
        assert!(bad(&|t| t.fetcher = Hash32([0; 32])));
        assert!(bad(&|t| t.window_secs = 0));
        assert!(bad(&|t| t.min_observers = 0));
        assert!(bad(&|t| t.url_template = "https://example.org/x".into()));
        assert!(bad(
            &|t| t.url_template = "https://e.org/{date}/{date}".into()
        ));
        let prefix = |p: &str| {
            let mut t = dated_template();
            t.subject = SubjectRule::Dated {
                prefix: p.into(),
                lag: lag(),
            };
            t.validate().is_err()
        };
        assert!(prefix("rate:x"));
        assert!(prefix("rate x"));
        assert!(prefix(""));
        assert!(!prefix("rate.y"));
        // A bad lag makes the template bad.
        assert!(bad(&|t| {
            if let SubjectRule::Dated { lag, .. } = &mut t.subject {
                lag.max_calendar_days = 0;
            }
        }));
        // A fixed feed: no {date}; it is not dated; it ages from its window end.
        let s = Subject::Aggregate("x".into());
        let mut f = dated_template();
        f.subject = SubjectRule::Fixed(s.clone());
        assert!(f.validate().is_err(), "a fixed feed with a {{date}}");
        f.url_template = "https://example.org/x".into();
        assert_eq!(f.validate(), Ok(()));
        assert!(!f.is_dated());
        assert_eq!(f.decay_start(&s, 1_234), Some(1_234));
        assert_eq!(f.decay_start(&Subject::Aggregate("y".into()), 1_234), None);
        // A period with min_days == max_days is a one-day window.
        let mut p = period_template();
        p.subject = SubjectRule::Period {
            prefix: "co.x".into(),
            min_days: 95,
            max_days: 95,
        };
        assert_eq!(p.validate(), Ok(()));
        p.subject = SubjectRule::Period {
            prefix: "co.x".into(),
            min_days: 0,
            max_days: 95,
        };
        assert!(p.validate().is_err());
    }

    /// CL7 (ADR-0057 item 2, THREATS CL7-8): `recheck_of` changes only the
    /// metric, the feed and the observable days. A recheck spec reads the
    /// same URL and selector as the original's spec of the same subject, with
    /// the same fetcher, kind, observers and rights.
    #[test]
    fn recheck_of_changes_only_metric_feed_and_window() {
        let d = |s: &str| day_number(s).expect("date");
        for t in [daily_template(), period_template(), dated_template()] {
            let r = recheck_of(&t, 7, 3).expect("recheck");
            assert_eq!(r.metric.0, format!("{}.recheck", t.metric.0));
            assert_eq!(r.feed, format!("{}.recheck", t.feed));
            assert_ne!(r.id(), t.id());
            let same = FeedTemplate {
                metric: t.metric.clone(),
                feed: t.feed.clone(),
                subject: t.subject.clone(),
                ..r.clone()
            };
            assert_eq!(same, t, "nothing else differs");
            // Deterministic: anyone recomputes the same id.
            assert_eq!(recheck_of(&t, 7, 3).expect("again").id(), r.id());
            assert_ne!(recheck_of(&t, 8, 3).expect("other").id(), r.id());
        }
        let t = daily_template();
        let r = recheck_of(&t, 7, 3).expect("recheck");
        let s = Subject::Aggregate("quakes.m5:2026-03-01".into());
        let last = t.last_observable_day(&s).expect("last");
        assert_eq!(last, d("2026-03-01") + 400);
        assert_eq!(r.recheck_days(&s), Some((last + 7, last + 9)));
        assert_eq!(r.last_observable_day(&s), Some(last + 9));
        let day = |n: i64| u64::try_from(n).expect("day") * DAY;
        // The original's spec (any allowed window) and the recheck's agree on what they read.
        let orig = t.instantiate(&s, day(last)).expect("original");
        let spec = r.instantiate(&s, day(last + 7)).expect("first recheck day");
        assert_eq!(
            (spec.url.as_str(), spec.selector.as_str()),
            (orig.url.as_str(), orig.selector.as_str())
        );
        assert_eq!(
            (spec.fetcher, spec.kind, spec.min_observers),
            (orig.fetcher, orig.kind, orig.min_observers)
        );
        assert_eq!(spec.rights, orig.rights);
        assert_eq!(spec.metric.0, "m.recheck");
        r.matches(&spec).expect("canonical");
        assert!(
            r.instantiate(&s, day(last + 9) + 300).is_ok(),
            "last recheck day"
        );
        assert!(matches!(
            r.instantiate(&s, day(last + 6) + DAY - 1),
            Err(TemplateError::DateNotObservable(_))
        ));
        assert!(matches!(
            r.instantiate(&s, day(last + 10)),
            Err(TemplateError::DateNotObservable(_))
        ));
        // The original's own window is never a recheck window, and vice versa.
        assert!(r.instantiate(&s, day(last)).is_err());
        assert!(t.instantiate(&s, day(last + 7)).is_err());
        // Ages from its first recheck day; one fact per date like the original.
        assert_eq!(r.decay_start(&s, 0), Some(day(last + 7) + t.window_secs));
        assert!(r.is_dated());
        assert_eq!(
            r.latest_window_start(&s, day(last + 7)),
            Some(day(last + 10) - 1)
        );
        // Another subject of the rule is the same subject in the recheck; a foreign one is not.
        assert!(r
            .recheck_days(&Subject::Aggregate("other:2026-03-01".into()))
            .is_none());
    }

    /// CL7: no recheck of a fixed-window or byte template, of a recheck, or with zero days;
    /// a recheck template's metric and feed must end in `.recheck`.
    #[test]
    fn recheck_of_refuses_what_has_no_correct_later() {
        let mut fixed = period_template();
        fixed.url_template = "https://example.org/now.json".into();
        fixed.selector = "price".into();
        fixed.subject = SubjectRule::Fixed(Subject::Aggregate("price.x".into()));
        assert!(fixed.validate().is_ok());
        assert!(recheck_of(&fixed, 1, 1).is_err());
        let mut bytes = daily_template();
        bytes.kind = ObservationKind::Exact { max_len: 32 };
        assert!(recheck_of(&bytes, 1, 1).is_err());
        let r = recheck_of(&daily_template(), 1, 1).expect("recheck");
        assert!(recheck_of(&r, 1, 1).is_err(), "no recheck of a recheck");
        assert!(recheck_of(&daily_template(), 0, 1).is_err());
        assert!(recheck_of(&daily_template(), 1, 0).is_err());
        let renamed = FeedTemplate {
            metric: MetricId("m.other".into()),
            ..r.clone()
        };
        assert!(renamed.validate().is_err());
        let nested = FeedTemplate {
            subject: SubjectRule::Recheck {
                of: Box::new(r.subject.clone()),
                after_days: 1,
                span_days: 1,
            },
            ..r
        };
        assert!(nested.validate().is_err());
    }

    /// CL7: the variant is appended last, so every existing template keeps its
    /// Borsh bytes and id (the live pins in the adapters' tests stay green too).
    #[test]
    fn the_recheck_variant_is_appended_last() {
        let r = recheck_of(&daily_template(), 2, 3).expect("recheck");
        let b = borsh::to_vec(&r.subject).expect("borsh");
        assert_eq!(
            b[0], 5,
            "Fixed 0, Dated 1, Period 2, Daily 3, Bucketed 4, Recheck 5"
        );
        assert_eq!(
            borsh::to_vec(&daily_template().subject).expect("borsh")[0],
            3
        );
        let back: SubjectRule = borsh::from_slice(&b).expect("decode");
        assert_eq!(back, r.subject);
    }
}
