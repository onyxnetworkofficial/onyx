//! Shard levels bound on chain (AUD-2R-F3): when the deployment pins an
//! `OnyxRegistry`, each catalog shard is floored at the level the registry
//! holds for it (`levelOf`). The registry refused any attestation below the
//! label set's or the metric's registered minimum (`metricMinLevel`, e.g. 4
//! for a mining-pool shard whose pool table is a third-party list), so an
//! operator's catalog file cannot lift a shard above that floor. The floor is
//! keyed by recipe on chain (REG-F-F2), and the node also requires the metric
//! the shard was attested under to be the catalog's metric: a shard attested
//! under another metric is treated as unattested (weakest level).

use crate::catalog::Catalog;
use crate::roots::JsonRpc;
use onyx_core::claim::MetricId;
use onyx_core::onchain::metric_key;
use onyx_core::{keccak256, Hash32, ProofLevel};
use serde_json::json;

/// `levelOf(uint64,bytes32)` calldata for (round, shard).
pub fn level_of_calldata(round: u64, shard: &Hash32) -> Vec<u8> {
    let mut data = keccak256(b"levelOf(uint64,bytes32)").0[..4].to_vec();
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&round.to_be_bytes());
    data.extend_from_slice(&word);
    data.extend_from_slice(&shard.0);
    data
}

/// `attestationOf(uint64,bytes32)` calldata for (round, shard).
pub fn attestation_of_calldata(round: u64, shard: &Hash32) -> Vec<u8> {
    let mut data = keccak256(b"attestationOf(uint64,bytes32)").0[..4].to_vec();
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&round.to_be_bytes());
    data.extend_from_slice(&word);
    data.extend_from_slice(&shard.0);
    data
}

/// The metric key, recipe id and label set hash an `attestationOf` return
/// names (its first three words; the struct is all static types, so the
/// return is 7 words).
fn attested_keys(ret: &[u8]) -> Result<[Hash32; 3], String> {
    if ret.len() != 7 * 32 {
        return Err(format!(
            "attestationOf returned {} bytes, want 224",
            ret.len()
        ));
    }
    let word = |i: usize| {
        let mut w = [0u8; 32];
        w.copy_from_slice(&ret[i * 32..(i + 1) * 32]);
        Hash32(w)
    };
    Ok([word(0), word(1), word(2)])
}

/// The level a `levelOf` return gives; `None` for 0 (no attestation, or not
/// final).
pub fn level_of_return(ret: &[u8]) -> Result<Option<ProofLevel>, String> {
    let word: [u8; 32] = ret
        .try_into()
        .map_err(|_| format!("levelOf returned {} bytes, want 32", ret.len()))?;
    if word[..31].iter().any(|b| *b != 0) {
        return Err("levelOf returned a value above 255".into());
    }
    match word[31] {
        0 => Ok(None),
        n => ProofLevel::from_number(n)
            .map(Some)
            .ok_or_else(|| format!("levelOf returned level {n}")),
    }
}

fn hex_of(v: serde_json::Value, what: &str) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or_else(|| format!("{what}: not a string"))?;
    hex::decode(
        s.strip_prefix("0x")
            .ok_or_else(|| format!("{what}: no 0x"))?,
    )
    .map_err(|e| format!("{what}: {e}"))
}

/// Refuse unless the registry at `registry` runs the pinned build.
pub async fn check_registry_code(
    rpc: &dyn JsonRpc,
    registry: [u8; 20],
    code_hash: Hash32,
) -> Result<(), String> {
    let to = format!("0x{}", hex::encode(registry));
    let code = rpc
        .call("eth_getCode", json!([to, "latest"]))
        .await
        .map_err(|e| format!("registry code read failed: {e}"))?;
    let code = hex_of(code, "registry code")?;
    if code.is_empty() || keccak256(&code) != code_hash {
        return Err(
            "the registry's runtime code is not the pinned build (registryCodeHash)".into(),
        );
    }
    Ok(())
}

/// `ledger()` calldata (`OnyxRegistry.ledger`, an immutable).
pub fn ledger_calldata() -> Vec<u8> {
    keccak256(b"ledger()").0[..4].to_vec()
}

/// The address a `ledger()` return names: one ABI word whose top 12 bytes
/// are zero.
pub fn registry_ledger_of(ret: &serde_json::Value) -> Result<[u8; 20], String> {
    let b = hex_of(ret.clone(), "ledger return")?;
    let word: [u8; 32] = b
        .as_slice()
        .try_into()
        .map_err(|_| format!("ledger() returned {} bytes, want 32", b.len()))?;
    if word[..12].iter().any(|x| *x != 0) {
        return Err("ledger() returned a value that is not an address".into());
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&word[12..]);
    Ok(a)
}

/// Refuse unless the registry at `registry` runs the pinned build
/// ([`check_registry_code`]) and is bound to `settlement`, the manifest's
/// pinned ledger (L16, THREATS L2R2-F2): the code hash covers the registry's
/// immutables, but only when the hash is the real deployment's; this check
/// holds even for a manifest whose (registry, registryCodeHash) pair names
/// another deployment's registry. Errors name no node URL.
pub async fn check_registry(
    rpc: &dyn JsonRpc,
    registry: [u8; 20],
    code_hash: Hash32,
    settlement: [u8; 20],
) -> Result<(), String> {
    check_registry_code(rpc, registry, code_hash).await?;
    let to = format!("0x{}", hex::encode(registry));
    let data = format!("0x{}", hex::encode(ledger_calldata()));
    let ret = rpc
        .call("eth_call", json!([{"to": to, "data": data}, "latest"]))
        .await
        .map_err(|e| format!("registry ledger() failed: {e}"))?;
    if registry_ledger_of(&ret)? != settlement {
        return Err(
            "the registry is not bound to the manifest's settlement contract (ledger())".into(),
        );
    }
    Ok(())
}

/// The level the registry attested for shard (round, `shard`) under
/// `metric`: `None` when it has none (never attested, or not final), or
/// when it attested the shard under another metric (REG-F-F2), or for
/// another recipe or label set than `recipe` and `label_set`, the catalog's
/// binding (review L16-R2: the shard id commits to both on chain, so this
/// only holds if the two shard id encodings ever drift apart; the attested
/// level stands in for an unknown label level, so it must be this label
/// set's). The caller checked the registry first ([`check_registry`]).
pub async fn registry_level(
    rpc: &dyn JsonRpc,
    registry: [u8; 20],
    round: u64,
    shard: &Hash32,
    metric: &str,
    recipe: &Hash32,
    label_set: &Hash32,
) -> Result<Option<ProofLevel>, String> {
    let to = format!("0x{}", hex::encode(registry));
    let data = format!("0x{}", hex::encode(level_of_calldata(round, shard)));
    let ret = rpc
        .call("eth_call", json!([{"to": to, "data": data}, "latest"]))
        .await
        .map_err(|e| format!("registry levelOf failed: {e}"))?;
    let level = level_of_return(&hex_of(ret, "levelOf return")?)?;
    if level.is_none() {
        return Ok(None);
    }
    let data = format!("0x{}", hex::encode(attestation_of_calldata(round, shard)));
    let ret = rpc
        .call("eth_call", json!([{"to": to, "data": data}, "latest"]))
        .await
        .map_err(|e| format!("registry attestationOf failed: {e}"))?;
    let [m, r, l] = attested_keys(&hex_of(ret, "attestationOf return")?)?;
    if m != metric_key(&MetricId(metric.to_string())) || r != *recipe || l != *label_set {
        return Ok(None);
    }
    Ok(level)
}

/// Check the registry's runtime code is the pinned build and its ledger the
/// pinned settlement ([`check_registry`]), then floor every
/// shard of `catalog` at its on-chain level. A shard the registry has no
/// level for (never attested, or not final) is floored to the weakest level,
/// so it never settles a market; so is a shard the registry attested under a
/// metric other than the catalog's (REG-F-F2). Returns how many shards that was.
/// Shards the chain indexer admits later are floored the same way, one at a
/// time ([`registry_level`], ADR-0033).
pub async fn floor_to_registry(
    rpc: &dyn JsonRpc,
    registry: [u8; 20],
    code_hash: Hash32,
    settlement: [u8; 20],
    catalog: &mut Catalog,
) -> Result<usize, String> {
    check_registry(rpc, registry, code_hash, settlement).await?;
    let mut unattested = 0;
    for (round, shard) in catalog.shard_keys() {
        let Some((metric, binding)) = catalog
            .shard(round, &shard)
            .map(|m| (m.metric.clone(), m.binding))
        else {
            continue;
        };
        let level = registry_level(
            rpc,
            registry,
            round,
            &shard,
            &metric,
            &binding.recipe_id,
            &binding.label_set_hash,
        )
        .await?
        .unwrap_or_else(|| {
            unattested += 1;
            ProofLevel::TrackRecord
        });
        catalog.floor_shard_level(round, &shard, level);
    }
    Ok(unattested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::BoxFuture;
    use serde_json::Value;

    #[test]
    fn level_of_calldata_is_the_abi_encoding() {
        let shard = Hash32([7; 32]);
        let d = level_of_calldata(258, &shard);
        assert_eq!(d.len(), 68);
        assert_eq!(&d[..4], &keccak256(b"levelOf(uint64,bytes32)").0[..4]);
        assert_eq!(&d[34..36], &[1, 2]);
        assert_eq!(&d[36..], &[7; 32]);
    }

    #[test]
    fn a_levelof_return_is_one_word_and_a_real_level() {
        let word = |n: u8| {
            let mut w = [0u8; 32];
            w[31] = n;
            w
        };
        assert_eq!(level_of_return(&word(0)), Ok(None));
        assert_eq!(level_of_return(&word(4)), Ok(Some(ProofLevel::BondedClaim)));
        assert!(level_of_return(&word(9)).is_err());
        assert!(level_of_return(&word(4)[..31]).is_err());
        let mut big = word(4);
        big[0] = 1;
        assert!(level_of_return(&big).is_err());
    }

    /// Answers `eth_getCode` with `code`, `levelOf` from the last byte of the
    /// shard id (0x01 and 0x03 attested 4, anything else unattested) and
    /// `attestationOf` naming `test.flow` for 0x01 and another metric for 0x03.
    struct Registry {
        code: Vec<u8>,
        /// What `ledger()` returns.
        ledger: [u8; 20],
        /// The recipe and label set every attestation names.
        recipe: Hash32,
        label_set: Hash32,
    }

    impl Registry {
        fn new(code: &[u8], ledger: [u8; 20]) -> Self {
            Registry {
                code: code.to_vec(),
                ledger,
                recipe: Hash32::ZERO,
                label_set: Hash32::ZERO,
            }
        }
    }

    impl JsonRpc for Registry {
        fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
            let v = match method {
                "eth_getCode" => Ok(json!(format!("0x{}", hex::encode(&self.code)))),
                "eth_call"
                    if p[0]["data"].as_str()
                        == Some(&format!("0x{}", hex::encode(ledger_calldata()))) =>
                {
                    let mut w = [0u8; 32];
                    w[12..].copy_from_slice(&self.ledger);
                    Ok(json!(format!("0x{}", hex::encode(w))))
                }
                "eth_call" => {
                    let data = p[0]["data"].as_str().unwrap_or_default().to_string();
                    let attested = data.ends_with("01") || data.ends_with("03");
                    let sel = format!(
                        "0x{}",
                        hex::encode(&keccak256(b"attestationOf(uint64,bytes32)").0[..4])
                    );
                    if data.starts_with(&sel) {
                        let m = if data.ends_with("01") {
                            "test.flow"
                        } else {
                            "other.metric"
                        };
                        let mut ret = metric_key(&MetricId(m.into())).0.to_vec();
                        ret.extend_from_slice(&self.recipe.0);
                        ret.extend_from_slice(&self.label_set.0);
                        ret.extend_from_slice(&[0u8; 4 * 32]);
                        Ok(json!(format!("0x{}", hex::encode(ret))))
                    } else {
                        let level = if attested { 4 } else { 0 };
                        Ok(json!(format!("0x{:064x}", level)))
                    }
                }
                _ => Err("unexpected".to_string()),
            };
            Box::pin(async move { v })
        }
    }

    const SETTLEMENT: [u8; 20] = [0x5e; 20];

    fn catalog_with(shards: &[u8]) -> Catalog {
        use onyx_core::{DataRights, ShardSpec, Snapshot, SourceId, SourceRef};
        let src = SourceId("chain.test".into());
        let spec = ShardSpec {
            recipe: None,
            runtime: None,
            label_set: None,
            source: SourceRef {
                source: src.clone(),
                snapshot: Snapshot::Block {
                    chain: onyx_core::ChainId("test".into()),
                    height: 10,
                    block_hash: keccak256(b"b"),
                },
            },
            schema: keccak256(b"schema"),
            rights_hash: DataRights::public_chain(src.clone()).hash(),
            params: vec![],
            registry_root: keccak256(b"r"),
        };
        let mut c = Catalog::new();
        for (i, last) in shards.iter().enumerate() {
            let mut m = crate::catalog::ShardMeta::from_spec(
                i as u64 + 1,
                &spec,
                "test.flow",
                DataRights::public_chain(src.clone()),
            )
            .expect("meta");
            let mut id = [0u8; 32];
            id[31] = *last;
            m.shard = Hash32(id);
            assert_eq!(m.proof_level, ProofLevel::Recomputed);
            c.add_shard(m);
        }
        c
    }

    #[tokio::test]
    async fn every_shard_is_floored_at_its_registry_level() {
        let code = b"registry runtime".to_vec();
        let rpc = Registry::new(&code, SETTLEMENT);
        let mut c = catalog_with(&[1, 2, 3]);
        // The wrong build is refused before any level is read.
        let e = floor_to_registry(&rpc, [1; 20], Hash32([0; 32]), SETTLEMENT, &mut c)
            .await
            .expect_err("wrong code");
        assert!(e.contains("registryCodeHash"), "{e}");
        let n = floor_to_registry(&rpc, [1; 20], keccak256(&code), SETTLEMENT, &mut c)
            .await
            .expect("floored");
        assert_eq!(n, 2);
        let levels: Vec<_> = c
            .shard_keys()
            .iter()
            .map(|(r, s)| c.shard(*r, s).expect("shard").proof_level)
            .collect();
        // The catalog said level 1 for all: the chain's 4 (a pool shard's
        // recipe floor) and "unattested" (weakest) win; a shard attested under
        // another metric counts as unattested (REG-F-F2).
        assert_eq!(
            levels,
            vec![
                ProofLevel::BondedClaim,
                ProofLevel::TrackRecord,
                ProofLevel::TrackRecord
            ]
        );
    }
    /// L16 (THREATS L2R2-F2): a registry with the pinned code that is bound
    /// to another settlement contract (another deployment's registry, or one
    /// an operator pointed at the wrong ledger) is refused before any level
    /// is read; so is a contract with the pinned code hash that answers
    /// `ledger()` with no address (a swapped contract). Fail closed: the
    /// catalog is left as it was.
    #[tokio::test]
    async fn a_registry_bound_to_another_settlement_is_refused() {
        let code = b"registry runtime".to_vec();
        let mut c = catalog_with(&[1]);
        let other = Registry::new(&code, [0x77; 20]);
        let e = floor_to_registry(&other, [1; 20], keccak256(&code), SETTLEMENT, &mut c)
            .await
            .expect_err("another ledger");
        assert!(e.contains("not bound to the manifest's settlement"), "{e}");
        let (r, sh) = c.shard_keys()[0];
        assert_eq!(
            c.shard(r, &sh).expect("shard").proof_level,
            ProofLevel::Recomputed,
            "nothing floored on a refusal"
        );
        assert!(
            check_registry(&other, [1; 20], keccak256(&code), [0x77; 20])
                .await
                .is_ok()
        );
        // `ledger()` answers that are not one address word are refused.
        for bad in ["0x", "0x01", &format!("0x{}", "ff".repeat(32))] {
            assert!(registry_ledger_of(&json!(bad)).is_err(), "{bad}");
        }
        assert!(registry_ledger_of(&json!(5)).is_err());
    }
    /// L16: a labelled shard from a catalog file (level unknown, 5) keeps the
    /// level the pinned registry attested (4), not the fail-closed 5; an
    /// unattested one stays 5.
    #[tokio::test]
    async fn a_labelled_shard_keeps_its_attested_level() {
        let code = b"registry runtime".to_vec();
        let mut rpc = Registry::new(&code, SETTLEMENT);
        rpc.label_set = keccak256(b"labels");
        let mut c = catalog_with(&[]);
        let base = catalog_with(&[1]);
        let (r, s) = base.shard_keys()[0];
        let mut m = base.shard(r, &s).expect("shard").clone();
        m.binding.label_set_hash = keccak256(b"labels");
        m.proof_level = ProofLevel::TrackRecord;
        let mut other = m.clone();
        other.round = 2;
        other.shard = Hash32([2; 32]);
        c.add_shard(m);
        c.add_shard(other);
        let n = floor_to_registry(&rpc, [1; 20], keccak256(&code), SETTLEMENT, &mut c)
            .await
            .expect("floored");
        assert_eq!(n, 1);
        assert_eq!(
            c.shard(r, &s).expect("shard").proof_level,
            ProofLevel::BondedClaim
        );
        assert_eq!(
            c.shard(2, &Hash32([2; 32])).expect("shard").proof_level,
            ProofLevel::TrackRecord
        );
        assert_eq!(
            c.level_counts().into_iter().collect::<Vec<_>>(),
            vec![(4, 1), (5, 1)]
        );
    }

    /// Review L16-R2: the attested level stands in for a label level only
    /// when the attestation names this shard's own recipe and label set; an
    /// attestation naming another one (the shard id encodings drifted, or a
    /// lying view) counts as none, so the labelled shard stays at the
    /// fail-closed 5 and an unlabelled one is floored at 5.
    #[tokio::test]
    async fn an_attestation_for_another_label_set_or_recipe_is_none() {
        let code = b"registry runtime".to_vec();
        let labelled = |c: &mut Catalog| {
            let base = catalog_with(&[1]);
            let (r, s) = base.shard_keys()[0];
            let mut m = base.shard(r, &s).expect("shard").clone();
            m.binding.label_set_hash = keccak256(b"labels");
            m.proof_level = ProofLevel::TrackRecord;
            c.add_shard(m);
            (r, s)
        };
        for (recipe, label_set) in [
            (Hash32::ZERO, keccak256(b"other labels")),
            (keccak256(b"other recipe"), keccak256(b"labels")),
        ] {
            let mut rpc = Registry::new(&code, SETTLEMENT);
            rpc.recipe = recipe;
            rpc.label_set = label_set;
            let mut c = catalog_with(&[]);
            let (r, s) = labelled(&mut c);
            let n = floor_to_registry(&rpc, [1; 20], keccak256(&code), SETTLEMENT, &mut c)
                .await
                .expect("floored");
            assert_eq!(n, 1, "counted as unattested");
            assert_eq!(
                c.shard(r, &s).expect("shard").proof_level,
                ProofLevel::TrackRecord
            );
            let mut plain = catalog_with(&[1]);
            floor_to_registry(&rpc, [1; 20], keccak256(&code), SETTLEMENT, &mut plain)
                .await
                .expect("floored");
            assert_eq!(
                plain.shard(r, &s).expect("shard").proof_level,
                ProofLevel::TrackRecord
            );
        }
    }
}
