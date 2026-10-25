//! Experimental concrete Android sub-device provider, based on the pinned MIT
//! Agent Messenger implementation. No live compatibility is implied. Registration
//! requires an explicit approval call; phone verification is never bypassed.
use crate::{
    connect::DeviceProfile,
    session::{AccessToken, AuthError},
};
use rand::RngCore;
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client, RequestBuilder,
};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
use tokio::{sync::watch, time::Instant};
use zeroize::Zeroizing;

const MAX_RESPONSE: usize = 65536;
/// Provider failure. No raw HTTP body, URL, password, device ID or token retained.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Invalid profile, account binding, inputs or time limits.
    #[error("invalid authentication configuration")]
    Config,
    /// An operation is not permitted in the current state.
    #[error("invalid authentication state")]
    State,
    /// A separate explicit human approval is required before registration.
    #[error("device registration requires explicit user approval")]
    ApprovalRequired,
    /// Account password was rejected; never retried automatically.
    #[error("invalid credentials (status {0})")]
    InvalidCredentials(i64),
    /// Server rejected the operation; numeric status is preserved.
    #[error("authentication rejected (status {0})")]
    Rejected(i64),
    /// Returned account does not match the required account.
    #[error("authentication account mismatch")]
    AccountMismatch,
    /// Malformed, duplicated, excessive or unexpected response fields.
    #[error("invalid authentication response")]
    Response,
    /// Non-success HTTP status.
    #[error("authentication HTTP status {0}")]
    Http(u16),
    /// Transport/TLS failure, without a secret-bearing source chain.
    #[error("authentication transport failed")]
    Transport,
    /// Operation or total approval window expired.
    #[error("authentication deadline expired")]
    Timeout,
    /// Cancellation was signaled or the sender was dropped.
    #[error("authentication cancelled")]
    Cancelled,
    /// Known credential/local lease expired.
    #[error("credential expired")]
    Expired,
    /// The account supplied no refresh token.
    #[error("refresh credential unavailable")]
    RefreshUnavailable,
    /// Device identity file is unsafe, invalid or inaccessible.
    #[error("private storage unavailable or unsafe")]
    Storage,
}
/// Observable provider state, separate from LOCO session authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Initial login may be attempted once.
    Ready,
    /// One account request is running.
    Authenticating,
    /// Login reported an unregistered device; no registration request was sent.
    NeedsApproval,
    /// The caller recorded explicit user approval; challenge generation allowed.
    Approved,
    /// Challenge generation is in progress.
    RequestingChallenge,
    /// User must confirm on the legitimate device; bounded polling is permitted.
    AwaitingDeviceConfirmation,
    /// A registration status poll is in progress.
    Registering,
    /// Server confirmed registration; one additional login is allowed.
    Registered,
    /// HTTP provider issued account-bound credentials, not a LOCO login claim.
    CredentialsReady,
    /// One explicit refresh is running.
    Refreshing,
    /// Credential/local lease has expired; an explicit refresh may be attempted.
    Expired,
    /// Terminal failure: no automatic retry.
    Failed(Error),
}
/// Stable device identity. Debug is redacted and generation uses OS randomness.
pub struct DeviceIdentity(Zeroizing<String>);
impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceIdentity([REDACTED])")
    }
}
impl DeviceIdentity {
    /// Load an existing private identity, or exclusively create a fresh 0600 file.
    /// Parent must already be a private real directory. Startup-only blocking I/O:
    /// call outside Tokio or in spawn_blocking. Never replaces an existing identity.
    pub fn load_or_create(path: &Path) -> Result<Self, Error> {
        private_parent(path)?;
        match read_private_file(path, 256) {
            Ok(bytes) => {
                Self::from_uuid(String::from_utf8(bytes.to_vec()).map_err(|_| Error::Storage)?)
            }
            Err(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    let mut options = fs::OpenOptions::new();
                    options
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .custom_flags(libc::O_NOFOLLOW);
                    match options.open(path) {
                        Ok(mut file) => {
                            let mut bytes = [0; 32];
                            rand::rngs::OsRng
                                .try_fill_bytes(&mut bytes)
                                .map_err(|_| Error::Storage)?;
                            let uuid = hex(&bytes);
                            file.write_all(uuid.as_bytes())
                                .map_err(|_| Error::Storage)?;
                            file.sync_all().map_err(|_| Error::Storage)?;
                            Self::from_uuid(uuid)
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            let bytes = read_private_file(path, 256)?;
                            Self::from_uuid(
                                String::from_utf8(bytes.to_vec()).map_err(|_| Error::Storage)?,
                            )
                        }
                        Err(_) => Err(Error::Storage),
                    }
                }
                #[cfg(not(unix))]
                {
                    Err(Error::Storage)
                }
            }
        }
    }
    /// Use an identity legitimately associated with the approved account/device.
    pub fn from_uuid(uuid: String) -> Result<Self, Error> {
        let uuid = Zeroizing::new(uuid);
        if uuid.is_empty() || uuid.len() > 256 || uuid.chars().any(char::is_control) {
            return Err(Error::Config);
        }
        Ok(Self(uuid))
    }
    /// Explicit access for the matching LOCO connection; do not log or persist copies.
    pub fn expose_uuid(&self) -> &str {
        &self.0
    }
}
/// Read a bounded 0600-style file without following its final symlink (Unix).
/// Startup-only blocking I/O. Returned bytes are zeroized on drop.
pub fn read_private_file(path: &Path, max: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
    if max == 0 || max > 1024 * 1024 {
        return Err(Error::Config);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| Error::Storage)?;
        let meta = file.metadata().map_err(|_| Error::Storage)?;
        if !meta.is_file()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
            || meta.len() > max as u64
        {
            return Err(Error::Storage);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Storage)?;
        if bytes.len() > max {
            return Err(Error::Storage);
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, max);
        Err(Error::Storage)
    }
}
fn private_parent(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let parent = path.parent().ok_or(Error::Storage)?;
        let meta = fs::symlink_metadata(parent).map_err(|_| Error::Storage)?;
        if meta.uid() != rustix::process::geteuid().as_raw()
            || !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(Error::Storage);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(Error::Storage)
    }
}
/// Explicit source-backed Android client profile; no hidden version switching.
#[derive(Clone)]
pub struct AndroidProfile {
    /// Application version used for both HTTP and LOCO.
    pub app_version: String,
    /// Android OS version in User-Agent.
    pub os_version: String,
    /// API level supplied during legitimate device approval.
    pub api_level: String,
    /// User-visible device name/model for the approved sub-device.
    pub device_name: String,
}
impl AndroidProfile {
    /// Documented Agent Messenger profile. Current server acceptance is unverified.
    pub fn documented() -> Self {
        Self {
            app_version: "25.9.2".into(),
            os_version: "13".into(),
            api_level: "33".into(),
            device_name: "SM-T870".into(),
        }
    }
    /// Validate profile bounds before constructing requests or prompting for credentials.
    pub fn validate(&self) -> Result<(), Error> {
        for s in [
            &self.app_version,
            &self.os_version,
            &self.api_level,
            &self.device_name,
        ] {
            if s.is_empty() || s.len() > 128 || s.chars().any(char::is_control) {
                return Err(Error::Config);
            }
        }
        Ok(())
    }
    /// Matching LOCO metadata; prevents accidental mixing with a PC profile.
    pub fn loco_profile(&self) -> DeviceProfile {
        DeviceProfile {
            os: "android".into(),
            app_version: self.app_version.clone(),
            protocol_version: "1".into(),
            mccmnc: "99999".into(),
            language: "ko".into(),
            country_iso: "KR".into(),
            use_sub: true,
        }
    }
    fn agent(&self) -> String {
        format!("android/{}/ko", self.app_version)
    }
    fn user_agent(&self) -> String {
        format!("KT/{} An/{} ko", self.app_version, self.os_version)
    }
}
/// Provider limits. Token age is an explicit local cap, not a fabricated server TTL.
#[derive(Clone)]
pub struct Limits {
    /// Each HTTP operation including response-body consumption (max 60 seconds).
    pub request_timeout: Duration,
    /// Overall approval window (max five minutes).
    pub approval_timeout: Duration,
    /// Maximum local credential age, capped by expires_in when supplied.
    pub token_age: Duration,
    /// Registration polls, maximum ten. Rejected registration is never retried.
    pub registration_polls: u8,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(15),
            approval_timeout: Duration::from_secs(120),
            token_age: Duration::from_secs(900),
            registration_polls: 10,
        }
    }
}
/// In-memory credentials. Tokens/passwords are never persisted by this provider.
pub struct Credentials {
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    user: u64,
    expires: Instant,
    server_expiry: bool,
    device: Zeroizing<String>,
}
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}
impl Credentials {
    /// Construct an account-bound LOCO token, with the known/local expiration cap.
    pub fn protocol_token(&self) -> Result<AccessToken, Error> {
        AccessToken::new(self.access.to_string(), self.user, self.expires).map_err(|e| {
            if matches!(e, AuthError::Expired) {
                Error::Expired
            } else {
                Error::Config
            }
        })
    }
    /// Exact account ID, preserved beyond JavaScript's safe integer range.
    pub fn user_id(&self) -> u64 {
        self.user
    }
    /// True only when the provider supplied expires_in, otherwise a local age cap.
    pub fn has_server_expiration(&self) -> bool {
        self.server_expiry
    }
    /// Known/local credential expiration.
    pub fn expires_at(&self) -> Instant {
        self.expires
    }
}
/// Challenge code for deliberate display to the approving human; Debug is redacted.
pub struct Challenge(Zeroizing<String>);
impl fmt::Debug for Challenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Challenge([REDACTED])")
    }
}
impl Challenge {
    /// Display only to the approving user; do not log raw HTTP responses.
    pub fn expose_code(&self) -> &str {
        &self.0
    }
}
#[derive(Deserialize)]
struct Reply {
    status: Option<i64>,
    #[serde(rename = "userId")]
    user: Option<u64>,
    access_token: Option<Zeroizing<String>>,
    refresh_token: Option<Zeroizing<String>>,
    expires_in: Option<u64>,
    passcode: Option<Zeroizing<String>>,
    #[serde(rename = "remainingSeconds")]
    remaining: Option<u64>,
    #[serde(rename = "nextRequestIntervalInSeconds")]
    interval: Option<u64>,
}
struct OperationGuard {
    state: watch::Sender<State>,
    armed: bool,
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.state.send_replace(State::Failed(Error::Cancelled));
        }
    }
}
/// Concrete HTTP provider. Fixed HTTPS origin, no redirects or TLS bypasses.
/// At most two login requests: initial, then one after approved registration;
/// at most one explicit refresh. No forced login or slot replacement.
pub struct Provider {
    client: Client,
    base: String,
    profile: AndroidProfile,
    identity: DeviceIdentity,
    expected: u64,
    limits: Limits,
    state: watch::Sender<State>,
    bound_email: Option<Zeroizing<String>>,
    challenge_deadline: Option<Instant>,
    logins: u8,
    refreshed: bool,
    credential_deadline: Option<Instant>,
}
impl Provider {
    /// Use native TLS roots (including supported system CA configuration) and
    /// inherited HTTP(S) proxy routing. Production origin is not caller-overridable.
    pub fn new(
        profile: AndroidProfile,
        identity: DeviceIdentity,
        expected_user: u64,
        limits: Limits,
    ) -> Result<Self, Error> {
        profile.validate()?;
        if expected_user == 0
            || expected_user > i64::MAX as u64
            || limits.request_timeout.is_zero()
            || limits.request_timeout > Duration::from_secs(60)
            || limits.approval_timeout.is_zero()
            || limits.approval_timeout > Duration::from_secs(300)
            || limits.token_age.is_zero()
            || limits.token_age > Duration::from_secs(86400)
            || !(1..=10).contains(&limits.registration_polls)
        {
            return Err(Error::Config);
        }
        let client = Client::builder()
            .pool_max_idle_per_host(0)
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Config)?;
        let (state, _) = watch::channel(State::Ready);
        Ok(Self {
            client,
            base: "https://katalk.kakao.com/android/account/".into(),
            profile,
            identity,
            expected: expected_user,
            limits,
            state,
            bound_email: None,
            challenge_deadline: None,
            logins: 0,
            refreshed: false,
            credential_deadline: None,
        })
    }
    /// Stable identity used for both HTTP authorization and LOCO.
    pub fn device_uuid(&self) -> &str {
        self.identity.expose_uuid()
    }
    /// Profile for the same sub-device session.
    pub fn loco_profile(&self) -> DeviceProfile {
        self.profile.loco_profile()
    }
    /// Latest state. watch is a latest-state channel, not an audit log.
    pub fn state(&self) -> State {
        if self
            .credential_deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.state.send_if_modified(|state| {
                if *state == State::CredentialsReady {
                    *state = State::Expired;
                    true
                } else {
                    false
                }
            });
        }
        self.state.borrow().clone()
    }
    /// Subscribe without exposing credentials.
    pub fn subscribe(&self) -> watch::Receiver<State> {
        self.state();
        self.state.subscribe()
    }
    fn begin(&self, state: State) -> OperationGuard {
        self.state.send_replace(state);
        OperationGuard {
            state: self.state.clone(),
            armed: true,
        }
    }
    fn finish<T>(
        &self,
        mut guard: OperationGuard,
        result: Result<T, Error>,
        success: State,
    ) -> Result<T, Error> {
        guard.armed = false;
        match result {
            Ok(value) => {
                self.state.send_replace(success);
                Ok(value)
            }
            Err(e) => {
                self.state.send_replace(State::Failed(e.clone()));
                Err(e)
            }
        }
    }
    fn bind_email(&mut self, email: &str) -> Result<(), Error> {
        match &self.bound_email {
            Some(bound) if bound.as_str() != email => Err(Error::AccountMismatch),
            Some(_) => Ok(()),
            None => {
                self.bound_email = Some(Zeroizing::new(email.to_owned()));
                Ok(())
            }
        }
    }
    fn headers(&self, email: &str) -> Result<HeaderMap, Error> {
        let ua = self.profile.user_agent();
        let input = Zeroizing::new(format!("BARD|{ua}|DANTE|{email}|SIAN"));
        let hash = hex(&Sha512::digest(input.as_bytes()));
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("a", self.profile.agent()),
            ("user-agent", ua),
            ("accept-language", "ko".into()),
            ("x-vc", hash[..16].into()),
        ] {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| Error::Config)?,
                HeaderValue::from_str(&value).map_err(|_| Error::Config)?,
            );
        }
        Ok(headers)
    }
    async fn send(
        &self,
        request: RequestBuilder,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Reply, Error> {
        let operation = async {
            let mut response = request.send().await.map_err(|_| Error::Transport)?;
            let code = response.status().as_u16();
            if response
                .content_length()
                .is_some_and(|size| size > MAX_RESPONSE as u64)
            {
                return Err(Error::Response);
            }
            let mut bytes = Zeroizing::new(Vec::new());
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
                if bytes.len() + chunk.len() > MAX_RESPONSE {
                    return Err(Error::Response);
                }
                bytes.extend_from_slice(&chunk);
            }
            if !(200..300).contains(&code) {
                return Err(Error::Http(code));
            }
            serde_json::from_slice(&bytes).map_err(|_| Error::Response)
        };
        tokio::select! {biased;
            _=crate::recovery::cancelled(&mut cancel)=>Err(Error::Cancelled),
            result=tokio::time::timeout(self.limits.request_timeout,operation)=>result.unwrap_or(Err(Error::Timeout)),
        }
    }
    /// Attempt login once (or once more after server-confirmed registration).
    /// -100 only enters NeedsApproval; no passcode/registration request is sent.
    pub async fn login(
        &mut self,
        email: &str,
        password: &str,
        cancel: watch::Receiver<bool>,
    ) -> Result<Option<Credentials>, Error> {
        if !matches!(self.state(), State::Ready | State::Registered) || self.logins >= 2 {
            return Err(Error::State);
        }
        validate_input(email, password)?;
        self.bind_email(email)?;
        self.logins += 1;
        let guard = self.begin(State::Authenticating);
        let result = async {
            let request = self
                .client
                .post(format!("{}login.json", self.base))
                .headers(self.headers(email)?)
                .form(&[
                    ("password", password),
                    ("device_name", &self.profile.device_name),
                    ("model_name", &self.profile.device_name),
                    ("forced", "false"),
                    ("permanent", "true"),
                    ("email", email),
                    ("device_uuid", self.device_uuid()),
                ]);
            let reply = self.send(request, cancel).await?;
            match reply.status.ok_or(Error::Response)? {
                0 => Ok(Some(self.credentials(reply, None)?)),
                -100 if self.logins == 1 => Ok(None),
                code @ (12 | 30) => Err(Error::InvalidCredentials(code)),
                code => Err(Error::Rejected(code)),
            }
        }
        .await;
        let state = if matches!(&result, Ok(None)) {
            State::NeedsApproval
        } else {
            State::CredentialsReady
        };
        if let Ok(Some(credentials)) = &result {
            self.credential_deadline = Some(credentials.expires);
        }
        self.finish(guard, result, state)
    }
    /// Record the caller's explicit human authorization before sending any device
    /// approval request. The UI must ask the actual user; silence is not approval.
    pub fn approve_device_registration(&mut self) -> Result<(), Error> {
        if self.state() != State::NeedsApproval {
            return Err(Error::State);
        }
        self.state.send_replace(State::Approved);
        Ok(())
    }
    /// Generate the documented approval challenge only after explicit approval.
    pub async fn request_challenge(
        &mut self,
        email: &str,
        password: &str,
        cancel: watch::Receiver<bool>,
    ) -> Result<Challenge, Error> {
        if self.state() == State::NeedsApproval {
            return Err(Error::ApprovalRequired);
        }
        if self.state() != State::Approved {
            return Err(Error::State);
        }
        validate_input(email, password)?;
        self.bind_email(email)?;
        let guard = self.begin(State::RequestingChallenge);
        let result=async{
            let body=serde_json::json!({"email":email,"password":password,"permanent":true,"device":{"name":self.profile.device_name,"uuid":self.device_uuid(),"model":self.profile.device_name,"osVersion":self.profile.api_level}});
            let reply=self.send(self.client.post(format!("{}passcodeLogin/generate",self.base)).headers(self.headers(email)?).header("Content-Type","application/json; charset=utf-8").json(&body),cancel).await?;
            if let Some(code)=reply.status{if code!=0{return Err(Error::Rejected(code));}}
            let code=reply.passcode.filter(|s|!s.is_empty()&&s.len()<=128&&!s.chars().any(char::is_control)).ok_or(Error::Response)?;
            let window = reply.remaining.map(Duration::from_secs)
                .unwrap_or(self.limits.approval_timeout).min(self.limits.approval_timeout);
            if window.is_zero() { return Err(Error::Expired); }
            self.challenge_deadline = Some(Instant::now() + window);
            Ok(Challenge(code))
        }.await;
        self.finish(guard, result, State::AwaitingDeviceConfirmation)
    }
    /// Poll documented registration status while the user confirms on the real
    /// device. Only -100 with a valid server interval permits another poll.
    /// Rejection is terminal. This never solves or bypasses a challenge.
    pub async fn confirm_device(
        &mut self,
        email: &str,
        password: &str,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        if self.state() != State::AwaitingDeviceConfirmation {
            return Err(Error::State);
        }
        validate_input(email, password)?;
        self.bind_email(email)?;
        let deadline = self.challenge_deadline.ok_or(Error::State)?;
        let guard = self.begin(State::Registering);
        let operation_cancel = cancel.clone();
        let operation = async {
            for _ in 0..self.limits.registration_polls {
                let body = serde_json::json!({"email":email,"password":password,"device":{"uuid":self.device_uuid()}});
                let reply = self
                    .send(
                        self.client
                            .post(format!("{}passcodeLogin/registerDevice", self.base))
                            .headers(self.headers(email)?)
                            .header("Content-Type", "application/json; charset=utf-8")
                            .json(&body),
                        operation_cancel.clone(),
                    )
                    .await?;
                match reply.status.ok_or(Error::Response)? {
                    0 => return Ok(()),
                    -100 => {
                        let interval = reply
                            .interval
                            .filter(|n| (1..=30).contains(n))
                            .ok_or(Error::Response)?;
                        tokio::time::sleep(Duration::from_secs(interval)).await;
                    }
                    code => return Err(Error::Rejected(code)),
                }
            }
            Err(Error::Timeout)
        };
        let result = tokio::select! {biased;
            _=crate::recovery::cancelled(&mut cancel)=>Err(Error::Cancelled),
            _=tokio::time::sleep_until(deadline)=>Err(Error::Timeout),
            result=operation=>result,
        };
        self.finish(guard, result, State::Registered)
    }
    fn credentials(
        &self,
        mut reply: Reply,
        old: Option<&Credentials>,
    ) -> Result<Credentials, Error> {
        if reply.user.is_some_and(|id| id != self.expected)
            || old.is_none() && reply.user != Some(self.expected)
        {
            return Err(Error::AccountMismatch);
        }
        let access = reply
            .access_token
            .take()
            .filter(|s| !s.is_empty() && s.len() <= 32768)
            .ok_or(Error::Response)?;
        let refresh = match reply.refresh_token.take() {
            Some(s) if !s.is_empty() && s.len() <= 32768 => Some(s),
            Some(s) if s.len() > 32768 => return Err(Error::Response),
            _ => old
                .and_then(|c| c.refresh.as_ref())
                .map(|s| Zeroizing::new(s.to_string())),
        };
        let age = reply
            .expires_in
            .map(Duration::from_secs)
            .unwrap_or(self.limits.token_age)
            .min(self.limits.token_age);
        if age.is_zero() {
            return Err(Error::Expired);
        }
        Ok(Credentials {
            access,
            refresh,
            user: self.expected,
            expires: Instant::now() + age,
            server_expiry: reply.expires_in.is_some(),
            device: Zeroizing::new(self.device_uuid().to_owned()),
        })
    }
    /// One explicit refresh, without automatic retry or registration. Rotations are
    /// account/device bound; LOGINLIST still confirms the resulting account identity.
    pub async fn refresh(
        &mut self,
        credentials: &Credentials,
        cancel: watch::Receiver<bool>,
    ) -> Result<Credentials, Error> {
        if !matches!(self.state(), State::CredentialsReady | State::Expired) || self.refreshed {
            return Err(Error::State);
        }
        if credentials.user != self.expected || credentials.device.as_str() != self.device_uuid() {
            return Err(Error::AccountMismatch);
        }
        let refresh = credentials
            .refresh
            .as_ref()
            .ok_or(Error::RefreshUnavailable)?;
        self.refreshed = true;
        let guard = self.begin(State::Refreshing);
        let result=async{
            let id=self.device_uuid();let id=if(40..=64).contains(&id.len())&&id.bytes().all(|b|b.is_ascii_hexdigit()){id.to_owned()}else{hex(&Sha256::digest(format!("dkljleskljfeisflssljeif {id}").as_bytes()))};
            let authorization=Zeroizing::new(format!("{}-{id}",credentials.access.as_str()));
            let mut header=HeaderValue::from_str(&authorization).map_err(|_|Error::Config)?;header.set_sensitive(true);
            let request=self.client.post(format!("{}oauth2_token.json",self.base)).header("A",self.profile.agent()).header("User-Agent",self.profile.user_agent()).header("Accept-Language","ko").header("C",uuid::Uuid::new_v4().to_string()).header("Connection","Close").header("Content-Type","application/json; charset=utf-8").header("Authorization",header)
                .json(&serde_json::json!({"access_token":credentials.access.as_str(),"refresh_token":refresh.as_str(),"grant_type":"refresh_token"}));
            let reply=self.send(request,cancel).await?;if let Some(status)=reply.status{if status!=0{return Err(Error::Rejected(status));}}self.credentials(reply,Some(credentials))
        }.await;
        if let Ok(credentials) = &result {
            self.credential_deadline = Some(credentials.expires);
        }
        self.finish(guard, result, State::CredentialsReady)
    }
    /// Mark expiration when known credentials have aged out. No network operation.
    pub fn check_expiration(&mut self, credentials: &Credentials) -> Result<(), Error> {
        if !matches!(self.state(), State::CredentialsReady | State::Expired) {
            return Err(Error::State);
        }
        if credentials.user != self.expected || credentials.device.as_str() != self.device_uuid() {
            return Err(Error::AccountMismatch);
        }
        if credentials.expires <= Instant::now() {
            self.state.send_replace(State::Expired);
            Err(Error::Expired)
        } else {
            Ok(())
        }
    }
}
fn validate_input(email: &str, password: &str) -> Result<(), Error> {
    if email.is_empty()
        || email.len() > 320
        || email.chars().any(char::is_control)
        || password.is_empty()
        || password.len() > 4096
    {
        Err(Error::Config)
    } else {
        Ok(())
    }
}
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(&mut out, "{b:02x}").expect("String formatting");
    }
    out
}

#[cfg(test)]
mod tests;
