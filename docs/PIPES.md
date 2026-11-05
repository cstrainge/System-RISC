# Pipes

Status: specification. Nothing in this document is implemented yet.
Baseline: `2f6d8e93`, 2026-11-05.

This document is the contract for anonymous and named pipes in System RISC. It
states what a pipe is, what each operation means, which refusals are distinct,
what the identifier allocations are, and which parts of the surrounding system
the facility deliberately does not touch. It contains no implementation
guidance: any equivalent implementation that satisfies the contract is correct.

## 1. Scope

A pipe is a byte transport. It carries bytes in, bytes out, in order. It has no
message framing, no record boundaries, no structured payloads, no type tags and
no convenience operations for "messages". Any framing or protocol belongs to the
caller and is layered above the pipe.

A pipe is a System Toolbox facility, reached through its own native trap, in the
same way the File Manager and Process Manager are. It is not a kernel service and
not a device driver. Pipe storage is ordinary System memory, so a transfer does
not require a context switch per byte and does not depend on the scheduler
running a user thread for each byte.

Two kinds exist:

- **Anonymous pipe.** Created in pairs. No name. Never appears in the Finder.
- **Named pipe.** Created in pairs and additionally registered under a name. The
  name is how another process reaches it. Named pipes appear as items in the
  `Pipes` folder described in section 9.

## 2. Object model

A pipe is a bounded ring of bytes with exactly one reader endpoint and one writer
endpoint. The endpoints are handles, not files and not file descriptors.

- A handle names one endpoint of one pipe and belongs to the process that
  received it.
- Handles are generation-tagged. A handle that has been closed, or whose process
  has exited, is never valid again, even if the underlying slot is reused.
- Handles are opaque 64-bit values. They are not HFS catalog node IDs, not fork
  references, not window or resource identities, and they are not interchangeable
  with any of those.
- Closing all writer handles and all reader handles destroys the pipe and its
  buffered bytes.
- A pipe's byte capacity is fixed at creation and never changes.

Multiple writer handles for one pipe can exist: the creator holds one, and each
successful `connect` to a named pipe adds another. `connect` never creates a
second reader endpoint.

## 3. Operations

The facility exposes seven operations. Every one of them is a synchronous native
call; blocking operations park the calling application thread and are resumed by
the System scheduler (section 7).

| Operation | Endpoint | Meaning | Success |
|---|---|---|---|
| `create` | none | Create an anonymous pipe with a stated capacity. | Returns a reader handle and a writer handle |
| `create_named` | none | Create a pipe as `create` and register the name. | Returns a reader handle and a writer handle; the name now resolves |
| `connect` | none | Obtain a writer handle for a registered name. | Returns a writer handle |
| `read` | reader | Remove up to the buffer length bytes from the ring. | Returns the number of bytes transferred |
| `write` | writer | Add bytes to the ring. | Returns the number of bytes transferred |
| `set_blocking` | either | Set this handle's blocking mode for future operations. | Nothing; the mode persists on that handle |
| `close` | either | Release this handle. | Nothing; the endpoint count decreases |

Constraints that apply to every operation:

- The operation code, the reserved fields and the handle kind are validated
  before any effect. An unknown operation, a nonzero reserved field, or an
  endpoint kind that does not match the operation is refused with `BadArgument`
  and changes nothing.
- A handle may be used only by the process it was issued to. Another process's
  live handle is refused with `AccessDenied` and the owner's state is untouched.
  A stale, closed or foreign-generation handle is refused with `BadArgument`.
- Handles are single-use for `close`. Closing an already closed handle is a stale
  handle and is refused with `BadArgument`.
- A refused operation never consumes data, never advances a position and never
  changes an endpoint count.

### 3.1 `create`

- Capacity is an exact byte count, supplied by the caller, in the inclusive
  range 512 to 65536. Values outside that range are refused with `BadArgument`.
  The lower bound exists because a 512-byte write must always be able to fit
  (section 6); the upper bound is the v1 storage policy.
- At most 64 pipes may exist at once. The limit belongs to the resident pipe
  registry, is the same for anonymous and named pipes, and is not a per-process
  quota in v1.
- The new pipe has no name. Both returned handles start in blocking mode.
- Allocation failure, or refusal by the registry's pipe limit, is reported as
  `NoMemory`. A refused `create` leaves no handle, no name and no storage behind.

### 3.2 `create_named`

As `create`, plus name registration under the rules of section 8. A name that is
not a legal item name, or that is already registered, is refused with
`BadArgument` or `AlreadyExists` respectively, and no pipe is created and no
handle is issued.

### 3.3 `connect`

- The name is resolved against the resident name registry. An unregistered name
  is refused with `NotFound`.
- A successful `connect` adds one writer handle. It does not change capacity, does
  not disturb existing data, and does not reorder anything.
- `connect` to a name that is being torn down because its last handle is closing
  is refused rather than handed a dead endpoint. It never succeeds against
  something nobody serves.

### 3.4 `read`

- Transfers up to the supplied buffer length bytes from the front of the ring.
- The bytes are removed in the order they were written.
- A read of zero length, or a null buffer with a nonzero length, is refused with
  `BadArgument`.
- When bytes are available, at least one byte is transferred and the call does
  not park.
- When the ring is empty and at least one writer handle remains, the call blocks
  or reports `WouldBlock` according to the handle's mode.
- When the ring is empty and no writer handles remain, the call returns zero
  bytes. Zero is the only end-of-stream signal and it means exactly one thing:
  no writers remain and no buffered bytes remain.
- A read never returns zero bytes for any other reason. In particular, a
  non-blocking read of an empty ring with writers remaining is `WouldBlock`, never
  a zero-byte success.

### 3.5 `write`

- Transfers bytes from the caller's buffer to the back of the ring.
- A write of zero length, or a null buffer with a nonzero length, is refused with
  `BadArgument`.
- When no reader handles remain, the call fails with `BrokenPipe` immediately, in
  both blocking and non-blocking mode. Buffered bytes that nobody can read are
  discarded at the moment the last reader closes.
- Otherwise the transfer rules of section 6 decide how many bytes move.

### 3.6 `set_blocking`

- Takes a boolean. It changes only the addressed handle and only for subsequent
  operations.
- It never wakes, interrupts or retargets a peer, and it does not affect an
  operation already in progress on another handle.
- Handles start blocking. `set_blocking(false)` makes that handle's subsequent
  `read` or `write` calls return `WouldBlock` instead of parking.

### 3.7 `close`

- Releases the named handle. Closing a writer handle decrements the writer count;
  closing a reader handle decrements the reader count.
- Closing the last writer handle wakes every reader waiting on that pipe, and
  those readers then observe end-of-stream once the buffered bytes are consumed.
- Closing the last reader handle wakes every writer waiting on that pipe, and
  those writers then observe `BrokenPipe`.
- Closing any handle never blocks.
- When the last handle of either kind closes and no handles of any kind remain,
  the pipe is destroyed, its ring storage is released, and a registered name is
  removed from the registry.
- Process exit closes every handle the process held, exactly as if `close` had
  been called on each. A pipe whose endpoints all belonged to an exiting process
  is destroyed and its name removed.

## 4. The three outcomes

End-of-stream, would-block and broken pipe are three different conditions with
three different refusals. They are part of the contract, not an implementation
detail, and no implementation may collapse one into another.

| Condition | Where it arises | Result |
|---|---|---|
| End of stream | `read` on an empty ring, no writers remain | Success, zero bytes transferred |
| Would block | `read` on an empty ring with writers remaining, or `write` with no space, in non-blocking mode | `WouldBlock`, zero bytes transferred, handle and ring unchanged |
| Broken pipe | `write` when no readers remain | `BrokenPipe`, zero bytes transferred |

The following are explicitly *not* the same outcome and must not be reported as
one:

- A non-blocking read that finds no data is not end-of-stream. Only the absence
  of writers is end-of-stream.
- A full ring is not an error. `WouldBlock` is a retry condition.
- A write to a pipe whose reader closed is not `WouldBlock` and not a partial
  success.
- A closed or foreign handle is not any of the three; it is `AccessDenied` or
  `BadArgument`.

## 5. Blocking

- Blocking does not spin. A blocking call that cannot proceed parks the calling
  application thread through the System scheduler's existing wait mechanism and
  is resumed when the pipe's state changes or the application is terminated.
- Only an operation that would otherwise return `WouldBlock` parks. A `read`
  that can transfer, a `write` that can transfer, an end-of-stream read and a
  broken-pipe write all return without parking.
- A blocking call made from an execution context the scheduler cannot suspend
  (for example a nested definition callback or a critical section) is refused
  with `WrongThread` before it touches the ring. It does not consume data and
  does not partially transfer.
- A blocked operation ends with `OperationCancelled` when its application is
  terminated, and the terminating application's handles are then closed by the
  ordinary owner cleanup.
- No v1 operation accepts a deadline. A caller that needs a time limit changes
  the handle to non-blocking and retries; there is no promise about how long a
  parked call may wait beyond "until the condition changes or the application
  ends".

## 6. Transfer semantics

- **Capacity is fixed at creation** and is the exact number of payload bytes the
  ring holds. No operation grows it or shrinks it.
- **Writes of at most 512 bytes are indivisible against concurrent writers.**
  Such a write either transfers all of its bytes or transfers none: it never
  interleaves with another writer's bytes and never leaves a partial write
  behind. It parks (or reports `WouldBlock`) until the whole write fits and no
  other writer is in the middle of a write.
- **512 is not a record size.** It is an atomicity bound only. The facility
  attaches no meaning to that boundary, does not align anything to it, and
  exposes no operation that depends on it. Callers must not treat it as a
  message boundary.
- **A write larger than 512 bytes has no atomicity guarantee.** It transfers at
  least one byte and at most the requested length. Concurrent writers may
  interleave with it.
- **Partial transfers are permitted and callers loop.** A `write` reports the
  number of bytes accepted; a `read` reports the number of bytes produced. Either
  may be fewer than requested. A zero-byte `write` success does not exist: a
  write either transfers at least one byte or reports `WouldBlock`/`BrokenPipe`.
- A `read` transfers at most the buffer length and never more than the bytes
  currently queued, regardless of the ring's free space.
- The ring holds bytes, not references. Once a `write` succeeds, the caller may
  reuse its buffer immediately.

## 7. Ordering and fairness

- Bytes are delivered in the order they were written. There is no reordering, no
  prioritisation and no out-of-band path.
- Waiters on the same condition are woken in the order they began waiting.
- Wakeup order is the only ordering promised. Which woken writer or reader wins
  the resulting race is not specified; a woken caller must re-check the condition
  rather than assume the transfer is available.
- Multiple writers are permitted and receive no fairness guarantee: a writer that
  always has data may keep the ring full. There is no round-robin, no quota and
  no priority.
- Multiple readers are not reachable in v1: `create`/`create_named` issue exactly
  one reader handle and `connect` issues only writers.

## 8. Name registry and lifetime

- A name is registered by `create_named` and resolves through `connect`.
- A name is 1 to 31 bytes of UTF-8 that converts losslessly to Mac Roman, because
  a named pipe is also a Finder item and HFS item names have that limit. Colon,
  slash and control characters are not legal. Name comparison uses the File
  Manager's existing HFS name-equality rules, so two names that the File Manager
  considers equal are the same pipe name.
- **The name is removed when the last handle closes.** A registered name is never
  persistent: there is no on-disk record, no survival across application exit
  beyond live handles, and no survival across boot.
- A persistent name would be a stale-endpoint hazard: a `connect` that succeeds
  against a name nobody serves is worse than an honest refusal. `NotFound` from
  `connect` after a pipe is gone is the intended behaviour, not a failure to
  clean up.
- Names live in one registry for the whole System. Two volumes do not have
  separate pipe namespaces, and a name does not move with a volume.
- Registering a name is refused with `AlreadyExists` while that name resolves to
  a live pipe.

## 9. The `Pipes` folder

### 9.1 The directory

- `System Folder/Pipes` is a real directory on the startup volume, with a real
  HFS catalog node ID, created if absent. It uses the File Manager's existing
  create-if-absent folder service, the same mechanism that finds and creates the
  blessed System Folder's `Fonts` folder. No new trap or new volume concept is
  introduced for it.
- The folder is ordinary storage. It can be opened, moved, renamed, trashed and
  recreated by ordinary File Manager operations, subject to the ordinary
  protections. The facility records its catalog node ID at boot; the merged
  listing follows the folder while that node ID is live.
- If the startup volume is read-only, no folder is created or expected. Named
  pipes still register and still connect by name; they have no Finder
  presentation. The folder is presentation, not authority.
- If the folder is deleted while pipes are registered, the pipes are unaffected
  and remain reachable by name until their handles close. The next boot recreates
  the folder when the volume is writable.

### 9.2 The merged listing

A directory read of the `Pipes` folder returns the real children and the named
pipes as one sequence. The merge is part of the File Manager's enumeration for
that catalog node ID, not a second volume and not a Finder-only display.

- Real children are listed first, in ordinary HFS catalog order.
- Named pipes follow, in HFS name order.
- An item's fields never reveal its source: a named pipe is a file-kind entry
  with the type and creator of section 9.3, the folder's volume and catalog node
  ID as its parent, and the pipe's name as its name.
- The merged listing is a single cursor with a single revision. Registering or
  removing a named pipe changes that revision, so an enumeration that spans the
  change reports `ScanStale` rather than silently combining two snapshots. A
  caller restarts the enumeration.
- Lookup of a single name in this folder searches real children first, then named
  pipes. Path resolution uses the same lookup, so a path through the folder
  resolves both kinds.
- A named pipe's item identity is a synthetic catalog node ID from a range the
  File Manager reserves for entries that are not HFS catalog records. Within one
  boot a synthetic identity is assigned once and never reused, so a stale
  identity is always refusable rather than silently naming a different pipe.
- Mutations that would change a named pipe's item — rename, move, trash, delete,
  lock, stationery, comment, icon location — are refused with `NotSupported`.
  A named pipe is not storage and its name is its registry key.
- `Get Info` on a named pipe reports the item's kind, name and zero lengths. It
  does not report dates or allocation, because a named pipe has none.

### 9.3 Item kind

A named pipe appears with file type `PIPE` and creator `PIPE`. The Finder
therefore treats it as a document of a known kind: double-clicking it does not
launch an arbitrary application and does not fall through to a generic document
association.

### 9.4 Opening an item is `connect`

- Opening a `PIPE`/`PIPE` item takes the item's name and performs `connect`. A
  successful open produces the writer handle the caller would have obtained from
  `connect` by name.
- A failed open reports why: `NotFound` when the pipe's last handle closed
  between enumeration and the open, `AccessDenied`/`BadArgument` for a stale
  identity. The Finder reports the failure and refreshes the folder; it does not
  open a different item and does not launch an application.
- No launch association, file-kind handler registration, or `OPEN`/`OPEN_PATH`
  File Manager operation is defined for `PIPE`/`PIPE`. The File Manager's generic
  open operations refuse these items; opening is the pipe facility's operation,
  performed on the item's name.

### 9.5 Why the merge is File Manager enumeration and not a mounted volume

The File Manager's loadable filesystem providers are mounted as whole volumes.
A provider publishes a volume descriptor with a root, a capacity and a
filesystem code; every request then addresses that volume and a catalog node
inside it. There is no mount point below a volume root and no operation that
returns two sources' children as one listing. A provider mount at
`System Folder/Pipes` therefore cannot be expressed by the existing mechanism.

The named-pipe registry is resident System state, not an independently owned
storage format. Mounting it through a loadable provider would add a driver
boundary, a module contract and a job/acknowledgement path around state that
never leaves the System. The contract above therefore requires the File Manager
to enumerate the folder from two sources directly: its own catalog and the
resident name registry.

A general sub-volume provider mount — a mount point on a live directory, a
provider whose entries are projected into that directory's catalogue, a shared
synthetic identifier space that both sources agree on, and a merged cursor with
one revision across two sources — remains a separate, larger change. It is not
a prerequisite for pipes, and nothing in this contract depends on it.

## 10. Launch paths, and the stubbed standard streams

The GUI standard streams are deliberately stubbed, and this facility does not
replace them:

| Path | What the application gets today | Uses pipes? |
|---|---|---|
| Finder-launched GUI application (`APPL`/`FNDR`), including the platform standard-library profile | stdin is permanently at end-of-stream; stdout and stderr discard bytes and report a successful flush; none of them is a terminal | No |
| Any native application that calls the Toolbox pipes interface | real endpoints from this facility | Yes |
| Child process creation, redirection or inherited standard streams | does not exist in this tree | No |
| Terminal/CLAP session endpoints | not implemented | No |

The two coexist on purpose. The null standard streams are a stated GUI profile,
not a placeholder for anonymous pipes, and the launch path that installs them is
not converted to this facility. A future Terminal or command-runner profile will
bind real session endpoints, and that work is separate from this contract.

In v1 the facility's only caller is therefore an application that asks for a pipe
explicitly. The intended first caller is the NeoPixel extension. No resident
service and no launcher consumes pipes in v1; the facility is built ahead of that
caller rather than in response to existing demand.

## 11. Authority

v1 has no access control. Any process may connect to any registered name, read
from a pipe it holds a reader handle for, and write to a pipe it holds a writer
handle for. There is no owner check on the name registry, no per-name
permission, and no way to reserve a name against another process.

This is recorded as a limitation rather than solved with an invented scheme.
Handle ownership (section 3) prevents one process from using another process's
handle, but it does not prevent a process from connecting to a name. A future
revision that adds authority must extend this contract rather than assume it.

## 12. Deferred

Explicitly not promised, with the reason:

- **Fairness among multiple writers.** Permitted, not promised. Nothing in v1
  prevents one writer from monopolising a pipe.
- **Wakeup ordering beyond arrival order.** Waiters are woken in arrival order,
  but the outcome of the resulting race is unspecified.
- **Deadlines.** No v1 operation has one. "Blocking does not spin" is the whole
  guarantee; a caller wanting a limit uses non-blocking mode.
- **Framing, message boundaries, record sizes.** Never part of pipes. The 512-byte
  atomicity bound is not a message size.
- **Persistent names, and names that survive boot.** A name exists only while a
  handle exists.
- **Access control.** Section 11.
- **Standard-stream, redirection and child-process integration.** Section 10.
- **Pipes across volumes or machines.** One System-wide registry, one machine.
- **Signals, polling/readiness sets, and asynchronous notification.** A caller
  polls with non-blocking operations or blocks; there is no readiness-set
  operation.

## 13. Collision register

This section records the identifiers this facility takes and the identifiers it
deliberately does not take. All statements are facts about baseline `2f6d8e93`.

### 13.1 Native trap numbers

Native service numbers occupy a contiguous block. Every number from `0x10000`
through `0x1002e` is in use; there are no gaps.

| Number | Service | Number | Service |
|---|---|---|---|
| `0x10000` | `LOG` | `0x10018` | `CLOCK` |
| `0x10001` | `WINDOW` | `0x10019` | `SCRAP` |
| `0x10002` | `DRAW` | `0x1001a` | `EVENTS` |
| `0x10003` | `NEXT_EVENT` | `0x1001b` | `TEXTEDIT` |
| `0x10004` | `DEFINITION` | `0x1001c` | `PROCESS` |
| `0x10005` | `CLASSIC` | `0x1001d` | `WAIT_EVENT` |
| `0x10006` | `GEOMETRY` | `0x1001e` | `MONOTONIC_CLOCK` |
| `0x10007` | `SURFACE` | `0x1001f` | `CRASH` |
| `0x10008` | `QUICKDRAW` | `0x10020` | `STANDARD_FILE` |
| `0x10009` | `FONT` | `0x10021` | `SCROLL_BOX` |
| `0x1000a` | `REGION` | `0x10022` | `DRAG` |
| `0x1000b` | `COPY_BITS` | `0x10023` | `POSITIONED_EVENT` |
| `0x1000c` | `WORLD` | `0x10024` | `INSPECTION` |
| `0x1000d` | `PICTURE` | `0x10025` | `THEME` |
| `0x1000e` | `CURSOR` | `0x10026` | `DEFINITION_PANIC` |
| `0x1000f` | `MAPPING` | `0x10027` | `POWER` |
| `0x10010` | `RESOURCE` | `0x10028` | `APPLICATION_HEAP` |
| `0x10011` | `ICON` | `0x10029` | `CPU_INFO` |
| `0x10012` | `FILE` | `0x1002a` | `NATIVE_THREAD` |
| `0x10013` | `CONTROL` | `0x1002b` | `TLS` |
| `0x10014` | `MEMORY` | `0x1002c` | `ADDRESS_WAIT` |
| `0x10015` | `IMAGE` | `0x1002d` | `ENTROPY` |
| `0x10016` | `MENU` | `0x1002e` | `USER_CALLBACK` |
| `0x10017` | `DIALOG` | | |

Two facts about the numbering:

- The bit `0x20000` is a flag, not an allocation. It selects the Pascal/Mac Roman
  variant of an existing native service carrying human text, so it does not
  consume numbers.
- The supervisor half of the scheduler uses a separate custom instruction
  encoding, not a number in this block.

**Allocated by this facility: `0x1002f` = `PIPES`.** It is the first unoccupied
number, it is not a Mac Roman variant, and it is additive: no existing number
changes meaning. The native development ABI stays at version 1 because this is
an additive service in an unreleased tree.

Operation selectors inside the pipes call occupy the small-integer space of that
call alone (0 through 6, in section 3 order). They are not trap numbers and share
no namespace with the File Manager's operation codes, the QuickDraw operation
codes, or any other service's selectors.

New status values, after the highest existing status `INVALID_NAME` (-20):

- `WOULD_BLOCK` = -21, surfaced by the Rust interface as `WouldBlock`.
- `BROKEN_PIPE` = -22, surfaced by the Rust interface as `BrokenPipe`.

Every status from 0 through -20 keeps its current meaning, and neither new value
is an alias for an existing condition.

### 13.2 Module and interface class identifiers

Class identifiers are four bytes, declared by the interface crate that owns the
request layout, and validated by the module loader against the records a module
offers. They are not centrally numbered; they are compared by value, so an
allocation is proven unused by showing the value appears nowhere.

In use at this baseline: `VBYT` (byte channel), `VDEV` (device interface),
`ENTR` (entropy), `FORK` (file source), `FSYS` (filesystem), `HDIR` (host
directory), `MLIF` (module lifecycle), `MPRE` (module preferences), `POWR`
(power), `SCHD` (scheduler policy), `SCTL` (scheduler control), `SCEX` (scrap
extension), `VIDO` (video). Bus names in use include `BUS!`, `VIRT`, `PCI!`,
`VMMI`. Test and fixture-only classes `TEST`, `PCTL`, `TIME` and `NETW` are not
production interfaces.

**Allocated by this facility: `PIPE`.** It appears in no class position anywhere
in the tree. It will be declared by the pipes interface module, if and when a
loadable component provides or consumes the interface; the resident facility
itself needs no class identifier because it is reached by its trap, exactly as
the File Manager is.

### 13.3 Object and lifetime namespaces

Resident managers register object namespaces centrally; a manager does not
allocate its own. The value is a four-byte code, and registrations carry a
capacity policy, an owner and a cleanup dispatch.

In use at this baseline include `KREG` (the registration namespace itself),
`CTRL`/`CSTA`, `THRD`/`THST`, `WIND`/`WNST`, `MENU`/`MNST`, `MMBL`/`MMST`,
`TEDT`/`TESV`, `ALRT`/`ALST`, `DRAG`/`DRST`, `DRCV`/`DRVS`, `FWCH`/`FWST`,
`FTSU`/`FTST`, `FTXN`/`FTXS`, `FFRK`/`FFST`, `FDWR`/`FDST`, `FSWR`/`FSST`,
`FIGR`/`FIGS`, `FPIN`/`FPST`/`FPOB`/`FPOS`, `DBAT`/`DBST`, `FIOP`/`FIOB`,
`AHEP`/`AHST`/`AALC`/`AAST`, `IMAG`/`IMST`/`IMOB`/`IMOS`, `SBOX`/`SBST`,
`SFDL`/`SFST`, `GWLD`/`GWST`, `SPCE`/`SPST`, and the driver namespaces
`DIFC`/`DIFS`/`DDEP`/`DDST`/`DREQ`/`DRQS`/`DJOB`/`DJST`/`DIOP`/`DIOB`/`SCHR`/`SCHS`.

**Allocated by this facility: `PIPE` for pipe endpoints and `PPST` for their
runtime state.** Neither appears in any namespace registration. They follow the
established endpoint/state pair shape used by windows, menus and controls. The
File Manager's synthetic identifier range for virtual entries is a separate
reservation recorded with the File Manager, not an object namespace.

### 13.4 File types and creators

File type and creator are four-byte Finder metadata. In use: `APPL`, `FNDR`,
`TEXT`, `TEDT`, `EDIT`, `JSON`, `BINA`, `SIT!`, `DRVR`, `INIT`, `RV64`, `zsys`,
`MACS`, `DMGR`, `BTFL`, `sfnt`, and the file system's own volume signature
`MACS`. The document classifier's preference table maps a small set of filename
extensions and never invents a type for a file that already has one.

**Allocated by this facility: type `PIPE`, creator `PIPE`** for named-pipe items.
Neither appears in any metadata position, any resource, or the classifier's
table. This is the same four bytes as the class identifier and the object
namespace in two other namespaces; that is not a collision, because a class
identifier is never compared with a file type and an object namespace is never
compared with either. The repeated value is chosen so a reader who meets `PIPE`
in any of the three places knows which facility it belongs to.

### 13.5 Resource types

Resource types in use include `MENU`, `ALRT`, `DLOG`, `DITL`, `BNDL`, `FREF`,
`ICN#`, `icl8`, `ics#`, `ics8`, `clut`, `FOND`, `FONT`, `NFNT`, `sfnt`, `DRVR`,
`INIT`, `RV64`, `EABI`, `NABI`, `DABI`, `RLOC`.

**No new resource type is allocated.** `PIPE` is a file type, not a resource
type, and the icon assets in section 9 use existing resource types only. Nothing
in the system resource manifest currently uses `PIPE` in a resource-type
position.

### 13.6 Operation names

`read`, `write`, `open`, `close` and `create` are classic Toolbox concepts. The
ambiguity is resolved by namespace and by handle type, not by renaming:

- The pipe operations are methods of the pipes module's reader and writer
  handle types. They are not free functions, not operations on a fork
  reference, and not operations on a window, resource or file handle.
- The file facility also has `open`, `read`, `close` and `seek`. They are methods
  of its own open-fork handle type, in its own module. Two modules with a method
  named `read` is ordinary Rust and creates no ambiguity: the receiver's type
  decides.
- The classic File Manager's `Open`, `Read`, `Write`, `Close` and their
  parameter-block forms are not implemented in this build. The classic trap
  dispatcher serves drag, rectangle/point geometry, text and fonts, cursors,
  events and `TickCount` only. No classic selector is shadowed or reinterpreted
  by this facility, and no classic trap number is taken.
- The pipe endpoint type is distinct from every other handle type. Passing a pipe
  handle where a file handle is expected is a type error in the Rust interface
  and a kind mismatch in the native call.

## 14. Acceptance

The facility is acceptable when all of the following hold, each exercised
against the shipping artifacts rather than a host substitute:

- A creator and a connector exchange bytes through a named pipe, including a
  write larger than the free space, with partial transfers and looping.
- End-of-stream, would-block and broken pipe are each observed, and each is
  distinguishable from the other two in the same scenario family.
- A 512-byte write from two concurrent writers is never interleaved.
- A non-blocking read of an empty pipe with a live writer reports would-block and
  not end-of-stream.
- Closing the last writer makes a blocked reader complete with end-of-stream;
  closing the last reader makes a blocked writer fail with broken pipe.
- Process exit closes handles, removes names and unblocks peers.
- The `Pipes` folder merges real children and named pipes in one enumeration; a
  registration during enumeration yields `ScanStale`; a lookup and a path
  resolution reach a named pipe; mutations are refused.
- Double-clicking a named pipe connects and does not launch an application.
- The folder shows its own icon and a named pipe shows the `PIPE`/`PIPE` icon.
- The GUI standard streams still behave exactly as section 10 describes.
- The boot check remains 17 of 17 and the canonical public-SDK suite still
  passes.
