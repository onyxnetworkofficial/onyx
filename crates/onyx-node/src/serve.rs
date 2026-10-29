//! The HTTP server loop (AUD-1-F5): `axum::serve` has no header read
//! timeout and no connection cap, so a slow client (slowloris) could hold
//! connections open without ever sending a request. This loop serves each
//! connection with hyper's HTTP/1 builder, a header read timeout and a cap on
//! open connections, and stops gracefully: no new connections, then every
//! open one finishes its request.
//!
//! The cap is shared fairly (AUD-1R-F1): one client (an IPv4 address or an
//! IPv6 /64) holds at most `max_per_client` connections, an IPv6 /56 at most
//! `PREFIX_RATE_MULTIPLIER` times that and a /48 that factor again, the same
//! scopes as the API's rate limiter (AUD-2-F5: a /56 holds 256 /64s, so a
//! /64-only cap let one /56 take 8,192 slots), and when every slot
//! is taken a new connection evicts the oldest one that has not yet sent a
//! request, instead of being refused. A peer that opens idle sockets can then
//! neither hold more than its share nor starve a client that sends its
//! request at once. Trusted proxies are exempt from the per-client cap: every
//! client behind them shares their address (their own limits apply, and the
//! API's per-client budgets read `X-Forwarded-For` at the HTTP layer).

use axum::extract::ConnectInfo;
use axum::{Extension, Router};
use hyper::service::Service as _;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use onyx_api::{Cidr, PREFIX_RATE_MULTIPLIER};
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

/// Default connections one client may hold open (AUD-1R-F1).
pub const DEFAULT_MAX_PER_CLIENT: usize = 32;

/// Connection limits of the server loop.
#[derive(Clone, Copy, Debug)]
pub struct ServeLimits {
    /// A connection must send its request headers within this time.
    pub header_read_timeout: Duration,
    /// Open connections. When all are taken, a new connection evicts the
    /// oldest one that has not sent a request; with none such, it is closed.
    pub max_connections: usize,
    /// Open connections per client (an IPv4 address or an IPv6 /64; a /56
    /// may hold `PREFIX_RATE_MULTIPLIER` times this and a /48 that factor
    /// again); a new one beyond is closed at once. Trusted proxies are exempt.
    pub max_per_client: usize,
}

/// One scope a connection counts against: an IPv4 address, or an IPv6 prefix
/// of the given length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Scope {
    V4(u32),
    V6 { prefix: u128, len: u8 },
}

fn v6(a: u128, len: u8) -> Scope {
    Scope::V6 {
        prefix: a & (u128::MAX << (128 - u32::from(len))),
        len,
    }
}

/// The client's own scope: an IPv4 address or an IPv6 /64.
#[cfg(test)]
fn scope(ip: IpAddr) -> Scope {
    scopes(ip)[0].0
}

/// Every scope a connection from `ip` counts against, with its cap as a
/// multiple of `max_per_client`: an IPv4 address alone, or an IPv6 /64, /56
/// and /48 (as `onyx_api::rate_scopes`).
fn scopes(ip: IpAddr) -> Vec<(Scope, usize)> {
    let m = PREFIX_RATE_MULTIPLIER as usize;
    match ip.to_canonical() {
        IpAddr::V4(a) => vec![(Scope::V4(u32::from(a)), 1)],
        IpAddr::V6(a) => {
            let a = u128::from(a);
            vec![(v6(a, 64), 1), (v6(a, 56), m), (v6(a, 48), m * m)]
        }
    }
}

struct Conn {
    scopes: Vec<Scope>,
    /// The connection has sent at least one request (its headers arrived).
    seen: bool,
    abort: Option<AbortHandle>,
}

/// The open connections. Ids grow with accept order, so the smallest id is
/// the oldest connection.
#[derive(Default)]
struct Book {
    conns: HashMap<u64, Conn>,
    per_scope: HashMap<Scope, usize>,
    next: u64,
}

impl Book {
    fn insert(&mut self, scopes: Vec<Scope>) -> u64 {
        let id = self.next;
        self.next += 1;
        for s in &scopes {
            *self.per_scope.entry(*s).or_insert(0) += 1;
        }
        self.conns.insert(
            id,
            Conn {
                scopes,
                seen: false,
                abort: None,
            },
        );
        id
    }

    fn remove(&mut self, id: u64) -> Option<Conn> {
        let c = self.conns.remove(&id)?;
        for s in &c.scopes {
            if let Some(n) = self.per_scope.get_mut(s) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    self.per_scope.remove(s);
                }
            }
        }
        Some(c)
    }

    /// Admit a connection counted against `scopes`, evicting the oldest
    /// connection that has not sent a request when every slot is taken.
    /// `None`: refused.
    fn admit(
        &mut self,
        scopes: &[(Scope, usize)],
        trusted: bool,
        limits: &ServeLimits,
    ) -> Option<u64> {
        let per = limits.max_per_client.max(1);
        if !trusted
            && scopes
                .iter()
                .any(|(s, m)| self.per_scope.get(s).copied().unwrap_or(0) >= per.saturating_mul(*m))
        {
            return None;
        }
        if self.conns.len() >= limits.max_connections.max(1) {
            let victim = self
                .conns
                .iter()
                .filter(|(_, c)| !c.seen)
                .map(|(id, _)| *id)
                .min()?;
            if let Some(abort) = self.remove(victim).and_then(|c| c.abort) {
                abort.abort();
            }
        }
        // A trusted proxy's connections are not counted against any client
        // scope (AUD-2R-F5): a busy proxy must not fill the /48 its address
        // sits in and lock out direct clients of that /48.
        let counted = if trusted {
            Vec::new()
        } else {
            scopes.iter().map(|(s, _)| *s).collect()
        };
        Some(self.insert(counted))
    }
}

fn lock(book: &Mutex<Book>) -> MutexGuard<'_, Book> {
    book.lock().unwrap_or_else(|p| p.into_inner())
}

/// Removes a connection from the book when its task ends (or is aborted).
struct Leave {
    book: Arc<Mutex<Book>>,
    id: u64,
}

impl Drop for Leave {
    fn drop(&mut self) {
        lock(&self.book).remove(self.id);
    }
}

/// Serve `app` on `listener` until `stop` resolves, then let open
/// connections finish. No trusted proxies: every peer is a client. The peer
/// address reaches handlers as `ConnectInfo<SocketAddr>`.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: ServeLimits,
    stop: impl Future<Output = ()>,
) -> std::io::Result<()> {
    serve_with(listener, app, limits, &[], stop).await
}

/// [`serve`] with `trusted_proxies` exempt from the per-client cap.
pub async fn serve_with(
    listener: TcpListener,
    app: Router,
    limits: ServeLimits,
    trusted_proxies: &[Cidr],
    stop: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let book = Arc::new(Mutex::new(Book::default()));
    let graceful = GracefulShutdown::new();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(a) => a,
                    // Out of file descriptors and the like: back off a moment.
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let trusted = trusted_proxies.iter().any(|c| c.contains(peer.ip()));
                let Some(id) = lock(&book).admit(&scopes(peer.ip()), trusted, &limits) else {
                    // Over the client's share, or every slot is mid-request:
                    // close at once rather than queue.
                    drop(stream);
                    continue;
                };
                let inner = TowerToHyperService::new(
                    app.clone().layer(Extension(ConnectInfo(peer))),
                );
                let marks = book.clone();
                let svc = hyper::service::service_fn(move |req| {
                    // Headers arrived: this connection is no longer evictable.
                    if let Some(c) = lock(&marks).conns.get_mut(&id) {
                        c.seen = true;
                    }
                    inner.call(req)
                });
                let conn = hyper::server::conn::http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(limits.header_read_timeout)
                    .serve_connection(TokioIo::new(stream), svc);
                let conn = graceful.watch(conn);
                let leave = Leave { book: book.clone(), id };
                let task = tokio::spawn(async move {
                    let _leave = leave;
                    let _ = conn.await;
                });
                // Same task as the admit above: no other accept ran between.
                if let Some(c) = lock(&book).conns.get_mut(&id) {
                    c.abort = Some(task.abort_handle());
                }
            }
        }
    }
    drop(listener);
    graceful.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ServeLimits = ServeLimits {
        header_read_timeout: Duration::from_secs(1),
        max_connections: 3,
        max_per_client: 2,
    };

    #[test]
    fn a_client_holds_at_most_its_share() {
        let mut b = Book::default();
        let a = scope("10.0.0.1".parse().expect("ip"));
        assert!(b.admit(&[(a, 1)], false, &LIMITS).is_some());
        assert!(b.admit(&[(a, 1)], false, &LIMITS).is_some());
        assert!(
            b.admit(&[(a, 1)], false, &LIMITS).is_none(),
            "third from one client"
        );
        // A trusted proxy is exempt from the per-client cap.
        assert!(b.admit(&[(a, 1)], true, &LIMITS).is_some());
    }

    #[test]
    fn ipv6_counts_per_slash_64_and_mapped_v4_as_v4() {
        let x = scope("2001:db8:1:2::1".parse().expect("ip"));
        let y = scope("2001:db8:1:2:ffff::9".parse().expect("ip"));
        let z = scope("2001:db8:1:3::1".parse().expect("ip"));
        assert_eq!(x, y);
        assert_ne!(x, z);
        assert_eq!(
            scope("::ffff:10.0.0.1".parse().expect("ip")),
            scope("10.0.0.1".parse().expect("ip"))
        );
    }

    #[test]
    fn a_full_book_evicts_the_oldest_connection_without_a_request() {
        let mut b = Book::default();
        let s = |n: &str| scope(n.parse().expect("ip"));
        let first = b.admit(&[(s("10.0.0.1"), 1)], false, &LIMITS).expect("1");
        let second = b.admit(&[(s("10.0.0.2"), 1)], false, &LIMITS).expect("2");
        let third = b.admit(&[(s("10.0.0.3"), 1)], false, &LIMITS).expect("3");
        // The oldest has sent a request: the next oldest idle one goes.
        b.conns.get_mut(&first).expect("first").seen = true;
        let fourth = b.admit(&[(s("10.0.0.4"), 1)], false, &LIMITS).expect("4");
        assert!(b.conns.contains_key(&first));
        assert!(!b.conns.contains_key(&second));
        assert!(b.conns.contains_key(&third) && b.conns.contains_key(&fourth));
        // Every connection mid-request: a new one is refused.
        for id in [third, fourth] {
            b.conns.get_mut(&id).expect("conn").seen = true;
        }
        assert!(b.admit(&[(s("10.0.0.5"), 1)], false, &LIMITS).is_none());
        assert_eq!(b.conns.len(), 3);
        assert_eq!(b.per_scope.values().sum::<usize>(), 3);
    }

    /// AUD-2-F5: one /56 cannot take the whole server through its 256 /64s:
    /// it holds at most `PREFIX_RATE_MULTIPLIER` times a client's share, and a
    /// /48 that factor again.
    #[test]
    fn an_ipv6_slash_56_and_48_hold_bounded_shares() {
        let limits = ServeLimits {
            header_read_timeout: Duration::from_secs(1),
            max_connections: 10_000,
            max_per_client: 2,
        };
        let m = PREFIX_RATE_MULTIPLIER as usize;
        let mut b = Book::default();
        // Many /64s inside one /56: the /56 stops them at 2 x m.
        let mut admitted = 0;
        for k in 0..256u32 {
            let ip: IpAddr = format!("2001:db8:7:12{:02x}::1", k % 256)
                .parse()
                .expect("ip");
            for _ in 0..2 {
                if b.admit(&scopes(ip), false, &limits).is_some() {
                    admitted += 1;
                }
            }
        }
        assert_eq!(admitted, 2 * m, "one /56");
        // Many /56s inside one /48: the /48 stops them at 2 x m x m.
        let mut b = Book::default();
        let mut admitted = 0;
        for k in 0..256u32 {
            for j in 0..256u32 {
                let ip: IpAddr = format!("2001:db8:7:{:02x}{:02x}::1", k, j)
                    .parse()
                    .expect("ip");
                if b.admit(&scopes(ip), false, &limits).is_some() {
                    admitted += 1;
                }
            }
        }
        assert_eq!(admitted, 2 * m * m, "one /48");
        // Another /48 still gets in, and trusted proxies stay exempt.
        let other: IpAddr = "2001:db9::1".parse().expect("ip");
        assert!(b.admit(&scopes(other), false, &limits).is_some());
        let inside: IpAddr = "2001:db8:7:1::9".parse().expect("ip");
        assert!(b.admit(&scopes(inside), true, &limits).is_some());
    }

    /// AUD-2R-F5: a trusted proxy's connections are not counted in any client
    /// scope, so a busy proxy does not fill its /48 and lock out direct
    /// clients of that /48.
    #[test]
    fn a_busy_trusted_proxy_does_not_fill_its_prefix() {
        let limits = ServeLimits {
            header_read_timeout: Duration::from_secs(1),
            max_connections: 10_000,
            max_per_client: 2,
        };
        let mut b = Book::default();
        let proxy: IpAddr = "2001:db8:7:1::1".parse().expect("ip");
        for _ in 0..1_000 {
            assert!(b.admit(&scopes(proxy), true, &limits).is_some());
        }
        assert!(b.per_scope.is_empty(), "proxy connections are not counted");
        // A direct client in the same /64 still gets its own share.
        let direct: IpAddr = "2001:db8:7:1::2".parse().expect("ip");
        assert!(b.admit(&scopes(direct), false, &limits).is_some());
        assert!(b.admit(&scopes(direct), false, &limits).is_some());
        assert!(b.admit(&scopes(direct), false, &limits).is_none());
    }
}
