# Messaging foundation

Status: implemented and offline-tested behind `sculk-client/messaging` (which
includes read-only/secure-session support). Current KakaoTalk acceptance is
unverified. No live login, message reception or message sending was performed.

## Receiving

```rust,no_run
use sculk_client::{messaging::{Event, ReceiveError}, session::Session};
use tokio::sync::watch;

async fn observe(session: &Session, cancel: watch::Receiver<bool>) -> Result<(), ReceiveError> {
    let mut events = session.message_events()?;
    loop {
        match events.recv(cancel.clone()).await {
            Ok(Event::Message(message)) => {
                let _room = message.room;         // typed exact Int64
                let _sender = message.sender;     // room-scoped identity
                let _log_id = message.id;         // server logId, not packet.id
                let _unix_seconds = message.sent_at_seconds;
                let _text = message.text();       // borrowed Option<&str>
                let _attachment = message.attachment(); // raw, possibly JSON
                let _metadata = message.packet(); // unknown fields retained
                // Handle promptly or apply your own bounded worker policy.
            }
            Ok(Event::Unknown(_packet)) => { /* future command or late reply */ }
            Ok(Event::Malformed(_packet)) => { /* MSG schema did not validate */ }
            Err(error) => return Err(error), // includes explicit Lagged(n)
        }
    }
}
```

The network driver continues independently of handlers. One bounded raw broadcast
ring holds the packets; decoding happens only when a subscriber asks. No callback
runs in the driver. Lag means packets were overwritten; callers must stop or
explicitly reconcile the gap before treating later events as continuous. Continuing
after Lagged resumes at the oldest retained packet, with no automatic history fetch.
Dropping a pending receive is safe. Cancellation includes dropping its sender.
Closure/expiry stops reception even with queued packets. Multiple subscriptions
share payload bytes, but application-retained events are outside the ring's bound.

Subscribe after authentication, from the current point. Initial LOGINLIST pushes,
messages before subscription, deduplication, heartbeats, reconnect/replay, ordering
across sessions and durable cursors are unsupported. This is a receive foundation,
not a complete bot event stream. Do not use feedType values to infer moderation
actions; unknown commands such as KICKOUT stay raw.

The typed MSG subset requires outer chatId and nested chatLog with logId, authorId,
type and sendAt. Text type 1 requires a message string; empty incoming text is
preserved. Optional outer logId/nested chatId must agree when present. Message
attachments, supplementary fields and unknown metadata are preserved. Resource bounds
are the framing limit (1 MiB) and at most 128 fields in each top/log container.
These are local safety policies, not researched server limits.

`Message::quote()` decodes the historical type-26 JSON subset on demand.
`Message::content()` additionally decodes mentions, thread metadata, photo,
video, audio, file, multi-photo and sticker metadata. A historical KakaoLink
type is recognized, with the original payload available for inspection.
Optional metadata is bounded to 32 KiB per JSON string. The original
attachment, supplement and BSON remain available when decoding fails or fields
are unknown. Media keys, URLs and display names are untrusted content.

## Text request foundation

```rust
use sculk_client::{messaging::TextRequest, read_only::RoomId};
let request = TextRequest::new(RoomId::new(123).unwrap(), "Hello".into()).unwrap();
let packet = request.packet().unwrap(); // offline encoding; sends nothing
assert_eq!(packet.command.as_str(), "WRITE");
```

The historical Agent Messenger text profile is `{chatId:Int64,msg:String,type:Int32(1),
noSeen:false}`. Older node-kakao requests add client msgId and default noSeen=true;
Sculk does not blend these profiles or claim current server acceptance. Input
policy allows 1..16384 UTF-8 bytes with no NUL; messages are never split.

An owner-authorized application can submit text using the session's profile:

```rust,no_run
use sculk_client::{messaging::{SendError, TextReceipt}, read_only::RoomId, session::Session};
use tokio::sync::watch;

async fn send(session: &Session, room: RoomId, cancel: watch::Receiver<bool>)
    -> Result<TextReceipt, SendError>
{
    session.send_text(room, "Hello!", cancel).await
}
```

`Session::text_sender()` and `Session::message_sender()` provide handles to the
same sender. Admission checks
authentication and lease, cancellation and capacity. Once admitted, a
cancellation can return unknown delivery even when the driver discards the
queued request before writing. A dropped call cannot undo a write. A sender
handle becomes unusable when its session closes or expires. The tablet profile
adds a client message ID, scope and other source-backed headers to its WRITE.
Those IDs persist across reconnects within a connection owner and do not
provide a durable idempotency key. [Protocol profiles](advanced-compatibility.md).

## Compose a reply and mention

```rust,no_run
use sculk_client::{
    messaging::{Message, MessageBuilder, SendError, TextReceipt},
    read_only::{OpenLinkId, RoomMemberId},
    session::Session,
};
use tokio::sync::watch;

async fn reply(
    session: &Session,
    source: &Message,
    member: RoomMemberId,
    open_link: Option<OpenLinkId>,
    cancel: watch::Receiver<bool>,
) -> Result<TextReceipt, SendError> {
    let message = MessageBuilder::new()
        .text("Thanks, ")
        .mention(member, "홍길동")
        .text("!")
        .reply_to(source, open_link)
        .build()?;
    session.message_sender()?.send_message(source.room, &message, cancel).await
}
```

Use a room-scoped member ID from the destination room and an independently
verified OpenChat link when applicable. The builder rejects cross-room
quotation and limits mention names to the BMP because the wire length of
other names is unverified. IDs stay exact in BSON and JSON. Existing scripts
continue to use the same command dispatch and text outbound. Media and
sticker sends, reactions, thread sends and rich-card composition require
further protocol evidence and owner validation.

For a Rust or script bot, pass `SessionSource::select` an authenticated
session and an authorized room. The adapter verifies `CHATINFO`, filters
incoming messages to the selected room, and ends on a reported gap or lost
connection. To enable outbound effects expressly, install
`source.text_outbound(interval)` on the bot. The adapter limits attempts per
interval, scopes sends to that room and uses the same `Outbound` contract as
mock adapters. Acknowledged WRITE receipts have server evidence from the active
peer. Use the separately authorized owner workflow before relying on any live
compatibility.

`TextRequest::outcome(result)` classifies one caller-submitted result:

| Result | Meaning |
|---|---|
| TextReceipt | Server acknowledged a room-bound log ID and Unix-second timestamp; not recipient delivery/read |
| NotSent | Session admission/authentication failed before submission/write |
| Rejected(status) | Explicit nonzero status; raw integer retained |
| TimeoutUnknown | Deadline passed after submission; never automatically resend |
| DeliveryUnknown | Interrupted/expired connection or missing/malformed/mismatched reply; never automatically resend |

No automatic retry, idempotency key, exactly-once promise, bulk send, attachment
send, read acknowledgment, moderation command or inferred rejection semantics is
implemented. Late WRITE replies remain unknown raw events; they cannot be confused
with MSG or automatically update an already returned timeout outcome.

## Owner validation

Follow authentication.md for private configuration, key provenance, route grants,
account approval and separate registration confirmation. `--listen 30` adds a
receive-only window after LOGINLIST; reports/stdout retain counts and static
failure categories, not room IDs, senders, tokens or content. A lease ending during
the window returns an explicit failure. `--dry-run` cannot enable reception.

After the read-only and controlled receive stages pass, the owner can try
`--send-one DELAY_SECONDS`. Prepare an owner-private UTF-8 text file and set
the absolute `test_message_file` and exact `test_room_id` in the local private
configuration. The owner CLI verifies the selected room after login and asks
for separate room and exact text confirmations on the controlling terminal.
It waits the selected 1–60 seconds before submitting a single WRITE. Use a
room dedicated to this test. The report records the acknowledgment bit, failure
category and known status, with message contents and room IDs kept off stdout
and out of the report. An unknown outcome requires manual reconciliation.
[Authentication and routing](authentication.md).

## Phase 7 companion APIs

Optional `synchronization` now provides bounded, separately typed history pages and
acknowledgment-based processing checkpoints. `session-recovery` supplies explicit
single-session reconnect/resubscription and caller-scheduled PING. These do not
change the Phase 6 `message_events()` subscription boundary or make its raw stream
lossless. See [recovery](recovery.md) for dedup windows, suspected versus confirmed
gaps, server history truncation and conditional at-least-once processing semantics.
The selected-room receive check is available with `--receive-one`.
