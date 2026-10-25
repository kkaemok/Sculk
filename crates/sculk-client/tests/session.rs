#![cfg(feature = "secure-session")]
mod support;
use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_client::session::{
    parse_loginlist_reply, AccessToken, AuthError, Grant, RequestError, Session, State,
};
use sculk_client::{Client, Config};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Duration;
use tokio::{
    io::{duplex, AsyncWriteExt},
    sync::watch,
    time::{timeout, Instant},
};
use tokio_util::codec::Framed;
fn packet(command: &str, document: bson::Document) -> Packet {
    Packet::from_document(0, Command::new(command).unwrap(), &document).unwrap()
}
fn token(seconds: u64) -> AccessToken {
    AccessToken::new(
        "synthetic-not-a-real-token".into(),
        42,
        Instant::now() + Duration::from_secs(seconds),
    )
    .unwrap()
}
async fn provider(client: Client, secret: AccessToken) -> Result<Grant, AuthError> {
    // Explicitly a synthetic mock schema, NOT a KakaoTalk LOGINLIST request builder.
    let expected_user = secret.user_id();
    let request = packet("LOGINLIST", doc! {"mockToken":secret.expose_secret()});
    let reply = client.request(request).await.map_err(AuthError::from)?;
    parse_loginlist_reply(&reply, expected_user, Duration::from_secs(30))
}
async fn setup(body: bson::Document) -> (Session, tokio::task::JoinHandle<()>) {
    let (settings, acceptor) = support::tls_pair();
    let (a, b) = duplex(16384);
    let server = tokio::spawn(async move {
        let tls = acceptor.accept(b).await.unwrap();
        let mut framed = Framed::new(tls, LocoCodec::default());
        let request = framed.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "LOGINLIST");
        let mut reply = packet("LOGINLIST", body);
        reply.id = request.id;
        framed.send(reply).await.unwrap();
        while let Some(Ok(p)) = framed.next().await {
            if framed.send(p).await.is_err() {
                break;
            }
        }
    });
    let (_tx, cancel) = watch::channel(false);
    let session = Session::establish(
        a,
        "localhost",
        &settings,
        Duration::from_secs(2),
        Config::default(),
        cancel,
    )
    .await
    .unwrap();
    (session, server)
}
#[tokio::test]
async fn successful_mock_authentication_then_request_and_clean_shutdown() {
    let (mut session, server) = setup(doc! {"status":0,"userId":42_i64}).await;
    assert_eq!(session.state(), State::SecureTransport);
    assert!(matches!(
        session.request(packet("PING", doc! {})).await,
        Err(RequestError::Unauthenticated)
    ));
    let mut states = session.subscribe();
    let (_tx, cancel) = watch::channel(false);
    let token = token(60);
    assert!(!format!("{token:?}").contains("synthetic"));
    session
        .authenticate_with(token, Duration::from_secs(1), cancel, provider)
        .await
        .unwrap();
    states.changed().await.unwrap();
    assert_eq!(*states.borrow(), State::Authenticated);
    assert_eq!(
        session
            .request(packet("PING", doc! {}))
            .await
            .unwrap()
            .command
            .as_str(),
        "PING"
    );
    session.disconnect().await;
    assert_eq!(session.state(), State::Closed);
    server.await.unwrap();
}
#[tokio::test]
async fn rejected_and_invalid_acknowledgments_are_terminal_without_retry() {
    for (body, expected) in [
        (doc! {"status":-999}, AuthError::Rejected(-999)),
        (doc! {"status":"0","userId":42}, AuthError::InvalidResponse),
        (doc! {"status":0,"userId":43}, AuthError::InvalidResponse),
        (doc! {"status":0}, AuthError::InvalidResponse),
    ] {
        let (mut session, server) = setup(body).await;
        let (_tx, cancel) = watch::channel(false);
        assert_eq!(
            session
                .authenticate_with(token(60), Duration::from_secs(1), cancel.clone(), provider)
                .await,
            Err(expected.clone())
        );
        assert_eq!(session.state(), State::Failed(expected));
        assert_eq!(
            session
                .authenticate_with(token(60), Duration::from_secs(1), cancel, provider)
                .await,
            Err(AuthError::State)
        );
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }
}
#[tokio::test]
async fn session_expiration_closes_connection_and_blocks_requests() {
    let (mut session, server) = setup(doc! {"status":0,"userId":42}).await;
    let (_tx, cancel) = watch::channel(false);
    session
        .authenticate_with(
            token(60),
            Duration::from_secs(1),
            cancel,
            |client, secret| async move {
                let mut grant = provider(client, secret).await?;
                grant.valid_for = Duration::from_millis(10);
                Ok(grant)
            },
        )
        .await
        .unwrap();
    let mut state = session.subscribe();
    timeout(Duration::from_secs(1), async {
        loop {
            if *state.borrow_and_update() == State::Failed(AuthError::Expired) {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        session.request(packet("PING", doc! {})).await,
        Err(RequestError::Expired)
    ));
    timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn interruptions_and_invalid_wire_frames_fail_closed() {
    for corrupt in [false, true] {
        let (settings, acceptor) = support::tls_pair();
        let (a, b) = duplex(16384);
        let server = tokio::spawn(async move {
            let tls = acceptor.accept(b).await.unwrap();
            let mut framed = Framed::new(tls, LocoCodec::default());
            framed.next().await.unwrap().unwrap();
            if corrupt {
                framed.get_mut().write_all(&[255; 22]).await.unwrap();
                framed.get_mut().flush().await.unwrap();
            }
        });
        let (_tx, cancel) = watch::channel(false);
        let mut session = Session::establish(
            a,
            "localhost",
            &settings,
            Duration::from_secs(2),
            Config::default(),
            cancel.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            session
                .authenticate_with(token(60), Duration::from_secs(1), cancel, provider)
                .await,
            Err(AuthError::Interrupted)
        );
        server.await.unwrap();
    }
}
#[tokio::test]
async fn dropped_authentication_future_cancels_state_and_stream() {
    let (settings, acceptor) = support::tls_pair();
    let (a, b) = duplex(16384);
    let server = tokio::spawn(async move {
        let tls = acceptor.accept(b).await.unwrap();
        let mut framed = Framed::new(tls, LocoCodec::default());
        // No provider request needed: pending provider is cancelled before output.
        assert!(matches!(framed.next().await, None | Some(Err(_))));
    });
    let (_tx, cancel) = watch::channel(false);
    let mut session = Session::establish(
        a,
        "localhost",
        &settings,
        Duration::from_secs(2),
        Config::default(),
        cancel.clone(),
    )
    .await
    .unwrap();
    let state = session.subscribe();
    {
        let attempt =
            session.authenticate_with(token(60), Duration::from_secs(1), cancel, |_, _| async {
                std::future::pending::<Result<Grant, AuthError>>().await
            });
        tokio::pin!(attempt);
        tokio::select! { biased;
            result=&mut attempt=>panic!("unexpected {result:?}"),
            _=tokio::task::yield_now()=>{}
        }
        assert_eq!(*state.borrow(), State::Authenticating);
    }
    assert_eq!(session.state(), State::Failed(AuthError::Cancelled));
    timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
}
#[test]
fn reply_parser_handles_signed_status_and_identity_without_guessing_rest_codes() {
    let mut reply = packet("LOGINLIST", doc! {"userId":42_i64});
    assert_eq!(
        parse_loginlist_reply(&reply, 42, Duration::from_secs(1))
            .unwrap()
            .user_id,
        42
    );
    reply.status = (-100_i16) as u16;
    assert!(matches!(
        parse_loginlist_reply(&reply, 42, Duration::from_secs(1)),
        Err(AuthError::Rejected(-100))
    ));
    assert!(matches!(
        AccessToken::new("secret".into(), 42, Instant::now()),
        Err(AuthError::Expired)
    ));
}

#[tokio::test]
async fn challenge_unsupported_and_wrong_account_grants_fail_closed() {
    for expected in [
        AuthError::VerificationRequired,
        AuthError::Unsupported,
        AuthError::InvalidGrant,
    ] {
        let (settings, acceptor) = support::tls_pair();
        let (a, b) = duplex(16384);
        let server = tokio::spawn(async move {
            let tls = acceptor.accept(b).await.unwrap();
            let mut f = Framed::new(tls, LocoCodec::default());
            assert!(matches!(f.next().await, None | Some(Err(_))));
        });
        let (_tx, cancel) = watch::channel(false);
        let mut session = Session::establish(
            a,
            "localhost",
            &settings,
            Duration::from_secs(2),
            Config::default(),
            cancel.clone(),
        )
        .await
        .unwrap();
        let error = expected.clone();
        let result = session
            .authenticate_with(
                token(60),
                Duration::from_secs(1),
                cancel,
                move |_, _| async move {
                    if error == AuthError::InvalidGrant {
                        Ok(Grant {
                            user_id: 43,
                            valid_for: Duration::from_secs(1),
                        })
                    } else {
                        Err(error)
                    }
                },
            )
            .await;
        assert_eq!(result, Err(expected.clone()));
        assert_eq!(session.state(), State::Failed(expected));
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }
}
#[tokio::test]
async fn cancellation_timeout_and_token_expiry_interrupt_pending_provider() {
    for expected in [AuthError::Cancelled, AuthError::Timeout, AuthError::Expired] {
        let (settings, acceptor) = support::tls_pair();
        let (a, b) = duplex(16384);
        let server = tokio::spawn(async move {
            let tls = acceptor.accept(b).await.unwrap();
            let mut f = Framed::new(tls, LocoCodec::default());
            assert!(matches!(f.next().await, None | Some(Err(_))));
        });
        let (tx, cancel) = watch::channel(false);
        let mut session = Session::establish(
            a,
            "localhost",
            &settings,
            Duration::from_secs(2),
            Config::default(),
            cancel.clone(),
        )
        .await
        .unwrap();
        let token = if expected == AuthError::Expired {
            AccessToken::new(
                "synthetic".into(),
                42,
                Instant::now() + Duration::from_millis(10),
            )
            .unwrap()
        } else {
            token(60)
        };
        if expected == AuthError::Cancelled {
            tx.send(true).unwrap();
        }
        let duration = if expected == AuthError::Timeout {
            Duration::from_millis(10)
        } else {
            Duration::from_secs(1)
        };
        let result = session
            .authenticate_with(token, duration, cancel, |_, _| async {
                std::future::pending::<Result<Grant, AuthError>>().await
            })
            .await;
        assert_eq!(result, Err(expected.clone()));
        assert_eq!(session.state(), State::Failed(expected));
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }
}
#[tokio::test]
async fn authenticated_session_detects_connection_loss() {
    let (settings, acceptor) = support::tls_pair();
    let (a, b) = duplex(16384);
    let (close_tx, close_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let tls = acceptor.accept(b).await.unwrap();
        let mut f = Framed::new(tls, LocoCodec::default());
        let p = f.next().await.unwrap().unwrap();
        let mut response = packet("LOGINLIST", doc! {"status":0,"userId":42});
        response.id = p.id;
        f.send(response).await.unwrap();
        let _ = close_rx.await;
    });
    let (_tx, cancel) = watch::channel(false);
    let mut session = Session::establish(
        a,
        "localhost",
        &settings,
        Duration::from_secs(2),
        Config::default(),
        cancel.clone(),
    )
    .await
    .unwrap();
    session
        .authenticate_with(token(60), Duration::from_secs(1), cancel, provider)
        .await
        .unwrap();
    let mut state = session.subscribe();
    close_tx.send(()).unwrap();
    timeout(Duration::from_secs(1), async {
        loop {
            if *state.borrow_and_update() == State::Failed(AuthError::Interrupted) {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    server.await.unwrap();
}

#[test]
fn duplicate_authentication_fields_and_non_integer_identity_are_rejected() {
    let original = packet("LOGINLIST", doc! {"userId":42_i64});
    let mut body = original.body().to_vec();
    let duplicate = body[4..body.len() - 1].to_vec();
    body.pop();
    body.extend_from_slice(&duplicate);
    body.push(0);
    let len = body.len() as i32;
    body[..4].copy_from_slice(&len.to_le_bytes());
    let reply = Packet::from_bytes(1, 0, Command::new("LOGINLIST").unwrap(), body.into()).unwrap();
    assert!(matches!(
        parse_loginlist_reply(&reply, 42, Duration::from_secs(1)),
        Err(AuthError::InvalidResponse)
    ));
    let reply = packet("LOGINLIST", doc! {"status":0,"userId":42.0});
    assert!(matches!(
        parse_loginlist_reply(&reply, 42, Duration::from_secs(1)),
        Err(AuthError::InvalidResponse)
    ));
}
