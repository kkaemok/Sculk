# Security considerations

This unofficial experimental SDK may become incompatible with KakaoTalk and
using unofficial clients may lead to account restrictions. No bypass of device
verification, account controls, or service enforcement is provided.

Treat all peer input as hostile. Close streams after framing/decryption errors.
Do not log payloads or session secrets: Packet Debug reports only headers and
length. The record cipher has no Debug/key accessor. Application-supplied BSON
serialization occurs before the packet size check; cap caller input separately.
Queued work and events are bounded but operators must size them against packet
limits and memory budgets. Raw events are not a lossless control plane.

There is no built-in current KakaoTalk account/device-authentication provider yet. Encryption with a supplied key
alone is not peer authentication. Do not invent a server key, disable TLS checks,
use unverified sessions, or automatically repeat non-idempotent timed-out requests.
GCM record replay detection belongs to the future session layer. Session secrets
are not persisted; future storage needs redaction, zeroization, atomic writes,
owner-only file permissions and expiration/refresh validation.

Report potential vulnerabilities privately through repository-owner contact or
GitHub private vulnerability reporting when available. Do not post secrets in
public issues. No public management endpoint is included.

## Optional session feature

`secure-session` adds ordinary rustls certificate/path/name/time/signature checks.
A custom trust store replaces the default store and must contain roots from a
trusted source. No alternate-hostname verifier or plaintext fallback is included.
AccessToken is zeroized on drop, not Clone, and Debug-redacted. Provider copies,
serialized BSON/Bytes and TLS buffers are not guaranteed to be zeroized.
No persistence or automatic refresh is implemented. An authorization provider is
trusted application code: it must validate its response, honor verification
requirements, and avoid retaining transport clones or tokens. No current
KakaoTalk provider is bundled. All authentication failures are terminal for the
session; create a new secure session only after addressing the actual cause.

## Explicit LOCO-GCM protocol profile

`protocol-connect` never treats a TLS failure as permission to downgrade.
RSA server-key provenance must be independently trusted by the caller. A copied
public-key fixture is not evidence that it is current or belongs to the intended
endpoint; it is not selected automatically. Sending the OAEP handshake alone
does not confirm server possession. Key confirmation requires an authentic GCM
response. RSA SHA-1 OAEP and AES-128-GCM parameters are source-backed protocol
choices, not recommendations for designing a new cryptosystem. Library RSA use
is public-key encryption only; private RSA decryption is test-only.

Discovery hosts are untrusted data. A dialer must enforce destination/network/
proxy policy on every address, even after a syntactically valid response. No
library method opens direct Internet sockets. GCM tag failures are fatal and
never skipped. No raw account-authentication response is logged. This milestone
implements no registration or refresh endpoint. Future registration needs
explicit approval and legitimate device confirmation; do not copy a flow that
registers automatically after a rejection or logs credential-bearing responses.

## Concrete Android provider

The experimental android-auth provider never starts registration after -100 on
its own. The application must obtain explicit user approval first; the CLI asks
separately and refuses nonterminal unattended approval. Legitimate phone/device
verification is server mediated. No forced login or automatic retry after any
rejection is available. HTTP redirects and automatic retries are disabled.

Production auth URLs are fixed HTTPS and use system trust plus inherited proxy
settings. Response bodies are bounded and zeroized on release; credential fields
are redacted zeroizing values. No raw response or reqwest cause is included in
errors. HTTP/TLS/serialized-body copies may still exist temporarily; complete
memory erasure is not claimed. Local credential age is not called server expiry.
Only owner-private stable device identity is persisted, never passwords/tokens.
Unix mode/owner/symlink checks are enforced; unsafe existing identity is not replaced.

The CONNECT dialer must use approved proxy/network routes and exact endpoint
allowlists. Help and check-config do not establish identity, network access or live
compatibility. The owner must independently trust the RSA pin and approved account
profile before interactive validation. See docs/authentication.md.
