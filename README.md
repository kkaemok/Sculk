# Sculk

An independent Rust foundation for an unofficial KakaoTalk SDK, licensed
**MIT OR Apache-2.0**. This is an early protocol/transport implementation, not a
working authenticated KakaoTalk client. Current direct-login compatibility is
unverified; see [compatibility](docs/compatibility.md) and [research](docs/research.md).

Implemented: bounded incremental historical LOCO framing, validated BSON, an
async request driver, correlation, deadlines, cancellation, explicit queue
limits, raw diagnostic events, clean shutdown, and finite connector retries.
An optional experimental AES-GCM record codec has offline cryptographic tests.

## Run the offline example

Install stable Rust with rustfmt and clippy, then from this checkout:

```sh
cargo run -p sculk-basic-client --locked
```

This starts an in-memory mock echo peer and sends a synthetic PING. It does not
log into KakaoTalk, send a chat, or need credentials. The example uses Tokio's
current-thread runtime. The core also works with a multithreaded runtime.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --release --all-features --locked
cargo doc --workspace --all-features --no-deps --locked
cargo bench --workspace --locked
```

The crates have no Android, desktop, Docker, Node, Python, browser, database, or
external service requirement. The optional protocol connector accepts already-authorized credentials;
account/device acquisition is available through the experimental android-auth provider; **never pass raw TCP carrying account tokens** to the
experimental low-level client. No live connection example is provided yet.

[Verification](docs/verification.md) · [Architecture](docs/architecture.md) · [Performance](docs/performance.md) ·
[Contributing](CONTRIBUTING.md) · [Security](SECURITY.md) · [Changelog](CHANGELOG.md)

In this managed cloud environment, activate the installed tools with:

```sh
export CARGO_HOME=/workspace/.cargo RUSTUP_HOME=/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
cd /workspace/Sculk
```

`scripts/cloud-install.sh` reproduces cloud tool installation and offline checks.

## Optional secure-session infrastructure

Enable `sculk-client/secure-session` for standard certificate-validating TLS,
a deadline/cancellation-aware authentication lifecycle, bounded zeroizing token
storage, and an experimental LOGINLIST acknowledgment parser. This feature has
**no account-login provider by itself**; the separate android-auth feature now adds an experimental concrete provider. Providers must
be explicitly supplied; successful mock authorization is not live compatibility.
The TLS helper accepts a caller-connected stream and trusted roots; it does not
bypass proxies, discover endpoints, substitute hostnames, or fall back to GCM.

Run its offline tests with:

```sh
cargo test -p sculk-client --features secure-session --test tls --test session --locked
```

The session tests use real local TLS and an explicitly synthetic `mockToken`
request schema. See [authentication research](docs/research.md) for the remaining
credential/device-verification and endpoint-identity blockers.

## Experimental source-profile connection sequence

`sculk-client/protocol-connect` adds TLS Booking/GETCONF, explicit TLS or LOCO-GCM
Checkin/CHECKIN and session/LOGINLIST, using the existing request/session modules.
It requires caller-supplied legitimate credentials, matching profile, trusted
server key (for GCM), explicit transport policy and an approved dialer. There is
no live-account compatibility claim, automatic device registration or insecure
fallback. See docs/research.md and docs/compatibility.md before using it.

Run the source-compatible **offline** sequence and crypto tests:

```sh
cargo test -p sculk-client --features protocol-connect --test connect --locked
cargo bench -p sculk-protocol --all-features --bench encryption --locked
```

Fixture provenance, synthetic/live distinctions and optional regeneration tools
are documented in tests/fixtures/README.md. Node/Bun are only used to investigate
reference implementations; Sculk builds, tests and runs with the native Rust stack.

## Experimental authentication provider and end-to-end example

`sculk-client/android-auth` adds concrete Android HTTPS login, an explicit human
approval gate for device registration, bounded phone-confirmation polling, one
explicit refresh, stable private device identity and exact account binding.
The example joins it to the existing GETCONF → CHECKIN → LOGINLIST sequence.
Live compatibility remains unverified; no account was contacted during development.

```sh
cargo run -p sculk-auth-client --features live --locked -- --help
cargo run -p sculk-auth-client --features live --locked -- --check-config /ABSOLUTE/private/config.json
```

These commands do not authenticate. The separate --live flag requires explicit
test-account authorization and, when needed, another registration approval.
Passwords are prompted without echo; tokens are not persisted. Read the complete
[owner validation procedure](docs/authentication.md) before any live attempt.
Core default features remain unchanged; the example's live feature is opt-in.
