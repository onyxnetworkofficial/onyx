//! THREATS CL4-1 and CL4-2 (ADR-0054 item 2): a keyed fetch sends the holder's key only to the
//! template's host, only in the place the template names, never across a redirect, and no
//! error text holds it. Against a local fixture server (no live call).

use onyx_fetch::fixture::{FixedResolver, FixtureServer, LoopbackDial, Reply, FIXTURE_PUBLIC_IP};
use onyx_fetch::{ApiKey, FetchError, HttpsFetch, KeyPlacement, Limits};
use std::sync::Arc;

/// A planted FAKE key (never a real one).
const KEY: &str = "FAKE-cl4-key-0001-abcdef";

fn key() -> ApiKey {
    ApiKey::new(KEY).expect("key")
}

fn query() -> KeyPlacement {
    KeyPlacement::Query {
        name: "api_key".into(),
    }
}

fn header() -> KeyPlacement {
    KeyPlacement::Header {
        name: "x-api-key".into(),
        bearer: false,
    }
}

#[tokio::test]
async fn a_query_key_goes_in_its_one_parameter_to_the_template_host() {
    let s = FixtureServer::spawn(vec![(
        format!("/v1/x?d=2026-10-27&api_key={KEY}"),
        Reply::json(r#"{"v":"1"}"#),
    )])
    .await
    .expect("server");
    let body = s
        .fetch(Limits::observer(1024))
        .get_keyed(
            "https://keyed.example.org/v1/x?d=2026-10-27&api_key={api_key}",
            &query(),
            &key(),
        )
        .await
        .expect("body");
    assert_eq!(body, br#"{"v":"1"}"#);
    let seen = s.seen();
    assert_eq!(seen.requests.len(), 1);
    let head = seen.requests[0].to_ascii_lowercase();
    assert!(head.contains("host: keyed.example.org\r\n"), "{head}");
    assert_eq!(
        seen.requests[0].matches(KEY).count(),
        1,
        "once, in the query"
    );
}

#[tokio::test]
async fn a_header_key_goes_in_its_one_header_and_not_in_the_url() {
    let s = FixtureServer::spawn(vec![("/v1/x".into(), Reply::json(r#"{"v":"1"}"#))])
        .await
        .expect("server");
    s.fetch(Limits::observer(1024))
        .get_keyed("https://keyed.example.org/v1/x", &header(), &key())
        .await
        .expect("body");
    let r = &s.seen().requests[0];
    assert!(r.starts_with("GET /v1/x HTTP/1.1\r\n"), "{r}");
    assert!(
        r.to_ascii_lowercase()
            .contains(&format!("x-api-key: {}\r\n", KEY.to_ascii_lowercase())),
        "{r}"
    );
    assert_eq!(r.matches(KEY).count(), 1);
}

/// THREATS CL4-2: a redirect is never followed, so the key is never sent to its target; the
/// only request is the one to the template's host, and the error does not hold the key.
#[tokio::test]
async fn a_redirect_never_carries_the_key() {
    let s = FixtureServer::spawn(vec![
        (
            format!("/v1/x?api_key={KEY}"),
            Reply::status(
                302,
                &[(
                    "location",
                    "https://collector.example.net/steal?k=FAKE-cl4-key-0001-abcdef",
                )],
            ),
        ),
        (
            "/h".into(),
            Reply::status(307, &[("location", "https://collector.example.net/h")]),
        ),
    ])
    .await
    .expect("server");
    let dial = Arc::new(LoopbackDial::new(s.addr()));
    let resolver = Arc::new(FixedResolver::new(vec![FIXTURE_PUBLIC_IP]));
    let f = HttpsFetch::with_transport(resolver.clone(), dial.clone(), Limits::observer(1024));
    let e = f
        .get_keyed(
            "https://keyed.example.org/v1/x?api_key={api_key}",
            &query(),
            &key(),
        )
        .await
        .expect_err("a redirect");
    assert_eq!(e, FetchError::Redirect(302));
    let e = f
        .get_keyed("https://keyed.example.org/h", &header(), &key())
        .await
        .expect_err("a redirect");
    assert_eq!(e, FetchError::Redirect(307));
    // Two requests, both to the template's host; one resolution and one dial each.
    let seen = s.seen();
    assert_eq!(seen.requests.len(), 2);
    for r in &seen.requests {
        assert!(r
            .to_ascii_lowercase()
            .contains("host: keyed.example.org\r\n"));
    }
    assert_eq!(resolver.calls(), 2);
    assert_eq!(dial.seen().len(), 2);
}

/// THREATS CL4-1: whatever goes wrong, the error text has no copy of the key.
#[tokio::test]
async fn keyed_fetch_errors_never_hold_the_key() {
    let mut huge = Reply::json(vec![b' '; 1024]);
    huge.repeat = 4;
    let s = FixtureServer::spawn(vec![
        (format!("/big?api_key={KEY}"), huge),
        (format!("/gone?api_key={KEY}"), Reply::status(401, &[])),
    ])
    .await
    .expect("server");
    let f = s.fetch(Limits::observer(1024));
    let cases = [
        f.get_keyed(
            "https://keyed.example.org/big?api_key={api_key}",
            &query(),
            &key(),
        )
        .await,
        f.get_keyed(
            "https://keyed.example.org/gone?api_key={api_key}",
            &query(),
            &key(),
        )
        .await,
        f.get_keyed("https://keyed.example.org/nothing", &query(), &key())
            .await,
        f.get_keyed(
            "http://keyed.example.org/x?api_key={api_key}",
            &query(),
            &key(),
        )
        .await,
        f.get_keyed(
            &format!(
                "https://keyed.example.org/x?api_key={{api_key}}&{}",
                "a".repeat(2100)
            ),
            &query(),
            &key(),
        )
        .await,
    ];
    for c in cases {
        let e = c.expect_err("an error");
        let text = format!("{e} {e:?}");
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains("FAKE-cl4"), "{text}");
    }
    // A private address is refused before any connection, so the key goes nowhere.
    let dial = Arc::new(LoopbackDial::new(s.addr()));
    let f = HttpsFetch::with_transport(
        Arc::new(FixedResolver::new(vec!["127.0.0.1".parse().expect("ip")])),
        dial.clone(),
        Limits::observer(1024),
    );
    assert!(matches!(
        f.get_keyed("https://keyed.example.org/x", &header(), &key())
            .await,
        Err(FetchError::Refused(_))
    ));
    assert!(dial.seen().is_empty());
}
