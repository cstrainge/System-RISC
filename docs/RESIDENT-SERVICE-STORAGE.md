# Resident service storage

A resident service can own an object in the permanent System ledger while an
application is its logical recipient. These are separate capabilities: System
owns storage and disposal; the authenticated recipient controls the public
protocol. Request fields never grant either authority.

## Choosing owners in resident controls

`Desktop::service_owners()` supplies `(recipient, storage)` from genuine resident
context. Normal application controls receive the same process owner twice.
System Open/Save, core alerts and report windows receive the actual recipient
and permanent System owner. The Control and ScrollBox managers take both
capabilities in their resident `request` methods. Their public Toolbox APIs stay
unchanged.

Route UI state by the recipient. `Store::owner(id)` identifies the physical
ledger owner and is not a UI routing label. `WindowManager::owner(id)` returns
the logical recipient. Read-only window metadata is available to that recipient;
mutating a System-owned service window requires the private service Activity.
Applications cannot close or paint that window through ordinary window calls.

Use one canonical manager/store for each owner and kind. In particular, boot
borrows the actual Control Manager for its progress object, then moves that
manager into Desktop. Boot completion disposes its own control ID. Creating a
second manager for the same System backing is an ownership conflict.

## Reserving publication and completion

The shared `mac-object-table::store` facility provides:

- `Store::reserve_publication(&OwnerRef)`, which reserves a real canonical slot,
  its index storage and reusable completion record.
- `PublicationReservation::ready()`, which becomes true only after the actual
  completion record is available again.
- `insert_reserved_in_scope_with_acknowledgement`, which publishes through a
  genuine operation scope and retains a lifetime pin through destruction and
  canonical acknowledgement.

A reservation is unique and is not an object ID. Every publication still issues
and validates its ID through the shared facility, including slot generations,
foreign-owner checks and exhaustion policy. Do not reset a generation, reopen a
closed scope or recycle an ID to reuse storage.

Shared vectors, indexes and queues must preserve held reservation headroom when
ordinary traffic grows them. Warming capacity during boot is insufficient:
ordinary callers could consume that capacity before the service needs it.
Releasing objects also retain their reservation until acknowledged.

## Allocation and lifetime

For ordinary System bookkeeping, `InvocationGate::with_system_allocation`
selects the permanent System ordinary account. Unique authority roots use the
foundation account where specified by the ownership model. Application-owned
resident payloads continue to carry their application's kernel charges.

The recovery coordinator alone issues the protected allocation capability.
Protected backing is disjoint from both the ordinary heap and terminal kit.
Allocation headers preserve the original backend, account and exact layout;
reallocation never silently migrates an ordinary allocation into the reserve.
A caller flag, object ID or module Host callback cannot select protected backing.

The one reusable modal Activity supplies actual System storage and an
authenticated recipient. `Activity::run` enters the service's own genuine I/O
root. `Activity::with_storage` restores backing without replacing an existing
request Subject or active-parent receipt; use that form for nested continuation
admission. Foreign module execution suppresses both capabilities. Ordinary child
application launch does not inherit them.

System-owned children do not belong to the recipient's teardown walk. Recipient
termination therefore explicitly cancels the service protocol. Children,
requests and retained cleanup tickets keep their Activity until actual
acknowledgement. An invalidated ID or missing live-window lookup is not proof of
disposal. Reuse waits for those acknowledgements and the real I/O-root drain;
concurrent requests receive typed Busy.

## Current scope and limits

Standard File, managed controls/scrollboxes, core alerts and the report viewer
use these mechanisms. The System ships resource-backed Error, Out Of Memory and
Save Changes alerts, exposed by the Rust dialog facade. Filesystem providers
validate new names through their authoritative PrepareName operation; the UI
does not guess another filesystem's rules.

This is a service-storage milestone, not completion of protected recovery.
Complete transitive drawing/cache admission, actual ordinary-memory-exhaustion
recovery, late Finder activation refusal/retry, repeated pressure recovery and
SMP remain separate gates. Typed errors and proven contained failures do not
establish containment of arbitrary privileged faults (M-D04).

Selected checks ran on an optimized single-hart RV64 QEMU image with Sv39,
8 GiB RAM, 800x600 display and input/event tracing disabled. Native SDK process
workloads exercised normal Open/Save reuse, malformed Dispose across scheduler
progress, denied direct service-window mutation and stale IDs after reserved-slot
reuse. Physical Text Edit exercised Options, saving exactly `hi`, preferences
files and app reaping. A private resource mutation exercised report-paint refusal,
retained pixels/metadata, denied external painting/closure, reset and exact UTF-8
READ. Earlier matching service paths exercised graphical short/long report
scroll/copy/dismiss. These observations establish only those scenarios; host
library results are not OS evidence, and no strict latency or SMP claim follows.
