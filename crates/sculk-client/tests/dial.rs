#![cfg(feature = "protocol-connect")]
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

#[tokio::test]
async fn direct_tcp_checks_host_port_and_pre_cancel_before_opening() {
    use sculk_client::dial::DirectTcp;
    use std::io::ErrorKind;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let allowed = Endpoint::new("localhost", port).unwrap();
    let route = DirectTcp::new(vec![allowed.clone()], Duration::from_secs(1)).unwrap();
    let (stop, cancel) = watch::channel(false);
    assert_eq!(
        route
            .dial(Endpoint::new("127.0.0.1", port).unwrap(), cancel.clone())
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::PermissionDenied,
    );
    stop.send_replace(true);
    assert_eq!(
        route
            .dial(allowed.clone(), cancel.clone())
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Interrupted,
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    stop.send_replace(false);
    let socket = route.dial(allowed, cancel).await.unwrap();
    let (_, peer) = listener.accept().await.unwrap();
    assert_eq!(socket.local_addr().unwrap(), peer);
}
