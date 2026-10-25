//! Certificate- and hostname-validating TLS over a caller-provided connection.
//! No direct sockets, proxy bypasses, hostname substitutions or insecure fallback.
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
};
use tokio_rustls::{
    client::TlsStream,
    rustls::{self, pki_types::ServerName, RootCertStore},
    TlsConnector,
};

/// TLS establishment errors. Peer-supplied error text is not exposed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Hostname must be a valid DNS name or IP address.
    #[error("invalid TLS server name")]
    ServerName,
    /// Zero or unreasonably large handshake timeout.
    #[error("invalid TLS handshake timeout")]
    TimeoutConfig,
    /// Cancellation was signaled, or its sender was dropped.
    #[error("TLS handshake cancelled")]
    Cancelled,
    /// Peer did not complete its handshake before the deadline.
    #[error("TLS handshake timed out")]
    Timeout,
    /// Certificate/hostname verification, handshake, or underlying I/O failed.
    #[error("TLS handshake or peer verification failed")]
    Handshake,
}
/// Explicit trust roots; custom roots never disable peer verification.
#[derive(Clone)]
pub struct TlsSettings {
    connector: TlsConnector,
}
impl Default for TlsSettings {
    fn default() -> Self {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Self::with_roots(roots)
    }
}
impl TlsSettings {
    /// Use explicit roots (e.g. a private test CA). Normal path/name/time/signature
    /// validation remains enabled. The caller is responsible for trust provenance.
    pub fn with_roots(roots: RootCertStore) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports TLS 1.2/1.3")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self {
            connector: TlsConnector::from(Arc::new(config)),
        }
    }
    /// Perform TLS on an existing direct or proxy-authorized byte stream. No
    /// authentication material is sent by this operation. Dropping the future
    /// closes the consumed stream; a failed stream must not be reused.
    pub async fn handshake<S>(
        &self,
        stream: S,
        server_name: &str,
        timeout: Duration,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<TlsStream<S>, Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(Error::TimeoutConfig);
        }
        let name = ServerName::try_from(server_name.to_owned()).map_err(|_| Error::ServerName)?;
        tokio::select! { biased;
            _ = crate::recovery::cancelled(&mut cancel) => Err(Error::Cancelled),
            result = tokio::time::timeout(timeout, self.connector.connect(name, stream)) =>
                result.map_err(|_| Error::Timeout)?.map_err(|_| Error::Handshake),
        }
    }
}
/// Re-export trust-store types so callers need no separate rustls dependency.
pub use tokio_rustls::rustls::{pki_types::CertificateDer, RootCertStore as TrustStore};
