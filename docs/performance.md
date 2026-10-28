# Performance record

Measured 2026-10-24 on the managed x86-64 Linux development host with Rust 1.99.1,
Tokio 1.54.1, default Cargo release settings. Host reported five logical CPUs
and ~33 GiB host memory; it is **not** the Oracle E2.1.Micro target.
Two short synthetic runs are reported as ranges, not statistical latency distributions.

Reproduce with `cargo bench --workspace --locked`.

| Workload | Measured result | Scope |
|---|---:|---|
| Decode 308-byte BSON frame + clone input | 169.8–170.6 ns/frame | 200,000 iterations; includes validation |
| Encode same frame + clone packet | 35.1–38.4 ns/frame | Reused output allocation |
| Mock request/response, current-thread Tokio | 824,575–881,739 pairs/s | 16 workers, 1,000 requests each, 18.1–19.4 ms |
| Mock request/response, two Tokio workers | 505,130–553,793 pairs/s | Same workload, 28.9–31.7 ms |
| One-shot release basic-client process | 8,448 KiB maximum RSS | Linux wait4; launcher/pre-exec effects possible |
| One-shot release basic-client user/system CPU | 0.002251 / 0.0 seconds | Too short to measure idle utilization |

The mock transport benchmark includes framing, BSON validation, actor queues,
correlation and responses through an in-memory duplex peer. It excludes TCP,
TLS/LOCO crypto, real messages, synchronization, history and user callbacks.
The codec benchmark excludes document serialization on the encoding path;
decoding validates borrowed BSON but does not materialize an owned Document.

The current-thread result favors this workload only. Repeat with warmups,
longer runs, varied payloads, realistic network delays and target CPU quotas
before selecting a deployment runtime. The one-shot RSS value is a process
high-water measurement, not idle RSS or a normal bot measurement. `/usr/bin/time`
was unavailable; wait4 resource accounting was used instead.

## Limits of these results

No Oracle VM test, 24-hour soak, allocation-count profile, idle CPU test,
connection-recovery performance test, standalone event dispatch benchmark,
media workload, or live service load test has been run. No target metric is
claimed achieved. In particular the <50 MiB idle / <100 MiB normal-bot targets
need a functioning authenticated bot and controlled measurement on target hardware.

Defaults retain no message history. Queues/payloads are bounded, but a 64-entry
ring of 1 MiB events plus requests and subscriber-retained values can consume
substantial memory. Consumers control how much data they retain themselves.
Set smaller capacities before deploying on a 1 GiB host.

No LTO or codegen-unit overrides were retained: no comparative experiment
justifies them. Cargo's ordinary release profile remains the baseline. Criterion,
perf, flamegraph and allocation instrumentation should be considered once a
representative authenticated workload is possible, rather than optimizing this
microbenchmark alone.

## Phase 2 follow-up — 2026-10-25

Reran the same `cargo bench --workspace --locked` workloads on the same cloud
host after the cancellation/deadline fixes and session-cancellation signaling.
A single short run measured:

| Workload | Phase 2 sample |
|---|---:|
| Decode 308-byte BSON + input clone | 191.1 ns/frame |
| Encode + packet clone, reused output | 40.8 ns/frame |
| Mock requests, current-thread | 615,930 pairs/s (26.0 ms) |
| Mock requests, two worker threads | 377,096 pairs/s (42.4 ms) |

These samples are slower than the phase-1 short runs. The driver now performs
additional cancellation checks and expires pending entries before admission;
this benchmark also ran alongside other build checks. No controlled comparison
isolates the cost of either change from host scheduling/load variation. No
performance improvement is claimed, and correctness fixes were retained.
Profiling and longer isolated runs are needed before attributing/optimizing the
throughput difference. Codec functionality was unchanged apart from an exact-
maximum-size regression test, illustrating that short-run host variation matters.

The secure-session feature is optional and off by default. Its authentication
lifecycle adds one bounded monitor task per successful session; it does not
introduce per-message tasks or persistent credential/history storage. TLS and
session integration tests exercise real local encryption but are not TLS
performance measurements. New TLS handshake latency, TLS/credential memory,
long-running session RSS and target Oracle VM behavior remain unmeasured.
The earlier one-shot RSS number must not be used as an authenticated-client
memory claim. No release-profile compiler overrides were added.

## Phase 3: packet, record and key-setup measurements — 2026-10-25

Executed `cargo bench --workspace --all-features --locked` on the same managed
cloud host/toolchain, ordinary release profile. Single short samples:

| Path | Size / iterations | Sample |
|---|---|---:|
| BSON packet decode + input clone | 308 B / 200,000 | 210.2 ns/frame |
| Packet encode + clone, reused output | 308 B / 200,000 | 43.1 ns/frame |
| GCM record encrypt + OS nonce + encode | 308 B / 20,000 | 766.4 ns/record |
| GCM record decrypt + input clone | 308 B / 20,000 | 493.2 ns/record |
| GCM record encrypt + OS nonce + encode | 16,384 B / 20,000 | 13,920.9 ns/record |
| GCM record decrypt + input clone | 16,384 B / 20,000 | 13,905.5 ns/record |
| RSA-2048 OAEP/SHA-1 fresh-key setup + OS entropy | 1,000 | 180,687.4 ns/handshake |
| Plain mock request/response, current-thread | 16,000 | 457,241 pairs/s |
| Plain mock request/response, two workers | 16,000 | 307,429 pairs/s |

The encryption benchmark exercises the same record codec and handshake builder
used by the connection pipeline. It is separate from the mock actor benchmark,
which does **not** use TLS or GCM. RSA timings are public-key-only setup using
the documented public-key fixture; no account/server operation occurs. Cipher
benchmarks use synthetic keys/data. Decryption reuses a fixed valid record for
measurement only; this does not implement or claim replay acceptance policy for
a future live session. No unsafe optimization or compiler-profile change was made.

Phase 2 plain mock samples were ~616k / ~377k pairs/s and codec decode was
191.1 ns/frame; phase 1 ranged ~825k–882k / ~505k–554k pairs/s. These short runs
use different feature sets and run amid changing host load and build activity;
they are not a controlled comparison. Phase 3 samples are slower, so no throughput
improvement is claimed. No TLS-vs-GCM latency, deployed RSS, authenticated idle
CPU or long-term stability claim can be drawn from them. A representative
profile and isolated longer measurements are needed before attribution or tuning.

The GCM stream adapter adds no task/channel/bridge and caps outbound plaintext
chunks at 16 KiB. Source responses/packets remain bounded, with two framing layers
and a temporary decrypted record to account for in peak memory. RSA setup runs
once per connection; its sampled ~0.18 ms cost is not an Oracle Micro latency
measurement. No target hardware, live account, aggressive live load or soak test
was used. The source-compatible sequence is validated offline only.

## Phase 4: actual secure stream + actor — 2026-10-25

Ran `cargo bench -p sculk-client --all-features --bench secure_transport --locked`
on the same managed development host with ordinary Cargo release settings.
The workload uses SecureStream/GcmRecords in both directions, the real LOCO
codec and request actor, OS-generated per-record nonces, 16 bounded workers,
1,000 request/response pairs each and a synthetic 256-character payload:

**16,000 encrypted pairs in 76.65 ms: 208,743 pairs/second.**

This includes actual AES-GCM record encrypt/decrypt, record-to-byte adaptation,
BSON framing/validation, bounded admission and correlation, over in-memory duplex
I/O. It excludes network latency, TLS Booking, HTTP authentication and RSA setup
(the latter were measured separately in phase 3). No live traffic was generated.

Phase 3's ~457k current-thread plain mock pairs/s did not encrypt and used a much
smaller packet. It is not a controlled baseline for claiming a regression or
improvement. Phase 3 isolated GCM measurements (~0.77 us/308 B encryption and
~13.9 us/16 KiB) measure individual paths, not the combined actor workload. All
samples are short, with changing host load/features. No performance improvement
or Oracle Micro CPU/memory target is claimed, and no speculative optimization
or release-profile flag was added.

The provider is optional and idle without explicit operations. It adds no idle
polling/refresh loop. Registration polling only occurs after separate approval,
with a bounded server interval, deadline and count. Each body is capped at 64 KiB;
TLS/caller/library buffer copies remain outside comprehensive zeroization claims.
File I/O and terminal prompts run outside Tokio or in spawn_blocking in the
example. The example uses a current-thread runtime. No new deployed RSS,
real-account idle CPU, network latency or long-duration soak measurement was run.

## Phase 5 readiness scope — 2026-10-26

No new benchmark was run: compatibility/security readiness took priority over
unrelated throughput work. The phase-4 encrypted in-memory sample remains
208,743 pairs/s and the earlier limitations still apply; it is not a live-network
or production guarantee. No compiler flags, packet-buffer optimization, protocol
unsafe code or new per-event task was introduced.

Consent input now uses readiness and cancellation instead of a blocking stdin
worker, preventing shutdown from hanging without adding an idle polling loop.
The CLI's state observer is one bounded watch task, and sanitized report writing
happens after the runtime attempt. Primary credential values are released earlier.
Read-only requests fetch one bounded page/snapshot, cap arrays/metadata strings
and retain no automatic message history or unbounded cursor/member cache. Their
performance and deployed RSS are unmeasured; no improvement is claimed.

Supported RustCrypto key/hash-state cleanup features were enabled during the
security audit. Their drop-time overhead is unmeasured; prior phase measurements
are retained as historical samples and are not claimed for the changed build.

## Phase 6 bounded event ring — 2026-10-28

Ran `cargo bench -p sculk-client --all-features --bench events --locked` on the
managed x86-64 host using stable Rust 1.99.1 and ordinary Cargo release settings.
Each case sends 2,048 uniquely decoded synthetic MSG frames through the real
current-thread transport actor to a deliberately stalled subscriber, then uses a
PING response as a completion barrier. Timed work includes BSON framing/validation
and broadcasting; typed decoding is checked during the subsequent untimed drain.
No live traffic, encryption, handler throughput or Oracle VM measurement is included.

| Ring slots | Text bytes | Retained body bytes | Overwritten packets | Packets/s (single short sample) |
|---:|---:|---:|---:|---:|
| 8 | 1,024 | 9,024 | 2,040 | 1,015,583 |
| 16 | 1,024 | 18,048 | 2,032 | 479,219 |
| 64 | 1,024 | 72,192 | 1,984 | 370,708 |
| 256 | 1,024 | 288,768 | 1,792 | 870,945 |
| 8 | 65,536 | 525,120 | 2,040 | 26,041 |
| 16 | 65,536 | 1,050,240 | 2,032 | 25,056 |
| 64 | 65,536 | 4,200,960 | 1,984 | 26,517 |
| 256 | 65,536 | 16,803,840 | 1,792 | 21,158 |

At 64 KiB text, 8 slots retain 0.50 MiB of logical BSON bodies versus 4.01 MiB
for 64 slots, an eightfold payload-budget reduction with an earlier explicit gap.
This counts retained body lengths, **not allocator residency/RSS**: Bytes can share
larger backing buffers, framing/crypto/request queues add memory, and applications
can retain returned packets indefinitely. Tokio rounds a requested capacity up to
a power of two. Near the 1 MiB body limit, 64 retained bodies alone approach
64 MiB before those costs. The typed model shares existing packet bytes rather
than copying content or creating another queue/task; parser scratch allocation
and actual process RSS were not profiled.

These samples ran alongside other verification builds and are short/noisy. They
justify the capacity-versus-retention tradeoff, not a throughput improvement or a
new default. The compatible 64-slot default remains; small hosts should explicitly
budget smaller event/request capacities (for example, 8 slots) and handle Lagged
before relying on continuity. Larger queues cannot substitute for synchronization.
No idle CPU, target 1 GiB RSS, soak, bot stability or live performance claim is made.
