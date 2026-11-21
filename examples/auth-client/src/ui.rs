//! No blocking Tokio stdin worker: canonical terminal input is read via readiness.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    time::Duration,
};
use tokio::sync::watch;
use zeroize::Zeroizing;

pub async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() || cancel.changed().await.is_err() {
            return;
        }
    }
}
#[cfg(unix)]
fn terminal() -> io::Result<File> {
    use std::io::IsTerminal;
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open("/dev/tty")?;
    if !file.is_terminal() {
        return Err(io::ErrorKind::NotConnected.into());
    }
    Ok(file)
}
#[cfg(not(unix))]
fn terminal() -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}
/// Code is sent only to the controlling terminal, never stdout/stderr or a report.
pub fn show_challenge(code: &str) -> io::Result<()> {
    if code.is_empty()
        || code.len() > 128
        || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut tty = terminal()?;
    writeln!(
        tty,
        "Confirm this approval code on your legitimate device: {code}"
    )?;
    tty.flush()
}
#[cfg(unix)]
pub async fn registration_approval(cancel: watch::Receiver<bool>) -> io::Result<bool> {
    let mut tty = terminal()?;
    writeln!(tty,"New sub-device registration requested. Confirm only for your authorized account.\nType register to authorize challenge generation and bounded confirmation polling:")?;
    tty.flush()?;
    confirmation_from_file(tty, cancel, b"register").await
}
#[cfg(unix)]
async fn confirmation_from_file(
    tty: File,
    mut cancel: watch::Receiver<bool>,
    answer: &[u8],
) -> io::Result<bool> {
    // SAFETY: this owned File stays exclusively inside the local AsyncFd. No code
    // replaces/closes it or exposes its raw descriptor before the AsyncFd drops.
    let tty = unsafe { tokio::io::unix::AsyncFd::register(tty) }.map_err(io::Error::from)?;
    let operation = async {
        let mut input = Zeroizing::new(Vec::new());
        loop {
            let mut ready = tty.readable().await?;
            let byte = match ready.try_io(|inner| {
                let mut file = inner.get_ref();
                let mut byte = [0];
                file.read(&mut byte).map(|n| (n, byte[0]))
            }) {
                Ok(value) => value?,
                Err(_) => continue,
            };
            if byte.0 == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if byte.1 == b'\n' {
                return Ok(input.strip_suffix(b"\r").unwrap_or(&input) == answer);
            }
            if input.len() >= 64 {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            input.push(byte.1);
        }
    };
    tokio::select! {biased;
        _=cancelled(&mut cancel)=>Err(io::ErrorKind::Interrupted.into()),
        result=tokio::time::timeout(Duration::from_secs(120),operation)=>result.unwrap_or_else(|_|Err(io::ErrorKind::TimedOut.into())),
    }
}
#[cfg(not(unix))]
pub async fn registration_approval(_cancel: watch::Receiver<bool>) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Review the exact room or escaped message only on the controlling terminal.
#[cfg(unix)]
pub async fn send_approval(
    room: i64,
    text: &str,
    cancel: watch::Receiver<bool>,
) -> io::Result<bool> {
    let mut tty = terminal()?;
    writeln!(
        tty,
        "Confirm the configured room {room} and your authorization to send there. Type room:"
    )?;
    tty.flush()?;
    if !confirmation_from_file(tty, cancel.clone(), b"room").await? {
        return Ok(false);
    }
    let mut tty = terminal()?;
    writeln!(tty, "Review the exact test message (escaped): {text:?}\nType send to submit once using the historical basic WRITE profile:")?;
    tty.flush()?;
    confirmation_from_file(tty, cancel, b"send").await
}

#[cfg(not(unix))]
pub async fn send_approval(_: i64, _: &str, _: watch::Receiver<bool>) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    fn pair() -> (File, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        b.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
        a.set_nonblocking(true).unwrap();
        let fd: OwnedFd = a.into();
        (File::from(fd), b)
    }
    #[tokio::test]
    async fn pending_approval_cancels_and_closes_owned_input() {
        let (file, mut peer) = pair();
        let (tx, cancel) = watch::channel(false);
        peer.write_all(b"regis").unwrap();
        let task = tokio::spawn(confirmation_from_file(file, cancel, b"register"));
        tokio::task::yield_now().await;
        tx.send(true).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        let mut byte = [0];
        match peer.read(&mut byte) {
            Ok(0) => {}
            // Closing a Unix stream with unread partial input may reset the peer.
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
            other => panic!("approval input was not closed: {other:?}"),
        }
    }
    #[tokio::test]
    async fn approval_requires_complete_exact_word_and_bounds_input() {
        for (input, expected) in [
            (b"register\n".as_slice(), true),
            (b"no\n".as_slice(), false),
        ] {
            let (file, mut peer) = pair();
            let (_tx, cancel) = watch::channel(false);
            peer.write_all(input).unwrap();
            assert_eq!(
                confirmation_from_file(file, cancel, b"register")
                    .await
                    .unwrap(),
                expected
            );
        }
        let (file, mut peer) = pair();
        let (_tx, cancel) = watch::channel(false);
        peer.write_all(&[b'x'; 65]).unwrap();
        assert_eq!(
            confirmation_from_file(file, cancel, b"register")
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
