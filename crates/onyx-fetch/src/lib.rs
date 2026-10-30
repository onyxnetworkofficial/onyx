//! The one HTTPS GET every Onyx pull uses (ADR-0053 item 7, contributor loop CL3): the level-2
//! observers (`onyx_level2::live`), the submission check pull (`onyx_api::submit`) and the
//! source review tool. A template or a DNS answer can never make it call a private address.
//!
//! - **URL** ([`check_target`]): `https://` only, a DNS host name (no IP literal, no user info,
//!   no fragment), port 443 (written or implied), at most [`MAX_URL`] visible ASCII bytes.
//!   [`check_template_url`] adds the manifest rules: no port written, no secret-named query
//!   parameter, host not blocked ([`onyx_intake::is_blocked_host`]).
//! - **Addresses** (THREATS CL3-1): the host is resolved ONCE; every address must be public
//!   ([`onyx_intake::is_public_ip`]: no loopback, private, link-local or cloud metadata
//!   address, no IPv4 hidden in IPv6); the connection goes to exactly those addresses, so a
//!   DNS answer that changes after the check (rebinding) is never looked up again.
//! - **TLS**: webpki roots, the host as server name. No proxy from the environment, no cookies,
//!   `Accept: application/json` (`*/*` for an adapter's plain-text source), a neutral
//!   User-Agent naming no person or account.
//! - **Answer**: 200 only; a redirect is [`FetchError::Redirect`], never followed. A declared
//!   `Content-Length` over the cap is refused before reading; reading stops at the cap
//!   (THREATS CL3-2); `identity` or `gzip` only, and the decoded body is capped too.
//! - **Time**: one deadline for the whole fetch (resolve, connect, TLS, headers, body), so a
//!   server that trickles bytes (slow loris) ends at the deadline.
//!
//! The network parts are traits ([`Resolve`], [`Dial`]) so tests and the local demo run every
//! rule above against a local fixture server (feature `fixture`); [`HttpsFetch::new`] is the
//! only constructor a live binary uses.

use http_body_util::BodyExt as _;
use hyper::body::Bytes;
use std::future::Future;
use std::io::Read as _;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;
mod keyed;
use keyed::KeyHeader;
pub use keyed::{
    check_keyed_template_url, ApiKey, KeyPlacement, KEY_PLACEHOLDER, KEY_WINDOW, MAX_KEY, MIN_KEY,
};

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The User-Agent of an observer's fetch: the project's name, nothing about who runs it.
pub const USER_AGENT: &str = "onyx-observer/1";
/// The User-Agent of a submission check pull.
pub const CHECK_USER_AGENT: &str = "onyx-submit-check/1";
/// Largest body an observer reads (1 MiB, ADR-0053 item 3).
pub const MAX_BYTES_CEILING: usize = 1_048_576;
/// An observer's whole-fetch deadline, in seconds.
pub const OBSERVER_TIMEOUT_SECS: u64 = 20;
/// Longest URL, in bytes.
pub const MAX_URL: usize = 2_048;
/// Each connection attempt's own deadline (inside the whole-fetch one).
const CONNECT_SECS: u64 = 5;

/// Caps of one fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest body, on the wire and after decoding.
    pub max_bytes: usize,
    /// The whole fetch's deadline.
    pub timeout: Duration,
    /// The User-Agent sent.
    pub user_agent: &'static str,
    /// The Accept header sent.
    pub accept: &'static str,
}

impl Limits {
    /// An observer's fetch of a JSON source: `max_bytes` (at most [`MAX_BYTES_CEILING`]),
    /// 20 s, `Accept: application/json`.
    pub fn observer(max_bytes: usize) -> Self {
        Limits {
            max_bytes: max_bytes.min(MAX_BYTES_CEILING),
            timeout: Duration::from_secs(OBSERVER_TIMEOUT_SECS),
            user_agent: USER_AGENT,
            accept: "application/json",
        }
    }

    /// An observer's fetch of an adapter's source (some answer plain text, so `Accept: */*`,
    /// as the earlier `curl` fetch sent): [`MAX_BYTES_CEILING`] and 20 s.
    pub fn observer_any() -> Self {
        Limits {
            accept: "*/*",
            ..Self::observer(MAX_BYTES_CEILING)
        }
    }

    /// The submission check pull: [`onyx_intake::MAX_BODY_BYTES`] and
    /// [`onyx_intake::FETCH_TIMEOUT_SECS`].
    pub fn check_pull() -> Self {
        Limits {
            max_bytes: onyx_intake::MAX_BODY_BYTES,
            timeout: Duration::from_secs(onyx_intake::FETCH_TIMEOUT_SECS),
            user_agent: CHECK_USER_AGENT,
            accept: "application/json",
        }
    }
}

/// Why a fetch gave no body.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The URL breaks a rule (scheme, host, port, user info, fragment, length, secret).
    #[error("refused URL: {0}")]
    BadUrl(String),
    /// The host resolved to an address that is not public, or is blocked.
    #[error("refused: {0}")]
    Refused(String),
    /// The source answered with a redirect; redirects are never followed.
    #[error("the source answered {0} (a redirect); redirects are not followed")]
    Redirect(u16),
    /// Any status but 200.
    #[error("the source answered {0}")]
    Status(u16),
    /// The body is over the cap (declared, on the wire, or decoded).
    #[error("the body is over {0} bytes")]
    TooLarge(usize),
    /// The whole fetch's deadline passed.
    #[error("no answer within {0} s")]
    Timeout(u64),
    /// A content encoding other than identity or gzip, or a broken gzip stream.
    #[error("content encoding refused: {0}")]
    Encoding(String),
    /// Resolution, connection, TLS or protocol failure.
    #[error("unreachable: {0}")]
    Unreachable(String),
}

/// A checked fetch target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The host, lower case.
    pub host: String,
    /// Path and query (at least `/`).
    pub path_and_query: String,
    /// Whether the URL wrote a port (`:443`).
    pub port_written: bool,
}

fn bad(why: &str) -> FetchError {
    FetchError::BadUrl(why.into())
}

/// Whether `host` (lower case) is a DNS name: at least two dot-separated labels of `a-z`,
/// `0-9` and `-` (not at a label's ends), at most 253 bytes, and not an IPv4 literal in any
/// form (the last label is not all digits).
fn is_dns_name(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.parse::<IpAddr>().is_ok() {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
        && !labels
            .last()
            .is_some_and(|l| l.bytes().all(|c| c.is_ascii_digit()))
        // Review CL3-R1-5: `inet_aton` reads `0x7f.0x0.0x0.0x1` as 127.0.0.1; a label that is a
        // hex number is refused like the intake's URL rules refuse it.
        && !labels.iter().any(|l| {
            l.strip_prefix("0x")
                .is_some_and(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
        })
}

/// The target of `url` under the transport's rules (see the module docs).
pub fn check_target(url: &str) -> Result<Target, FetchError> {
    if url.len() > MAX_URL || !url.bytes().all(|c| (0x21..=0x7e).contains(&c)) {
        return Err(bad("the URL must be at most 2048 visible ASCII bytes"));
    }
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| bad("https only"))?;
    if url.contains('#') {
        return Err(bad("a fragment"));
    }
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.contains('@') {
        return Err(bad("user info"));
    }
    if authority.starts_with('[') {
        return Err(bad("an IP address, not a host name"));
    }
    let (host, port_written) = match authority.split_once(':') {
        Some((h, "443")) => (h, true),
        Some(_) => return Err(bad("a port other than 443")),
        None => (authority, false),
    };
    let host = host.to_ascii_lowercase();
    if !is_dns_name(&host) {
        return Err(bad("the host must be a DNS name, not an IP address"));
    }
    let path_and_query = match tail {
        "" => "/".to_string(),
        t if t.starts_with('?') => format!("/{t}"),
        t => t.to_string(),
    };
    Ok(Target {
        host,
        path_and_query,
        port_written,
    })
}

/// The rules for a reviewed template's URL (ADR-0053 item 7), checked with each `{date}`
/// filled: [`check_target`], no port written, no secret-named query parameter, host not
/// blocked.
pub fn check_template_url(url_template: &str) -> Result<Target, FetchError> {
    let url = url_template.replace("{date}", "2000-01-01");
    let t = check_target(&url)?;
    if t.port_written {
        return Err(bad("a template writes no port"));
    }
    if onyx_intake::submission::has_secret_param(&url)
        || url
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or("")
            .split(['&', ';'])
            .any(|p| {
                let name = p.split_once('=').map_or(p, |(n, _)| n);
                onyx_intake::submission::is_secret_name(&onyx_intake::submission::param_name(name))
            })
    {
        return Err(bad("a secret-named query parameter"));
    }
    if onyx_intake::is_blocked_host(&t.host) {
        return Err(FetchError::Refused(format!(
            "{} is a blocked host (its terms forbid redistribution)",
            t.host
        )));
    }
    Ok(t)
}

/// `Ok` when there is at least one address and every one is public.
pub fn public_only(host: &str, addrs: &[IpAddr]) -> Result<(), FetchError> {
    if addrs.is_empty() {
        return Err(FetchError::Refused(format!("{host} has no address")));
    }
    if let Some(bad) = addrs.iter().find(|a| !onyx_intake::is_public_ip(**a)) {
        return Err(FetchError::Refused(format!(
            "{host} resolves to {bad}, which is not a public address"
        )));
    }
    Ok(())
}

/// Resolves a host name.
pub trait Resolve: Send + Sync {
    /// Every address of `host`.
    fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, std::io::Result<Vec<IpAddr>>>;
}

/// The system resolver.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str) -> BoxFuture<'a, std::io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host, 443))
                .await?
                .map(|a| a.ip())
                .collect())
        })
    }
}

/// A byte stream to the source.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Opens the connection to `host` at exactly the checked `addrs`.
pub trait Dial: Send + Sync {
    /// A stream to `host`, connecting only to `addrs`.
    fn dial<'a>(
        &'a self,
        host: &'a str,
        addrs: &'a [IpAddr],
    ) -> BoxFuture<'a, std::io::Result<Box<dyn Io>>>;
}

/// TCP to each checked address on port 443 in turn, then TLS (webpki roots, `host` as the
/// server name).
#[derive(Clone)]
pub struct TlsDial {
    config: Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for TlsDial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsDial")
    }
}

impl TlsDial {
    /// With the webpki roots.
    pub fn new() -> Result<Self, FetchError> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| FetchError::Unreachable(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(TlsDial {
            config: Arc::new(config),
        })
    }
}

impl Dial for TlsDial {
    fn dial<'a>(
        &'a self,
        host: &'a str,
        addrs: &'a [IpAddr],
    ) -> BoxFuture<'a, std::io::Result<Box<dyn Io>>> {
        Box::pin(async move {
            let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "no address");
            for ip in addrs {
                let tcp = tokio::time::timeout(
                    Duration::from_secs(CONNECT_SECS),
                    tokio::net::TcpStream::connect(SocketAddr::new(*ip, 443)),
                )
                .await;
                let tcp = match tcp {
                    Ok(Ok(s)) => s,
                    Ok(Err(e)) => {
                        last = e;
                        continue;
                    }
                    Err(_) => {
                        last = std::io::Error::new(std::io::ErrorKind::TimedOut, "connect");
                        continue;
                    }
                };
                let name = rustls::pki_types::ServerName::try_from(host.to_string())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                // Review CL3-R1-6: a TLS failure on one address tries the next checked one.
                match tokio_rustls::TlsConnector::from(self.config.clone())
                    .connect(name, tcp)
                    .await
                {
                    Ok(tls) => return Ok(Box::new(tls) as Box<dyn Io>),
                    Err(e) => last = e,
                }
            }
            Err(last)
        })
    }
}

/// The SSRF-safe HTTPS GET (see the module docs).
#[derive(Clone)]
pub struct HttpsFetch {
    resolve: Arc<dyn Resolve>,
    dial: Arc<dyn Dial>,
    limits: Limits,
}

impl std::fmt::Debug for HttpsFetch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsFetch")
            .field("limits", &self.limits)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Identity,
    Gzip,
}

impl HttpsFetch {
    /// The live transport: the system resolver, TCP on 443 and TLS with the webpki roots.
    pub fn new(limits: Limits) -> Result<Self, FetchError> {
        Ok(Self::with_transport(
            Arc::new(SystemResolver),
            Arc::new(TlsDial::new()?),
            limits,
        ))
    }

    /// Another resolver and dialer (tests and the local fixture demo). Every rule but the
    /// network itself still applies: the URL check, one resolution, public addresses only,
    /// the status, size, encoding and time rules.
    pub fn with_transport(resolve: Arc<dyn Resolve>, dial: Arc<dyn Dial>, limits: Limits) -> Self {
        HttpsFetch {
            resolve,
            dial,
            limits,
        }
    }

    /// The caps.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// GET `url` once, under one deadline.
    pub async fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let secs = self.limits.timeout.as_secs();
        tokio::time::timeout(self.limits.timeout, self.get_inner(url, None))
            .await
            .map_err(|_| FetchError::Timeout(secs))?
    }

    /// GET a keyed source's `url` once with the holder's own `key` (ADR-0054 item 2, THREATS
    /// CL4-1, CL4-2), under the same rules as [`Self::get`]: the key goes to the URL's own
    /// host only (one resolution, no redirect followed, so it is never sent anywhere else),
    /// in exactly the place `placement` names. A query key fills the URL's one
    /// `name=`[`KEY_PLACEHOLDER`]; a header key goes in one header marked sensitive. The
    /// error text never holds the key (every error is passed through [`ApiKey::redact`]).
    pub async fn get_keyed(
        &self,
        url: &str,
        placement: &KeyPlacement,
        key: &ApiKey,
    ) -> Result<Vec<u8>, FetchError> {
        let secs = self.limits.timeout.as_secs();
        let run = async {
            let (filled, header) = placement.apply(url, key)?;
            self.get_inner(&filled, header).await
        };
        tokio::time::timeout(self.limits.timeout, run)
            .await
            .map_err(|_| FetchError::Timeout(secs))?
            .map_err(|e| key.redact_error(e))
    }

    /// [`Self::get_keyed`] on a runtime of its own.
    pub fn get_keyed_blocking(
        &self,
        url: &str,
        placement: &KeyPlacement,
        key: &ApiKey,
    ) -> Result<Vec<u8>, FetchError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FetchError::Unreachable(e.to_string()))?;
        let out = rt.block_on(self.get_keyed(url, placement, key));
        rt.shutdown_background();
        out
    }

    /// [`Self::get`] on a runtime of its own (callers that are not async: the observer).
    /// The deadline bounds it too: the runtime is shut down without waiting for a resolver
    /// thread still blocked in the system's lookup (review CL3-R1-1).
    pub fn get_blocking(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FetchError::Unreachable(e.to_string()))?;
        let out = rt.block_on(self.get(url));
        rt.shutdown_background();
        out
    }

    async fn get_inner(
        &self,
        url: &str,
        key_header: Option<KeyHeader>,
    ) -> Result<Vec<u8>, FetchError> {
        let t = check_target(url)?;
        let addrs = self
            .resolve
            .resolve(&t.host)
            .await
            .map_err(|e| FetchError::Unreachable(format!("{}: {e}", t.host)))?;
        public_only(&t.host, &addrs)?;
        let io = self
            .dial
            .dial(&t.host, &addrs)
            .await
            .map_err(|e| FetchError::Unreachable(format!("{}: {e}", t.host)))?;
        let unreachable = |e: hyper::Error| FetchError::Unreachable(e.to_string());
        let (mut send, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io))
                .await
                .map_err(unreachable)?;
        let driver = tokio::spawn(conn);
        let out = self.exchange(&t, &mut send, key_header).await;
        driver.abort();
        out
    }

    async fn exchange(
        &self,
        t: &Target,
        send: &mut hyper::client::conn::http1::SendRequest<http_body_util::Empty<Bytes>>,
        key_header: Option<KeyHeader>,
    ) -> Result<Vec<u8>, FetchError> {
        let max = self.limits.max_bytes;
        let mut req = hyper::Request::get(t.path_and_query.as_str())
            .header("host", t.host.as_str())
            .header("user-agent", self.limits.user_agent)
            .header("accept", self.limits.accept)
            .header("accept-encoding", "gzip");
        if let Some((name, value)) = key_header {
            req = req.header(name, value);
        }
        let req = req
            .body(http_body_util::Empty::new())
            .map_err(|e| FetchError::Unreachable(e.to_string()))?;
        let r = send
            .send_request(req)
            .await
            .map_err(|e| FetchError::Unreachable(e.to_string()))?;
        let status = r.status().as_u16();
        if (300..400).contains(&status) {
            return Err(FetchError::Redirect(status));
        }
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        let h = r.headers();
        let lengths: Vec<_> = h.get_all("content-length").iter().collect();
        if lengths.len() > 1 {
            return Err(FetchError::Unreachable("two Content-Length headers".into()));
        }
        if let Some(v) = lengths.first() {
            let n = v
                .to_str()
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| FetchError::Unreachable("bad Content-Length".into()))?;
            if n > max as u64 {
                return Err(FetchError::TooLarge(max));
            }
        }
        let encodings: Vec<_> = h.get_all("content-encoding").iter().collect();
        let encoding = match encodings.as_slice() {
            [] => Encoding::Identity,
            [one] => match one.to_str().map(|s| s.trim().to_ascii_lowercase()) {
                Ok(s) if s == "identity" => Encoding::Identity,
                Ok(s) if s == "gzip" => Encoding::Gzip,
                _ => {
                    return Err(FetchError::Encoding(format!("{one:?}")));
                }
            },
            _ => return Err(FetchError::Encoding("several content encodings".into())),
        };
        let mut body = r.into_body();
        let mut raw = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| FetchError::Unreachable(e.to_string()))?;
            if let Some(chunk) = frame.data_ref() {
                if raw.len() + chunk.len() > max {
                    // Stop reading: the rest is never pulled (THREATS CL3-2).
                    return Err(FetchError::TooLarge(max));
                }
                raw.extend_from_slice(chunk);
            }
        }
        match encoding {
            Encoding::Identity => Ok(raw),
            Encoding::Gzip => gunzip(&raw, max),
        }
    }
}

/// Decode a gzip body, refusing more than `max` decoded bytes.
/// Exactly one gzip member and nothing after it (review CL3-R1-2: a second member or trailing
/// bytes would let a reader that decodes them see another document).
pub fn gunzip(raw: &[u8], max: usize) -> Result<Vec<u8>, FetchError> {
    let mut out = Vec::new();
    let mut d = flate2::bufread::GzDecoder::new(raw);
    (&mut d)
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| FetchError::Encoding(format!("gzip: {e}")))?;
    if out.len() > max {
        return Err(FetchError::TooLarge(max));
    }
    if !d.into_inner().is_empty() {
        return Err(FetchError::Encoding(
            "bytes after the gzip stream (a second member or trailing data)".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        let ok =
            check_target("https://api.fiscaldata.treasury.gov/services/x?a=b&c=d").expect("ok");
        assert_eq!(ok.host, "api.fiscaldata.treasury.gov");
        assert_eq!(ok.path_and_query, "/services/x?a=b&c=d");
        assert_eq!(
            check_target("https://Example.ORG")
                .expect("ok")
                .path_and_query,
            "/"
        );
        assert_eq!(
            check_target("https://example.org?q=1")
                .expect("ok")
                .path_and_query,
            "/?q=1"
        );
        assert!(
            check_target("https://example.org:443/x")
                .expect("ok")
                .port_written
        );
        for refused in [
            "http://example.org/",
            "HTTPS://example.org/",
            "https://example.org:8443/",
            "https://example.org:/",
            "https://user@example.org/",
            "https://user:pw@example.org/",
            "https://example.org/#frag",
            "https://127.0.0.1/",
            "https://2130706433/",
            "https://0x7f.1/",
            "https://0x7f.0x0.0x0.0x1/",
            "https://0x7f000001.example/",
            "https://a.0xA9FEA9FE/",
            "https://[::1]/",
            "https://localhost/",
            "https://example.org./",
            "https://-a.org/",
            "https://exa mple.org/",
            "https://ex_ample.org/",
            "https://example.org/\u{e9}",
            "ftp://example.org/",
            "https:///x",
        ] {
            assert!(check_target(refused).is_err(), "{refused}");
        }
        let long = format!("https://example.org/{}", "a".repeat(MAX_URL));
        assert!(check_target(&long).is_err());
    }

    #[test]
    fn template_url_rules() {
        check_template_url("https://api.fiscaldata.treasury.gov/x?filter=record_date:eq:{date}")
            .expect("ok");
        for refused in [
            "https://example.org:443/x",
            "https://example.org/x?api_key=abc",
            "https://example.org/x?token=abc",
            "https://example.org/x?A%50PID=abc",
            "https://example.org/x?api_key=",
            "https://fred.stlouisfed.org/x",
        ] {
            assert!(check_template_url(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn gzip_is_capped_after_decoding() {
        use std::io::Write as _;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&vec![b'0'; 4 * MAX_BYTES_CEILING])
            .expect("gzip");
        let bomb = enc.finish().expect("gzip");
        assert!(bomb.len() < 64 * 1024);
        assert_eq!(
            gunzip(&bomb, MAX_BYTES_CEILING),
            Err(FetchError::TooLarge(MAX_BYTES_CEILING))
        );
        assert!(matches!(
            gunzip(b"not gzip", 100),
            Err(FetchError::Encoding(_))
        ));
        // Review CL3-R1-2: one member only, nothing after it.
        let gz = |b: &[u8]| {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(b).expect("gzip");
            e.finish().expect("gzip")
        };
        let one = gz(br#"{"v":"1"}"#);
        assert_eq!(gunzip(&one, 100).expect("one member"), br#"{"v":"1"}"#);
        let two = [one.clone(), gz(br#"{"v":"999"}"#)].concat();
        assert!(matches!(gunzip(&two, 100), Err(FetchError::Encoding(_))));
        let junk = [one, b"<html>junk".to_vec()].concat();
        assert!(matches!(gunzip(&junk, 100), Err(FetchError::Encoding(_))));
    }
}
