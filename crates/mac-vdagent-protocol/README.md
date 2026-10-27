# SPICE vdagent clipboard protocol

This `no_std` crate implements the standard SPICE vdagent byte contract used by
QEMU's `qemu-vdagent` chardev on `com.redhat.spice.0`. It contains no native host
clipboard implementation and no authority, registration, scheduler or Scrap
policy. The guest extension and transport retain those responsibilities.

`Decoder` owns its bounded receive allocation through partial feeds. A frame
borrows that allocation; the caller cannot feed another fragment while using it.
Each feed reports its exact consumed byte count. Allocation refusal retains the
admitted partial state for retry; malformed framing poisons the decoder until
the transport has been replaced. Unknown nonclipboard messages and server-port
chunks have explicit ignored outcomes and allocate no payload storage.

The standard chunk limit is 2048 payload bytes; QEMU currently emits chunks of
1024 bytes. The local complete-message ceiling is 1 MiB, reflecting QEMU's
vdagent output admission. Text must leave room for the 20-byte message header,
format word, and negotiated selection prefix. A smaller local limit can be
selected before parsing. `OwnedMessage` provides typed outbound constructors
and borrowed chunk slices whose lifetime is pinned to their queued owner.

Capability intersections preserve only locally selected named bits. Clipboard
layouts depend on negotiated selection and grab-serial support. The serial is
present on GRAB only; REQUEST and DATA do not contain a serial or usable request
token. The bridge must keep at most one fetch in flight per selection/type,
retain invalidated work until its response is drained, and reset the connection
after an ambiguous timeout before reusing that request state. Local connection,
registration, and Scrap generations remain separate from these external labels.

The codec preserves valid UTF-8 byte content, including a zero-byte text body.
It distinguishes empty UTF-8 text, unavailable format, and unsupported formats.
It does not normalize newlines, add NUL terminators, substitute invalid Unicode,
or invent a private encoding. Platform/native conversion and bridge policy must
be explicit. QEMU's zero-length native clipboard handling still needs real host
qualification; codec success alone does not establish empty-text interoperability.

## Provenance

The canonical external structures and identifiers are from
[`spice/vd_agent.h`](https://gitlab.freedesktop.org/spice/spice-protocol/-/blob/master/spice/vd_agent.h).
The implementation was checked against installed `libspice-protocol-dev`
`0.14.3-1`, header SHA-256
`9b23ba83958e6117bec7a0c8b6b4dc946dded8cdf9a23316ca35636442bce542`.
The [SPICE agent protocol](https://www.spice-space.org/agent-protocol.html)
documents framing, capability exchange and the by-demand clipboard workflow.
The [QEMU v10.1.0 implementation](https://github.com/qemu/qemu/blob/v10.1.0/ui/vdagent.c)
establishes the current chardev's chunking and local buffer constraints.

QEMU advertises by-demand clipboard, selection and grab-serial capabilities;
its implementation does not currently negotiate MAX_CLIPBOARD or line-ending
conversion. Do not treat absent capability support as a successful negotiated
limit or conversion. QEMU's
[clipboard core](https://github.com/qemu/qemu/blob/v10.1.0/ui/clipboard.c)
and [GTK adapter](https://github.com/qemu/qemu/blob/v10.1.0/ui/gtk-clipboard.c)
need separate real-host tests for zero-byte text.

## Checks

Run `cargo test --locked -p mac-vdagent-protocol` using the private build target
selected for the extension campaign. Tests cover fragmentation, coalescing,
unknown data, invalid layouts/capabilities, Unicode, local limits, real receive
allocation refusal/retry and borrow lifetimes. They do not claim guest service
integration or Windows/Linux/macOS host clipboard qualification.
