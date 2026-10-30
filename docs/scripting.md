# Sculk scripting (Phase 10)

`sculk-script` is a small, compiled Skript-inspired language above `sculk-bot`.
It works offline with synthetic events and explicit mock outbound adapters. It
adds no authentication, connection ownership or verified live KakaoTalk sending.

## Quick start

```sh
cargo run -p sculk-script-bot --locked
cargo test -p sculk-script --locked
cargo bench -p sculk-script --bench runtime --locked
```

The example loads actual `hello.sculk` and `commands.sc` files from
`examples/script-bot/scripts/` at startup. Edit those files and run again, or use
the Rust loader below for your own files. It feeds five synthetic messages,
prints `User (synthetic)` / `Bot (mock)`, then drains and disconnects its local
source. `MockAccepted` means no real server received anything.

```text
User (synthetic): 안녕
Bot (mock): 안녕하세요!
User (synthetic): /ping
Bot (mock): Pong!
User (synthetic): !ping
Bot (mock): Pong!
User (synthetic): >>greet Sculk
Bot (mock): Hello, Sculk!
```

Both `.sculk` (recommended) and `.sc` use **one lexer, parser, compiler and runtime**.
Extensions are case-sensitive; other extensions are rejected. There is no legacy
mode, extension-specific grammar or runtime source evaluation.

## Layout, literals and comments

Use four spaces per indentation level; indentation tabs, unexpected jumps and
empty blocks are errors. Blank lines and `#` comments are ignored. `#` inside a
quoted string is data. UTF-8 and CRLF source are supported. Keywords are lowercase.

Strings use double quotes and support `\n`, `\r`, `\t`, `\"`, `\\` and `\%`.
Write Unicode directly; other escape forms are unsupported. Literals also include
`true`, `false` and signed 64-bit decimal integers, including `-9223372036854775808`.
Overflow is rejected. No floating-point conversion is used for identifiers.

## Events

```vb
on message:
    if message is "안녕":
        reply "안녕하세요!"
```

Supported headers map directly to existing bot events:

| Script header | Existing bot notification |
|---|---|
| `on message:` | Typed real-time incoming message |
| `on connection:` | Local session state / terminal loss observation |
| `on synchronization:` | Explicit room/checkpoint/gap snapshot |
| `on unknown:` | Preserved unknown or malformed client packet |
| `on lag:` | Explicit lost-packet count notification |

Connection/synchronization notifications are local observations, not newly
discovered server schemas. The source must provide them. No join, leave, kick,
administrator or moderation event is inferred. Unknown packets remain intact in
the bot context; this language exposes no raw packet/secret reflection.

General event handlers execute in registration order before a matching command,
following the bot's FIFO policy. `on message` also sees command text. A `stop`
stops only that script handler, not other handlers or command dispatch. Handler
failures are reported through the normal bot error system and do not replay work.

## Commands, prefixes and aliases

```vb
options:
    prefixes: "/", "!"

command ping:
    aliases: p, 도움말
    trigger:
        reply "Pong!"

command greet <name>:
    prefixes: "!", ">>", "봇아 "
    trigger:
        reply "Hello, %arg-1%!"
```

`options` may appear once before handlers; its only property is `prefixes`.
Defaults apply to that file. Each command needs one nonempty `trigger:` block.
Properties use the same indentation as `trigger` and may appear before or after it.

Prefix precedence:

1. Explicit punctuation prefix in the declaration, such as `command /ping:`.
2. A command's `prefixes` property.
3. That file's `options.prefixes`.
4. Compiler/framework fallback, `!` by default.

Convenient explicit headers include `/ping`, `!help`, `?info`, `.status` and
`>>debug`. Use quoted properties for word prefixes or meaningful whitespace:
`prefixes: "봇아 "` matches `봇아 greet`. An explicit header plus a different
`prefixes` list is a compile error. A redundant identical singleton is accepted.
Comma-separated prefix strings are preserved exactly, without trimming.

Prefix matching belongs to **sculk-bot**, not a script dispatcher. A startup-built
byte trie selects the longest matching prefix globally, then one hash lookup
selects the complete command/alias name. It never falls back to a shorter prefix
because a name was unknown. `!pingextra` cannot invoke `!ping`. Prefix matching is
exact, including case/whitespace. At most 128 unique prefixes of 64 UTF-8 bytes,
16 per command, are registered. Control characters are rejected.

Empty prefixes are refused unless the host sets `Limits.allow_prefixless = true`.
Installation then explicitly opts the consuming bot into prefixless commands.
An empty prefix is the fallback after any longer registered prefix, including the
bot's default. Ordinary text may therefore become an unknown-command notification.

Names and aliases are 1..64 UTF-8 bytes of Unicode alphanumerics, `_` and `-`.
Script declaration/argument names are word tokens: an ASCII digit at the start
is parsed as an integer, so those names must start with another name character. Combining marks are not standalone
name characters; there is no Unicode normalization. Set both `Compiler` and
`Bot::Config` case sensitivity consistently; default is case-sensitive. When false,
Unicode lowercase is used, without locale rules or full Unicode case folding.
Aliases share one handler, arguments and permissions. Colliding `(prefix, name)`
pairs, repeated aliases and ambiguous definitions across loaded files fail during
compilation. Conflicts with preexisting native commands fail during installation.

Room-specific prefix overrides are future work. Prefix validation/matching stays
in the bot's centralized prefix registry, the configuration boundary for a future
room-aware selection policy. Scripts will reuse that policy; no second matcher,
room database or persistent configuration is introduced here.

## Arguments and permissions

`<name>` declares a required positional argument, up to 64. Read it as `arg-1`,
`arg-2`, etc. Missing required values fail before any script effect. References
beyond the declared positions fail at compile time. Extra arguments are accepted
but have no additional named script positions. Names must be distinct.

```text
!greet "John Smith"
!greet '한글 😀'
```

The existing bot parser handles whitespace, both quote styles, concatenated
quoted/unquoted segments and backslash escaping. Its 16 KiB input / 64-argument
bounds apply unchanged. Script source strings themselves use double quotes.

Optional permissions reuse the host registry:

```vb
command private:
    permissions: "commands.private"
    trigger:
        reply "Authorized"
```

The Rust host must register those permission IDs/defaults and grants before
installation. Scripts cannot grant permissions, invent server roles or change
the permission store. The bot checks all requirements before invocation; direct
trusted-host execution also checks context permissions. Event handlers have no
implicit command permission requirement, so a host must review their allowed
effects and outbound adapter. No live sender is installed by default.

## Expressions, variables and control flow

| Expression | Type / availability |
|---|---|
| `message` | Full message string in message/command handlers |
| `sender`, `sender-id` | Exact signed 64-bit sender in message/command handlers |
| `room`, `room-id`, `event-room` | Exact signed 64-bit room in message/command/synchronization handlers |
| `arg-N` | String at a declared command position |
| `"text"`, integer, `true` / `false` | Literal string / integer / boolean |
| `{_name}` | Previously assigned event-local value |

Unavailable contexts fail at compile time where known. A typed non-text incoming
message can still lack text; evaluating `message` then returns a runtime context
error rather than fabricating an empty string.

```vb
on message:
    set {_text} to message
    set {_답변} to "unknown"
    if {_text} is "hello" or {_text} is "hi":
        if sender is 42:
            set {_답변} to "Hello, operator"
        else:
            set {_답변} to "Hello"
    else:
        stop
    reply {_답변}
```

`is` / `is not` compare values of the same type. String equality compares exact
UTF-8 text. `and` / `or` require booleans and short-circuit; precedence is comparison,
then `and`, then `or`. Parentheses group expressions. An `if` condition must be
boolean; `else:` is optional. Nesting is bounded. There are no unrestricted loops.

Locals use `{_name}` with Unicode alphanumerics/underscore after the initial `_`.
Assignment defines a value or replaces an existing value of the same type. Using
a local before assignment, changing its type or reading a branch-local outside
its branch is an error. Assigning an outer local inside a branch updates it.
All slots belong to one invocation and are released afterward; immutable compiled
programs can be shared by concurrent bots without sharing local values.

## Interpolation and effects

```vb
command greet <name>:
    trigger:
        set {_name} to arg-1
        reply "Hello, %{_name}%! Your sender is %sender%."
        send "Received %arg-1%" to event-room
```

`%expression%` placeholders are parsed and type-checked during compilation, then
stored as reusable segments. Strings, integers and booleans can be interpolated.
Use `\%` for a literal percent; empty/unclosed/unknown placeholders are errors.
Runtime arguments and variable text are appended as data and **never reparsed**.
Nested placeholder delimiters are unsupported.

Effects are `reply <string expression>`, `send <string expression> to event-room`,
`set {_local} to <expression>` and `stop`. Both messaging spellings use exactly
`Context::reply` and the host's explicit outbound adapter; there is no arbitrary
target-room send, script-owned connection, live WRITE adapter or automatic retry.
Empty/NUL/oversized outbound text is refused by the existing bot adapter boundary.

## Rust loading and runtime limits

```rust
use sculk_script::{Compiler, Limits};
use sculk_bot::{Bot, Config};

let compiler = Compiler::new(Limits::default());
let program = compiler.load_files(&["scripts/hello.sculk", "scripts/commands.sc"])
    .expect("valid startup scripts");
let bot = Bot::new(Config::default()).unwrap();
let bot = program.install(bot).expect("compatible registrations");
// Configure Outbound before install, then run with a bounded EventSource/cancellation.
```

Compiler errors are a `Vec<Diagnostic>`; applications should print each safe
diagnostic or convert that collection into their own startup error type. See the
example's explicit handling. `compile(&[Source { filename, text }])` avoids file
I/O. `default_prefixes` changes the framework fallback; `case_sensitive` must match
the bot. Files are loaded synchronously at startup with a byte bound before UTF-8
decoding. The host explicitly chooses trusted paths; scripts have no file loader.

The pipeline is lexer → indentation parser → typed AST → semantic checks →
immutable instructions/templates → existing bot registration. Sources/ASTs are
discarded. Instructions contain only forward jumps; no source tokenization occurs
on incoming events. Installation consumes the startup bot, so failure cannot
expose a partially installed runnable bot. It starts no tasks or I/O.

| Configurable `Limits` field | Default | Hard maximum |
|---|---:|---:|
| `file_bytes` (each UTF-8 file) | 65,536 | 1,048,576 |
| `scripts` | 16 | 128 |
| `depth` (blocks and expressions) | 16 | 64 |
| `instructions` (bundle; includes expression nodes) | 4,096 | 16,384 |
| `steps` (per invocation; includes expression evaluation) | 8,192 | 100,000 |
| `string_bytes` | 16,384 | 65,536 |
| `interpolation_bytes` (also capped by `string_bytes`) | 16,384 | 16,384 |
| `locals` (per handler) | 64 | 256 |
| `handler_time` (includes awaiting effects) | 100 ms | 30 s |
| `diagnostics` | 32 | 128 |

Nonzero bounds are required. Larger explicit string limits do not increase the
bot's 16 KiB outbound policy. Bot queue/body/registration limits remain separate;
the compiler also validates their static metadata limits. Hard ceilings are not
recommended settings for a 1 GB deployment. Defaults bound aggregate source to
1 MiB before compilation, aggregate compiled work, and per-invocation local/output
memory. No timer/background polling occurs for an inactive compiled program.

Runtime checks cancellation, steps and elapsed time during evaluation, yields at
instruction boundaries after bounded work, and applies an async deadline around
effects. It owns no tasks, connections or another dispatcher. Bot cancellation
can drop a handler before its own error is observed. Timeout/cancellation can
leave **partial effects or unknown delivery**; no automatic replay or transaction
rollback is claimed. Event loss/dedup/shutdown follow [bot semantics](bot.md).

## Diagnostics and restrictions

Diagnostics include filename, one-based line/Unicode-scalar column, category and
explanation. Independent errors are accumulated up to the configured cap. Default
Display/Debug never includes source literals, message text, account IDs or raw
adapter errors. `Diagnostic::with_excerpt(source)` explicitly opts into showing
only the implicated line, bounded to 256 characters; source can contain private
literals, so callers must decide when this is safe.

`install_with_errors(bot, sink)` optionally receives bounded metadata per runtime
failure; the sink is synchronous trusted host code and must not block or panic.
Normal bot error handling still receives one handler/command failure. Runtime
errors distinguish context/argument/permission failures, step/output limits,
deadline/cancellation and outbound refusal/unknown delivery. There is no silent
retry, effect rollback or exactly-once guarantee.

Unsupported: persistent/global variables, functions, loops, arithmetic, floating
point, user-defined types, hot reload, imports, filesystem/network/environment
access, arbitrary native calls, account tokens, raw client internals, moderation
effects and room-administrator inference. Builtin syntax/type resolution and
execution operations are centralized behind the compiler and bot context, without
a heavyweight VM. Future trusted Rust addons can extend those typed resolution
points and bot registrations; a public custom-syntax registry is not implemented.
The next milestone is a small typed addon registry/execution-cost contract and
function frontend, retaining these resource limits and shared extension semantics.
