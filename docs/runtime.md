# Sculk runtime

Use `sculk script check --json` for stable compiler diagnostics and
`sculk script commands` for names, aliases, prefixes and argument types.
These commands are offline. The authenticated Unix runner accepts SIGHUP to
compile a replacement from its configured script directory on a blocking
worker. Activation is between events and shares the existing stored state and
connection. A failed compilation or registration keeps the prior program.
For services, use `systemctl reload sculk.service` and inspect the journal for
`scripts-reloaded` or `script-reload-rejected`. Scope and connection settings
are fixed for the lifetime of the runner.

The optional owner probe exposes bounded transport diagnostics:

```sh
sculk protocol status /private/auth.json
sculk protocol monitor /private/auth.json --listen 20
sculk protocol stats /private/auth.json
```

Status performs local configuration validation. Monitor and stats go through
the ordinary interactive account gate and report after the requested probe.
Trace records contain local request correlation, allowlisted command names,
timing, sizes, queue depths and transport outcome. Unknown command names are
masked. Bodies are never retained and diagnostic consumers cannot block the
network actor. A reported transport acknowledgment may still fail application
status or receipt validation.

Build or install the executable from the repository with a stable Rust toolchain.

~~~sh
cargo install --path apps/sculk --locked
sculk init
sculk doctor
sculk run --offline
~~~

Type !ping or /help for a mock reply. Type !greet "Ada Lovelace" to pass one
quoted argument and !count to update a global counter. The starter bundle
also answers 안녕. Enter :status to see completed events, commands and failures.
Enter :quit or close standard input to exit.

The executable reads sculk.toml by default. Place --config PATH before
the command to choose a different file. Relative script and database paths
resolve against that file's directory.

## Configuration

~~~toml
[bot]
scripts = "scripts"
default_prefixes = ["!", "/"]
case_sensitive = true
offline_room = 1
offline_sender = 2

[limits]
event_capacity = 16
handler_ms = 100

[storage]
enabled = false
path = "data/sculk.db"

[[rooms]]
id = 1
prefixes = [">", ">>", "봇아 "]

[[permissions]]
name = "bot.manage"
default = false

[[permissions.rules]]
room = 1
sender = 2
grant = true
~~~

All fields are optional. The example with a room override invokes the starter
ping command as >>ping or 봇아 ping. A room override replaces the registry's
prefixes in that room, including command-specific prefixes. Names and aliases
must be unique when a room override exposes them under the same prefixes.
Otherwise commands use their own, file-level, or host-default prefixes.
Permission rules apply to the configured synthetic sender and room. They do
not imply KakaoTalk roles. Declare granted IDs in scripts with, for example,
permissions: "bot.manage".

The console uses the configured offline_room and offline_sender. Add
entries in [[rooms]] and use :room ID to switch between synthetic rooms.
A deployment's local TOML contains scripts, resource limits and application
permissions. The private JSON used by the owner probe is a separate file.

## Check and reload

sculk script check validates source and registration, including permissions,
aliases and room prefixes. sculk script list prints bundle filenames. The host
discovers regular .sculk and .sc files in one directory, sorts paths, and
compiles both with the same grammar. Use :reload in the running console to
compile, validate and swap the bundle between events. A failed reload leaves
the previous bot and its counters active. Use :list to inspect files on disk.
Changes to TOML take effect on restart.

## Persistence

Set storage.enabled = true to persist the {global::name} integer counters
on Unix. The executable opens one SQLite connection, locks the state against a
second Sculk process, and validates integrity, schema version and entry bounds.
The containing directory must be owner-private, such as mode 0700. New state
files use mode 0600. The database commits changed snapshots in a transaction
after dispatch of each synthetic event, including counter changes made before
a later handler fails. Reads and writes occur outside the Tokio dispatcher.
A failed transaction stops the console. Earlier committed snapshots remain
available.

The schema stores up to 256 named signed 64-bit integers. It has no message
log, credentials, permission grants or message delivery transaction. Connection
checkpoints still use the client library's documented recovery store.
[Checkpoint format and ownership](recovery.md).

For backup, stop the console cleanly and copy the database to a private backup
directory. Preserve its mode and ownership when restoring it with the console
stopped. Retain the current database as a separate backup during restore. On
startup Sculk validates the restored file before dispatch. Deleting a counter
in a script removes it on the next successful state commit. Remove any copies
from backups according to the retention requirements for that deployment.

## Account-owner validation and live runtime

Build with --features live-validation to include the owner probe and Unix
live runner. The offline console continues to use its mock adapter.

~~~sh
sculk auth check /private/auth.json
sculk auth preflight /private/auth.json
sculk auth login /private/auth.json --room-info --receive-one 60
sculk auth login /private/auth.json --send-one 5
sculk --config sculk.toml run --live /private/auth.json --validate-command /private/trigger.txt --report /private/bot-report.json
~~~

Each account operation needs interactive authorization. The one-command validator
reads one exact UTF-8 command from the owner-private trigger file. It waits in
one selected room and permits a single text attempt for that trigger. Success
requires an actual command dispatch and a correlated WRITE acknowledgment.
It uses temporary processing state and leaves the normal live checkpoint alone.
Use a dedicated test room where other participants cannot accidentally supply the
trigger. [Owner testing procedure](authentication.md).

For continuous use, record the owner-validated service profile in the private
authentication JSON and add live settings to sculk.toml.

~~~toml
[live]
checkpoint_dir = "/var/lib/sculk/checkpoint"
enable_write = false
write_interval_seconds = 2

[limits]
handler_ms = 7000
~~~

Start with sculk --config sculk.toml run --live /private/auth.json. The host
validates scripts and private state before acquiring credentials. Write-enabled
runs require separate explicit consent. Allow a handler deadline above the
five-second request timeout if scripts should be able to observe its full outcome.
Control events and read-only runs use an outbound adapter that explicitly refuses
text sending. The existing permission registry remains local to the application.

The connector subscribes before reading the selected room's verified watermark.
On the first start it records the current watermark and starts live processing
from there. A normal restart or reconnect reconciles the saved checkpoint through
bounded history. A predecessor mismatch or queue lag suspends dispatch until
that reconciliation succeeds. Logs received only as history are reviewed read-only
and never invoked as script commands. The write-attempt checkpoint is committed
before a live handler can submit a request. An ambiguous or rejected attempt
stops the run with an explicit outcome. [Recovery](recovery.md).

The optional offline storage setting commit_every defaults to 1. Setting it to
2 through 256 groups changed snapshots into a full SQLite transaction at each
boundary and on clean EOF or :quit. A crash can lose the uncommitted group.
The live host always saves changed counters before recording the associated
processed checkpoint. [Storage measurements](performance.md).
