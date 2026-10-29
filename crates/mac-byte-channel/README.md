# VBYT byte-channel interface

VBYT version 1 represents one declared named byte stream. System selects its
provider by the declared class. VDAgent Channel binds that stream to the standard
`com.redhat.spice.0` port. All integer fields use little-endian encoding.

| Operation | Input | Reply |
| --- | --- | --- |
| `STATUS` (1) | empty | 16 bytes: state `u32`, last status `i32`, generation `u64` |
| `READ` (2) | empty | zero to 4096 stream bytes, bounded by output capacity |
| `WRITE` (3) | one to 4096 stream bytes | accepted byte count `u32` |
| `ERROR_TEXT` (4) | empty | current informative UTF-8 diagnostic, at most 256 bytes |
| `RESET` (5) | empty | empty acknowledgement after device cleanup/reset |

States are discovering (0), open (1), disconnected (2), unavailable (3), and
failed (4). A zero-byte read/write means backpressure. An open connection has a
nonzero, checked generation. Close or a generation change revokes every old
partial message and queued byte; a client must reset its protocol correlation.

Provider operations are synchronous and retain no caller slice. The resident
broker can still defer dispatch: callers retain every pending ticket and poll or
cancel that same ticket until terminal acknowledgement. `ByteChannel::invoke`
preserves that explicit lifetime. Broker contention can return `SERVICE_BUSY`
with a nonzero ticket before the provider sees input. A retry uses `Poll` and
resupplies the same owned WRITE bytes. WRITE callers advance only the accepted count;
`accepted` checks the exact reply length and rejects an impossible count.

READ/WRITE cancellation returns immediately because a raw operation retains
no state after its callback. RESET may remain pending through actual device
cleanup; cancelling its broker request does not free the device's retained
cleanup ticket. Later STATUS continues that cleanup before rediscovery. Bytes already accepted by a completed WRITE belong to the stream;
canceling a higher-level request cannot retract them. Protocol clients therefore
own message fragmentation, stale-response draining and recovery.

RESET acknowledges actual virtio device/claim/DMA cleanup before rediscovery,
not merely a new local protocol counter. The QEMU guest-close/reset path closes
the vdagent peer and clears its partial protocol state. An unavailable controller
can still refuse rediscovery; RESET success alone does not promise an open peer.
