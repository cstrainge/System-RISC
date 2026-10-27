# Named virtio-console adapter

`mac-virtio-console` supplies the single MMIO/DMA implementation used by Host
Transport and VDAgent Channel. It searches every slot reported by the resident
VDEV inventory and admits a device only after a claim, reset acknowledgement,
modern feature negotiation, and an exact UTF-8 `PORT_NAME` match. One five-second wall-clock budget covers
all inventory, claim, initial reset and name discovery retries. A contended probe
retries the same ordinal, and an unrelated claimed controller cannot keep an
absent endpoint searching indefinitely. Cleanup continues after discovery expires
until its reset ticket reaches terminal acknowledgement. Driver filenames
and inventory order do not identify the service.

The admitted QEMU profile is one dedicated `virtio-serial-device,max_ports=2`
controller containing one `virtserialport,nr=1`. QEMU reserves port zero for
`virtconsole` and only advertises the named-port control feature when `max_ports`
is greater than one. Host Transport requires `org.system-risc.host-filesystem`;
VDAgent Channel requires `com.redhat.spice.0`. A different maximum or an announced
port other than one is a typed unsupported-profile refusal. Configure the declared
profile and restart the guest. General multiport-controller support remains a gate.

Each loaded provider owns a static, admitted four-queue DMA arena: eight 4 KiB
buffers per queue, with distinct ring metadata. Arena banks zero and one map to
device data queues four and five; banks two and three map to control queues two
and three. Reserved port-zero queues remain unconfigured. The mapped arena is
144 KiB, including ring storage and alignment. Control handling, completion
reclamation and transfers are bounded per callback. Short reads retain the
remainder of the completed DMA buffer without reposting it prematurely.

Both host-open and published guest-open are required before byte I/O. Host close
or removal revokes byte access immediately; reset acknowledgement precedes ring
reuse. A checked connection generation changes on each subsequent open. Clients
must discard their old protocol state whenever the connection closes or its
generation changes. The generation never wraps.

Reset, unmap and claim release follow one cleanup walk. A pending reset keeps its
exact ticket and pins every mapping. Failed drop retains that authority in the
arena; another initialization retries cleanup before acquiring the arena. The
resident module owner must also keep the admitted module backing loaded until
all device claims and mappings retire. Cleanup failure is reported, never treated
as permission to reclaim hardware-owned memory.

Host tests cover control ordering, exact names, profile refusals, cleanup failures
and the VBYT client/provider stream contract. Native release builds check the
shared adapter in both consumers. Real guest boot and SPICE-client interoperability
must additionally qualify discovery, runtime transport and clipboard behavior;
host tests and native compilation do not establish those gates.
