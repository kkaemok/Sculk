# Changelog

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
