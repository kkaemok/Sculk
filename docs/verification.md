# Verification record

## Phase 5 — 2026-10-26

| Check | Outcome |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | 85 passed; none failed/ignored/filtered |
| `cargo test --workspace --locked` | 23 passed; optional provider/read-only/tool features not enabled |
| `cargo build --workspace --release --all-features --locked` | Passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked` | Passed |
| CLI `--help` | Passed; no network |
| CLI `--dry-run ... --report NEW_PATH` | Completed local validation; private sanitized report, no identity creation/network |
| Read-only source-fixture generator | Executed actual source methods with all sockets intercepted |
| Public reference refresh | Four fetched HEADs unchanged; research.md records exact revisions |
| `git diff --check` | Passed |

All original 73 tests remain. Twelve additions: five CLI tests (strict options,
pin/report sanitization, level-triggered cancellation and two owned-FD input
regressions), two provider audit tests (sensitive-header Debug and expired device
confirmation), and five read-only TLS/mock-session tests. Read-only requests are
byte-compared with actual Agent source-generated contracts; response schemas are
synthetic, never live captures. Int64 IDs remain exact beyond 2^53, and malformed,
duplicate, oversized, mismatched or non-progressing responses fail explicitly.

The input cleanup test accepts EOF or ECONNRESET after cancellation: Unix streams
with unread partial input can legitimately reset when the owned endpoint closes.
It still requires the cancelled future to finish promptly and the descriptor to
be closed; no assertion or old regression was disabled. New input reads are
bounded and do not use an uncancellable Tokio stdin worker.

Audit fixes include earlier password/old-token/provider drops, terminal-only code
display (not stdout/stderr/report), sensitive X-VC headers, absolute HTTP deadlines,
expired-registration termination and supported RustCrypto zeroize features for
key/hash-state cleanup. Memory erasure of all allocator, serialized, TLS or kernel
copies is not claimed. No TLS bypass, alternate hostname, silent downgrade,
forced session replacement or automatic authentication retry was introduced.

The dry-run report was validated as mode 600 and contained only mode/last_stage/
outcome/live_connection_verified=false. The identity path remained absent. No
live account was explicitly supplied/approved, no password/phone approval could
be collected safely in this API session, and **no live authentication or read
against KakaoTalk was attempted**. authentication.md provides the owner workflow.
No new benchmark was run; performance.md retains and qualifies prior measurements.

GitHub upload was explicitly authorized with Phase 5. At the start remote main
was ab89801 and local work was 7a2746a. A fetch and merge-base ancestor check
confirmed remote history is preserved. Upload uses a normal fast-forward push,
never force, and is checked against the remote HEAD after completion. Source
upload is separate from any live-account authorization, visibility change,
package publication or release; none of those latter actions was authorized/run.

---

## Phase 4 — 2026-10-25

| Check | Outcome |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | 73 passed; none failed/ignored/filtered |
| `cargo test --workspace --locked` | 23 passed; opt-in provider/example not enabled |
| `cargo build --workspace --release --all-features --locked` | Passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked` | Passed |
| Auth example `--help` with Cargo feature live | Passed; no account requests |
| Auth example `--check-config` with private synthetic config | Passed; no network or identity creation |
| `cargo bench -p sculk-client --all-features --bench secure_transport --locked` | 208,743 encrypted pairs/s; performance.md |
| Source HTTP-auth fixture generator | Executed with intercepted fetch; no live traffic |
| `cargo metadata --all-features --locked` | Updated dependency license metadata |
| `git diff --check` | Passed |

The 17 added tests comprise 15 provider/storage/integrated HTTP tests plus two
CONNECT-proxy tests. All original 56 tests are retained. Provider tests run real
bounded asynchronous HTTP requests over loopback, with source-backed request
fields and synthetic response fixtures. The production provider has fixed HTTPS
origin, default certificate validation, disabled redirects/retries, bounded bodies
and no public test-origin override. Test-only code replaces its origin/client for
local fixtures; no production TLS downgrade is involved.

The integrated test obtains HTTP credentials with exact account 9007199254740993,
then performs TLS GETCONF and RSA-OAEP/GCM CHECKIN/LOGINLIST, asserts the Int64
value and validates that exact account again. It also changes the assigned server
endpoint and delivers partial packets across real encrypted records. Existing
crypto, malformed-packet, session-expiry and connection-cleanup tests still pass.

New tests cover credentials/status rejection, approval required before any device
request, rejected registration without another login, email/account mismatch,
server/local expiration, refresh token rotation/missing refresh/mismatch/rejection,
response limits/redirects/interrupted bodies, operation drop/cancellation/deadline,
registration poll budgets and expired approval windows, owner-private stable
identity and symlink/mode rejection, proxy denial/cancellation/deadline and no
over-reading of tunneled bytes. Source-generated HTTP fixtures validate form,
JSON, Android headers and X-VC, not live service acceptance.

No live login, challenge, registration, refresh or LOCO connection to KakaoTalk
was authorized or attempted. No credential values were printed. Only help and
check-config were executed in the developer example. The safe owner validation
procedure and exact remaining prerequisites are in authentication.md.

Repository inspection: origin is github.com/kkaemok/Sculk. Remote main and the
starting local work branch were ab89801 (README-only initial commit). All Phases
1–3 implementation files were uncommitted at the start of Phase 4. The requested
local commit records Phases 1–4; the remote remains unchanged because pushing is
not authorized. This explains why GitHub has no implementation despite the local
workspace. No visibility change, package publication or release was performed.

Prior unrun audit/deny/fuzz-campaign/soak/target-hardware limitations still apply.

---

## Phase 3 — 2026-10-25

| Check | Outcome |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | 56 passed, none failed/ignored/filtered |
| `cargo test --workspace --locked` | 23 passed; optional connection/TLS/GCM suites not enabled |
| `cargo build --workspace --release --all-features --locked` | Passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked` | Passed |
| `cargo bench --workspace --all-features --locked` | Packet, GCM, RSA setup, mock transport completed; performance.md |
| Upstream Agent Messenger selected tests (four files) | 34 passed, 0 failed, 102 assertions; offline mocks only |
| Source request/response fixture harness | Executed, portable regeneration rerun, 15 connection tests passed afterward; no live traffic |
| `cargo metadata --all-features --locked` | Refreshed dependency license metadata |
| `git diff --check` | Passed |

The 15 new protocol-connect tests exercise successful TLS GETCONF → RSA/GCM
CHECKIN → RSA/GCM LOGINLIST, advertised server/port selection, malformed Booking,
rejected Checkin, rejected authentication, unexpected response commands,
interrupted connections, expired tokens before dial, overall deadlines,
cancellation during dial/key setup, cleanup after errors, split/coalesced
records/packets, oversized/truncated records, tag failure, empty records,
source-generated request bytes and a separate Node-crypto vector. All original
41 all-feature tests still pass. The existing session lifecycle is reused through
an additive internal factory for an explicitly selected secure LOCO stream.

GETCONF and LOGINLIST byte fixtures come from actual Agent source encoders with
synthetic credentials. Response fixtures are schema-shaped synthetic test inputs,
not live captures. CHECKIN intentionally widens account IDs to Int64; this and
profile/transport selection remain compatibility assumptions. RSA mock servers
assert handshake constants, decrypt OAEP/SHA-1 with their synthetic private key,
and exchange real authenticated AES-GCM records. Known public key import is
validated separately without claiming the pin is current. Fixture provenance and
regeneration instructions are in tests/fixtures/README.md.

The reference tests initially lacked zod; installing its declared major version
resolved that dependency error without changing source/tests. Only selected
upstream tests were executed; neither a whole upstream test pass nor live
registration/login compatibility is claimed. Bun/Node are isolated research-only
tools, and are not required by the Rust workspace tests or client runtime.

The existing fuzz target and audit/deny/soak limitations remain as recorded in
prior phases. No live account was authorized or contacted, no device registered,
no refresh/password request sent, and no package/release published. No secret
values were printed. The remaining blocker is a legitimately acquired current
LOCO credential/profile with approved device verification and trusted endpoint/
key identity. The native initial protocol sequence itself is now offline-tested.

---

## Phase 2 — 2026-10-25

| Check | Outcome |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | 41 passed, none failed/ignored/filtered |
| `cargo test --workspace --locked` | 23 passed; TLS/session/GCM features intentionally absent |
| `cargo build --workspace --release --all-features --locked` | Passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked` | Passed |
| `cargo run -p sculk-basic-client --locked` | Mock PING acknowledgment and clean shutdown |
| `cargo bench --workspace --locked` | Completed; phase-2 samples in performance.md |
| `cargo metadata --all-features --locked` | Updated dependency license metadata |
| `git diff --check` | Passed |

All-feature breakdown: 10 protocol tests, 2 experimental GCM tests, 9 transport
tests, 4 recovery tests, 6 TLS tests and 10 session tests. Two property tests
still execute 512 generated cases each. No live service/account test was run.
The TLS tests generate ephemeral certificates and use rustls over local duplex
streams. The existing plain TCP integration test remains a local mock.

Reviewed 22-byte LE header offsets and BSON type 0 against the unchanged fetched
reference snapshots. Header length limits are checked before reserve/peer-directed
allocation; exact 1 MiB and one-byte-over limits are now exercised. Type 8 remains
explicitly unsupported. GCM remains experimental and independent of chat TLS.

Fixed false cancellation-watch updates and response-deadline checking. Added
regressions for both. Added state-machine tests for accepted/rejected/challenged/
unsupported providers, credential and lease expiry, account mismatch, malformed
and duplicate fields, malformed frames, disconnected peers, authentication
future drop, cancellation and attempt deadlines. Authentication is single-attempt;
only the existing explicitly transient connector retry policy retries.

One TLS test initially dropped the client before the server finished flushing
handshake output, producing BrokenPipe in the test peer. Holding the established
stream through the server handshake fixed the test-lifetime error; certificate
checks and authentication assertions were retained.

No current production credential/device-login provider, live account validation,
coverage-guided fuzz campaign, audit/deny run, soak test or Oracle VM performance
test is claimed. Live login is blocked by absent authorized credentials and an
unverified current device/identity/profile flow. See research.md and compatibility.md.

---

## Phase 1

Validated in the managed Linux environment on 2026-10-24. No live account used.

| Check | Outcome |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | 22 passed; none failed, skipped, ignored or filtered |
| `cargo build --workspace --release --all-features --locked` | Passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked` | Passed |
| `cargo run -p sculk-basic-client --locked` | Mock PING receives matching ID and command, then clean shutdown |
| `cargo bench --workspace --locked` | Completed codec and two-runtime transport workloads; performance.md |
| `cargo check --manifest-path fuzz/Cargo.toml --locked` | Fuzz target compiles |
| `cargo metadata --all-features --locked` | Dependency license metadata recorded in THIRD_PARTY.md |
| `git diff --check` | Passed |

Test breakdown: 9 framing/BSON tests, 2 secure-record tests, 8 transport tests,
3 retry/cancellation tests. Two framing tests each execute 512 generated cases.
Golden PING bytes are synthetic. TCP tests bind loopback and require no external
network. Empty library/main unit-test runners and rustdoc runners are not counted
as tested behavior.

An initial test used a 12-byte command where the test intended to exercise an
11-byte boundary. The codec correctly rejected it; the fixture input was corrected
and the suite rerun. Later checks added duration/capacity guards and cancellation
coverage. No assertions were disabled to obtain a pass.

The fuzz harness compiles, but a coverage-guided fuzz campaign has not been run.
cargo-audit and cargo-deny have not been executed; advisory and full source-license
review remain release prerequisites. Their role and usage are in CONTRIBUTING.md.
No live authentication, event schema, messaging, moderation, or 24-hour stability
claim follows from offline tests. Refer to compatibility.md for blockers.

The complete scripts/cloud-install.sh was executed successfully after initial setup;
its repeat run rebuilt with --locked, passed all 22 tests and ran the mock example.
Cloud install_script and start_skill drafts were saved; saving is not publication.

## Phase 6 — 2026-10-28 (Asia/Seoul)

Restored checkout at confirmed `11c5ab5320ec52d0e64c03d9d94b7a160bad8c91`;
public origin HEAD matches that commit. The environment had no Rust installation;
stable 1.99.1, rustfmt and clippy were installed before changes. The unchanged
all-feature baseline passed **85 tests** before implementation. No prior milestone
was recreated and no existing test/assertion was removed or weakened.

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | **95 passed**, none failed/ignored/filtered |
| `cargo test --workspace --locked` | **23 passed**, none failed/ignored/filtered |
| `cargo build --workspace --release --all-features --locked` | Passed |
| `cargo test -p sculk-client --features messaging --test messaging --locked` | **9 passed**, feature independently verified |
| `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --locked` | Passed |
| `cargo +nightly fuzz run decode -- -max_total_time=30 -rss_limit_mb=512` | **348,708 runs in 31 seconds**, no crash; seeded with binary fixtures including synthetic MSG |
| `git diff --check` | Passed |

The new tests cover exact large IDs, borrowed/raw payload retention, message type
classification, invalid required/optional fields and duplicate keys, unknown and
malformed event preservation, explicit lag from a stalled consumer while requests
still complete, cancellation/drop safety, closure and lease expiry with queued
content. Text tests pin actual source method bytes, local TLS WRITE acknowledgment,
rejection/invalid-response outcomes and ambiguous timeout with exactly one write.
The added CLI test verifies sanitized typed categories; option tests reject listen
in dry-run mode and invalid durations. Existing authentication/TLS/approval guards
remain covered by the full suite.

The local TCP/terminal tests require the execution sandbox's supported network
setting; running without it produced permission errors, then passed unchanged
under that setting, as did the baseline. This grants the local test capability;
no live Kakao destination was contacted. Fixture generation ran pinned source
methods with a replacement no-network connection module; all values are synthetic.
The fuzz result is a short smoke campaign, not comprehensive correctness/security
proof. Fuzz dependencies are separately locked; the root Cargo.lock is unchanged.

Changed implementation/test files: `crates/sculk-client/{Cargo.toml,src/lib.rs,
src/session.rs,src/messaging.rs,tests/messaging.rs}`, `examples/auth-client/{Cargo.toml,
src/main.rs}`, `benches/events.rs`, `fuzz/{Cargo.toml,Cargo.lock,fuzz_targets/decode.rs}`,
and `tests/fixtures/{agent-write-request.bin,node-msg-synthetic.bin,
tools/generate-messaging.cjs,README.md}`. Documentation updates: `README.md`,
`SECURITY.md`, `CHANGELOG.md`, and `docs/{architecture,authentication,compatibility,messaging,
performance,research,verification}.md`.

Live compatibility remains unverified. Environment readiness reports no credential
bindings, no Kakao HTTP grant and no raw TCP destination grants. Owner approval,
legitimate device confirmation, an accepted current profile, independently trusted
key/endpoint identity and permitted routes remain necessary. The owner CLI can
receive counts for a bounded window after LOGINLIST; it cannot send text. No live
authentication/registration/refresh/message, publication, visibility change or
remote push was performed. Completed changes are committed locally only.

The event benchmark completed for capacities 8/16/64/256 and text sizes 1/64 KiB;
all retained/lag counts matched the bounded ring, with performance scope/results
in performance.md. `cargo run -p sculk-auth-client --all-features --locked -- --help`
completed without account/configuration/network operations.

Dependency security: `cargo audit --json` (cargo-audit 0.22.2; database commit
`99670cfc0bc574433034b75876a4b791ccaa2efb`, updated 2026-10-27) exited nonzero
for **one existing advisory**, RUSTSEC-2023-0071 in rsa 0.9.10; no patched release
is listed, and there were no informational warnings. Runtime RSA is public-key
encryption only; private-key decryption is confined to local synthetic test peers.
See SECURITY.md for scope and limitations. No ignore rule, dependency downgrade,
custom cryptography, TLS bypass or test weakening was introduced.

`cargo audit --file fuzz/Cargo.lock --json` passed: zero advisories and no
informational warnings for the separately locked messaging/framing fuzz harness.

## Phase 7 — 2026-10-28 (Asia/Seoul)

Base `6476179206f7e458cd128ad6633a80768e54e3b5`; clean existing checkout preserved.
The unchanged all-feature baseline passed **95 tests before modifications**.
No project recreation, protocol rewrite, dependency upgrade, disabled assertion,
ignored test or weakened security check was used to obtain passing results.

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed; fuzz manifest formatted separately |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed |
| `cargo test --workspace --all-features --locked` | **111 passed**, none failed/ignored/filtered |
| `cargo test --workspace --locked` | **23 passed**, none failed/ignored/filtered |
| `cargo test -p sculk-client --features synchronization --test synchronization --locked` | **7 passed**, isolated optional feature |
| `cargo test -p sculk-client --features checkpoint-file --test checkpoint --locked` | **2 passed**, isolated optional feature |
| `cargo build --workspace --release --all-features --locked` | Passed |
| `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --locked` | Passed |
| `cargo run -p sculk-auth-client --features live --locked -- --help` | Passed, no account/configuration/network operation |
| `cargo bench -p sculk-client --all-features --bench reliability --bench events --locked` | Completed; parsing, dedup, state, jitter/local TLS lifecycle and isolated total RSS results in performance.md |
| `cargo +nightly fuzz run decode -- -max_total_time=30 -rss_limit_mb=512` | **847,438 runs in 31 seconds**, no crash; framing, MSG/raw-view access and SYNCMSG parsing |
| `cargo audit --json` | Nonzero: one existing RUSTSEC-2023-0071/rsa 0.9.10 advisory; no patched version listed, no warnings/suppressions |
| `cargo audit --file fuzz/Cargo.lock --json` | Passed: zero advisories/warnings |
| `git diff --check` | Passed |

New behavioral coverage includes same-ID duplicates/conflicting records, equal
message timestamps with distinct large Int64 IDs, out-of-order history and live
messages, genuine server-returned previously unprocessed logs, predecessor gaps,
truncation/minimum history, unsupported schemas, wrong-room/range/type/duplicate-key
corruption, empty nonprogressing pages and incomplete upper anchors. Processing
tests exercise ordered acknowledgment, store failure, cancellation and partial
restart without advancing the last safe checkpoint prematurely. Bounded recent-ID
expiry returns OlderUntracked rather than falsely dropping a message as duplicate.

Local TLS integration covers subscription before catch-up, live/history overlap,
unknown packets, slow consumers and explicit queue overflow, partial sync interrupted
by disconnection, bounded transient reconnect, rejection/verification/policy/ambiguous
login timeout as terminal, cancelled attempts with dropped guards, shutdown with an
unanswered request, expiry intervention and a source-backed PING. Explicit shutdown
initially exposed missing TLS close_notify; the driver now performs bounded,
cancellable graceful close only for explicit disconnect. Failed-write/abort paths
still drop without an implicit flush/replay. Assertions were retained.

Checkpoint-file tests exercise exclusive single writer, exact large-ID reopening,
0600 file modes, CAS/backward-boundary rejection, corrupt/unsafe/symlink/stale-file
refusal and lock cleanup after failed open. Disk checkpoints do not transact with
application side effects. Crash durability after fsync failure/host power loss,
long-duration leak/soak tests and exactly-once effects are not claimed.

The receive-one CLI tests verify live-only/mutually exclusive flags and selected
room/timestamp validation using synthetic messages. The procedure in authentication.md
requires account consent, separate test-room consent and legitimate phone approval
when needed. Reports contain only static labels/booleans. No message body, personal
identifier, account secret, live login, registration, refresh, message send, read
mark or moderation operation was exercised against KakaoTalk.

Research refreshed Agent Messenger/node-kakao unchanged HEADs, executed the actual
source syncMessages builder with a no-network replacement, and reviewed documented
SYNCMSG/minLogId/isOK and CHATINFO watermark behavior. Replies are explicitly
synthetic. Numerical IDs are not assumed contiguous; timestamps are not keys.
A completed bounded server window is not global, lossless control-event replay.

Security review confirms RSA private-key operations occur only in synthetic test
peers (including cfg(test)-gated auth/tests.rs). Production ServerKey parses a
public key and encrypts; normal AES-GCM decryption is not RSA private-key decryption.
The RustSec database is `99670cfc0bc574433034b75876a4b791ccaa2efb` (updated
2026-10-27), as in Phase 6. Root/fuzz lockfiles and runtime dependency versions did
not change. No advisory ignore, custom cryptography or certificate bypass was added.

Live readiness: the **software procedure is prepared and offline-tested**, but this
environment is **not ready for a live account test**. It has no configured account
credentials/approval, owner-selected verified server key/profile configuration or
Kakao HTTP/raw-TCP grants. The enforced network snapshot contains no Kakao HTTP
destination and raw TCP domains/IP ranges are empty. Acceptance of the researched
Android endpoints/profile, legitimate device approval, public-key identity, actual
transport selection, token lease and server-assigned endpoint schemas remain live
compatibility blockers. No secrets were requested in chat or inferred from fixtures.

The next smallest milestone is one owner-observed selected-room login/receive-only
run. After its sanitized metadata evidence, validate a bounded read-only history
window and one controlled connection-loss/reconnect with acknowledgment/dedup checks
before relying on recovery for a moderation consumer. Live sends and moderation
remain separately unauthorized/unsupported; no WRITE retry was introduced.

Completed work is committed locally only. Phase 6's earlier push authorization was
for that delivered phase; no Phase 7 remote push, release, crate publication or
repository visibility change was performed.

Changed files:

- `CHANGELOG.md`

- `README.md`

- `SECURITY.md`

- `benches/events.rs`

- `benches/reliability.rs`

- `crates/sculk-client/Cargo.toml`

- `crates/sculk-client/src/checkpoint.rs`

- `crates/sculk-client/src/connect.rs`

- `crates/sculk-client/src/lib.rs`

- `crates/sculk-client/src/managed.rs`

- `crates/sculk-client/src/messaging.rs`

- `crates/sculk-client/src/synchronization.rs`

- `crates/sculk-client/tests/checkpoint.rs`

- `crates/sculk-client/tests/managed.rs`

- `crates/sculk-client/tests/synchronization.rs`

- `docs/architecture.md`

- `docs/authentication.md`

- `docs/compatibility.md`

- `docs/messaging.md`

- `docs/performance.md`

- `docs/recovery.md`

- `docs/research.md`

- `docs/verification.md`

- `examples/auth-client/src/main.rs`

- `fuzz/Cargo.toml`

- `fuzz/fuzz_targets/decode.rs`

- `tests/fixtures/README.md`

- `tests/fixtures/agent-syncmsg-request.bin`

- `tests/fixtures/node-syncmsg-synthetic.bin`

- `tests/fixtures/tools/generate-messaging.cjs`
