//! Opt-in local validation tool. Help/dry-run perform no network operations.
mod diagnostics;
mod ui;
mod validation;
use diagnostics::{Milestones, Progress, Stage};
use sculk_client::{
    auth::{self, AndroidProfile, DeviceIdentity, Limits, Provider},
    connect::{BookingSchema, Endpoint, Options, Protection},
    dial::ConnectProxy,
    messaging::Event as MessageEvent,
    read_only::Cursor,
    tls::{TlsSettings, TrustStore},
    Config,
};
use sculk_protocol::handshake::ServerKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use zeroize::Zeroizing;
type Failure = Box<dyn std::error::Error + Send + Sync>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    expected_user_id: u64,
    email: String,
    device_file: PathBuf,
    server_key_file: PathBuf,
    server_key_sha256: String,
    tcp_proxy_host: String,
    tcp_proxy_port: u16,
    allowed_endpoints: Vec<Destination>,
    app_version: String,
    os_version: String,
    api_level: String,
    device_name: String,
    test_room_id: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Destination {
    host: String,
    port: u16,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Help,
    DryRun,
    Live,
}
struct Arguments {
    mode: Mode,
    config: Option<PathBuf>,
    refresh: bool,
    read_only: bool,
    listen: Option<u64>,
    receive_one: Option<u64>,
    report: Option<PathBuf>,
}
impl Arguments {
    fn parse(args: &[String]) -> Result<Self, Failure> {
        let mut result = Self {
            mode: Mode::Help,
            config: None,
            refresh: false,
            read_only: false,
            listen: None,
            receive_one: None,
            report: None,
        };
        if args.is_empty() || args == ["--help"] {
            return Ok(result);
        }
        if args.len() < 2 {
            return Err(fail("missing configuration; use --help"));
        }
        result.mode = match args[0].as_str() {
            "--live" => Mode::Live,
            "--dry-run" | "--check-config" => Mode::DryRun,
            _ => return Err(fail("invalid mode; use --help")),
        };
        result.config = Some(PathBuf::from(&args[1]));
        let mut index = 2;
        while index < args.len() {
            match args[index].as_str() {
                "--refresh" if result.mode == Mode::Live && !result.refresh => {
                    result.refresh = true
                }
                "--read-only" if result.mode == Mode::Live && !result.read_only => {
                    result.read_only = true
                }
                "--listen" | "--receive-one"
                    if result.mode == Mode::Live
                        && result.listen.is_none()
                        && result.receive_one.is_none()
                        && index + 1 < args.len() =>
                {
                    let receive_one = args[index] == "--receive-one";
                    index += 1;
                    let seconds = args[index]
                        .parse::<u64>()
                        .map_err(|_| fail("invalid listen duration"))?;
                    if !(1..=300).contains(&seconds) {
                        return Err(fail("listen duration must be 1..300 seconds"));
                    }
                    if receive_one {
                        result.receive_one = Some(seconds);
                    } else {
                        result.listen = Some(seconds);
                    }
                }
                "--report" if result.report.is_none() && index + 1 < args.len() => {
                    index += 1;
                    result.report = Some(PathBuf::from(&args[index]));
                }
                _ => return Err(fail("invalid, duplicated or unsafe option; use --help")),
            }
            index += 1;
        }
        Ok(result)
    }
}
#[derive(Serialize)]
struct Report {
    mode: &'static str,
    schema_version: u8,
    last_stage: Stage,
    milestones: Milestones,
    outcome: &'static str,
    failure_category: Option<&'static str>,
    server_status: Option<i64>,
    http_status: Option<u16>,
    live_connection_verified: bool,
    controlled_receive_verified: bool,
}
// Extract only documented status fields from known typed errors; never format
// a source chain or serialize account IDs, raw HTTP/BSON or arbitrary errors.
fn status_codes(error: &Failure) -> (Option<i64>, Option<u16>) {
    use sculk_client::{
        connect::Error as Connect,
        read_only::Error as Read,
        session::{AuthError, RequestError},
    };
    if let Some(error) = error.downcast_ref::<auth::Error>() {
        return match error {
            auth::Error::InvalidCredentials(n) | auth::Error::Rejected(n) => (Some(*n), None),
            auth::Error::Http(n) => (None, Some(*n)),
            _ => (None, None),
        };
    }
    if let Some(error) = error.downcast_ref::<Connect>() {
        return match error {
            Connect::Rejected(_, n) | Connect::Auth(AuthError::Rejected(n)) => (Some(*n), None),
            Connect::Transport(_, sculk_client::Error::Rejected(n)) => {
                (Some(i64::from(*n as i16)), None)
            }
            _ => (None, None),
        };
    }
    if let Some(validation::Error::Room(error)) = error.downcast_ref::<validation::Error>() {
        return match error {
            Read::Rejected(n) => (Some(*n), None),
            Read::Session(RequestError::Transport(sculk_client::Error::Rejected(n))) => {
                (Some(i64::from(*n as i16)), None)
            }
            _ => (None, None),
        };
    }
    (None, None)
}
// Only typed, static categories enter reports. Never format raw error/source text.
fn failure_category(error: &Failure) -> &'static str {
    if let Some(error) = error.downcast_ref::<validation::Error>() {
        return match error {
            validation::Error::Room(sculk_client::read_only::Error::Rejected(_)) => {
                "room-query-rejected"
            }
            validation::Error::Room(_) => "room-query-failed",
            validation::Error::Receive(sculk_client::messaging::ReceiveError::Lagged(_)) => {
                "message-stream-lagged"
            }
            validation::Error::Receive(_) => "message-stream-ended",
            validation::Error::Timeout => "receive-timeout",
            validation::Error::Cancelled => "cancelled",
            validation::Error::Clock => "system-clock",
            validation::Error::InvalidMessage => "message-metadata-invalid",
            validation::Error::UnsupportedEvent => "unsupported-event",
        };
    }
    if let Some(error) = error.downcast_ref::<sculk_client::ShutdownError>() {
        return match error {
            sculk_client::ShutdownError::Closed => "shutdown-already-lost",
            sculk_client::ShutdownError::Timeout => "shutdown-timeout",
            sculk_client::ShutdownError::Transport => "shutdown-transport",
            sculk_client::ShutdownError::Cancelled => "cancelled",
        };
    }
    if let Some(error) = error.downcast_ref::<auth::Error>() {
        return match error {
            auth::Error::InvalidCredentials(_) => "credentials-rejected",
            auth::Error::Rejected(_) => "server-rejected",
            auth::Error::Http(_) => "http-status",
            auth::Error::Transport => "http-transport-or-tls",
            auth::Error::Response => "invalid-http-response",
            auth::Error::Timeout => "timeout",
            auth::Error::Cancelled => "cancelled",
            auth::Error::AccountMismatch => "account-mismatch",
            _ => "authentication-state-or-configuration",
        };
    }
    if let Some(error) = error.downcast_ref::<sculk_client::connect::Error>() {
        use sculk_client::connect::Error::*;
        return match error {
            DialIo(_, io::ErrorKind::PermissionDenied) => "dial-permission-denied",
            DialIo(_, io::ErrorKind::ConnectionRefused) => "dial-connection-refused",
            DialIo(_, io::ErrorKind::ConnectionReset) => "dial-connection-reset",
            DialIo(_, io::ErrorKind::TimedOut) => "dial-timeout",
            Dial(_) | DialIo(_, _) => "dial-failed",
            Transport(_, _) => "loco-transport",
            Response(_) => "invalid-loco-response",
            Rejected(_, _) => "server-rejected",
            Tls(_) => "tls-handshake-or-verification",
            Crypto => "key-exchange",
            Cancelled => "cancelled",
            Timeout => "timeout",
            Expired => "expired",
            Auth(_) => "loginlist-authentication",
            Config => "configuration",
        };
    }
    if error
        .downcast_ref::<sculk_client::messaging::ReceiveError>()
        .is_some()
    {
        return "message-stream-ended-or-lagged";
    }
    "local-validation-or-operation"
}
#[derive(Debug)]
struct LocalFailure(&'static str);
impl std::fmt::Display for LocalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for LocalFailure {}
fn fail(message: &'static str) -> Failure {
    Box::new(LocalFailure(message))
}
fn ask(prompt: &str, answer: &str) -> bool {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return false;
    }
    print!("{prompt}\nType {answer} to continue: ");
    if io::stdout().flush().is_err() {
        return false;
    }
    let mut input = String::new();
    io::stdin().read_line(&mut input).is_ok() && input.trim() == answer
}
fn pin_matches(bytes: &[u8], expected: &str) -> bool {
    expected.len() == 64
        && expected.bytes().all(|b| b.is_ascii_hexdigit())
        && format!("{:x}", Sha256::digest(bytes)).eq_ignore_ascii_case(expected)
}
fn write_report(path: &Path, report: &Report) -> Result<(), Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| {
                fail("cannot create private report; existing files are never overwritten")
            })?;
        serde_json::to_writer_pretty(&mut file, report)
            .map_err(|_| fail("cannot encode sanitized report"))?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, report);
        Err(fail("private reports require Unix file permission support"))
    }
}
fn state_name(state: &auth::State) -> &'static str {
    use auth::State::*;
    match state {
        Ready => "ready",
        Authenticating => "authenticating",
        NeedsApproval => "approval-required",
        Approved => "approved",
        RequestingChallenge => "requesting-challenge",
        AwaitingDeviceConfirmation => "awaiting-device-confirmation",
        Registering => "registering",
        Registered => "registered",
        CredentialsReady => "credentials-ready",
        Refreshing => "refreshing",
        Expired => "expired",
        Failed(_) => "failed",
    }
}
fn main() {
    if let Err(error) = run() {
        if let Some(local) = error.downcast_ref::<LocalFailure>() {
            eprintln!("{local}");
        } else {
            eprintln!(
                "Validation failed ({}). Use --help; no automatic retry was performed.",
                failure_category(&error)
            );
        }
        let (server, http) = status_codes(&error);
        if let Some(code) = server {
            eprintln!("Server status: {code}");
        }
        if let Some(code) = http {
            eprintln!("HTTP status: {code}");
        }
        std::process::exit(1);
    }
}
fn run() -> Result<(), Failure> {
    let args = Arguments::parse(&std::env::args().skip(1).collect::<Vec<_>>())?;
    if args.mode == Mode::Help {
        println!("Sculk experimental connection validation\n--dry-run PATH [--report NEW_PATH]: local config/key checks only; no network\n--check-config is an alias for --dry-run\n--live PATH [--refresh] [--read-only] [--listen SECONDS | --receive-one SECONDS] [--report NEW_PATH]: explicitly authorized account only\nLive mode requires a terminal, account approval, and separate device-registration approval.\n--receive-one requires configured test_room_id and separate room consent; validates one fresh text without logging content or IDs.\n--read-only requests one LCHATLIST page; --listen 1..300 receives events and prints counts only. No messages, read marks or moderation operations are sent.\nReports contain separate fixed-label milestones and booleans; never secrets, IDs or approval codes.");
        return Ok(());
    }
    let progress = Progress::default();
    let result = attempt(&args, &progress);
    let milestones = progress.milestones();
    let (server_status, http_status) = result
        .as_ref()
        .err()
        .map(status_codes)
        .unwrap_or((None, None));
    if let Some(path) = &args.report {
        write_report(
            path,
            &Report {
                mode: if args.mode == Mode::Live {
                    "live"
                } else {
                    "dry-run"
                },
                schema_version: 2,
                last_stage: progress.stage.get(),
                milestones,
                outcome: if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
                live_connection_verified: milestones.loginlist,
                controlled_receive_verified: milestones.controlled_message,
                failure_category: result.as_ref().err().map(failure_category),
                server_status,
                http_status,
            },
        )?;
    }
    if result.is_err() {
        eprintln!(
            "Failed stage: {}. No automatic retry was performed.",
            progress.stage.get().label()
        );
    }
    result
}
fn attempt(args: &Arguments, progress: &Progress) -> Result<(), Failure> {
    let bytes = auth::read_private_file(
        args.config
            .as_ref()
            .ok_or_else(|| fail("missing configuration"))?,
        16384,
    )?;
    let config: Configuration = serde_json::from_slice(&bytes).map_err(|_| {
        fail("invalid private configuration (required fields include server_key_sha256)")
    })?;
    drop(bytes);
    if config.expected_user_id == 0
        || config.expected_user_id > i64::MAX as u64
        || config.email.is_empty()
        || config.email.len() > 320
        || config.email.chars().any(char::is_control)
    {
        return Err(fail("invalid configured account"));
    }
    if config.test_room_id.is_some_and(|id| id <= 0)
        || (args.receive_one.is_some() && config.test_room_id.is_none())
    {
        return Err(fail(
            "receive-one requires a positive configured test_room_id",
        ));
    }
    let test_room = config
        .test_room_id
        .map(sculk_client::read_only::RoomId::new)
        .transpose()?;
    let profile = AndroidProfile {
        app_version: config.app_version,
        os_version: config.os_version,
        api_level: config.api_level,
        device_name: config.device_name,
    };
    profile.validate()?;
    if !config.device_file.is_absolute() || !config.server_key_file.is_absolute() {
        return Err(fail("identity and key paths must be absolute"));
    }
    let allowed = config
        .allowed_endpoints
        .iter()
        .map(|d| Endpoint::new(&d.host, d.port))
        .collect::<Result<Vec<_>, _>>()?;
    let booking = Endpoint::new("booking-loco.kakao.com", 443)?;
    if !allowed.contains(&booking) {
        return Err(fail("allowlist must include Booking"));
    }
    let proxy = ConnectProxy::new(
        config.tcp_proxy_host,
        config.tcp_proxy_port,
        allowed,
        Duration::from_secs(15),
    )?;
    progress.completed(Stage::Configuration);
    progress.stage.set(Stage::ServerKey);
    let key = auth::read_private_file(&config.server_key_file, 4096)?;
    if !pin_matches(&key, &config.server_key_sha256) {
        return Err(fail(
            "RSA public-key fingerprint mismatch; verify provenance before any login",
        ));
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
    if args.mode == Mode::DryRun {
        progress.stage.set(Stage::LocalValidationComplete);
        println!("LOCAL DRY RUN complete. No network requests, authentication or device identity creation. Live compatibility remains unverified.");
        return Ok(());
    }
    progress.stage.set(Stage::AccountAuthorization);
    if !ask("Authorize this configured test account for experimental authentication and connection? No messages or moderation operations will be sent.","authorize"){return Err(fail("test-account authorization not granted; no network request"));}
    if args.receive_one.is_some() {
        progress.stage.set(Stage::RoomAuthorization);
    }
    if args.receive_one.is_some() && !ask("Authorize receive-only validation in the configured test room you own or have permission to test?", "receive") {
        return Err(fail("test-room authorization not granted; no network request"));
    }
    progress.stage.set(Stage::DeviceIdentity);
    let identity = DeviceIdentity::load_or_create(&config.device_file)?;
    progress.stage.set(Stage::PasswordInput);
    let password = Zeroizing::new(
        rpassword::prompt_password("Account password (hidden): ")
            .map_err(|_| fail("password prompt failed"))?,
    );
    let email = Zeroizing::new(config.email);
    let mut provider = Provider::new(
        profile,
        identity,
        config.expected_user_id,
        Limits::default(),
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async{
        let (cancel_tx,cancel)=tokio::sync::watch::channel(false);let signal=tokio::spawn(async move{let _=tokio::signal::ctrl_c().await;cancel_tx.send_replace(true);});
        let mut states=provider.subscribe();let observer=tokio::spawn(async move{loop{println!("auth/{}",state_name(&states.borrow_and_update()));if states.changed().await.is_err(){break;}}});
        let result=async{
            progress.stage.set(Stage::HttpsAuthentication);let credentials=match provider.login(&email,&password,cancel.clone()).await?{Some(c)=>c,None=>{
                progress.stage.set(Stage::DeviceRegistrationApproval);if !ui::registration_approval(cancel.clone()).await.map_err(|_|fail("registration approval cancelled, timed out or unavailable"))?{return Err(fail("device registration not approved"));}
                provider.approve_device_registration()?;progress.completed(Stage::DeviceRegistrationApproval);progress.stage.set(Stage::DeviceChallenge);let challenge=provider.request_challenge(&email,&password,cancel.clone()).await?;
                ui::show_challenge(challenge.expose_code()).map_err(|_|fail("cannot display approval code privately"))?;drop(challenge);
                progress.stage.set(Stage::DeviceConfirmation);provider.confirm_device(&email,&password,cancel.clone()).await?;
                progress.stage.set(Stage::PostRegistrationLogin);provider.login(&email,&password,cancel.clone()).await?.ok_or_else(||fail("device approval incomplete"))?
            }};
            progress.completed(Stage::HttpsAuthentication);
            drop(password);drop(email);
            let credentials=if args.refresh{progress.stage.set(Stage::ExplicitRefresh);let next=provider.refresh(&credentials,cancel.clone()).await?;drop(credentials);next}else{credentials};
            if !credentials.has_server_expiration(){println!("Using an explicit local credential-age cap; server expiry was absent.");}
            let profile=provider.loco_profile();let device=Zeroizing::new(provider.device_uuid().to_owned());let token=credentials.protocol_token()?;drop(credentials);drop(provider);
            let options=Options{booking,booking_schema:BookingSchema::AgentMessenger,checkin:Protection::LocoGcm(key.clone()),chat:Protection::LocoGcm(key),tls:TlsSettings::with_roots(trust),transport:Config::default(),stage_timeout:Duration::from_secs(15),total_timeout:Duration::from_secs(60),session_lease:Duration::from_secs(300)};
            let mut session=options.connect(&profile,token,&device,cancel.clone(),|phase,endpoint|{progress.protocol_enter(phase);println!("protocol/{}",progress.stage.get().label());proxy.dial(endpoint,cancel.clone())}).await?;drop(device);
            progress.completed(Stage::Loginlist);println!("LIVE LOGINLIST acknowledged; configured account identity validated for this run.");
            let operation=async{if args.read_only{progress.stage.set(Stage::RoomPage);let page=session.chat_rooms(Cursor::default()).await?;println!("Read-only page received: {} rooms; eof={}. No personal room data recorded.",page.rooms.len(),page.eof);}
                if let Some(seconds)=args.listen {
                    progress.stage.set(Stage::MessageReception);
                    let mut events=session.message_events()?;
                    let until=tokio::time::Instant::now()+Duration::from_secs(seconds);
                    let (mut messages,mut unknown,mut malformed)=(0_u64,0_u64,0_u64);
                    loop { tokio::select! { biased;
                        _=tokio::time::sleep_until(until)=>break,
                        event=events.recv(cancel.clone())=>match event? {
                            MessageEvent::Message(_)=>messages+=1,
                            MessageEvent::Unknown(_)=>unknown+=1,
                            MessageEvent::Malformed(_)=>malformed+=1,
                        }
                    }}
                    println!("Receive window complete: messages={messages}; unknown={unknown}; malformed={malformed}. No content or identities recorded.");
                }
                if let Some(seconds)=args.receive_one {
                    let room=test_room.ok_or_else(||fail("missing authorized test room"))?;
                    validation::receive_one(&session,room,Duration::from_secs(seconds),cancel.clone(),progress).await?;
                }
                Ok::<(),Failure>(())};
            let mut read_cancel=cancel.clone();
            let read=tokio::select!{biased;_=ui::cancelled(&mut read_cancel)=>Err(fail("post-login validation cancelled")),result=operation=>result};
            let failure_stage=progress.stage.get();
            progress.stage.set(Stage::Shutdown);
            let mut shutdown_cancel=cancel.clone();
            let closed=tokio::select!{biased;
                _=ui::cancelled(&mut shutdown_cancel)=>Err::<(),Failure>(fail("shutdown interrupted; clean close unverified")),
                result=session.disconnect_checked()=>result.map_err(|e|Box::new(e) as Failure),
            };
            if closed.is_ok(){progress.completed(Stage::Shutdown);}
            if let Err(error)=read {progress.stage.set(failure_stage);return Err(error);}
            closed?;progress.stage.set(Stage::Complete);println!("Checked local shutdown completed. No messages or moderation operations performed.");Ok::<(),Failure>(())
        }.await;
        observer.abort();let _=observer.await;signal.abort();let _=signal.await;result
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dry_run_cannot_enable_live_operations() {
        let a = |s: &[&str]| Arguments::parse(&s.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(a(&["--dry-run", "config", "--refresh"]).is_err());
        assert!(a(&["--dry-run", "config", "--read-only"]).is_err());
        assert!(a(&["--live", "config", "--refresh", "--refresh"]).is_err());
        assert!(a(&["--live", "config", "--read-only", "--report", "report.json"]).is_ok());
        assert!(a(&["--check-config", "config"]).unwrap().mode == Mode::DryRun);
        assert!(a(&["--dry-run", "config", "--listen", "10"]).is_err());
        assert!(a(&["--dry-run", "config", "--receive-one", "10"]).is_err());
        assert!(a(&["--live", "config", "--receive-one", "10", "--listen", "10"]).is_err());
        assert_eq!(
            a(&["--live", "config", "--receive-one", "10"])
                .unwrap()
                .receive_one,
            Some(10)
        );
        for invalid in ["0", "301", "-1", "text"] {
            assert!(a(&["--live", "config", "--listen", invalid]).is_err());
        }
        assert_eq!(
            a(&["--live", "config", "--listen", "10"]).unwrap().listen,
            Some(10)
        );
    }
    #[test]
    fn pin_changes_and_reports_are_sanitized() {
        let bytes = include_bytes!("../../../tests/fixtures/documented-server-public.der");
        let pin = format!("{:x}", Sha256::digest(bytes));
        assert!(pin_matches(bytes, &pin));
        assert!(!pin_matches(b"changed", &pin));
        let report = serde_json::to_string(&Report {
            mode: "dry-run",
            schema_version: 2,
            last_stage: Stage::ServerKey,
            milestones: Milestones::default(),
            outcome: "failed",
            live_connection_verified: false,
            controlled_receive_verified: false,
            failure_category: Some("server-rejected"),
            server_status: Some(-203),
            http_status: None,
        })
        .unwrap();
        for secret in ["token", "password", "approval", "userId", "email"] {
            assert!(!report.contains(secret));
        }
    }
    #[test]
    fn controlled_receive_checks_selected_room_freshness_and_type_without_content_matching() {
        use sculk_protocol::{Command, Packet};
        let room = sculk_client::read_only::RoomId::new(42).unwrap();
        let raw = include_bytes!("../../../tests/fixtures/node-msg-synthetic.bin");
        let packet = Packet::from_bytes(
            0,
            0,
            Command::new("MSG").unwrap(),
            raw[22..].to_vec().into(),
        )
        .unwrap();
        let mut doc = packet.document().unwrap();
        doc.insert("chatId", 42_i64);
        doc.remove("logId");
        doc.get_document_mut("chatLog")
            .unwrap()
            .insert("chatId", 42_i64);
        let packet = Packet::from_document(0, Command::new("MSG").unwrap(), &doc).unwrap();
        let message = sculk_client::messaging::Message::from_packet(packet).unwrap();
        assert!(validation::valid_controlled_message(
            &message, room, 1700000000
        ));
        assert!(!validation::valid_controlled_message(
            &message, room, 1700001000
        ));
        assert!(!validation::valid_controlled_message(
            &message,
            sculk_client::read_only::RoomId::new(43).unwrap(),
            1700000000
        ));
    }
    #[cfg(unix)]
    #[test]
    fn dry_run_validates_local_pin_without_identity_creation_or_live_milestones() {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let directory = std::env::temp_dir().join(format!("sculk-dry-run-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let key = include_bytes!("../../../tests/fixtures/documented-server-public.der");
        let key_path = directory.join("public.der");
        let config_path = directory.join("config.json");
        let identity = directory.join("not-created.device-identity");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_path)
            .unwrap();
        file.write_all(key).unwrap();
        let config = serde_json::json!({"expected_user_id":42,"email":"synthetic@example.invalid","device_file":identity,"server_key_file":key_path,"server_key_sha256":format!("{:x}",Sha256::digest(key)),"tcp_proxy_host":"127.0.0.1","tcp_proxy_port":1,"allowed_endpoints":[{"host":"booking-loco.kakao.com","port":443}],"app_version":"25.9.2","os_version":"13","api_level":"33","device_name":"SM-T870","test_room_id":42});
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&config_path)
            .unwrap();
        serde_json::to_writer(&mut file, &config).unwrap();
        drop(file);
        let args =
            Arguments::parse(&["--dry-run".into(), config_path.to_str().unwrap().into()]).unwrap();
        let progress = Progress::default();
        attempt(&args, &progress).unwrap();
        let m = progress.milestones();
        assert!(m.configuration && m.server_key);
        assert!(
            !m.https_authentication
                && !m.getconf
                && !m.loginlist
                && !m.controlled_message
                && !m.shutdown
        );
        assert!(!identity.exists());
        std::fs::remove_dir_all(&directory).unwrap();
    }
    #[test]
    fn diagnostic_categories_never_include_remote_content() {
        let e: Failure = auth::Error::InvalidCredentials(30).into();
        assert_eq!(failure_category(&e), "credentials-rejected");
        assert_eq!(status_codes(&e), (Some(30), None));
        let e: Failure =
            sculk_client::connect::Error::Rejected(sculk_client::connect::Stage::Checkin, -203)
                .into();
        assert_eq!(failure_category(&e), "server-rejected");
        let e = fail("secret peer payload must never be reported");
        assert_eq!(failure_category(&e), "local-validation-or-operation");
        assert_eq!(status_codes(&e), (None, None));
        let e: Failure = auth::Error::Http(403).into();
        assert_eq!(status_codes(&e), (None, Some(403)));
        let e: Failure = auth::Error::AccountMismatch.into();
        assert_eq!(status_codes(&e), (None, None));
    }
    #[tokio::test]
    async fn cancellation_waiter_is_level_triggered() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tx.send(false).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), ui::cancelled(&mut rx))
                .await
                .is_err()
        );
        tx.send(true).unwrap();
        ui::cancelled(&mut rx).await;
    }
}
