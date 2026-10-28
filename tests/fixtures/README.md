# Fixture provenance

These are offline test artifacts, **not live KakaoTalk captures**. No fixture
establishes current account compatibility.

- `ping.bin`: hand-authored historical LOCO framing golden vector, synthetic ID.
- `agent-getconf.bin`, `agent-checkin.bin`, `agent-loginlist.bin`: produced by
  Agent Messenger's actual session/packet code at
  `9d2de96a43fe3e0e172e8e399454c475146a2697`, with all connection methods intercepted.
  They use synthetic token/device/account values and the reference's public Mac
  profile constants. No account endpoint is contacted.
- `agent-*-response.bin`: source encoder output for **synthetic, schema-shaped
  server replies** chosen by the harness. The source does not prove those replies
  are current live server output. Hosts use `.example`; IDs are synthetic.
- `gcm-node-ping.bin`: independent Node built-in crypto vector: AES-128-GCM,
  key `[7;16]`, nonce `[9;12]`, no AAD, plaintext `ping.bin`. LE body length
  includes nonce and tag. This checks crypto/wire interoperability, not live keys.
- `documented-server-public.der`: public PKCS#1 RSA-2048 key from Agent's
  protocol/config.ts, also in OpenKakao crypto.rs. SHA-256:
  `c64be379ff4333c91346e7d8685ab11c0ab15cf46183f5316853eb9ae4f1d3ed`.
  Public, non-secret, unverified current provenance; **test fixture only**.
- `synthetic-rsa-private.pem`: generated test-only RSA-2048 key for the local mock
  server. It is public test data and must never be used for a real server/session.

The tests compare GETCONF/LOGINLIST bytes exactly against source-generated
requests. CHECKIN intentionally widens the reference's JS-number account ID
(Int32 for synthetic 42) to Int64, as Rust reference schemas declare. Other
fields are compared exactly. This preserves full IDs but requires live validation.

Regenerate requests/replies with Bun and the pinned reference's bson 6/zod 4
installed **outside Sculk**:

```
bun tests/fixtures/tools/generate-agent.ts /path/to/pinned/agent-messenger
node tests/fixtures/tools/generate-gcm.mjs /path/to/pinned/agent-messenger
```

The first generator replaces every socket operation before calling LocoSession.
Use the exact pinned checkout and inspect local changes before regeneration.
Neither Node nor Bun is needed to build Sculk, run Rust tests, or deploy it.
The private synthetic fixture was generated with OpenSSL genpkey, algorithm RSA,
rsa_keygen_bits:2048; regenerating that fixture does not change wire expectations.
See THIRD_PARTY_NOTICES.md for attribution and MIT notices.

Phase 4 adds `agent-android-auth-requests.json`: actual source-generated request
forms/headers/JSON for login, challenge, registration and refresh, with intercepted
fetch and synthetic email/password/device/token values. They are not live captures.
Regenerate with `bun tests/fixtures/tools/generate-auth.ts /path/to/pinned/reference`.
Tests compare body contracts, Android headers and X-VC to this fixture. Response
JSON in provider tests is synthetic and is deliberately encoded directly with
an exact account number beyond 2^53; it does not pass through JavaScript Number.

Phase 5 adds `agent-{lchatlist,chatinfo,getmem}-{request,response}.bin`. Requests
come from the actual pinned Agent methods; responses are source-encoder output
for deliberately synthetic schema examples (not live captures). IDs use exact
BSON Long 9007199254740993. Regenerate via
`bun tests/fixtures/tools/generate-read-only.ts /path/to/pinned/reference`.
Native tests compare request bytes and validate returned shapes/identity/cursors.

## Phase 6 messaging contracts

* `agent-write-request.bin`: actual pinned Agent Messenger `sendMessage` method,
  synthetic Int64 room `9007199254740993` and `Synthetic text`. Replaced only the
  connection boundary; packet encoder is the original reference. No socket/login.
* `node-msg-synthetic.bin`: synthetic MsgRes/ChatlogStruct fields from pinned
  node-kakao, encoded by Agent's packet encoder. This tests source-shaped BSON,
  **not** live event correctness. Large IDs, timestamps and content are fictional.
* Flat WRITE acknowledgments/rejections are constructed by Rust test peers from
  node-kakao's WriteRes interface. No independent live reply fixture exists.

Regenerate in a disposable dev directory with `esbuild`, `bson@6` and `zod@4`
installed, set NODE_PATH to its node_modules, then run:

```sh
node tests/fixtures/tools/generate-messaging.cjs /PATH/TO/PINNED/agent-messenger
```

The generator checks the exact pinned commit and replaces the reference connection
module with a constructor that throws before any networking could occur. Only
source methods and BSON serialization execute. No timer, real credential, room,
login, registration or third-party network transport is used.

## Phase 7 synchronization contracts

`agent-syncmsg-request.bin` is produced by the actual pinned Agent Messenger
syncMessages method with room/cursor `9007199254740993`, count 2 and upper watermark
`9007199254741093`, using the original packet encoder and replacement no-network
connection. `node-syncmsg-synthetic.bin` is a fictional isOK=true reply with two
ChatlogStruct records (including an unknown message type); its schemas are from
node-kakao and its framing comes from Agent's encoder. It is not a server capture.

The existing generate-messaging.cjs now regenerates these alongside Phase 6
fixtures. Exact source snapshots, license notices and no-network guards remain
unchanged. Rust tests also construct corrupt, duplicate, reordered and truncated
synthetic replies. No fixture establishes current authentication, complete history
availability or recovery of real account messages.
