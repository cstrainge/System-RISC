# Hosted Rust toolchain

Status: design/backlog, tracked as [P12](CORE-TOOLBOX-ROADMAP.md#p12--hosted-rust-toolchain).
Run rustc, Cargo, the linker and resource-packaging tools as CLAPs under Terminal.
First cross-build these tools on the host; later use them to build applications
inside System RISC. Rebuilding the compiler itself inside the OS is a subsequent
self-hosting gate, not the first milestone.

## Dependencies and porting inventory

- P8 std must support the compiler's real allocation, threads/TLS/synchronization,
  files, clocks/entropy, environment/paths, child processes, pipes and errors.
  P11 supplies session attachment, terminal capabilities and environment defaults.
  P13 supplies application protection; a compiler fault must not corrupt System.
- Inventory source/path lengths, encoding/case behavior, random-access and large
  file I/O, rename/locking/sync, temporary storage and working directories. The
  current classic HFS name limits and bounded whole-file writer are not a general
  compiler workspace. Select a working filesystem/storage profile with required
  semantics and explicit resource-fork publication support; do not shorten names
  lossily or assume every ELF/debug/archive file fits the current 4 MiB limits.
- Measure compiler/linker image size, stack/heap peaks, concurrent jobs and disk
  use before setting QEMU RAM or process reservations. Replace current fixed
  small image/cache limits with checked virtual mappings and resource budgets.
  Start with bounded build parallelism; do not tie this work to a frozen 500 MiB
  runtime image if compiler/workspace measurements require more storage.
- Pin the patched compiler/std sources and host/target definitions. Follow
  [Rust's new-target/host guidance](https://rustc-dev-guide.rust-lang.org/building/new-target.html).
  An existing cross-compilation target does not establish a compiler executable
  hosted by that OS. Record exactly which bootstrap and toolchain stages run
  outside versus inside the guest.
- Qualify LLVM/linker support and all native/C/C++ runtime dependencies (allocation,
  TLS, synchronization, files, math and process calls). Port only required ABI
  bridges to System services; do not infer libc/Unix availability from Rust std.
  Static tool components can precede general dynamic linking where feasible.
- Keep build-time ELF/object/archive/rlib formats distinct from installed native
  resources. The linker/package pipeline must publish APPL or CLAP resource
  forks with descriptors/RLOC and file metadata before launch. Define how Cargo's
  executable output paths pass through this pipeline, including temporary host
  tools and test binaries. Native resource-fork writing is a real prerequisite.
- Cargo [build scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html)
  execute host programs: here the host is System RISC. Run them as child CLAPs in
  the inherited Terminal session, preserving cwd/env/stdout protocol and status.
  Build host and target artifacts with their correct profiles, not the bootstrap
  computer's profile.
- [Proc macros](https://doc.rust-lang.org/reference/procedural-macros.html) require
  a separately qualified host module/loading and compiler bridge path. The
  current independently loaded WDEF contract is not Rust dylib support. Begin
  with a no-proc-macro crate profile, then implement the required linking/TLS/
  lifetime/ABI machinery against the exact compiler before widening claims.
- Offline builds come first. Cargo registry/git fetching later needs P7 TLS/DNS,
  trust and cache behavior, plus any actual native dependencies selected for
  Cargo. rustup, rustdoc, clippy, debugger and C compiler support are independent
  follow-on tool profiles; none follows solely from rustc printing its version.

## Delivery and evidence

1. Cross-package a diagnostic/std CLAP that exercises compiler-shaped workloads
   and normal child/pipe/filesystem operations. Establish memory/storage budgets.
2. Run a cross-built rustc plus linker/package tools under Terminal to compile
   a local Hello World into CLAP, then launch it and verify output/exit status.
   Also package a small resource-based GUI APPL and launch it from Finder.
3. Run Cargo offline on a vendored multi-crate workspace, with incremental/no-
   change builds, a build.rs child and normal diagnostics/failed compilations.
   Add tests and compiler output relocation at different process addresses.
4. Qualify proc macros and selected ecosystem dependencies, then authenticated
   online dependency acquisition. Publish supported tool/feature profiles.
5. Attempt a measured native Rust bootstrap and compiler test subset only after
   the preceding filesystem, process, module and memory gates pass. Distinguish
   compiling apps from actually self-hosting the compiler.

Use std/Terminal CLAP fixtures and real resident services for runtime acceptance;
retain canonical service assertions in `crates/mac-toolbox/tests`. Private guests
capture compiler/linker/Cargo versions, source hashes, command/env/cwd, artifacts,
exit statuses and memory/disk/timing results. No-change builds must avoid needless
work; busy compile/output workloads must preserve interactive UI responsiveness.
Prune obsolete generated test disks under the workspace rules; keep reports and
artifacts needed for provenance without accumulating HD snapshots.
