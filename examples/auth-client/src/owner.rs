use crate::{ask, fail, pin_matches, ui, Failure, Progress, Stage};
use sculk_client::{
    auth::{self, AndroidProfile, Credentials, DeviceIdentity, Limits, Provider},
    connect::{self, Endpoint},
    dial::{ConnectProxy, DirectTcp},
    profile::{Connector, Profile, Settings},
    read_only::RoomId,
    session::Session,
    tls::{TlsSettings, TrustStore},
    Config,
};
use sculk_protocol::handshake::ServerKey;
use serde::Deserialize;
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{net::TcpStream, sync::watch};
use zeroize::Zeroizing;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Configuration {
    expected_user_id: u64,
    email: String,
    device_file: PathBuf,
    server_key_file: PathBuf,
    server_key_sha256: String,
    connection_mode: ConnectionMode,
    tcp_proxy_host: Option<String>,
    tcp_proxy_port: Option<u16>,
    allowed_endpoints: Vec<Destination>,
    app_version: String,
    os_version: String,
    api_level: String,
    device_name: String,
    test_room_id: Option<i64>,
    test_message_file: Option<PathBuf>,
    profile_validated: bool,
    protocol_profile: Option<String>,
    validated_protocol_profile: Option<String>,
    credential_age_seconds: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ConnectionMode {
    #[default]
    Proxy,
    Direct,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Destination {
    host: String,
    port: u16,
}

enum Route {
    Proxy(ConnectProxy),
    Direct(DirectTcp),
}

impl Route {
    fn configure(config: &Configuration, endpoints: &[Endpoint]) -> Result<Self, Failure> {
        let timeout = Duration::from_secs(10);
        match (
            &config.connection_mode,
            &config.tcp_proxy_host,
            config.tcp_proxy_port,
        ) {
            (ConnectionMode::Proxy, Some(host), Some(port)) => Ok(Self::Proxy(
                ConnectProxy::new(host.clone(), port, endpoints.to_vec(), timeout)
                    .map_err(|_| fail("invalid CONNECT proxy settings"))?,
            )),
            (ConnectionMode::Direct, None, None) => {
                Ok(Self::Direct(DirectTcp::new(endpoints.to_vec(), timeout)?))
            }
            _ => Err(fail("connection mode and proxy settings disagree")),
        }
    }

    async fn dial(
        &self,
        endpoint: Endpoint,
        cancel: watch::Receiver<bool>,
    ) -> io::Result<TcpStream> {
        match self {
            Self::Proxy(route) => route.dial(endpoint, cancel).await,
            Self::Direct(route) => route.dial(endpoint, cancel).await,
        }
    }
}

impl Configuration {
    fn read(path: &Path) -> Result<Self, Failure> {
        let bytes = auth::read_private_file(path, 16_384)
            .map_err(|_| fail("private authentication JSON unavailable or unsafe"))?;
        serde_json::from_slice(&bytes).map_err(|_| fail("invalid private authentication JSON"))
    }

    fn endpoints(&self) -> Result<Vec<Endpoint>, Failure> {
        if self.allowed_endpoints.is_empty() || self.allowed_endpoints.len() > 128 {
            return Err(fail("configure 1..128 authorized allowed_endpoints"));
        }
        self.allowed_endpoints
            .iter()
            .map(|endpoint| {
                Endpoint::new(&endpoint.host, endpoint.port)
                    .map_err(|_| fail("invalid endpoint in allowed_endpoints"))
            })
            .collect()
    }
}

/// Checked local settings. Loading them does not open a socket or create a device.
pub struct Prepared {
    config: Configuration,
    profile: AndroidProfile,
    connector: Connector,
    route: Route,
    room: Option<RoomId>,
    existing_identity: bool,
}

/// A password supplied through an explicit operator gate for this process.
pub struct Approved {
    password: Zeroizing<String>,
    registered_only: bool,
}

/// Owner-approved credentials and the exact route/profile bound to their UUID.
pub struct Authorized {
    credentials: Credentials,
    device: Zeroizing<String>,
    connector: Connector,
    route: Route,
    _device_lock: std::fs::File,
}

impl Prepared {
    pub fn load(path: &Path, progress: &Progress, require_room: bool) -> Result<Self, Failure> {
        let config = Configuration::read(path)?;
        let selected = config
            .protocol_profile
            .as_deref()
            .map(str::parse::<Profile>)
            .transpose()?
            .unwrap_or_default();
        let mut issues = Vec::new();
        if config.expected_user_id == 0
            || config.expected_user_id > i64::MAX as u64
            || config.email.is_empty()
            || config.email.len() > 320
            || config.email.chars().any(char::is_control)
        {
            issues.push("account identity (expected_user_id and email)");
        }
        let profile = AndroidProfile {
            app_version: config.app_version.clone(),
            os_version: config.os_version.clone(),
            api_level: config.api_level.clone(),
            device_name: config.device_name.clone(),
        };
        if profile.validate().is_err() {
            issues.push("client profile (app_version, os_version, api_level and device_name)");
        } else {
            if profile.validate_for(selected).is_err() {
                eprintln!("android-tablet requires the documented app_version 25.9.2, os_version 13 and api_level 33 for the owner-authorized device");
                return Err(sculk_client::profile::UnsupportedProfile.into());
            }
        }
        if config
            .validated_protocol_profile
            .as_deref()
            .map(str::parse::<Profile>)
            .transpose()?
            .is_some_and(|validated| validated != selected)
        {
            issues.push("validated_protocol_profile must match protocol_profile");
        }
        if !config.device_file.is_absolute() {
            issues.push("absolute device_file path");
        }
        if !config.server_key_file.is_absolute() {
            issues.push("absolute server_key_file path to an independently trusted key");
        }
        if config.server_key_sha256.len() != 64
            || !config
                .server_key_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            issues.push("independently verified server_key_sha256 fingerprint");
        }
        let room = config
            .test_room_id
            .and_then(|value| RoomId::new(value).ok());
        if room.is_none() && (require_room || config.test_room_id.is_some()) {
            issues.push("positive test_room_id for an operator-authorized room");
        }
        let booking = Endpoint::new("booking-loco.kakao.com", 443)?;
        let endpoints = config.endpoints();
        if !endpoints
            .as_ref()
            .is_ok_and(|endpoints| endpoints.contains(&booking))
        {
            issues.push("authorized allowed_endpoints including Booking");
        }
        if let Some(port) = selected.chat_tls_port() {
            if !endpoints
                .as_ref()
                .is_ok_and(|routes| routes.iter().any(|route| route.port() == port))
            {
                issues.push("authorized assigned chat endpoint on port 10009 for android-tablet");
            }
        }
        if config
            .credential_age_seconds
            .is_some_and(|sec| !(1..=86400).contains(&sec))
        {
            issues.push("credential_age_seconds must be 1..86400");
        }
        if !issues.is_empty() {
            for issue in issues {
                eprintln!("configuration missing or invalid: {issue}");
            }
            return Err(fail("check the listed private configuration settings"));
        }
        let endpoints = endpoints?;
        let route = Route::configure(&config, &endpoints)?;
        progress.completed(Stage::Configuration);
        progress.stage.set(Stage::ServerKey);
        let key = auth::read_private_file(&config.server_key_file, 4096)
            .map_err(|_| fail("independently trusted public key file is missing or unsafe"))?;
        if !pin_matches(&key, &config.server_key_sha256) {
            return Err(fail("RSA public-key fingerprint mismatch"));
        }
        let key = ServerKey::from_pkcs1_der(&key).map_err(|_| fail("invalid RSA public key"))?;
        progress.completed(Stage::ServerKey);
        progress.stage.set(Stage::SystemTrust);
        let mut trust = TrustStore::empty();
        for cert in rustls_native_certs::load_native_certs().certs {
            trust.add(cert).map_err(|_| fail("invalid system CA"))?;
        }
        if trust.is_empty() {
            return Err(fail("system trust store unavailable"));
        }
        progress.stage.set(Stage::DeviceIdentity);
        let existing_identity = match std::fs::symlink_metadata(&config.device_file) {
            Ok(_) => {
                DeviceIdentity::load_existing(&config.device_file).map_err(|_| {
                    fail("existing device identity has invalid contents or permissions")
                })?;
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return Err(fail("device identity could not be inspected")),
        };
        let settings = Settings {
            booking,
            tls: TlsSettings::with_roots(trust),
            transport: Config {
                request_timeout: Duration::from_secs(5),
                ..Config::default()
            },
            stage_timeout: Duration::from_secs(15),
            total_timeout: Duration::from_secs(60),
            session_lease: Duration::from_secs(86400),
        };
        let connector = Connector::new(selected, profile.loco_profile(), key, settings)?;
        let prepared = Self {
            config,
            profile,
            connector,
            route,
            room,
            existing_identity,
        };
        println!("protocol-profile={selected} device-identity-present={existing_identity} owner-profile-validation-recorded={} room-configured={}", prepared.profile_validated(), room.is_some());
        Ok(prepared)
    }

    pub fn room(&self) -> Option<RoomId> {
        self.room
    }

    pub fn monitor(&mut self, monitor: sculk_client::monitor::Monitor) {
        self.connector.monitor(monitor);
    }

    fn profile_validated(&self) -> bool {
        self.config.profile_validated
            && (self.connector.profile() == Profile::AndroidLegacy
                || self.config.validated_protocol_profile.is_some())
    }

    pub fn require_validated_profile(&self) -> Result<(), Failure> {
        if self.profile_validated() {
            Ok(())
        } else {
            Err(fail("continuous operation requires an owner-validated client profile and approved room (profile_validated and, for tablet, validated_protocol_profile)"))
        }
    }

    pub fn test_message(&self) -> Result<Zeroizing<String>, Failure> {
        let path = self
            .config
            .test_message_file
            .as_ref()
            .filter(|p| p.is_absolute())
            .ok_or_else(|| fail("send-one requires an absolute test_message_file path"))?;
        let data = auth::read_private_file(path, 16384)?;
        let text = std::str::from_utf8(&data).map_err(|_| fail("invalid UTF-8 test message"))?;
        if text.is_empty() || text.contains('\0') {
            return Err(fail("invalid test message"));
        }
        Ok(Zeroizing::new(text.to_owned()))
    }

    /// Account and room authorization are separate from any permission to send.
    pub fn permit_terminal(
        &self,
        progress: &Progress,
        confirm_room: bool,
        writes: bool,
    ) -> Result<Approved, Failure> {
        progress.stage.set(Stage::AccountAuthorization);
        if !ask(
            "Authorize this configured account for KakaoTalk authentication and connection?",
            "authorize",
        ) {
            return Err(fail("account authorization declined"));
        }
        if confirm_room {
            progress.stage.set(Stage::RoomAuthorization);
            let room = self.room.ok_or_else(|| fail("missing selected room"))?;
            if !ask(
                &format!(
                    "Confirm authorization for the configured room {}.",
                    room.get()
                ),
                "room",
            ) {
                return Err(fail("room authorization declined"));
            }
        }
        if writes {
            progress.stage.set(Stage::RoomAuthorization);
            if !ask(
                "Authorize the loaded scripts to send text in this room during this run?",
                "write",
            ) {
                return Err(fail("script write authorization declined"));
            }
        }
        progress.stage.set(Stage::PasswordInput);
        let password = rpassword::prompt_password("Account password (hidden): ")
            .map_err(|_| fail("password prompt failed"))?;
        Ok(Approved {
            password: Zeroizing::new(password),
            registered_only: false,
        })
    }

    /// Acquire credentials once, with separate human registration approval if needed.
    pub async fn authorize(
        self,
        approval: Approved,
        refresh: bool,
        progress: &Progress,
        cancel: watch::Receiver<bool>,
    ) -> Result<Authorized, Failure> {
        progress.stage.set(Stage::DeviceIdentity);
        let identity = if approval.registered_only {
            if !self.existing_identity {
                return Err(fail(
                    "the service requires a previously approved device identity",
                ));
            }
            DeviceIdentity::load_existing(&self.config.device_file)?
        } else {
            DeviceIdentity::load_or_create(&self.config.device_file)?
        };
        let device_lock = lock_device(&self.config.device_file)?;
        let mut provider = Provider::new(
            self.profile,
            identity,
            self.config.expected_user_id,
            Limits {
                token_age: Duration::from_secs(self.config.credential_age_seconds.unwrap_or(900)),
                ..Limits::default()
            },
        )?;
        let email = Zeroizing::new(self.config.email);
        progress.stage.set(Stage::HttpsAuthentication);
        let mut credentials = match provider
            .login(&email, &approval.password, cancel.clone())
            .await?
        {
            Some(credentials) => credentials,
            None if approval.registered_only => {
                return Err(fail(
                    "device approval required; use the interactive owner probe",
                ));
            }
            None => {
                progress.stage.set(Stage::DeviceRegistrationApproval);
                if !ui::registration_approval(cancel.clone())
                    .await
                    .map_err(|_| fail("registration approval unavailable or cancelled"))?
                {
                    return Err(fail("device registration not approved"));
                }
                provider.approve_device_registration()?;
                progress.completed(Stage::DeviceRegistrationApproval);
                progress.stage.set(Stage::DeviceChallenge);
                let challenge = provider
                    .request_challenge(&email, &approval.password, cancel.clone())
                    .await?;
                ui::show_challenge(challenge.expose_code())
                    .map_err(|_| fail("cannot display approval code privately"))?;
                drop(challenge);
                progress.stage.set(Stage::DeviceConfirmation);
                provider
                    .confirm_device(&email, &approval.password, cancel.clone())
                    .await?;
                progress.stage.set(Stage::PostRegistrationLogin);
                provider
                    .login(&email, &approval.password, cancel.clone())
                    .await?
                    .ok_or_else(|| fail("device approval incomplete"))?
            }
        };
        drop(approval);
        drop(email);
        progress.completed(Stage::HttpsAuthentication);
        if refresh {
            progress.stage.set(Stage::ExplicitRefresh);
            credentials = provider.refresh(&credentials, cancel).await?;
        }
        println!(
            "credential-expiry={}",
            if credentials.has_server_expiration() {
                "server-capped"
            } else {
                "local-cap"
            }
        );
        Ok(Authorized {
            credentials,
            device: Zeroizing::new(provider.device_uuid().to_owned()),
            connector: self.connector,
            route: self.route,
            _device_lock: device_lock,
        })
    }
}

// The validated identity lives in a private owner directory. A kernel lock
// spans HTTP authorization and all LOCO generations, and releases on exit.
#[cfg(unix)]
fn lock_device(device: &Path) -> Result<std::fs::File, Failure> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut lock_path = device.as_os_str().to_owned();
    lock_path.push(".session.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(Path::new(&lock_path))
        .map_err(|_| fail("cannot open private device session lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.uid() != device.symlink_metadata()?.uid()
    {
        return Err(fail("unsafe private device session lock"));
    }
    file.try_lock()
        .map_err(|_| fail("the device identity is already in use by another process"))?;
    Ok(file)
}

#[cfg(not(unix))]
fn lock_device(_: &Path) -> Result<std::fs::File, Failure> {
    Err(fail("device locking requires Unix private state support"))
}

impl Authorized {
    /// Open one verified session with the same in-memory credentials and exact route.
    /// Reconnection does not call the HTTPS login or registration APIs.
    pub async fn connect(
        &self,
        progress: &Progress,
        cancel: watch::Receiver<bool>,
    ) -> Result<Session, connect::Error> {
        let token = self
            .credentials
            .protocol_token()
            .map_err(|error| match error {
                auth::Error::Expired => connect::Error::Expired,
                _ => connect::Error::Config,
            })?;
        let session = self
            .connector
            .connect(token, &self.device, cancel.clone(), |stage, endpoint| {
                progress.protocol_enter(stage);
                println!("protocol/{}", progress.stage.get().label());
                self.route.dial(endpoint, cancel.clone())
            })
            .await?;
        progress.completed(Stage::Loginlist);
        println!("protocol/account-identity-verified");
        Ok(session)
    }
}

/// Probe only the configured routes. No account, device, certificate or key
/// is needed, and no LOCO or account packet is transmitted.
pub async fn preflight(
    path: &Path,
    progress: &Progress,
    cancel: watch::Receiver<bool>,
) -> Result<(), Failure> {
    let config = Configuration::read(path)?;
    let endpoints = config.endpoints()?;
    let route = Route::configure(&config, &endpoints)?;
    progress.stage.set(Stage::NetworkPreflight);
    let operation = async {
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            let socket = match route.dial(endpoint, cancel.clone()).await {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("preflight target={} result={:?}", index + 1, error.kind());
                    return Err(fail("authorized route did not open"));
                }
            };
            drop(socket);
            println!("preflight target={} result=open", index + 1);
        }
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(60), operation)
        .await
        .map_err(|_| fail("overall network preflight deadline elapsed"))??;
    progress.completed(Stage::NetworkPreflight);
    Ok(())
}

/// Local signal watcher shared by the owner probe and the live executable.
pub fn signal_channel() -> io::Result<(tokio::task::JoinHandle<()>, watch::Receiver<bool>)> {
    let (sender, receiver) = watch::channel(false);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let task = tokio::spawn(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        sender.send_replace(true);
    });
    Ok((task, receiver))
}

/// Consume a systemd credential staged by the operator for one explicit start.
/// The application never stores account passwords.
#[cfg(target_os = "linux")]
pub fn permit_service(
    prepared: &Prepared,
    writes: bool,
    progress: &Progress,
) -> Result<Approved, Failure> {
    if !prepared.existing_identity {
        return Err(fail(
            "the service requires a previously approved device identity",
        ));
    }
    progress.stage.set(Stage::AccountAuthorization);
    let directory = std::env::var_os("CREDENTIALS_DIRECTORY")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| fail("start the service with a fresh operator credential"))?;
    let bytes = auth::read_private_file(&directory.join("sculk-approval"), 4096)
        .map_err(|_| fail("private systemd approval credential unavailable"))?;
    let content = std::str::from_utf8(&bytes).map_err(|_| fail("invalid approval credential"))?;
    let (scope, remainder) = content
        .split_once('\n')
        .ok_or_else(|| fail("invalid approval credential"))?;
    let (issued, password) = remainder
        .split_once('\n')
        .ok_or_else(|| fail("invalid approval credential"))?;
    let issued = issued
        .parse::<u64>()
        .map_err(|_| fail("invalid approval credential timestamp"))?;
    if !std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|now| now.as_secs().checked_sub(issued))
        .is_some_and(|age| age <= 120)
    {
        return Err(fail(
            "systemd approval is stale; request a new operator credential",
        ));
    }
    let expected = if writes {
        "sculk-write"
    } else {
        "sculk-read-only"
    };
    if scope != expected {
        return Err(fail(
            "operator approval scope differs from service configuration",
        ));
    }
    let password = password.strip_suffix('\n').unwrap_or(password);
    if password.is_empty() || password.contains(['\n', '\r', '\0']) {
        return Err(fail("invalid approval credential"));
    }
    Ok(Approved {
        password: Zeroizing::new(password.to_owned()),
        registered_only: true,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn permit_service(_: &Prepared, _: bool, _: &Progress) -> Result<Approved, Failure> {
    Err(fail("systemd authorization is available on Linux"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    use tokio::io::AsyncReadExt;

    #[test]
    fn a_device_session_lock_refuses_another_owner_until_released() {
        let root = std::env::temp_dir().join(format!("sculk-session-lock-{}", std::process::id()));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let path = root.join("device");
        DeviceIdentity::load_or_create(&path).unwrap();
        let lock = lock_device(&path).unwrap();
        assert!(lock_device(&path).is_err());
        drop(lock);
        assert!(lock_device(&path).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn network_preflight_uses_only_an_approved_local_route_and_sends_no_auth_data() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let directory =
            std::env::temp_dir().join(format!("sculk-preflight-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("routing.json");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let config = serde_json::json!({"connection_mode":"direct", "allowed_endpoints":[
            {"host":"localhost","port":listener.local_addr().unwrap().port()}
        ]});
        serde_json::to_writer(&mut file, &config).unwrap();
        file.flush().unwrap();
        drop(file);
        let (_stop, cancel) = watch::channel(false);
        let progress = Progress::default();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(preflight(&path, &progress, cancel), async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
            })
        })
        .await
        .unwrap();
        result.unwrap();
        let m = progress.milestones();
        assert!(m.network_preflight);
        assert!(!m.https_authentication && !m.loginlist);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
