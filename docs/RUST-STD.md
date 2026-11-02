# Rust standard library bring-up

Status: incremental implementation, tracked as [platform workstream P8](CORE-TOOLBOX-ROADMAP.md#p8--rust-standard-library).
The bounded [native File Manager foundation](NATIVE-FILE-IO.md) has finite target
observations. Basic std File/cwd adapters and the deliberate GUI null-stream
profile are under target qualification; real CLAP streams and pipes remain future work.
The goal is ordinary `std` for every native userspace application, including
Finder and all `apps/*`; userspace applications must no longer need `#![no_std]`,
`#![no_main]`, a handwritten `_start` or a panic-handler macro. Toolbox libraries
remain usable without std. Kernel, machine adapters, resource definitions and the
minimal ABI/transport remain `no_std`. The private development SDK and a native
ordinary-main probe now exist. Installed app sources use ordinary `main` and
the image builder selects the std SDK for them. This implementation step does
not close the remaining adoption gates below.

The application policy includes [P11 CLAP commands](TERMINAL.md), hosted by
Terminal with preference-defined base environments and checked session identity.
[P12](HOSTED-TOOLCHAIN.md) extends this to hosted Rust tools; [P13](PROCESS-ISOLATION.md)
adds hardware process protection. Ordinary CLI/std support does not establish
either compiler hosting or memory isolation.

[P15 HFS-B](CORE-TOOLBOX-ROADMAP.md#p15--hfs-b-and-early-system-migration) is the
current boot/system/workspace target. The normal image now requires the pinned
std SDK for application images; System and low-level modules remain no_std.
Continue qualifying actual
formatter/boot/write behavior on HFS-B startup media.
Keep mounted classic HFS behavior unchanged; validate the new format's full-name,
64-bit size/time and dual-fork services instead of extending HFS storage limits.
Audit the existing 4 MiB resource-loading and staged-write ceilings early as part
of that work. A larger filesystem or std-linked binary cannot bypass an unchanged
ROM/resource/app loader limit; verify real packaged images beyond that boundary
with bounded memory use before making std the application build default.

## Current evidence and gaps

The development SDK pins rustc commit
`28221559263a3976766cf305940e80e30cf9ba8a` and its matching rust-src, with custom
target `riscv64-unknown-systemrisc`, PIC and `panic = "abort"`. The reproducible
private-source builder is documented in [platforms/rust](../platforms/rust/README.md).
Kernel and independent low-level modules keep their no_std target. An application
supplies normal `main`; the platform supplies native entry and shared services.

Finite optimized four-CPU QEMU runs of the real packaged `Std Probe` exercised
ordinary main, Vec/String/Box allocation, repeated `System::new()`, native TLS and
its destructor, clocks, a scheduler-backed Condvar timeout, and window create/close.
Matching terminal variants observed normal return with code0, explicit
`std::process::exit(-23)` without user TLS cleanup, the default System panic report,
and a real 512MiB allocation refusal against the 64MiB process account. Panic and
OOM submit Abort immediately after reporting. System owns the report, dismissal,
thread/process reclamation and subsequent input; these were observed in each
applicable case. The panic dispatcher must not skip the System hook merely because
stderr has no panic-output endpoint.

These are bounded main-thread cases, not complete S1 or std adoption acceptance.
Their scope does not qualify early-startup failures, custom hooks, full memory exhaustion, Finder OOM recovery,
Windows or other hardware. Non-ASCII bytes reached the report transport; glyph
availability is a separate font/rendering concern. Existing native scheduler and
module-worker evidence does not substitute for std child-thread observations.

Later finite optimized four-CPU QEMU observations exercised actual std children,
TLS teardown, synchronization, scoped borrowing, transferred join/detach rights,
creation-budget refusal/reuse, child panic and process exit with a live child.
Paused physical PC/SP observations caught each of four real child register bodies
migrating at the same stack pointer, with overlapping execution. Actual thread
and Process/account reclamation and subsequent input were observed. These are
bounded scenarios, including instrumentation and pauses, not race-absence or
latency guarantees.

The native random backend now uses the same checked entropy transport as the
public API. With optimized SDK
`c76fa2ded199ab765b77d4dbc45abb9113e30a8aa00fade3f82ac1364c19ec9e`,
System resource hash
`bb6873aad2b92a75fa85861f46c040080cfdd7b79a5b6892656bbbd6da4e7727`,
and private RV64 QEMU with four CPUs and 8 GiB, HashMap/HashSet operations completed
in main and four native children. Queue metadata recorded six real device
completions; source bytes and seeds were not logged. Removing the RNG produced
an informative System report, Abort101, actual reclamation and subsequent input.
The positive diagnostic explicitly checked source readiness in main. It does
not qualify pending readiness during launch, initial/replacement Finder,
malformed-device handling, provider retirement, source security or full adoption.

The retained ENTR1 launch coordinator now has bounded native observations for
initial Finder, ordinary launch/cancellation/retry and asynchronous document
commit/refusal/retry, plus already-ready Finder-role replacements. Their exact
artifacts, source-freshness correction, injected-device configuration and remaining
gaps are recorded in [application runtime observations](APPLICATION-RUNTIME.md#bounded-ready-observations--2026-11-01).
Matching SDK `056bc07dc1d2272fed1caf2c3a318a0b87ff16eee8a6ed434f6c39bdb04a58a3`
also exercised cross-thread std join/drop with actual THRD/account closure on that
System artifact. Public entropy normal, absent, short completion and caller-stop /
reset / fresh retry ran on the same artifact; no bytes or seeds were logged.
The prior explicit-readiness HashMap cases above remain scoped to their earlier
SDK/System artifact and do not establish later automatic launch admission.

The normal `xtask sdk app` packaging route now adds ENTR1 automatically. An
identical author declaration is reused; an incompatible version or duplicate
author declaration fails with its cause. Generic low-level native packaging does
not infer a runtime from arbitrary ELF files; std builds use the SDK route.

With optimized SDK
`21e9737a2b06f05b90dc4a7ff69f4cb47ee3e32b28e69b113b91aac8cd2193db`
and the System/ROM artifacts recorded in the application-runtime READY observations,
a private ordinary-main application declared no services and made no author
readiness call. Its actual packaged NREQ contained ENTR1. Holding the real source
kept main unpublished while input responded; release led to HashMap/HashSet work
in main and four children, six device completions, five actual THRD reclaims,
original Process/account closure and input. Removing the real RNG refused launch
before main, reclaimed the unstarted owner and allowed typed alert dismissal/input.
Actual SDK builds also observed identical-declaration reuse, informative version
conflict/duplicate refusals, then a restored successful build. These are finite
packaging/launch/seeding cases, not source-security or complete adoption acceptance.

Basic native std File/cwd adapters are connected and under target qualification.
Installed app sources now use ordinary main through the shared std SDK; final
combined-system migration and launch-context qualification remain. The GUI profile
uses intentional null stdin/stdout/stderr as documented below, without a Terminal
or output view. This does not implement real CLAP/session streams or the future
pipes extension. Panic reporting remains independent of these null streams.
Networking remains [P7](NETWORKING.md).

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
- Match the current RV64 LP64 calling convention and RVA23 instruction profile, pointer/atomic widths, stack
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
| HashMap/HashSet and random-dependent runtime behavior | External VirtIO RNG, checked resident entropy transport and bounded native seeding observations | Add an entropy/randomness backend, seed policy, early-boot failure behavior and independent state across apps/threads. Verify the actual pinned std seeding path; do not present timestamps as secure entropy for P7 TLS or third-party getrandom. |
| `thread`, scoped threads, join, sleep, park/unpark | Genuine native children/rights and bounded std thread observations | Spawn native workers in the same AppId with distinct ThreadIds/stacks, join/exit, wake tokens, deadlines, names/IDs, stack exhaustion and cleanup. Scoped threads must not outlive borrowed state; dropping a join handle and app exit require explicit lifecycle rules. |
| `thread_local!` and runtime TLS | Canonical per-thread OS-key TLS and bounded std teardown observations | Per-OS-thread keys/values, initialization/reentrancy, destructor order/reentrancy and teardown. Keep code and thread state alive through destructors. Test distinct values on workers, multiple apps and thread-slot reuse. |
| Mutex/RwLock/Condvar/Once/OnceLock/LazyLock, channels | Interrupt-masked service guards; event-specific wait | General atomic check/enqueue-and-park plus wake/timeout; no lost wakeups, spins behind preempted owners or locks across switches. Specify memory ordering, spurious wakes and fairness. Test blocked-thread progress, initialization races and channels using real workers. |
| `time::{Instant,SystemTime}`, thread sleep | Monotonic ns, signed Unix seconds and checked HFS conversion | Define precision/resolution, range, subtraction/overflow and wall adjustment. Sleep uses scheduler deadlines and never UI event consumption. Test 2026+, before/after epoch, 2040 HFS boundary and adjustment-independent Instant. |
| `io` stdin/stdout/stderr, buffering and printing | GUI null-stream profile; CLAP streams deferred | GUI APPL/FNDR stdin is permanent EOF and stdout/stderr intentionally discard writes with successful flush; all are non-terminal. No output window or serial delivery is implied. Future CLAP/Terminal sessions require real owned endpoints, partial I/O, EOF, locks, redirection and readiness. Full pipe functionality belongs to the planned pipes extension, replacing kernel stubs through the service boundary. |
| `fs`, File/OpenOptions, Read/Write/Seek, metadata/read_dir | Native bounded writable descriptions and path services; basic std File/cwd adapter under qualification | General open/create/read/write/append/truncate/seek/sync, large offsets, cursor sharing on clone, flags and access errors, atomic replacement and directory races. A 4 MiB staging writer is not a writable File. Plain std files address the data fork; resource forks stay an explicit native extension. |
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
   Text Edit, Cursor Test, Panic Test and ResEdit to normal main without
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


### Entropy source deadline observations — 2026-11-01

A private optimized RV64 QEMU image with System resource
`f7fef2e643ddb3af4b47cd1beff66ef2d92c451e07749ee6620bdfbf519683f7`
and ROM `73954623bc3ed093a15ddce337473768263e14dfc748cf5cfda39be07fbf1a09`
exercised the actual 104-byte Host clock descriptor and external RNG deadline.
With SDK `f2d785d5e70a1d71eae401220614a13a19fe3409ff33d0e05179124d242eacc0`,
normal delayed READY led to std collection work in main/four children and real
reclamation. With the source withheld, ordinary and document launch reached the
native deadline, refused with ENTR1/I/O identity, reset the device, reclaimed the
unstarted owner and allowed alert dismissal and genuine retry. Initial Finder
reached its terminal startup screen, retained unique original boot ownership
until actual Restart, then booted a fresh desktop.

A real no_std diagnostic using only the public facade measured 30.0585137 seconds
on the guest clock before refusal, observed zero initialized bytes and an unchanged
64-byte caller buffer, received the provider's explicit 30-second explanation,
then completed actual reset and source-backed retry, THRD/account closure and
input. A single diagnostic guest timer interval permitted reset metadata sampling;
this is not latency or concurrency acceptance. The first public message still
called the generic I/O cause disk I/O; that wording failed presentation review and
the shared Display text was corrected. This historical capture does not establish
the corrected text's native presentation.

Those five captures kept source/base fixed, reaped owned guests and removed cloned
media. Logs include transient pending cleanup/quarantine messages; actual later
PROCESS_EXIT is emitted only after successful canonical removal, settled cleanup
and original account closure. Pending messages alone do not establish either final
failure or completed cleanup. Clock-policy/foreign-context refusal, regression,
overflow, malformed DMA, unacknowledged reset, pending replacement cancellation,
provider withdrawal, Windows and other hardware remain unqualified.


The corrected final assembly used the same System executable
`5caa5a12d7a503ae29a2cf1f254cd2817a59470507bbb79c2578b1e9c21e40b7`,
System resource `eb2e53170ffec45d8ce555b6eaa1ac30b0ac61c89ee84ff346e0e953baa993ea`,
Finder resource `1c31d265d01b21f257d1d98157129f13ed250074425a33dd6e4c76c9a319865a`,
the ROM above, and matching SDK
`55265da35c763f56a6311d1e69b43a2d76f558bd0c11480681b6c35d5506614c`.
The real public diagnostic then observed 30.0429125 guest-clock seconds, unchanged
output, the corrected general I/O cause and explicit source deadline, actual reset,
successful retry and original thread/account closure. The assembled resource set
changed even though the System executable stayed identical; the final image was
rebuilt and the ordinary/document/boot deadline scenarios were repeated on it.

A further final-assembly std case supplied the genuine automatic launch READY
reply, allowed ordinary main to enter, then withheld the actual std seed reply.
The native deadline produced the precise retained System report and Abort101;
the seed reply stayed withheld, no collection workload completed, the device reset,
and report dismissal allowed actual THRD and original account closure and input.
The actual report was inspected visually. This exercises unexpected source loss
after admission, not merely absent-device rejection before author entry. All six
final captures kept source/base fixed, reaped owned guests and removed cloned
media without host cleanup errors. These observations do not enlarge the
unqualified clock/cleanup/provider/platform cases listed above or complete std
adoption.

### GUI standard streams and deferred pipes

The current graphical APPL/FNDR profile deliberately uses null standard streams:
stdin immediately returns zero bytes/EOF without changing the caller's buffer;
stdout and stderr discard arbitrary bytes, report successful writes and flush
successfully. They are non-terminal. The required GUI `main` Result-error policy
is a System-owned error-details dialog, horizontally centered in the upper-middle
of the usable screen, followed by normal cleanup and failure exit status 1.
A cleanup panic must supersede that draft through the separate panic path.
Integration of that policy is pending here; earlier silent-Err observations only
qualify the older policy. Null stderr does not satisfy the error-dialog contract.
Panic reporting remains independent through the System crash service.

These semantics are the graphical application contract, not a real output
stream, interactive input, serial console or CLAP acceptance. A future CLAP
profile must bind actual session endpoints before it is supported. Full pipes
functionality is planned for an external pipes extension that replaces resident
kernel placeholders through the production service/module boundary; do not grow
a parallel full kernel pipe implementation or treat unsupported stubs as pipe
support. Redirection, pipe readiness/backpressure and lifecycle remain deferred.


### Combined Control Suite observation — 2026-11-02

The combined image builds all eight installed applications with the std SDK,
including Control Suite. A finite optimized RV64 QEMU capture with four configured
CPUs, 800x600 display and exactly 2 GiB RAM used System executable
`7a78ca81ca4b8fec659ea2b7eecb2d9e6c2550255f9f9b25e4f004f1fa1e0457`,
System resource `63b073ca855e105ab2575b2154066136d89a0db405d94b67307632503b9b597e`,
ROM `3e53daf4485f326a8662aacc581da565ba588316e56a4378ea07e86d053fc291`
and SDK `f21d209834f9b38c17a9f195fe1102430465eb55822b46fe7c93f9355e35ce7d`.
Offline installed data/resource forks matched each of the eight build packages.
The actual launch admitted Documents for the Control Suite owner before entry.
Physical input exercised its four labeled cells, both splitters' parent-sized
outlines and accepted releases, window resize, normal return with status 0,
actual thread and Process/account removal, and subsequent Finder input. Saved
frames were inspected; source and base media stayed fixed, and the private guest
was reaped with its cloned media removed.

The preceding full sequence and a drag-only partition failed account closure:
the global outline visibility cache retained 264 bytes and two allocation pins
charged to the invoking application. A launch/close-only partition closed its
account. Newly constructed shared visibility snapshots now select ordinary
System backing while preserving actual allocation receipts and cached refusal
behavior. The original full sequence then reached the outcome above. This is
finite evidence for that cache lifetime and GUI sequence, not general pressure,
allocation-failure, cancellation, race or SMP qualification. The kernel still
keeps additional harts parked. Initial/replacement Finder Documents and recovery,
the new Result-error dialog, and final integration with local changes remain
separate adoption gates.
