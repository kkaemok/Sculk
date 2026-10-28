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

An optional experimental Android account/device provider is implemented, but live
compatibility is unverified. Encryption with a supplied key alone is not peer authentication. Do not invent a server key, disable TLS checks,
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
No credential persistence or automatic refresh is implemented. An authorization provider is
trusted application code: it must validate its response, honor verification
requirements, and avoid retaining transport clones or tokens. The optional Android provider remains live-unverified. All authentication failures are terminal for the
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
never skipped. No raw account-authentication response is logged. The optional Android provider
implements explicit registration and refresh. Registration requires
separate approval and legitimate device confirmation; do not copy a flow that
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

## Phase 5 audit fixes

Registration consent is no longer an uncancellable blocking stdin task. The CLI
reads a bounded line from its owned nonblocking controlling terminal with a deadline
and Ctrl-C support. The required Tokio descriptor registration has a locally
commented ownership invariant; the descriptor is never replaced or exposed.
Approval codes go only to the approving terminal, never stdout/stderr or reports.
X-VC is marked sensitive in header Debug, and primary password/old-token/provider
values are dropped earlier. Complete transport-buffer erasure remains unproven.

An independently trusted SHA-256 key fingerprint is required before live account
requests. Dry-run performs no network activity. Reports contain only static stage,
mode/outcome and verified-live boolean, use exclusive 0600 creation with no-follow,
and refuse overwrite. Read-only queries require an active Session and never
fabricate missing fields, repeat pages automatically, or print personal payloads.
No forced login, alternate-name TLS validation, automatic downgrade, challenge
bypass, uncontrolled retry or unattended registration path was added.

The crypto audit also found that optional RustCrypto zeroize features had not been
selected. They are now explicitly enabled for AES expanded-key cleanup, GHASH/
POLYVAL state and AES-GCM temporary key material. These are existing transitive
primitives, not replacement cryptography. Source/feature resolution and unchanged
known-answer/record tests verify integration; comprehensive allocator/TLS/kernel
memory erasure is still not claimed.

## Phase 6 dependency audit and messaging boundary

`cargo-audit 0.22.2`, advisory database commit
`99670cfc0bc574433034b75876a4b791ccaa2efb` (updated 2026-10-27), reports one
root-lockfile advisory: [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)
(CVE-2023-49092), Marvin private-key timing side channels in `rsa 0.9.10`.
No patched or unaffected release is listed. The audit exits nonzero; this is not
a clean security result and no ignore/suppression was added.

Reviewed `sculk-protocol/src/handshake.rs`: deployed code imports RsaPublicKey and
performs public-key OAEP encryption only. RSA private-key import/decryption occurs
in local synthetic test peers, not a deployed remote private-key service. The
advisory's private-key-recovery mechanism is therefore not exposed by the reviewed
client path; this scope assessment does not fix the dependency or certify all
cryptographic behavior. Keep private-key operations out of the runtime and track
a maintained upstream fix. Replacing protocol cryptography or disabling security
checks would not be a suitable workaround. The separate messaging fuzz lockfile
has its own audit result in docs/verification.md.

Message parsing happens outside the receive driver and preserves malformed/unknown
packets without converting database feed values into actions. Content, attachment
and metadata access is explicit and sensitive; Debug redacts content/IDs. The ring
is bounded by slots, not application memory: payload backing allocations and
subscriber-held packets require a deployment budget. Lag is explicit and must be
reconciled before processing relies on continuity. Receive subscriptions have a
fixed authenticated lease, cancellation and transport-state guards.

Text encoding performs no networking. No high-level live sender or CLI send option
was added, and live writes still require separate owner authorization. Timeout,
expiry, interruption and invalid acknowledgments never trigger a resend. A valid
WRITE acknowledgment is not recipient delivery, a read receipt or exactly-once
proof. The owner receive-only window records counts, while reports add static
failure categories; raw message/HTTP errors and personal content are not serialized.

## Phase 7 review

Repeated cargo-audit with the same current database snapshot: the root lockfile
still reports RUSTSEC-2023-0071 in rsa 0.9.10, no patched version and no suppression.
The separately locked synchronization/framing fuzz harness reports zero advisories.
Production `ServerKey` imports RsaPublicKey, parses public DER and calls OAEP public
key encrypt only. RsaPrivateKey/OAEP decrypt occurrences are in local synthetic
integration peers and `auth/tests.rs`, which is gated by `#[cfg(test)]`. The runtime
AES-GCM decrypt path is symmetric record authentication, not RSA private-key
operation. No deployed RSA private-key decrypt/sign service is exposed by Sculk.

Available mitigations remain: keep private RSA operations out of the deployed
runtime, track a maintained upstream fix, and use a separately verified compatible
TLS-only route if the actual profile supports one. Do not switch transport blindly,
replace protocol cryptography, suppress the advisory or disable certificate checks
for a clean report. Root Cargo.lock/dependency versions were not changed.

Reconnect retains only one owned session, uses typed non-sensitive I/O kinds,
limits retries, latches permanent authentication/security failures and requires
intervention on credential expiry. It never wraps WRITE in operation replay.
Cancellation drops pending attempt/request state and interrupts bounded graceful
close; explicit shutdown now sends TLS close_notify without flushing failed writes.
No automatic HTTP password/device approval, token refresh or identity rotation
was added. Existing memory-only zeroizing credential storage and its transport/
serialized-buffer cleanup limitations still apply.

Optional checkpoint files contain private room/cursor metadata, no tokens or message
content. A trusted owner-private directory, exclusive lock, no-follow/nonblocking
read, file ownership/mode/type/link/size checks, CAS boundary and atomic fsync/rename
prevent silently reusing unsafe/stale state. A post-rename fsync error poisons the
store. Crash-stale lock/temp files are an explicit operational recovery step;
there is no automatic takeover or exactly-once transaction with application effects.

The receive-one CLI requires separate selected-room consent, validates fresh text
metadata, logs no content or IDs and records only a boolean outcome. It sends no
message/read/moderation command. Unsupported events, lag and expiry fail validation;
a first matching message cannot establish global session/history compatibility.
