//! A local fixture server and the transport that reaches it (feature `fixture`; tests and the
//! local end-to-end demo only). The fetch under test still runs every rule of
//! [`crate::HttpsFetch`]: the URL check, one resolution through [`FixedResolver`], the
//! public-address check on what it answers, and the status, size, encoding and time rules. Only
//! the last step differs: [`LoopbackDial`] connects to the fixture over plain TCP on the loopback
//! interface instead of TLS to the checked address, and records which addresses it was given.

use crate::{BoxFuture, Dial, HttpsFetch, Io, Limits, Resolve};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A resolver with fixed answers: the n-th call gets `answers[min(n, last)]`. Counts its calls.
#[derive(Debug, Default)]
pub struct FixedResolver {
    answers: Vec<Vec<IpAddr>>,
    calls: AtomicUsize,
}

impl FixedResolver {
    /// One answer for every call.
    pub fn new(addrs: Vec<IpAddr>) -> Self {
        Self::sequence(vec![addrs])
    }

    /// A different answer per call (a rebinding name).
    pub fn sequence(answers: Vec<Vec<IpAddr>>) -> Self {
        FixedResolver {
            answers,
            calls: AtomicUsize::new(0),
        }
    }

    /// How many times it was asked.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Resolve for FixedResolver {
    fn resolve<'a>(&'a self, _host: &'a str) -> BoxFuture<'a, std::io::Result<Vec<IpAddr>>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let a = self
            .answers
            .get(n.min(self.answers.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        Box::pin(async move { Ok(a) })
    }
}

/// Plain TCP to the fixture server, whatever the checked addresses; records them.
#[derive(Debug)]
pub struct LoopbackDial {
    addr: SocketAddr,
    seen: Mutex<Vec<Vec<IpAddr>>>,
}

impl LoopbackDial {
    /// To the fixture at `addr`.
    pub fn new(addr: SocketAddr) -> Self {
        LoopbackDial {
            addr,
            seen: Mutex::new(vec![]),
        }
    }

    /// The addresses each dial was given.
    pub fn seen(&self) -> Vec<Vec<IpAddr>> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Dial for LoopbackDial {
    fn dial<'a>(
        &'a self,
        _host: &'a str,
        addrs: &'a [IpAddr],
    ) -> BoxFuture<'a, std::io::Result<Box<dyn Io>>> {
        if let Ok(mut s) = self.seen.lock() {
            s.push(addrs.to_vec());
        }
        Box::pin(async move {
            let s = tokio::net::TcpStream::connect(self.addr).await?;
            Ok(Box::new(s) as Box<dyn Io>)
        })
    }
}

/// A public address the fixture's names resolve to (TEST: it is never connected to).
pub const FIXTURE_PUBLIC_IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 215, 14));

/// What the fixture answers on one path.
#[derive(Clone, Debug)]
pub struct Reply {
    /// Status code.
    pub status: u16,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// The body (sent `repeat` times).
    pub body: Vec<u8>,
    /// How many times the body is sent (a huge body streamed; stops when the client leaves).
    pub repeat: usize,
    /// Send `Content-Length` (of the whole body) unless a header gives one.
    pub content_length: bool,
    /// Send one byte per this interval, forever (slow loris).
    pub trickle: Option<Duration>,
}

impl Reply {
    /// 200 with a JSON body.
    pub fn json(body: impl Into<Vec<u8>>) -> Self {
        Reply {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into(),
            repeat: 1,
            content_length: true,
            trickle: None,
        }
    }

    /// Another status with an empty body and these headers.
    pub fn status(status: u16, headers: &[(&str, &str)]) -> Self {
        Reply {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: vec![],
            repeat: 1,
            content_length: true,
            trickle: None,
        }
    }
}

/// What the fixture saw: each request's head, and the body bytes it managed to send.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    /// Request heads (request line and headers).
    pub requests: Vec<String>,
    /// Body bytes written per response.
    pub written: Vec<usize>,
}

/// A local HTTP/1.1 fixture server on 127.0.0.1 (one request per connection).
#[derive(Debug)]
pub struct FixtureServer {
    addr: SocketAddr,
    seen: Arc<Mutex<Seen>>,
}

impl FixtureServer {
    /// Serve `routes` (exact path and query; anything else is 404) on a thread of its own.
    pub fn start(routes: Vec<(String, Reply)>) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let seen2 = seen.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        std::thread::spawn(move || {
            rt.block_on(async move {
                if let Ok(l) = tokio::net::TcpListener::from_std(listener) {
                    serve(l, Arc::new(routes), seen2).await;
                }
            });
        });
        Ok(FixtureServer { addr, seen })
    }

    /// Serve `routes` as a task of the current tokio runtime (a test with a paused clock).
    pub async fn spawn(routes: Vec<(String, Reply)>) -> std::io::Result<Self> {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = l.local_addr()?;
        let seen = Arc::new(Mutex::new(Seen::default()));
        tokio::spawn(serve(l, Arc::new(routes), seen.clone()));
        Ok(FixtureServer { addr, seen })
    }

    /// Its address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// What it saw so far.
    pub fn seen(&self) -> Seen {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// A fetch whose names resolve to [`FIXTURE_PUBLIC_IP`] and whose connections reach this
    /// server.
    pub fn fetch(&self, limits: Limits) -> HttpsFetch {
        HttpsFetch::with_transport(
            Arc::new(FixedResolver::new(vec![FIXTURE_PUBLIC_IP])),
            Arc::new(LoopbackDial::new(self.addr)),
            limits,
        )
    }
}

async fn serve(
    l: tokio::net::TcpListener,
    routes: Arc<Vec<(String, Reply)>>,
    seen: Arc<Mutex<Seen>>,
) {
    loop {
        let Ok((sock, _)) = l.accept().await else {
            return;
        };
        tokio::spawn(answer(sock, routes.clone(), seen.clone()));
    }
}

async fn answer(
    mut sock: tokio::net::TcpStream,
    routes: Arc<Vec<(String, Reply)>>,
    seen: Arc<Mutex<Seen>>,
) {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let target = text.split(' ').nth(1).unwrap_or("").to_string();
    if let Ok(mut s) = seen.lock() {
        s.requests.push(text);
    }
    let reply = routes
        .iter()
        .find(|(p, _)| *p == target)
        .map(|(_, r)| r.clone())
        .unwrap_or_else(|| Reply::status(404, &[]));
    let mut out = format!("HTTP/1.1 {} Fixture\r\nconnection: close\r\n", reply.status);
    let has_length = reply
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    if reply.content_length && !has_length && reply.trickle.is_none() {
        out.push_str(&format!(
            "content-length: {}\r\n",
            reply.body.len() * reply.repeat
        ));
    }
    out.push_str("\r\n");
    if sock.write_all(out.as_bytes()).await.is_err() {
        return;
    }
    let mut written = 0usize;
    if let Some(gap) = reply.trickle {
        loop {
            if sock.write_all(b" ").await.is_err() {
                break;
            }
            written += 1;
            tokio::time::sleep(gap).await;
        }
    } else {
        for _ in 0..reply.repeat {
            if sock.write_all(&reply.body).await.is_err() {
                break;
            }
            written += reply.body.len();
        }
        let _ = sock.shutdown().await;
    }
    if let Ok(mut s) = seen.lock() {
        s.written.push(written);
    }
}
