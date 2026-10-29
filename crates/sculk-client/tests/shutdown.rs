use sculk_client::{Client, Config, ConnectionState, ShutdownError};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream, ReadBuf};
#[derive(Clone, Copy)]
enum Close {
    Normal,
    Fail,
    Stall,
}
struct Stream {
    inner: DuplexStream,
    close: Close,
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.close {
            Close::Normal => Pin::new(&mut self.inner).poll_shutdown(cx),
            Close::Fail => Poll::Ready(Err(io::Error::other(
                "private error source must not reach diagnostic output",
            ))),
            Close::Stall => Poll::Pending,
        }
    }
}
#[tokio::test(start_paused = true)]
async fn checked_shutdown_distinguishes_success_transport_failure_and_timeout() {
    for (mode, expected) in [
        (Close::Normal, Ok(())),
        (Close::Fail, Err(ShutdownError::Transport)),
        (Close::Stall, Err(ShutdownError::Timeout)),
    ] {
        let (a, mut b) = tokio::io::duplex(1024);
        let client = Client::from_stream(
            Stream {
                inner: a,
                close: mode,
            },
            Config {
                write_timeout: Duration::from_secs(1),
                ..Config::default()
            },
        )
        .unwrap();
        assert_eq!(client.disconnect_checked().await, expected);
        assert_eq!(
            client.state(),
            if expected.is_ok() {
                ConnectionState::Closed
            } else {
                ConnectionState::Lost
            }
        );
        assert_eq!(b.read(&mut [0_u8; 1]).await.unwrap(), 0);
        if let Err(error) = expected {
            assert!(!error.to_string().contains("private error"));
        }
    }
}
#[tokio::test]
async fn an_already_lost_connection_is_not_a_verified_clean_shutdown() {
    let (a, b) = tokio::io::duplex(1024);
    let client = Client::from_stream(a, Config::default()).unwrap();
    let mut state = client.connection_state();
    drop(b);
    while *state.borrow_and_update() == ConnectionState::Connected {
        state.changed().await.unwrap();
    }
    assert_eq!(
        client.disconnect_checked().await,
        Err(ShutdownError::Closed)
    );
}
