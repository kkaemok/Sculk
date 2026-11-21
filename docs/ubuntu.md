# Ubuntu on Oracle Cloud

Use this procedure to build the offline host and prepare the interactive account
probe. Sculk has been exercised on local x86-64 Linux. AArch64 is an intended
native build target and has not been tested on Oracle Cloud.

~~~sh
uname -m
rustc -V
cargo build -p sculk --locked --release --features live-validation --jobs 1
install -m 0755 target/release/sculk /usr/local/bin/sculk
~~~

Run the install command with the privileges required by the destination. On
Ubuntu, a stable Rust toolchain and the system C compiler are build-time
requirements. SQLite is bundled. Keep the executable and operator files
under a dedicated unprivileged account where possible. Install the release
binary from a build host of the same target architecture if the VM has tight
memory or CPU limits.

Prepare a private directory for config and data. Run sculk init, check the
starter, and try the offline console. New files created by the executable are
owner-private on Unix. The default runtime needs only a Linux terminal, its
binary, config and scripts. The owner probe also needs a system CA store.
[Configuration and backup](runtime.md).

## Network and authentication

Check that the VM has the correct clock and system CA store. Validate system
DNS for the specifically approved hosts and outbound TCP access to the exact
ports in the owner configuration. Configure an explicit route. For ordinary
direct egress use "connection_mode": "direct" in the private JSON. For
approved HTTP CONNECT use the proxy mode and its host and port. Provider HTTPS
follows its normal HTTPS proxy and trust configuration. Neither route changes
the TLS identities or cryptographic profile.

Run the local configuration check before opening an account session. Obtain
the independently verified RSA key and accepted profile, complete the owner
authorization in a terminal, and progress through the read-only room and
receive stages before one controlled send. Stop on rejected authentication,
endpoint or key mismatch, or unknown status. Keep the sanitized report private.
[Owner procedure](authentication.md).

Systemd supervision of a continuous KakaoTalk bot depends on a working
noninteractive credential and session lifecycle. The interactive owner probe
is a finite validation process. Configure supervision of a future validated
runtime with a dedicated user, a writable private state directory, read-only
configuration and regular backups. Keep permanent authentication and policy
failures outside the restart loop.
