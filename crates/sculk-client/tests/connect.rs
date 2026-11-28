#![cfg(feature = "protocol-connect")]
mod support;
use bson::doc;
use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use rsa::{pkcs1::EncodeRsaPublicKey, pkcs8::DecodePrivateKey, PrivateKey};
use sculk_client::{
    connect::{self, Booking, BookingSchema, DeviceProfile, Endpoint, Options, Protection, Stage},
    profile::{Connector, Profile, Settings},
    records::SecureStream,
    session::{AccessToken, AuthError, State},
    Config,
};
use sculk_protocol::{handshake::ServerKey, secure::GcmRecords, Command, LocoCodec, Packet};
use std::{
    collections::VecDeque,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::watch,
    time::{timeout, Instant},
};
use tokio_util::codec::{Decoder, Encoder, Framed};

fn private() -> Arc<PrivateKey> {
    static KEY: OnceLock<Arc<PrivateKey>> = OnceLock::new();
    KEY.get_or_init(|| {
        Arc::new(
            PrivateKey::from_pkcs8_pem(include_str!(
                "../../../tests/fixtures/synthetic-rsa-private.pem"
            ))
            .unwrap(),
        )
    })
    .clone()
}
fn key() -> ServerKey {
    ServerKey::from_pkcs1_der(private().to_public_key().to_pkcs1_der().unwrap().as_bytes()).unwrap()
}
fn profile() -> DeviceProfile {
    DeviceProfile {
        os: "mac".into(),
        app_version: "26.2.0".into(),
        protocol_version: "1".into(),
        mccmnc: "99999".into(),
        language: "ko".into(),
        country_iso: "KR".into(),
        use_sub: false,
    }
}
fn token() -> AccessToken {
    AccessToken::new(
        "synthetic-token".into(),
        42,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn packet(command: &str, body: bson::Document) -> Packet {
    Packet::from_document(1, Command::new(command).unwrap(), &body).unwrap()
}
fn options(tls: sculk_client::tls::TlsSettings) -> Options {
    Options {
        booking: Endpoint::new("localhost", 443).unwrap(),
        booking_schema: BookingSchema::AgentMessenger,
        checkin: Protection::LocoGcm(key()),
        chat: Protection::LocoGcm(key()),
        tls,
        transport: Config::default(),
        stage_timeout: Duration::from_secs(2),
        total_timeout: Duration::from_secs(5),
        session_lease: Duration::from_secs(30),
    }
}
fn settings(tls: sculk_client::tls::TlsSettings) -> Settings {
    let options = options(tls);
    Settings {
        booking: options.booking,
        tls: options.tls,
        transport: options.transport,
        stage_timeout: options.stage_timeout,
        total_timeout: options.total_timeout,
        session_lease: options.session_lease,
    }
}

fn tablet_device() -> DeviceProfile {
    DeviceProfile {
        os: "android".into(),
        app_version: "25.9.2".into(),
        use_sub: true,
        ..profile()
    }
}

#[test]
fn named_profiles_reject_inconsistent_provider_metadata() {
    assert!("pc".parse::<Profile>().is_err());
    let (tls, _) = support::tls_pair();
    assert!(matches!(
        Connector::new(
            Profile::AndroidTablet,
            profile(),
            key(),
            settings(tls.clone())
        ),
        Err(connect::Error::UnsupportedProfile(_))
    ));
    let mut device = tablet_device();
    device.app_version = "26.2.0".into();
    assert!(matches!(
        Connector::new(Profile::AndroidTablet, device, key(), settings(tls.clone())),
        Err(connect::Error::UnsupportedProfile(_))
    ));
    assert_eq!(
        Connector::new(
            Profile::AndroidTablet,
            tablet_device(),
            key(),
            settings(tls)
        )
        .unwrap()
        .profile(),
        Profile::AndroidTablet,
    );
}

#[cfg(feature = "messaging")]
async fn tablet_round(
    connector: &Connector,
    acceptor: tokio_rustls::TlsAcceptor,
    valid_name: bool,
    lose_write: bool,
) -> Option<i64> {
    use sculk_client::{messaging::SendError, read_only::RoomId};
    let (bclient, bserver) = duplex(16384);
    let (cclient, cserver) = duplex(16384);
    let (lclient, lserver) = duplex(16384);
    let mut streams = VecDeque::from([bclient, cclient, lclient]);
    let booking_tls = acceptor.clone();
    let booking = async move {
        let mut peer = Framed::new(
            booking_tls.accept(bserver).await.unwrap(),
            LocoCodec::default(),
        );
        let request = peer.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "GETCONF");
        assert_eq!(
            request.document().unwrap(),
            doc! {"os":"android","model":""}
        );
        let mut reply = fixture(include_bytes!(
            "../../../tests/fixtures/agent-getconf-response.bin"
        ));
        reply.id = request.id;
        peer.send(reply).await.unwrap();
        assert!(peer.next().await.is_none());
    };
    let checkin = async move {
        let mut peer = Framed::new(server_secure(cserver).await, LocoCodec::default());
        let request = peer.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "CHECKIN");
        let body = request.document().unwrap();
        assert_eq!(body.get_i64("userId"), Ok(42));
        assert_eq!(body.get_bool("useSub"), Ok(true));
        let mut reply = fixture(include_bytes!(
            "../../../tests/fixtures/agent-checkin-response.bin"
        ));
        reply.id = request.id;
        peer.send(reply).await.unwrap();
        assert!(peer.next().await.is_none());
    };
    let chat = async move {
        let stream = acceptor.accept(lserver).await;
        if !valid_name {
            assert!(stream.is_err());
            return None;
        }
        let stream = stream.unwrap();
        assert_eq!(stream.get_ref().1.server_name(), Some("kakao.com"));
        let mut peer = Framed::new(stream, LocoCodec::default());
        let request = peer.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "LOGINLIST");
        let body = request.document().unwrap();
        assert_eq!(body.get_bool("isSw"), Ok(false));
        assert_eq!(body.get_str("duuid"), Ok("synthetic-device"));
        let mut reply = fixture(include_bytes!(
            "../../../tests/fixtures/agent-loginlist-response.bin"
        ));
        reply.id = request.id;
        peer.send(reply).await.unwrap();
        let request = peer.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "WRITE");
        let body = request.document().unwrap();
        assert_eq!(body.get_i32("scope"), Ok(1));
        assert_eq!(body.get_bool("silence"), Ok(false));
        assert_eq!(body.get_bool("noSeen"), Ok(false));
        assert_eq!(body.get_str("extra"), Ok("{}"));
        assert_eq!(body.get_i64("chatId"), Ok(42));
        let id = body.get_i64("msgId").unwrap();
        assert!((1..=i64::from(i32::MAX)).contains(&id));
        if !lose_write {
            let mut reply = packet(
                "WRITE",
                doc! {"status":0,"chatId":42_i64,"logId":54_i64,"sendAt":19_i64},
            );
            reply.id = request.id;
            peer.send(reply).await.unwrap();
            assert!(peer.next().await.is_none());
        }
        Some(id)
    };
    let (_tx, cancel) = watch::channel(false);
    let client = async {
        let session = connector
            .connect(
                token(),
                "synthetic-device",
                cancel.clone(),
                |stage, endpoint| {
                    assert_eq!(
                        match stage {
                            Stage::Booking => ("localhost", 443),
                            Stage::Checkin => ("checkin.example", 995),
                            Stage::Login => ("chat.example", 10009),
                        },
                        (endpoint.host(), endpoint.port())
                    );
                    let stream = streams
                        .pop_front()
                        .expect("no extra dial or transport fallback");
                    async { Ok(stream) }
                },
            )
            .await;
        if !valid_name {
            assert!(matches!(
                session,
                Err(connect::Error::Tls(sculk_client::tls::Error::Handshake))
            ));
            return;
        }
        let mut session = session.unwrap();
        assert_eq!(session.profile(), Some(Profile::AndroidTablet));
        let duplicate = connector
            .connect(
                token(),
                "synthetic-device",
                cancel.clone(),
                |_, _| -> std::future::Ready<std::io::Result<DuplexStream>> {
                    panic!("a second owner must fail before dialing")
                },
            )
            .await;
        assert!(matches!(duplicate, Err(connect::Error::SessionOwned)));
        let result = session
            .send_text(RoomId::new(42).unwrap(), "test", cancel)
            .await;
        if lose_write {
            assert_eq!(result.unwrap_err(), SendError::DeliveryUnknown);
        } else {
            assert_eq!(result.unwrap().id.get(), 54);
            session.disconnect_checked().await.unwrap();
        }
        assert!(streams.is_empty());
    };
    let ((), (), id, ()) = timeout(Duration::from_secs(6), async {
        tokio::join!(booking, checkin, chat, client)
    })
    .await
    .unwrap();
    id
}

#[cfg(feature = "messaging")]
#[tokio::test]
async fn tablet_uses_verified_tls_profile_write_and_unknown_delivery_across_generations() {
    let (tls, acceptor) = support::pair_with_names(&["localhost", "kakao.com"], false);
    let connector = Connector::new(
        Profile::AndroidTablet,
        tablet_device(),
        key(),
        settings(tls),
    )
    .unwrap();
    let first = tablet_round(&connector, acceptor.clone(), true, true)
        .await
        .unwrap();
    let second = tablet_round(&connector, acceptor, true, false)
        .await
        .unwrap();
    assert_ne!(first, second);
}

#[cfg(feature = "messaging")]
#[tokio::test]
async fn tablet_wrong_service_name_fails_before_login_without_fallback() {
    let (tls, acceptor) = support::tls_pair();
    let connector = Connector::new(
        Profile::AndroidTablet,
        tablet_device(),
        key(),
        settings(tls),
    )
    .unwrap();
    assert_eq!(tablet_round(&connector, acceptor, false, false).await, None);
}
async fn server_secure(mut stream: DuplexStream) -> SecureStream<DuplexStream> {
    let mut wire = [0; 268];
    stream.read_exact(&mut wire).await.unwrap();
    assert_eq!(&wire[..12], &[0, 1, 0, 0, 16, 0, 0, 0, 3, 0, 0, 0]);
    let aes = private().oaep_decrypt::<sha1::Sha1>(&wire[12..]).unwrap();
    assert_eq!(aes.len(), 16);
    SecureStream::from_codec(stream, GcmRecords::new(aes.as_slice().try_into().unwrap()))
}
fn fixture(bytes: &[u8]) -> Packet {
    LocoCodec::default()
        .decode(&mut BytesMut::from(bytes))
        .unwrap()
        .unwrap()
}
#[derive(Clone, Copy)]
enum Fault {
    None,
    Booking,
    Checkin,
    Login,
    Unexpected,
    Interrupted,
}
async fn run(fault: Fault) -> Result<sculk_client::session::Session, connect::Error> {
    let (tls, acceptor) = support::tls_pair();
    let opts = options(tls);
    let (bclient, bserver) = duplex(16384);
    let (cclient, cserver) = duplex(16384);
    let (lclient, lserver) = duplex(16384);
    let mut streams = VecDeque::from([
        (Stage::Booking, bclient),
        (Stage::Checkin, cclient),
        (Stage::Login, lclient),
    ]);
    let booking_server = tokio::spawn(async move {
        let stream = acceptor.accept(bserver).await.unwrap();
        let mut framed = Framed::new(stream, LocoCodec::default());
        let p = framed.next().await.unwrap().unwrap();
        assert_eq!(p.command.as_str(), "GETCONF");
        assert_eq!(p.document().unwrap(), doc! {"os":"mac","model":""});
        let mut reply = if matches!(fault, Fault::Booking) {
            packet("GETCONF", doc! {"wifi":{"ports":[995]}})
        } else {
            fixture(include_bytes!(
                "../../../tests/fixtures/agent-getconf-response.bin"
            ))
        };
        reply.id = p.id;
        framed.send(reply).await.unwrap();
        assert!(matches!(framed.next().await, None | Some(Err(_))));
    });
    let checkin_server = tokio::spawn(async move {
        if matches!(fault, Fault::Booking) {
            drop(cserver);
            return;
        }
        let stream = server_secure(cserver).await;
        let mut framed = Framed::new(stream, LocoCodec::default());
        let p = framed.next().await.unwrap().unwrap();
        assert_eq!(p.command.as_str(), "CHECKIN");
        assert_eq!(
            p.document().unwrap(),
            profile().checkin_request(42).unwrap().document().unwrap()
        );
        if matches!(fault, Fault::Interrupted) {
            return;
        }
        let mut reply = fixture(include_bytes!(
            "../../../tests/fixtures/agent-checkin-response.bin"
        ));
        if matches!(fault, Fault::Unexpected) {
            reply.command = Command::new("OTHER").unwrap();
        }
        reply.id = p.id;
        if matches!(fault, Fault::Checkin) {
            reply.status = (-12i16) as u16;
        }
        let _ = framed.send(reply).await;
        assert!(matches!(framed.next().await, None | Some(Err(_))));
    });
    let login_server = tokio::spawn(async move {
        if matches!(
            fault,
            Fault::Booking | Fault::Checkin | Fault::Unexpected | Fault::Interrupted
        ) {
            drop(lserver);
            return;
        }
        let stream = server_secure(lserver).await;
        let mut framed = Framed::new(stream, LocoCodec::default());
        let p = framed.next().await.unwrap().unwrap();
        assert_eq!(p.command.as_str(), "LOGINLIST");
        let body = p.document().unwrap();
        assert_eq!(
            body,
            profile()
                .login_request(&token(), "synthetic-device")
                .unwrap()
                .document()
                .unwrap()
        );
        let mut reply = if matches!(fault, Fault::Login) {
            packet("LOGINLIST", doc! {"status":-950})
        } else {
            fixture(include_bytes!(
                "../../../tests/fixtures/agent-loginlist-response.bin"
            ))
        };
        reply.id = p.id;
        framed.send(reply).await.unwrap();
        while let Some(Ok(p)) = framed.next().await {
            if framed.send(p).await.is_err() {
                break;
            }
        }
    });
    let (_tx, cancel) = watch::channel(false);
    let result = opts
        .connect(
            &profile(),
            token(),
            "synthetic-device",
            cancel,
            |stage, endpoint| {
                let (expected, stream) = streams.pop_front().expect("unexpected extra connection");
                assert_eq!(stage, expected);
                assert_eq!(
                    endpoint.host(),
                    match stage {
                        Stage::Booking => "localhost",
                        Stage::Checkin => "checkin.example",
                        Stage::Login => "chat.example",
                    }
                );
                assert_eq!(
                    endpoint.port(),
                    match stage {
                        Stage::Booking => 443,
                        Stage::Checkin => 995,
                        Stage::Login => 5223,
                    }
                );
                async { Ok(stream) }
            },
        )
        .await;
    drop(streams);
    booking_server.await.unwrap();
    checkin_server.await.unwrap();
    if result.is_err() {
        timeout(Duration::from_secs(2), login_server)
            .await
            .unwrap()
            .unwrap();
    } else {
        // Successful login server remains active until returned Session is dropped.
        drop(login_server);
    }
    result
}
#[tokio::test]
async fn documented_booking_checkin_and_login_sequence() {
    let mut session = run(Fault::None).await.unwrap();
    assert_eq!(session.state(), State::Authenticated);
    let response = session.request(packet("PING", doc! {})).await.unwrap();
    assert_eq!(response.command.as_str(), "PING");
    session.disconnect().await;
    assert_eq!(session.state(), State::Closed);
}
#[tokio::test]
async fn malformed_booking_rejected_before_checkin() {
    assert!(matches!(
        run(Fault::Booking).await,
        Err(connect::Error::Response(Stage::Booking))
    ));
}
#[tokio::test]
async fn rejected_checkin_stops_before_login() {
    assert!(matches!(
        run(Fault::Checkin).await,
        Err(connect::Error::Rejected(Stage::Checkin, -12))
    ));
}
#[tokio::test]
async fn rejected_login_is_terminal() {
    assert!(matches!(
        run(Fault::Login).await,
        Err(connect::Error::Auth(AuthError::Rejected(-950)))
    ));
}
#[tokio::test]
async fn unexpected_command_and_interrupted_connection_fail_closed() {
    assert!(matches!(
        run(Fault::Unexpected).await,
        Err(connect::Error::Transport(
            Stage::Checkin,
            sculk_client::Error::Mismatch
        ))
    ));
    assert!(matches!(
        run(Fault::Interrupted).await,
        Err(connect::Error::Transport(
            Stage::Checkin,
            sculk_client::Error::Closed
        ))
    ));
}
#[test]
fn invalid_booking_assignment_and_profiles() {
    for body in [
        doc! {},
        doc! {"hosts":[],"wifi":{"ports":[995]}},
        doc! {"hosts":["bad/host"],"wifi":{"ports":[995]}},
        doc! {"hosts":["ok.example"],"wifi":{"ports":[65536]}},
        doc! {"hosts":["ok.example"],"wifi":{"ports":[0]}},
        doc! {"hosts":["ok.example"],"ticket":{"lsl":["other.example"]},"wifi":{"ports":[995]}},
    ] {
        assert!(Booking::parse(&packet("GETCONF", body)).is_err());
    }
    let b = Booking::parse(&packet(
        "GETCONF",
        doc! {"ticket":{"lsl":["checkin.example","backup.example"]},"wifi":{"ports":[443,995]}},
    ))
    .unwrap();
    assert_eq!(b.selected(), Endpoint::new("checkin.example", 443).unwrap());
    for body in [
        doc! {},
        doc! {"host":"chat.example","port":-1},
        doc! {"host":"chat.example","port":443.0},
        doc! {"host":"https://chat.example","port":443},
    ] {
        assert!(connect::checkin_assignment(&packet("CHECKIN", body)).is_err());
    }
    let mut p = profile();
    p.os.clear();
    assert!(p.checkin_request(42).is_err());
    assert!(profile().checkin_request(u64::MAX).is_err());
}
#[tokio::test]
async fn cancellation_during_dial_and_total_timeout_do_not_retry() {
    for cancelled in [true, false] {
        let (tls, _) = support::tls_pair();
        let mut opts = options(tls);
        opts.total_timeout = Duration::from_millis(20);
        let (tx, cancel) = watch::channel(false);
        let called = std::sync::atomic::AtomicUsize::new(0);
        let p = profile();
        let future = opts.connect(&p, token(), "synthetic-device", cancel, |_, _| {
            called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { std::future::pending::<Result<DuplexStream, std::io::Error>>().await }
        });
        let signal = async {
            tokio::task::yield_now().await;
            if cancelled {
                tx.send(true).unwrap();
            }
        };
        let (result, _) = tokio::join!(future, signal);
        assert!(if cancelled {
            matches!(result, Err(connect::Error::Cancelled))
        } else {
            matches!(result, Err(connect::Error::Timeout))
        });
        assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn expired_token_blocks_all_dials() {
    let (tls, _) = support::tls_pair();
    let opts = options(tls);
    let profile = profile();
    let expired = AccessToken::new(
        "synthetic".into(),
        42,
        Instant::now() + Duration::from_millis(1),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let (_tx, cancel) = watch::channel(false);
    let result = opts
        .connect(
            &profile,
            expired,
            "synthetic-device",
            cancel,
            |_, _| async {
                panic!("expired token must not connect");
                #[allow(unreachable_code)]
                Ok::<DuplexStream, std::io::Error>(duplex(1).0)
            },
        )
        .await;
    assert!(matches!(result, Err(connect::Error::Expired)));
}
#[tokio::test]
async fn encrypted_record_boundaries_are_independent_and_invalid_tags_fail() {
    let (a, b) = duplex(131072);
    let mut client = SecureStream::from_codec(a, GcmRecords::new(&[7; 16]));
    let bytes = include_bytes!("../../../tests/fixtures/gcm-node-ping.bin");
    let server = tokio::spawn(async move {
        let mut raw = b;
        for chunk in bytes.chunks(3) {
            raw.write_all(chunk).await.unwrap();
        }
        raw.flush().await.unwrap();
    });
    let mut out = vec![0; 27];
    client.read_exact(&mut out).await.unwrap();
    assert_eq!(
        out.as_slice(),
        include_bytes!("../../../tests/fixtures/ping.bin")
    );
    server.await.unwrap();
    let (a, mut b) = duplex(1024);
    let mut client = SecureStream::from_codec(a, GcmRecords::new(&[7; 16]));
    let mut bad = bytes.to_vec();
    let n = bad.len();
    bad[n - 1] ^= 1;
    b.write_all(&bad).await.unwrap();
    b.flush().await.unwrap();
    assert_eq!(
        client.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}
#[tokio::test]
async fn large_packet_spans_multiple_gcm_records() {
    let (a, b) = duplex(65536);
    let mut writer = SecureStream::from_codec(a, GcmRecords::new(&[5; 16]));
    let reader = SecureStream::from_codec(b, GcmRecords::new(&[5; 16]));
    let payload = packet("LARGE", doc! {"data":"x".repeat(100000)});
    let expected = payload.clone();
    let sender = tokio::spawn(async move {
        let mut framed = Framed::new(&mut writer, LocoCodec::default());
        framed.send(payload).await.unwrap();
    });
    let mut framed = Framed::new(reader, LocoCodec::default());
    assert_eq!(framed.next().await.unwrap().unwrap(), expected);
    sender.await.unwrap();
}
#[test]
fn known_public_key_import_and_node_crypto_fixture() {
    let key = ServerKey::from_pkcs1_der(include_bytes!(
        "../../../tests/fixtures/documented-server-public.der"
    ))
    .unwrap();
    let (wire, _) = key.initiate().unwrap();
    assert_eq!(wire.len(), 268);
    assert!(ServerKey::from_pkcs1_der(&[0; 4097]).is_err());
    let mut wire = BytesMut::from(&include_bytes!("../../../tests/fixtures/gcm-node-ping.bin")[..]);
    assert_eq!(
        GcmRecords::new(&[7; 16])
            .decode(&mut wire)
            .unwrap()
            .unwrap()
            .as_ref(),
        include_bytes!("../../../tests/fixtures/ping.bin")
    );
}

#[test]
fn request_contract_matches_agent_source_fixtures() {
    let p = profile();
    for (mut request, bytes) in [
        (
            p.booking_request(BookingSchema::AgentMessenger, 42)
                .unwrap(),
            &include_bytes!("../../../tests/fixtures/agent-getconf.bin")[..],
        ),
        (
            p.login_request(&token(), "synthetic-device").unwrap(),
            &include_bytes!("../../../tests/fixtures/agent-loginlist.bin")[..],
        ),
    ] {
        request.id = 1;
        let mut wire = BytesMut::new();
        LocoCodec::default().encode(request, &mut wire).unwrap();
        assert_eq!(wire.as_ref(), bytes);
    }
    // Agent uses JS Number; Sculk deliberately preserves account IDs as Int64.
    let mut reference = fixture(include_bytes!("../../../tests/fixtures/agent-checkin.bin"))
        .document()
        .unwrap();
    let user = reference.remove("userId").unwrap();
    assert_eq!(user, bson::Bson::Int32(42));
    let mut actual = p.checkin_request(42).unwrap().document().unwrap();
    assert_eq!(actual.remove("userId").unwrap(), bson::Bson::Int64(42));
    assert_eq!(actual, reference);
    let extended = p
        .booking_request(BookingSchema::OpenKakao, 42)
        .unwrap()
        .document()
        .unwrap();
    assert_eq!(extended.get_i64("userId").unwrap(), 42);
    assert_eq!(extended.get_str("MCCMNC").unwrap(), "99999");
}

#[tokio::test]
async fn cancellation_during_key_exchange_drops_partial_stream() {
    let (client, mut server) = duplex(1);
    let (tx, cancel) = watch::channel(false);
    let task = tokio::spawn(async move {
        SecureStream::establish(client, &key(), Duration::from_secs(1), cancel).await
    });
    tokio::task::yield_now().await;
    tx.send(true).unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Err(connect::Error::Cancelled)
    ));
    let mut bytes = Vec::new();
    timeout(Duration::from_secs(1), server.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert!(bytes.len() <= 1);
}
#[test]
fn record_lengths_rejected_before_allocation_and_truncated_eof_is_fatal() {
    for size in [0u32, 27, u32::MAX] {
        let mut input = BytesMut::from(&size.to_le_bytes()[..]);
        let capacity = input.capacity();
        assert!(GcmRecords::new(&[7; 16]).decode(&mut input).is_err());
        assert_eq!(input.capacity(), capacity);
    }
    let mut short = BytesMut::from(&[28, 0, 0, 0, 1, 2, 3][..]);
    assert!(matches!(
        GcmRecords::new(&[7; 16]).decode_eof(&mut short),
        Err(sculk_protocol::Error::Truncated)
    ));
}
#[tokio::test]
async fn empty_records_do_not_report_eof_before_real_plaintext() {
    let (client, mut server) = duplex(4096);
    let mut reader = SecureStream::from_codec(client, GcmRecords::new(&[8; 16]));
    let mut codec = GcmRecords::new(&[8; 16]);
    let mut bytes = BytesMut::new();
    for _ in 0..40 {
        codec.encode(bytes::Bytes::new(), &mut bytes).unwrap();
    }
    codec
        .encode(bytes::Bytes::from_static(b"data"), &mut bytes)
        .unwrap();
    server.write_all(&bytes).await.unwrap();
    server.flush().await.unwrap();
    let mut out = [0; 4];
    timeout(Duration::from_secs(1), reader.read_exact(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&out, b"data");
}
