//! The bounty board (north-star loop NL9, ROADMAP S10, docs/spec/bounty-board.md): coarse
//! categories of wanted data, never exact queries.
//!
//! A [`Want`] is four values from closed lists: the [`Kind`] of data (the submit flow's six
//! kinds), a [`Domain`], a [`Region`] (a continent or global, never a place) and a [`Cadence`].
//! Nothing else can be said: no free text, URL, metric, subject, entity, person or date
//! ([`parse_want`] refuses any other field, R-BNT-1 and R-BNT-4).
//!
//! The [`Board`] keeps one want per category per account, keyed by an account hash the caller
//! salts (R-BNT-3). It shows a category only once wants from at least [`MIN_ACCOUNTS`] distinct
//! accounts hold it, as a [`Bucket`], and only wants made before the current week's cut count,
//! so the board changes once a week and the time of one want cannot be read off it (R-BNT-2).
//! A want lapses [`LIVE_WEEKS`] weeks after its week.
//!
//! Pure: no I/O, no clock. The API (`onyx-api`'s `wants` module) proves the account and stores
//! the board.

use crate::answers::Kind;
use onyx_core::Hash32;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A category is shown only with wants from at least this many distinct accounts (spec open
/// question 1: the larger of the two candidates, which hides one buyer better).
pub const MIN_ACCOUNTS: usize = 5;
/// The board's period: counts move only at each week's cut (unix seconds).
pub const WEEK: u64 = 604_800;
/// A want counts for this many weeks after the week it was made or renewed in.
pub const LIVE_WEEKS: u64 = 4;
/// Most live wants one account may hold.
pub const MAX_WANTS_PER_ACCOUNT: usize = 16;
/// The largest want accepted, in bytes of JSON.
pub const MAX_WANT_BYTES: usize = 256;
/// Domain tag of the text an account signs ([`want_message`]).
pub const WANT_DOMAIN: &str = "onyx-want-v1";

/// What field of life the data is about (a fixed list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    /// Markets, companies, public finance figures.
    Finance,
    /// Energy production, prices, grids.
    Energy,
    /// Weather and climate observations.
    WeatherClimate,
    /// Earthquakes, water, land and other earth science.
    EarthScience,
    /// Published health statistics (never a person's data).
    HealthStatistics,
    /// Transport and traffic.
    Transport,
    /// Public spending and budgets.
    PublicSpending,
    /// Anything else.
    Other,
}

/// Where the data is about: a continent or the whole world, never a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// The whole world.
    Global,
    /// Africa.
    Africa,
    /// Asia.
    Asia,
    /// Europe.
    Europe,
    /// North America.
    NorthAmerica,
    /// South America.
    SouthAmerica,
    /// Oceania.
    Oceania,
}

/// How often the data is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    /// One value.
    Once,
    /// A value a day.
    Daily,
    /// A value a month.
    Monthly,
}

/// One wanted category: four values from closed lists, nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Want {
    /// The kind of data (the submit flow's question 1).
    pub kind: Kind,
    /// Its domain.
    pub domain: Domain,
    /// Its region.
    pub region: Region,
    /// How often.
    pub cadence: Cadence,
}

/// How many accounts want a shown category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Bucket {
    /// 1 to 4 (never shown while [`MIN_ACCOUNTS`] is 5).
    #[serde(rename = "1-4")]
    UpTo4,
    /// 5 to 19.
    #[serde(rename = "5-19")]
    UpTo19,
    /// 20 or more.
    #[serde(rename = "20+")]
    Many,
}

impl Bucket {
    /// The bucket of `n` accounts.
    pub fn of(n: usize) -> Self {
        match n {
            0..=4 => Bucket::UpTo4,
            5..=19 => Bucket::UpTo19,
            _ => Bucket::Many,
        }
    }

    /// Its label as served.
    pub fn label(self) -> &'static str {
        match self {
            Bucket::UpTo4 => "1-4",
            Bucket::UpTo19 => "5-19",
            Bucket::Many => "20+",
        }
    }
}

/// Why a want was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum BountyError {
    /// Anything but exactly the four fields: free text, a URL, a metric, a subject, a date.
    #[error(
        "a want is four values from closed lists (kind, domain, region, cadence) and nothing else"
    )]
    TooSpecific,
    /// A value outside its closed list.
    #[error("a want's value is not in its list: {0}")]
    BadValue(String),
    /// Larger than [`MAX_WANT_BYTES`].
    #[error("a want is at most {MAX_WANT_BYTES} bytes")]
    TooLarge,
    /// The account already holds [`MAX_WANTS_PER_ACCOUNT`] live wants.
    #[error("an account holds at most {MAX_WANTS_PER_ACCOUNT} live wants")]
    TooMany,
}

/// Parse a want from JSON: an object with exactly the keys `kind`, `domain`, `region` and
/// `cadence`, each a value of its closed list.
pub fn parse_want(json: &[u8]) -> Result<Want, BountyError> {
    if json.len() > MAX_WANT_BYTES {
        return Err(BountyError::TooLarge);
    }
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|_| BountyError::TooSpecific)?;
    let o = v.as_object().ok_or(BountyError::TooSpecific)?;
    let keys: BTreeSet<&str> = o.keys().map(String::as_str).collect();
    if keys != BTreeSet::from(["cadence", "domain", "kind", "region"]) {
        return Err(BountyError::TooSpecific);
    }
    if o.values().any(|x| !x.is_string()) {
        return Err(BountyError::TooSpecific);
    }
    serde_json::from_value(v).map_err(|e| BountyError::BadValue(e.to_string()))
}

/// The week number of `unix` (weeks since 1970-01-01 00:00 UTC, a Thursday).
pub fn week_of(unix: u64) -> u64 {
    unix / WEEK
}

/// The text an account signs (EIP-191) to post `want` in `week`. Tied to the week, so an old
/// signature cannot renew a want later.
pub fn want_message(want: &Want, week: u64) -> String {
    format!(
        "{WANT_DOMAIN}\nkind={}\ndomain={}\nregion={}\ncadence={}\nweek={week}",
        name(&want.kind),
        name(&want.domain),
        name(&want.region),
        name(&want.cadence)
    )
}

/// The serde name of a closed-list value.
fn name<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// One shown category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Shown {
    /// The category.
    #[serde(flatten)]
    pub want: Want,
    /// How many accounts want it.
    pub bucket: Bucket,
}

/// The wants, one per category per account (salted account hashes).
#[derive(Clone, Debug, Default)]
pub struct Board {
    /// (account, category) -> the week it was made or last renewed in.
    wants: BTreeMap<(Hash32, Want), u64>,
}

impl Board {
    /// An empty board.
    pub fn new() -> Self {
        Board::default()
    }

    fn live(week: u64, now_week: u64) -> bool {
        now_week.saturating_sub(week) < LIVE_WEEKS
    }

    /// Record `account`'s want at `at`. A category the account already holds is renewed to this
    /// week (`Ok(false)`); a new one counts against [`MAX_WANTS_PER_ACCOUNT`] (`Ok(true)`).
    pub fn add(&mut self, account: Hash32, want: Want, at: u64) -> Result<bool, BountyError> {
        let week = week_of(at);
        if let Some(w) = self.wants.get_mut(&(account, want)) {
            *w = (*w).max(week);
            return Ok(false);
        }
        let held = self
            .wants
            .iter()
            .filter(|((a, _), w)| *a == account && Board::live(**w, week))
            .count();
        if held >= MAX_WANTS_PER_ACCOUNT {
            return Err(BountyError::TooMany);
        }
        self.wants.insert((account, want), week);
        Ok(true)
    }

    /// The week `account`'s want of this category was made or last renewed in.
    pub fn held_since(&self, account: &Hash32, want: &Want) -> Option<u64> {
        self.wants.get(&(*account, *want)).copied()
    }

    /// The board as shown at `now`: every category held by at least `min_accounts` distinct
    /// accounts with a live want made before this week's cut, as a bucket, in category order.
    pub fn published(&self, min_accounts: usize, now: u64) -> Vec<Shown> {
        let cut = week_of(now);
        let mut counts: BTreeMap<Want, usize> = BTreeMap::new();
        for ((_, want), week) in &self.wants {
            // Made before the cut (its week has ended) and still live at the cut.
            if *week < cut && Board::live(*week, cut) {
                *counts.entry(*want).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n >= min_accounts.max(1))
            .map(|(want, n)| Shown {
                want,
                bucket: Bucket::of(n),
            })
            .collect()
    }

    /// When the board shown at `now` next changes (the next cut, unix seconds).
    pub fn next_cut(now: u64) -> u64 {
        (week_of(now) + 1) * WEEK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_793_145_600; // 2026-10-28 00:00 UTC

    fn want() -> Want {
        Want {
            kind: Kind::PublicApi,
            domain: Domain::WeatherClimate,
            region: Region::Europe,
            cadence: Cadence::Daily,
        }
    }

    fn acct(i: u8) -> Hash32 {
        onyx_core::keccak256(&[i])
    }

    /// The Done-when test of NL9: an exact query or subject cannot be posted.
    #[test]
    fn a_want_with_free_text_or_a_subject_is_refused() {
        let ok = br#"{"kind":"public_api","domain":"weather_climate","region":"europe","cadence":"daily"}"#;
        assert_eq!(parse_want(ok), Ok(want()));
        for bad in [
            &br#"{"kind":"public_api","domain":"weather_climate","region":"europe","cadence":"daily","note":"rain in Lyon"}"#[..],
            br#"{"kind":"public_api","domain":"weather_climate","region":"europe","cadence":"daily","subject":"noaa.ghcnd.usw00003017:2026-10-29"}"#,
            br#"{"kind":"public_api","domain":"weather_climate","region":"europe","cadence":"daily","metric":"x.y"}"#,
            br#"{"kind":"public_api","domain":"weather_climate","region":"europe","cadence":"daily","url":"https://example.org/q?x=1"}"#,
            br#"{"kind":"public_api","domain":"weather_climate","region":"europe"}"#,
            br#"{"kind":"public_api","domain":"weather_climate","region":{"city":"Lyon"},"cadence":"daily"}"#,
            br#"["public_api","weather_climate","europe","daily"]"#,
            br#"rain in Lyon"#,
        ] {
            assert_eq!(parse_want(bad), Err(BountyError::TooSpecific), "{}", String::from_utf8_lossy(bad));
        }
        let big = format!(
            r#"{{"kind":"public_api","domain":"{}","region":"europe","cadence":"daily"}}"#,
            "x".repeat(300)
        );
        assert_eq!(parse_want(big.as_bytes()), Err(BountyError::TooLarge));
    }

    #[test]
    fn unknown_enum_values_are_refused() {
        for bad in [
            &br#"{"kind":"rumour","domain":"finance","region":"global","cadence":"once"}"#[..],
            br#"{"kind":"chain","domain":"crypto_whales","region":"global","cadence":"once"}"#,
            br#"{"kind":"chain","domain":"finance","region":"lyon","cadence":"once"}"#,
            br#"{"kind":"chain","domain":"finance","region":"global","cadence":"hourly"}"#,
            br#"{"kind":"Chain","domain":"finance","region":"global","cadence":"once"}"#,
        ] {
            assert!(
                matches!(parse_want(bad), Err(BountyError::BadValue(_))),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn a_category_below_k_is_not_published() {
        let mut b = Board::new();
        for i in 0..4 {
            b.add(acct(i), want(), T).expect("test");
        }
        // The same account twice is one account.
        b.add(acct(0), want(), T).expect("test");
        assert!(b.published(MIN_ACCOUNTS, T + 2 * WEEK).is_empty());
        b.add(acct(4), want(), T).expect("test");
        assert_eq!(
            b.published(MIN_ACCOUNTS, T + WEEK),
            vec![Shown {
                want: want(),
                bucket: Bucket::UpTo19
            }]
        );
    }

    #[test]
    fn counts_are_bucketed() {
        assert_eq!(
            [1, 4, 5, 19, 20, 900].map(Bucket::of),
            [
                Bucket::UpTo4,
                Bucket::UpTo4,
                Bucket::UpTo19,
                Bucket::UpTo19,
                Bucket::Many,
                Bucket::Many
            ]
        );
        let mut b = Board::new();
        for i in 0..25 {
            b.add(acct(i), want(), T).expect("test");
        }
        let shown = b.published(MIN_ACCOUNTS, T + WEEK);
        assert_eq!(shown[0].bucket, Bucket::Many);
        let json = serde_json::to_value(shown[0]).expect("test");
        assert_eq!(
            json,
            serde_json::json!({"kind":"public_api","domain":"weather_climate","region":"europe","cadence":"daily","bucket":"20+"})
        );
    }

    #[test]
    fn the_board_moves_only_at_the_weekly_cut_and_wants_lapse() {
        let mut b = Board::new();
        for i in 0..5 {
            b.add(acct(i), want(), T).expect("test");
        }
        // Not this week: the wants were made after this week's cut.
        assert!(b.published(MIN_ACCOUNTS, T).is_empty());
        let next = Board::next_cut(T);
        assert!(b.published(MIN_ACCOUNTS, next - 1).is_empty());
        assert_eq!(b.published(MIN_ACCOUNTS, next).len(), 1);
        // Lapsed after LIVE_WEEKS weeks unless renewed.
        assert!(b.published(MIN_ACCOUNTS, T + LIVE_WEEKS * WEEK).is_empty());
        for i in 0..5 {
            assert_eq!(b.add(acct(i), want(), T + 2 * WEEK), Ok(false), "a renewal");
        }
        assert_eq!(b.published(MIN_ACCOUNTS, T + LIVE_WEEKS * WEEK).len(), 1);
    }

    #[test]
    fn one_account_holds_at_most_its_quota() {
        let mut b = Board::new();
        let mut n = 0;
        'all: for kind in [Kind::Chain, Kind::PublicApi, Kind::Signed] {
            for domain in [Domain::Finance, Domain::Energy, Domain::Transport] {
                for cadence in [Cadence::Once, Cadence::Daily] {
                    let w = Want {
                        kind,
                        domain,
                        region: Region::Global,
                        cadence,
                    };
                    if n == MAX_WANTS_PER_ACCOUNT {
                        assert_eq!(b.add(acct(1), w, T), Err(BountyError::TooMany));
                        break 'all;
                    }
                    assert_eq!(b.add(acct(1), w, T), Ok(true));
                    n += 1;
                }
            }
        }
        assert_eq!(n, MAX_WANTS_PER_ACCOUNT);
    }

    #[test]
    fn the_signed_text_names_the_category_and_the_week() {
        assert_eq!(
            want_message(&want(), 2_964),
            "onyx-want-v1\nkind=public_api\ndomain=weather_climate\nregion=europe\ncadence=daily\nweek=2964"
        );
    }
}
