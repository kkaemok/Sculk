# Bounded synchronization and recovery

Status: source-backed and offline tested. No current live KakaoTalk compatibility
has been established. Enable `sculk-client/synchronization` for bounded SYNCMSG
pages and processing state, `session-recovery` for the explicit reconnect coordinator,
and optionally `checkpoint-file` for an owner-private Unix checkpoint. Default
features and the raw/typed Phase 6 receive APIs remain available unchanged.

## The processing boundary

A room cursor is a nonnegative exact BSON Int64; zero is an initial query. Log IDs
are not assumed contiguous, and timestamps never identify messages. A `Request`
contains one room, committed starting cursor, observed upper log-ID watermark and
1..100 requested records. `Session::synchronize` performs one cancellable read-only
request. No automatic pagination or retry occurs. `Session::room_watermark` reads
CHATINFO.lastLogId without entering a room or changing unread state.

`Page` is separate from realtime `Event`. It retains the original response packet
and zero-copy message views. It checks command/status, integer IDs, room binding,
upper bound, isOK and local count limits; sorts one room's returned logs; removes
only byte-identical same-ID records; and rejects conflicting duplicates. Inclusive
cursor echoes/older records do not advance pagination. An incomplete page with no
progress fails. Missing optional chatLogs is accepted only with isOK=true, matching
the historical optional array schema. Unknown message types/metadata are retained.
MCHATLOGS is researched but not implemented: its older response lacks an explicit
completion flag and its request cannot bound count. No speculative fallback exists.

A proposed next cursor is not a processing checkpoint. The application must:

1. Call `Tracker::begin_page` with a parsed page starting at its committed cursor.
2. For each message in page order, call `observe`. Skip side effects only for a
   known `Duplicate`; process `New` and explicitly handle `OlderUntracked`.
3. After processing succeeds, call `acknowledge_history(page, index, store)`.
4. Call `finish_page` only after all returned entries are acknowledged. Check its
   actual `Gap` result; a server flag alone does not prove full recovery.

Checkpoint commits happen per successfully processed log. A failed store commit
leaves the in-memory cursor and page position unchanged. Side effects may already
have happened: their retry needs application idempotency keyed by `(room, logId)`.
Abandoning a page/reconnecting resumes from the last acknowledged entry rather than
the last received ID or server-returned pagination suggestion.

Realtime `acknowledge_live` remembers processed IDs, but advances the safe cursor
only when continuity is already established and prevId matches that cursor.
Otherwise it freezes the cursor and marks a suspected gap. A late/out-of-order ID
outside the bounded dedup window is `OlderUntracked`, not a fabricated duplicate.
Recently processed live messages may reappear in history: observe identifies them,
but acknowledge their history position to advance the safe checkpoint without
repeating the side effect. Startup/restart begins suspect, including with a durable
checkpoint; the in-memory dedup window is not a durable global ID database.

`Gap` distinguishes:

| Evidence | Meaning |
|---|---|
| Suspected | Startup, reconnect, lag, missing/mismatched predecessor or out-of-order delivery; numeric ID jumps alone prove nothing |
| ConfirmedUnprocessed | A server history response contains an actual previously unprocessed log in a suspect range |
| UnavailableHistory | Server minLogId is newer than the requested start; the missing historical window cannot be claimed recovered |
| None | All returned logs were acknowledged, isOK=true, the requested upper anchor was reached, and no newer lag/truncation is known; only this bounded window is covered |

No nonexistent message is synthesized. A complete page that omits the requested
upper anchor does not clear suspicion. Lag during page processing prevents that
page from clearing the newer gap. Truncation remains explicit. There is no promise
that a server returns all deleted, membership, permission or unknown control events.

## Reconnection sequence

`managed::Room` owns at most one Session and one subscription. It uses exclusive
mutable access to serialize initialization and refuses to replace an authenticated
session. On an explicit reconnect it closes the old connection, abandons partial
history state, retries only classified transient errors with the existing bounded
exponential full-jitter policy, then subscribes before any history read. No manager
or per-message Tokio task is spawned. Subscriptions are dropped during replacement.

A normal recovery application does the following:

1. Load its acknowledged checkpoint and create the room coordinator.
2. Call `reconnect` using an owner-approved connector and still-valid credentials.
3. Call `recover_page` to read an announced watermark and at most one bounded
   SYNCMSG page. Process it using the boundary above; continue explicitly within
   the configured budget, or surface the incomplete state.
4. Drain realtime events. Treat Lagged/disconnect/expiry as explicit outcomes,
   preserving unknown/malformed events. Decide whether owner policy permits a new
   reconnect. Recovery never happens by replaying the last arbitrary request.

Default recovery budget is eight page attempts / 256 requested records per session
generation, including failed/cancelled requests. Policy caps are 32 pages / 4096
records, eight connection attempts, 30-second maximum jitter and 300-second attempt
timeouts. Fixed bounds keep memory/work finite; a budget failure is reported.
Reconnection while history work is pending drops that request and retains the
acknowledged prefix. A new generation resets the explicit budget, not its checkpoint.

Only typed pre-login transport interruptions and well-understood dial I/O kinds
(connection reset/refused/aborted, timeout, broken pipe, not connected, EOF) are
transient. Permission/policy errors, certificate verification failures, rejected
credentials/device verification, invalid schemas, ambiguous LOGINLIST/overall
timeouts and expired credentials are permanent/intervention cases. Permanent
failure latches the manager. New credentials/approval require a new explicitly
constructed coordinator. Generic collapsed legacy Dial errors stay permanent.
The connector must reuse approved credentials; it must not run an automatic HTTP
password, device-registration or identity-rotation workflow.

`heartbeat` performs one bounded source-backed PING on an application timer.
It adds no idle polling/background timer; scheduling and reconnect decisions remain
explicit. It never replays a WRITE. Applications must not schedule concurrent
initializations or blindly wrap arbitrary operations in reconnect-and-retry.

## Memory, persistence and delivery limits

The coordinator shares the existing bounded broadcast ring. Pushes arriving during
history reads are buffered there; overflow returns an explicit lag count. The
bounded recent-ID set stores identifiers, not content. A page contains at most 100
views over one validated, at-most-1-MiB response, and the tracker may retain one
shared active-page packet. Application-retained pages/messages are outside these
bounds. Live messages preserve transport arrival order; history has per-room
ascending log-ID order. There is no unbounded live reorder buffer or multi-room
ordering promise. Unknown control events are best-effort raw delivery and cannot
be reconstructed from message history after loss.

`CheckpointStore` is a small compare-and-swap interface; `MemoryStore` is not durable.
Optional Unix `FileStore` stores just a format version, room and cursor. Use a
trusted, absolute 0700 directory. It rejects unsafe ownership, permissions,
symlinks, hardlinks, special files, corrupt/oversized data and competing writers.
Commit uses an exclusive 0600 temporary file, fsync, atomic rename and directory
fsync. A post-rename sync failure poisons the store rather than guessing durability.
Crash-stale lock/temp files require operator inspection and removal only after
confirming no writer remains. No automatic stale-lock takeover exists.

File I/O is synchronous and should run at startup or on one bounded blocking
worker; do not perform repeated fsync inside the network driver. No Redis,
PostgreSQL, SQLite, Docker, Android or new runtime dependency is introduced.

Live reception alone is best-effort. Replaying a supported retained history window
with durable acknowledgments can provide at-least-once application attempts, with
possible duplicate side effects between processing and checkpoint commit. Bounded
in-memory dedup helps within a process, but is not exactly-once processing. A bot
requiring durable exactly-once side effects needs an enforceable application
transaction/idempotency mechanism; this SDK does not claim one.

## Phase 8 checkpoint integrity and process restart

The Phase 7 version-1 file had syntax/range checks but could not detect a cursor
digit changing into another valid integer. Version 2 now stores canonical version,
room and cursor lines followed by SHA-256 of that exact payload. Full record length
remains bounded by 128 bytes, including maximal signed-Int64 IDs. The reader rejects
missing/incorrect digests, noncanonical/truncated/trailing data and version-1 files.
SHA-256 is supplied by the existing maintained sha2 dependency, now enabled by
checkpoint-file; it is not custom cryptography or a keyed authenticity guarantee.
An owner who can rewrite the file can forge its checksum. Existing private-directory,
no-follow, file-type/link/mode/size, exclusive-lock and fsync/rename checks still apply.

Version-1 state is never overwritten or automatically trusted/upgraded. Before
migration, the owner must independently verify the acknowledged processing boundary
from application records. Preserve the old directory, create a new private directory,
and initialize FileStore with only that verified boundary through its existing CAS
interface. If a boundary cannot be verified, restart from a conservative earlier
cursor and handle possible duplicate side effects. Do not merely compute a checksum
for an unverified old cursor. The receive-only CLI does not load these checkpoints.

A deterministic subprocess test exits without Rust destructors before processing,
after a simulated side effect but before acknowledgment, after a successful actual
acknowledgment, and with a valid orphan temporary checkpoint. The parent waits for
process termination before simulating explicit operator removal of the stale lock.
It verifies that unacknowledged work replays, acknowledged state reopens exactly,
and an orphan temp file is never adopted. The orphan is constructed using bytes from
the real writer and explicitly simulates an interruption before rename; this is not
power-loss injection into an actual filesystem commit. A side effect before commit
can repeat. No exactly-once or physical disk-crash durability claim is made.

`Client::disconnect_checked` and `Session::disconnect_checked` now expose local
shutdown success versus already-ended, transport failure, deadline and abort outcomes.
The existing disconnect methods remain available. Failed writes are still dropped
without implicit flush/replay. The validation CLI uses checked shutdown so a timeout
or failed close cannot silently become a verified clean-close milestone.
