//! Provider-driven authentication lifecycle over an explicit secure transport.
//!
//! The optional Android account provider is experimental. A caller
//! must explicitly supply an authorized provider; a mock provider proves only
//! lifecycle behavior. Providers must not persist tokens, expose them to logs,
//! clone the transport for other use, or bypass required verification.
use crate::tls::TlsSettings;
use crate::{Client, Config, ConnectionState};
use std::{fmt, future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
    time::Instant,
};
use zeroize::Zeroizing;

/// Credential with redacted Debug, no Clone, bounded size, and zeroizing storage.
/// Provider-created copies and serialized packet buffers need separate handling.
pub struct AccessToken {
    secret: Zeroizing<String>,
    user_id: u64,
    expires: Instant,
}
impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken([REDACTED])")
    }
}
impl AccessToken {
    /// Construct only from legitimately acquired credentials and known expiration.
    /// Device verification and token acquisition are not performed here.
    pub fn new(secret: String, user_id: u64, expires: Instant) -> Result<Self, AuthError> {
        let secret = Zeroizing::new(secret);
        if secret.is_empty() || secret.len() > 65536 || user_id == 0 {
            return Err(AuthError::Credential);
        }
        if expires <= Instant::now() {
            return Err(AuthError::Expired);
        }
        Ok(Self {
            secret,
            user_id,
            expires,
        })
    }
    /// Account identity to which the caller bound this credential.
    pub fn user_id(&self) -> u64 {
        self.user_id
    }
    /// Known expiration on this process's monotonic clock; not a server sync token.
    pub fn expires_at(&self) -> Instant {
        self.expires
    }
    /// Explicit secret access for a trusted provider. Never log or persist it.
    pub fn expose_secret(&self) -> &str {
        &self.secret
    }
}
/// A provider's validated success evidence, not a fabricated local login.
#[derive(Debug)]
pub struct Grant {
    /// Must match the account to which the supplied token is bound.
    pub user_id: u64,
    /// Provider-established lease, capped by the supplied credential expiration.
    pub valid_for: Duration,
}
/// Authentication failure; all are terminal for this session (no automatic retry).
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    /// No verified current account/device-login implementation exists here.
    #[error("current KakaoTalk account/device authentication is unsupported")]
    Unsupported,
    /// Caller supplied invalid credential metadata.
    #[error("invalid credential metadata")]
    Credential,
    /// Provider requires legitimate device/challenge verification.
    #[error("device or security verification is required")]
    VerificationRequired,
    /// Unknown nonzero server status, preserved without inferred meaning.
    #[error("authentication rejected with status {0}")]
    Rejected(i64),
    /// Invalid, contradictory, mismatched or missing response fields.
    #[error("invalid authentication response")]
    InvalidResponse,
    /// Token/session has expired. No implicit refresh occurs.
    #[error("authentication expired")]
    Expired,
    /// Account credential/lease does not match its validated result.
    #[error("invalid authentication grant")]
    InvalidGrant,
    /// Operation was cancelled, or the cancel sender disappeared.
    #[error("authentication cancelled")]
    Cancelled,
    /// Attempt deadline exceeded; outcome may be unknown.
    #[error("authentication timed out")]
    Timeout,
    /// Transport or framing failed; no replay is attempted.
    #[error("authentication connection interrupted")]
    Interrupted,
    /// Session is terminal or authentication is already in progress.
    #[error("authentication requires a fresh secure session")]
    State,
}
/// Latest lifecycle state. It never includes credentials or raw peer documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Secure transport configured; account authentication not yet attempted.
    SecureTransport,
    /// One explicitly supplied provider is running.
    Authenticating,
    /// Provider returned validated success; this is not a live-compatibility claim.
    Authenticated,
    /// Terminal failure; establish a new secure session before another attempt.
    Failed(AuthError),
    /// Graceful local closure.
    Closed,
}
/// Secure transport plus a single-attempt authentication lifecycle.
/// One monitor per session tracks expiry/connection loss; no per-event tasks.
pub struct Session {
    client: Client,
    state: watch::Sender<State>,
    monitor: Option<tokio::task::JoinHandle<()>>,
    deadline: Option<Instant>,
}
struct AttemptGuard {
    client: Client,
    state: watch::Sender<State>,
    armed: bool,
}
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if self.armed {
            self.state.send_replace(State::Failed(AuthError::Cancelled));
            self.client.abort();
        }
    }
}
impl Session {
    /// Establish verified TLS before creating the LOCO driver. Consumes the stream
    /// on failure and never downgrades to GCM or plaintext. No credentials sent.
    pub async fn establish<S>(
        stream: S,
        server_name: &str,
        tls: &TlsSettings,
        handshake_timeout: Duration,
        config: Config,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, EstablishError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let stream = tls
            .handshake(stream, server_name, handshake_timeout, cancel)
            .await?;
        let client = Client::from_stream(stream, config)?;
        Ok(Self::from_secure_client(client))
    }
    // Only callers that have selected an explicit secure transport can enter here.
    pub(crate) fn from_secure_client(client: Client) -> Self {
        let (state, _) = watch::channel(State::SecureTransport);
        Self {
            client,
            state,
            monitor: None,
            deadline: None,
        }
    }
    /// Observe current state without raw diagnostics. Consumers may miss intermediate
    /// states; use this as a latest-state channel rather than an audit log.
    pub fn subscribe(&self) -> watch::Receiver<State> {
        self.state.subscribe()
    }
    /// Current lifecycle state.
    pub fn state(&self) -> State {
        let current = self.state.borrow().clone();
        if current == State::Authenticated {
            let error = if self
                .deadline
                .is_some_and(|deadline| deadline <= Instant::now())
            {
                Some(AuthError::Expired)
            } else if self.client.state() != ConnectionState::Connected {
                Some(AuthError::Interrupted)
            } else {
                None
            };
            if let Some(error) = error {
                self.state.send_replace(State::Failed(error));
                self.client.abort();
            }
        }
        self.state.borrow().clone()
    }
    /// Run exactly one explicit provider, with a deadline, cancellation and token
    /// expiry. Dropping this future closes its connection and marks it cancelled.
    /// Success means only that the supplied provider returned a validated grant.
    pub async fn authenticate_with<F, Fut>(
        &mut self,
        token: AccessToken,
        timeout: Duration,
        mut cancel: watch::Receiver<bool>,
        provider: F,
    ) -> Result<(), AuthError>
    where
        F: FnOnce(Client, AccessToken) -> Fut,
        Fut: Future<Output = Result<Grant, AuthError>>,
    {
        if self.state() != State::SecureTransport {
            return Err(AuthError::State);
        }
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(AuthError::Credential);
        }
        let user_id = token.user_id;
        let expires = token.expires;
        self.state.send_replace(State::Authenticating);
        let mut guard = AttemptGuard {
            client: self.client.clone(),
            state: self.state.clone(),
            armed: true,
        };
        let mut connection = self.client.connection_state();
        let result = tokio::select! { biased;
            _ = crate::recovery::cancelled(&mut cancel) => Err(AuthError::Cancelled),
            _ = tokio::time::sleep_until(expires) => Err(AuthError::Expired),
            _ = connection_ended(&mut connection) => Err(AuthError::Interrupted),
            result = tokio::time::timeout(timeout, async { provider(self.client.clone(), token).await }) => result.unwrap_or(Err(AuthError::Timeout)),
        };
        let result = result.and_then(|grant| {
            if grant.user_id != user_id
                || grant.valid_for.is_zero()
                || grant.valid_for > Duration::from_secs(86400)
            {
                return Err(AuthError::InvalidGrant);
            }
            if expires <= Instant::now() {
                return Err(AuthError::Expired);
            }
            Ok((Instant::now() + grant.valid_for).min(expires))
        });
        guard.armed = false;
        match result {
            Ok(deadline) => {
                self.deadline = Some(deadline);
                self.state.send_replace(State::Authenticated);
                let state = self.state.clone();
                let client = self.client.clone();
                self.monitor = Some(tokio::spawn(async move {
                    let reason = tokio::select! { biased;
                        _ = connection_ended(&mut connection) => AuthError::Interrupted,
                        _ = tokio::time::sleep_until(deadline) => AuthError::Expired,
                    };
                    state.send_if_modified(|state| {
                        if *state == State::Authenticated {
                            *state = State::Failed(reason);
                            true
                        } else {
                            false
                        }
                    });
                    client.abort();
                }));
                Ok(())
            }
            Err(error) => {
                self.state.send_replace(State::Failed(error.clone()));
                self.client.abort();
                Err(error)
            }
        }
    }
    /// Send only while the provider-established session remains authenticated.
    /// No automatic reauthentication or replay of an uncertain request occurs.
    pub async fn request(
        &self,
        packet: sculk_protocol::Packet,
    ) -> Result<sculk_protocol::Packet, RequestError> {
        match self.state() {
            State::Authenticated => {}
            State::Failed(AuthError::Expired) => return Err(RequestError::Expired),
            _ => return Err(RequestError::Unauthenticated),
        }
        let deadline = self.deadline.ok_or(RequestError::Unauthenticated)?;
        match tokio::time::timeout_at(deadline, self.client.request(packet)).await {
            Ok(result) => Ok(result?),
            Err(_) => {
                self.state.send_replace(State::Failed(AuthError::Expired));
                self.client.abort();
                Err(RequestError::Expired)
            }
        }
    }
    /// Subscribe from now to source-backed MSG events while authenticated.
    /// Uses the existing bounded ring; no replay or per-subscriber worker.
    /// Initial LOGINLIST pushes are not replayed by this API.
    #[cfg(feature = "messaging")]
    pub fn message_events(
        &self,
    ) -> Result<crate::messaging::Events, crate::messaging::ReceiveError> {
        if self.state() != State::Authenticated {
            return Err(crate::messaging::ReceiveError::Inactive);
        }
        Ok(crate::messaging::Events::new(
            self.client.subscribe(),
            self.subscribe(),
            self.client.connection_state(),
            self.deadline
                .ok_or(crate::messaging::ReceiveError::Inactive)?,
        ))
    }
    /// Stop expiry monitoring and gracefully close the connection.
    pub async fn disconnect(&mut self) {
        if let Some(task) = self.monitor.take() {
            task.abort();
            let _ = task.await;
        }
        self.client.disconnect().await;
        self.state.send_replace(State::Closed);
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = self.monitor.take() {
            task.abort();
        }
        self.client.abort();
    }
}
async fn connection_ended(connection: &mut watch::Receiver<ConnectionState>) {
    loop {
        if *connection.borrow_and_update() != ConnectionState::Connected
            || connection.changed().await.is_err()
        {
            return;
        }
    }
}
/// Failure before any account authentication can be attempted.
#[derive(Debug, thiserror::Error)]
pub enum EstablishError {
    /// TLS handshake, verification, timeout or cancellation failure.
    #[error(transparent)]
    Tls(#[from] crate::tls::Error),
    /// Invalid LOCO transport configuration.
    #[error(transparent)]
    Transport(#[from] crate::Error),
}
/// Post-authentication request failure.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// Session expired while awaiting a response; delivery may be unknown.
    #[error("session expired; delivery unknown")]
    Expired,
    /// No active provider-established authentication.
    #[error("session is not authenticated")]
    Unauthenticated,
    /// Request failure; delivery may be unknown.
    #[error(transparent)]
    Transport(#[from] crate::Error),
}
/// Parse the researched LOGINLIST acknowledgment subset, not an entire current
/// session contract. Checks command, signed header status, optional BSON status,
/// and account identity. Unknown statuses remain raw; REST auth codes are not
/// interpreted as LOCO codes. Missing body status is allowed by node-kakao's schema.
pub fn parse_loginlist_reply(
    packet: &sculk_protocol::Packet,
    expected_user: u64,
    valid_for: Duration,
) -> Result<Grant, AuthError> {
    if packet.command.as_str() != "LOGINLIST" || expected_user == 0 {
        return Err(AuthError::InvalidResponse);
    }
    if packet.status != 0 {
        return Err(AuthError::Rejected(i64::from(packet.status as i16)));
    }
    let doc = bson::raw::RawDocument::from_bytes(packet.body())
        .map_err(|_| AuthError::InvalidResponse)?;
    let mut user = None;
    let mut status = None;
    for field in doc.iter() {
        let (key, value) = field.map_err(|_| AuthError::InvalidResponse)?;
        let slot = match key {
            "userId" => &mut user,
            "status" => &mut status,
            _ => continue,
        };
        if slot.is_some() {
            return Err(AuthError::InvalidResponse);
        }
        *slot = Some(match value {
            bson::raw::RawBsonRef::Int32(n) => i64::from(n),
            bson::raw::RawBsonRef::Int64(n) => n,
            _ => return Err(AuthError::InvalidResponse),
        });
    }
    if let Some(status) = status {
        if status != 0 {
            return Err(AuthError::Rejected(status));
        }
    }
    let user = user.ok_or(AuthError::InvalidResponse)?;
    if user <= 0 || user as u64 != expected_user {
        return Err(AuthError::InvalidResponse);
    }
    Ok(Grant {
        user_id: user as u64,
        valid_for,
    })
}

impl From<crate::Error> for AuthError {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::Rejected(status) => Self::Rejected(i64::from(status as i16)),
            crate::Error::Mismatch => Self::InvalidResponse,
            crate::Error::Timeout => Self::Timeout,
            crate::Error::AuthenticationUnavailable => Self::Unsupported,
            crate::Error::Config => Self::Credential,
            crate::Error::Closed | crate::Error::Overloaded => Self::Interrupted,
        }
    }
}
