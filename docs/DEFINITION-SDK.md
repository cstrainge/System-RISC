# Definition author interface

Accepted review requirements, Chloe via Nyx, 2026-10-23. These apply to WDEF,
CDEF, MDEF and future independently loaded definition resources. The current
raw entry points are being replaced alongside the resource-model migration;
this document does not claim that replacement is complete.

Authors implement safe Rust handlers over native messages, typed state, a
borrowed port and resident services. A single audited binding layer validates
and decodes the raw call, establishes its borrows, calls the author and encodes
the result. Authors do not cast object/surface pointers, reconstitute services
or write FFI safety contracts. There is no Rust trait-object ABI across
independently linked code resources. The loader still validates and relocates
the code and pins the invocation's executable backing.

Message enums are exhaustive, with family-specific semantics such as Draw,
HitTest, CalculateRegions, Initialize, Dispose, Validate and ChangeBounds.
The shim rejects unknown wire values before entering author code. It decodes
kind/variation into their native family types too. A new supported message
requires an explicit answer from each author; numeric range arms and duplicate
validation branches are superseded. Definition-specific geometry and behavior
remain in the definition, not in the shim or resident Control Manager.

The platform scaffold supplies entry and panic handling. There is no
spin-forever panic handler in author code. Contained failures report through
the System-owned recovery machinery, fail the invocation typed and quarantine
the registration according to the proven invocation contract. The host owns
current-definition identity and diagnostics; callers cannot fabricate that
identity in a panic-report request. Panic reporting uses bounded, preallocated
storage and cannot depend on the failing definition's allocator or a borrowed
Desktop lock. Where the current abort/context contract cannot safely recover,
the System reports the definition and cause, then shuts down loudly. It must
not return into a corrupted invocation or silently spin.

Contained failures are typed errors, contract-level refusals, and faults covered
by the proven panic/unwind contract.

**M-D04:** The walk provides resource containment. Arbitrary privileged-fault
containment is **unproven** and is a **required future gate**. Until a recovery
contract exists and passes, the System must not claim it can safely recover from
every mid-call module fault.

Colors come from resident theme roles. Do not add literal palettes while role
plumbing is staged. Static text is transparent: it draws its glyphs into the
borrowed port without erasing its bounds. Restoring removed/changed glyphs is
bounded retained-compositor work over that control's damage, preserving the
parent's background and other objects.

Acceptance uses the public Toolbox for pixel/geometry/lifetime behavior, plus
binding tests for malformed headers, null/misaligned state, unknown messages,
invalid variations and no author entry on rejection. A private native definition
panic probe must produce a System report and a contained refusal/quarantine or
an observable shutdown; a timeout or silent hang is failure. Panic containment
is not inferred from a host catch_unwind test. Keep original-system pixel and
interaction reference provenance when migrating the remaining families.
