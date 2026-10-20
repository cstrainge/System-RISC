# Networking stack

Status: design/backlog, tracked as [platform workstream P7](CORE-TOOLBOX-ROADMAP.md#p7--full-networking-stack).
Plan a resident, thread-capable host networking stack with native Rust APIs,
future `std::net` integration and separate classic networking adapters. There is
no network device driver, socket service or protocol stack implemented by this
document. A successful ping will establish one slice, not full networking.

## Architecture and interfaces

Keep protocol state and packet processing in System, with an architecture-neutral
Rust protocol core and narrow machine/device adapters. Applications use thin
`toolbox/networking` bindings through `mac-toolbox`; they must not each link a
TCP/IP stack. Build the eventual Rust std platform adapter on the minimal native
service/transport ABI, avoiding a dependency on the application Toolbox facade.

The public surface needs owned RAII TCP streams/listeners, UDP sockets, address
and interface types, resolver requests and readiness subscriptions. Specify
bind/listen/accept/connect, send/receive, datagram boundaries and truncation,
partial I/O, EOF/half-close/shutdown/reset, local/peer addresses, options,
nonblocking mode, deadlines, cancellation, readiness and errors. Readiness is
advisory and must not lose a wakeup between checking and sleeping. Define handle
sharing/cloning and last-close semantics explicitly, including process exit and
abandoned requests. Drop must not panic or perform an unbounded network wait.

All interface/socket/request/registration IDs use the shared generation-tagged
facility with centrally assigned namespaces and separate trusted ownership
checks. Never expose stack-internal pointers as app descriptors. The future std
descriptor adapter must preserve validation through reuse, duplication and
teardown. Define privileged operations such as interface configuration, raw
packets and port policy through actual System access checks.

The core and SDK remain usable without std. Favor safe Rust and bounded/fallible
buffers; confine unsafe code to DMA, MMIO and ABI adapters. Evaluate an existing
engine, initially [smoltcp](https://github.com/smoltcp-rs/smoltcp), against the
protocol/error/performance inventory before selecting a pinned dependency.
Its documented omissions make adoption a starting point, not a completion claim.
Record feature gaps, licenses, patches and parser-testing evidence.

## Devices, execution and resource limits

- Start with software loopback and a virtio-net adapter on the existing QEMU
  machine. Negotiate features, validate descriptor/packet lengths, define DMA
  ownership, MAC/MTU/link state, RX/TX completion, reset and buffer reclamation.
  Only advertise checksum/segmentation offloads actually implemented. Device
  generations must prevent late completions reaching a replacement interface.
- Keep interrupt work bounded; defer parsing and protocol progression to
  preemptible network work. Drive timers by monotonic deadlines and wake on
  packet arrivals, completions or the next deadline. Do not poll the whole stack
  on every UI input, trap or animation tick, or busy-wait for packets.
- Define queue ownership, lock ordering and cross-hart publication. Never hold
  manager locks while waiting for I/O or calling application/extension code.
  General blocking sockets require scheduler park/wake/cancellation; today's
  globally interrupt-masked synchronous traps cannot host long network work.
- Bound packet pools, per-socket buffers, connection/listen queues, neighbor and
  route caches, reassembly, resolver requests and retries. Provide backpressure,
  expiration and explicit resource errors. Limit work per drain and preserve
  fairness between applications, receive/transmit and interactive UI work.
  Define overload/drop counters without synchronous per-packet serial logging.
- Keep NIC implementations replaceable through the P6 kernel-extension service
  contract once available; basic networking need not await dynamic loading.
  Other Ethernet adapters and future x86 reuse the same resident protocol/API
  layer. Wireless, PPP, bridging, routing/NAT and VPNs need their own later
  device/protocol profiles; do not silently treat them as delivered by TCP/IP.

## Protocol and service inventory

Create a versioned implementation/conformance matrix before coding, including
the applicable RFC updates and errata. Primary starting points include
[host requirements](https://www.rfc-editor.org/info/rfc1122/),
[TCP](https://www.rfc-editor.org/info/rfc9293/) and
[IPv6](https://www.rfc-editor.org/info/rfc8200/). Explicitly record supported
options, bounded limits, unimplemented behavior and interoperability evidence.

| Area | Required host-stack work |
| --- | --- |
| Interfaces and Ethernet | Loopback, Ethernet framing, unicast/broadcast/multicast, MTU, link changes and multiple interface identities. |
| IPv4 | ARP/neighbor expiry, addresses/subnets/routes/default gateway, ICMP errors/echo, checksums, bounded fragmentation/reassembly and path-MTU behavior. |
| IPv6 | Scoped addresses, NDP, router advertisements, SLAAC and duplicate-address detection, ICMPv6/errors, extension-header validation, bounded fragment handling and path MTU. |
| UDP | IPv4/IPv6, checksums, bind/demultiplex, connected/unconnected datagrams, broadcast and explicit multicast memberships/interface selection. |
| TCP | Client/server state machines, sequence/window handling, retransmit/RTT, congestion control, out-of-order data, listen backlog, flow control/backpressure, half-close/reset, TIME_WAIT and socket options. |
| Configuration | Static IPv4/IPv6, DHCPv4 lease acquisition/renew/rebind/expiry, IPv6 autoconfiguration and DHCPv6 profile, route/address lifetimes and link-loss recovery. |
| Naming | DNS A/AAAA, bounded parsing/cache/TTL/retries, negative replies, UDP-to-TCP fallback, cancellation and address-selection policy. Define internationalized-name conversion at the native UTF-8 boundary. |
| Discovery | Multicast lifecycle and required IGMP/MLD behavior, followed by mDNS/DNS-SD service discovery; test on a real shared link. |
| Protected application traffic | TLS above sockets, with an evaluated Rust implementation, usable entropy, certificate/hostname validation, trust-store updates and reliable wall-clock policy. HTTPS is an application/service layer. |

Do not equate transport success with all higher-level services. File sharing,
remote mounts, printing and browsing/discovery UIs need their own protocol and
Toolbox integration. Protocol helpers such as TLS need not execute in the kernel
merely because TCP does. Entropy/clock/storage prerequisites must be real before
claiming authenticated modern networking; do not ship a bypassed-validation demo
as the TLS milestone.

## Configuration and control panels

Use a resource-based Network control panel through the P6 panel/settings model.
Persist versioned JSON for interface profiles, addresses/routes, resolver,
enabled protocols and policy. Distinguish desired configuration from live leases,
link status and effective routes; transient lease data is not a preference write
on every packet. Validate edits, define apply/cancel/restart behavior and publish
configuration changes atomically. Expire stale leases and invalidate affected
resolver/routes/sockets according to explicit rules.

Expose bounded diagnostics: link/address/route/resolver state, socket ownership,
queue/drop/error counters and opt-in packet/timing traces. UI updates follow
actual state changes and invalidate only their own bounds. An absent cable,
failed DHCP exchange or missing NIC is an ordinary reported state.

## Legacy Mac networking

Inventory the pinned System 7.6.1 installation and availability before choosing
the first original application. Apple's
[Open Transport documentation](https://dev.os9.ca/techpubs/mac/OpenTransport/OpenTransport-9.html)
describes both TCP/IP and AppleTalk; the
[Networking reference](https://dev.os9.ca/techpubs/mac/pdf/Networking/pdf.html)
is a starting point for the original AppleTalk family. Capture actual installed
versions rather than assuming all interfaces coexist in one profile.

Track MacTCP and Open Transport endpoint/mapper adapters, synchronous/asynchronous
requests, notifier callbacks, cancellation/completion ordering, buffer ownership,
error codes and legacy names/records independently. Translate supported TCP/IP
requests to the same resident stack used by Rust apps, preserving their app and
cooperative-context identity without blocking unrelated applications.

AppleTalk is a separate protocol-family milestone: inventory DDP, NBP, ZIP, ATP,
ADSP and related session/link contracts against the selected original apps.
AFP/AppleShare and Chooser integration depend on those services and the File
Manager, not just a TCP socket. QEMU user-mode TCP/IP connectivity does not prove
AppleTalk or multicast compatibility. Original execution also requires the 68k
translator, callback and classic record/trap adapters; native APIs must not
reinterpret classic events or advertised Gestalt capabilities.

## Delivery and acceptance

1. Inventory and candidate-engine spike: protocol/ABI matrix, host packet tests,
   loopback, bounded buffers and virtio-net RX/TX on a private guest. Establish
   scheduler, timers, entropy and ownership prerequisites and explicit gaps.
2. IPv4/ARP/ICMP, UDP and TCP with native nonblocking/readiness services. Add
   blocking APIs only with real park/wake/timeout/cancellation. Private fixtures
   prove incoming/outgoing connections, two apps, close/reuse and loss recovery.
3. DHCPv4, resolver and Network control panel with persisted JSON and link/lease
   recovery. Demonstrate named-host traffic and a local HTTP sample, including
   DNS/TCP fallback and failures, without relying on the public Internet.
4. IPv6 dual stack, scoped routing/autoconfiguration, DHCPv6 profile, multicast
   and discovery. Validate against a controlled dual-stack peer with a real L2
   test link and explicit MTU/fragment/error scenarios. Complete the host matrix.
5. `std::net` adapter and TLS/HTTPS with real entropy, clock and trust gates.
   Add sockets/readiness tests through the platform layer and native SDK; verify
   certificate failures as well as successful local HTTPS connections.
6. Original-system captures, native compatibility assertions and translated
   MacTCP/Open Transport probes; then AppleTalk and selected file-sharing/
   discovery workflows. Advertise only demonstrated profiles and selectors.

Canonical public-service assertions live in `crates/mac-toolbox/tests` through
the aggregate SDK and real resident services. Supplement with pure protocol
packet tests, malformed-input fuzzing, deterministic clock/loss/reorder fixtures
and private QEMU interoperability guests. Use
[QEMU network backends](https://www.qemu.org/docs/master/system/devices/net.html)
appropriate to each protocol: user-mode connectivity alone is not L2 coverage.
Keep tests local and reproducible, using controlled servers and packet captures.

Required cases include partial I/O, zero-length UDP versus TCP EOF, bad lengths/
checksums, truncated and oversized packets, loss/duplicates/reorder, fragmented
traffic, connect/accept races, no lost readiness wakeups, timeouts/cancellation,
backlog/buffer exhaustion, stale/foreign IDs, app exit, link reset, DHCP renewal,
DNS failure/TTL/TCP fallback, IPv6 scope and concurrent clients. Repeat contention
and routing tests on multiple harts when P1 is available. Record throughput,
memory bounds, scheduling/interrupt durations and UI dispatch latency under
sustained RX/TX load. Cursor Test responsiveness, present timings and bounded
repainting remain gates; a fast bulk transfer cannot excuse a stalled desktop.
