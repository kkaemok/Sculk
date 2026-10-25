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
