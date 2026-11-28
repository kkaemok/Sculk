# Ubuntu on Oracle Cloud

Build on native x86-64 or AArch64 Linux. The current workspace was tested on
x86-64. The manual Native Linux validation workflow builds, tests and runs
the storage benchmark on both Ubuntu runner architectures when dispatched.

~~~sh
uname -m
rustc -Vv
cargo +1.100.0 build -p sculk --locked --release --features live-validation --jobs 1
~~~

Use the pinned Rust toolchain and a system C compiler for the build. The
executable uses standard Linux system libraries and bundled SQLite. The owner
probe needs a system CA store. Use a build host with the same architecture and
a compatible Ubuntu release if compiling on a 1 GiB instance is impractical.

## Private configuration

For a first installation, build from the checkout and create the dedicated
account and directories. Run the installation commands as an administrator.

~~~sh
sudo useradd --system --home-dir /var/lib/sculk --shell /usr/sbin/nologin sculk
sudo install -m 0755 target/release/sculk /usr/local/bin/sculk
sudo install -d -m 0750 -o root -g sculk /etc/sculk /etc/sculk/scripts
sudo install -d -m 0700 -o sculk -g sculk /etc/sculk/private /var/lib/sculk /var/lib/sculk/device
sudo install -m 0640 -o root -g sculk deploy/ubuntu/sculk.toml.example /etc/sculk/sculk.toml
sudo install -m 0600 -o sculk -g sculk deploy/ubuntu/auth.json.example /etc/sculk/private/auth.json
sudo install -m 0640 -o root -g sculk examples/openchat-bot/scripts/chat.sculk /etc/sculk/scripts/chat.sculk
~~~

Fill the authentication template with the authorized account, exact room and
client profile, approved endpoints, and independently trusted public key and
fingerprint. Store the key as an owner-private file. The template intentionally
contains empty account and key settings. Keep the sample write setting disabled
through the read-only checks.

## Network and owner checks

Check the VM clock and CA store. The direct route uses the VM resolver and opens
TCP to each explicitly approved endpoint. For CONNECT, configure the approved
proxy host and port in the JSON. Preflight checks the named routes and reports
each target by ordinal. It sends no LOCO request or account credential, and
proxy-side resolution follows the proxy's DNS policy. Provider HTTPS uses its
normal system trust and authorized HTTPS egress separately.

~~~sh
sudo -u sculk sculk --config /etc/sculk/sculk.toml doctor
sudo -u sculk sculk auth check /etc/sculk/private/auth.json
sudo -u sculk sculk auth preflight /etc/sculk/private/auth.json
~~~

Run the interactive login, room lookup, controlled receive and separately
confirmed send in the approved test room. Run the exact trigger test before
approving scripts for continuous use. Complete any legitimate device
confirmation interactively before service startup. Only after live testing
of the exact profile and route, record the owner decision with
`"profile_validated": true`. For `android-tablet` also record
`"validated_protocol_profile": "android-tablet"`.
[Owner validation commands](authentication.md).

## Install and start the service

~~~sh
sudo install -m 0644 deploy/ubuntu/sculk.service /etc/systemd/system/sculk.service
sudo install -m 0755 deploy/ubuntu/start-sculk /usr/local/sbin/start-sculk
sudo systemd-analyze verify /etc/systemd/system/sculk.service
sudo systemctl daemon-reload
sudo /usr/local/sbin/start-sculk
sudo journalctl -u sculk.service -f
~~~

Use an administrator terminal for start-sculk. It asks for explicit account
and room consent, then obtains the password without terminal echo. The staged
credential lives in a root-private directory under /run, expires for new use
after 120 seconds and is removed by the launcher and unit. systemd gives the
service a private per-run credential file. Only the in-memory session token is
used for bounded reconnects. The service requires a previously approved device.
The unit is started manually and has no boot or automatic restart policy.

For an already validated deployment that should send script replies, explicitly
set live.enable_write = true and run sudo /usr/local/sbin/start-sculk --write.
The launcher requires a separate write confirmation. The service rejects an
approval whose scope disagrees with its configuration. Run the service and
individual owner probes serially with the same approved device identity.

The unit uses the unprivileged sculk account, a private StateDirectory, read-only
operator configuration, a 256 MiB memory cap, task and file-descriptor limits,
restricted address families, and SIGTERM for shutdown. systemctl start confirms
that systemd executed the binary. Look for live/ready in the journal to
confirm authenticated room verification and the current synchronization boundary.
Periodic live/healthy lines carry bounded counters. A recoverable loss can produce
live/reconnecting and a new generation. A credential expiry, permanent rejection,
unavailable history or uncertain WRITE ends the run with a stage-specific reason.
For changes to credentials, profile or scripts, stop the service and authorize
the new run after the relevant checks.

## Backup and recovery

~~~sh
sudo systemctl stop sculk.service
sudo systemctl is-active sculk.service
~~~

Confirm the process stopped before copying the owner-private SQLite database,
the primary checkpoint and its write-attempts checkpoint. Keep both checkpoint
files together, plus the device identity in a separate private backup. Preserve
file ownership, restrictive permissions and version-2 contents. The root-private
/run approval and lock/temp files are transient. After a hard crash, verify
there is no remaining owner process before following the checkpoint lock recovery
procedure. Restoring an earlier checkpoint never authorizes resending an
ambiguous reply. If the durable write-attempt boundary cannot be established,
reconcile the selected room with the account owner before enabling writes.
[Checkpoint recovery](recovery.md).

The unit was syntax-checked against the locally built executable. A running
Oracle instance, an AArch64 host and a 24-hour live session were unavailable
for this verification.
