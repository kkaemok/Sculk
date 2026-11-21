//! Explicit TCP routes for native LOCO byte streams. Each route enforces the same
//! destination policy and never falls back to another route.
use crate::connect::Endpoint;
use std::{io, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
};
/// Direct TCP with system DNS and an exact host/port allowlist.
pub struct DirectTcp {
    allowed: Vec<Endpoint>,
    timeout: Duration,
}

impl DirectTcp {
    /// Validate a finite destination policy and a DNS/connect deadline.
    pub fn new(allowed: Vec<Endpoint>, timeout: Duration) -> io::Result<Self> {
        validate_policy(&allowed, timeout)?;
        Ok(Self { allowed, timeout })
    }

    /// Resolve and connect only after checking the requested host and port.
    /// DNS results follow the host resolver's trust policy. Secure protocol
    /// establishment and peer identity verification are performed by the caller.
    pub async fn dial(
        &self,
        endpoint: Endpoint,
        mut cancel: watch::Receiver<bool>,
    ) -> io::Result<TcpStream> {
        if !self.allowed.contains(&endpoint) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let connect = TcpStream::connect((endpoint.host(), endpoint.port()));
        tokio::select! { biased;
            _ = crate::recovery::cancelled(&mut cancel) => Err(io::ErrorKind::Interrupted.into()),
            result = tokio::time::timeout(self.timeout, connect) => {
                result.unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
            }
        }
    }
}

fn validate_policy(allowed: &[Endpoint], timeout: Duration) -> io::Result<()> {
    if allowed.is_empty()
        || allowed.len() > 128
        || timeout.is_zero()
        || timeout > Duration::from_secs(60)
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}

/// Explicitly configured TCP proxy and exact allowed destination pairs.
/// Use the managed TCP forwarding proxy when required by environment policy.
pub struct ConnectProxy {
    host: String,
    port: u16,
    allowed: Vec<Endpoint>,
    timeout: Duration,
}
impl ConnectProxy {
    /// Validate a finite allowlist and timeout. Proxy credentials are not accepted.
    pub fn new(
        host: String,
        port: u16,
        allowed: Vec<Endpoint>,
        timeout: Duration,
    ) -> io::Result<Self> {
        Endpoint::new(&host, port).map_err(|_| io::ErrorKind::InvalidInput)?;
        validate_policy(&allowed, timeout)?;
        Ok(Self {
            host,
            port,
            allowed,
            timeout,
        })
    }
    /// Open only an approved destination through CONNECT. Read the bounded proxy
    /// header without over-reading bytes belonging to the tunneled LOCO stream.
    pub async fn dial(
        &self,
        endpoint: Endpoint,
        mut cancel: watch::Receiver<bool>,
    ) -> io::Result<TcpStream> {
        if !self.allowed.contains(&endpoint) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let operation = async {
            let mut socket = TcpStream::connect((self.host.as_str(), self.port)).await?;
            let host = if endpoint.host().contains(':') {
                format!("[{}]", endpoint.host())
            } else {
                endpoint.host().to_owned()
            };
            let target = format!("{host}:{}", endpoint.port());
            socket
                .write_all(
                    format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes(),
                )
                .await?;
            socket.flush().await?;
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                if header.len() >= 8192 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                header.push(socket.read_u8().await?);
            }
            let first = header
                .split(|b| *b == b'\n')
                .next()
                .ok_or(io::ErrorKind::InvalidData)?;
            let line = std::str::from_utf8(first).map_err(|_| io::ErrorKind::InvalidData)?;
            let mut parts = line.split_whitespace();
            if !matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) || parts.next() != Some("200")
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(socket)
        };
        tokio::select! {biased;
            _=crate::recovery::cancelled(&mut cancel)=>Err(io::ErrorKind::Interrupted.into()),
            result=tokio::time::timeout(self.timeout,operation)=>result.unwrap_or_else(|_|Err(io::ErrorKind::TimedOut.into())),
        }
    }
}
