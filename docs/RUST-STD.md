# Rust standard library bring-up

Status: design/backlog, tracked as [platform workstream P8](CORE-TOOLBOX-ROADMAP.md#p8--rust-standard-library).
The goal is ordinary `std` for every native userspace application, including
Finder and all `apps/*`; userspace applications must no longer need `#![no_std]`,
`#![no_main]`, a handwritten `_start` or a panic-handler macro. Toolbox libraries
remain usable without std. Kernel, machine adapters, resource definitions and the
minimal ABI/transport remain `no_std`. This plan does not install a new toolchain
or implement a std backend.

The application policy includes [P11 CLAP commands](TERMINAL.md), hosted by
Terminal with preference-defined base environments and checked session identity.
[P12](HOSTED-TOOLCHAIN.md) extends this to hosted Rust tools; [P13](PROCESS-ISOLATION.md)
adds hardware process protection. Ordinary CLI/std support does not establish
either compiler hosting or memory isolation.

[P15 HFS-B](CORE-TOOLBOX-ROADMAP.md#p15--hfs-b-and-early-system-migration) is the
immediate implementation priority, before the P8 application/sysroot migration.
Use the current no_std apps to establish its boot/launch gate; do not make P15
depend on std support. HFS-B is the boot/system/workspace target. After its
formatter/boot/write-integrity gate, normal builds require HFS-B startup media.
Keep mounted classic HFS behavior unchanged; validate the new format's full-name,
64-bit size/time and dual-fork services instead of extending HFS storage limits.
Audit the existing 4 MiB resource-loading and staged-write ceilings early as part
of that work. A larger filesystem or std-linked binary cannot bypass an unchanged
ROM/resource/app loader limit; verify real packaged images beyond that boundary
with bounded memory use before making std the application build default.

## Current evidence and gaps

At this planning checkpoint the host has rustc nightly 1.101.0, commit
`28221559263a3976766cf305940e80e30cf9ba8a`, and rust-src installed. This is an
observed toolchain, not a selected release requirement. Guest builds explicitly
require `riscv64imac-unknown-none-elf`; they use `_start(SystemServices)`, compact
native resource packaging and `panic = "abort"`. The linker drops `.eh_frame`.

The one-hart scheduler supplies separate application threads, preemption, trusted
identities and event-specific blocking/wakeup. This is not worker-thread spawning,
general park/wake, TLS or a std runtime. Memory handles are generation checked
but do not expose a stable-pointer allocator. File reads/catalog operations and
bounded transactional data-file creation exist; generic writable file streams,
pipes and std descriptors do not. Monotonic nanoseconds and signed Unix wall
seconds exist, with finer wall-clock precision still to define. Networking is
[P7](NETWORKING.md), not an existing socket backend.

## Application build and default panic contract

S0/S1 build/runtime support is a prerequisite for the userspace migration and an
early delivery priority after the agreed P15 storage, P14/P16/P17 driver/mount
and P18 host-clipboard validation sequence. Those initial no_std kernel modules
do not depend on the app std port. The default application
build must select the std-capable System RISC target/sysroot and supply startup,
allocation and panic integration.
The build must fail clearly if the required platform sysroot is absent; it must
not silently select bare-metal or a stub implementation. System, boot ROM and
definition/extension modules retain explicit no_std build roles.

Keep `resources.json`, Cargo-derived versions, icons, code resources and RLOC as
the normal packaging flow. Applications use normal `fn main()` (including the
standard Termination contract), and safe access to the loader-initialized Toolbox
client. The generated/platform startup adapter owns service-table initialization
and the existing native loader entry; this ABI plumbing is not app boilerplate.
Host tools/build scripts remain host std programs.

The platform's **default std panic hook** calls the resident crash-report service,
which owns the classic bomb dialog, complete retained report, scrolling and event
handling. No per-app macro, initialization call or `set_hook` is required. This
must be the actual platform default, not merely a startup-installed custom hook:
standard [take_hook behavior](https://doc.rust-lang.org/std/panic/fn.take_hook.html)
must restore the same dialog-producing default. Retain normal explicit custom
hook behavior, including taking/calling the previous hook.

Forward available panic message/payload and source location, app identity and
actual thread identity/name; include available backtrace information with its
coverage stated. Preserve the current long Unicode report and explicit
truncation/failure markers. Stream through the minimal ABI without depending on
mac-toolbox, stdio/terminal initialization or application repaint/event callbacks.
System retains and displays the report even after the failed app is stopped.
Initialize the bridge before app/runtime code can panic; specify recursive,
simultaneous-worker, early-startup and allocation-failure reporting paths without
requiring healthy heap/TLS/UI state in the dying app. If reporting cannot enter
the service, use a bounded emergency diagnostic/termination path rather than
recursive panics or silent success.

With the initial panic-abort profile, the default path reports then terminates
the failing application and its workers, preserving System's dialog and other
apps. This is not unwinding or kernel-panic recovery. Remove the existing custom
`#[panic_handler]` macros from apps only as their std build lands; do not strip
them from today's working no_std builds before the platform replacement exists.

## Platform boundary

```text
Rust std platform backends ──> minimal native ABI/transport ──> System services
mac-toolbox bindings       ──> minimal native ABI/transport ──> same services
```

Keep the sysroot's dependency graph acyclic: it cannot need the application-level
`mac-toolbox` crate to build itself. Reuse the low-level request definitions and
transport through a core-only substrate, auditing the existing mac-abi/mac-traps
split before extracting or sharing code. Both paths validate trusted AppId and
actual ThreadId, generation-tagged object IDs and owner checks in resident state.
Classic trap/string/record adapters remain separate.

The std backend translates platform contracts and errors; it does not implement
another filesystem, scheduler, socket stack or GUI. Ordinary std library code
may link into apps while shared OS implementations remain resident. Resource-
based packaging does not require shared Rust dylibs or a stable Rust binary ABI.

## Target, compiler and build work

- Add a distinct System RISC target profile rather than impersonating Linux or
  assuming the bare-metal target supplies a working std. Pin rustc, Cargo,
  rust-src, compiler-builtins and the maintained Rust-source patch together.
  Audit rustc target/cfg selection as well as std backend dispatch; a target JSON
  alone is insufficient. Follow the Rust project's
  [custom-target guidance](https://doc.rust-lang.org/rustc/targets/custom.html).
- Match RV64IMAC/soft-float calling conventions, pointer/atomic widths, stack
  alignment, relocation/code model and linker entry. Inventory required float/
  math and memory builtins without introducing an accidental libc dependency.
  Host build scripts and proc macros still execute on the host target.
- Maintain a reproducible patched sysroot outside installed global toolchains
  and outside generated artifacts committed to the repo. Record source/patch
  hashes and provide one xtask build/check route. Cargo's
  [build-std workflow](https://doc.rust-lang.org/cargo/reference/unstable.html#build-std)
  is a candidate build mechanism; rebuilding unpatched upstream sources cannot
  create missing OS implementations. Validate required flags/features against
  the pinned compiler. No global rustup/default-target change is needed.
- The inspected Rust source distributes backends across `std/src/sys` modules
  (allocation, args, files, I/O, random, synchronization, threads, TLS, clocks,
  networking and process support), not just `sys/pal`. Inventory every selection
  point for the pinned source and retain explicit unsupported implementations
  until each service's runtime gate passes.
- Update mac-build's hardcoded target check and xtask artifact lookup to use
  explicit role/target profiles. Continue emitting version-1 compact code
  resources plus relocations; keep ELF, symbols and build/debug metadata on the
  host. Validate emitted relocations and code size with a real std app.
- Supply a startup adapter from the loader's service-table entry to Rust's
  normal main/runtime initialization and exit. Initialize services, allocation,
  app/thread context, arguments, environment and streams before Rust runtime
  code uses them. Do not call a Rust main symbol through a guessed C signature.
  Make initialization/allocation/panic paths independent of partially initialized
  TLS and lazy std state.
- Support compiler-generated TLS only after loader/thread machinery handles its
  templates, relocations, alignment and destruction. A first OS-key-based TLS
  implementation may avoid native TLS code generation; target cfg and std's
  chosen TLS backend must agree. Current app relocation is not TLS support.

## Module and service inventory

The table groups the public [standard library](https://doc.rust-lang.org/std/)
by OS dependency. For the pinned compiler maintain a per-module support matrix
with compile, runtime, failure, concurrency and unsupported states. Do not label
a linked std stub as an implemented service.

| std surface | Existing foundation | Required work / acceptance |
| --- | --- | --- |
| `alloc`, Box/Vec/String, collections, Arc | Internal System heap, atomics and memory handles | Stable-pointer allocate/deallocate/reallocate with alignment, zeroing, ownership and OOM policy. Never use movable handles as raw allocations. Match GlobalAlloc contracts, including no unwinding/reentrant allocation; test alignment, growth, reuse, failure and concurrent clients. |
| Iterators, formatting, Unicode/string logic, numerics, generic `io` helpers | Mostly portable Rust/compiler code | Audit target intrinsics/math/builtins and dependency paths; verify representative runtime results. Their presence does not imply the OS-backed portions of std work. |
| HashMap/HashSet and random-dependent runtime behavior | No verified OS entropy service | Add an entropy/randomness backend, seed policy, early-boot failure behavior and independent state across apps/threads. Verify the actual pinned std seeding path; do not present timestamps as secure entropy for P7 TLS or third-party getrandom. |
| `thread`, scoped threads, join, sleep, park/unpark | One main OS thread per app; event wait blocks it | Spawn native workers in the same AppId with distinct ThreadIds/stacks, join/exit, wake tokens, deadlines, names/IDs, stack exhaustion and cleanup. Scoped threads must not outlive borrowed state; dropping a join handle and app exit require explicit lifecycle rules. |
| `thread_local!` and runtime TLS | App-local manager context, not native thread TLS | Per-OS-thread keys/values, initialization/reentrancy, destructor order/reentrancy and teardown. Keep code and thread state alive through destructors. Test distinct values on workers, multiple apps and thread-slot reuse. |
| Mutex/RwLock/Condvar/Once/OnceLock/LazyLock, channels | Interrupt-masked service guards; event-specific wait | General atomic check/enqueue-and-park plus wake/timeout; no lost wakeups, spins behind preempted owners or locks across switches. Specify memory ordering, spurious wakes and fairness. Test blocked-thread progress, initialization races and channels using real workers. |
| `time::{Instant,SystemTime}`, thread sleep | Monotonic ns, signed Unix seconds and checked HFS conversion | Define precision/resolution, range, subtraction/overflow and wall adjustment. Sleep uses scheduler deadlines and never UI event consumption. Test 2026+, before/after epoch, 2040 HFS boundary and adjustment-independent Instant. |
| `io` stdin/stdout/stderr, buffering and printing | Serial diagnostics and UI services | Owned stream/descriptor table, partial reads/writes, EOF, locks, redirection and pipe readiness. Define absent stdin and broken-pipe behavior. GUI apps need no terminal until actual output requests one; no serial log stream masquerading as user stdout. |
| `fs`, File/OpenOptions, Read/Write/Seek, metadata/read_dir | Read-only fork streams, catalog/identity, mutations, bounded whole-data-file writer | General open/create/read/write/append/truncate/seek/sync, large offsets, cursor sharing on clone, flags and access errors, atomic replacement and directory races. A 4 MiB staging writer is not a writable File. Plain std files address the data fork; resource forks stay an explicit native extension. |
| `path`, OsStr/OsString, environment/cwd/temp/exe paths | Native UTF-8 strings over classic HFS names | Define native path/volume syntax, encoding and invalid-input behavior, HFS case/name limitations, normalization and unsupported Unicode names without lossy substitution. Snapshot args/env and synchronize allowed mutations. Preferences remain JSON, separate from the process environment. |
| `process::{Command,Child,Stdio}`, exit/abort | Resource-app launch/activation/exit | Child arguments/environment/cwd, process ownership, exit status, wait/try_wait, kill and redirected pipes. Launch resource-fork apps through the loader; no need to implement Unix fork to provide Command. Do not treat activate-existing-app as spawn-a-child. |
| `net`, address resolution and socket readiness | No device/socket service | P7 sockets, owned descriptors, connect/accept, UDP, names, options, partial I/O, EOF, deadlines and cancellation, then std backend mapping. Verify IPv4/IPv6 and process cleanup without a second TCP/IP stack. |
| `panic`, `error`, abort and backtrace | System-owned app crash service; no_std panic macro | std owns the language panic handler: std apps must not also install `mac_toolbox::panic_handler!`. Forward hook/runtime details through the low-level crash API and terminate the correct app; define double-panic/OOM fallback. Backtrace capture/symbolication needs retained host symbols and a separate frame/unwind design. |
| `ffi`, `os`, raw/borrowed handles | Versioned C-layout service boundary | Select platform OsStr representation, descriptor borrowing/duplication and owned-handle conversions with generation/owner validation. Define a System RISC extension surface; do not advertise Unix/Windows APIs by setting an inaccurate target cfg. |
| `future`, async I/O ecosystem, external crates | Core futures can be polled by app code | std supplies no general async runtime. Build readiness/worker/timer hooks, then independently qualify executors and crates with platform assumptions. Add explicit getrandom/socket/raw-handle/FFI portability work where required. |
| Test harness and doctests | Host Rust tests plus custom SDK/QEMU guests | Start with a deterministic guest runner. Port the pinned libtest backend/options separately; handle aborting cases in separate processes/VMs. A host cargo test result does not execute the std target. |

## Panic, exit and ownership rules

CLAP startup requires a validated Terminal session even for redirected child
streams. Environment defaults originate in Terminal's JSON preferences; process
and child snapshots/overrides follow P11, including env_clear without losing the
out-of-band session. Distinguish APPL activation from creation of a new CLAP child
in Command; do not bypass terminal-only policy merely because stdout is a pipe.

First use `panic_abort`. Normal returns and ordinary scope exits still run Rust
Drop; `process::exit` and abort have different cleanup contracts and cannot
promise all destructors. Kernel owner cleanup must cancel/stop app threads and
in-flight work before reclaiming stacks, TLS or callback/code memory. Define
orderly main return, detached workers, forced termination and join semantics.

In this profile a worker panic terminates the application; it does not become a
recoverable `JoinHandle::join` panic result. Likewise
[catch_unwind](https://doc.rust-lang.org/std/panic/fn.catch_unwind.html) cannot
recover an aborting panic. Unwinding is a separate later gate: retain the needed
metadata/personality/runtime support, define poisoning/join results and audit
cleanup. Never unwind across ECALL, C resource entries or firmware callbacks.
Kernel extensions keep the separate fatal policy in P6; std app panic hooks
cannot turn kernel panic into recoverable app failure.

## Delivery sequence

1. **S0 — Toolchain and ABI:** pin sources/patches, target and backend inventory;
   build a minimal patched std sysroot and resource-packaged normal-main app.
   Report unsupported services explicitly; keep existing no_std builds passing.
2. **S1 — Allocating graphical app:** allocator, startup/exit, basic streams,
   clock and System-owned panic integration. Exercise Box/Vec/String, formatting,
   output, timing and a Toolbox window using the same std binary. Add seeded
   collections after the pinned runtime's randomness gate is established.
   Make std the application-role default and migrate Finder, QuickDraw Test,
   TextEdit Exerciser, Cursor Test, Panic Test and ResEdit to normal main without
   no_std/no_main/custom entry/panic macros. Their existing Toolbox workflows
   need not wait for std filesystem or networking support. Apply the same rule
   to future standalone native applications, including P6 control panels.
3. **S2 — Real concurrency:** worker spawn/join, TLS/destructors, park/wake,
   synchronization/channels and sleeping. Other apps and the UI progress while
   workers block or compute without traps. Test one hart first; P1 SMP is not a
   prerequisite for std threads but adds independent parallel-execution gates.
4. **S3 — Documents and child processes:** general files, native paths, args/env,
   streams/pipes, process status/wait and Command. A std app creates/edits/reopens
   a document, launches a separate copy with arguments and captures its output.
   Test failures and reboot persistence; adopt broader std file/process features
   in the already std-based Finder in bounded slices.
5. **S4 — Networked std apps:** P7 backend, resolution and local TCP/UDP/IPv6
   tests, then TLS/HTTPS with its real prerequisites. No public Internet needed
   for acceptance. Wider executor/getrandom ecosystem qualification follows.
6. **S5 — Coverage and maintenance:** expand selected upstream std tests/libtest,
   cross-app contention, OOM/I/O fault tests and compiler-upgrade checks. Track
   unsupported platform-specific operations, backtraces and optional unwinding
   as explicit remaining capabilities rather than declaring all std complete.

S0 and resident service design can progress together. S1 need not wait for
networking, SMP or general legacy translation. S3 filesystem work and P7 can
progress alongside S2, but blocking std APIs require S2's wakeup foundations.
P6 preferences/control panels can share the same services without requiring std
inside the kernel or extensions.

## Verification and completion

Keep canonical resident-service conformance in `crates/mac-toolbox/tests` through
the public SDK. Add a std-specific guest suite exercising ordinary std calls
through the new backend and real services; kernel-only/toolchain tests supplement
both. Use controlled fault injection and private QEMU guests for allocator
alignment/OOM, lost-wakeup races, TLS/thread reuse, scope/join lifetimes, stale/
foreign descriptors, partial I/O/EOF, cancellation/exit cleanup, clock changes,
file persistence, child output, socket failures and panic reports. Trace actual
ThreadIds and residency without per-event production logging.

Publish per-module compile/runtime/error/concurrency status and toolchain hashes
with each milestone. A Finder build using std plus graphical document workflow
is the application gate, not proof of process/network/unwind completeness.
Require a new ordinary-main sample that panics without registering a hook, plus
Panic Test's short/long Unicode cases through the platform default. Verify hook
replacement and take_hook restoration, worker panic, early/failed reporting and
that the System dialog remains interactive while another app makes progress.
The userspace migration gate requires all six current applications to build,
launch and retain their existing graphical/ownership regressions through the
default application build. Compiling one opt-in std sample does not meet it.
Measure image/RAM growth, idle wakeups, syscall and lock durations, and Cursor
Test/input/present responsiveness under worker, filesystem and network load.
No std adapter should poll queues, redraw windows or spin in a blocked call when
a readiness wake or no-change early-out suffices.

## Async SDK and runtime integration

The native SDK must support ordinary async application entry points and
awaitable services. Its built-in event-driven executor is std-compatible, with
allocation and owned futures in the normal configuration. Optional Tokio support
uses the same completion notifications; it does not replace resident services or
become a kernel dependency. Include event/operation readiness, lost-wake-free
registration, parking, timers, cancellation and UI thread affinity in the std
platform work. See [ASYNC-SDK.md](ASYNC-SDK.md) for acceptance and actual status.

P8 starts at the minimum platform/Toolbox foundation, not after complete Toolbox
coverage. Full std is the normal userspace destination, not an optional parallel
application tier. Keep current no_std apps as bring-up gates only; do not let
allocator-free APIs or temporary event-pump plumbing become permanent application
requirements. Broader Toolbox managers and networking can develop alongside std.

Priority clarification, 2026-10-21: generic IPC and networking are optional later
work, not prerequisites for the first std/async applications. Prioritize
allocation/startup, files, clocks, OS threads/TLS/synchronization, event wakeups and
System-owned panic reporting. Compile the standard library's surface while
reporting unavailable facilities honestly; do not invent working sockets or
require a generic IPC subsystem merely to deliver resident completions.
