# Changelog

## Phase 9 — 2026-10-29

- Add `sculk-bot`: typed events/context, bounded FIFO dispatch without spawned
  tasks, startup commands/aliases, quoted arguments, permissions and error routing.
- Add bounded recent-ID suppression, explicit overflow/source/cleanup outcomes,
  cancellation during handlers/EOF drain and optional managed-client intake.
- Add checked manager shutdown and room/state observations; preserve explicit
  authentication/recovery policy without automatic login or command replay.
- Add offline ping/help bot with mock receipts, tests, synthetic benchmarks,
  public API documentation and CI execution; no live compatibility claim.
- Reserve the shared `.sculk`/`.sc` script host boundary without adding a language.

## 0.1.0 — unreleased

Phase 5:
- Audit auth/session/transport; fix cancellable consent, approval-code log routing,
  sensitive header redaction, credential lifetimes and expired-registration handling.
- Add required owner-verified RSA fingerprint, local dry-run, sanitized private reports,
  visible state/stages and cancellable optional read-only validation before shutdown.
- Add source-backed bounded room listing, metadata/member subsets and regressions.
- Upload the local implementation to GitHub only after fast-forward ancestry checks;
  no real account test, message send, package publication or release.

Phase 4:
- Add concrete optional Android HTTP login/approval/registration/refresh provider,
  explicit human registration gate, stable private device identity and exact account binding.
- Add hidden-password interactive native example, fixed HTTPS trust, bounded CONNECT
  proxy dialer, no automatic redirects/retries, and a network-free configuration check.
- Add source-generated HTTP auth fixtures, high-ID HTTP→TLS→GCM→LOGINLIST integration,
  lifecycle/error tests and actual secure-stream packet throughput measurement.
- Commit the previously uncommitted Phases 1–4 locally; no remote push or live login.

Phase 3:
- Add optional protocol-connect: TLS GETCONF, bounded server configuration parsing,
  explicit secure CHECKIN and initial LOGINLIST using authorized credentials.
- Add RSA-2048 OAEP/SHA-1 type-16/type-3 key setup and an async bounded GCM byte
  stream without per-record tasks, unsafe downgrade or skipped authentication tags.
- Add source-generated synthetic request fixtures, an independent Node crypto
  vector, full offline sequence/failure tests and encryption/key-setup benchmarks.
- Investigate Agent Messenger auth/refresh; no live login, registration or release.

Phase 2:
- Fix false cancellation-watch updates and late response deadline handling.
- Add optional verified TLS over supplied byte streams, without trust bypasses.
- Add explicit provider-driven session authentication, token/lease expiry,
  zeroizing token storage, cancellation guards and session cleanup.
- Add experimental LOGINLIST acknowledgment subset validation with account
  binding and duplicate-field rejection; no production request builder.
- Expand offline TLS/session integration coverage; current device login remains
  unsupported and no live KakaoTalk compatibility is claimed.

Phase 1:

- Add independent bounded historical LOCO/BSON framing and synthetic fixtures.
- Add async stream requests, response correlation, explicit overload/lag,
  timeouts, cancellation, shutdown, and finite cancellable connector retries.
- Add optional experimental AES-GCM records with a NIST known-answer test.
- Add offline TCP/mock tests, properties, fuzz target, synthetic benchmarks and CI.
- Document inspected reference snapshots and current authentication blockers.
- No live KakaoTalk compatibility, messaging, or moderation claim.

## Phase 6 — 2026-10-28

- Add optional consumer-side typed MSG events with exact identifiers/timestamps,
  zero-copy content/attachment access and raw unknown/malformed preservation.
- Add authenticated bounded reception with explicit lag, cancellation and expiry;
  keep the existing raw event API and default ring capacity compatible.
- Add offline text WRITE encoding and acknowledgment/rejection/unknown-delivery
  classification, including actual source-generated request bytes; no live sender.
- Add owner CLI receive-only window and static sanitized failure categories.
- Extend source research, integration tests, event ring benchmarks and decoder
  fuzzing through the message parser. No live compatibility claim.

## Phase 7 — 2026-10-28

- Add optional source-backed bounded SYNCMSG pages and read-only room watermarks,
  preserving Int64 IDs, unknown types/metadata and distinct history delivery.
- Add per-room deduplication, suspected/confirmed/unavailable gap evidence and
  processing acknowledgments with compare-and-swap checkpoints; no exactly-once claim.
- Add explicit single-session reconnect/resubscription with bounded full jitter,
  transient dial classification, terminal auth failures, page budgets and caller PING.
- Add optional private Unix checkpoint files using exclusive locks and atomic fsync/
  rename, without database/new runtime dependencies.
- Add selected-room receive-one owner validation with separate room consent and a
  redacted boolean outcome; no live account operation was performed.
- Fix explicit TLS shutdown, add sync/recovery/persistence integration coverage,
  extend fuzzing and benchmark parsing/state/reconnect plus total process RSS.

## Phase 8 — 2026-10-29

- Preserve recovered Phase 7 baseline; no live compatibility claim or account activity.
- Record separate typed validation milestones, sanitized numeric statuses and precise
  dial failure categories; never infer message reception from HTTP or LOGINLIST.
- Query selected-room CHATINFO before controlled reception and report checked local
  shutdown failures instead of silently calling every close successful.
- Add checksummed checkpoint format 2; refuse unprotected legacy state without
  automatic migration, and cover abrupt process restart/orphan temporary records.
- Add concrete local TLS validation, Unicode/malformed input and shutdown regressions,
  plus isolated idle/low-traffic CPU/RSS, synchronization memory and checkpoint-I/O samples.
