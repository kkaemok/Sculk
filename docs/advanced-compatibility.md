# Protocol profiles and evidence

Sculk implements two explicit Android sub-device profiles. Both have offline
coverage. Owner validation against KakaoTalk remains outstanding for both.

| `protocol_profile` | Authentication and wire contract | Verification |
| --- | --- | --- |
| `android-legacy` (default) | Existing Android HTTPS provider, TLS Booking, pinned RSA/AES-GCM CHECKIN and chat, historical LOGINLIST and basic WRITE. Supported reply and mention extras use the same request envelope. | Existing framing, login, message, and recovery fixtures. |
| `android-tablet` | Android HTTPS provider for the documented app 25.9.2, Android 13 and API 33 contract, TLS Booking, pinned RSA/AES-GCM CHECKIN, verified TLS chat on port 10009, LOGINLIST with `isSw=false`, tablet WRITE headers. | Local trusted peer verifies transport selection, SNI, LOGINLIST, WRITE widths, reconnect generations, ambiguous closure and wrong-certificate rejection. |
| PC, Mac, main phone, other tablet versions and profile combinations | No matching Sculk account provider or validated contract. | Explicit unsupported-profile error. |

The October 3, 2026 loco-wrapper revision describes Android 26.8.2 using TLS
for CHECKIN as well as chat. Its TLS client accepts arbitrary certificates,
and its LOGINLIST and Android HTTP profile differ from Agent Messenger's.
It supplies evidence for investigating a separate profile once exact service
identities and the owner-approved provider contract are established.

The tablet profile connects to the CHECKIN-assigned host on TCP port 10009. The
assigned host and port must appear together in `allowed_endpoints`. For this
role the profile fixes the TLS service identity and SNI to `kakao.com`, citing
the upstream TLS contract below. Rustls checks trust, validity and that name.
Booking still verifies its exact host. Both roles still require the independently
verified CHECKIN RSA key and its SHA-256 fingerprint. A repository fixture or a
hash calculated from its contents supplies neither server-key provenance nor
route authorization. There is one profile per connection owner and no
transport, version or device-role fallback.

## Owner workflow

Existing JSON can omit `protocol_profile` and keeps the historical behavior. To
test the tablet path, specify `"protocol_profile": "android-tablet"` in the private
JSON, set the validated Android fields for the authorized device, and include
the independently approved tablet chat route. Keep the same registered device
identity. After owner testing, continuous runtime requires both
`"profile_validated": true` and `"validated_protocol_profile": "android-tablet"`.
The attestation records the owner's explicit validation for this profile.

```sh
cargo run -p sculk-auth-client --features live --locked -- --check-config /ABSOLUTE/private/auth.json
cargo run -p sculk-auth-client --features live --locked -- --preflight /ABSOLUTE/private/auth.json
cargo run -p sculk-auth-client --features live --locked -- --live /ABSOLUTE/private/auth.json --receive-one 30
cargo run -p sculk-auth-client --features live --locked -- --live /ABSOLUTE/private/auth.json --send-one 5
```

The first command is local. The second checks approved routes without account
requests. Run the last two from a private terminal only with the relevant
account and room authorization. `--receive-one` waits for the selected room;
`--send-one` uses `test_message_file` and separately confirms the destination and
text. Stop on rejection or uncertain delivery. See the complete operator
procedure for credential provisioning, stage reports and the command probe.
[Owner authentication](authentication.md).

The provider allows one explicitly requested refresh for a verified device and
account. Recovery reuses the in-memory credential only while its known lifetime
permits and stops at expiry. A new owner-authorized run can establish a new
lease. Disconnect closes the transport and local credentials leave scope.
The inspected upstream logout removes local stored credentials. There is no
verified server-side revocation or long-running renewal contract for either
profile. [Recovery](recovery.md).

## Operation matrix

Source names below refer to the pinned revisions in the next section. Packet,
schema and fixture evidence is distinct from real-service verification. Every
implemented row remains owner-unverified against KakaoTalk.

| Operation | Source and relevant file | Request, response and event evidence | Applicability, implementation and offline verification |
| --- | --- | --- | --- |
| Android login and approval | Agent `auth/kakao-login.ts`, `auth/kakao-login.test.ts`; Kiwi `talk-api-internal/src/auth/client.rs` | `login.json`, passcode generation/registration, typed statuses and account identity. These are HTTPS responses. | Both Android profiles use the existing verified HTTPS provider. Loopback tests cover terminal rejection, bounded human confirmation and identity. |
| Explicit refresh and local logout | Agent `auth/oauth-refresh.ts`, `auth/oauth-refresh.test.ts`, `commands/auth.ts`, `credential-manager.ts` | Account/device-bound refresh and token rotation. Upstream logout removes a local account record. No logout event or remote revocation acknowledgment established. | HTTPS refresh fixture-tested; local session shutdown and credential lifetime tested. Remote revocation and unattended rotation unavailable. |
| GETCONF | Agent `protocol/session.ts`, `protocol/config.ts`; OpenKakao `protocol/connection.mdx` | Agent sends `os, model`; response carries Checkin endpoint attributes. No associated push. OpenKakao documents a distinct historical request. | Exact-name TLS Booking for both. Binary response fixture and mixed local handshake. Discovery fails closed when required route fields are absent. |
| CHECKIN | Agent `protocol/session.ts`, `protocol/connection.ts`; OpenKakao `protocol/encryption.mdx` and `protocol/connection.mdx` | Sub-device and account fields and assigned `host, port`, validated before opening chat. No CHECKIN push. | Pinned GCM for both. Local synthetic key and binary assignment fixture. Outer route authorization remains required. |
| LOGINLIST | Agent `protocol/session.ts`, `protocol/config.ts`, `protocol/session.test.ts`; node `src/packet/chat/login-list.ts` | Device and OAuth credentials, cold-start sync fields, tablet `isSw=false`; success requires exact response `userId`. No implied cursor continuity. | Legacy GCM or tablet TLS. Local three-stage flow, certificate mismatch, and account mismatch/rejection fixtures. |
| WRITE text, mention, quote | Agent `protocol/session.ts`, `protocol/message-id.ts`, `protocol/send-response.ts`, `mention.ts`, `client.ts`; node `src/packet/chat/write.ts`, `src/chat/attachment/{mention,reply}.ts` | Legacy body or tablet `scope, msgId, silence, extra`. Mentions and type-26 quote live in exact JSON. Sculk requires bound `chatId, logId, sendAt`; an echoed tablet `msgId` must match. A successful server response is an acknowledgment. MSG may also arrive separately. | Both chat transports, offline packet and acknowledgment tests. Some upstream response variants omit fields Sculk requires; these return unknown delivery. No automatic resend. |
| MSG and thread fields | Agent `listener.ts`, `chat-log-fields.ts`, `types.ts`, `mention.ts`; node `src/packet/chat/msg.ts` | Push with `chatId, chatLog` and exact author/log IDs. Optional thread scope, exact thread and previous IDs and unmodified attachment/supplement. | Both chat transports, existing local MSG fixture plus typed metadata tests. Thread routing and arbitrary thread WRITE scope unsupported. |
| Media and sticker metadata | Agent `types.ts`, `listener.ts`, `attachment-router.ts`; node `src/chat/chat-type.ts`, `src/chat/attachment/{media,emoticon}.ts` | MSG type and JSON media fields, ordered multi-photo arrays, optional item path. Upstream routing treats multi-photo and individual media as different submissions. | Content decoded locally when requested. Synthetic tests cover raw preservation and array alignment. Upload, downloads, outbound media and sticker delivery have no Sculk contract. |
| Rich links and reactions | node `src/chat/chat-type.ts`; OpenKakao `overview/limitations.mdx`; KakaoForge public API documentation | Historical type-9 KakaoLink classification. Public action APIs and desktop-specific reaction limitations do not establish an Android request/response/push contract. | Type-9 payload and unknown events remain accessible as raw metadata. Reaction mutation and decoding, rich-card field mapping and rich-card sending unsupported. |
| LCHATLIST | Agent `client.ts`, `protocol/session.ts`; node `src/packet/chat/lchat-list.ts`; Kiwi `talk-loco-client/src/talk/session/load_channel_list.rs` | Exact pair of pagination cursors and server `eof`; no list push assumed. | Chat transport for selected profile. Local bounded pagination verifies order, duplicate suppression, exact cursors and incomplete result at budget. |
| CHATINFO / GETMEM / INFOLINK | Agent `client.ts`, `protocol/session.ts`; node `src/packet/chat/{chat-info,get-mem,info-link}.ts` | Room ID and scoped member metadata; optional `displayMembers`, advertised count, GETMEM member IDs and repeated reads. INFOLINK replies by exact link ID. | Chat transport for selected profile. Local tests for completed and changed member observations, room and link identity, count and malformed fields. No CHATONROOM entry, enrollment or moderation. |
| PING, SYNCMSG and recovery | Agent `protocol/session.ts`, `client.ts`, `sync-state-store.ts` and tests; node `src/packet/chat/sync-msg.ts`; Kiwi headless updater | Bounded PING, room history cursor and last log, MSG reconciliation. Agent LOGINLIST cache is device-scoped; its cached token is not credential expiry. | Existing Sculk managed chat lifecycle, checkpoint and bounded history fixtures. No adoption of upstream credential persistence or cached LOGINLIST state. |
| OpenChat entry, roles and moderation | node `src/openlink/`, `src/packet/chat/{join-link,chat-on-room,kl-sync}.ts`; Agent `client.ts` | Historical join/entry and administrative APIs can mutate room or unread state. No owner-approved current mutation or event contract for Sculk. | Unavailable. Read-only snapshots do not use entry fallback. |

## Pinned reference inventory

| Reference | Exact revision and license | Inspected paths and scope |
| --- | --- | --- |
| [Agent Messenger](https://github.com/agent-messenger/agent-messenger) | `c2e5368a0b3396aa4e29774d9c01f927f7aadc92`, MIT | `src/platforms/kakaotalk/` paths in the matrix, plus `protocol/NOTICE.md`, `attachment-router.test.ts`, `credential-manager.test.ts`, `protocol/message-id.test.ts`, `protocol/connection.test.ts` and `sync-state-store.test.ts`. Direct LOCO; upstream platform-specific ClientHello tuning and delivery retry opt-in are outside Sculk's contract. |
| [KiwiTalk](https://github.com/KiwiTalk/KiwiTalk) | `4e565ca6bfff72f6085f1b0a4a767558c78ff450`, Apache-2.0 | `ARCHITECTURE.md`, `backend/bin/client/src/event.rs`, `backend/crates/{headless-talk,talk-api-internal,talk-loco-client}`. Headless event and database layers inform separation of concerns. Its `headless-talk/src/updater/chat.rs` accumulates a whole range; Sculk keeps bounded recovery pages. |
| [node-kakao](https://github.com/storycraft/node-kakao) | `8512e4699e2cd4c26516b80d38b57b2f85a6cc8a`, MIT | `src/chat/{chat-builder.ts,content,attachment,store.ts}`, `src/{channel,openlink,packet/chat}`, `src/network/request-session.ts`. Direct historical protocol and composable API; current acceptance unestablished. |
| [KakaoForge](https://github.com/play2fly/KakaoForge) and [upstream](https://github.com/aodjo/KakaoForge) | `4b774ea40b1347280fadb685415436584093118b` and `f1f44d3cef15d387925f508f3f96a909f173e291`, KakaoForge Non-Commercial / No Abuse License | `README.md`, `docs/README-en.md`, `LICENSE` only. Public API groups events, sends, attachments, reactions and OpenChat permissions. It supplies no independently verified wire contract for this work. |
| [OpenKakao CLI](https://github.com/JungHoonGhae/openkakao-cli) | `2e206f4412fe6d16f393964c9ccce8456b6f4c71`, MIT | `README.md`, `website/content/docs/{getting-started/transport-boundary,overview/limitations,protocol/connection,protocol/commands}.mdx`. Distinguishes desktop REST/cache, app-dependent workflows and direct LOCO commands. Platform and role constraints limit use as Android evidence. |
| [loco-protocol-rs](https://github.com/storycraft/loco-protocol-rs) | `849ba913d9563bb856c62a85dd4299f07fdea18e`, MIT | `readme.md`, `specification.md`. Historical frame and secure record definitions. |
| [futures-loco-protocol](https://github.com/storycraft/futures-loco-protocol) | `bcb8b16f51632f8d1d4786421adeb2a8ed3c848a`, MIT | `README.md`, session/secure-stream API. Async LOCO transport layer. |
| [talk-loco-client-rs](https://github.com/storycraft/talk-loco-client-rs) | `99b4f6f52e7d78814cef7541684aef088c14ec42`, MIT | `readme.md`, `src/structs/connection.rs`. Historical typed connection layer. |
| [loco-protocol-kotlin](https://github.com/yushosei/loco-protocol-kotlin) | `7f7ce6b580f47d4b28f616ca351f347d1a834e44`, MIT | `README.md`, `src/main/kotlin/com/yushosei/loco/LocoPacket.kt`. Direct framing and diagnostics. No Kotlin runtime dependency. |
| [kakao-agent](https://github.com/HSUNEH/kakao-agent) | `d2e24ad547cf67db5c4c33fad243fdb5105b53e1`, MIT | `README.md`. macOS Accessibility bridge with a confirmed send path. No LOCO evidence. |
| [Iris](https://github.com/dolidolih/Iris) | `a0b29d8a8301337daf903483ba008fe18f35e096`, GPL-3.0 and separately licensed MIT material | `README.md`, Android message-type listings. Android app/database bridge. Message enums alone do not establish LOCO events. |
| [kakaocli](https://github.com/silver-flight-group/kakaocli) | `8b6ffcfdaebc592a735dc1a8bd5e50037e626406`, MIT | `README.md`, `Sources/KakaoCLI/Commands/MessagesCommand.swift`. macOS local database and UI bridge. |
| [pykakao](https://github.com/hallazzang/pykakao) | `69b0cbd15cc8019b5c23fdd14470aea1181a5773`, MIT | `README.md`, old Python LOCO/HTTP wrapper. Historical only. |
| [kakao.py](https://github.com/jhleekr/kakao.py) | `0f2ba8b996ba5379d67a2fc4760ea575c90bf540`, MIT | `README.md`, `kakao/packet.py`. Historical direct LOCO/HTTP protocol and packet codec. |
| [loco-wrapper](https://github.com/NetRiceCake/loco-wrapper) | `1dec3b79e59f2776ebd1f03e60744162ff195b24`, MIT | `README.md`, `src/main/java/com/github/netricecake/kakao/packet/outbound/login/LoginListOut.java`. Direct Android tablet reference with a different version/transport contract and permissive TLS. |

Additional earlier pins and fixture provenance are in the
[research history](research.md).
