//! Borsh decoding (ADR-0002) of every protocol type a client, validator,
//! node or browser verifier decodes from bytes it did not make: the first
//! byte picks the type. Decoding never panics or allocates without bound,
//! and is canonical: whatever decodes re-encodes to exactly the input.
//! The decoded value's own checks (validate, hash, id) run too.
#![no_main]

use borsh::{BorshDeserialize, BorshSerialize};
use libfuzzer_sys::fuzz_target;
use onyx_core::{from_canonical_bytes, to_canonical_bytes};

/// Decode `bytes` as `T`; on success it must re-encode to `bytes`.
fn canon<T: BorshDeserialize + BorshSerialize>(bytes: &[u8]) -> Option<T> {
    let v: T = from_canonical_bytes(bytes).ok()?;
    let again = to_canonical_bytes(&v).expect("a decoded value encodes");
    assert_eq!(again, bytes, "non-canonical encoding accepted");
    Some(v)
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, b)) = data.split_first() else {
        return;
    };
    let reg = onyx_fuzz_common::registry();
    match sel % 40 {
        0 => {
            if let Some(c) = canon::<onyx_core::Claim>(b) {
                let _ = c.validate(&reg);
                let _ = onyx_core::Claim::decode(b, &reg);
            }
        }
        1 => {
            if let Some(s) = canon::<onyx_core::ShardSpec>(b) {
                let _ = s.validate(&reg);
                let rights = onyx_core::DataRights::public_chain(s.source.source.clone());
                let _ = onyx_api::catalog::ShardMeta::from_spec(1, &s, "m", rights);
                let _ = s.shard_id();
                let _ = onyx_api::catalog::ShardBinding::from_spec(&s).shard_id();
            }
        }
        2 => {
            if let Some(c) = canon::<onyx_core::Commitment>(b) {
                let _ = c.validate();
            }
        }
        3 => {
            if let Some(r) = canon::<onyx_core::RuntimeProfile>(b) {
                let _ = r.hash();
            }
        }
        4 => {
            if let Some(r) = canon::<onyx_core::DataRights>(b) {
                let _ = r.hash();
            }
        }
        5 => {
            if let Some(s) = canon::<onyx_core::SourceRef>(b) {
                let _ = s.validate();
                let _ = s.hash();
                let _ = onyx_api::catalog::snapshot_end(&s.snapshot);
            }
        }
        6 => {
            if let Some(v) = canon::<onyx_core::Value>(b) {
                let _ = v.validate();
            }
        }
        7 => {
            if let Some(t) = canon::<onyx_core::FeedTemplate>(b) {
                let _ = t.validate();
                let _ = t.id();
            }
        }
        8 => {
            if let Some(s) = canon::<onyx_core::ObservationSpec>(b) {
                let _ = s.validate();
                let _ = s.id();
            }
        }
        9 => {
            let _ = canon::<onyx_core::Observation>(b);
        }
        10 => {
            let _ = canon::<onyx_core::Answer>(b);
        }
        11 => {
            let _ = canon::<onyx_core::MarketPolicy>(b);
        }
        12 => {
            if let Some(p) = canon::<onyx_core::SignedPayload>(b) {
                let _ = p.validate();
                let _ = p.digest();
                let _ = p.message();
            }
        }
        13 => {
            if let Some(r) = canon::<onyx_core::SignedReport>(b) {
                let _ = r.payload.validate();
                let _ = r.hash();
            }
            let _ = onyx_coordinator::signed::decode_report(b);
        }
        14 => {
            if let Some(k) = canon::<onyx_core::SourceKey>(b) {
                let _ = k.validate();
                let _ = k.id();
            }
        }
        15 => {
            if let Some(l) = canon::<onyx_core::LabelSetRef>(b) {
                let _ = l.validate();
                let _ = l.hash();
            }
        }
        16 => {
            let _ = canon::<onyx_core::EvidenceRef>(b);
        }
        17 => {
            let _ = canon::<onyx_core::Subject>(b);
        }
        18 => {
            let _ = canon::<onyx_core::MetricInfo>(b);
        }
        19 => {
            let _ = canon::<onyx_core::Hash32>(b);
        }
        20 => {
            // A Merkle proof from a server: verifying it never panics.
            if let Some(p) = canon::<onyx_merkle::Proof>(b) {
                let _ = onyx_merkle::verify(&onyx_core::Hash32::ZERO, b"row", &p);
            }
        }
        21 => {
            if let Some(s) = canon::<onyx_labels::LabelSet>(b) {
                let _ = s.validate(&reg);
                let _ = s.content_hash();
            }
        }
        22 => {
            let _ = canon::<onyx_labels::Label>(b);
        }
        23 => {
            if let Some(a) = canon::<onyx_labels::attest::Attestation>(b) {
                let _ = a.content_hash();
                let _ = onyx_labels::attest::AttestCache::new();
            }
        }
        24 => {
            if let Some(r) = canon::<onyx_labels::revoke::Revocation>(b) {
                let _ = r.content_hash();
                let _ = r.verify(&reg);
            }
        }
        25 => {
            if let Some(m) = canon::<onyx_labels::version::Manifest>(b) {
                let _ = onyx_labels::version::VersionLog::verify(
                    std::slice::from_ref(&m),
                    &onyx_core::Hash32::ZERO,
                );
            }
        }
        26 => {
            let _ = canon::<onyx_recipe_sdk::RecipeInput>(b);
        }
        27 => {
            let _ = canon::<onyx_recipe_sdk::LabelSetView>(b);
        }
        28 => {
            let _ = canon::<onyx_recipe_sdk::OutputRow>(b);
        }
        29 => {
            let _ = canon::<onyx_recipe_sdk::RecipeOutput>(b);
        }
        30 => {
            let _ = canon::<onyx_recipe_sdk::RunReturn>(b);
        }
        31 => {
            let _ = canon::<onyx_btc_schema::BtcSource>(b);
        }
        32 => {
            let _ = canon::<onyx_btc_schema::EntityFlowRow>(b);
        }
        33 => {
            let _ = canon::<onyx_btc_schema::EntityBlockFlowRow>(b);
        }
        34 => {
            let _ = canon::<onyx_btc_schema::BalanceParams>(b);
        }
        35 => {
            let _ = canon::<onyx_pool_schema::CoinbaseSource>(b);
            let _ = canon::<onyx_pool_schema::PoolTable>(b);
        }
        36 => {
            let _ = canon::<onyx_pool_schema::PoolBlocksRow>(b);
        }
        37 => {
            let _ = canon::<onyx_reserve_schema::ReserveParams>(b);
            let _ = canon::<onyx_reserve_schema::EntityReserveRow>(b);
        }
        38 => {
            let _ = canon::<onyx_coordinator::observe::Observer>(b);
            let _ = canon::<onyx_coordinator::observe::DrawRecord>(b);
            let _ = canon::<onyx_coordinator::observe::ObserveConfig>(b);
        }
        _ => {
            let _ = canon::<onyx_coordinator::observe::PostedCommit>(b);
            let _ = canon::<onyx_coordinator::observe::ArrivedReveal>(b);
            let _ = canon::<onyx_coordinator::observe::LogBlock>(b);
            let _ = canon::<onyx_coordinator::observe::DrawSeed>(b);
        }
    }
});
