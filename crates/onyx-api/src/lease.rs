//! A response body that holds its in-flight permit until it is written
//! (M7-L47-F2).
//!
//! Without it, the permit is released when the handler returns, and a slow
//! client then holds a whole export in memory outside every limit. The body
//! is handed to the connection in [`CHUNK`]-byte frames, so the connection
//! asks for the next frame only when it has room (it buffers a bounded
//! amount), and the permit is held until the last frame is taken. A client
//! that stops reading cannot keep the permit: a watchdog releases it after
//! the send timeout, and the next frame asked for after that is an error,
//! which closes the connection. A connection that is never polled again
//! keeps its unsent bytes until the client closes it: public deployments
//! need a buffering proxy with a send timeout (docs/ops/run-api.md).

use axum::body::{Body, Bytes};
use http_body::{Frame, SizeHint};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{oneshot, OwnedSemaphorePermit};
use tokio::time::Instant;

/// Largest frame handed to the connection.
pub const CHUNK: usize = 64 * 1024;

/// The body was not written within the send timeout.
#[derive(Debug, thiserror::Error)]
#[error("response not written within the send timeout")]
pub struct SendTimeout;

/// `inner`, written in frames of at most [`CHUNK`] bytes, holding `permit`
/// until the end of the body, its drop, or `send_timeout`, whichever is first.
pub fn leased(inner: Body, permit: OwnedSemaphorePermit, send_timeout: Duration) -> Body {
    let slot = Arc::new(Mutex::new(Some(permit)));
    let (done, wait) = oneshot::channel::<()>();
    let watchdog = slot.clone();
    tokio::spawn(async move {
        // Ends early when the body ends or is dropped (the sender goes away);
        // at the deadline it takes the permit back from a stalled body.
        if tokio::time::timeout(send_timeout, wait).await.is_err() {
            release(&watchdog);
        }
    });
    Body::new(Leased {
        inner,
        pending: Bytes::new(),
        deadline: Instant::now() + send_timeout,
        slot,
        done: Some(done),
    })
}

fn release(slot: &Mutex<Option<OwnedSemaphorePermit>>) {
    // A poisoned lock still gives the permit back.
    let permit = match slot.lock() {
        Ok(mut g) => g.take(),
        Err(p) => p.into_inner().take(),
    };
    drop(permit);
}

struct Leased {
    inner: Body,
    pending: Bytes,
    deadline: Instant,
    slot: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    done: Option<oneshot::Sender<()>>,
}

impl Leased {
    /// The body is finished (written, failed or dropped): the permit goes
    /// back now, not when the watchdog next runs.
    fn end(&mut self) {
        release(&self.slot);
        self.done = None;
    }
}

impl Drop for Leased {
    fn drop(&mut self) {
        self.end();
    }
}

impl http_body::Body for Leased {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = &mut *self;
        if Instant::now() >= this.deadline {
            this.end();
            return Poll::Ready(Some(Err(axum::Error::new(SendTimeout))));
        }
        if this.pending.is_empty() {
            match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Ready(Some(Ok(f))) => match f.into_data() {
                    Ok(d) => this.pending = d,
                    Err(f) => return Poll::Ready(Some(Ok(f))),
                },
                Poll::Ready(None) => {
                    this.end();
                    return Poll::Ready(None);
                }
                other => return other,
            }
        }
        let n = this.pending.len().min(CHUNK);
        Poll::Ready(Some(Ok(Frame::data(this.pending.split_to(n)))))
    }

    fn is_end_stream(&self) -> bool {
        self.pending.is_empty() && self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        let h = self.inner.size_hint();
        let p = self.pending.len() as u64;
        let mut out = SizeHint::new();
        if let Some(u) = h.upper() {
            out.set_upper(u + p);
        }
        out.set_lower(h.lower() + p);
        out
    }
}
