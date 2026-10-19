# Resident Memory Manager: first handle slice

Applications use `System::memory()` from the aggregate `mac-toolbox` facade.
`toolbox-memory` owns only safe transport and RAII handle wrappers. System owns
storage, logical allocation spans, sizes, state bits and the last Memory Manager
error. The native boundary is shared by the host fixture and real RV64 ECALLs.

The implemented original-operation mappings are NewHandleClear, NewEmptyHandle,
GetHandleSize, SetHandleSize, HGetState/HSetState, HLock/HUnlock, HPurge/HNoPurge,
EmptyHandle, ReallocateHandle, DisposeHandle, HandToHand, PtrToHand, PtrAndHand,
HandAndHand and PtrToXHand. Safe byte read/write calls replace application pointer
access without changing the classic last error. Handle IDs are opaque,
generation-tagged native values, not 68k master pointers. Explicit dispose can
report errors; dropping a handle disposes it and can change the last error.

The reference is the actual English/Roman System7.6.1 Memory Manager running on
the pinned private q800 clone. See [the original findings](../../../../../analysis/memory-manager-reference.md)
and `provenance/memory-manager-reference.json`. All48 captured handle observations
are encoded in the aggregate userspace suite; the three NewPtrClear observations
are retained and explicitly deferred. Tests compare initialized bytes, errors,
sizes and all eight state bits. Invalid query return values are not compared.

Notable original behavior preserved by this slice:

- A zero-size NewHandleClear is allocated and queryable. An EmptyHandle retains
  handle identity but GetHandleSize/HGetState report nilHandleErr(-109).
- State is a byte, not a lock nesting count; HSetState preserves the tested low
  bits as well as the lock/purge/resource bits.
- Locked shrinking and zero/same-size requests succeed. The captured guarded
  growth to32/4096 fails with memFullErr(-108), leaving bytes, size and state
  intact. The captured locked append to20 succeeds with or without the guard.
- EmptyHandle and ReallocateHandle on a locked allocation fail with
  memPurErr(-112). A successful reallocation has state0. Uninitialized growth
  and reallocation bytes remain outside the original comparison contract.
- HandToHand preserves the source state and creates an independent state0 copy.
  Successful size/state queries reset MemError; safe byte copies preserve it.

The native zone is bounded to4MiB,1MiB per allocation and512 live handle slots.
It uses16-byte span alignment with12 bytes reserved per block. Backing bytes grow
on demand; handles retain logical offsets and expose no raw data pointers. Growth
can reuse adjacent free space; a locked block cannot move to another span. These
are explicit native allocation policies sufficient for the captured sequences,
**not** a reverse-engineered claim about the original heap's physical alignment,
master-pointer allocation, fragmentation, or every locked-growth outcome.

Full pointer APIs, zones, automatic purging/compaction, reserve/grow-zone callbacks,
resource ownership/loading integration, VM/physical-memory services, classic
32-bit pointer/handle records and application partitions remain future slices.
Setting a purgeable bit does not yet trigger pressure-driven reclamation.

All acceptance tests live in `crates/mac-toolbox/tests`, not this module. The
shared no-std conformance functions run against the host service adapter and the
actual RV64 gate from the launch-only ABI smoke runner. Private engine imports
are confined to bootstrap support; test bodies call only the public SDK.
