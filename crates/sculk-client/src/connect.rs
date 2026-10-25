//! Source-backed initial connection sequence. Live compatibility is unverified.
//! No account login, device registration, token refresh or automatic retry is
//! performed. The caller supplies already-authorized credentials, matching
//! profile, trusted keys, explicit transports and a policy-enforcing dialer.
use crate::{
    records::SecureStream,
    session::{parse_loginlist_reply, AccessToken, AuthError, Session},
    tls::TlsSettings,
    Client, Config,
};
use bson::{doc, Bson, Document};
use sculk_protocol::{handshake::ServerKey, Command, Packet};
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
    time::Instant,
};

/// Connection phase, without endpoint or credential data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// TLS Booking / GETCONF.
    Booking,
    /// CHECKIN server assignment.
    Checkin,
    /// LOGINLIST on assigned server.
    Login,
}
/// No peer-supplied text or raw response is included in errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid caller configuration.
    #[error("invalid connection configuration")]
    Config,
    /// Dialer refused or failed a connection; no direct fallback is attempted.
    #[error("connection failed at {0:?}")]
    Dial(Stage),
    /// Request/frame failure.
    #[error("transport failed at {0:?}: {1}")]
    Transport(Stage, crate::Error),
    /// Invalid or ambiguous discovery/assignment response.
    #[error("invalid response at {0:?}")]
    Response(Stage),
    /// Peer rejected a request. Numeric status remains uninterpreted.
    #[error("request rejected at {0:?}: {1}")]
    Rejected(Stage, i64),
    /// Standard TLS validation/handshake failure.
    #[error(transparent)]
    Tls(#[from] crate::tls::Error),
    /// Key exchange or cryptographic operation failed.
    #[error("LOCO key exchange failed")]
    Crypto,
    /// Explicit cancellation (or dropped cancellation sender).
    #[error("connection sequence cancelled")]
    Cancelled,
    /// Stage or overall deadline expired.
    #[error("connection sequence timed out")]
    Timeout,
    /// Credential expired before completion.
    #[error("credential expired")]
    Expired,
    /// Account/session authorization failed.
    #[error(transparent)]
    Auth(#[from] AuthError),
}
/// Validated DNS/IP endpoint. Dialer must additionally enforce its own allowlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    host: String,
    port: u16,
}
impl Endpoint {
    /// Reject schemes, paths, whitespace, empty labels and invalid port values.
    pub fn new(host: &str, port: u16) -> Result<Self, Error> {
        let ip = host.parse::<std::net::IpAddr>().is_ok();
        let dns = host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if port == 0 || host.is_empty() || !(ip || dns) {
            return Err(Error::Config);
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }
    /// DNS name or IP; never includes a URL scheme or credentials.
    pub fn host(&self) -> &str {
        &self.host
    }
    /// Valid nonzero TCP port.
    pub fn port(&self) -> u16 {
        self.port
    }
}
/// Explicit protocol selection; no automatic fallback or server-directed downgrade.
#[derive(Clone)]
pub enum Protection {
    /// TLS validates the exact endpoint hostname.
    Tls,
    /// Documented type-16/type-3 key exchange using a caller-trusted pin.
    LocoGcm(ServerKey),
}
/// Select the GETCONF schema without silently mixing client profiles.
#[derive(Clone, Copy)]
pub enum BookingSchema {
    /// Agent Messenger: os + empty model.
    AgentMessenger,
    /// OpenKakao research: os, model, MCCMNC and userId.
    OpenKakao,
}
/// Caller-supplied device metadata must match its legitimate authentication provider.
/// No Android/device identity or version is fabricated by Sculk.
pub struct DeviceProfile {
    /// Actual protocol OS profile.
    pub os: String,
    /// Actual application version for that profile.
    pub app_version: String,
    /// Version string; references disagree across client profiles.
    pub protocol_version: String,
    /// Carrier/network identifier.
    pub mccmnc: String,
    /// Language code.
    pub language: String,
    /// Country ISO code used by CHECKIN.
    pub country_iso: String,
    /// Explicit sub-device setting; references differ for PC/tablet.
    pub use_sub: bool,
}
impl DeviceProfile {
    fn validate(&self) -> Result<(), Error> {
        for s in [
            &self.os,
            &self.app_version,
            &self.protocol_version,
            &self.mccmnc,
            &self.language,
            &self.country_iso,
        ] {
            if s.is_empty() || s.len() > 128 || s.bytes().any(|b| b.is_ascii_control()) {
                return Err(Error::Config);
            }
        }
        Ok(())
    }
    /// Serialize only the selected researched GETCONF shape.
    pub fn booking_request(&self, schema: BookingSchema, user_id: u64) -> Result<Packet, Error> {
        self.validate()?;
        let mut body = doc! { "os": &self.os, "model": "" };
        if matches!(schema, BookingSchema::OpenKakao) {
            body.insert("MCCMNC", &self.mccmnc);
            body.insert("userId", account(user_id)?);
        }
        request("GETCONF", &body)
    }
    /// Common CHECKIN fields, preserving account IDs as BSON Int64.
    pub fn checkin_request(&self, user_id: u64) -> Result<Packet, Error> {
        self.validate()?;
        request(
            "CHECKIN",
            &doc! { "userId": account(user_id)?, "os": &self.os, "ntype": 0_i32,
            "appVer": &self.app_version, "MCCMNC": &self.mccmnc, "lang": &self.language,
            "countryISO": &self.country_iso, "useSub": self.use_sub },
        )
    }
    fn validate_login(&self, token: &AccessToken, device_uuid: &str) -> Result<(), Error> {
        self.validate()?;
        account(token.user_id())?;
        if device_uuid.is_empty()
            || device_uuid.len() > 256
            || device_uuid.chars().any(char::is_control)
        {
            return Err(Error::Config);
        }
        if token.expires_at() <= Instant::now() {
            return Err(Error::Expired);
        }
        Ok(())
    }
    /// Initial LOGINLIST schema shared by the inspected Agent Messenger/OpenKakao
    /// builders. No resume/replay promise or inferred token expiration is made.
    pub fn login_request(&self, token: &AccessToken, device_uuid: &str) -> Result<Packet, Error> {
        self.validate_login(token, device_uuid)?;
        request(
            "LOGINLIST",
            &doc! { "appVer": &self.app_version, "prtVer": &self.protocol_version,
            "os": &self.os, "lang": &self.language, "dtype": 2_i32, "duuid": device_uuid,
            "oauthToken": token.expose_secret(), "ntype": 0_i32, "MCCMNC": &self.mccmnc,
            "revision": 0_i32, "chatIds": [], "maxIds": [], "lastTokenId": 0_i64, "lbk": 0_i32,
            "rp": Bson::Binary(bson::Binary { subtype: bson::spec::BinarySubtype::Generic, bytes: vec![0,0,255,255,0,0] }), "bg": false },
        )
    }
}
fn account(id: u64) -> Result<i64, Error> {
    i64::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(Error::Config)
}
fn request(command: &str, body: &Document) -> Result<Packet, Error> {
    Packet::from_document(0, Command::new(command).expect("constant command"), body)
        .map_err(|_| Error::Config)
}
fn unique(doc: &bson::raw::RawDocument, fields: &[&str], stage: Stage) -> Result<(), Error> {
    let mut seen = 0u64;
    for field in doc.iter() {
        let (key, _) = field.map_err(|_| Error::Response(stage))?;
        if let Some(index) = fields.iter().position(|name| *name == key) {
            let bit = 1u64 << index;
            if seen & bit != 0 {
                return Err(Error::Response(stage));
            }
            seen |= bit;
        }
    }
    Ok(())
}
fn response(packet: &Packet, command: &str, stage: Stage) -> Result<Document, Error> {
    if packet.command.as_str() != command {
        return Err(Error::Response(stage));
    }
    if packet.status != 0 {
        return Err(Error::Rejected(stage, i64::from(packet.status as i16)));
    }
    let raw =
        bson::raw::RawDocument::from_bytes(packet.body()).map_err(|_| Error::Response(stage))?;
    unique(
        raw,
        &["status", "hosts", "wifi", "ticket", "host", "port"],
        stage,
    )?;
    for key in ["wifi", "ticket"] {
        if let Ok(Some(bson::raw::RawBsonRef::Document(doc))) = raw.get(key) {
            unique(doc, &["ports", "lsl"], stage)?;
        }
    }
    let doc = packet.document().map_err(|_| Error::Response(stage))?;
    if let Some(status) = doc.get("status") {
        let code = integer(status).ok_or(Error::Response(stage))?;
        if code != 0 {
            return Err(Error::Rejected(stage, code));
        }
    }
    Ok(doc)
}
fn integer(value: &Bson) -> Option<i64> {
    match value {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        _ => None,
    }
}
/// Bounded, parsed Booking server candidates. No hardcoded fallback host/port.
#[derive(Debug)]
pub struct Booking {
    hosts: Vec<String>,
    ports: Vec<u16>,
}
impl Booking {
    /// Accept Agent's `hosts` or OpenKakao's `ticket.lsl` shape. Ambiguous host
    /// lists, missing fields, bad entries and >32 candidates are rejected.
    pub fn parse(packet: &Packet) -> Result<Self, Error> {
        let stage = Stage::Booking;
        let doc = response(packet, "GETCONF", stage)?;
        let primary = doc.get("hosts");
        let ticket = doc.get_document("ticket").ok().and_then(|d| d.get("lsl"));
        if primary.is_some() && ticket.is_some() {
            return Err(Error::Response(stage));
        }
        let list = primary
            .or(ticket)
            .and_then(Bson::as_array)
            .ok_or(Error::Response(stage))?;
        if list.is_empty() || list.len() > 32 {
            return Err(Error::Response(stage));
        }
        let ports = doc
            .get_document("wifi")
            .ok()
            .and_then(|d| d.get_array("ports").ok())
            .ok_or(Error::Response(stage))?;
        if ports.is_empty() || ports.len() > 32 {
            return Err(Error::Response(stage));
        }
        let ports: Vec<u16> = ports
            .iter()
            .map(|v| {
                integer(v)
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|p| *p > 0)
                    .ok_or(Error::Response(stage))
            })
            .collect::<Result<_, Error>>()?;
        let hosts = list
            .iter()
            .map(|v| {
                let host = v.as_str().ok_or(Error::Response(stage))?;
                Endpoint::new(host, ports[0]).map_err(|_| Error::Response(stage))?;
                Ok(host.to_owned())
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self { hosts, ports })
    }
    /// Select the first advertised host/port deterministically, as the reference
    /// implementations do. Dialer authorization is still mandatory.
    pub fn selected(&self) -> Endpoint {
        Endpoint {
            host: self.hosts[0].clone(),
            port: self.ports[0],
        }
    }
}
/// Validated CHECKIN server assignment.
pub fn checkin_assignment(packet: &Packet) -> Result<Endpoint, Error> {
    let stage = Stage::Checkin;
    let doc = response(packet, "CHECKIN", stage)?;
    let host = doc.get_str("host").map_err(|_| Error::Response(stage))?;
    let port = doc
        .get("port")
        .and_then(integer)
        .and_then(|p| u16::try_from(p).ok())
        .ok_or(Error::Response(stage))?;
    Endpoint::new(host, port).map_err(|_| Error::Response(stage))
}
/// Explicit connection policy. All timeouts are bounded to five minutes.
pub struct Options {
    /// Booking always uses verified TLS to this endpoint's exact name.
    pub booking: Endpoint,
    /// GETCONF wire shape to send.
    pub booking_schema: BookingSchema,
    /// Protection on the advertised Checkin endpoint.
    pub checkin: Protection,
    /// Protection on the assigned chat endpoint.
    pub chat: Protection,
    /// TLS trust configuration.
    pub tls: TlsSettings,
    /// Existing actor capacity and request timing configuration.
    pub transport: Config,
    /// Dial/handshake/request timeout per stage.
    pub stage_timeout: Duration,
    /// Overall sequence deadline.
    pub total_timeout: Duration,
    /// Local session lease, not a server-reported TTL (capped by token expiry).
    pub session_lease: Duration,
}
struct OwnedClient(Client, bool);
impl Drop for OwnedClient {
    fn drop(&mut self) {
        if self.1 {
            self.0.abort();
        }
    }
}
impl Options {
    /// Run TLS GETCONF → explicit secure CHECKIN → explicit secure LOGINLIST.
    /// The dialer receives each endpoint and must enforce proxy/network/endpoint
    /// authorization. No socket is opened by Sculk and no failure is retried.
    pub async fn connect<S, F, Fut>(
        &self,
        profile: &DeviceProfile,
        token: AccessToken,
        device_uuid: &str,
        mut cancel: watch::Receiver<bool>,
        mut dial: F,
    ) -> Result<Session, Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnMut(Stage, Endpoint) -> Fut,
        Fut: Future<Output = Result<S, std::io::Error>>,
    {
        profile.validate()?;
        for duration in [self.stage_timeout, self.total_timeout] {
            if duration.is_zero() || duration > Duration::from_secs(300) {
                return Err(Error::Config);
            }
        }
        if self.session_lease.is_zero() || self.session_lease > Duration::from_secs(86400) {
            return Err(Error::Config);
        }
        // Preflight all caller-controlled request fields before any connection.
        profile.validate_login(&token, device_uuid)?;
        let expires = token.expires_at();
        let cancellation = cancel.clone();
        let workflow = async {
            let booking = self
                .open(
                    Stage::Booking,
                    self.booking.clone(),
                    &Protection::Tls,
                    &mut dial,
                    cancellation.clone(),
                )
                .await?;
            let packet = self
                .exchange(
                    Stage::Booking,
                    &booking.0,
                    profile.booking_request(self.booking_schema, token.user_id())?,
                )
                .await?;
            let endpoint = Booking::parse(&packet)?.selected();
            booking.0.disconnect().await;
            drop(booking);
            let checkin = self
                .open(
                    Stage::Checkin,
                    endpoint,
                    &self.checkin,
                    &mut dial,
                    cancellation.clone(),
                )
                .await?;
            let packet = self
                .exchange(
                    Stage::Checkin,
                    &checkin.0,
                    profile.checkin_request(token.user_id())?,
                )
                .await?;
            let endpoint = checkin_assignment(&packet)?;
            checkin.0.disconnect().await;
            drop(checkin);
            let mut chat = self
                .open(
                    Stage::Login,
                    endpoint,
                    &self.chat,
                    &mut dial,
                    cancellation.clone(),
                )
                .await?;
            let mut session = Session::from_secure_client(chat.0.clone());
            let packet = profile.login_request(&token, device_uuid)?;
            let user = token.user_id();
            let lease = self.session_lease;
            session
                .authenticate_with(
                    token,
                    self.stage_timeout,
                    cancellation,
                    move |client, _| async move {
                        let reply = client.request(packet).await.map_err(AuthError::from)?;
                        parse_loginlist_reply(&reply, user, lease)
                    },
                )
                .await?;
            // Transfer the actor lifetime to Session rather than the failure guard.
            chat.1 = false;
            drop(chat);
            Ok(session)
        };
        tokio::select! { biased;
            _=crate::recovery::cancelled(&mut cancel)=>Err(Error::Cancelled),
            _=tokio::time::sleep_until(expires)=>Err(Error::Expired),
            result=tokio::time::timeout(self.total_timeout,workflow)=>result.map_err(|_|Error::Timeout)?,
        }
    }
    async fn exchange(
        &self,
        stage: Stage,
        client: &Client,
        packet: Packet,
    ) -> Result<Packet, Error> {
        tokio::time::timeout(self.stage_timeout, client.request(packet))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|error| match error {
                crate::Error::Rejected(status) => Error::Rejected(stage, i64::from(status as i16)),
                other => Error::Transport(stage, other),
            })
    }
    async fn open<S, F, Fut>(
        &self,
        stage: Stage,
        endpoint: Endpoint,
        protection: &Protection,
        dial: &mut F,
        cancel: watch::Receiver<bool>,
    ) -> Result<OwnedClient, Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnMut(Stage, Endpoint) -> Fut,
        Fut: Future<Output = Result<S, std::io::Error>>,
    {
        tokio::time::timeout(self.stage_timeout, async {
            let stream = dial(stage, endpoint.clone())
                .await
                .map_err(|_| Error::Dial(stage))?;
            let client = match protection {
                Protection::Tls => {
                    let stream = self
                        .tls
                        .handshake(stream, endpoint.host(), self.stage_timeout, cancel)
                        .await?;
                    Client::from_stream(stream, self.transport.clone())
                }
                Protection::LocoGcm(key) => {
                    let stream =
                        SecureStream::establish(stream, key, self.stage_timeout, cancel).await?;
                    Client::from_stream(stream, self.transport.clone())
                }
            }
            .map_err(|error| Error::Transport(stage, error))?;
            Ok(OwnedClient(client, true))
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}
