//! Opt-in local validation tool. Help/dry-run perform no network operations.
mod ui;
use sculk_client::{
    auth::{self, AndroidProfile, DeviceIdentity, Limits, Provider},
    connect::{BookingSchema, Endpoint, Options, Protection, Stage},
    dial::ConnectProxy,
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
    report: Option<PathBuf>,
}
impl Arguments {
    fn parse(args: &[String]) -> Result<Self, Failure> {
        let mut result = Self {
            mode: Mode::Help,
            config: None,
            refresh: false,
            read_only: false,
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
    live_connection_verified: bool,
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
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Failure> {
    let args = Arguments::parse(&std::env::args().skip(1).collect::<Vec<_>>())?;
    if args.mode == Mode::Help {
        println!("Sculk experimental connection validation\n--dry-run PATH [--report NEW_PATH]: local config/key checks only; no network\n--check-config is an alias for --dry-run\n--live PATH [--refresh] [--read-only] [--report NEW_PATH]: explicitly authorized account only\nLive mode requires a terminal, account approval, and separate device-registration approval.\n--read-only requests one LCHATLIST page; no message, read mark or moderation operation.\nReports contain only mode/stage/outcome and never secrets, IDs or approval codes.");
        return Ok(());
    }
    let stage = Cell::new("configuration");
    let verified = Cell::new(false);
    let result = attempt(&args, &stage, &verified);
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
            let operation=async{if args.read_only{stage.set("read-only-room-page");let page=session.chat_rooms(Cursor::default()).await?;println!("Read-only page received: {} rooms; eof={}. No personal room data recorded.",page.rooms.len(),page.eof);}Ok::<(),Failure>(())};
            let mut read_cancel=cancel.clone();
            let read=tokio::select!{biased;_=ui::cancelled(&mut read_cancel)=>Err(fail("post-login validation cancelled")),result=operation=>result};
            stage.set(if read.is_ok(){"disconnect"}else{"read-only-room-page"});session.disconnect().await;
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
        })
        .unwrap();
        for secret in ["token", "password", "approval", "userId", "email"] {
            assert!(!report.contains(secret));
        }
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
