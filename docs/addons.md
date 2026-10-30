# Typed Rust addons (Phase 11)

`sculk_script::addon` extends the existing compiler/runtime with statically linked
Rust expressions, conditions, async effects and filters over verified bot event
kinds. It creates no second event/command dispatcher, connection owner, native
library loader, background task pool or heavyweight VM.

```sh
cargo run -p sculk-addon-bot --locked
cargo test -p sculk-script --test addons --locked
cargo bench -p sculk-script --bench addons --locked
```

The example registers all three operation types and a local message filter. Its
actual `.sculk` file is:

```vb
on synthetic report:
    set {_score} to reputation of sender
    if trusted score {_score}:
        announce score {_score}
```

`synthetic report` filters existing Message events for the text `profile`. Scores
are synthetic constants, and `trusted score` is local policy, not a server role.
Output is `Bot (mock): Synthetic reputation: 42`. No service is contacted; mock
acceptance is not real acknowledgment/delivery. Sender IDs remain room-scoped.

## Rust API

```rust
use sculk_script::{addon::{AddonError, Capability, Registry, Syntax}, Type, Value};

let mut registry = Registry::new();
registry.expression(
    Syntax::new("reputation of %subject%")
        .parameter("subject", Type::Integer).returns(Type::Integer).cost(10),
    |_, args| {
        let subject = args[0].as_integer().ok_or(AddonError::Type)?;
        Ok(Value::Integer(if subject == 2 { 42 } else { 7 }))
    },
)?;
registry.condition(
    Syntax::new("trusted score %score%")
        .parameter("score", Type::Integer).cost(3),
    |_, args| Ok(args[0].as_integer().ok_or(AddonError::Type)? >= 40),
)?;
registry.effect(
    Syntax::new("announce score %score%")
        .parameter("score", Type::Integer).cost(2).requires(Capability::Reply),
    |ctx, args| async move {
        let score = args[0].as_integer().ok_or(AddonError::Type)?;
        ctx.reply(&format!("Synthetic reputation: {score}")).await?;
        Ok(())
    },
)?;
let compiler = sculk_script::Compiler::default()
    .with_registry(registry).capabilities([Capability::Reply]);
```

Expressions use synchronous `Fn(AddonContext, &[Value]) -> Result<Value, AddonError>`
with an explicit return type. Conditions return `Result<bool, AddonError>` and
implicitly declare Boolean; inconsistent explicit return types fail registration.
Effects accept owned `Vec<Value>` across awaits and return `Result<(), AddonError>`
without a return type. Implementations are `Send + Sync + 'static`; effect futures
are `Send + 'static`. There are no registration macros.

Public primitive Type/Value variants are String (`Arc<str>`), Integer (`i64`) and
Boolean. Typed optional accessors avoid panicking. Parameters are checked during
compilation, and native result types/string lengths again during execution.
Default Value debug formatting prints only its type, never text or identifiers.

`Registry::event(syntax, EventKind, predicate)` adds a literal alias/filter with no
parameters and a synchronous Boolean result. Each matching event pays its declared
cost even if the predicate rejects it. Rejection stops only that handler normally.
Allowed mappings are Message, Connection, Synchronization, Unknown and Lagged;
All is rejected. These remain existing bot observations, not new LOCO schemas.
Unknown/malformed packets keep their original bot representation.

## Pattern grammar and limits

Patterns consist of literal Unicode words and `%named%` slots, separated by
whitespace, with a leading literal. Each slot appears once; parameter names/types
must be declared in matching order. Words use alphanumerics, underscore/hyphen,
at most 64 UTF-8 bytes, and cannot start an ASCII integer token. Literal keywords
are exact/case-sensitive, independently of command case settings.

Slots accept one existing atom (literal, local, builtin expression, zero-argument
custom expression), or a parenthesized full expression. Nested multiword calls
need parentheses in parameters:

```vb
if trusted score (reputation of sender):
    announce score (reputation of sender)
reply "Score=%reputation of sender%"
```

Arguments and interpolation reuse the existing language expression parser/spans;
command arguments/prefixes/aliases still use sculk-bot. There is no runtime source
matching or string replacement. No implementation executes during compilation.

Duplicates and potential prefix/slot overlaps are rejected conservatively across
the registry, ignoring types. `rank of %id%` conflicts with the same shape using
another slot name/type, `rank %a% %b%`, and `rank of %id% extra`. Distinct literal
branches (`score for %id%`, `score from %id%`) work. Builtin initial names and the
operators `is`, `not`, `and`, `or` cannot be overridden. There are no type overloads,
regexes, optional words, alternatives, dynamic patterns or slot-leading patterns.

Limits: 64 entries, 256 pattern bytes, 16 parts, 8 parameters and 8 required
capabilities per entry; positive costs 1..100,000. Events have zero slots.
Registration failure leaves previous entries intact. SyntaxId is registry-local;
descriptor exposes bounded metadata, not an invocation bypass. Order is stable and
resolution does not depend on hash iteration.

Moving Registry into Compiler freezes a snapshot. Parsing resolves syntax once;
semantic analysis checks types and host grants. Instructions retain direct shared
implementations and compiled parameter expressions. Runtime never resolves a
pattern again. Only used implementations survive after compiler/registry drop;
dropping all programs releases those references. Native captured state is ordinary
application-owned memory. Invocation locals/meters/leases remain separate across
concurrent bots. Installation and errors use the existing bot lifecycle.

## Capabilities and trusted native code

**Statically linked Rust addons are trusted code. Capability checks constrain
Sculk's API; they cannot sandbox malicious Rust, syscalls, unsafe code, captured
credentials/clients, arbitrary allocations, panics, blocking work or tasks.**
There is no dynamic library loading. Review native addons as application code.

No addon capabilities are granted by default. Each used registration's required
capabilities must be explicitly granted by the host. Runtime repeats the check;
a callback receives only its own declared requirements, even if more host grants
exist. Grants are bounded to 64 unique capabilities.

| Capability | AddonContext facility |
|---|---|
| ReadMessage | Borrow current bounded message text |
| ReadIdentity | Exact signed 64-bit room/sender IDs |
| Reply | Explicit bot outbound adapter submission |
| Application(PermissionId) | Host-defined application gate |

Application gates reuse validated identifiers but are distinct from per-user/room
command permissions. The bot checks command permissions before native syntax runs.
There is no raw Context, Packet, authentication, stream, filesystem, HTTP endpoint,
environment or internal-session accessor. Parameters can carry data explicitly
provided by scripts; capabilities do not track provenance or redact such inputs.
Native Rust can independently call APIs outside Sculk, hence the trust distinction.

Context clones expire when their callback finishes, fails, is dropped, times out
or is cancelled. Retained clones refuse SDK facilities with Closed. Pending API
submissions are interrupted with unknown delivery. Application-retained contexts
still occupy application-owned memory until dropped; the SDK stores none globally.

## Execution budgets and errors

Every actual custom call pays its declared cost before native execution. Arguments
and normal instructions also consume steps. Repeated occurrences pay repeatedly;
short-circuited calls do not run. Facility attempts and capability queries each
cost another step, sharing the script's atomic meter. Exhaustion/overflow latches:
native code cannot hide it by catching an SDK budget error and returning success.
`ctx.charge(n)` accounts for extra cooperative work; charge(0) checks time and
cancellation without spending steps.

Declared cost does not replace real steps, elapsed-time, string/output limits or
cancellation. Async effects run inside the existing handler deadline, SDK replies
yield cooperatively, and the SDK creates no addon tasks. Intake/network progress,
FIFO ordering and explicit queue-overflow behavior remain those of sculk-bot.

Synchronous callbacks must finish promptly. All native implementations/futures
must cooperate, avoid blocking runtime threads and return AddonError rather than
panic. A native infinite loop/blocking call cannot be preempted by a cost or timer.
Elapsed time is checked again after synchronous code returns; cooperative async
futures are dropped at cancellation/deadline. Native work outside SDK facilities
must be bounded and charged by its author. Native-created tasks are its responsibility.

Returned errors flow through normal bot handler/command failures and optional
safe runtime diagnostics; following handlers/invocations continue. No raw native
error source or payload is formatted. Cancellation after submission may leave
partial effects or unknown delivery. There is no retry, rollback, replay or
exactly-once claim. Mock adapter success remains distinct from actual server evidence.

## Verification and next scope

Tests cover validity/conflicts, typed arguments/results, capabilities, repeated
costs, ignored-budget-error latching, cancellation/timeouts, retained contexts,
isolation, Unicode, both extensions and implementation retention. Fuzz targets
exercise source compilation with registered addons and the registration grammar.
See [verification](verification.md) and [native/builtin/addon measurements](performance.md).

Unsupported: dynamic modules, user-defined types, arbitrary regexes, type-based
overloads, async expressions and a native-process sandbox. Functions, persistent
globals and room-specific prefixes remain outside this phase. The next small
milestone is typed source-level functions with shared budgets and source diagnostics,
reusing this registry/executor.
