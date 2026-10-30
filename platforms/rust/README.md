# System RISC Rust SDK source integration

This directory pins the application target and the Rust-source changes for the
std port. The existing normal image still uses the published no_std application
runtime. Switching installed applications requires the native startup, Thread
Manager, TLS, entropy, file and panic milestones in `docs/RUST-STD.md`; a compiled
standard-library archive does not establish those properties.

## Private build

The initial compiler is nightly-2026-10-05, full rustc commit
`28221559263a3976766cf305940e80e30cf9ba8a`. Install its rust-src component. An
existing local alias can be selected with `SYSTEM7_RUST_TOOLCHAIN`; the full
compiler hash is checked even when an alias is used. Preparation does not change
the default compiler or edit an installed source tree.

```
cargo xtask sdk prepare --build-dir /absolute/private/build
cargo xtask sdk build --build-dir /absolute/private/build
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
atomic/futex synchronization implementations. It forwards thread creation, OS-key
TLS, waits, entropy and exit through private CRT symbols declared in
`library/std/src/sys/pal/systemrisc/runtime.rs`. The corresponding native service
adapters are an integration dependency: missing symbols must fail final linkage.
There is no no_threads implementation, static TLS, zero-seed generator or host
allocator substituted to get an executable to link.

The patch replaces std's actual default panic hook for this target, including the
default restored by `take_hook()`. It sends borrowed UTF-8 message/location spans
through the CRT panic bridge for the existing System crash service. The hook adds
no allocation, stdio lock, thread-name/TLS lookup or backtrace lock. This does not
remove TLS/synchronization used earlier by std's panic machinery. Custom hooks
remain upstream behavior; explicit abort and abort after a custom hook use a
separate terminal bridge and do not call the default hook again. The CRT bridge
and concurrent process-failure ownership still need native implementation and
qualification; compiling this hook does not establish a functioning bomb dialog.

Files, launch environment, clock adapters and process streams still require their
named implementations. Existing upstream unsupported backends remain unsupported
while this stage is developed. Neither linking std nor resolving one of its
private symbols qualifies a service.

`mac_toolbox::System::new()` now acquires the table installed by the current
application runtime without allocation or a new authority selector. The existing
native heap APPL fixture checks repeated acquisition on its real startup path;
native observations must be recorded separately from compiling that fixture.

The pinned compiler emits its ordinary main wrapper as `i32 main(i32, pointer)`
on this target. The CRT must preserve that C entry signature; the internal
`lang_start` signature is not the wrapper ABI. Inspect it again when changing
compiler/target revisions. Emitting LLVM IR establishes compiler output only,
not successful linking, startup or execution of an application.

The next integration boundary is the kernel Thread Manager contract, especially
TLS before std initialization, actual child completion/destruction before join,
and address-lifetime-safe compare-and-park/wake. Native thread creation is a
required delivery milestone, not an optional permanent stub.
