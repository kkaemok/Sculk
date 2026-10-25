//! Async byte-stream adapter for explicitly selected LOCO GCM transport.
//! Packet boundaries and encrypted record boundaries are independent. This
//! adapter adds no bridge tasks and never skips a failed authentication tag.
use bytes::{Buf, Bytes};
use futures_util::{Sink, Stream};
use sculk_protocol::{handshake::ServerKey, secure::GcmRecords};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf},
    sync::watch,
};
use tokio_util::codec::Framed;

/// Independently framed encrypted stream with bounded write chunks (16 KiB).
/// Server key possession is not confirmed until a valid inbound GCM record.
pub struct SecureStream<S> {
    framed: Framed<S, GcmRecords>,
    read: Bytes,
}
impl<S: AsyncRead + AsyncWrite + Unpin> SecureStream<S> {
    /// Send the documented RSA key exchange over an existing approved connection.
    /// Cancellation/failure consumes the stream. There is no plaintext/TLS fallback.
    pub async fn establish(
        mut stream: S,
        key: &ServerKey,
        timeout: Duration,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Self, crate::connect::Error> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(crate::connect::Error::Config);
        }
        let (wire, codec) = key.initiate().map_err(|_| crate::connect::Error::Crypto)?;
        tokio::select! { biased;
            _ = crate::recovery::cancelled(&mut cancel) => return Err(crate::connect::Error::Cancelled),
            result = tokio::time::timeout(timeout, async { stream.write_all(&wire).await?; stream.flush().await }) => {
                result.map_err(|_| crate::connect::Error::Timeout)?.map_err(|_| crate::connect::Error::Crypto)?;
            }
        }
        Ok(Self::from_codec(stream, codec))
    }
    /// Attach an already-established record codec (also useful for local servers).
    /// This does not negotiate keys or validate their provenance.
    pub fn from_codec(stream: S, codec: GcmRecords) -> Self {
        Self {
            framed: Framed::new(stream, codec),
            read: Bytes::new(),
        }
    }
}
fn invalid(error: sculk_protocol::Error) -> io::Error {
    match error {
        sculk_protocol::Error::Io(e) => e,
        _ => io::Error::new(io::ErrorKind::InvalidData, "invalid encrypted LOCO record"),
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for SecureStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        dst: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if dst.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        for _ in 0..16 {
            if !this.read.is_empty() {
                let len = dst.remaining().min(this.read.len());
                dst.put_slice(&this.read[..len]);
                this.read.advance(len);
                return Poll::Ready(Ok(()));
            }
            match std::task::ready!(Pin::new(&mut this.framed).poll_next(cx)) {
                Some(Ok(bytes)) => this.read = bytes,
                Some(Err(error)) => return Poll::Ready(Err(invalid(error))),
                None => return Poll::Ready(Ok(())),
            }
        }
        // An adversary can send empty valid records: bound work per poll.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for SecureStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if data.is_empty() {
            return Poll::Ready(Ok(0));
        }
        std::task::ready!(Pin::new(&mut this.framed).poll_ready(cx)).map_err(invalid)?;
        let len = data.len().min(16384);
        Pin::new(&mut this.framed)
            .start_send(Bytes::copy_from_slice(&data[..len]))
            .map_err(invalid)?;
        Poll::Ready(Ok(len))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().framed)
            .poll_flush(cx)
            .map_err(invalid)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().framed)
            .poll_close(cx)
            .map_err(invalid)
    }
}
