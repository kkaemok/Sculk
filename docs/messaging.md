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
attachments, supplementary fields and unknown metadata are preserved; no JSON
attachment schema, title, identity or missing value is fabricated. Resource bounds
are the framing limit (1 MiB) and at most 128 fields in each top/log container.
These are local safety policies, not researched server limits.

## Text request foundation

```rust
use sculk_client::{messaging::TextRequest, read_only::RoomId};
let request = TextRequest::new(RoomId::new(123).unwrap(), "Hello".into()).unwrap();
let packet = request.packet().unwrap(); // offline encoding; sends nothing
assert_eq!(packet.command.as_str(), "WRITE");
```

The exact Agent Messenger text profile is `{chatId:Int64,msg:String,type:Int32(1),
noSeen:false}`. Older node-kakao requests add client msgId and default noSeen=true;
Sculk does not blend these profiles or claim current server acceptance. Input
policy allows 1..16384 UTF-8 bytes with no NUL; messages are never split.

There is no high-level live text sender or send flag in this milestone. Existing
low-level Session::request can transmit arbitrary commands, so application owners
must maintain the separate live-send authorization boundary. All submitted WRITE
tests use local TLS and an explicitly synthetic authentication provider.

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

The next smallest bot milestone is an owner-observed authenticated receive-only
run in one test room, with sanitized command/cursor evidence. Then validate initial
sync, heartbeat, gap reconciliation and duplicate handling before a moderation
consumer relies on continuity. A single live text write requires separate owner
authorization after connection validation; no moderation operation is authorized
or implemented by this milestone.

## Phase 7 companion APIs

Optional `synchronization` now provides bounded, separately typed history pages and
acknowledgment-based processing checkpoints. `session-recovery` supplies explicit
single-session reconnect/resubscription and caller-scheduled PING. These do not
change the Phase 6 `message_events()` subscription boundary or make its raw stream
lossless. See [recovery](recovery.md) for dedup windows, suspected versus confirmed
gaps, server history truncation and conditional at-least-once processing semantics.
The owner CLI also adds selected-room --receive-one; live sending remains absent.
