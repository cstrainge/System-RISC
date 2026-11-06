# Pipes

Status: core, the writer limit and the merged `Pipes` folder listing implemented;
folder creation-at-boot, Finder open-as-connect and the icon families not.
Baseline: `2f6d8e93`, 2026-11-05.

The resident byte-transport facility, its blocking wait, the per-name writer
limit, the public SDK and the File Manager's merged listing of the `Pipes` folder
are implemented and the guest builds and boots. Boot-time folder ensure, Finder
open-as-connect and the two icon families are not implemented; section 15 records
the exact state.

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
  It is not registered anywhere: it has no name, no catalog identity and no
  object in the folder's namespace, so enumeration cannot reveal it — not even
  through a count or through timing.
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

The number of *connected* writers is bounded by the writer limit the name was
registered with (section 3.2). A limit of one makes writer ownership of a name
structural: a second application is refused at `connect` rather than joined to
the first application's stream. Without a limit the ring has one byte stream and
no per-writer provenance, so two writers can interleave and neither can tell its
own bytes from the other's; a name that backs an exclusive resource must state a
limit of one.

The creator's own writer handle is not a connection and is not counted, so a
limit of one means one connecting owner at a time, not zero.

## 3. Operations

The facility exposes seven operations. Every one of them is a synchronous native
call; blocking operations park the calling application thread and are resumed by
the System scheduler (section 7).

| Operation | Endpoint | Meaning | Success |
|---|---|---|---|
| `create` | none | Create an anonymous pipe with a stated capacity. | Returns a reader handle and a writer handle |
| `create_named` | none | Create a pipe as `create`, register the name, and state its writer limit. | Returns a reader handle and a writer handle; the name now resolves |
| `connect` | none | Obtain a writer handle for a registered name, within its writer limit. | Returns a writer handle |
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

The call also states the name's **writer limit**: the maximum number of
concurrently open writer handles obtained through `connect`. Zero means
unlimited. The limit belongs to the name, is fixed at registration, and is not
a property of any endpoint.

- A limit is validated before anything is created. An anonymous `create` that
  carries a nonzero limit is `BadArgument`, because an anonymous pipe has no
  name and no connections.
- The limit does not count the creator's own writer handle. A limit of one
  therefore means one connecting owner at a time, and the creator still holds
  the writer it was given.
- A limit larger than the endpoint population the System can hold is accepted
  and behaves as unlimited; the registry's own limit decides when endpoints run
  out.

### 3.3 `connect`

- The name is resolved against the resident name registry. An unregistered name
  is refused with `NotFound`.
- A successful `connect` adds one writer handle. It does not change capacity, does
  not disturb existing data, and does not reorder anything.
- A name whose declared writer limit is already reached refuses `connect` with
  `AccessDenied`. The refusal consumes nothing: no handle is issued, no existing
  writer is disturbed and the pipe is unchanged. This is the refusal that makes
  exclusive writer ownership structural rather than advisory.
- Closing a connected writer handle releases its slot, so a later `connect`
  within the limit succeeds. A closed slot is never reassigned to a different
  pipe.
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
  no priority. A name's writer limit bounds how many writers there are; it does
  not order them, and a limit above one therefore permits interleaving.
- Multiple readers are not reachable in v1: `create`/`create_named` issue exactly
  one reader handle and `connect` issues only writers.

## 8. Name registry and lifetime

- A name is registered by `create_named` and resolves through `connect`.
- A name is 1 to 31 bytes of printable ASCII. A named pipe is also a Finder item,
  so its name is also an HFS item name, and the item listing carries the name as
  UTF-8 without a Mac Roman conversion step; a name that has no printable-ASCII
  form is refused at creation rather than sanitised on every enumeration. Space
  through `~` is legal. Control characters and the colon are not. Name comparison
  uses the File Manager's existing HFS name-equality rules, so two names that the
  File Manager considers equal are the same pipe name.
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
- Application exit is the ordinary last-close path, not a special case. Every
  handle the exiting process held is closed; if that closes the last handle of a
  pipe, the pipe is destroyed and its name is removed. A process that exits with
  a named pipe still open therefore takes its name with it, and a concurrent
  `connect` fails with `NotFound` rather than reaching a name nobody serves.
  Termination never leaks a name and never leaves a handle usable by a
  relaunched process.

## 9. The `Pipes` folder

### 9.1 The directory

- `System Folder/Pipes` is a real directory on the startup volume, with a real
  HFS catalog node ID, created if absent. It uses the File Manager's existing
  create-if-absent folder service, the same mechanism that finds and creates the
  blessed System Folder's `Fonts` folder. No new trap or new volume concept is
  introduced for it.
- The folder is ordinary storage. It can be opened, moved, renamed, trashed and
  recreated by ordinary File Manager operations, subject to the ordinary
  protections. The facility records its catalog node ID when the volume is
  mounted and again whenever a `Pipes` folder is created directly under the
  blessed folder; the merged listing follows the folder while that node ID is
  live. A mutation that removes, renames or moves the folder re-resolves the
  identity, so a stale node ID is never followed.
- Boot-time ensure is implemented. The pass that already resolves the startup
  volume's blessed folder creates the folder when it is absent, before any
  application starts, and logs the identity it recorded. A volume that cannot
  hold the folder is answered from its mount information without reading the
  catalog: the header already says it cannot hold one, and an existing folder
  there is already recorded when the volume is mounted.
- If the startup volume is read-only, no folder is created or expected. Named
  pipes still register and still connect by name; they have no Finder
  presentation. The folder is presentation, not authority.
- If the folder is deleted while pipes are registered, the pipes are unaffected
  and remain reachable by name until their handles close.

### 9.2 The merged listing

A directory read of the `Pipes` folder returns the real children and the named
pipes as one sequence. The merge is part of the File Manager's enumeration for
that catalog node ID, not a second volume and not a Finder-only display.

The provider behind the folder is the **named-pipe registry and nothing else**.
It is not a filtered view of one table that holds every pipe: an anonymous pipe
never enters the registry, so the folder has no object to describe and no count
to report. A request for an anonymous pipe's item therefore does not exist as a
refusal case — it is not an entry in that namespace at all.

- Real children are listed first, in ordinary HFS catalog order.
- Named pipes follow, in registration order. Registration order is the registry's
  own order and is stable for the life of an enumeration; sorting by name would
  cost an allocation per scan step for a display nicety the Finder's own layout
  decides anyway.
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
- `Get Info` on a named pipe reports the item's kind, name, zero fork lengths and
  the instant the name was registered as its created and modified date. It
  reports no allocation and no child count, because a named pipe has none.

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
  between enumeration and the open, `AccessDenied` when the name's writer limit is
  already reached or for a stale identity, `BadArgument` for a malformed one. The
  Finder reports the failure and refreshes the folder; it does not
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
storage format. A provider exists because a volume's contents come from
somewhere else: a disk, an archive, a host export. The pipe registry's contents
do not come from anywhere else. They are System state, owned and validated by
the same System that would have to be the provider's only client. Mounting it
through a loadable provider would add a driver boundary, a module contract and a
job/acknowledgement path around state that never leaves the System, and the
boundary would exist for nobody. That is a false abstraction, and this project
rejects that kind rather than keeping it for symmetry with real volumes.

The contract above therefore requires the File Manager to enumerate the folder
from two sources directly: its own catalog and the resident name registry.

The cost comparison supports the same conclusion, though the correctness argument
is the reason: a merged enumeration in the File Manager is roughly 600 to 1,000
resident lines, while expressing the same result as a sub-volume provider mount
is roughly 1,800 to 2,500. The larger number buys a general mount-point feature
that this facility does not need and a provider boundary that would be
unjustified.

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
| Terminal/CLAP session endpoints | not implemented; the launch attachment gap is recorded in section 10.1 | No |

The two coexist on purpose. The null standard streams are a stated GUI profile,
not a placeholder for anonymous pipes, and the launch path that installs them is
not converted to this facility. A future Terminal or command-runner profile will
bind real session endpoints, and that work is separate from this contract.

In v1 the facility's first named consumer is the NeoPixel extension. It depends
on the writer limit, not merely on named pipes: one pipe per strip is worthless
if a second application can also connect to it, because two applications'
frames would interleave in one stream and the extension could neither tell them
apart nor refuse the second owner. A name registered with a writer limit of one
is what makes "this strip has one owner" a structural fact. Anonymous pipes are
intended for the Terminal/`CLAP` path described in section 10.1, where the
Terminal creates them; named-pipe creation and connection remain ordinary
application operations because the folder and that extension need them.

### 10.1 The Terminal and `CLAP` command-line applications

`CLAP` is the file type for command-line applications. The Finder associates that
type with Terminal, so double-clicking a command-line application launches
Terminal. Terminal is the user interface; it is not the executor. A separate
System extension registered as an execute handler knows how to execute a `CLAP`.

The layering, in one line: **the Finder binds `CLAP` to Terminal; Terminal
creates the pipes and keeps one end of each; Terminal asks the process API to
execute the file, naming the other ends; the execute chain gives an extension
first pass, and seeing `CLAP` it attaches the named pipes and calls the base
handler.**

A `CLAP` has three stream endpoints, each a one-way byte transport: standard
input, standard output, standard error. There is no framing, no record boundary
and no terminal-protocol object inside a pipe.

**Who creates pipes.** The Terminal creates them. It keeps the writer of the
input pipe, the reader of the output pipe and the reader of the error pipe, and
offers the three opposite ends to the execute request. Anonymous `create` is
therefore a load-bearing operation of this design and is available to ordinary
applications, not only to System.

An earlier revision of this section moved creation to the execute extension. That
was wrong, and the reason is worth recording so it is not proposed again: an
extension that creates a pipe would have to hand one end to Terminal, and a
handle belongs to the process it was issued to, so Terminal could not use it.
Either a general handle-transfer facility would have to be invented, or the
architecture would simply not work. Under the specified model nothing of the kind
is needed: Terminal creates both ends, keeps one and offers the other in the
child's launch record, and the extension never holds a handle at all.

The execute extension is a **handler in the execution chain, not a service the
Terminal calls.** It gets first pass on the file being executed, recognises
`CLAP`, attaches the endpoints the execute request named, and calls the base
handler. If no handler claims a `CLAP`, the execute call fails and Terminal
receives that error through the ordinary return code; Terminal does not look the
extension up and does not ask it for anything directly.

Named-pipe creation is unaffected: `create_named` and `connect` remain ordinary
application operations, because the folder's purpose is that applications offer
and reach named pipes, and the NeoPixel extension depends on registering each
strip's name with a writer limit of one.

**What exists, and what does not.**

- *Exists:* application-created pipes with one reader and one writer end,
  ordinary ownership and last-close consequences.
- *Does not exist — the execute request carries no pipe endpoints.* The Process
  Manager's launch call identifies a target only by volume and catalog node ID.
  It has no handle, endpoint or attachment field, so there is nowhere for
  Terminal to name the three ends it is offering, and nowhere for the execute
  chain to say which ends the child should receive. This is the first real gap.
- *Does not exist — a child-side attachment record.* The services table an
  application receives at entry is a single System-wide immutable record holding
  a version, a size, a trap entry point and a capability bit set. There is no
  per-application attachment, and no per-application table to hang one on. The
  standard-library profile binds the null streams and knows nothing about pipes.
  This is the second real gap.

One fact is not a separate gap but it shapes both: a handle is valid only for the
process it was issued to, and this facility has no general transfer, adoption or
duplication operation. The execute request must therefore state that the endpoints
it names pass to the child as part of that launch — offered by Terminal, validated
as Terminal's at request time, and owned by the child from commit, with the
request's own ends invalid in Terminal afterwards. That is a rule belonging to
the execute request, not a new handle-transfer facility, and it is the reason the
endpoints must be named in the launch record rather than copied into it.

**What the execute path needs.** Two things, both outside this facility:

1. An execute request that carries the three child endpoints in addition to the
   target identity, with the pass-on-commit rule above. (Process API.)
2. A child-side way to read its three endpoints, so the standard-library profile
   can bind real `stdin`/`stdout`/`stderr` instead of the null streams.
   (Platform/runtime.)

The ordinary last-close consequences already follow from this contract: when the
child exits, its ends close, Terminal's reads see end-of-stream and its writes
fail with broken pipe.

**Gates on the Terminal/`CLAP` project, not on this facility.** Two behaviours
must be closed before the execute path depends on pipes, because a blocked
process that is never woken is a hang, not a wrong byte, and a hang in a terminal
looks like a dead machine:

- **No lost wake under load.** Blocking and waking must be exercised with
  concurrent readers, writers and closes under sustained pressure, beyond the
  single-writer scenario the transport probe covers.
- **Cancellation of a blocked caller by process exit.** The cleanup path exists
  and is exactly what a crashed child will exercise; it must be executed, not
  merely written.

These are recorded as gates on that dependent work so they cannot be forgotten as
a footnote to this one.

**Named dependency, not part of this facility.** The execute-handler chain —
extensions registering for first pass on execution, the association of `CLAP`
with Terminal, and the fallthrough to the base handler — is a separate mechanism
with its own design. This facility does not build it and nothing here implies it.

**Why the base handler's stub pipes are correct.** The base exec handler starts
GUI applications and attaches stub pipes. For a GUI application the standard
streams are placeholders by design, so the null standard-stream profile is the
correct behaviour rather than a missing implementation.

## 11. Authority

v1 has no access control. Any process may connect to any registered name, read
from a pipe it holds a reader handle for, and write to a pipe it holds a writer
handle for. There is no owner check on the name registry, no per-name
permission, and no way to reserve a name against another process.

This is recorded as a limitation rather than solved with an invented scheme.
Handle ownership (section 3) prevents one process from using another process's
handle, but it does not prevent a process from connecting to a name. A future
revision that adds authority must extend this contract rather than assume it.

The writer limit of section 3.2 is a capacity bound, not a permission. It says
how many connections a name has, not who may hold them: any process may connect
while a slot is free, and the limit does not distinguish one process from
another.

## 12. Deferred

Explicitly not promised, with the reason:

- **Fairness among multiple writers.** Permitted, not promised. Nothing in v1
  prevents one writer from monopolising a pipe. A writer limit of one removes the
  case entirely rather than ordering it.
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

`CLAP` is the second reserved type, for command-line applications (section 10.1).
It is not implemented and appears in no metadata position, resource or classifier
row at this baseline: every occurrence is prose planning. This facility does not
allocate it, does not read it, and does not claim it; it is recorded here because
the intended consumer of pipes introduces it, and a reader of this register
should see that the two codes are distinct and that `CLAP` is not yet a real
type.

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
- A name registered with a writer limit of one refuses a second `connect` with
  `AccessDenied`, accepts a new connection after the first closes, and leaves the
  first owner's stream untouched by the refusal.
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
- The boot check remains 18 of 18 and the canonical public-SDK suite still
  passes.

Section 15 records which of these have actually been executed.

## 15. Implementation status

Implemented and built into the guest, with the guest booting:

- The seven operations, the three outcomes, the fixed ring, the 512-byte
  atomicity rule, capacity bounds, the name registry and last-close removal.
- The per-name writer limit: `create_named` states it, `connect` refuses a full
  name with `AccessDenied`, and closing a connected writer releases the slot.
- Per-process endpoint handles from the shared handle facility, cleanup on
  process exit, and blocking that parks the calling thread.
- The public SDK (`Pipes`, `PipeReader`, `PipeWriter`) behind `System::pipes()`.
- The File Manager's merged enumeration of the `Pipes` folder: named pipes are
  items with synthetic identities, lookup by parent and name and by identity
  resolve the same item, an unknown name is `NotFound`, and every mutation inside
  the folder or on an item is refused with `NotSupported`. The folder identity is
  recorded at mount and again when a `Pipes` folder is created under the blessed
  folder.
- Boot-time creation of the folder, from the boot pass that already resolves the
  blessed folder, with the recorded identity proved against the catalog rather
  than assumed. A read-only startup volume is skipped from mount information
  alone.

Verified by execution in the guest, not only by compilation: a probe
application replaces Finder on a cloned disk and runs five cases — echo,
a 512-byte atomic write into a full pipe that must wait for a reader, would-block
versus end-of-stream, broken pipe, and name removal — followed by the writer-limit
and folder markers. The probe is
`crates/mac-toolbox/tests/fixtures/pipes-probe`; `cargo xtask boot-check` builds
it and runs it as the single `pipes-probe` case, so the marker set is part of the
gate. A second case, `pipes-folder-boot`, boots a writable clone that has no
`Pipes` folder in it and asserts `PIPES_FOLDER_READY … created=1` before the
probe finds the folder with the same identity by independent lookup; that is the
boot-creation evidence, and it is why the gate is 19 of 19. The read-only cases
assert `PIPES_FOLDER_SKIPPED … reason=readonly` instead. The writer-limit markers
assert that a one-writer name accepts one `connect`, refuses the second with
`AccessDenied`, accepts a new connection once the first closes, and still
disappears with its last handle. The folder markers assert one item per named
pipe, identical identity by name and by CNID, `NotFound` for an unknown name,
identity stability across two enumerations and non-reuse after destruction, that
an anonymous pipe does not appear, and eleven refused mutations.
`scripts/capture_pipes_probe.py` boots the same artifact standalone and records
the QEMU command and the serial hash.

Not implemented:

- Finder open-as-connect.
- The folder icon family, the `PIPE`/`PIPE` document family and its bundle, and
  the corresponding extension of the canonical icon-reservation allowlist. The
  type pair is reported today; the icons that would use it are not.

The probe covers the transport contract and the writer limit. It does not cover
multiple concurrent writers interleaving under load, or stale and foreign handle
refusals, which are refinements. Lost wake
under load and cancellation of a blocked caller by process exit are recorded in
section 10.1 as gates on the Terminal/`CLAP` project, not as footnotes: a hang
there is the failure that project cannot absorb.
