#![cfg(feature = "android-auth")]
use sculk_client::{connect::Endpoint, dial::ConnectProxy};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::watch,
};
#[tokio::test]
async fn approved_tunnel_preserves_first_payload_bytes_and_denies_other_hosts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let target = Endpoint::new("approved.example", 995).unwrap();
    let proxy = ConnectProxy::new(
        address.ip().to_string(),
        address.port(),
        vec![target.clone()],
        Duration::from_secs(1),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(s.read_u8().await.unwrap());
        }
        assert!(std::str::from_utf8(&header)
            .unwrap()
            .starts_with("CONNECT approved.example:995 HTTP/1.1"));
        s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\nPAYLOAD")
            .await
            .unwrap();
    });
    let (_tx, cancel) = watch::channel(false);
    assert_eq!(
        proxy
            .dial(Endpoint::new("other.example", 995).unwrap(), cancel.clone())
            .await
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let mut socket = proxy.dial(target, cancel).await.unwrap();
    let mut out = [0; 7];
    socket.read_exact(&mut out).await.unwrap();
    assert_eq!(&out, b"PAYLOAD");
    server.await.unwrap();
}
#[tokio::test]
async fn proxy_denial_timeout_and_cancellation_do_not_fallback() {
    for (response, expected) in [
        (
            Some("HTTP/1.1 403 Forbidden\r\n\r\n"),
            std::io::ErrorKind::PermissionDenied,
        ),
        (None, std::io::ErrorKind::TimedOut),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = Endpoint::new("approved.example", 995).unwrap();
        let proxy = ConnectProxy::new(
            address.ip().to_string(),
            address.port(),
            vec![endpoint.clone()],
            Duration::from_millis(20),
        )
        .unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            assert!(s.read(&mut bytes).await.unwrap() > 0);
            if let Some(response) = response {
                s.write_all(response.as_bytes()).await.unwrap();
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let (_tx, cancel) = watch::channel(false);
        assert_eq!(
            proxy.dial(endpoint, cancel).await.err().unwrap().kind(),
            expected
        );
        task.await.unwrap();
    }
    let proxy = ConnectProxy::new(
        "127.0.0.1".into(),
        1,
        vec![Endpoint::new("approved.example", 995).unwrap()],
        Duration::from_secs(1),
    )
    .unwrap();
    let (tx, cancel) = watch::channel(false);
    tx.send(true).unwrap();
    assert_eq!(
        proxy
            .dial(Endpoint::new("approved.example", 995).unwrap(), cancel)
            .await
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
}
