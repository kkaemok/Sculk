#![cfg(feature = "secure-session")]
mod support;
use sculk_client::tls::{Error, TlsSettings};
use std::time::Duration;
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt},
    sync::watch,
};

#[tokio::test]
async fn verified_tls_encrypts_application_exchange() {
    let (settings, acceptor) = support::tls_pair();
    let (a, b) = duplex(16384);
    let server = tokio::spawn(async move {
        let mut stream = acceptor.accept(b).await.unwrap();
        assert!(!stream.get_ref().1.is_handshaking());
        let mut bytes = [0; 4];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"PING");
        stream.write_all(b"PONG").await.unwrap();
        stream.flush().await.unwrap();
    });
    let (_tx, cancel) = watch::channel(false);
    let mut stream = settings
        .handshake(a, "localhost", Duration::from_secs(2), cancel)
        .await
        .unwrap();
    stream.write_all(b"PING").await.unwrap();
    stream.flush().await.unwrap();
    let mut bytes = [0; 4];
    stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"PONG");
    server.await.unwrap();
}
#[tokio::test]
async fn untrusted_certificate_and_wrong_hostname_are_rejected() {
    for trusted in [false, true] {
        let (settings, acceptor) = support::tls_pair();
        let (a, b) = duplex(16384);
        let server = tokio::spawn(async move {
            let _ = acceptor.accept(b).await;
        });
        let settings = if trusted {
            settings
        } else {
            TlsSettings::default()
        };
        let (_tx, cancel) = watch::channel(false);
        let name = if trusted {
            "wrong.example"
        } else {
            "localhost"
        };
        assert!(matches!(
            settings
                .handshake(a, name, Duration::from_secs(2), cancel)
                .await,
            Err(Error::Handshake)
        ));
        server.await.unwrap();
    }
}
#[tokio::test]
async fn tls_timeout_cancellation_and_bad_configuration() {
    let settings = TlsSettings::default();
    for (duration, name, expected) in [
        (Duration::from_millis(10), "localhost", Error::Timeout),
        (Duration::ZERO, "localhost", Error::TimeoutConfig),
        (Duration::from_secs(1), "bad host", Error::ServerName),
    ] {
        let (a, _b) = duplex(16384);
        let (_tx, cancel) = watch::channel(false);
        assert_eq!(
            settings.handshake(a, name, duration, cancel).await.err(),
            Some(expected)
        );
    }
    let (a, _b) = duplex(16384);
    let (tx, cancel) = watch::channel(false);
    tx.send(true).unwrap();
    assert_eq!(
        settings
            .handshake(a, "localhost", Duration::from_secs(2), cancel)
            .await
            .err(),
        Some(Error::Cancelled)
    );
}
#[tokio::test]
async fn tls_false_cancel_update_is_not_cancellation() {
    let (settings, acceptor) = support::tls_pair();
    let (a, b) = duplex(16384);
    let (tx, cancel) = watch::channel(false);
    tx.send(false).unwrap();
    let server = tokio::spawn(async move { acceptor.accept(b).await.unwrap() });
    let stream = settings
        .handshake(a, "localhost", Duration::from_secs(2), cancel)
        .await
        .unwrap();
    server.await.unwrap();
    drop(stream);
}

#[tokio::test]
async fn trusted_but_expired_certificate_is_rejected() {
    let (settings, acceptor) = support::expired_pair();
    let (a, b) = duplex(16384);
    let server = tokio::spawn(async move {
        let _ = acceptor.accept(b).await;
    });
    let (_tx, cancel) = watch::channel(false);
    assert_eq!(
        settings
            .handshake(a, "localhost", Duration::from_secs(2), cancel)
            .await
            .err(),
        Some(Error::Handshake)
    );
    server.await.unwrap();
}
#[tokio::test]
async fn cancellation_interrupts_a_stalled_tls_handshake() {
    let settings = TlsSettings::default();
    let (a, _b) = duplex(16384);
    let (tx, cancel) = watch::channel(false);
    let task = tokio::spawn(async move {
        settings
            .handshake(a, "localhost", Duration::from_secs(2), cancel)
            .await
    });
    tokio::task::yield_now().await;
    tx.send(true).unwrap();
    assert_eq!(task.await.unwrap().err(), Some(Error::Cancelled));
}
