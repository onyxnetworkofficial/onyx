//! `onyx-node healthcheck`: a dependency-free probe for container health
//! checks (the runtime image has no shell or curl). It asks the local server
//! for `GET /v1/health` and succeeds on HTTP 200.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// The address to probe for a server listening on `listen`: an unspecified
/// address (`0.0.0.0`, `::`) is reached on loopback.
pub fn probe_addr(listen: SocketAddr) -> SocketAddr {
    let mut a = listen;
    if a.ip().is_unspecified() {
        a.set_ip(if a.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    a
}

/// `GET /v1/health` on `addr`; `Ok` with the status line on 200.
pub fn check(addr: SocketAddr, timeout: Duration) -> Result<String, String> {
    let mut s =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {addr}: {e}"))?;
    s.set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    s.write_all(
        format!("GET /v1/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    s.take(64 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let status = text.lines().next().unwrap_or("").to_string();
    if status.split_whitespace().nth(1) == Some("200") {
        Ok(status)
    } else {
        Err(format!("unhealthy: {status:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unspecified_listen_addresses_are_probed_on_loopback() {
        let a: SocketAddr = "0.0.0.0:8080".parse().expect("addr");
        assert_eq!(probe_addr(a), "127.0.0.1:8080".parse().expect("addr"));
        let a: SocketAddr = "[::]:8080".parse().expect("addr");
        assert_eq!(probe_addr(a), "[::1]:8080".parse().expect("addr"));
        let a: SocketAddr = "10.0.0.5:9000".parse().expect("addr");
        assert_eq!(probe_addr(a), a);
    }

    #[test]
    fn a_closed_port_is_unhealthy() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let a = l.local_addr().expect("addr");
        drop(l);
        assert!(check(a, Duration::from_millis(300)).is_err());
    }
}
