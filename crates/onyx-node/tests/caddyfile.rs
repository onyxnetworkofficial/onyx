//! The shipped TLS proxy config, `deploy/caddy/Caddyfile` (ROADMAP L11,
//! THREATS.md M7-7), checked against the server it fronts, so the two cannot
//! drift apart: every onyx-api route is proxied and nothing else, the header
//! cap fits a full payment header, the write timeout covers the server's own
//! deadlines, and the client's `X-Forwarded-For` is never believed. The live
//! run behind Caddy is docs/audit/2026-10-15-tls-front.md.

use onyx_api::ApiConfig;

const CADDYFILE: &str = include_str!("../../../deploy/caddy/Caddyfile");
const API_LIB: &str = include_str!("../../onyx-api/src/lib.rs");
/// The level-5 track routes (ROADMAP C6), merged into the router by the node.
const API_SIGNED: &str = include_str!("../../onyx-api/src/signed_serve.rs");
const API_SUBMIT: &str = include_str!("../../onyx-api/src/submit.rs");
const API_TRACK: &str = include_str!("../../onyx-api/src/track_serve.rs");
const API_LABELS: &str = include_str!("../../onyx-api/src/labels_serve.rs");
/// The bounty board (north-star loop NL9), merged into the router by the node.
const API_WANTS: &str = include_str!("../../onyx-api/src/wants.rs");

/// The config without comments, one trimmed line each.
fn lines() -> Vec<&'static str> {
    CADDYFILE
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The arguments of the first line starting with `key `.
fn args(key: &str) -> Vec<&'static str> {
    let l = lines()
        .into_iter()
        .find(|l| l.split_whitespace().next() == Some(key))
        .unwrap_or_else(|| panic!("no {key} line in the Caddyfile"));
    l.split_whitespace().skip(1).collect()
}

/// How many lines start with `key `.
fn count(key: &str) -> usize {
    lines()
        .into_iter()
        .filter(|l| l.split_whitespace().next() == Some(key))
        .count()
}

fn secs(v: &str) -> u64 {
    v.strip_suffix('s')
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{v:?} is not whole seconds"))
}

fn kib(v: &str) -> usize {
    v.strip_suffix("KiB")
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("{v:?} is not KiB"))
        * 1024
}

/// The `/v1/...` paths of one function's body in `src`.
fn routes_of(src: &'static str, sig: &str) -> Vec<&'static str> {
    let body = src
        .split(sig)
        .nth(1)
        .and_then(|r| r.split("\n}\n").next())
        .unwrap_or_else(|| panic!("no {sig} in the onyx-api source"));
    body.split('"').filter(|s| s.starts_with("/v1/")).collect()
}

/// The `/v1/...` paths a node serves, read from the source: `onyx_api::router_with` and the
/// track routes the node merges into it (ROADMAP C6; C20 adds the log and history pages; C23
/// the registry).
fn api_routes() -> Vec<&'static str> {
    let mut routes = routes_of(API_LIB, "pub fn router_with(");
    assert!(routes.len() >= 7, "found only {routes:?}");
    let track = routes_of(API_TRACK, "pub fn routes(");
    assert_eq!(track.len(), 5, "track routes {track:?}");
    routes.extend(track);
    routes.extend(routes_of(API_SIGNED, "pub fn routes("));
    // ROADMAP D30 (PP3): salted label rows.
    routes.extend(routes_of(API_LABELS, "pub fn routes("));
    // North-star loop NL9: the bounty board (GET, and POST in the @submit group).
    routes.extend(routes_of(API_WANTS, "pub fn routes("));
    routes.push("/v1/submit/{id}");
    routes
}

#[test]
fn every_api_route_is_proxied_and_nothing_else() {
    // Both bounded method groups are checked below; no hidden proxy blocks.
    for key in ["path", "method", "reverse_proxy"] {
        assert_eq!(count(key), 2, "{key} lines in the Caddyfile");
    }
    assert_eq!(count("@api"), 1);
    assert_eq!(count("@submit"), 1);
    assert_eq!(count("handle"), 3);
    let patterns = args("path");
    assert!(!patterns.is_empty());
    let covers = |p: &str, route: &str| match p.strip_suffix('*') {
        Some(prefix) => route.starts_with(prefix) && route.len() > prefix.len(),
        None => p == route,
    };
    for route in api_routes() {
        assert!(
            patterns.iter().any(|p| covers(p, route)),
            "onyx-api route {route} is not proxied by deploy/caddy/Caddyfile"
        );
    }
    for p in &patterns {
        assert!(p.starts_with("/v1/"), "{p} proxies more than the API");
        assert!(
            api_routes().iter().any(|r| covers(p, r)),
            "{p} matches no onyx-api route"
        );
    }
    // Read routes remain GET/HEAD only; the separate group admits exactly the two POSTs.
    assert_eq!(args("method"), ["GET", "HEAD"]);
    assert!(lines().contains(&"respond \"not found\" 404"));
    // Port 80 only redirects (an explicit block, so the server options
    // apply to it too).
    assert!(lines().contains(&"redir https://{host}{uri} 308"));
    assert!(lines().contains(&"http://{$ONYX_CADDY_DOMAIN} {"));
    assert!(lines().contains(&"https://{$ONYX_CADDY_DOMAIN} {"));
}

#[test]
fn tls_hsts_and_loopback_upstream() {
    assert_eq!(args("tls"), ["{$ONYX_CADDY_TLS}"]);
    let hsts = args("Strict-Transport-Security").join(" ");
    let age: u64 = hsts
        .trim_matches('"')
        .strip_prefix("max-age=")
        .and_then(|a| a.split(';').next())
        .and_then(|a| a.parse().ok())
        .expect("max-age");
    assert!(age >= 31_536_000, "HSTS max-age {age} under a year");
    // Also on Caddy's own errors (413, 502), which skip the site's header
    // handler (review finding, TLS-7).
    assert_eq!(count("Strict-Transport-Security"), 2);
    assert_eq!(count("handle_errors"), 1);
    // HTTP/1.1 only, so a per-address connection cap bounds requests in
    // flight too (TLS-2).
    assert_eq!(args("protocols"), ["h1"]);
    // onyx-node binds loopback (it refuses anything else without an opt-in).
    let upstream = args("reverse_proxy")[0];
    assert!(
        upstream.ends_with(":127.0.0.1:8080}"),
        "upstream {upstream} is not loopback by default"
    );
}

#[test]
fn the_clients_forwarded_for_is_never_believed() {
    // Without trusted_proxies Caddy drops the client's X-Forwarded-For and
    // sends the client's address; onyx-node trusts only Caddy
    // (ONYX_TRUSTED_PROXIES=127.0.0.1), so a client cannot pick its own
    // rate-limit key. No header_up may rewrite it either.
    for l in lines() {
        assert!(!l.contains("trusted_proxies"), "{l}");
        assert!(!l.to_ascii_lowercase().contains("x-forwarded-for"), "{l}");
    }
}

#[test]
fn limits_fit_the_server() {
    let api = ApiConfig::default();
    // A full PAYMENT-SIGNATURE header plus the rest of the request.
    let header_cap = kib(args("max_header_size")[0]);
    assert!(
        header_cap >= onyx_api::x402::MAX_PAYMENT_HEADER + 8 * 1024,
        "max_header_size {header_cap} cannot carry a {} byte payment header",
        onyx_api::x402::MAX_PAYMENT_HEADER
    );
    // No route reads a request body: the cap stays a token one, and the
    // proxy reads past it before dialing, so the 413 does not depend on
    // when onyx-node answers (a 2 KiB body passed without the buffer).
    let cap = kib(args("max_size")[0]);
    assert!(cap <= 1024);
    assert!(kib(args("request_buffers")[0]) > cap);
    // A response may take the request deadline plus the send timeout.
    let write = secs(args("write")[0]);
    assert!(
        write > (api.request_timeout + api.send_timeout).as_secs(),
        "write {write}s cuts responses onyx-node still sends"
    );
    // The proxy waits for the answer at least as long as the server works on it.
    assert!(secs(args("response_header_timeout")[0]) > api.request_timeout.as_secs());
    // Slow headers end at the proxy no later than at the server (default 10 s).
    assert!(secs(args("read_header")[0]) <= 10);
}

/// Arguments inside one explicitly named nested Caddy block.
fn block_args(marker: &str, key: &str) -> Vec<&'static str> {
    let ls = lines();
    let start = ls
        .iter()
        .position(|l| *l == format!("{marker} {{"))
        .expect("block");
    let mut depth = 1;
    for l in &ls[start + 1..] {
        if *l == "}" {
            depth -= 1;
        }
        if depth == 0 {
            break;
        }
        if l.split_whitespace().next() == Some(key) {
            return l.split_whitespace().skip(1).collect();
        }
        if l.ends_with(" {") {
            depth += 1;
        }
    }
    panic!("no {key} in {marker}")
}

#[test]
fn submission_methods_body_caps_and_all_extra_routes_match_the_node() {
    let routes = routes_of(API_SUBMIT, "pub fn routes(");
    assert_eq!(
        routes,
        ["/v1/submit/check", "/v1/submit", "/v1/submit/{id}"]
    );
    assert!(API_SUBMIT.contains(".route(\"/v1/submit/check\", post(check))"));
    assert!(API_SUBMIT.contains(".route(\"/v1/submit\", post(submit))"));
    assert!(API_SUBMIT.contains(".route(\"/v1/submit/{id}\", get(status))"));
    assert_eq!(block_args("@submit", "method"), ["POST"]);
    assert_eq!(
        block_args("@submit", "path"),
        ["/v1/submit", "/v1/submit/check", "/v1/wants"]
    );
    // NL9: the board posts a want in the same bounded group; the node caps it far lower.
    assert_eq!(
        routes_of(API_WANTS, "pub fn routes("),
        ["/v1/wants", "/v1/wants"]
    );
    assert!(API_WANTS.contains(".route(\"/v1/wants\", get(board))"));
    assert!(API_WANTS.contains(".route(\"/v1/wants\", post(post_want))"));
    const { assert!(onyx_api::wants::MAX_WANTS_BODY < onyx_api::submit::MAX_BODY) };
    assert_eq!(
        kib(block_args("handle @submit", "max_size")[0]),
        onyx_api::submit::MAX_BODY
    );
    assert!(kib(block_args("handle @submit", "request_buffers")[0]) > onyx_api::submit::MAX_BODY);
    assert_eq!(
        block_args("handle @submit", "reverse_proxy"),
        block_args("handle @api", "reverse_proxy")
    );
    assert!(
        secs(block_args("handle @submit", "response_header_timeout")[0])
            > ApiConfig::default().request_timeout.as_secs()
    );
    let app = include_str!("../src/app.rs");
    assert!(app.contains("crate::submissions::routes(c.submissions.as_deref())"));
    assert!(app.contains("extra.merge(intake)"));
    assert!(app.contains("router_with(state, extra)"));
}
