# Bot framework (Phase 9)

`sculk-bot` implements offline-tested Rust event and command dispatch above
`sculk-client`. It starts no account authentication and adds no live WRITE adapter.
Protocol, transport, chat/history and authentication remain in the existing crates.

## Public API

```rust
use sculk_bot::{Bot, Config, Command, PermissionId};

let mut bot = Bot::new(Config::default())?;
let ping = PermissionId::new("commands.ping")?;
bot.permission(ping.clone(), true)?;
bot.register(Command::new("ping").alias("p").require(ping), |ctx| async move {
    ctx.reply("Pong!").await?;
    Ok(())
})?;
bot.command("help", |ctx| async move {
    ctx.reply("Available commands: ping, help").await?;
    Ok(())
})?;
```

This registers handlers. Configure an explicit `Outbound` adapter before running
them; the default adapter returns `Unavailable`. `MockAccepted` means local mock
acceptance only. `ServerAcknowledged(TextReceipt)` requires the application adapter
to supply actual client receipt evidence; it does not imply recipient delivery or
read status. Adapters must be drop-safe, preserve unknown delivery and never retry
submitted writes. `reply` validates text locally and returns unknown delivery if
cancelled after submission. Cancellation can also drop the entire handler before
it observes that result; partial effects are never automatically replayed.

`Bot::on(EventKind, handler)` registers event callbacks. `on_error` receives a
`Context` with `Failure`: unknown command, argument parse error, permission denial,
event-handler failure or command failure. Dispatch continues after returned errors.
Error-handler failures are counted without recursion. The finite `RunReport`
counts failures and retains only the first; configure error handlers for individual
notifications. No default message/account/room logging occurs.

`Context` exposes current event, optional room/sender/message/content, entered
command name, borrowed arguments, permission checks and a cancellation receiver.
Message bytes stay in the client's shared packet. `parse(text, prefix)` is also
public; its result borrows its original input so argument ranges cannot be applied
to a different message. Unquoted/unescaped arguments borrow the original text;
quoted/escaped arguments allocate decoded strings.

## Commands and permissions

Prefixes match the beginning exactly; leading whitespace is not ignored. Prefixes
are 1..64 non-whitespace UTF-8 bytes. Names/aliases use 1..64 ASCII letters, digits,
`_` or `-`. Case-insensitive mode folds ASCII names and lookup tokens. Registration
compiles a lookup table once, rejects collisions atomically and limits it to 128
commands with 16 aliases each. Only real-time text-type messages invoke commands.

Arguments use whitespace separation, single/double quotes, concatenated segments
and backslash escaping of the next Unicode scalar, including inside quotes. Empty
quoted arguments are preserved. There is no shell, variable or glob expansion.
Unterminated quotes, trailing escapes, invalid names, more than 64 arguments or
command text above 16 KiB fail explicitly.

Register at most 128 `PermissionId`s with explicit defaults. Commands requiring an
unregistered permission cannot be registered; unknown context checks fail closed.
Every required permission is checked before invoking the command. General message
handlers run before command authorization and must check permissions themselves
before effects. No KakaoTalk administrator/moderator role is inferred.

`PermissionStore` is a synchronous, nonblocking lookup boundary; implementations
must not perform disk/network I/O there. `MemoryPermissions` holds at most 4096
rules. Applicable global/room denials override all grants, then grants override the
registered default. Sender IDs are room-scoped in client: prefer room-scoped rules.
Global rules across equal numeric IDs are an explicit operator choice.

## Dispatch, ordering and resource bounds

`Bot::run(source, cancellation)` consumes the bot and its owned `EventSource`.
One intake future and one sequential worker run concurrently within it: the bot
spawns **zero tasks**. Event handlers run in registration order, then the matching
command, then the next event. Global FIFO by source admission preserves per-room
received order. It does not sort server IDs or promise cross-room server ordering.
Concurrent producers have no ordering guarantee before admission.

The dispatch queue defaults to 16 slots (1..128), with a default 64 KiB packet-body
admission limit per event (maximum 1 MiB). An immediately-ready source yields
cooperatively after each admission. Slow async callbacks allow intake/network
progress until this finite queue fills. Overflow stops intake, cancels the active
callback, discards queued work, cleans up the source and reports `QueueOverflow`.
Oversized input reports `EventTooLarge`. Neither is silently converted to success.
Use `received`, `admitted`, `completed` and `duplicates` to account for work;
`admitted - completed - duplicates` includes interrupted or queued events.

The independent client network actor never executes bot callbacks. Its own ring
loss is forwarded as `Event::Lagged(n)` before subsequent retained events. The
application decides whether to stop effects and reconcile supported history.
Unknown/malformed packets remain intact. Unknown control-event recovery is not
guaranteed. Callbacks must return `HandlerError` rather than panic, block a runtime
thread or spawn uncontrolled work. CPU-bound code that never yields cannot be
preempted. Retained contexts and application-spawned tasks are outside bot bounds.

Recent `(room, message ID)` pairs are suppressed in a per-run window, default 256
(1..4096), including failed command attempts. Older IDs outside that window can be
processed again. No timestamps or contiguous IDs are used for deduplication.
State is not persistent; rebuilding a bot does not authorize replay. Delivery is
**best effort with bounded duplicate suppression**, not exactly once or guaranteed
at least once. Effects need application idempotency; dispatch never commits a
synchronization checkpoint by itself.

## Lifecycle and managed client

Finite source EOF drains admitted events. Explicit cancellation, including loss
of its sender, interrupts receive and callbacks even during EOF drain. Cleanup has
a default 2-second deadline (maximum 30 seconds); its result is reported separately.
EOF can wait for a slow callback; use cancellation to interrupt it. Dropping the
run future drops owned source/callbacks immediately without claiming graceful close.

`channel(capacity)` is a bounded local source (1..1024) for offline input or another
application adapter. Producers must respect backpressure and cancellation. Its
queue and the dispatch queue are separate memory bounds. Neither idle intake nor
idle dispatch polls in a background loop.

Enable `sculk-bot/managed-client` and pass an already explicitly connected
`sculk_client::managed::Room` to `ManagedSource::new`. The adapter observes actual
session state/generation and an initial tracker snapshot, forwards typed/unknown
events and lag, and reports terminal connection loss/expiry. It never authenticates,
reconnects, heartbeats, fetches history, acknowledges checkpoints, writes messages,
enters a room or marks messages read. Permanent authentication failure/expiry
requires explicit owner intervention. The manager's new `shutdown_checked` reports
transport-close errors; legacy `shutdown` remains compatible. Room/state accessors
start no network operations.

Recovery remains a deliberate application phase using the existing manager,
tracker and store APIs, before transferring ownership to a run. Process/acknowledge
history at the application's actual effect boundary. Applications can emit
`Event::Synchronization` snapshots through their own source. Never reclassify
history as ordinary commands. A fresh run rebuilds registrations and uses a newly
approved source after cleanup; there is no hidden restart or replay loop.

## Offline usage and future scripting

```sh
cargo run -p sculk-basic-bot --locked
cargo test -p sculk-bot --all-features --locked
cargo bench -p sculk-bot --bench dispatch --locked
```

The example announces offline mode, feeds synthetic `!ping`/`!help`, checks local
permissions and uses a console mock. The TLS integration test uses a **synthetic
authentication grant**, not real LOCO compatibility. See [verification](verification.md)
and [performance](performance.md).

Future `sculk-script` depends on `sculk-bot`: register the same `Command` metadata,
async callbacks, `EventKind` handlers and permission IDs; read conditions and
expressions from `Context`; implement effects through its permission/outbound
boundaries. Variables/functions and compiled programs belong in that future host.
`.sculk` is the official extension and `.sc` an equivalent alias to **the same**
parser, compiler, runtime and semantics. Phase 9 adds no loader, language parser,
VM or interpreter. The next slice should implement a shared frontend, diagnostics
and execution budgets for one command/event/effect using these APIs.
