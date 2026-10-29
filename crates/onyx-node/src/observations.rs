//! Level-2 store wiring checks at start (AUD-2-F8): a store may name only the
//! deployment's `OnyxRegistry`, and every pinned template must have a store.
//! The registry's runtime code is checked against `registryCodeHash` at serve
//! time (`onyx_api::observations::verify_registered`), when the node reads the
//! chain.

use crate::manifest::Deployment;
use onyx_api::catalog::Catalog;
use onyx_api::observations::{ObservationFeed, TemplatePins};

/// Refuse a store whose on-chain pointer is not the deployment's registry on
/// the deployment's chain. A store with no pointer is fine only when the
/// manifest pins no registry (AUD-2R-F6): with a pinned registry, every store
/// must name it, so none skips the template and code-hash check.
pub fn check_store_registry(feed: &ObservationFeed, d: &Deployment) -> Result<(), String> {
    match &feed.onchain {
        None if d.registry.is_some() => Err(format!(
            "the deployment manifest pins a registry, but the store for {} names none",
            feed.metric()
        )),
        None => Ok(()),
        Some(p) if p.chain_id == d.chain_id && Some(p.registry) == d.registry => Ok(()),
        Some(_) => Err(format!(
            "the store for {} names a registry that is not the deployment manifest's (registry, chainId)",
            feed.metric()
        )),
    }
}

/// Refuse a pinned template that no loaded store serves: a pin without a
/// store is a configuration mistake, not a silent no-op.
pub fn check_pins_have_stores(pins: &TemplatePins, catalog: &Catalog) -> Result<(), String> {
    for metric in pins.keys() {
        if catalog.observations(metric).is_none() {
            return Err(format!(
                "ONYX_OBSERVATION_TEMPLATES pins {metric} but no ONYX_OBSERVATIONS store serves it"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::BASE_SEPOLIA_CHAIN_ID;
    use onyx_api::observations::OnchainPointer;
    use onyx_core::{
        keccak256, DataRights, FeedTemplate, Hash32, MetricId, ObservationKind, Redistribution,
        SourceId, Subject, SubjectRule,
    };
    use serde_json::json;

    fn deployment(registry: Option<[u8; 20]>) -> Deployment {
        Deployment {
            chain_id: BASE_SEPOLIA_CHAIN_ID,
            settlement: [1; 20],
            settlement_code_hash: Hash32([2; 32]),
            revenue: None,
            revenue_code_hash: None,
            usdc: None,
            registry,
            registry_code_hash: registry.map(|_| Hash32([3; 32])),
            deploy_block: None,
            signed_book: None,
            raw: json!({}),
        }
    }

    fn feed() -> ObservationFeed {
        ObservationFeed::new(FeedTemplate {
            feed: "test.index.v1".into(),
            metric: MetricId("test.index".into()),
            fetcher: keccak256(b"test fetcher"),
            url_template: "https://example.test/index.json".into(),
            selector: "/value".into(),
            kind: ObservationKind::Numeric {
                scale: 2,
                tolerance: 0,
            },
            window_secs: 300,
            min_observers: 3,
            rights: DataRights {
                license: "CC0-1.0".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId("test.page".into()),
                attribution: None,
                terms: String::new(),
            },
            subject: SubjectRule::Fixed(Subject::Aggregate("test.index".into())),
        })
        .expect("feed")
    }

    #[test]
    fn a_store_names_only_the_deployments_registry() {
        let mut f = feed();
        assert!(check_store_registry(&f, &deployment(None)).is_ok());
        // AUD-2R-F6: with a pinned registry, a store that names none is refused.
        assert!(check_store_registry(&f, &deployment(Some([9; 20])))
            .expect_err("no pointer")
            .contains("names none"));
        f.onchain = Some(OnchainPointer {
            chain_id: BASE_SEPOLIA_CHAIN_ID,
            registry: [9; 20],
        });
        assert!(check_store_registry(&f, &deployment(None)).is_err());
        assert!(check_store_registry(&f, &deployment(Some([8; 20]))).is_err());
        assert!(check_store_registry(&f, &deployment(Some([9; 20]))).is_ok());
        f.onchain = Some(OnchainPointer {
            chain_id: 1,
            registry: [9; 20],
        });
        assert!(check_store_registry(&f, &deployment(Some([9; 20]))).is_err());
    }

    #[test]
    fn a_pin_without_a_store_is_refused() {
        let mut pins = TemplatePins::new();
        let f = feed();
        pins.insert("test.index".into(), f.template.id());
        let mut c = Catalog::new();
        assert!(check_pins_have_stores(&pins, &c)
            .expect_err("no store")
            .contains("test.index"));
        c.add_observations(f).expect("adds");
        assert!(check_pins_have_stores(&pins, &c).is_ok());
    }
}
