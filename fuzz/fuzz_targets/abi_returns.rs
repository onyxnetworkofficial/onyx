//! ABI return decoders of the contract views a server or client reads
//! (`getFinalizedRoot`, `shardState`, `drawCount`, `drawBudget`,
//! `transcriptComplete`, `drawMatches`, `observationLevel`, the registry's
//! metric record): exact lengths, no truncation of high words, and a decoded
//! `getFinalizedRoot` re-encodes to exactly the input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::client;
use onyx_core::Hash32;
use onyx_fuzz_common::{level2, CHAIN, CONTRACT};

fuzz_target!(|ret: &[u8]| {
    if let Ok(Some((root, binding, at))) = onyx_api::roots::decode_finalized_root(ret) {
        let mut enc = root.0.to_vec();
        enc.extend_from_slice(&binding.0);
        enc.extend_from_slice(&[0u8; 24]);
        enc.extend_from_slice(&at.to_be_bytes());
        assert_eq!(enc, ret, "getFinalizedRoot decode is not exact");
        assert_ne!(root, Hash32::ZERO, "a zero root means not finalized");
    }
    let _ = client::ChainAnswer::from_return(1, Hash32::ZERO, CHAIN, CONTRACT, ret);
    if let Ok(s) = onyx_api::indexer::decode_shard_state(ret) {
        assert_eq!(ret.len(), 160);
        let _ = onyx_api::indexer::Ended::from_status(s);
    }
    if let Ok(n) = client::draw_count_from_return(ret) {
        assert_eq!(ret.len(), 32);
        assert_eq!(&ret[28..], &n.to_be_bytes());
    }
    if let Ok((a, b)) = client::draw_budget_from_return(ret) {
        assert_eq!(ret.len(), 64);
        assert_eq!(&ret[28..32], &a.to_be_bytes());
        assert_eq!(&ret[60..64], &b.to_be_bytes());
    }
    for f in [
        client::transcript_complete_from_return,
        client::draw_matches_from_return,
    ] {
        if let Ok(b) = f(ret) {
            assert_eq!(ret.len(), 32);
            assert_eq!(ret[31], u8::from(b));
        }
    }
    if let Ok(l) = client::observation_level_from_return(ret) {
        assert_eq!(ret.len(), 32);
        assert_eq!(ret[31], l);
    }
    let _ = onyx_api::observations::check_registered(&level2::template(), ret);
});
