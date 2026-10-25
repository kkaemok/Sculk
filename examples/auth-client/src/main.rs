//! Explicitly opt-in live example. Default/help/check-config never contacts Kakao.
use sculk_client::{
    auth::{self, AndroidProfile, DeviceIdentity, Limits, Provider},
    connect::{BookingSchema, Endpoint, Options, Protection},
    dial::ConnectProxy,
    tls::{TlsSettings, TrustStore},
    Config,
};
use sculk_protocol::handshake::ServerKey;
use serde::Deserialize;
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    expected_user_id: u64,
    email: String,
    device_file: PathBuf,
    server_key_file: PathBuf,
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
fn fail(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
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
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args == ["--help"] {
        println!("Sculk experimental authentication example\n--check-config PATH: private-file validation only, no network\n--live PATH [--refresh]: authorized account login and protocol handshake\nLive mode requires a terminal and explicit test-account approval. Registration asks separately.\nNo credentials, public-key pin or account are chosen automatically.");
        return Ok(());
    }
    let live = args.first().is_some_and(|s| s == "--live");
    let check = args.first().is_some_and(|s| s == "--check-config");
    if !(live || check)
        || !(2..=3).contains(&args.len())
        || args.len() == 3 && (!live || args[2] != "--refresh")
    {
        return Err(fail("invalid arguments; use --help"));
    }
    let bytes = auth::read_private_file(Path::new(&args[1]), 16384)?;
    let config: Configuration =
        serde_json::from_slice(&bytes).map_err(|_| fail("invalid private configuration"))?;
    if config.expected_user_id == 0
        || config.expected_user_id > i64::MAX as u64
        || config.email.is_empty()
        || config.email.len() > 320
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
    let allowed: Vec<_> = config
        .allowed_endpoints
        .iter()
        .map(|d| Endpoint::new(&d.host, d.port))
        .collect::<Result<_, _>>()?;
    let booking = Endpoint::new("booking-loco.kakao.com", 443)?;
    if !allowed.contains(&booking) {
        return Err(fail("allowlist must include the Booking endpoint"));
    }
    let proxy = ConnectProxy::new(
        config.tcp_proxy_host,
        config.tcp_proxy_port,
        allowed,
        Duration::from_secs(15),
    )?;
    let key = auth::read_private_file(&config.server_key_file, 4096)?;
    if key.len() > 4096 {
        return Err(fail("public-key file too large"));
    }
    let key =
        ServerKey::from_pkcs1_der(&key).map_err(|_| fail("invalid trusted public-key file"))?;
    let roots = rustls_native_certs::load_native_certs();
    let mut trust = TrustStore::empty();
    for cert in roots.certs {
        trust.add(cert).map_err(|_| fail("invalid system CA"))?;
    }
    if trust.is_empty() {
        return Err(fail("system trust store unavailable"));
    }
    // Check-only mode must not generate an identity or ask for account secrets.
    if check {
        if !config.device_file.is_absolute() {
            return Err(fail("device_file must be an absolute path"));
        }
        println!("Configuration and public key validated locally. No network or authentication performed.");
        return Ok(());
    }
    if !ask("Authorize this configured test account for experimental login and LOCO connection? No message will be sent.","authorize"){return Err(fail("test account authorization not granted; no network operation"));}
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
    let refresh = args.len() == 3;
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move{
        let (cancel_tx,cancel)=tokio::sync::watch::channel(false);
        let signal=tokio::spawn(async move{let _=tokio::signal::ctrl_c().await;cancel_tx.send_replace(true);});
        let result=async{
            let credentials=match provider.login(&email,&password,cancel.clone()).await?{
                Some(c)=>c,
                None=>{
                    let approved=tokio::task::spawn_blocking(||ask("The server requires approval of a new sub-device. Authorize challenge generation and bounded registration polling?","register")).await?;
                    if !approved{return Err(fail("device registration was not approved"));}
                    provider.approve_device_registration()?;
                    let challenge=provider.request_challenge(&email,&password,cancel.clone()).await?;
                    // Explicit, transient human display; never print the raw login response.
                    println!("Confirm this code on your legitimate device: {}",challenge.expose_code());
                    provider.confirm_device(&email,&password,cancel.clone()).await?;
                    provider.login(&email,&password,cancel.clone()).await?.ok_or_else(||fail("device approval did not complete"))?
                }
            };
            let credentials=if refresh{provider.refresh(&credentials,cancel.clone()).await?}else{credentials};
            println!("HTTP provider accepted the configured account; LOCO identity remains to be validated.");
            if !credentials.has_server_expiration(){println!("Server expiry absent: enforcing an explicit local credential-age cap.");}
            let options=Options{booking,booking_schema:BookingSchema::AgentMessenger,checkin:Protection::LocoGcm(key.clone()),chat:Protection::LocoGcm(key),tls:TlsSettings::with_roots(trust),transport:Config::default(),stage_timeout:Duration::from_secs(15),total_timeout:Duration::from_secs(60),session_lease:Duration::from_secs(300)};
            let mut session=options.connect(&provider.loco_profile(),credentials.protocol_token()?,provider.device_uuid(),cancel.clone(),|_,endpoint|proxy.dial(endpoint,cancel.clone())).await?;
            println!("LIVE LOGINLIST acknowledged and exact account identity validated for this run. No message sent.");
            session.disconnect().await;println!("Connection closed cleanly.");
            Ok::<(),Box<dyn std::error::Error+Send+Sync>>(())
        }.await;
        signal.abort();let _=signal.await;result
    })
}
