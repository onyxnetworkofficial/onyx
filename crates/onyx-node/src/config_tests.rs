//! Configuration tests: defaults, bounds, and the settings that are refused (a public listen
//! address without opt-in, dev facilitators, mainnet; docs/ops/config.md).

use super::*;
use std::collections::BTreeMap;

fn base() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("ONYX_ENV", "testnet"),
        ("ONYX_PUBLIC_URL", "https://api.example.test/"),
        ("ONYX_RPC_URL", "https://sepolia.base.org"),
        ("ONYX_DEPLOYMENT", "/etc/onyx/deployment.json"),
        ("ONYX_CATALOG", "/etc/onyx/catalog.json"),
        ("ONYX_CAS_DIR", "/var/lib/onyx/cas"),
        ("ONYX_STATE_DIR", "/var/lib/onyx/state"),
        ("ONYX_RIGHTS", "cleared"),
    ])
}

fn load(m: &BTreeMap<&str, &str>) -> Result<NodeConfig, String> {
    NodeConfig::from_env(|k| m.get(k).map(|v| v.to_string()), false)
}

#[test]
fn a_minimal_testnet_config_is_free_and_strict() {
    let c = load(&base()).expect("config");
    assert_eq!(c.env, Env::Testnet);
    assert_eq!(c.payments, PaymentMode::Off);
    assert_eq!(c.public_url, "https://api.example.test");
    assert!(!c.allow_test_labels && !c.allow_mainnet);
    assert_eq!(c.listen, "127.0.0.1:8080".parse().expect("addr"));
    assert!(c.facilitator_url.is_none());
    assert!(c.storage.is_none());
}

/// ADR-0060: the storage-node mode is off unless `ONYX_STORAGE_CONTRACT`
/// is set, needs the bulk store, a node id and a key file, and its
/// settings are bounded and refused when it is off.
#[test]
fn storage_mode_settings() {
    let mut m = base();
    m.insert("ONYX_STORAGE_KEEPER", "on");
    assert!(load(&m)
        .expect_err("off")
        .contains("ONYX_STORAGE_KEEPER is set but ONYX_STORAGE_CONTRACT is not"));
    m.remove("ONYX_STORAGE_KEEPER");
    m.insert(
        "ONYX_STORAGE_CONTRACT",
        "0x5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e",
    );
    assert!(load(&m)
        .expect_err("no bulk")
        .contains("needs ONYX_BULK_DIR"));
    m.insert("ONYX_BULK_DIR", "/var/lib/onyx/bulk");
    assert!(load(&m)
        .expect_err("no id")
        .contains("ONYX_STORAGE_NODE_ID"));
    m.insert(
        "ONYX_STORAGE_NODE_ID",
        "0x0101010101010101010101010101010101010101010101010101010101010101",
    );
    assert!(load(&m)
        .expect_err("no key")
        .contains("ONYX_STORAGE_KEY_FILE"));
    m.insert("ONYX_STORAGE_KEY_FILE", "/run/credentials/onyx/storage-key");
    let s = load(&m).expect("on").storage.expect("storage");
    assert_eq!(s.contract, [0x5e; 20]);
    assert_eq!(s.node, [1; 32]);
    assert!(s.fetch && !s.keeper);
    assert_eq!((s.every_secs, s.fetches_per_tick, s.from_block), (15, 8, 0));
    m.insert("ONYX_STORAGE_FETCH", "off");
    m.insert("ONYX_STORAGE_KEEPER", "on");
    m.insert("ONYX_STORAGE_EVERY_SECS", "2");
    let s = load(&m).expect("on").storage.expect("storage");
    assert!(!s.fetch && s.keeper && s.every_secs == 2);
    m.insert("ONYX_STORAGE_EVERY_SECS", "0");
    assert!(load(&m).is_err());
    m.insert("ONYX_STORAGE_EVERY_SECS", "2");
    m.insert("ONYX_STORAGE_FETCH", "yes");
    assert!(load(&m).is_err());
    m.insert("ONYX_STORAGE_FETCH", "on");
    m.insert("ONYX_STORAGE_NODE_ID", "0x01");
    assert!(load(&m).is_err());
}

/// IDX-R6-3: `ONYX_INDEX_MAX_JUMP` past `ONYX_INDEX_OVERLAP` is refused,
/// so the overlap always reads again what one lying head let the scan
/// pass (IDX-R5-5); equal is fine.
#[test]
fn idx_r6_3_the_jump_is_at_most_the_overlap() {
    let mut m = base();
    m.insert("ONYX_INDEXER", "on");
    m.insert("ONYX_INDEX_OVERLAP", "50");
    let e = load(&m).expect_err("default jump 100 > overlap 50");
    assert!(
        e.contains("ONYX_INDEX_MAX_JUMP (100) must be at most ONYX_INDEX_OVERLAP (50)"),
        "{e}"
    );
    m.insert("ONYX_INDEX_MAX_JUMP", "50");
    let c = load(&m).expect("equal").indexer.expect("on");
    assert_eq!((c.max_jump, c.overlap), (50, 50));
    m.insert("ONYX_INDEX_MAX_JUMP", "51");
    assert!(load(&m).is_err());
}

/// ADR-0033: the indexer is off unless asked for; its settings are
/// bounded and refused when it is off.
#[test]
fn the_indexer_is_opt_in_and_bounded() {
    assert!(load(&base()).expect("config").indexer.is_none());
    let mut m = base();
    m.insert("ONYX_INDEX_SPAN", "100");
    assert!(load(&m).expect_err("off").contains("ONYX_INDEXER"));
    m.insert("ONYX_INDEXER", "on");
    let c = load(&m).expect("config").indexer.expect("on");
    assert_eq!(c.span, 100);
    assert_eq!(c.overlap, onyx_api::indexer::DEFAULT_OVERLAP);
    assert_eq!(c.every, Duration::from_secs(15));
    assert!(c.health_detail, "full health detail by default");
    assert_eq!(c.rescan_every, onyx_api::indexer::DEFAULT_RESCAN_EVERY);
    assert_eq!(c.rescan_budget, onyx_api::indexer::DEFAULT_RESCAN_BUDGET);
    assert_eq!(c.max_jump, onyx_api::indexer::DEFAULT_MAX_JUMP);
    // IDX-R5-4: the interval as a time, in whole polls.
    let mut daily = m.clone();
    daily.insert("ONYX_INDEX_RESCAN_SECS", "86400");
    daily.insert("ONYX_INDEX_EVERY_SECS", "7");
    assert_eq!(
        load(&daily)
            .expect("secs")
            .indexer
            .expect("on")
            .rescan_every,
        12_343
    );
    daily.insert("ONYX_INDEX_RESCAN_POLLS", "10");
    assert!(load(&daily).expect_err("both").contains("not both"));
    let mut tuned = m.clone();
    tuned.insert("ONYX_INDEX_RESCAN_CALLS", "20");
    tuned.insert("ONYX_INDEX_MAX_JUMP", "50");
    let t = load(&tuned).expect("tuned").indexer.expect("on");
    assert_eq!((t.rescan_budget, t.max_jump), (20, 50));
    let mut rescan = m.clone();
    rescan.insert("ONYX_INDEX_RESCAN_POLLS", "0");
    assert_eq!(
        load(&rescan)
            .expect("rescan")
            .indexer
            .expect("on")
            .rescan_every,
        0
    );
    let mut summary = m.clone();
    summary.insert("ONYX_INDEX_HEALTH", "summary");
    assert!(
        !load(&summary)
            .expect("summary")
            .indexer
            .expect("on")
            .health_detail
    );
    assert_eq!(
        c.state_file,
        PathBuf::from("/var/lib/onyx/state/indexer.json")
    );
    for (k, v) in [
        ("ONYX_INDEX_SPAN", "0"),
        ("ONYX_INDEX_SPAN", "10001"),
        ("ONYX_INDEX_OVERLAP", "-1"),
        ("ONYX_INDEX_EVERY_SECS", "0"),
        ("ONYX_INDEX_RESCAN_POLLS", "1000001"),
        ("ONYX_INDEX_RESCAN_SECS", "31536001"),
        ("ONYX_INDEX_RESCAN_CALLS", "0"),
        ("ONYX_INDEX_RESCAN_CALLS", "2001"),
        ("ONYX_INDEX_MAX_JUMP", "0"),
        // IDX-R6-3: a jump past the overlap (the default jump is 100).
        ("ONYX_INDEX_MAX_JUMP", "101"),
        ("ONYX_INDEX_OVERLAP", "99"),
        ("ONYX_INDEX_OVERLAP", "0"),
        ("ONYX_INDEXER", "yes"),
        ("ONYX_INDEX_HEALTH", "none"),
    ] {
        let mut bad = m.clone();
        bad.insert(k, v);
        assert!(load(&bad).is_err(), "{k}={v}");
    }
    m.insert("ONYX_INDEX_STATE", "/srv/idx.json");
    assert_eq!(
        load(&m).expect("config").indexer.expect("on").state_file,
        PathBuf::from("/srv/idx.json")
    );
}

#[test]
fn level_two_stores_are_optional_and_comma_separated() {
    assert!(load(&base()).expect("config").observations.is_empty());
    let mut m = base();
    m.insert(
        "ONYX_OBSERVATIONS",
        " /etc/onyx/debt.json, ,/etc/onyx/other.json ",
    );
    // L2API-R4-F1: stores need pinned templates.
    assert!(load(&m)
        .expect_err("no pins")
        .contains("ONYX_OBSERVATION_TEMPLATES"));
    let id = format!("0x{}", "ab".repeat(32));
    m.insert(
        "ONYX_OBSERVATION_TEMPLATES",
        Box::leak(format!("a.b={id},a.b={id}").into_boxed_str()),
    );
    assert!(load(&m).expect_err("twice").contains("twice"));
    m.insert("ONYX_OBSERVATION_TEMPLATES", "a.b=0x12");
    assert!(load(&m).is_err(), "short id");
    m.insert(
        "ONYX_OBSERVATION_TEMPLATES",
        Box::leak(format!(" us.debt = {id} ").into_boxed_str()),
    );
    let c = load(&m).expect("config");
    assert_eq!(
        c.observation_templates.get("us.debt").map(|h| h.0),
        Some([0xab; 32])
    );
    assert_eq!(
        c.observations,
        vec![
            PathBuf::from("/etc/onyx/debt.json"),
            PathBuf::from("/etc/onyx/other.json")
        ]
    );
}

#[test]
fn fee_gas_and_per_client_limits_are_settable_and_bounded() {
    // AUD-1R-F1/F3: defaults, overrides, and out-of-range values refused.
    let c = load(&base()).expect("config");
    assert_eq!(c.max_per_client, crate::serve::DEFAULT_MAX_PER_CLIENT);
    assert_eq!(c.reads_per_minute, onyx_api::DEFAULT_READS_PER_MINUTE);
    assert_eq!(c.max_fee_per_gas, onyx_api::evm_tx::DEFAULT_MAX_FEE_PER_GAS);
    assert_eq!(c.max_gas, onyx_api::evm_tx::DEFAULT_MAX_GAS);
    let mut m = base();
    m.insert("ONYX_MAX_CONNS_PER_IP", "8");
    m.insert("ONYX_READS_PER_MINUTE", "0");
    m.insert("ONYX_MAX_FEE_GWEI", "50");
    m.insert("ONYX_MAX_GAS", "2000000");
    let c = load(&m).expect("config");
    assert_eq!(c.max_per_client, 8);
    assert_eq!(c.reads_per_minute, 0);
    assert_eq!(c.max_fee_per_gas, 50_000_000_000);
    assert_eq!(c.max_gas, 2_000_000);
    for (k, v) in [
        ("ONYX_MAX_CONNS_PER_IP", "0"),
        ("ONYX_READS_PER_MINUTE", "1000001"),
        ("ONYX_READS_PER_MINUTE", "-1"),
        ("ONYX_MAX_FEE_GWEI", "0"),
        ("ONYX_MAX_FEE_GWEI", "10001"),
        ("ONYX_MAX_FEE_GWEI", "1.5"),
        ("ONYX_MAX_GAS", "20999"),
        ("ONYX_MAX_GAS", "30000001"),
    ] {
        let mut m = base();
        m.insert(k, v);
        assert!(load(&m).expect_err(v).contains(k), "{k}={v}");
    }
}

/// THREATS SERVE-WRITE-1: the write stall timeout defaults to 30 s, is
/// settable, and out-of-range values are refused.
#[test]
fn serve_write_1_the_write_stall_timeout_is_settable_and_bounded() {
    let c = load(&base()).expect("config");
    assert_eq!(c.write_stall_timeout, std::time::Duration::from_secs(30));
    let mut m = base();
    m.insert("ONYX_WRITE_STALL_SECS", "120");
    let c = load(&m).expect("config");
    assert_eq!(c.write_stall_timeout, std::time::Duration::from_secs(120));
    for v in ["0", "601", "-1", "1.5", ""] {
        let mut m = base();
        m.insert("ONYX_WRITE_STALL_SECS", v);
        assert!(
            load(&m).expect_err(v).contains("ONYX_WRITE_STALL_SECS"),
            "ONYX_WRITE_STALL_SECS={v}"
        );
    }
}

/// M7-7 (ROADMAP L11): the server speaks plain HTTP, so a listen address
/// other machines can reach is refused unless the operator says, with
/// `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1`, that something else keeps it off
/// the open network (a container published on 127.0.0.1, a testnet lab).
/// Loopback (127.0.0.0/8, ::1, and ::ffff:127.x) needs nothing.
#[test]
fn m7_7_a_public_listen_address_needs_an_explicit_opt_in() {
    for addr in [
        "0.0.0.0:8080",
        "[::]:8080",
        "192.0.2.10:8080",
        "[2001:db8::1]:8080",
    ] {
        let mut m = base();
        m.insert("ONYX_LISTEN", addr);
        let e = load(&m).expect_err(addr);
        assert!(
            e.contains("ONYX_LISTEN")
                && e.contains("plain HTTP")
                && e.contains("TLS")
                && e.contains("ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1"),
            "{addr}: {e}"
        );
        for off in ["0", ""] {
            m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", off);
            assert!(load(&m).is_err(), "{addr} with {off:?}");
        }
        m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", "1");
        let c = load(&m).expect(addr);
        let w = c.plain_http_public_warning().expect("a warning at start");
        assert!(w.contains(addr) && w.contains("TLS"), "{w}");
    }
    for addr in [
        "127.0.0.1:8080",
        "127.0.0.2:9000",
        "[::1]:8080",
        "[::ffff:127.0.0.1]:8080",
    ] {
        let mut m = base();
        m.insert("ONYX_LISTEN", addr);
        let c = load(&m).expect(addr);
        assert!(c.plain_http_public_warning().is_none(), "{addr}");
        // The opt-in changes nothing on loopback.
        m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", "1");
        assert!(load(&m).expect(addr).plain_http_public_warning().is_none());
    }
    for bad in ["yes", "true", "2", " 1"] {
        let mut m = base();
        m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", bad);
        assert!(
            load(&m)
                .expect_err(bad)
                .contains("ONYX_ALLOW_PLAIN_HTTP_PUBLIC must be 0 or 1"),
            "{bad:?}"
        );
    }
}

/// AUD-2-F8: a per-client cap above the server's cap, and a /0 trusted
/// proxy range (which would trust every peer), are refused.
#[test]
fn per_client_cap_and_trusted_proxy_ranges_are_checked() {
    let mut m = base();
    m.insert("ONYX_MAX_CONNECTIONS", "64");
    m.insert("ONYX_MAX_CONNS_PER_IP", "65");
    assert!(load(&m)
        .expect_err("per ip > total")
        .contains("ONYX_MAX_CONNS_PER_IP"));
    m.insert("ONYX_MAX_CONNS_PER_IP", "64");
    assert!(load(&m).is_ok());
    for r in ["0.0.0.0/0", "::/0", "10.0.0.0/8,::/0"] {
        let mut m = base();
        m.insert("ONYX_TRUSTED_PROXIES", r);
        assert!(load(&m).expect_err(r).contains("/0"), "{r}");
    }
    let mut m = base();
    m.insert("ONYX_TRUSTED_PROXIES", "10.0.0.0/8, 2001:db8::/32");
    assert_eq!(load(&m).expect("ranges").trusted_proxies.len(), 2);
}

#[test]
fn no_mock_or_dev_facilitator_can_be_selected() {
    for mode in ["mock", "dev", "local", "Mock", ""] {
        let mut m = base();
        m.insert("ONYX_PAYMENTS", mode);
        let e = load(&m).expect_err(mode);
        assert!(e.contains("ONYX_PAYMENTS"), "{mode}: {e}");
    }
    // Facilitator mode: an https facilitator only (or loopback http).
    let mut m = base();
    m.insert("ONYX_PAYMENTS", "facilitator");
    m.insert("ONYX_REPORTER", "off");
    let c = load(&m).expect("default facilitator");
    assert!(c
        .facilitator_url
        .as_deref()
        .is_some_and(|u| u.starts_with("https://")));
    m.insert("ONYX_X402_FACILITATOR", "http://f.example");
    assert!(load(&m).is_err(), "plain http to a remote facilitator");
}

/// OPS-L1-F3: a loopback http facilitator decides what is paid; it is a
/// dev-box tool, refused on testnet.
#[test]
fn a_loopback_http_facilitator_is_dev_only() {
    let mut m = base();
    m.insert("ONYX_PAYMENTS", "facilitator");
    m.insert("ONYX_REPORTER", "off");
    m.insert("ONYX_X402_FACILITATOR", "http://127.0.0.1:9");
    let e = load(&m).expect_err("testnet");
    assert!(e.contains("dev only"), "{e}");
    m.insert("ONYX_ENV", "dev");
    assert!(load(&m).is_ok(), "dev may use a local facilitator");
}

/// OPS-L1-F5: facilitator mode without a key would record receipts that
/// ROADMAP S7: ONYX_PREPAID is one address, and only where the reporter sends.
#[test]
fn prepaid_needs_an_address_and_the_facilitator_reporter() {
    let mut m = base();
    m.insert("ONYX_PREPAID", "0x1111111111111111111111111111111111111111");
    assert!(load(&m)
        .expect_err("payments off")
        .contains("ONYX_PREPAID needs"));
    m.insert("ONYX_PAYMENTS", "facilitator");
    m.insert("ONYX_REPORTER", "off");
    assert!(load(&m)
        .expect_err("reporter off")
        .contains("ONYX_PREPAID needs"));
    m.insert("ONYX_REPORTER", "on");
    m.insert(
        "ONYX_OPERATOR_KEY_FILE",
        "/run/credentials/onyx/operator-key",
    );
    assert_eq!(load(&m).expect("ok").prepaid, Some([0x11; 20]));
    for bad in [
        "1111111111111111111111111111111111111111",
        "0x11",
        "0xzz11111111111111111111111111111111111111",
    ] {
        m.insert("ONYX_PREPAID", bad);
        assert!(load(&m).expect_err(bad).contains("0x address"), "{bad}");
    }
    m.insert("ONYX_PREPAID", "");
    assert_eq!(load(&m).expect("unset").prepaid, None);
}

/// CL6: a relay key needs prepaid credits, is never the operator key, and its fees parse.
#[test]
fn relay_needs_prepaid_its_own_key_and_valid_fees() {
    let mut m = base();
    m.insert("ONYX_RELAY_KEY_FILE", "/run/credentials/onyx/relay-key");
    assert!(load(&m)
        .expect_err("no prepaid")
        .contains("ONYX_RELAY_KEY_FILE needs ONYX_PREPAID"));
    m.insert("ONYX_PAYMENTS", "facilitator");
    m.insert(
        "ONYX_OPERATOR_KEY_FILE",
        "/run/credentials/onyx/operator-key",
    );
    m.insert("ONYX_PREPAID", "0x1111111111111111111111111111111111111111");
    let c = load(&m).expect("ok");
    assert_eq!(
        c.relay_key_file,
        Some(PathBuf::from("/run/credentials/onyx/relay-key"))
    );
    assert_eq!(c.relay_fees, DEFAULT_RELAY_FEES);
    m.insert("ONYX_RELAY_KEY_FILE", "/run/credentials/onyx/operator-key");
    assert!(load(&m).expect_err("same key").contains("own key"));
    m.insert("ONYX_RELAY_KEY_FILE", "/run/credentials/onyx/relay-key");
    for bad in [
        "post_commit=0",
        "post_commit=1000001",
        "transfer=5",
        "post_commit",
    ] {
        m.insert("ONYX_RELAY_FEES", bad);
        assert!(
            load(&m).expect_err(bad).contains("ONYX_RELAY_FEES"),
            "{bad}"
        );
    }
    m.insert("ONYX_RELAY_FEES", "post_commit=1500");
    assert_eq!(load(&m).expect("ok").relay_fees, "post_commit=1500");
    m.insert("ONYX_CREDITS_FROM_BLOCK", "123");
    assert_eq!(load(&m).expect("ok").credits_from_block, Some(123));
    m.insert("ONYX_CREDITS_FROM_BLOCK", "-1");
    assert!(load(&m).is_err());
}

/// ADR-0049: ONYX_OBSERVE only with prepaid holds.
#[test]
fn observe_needs_prepaid() {
    let mut m = base();
    m.insert("ONYX_OBSERVE", "/etc/onyx/observe.json");
    assert!(load(&m)
        .expect_err("no prepaid")
        .contains("ONYX_OBSERVE needs ONYX_PREPAID"));
    m.insert("ONYX_PAYMENTS", "facilitator");
    m.insert(
        "ONYX_OPERATOR_KEY_FILE",
        "/run/credentials/onyx/operator-key",
    );
    m.insert("ONYX_PREPAID", "0x1111111111111111111111111111111111111111");
    assert_eq!(
        load(&m).expect("ok").observe,
        Some(PathBuf::from("/etc/onyx/observe.json"))
    );
}

/// nobody posts; that needs an explicit ONYX_REPORTER=off.
#[test]
fn facilitator_mode_needs_a_key_or_an_explicit_reporter_off() {
    let mut m = base();
    m.insert("ONYX_PAYMENTS", "facilitator");
    let e = load(&m).expect_err("no key, reporter on");
    assert!(e.contains("ONYX_REPORTER=off"), "{e}");
    m.insert("ONYX_REPORTER", "off");
    assert!(!load(&m).expect("explicit off").reporter);
    m.insert("ONYX_REPORTER", "maybe");
    assert!(load(&m).is_err());
    m.insert("ONYX_REPORTER", "on");
    m.insert(
        "ONYX_OPERATOR_KEY_FILE",
        "/run/credentials/onyx/operator-key",
    );
    assert!(load(&m).expect("key").reporter);
}

#[test]
fn mainnet_and_unknown_environments_are_refused() {
    for env in ["mainnet", "prod", "production", ""] {
        let mut m = base();
        m.insert("ONYX_ENV", env);
        assert!(load(&m).is_err(), "{env}");
    }
}

#[test]
fn urls_are_https_or_loopback() {
    let mut m = base();
    m.insert("ONYX_RPC_URL", "http://node.example:8545");
    assert!(load(&m).is_err());
    m.insert("ONYX_RPC_URL", "http://127.0.0.1:8545");
    assert!(load(&m).is_ok());
    m.insert("ONYX_PUBLIC_URL", "http://api.example.test");
    assert!(load(&m).is_err());
}

#[test]
fn self_settle_needs_a_key_file_and_test_labels_need_an_explicit_switch() {
    let mut m = base();
    m.insert("ONYX_PAYMENTS", "self-settle");
    assert!(load(&m).is_err());
    m.insert(
        "ONYX_OPERATOR_KEY_FILE",
        "/run/credentials/onyx/operator-key",
    );
    assert!(load(&m).is_ok());
    m.insert("ONYX_ALLOW_TEST_LABELS", "yes");
    assert!(load(&m).is_err());
    m.insert("ONYX_ALLOW_TEST_LABELS", "1");
    assert!(load(&m).expect("config").allow_test_labels);
}

#[test]
fn rights_take_only_cleared_or_fallback_and_a_testnet_node_must_name_one() {
    use onyx_api::catalog::LabelRights;
    let mut m = base();
    // Review L14-R1: a testnet node (the one that can be public) with the
    // switch unset or empty fails closed; it never defaults to cleared.
    for unset in [None, Some("")] {
        match unset {
            None => m.remove("ONYX_RIGHTS"),
            Some(v) => m.insert("ONYX_RIGHTS", v),
        };
        let e = load(&m).expect_err("testnet without ONYX_RIGHTS");
        assert!(
            e.contains("ONYX_RIGHTS is required with ONYX_ENV=testnet"),
            "{e}"
        );
    }
    // A dev node (local chain) keeps today's default.
    m.insert("ONYX_ENV", "dev");
    m.insert("ONYX_RPC_URL", "http://127.0.0.1:8545");
    m.remove("ONYX_RIGHTS");
    assert_eq!(load(&m).expect("dev config").rights, LabelRights::Cleared);
    m.insert("ONYX_RIGHTS", "");
    assert_eq!(load(&m).expect("dev config").rights, LabelRights::Cleared);
    let mut m = base();
    m.insert("ONYX_RIGHTS", "cleared");
    assert_eq!(load(&m).expect("config").rights, LabelRights::Cleared);
    m.insert("ONYX_RIGHTS", "fallback");
    assert_eq!(load(&m).expect("config").rights, LabelRights::Fallback);
    for bad in ["Fallback", "FALLBACK", "true", "1", "off", "fallback "] {
        m.insert("ONYX_RIGHTS", bad);
        let e = load(&m).expect_err(bad);
        assert!(e.contains("cleared or fallback"), "{bad:?}: {e}");
    }
}

#[test]
fn url_host_drops_paths_queries_and_credentials() {
    assert_eq!(
        url_host("https://base-sepolia.g.alchemy.com/v2/SECRETKEY"),
        "base-sepolia.g.alchemy.com"
    );
    assert_eq!(
        url_host("https://user:pw@node.example:8545/?key=x"),
        "node.example:8545"
    );
    // OPS-L2-F4: not a URL at all (a bare key): never echoed.
    assert!(!url_host("SECRETAPIKEY123").contains("SECRET"));
    assert!(!url_host("n.example/v2/SECRETPATH").contains("SECRET"));
}

/// OPS-L1-F4: the node URL can come from a private file (a credential),
/// since a provider URL often carries an API key.
#[cfg(unix)]
#[test]
fn the_rpc_url_can_come_from_a_private_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("onyx-node-rpc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let p = dir.join("rpc");
    std::fs::write(&p, "https://node.example/v2/SECRETKEY\n").expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let path = p.display().to_string();
    let mut m: BTreeMap<&str, String> = base()
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect();
    m.remove("ONYX_RPC_URL");
    m.insert("ONYX_RPC_URL_FILE", path.clone());
    let get = |m: &BTreeMap<&str, String>| {
        let m = m.clone();
        NodeConfig::from_env(move |k| m.get(k).cloned(), false)
    };
    let e = get(&m).expect_err("world readable");
    assert!(e.contains("chmod 600") && !e.contains("SECRETKEY"), "{e}");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(
        get(&m).expect("config").rpc_url,
        "https://node.example/v2/SECRETKEY"
    );
    m.insert("ONYX_RPC_URL", "https://sepolia.base.org".into());
    assert!(get(&m).expect_err("both").contains("not both"));
    m.remove("ONYX_RPC_URL");
    std::fs::write(&p, "http://node.example/v2/SECRETKEY").expect("write");
    let e = get(&m).expect_err("plain http remote");
    assert!(!e.contains("SECRETKEY"), "errors never echo the URL: {e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn key_files_must_be_private_and_well_formed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("onyx-node-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let p = dir.join("k");
    std::fs::write(&p, format!("0x{}\n", "11".repeat(32))).expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(read_key_file(&p)
        .expect_err("world readable")
        .contains("chmod 600"));
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(read_key_file(&p).expect("key"), [0x11; 32]);
    std::fs::write(&p, "not hex").expect("write");
    let e = read_key_file(&p).expect_err("bad key");
    assert!(!e.contains("not hex"), "errors never echo the file: {e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// L7-SD-1: the pure rule accepts exactly what systemd writes for
/// `LoadCredential=` (file root:root 0440 or tighter, directory root:root
/// closed to others) and refuses every looser owner, group or mode.
#[test]
fn credential_rule_accepts_only_root_owned_files_in_a_root_owned_closed_directory() {
    let p = |mode, uid, gid| Perms { mode, uid, gid };
    let systemd_dir = p(0o550, 0, 0);
    assert!(credential_allowed(p(0o440, 0, 0), systemd_dir));
    assert!(credential_allowed(p(0o400, 0, 0), p(0o500, 0, 0)));
    assert!(credential_allowed(p(0o600, 0, 0), p(0o700, 0, 0)));
    // The review's hole: /etc/onyx root:onyx 0750 with a root:onyx 0440 key.
    assert!(!credential_allowed(p(0o440, 0, 998), p(0o750, 0, 998)));
    // A 0750 directory with a non-root group, even with a root:root file.
    assert!(!credential_allowed(p(0o440, 0, 0), p(0o750, 0, 998)));
    // A file with a non-root owner, or a non-root group.
    assert!(!credential_allowed(p(0o440, 1000, 0), systemd_dir));
    assert!(!credential_allowed(p(0o440, 0, 1000), systemd_dir));
    // A directory not owned by root.
    assert!(!credential_allowed(p(0o440, 0, 0), p(0o550, 1000, 0)));
    // Others may never read, group may never write, the directory never open.
    assert!(!credential_allowed(p(0o444, 0, 0), systemd_dir));
    assert!(!credential_allowed(p(0o460, 0, 0), systemd_dir));
    assert!(!credential_allowed(p(0o440, 0, 0), p(0o551, 0, 0)));
    assert!(!credential_allowed(p(0o440, 0, 0), p(0o1777, 0, 0)));
}

/// L7-SD-1 on the file system (tests do not run as root, so here every
/// group-readable file is refused; the root case is the pure rule above):
/// only a regular file directly in the directory is a candidate, never a
/// symlink in it that points elsewhere, and 0600 passes anywhere.
#[cfg(unix)]
#[test]
fn credential_candidates_are_regular_files_directly_in_the_directory() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &std::path::Path, m: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).expect("chmod")
    };
    let root = std::env::temp_dir().join(format!("onyx-node-cred-{}", std::process::id()));
    let creds = root.join("credentials");
    let other = root.join("other");
    std::fs::create_dir_all(&creds).expect("dir");
    std::fs::create_dir_all(&other).expect("dir");
    let key = creds.join("operator-key");
    std::fs::write(&key, "11".repeat(32)).expect("write");
    mode(&key, 0o440);
    let elsewhere = other.join("operator-key");
    std::fs::write(&elsewhere, "11".repeat(32)).expect("write");
    mode(&elsewhere, 0o440);
    let link = creds.join("linked-key");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("symlink");
    mode(&creds, 0o550);
    // A regular file in the directory is a candidate (its real owner is
    // read, not assumed); a symlink in it pointing outside is not, nor is
    // a file elsewhere.
    let (f, d) = credential_perms(&key, &creds).expect("candidate");
    assert_eq!(f.mode & 0o777, 0o440);
    assert_eq!(d.mode & 0o777, 0o550);
    assert!(credential_perms(&link, &creds).is_none());
    assert!(credential_perms(&elsewhere, &creds).is_none());
    // Not root-owned here, so the 0440 file, the symlink and the outside
    // file are all refused, with or without the directory.
    let c = Some(creds.as_path());
    if running_as_root() {
        eprintln!("running as root: the not-root refusals below are skipped");
    } else {
        assert!(check_private(&key, "k", c).is_err());
        assert!(check_private(&link, "k", c).is_err());
    }
    assert!(check_private(&key, "k", None).is_err());
    assert!(check_private(&elsewhere, "k", c).is_err());
    // 0600 passes anywhere, as before.
    mode(&elsewhere, 0o600);
    check_private(&elsewhere, "k", None).expect("0600");
    mode(&creds, 0o700);
    let _ = std::fs::remove_dir_all(&root);
}

/// Whether the test runs as root (then files it writes are root-owned).
#[cfg(unix)]
fn running_as_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
}

/// Review SERVE-WRITE-R2 finding 1: a streamed shard holds a socket and a
/// file, so the node warns when the open-file limit is below two a
/// connection plus a reserve, and the shipped units set a limit far above it.
#[test]
fn serve_write_1_a_low_open_file_limit_is_warned_and_the_units_raise_it() {
    use crate::config::{fd_limit_warning, FD_RESERVE};
    assert!(fd_limit_warning(None, 1_024).is_none(), "unknown: no guess");
    assert!(fd_limit_warning(Some(65_536), 1_024).is_none());
    assert!(fd_limit_warning(Some(2 * 1_024 + FD_RESERVE), 1_024).is_none());
    let w = fd_limit_warning(Some(1_024), 1_024).expect("warned");
    assert!(
        w.contains("LimitNOFILE") && w.contains("ONYX_MAX_CONNECTIONS"),
        "{w}"
    );
    for unit in [
        include_str!("../../../deploy/systemd/onyx-node.service"),
        include_str!("../../../deploy/systemd/onyx-storage@.service"),
    ] {
        let n: u64 = unit
            .lines()
            .find_map(|l| l.strip_prefix("LimitNOFILE="))
            .expect("LimitNOFILE set")
            .trim()
            .parse()
            .expect("a number");
        // The default cap, and sixteen times it.
        assert!(fd_limit_warning(Some(n), 16 * 1_024).is_none(), "{n}");
    }
}
