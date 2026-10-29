//! Concrete selected-room validation used by the owner CLI. Authorization belongs
//! to its existing terminal gates; this helper never acquires credentials or sends.
use crate::{
    diagnostics::{Progress, Stage},
    ui,
};
use sculk_client::{
    messaging::{Event, Message, MessageType, ReceiveError},
    read_only::RoomId,
    session::Session,
};
use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
#[derive(Debug)]
pub enum Error {
    Room(sculk_client::read_only::Error),
    Receive(ReceiveError),
    Timeout,
    Cancelled,
    Clock,
    InvalidMessage,
    UnsupportedEvent,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Room(_) => "selected room query failed",
            Self::Receive(_) => "message reception failed",
            Self::Timeout => "controlled receive timed out",
            Self::Cancelled => "controlled receive cancelled",
            Self::Clock => "system clock unavailable",
            Self::InvalidMessage => "controlled message metadata invalid",
            Self::UnsupportedEvent => "unsupported event during controlled validation",
        })
    }
}
impl std::error::Error for Error {}
pub fn valid_controlled_message(message: &Message, room: RoomId, now: u64) -> bool {
    message.room == room
        && message.id.get() > 0
        && message.sender.get() > 0
        && message.kind == MessageType::Text
        && u64::try_from(message.sent_at_seconds).is_ok_and(|sent| sent.abs_diff(now) <= 300)
}
pub async fn receive_one(
    session: &Session,
    room: RoomId,
    timeout: Duration,
    mut cancel: watch::Receiver<bool>,
    progress: &Progress,
) -> Result<(), Error> {
    // Subscribe before CHATINFO so pushes during the query retain explicit lag.
    let mut events = session.message_events().map_err(Error::Receive)?;
    progress.stage.set(Stage::RoomSelection);
    tokio::select! {biased;_=ui::cancelled(&mut cancel)=>return Err(Error::Cancelled),result=session.room_info(room)=>{result.map_err(Error::Room)?;}}
    progress.completed(Stage::RoomSelection);
    progress.stage.set(Stage::MessageReception);
    println!("READY: send one controlled text in the authorized room using the official client. No content or identifiers will be printed.");
    let deadline = tokio::time::Instant::now() + timeout;
    let receive_cancel = cancel.clone();
    loop {
        tokio::select! {biased;
            _=ui::cancelled(&mut cancel)=>return Err(Error::Cancelled),
            _=tokio::time::sleep_until(deadline)=>return Err(Error::Timeout),
            event=events.recv(receive_cancel.clone())=>match event.map_err(Error::Receive)?{
                Event::Message(message) if message.room==room=>{
                    let now=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|Error::Clock)?.as_secs();
                    if !valid_controlled_message(&message,room,now){return Err(Error::InvalidMessage)}
                    progress.completed(Stage::MessageReception);
                    println!("Controlled receive passed: selected room, log/sender IDs, timestamp and text type validated. No content or identities recorded.");return Ok(())
                },
                Event::Message(_)=>{},Event::Unknown(_)|Event::Malformed(_)=>return Err(Error::UnsupportedEvent),
            }
        }
    }
}

#[cfg(test)]
#[path = "../../../crates/sculk-client/tests/support/mod.rs"]
mod support;
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use sculk_client::{
        session::{AccessToken, Grant},
        Config,
    };
    use sculk_protocol::{Command, LocoCodec, Packet};
    use tokio_util::codec::Framed;
    // Native local TLS + an explicitly synthetic provider; not Kakao authentication.
    async fn session(
        capacity: usize,
    ) -> (
        Session,
        Framed<tokio_rustls::server::TlsStream<tokio::io::DuplexStream>, LocoCodec>,
    ) {
        let (tls, acceptor) = support::tls_pair();
        let (a, b) = tokio::io::duplex(32768);
        let (_tx, cancel) = watch::channel(false);
        let (s, peer) = tokio::join!(
            Session::establish(
                a,
                "localhost",
                &tls,
                Duration::from_secs(1),
                Config {
                    events: capacity,
                    ..Config::default()
                },
                cancel.clone()
            ),
            acceptor.accept(b)
        );
        let mut s = s.unwrap();
        s.authenticate_with(
            AccessToken::new(
                "synthetic".into(),
                42,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
            Duration::from_secs(1),
            cancel,
            |_, _| async {
                Ok(Grant {
                    user_id: 42,
                    valid_for: Duration::from_secs(30),
                })
            },
        )
        .await
        .unwrap();
        (s, Framed::new(peer.unwrap(), LocoCodec::default()))
    }
    fn room_reply(id: u32, room: RoomId) -> Packet {
        let mut d = fixture().document().unwrap();
        d.clear();
        let mut info = d.clone();
        info.insert("chatId", room.get());
        d.insert("chatInfo", info);
        Packet::from_document(id, Command::new("CHATINFO").unwrap(), &d).unwrap()
    }
    fn fixture() -> Packet {
        let raw = include_bytes!("../../../tests/fixtures/node-msg-synthetic.bin");
        Packet::from_bytes(
            0,
            0,
            Command::new("MSG").unwrap(),
            raw[22..].to_vec().into(),
        )
        .unwrap()
    }
    fn fresh_message() -> Packet {
        let p = fixture();
        let mut d = p.document().unwrap();
        let log = d.get_document_mut("chatLog").unwrap();
        log.insert(
            "sendAt",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        );
        log.insert("message", "합성 테스트 👋🏽\nUTF-8");
        log.insert("attachment", "{\"설명\":\"합성\"}");
        Packet::from_document(0, Command::new("MSG").unwrap(), &d).unwrap()
    }
    #[tokio::test]
    async fn selected_room_is_queried_before_reception_and_milestones_stay_separate() {
        let (mut session, mut peer) = session(8).await;
        let room = RoomId::new(9_007_199_254_740_993).unwrap();
        let progress = Progress::default();
        let (_tx, cancel) = watch::channel(false);
        let server = tokio::spawn(async move {
            let query = peer.next().await.unwrap().unwrap();
            assert_eq!(query.command.as_str(), "CHATINFO");
            assert_eq!(
                query.document().unwrap().get_i64("chatId").unwrap(),
                room.get()
            );
            peer.send(room_reply(query.id, room)).await.unwrap();
            peer.send(fresh_message()).await.unwrap();
            assert!(peer.next().await.is_none());
        });
        receive_one(&session, room, Duration::from_secs(1), cancel, &progress)
            .await
            .unwrap();
        let m = progress.milestones();
        assert!(m.selected_room && m.controlled_message);
        assert!(!m.https_authentication && !m.loginlist && !m.shutdown);
        session.disconnect_checked().await.unwrap();
        progress.completed(Stage::Shutdown);
        server.await.unwrap();
        assert!(progress.milestones().shutdown);
    }
    #[tokio::test]
    async fn wrong_room_query_and_rejected_query_never_reach_ready_or_message_success() {
        for rejected in [false, true] {
            let (mut session, mut peer) = session(8).await;
            let room = RoomId::new(9_007_199_254_740_993).unwrap();
            let progress = Progress::default();
            let (_tx, cancel) = watch::channel(false);
            let server = tokio::spawn(async move {
                let query = peer.next().await.unwrap().unwrap();
                let mut reply = room_reply(query.id, RoomId::new(42).unwrap());
                if rejected {
                    reply.status = 65535;
                }
                peer.send(reply).await.unwrap();
                assert!(peer.next().await.is_none());
            });
            assert!(matches!(
                receive_one(&session, room, Duration::from_secs(1), cancel, &progress).await,
                Err(Error::Room(_))
            ));
            assert_eq!(progress.stage.get(), Stage::RoomSelection);
            assert!(
                !progress.milestones().selected_room && !progress.milestones().controlled_message
            );
            session.disconnect_checked().await.unwrap();
            server.await.unwrap();
        }
    }
    #[tokio::test(start_paused = true)]
    async fn timeout_after_room_query_is_not_message_success() {
        let (mut session, mut peer) = session(8).await;
        let room = RoomId::new(9_007_199_254_740_993).unwrap();
        let progress = Progress::default();
        let (_tx, cancel) = watch::channel(false);
        let server = tokio::spawn(async move {
            let query = peer.next().await.unwrap().unwrap();
            peer.send(room_reply(query.id, room)).await.unwrap();
            assert!(peer.next().await.is_none());
        });
        assert!(matches!(
            receive_one(&session, room, Duration::from_secs(1), cancel, &progress).await,
            Err(Error::Timeout)
        ));
        assert!(progress.milestones().selected_room && !progress.milestones().controlled_message);
        assert_eq!(progress.stage.get(), Stage::MessageReception);
        session.disconnect_checked().await.unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_during_room_query_does_not_advance_success() {
        let (mut session, mut peer) = session(8).await;
        let room = RoomId::new(9_007_199_254_740_993).unwrap();
        let progress = Progress::default();
        let (tx, cancel) = watch::channel(false);
        let server = tokio::spawn(async move {
            let query = peer.next().await.unwrap().unwrap();
            assert_eq!(query.command.as_str(), "CHATINFO");
            tx.send_replace(true);
            assert!(peer.next().await.is_none());
        });
        assert!(matches!(
            receive_one(&session, room, Duration::from_secs(1), cancel, &progress).await,
            Err(Error::Cancelled)
        ));
        assert!(!progress.milestones().selected_room && !progress.milestones().controlled_message);
        session.disconnect_checked().await.unwrap();
        server.await.unwrap();
    }
}
