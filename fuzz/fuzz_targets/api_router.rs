//! The public HTTP API (`onyx_api::router`) on a small fixture: one shard,
//! one market, free or paid through the mock facilitator. The input is a
//! request in text: a flag byte (bit 0: paid), then `METHOD URI`, then one
//! `name: value` header per line (paths, queries, `payment-signature`,
//! `X-Forwarded-For`, contributor passes). Every request gets a response;
//! nothing panics.
#![no_main]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use libfuzzer_sys::fuzz_target;
use onyx_fuzz_common::{api_fixture, block_on};
use std::net::SocketAddr;
use tower::ServiceExt;

fuzz_target!(|data: &[u8]| {
    let Some((&flag, rest)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or("");
    let (method, uri) = first.split_once(' ').unwrap_or(("GET", first));
    let Ok(method) = Method::from_bytes(method.as_bytes()) else {
        return;
    };
    let mut req = Request::builder().method(method).uri(uri);
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            req = req.header(k.trim(), v.trim());
        }
    }
    let Ok(mut req) = req.body(Body::empty()) else {
        return;
    };
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 9], 4000))));
    let Some(f) = api_fixture(flag & 1 == 1) else {
        return;
    };
    let _ = block_on(async {
        let resp = onyx_api::router(f.state.clone())
            .oneshot(req)
            .await
            .expect("the router always answers");
        let _ = resp.into_body().collect().await;
    });
});
