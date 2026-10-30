# Architecture

## Phase 10 script layer

`sculk-script -> sculk-bot -> sculk-client -> sculk-protocol` preserves the existing
ownership boundaries. Explicit startup loading feeds one lexer/indentation parser,
a typed AST, scope/type/resource analysis and immutable forward-jump instructions
with precompiled interpolation segments. `.sculk` and `.sc` share every stage.
Source/AST data is discarded; event handling never parses source text.

Installation registers ordinary bot event/command callbacks. The bot's additive
per-command prefix lists and bounded byte trie handle longest-prefix matching,
Unicode aliases, shared argument parsing and the existing permission registry.
There is no script command dispatcher or connection copy. A runtime invocation
owns its local slots and checks steps, time, cancellation and string/output bounds;
asynchronous messaging effects use only the explicit bot outbound adapter.
See [language/runtime design](scripting.md) and [controlled measurements](performance.md).

## Phase 9 bot layer

`sculk-bot -> sculk-client -> sculk-protocol` is a one-way dependency chain.
The bot owns application dispatch, startup command lookup, permission checks,
context and lifecycle. Transport/authentication/chat/history stay in client.
One intake future and one sequential worker share a finite queue, without spawned
bot tasks; overflow terminates explicitly. See [bot architecture/API](bot.md) for
ordering, cancellation, memory bounds and best-effort delivery. The optional
managed adapter accepts an owner-approved session and never reconnects or commits
checkpoints. Future `sculk-script` uses these same APIs with one implementation
for equivalent `.sculk`/`.sc` sources; Phase 9 adds no interpreter.

## Implemented vertical slice

`sculk-protocol` owns a 22-byte historical LOCO header and BSON payloads.
Command validation accepts unknown printable ASCII names of up to 11 bytes.
The decoder checks the 1 MiB default body limit before requesting more data,
validates BSON recursively through borrowed raw values (depth <=32), and uses
Bytes slices to avoid copying received bodies. No unsafe code is needed.
Unsupported body types fail explicitly. Header status remains raw unsigned bits.
An invalid frame is fatal; do not attempt resynchronization on the same stream.
The protocol fixture is synthetic, independently written from the researched
layout. Self-roundtrips alone are not evidence of service compatibility.

The optional `experimental-gcm` feature implements length-prefixed authenticated
records through RustCrypto AES-GCM, tested against a NIST known answer and
corruption. Phase 1/2 exposed this as a separate codec. Phase 3 wires it into explicitly
selected LOCO-GCM routes with caller-trusted RSA key setup; live endpoint/key
compatibility and account login remain unverified.
It cannot make a raw network connection safe by itself. Newer static research
reports TLS for chat and GCM for Trailer media; this codec is not a substitute
for verified chat TLS. GCM authenticates records
but does not provide replay prevention; the future session layer must do so.

`sculk-client` accepts AsyncRead+AsyncWrite streams. One task owns a Framed codec,
a bounded command queue, and a pending-response map. The actor does not spawn
per-event tasks or run user callbacks. Admission is fail-fast; deadlines include
queueing. IDs increase from 1 and never wrap; exhausted IDs terminate the stream.
Only both matching ID and command complete a response. Nonzero header status is
returned as a typed rejection; command-specific BSON statuses remain caller data.
Closing, malformed frames and write deadline failures resolve outstanding work.
Each write has a deadline because cancellation after a partial frame makes the
stream unusable; on failure the connection is dropped. There are no blind retries.

## Limits and delivery semantics

Default pending requests: 32. Queued requests: 32. Optional raw event ring: 64.
Bodies: 1 MiB. BSON nesting: 32. Channel settings are capped at 65,536.
Memory is bounded by capacities * maximum body size, plus framing/OS/runtime
buffers; defaults do not guarantee the normal-bot RSS target under maximum-size
traffic. Slow subscribers receive an explicit broadcast Lagged count. The latest
connection state uses a separate watch channel. This is an experimental diagnostic
packet stream, **not a reliable critical chat-event subscription**. There is no
lossless delivery claim, event deduplication, replay, or synchronization cursor.

Cancelling a request stops waiting; it does not retract bytes already written.
Closed reply senders are removed before admission or at deadline. Timed-out late
responses become unmatched packets. A timeout, transport failure, or cancellation
has unknown delivery outcome; the driver does not resend. Overloaded means the
specific request was not admitted/written. A successful correlated response is
acknowledgment of that request, not proof a recipient received a chat message.

The actor may spend up to write_timeout writing before receiving more packets.
Disconnect waits for that bounded write and then closes the stream and all
requests. Dropping the last Client closes its control channel and the stream.

`recovery::connect_with_retry` retries a supplied secure-session connector only
for explicitly transient failures, with finite attempts, exponential full jitter,
a cap, and cancellation during both connect and sleep. Permanent authentication
errors are never retried. It deliberately does not replay requests or pretend to
recover session state. A future session connector must supply an attempt timeout.

## Optional secure-session infrastructure (phase 2)

Enable `sculk-client/secure-session` explicitly. Default builds retain the small
transport-only dependency set. The feature adds tokio-rustls/ring, webpki-roots,
zeroize and BSON response inspection. rcgen is test-only. No new crate, service
or non-Rust runtime is required.

`TlsSettings` performs a deadline-bound, cancellable TLS handshake over a
caller-connected stream. Standard rustls certificate, hostname, time and
signature validation remains enabled. Explicit roots support private/test CAs;
there is no custom permissive verifier, hostname substitution, direct dialer,
plaintext downgrade or automatic GCM fallback. Default Mozilla roots do not
implicitly load a managed proxy CA; callers must add trusted CA material when
required. Chat TLS and experimental media GCM remain separate layers.

`Session::establish` only reaches SecureTransport. It does not establish an
account session. `authenticate_with` invokes one explicitly supplied provider
with a Client handle and an AccessToken. The provider is a trusted boundary:
it must implement legitimate authorization, validate the response and return
Grant only with evidence; it must not retain/clobber the stream, clone it for
unrelated traffic, log/persist tokens or bypass verification. No working current
KakaoTalk account/device provider is bundled. Unsupported steps stay unsupported.
The existing Client::authenticate still returns AuthenticationUnavailable.

State transitions: SecureTransport → Authenticating → Authenticated or
Failed(VerificationRequired/Rejected/Unsupported/InvalidResponse/Interrupted/
Expired/Cancelled/Timeout/InvalidGrant). Terminal sessions require a fresh secure
transport; authentication is never retried automatically. Known credential
expiration and the provider's grant lease bound authentication and requests.
No server TTL is invented. Grant account identity must match the token metadata.

One additional monitor task tracks transport loss and expiration after success;
state checks also validate expiry/connection synchronously so scheduling delays
cannot permit a new request after expiration. Requests in progress at expiry
are aborted with unknown delivery. Explicit disconnect cancels and joins the
monitor before publishing Closed. Dropping Session cancels the monitor and
signals driver termination; dropping an authentication future synchronously
marks it Cancelled and signals termination, including interrupted writes.
There are no per-event tasks or durable credential stores.

AccessToken uses bounded zeroizing String storage, redacted Debug and no Clone.
Providers can explicitly borrow its value. This does not zeroize every provider-
created copy, BSON Document, Bytes allocation, rustls buffer or allocator page;
there is no claim of comprehensive memory erasure. No secret values appear in
errors, state notifications or test output. Authentication acknowledgment parsing
borrows raw BSON and rejects duplicate relevant fields rather than silently
accepting the last duplicate through an owned Document.

## Deliberately deferred components

Credential acquisition, device/challenge completion, live validation of studied
LOGINLIST schemas and endpoint/key identity policy, persistent sessions, token refresh,
typed account events, chat send API and OpenChat moderation remain unverified.
A provider-driven mock grant never establishes current KakaoTalk compatibility.

A future bot crate can translate verified event schemas into bounded typed events.
Critical events need durable cursors or an explicit disconnect/resync overload
policy. Stable log IDs can support bounded deduplication only after replay rules
are verified. Room-scoped, profile, and account IDs must be distinct types.
A local blocklist cannot modify KakaoTalk's departed-member/kicked-member lists.
SQLite belongs in a moderation example, not this transport core. Echo/moderation
bots are deferred until real authentication and event schemas are verified.

## Phase 3: source-profile connection sequence

The optional `protocol-connect` feature enables secure-session plus protocol
handshake support. It adds no workspace crate, native runtime, external service
or per-record bridge task. Existing framing, actor, token and session lifecycle
remain in use.

`Options::connect` accepts a provider-consistent DeviceProfile, an already-
authorized AccessToken, its device UUID, explicit protection on Checkin/chat,
BookingSchema, TLS roots, request limits, timeouts and a policy-enforcing dialer.
Booking is always TLS with the exact endpoint's name. The stages are:

1. Dial approved Booking endpoint → TLS → GETCONF → validate bounded candidate
   lists and select first advertised host/port → disconnect Booking.
2. Dial approved selected endpoint → explicitly configured TLS or RSA/GCM →
   CHECKIN → validate status/assigned host/port → disconnect Checkin.
3. Dial approved assigned endpoint → explicitly configured TLS or RSA/GCM →
   send initial LOGINLIST → validate status/account identity → retain Session.

There is no direct socket operation, hidden fallback endpoint, automatic transport
probe/downgrade, credential acquisition, registration, token refresh or repeated
authentication attempt. Dialers must enforce endpoint/network/proxy policy on
**every** server-advertised address; syntactic host validation is not authorization.
DNS resolution and connecting through a permitted proxy belong to that dialer.

Discovery accepts source-backed `hosts[]` or `ticket.lsl[]` plus `wifi.ports[]`,
with at most 32 hosts and 32 ports. Ambiguous/duplicate relevant fields, invalid
ports and malformed endpoint names are errors. It does not retain raw discovery
payloads. Unsolicited packets remain bounded diagnostics and cannot complete
requests with a different command or ID. Source-side status fields are checked
for GETCONF/CHECKIN as well as LOGINLIST.

`ServerKey` imports bounded PKCS#1 RSA-2048 public keys supplied through a trusted
configuration path. Every GCM connection has a fresh random AES-128 key wrapped
with OAEP SHA-1/MGF1-SHA-1, empty label. The handshake is client-selected 256/16/3
and does not negotiate versions or include a standalone server acknowledgment.
The first valid GCM response confirms the negotiated key's possession; writing
the RSA handshake alone is not peer confirmation. No production RSA pin is
chosen implicitly from the test fixture. Library RSA operations are public-key
encryption only; private-key decryption is confined to the mock servers.

`SecureStream` implements AsyncRead/AsyncWrite over the existing GcmRecords codec.
It accumulates decrypted bytes independently of packet boundaries, so a LOCO
packet can span records or share a record with other packets. Outgoing writes
are chunked into at most 16 KiB plaintext records; incoming record size uses the
existing ~1 MiB bound, with the inner packet body independently capped at 1 MiB.
A tag/length error is fatal and not skipped. Empty authenticated records are not
EOF; work is bounded to 16 records per poll before yielding. Plaintext framing
and encrypted framing each retain bounded buffers, so capacity planning must
account for both. Credentials in serialized packet/TLS/cipher buffers are not
promised to be fully zeroized, consistent with the existing security model.

Overall and stage deadlines are at most five minutes, and credential expiration
bounds the entire sequence. The local session lease is explicit and capped by
credential expiration; it is not interpreted from undocumented sync tokens.
Failure/drop guards signal actor shutdown, including partially written exchanges.
Booking/Checkin actors are closed before moving on. On success the actor lifetime
transfers to the existing Session monitor without leaking a Client handle.

Initial LOGINLIST has empty sync arrays and zero cursor values. No raw token is
logged or persisted. The returned Session does not retain a room snapshot or
claim replay, automatic heartbeat, refresh, or long-running account stability.
Callers may issue PING explicitly through Session::request. Current heartbeat
and synchronization requirements require subsequent profile validation.

## Phase 4: concrete HTTP provider and integrated example

The optional android-auth feature contains Provider, AndroidProfile, DeviceIdentity,
Credentials and a bounded CONNECT proxy dialer. reqwest/rustls-native-roots supply
HTTPS, normal hostname/certificate checks and inherited proxy support. The HTTPS
origin is fixed; redirect and HTTP retry policies are explicitly disabled. A
private unit-test hook substitutes loopback only in tests, not in the public API.
Responses, including stalled bodies, have one request deadline and a 64 KiB cap.
Known duplicate/malformed fields fail typed deserialization. Errors contain no
response bodies, tokens, passwords, URLs or secret-bearing transport causes.

Provider states distinguish Ready, Authenticating, NeedsApproval, Approved,
RequestingChallenge, AwaitingDeviceConfirmation, Registering, Registered,
CredentialsReady, Refreshing, Expired and terminal Failed. Dropping an operation
future marks it Cancelled. Two account-login attempts maximum are possible: the
initial request and one after successfully approved registration. Registration
itself needs a separate explicit approval call and genuine server confirmation.
Only documented pending status with a bounded server interval permits another
poll (max ten/five minutes). One explicit refresh is supported; no idle/background
refresh task, automatic error recovery, forced login or repeated rejection occurs.

Identity is stable across restarts in a private owner-only Unix file. Passwords
remain caller owned; tokens and challenge codes use zeroizing redacted types.
Only device identity is persisted. Owner/symlink/mode/size checks prevent silently
replacing or reading an unsafe identity. Startup file operations run before the
runtime in the example; library callers can use spawn_blocking. Protocol payload
and HTTP-library buffer copies are not promised to be comprehensively erased.

Numeric expected account ID and the initial email are bound before approval.
Login must return the exact integer; credentials stay bound to account/device.
Refresh cannot switch that identity; final LOGINLIST must confirm it again.
Provider-supplied expiry is combined with an explicit local cap; absent server
expiry is labeled local policy. Refresh does not mutate a connected Session's
lease; use returned credentials for a new explicitly chosen connection.

The example sculk-auth-client is gated by its live Cargo feature and a separate
--live runtime flag. Help/check-config send nothing. Live mode requires an actual
terminal, explicit test-account authorization and a separate registration prompt;
password input is hidden. Native HTTP auth and LOCO metadata share one profile /
identity. The only live-success message follows the exact LOGINLIST account check.
The example disconnects immediately and sends no messages. Ctrl-C cancellation
propagates to provider, dialer and protocol setup.

CONNECT dialing is an explicit proxy route with exact destination allowlists and
bounded header reads; it does not over-read tunneled bytes or fall back to direct
TCP. Every discovered/assigned host is rechecked. Operator grants and key provenance
remain required. The provider and integrated example contain no Android, Docker,
Node or Python runtime dependency.

## Phase 5: auditable validation CLI and read-only subset

The example now has a strict mode/option parser. --dry-run (--check-config alias)
performs local configuration, owner-private file, profile, system-root and explicit
RSA SHA-256 pin checks, then exits without identity creation or network access.
Live mode remains an explicit terminal-only account authorization, with a separate
registration authorization. A required configured fingerprint binds the key file
before any HTTP login; matching a hash does not prove real-world provenance.

Registration confirmation reads the controlling terminal using a local owned
nonblocking File registered with Tokio readiness. It has a 120-second limit and
responds to cancellation even with partial input; no uncancellable stdin worker
remains. The one unsafe IO registration uses Tokio's required owned-descriptor
contract: the descriptor is never replaced/closed/exposed while registered. No
packet/crypto parser unsafe optimization was introduced. Approval codes are only
deliberately displayed on /dev/tty, never standard log streams or JSON reports.

Provider state output uses static labels and can coalesce watch updates. Protocol
stages are explicitly labeled booking/checkin/loginlist. --report creates a new
0600 no-follow file containing only mode, last stage, outcome and a verified-live
boolean. It refuses overwrite; no account ID, email, device UUID, credential,
approval code, room content or raw error is serialized. Post-login read cancellation
still closes the session before returning. Passwords, replaced credentials and
unneeded provider/token-source values are dropped earlier than in phase 4.

The optional read-only feature adds Session::chat_rooms, room_info and room_members.
Room IDs are positive signed Int64, pagination tokens are opaque exact integers,
arrays/strings are bounded and required fields/status/room binding are validated.
Only one page is fetched at a time. eof=false requires an explicit progressing
cursor; no complete-list claim follows from a partial page. The structs retain
only selected source-backed fields, do not fabricate names/titles, and do not
implement a globally stable identity assumption or automatic replay/history.
A closed, expired or unconfirmed Session cannot issue these requests. --read-only
in the live tool requests one LCHATLIST page and prints only count/eof, then closes.

## Phase 6 messaging boundary

The optional messaging module sits above framing and the unchanged raw broadcast
transport. `Events::recv` checks authenticated state, transport state and lease,
then selects on cancellation, expiry, state changes or the existing ring. It
parses MSG only after a consumer receives a packet. There is no dispatcher task,
callback, per-message spawn or second queue. Slow handlers cause explicit Lagged
rather than blocking network progress. A closed/expired session does not drain
queued content. Subscriptions begin after authentication and do not promise initial
LOGINLIST events or offline recovery.

`Message` owns one shared Packet and stores small typed fields. Text, attachment
and unknown metadata are borrowed from raw BSON; cloning a message does not clone
payload bytes. Malformed known events and unknown commands preserve the Packet.
Raw BSON/attachments are sensitive; redacted Debug is not a permission to log them.

The offline `TextRequest` builder encodes one pinned WRITE profile and classifies
one supplied request outcome. It neither sends nor retries. Packet ID is transport
correlation, message ID is the server logId, and client msgId is a separate field.
No server acknowledgment implies recipient delivery or exactly-once processing.
Existing generic Session::request remains a low-level, caller-controlled escape
hatch; use for live writes requires separate owner authorization.

## Phase 7 processing and recovery

The optional synchronization module sits above the unchanged protocol and secure
session APIs. It issues one finite SYNCMSG page, validates room/range/schema,
orders one room's history and retains raw response bytes. Message views use checked
raw-document byte ranges over a shared packet, avoiding repeated array scans and
payload copies. Ordinary MSG, history Page, dedup Observation and continuity Gap
remain distinct concepts; the transport actor never executes application handlers.

A Tracker stores one acknowledged room cursor, a bounded recent-ID set and at most
one shared active-page packet. It commits through a small compare-and-swap store
only after explicit application acknowledgment. Suspected gaps freeze realtime
cursor advancement. Server response evidence can confirm unprocessed history;
truncation, missing upper anchors and newer lag cannot be called recovered.

The optional managed Room owns one Session/subscription, serializes reconnect,
cleans up the old generation, uses finite full-jitter retries for conservatively
classified transport failures and subscribes before read-only catch-up. Permanent
failures latch; expired credentials need intervention. No connector retries an
arbitrary operation, refreshes credentials automatically, or resends a WRITE.
Bounded page/request budgets survive cancellation until the next explicit generation.
Heartbeat is one caller-scheduled PING, not a background polling/dispatcher task.

The optional Unix checkpoint adapter is outside sculk-protocol. It writes room/cursor
in a private single-writer file using fsync/rename, without SQLite or new runtime
dependencies. A crash between an application side effect and checkpoint commit can
repeat that effect: at-least-once attempts within supported history are conditional,
not exactly-once processing. Unknown control-event recovery remains unsupported.

Explicit transport disconnect now performs bounded AsyncWrite shutdown (including
TLS close_notify). Abort, corruption and failed writes still drop immediately;
a cancellation during graceful close interrupts it. No partially failed WRITE is
flushed as an implicit retry. No per-message Tokio tasks were introduced.
