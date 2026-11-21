# Offline OpenChat command example

sculk init installs the example script. Run sculk run --offline and try
!ping, /help, !greet "Ada Lovelace", !count or 안녕. Enable SQLite in the
configuration to carry the counter across console restarts. To require a local
admin permission on a command, add permissions: "bot.manage" to its
definition and configure a named local grant for your synthetic room and sender.

The script operates on ordinary message events. The same command framework can
be attached to an owner-authorized session and selected room, subject to
current profile and transport validation. Welcome and goodbye handlers, server
moderation and native kicked-member list changes need verified server operations.
[Runtime configuration](../../docs/runtime.md) · [Compatibility](../../docs/compatibility.md)
