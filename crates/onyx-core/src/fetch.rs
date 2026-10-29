//! Polite level-2 fetching (partner tracker M2, from a partner's question Q4).
//!
//! Every drawn observer used to fetch the source at the window start, so a
//! round of 16 observers hit one origin 16 times in the same second, and a
//! rate-limited source could block all of them. Two rules fix that:
//!
//! - [`fetch_time`]: each observer fetches at its own second inside the
//!   window, spread by a hash of the round id and its address. Anyone can
//!   recompute an observer's slot; nobody can pick it, because the round id
//!   is fixed by the draw. The last [`FETCH_MARGIN_SECS`] of the window stay
//!   free so a late slot still fetches inside the window.
//! - [`url_origin`]: the origin (lowercase host) a spec's URL points at, the
//!   key of the coordinator's per-origin budget
//!   (`onyx_coordinator::origin_budget`).
//!
//! The slot is a courtesy rule for honest observers: the coordinator cannot
//! see when a page was fetched, only when its commit arrived.

use crate::commitment::MinerAddress;
use crate::hash::{keccak256, Hash32};
use crate::proof::ObservationSpec;
use crate::signed::is_origin_host;

/// Seconds at the end of a window that no fetch slot uses.
pub const FETCH_MARGIN_SECS: u64 = 30;

/// Domain tag of the slot hash.
const SLOT_TAG: &[u8] = b"onyx-fetch-slot:v1";

/// Seconds after the window start at which `observer` fetches for `round`,
/// for a window of `window_len` seconds: in `0..window_len - FETCH_MARGIN_SECS`,
/// or 0 when the window is no longer than the margin.
pub fn fetch_offset(round: &Hash32, observer: &MinerAddress, window_len: u64) -> u64 {
    let spread = window_len.saturating_sub(FETCH_MARGIN_SECS);
    if spread == 0 {
        return 0;
    }
    let mut msg = Vec::with_capacity(SLOT_TAG.len() + 32 + 20);
    msg.extend_from_slice(SLOT_TAG);
    msg.extend_from_slice(&round.0);
    msg.extend_from_slice(observer);
    let h = keccak256(&msg);
    let mut word = [0u8; 8];
    word.copy_from_slice(&h.0[..8]);
    u64::from_be_bytes(word) % spread
}

/// The unix second at which `observer` fetches `spec` for `round`.
pub fn fetch_time(spec: &ObservationSpec, round: &Hash32, observer: &MinerAddress) -> u64 {
    let len = spec.window_end.saturating_sub(spec.window_start);
    spec.window_start + fetch_offset(round, observer, len)
}

/// The lowercase host of an `https://` URL, without port, path, query or
/// fragment. `None` for any other scheme, a URL with user information
/// (`https://a@b/`), or a host that is not a DNS name with two or more labels
/// (an IP address included, [`is_origin_host`]), so one origin cannot hide
/// behind another spelling.
pub fn url_origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        Some(_) => return None,
        None => authority,
    };
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    is_origin_host(&host).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(b: u8) -> Hash32 {
        Hash32([b; 32])
    }

    #[test]
    fn offsets_stay_inside_the_spread() {
        for i in 0..200u8 {
            let off = fetch_offset(&round(i), &[i; 20], 300);
            assert!(off < 300 - FETCH_MARGIN_SECS, "offset {off}");
        }
    }

    #[test]
    fn short_windows_fetch_at_the_start() {
        assert_eq!(fetch_offset(&round(1), &[1; 20], FETCH_MARGIN_SECS), 0);
        assert_eq!(fetch_offset(&round(1), &[1; 20], 0), 0);
        assert_eq!(fetch_offset(&round(1), &[1; 20], FETCH_MARGIN_SECS + 1), 0);
    }

    #[test]
    fn sixteen_observers_do_not_share_one_second() {
        // The case in Q4: 16 observers on one origin in a 300 s window.
        let r = round(7);
        let mut slots: Vec<u64> = (0..16u8)
            .map(|i| fetch_offset(&r, &[i.wrapping_mul(37).wrapping_add(1); 20], 300))
            .collect();
        slots.sort_unstable();
        let distinct = {
            let mut s = slots.clone();
            s.dedup();
            s.len()
        };
        assert!(distinct >= 12, "slots {slots:?}");
        // Never more than 2 observers in the same second.
        for w in slots.windows(3) {
            assert!(!(w[0] == w[1] && w[1] == w[2]), "slots {slots:?}");
        }
    }

    #[test]
    fn the_slot_depends_on_round_and_observer() {
        let a = fetch_offset(&round(1), &[9; 20], 3_600);
        assert_eq!(a, fetch_offset(&round(1), &[9; 20], 3_600));
        let others = (2..20u8)
            .map(|i| fetch_offset(&round(i), &[9; 20], 3_600))
            .filter(|o| *o != a)
            .count();
        assert!(others >= 16);
    }

    #[test]
    fn origins() {
        let o = |u| url_origin(u);
        assert_eq!(
            o("https://api.fiscaldata.treasury.gov/services/x?a=1").as_deref(),
            Some("api.fiscaldata.treasury.gov")
        );
        assert_eq!(
            o("https://API.Example.COM").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(
            o("https://example.com.:443/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(o("https://example.com#f").as_deref(), Some("example.com"));
        assert_eq!(o("http://example.com/"), None);
        assert_eq!(o("https://user@example.com/"), None);
        assert_eq!(o("https://example.com:x/"), None);
        assert_eq!(o("https:///x"), None);
        assert_eq!(o("https://.example.com/"), None);
        assert_eq!(o("https://127.0.0.1/"), None);
        assert_eq!(o("https://[::1]/"), None);
        assert_eq!(o("https://localhost/"), None);
    }
}
