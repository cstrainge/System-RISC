# System RISC Rust SDK source integration

This directory pins the application target and the Rust-source changes for the
std port. The image builder compiles installed applications, including Finder,
through this SDK using ordinary Rust `main`. Kernel, ROM, definitions, drivers
and extensions keep their low-level no_std target. The native startup, Thread
Manager, TLS, entropy, file, launch and panic gates in `docs/RUST-STD.md` remain
runtime requirements; compilation does not establish adoption readiness.

## Private build

The initial compiler is nightly-2026-10-05, full rustc commit
`28221559263a3976766cf305940e80e30cf9ba8a`. Install its rust-src component. An
existing local alias can be selected with `SYSTEM7_RUST_TOOLCHAIN`; the full
compiler hash is checked even when an alias is used. Preparation does not change
the default compiler or edit an installed source tree.

```
cargo xtask sdk prepare --build-dir /absolute/private/build
cargo xtask sdk build --build-dir /absolute/private/build
cargo xtask sdk app crates/mac-toolbox/tests/fixtures/std-application/Cargo.toml --build-dir /absolute/private/build
```

Use an appropriate native path on Windows. The existing `SYSTEM7_PYTHON`
selection and xtask's build-directory lease apply. The native xtask executable
also acts as Cargo's rustc proxy, allowing private sysroot discovery without
requiring an executable shell/Python wrapper on Windows.

`prepare` materializes matching rust-src, applies the pinned patch series and
overlays, and records hashes. It exposes the original host-target libraries
read-only through a directory link, or copies them where links are unavailable.
`build` compiles core/alloc/std/panic_abort and the actual mac-traps library for
the new target, then installs the paired target archives and full metadata into
the private sysroot. The pinned compiler separates full `.rmeta` from the metadata
stub in `.rlib`; distributing only the archive is not a usable compiler sysroot.
Sources, target outputs, provenance and failure receipts stay
below the supplied build directory. No guest image is launched or replaced.

`app` checks or builds the matching sysroot, compiles an ordinary application in
the optimized release profile, and uses the existing native resource packager.
Its manifest must declare the application role and APPL file type. The linker
pulls the platform-owned `_start` from std; authors supply ordinary `main`.
`sdk/application.json` distinguishes compilation and packaging from execution.

The target uses the existing LP64 ABI and RVA23 ISA, PIC code, panic-abort and
OS-key TLS. It advertises neither Unix/Windows APIs nor compiler-native TLS.
Host build tools and low-level no_std guest roles keep their existing targets.

The std dependency graph uses private, distinctly named copies of the core-only
ABI and trap transport, built against the same std-workspace core. Those sources
are copied from the current checkout and included in the SDK identity; there is
no second resident implementation. Distinct package names avoid collision with
an application's Cargo dependencies. Only C-layout wire values cross the private
CRT interface, not Rust types from different crate instances.

## Backend status

The source port selects the existing real application-heap transport and std's
atomic/futex synchronization implementations. Native child threads and entropy
use the shared minimal trap transport; OS-key TLS and termination retain their
platform CRT boundaries. Startup installs the validated
service table once inside std before entering the compiler's main wrapper. The
Toolbox reads that same installation through a C-layout getter. Canonical current
thread and OS-key TLS bind the published native traps; std and no_std startup
share the real teardown protocol in mac-traps. Atomic wait/wake now bind the
published scheduler compare-and-park transport; only timeout returns false,
while typed refusals retain their resident diagnostic. Signed process exit binds
the native termination service. Main return runs current-thread TLS teardown;
explicit process exit skips it. Child creation/join/detach uses genuine native
thread rights. The random backend fills through checked native entropy calls and
reports a source failure through System's failure service before Abort; it never
substitutes a seed. ENTR1 launch readiness retains the genuine prospective
Process and provider job before author entry. Bounded native observations cover
initial/ordinary/document launch, cancellation and injected-device refusal/retry;
pending replacement and provider-withdrawal cases remain unqualified. The
`sdk app` packager automatically merges ENTR1 into the actual NREQ through
mac-build's normal manifest compilation; authors need no readiness call or
manifest entry. Identical declarations are reused, conflicting versions refused.
Low-level native packaging does not infer std from arbitrary ELF input. Writable
files and launch data remain adoption gates; GUI standard streams deliberately
use the null profile and real CLAP streams/pipes are deferred, as detailed in
`docs/RUST-STD.md`.
There is no no_threads implementation, static TLS, zero-seed generator or host
allocator substituted to get an executable to link.

The patch replaces std's actual default panic hook for this target, including the
default restored by `take_hook()`. It sends borrowed UTF-8 message/location spans
through the CRT panic bridge for the existing System crash service. The hook adds
no allocation, stdio lock, thread-name/TLS lookup or backtrace lock. This does not
remove TLS/synchronization used earlier by std's panic machinery. Custom hooks
remain upstream behavior; explicit abort and abort after a custom hook use a
separate terminal bridge and do not call the default hook again. The CRT report
bridge uses the existing CRASH begin/append/show service, then immediately asks
System to Abort. System owns report retention and dismissal; the failed author
never polls the viewer. Early failure uses the shared core-only emergency Abort
transport. Native std reporting and concurrent process-failure ownership require
their own matching-artifact evidence; compiling
this hook does not establish a functioning bomb dialog.

Ordinary main may return `mac_toolbox::ToolboxResult<()>` or another Rust
`Termination` result. For `Result::Err`, the platform streams the error's `Debug`
details to a System-owned draft while the value is alive. After normal std cleanup
and main-thread TLS destruction, the CRT publishes the error report and exits
normally with failure status 1. This is separate from panic/Abort101. A cleanup
panic supersedes the draft through the default panic path. The error window uses
the actual WDEF structure and usable desktop bounds to center horizontally and
place one third of the spare vertical space above it. Reporting refusal retains
its typed diagnostic and never changes an ordinary Err into a panic.
These new main-error paths still require matching-artifact native qualification.

The actual default allocation-error hook also uses a dedicated CRT report bridge,
preserving the refused size/alignment and `NO_MEMORY` cause. It does not invoke
a user panic hook or format through allocating stdio/backtrace code. Fallible
allocation still returns refusal normally. Custom allocation-error hooks retain
their upstream semantics. Native OOM reporting remains a separate runtime gate.

Instant and SystemTime now bind the published native clocks, with checked signed
Unix-epoch arithmetic and nanosecond calculations. Their std execution remains
subject to matching-artifact observations. Basic native std File/cwd adapters
have bounded matching-artifact observations recorded in docs/NATIVE-FILE-IO.md;
that does not qualify the complete filesystem. GUI default cwd is the user's
Documents directory. The current_exe adapter traverses a scoped share of genuine
executable protection, independently of cwd. The GUI environment starts empty
and supports synchronized process-local UTF-8 snapshots/mutations; no host or
Terminal environment is imported. GUI arguments remain the actual empty argc0
contract. These new executable/environment adapters require separate matching
native qualification; CLAP/Command inheritance remains deferred.
GUI stdin deliberately returns EOF, while stdout/stderr discard bytes and
flush successfully; all are non-terminal. These graphical semantics do not
implement real CLAP session streams. Full pipes belong to the planned pipes
extension replacing kernel stubs through the production service boundary.
Other upstream unsupported backends remain explicitly unsupported. Neither linking std nor resolving one of its
private symbols qualifies a service.

`mac_toolbox::System::new()` now acquires the table installed by the current
application runtime without allocation or a new authority selector. The existing
native heap APPL fixture checks repeated acquisition on its real startup path;
native observations must be recorded separately from compiling that fixture.
The ordinary-main Std Probe fixture adds Vec/String/Box, repeated System
acquisition, window calls and a real thread-local destructor. It requires a
matching native execution record before any of those std paths are accepted.

The pinned compiler emits its ordinary main wrapper as `i32 main(i32, pointer)`
on this target. The CRT must preserve that C entry signature; the internal
`lang_start` signature is not the wrapper ABI. Inspect it again when changing
compiler/target revisions. Emitting LLVM IR establishes compiler output only,
not successful linking, startup or execution of an application.

Native thread creation, TLS and compare-and-park/wake are implemented through the
shared transport described above. Their acceptance still requires matching
System/SDK observations of child completion, destruction before join, address
lifetimes and process retirement; earlier artifacts do not qualify later builds.
The image builder and `sdk app` share the std compiler and automatic ENTR1
packager. Application role accepts ordinary `APPL` and shell `FNDR` packages;
Finder retains its original file type. `sdk app PATH --features feature1,feature2`
selects Cargo application features. Embedded low-level definitions are resolved
from this build root's explicit low-level target/release directory during both
build-script input tracking and packaging. Each packaged application retains
its own `.sdk.json` receipt with the selected SDK, source inputs and features.
Writable files, launch data, streams and complete migrated app workflows remain
separate adoption gates.


For this pinned Cargo, private native rebuilds select both
`CARGO_UNSTABLE_CHECKSUM_FRESHNESS=true` and `CARGO_BUILD_FINGERPRINT=content`.
Inspect actual generated Cargo fingerprint records for `content`; the feature
flag alone does not change the default mtime mode. Build-script/resource inputs
still require explicit rebuilds when changed. Record final packaged artifact
hashes before native capture; compilation success is not runtime acceptance.


Entropy device work has a 30-second deadline using the authenticated resident
module clock and admitted machine timebase. Timeout or clock refusal enters the
same real reset/unmap/release path; only positive cleanup acknowledgement permits
a terminal reply or reuse. An unacknowledged reset retains the original work,
DMA mapping and cleanup ticket. This is not forced recovery from arbitrary device
faults. Public fallible fills preserve their actual initialized prefix and cause;
std's infallible seeding reports genuine source loss through System then aborts.
