# Verification record

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
