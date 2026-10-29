# Research record

Inspected 2026-10-24. Each repository was fetched with a shallow HTTPS clone.
Dates below are the fetched HEAD commit dates, not an assertion of active support.
No third-party application was executed and no live KakaoTalk session was tested.
README claims, source evidence, and live evidence are different categories.

| Project / exact snapshot | HEAD / last commit | License inspected | Source or documented scope | Sculk decision |
|---|---|---|---|---|
| [storycraft/loco-protocol-rs](https://github.com/storycraft/loco-protocol-rs/tree/849ba913d9563bb856c62a85dd4299f07fdea18e) | `849ba913d956` (2023-10-15) | MIT | Framing, RSA-OAEP/SHA-1 + AES-128-CFB; passive-maintenance badge. | Independent wire-format implementation; do not adopt old crypto defaults. |
| [storycraft/futures-loco-protocol](https://github.com/storycraft/futures-loco-protocol/tree/bcb8b16f51632f8d1d4786421adeb2a8ed3c848a) | `bcb8b16f5163` (2023-10-15) | MIT | futures-io stream adapter, session request correlation, flume/oneshot. | Reference only; native Tokio driver avoids compatibility adapters. |
| [storycraft/talk-loco-client-rs](https://github.com/storycraft/talk-loco-client-rs/tree/99b4f6f52e7d78814cef7541684aef088c14ec42) | `99b4f6f52e7d` (2022-01-08) | MIT | BSON command sets, booking/checkin/talk/media clients; old loco-protocol 5 dependency. | Reference only; no evidence of current login. |
| [KiwiTalk/KiwiTalk](https://github.com/KiwiTalk/KiwiTalk/tree/4e565ca6bfff72f6085f1b0a4a767558c78ff450) | `4e565ca6bfff` (2026-10-22) | Apache-2.0 | Rust headless model/updater/database, LOCO command client and HTTP auth, wrapped by GUI. | Backend useful design reference; do not inherit GUI or outdated transport. |
| [storycraft/node-kakao](https://github.com/storycraft/node-kakao/tree/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a) | `8512e4699e2c` (2023-11-01) | MIT | 22-byte codec, BSON, rooms/users, messaging, OpenChat commands/events. | Source cross-check and API reference; no Node runtime dependency. |
| [play2fly/KakaoForge](https://github.com/play2fly/KakaoForge/tree/4b774ea40b1347280fadb685415436584093118b) | `4b774ea40b13` (2026-02-09) | Custom noncommercial/no-abuse | README documents messages, joins/leaves/kicks, reactions, media, reconnect controls. | Documentation and license only; no source copied or integrated. |
| [JungHoonGhae/openkakao-cli](https://github.com/JungHoonGhae/openkakao-cli/tree/6ca6ea2fd05a6660c4a80c0b61b43b416653d1d5) | `6ca6ea2fd05a` (2026-10-11) | MIT | Rust CLI: experimental direct LOCO plus macOS Accessibility/notification/local DB integrations. | Recent wire research useful; latest docs explicitly report broken direct login. |
| [minsuho/KakaoDBpy](https://github.com/minsuho/KakaoDBpy/tree/d5acce81d2159579f7ab6185cc882f063adb0c2f) | `d5acce81d215` (2024-08-16) | Deep Dark Type A (custom noncommercial) | Android local DB observer and external JavaScript sender, not standalone LOCO. | Behavioral research only; no source reused. |

## Verified source findings

* `loco-protocol-rs/specification.md` describes LE ID (u32), status (u16),
  command (11 NUL-padded bytes), type (u8), body length (u32), then body.
  `src/command/mod.rs` corroborates field sizes. `node-kakao/src/network/loco-packet-codec.ts`
  independently uses offsets 0/4/6/17/18, with a 22-byte total header.
  `node-kakao/src/packet/bson-data-codec.ts` encodes BSON as type 0 and accepts type 8 on decode. Sculk currently
  accepts only type 0; type 8 fails explicitly pending verified semantics.
  `openkakao-cli/src/loco/packet.rs` uses the same offsets. These three code
  paths corroborate the historical framing, not current live compatibility.
* `loco-protocol-rs/src/secure/client.rs` uses RSA OAEP SHA-1 (type 16)
  with AES-128-CFB (type 2), 16-byte IV and length-prefixed records.
  `openkakao-cli/src/loco/crypto.rs` instead selects type 3, AES-128-GCM:
  LE length, 12-byte nonce, ciphertext and 16-byte tag. Its comments claim
  December 2025 corroboration; Sculk has not reproduced that claim. We offer
  only a feature-gated experimental GCM record codec with a caller-provided key.
  A NIST known-answer test verifies the primitive/record handling offline.
  No hardcoded server keys, RSA handshake, endpoint discovery, or trusted
  negotiation are implemented. CFB is deliberately not assumed current.
* `futures-loco-protocol/src/session.rs` separates requests and asynchronous
  packets with IDs. Sculk independently uses bounded admission, a single stream
  owner and oneshot replies. IDs are never reused within a connection and
  command names must match as well. A matching ID is not proof of delivery.
* `KiwiTalk/backend/crates/talk-api-internal/src/auth/{mod.rs,client.rs}` has
  account/login.json, password/token forms, X-VC headers and device identifiers;
  `auth/status.rs` models server failures. `headless-talk/src/event/{mod.rs,channel.rs}`
  and its handlers/updaters separate channel state from network/session events.
  Recent dependency updates in KiwiTalk do not prove current direct login.
* `node-kakao/src/packet/chat` includes WRITE, MSG, SYNCJOIN, LEFT, KICKOUT,
  GETMEM, OPENLINK-related requests and synchronization schemas. Its
  `src/packet/struct/{chat,channel,user,openlink}.ts` separates protocol models.
  These are valuable historical command catalogues, not authorization or a
  verified contemporary event contract. Server kickout can also mean session
  termination; raw command names alone are insufficient to infer a member kick.
* `openkakao-cli/README.en.md` warns recent macOS server-login flows are broken.
  `CHANGELOG.md` v1.3.3 explicitly retracts v1.3.2's purported verification fix:
  request_passcode/register_device endpoints returned 404 and repeated attempts
  reportedly led to account restrictions. Later work restores usability through
  local macOS Accessibility and notification observation, requiring an installed,
  logged-in app. `src/loco/client.rs`, `src/loco/message.rs` and the changelog
  contain direct transport/sync work, but do not establish a working new login.
  `docs/research/android-apk-26.7.1.md` is static research, not live evidence.
  Crucially it reports TLS for Booking/chat/Ticket and V2SL AES-GCM for
  Trailer media, so the GCM record codec must not be assumed to be current
  chat transport. Its Mac/Android cross-check warns against transferring
  platform identity constants or authentication formulas between profiles.
  `src/loco/client.rs` contains a custom certificate verifier; Sculk has not
  reused it and would require ordinary full certificate validation in a TLS
  connector. The existing generic stream API can accept such a connector.
* `KakaoForge/README.md` documents onMessage, join/leave/kick callbacks,
  openChatKick, reactions, media sending and bounded reconnect delay settings.
  These are documentation claims only. Its custom license disallows commercial
  use; no implementation code has been copied, modified, linked, or run.

## KakaoDBpy: verified behavior and limitations

Inspected `kakaobot/bot.py`, `kakaoDB.py`, `Observer.py`, `types.py`, `send.py`,
`KakaoDecrypt.py`, and `response.js` strictly as research. No code was reused.

* `kakaoDB.py::process_message` checks DB message type 0 then decoded JSON
  `feedType`: 2 leave, 4 join, 6 kick, 13 hide, 14 delete. Types 1/26 dispatch
  normal messages. These are **database feed mappings**, not LOCO event IDs.
* Other handlers include profile and member-role changes and invitations, but
  presence of a handler does not prove every change is reliably observed.
  `types.py` has user/member flags and channel/chat models; identifiers are not
  evidence that an OpenChat profile ID is a durable account identity.
* Query helpers open aiosqlite connections repeatedly. `chat_check` fetches
  only 1 initial row or 5 later rows; bursts exceeding that window may be lost.
  It advances an in-memory lastID before all event handling completes.
* `Observer.py` debounces all modifications using a shared 0.5-second timestamp,
  with no durable catch-up cursor. Suppressed changes need not trigger another
  scan. Callbacks execute inline and can delay subsequent handling.
* `send.py` uses an unbounded asyncio.Queue, writes JSON without a newline,
  while `response.js` waits on BufferedReader.readLine. This framing mismatch
  can stall a persistent connection. `send_message(..., type, ...)` then calls
  `type(room)`, where the string parameter shadows the builtin and raises.
* `KakaoDecrypt.py` handles local database cryptography. It is unrelated to
  LOCO transport encryption and is not a legitimate source of network keys.

## Reuse and evidence policy

Sculk uses maintained general-purpose ecosystem crates, not any upstream SDK
source. MIT/Apache reference code would permit reuse with required notices, but
adopting the old networking/crypto stack would add compatibility assumptions.
BSON 2.15 is chosen for its stable raw-document API and bounded lazy validation;
BSON 3 is available and an upgrade should be evaluated separately. Tokio, bytes,
thiserror and RustCrypto AES-GCM supply runtime, buffers, errors and primitives.
Exact resolved versions are in Cargo.lock. See THIRD_PARTY.md for dependency
license review and its limits. No restrictive-license implementation is included.

There is no inspected repository that supplies reproduced evidence in this
workspace of current direct authentication. Current live messaging, moderation,
and real-account recovery therefore remain unverified; independently testable
source-backed messaging subsets can still be implemented offline.
The next evidence needed is a legitimate current login/device-verification flow,
authorized test-account access, and sanitized wire fixtures including negotiated
crypto and synchronization semantics. No authentication/security bypass is proposed.

## Phase 2 follow-up — 2026-10-25 (Asia/Seoul)

Fetched current origin refs for KiwiTalk, openkakao-cli, node-kakao, and all three
Rust LOCO repositories again. Their exact fetched HEADs are unchanged from the
snapshot table above. No third-party implementation was run. Current development
activity still supplies no reproduced direct-login success in this workspace.

### Sequence established by source, not live testing

1. **Account credential acquisition / device approval.** KiwiTalk's
   `backend/crates/talk-api-internal/src/auth/mod.rs` contains password/token
   `account/login.json` forms, device UUID and X-VC computation; subsequent
   request_passcode/register_device helpers are historical private APIs.
   `auth/status.rs` lists verification/challenge/restriction/version failures.
   These HTTP status values must not be interpreted as LOCO status codes.
   openkakao-cli's current README still reports broken recent login paths;
   CHANGELOG v1.3.3 retracts the purported new-device implementation because its
   endpoints returned 404. No automated challenge solving or retry is implemented.
2. **Booking.** GETCONF obtains candidate endpoints (openkakao-cli
   `src/loco/client.rs::booking_request_body`, `booking`, `full_connect`).
   Its researched Mac profile includes userId, MCCMNC, os and retained model.
3. **Check-in.** CHECKIN supplies account/device-profile information to select
   a session endpoint. A successful TLS connection is not account authentication.
4. **Session authorization.** LOGINLIST carries a legitimately obtained oauthToken,
   duuid and platform/version/synchronization fields. KiwiTalk's actual request
   is `talk-loco-client/src/talk/session/login.rs` (not create.rs, which creates
   chatrooms). It says PC prtVer "1.0", whereas the newer openkakao Mac builder
   uses "1"; MCCMNC, dtype, pcst/revision/bg and sync fields differ too. Sculk
   therefore does not invent a universal current request builder.
5. **Acknowledgment / expiry.** node-kakao's `packet/chat/login-list.ts` and
   KiwiTalk's login.rs include an integer userId. KiwiTalk wraps BSON status;
   openkakao's packet.rs permits BSON status or signed header status. Sculk's
   experimental subset parser accepts Int32/Int64, rejects a nonzero status,
   malformed/duplicate relevant fields and mismatched account identity. It
   accepts missing BSON status with a zero header because the node schema does
   not require that field. This is a historical acknowledgment subset, not a
   complete current session or token-expiration contract. Lease/expiry metadata
   must be supplied by a legitimate provider; no opaque sync token is assumed
   to be a time-to-live or refresh token.

### Secure transport and trust boundary

openkakao-cli's static Android/Mac research reports TLS for Booking/chat/Ticket
and V2SL GCM for Trailer media. Its `src/loco/client.rs::KakaoVerifier` discards
the requested TLS server name and tries `kakao.com` and `probe.kakao.com` instead.
Sculk does not adopt that hostname substitution. The new optional secure-session
feature uses ordinary rustls path, hostname, signature and validity verification
for the exact caller-supplied peer name. This could reject a real endpoint that
requires special, independently verified identity handling; that remains an
explicit compatibility blocker, not a reason to disable validation.

Default trust uses Mozilla webpki roots; an explicit RootCertStore allows a
legitimate private CA or local test certificate. A managed proxy CA must be
supplied through that supported trust path when required. Sculk accepts an
already-connected byte stream and never opens a direct Internet socket or
silently bypasses configured proxy/TCP policy. Endpoint discovery, proxy dialing,
actual server identities, authorized credentials, supported device verification
and current account refresh remain outside this milestone.

### What the new authentication tests establish

Tests run real rustls handshakes with an ephemeral local certificate, then send
synthetic LOCO authentication requests and acknowledgments. The request's
`mockToken` field is deliberately a mock schema and is not a production LOGINLIST
builder. The authentication provider is explicitly supplied by the test; there
is no default provider that reports success. These tests establish state,
timeout, cancellation, expiry, rejection, account binding and cleanup behavior.
They do not establish that KakaoTalk accepts any Sculk authentication request.

The runtime reports no configured Kakao credential bindings; exported variable
names contain no KAKAO/SCULK credential. No token values or credential files were
read. Combined with the unsupported current verification flow, this prevents
an authorized live login test. No live Kakao endpoint was contacted.

## Phase 3: Agent Messenger — 2026-10-25

Inspected MIT-licensed [agent-messenger/agent-messenger](https://github.com/agent-messenger/agent-messenger/tree/9d2de96a43fe3e0e172e8e399454c475146a2697)
at `9d2de96a43fe3e0e172e8e399454c475146a2697` (2026-10-20, version 2.39.1).
Re-fetched OpenKakao: HEAD remains `6ca6ea2fd05a6660c4a80c0b61b43b416653d1d5`.
The inspected files are all under `src/platforms/kakaotalk`:

| Source | Observed implementation | Sculk decision |
|---|---|---|
| auth/kakao-login.ts | Android sub-device login; generate/display passcode; phone confirmation; registerDevice polling (max 10); one subsequent login | Research only. No registration or HTTP login calls added. Any future device registration needs explicit user approval and legitimate phone confirmation. |
| auth/oauth-refresh.ts | Android oauth2_token.json, access/refresh tokens, device-id hash, timeout, token rotation | Kept separate; no speculative refresh provider or credential persistence added. |
| protocol/config.ts | Public RSA-2048 key, Booking host/443, fallback Checkin host/995, differing PC/tablet metadata | No implicit device profile or fallback host. Trusted key and provider-consistent profile are caller supplied. |
| protocol/crypto.ts + types.ts | RSA-OAEP/SHA-1, 256-byte ciphertext, type 16/type 3, AES-128-GCM, nonce 12/tag 16, no AAD | Implemented with maintained primitives; no custom crypto. |
| protocol/connection.ts | TLS Booking with rejectUnauthorized; raw RSA/GCM Checkin/chat; accumulates plaintext independently of encrypted frames | Implemented explicit transports and record-to-byte-stream adapter. Reject corrupt records instead of continuing. |
| protocol/session.ts | GETCONF → CHECKIN → LOGINLIST; initial sync fields and periodic PING | Implemented initial sequence only; no automatic heartbeat/reconnect/replay claim. |
| protocol/packet.ts | Same 22-byte little-endian header; signed status, BSON type 0 | Reused existing bounded Sculk codec. |
| protocol/login-response.ts | Checks header/body statuses; labels -950 invalid token; accepts missing body status | Reused Sculk's stricter identity/duplicate/type validation; preserve numeric rejections without refreshing. |
| listener.ts | Attaches to an existing client's push stream and translates MSG/join/leave/read events | Not authentication evidence; event features remain out of this milestone. |

### What is independently verified

Executed the upstream connection, session, login-response and oauth-refresh test
files: **34 passed, 0 failed**, 102 assertions, using Bun 1.4.4 and isolated
reference-only dependencies (bson 6, zod 4). They mock sockets/fetch and do not
establish live login or key-exchange success. The initial run lacked zod; after
installing the declared major version, the selected suites passed unchanged.
No release, account login, passcode, registration or refresh endpoint was invoked.
Bun/Node are research tools outside Sculk, not native runtime requirements.

Captured GETCONF and LOGINLIST requests from actual upstream session/encoder
calls with every socket method intercepted: Sculk's bytes match exactly for the
synthetic profile. CHECKIN fields match except intentional Int64 widening of
userId (upstream uses Number; KiwiTalk's checkin.rs declares i64). Synthetic
replies use the documented shapes; they are not captured server responses.
A separate Node built-in crypto GCM vector decrypts correctly in Sculk, in
addition to the existing NIST vector. Local servers decrypt actual RSA-OAEP
handshakes and exchange real GCM records, including fragmented/concatenated
records and packets split across records. See tests/fixtures/README.md.

### Exact implemented wire contract

Booking is standard TLS, with exact requested-name verification. GETCONF is
explicitly selectable: Agent's `{os, model:""}` or OpenKakao's researched
`{os, model:"", MCCMNC, userId}`. Discovery accepts `hosts[]` or `ticket.lsl[]`
with `wifi.ports[]`; empty, invalid, oversized or ambiguous candidates are rejected.
No hidden ticket-loco/995 fallback is used. The first validated advertised host
and first port are selected, as the sources do. Every dial is delegated to a
caller policy; untrusted discovery cannot directly open arbitrary sockets.

On explicit LOCO-GCM routes, each connection generates a fresh random AES-128
key. Handshake bytes are u32 LE 256, 16, 3, then RSA-2048 OAEP ciphertext using
SHA-1 for both OAEP and MGF1, empty label. Records are u32 LE body size, 12-byte
random nonce, ciphertext and 16-byte tag, no AAD. Size includes nonce/tag but not
the prefix. The nonce belongs to each record, not each LOCO packet. The caller
must trust the RSA key's provenance; only an authenticated encrypted response
confirms possession of the corresponding key. No unauthenticated key is fetched
from a peer and no failed tag is skipped. The documented public key is retained
only as an attributed test fixture, not an automatically trusted production pin.

CHECKIN sends userId (Int64), os, ntype (Int32 0), appVer, MCCMNC, lang,
countryISO, useSub. Reply requires valid host and nonzero 16-bit port. LOGINLIST
sends appVer/prtVer/os/lang/dtype (2), duuid/oauthToken, ntype (0), MCCMNC,
revision (0), empty chatIds/maxIds, lastTokenId (Int64 0), lbk (0), generic BSON
binary rp `[00 00 ff ff 00 00]`, and bg=false. No sync cursor semantics or expiry
is inferred from these fields. Credential expiration is provider supplied;
Options.session_lease is an explicit local lease, not a server TTL.

### Evidence that must not be conflated

Agent's NOTICE explicitly derives its protocol knowledge from OpenKakao and
other references. Their agreement is not independent evidence of current server
compatibility. OpenKakao's IMPROVEMENT_PLAN documents historical GCM success and
-950 for a REST-only cached token; its current client tries TLS before GCM and
falls back across ports. Its later signed Android/Mac static research reports
TLS for chat/Ticket and GCM for Trailer media. Agent directly uses GCM for both
Checkin/chat. Sculk supports explicit choices but never probes/downgrades routes
automatically. These are profile/version differences requiring authorized live
verification, not grounds for treating TLS and LOCO GCM as interchangeable.

Agent's Android `passcodeLogin/generate` and `passcodeLogin/registerDevice` paths
differ from the older Mac endpoints that OpenKakao reports as returning 404.
Mac failure does not prove Android failure or success. Agent's login flow can
register automatically after -100 and its debug callback prints raw login JSON;
Sculk adopts neither behavior. Its packet/record reader lacks Sculk's frame
limits and skips failed GCM tags; those behaviors are deliberately not reused.
Account/profile correctness, current server/key identity, registration/refresh
success, and a genuinely LOCO-capable token remain unverified here. No test
account was explicitly authorized, so no live authentication was attempted.

Sculk's Rust implementation remains independent. Public-key/fixture provenance
and MIT notices are recorded in THIRD_PARTY_NOTICES.md. RSA private-key operations
exist only in local test-server code; the library uses RSA public-key encryption
only. No restrictive-license implementation has been integrated.

## Phase 4 concrete provider review — 2026-10-25

Fetched Agent Messenger again: unchanged
`9d2de96a43fe3e0e172e8e399454c475146a2697`. Re-inspected auth/kakao-login.ts,
auth/oauth-refresh.ts, credential-manager.ts and the existing native sequence.
Provider/session transport remain separated. Source-backed form/JSON/header
contracts are now implemented as real asynchronous HTTPS requests to the fixed
Android origin, not a mock-only callback. No request to that origin was run here.

New source-generated `agent-android-auth-requests.json` captures actual login,
challenge, registration and refresh request construction with intercepted fetch
and synthetic inputs. Rust tests compare form bytes, JSON bodies, Android headers
and X-VC against that fixture. The independent native end-to-end test issues an
HTTP token over a local server, then performs TLS GETCONF and RSA/GCM CHECKIN /
LOGINLIST with account `9007199254740993` (2^53+1). JSON parsing and BSON Int64
preserve it exactly; no JavaScript Number conversion is used. Tests also preserve
assigned-endpoint changes and partial packets across encrypted record boundaries.

The provider differs deliberately from Agent's automatic flow: -100 stops at a
user approval gate; no challenge or registration is sent until explicit approval.
Phone confirmation is server-mediated, registration polls are bounded, and only
one post-registration login is permitted. Rejections never lead to automatic
retry, forced login, profile switching or UUID rotation. Unlike the reference's
raw-response debug path, Sculk retains no raw response in errors/log output.

Refresh uses the same account/device and validates any returned userId. The source
refresh schema does not require userId, so a rotated token without it inherits
previous binding metadata and still requires an exact LOGINLIST account check.
The source login interface does not establish an expiration contract; absent
expires_in leads to a clearly labeled local credential-age cap. Device approval
and refresh routes are source-supported implementations, not independently proven
current service behavior. The CLI's matching Android/LOCO profile prevents accidental
PC/tablet mixing but cannot prove the configured version is accepted today.

Native request validation remains limited to the studied profile. The owner must
supply an explicitly authorized account, its known numeric user ID, correct device
profile, trusted key provenance, approved routes and legitimate device confirmation.
No configured Kakao secret bindings or KAKAO/SCULK credential variables were found
by name/presence checks. No values were inspected and no live test was attempted.
See authentication.md for the exact safe owner validation procedure.

## Phase 5 readiness audit — 2026-10-26

Fetched the current public refs again: Agent Messenger remains
`9d2de96a43fe3e0e172e8e399454c475146a2697`, OpenKakao remains
`6ca6ea2fd05a6660c4a80c0b61b43b416653d1d5`, KiwiTalk remains
`4e565ca6bfff72f6085f1b0a4a767558c78ff450`, and node-kakao remains
`8512e4699e2cd4c26516b80d38b57b2f85a6cc8a`. None supplies newly reproduced live
acceptance in this environment. Existing endpoint/transport/profile differences
and current RSA-pin provenance remain open compatibility requirements.

Actual-source audit confirmed fixed HTTPS origin, ordinary TLS verification,
no redirects/retries/forced login, explicit registration approval, bounded polls,
Int64 account binding and zeroizing credential types. Focused fixes:

- The CLI's registration spawn_blocking stdin prompt could outlive cancellation
  and prevent runtime shutdown. It now uses bounded nonblocking terminal readiness
  with cancellation, tested with partial input and owned-descriptor cleanup.
- Password/email and superseded refresh credentials remained in scope during
  protocol setup. The CLI now drops them promptly, then drops provider/credentials
  after creating the short-lived protocol token. Serialized/TLS/kernel copies
  remain outside comprehensive memory-erasure guarantees.
- Approval codes went to stdout. They now go only to the controlling terminal
  for deliberate human confirmation; sanitized reports and stdout/stderr contain
  no code. Non-ASCII/control-bearing challenge codes are rejected.
- X-VC was not marked sensitive in HeaderMap debug output. It is now marked
  sensitive. Errors still retain no raw HTTP/LOCO payload or credential-bearing cause.
- HTTP completion now checks the absolute deadline after body parsing, and an
  expired pending registration response stops polling immediately.

Read-only schemas were cross-checked against Agent's protocol/session.ts,
node-kakao packet/chat/{lchat-list,chat-info,get-mem}.ts and packet/struct/{channel,user}.ts,
and KiwiTalk talk/session/load_channel_list.rs and session/channel/{mod,info}.rs.
Implemented requests are LCHATLIST with empty known chat/max arrays and explicit
lastTokenId/lastChatId, CHATINFO with chatId, and GETMEM with chatId. Responses
use chatDatas[].c/t/a, chatInfo.chatId/type/activeMembersCount/chatMetas, and
members[].userId/nickName plus token. Unknown kinds remain raw; member userId
is not conflated with an account or OpenChat profile identity.

The fixture tool calls actual Agent read-only methods with all sockets replaced;
request bytes are compared exactly in native tests. Responses are independently
labeled synthetic schema examples, never live captures. One page/request is
bounded; no automatic history fetch, read marking or fabricated title/member name
exists. Missing required fields, duplicate keys, account/room mismatch, malformed
integer IDs and non-progressing pagination fail explicitly. No new request fields
were invented to make an unverified endpoint accept a connection.

The crypto audit also found that optional RustCrypto zeroize features had not been
selected. They are now explicitly enabled for AES expanded-key cleanup, GHASH/
POLYVAL state and AES-GCM temporary key material. These are existing transitive
primitives, not replacement cryptography. Source/feature resolution and unchanged
known-answer/record tests verify integration; comprehensive allocator/TLS/kernel
memory erasure is still not claimed.

## Phase 6 source refresh — 2026-10-28 (Asia/Seoul)

Fetched public HEADs for node-kakao (`8512e4699e2c`), Agent Messenger
(`9d2de96a43fe`), openkakao-cli (`6ca6ea2fd05a`) and KiwiTalk (`4e565ca6bfff`).
All match prior pinned snapshots. No newer compatible Android auth flow is
established by these sources. The existing provider still follows Agent's fixed
Android HTTPS forms/X-VC and explicit device approval, without forced login or
rejected-authentication retry. OpenKakao's platform/transport contradictions and
custom verifier remain reasons to keep transport/key/profile selection explicit.
No third-party login or networking implementation was executed.

| Evidence at exact snapshot | Implemented subset / limitation |
|---|---|
| [node-kakao MsgRes](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/packet/chat/msg.ts), [ChatlogStruct](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/packet/struct/chat.ts) | MSG chatId; nested logId/authorId/type/sendAt/message/attachment; optional raw metadata. sendAt is seconds. Outer logId/nested chatId are validated echoes when present. |
| [Agent listener](https://github.com/agent-messenger/agent-messenger/blob/9d2de96a43fe3e0e172e8e399454c475146a2697/src/platforms/kakaotalk/listener.ts) MSG handler | Corroborates outer room plus nested author/log/type/time/content. Does not require outer logId or nested chatId. Sculk preserves exact integers instead of coercing authors to JS Number. |
| [node-kakao chat types](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/chat/chat-type.ts) | Feed=0, Text=1, Photo=2, Video=3, Audio=5, File=18, Reply=26. Other values/flags remain Unknown(raw). Feed payloads do not become inferred LOCO event identifiers. |
| [Agent sendMessage](https://github.com/agent-messenger/agent-messenger/blob/9d2de96a43fe3e0e172e8e399454c475146a2697/src/platforms/kakaotalk/protocol/session.ts) | WRITE {chatId:Int64,msg:String,type:1,noSeen:false}, with no client msgId/extra. Actual method executed with a replacement no-network connection, generating agent-write-request.bin. |
| [node-kakao sendChat](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/talk/channel/talk-channel-session.ts), [WriteRes](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/packet/chat/write.ts) | Older request differs: adds client msgId and defaults noSeen=true. No universal merged request is invented. Flat reply has chatId/logId/sendAt/msgId/prevId and optional chatLog; only room/log/time are required by Sculk's acknowledgment subset. Optional chatLog is preserved at the raw API, not used to fabricate delivery proof. |

`node-msg-synthetic.bin` has schema-shaped fictional values, encoded using Agent's
real packet encoder; it is not generated by a server or a source message producer.
The text fixture executes the real source method. Generator dependencies are
isolated dev tools, not Sculk dependencies. Networking imports are replaced before
execution; no authentication, socket or heartbeat is allowed. MIT source notices
remain in THIRD_PARTY_NOTICES.md. Sculk's Rust implementation is independent.

Local policies (positive IDs, maximum 128 top/log fields, 16 KiB nonempty outbound
text with no NUL) are resource/input bounds, not claims about server limits.
Missing optional message content/attachment stays None; unknown attachment JSON
is preserved as text. Duplicated top/log keys, floating-point IDs, invalid required
fields and conflicting echoes fail typed parsing but retain the raw packet.

## Phase 7 synchronization and recovery — 2026-10-28 (Asia/Seoul)

Refetched Agent Messenger (`9d2de96a43fe3e0e172e8e399454c475146a2697`,
2026-10-20) and node-kakao (`8512e4699e2cd4c26516b80d38b57b2f85a6cc8a`,
2023-11-01). HEADs are unchanged. Repository origin main matched Phase 6 base
`6476179206f7e458cd128ad6633a80768e54e3b5`. Research is public-source evidence,
not independent current account acceptance or a live capture.

| Exact source | Finding and Sculk decision |
|---|---|
| [Agent session.syncMessages/getChatLogs](https://github.com/agent-messenger/agent-messenger/blob/9d2de96a43fe3e0e172e8e399454c475146a2697/src/platforms/kakaotalk/protocol/session.ts) | SYNCMSG `{chatId,cur,cnt,max}` and MCHATLOGS `{chatIds,sinces}`. Implement only finite SYNCMSG pages with an observed upper watermark; no automatic MCHATLOGS fallback. |
| [node-kakao syncChatList/getChatListFrom](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/talk/channel/talk-channel-session.ts) | Forward SYNCMSG sets cur to returned last log ID, uses explicit endLogId, and stops on isOK. Its cnt=0 is commented unknown; Sculk instead uses Agent's positive count profile and rejects pages exceeding the local bound. |
| [node-kakao SyncMsgRes](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/packet/chat/sync-msg.ts) | isOK=false means more chats; chatLogs is optional; minLogId is minimum requestable log. lastTokenId/jsi/li remain opaque metadata, not timestamps or fabricated cursors. |
| [node-kakao MChatlogsRes](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/packet/chat/mchatlogs.ts) | Older response exposes only chatLogs, no authoritative completion flag or request count. Researched but not exposed by Sculk in this phase. |
| [Agent getMessagePage/buildMessagePage](https://github.com/agent-messenger/agent-messenger/blob/9d2de96a43fe3e0e172e8e399454c475146a2697/src/platforms/kakaotalk/client.ts) | Uses read-only CHATINFO.lastLogId, filters IDs after cur, sorts/deduplicates, and keeps the oldest forward page. It explicitly avoids CHATONROOM because entering can advance unread state. Sculk also avoids it, requires room binding, rejects conflicting duplicate records and never discards an oversized remainder silently. |
| [node-kakao ChatlogStruct](https://github.com/storycraft/node-kakao/blob/8512e4699e2cd4c26516b80d38b57b2f85a6cc8a/src/packet/struct/chat.ts) | logId and prevId are distinct; prevId=0 is first chat. There is no evidence that log IDs are contiguous. Sculk uses source predecessor evidence and server history, never ID+1/timestamp uniqueness assumptions. |
| Agent ensureSession/executeWithReconnect in the same client | Concurrent initialization guard prevents duplicate LOGINLIST/session self-eviction. Its operation-level retry is not copied: Sculk reconnects only an owned disconnected session and never replays arbitrary operations/WRITE. |
| Agent session.startPing | PING uses an empty document on a timer. Sculk adds a single bounded heartbeat operation; scheduling stays explicit and adds no background task. |

The fixture generator now executes the actual pinned syncMessages method with a
no-network connection replacement, producing agent-syncmsg-request.bin. The
node-syncmsg-synthetic.bin reply uses fictional ChatlogStructs and the original
packet encoder. No third-party authentication, socket or live test was executed.

Android provider review: fixed `https://katalk.kakao.com/android/account/` login,
passcodeLogin/generate, passcodeLogin/registerDevice and oauth2_token.json routes,
Android A/User-Agent/X-VC/device fields, explicit phone approval, forced=false,
no redirects/retries and account/device-bound tokens still follow the pinned Agent
profile. Its public example uses app 25.9.2 / Android 13 / API 33; those constants
are not proof of current profile acceptance. Sculk requires caller-selected
consistent metadata and does not rotate identity or force an occupied device slot.
The owner CLI still verifies a supplied RSA fingerprint before authentication.

Unverified assumptions remain endpoint availability, accepted app/sub-device
profile, legitimate approval behavior, independently trusted current RSA identity,
server TLS identities, GCM-versus-TLS route selection for Checkin/chat, token lease
semantics, LOGINLIST/sync field acceptance and assigned endpoint allowlists. The
existing GETCONF → CHECKIN → LOGINLIST path retains explicit protection and exact
allowlist checks; it never guesses fallback hosts or substitutes certificate names.
No credentials/account approval/key configuration or permitted Kakao route is
available here. All source-shaped success remains offline evidence only.

## Phase 8 reference refresh — 2026-10-29 (Asia/Seoul)

Agent Messenger now resolves to
`e58739d80305d5e8ddf72e9aad085c2069f4fd3c` (2.40.0, October 29). Comparing its
src/platforms/kakaotalk tree against the previously pinned
`9d2de96a43fe3e0e172e8e399454c475146a2697` produced no changes: Android authentication,
refresh, connection, cryptography, GETCONF/CHECKIN/LOGINLIST, MSG and synchronization
code are unchanged. Other platform/shared keyring code and skill/version metadata
changed; they are not a new Sculk wire-compatibility result. node-kakao remains at
`8512e4699e2cd4c26516b80d38b57b2f85a6cc8a`. No reference login or network code was run.

Existing synthetic fixtures therefore retain their original pinned provenance.
Nothing in this refresh establishes availability of the Android endpoints, accepted
current device profile/approval, trusted RSA identity, GCM/TLS route, token lease or
assigned endpoint schemas. Those remain owner-authorized live test questions.

Also refetched openkakao-cli and KiwiTalk: their HEADs remain respectively
`6ca6ea2fd05a6660c4a80c0b61b43b416653d1d5` and
`4e565ca6bfff72f6085f1b0a4a767558c78ff450`; no newer LOCO/authentication evidence
was available there. Their previously documented platform/transport caveats remain.
