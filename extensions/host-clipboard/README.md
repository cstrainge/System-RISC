# Host Clipboard

An independently loaded System INIT extension, installed by `cargo xtask image`
in the blessed `System Folder/Extensions`. `VDAgent Channel` remains a separate
external driver in `System Folder/Drivers`. The package uses no host companion;
QEMU supplies its standard `qemu-vdagent` clipboard backend.

The initializer defers readiness and admits one real module-owned scheduled
worker. The worker owns bounded channel tickets, immutable Scrap pins, protocol
state and policy; it confirms readiness only after actual protocol and resident
Scrap admission. It sleeps through the native worker timer API, never handles
clipboard traffic on UI input or under the controller mutex, and retains owned
protocol/cleanup state through refusals.

The image installs version 1 JSON at
`System Folder/Preferences/7731a000000000000000000000000080.json`:

```json
{"version":1,"enabled":true,"direction":"both"}
```

Directions are `both`, `host-to-guest` and `guest-to-host`. Missing preferences
are created by the real Files compare/exchange service; invalid or unavailable
preferences keep sharing local. There is no settings panel yet.

Build the complete paired ROM/System/module image with a private output root:

```sh
SYSTEM7_BUILD_DIR=/path/to/private/build cargo xtask image
```

See [the architecture, limits and native evidence](../../docs/HOST-CLIPBOARD.md).
The observed Linux GTK editor round trip does not qualify native Windows/macOS,
SMP, empty text, stress or full controller cancellation/reclamation. Asynchronous
refusal currently has retained registry/serial status but no post-desktop UI.
