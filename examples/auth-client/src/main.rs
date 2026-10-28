//! Opt-in local validation tool. Help/dry-run perform no network operations.
mod ui;
use sculk_client::{
    auth::{self, AndroidProfile, DeviceIdentity, Limits, Provider},
    connect::{BookingSchema, Endpoint, Options, Protection, Stage},
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
    cell::Cell,
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
    last_stage: &'static str,
    outcome: &'static str,
    failure_category: Option<&'static str>,
    live_connection_verified: bool,
    controlled_receive_verified: bool,
}
// Only typed, static categories enter reports. Never format raw error/source text.
fn failure_category(error: &Failure) -> &'static str {
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
            Dial(_) | DialIo(_, _) => "dial-or-policy",
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
fn fail(message: &str) -> Failure {
    io::Error::other(message).into()
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
fn valid_controlled_message(
    message: &sculk_client::messaging::Message,
    room: sculk_client::read_only::RoomId,
    now: u64,
) -> bool {
    message.room == room
        && message.id.get() > 0
        && message.sender.get() > 0
        && message.kind == sculk_client::messaging::MessageType::Text
        && u64::try_from(message.sent_at_seconds).is_ok_and(|sent| sent.abs_diff(now) <= 300)
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Failure> {
    let args = Arguments::parse(&std::env::args().skip(1).collect::<Vec<_>>())?;
    if args.mode == Mode::Help {
        println!("Sculk experimental connection validation\n--dry-run PATH [--report NEW_PATH]: local config/key checks only; no network\n--check-config is an alias for --dry-run\n--live PATH [--refresh] [--read-only] [--listen SECONDS | --receive-one SECONDS] [--report NEW_PATH]: explicitly authorized account only\nLive mode requires a terminal, account approval, and separate device-registration approval.\n--receive-one requires configured test_room_id and separate room consent; validates one fresh text without logging content or IDs.\n--read-only requests one LCHATLIST page; --listen 1..300 receives events and prints counts only. No messages, read marks or moderation operations are sent.\nReports contain only mode/stage/outcome and never secrets, IDs or approval codes.");
        return Ok(());
    }
    let stage = Cell::new("configuration");
    let verified = Cell::new(false);
    let received = Cell::new(false);
    let result = attempt(&args, &stage, &verified, &received);
    if let Some(path) = &args.report {
        write_report(
            path,
            &Report {
                mode: if args.mode == Mode::Live {
                    "live"
                } else {
                    "dry-run"
                },
                last_stage: stage.get(),
                outcome: if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
                live_connection_verified: verified.get(),
                controlled_receive_verified: received.get(),
                failure_category: result.as_ref().err().map(failure_category),
            },
        )?;
    }
    if result.is_err() {
        eprintln!(
            "Failed stage: {}. No automatic retry was performed.",
            stage.get()
        );
    }
    result
}
fn attempt(
    args: &Arguments,
    stage: &Cell<&'static str>,
    verified: &Cell<bool>,
    received: &Cell<bool>,
) -> Result<(), Failure> {
    let bytes = auth::read_private_file(
        args.config
            .as_ref()
            .ok_or_else(|| fail("missing configuration"))?,
        16384,
    )?;
    let config: Configuration = serde_json::from_slice(&bytes).map_err(|_| {
        fail("invalid private configuration (required fields include server_key_sha256)")
    })?;
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
    stage.set("key-identity");
    let key = auth::read_private_file(&config.server_key_file, 4096)?;
    if !pin_matches(&key, &config.server_key_sha256) {
        return Err(fail(
            "RSA public-key fingerprint mismatch; verify provenance before any login",
        ));
    }
    let key = ServerKey::from_pkcs1_der(&key).map_err(|_| fail("invalid RSA public key"))?;
    let mut trust = TrustStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        trust.add(cert).map_err(|_| fail("invalid system CA"))?;
    }
    if trust.is_empty() {
        return Err(fail("system trust store unavailable"));
    }
    if args.mode == Mode::DryRun {
        stage.set("local-validation-complete");
        println!("LOCAL DRY RUN complete. No network requests, authentication or device identity creation. Live compatibility remains unverified.");
        return Ok(());
    }
    stage.set("account-authorization");
    if !ask("Authorize this configured test account for experimental authentication and connection? No messages or moderation operations will be sent.","authorize"){return Err(fail("test-account authorization not granted; no network request"));}
    if args.receive_one.is_some() && !ask("Authorize receive-only validation in the configured test room you own or have permission to test?", "receive") {
        return Err(fail("test-room authorization not granted; no network request"));
    }
    stage.set("device-identity");
    let identity = DeviceIdentity::load_or_create(&config.device_file)?;
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
            stage.set("http-login");let credentials=match provider.login(&email,&password,cancel.clone()).await?{Some(c)=>c,None=>{
                stage.set("registration-approval");if !ui::registration_approval(cancel.clone()).await.map_err(|_|fail("registration approval cancelled, timed out or unavailable"))?{return Err(fail("device registration not approved"));}
                provider.approve_device_registration()?;stage.set("device-challenge");let challenge=provider.request_challenge(&email,&password,cancel.clone()).await?;
                ui::show_challenge(challenge.expose_code()).map_err(|_|fail("cannot display approval code privately"))?;drop(challenge);
                stage.set("device-confirmation");provider.confirm_device(&email,&password,cancel.clone()).await?;
                stage.set("post-registration-login");provider.login(&email,&password,cancel.clone()).await?.ok_or_else(||fail("device approval incomplete"))?
            }};
            drop(password);drop(email);
            let credentials=if args.refresh{stage.set("explicit-refresh");let next=provider.refresh(&credentials,cancel.clone()).await?;drop(credentials);next}else{credentials};
            if !credentials.has_server_expiration(){println!("Using an explicit local credential-age cap; server expiry was absent.");}
            let profile=provider.loco_profile();let device=Zeroizing::new(provider.device_uuid().to_owned());let token=credentials.protocol_token()?;drop(credentials);drop(provider);
            let options=Options{booking,booking_schema:BookingSchema::AgentMessenger,checkin:Protection::LocoGcm(key.clone()),chat:Protection::LocoGcm(key),tls:TlsSettings::with_roots(trust),transport:Config::default(),stage_timeout:Duration::from_secs(15),total_timeout:Duration::from_secs(60),session_lease:Duration::from_secs(300)};
            let mut session=options.connect(&profile,token,&device,cancel.clone(),|phase,endpoint|{let name=match phase{Stage::Booking=>"booking",Stage::Checkin=>"checkin",Stage::Login=>"loginlist"};stage.set(name);println!("protocol/{name}");proxy.dial(endpoint,cancel.clone())}).await?;drop(device);
            verified.set(true);println!("LIVE LOGINLIST acknowledged; configured account identity validated for this run.");
            let operation=async{if args.read_only{stage.set("read-only-room-page");let page=session.chat_rooms(Cursor::default()).await?;println!("Read-only page received: {} rooms; eof={}. No personal room data recorded.",page.rooms.len(),page.eof);}
                if let Some(seconds)=args.listen {
                    stage.set("receive-events");
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
                    stage.set("controlled-receive");
                    let room=test_room.ok_or_else(||fail("missing authorized test room"))?;
                    let mut events=session.message_events()?;
                    println!("READY: send one controlled text in the authorized room using the official client. No content or identifiers will be printed.");
                    let deadline=tokio::time::Instant::now()+Duration::from_secs(seconds);
                    loop {tokio::select!{biased;
                        _=tokio::time::sleep_until(deadline)=>return Err(fail("controlled receive timed out")),
                        event=events.recv(cancel.clone())=>match event? {
                            MessageEvent::Message(message) if message.room==room=>{
                                let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|fail("system clock unavailable"))?.as_secs();
                                if !valid_controlled_message(&message,room,now) {return Err(fail("controlled message failed room, ID, timestamp or type validation"));}
                                received.set(true);println!("Controlled receive passed: room, log ID, sender ID, timestamp and text type validated. No content or identities recorded.");break;
                            },
                            MessageEvent::Message(_)=>{},
                            MessageEvent::Unknown(_)|MessageEvent::Malformed(_)=>return Err(fail("unsupported event during controlled validation")),
                        }
                    }}
                }
                Ok::<(),Failure>(())};
            let mut read_cancel=cancel.clone();
            let read=tokio::select!{biased;_=ui::cancelled(&mut read_cancel)=>Err(fail("post-login validation cancelled")),result=operation=>result};
            if read.is_ok(){stage.set("disconnect");}session.disconnect().await;
            read?;stage.set("complete");println!("Connection closed cleanly. No messages or moderation operations performed.");Ok::<(),Failure>(())
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
            last_stage: "key-identity",
            outcome: "failed",
            live_connection_verified: false,
            controlled_receive_verified: false,
            failure_category: Some("server-rejected"),
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
        assert!(valid_controlled_message(&message, room, 1700000000));
        assert!(!valid_controlled_message(&message, room, 1700001000));
        assert!(!valid_controlled_message(
            &message,
            sculk_client::read_only::RoomId::new(43).unwrap(),
            1700000000
        ));
    }
    #[test]
    fn diagnostic_categories_never_include_remote_content() {
        let e: Failure = auth::Error::InvalidCredentials(30).into();
        assert_eq!(failure_category(&e), "credentials-rejected");
        let e: Failure = sculk_client::connect::Error::Rejected(Stage::Checkin, -203).into();
        assert_eq!(failure_category(&e), "server-rejected");
        let e = fail("secret peer payload must never be reported");
        assert_eq!(failure_category(&e), "local-validation-or-operation");
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
