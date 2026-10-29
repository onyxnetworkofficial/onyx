//! ROADMAP B6: a live round's `--record-date latest` is the newest day the
//! daily rule allows for the window (the coordinator never picks the day).
#![allow(clippy::unwrap_used)]

use onyx_core::feed::{civil_date, day_number};
use onyx_level2::Source;
use onyx_source_usgs::round::Usgs;
use onyx_source_usgs::{spec, MIN_DAYS};

#[test]
fn latest_is_the_newest_day_the_rule_allows() {
    for w in [1_793_100_000_u64, 1_793_100_000 + 86_399, 1_800_000_017] {
        let today = i64::try_from(w / 86_400).unwrap();
        let day = Usgs.latest(w).unwrap();
        assert_eq!(day_number(&day), Some(today - i64::from(MIN_DAYS)), "{w}");
        assert!(spec(&day, w).is_ok());
        // One day newer is not settled yet.
        assert!(spec(&civil_date(today - i64::from(MIN_DAYS) + 1), w).is_err());
    }
}
