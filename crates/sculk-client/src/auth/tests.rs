use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
const USER: u64 = 9_007_199_254_740_993;
fn response(status: i64) -> String {
    format!(
        r#"{{"status":{status},"userId":{USER},"access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_in":3600}}"#
    )
}
async fn server(replies: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                bytes.push(socket.read_u8().await.unwrap());
                assert!(bytes.len() < 16384);
            }
            let header = String::from_utf8(bytes).unwrap();
            let length = header
                .lines()
                .find_map(|s| {
                    s.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            requests.push(format!("{header}{}", String::from_utf8(body).unwrap()));
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = socket.write_all(wire.as_bytes()).await;
        }
        requests
    });
    (format!("http://{address}/android/account/"), handle)
}
fn provider(base: String) -> Provider {
    let mut provider = Provider::new(
        AndroidProfile::documented(),
        DeviceIdentity::from_uuid("a".repeat(64)).unwrap(),
        USER,
        Limits::default(),
    )
    .unwrap();
    // Private unit-integration hook only; no production override or HTTP downgrade API.
    provider.base = base;
    provider.client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    provider
}
#[tokio::test]
async fn login_preserves_large_account_and_exact_source_fields() {
    let (base, task) = server(vec![response(0)]).await;
    let mut p = provider(base);
    let (_tx, cancel) = watch::channel(false);
    let credentials = p
        .login("owner@example.invalid", "synthetic-password", cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(credentials.user_id(), USER);
    assert!(credentials.has_server_expiration());
    assert_eq!(p.state(), State::CredentialsReady);
    assert!(!format!("{credentials:?}").contains("synthetic-access"));
    let request = task.await.unwrap().pop().unwrap();
    assert!(request.starts_with("POST /android/account/login.json HTTP/1.1"));
    assert!(request.contains("forced=false"));
    assert!(request.contains("permanent=true"));
    assert!(request.contains("device_name=SM-T870"));
    assert!(request.to_lowercase().contains("x-vc:"));
    let packet = p.loco_profile().checkin_request(USER).unwrap();
    assert_eq!(
        packet.document().unwrap().get_i64("userId").unwrap(),
        USER as i64
    );
    assert_eq!(credentials.protocol_token().unwrap().user_id(), USER);
}
#[tokio::test]
async fn invalid_rejected_mismatched_and_malformed_login_are_terminal() {
    for (reply, expected) in [
        (response(12), Error::InvalidCredentials(12)),
        (response(-999), Error::Rejected(-999)),
        (
            response(0).replace(&USER.to_string(), "42"),
            Error::AccountMismatch,
        ),
        (
            r#"{"status":0,"status":0,"userId":42}"#.into(),
            Error::Response,
        ),
        (
            format!(r#"{{"status":0,"userId":{USER}.0,"access_token":"synthetic"}}"#),
            Error::Response,
        ),
    ] {
        let (base, task) = server(vec![reply]).await;
        let mut p = provider(base);
        let (_tx, cancel) = watch::channel(false);
        assert_eq!(
            p.login(
                "owner@example.invalid",
                "synthetic-password",
                cancel.clone()
            )
            .await
            .err(),
            Some(expected.clone())
        );
        assert_eq!(p.state(), State::Failed(expected));
        assert_eq!(
            p.login("owner@example.invalid", "synthetic-password", cancel)
                .await
                .err(),
            Some(Error::State)
        );
        assert_eq!(task.await.unwrap().len(), 1);
    }
}
#[tokio::test]
async fn explicit_approval_phone_confirmation_then_one_login() {
    let (base, task) = server(vec![
        "{\"status\":-100}".into(),
        "{\"status\":0,\"passcode\":\"123456\",\"remainingSeconds\":30}".into(),
        "{\"status\":0}".into(),
        response(0),
    ])
    .await;
    let mut p = provider(base);
    let (_tx, cancel) = watch::channel(false);
    assert!(p
        .login(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone()
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(p.state(), State::NeedsApproval);
    assert_eq!(
        p.request_challenge(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone()
        )
        .await
        .err(),
        Some(Error::ApprovalRequired)
    );
    p.approve_device_registration().unwrap();
    assert_eq!(
        p.request_challenge(
            "other@example.invalid",
            "synthetic-password",
            cancel.clone()
        )
        .await
        .err(),
        Some(Error::AccountMismatch)
    );
    let challenge = p
        .request_challenge(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap();
    assert_eq!(challenge.expose_code(), "123456");
    assert!(!format!("{challenge:?}").contains("123456"));
    p.confirm_device(
        "owner@example.invalid",
        "synthetic-password",
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(p.state(), State::Registered);
    p.login("owner@example.invalid", "synthetic-password", cancel)
        .await
        .unwrap()
        .unwrap();
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].starts_with("POST /android/account/passcodeLogin/generate"));
    assert!(requests[2].starts_with("POST /android/account/passcodeLogin/registerDevice"));
    let json: serde_json::Value =
        serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(json["device"]["osVersion"], "33");
}
#[tokio::test]
async fn registration_rejection_stops_without_another_login() {
    let (base, task) = server(vec![
        "{\"status\":-100}".into(),
        "{\"passcode\":\"123456\"}".into(),
        "{\"status\":-110}".into(),
    ])
    .await;
    let mut p = provider(base);
    let (_tx, cancel) = watch::channel(false);
    p.login(
        "owner@example.invalid",
        "synthetic-password",
        cancel.clone(),
    )
    .await
    .unwrap();
    p.approve_device_registration().unwrap();
    p.request_challenge(
        "owner@example.invalid",
        "synthetic-password",
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        p.confirm_device(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone()
        )
        .await,
        Err(Error::Rejected(-110))
    );
    assert_eq!(
        p.login("owner@example.invalid", "synthetic-password", cancel)
            .await
            .err(),
        Some(Error::State)
    );
    assert_eq!(task.await.unwrap().len(), 3);
}
#[tokio::test]
async fn refresh_rotates_tokens_and_keeps_account_binding() {
    let(base,task)=server(vec![response(0),"{\"status\":0,\"access_token\":\"rotated-access\",\"refresh_token\":\"rotated-refresh\",\"expires_in\":60}".into()]).await;
    let mut p = provider(base);
    let (_tx, cancel) = watch::channel(false);
    let c = p
        .login(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let next = p.refresh(&c, cancel.clone()).await.unwrap();
    assert_eq!(next.user, USER);
    assert_eq!(next.access.as_str(), "rotated-access");
    assert_eq!(next.refresh.as_ref().unwrap().as_str(), "rotated-refresh");
    assert_eq!(p.refresh(&next, cancel).await.err(), Some(Error::State));
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].to_lowercase().contains(&format!(
        "authorization: synthetic-access-{}",
        "a".repeat(64)
    )));
    let json: serde_json::Value =
        serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(json["grant_type"], "refresh_token");
}
#[tokio::test]
async fn refresh_mismatch_rejection_expiration_and_missing_refresh() {
    for (refresh, expected) in [
        ("{\"status\":-950}".into(), Error::Rejected(-950)),
        (
            r#"{"status":0,"userId":42,"access_token":"other"}"#.to_owned(),
            Error::AccountMismatch,
        ),
        (
            "{\"status\":0,\"access_token\":\"expired\",\"expires_in\":0}".into(),
            Error::Expired,
        ),
    ] {
        let (base, task) = server(vec![response(0), refresh]).await;
        let mut p = provider(base);
        let (_tx, cancel) = watch::channel(false);
        let c = p
            .login(
                "owner@example.invalid",
                "synthetic-password",
                cancel.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(p.refresh(&c, cancel).await.err(), Some(expected));
        task.await.unwrap();
    }
    let (base, task) = server(vec![format!(
        r#"{{"status":0,"userId":{USER},"access_token":"synthetic"}}"#
    )])
    .await;
    let mut p = provider(base);
    let (_tx, cancel) = watch::channel(false);
    let c = p
        .login(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!c.has_server_expiration());
    assert_eq!(
        p.refresh(&c, cancel).await.err(),
        Some(Error::RefreshUnavailable)
    );
    task.await.unwrap();
}
#[tokio::test]
async fn timeout_cancellation_and_dropped_login_are_terminal() {
    for cancelled in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let mut p = provider(base);
        p.limits.request_timeout = Duration::from_millis(20);
        let (tx, cancel) = watch::channel(false);
        if cancelled {
            tx.send(true).unwrap();
        }
        assert_eq!(
            p.login("owner@example.invalid", "synthetic-password", cancel)
                .await
                .err(),
            Some(if cancelled {
                Error::Cancelled
            } else {
                Error::Timeout
            })
        );
    }
    let mut p = provider("http://127.0.0.1:1/".into());
    let (_tx, cancel) = watch::channel(false);
    let state = p.subscribe();
    {
        let future = p.login("owner@example.invalid", "synthetic-password", cancel);
        tokio::pin!(future);
        tokio::select! {biased;_=&mut future=>{},_=tokio::task::yield_now()=>{}}
    }
    assert!(matches!(
        *state.borrow(),
        State::Failed(Error::Cancelled | Error::Transport)
    ));
}
#[tokio::test]
async fn local_expiry_is_explicit_and_protocol_token_refused() {
    let (base, task) = server(vec![response(0)]).await;
    let mut p = provider(base);
    p.limits.token_age = Duration::from_millis(5);
    let (_tx, cancel) = watch::channel(false);
    let c = p
        .login("owner@example.invalid", "synthetic-password", cancel)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(p.check_expiration(&c), Err(Error::Expired));
    assert_eq!(c.protocol_token().err(), Some(Error::Expired));
    task.await.unwrap();
}
#[tokio::test]
async fn http_response_body_is_bounded_and_redirect_not_followed() {
    for wire in [
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\n\r\n"
            .to_owned(),
        "HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n".to_owned(),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut p = provider(format!("http://{}/", listener.local_addr().unwrap()));
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut b = [0; 4096];
            assert!(s.read(&mut b).await.unwrap() > 0);
            s.write_all(wire.as_bytes()).await.unwrap();
        });
        let (_tx, cancel) = watch::channel(false);
        assert!(matches!(
            p.login("owner@example.invalid", "synthetic-password", cancel)
                .await
                .err(),
            Some(Error::Http(302) | Error::Response)
        ));
        task.await.unwrap();
    }
}
#[test]
fn persistent_identity_is_private_stable_and_symlink_safe() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = std::env::temp_dir().join(format!("sculk-identity-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join("device");
    let first = DeviceIdentity::load_or_create(&path).unwrap();
    let second = DeviceIdentity::load_or_create(&path).unwrap();
    assert_eq!(first.expose_uuid(), second.expose_uuid());
    assert_eq!(first.expose_uuid().len(), 64);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let link = dir.join("link");
    symlink(&path, &link).unwrap();
    assert!(read_private_file(&link, 256).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(DeviceIdentity::load_or_create(&path).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn http_provider_to_tls_booking_gcm_checkin_and_login_exact_account() {
    use crate::{
        connect::{BookingSchema, Endpoint, Options, Protection, Stage},
        records::SecureStream,
        tls::{TlsSettings, TrustStore},
        Config,
    };
    use futures_util::{SinkExt, StreamExt};
    use rsa::{pkcs1::EncodeRsaPublicKey, pkcs8::DecodePrivateKey, Oaep};
    use sculk_protocol::{handshake::ServerKey, secure::GcmRecords, Command, LocoCodec, Packet};
    use std::sync::Arc;
    use tokio_util::codec::Framed;
    let (base, http) = server(vec![response(0)]).await;
    let mut provider = provider(base);
    let (_tx, cancel) = watch::channel(false);
    let credentials = provider
        .login(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    http.await.unwrap();
    let private = Arc::new(
        rsa::RsaPrivateKey::from_pkcs8_pem(include_str!(
            "../../../../tests/fixtures/synthetic-rsa-private.pem"
        ))
        .unwrap(),
    );
    let pin = ServerKey::from_pkcs1_der(private.to_public_key().to_pkcs1_der().unwrap().as_bytes())
        .unwrap();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let mut roots = TrustStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der())
            .into(),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let (b1, b2) = tokio::io::duplex(16384);
    let (c1, c2) = tokio::io::duplex(16384);
    let (l1, l2) = tokio::io::duplex(16384);
    let booking = tokio::spawn(async move {
        let mut f = Framed::new(acceptor.accept(b2).await.unwrap(), LocoCodec::default());
        let p = f.next().await.unwrap().unwrap();
        assert_eq!(p.command.as_str(), "GETCONF");
        assert_eq!(p.document().unwrap().get_str("os").unwrap(), "android");
        let mut r = Packet::from_document(
            p.id,
            Command::new("GETCONF").unwrap(),
            &bson::doc! {"hosts":["checkin.example"],"wifi":{"ports":[995]}},
        )
        .unwrap();
        r.id = p.id;
        f.send(r).await.unwrap();
        let _ = f.next().await;
    });
    let mut peers = Vec::new();
    for (index, mut socket) in [c2, l2].into_iter().enumerate() {
        let private = private.clone();
        peers.push(tokio::spawn(async move {
            let mut h = [0; 268];
            socket.read_exact(&mut h).await.unwrap();
            assert_eq!(&h[..12], &[0, 1, 0, 0, 16, 0, 0, 0, 3, 0, 0, 0]);
            let key = private
                .decrypt(Oaep::new::<sha1::Sha1>(), &h[12..])
                .unwrap();
            let stream = SecureStream::from_codec(
                socket,
                GcmRecords::new(key.as_slice().try_into().unwrap()),
            );
            let mut f = Framed::new(stream, LocoCodec::default());
            let p = f.next().await.unwrap().unwrap();
            let body = p.document().unwrap();
            let reply = if index == 0 {
                assert_eq!(p.command.as_str(), "CHECKIN");
                assert_eq!(body.get_i64("userId").unwrap(), USER as i64);
                bson::doc! {"host":"changed.chat.example","port":5223}
            } else {
                assert_eq!(p.command.as_str(), "LOGINLIST");
                assert_eq!(body.get_str("oauthToken").unwrap(), "synthetic-access");
                assert_eq!(body.get_str("os").unwrap(), "android");
                bson::doc! {"status":0,"userId":USER as i64,"chatDatas":[],"eof":true}
            };
            let r = Packet::from_document(p.id, p.command, &reply).unwrap();
            let mut encoded = bytes::BytesMut::new();
            use tokio_util::codec::Encoder;
            LocoCodec::default().encode(r, &mut encoded).unwrap();
            for part in encoded.chunks(7) {
                f.get_mut().write_all(part).await.unwrap();
                f.get_mut().flush().await.unwrap();
            }
            let _ = f.next().await;
        }));
    }
    let options = Options {
        booking: Endpoint::new("localhost", 443).unwrap(),
        booking_schema: BookingSchema::AgentMessenger,
        checkin: Protection::LocoGcm(pin.clone()),
        chat: Protection::LocoGcm(pin),
        tls: TlsSettings::with_roots(roots),
        transport: Config::default(),
        stage_timeout: Duration::from_secs(2),
        total_timeout: Duration::from_secs(5),
        session_lease: Duration::from_secs(30),
    };
    let mut streams = std::collections::VecDeque::from([b1, c1, l1]);
    let mut session = options
        .connect(
            &provider.loco_profile(),
            credentials.protocol_token().unwrap(),
            provider.device_uuid(),
            cancel,
            |stage, endpoint| {
                assert_eq!(
                    endpoint.host(),
                    match stage {
                        Stage::Booking => "localhost",
                        Stage::Checkin => "checkin.example",
                        Stage::Login => "changed.chat.example",
                    }
                );
                let stream = streams.pop_front().unwrap();
                async { Ok(stream) }
            },
        )
        .await
        .unwrap();
    assert_eq!(session.state(), crate::session::State::Authenticated);
    session.disconnect().await;
    booking.await.unwrap();
    for peer in peers {
        peer.await.unwrap();
    }
}
#[tokio::test]
async fn interrupted_http_and_stalled_body_never_report_success() {
    for stalled in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut p = provider(format!("http://{}/", listener.local_addr().unwrap()));
        p.limits.request_timeout = Duration::from_millis(20);
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            assert!(s.read(&mut buffer).await.unwrap() > 0);
            if stalled {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"status\":")
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let (_tx, cancel) = watch::channel(false);
        let e = p
            .login("owner@example.invalid", "synthetic-password", cancel)
            .await
            .err()
            .unwrap();
        assert!(matches!(e, Error::Transport | Error::Timeout));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn requests_match_source_generated_android_auth_fixtures() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/agent-android-auth-requests.json"
    ))
    .unwrap();
    let (base, task) = server(vec![
        "{\"status\":-100}".into(),
        "{\"status\":0,\"passcode\":\"123456\"}".into(),
        "{\"status\":0}".into(),
        response(0),
        "{\"status\":0,\"access_token\":\"rotated\",\"expires_in\":60}".into(),
    ])
    .await;
    let mut p = provider(base);
    let (_tx, cancel) = watch::channel(false);
    p.login(
        "owner@example.invalid",
        "synthetic-password",
        cancel.clone(),
    )
    .await
    .unwrap();
    p.approve_device_registration().unwrap();
    p.request_challenge(
        "owner@example.invalid",
        "synthetic-password",
        cancel.clone(),
    )
    .await
    .unwrap();
    p.confirm_device(
        "owner@example.invalid",
        "synthetic-password",
        cancel.clone(),
    )
    .await
    .unwrap();
    let c = p
        .login(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    p.refresh(&c, cancel).await.unwrap();
    let requests = task.await.unwrap();
    for (index, reference) in [0usize, 1, 2, 4]
        .into_iter()
        .zip(fixtures.as_array().unwrap())
    {
        let raw = &requests[index];
        let (header, body) = raw.split_once("\r\n\r\n").unwrap();
        for name in ["A", "User-Agent", "Accept-Language"] {
            let value = reference["headers"][name].as_str().unwrap();
            assert!(header.to_lowercase().contains(&format!(
                "{}: {}",
                name.to_lowercase(),
                value.to_lowercase()
            )));
        }
        if index == 0 {
            assert_eq!(body, reference["body"].as_str().unwrap());
            let xvc = reference["headers"]["X-VC"].as_str().unwrap();
            assert!(header.to_lowercase().contains(&format!("x-vc: {xvc}")));
        } else {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(body).unwrap(),
                serde_json::from_str::<serde_json::Value>(reference["body"].as_str().unwrap())
                    .unwrap()
            );
        }
    }
}
#[tokio::test]
async fn approval_poll_budget_and_expired_window_prevent_repeat_requests() {
    for expired in [false, true] {
        let mut replies = vec![
            "{\"status\":-100}".into(),
            "{\"status\":0,\"passcode\":\"123456\"}".into(),
        ];
        if !expired {
            replies.push("{\"status\":-100,\"nextRequestIntervalInSeconds\":1}".into());
        }
        let (base, task) = server(replies).await;
        let mut p = provider(base);
        p.limits.registration_polls = 1;
        let (_tx, cancel) = watch::channel(false);
        p.login(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap();
        p.approve_device_registration().unwrap();
        p.request_challenge(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap();
        if expired {
            p.challenge_deadline = Some(Instant::now() - Duration::from_secs(1));
        }
        assert_eq!(
            p.confirm_device(
                "owner@example.invalid",
                "synthetic-password",
                cancel.clone()
            )
            .await,
            Err(Error::Timeout)
        );
        assert_eq!(
            p.confirm_device("owner@example.invalid", "synthetic-password", cancel)
                .await,
            Err(Error::State)
        );
        assert_eq!(task.await.unwrap().len(), if expired { 2 } else { 3 });
    }
}
#[tokio::test]
async fn expired_credentials_can_refresh_once_and_cancellation_wins_ready_reply() {
    let (base, task) = server(vec![
        response(0),
        "{\"status\":0,\"access_token\":\"refreshed\",\"expires_in\":60}".into(),
    ])
    .await;
    let mut p = provider(base);
    let (_tx, cancel) = watch::channel(false);
    let mut c = p
        .login(
            "owner@example.invalid",
            "synthetic-password",
            cancel.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    c.expires = Instant::now() - Duration::from_secs(1);
    p.credential_deadline = Some(c.expires);
    assert_eq!(p.state(), State::Expired);
    p.refresh(&c, cancel)
        .await
        .unwrap()
        .protocol_token()
        .unwrap();
    task.await.unwrap();
    let mut p = provider("http://127.0.0.1:1/".into());
    let (tx, cancel) = watch::channel(false);
    tx.send(true).unwrap();
    assert_eq!(
        p.login("owner@example.invalid", "synthetic-password", cancel)
            .await
            .err(),
        Some(Error::Cancelled)
    );
}
