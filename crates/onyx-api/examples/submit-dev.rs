//! Local dev server for the submission routes only (ROADMAP S1, ADR-0038):
//! `POST /v1/submit/check`, `POST /v1/submit`, `GET /v1/submit/{id}` on a
//! loopback address, for the website's `#/submit` page in `pnpm dev`
//! (apps/web/README.md). Not a deployment (review S1-R1-8): the routes are
//! mounted without the API's guard, so there is no concurrency limit, no
//! request deadline (a pull still stops at 10 s) and no trusted-proxy client
//! key (every request is client "local", one shared budget); no TLS; one
//! process. A hosted endpoint mounts them behind the guard in onyx-node (S5).
//!
//! ```sh
//! cargo run -p onyx-api --features http-client --example submit-dev -- [JOURNAL] [127.0.0.1:8788]
//! ```
//! JOURNAL defaults to `target/submit-dev/submissions.journal`. A check pull
//! goes to the network only for a rights-cleared host (today
//! api.fiscaldata.treasury.gov) and only when the page asks for one;
//! `ONYX_SUBMIT_NO_FETCH=1` turns pulls off.

use onyx_api::submit::{routes, HttpsFetch, PageFetch, SubmitLimits, SubmitStore};
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let journal = args
        .next()
        .unwrap_or_else(|| "target/submit-dev/submissions.journal".into());
    let listen: SocketAddr = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:8788".into())
        .parse()?;
    if !listen.ip().is_loopback() {
        return Err("submit-dev listens on a loopback address only".into());
    }
    if let Some(dir) = std::path::Path::new(&journal).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let fetcher: Option<Arc<dyn PageFetch>> =
        if std::env::var("ONYX_SUBMIT_NO_FETCH").as_deref() == Ok("1") {
            None
        } else {
            Some(Arc::new(HttpsFetch::new()))
        };
    let store = SubmitStore::open(&journal, fetcher, SubmitLimits::default())?;
    let app = routes(Arc::new(store));
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!("submit-dev: http://{listen}/v1/submit (journal {journal})");
    axum::serve(listener, app).await?;
    Ok(())
}
