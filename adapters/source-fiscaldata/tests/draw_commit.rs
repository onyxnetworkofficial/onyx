//! L2RND-R3-F1/F2 (was the reviewer's `r3_post_beacon_grind` proof): the
//! committee depends on the pool, the spec id (so the window start, which the
//! dated fact key ignores) and `cfg.outlier_slash` (the draw's `min_bond`).
//! The registry's draw commits all three before the beacon exists, and
//! `recordObservation` / `drawMatches` refuse any other: of every window and
//! config variant a coordinator could pick after the beacon, exactly the
//! committed one matches the recorded draw.
use onyx_coordinator::observe::{cfg_hash, draw_commit, pool_hash};
use onyx_source_fiscaldata::round::{config, recorded_draw, rehearsal_pool};
use onyx_source_fiscaldata::{spec, template, RECORDED_WINDOW_START};

const DATE: &str = "2026-10-08";

#[test]
fn one_announced_draw_reaches_exactly_one_window() {
    let rec = recorded_draw();
    let pool = pool_hash(&rehearsal_pool());
    let t = template();
    let mut matching = Vec::new();
    let mut valid = 0;
    for off in 0..20_000u64 {
        let ws = RECORDED_WINDOW_START + off * 7;
        let Ok(s) = spec(DATE, ws) else { continue };
        assert!(t.matches(&s).is_ok());
        valid += 1;
        if draw_commit(&pool, &s.id(), &cfg_hash(&config(&s))) == rec.commit {
            matching.push(ws);
        }
    }
    assert!(valid > 1_000, "the template accepts many windows ({valid})");
    assert_eq!(
        matching,
        vec![RECORDED_WINDOW_START],
        "only the committed window's spec matches the announced draw"
    );
}

#[test]
fn one_announced_draw_reaches_exactly_one_config_and_pool() {
    let rec = recorded_draw();
    let members = rehearsal_pool();
    let s = spec(DATE, RECORDED_WINDOW_START).expect("spec");
    let cfg = config(&s);
    let commit = |pool: &[_], c| draw_commit(&pool_hash(pool), &s.id(), &cfg_hash(&c));
    assert_eq!(commit(&members, cfg), rec.commit, "the committed draw");
    // Every other `min_bond` (outlier slash) the coordinator could pick after
    // the beacon to filter the pool: refused.
    for slash in (0..=10_000u128).step_by(7) {
        let mut c = cfg;
        c.outlier_slash = slash;
        assert_eq!(
            commit(&members, c) == rec.commit,
            slash == cfg.outlier_slash
        );
    }
    // Every other config field, and a pool with one bond changed: refused.
    let mut c = cfg;
    c.reward += 1;
    assert_ne!(commit(&members, c), rec.commit);
    let mut c = cfg;
    c.miss_slash += 1;
    assert_ne!(commit(&members, c), rec.commit);
    let mut other = members.clone();
    other[0].bond += 1;
    assert_ne!(commit(&other, cfg), rec.commit);
}

fn choose(n: u64, k: u64) -> u64 {
    (0..k).fold(1, |acc, i| acc * (n - i) / (i + 1))
}

/// L2RND-R3-F4: the L2-F2 residual with the rehearsal's numbers (pool 9,
/// draw 5, a strict majority is 3). Odds a fixed set of `c` colluders holds a
/// majority of one fair draw, and of the best of the 3 public draws the
/// subject bound allows, in per mille (rounded down).
#[test]
fn collusion_odds_of_the_rehearsal_pool() {
    let (pool, draw, majority) = (9u64, 5u64, 3u64);
    let odds = |c: u64| {
        let all = choose(pool, draw);
        let win: u64 = (majority..=c.min(draw))
            .map(|k| choose(c, k) * choose(pool - c, draw - k))
            .sum();
        let one = win * 1_000 / all;
        let best3 = (all.pow(3) - (all - win).pow(3)) * 1_000 / all.pow(3);
        (win, all, one, best3)
    };
    assert_eq!(odds(3), (15, 126, 119, 316), "3 colluders: 11.9%, 31.6%");
    assert_eq!(odds(4), (45, 126, 357, 734), "4 colluders: 35.7%, 73.4%");
}
