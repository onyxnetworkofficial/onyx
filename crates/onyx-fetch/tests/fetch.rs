//! THREATS CL3-1, CL3-2 and the slow-loris row: the transport's rules against a local fixture
//! server (no live third-party call). Every fetch goes through `HttpsFetch::get`, so the URL
//! check, the single resolution and the public-address check run as they do live.

use onyx_fetch::fixture::{FixedResolver, FixtureServer, LoopbackDial, Reply, FIXTURE_PUBLIC_IP};
use onyx_fetch::{FetchError, HttpsFetch, Limits, MAX_BYTES_CEILING, USER_AGENT};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

const URL: &str = "https://data.example.org/v1/x?d=2026-10-27";
const PATH: &str = "/v1/x?d=2026-10-27";

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

fn limits(max_bytes: usize) -> Limits {
    Limits {
        max_bytes,
        timeout: Duration::from_secs(20),
        user_agent: USER_AGENT,
        accept: "application/json",
    }
}

#[tokio::test]
async fn a_plain_answer_is_read_with_the_neutral_headers() {
    let s = FixtureServer::spawn(vec![(PATH.into(), Reply::json(r#"{"v":1}"#))])
        .await
        .expect("server");
    let body = s.fetch(limits(1024)).get(URL).await.expect("body");
    assert_eq!(body, br#"{"v":1}"#);
    let head = s.seen().requests[0].to_ascii_lowercase();
    assert!(
        head.starts_with("get /v1/x?d=2026-10-27 http/1.1\r\n"),
        "{head}"
    );
    assert!(head.contains("host: data.example.org\r\n"));
    assert!(head.contains("user-agent: onyx-observer/1\r\n"));
    assert!(head.contains("accept: application/json\r\n"));
    assert!(!head.contains("cookie"));
}

/// THREATS CL3-1: a template or a DNS answer never makes the fetch reach a private address.
#[tokio::test]
async fn fetch_refuses_private_rebinding_and_redirects() {
    let s = FixtureServer::spawn(vec![
        (PATH.into(), Reply::json(r#"{"v":1}"#)),
        (
            "/moved".into(),
            Reply::status(
                302,
                &[("location", "https://169.254.169.254/latest/meta-data/")],
            ),
        ),
        (
            "/see-other".into(),
            Reply::status(303, &[("location", "/v1/x")]),
        ),
    ])
    .await
    .expect("server");
    // A name that resolves (also) to a private, loopback, link-local, metadata or
    // IPv4-in-IPv6 address is refused before any connection.
    for answer in [
        vec![ip("127.0.0.1")],
        vec![ip("10.1.2.3")],
        vec![ip("169.254.169.254")],
        vec![FIXTURE_PUBLIC_IP, ip("192.168.1.1")],
        vec![ip("::1")],
        vec![ip("::ffff:127.0.0.1")],
        vec![ip("fd00:ec2::254")],
        vec![ip("100.64.0.1")],
        vec![],
    ] {
        let dial = Arc::new(LoopbackDial::new(s.addr()));
        let f = HttpsFetch::with_transport(
            Arc::new(FixedResolver::new(answer.clone())),
            dial.clone(),
            limits(1024),
        );
        assert!(
            matches!(f.get(URL).await, Err(FetchError::Refused(_))),
            "{answer:?}"
        );
        assert!(dial.seen().is_empty(), "no connection for {answer:?}");
    }
    // Rebinding: the name answers a public address at the check and a private one after. The
    // fetch resolves once and dials exactly the checked addresses.
    let resolver = Arc::new(FixedResolver::sequence(vec![
        vec![FIXTURE_PUBLIC_IP],
        vec![ip("127.0.0.1")],
    ]));
    let dial = Arc::new(LoopbackDial::new(s.addr()));
    let f = HttpsFetch::with_transport(resolver.clone(), dial.clone(), limits(1024));
    assert_eq!(f.get(URL).await.expect("body"), br#"{"v":1}"#);
    assert_eq!(resolver.calls(), 1);
    assert_eq!(dial.seen(), vec![vec![FIXTURE_PUBLIC_IP]]);
    // IP literals, other ports, user info and plain http never reach the resolver.
    let resolver = Arc::new(FixedResolver::new(vec![FIXTURE_PUBLIC_IP]));
    let f = HttpsFetch::with_transport(
        resolver.clone(),
        Arc::new(LoopbackDial::new(s.addr())),
        limits(1024),
    );
    for url in [
        "https://127.0.0.1/v1/x",
        "https://[::1]/v1/x",
        "https://169.254.169.254/latest/meta-data/",
        "https://data.example.org:8080/v1/x",
        "https://admin@data.example.org/v1/x",
        "http://data.example.org/v1/x",
        "https://localhost/v1/x",
    ] {
        assert!(
            matches!(f.get(url).await, Err(FetchError::BadUrl(_))),
            "{url}"
        );
    }
    assert_eq!(resolver.calls(), 0);
    // A redirect is an answer of its own, never followed.
    let f = s.fetch(limits(1024));
    assert_eq!(
        f.get("https://data.example.org/moved").await,
        Err(FetchError::Redirect(302))
    );
    assert_eq!(
        f.get("https://data.example.org/see-other").await,
        Err(FetchError::Redirect(303))
    );
    assert_eq!(
        f.get("https://data.example.org/missing").await,
        Err(FetchError::Status(404))
    );
    assert!(s.seen().requests.iter().all(|r| !r.contains("meta-data")));
}

/// THREATS CL3-2: a body over the cap is refused, declared or not, plain or compressed, and the
/// rest is not read.
#[tokio::test]
async fn a_body_over_the_cap_is_unavailable_and_not_read_further() {
    use std::io::Write as _;
    let cap = 64 * 1024;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&vec![b'0'; 4 * MAX_BYTES_CEILING])
        .expect("gzip");
    let bomb = enc.finish().expect("gzip");
    let mut small = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    small.write_all(br#"{"v":1}"#).expect("gzip");
    let small = small.finish().expect("gzip");
    let gz = |body: Vec<u8>| {
        let mut r = Reply::json(body);
        r.headers.push(("content-encoding".into(), "gzip".into()));
        r
    };
    let mut streamed = Reply::json(vec![b' '; 64 * 1024]);
    streamed.repeat = 1024; // 64 MiB if read to the end
    streamed.content_length = false;
    let mut declared = Reply::json(b"{}".to_vec());
    declared
        .headers
        .push(("content-length".into(), "10000000".into()));
    let mut brotli = Reply::json(b"{}".to_vec());
    brotli
        .headers
        .push(("content-encoding".into(), "br".into()));
    let s = FixtureServer::spawn(vec![
        ("/exact".into(), Reply::json(vec![b' '; cap])),
        ("/one-over".into(), Reply::json(vec![b' '; cap + 1])),
        ("/streamed".into(), streamed),
        ("/declared".into(), declared),
        ("/bomb".into(), gz(bomb)),
        ("/gzip".into(), gz(small)),
        ("/br".into(), brotli),
    ])
    .await
    .expect("server");
    let f = s.fetch(limits(cap));
    let get = |p: &str| {
        let f = f.clone();
        let url = format!("https://data.example.org{p}");
        async move { f.get(&url).await }
    };
    assert_eq!(get("/exact").await.expect("at the cap").len(), cap);
    assert_eq!(get("/one-over").await, Err(FetchError::TooLarge(cap)));
    assert_eq!(get("/declared").await, Err(FetchError::TooLarge(cap)));
    assert_eq!(get("/bomb").await, Err(FetchError::TooLarge(cap)));
    assert_eq!(get("/gzip").await.expect("gzip"), br#"{"v":1}"#);
    assert!(matches!(get("/br").await, Err(FetchError::Encoding(_))));
    assert_eq!(get("/streamed").await, Err(FetchError::TooLarge(cap)));
    // The streamed body stopped near the cap: the server's writes failed once the client left,
    // far short of 64 MiB (the margin is the socket buffers).
    let written = loop {
        let w = s.seen().written;
        if w.len() == 7 {
            break w;
        }
        tokio::task::yield_now().await;
    };
    assert!(written.iter().all(|n| *n < 32 * 1024 * 1024), "{written:?}");
}

/// The slow-loris row: a server that sends a byte now and then never holds an observer past
/// the whole fetch's deadline. Paused tokio time: the 20 s deadline passes without a real wait.
#[tokio::test(start_paused = true)]
async fn a_slow_body_hits_the_whole_fetch_deadline() {
    let mut slow = Reply::json(vec![]);
    slow.trickle = Some(Duration::from_secs(1));
    let s = FixtureServer::spawn(vec![(PATH.into(), slow)])
        .await
        .expect("server");
    let f = s.fetch(Limits::observer(MAX_BYTES_CEILING));
    assert_eq!(f.limits().timeout, Duration::from_secs(20));
    assert_eq!(f.get(URL).await, Err(FetchError::Timeout(20)));
}
