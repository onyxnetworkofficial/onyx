//! Text parsers on a trust boundary: signed label attestation and
//! revocation statements (strict: what parses re-renders to exactly the
//! input), canonical addresses, decimal and cents parsers of fetched pages,
//! the recorded-fetch manifests, trusted-proxy ranges with
//! `X-Forwarded-For`, and facilitator URLs and networks. The first byte
//! picks the parser.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::{client_ip, Cidr};
use std::net::IpAddr;
use std::str::FromStr;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let Ok(s) = std::str::from_utf8(rest) else {
        return;
    };
    match sel % 8 {
        0 => {
            if let Ok(st) = onyx_labels::attest::Statement::parse(s) {
                assert_eq!(st.render(), s, "attestation has a second spelling");
                let _ = st.check_grammar();
            }
        }
        1 => {
            if let Some(st) = onyx_labels::revoke::RevocationStatement::parse(s) {
                assert_eq!(st.render(), s, "revocation has a second spelling");
            }
        }
        2 => {
            let _ = onyx_core::ids::parse_address(s);
            let _ = onyx_core::is_origin_host(s);
            let _ = onyx_source_fiscaldata::record_date_of(s);
        }
        3 => {
            for scale in [0u8, 2, 8, 18, 38, 39, 255] {
                let _ = onyx_source_page::parse_decimal(s, scale);
            }
            let _ = onyx_source_fiscaldata::parse_cents(s);
        }
        4 => {
            let (manifest, body) = s.split_once('\0').unwrap_or((s, ""));
            let _ = onyx_source_fiscaldata::parse_recording(manifest, body.as_bytes());
            for c in onyx_source_edgar::COMPANIES {
                let _ = onyx_source_edgar::parse_recording(c, manifest, body.as_bytes());
            }
        }
        5 => {
            let mut lines = s.splitn(3, '\n');
            let ranges: Vec<Cidr> = lines
                .next()
                .unwrap_or("")
                .split(',')
                .filter_map(|r| Cidr::from_str(r).ok())
                .collect();
            let peer = lines
                .next()
                .and_then(|p| p.parse::<IpAddr>().ok())
                .unwrap_or(IpAddr::from([10, 0, 0, 1]));
            let hops: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
            let ip = client_ip(peer, &hops, &ranges);
            if !ranges.iter().any(|c| c.contains(peer)) {
                assert_eq!(ip, peer, "an untrusted peer chose its own address");
            }
        }
        6 => {
            let _ = onyx_api::x402::check_facilitator_url(s);
            let _ = onyx_api::x402::check_network(s, false);
            let _ = onyx_api::x402::exact_evm::chain_id(s);
        }
        _ => {
            let _ = onyx_coordinator::cast_chain::parse_addr(s);
            let _ = onyx_api::client_key(IpAddr::from([10, 0, 0, 1]));
            let _ = onyx_api::rate_scopes(s, 60);
        }
    }
});
