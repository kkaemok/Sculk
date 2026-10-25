use futures_util::{SinkExt, StreamExt};
use sculk_client::{Client, Config, ConnectionState, Error, Event};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Duration;
use tokio::{
    io::{duplex, AsyncWriteExt},
    time::{sleep, timeout},
};
use tokio_util::codec::Framed;
fn packet(name: &str) -> Packet {
    Packet::from_bytes(
        0,
        0,
        Command::new(name).unwrap(),
        vec![5, 0, 0, 0, 0].into(),
    )
    .unwrap()
}
#[tokio::test]
async fn out_of_order_responses_and_events_on_tcp() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut f = Framed::new(socket, LocoCodec::default());
        let a = f.next().await.unwrap().unwrap();
        let b = f.next().await.unwrap().unwrap();
        assert_ne!(a.id, b.id);
        assert_ne!(a.id, 0);
        f.send(packet("BROADCAST")).await.unwrap();
        f.send(b).await.unwrap();
        f.send(a).await.unwrap();
        // Keep the stream alive until the client explicitly disconnects.
        assert!(f.next().await.is_none());
    });
    let client = Client::from_stream(
        tokio::net::TcpStream::connect(address).await.unwrap(),
        Config::default(),
    )
    .unwrap();
    let mut events = client.subscribe();
    let (a, b) = tokio::join!(
        client.request(packet("FIRST")),
        client.request(packet("SECOND"))
    );
    assert_eq!(a.unwrap().command.as_str(), "FIRST");
    assert_eq!(b.unwrap().command.as_str(), "SECOND");
    let Event::UnmatchedPacket(p) = events.recv().await.unwrap();
    assert_eq!(p.command.as_str(), "BROADCAST");
    assert_eq!(
        client.authenticate().await,
        Err(Error::AuthenticationUnavailable)
    );
    client.disconnect().await;
    assert_eq!(client.state(), ConnectionState::Closed);
    server.await.unwrap();
}
#[tokio::test]
async fn timeouts_are_not_replayed_and_cancellation_releases_capacity() {
    let (a, b) = duplex(1024);
    let client = Client::from_stream(
        a,
        Config {
            requests: 1,
            request_timeout: Duration::from_millis(40),
            ..Config::default()
        },
    )
    .unwrap();
    let mut server = Framed::new(b, LocoCodec::default());
    let c = client.clone();
    let request = tokio::spawn(async move { c.request(packet("ONE")).await });
    let first = server.next().await.unwrap().unwrap();
    assert_eq!(client.request(packet("FULL")).await, Err(Error::Overloaded));
    assert_eq!(request.await.unwrap(), Err(Error::Timeout));
    server.send(first).await.unwrap();
    let c = client.clone();
    let cancelled = tokio::spawn(async move { c.request(packet("CANCEL")).await });
    let _ = server.next().await.unwrap().unwrap();
    cancelled.abort();
    let _ = cancelled.await;
    let c = client.clone();
    let next = tokio::spawn(async move { c.request(packet("NEXT")).await });
    let p = server.next().await.unwrap().unwrap();
    assert_eq!(p.command.as_str(), "NEXT");
    server.send(p).await.unwrap();
    next.await.unwrap().unwrap();
    client.disconnect().await;
}
#[tokio::test]
async fn peer_loss_and_malformed_data_resolve_pending() {
    for malformed in [false, true] {
        let (a, mut b) = duplex(1024);
        let client = Client::from_stream(a, Config::default()).unwrap();
        let c = client.clone();
        let request = tokio::spawn(async move { c.request(packet("WAIT")).await });
        let mut f = Framed::new(&mut b, LocoCodec::default());
        f.next().await.unwrap().unwrap();
        drop(f);
        if malformed {
            b.write_all(&[0xff; 22]).await.unwrap();
        } else {
            drop(b);
        }
        assert_eq!(
            timeout(Duration::from_secs(1), request)
                .await
                .unwrap()
                .unwrap(),
            Err(Error::Closed)
        );
        assert_eq!(client.state(), ConnectionState::Lost);
    }
}
#[tokio::test]
async fn mismatched_command_and_rejection_are_distinct() {
    let (a, b) = duplex(1024);
    let client = Client::from_stream(a, Config::default()).unwrap();
    let server = tokio::spawn(async move {
        let mut f = Framed::new(b, LocoCodec::default());
        let mut p = f.next().await.unwrap().unwrap();
        p.command = Command::new("WRONG").unwrap();
        f.send(p).await.unwrap();
        let mut p = f.next().await.unwrap().unwrap();
        p.status = 42;
        f.send(p).await.unwrap();
    });
    assert_eq!(client.request(packet("ONE")).await, Err(Error::Mismatch));
    assert_eq!(
        client.request(packet("TWO")).await,
        Err(Error::Rejected(42))
    );
    server.await.unwrap();
}
#[tokio::test]
async fn slow_subscriber_reports_lag_without_blocking_requests() {
    let (a, b) = duplex(4096);
    let client = Client::from_stream(
        a,
        Config {
            events: 2,
            ..Config::default()
        },
    )
    .unwrap();
    let mut events = client.subscribe();
    let server = tokio::spawn(async move {
        let mut f = Framed::new(b, LocoCodec::default());
        let p = f.next().await.unwrap().unwrap();
        for _ in 0..10 {
            f.send(packet("PUSH")).await.unwrap();
        }
        f.send(p).await.unwrap();
    });
    client.request(packet("PING")).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(8))
    ));
    server.await.unwrap();
}
#[tokio::test]
async fn stalled_write_is_bounded_and_shutdown_cleans_up() {
    let (a, _b) = duplex(1);
    let client = Client::from_stream(
        a,
        Config {
            write_timeout: Duration::from_millis(20),
            ..Config::default()
        },
    )
    .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(1), client.request(packet("STALL")))
            .await
            .unwrap(),
        Err(Error::Closed)
    );
    assert_eq!(client.state(), ConnectionState::Lost);
    let (a, mut b) = duplex(1024);
    let client = Client::from_stream(a, Config::default()).unwrap();
    drop(client);
    use tokio::io::AsyncReadExt;
    assert_eq!(
        timeout(Duration::from_secs(1), b.read_u8())
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}
#[tokio::test]
async fn graceful_shutdown_fails_outstanding_requests() {
    let (a, b) = duplex(1024);
    let client = Client::from_stream(a, Config::default()).unwrap();
    let c = client.clone();
    let request = tokio::spawn(async move { c.request(packet("WAIT")).await });
    let mut server = Framed::new(b, LocoCodec::default());
    server.next().await.unwrap().unwrap();
    client.disconnect().await;
    assert_eq!(request.await.unwrap(), Err(Error::Closed));
    assert_eq!(client.request(packet("AFTER")).await, Err(Error::Closed));
    sleep(Duration::from_millis(1)).await;
}

#[tokio::test]
async fn dangerous_configuration_is_rejected_without_spawning() {
    for config in [
        Config {
            requests: 0,
            ..Config::default()
        },
        Config {
            events: 65537,
            ..Config::default()
        },
        Config {
            request_timeout: Duration::MAX,
            ..Config::default()
        },
        Config {
            write_timeout: Duration::ZERO,
            ..Config::default()
        },
    ] {
        let (a, _b) = duplex(1);
        assert!(matches!(Client::from_stream(a, config), Err(Error::Config)));
    }
}

#[tokio::test(start_paused = true)]
async fn expired_request_is_not_acknowledged_by_a_late_response() {
    let (a, b) = duplex(1024);
    let client = Client::from_stream(
        a,
        Config {
            request_timeout: Duration::from_secs(1),
            ..Config::default()
        },
    )
    .unwrap();
    let c = client.clone();
    let pending = tokio::spawn(async move { c.request(packet("LATE")).await });
    let mut server = Framed::new(b, LocoCodec::default());
    let p = server.next().await.unwrap().unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    server.send(p).await.unwrap();
    assert_eq!(pending.await.unwrap(), Err(Error::Timeout));
    client.disconnect().await;
}
