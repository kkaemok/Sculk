# Experimental Android authentication and owner validation

The optional `android-auth` feature now implements the **actual documented HTTP
requests**, followed by the existing native LOCO connection sequence. Current
KakaoTalk acceptance is unverified. No live account test has been authorized or
run in this workspace. Do not interpret source fixtures or local-server success
as a live login.

## Provider behavior

The fixed HTTPS origin is `https://katalk.kakao.com/android/account/`. The
provider uses ordinary certificate/name validation, system roots (including
supported SSL_CERT_FILE/SSL_CERT_DIR configuration), inherited HTTP(S) proxies,
no redirects and explicitly disabled HTTP retries. It sends no forced login.

- Initial `login.json`: form fields password, device_name/model_name, forced=false,
  permanent=true, email and the persisted device UUID. Agent/User-Agent/X-VC are
  derived from the selected Android profile and exact email, as in the pinned
  Agent Messenger source.
- `status=-100` only produces NeedsApproval. It sends no challenge or registration
  request. The application must ask the user, then call approve_device_registration.
- `passcodeLogin/generate`: JSON with email/password/permanent and nested device
  name/uuid/model/osVersion. The returned code is displayed deliberately to the
  approving human; raw authentication responses are never logged.
- `passcodeLogin/registerDevice`: bounded polling while the human confirms in the
  legitimate KakaoTalk device. Only -100 with a server interval of 1–30 seconds
  permits another poll. At most ten polls and five minutes; rejection stops.
  A successful registration permits exactly one subsequent login. There is no
  challenge bypass, automatic registration approval or occupied-slot replacement.
- `oauth2_token.json`: one **explicit** refresh using the documented Android
  headers, device-id derivation, access_token/refresh_token and grant_type. A
  rejected refresh is not repeated. Rotation is preserved; an omitted replacement
  refresh token retains the existing one. No refresh loop or background retry runs.

Expected numeric KakaoTalk user ID is mandatory, positive and within signed
64-bit range. Login must return that exact JSON integer. Fractional values,
wrong identities, duplicate known fields and malformed bodies are rejected.
Refresh stays bound to the previously validated account/device and checks any
returned userId. A refresh response without userId does not independently confirm
the new token's account: subsequent LOGINLIST still validates it exactly.

Credentials are zeroizing in-memory values with redacted Debug. Only device
identity is persisted. Server expires_in, when supplied, caps lifetime together
with an explicit local age limit. When absent, the default 15-minute cap is
**local policy**, not an invented server TTL. Provider state/check_expiration and
protocol_token refuse known-expired credentials. Explicit refresh can use expired
access credentials when a refresh token exists; LOCO never replays the failed login.
A refresh does not extend an already-created Session automatically: establish a
new explicitly authorized connection with the returned credentials.

DeviceIdentity::load_or_create uses OS randomness, exclusive 0600 creation,
owner/permission checks, a private real parent directory, bounded reads and final
symlink rejection on Unix. It never overwrites or silently regenerates an existing
identity. Startup file I/O is blocking: run it before Tokio or in spawn_blocking.
Persistent-file support fails explicitly on non-Unix platforms; the HTTP provider
can use a caller-supplied legitimate identity via from_uuid instead.

## Safe, reproducible owner procedure

1. Use a separate account you own and explicitly authorize for this experimental
   test. Obtain its **numeric KakaoTalk user ID**, consistent Android sub-device
   profile and trusted current LOCO public key through legitimate means. A REST
   bearer token is not assumed to be LOCO-capable. Do not guess another account's
   ID, bypass phone verification or disable endpoint identity checks.
2. Configure approved HTTPS access to katalk.kakao.com and the required raw TCP
   proxy grants for Booking/selected Checkin/assigned chat endpoints. The example
   uses an explicit HTTP CONNECT proxy for every LOCO connection and exact endpoint
   allowlist; there is no direct fallback. The managed startup snapshot here lists
   no custom Kakao destinations or TCP grants. No denial is claimed from an unrun
   request: verify current policy and approved routes before testing.
3. Create a private directory (`mkdir -m 700 ~/.config/sculk`), store the trusted
   PKCS#1 RSA-2048 public DER key there with mode 600, and create a mode-600 JSON
   configuration. Replace **all** owner/profile/path/endpoint placeholders below:

```json
{
  "expected_user_id": 0,
  "email": "YOUR_OWN_ACCOUNT_EMAIL",
  "device_file": "/ABSOLUTE/PRIVATE/DIRECTORY/device.identity",
  "server_key_file": "/ABSOLUTE/PRIVATE/DIRECTORY/trusted-public.der",
  "tcp_proxy_host": "proxy",
  "tcp_proxy_port": 8088,
  "allowed_endpoints": [
    {"host": "booking-loco.kakao.com", "port": 443},
    {"host": "OWNER_APPROVED_CHECKIN_HOST", "port": 995},
    {"host": "OWNER_APPROVED_CHAT_HOST", "port": 5223}
  ],
  "app_version": "25.9.2",
  "os_version": "13",
  "api_level": "33",
  "device_name": "SM-T870"
}
```

The zero ID and endpoint/path placeholders are deliberately not runnable defaults.
The profile values reproduce the public reference, not a guarantee of current
acceptance. Confirm the legitimately approved profile before use. The test public
key fixture is **not automatically chosen** or declared trusted for production.
Do not store the password or tokens in this JSON. Configuration, device identity
and public-key files belong outside the repository. Keep the same identity for
subsequent approved runs; do not rotate UUIDs to evade device restrictions.

4. Build/help/configuration validation are safe without account requests:

```sh
cargo run -p sculk-auth-client --features live --locked -- --help
cargo run -p sculk-auth-client --features live --locked -- --check-config /ABSOLUTE/config.json
```

`--check-config` checks local structure, private-file permissions, profile and key
format. It neither creates an identity nor resolves/dials endpoints, verifies the
key's real-world provenance, or authenticates. These modes were exercised here
with synthetic configuration only.

5. Only after the above account, identity and network authorization, run from a
   real terminal:

```sh
cargo run -p sculk-auth-client --features live --locked -- --live /ABSOLUTE/config.json
```

The CLI separately requires the exact word `authorize`, prompts for the password
without echo, and, only when -100 is returned, separately asks for `register`.
It displays the approval code for confirmation in your legitimate app. Ctrl-C
cancels network operations. Never share the password, passcode, token or raw
response in chat or issue reports. `--refresh` is an additional explicit option
for a single refresh **before** LOCO connection; it is not an automatic rejection
recovery path.

6. Success is reported as LIVE only after LOGINLIST acknowledges the exact account.
   The example then disconnects; it sends no messages. Failure is terminal and
   reports only safe status/type information. Stop after rejection, challenge
   failure, key/certificate mismatch, disallowed endpoint, timeout or unknown
   status. Investigate the actual current profile/flow with the account owner
   before another explicit attempt; do not cycle versions, UUIDs or transport modes.

A changing assigned endpoint must pass both configured network policy and the
operator's allowlist; update only after verifying its provenance. TLS validates
its requested endpoint name; GCM requires the configured trusted RSA key and
valid authenticated records. There is no opportunistic downgrade.

## What remains unverified or intentionally absent

No live credential issuance, phone approval, refresh, expiry contract, routing or
LOCO login has been reproduced against KakaoTalk. macOS/Windows account providers,
credential/token persistence, automatic refresh scheduling, forced login,
automatic registration, profile probing, account recovery and message sending
are not implemented by this provider. A successful HTTP provider result is not
called an authenticated LOCO session until the protocol identity check succeeds.

The next minimal owner test is the above one-time authorization/connection only.
After it succeeds, validate a read-only room lookup before an explicitly authorized
single text send. No package push/publication is part of that procedure.
