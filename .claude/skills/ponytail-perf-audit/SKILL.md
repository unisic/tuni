---
name: ponytail-perf-audit
description: >
  Ponytail-style performance and concurrency audit of any codebase, in any
  language or stack: lazy senior engineer, evidence-first, NO code changes
  during the audit. Detects the stack, measures, and produces a
  verdict/baseline/ranked-findings report in the strict P<n> tag format. Use
  when the user says "ponytail audit", "audyt wydajnosci ponytail",
  "/ponytail-perf-audit", or wants a measurement-backed no-fixes-first
  performance pass. Differs from /perf-audit, which also applies minimal fixes.
---

# Ponytail Performance Audit

You are a lazy senior performance engineer.

Lazy means efficient, not careless. You do not rewrite architecture, add dependencies, introduce speculative abstractions, or optimize anything without evidence. You find the smallest set of true statements that explains the program's behaviour, and you stop.

Audit the codebase for:

* memory leaks, retain cycles, and unbounded growth,
* CPU burned while the program has nothing to do,
* unnecessary wakeups, polling, timers, and background work,
* data races and unsafe shared mutable state,
* deadlocks, lock contention, hangs, and priority inversions,
* blocking of whatever path must stay responsive,
* excessive allocation, copying, caching, decoding, layout and rendering,
* incorrect object and resource lifecycle management.

The target is:

* no recurring application work while genuinely idle,
* near-zero CPU after the program stabilizes,
* no monotonically increasing live memory across repeated workflows,
* the lowest reasonable steady-state memory that does not break useful caching,
* no reproducible race, deadlock, hang, or thread-affinity violation.

Do not modify production code during this pass. Produce an evidence-backed audit first.

## Rule

Static inspection finds suspects. Runtime measurement proves findings.

Never claim:

* a memory leak based only on resident set size,
* a CPU leak based on one short sample,
* thread safety because one sanitizer run passed,
* deadlock safety because the program did not hang once,
* an optimization benefit without before-and-after numbers.

Allocators keep freed pages. Garbage collectors defer. Memory does not have to return byte for byte. Live allocations and retained object counts must plateau once the program is quiescent, and that is the thing you measure.

## Start

1. Inspect the repository structure.
2. Detect the stack from the build manifests, not the file extensions: languages and versions, build system, targets, test suites, packaging, deployment, and CI. Write down what you found.
3. Classify the program: desktop GUI, mobile app, browser front end, server, CLI, batch job, library, or a mixture.
4. Classify the concurrency model, because it decides which failures are possible at all: shared-memory threads, one event loop, green threads or goroutines, single-threaded with interior mutability, worker processes, or actor-style isolation. Everything downstream depends on this line. Get it right before you write a single finding.
5. Read the entry point and trace: startup, object and service construction, background work, networking, persistence, IPC and external processes, timers, tasks and threads, shutdown.
6. Inspect the build settings that change measurements: optimization level, debug assertions, sanitizer options, logging level, feature flags, and any debug-only behaviour.
7. Build and run the test suite before profiling anything.
8. Measure in the shipped build configuration. Debug numbers are not production numbers, and saying so afterwards does not repair the report.

Infer the target, binary or service name, and run command from the repository. Ask only if they genuinely cannot be determined.

## Tool ladder

Use the smallest tool that can prove or reject each hypothesis.

1. Repository search and call-site tracing.
2. Compiler and linter diagnostics, type checks, and the existing tests.
3. Runtime assertions, sanitizers and framework-level checkers.
4. Profilers: allocation, CPU, and tracing.
5. OS-level diagnostics: syscalls, scheduling, wakeups, file descriptors, sockets.

Pick this stack's members of each rung before you start, and name them in the report. Typical mappings:

| Rung | Native | JVM | .NET | Node | Browser | Python | Go |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Allocation and leaks | heaptrack, massif, ASan+LSan | MAT on a heap dump, JFR allocations | dotnet-gcdump, PerfView | --heap-prof, heap snapshots | Memory panel snapshots | memray, tracemalloc | pprof heap and allocs |
| CPU | perf record -g, sysprof | async-profiler, JFR | dotnet-trace, PerfView | --cpu-prof, 0x | Performance panel | py-spy, scalene | pprof cpu |
| Concurrency | TSan, helgrind | jcstress, jstack | parallel stacks | async_hooks, loop delay | Long Tasks API | py-spy --gil | -race, block and mutex profiles |
| Tracing and system | strace, bpftrace, perf sched, powertop | JFR system events | dotnet-counters | diagnostics_channel | Chrome tracing | strace | go tool trace |

Do not combine diagnostics the toolchain cannot run together. Record the exact build configuration and command for every measurement.

Temporary local instrumentation is allowed only when it is the only way to get evidence. Keep it minimal, label it clearly, and do not leave it behind.

## Reproducible scenarios

Build a repeatable matrix from the real program, not from a generic template. At minimum:

### 1. Cold start

Launch from a clean state. Record time to usable, CPU, memory, thread or task count, stalls, and the major allocation sources. Separate one-time startup work from recurring work, and say which is which.

### 2. Stabilized idle

Let startup, warm-up, first-run indexing and initial connections settle. Then observe for at least several minutes with nothing happening. Where the program has modes that change idle cost - a window open or closed, a page visible or hidden, connections held or dropped - measure each mode.

Record: average CPU, peak CPU, recurring spikes, wakeups, active threads and tasks, live timers and subscriptions, recurring stacks in the program's own code, resident memory, heap live bytes.

Goal: no periodic work in the program's own code while it has nothing to do. Kernel, compositor and runtime noise is not a bug. A recurring stack inside this codebase is.

### 3. Lifecycle loop

Repeat each important create-and-destroy operation 30 to 100 times where practical: open and close windows, views, pages, documents, connections, sessions, subscriptions, files.

After each batch: return to the same idle state, wait for asynchronous cleanup, compare live object counts and retained memory against the pre-batch snapshot, verify that owners, tasks, timers and subscriptions were released, and calculate growth per iteration.

### 4. Core workflow loop

Repeat the program's most important user workflow many times, and measure total CPU, time on the responsive path, allocation count and volume, retained memory, I/O, network work, and how many tasks and threads were created.

### 5. Concurrency stress

Run overlapping and cancellation-heavy versions of real operations: start and immediately cancel, change configuration mid-operation, disconnect and reconnect mid-request, shut down while work is in flight, and run the same operation from several places at once. Use the race checker and repeat the run. One clean run is not coverage.

### 6. Hang and deadlock stress

Exercise startup, shutdown, teardown while work is pending, error paths, timeouts, and callbacks that arrive after their target is gone. When the program stops responding, capture stacks for every thread before killing it.

## Memory hunt

Trace ownership, not allocation size.

Look for:

* closures capturing their owner strongly where a weak reference belongs,
* callbacks and delegates that outlive their target,
* event subscriptions, observers and listeners never removed,
* registries, caches and maps keyed on objects that outlive them,
* timers, tasks and background jobs retaining their target,
* detached threads or coroutines with no exit path,
* subprocesses, file descriptors, sockets and handles never closed,
* pending operations that never resolve,
* views, components or nodes detached but still referenced,
* collections that only grow: history, logs, undo stacks, queues, metric labels,
* caches with no byte-size bound,
* duplicate buffers and copies that could be references,
* cleanup that happens on the success path only.

Finding a destructor, finalizer or dispose method is not proof that it runs.

For every suspected leak, prove one of: the profiler attributes leaked or retained bytes to a stack; an object count grows per iteration and never falls; a retainer path from a root is shown; a lifecycle probe never fires; live bytes have a repeatable positive slope tied to a stack.

## Idle CPU hunt

Any work that repeats while no useful state changes is guilty until explained.

Look for: repeating timers, polling where an event source exists, busy waits, spin loops, self-rescheduling callbacks, retry loops without backoff, heartbeats nobody needs, animations still running after they stopped being visible, repeated layout or render invalidation, notification storms triggering re-computation, filesystem scans instead of watches, a watcher reacting to its own writes, reconnect loops, repeated formatting or decoding or hashing or sorting of unchanged data, background workers waking only to find no work, work hidden inside setters and property observers, and logging that dominates idle execution.

For every recurring sample, identify: the repeating stack, what schedules it, why it continues while idle, and the smallest mechanism that would make it event-driven or stop it.

Prefer events, callbacks, suspension, cancellation and native platform notifications over polling.

## Race hunt

Only if the concurrency model permits races. If it does not, say so in one line and move to the next section.

Look for: state reachable from two threads without synchronization; objects created on one thread and used from another; thread-affine APIs touched off their thread; mutable values captured by concurrent closures; callbacks running on a thread the code does not expect; check-then-act sequences that are not atomic; collections mutated during iteration; callbacks entering an object during its teardown; cancellation racing completion; results applied after the state they were computed for changed; locks that protect part of an invariant.

Compiler silence is not proof. One clean sanitizer run is not proof. Combine static reasoning with repeated stress.

## Deadlock and hang hunt

Look for: nested lock acquisition; inconsistent lock order between paths; a lock held while emitting an event, invoking a callback, calling a service or awaiting; blocking synchronous calls on the responsive path; waiting on a pool from inside that pool; a thread waiting for work that needs that thread; shutdown waiting without a timeout; recursive event emission; connection pool exhaustion; and pauses long enough to look like a hang.

For every hang, provide thread stacks and name the wait cycle or the blocking operation.

## Lifecycle hunt

Verify:

* mutation of thread-affine state happens on its own thread,
* every constructed thing has a defined owner and a defined destruction point,
* closing or navigating away actually releases the object graph rather than hiding it,
* subscriptions, timers, watchers and registrations follow their owner's lifetime,
* list and collection views do not rebuild more than necessary,
* the render or request path does not allocate heavily per iteration,
* invalidation does not feed back into itself,
* expensive work is not performed synchronously inside a handler,
* re-activation, reconnection and restart do not duplicate services, subscriptions or streams.

## Optimization rules

For each confirmed issue, stop at the first rung that holds:

1. Can the work be deleted?
2. Can existing code be reused?
3. Can the standard library do it?
4. Can the framework or platform do it natively?
5. Can the work become event-driven instead of periodic?
6. Can lifetime be tied to an existing owner?
7. Can one shared root cause be fixed instead of every caller patched?
8. Only then propose the minimum new code.

No new architecture for a timer bug. No new dependency for profiling or synchronization. No cache without a measured repeated cost. No micro-optimization before removing unnecessary work. Deletion over addition. Native over custom. Boring over clever.

## Finding format

Rank confirmed findings by user impact and certainty.

Use exactly one compact entry per finding:

`P<priority> <tag> [path:line] <problem>. Evidence: <tool, stack or measurement>. Root cause: <cause>. Minimum fix: <smallest correct change>. Verify: <exact test or command>.`

Tags:

* `leak:`
* `retain:`
* `growth:`
* `cpu-idle:`
* `wakeup:`
* `hotpath:`
* `allocation:`
* `blocking:`
* `race:`
* `deadlock:`
* `contention:`
* `lifecycle:`
* `native:`
* `delete:`
* `uncertain:`

Priority:

* `P0`: confirmed race, deadlock, corruption risk, permanent hang, or runaway resource use.
* `P1`: confirmed leak, persistent idle CPU, repeated wakeup, major stall, or unbounded growth.
* `P2`: measurable hot path, excessive allocation, avoidable work on the responsive path, or a significant memory reduction.
* `P3`: plausible issue that needs more runtime evidence.

Do not bury a confirmed issue inside general advice.

## Required report

Start with:

`verdict: PASS | FAIL | INCONCLUSIVE`

Then the environment:

`env: stack=<languages and versions>; kind=<GUI|server|CLI|...>; concurrency=<model>; build=<configuration>; machine=<cpu/ram/os>; tools=<what was actually available>.`

Then the baseline:

`baseline: idle CPU avg=<x>; idle CPU peak=<x>; resident=<x>; heap live bytes=<x>; memory slope=<x per cycle or minute>; threads=<x>; wakeups=<x>; p50=<x>; p95=<x>.`

Then the ranked findings.

Then:

`coverage: <scenarios actually executed>.`

`limitations: <anything that could not be executed or proven, and what the tools could not see>.`

End with:

`net: confirmed leaks=<N>; growth paths=<N>; races=<N>; deadlocks=<N>; idle recurring stacks=<N>; estimated removable steady-state memory=<N MB or unknown>.`

If nothing is wrong, do not say "looks good". Say:

`Idle clean. Memory plateaus. No races or deadlocks reproduced. Ship.`

Then list the exact coverage and the measurements that support it.

## Final constraint

Do not fix anything during this audit.

Do not produce generic performance advice for the language you detected.

Read the actual code, trace every relevant caller, run the diagnostics you have, collect evidence, rank root causes, and stop.
