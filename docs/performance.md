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

## Phase 7 parsing, state and total RSS — 2026-10-28

Reproduce with `cargo bench -p sculk-client --all-features --bench reliability
--bench events --locked`. Stable Rust 1.99.1, ordinary Cargo release profile,
x86-64 managed Linux host. This environment reports 3 available logical CPUs;
its cgroup limits are 2 CPU equivalents / 8 GiB, **not** the Oracle target's
1/8 OCPU / 1 GiB. No target-hardware or production-bot claim follows.

| Workload | Single sample | Included work |
|---|---:|---|
| Typed 512-byte-text MSG parse | 2,194.5 ns/message | 100,000 iterations; shared packet clone/drop, raw BSON fields and typed validation |
| Duplicate lookup, 128 recent IDs | 11.0 ns/lookup | 100,000 real hash-set lookups; input/receiver black-boxed; not the cursor-equality shortcut |
| State processing acknowledgment | 260.9 ns/log | 10,000 pre-parsed sequential records; bounded set/deque, predecessor validation and MemoryStore CAS; no disk fsync |
| Sync page, 20 × 512-byte text | 33,308.1 ns/page | 10,000 parses, room/range validation, zero-copy views, sort/dedup; no network or application handler |
| Successful retry wrapper | 163.9 ns/attempt | 1,000 immediate successes, policy/cancellation bookkeeping; no handshake or backoff |
| One transient retry with 0..1 ms full jitter | 1,634.9 us/cycle | 100 two-attempt cycles; includes the intentional delay and Tokio timer granularity |
| Local TLS session setup/cleanup | 414.1 us/session | 100 iterations including ephemeral cert generation, real rustls handshake, driver/lease monitor, synthetic grant and manager lifecycle; no HTTP/GETCONF/CHECKIN/LOGINLIST or real RTT |

Event-ring cases each run in a separate child process, send 2,048 synthetic MSG
packets through the real driver, then use a PING barrier before measurement.
Linux `/proc/self/smaps_rollup` supplies total process RSS snapshots; these include
Tokio, the in-memory producer/mock peer, framing and allocator buffers. They are
neither payload-only estimates nor an authenticated SDK-only/idle measurement.
This is a single release-build sample with identical workload shape across cases;
no before/after improvement, peak-RSS or statistical tail-latency claim is made.

| Capacity | Text bytes | Logical retained body bytes | Baseline RSS KiB | RSS at retention KiB | After drain KiB | Packet/s |
|---:|---:|---:|---:|---:|---:|---:|
| 8 | 512 | 4,928 | 1,884 | 2,088 | 2,088 | 1,314,931 |
| 16 | 512 | 9,856 | 1,820 | 2,096 | 2,096 | 1,057,311 |
| 32 | 512 | 19,712 | 1,888 | 2,104 | 2,104 | 1,269,452 |
| 8 | 2,048 | 17,216 | 1,804 | 2,092 | 2,092 | 1,076,302 |
| 16 | 2,048 | 34,432 | 1,884 | 2,184 | 2,184 | 1,081,220 |
| 32 | 2,048 | 68,864 | 1,820 | 2,224 | 2,224 | 1,060,649 |
| 8 | 16,384 | 131,904 | 1,824 | 2,584 | 2,552 | 369,404 |
| 16 | 16,384 | 263,808 | 1,884 | 2,748 | 2,748 | 373,060 |
| 32 | 16,384 | 527,616 | 1,928 | 3,188 | 3,188 | 311,671 |

Stalled receivers reported exact overwritten counts: 2,040/2,032/2,016 for
8/16/32 slots. At 16 KiB text, retained process RSS was about 2.52/2.68/3.11 MiB;
it does not scale solely with body bytes because backing/read buffers and allocator
residency contribute. After-drain RSS need not return to baseline. Shared packet
views and one active history page do not add an unbounded content cache.

These results motivate explicit capacity budgets and lag handling, not a new default.
The compatible 64-slot default remains. Real TLS/GCM/authentication buffers, disk
checkpoint fsync, live traffic, callback costs, long-lived fragmentation, reconnect
storms and multi-room history are outside these samples. The 1 GiB / 1/8 OCPU
hardware target, idle CPU, soak stability and live latency remain unmeasured.

## Phase 8 operational samples — 2026-10-29

Ran `cargo bench -p sculk-client --all-features --bench operational --bench events
--bench reliability --locked`. Stable Rust 1.99.1, ordinary release settings, managed
x86-64 Linux with a 2-CPU / 8-GiB cgroup. This is not the Oracle E2.1.Micro target.
No speedup or production/target-resource guarantee is inferred from these samples.

The new operational cases run in separate processes, use real local rustls over
in-memory streams and a synthetic authorization grant, and retain a local peer.
They perform no HTTP account authentication, GETCONF/CHECKIN/LOGINLIST or real
service traffic. Total process RSS comes from `/proc/self/smaps_rollup`; CPU time
uses the safe rustix CLOCK_PROCESS_CPUTIME_ID wrapper, not a lazily updated scheduler
counter. Measurements include the in-process mock peer, allocation/framing overhead
and the measurement calls themselves. The time feature is dev-only; no new unsafe
runtime code, background poller or per-message task was added.

| Operational case | Observed sample | Scope |
|---|---:|---|
| Idle local TLS session, 5.000 s | 3,764 KiB RSS before/after; 0.137 ms process CPU (0.00275% of one core) | Existing actor/lease monitor, no traffic or automatic heartbeat |
| Five 512-byte-text messages at 1 Hz, 5.009 s | 3,684 → 3,828 KiB RSS; 6.857 ms process CPU (0.13688% of one core) | TLS send/receive, frame validation, typed event parsing; only five samples |
| One 100 × 2-KiB-text SYNCMSG page | 214,217-byte body; 3,688 → 4,432 KiB total RSS | Includes source peer/allocator buffers and 100 zero-copy message views |
| Tracker retaining that active page | 4,432 KiB RSS | Shares response bytes; no additional measured RSS at snapshot granularity |
| After acknowledging/dropping that page | 4,432 KiB RSS | Allocator/buffer residency need not return to baseline |
| Version-2 checkpoint CAS + checksum/fsync/rename | 10.6 us wall and 10.6 us process CPU per commit; 1,992 → 2,000 KiB RSS | Mean of 100 commits after warmup on `/tmp` **tmpfs**, not physical block-storage durability/latency |

The short 5-second idle/traffic windows are preparation measurements, not live idle
usage, a leak test, target CPU scaling evidence or a soak. Disk-backed fsync on the
Oracle volume can be very different from tmpfs. No conclusion about power-loss
behavior follows from the checkpoint timing or process-crash tests.

Repeated bounded-ring samples (2,048 frames, stalled receiver, no TLS in this
particular benchmark) measured total RSS at retention as follows:

| Capacity | 512-byte text RSS KiB | 2-KiB text RSS KiB | 16-KiB text RSS KiB | 16-KiB logical retained bytes |
|---:|---:|---:|---:|---:|
| 8 | 2,140 | 2,092 | 2,524 | 131,904 |
| 16 | 2,032 | 2,184 | 2,748 | 263,808 |
| 32 | 2,044 | 2,156 | 3,148 | 527,616 |

Each case reported exactly 2,040/2,032/2,016 overwritten packets for 8/16/32 slots.
The small-case RSS variation illustrates why payload counts alone and tiny runtime
samples cannot establish a general memory improvement. Capacities/defaults are
unchanged; application-retained pages/events still need their own budget.

The existing reliability benchmark also completed: typed 512-byte-text parse
2,217.8 ns; real 128-ID dedup lookup 11.8 ns; in-memory state acknowledgment 198.6 ns;
20 × 512-byte sync page 36,864.3 ns; immediate retry wrapper 159.1 ns; one transient
retry with configured 0..1-ms jitter 1,545.7 us including timer delay; local TLS
setup/cleanup 416.7 us including certificate generation and synthetic grant.
No HTTP auth, server RTT or live protocol reconnection is included in that last
number. Earlier phase samples remain historical measurements, not controlled
before/after comparisons or claims of improvement.

## Phase 9 bot dispatch — 2026-10-29

Reproduce with `cargo bench -p sculk-bot --bench dispatch --locked`. One release
run on the same managed x86-64 development host (Rust 1.99.1, default release
settings, current-thread Tokio); **not an Oracle 1/8-OCPU/1-GB measurement**.
200,000 operations per case, in process, with no network or outbound adapter.

| Dispatch capacity | One generic event + no-op handler | 512-byte text command + argument access |
|---|---:|---:|
| 8 | 929.3 ns/event | 2758.2 ns/event |
| 16 | 936.3 ns/event | 2768.9 ns/event |
| 32 | 934.7 ns/event | 2757.1 ns/event |

Standalone quoted-argument parsing: **130.1 ns/operation**, using the short
Unicode input in the benchmark. Generic dispatch uses a synthetic Lagged event,
without message parsing. Command dispatch cycles 257 fully consistent, pre-parsed
synthetic packets, exceeding the default 256-ID dedup window and exercising its
bounded eviction. Payloads are prepared outside timing. Timing includes source
futures, queue admission/cooperative scheduling, context, command lookup, parsing,
dedup and a no-op command accessing arguments. It excludes BSON parsing, explicit
permission rules, authentication, I/O, user work, TLS, synchronization and live RTT.
The test checks all 200,000 callbacks complete; overflow/slow-callback behavior is
covered separately by deterministic tests. These are single-run synthetic costs,
not latency percentiles, production throughput, target sizing or an improvement
claim. No additional target-memory or idle-CPU claim is made from this benchmark.

Bot dispatch retains at most capacity + active/intake events, each admitted with
an explicit body-size cap. The source's queue and existing client ring are separate
bounds, as are dedup/registration tables. Application-retained contexts and handler
work must be budgeted separately. No bot background task or idle timer is created.
EOF/cancellation, failure and memory ownership are specified in [bot.md](bot.md).
