# Virtual host clipboard extension

Status: design/backlog, tracked as
[P18](CORE-TOOLBOX-ROADMAP.md#p18--virtual-host-clipboard-extension). Implement
after P15 built-in HFS-B/tooling and the P14 interface exercised by P16 StuffIt
and P17 external mounts. This is the next real consumer of the
[P6 extension SDK](SYSTEM-EXTENSIONS.md), proving typed hooks beyond storage.

## Host detection and packaging

Build a separately packaged kernel extension with the native INIT/module entry,
generated contract metadata, relocatable resources and owned service/hook
registrations. The underlying host-channel device provider uses DRVR/DABI where
applicable; the clipboard policy extension itself follows the P6 extension
contract. Install the clipboard extension in `System Folder/Extensions` and any
separately packaged host-channel driver in `System Folder/Drivers`, both under
the selected blessed installation. Avoid compiling host clipboard policy into
each app or into the Scrap Manager. Keep the local Scrap implementation available
without the extension.

Probe for an advertised virtual-host integration endpoint and negotiate its
clipboard protocol/version and supported formats. A virtualization indicator
alone is insufficient: sharing becomes active only with a working compatible
bridge. Physical hardware, an absent host endpoint, disabled sharing or an
unsupported version leaves the guest clipboard fully functional locally.
Discovery is bounded, with explicit unavailable/disconnected status and retry
on endpoint changes rather than on each input event.

Select and qualify the actual host transport/agent against Linux and native
Windows QEMU. Deliver any required host companion and launch configuration as
part of that milestone; detecting a VM or passing mock-channel tests is not
end-to-end sharing. Reuse host-integration transport services where suitable,
without coupling clipboard messages to filesystem container files or requiring
the full network stack. Specify connection/reset behavior and message limits.

## Resident Scrap integration

Applications keep using the public Toolbox clipboard APIs. Native Rust text is
UTF-8; legacy counted-MacRoman/Pascal conversions stay at their existing adapter
boundaries. Begin with bidirectional Unicode plain text, including empty text,
multiline text and emoji. Define host UTF-16/UTF-8 and newline conversion rules
explicitly; conversion/size errors preserve the current clipboard and report a
status. Images, styled text, file lists and other flavors need separate negotiated
contracts; do not claim lossless transfer of arbitrary Scrap records as text.

Expose a typed extension-side Scrap change subscription and snapshot/publish
service through resident interfaces, without a fabricated application identity.
Preserve ordinary native/legacy Scrap semantics. Introduce an internal change
revision for every relevant mutation: the current public compatibility generation
changes on clear, so it cannot by itself detect subsequent puts. Coalesce a
clear/put sequence into a consistent export snapshot; never publish mixed bytes
from different revisions or assume the empty intermediate state is the final
copy. Define the bounded batching/snapshot commit rule before implementing it.

Tag bridge transfers with origin/session and revision tokens to suppress echoes.
A host-imported snapshot must not be exported back as a fresh guest copy.
Define ordering for simultaneous host/guest updates; discard a delayed result if
the clipboard or bridge session changed while it was being fetched/converted.
Apply imports atomically through Scrap, preserving existing size/failure rules;
an unsupported host offer must not clear unrelated guest clipboard flavors.

Perform host I/O and conversion outside Scrap/desktop locks and UI/input paths.
Use bounded queues/payloads and asynchronous work with cancellation/timeouts;
take only the scheduler prerequisites needed for that context. Cache offers and
transfer on changes or demand, never per-event polling or whole-clipboard copies
on mouse motion. Notify after commit without calling extension code under Scrap
locks. Registration/request IDs use the shared generation-tagged facility with
separate ownership checks. Disconnect/reset revokes subscriptions and rejects
late replies from the previous connection before reuse.

Store enablement/direction preferences in versioned System JSON. Support local
operation when preferences are read-only/unavailable, and extensions-disabled
startup. The module stays pinned for the boot session initially; disabling bridge
traffic must still cancel pending work and preserve the local clipboard.

## Acceptance

Canonical `crates/mac-toolbox/tests` assertions use public Scrap calls through
real services. Test native Unicode copy/paste both ways, legacy conversion where
implemented, cross-app copying, clear/put batching, empty/large/unsupported offers,
conversion failures, no echo loops, concurrent changes and delayed replies.
Fake endpoints cover deterministic failures; private VM checks must additionally
copy real text between host editors and guest apps on each claimed host profile.

Cover absent/disabled/unsupported bridges, disconnect/reconnect, timeout, malformed
messages, allocation limits, boot without host integration and stale/foreign
registration/request IDs. Verify extensions-disabled recovery. Record transfer
latency, idle work, queue bounds and Cursor Test/present responsiveness while
sharing; no new UI redraw or input-event storm is acceptable.
